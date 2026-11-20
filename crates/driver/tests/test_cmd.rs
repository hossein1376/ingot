//! `ingot test` (tools.md section 3, 05 section 32): the test executable, its entry
//! `std.sys.rt.test_start`, and the harness that runs it once per test.
//!
//! Every scenario that depends on the code a test compiles to runs under both
//! pipelines, the default one (`--fast`) and `--opt`. A missing toolchain fails these
//! tests, as it does the conformance ratchet; nothing here skips.

use ingot_diag::json::{self, Json};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

fn scratch(label: &str) -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "ingot-test-cmd-{}-{label}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn write(dir: &Path, rel: &str, text: &str) {
    let path = dir.join(rel);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

/// What one `ingot test` run printed and returned.
struct Run {
    code: i32,
    stdout: String,
    stderr: String,
    /// The JSON lines of a `--json` run.
    lines: Vec<Json>,
}

impl Run {
    fn of(output: &Output, json: bool) -> Run {
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let lines = if json {
            stdout
                .lines()
                .map(|line| json::parse(line).unwrap_or_else(|e| panic!("{line}: {e:?}")))
                .collect()
        } else {
            Vec::new()
        };
        Run {
            code: output.status.code().expect("ingot ended by a signal"),
            stdout,
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            lines,
        }
    }

    fn tests(&self) -> Vec<&Json> {
        self.lines.iter().filter(|l| ty(l) == "test").collect()
    }

    fn summary(&self) -> &Json {
        let last = self.lines.last().expect("no lines");
        assert_eq!(ty(last), "summary", "the last line is the summary");
        last
    }

    fn names(&self) -> Vec<String> {
        self.tests().iter().map(|t| text(t, "name")).collect()
    }
}

fn command(args: &[&str], dir: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ingot"));
    command.args(args).current_dir(dir);
    command
}

/// `ingot test --json ARGS`, under `--opt` or not.
fn ingot_test(dir: &Path, args: &[&str], opt: bool) -> Run {
    let mut all = vec!["test", "--json"];
    if opt {
        all.push("--opt");
    }
    all.extend_from_slice(args);
    Run::of(&command(&all, dir).output().unwrap(), true)
}

fn ty(value: &Json) -> &str {
    value.get("type").and_then(Json::as_str).expect("a type")
}

fn text(value: &Json, key: &str) -> String {
    value
        .get(key)
        .and_then(Json::as_str)
        .unwrap_or_else(|| panic!("`{key}` is a string in {value:?}"))
        .to_string()
}

fn int(value: &Json, key: &str) -> i64 {
    value
        .get(key)
        .and_then(Json::as_i64)
        .unwrap_or_else(|| panic!("`{key}` is an integer in {value:?}"))
}

/// `{"line":L,"col":C,"offset":O}` as `(line, col, offset)`.
fn position(span: &Json, which: &str) -> (i64, i64, usize) {
    let at = span.get(which).expect("a position");
    (int(at, "line"), int(at, "col"), int(at, "offset") as usize)
}

/// The text a span covers in `source`.
fn covered<'a>(span: &Json, source: &'a str) -> &'a str {
    let (_, _, start) = position(span, "start");
    let (_, _, end) = position(span, "end");
    &source[start..end]
}

const BOTH: [bool; 2] = [false, true];

// ------------------------------------------------------------------ the fixture

const MAIN: &str = r#"import pkg.shapes;
import std.Env;
import std.testing;

extern "c" {
    fn abort() -> never;
    fn _exit(status: i32) -> never;
    fn access(path: *u8, mode: i32) -> i32;
}

pub fn main(env: &Env) -> void {
    _ = env;
}

enum ParseErr {
    empty,
    bad_digit: u8,
}

fn parse(x: i64) -> i64 ! ParseErr {
    if x == 0 {
        fail .empty;
    }
    return x;
}

fn check_it(x: i64) -> void ! ParseErr {
    if x == 0 {
        fail .bad_digit(120);
    }
}

test "passes and sees its environment" {
    let t = @test_env();
    let u = @test_env();
    let p: *testing.TestEnv = t as *testing.TestEnv;
    let q: *testing.TestEnv = u as *testing.TestEnv;
    @assert(p == q);
    @assert(t.dir.len > 0);
    @assert(t.env.args.len == 1);
    let dir: *u8 = t.dir.ptr;
    unsafe("the directory comes from argv, so it ends in NUL") {
        @assert(access(dir, 0) == 0);
    }
}

