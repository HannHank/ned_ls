use ropey::Rope;

use crate::{
    completion::ned::ContextType,
    db::Db,
    hir::{self, ned, tree_utils::child_field_name},
};

#[salsa::tracked(debug)]
pub struct CompoundModuleHir<'db> {
    pub name: hir::Word<'db>, // "Node"
    /// Span of the entire module definition
    // #[tracked(ignore)]
    pub span: hir::Span,

    /// Represents: `extends <name>, ...`
    #[tracked]
    #[returns(ref)]
    pub extends: Option<ned::ExtendsHir<'db>>,

    /// Represents: `like <name>, ...`
    #[tracked]
    #[returns(ref)]
    pub like: Vec<ned::LikeHir<'db>>,

    /// Represents: `types:` block → which can contain module and channel type definitions
    #[tracked]
    #[returns(ref)]
    pub types: Vec<ned::ItemHir<'db>>,

    /// Represents the `parameters:` block
    #[tracked]
    #[returns(ref)]
    pub parameters: Vec<ned::ParamBlockItem<'db>>,

    /// Represents the `gates:` block
    #[tracked]
    #[returns(ref)]
    pub gates: Vec<ned::GateHir<'db>>,

    /// Represents the `submodules:` block
    #[tracked]
    #[returns(ref)]
    pub submodules: Vec<ned::SubmoduleHir<'db>>,

    /// Represents the `connections:` block
    #[tracked]
    #[returns(ref)]
    pub connections: Vec<ned::connections_hir::ConnectionHir<'db>>,
}

pub fn build_compound_module_hir<'db>(
    db: &'db dyn Db,
    node: tree_sitter::Node,
    rope: &Rope,
    version: i32,
) -> Option<ned::CompoundModuleHir<'db>> {
    let name_word = child_field_name(db, "name", node, rope)?; // TODO: Do not return on error
    let cursor = &mut node.walk();

    // create span
    let span = hir::Span::new(node.start_byte(), node.end_byte());

    let extends_node = child_field_name(db, "extends", node, rope);
    let mut extends_hir: Option<ned::ExtendsHir> = None;
    if let Some(extends) = extends_node {
        extends_hir = Some(ned::ExtendsHir {
            name: extends.word,
            span: extends.span,
        });
    }

    let like_nodes = node.children_by_field_name("implements", cursor);
    let mut like_hir: Vec<ned::LikeHir> = Vec::new();
    for like in like_nodes {
        let node_context_opt = hir::tree_utils::extract_field(db, &like, rope);
        if let Some(node_context) = node_context_opt {
            like_hir.push(ned::LikeHir {
                name: node_context.word,
                span: node_context.span,
            });
        }
    }

    let sections =
        super::parse_module_sections(db, node, rope, version, ContextType::CompoundModule);

    Some(ned::CompoundModuleHir::new(
        db,
        name_word.word,
        span,
        extends_hir,
        like_hir,
        sections.types,
        sections.parameters,
        sections.gates,
        sections.submodules,
        sections.connections,
    ))
}
