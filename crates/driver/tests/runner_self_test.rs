//! The conformance runner's own tests. Each mutant of a copied test must fail;
//! the unmutated copy must pass.
//!
//! The fixtures are lex, syntax, reject and cost tests, which the front end decides
//! on its own, and one trap test, which the back end now builds and runs.

use ingot_driver::conformance::{self, read_case};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

/// One marker, on line 5 (`a + b;`). Line 4 is an innocent line.
const REJECT: &str = "syntax/not_a_statement.ingot";
/// A reject test whose one diagnostic has a fix, and whose fixed result declares
/// nothing, so that it is an `accept` test the compiler can pass today.
const REJECT_WITH_FIX: &str = "names/unused_import.ingot";
/// The same test, for the mutation of its `.fixed.ingot` partner.
const REJECT_WITH_FIXED: &str = "names/unused_import.ingot";
const TRAP: &str = "arith/index_out_of_bounds.ingot";
const COST: &str = "cost/unproven_index_keeps_check.ingot";

fn conformance_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../conformance")
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
    let text = fs::read_to_string(conformance_root().join(relative)).unwrap();
    fs::write(destination, &text).unwrap();
    text
}

fn copy_fixed(relative: &str, destination: &Path) -> String {
    let source = conformance_root()
        .join(relative)
        .with_extension("fixed.ingot");
    let text = fs::read_to_string(&source).unwrap();
    fs::write(destination, &text).unwrap();
    text
}

fn assert_passes(dir: &Path) {
    let report = conformance::run(dir);
    for failure in &report.failures {
        eprintln!("FAIL {}: {}", failure.path.display(), failure.message);
    }
    assert_eq!(report.failed, 0, "unmutated copy must pass");
    assert!(report.passed > 0, "unmutated copy must be discovered");
}

fn assert_fails(dir: &Path, why: &str) {
    let report = conformance::run(dir);
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
    copy_test(REJECT, &case);
    assert_passes(&dir);
    replace(&case, "//~ E0206", "//~ E0207");
    assert_fails(&dir, "a changed reject code");
}

#[test]
fn missing_reject_marker_fails() {
    let dir = scratch("reject-missing");
    let case = dir.join("case.ingot");
    copy_test(REJECT, &case);
    assert_passes(&dir);
    replace(&case, " //~ E0206", "");
    assert_fails(&dir, "a removed reject marker");
}

#[test]
fn extra_marker_on_an_innocent_line_fails() {
    let dir = scratch("extra-marker");
    let case = dir.join("case.ingot");
    let text = copy_test(REJECT, &case);
    assert_passes(&dir);
    let mutated = text.replacen(
        "fn f(a: i64, b: i64) {",
        "fn f(a: i64, b: i64) { //~ E0206",
        1,
    );
    fs::write(&case, mutated).unwrap();
    assert_fails(&dir, "an extra marker");
}

#[test]
fn changed_trap_code_fails() {
    // The unmutated copy builds, runs and traps at the marked position. Changing
    // the marker makes the runner compare the program's real trap code with a
    // different expectation, so the mutant must fail.
    let dir = scratch("trap-code");
    let case = dir.join("case.ingot");
    copy_test(TRAP, &case);
    assert_passes(&dir);
    replace(&case, "//~ T01", "//~ T02");
    assert!(read_case(&case).markers.iter().any(|m| m.code == "T02"));
    assert_fails(&dir, "a changed trap code marker");
}

#[test]
fn missing_cost_marker_fails() {
    let dir = scratch("cost-missing");
    let case = dir.join("case.ingot");
    copy_test(COST, &case);
    assert_passes(&dir);
    let before = read_case(&case).markers.len();
    replace(&case, " //~ bounds_check", "");
    assert_eq!(read_case(&case).markers.len(), before - 1);
    assert_fails(&dir, "a removed cost marker");
}

#[test]
fn extra_cost_marker_fails() {
    let dir = scratch("cost-extra");
    let case = dir.join("case.ingot");
    let text = copy_test(COST, &case);
    assert_passes(&dir);
    let before = read_case(&case).markers.len();
    let mutated = text.replacen(
        "fn pick(xs: []i64, i: i64) -> i64 {",
        "fn pick(xs: []i64, i: i64) -> i64 { //~ bounds_check",
        1,
    );
    fs::write(&case, mutated).unwrap();
    assert_eq!(read_case(&case).markers.len(), before + 1);
    assert_fails(&dir, "an extra cost marker");
}

