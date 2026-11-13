//! Runner for the hand-written conformance suite.
//!
//! The suite's contract is in `conformance/README.md`. This runner implements
//! `accept`, `reject`, `run`, `trap` and `cost`, checks every `.fixed.ingot`
//! against the fix engine, and treats a malformed test as a failure rather
//! than a skip.

use crate::check::Analysis;
use crate::diagnostic::DEFAULT_TARGET;
use crate::interp;
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

#[derive(Clone, Debug)]
pub struct Case {
    pub path: PathBuf,
    pub expect: Expect,
    pub markers: BTreeSet<(String, u32)>,
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
        if let Some(idx) = line.find("//~") {
            let code = line[idx + 3..]
                .split_whitespace()
                .next()
                .unwrap_or("")
                .to_string();
            if code.is_empty() {
                case.errors
                    .push(format!("line {line_no}: marker without a kind"));
            } else {
                case.markers.insert((code, line_no));
            }
        }
    }
    case.expect = expect.unwrap_or_else(|| {
        case.errors
            .push("missing `//! expect:` directive".to_string());
        Expect::Accept
    });
    for (code, line) in &case.markers {
        if !marker_fits(case.expect, code) {
            case.errors.push(format!(
                "line {line}: marker `{code}` does not fit a `{:?}` test",
                case.expect
            ));
        }
    }
    case.has_fix = fixed_path(path).exists();
    case
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
            "bounds_check" | "overflow_check" | "div_check" | "shift_check"
        ),
    }
}

pub fn run(root: &Path) -> Report {
    let mut report = Report::default();
    for case in discover(root) {
        let failure = if !case.errors.is_empty() {
            Err(case.errors.join("; "))
        } else if case
            .target
            .as_ref()
            .is_some_and(|target| target != DEFAULT_TARGET)
        {
            report.skipped += 1;
            continue;
        } else {
            run_expectation(&case).and_then(|()| if case.has_fix { run_fix(&case) } else { Ok(()) })
        };
        match failure {
            Ok(()) => report.passed += 1,
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

fn run_expectation(case: &Case) -> Result<(), String> {
    let analysis = analyze_case(case).ok_or_else(|| "could not read source".to_string())?;
    match case.expect {
        Expect::Accept => {
            if analysis.diagnostics.is_empty() {
                Ok(())
            } else {
                Err(format!(
                    "accept test reported [{}]",
                    format_markers(&diagnostics_markers(&analysis))
                ))
            }
        }
        Expect::Reject => {
            let actual = diagnostics_markers(&analysis);
            if actual == case.markers {
                Ok(())
            } else {
                Err(format!(
                    "expected [{}], got [{}]",
                    format_markers(&case.markers),
                    format_markers(&actual)
                ))
            }
        }
        Expect::Cost => {
            if !analysis.diagnostics.is_empty() {
                return Err("unexpected diagnostics".to_string());
            }
            let actual = cost_markers(&analysis);
            if actual == case.markers {
                Ok(())
            } else {
                Err(format!(
                    "cost expected [{}], got [{}]",
                    format_markers(&case.markers),
                    format_markers(&actual)
                ))
            }
        }
        Expect::Run | Expect::Trap => {
            if !analysis.diagnostics.is_empty() {
                let got = analysis
                    .diagnostics
                    .iter()
                    .map(|d| d.code)
                    .collect::<Vec<_>>()
                    .join(", ");
                return Err(format!("unexpected diagnostics: {got}"));
            }
            let outcome = interp::run(&analysis).map_err(|e| format!("evaluation failed: {e}"))?;
            match case.expect {
                Expect::Run => {
                    if outcome.trap.is_none() && outcome.status == case.exit {
                        Ok(())
                    } else {
                        Err(format!(
                            "expected exit {}, got {}",
                            case.exit, outcome.status
                        ))
                    }
                }
                Expect::Trap => {
                    let Some(trap) = &outcome.trap else {
                        return Err(format!(
                            "expected trap {:?}, got exit {}",
                            case.markers, outcome.status
                        ));
                    };
                    let source = analysis.sources.files.first();
                    let line = source
                        .map(|s| s.position(trap.span.start).line)
                        .unwrap_or(1);
                    let actual = BTreeSet::from([(trap.code.to_string(), line)]);
                    let names_file = source
                        .map(|s| outcome.stderr.starts_with(&format!("{}:{}:", s.path, line)))
                        .unwrap_or(false);
                    if outcome.status == 101 && actual == case.markers && names_file {
                        Ok(())
                    } else {
                        Err(format!(
                            "expected trap {:?}, got {}",
                            case.markers, outcome.stderr
                        ))
                    }
                }
                _ => unreachable!(),
            }
        }
    }
}

fn run_fix(case: &Case) -> Result<(), String> {
    let outcome =
        crate::fix::fix_path(&case.path).map_err(|e| format!("fix engine failed: {e}"))?;
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

fn diagnostics_markers(analysis: &Analysis) -> BTreeSet<(String, u32)> {
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
            (d.code.to_string(), line)
        })
        .collect()
}

fn cost_markers(analysis: &Analysis) -> BTreeSet<(String, u32)> {
    crate::cost::cost_items(&analysis.program)
        .into_iter()
        .map(|item| {
            let line = analysis
                .sources
                .iter()
                .find(|s| s.path == item.file)
                .or_else(|| analysis.sources.files.first())
                .map(|s| s.position(item.span.start).line)
                .unwrap_or(1);
            (item.kind.name().to_string(), line)
        })
        .collect()
}

fn format_markers(markers: &BTreeSet<(String, u32)>) -> String {
    markers
        .iter()
        .map(|(c, l)| format!("{c}@{l}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// True when a case is in an area the MVP front end claims to handle.
pub fn supported(case: &Case) -> bool {
    matches!(
        case.expect,
        Expect::Accept | Expect::Reject | Expect::Run | Expect::Trap | Expect::Cost
    )
}

pub fn analyze_case(case: &Case) -> Option<Analysis> {
    crate::check::analyze_file(&case.path).ok()
}
