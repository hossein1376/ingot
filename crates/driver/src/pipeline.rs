//! The front-end pipeline (`docs/impl/pipeline.md`), as far as it is implemented.
//!
//! Phase 1 (load, with the manifest and package rules of E17xx), 2 and 3 (lex and parse
//! every file), 4 (`when`), 5 (name resolution), 6 (signatures, type properties,
//! layouts), 7 (type-checking bodies) and 8 (lowering to a CFG) run. Phases 9 and 10 do
//! not exist yet. A package with a body that passes phase 7, or with a constant that
//! phase 6 cannot evaluate, is not accepted: `Analysis` says so in `not_implemented`,
//! and the command line exits with status 2 instead of 0.

use crate::loader::{self, LoadOptions, LoadedPackage};
use ingot_diag::diagnostic::{DEFAULT_TARGET, Diagnostic, sort_diagnostics};
use ingot_diag::source::{Source, Sources};
use ingot_sema::{ModuleInput, PackageInput, Target};
use ingot_syntax::lexer::lex;
use ingot_syntax::parser::{check_operator_mixing, parse};
use std::io;
use std::path::Path;

/// What phase 6 alone leaves unchecked in a package that has bodies. Phase 7 checks
/// them, so it never appears in an `Analysis`.
pub const UNCHECKED: &str = ingot_sema::UNCHECKED_BODIES;

/// What the pipeline does not do yet for a body that passed phase 7.
pub const UNCHECKED_OWNERSHIP: &str = ingot_cfg::UNCHECKED_OWNERSHIP;

#[derive(Clone, Debug)]
pub struct Options {
    /// One of the three targets of spec section 24.
    pub target: String,
    pub load: LoadOptions,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            target: DEFAULT_TARGET.to_string(),
            load: LoadOptions::default(),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Analysis {
    pub sources: Sources,
    pub diagnostics: Vec<Diagnostic>,
    /// Reasons the result cannot be reported as an acceptance although no
    /// diagnostic was found: parts of the language the compiler does not check yet.
    pub not_implemented: Vec<String>,
    pub target: String,
    /// The number of `.ingot` files of the checked package (not the standard
    /// library's).
    pub files: usize,
    /// Phase 8's CFGs for the checked package, one per function and test body and one
    /// per closure and `par for` body.
    pub cfgs: ingot_cfg::Cfgs,
}

impl Analysis {
    /// True when the package was found to be correct: no diagnostics, and nothing was
    /// left unchecked.
    pub fn accepted(&self) -> bool {
        self.diagnostics.is_empty() && self.not_implemented.is_empty()
    }
}

/// A package whose files have been lexed and parsed.
struct Parsed {
    name: String,
    modules: Vec<ModuleInput>,
}

/// Lex and parse the files of `package`, whose sources start at `base` in `all`.
fn parse_package(
    package: &LoadedPackage,
    all: &Sources,
    base: usize,
    diagnostics: &mut Vec<Diagnostic>,
) -> Parsed {
    let mut modules = Vec::new();
    for module in &package.modules {
        let file = base + module.source_id;
        let source = all.get(file);
        let (tokens, mut found) = lex(source);
        let (ast, parse_diags) = parse(source, tokens);
        found.extend(parse_diags);
        found.extend(check_operator_mixing(&ast, source));
        // A file with a lexical or syntax error declares nothing (pipeline §1).
        let clean = found.is_empty() && !module.unreadable;
        diagnostics.extend(found);
        modules.push(ModuleInput {
            path: package.module_path(module),
            file,
            ast: clean.then_some(ast),
        });
    }
    Parsed {
        name: package.package_name.clone(),
        modules,
    }
}

/// Lex, parse, apply `when` and resolve every file of a loaded package.
pub fn analyze_with(package: LoadedPackage, options: &Options) -> io::Result<Analysis> {
    let target = Target::by_name(&options.target)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "unknown target"))?;
    let mut diagnostics = package.diagnostics.clone();
    let mut not_implemented = package.not_implemented.clone();

    // The standard library is implicitly available to every package but itself.
    let is_std = package.manifest.as_ref().is_some_and(|m| m.name == "std");
    let std = if is_std || package.manifest_failed {
        None
    } else {
        Some(loader::load_std()?)
    };

    // One list of sources: the package's own, then the standard library's.
    let mut all = package.sources.clone();
    let std_base = all.files.len();
    if let Some(std) = &std {
        for source in &std.sources.files {
            all.add(source.clone());
        }
    }

    let user = parse_package(&package, &all, 0, &mut diagnostics);
    let mut inputs = Vec::new();
    if let Some(std) = &std {
        diagnostics.extend(std.diagnostics.iter().cloned());
        let parsed = parse_package(std, &all, std_base, &mut diagnostics);
        inputs.push(PackageInput {
            name: parsed.name,
            modules: parsed.modules,
        });
    }
    let std_count = inputs.len();
    inputs.push(PackageInput {
        name: user.name,
        modules: user.modules,
    });

    let mut cfgs = ingot_cfg::Cfgs::default();
    // With dependencies the imports of their packages cannot be resolved, and
    // reporting every one of them as unresolved would be wrong: stop at what is known.
    if !package.manifest_failed && package.not_implemented.is_empty() {
        let output = ingot_sema::resolve(&mut all, inputs, &target);
        // Phase 6 finishes for every item of every package (std included) before
        // anything later; nothing in a body is looked at.
        let declarations = ingot_sema::check_declarations(&all, &output, &target, std_count);
        // Phase 7 checks the bodies of the package against those signatures.
        let mut bodies = ingot_sema::check_bodies(&all, &output, declarations, &target, std_count);
        // Phase 8 lowers every body phase 7 accepted.
        let lowered = ingot_cfg::lower_package(
            &all,
            &output.sema,
            &output.packages,
            &mut bodies.declarations,
            &bodies.bodies,
        );
        let lowered_not_implemented = lowered.not_implemented;
        cfgs = lowered.cfgs;
        diagnostics.extend(output.diagnostics);
        diagnostics.extend(bodies.declarations.diagnostics);
        diagnostics.extend(bodies.diagnostics);
        // A bare variant name that E0603 reports is not also an unused binding (E0311);
        // the binding error is made in phase 4, before the pattern is typed.
        let pattern_errors: std::collections::BTreeSet<(String, usize, usize)> = diagnostics
            .iter()
            .filter(|d| d.code == "E0603")
            .map(|d| (d.file.clone(), d.span.start, d.span.end))
            .collect();
        diagnostics.retain(|d| {
            !(d.code == "E0311"
                && pattern_errors.contains(&(d.file.clone(), d.span.start, d.span.end)))
        });
        not_implemented.extend(output.not_implemented);
        // Phase 7 does what phase 6 reports as unchecked.
        not_implemented.extend(
            bodies
                .declarations
                .not_implemented
                .into_iter()
                .filter(|m| m != UNCHECKED),
        );
        not_implemented.extend(bodies.not_implemented);
        not_implemented.extend(lowered_not_implemented);
        if bodies.bodies.bodies.values().any(|b| {
            !b.failed
                && matches!(
                    b.kind,
                    ingot_sema::typed::BodyKind::Fn | ingot_sema::typed::BodyKind::Test
                )
        }) {
            not_implemented.push(UNCHECKED_OWNERSHIP.to_string());
        }
    }
    not_implemented.dedup();
    sort_diagnostics(&mut diagnostics);
    let files = package
        .sources
        .iter()
        .filter(|s| s.path.ends_with(".ingot"))
        .count();
    Ok(Analysis {
        sources: all,
        diagnostics,
        not_implemented,
        target: options.target.clone(),
        files,
        cfgs,
    })
}

