//! Permanent sweep: every `accept` test in `conformance/` is accepted by phases 1 to 8.
//! The analysis has no diagnostics, phase 8 records no `lowering:` entry (a body it
//! cannot lower records one and drops the CFG, so this is the check that every body
//! phase 7 checked has a CFG), and every CFG passes the validator. The remaining
//! `not_implemented` entries are the phase 9 and 10 message and phase 7's own entries;
//! this test does not recompute that list.

use ingot_driver::conformance::{self, Expect};
use std::path::{Path, PathBuf};

fn conformance_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../conformance")
}

#[test]
fn every_accept_test_lowers() {
    let root = conformance_dir();
    let cases = conformance::discover(&root);
    assert!(
        !cases.is_empty(),
        "no conformance tests under {}",
        root.display()
    );
    let mut failures = Vec::new();
    let mut lowered = 0usize;
    for case in cases {
        if case.expect != Expect::Accept || !case.errors.is_empty() {
            continue;
        }
        if case
            .target
            .as_ref()
            .is_some_and(|t| t != ingot_diag::diagnostic::DEFAULT_TARGET)
        {
            continue;
        }
        let Some(analysis) = conformance::analyze_case(&case) else {
            failures.push(format!("{}: could not be analyzed", case.path.display()));
            continue;
        };
        lowered += 1;
        if !analysis.diagnostics.is_empty() {
            let text = analysis
                .diagnostics
                .iter()
                .map(|d| d.to_human(&analysis.sources))
                .collect::<Vec<_>>()
                .join("\n");
            failures.push(format!("{}: diagnostics:\n{text}", case.path.display()));
            continue;
        }
        let phase8: Vec<&String> = analysis
            .not_implemented
            .iter()
            .filter(|m| m.starts_with("lowering:"))
            .collect();
        if !phase8.is_empty() {
            failures.push(format!(
                "{}: phase 8 did not lower: {phase8:?}",
                case.path.display()
            ));
        }
        let problems = ingot_cfg::validate::validate_all(&analysis.cfgs);
        if !problems.is_empty() {
            failures.push(format!(
                "{}: validator:\n  {}",
                case.path.display(),
                problems.join("\n  ")
            ));
        }
    }
    assert!(lowered > 0, "no accept test was analyzed");
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}
