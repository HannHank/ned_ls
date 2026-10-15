use crate::Backend;
use crate::db::Db;
use crate::incremental_parsing;
use crate::indexing;
use crate::utils;
use log::debug;
use ropey::Rope;
use salsa::Setter;
use std::path::PathBuf;

use tower_lsp::lsp_types::*;

pub async fn did_open(backend: &Backend, params: DidOpenTextDocumentParams) {
    backend
        .shared
        .open_documents
        .insert(params.text_document.uri.clone());

    backend
        .partial_update_worker
        .enqueue_job(incremental_parsing::UpdateJob::Full(TextDocumentItem {
            uri: params.text_document.uri,
            text: params.text_document.text,
            version: params.text_document.version,
            language_id: params.text_document.language_id,
        }));
}

pub async fn did_save(backend: &Backend, params: DidSaveTextDocumentParams) {
    backend
        .partial_update_worker
        .enqueue_job(incremental_parsing::UpdateJob::ForceAnalysis(
            params.text_document.uri,
        ));

    //  TODO: have not implemented semantic tokens yet
    // let _ = backend.shared.client.semantic_tokens_refresh().await;

    debug!("file saved and analysis forced!");
}

pub async fn did_change(backend: &Backend, params: DidChangeTextDocumentParams) {
    debug!("document version: {:?}", params.text_document.version);

    // Send an wncremental job
    backend
        .partial_update_worker
        .enqueue_job(incremental_parsing::UpdateJob::Incremental(params));
}

pub async fn did_close(backend: &Backend, params: DidCloseTextDocumentParams) {
    let uri = params.text_document.uri;

    backend.shared.open_documents.remove(&uri);

    // unsaved edits in the buffer → re-read.
    if let Ok(path) = uri.to_file_path()
        && path.is_file()
    {
        reload_files(backend, vec![path]).await;
    }

    debug!("file closed: {uri}");
}

/// Re-read files from disk into the db, keeping their existing SourceFile handles.
///
/// Skips anything the client currently has open → that content belongs to the editor.
pub async fn reload_files(backend: &Backend, paths: Vec<PathBuf>) {
    let paths: Vec<PathBuf> = paths
        .into_iter()
        .filter(|p| {
            Url::from_file_path(p)
                .map(|uri| !backend.shared.open_documents.contains(&uri))
                .unwrap_or(false)
        })
        .collect();

    if paths.is_empty() {
        return;
    }

    indexing::index_files(paths, backend.shared.clone(), salsa::Durability::LOW).await;

    {
        let mut db_guard = backend.shared.db.lock().await;
        db_guard.update_workspace_structure();
    }

    backend.shared.diagnostics_publisher.request_publish_all();
}

/// Drop files from the db and clear their diagnostics.
pub async fn remove_files(backend: &Backend, uris: Vec<Url>) {
    if uris.is_empty() {
        return;
    }

    {
        let mut db_guard = backend.shared.db.lock().await;
        for uri in &uris {
            if let Some(ned_file) = db_guard.get_source_file_input(uri) {
                ned_file.set_rope(&mut *db_guard).to(Rope::from_str("")); // empty rope
            }
            db_guard.remove_source_file_input(uri);
            backend.shared.open_documents.remove(uri);
        }
        db_guard.update_workspace_structure();
    }

    backend.shared.diagnostics_publisher.request_publish_all();
    backend
        .shared
        .diagnostics_publisher
        .request_clear_files(uris);
}

/// React to filesystem changes made outside the editor.
/// INFO: Currently external disk updates from related projects are not detected by the LSP client,
/// as they are not part of the workspace. However if those files are changed through the editor, the LSP server will be notified and will update its database accordingly.
/// Something that might change with notify-rs.
pub async fn did_change_watched_files(backend: &Backend, params: DidChangeWatchedFilesParams) {
    let mut to_reload: Vec<PathBuf> = Vec::new();
    let mut to_remove: Vec<Url> = Vec::new();
    let mut config_changed = false;

    let nedfolders = {
        let config = backend.shared.config.lock().await;
        config.as_ref().map(|c| c.nedfolders.clone())
    };

    for event in &params.changes {
        let Ok(path) = event.uri.to_file_path() else {
            continue;
        };

        // Filter for config changes in .nedfolders or Ned_ls.toml, which require a workspace reload.
        if matches!(
            path.file_name().and_then(|n| n.to_str()),
            Some(".nedfolders") | Some("Ned_ls.toml")
        ) {
            config_changed = true;
            continue;
        }

        match event.typ {
            FileChangeType::DELETED => to_remove.push(event.uri.clone()),

            // read what is on disk now. A file the db
            // has not seen gets registered, one it already knows gets its content refreshed.
            FileChangeType::CREATED | FileChangeType::CHANGED => {
                // Only index files that are actually part of the indexed scope. A stray .ned in a
                // build directory should not silently join the workspace.
                let in_scope = nedfolders
                    .as_ref()
                    .map(|roots| {
                        let normalized = crate::workspace_membership::normalize(&path);
                        crate::workspace_membership::nedfolder_root_for(&normalized, roots)
                            .is_some()
                    })
                    .unwrap_or(false);

                if in_scope {
                    to_reload.push(path);
                }
            }

            _ => {}
        }
    }

    if !to_remove.is_empty() {
        backend
            .shared
            .client
            .log_message(
                MessageType::INFO,
                format!("{} watched file(s) deleted on disk", to_remove.len()),
            )
            .await;
        remove_files(backend, to_remove).await;
    }

    if !to_reload.is_empty() {
        backend
            .shared
            .client
            .log_message(
                MessageType::INFO,
                format!("{} watched file(s) changed on disk", to_reload.len()),
            )
            .await;
        reload_files(backend, to_reload).await;
    }

    if config_changed {
        crate::handlers::commands::reload_workspace_config(backend).await;
    }
}

