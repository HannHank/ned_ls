use ropey::Rope;
use tower_lsp::lsp_types::{Position, Range};
use tree_sitter::Tree;

use crate::db::Db;

use super::*;

static FALLBACK_HOOKS: &[ContextType] = &[
    ContextType::Parameters,
    ContextType::Gates,
    ContextType::LocalTypes,
    ContextType::Submodules,
    ContextType::Submodule,
    ContextType::Connections,
    ContextType::SimpleModule,
    ContextType::CompoundModule,
];

pub struct CompletionEngine<'a, 'db> {
    pub db: &'db dyn Db,
    pub rope: &'a Rope,
    pub ast: &'a Tree,
    pub uri: &'a tower_lsp::lsp_types::Url,
    pub position: Position,
    pub byte_offset: usize,
    pub char_offset: usize,
}

impl<'a, 'db> CompletionEngine<'a, 'db> {
    // keywords for a simple module or module
    pub fn get_current_context(&self) -> ContextPath {
        let root = self.ast.root_node();

        let mut current = root
            .descendant_for_byte_range(self.byte_offset, self.byte_offset)
            .unwrap_or(root);

        let mut _error = false;
        let mut contexts: ContextPath = Vec::new();
        // Ascent Logic
        // returns the first valid parent context if current node is error
        loop {
            let kind = current.kind();
            log::debug!("Node kind: {}", kind);
            if kind == "import" {
                contexts.push(ContextType::ImportLine);
            }
            if kind == "package" {
                contexts.push(ContextType::PackageLine);
            }
            if kind == "extends" {
                contexts.push(ContextType::ExtendsLine);
            }
            if kind == "implements" {
                contexts.push(ContextType::LikeLine);
            }
            if kind == "gates" {
                contexts.push(ContextType::Gates);
            }
            if kind == "submodule_gates" {
                contexts.push(ContextType::SubmoduleGates);
            }
            if kind == "gate" {
                contexts.push(ContextType::Gate);
            }
            if kind == "parameters" {
                contexts.push(ContextType::Parameters);
            }

            if kind == "submodule_parameters" {
                contexts.push(ContextType::SubmoduleParameters);
            }
            if kind == "connections" {
                contexts.push(ContextType::Connections);
            }
            if kind == "connection" {
                contexts.push(ContextType::Connection);
            }
            if kind == "src" {
                contexts.push(ContextType::ConnectionSrc);
            }
            if kind == "dest" {
                contexts.push(ContextType::ConnectionDst);
            }
            if kind == "property" {
                contexts.push(ContextType::Parameters);
            }
            if kind == "string_literal" {
                contexts.push(ContextType::StringLiteral);
            }
            if kind == "submodules" {
                contexts.push(ContextType::Submodules);
            }
            if kind == "submodule" {
                contexts.push(ContextType::Submodule);
            }
            if kind == "type" {
                contexts.push(ContextType::Type);
            }
            if kind == "types" {
                contexts.push(ContextType::LocalTypes);
            }
            if kind == "like_type" {
                contexts.push(ContextType::LikeType);
            }
            if kind == "like_expr" {
                contexts.push(ContextType::LikeExpr);
            }
            if kind == "simple" {
                contexts.push(ContextType::SimpleModule);
            }
            if kind == "NedFile" {
                contexts.push(ContextType::NedFile);
            }
            if kind == "network" {
                contexts.push(ContextType::Network);
            }
            if kind == "module" {
                contexts.push(ContextType::CompoundModule);
            }
            if current.is_error() || current.is_missing() && contexts.is_empty() {
                _error = true;
            }
            if let Some(parent) = current.parent() {
                current = parent;
            } else {
                // Reached the root without finding relevant context
                contexts.push(ContextType::None);
                break;
            }
        }

        // The FALLBACK_HOOKS allow to only trigger the fallback if we really fall through the
        // section and want to make a quick check if are are not just at the end of the section
        // or really at the outer level
        if contexts.is_empty()
            || FALLBACK_HOOKS.contains(contexts.first().unwrap_or(&ContextType::None))
        {
            let char_offset = match self.rope.try_byte_to_char(self.byte_offset) {
                Ok(c) => c,
                Err(_) => return vec![ContextType::None],
            };

            // Grab the raw text of the current line up to the cursor
            let line_idx = self.rope.char_to_line(char_offset);
            let line_start = self.rope.line_to_char(line_idx);
            let text_before_cursor = self.rope.slice(line_start..char_offset).to_string();
            let trimmed_line = text_before_cursor.trim();

            // If the line starts with import or package, we know exactly where we are
            if trimmed_line.starts_with("import ") {
                return vec![ContextType::ImportLine];
            }
            if trimmed_line.starts_with("package ") {
                return vec![ContextType::PackageLine];
            }

            let prev_token = self.get_previous_token();
            if prev_token == "like"
                || (trimmed_line.contains(" like ") && !trimmed_line.contains('{'))
            {
                return vec![ContextType::LikeLine];
            }

            let prev_token = self.get_previous_token();
            if prev_token == "extends"
                || (trimmed_line.contains(" extends ") && !trimmed_line.contains('{'))
            {
                return vec![ContextType::ExtendsLine];
            }

            let fallback = self.get_fallback_section();
            if fallback != ContextType::None {
                // If the fallback is different from the AST node we insert it as the first context
                if contexts.first() != Some(&fallback) {
                    contexts.insert(0, fallback);
                }
            } else if contexts.is_empty() {
                contexts.push(ContextType::None);
            }
        }
        contexts
    }

