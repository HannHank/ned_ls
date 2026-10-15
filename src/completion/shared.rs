use tower_lsp::lsp_types::{CompletionItem, CompletionTextEdit, Position, Range, TextEdit};

/// Range from the start of the token currently being typed to the cursor.
/// The token starts after the last delimiter
pub fn replacement_range(text_before_cursor: &str, delims: &[char], cursor: Position) -> Range {
    let mut start = 0;
    for (i, ch) in text_before_cursor.chars().enumerate() {
        if delims.contains(&ch) {
            start = i + 1;
        }
    }
    for ch in text_before_cursor.chars().skip(start) {
        if ch.is_whitespace() {
            start += 1;
        } else {
            break;
        }
    }

    // LSP columns are utf-16 code units
    let utf16_col: usize = text_before_cursor
        .chars()
        .take(start)
        .map(|c| c.len_utf16())
        .sum();

    Range {
        start: Position {
            line: cursor.line,
            character: utf16_col as u32,
        },
        end: cursor,
    }
}

/// Attaches the replacement range to all items so clients replace the typed
/// token instead of relying on prefix filtering.
//
// Most of the time vscode is able to figure it out, but I had some edge cases where the suggestion
// was applied in full lenght and the token was duplicated. In addition maybe other clients have no
// prefix filtering at all
pub fn attach_range(items: Vec<CompletionItem>, range: Range) -> Vec<CompletionItem> {
    items
        .into_iter()
        .map(|mut item| {
            let new_text = item
                .insert_text
                .clone()
                .unwrap_or_else(|| item.label.clone());
            item.text_edit = Some(CompletionTextEdit::Edit(TextEdit { range, new_text }));
            item
        })
        .collect()
}
