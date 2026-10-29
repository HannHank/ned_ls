use super::prelude::*;

use crate::hir::ini::assignment_hir::IniAssignmentHir;
use crate::hir::ned::ParamBlockItem;
use crate::hir::ned::import_hir::ImportPattern;
use crate::hir::ned::type_inference::NedType;
use crate::semantics::ini::validate_gate_channel_assignment;
use crate::semantics::ned::shared::match_glob;
use std::sync::Arc;

/// Processes a typename assignment in an INI file, updating the network state with the new type information for matched submodules.
pub fn process_typename_assignment<'db>(
    db: &'db dyn crate::db::Db,
    ctx: &mut ValidationCtx<'_, 'db>,
    network_state: &mut super::NetworkState<'_, 'db>,
    assignment: &IniAssignmentHir<'db>,
    segments: &[super::ParsedIniSegment],
    is_active_section: bool,
) {
    let raw_value = assignment.value.text(db);

    let clean_type_name = raw_value.trim_matches(|c| c == '"' || c == '\'');

    // it is alowed to assign an empty typename
    if clean_type_name.is_empty() {
        return;
    }

    let clean_pattern = segments
        .iter()
        .map(|s| s.name.as_str())
        .collect::<Vec<_>>()
        .join(".");

    let full_pattern = super::build_full_pattern(segments);

    let mut matching_paths = Vec::new();

    for idx in network_state.find_flattened_matches(&clean_pattern) {
        let node = &network_state.flattened_nodes[idx];
        let absolute_path = &network_state.flattened_paths[idx];
        matching_paths.push((absolute_path.clone(), node.relative_path.clone()));
    }

    for (path, node) in &network_state.dynamic_nodes {
        if super::match_glob(&clean_pattern, path) {
            matching_paths.push((path.clone(), node.relative_path.clone()));
        }
    }

    // no match → warning
    if matching_paths.is_empty() {
        if is_active_section {
            ctx.warning(
                assignment.key_span.start,
                assignment.key_span.end,
                "Typename assignment did not match any submodules.".to_string(),
            );
        }
        return;
    }

    // Resolve the type definition for the assigned typename
    let name_word = crate::hir::Word::new(db, clean_type_name.to_string());

    if clean_type_name.contains("*") || clean_type_name.contains("?") {
        ctx.error(
            assignment.value_span.start,
            assignment.value_span.end,
            "Type assignments cannot use wildcards.".to_string(),
        );
        return;
    }

    let def_opt = if clean_type_name.contains(".") {
        let import_pattern = ImportPattern::new(db, clean_type_name.to_string());

        // Currently I think an fqn can still resolve to multiple defs (e.g. same project included
        // twice) → only returning first match and log warning
        let resolved_import = crate::db::package_tree::resolve_import(db, import_pattern);
        if let Some(vec) = resolved_import.as_ref()
            && vec.len() > 1
        {
            log::warn!(
                "Type '{}' resolved to {} definitions, using the first one.",
                clean_type_name,
                vec.len()
            );
        }
        resolved_import
            .as_ref()
            .and_then(|vec| vec.first().cloned())
    } else {
        *crate::db::package_tree::resolve_global_type(db, name_word)
    };
    // to fqn only simple names

    if let Some(def_index) = def_opt {
        // Resolve the context for the override
        let resolved_hir = crate::context::resolve_item_hir(db, def_index);

        let new_skeleton = crate::semantics::shared::get_type_skeleton(db, def_index);

        for (absolute_path, relative_path) in matching_paths {
            // update override context for the matched submodule
            if let Some(res_hir) = &resolved_hir {
                let new_ctx =
                    crate::context::get_item_context(db, res_hir.item, res_hir.definitions.clone());

                network_state
                    .dynamic_contexts
                    .entry(absolute_path)
                    .or_default()
                    .push(ContextOverride {
                        source_pattern: full_pattern.clone(),
                        context: new_ctx.clone(),
                    });
            }

            // expand dynamic tree
            for skel_node in new_skeleton.nodes.iter() {
                if skel_node.relative_path.is_empty() {
                    continue;
                }

                // could be improved
                let new_path = if relative_path.is_empty() {
                    skel_node.relative_path.clone()
                } else {
                    format!("{}.{}", relative_path, skel_node.relative_path)
                };

                let dynamic_abs_path = format!("{}.{}", network_state.name, new_path);

                network_state.dynamic_nodes.push((
                    dynamic_abs_path,
                    SkeletonNode {
                        relative_path: new_path,
                        is_vector: skel_node.is_vector,
                        vector_size: skel_node.vector_size.clone(),
                        type_def: skel_node.type_def,
                        like_param: skel_node.like_param.clone(),
                    },
                ));
            }
        }
    } else if is_active_section {
        ctx.error(
            assignment.value_span.start,
            assignment.value_span.end,
            format!("Type '{}' not found in workspace.", clean_type_name),
        );
    }
}

