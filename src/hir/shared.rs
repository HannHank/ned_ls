use std::path::PathBuf;

use ropey::Rope;
use salsa::Setter;
use tower_lsp::lsp_types::Url;

use crate::db::Db;

use crate::db::RootLspDatabase;

#[salsa::interned(debug)]
pub struct Word<'db> {
    #[returns(ref)]
    pub text: String,
}

// High-level Intermediate Representation (HIR) for the NED language.
/// Represents the complete semantic content of a single .ned file.
#[salsa::interned(debug)]
pub struct FilePath {
    #[returns(ref)]
    pub path: PathBuf,
}

#[salsa::interned(debug)]
pub struct FilePathTest {
    #[returns(ref)]
    pub path: String,
}

/// WIP: used to register queries that depend on workspace changes. I think interned values are a
/// better solution but I do not fully understand how this would function in a more complex setup
/// where missing 'imports' should trigger a re-analysis if they are later added.
#[salsa::input]
pub struct WorkspaceStructure {
    pub revision: u32,
}

#[salsa::input(debug)]
pub struct WorkspaceState {
    pub files: std::collections::HashMap<Url, SourceFile>,
}

#[salsa::input(debug)]
pub struct SourceFile {
    #[returns(ref)]
    pub rope: Rope,

    #[returns(ref)]
    pub uri: Url,

    pub version: i32,
}

impl SourceFile {
    #[allow(dead_code)]
    pub fn increment_version(&self, db: &mut RootLspDatabase) -> i32 {
        let updated_version = self.version(db) + 1;
        self.set_version(db).to(updated_version);
        updated_version
    }

    pub fn rename(&self, db: &mut RootLspDatabase, new_uri: Url) {
        db.remove_source_file_input(self.uri(&db.clone()));
        self.set_uri(db).to(new_uri.clone());
        db.set_ned_file_input(new_uri.clone(), *self);
    }
}
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash, salsa::SalsaValue)]
pub struct Span {
    pub start: usize,
    pub end: usize,
}

impl Span {
    pub fn new(start: usize, end: usize) -> Self {
        Self { start, end }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
    Hint,
    Information,
}

impl From<Severity> for tower_lsp::lsp_types::DiagnosticSeverity {
    fn from(severity: Severity) -> Self {
        match severity {
            Severity::Error => Self::ERROR,
            Severity::Warning => Self::WARNING,
            Severity::Hint => Self::HINT,
            Severity::Information => Self::INFORMATION,
        }
    }
}

#[salsa::accumulator]
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)] // Debug impl uses them
pub struct Diagnostic {
    pub start: usize,
    pub end: usize,
    pub message: String,
    pub version: i32,
    pub severity: Severity,
    pub url: Option<Url>,
}

impl Diagnostic {
    #[allow(dead_code)]
    pub fn new(
        start: usize,
        end: usize,
        message: String,
        version: i32,
        severity: Severity,
        url: Option<Url>,
    ) -> Self {
        Diagnostic {
            start,
            end,
            message,
            version,
            severity,
            url,
        }
    }

    // #[cfg(test)]
    // pub fn render(&self, db: &dyn crate::Db, src: NedFileHir) -> String {
    //     use annotate_snippets::*;
    //     let line_start = src.text(db)[..self.start].lines().count() + 1;
    //     Renderer::plain()
    //         .render(
    //             Level::Error.title(&self.message).snippet(
    //                 Snippet::source(src.text(db))
    //                     .line_start(line_start)
    //                     .origin("input")
    //                     .fold(true)
    //                     .annotation(Level::Error.span(self.start..self.end).label("here")),
    //             ),
    //         )
    //         .to_string()
    // }
}
