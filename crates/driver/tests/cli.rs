//! The `ingot check` command: exit status, the target selection, the manifest, and how
//! a package that cannot be fully checked yet is reported.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

fn scratch(label: &str) -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "ingot-cli-{}-{label}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn package(label: &str, manifest: &str, main: &str) -> PathBuf {
    let dir = scratch(label);
    fs::write(dir.join("ingot.toml"), manifest).unwrap();
    fs::write(dir.join("main.ingot"), main).unwrap();
    dir
}

fn ingot(args: &[&str], dir: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_ingot"))
        .args(args)
        .arg(dir)
        .output()
        .unwrap()
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

#[test]
fn a_package_with_nothing_to_check_is_accepted() {
    let dir = package("empty", "name = \"app\"\n", "import std.Env;\n");
    // The import is unused, which is an error.
    let o = ingot(&["check"], &dir);
    assert_eq!(o.status.code(), Some(1), "{}", stdout(&o));
    assert!(stdout(&o).contains("E0310"));

    let dir = package("empty2", "name = \"app\"\n", "// nothing here\n");
    let o = ingot(&["check", "--json"], &dir);
    assert_eq!(o.status.code(), Some(0), "{}{}", stdout(&o), stderr(&o));
    let out = stdout(&o);
    assert!(out.contains("\"ok\":true"), "{out}");
    assert!(!out.contains("not_implemented"), "{out}");
}

#[test]
fn checking_one_file_of_a_package_prints_only_that_files_diagnostics() {
    let dir = package(
        "one-file",
        "name = \"app\"\n",
        "fn main_error() -> i64 {\n    return true;\n}\n",
    );
    fs::write(
        dir.join("other.ingot"),
        "fn other_error() -> i64 {\n    return true;\n}\n",
    )
    .unwrap();
    let o = Command::new(env!("CARGO_BIN_EXE_ingot"))
        .args(["check"])
        .arg(dir.join("main.ingot"))
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(1), "{}", stdout(&o));
    let out = stdout(&o);
    assert!(out.contains("main.ingot"), "{out}");
    assert!(!out.contains("other.ingot"), "{out}");

    let o = Command::new(env!("CARGO_BIN_EXE_ingot"))
        .args(["check"])
        .arg(dir.join("other.ingot"))
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(1), "{}", stdout(&o));
    let out = stdout(&o);
    assert!(out.contains("other.ingot"), "{out}");
    assert!(!out.contains("main.ingot"), "{out}");
}

#[test]
fn a_relative_file_path_finds_the_manifest_above_it() {
    let dir = package(
        "relative",
        "name = \"app\"\n",
        "fn a() -> i64 { return 1; }\n",
    );
    fs::create_dir_all(dir.join("sub")).unwrap();
    fs::write(
        dir.join("sub/other.ingot"),
        "fn b() -> i64 {\n    return true;\n}\n",
    )
    .unwrap();
    // From the package root and from the file's own directory, the path printed is
    // relative to the package root.
    for (cwd, arg) in [
        (dir.clone(), "sub/other.ingot"),
        (dir.join("sub"), "other.ingot"),
    ] {
        let o = Command::new(env!("CARGO_BIN_EXE_ingot"))
            .args(["check", arg])
            .current_dir(&cwd)
            .output()
            .unwrap();
        assert_eq!(o.status.code(), Some(1), "{}", stdout(&o));
        assert!(stdout(&o).contains("sub/other.ingot:2:"), "{}", stdout(&o));
    }
}

#[test]
fn checking_one_file_still_prints_the_manifests_diagnostics() {
    let dir = package(
        "one-file-manifest",
        "name = 3\n",
        "fn a() -> i64 { return 1; }\n",
    );
    let o = ingot(&["check"], &dir.join("main.ingot"));
    assert_eq!(o.status.code(), Some(1), "{}", stdout(&o));
    assert!(stdout(&o).contains("E1701"), "{}", stdout(&o));
}

#[test]
fn a_package_with_checked_bodies_is_accepted() {
    let dir = package("decl", "name = \"app\"\n", "fn f() -> i64 { return 1; }\n");
    let o = ingot(&["check", "--json"], &dir);
    assert_eq!(o.status.code(), Some(0), "{}", stdout(&o));
    let out = stdout(&o);
    let summary = out.lines().last().unwrap();
    assert!(summary.contains("\"ok\":true"), "{summary}");
    assert!(!summary.contains("\"not_implemented\""), "{summary}");
    assert!(summary.contains("\"errors\":0"), "{summary}");
    assert!(summary.contains("\"files\":1"), "{summary}");
}

