pub mod ini;
pub mod ned;
pub mod shared;
pub mod tree_utils;
use salsa::Accumulator;

pub use shared::{
    Diagnostic, Severity, SourceFile, Span, Word, WorkspaceState, WorkspaceStructure,
};

pub use ned::{ItemHir, NedFileHir};
