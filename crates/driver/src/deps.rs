//! `ingot deps`, `ingot deps fetch` and `ingot deps update` (05 §33.2, tools.md §4).
//!
//! `deps` reads the cache like `check` does. `fetch` and `update` are the two commands
//! that use the network: they run the system `git` through `std::process::Command`,
//! check out into a temporary directory inside the cache, drop `.git`, hash the tree
//! and only then rename it into place, so a half-fetched tree never sits under a hash
//! name.

use crate::cache;
use crate::commands::{self, Output};
use crate::files::Overlays;
use crate::loader::{self, LoadedGraph};
use crate::manifest::DepSource;
use crate::sha256;
use ingot_diag::diagnostic::json_string;
use ingot_diag::source::{Source, Sources, Span};
use std::path::Path;
use std::process::Command;
use std::time::Instant;

/// `ingot deps [PATH]`: one `dep` line per package of the graph, other than the root
/// and `std`, in graph order.
pub fn list(path: &Path, overlays: &Overlays) -> Output {
    let started = Instant::now();
    let graph = match load(path, overlays) {
        Ok(graph) => graph,
        Err(error) => return commands::failure("deps", error.to_string(), started),
    };
    let sources = graph_sources(&graph);
    let diagnostics = load_diagnostics(&graph);
    let mut lines = Vec::new();
    let mut human = String::new();
    for diagnostic in &diagnostics {
        lines.push(diagnostic.to_json(&sources));
        human.push_str(&diagnostic.to_human(&sources));
    }
    let mut count = 0usize;
    for &index in &graph.order {
        if Some(index) == graph.root {
            continue;
        }
        let Some(entry) = &graph.entries[index] else {
            continue;
        };
        let package = &graph.packages[index];
        if package.package_name == "std" {
            continue;
        }
        count += 1;
        lines.push(dep_line(
            &package.package_name,
            entry,
            &graph.chains[index][..graph.chains[index].len().saturating_sub(1)],
            graph.overridden[index],
            Some(&graph.display_root),
            Some(&package.root),
        ));
        human.push_str(&format!(
            "{} {}\n",
            package.package_name,
            entry.path().unwrap_or("git")
        ));
    }
    let ok = diagnostics.is_empty();
    lines.push(summary(ok, count, started));
    Output {
        lines,
        human,
        error: None,
        not_implemented: Vec::new(),
        exit: if ok { 0 } else { 1 },
    }
}

/// `ingot deps fetch`: fill the cache for every git dependency of the graph. It never
/// edits a manifest. Fetching can reveal further git dependencies (a fetched package
/// has its own manifest), so the graph is loaded again after every round.
pub fn fetch(path: &Path, overlays: &Overlays) -> Output {
    let started = Instant::now();
    let mut attempted: Vec<GitPin> = Vec::new();
    let mut lines = Vec::new();
    let mut human = String::new();
    let mut errors = 0usize;
    let graph = loop {
        let graph = match load(path, overlays) {
            Ok(graph) => graph,
            Err(error) => return commands::failure("deps", error.to_string(), started),
        };
        let pins = git_pins(&graph);
        let missing: Vec<&GitPin> = pins
            .iter()
            .filter(|pin| !attempted.contains(pin) && !cache_ok(pin))
            .collect();
        if missing.is_empty() {
            break graph;
        }
        let mut fetched_any = false;
        for pin in missing {
            attempted.push(pin.clone());
            match fetch_one(&pin.url, &pin.rev, &pin.hash) {
                Ok(_) => fetched_any = true,
                Err(FetchError::Tool(message)) => {
                    errors += 1;
                    lines.push(commands::error_line(&message));
                    human.push_str(&format!("{message}\n"));
                }
                Err(FetchError::Hash(actual)) => {
                    errors += 1;
                    let (source, span) = graph
                        .entries
                        .iter()
                        .enumerate()
                        .find_map(|(index, entry)| {
                            let entry = entry.as_ref()?;
                            let DepSource::Git {
                                url,
                                rev,
                                hash_span,
                                ..
                            } = &entry.source
                            else {
                                return None;
                            };
                            if url == &pin.url && rev == &pin.rev {
                                Some((entry_source(&graph, index), *hash_span))
                            } else {
                                None
                            }
                        })
                        .unwrap_or((None, Span::default()));
                    let source = source.unwrap_or_else(|| {
                        graph
                            .root_manifest_source
                            .clone()
                            .unwrap_or_else(|| Source::new("ingot.toml", ""))
                    });
                    let diagnostic = ingot_diag::diagnostic::Diagnostic::new(
                        "E1703",
                        format!(
                            "the fetched content of `{}` hashes to `{actual}`, not the pinned `{}`",
                            pin.name, pin.hash
                        ),
                        &source,
                        span,
                    );
                    lines.push(diagnostic.to_json(&graph_sources(&graph)));
                    human.push_str(&diagnostic.to_human(&graph_sources(&graph)));
                }
            }
        }
        if !fetched_any {
            break graph;
        }
    };
    let sources = graph_sources(&graph);
    let diagnostics = load_diagnostics(&graph);
    for diagnostic in &diagnostics {
        lines.push(diagnostic.to_json(&sources));
        human.push_str(&diagnostic.to_human(&sources));
    }
    let ok = errors == 0 && diagnostics.is_empty();
    let packages = git_pins(&graph).len();
    lines.push(summary(ok, packages, started));
    Output {
        lines,
        human,
        error: None,
        not_implemented: Vec::new(),
        exit: if ok { 0 } else { 1 },
    }
}

