use ropey::Rope;

use crate::{
    completion::ned::ContextType,
    db::Db,
    hir::{ned::section_parser::parse_module_sections, tree_utils::child_field_name, *},
};

#[salsa::tracked(debug)]
pub struct SimpleModuleHir<'db> {
    pub name: Word<'db>, // "Node"
    /// Span of the entire module definition
    // #[tracked(ignore)]
    pub span: Span,

    /// Represents: `extends <name>, ...`
    #[tracked]
    #[returns(ref)]
    pub extends: Option<super::ExtendsHir<'db>>,

    /// Represents: `like <name>, ...`
    #[tracked]
    #[returns(ref)]
    pub like: Vec<super::LikeHir<'db>>,

    /// Represents the `parameters:` block
    #[tracked]
    #[returns(ref)]
    pub parameters: Vec<super::ParamBlockItem<'db>>,
    #[tracked]
    #[returns(ref)]
    pub gates: Vec<super::GateHir<'db>>,
}

pub fn build_simple_module_hir<'db>(
    db: &'db dyn Db,
    node: tree_sitter::Node,
    rope: &Rope,
    version: i32,
) -> Option<SimpleModuleHir<'db>> {
    let name_word = child_field_name(db, "name", node, rope)?;

    let cursor = &mut node.walk();

    // create span
    let span = Span::new(node.start_byte(), node.end_byte());

    // TODO: This needs to be updated if multiple inheritance is allowed by the grammar
    let extends_node = child_field_name(db, "extends", node, rope);
    let mut extends_hir: Option<super::ExtendsHir> = None;
    if let Some(extends) = extends_node {
        extends_hir = Some(super::ExtendsHir {
            name: extends.word,
            span: extends.span,
        });
    }

    let like_nodes = node.children_by_field_name("implements", cursor);
    let mut like_hir: Vec<super::LikeHir> = Vec::new();
    for like in like_nodes {
        let node_context_opt = tree_utils::extract_field(db, &like, rope);
        if let Some(node_context) = node_context_opt {
            like_hir.push(super::LikeHir {
                name: node_context.word,
                span: node_context.span,
            });
        }
    }
    let sections = parse_module_sections(db, node, rope, version, ContextType::SimpleModule);

    Some(SimpleModuleHir::new(
        db,
        name_word.word,
        span,
        extends_hir,
        like_hir,
        sections.parameters,
        sections.gates,
    ))
}
