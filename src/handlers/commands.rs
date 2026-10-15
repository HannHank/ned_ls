use crate::Backend;
use crate::db::Db;
use crate::indexing;
use crate::utils;
use crate::workspace_membership;

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::notification::Progress;
use tower_lsp::lsp_types::*;

use toml_edit::{Array, DocumentMut, value};

/// Append the folder to the owning projects .nedfolders → index it.
pub const ADD_NED_FOLDER: &str = "ned_ls.addNedFolder";
/// Append the project to the workspace's Ned_ls.toml → index its nedfolders.
pub const ADD_IMPORTED_PROJECT: &str = "ned_ls.addImportedProject";
/// Index a folder into the running server without touching any file on disk.
pub const INDEX_FOLDER: &str = "ned_ls.indexFolder";
/// Index a project's nedfolders into the running server without touching any file on disk.
pub const INDEX_PROJECT: &str = "ned_ls.indexProject";

pub const COMMANDS: &[&str] = &[
    ADD_NED_FOLDER,
    ADD_IMPORTED_PROJECT,
    INDEX_FOLDER,
    INDEX_PROJECT,
];

/// command triggered by code action
pub async fn execute_command(
    backend: &Backend,
    params: ExecuteCommandParams,
) -> Result<Option<serde_json::Value>> {
    let args: Vec<PathBuf> = params
        .arguments
        .iter()
        .filter_map(|v| v.as_str())
        .map(PathBuf::from)
        .collect();

    match params.command.as_str() {
        // add folder to the .nedfolders
        ADD_NED_FOLDER => {
            let [project_root, folder] = args.as_slice() else {
                return bad_args(backend, &params.command).await;
            };

            match add_to_nedfolders(project_root, folder) {
                Ok(()) => activate_folders(backend, vec![folder.clone()]).await,
                Err(e) => {
                    report_write_failure(backend, &project_root.join(".nedfolders"), &e).await;
                }
            }
        }

        // add project to the Ned_ls.toml
        ADD_IMPORTED_PROJECT => {
            let [workspace_root, project_root] = args.as_slice() else {
                return bad_args(backend, &params.command).await;
            };

            match add_to_ned_ls_toml(workspace_root, project_root) {
                Ok(()) => {
                    let folders = utils::find_and_parse_nedfolders(project_root);
                    register_imported_project(backend, project_root).await;
                    activate_folders(backend, folders).await;
                }
                Err(e) => {
                    report_write_failure(backend, &workspace_root.join("Ned_ls.toml"), &e).await;
                }
            }
        }

        // index a folder without touching any file on disk
        INDEX_FOLDER => {
            let [folder] = args.as_slice() else {
                return bad_args(backend, &params.command).await;
            };
            activate_folders(backend, vec![folder.clone()]).await;
        }

        // index a project without touching any file on disk
        INDEX_PROJECT => {
            let [project_root] = args.as_slice() else {
                return bad_args(backend, &params.command).await;
            };
            register_imported_project(backend, project_root).await;
            activate_folders(backend, utils::find_and_parse_nedfolders(project_root)).await;
        }

        other => {
            backend
                .shared
                .client
                .log_message(MessageType::WARNING, format!("Unknown command: {other}"))
                .await;
        }
    }

    Ok(None)
}

/// Add folders to the live nedfolder set and index them.
async fn activate_folders(backend: &Backend, folders: Vec<PathBuf>) {
    let folders: Vec<PathBuf> = folders
        .iter()
        .map(|f| workspace_membership::normalize(f))
        .collect();

    let updated = backend
        .shared
        .update_config(|config| {
            config.nedfolders.extend(folders.iter().cloned());
            // std::mem::take() to avoid double-borrowing config.nedfolders in the deduplication function
            let merged = std::mem::take(&mut config.nedfolders);
            config.nedfolders = utils::deduplicate_nested_roots(merged);
        })
        .await;

    if updated.is_none() {
        backend
            .shared
            .client
            .show_message(
                MessageType::ERROR,
                "Cannot index: the server is running without a workspace configuration.",
            )
            .await;
        return;
    }

    index_folders(backend, folders).await;
}

