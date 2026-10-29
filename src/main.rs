mod completion;
mod context;
mod db;
mod diagnostics;
mod formatting;
mod goto_definition;
mod handlers;
mod hir;
mod incremental_parsing;
mod indexing;
mod parser;
mod semantics;
#[cfg(test)]
mod test_utils;
mod utils;
mod workspace_membership;

use crate::db::RootLspDatabase;
use crate::diagnostics::DiagnosticsPublisher;

use crate::completion::{ini, ned};

use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

use crate::db::Db;

use futures::future::join_all;
use std::env;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;

use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;
use tower_lsp::{Client, LanguageServer, LspService, Server};

use tower_lsp::lsp_types::notification::Progress;
use tower_lsp::lsp_types::{
    ProgressParams, ProgressParamsValue, ProgressToken, WorkDoneProgress, WorkDoneProgressBegin,
    WorkDoneProgressEnd,
};

// parallel hashmap for indexing
use dashmap::{DashMap, DashSet};
use std::collections::HashSet;

// tree-sitter stuff
pub type FQN = String;

#[derive(Debug, Clone)]
pub struct Config {
    /// Consulted by `workspace_membership::classify` to tell "folder missing from .nedfolders"
    /// apart from "file belongs to a project outside this workspace".
    pub imported_projects: HashSet<PathBuf>,
    pub project_root: Url,
    pub nedfolders: Vec<PathBuf>,
    // Ned_ls.toml exists → opp workspace
    pub has_config_file: bool,
    /// A .nedfolders file exists → opp workspace
    pub has_nedfolders: bool,
}

#[repr(u8)]
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum ServerState {
    Initializing = 0,
    Running = 1,
    ShuttingDown = 2,
}

impl From<u8> for ServerState {
    fn from(value: u8) -> Self {
        match value {
            0 => ServerState::Initializing,
            1 => ServerState::Running,
            2 => ServerState::ShuttingDown,
            _ => ServerState::Initializing, // fallback
        }
    }
}

#[derive(Debug)]
pub struct SharedState {
    pub client: Client,
    // This is important for autocompletion → feels a bit off, might change in future
    pub config: Mutex<Option<Config>>,
    pub db: Arc<Mutex<RootLspDatabase>>,
    pub state: AtomicU8,
    pub running: Arc<AtomicBool>, // This is somewhat redundant but useful for quick checks → might
    // need to create a facate later to gurantee consistency
    pub diagnostics_publisher: DiagnosticsPublisher,
    pub watched_files_dynamic_registration: AtomicBool,
    /// While a document is open the client owns its content → its buffer may hold unsaved edits
    pub open_documents: DashSet<Url>,
}

impl SharedState {
    /// Atomic Mutate of config (because it is located in db and sharedState)
    pub async fn update_config<F>(&self, f: F) -> Option<Config>
    where
        F: FnOnce(&mut Config),
    {
        let mut config_guard = self.config.lock().await;
        let mut db_guard = self.db.lock().await;

        let config = config_guard.as_mut()?;
        f(config);

        let updated = config.clone();
        db_guard.set_config(Some(updated.clone()));
        Some(updated)
    }
}

#[derive(Debug)]
pub struct Backend {
    pub shared: Arc<SharedState>,
    pub workspace_root: Mutex<Option<Url>>, // store the workspace root URI to index files later
    // partial update worker
    pub partial_update_worker: Arc<incremental_parsing::ChangeProcessor>,
}

