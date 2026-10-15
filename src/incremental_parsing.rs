use crate::SharedState;
use crate::db::Db;
use crate::parser::Parser;
use crate::utils;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use std::time::Duration;
use tokio::time::{Instant, sleep};

use salsa::Setter;
use tokio::sync::Notify;
use tower_lsp::lsp_types::DidChangeTextDocumentParams;

use tokio::sync::{Mutex, mpsc};

use tokio::task;

use log::debug;

use tower_lsp::lsp_types::notification::Progress;
use tower_lsp::lsp_types::*;

use ropey::Rope;

#[derive(Debug, Clone)]
pub enum UpdateJob {
    Incremental(DidChangeTextDocumentParams),
    Full(TextDocumentItem),
    // Force immediate analysis without debouncing → currently triggering two analyses if auto-save
    // is enabled but since i am using the smart diagnostics it should be fine and performant
    // enough
    ForceAnalysis(Url),
}

#[derive(Debug)]
pub struct ChangeProcessor {
    sender: mpsc::UnboundedSender<UpdateJob>,
    _worker_handle: Arc<Mutex<Option<task::JoinHandle<()>>>>,
    is_active: Arc<AtomicBool>,
    start_signal: Arc<Notify>,
}

impl ChangeProcessor {
    pub fn new(backend: Arc<SharedState>) -> Self {
        let (tx, rx) = mpsc::unbounded_channel::<UpdateJob>();

        let processor = Self {
            sender: tx,
            _worker_handle: Arc::new(Mutex::new(None)),
            is_active: Arc::new(AtomicBool::new(false)),
            start_signal: Arc::new(Notify::new()),
        };

        processor.start_worker(rx, backend);
        processor
    }

    pub fn start(&self) {
        if !self.is_active.load(Ordering::SeqCst) {
            self.is_active.store(true, Ordering::SeqCst);
            self.start_signal.notify_one();
        }
    }

    pub fn stop(&self) {
        if self.is_active.load(Ordering::SeqCst) {
            self.is_active.store(false, Ordering::SeqCst);
        }
    }

