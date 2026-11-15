mod common;

use common::*;
use ingot_lsp::Json;

const LIB: &str = r#"/// A ticket.
pub struct Ticket {
    /// The seat.
    pub seat: i64,
}

/// Adds one.
pub fn helper(value: i64) -> i64 {
    return value + 1;
}

impl Ticket {
    /// Twice the seat.
    pub fn twice(self: &Ticket) -> i64 {
        return self.seat + self.seat;
    }
}
"#;

const MAIN: &str = r#"import app.lib;
import std.Env;

/// Adds two.
fn add(a: i64, b: i64) -> i64 {
    return a + b;
}

fn env(value: &Env) -> i64 {
    return 0;
}

fn run(t: lib.Ticket) -> i64 {
    let x = t.seat;
    let y = lib.helper(x);
    return add(x, y) + t.twice();
}
"#;

fn position(text: &str, needle: &str, delta: usize) -> Json {
    let offset = text.find(needle).unwrap_or_else(|| panic!("{needle}")) + delta;
    let line = text[..offset].matches('\n').count();
    let start = text[..offset].rfind('\n').map(|i| i + 1).unwrap_or(0);
    obj(vec![("line", n(line)), ("character", n(offset - start))])
}

fn position_utf16(text: &str, needle: &str, delta: usize) -> Json {
    let offset = text.find(needle).unwrap_or_else(|| panic!("{needle}")) + delta;
    let line = text[..offset].matches('\n').count();
    let start = text[..offset].rfind('\n').map(|i| i + 1).unwrap_or(0);
    obj(vec![
        ("line", n(line)),
        ("character", n(text[start..offset].encode_utf16().count())),
    ])
}

fn location(client: &mut Client, uri: &str, text: &str, needle: &str, delta: usize) -> Json {
    let id = client.request(
        "textDocument/definition",
        obj(vec![
            ("textDocument", obj(vec![("uri", s(uri))])),
            ("position", position(text, needle, delta)),
        ]),
    );
    let response = client.response(id);
    assert!(response.get("error").is_none(), "{response:?}");
    response.get("result").cloned().unwrap()
}

fn fixture() -> (std::path::PathBuf, std::path::PathBuf, String, String) {
    let dir = package(
        "queries",
        "name = \"app\"\n",
        &[("lib.ingot", LIB), ("main.ingot", MAIN)],
    );
    let lib = uri(&dir.join("lib.ingot"));
    let main = uri(&dir.join("main.ingot"));
    let main_path = dir.join("main.ingot");
    (dir, main_path, lib, main)
}

#[test]
fn definition_same_file_other_file_and_std() {
    let (_dir, _main_path, lib, main) = fixture();
    let mut client = Client::spawn(&[]);
    client.initialize(true);
    client.open(&main, MAIN);
    client.published(&main);

    let same = location(&mut client, &main, MAIN, "add(x, y)", 0);
    let same = match same {
        Json::Array(items) => items,
        other => panic!("expected locations, got {other:?}"),
    };
    assert_eq!(same.len(), 1, "{same:?}");
    assert_eq!(
        same[0].get("uri").and_then(Json::as_str),
        Some(main.as_str())
    );
    assert_eq!(
        same[0]
            .get("range")
            .and_then(|range| range.get("start"))
            .and_then(|point| point.get("line"))
            .and_then(Json::as_i64),
        Some(4)
    );

    // A field in a file that is not open.
    let other = location(&mut client, &main, MAIN, "t.seat", 2);
    let other = match other {
        Json::Array(items) => items,
        other => panic!("expected locations, got {other:?}"),
    };
    assert_eq!(
        other[0].get("uri").and_then(Json::as_str),
        Some(lib.as_str())
    );
    assert_eq!(
        other[0]
            .get("range")
            .and_then(|range| range.get("start"))
            .and_then(|point| point.get("line"))
            .and_then(Json::as_i64),
        Some(3)
    );

    let std_file = ingot_driver::loader::std_root()
        .map(|root| uri(&ingot_driver::files::normalize(&root).join("main.ingot")))
        .unwrap();
    let std_item = location(&mut client, &main, MAIN, "&Env", 1);
    let std_item = match std_item {
        Json::Array(items) => items,
        other => panic!("expected locations, got {other:?}"),
    };
    assert_eq!(
        std_item[0].get("uri").and_then(Json::as_str),
        Some(std_file.as_str())
    );

    // Nothing at a whitespace position.
    let id = client.request(
        "textDocument/definition",
        obj(vec![
            ("textDocument", obj(vec![("uri", s(&main))])),
            ("position", position(MAIN, "\n\n", 1)),
        ]),
    );
    let response = client.response(id);
    assert_eq!(response.get("result"), Some(&Json::Null), "{response:?}");
}

