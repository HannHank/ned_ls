use crate::db::Db;
use crate::goto_definition::{ResolvedDefinition, ScopeResolver};
use crate::hir;
use crate::hir::tree_utils::text_for_node;
use crate::utils;
use tower_lsp::lsp_types::{Position, Range, Url};

/// The symbol under the cursor resolved to its definition
pub struct SymbolAt<'db> {
    pub target: ResolvedDefinition<'db>,
    pub word: String,
    pub token_range: Range, // range to replace
}

/// Resolves the identifier under the cursor
// That is quite cool because this is shared by show reference and rename
pub fn symbol_at<'db>(
    db: &'db crate::db::RootLspDatabase,
    uri: &Url,
    position: Position,
) -> Option<SymbolAt<'db>> {
    // default to get the current ast
    let source_input = db.get_source_file_input(uri)?;
    let rope = source_input.rope(db);

    let char_offset = utils::position_to_char(rope, position);
    let byte_offset = rope.char_to_byte(char_offset);

    let ast = db.get_ast_for_file(&source_input)?;
    let node = ast
        .root_node()
        .descendant_for_byte_range(byte_offset, byte_offset)?;

    let mut t_node = node;
    while !t_node.is_named() && t_node.parent().is_some() {
        t_node = t_node.parent().unwrap();
    }
    let name_node = t_node.child_by_field_name("name").unwrap_or(t_node);
    let word = text_for_node(&name_node, rope)?;

    // for now I am not supporting pattern rename or references
    if word.is_empty() || !word.chars().all(|c| c.is_alphanumeric() || c == '_') {
        return None;
    }

    // Just contact the scope resolver like goto_def
    let resolver = ScopeResolver::new(db, source_input);
    let target = resolver
        .resolve(&t_node, byte_offset, &word)?
        .first()?
        .clone();

    let token_range = utils::span_to_range(
        rope,
        hir::Span::new(name_node.start_byte(), name_node.end_byte()),
    )?;

    Some(SymbolAt {
        target,
        word,
        token_range,
    })
}
