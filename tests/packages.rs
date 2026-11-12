use ingotc::check::analyze_file;
use ingotc::interp;
use std::path::PathBuf;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

#[test]
fn package_modules_check_and_run_across_files() {
    let analysis = analyze_file(&fixture("package_basic")).unwrap();
    for d in &analysis.diagnostics {
        eprintln!("{}", d.to_human(&analysis.sources));
    }
    assert!(analysis.diagnostics.is_empty());
    let outcome = interp::run(&analysis).unwrap();
    assert_eq!(outcome.status, 0);
    assert!(outcome.trap.is_none());
}

#[test]
fn private_fields_across_modules_are_rejected() {
    let analysis = analyze_file(&fixture("package_private")).unwrap();
    let codes: Vec<&str> = analysis.diagnostics.iter().map(|d| d.code).collect();
    assert!(codes.contains(&"E0315"), "codes: {codes:?}");
    assert!(codes.contains(&"E0307"), "codes: {codes:?}");
}
