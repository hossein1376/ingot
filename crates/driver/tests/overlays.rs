//! Loader overlays (tools.md §6): text in place of files, files that are not on disk,
//! manifests, and `std`.

use ingot_diag::diagnostic::Diagnostic;
use ingot_driver::files::Overlays;
use ingot_driver::loader::{self, LoadOptions};
use ingot_driver::pipeline::{self, Options};
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

fn scratch(label: &str) -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "ingot-overlays-{}-{label}-{}",
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

fn options(overlays: Overlays) -> Options {
    Options {
        load: LoadOptions {
            overlays,
            ..LoadOptions::default()
        },
        ..Options::default()
    }
}

fn codes(diagnostics: &[Diagnostic]) -> Vec<(&str, &str)> {
    diagnostics
        .iter()
        .map(|d| (d.code, d.file.as_str()))
        .collect()
}

#[test]
fn an_overlay_replaces_a_modules_text() {
    let dir = package("replace", "name = \"app\"\n", "// nothing\n");
    fs::write(dir.join("other.ingot"), "fn g() -> i64 { return true; }\n").unwrap();
    let base = pipeline::analyze_path_with(&dir, &Options::default()).unwrap();
    assert!(base.diagnostics.iter().any(|d| d.code == "E0401"));

    let mut overlays = Overlays::new();
    overlays.insert(
        &dir.join("other.ingot"),
        "fn g() -> i64 { return nope; }\n".to_string(),
    );
    let analysis = pipeline::analyze_path_with(&dir, &options(overlays)).unwrap();
    assert_eq!(codes(&analysis.diagnostics), [("E0304", "other.ingot")]);
}

#[test]
fn an_overlay_adds_a_module_that_is_not_on_disk() {
    let dir = package("add", "name = \"app\"\n", "// nothing\n");
    fs::write(
        dir.join("main.ingot"),
        "import app.extra;\nfn f() -> i64 { return extra.value(); }\n",
    )
    .unwrap();
    let base = pipeline::analyze_path_with(&dir, &Options::default()).unwrap();
    assert!(
        base.diagnostics.iter().any(|d| d.code == "E0301"),
        "{:?}",
        codes(&base.diagnostics)
    );

    let mut overlays = Overlays::new();
    overlays.insert(
        &dir.join("extra.ingot"),
        "pub fn value() -> i64 { return 1; }\n".to_string(),
    );
    let analysis = pipeline::analyze_path_with(&dir, &options(overlays)).unwrap();
    assert!(
        !analysis.diagnostics.iter().any(|d| d.code == "E0301"),
        "{:?}",
        codes(&analysis.diagnostics)
    );
}

#[test]
fn an_overlay_file_that_is_not_on_disk_is_listed_and_checked() {
    let dir = package("list", "name = \"app\"\n", "// nothing\n");
    let mut overlays = Overlays::new();
    overlays.insert(
        &dir.join("extra.ingot"),
        "fn broken() -> i64 { return true; }\n".to_string(),
    );
    let analysis = pipeline::analyze_path_with(&dir, &options(overlays)).unwrap();
    assert_eq!(codes(&analysis.diagnostics), [("E0401", "extra.ingot")]);
    assert_eq!(analysis.files, 2);
}

#[test]
fn an_overlay_replaces_the_manifest() {
    let dir = package("manifest", "name = 3\n", "// nothing\n");
    let base = pipeline::analyze_path_with(&dir, &Options::default()).unwrap();
    assert!(base.diagnostics.iter().any(|d| d.code == "E1701"));

    let mut overlays = Overlays::new();
    overlays.insert(&dir.join("ingot.toml"), "name = \"app\"\n".to_string());
    let analysis = pipeline::analyze_path_with(&dir, &options(overlays)).unwrap();
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        codes(&analysis.diagnostics)
    );
}

