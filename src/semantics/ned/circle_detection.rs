use crate::db::Db;
use crate::hir::{self, Diagnostic, Severity};
use std::collections::HashSet;

// INFO: I originally wanted to integrate this into the anchestor resolver but it got too
// complicated and with this structure I can report report the cycle more precisely and abort the
// validation early
pub fn detect_structural_cycles<'db>(
    db: &'db dyn Db,
    ned_file: hir::SourceFile,
    start_item: &hir::ItemHir<'db>,
    visible_defs: &[crate::db::DefIndex<'db>],
) -> Option<Diagnostic> {
    let mut on_stack = HashSet::new();
    let mut path = Vec::new();
    let uri = ned_file.uri(db);
    let version = ned_file.version(db);

    // just dfs visited list
    let mut safe_modules = HashSet::new();

    fn dfs<'db>(
        db: &'db dyn Db,
        current_name: String,
        visible_defs: &[crate::db::DefIndex<'db>],
        on_stack: &mut HashSet<String>,
        safe_modules: &mut HashSet<String>,
        path: &mut Vec<String>,
    ) -> Option<String> {
        if on_stack.contains(&current_name) {
            path.push(current_name.clone());
            return Some(path.join(" -> ")); // test → bla → test
        }

        if safe_modules.contains(&current_name) {
            return None; // do not need to check this branch again 
        }

        on_stack.insert(current_name.clone());
        path.push(current_name.clone());

        if let Some(def) = crate::db::find_def(db, &current_name, visible_defs)
            && let Some(hir) = crate::context::resolve_item_hir(db, def)
        {
            // check inheritance
            for ext in hir.item.extends(db) {
                let ext_name = ext.name.text(db).to_string();
                if let Some(cycle) = dfs(db, ext_name, visible_defs, on_stack, safe_modules, path) {
                    return Some(cycle);
                }
            }

            // check submodules
            for sub in hir.item.submodules(db) {
                if let hir::ned::submodules_hir::SubmoduleDefinition::Concrete {
                    type_name, ..
                } = &sub.definition
                {
                    let sub_name = type_name.to_string();
                    if let Some(cycle) =
                        dfs(db, sub_name, visible_defs, on_stack, safe_modules, path)
                    {
                        return Some(cycle);
                    }
                }
            }
        }

        path.pop();
        on_stack.remove(&current_name);
        safe_modules.insert(current_name); // TODO: 
        None
    }

    let start_name = start_item.name(db).text(db).to_string();

    if let Some(cycle_path) = dfs(
        db,
        start_name,
        visible_defs,
        &mut on_stack,
        &mut safe_modules,
        &mut path,
    ) {
        let span = start_item.span(db);
        return Some(Diagnostic {
            start: span.start,
            end: span.end,
            message: format!("Cyclic dependency detected: {}", cycle_path),
            version: *version,
            severity: Severity::Error,
            url: Some(uri.clone()),
        });
    }

    None
}
