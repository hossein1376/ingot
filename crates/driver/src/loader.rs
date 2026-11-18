//! Phase 1: load a package graph (`ingot.toml` and its directory tree) into source
//! files and module paths, and locate the standard library.
//!
//! A package is a directory with an `ingot.toml` (spec section 33); every `.ingot` file
//! below it is a module. A nested `ingot.toml` starts another package, so it ends this
//! one. `main.ingot` at the package root is the root module, whose path is the package
//! name itself; `net/http.ingot` is `net.http`. A single file given on its own is a
//! package named `t` with no manifest, whose root module is that file
//! (`conformance/README.md`).
//!
//! `[deps]` are loaded as a graph (05 §33.2): breadth-first, each manifest's entries in
//! name order, path dependencies relative to the manifest that names them, and git
//! dependencies from the cache. The conformance harness has two conventions the command
//! line does not: a directory test needs no `ingot.toml` (its package is named after the
//! directory), and its `*.fixed.ingot` files are expected results, not modules. They are
//! enabled by `LoadOptions::harness`.

use crate::files::{Files, Overlays};
use crate::manifest::{self, DepSource, Dependency, Manifest, Member};
use crate::sha256;
use ingot_diag::diagnostic::Diagnostic;
use ingot_diag::source::{Source, Sources, Span};
use ingot_syntax::lexer::lex;
use ingot_syntax::token::TokenKind;
use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Default)]
pub struct LoadOptions {
    /// The conformance harness's conventions (see the module comment).
    pub harness: bool,
    /// Text used in place of files on disk (tools.md §6).
    pub overlays: Overlays,
    /// The directory git dependencies are read from, `<cache>/<64 hex>`. The command
    /// layer resolves `$INGOT_CACHE` or `~/.cache/ingot`; the loader reads no
    /// environment. `None` means there is no cache, so every git dependency is E1712.
    pub cache: Option<PathBuf>,
    /// The target being checked, for E1709. `None` skips that check.
    pub target: Option<String>,
}

#[derive(Clone, Debug)]
pub struct LoadedModule {
    /// Dotted path below the package: empty for the root module, `net.http` for
    /// `net/http.ingot`.
    pub rel: String,
    pub source_id: usize,
    /// False when a file or directory name on the way is not a value identifier
    /// (E1705); the module then cannot be named by an import.
    pub valid_name: bool,
    /// The file is not valid UTF-8 (E0101).
    pub unreadable: bool,
}

#[derive(Clone, Debug, Default)]
pub struct LoadedPackage {
    pub package_name: String,
    pub root: PathBuf,
    pub manifest: Option<Manifest>,
    /// The manifest is missing or has an error: the package name is not known and
    /// the package cannot be resolved.
    pub manifest_failed: bool,
    pub sources: Sources,
    pub modules: Vec<LoadedModule>,
    /// E0101, E17xx.
    pub diagnostics: Vec<Diagnostic>,
    /// Package features the compiler does not have yet.
    pub not_implemented: Vec<String>,
}

impl LoadedPackage {
    /// The dotted path of a module of this package.
    pub fn module_path(&self, module: &LoadedModule) -> String {
        if module.rel.is_empty() {
            self.package_name.clone()
        } else {
            format!("{}.{}", self.package_name, module.rel)
        }
    }
}

/// A loaded build: the packages of 05 §33.2 in graph order, plus the sources of a
/// workspace manifest that is not itself a package.
#[derive(Clone, Debug, Default)]
pub struct LoadedGraph {
    pub packages: Vec<LoadedPackage>,
    /// The package the command is about, if it is a package.
    pub root: Option<usize>,
    /// The packages `check` covers: the root, or every member of a workspace root
    /// (and the root package, when it is one).
    pub checked: Vec<usize>,
    /// Breadth-first graph order (05 §33.2).
    pub order: Vec<usize>,
    /// Direct dependencies of each package, as package indices.
    pub deps: Vec<Vec<usize>>,
    /// The nearest workspace root, when one applies.
    pub workspace: Option<PathBuf>,
    /// Sources of manifests that are not a package (a workspace root without a name).
    pub extra_sources: Sources,
    /// E17xx/E0303 diagnostics whose span is in a manifest of `extra_sources`.
    pub diagnostics: Vec<Diagnostic>,
    /// The chain of package names from a root to each package (05 §33.2).
    pub chains: Vec<Vec<String>>,
    /// The `[deps]` entry that reached each package (`[override]` applied).
    pub entries: Vec<Option<Dependency>>,
    /// True when the package was taken from the root's `[override]`.
    pub overridden: Vec<bool>,
    /// The manifest that holds each package's `[deps]` entry.
    pub entry_files: Vec<Option<String>>,
    /// Every dependency edge, with the entry that made it, for the cycle check.
    pub edge_entries: Vec<EdgeEntry>,
    /// The directory `deps` paths are shown relative to: the workspace root when one
    /// applies, else the root package's directory.
    pub display_root: PathBuf,
    /// The manifest whose `[override]` and `[deps]` the command's root names.
    pub root_manifest: Option<Manifest>,
    pub root_manifest_source: Option<Source>,
    pub root_manifest_dir: PathBuf,
}

impl LoadedGraph {
    /// The packages `check` runs over, in graph order.
    pub fn checked_in_order(&self) -> Vec<usize> {
        let mut checked = self.checked.clone();
        checked.sort_by_key(|index| {
            self.order
                .iter()
                .position(|p| p == index)
                .unwrap_or(usize::MAX)
        });
        checked
    }

