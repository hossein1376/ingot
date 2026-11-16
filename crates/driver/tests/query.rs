//! `ingot query def|type|symbols` (tools.md §5).

use ingot_diag::json::{self, Json};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

fn scratch(label: &str) -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "ingot-query-{}-{label}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn package(label: &str, files: &[(&str, &str)]) -> PathBuf {
    let dir = scratch(label);
    for (name, text) in files {
        let path = dir.join(name);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, text).unwrap();
    }
    dir
}

fn at(path: &Path, needle: &str, nth: usize) -> String {
    let text = fs::read_to_string(path).unwrap();
    let mut from = 0;
    let mut offset = None;
    for _ in 0..=nth {
        let Some(found) = text[from..].find(needle) else {
            panic!("`{needle}` #{nth} is not in {}", path.display());
        };
        offset = Some(from + found);
        from += found + 1;
    }
    let offset = offset.unwrap();
    let line = text[..offset].matches('\n').count() + 1;
    let line_start = text[..offset].rfind('\n').map(|i| i + 1).unwrap_or(0);
    format!("{}:{}:{}", path.display(), line, offset - line_start + 1)
}

fn query(args: &[&str]) -> (Vec<Json>, i32) {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ingot"));
    command.arg("query");
    command.args(args);
    command.arg("--json");
    let Output { stdout, status, .. } = command.output().unwrap();
    let lines: Vec<Json> = String::from_utf8_lossy(&stdout)
        .lines()
        .map(|line| json::parse(line).unwrap_or_else(|e| panic!("{line}: {e}")))
        .collect();
    (lines, status.code().unwrap())
}

fn def(at: &str) -> Vec<Json> {
    let (lines, status) = query(&["def", at]);
    assert_eq!(status, 0, "{lines:?}");
    lines_with_type(&lines, "definition")
}

fn typ(at: &str) -> Option<Json> {
    let (lines, status) = query(&["type", at]);
    assert_eq!(status, 0, "{lines:?}");
    lines_with_type(&lines, "type_at").into_iter().next()
}

fn symbols(args: &[&str]) -> Vec<Json> {
    let mut all = vec!["symbols"];
    all.extend_from_slice(args);
    let (lines, status) = query(&all);
    assert_eq!(status, 0, "{lines:?}");
    lines_with_type(&lines, "symbol")
}

fn lines_with_type(lines: &[Json], kind: &str) -> Vec<Json> {
    lines
        .iter()
        .filter(|line| line.get("type").and_then(Json::as_str) == Some(kind))
        .cloned()
        .collect()
}

fn field<'a>(line: &'a Json, name: &str) -> &'a Json {
    line.get(name)
        .unwrap_or_else(|| panic!("no `{name}` in {line:?}"))
}

fn text_field<'a>(line: &'a Json, name: &str) -> &'a str {
    field(line, name).as_str().unwrap()
}

const LIB: &str = r#"/// A ticket.
pub struct Ticket {
    /// Where it sits.
    pub seat: i64,
}

/// A point.
pub copy struct Point { pub x: i64, pub y: i64 }

/// A token.
linear struct Token { x: i64 }

/// A colour.
pub enum Colour {
    /// Red.
    red,
    /// Green.
    green: i64,
}

/// Adds one.
pub fn helper(value: i64) -> i64 {
    return value + 1;
}

/// Ticket methods.
impl Ticket {
    /// Twice the seat.
    pub fn twice(self: &Ticket) -> i64 {
        return self.seat + self.seat;
    }
}

/// A named thing.
pub interface Named {
    fn name(self: &Self) -> i64;
}

impl Named for Ticket {
    fn name(self: &Ticket) -> i64 {
        return self.seat;
    }
}
"#;

const MAIN: &str = r#"import app.lib;
import std.Env;

// A plain comment.

/// Adds two.
fn add(a: i64, b: i64) -> i64 {
    return a + b;
}

fn run(t: lib.Ticket) -> i64 {
    let x = t.seat;
    let y = lib.helper(x);
    let c = lib.Colour.green(y);
    let n = match c {
        .green(v) => v,
        .red => 0,
    };
    let z = t.twice();
    return add(x, n) + z;
}

fn points(p: lib.Point) -> i64 {
    let q = lib.Point.{ x: p.x, y: p.y };
    return match q {
        .{ x, y } => x + y,
    };
}

fn generic[T: lib.Named](v: &T) -> i64 {
    return v.name();
}

fn closure() -> i64 {
    let f = fn [](v: i64) -> i64 {
        return v;
    };
    return f(1);
}

fn use_env(env: &Env) -> i64 {
    return 0;
}

test "sample test" {
    let answer = add(1, 2);
    _ = answer;
}
"#;

