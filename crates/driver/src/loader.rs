//! Phase 1: load a package directory tree (or one file) into source files and
//! module names.
//!
//! A package is a directory with `ingot.toml` or a `main.ingot`.  Every `.ingot`
//! under it is a module. `main.ingot` at the package root is module `main`; other
//! paths become dotted module names (`net/http.ingot` is `net.http`). A single
//! file checked on its own is module `main` of package `t`, exactly as the
//! conformance README defines. The standard library is not loaded.

use ingot_diag::diagnostic::Diagnostic;
use ingot_diag::source::{Source, Sources, Span};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct Manifest {
    pub name: Option<String>,
    pub targets: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct LoadedModule {
    pub name: String,
    pub source_id: usize,
    pub root: bool,
}

#[derive(Clone, Debug, Default)]
pub struct LoadedPackage {
    pub package_name: String,
    pub root: PathBuf,
    pub manifest: Option<Manifest>,
    pub sources: Sources,
    pub modules: Vec<LoadedModule>,
    pub diagnostics: Vec<Diagnostic>,
}

impl LoadedPackage {
    pub fn root_module(&self) -> &str {
        "main"
    }

    pub fn modules_by_name(&self) -> BTreeMap<String, usize> {
        self.modules
            .iter()
            .map(|m| (m.name.clone(), m.source_id))
            .collect()
    }
}

pub fn load(path: &Path) -> std::io::Result<LoadedPackage> {
    if path.is_dir() {
        load_dir(path)
    } else {
        load_file(path)
    }
}

fn load_file(path: &Path) -> std::io::Result<LoadedPackage> {
    // A file inside a package is checked as part of that package. Walk up until
    // a directory with `ingot.toml` or a root `main.ingot` is found. A bare file
    // name has the empty path as its parent, which means the current directory.
    let mut dir = Some(
        path.parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new(".")),
    );
    while let Some(d) = dir {
        let has_manifest = d.join("ingot.toml").exists();
        let has_root_main = d.join("main.ingot").exists();
        if has_manifest || has_root_main {
            return load_dir(d);
        }
        dir = d.parent();
    }
    let text = std::fs::read_to_string(path)?;
    // A single file is its own package root, so its path is relative to the
    // directory that contains it (tools.md §1: paths are relative to the
    // package root).
    let display = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().replace('\\', "/"));
    let source = Source::with_absolute(display, path.to_path_buf(), text);
    let mut sources = Sources::default();
    sources.add(source);
    Ok(LoadedPackage {
        package_name: "t".to_string(),
        root: path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf(),
        manifest: None,
        sources,
        modules: vec![LoadedModule {
            name: "main".to_string(),
            source_id: 0,
            root: true,
        }],
        diagnostics: Vec::new(),
    })
}

fn load_dir(dir: &Path) -> std::io::Result<LoadedPackage> {
    let manifest_path = dir.join("ingot.toml");
    let manifest = if manifest_path.exists() {
        Some(parse_manifest(&manifest_path)?)
    } else {
        None
    };
    let package_name = manifest
        .as_ref()
        .and_then(|m| m.name.clone())
        .or_else(|| dir.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "t".to_string());

    let mut files = Vec::new();
    collect_ingot(dir, dir, &mut files)?;
    files.sort();
    if files.is_empty() {
        return Ok(LoadedPackage {
            package_name,
            root: dir.to_path_buf(),
            manifest,
            ..LoadedPackage::default()
        });
    }

    let mut sources = Sources::default();
    let mut modules = Vec::new();
    let mut diagnostics = Vec::new();
    for file in files {
        let rel = file.strip_prefix(dir).unwrap_or(&file);
        let module = module_name(rel);
        let bytes = std::fs::read(&file)?;
        let text = match String::from_utf8(bytes) {
            Ok(t) => t,
            Err(e) => {
                let bad = e.utf8_error().valid_up_to();
                let lossy = String::from_utf8_lossy(e.as_bytes()).into_owned();
                let source = Source::with_absolute(relative_path(rel), file.clone(), lossy);
                let id = sources.add(source.clone());
                diagnostics.push(Diagnostic::new(
                    "E0101",
                    "file is not valid UTF-8",
                    &source,
                    Span::new(bad, bad + 1),
                ));
                modules.push(LoadedModule {
                    name: module.clone(),
                    source_id: id,
                    root: module == "main",
                });
                continue;
            }
        };
        let source = Source::with_absolute(relative_path(rel), file.clone(), text);
        let id = sources.add(source);
        modules.push(LoadedModule {
            name: module.clone(),
            source_id: id,
            root: module == "main",
        });
    }
    // Ensure module names are unique and deterministic.
    modules.sort_by(|a, b| a.name.cmp(&b.name).then(a.source_id.cmp(&b.source_id)));
    if !modules.iter().any(|m| m.root) && !modules.is_empty() {
        // A package used as a library has no `main` module; the check still runs.
    }
    Ok(LoadedPackage {
        package_name,
        root: dir.to_path_buf(),
        manifest,
        sources,
        modules,
        diagnostics,
    })
}

fn collect_ingot(root: &Path, dir: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    let mut entries: Vec<_> = std::fs::read_dir(dir)?.flatten().collect();
    entries.sort_by_key(|e| e.path());
    for entry in entries {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if path.is_dir() {
            if matches!(name.as_ref(), "target" | ".git") || name.starts_with('.') {
                continue;
            }
            collect_ingot(root, &path, out)?;
        } else if path.extension().map(|x| x == "ingot").unwrap_or(false) {
            let _ = root;
            out.push(path);
        }
    }
    Ok(())
}

fn module_name(rel: &Path) -> String {
    if rel == Path::new("main.ingot") {
        return "main".to_string();
    }
    let mut parts: Vec<String> = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    if let Some(last) = parts.pop() {
        let stem = last.strip_suffix(".ingot").unwrap_or(&last).to_string();
        parts.push(stem);
    }
    parts.join(".")
}

fn relative_path(rel: &Path) -> String {
    rel.components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

fn parse_manifest(path: &Path) -> std::io::Result<Manifest> {
    let text = std::fs::read_to_string(path)?;
    let mut manifest = Manifest {
        name: None,
        targets: Vec::new(),
    };
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with('[') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        let value = value.trim();
        match key {
            "name" => manifest.name = Some(unquote(value).to_string()),
            "targets" => manifest.targets = parse_string_array(value),
            _ => {}
        }
    }
    Ok(manifest)
}

fn unquote(s: &str) -> &str {
    s.strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .unwrap_or(s)
}

fn parse_string_array(s: &str) -> Vec<String> {
    s.trim()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| unquote(s).to_string())
        .collect()
}