    /// The packages in dependency order: every dependency before its dependent, ties
    /// by package name. `root` is last when given (pipeline §1).
    pub fn dependency_order(&self, root: Option<usize>) -> Vec<usize> {
        let mut order = Vec::new();
        let mut state: Vec<u8> = vec![0; self.packages.len()];
        fn visit(graph: &LoadedGraph, index: usize, state: &mut [u8], out: &mut Vec<usize>) {
            if state[index] != 0 {
                return;
            }
            state[index] = 1;
            let mut deps = graph.deps[index].clone();
            deps.sort_by(|a, b| {
                graph.packages[*a]
                    .package_name
                    .cmp(&graph.packages[*b].package_name)
            });
            deps.dedup();
            for dep in deps {
                visit(graph, dep, state, out);
            }
            state[index] = 2;
            out.push(index);
        }
        let mut roots: Vec<usize> = self.checked.clone();
        roots.sort_by(|a, b| {
            self.packages[*a]
                .package_name
                .cmp(&self.packages[*b].package_name)
        });
        for index in roots {
            visit(self, index, &mut state, &mut order);
        }
        if let Some(root) = root
            && let Some(position) = order.iter().position(|i| *i == root)
        {
            let last = order.remove(position);
            order.push(last);
        }
        order
    }
}

/// True if `name` is a value identifier that is not a keyword, a scalar type or a
/// vector type: something an `import` can name.
pub fn is_value_identifier(name: &str) -> bool {
    if name.is_empty() {
        return false;
    }
    let source = Source::new("name", name);
    let (tokens, diags) = lex(&source);
    diags.is_empty()
        && matches!(tokens.first().map(|t| &t.kind), Some(TokenKind::Ident(n)) if n == name)
        && tokens.len() <= 2
}

pub fn load(path: &Path) -> std::io::Result<LoadedPackage> {
    load_with(path, LoadOptions::default())
}

/// Load `path` as a package of its own: the root package of its graph. Dependencies
/// are loaded and checked by [`load_graph`], but this compatibility entry point
/// returns only the package itself.
pub fn load_with(path: &Path, options: LoadOptions) -> std::io::Result<LoadedPackage> {
    let graph = load_graph(path, options)?;
    match graph.root {
        Some(index) => Ok(graph.packages[index].clone()),
        None => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("{} is not a package", path.display()),
        )),
    }
}

/// The directory of the nearest `ingot.toml` at or above `path` (the package rule of
/// tools.md §1): the file's own directory, then its ancestors. `None` means `path` has
/// no manifest above it and is a package of its own. Overlays count, so an open,
/// unsaved `ingot.toml` creates a package for the adapter too.
pub fn manifest_dir(path: &Path, files: &Files) -> Option<PathBuf> {
    // A bare file name has the empty path as its parent, which means the current
    // directory. The walk starts from the absolute directory, so a relative path such
    // as `other.ingot` or `sub/other.ingot` still reaches a manifest above it.
    let start = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let start = start.canonicalize().unwrap_or_else(|_| start.to_path_buf());
    let mut dir = Some(start.as_path());
    while let Some(d) = dir {
        if files.is_dir(d) && files.exists(&d.join("ingot.toml")) {
            return Some(d.to_path_buf());
        }
        dir = d.parent();
    }
    None
}

/// Load the build at `path`: a package directory, a single file, or a workspace root.
pub fn load_graph(path: &Path, options: LoadOptions) -> std::io::Result<LoadedGraph> {
    let files = Files::new(options.overlays.clone());
    let context = match context(path, &files, options.harness)? {
        Some(context) => context,
        None => return load_single_file(path, &files),
    };
    let member_context = context.workspace.is_some()
        && context.members.is_empty()
        && context
            .root_package
            .as_ref()
            .is_some_and(|(dir, _)| context.workspace.as_ref() != Some(dir));
    let display_root = if member_context {
        context
            .root_package
            .as_ref()
            .map(|(dir, _)| dir.clone())
            .unwrap_or_else(|| PathBuf::from("."))
    } else {
        context
            .workspace
            .clone()
            .or_else(|| context.root_package.as_ref().map(|(dir, _)| dir.clone()))
            .or_else(|| context.members.first().map(|(dir, _)| dir.clone()))
            .unwrap_or_else(|| PathBuf::from("."))
    };
    let mut loader = Loader {
        files,
        harness: options.harness,
        cache: options.cache.clone(),
        target: options.target.clone(),
        graph: LoadedGraph::default(),
        by_dir: BTreeMap::new(),
        by_name: BTreeMap::new(),
        chains: Vec::new(),
        git_roots: Vec::new(),
        first_entry: Vec::new(),
        edge_entries: Vec::new(),
        queue: VecDeque::new(),
        overrides: context.overrides.clone(),
        root_manifest_dir: context.root_manifest_dir.clone(),
        std_dir: std_root(),
    };
    loader.graph.workspace = context.workspace.clone();
    loader.graph.display_root = display_root;
    loader.graph.root_manifest = context.root_manifest.clone();
    loader.graph.root_manifest_source = context.root_manifest_source.clone();
    loader.graph.root_manifest_dir = context.root_manifest_dir.clone();
    if let Some(source) = &context.workspace_source {
        loader.graph.extra_sources.add(source.clone());
        loader
            .graph
            .diagnostics
            .extend(context.workspace_diagnostics.iter().cloned());
    }
    let mut roots = Vec::new();
    if let Some((dir, prefix)) = &context.root_package {
        let index = loader.load_package(dir, prefix, None)?;
        roots.push(index);
    }
    for (dir, prefix) in &context.members {
        let index = loader.load_package(dir, prefix, None)?;
        if !roots.contains(&index) {
            roots.push(index);
        }
    }
    loader.graph.root = context
        .root_package
        .as_ref()
        .and_then(|_| roots.first().copied());
    loader.graph.checked = roots.clone();
    for index in &roots {
        if !loader.graph.order.contains(index) {
            loader.graph.order.push(*index);
        }
        if !loader.queue.contains(index) {
            loader.queue.push_back(*index);
        }
    }
    while let Some(index) = loader.queue.pop_front() {
        loader.walk_deps(index)?;
    }
    loader.check_cycles();
    loader.check_targets();
    loader.graph.edge_entries = loader.edge_entries;
    Ok(loader.graph)
}

