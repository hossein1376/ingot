//! Semantic analysis for Ingot: pipeline phase 4 (`when`) and phase 5 (name
//! resolution), lowering every package to a resolved HIR.
//!
//! The entry point is [`resolve`]. It takes the packages of a compilation in
//! dependency order (each file already parsed, or `None` when the file had a lexical
//! or syntax error) and returns the definitions, one HIR package per input package,
//! and the diagnostics.

pub mod defs;
pub mod hir;
mod lower;
mod resolve;
pub mod target;
mod when;

pub use defs::{
    Builtin, Def, DefId, DefKind, ModuleData, ModuleId, PackageData, PackageId, Sema, Vis,
};
pub use target::Target;

use ingot_diag::diagnostic::Diagnostic;
use ingot_diag::source::Sources;
use ingot_syntax::ast;
use std::collections::BTreeMap;

/// One module of a package.
pub struct ModuleInput {
    /// Dotted path starting with the package name: `app` for the root module,
    /// `app.net.http` for `net/http.ingot`.
    pub path: String,
    /// Index of the module's file in the `Sources` given to [`resolve`].
    pub file: usize,
    /// The parsed file, or `None` when it has an E01xx or E02xx error: it then
    /// declares nothing and every name that would resolve into it is poison.
    pub ast: Option<ast::Module>,
}

pub struct PackageInput {
    pub name: String,
    pub modules: Vec<ModuleInput>,
}

pub struct Output {
    pub sema: Sema,
    /// One per input package, in the order given.
    pub packages: Vec<hir::Package>,
    pub diagnostics: Vec<Diagnostic>,
    /// Things the specification leaves undefined or that are not built yet. A
    /// non-empty list means the result must not be reported as a success.
    pub not_implemented: Vec<String>,
}

/// Run phases 4 and 5 for `packages` (in dependency order) for `target`.
pub fn resolve(sources: &Sources, packages: Vec<PackageInput>, target: &Target) -> Output {
    let mut r = resolve::Resolver {
        sources,
        sema: Sema::new(),
        diags: Vec::new(),
        not_implemented: Vec::new(),
        decl_at: BTreeMap::new(),
        self_defs: BTreeMap::new(),
        target,
    };
    let mut hirs = Vec::new();
    for package in packages {
        let declared = r.declare_package(package);
        hirs.push(lower::lower_package(&mut r, declared));
    }
    Output {
        sema: r.sema,
        packages: hirs,
        diagnostics: r.diags,
        not_implemented: r.not_implemented,
    }
}

#[cfg(test)]
mod tests;
