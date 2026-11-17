//! Phase 11a's per-instance checks and `instantiation` labels: a static assertion, a
//! length, an alignment, a lane index, a shuffle index, a trapped operation and a
//! kernel type are checked for each instance, with the shortest call chain.

use ingot_driver::pipeline;

fn analyze(name: &str, source: &str) -> pipeline::Analysis {
    analyze_roots(name, source, ingot_cfg::instances::Roots::Check)
}

fn analyze_roots(
    name: &str,
    source: &str,
    roots: ingot_cfg::instances::Roots,
) -> pipeline::Analysis {
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
    let options = pipeline::Options {
        roots,
        ..pipeline::Options::default()
    };
    let analysis = pipeline::analyze_path_with(&dir, &options).expect("analysis");
    let _ = std::fs::remove_dir_all(&dir);
    analysis
}

fn labels(d: &ingot_diag::diagnostic::Diagnostic) -> Vec<&ingot_diag::diagnostic::Related> {
    d.related
        .iter()
        .filter(|r| r.role == "instantiation")
        .collect()
}

#[test]
fn a_static_assert_over_a_generic_parameter_is_checked_per_instance() {
    let source = "fn f[const n: i64]() -> i64 {\n    @static_assert(n > 0, \"positive\");\n    return n;\n}\n\nfn run() -> i64 {\n    return f.[0]();\n}\n";
    let analysis = analyze("static-assert", source);
    let d = analysis
        .diagnostics
        .iter()
        .find(|d| d.code == "E0810")
        .unwrap_or_else(|| panic!("{:?}", analysis.diagnostics));
    assert!(d.message.contains("positive"));
    let chain = labels(d);
    assert_eq!(chain.len(), 1, "{:?}", d.related);
    assert_eq!(
        chain[0].span.start,
        source.find("return f.[0]()").expect("the call"),
        "{:?}",
        d.related
    );
    assert!(chain[0].span.start < chain[0].span.end);
    assert!(
        analysis.not_implemented.is_empty(),
        "{:?}",
        analysis.not_implemented
    );
}

#[test]
fn a_length_over_a_generic_parameter_is_checked_per_instance() {
    let analysis = analyze(
        "generic-constant",
        "fn f[const n: i64]() -> i64 {\n    let a: [n]u8 = [n]u8.{};\n    @forget(a);\n    return 0;\n}\n\nfn run() -> i64 {\n    return f.[-1]();\n}\n",
    );
    let d = analysis
        .diagnostics
        .iter()
        .find(|d| d.code == "E0521")
        .unwrap_or_else(|| panic!("{:?}", analysis.diagnostics));
    assert_eq!(labels(d).len(), 1, "{:?}", d.related);
    assert!(
        analysis.not_implemented.is_empty(),
        "{:?}",
        analysis.not_implemented
    );
}

#[test]
fn a_generic_operation_that_traps_is_e0806_per_instance() {
    let analysis = analyze(
        "generic-trap",
        "fn f[const n: i64]() -> i64 {\n    return n * 2;\n}\n\nfn run() -> i64 {\n    return f.[9223372036854775807]();\n}\n",
    );
    let d = analysis
        .diagnostics
        .iter()
        .find(|d| d.code == "E0806")
        .unwrap_or_else(|| panic!("{:?}", analysis.diagnostics));
    assert!(d.message.contains("T03"));
    assert_eq!(labels(d).len(), 1, "{:?}", d.related);
}

#[test]
fn a_chain_is_innermost_first_and_has_no_empty_span() {
    let analysis = analyze(
        "chain",
        "fn h[const n: i64]() -> i64 {\n    @static_assert(n > 0, \"positive\");\n    return n;\n}\n\nfn g[const n: i64]() -> i64 {\n    return h.[n]();\n}\n\nfn run() -> i64 {\n    return g.[0]();\n}\n",
    );
    let d = analysis
        .diagnostics
        .iter()
        .find(|d| d.code == "E0810")
        .unwrap_or_else(|| panic!("{:?}", analysis.diagnostics));
    let chain = labels(d);
    assert_eq!(chain.len(), 2, "{:?}", d.related);
    assert!(
        chain[0].span.start < chain[1].span.start,
        "innermost first: {:?}",
        d.related
    );
    assert!(
        chain.iter().all(|r| r.span.start < r.span.end),
        "{:?}",
        d.related
    );
}

