//! The front-end pipeline (`docs/impl/pipeline.md`), as far as it is implemented.
//!
//! Phase 1 (load, with the manifest and package rules of E17xx), 2 and 3 (lex and parse
//! every file), 4 (`when`), 5 (name resolution), 6 (signatures, type properties,
//! layouts), 7 (type-checking bodies) and 8 (lowering to a CFG) run. Phase 9 runs
//! Analysis A (moves and linear values), Analysis B (origin sets) and Analysis C
//! (loans, E1101–E1105 and E1403) and the task rules (E1401, E1402, E1407) on the
//! checked package's CFGs. A `from` clause computed for E1201 is attached to that
//! diagnostic. Phase 10 is not done, so a package with a body that passes phase 7
//! still records that in `not_implemented`. A
//! constant that phase 6 cannot evaluate is not accepted either. The command line exits
//! with status 2 instead of 0 when
//! `not_implemented` is non-empty.

use crate::files::{self, Files};
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
pub const UNCHECKED_CHECK_REMOVAL: &str = ingot_cfg::UNCHECKED_CHECK_REMOVAL;

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

/// What phases 5 to 7 produced, kept for queries after `check` has printed its
/// diagnostics (L3). Nothing in `check` reads it.
pub struct FrontEnd {
    /// Phase 5's definitions and the HIR of every loaded package, std included.
    pub resolved: ingot_sema::Output,
    /// Phase 6's signatures, types and properties.
    pub declarations: ingot_sema::Declarations,
    /// Phase 7's typed bodies of the checked package.
    pub bodies: ingot_sema::typed::Bodies,
    /// Index of the checked package among `resolved.packages`.
    pub checked: usize,
}

#[derive(Default)]
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
    /// The front end, kept when phases 5 to 7 ran.
    pub front_end: Option<FrontEnd>,
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

/// The stack the pipeline runs on. Constant evaluation nests (a body it pulls forward
/// can evaluate its own constants), up to the 1024-deep call limit of pipeline section
/// 4.6, and each level uses host stack for typing, lowering and evaluation. A thread
/// stack is virtual memory: only the pages reached are resident.
pub const PIPELINE_STACK: usize = 256 * 1024 * 1024;

/// Lex, parse, apply `when` and resolve every file of a loaded package. Runs on its own
/// thread with [`PIPELINE_STACK`], so every caller (the binary, the conformance runner,
/// the language server) gets the stack the evaluation limits assume.
pub fn analyze_with(package: LoadedPackage, options: &Options) -> io::Result<Analysis> {
    std::thread::scope(|s| {
        let worker = std::thread::Builder::new()
            .name("ingot-pipeline".to_string())
            .stack_size(PIPELINE_STACK)
            .spawn_scoped(s, || analyze_here(package, options))?;
        worker
            .join()
            .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
    })
}

fn analyze_here(package: LoadedPackage, options: &Options) -> io::Result<Analysis> {
    let target = Target::by_name(&options.target)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "unknown target"))?;
    let mut diagnostics = package.diagnostics.clone();
    let mut not_implemented = package.not_implemented.clone();

    // The standard library is implicitly available to every package but itself.
    let is_std = package.manifest.as_ref().is_some_and(|m| m.name == "std");
    let std = if is_std || package.manifest_failed {
        None
    } else {
        Some(loader::load_std(&options.load)?)
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
    let mut front_end = None;
    // With dependencies the imports of their packages cannot be resolved, and
    // reporting every one of them as unresolved would be wrong: stop at what is known.
    if !package.manifest_failed && package.not_implemented.is_empty() {
        let output = ingot_sema::resolve(&mut all, inputs, &target);
        // Phase 6 finishes for every item of every package (std included) before
        // anything later; nothing in a body is looked at.
        let evaluator = ingot_cfg::eval::EvaluatorFactory::default();
        let declarations =
            ingot_sema::check_declarations(&all, &output, &target, std_count, Some(&evaluator));
        // Phase 7 checks the bodies of the package against those signatures.
        let mut bodies = ingot_sema::check_bodies(
            &all,
            &output,
            declarations,
            &target,
            std_count,
            Some(&evaluator),
        );
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
        diagnostics.extend(output.diagnostics.iter().cloned());
        diagnostics.extend(bodies.declarations.diagnostics.iter().cloned());
        diagnostics.extend(bodies.diagnostics.iter().cloned());
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
        not_implemented.extend(output.not_implemented.iter().cloned());
        // Phase 7 does what phase 6 reports as unchecked.
        not_implemented.extend(
            bodies
                .declarations
                .not_implemented
                .iter()
                .filter(|m| *m != UNCHECKED)
                .cloned(),
        );
        not_implemented.extend(bodies.not_implemented.iter().cloned());
        not_implemented.extend(lowered_not_implemented);
        // Phase 9, analyses A, B and C, plus the task rules. Constant-evaluation
        // CFGs are not in `cfgs`. Phase 10 is still open.
        // E1201 was copied out of the declarations above; attach the clause here.
        let owned =
            ingot_cfg::check::check_ownership(&output.sema, &mut bodies.declarations, &all, &cfgs);
        ingot_cfg::check::attach_from_fixes(&mut diagnostics, &owned.from_fixes);
        diagnostics.extend(owned.diagnostics);
        not_implemented.extend(owned.not_implemented);
        if bodies.bodies.bodies.values().any(|b| {
            !b.failed
                && matches!(
                    b.kind,
                    ingot_sema::typed::BodyKind::Fn | ingot_sema::typed::BodyKind::Test
                )
        }) {
            not_implemented.push(UNCHECKED_CHECK_REMOVAL.to_string());
        }
        front_end = Some(FrontEnd {
            resolved: output,
            declarations: bodies.declarations,
            bodies: bodies.bodies,
            checked: std_count,
        });
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
        front_end,
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
    let package = loader::load_with(path, options.load.clone())?;
    let files = Files::new(options.load.overlays.clone());
    let only = if files.is_file(path) {
        let root = &package.root;
        package_relative(path, root)
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

/// The path of `path` relative to the package root, using `/`. A file that an overlay
/// makes exist has no canonical path of its own, so its directory is canonicalised
/// instead.
fn package_relative(path: &Path, root: &Path) -> Option<String> {
    if let Ok(rel) = path.strip_prefix(root) {
        return Some(loader::relative_path(rel));
    }
    let canonical = path
        .canonicalize()
        .ok()
        .unwrap_or_else(|| files::normalize(path));
    let root = root
        .canonicalize()
        .unwrap_or_else(|_| files::normalize(root));
    if let Ok(rel) = canonical.strip_prefix(&root) {
        return Some(loader::relative_path(rel));
    }
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
}

/// A source for a diagnostic that has no file: the command line.
pub fn command_line_source(text: &str) -> Source {
    Source::new("<command line>", text)
}
