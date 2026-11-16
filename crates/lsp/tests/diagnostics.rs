mod common;

use common::*;
use ingot_lsp::Json;
use std::time::Duration;

fn diagnostic<'a>(diagnostics: &'a [Json], code: &str) -> &'a Json {
    diagnostics
        .iter()
        .find(|diagnostic| diagnostic.get("code").and_then(Json::as_str) == Some(code))
        .unwrap_or_else(|| panic!("no {code} in {diagnostics:?}"))
}

fn range(diagnostic: &Json) -> (i64, i64, i64, i64) {
    let range = diagnostic.get("range").unwrap();
    let point = |name: &str, field: &str| {
        range
            .get(name)
            .and_then(|point| point.get(field))
            .and_then(Json::as_i64)
            .unwrap()
    };
    (
        point("start", "line"),
        point("start", "character"),
        point("end", "line"),
        point("end", "character"),
    )
}

#[test]
fn did_open_publishes_exact_ranges_code_and_related_information() {
    let dir = scratch("duplicate");
    let file = dir.join("main.ingot");
    std::fs::write(&file, "fn f() {}\nfn f() {}\n").unwrap();
    let document = uri(&file);
    let mut client = Client::spawn(&[]);
    client.initialize(false);
    client.open(&document, "fn f() {}\nfn f() {}\n");
    let diagnostics = client.published(&document);
    let duplicate = diagnostic(&diagnostics, "E0308");
    assert_eq!(range(duplicate), (1, 3, 1, 4));
    assert_eq!(duplicate.get("severity").and_then(Json::as_i64), Some(1));
    assert_eq!(
        duplicate.get("source").and_then(Json::as_str),
        Some("ingot")
    );
    let related = duplicate
        .get("relatedInformation")
        .and_then(|related| match related {
            Json::Array(items) => items.first(),
            _ => None,
        })
        .expect("related information");
    assert_eq!(
        related.get("message").and_then(Json::as_str),
        Some("previous")
    );
    let location = related.get("location").unwrap();
    assert_eq!(
        location.get("uri").and_then(Json::as_str),
        Some(document.as_str())
    );
    let previous = location.get("range").unwrap();
    assert_eq!(
        previous
            .get("start")
            .and_then(|point| point.get("line"))
            .and_then(Json::as_i64),
        Some(0)
    );
    assert_eq!(
        previous
            .get("start")
            .and_then(|point| point.get("character"))
            .and_then(Json::as_i64),
        Some(3)
    );
    assert_eq!(
        previous
            .get("end")
            .and_then(|point| point.get("character"))
            .and_then(Json::as_i64),
        Some(4)
    );
}

#[test]
fn non_ascii_before_the_error_uses_the_agreed_encoding() {
    let text = "fn f() -> i64 { let s = \"😀\"; return nope; }\n";
    for offered in [true, false] {
        let dir = scratch("non-ascii");
        let file = dir.join("main.ingot");
        std::fs::write(&file, text).unwrap();
        let document = uri(&file);
        let mut client = Client::spawn(&[]);
        client.initialize(offered);
        client.open(&document, text);
        let diagnostics = client.published(&document);
        let undefined = diagnostic(&diagnostics, "E0304");
        let (_, start, _, end) = range(undefined);
        let expected = if offered {
            (39, 43)
        } else {
            // The emoji is 4 bytes but 2 UTF-16 units.
            (37, 41)
        };
        assert_eq!((start, end), expected, "offered utf-8: {offered}");
    }
}

#[test]
fn a_change_clears_and_close_restores_the_disk_text() {
    let dir = scratch("change");
    let file = dir.join("main.ingot");
    let broken = "fn f() -> i64 { return nope; }\n";
    std::fs::write(&file, broken).unwrap();
    let document = uri(&file);
    let mut client = Client::spawn(&[]);
    client.initialize(false);
    client.open(&document, broken);
    let diagnostics = client.published(&document);
    diagnostic(&diagnostics, "E0304");

    client.change(&document, "// nothing\n");
    assert!(client.published(&document).is_empty());

    client.close(&document);
    let diagnostics = client.published(&document);
    diagnostic(&diagnostics, "E0304");
}