    pub fn enqueue_job(&self, job: UpdateJob) {
        if self.sender.send(job).is_err() {
            log::error!(
                "Failed to send job to update worker. The worker thread may have panicked."
            );
        } else if !self.is_active.load(Ordering::Relaxed) {
            debug!("Job queued (worker is paused)");
        }
    }
    fn start_worker(&self, mut rx: mpsc::UnboundedReceiver<UpdateJob>, backend: Arc<SharedState>) {
        let is_active = self.is_active.clone();
        let start_signal = self.start_signal.clone();
        let handle_slot = self._worker_handle.clone();

        let handle = task::spawn(async move {
            let mut parser = Parser::new();

            // Debounce state
            let debounce_duration = Duration::from_millis(500);
            let sleep = sleep(Duration::MAX); // Initialize with a long sleep 
            tokio::pin!(sleep);

            let mut pending_analysis_file: Option<(Url, i32)> = None;

            loop {
                // Pause Logic
                if !is_active.load(Ordering::SeqCst) {
                    debug!("Worker paused. Waiting for signal...");
                    start_signal.notified().await;
                    debug!("Worker resumed.");
                }

                tokio::select! {
                     job = rx.recv() => {
                        let Some(job) = job else {
                            debug!("Job channel closed - update worker exiting");
                            break;
                        };

                        match job {
                            UpdateJob::Incremental(changes) => {
                                let uri = changes.text_document.uri.clone();
                                let version = changes.text_document.version;

                                debug!("Worker: Processing INCREMENTAL v{}", version);

                                // Apply edits fast
                                if let Err(e) = process_incremental_edit_only(&changes, &backend, &mut parser).await {
                                    log::error!("Failed to apply incremental edits for {}: {}", uri, e);
                                    // on fail we should just skip analysis
                                    pending_analysis_file = None;
                                } else {
                                    // queue for analysis and reset the timer
                                    pending_analysis_file = Some((uri, version));
                                    sleep.as_mut().reset(Instant::now() + debounce_duration);
                                }
                            }
                            UpdateJob::Full(doc) => {
                                let uri = doc.uri.clone();
                                debug!("Worker: FULL update for {}", uri);

                                // Cancel any pending analysis for this file
                                pending_analysis_file = None;

                                // Infinite timer
                                sleep.as_mut().reset(Instant::now() + Duration::from_secs(86400));

                                process_full_update(doc, &backend).await;
                            }
                            UpdateJob::ForceAnalysis(uri) => {
                                debug!("Worker: FORCE analysis for {}", uri);
                                pending_analysis_file = None; // Clear pending debounce
                                run_analysis_for_file(&uri, -1, &backend).await;
                            }
                        }
                    }

                    // Handle Debounce Timer
                    () = &mut sleep, if pending_analysis_file.is_some() => {
                        if let Some((uri, version)) = pending_analysis_file.take() {
                            run_analysis_for_file(&uri, version, &backend).await;
                        }

                        // Reset timer to "infinite" wait
                        sleep.as_mut().reset(Instant::now() + Duration::from_secs(86400));
                    }
                }
            }
        });

        // Store the handle in case we need to abort it later
        tokio::spawn(async move {
            let mut h = handle_slot.lock().await;
            *h = Some(handle);
        });
    }
}
/// Apply incremental edits to rope and tree (fast, no analysis)
async fn process_incremental_edit_only(
    changes: &DidChangeTextDocumentParams,
    backend: &SharedState,
    parser: &mut Parser,
) -> Result<(), String> {
    let uri = &changes.text_document.uri;

    let mut rope;
    let mut tree;
    let ned_file;
    {
        let db = backend.db.lock().await;
        ned_file = db.get_source_file_input(uri).ok_or("Document not found")?;
        rope = ned_file.rope(&*db).clone();
        tree = db.get_ast_for_file(&ned_file).ok_or("AST not found")?;
    }

    let mut full_replacement = false;

    for change in &changes.content_changes {
        match change.range {
            Some(range) => {
                let len_lines = rope.len_lines();
                let len_chars = rope.len_chars();

                // Some safety checks
                if range.start.line as usize >= len_lines || range.end.line as usize > len_lines {
                    return Err(format!(
                        "Line range out of bounds: {:?} (max lines: {})",
                        range, len_lines
                    ));
                }

                let start_char = utils::position_to_char(&rope, range.start);
                let end_char = utils::position_to_char(&rope, range.end);

                if start_char > len_chars || end_char > len_chars || start_char > end_char {
                    return Err(format!(
                        "Char range out of bounds: {}..{} (max chars: {})",
                        start_char, end_char, len_chars
                    ));
                }

                // if safe → calculate bytes
                let start_byte = rope.char_to_byte(start_char);
                let old_end_byte = rope.char_to_byte(end_char);

                // INFO: the points must be computed on the rope before the edit —
                // the old end may not exist in the new rope anymore
                let start_position = utils::byte_to_point(&rope, start_byte);
                let old_end_position = utils::byte_to_point(&rope, old_end_byte);

                rope.remove(start_char..end_char);
                rope.insert(start_char, &change.text);

                let new_end_byte = start_byte + change.text.len();
                let new_end_pos = utils::byte_to_point(&rope, new_end_byte);

                tree.edit(&tree_sitter::InputEdit {
                    start_byte,
                    old_end_byte,
                    new_end_byte,
                    start_position,
                    old_end_position,
                    new_end_position: new_end_pos,
                });
            }
            None => {
                // full text replacement → the old tree is useless as a reparse base
                rope = ropey::Rope::from_str(&change.text);
                full_replacement = true;
            }
        }
    }

    let old_tree = if full_replacement {
        None
    } else {
        Some(tree.clone())
    };
    let new_tree = parser.parse_with_options(
        uri,
        &mut |byte_offset, _position| {
            if byte_offset >= rope.len_bytes() {
                return ""; // Return empty slice at EOF
            }
            let (chunk, chunk_byte_idx, _, _) = rope.chunk_at_byte(byte_offset);
            // We need the slice of the chunk starting from our offset
            &chunk[byte_offset - chunk_byte_idx..]
        },
        old_tree.as_ref(),
    );

    if let Some(parsed_tree) = new_tree {
        tree = parsed_tree;
    } else {
        return Err("Tree-sitter parse failed".to_string());
    }

    {
        let mut db = backend.db.lock().await;
        ned_file.set_rope(&mut *db).to(rope);
        db.set_ast_for_file(ned_file, tree);
    }

    Ok(())
}