#[tower_lsp::async_trait]
impl LanguageServer for Backend {
    async fn initialize(&self, params: InitializeParams) -> Result<InitializeResult> {
        let mut workspace: Option<Url> = None;

        // Clients that support this watch nothing until the server registers globs with them.
        self.shared.watched_files_dynamic_registration.store(
            params
                .capabilities
                .workspace
                .as_ref()
                .and_then(|w| w.did_change_watched_files.as_ref())
                .and_then(|d| d.dynamic_registration)
                .unwrap_or(false),
            Ordering::SeqCst,
        );

        if let Some(root_uri) = params.root_uri {
            match root_uri.to_file_path() {
                Ok(folder_path) => {
                    let workspace_uri = utils::find_file_upwards(&folder_path, ".project")
                        .and_then(|p| Url::from_file_path(p).ok()) // convert PathBuf -> Url
                        .unwrap_or_else(|| root_uri.clone()); // fallback to original folder_uri
                    // Update backend root
                    {
                        // TODO: try searching for a .nedfolders file

                        let mut root = self.workspace_root.lock().await;
                        *root = Some(workspace_uri.clone());
                        workspace = Some(workspace_uri.clone());
                    }
                }
                Err(_) => {
                    self.shared
                        .client
                        .show_message(
                            MessageType::WARNING,
                            "Workspace URI is not a valid file:// path",
                        )
                        .await;
                }
            }
        } else {
            self.shared
                .client
                .show_message(
                    MessageType::WARNING,
                    "No workspace folders provided in InitializeParams",
                )
                .await;
        }

        // Try to find config and parse
        if let Some(workspace_url) = workspace {
            match utils::discover_config(&workspace_url) {
                Some(discovery) => {
                    for (severity, text) in &discovery.messages {
                        if *severity == MessageType::WARNING {
                            self.shared.client.show_message(*severity, text).await;
                        } else {
                            self.shared.client.log_message(*severity, text).await;
                        }
                    }

                    let mut config = self.shared.config.lock().await;
                    let mut db_guard = self.shared.db.lock().await;

                    // initialize workspace_structure
                    db_guard.init_workspace_structure();

                    *config = Some(discovery.config.clone());

                    // set config also for the db to be used in queries
                    db_guard.set_config(Some(discovery.config));
                }
                None => {
                    self.shared
                        .client
                        .show_message(
                            MessageType::WARNING,
                            "Workspace URI is not a valid file:// path",
                        )
                        .await;
                }
            }
        } else {
            self.shared
                .client
                .show_message(MessageType::WARNING, "No workspace root set")
                .await;
        }

        Ok(InitializeResult {
            capabilities: ServerCapabilities {
                // INFO: telling the client to notify the server about workspace folder changes
                workspace: Some(WorkspaceServerCapabilities {
                    workspace_folders: Some(WorkspaceFoldersServerCapabilities {
                        supported: Some(true),
                        change_notifications: Some(OneOf::Left(true)),
                    }),
                    file_operations: Some(WorkspaceFileOperationsServerCapabilities {
                        did_create: Some(FileOperationRegistrationOptions {
                            filters: vec![
                                FileOperationFilter {
                                    scheme: Some("file".to_string()),
                                    pattern: FileOperationPattern {
                                        glob: "**/*.ned".to_string(),
                                        matches: Some(FileOperationPatternKind::File),
                                        options: None,
                                    },
                                },
                                // for ini files
                                FileOperationFilter {
                                    scheme: Some("file".to_string()),
                                    pattern: FileOperationPattern {
                                        glob: "**/*.ini".to_string(),
                                        matches: Some(FileOperationPatternKind::File),
                                        options: None,
                                    },
                                },
                            ],
                        }),
                        did_rename: Some(FileOperationRegistrationOptions {
                            filters: vec![
                                FileOperationFilter {
                                    scheme: Some("file".to_string()),
                                    pattern: FileOperationPattern {
                                        glob: "**/*.ned".to_string(),
                                        matches: Some(FileOperationPatternKind::File),
                                        options: None,
                                    },
                                },
                                FileOperationFilter {
                                    scheme: Some("file".to_string()),
                                    pattern: FileOperationPattern {
                                        glob: "**/*.ini".to_string(),
                                        matches: Some(FileOperationPatternKind::File),
                                        options: None,
                                    },
                                },
                                FileOperationFilter {
                                    scheme: Some("file".to_string()),
                                    pattern: FileOperationPattern {
                                        glob: "**/*".to_string(),
                                        matches: Some(FileOperationPatternKind::Folder),
                                        options: None,
                                    },
                                },
                            ],
                        }),
                        did_delete: Some(FileOperationRegistrationOptions {
                            filters: vec![
                                FileOperationFilter {
                                    scheme: Some("file".to_string()),
                                    pattern: FileOperationPattern {
                                        glob: "**/*.ned".to_string(),
                                        matches: Some(FileOperationPatternKind::File),
                                        options: None,
                                    },
                                },
                                FileOperationFilter {
                                    scheme: Some("file".to_string()),
                                    pattern: FileOperationPattern {
                                        glob: "**/*.ini".to_string(),
                                        matches: Some(FileOperationPatternKind::File),
                                        options: None,
                                    },
                                },
                                FileOperationFilter {
                                    scheme: Some("file".to_string()),
                                    pattern: FileOperationPattern {
                                        glob: "**/*".to_string(),
                                        matches: Some(FileOperationPatternKind::Folder),
                                        options: None,
                                    },
                                },
                            ],
                        }),
                        will_create: None,
                        will_rename: None,
                        will_delete: None,
                    }),
                }),
                // TODO: add syntax highlighting via semantic tokens and use the existing
                text_document_sync: Some(TextDocumentSyncCapability::Options(
                    TextDocumentSyncOptions {
                        open_close: Some(true),
                        change: Some(TextDocumentSyncKind::INCREMENTAL),
                        save: Some(TextDocumentSyncSaveOptions::SaveOptions(SaveOptions {
                            include_text: Some(true),
                        })),
                        ..Default::default()
                    },
                )),
                definition_provider: Some(OneOf::Left(true)),
                references_provider: Some(OneOf::Left(true)),
                rename_provider: Some(OneOf::Right(RenameOptions {
                    prepare_provider: Some(true),
                    work_done_progress_options: Default::default(),
                })),
                completion_provider: Some(CompletionOptions {
                    // descriptions of NED types are only read for the selected item
                    resolve_provider: Some(true), // this is very important otherwise the doc is not
                    // read lazy
                    trigger_characters: None,
                    work_done_progress_options: Default::default(),
                    all_commit_characters: None,
                    completion_item: None,
                }),
                hover_provider: Some(HoverProviderCapability::Simple(true)),
                document_formatting_provider: Some(OneOf::Left(true)),
                code_action_provider: Some(CodeActionProviderCapability::Options(
                    CodeActionOptions {
                        code_action_kinds: Some(vec![CodeActionKind::QUICKFIX]),
                        resolve_provider: Some(false),
                        work_done_progress_options: Default::default(),
                    },
                )),
                execute_command_provider: Some(ExecuteCommandOptions {
                    commands: handlers::commands::COMMANDS
                        .iter()
                        .map(|c| c.to_string())
                        .collect(),
                    work_done_progress_options: Default::default(),
                }),
                ..Default::default()
            },
            server_info: Some(ServerInfo {
                name: "ned_ls".to_string(),
                version: Some("0.1.0".to_string()),
            }),
        })
    }

