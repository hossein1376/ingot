mod common;

use common::*;
use ingot_lsp::Json;

const LIB: &str = r#"/// A ticket.
pub struct Ticket {
    /// The seat.
    pub seat: i64,
}

impl Ticket {
    /// Twice the seat.
    pub fn twice(self: &Ticket) -> i64 {
        return self.seat + self.seat;
    }
}

pub fn helper(value: i64) -> i64 {
    return value + 1;
}

pub enum Colour {
    red,
    green: i64,
}
"#;

fn position_utf16(text: &str, needle: &str) -> Json {
    let offset = text.find(needle).unwrap_or_else(|| panic!("{needle}")) + needle.len();
    let line = text[..offset].matches('\n').count();
    let start = text[..offset].rfind('\n').map(|i| i + 1).unwrap_or(0);
    obj(vec![
        ("line", n(line)),
        ("character", n(text[start..offset].encode_utf16().count())),
    ])
}

fn complete(client: &mut Client, uri: &str, text: &str, needle: &str) -> Vec<Json> {
    let id = client.request(
        "textDocument/completion",
        obj(vec![
            ("textDocument", obj(vec![("uri", s(uri))])),
            ("position", position_utf16(text, needle)),
        ]),
    );
    let response = client.response(id);
    assert!(response.get("error").is_none(), "{response:?}");
    let result = response.get("result").expect("result");
    assert_eq!(
        result.get("isIncomplete").and_then(Json::as_bool),
        Some(false)
    );
    match result.get("items") {
        Some(Json::Array(items)) => items.clone(),
        other => panic!("expected items, got {other:?}"),
    }
}

fn item<'a>(items: &'a [Json], label: &str) -> &'a Json {
    items
        .iter()
        .find(|item| item.get("label").and_then(Json::as_str) == Some(label))
        .unwrap_or_else(|| panic!("no `{label}` in {items:?}"))
}

#[test]
fn completion_after_a_dot_uses_the_unsaved_text() {
    let dir = package(
        "completion",
        "name = \"app\"\n",
        &[
            ("lib.ingot", LIB),
            ("main.ingot", "fn f() -> i64 { return 0; }\n"),
        ],
    );
    let lib = uri(&dir.join("lib.ingot"));
    let main = uri(&dir.join("main.ingot"));
    let mut client = Client::spawn(&[]);
    let result = client.initialize(false);
    let characters = result
        .get("capabilities")
        .and_then(|capabilities| capabilities.get("completionProvider"))
        .and_then(|provider| provider.get("triggerCharacters"))
        .expect("completionProvider");
    assert_eq!(characters, &Json::Array(vec![s("."), s("@")]));
    client.open(&lib, LIB);
    client.open(&main, "fn f() -> i64 { return 0; }\n");
    client.published(&main);

    let text = "import app.lib;\n\nfn run(t: lib.Ticket) -> i64 {\n    let n = 1;\n    let x = t.\n    return n;\n}\n";
    client.change(&main, text);
    let items = complete(&mut client, &main, text, "t.");
    let seat = item(&items, "seat");
    assert_eq!(seat.get("kind").and_then(Json::as_i64), Some(5));
    assert_eq!(
        seat.get("detail").and_then(Json::as_str),
        Some("pub seat: i64")
    );
    let doc = seat
        .get("documentation")
        .and_then(|documentation| documentation.get("value"))
        .and_then(Json::as_str);
    assert_eq!(doc, Some("The seat."));
    let twice = item(&items, "twice");
    assert_eq!(twice.get("kind").and_then(Json::as_i64), Some(2));
    assert_eq!(
        twice.get("detail").and_then(Json::as_str),
        Some("fn twice(self: &Ticket) -> i64")
    );
    assert_eq!(
        twice
            .get("documentation")
            .and_then(|documentation| documentation.get("value"))
            .and_then(Json::as_str),
        Some("Twice the seat.")
    );

    // A local: kind 6 and `name: type` as the detail.
    let text = "import app.lib;\n\nfn run(t: lib.Ticket) -> i64 {\n    let n = 1;\n    let x = n;\n    return x;\n}\n";
    client.change(&main, text);
    let items = complete(&mut client, &main, text, "= n");
    let local = item(&items, "n");
    assert_eq!(local.get("kind").and_then(Json::as_i64), Some(6));
    assert_eq!(local.get("detail").and_then(Json::as_str), Some("i64"));

    // A type: its inherent functions keep kind 3.
    let text = "import app.lib;\n\nfn run() -> i64 {\n    let x = lib.Ticket.;\n    return x;\n}\n";
    client.change(&main, text);
    let items = complete(&mut client, &main, text, "lib.Ticket.");
    assert_eq!(
        item(&items, "twice").get("kind").and_then(Json::as_i64),
        Some(3)
    );

    // A module: structs and functions, and a module function keeps kind 3.
    let text = "import app.lib;\n\nfn run() -> i64 {\n    let x = lib.;\n    return x;\n}\n";
    client.change(&main, text);
    let items = complete(&mut client, &main, text, "lib.");
    assert_eq!(
        item(&items, "Ticket").get("kind").and_then(Json::as_i64),
        Some(22)
    );
    assert_eq!(
        item(&items, "helper").get("kind").and_then(Json::as_i64),
        Some(3)
    );

    // A keyword at the start of a statement, and a builtin after `@`.
    let text = "fn run() -> i64 {\n    let result = 1;\n    re\n    return result;\n}\n";
    client.change(&main, text);
    let items = complete(&mut client, &main, text, "    re");
    assert_eq!(
        item(&items, "return").get("kind").and_then(Json::as_i64),
        Some(14)
    );
    assert_eq!(
        item(&items, "result").get("kind").and_then(Json::as_i64),
        Some(6)
    );
    let text = "fn run() -> i64 {\n    let x = @ab;\n    return 0;\n}\n";
    client.change(&main, text);
    let items = complete(&mut client, &main, text, "@ab");
    assert_eq!(
        item(&items, "abs").get("kind").and_then(Json::as_i64),
        Some(3)
    );
}
