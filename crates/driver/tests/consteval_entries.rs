//! Every `consteval/*` test must be free of not-implemented entries other than the
//! ownership phases. A reject test only compares diagnostics, so without this a value
//! that silently became an entry would hide a failure.

use ingot_driver::conformance;
use ingot_driver::pipeline::UNCHECKED_CHECK_REMOVAL;
use std::path::{Path, PathBuf};

#[test]
fn consteval_tests_have_no_not_implemented_entries() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../conformance/consteval");
    let root = std::fs::canonicalize(&root).expect("consteval directory");
    let cases = conformance::discover(&root);
    assert!(!cases.is_empty(), "no consteval tests found");
    let mut offenders = Vec::new();
    for case in &cases {
        let Some(analysis) = conformance::analyze_case(case) else {
            offenders.push(format!("{}: could not analyze", case.path.display()));
            continue;
        };
        for entry in &analysis.not_implemented {
            if entry != UNCHECKED_CHECK_REMOVAL {
                offenders.push(format!(
                    "{}: not implemented: {entry}",
                    relative(&root, &case.path)
                ));
            }
        }
    }
    for offender in &offenders {
        eprintln!("{offender}");
    }
    assert!(
        offenders.is_empty(),
        "{} consteval test(s) leave a not-implemented entry",
        offenders.len()
    );
}

fn relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}
