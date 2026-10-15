mod circle_detection;
mod imports;
mod package;
mod resolve_interfaces;
mod validate_connections;
mod validate_extends;
mod validate_implements;
mod validate_inheritance;
mod validate_interfaces;
mod validate_item;
pub mod validate_parameters;
mod validate_submodules;

// pub mods
pub mod prelude;
pub mod shared;
pub use imports::validate_imports;
pub use package::determine_expected_package;
pub use package::validate_package;

use crate::db::visible_definitions;
use crate::hir;
use crate::semantics::ned::circle_detection::detect_structural_cycles;

#[salsa::tracked]
pub fn semantic_analysis(
    db: &dyn crate::db::Db,
    ned_file: hir::SourceFile,
) -> Vec<hir::Diagnostic> {
    let mut diagnostics = Vec::new();
    let hir = crate::hir::ned::lower_to_hir(db, ned_file);
    let visible_defs = visible_definitions(db, ned_file);

    let diag_opt = validate_package(db, ned_file, *hir);
    if let Some(diag) = diag_opt {
        diagnostics.push(diag.clone());
    }

    let import_diag = validate_imports(db, ned_file, *hir);
    diagnostics.extend(import_diag.iter().cloned());

    for item in hir.items(db).iter() {
        if let Some(cycle_diag) = detect_structural_cycles(db, ned_file, item, visible_defs) {
            diagnostics.push(cycle_diag);
            continue; // stop further validation
        }
        let item_diagnostics =
            validate_item::validate_item(db, ned_file, visible_defs.clone(), *item);
        diagnostics.extend(item_diagnostics.iter().cloned());
    }

    diagnostics
}