#[test]
fn hover_shows_signatures_docs_and_local_types() {
    let (_dir, _main_path, _lib, main) = fixture();
    let mut client = Client::spawn(&[]);
    client.initialize(true);
    client.open(&main, MAIN);
    client.published(&main);

    let id = client.request(
        "textDocument/hover",
        obj(vec![
            ("textDocument", obj(vec![("uri", s(&main))])),
            ("position", position(MAIN, "fn add", 3)),
        ]),
    );
    let response = client.response(id);
    let value = response
        .get("result")
        .and_then(|result| result.get("contents"))
        .and_then(|contents| contents.get("value"))
        .and_then(Json::as_str)
        .unwrap_or_else(|| panic!("no hover: {response:?}"));
    assert!(value.contains("fn add(a: i64, b: i64) -> i64"), "{value}");
    assert!(value.contains("Adds two."), "{value}");

    let id = client.request(
        "textDocument/hover",
        obj(vec![
            ("textDocument", obj(vec![("uri", s(&main))])),
            ("position", position(MAIN, "lib.helper(x)", 11)),
        ]),
    );
    let response = client.response(id);
    let value = response
        .get("result")
        .and_then(|result| result.get("contents"))
        .and_then(|contents| contents.get("value"))
        .and_then(Json::as_str)
        .unwrap_or_else(|| panic!("no hover: {response:?}"));
    assert!(value.contains("x: i64"), "{value}");

    let id = client.request(
        "textDocument/hover",
        obj(vec![
            ("textDocument", obj(vec![("uri", s(&main))])),
            ("position", position(MAIN, "\n\n", 1)),
        ]),
    );
    let response = client.response(id);
    assert_eq!(response.get("result"), Some(&Json::Null), "{response:?}");
}

#[test]
fn document_symbols_have_names_kinds_and_containers() {
    let (_dir, _main_path, lib, main) = fixture();
    let mut client = Client::spawn(&[]);
    client.initialize(false);
    client.open(&lib, LIB);
    client.open(&main, MAIN);
    client.published(&main);

    let id = client.request(
        "textDocument/documentSymbol",
        obj(vec![("textDocument", obj(vec![("uri", s(&main))]))]),
    );
    let response = client.response(id);
    let symbols = match response.get("result") {
        Some(Json::Array(items)) => items,
        other => panic!("expected symbols, got {other:?}"),
    };
    assert!(symbols.iter().any(|symbol| {
        symbol.get("name").and_then(Json::as_str) == Some("add")
            && symbol.get("kind").and_then(Json::as_i64) == Some(12)
    }));

    let id = client.request(
        "textDocument/documentSymbol",
        obj(vec![("textDocument", obj(vec![("uri", s(&lib))]))]),
    );
    let response = client.response(id);
    let symbols = match response.get("result") {
        Some(Json::Array(items)) => items,
        other => panic!("expected symbols, got {other:?}"),
    };
    let ticket = symbols
        .iter()
        .find(|symbol| symbol.get("name").and_then(Json::as_str) == Some("Ticket"))
        .expect("Ticket");
    assert_eq!(ticket.get("kind").and_then(Json::as_i64), Some(23));
    let field = symbols
        .iter()
        .find(|symbol| symbol.get("name").and_then(Json::as_str) == Some("seat"))
        .expect("seat");
    assert_eq!(field.get("kind").and_then(Json::as_i64), Some(8));
    assert_eq!(
        field.get("containerName").and_then(Json::as_str),
        Some("app.lib.Ticket")
    );
    let method = symbols
        .iter()
        .find(|symbol| symbol.get("name").and_then(Json::as_str) == Some("twice"))
        .expect("twice");
    assert_eq!(method.get("kind").and_then(Json::as_i64), Some(6));
}

