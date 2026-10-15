use crate::{
    hir::ned::{
        ParamHir,
        type_inference::{self, NedType},
    },
    semantics::{ned::prelude::*, shared::get_type_structure},
};

/// Entry point for validating parameter assignments in a given scope.
pub fn validate_parameters<'a, 'db>(
    ctx: &mut ValidationCtx<'a, 'db>,
    scope: &ValidationScope<'a, 'db>,
    module_def_index: crate::db::DefIndex<'db>,
) {
    for param in scope.item.parameters(ctx.db) {
        if let ParamBlockItem::Param(p) = param {
            if *p.pattern(ctx.db) {
                validate_pattern_assignment(ctx, p, module_def_index);
            } else {
                validate_direct_assignment(ctx, scope, p);
            }
        }
    }
}

/// This function validates a parameter assignment that uses a pattern with potential wildcards.
pub fn validate_pattern_assignment<'a, 'db>(
    ctx: &mut ValidationCtx<'a, 'db>,
    p: &ParamHir<'db>,
    module_def_index: crate::db::DefIndex<'db>,
) {
    let raw = p.name(ctx.db).text(ctx.db);
    let segments = p.segments(ctx.db);
    let target_param_opt = p.target_param(ctx.db);

    if segments.is_empty() || target_param_opt.is_none() {
        return;
    }

    let flattened_type = get_type_structure(ctx.db, module_def_index);

    let target_param_name = target_param_opt.unwrap().text(ctx.db);
    let is_param_wildcard = target_param_name.contains('*') || target_param_name.contains('?');
    let target_param_pattern = target_param_name.replace("**", "*");

    let owner_segment = segments.last().unwrap();
    let is_owner_vectorized = owner_segment.index(ctx.db).is_some();
    let owner_name = owner_segment.name(ctx.db).text(ctx.db);
    let is_owner_name_wildcard = owner_name.contains('*') || owner_name.contains('?');

    let clean_pattern = segments
        .iter()
        .map(|s| s.name(ctx.db).text(ctx.db).as_str())
        .collect::<Vec<_>>()
        .join(".");

    // deep resolution
    let mut matched_submodules = Vec::new();

    // gather all submodules that match the pattern
    for node in flattened_type.nodes.iter() {
        if match_glob(&clean_pattern, &node.relative_path) {
            matched_submodules.push(node);
        }
    }

    let is_sub_wildcard = segments.iter().any(|s| {
        s.name(ctx.db).text(ctx.db).contains('*') || s.name(ctx.db).text(ctx.db).contains('?')
    });

    if matched_submodules.is_empty() {
        let sub_pattern_str = segments
            .iter()
            .map(|s| s.name(ctx.db).text(ctx.db).as_str())
            .collect::<Vec<_>>()
            .join(".");
        let message = if is_sub_wildcard {
            format!(
                "Submodule pattern '{}' did not match any local submodules.",
                sub_pattern_str
            )
        } else {
            format!(
                "Submodule '{}' not found for parameter assignment.",
                sub_pattern_str
            )
        };

        ctx.warning(p.span(ctx.db).start, p.span(ctx.db).end, message);
        return;
    }

    let mut param_found_in_any = false;

    // Iterate over matched submodules and validate parameter existence and type compatibility
    for node in matched_submodules {
        if target_param_name == "typename" {
            param_found_in_any = true;
            continue;
        }

        let actual_sub_name = &node.relative_path;
        let sub_ctx = &node.context;

        let mut vectorization_mismatch = false;
        let mut custom_error_message = None;

        if let Some(param_index_word) = owner_segment.index(ctx.db) {
            let param_index_str = param_index_word.text(ctx.db);

            if !node.is_vector {
                vectorization_mismatch = true;
            } else {
                // Check if index is out of bound for vectors
                let sub_vector_size = node.vector_size.as_ref().unwrap();

                if param_index_str == "*" {
                    // do not need to throw an error in that case
                } else if param_index_str.parse::<i32>().is_ok()
                    && sub_vector_size.parse::<i32>().is_ok()
                {
                    // If both are integers, we can compare them
                    let p_idx: i32 = param_index_str.parse().unwrap();
                    let s_size: i32 = sub_vector_size.parse().unwrap();

                    if p_idx >= s_size || p_idx < 0 {
                        custom_error_message = Some(format!(
                            "Index out of bounds for '{}': submodule '{}' has size {}, but index is {}.",
                            raw, actual_sub_name, s_size, p_idx
                        ));
                    }
                } else {
                    // Ignore cases like sizeof
                }
            }
        } else if !is_owner_name_wildcard && node.is_vector {
            // submodule is a vector
            vectorization_mismatch = true;
        }

        if let Some(msg) = custom_error_message {
            ctx.error(p.span(ctx.db).start, p.span(ctx.db).end, msg);
        } else if vectorization_mismatch {
            let message = format!(
                "Vectorization mismatch for '{}': pattern is {} but submodule '{}' is {}.",
                raw,
                if is_owner_vectorized {
                    "vectorized"
                } else {
                    "not vectorized"
                },
                actual_sub_name,
                if node.is_vector {
                    "a vector"
                } else {
                    "not a vector"
                }
            );
            ctx.error(p.span(ctx.db).start, p.span(ctx.db).end, message);
        }

        if is_param_wildcard {
            for (pname, param_entry) in sub_ctx.parameters.iter() {
                if match_glob(&target_param_pattern, pname) {
                    param_found_in_any = true;
                    if let ParamBlockItem::Param(tp) = &param_entry.item {
                        check_type_compatibility(
                            ctx,
                            p,
                            tp.ned_type(ctx.db).clone(),
                            raw.as_str(),
                            actual_sub_name,
                            pname,
                        );
                    }
                }
            }
        } else {
            // no wildcard → check if the parameter exists in the submodule and validate its type
            let mut param_exists = false;
            let mut expected_type = None;

            if let Some(target_param) = sub_ctx.parameters.get(target_param_name.as_str()) {
                param_exists = true;
                if let ParamBlockItem::Param(tp) = &target_param.item {
                    expected_type = tp.ned_type(ctx.db).clone();
                }
            } else if sub_ctx.gates.contains_key(target_param_name.as_str()) {
                param_exists = true;
                expected_type = Some(type_inference::NedType::Int);
            }

            if param_exists {
                param_found_in_any = true;
                check_type_compatibility(ctx, p, expected_type, raw.as_str(), actual_sub_name, "");
            }
        }
    }

    if !param_found_in_any {
        let message = if is_param_wildcard {
            format!(
                "Parameter pattern '{}' did not match any parameters in matched submodules.",
                target_param_name
            )
        } else {
            format!(
                "Parameter or gate '{}' not found in any matched submodules.",
                target_param_name
            )
        };
        ctx.warning(p.span(ctx.db).start, p.span(ctx.db).end, message);
    }
}

