use crate::hir::Span;
use home::home_dir;
use regex::Regex;
use ropey::Rope;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use tower_lsp::lsp_types::{Position, Range};

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use crate::hir;

#[allow(dead_code)]
pub fn byte_to_point(rope: &Rope, target_byte_offset: usize) -> tree_sitter::Point {
    if target_byte_offset > rope.len_bytes() {
        // Handle out-of-bounds, maybe return Point at end of doc?
        let last_char = rope.len_chars();
        let row = rope.char_to_line(last_char);
        let line_start_byte = rope.line_to_byte(row);
        return tree_sitter::Point {
            row,
            column: rope.len_bytes() - line_start_byte,
        };
    }

    let row = rope.byte_to_line(target_byte_offset);
    let line_start_byte = rope.line_to_byte(row);
    let column = target_byte_offset - line_start_byte; // byte offset within the line
    tree_sitter::Point { row, column }
}

pub fn byte_to_lsp_position(rope: &Rope, byte_offset: usize) -> Option<Position> {
    if byte_offset > rope.len_bytes() {
        log::error!(
            "byte_to_lsp_position out of bounds: offset {} > rope length {}",
            byte_offset,
            rope.len_bytes()
        );
        return None;
    }

    let line_idx = rope.byte_to_line(byte_offset);
    let line_start = rope.line_to_byte(line_idx);

    let line_slice = rope.line(line_idx);

    // byte index within line
    let byte_in_line = byte_offset - line_start;
    let char_in_line = line_slice.byte_to_char(byte_in_line);

    let mut utf16_col = 0;
    let chars = line_slice.chars().take(char_in_line);

    for c in chars {
        // len_utf16() is extremely cheap (just checks for >0xFFFF)
        utf16_col += c.len_utf16();
    }

    Some(Position {
        line: line_idx as u32,
        character: utf16_col as u32,
    })
}

/// Converts a char offset into an LSP position → line + UTF-16 column.
pub fn char_to_lsp_position(rope: &Rope, char_offset: usize) -> Position {
    let char_offset = char_offset.min(rope.len_chars());
    let line_idx = rope.char_to_line(char_offset);
    let line_start_char = rope.line_to_char(line_idx);

    let utf16_col: usize = rope
        .slice(line_start_char..char_offset)
        .chars()
        .map(|c| c.len_utf16())
        .sum();

    Position {
        line: line_idx as u32,
        character: utf16_col as u32,
    }
}

#[allow(dead_code)]
pub fn position_to_char(rope: &Rope, pos: Position) -> usize {
    let line_idx = pos.line as usize;
    let target_utf16_col = pos.character as usize;

    if line_idx >= rope.len_lines() {
        // Position is out of bounds (e.g., file is empty or pos is too large)
        // Return the end of the document.
        return rope.len_chars();
    }

    let line_start_char_idx = rope.line_to_char(line_idx);
    let line_slice = rope.line(line_idx);

    let mut current_utf16_col = 0;
    let mut char_col = 0; // This is the offset in Rust `char`s

    // Iterate over the `char`s in the line
    for c in line_slice.chars() {
        if current_utf16_col == target_utf16_col {
            // We found the exact char boundary
            break;
        }

        // Check if adding this char's UTF-16 length would go *over* the target
        let char_utf16_len = c.len_utf16();
        if current_utf16_col + char_utf16_len > target_utf16_col {
            break;
        }

        current_utf16_col += char_utf16_len;
        char_col += 1;
    }

    // The loop will stop at the correct `char_col` or at the end of the line.
    line_start_char_idx + char_col
}

// Was doing this multiple times in different functions, so I factored it out
pub fn span_to_range(rope: &ropey::Rope, span: hir::Span) -> Option<Range> {
    Some(Range {
        start: byte_to_lsp_position(rope, span.start)?,
        end: byte_to_lsp_position(rope, span.end)?,
    })
}

