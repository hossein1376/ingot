//! The conformance runner's own tests. Each mutant of a copied test must fail;
//! the unmutated copy must pass.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn scratch(label: &str) -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "ingot-runner-{}-{label}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn copy_test(relative: &str, destination: &Path) -> String {
    let source = repo().join("conformance").join(relative);
    let text = fs::read_to_string(&source).unwrap();
    fs::write(destination, &text).unwrap();
    text
}

fn copy_fixed(relative: &str, destination: &Path) -> String {
    let source = repo()
        .join("conformance")
        .join(relative)
        .with_extension("fixed.ingot");
    let text = fs::read_to_string(&source).unwrap();
    fs::write(destination, &text).unwrap();
    text
}

fn assert_passes(dir: &Path) {
    let report = ingotc::conformance::run(dir);
    for failure in &report.failures {
        eprintln!("FAIL {}: {}", failure.path.display(), failure.message);
    }
    assert_eq!(report.failed, 0, "unmutated copy must pass");
    assert!(report.passed > 0, "unmutated copy must be discovered");
}

fn assert_fails(dir: &Path, why: &str) {
    let report = ingotc::conformance::run(dir);
    assert!(report.failed > 0, "mutant must fail: {why}");
}

fn replace(path: &Path, from: &str, to: &str) {
    let text = fs::read_to_string(path).unwrap();
    assert!(text.contains(from), "fixture must contain {from:?}");
    fs::write(path, text.replacen(from, to, 1)).unwrap();
}

#[test]
fn reject_marker_changed_to_another_code_fails() {
    let dir = scratch("reject-code");
    let case = dir.join("case.ingot");
    copy_test("names/var_never_mutated.ingot", &case);
    assert_passes(&dir);
    replace(&case, "//~ E0312", "//~ E0403");
    assert_fails(&dir, "a changed reject code");
}

#[test]
fn missing_reject_marker_fails() {
    let dir = scratch("reject-missing");
    let case = dir.join("case.ingot");
    copy_test("names/var_never_mutated.ingot", &case);
    assert_passes(&dir);
    replace(&case, " //~ E0312", "");
    assert_fails(&dir, "a removed reject marker");
}

#[test]
fn extra_marker_on_an_innocent_line_fails() {
    let dir = scratch("extra-marker");
    let case = dir.join("case.ingot");
    let text = copy_test("names/var_never_mutated.ingot", &case);
    assert_passes(&dir);
    let mutated = text.replacen("fn f() -> i64 {", "fn f() -> i64 { //~ E0312", 1);
    fs::write(&case, mutated).unwrap();
    assert_fails(&dir, "an extra marker");
}

#[test]
fn changed_trap_code_fails() {
    let dir = scratch("trap-code");
    let case = dir.join("case.ingot");
    copy_test("arith/index_out_of_bounds.ingot", &case);
    assert_passes(&dir);
    replace(&case, "//~ T01", "//~ T02");
    assert_fails(&dir, "a changed trap code");
}

#[test]
fn missing_cost_marker_fails() {
    let dir = scratch("cost-missing");
    let case = dir.join("case.ingot");
    copy_test("cost/unproven_index_keeps_check.ingot", &case);
    assert_passes(&dir);
    replace(&case, " //~ bounds_check", "");
    assert_fails(&dir, "a removed cost marker");
}

#[test]
fn extra_cost_marker_fails() {
    let dir = scratch("cost-extra");
    let case = dir.join("case.ingot");
    let text = copy_test("cost/unproven_index_keeps_check.ingot", &case);
    assert_passes(&dir);
    let mutated = text.replacen(
        "fn pick(xs: []i64, i: i64) -> i64 {",
        "fn pick(xs: []i64, i: i64) -> i64 { //~ bounds_check",
        1,
    );
    fs::write(&case, mutated).unwrap();
    assert_fails(&dir, "an added cost marker");
}

#[test]
fn altered_fixed_file_fails() {
    let dir = scratch("fixed-altered");
    let case = dir.join("case.ingot");
    let fixed = dir.join("case.fixed.ingot");
    copy_test("syntax/struct_literal.ingot", &case);
    let text = copy_fixed("syntax/struct_literal.ingot", &fixed);
    assert_passes(&dir);
    let altered = text.replacen("Point.{ x:", "Point.{  x:", 1);
    fs::write(&fixed, altered).unwrap();
    assert_fails(&dir, "a one-character change to .fixed.ingot");
}

#[test]
fn original_that_no_longer_needs_the_fix_fails() {
    let dir = scratch("fixed-stale");
    let case = dir.join("case.ingot");
    let fixed = dir.join("case.fixed.ingot");
    copy_test("names/var_never_mutated.ingot", &case);
    copy_fixed("names/var_never_mutated.ingot", &fixed);
    assert_passes(&dir);
    // Make the original acceptable already, so `fix` applies nothing and the
    // remaining text still differs from `.fixed.ingot`.
    let text = fs::read_to_string(&case).unwrap();
    let text = text
        .replace("//! expect: reject", "//! expect: accept")
        .replace(" //~ E0312", "")
        .replace("var total: i64 = 2;", "var total: i64 = 2; total = 3;");
    fs::write(&case, text).unwrap();
    assert_fails(&dir, "an original with no fix left to apply");
}

#[test]
fn unknown_expect_value_fails() {
    let dir = scratch("bad-expect");
    let case = dir.join("case.ingot");
    copy_test("names/var_never_mutated.ingot", &case);
    replace(&case, "expect: reject", "expect: maybe");
    assert_fails(&dir, "an unknown expect value");
}

#[test]
fn unknown_directive_fails() {
    let dir = scratch("bad-directive");
    let case = dir.join("case.ingot");
    copy_test("names/var_never_mutated.ingot", &case);
    replace(&case, "//! section: 7", "//! section: 7\n//! mystery: 1");
    assert_fails(&dir, "an unknown directive");
}

#[test]
fn marker_kind_must_fit_expectation() {
    let dir = scratch("bad-marker-kind");
    let case = dir.join("case.ingot");
    copy_test("names/var_never_mutated.ingot", &case);
    replace(&case, "//~ E0312", "//~ T01");
    assert_fails(&dir, "a trap marker in a reject test");
}

#[test]
fn other_target_is_skipped_and_this_target_runs() {
    let dir = scratch("target");
    let case = dir.join("case.ingot");
    copy_test("names/var_never_mutated.ingot", &case);
    replace(
        &case,
        "//! section: 7",
        "//! section: 7\n//! target: some-other-target",
    );
    let report = ingotc::conformance::run(&dir);
    assert_eq!(report.failed, 0);
    assert_eq!(report.passed, 0);
    assert_eq!(report.skipped, 1);

    replace(&case, "some-other-target", "aarch64-macos-m4");
    assert_passes(&dir);
}
