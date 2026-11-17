//! Runner for the hand-written conformance suite.
//!
//! The suite's contract is in `conformance/README.md`. This runner implements
//! `accept`, `reject` and `cost`, and checks every `.fixed.ingot` against the fix
//! engine. A `cost` test compares the surviving check markers from phase 10. `run`
//! and `trap` tests are recognised but fail, because evaluation is not implemented.
//! A malformed test is a failure rather than a skip.

use crate::fix;
use crate::loader::{self, LoadOptions};
use crate::pipeline::{self, Analysis, Options};
use ingot_diag::diagnostic::DEFAULT_TARGET;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Expect {
    Accept,
    Reject,
    Run,
    Trap,
    Cost,
}

/// One marked line: the file it is in, the code marked on it, and the line number.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Marker {
    /// Path of the file relative to the package root, with `/` separators.
    pub file: String,
    pub code: String,
    pub line: u32,
}

/// A `//~ related CODE ROLE` marker: a related span that must start on this line.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct RelatedMarker {
    pub file: String,
    pub code: String,
    pub role: String,
    pub line: u32,
}

#[derive(Clone, Debug)]
pub struct Case {
    pub path: PathBuf,
    pub expect: Expect,
    pub markers: BTreeSet<Marker>,
    pub related: BTreeSet<RelatedMarker>,
    pub exit: i32,
    pub target: Option<String>,
    /// Malformed directives or markers. A non-empty list fails the case.
    pub errors: Vec<String>,
    pub has_fix: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Report {
    pub passed: usize,
    pub failed: usize,
    pub skipped: usize,
    /// Paths of the cases that passed, in discovery order.
    pub passes: Vec<PathBuf>,
    pub failures: Vec<Failure>,
}

#[derive(Clone, Debug)]
pub struct Failure {
    pub path: PathBuf,
    pub message: String,
}

pub fn discover(root: &Path) -> Vec<Case> {
    let mut files = Vec::new();
    collect(root, &mut files);
    files.sort();
    files.into_iter().map(|p| read_case(&p)).collect()
}

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = entries.flatten().collect();
    entries.sort_by_key(|e| e.path());
    // A directory with `main.ingot` is one package test, not one test per file.
    if dir.join("main.ingot").exists() {
        out.push(dir.join("main.ingot"));
        return;
    }
    for e in entries {
        let p = e.path();
        if p.is_dir() {
            collect(&p, out);
        } else if p.extension().map(|x| x == "ingot").unwrap_or(false) {
            out.push(p);
        }
    }
}

pub fn read_case(path: &Path) -> Case {
    let mut case = Case {
        path: path.to_path_buf(),
        expect: Expect::Accept,
        markers: BTreeSet::new(),
        related: BTreeSet::new(),
        exit: 0,
        target: None,
        errors: Vec::new(),
        has_fix: false,
    };
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) => {
            case.errors.push(format!("could not read source: {e}"));
            return case;
        }
    };
    // Directives come from the test's own file only.
    let mut expect = None;
    for (i, line) in text.lines().enumerate() {
        let line_no = i as u32 + 1;
        if let Some(rest) = line.trim_start().strip_prefix("//!") {
            let rest = rest.trim();
            if let Some(v) = rest.strip_prefix("expect:") {
                match v.trim() {
                    "accept" => expect = Some(Expect::Accept),
                    "reject" => expect = Some(Expect::Reject),
                    "run" => expect = Some(Expect::Run),
                    "trap" => expect = Some(Expect::Trap),
                    "cost" => expect = Some(Expect::Cost),
                    other => case
                        .errors
                        .push(format!("line {line_no}: unknown expect value `{other}`")),
                }
            } else if rest.starts_with("section:") {
                // Free-form; only its presence is meaningful.
            } else if let Some(v) = rest.strip_prefix("exit:") {
                match v.trim().parse() {
                    Ok(n) => case.exit = n,
                    Err(_) => case
                        .errors
                        .push(format!("line {line_no}: exit is not an integer")),
                }
            } else if let Some(v) = rest.strip_prefix("target:") {
                let v = v.trim();
                if v.is_empty() {
                    case.errors.push(format!("line {line_no}: target is empty"));
                } else {
                    case.target = Some(v.to_string());
                }
            } else {
                case.errors
                    .push(format!("line {line_no}: unknown directive `{rest}`"));
            }
        }
    }
    case.expect = expect.unwrap_or_else(|| {
        case.errors
            .push("missing `//! expect:` directive".to_string());
        Expect::Accept
    });
    match test_sources(path, &text) {
        Ok(files) => {
            for (file, file_text) in &files {
                collect_markers(&mut case, file, file_text);
            }
        }
        Err(e) => case.errors.push(e),
    }
    for marker in &case.markers {
        if !marker_fits(case.expect, &marker.code) {
            case.errors.push(format!(
                "{}:{}: marker `{}` does not fit a `{:?}` test",
                marker.file, marker.line, marker.code, case.expect
            ));
        }
    }
    for marker in &case.related {
        if case.expect != Expect::Reject {
            case.errors.push(format!(
                "{}:{}: a `related` marker fits only a `reject` test",
                marker.file, marker.line
            ));
        } else if !marker_fits(Expect::Reject, &marker.code) || !known_role(&marker.role) {
            case.errors.push(format!(
                "{}:{}: `related {} {}` is not a diagnostic code and a related role",
                marker.file, marker.line, marker.code, marker.role
            ));
        }
    }
    case.has_fix = fixed_path(path).exists();
    case
}

