pub mod assignment_hir;
pub mod section_hir;
use crate::db::Db;
use crate::hir::SourceFile;
use crate::hir::ini::section_hir::IniSettingHir;
use crate::hir::shared::{Diagnostic, Severity, Span, Word};
use crate::hir::tree_utils::*;
use ropey::Rope;
use salsa::Accumulator;
pub use section_hir::IniSectionHir;
use tower_lsp::lsp_types::Url;

#[salsa::tracked(debug)]
pub struct IniFileHir<'db> {
    #[returns(ref)]
    pub uri: Url,

    #[returns(ref)]
    pub sections: Vec<section_hir::IniSectionHir<'db>>,
}

#[salsa::tracked]
pub fn lower_to_hir<'db>(db: &'db dyn Db, file: SourceFile) -> IniFileHir<'db> {
    let uri = file.uri(db).clone();
    let rope = file.rope(db);
    let version = file.version(db);

    let tree = match db.get_ast_for_file(&file) {
        Some(t) => t,
        None => return IniFileHir::new(db, uri, vec![]),
    };

    let mut sections = Vec::new();

    let mut current_builder: Option<section_hir::SectionBuilder<'db>> = None;

    let root = tree.root_node();
    let mut cursor = root.walk();

    for node in root.children(&mut cursor) {
        match node.kind() {
            "section_heading" => {
                if let Some(builder) = current_builder.take() {
                    let completed_section = section_hir::IniSectionHir::new(
                        db,
                        builder.name,
                        builder.span,
                        builder.header_span,
                        builder.configs,
                        builder.assignments,
                    );
                    sections.push(completed_section);
                }

                // Start building a new section
                current_builder = section_hir::parse_section_header(db, node, rope, *version);
            }
            "setting" => {
                if let Some(setting_hir) =
                    assignment_hir::build_setting_hir(db, node, rope, *version)
                {
                    if let Some(builder) = &mut current_builder {
                        match setting_hir {
                            IniSettingHir::Config(cfg) => builder.configs.push(cfg),
                            IniSettingHir::Parameter(param) => builder.assignments.push(param),
                        }
                        // Expand the overall section span to include this setting
                        builder.span.end = node.end_byte();
                    } else {
                        Diagnostic {
                            start: node.start_byte(),
                            end: node.end_byte(),
                            message:
                                "Global assignments are not allowed. Expected a [Section] first."
                                    .to_string(),
                            version: *version,
                            severity: Severity::Error,
                            url: Some(uri.clone()),
                        }
                        .accumulate(db);
                    }
                }
            }
            "ERROR" => {
                Diagnostic {
                    start: node.start_byte(),
                    end: node.end_byte(),
                    message: "Syntax error in INI file".to_string(),
                    version: *version,
                    severity: Severity::Error,
                    url: Some(uri.clone()),
                }
                .accumulate(db);
            }
            _ => {}
        }
    }

    // Push the very last section into the database when the file ends
    if let Some(builder) = current_builder.take() {
        let last_section = section_hir::IniSectionHir::new(
            db,
            builder.name,
            builder.span,
            builder.header_span,
            builder.configs,
            builder.assignments,
        );
        sections.push(last_section);
    }

    IniFileHir::new(db, uri, sections)
}