test "void result fails" {
    try check_it(0);
}

test "value result fails" {
    let v = try parse(0);
    _ = v;
}

test "hidden field prints as two dots" {
    let v = try shapes.make();
    _ = v;
}

test "index traps" {
    let a: [3]i64 = [3]i64.{ 1, 2, 3 };
    var i: i64 = 5;
    i += 0;
    _ = a[i];
}

test "exits with 7" {
    unsafe("leaves the process") {
        _exit(7);
    }
}

test "aborts" {
    unsafe("leaves the process") {
        abort();
    }
}
"#;

const SHAPES: &str = r#"pub struct Shape {
    pub w: i64,
    h: i64,
}

pub fn make() -> i64 ! Shape {
    fail Shape.{ w: 1, h: 2 };
}

test "own view of a private field" {
    let v = try make();
    _ = v;
}

test "shapes pass" {
    @assert(1 < 2);
}
"#;

fn fixture(label: &str) -> PathBuf {
    let dir = scratch(label);
    write(&dir, "ingot.toml", "name = \"pkg\"\n");
    write(&dir, "main.ingot", MAIN);
    write(&dir, "shapes.ingot", SHAPES);
    dir
}

/// (name, module, file, status) in the order the specification gives.
const EXPECTED: [(&str, &str, &str, &str); 9] = [
    ("passes and sees its environment", "pkg", "main.ingot", "ok"),
    ("void result fails", "pkg", "main.ingot", "failed"),
    ("value result fails", "pkg", "main.ingot", "failed"),
    (
        "hidden field prints as two dots",
        "pkg",
        "main.ingot",
        "failed",
    ),
    ("index traps", "pkg", "main.ingot", "trapped"),
    ("exits with 7", "pkg", "main.ingot", "crashed"),
    ("aborts", "pkg", "main.ingot", "crashed"),
    (
        "own view of a private field",
        "pkg.shapes",
        "shapes.ingot",
        "failed",
    ),
    ("shapes pass", "pkg.shapes", "shapes.ingot", "ok"),
];

fn by_name<'a>(run: &'a Run, name: &str) -> &'a Json {
    run.tests()
        .into_iter()
        .find(|t| text(t, "name") == name)
        .unwrap_or_else(|| panic!("no test line for {name:?}"))
}

#[test]
fn two_modules_run_in_order_with_their_spans_and_a_summary() {
    for opt in BOTH {
        let dir = fixture(&format!("order-{opt}"));
        let run = ingot_test(&dir, &[], opt);
        assert_eq!(run.code, 1, "opt={opt}: {}{}", run.stdout, run.stderr);
        // Test lines first, then exactly one summary.
        assert!(
            run.lines[..run.lines.len() - 1]
                .iter()
                .all(|l| ty(l) == "test")
        );
        let got: Vec<(String, String, String, String)> = run
            .tests()
            .iter()
            .map(|t| {
                (
                    text(t, "name"),
                    text(t, "module"),
                    text(t.get("span").unwrap(), "file"),
                    text(t, "status"),
                )
            })
            .collect();
        let want: Vec<(String, String, String, String)> = EXPECTED
            .iter()
            .map(|(a, b, c, d)| (a.to_string(), b.to_string(), c.to_string(), d.to_string()))
            .collect();
        assert_eq!(got, want, "opt={opt}");
        for test in run.tests() {
            let name = text(test, "name");
            let file = text(test.get("span").unwrap(), "file");
            let source = if file == "main.ingot" { MAIN } else { SHAPES };
            // The span is the whole `test` item, keyword to closing brace.
            let item = covered(test.get("span").unwrap(), source);
            assert!(item.starts_with(&format!("test \"{name}\" {{")), "{item}");
            assert!(item.ends_with('}'), "{item}");
            // `ms` is a float: written with a decimal point.
            let ms = test.get("ms").expect("ms");
            let Json::Number(written) = ms else {
                panic!("ms is a number: {ms:?}");
            };
            assert!(
                written.contains('.') && ms.as_f64().unwrap() >= 0.0,
                "{written}"
            );
        }
        let summary = run.summary();
        assert_eq!(summary.get("ok"), Some(&Json::Bool(false)));
        assert_eq!(text(summary, "command"), "test");
        assert_eq!(int(summary, "protocol"), 1);
        assert_eq!(int(summary, "passed"), 2, "opt={opt}");
        assert_eq!(int(summary, "failed"), 7, "opt={opt}");
        assert!(summary.get("seed").is_some());
        assert!(summary.get("ms").is_some());
    }
}