fn fixture() -> (PathBuf, PathBuf, PathBuf) {
    let dir = package(
        "def",
        &[
            ("ingot.toml", "name = \"app\"\n"),
            ("lib.ingot", LIB),
            ("main.ingot", MAIN),
        ],
    );
    let lib = dir.join("lib.ingot");
    let main = dir.join("main.ingot");
    (dir, lib, main)
}

#[test]
fn def_finds_locals_params_calls_and_fields() {
    let (_dir, _lib, main) = fixture();
    let local = def(&at(&main, "x)", 0));
    assert_eq!(text_field(&local[0], "path"), "x");
    assert_eq!(text_field(&local[0], "item"), "local");

    let parameter = def(&at(&main, "a + b", 0));
    assert_eq!(text_field(&parameter[0], "path"), "a");
    assert_eq!(text_field(&parameter[0], "item"), "param");

    let call = def(&at(&main, "add(x, n)", 0));
    assert_eq!(text_field(&call[0], "path"), "app.add");
    assert_eq!(text_field(&call[0], "item"), "fn");

    let inherent = def(&at(&main, "twice", 0));
    assert_eq!(text_field(&inherent[0], "path"), "app.lib.Ticket.twice");

    let interface = def(&at(&main, "name()", 0));
    assert_eq!(text_field(&interface[0], "path"), "app.lib.Named.name");

    let field = def(&at(&main, "seat", 0));
    assert_eq!(text_field(&field[0], "path"), "app.lib.Ticket.seat");
    assert_eq!(text_field(&field[0], "item"), "field");
}

#[test]
fn def_finds_struct_literal_and_pattern_fields_and_variants() {
    let (_dir, _lib, main) = fixture();
    let literal = def(&at(&main, "x: p.x", 0));
    assert_eq!(text_field(&literal[0], "path"), "app.lib.Point.x");
    assert_eq!(text_field(&literal[0], "item"), "field");

    let pattern = def(&at(&main, "x, y", 0));
    assert_eq!(text_field(&pattern[0], "path"), "app.lib.Point.x");

    let variant = def(&at(&main, "green(y)", 0));
    assert_eq!(text_field(&variant[0], "path"), "app.lib.Colour.green");
    assert_eq!(text_field(&variant[0], "item"), "variant");

    let pattern_variant = def(&at(&main, "green(v)", 0));
    assert_eq!(
        text_field(&pattern_variant[0], "path"),
        "app.lib.Colour.green"
    );
}

#[test]
fn def_finds_types_modules_imports_and_declarations() {
    let (_dir, _lib, main) = fixture();
    let ticket_at = shift(&at(&main, "lib.Ticket", 0), 4);
    let ticket = def(&ticket_at);
    assert_eq!(text_field(&ticket[0], "path"), "app.lib.Ticket");
    assert_eq!(text_field(&ticket[0], "item"), "struct");
    assert_eq!(
        ticket[0]
            .get("span")
            .and_then(|span| span.get("file"))
            .and_then(Json::as_str),
        Some("lib.ingot")
    );

    // The first path segment names the module.
    let module = def(&at(&main, "lib.Ticket", 0));
    assert_eq!(text_field(&module[0], "path"), "app.lib");
    assert_eq!(text_field(&module[0], "item"), "module");

    let import_module = def(&at(&main, "app.lib", 0));
    assert_eq!(text_field(&import_module[0], "path"), "app");
    let import_final = def(&shift(&at(&main, "app.lib", 0), 4));
    assert_eq!(text_field(&import_final[0], "path"), "app.lib");
    assert_eq!(text_field(&import_final[0], "item"), "module");

    let declaration = def(&at(&main, "add(a", 0));
    assert_eq!(text_field(&declaration[0], "path"), "app.add");

    let std_item = def(&at(&main, "Env", 0));
    assert_eq!(text_field(&std_item[0], "path"), "std.Env");
    assert_eq!(
        std_item[0]
            .get("span")
            .and_then(|span| span.get("file"))
            .and_then(Json::as_str),
        Some("std/main.ingot")
    );
}

fn shift(value: &str, delta: usize) -> String {
    let (path, line, col) = split_at(value);
    format!("{path}:{line}:{}", col.parse::<usize>().unwrap() + delta)
}

fn split_at(value: &str) -> (String, String, String) {
    let (rest, col) = value.rsplit_once(':').unwrap();
    let (path, line) = rest.rsplit_once(':').unwrap();
    (path.to_string(), line.to_string(), col.to_string())
}

