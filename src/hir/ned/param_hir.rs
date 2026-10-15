use ropey::Rope;
use salsa::Accumulator;

use crate::{
    db::Db,
    hir::{self, Diagnostic, Span, Word, ned::type_inference::NedType, tree_utils::extract_field},
};

/// An item inside a `parameters:` block.
#[derive(Debug, Clone, PartialEq, Eq, Hash, salsa::SalsaValue)]
pub enum ParamBlockItem<'db> {
    Param(ParamHir<'db>),
    Property(PropertyHir<'db>),
}

impl<'db> ParamBlockItem<'db> {
    #[allow(dead_code)]
    pub fn name(&self, db: &'db dyn crate::db::Db) -> Word<'db> {
        match self {
            ParamBlockItem::Param(param) => *param.name(db),
            ParamBlockItem::Property(property) => *property.name(db),
        }
    }

    pub fn span(&self, db: &'db dyn crate::db::Db) -> Span {
        match self {
            ParamBlockItem::Param(param) => *param.span(db),
            ParamBlockItem::Property(property) => *property.span(db),
        }
    }
}
#[salsa::interned(debug)]
pub struct ParamSegment<'db> {
    pub name: Word<'db>,
    pub index: Option<Word<'db>>,
    pub is_path: bool,
}

/// Represents: `int address;`
#[salsa::tracked(debug)]
pub struct ParamHir<'db> {
    pub name: Word<'db>,
    pub pattern: bool,
    pub segments: Vec<ParamSegment<'db>>,
    pub target_param: Option<Word<'db>>,
    pub vectorized: bool, // Important for submodules
    pub ned_type: Option<NedType>,
    pub inferred_type: Option<NedType>,
    pub value: Option<Word<'db>>,
    pub span: Span,
}

/// Represents: `@display("i=misc/node_vs,gold");`
#[salsa::tracked(debug)]
pub struct PropertyHir<'db> {
    pub name: Word<'db>,       // "display"
    pub value_list: Word<'db>, // "i=misc/node_vs,gold"
    pub span: Span,
}

