//! Semantic analysis for Ingot: pipeline phase 4 (`when`), phase 5 (name resolution,
//! lowering every package to a resolved HIR) and phase 6 (signatures, type properties
//! and layouts).
//!
//! [`resolve`] takes the packages of a compilation in dependency order (each file
//! already parsed, or `None` when the file had a lexical or syntax error) and returns
//! the definitions, one HIR package per input package, and the diagnostics.
//! [`check_declarations`] then builds the [`Declarations`] of all of them.
//!
//! The built-in interfaces of specification section 4.4 are declared in Ingot text
//! (`builtin.ingot`) that [`resolve`] adds to the package `std` as a hidden module; see
//! [`BUILTIN_PATH`].

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
    AliasSig, AlignAttr, Bound, BuiltinNames, ConstSig, Declarations, EnumSig, FieldLayout,
    FieldSig, FnSig, GenericSig, Head, ImplOf, ImplSig, ImplTables, InterfaceSig, ItemSig, Layout,
    LayoutKind, OriginSig, ParamSig, PropBound, Props, StaticSig, StructSig, Tri, VariantSig,
};
pub use traits::{ImplMatch, MethodCandidate, MethodSource};
pub use ty::{
    ConstArg, ConstValue, FloatTy, FnKind, FnPtrSig, GenericArg, IntTy, Lane, Mutability, Subst,
    TyKind, TypeId, TypeTable,
};

pub use defs::{
    Builtin, Def, DefId, DefKind, ModuleData, ModuleId, PackageData, PackageId, Sema, Vis,
};
pub use target::Target;

use ingot_diag::diagnostic::Diagnostic;
use ingot_diag::source::{Source, Sources};
use ingot_syntax::ast;
use ingot_syntax::lexer::lex;
use ingot_syntax::parser::parse;
use std::collections::BTreeMap;

/// The Ingot text that declares `Ordering`, `Eq`, `Ord`, `Hash`, `Iter` and `Format`
/// (specification section 4.4).
pub const BUILTIN_SOURCE: &str = include_str!("builtin.ingot");

/// The path of the builtin declarations among the sources. No diagnostic may name it:
/// [`resolve`] and [`check_declarations`] turn one that would into a
/// `not_implemented` entry, since it is a compiler failure, not an error in the program.
pub const BUILTIN_PATH: &str = "<builtin>";

/// The module path of the builtin declarations: a hidden module of the package `std`
/// (whose `hash` and `fmt` modules the declarations name). No `import` can spell it.
pub const BUILTIN_MODULE_PATH: &str = "std.<builtin>";

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
///
/// The builtin declarations are added to `sources` and, as a hidden module, to the
/// package named `std`, which must therefore be among `packages` and declare the
/// modules `hash` and `fmt`. Without it the builtin names do not exist and the result
/// says so in `not_implemented`.
pub fn resolve(sources: &mut Sources, mut packages: Vec<PackageInput>, target: &Target) -> Output {
    let mut not_implemented = Vec::new();
    let file = sources.add(Source::new(BUILTIN_PATH, BUILTIN_SOURCE));
    let (tokens, mut problems) = lex(sources.get(file));
    let (ast, parse_problems) = parse(sources.get(file), tokens);
    problems.extend(parse_problems);
    if !problems.is_empty() {
        not_implemented.push("internal error: the builtin declarations do not parse".to_string());
    } else if let Some(std) = packages.iter_mut().find(|p| p.name == "std") {
        std.modules.push(ModuleInput {
            path: BUILTIN_MODULE_PATH.to_string(),
            file,
            ast: Some(ast),
        });
    } else {
        not_implemented.push(
            "the builtin interfaces: the package `std`, which declares them, is missing"
                .to_string(),
        );
    }
    let sources: &Sources = sources;
    let mut r = resolve::Resolver {
        sources,
        sema: Sema::new(),
        diags: Vec::new(),
        not_implemented,
        decl_at: BTreeMap::new(),
        self_defs: BTreeMap::new(),
        target,
    };
    let mut hirs = Vec::new();
    for package in packages {
        let declared = r.declare_package(package);
        hirs.push(lower::lower_package(&mut r, declared));
    }
    let mut not_implemented = r.not_implemented;
    let diagnostics = scrub_builtin(r.diags, &mut not_implemented);
    Output {
        sema: r.sema,
        packages: hirs,
        diagnostics,
        not_implemented,
    }
}

/// Keep diagnostics out of the builtin declarations. One whose primary span is there is
/// a compiler failure and becomes a `not_implemented` entry; related spans and fixes
/// there are dropped (the declaration of a built-in interface is not shown to the user).
fn scrub_builtin(diags: Vec<Diagnostic>, internal: &mut Vec<String>) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    for mut d in diags {
        if d.file == BUILTIN_PATH {
            internal.push(format!(
                "internal error: {} points into the builtin declarations: {}",
                d.code, d.message
            ));
            continue;
        }
        d.related.retain(|r| r.file != BUILTIN_PATH);
        d.fixes
            .retain(|f| f.edits.iter().all(|e| e.file != BUILTIN_PATH));
        out.push(d);
    }
    out
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
    let mut d = cx.d;
    d.diagnostics = scrub_builtin(std::mem::take(&mut d.diagnostics), &mut d.not_implemented);
    d
}

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests6;