#[test]
fn def_finds_self_generics_closures_and_test_bodies() {
    let (_dir, lib, main) = fixture();
    let self_ty = def(&at(&lib, "Self", 0));
    assert_eq!(text_field(&self_ty[0], "path"), "app.lib.Named");
    assert_eq!(text_field(&self_ty[0], "item"), "interface");

    let generic = def(&at(&main, "T: lib.Named", 0));
    assert_eq!(text_field(&generic[0], "path"), "T");
    assert_eq!(text_field(&generic[0], "item"), "generic");

    let closure_param = def(&at(&main, "v: i64", 0));
    assert_eq!(text_field(&closure_param[0], "path"), "v");
    assert_eq!(text_field(&closure_param[0], "item"), "param");

    let test_local = def(&at(&main, "answer", 0));
    assert_eq!(text_field(&test_local[0], "path"), "answer");
    assert_eq!(text_field(&test_local[0], "item"), "local");

    let test = def(&at(&main, "sample test", 0));
    assert_eq!(text_field(&test[0], "path"), "app.sample test");
    assert_eq!(text_field(&test[0], "item"), "test");
}

#[test]
fn def_maps_self_in_an_impl_to_the_target_type() {
    let dir = package(
        "self-impl",
        &[
            ("ingot.toml", "name = \"app\"\n"),
            (
                "main.ingot",
                "pub struct T { pub x: i64 }\n\nimpl T {\n    fn me(self: &Self) -> i64 { return self.x; }\n}\n",
            ),
        ],
    );
    let main = dir.join("main.ingot");
    let self_ty = def(&at(&main, "Self", 0));
    assert_eq!(text_field(&self_ty[0], "path"), "app.T");
    assert_eq!(text_field(&self_ty[0], "item"), "struct");

    let method = def(&at(&main, "me", 0));
    assert_eq!(text_field(&method[0], "path"), "app.T.me");
}

#[test]
fn def_names_nothing_on_whitespace_comments_keywords_and_builtins() {
    let (_dir, _lib, main) = fixture();
    for needle in [
        "    return a + b;",
        "// A plain comment.",
        "return a + b",
        "i64 {",
    ] {
        let (lines, status) = query(&["def", &at(&main, needle, 0)]);
        assert_eq!(status, 0, "{needle}: {lines:?}");
        assert!(
            lines_with_type(&lines, "definition").is_empty(),
            "{needle}: {lines:?}"
        );
        assert_eq!(
            lines
                .last()
                .and_then(|line| line.get("ok"))
                .and_then(Json::as_bool),
            Some(true)
        );
    }
}

#[test]
fn a_missing_file_is_a_failure() {
    let (_dir, _lib, main) = fixture();
    let missing = main.parent().unwrap().join("missing.ingot");
    let (lines, status) = query(&["def", &format!("{}:1:1", missing.display())]);
    assert_eq!(status, 2, "{lines:?}");
    assert_eq!(
        lines
            .first()
            .and_then(|line| line.get("type"))
            .and_then(Json::as_str),
        Some("error")
    );
}

#[test]
fn query_name_is_not_implemented() {
    let (_dir, _lib, main) = fixture();
    let (lines, status) = query(&["def", &at(&main, "add(a", 0), "--name", "app.add"]);
    assert_eq!(status, 2, "{lines:?}");
    assert_eq!(
        lines
            .first()
            .and_then(|line| line.get("message"))
            .and_then(Json::as_str),
        Some("not implemented: `query --name`")
    );
}

#[test]
fn a_position_past_the_end_of_the_file_is_a_failure() {
    let (_dir, _lib, main) = fixture();
    let (lines, status) = query(&["def", &format!("{}:9999:1", main.display())]);
    assert_eq!(status, 2, "{lines:?}");
    assert_eq!(
        lines
            .first()
            .and_then(|line| line.get("type"))
            .and_then(Json::as_str),
        Some("error")
    );
}

const SIGNATURES: &str = r#"/// First line.
/// Second line.
pub fn long(
    a: i64,
    b: i64,
) -> i64 {
    return a + b;
}

/// A header.
pub struct Header[T] {
    /// A field.
    pub value: T,
}

/// A width.
pub const width: i64 = 2 + 2;

pub enum E {
    /// The first.
    first: i64,
}

/// Not attached.

pub fn blank() -> i64 {
    return 0;
}

// plain comment
pub fn commented() -> i64 {
    return 0;
}

test "a { in the name" {
}
"#;