/// `ingot deps update NAME [--rev R]`: fetch `NAME`, compute its hash, and rewrite the
/// `rev` and `hash` values in the root's `[override]` when it has one, otherwise in
/// every git entry for `NAME` in the manifests reached from the root through path
/// dependencies alone.
pub fn update(path: &Path, name: &str, rev: Option<&str>, overlays: &Overlays) -> Output {
    let started = Instant::now();
    let graph = match load(path, overlays) {
        Ok(graph) => graph,
        Err(error) => return commands::failure("deps", error.to_string(), started),
    };
    let mut edits: Vec<ManifestEdit> = Vec::new();
    let override_entry = graph
        .root_manifest
        .as_ref()
        .and_then(|manifest| manifest.overrides.iter().find(|entry| entry.name == name));
    let overridden = override_entry.is_some();
    if let Some(entry) = override_entry
        && let DepSource::Git {
            url,
            rev: entry_rev,
            rev_span,
            hash_span,
            ..
        } = &entry.source
        && let Some(source) = &graph.root_manifest_source
    {
        edits.push(ManifestEdit {
            source: source.clone(),
            url: url.clone(),
            rev: entry_rev.clone(),
            rev_span: *rev_span,
            hash_span: *hash_span,
        });
    }
    if !overridden {
        // At a workspace root the members are roots of the build too.
        let mut roots: Vec<usize> = if graph.workspace.is_some() {
            graph.checked.clone()
        } else {
            graph.root.into_iter().collect()
        };
        if roots.is_empty() {
            return no_entry(name, started);
        }
        roots.sort_by_key(|index| {
            graph
                .order
                .iter()
                .position(|item| item == index)
                .unwrap_or(usize::MAX)
        });
        let mut queue = roots.clone();
        let mut visited = roots;
        while let Some(index) = queue.pop() {
            if let Some(manifest) = &graph.packages[index].manifest
                && let Some(source) = graph.packages[index].sources.files.first()
            {
                for entry in &manifest.deps {
                    if entry.name != name {
                        continue;
                    }
                    if let DepSource::Git {
                        url,
                        rev: entry_rev,
                        rev_span,
                        hash_span,
                        ..
                    } = &entry.source
                    {
                        edits.push(ManifestEdit {
                            source: source.clone(),
                            url: url.clone(),
                            rev: entry_rev.clone(),
                            rev_span: *rev_span,
                            hash_span: *hash_span,
                        });
                    }
                }
            }
            for &dep in &graph.deps[index] {
                let via_path = graph.entries[dep]
                    .as_ref()
                    .is_some_and(|entry| entry.path().is_some());
                if via_path && !visited.contains(&dep) {
                    visited.push(dep);
                    queue.push(dep);
                }
            }
        }
    }
    if edits.is_empty() {
        return no_entry(name, started);
    }
    let new_rev = rev
        .map(str::to_string)
        .unwrap_or_else(|| edits[0].rev.clone());
    let url = edits[0].url.clone();
    let fetched = match fetch_one(&url, &new_rev, "") {
        Ok(fetched) => fetched,
        Err(FetchError::Tool(message)) => return commands::failure("deps", message, started),
        Err(FetchError::Hash(_)) => {
            return commands::failure("deps", "fetch produced no hash".to_string(), started);
        }
    };
    let commit = fetched.commit;
    let hash = fetched.hash;
    for edit in &edits {
        if let Err(error) = apply(&edit.source, edit.rev_span, &commit, edit.hash_span, &hash) {
            return commands::failure("deps", error.to_string(), started);
        }
    }
    let via = graph
        .packages
        .iter()
        .position(|package| package.package_name == name)
        .map(|index| {
            let chain = &graph.chains[index];
            chain[..chain.len().saturating_sub(1)].to_vec()
        })
        .unwrap_or_default();
    let entry = crate::manifest::Dependency {
        name: name.to_string(),
        source: DepSource::Git {
            url,
            rev: commit.clone(),
            hash: hash.clone(),
            rev_span: Span::default(),
            hash_span: Span::default(),
        },
        span: Span::default(),
    };
    let mut lines = vec![dep_line(name, &entry, &via, overridden, None, None)];
    lines.push(summary(true, 1, started));
    Output {
        lines,
        human: format!("{name} {commit} {hash}\n"),
        error: None,
        not_implemented: Vec::new(),
        exit: 0,
    }
}