/// A single file with no manifest above it: its own package named `t`.
fn load_single_file(path: &Path, files: &Files) -> std::io::Result<LoadedGraph> {
    let text = files.read_to_string(path)?;
    let display = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().replace('\\', "/"));
    let source = Source::with_absolute(display, path.to_path_buf(), text);
    let mut sources = Sources::default();
    sources.add(source);
    let package = LoadedPackage {
        package_name: "t".to_string(),
        root: path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf(),
        manifest: None,
        manifest_failed: false,
        sources,
        modules: vec![LoadedModule {
            rel: String::new(),
            source_id: 0,
            valid_name: true,
            unreadable: false,
        }],
        diagnostics: Vec::new(),
        not_implemented: Vec::new(),
    };
    Ok(LoadedGraph {
        packages: vec![package],
        root: Some(0),
        checked: vec![0],
        order: vec![0],
        deps: vec![Vec::new()],
        chains: vec![vec!["t".to_string()]],
        entries: vec![None],
        entry_files: vec![None],
        overridden: vec![false],
        display_root: path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf(),
        ..LoadedGraph::default()
    })
}

/// What the command line names: a root package, a workspace root's members, the
/// root `[override]` table, and the display prefixes.
struct Context {
    /// The package the command is about: `(canonical directory, display prefix)`.
    root_package: Option<(PathBuf, String)>,
    /// Workspace members, in the order `check` covers them.
    members: Vec<(PathBuf, String)>,
    /// The workspace root, when one applies.
    workspace: Option<PathBuf>,
    /// The workspace manifest's source, when the root is not a package.
    workspace_source: Option<Source>,
    workspace_diagnostics: Vec<Diagnostic>,
    /// The root's `[override]` table.
    overrides: Vec<Dependency>,
    /// The manifest directory that the root's `[override]` entries are relative to.
    root_manifest_dir: PathBuf,
    root_manifest: Option<Manifest>,
    root_manifest_source: Option<Source>,
}

/// Work out the package or workspace the argument names. `None` means a single file
/// with no manifest above it.
fn context(path: &Path, files: &Files, _harness: bool) -> std::io::Result<Option<Context>> {
    let is_dir = files.is_dir(path);
    let package_dir = if is_dir {
        canonical_dir(path)
    } else {
        match manifest_dir(path, files) {
            Some(dir) => canonical_dir(&dir),
            None => return Ok(None),
        }
    };
    // The nearest workspace above the package that lists it.
    let workspace = nearest_workspace(&package_dir, files)?;
    if let Some((root, manifest, source, diagnostics)) = workspace {
        let at_root = is_dir && canonical_dir(path) == root;
        let overrides = manifest.overrides.clone();
        let prefixes = member_prefixes(&root, &manifest, files)?;
        let member_dirs: Vec<PathBuf> = prefixes.iter().map(|(dir, _)| dir.clone()).collect();
        let listed = member_dirs.contains(&package_dir);
        if at_root {
            let mut members = Vec::new();
            for (dir, prefix) in prefixes {
                members.push((dir, prefix));
            }
            let root_package = manifest
                .name
                .as_ref()
                .map(|_| (root.clone(), String::new()));
            let workspace_source = if root_package.is_none() {
                Some(source.clone())
            } else {
                None
            };
            return Ok(Some(Context {
                root_package,
                members,
                workspace: Some(root.clone()),
                workspace_source,
                workspace_diagnostics: diagnostics,
                overrides,
                root_manifest_dir: root.clone(),
                root_manifest: Some(manifest.clone()),
                root_manifest_source: Some(source),
            }));
        }
        if listed {
            return Ok(Some(Context {
                root_package: Some((package_dir, String::new())),
                members: Vec::new(),
                workspace: Some(root.clone()),
                workspace_source: Some(source.clone()),
                workspace_diagnostics: diagnostics,
                overrides,
                root_manifest_dir: root.clone(),
                root_manifest: Some(manifest.clone()),
                root_manifest_source: Some(source),
            }));
        }
        // A closer workspace did not list this package: keep climbing.
    }
    let (overrides, root_manifest, root_manifest_source) =
        match read_manifest(&package_dir, files, "", false)? {
            Some(file) => (
                file.manifest.overrides.clone(),
                Some(file.manifest),
                Some(file.source),
            ),
            None => (Vec::new(), None, None),
        };
    Ok(Some(Context {
        root_package: Some((package_dir.clone(), String::new())),
        members: Vec::new(),
        workspace: None,
        workspace_source: None,
        workspace_diagnostics: Vec::new(),
        overrides,
        root_manifest_dir: package_dir,
        root_manifest,
        root_manifest_source,
    }))
}

type WorkspaceManifest = (PathBuf, Manifest, Source, Vec<Diagnostic>);

/// The nearest workspace above `package_dir` that lists it. A package's own
/// `[workspace]` is used only when no workspace above lists it: a member's own
/// `[workspace]` has no effect (05 §33.2).
fn nearest_workspace(
    package_dir: &Path,
    files: &Files,
) -> std::io::Result<Option<WorkspaceManifest>> {
    let mut current = package_dir.parent().map(Path::to_path_buf);
    while let Some(candidate) = current {
        if let Some(file) = read_manifest(&candidate, files, "", false)?
            && file.manifest.workspace.is_some()
        {
            let members = member_dirs(&candidate, &file.manifest, files)?;
            if members.iter().any(|member| member == package_dir) {
                return Ok(Some((
                    candidate,
                    file.manifest,
                    file.source,
                    file.diagnostics,
                )));
            }
        }
        current = candidate.parent().map(Path::to_path_buf);
    }
    if let Some(file) = read_manifest(package_dir, files, "", false)?
        && file.manifest.workspace.is_some()
    {
        return Ok(Some((
            package_dir.to_path_buf(),
            file.manifest,
            file.source,
            file.diagnostics,
        )));
    }
    Ok(None)
}

