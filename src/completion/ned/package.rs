use crate::hir;
use crate::hir::ned::package_hir::ResolvedPackage;
use crate::semantics;

pub fn resolve_package_name(engine: &super::CompletionEngine) -> String {
    let file_path = match engine.uri.to_file_path() {
        Ok(p) => p,
        Err(_) => {
            log::warn!("Unable to convert URI to file path: {}", engine.uri);
            return "default.package".to_string();
        }
    };

    let file_dir = file_path.parent().unwrap_or(&file_path);

    let pkg = semantics::ned::determine_expected_package(
        engine.db,
        hir::shared::FilePath::new(engine.db, file_dir),
    );

    match pkg {
        ResolvedPackage::FromPath(name) | ResolvedPackage::FromName(name) => {
            name.name(engine.db).to_string()
        }
    }
}