#[test]
fn signatures_and_docs_are_cut_from_the_source() {
    let dir = package(
        "signatures",
        &[
            ("ingot.toml", "name = \"app\"\n"),
            ("main.ingot", SIGNATURES),
        ],
    );
    let main = dir.join("main.ingot");

    let long = def(&at(&main, "long", 0));
    assert_eq!(
        text_field(&long[0], "signature"),
        "pub fn long(\n    a: i64,\n    b: i64,\n) -> i64"
    );
    assert_eq!(text_field(&long[0], "doc"), "First line.\nSecond line.");

    let header = def(&at(&main, "Header", 0));
    assert_eq!(text_field(&header[0], "signature"), "pub struct Header[T]");
    assert_eq!(text_field(&header[0], "doc"), "A header.");

    let field = def(&at(&main, "value", 0));
    assert_eq!(text_field(&field[0], "signature"), "pub value: T");
    assert_eq!(text_field(&field[0], "doc"), "A field.");

    let width = def(&at(&main, "width: i64", 0));
    assert_eq!(
        text_field(&width[0], "signature"),
        "pub const width: i64 = 2 + 2"
    );
    assert_eq!(text_field(&width[0], "doc"), "A width.");

    let variant = def(&at(&main, "first: i64", 0));
    assert_eq!(text_field(&variant[0], "signature"), "first: i64");
    assert_eq!(text_field(&variant[0], "doc"), "The first.");

    let blank = def(&at(&main, "blank", 0));
    assert!(blank[0].get("doc").is_none(), "{:?}", blank[0]);

    let commented = def(&at(&main, "commented", 0));
    assert!(commented[0].get("doc").is_none(), "{:?}", commented[0]);

    // The body's `{` is the first one after the name, not the one inside it.
    let test = def(&at(&main, "a { in", 0));
    assert_eq!(
        text_field(&test[0], "signature"),
        "test \"a { in the name\""
    );
}

const TYPES: &str = r#"copy struct Pair { pub a: i64, pub b: i64 }
linear struct Token { x: i64 }
fn add(a: i64, b: i64) -> i64 { return a + b; }
fn takes(s: []u8) -> u8 { return s[0]; }
fn pattern(p: Pair) -> i64 {
    let Pair.{ a, b } = p;
    return a + b;
}
fn run(p: Pair, s: []u8) -> i64 {
    let n = 2;
    let sum = add(p.a, p.b);
    let t = Token.{ x: 1 };
    return sum + n + t.x;
}
"#;

#[test]
fn type_answers_for_expressions_bindings_and_parameters() {
    let dir = package(
        "types",
        &[("ingot.toml", "name = \"app\"\n"), ("main.ingot", TYPES)],
    );
    let main = dir.join("main.ingot");

    let literal = typ(&at(&main, "2;", 0)).expect("literal");
    assert_eq!(text_field(&literal, "of"), "i64");

    let binary = typ(&shift(&at(&main, "sum + n", 0), 4)).expect("binary");
    assert_eq!(text_field(&binary, "of"), "i64");

    let field_type = typ(&shift(&at(&main, "p.a", 0), 2)).expect("field");
    assert_eq!(text_field(&field_type, "of"), "i64");

    let call = typ(&at(&main, "add(p.a", 0)).expect("callee");
    assert_eq!(text_field(&call, "of"), "fn(i64, i64) -> i64");
    let call_paren = at(&main, "add(p.a", 0);
    let (path, line, col) = split_at(&call_paren);
    let call = typ(&format!(
        "{path}:{line}:{}",
        col.parse::<usize>().unwrap() + 3
    ))
    .expect("call");
    assert_eq!(text_field(&call, "of"), "i64");

    let binding = typ(&at(&main, "n =", 0)).expect("let binding");
    assert_eq!(text_field(&binding, "of"), "i64");

    let pattern_binding = typ(&at(&main, "a, b", 0)).expect("pattern binding");
    assert_eq!(text_field(&pattern_binding, "of"), "i64");

    let parameter = typ(&at(&main, "p: Pair", 0)).expect("parameter");
    assert_eq!(text_field(&parameter, "of"), "Pair");

    let linear = typ(&at(&main, "t =", 0)).expect("linear binding");
    assert_eq!(text_field(&linear, "of"), "Token");
    assert_eq!(
        field(&linear, "properties")
            .get("linear")
            .and_then(Json::as_bool),
        Some(true)
    );

    let view = typ(&at(&main, "s: []u8", 0)).expect("view parameter");
    assert_eq!(text_field(&view, "of"), "[]u8");
    assert_eq!(
        field(&view, "properties")
            .get("view")
            .and_then(Json::as_bool),
        Some(true)
    );

    let signature = typ(&at(&main, "-> i64", 0));
    assert!(signature.is_none(), "{signature:?}");
}

const SYMBOLS: &str = r#"/// A struct.
pub struct S {
    pub a: i64,
}

pub enum E { one, two: i64 }

pub interface I {
    fn m(self: &Self) -> i64;
}

impl S {
    fn own(self: &S) -> i64 { return self.a; }
}

impl I for S {
    fn m(self: &S) -> i64 { return self.a; }
}

pub const c: i64 = 1;
pub static g: i64 = 2;
pub type A = S;
pub fn f() -> i64 { return 0; }
extern "c" {
    fn ext(x: i32) -> i32;
}
test "a test" {
}
"#;