/// The members of a workspace, expanded and canonical, in listed order.
fn member_dirs(root: &Path, manifest: &Manifest, files: &Files) -> std::io::Result<Vec<PathBuf>> {
    Ok(member_prefixes(root, manifest, files)?
        .into_iter()
        .map(|(dir, _)| dir)
        .collect())
}

/// The members of a workspace with their display prefixes relative to the root.
fn member_prefixes(
    root: &Path,
    manifest: &Manifest,
    files: &Files,
) -> std::io::Result<Vec<(PathBuf, String)>> {
    let Some(workspace) = &manifest.workspace else {
        return Ok(Vec::new());
    };
    let mut members = Vec::new();
    for Member { pattern, star, .. } in &workspace.members {
        if *star {
            let parent = match pattern.rsplit_once('/') {
                Some((parent, _)) => root.join(parent),
                None => root.to_path_buf(),
            };
            if files.is_dir(&parent) {
                let mut entries = files.read_dir(&parent)?;
                entries.sort_by_key(|path| {
                    path.file_name()
                        .map(|name| name.to_string_lossy().as_bytes().to_vec())
                        .unwrap_or_default()
                });
                for entry in entries {
                    if files.is_dir(&entry) && files.exists(&entry.join("ingot.toml")) {
                        let dir = canonical_dir(&entry);
                        let prefix =
                            format!("{}/", relative_path(dir.strip_prefix(root).unwrap_or(&dir)));
                        members.push((dir, prefix));
                    }
                }
            }
        } else {
            let dir = canonical_dir(&root.join(pattern));
            let prefix = format!("{}/", relative_path(dir.strip_prefix(root).unwrap_or(&dir)));
            members.push((dir, prefix));
        }
    }
    let mut seen = Vec::new();
    members.retain(|(dir, _)| {
        if seen.contains(dir) {
            false
        } else {
            seen.push(dir.clone());
            true
        }
    });
    Ok(members)
}

/// A parsed manifest and the source it came from.
struct ManifestFile {
    manifest: Manifest,
    source: Source,
    diagnostics: Vec<Diagnostic>,
}

fn read_manifest(
    dir: &Path,
    files: &Files,
    prefix: &str,
    is_std: bool,
) -> std::io::Result<Option<ManifestFile>> {
    let path = dir.join("ingot.toml");
    if !files.exists(&path) {
        return Ok(None);
    }
    let text = files.read_to_string(&path)?;
    let source = Source::with_absolute(format!("{prefix}ingot.toml"), path, text);
    let (manifest, diagnostics) = manifest::parse_with(&source, is_std);
    Ok(Some(ManifestFile {
        manifest,
        source,
        diagnostics,
    }))
}

struct NameEntry {
    dir: PathBuf,
    file: String,
    span: Span,
    chain: Vec<String>,
}

struct FirstEntry {
    file: String,
    span: Span,
}

/// One dependency edge and the `[deps]`/`[override]` entry that made it.
#[derive(Clone, Debug)]
pub struct EdgeEntry {
    pub parent: usize,
    pub child: usize,
    pub dependency: Dependency,
    pub file: String,
}

struct Loader {
    files: Files,
    harness: bool,
    cache: Option<PathBuf>,
    target: Option<String>,
    graph: LoadedGraph,
    by_dir: BTreeMap<PathBuf, usize>,
    by_name: BTreeMap<String, NameEntry>,
    chains: Vec<Vec<String>>,
    git_roots: Vec<Option<PathBuf>>,
    /// The `[deps]` entry that first reached each package, for E1709.
    first_entry: Vec<Option<FirstEntry>>,
    /// Every dependency edge, for the whole-graph cycle check.
    edge_entries: Vec<EdgeEntry>,
    queue: VecDeque<usize>,
    overrides: Vec<Dependency>,
    root_manifest_dir: PathBuf,
    /// The standard library's directory, whose manifest alone may name `asm` sources.
    std_dir: Option<PathBuf>,
}

impl Loader {
    /// Load the package at `dir`. `prefix` is the display prefix of its sources (`""`
    /// for a root, `name/` for a dependency, `member/` in a workspace).
    fn load_package(
        &mut self,
        dir: &Path,
        prefix: &str,
        entry: Option<(&Dependency, &str)>,
    ) -> std::io::Result<usize> {
        let dir = canonical_dir(dir);
        if let Some(&index) = self.by_dir.get(&dir) {
            return Ok(index);
        }
        let is_std = self.std_dir.as_ref().is_some_and(|std| *std == dir);
        let file = read_manifest(&dir, &self.files, prefix, is_std)?;
        let package_name = file
            .as_ref()
            .and_then(|f| f.manifest.name.clone())
            .or_else(|| entry.map(|(dep, _)| dep.name.clone()))
            .unwrap_or_else(|| dir_name(&dir));
        let mut package = LoadedPackage {
            package_name: package_name.clone(),
            root: dir.clone(),
            ..LoadedPackage::default()
        };
        match file {
            Some(file) => {
                if !file.diagnostics.is_empty() {
                    package.manifest_failed = true;
                }
                package.diagnostics.extend(file.diagnostics);
                package.sources.add(file.source);
                package.manifest = Some(file.manifest);
            }
            None if self.harness => {}
            None => {
                let source = Source::with_absolute(
                    format!("{prefix}ingot.toml"),
                    dir.join("ingot.toml"),
                    String::new(),
                );
                package.diagnostics.push(Diagnostic::new(
                    "E1701",
                    "`ingot.toml` is missing",
                    &source,
                    Span::new(0, 0),
                ));
                package.sources.add(source);
                package.manifest_failed = true;
            }
        }
        collect_modules(&dir, &self.files, self.harness, prefix, &mut package)?;
        let chain = vec![package_name];
        let index = self.push_package(package, entry, None, chain);
        Ok(index)
    }

