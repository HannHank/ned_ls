use crate::Backend;
use crate::handlers::commands;
use crate::workspace_membership::{self, Membership};

use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;

/// Offer the quickfixes from crate::diagnostics::membership_diagnostic
pub async fn code_action(
    backend: &Backend,
    params: CodeActionParams,
) -> Result<Option<CodeActionResponse>> {
    let uri = params.text_document.uri;

    if uri.scheme() != "file" {
        return Ok(None);
    }

    let Some(config) = backend.shared.config.lock().await.clone() else {
        return Ok(None);
    };
    let Ok(file_path) = uri.to_file_path() else {
        return Ok(None);
    };

    // If the file is not part of a workspace, no actions.
    let membership = workspace_membership::classify(&file_path, &config);
    let Some(code) = membership.diagnostic_code() else {
        return Ok(None);
    };

    // anchor the quickfix to the diagnostic that triggered it, if any.
    let diagnostic = params.context.diagnostics.iter().find(|d| {
        matches!(&d.code, Some(NumberOrString::String(c)) if c == code)
            && d.source.as_deref() == Some("ned_ls") // only offer quickfixes for diagnostics for
        // the ned_ls source
    });

    // Requests scoped to a code-action kind that excludes quickfixes get nothing.
    if let Some(only) = &params.context.only
        && !only
            .iter()
            .any(|k| CodeActionKind::QUICKFIX.as_str().starts_with(k.as_str()))
    {
        return Ok(None);
    }

    let actions = match &membership {
        Membership::Indexed => return Ok(None),

        // The folder is not listed in .nedfolders → common mistake
        Membership::FolderNotListed {
            project_root,
            folder,
            rel,
        } => vec![
            action(
                format!("Add '{rel}' to .nedfolders and index it"),
                commands::ADD_NED_FOLDER,
                vec![path_arg(project_root), path_arg(folder)],
                diagnostic,
            ),
            action(
                format!("Index '{rel}' for this session only"),
                commands::INDEX_FOLDER,
                vec![path_arg(folder)],
                diagnostic,
            ),
        ],

        // The project is not listed in Ned_ls.toml → common mistake
        Membership::ProjectNotImported { project_root, name } => {
            let mut actions = vec![];

            // Writing Ned_ls.toml only makes sense when we know where the workspace root is.
            if let Ok(workspace_root) = config.project_root.to_file_path() {
                actions.push(action(
                    format!("Add project '{name}' to Ned_ls.toml and index it"),
                    commands::ADD_IMPORTED_PROJECT,
                    vec![path_arg(&workspace_root), path_arg(project_root)],
                    diagnostic,
                ));
            }

            // I think it makes sense to offer this action for testing purposes
            actions.push(action(
                format!("Index project '{name}' for this session only"),
                commands::INDEX_PROJECT,
                vec![path_arg(project_root)],
                diagnostic,
            ));
            actions
        }

        // Without .project file, we do not know the project name, so we only offer the session-only indexing action.
        Membership::NoProject { folder } => vec![action(
            format!("Index '{}' for this session only", folder.display()),
            commands::INDEX_FOLDER,
            vec![path_arg(folder)],
            diagnostic,
        )],
    };

    Ok(Some(actions))
}

// return a serde_json::Value representing the path → command arguments
fn path_arg(path: &std::path::Path) -> serde_json::Value {
    serde_json::Value::String(path.to_string_lossy().into_owned())
}

/// Return a CodeActionOrCommand for a quickfix with the given title, command, arguments, and optional diagnostic.
fn action(
    title: String,
    command: &str,
    arguments: Vec<serde_json::Value>,
    diagnostic: Option<&Diagnostic>,
) -> CodeActionOrCommand {
    CodeActionOrCommand::CodeAction(CodeAction {
        title: title.clone(),
        kind: Some(CodeActionKind::QUICKFIX),
        diagnostics: diagnostic.map(|d| vec![d.clone()]),
        command: Some(Command {
            title,
            command: command.to_string(),
            arguments: Some(arguments),
        }),
        edit: None,
        is_preferred: None,
        disabled: None,
        data: None,
    })
}
