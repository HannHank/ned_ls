use crate::db::Db;
use crate::hir;
use crate::semantics;
use crate::utils;
use crate::workspace_membership::{self, Membership};

use dashmap::DashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::{Mutex, mpsc};
use tower_lsp::lsp_types::*;

use tower_lsp::Client;

use log::debug;

#[derive(Debug, Clone)]
pub enum DiagnosticsRequest {
    PublishAll(Option<Url>),
    PublishFile(Url),
    ClearFiles(Vec<Url>),
}

#[derive(Debug)]
pub struct DiagnosticsPublisher {
    tx: mpsc::UnboundedSender<DiagnosticsRequest>,
}

impl DiagnosticsPublisher {
    pub fn new(
        client: Client,
        db: Arc<Mutex<crate::db::RootLspDatabase>>,
        published_diagnostics: Arc<DashMap<Url, u64>>,
        state: Arc<AtomicBool>,
    ) -> Self {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut current_handle: Option<tokio::task::JoinHandle<()>> = None;
        let mut current_cancel: Option<Arc<AtomicBool>> = None;
        let mut pending_publish: Option<DiagnosticsRequest> = None;

        tokio::spawn(async move {
            loop {
                // Process pending request if no task is running
                if let Some(request) = pending_publish.take() {
                    if current_handle.is_none() {
                        let cancelled = Arc::new(AtomicBool::new(false));
                        current_cancel = Some(cancelled.clone());

                        let client_clone = client.clone();
                        let db_clone = db.clone();
                        let published_clone = published_diagnostics.clone();
                        let state_clone = state.clone();

                        let handle = tokio::spawn(async move {
                            match request {
                                DiagnosticsRequest::PublishAll(priority_uri) => {
                                    publish_all_diagnostics(
                                        client_clone,
                                        db_clone,
                                        published_clone,
                                        state_clone,
                                        cancelled,
                                        priority_uri,
                                    )
                                    .await;
                                }
                                DiagnosticsRequest::PublishFile(uri) => {
                                    publish_file_diagnostics(
                                        client_clone,
                                        db_clone,
                                        published_clone,
                                        state_clone,
                                        cancelled,
                                        uri,
                                    )
                                    .await;
                                }
                                _ => {}
                            }
                        });

                        current_handle = Some(handle);
                    } else {
                        // Put it back if we can't process it yet
                        pending_publish = Some(request);
                    }
                }

                tokio::select! {
                    Some(req) = rx.recv() => {
                        match req {
                            DiagnosticsRequest::ClearFiles(uris) => {
                                let client = client.clone();
                                let published = published_diagnostics.clone();
                                tokio::spawn(async move {
                                    clear_diagnostics(client, published, uris).await;
                                });
                            }

                            DiagnosticsRequest::PublishAll(priority) => {
                                pending_publish = Some(DiagnosticsRequest::PublishAll(priority));
                                if let Some(cancel) = &current_cancel {
                                    cancel.store(true, Ordering::SeqCst);
                                }
                            }

                            DiagnosticsRequest::PublishFile(uri) => {
                                match &pending_publish {
                                    Some(DiagnosticsRequest::PublishAll(_)) => {
                                        // Don't override PublishAll with PublishFile
                                    }
                                    _ => {
                                        pending_publish = Some(DiagnosticsRequest::PublishFile(uri));
                                        if let Some(cancel) = &current_cancel {
                                            cancel.store(true, Ordering::SeqCst);
                                        }
                                    }
                                }
                            }
                        }
                        // Loop will restart and process pending_publish
                    }

                    result = async {
                        match current_handle.as_mut() {
                            Some(handle) => handle.await,
                            None => std::future::pending().await, // Never resolves
                        }
                    }, if current_handle.is_some() => {
                        // Task completed
                        current_handle = None;
                        current_cancel = None;

                        if let Err(e) = result {
                         log::error!("Diagnostics task failed: {:?}", e);
                         client.log_message(
                                MessageType::ERROR,
                                format!("Diagnostics task failed: {:?}", e)
                            ).await;

                        }
                        // Loop will restart and check for pending_publish
                    }
                }
            }
        });

        Self { tx }
    }

