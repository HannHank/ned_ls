use crate::hir::ini::section_hir::IniSettingHir;

use super::*;

#[derive(Debug, Clone, PartialEq, Eq, Hash, salsa::SalsaValue)]
pub struct IniAssignmentHir<'db> {
    pub key: Word<'db>,
    pub value: Word<'db>,
    pub span: Span,
    pub key_span: Span,
    pub value_span: Span,
}

pub fn build_setting_hir<'db>(
    db: &'db dyn Db,
    setting_node: tree_sitter::Node,
    rope: &Rope,
    version: i32,
) -> Option<IniSettingHir<'db>> {
    let mut cursor = setting_node.walk();
    for child in setting_node.children(&mut cursor) {
        if child.is_error() {
            Diagnostic {
                start: child.start_byte(),
                end: child.end_byte(),
                message: "Syntax error in assignment".to_string(),
                version,
                severity: crate::hir::Severity::Error,
                url: None,
            }
            .accumulate(db);
            return None;
        }
    }

    let key_node = setting_node.child_by_field_name("name")?;
    let value_node = setting_node.child_by_field_name("value")?;

    let key_text = text_for_node(&key_node, rope)
        .unwrap_or_default()
        .trim()
        .to_string();
    let value_text = text_for_node(&value_node, rope)
        .unwrap_or_default()
        .trim()
        .to_string();

    let assignment = IniAssignmentHir {
        key: Word::new(db, key_text),
        value: Word::new(db, value_text),
        span: Span::new(setting_node.start_byte(), setting_node.end_byte()),
        key_span: Span::new(key_node.start_byte(), key_node.end_byte()),
        value_span: Span::new(value_node.start_byte(), value_node.end_byte()),
    };

    if key_node.kind() == "keyword_name" {
        Some(IniSettingHir::Config(assignment))
    } else {
        Some(IniSettingHir::Parameter(assignment))
    }
}
