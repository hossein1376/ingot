//! The conformance suite is the shared contract. This test fails if the MVP
//! regresses on any case it claims to run.

use std::path::PathBuf;

#[test]
fn conformance_suite_passes() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("conformance");
    let report = ingotc::conformance::run(&root);
    for failure in &report.failures {
        eprintln!("FAIL {}: {}", failure.path.display(), failure.message);
    }
    assert_eq!(report.failed, 0, "{} conformance failures", report.failed);
    assert_eq!(
        report.skipped, 0,
        "{} conformance cases skipped",
        report.skipped
    );
    assert!(
        report.passed >= 100,
        "only {} cases discovered",
        report.passed
    );
}