    pub fn request_publish_all(&self) {
        if let Err(e) = self.tx.send(DiagnosticsRequest::PublishAll(None)) {
            log::error!("Failed to send diagnostics request: {:?}", e);
        }
    }

    pub fn request_publish_all_priority(&self, uri: Url) {
        if let Err(e) = self.tx.send(DiagnosticsRequest::PublishAll(Some(uri))) {
            log::error!("Failed to send diagnostics request: {:?}", e);
        }
    }

    #[allow(dead_code)]
    /// Request publishing diagnostics for a specific file
    pub fn request_publish_file(&self, uri: Url) {
        if let Err(e) = self.tx.send(DiagnosticsRequest::PublishFile(uri)) {
            log::error!("Failed to send diagnostics request: {:?}", e);
        }
    }

    /// Request clearing diagnostics → otherwise the error markers will persist in the client
    pub fn request_clear_files(&self, uris: Vec<Url>) {
        if let Err(e) = self.tx.send(DiagnosticsRequest::ClearFiles(uris)) {
            log::error!("Failed to send clear request: {:?}", e);
        }
    }
}

/// Publish diagnostics for all files in the workspace
async fn publish_all_diagnostics(
    client: Client,
    db: Arc<Mutex<crate::db::RootLspDatabase>>,
    published_diagnostics: Arc<DashMap<Url, u64>>,
    state: Arc<AtomicBool>,
    cancelled: Arc<AtomicBool>,
    priority_uri: Option<Url>,
) {
    if !state.load(Ordering::SeqCst) {
        debug!("Skipping diagnostics: server not running");
        return;
    }

    debug!("[*] Running smart diagnostics check");

    let db_snapshot = {
        let db_guard = db.lock().await;
        db_guard.clone()
    };

    let inputs = db_snapshot.get_all_source_file_inputs();

    let mut ned_files: Vec<_> = inputs
        .iter()
        .filter(|f| f.uri(&db_snapshot).path().ends_with(".ned"))
        .collect();
    let mut ini_files: Vec<_> = inputs
        .iter()
        .filter(|f| f.uri(&db_snapshot).path().ends_with(".ini"))
        .collect();

    // Process the actively-edited file first for immediate feedback → this is necessary after the
    // INI file support was added.
    // basically just putting the current file up front
    if let Some(ref prio) = priority_uri {
        let sort_priority = |files: &mut Vec<&hir::SourceFile>| {
            files.sort_by_key(|f| f.uri(&db_snapshot) != prio);
        };
        sort_priority(&mut ned_files);
        sort_priority(&mut ini_files);
    }

    let mut published_count = 0;
    let mut skipped_count = 0;

    // NED files → instant feedack
    for ned_file_input in &ned_files {
        if cancelled.load(Ordering::Relaxed) {
            return;
        }
        match compute_file_diagnostics(&db_snapshot, **ned_file_input, &published_diagnostics) {
            FileResult::Published(uri, diags, version, hash) => {
                client
                    .publish_diagnostics(uri.clone(), diags, Some(version))
                    .await;
                published_diagnostics.insert(uri, hash);
                published_count += 1;
            }
            FileResult::Skipped => skipped_count += 1,
            FileResult::Cancelled => return,
            FileResult::Error(msg) => {
                client.log_message(MessageType::ERROR, msg).await;
            }
        }
    }

    // INI files → because of the heavy wildcard expansion, I decided to pospone the INI diagnostics until after the NED files are processed.
    // this makes the ned files way more responsive
    for ini_file_input in &ini_files {
        if cancelled.load(Ordering::Relaxed) {
            return;
        }
        match compute_file_diagnostics(&db_snapshot, **ini_file_input, &published_diagnostics) {
            FileResult::Published(uri, diags, version, hash) => {
                client
                    .publish_diagnostics(uri.clone(), diags, Some(version))
                    .await;
                published_diagnostics.insert(uri, hash);
                published_count += 1;
            }
            FileResult::Skipped => skipped_count += 1,
            FileResult::Cancelled => return,
            FileResult::Error(msg) => {
                client.log_message(MessageType::ERROR, msg).await;
            }
        }
    }

    debug!(
        "[+] Smart diagnostics: {} published, {} skipped (unchanged)",
        published_count, skipped_count
    );
}