struct ManifestEdit {
    source: Source,
    url: String,
    rev: String,
    rev_span: Span,
    hash_span: Span,
}

fn apply(
    source: &Source,
    rev_span: Span,
    rev: &str,
    hash_span: Span,
    hash: &str,
) -> std::io::Result<()> {
    let rev = toml_value(source, rev_span, rev);
    let hash = toml_value(source, hash_span, hash);
    let mut edits = [(rev_span, rev.as_str()), (hash_span, hash.as_str())];
    edits.sort_by_key(|(span, _)| span.start);
    let mut out = String::new();
    let mut position = 0;
    for (span, text) in edits {
        if span.start < position || span.end > source.text.len() {
            continue;
        }
        out.push_str(&source.text[position..span.start]);
        out.push_str(text);
        position = span.end;
    }
    out.push_str(&source.text[position..]);
    if out != source.text {
        std::fs::write(&source.absolute, out)?;
    }
    Ok(())
}

/// Keep the quote style the manifest used for this value.
fn toml_value(source: &Source, span: Span, text: &str) -> String {
    if source.text.as_bytes().get(span.start) == Some(&b'\'') {
        format!("'{text}'")
    } else {
        toml_string(text)
    }
}

fn toml_string(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            other => out.push(other),
        }
    }
    out.push('"');
    out
}

fn no_entry(name: &str, started: Instant) -> Output {
    commands::failure(
        "deps",
        format!("no git `[deps]` or `[override]` entry for `{name}`"),
        started,
    )
}

fn load(path: &Path, overlays: &Overlays) -> std::io::Result<LoadedGraph> {
    let options = loader::LoadOptions {
        overlays: overlays.clone(),
        cache: cache::default_cache(),
        ..loader::LoadOptions::default()
    };
    loader::load_graph(path, options)
}

fn load_diagnostics(graph: &LoadedGraph) -> Vec<ingot_diag::diagnostic::Diagnostic> {
    let mut diagnostics = graph.diagnostics.clone();
    for package in &graph.packages {
        diagnostics.extend(package.diagnostics.iter().cloned());
    }
    diagnostics
}

fn entry_source(graph: &LoadedGraph, index: usize) -> Option<Source> {
    let file = graph.entry_files.get(index)?.as_ref()?;
    graph
        .packages
        .iter()
        .find_map(|package| {
            package
                .sources
                .iter()
                .find(|source| &source.path == file)
                .cloned()
        })
        .or_else(|| {
            graph
                .root_manifest_source
                .clone()
                .filter(|source| source.path == *file)
        })
}

fn graph_sources(graph: &LoadedGraph) -> Sources {
    let mut sources = Sources::default();
    for package in &graph.packages {
        for source in &package.sources.files {
            sources.add(source.clone());
        }
    }
    for source in &graph.extra_sources.files {
        sources.add(source.clone());
    }
    if let Some(source) = &graph.root_manifest_source {
        sources.add(source.clone());
    }
    sources
}

