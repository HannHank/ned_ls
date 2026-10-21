pub mod package_tree;
pub use package_tree::*;

use crate::{
    Config,
    hir::{self, ned::import_hir::ImportPattern, shared::FilePathTest},
};
use dashmap::DashMap;
use salsa::Setter;

#[allow(unused_imports)]
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};
use tower_lsp::lsp_types::Url;
use tree_sitter::Tree;

#[salsa::db]
#[derive(Clone)]
#[cfg_attr(not(test), derive(Default))]
pub struct RootLspDatabase {
    storage: salsa::Storage<Self>,

    config: Option<Config>,

    source_input: Arc<DashMap<Url, hir::SourceFile>>,

    ast_map: Arc<DashMap<hir::SourceFile, Tree>>,

    workspace_structure: Option<hir::WorkspaceStructure>,

    workspace: Option<hir::WorkspaceState>,

    #[cfg(test)]
    logs: Arc<Mutex<Option<Vec<String>>>>,
}

impl std::fmt::Debug for RootLspDatabase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut d = f.debug_struct("RootLspDatabase");

        d.field("storage", &"<Salsa storage>");

        #[cfg(test)]
        d.field("logs", &self.logs);

        d.finish()
    }
}

#[cfg(test)]
impl Default for RootLspDatabase {
    fn default() -> Self {
        let logs = <Arc<Mutex<Option<Vec<String>>>>>::default();
        Self {
            storage: salsa::Storage::new(Some(Box::new({
                let logs = logs.clone();
                move |event| {
                    eprintln!("Event: {event:?}");
                    // Log interesting events, if logging is enabled
                    if let Some(logs) = &mut *logs.lock().unwrap()
                        && let salsa::EventKind::WillExecute { .. } = event.kind
                    {
                        logs.push(format!("Event: {event:?}"));
                    }
                }
            }))),
            config: None,
            source_input: Arc::new(DashMap::<tower_lsp::lsp_types::Url, hir::SourceFile>::new()),
            ast_map: Arc::new(DashMap::<hir::SourceFile, Tree>::new()),
            workspace_structure: None,
            workspace: None,
            logs,
        }
    }
}

impl RootLspDatabase {
    pub fn new() -> Self {
        // TODO: This is required for salsa::par_map but currently this features does not work well
        // for me
        let db = Self::default();
        let _ = <RootLspDatabase as salsa::Database>::zalsa_register_downcaster(&db);
        let _ = <RootLspDatabase as Db>::zalsa_register_downcaster(&db);
        db
    }

    #[cfg(test)]
    pub fn enable_logging(&self) {
        let mut logs = self.logs.lock().unwrap();
        if logs.is_none() {
            *logs = Some(vec![]);
        }
    }

    #[cfg(test)]
    #[allow(unused)]
    pub fn take_logs(&self) -> Vec<String> {
        let mut logs = self.logs.lock().unwrap();
        if let Some(logs) = &mut *logs {
            std::mem::take(logs)
        } else {
            vec![]
        }
    }
}

#[salsa::input]
struct File {
    path: PathBuf,
    #[returns(ref)]
    contents: String,
}
//
#[salsa::db]
pub trait Db: salsa::Database {
    //     fn input(&self, path: PathBuf) -> Result<File>;
    #[allow(dead_code)]
    fn input(&self) -> String;
    fn get_config(&self) -> Option<&Config>;
    fn set_config(&mut self, config: Option<Config>);

    // Get Stored NED File by URI
    fn get_source_file_input(&self, uri: &Url) -> Option<hir::SourceFile>;

    fn get_all_source_file_inputs(&self) -> Vec<hir::SourceFile>;

    /// true if at least one NED file from disk is indexed
    fn has_ned_file_on_disk(&self) -> bool;

    // Store NED File Input
    fn set_ned_file_input(&mut self, uri: Url, ned_file: hir::SourceFile);

    // Remove NED File Input
    fn remove_source_file_input(&mut self, uri: &Url);
    // Store the NED file in a cache to avoid re-parsing

    fn init_workspace_structure(&mut self);

    fn update_workspace_structure(&mut self);

    fn register_for_workspace_updates(&self) -> u32;

    // Ast related functions
    fn get_ast_for_file(&self, ned_file: &hir::SourceFile) -> Option<Tree>; // INFO: normally you would
    // compute this inside the lower_to_hir but I changed this to use the incremental parsing of
    // tree-sitter for autocompletion and error recovery

