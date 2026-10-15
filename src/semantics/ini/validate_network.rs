use super::prelude::*;
use crate::db::{Db, DefIndex, DefKind, resolve_import};
use crate::hir::ini::IniSectionHir;
use crate::hir::{self, Severity, SourceFile, Span, Word, ned::import_hir::ImportPattern};
use crate::semantics::ned::determine_expected_package;

/// Recursively find the network word for a section → following extends and General.
pub fn resolve_network_word<'db>(
    db: &'db dyn Db,
    ini_file: SourceFile,
    section: IniSectionHir<'db>,
) -> Option<(Word<'db>, Span)> {
    let raw_sec_name = section.name(db).text(db);
    let current_section_name = raw_sec_name
        .strip_prefix("Config ")
        .unwrap_or(raw_sec_name)
        .trim()
        .to_string();

    let section_chain = super::resolve_section_chain(db, ini_file, current_section_name);

    // search from current config up to General
    for chain_sec in section_chain.iter().rev() {
        if let Some(net_kv) = chain_sec
            .configs(db)
            .iter()
            .find(|kv| kv.key.text(db) == "network")
        {
            return Some((net_kv.value, net_kv.value_span));
        }
    }

    None
}

/// Validate that the network name actually exists in the project and is a Network.
pub fn validate_network_def<'a, 'db>(
    db: &'db dyn Db,
    ctx: &mut ValidationCtx<'a, 'db>,
    ini_file: SourceFile,
    network_name: Word<'db>,
    network_span: Span,
    emit_errors: bool,
) -> Option<DefIndex<'db>> {
    let network_name_str = network_name.text(db);
    let uri = ini_file.uri(db);

    // get expected package name based on the current ini file location
    let Ok(file_path) = uri.to_file_path() else {
        return None;
    };

    let file_dir = file_path.parent().unwrap_or(&file_path);
    let interned_file_dir = hir::shared::FilePath::new(db, file_dir.to_path_buf());

    let resolved_pkg = determine_expected_package(db, interned_file_dir); // tracked
    let expected_pkg_name = match resolved_pkg {
        hir::ned::package_hir::ResolvedPackage::FromPath(pkg) => pkg.name(db).as_str(),
        hir::ned::package_hir::ResolvedPackage::FromName(pkg) => pkg.name(db).as_str(),
    };

    // opp allows unqualified names if they are in the same package, or fqns.
    let is_fqn = network_name_str.contains('.');
    let search_pattern = if is_fqn || expected_pkg_name.is_empty() {
        network_name_str.to_string()
    } else {
        format!("{}.{}", expected_pkg_name, network_name_str)
    };

    let pattern = ImportPattern::new(db, search_pattern.clone());

    // network exists in the package tree?
    let network_def = match resolve_import(db, pattern).as_deref() {
        // tracked
        None | Some([]) => {
            if emit_errors {
                ctx.push(
                    network_span.start,
                    network_span.end,
                    format!("Network `{}` not found in the project.", search_pattern),
                    Severity::Error,
                );
            }
            return None;
        }
        // multiple matches for the same name
        Some([_, _, ..]) => {
            if emit_errors {
                ctx.push(
                    network_span.start,
                    network_span.end,
                    format!(
                        "Import pattern `{}` is ambiguous and matches multiple definitions.",
                        search_pattern
                    ),
                    Severity::Error,
                );
            }
            return None;
        }
        Some([def]) => *def,
    };

    if *network_def.kind(db) != DefKind::Network && emit_errors {
        ctx.push(
            network_span.start,
            network_span.end,
            format!(
                "Definition `{}` found but is not a network.",
                search_pattern
            ),
            Severity::Error,
        );
        return None;
    }

    Some(network_def)
}
