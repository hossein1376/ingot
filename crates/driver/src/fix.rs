//! The `ingot fix` engine.
//!
//! `docs/impl/diagnostics.md` §1 "Fixes": take the first fix of each
//! diagnostic that does not overlap a fix already taken this round, apply the
//! round, re-check, and repeat until a round applies nothing. The engine works
//! on the loaded package in memory so the conformance runner and the CLI share
//! it; only `write_back` writes the result to disk, and only inside the root of
//! the package that was fixed.

use crate::loader::{self, LoadedPackage};
use crate::pipeline::{self, Options};
use ingot_diag::diagnostic::{Diagnostic, json_string};
use ingot_diag::source::{Position, Sources, Span};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct AppliedEdit {
    pub code: &'static str,
    pub file: String,
    pub span: Span,
    /// Position of the span in the text of the round that applied it.
    pub start: Position,
    pub end: Position,
    pub text: String,
    pub round: u32,
}

impl AppliedEdit {
    pub fn to_json(&self) -> String {
        let mut out = String::from("{\"type\":\"edit\",\"code\":");
        json_string(&mut out, self.code);
        out.push_str(",\"span\":{\"file\":");
        json_string(&mut out, &self.file);
        let _ = write!(
            out,
            ",\"start\":{{\"line\":{},\"col\":{},\"offset\":{}}},\"end\":{{\"line\":{},\"col\":{},\"offset\":{}}}}}",
            self.start.line,
            self.start.col,
            self.span.start,
            self.end.line,
            self.end.col,
            self.span.end
        );
        let _ = write!(out, ",\"text\":");
        json_string(&mut out, &self.text);
        let _ = write!(out, ",\"round\":{}}}", self.round);
        out
    }
}

#[derive(Clone, Debug)]
pub struct FixOutcome {
    /// Edits in the order the rounds applied them.
    pub edits: Vec<AppliedEdit>,
    /// Rounds that applied at least one edit.
    pub rounds: u32,
    /// What is left after the last round applied nothing.
    pub diagnostics: Vec<Diagnostic>,
    /// Root directory of the package that was fixed.
    pub root: PathBuf,
    /// Final text of every source of the package.
    pub sources: Sources,
}

pub fn fix_path(path: &Path) -> std::io::Result<FixOutcome> {
    fix_path_with(path, &Options::default())
}

pub fn fix_path_with(path: &Path, options: &Options) -> std::io::Result<FixOutcome> {
    // Fixing writes to disk; it has no overlay support (tools.md §6 overlays belong to
    // serve). Keep the harness convention, drop any overlays a library caller passes.
    let mut options = options.clone();
    options.load.overlays = Default::default();
    let mut package = loader::load_with(path, options.load.clone())?;
    fix_package_with(&mut package, &options)
}

pub fn fix_package(package: &mut LoadedPackage) -> std::io::Result<FixOutcome> {
    fix_package_with(package, &Options::default())
}

pub fn fix_package_with(
    package: &mut LoadedPackage,
    options: &Options,
) -> std::io::Result<FixOutcome> {
    let mut edits = Vec::new();
    let mut rounds = 0u32;
    loop {
        let analysis = pipeline::analyze_with(package.clone(), options)?;
        let mut selected: Vec<AppliedEdit> = Vec::new();
        for diagnostic in &analysis.diagnostics {
            let Some(fix) = diagnostic.fixes.first() else {
                continue;
            };
            let mut accepted = Vec::new();
            let mut conflict = false;
            for edit in &fix.edits {
                // A fix may change only files of the package being fixed. An edit
                // that names any other file makes the whole fix inapplicable.
                let Some(source) = analysis.sources.iter().find(|s| s.path == edit.file) else {
                    conflict = true;
                    break;
                };
                if selected
                    .iter()
                    .chain(accepted.iter())
                    .any(|taken| taken.file == edit.file && overlaps(taken.span, edit.span))
                {
                    conflict = true;
                    break;
                }
                accepted.push(AppliedEdit {
                    code: diagnostic.code,
                    file: edit.file.clone(),
                    span: edit.span,
                    start: source.position(edit.span.start),
                    end: source.position(edit.span.end),
                    text: edit.text.clone(),
                    round: rounds + 1,
                });
            }
            if !conflict {
                selected.extend(accepted);
            }
        }
        if selected.is_empty() {
            return Ok(FixOutcome {
                edits,
                rounds,
                diagnostics: analysis.diagnostics,
                root: package.root.clone(),
                sources: package.sources.clone(),
            });
        }
        if rounds >= 1000 {
            return Err(std::io::Error::other(
                "fix did not converge: a fix keeps reapplying",
            ));
        }
        apply_edits(package, &selected);
        rounds += 1;
        edits.extend(selected);
    }
}

/// Write the fixed sources back to disk. A source is written only when its
/// canonical path lies under the package root and its text changed.
pub fn write_back(outcome: &FixOutcome) -> std::io::Result<()> {
    let root = std::fs::canonicalize(&outcome.root)?;
    for source in outcome.sources.iter() {
        let Ok(absolute) = std::fs::canonicalize(&source.absolute) else {
            continue;
        };
        if !absolute.starts_with(&root) {
            continue;
        }
        if std::fs::read_to_string(&absolute).is_ok_and(|on_disk| on_disk == source.text) {
            continue;
        }
        std::fs::write(&absolute, &source.text)?;
    }
    Ok(())
}

fn overlaps(a: Span, b: Span) -> bool {
    if a.is_empty() && b.is_empty() {
        return a.start == b.start;
    }
    if a.is_empty() {
        return b.start <= a.start && a.start < b.end;
    }
    if b.is_empty() {
        return a.start <= b.start && b.start < a.end;
    }
    a.start < b.end && b.start < a.end
}

fn apply_edits(package: &mut LoadedPackage, edits: &[AppliedEdit]) {
    for index in 0..package.sources.files.len() {
        let path = package.sources.files[index].path.clone();
        let mut file_edits: Vec<&AppliedEdit> = edits.iter().filter(|e| e.file == path).collect();
        if file_edits.is_empty() {
            continue;
        }
        file_edits.sort_by_key(|e| (e.span.start, e.span.end));
        let text = package.sources.files[index].text.clone();
        let mut out = String::new();
        let mut pos = 0;
        for edit in file_edits {
            if edit.span.start < pos || edit.span.end > text.len() {
                continue;
            }
            out.push_str(&text[pos..edit.span.start]);
            out.push_str(&edit.text);
            pos = edit.span.end;
        }
        out.push_str(&text[pos..]);
        package.sources.set_text(index, out);
    }
}
