//! A chain of constant evaluations, each started while the previous one is suspended,
//! is bounded by the one 1024-deep call limit of pipeline section 4.6, so it stops with
//! E0808 instead of overflowing the compiler's stack: an `@static_assert` in a body
//! that calls the next function, whose body is typed (pulled forward) while the first
//! evaluation waits.
//!
//! A return type whose array length calls the next function is not such a chain: each
//! length is typed, which builds the next signature, before its own evaluation starts,
//! so no evaluation is suspended. It is the ordinary recursion of building signatures
//! and must simply not crash.
//!
//! The tests call the library on the test harness's own small thread stack:
//! `analyze_with` must provide the stack the limit assumes to every caller.

use ingot_driver::pipeline::{self, Analysis, UNCHECKED_CHECK_REMOVAL};
use std::path::PathBuf;

/// `g0`'s `@static_assert` calls `g1`, whose `@static_assert` calls `g2`, ...
fn assert_chain(n: usize) -> String {
    let mut s = String::new();
    for i in 0..n {
        s.push_str(&format!("fn g{i}() -> i64 {{\n"));
        if i + 1 < n {
            s.push_str(&format!(
                "    @static_assert(g{}() == 1, \"chain\");\n",
                i + 1
            ));
        }
        s.push_str("    return 1;\n}\n");
    }
    s
}

/// `g0`'s return type has the length `g1().len`, whose return type has the length
/// `g2().len`, ... The last returns `[1]u8`, so every length is 1.
fn signature_chain(n: usize) -> String {
    let mut s = String::new();
    for i in 0..n {
        let t = if i + 1 < n {
            format!("[g{}().len]u8", i + 1)
        } else {
            "[1]u8".to_string()
        };
        s.push_str(&format!(
            "fn g{i}() -> {t} {{\n    return @zero({t});\n}}\n"
        ));
    }
    s.push_str("fn f() {\n    @static_assert(g0().len == 1, \"signature chain\");\n}\n");
    s
}

fn analyze(name: &str, source: String) -> Analysis {
    let dir = temp_dir(name);
    std::fs::create_dir_all(&dir).expect("temp dir");
    std::fs::write(dir.join("ingot.toml"), "name = \"app\"\n").expect("manifest");
    std::fs::write(dir.join("main.ingot"), source).expect("source");
    let analysis = pipeline::analyze_path(&dir).expect("analysis");
    let _ = std::fs::remove_dir_all(&dir);
    analysis
}

fn temp_dir(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("ingot-chain-{}-{name}", std::process::id()))
}

fn assert_only_ownership(analysis: &Analysis) {
    assert!(
        analysis.diagnostics.is_empty(),
        "{}",
        analysis
            .diagnostics
            .iter()
            .map(|d| format!("{}: {}", d.code, d.message))
            .collect::<Vec<_>>()
            .join("\n")
    );
    assert!(
        analysis
            .not_implemented
            .iter()
            .all(|m| m == UNCHECKED_CHECK_REMOVAL),
        "{:?}",
        analysis.not_implemented
    );
}

fn assert_one_depth_limit(analysis: &Analysis) {
    let limits: Vec<_> = analysis
        .diagnostics
        .iter()
        .filter(|d| d.code == "E0808")
        .collect();
    assert_eq!(limits.len(), 1, "{:?}", analysis.diagnostics);
    assert!(limits[0].message.contains("1024"), "{}", limits[0].message);
}

#[test]
fn a_chain_of_1000_asserts_is_under_the_limit() {
    assert_only_ownership(&analyze("assert-1000", assert_chain(1000)));
}

#[test]
fn a_chain_of_1100_asserts_reports_e0808_once() {
    assert_one_depth_limit(&analyze("assert-1100", assert_chain(1100)));
}

#[test]
fn a_chain_of_3000_signatures_is_accepted() {
    assert_only_ownership(&analyze("signature-3000", signature_chain(3000)));
}