/// The files a test is made of, as `(path relative to the package root, text)`.
/// A package test (`main.ingot` in a directory) is every `.ingot` file the loader
/// finds under that directory. A single-file test is that file alone.
fn test_sources(path: &Path, text: &str) -> Result<Vec<(String, String)>, String> {
    if path.file_name().is_some_and(|name| name == "main.ingot") {
        let dir = match path.parent() {
            Some(dir) if !dir.as_os_str().is_empty() => dir,
            _ => Path::new("."),
        };
        let package = loader::load_with(dir, harness_load())
            .map_err(|e| format!("could not load {}: {e}", dir.display()))?;
        return Ok(package
            .sources
            .files
            .into_iter()
            .map(|s| (s.path, s.text))
            .collect());
    }
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    Ok(vec![(name, text.to_string())])
}

/// The marker comment of a file: `//~` in source files, `#~` in `ingot.toml`.
fn marker_prefix(file: &str) -> &'static str {
    if file.ends_with(".toml") { "#~" } else { "//~" }
}

fn collect_markers(case: &mut Case, file: &str, text: &str) {
    let prefix = marker_prefix(file);
    for (i, line) in text.lines().enumerate() {
        let line_no = i as u32 + 1;
        let mut rest = line;
        while let Some(idx) = rest.find(prefix) {
            rest = &rest[idx + prefix.len()..];
            let mut words = rest.split_whitespace();
            let first = words.next().unwrap_or("");
            if first == "related" {
                let code = words.next().unwrap_or("").to_string();
                let role = words.next().unwrap_or("").to_string();
                if code.is_empty() || role.is_empty() {
                    case.errors.push(format!(
                        "{file}:{line_no}: `related` marker needs a code and a role"
                    ));
                } else {
                    case.related.insert(RelatedMarker {
                        file: file.to_string(),
                        code,
                        role,
                        line: line_no,
                    });
                }
            } else if first.is_empty() {
                case.errors
                    .push(format!("{file}:{line_no}: marker without a kind"));
            } else {
                case.markers.insert(Marker {
                    file: file.to_string(),
                    code: first.to_string(),
                    line: line_no,
                });
            }
        }
    }
}

/// The closed set of related roles in diagnostics.md section 1.
fn known_role(role: &str) -> bool {
    matches!(
        role,
        "declared"
            | "previous"
            | "expected"
            | "borrow"
            | "move"
            | "later_use"
            | "scope_end"
            | "defer"
            | "signature"
            | "callee"
            | "spawn"
            | "loop"
            | "instantiation"
            | "call"
            | "candidate"
    )
}

