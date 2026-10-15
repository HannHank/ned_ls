use ropey::Rope;

use crate::{
    completion::ned::ContextType,
    db::Db,
    hir::{self, ned::section_parser::parse_module_sections, tree_utils::child_field_name},
};

#[salsa::tracked(debug)]
pub struct NetworkHir<'db> {
    pub name: hir::Word<'db>, // "Node"
    /// Span of the entire module definition
    // #[tracked(ignore)]
    pub span: hir::Span,

    /// Represents: `extends <name>, ...`
    #[tracked]
    #[returns(ref)]
    pub extends: Option<super::ExtendsHir<'db>>,

    /// Represents: `types:` block → which can contain module and channel type definitions
    #[tracked]
    #[returns(ref)]
    pub types: Vec<super::ItemHir<'db>>,

    // // /// Represents the `parameters:` block
    #[tracked]
    #[returns(ref)]
    pub parameters: Vec<super::ParamBlockItem<'db>>,

    /// Represents the `gates:` block
    #[tracked]
    #[returns(ref)]
    pub gates: Vec<super::GateHir<'db>>,

    /// Represents the `submodules:` block
    #[tracked]
    #[returns(ref)]
    pub submodules: Vec<super::SubmoduleHir<'db>>,

    /// Represents the `connections:` block
    #[tracked]
    #[returns(ref)]
    pub connections: Vec<super::connections_hir::ConnectionHir<'db>>,
}

pub fn build_network_hir<'db>(
    db: &'db dyn Db,
    node: tree_sitter::Node,
    rope: &Rope,
    version: i32,
) -> Option<NetworkHir<'db>> {
    let name_word = child_field_name(db, "name", node, rope)?; // TODO: Do not return on error

    // create span
    let span = hir::Span::new(node.start_byte(), node.end_byte());

    let extends_node = child_field_name(db, "extends", node, rope);
    let mut extends_hir: Option<super::ExtendsHir> = None;
    if let Some(extends) = extends_node {
        extends_hir = Some(super::ExtendsHir {
            name: extends.word,
            span: extends.span,
        });
    }

    let sections = parse_module_sections(db, node, rope, version, ContextType::Network);

    Some(NetworkHir::new(
        db,
        name_word.word,
        span,
        extends_hir,
        sections.types,
        sections.parameters,
        sections.gates,
        sections.submodules,
        sections.connections,
    ))
}