pub fn build_parameters_block<'db>(
    db: &'db dyn Db,
    node: &tree_sitter::Node,
    rope: &Rope,
    version: i32, // TODO: remove me later
) -> Option<Vec<ParamBlockItem<'db>>> {
    let mut params: Vec<ParamBlockItem> = Vec::new();

    let cursor = &mut node.walk();

    // go into parameter block
    for child in node.children(cursor) {
        if child.is_missing() {
            Diagnostic {
                severity: crate::hir::Severity::Error,
                url: None,
                start: child.range().start_byte,
                end: child.range().end_byte,
                message: format!("Syntax error: expected '{}'", child.kind()),
                version,
            }
            .accumulate(db);
            continue;
        }

        match child.kind() {
            "property" => {
                let name_node = child.child_by_field_name("name").or_else(|| {
                    child
                        .child_by_field_name("property_signature")
                        .and_then(|sig| sig.child_by_field_name("name").or(Some(sig)))
                });

                let name_node = match name_node {
                    Some(n) => n,
                    None => continue,
                };

                let name_word_content = extract_field(db, &name_node, rope);

                let value_word_content = if let Some(tag) = child.child_by_field_name("tag") {
                    if let Some(value_list) = tag.child_by_field_name("value_list") {
                        extract_field(db, &value_list, rope)
                    } else {
                        None
                    }
                } else {
                    None
                };

                if let Some(name_word) = name_word_content {
                    let value_text = value_word_content
                        .map(|c| c.word)
                        .unwrap_or(hir::Word::new(db, ""));
                    let end_byte = child.end_byte();

                    let param_item = PropertyHir::new(
                        db,
                        name_word.word,
                        value_text,
                        Span::new(name_node.start_byte(), end_byte),
                    );
                    params.push(ParamBlockItem::Property(param_item));
                }
            }

            "parameter" => {
                let mut is_pattern = false;

                let name_node = if let Some(n) =
                    child.child_by_field_name("parameter_signature_name")
                {
                    Some(n)
                } else if let Some(p) = child.child_by_field_name("parameter_signature_pattern") {
                    is_pattern = true;
                    Some(p)
                } else {
                    child.child_by_field_name("name")
                };

                if let Some(name_node) = name_node
                    && let Some(name_word) = extract_field(db, &name_node, rope)
                {
                    let mut segments = Vec::new();
                    let mut target_param = None;

                    if is_pattern {
                        let mut pattern_cursor = name_node.walk();

                        for path_elem in
                            name_node.children_by_field_name("path", &mut pattern_cursor)
                        {
                            if path_elem.kind() == "pattern_elem" {
                                let elem_name = path_elem
                                    .child_by_field_name("name")
                                    .and_then(|n| extract_field(db, &n, rope))
                                    .map(|w| w.word)
                                    .unwrap_or_else(|| Word::new(db, ""));
                                let elem_index = path_elem
                                    .child_by_field_name("index")
                                    .and_then(|n| extract_field(db, &n, rope))
                                    .map(|w| w.word);

                                segments.push(ParamSegment::new(db, elem_name, elem_index, true));
                            }
                        }

                        // Read the final parameter field separately
                        if let Some(param_node) = name_node.child_by_field_name("parameter") {
                            target_param = match param_node.kind() {
                                "typename" => Some(Word::new(db, "typename")),
                                "pattern_elem" | "pattern_name" => {
                                    param_node
                                        .child_by_field_name("name")
                                        .and_then(|n| extract_field(db, &n, rope))
                                        .map(|w| w.word)
                                        .or_else(|| {
                                            // pattern_elem with no name child → the node itself is the name
                                            extract_field(db, &param_node, rope).map(|w| w.word)
                                        })
                                }
                                _ => extract_field(db, &param_node, rope).map(|w| w.word),
                            };
                        }
                    }
                    let type_node = child
                        .child_by_field_name("type")
                        .or_else(|| child.child_by_field_name("typename"));
                    let value_node = child.child_by_field_name("value");

                    let type_word = type_node
                        .and_then(|n| extract_field(db, &n, rope))
                        .map(|w| w.word);

                    let value_word = value_node
                        .and_then(|n| extract_field(db, &n, rope))
                        .map(|w| w.word);

                    // Compute Semantic Types
                    let declared_type = type_word.as_ref().map(|w| NedType::from_str(w.text(db)));

                    // Inferred Type
                    let inferred_type =
                        value_node.map(|n| NedType::infer_type_from_expression(db, n, rope));

                    let vectorized = segments.first().is_some_and(|s| s.index(db).is_some());

                    // INFO: Maybe move this to the semantics
                    // Check for type mismatches and emit diagnostics
                    if let (Some(declared), Some(inferred)) = (&declared_type, &inferred_type)
                        && !declared.accepts(inferred)
                    {
                        Diagnostic {severity: crate::hir::Severity::Error,

                                start: child.start_byte(),
                                end: child.end_byte(),
                                message: format!(
                                    "Type mismatch: declared type '{}' does not match inferred type '{}'.",
                                    declared.as_str(),
                                    inferred.as_str()
                                ),
                                version,
                                url: None,
                            }
                            .accumulate(db);
                    }

                    params.push(ParamBlockItem::Param(ParamHir::new(
                        db,
                        name_word.word,
                        is_pattern,
                        segments,
                        target_param,
                        vectorized,
                        declared_type,
                        inferred_type,
                        value_word,
                        Span::new(name_node.start_byte(), child.end_byte()),
                    )));
                }
            }
            "ERROR" => {
                Diagnostic {
                    start: child.start_byte(),
                    end: child.end_byte(),
                    message: "unexpected character ~ Module level".to_string(),
                    version,
                    severity: crate::hir::Severity::Error,
                    url: None,
                }
                .accumulate(db);
            }
            _ => {
                // INFO: do not throw an error here, because parameters is again included as a
                // child marking the text range of the parameters keyword
                // eprintln!(
                //     "Skipping unrecognized child in parameters block: kind={}",
                //     child.kind()
                // );
            }
        }
    }

    Some(params)
}

#[cfg(test)]
mod tests {

    #[test]
    fn test_ast_dump_param() {
        let code = "
        module Test {
            parameters:
                queue.typename = default(\"DropTailQueue\");
                wlan[*].protocol = default(1);
        }
        ";
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&tree_sitter_ned::language()).unwrap();
        let tree = parser.parse(code, None).unwrap();

        // Find parameter node
        let mut cursor = tree.walk();
        for child in tree.root_node().children(&mut cursor) {
            if child.kind() == "module"
                && let Some(params) = child.child_by_field_name("parameters")
            {
                for param in params.children(&mut tree.walk()) {
                    if param.kind() == "parameter"
                        && let Some(pattern) =
                            param.child_by_field_name("parameter_signature_pattern")
                    {
                        println!("PATTERN: {}", pattern.to_sexp());
                        let mut walk = pattern.walk();
                        for elem in pattern.children(&mut walk) {
                            println!("- CHILD: {}, named: {}", elem.kind(), elem.is_named());
                        }
                    }
                }
            }
        }
        // panic!("Show output");
    }
}