    fn push_package(
        &mut self,
        package: LoadedPackage,
        entry: Option<(&Dependency, &str)>,
        parent: Option<usize>,
        chain: Vec<String>,
    ) -> usize {
        let index = self.graph.packages.len();
        let name = package.package_name.clone();
        let root = canonical_dir(&package.root);
        self.graph.packages.push(package);
        self.graph.deps.push(Vec::new());
        self.by_dir.insert(root.clone(), index);
        self.by_name
            .entry(name.clone())
            .or_insert_with(|| NameEntry {
                dir: root,
                file: entry.map(|(_, file)| file.to_string()).unwrap_or_default(),
                span: entry.map(|(dep, _)| dep.span).unwrap_or_default(),
                chain: chain.clone(),
            });
        if entry.is_none()
            && let Some(file) = self.graph.packages[index].sources.files.first()
            && let Some(entry) = self.by_name.get_mut(&name)
        {
            entry.file = file.path.clone();
            entry.span = Span::new(0, 0);
        }
        self.graph.chains.push(chain.clone());
        self.chains.push(chain);
        let git_root = parent.and_then(|p| self.git_roots[p].clone());
        self.git_roots.push(git_root);
        self.first_entry.push(entry.map(|(dep, file)| FirstEntry {
            file: file.to_string(),
            span: dep.span,
        }));
        self.graph.entries.push(entry.map(|(dep, _)| dep.clone()));
        self.graph
            .entry_files
            .push(entry.map(|(_, file)| file.to_string()));
        self.graph.overridden.push(false);
        self.graph.order.push(index);
        if let Some(parent) = parent {
            if let Some((dependency, file)) = entry {
                self.record_edge(parent, index, dependency, file);
            } else {
                self.graph.deps[parent].push(index);
            }
        }
        self.queue.push_back(index);
        index
    }

    fn walk_deps(&mut self, index: usize) -> std::io::Result<()> {
        let Some(guard) = self.graph.packages[index].manifest.clone() else {
            return Ok(());
        };
        if self.graph.packages[index].manifest_failed {
            return Ok(());
        }
        let mut deps = guard.deps.clone();
        deps.sort_by(|a, b| a.name.as_bytes().cmp(b.name.as_bytes()));
        for listed in deps {
            let entry_file = self.graph.packages[index]
                .sources
                .files
                .first()
                .map(|s| s.path.clone())
                .unwrap_or_default();
            let override_entry = self.overrides.iter().find(|o| o.name == listed.name);
            let via_override = override_entry.is_some();
            let (dependency, entry_file) = match override_entry {
                Some(entry) => (
                    entry.clone(),
                    self.graph
                        .root_manifest_source
                        .as_ref()
                        .map(|source| source.path.clone())
                        .unwrap_or(entry_file),
                ),
                None => (listed.clone(), entry_file),
            };
            self.add_edge(index, &dependency, &entry_file, via_override)?;
        }
        Ok(())
    }

