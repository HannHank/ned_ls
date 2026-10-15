use ropey::Rope;

use crate::{
    db::Db,
    hir::{tree_utils::child_field_name, *},
};
/// Represents: `inout port[];`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, salsa::SalsaValue)]
pub struct GateHir<'db> {
    pub name: Word<'db>, // "port"
    pub span: Span,
    pub direction: GateDirection, // GateDirection::Inout
    pub is_vector: bool,
}

/// Represents the direction of a gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, salsa::SalsaValue)]
#[allow(dead_code)]
pub enum GateDirection {
    Input,
    Output,
    Inout,
}

pub fn build_gates_block<'db>(
    db: &'db dyn Db,
    node: &tree_sitter::Node,
    rope: &Rope,
    version: i32,
) -> Option<Vec<GateHir<'db>>> {
    let mut gates: Vec<GateHir> = Vec::new();
    let cursor = &mut node.walk();
    // go into parameter block
    for child in node.children(cursor) {
        match child.kind() {
            "gate" => {
                // Extract parameter name and type

                let cursor = &mut child.walk();

                for gate_child in child.children(cursor) {
                    if gate_child.kind() == "ERROR" {
                        // use salsa to accumulate diagnostics
                        Diagnostic {
                            start: gate_child.start_byte(),
                            end: gate_child.end_byte(),
                            message: "unexpected character ~ Gate Level".to_string(),
                            version,
                            severity: crate::hir::Severity::Error,
                            url: None,
                        }
                        .accumulate(db);
                    }
                }

                let name_opt = child_field_name(db, "name", child, rope);
                let type_opt = child_field_name(db, "type", child, rope); // simple "input", "output"
                let vector_opt = child.child_by_field_name("vector");

                if let Some(name_word) = name_opt {
                    // Parse direction
                    let direction = match type_opt.map(|t| t.word.text(db).as_str()) {
                        Some("input") => GateDirection::Input,
                        Some("output") => GateDirection::Output,
                        _ => GateDirection::Inout, // default or explicit "inout"
                    };

                    let is_vector = vector_opt.is_some();

                    let span = Span::new(child.start_byte(), child.end_byte());

                    gates.push(GateHir {
                        name: name_word.word,
                        span,
                        direction,
                        is_vector,
                    });
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
            _ => {
                // eprint!("Unexpected node in gates block: {}", child.kind());
            }
        }
    }
    Some(gates)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ast_dump() {
        let code = "
        module Test {
            gates:
                inout ethg[];
        }
        ";
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&tree_sitter_ned::language()).unwrap();
        let tree = parser.parse(code, None).unwrap();
        println!("AST DUMP:\n{}", tree.root_node().to_sexp());

        let rope = ropey::Rope::from_str(code);
        let db = crate::db::RootLspDatabase::default();

        let mut cursor = tree.walk();
        let mut gates = None;
        for child in tree.root_node().children(&mut cursor) {
            if child.kind() == "module" {
                let mut module_cursor = child.walk();
                for subchild in child.children(&mut module_cursor) {
                    if subchild.kind() == "gates" {
                        gates = build_gates_block(&db, &subchild, &rope, 1);
                    }
                }
            }
        }

        println!("GATES: {:#?}", gates);
    }
}
