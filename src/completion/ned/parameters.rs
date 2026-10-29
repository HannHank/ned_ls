use crate::{
    completion::ned::CompletionItemResponse, goto_definition::ScopeResolver,
    hir::ned::ParamBlockItem,
};

use crate::completion::shared::param_doc_data;

pub fn parameters_completions(
    engine: &super::CompletionEngine<'_, '_>,
    completions: &mut super::CompletionResult,
    submodules: bool,
) {
    let ned_file = engine.db.get_source_file_input(engine.uri).unwrap();
    let resolver = ScopeResolver::new(engine.db, ned_file);

    if let Some(containing_module) = resolver.find_containing_module(engine.byte_offset) {
        let visible_defs = crate::db::visible_definitions(engine.db, ned_file);
        let base_context =
            crate::context::get_item_context(engine.db, *containing_module, visible_defs.clone());

        // Not necessary for parameter section but allows me to reuse this for the submodules
        let node = engine
            .ast
            .root_node()
            .descendant_for_byte_range(engine.byte_offset, engine.byte_offset)
            .unwrap_or(engine.ast.root_node());

        // Replace outer context with submodule context
        let base_context = resolver.determine_active_context(
            &node,
            base_context,
            containing_module,
            ned_file.rope(engine.db),
            visible_defs.clone(),
        );

        let (last_token, token_range) = engine.get_replacement_range();

        // dot access
        if let Some(base_name) = super::get_dot_target_name(&last_token)
            && submodules
        {
            if let Some(sub_mod) = base_context.submodules.get(base_name)
                && let Some(sub_context) = crate::semantics::ned::shared::resolve_submodule_context(
                    engine.db,
                    containing_module,
                    &base_context,
                    base_name,
                    &sub_mod.item,
                    visible_defs.clone(),
                )
            {
                let sub_params: Vec<super::GenericSuggestion> = sub_context
                    .parameters
                    .iter()
                    .filter_map(|(name, entry)| match entry.item {
                        ParamBlockItem::Param(param) => Some(super::GenericSuggestion {
                            name: name.clone(),
                            detail: param
                                .ned_type(engine.db)
                                .as_ref()
                                .map(|t| format!("Parameter: {}", t.as_str())),
                            doc: param_doc_data(engine.db, entry.origin, name),
                        }),
                        ParamBlockItem::Property(_) => None, // Properties are suggested as fix
                                                             // keywords, therefore I must filter them out here to avoid duplicates
                    })
                    .collect();
                // suggestions are bare parameter names → only replace the segment after the dot
                completions.insert(
                    "SubmoduleParams".to_string(),
                    CompletionItemResponse::Parameters(
                        sub_params,
                        engine.get_segment_replacement_range(),
                    ),
                );
            }
        } else {
            // Suggest own parameters, submodule names, and flattened submodule parameters
            let own_params: Vec<super::GenericSuggestion> = base_context
                .parameters
                .iter()
                .filter_map(|(name, entry)| match entry.item {
                    ParamBlockItem::Param(param) => Some(super::GenericSuggestion {
                        name: name.clone(),
                        detail: param
                            .ned_type(engine.db)
                            .as_ref()
                            .map(|t| format!("Type: {}", t.as_str())),
                        doc: param_doc_data(engine.db, entry.origin, name),
                    }),
                    ParamBlockItem::Property(_) => None, // Properties are suggested as fix
                                                         // keywords, therefore I must filter them out here to avoid duplicates
                })
                .collect();
            if submodules {
                let submodules: Vec<String> = base_context.submodules.keys().cloned().collect();
                let mut flattened_sub_params: Vec<super::GenericSuggestion> = Vec::new();

                for (sub_name, sub_mod) in &base_context.submodules {
                    // Use the helper to fetch the context
                    if let Some(sub_context) =
                        crate::semantics::ned::shared::resolve_submodule_context(
                            engine.db,
                            containing_module,
                            &base_context,
                            sub_name,
                            &sub_mod.item,
                            visible_defs.clone(),
                        )
                    {
                        for (param, declared_in) in
                            sub_context.parameters.values().filter_map(|entry| {
                                if let ParamBlockItem::Param(param) = &entry.item {
                                    Some((param, entry.origin))
                                } else {
                                    None
                                }
                            })
                        {
                            let name = param.name(engine.db).text(engine.db).clone();
                            let doc = param_doc_data(engine.db, declared_in, &name);
                            flattened_sub_params.push(super::GenericSuggestion {
                                name: format!("{}.{}", sub_name, name),
                                detail: param
                                    .ned_type(engine.db)
                                    .as_ref()
                                    .map(|t| format!("Parameter: {}", t.as_str())),
                                doc: doc.clone(),
                            });

                            // If the submodule is a vector → suggest wildcard access
                            if sub_mod.item.vector_size_expr.is_some() {
                                flattened_sub_params.push(super::GenericSuggestion {
                                    name: format!("{}[*].{}", sub_name, name),
                                    detail: param
                                        .ned_type(engine.db)
                                        .as_ref()
                                        .map(|t| format!("Parameter: {}", t.as_str())),
                                    doc,
                                });
                            }
                        }
                    }
                }
                completions.insert(
                    "Submodules".to_string(),
                    CompletionItemResponse::Submodules(submodules, token_range),
                );
                completions.insert(
                    "FlattenedSubParams".to_string(),
                    CompletionItemResponse::Parameters(flattened_sub_params, token_range),
                );
            }

            completions.insert(
                "OwnParameters".to_string(),
                CompletionItemResponse::Parameters(own_params, token_range),
            );
        }
    }
}
