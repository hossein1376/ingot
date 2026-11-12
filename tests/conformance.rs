//! The conformance suite is the shared contract. This test fails if the MVP
//! regresses on any case it claims to run.

use std::path::PathBuf;

#[test]
fn conformance_suite_passes() {
    // The runner walks every program in one pass; give it the same stack the
    // command-line binary gets on the main thread.
    let handle = std::thread::Builder::new()
        .stack_size(32 * 1024 * 1024)
        .spawn(|| {
            let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("conformance");
            ingotc::conformance::run(&root)
        })
        .unwrap();
    let report = handle.join().unwrap();
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
        report.passed >= 99,
        "only {} cases discovered",
        report.passed
    );
}
