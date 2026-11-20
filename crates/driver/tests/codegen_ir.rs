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