/// Resolve path relative to workspace root
/// TODO: write tests
pub fn resolve_path(input: &Path, workspace: &Path) -> Option<PathBuf> {
    let resolved_path = if input.is_absolute() {
        input.to_path_buf()
    } else {
        let s = input.to_str()?;
        if s.starts_with("~/") {
            let home = home_dir()?;
            home.join(s.trim_start_matches("~/"))
        } else {
            workspace.join(input)
        }
    };

    match std::fs::canonicalize(&resolved_path) {
        Ok(normalized_path) => Some(normalized_path),
        Err(e) => {
            log::warn!("Error canonicalizing path {:?}: {}", resolved_path, e);
            // Return the uncanonicalized path as a fallback, or None to signal failure
            Some(resolved_path) // TODO: this means all look ups might fail because of
            // non-normalized paths (currently only completion for package names is affected)
        }
    }
}

/// Extracts the current word under the cursor from Rope.
pub fn get_word_under_cursor(rope: &Rope, pos: Position) -> Option<String> {
    let line_idx = pos.line as usize;
    if line_idx >= rope.len_lines() {
        return None;
    }

    let line_slice = rope.line(line_idx);
    let line_text = line_slice.to_string();

    let target_utf16_col = pos.character as usize;
    let mut current_utf16_col = 0;
    let mut byte_idx_in_line = 0;
    for c in line_slice.chars() {
        if current_utf16_col >= target_utf16_col {
            break;
        }
        let char_utf16_len = c.len_utf16();
        if current_utf16_col + char_utf16_len > target_utf16_col {
            break;
        }
        current_utf16_col += char_utf16_len;
        byte_idx_in_line += c.len_utf8();
    }
    let start = line_text[..byte_idx_in_line]
        .rfind(|c: char| !c.is_alphanumeric() && c != '_')
        .map_or(0, |i| i + 1); // This gives byte index

    let end_offset_from_idx = line_text[byte_idx_in_line..]
        .find(|c: char| !c.is_alphanumeric() && c != '_')
        .unwrap_or(line_text.len() - byte_idx_in_line);
    let end = byte_idx_in_line + end_offset_from_idx;

    if start < end && end <= line_text.len() {
        Some(line_text[start..end].to_string())
    } else {
        None
    }
}

/// Extracts the fully qualified name
#[allow(dead_code)]
pub fn get_fqn_under_cursor(rope: &Rope, pos: Position) -> Option<(String, hir::Span)> {
    let line_idx = pos.line as usize;
    if line_idx >= rope.len_lines() {
        return None;
    }

    let line_slice = rope.line(line_idx);
    let line_text = line_slice.to_string();

    let target_utf16_col = pos.character as usize;
    let mut current_utf16_col = 0;
    let mut byte_idx_in_line = 0;
    // INFO: refactor with the get_word_under_cursor
    for c in line_slice.chars() {
        if current_utf16_col >= target_utf16_col {
            break;
        }
        let char_utf16_len = c.len_utf16();
        if current_utf16_col + char_utf16_len > target_utf16_col {
            break;
        }
        current_utf16_col += char_utf16_len;
        byte_idx_in_line += c.len_utf8();
    }

    // walk back
    let start_offset_in_line = line_text[..byte_idx_in_line]
        .rfind(|c: char| !c.is_alphanumeric() && c != '_' && c != '.')
        .map_or(0, |i| i + 1); // byte index 

    let end_offset_from_idx = line_text[byte_idx_in_line..]
        .find(|c: char| !c.is_alphanumeric() && c != '_' && c != '.')
        .unwrap_or(line_text.len() - byte_idx_in_line);
    let end_offset_in_line = byte_idx_in_line + end_offset_from_idx;

    if start_offset_in_line < end_offset_in_line && end_offset_in_line <= line_text.len() {
        let word = line_text[start_offset_in_line..end_offset_in_line].to_string();

        let line_start_byte = rope.line_to_byte(line_idx);
        let span = crate::hir::Span {
            start: line_start_byte + start_offset_in_line,
            end: line_start_byte + end_offset_in_line,
        };

        Some((word, span))
    } else {
        None
    }
}