#[test]
fn symbols_are_in_source_order_with_members() {
    let dir = package(
        "symbols",
        &[
            ("ingot.toml", "name = \"app\"\n"),
            ("main.ingot", SYMBOLS),
            ("other.ingot", "pub fn other() -> i64 { return 1; }\n"),
        ],
    );
    let paths: Vec<String> = symbols(&[&dir.to_string_lossy()])
        .into_iter()
        .map(|symbol| {
            symbol
                .get("path")
                .and_then(Json::as_str)
                .unwrap()
                .to_string()
        })
        .collect();
    assert_eq!(
        paths,
        [
            "app.S",
            "app.S.a",
            "app.E",
            "app.E.one",
            "app.E.two",
            "app.I",
            "app.I.m",
            "app.S.own",
            "app.S.m",
            "app.c",
            "app.g",
            "app.A",
            "app.f",
            "app.ext",
            "app.a test",
            "app.other.other",
        ],
        "{paths:?}"
    );

    let matched: Vec<String> = symbols(&[&dir.to_string_lossy(), "--match", "S."])
        .into_iter()
        .map(|symbol| {
            symbol
                .get("path")
                .and_then(Json::as_str)
                .unwrap()
                .to_string()
        })
        .collect();
    assert_eq!(matched, ["app.S.a", "app.S.own", "app.S.m"]);

    let single = symbols(&[&dir.join("main.ingot").to_string_lossy()]);
    assert!(single.iter().all(|symbol| {
        symbol
            .get("span")
            .and_then(|span| span.get("file"))
            .and_then(Json::as_str)
            == Some("main.ingot")
    }));
    assert!(
        single
            .iter()
            .any(|symbol| symbol.get("path").and_then(Json::as_str) == Some("app.f"))
    );
    assert!(
        !single
            .iter()
            .any(|symbol| symbol.get("path").and_then(Json::as_str) == Some("app.other"))
    );
}

// --------------------------------------------------------------- completion

const COMPLETE_LIB: &str = r#"/// A ticket.
pub struct Ticket {
    /// The seat.
    pub seat: i64,
    pub row: i64,
    private: i64,
}

impl Ticket {
    /// Twice the seat.
    pub fn twice(self: &Ticket) -> i64 {
        return self.seat + self.seat;
    }
    pub fn label(self: &Ticket) -> i64 {
        return self.row;
    }
}

/// A named thing.
pub interface Named {
    fn name(self: &Self) -> i64;
}

impl Named for Ticket {
    fn name(self: &Ticket) -> i64 {
        return self.seat;
    }
}

/// A colour.
pub enum Colour {
    red,
    green: i64,
}

pub fn helper(value: i64) -> i64 {
    return value + 1;
}

fn private_helper() -> i64 {
    return 0;
}

pub fn ticket() -> i64 {
    return 1;
}

pub const max: i64 = 3;
"#;

fn after(path: &Path, needle: &str, nth: usize) -> String {
    shift(&at(path, needle, nth), needle.len())
}

fn completions(position: &str) -> Vec<Json> {
    let (lines, status) = query(&["complete", position]);
    assert_eq!(status, 0, "{lines:?}");
    let summary = lines.last().expect("summary");
    assert_eq!(
        summary.get("ok").and_then(Json::as_bool),
        Some(true),
        "{lines:?}"
    );
    lines_with_type(&lines, "completion")
}

fn completion_names(lines: &[Json]) -> Vec<String> {
    let mut names: Vec<String> = lines
        .iter()
        .map(|line| line.get("name").and_then(Json::as_str).unwrap().to_string())
        .collect();
    names.sort();
    names
}

fn complete_package(label: &str) -> (PathBuf, PathBuf) {
    let dir = package(
        label,
        &[
            ("ingot.toml", "name = \"app\"\n"),
            ("lib.ingot", COMPLETE_LIB),
            ("main.ingot", ""),
        ],
    );
    let main = dir.join("main.ingot");
    (dir, main)
}