    fn set_ast_for_file(&mut self, ned_file: hir::SourceFile, tree: Tree);
}

#[salsa::db]
impl salsa::Database for RootLspDatabase {}

#[salsa::db]
impl Db for RootLspDatabase {
    fn input(&self) -> String {
        "test contents".to_string()
    }
    fn get_config(&self) -> Option<&Config> {
        self.config.as_ref()
    }

    fn set_config(&mut self, config: Option<Config>) {
        self.config = config;

        // to force a re-evaluation of all queries that depend on the config
        self.update_workspace_structure();
    }

    fn get_source_file_input(&self, uri: &Url) -> Option<hir::SourceFile> {
        self.source_input.get(uri).map(|entry| *entry.value())
    }

    fn get_all_source_file_inputs(&self) -> Vec<hir::SourceFile> {
        self.source_input
            .iter()
            .map(|entry| *entry.value())
            .collect()
    }

    fn has_ned_file_on_disk(&self) -> bool {
        self.source_input
            .iter()
            .any(|entry| entry.key().scheme() == "file" && entry.key().path().ends_with(".ned"))
    }

    fn set_ned_file_input(&mut self, uri: Url, ned_file: hir::SourceFile) {
        self.source_input.insert(uri.clone(), ned_file);
        if let Some(workspace) = self.workspace {
            let mut files = workspace.files(self).clone();
            files.insert(uri, ned_file);
            workspace.set_files(self).to(files);
        } else {
            let workspace = hir::WorkspaceState::new(self, {
                let mut map = std::collections::HashMap::new();
                map.insert(uri, ned_file);
                map
            });
            self.workspace = Some(workspace);
        }
    }

    fn remove_source_file_input(&mut self, uri: &Url) {
        self.source_input.remove(uri);
        // self.update_workspace_structure(); // Not necessary here, because existing files should
        // create a dependency and rerun automatically when file is remove (rope input changed)
        if let Some(workspace) = self.workspace {
            let mut files = workspace.files(self).clone();
            files.remove(uri);
            workspace.set_files(self).to(files);
        }
    }

    fn init_workspace_structure(&mut self) {
        // Ensure that we cannot overwrite an existing structure
        if self.workspace_structure.is_some() {
            return;
        }
        self.workspace_structure = Some(hir::WorkspaceStructure::new(self, 0));
    }

    fn update_workspace_structure(&mut self) {
        if let Some(workspace) = self.workspace_structure {
            let revision = *workspace.revision(self);
            workspace.set_revision(self).to(revision + 1);
            eprint!("updated workspace revision");
        }
    }

    fn register_for_workspace_updates(&self) -> u32 {
        if let Some(workspace) = self.workspace_structure {
            return *workspace.revision(self);
        }
        0
    }

    fn get_ast_for_file(&self, ned_file: &hir::SourceFile) -> Option<Tree> {
        self.ast_map
            .get(ned_file)
            .map(|entry| entry.value().clone())
    }

    fn set_ast_for_file(&mut self, ned_file: hir::SourceFile, tree: Tree) {
        self.ast_map.insert(ned_file, tree);
    }

    // INFO: might use something like this later to register for file changes since the workspace
    // is a salsa input in comparision to the nefile_input dashmap

    // fn get_ned_file(&self,uri: &Url) -> Option<hir::SourceFile> {
    //     if let Some(workspace) = &self.workspace {
    //         return workspace.files(self).get(&uri).map(|entry| *entry);
    //     }
    //     eprint!("No workspace available in DB");
    //     None
    //
    // }
}

#[salsa::tracked]
pub fn visible_definitions<'db>(
    db: &'db dyn crate::db::Db,
    ned_file: hir::SourceFile,
) -> Arc<Vec<DefIndex<'db>>> {
    let hir = crate::hir::ned::lower_to_hir(db, ned_file);
    let mut definitions = Vec::new();

    // Items in the same file are always visible → reuse the package tree instances
    definitions.extend(crate::db::package_tree::file_def_index(db, ned_file));

    // Explicit Imports
    for import in hir.imports(db).iter() {
        let pattern = ImportPattern::new(db, import.value.text(db).clone());

        if let Some(defs) = resolve_import(db, pattern) {
            definitions.extend(defs);
        }
    }

    //    Items in the same package are visible without import.
    if let Some(package_decl) = hir.package(db) {
        let package_name = package_decl.path.text(db);
        let wildcard = format!("{}.*", package_name);
        let pattern = ImportPattern::new(db, wildcard);

        if let Some(pkg_defs) = resolve_import(db, pattern) {
            definitions.extend(pkg_defs);
        }
    }
    Arc::new(definitions)
}