#[allow(dead_code)]
pub fn span_to_lsp_range(rope: &Rope, span: &Span) -> Option<tower_lsp::lsp_types::Range> {
    Some(tower_lsp::lsp_types::Range {
        start: byte_to_lsp_position(rope, span.start)?,
        end: byte_to_lsp_position(rope, span.end)?,
    })
}

/// Returns the word at the given byte offset in the key text, considering '.' and '[' as delimiters
pub fn word_at_byte_offset(key_text: &str, key_start: usize, cursor_offset: usize) -> String {
    let bytes = key_text.as_bytes();
    if bytes.is_empty() {
        return String::new();
    }

    let offset = cursor_offset.saturating_sub(key_start).min(bytes.len() - 1);

    let mut start = offset;
    while start > 0 && bytes[start - 1] != b'.' {
        start -= 1;
    }

    let mut end = offset;
    while end < bytes.len() && bytes[end] != b'.' && bytes[end] != b'[' {
        end += 1;
    }

    key_text[start..end].to_string()
}

/// Fast, zero-allocation glob matcher for `*` and `?`.
pub fn match_pattern(pattern: &str, target: &str) -> bool {
    let (mut p_idx, mut t_idx) = (0, 0);
    let (mut star_idx, mut match_idx) = (None, 0);
    let p_bytes = pattern.as_bytes();
    let t_bytes = target.as_bytes();

    while t_idx < t_bytes.len() {
        if p_idx < p_bytes.len() && (p_bytes[p_idx] == b'?' || p_bytes[p_idx] == t_bytes[t_idx]) {
            p_idx += 1;
            t_idx += 1;
        } else if p_idx < p_bytes.len() && p_bytes[p_idx] == b'*' {
            star_idx = Some(p_idx);
            match_idx = t_idx;
            p_idx += 1;
        } else if let Some(star) = star_idx {
            p_idx = star + 1;
            match_idx += 1;
            t_idx = match_idx;
        } else {
            return false;
        }
    }
    while p_idx < p_bytes.len() && p_bytes[p_idx] == b'*' {
        p_idx += 1;
    }
    p_idx == p_bytes.len()
}

#[derive(Debug, Deserialize, Serialize, Clone, Default)]
pub struct NedConfig {
    pub imported_projects: Vec<String>,
}

#[derive(Debug)]
pub enum ConfigError {
    FileNotFound,
    #[allow(dead_code)]
    ParseError(String),
}

pub fn parse_config<P: AsRef<Path>>(path: P) -> Result<NedConfig, ConfigError> {
    let data = fs::read_to_string(path).map_err(|_| ConfigError::FileNotFound)?;

    toml::from_str::<NedConfig>(&data).map_err(|e| ConfigError::ParseError(e.to_string()))
}

// TODO: not sure if this is the correct way of doing it
pub fn find_file_upwards(current_path: &Path, file_name: &str) -> Option<PathBuf> {
    // Ensure we start with a directory
    let mut current = if current_path.is_dir() {
        Some(current_path)
    } else {
        current_path.parent()
    };

    // Search upwards
    let home = home_dir()?;
    while let Some(dir) = current {
        let candidate = dir.join(file_name);
        if candidate.exists() {
            return Some(dir.to_path_buf());
        }

        // Stop if we reached the home dir
        if dir == home {
            break;
        }
        current = dir.parent();
    }
    None
}

/// Hash diagnostics for change detection
pub fn hash_hir_diagnostics(diagnostics: &[hir::Diagnostic]) -> u64 {
    let mut hasher = DefaultHasher::new();
    for diag in diagnostics {
        diag.start.hash(&mut hasher);
        diag.end.hash(&mut hasher);
        diag.message.hash(&mut hasher);
    }
    hasher.finish()
}

// TODO: move this to a seperate file
#[derive(Debug, Clone)]
pub struct ProjectDependencies {
    pub projects: HashMap<String, PathBuf>,
    pub paths_to_index: Vec<PathBuf>,
}