fn dep_line(
    name: &str,
    entry: &crate::manifest::Dependency,
    via: &[String],
    overridden: bool,
    display_root: Option<&Path>,
    package_root: Option<&Path>,
) -> String {
    let mut line = String::from("{\"type\":\"dep\",\"name\":");
    json_string(&mut line, name);
    line.push_str(",\"source\":");
    match &entry.source {
        DepSource::Path { .. } => {
            json_string(&mut line, "path");
            if let (Some(root), Some(package)) = (display_root, package_root) {
                line.push_str(",\"path\":");
                json_string(&mut line, &relative_to(root, package));
            }
        }
        DepSource::Git { rev, hash, .. } => {
            json_string(&mut line, "git");
            line.push_str(",\"rev\":");
            json_string(&mut line, rev);
            line.push_str(",\"hash\":");
            json_string(&mut line, hash);
        }
    }
    line.push_str(",\"via\":[");
    for (i, package) in via.iter().enumerate() {
        if i > 0 {
            line.push(',');
        }
        json_string(&mut line, package);
    }
    line.push(']');
    if overridden {
        line.push_str(",\"override\":true");
    }
    line.push('}');
    line
}

/// `path` relative to `root` with `/` separators, `..` where needed.
fn relative_to(root: &Path, path: &Path) -> String {
    let mut root_parts: Vec<String> = root
        .components()
        .map(|part| part.as_os_str().to_string_lossy().into_owned())
        .collect();
    let mut path_parts: Vec<String> = path
        .components()
        .map(|part| part.as_os_str().to_string_lossy().into_owned())
        .collect();
    while !root_parts.is_empty() && !path_parts.is_empty() && root_parts[0] == path_parts[0] {
        root_parts.remove(0);
        path_parts.remove(0);
    }
    let mut out: Vec<String> = std::iter::repeat_n("..".to_string(), root_parts.len()).collect();
    out.extend(path_parts);
    if out.is_empty() {
        ".".to_string()
    } else {
        out.join("/")
    }
}

fn summary(ok: bool, packages: usize, started: Instant) -> String {
    format!(
        "{{\"type\":\"summary\",\"command\":\"deps\",\"ok\":{ok},\"protocol\":1,\"packages\":{packages},\"ms\":{}}}",
        started.elapsed().as_millis()
    )
}

/// One git dependency of the graph, deduplicated by its content pin.
#[derive(Clone, Debug, PartialEq, Eq)]
struct GitPin {
    name: String,
    url: String,
    rev: String,
    hash: String,
}

fn git_pins(graph: &LoadedGraph) -> Vec<GitPin> {
    let mut pins: Vec<GitPin> = Vec::new();
    for &index in &graph.order {
        let Some(entry) = &graph.entries[index] else {
            continue;
        };
        let DepSource::Git { url, rev, hash, .. } = &entry.source else {
            continue;
        };
        let pin = GitPin {
            name: graph.packages[index].package_name.clone(),
            url: url.clone(),
            rev: rev.clone(),
            hash: hash.clone(),
        };
        if !pins.contains(&pin) {
            pins.push(pin);
        }
    }
    pins
}

fn cache_ok(pin: &GitPin) -> bool {
    let Some(cache) = cache::default_cache() else {
        return false;
    };
    let Some(hex) = pin.hash.strip_prefix("sha256:") else {
        return false;
    };
    sha256::hash_tree(&cache.join(hex), &[]).is_ok_and(|actual| actual == pin.hash)
}

enum FetchError {
    Tool(String),
    /// The fetched tree's hash differs from the manifest's pinned hash.
    Hash(String),
}

struct Fetched {
    /// The commit the checkout is at, for `deps update`.
    commit: String,
    hash: String,
}