#[test]
fn a_package_of_checked_declarations_is_accepted() {
    // Phases 1 to 6 check every item here: types, a constant, an alias, an interface
    // and a foreign declaration. There is no body and no constant outside the
    // evaluated subset.
    let main = "const width: i64 = 2 + 2;\n\
                struct Row { cells: [width]u8 }\n\
                type Rows = [3]Row;\n\
                interface Measure { fn measure(self: &Self) -> i64; }\n\
                extern \"c\" { fn abs(x: i32) -> i32; }\n";
    let dir = package("decls-only", "name = \"app\"\n", main);
    let o = ingot(&["check", "--json"], &dir);
    assert_eq!(o.status.code(), Some(0), "{}{}", stdout(&o), stderr(&o));
    assert!(stdout(&o).contains("\"ok\":true"), "{}", stdout(&o));
    assert!(!stdout(&o).contains("not_implemented"));
}

#[test]
fn a_declaration_error_is_a_rejection() {
    let dir = package(
        "decl-error",
        "name = \"app\"\n",
        "const too_big: u8 = 255 + 1;\n",
    );
    let o = ingot(&["check", "--json"], &dir);
    assert_eq!(o.status.code(), Some(1));
    assert!(stdout(&o).contains("\"code\":\"E0806\""), "{}", stdout(&o));
}

#[test]
fn a_struct_constant_is_evaluated() {
    // A struct value is outside the direct evaluator's subset; the interpreter
    // evaluates it, so the package has nothing left unchecked.
    let dir = package(
        "const-eval",
        "name = \"app\"\n",
        "struct P { x: i64 }\nconst origin: P = P.{ x: 0 };\n",
    );
    let o = ingot(&["check", "--json"], &dir);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    assert!(stdout(&o).contains("\"ok\":true"), "{}", stdout(&o));
}

#[test]
fn an_error_is_a_rejection_even_when_checking_is_incomplete() {
    let dir = package(
        "reject",
        "name = \"app\"\n",
        "fn f() -> i64 { return nope; }\n",
    );
    let o = ingot(&["check", "--json"], &dir);
    assert_eq!(o.status.code(), Some(1));
    let out = stdout(&o);
    assert!(out.contains("\"code\":\"E0304\""), "{out}");
    assert!(out.contains("\"ok\":false"), "{out}");
}

#[test]
fn the_target_selects_the_branch_of_a_when() {
    let main = "when @target.os == .macos {\n\
                fn here() -> i64 { return 1; }\n\
                } else {\n\
                fn there() -> i64 { return 2; }\n\
                }\n\
                fn f() -> i64 { return here(); }\n";
    let dir = package("when", "name = \"app\"\n", main);
    // The default target is the first of section 24, a macOS machine.
    let o = ingot(&["check", "--json"], &dir);
    assert!(!stdout(&o).contains("E0304"), "{}", stdout(&o));
    assert!(stdout(&o).contains("\"targets\":[\"aarch64-macos-m4\"]"));
    let o = ingot(
        &["check", "--json", "--target", "x86_64-linux-znver4"],
        &dir,
    );
    let out = stdout(&o);
    assert!(out.contains("\"code\":\"E0304\""), "{out}");
    assert!(
        out.contains("\"targets\":[\"x86_64-linux-znver4\"]"),
        "{out}"
    );
    assert_eq!(o.status.code(), Some(1));
}

#[test]
fn an_unknown_target_is_e1702() {
    let dir = package("target", "name = \"app\"\n", "// nothing\n");
    let o = ingot(&["check", "--json", "--target", "riscv64"], &dir);
    assert_eq!(o.status.code(), Some(1));
    let out = stdout(&o);
    assert!(out.contains("\"code\":\"E1702\""), "{out}");
    assert!(out.lines().last().unwrap().contains("\"ok\":false"));
}

#[test]
fn all_targets_is_not_implemented() {
    let dir = package("all", "name = \"app\"\n", "// nothing\n");
    let o = ingot(&["check", "--all-targets"], &dir);
    assert_eq!(o.status.code(), Some(2));
    assert!(stderr(&o).contains("not implemented"));
}