/// Get all definitions (modules, networks, channels, etc.) in a given NED file
#[salsa::tracked]
pub fn get_definitions_in_file(
    db: &dyn crate::db::Db,
    ned: hir::SourceFile,
) -> Vec<hir::ItemHir<'_>> {
    let hir_file = hir::ned::lower_to_hir(db, ned);
    hir_file.items(db).to_vec()
}

#[salsa::tracked]
pub fn file_package_name(db: &dyn Db, ned_file: hir::SourceFile) -> Option<String> {
    let hir = hir::ned::lower_to_hir(db, ned_file);
    hir.package(db).map(|pkg| pkg.path.text(db).clone())
}

fn not_tracked_function(db: &dyn Db, file: hir::SourceFile) -> String {
    other_tracked_function(db, file).clone()
}

// only reading the rope
#[salsa::tracked]
fn other_tracked_function(db: &dyn Db, file: hir::SourceFile) -> String {
    file.rope(db).to_string() // This is important
}

// only reading the url
#[salsa::tracked]
fn other_tracked_function2(db: &dyn Db, file: hir::SourceFile) -> String {
    file.uri(db).to_string()
}

#[salsa::tracked]
fn memoization_test_interned<'db>(db: &'db dyn Db, file: FilePathTest<'db>) -> String {
    println!(
        "Executing memoization_test_interned for path: {}",
        file.path(db)
    );

    let input = db.get_source_file_input(&Url::from_file_path(file.path(db)).unwrap());

    not_tracked_function(db, input.unwrap());

    file.path(db).clone()
}

#[salsa::tracked]
fn memoization_test_interned2<'db>(db: &'db dyn Db, file: FilePathTest<'db>) -> String {
    println!(
        "Executing memoization_test_interned for path: {}",
        file.path(db)
    );

    let input = db.get_source_file_input(&Url::from_file_path(file.path(db)).unwrap());

    other_tracked_function2(db, input.unwrap());

    file.path(db).clone()
}

#[allow(dead_code)]
fn print_node(node: tree_sitter::Node, indent: usize) {
    let indent_str = "  ".repeat(indent);

    // Byte ranges
    let start = node.start_byte();
    let end = node.end_byte();

    println!("{}{} [{}..{}]", indent_str, node.kind(), start, end);

    // Recurse into children
    for child in node.children(&mut node.walk()) {
        print_node(child, indent + 1);
    }
}

// TODO: Rewrite
#[cfg(test)]
mod tests {
    use tower_lsp::lsp_types::Url;

    use super::*; // Import db definitions
    use crate::hir::ned::lower_to_hir;
    use crate::hir::{ned::ParamBlockItem, *}; // Import hir definitions

    use ropey::Rope;

