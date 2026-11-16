//! Golden tests for pipeline phase 8 (`ingot-cfg`): every `NAME.ingot` in
//! `tests/cfg/` is checked as a package, every body is lowered, and the dump of the
//! CFGs must equal `NAME.cfg` exactly.
//!
//! Run with `INGOT_BLESS=1` to rewrite the expected files. A missing `.cfg` fails
//! without it. The runner also requires that the package has no diagnostics, that
//! lowering recorded nothing as unimplemented, and that every CFG passes
//! [`ingot_cfg::validate_all`].

use ingot_driver::pipeline;
use std::path::{Path, PathBuf};

fn cfg_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/cfg")
}

fn cases() -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(cfg_dir()) else {
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
fn cfg_golden() {
    let bless = std::env::var_os("INGOT_BLESS").is_some();
    let cases = cases();
    assert!(
        !cases.is_empty(),
        "no .ingot files in {}",
        cfg_dir().display()
    );
    let mut failures = Vec::new();
    for path in cases {
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
        let lowering: Vec<&String> = analysis
            .not_implemented
            .iter()
            .filter(|m| m.starts_with("lowering:"))
            .collect();
        if !lowering.is_empty() {
            failures.push(format!("{}: not lowered: {lowering:?}", path.display()));
            continue;
        }
        let problems = ingot_cfg::validate::validate_all(&analysis.cfgs);
        let after = ingot_cfg::validate::validate_all(&analysis.removed_cfgs);
        if !after.is_empty() {
            failures.push(format!(
                "{}: phase 10 CFG validator:\n  {}",
                path.display(),
                after.join("\n  ")
            ));
            continue;
        }
        let actual = ingot_cfg::dump::dump(&analysis.cfgs, &analysis.sources);
        if !problems.is_empty() {
            failures.push(format!(
                "{}: validator:\n  {}\n--- dump\n{actual}",
                path.display(),
                problems.join("\n  ")
            ));
            continue;
        }
        let expected_path = path.with_extension("cfg");
        if bless {
            if let Err(e) = std::fs::write(&expected_path, &actual) {
                failures.push(format!("{}: could not bless: {e}", expected_path.display()));
            }
            continue;
        }
        let expected = match std::fs::read_to_string(&expected_path) {
            Ok(t) => t,
            Err(e) => {
                failures.push(format!(
                    "{}: could not read the expected dump: {e} (run with INGOT_BLESS=1)",
                    expected_path.display()
                ));
                continue;
            }
        };
        if actual != expected {
            failures.push(format!(
                "{}: dump differs from {}\n--- expected\n{expected}--- actual\n{actual}",
                path.display(),
                expected_path.display()
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}
