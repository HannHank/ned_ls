use ropey::Rope;
use salsa::Accumulator;
use tree_sitter::Node;

use crate::{
    db::Db,
    hir::{
        Diagnostic, Span,
        ned::gate_hir::{GateHir, build_gates_block},
        ned::param_hir::{ParamBlockItem, ValueKind, build_parameters_block},
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
        /// expression inside <...> without a surrounding default(...) → None for <>
        like_expr: Option<String>,
        /// <"MultiLevelPreemption"> fixed or <default("MultiLevelPreemption")> → None for <>
        like_kind: Option<ValueKind>,
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

        let like = node
            .child_by_field_name("like_expr")
            .and_then(|n| ValueKind::split(&n, rope));
        let like_kind = like.as_ref().map(|(kind, _)| *kind);
        let like_expr = like.and_then(|(_, expr)| expr);

        SubmoduleDefinition::Parametric {
            interface_name,
            like_expr,
            like_kind,
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

#[cfg(test)]
mod tests {
    use super::SubmoduleDefinition;
    use crate::hir::ned::ValueKind;

    #[test]
    fn like_expressions_keep_kind_and_expression() {
        let (db, file) = crate::test_utils::setup_ned(
            "\
network Net
{
    submodules:
        a: <default(\"UdpApp\")> like IApp;
        b: <appType> like IApp;
        c: <> like IApp;
}
",
        );
        let hir = crate::hir::ned::lower_to_hir(&db, file);
        let likes: Vec<(String, Option<String>, Option<ValueKind>)> = hir.items(&db)[0]
            .submodules(&db)
            .iter()
            .map(|s| match &s.definition {
                SubmoduleDefinition::Parametric {
                    like_expr,
                    like_kind,
                    ..
                } => (s.name.clone(), like_expr.clone(), *like_kind),
                SubmoduleDefinition::Concrete { .. } => panic!("expected a like submodule"),
            })
            .collect();

        assert_eq!(
            likes,
            vec![
                (
                    "a".to_string(),
                    Some("\"UdpApp\"".to_string()),
                    Some(ValueKind::Default)
                ),
                (
                    "b".to_string(),
                    Some("appType".to_string()),
                    Some(ValueKind::Fixed)
                ),
                ("c".to_string(), None, None),
            ]
        );
    }

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