/// Index a set of folders and refresh the workspace, assuming the configuration already lists them.
async fn index_folders(backend: &Backend, folders: Vec<PathBuf>) {
    for folder in &folders {
        let name = folder
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("folder");

        // displaying current state to the client
        let token = ProgressToken::String(format!("indexing_{}", folder.display()));

        backend
            .shared
            .client
            .send_notification::<Progress>(ProgressParams {
                token: token.clone(),
                value: ProgressParamsValue::WorkDone(WorkDoneProgress::Begin(
                    WorkDoneProgressBegin {
                        title: format!("Indexing {name}"),
                        message: Some("Discovering NED files...".into()),
                        cancellable: Some(false),
                        percentage: None,
                    },
                )),
            })
            .await;

        indexing::index_project(Some(folder), backend.shared.clone(), salsa::Durability::LOW).await;

        backend
            .shared
            .client
            .send_notification::<Progress>(ProgressParams {
                token,
                value: ProgressParamsValue::WorkDone(WorkDoneProgress::End(WorkDoneProgressEnd {
                    message: Some("Indexing complete.".into()),
                })),
            })
            .await;
    }

    {
        let mut db_guard = backend.shared.db.lock().await;
        db_guard.update_workspace_structure();
    }

    // Republish everything → the new files need diagnostics, and previously unresolvable imports in
    // already-open files may now resolve.
    backend.shared.diagnostics_publisher.request_publish_all();
}

/// Re-run workspace discovery after .nedfolders or Ned_ls.toml changed on disk → index
/// anything the new configuration brought into scope.
pub async fn reload_workspace_config(backend: &Backend) {
    let workspace_url = {
        let config = backend.shared.config.lock().await;
        match config.as_ref() {
            Some(c) => c.project_root.clone(),
            None => return,
        }
    };

    let Some(discovery) = utils::discover_config(&workspace_url) else {
        return;
    };

    for (severity, text) in &discovery.messages {
        backend.shared.client.log_message(*severity, text).await;
    }

    let previous: Vec<PathBuf> = {
        let mut config_guard = backend.shared.config.lock().await;
        let mut db_guard = backend.shared.db.lock().await;

        let previous = config_guard
            .as_ref()
            .map(|c| c.nedfolders.clone())
            .unwrap_or_default();

        *config_guard = Some(discovery.config.clone());
        db_guard.set_config(Some(discovery.config.clone()));
        previous
    };

    let added: Vec<PathBuf> = discovery
        .config
        .nedfolders
        .iter()
        .filter(|folder| !previous.contains(folder))
        .cloned()
        .collect();

    backend
        .shared
        .client
        .log_message(
            MessageType::INFO,
            format!(
                "Workspace configuration reloaded; {} new folder(s) to index",
                added.len()
            ),
        )
        .await;

    if added.is_empty() {
        // The nedfolder set is unchanged, but set_config already bumped the workspace revision → so package expectations still need to be recomputed.
        backend.shared.diagnostics_publisher.request_publish_all();
        return;
    }

    index_folders(backend, added).await;
}

/// Record the project as known so the membership check stops reporting it as foreign → also for
/// session only
async fn register_imported_project(backend: &Backend, project_root: &Path) {
    let normalized = workspace_membership::normalize(project_root);
    backend
        .shared
        .update_config(|config| {
            config.imported_projects.insert(normalized);
        })
        .await;
}

/// Append folder to the projects .nedfolders.
fn add_to_nedfolders(project_root: &Path, folder: &Path) -> io::Result<()> {
    let path = project_root.join(".nedfolders");

    let rel = folder
        .strip_prefix(project_root)
        .map(|r| r.to_string_lossy().replace('\\', "/"))
        .unwrap_or_else(|_| folder.to_string_lossy().into_owned());
    let rel = if rel.is_empty() { ".".to_string() } else { rel };

    let existing = fs::read_to_string(&path).unwrap_or_default();

    let mut entries: Vec<String> = existing
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect();

    //
    if entries.is_empty() {
        entries.push(".".to_string());
    }

    if entries.iter().any(|e| e == &rel) {
        return Ok(());
    }
    entries.push(rel);

    let mut content = entries.join("\n");
    content.push('\n');
    fs::write(&path, content)
}

/// Append project_root to imported_projects in the workspaces Ned_ls.toml → creating the file
/// if needed.
/// Tries to express the path as relative to the workspace root, but falls back to absolute if no common prefix exists.
fn add_to_ned_ls_toml(workspace_root: &Path, project_root: &Path) -> io::Result<()> {
    let path = workspace_root.join("Ned_ls.toml");
    let toml = fs::read_to_string(&path).unwrap_or_default();

    let mut doc = toml.parse::<DocumentMut>().map_err(|e| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("Ned_ls.toml is not valid TOML: {e}"),
        )
    })?;

    // Format the entry as a relative path string
    let entry = pathdiff_relative(project_root, workspace_root)
        .unwrap_or_else(|| project_root.to_path_buf())
        .to_string_lossy()
        .replace('\\', "/");

    let target = workspace_membership::normalize(project_root);
    let mut already_present = false;

    // Check if the entry already exists by traversing the toml_edit Document
    if let Some(imported_projects) = doc.get("imported_projects")
        && let Some(array) = imported_projects.as_array()
    {
        already_present = array.iter().any(|item| {
            if let Some(existing_str) = item.as_str() {
                utils::resolve_path(Path::new(existing_str), workspace_root)
                    .map(|p| workspace_membership::normalize(&p) == target)
                    .unwrap_or(false)
            } else {
                false
            }
        });
    }

    if !already_present {
        // get existing or create new
        let projects_item = doc
            .entry("imported_projects")
            .or_insert_with(|| value(Array::new()));

        // check if it is an array
        let array = projects_item.as_array_mut().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "'imported_projects' exists but is not an array in Ned_ls.toml",
            )
        })?;

        // Push the new string entry to the TOML array
        array.push(entry);
    }

    // Write the document back to string
    fs::write(&path, doc.to_string())
}