impl ProjectDependencies {
    pub fn new() -> Self {
        ProjectDependencies {
            projects: HashMap::new(),
            paths_to_index: Vec::new(),
        }
    }
}

/// Result of scanning a workspace for its indexing configuration and the messages the caller
/// should send to the client.
pub struct ConfigDiscovery {
    pub config: crate::Config,
    /// (severity, text) pairs
    pub messages: Vec<(tower_lsp::lsp_types::MessageType, String)>,
}

/// Work out which folders make up the indexed scope for a workspace.
///
/// Sources, in order: Ned_ls.toml imported projects, an OMNeT++ Makefiles *_PROJ variables,
/// each projects .nedfolders, and $NEDPATH.
pub fn discover_config(workspace_url: &tower_lsp::lsp_types::Url) -> Option<ConfigDiscovery> {
    use tower_lsp::lsp_types::MessageType;

    let workspace_path = workspace_url.to_file_path().ok()?;
    let mut messages = Vec::new();
    let mut imported_projects = std::collections::HashSet::new();

    let config_result = parse_config(workspace_path.join("Ned_ls.toml"));
    // A broken config still shows the intent to use ned_ls in this workspace
    let has_config_file = !matches!(config_result, Err(ConfigError::FileNotFound));
    let has_nedfolders = workspace_path.join(".nedfolders").exists();

    match config_result {
        Ok(parsed) => {
            for project in &parsed.imported_projects {
                let Some(resolved) = resolve_path(Path::new(project), &workspace_path) else {
                    continue;
                };

                messages.push((
                    MessageType::INFO,
                    format!("Resolved imported project path: {resolved:?}"),
                ));

                if !resolved.exists() {
                    messages.push((
                        MessageType::WARNING,
                        format!("Imported project path does not exist: {resolved:?}"),
                    ));
                    continue;
                }

                imported_projects.insert(crate::workspace_membership::normalize(&resolved));
            }
        }

        // If the config file is not found, we dont treat it as an error, just log an info message →
        // I think this is often the default case.
        Err(ConfigError::FileNotFound) => messages.push((
            MessageType::INFO,
            "No Ned_ls.toml found in the workspace root.".to_string(),
        )),
        Err(e) => messages.push((
            MessageType::WARNING,
            format!("Failed to parse config: {e:?}"),
        )),
    }

    // Makefiles can name further projects to index
    if let Some(imports) = find_and_parse_makefile(&workspace_path) {
        imported_projects.extend(
            imports
                .paths_to_index
                .iter()
                .map(|p| crate::workspace_membership::normalize(p)),
        );
    }

    let mut nedfolders = find_and_parse_nedfolders(&workspace_path);
    for project in imported_projects.iter() {
        nedfolders.extend(find_and_parse_nedfolders(project));
    }

    if let Ok(nedpath_var) = std::env::var("NEDPATH") {
        let paths: Vec<PathBuf> = std::env::split_paths(&nedpath_var).collect();
        for path in &paths {
            messages.push((
                MessageType::INFO,
                format!("NEDPATH entry: {}", path.display()),
            ));
        }
        nedfolders.extend(paths);
    }

    // Normalize before deduplicating
    let nedfolders: Vec<PathBuf> = nedfolders
        .iter()
        .map(|p| crate::workspace_membership::normalize(p))
        .collect();

    Some(ConfigDiscovery {
        config: crate::Config {
            imported_projects,
            project_root: workspace_url.clone(),
            nedfolders: deduplicate_nested_roots(nedfolders),
            has_config_file,
            has_nedfolders,
        },
        messages,
    })
}

/// Parses .nedfolders or defaults to "." .
pub fn find_and_parse_nedfolders(project_root: &Path) -> Vec<PathBuf> {
    let nedfolders_path = project_root.join(".nedfolders");

    if nedfolders_path.exists()
        && let Ok(content) = fs::read_to_string(&nedfolders_path)
    {
        let paths: Vec<PathBuf> = content
            .lines()
            .map(|line| line.trim())
            .filter(|line| !line.is_empty()) // Skip empty lines
            .map(|folder| project_root.join(folder))
            .collect();

        // If the file was not empty, return the paths found
        if !paths.is_empty() {
            return paths;
        }
    }

    vec![project_root.join(".")]
}