    async fn initialized(&self, _: InitializedParams) {
        self.shared
            .client
            .log_message(MessageType::INFO, "NED LSP initialized!")
            .await;

        self.register_file_watchers().await;

        // INFO: indicate the start of indexing via progress notification

        let workspace_root_url = { self.workspace_root.lock().await.clone() };

        let backend = self.shared.clone();
        let mut ned_folders_to_index = Vec::new();

        // None when the editor was opened without a workspace (single file mode)
        let workspace_root_path = workspace_root_url
            .as_ref()
            .and_then(|url| url.to_file_path().ok());

        {
            let config_opt = self.shared.config.lock().await.clone();
            if let Some(config) = config_opt {
                ned_folders_to_index = config.nedfolders.clone();
            }
        }

        let mut handles = Vec::with_capacity(ned_folders_to_index.len());

        let backend_for_std = self.shared.clone();
        handles.push(tokio::spawn(async move {
            // Load the file content into the binary at compile time
            static STD_LIB_CONTENT: &str = include_str!("../resources/std.ned");

            indexing::index_in_memory_file(backend_for_std, STD_LIB_CONTENT, "std.ned").await;
        }));

        // Spawn one async indexing task per project
        // INFO: Not a performance gain but keeps the UI responsive
        for project_path in ned_folders_to_index {
            let backend = backend.clone(); // increase ref count 

            let project_name = project_path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("unknown");

            let token = ProgressToken::String(format!("indexing_{}", project_name));
            self.shared
                .client
                .send_notification::<Progress>(ProgressParams {
                    token: token.clone(),
                    value: ProgressParamsValue::WorkDone(WorkDoneProgress::Begin(
                        WorkDoneProgressBegin {
                            title: format!("Indexing {}", project_name),
                            message: Some("Discovering NED files...".into()),
                            cancellable: Some(false),
                            percentage: None,
                        },
                    )),
                })
                .await;

            // Small optimization for Salsa
            let durability = if workspace_root_path.as_deref() == Some(project_path.as_path()) {
                salsa::Durability::LOW
            } else {
                salsa::Durability::HIGH
            };

            // Each task runs in background
            handles.push(tokio::spawn(async move {
                backend
                    .client
                    .log_message(
                        MessageType::INFO,
                        format!(
                            "[*] Starting indexing of project: {}",
                            project_path.display()
                        ),
                    )
                    .await;

                // TODO: is there a more efficient way?
                indexing::index_project(Some(&project_path), backend.clone(), durability).await;

                backend
                    .client
                    .log_message(
                        MessageType::INFO,
                        format!("[+] Finished indexing: {}", project_path.display()),
                    )
                    .await;

                backend
                    .client
                    .send_notification::<Progress>(ProgressParams {
                        token: token.clone(),
                        value: ProgressParamsValue::WorkDone(WorkDoneProgress::End(
                            WorkDoneProgressEnd {
                                message: Some("Indexing complete.".into()),
                            },
                        )),
                    })
                    .await;
            }));
        }

        let shared_state = self.shared.clone();
        let worker = self.partial_update_worker.clone();
        tokio::spawn(async move {
            let results = join_all(handles).await;

            for res in results {
                if let Err(join_err) = res {
                    if join_err.is_cancelled() {
                        shared_state
                            .client
                            .log_message(
                                MessageType::ERROR,
                                "[!] Indexing task was cancelled!".to_string(),
                            )
                            .await;
                    } else if join_err.is_panic() {
                        shared_state
                            .client
                            .log_message(
                                MessageType::ERROR,
                                "[!] An indexing task PANICKED!".to_string(),
                            )
                            .await;
                    } else {
                        shared_state
                            .client
                            .log_message(
                                MessageType::ERROR,
                                format!("[!] Indexing task failed: {:?}", join_err),
                            )
                            .await;
                    }
                }
            }
            shared_state
                .client
                .log_message(MessageType::INFO, "[+] All projects indexed successfully.")
                .await;

            // server should stay silent if the workspace is not an opp workspace
            let is_omnetpp = {
                let db = shared_state.db.lock().await;
                crate::workspace_membership::is_omnetpp_workspace(&*db)
            };
            if !is_omnetpp {
                shared_state
                    .client
                    .log_message(
                        MessageType::INFO,
                        "[*] No NED files, .nedfolders or Ned_ls.toml found → INI features are disabled for this workspace.",
                    )
                    .await;
            }

            worker.start(); // start the incremental parsing worker after indexing
            shared_state
                .state
                .store(ServerState::Running as u8, Ordering::SeqCst);

            shared_state.running.store(true, Ordering::SeqCst);

            shared_state.diagnostics_publisher.request_publish_all();
        });

        self.shared
            .client
            .log_message(
                MessageType::INFO,
                "Background indexing started for all projects.",
            )
            .await;
    }

