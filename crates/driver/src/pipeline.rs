//! The front-end pipeline (`docs/impl/pipeline.md`), as far as it is implemented.
//!
//! Phase 1 (load, with the package graph, manifest and package rules of E17xx), 2 and
//! 3 (lex and parse every file), 4 (`when`), 5 (name resolution), 6 (signatures, type
//! properties, layouts), 7 (type-checking bodies) and 8 (lowering to a CFG) run. Phase 9
//! runs Analysis A (moves and linear values), Analysis B (origin sets) and Analysis C
//! (loans, E1101–E1105 and E1403) and the task rules (E1401, E1402, E1407) on the
//! checked packages' CFGs. A `from` clause computed for E1201 is attached to that
//! diagnostic. Phase 10 removes the guaranteed checks, reports E0806 for a folded
//! operation that traps, and records the surviving checks for `ingot cost`. A constant
//! that phase 6 cannot evaluate is not accepted either. The command line exits with
//! status 2 instead of 0 when `not_implemented` is non-empty.
//!
//! "Checked" means every package of the graph but `std` (05 §33.2). The packages are
//! fed to phase 5 in dependency order, `std` first and the root last.

use crate::files::{self, Files};
use crate::loader::{self, LoadOptions, LoadedGraph, LoadedPackage};
use ingot_diag::diagnostic::{DEFAULT_TARGET, Diagnostic, sort_diagnostics};
use ingot_diag::source::{Source, Sources};
use ingot_sema::{ModuleInput, PackageInput, Target};
use ingot_syntax::lexer::lex;
use ingot_syntax::parser::{check_operator_mixing, parse};
use std::collections::BTreeMap;
use std::io;
use std::path::Path;

/// What phase 6 alone leaves unchecked in a package that has bodies. Phase 7 checks
/// them, so it never appears in an `Analysis`.
pub const UNCHECKED: &str = ingot_sema::UNCHECKED_BODIES;

#[derive(Clone, Debug)]
pub struct Options {
    /// One of the three targets of spec section 24.
    pub target: String,
    pub load: LoadOptions,
    /// The roots phase 11a uses (pipeline §6a).
    pub roots: ingot_cfg::instances::Roots,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            target: DEFAULT_TARGET.to_string(),
            load: LoadOptions::default(),
            roots: ingot_cfg::instances::Roots::Check,
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
    /// Phase 7's typed bodies of the checked packages.
    pub bodies: ingot_sema::typed::Bodies,
    /// Index of the root package among `resolved.packages`: the package the command is
    /// about (a query runs against it).
    pub checked: usize,
    /// The checked packages among `resolved.packages` (every package but `std`).
    pub checked_packages: Vec<usize>,
}

