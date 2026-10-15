use crate::Backend;
use std::panic::AssertUnwindSafe;
use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::{
    PrepareRenameResponse, RenameParams, TextDocumentPositionParams, WorkspaceEdit,
};

pub async fn prepare(
    backend: &Backend,
    params: TextDocumentPositionParams,
) -> Result<Option<PrepareRenameResponse>> {
    let uri = params.text_document.uri;
    let position = params.position;

    if uri.as_str().ends_with(".ini") {
        return Ok(None);
    }

    let db = backend.shared.db.lock().await.clone();

    match salsa::Cancelled::catch(AssertUnwindSafe(|| {
        crate::goto_definition::symbol_at(&db, &uri, position).map(|sym| {
            PrepareRenameResponse::RangeWithPlaceholder {
                range: sym.token_range,
                placeholder: sym.word,
            }
        })
    })) {
        Ok(response) => Ok(response),
        Err(_cancelled) => Ok(None),
    }
}

pub async fn handle(backend: &Backend, params: RenameParams) -> Result<Option<WorkspaceEdit>> {
    let uri = params.text_document_position.text_document.uri;
    let position = params.text_document_position.position;
    let new_name = params.new_name;

    if uri.as_str().ends_with(".ini") {
        return Ok(None);
    }

    // work on a snapshot so the edit pipeline is not blocked
    let db = backend.shared.db.lock().await.clone();

    match salsa::Cancelled::catch(AssertUnwindSafe(|| {
        crate::goto_definition::rename(&db, &uri, position, &new_name)
    })) {
        Ok(Ok(edit)) => Ok(edit),
        // invalid name, collision, built-in → shown by the client
        Ok(Err(message)) => Err(tower_lsp::jsonrpc::Error::invalid_params(message)),
        Err(_cancelled) => {
            log::debug!("rename cancelled by concurrent edit");
            Ok(None)
        }
    }
}