/// Search for an OMNeT++ Makefile in common locations and parse it
pub fn find_and_parse_makefile(project_root: &Path) -> Option<ProjectDependencies> {
    // I think this are all the possible locations for an OMNeT++ Makefile
    // I could also implement a search for all Makefiles but since the additional config file
    // exists I think this is sufficient for now
    let candidates = vec![
        project_root.join("src/Makefile"),
        project_root.join("simulations/Makefile"),
        project_root.join("Makefile"),
        project_root.join("src/makefile"),
        project_root.join("makefile"),
    ];

    for makefile_path in candidates {
        // eprintln!(
        //     "Searching for OMNeT++ Makefile at: {}",
        //     makefile_path.display()
        // );
        if makefile_path.exists() {
            // get parent folder of makefile_path
            let makefile_dir = makefile_path.parent().unwrap_or(project_root);
            if let Ok(deps) = parse_omnetpp_makefile(&makefile_path, makefile_dir) {
                // Only return if we found actual dependencies
                if !deps.projects.is_empty() {
                    return Some(deps);
                }
                // Otherwise, keep searching
            }
        }
    }

    None
}

/// Parse an OMNeT++ Makefile and extract project dependencies
pub fn parse_omnetpp_makefile(
    makefile_path: &Path,
    project_root: &Path,
) -> Result<ProjectDependencies, std::io::Error> {
    let content = std::fs::read_to_string(makefile_path)?;
    let mut deps = ProjectDependencies::new();

    let var_pattern = Regex::new(r"^([A-Z_]+_PROJ)\s*=\s*(.+)$").unwrap();

    for line in content.lines() {
        // Skip if not interesting for us
        if !line.contains("_PROJ") {
            continue;
        }

        // ignore the opp_makemake comments
        if !line.trim().starts_with('#')
            && let Some(cap) = var_pattern.captures(line.trim())
            && let (Some(var_name), Some(path_str)) = (cap.get(1), cap.get(2))
        {
            let var = var_name.as_str();
            let path = Path::new(path_str.as_str().trim());
            let resolved_path = resolve_path(path, project_root);
            if let Some(resolved) = resolved_path {
                // eprintln!(
                //     "Found dependency from variable: {} -> {}",
                //     var,
                //     resolved.display()
                // );
                deps.projects.insert(var.to_string(), resolved);
            }
        }
    }

    // Resolve all project paths
    for path in deps.projects.values() {
        if path.exists() {
            deps.paths_to_index.push(path.clone());
        } else {
            log::warn!("Project path does not exist: {}", path.display());
        }
    }

    Ok(deps)
}

/// Deduplicate nested root paths
pub fn deduplicate_nested_roots(roots: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut sorted = roots;
    sorted.sort();

    let mut unique_roots: Vec<PathBuf> = Vec::new();

    for path in sorted {
        if !unique_roots.iter().any(|root| path.starts_with(root)) {
            unique_roots.push(path);
        }
    }
    unique_roots
}

/// Find all ned files inside a folder
pub fn find_all_ned_files(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                files.extend(find_all_ned_files(&path));
            } else if let Some(ext) = path.extension()
                && ext == "ned"
            {
                files.push(path);
            }
        }
    }
    files
}

/// Documentation comment above the node at span in file →  as Markdown
pub fn documentation_at_span(
    db: &dyn crate::db::Db,
    file: hir::SourceFile,
    span: Span,
) -> Option<String> {
    let tree = db.get_ast_for_file(&file)?;
    let node = tree
        .root_node()
        .descendant_for_byte_range(span.start, span.end)?;
    extract_documentation(node, file.rope(db))
}

