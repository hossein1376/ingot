//! Semantic analysis for Ingot: pipeline phase 4 (`when`), phase 5 (name resolution,
//! lowering every package to a resolved HIR) and phase 6 (signatures, type properties
//! and layouts).
//!
//! [`resolve`] takes the packages of a compilation in dependency order (each file
//! already parsed, or `None` when the file had a lexical or syntax error) and returns
//! the definitions, one HIR package per input package, and the diagnostics.
//! [`check_declarations`] then builds the [`Declarations`] of all of them.

mod checks;
pub mod consteval;
pub mod defs;
pub mod hir;
mod layout;
mod lower;
pub mod lowering;
mod props;
mod resolve;
pub mod sigs;
pub mod target;
mod traits;
pub mod ty;
mod when;

pub use layout::{LayoutError, align_up, valid_align};
pub use sigs::{
    AliasSig, AlignAttr, Bound, ConstSig, Declarations, EnumSig, FieldLayout, FieldSig, FnSig,
    GenericSig, Head, ImplOf, ImplSig, ImplTables, InterfaceSig, ItemSig, Layout, LayoutKind,
    OriginSig, ParamSig, PropBound, Props, StaticSig, StructSig, Tri, VariantSig,
};
pub use traits::ImplMatch;
pub use ty::{
    ConstArg, ConstValue, FloatTy, FnKind, FnPtrSig, GenericArg, IntTy, Lane, Mutability, Subst,
    TyKind, TypeId, TypeTable,
};

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

/// What phase 6 does not do for a package that has function or test bodies.
pub const UNCHECKED_BODIES: &str = "type checking of function bodies (pipeline phases 7 to 10)";

/// Run phase 6 for every package of `resolved`. `checked` is the index of the package
/// the command is about (the last one): the parts of the language that are not
/// implemented are reported for it only, since an unimplemented part of the standard
/// library's constants does not make a program that does not use it unchecked.
pub fn check_declarations(
    sources: &Sources,
    resolved: &Output,
    target: &Target,
    checked: usize,
) -> Declarations {
    let mut cx = lowering::Cx::new(&resolved.sema, &resolved.packages, sources, target, checked);
    cx.check_all();
    cx.d
}

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests6;
