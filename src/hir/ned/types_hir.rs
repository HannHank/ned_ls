use tower_lsp::lsp_types::Url;

use crate::{
    db::Db,
    hir::{ItemHir, ned::parse_definition_node},
};

pub fn build_types_block<'db>(
    db: &'db dyn Db,
    node: &tree_sitter::Node,
    rope: &ropey::Rope,
    url: Option<&Url>,
    version: i32,
) -> Option<Vec<ItemHir<'db>>> {
    let mut local_types = Vec::new();
    let mut cursor = node.walk();

    for child in node.children(&mut cursor) {
        if let Some(item) = parse_definition_node(db, child, rope, url, version) {
            local_types.push(item);
        }
    }

    Some(local_types)
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_ast_dump_types() {
        let code = r#"
        network Test {
            types:
                module Area {
                    gates:
                        inout ethg[];
                }
        }
        "#;
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&tree_sitter_ned::language()).unwrap();
        let tree = parser.parse(code, None).unwrap();

        let mut cursor = tree.walk();
        for child in tree.root_node().children(&mut cursor) {
            if child.kind() == "network" {
                let mut network_cursor = child.walk();
                for subchild in child.children(&mut network_cursor) {
                    if subchild.kind() == "types" {
                        println!("TYPES NODE: {}", subchild.to_sexp());
                        for c in subchild.children(&mut tree.walk()) {
                            println!(" - CHILD KIND: {}", c.kind());
                        }
                    }
                }
            }
        }
    }
}