/// This function validates a direct parameter assignment (without patterns) by checking if the parameter exists in the current scope and if its type is compatible with the expected type.
fn validate_direct_assignment<'a, 'db>(
    ctx: &mut ValidationCtx<'a, 'db>,
    scope: &ValidationScope<'a, 'db>,
    p: &ParamHir<'db>,
) {
    let name = p.name(ctx.db).text(ctx.db);

    match scope.context.parameters.get(name.as_str()) {
        Some(context_param) => {
            if let Some(context_p) = match &context_param.item {
                ParamBlockItem::Param(p) => Some(p),
                _ => None,
            } {
                check_type_compatibility(ctx, p, context_p.ned_type(ctx.db).clone(), name, "", "");
            }
        }
        None => {
            ctx.error(
                p.span(ctx.db).start,
                p.span(ctx.db).end,
                format!("Parameter '{}' used before declared.", name),
            );
        }
    }
}

/// Simple type compatibility check that compares the expected and inferred types and pushes a diagnostic if they are not compatible. The message is adapted based on whether it's a direct parameter assignment or a pattern assignment with submodules.
pub fn check_type_compatibility<'db>(
    ctx: &mut ValidationCtx<'_, 'db>,
    p: &ParamHir<'db>,
    expected_type_opt: Option<NedType>,
    raw_pattern: &str,
    actual_sub_name: &str,
    pname: &str, // the parameter name
) {
    // simple pattern to compare the types
    if let (Some(expected_type), Some(inferred_type)) = (expected_type_opt, p.inferred_type(ctx.db))
        && !expected_type.accepts(inferred_type)
    {
        // Preparing the error message
        let message = if pname.is_empty() {
            format!(
                "Pattern '{}' for '{}' has incompatible type. Expected '{}', found '{}'.",
                raw_pattern,
                actual_sub_name,
                expected_type.as_str(),
                inferred_type.as_str()
            )
        } else {
            format!(
                "Pattern '{}' for '{}.{}' has incompatible type. Expected '{}', found '{}'.",
                raw_pattern,
                actual_sub_name,
                pname,
                expected_type.as_str(),
                inferred_type.as_str()
            )
        };

        ctx.error(p.span(ctx.db).start, p.span(ctx.db).end, message);
    }
}
