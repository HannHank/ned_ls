use crate::hir::ini::assignment_hir::IniAssignmentHir;

use super::*;

pub enum IniSettingHir<'db> {
    Config(IniAssignmentHir<'db>),
    Parameter(IniAssignmentHir<'db>),
}

#[salsa::tracked(debug)]
pub struct IniSectionHir<'db> {
    pub name: Word<'db>,
    pub span: Span,
    pub header_span: Span,
    pub configs: Vec<IniAssignmentHir<'db>>, // network, extends, sim-time-limit, etc.
    pub assignments: Vec<IniAssignmentHir<'db>>, // **.node.port = 80
}

pub struct SectionBuilder<'db> {
    pub name: Word<'db>,
    pub span: Span,
    pub header_span: Span,
    pub configs: Vec<IniAssignmentHir<'db>>,
    pub assignments: Vec<IniAssignmentHir<'db>>,
}

pub fn parse_section_header<'db>(
    db: &'db dyn Db,
    heading_node: tree_sitter::Node,
    rope: &Rope,
    version: i32,
) -> Option<SectionBuilder<'db>> {
    let mut cursor = heading_node.walk();

    let mut name = None;
    let header_span = Span::new(heading_node.start_byte(), heading_node.end_byte());

    for child in heading_node.children(&mut cursor) {
        match child.kind() {
            "General" => {
                name = Some(Word::new(db, "General".to_string()));
                break;
            }
            "section_word" => {
                let text = text_for_node(&child, rope)
                    .unwrap_or_default()
                    .trim()
                    .to_string();

                name = Some(Word::new(db, text));
                break;
            }
            "ERROR" => {
                Diagnostic {
                    start: child.start_byte(),
                    end: child.end_byte(),
                    message: "Unexpected character or space in section heading".to_string(),
                    version,
                    severity: crate::hir::Severity::Error,
                    url: None,
                }
                .accumulate(db);
            }
            _ => {}
        }
    }

    name.map(|n| SectionBuilder {
        name: n,
        span: header_span,
        header_span,
        configs: Vec::new(),
        assignments: Vec::new(),
    })
}