    fn add_edge(
        &mut self,
        parent: usize,
        dependency: &Dependency,
        parent_file: &str,
        via_override: bool,
    ) -> std::io::Result<()> {
        let target = match &dependency.source {
            DepSource::Path { rel, span } => {
                if Path::new(rel).is_absolute() {
                    if let Some(source) = self.source_for(parent, parent_file) {
                        self.graph.packages[parent]
                            .diagnostics
                            .push(Diagnostic::new(
                                "E1701",
                                format!(
                                    "the path dependency `{}` is not relative to the manifest",
                                    dependency.name
                                ),
                                &source,
                                *span,
                            ));
                    }
                    return Ok(());
                }
                let base = if via_override {
                    &self.root_manifest_dir
                } else {
                    &self.graph.packages[parent].root
                };
                let dir = canonical_dir(&base.join(rel));
                if let Some(git_root) = &self.git_roots[parent]
                    && !dir.starts_with(git_root)
                {
                    if let Some(source) = self.source_for(parent, parent_file) {
                        self.graph.packages[parent]
                            .diagnostics
                            .push(Diagnostic::new(
                                "E1701",
                                format!(
                                    "the path dependency `{}` leaves its git dependency's tree",
                                    dependency.name
                                ),
                                &source,
                                *span,
                            ));
                    }
                    return Ok(());
                }
                dir
            }
            DepSource::Git {
                hash, hash_span, ..
            } => {
                let hex = match hash.strip_prefix("sha256:") {
                    Some(hex) => hex,
                    None => return Ok(()),
                };
                let Some(cache) = &self.cache else {
                    self.missing_git(parent, dependency, parent_file);
                    let dir = self.root_manifest_dir.join(format!(".missing-{hex}"));
                    self.add_placeholder(parent, dependency, parent_file, dir);
                    return Ok(());
                };
                let dir = cache.join(hex);
                if !self.files.is_dir(&dir) {
                    self.missing_git(parent, dependency, parent_file);
                    self.add_placeholder(parent, dependency, parent_file, dir);
                    return Ok(());
                }
                let actual = sha256::hash_tree(&dir, &[])?;
                if &actual != hash {
                    if let Some(source) = self.source_for(parent, parent_file) {
                        self.graph.packages[parent].diagnostics.push(Diagnostic::new(
                            "E1703",
                            format!(
                                "the content of `{}` hashes to `{actual}`, not the pinned `{hash}`",
                                dependency.name
                            ),
                            &source,
                            *hash_span,
                        ));
                    }
                    self.add_placeholder(parent, dependency, parent_file, dir);
                    return Ok(());
                }
                dir
            }
        };
        // One package per name; two contents under one name are E1708 unless the
        // root's `[override]` picked one.
        if let Some(previous) = self.by_name.get(&dependency.name)
            && previous.dir != target
        {
            let previous_index = self.by_dir.get(&previous.dir).copied();
            let previous_file = previous.file.clone();
            let previous_span = previous.span;
            let previous_chain = previous.chain.clone();
            let mut chain = self.chains[parent].clone();
            chain.push(dependency.name.clone());
            if let Some(source) = self.source_for(parent, parent_file) {
                let mut diagnostic = Diagnostic::new(
                    "E1708",
                    format!(
                        "`{}` is reached from two chains with different contents: {} and {}",
                        dependency.name,
                        previous_chain.join(" -> "),
                        chain.join(" -> ")
                    ),
                    &source,
                    dependency.span,
                );
                diagnostic.relate_in("previous", &previous_file, previous_span);
                self.graph.packages[parent].diagnostics.push(diagnostic);
            }
            if let Some(previous_index) = previous_index {
                self.record_edge(parent, previous_index, dependency, parent_file);
            }
            return Ok(());
        }
        if let Some(&existing) = self.by_dir.get(&target) {
            self.check_dep_name(existing, dependency, parent, parent_file);
            self.record_edge(parent, existing, dependency, parent_file);
            return Ok(());
        }
        if !self.files.exists(&target.join("ingot.toml")) && !self.harness {
            if let Some(source) = self.source_for(parent, parent_file) {
                self.graph.packages[parent]
                    .diagnostics
                    .push(Diagnostic::new(
                        "E1701",
                        format!("the dependency `{}` has no `ingot.toml`", dependency.name),
                        &source,
                        dependency.span,
                    ));
            }
            self.add_placeholder(parent, dependency, parent_file, target);
            return Ok(());
        }
        let prefix = format!("{}/", dependency.name);
        let index = self.load_package(&target, &prefix, Some((dependency, parent_file)))?;
        self.check_dep_name(index, dependency, parent, parent_file);
        let child_chain = {
            let mut chain = self.chains[parent].clone();
            chain.push(dependency.name.clone());
            chain
        };
        self.chains[index] = child_chain.clone();
        if let Some(entry) = self.by_name.get_mut(&dependency.name)
            && entry.dir == target
        {
            entry.chain = child_chain;
        }
        // A git dependency's tree is the boundary its path dependencies stay inside.
        if matches!(dependency.source, DepSource::Git { .. }) {
            self.git_roots[index] = Some(target);
        }
        self.graph.chains[index] = self.chains[index].clone();
        self.graph.entries[index] = Some(dependency.clone());
        self.graph.overridden[index] = via_override;
        self.record_edge(parent, index, dependency, parent_file);
        Ok(())
    }

    /// Keep a package in the graph whose content could not be loaded, so `ingot deps`
    /// and the error reporting still see it (E1712, E1703, a missing manifest).
    fn add_placeholder(
        &mut self,
        parent: usize,
        dependency: &Dependency,
        parent_file: &str,
        dir: PathBuf,
    ) -> usize {
        let mut chain = self.chains[parent].clone();
        chain.push(dependency.name.clone());
        let package = LoadedPackage {
            package_name: dependency.name.clone(),
            root: dir,
            manifest_failed: true,
            ..LoadedPackage::default()
        };
        let index = self.push_package(
            package,
            Some((dependency, parent_file)),
            Some(parent),
            chain,
        );
        self.graph.overridden[index] = self.overrides.iter().any(|o| o.name == dependency.name);
        self.record_edge(parent, index, dependency, parent_file);
        index
    }

    /// Add the edge once and remember the entry that made it, for the cycle check.
    fn record_edge(&mut self, parent: usize, child: usize, dependency: &Dependency, file: &str) {
        if !self.graph.deps[parent].contains(&child) {
            self.graph.deps[parent].push(child);
        }
        if !self
            .edge_entries
            .iter()
            .any(|edge| edge.parent == parent && edge.child == child)
        {
            self.edge_entries.push(EdgeEntry {
                parent,
                child,
                dependency: dependency.clone(),
                file: file.to_string(),
            });
        }
    }

    fn check_dep_name(
        &mut self,
        index: usize,
        dependency: &Dependency,
        parent: usize,
        parent_file: &str,
    ) {
        let name = self.graph.packages[index].package_name.clone();
        if name == dependency.name {
            return;
        }
        if let Some(source) = self.source_for(parent, parent_file) {
            self.graph.packages[parent]
                .diagnostics
                .push(Diagnostic::new(
                    "E1701",
                    format!(
                        "the dependency key `{}` does not equal the package's `name`, `{name}`",
                        dependency.name
                    ),
                    &source,
                    dependency.span,
                ));
        }
    }

    fn missing_git(&mut self, parent: usize, dependency: &Dependency, parent_file: &str) {
        if let Some(source) = self.source_for(parent, parent_file) {
            self.graph.packages[parent]
                .diagnostics
                .push(Diagnostic::new(
                    "E1712",
                    format!(
                        "the git dependency `{}` is not in the cache; run `ingot deps fetch`",
                        dependency.name
                    ),
                    &source,
                    dependency.span,
                ));
        }
    }

    /// The source of `parent`'s manifest named `file`.
    fn source_for(&self, parent: usize, file: &str) -> Option<Source> {
        self.graph.packages[parent]
            .sources
            .iter()
            .find(|source| source.path == file)
            .cloned()
            .or_else(|| {
                self.graph
                    .root_manifest_source
                    .clone()
                    .filter(|source| source.path == file)
            })
            .or_else(|| {
                self.graph
                    .extra_sources
                    .iter()
                    .find(|source| source.path == file)
                    .cloned()
            })
    }