#[test]
fn completion_after_a_value_dot_offers_fields_methods_and_builtin_members() {
    let (_dir, main) = complete_package("complete-value");

    let text =
        "import app.lib;\nfn run(t: lib.Ticket) -> i64 {\n    let x = t.;\n    return x;\n}\n";
    fs::write(&main, text).unwrap();
    let lines = completions(&after(&main, "t.", 0));
    // `name` is supplied by the impl of `Named`: offered once, as the impl's method.
    assert_eq!(
        completion_names(&lines),
        ["label", "name", "row", "seat", "twice"]
    );
    let name = lines
        .iter()
        .find(|line| line.get("name").and_then(Json::as_str) == Some("name"))
        .unwrap();
    assert_eq!(text_field(name, "path"), "app.lib.Ticket.name");
    assert!(
        !lines
            .iter()
            .any(|line| { line.get("name").and_then(Json::as_str) == Some("private") })
    );
    let seat = lines
        .iter()
        .find(|line| line.get("name").and_then(Json::as_str) == Some("seat"))
        .unwrap();
    assert_eq!(text_field(seat, "item"), "field");
    assert_eq!(text_field(seat, "path"), "app.lib.Ticket.seat");
    assert_eq!(text_field(seat, "signature"), "pub seat: i64");
    assert_eq!(text_field(seat, "doc"), "The seat.");

    // A partial name parses as written: no placeholder is needed.
    let text =
        "import app.lib;\nfn run(t: lib.Ticket) -> i64 {\n    let x = t.se;\n    return x;\n}\n";
    fs::write(&main, text).unwrap();
    let lines = completions(&after(&main, "t.se", 0));
    assert_eq!(completion_names(&lines), ["seat"]);

    // A `&T` parameter looks through the borrow.
    let text =
        "import app.lib;\nfn run(t: &lib.Ticket) -> i64 {\n    let x = t.;\n    return x;\n}\n";
    fs::write(&main, text).unwrap();
    let names = completion_names(&completions(&after(&main, "t.", 0)));
    assert!(names.contains(&"seat".to_string()), "{names:?}");
    assert!(names.contains(&"twice".to_string()), "{names:?}");

    // A cursor at the end of a line, before the `;` the line still needs.
    let text =
        "import app.lib;\nfn run(t: lib.Ticket) -> i64 {\n    let x = t.\n    return x;\n}\n";
    fs::write(&main, text).unwrap();
    let names = completion_names(&completions(&after(&main, "t.", 0)));
    assert!(names.contains(&"seat".to_string()), "{names:?}");

    // `t.` typed at the start of a statement, where only a call may stand (E0206).
    let text = "import app.lib;\nfn run(t: lib.Ticket) -> i64 {\n    let n = 1;\n    t.\n    return n;\n}\n";
    fs::write(&main, text).unwrap();
    let names = completion_names(&completions(&after(&main, "    t.", 0)));
    assert!(names.contains(&"seat".to_string()), "{names:?}");
    assert!(names.contains(&"twice".to_string()), "{names:?}");

    // A name typed at the start of a statement offers the names in scope.
    let text = "import app.lib;\nfn run(t: lib.Ticket) -> i64 {\n    let count = 1;\n    co\n    return count;\n}\n";
    fs::write(&main, text).unwrap();
    let names = completion_names(&completions(&after(&main, "    co", 0)));
    assert!(names.contains(&"count".to_string()), "{names:?}");

    // The members the language gives a slice.
    let text = "fn run(s: []u8) -> i64 {\n    let x = s.;\n    return x;\n}\n";
    fs::write(&main, text).unwrap();
    let lines = completions(&after(&main, "s.", 0));
    let names = completion_names(&lines);
    assert!(names.contains(&"len".to_string()), "{names:?}");
    assert!(names.contains(&"ptr".to_string()), "{names:?}");
    for name in ["len", "ptr"] {
        let line = lines
            .iter()
            .find(|line| line.get("name").and_then(Json::as_str) == Some(name))
            .unwrap();
        assert_eq!(line.get("item").and_then(Json::as_str), Some("fn"));
    }
}

#[test]
fn completion_after_a_type_parameter_offers_its_bounds_methods() {
    let (_dir, main) = complete_package("complete-bound");
    let text = "import app.lib;\nfn run[T: lib.Named](v: &T) -> i64 {\n    let x = v.;\n    return x;\n}\n";
    fs::write(&main, text).unwrap();
    let lines = completions(&after(&main, "v.", 0));
    assert_eq!(completion_names(&lines), ["name"]);
    assert_eq!(text_field(&lines[0], "path"), "app.lib.Named.name");
}

#[test]
fn completion_after_a_module_and_a_type_offers_their_items() {
    let (_dir, main) = complete_package("complete-context");

    let text = "import app.lib;\nfn run() -> i64 {\n    let x = lib.;\n    return x;\n}\n";
    fs::write(&main, text).unwrap();
    let lines = completions(&after(&main, "lib.", 0));
    let names = completion_names(&lines);
    assert_eq!(
        names,
        ["Colour", "Named", "Ticket", "helper", "max", "ticket"]
    );
    assert_eq!(
        lines
            .iter()
            .find(|line| line.get("name").and_then(Json::as_str) == Some("helper"))
            .map(|line| text_field(line, "signature")),
        Some("pub fn helper(value: i64) -> i64")
    );

    let text =
        "import app.lib;\nfn run() -> lib.Colour {\n    let x = lib.Colour.;\n    return x;\n}\n";
    fs::write(&main, text).unwrap();
    let lines = completions(&after(&main, "lib.Colour.", 0));
    assert_eq!(completion_names(&lines), ["green", "red"]);
    assert!(
        lines
            .iter()
            .all(|line| line.get("item").and_then(Json::as_str) == Some("variant"))
    );

    let text = "import app.lib;\nfn run() -> i64 {\n    let x = lib.Ticket.;\n    return x;\n}\n";
    fs::write(&main, text).unwrap();
    let lines = completions(&after(&main, "lib.Ticket.", 0));
    assert_eq!(completion_names(&lines), ["label", "twice"]);
}

