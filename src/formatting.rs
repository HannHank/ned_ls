use tree_sitter::Node;

pub struct NedFormatter<'a> {
    lines: Vec<&'a str>,
    indents: Vec<Option<usize>>,
    needs_semicolon: Vec<bool>, // Maybe this can be more generic
    indent_size: usize,
}

impl<'a> NedFormatter<'a> {
    pub fn new(source: &'a str) -> Self {
        let lines: Vec<&str> = source.lines().collect();
        let len = lines.len();
        Self {
            lines,
            indents: vec![None; len],
            needs_semicolon: vec![false; len],
            indent_size: 4,
        }
    }

    pub fn format(mut self, root: Node) -> String {
        self.visit_node(root, 0);

        let mut output = String::new();
        for (i, line) in self.lines.iter().enumerate() {
            if line.trim().is_empty() {
                output.push('\n');
                continue;
            }

            let mut formatted_line = if let Some(indent_level) = self.indents[i] {
                let trimmed = line.trim_start();
                format!("{}{}", " ".repeat(indent_level * self.indent_size), trimmed)
            } else {
                // keep spacing
                line.to_string()
            };

            // insert semicolon if it was missing -> more generic in the future
            if self.needs_semicolon[i] {
                if let Some(comment_idx) = formatted_line.find("//") {
                    // Split at the comment
                    let code_part = formatted_line[..comment_idx].trim_end();
                    let comment_part = &formatted_line[comment_idx..];
                    formatted_line = format!("{}; {}", code_part, comment_part);
                } else {
                    // Without comment, just append
                    formatted_line = format!("{};", formatted_line.trim_end());
                }
            }

            output.push_str(&formatted_line);
            output.push('\n');
        }

        // Cleanup trailing newlines
        while output.ends_with("\n\n") {
            output.pop();
        }
        if !output.ends_with('\n') {
            output.push('\n');
        }

        output
    }

    fn visit_node(&mut self, node: Node, indent: usize) {
        // If tree-sitter knows this node is missing, I might as well just add it while formatting.
        // Currently only semicolons are inserted, because I am not sure which other kinds the
        // parser can also infer the correct structure for.
        if node.is_missing() && node.kind() == ";" {
            let target_row = if let Some(prev) = node.prev_sibling() {
                prev.end_position().row
            } else {
                node.start_position().row
            };

            if target_row < self.needs_semicolon.len() {
                self.needs_semicolon[target_row] = true;
            }
            return; // Missing nodes should not have children  
        }

        let start_row = node.start_position().row;
        if start_row < self.indents.len() && self.indents[start_row].is_none() {
            self.indents[start_row] = Some(indent);
        }

        match node.kind() {
            "simple" | "module" | "network" | "moduleinterface" | "channel"
            | "channel_interface" | "submodule" | "for_loop" | "if_statement" => {
                let mut cursor = node.walk();
                for child in node.children(&mut cursor) {
                    if child.kind() == "}" || child.kind() == "{" {
                        self.visit_node(child, indent);
                    } else {
                        self.visit_node(child, indent + 1);
                    }
                }
            }

            "parameters" | "gates" | "types" | "submodules" | "connections" => {
                let mut cursor = node.walk();
                for child in node.children(&mut cursor) {
                    if matches!(
                        child.kind(),
                        "parameter"
                            | "property"
                            | "gate"
                            | "submodule"
                            | "connection"
                            | "comment"
                            | "for_loop"
                            | "if_statement"
                    ) {
                        self.visit_node(child, indent + 1);
                    } else {
                        self.visit_node(child, indent);
                    }
                }
            }

            _ => {
                let mut cursor = node.walk();
                for child in node.children(&mut cursor) {
                    self.visit_node(child, indent);
                }
            }
        }
    }
}

pub fn format(root: Node, source: &str) -> String {
    let formatter = NedFormatter::new(source);
    formatter.format(root)
}

// Formatting Tests
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simple_module() {
        // If the semicolon is missing in the first parameter, the parser gets to confused right
        // now
        let not_supported = r#"module SimpleModule {
parameters:
    int param1
    int param2; // This is a test with comment
gates:
    input inGate;
    output outGate;
}"#;

        let expected = r#"module SimpleModule {
    parameters:
        int param1;
        int param2; // This is a test with comment
    gates:
        input inGate;
        output outGate;
}
"#;

        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&tree_sitter_ned::language()).unwrap();
        let not_supported_tree = parser.parse(not_supported, None).unwrap();
        let not_supported_root = not_supported_tree.root_node();

        let formatted = format(not_supported_root, not_supported);

        assert_ne!(formatted, expected);

        // If the semicolon is missing in the second parameter, the Tree-Sitter LR parser is able
        // to infer the correct structure, and I can add the semicolon in the right place
        let supported = r#"module SimpleModule {
parameters:
    int param1;
    int param2 // This is a test with comment
gates:
    input inGate;
    output outGate;
}"#;
        let supported_tree = parser.parse(supported, None).unwrap();
        let supported_root = supported_tree.root_node();

        let formatted = format(supported_root, supported);

        assert_eq!(formatted, expected);
    }
}