/// Comment after a declaration on the same line
/// Semicolon and whitespace are allowed before the comment
fn trailing_comment(rope: &Rope, end_byte: usize) -> Option<String> {
    if end_byte > rope.len_bytes() {
        return None;
    }
    let line = rope.byte_to_line(end_byte);
    let line_end = rope.line_to_byte(line + 1);
    let rest = rope.byte_slice(end_byte..line_end).to_string();
    let rest = rest.trim_start_matches(|c: char| c == ';' || c.is_whitespace());
    let comment = rest.strip_prefix("//")?.trim();
    (!comment.is_empty()).then(|| format_ned_doc(comment))
}

/// documentation of a member
pub fn member_documentation_at_span(
    db: &dyn crate::db::Db,
    file: hir::SourceFile,
    span: Span,
) -> Option<String> {
    // doc from above
    let above = documentation_at_span(db, file, span);
    // doc at current line
    let trailing = trailing_comment(file.rope(db), span.end);
    // returning the above doc if it exists, otherwise the trailing doc
    match (above, trailing) {
        (Some(above), Some(trailing)) => Some(format!("{above}\n\n{trailing}")),
        (above, trailing) => above.or(trailing),
    }
}

/// Documentation of the parameter param declared in the NED type type_name in the file uri
pub fn documentation_for_parameter(
    db: &dyn crate::db::Db,
    uri: &tower_lsp::lsp_types::Url,
    type_name: &str,
    param: &str,
) -> Option<String> {
    let file = db.get_source_file_input(uri)?;
    let hir = hir::ned::lower_to_hir(db, file);
    let item = hir
        .items(db)
        .iter()
        .find(|item| item.name(db).text(db) == type_name)?;

    // get the parameter declaration with the given name
    let declaration = item.parameters(db).iter().find_map(|p| match p {
        hir::ned::ParamBlockItem::Param(p)
            if p.ned_type(db).is_some() && p.name(db).text(db) == param =>
        {
            Some(*p)
        }
        _ => None,
    })?;
    // resolve the doc for that parameter declaration
    member_documentation_at_span(db, file, *declaration.span(db))
}

/// Documentation comment above the NED type name declared in the file uri → as Markdown
pub fn documentation_for_type(
    db: &dyn crate::db::Db,
    uri: &tower_lsp::lsp_types::Url,
    name: &str,
) -> Option<String> {
    let file = db.get_source_file_input(uri)?;
    let hir = hir::ned::lower_to_hir(db, file);
    let item = hir
        .items(db)
        .iter()
        .find(|item| item.name(db).text(db) == name)?;
    documentation_at_span(db, file, item.span(db))
}

// TODO: move into seperate file
pub fn extract_documentation(node: tree_sitter::Node, rope: &Rope) -> Option<String> {
    // Some nodes () start with the line break that ends the comment above → the
    // first character of the declaration decides the row
    let mut start_byte = node.start_byte();
    while start_byte < node.end_byte() && rope.byte(start_byte).is_ascii_whitespace() {
        start_byte += 1;
    }
    let start_row = rope.byte_to_line(start_byte);
    let mut doc_lines = Vec::new();

    let mut current_row = start_row;

    while current_row > 0 {
        current_row -= 1;

        let line_slice = rope.line(current_row);
        let line_str = line_slice.to_string();
        let trimmed = line_str.trim();

        if trimmed.starts_with("//") {
            let content = trimmed.strip_prefix("//").unwrap_or("").trim();
            doc_lines.push(content.to_string());
        } else if trimmed.is_empty() {
            if !doc_lines.is_empty() {
                break;
            }
        } else {
            // Code found
            break;
        }
    }

    if doc_lines.is_empty() {
        return None;
    }

    // reverse the lines to maintain original order
    doc_lines.reverse();

    // join and format the markdown
    let raw_text = doc_lines.join("\n");
    Some(format_ned_doc(&raw_text))
}