#[test]
fn an_overlay_manifest_appears_for_a_file_that_had_none_above_it() {
    let dir = scratch("new-manifest");
    fs::write(
        dir.join("a.ingot"),
        "import app.b;\nfn f() -> i64 { return b.g(); }\n",
    )
    .unwrap();
    fs::write(dir.join("b.ingot"), "pub fn g() -> i64 { return 1; }\n").unwrap();

    let base = pipeline::analyze_path_with(&dir.join("a.ingot"), &Options::default()).unwrap();
    assert!(
        base.diagnostics.iter().any(|d| d.code == "E0301"),
        "{:?}",
        codes(&base.diagnostics)
    );

    let mut overlays = Overlays::new();
    overlays.insert(&dir.join("ingot.toml"), "name = \"app\"\n".to_string());
    let analysis = pipeline::analyze_path_with(&dir.join("a.ingot"), &options(overlays)).unwrap();
    assert!(
        !analysis.diagnostics.iter().any(|d| d.code == "E0301"),
        "{:?}",
        codes(&analysis.diagnostics)
    );
}

#[test]
fn a_cleared_overlay_restores_the_disk_text() {
    let dir = package("clear", "name = \"app\"\n", "// nothing\n");
    fs::write(dir.join("other.ingot"), "fn g() -> i64 { return true; }\n").unwrap();
    let mut overlays = Overlays::new();
    overlays.insert(&dir.join("other.ingot"), "// replaced\n".to_string());
    let analysis = pipeline::analyze_path_with(&dir, &options(overlays.clone())).unwrap();
    assert!(analysis.diagnostics.is_empty());

    overlays.remove(&dir.join("other.ingot"));
    let analysis = pipeline::analyze_path_with(&dir, &options(overlays)).unwrap();
    assert_eq!(codes(&analysis.diagnostics), [("E0401", "other.ingot")]);
}

#[test]
fn an_overlay_replaces_a_std_file_when_std_is_checked() {
    let root = loader::std_root().expect("the standard library is in the source tree");
    let file = root.join("main.ingot");
    let mut overlays = Overlays::new();
    overlays.insert(&file, "// replaced\n".to_string());
    let package = loader::load_with(
        &root,
        LoadOptions {
            overlays,
            ..LoadOptions::default()
        },
    )
    .unwrap();
    let source = package
        .sources
        .iter()
        .find(|s| s.absolute == file)
        .expect("std/main.ingot is loaded");
    assert_eq!(source.text, "// replaced\n");
}

#[test]
fn an_overlay_on_std_applies_when_a_user_package_is_checked() {
    let dir = package(
        "std-overlay",
        "name = \"app\"\n",
        "import std.Env;\nfn f(env: &Env) { }\n",
    );
    let base = pipeline::analyze_path_with(&dir, &Options::default()).unwrap();
    assert!(
        base.diagnostics.is_empty(),
        "{:?}",
        codes(&base.diagnostics)
    );

    let root = loader::std_root().expect("std");
    let mut overlays = Overlays::new();
    overlays.insert(&root.join("main.ingot"), "// replaced\n".to_string());
    let analysis = pipeline::analyze_path_with(&dir, &options(overlays)).unwrap();
    assert!(
        analysis.diagnostics.iter().any(|d| d.code == "E0301"),
        "{:?}",
        codes(&analysis.diagnostics)
    );
}

#[test]
fn only_the_named_files_diagnostics_are_printed_with_an_overlay() {
    let dir = package("one-file", "name = \"app\"\n", "// nothing\n");
    fs::write(dir.join("other.ingot"), "fn g() -> i64 { return true; }\n").unwrap();
    let mut overlays = Overlays::new();
    overlays.insert(
        &dir.join("main.ingot"),
        "fn f() -> i64 { return nope; }\n".to_string(),
    );
    let analysis =
        pipeline::analyze_path_with(&dir.join("other.ingot"), &options(overlays)).unwrap();
    assert_eq!(codes(&analysis.diagnostics), [("E0401", "other.ingot")]);
}