enum FileResult {
    Published(Url, Vec<Diagnostic>, i32, u64),
    Skipped,
    Cancelled,
    Error(String),
}

/// Compute diagnostics for a single file, returning the result without publishing
fn compute_file_diagnostics(
    db: &crate::db::RootLspDatabase,
    file_input: hir::SourceFile,
    published_diagnostics: &DashMap<Url, u64>,
) -> FileResult {
    let uri = file_input.uri(db);
    let ext = uri.path().rsplit('.').next().unwrap_or_default();

    let syntax_diagnostics_result = std::panic::catch_unwind(AssertUnwindSafe(|| match ext {
        "ned" => crate::hir::ned::lower_to_hir::accumulated::<hir::Diagnostic>(db, file_input),
        "ini" => crate::hir::ini::lower_to_hir::accumulated::<hir::Diagnostic>(db, file_input),
        _ => vec![],
    }));

    let syntax_diagnostics = match syntax_diagnostics_result {
        Ok(diags) => diags,
        Err(payload) => {
            if payload.is::<salsa::Cancelled>() {
                return FileResult::Cancelled;
            }
            let msg = if let Some(s) = payload.downcast_ref::<&str>() {
                s.to_string()
            } else if let Some(s) = payload.downcast_ref::<String>() {
                s.clone()
            } else {
                "Unknown panic".to_string()
            };
            return FileResult::Error(format!("Syntax CRASH in {}: {}", uri, msg));
        }
    };

    let semantic_diagnostics_result = std::panic::catch_unwind(AssertUnwindSafe(|| match ext {
        "ned" => semantics::semantic_analysis(db, file_input).clone(),
        "ini" => semantics::ini_semantic_analysis(db, file_input).clone(),
        _ => vec![],
    }));

    let semantic_diagnostics = match semantic_diagnostics_result {
        Ok(diags) => diags,
        Err(payload) => {
            if payload.is::<salsa::Cancelled>() {
                return FileResult::Cancelled;
            }
            let msg = if let Some(s) = payload.downcast_ref::<&str>() {
                s.to_string()
            } else if let Some(s) = payload.downcast_ref::<String>() {
                s.clone()
            } else {
                "Unknown panic".to_string()
            };
            return FileResult::Error(format!("Semantic CRASH in {}: {}", uri, msg));
        }
    };

    let rope_version = file_input.version(db);

    let valid_diagnostics: Vec<hir::Diagnostic> = syntax_diagnostics
        .into_iter()
        .cloned()
        .chain(semantic_diagnostics)
        .filter(|diag| diag.version == *rope_version)
        .collect();

    let membership_diagnostic = membership_diagnostic(db, uri);

    // Mix the membership warning into the change-suppression hash → otherwise a file whose content
    // diagnostics did not change would be skipped and the warning would never reach the client
    let mut hasher = DefaultHasher::new();
    utils::hash_hir_diagnostics(&valid_diagnostics).hash(&mut hasher);
    membership_diagnostic
        .as_ref()
        .and_then(|d| d.code.clone())
        .map(|c| match c {
            NumberOrString::String(s) => s,
            NumberOrString::Number(n) => n.to_string(),
        })
        .hash(&mut hasher);
    let current_hash = hasher.finish();

    let should_publish = published_diagnostics
        .get(uri)
        .map(|last_hash| *last_hash != current_hash)
        .unwrap_or(true);

    if !should_publish {
        return FileResult::Skipped;
    }

    let rope = file_input.rope(db);
    let lsp_diagnostics: Vec<Diagnostic> = membership_diagnostic
        .into_iter()
        .chain(valid_diagnostics.iter().filter_map(|diag| {
            Some(Diagnostic {
                range: Range {
                    start: utils::byte_to_lsp_position(rope, diag.start)?,
                    end: utils::byte_to_lsp_position(rope, diag.end)?,
                },
                severity: Some(diag.severity.into()),
                source: Some("ned_ls".into()),
                message: diag.message.clone(),
                code: None,
                code_description: None,
                related_information: None,
                tags: None,
                data: None,
            })
        }))
        .collect();

    FileResult::Published(uri.clone(), lsp_diagnostics, *rope_version, current_hash)
}

