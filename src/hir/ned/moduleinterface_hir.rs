use crate::{
    completion::ned::ContextType,
    db::Db,
    hir::{self, tree_utils::child_field_name},
};
use ropey::Rope;

#[salsa::tracked(debug)]
pub struct ModuleInterfaceHir<'db> {
    pub name: hir::Word<'db>,
    pub span: hir::Span,

    /// Represents: `extends <name>, <name>, ...`
    /// Module interfaces support multiple inheritance
    #[tracked]
    #[returns(ref)]
    pub extends: Vec<super::ExtendsHir<'db>>,

    /// Represents the `parameters:` block
    #[tracked]
    #[returns(ref)]
    pub parameters: Vec<super::ParamBlockItem<'db>>,

    /// Represents the `gates:` block
    #[tracked]
    #[returns(ref)]
    pub gates: Vec<super::GateHir<'db>>,
}

pub fn build_module_interface_hir<'db>(
    db: &'db dyn Db,
    node: tree_sitter::Node,
    rope: &Rope,
    version: i32,
) -> Option<ModuleInterfaceHir<'db>> {
    let name_word = child_field_name(db, "name", node, rope)?;

    let span = hir::Span::new(node.start_byte(), node.end_byte());

    let mut extends_list: Vec<super::ExtendsHir> = Vec::new();

    let mut cursor = node.walk();
    // Moduleinterface can have multiple extends
    for child in node.children_by_field_name("extends", &mut cursor) {
        if let Some(content) = hir::tree_utils::extract_field(db, &child, rope) {
            extends_list.push(super::ExtendsHir {
                name: content.word,
                span: hir::Span::new(child.start_byte(), child.end_byte()),
            });
        }
    }

    let sections =
        super::parse_module_sections(db, node, rope, version, ContextType::ModuleInterface);

    Some(ModuleInterfaceHir::new(
        db,
        name_word.word,
        span,
        extends_list,
        sections.parameters,
        sections.gates,
    ))
}
