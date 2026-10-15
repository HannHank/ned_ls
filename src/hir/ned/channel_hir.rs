use ropey::Rope;

use super::*;
use crate::{completion::ned::ContextType, db::Db};

#[salsa::tracked(debug)]
pub struct ChannelHir<'db> {
    pub name: Word<'db>, // "LINK_100"
    /// Span of the entire channel definition
    pub span: Span,

    /// Represents: `extends <name>, ...`
    #[tracked]
    #[returns(ref)]
    pub extends: Option<ExtendsHir<'db>>,

    /// Represents: `like <name>, ...`
    #[tracked]
    #[returns(ref)]
    pub like: Vec<LikeHir<'db>>,

    /// Represents the `parameters:` block
    #[tracked]
    #[returns(ref)]
    pub parameters: Vec<ParamBlockItem<'db>>,
}

pub fn build_channel_hir<'db>(
    db: &'db dyn Db,
    node: tree_sitter::Node,
    rope: &Rope,
    version: i32,
) -> Option<ChannelHir<'db>> {
    let name_word = child_field_name(db, "name", node, rope)?;

    let cursor = &mut node.walk();

    // create span
    let span = Span::new(node.start_byte(), node.end_byte());

    let extends_node = child_field_name(db, "extends", node, rope);
    let mut extends_hir: Option<ExtendsHir> = None;
    if let Some(extends) = extends_node {
        extends_hir = Some(ExtendsHir {
            name: extends.word,
            span: extends.span,
        });
    }

    let like_nodes = node.children_by_field_name("implements", cursor);
    let mut like_hir: Vec<LikeHir> = Vec::new();
    for like in like_nodes {
        let node_context_opt = extract_field(db, &like, rope);
        if let Some(node_context) = node_context_opt {
            like_hir.push(LikeHir {
                name: node_context.word,
                span: node_context.span,
            });
        }
    }

    let sections = parse_module_sections(db, node, rope, version, ContextType::Channel);

    Some(ChannelHir::new(
        db,
        name_word.word,
        span,
        extends_hir,
        like_hir,
        sections.parameters,
    ))
}
