use crate::db;

pub fn suggest_imports(db: &dyn db::Db, unfinished_import: &str) -> Vec<String> {
    let tree = db::workspace_package_tree(db);
    let indexes = db::workspace_indexes(db);

    // Custom logic to suggest imports based on the unfinished import string and the package tree
    // and indexes. This is much faster than doing a full search through all declarations.
    let resolved = tree.suggest_completions(db, unfinished_import, indexes);

    resolved
        .into_iter()
        .map(|decl| decl.fqn(db).text(db).clone())
        .collect()
}
