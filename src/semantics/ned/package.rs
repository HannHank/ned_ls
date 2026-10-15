use crate::hir;

use crate::hir::ned::package_hir::{PackageHir, PackageName, ResolvedPackage};

use std::path::Path;
use tower_lsp::lsp_types::Url;

/// Checks if the package declaration matches the file structure.
#[salsa::tracked]
pub fn validate_package<'db>(
    db: &'db dyn crate::db::Db,
    ned_file: hir::SourceFile,
    hir: hir::NedFileHir<'db>,
) -> Option<hir::Diagnostic> {
    // eprintln!("Validating package for {:?}", ned_file.uri(db)); // TODO: remove me later

    let declared = hir.package(db);

    let uri = hir.uri(db);

    let Ok(file_path) = uri.to_file_path() else {
        return None;
    };

    let file_dir = file_path.parent().unwrap_or(&file_path);

    let interned_file_dir = hir::shared::FilePath::new(db, file_dir);

    match determine_expected_package(db, interned_file_dir) {
        ResolvedPackage::FromPath(pkg) => {
            db.register_for_workspace_updates();

            // check if current file is ned root package.ned
            let root_ned = pkg.root(db);
            let root_package_ned = Path::new(&root_ned).join("package.ned");
            if file_path == root_package_ned {
                None // Marking the root package → we do not care what is written there
            } else {
                emit_diagnostic_if_mismatch(db, declared, pkg.name(db).as_str(), &ned_file)
            }
        }
        ResolvedPackage::FromName(pkg) => {
            emit_diagnostic_if_mismatch(db, declared, pkg.name(db).as_str(), &ned_file)
        }
    }
}

/// Determines the expected package name based on the file directory and project configuration.
///
/// This functions is tracked in order to reduce redundant computation for files in the same
/// directory. It would be cool to turn this into an recursive query because this would completely
/// eliminate the loop over ancestors but salsa has some problems which I could not yet resolve.
#[salsa::tracked]
pub fn determine_expected_package<'db>(
    db: &'db dyn crate::db::Db,
    file_dir: hir::shared::FilePath<'db>,
) -> hir::ned::package_hir::ResolvedPackage<'db> {
    // The nedfolder root below comes from `db.get_config()`, which is a plain field salsa cannot
    // track.
    db.register_for_workspace_updates(); // TODO: profile this → required because code action to add
    // missing project/file need to re-run this query to get the new expected package name

    // From the omnetpp documentation
    // package.ned files are allowed in other folders as well. They may contain properties and/or documentation for their package, but cannot be used to define the package they are in.
    resolve_expected_package_name(
        db,
        file_dir.path(db),
        &get_nedfolder_root_from_config(db, file_dir),
    )
}

// Resolves the expected package name for a given file directory and project root
pub fn resolve_expected_package_name<'db>(
    db: &'db dyn crate::db::Db,
    file_dir: &Path,
    ned_root: &Path,
) -> ResolvedPackage<'db> {
    // Check if root contains package.ned
    let root_pkg_file = ned_root.join("package.ned");
    let root_pkg_url = Url::from_file_path(&root_pkg_file).ok().unwrap(); // TODO: handle error
    let root_ned_file = db.get_source_file_input(&root_pkg_url);
    if let Some(root_ned_file) = root_ned_file {
        // Then use it as a prefix and build dotted path
        if let Some(extracted_name) = extract_package_name(
            db,
            root_ned_file,
            root_pkg_file.to_str().unwrap().to_string(),
        ) {
            return ResolvedPackage::FromName(PackageName::new(
                db,
                build_dotted_path(file_dir, ned_root, extracted_name.name(db).as_str()),
                ned_root.to_str().unwrap().to_string(),
            ));
        }
    } else {
        // no package.ned in nedfolder root → just build dotted path from root
        return ResolvedPackage::FromPath(PackageName::new(
            db,
            build_dotted_path(file_dir, ned_root, ""),
            ned_root.to_str().unwrap().to_string(),
        ));
    }

    panic!(
        "Could not resolve expected package name for file dir {:?}",
        file_dir
    );
}

/// Extracts the package declared inside a package.ned file.
///
/// This tracked function is import to rerun the extraction when the ned file changes or is deleted
#[salsa::tracked]
fn extract_package_name<'db>(
    db: &'db dyn crate::db::Db,
    ned_file: hir::SourceFile,
    ned_root: String,
) -> Option<PackageName<'db>> {
    let root_hir = hir::ned::lower_to_hir(db, ned_file); // lower to HIR
    root_hir
        .package(db)
        .as_ref()
        .map(|pkg_hir| pkg_hir.path.text(db).to_string())
        .map(|name| PackageName::new(db, name, ned_root))
}

/// Build `<prefix>.<relative path>` where prefix = project or package.ned package.
pub fn build_dotted_path(file_dir: &Path, base_dir: &Path, prefix: &str) -> String {
    let rel = file_dir
        .strip_prefix(base_dir)
        .ok()
        .and_then(|p| p.to_str())
        .unwrap_or("")
        .replace(std::path::MAIN_SEPARATOR, ".")
        .trim_matches('.')
        .to_string();

    // Handle empty cases to avoid leading dots
    if prefix.is_empty() {
        return rel;
    }

    if rel.is_empty() {
        prefix.to_string()
    } else {
        format!("{prefix}.{rel}")
    }
}

/// Emit a diagnostic when the declared package does not match expected one.
fn emit_diagnostic_if_mismatch(
    db: &dyn crate::db::Db,
    declared: &Option<PackageHir>,
    expected: &str,
    ned_file: &hir::SourceFile,
) -> Option<hir::Diagnostic> {
    match declared {
        Some(pkg) => {
            let actual = pkg.path.text(db);
            if actual.as_str() != expected {
                return Some(hir::Diagnostic {
                    severity: crate::hir::Severity::Error,
                    start: pkg.span.start,
                    end: pkg.span.end,
                    message: format!("Package mismatch. Expected package: `{expected}`."),
                    version: *ned_file.version(db),
                    url: None,
                });
            }
            None
        }
        None => {
            let (message, severity) =
                if expected.is_empty() {
                    ("Missing package declaration. It is highly recommended to define a package."
                    .to_string(), crate::hir::Severity::Warning)
                } else {
                    (
                        format!("Missing package declaration. Expected: `{expected}`."),
                        crate::hir::Severity::Error,
                    )
                };
            Some(hir::Diagnostic {
                severity,
                start: 0,
                end: 0,
                message,
                version: *ned_file.version(db),
                url: None,
            })
        }
    }
}

// TODO: return option
/// Get the nedfolder root path from the configuration for a given file directory.
fn get_nedfolder_root_from_config<'db>(
    db: &'db dyn crate::db::Db,
    file_dir: hir::shared::FilePath<'db>,
) -> std::path::PathBuf {
    let config = db.get_config();
    if let Some(config) = config {
        let path = file_dir.path(db);

        crate::workspace_membership::nedfolder_root_for(path, &config.nedfolders)
            .cloned()
            .unwrap_or_else(|| path.clone())
    } else {
        file_dir.path(db).clone()
    }
}