    // Testing the functionality of Salsa and when it correctly detects changes and invalidate
    // dependent queries
    #[test]
    fn test_lower_simple_module() {
        let mut db = RootLspDatabase::default();
        db.enable_logging();

        // Parse the text
        let text = r#"
package my.package;

import other.package.Base;

this_is_an_error

simple Node extends Base 
{
parameters: 
    int param1 = 1;
    @display("i=block/circle");

}"#;
        let rope = Rope::from_str(text);

        let ned_file = hir::SourceFile::new(
            &db,
            rope.clone(),
            Url::parse("file:///test.ned").unwrap(),
            0,
        );

        let hir = lower_to_hir(&db, ned_file);

        println!("--- Query Complete. Verifying HIR ---");

        let pkg_word = Word::new(&db, "my.package".to_string());
        assert_eq!(hir.package(&db).as_ref().unwrap().path, pkg_word);
        println!("Package: OK");

        // Check imports
        let import_word = Word::new(&db, "other.package.Base".to_string());
        assert_eq!(hir.imports(&db).len(), 1);
        assert_eq!(hir.imports(&db)[0].value, import_word);
        println!("Imports: OK");

        // Check definitions
        assert_eq!(hir.items(&db).len(), 1);
        let module_hir = &hir.items(&db)[0];
        // Check module name
        let name_word = Word::new(&db, "Node".to_string());
        assert_eq!(module_hir.name(&db), name_word);
        println!("Module Name: OK");

        // Check module extends
        // let extends_word = Word::new(&db, "Base".to_string());
        // println!(
        //     "Modules Extends: {:?}",
        //     module_hir.extends(&db).map(|word| word.text(&db))
        // );
        // assert_eq!(module_hir.extends(&db), &Some(extends_word));
        // println!("Module Extends: OK");
        // println!("\n--- Test Passed! ---");

        // Test parameters
        let params = module_hir.parameters(&db);
        assert_eq!(params.len(), 2);
        // check types
        match &params[0] {
            ParamBlockItem::Param(param) => {
                assert_eq!(*param.name(&db), Word::new(&db, "param1".to_string()));
                assert_eq!(
                    *param.ned_type(&db),
                    Some(hir::ned::type_inference::NedType::Int)
                );
            }
            _ => panic!("Expected Param"),
        }
        match &params[1] {
            ParamBlockItem::Property(prop) => {
                println!("Modules Extends: {:?}", prop.value_list(&db).text(&db));
                assert_eq!(*prop.name(&db), Word::new(&db, "display".to_string()));
                assert_eq!(
                    *prop.value_list(&db),
                    Word::new(&db, "\"i=block/circle\"".to_string())
                );
            }
            _ => panic!("Expected Property"),
        }

        // Check for error node
        let diagnostics: Vec<&hir::Diagnostic> =
            crate::hir::ned::lower_to_hir::accumulated::<hir::Diagnostic>(&db, ned_file);
        assert_eq!(diagnostics.len(), 1);
        for diag in diagnostics {
            println!(
                "Diagnostic: {} ({}..{})",
                diag.message, diag.start, diag.end
            );
        }

        println!("--- Running query again ---");
        let cached_output = crate::hir::ned::lower_to_hir(&db, ned_file);
        assert_eq!(hir, cached_output); // Should be identical

        // Check that `lower_to_hir` was not re-computed
        let logs = db.take_logs();
        // print logs
        for log in &logs {
            println!("{}", log);
        }

        let path_string_1 = "test".to_string();
        let path_string_2 = "test".to_string(); // Identical content, diff location

        // If FilePathTest is #[salsa::interned], these return the SAME integer ID.
        let id_1 = hir::shared::FilePath::new(&db, PathBuf::from(path_string_1));
        let id_2 = hir::shared::FilePath::new(&db, PathBuf::from(path_string_2));

        assert_eq!(id_1, id_2, "Interning failed! These should be the same ID.");

        let execution_count = logs.iter().filter(|s| s.contains("lower_to_hir")).count();
        assert_eq!(execution_count, 1, "Query was re-executed, caching failed!");

        let path = PathBuf::from("/some/path/to/file.ned");

        let test_ned_file = hir::SourceFile::new(
            &db,
            Rope::from_str("simple TestModule {}"),
            Url::from_file_path(&path).unwrap(),
            0,
        );

        db.set_ned_file_input(Url::from_file_path(&path).unwrap(), test_ned_file);

        let memoized_path =
            memoization_test_interned(&db, FilePathTest::new(&db, "/some/path/to/file.ned"));
        assert_eq!(*memoized_path, "/some/path/to/file.ned".to_string());

        memoization_test_interned(&db, FilePathTest::new(&db, "/some/path/to/file.ned"));

        // We are changing the content
        test_ned_file
            .set_rope(&mut db)
            .to(Rope::from_str("simple TestModule { int x; }"));

        memoization_test_interned(&db, FilePathTest::new(&db, "/some/path/to/file.ned"));

        let logs = db.take_logs();
        for log in &logs {
            println!("{}", log);
        }
        let execution_count = logs
            .iter()
            .filter(|s| s.contains("memoization_test_interned"))
            .count();
        assert_eq!(
            execution_count, 2,
            "Memoization did not break on input change!"
        );

        let memoized_path =
            memoization_test_interned2(&db, FilePathTest::new(&db, "/some/path/to/file.ned"));
        assert_eq!(*memoized_path, "/some/path/to/file.ned".to_string());

        memoization_test_interned2(&db, FilePathTest::new(&db, "/some/path/to/file.ned"));

        // We are changing the content
        test_ned_file
            .set_rope(&mut db)
            .to(Rope::from_str("simple TestModule { int x; int y;}"));

        memoization_test_interned2(&db, FilePathTest::new(&db, "/some/path/to/file.ned"));

        let logs = db.take_logs();
        for log in &logs {
            println!("{}", log);
        }
        let execution_count = logs
            .iter()
            .filter(|s| s.contains("memoization_test_interned"))
            .count();
        assert_eq!(
            execution_count, 1,
            "Memoization incorrectly broke on irrelevant input change!"
        );
    }
}
