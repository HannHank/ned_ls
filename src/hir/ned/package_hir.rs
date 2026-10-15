use crate::hir::*;

#[derive(Debug, Clone, PartialEq, Eq, Hash, salsa::SalsaValue)] // salsa::SalsaValue needed in order to
pub enum ResolvedPackage<'db> {
    FromPath(PackageName<'db>),
    FromName(PackageName<'db>),
}

#[salsa::interned(debug)]
pub struct PackageName {
    #[returns(ref)]
    pub name: String,
    #[returns(ref)]
    pub root: String,
}

/// Represents: `package <path>;`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, salsa::SalsaValue)]
pub struct PackageHir<'db> {
    pub path: Word<'db>,
    pub span: Span,
}

#[salsa::tracked(debug)]
pub struct PackagesInWorkspace<'db> {
    pub packages: Vec<PackageHir<'db>>,
}
