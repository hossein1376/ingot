//! The IR golden and its determinism (B1). `small.ll` is the pinned text of
//! `tests/ir/small.ingot`; run with `INGOT_BLESS=1` to rewrite it.

use ingot_codegen::Program;
use ingot_driver::pipeline;
use std::path::{Path, PathBuf};

fn dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/ir")
}

#[test]
fn small_program_has_pinned_ir() {
    let path = dir().join("small.ingot");
    let options = pipeline::Options {
        roots: ingot_cfg::instances::Roots::Build,
        ..pipeline::Options::default()
    };
    let mut analysis = pipeline::analyze_path_with(&path, &options).expect("analyze");
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
    let front = analysis.front_end.as_mut().expect("front end");
    let program = Program {
        sources: &analysis.sources,
        sema: &front.resolved.sema,
        decls: &mut front.declarations,
        cfgs: &analysis.removed_cfgs,
        extra_cfgs: &analysis.extra_cfgs,
        instances: &analysis.instances,
        checked: front.checked,
        checked_packages: &front.checked_packages,
        target: &analysis.target,
        test_build: false,
    };
    let ir = ingot_codegen::emit(program).expect("emit IR");
    let expected = dir().join("small.ll");
    if std::env::var_os("INGOT_BLESS").is_some() {
        std::fs::write(&expected, &ir).expect("write golden");
        return;
    }
    let want = std::fs::read_to_string(&expected)
        .unwrap_or_else(|e| panic!("{}: {e}", expected.display()));
    assert_eq!(want, ir, "{}", expected.display());
}

#[test]
fn the_same_program_gives_byte_identical_ir() {
    let path = dir().join("small.ingot");
    let options = pipeline::Options {
        roots: ingot_cfg::instances::Roots::Build,
        ..pipeline::Options::default()
    };
    let mut first = pipeline::analyze_path_with(&path, &options).expect("analyze");
    let mut second = pipeline::analyze_path_with(&path, &options).expect("analyze");
    let a = {
        let front = first.front_end.as_mut().expect("front end");
        ingot_codegen::emit(Program {
            sources: &first.sources,
            sema: &front.resolved.sema,
            decls: &mut front.declarations,
            cfgs: &first.removed_cfgs,
            extra_cfgs: &first.extra_cfgs,
            instances: &first.instances,
            checked: front.checked,
            checked_packages: &front.checked_packages,
            target: &first.target,
            test_build: false,
        })
        .expect("emit IR")
    };
    let b = {
        let front = second.front_end.as_mut().expect("front end");
        ingot_codegen::emit(Program {
            sources: &second.sources,
            sema: &front.resolved.sema,
            decls: &mut front.declarations,
            cfgs: &second.removed_cfgs,
            extra_cfgs: &second.extra_cfgs,
            instances: &second.instances,
            checked: front.checked,
            checked_packages: &front.checked_packages,
            target: &second.target,
            test_build: false,
        })
        .expect("emit IR")
    };
    assert_eq!(a, b);
}

/// The IR of one of `tests/ir/*.ingot`.
fn ir_of(name: &str) -> String {
    let path = dir().join(name);
    let options = pipeline::Options {
        roots: ingot_cfg::instances::Roots::Build,
        ..pipeline::Options::default()
    };
    let mut analysis = pipeline::analyze_path_with(&path, &options).expect("analyze");
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    let front = analysis.front_end.as_mut().expect("front end");
    ingot_codegen::emit(Program {
        sources: &analysis.sources,
        sema: &front.resolved.sema,
        decls: &mut front.declarations,
        cfgs: &analysis.removed_cfgs,
        extra_cfgs: &analysis.extra_cfgs,
        instances: &analysis.instances,
        checked: front.checked,
        checked_packages: &front.checked_packages,
        target: &analysis.target,
        test_build: false,
    })
    .expect("emit IR")
}

/// The text of the function whose symbol ends in `.name`.
fn function_of<'a>(ir: &'a str, name: &str) -> &'a str {
    let start = ir
        .lines()
        .position(|l| l.starts_with("define") && l.contains(&format!(".{name}\"")))
        .unwrap_or_else(|| panic!("no function `{name}` in the IR"));
    let from: usize = ir.lines().take(start).map(|l| l.len() + 1).sum();
    let len = ir[from..].find("\n}\n").expect("an unterminated function");
    &ir[from..from + len]
}

/// Pipeline section 7: an atomic builtin is one instruction with the given order, and
/// an order that is not known becomes a branch over its values, one instruction each.
#[test]
fn a_known_atomic_order_is_one_instruction_and_any_other_is_a_switch() {
    let ir = ir_of("atomic.ingot");

    let known = function_of(&ir, "load_relaxed");
    assert_eq!(known.matches("load atomic i64").count(), 1, "{known}");
    assert!(known.contains(" monotonic, align 8"), "{known}");
    assert!(
        !known.contains("switch") && !known.contains("phi"),
        "{known}"
    );

    let unknown = function_of(&ir, "add_any");
    assert_eq!(unknown.matches("switch").count(), 1, "{unknown}");
    assert_eq!(unknown.matches("atomicrmw add").count(), 5, "{unknown}");
    for order in ["monotonic", "acquire", "release", "acq_rel", "seq_cst"] {
        let arm = format!(", i64 1 {order}, align 8");
        assert_eq!(unknown.matches(&arm).count(), 1, "{order}: {unknown}");
    }

    // Both orders unknown: a switch on the success order, and in each arm one on the
    // failure order, so every pair gets its own instruction and none is adjusted.
    let both = function_of(&ir, "exchange_any");
    assert_eq!(both.matches("switch").count(), 1 + 5 + 1, "{both}");
    assert_eq!(both.matches("cmpxchg ptr").count(), 5 * 3, "{both}");
    assert!(both.contains(", i64 1, i64 2 monotonic seq_cst, align 8"));
}

/// A static that holds a `Cell` is written through `&`, so its global is not `constant`;
/// any other static keeps being one.
#[test]
fn a_static_with_interior_mutability_is_a_writable_global() {
    let ir = ir_of("atomic.ingot");
    let globals: Vec<&str> = ir.lines().filter(|l| l.starts_with("@static.")).collect();
    assert_eq!(globals.len(), 2, "{globals:?}");
    let writable: Vec<&&str> = globals
        .iter()
        .filter(|l| l.contains(" private global "))
        .collect();
    let fixed: Vec<&&str> = globals
        .iter()
        .filter(|l| l.contains(" private constant "))
        .collect();
    assert_eq!((writable.len(), fixed.len()), (1, 1), "{globals:?}");
    assert!(writable[0].contains("align 8"), "{writable:?}");
    assert!(!writable[0].contains("unnamed_addr"), "{writable:?}");
}