fn fixed_path(path: &Path) -> PathBuf {
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "test".to_string());
    path.with_file_name(format!("{stem}.fixed.ingot"))
}

fn marker_fits(expect: Expect, code: &str) -> bool {
    match expect {
        Expect::Accept | Expect::Run => false,
        Expect::Reject => {
            code.len() == 5
                && code.starts_with('E')
                && code[1..].bytes().all(|b| b.is_ascii_digit())
        }
        Expect::Trap => {
            code.len() == 3
                && code.starts_with('T')
                && code[1..].bytes().all(|b| b.is_ascii_digit())
        }
        Expect::Cost => matches!(
            code,
            "bounds_check"
                | "overflow_check"
                | "div_check"
                | "shift_check"
                | "scatter_check"
                | "alloc"
                | "indirect_call"
                | "copy"
                | "task_spawn"
                | "lock"
                | "park_point"
        ),
    }
}

pub fn run(root: &Path) -> Report {
    let mut report = Report::default();
    for case in discover(root) {
        let outcome = if !case.errors.is_empty() {
            Err(case.errors.join("; "))
        } else if case
            .target
            .as_ref()
            .is_some_and(|target| target != DEFAULT_TARGET)
        {
            report.skipped += 1;
            continue;
        } else {
            run_case(&case)
        };
        match outcome {
            Ok(()) => {
                report.passed += 1;
                report.passes.push(case.path.clone());
            }
            Err(message) => {
                report.failed += 1;
                report.failures.push(Failure {
                    path: case.path.clone(),
                    message,
                });
            }
        }
    }
    report
}

/// The loader conventions of the harness (see `loader.rs`).
fn harness_load() -> LoadOptions {
    LoadOptions {
        harness: true,
        ..LoadOptions::default()
    }
}

/// The analysis options of a case: the harness's loader and the default target.
pub fn case_options() -> Options {
    Options {
        load: harness_load(),
        ..Options::default()
    }
}

/// What to hand the loader: the directory of a package test, or the single file.
fn case_root(path: &Path) -> PathBuf {
    if path.file_name().is_some_and(|name| name == "main.ingot") {
        match path.parent() {
            Some(dir) if !dir.as_os_str().is_empty() => dir.to_path_buf(),
            _ => PathBuf::from("."),
        }
    } else {
        path.to_path_buf()
    }
}

fn run_case(case: &Case) -> Result<(), String> {
    match case.expect {
        Expect::Accept | Expect::Reject | Expect::Cost => {}
        Expect::Run | Expect::Trap => return Err("evaluation not implemented".to_string()),
    }
    run_expectation(case).and_then(|()| if case.has_fix { run_fix(case) } else { Ok(()) })
}

fn run_expectation(case: &Case) -> Result<(), String> {
    let analysis = analyze_case(case).ok_or_else(|| "could not read source".to_string())?;
    if case.expect == Expect::Cost {
        if !analysis.diagnostics.is_empty() {
            return Err(format!(
                "cost test reported [{}]",
                format_markers(&diagnostics_markers(&analysis))
            ));
        }
        let blocking: Vec<&String> = analysis.not_implemented.iter().collect();
        if !blocking.is_empty() {
            return Err(format!("not implemented: {blocking:?}"));
        }
        let actual = cost_markers(&analysis);
        return if actual == case.markers {
            Ok(())
        } else {
            Err(format!(
                "expected [{}], got [{}]",
                format_markers(&case.markers),
                format_markers(&actual)
            ))
        };
    }
    let actual = diagnostics_markers(&analysis);
    if case.expect == Expect::Accept {
        return if !actual.is_empty() {
            Err(format!(
                "accept test reported [{}]",
                format_markers(&actual)
            ))
        } else if !analysis.not_implemented.is_empty() {
            // Not a rejection, but not an acceptance either: part of the check is
            // missing, and a test must not pass on that.
            Err(format!(
                "not implemented: {}",
                analysis.not_implemented.join("; ")
            ))
        } else {
            Ok(())
        };
    }
    if actual != case.markers {
        return Err(format!(
            "expected [{}], got [{}]",
            format_markers(&case.markers),
            format_markers(&actual)
        ));
    }
    check_related(case, &analysis)
}

