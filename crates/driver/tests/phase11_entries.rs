//! What phase 7 defers to instantiation must be a `not implemented` entry, never a
//! silent acceptance: `@static_assert` over a generic parameter, constant evaluation
//! over one, a kernel type that is a parameter, and polymorphic recursion (E0809).

use ingot_driver::pipeline;

fn analyze(name: &str, source: &str) -> pipeline::Analysis {
    let dir = std::env::temp_dir().join(format!(
        "ingot-phase11-{}-{}-{name}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).expect("temp dir");
    std::fs::write(dir.join("ingot.toml"), "name = \"app\"\n").expect("manifest");
    std::fs::write(dir.join("main.ingot"), source).expect("source");
    let analysis = pipeline::analyze_path(&dir).expect("analysis");
    let _ = std::fs::remove_dir_all(&dir);
    analysis
}

fn has_phase11(analysis: &pipeline::Analysis) -> bool {
    analysis
        .not_implemented
        .iter()
        .any(|m| m == ingot_sema::PER_INSTANCE_CHECKS)
}

#[test]
fn a_static_assert_over_a_generic_parameter_is_a_phase11_entry() {
    let analysis = analyze(
        "static-assert",
        "fn f[T](x: T) -> i64 {\n    @static_assert(@sizeof(T) > 0, \"size\");\n    return 1;\n}\n",
    );
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    assert!(has_phase11(&analysis), "{:?}", analysis.not_implemented);
}

#[test]
fn a_constant_over_a_generic_parameter_is_a_phase11_entry() {
    let analysis = analyze(
        "generic-constant",
        "fn f[T](x: T) -> i64 {\n    let a: [@sizeof(T)]u8 = [@sizeof(T)]u8.{};\n    @forget(a);\n    @forget(x);\n    return 0;\n}\n",
    );
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    assert!(has_phase11(&analysis), "{:?}", analysis.not_implemented);
}

#[test]
fn polymorphic_recursion_is_a_phase11_entry() {
    let analysis = analyze(
        "polymorphic",
        "fn grow[T](x: T) -> i64 {\n    return grow((x, 1));\n}\n",
    );
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    assert!(has_phase11(&analysis), "{:?}", analysis.not_implemented);
}

#[test]
fn a_closure_argument_is_growing() {
    let analysis = analyze(
        "closure-argument",
        "fn nest[F: Fn() -> i64](f: F, n: i64) -> i64 {\n    if n <= 0 {\n        return f();\n    }\n    return nest(fn [&f]() -> i64 { return f() +% 1; }, n - 1);\n}\n",
    );
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    assert!(has_phase11(&analysis), "{:?}", analysis.not_implemented);
}

#[test]
fn forwarding_a_generic_argument_is_not_growing() {
    let analysis = analyze(
        "forwarding",
        "fn ping[T: Copy + Discard](x: T, n: i64) -> i64 {\n    if n <= 0 {\n        return 0;\n    }\n    return pong(x, n - 1);\n}\n\nfn pong[U: Copy + Discard](y: U, n: i64) -> i64 {\n    return ping(y, n);\n}\n",
    );
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    assert!(!has_phase11(&analysis), "{:?}", analysis.not_implemented);
}

#[test]
fn monomorphic_generic_recursion_is_not_a_phase11_entry() {
    let analysis = analyze(
        "monomorphic",
        "fn f[T](x: T, go: bool) -> i64 {\n    if go {\n        return f(x, false);\n    }\n    @forget(x);\n    return 1;\n}\n",
    );
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    assert!(!has_phase11(&analysis), "{:?}", analysis.not_implemented);
}