/// `analyze_with` for the default options.
pub fn analyze(package: LoadedPackage) -> Analysis {
    analyze_with(package, &Options::default()).unwrap_or_else(|e| internal_failure(&e))
}

fn internal_failure(error: &io::Error) -> Analysis {
    Analysis {
        not_implemented: vec![format!("the standard library is unavailable: {error}")],
        target: DEFAULT_TARGET.to_string(),
        ..Analysis::default()
    }
}

/// Load the package at `path` (a directory, or a file inside a package) and analyze it.
pub fn analyze_path(path: &Path) -> io::Result<Analysis> {
    analyze_path_with(path, &Options::default())
}

pub fn analyze_path_with(path: &Path, options: &Options) -> io::Result<Analysis> {
    let package = loader::load_with(path, options.load)?;
    let only = if path.is_file() {
        let root = &package.root;
        path.strip_prefix(root)
            .ok()
            .map(loader::relative_path)
            .or_else(|| {
                path.canonicalize().ok().and_then(|p| {
                    root.canonicalize()
                        .ok()
                        .and_then(|r| p.strip_prefix(&r).ok().map(loader::relative_path))
                })
            })
            .or_else(|| path.file_name().map(|n| n.to_string_lossy().into_owned()))
    } else {
        None
    };
    let mut analysis = analyze_with(package, options)?;
    // Only that file's source diagnostics: a manifest or command-line diagnostic is
    // about the whole package and is always printed.
    if let Some(file) = only {
        analysis
            .diagnostics
            .retain(|d| d.file == file || !d.file.ends_with(".ingot"));
    }
    Ok(analysis)
}

/// A source for a diagnostic that has no file: the command line.
pub fn command_line_source(text: &str) -> Source {
    Source::new("<command line>", text)
}
