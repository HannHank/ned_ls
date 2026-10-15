use ropey::Rope;

use crate::{db::Db, hir};

pub fn child_field_name<'db>(
    db: &'db dyn Db,
    field_name: &str,
    node: tree_sitter::Node,
    rope: &Rope,
) -> Option<NodeContent<'db>> {
    let child = node.child_by_field_name(field_name.as_bytes())?;

    extract_field(db, &child, rope)
}

pub fn text_for_node(node: &tree_sitter::Node, rope: &Rope) -> Option<String> {
    let start = node.start_byte();
    let end = node.end_byte();

    if start > rope.len_bytes() || end > rope.len_bytes() || start > end {
        return None;
    }

    // to_string() is more robust than as_str()
    Some(rope.byte_slice(start..end).to_string())
}

pub struct NodeContent<'a> {
    pub word: hir::Word<'a>,
    pub span: hir::Span,
}

pub fn extract_field<'db>(
    db: &'db dyn Db,
    node: &tree_sitter::Node,
    rope: &Rope,
) -> Option<NodeContent<'db>> {
    text_for_node(node, rope).map(|s| NodeContent {
        word: hir::Word::new(db, s),
        span: hir::Span::new(node.start_byte(), node.end_byte()),
    })
}

pub fn get_target_node<'a>(
    byte_offset: usize,
    ast: Option<&'a tree_sitter::Tree>,
) -> Option<tree_sitter::Node<'a>> {
    let node = ast?
        .root_node()
        .descendant_for_byte_range(byte_offset, byte_offset)?;
    // Name is always hit first
    match node.kind() {
        "name" => node.parent(),
        _ => None,
    }
}
