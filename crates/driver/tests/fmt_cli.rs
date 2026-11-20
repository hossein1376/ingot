//! `ingot fmt` command behaviour.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

fn scratch(label: &str) -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "ingot-fmt-{}-{label}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn package(dir: &Path, name: &str, deps: &str, main: &str) {
    fs::create_dir_all(dir).unwrap();
    fs::write(dir.join("ingot.toml"), format!("name = \"{name}\"\n{deps}")).unwrap();
    fs::write(dir.join("main.ingot"), main).unwrap();
}

fn ingot_stdin(args: &[&str], dir: &Path, text: &str) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_ingot"))
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(text.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

fn ingot(args: &[&str], dir: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_ingot"))
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap()
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

const BAD: &str = "pub fn f(x:i64)->i64 { let y=x*2; return y+1; }\n";
const GOOD: &str = "pub fn f(x: i64) -> i64 {\n    let y = x * 2;\n    return y + 1;\n}\n";

#[test]
fn check_reports_and_writes_the_formatted_file() {
    let dir = scratch("package");
    package(&dir, "app", "", BAD);
    let check = ingot(&["fmt", "--check", "--json"], &dir);
    assert_eq!(check.status.code(), Some(1));
    assert!(stdout(&check).contains("\"type\":\"unformatted\""));
    assert_eq!(fs::read_to_string(dir.join("main.ingot")).unwrap(), BAD);

    let format = ingot(&["fmt", "--json"], &dir);
    assert_eq!(format.status.code(), Some(0), "{}", stdout(&format));
    assert!(stdout(&format).contains("\"type\":\"formatted\""));
    assert_eq!(fs::read_to_string(dir.join("main.ingot")).unwrap(), GOOD);
    let again = ingot(&["fmt", "--check", "--json"], &dir);
    assert_eq!(again.status.code(), Some(0));
}

#[test]
fn stdin_writes_text_and_leaves_a_bad_file_unchanged() {
    let dir = scratch("stdin");
    let output = ingot_stdin(&["fmt", "--stdin"], &dir, BAD);
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(stdout(&output), GOOD);
    let broken = "pub fn broken( {\n";
    let bad = ingot_stdin(&["fmt", "--stdin"], &dir, broken);
    assert_eq!(bad.status.code(), Some(1));
    assert_eq!(stdout(&bad), broken);
}

#[test]
fn a_file_with_a_syntax_error_is_left_alone() {
    let dir = scratch("syntax");
    let path = dir.join("bad.ingot");
    let text = "pub fn broken( {\n";
    fs::write(&path, text).unwrap();
    let output = ingot(&["fmt", "--json", "bad.ingot"], &dir);
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(fs::read_to_string(&path).unwrap(), text);
}

#[test]
fn a_dependency_is_never_formatted() {
    let dir = scratch("dependency");
    let app = dir.join("app");
    let dep = dir.join("dep");
    package(
        &app,
        "app",
        "dep = { path = \"../dep\" }\n",
        "pub fn f() -> i64 { return 1; }\n",
    );
    package(&dep, "dep", "", BAD);
    let output = ingot(&["fmt", "--json"], &app);
    assert_eq!(output.status.code(), Some(0), "{}", stdout(&output));
    assert_eq!(fs::read_to_string(dep.join("main.ingot")).unwrap(), BAD);
}

#[test]
fn a_workspace_root_formats_every_member() {
    let dir = scratch("workspace");
    fs::create_dir_all(dir.join("tools/a")).unwrap();
    fs::create_dir_all(dir.join("tools/b")).unwrap();
    fs::write(
        dir.join("ingot.toml"),
        "[workspace]\nmembers = [\"tools/*\"]\n",
    )
    .unwrap();
    package(&dir.join("tools/a"), "a", "", BAD);
    package(&dir.join("tools/b"), "b", "", BAD);
    let output = ingot(&["fmt", "--json"], &dir);
    assert_eq!(output.status.code(), Some(0), "{}", stdout(&output));
    assert_eq!(
        fs::read_to_string(dir.join("tools/a/main.ingot")).unwrap(),
        GOOD
    );
    assert_eq!(
        fs::read_to_string(dir.join("tools/b/main.ingot")).unwrap(),
        GOOD
    );
}

#[test]
fn a_lone_file_is_its_own_package() {
    let dir = scratch("lone");
    let path = dir.join("test.ingot");
    fs::write(&path, BAD).unwrap();
    let output = ingot(&["fmt", "--json", "test.ingot"], &dir);
    assert_eq!(output.status.code(), Some(0), "{}", stdout(&output));
    assert_eq!(fs::read_to_string(&path).unwrap(), GOOD);
}

#[test]
fn a_module_named_fixed_is_formatted_like_any_other() {
    // `x.fixed.ingot` is a test-harness convention only; in a package it is a module.
    let dir = scratch("fixed-module");
    package(&dir, "app", "", GOOD);
    fs::write(dir.join("x.fixed.ingot"), BAD).unwrap();
    let output = ingot(&["fmt", "--check", "--json"], &dir);
    assert_eq!(output.status.code(), Some(1), "{}", stdout(&output));
    assert!(
        stdout(&output).contains("\"type\":\"unformatted\",\"file\":\"x.fixed.ingot\""),
        "{}",
        stdout(&output)
    );
    let _ = fs::remove_dir_all(&dir);
}
