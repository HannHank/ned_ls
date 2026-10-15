use std::hash::Hash;

use crate::hir::{Span, Word};

/// Represents: `import <path>;`
#[derive(Debug, Clone, PartialEq, Eq, Hash, salsa::SalsaValue)]
pub struct ImportHir<'db> {
    pub value: Word<'db>,
    pub span: Span,
}

// Test using the idea of Rust-analyzer
// I should remove importHir in favor of this but in a quick test I saw performance degradation
// which is why I kept both for now.
/// Intern the import pattern so we can query it efficiently
#[salsa::interned(debug)]
pub struct ImportPattern<'db> {
    #[returns(ref)]
    pub pattern: String,
}