/// Fetch `rev` from `url` into the cache under its content hash. `expected` is the
/// manifest's pinned hash, checked when it is not empty.
fn fetch_one(url: &str, rev: &str, expected: &str) -> Result<Fetched, FetchError> {
    let Some(cache) = cache::default_cache() else {
        return Err(FetchError::Tool(
            "no cache directory: set `$INGOT_CACHE` or `$HOME`".to_string(),
        ));
    };
    if !expected.is_empty()
        && let Some(hex) = expected.strip_prefix("sha256:")
        && let Ok(actual) = sha256::hash_tree(&cache.join(hex), &[])
        && actual == expected
    {
        return Ok(Fetched {
            commit: rev.to_string(),
            hash: actual,
        });
    }
    std::fs::create_dir_all(&cache).map_err(|error| {
        FetchError::Tool(format!("could not create {}: {error}", cache.display()))
    })?;
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let temp = cache.join(format!(
        ".tmp-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&temp);
    std::fs::create_dir_all(&temp).map_err(|error| {
        FetchError::Tool(format!("could not create {}: {error}", temp.display()))
    })?;
    let result = (|| {
        run_git(&temp, &["init", "-q"])?;
        run_git(&temp, &["remote", "add", "origin", url])?;
        // The direct fetch handles a branch or tag name. A commit hash the server
        // will not hand out directly is reached through the full ref set.
        let fetched = if run_git(&temp, &["fetch", "-q", "--depth", "1", "origin", rev]).is_err() {
            run_git(&temp, &["fetch", "-q", "origin"])?;
            rev.to_string()
        } else {
            "FETCH_HEAD".to_string()
        };
        run_git_checkout(&temp, &fetched)?;
        let commit = run_git_output(&temp, &["rev-parse", "HEAD"])?;
        std::fs::remove_dir_all(temp.join(".git")).map_err(|error| {
            FetchError::Tool(format!("could not drop {}/.git: {error}", temp.display()))
        })?;
        let hash = sha256::hash_tree(&temp, &[])
            .map_err(|error| FetchError::Tool(format!("could not hash the tree: {error}")))?;
        let hex = hash.strip_prefix("sha256:").unwrap_or(&hash).to_string();
        let destination = cache.join(&hex);
        if destination.exists() {
            let _ = std::fs::remove_dir_all(&temp);
            if !expected.is_empty() && expected != hash {
                return Err(FetchError::Hash(hash));
            }
            return Ok(Fetched { commit, hash });
        }
        std::fs::rename(&temp, &destination).map_err(|error| {
            FetchError::Tool(format!(
                "could not move the fetched tree to {}: {error}",
                destination.display()
            ))
        })?;
        if !expected.is_empty() && expected != hash {
            return Err(FetchError::Hash(hash));
        }
        Ok(Fetched { commit, hash })
    })();
    if result.is_err() {
        let _ = std::fs::remove_dir_all(&temp);
    }
    result
}

/// A checkout must not depend on the user's git configuration: a global filter,
/// `autocrlf` or a system setting could change the bytes and so the hash.
fn run_git_checkout(dir: &Path, rev: &str) -> Result<(), FetchError> {
    let config = dir
        .parent()
        .unwrap_or(dir)
        .join(format!(".gitconfig-checkout-{}", std::process::id()));
    std::fs::write(&config, "").map_err(|error| {
        FetchError::Tool(format!("could not write {}: {error}", config.display()))
    })?;
    let result = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "core.autocrlf=false"])
        .args(["-c", "core.eol=lf"])
        .args(["-c", "core.symlinks=false"])
        .arg("checkout")
        .args(["-q", "--detach", rev])
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", &config)
        .output();
    let _ = std::fs::remove_file(&config);
    let output = result.map_err(|error| FetchError::Tool(format!("could not run git: {error}")))?;
    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(FetchError::Tool(format!(
            "git checkout failed: {}",
            stderr.lines().take(4).collect::<Vec<_>>().join("; ")
        )))
    }
}

fn run_git_output(dir: &Path, args: &[&str]) -> Result<String, FetchError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .map_err(|error| FetchError::Tool(format!("could not run git: {error}")))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        Err(FetchError::Tool(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
                .lines()
                .take(4)
                .collect::<Vec<_>>()
                .join("; ")
        )))
    }
}

fn run_git(dir: &Path, args: &[&str]) -> Result<(), FetchError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .map_err(|error| FetchError::Tool(format!("could not run git: {error}")))?;
    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(FetchError::Tool(format!(
            "git {} failed: {}",
            args.join(" "),
            stderr.lines().take(4).collect::<Vec<_>>().join("; ")
        )))
    }
}