#[test]
fn completion_offers_std_items_after_an_import() {
    let (_dir, main) = complete_package("complete-std");
    let text = "import app.lib;\nimport std.Env;\nfn run(e: &En) -> i64 {\n    return 0;\n}\n";
    fs::write(&main, text).unwrap();
    let lines = completions(&after(&main, "&En", 0));
    let env = lines
        .iter()
        .find(|line| line.get("name").and_then(Json::as_str) == Some("Env"))
        .expect("Env");
    assert_eq!(text_field(env, "path"), "std.Env");
}

#[test]
fn completion_offers_names_in_scope() {
    let (_dir, main) = complete_package("complete-scope");

    // A local before the cursor is offered; one after it is not.
    let text = "fn run(a: i64) -> i64 {\n    let before = 1;\n    let x = 2;\n    let z = \"s\";\n    let after = 2;\n    return before + after + x;\n}\n";
    fs::write(&main, text).unwrap();
    let position = after(&main, "\"s\"", 0);
    let names = completion_names(&completions(&position));
    assert!(names.contains(&"a".to_string()), "{names:?}");
    assert!(names.contains(&"before".to_string()), "{names:?}");
    assert!(names.contains(&"x".to_string()), "{names:?}");
    assert!(!names.contains(&"z".to_string()), "{names:?}");
    assert!(!names.contains(&"after".to_string()), "{names:?}");

    // A closed block's local is not in scope.
    let text = "fn run() -> i64 {\n    {\n        let closed = 1;\n    }\n    let x = clo;\n    return x;\n}\n";
    fs::write(&main, text).unwrap();
    let lines = completions(&after(&main, "clo", 0));
    assert!(lines.is_empty(), "{lines:?}");

    // A shadowed name is offered once, as the innermost.
    let text = "fn run() -> i64 {\n    let v = 1;\n    {\n        let v = true;\n        let x = v;\n    }\n    return 0;\n}\n";
    fs::write(&main, text).unwrap();
    let lines = completions(&after(&main, "= v;", 0));
    let vs: Vec<&Json> = lines
        .iter()
        .filter(|line| line.get("name").and_then(Json::as_str) == Some("v"))
        .collect();
    assert_eq!(vs.len(), 1, "{lines:?}");
    assert_eq!(vs[0].get("of").and_then(Json::as_str), Some("bool"));

    // A closure parameter.
    let text =
        "fn run() -> i64 {\n    let f = fn [](c: i64) -> i64 { return c; };\n    return 0;\n}\n";
    fs::write(&main, text).unwrap();
    let lines = completions(&after(&main, "return c", 0));
    let c = lines
        .iter()
        .find(|line| line.get("name").and_then(Json::as_str) == Some("c"))
        .expect("c");
    assert_eq!(text_field(c, "item"), "param");
    assert_eq!(text_field(c, "of"), "i64");

    // A loop binding.
    let text =
        "fn run() -> i64 {\n    for i in 0..3 {\n        let z = i;\n    }\n    return 0;\n}\n";
    fs::write(&main, text).unwrap();
    let lines = completions(&after(&main, "= i;", 0));
    let i = lines
        .iter()
        .find(|line| line.get("name").and_then(Json::as_str) == Some("i"))
        .expect("i");
    assert_eq!(text_field(i, "item"), "local");
    assert_eq!(text_field(i, "of"), "i64");

    // A generic parameter.
    let text =
        "import app.lib;\nfn run[T: lib.Named](v: &T) -> i64 {\n    let x = T;\n    return 0;\n}\n";
    fs::write(&main, text).unwrap();
    let lines = completions(&after(&main, "= T;", 0));
    let generic = lines
        .iter()
        .find(|line| line.get("name").and_then(Json::as_str) == Some("T"))
        .expect("T");
    assert_eq!(text_field(generic, "item"), "generic");
    assert_eq!(text_field(generic, "path"), "T");
}

#[test]
fn completion_separates_types_from_values_and_offers_builtins() {
    let (_dir, main) = complete_package("complete-case");
    let text = "import app.lib.Ticket;\nimport app.lib.ticket;\n\nfn run() -> i64 {\n    let x = Ti;\n    let y = ti;\n    let z = Op;\n    return 0;\n}\n";
    fs::write(&main, text).unwrap();
    assert_eq!(
        completion_names(&completions(&after(&main, "= Ti", 0))),
        ["Ticket"]
    );
    assert_eq!(
        completion_names(&completions(&after(&main, "= ti", 0))),
        ["ticket"]
    );
    let lines = completions(&after(&main, "= Op", 0));
    assert_eq!(completion_names(&lines), ["Option"]);
    assert_eq!(text_field(&lines[0], "item"), "builtin");
}