pub async fn did_create_files(backend: &Backend, params: CreateFilesParams) {
    for file in &params.files {
        backend
            .shared
            .client
            .log_message(MessageType::INFO, format!("File created: {}", file.uri))
            .await;
    }
    let files = params
        .files
        .iter()
        .filter_map(|f| Url::parse(&f.uri).ok())
        .filter_map(|url| url.to_file_path().ok())
        .collect::<Vec<PathBuf>>();
    indexing::index_files(files, backend.shared.clone(), salsa::Durability::LOW).await;

    // update workspace revision
    {
        let mut db_guard = backend.shared.db.lock().await;
        db_guard.update_workspace_structure();
    }

    // rerun diagnostics for the workspace
    backend.shared.diagnostics_publisher.request_publish_all();
}

pub async fn did_delete_files(backend: &Backend, params: DeleteFilesParams) {
    let deleted_uris: Vec<Url> = params
        .files
        .iter()
        .filter_map(|f| Url::parse(&f.uri).ok())
        .collect();

    for uri in &deleted_uris {
        backend
            .shared
            .client
            .log_message(MessageType::INFO, format!("File deleted: {uri}"))
            .await;
    }

    remove_files(backend, deleted_uris).await;
}

pub async fn did_rename_files(backend: &Backend, params: RenameFilesParams) {
    backend
        .shared
        .client
        .log_message(
            MessageType::INFO,
            format!(
                "[*] did_rename_files called with {} items",
                params.files.len()
            ),
        )
        .await;

    // Parse to Url and determine if folder or file rename
    let renames_to_process: Vec<(Url, Url, bool)> = {
        params
            .files
            .iter()
            .filter_map(|file| {
                if let (Ok(old_url), Ok(new_url)) =
                    (Url::parse(&file.old_uri), Url::parse(&file.new_uri))
                {
                    let is_folder = new_url.to_file_path().map(|p| p.is_dir()).unwrap_or(false);

                    Some((old_url, new_url, is_folder))
                } else {
                    None
                }
            })
            .collect()
    };

    for (old_url, new_url, is_folder) in renames_to_process {
        backend
            .shared
            .client
            .log_message(
                MessageType::INFO,
                format!("Processing rename: {} -> {}", old_url, new_url),
            )
            .await;

        if is_folder {
            backend
                .shared
                .client
                .log_message(MessageType::INFO, "Detected as folder rename".to_string())
                .await;
            handle_folder_rename(backend, &old_url, &new_url).await;
        } else {
            backend
                .shared
                .client
                .log_message(
                    MessageType::INFO,
                    "Detected as single file rename".to_string(),
                )
                .await;
            handle_file_rename(backend, &old_url, &new_url).await;
        }
    }

    // Update workspace revision once after all renames
    {
        let mut db_guard = backend.shared.db.lock().await;
        db_guard.update_workspace_structure();
    }

    // Rerun diagnostics for the workspace
    backend.shared.diagnostics_publisher.request_publish_all();
}

