//! A build is deterministic (05 section 33, pipeline section 2, COMPILER.md
//! "Reproducible output"): the same source, compiler, target and manifest give the
//! same executable, byte for byte, wherever the package sits.
//!
//! Each of the three milestone programs is built under both pipelines three times:
//! twice from a copy in one temporary directory and once from a copy in another, at
//! a different absolute path and depth. The artifact has the same file name every
//! time: `ld` signs an arm64 executable ad hoc with the output file's name as its
//! identifier, so the name is part of the output. A missing toolchain fails the
//! test, as it does the conformance ratchet; nothing here skips.

use std::path::{Path, PathBuf};
use std::process::Command;

const ARTIFACT: &str = "prog";

/// A fresh temporary directory, and a working directory `nested` below it.
fn scratch(label: &str, nested: &[&str]) -> (PathBuf, PathBuf) {
    let base =
        std::env::temp_dir().join(format!("ingot-reproducible-{}-{label}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let mut work = base.clone();
    for part in nested {
        work.push(part);
    }
    std::fs::create_dir_all(&work).unwrap();
    (base, work)
}

/// A package holding `conformance/programs/<name>.ingot` as its root module.
fn package(root: &Path, name: &str) -> PathBuf {
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../conformance/programs")
        .join(format!("{name}.ingot"));
    let dir = root.join("package");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::copy(&source, dir.join("main.ingot"))
        .unwrap_or_else(|e| panic!("could not copy {}: {e}", source.display()));
    std::fs::write(dir.join("ingot.toml"), "name = \"prog\"\n").unwrap();
    dir
}

/// Build the package into `<out_dir>/prog` and return the executable's bytes.
fn build(package: &Path, out_dir: &Path, fast: bool) -> Vec<u8> {
    std::fs::create_dir_all(out_dir).unwrap();
    let artifact = out_dir.join(ARTIFACT);
    let mut command = Command::new(env!("CARGO_BIN_EXE_ingot"));
    command.arg("build");
    if fast {
        command.arg("--fast");
    }
    command.arg("-o").arg(&artifact).arg(".");
    command.current_dir(package);
    let output = command.output().expect("could not run `ingot build`");
    assert!(
        output.status.success(),
        "`ingot build{}` failed in {}: {}{}",
        if fast { " --fast" } else { "" },
        package.display(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    std::fs::read(&artifact).unwrap_or_else(|e| panic!("no artifact {}: {e}", artifact.display()))
}

fn first_difference(a: &[u8], b: &[u8]) -> String {
    match a.iter().zip(b).position(|(x, y)| x != y) {
        Some(at) => format!("first difference at byte {at}"),
        None => format!("lengths {} and {}", a.len(), b.len()),
    }
}

fn check(program: &str) {
    let (base_a, a) = scratch(&format!("{program}-a"), &["a"]);
    let (base_b, b) = scratch(
        &format!("{program}-b"),
        &["deeper", "and", "elsewhere", "b"],
    );
    let in_a = package(&a, program);
    let in_b = package(&b, program);
    for fast in [false, true] {
        let pipeline = if fast { "--fast" } else { "optimized" };
        let first = build(&in_a, &a.join("out-1"), fast);
        let second = build(&in_a, &a.join("out-2"), fast);
        let third = build(&in_b, &b.join("out"), fast);
        assert!(
            first == second,
            "{program} ({pipeline}): two builds from one directory differ: {}",
            first_difference(&first, &second)
        );
        assert!(
            first == third,
            "{program} ({pipeline}): a build from {} differs from one from {}: {}",
            in_b.display(),
            in_a.display(),
            first_difference(&first, &third)
        );
    }
    let _ = std::fs::remove_dir_all(&base_a);
    let _ = std::fs::remove_dir_all(&base_b);
}

#[test]
fn sha256_is_byte_identical() {
    check("sha256");
}

#[test]
fn expr_eval_is_byte_identical() {
    check("expr_eval");
}

#[test]
fn generic_sort_heap_is_byte_identical() {
    check("generic_sort_heap");
}
