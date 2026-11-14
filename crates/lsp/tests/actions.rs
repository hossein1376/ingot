mod common;

use common::*;
use ingot_lsp::Json;

fn point(line: i64, character: i64) -> Json {
    obj(vec![("line", n(line)), ("character", n(character))])
}

#[test]
fn a_code_action_carries_the_fix_edit() {
    let dir = scratch("actions");
    let file = dir.join("main.ingot");
    let text = "fn f() -> i64 {\n    var total: i64 = 2;\n    return total;\n}\n";
    std::fs::write(&file, text).unwrap();
    let document = uri(&file);
    let mut client = Client::spawn(&[]);
    client.initialize(true);
    client.open(&document, text);
    let diagnostics = client.published(&document);
    let var = diagnostics
        .iter()
        .find(|diagnostic| diagnostic.get("code").and_then(Json::as_str) == Some("E0312"))
        .expect("E0312 for the unmutated var");

    let id = client.request(
        "textDocument/codeAction",
        obj(vec![
            ("textDocument", obj(vec![("uri", s(&document))])),
            (
                "range",
                obj(vec![("start", point(0, 0)), ("end", point(3, 0))]),
            ),
            (
                "context",
                obj(vec![("diagnostics", arr(vec![var.clone()]))]),
            ),
        ]),
    );
    let response = client.response(id);
    assert!(response.get("error").is_none(), "{response:?}");
    let actions = match response.get("result") {
        Some(Json::Array(actions)) => actions,
        other => panic!("expected an array of actions, got {other:?}"),
    };
    let action = actions
        .iter()
        .find(|action| {
            action.get("title").and_then(Json::as_str) == Some("replace `var` with `let`")
        })
        .unwrap_or_else(|| panic!("the E0312 fix is missing: {actions:?}"));
    assert_eq!(action.get("kind").and_then(Json::as_str), Some("quickfix"));
    assert_eq!(
        action
            .get("diagnostics")
            .and_then(|diagnostics| match diagnostics {
                Json::Array(items) => items.first(),
                _ => None,
            })
            .and_then(|diagnostic| diagnostic.get("code"))
            .and_then(Json::as_str),
        Some("E0312")
    );
    let changes = action
        .get("edit")
        .and_then(|edit| edit.get("changes"))
        .expect("changes");
    let edits = changes.get(&document).expect("edits of the open document");
    let Json::Array(edits) = edits else {
        panic!("edits are not an array");
    };
    assert_eq!(edits.len(), 1, "{edits:?}");
    let edit = &edits[0];
    assert_eq!(edit.get("newText").and_then(Json::as_str), Some("let"));
    let range = edit.get("range").unwrap();
    assert_eq!(
        range
            .get("start")
            .and_then(|point| point.get("line"))
            .and_then(Json::as_i64),
        Some(1)
    );
    assert_eq!(
        range
            .get("start")
            .and_then(|point| point.get("character"))
            .and_then(Json::as_i64),
        Some(4)
    );
    assert_eq!(
        range
            .get("end")
            .and_then(|point| point.get("character"))
            .and_then(Json::as_i64),
        Some(7)
    );
}

#[test]
fn an_empty_range_touching_the_span_gives_the_action() {
    let dir = scratch("actions-touch");
    let file = dir.join("main.ingot");
    let text = "fn f() -> i64 {\n    var total: i64 = 2;\n    return total;\n}\n";
    std::fs::write(&file, text).unwrap();
    let document = uri(&file);
    let mut client = Client::spawn(&[]);
    client.initialize(false);
    client.open(&document, text);
    let diagnostics = client.published(&document);
    assert!(!diagnostics.is_empty());
    let id = client.request(
        "textDocument/codeAction",
        obj(vec![
            ("textDocument", obj(vec![("uri", s(&document))])),
            (
                "range",
                obj(vec![("start", point(1, 7)), ("end", point(1, 7))]),
            ),
            ("context", obj(vec![("diagnostics", arr(vec![]))])),
        ]),
    );
    let response = client.response(id);
    let Json::Array(actions) = response.get("result").unwrap() else {
        panic!("expected actions: {response:?}");
    };
    assert!(!actions.is_empty(), "{response:?}");
}

#[test]
fn a_range_that_does_not_touch_the_diagnostic_gives_no_action() {
    let dir = scratch("actions-empty");
    let file = dir.join("main.ingot");
    let text = "fn f() -> i64 {\n    var total: i64 = 2;\n    return total;\n}\n";
    std::fs::write(&file, text).unwrap();
    let document = uri(&file);
    let mut client = Client::spawn(&[]);
    client.initialize(false);
    client.open(&document, text);
    let diagnostics = client.published(&document);
    assert!(!diagnostics.is_empty());
    let id = client.request(
        "textDocument/codeAction",
        obj(vec![
            ("textDocument", obj(vec![("uri", s(&document))])),
            (
                "range",
                obj(vec![("start", point(0, 0)), ("end", point(0, 1))]),
            ),
            ("context", obj(vec![("diagnostics", arr(vec![]))])),
        ]),
    );
    let response = client.response(id);
    assert_eq!(
        response.get("result"),
        Some(&Json::Array(vec![])),
        "{response:?}"
    );
}
