//! The conformance ratchet. `expected_pass.txt` lists exactly the suite's tests that
//! pass now, one path per line relative to `conformance/`. This test fails when a
//! listed test fails (or is gone), and when an unlisted test passes, so the list
//! only changes by an explicit edit.

use ingot_driver::conformance;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

fn relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

#[test]
fn conformance_matches_expected_pass() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let root = fs::canonicalize(manifest.join("../../conformance")).unwrap();
    // The runner walks every program in one pass; give it the same stack the
    // command-line binary gets on the main thread.
    let worker_root = root.clone();
    let report = std::thread::Builder::new()
        .stack_size(32 * 1024 * 1024)
        .spawn(move || conformance::run(&worker_root))
        .unwrap()
        .join()
        .unwrap();

    let expected: BTreeSet<String> = fs::read_to_string(manifest.join("tests/expected_pass.txt"))
        .unwrap()
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect();
    let passing: BTreeSet<String> = report.passes.iter().map(|p| relative(&root, p)).collect();
    let failing: BTreeMap<String, String> = report
        .failures
        .iter()
        .map(|f| (relative(&root, &f.path), f.message.clone()))
        .collect();

    let mut offenders = Vec::new();
    for name in &expected {
        if let Some(message) = failing.get(name) {
            offenders.push(format!("listed test fails: {name}: {message}"));
        } else if !passing.contains(name) {
            offenders.push(format!("listed test was not found: {name}"));
        }
    }
    for name in &passing {
        if !expected.contains(name) {
            offenders.push(format!(
                "unlisted test passes: {name} (add it to crates/driver/tests/expected_pass.txt)"
            ));
        }
    }
    if report.skipped > 0 {
        offenders.push(format!(
            "{} conformance case(s) skipped for another target, so not run",
            report.skipped
        ));
    }

    for offender in &offenders {
        eprintln!("{offender}");
    }
    assert!(
        offenders.is_empty(),
        "{} conformance ratchet offender(s), see above",
        offenders.len()
    );
}
