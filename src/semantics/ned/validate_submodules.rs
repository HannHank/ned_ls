use crate::{
    hir::{
        Word,
        ned::{submodules_hir::SubmoduleDefinition, type_inference::NedType},
    },
    semantics::ned::prelude::*,
};

pub fn validate_submodules<'a, 'db>(
    ctx: &mut ValidationCtx<'a, 'db>,
    scope: &ValidationScope<'a, 'db>,
    local_definitions: &Vec<DefIndex<'db>>,
    module_def_index: DefIndex<'db>,
) {
    let mut seen_local = std::collections::HashSet::new();
    for submodule in scope.item.submodules(ctx.db).iter() {
        let sub_name = submodule.name.clone();
        // duplicates in the current module
        if !seen_local.insert(sub_name.clone()) {
            ctx.error(
                submodule.span.start,
                submodule.span.end,
                format!("Duplicate submodule name '{}'.", sub_name),
            );
            continue;
        }
        // Overrides a submodule from an inherited module
        let mut is_inherited_duplicate = false;
        for ancestor_def in &scope.context.ancestry {
            if let Some(ancestor_hir) = crate::context::resolve_item_hir(ctx.db, *ancestor_def)
                && ancestor_hir
                    .item
                    .submodules(ctx.db)
                    .iter()
                    .any(|s| s.name == sub_name)
            {
                ctx.error(
                    submodule.span.start,
                    submodule.span.end,
                    format!(
                        "Submodule '{}' is already defined in inherited module '{}'.",
                        sub_name,
                        ancestor_def.simple_name(ctx.db).text(ctx.db)
                    ),
                );
                is_inherited_duplicate = true;
                break;
            }
        }
        if is_inherited_duplicate {
            continue;
        }
        let parent = ParentScope {
            params: scope.item.parameters(ctx.db),
            context: &scope.context,
        };
        let (actual_type_str, is_concrete) =
            get_submodule_actual_type(ctx.db, &parent, &sub_name, submodule).unwrap_or_default();

        // Validate the instantiation types and interfaces
        match &submodule.definition {
            SubmoduleDefinition::Parametric {
                interface_name: name,
                like_expr,
                span,
            } => {
                if name.contains('*') {
                    ctx.error(
                        span.start,
                        span.end,
                        format!("Wildcards are not allowed in 'like' interface names: '{name}'."),
                    );
                    continue;
                }

                let required_iface_def_opt = crate::db::find_def(ctx.db, name, &scope.arc_defs)
                    .or_else(|| crate::db::find_def(ctx.db, name, local_definitions));

                if required_iface_def_opt.is_none() {
                    ctx.error(
                        submodule.span.start,
                        submodule.span.end,
                        format!(
                            "Submodule interface '{}' in 'like' expression not found.",
                            name
                        ),
                    );
                    continue;
                }

                // grab the interned FQN Word of the required interface
                let req_fqn_word = required_iface_def_opt.unwrap().fqn(ctx.db);

                // If a like_expr exists, parse out the default types and verify them
                if let Some(expr) = like_expr {
                    let concrete_types = extract_all_concrete_types(expr);
                    for concrete_type in concrete_types {
                        if concrete_type.is_empty() {
                            continue;
                        }

                        if concrete_type.contains('*') {
                            ctx.error(
                                submodule.span.start,
                                submodule.span.end,
                                format!(
                                    "Wildcards are not allowed in submodule types: '{concrete_type}'."
                                ),
                            );
                            continue;
                        }

                        let type_def_opt =
                            crate::db::find_def(ctx.db, &concrete_type, &scope.arc_defs)
                                .or_else(|| {
                                    crate::db::find_def(ctx.db, &concrete_type, local_definitions)
                                })
                                .or_else(|| {
                                    let name_word =
                                        crate::hir::Word::new(ctx.db, concrete_type.clone());
                                    *crate::db::package_tree::resolve_global_type(ctx.db, name_word)
                                });

                        if let Some(type_def) = type_def_opt {
                            let mut implements = false;

                            // resolves and compares fqn → TODO: maybe I should put this in a
                            // seperated function and track it

                            // Check own likes
                            if *check_type_implements(ctx.db, type_def, *req_fqn_word) {
                                implements = true;
                            } else {
                                // Check ancestry likes
                                if let Some(resolved_type) =
                                    crate::context::resolve_item_hir(ctx.db, type_def)
                                    && let Some(concrete_input) =
                                        ctx.db.get_source_file_input(type_def.uri(ctx.db))
                                {
                                    let concrete_vis_defs =
                                        crate::db::visible_definitions(ctx.db, concrete_input);

                                    let type_ctx = crate::context::get_item_context(
                                        ctx.db,
                                        resolved_type.item,
                                        concrete_vis_defs.clone(),
                                    );

                                    // walk up the ancestry tree
                                    for ancestor_def in &type_ctx.ancestry {
                                        if *check_type_implements(
                                            ctx.db,
                                            *ancestor_def,
                                            *req_fqn_word,
                                        ) {
                                            implements = true;
                                            break;
                                        }
                                    }
                                }
                            }

                            if !implements {
                                ctx.warning(
                                    submodule.span.start,
                                    submodule.span.end,
                                    format!(
                                        "Type '{}' does not implement the required interface '{}'.",
                                        concrete_type, name
                                    ),
                                );
                            }
                        } else {
                            ctx.warning(
                                submodule.span.start,
                                submodule.span.end,
                                format!(
                                    "Type '{}' inside 'like' expression could not be resolved.",
                                    concrete_type
                                ),
                            );
                        }
                    }
                }
            }
            SubmoduleDefinition::Concrete { type_name, span } => {
                if type_name.contains('*') {
                    ctx.error(
                        span.start,
                        span.end,
                        format!("Wildcards are not allowed in submodule types: '{type_name}'."),
                    );
                    continue;
                }

                let is_defined = crate::db::find_def(ctx.db, type_name, &scope.arc_defs).is_some()
                    || crate::db::find_def(ctx.db, type_name, local_definitions).is_some();

                if !is_defined {
                    ctx.error(
                        submodule.span.start,
                        submodule.span.end,
                        format!(
                            "Submodule type '{}' must be explicitly imported or defined locally.",
                            type_name
                        ),
                    );
                    continue;
                }
            }
        }

        // Validate inline parameters and gates
        if is_concrete || !actual_type_str.is_empty() {
            let sub_def_index_opt = crate::db::find_def(ctx.db, &actual_type_str, &scope.arc_defs)
                .or_else(|| {
                    if !actual_type_str.contains('.') {
                        let name_word = crate::hir::Word::new(ctx.db, actual_type_str.clone());
                        *crate::db::package_tree::resolve_global_type(ctx.db, name_word)
                    } else {
                        None
                    }
                });

            let sub_ctx_opt = if let Some(local_type) = scope.context.types.get(&actual_type_str) {
                Some(
                    crate::context::get_item_context(
                        ctx.db,
                        local_type.item,
                        scope.arc_defs.clone(),
                    )
                    .clone(),
                )
            } else {
                crate::context::get_submodule_context_from_type(
                    ctx.db,
                    &actual_type_str,
                    scope.arc_defs.clone(),
                )
            };

            if let Some(sub_ctx) = sub_ctx_opt {
                let pattern_def_index = sub_def_index_opt.unwrap_or(module_def_index);
                for param in &submodule.parameters {
                    if let ParamBlockItem::Param(p) = param {
                        if *p.pattern(ctx.db) {
                            validate_pattern_assignment(ctx, p, pattern_def_index);
                        } else {
                            let pname = p.name(ctx.db).text(ctx.db);
                            if pname == "typename" {
                                continue;
                            }

                            if p.ned_type(ctx.db).is_some() {
                                ctx.error(
                                    p.span(ctx.db).start,
                                    p.span(ctx.db).end,
                                    "Cannot define new parameters within a submodule.".to_string(),
                                );
                                continue;
                            }

                            let mut param_exists = false;
                            let mut expected_type = None;

                            if let Some(target_param) = sub_ctx.parameters.get(pname) {
                                param_exists = true;
                                if let ParamBlockItem::Param(tp) = &target_param.item {
                                    expected_type = tp.ned_type(ctx.db).clone();
                                }
                            } else if sub_ctx.gates.contains_key(pname) {
                                param_exists = true;
                                expected_type = Some(NedType::Int); // TODO:remove
                                // placeholder
                            }

                            if !param_exists {
                                if is_concrete {
                                    ctx.error(
                                        p.span(ctx.db).start, p.span(ctx.db).end,
                                        format!("Parameter or gate '{}' not found in submodule type '{}'.", pname, actual_type_str),
                                    );
                                }
                            } else {
                                check_type_compatibility(
                                    ctx,
                                    p,
                                    expected_type,
                                    pname,
                                    &actual_type_str,
                                    "",
                                );
                            }
                        }
                    }
                }
            }
        }
    }
}

#[salsa::tracked]
pub fn check_type_implements<'db>(
    db: &'db dyn crate::db::Db,
    check_def: DefIndex<'db>,
    req_fqn_word: Word<'db>,
) -> bool {
    if let Some(hir) = crate::context::resolve_item_hir(db, check_def) {
        let file_uri = check_def.uri(db);
        // the def index can be stale after a file deletion → no match, not a panic
        let Some(file_input) = db.get_source_file_input(file_uri) else {
            return false;
        };
        let vis_defs = crate::db::visible_definitions(db, file_input);

        for l in hir.item.like(db) {
            let impl_raw_name = l.name.text(db);

            // eprintln!("{:?}", l);
            if let Some(impl_def) = crate::db::find_def(db, impl_raw_name, vis_defs)
                && *impl_def.fqn(db) == req_fqn_word
            {
                return true;
            }
        }
    }
    false
}