#[derive(Default)]
pub struct Analysis {
    pub sources: Sources,
    pub diagnostics: Vec<Diagnostic>,
    /// Reasons the result cannot be reported as an acceptance although no
    /// diagnostic was found: parts of the language the compiler does not check yet.
    pub not_implemented: Vec<String>,
    pub target: String,
    /// The number of `.ingot` files of the build's packages (not the standard
    /// library's), each counted once.
    pub files: usize,
    /// Phase 8's CFGs for the checked packages, one per function and test body and one
    /// per closure and `par for` body. Phase 10 does not change these: the dump and
    /// its goldens are phase 8's output.
    pub cfgs: ingot_cfg::Cfgs,
    /// Phase 10's CFGs with the removed check statements gone.
    pub removed_cfgs: ingot_cfg::Cfgs,
    /// The checks that survived phase 10, in `ingot cost` order.
    pub cost: Vec<ingot_cfg::removal::CostItem>,
    /// When `PATH` was one file of a package, that file's display path. `check` and
    /// `cost` print only what belongs to it.
    pub only_file: Option<String>,
    /// The front end, kept when phases 5 to 7 ran.
    pub front_end: Option<FrontEnd>,
    /// Phase 11a: the instances of the checked packages, sorted by mangled name.
    pub instances: Vec<ingot_cfg::instances::Instance>,
    /// Bodies of `std` the back end links but the checked packages do not own.
    /// They are lowered by `lower_for_eval` and are not part of `cfgs`, so phase 9,
    /// phase 10 and instance discovery do not see them.
    pub extra_cfgs: Vec<ingot_cfg::Cfg>,
    /// The `[link]` tables of the build's packages, unioned in package order.
    pub link: BTreeMap<String, crate::manifest::Link>,
    /// A workspace root that is not a package: `check` runs over its members, but
    /// `build` is a named failure (05 §33.2).
    pub workspace_root_without_package: bool,
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

/// Lex, parse, apply `when` and resolve every file of a loaded build. Runs on its own
/// thread with [`PIPELINE_STACK`], so every caller (the binary, the conformance runner,
/// the language server) gets the stack the evaluation limits assume.
pub fn analyze_graph_with(graph: LoadedGraph, options: &Options) -> io::Result<Analysis> {
    std::thread::scope(|s| {
        let worker = std::thread::Builder::new()
            .name("ingot-pipeline".to_string())
            .stack_size(PIPELINE_STACK)
            .spawn_scoped(s, || analyze_here(graph, options))?;
        worker
            .join()
            .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
    })
}

/// `analyze_graph_with` for one package with no dependencies: the fix engine's entry.
pub fn analyze_with(package: LoadedPackage, options: &Options) -> io::Result<Analysis> {
    let graph = LoadedGraph {
        packages: vec![package],
        root: Some(0),
        checked: vec![0],
        order: vec![0],
        deps: vec![Vec::new()],
        ..LoadedGraph::default()
    };
    analyze_graph_with(graph, options)
}

fn analyze_here(graph: LoadedGraph, options: &Options) -> io::Result<Analysis> {
    let target = Target::by_name(&options.target)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "unknown target"))?;
    let mut diagnostics = graph.diagnostics.clone();
    let mut not_implemented = Vec::new();
    for package in &graph.packages {
        diagnostics.extend(package.diagnostics.iter().cloned());
        not_implemented.extend(package.not_implemented.iter().cloned());
    }
    // A graph error that stopped loading leaves names unresolved; reporting every one
    // of them as E0301 would bury the error that stopped it.
    let load_failed = graph.packages.iter().any(|package| package.manifest_failed)
        || diagnostics
            .iter()
            .any(|d| d.code == "E0303" || d.code.starts_with("E17"));

    let is_std_root = graph
        .root
        .is_some_and(|index| graph.packages[index].package_name == "std");
    let std = if is_std_root {
        None
    } else {
        Some(loader::load_std(&options.load)?)
    };
    let mut link: BTreeMap<String, crate::manifest::Link> = BTreeMap::new();
    let merge = |link: &mut BTreeMap<String, crate::manifest::Link>,
                 manifest: &crate::manifest::Manifest| {
        for (target, table) in &manifest.link {
            let entry = link.entry(target.clone()).or_default();
            for framework in &table.frameworks {
                if !entry.frameworks.contains(framework) {
                    entry.frameworks.push(framework.clone());
                }
            }
            for lib in &table.libs {
                if !entry.libs.contains(lib) {
                    entry.libs.push(lib.clone());
                }
            }
            for asm in &table.asm {
                if !entry.asm.contains(asm) {
                    entry.asm.push(asm.clone());
                }
            }
        }
    };
    if let Some(std) = &std
        && let Some(manifest) = &std.manifest
    {
        merge(&mut link, manifest);
    }
    for package in &graph.packages {
        if let Some(manifest) = &package.manifest {
            merge(&mut link, manifest);
        }
    }

    // One list of sources, std first, then the build's packages in dependency order.
    let mut all = Sources::default();
    let mut inputs = Vec::new();
    let mut feed_of: BTreeMap<usize, usize> = BTreeMap::new();
    let mut std_base = None;
    if let Some(std) = &std {
        std_base = Some(all.files.len());
        for source in &std.sources.files {
            all.add(source.clone());
        }
    }
    for source in &graph.extra_sources.files {
        all.add(source.clone());
    }
    let order = graph.dependency_order(graph.root);
    let mut package_bases = Vec::new();
    for &index in &order {
        let base = all.files.len();
        for source in &graph.packages[index].sources.files {
            all.add(source.clone());
        }
        package_bases.push((index, base));
    }

    // Parse std first, as the pipeline always feeds it first.
    if let (Some(std), Some(base)) = (&std, std_base) {
        diagnostics.extend(std.diagnostics.iter().cloned());
        not_implemented.extend(std.not_implemented.iter().cloned());
        let parsed = parse_package(std, &all, base, &mut diagnostics);
        inputs.push(PackageInput {
            name: parsed.name,
            deps: Vec::new(),
            modules: parsed.modules,
        });
    }
    let std_feed = if std_base.is_some() { Some(0) } else { None };
    let first = std_feed.map_or(0, |_| 1);
    for (position, (index, base)) in package_bases.iter().enumerate() {
        let parsed = parse_package(&graph.packages[*index], &all, *base, &mut diagnostics);
        feed_of.insert(*index, first + position);
        inputs.push(PackageInput {
            name: parsed.name,
            deps: graph.deps[*index]
                .iter()
                .map(|dep| graph.packages[*dep].package_name.clone())
                .collect(),
            modules: parsed.modules,
        });
    }
    let offset = first;
    // Every package of the graph but std is checked (05 §33.2).
    let checked_feed: Vec<usize> = (offset..inputs.len()).collect();
    let root_feed = graph
        .root
        .map(|index| feed_of[&index])
        .or_else(|| checked_feed.last().copied())
        .unwrap_or(offset);

    let mut cfgs = ingot_cfg::Cfgs::default();
    let mut extra_cfgs = Vec::new();
    let mut removed_cfgs = ingot_cfg::Cfgs::default();
    let mut cost = Vec::new();
    let mut front_end = None;
    let mut instances = Vec::new();
    if !load_failed && !inputs.is_empty() {
        let output = ingot_sema::resolve(&mut all, inputs, &target);
        // Phase 6 finishes for every item of every package (std included) before
        // anything later; nothing in a body is looked at.
        let evaluator = ingot_cfg::eval::EvaluatorFactory::default();
        let declarations =
            ingot_sema::check_declarations(&all, &output, &target, &checked_feed, Some(&evaluator));
        // Phase 7 checks the bodies of every checked package against those
        // signatures.
        let mut bodies = ingot_sema::check_bodies(
            &all,
            &output,
            declarations,
            &target,
            &checked_feed,
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
        // A build links the runtime core, whose bodies are `std`'s and are not a
        // checked package's. Type them here and lower every body phase 7 accepted
        // for the back end; reachability picks what is emitted. `ingot check` does
        // not need them.
        if options.roots == ingot_cfg::instances::Roots::Build
            && let Some(std_feed) = std_feed
        {
            let std_checked = ingot_sema::check_bodies(
                &all,
                &output,
                bodies.declarations,
                &target,
                &[std_feed],
                Some(&evaluator),
            );
            bodies.declarations = std_checked.declarations;
            diagnostics.extend(std_checked.diagnostics);
            not_implemented.extend(std_checked.not_implemented);
            let mut extra_set = ingot_cfg::Cfgs::default();
            for typed in std_checked.bodies.bodies.values() {
                if typed.failed {
                    continue;
                }
                match ingot_cfg::lower_for_eval(
                    &all,
                    &output.sema,
                    &output.packages,
                    &mut bodies.declarations,
                    typed,
                ) {
                    Ok(found) => {
                        for cfg in found.into_vec() {
                            extra_set.push(cfg);
                        }
                    }
                    Err(what) => not_implemented.push(format!("lowering: {what}")),
                }
            }
            if !extra_set.is_empty() {
                let removal =
                    ingot_cfg::removal::remove_checks(&bodies.declarations, &all, &extra_set);
                diagnostics.extend(removal.diagnostics);
                not_implemented.extend(removal.not_implemented);
                extra_cfgs.extend(removal.cfgs.into_vec());
            }
        }
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
        // CFGs are not in `cfgs`.
        // E1201 was copied out of the declarations above; attach the clause here.
        let owned =
            ingot_cfg::check::check_ownership(&output.sema, &mut bodies.declarations, &all, &cfgs);
        ingot_cfg::check::attach_from_fixes(&mut diagnostics, &owned.from_fixes);
        diagnostics.extend(owned.diagnostics);
        not_implemented.extend(owned.not_implemented);
        // Phase 10: the guaranteed removals, E0806 for a folded operation that traps,
        // and the surviving checks `ingot cost` reports.
        let removal = ingot_cfg::removal::remove_checks(&bodies.declarations, &all, &cfgs);
        diagnostics.extend(removal.diagnostics);
        not_implemented.extend(removal.not_implemented);
        removed_cfgs = removal.cfgs;
        cost = removal.items;
        // The cost rows that are properties of the checked CFG rather than a
        // surviving check: alloc, indirect_call, copy and task_spawn (§31).
        cost.extend(ingot_cfg::cost::cost_items(
            &mut bodies.declarations,
            &output.sema,
            &cfgs,
        ));
        cost.sort_by(|a, b| {
            (a.fn_name.clone(), a.file, a.span.start, a.span.end, a.kind).cmp(&(
                b.fn_name.clone(),
                b.file,
                b.span.start,
                b.span.end,
                b.kind,
            ))
        });
        // Phase 11a: instance discovery, depth (E0809) and mangled names. Later
        // per-instance checks run on this set (part 2).
        let discovered = ingot_cfg::instances::discover(
            &all,
            &output.sema,
            &mut bodies.declarations,
            &bodies.bodies,
            &output.packages,
            &cfgs,
            options.roots,
            graph.root.map(|index| feed_of[&index]),
        );
        for d in &discovered.diagnostics {
            let duplicate = diagnostics.iter().any(|e| {
                e.code == d.code && e.file == d.file && e.span == d.span && e.message == d.message
            });
            if !duplicate {
                diagnostics.push(d.clone());
            }
        }
        not_implemented.extend(discovered.not_implemented.iter().cloned());
        // Per-instance checks that need typed bodies or the interpreter (E0810,
        // E0521, E0515, E1502, E1504, E1514).
        let scopes: Vec<ingot_sema::InstanceArgs> = discovered
            .instances
            .iter()
            .filter(|i| matches!(i.key.body, ingot_cfg::instances::InstanceBody::Item))
            .map(|i| ingot_sema::InstanceArgs {
                owner: i.key.owner,
                subst: i.key.subst.clone(),
                labels: i.labels.clone(),
            })
            .collect();
        let checked = ingot_sema::check_instances(
            &all,
            &output,
            bodies.declarations,
            &bodies.bodies,
            &target,
            &checked_feed,
            Some(&evaluator),
            &scopes,
        );
        bodies.declarations = checked.declarations;
        diagnostics.extend(checked.diagnostics);
        not_implemented.extend(
            checked
                .not_implemented
                .into_iter()
                .filter(|m| m != UNCHECKED),
        );
        cost.extend(ingot_cfg::cost::park_points(
            &mut bodies.declarations,
            &output.sema,
            &cfgs,
            &discovered,
        ));
        cost.sort_by(|a, b| {
            (a.fn_name.clone(), a.file, a.span.start, a.span.end, a.kind).cmp(&(
                b.fn_name.clone(),
                b.file,
                b.span.start,
                b.span.end,
                b.kind,
            ))
        });
        instances = discovered.instances;
        front_end = Some(FrontEnd {
            resolved: output,
            declarations: bodies.declarations,
            bodies: bodies.bodies,
            checked: root_feed,
            checked_packages: checked_feed.clone(),
        });
    }
    not_implemented.dedup();
    sort_diagnostics(&mut diagnostics);
    let mut seen = std::collections::BTreeSet::new();
    let files = graph
        .packages
        .iter()
        .flat_map(|package| package.sources.iter())
        .filter(|source| source.path.ends_with(".ingot"))
        .filter(|source| seen.insert(files::normalize(&source.absolute)))
        .count();
    Ok(Analysis {
        sources: all,
        diagnostics,
        not_implemented,
        target: options.target.clone(),
        files,
        cfgs,
        removed_cfgs,
        cost,
        only_file: None,
        front_end,
        instances,
        extra_cfgs,
        link,
        workspace_root_without_package: graph.workspace.is_some() && graph.root.is_none(),
    })
}