    // TODO: do the rest of the cleanup
    async fn shutdown(&self) -> Result<()> {
        self.partial_update_worker.stop();
        Ok(())
    }

    // TODO: add new files to the db and shared state
    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        crate::handlers::workspace::did_open(self, params).await
    }

    async fn did_save(&self, params: DidSaveTextDocumentParams) {
        crate::handlers::workspace::did_save(self, params).await
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        crate::handlers::workspace::did_change(self, params).await
    }

    #[allow(unused_variables)]
    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        crate::handlers::workspace::did_close(self, params).await
    }

    async fn did_create_files(&self, params: CreateFilesParams) {
        crate::handlers::workspace::did_create_files(self, params).await
    }

    async fn did_delete_files(&self, params: DeleteFilesParams) {
        crate::handlers::workspace::did_delete_files(self, params).await
    }

    async fn did_rename_files(&self, params: RenameFilesParams) {
        crate::handlers::workspace::did_rename_files(self, params).await
    }

    async fn did_change_watched_files(&self, params: DidChangeWatchedFilesParams) {
        crate::handlers::workspace::did_change_watched_files(self, params).await
    }

    async fn code_action(&self, params: CodeActionParams) -> Result<Option<CodeActionResponse>> {
        crate::handlers::code_action::code_action(self, params).await
    }

    async fn execute_command(
        &self,
        params: ExecuteCommandParams,
    ) -> Result<Option<serde_json::Value>> {
        crate::handlers::commands::execute_command(self, params).await
    }

    async fn completion_resolve(&self, item: CompletionItem) -> Result<CompletionItem> {
        let db = self.shared.db.lock().await.clone();
        let unresolved = item.clone();
        match salsa::Cancelled::catch(std::panic::AssertUnwindSafe(|| {
            completion::shared::resolve_item(&db, item)
        })) {
            Ok(item) => Ok(item),
            Err(_cancelled) => Ok(unresolved),
        }
    }

    async fn completion(&self, params: CompletionParams) -> Result<Option<CompletionResponse>> {
        let uri = &params.text_document_position.text_document.uri;
        let position = params.text_document_position.position;

        if uri.as_str().ends_with(".ini") {
            let db = self.shared.db.lock().await.clone();
            // INI files of unrelated projects should not get opp suggestions
            if !crate::workspace_membership::is_omnetpp_workspace(&db) {
                return Ok(None);
            }
            let items = salsa::Cancelled::catch(std::panic::AssertUnwindSafe(|| {
                db.get_source_file_input(uri).map(|ini_file| {
                    let rope = ini_file.rope(&db);
                    ini::completion(&db, ini_file, rope, position)
                })
            }));
            return match items {
                Ok(Some(items)) if !items.is_empty() => Ok(Some(CompletionResponse::Array(items))),
                Ok(_) => Ok(None),
                Err(_cancelled) => {
                    log::debug!("[!] ini completion cancelled by concurrent edit");
                    Ok(None)
                }
            };
        }

        let backend = self.shared.clone();

        let handle =
            tokio::spawn(
                async move { ned::completion(&params.text_document_position, backend).await },
            );

        match handle.await {
            Ok(Ok(completion_response)) => {
                let mut ret = Vec::with_capacity(completion_response.len());
                for (_, item) in completion_response {
                    match item {
                        ned::CompletionItemResponse::Import(typed_text, suggestions, range) => {
                            let is_inline_fqn =
                                typed_text.contains('.') || typed_text.contains('*');

                            let items = suggestions
                                .iter()
                                .map(|fqn| {
                                    let short_name =
                                        fqn.split('.').next_back().unwrap_or(fqn).to_string();

                                    // I find it more appealing to show the short names but still allow
                                    // fqns if a dot or star is used.
                                    let menu_label = if is_inline_fqn {
                                        fqn.clone()
                                    } else {
                                        short_name
                                    };

                                    CompletionItem {
                                        label: menu_label,
                                        kind: Some(CompletionItemKind::MODULE),
                                        detail: Some(format!("FQN: {}", fqn)),
                                        filter_text: Some(fqn.clone()),
                                        insert_text: Some(fqn.clone()),
                                        ..Default::default()
                                    }
                                })
                                .collect();
                            ret.extend(completion::shared::attach_range(items, range));
                        }
                        ned::CompletionItemResponse::GlobalSuggestions {
                            kind,
                            suggestions,
                            range,
                            import_insert_line,
                        } => {
                            let mut items = Vec::new();
                            for s in &suggestions {
                                // Check if we need auto import
                                let additional_text_edits = if s.needs_import {
                                    let insert_pos = Position::new(import_insert_line, 0);

                                    Some(vec![TextEdit {
                                        range: Range::new(insert_pos, insert_pos),
                                        new_text: format!("import {};\n", s.fqn),
                                    }])
                                } else {
                                    None
                                };

                                items.push(CompletionItem {
                                    label: s.name.clone(),
                                    kind: Some(kind),

                                    // full path shown to the user
                                    detail: if s.needs_import {
                                        Some(format!("Auto-import: {}", s.fqn))
                                    } else {
                                        Some(format!("FQN: {}", s.fqn))
                                    },

                                    insert_text: Some(s.name.clone()),

                                    // allow matching against the full path as well
                                    filter_text: Some(s.fqn.clone()),

                                    // rank the short-name or auto-import item above the fqn item
                                    sort_text: Some(format!("0_{}", s.name)),

                                    // Auto-import
                                    additional_text_edits,
                                    data: s.doc.clone(),
                                    ..Default::default()
                                });

                                // Alternative to the auto import → insert the fqn inline
                                if s.needs_import {
                                    items.push(CompletionItem {
                                        label: s.fqn.clone(),
                                        kind: Some(kind),
                                        detail: Some("Insert fully qualified name".to_string()),
                                        insert_text: Some(s.fqn.clone()),
                                        filter_text: Some(s.fqn.clone()),
                                        sort_text: Some(format!("1_{}", s.fqn)),
                                        data: s.doc.clone(),
                                        ..Default::default()
                                    });
                                }
                            }
                            ret.extend(completion::shared::attach_range(items, range));
                        }

                        ned::CompletionItemResponse::Keywords(keywords, range) => {
                            let items = keywords
                                .iter()
                                .map(|kw| CompletionItem {
                                    label: kw.to_string(),
                                    kind: Some(CompletionItemKind::KEYWORD),
                                    detail: Some(format!("Keyword {}", kw)),
                                    insert_text: Some(kw.to_string()),
                                    ..Default::default()
                                })
                                .collect();
                            ret.extend(completion::shared::attach_range(items, range));
                        }
                        ned::CompletionItemResponse::Package(package, range) => {
                            let items = vec![CompletionItem {
                                label: package.clone(),
                                kind: Some(CompletionItemKind::MODULE),
                                detail: Some(format!("Package {}", package)),
                                insert_text: Some(package.clone()),
                                ..Default::default()
                            }];
                            ret.extend(completion::shared::attach_range(items, range));
                        }
                        ned::CompletionItemResponse::Parameters(params, range) => {
                            let items = params
                                .iter()
                                .map(|p| CompletionItem {
                                    label: p.name.clone(),
                                    kind: Some(CompletionItemKind::FIELD),
                                    detail: p.detail.clone(),
                                    insert_text: Some(p.name.clone()),
                                    data: p.doc.clone(),
                                    ..Default::default()
                                })
                                .collect();
                            ret.extend(completion::shared::attach_range(items, range));
                        }
                        ned::CompletionItemResponse::Gates(gates, range) => {
                            let items = gates
                                .iter()
                                .map(|kw| CompletionItem {
                                    label: kw.clone(),
                                    kind: Some(CompletionItemKind::EVENT),
                                    detail: Some(format!("Gate {}", kw)),
                                    insert_text: Some(kw.clone()),
                                    ..Default::default()
                                })
                                .collect();
                            ret.extend(completion::shared::attach_range(items, range));
                        }
                        ned::CompletionItemResponse::Submodules(submodules, range) => {
                            let items = submodules
                                .iter()
                                .map(|kw| CompletionItem {
                                    label: kw.clone(),
                                    kind: Some(CompletionItemKind::CLASS),
                                    detail: Some(format!("Submodule {}", kw)),
                                    insert_text: Some(kw.clone()),
                                    ..Default::default()
                                })
                                .collect();
                            ret.extend(completion::shared::attach_range(items, range));
                        }

                        ned::CompletionItemResponse::Snippet(label, snippet, range) => {
                            let items = vec![CompletionItem {
                                label,
                                kind: Some(CompletionItemKind::SNIPPET),
                                detail: Some("Snippet".to_string()),
                                insert_text_format: Some(InsertTextFormat::SNIPPET),
                                insert_text: Some(snippet),
                                preselect: Some(true),
                                ..Default::default()
                            }];
                            ret.extend(completion::shared::attach_range(items, range));
                        }
                    }
                }
                Ok(Some(CompletionResponse::Array(ret)))
            }
            Ok(Err(e)) => {
                self.shared
                    .client
                    .log_message(
                        MessageType::WARNING,
                        format!("[!] Completion exited early: {}", e),
                    )
                    .await;

                Ok(Some(CompletionResponse::Array(vec![])))
            }
            Err(join_err) => {
                if join_err.is_panic() {
                    let panic_payload = join_err.into_panic();

                    // Salsa deliberately cancels current queries if the revision changes when the
                    // user is typing. This saves ressources and took me a long time to figure it
                    // out.
                    if panic_payload.downcast_ref::<salsa::Cancelled>().is_some() {
                        return Ok(Some(CompletionResponse::Array(vec![])));
                    }

                    let panic_msg = if let Some(s) = panic_payload.downcast_ref::<&str>() {
                        s.to_string()
                    } else if let Some(s) = panic_payload.downcast_ref::<String>() {
                        s.clone()
                    } else {
                        "Unknown panic payload".to_string()
                    };

                    self.shared
                        .client
                        .log_message(
                            MessageType::ERROR,
                            format!("[!] Completion Panic! Cause: {}", panic_msg),
                        )
                        .await;
                }

                Err(tower_lsp::jsonrpc::Error::internal_error())
            }
        }
    }

    async fn goto_definition(
        &self,
        params: GotoDefinitionParams,
    ) -> Result<Option<GotoDefinitionResponse>> {
        crate::handlers::goto_definition::handle(self, params).await
    }
    async fn hover(&self, params: HoverParams) -> Result<Option<Hover>> {
        crate::handlers::hover::handle(self, params).await
    }
    async fn references(&self, params: ReferenceParams) -> Result<Option<Vec<Location>>> {
        crate::handlers::references::handle(self, params).await
    }
    async fn prepare_rename(
        &self,
        params: TextDocumentPositionParams,
    ) -> Result<Option<PrepareRenameResponse>> {
        crate::handlers::rename::prepare(self, params).await
    }
    async fn rename(&self, params: RenameParams) -> Result<Option<WorkspaceEdit>> {
        crate::handlers::rename::handle(self, params).await
    }
    async fn formatting(&self, params: DocumentFormattingParams) -> Result<Option<Vec<TextEdit>>> {
        crate::handlers::formatting::handle(self, params).await
    }
}