#[test]
fn a_missing_manifest_is_e1701_on_the_command_line_but_not_in_the_harness() {
    let dir = scratch("nomanifest");
    fs::write(dir.join("main.ingot"), "// nothing\n").unwrap();
    let o = ingot(&["check", "--json"], &dir);
    assert_eq!(o.status.code(), Some(1));
    let out = stdout(&o);
    assert!(out.contains("\"code\":\"E1701\""), "{out}");
    assert!(out.contains("\"file\":\"ingot.toml\""), "{out}");

    let options = ingot_driver::pipeline::Options {
        load: ingot_driver::loader::LoadOptions {
            harness: true,
            ..Default::default()
        },
        ..Default::default()
    };
    let a = ingot_driver::pipeline::analyze_path_with(&dir, &options).unwrap();
    assert!(a.diagnostics.is_empty());
}

#[test]
fn dependencies_are_not_implemented_yet() {
    let dir = package(
        "deps",
        "name = \"app\"\n[deps]\ncsv = { path = \"../csv\" }\n",
        "// nothing\n",
    );
    let o = ingot(&["check"], &dir);
    assert_eq!(o.status.code(), Some(2));
    assert!(
        stderr(&o).contains("not implemented: dependencies"),
        "{}",
        stderr(&o)
    );
}

#[test]
fn a_bad_manifest_is_reported_and_the_package_is_not_resolved() {
    let dir = package(
        "badtoml",
        "name = \"app\"\nbogus = 1\n",
        "fn f() -> i64 { return nope; }\n",
    );
    let o = ingot(&["check", "--json"], &dir);
    assert_eq!(o.status.code(), Some(1));
    let out = stdout(&o);
    assert!(out.contains("\"code\":\"E1701\""), "{out}");
    // The package name is not known, so names are not resolved.
    assert!(!out.contains("E0304"), "{out}");
}

#[test]
fn a_missing_path_with_json_writes_an_error_line_and_a_summary() {
    let dir = scratch("missing-json");
    let missing = dir.join("nope");
    let o = Command::new(env!("CARGO_BIN_EXE_ingot"))
        .args(["check", "--json"])
        .arg(&missing)
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(2));
    let out = stdout(&o);
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 2, "{out}");
    assert!(
        lines[0].starts_with("{\"type\":\"error\",\"message\":\""),
        "{out}"
    );
    assert!(lines[0].contains("does not exist"), "{out}");
    assert_eq!(
        lines[1].rsplit_once("\"ms\":").map(|(head, _)| head),
        Some("{\"type\":\"summary\",\"command\":\"check\",\"ok\":false,\"protocol\":1,"),
        "{out}"
    );
}

#[test]
fn an_unknown_command_with_json_writes_an_error_line_and_a_summary() {
    let o = Command::new(env!("CARGO_BIN_EXE_ingot"))
        .args(["bogus", "--json"])
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(2));
    let out = stdout(&o);
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 2, "{out}");
    assert!(lines[0].contains("unknown command `bogus`"), "{out}");
    assert!(
        lines[1].starts_with(
            "{\"type\":\"summary\",\"command\":\"bogus\",\"ok\":false,\"protocol\":1,"
        ),
        "{out}"
    );
}

#[test]
fn all_targets_with_json_writes_an_error_line_and_a_summary() {
    let dir = package("all-json", "name = \"app\"\n", "// nothing\n");
    let o = ingot(&["check", "--json", "--all-targets"], &dir);
    assert_eq!(o.status.code(), Some(2));
    let out = stdout(&o);
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 2, "{out}");
    assert!(
        lines[0].contains("not implemented: `check --all-targets`"),
        "{out}"
    );
    assert!(
        lines[1].starts_with(
            "{\"type\":\"summary\",\"command\":\"check\",\"ok\":false,\"protocol\":1,"
        ),
        "{out}"
    );
}

#[test]
fn the_output_is_the_same_on_every_run() {
    let dir = package(
        "determinism",
        "name = \"app\"\n",
        "fn f() -> i64 { return nope + nada; }\nfn g(x: Nothing) { }\n",
    );
    let first = stdout(&ingot(&["check", "--json"], &dir));
    for _ in 0..3 {
        let again = stdout(&ingot(&["check", "--json"], &dir));
        let strip = |s: &str| -> String {
            s.lines()
                .map(|l| match l.find("\"ms\":") {
                    Some(i) => l[..i].to_string(),
                    None => l.to_string(),
                })
                .collect::<Vec<_>>()
                .join("\n")
        };
        assert_eq!(strip(&first), strip(&again));
    }
}