pub async fn handle_file_rename(backend: &Backend, old_url: &Url, new_url: &Url) {
    let config_opt = { backend.shared.config.lock().await.clone() };

    let (was_in_nedfolder, is_in_nedfolder) = if let Some(config) = config_opt {
        let in_nedfolder = |url: &Url| {
            url.to_file_path().is_ok_and(|path| {
                let path = crate::workspace_membership::normalize(&path);
                crate::workspace_membership::nedfolder_root_for(&path, &config.nedfolders).is_some()
            })
        };

        (in_nedfolder(old_url), in_nedfolder(new_url))
    } else {
        (false, false)
    };

    backend
        .shared
        .client
        .log_message(
            MessageType::INFO,
            format!(
                "File rename: was_in_nedfolder={}, is_in_nedfolder={}",
                was_in_nedfolder, is_in_nedfolder
            ),
        )
        .await;

    match (was_in_nedfolder, is_in_nedfolder) {
        // File moved out of tracked folders
        (true, false) => {
            backend
                .shared
                .client
                .log_message(
                    MessageType::INFO,
                    format!(
                        "[!] File moved outside tracked folders, removing: {} -> {}",
                        old_url, new_url
                    ),
                )
                .await;

            // Remove from database
            let mut db_guard = backend.shared.db.lock().await;
            if let Some(ned_file) = db_guard.get_source_file_input(old_url) {
                ned_file.set_rope(&mut *db_guard).to(Rope::from_str("")); // empty rope
            }
            db_guard.remove_source_file_input(old_url);
            backend
                .shared
                .diagnostics_publisher
                .request_clear_files(vec![old_url.clone()]);
        }

        // File moved into nedfolder → index it
        (false, true) => {
            backend
                .shared
                .client
                .log_message(
                    MessageType::INFO,
                    format!(
                        "[+] File moved into tracked folders, indexing: {} -> {}",
                        old_url, new_url
                    ),
                )
                .await;

            // Index the new file
            if let Ok(new_path) = new_url.to_file_path() {
                indexing::index_files(
                    vec![new_path],
                    backend.shared.clone(),
                    salsa::Durability::LOW,
                )
                .await;
            }
            backend
                .shared
                .diagnostics_publisher
                .request_clear_files(vec![old_url.clone()]);
        }

        // File renamed within nedfolder
        (true, true) => {
            backend
                .shared
                .client
                .log_message(
                    MessageType::INFO,
                    format!(
                        "[*] File renamed within tracked folders: {} -> {}",
                        old_url, new_url
                    ),
                )
                .await;

            // Update database
            let mut db_guard = backend.shared.db.lock().await;
            if let Some(ned_file) = db_guard.get_source_file_input(old_url) {
                let rope = ned_file.rope(&*db_guard).clone();
                ned_file.set_uri(&mut *db_guard).to(new_url.clone());
                ned_file.rename(&mut db_guard, new_url.clone());
                ned_file.set_rope(&mut *db_guard).to(rope.clone());

                backend
                    .shared
                    .client
                    .log_message(
                        MessageType::INFO,
                        format!("[+] Updated file URI in DB: {} -> {}", old_url, new_url),
                    )
                    .await;
            } else {
                backend
                    .shared
                    .client
                    .log_message(
                        MessageType::WARNING,
                        format!("[!] File not found in database: {}", old_url),
                    )
                    .await;
            }
            backend
                .shared
                .diagnostics_publisher
                .request_clear_files(vec![old_url.clone()]);
        }

        // File outside tracked folders renamed → ignore
        (false, false) => {
            backend
                .shared
                .client
                .log_message(
                    MessageType::INFO,
                    format!(
                        "[*] File renamed outside tracked folders (ignored): {} -> {}",
                        old_url, new_url
                    ),
                )
                .await;
        }
    }
}

pub async fn handle_folder_rename(backend: &Backend, old_folder_url: &Url, new_folder_url: &Url) {
    backend
        .shared
        .client
        .log_message(
            MessageType::INFO,
            format!(
                "Handling folder rename: {} -> {}",
                old_folder_url, new_folder_url
            ),
        )
        .await;

    let old_prefix = old_folder_url.as_str();
    let new_prefix = new_folder_url.as_str();

    // Collect all files that need to be renamed
    let mut files_to_rename: Vec<(Url, Url)> = {
        let db_guard = backend.shared.db.lock().await;
        let ned_file_inputs = db_guard.get_all_source_file_inputs();

        ned_file_inputs
            .iter()
            .filter_map(|ned_file| {
                let uri = ned_file.uri(&*db_guard);
                let uri_str = uri.as_str();

                // Check if this file is under the old folder
                if uri_str.starts_with(old_prefix) {
                    let new_uri_str = uri_str.replacen(old_prefix, new_prefix, 1);
                    if let Ok(new_uri) = Url::parse(&new_uri_str) {
                        Some((uri.clone(), new_uri))
                    } else {
                        None
                    }
                } else {
                    None
                }
            })
            .collect()
    };

    // If folder from a non nedfolder was "renamed" or moved into a nedfolder, we cannot
    // consult the database for the old files but have to scan the filesystem
    if files_to_rename.is_empty()
        && let Ok(new_folder_path) = new_folder_url.to_file_path()
    {
        for file in utils::find_all_ned_files(&new_folder_path) {
            let file_url_res = Url::from_file_path(file.clone());
            if let Ok(file_url) = file_url_res {
                files_to_rename.push((old_folder_url.clone(), file_url));
            }
        }
    }

    backend
        .shared
        .client
        .log_message(
            MessageType::INFO,
            format!("Found {} files to rename in folder", files_to_rename.len()),
        )
        .await;

    // Now rename each file without holding the lock across iterations
    for (old_file_url, new_file_url) in files_to_rename {
        handle_file_rename(backend, &old_file_url, &new_file_url).await;
    }

    backend
        .shared
        .client
        .log_message(MessageType::INFO, "Folder rename complete".to_string())
        .await;
}
