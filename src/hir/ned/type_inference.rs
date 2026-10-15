use crate::hir::tree_utils::child_field_name;

#[derive(Debug, Clone, PartialEq, Eq, Hash, salsa::SalsaValue)]
pub enum NedType {
    Bool,
    Int,
    Double,
    String,
    Xml,
    Object,
    #[allow(dead_code)]
    Unit(String),
    Unknown, // Complex or undetermined type
}

/// Recursively checks if a quantity node contains a double literal
fn is_quantity_double(node: tree_sitter::Node) -> bool {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        let k = child.kind();
        if k == "real_literal" || k == "_realconstant_ext" {
            return true;
        }
        if k == "quantity" && is_quantity_double(child) {
            return true;
        }
    }
    false
}

impl NedType {
    pub fn as_str(&self) -> &str {
        match self {
            NedType::Bool => "bool",
            NedType::Int => "int",
            NedType::Double => "double",
            NedType::String => "string",
            NedType::Xml => "xml",
            NedType::Object => "object",
            NedType::Unit(_) => "unit",
            NedType::Unknown => "unknown",
        }
    }

    pub fn from_str(type_str: &str) -> Self {
        match type_str {
            "bool" => NedType::Bool,
            "int" => NedType::Int,
            "double" => NedType::Double,
            "string" => NedType::String,
            "xml" => NedType::Xml,
            "object" => NedType::Object,
            _ => NedType::Unknown,
        }
    }

    pub fn infer_type_from_expression(
        db: &dyn crate::db::Db,
        node: tree_sitter::Node,
        rope: &ropey::Rope,
    ) -> NedType {
        let kind = node.kind();

        match kind {
            // Literals
            "true" | "false" => NedType::Bool,
            "int_literal" => NedType::Int,
            "real_literal" => NedType::Double,
            "string_literal" => NedType::String,
            "xml_literal" => NedType::Xml,

            "value" | "paramvalue" => {
                if let Some(inner_expr) = node.child_by_field_name("expr") {
                    return NedType::infer_type_from_expression(db, inner_expr, rope);
                }

                NedType::Unknown
            }

            "parenthesized_expression" | "expression" => {
                let mut cursor = node.walk();
                for child in node.children(&mut cursor) {
                    let k = child.kind();
                    if k == "(" || k == ")" {
                        continue;
                    }
                    let t = NedType::infer_type_from_expression(db, child, rope);
                    if t != NedType::Unknown {
                        return t;
                    }
                }
                NedType::Unknown
            }

            k if k.contains("quantity") => {
                if is_quantity_double(node) {
                    NedType::Double
                } else {
                    // If its just 6Mbps (int + unit), treat it as Int
                    NedType::Int
                }
            }

            // Function Calls (int(), string())
            "functioncall" => {
                if let Some(name_word) = child_field_name(db, "name", node, rope) {
                    return match name_word.word.text(db).as_str() {
                        "int" => NedType::Int,
                        "double" => NedType::Double,
                        "string" => NedType::String,
                        "bool" => NedType::Bool,
                        "xml" | "xmldoc" => NedType::Xml,
                        _ => NedType::Unknown,
                    };
                }
                NedType::Unknown
            }

            _ => NedType::Unknown,
        }
    }

    /// Infer the type of an ini value, returns NedType
    pub fn infer_type_from_str(val: &str) -> Option<NedType> {
        let val = val.trim();

        if val.is_empty() || val == "default" {
            return None;
        }
        if val == "true" || val == "false" {
            return Some(NedType::Bool);
        }
        if val.starts_with('"') && val.ends_with('"') {
            return Some(NedType::String);
        }
        if val.parse::<i64>().is_ok() {
            return Some(NedType::Int);
        }
        if val.parse::<f64>().is_ok() {
            return Some(NedType::Double);
        }

        // Value with unit suffix (e.g. 100m, 1s, 500ms, 80bit)
        let numeric_end = val
            .find(|c: char| {
                !c.is_ascii_digit() && c != '.' && c != '-' && c != '+' && c != 'e' && c != 'E'
            })
            .unwrap_or(val.len());
        if numeric_end > 0 {
            let num_part = &val[..numeric_end];
            let unit_part = val[numeric_end..].trim();
            if !unit_part.is_empty() && unit_part.chars().all(|c| c.is_ascii_alphabetic()) {
                if num_part.parse::<i64>().is_ok() {
                    return Some(NedType::Int);
                }
                if num_part.parse::<f64>().is_ok() {
                    return Some(NedType::Double);
                }
            }
        }

        if val.contains('(') {
            return None;
        }

        None
    }

    pub fn accepts(&self, other: &NedType) -> bool {
        match (self, other) {
            // Exact match
            (a, b) if a == b => true,

            // Int into Double
            (NedType::Double, NedType::Int) => true,

            // Object can accept anything
            (NedType::Object, _) => true,

            // INFO: If we do not know the type, accept anything → I do not want to highlight too many
            // errors (todo)
            (_, NedType::Unknown) => true,

            // Mismatch
            _ => false,
        }
    }
}