impl SharedState {}

impl Backend {
    /// Ask the client to watch the files that make up the workspace, so external changes reach the server (git pull)
    ///
    /// But this only applies to the current folder
    async fn register_file_watchers(&self) {
        if !self
            .shared
            .watched_files_dynamic_registration
            .load(Ordering::SeqCst)
        {
            self.shared
                .client
                .log_message(
                    MessageType::INFO,
                    "Client does not support watched-file registration; \
                     external changes will only be picked up when a file is opened.",
                )
                .await;
            return;
        }

        let watchers = ["**/*.ned", "**/*.ini", "**/.nedfolders", "**/Ned_ls.toml"]
            .into_iter()
            .map(|glob| FileSystemWatcher {
                glob_pattern: GlobPattern::String(glob.to_string()),
                kind: None, // create + change + delete
            })
            .collect();

        let options = DidChangeWatchedFilesRegistrationOptions { watchers };
        let register_options = match serde_json::to_value(options) {
            Ok(value) => Some(value),
            Err(e) => {
                log::error!("could not encode watcher registration: {e}");
                return;
            }
        };

        let registration = Registration {
            id: "ned_ls-watched-files".to_string(),
            method: "workspace/didChangeWatchedFiles".to_string(),
            register_options,
        };

        match self
            .shared
            .client
            .register_capability(vec![registration])
            .await
        {
            Ok(()) => {
                self.shared
                    .client
                    .log_message(MessageType::INFO, "Watching workspace files for changes.")
                    .await;
            }
            Err(e) => {
                self.shared
                    .client
                    .log_message(
                        MessageType::WARNING,
                        format!("Could not register file watchers: {e}"),
                    )
                    .await;
            }
        }
    }