// TODO:Change this to regex replacements to avoid allocating intermediate strings for every
// replace
/// Simple formatter for NED documentation comments to Markdown
fn format_ned_doc(text: &str) -> String {
    text.replace("*", "\\*")
        .replace("_", "\\_")
        .replace("<pre>", "\n```ned\n")
        .replace("</pre>", "\n```\n")
        .replace("<src>", "\n```ned\n")
        .replace("</src>", "\n```\n")
        .replace("<tt>", "`")
        .replace("</tt>", "`")
        .replace("<code>", "`")
        .replace("</code>", "`")
        .replace("<br>", "\n\n")
        .replace("<b>", "**")
        .replace("</b>", "**")
        .replace("<i>", "*")
        .replace("</i>", "*")
        .replace("<li>", "\n- ")
        .replace("</li>", "")
        .replace("<ul>", "")
        .replace("</ul>", "")
        .replace("~", "")
}

// Tests
#[cfg(test)]
mod tests {
    use super::*;
    use ropey::Rope;
    use std::fs;
    use std::path::{Path, PathBuf};
    use tempfile::tempdir;
    use tower_lsp::lsp_types::Position;

    fn param_doc(ned: &str, param: &str) -> Option<String> {
        let (db, file) = crate::test_utils::setup_ned(ned);
        let uri = file.uri(&db).clone();
        super::documentation_for_parameter(&db, &uri, "App", param)
    }

    #[test]
    fn parameter_documentation_from_trailing_comment() {
        let ned = "\
simple App
{
    parameters:
        int numApps = default(0); // number of apps
        int plain = 1;
}
";
        assert_eq!(param_doc(ned, "numApps").as_deref(), Some("number of apps"));
        assert_eq!(param_doc(ned, "plain"), None);
    }

    #[test]
    fn parameter_documentation_combines_above_and_trailing() {
        let ned = "\
simple App
{
    parameters:
        // Port the app listens on.
        int port = default(1000); // must be free
}
";
        assert_eq!(
            param_doc(ned, "port").as_deref(),
            Some("Port the app listens on.\n\nmust be free")
        );
    }

    #[test]
    fn trailing_comment_must_follow_the_declaration() {
        let ned = "\
simple App
{
    parameters:
        string url = \"http://example.com\";
        int a = 1; int b = 2; // only b
}
";
        // "//" inside the value is not a comment
        assert_eq!(param_doc(ned, "url"), None);
        // the comment belongs to the last declaration on the line
        assert_eq!(param_doc(ned, "a"), None);
        assert_eq!(param_doc(ned, "b").as_deref(), Some("only b"));
    }

    #[test]
    fn documentation_keeps_every_comment_line() {
        // the module node starts at the line break after the comment → the last line must not get lost
        let ned = "\
// First line.
// Last line.
module Host
{
}
";
        let (db, file) = crate::test_utils::setup_ned(ned);
        let uri = file.uri(&db).clone();
        assert_eq!(
            super::documentation_for_type(&db, &uri, "Host").as_deref(),
            Some("First line.\nLast line.")
        );
    }

    #[test]
    fn test_byte_to_point() {
        let text = "Hello, 😂\nThis is a test.\n";
        let rope = Rope::from_str(text);

        // "Hello, " 7 bytes + smiley 4 bytes = byte 11 on line 0
        //
        let point = byte_to_point(&rope, 11);
        // eprintln("{:?}", point);
        assert_eq!(point, tree_sitter::Point { row: 0, column: 11 });

        // 5 bytes into line 1 (line 0 is 11 bytes + newline → line 1 starts at 12)
        let point2 = byte_to_point(&rope, 12 + 5);
        // eprintln("{:?}", point2);
        assert_eq!(point2, tree_sitter::Point { row: 1, column: 5 });
    }
    #[test]
    fn test_char_to_lsp_position_roundtrip_utf16() {
        let text = "Hello, 😂 öpp\nnetwork = Väns\n";
        let rope = Rope::from_str(text);

        for char_offset in 0..=rope.len_chars() {
            let pos = char_to_lsp_position(&rope, char_offset);
            assert_eq!(
                position_to_char(&rope, pos),
                char_offset,
                "roundtrip failed at char offset {char_offset}"
            );
        }

        // the smiley should be two utf-16 units char 8 → column 9
        //
        let pos = char_to_lsp_position(&rope, 8);
        assert_eq!((pos.line, pos.character), (0, 9));
    }

