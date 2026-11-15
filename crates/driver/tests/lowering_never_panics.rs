//! Permanent sweep: phase 8 never panics. It lowers every non-failed body of every
//! conformance test (reject tests included) and of `std`, and every CFG it produces
//! passes [`ingot_cfg::validate_all`].

use ingot_driver::conformance;
use ingot_driver::pipeline;
use std::path::{Path, PathBuf};

fn conformance_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../conformance")
}

fn std_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../std")
}

#[test]
fn lowering_never_panics_over_the_suite() {
    let mut checked = 0usize;
    let mut failures = Vec::new();
    for case in conformance::discover(&conformance_dir()) {
        if !case.errors.is_empty() {
            continue;
        }
        let Some(analysis) = conformance::analyze_case(&case) else {
            continue;
        };
        checked += 1;
        let problems = ingot_cfg::validate::validate_all(&analysis.cfgs);
        if !problems.is_empty() {
            failures.push(format!(
                "{}: validator:\n  {}",
                case.path.display(),
                problems.join("\n  ")
            ));
        }
    }
    assert!(checked > 0, "no conformance test was analyzed");
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}

#[test]
fn lowering_never_panics_over_std() {
    let analysis = pipeline::analyze_path(&std_dir()).expect("std loads");
    let problems = ingot_cfg::validate::validate_all(&analysis.cfgs);
    assert!(
        problems.is_empty(),
        "std: validator:\n  {}",
        problems.join("\n  ")
    );
}