    pub fn new(client: Client, db: RootLspDatabase) -> Self {
        let db = Arc::new(Mutex::new(db));
        let state_flag = Arc::new(AtomicBool::new(false));

        // Create diagnostics publisher
        let diagnostics_publisher = DiagnosticsPublisher::new(
            client.clone(),
            db.clone(),
            Arc::new(DashMap::new()),
            state_flag.clone(),
        );

        let shared = Arc::new(SharedState {
            client,
            config: Mutex::new(None),
            db,
            state: AtomicU8::new(ServerState::Initializing as u8),
            running: state_flag,
            diagnostics_publisher,
            watched_files_dynamic_registration: AtomicBool::new(false),
            open_documents: DashSet::new(),
        });

        let worker = incremental_parsing::ChangeProcessor::new(shared.clone());

        Backend {
            workspace_root: Mutex::new(None),
            shared,
            partial_update_worker: Arc::new(worker),
        }
    }
}

#[tokio::main]
async fn main() {
    env_logger::init();

    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();

    let db = RootLspDatabase::new();

    let (service, socket) = LspService::new(|client| Backend::new(client, db));

    // version and commit info
    service
        .inner()
        .shared
        .client
        .log_message(
            MessageType::INFO,
            format!(
                "Starting ned_ls version {} (commit {})",
                env!("CARGO_PKG_VERSION"),
                env!("GIT_COMMIT_HASH")
            ),
        )
        .await;
    Server::new(stdin, stdout, socket).serve(service).await;
}