#[test]
fn failed_tests_carry_the_error_of_their_own_try() {
    for opt in BOTH {
        let dir = fixture(&format!("failed-{opt}"));
        let run = ingot_test(&dir, &[], opt);
        // A `try` on `void ! E` and on `T ! E`, two error types in one package; a
        // struct with a private field prints `..` from another module and in full from
        // its own.
        let errors = [
            ("void result fails", "ParseErr.bad_digit(120)"),
            ("value result fails", "ParseErr.empty"),
            ("hidden field prints as two dots", "Shape.{ w: 1, .. }"),
            ("own view of a private field", "Shape.{ w: 1, h: 2 }"),
        ];
        for (name, error) in errors {
            let line = by_name(&run, name);
            assert_eq!(text(line, "status"), "failed", "{name} (opt={opt})");
            assert_eq!(text(line, "error"), error, "{name} (opt={opt})");
        }
    }
}

#[test]
fn a_trap_names_its_code_text_and_position() {
    for opt in BOTH {
        let dir = fixture(&format!("trap-{opt}"));
        let run = ingot_test(&dir, &[], opt);
        let line = by_name(&run, "index traps");
        assert_eq!(text(line, "status"), "trapped");
        let trap = line.get("trap").expect("a trap");
        assert_eq!(text(trap, "code"), "T01");
        assert_eq!(text(trap, "text"), "index 5 out of bounds for length 3");
        let span = trap.get("span").expect("a span");
        assert_eq!(text(span, "file"), "main.ingot");
        // The line names a position and nothing more: the span is empty, and its
        // start is the byte `a[i]` begins at.
        let (line_no, col, start) = position(span, "start");
        let (_, _, end) = position(span, "end");
        assert_eq!(start, end);
        assert!(MAIN[start..].starts_with("a[i]"), "{:?}", &MAIN[start..]);
        let want_line = MAIN.lines().position(|l| l.contains("_ = a[i];")).unwrap() + 1;
        assert_eq!(line_no as usize, want_line);
        assert_eq!(
            col as usize,
            MAIN.lines()
                .nth(want_line - 1)
                .unwrap()
                .find("a[i]")
                .unwrap()
                + 1
        );
        // The trap is the ordinary trap path: it has a seed like every failure.
        assert!(line.get("seed").is_some());
    }
}

#[test]
fn a_process_that_ends_another_way_is_crashed() {
    for opt in BOTH {
        let dir = fixture(&format!("crash-{opt}"));
        let run = ingot_test(&dir, &[], opt);
        let exited = by_name(&run, "exits with 7");
        assert_eq!(text(exited, "status"), "crashed");
        assert_eq!(int(exited, "exit"), 7);
        assert!(exited.get("signal").is_none());
        // `abort` ends the process with SIGABRT.
        let aborted = by_name(&run, "aborts");
        assert_eq!(text(aborted, "status"), "crashed");
        assert_eq!(int(aborted, "signal"), 6);
        assert!(aborted.get("exit").is_none());
    }
}

#[test]
fn both_pipelines_report_the_same_results() {
    let dir = fixture("same");
    let strip = |run: &Run| -> Vec<String> {
        run.tests()
            .iter()
            .map(|t| {
                // Everything but the timing and the drawn seed.
                let Json::Object(members) = t else { panic!() };
                let kept: Vec<(String, Json)> = members
                    .iter()
                    .filter(|(k, _)| k != "ms" && k != "seed")
                    .cloned()
                    .collect();
                Json::Object(kept).to_text()
            })
            .collect()
    };
    let fast = ingot_test(&dir, &[], false);
    let optimized = ingot_test(&dir, &[], true);
    assert_eq!(strip(&fast), strip(&optimized));
}

