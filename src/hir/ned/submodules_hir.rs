use ropey::Rope;
use salsa::Accumulator;
use tree_sitter::Node;

use crate::{
    db::Db,
    hir::{
        Diagnostic, Span,
        ned::gate_hir::{GateHir, build_gates_block},
        ned::param_hir::{ParamBlockItem, build_parameters_block},
        tree_utils::text_for_node,
    },
};

/// Represents: `app: App;` or `queue[sizeof(port)]: Queue;`
#[derive(Debug, Clone, PartialEq, Eq, Hash, salsa::SalsaValue)]
pub struct SubmoduleHir<'db> {
    pub name: String,

    pub vector_size_expr: Option<String>,

    pub condition: Option<String>,

    pub definition: SubmoduleDefinition,

    pub has_body: bool,

    pub parameters: Vec<ParamBlockItem<'db>>,

    pub gates: Vec<GateHir<'db>>,

    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SubmoduleDefinition {
    Concrete {
        type_name: String,
        span: Span,
    },

    Parametric {
        interface_name: String,
        like_expr: Option<String>,
        span: Span,
    },
}

pub fn build_submodules_block<'db>(
    db: &'db dyn Db,
    node: &tree_sitter::Node,
    rope: &Rope,
    version: i32,
) -> Option<Vec<SubmoduleHir<'db>>> {
    let mut submodules: Vec<SubmoduleHir> = Vec::new();

    let cursor = &mut node.walk();
    for child in node.children(cursor) {
        match child.kind() {
            "submodule" => {
                if let Some(submodule_hir) = build_submodule_hir(db, &child, rope, version) {
                    submodules.push(submodule_hir);
                }
            }
            "ERROR" => {
                Diagnostic {
                    start: child.start_byte(),
                    end: child.end_byte(),
                    message: "unexpected character ~ Gates Level".to_string(),
                    version,
                    severity: crate::hir::Severity::Error,
                    url: None,
                }
                .accumulate(db);
            }
            _ => {}
        }
    }

    Some(submodules)
}

pub fn build_submodule_hir<'db>(
    db: &'db dyn Db,
    node: &Node,
    rope: &Rope,
    version: i32,
) -> Option<SubmoduleHir<'db>> {
    let name = node
        .child_by_field_name("name")
        .and_then(|n| text_for_node(&n, rope))
        .unwrap_or_default();

    let vector_size_expr = node
        .child_by_field_name("vector")
        .and_then(|v_node| v_node.child_by_field_name("size"))
        .and_then(|size_node| text_for_node(&size_node, rope));

    let definition = if let Some(like_type_node) = node.child_by_field_name("like_type") {
        let interface_name = text_for_node(&like_type_node, rope).unwrap_or_default();

        let like_expr = node
            .child_by_field_name("like_expr")
            .and_then(|n| text_for_node(&n, rope))
            .map(|raw| clean_like_expr(&raw))
            .filter(|s| !s.is_empty());

        SubmoduleDefinition::Parametric {
            interface_name,
            like_expr,
            span: Span {
                start: like_type_node.start_byte(),
                end: like_type_node.end_byte(),
            },
        }
    } else {
        // Unknown while typing
        // TODO: better error handling
        let type_name = node
            .child_by_field_name("type")
            .and_then(|n| text_for_node(&n, rope))
            .unwrap_or_else(|| "Unknown".to_string());

        SubmoduleDefinition::Concrete {
            type_name,
            span: Span {
                start: node.start_byte(),
                end: node.end_byte(),
            },
        }
    };

    let mut condition = None;
    let mut parameters = Vec::new();
    let mut gates = Vec::new();
    let mut has_body = false;

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "condition" => {
                if let Some(val) = child.child_by_field_name("value") {
                    condition = text_for_node(&val, rope);
                }
            }
            "submodule_parameters" => {
                parameters = build_parameters_block(db, &child, rope, version).unwrap_or_default();
                has_body = true;
            }
            "submodule_gates" => {
                gates = build_gates_block(db, &child, rope, version).unwrap_or_default();
                has_body = true;
            }
            "{" => has_body = true,
            "ERROR" => {
                Diagnostic {
                    start: child.start_byte(),
                    end: child.end_byte(),
                    message: "Syntax error in submodule definition".to_string(),
                    version,
                    severity: crate::hir::Severity::Error,
                    url: None,
                }
                .accumulate(db);
            }
            _ => {}
        }
    }

    // Span
    let span = Span {
        start: node.start_byte(),
        end: node.end_byte(),
    };

    Some(SubmoduleHir {
        name,
        vector_size_expr,
        condition,
        definition,
        has_body,
        parameters,
        gates,
        span,
    })
}
/// Turns "<expr>" into "expr" and "<>" into ""
fn clean_like_expr(raw: &str) -> String {
    raw.trim()
        .strip_prefix('<')
        .unwrap_or(raw)
        .strip_suffix('>')
        .unwrap_or(raw)
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {

    #[test]
    fn test_submodule_ast() {
        let code = r#"
        network Test {
            submodules:
                ethernet: <default("EthernetEncapsulation")> like IEthernetLayer if typename != "" && exists(bridging) {
                    @display("p=700,300");
                }
        }
        "#;
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&tree_sitter_ned::language()).unwrap();
        let tree = parser.parse(code, None).unwrap();
        println!("AST DUMP: {}", tree.root_node().to_sexp());
        // panic!("show output");
    }
}
