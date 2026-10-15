use crate::db;
use crate::hir;

/// Validate all imports
/// Return diagnostics and all explicit and implicit visible definitions
#[salsa::tracked]
pub fn validate_imports<'db>(
    db: &'db dyn crate::db::Db,
    ned_file: hir::SourceFile,
    hir: hir::NedFileHir<'db>,
) -> Vec<hir::Diagnostic> {
    let mut diagnostics = Vec::new();

    for import in hir.imports(db).iter() {
        let pattern = hir::ned::import_hir::ImportPattern::new(db, import.value.text(db).clone());

        // We only care if it FAILS
        if db::resolve_import(db, pattern).is_none() {
            let message = format!(
                "Import pattern '{}' does not match any package declarations in the workspace.",
                import.value.text(db)
            );

            diagnostics.push(hir::Diagnostic::new(
                import.span.start,
                import.span.end,
                message,
                *ned_file.version(db),
                crate::hir::Severity::Error,
                None,
            ));
        }
    }

    diagnostics
}