/// Build the file-level warning for a file that is not covered by the indexed scope.
///
/// Is not tracked by salsa because it does not depend on the file content
fn membership_diagnostic(db: &crate::db::RootLspDatabase, uri: &Url) -> Option<Diagnostic> {
    if uri.scheme() != "file" {
        return None;
    }

    // No workspace (single-file mode) → there is no config to suggest a change to.
    let config = db.get_config()?;
    let file_path = uri.to_file_path().ok()?;

    let membership = workspace_membership::classify(&file_path, config);
    if matches!(membership, Membership::Indexed) {
        return None;
    }

    Some(Diagnostic {
        range: Range {
            start: Position::new(0, 0),
            end: Position::new(0, 0),
        },
        severity: Some(DiagnosticSeverity::WARNING),
        source: Some("ned_ls".into()),
        message: membership.message(),
        code: membership
            .diagnostic_code()
            .map(|c| NumberOrString::String(c.to_string())),
        code_description: None,
        related_information: None,
        tags: None,
        data: None,
    })
}

/// Publish diagnostics for a single file
async fn publish_file_diagnostics(
    client: Client,
    db: Arc<Mutex<crate::db::RootLspDatabase>>,
    published_diagnostics: Arc<DashMap<Url, u64>>,
    state: Arc<AtomicBool>,
    cancelled: Arc<AtomicBool>,
    uri: Url,
) {
    if !state.load(Ordering::SeqCst) {
        debug!("Skipping diagnostics for {}: server not running", uri);
        return;
    }

    // Check cancellation early
    if cancelled.load(Ordering::Relaxed) {
        return;
    }

    let db_snapshot = {
        let db_guard = db.lock().await;
        db_guard.clone()
    };

    let ned_file_input = match db_snapshot.get_source_file_input(&uri) {
        Some(file) => file,
        None => {
            client
                .log_message(
                    MessageType::WARNING,
                    format!(
                        "Cannot publish diagnostics for {} - file not in database",
                        uri
                    ),
                )
                .await;
            return;
        }
    };

    if cancelled.load(Ordering::Relaxed) {
        return;
    }

    match compute_file_diagnostics(&db_snapshot, ned_file_input, &published_diagnostics) {
        FileResult::Published(uri, diags, version, hash) => {
            client
                .publish_diagnostics(uri.clone(), diags, Some(version))
                .await;
            published_diagnostics.insert(uri, hash);
        }
        FileResult::Skipped | FileResult::Cancelled => {}
        FileResult::Error(msg) => {
            client.log_message(MessageType::ERROR, msg).await;
        }
    }
}

/// Clear diagnostics for deleted files
async fn clear_diagnostics(
    client: Client,
    published_diagnostics: Arc<DashMap<Url, u64>>,
    uris: Vec<Url>,
) {
    for uri in uris {
        published_diagnostics.remove(&uri);
        client.publish_diagnostics(uri.clone(), vec![], None).await;
    }
}