#[test]
fn the_filter_keeps_the_tests_whose_name_contains_it() {
    for opt in BOTH {
        let dir = fixture(&format!("filter-{opt}"));
        let run = ingot_test(&dir, &["--filter", "result fails"], opt);
        assert_eq!(
            run.names(),
            ["void result fails", "value result fails"],
            "opt={opt}"
        );
        let summary = run.summary();
        assert_eq!(int(summary, "passed"), 0);
        assert_eq!(int(summary, "failed"), 2);
        // Tests that were not selected print nothing and are not counted.
        let run = ingot_test(&dir, &["--filter", "shapes pass"], opt);
        assert_eq!(run.names(), ["shapes pass"]);
        assert_eq!(run.code, 0);
        assert_eq!(int(run.summary(), "passed"), 1);
        assert_eq!(int(run.summary(), "failed"), 0);
        assert_eq!(run.summary().get("ok"), Some(&Json::Bool(true)));
        let run = ingot_test(&dir, &["--filter", "no such test"], opt);
        assert!(run.names().is_empty());
        assert_eq!(run.code, 0);
        assert_eq!(int(run.summary(), "passed"), 0);
    }
}

#[test]
fn the_seed_is_reported_on_failures_and_in_the_summary() {
    let dir = fixture("seed");
    let run = ingot_test(&dir, &["--seed", "7731"], false);
    for test in run.tests() {
        match text(test, "status").as_str() {
            "ok" => assert!(test.get("seed").is_none(), "{test:?}"),
            _ => assert_eq!(int(test, "seed"), 7731, "{test:?}"),
        }
    }
    assert_eq!(int(run.summary(), "seed"), 7731);
    // Without `--seed` one is drawn per run: the same on every line of the run.
    let run = ingot_test(&dir, &[], true);
    let seed = int(run.summary(), "seed");
    for test in run.tests() {
        match text(test, "status").as_str() {
            "ok" => assert!(test.get("seed").is_none(), "{test:?}"),
            _ => assert_eq!(int(test, "seed"), seed, "{test:?}"),
        }
    }
}

#[test]
fn a_seed_that_is_not_a_number_is_a_named_failure() {
    let dir = fixture("badseed");
    let output = command(&["test", "--json", "--seed", "soon"], &dir)
        .output()
        .unwrap();
    let run = Run::of(&output, true);
    assert_eq!(run.code, 2);
    assert_eq!(ty(&run.lines[0]), "error");
    assert_eq!(run.summary().get("ok"), Some(&Json::Bool(false)));
}

// -------------------------------------------------------------------- the temporary directory

const TMPDIRS: &str = r#"import std.testing;

extern "c" {
    fn _exit(status: i32) -> never;
    fn access(path: *u8, mode: i32) -> i32;
    fn chdir(path: *u8) -> i32;
    fn mkdir(path: *u8, mode: i32) -> i32;
    fn write(fd: i32, buf: *u8, n: i64) -> i64;
}

// The directory exists while the test runs, and it can hold something.
fn use_dir(t: &testing.TestEnv) -> void {
    let dir: *u8 = t.dir.ptr;
    let name: []u8 = "inner\0";
    let inner: *u8 = name.ptr;
    unsafe("libc calls on a NUL-terminated path from argv and on a literal") {
        @assert(access(dir, 0) == 0);
        @assert(chdir(dir) == 0);
        @assert(mkdir(inner, 493) == 0);
    }
}

// Tell the harness where the directory is.
fn say_dir(t: &testing.TestEnv) -> void {
    let dir: *u8 = t.dir.ptr;
    unsafe("write to standard output") {
        _ = write(1, dir, t.dir.len);
    }
}

fn boom() -> void ! i64 {
    fail 3;
}

test "passes" {
    use_dir(@test_env());
}

test "fails" {
    use_dir(@test_env());
    say_dir(@test_env());
    try boom();
}

test "traps" {
    use_dir(@test_env());
    var i: i64 = 0;
    i += 1;
    @assert(i == 0);
}

test "crashes" {
    use_dir(@test_env());
    unsafe("leaves the process") {
        _exit(3);
    }
}
"#;

