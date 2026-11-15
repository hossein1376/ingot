//! The standard library package at `std/`: it must resolve without a diagnostic, its
//! `Env` must have the fields of spec section 33.1, and its capability types must
//! have no public field (section 4.2).

use ingot_diag::source::Source;
use ingot_driver::pipeline::{self, UNCHECKED};
use ingot_syntax::ast::{Item, StructDecl};
use ingot_syntax::lexer::lex;
use ingot_syntax::parser::parse;
use std::path::{Path, PathBuf};

fn std_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../std")
}

fn module(name: &str) -> (Source, ingot_syntax::ast::Module) {
    let path = std_dir().join(name);
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let source = Source::new(name, text);
    let (tokens, lex_diags) = lex(&source);
    let (ast, parse_diags) = parse(&source, tokens);
    assert!(
        lex_diags.is_empty() && parse_diags.is_empty(),
        "{name} must parse"
    );
    (source, ast)
}

fn structs(ast: &ingot_syntax::ast::Module) -> Vec<&StructDecl> {
    ast.items
        .iter()
        .filter_map(|i| match i {
            Item::Struct(s) => Some(s),
            _ => None,
        })
        .collect()
}

#[test]
fn std_resolves_without_a_diagnostic() {
    let analysis = pipeline::analyze_path(&std_dir()).expect("std loads");
    for d in &analysis.diagnostics {
        eprintln!("{}", d.to_human(&analysis.sources));
    }
    assert!(analysis.diagnostics.is_empty());
    // std's bodies are type-checked too (`ingot check std`); ownership is the next
    // phase. Its constants, including the executors' struct values, are evaluated
    // (S5b).
    assert_eq!(
        analysis.not_implemented,
        vec![ingot_cfg::UNCHECKED_CHECK_REMOVAL.to_string()]
    );
    assert!(!analysis.not_implemented.contains(&UNCHECKED.to_string()));
    assert_eq!(analysis.files, 14);
}

#[test]
fn std_resolves_for_every_target() {
    for target in ingot_sema::target::TARGETS {
        let options = pipeline::Options {
            target: target.to_string(),
            ..pipeline::Options::default()
        };
        let analysis = pipeline::analyze_path_with(&std_dir(), &options).expect("std loads");
        assert!(analysis.diagnostics.is_empty(), "{target}");
    }
}

#[test]
fn env_has_the_fields_of_section_33_1() {
    let (source, ast) = module("main.ingot");
    let env = structs(&ast)
        .into_iter()
        .find(|s| s.name == "Env")
        .expect("std.Env");
    assert!(env.public && env.generics.is_empty());
    let fields: Vec<(&str, &str, bool)> = env
        .fields
        .iter()
        .map(|f| (f.name.as_str(), source.text_of(f.ty.span()), f.public))
        .collect();
    assert_eq!(
        fields,
        vec![
            ("args", "[][]u8", true),
            ("vars", "&Vars", true),
            ("heap", "Heap", true),
            ("vm", "Vm", true),
            ("fs", "&Fs", true),
            ("net", "&Net", true),
            ("clock", "&Clock", true),
            ("rand", "&Rand", true),
            ("stdin", "&Reader", true),
            ("stdout", "&Writer", true),
            ("stderr", "&Writer", true),
            ("gpu", "?gpu.Device", true),
        ]
    );
}

#[test]
fn capability_types_have_no_public_field() {
    for (file, names) in [
        ("main.ingot", &["Vars"][..]),
        ("mem.ingot", &["Heap", "Vm"]),
        ("fs.ingot", &["Fs"]),
        ("net.ingot", &["Net"]),
        ("time.ingot", &["Clock"]),
        ("rand.ingot", &["Rand"]),
        ("io.ingot", &["Reader", "Writer"]),
        ("gpu.ingot", &["Device"]),
    ] {
        let (_, ast) = module(file);
        let found = structs(&ast);
        for name in names {
            let s = found
                .iter()
                .find(|s| s.name == *name)
                .unwrap_or_else(|| panic!("{file} declares {name}"));
            assert!(s.public, "{name} is public");
            assert!(
                s.fields.iter().all(|f| !f.public),
                "{name} must not have a public field"
            );
        }
    }
}

#[test]
fn std_declares_the_types_the_conformance_tests_import() {
    let (_, task) = module("task.ingot");
    let task = structs(&task);
    assert!(
        task.iter()
            .any(|s| s.name == "Scope" && s.generics.is_empty())
    );
    let t = task.iter().find(|s| s.name == "Task").expect("Task");
    assert_eq!(t.generics.len(), 1);

    let (_, sync) = module("sync.ingot");
    let sync = structs(&sync);
    for name in ["Atomic", "Mutex"] {
        let s = sync.iter().find(|s| s.name == name).expect(name);
        assert_eq!(s.generics.len(), 1, "{name}[T]");
        // Section 13.6: the contents of a type with interior mutability are `Owned`.
        assert_eq!(s.generics[0].bounds[0].name(), "Owned");
    }

    // The executors of section 27.2, `exec.par` among them.
    let (_, exec) = module("exec.ingot");
    for name in ["seq", "simd", "par"] {
        assert!(
            exec.items
                .iter()
                .any(|i| matches!(i, Item::Const(c) if c.name == name && c.public)),
            "exec.{name} is a public const"
        );
    }
}

#[test]
fn std_has_no_functions_yet() {
    // Methods and bodies come with type checking.
    for file in [
        "main.ingot",
        "mem.ingot",
        "fs.ingot",
        "net.ingot",
        "time.ingot",
        "rand.ingot",
        "io.ingot",
        "gpu.ingot",
        "task.ingot",
        "sync.ingot",
        "exec.ingot",
    ] {
        let (_, ast) = module(file);
        assert!(
            !ast.items.iter().any(|i| match i {
                Item::Fn(_) => true,
                // `unsafe impl Send/Sync` states a property and has no member.
                Item::Impl(imp) => !imp.members.is_empty(),
                _ => false,
            }),
            "{file}"
        );
    }
}