/// Run full analysis and diagnostics for a file (slower, debounced)
async fn run_analysis_for_file(uri: &Url, version: i32, backend: &SharedState) {
    {
        let mut db = backend.db.lock().await;
        if let Some(ned_file) = db.get_source_file_input(uri) {
            // eprintln!("{:?}", ned_file);
            debug!(
                "[*] Analyzing {} (rope size: {} bytes)",
                uri,
                ned_file.rope(&*db).len_bytes()
            );

            // Client only sends version on_change not on_save → use -1 as indicator to not update
            if version >= 0 {
                ned_file.set_version(&mut *db).to(version); // sync with client version
            }
        } else {
            backend
                .client
                .log_message(
                    MessageType::WARNING,
                    format!("Cannot analyze {} - file not in database", uri),
                )
                .await;
            return;
        }
    } // db

    backend
        .diagnostics_publisher
        .request_publish_all_priority(uri.clone());

    debug!("[+] Analysis complete for {}", uri);
}

/// Process a full document update: re-parse, re-index, and update all maps.
pub async fn process_full_update(item: TextDocumentItem, backend: &SharedState) {
    let uri = item.uri;
    let content = item.text;
    let version = item.version;

    let token = ProgressToken::String(format!("indexing_{}", uri));
    backend
        .client
        .send_notification::<Progress>(ProgressParams {
            token: token.clone(),
            value: ProgressParamsValue::WorkDone(WorkDoneProgress::Begin(WorkDoneProgressBegin {
                title: format!("Indexing {}", uri),
                message: Some("Parsing file...".into()),
                cancellable: Some(false),
                percentage: None,
            })),
        })
        .await;

    let old_ast = {
        let db = backend.db.lock().await;
        db.get_source_file_input(&uri)
            .and_then(|ned_file| db.get_ast_for_file(&ned_file))
    };

    let rope = Rope::from_str(&content);
    let mut parser = Parser::new();

    let new_tree = match parser.parse(&uri, &content, old_ast.as_ref()) {
        Some(tree) => tree,
        None => {
            backend
                .client
                .log_message(MessageType::ERROR, format!("Failed to parse file: {}", uri))
                .await;
            return;
        }
    };

    {
        let mut db = backend.db.lock().await;
        match db.get_source_file_input(&uri) {
            Some(ned_file) => {
                db.set_ast_for_file(ned_file, new_tree);
                ned_file.set_rope(&mut *db).to(rope.clone());
            }
            None => {
                // File opened from outside the indexed nedfolders → register it
                // so completion/goto/diagnostics work instead of failing silently.
                let handle =
                    crate::hir::SourceFile::new(&*db, Rope::from_str(""), uri.clone(), version);
                db.set_ast_for_file(handle, new_tree);
                handle.set_rope(&mut *db).to(rope.clone());
                db.set_ned_file_input(uri.clone(), handle);

                db.update_workspace_structure();
            }
        }
    }

    backend
        .diagnostics_publisher
        .request_publish_all_priority(uri.clone());

    backend
        .client
        .send_notification::<Progress>(ProgressParams {
            token: token.clone(),
            value: ProgressParamsValue::WorkDone(WorkDoneProgress::End(WorkDoneProgressEnd {
                message: Some("Indexing complete!".into()),
            })),
        })
        .await;
}
