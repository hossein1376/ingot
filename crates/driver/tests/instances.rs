//! Golden tests for phase 11a: instance discovery, depth and mangled names
//! (pipeline §6a). Every `NAME.ingot` in `tests/instances/` is checked, and the
//! sorted `depth<TAB>mangled` listing must equal `NAME.txt`.
//!
//! Run with `INGOT_BLESS=1` to rewrite the expected files.

use ingot_driver::pipeline;
use std::path::{Path, PathBuf};

fn dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/instances")
}

fn cases() -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir()) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "ingot")
            && !path.to_string_lossy().ends_with(".fixed.ingot")
        {
            out.push(path);
        }
    }
    out.sort();
    out
}

#[test]
fn instances_golden() {
    let bless = std::env::var_os("INGOT_BLESS").is_some();
    let mut failures = Vec::new();
    for path in cases() {
        let analysis = match pipeline::analyze_path(&path) {
            Ok(a) => a,
            Err(e) => {
                failures.push(format!("{}: {e}", path.display()));
                continue;
            }
        };
        if !analysis.diagnostics.is_empty() {
            let text = analysis
                .diagnostics
                .iter()
                .map(|d| d.to_human(&analysis.sources))
                .collect::<Vec<_>>()
                .join("\n");
            failures.push(format!("{}: diagnostics:\n{text}", path.display()));
            continue;
        }
        let mut text = String::new();
        for i in &analysis.instances {
            text.push_str(&format!("{}\t{}\n", i.depth, i.mangled));
        }
        let expected = path.with_extension("txt");
        if bless {
            std::fs::write(&expected, &text).expect("write golden");
            continue;
        }
        match std::fs::read_to_string(&expected) {
            Ok(want) if want == text => {}
            Ok(want) => failures.push(format!("{}: expected\n{want}\ngot\n{text}", path.display())),
            Err(e) => failures.push(format!("{}: {e}", expected.display())),
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}