/// Express path relative to base
/// If no common prefix → default to absolute path  
fn pathdiff_relative(target: &Path, base: &Path) -> Option<PathBuf> {
    let target = workspace_membership::normalize(target);
    let base = workspace_membership::normalize(base);

    let mut t = target.components().peekable();
    let mut b = base.components().peekable();

    while t.peek().is_some() && t.peek() == b.peek() {
        t.next();
        b.next();
    }

    // No shared root at all → a relative path would be meaningless
    if target.is_absolute()
        && base.is_absolute()
        && target.components().next() != base.components().next()
    {
        return None;
    }

    let mut result = PathBuf::new();
    //
    for _ in b {
        result.push("..");
    }
    for component in t {
        result.push(component);
    }

    (!result.as_os_str().is_empty()).then_some(result)
}

async fn report_write_failure(backend: &Backend, path: &Path, error: &io::Error) {
    backend
        .shared
        .client
        .show_message(
            MessageType::ERROR,
            format!("Could not update {}: {error}", path.display()),
        )
        .await;
}

async fn bad_args(backend: &Backend, command: &str) -> Result<Option<serde_json::Value>> {
    backend
        .shared
        .client
        .show_message(
            MessageType::ERROR,
            format!("{command} was invoked with unexpected arguments."),
        )
        .await;
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seeds_dot_when_creating_nedfolders() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("src")).unwrap();

        add_to_nedfolders(root, &root.join("src")).unwrap();
        // TODO
        assert_eq!(
            std::fs::read_to_string(root.join(".nedfolders")).unwrap(),
            ".\nsrc\n"
        );
    }

    #[test]
    fn appends_to_existing_nedfolders() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::write(root.join(".nedfolders"), "simulations\n").unwrap();

        add_to_nedfolders(root, &root.join("src")).unwrap();

        assert_eq!(
            std::fs::read_to_string(root.join(".nedfolders")).unwrap(),
            "simulations\nsrc\n"
        );
    }

    #[test]
    fn nedfolders_append_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();

        add_to_nedfolders(root, &root.join("src")).unwrap();
        add_to_nedfolders(root, &root.join("src")).unwrap();

        assert_eq!(
            std::fs::read_to_string(root.join(".nedfolders")).unwrap(),
            ".\nsrc\n"
        );
    }

    #[test]
    fn creates_ned_ls_toml_with_relative_entry() {
        let tmp = tempfile::tempdir().unwrap();
        let workspace = tmp.path().join("workspace");
        let other = tmp.path().join("inet");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::create_dir_all(&other).unwrap();

        // println!("workspace: {:?}", workspace);

        add_to_ned_ls_toml(&workspace, &other).unwrap();

        let parsed = utils::parse_config(workspace.join("Ned_ls.toml")).unwrap();
        assert_eq!(parsed.imported_projects, vec!["../inet".to_string()]);
    }

    #[test]
    fn ned_ls_toml_append_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let workspace = tmp.path().join("workspace");
        let other = tmp.path().join("inet");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::create_dir_all(&other).unwrap();

        // println!("workspace: {:?}", workspace);

        add_to_ned_ls_toml(&workspace, &other).unwrap();
        add_to_ned_ls_toml(&workspace, &other).unwrap();

        let parsed = utils::parse_config(workspace.join("Ned_ls.toml")).unwrap();
        assert_eq!(parsed.imported_projects, vec!["../inet".to_string()]);
    }

    #[test]
    fn ned_ls_toml_keeps_existing_entries() {
        let tmp = tempfile::tempdir().unwrap();
        let workspace = tmp.path().join("workspace");
        let other = tmp.path().join("inet");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(
            workspace.join("Ned_ls.toml"),
            "imported_projects = [\"../veins\"]\n",
        )
        .unwrap();

        add_to_ned_ls_toml(&workspace, &other).unwrap();

        let parsed = utils::parse_config(workspace.join("Ned_ls.toml")).unwrap();
        assert_eq!(
            parsed.imported_projects,
            vec!["../veins".to_string(), "../inet".to_string()]
        );
    }
}
