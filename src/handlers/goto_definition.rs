use crate::Backend;
use crate::db::Db;
use crate::goto_definition::GotoResult;
use crate::goto_definition::gd_ini_goto_definition;
use crate::hir;
use crate::hir::tree_utils::text_for_node;
use crate::utils::{self};
use std::panic::AssertUnwindSafe;
use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::{GotoDefinitionParams, GotoDefinitionResponse, MessageType};

pub async fn handle(
    backend: &Backend,
    params: GotoDefinitionParams,
) -> Result<Option<GotoDefinitionResponse>> {
    let uri = params.text_document_position_params.text_document.uri;
    let position = params.text_document_position_params.position;

    log::debug!("Goto definition requested at {:?} in {}", position, uri);

    let db = backend.shared.db.lock().await.clone();

    let source_input = match db.get_source_file_input(&uri) {
        Some(input) => input,
        None => {
            backend
                .shared
                .client
                .log_message(MessageType::WARNING, "File not found in database")
                .await;
            return Ok(None);
        }
    };

    // If ini file → use the ini-specific goto definition logic
    if uri.as_str().ends_with(".ini") {
        let links = match salsa::Cancelled::catch(AssertUnwindSafe(|| {
            gd_ini_goto_definition(&db, source_input, position)
        })) {
            Ok(links) => links,
            Err(_cancelled) => return Ok(None),
        };
        if links.is_empty() {
            return Ok(None);
        }
        return Ok(Some(GotoDefinitionResponse::Link(links)));
    }

    // Continue with NED file goto defintion by first finding the position
    let result = salsa::Cancelled::catch(AssertUnwindSafe(|| {
        let rope = source_input.rope(&db);
        let char_offset = utils::position_to_char(rope, position);
        let byte_offset = rope.char_to_byte(char_offset);

        let resolver = crate::goto_definition::ScopeResolver::new(&db, source_input);

        let Some(ast) = db.get_ast_for_file(&source_input) else {
            return GotoResult::NoWord;
        };

        let Some(node) = ast
            .root_node()
            .descendant_for_byte_range(byte_offset, byte_offset)
        else {
            return GotoResult::NoWord;
        };

        log::debug!(
            "Node at cursor: kind='{}', text='{}'",
            node.kind(),
            text_for_node(&node, rope).unwrap_or("<invalid>".to_string())
        );

        let mut t_node = node;

        // skipping over non-named nodes to get to the actual symbol node
        while !t_node.is_named() && t_node.parent().is_some() {
            t_node = t_node.parent().unwrap();
        }

        // Try to get the `name` child if this node has one, otherwise use the node's text
        let name_node = t_node.child_by_field_name("name").unwrap_or(t_node);
        let word = text_for_node(&name_node, rope).unwrap_or_default();

        let node_span = hir::Span {
            start: name_node.start_byte(),
            end: name_node.end_byte(),
        };

        if let Some(resolved_list) = resolver.resolve(&t_node, byte_offset, &word) {
            let links: Vec<tower_lsp::lsp_types::LocationLink> = resolved_list
                .iter()
                .filter_map(|resolved| resolved.to_location_link(&db, rope, node_span))
                .collect();

            if links.is_empty() {
                GotoResult::SymbolNotFound(word)
            } else {
                GotoResult::SymbolFound(word, links)
            }
        } else {
            GotoResult::SymbolNotFound(word)
        }
    }));

    let result = match result {
        Ok(result) => result,
        Err(_cancelled) => return Ok(None),
    };

    match result {
        GotoResult::ImportSuccess(import_text, count, links) => {
            log::debug!(
                "Import '{}' resolves to {} declaration(s)",
                import_text,
                count
            );
            Ok(Some(GotoDefinitionResponse::Link(links)))
        }
        GotoResult::ImportNotFound(import_text) => {
            backend
                .shared
                .client
                .show_message(
                    MessageType::WARNING,
                    format!(
                        "Import pattern '{}' matched declarations but files not found",
                        import_text
                    ),
                )
                .await;
            Ok(None)
        }
        GotoResult::ImportNoMatch(import_text) => {
            backend
                .shared
                .client
                .show_message(
                    MessageType::WARNING,
                    format!(
                        "Import pattern '{}' does not match any declarations",
                        import_text
                    ),
                )
                .await;
            Ok(None)
        }
        GotoResult::NoWord => {
            log::debug!("No word under cursor");
            Ok(None)
        }
        GotoResult::SymbolFound(word, locations) => {
            log::debug!("Found definition for '{}'", word);
            Ok(Some(GotoDefinitionResponse::Link(locations)))
        }
        GotoResult::SymbolNotFound(word) => {
            log::debug!("No definition found for '{}'", word);
            Ok(None)
        }
    }
}