#[test]
fn cost_prints_surviving_checks_and_exits_clean() {
    let dir = package(
        "cost",
        "name = \"app\"\n",
        "fn first(xs: []i64, i: i64) -> i64 {\n    return xs[i];\n}\n\nfn second() -> i64 {\n    return 2 + 3;\n}\n",
    );
    let o = ingot(&["cost", "--json"], &dir);
    assert_eq!(o.status.code(), Some(0), "{}{}", stdout(&o), stderr(&o));
    let out = stdout(&o);
    assert!(
        out.contains("\"type\":\"cost\",\"kind\":\"bounds_check\",\"fn\":\"app.first\""),
        "{out}"
    );
    assert!(!out.contains("app.second"), "{out}");
    let summary = out.lines().last().unwrap();
    assert!(summary.contains("\"checks\":1"), "{summary}");
    assert!(summary.contains("\"not_implemented\":[]"), "{summary}");
    assert!(!stderr(&o).contains("not implemented"), "{}", stderr(&o));

    let o = ingot(&["cost", "--json", "--fn", "app.second"], &dir);
    let out = stdout(&o);
    assert!(!out.contains("\"type\":\"cost\""), "{out}");
    assert!(
        out.lines().last().unwrap().contains("\"checks\":0"),
        "{out}"
    );
}

#[test]
fn cost_prints_diagnostics_and_filters_to_one_file() {
    let dir = package(
        "cost-files",
        "name = \"app\"\n",
        "fn main_pick(xs: []i64, i: i64) -> i64 {\n    return xs[i];\n}\n",
    );
    fs::write(
        dir.join("lib.ingot"),
        "pub fn lib_pick(xs: []i64, i: i64) -> i64 {\n    return xs[i];\n}\n",
    )
    .unwrap();
    let main = dir.join("main.ingot").to_string_lossy().into_owned();
    let o = ingot(&["cost", "--json", &main], &dir);
    let out = stdout(&o);
    assert!(out.contains("app.main_pick"), "{out}");
    assert!(!out.contains("app.lib_pick"), "{out}");
    assert!(
        out.lines().last().unwrap().contains("\"checks\":1"),
        "{out}"
    );

    let bad = dir.join("bad.ingot");
    fs::write(&bad, "fn bad() -> i64 {\n    return nope;\n}\n").unwrap();
    let bad = bad.to_string_lossy().into_owned();
    let o = ingot(&["cost", "--json", &bad], &dir);
    assert_eq!(o.status.code(), Some(1), "{}{}", stdout(&o), stderr(&o));
    let out = stdout(&o);
    assert!(out.contains("\"type\":\"diagnostic\""), "{out}");
    assert!(
        out.lines().last().unwrap().contains("\"errors\":1"),
        "{out}"
    );
}

#[test]
fn building_for_another_target_is_not_implemented() {
    let dir = package(
        "build-target",
        "name = \"app\"\n",
        "import std.Env;\n\npub fn main(env: &Env) -> u8 {\n    return 0;\n}\n",
    );
    let o = ingot(&["build", "--target", "x86_64-linux-znver4"], &dir);
    assert_eq!(o.status.code(), Some(2), "{}{}", stdout(&o), stderr(&o));
    assert!(
        stderr(&o).contains("not implemented: building for x86_64-linux-znver4 (T-steps)"),
        "{}",
        stderr(&o)
    );
    assert!(
        !dir.join("target").exists(),
        "a refused target wrote an artifact"
    );
}

#[test]
fn an_exported_aggregate_is_not_implemented() {
    let dir = package(
        "export-aggregate",
        "name = \"app\"\n",
        "import std.Env;\n\ncopy struct Fp { x: f64, y: f64 }\n\nexport fn swap_fp(p: Fp) -> f64 {\n    return p.y;\n}\n\npub fn main(env: &Env) -> u8 {\n    return 0;\n}\n",
    );
    let o = ingot(&["build"], &dir);
    assert_eq!(o.status.code(), Some(2), "{}{}", stdout(&o), stderr(&o));
    assert!(
        stderr(&o).contains("not implemented: the C ABI for aggregates (B6)"),
        "{}",
        stderr(&o)
    );
}