#[test]
fn completion_offers_keywords_beside_names_and_builtins_after_an_at() {
    let (_dir, main) = complete_package("complete-keywords");
    // A partial name at the start of a statement: the local and the keywords.
    let text = "fn run() -> i64 {\n    let result = 1;\n    re\n    return result;\n}\n";
    fs::write(&main, text).unwrap();
    let lines = completions(&after(&main, "    re", 0));
    assert_eq!(completion_names(&lines), ["result", "return"]);
    let keyword = lines
        .iter()
        .find(|line| line.get("name").and_then(Json::as_str) == Some("return"))
        .unwrap();
    assert_eq!(text_field(keyword, "item"), "keyword");
    assert!(keyword.get("path").is_none(), "{keyword:?}");

    // Keywords are not members: nothing after a dot is a keyword.
    let text =
        "import app.lib;\nfn run(t: lib.Ticket) -> i64 {\n    let x = t.;\n    return x;\n}\n";
    fs::write(&main, text).unwrap();
    let lines = completions(&after(&main, "t.", 0));
    assert!(
        lines
            .iter()
            .all(|line| text_field(line, "item") != "keyword"),
        "{lines:?}"
    );

    // After `@`: the builtins only, without the `@`, in order of name.
    let text = "fn run() -> i64 {\n    let x = @a;\n    return 0;\n}\n";
    fs::write(&main, text).unwrap();
    let lines = completions(&after(&main, "@a", 0));
    let names = completion_names(&lines);
    assert!(names.contains(&"abs".to_string()), "{names:?}");
    assert!(names.contains(&"alignof".to_string()), "{names:?}");
    assert!(names.iter().all(|name| name.starts_with('a')), "{names:?}");
    let mut sorted = names.clone();
    sorted.sort();
    assert_eq!(names, sorted);
    assert!(
        lines
            .iter()
            .all(|line| text_field(line, "item") == "builtin_fn")
    );

    // `@` with nothing after it offers every builtin, whatever else is broken.
    let text = "fn run() -> i64 {\n    @\n    return 0;\n}\n";
    fs::write(&main, text).unwrap();
    let lines = completions(&after(&main, "@", 0));
    assert_eq!(lines.len(), ingot_syntax::lexer::BUILTINS.len());
}

#[test]
fn completion_works_among_the_items_of_a_module() {
    let (_dir, main) = complete_package("complete-items");
    let body = "fn add(a: i64, b: i64) -> i64 {\n    return a + b;\n}\n\n";

    // A partial keyword where an item would start: no name can stand there.
    for (typed, expected) in [
        ("pu", vec!["pub"]),
        ("st", vec!["static", "struct"]),
        ("pub st", vec!["static", "struct"]),
    ] {
        let text = format!("{body}{typed}\n");
        fs::write(&main, &text).unwrap();
        let names = completion_names(&completions(&after(&main, typed, 0)));
        assert_eq!(names, expected, "{typed}");
    }

    // A type in a signature that is not finished: the scalar types are built-in names.
    let text = format!("{body}fn f() -> i6\n");
    fs::write(&main, &text).unwrap();
    let names = completion_names(&completions(&after(&main, "f() -> i6", 0)));
    assert_eq!(names, ["i64"]);

    // The items of the module are offered there too.
    let text = format!("{body}ad\n");
    fs::write(&main, &text).unwrap();
    let names = completion_names(&completions(&after(&main, "ad", 1)));
    assert!(names.contains(&"add".to_string()), "{names:?}");
}

#[test]
fn symbols_report_the_visibility_level() {
    let dir = package(
        "visibility",
        &[
            ("ingot.toml", "name = \"app\"\n"),
            (
                "main.ingot",
                "pub fn open() {}\npub(pkg) fn inside() {}\nfn local() {}\n",
            ),
        ],
    );
    let lines = symbols(&[&dir.to_string_lossy()]);
    for (name, vis, pub_) in [
        ("open", "pub", true),
        ("inside", "pkg", false),
        ("local", "private", false),
    ] {
        let symbol = lines
            .iter()
            .find(|s| text_field(s, "path") == format!("app.{name}"))
            .unwrap_or_else(|| panic!("{name} is a symbol"));
        assert_eq!(text_field(symbol, "vis"), vis, "{name}");
        assert_eq!(field(symbol, "pub").as_bool(), Some(pub_), "{name}");
    }
}

#[test]
fn completion_in_a_file_with_another_syntax_error_gives_no_lines() {
    let (_dir, main) = complete_package("complete-error");
    let text = "import app.lib;\nfn run(t: lib.Ticket) -> i64 {\n    let x = t.;\n    return x;\n}\nfn broken( {\n";
    fs::write(&main, text).unwrap();
    let lines = completions(&after(&main, "t.", 0));
    assert!(lines.is_empty(), "{lines:?}");
}