#[test]
fn a_closure_in_a_generic_body_is_an_instance() {
    let analysis = analyze(
        "closure-instance",
        "fn make[T: Copy + Discard](x: T) -> i64 {\n    let f = fn [x]() -> i64 { _ = x; return 1; };\n    return f();\n}\n\nfn run() -> i64 {\n    return make(7);\n}\n",
    );
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    assert!(
        analysis
            .instances
            .iter()
            .any(|i| i.mangled.contains("closure") && i.depth == 2),
        "{:?}",
        analysis
            .instances
            .iter()
            .map(|i| (&i.mangled, i.depth))
            .collect::<Vec<_>>()
    );
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
    assert!(
        analysis.not_implemented.is_empty(),
        "{:?}",
        analysis.not_implemented
    );
}

#[test]
fn a_method_table_chain_points_at_real_code() {
    let source = "interface Named {\n    fn name(self: &Self) -> i64;\n}\n\nstruct Box[T] { v: T }\n\nimpl[T: Copy + Discard] Named for Box[T] {\n    fn name(self: &Box[T]) -> i64 {\n        return grow(self.v);\n    }\n}\n\nfn grow[T: Copy + Discard](x: T) -> i64 {\n    return grow((x, 1));\n}\n\nfn by_dyn(x: &dyn Named) -> i64 {\n    return x.name();\n}\n\nfn run() -> i64 {\n    let b = Box[i64].{ v: 1 };\n    return by_dyn(&b);\n}\n";
    let analysis = analyze("table-chain", source);
    let e0809 = analysis
        .diagnostics
        .iter()
        .find(|d| d.code == "E0809")
        .unwrap_or_else(|| panic!("{:?}", analysis.diagnostics));
    assert!(!e0809.related.is_empty());
    assert!(
        e0809.related.iter().all(|r| r.span.start < r.span.end),
        "{:?}",
        e0809.related
    );
    let coercion = source.find("&b").expect("the coercion");
    assert!(
        e0809.related.iter().any(|r| r.span.start == coercion),
        "{:?}",
        e0809.related
    );
}

#[test]
fn build_test_and_check_have_their_own_roots() {
    let source = "import std.Env;\n\nfn helper[T](x: T) -> i64 {\n    @forget(x);\n    return 1;\n}\n\npub fn main(env: &Env) -> void {\n    _ = env;\n    _ = helper(1);\n}\n\ntest \"t\" {\n    _ = helper(2);\n}\n\nfn unused() -> i64 {\n    return 3;\n}\n";
    for (roots, has_main, has_test) in [
        (ingot_cfg::instances::Roots::Build, true, false),
        (ingot_cfg::instances::Roots::Test, false, true),
        (ingot_cfg::instances::Roots::Check, true, true),
    ] {
        let analysis = analyze_roots("roots", source, roots);
        assert!(
            analysis.diagnostics.is_empty(),
            "{:?}: {:?}",
            roots,
            analysis.diagnostics
        );
        let names: Vec<&str> = analysis
            .instances
            .iter()
            .map(|i| i.mangled.as_str())
            .collect();
        assert_eq!(
            names.iter().any(|n| n.ends_with("main")),
            has_main,
            "{roots:?}: {names:?}"
        );
        assert_eq!(
            names.iter().any(|n| n.ends_with(".t")),
            has_test,
            "{roots:?}: {names:?}"
        );
        assert!(
            names
                .iter()
                .any(|n| n.contains("helper") && n.contains("i64")),
            "{roots:?}: {names:?}"
        );
        if roots != ingot_cfg::instances::Roots::Check {
            assert!(
                !names.iter().any(|n| n.contains("unused")),
                "{roots:?}: {names:?}"
            );
        }
    }
}

#[test]
fn monomorphic_generic_recursion_is_not_e0809() {
    let analysis = analyze(
        "monomorphic",
        "fn f[T](x: T, go: bool) -> i64 {\n    if go {\n        return f(x, false);\n    }\n    @forget(x);\n    return 1;\n}\n",
    );
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    assert!(
        analysis.not_implemented.is_empty(),
        "{:?}",
        analysis.not_implemented
    );
}