/// `analyze_graph_with` for the default options.
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

/// Load the package at `path` (a directory, a file inside a package, or a workspace
/// root) and analyze it.
pub fn analyze_path(path: &Path) -> io::Result<Analysis> {
    analyze_path_with(path, &Options::default())
}

pub fn analyze_path_with(path: &Path, options: &Options) -> io::Result<Analysis> {
    let mut load = options.load.clone();
    load.target = Some(options.target.clone());
    let graph = loader::load_graph(path, load)?;
    let only = if Files::new(options.load.overlays.clone()).is_file(path) {
        source_display_path(&graph, path)
    } else {
        None
    };
    let mut analysis = analyze_graph_with(graph, options)?;
    analysis.only_file = only.clone();
    // Only that file's source diagnostics: a manifest or command-line diagnostic is
    // about the whole package and is always printed.
    if let Some(file) = only {
        analysis
            .diagnostics
            .retain(|d| d.file == file || !d.file.ends_with(".ingot"));
    }
    Ok(analysis)
}

/// The display path of the file `path` in the loaded build: `name/rel` for a
/// dependency, the workspace-relative path for a member.
fn source_display_path(graph: &LoadedGraph, path: &Path) -> Option<String> {
    let key = files::normalize(path);
    for package in &graph.packages {
        for source in package.sources.iter() {
            if files::normalize(&source.absolute) == key {
                return Some(source.path.clone());
            }
        }
    }
    graph
        .extra_sources
        .iter()
        .find(|source| files::normalize(&source.absolute) == key)
        .map(|source| source.path.clone())
}

/// A source for a diagnostic that has no file: the command line.
pub fn command_line_source(text: &str) -> Source {
    Source::new("<command line>", text)
}
