//! Phase 1: load a package (`ingot.toml` and its directory tree) into source files
//! and module paths, and locate the standard library.
//!
//! A package is a directory with an `ingot.toml` (spec section 33); every `.ingot` file
//! below it is a module. `main.ingot` at the package root is the root module, whose
//! path is the package name itself; `net/http.ingot` is `net.http`. A single file given
//! on its own is a package named `t` with no manifest, whose root module is that file
//! (`conformance/README.md`).
//!
//! The conformance harness has two conventions the command line does not: a directory
//! test needs no `ingot.toml` (its package is named after the directory), and its
//! `*.fixed.ingot` files are expected results, not modules. They are enabled by
//! `LoadOptions::harness`.

use crate::manifest::{self, Manifest};
use ingot_diag::diagnostic::Diagnostic;
use ingot_diag::source::{Source, Sources, Span};
use ingot_syntax::lexer::lex;
use ingot_syntax::token::TokenKind;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, Default)]
pub struct LoadOptions {
    /// The conformance harness's conventions (see the module comment).
    pub harness: bool,
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

pub fn load_with(path: &Path, options: LoadOptions) -> std::io::Result<LoadedPackage> {
    if path.is_dir() {
        load_dir(path, options, "")
    } else {
        load_file(path, options)
    }
}

fn load_file(path: &Path, options: LoadOptions) -> std::io::Result<LoadedPackage> {
    // A file inside a package is checked as part of that package: walk up to the
    // directory with `ingot.toml`. A bare file name has the empty path as its
    // parent, which means the current directory.
    // The walk starts from the absolute directory, so a relative path such as
    // `other.ingot` or `sub/other.ingot` still reaches a manifest above it.
    let start = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let start = start.canonicalize().unwrap_or_else(|_| start.to_path_buf());
    let mut dir = Some(start.as_path());
    while let Some(d) = dir {
        if d.join("ingot.toml").exists() {
            return load_dir(d, options, "");
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
    })
}

/// Load the package rooted at `dir`. Every source's displayed path starts with
/// `prefix` (the standard library's files are shown as `std/...`).
pub fn load_dir(dir: &Path, options: LoadOptions, prefix: &str) -> std::io::Result<LoadedPackage> {
    let dir_name = dir
        .canonicalize()
        .ok()
        .as_deref()
        .unwrap_or(dir)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "t".to_string());
    let mut package = LoadedPackage {
        package_name: dir_name.clone(),
        root: dir.to_path_buf(),
        ..LoadedPackage::default()
    };

    // The manifest.
    let manifest_path = dir.join("ingot.toml");
    let manifest_display = format!("{prefix}ingot.toml");
    if manifest_path.exists() {
        let text = std::fs::read_to_string(&manifest_path)?;
        let source = Source::with_absolute(manifest_display, manifest_path.clone(), text);
        let (manifest, diags) = manifest::parse(&source);
        package.sources.add(source);
        if diags.is_empty() {
            package.package_name = manifest.name.clone();
            if !manifest.deps.is_empty() {
                package
                    .not_implemented
                    .push("dependencies (`[deps]` in ingot.toml)".to_string());
            }
            package.manifest = Some(manifest);
        } else {
            package.manifest_failed = true;
            package.diagnostics.extend(diags);
        }
    } else if options.harness {
        // The harness convention: a directory test is a package named after it.
    } else {
        let source = Source::with_absolute(manifest_display, manifest_path, String::new());
        package.diagnostics.push(Diagnostic::new(
            "E1701",
            "`ingot.toml` is missing",
            &source,
            Span::new(0, 0),
        ));
        package.sources.add(source);
        package.manifest_failed = true;
    }

    let mut files = Vec::new();
    collect_ingot(dir, &mut files)?;
    files.sort();
    for file in files {
        let rel = file.strip_prefix(dir).unwrap_or(&file);
        let rel_path = relative_path(rel);
        if options.harness && rel_path.ends_with(".fixed.ingot") {
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
        let bytes = std::fs::read(&file)?;
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
    Ok(package)
}

fn collect_ingot(dir: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
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
            collect_ingot(&path, out)?;
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

/// Where the standard library's source is: `$INGOT_STD`, then next to the
/// executable, then relative to this crate's source tree.
pub fn std_root() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("INGOT_STD") {
        let path = PathBuf::from(path);
        if path.join("ingot.toml").exists() {
            return Some(path);
        }
    }
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        for candidate in [dir.join("std"), dir.join("../std"), dir.join("../../std")] {
            if candidate.join("ingot.toml").exists() {
                return Some(candidate);
            }
        }
    }
    let development = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../std");
    development
        .join("ingot.toml")
        .exists()
        .then_some(development)
}

/// Load the standard library as a package. Its files are shown as `std/...`.
pub fn load_std() -> std::io::Result<LoadedPackage> {
    let Some(root) = std_root() else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "cannot find the standard library (a directory `std` with an `ingot.toml`); set INGOT_STD",
        ));
    };
    load_dir(&root, LoadOptions::default(), "std/")
}