#[test]
fn altered_fixed_file_fails() {
    let dir = scratch("fixed-altered");
    let case = dir.join("case.ingot");
    let fixed = dir.join("case.fixed.ingot");
    copy_test(REJECT_WITH_FIXED, &case);
    let text = copy_fixed(REJECT_WITH_FIXED, &fixed);
    assert_passes(&dir);
    // A comment keeps the fixed file an acceptable program but changes its text.
    fs::write(&fixed, format!("{text}// altered\n")).unwrap();
    let report = conformance::run(&dir);
    assert!(
        report
            .failures
            .iter()
            .any(|f| f.message.contains("fixed output differs")),
        "a change to .fixed.ingot must be seen: {:?}",
        report.failures
    );
}

#[test]
fn original_that_no_longer_needs_the_fix_fails() {
    let dir = scratch("fixed-stale");
    let case = dir.join("case.ingot");
    let fixed = dir.join("case.fixed.ingot");
    copy_test(REJECT_WITH_FIX, &case);
    copy_fixed(REJECT_WITH_FIX, &fixed);
    assert_passes(&dir);
    // Make the original acceptable already, so `fix` applies nothing, and add a
    // line the `.fixed.ingot` does not have, so the remaining text still differs.
    let text = fs::read_to_string(&case).unwrap();
    let text = text
        .replace("//! expect: reject", "//! expect: accept")
        .replace("import std.Env; //~ E0310\n", "// extra\n");
    fs::write(&case, text).unwrap();
    let report = conformance::run(&dir);
    assert!(
        report
            .failures
            .iter()
            .any(|f| f.message.contains("fixed output differs")),
        "an original with no fix left to apply must fail: {:?}",
        report.failures
    );
}

#[test]
fn unknown_expect_value_fails() {
    let dir = scratch("bad-expect");
    let case = dir.join("case.ingot");
    copy_test(REJECT, &case);
    assert_passes(&dir);
    replace(&case, "expect: reject", "expect: maybe");
    assert_fails(&dir, "an unknown expect value");
}

#[test]
fn unknown_directive_fails() {
    let dir = scratch("bad-directive");
    let case = dir.join("case.ingot");
    copy_test(REJECT, &case);
    assert_passes(&dir);
    replace(&case, "//! section: 7", "//! section: 7\n//! mystery: 1");
    assert_fails(&dir, "an unknown directive");
}

#[test]
fn marker_kind_must_fit_expectation() {
    let dir = scratch("bad-marker-kind");
    let case = dir.join("case.ingot");
    copy_test(REJECT, &case);
    assert_passes(&dir);
    replace(&case, "//~ E0206", "//~ T01");
    assert_fails(&dir, "a trap marker in a reject test");
}

#[test]
fn other_target_is_skipped_and_this_target_runs() {
    let dir = scratch("target");
    let case = dir.join("case.ingot");
    copy_test(REJECT, &case);
    replace(
        &case,
        "//! section: 7",
        "//! section: 7\n//! target: some-other-target",
    );
    let report = conformance::run(&dir);
    assert_eq!(report.failed, 0);
    assert_eq!(report.passed, 0);
    assert_eq!(report.skipped, 1);

    replace(&case, "some-other-target", "aarch64-macos-m4");
    assert_passes(&dir);
}

const ERROR_LINE: &str = "errors/main_error_enum_payload.ingot";

#[test]
fn changed_stderr_line_fails_and_an_absent_one_is_not_compared() {
    let dir = scratch("stderr-line");
    let case = dir.join("case.ingot");
    copy_test(ERROR_LINE, &case);
    assert_passes(&dir);
    replace(
        &case,
        "//! stderr: error: ParseErr.bad_digit(120)",
        "//! stderr: error: ParseErr.bad_digit(121)",
    );
    assert_fails(&dir, "a changed stderr line");
    // A line too many or too few is a difference as well.
    replace(
        &case,
        "//! stderr: error: ParseErr.bad_digit(121)",
        "//! stderr: error: ParseErr.bad_digit(120)\n//! stderr: another line",
    );
    assert_fails(&dir, "an extra stderr line");
    replace(
        &case,
        "//! stderr: error: ParseErr.bad_digit(120)\n//! stderr: another line\n",
        "",
    );
    assert_passes(&dir);
}

#[test]
fn stderr_directive_outside_a_run_test_is_malformed() {
    let dir = scratch("stderr-reject");
    let case = dir.join("case.ingot");
    copy_test(REJECT, &case);
    assert_passes(&dir);
    replace(&case, "//! section: 7", "//! section: 7\n//! stderr: text");
    let report = conformance::run(&dir);
    assert!(
        report
            .failures
            .iter()
            .any(|f| f.message.contains("fits only a `run` test")),
        "{:?}",
        report.failures
    );
}