    /// E0303 for the first back edge in a depth-first walk of the whole graph, in
    /// name order of each package's dependencies (05 §33.2).
    fn check_cycles(&mut self) {
        let count = self.graph.packages.len();
        let mut color = vec![0u8; count];
        let mut roots = self.graph.checked.clone();
        if let Some(root) = self.graph.root
            && !roots.contains(&root)
        {
            roots.push(root);
        }
        roots.sort_by_key(|index| {
            self.graph
                .order
                .iter()
                .position(|item| item == index)
                .unwrap_or(usize::MAX)
        });
        let mut stack = Vec::new();
        for start in roots {
            if color[start] != 0 {
                continue;
            }
            if let Some(cycle) = self.find_cycle(start, &mut color, &mut stack) {
                self.report_cycle(&cycle);
                return;
            }
        }
        for start in 0..count {
            if color[start] != 0 {
                continue;
            }
            if let Some(cycle) = self.find_cycle(start, &mut color, &mut stack) {
                self.report_cycle(&cycle);
                return;
            }
        }
    }

    /// The cycle found by one DFS, as the package indices on it, first repeated last.
    fn find_cycle(
        &self,
        node: usize,
        color: &mut [u8],
        stack: &mut Vec<usize>,
    ) -> Option<Vec<usize>> {
        color[node] = 1;
        stack.push(node);
        let mut children = self.graph.deps[node].clone();
        children.sort_by(|a, b| {
            self.graph.packages[*a]
                .package_name
                .cmp(&self.graph.packages[*b].package_name)
                .then(a.cmp(b))
        });
        children.dedup();
        for child in children {
            match color[child] {
                0 => {
                    if let Some(cycle) = self.find_cycle(child, color, stack) {
                        return Some(cycle);
                    }
                }
                1 => {
                    let at = stack.iter().position(|item| *item == child)?;
                    let mut cycle = stack[at..].to_vec();
                    cycle.push(child);
                    return Some(cycle);
                }
                _ => {}
            }
        }
        stack.pop();
        color[node] = 2;
        None
    }

    fn report_cycle(&mut self, cycle: &[usize]) {
        let parent = cycle[cycle.len() - 2];
        let child = cycle[cycle.len() - 1];
        let Some(edge) = self
            .edge_entries
            .iter()
            .find(|edge| edge.parent == parent && edge.child == child)
            .cloned()
        else {
            return;
        };
        let names: Vec<String> = cycle
            .iter()
            .map(|index| self.graph.packages[*index].package_name.clone())
            .collect();
        let message = format!("the packages form a cycle: {}", names.join(" -> "));
        let source = self
            .graph
            .packages
            .iter()
            .find_map(|package| {
                package
                    .sources
                    .iter()
                    .find(|source| source.path == edge.file)
                    .cloned()
            })
            .or_else(|| self.graph.root_manifest_source.clone());
        if let Some(source) = source {
            self.graph.packages[parent]
                .diagnostics
                .push(Diagnostic::new(
                    "E0303",
                    message,
                    &source,
                    edge.dependency.span,
                ));
        }
    }

    /// E1709 for every package that does not list the target being checked.
    fn check_targets(&mut self) {
        let Some(target) = self.target.clone() else {
            return;
        };
        for index in 0..self.graph.packages.len() {
            let Some(manifest) = self.graph.packages[index].manifest.clone() else {
                continue;
            };
            if manifest.target_list().contains(&target.as_str()) {
                continue;
            }
            let package_name = self.graph.packages[index].package_name.clone();
            let message =
                format!("the package `{package_name}` does not list the target `{target}`");
            let entry = self.first_entry[index]
                .as_ref()
                .map(|entry| (entry.file.clone(), entry.span));
            match entry {
                Some((file, span)) => {
                    let source = self
                        .graph
                        .packages
                        .iter()
                        .find_map(|pkg| pkg.sources.iter().find(|s| s.path == file).cloned())
                        .or_else(|| {
                            self.graph
                                .root_manifest_source
                                .clone()
                                .filter(|source| source.path == file)
                        })
                        .or_else(|| {
                            self.graph
                                .extra_sources
                                .iter()
                                .find(|source| source.path == file)
                                .cloned()
                        });
                    if let Some(source) = source {
                        self.graph.packages[index]
                            .diagnostics
                            .push(Diagnostic::new("E1709", message, &source, span));
                    }
                }
                None => {
                    let span = manifest.targets_span.unwrap_or(Span::new(0, 0));
                    let source = self.graph.packages[index].sources.files.first().cloned();
                    if let Some(source) = source {
                        self.graph.packages[index]
                            .diagnostics
                            .push(Diagnostic::new("E1709", message, &source, span));
                    }
                }
            }
        }
    }
}

fn canonical_dir(path: &Path) -> PathBuf {
    path.canonicalize()
        .unwrap_or_else(|_| crate::files::absolute_path(path))
}

fn dir_name(dir: &Path) -> String {
    dir.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "t".to_string())
}