/// Every `related` marker must match a related span of the named code on its line.
fn check_related(case: &Case, analysis: &Analysis) -> Result<(), String> {
    let mut missing = Vec::new();
    for wanted in &case.related {
        let found = analysis.diagnostics.iter().any(|d| {
            if d.code != wanted.code {
                return false;
            }
            d.related.iter().any(|r| {
                r.role == wanted.role
                    && r.file == wanted.file
                    && analysis
                        .sources
                        .iter()
                        .find(|s| s.path == r.file)
                        .map(|s| s.position(r.span.start).line)
                        .is_some_and(|line| line == wanted.line)
            })
        });
        if !found {
            missing.push(format!(
                "{}:{}: no `{}` related {}",
                wanted.file, wanted.line, wanted.role, wanted.code
            ));
        }
    }
    if missing.is_empty() {
        Ok(())
    } else {
        Err(missing.join("; "))
    }
}

fn run_fix(case: &Case) -> Result<(), String> {
    let outcome = fix::fix_path_with(&case_root(&case.path), &case_options())
        .map_err(|e| format!("fix engine failed: {e}"))?;
    let expected_path = fixed_path(&case.path);
    let expected = std::fs::read_to_string(&expected_path)
        .map_err(|e| format!("could not read {}: {e}", expected_path.display()))?;
    let actual = outcome
        .sources
        .iter()
        .find(|s| s.absolute == case.path)
        .or_else(|| {
            let name = case.path.file_name()?.to_string_lossy().into_owned();
            outcome.sources.iter().find(|s| s.path == name)
        })
        .ok_or_else(|| "fix engine lost the test source".to_string())?;
    let actual = normalize(&actual.text);
    let expected = normalize(&expected);
    if actual == expected {
        Ok(())
    } else {
        Err(format!(
            "fixed output differs from {}",
            expected_path.display()
        ))
    }
}

/// README "Fix tests": drop directive lines, marker comments and trailing
/// white space from both texts before comparing.
fn normalize(text: &str) -> String {
    text.lines()
        .filter(|line| !line.trim_start().starts_with("//!"))
        .map(|line| match line.find("//~") {
            Some(i) => line[..i].trim_end(),
            None => line.trim_end(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn diagnostics_markers(analysis: &Analysis) -> BTreeSet<Marker> {
    analysis
        .diagnostics
        .iter()
        .map(|d| {
            let line = analysis
                .sources
                .iter()
                .find(|s| s.path == d.file)
                .map(|s| s.position(d.span.start).line)
                .unwrap_or(1);
            Marker {
                file: d.file.clone(),
                code: d.code.to_string(),
                line,
            }
        })
        .collect()
}

/// The cost markers of a `cost` test: every reported kind is compared
/// (`conformance/README.md`).
fn cost_markers(analysis: &Analysis) -> BTreeSet<Marker> {
    analysis
        .cost
        .iter()
        .filter_map(|item| {
            let source = analysis.sources.files.get(item.file)?;
            let line = source.position(item.span.start).line;
            Some(Marker {
                file: source.path.clone(),
                code: item.kind.name().to_string(),
                line,
            })
        })
        .collect()
}

fn format_markers(markers: &BTreeSet<Marker>) -> String {
    markers
        .iter()
        .map(|m| format!("{}:{}:{}", m.file, m.line, m.code))
        .collect::<Vec<_>>()
        .join(", ")
}

/// True when a case is in an area the front end claims to handle.
pub fn supported(case: &Case) -> bool {
    matches!(
        case.expect,
        Expect::Accept | Expect::Reject | Expect::Run | Expect::Trap | Expect::Cost
    )
}

pub fn analyze_case(case: &Case) -> Option<Analysis> {
    pipeline::analyze_path_with(&case_root(&case.path), &case_options()).ok()
}
