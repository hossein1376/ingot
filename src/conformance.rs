//! Runner for the hand-written conformance suite.
//!
//! The suite's contract is in `conformance/README.md`. This runner implements
//! the parts the MVP can decide: `accept`, `reject`, and (once the evaluator
//! lands) `run` and `trap`. `cost` is skipped until phase 10 exists.

use crate::check::Analysis;
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
    files
        .into_iter()
        .filter_map(|p| read_case(&p).ok())
        .collect()
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

pub fn read_case(path: &Path) -> std::io::Result<Case> {
    let text = std::fs::read_to_string(path)?;
    let mut expect = None;
    let mut markers = BTreeSet::new();
    let mut exit = 0;
    for (i, line) in text.lines().enumerate() {
        let line_no = i as u32 + 1;
        if let Some(rest) = line.trim_start().strip_prefix("//!") {
            let rest = rest.trim();
            if let Some(v) = rest.strip_prefix("expect:") {
                expect = Some(match v.trim() {
                    "accept" => Expect::Accept,
                    "reject" => Expect::Reject,
                    "run" => Expect::Run,
                    "trap" => Expect::Trap,
                    "cost" => Expect::Cost,
                    _ => Expect::Accept,
                });
            } else if let Some(v) = rest.strip_prefix("exit:") {
                exit = v.trim().parse().unwrap_or(0);
            }
        }
        if let Some(idx) = line.find("//~") {
            let code = line[idx + 3..]
                .split_whitespace()
                .next()
                .unwrap_or("")
                .to_string();
            if !code.is_empty() {
                markers.insert((code, line_no));
            }
        }
    }
    Ok(Case {
        path: path.to_path_buf(),
        expect: expect.unwrap_or(Expect::Accept),
        markers,
        exit,
    })
}

pub fn run(root: &Path) -> Report {
    let mut report = Report::default();
    for case in discover(root) {
        // `.fixed.ingot` is an accept test in its own right.
        if case.path.to_string_lossy().ends_with(".fixed.ingot") {
            // Still run it: its header says accept.
        }
        match case.expect {
            Expect::Cost => {
                let Some(analysis) = analyze_case(&case) else {
                    report.failed += 1;
                    report.failures.push(Failure {
                        path: case.path.clone(),
                        message: "could not read source".to_string(),
                    });
                    continue;
                };
                if !analysis.diagnostics.is_empty() {
                    report.failed += 1;
                    report.failures.push(Failure {
                        path: case.path.clone(),
                        message: "unexpected diagnostics".to_string(),
                    });
                    continue;
                }
                let actual: BTreeSet<(String, u32)> = crate::cost::cost_items(&analysis.program)
                    .into_iter()
                    .map(|i| {
                        let line = analysis.sources.files[0].position(i.span.start).line;
                        (i.kind.name().to_string(), line)
                    })
                    .collect();
                if actual == case.markers {
                    report.passed += 1;
                } else {
                    report.failed += 1;
                    let expected = case
                        .markers
                        .iter()
                        .map(|(c, l)| format!("{c}@{l}"))
                        .collect::<Vec<_>>()
                        .join(", ");
                    let got = actual
                        .iter()
                        .map(|(c, l)| format!("{c}@{l}"))
                        .collect::<Vec<_>>()
                        .join(", ");
                    report.failures.push(Failure {
                        path: case.path.clone(),
                        message: format!("cost expected [{expected}], got [{got}]"),
                    });
                }
            }
            Expect::Accept | Expect::Reject => {
                let Some(analysis) = analyze_case(&case) else {
                    report.failed += 1;
                    report.failures.push(Failure {
                        path: case.path.clone(),
                        message: "could not read source".to_string(),
                    });
                    continue;
                };
                let actual: BTreeSet<(String, u32)> = analysis
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
                    .collect();
                match case.expect {
                    Expect::Accept if analysis.diagnostics.is_empty() => report.passed += 1,
                    Expect::Reject if actual == case.markers => report.passed += 1,
                    _ => {
                        report.failed += 1;
                        let expected = case
                            .markers
                            .iter()
                            .map(|(c, l)| format!("{c}@{l}"))
                            .collect::<Vec<_>>()
                            .join(", ");
                        let got = actual
                            .iter()
                            .map(|(c, l)| format!("{c}@{l}"))
                            .collect::<Vec<_>>()
                            .join(", ");
                        report.failures.push(Failure {
                            path: case.path.clone(),
                            message: format!("expected [{expected}], got [{got}]"),
                        });
                    }
                }
            }
            Expect::Run | Expect::Trap => {
                let Some(analysis) = analyze_case(&case) else {
                    report.failed += 1;
                    report.failures.push(Failure {
                        path: case.path.clone(),
                        message: "could not read source".to_string(),
                    });
                    continue;
                };
                if !analysis.diagnostics.is_empty() {
                    report.failed += 1;
                    let got = analysis
                        .diagnostics
                        .iter()
                        .map(|d| d.code)
                        .collect::<Vec<_>>()
                        .join(", ");
                    report.failures.push(Failure {
                        path: case.path.clone(),
                        message: format!("unexpected diagnostics: {got}"),
                    });
                    continue;
                }
                match interp::run(&analysis) {
                    Ok(outcome) => {
                        let ok = match case.expect {
                            Expect::Run => outcome.trap.is_none() && outcome.status == case.exit,
                            Expect::Trap => {
                                if let Some(trap) = &outcome.trap {
                                    let line =
                                        analysis.sources.files[0].position(trap.span.start).line;
                                    let actual = BTreeSet::from([(trap.code.to_string(), line)]);
                                    outcome.status == 101 && actual == case.markers
                                } else {
                                    false
                                }
                            }
                            _ => false,
                        };
                        if ok {
                            report.passed += 1;
                        } else {
                            report.failed += 1;
                            report.failures.push(Failure {
                                path: case.path.clone(),
                                message: if let Some(t) = &outcome.trap {
                                    format!(
                                        "expected {:?} exit {}, got trap {}: {} ({})",
                                        case.markers, case.exit, t.code, t.message, outcome.stderr
                                    )
                                } else {
                                    format!(
                                        "expected {:?} exit {}, got exit {}",
                                        case.markers, case.exit, outcome.status
                                    )
                                },
                            });
                        }
                    }
                    Err(e) => {
                        report.failed += 1;
                        report.failures.push(Failure {
                            path: case.path.clone(),
                            message: format!("evaluation failed: {e}"),
                        });
                    }
                }
            }
        }
    }
    report
}

/// True when a case is in an area the MVP front end claims to handle.
pub fn supported(case: &Case) -> bool {
    matches!(case.expect, Expect::Accept | Expect::Reject)
}

pub fn analyze_case(case: &Case) -> Option<Analysis> {
    crate::check::analyze_file(&case.path).ok()
}