#[test]
fn workspace_symbols_search_every_open_package() {
    let (_dir, _main_path, _lib, main) = fixture();
    let mut client = Client::spawn(&[]);
    client.initialize(false);
    client.open(&main, MAIN);
    client.published(&main);

    let id = client.request("workspace/symbol", obj(vec![("query", s("Ticket"))]));
    let response = client.response(id);
    let symbols = match response.get("result") {
        Some(Json::Array(items)) => items,
        other => panic!("expected symbols, got {other:?}"),
    };
    assert!(
        symbols
            .iter()
            .any(|symbol| { symbol.get("name").and_then(Json::as_str) == Some("Ticket") }),
        "{symbols:?}"
    );
    assert!(symbols.iter().any(|symbol| {
        symbol.get("name").and_then(Json::as_str) == Some("twice")
            && symbol.get("kind").and_then(Json::as_i64) == Some(6)
    }));
}

#[test]
fn a_query_uses_the_unsaved_overlay() {
    let dir = package(
        "overlay-query",
        "name = \"app\"\n",
        &[("main.ingot", "fn f() -> i64 { return 0; }\n")],
    );
    let file = uri(&dir.join("main.ingot"));
    let changed = "fn f() -> i64 { return g(); }\nfn g() -> i64 { return 1; }\n";
    let mut client = Client::spawn(&[]);
    client.initialize(true);
    client.open(&file, "fn f() -> i64 { return 0; }\n");
    client.published(&file);
    client.change(&file, changed);
    let position = position(changed, "g()", 0);
    let id = client.request(
        "textDocument/definition",
        obj(vec![
            ("textDocument", obj(vec![("uri", s(&file))])),
            ("position", position),
        ]),
    );
    let response = client.response(id);
    let locations = match response.get("result") {
        Some(Json::Array(items)) => items,
        other => panic!("expected locations, got {other:?}"),
    };
    assert_eq!(
        locations[0]
            .get("range")
            .and_then(|range| range.get("start"))
            .and_then(|point| point.get("line"))
            .and_then(Json::as_i64),
        Some(1)
    );
}

#[test]
fn a_utf16_position_after_a_multi_byte_character_is_converted() {
    let text = "fn g(s: []u8, n: i64) -> i64 { return n; }\nfn f() -> i64 {\n    let n = 1;\n    return g(\"😀\", n);\n}\n";
    let dir = package("utf16-query", "name = \"app\"\n", &[("main.ingot", text)]);
    let file = uri(&dir.join("main.ingot"));
    let mut client = Client::spawn(&[]);
    client.initialize(false);
    client.open(&file, text);
    client.published(&file);
    let id = client.request(
        "textDocument/definition",
        obj(vec![
            ("textDocument", obj(vec![("uri", s(&file))])),
            ("position", position_utf16(text, "n);", 0)),
        ]),
    );
    let response = client.response(id);
    let locations = match response.get("result") {
        Some(Json::Array(items)) => items,
        other => panic!("expected locations, got {other:?}"),
    };
    let start = locations[0]
        .get("range")
        .and_then(|range| range.get("start"))
        .unwrap();
    assert_eq!(start.get("line").and_then(Json::as_i64), Some(2));
    assert_eq!(start.get("character").and_then(Json::as_i64), Some(8));
}
