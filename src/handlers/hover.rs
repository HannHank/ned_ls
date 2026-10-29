use crate::Backend;
use crate::db::Db;
use crate::hir::tree_utils::get_target_node;
use crate::utils;
use std::panic::AssertUnwindSafe;
use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::{
    Hover, HoverContents, HoverParams, MarkupContent, MarkupKind, Position, Url,
};

pub async fn handle(backend: &Backend, params: HoverParams) -> Result<Option<Hover>> {
    let uri = params.text_document_position_params.text_document.uri;
    let position = params.text_document_position_params.position;

    // INI files do not have a NED-style scope resolver
    if uri.as_str().ends_with(".ini") {
        return Ok(None);
    }

    // work on a snapshot so the edit pipeline is not blocked while resolving
    let db = backend.shared.db.lock().await.clone();

    match salsa::Cancelled::catch(AssertUnwindSafe(|| resolve_hover(&db, &uri, position))) {
        Ok(hover) => Ok(hover),
        Err(_cancelled) => {
            log::debug!("hover cancelled by concurrent edit");
            Ok(None)
        }
    }
}

fn resolve_hover(db: &crate::db::RootLspDatabase, uri: &Url, position: Position) -> Option<Hover> {
    let ned_input = db.get_source_file_input(uri)?;
    let source_rope = ned_input.rope(db);

    let char_offset = utils::position_to_char(source_rope, position);
    let byte_offset = source_rope.char_to_byte(char_offset);

    let word = utils::get_word_under_cursor(source_rope, position)?;

    let ast = db.get_ast_for_file(&ned_input)?;
    let t_node = get_target_node(byte_offset, Some(&ast))?;

    let resolver = crate::goto_definition::ScopeResolver::new(db, ned_input);

    // Resolve the symbol to find its Definition
    let resolved_decl = resolver
        .resolve(&t_node, byte_offset, &word)?
        .first()?
        .clone();

    // Get the Target File
    let target_uri = resolved_decl
        .to_location(db)
        .map(|loc| loc.uri)
        .unwrap_or(uri.clone()); // fallback to source uri

    let target_input = db.get_source_file_input(&target_uri)?;

    // members (parameters, gates, submodules) are often documented with a trailing comment
    let doc_string = match &resolved_decl {
        crate::goto_definition::ResolvedDefinition::Module(def_index) => {
            utils::documentation_at_span(db, target_input, def_index.span(db))
        }
        crate::goto_definition::ResolvedDefinition::Member { span, .. } => {
            utils::member_documentation_at_span(db, target_input, *span)
        }
    }?;

    Some(Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value: doc_string,
        }),
        range: None,
    })
}