/// Validates a parameter assignment in an ini file, checking for matching submodules and type compatibility with the assigned value.
pub fn validate_ini_assignment<'db>(
    ctx: &mut ValidationCtx<'_, 'db>,
    network_state: &super::NetworkState<'_, 'db>,
    assignment: &IniAssignmentHir<'db>,
    segments: &[super::ParsedIniSegment],
    target_param_name: &str,
) {
    let raw_key = assignment.key.text(ctx.db);
    let raw_value = assignment.value.text(ctx.db);

    // If there are no segments, the user forgot the module path
    if segments.is_empty() {
        ctx.error(
            assignment.key_span.start,
            assignment.key_span.end,
            format!(
                "Invalid parameter assignment. Must include a module path (e.g., '{}.{}' or '**.{}').",
                network_state.name, target_param_name, target_param_name
            ),
        );
        return;
    }

    // If any segment has a gate direction, this is a gate assignment, so we validate it separately
    // → very basic validation with no custom channel type support
    // The idea was more to hide the warnings but then i decided I might as well validate the
    // assignment against some buildin rules.
    if let Some(gate_pos) = segments.iter().position(|s| s.gate_direction.is_some()) {
        validate_gate_channel_assignment(
            ctx,
            network_state,
            assignment,
            segments,
            gate_pos,
            target_param_name,
        );
        return;
    }

    let clean_pattern = segments
        .iter()
        .map(|s| s.name.as_str())
        .collect::<Vec<_>>()
        .join(".");

    let current_full_pattern = super::build_full_pattern(segments);

    let mut matched_submodules: Vec<(
        &str,
        Arc<crate::context::ItemContext<'db>>,
        &SkeletonNode<'db>,
    )> = Vec::new();

    for idx in network_state.find_flattened_matches(&clean_pattern) {
        let node = &network_state.flattened_nodes[idx];
        let absolute_path = &network_state.flattened_paths[idx];

        let resolved_ctx = resolve_active_context(
            ctx.db,
            node,
            absolute_path,
            &current_full_pattern,
            network_state,
        );

        matched_submodules.push((absolute_path.as_str(), resolved_ctx, node));
    }

    //
    for (path, node) in &network_state.dynamic_nodes {
        if match_glob(&clean_pattern, path) {
            let resolved_ctx =
                resolve_active_context(ctx.db, node, path, &current_full_pattern, network_state);

            matched_submodules.push((path.as_str(), resolved_ctx, node));
        }
    }

    let is_sub_wildcard = segments
        .iter()
        .any(|s| s.name.contains('*') || s.name.contains('?'));

    // no submodule matches
    if matched_submodules.is_empty() {
        let message = if is_sub_wildcard {
            "Submodule pattern did not match any submodules in this network.".to_string()
        } else {
            "Submodule not found for parameter assignment.".to_string()
        };
        ctx.warning(assignment.key_span.start, assignment.key_span.end, message);
        return;
    }

    let mut param_found_in_any = false;

    // check if the parameter is fixed in NED → if so, then warn that the ini value is ignored
    let mut fixed_in: Option<Vec<String>> = Some(Vec::new());
    let target_param_pattern = target_param_name.replace("**", "*");
    let is_param_wildcard = target_param_name.contains('*') || target_param_name.contains('?');

    // validate against the matched submodules
    // 'tag neede so that I can jump to the outer loop
    'matches: for (absolute_path, sub_ctx, node) in &matched_submodules {
        if target_param_name == "typename" {
            param_found_in_any = true;
            continue;
        }

        // Checking if any submodule index in the key is out of bounds
        // ** can absorb a variable number of elements → stop loop
        let path_elems: Vec<&str> = absolute_path.split('.').collect();

        let mut seg_pos = segments.len();
        let mut elem_pos = path_elems.len();

        while seg_pos > 0 && elem_pos > 0 {
            let segment = &segments[seg_pos - 1];
            if segment.name.contains("**") {
                break;
            }
            seg_pos -= 1;
            elem_pos -= 1;

            let Some(index_str) = &segment.index else {
                continue;
            };

            let node_path = path_elems[..=elem_pos].join(".");

            // the matched node itself or one of its parents
            let indexed_node = if elem_pos + 1 == path_elems.len() {
                Some(*node)
            } else {
                find_node_for_path(network_state, &node_path)
            };

            let Some(indexed_node) = indexed_node else {
                continue;
            };

            if !indexed_node.is_vector {
                if is_sub_wildcard {
                    // Wildcard pattern (**.eth[*]) matched a scalar submodule →
                    // in opp this means the pattern does not apply here
                    continue 'matches;
                }
                ctx.error(
                    assignment.key_span.start,
                    assignment.key_span.end,
                    format!(
                        "Vectorization mismatch for '{}': pattern is vectorized but submodule '{}' is not a vector.",
                        raw_key, node_path
                    ),
                );
                continue 'matches;
            }

            // check the index bounds like autoTSN[*0] = 1;
            match check_index_bounds(index_str, indexed_node, network_state, &node_path, raw_key) {
                Some(IndexIssue::Error(msg)) => {
                    ctx.error(assignment.key_span.start, assignment.key_span.end, msg);
                }
                Some(IndexIssue::Warning(msg)) => {
                    ctx.warning(assignment.key_span.start, assignment.key_span.end, msg);
                }
                None => {}
            }
        }
        // → index check finished

        // if target contains wildcard → AutoTSN.FramePree* = 1;
        if is_param_wildcard {
            // print parameters
            // eprintln!(
            //     "Validating parameter pattern '{}' in '{}'",
            //     target_param_pattern, absolute_path
            // );
            for (pname, param_entry) in sub_ctx.parameters.iter() {
                if match_glob(&target_param_pattern, pname) {
                    param_found_in_any = true;
                    if let ParamBlockItem::Param(tp) = &param_entry.item {
                        let expected_type = tp.ned_type(ctx.db);
                        let inferred_type = NedType::infer_type_from_str(raw_value);

                        // eprintln!(
                        //     "Validating parameter '{}' in '{}': expected type = {:?}, inferred type = {:?}",
                        //     pname, absolute_path, expected_type, inferred_type
                        // );

                        if let (Some(expected), Some(inferred)) = (expected_type, inferred_type)
                            && !expected.accepts(&inferred)
                        {
                            ctx.error(
                                assignment.value_span.start,
                                assignment.value_span.end,
                                format!(
                                    "Type mismatch for '{}.{}'. Expected '{}', found '{}'.",
                                    absolute_path,
                                    pname,
                                    expected.as_str(),
                                    inferred.as_str()
                                ),
                            );
                        }
                    }
                }
            }
        } else {
            // target does not contain wildcard → AutoTSN.FramePreemption = 1;

            let mut expected_type = None;
            let mut param_exists = false;

            if let Some(target_param) = sub_ctx.parameters.get(target_param_name) {
                param_exists = true;
                if let ParamBlockItem::Param(tp) = &target_param.item {
                    expected_type = tp.ned_type(ctx.db).clone();
                }

                // only checked while every match so far is fixed → stops at the first free one
                if let Some(locations) = fixed_in.as_mut() {
                    match super::fixed_params::ned_fixed_assignment(
                        ctx.db,
                        network_state,
                        absolute_path,
                        sub_ctx,
                        target_param_name,
                    ) {
                        Some(location) => {
                            if !locations.contains(&location) {
                                locations.push(location);
                            }
                        }
                        None => fixed_in = None,
                    }
                }
            } else if sub_ctx.gates.contains_key(target_param_name) {
                param_exists = true;
                fixed_in = None;
            }

            if param_exists {
                param_found_in_any = true;
                let inferred_type = NedType::infer_type_from_str(raw_value);

                if let (Some(expected), Some(inferred)) = (expected_type, inferred_type)
                    && !expected.accepts(&inferred)
                {
                    ctx.error(
                        assignment.value_span.start,
                        assignment.value_span.end,
                        format!(
                            "Type mismatch for '{}.{}'. Expected '{}', found '{}'.",
                            absolute_path,
                            target_param_name,
                            expected.as_str(),
                            inferred.as_str()
                        ),
                    );
                }
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
        ctx.warning(assignment.key_span.start, assignment.key_span.end, message);
    }

    // opp ignores ini values for parameters that ned assigns without default()
    // This is a really important warning and saves a lot of time
    if param_found_in_any
        && !is_param_wildcard
        && let Some(locations) = fixed_in
        && !locations.is_empty()
    {
        // println!("Parameter '{}' is fixed in NED without default() → ini value is ignored.", target_param_name);
        ctx.warning(
            assignment.key_span.start,
            assignment.key_span.end,
            format!(
                "Parameter '{}' is set in NED without default() ({}), so this value is ignored.",
                target_param_name,
                locations.join("; ")
            ),
        );
    }
}

/// Finds the skeleton node for an exact absolute path → considering both the
/// static skeleton and dynamically added nodes.
pub(super) fn find_node_for_path<'a, 'db>(
    network_state: &'a super::NetworkState<'_, 'db>,
    path: &str,
) -> Option<&'a SkeletonNode<'db>> {
    if let Some(idx) = network_state.find_flattened_matches(path).first() {
        return Some(&network_state.flattened_nodes[*idx]);
    }
    network_state
        .dynamic_nodes
        .iter()
        .find(|(p, _)| p == path)
        .map(|(_, node)| node)
}

// Fix indexes → Error, Non-Matched wildcards → Warning
enum IndexIssue {
    Error(String),
    Warning(String),
}

/// Returns a diagnostic if the given index,range,pattern is out of bounds for
/// the vector submodule at node_path
fn check_index_bounds(
    index_str: &str,
    node: &SkeletonNode,
    network_state: &super::NetworkState<'_, '_>,
    node_path: &str,
    raw_key: &str,
) -> Option<IndexIssue> {
    let sub_vector_size_expr = node.vector_size.as_ref()?;

    if index_str == "*" {
        // plain wildcard matches every index → fine even if the size is unresolvable
        return None;
    }

    if index_str.contains('*') || index_str.contains('?') {
        // index pattern like [*0] → valid if any concrete index in bounds matches it
        let s_size = resolve_vector_size(sub_vector_size_expr, network_state, node_path)?;

        // TODO: populated dynamically at runtime → No warning for now, not sure if I want to stick
        // with it. Maybe I should implement different levels of diagnostics to allow the user to
        // filter out warnings that are not relevant for them.
        if s_size == 0 {
            return None;
        }

        // I cannot scan through all indices if the vector is too big
        // TODO: maybe this should just be dropped without a warning
        if s_size > 10_000 {
            return Some(IndexIssue::Warning(format!(
                "Index pattern [{}] cannot be fully checked for submodule '{}' (size {}).",
                index_str, node_path, s_size
            )));
        }

        // Utilize the glob matching function to check if any index matches the pattern. If none match → return a warning
        if !(0..s_size).any(|i| match_glob(index_str, &i.to_string())) {
            return Some(IndexIssue::Warning(format!(
                "Index pattern [{}] does not match any index of '{}' (size {}).",
                index_str, node_path, s_size
            )));
        }
        return None;
    }

    if index_str.contains("..") {
        // opp allows ranges like 0..3
        let parts: Vec<&str> = index_str.split("..").collect();
        if parts.len() == 2 {
            let start_idx = parts[0].parse::<i32>().unwrap_or(0);
            let end_idx_opt = parts[1].parse::<i32>().ok();

            let s_size = resolve_vector_size(sub_vector_size_expr, network_state, node_path)?;

            if start_idx < 0 || start_idx >= s_size {
                return Some(IndexIssue::Error(format!(
                    "Range start {} is out of bounds for '{}' (size {}).",
                    start_idx, node_path, s_size
                )));
            }
            if let Some(end_idx) = end_idx_opt {
                if start_idx > end_idx {
                    return Some(IndexIssue::Error(format!(
                        "Invalid range [{}..{}].",
                        start_idx, end_idx
                    )));
                }
                if end_idx >= s_size {
                    return Some(IndexIssue::Error(format!(
                        "Range end {} is out of bounds for '{}' (size {}).",
                        end_idx, node_path, s_size
                    )));
                }
            }
        }
        None
    } else if let Ok(p_idx) = index_str.parse::<i32>() {
        // no wildcard and no range → check index
        let s_size = resolve_vector_size(sub_vector_size_expr, network_state, node_path)?;

        // bigger or negative
        if p_idx >= s_size || p_idx < 0 {
            return Some(IndexIssue::Error(format!(
                "Index out of bounds for '{}': submodule '{}' has size {}, but index is {}.",
                raw_key, node_path, s_size, p_idx
            )));
        }
        None
    } else {
        // array index contains invalid characters
        Some(IndexIssue::Error(format!(
            "Invalid index format: [{}]",
            index_str
        )))
    }
}