#[test]
fn a_diagnostic_in_a_file_that_is_not_open_is_published_and_cleared() {
    let dir = package(
        "two-files",
        "name = \"app\"\n",
        &[
            ("main.ingot", "// nothing\n"),
            ("lib.ingot", "// nothing\n"),
            (
                "other.ingot",
                "import app.lib;\nfn g() -> i64 { return lib.width; }\n",
            ),
        ],
    );
    let lib = uri(&dir.join("lib.ingot"));
    let other = uri(&dir.join("other.ingot"));
    let mut client = Client::spawn(&[]);
    client.initialize(false);
    client.open(&lib, "// nothing\n");
    let diagnostics = client.published(&other);
    diagnostic(&diagnostics, "E0304");

    client.change(&lib, "pub const width: i64 = 2;\n");
    assert!(client.published(&other).is_empty());
}

#[test]
fn a_file_with_no_manifest_is_its_own_package() {
    let dir = scratch("no-manifest");
    let file = dir.join("solo.ingot");
    let text = "fn f() -> i64 { return nope; }\n";
    std::fs::write(&file, text).unwrap();
    let document = uri(&file);
    let mut client = Client::spawn(&[]);
    client.initialize(false);
    client.open(&document, text);
    let diagnostics = client.published(&document);
    diagnostic(&diagnostics, "E0304");
}

#[test]
fn a_file_under_std_is_checked_as_the_std_package() {
    let root = ingot_driver::loader::std_root().expect("std is in the source tree");
    let root = ingot_driver::files::normalize(&root);
    let file = root.join("mem.ingot");
    let text = format!(
        "{}\nfn broken() -> i64 {{ return true; }}\n",
        std::fs::read_to_string(&file).unwrap()
    );
    let document = uri(&file);
    let mut client = Client::spawn(&[]);
    client.initialize(false);
    client.open(&document, &text);
    let diagnostics = client.published(&document);
    diagnostic(&diagnostics, "E0401");
}

#[test]
fn editing_std_checks_the_other_open_packages_again() {
    let text = "import std.Env;\nfn f(env: &Env) { }\n";
    let dir = package("std-dependent", "name = \"app\"\n", &[("main.ingot", text)]);
    let user = uri(&dir.join("main.ingot"));
    let root = ingot_driver::loader::std_root().expect("std is in the source tree");
    let std_main = uri(&ingot_driver::files::normalize(&root).join("main.ingot"));
    let mut client = Client::spawn(&[]);
    client.initialize(false);
    client.open(&user, text);
    assert!(client.published(&user).is_empty());

    // `std/main.ingot` without its declarations: the user package's import breaks.
    client.open(&std_main, "// replaced\n");
    diagnostic(&client.published(&user), "E0301");

    client.close(&std_main);
    assert!(client.published(&user).is_empty());
}

#[test]
fn a_checked_body_has_no_not_implemented_log() {
    let dir = scratch("not-implemented");
    let file = dir.join("main.ingot");
    let text = "fn f() -> i64 { return 1; }\n";
    std::fs::write(&file, text).unwrap();
    let document = uri(&file);
    let mut client = Client::spawn(&[]);
    client.initialize(false);
    client.open(&document, text);
    assert!(client.published(&document).is_empty());
    assert!(!client.pending_log_contains("not implemented"));
}

#[test]
fn several_changes_in_one_batch_run_fewer_checks_than_changes() {
    let dir = scratch("batch");
    let file = dir.join("main.ingot");
    std::fs::write(&file, "// nothing\n").unwrap();
    let document = uri(&file);
    let mut client = Client::spawn(&[("INGOT_LSP_DEBUG_CHECKS", "1")]);
    client.initialize(false);
    client.open(&document, "// nothing\n");
    client.published(&document);
    // Clear the open's debug log and anything else that was pending.
    client.drain(Duration::from_millis(20));
    let changes: Vec<Json> = (0..5)
        .map(|index| {
            notification(
                "textDocument/didChange",
                obj(vec![
                    (
                        "textDocument",
                        obj(vec![("uri", s(&document)), ("version", n(index + 2))]),
                    ),
                    (
                        "contentChanges",
                        arr(vec![obj(vec![(
                            "text",
                            s(&format!("// change {index}\n")),
                        )])]),
                    ),
                ]),
            )
        })
        .collect();
    client.send_all(&changes);
    let messages = client.drain(quiet());
    let checks = checks_in(&messages);
    assert!(
        (1..5).contains(&checks),
        "{checks} checks for 5 changes: {messages:?}"
    );
}
