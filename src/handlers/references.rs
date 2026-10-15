use crate::Backend;
use std::panic::AssertUnwindSafe;
use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::{Location, ReferenceParams};

pub async fn handle(backend: &Backend, params: ReferenceParams) -> Result<Option<Vec<Location>>> {
    let uri = params.text_document_position.text_document.uri;
    let position = params.text_document_position.position;
    let include_declaration = params.context.include_declaration;

    // references are resolved for ned files only
    if uri.as_str().ends_with(".ini") {
        return Ok(None);
    }

    // work on a snapshot so the edit pipeline is not blocked while scanning
    let db = backend.shared.db.lock().await.clone();

    match salsa::Cancelled::catch(AssertUnwindSafe(|| {
        crate::goto_definition::find_references(&db, &uri, position, include_declaration)
    })) {
        Ok(Some(locations)) if !locations.is_empty() => Ok(Some(locations)),
        Ok(_) => Ok(None),
        Err(_cancelled) => {
            log::debug!("references cancelled by concurrent edit");
            Ok(None)
        }
    }
}
