use crate::{completion::ned::CompletionItemResponse, goto_definition::ScopeResolver};

/// Provides completion suggestions for connections sections in NED files
pub fn connections_completions<'a, 'db>(
    engine: &super::CompletionEngine<'a, 'db>,
    completions: &mut super::CompletionResult,
) {
    let ned_file = engine
        .db
        .get_source_file_input(engine.uri)
        .unwrap_or_else(|| {
            panic!(
                "Error: Unable to retrieve NED file input for URI: {}",
                engine.uri
            )
        });
    let resolver = ScopeResolver::new(engine.db, ned_file);

    if let Some(containing_module) = resolver.find_containing_module(engine.byte_offset) {
        let visible_defs = crate::db::visible_definitions(engine.db, ned_file);
        let base_context =
            crate::context::get_item_context(engine.db, *containing_module, visible_defs.clone());

        let (last_token, token_range) = engine.get_replacement_range();

        // dot access
        if let Some(base_name) = super::get_dot_target_name(&last_token)
            && let Some(sub_mod) = base_context.submodules.get(base_name)
            && let Some(sub_context) = crate::semantics::ned::shared::resolve_submodule_context(
                engine.db,
                containing_module,
                base_context,
                base_name,
                &sub_mod.item,
                visible_defs.clone(),
            )
        {
            let sub_gates: Vec<String> = sub_context.gates.keys().cloned().collect();
            // suggestions are bare gate names → only replace the segment after the dot
            completions.insert(
                "SubmoduleGates".to_string(),
                CompletionItemResponse::Gates(sub_gates, engine.get_segment_replacement_range()),
            );
        } else {
            let prev_token = engine.get_previous_token();

            let is_new_statement = prev_token == "connections:"
                || prev_token == "{"
                || prev_token == "}"
                || prev_token == ";"
                || prev_token.ends_with(';')
                || prev_token == "if"
                || prev_token == "for";

            let is_after_arrow = prev_token == "-->" || prev_token == "<--" || prev_token == "<-->";
            // || prev_token == "allowunconnected:";

            // expecting arrow → allows submodule.gate (arrow suggestions)
            let is_expecting_arrow = !prev_token.is_empty() && !is_new_statement && !is_after_arrow;

            if is_expecting_arrow && last_token.is_empty() {
                completions.insert(
                    "Arrows".to_string(),
                    CompletionItemResponse::Keywords(vec!["-->", "<--", "<-->"], token_range),
                );
            } else {
                // space after arrow -> dst
                let own_gates: Vec<String> = base_context.gates.keys().cloned().collect();

                // Start with empty vectors and ONLY add submodules if they actually have gates
                let mut submodules = Vec::new();
                let mut flattened_sub_gates = Vec::new();

                for (sub_name, sub_mod) in &base_context.submodules {
                    if let Some(sub_context) =
                        crate::semantics::ned::shared::resolve_submodule_context(
                            engine.db,
                            containing_module,
                            base_context,
                            sub_name,
                            &sub_mod.item,
                            visible_defs.clone(),
                        )
                    {
                        // only suggest submodules that actually have gates
                        if !sub_context.gates.is_empty() {
                            submodules.push(sub_name.clone());

                            for gate in sub_context.gates.keys() {
                                flattened_sub_gates.push(format!("{}.{}", sub_name, gate));
                                if sub_mod.item.vector_size_expr.is_some() {
                                    flattened_sub_gates.push(format!("{}[].{}", sub_name, gate));
                                }
                            }
                        }
                    }
                }

                completions.insert(
                    "OwnGates".to_string(),
                    CompletionItemResponse::Gates(own_gates, token_range),
                );
                completions.insert(
                    "Submodules".to_string(),
                    CompletionItemResponse::Submodules(submodules, token_range),
                );
                completions.insert(
                    "FlattenedSubGates".to_string(),
                    CompletionItemResponse::Gates(flattened_sub_gates, token_range),
                );

                // only suggested at the beginning of a new line
                if is_new_statement {
                    completions.insert(
                        "Keywords".to_string(),
                        CompletionItemResponse::Keywords(vec!["for", "if"], token_range),
                    );
                }
            }
        }
    }
}
