use crate::SharedState;
use crate::db::Db;
use crate::hir;
use crate::parser::Parser;

use salsa::{Durability, Setter};
use tower_lsp::lsp_types::*;

use walkdir::WalkDir;

// efficient file editing
use ropey::Rope;

// parallel iterator
use rayon::prelude::*;

// tree-sitter stuff
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Index all files in a project folder, or a list of files. This is the main entry point for indexing.
pub async fn index_project(
    project_path: Option<&Path>,
    backend: Arc<SharedState>,
    durability: salsa::Durability,
) {
    let Some(root_path) = project_path else {
        return;
    };

    // Collect all files in sequence
    let entries: Vec<_> = WalkDir::new(root_path)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| {
            // Since I only subscribe to ned an ini files this is just a sanity check
            e.path().extension().and_then(std::ffi::OsStr::to_str) == Some("ned")
                || e.path().extension().and_then(std::ffi::OsStr::to_str) == Some("ini")
        })
        .collect();
    let paths: Vec<PathBuf> = entries.iter().map(|e| e.path().to_path_buf()).collect();

    index_files(paths, backend, durability).await;
}

pub async fn index_files(
    file_paths: Vec<PathBuf>,
    backend: Arc<SharedState>,
    durability: salsa::Durability,
) {
    let client = backend.client.clone();

    // Never read over a document the client has open → its buffer may hold unsaved edits
    // Filtering here also keeps the parse off the hot path.
    let file_paths: Vec<PathBuf> = file_paths
        .into_iter()
        .filter(|path| {
            Url::from_file_path(path)
                .map(|uri| !backend.open_documents.contains(&uri))
                .unwrap_or(false)
        })
        .collect();

    // Rayon parallel iteration
    let parsed_results: Vec<(Url, Rope, tree_sitter::Tree)> =
        tokio::task::spawn_blocking(move || {
            file_paths
                .par_iter()
                .map_init(
                    // This is cool and lets me reuse the same parser instance for each
                    // thread
                    Parser::new,
                    |parser, path| {
                        let content = std::fs::read_to_string(path).ok()?;
                        let uri = Url::from_file_path(path).ok()?;
                        let tree = parser.parse(&uri, &content, None)?;
                        let rope = Rope::from_str(&content);
                        Some((uri, rope, tree))
                    },
                )
                .filter_map(|res| res) // just filter out None values
                .collect()
        })
        .await
        .unwrap();

    let mut db = backend.db.lock().await;
    for (uri, rope, tree) in parsed_results {
        match db.get_source_file_input(&uri) {
            // Already tracked → reuse the handle so every memo keyed on this SourceFile stays
            // valid and only the content changes.
            // Creating a fresh handle here would orphan the
            // old one and force a full recomputation for the file.
            Some(existing) => {
                db.set_ast_for_file(existing, tree);
                existing
                    .set_rope(&mut *db)
                    .with_durability(durability)
                    .to(rope);
            }
            None => {
                let handle = hir::SourceFile::new(&*db, Rope::from_str(""), uri.clone(), 0);
                db.set_ast_for_file(handle, tree);
                handle
                    .set_rope(&mut *db)
                    .with_durability(durability)
                    .to(rope);
                db.set_ned_file_input(uri, handle);
            }
        }
    }

    client
        .log_message(MessageType::INFO, "[+] Finished indexing all files.")
        .await;
}

pub async fn index_in_memory_file(
    backend: Arc<SharedState>,
    content: &'static str,
    virtual_path: &str, // std/standard.ned
) {
    let uri = Url::parse(&format!("ned-internal:///{}", virtual_path)).unwrap();

    let mut parser = Parser::new();
    let tree = parser
        .parse(&uri, content, None)
        .expect("Failed to parse std lib");
    let rope = Rope::from_str(content);

    let mut db = backend.db.lock().await;

    let handle = hir::SourceFile::new(&*db, Rope::from_str(""), uri.clone(), 0);
    db.set_ast_for_file(handle, tree);

    handle
        .set_rope(&mut *db)
        .with_durability(Durability::HIGH) // High because std lib does not change at all
        .to(rope);

    db.set_ned_file_input(uri, handle);

    backend
        .client
        .log_message(MessageType::INFO, "[*] Indexed internal standard library.")
        .await;
}
