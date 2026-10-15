use zed::settings::LspSettings;
use zed_extension_api::{self as zed, Result};

// name of the binary that is installed via cargo install
const SERVER_BINARY: &str = "ned_ls";

// key in zed settings.json that points to the binary and its arguments
const SERVER_ID: &str = "ned_ls";

struct NedLsExtension;

impl NedLsExtension {
    fn server_binary(&self, worktree: &zed::Worktree) -> Result<(String, Vec<String>)> {
        let settings = LspSettings::for_worktree(SERVER_ID, worktree)
            .ok()
            .and_then(|settings| settings.binary);

        let args = settings
            .as_ref()
            .and_then(|binary| binary.arguments.clone())
            .unwrap_or_default();

        if let Some(path) = settings.as_ref().and_then(|binary| binary.path.clone()) {
            return Ok((path, args));
        }

        if let Some(path) = worktree.which(SERVER_BINARY) {
            return Ok((path, args));
        }

        Err(format!(
            "`{SERVER_BINARY}` was not found on your PATH.\n\n\
             Install it with `cargo install --git https://github.com/HannHank/ned_ls`, or point Zed at the binary by adding to your settings.json:\n\n\
             {{\n  \"lsp\": {{\n    \"{SERVER_ID}\": {{\n      \"binary\": {{\n        \
             \"path\": \"/absolute/path/to/{SERVER_BINARY}\"\n      }}\n    }}\n  }}\n}}"
        ))
    }
}

impl zed::Extension for NedLsExtension {
    fn new() -> Self {
        Self
    }

    fn language_server_command(
        &mut self,
        _language_server_id: &zed::LanguageServerId,
        worktree: &zed::Worktree,
    ) -> Result<zed::Command> {
        let (command, args) = self.server_binary(worktree)?;

        Ok(zed::Command {
            command,
            args,
            env: worktree.shell_env(),
        })
    }

    fn language_server_workspace_configuration(
        &mut self,
        _language_server_id: &zed::LanguageServerId,
        worktree: &zed::Worktree,
    ) -> Result<Option<zed::serde_json::Value>> {
        Ok(LspSettings::for_worktree(SERVER_ID, worktree)
            .ok()
            .and_then(|settings| settings.settings))
    }

    fn language_server_initialization_options(
        &mut self,
        _language_server_id: &zed::LanguageServerId,
        worktree: &zed::Worktree,
    ) -> Result<Option<zed::serde_json::Value>> {
        Ok(LspSettings::for_worktree(SERVER_ID, worktree)
            .ok()
            .and_then(|settings| settings.initialization_options))
    }
}

zed::register_extension!(NedLsExtension);
