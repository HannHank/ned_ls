use crate::Backend;
use crate::db::Db;
use tower_lsp::jsonrpc::{Error, Result};
use tower_lsp::lsp_types::{DocumentFormattingParams, MessageType, Position, Range, TextEdit};

pub async fn handle(
    backend: &Backend,
    params: DocumentFormattingParams,
) -> Result<Option<Vec<TextEdit>>> {
    let uri = params.text_document.uri;

    backend
        .shared
        .client
        .log_message(
            MessageType::INFO,
            format!("Formatting requested for {}", uri),
        )
        .await;

    let db = backend.shared.db.lock().await;

    // avoids formatting ini files that are not part of an omnetpp workspace
    if uri.as_str().ends_with(".ini") && !crate::workspace_membership::is_omnetpp_workspace(&*db) {
        return Ok(None);
    }

    let ned_input = db
        .get_source_file_input(&uri)
        .ok_or_else(Error::internal_error)?;
    let tree = db
        .get_ast_for_file(&ned_input)
        .ok_or_else(Error::internal_error)?;
    let rope = ned_input.rope(&*db);
    let source_text = rope.to_string();

    let formatted_text = crate::formatting::format(tree.root_node(), &source_text);

    let line_count = rope.len_lines();
    let last_line_len = rope.line(line_count - 1).len_chars();

    let full_range = Range {
        start: Position::new(0, 0),
        end: Position::new(line_count as u32, last_line_len as u32),
    };

    Ok(Some(vec![TextEdit {
        range: full_range,
        new_text: formatted_text,
    }]))
}