    fn canonicalize_temp(path: &Path) -> PathBuf {
        std::fs::canonicalize(path).expect("Failed to canonicalize temp path")
    }

    #[test]
    fn test_resolve_relative_path() {
        let workspace = tempdir().expect("Failed to create temp dir");
        let workspace_root = canonicalize_temp(workspace.path());

        // Create a test file
        let file_path = workspace_root.join("test_file.txt");
        fs::write(&file_path, "content").expect("Failed to write file");

        let input = Path::new("test_file.txt");
        let expected = canonicalize_temp(&file_path);

        assert_eq!(resolve_path(input, &workspace_root), Some(expected));
    }

    #[test]
    fn test_resolve_absolute_path() {
        let workspace = tempdir().expect("Failed to create temp dir");
        let workspace_root = canonicalize_temp(workspace.path());

        // Create a test file
        let file_path = workspace_root.join("test_file.txt");
        fs::write(&file_path, "content").expect("Failed to write file");

        // Get the absolute (and canonicalized) path
        let input = canonicalize_temp(&file_path);
        let expected = input.clone();

        assert_eq!(resolve_path(&input, &workspace_root), Some(expected));
    }

    #[test]
    fn test_resolve_dot_dot_path() {
        let workspace = tempdir().expect("Failed to create temp dir");
        let workspace_root = canonicalize_temp(workspace.path());

        let sub_dir = workspace_root.join("sub");
        fs::create_dir(&sub_dir).expect("Failed to create sub dir");

        let file_path = workspace_root.join("test_file.txt");
        fs::write(&file_path, "content").expect("Failed to write file");

        let input = Path::new("sub/../test_file.txt");
        let expected = canonicalize_temp(&file_path);

        assert_eq!(resolve_path(input, &workspace_root), Some(expected));
    }

    #[test]
    fn test_resolve_non_existent_relative_path() {
        // This test checks the *original* function's fallback behavior
        let workspace = tempdir().expect("Failed to create temp dir");
        let workspace_root = canonicalize_temp(workspace.path());

        let input = Path::new("does_not_exist.txt");

        // Expected: the uncanonicalized, joined path
        let expected = workspace_root.join(input);

        // The original function returns the uncanonicalized path on error
        assert_eq!(resolve_path(input, &workspace_root), Some(expected));
    }

    #[test]
    fn test_resolve_home_directory_path() {
        let workspace = tempdir().expect("Failed to create temp dir");
        let workspace_root = canonicalize_temp(workspace.path());

        // If file does not exists

        let home = std::env::home_dir().expect("Failed to get home directory");
        let input = Path::new("~/some_non_existent_file_abc123.txt");

        // Expected: the uncanonicalized, home-expanded path
        let expected = home.join("some_non_existent_file_abc123.txt");

        assert_eq!(resolve_path(input, &workspace_root), Some(expected));
    }

    #[test]
    fn test_resolve_path_with_non_utf8() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;

        let workspace = tempdir().expect("Failed to create temp dir");
        let workspace_root = canonicalize_temp(workspace.path());

        // Create an invalid UTF-8 path
        let invalid_bytes = [0x66, 0x6f, 0x80, 0x6f]; // "fo\x80o"
        let invalid_os_str = OsStr::from_bytes(&invalid_bytes);
        let input = Path::new(invalid_os_str);

        assert_eq!(resolve_path(input, &workspace_root), None);
    }

    #[test]
    fn test_get_fqn_under_cursor() {
        let text = "nic: <nicType> like org.car2x.veins.modules.nic.INic80211p {";
        let rope = Rope::from_str(text);

        let pos = Position {
            line: 0,
            character: 30,
        };

        let (word, span) = super::get_fqn_under_cursor(&rope, pos).unwrap();

        assert_eq!(word, "org.car2x.veins.modules.nic.INic80211p");
        assert_eq!(span.start, 20);
        assert_eq!(span.end, 58);
    }
}
