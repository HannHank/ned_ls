use crate::{
    db::Db,
    hir::{self, Span, Word},
};
use ropey::Rope;

#[derive(Debug, Clone, PartialEq, Eq, Hash, salsa::SalsaValue)]
pub enum ArrowKind {
    Right,         // -->
    Left,          // <--
    Bidirectional, // <-->
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, salsa::SalsaValue)]
pub struct ConnectionEndpoint<'db> {
    pub module_name: Option<Word<'db>>,
    pub gate_name: Word<'db>,
    pub is_plusplus: bool,
    pub has_index: bool,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, salsa::SalsaValue)]
pub struct ChannelSpec<'db> {
    pub name: Option<Word<'db>>,
    pub channel_type: Option<Word<'db>>,
    pub span: Span,
}

// I decided to model the connection with two endpoints and an arrow in between to make the
// structure more explicit and easier to work with
#[derive(Debug, Clone, PartialEq, Eq, Hash, salsa::SalsaValue)]
pub struct ConnectionHir<'db> {
    pub src: ConnectionEndpoint<'db>,
    pub arrow: ArrowKind,
    pub dest: ConnectionEndpoint<'db>,
    pub channel: Option<ChannelSpec<'db>>,
    pub span: Span,
}

fn build_endpoint<'db>(
    db: &'db dyn Db,
    node: tree_sitter::Node,
    rope: &Rope,
) -> Option<ConnectionEndpoint<'db>> {
    let gate_node = node.child_by_field_name("gate")?;
    let gate_name_node = gate_node.child_by_field_name("name")?;
    let gate_name_content = hir::tree_utils::extract_field(db, &gate_name_node, rope)?;

    let mut module_name = None;
    // The module part is aliased as "module" in the grammar → https://github.com/HannHank/tree-sitter-ned/blob/main/grammar.js#L466-L475
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "module"
            && let Some(module_name_node) = child.child_by_field_name("name")
            && let Some(module_content) =
                hir::tree_utils::extract_field(db, &module_name_node, rope)
        {
            module_name = Some(module_content.word);
        }
    }

    let is_plusplus = gate_node.child_by_field_name("plusplpus").is_some()
        || gate_node.child_by_field_name("plusplus").is_some();
    let has_index = gate_node.child_by_field_name("index").is_some();

    Some(ConnectionEndpoint {
        module_name,
        gate_name: gate_name_content.word,
        is_plusplus,
        has_index,
        span: Span::new(node.start_byte(), node.end_byte()),
    })
}

pub fn build_connections_block<'db>(
    db: &'db dyn Db,
    node: &tree_sitter::Node,
    rope: &Rope,
) -> Option<Vec<ConnectionHir<'db>>> {
    let mut connections = Vec::new();
    let mut queue = vec![*node];

    while let Some(current_node) = queue.pop() {
        if current_node.kind() == "connection" {
            let src_node = match current_node.child_by_field_name("src") {
                Some(n) => n,
                None => continue,
            };
            let dest_node = match current_node.child_by_field_name("dest") {
                Some(n) => n,
                None => continue,
            };

            let src = match build_endpoint(db, src_node, rope) {
                Some(e) => e,
                None => continue,
            };
            let dest = match build_endpoint(db, dest_node, rope) {
                Some(e) => e,
                None => continue,
            };

            let mut arrow_kind = ArrowKind::Right;
            let mut channel = None;

            let channel_name_node = current_node.child_by_field_name("name");
            let channel_type_node = current_node.child_by_field_name("type");
            let like_expr_node = current_node.child_by_field_name("like_expr");

            if channel_name_node.is_some()
                || channel_type_node.is_some()
                || like_expr_node.is_some()
            {
                let name = channel_name_node
                    .and_then(|n| hir::tree_utils::extract_field(db, &n, rope).map(|c| c.word));
                let channel_type = channel_type_node
                    .and_then(|n| hir::tree_utils::extract_field(db, &n, rope).map(|c| c.word));
                let like_expr = like_expr_node
                    .and_then(|n| hir::tree_utils::extract_field(db, &n, rope).map(|c| c.word));

                let actual_type = if channel_type.is_some() {
                    channel_type
                } else if like_expr.is_some() {
                    like_expr
                } else {
                    name
                };

                let span_node = channel_type_node
                    .or(like_expr_node)
                    .or(channel_name_node)
                    .unwrap();

                channel = Some(ChannelSpec {
                    name,
                    channel_type: actual_type,
                    span: Span::new(span_node.start_byte(), span_node.end_byte()),
                });
            }

            let mut arrow_cursor = current_node.walk();
            for subchild in current_node.children(&mut arrow_cursor) {
                if subchild.kind() == "arrow"
                    && let Some(arrow_text) = hir::tree_utils::text_for_node(&subchild, rope)
                {
                    match arrow_text.as_str() {
                        "-->" => arrow_kind = ArrowKind::Right,
                        "<--" => arrow_kind = ArrowKind::Left,
                        "<-->" => arrow_kind = ArrowKind::Bidirectional,
                        _ => {}
                    }
                }
            }

            connections.push(ConnectionHir {
                src,
                arrow: arrow_kind,
                dest,
                channel,
                span: Span::new(current_node.start_byte(), current_node.end_byte()),
            });
        } else {
            let mut child_cursor = current_node.walk();
            for child in current_node.children(&mut child_cursor) {
                queue.push(child);
            }
        }
    }

    Some(connections)
}

// TODO: Need to finish the test and also create some more tests for the other hirs aswell.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ast_dump() {
        let code = "
        network Test {
            connections:
                A <--> e1: NormalP <--> B;
                C <--> NormalP <--> D;
                E <--> <default(\"NormalP\")> like IChannel <--> F;
        }
        ";
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&tree_sitter_ned::language()).unwrap();
        let tree = parser.parse(code, None).unwrap();

        let db = crate::db::RootLspDatabase::default();
        let rope = ropey::Rope::from_str(code);

        // Find connections node
        let mut cursor = tree.walk();
        for child in tree.root_node().children(&mut cursor) {
            if child.kind() == "network" {
                let mut network_cursor = child.walk();
                for subchild in child.children(&mut network_cursor) {
                    if subchild.kind() == "connections" {
                        let hir = build_connections_block(&db, &subchild, &rope).unwrap();
                        println!("HIR: {:#?}", hir);
                    }
                }
            }
        }
        println!("AST DUMP: {}", tree.root_node().to_sexp());
    }
}