fn entries(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[test]
fn the_harness_makes_the_directory_before_and_removes_it_after_every_outcome() {
    for opt in BOTH {
        let dir = scratch(&format!("tmpdir-{opt}"));
        write(&dir, "ingot.toml", "name = \"tmpdirs\"\n");
        write(&dir, "main.ingot", TMPDIRS);
        // The harness makes the directories under the system temporary directory;
        // give it one of its own to look at.
        let tmp = dir.join("tmp");
        fs::create_dir_all(&tmp).unwrap();
        let mut args = vec!["test"];
        if opt {
            args.push("--opt");
        }
        let output = command(&args, &dir).env("TMPDIR", &tmp).output().unwrap();
        let run = Run::of(&output, false);
        // The text form: a line per test, the output of those that did not pass, a
        // summary.
        assert_eq!(run.code, 1, "{}{}", run.stdout, run.stderr);
        assert!(run.stdout.contains("tmpdirs passes: ok"), "{}", run.stdout);
        assert!(
            run.stdout.contains("tmpdirs fails: FAILED  error: 3"),
            "{}",
            run.stdout
        );
        assert!(
            run.stdout.contains("tmpdirs traps: TRAPPED T08"),
            "{}",
            run.stdout
        );
        assert!(
            run.stdout.contains("tmpdirs crashes: CRASHED exit 3"),
            "{}",
            run.stdout
        );
        assert!(run.stdout.contains("stderr: error: 3"), "{}", run.stdout);
        assert!(
            run.stdout.contains("1 passed, 3 failed (seed "),
            "{}",
            run.stdout
        );
        // The directory the failing test reported was under the temporary directory,
        // existed while the test ran (its `access` and `mkdir` succeeded, so `passes`
        // is ok), and is gone now, `inner` and all, for every outcome.
        let reported = run
            .stdout
            .lines()
            .find_map(|l| l.trim().strip_prefix("stdout: "))
            .expect("the failing test printed its directory")
            .to_string();
        let reported = PathBuf::from(reported);
        let parent = reported.parent().unwrap().canonicalize().unwrap();
        assert_eq!(parent, tmp.canonicalize().unwrap());
        assert!(!reported.exists());
        let left: Vec<String> = entries(&tmp)
            .into_iter()
            .filter(|n| n.starts_with("ingot-test-"))
            .collect();
        assert!(left.is_empty(), "left behind: {left:?}");
    }
}

// ------------------------------------------------------------------ one file

#[test]
fn a_file_with_no_manifest_is_a_package_of_its_own() {
    for opt in BOTH {
        let dir = scratch(&format!("solo-{opt}"));
        write(
            &dir,
            "solo.ingot",
            "test \"solo works\" {\n    @assert(2 > 1);\n}\n\ntest \"solo fails\" {\n    @assert(2 < 1);\n}\n",
        );
        let run = ingot_test(&dir, &["solo.ingot"], opt);
        assert_eq!(run.code, 1, "{}{}", run.stdout, run.stderr);
        assert_eq!(run.names(), ["solo works", "solo fails"]);
        let ok = by_name(&run, "solo works");
        assert_eq!(text(ok, "status"), "ok");
        // Paths are bare file names.
        assert_eq!(text(ok.get("span").unwrap(), "file"), "solo.ingot");
        assert!(!text(ok, "module").is_empty());
        let failed = by_name(&run, "solo fails");
        assert_eq!(text(failed, "status"), "trapped");
        assert_eq!(
            text(failed.get("trap").unwrap(), "text"),
            "assertion failed: 2 < 1"
        );
        assert_eq!(int(run.summary(), "passed"), 1);
        assert_eq!(int(run.summary(), "failed"), 1);
    }
}

#[test]
fn a_file_of_a_package_runs_only_the_tests_of_its_module() {
    for opt in BOTH {
        let dir = fixture(&format!("file-{opt}"));
        let run = ingot_test(&dir, &["shapes.ingot"], opt);
        assert_eq!(
            run.names(),
            ["own view of a private field", "shapes pass"],
            "opt={opt}"
        );
        for test in run.tests() {
            assert_eq!(text(test, "module"), "pkg.shapes");
            assert_eq!(text(test.get("span").unwrap(), "file"), "shapes.ingot");
        }
        assert_eq!(int(run.summary(), "passed"), 1);
        assert_eq!(int(run.summary(), "failed"), 1);
        // From another directory, by path.
        let elsewhere = scratch(&format!("file-elsewhere-{opt}"));
        let path = dir.join("main.ingot");
        let run = ingot_test(
            &elsewhere,
            &[path.to_str().unwrap(), "--filter", "env"],
            opt,
        );
        assert_eq!(run.names(), ["passes and sees its environment"]);
        assert_eq!(run.code, 0, "{}{}", run.stdout, run.stderr);
    }
}

// ------------------------------------------------------------------ build outcomes

#[test]
fn a_build_error_prints_diagnostics_runs_nothing_and_exits_1() {
    for opt in BOTH {
        let dir = scratch(&format!("broken-{opt}"));
        write(&dir, "ingot.toml", "name = \"broken\"\n");
        write(
            &dir,
            "main.ingot",
            "test \"never runs\" {\n    let x: bool = 1;\n    _ = x;\n}\n\ntest \"neither\" {\n    @assert(true);\n}\n",
        );
        let run = ingot_test(&dir, &[], opt);
        assert_eq!(run.code, 1, "{}{}", run.stdout, run.stderr);
        assert!(run.tests().is_empty());
        assert_eq!(ty(&run.lines[0]), "diagnostic");
        let summary = run.summary();
        assert_eq!(summary.get("ok"), Some(&Json::Bool(false)));
        assert_eq!(int(summary, "errors"), 1);
        assert_eq!(int(summary, "passed"), 0);
        assert_eq!(int(summary, "failed"), 0);
        // No executable was made.
        assert!(!dir.join("target/ingot/broken-test").exists());
        // The text form prints the diagnostic as `check` does, and no test line.
        let output = command(&["test"], &dir).output().unwrap();
        let text_run = Run::of(&output, false);
        assert_eq!(text_run.code, 1);
        assert!(
            text_run.stdout.contains("main.ingot:2:"),
            "{}",
            text_run.stdout
        );
        assert!(
            !text_run.stdout.contains("never runs"),
            "{}",
            text_run.stdout
        );
    }
}

#[test]
fn a_file_whose_package_has_errors_elsewhere_prints_them_and_runs_nothing() {
    // `ingot test FILE` builds the package, so an error in another file stops it and
    // is printed, unlike `check FILE` (tools.md section 1). Before, the other file's
    // diagnostics were dropped and the build reached the back end: an internal error.
    let dir = scratch("elsewhere-broken");
    write(&dir, "ingot.toml", "name = \"pr\"\n");
    write(
        &dir,
        "main.ingot",
        "import pr.other;\n\ntest \"reaches the broken file\" {\n    @assert(other.broken() == 1);\n}\n",
    );
    write(
        &dir,
        "other.ingot",
        "pub fn broken() -> i64 {\n    return true;\n}\n",
    );
    let run = ingot_test(&dir, &["main.ingot"], false);
    assert_eq!(run.code, 1, "{}{}", run.stdout, run.stderr);
    assert!(run.tests().is_empty());
    assert_eq!(ty(&run.lines[0]), "diagnostic");
    assert_eq!(
        text(run.lines[0].get("span").unwrap(), "file"),
        "other.ingot"
    );
    assert_eq!(int(run.summary(), "errors"), 1);
    // `check` of the same file still reports only that file.
    let output = command(&["check", "--json", "main.ingot"], &dir)
        .output()
        .unwrap();
    let check = Run::of(&output, true);
    assert_eq!(check.code, 0, "{}", check.stdout);
}

#[test]
fn a_construct_the_back_end_lacks_is_a_compiler_failure_exit_2() {
    // A `try` whose error is a float needs the structural form of a float (R5).
    let dir = scratch("float");
    write(&dir, "ingot.toml", "name = \"floaty\"\n");
    write(
        &dir,
        "main.ingot",
        "fn f() -> void ! f64 {\n    fail 1.5;\n}\n\ntest \"float error\" {\n    try f();\n}\n",
    );
    let run = ingot_test(&dir, &[], false);
    assert_eq!(run.code, 2, "{}{}", run.stdout, run.stderr);
    assert!(run.tests().is_empty());
    assert_eq!(ty(&run.lines[0]), "error");
    assert!(text(&run.lines[0], "message").contains("not implemented"));
    assert_eq!(run.summary().get("ok"), Some(&Json::Bool(false)));
}

#[test]
fn a_package_with_no_tests_succeeds_with_none() {
    let dir = scratch("none");
    write(&dir, "ingot.toml", "name = \"none\"\n");
    write(
        &dir,
        "main.ingot",
        "import std.Env;\n\npub fn main(env: &Env) -> void {\n    _ = env;\n}\n",
    );
    let run = ingot_test(&dir, &[], false);
    assert_eq!(run.code, 0, "{}{}", run.stdout, run.stderr);
    assert!(run.tests().is_empty());
    let summary = run.summary();
    assert_eq!(summary.get("ok"), Some(&Json::Bool(true)));
    assert_eq!(int(summary, "passed"), 0);
    assert_eq!(int(summary, "failed"), 0);
}

#[test]
fn the_standard_library_has_no_tests() {
    let std = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../std");
    let run = ingot_test(&std, &[], false);
    assert_eq!(run.code, 0, "{}{}", run.stdout, run.stderr);
    assert!(run.tests().is_empty());
    assert_eq!(run.summary().get("ok"), Some(&Json::Bool(true)));
    assert_eq!(int(run.summary(), "passed"), 0);
}

#[test]
fn the_tests_of_a_dependency_are_not_built_or_run() {
    for opt in BOTH {
        let dir = scratch(&format!("dep-{opt}"));
        write(
            &dir,
            "ingot.toml",
            "name = \"app\"\n\n[deps]\nlib = { path = \"lib\" }\n",
        );
        write(
            &dir,
            "main.ingot",
            "import lib;\n\ntest \"uses the dependency\" {\n    @assert(lib.twice(3) == 6);\n}\n",
        );
        write(&dir, "lib/ingot.toml", "name = \"lib\"\n");
        write(
            &dir,
            "lib/main.ingot",
            "pub fn twice(x: i64) -> i64 {\n    return x * 2;\n}\n\ntest \"the dependency's own test\" {\n    @assert(false);\n}\n",
        );
        let run = ingot_test(&dir, &[], opt);
        assert_eq!(run.code, 0, "opt={opt}: {}{}", run.stdout, run.stderr);
        assert_eq!(run.names(), ["uses the dependency"]);
        assert_eq!(int(run.summary(), "passed"), 1);
        assert_eq!(int(run.summary(), "failed"), 0);
        // Not built either: the dependency's test is in no object of the executable.
        let ir = fs::read_to_string(dir.join("target/ingot/app-test.ll")).unwrap();
        assert!(
            !ir.contains("dependency's own test"),
            "the test was compiled"
        );
    }
}

#[test]
fn build_and_run_still_build_no_test_code() {
    for fast in BOTH {
        let dir = scratch(&format!("noteststuff-{fast}"));
        write(&dir, "ingot.toml", "name = \"prog\"\n");
        write(
            &dir,
            "main.ingot",
            "import std.Env;\n\npub fn main(env: &Env) -> void {\n    _ = env;\n}\n\ntest \"only for ingot test\" {\n    @assert(false);\n}\n",
        );
        let mut args = vec!["build"];
        if fast {
            args.push("--fast");
        }
        let output = command(&args, &dir).output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let ir = fs::read_to_string(dir.join("target/ingot/prog.ll")).unwrap();
        assert!(!ir.contains("only for ingot test"), "a test was compiled");
        assert!(!ir.contains("test_start"), "the test entry was compiled");
        assert!(!ir.contains("_I.tests"), "the test table was emitted");
        // `ingot run` runs `main`, not the tests: it exits 0 although the test fails.
        let output = command(&["run"], &dir).output().unwrap();
        assert_eq!(output.status.code(), Some(0));
    }
}

#[test]
fn a_test_text_run_prints_a_line_per_test_and_the_output_of_failures() {
    let dir = fixture("text");
    let output = command(&["test", "--filter", "fails"], &dir)
        .output()
        .unwrap();
    let run = Run::of(&output, false);
    assert_eq!(run.code, 1);
    assert!(
        run.stdout
            .contains("pkg void result fails: FAILED  error: ParseErr.bad_digit(120)")
    );
    assert!(
        run.stdout
            .contains("    stderr: error: ParseErr.bad_digit(120)")
    );
    let last = run.stdout.lines().last().unwrap();
    assert!(last.starts_with("0 passed, 2 failed (seed "), "{last}");
}