    // INFO: a real tokenizer would be more robust
    // Maybe if I find some time I can replace this
    pub fn get_last_token(&self) -> String {
        let mut char_offset = self.char_offset;
        let mut skipped_space = false;
        let mut hit_newline = false;

        // check what is before the cursor
        while char_offset > 0 {
            let ch = self.rope.char(char_offset - 1);
            if ch == '\n' || ch == '\r' {
                hit_newline = true;
                // Reset skipped space because spaces before a newline are just trailing,
                // and spaces after are just indentation.
                skipped_space = false;
                char_offset -= 1;
            } else if ch.is_whitespace() {
                skipped_space = true;
                char_offset -= 1;
            } else {
                break;
            }
        }

        // `wlan.   |`
        // Starting a new token → return empty
        if !hit_newline && skipped_space {
            return "".to_string();
        }

        let end = char_offset;

        // extract the actual word/token
        // Simple scanning → do not really like that approach
        while char_offset > 0 {
            let prev_char = self.rope.char(char_offset - 1);

            if prev_char.is_whitespace()
                || prev_char == '<'
                || prev_char == '>'
                || prev_char == '('
                || prev_char == ')'
                || prev_char == '"'
                || prev_char == ':'
                || prev_char == '='
                || prev_char == ','
            {
                break;
            }
            char_offset -= 1;
        }

        let start = char_offset;
        let token = self.rope.slice(start..end).to_string();

        // `wlan \n |`
        // We only want to keep the token if user hit enter after a dot
        if hit_newline && !token.ends_with('.') {
            return "".to_string();
        }

        token
    }

    // I use this to provide better context sensitiv suggestions
    // INFO: it feels like to much manual scanning → maybe I can use TS but with a broken ast this
    // has a limit
    /// Finds the completed word before the current token/space.
    pub fn get_previous_token(&self) -> String {
        let mut char_offset = self.char_offset;

        // Skip the current token
        while char_offset > 0
            && !self
                .rope
                .char(char_offset.saturating_sub(1))
                .is_whitespace()
        {
            char_offset -= 1;
        }

        // Skip the whitespace behind the current token
        while char_offset > 0
            && self
                .rope
                .char(char_offset.saturating_sub(1))
                .is_whitespace()
        {
            char_offset -= 1;
        }

        // If we hit the beginning of the file and found no previous token, return empty string.
        if char_offset == 0 {
            return "".to_string();
        }

        let end = char_offset;

        // Extract the previous token safely
        while char_offset > 0
            && !self
                .rope
                .char(char_offset.saturating_sub(1))
                .is_whitespace()
        {
            char_offset -= 1;
        }

        self.rope.slice(char_offset..end).to_string()
    }

    // I needed to add this because the speculative parsing approach is not working for the connections
    // section. One way would be to identify the outer context and adapt the token. For now I am going
    // with this fallback.
    /// Fallback context resolution by scanning backwards for section headers.
    fn get_fallback_section(&self) -> ContextType {
        let char_offset = match self.rope.try_byte_to_char(self.byte_offset) {
            Ok(c) => c,
            Err(_) => return ContextType::None,
        };
        let mut line_idx = self.rope.char_to_line(char_offset);

        loop {
            let line = self.rope.line(line_idx).to_string();
            let trimmed = line.trim();

            if trimmed.starts_with("parameters:") {
                return ContextType::Parameters;
            }
            if trimmed.starts_with("gates:") {
                return ContextType::Gates;
            }
            if trimmed.starts_with("submodules:") {
                return ContextType::Submodules;
            }
            if trimmed.starts_with("connections:")
                || trimmed.starts_with("connections allowunconnected:")
            {
                return ContextType::Connections;
            }
            if trimmed.starts_with("types:") {
                return ContextType::LocalTypes;
            }

            // Stop if we hit the top of the module/network
            if trimmed.starts_with("module ")
                || trimmed.starts_with("simple ")
                || trimmed.starts_with("network ")
                || trimmed.starts_with("channel ")
                || trimmed.starts_with("moduleinterface ")
            {
                break;
            }

            if line_idx == 0 {
                break;
            }
            line_idx -= 1;
        }
        ContextType::None
    }

    /// calculates the insertion range by simply subtracting the length of the currently typed word
    /// from the cursor position.
    pub fn get_replacement_range(&self) -> (String, Range) {
        let partial_word = self.get_last_token();
        (
            partial_word.clone(),
            self.range_ending_at_cursor(&partial_word),
        )
    }

    /// Like get_replacement_range but only covers the segment after the last dot → important for
    /// gate names and so on
    pub fn get_segment_replacement_range(&self) -> Range {
        let token = self.get_last_token();
        let segment = token.rsplit('.').next().unwrap_or(&token);
        self.range_ending_at_cursor(segment)
    }

    fn range_ending_at_cursor(&self, typed: &str) -> Range {
        // subs in 1D space → helps with newlines
        // Otherwise I had some weird output where the range was not correct
        let start_char_offset = self.char_offset.saturating_sub(typed.chars().count());

        // convert back to 2D position
        let start = crate::utils::char_to_lsp_position(self.rope, start_char_offset);

        Range {
            start,
            end: self.position,
        }
    }
}