/// Every `.ingot` file below `dir`, except those inside a nested package, as modules.
fn collect_modules(
    dir: &Path,
    files: &Files,
    harness: bool,
    prefix: &str,
    package: &mut LoadedPackage,
) -> std::io::Result<()> {
    let mut module_files = Vec::new();
    collect_ingot(dir, dir, files, &mut module_files)?;
    module_files.sort();
    for file in module_files {
        let rel = file.strip_prefix(dir).unwrap_or(&file);
        let rel_path = relative_path(rel);
        if harness && rel_path.ends_with(".fixed.ingot") {
            continue;
        }
        let components = module_components(rel);
        let valid_name = components.iter().all(|c| is_value_identifier(c));
        let rel_module =
            if components.len() == 1 && components[0] == "main" && rel.components().count() == 1 {
                String::new()
            } else {
                components.join(".")
            };
        let display = format!("{prefix}{rel_path}");
        let bytes = files.read(&file)?;
        let (text, unreadable, bad) = match String::from_utf8(bytes) {
            Ok(t) => (t, false, 0),
            Err(e) => {
                let bad = e.utf8_error().valid_up_to();
                (
                    String::from_utf8_lossy(e.as_bytes()).into_owned(),
                    true,
                    bad,
                )
            }
        };
        let source = Source::with_absolute(display, file.clone(), text);
        if unreadable {
            package.diagnostics.push(Diagnostic::new(
                "E0101",
                "file is not valid UTF-8",
                &source,
                Span::new(bad, bad + 1),
            ));
        }
        if !valid_name {
            package.diagnostics.push(Diagnostic::new(
                "E1705",
                format!(
                    "`{}` is not a module name: every file and directory name on the path must be a lowercase identifier that is not a keyword",
                    rel_path
                ),
                &source,
                Span::new(0, 0),
            ));
        }
        let id = package.sources.add(source);
        package.modules.push(LoadedModule {
            rel: rel_module,
            source_id: id,
            valid_name,
            unreadable,
        });
    }
    package
        .modules
        .sort_by(|a, b| a.rel.cmp(&b.rel).then(a.source_id.cmp(&b.source_id)));
    Ok(())
}

/// Every `.ingot` file below `dir`, stopping at a nested `ingot.toml` (05 §33.2).
fn collect_ingot(
    root: &Path,
    dir: &Path,
    files: &Files,
    out: &mut Vec<PathBuf>,
) -> std::io::Result<()> {
    let mut entries = files.read_dir(dir)?;
    entries.sort();
    for path in entries {
        let Some(name) = path.file_name().map(|n| n.to_string_lossy().into_owned()) else {
            continue;
        };
        if files.is_dir(&path) {
            if matches!(name.as_str(), "target" | ".git") || name.starts_with('.') {
                continue;
            }
            if path != root && files.exists(&path.join("ingot.toml")) {
                continue;
            }
            collect_ingot(root, &path, files, out)?;
        } else if path.extension().map(|x| x == "ingot").unwrap_or(false) {
            out.push(path);
        }
    }
    Ok(())
}

/// The names that make up a module path: directories, then the file name without its
/// extension. `net/http.ingot` is `["net", "http"]`.
fn module_components(rel: &Path) -> Vec<String> {
    let mut parts: Vec<String> = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    if let Some(last) = parts.pop() {
        let stem = last.strip_suffix(".ingot").unwrap_or(&last).to_string();
        parts.push(stem);
    }
    parts
}

pub(crate) fn relative_path(rel: &Path) -> String {
    rel.components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

/// Load a package directory without the graph: the standard library, whose manifest
/// alone may name `asm` sources (05 §33.2).
pub fn load_dir(dir: &Path, options: LoadOptions, prefix: &str) -> std::io::Result<LoadedPackage> {
    let files = Files::new(options.overlays);
    load_standalone(dir, &files, options.harness, prefix, true)
}

fn load_standalone(
    dir: &Path,
    files: &Files,
    harness: bool,
    prefix: &str,
    is_std: bool,
) -> std::io::Result<LoadedPackage> {
    let dir = canonical_dir(dir);
    let mut package = LoadedPackage {
        package_name: dir_name(&dir),
        root: dir.clone(),
        ..LoadedPackage::default()
    };
    match read_manifest(&dir, files, prefix, is_std)? {
        Some(file) => {
            if !file.diagnostics.is_empty() {
                package.manifest_failed = true;
            }
            package.diagnostics.extend(file.diagnostics);
            package.package_name = file.manifest.name.clone().unwrap_or_else(|| dir_name(&dir));
            package.sources.add(file.source);
            package.manifest = Some(file.manifest);
        }
        None if harness => {}
        None => {
            let source = Source::with_absolute(
                format!("{prefix}ingot.toml"),
                dir.join("ingot.toml"),
                String::new(),
            );
            package.diagnostics.push(Diagnostic::new(
                "E1701",
                "`ingot.toml` is missing",
                &source,
                Span::new(0, 0),
            ));
            package.sources.add(source);
            package.manifest_failed = true;
        }
    }
    collect_modules(&dir, files, harness, prefix, &mut package)?;
    Ok(package)
}

/// Where the standard library's source is: `$INGOT_STD`, then next to the
/// executable, then relative to this crate's source tree.
pub fn std_root() -> Option<PathBuf> {
    std_root_with(&Files::default()).map(|root| root.canonicalize().unwrap_or(root))
}

fn std_root_with(files: &Files) -> Option<PathBuf> {
    if let Ok(path) = std::env::var("INGOT_STD") {
        let path = PathBuf::from(path);
        if files.exists(&path.join("ingot.toml")) {
            return Some(path);
        }
    }
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        for candidate in [dir.join("std"), dir.join("../std"), dir.join("../../std")] {
            if files.exists(&candidate.join("ingot.toml")) {
                return Some(candidate);
            }
        }
    }
    let development = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../std");
    files
        .exists(&development.join("ingot.toml"))
        .then_some(development)
}

/// Load the standard library as a package, with the overlays of `options`. Its files
/// are shown as `std/...`.
pub fn load_std(options: &LoadOptions) -> std::io::Result<LoadedPackage> {
    let files = Files::new(options.overlays.clone());
    let Some(root) = std_root_with(&files) else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "cannot find the standard library (a directory `std` with an `ingot.toml`); set INGOT_STD",
        ));
    };
    load_standalone(&root, &files, false, "std/", true)
}
