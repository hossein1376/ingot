use ingotc::check::{analyze, analyze_file};
use ingotc::interp;
use ingotc::source::Source;
use ingotc::types::Type;
use std::path::PathBuf;

fn std_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("std")
}

#[test]
fn std_package_checks_clean() {
    let analysis = analyze_file(&std_root()).unwrap();
    for d in &analysis.diagnostics {
        eprintln!("{}", d.to_human(&analysis.sources));
    }
    assert!(analysis.diagnostics.is_empty());
}

#[test]
fn std_module_and_item_imports_resolve() {
    let source = r#"
import std.mem;
import std.mem.Arena;
import std.mem.copy;

fn copy_all(dst: []mut u8, src: []u8) {
    _ = copy(dst, src);
}

fn consume(arena: Arena) {
    arena.deinit();
}

fn reserve(mem: mem.Arena) {
    mem.deinit();
}
"#;
    let analysis = analyze(Source::new("test.ingot", source));
    for d in &analysis.diagnostics {
        eprintln!("{}", d.to_human(&analysis.sources));
    }
    assert!(analysis.diagnostics.is_empty());
}

#[test]
fn std_env_is_a_declared_type() {
    let source = r#"
import std.Env;

pub fn main(env: &Env) -> u8 {
    return 0;
}
"#;
    let analysis = analyze(Source::new("test.ingot", source));
    assert!(analysis.diagnostics.is_empty());
    let env = analysis.program.defs.structs.get("std.Env").unwrap();
    let heap = env
        .fields
        .iter()
        .find(|(name, _, _)| name == "heap")
        .unwrap();
    assert!(matches!(&heap.1, Type::Struct(name, _) if name == "std.mem.Heap"));
}

#[test]
fn std_sys_externs_are_importable() {
    let source = r#"
import std.sys.macos;

fn close_stdout() {
    _ = macos.close(1);
}
"#;
    let analysis = analyze(Source::new("test.ingot", source));
    for d in &analysis.diagnostics {
        eprintln!("{}", d.to_human(&analysis.sources));
    }
    assert!(analysis.diagnostics.is_empty());
}

#[test]
fn std_slice_text_and_format_helpers_run() {
    let source = r#"
import std.Env;
import std.fmt;
import std.mem;
import std.text;

pub fn main(env: &Env) -> u8 {
    @assert(env.args.len == 0);
    var a: [5]u8 = @zero;
    var b: [5]u8 = @zero;
    mem.fill(&mut b[0..5], 65);
    let n = mem.copy(&mut a[0..5], &b[0..5]);
    @assert(n == 5);
    @assert(mem.eq(&a[0..5], &b[0..5]));

    @assert(text.valid("hello"));
    @assert(text.valid("hé"));
    @assert((text.count("hello") catch _ => -1) == 5);
    let trimmed = text.trim("  hi  ");
    @assert(trimmed.len == 2);
    @assert(trimmed[0] == 'h');
    @assert(trimmed[1] == 'i');

    let parsed = text.parse_i64("-9223372036854775808") catch _ => 0;
    var out: [21]u8 = @zero;
    let written = fmt.format_i64(&mut out[0..21], parsed);
    @assert(written == 20);
    @assert(out[0] == '-');
    @assert(out[1] == '9');
    return 0;
}
"#;
    let analysis = analyze(Source::new("test.ingot", source));
    for d in &analysis.diagnostics {
        eprintln!("{}", d.to_human(&analysis.sources));
    }
    assert!(analysis.diagnostics.is_empty());
    let outcome = interp::run(&analysis).unwrap();
    assert_eq!(outcome.status, 0);
    assert!(outcome.trap.is_none(), "{:?}", outcome.trap);
}
