use crate::{completion::ned::CompletionItemResponse, goto_definition::ScopeResolver};

pub fn gates_completions(
    engine: &super::CompletionEngine<'_, '_>,
    completions: &mut super::CompletionResult,
) {
    let ned_file = engine.db.get_source_file_input(engine.uri).unwrap();
    let resolver = ScopeResolver::new(engine.db, ned_file);

    if let Some(containing_module) = resolver.find_containing_module(engine.byte_offset) {
        let visible_defs = crate::db::visible_definitions(engine.db, ned_file);
        let base_context =
            crate::context::get_item_context(engine.db, *containing_module, visible_defs.clone());

        let node = engine
            .ast
            .root_node()
            .descendant_for_byte_range(engine.byte_offset, engine.byte_offset)
            .unwrap_or(engine.ast.root_node());

        let base_context = resolver.determine_active_context(
            &node,
            base_context,
            containing_module,
            ned_file.rope(engine.db),
            visible_defs.clone(),
        );

        completions.insert(
            "Gates".to_string(),
            CompletionItemResponse::Gates(
                base_context.gates.keys().cloned().collect(),
                engine.get_replacement_range().1, // the end of the replacement range
            ),
        );
    }
}
