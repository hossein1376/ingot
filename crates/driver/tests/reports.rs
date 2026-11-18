//! P4b: the report tools. Goldens are in `tests/reports/`; `"ms"` is normalized.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn workspace() -> PathBuf {
    root().join("../..")
}

fn fixture(name: &str) -> PathBuf {
    root().join("tests/reports").join(name)
}

fn ingot(args: &[&str], cwd: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_ingot"))
        .args(args)
        .current_dir(cwd)
        .output()
        .unwrap()
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn normalize(text: &str) -> String {
    let mut out = String::new();
    for line in text.lines() {
        if !line.starts_with('{') {
            out.push_str(line);
            out.push('\n');
            continue;
        }
        let mut rest = line;
        while let Some(at) = rest.find("\"ms\":") {
            out.push_str(&rest[..at + 5]);
            rest = &rest[at + 5..];
            let digits = rest
                .find(|character: char| !character.is_ascii_digit())
                .unwrap_or(rest.len());
            out.push('0');
            rest = &rest[digits..];
        }
        out.push_str(rest);
        out.push('\n');
    }
    out
}

fn golden(name: &str, output: &Output) {
    assert!(
        output.status.success(),
        "{name}: status {:?}: {}",
        output.status.code(),
        stdout(output)
    );
    let actual = normalize(&stdout(output));
    let path = fixture(name);
    if std::env::var_os("INGOT_UPDATE_GOLDENS").is_some() {
        fs::write(&path, &actual).unwrap();
        return;
    }
    let expected =
        fs::read_to_string(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    assert_eq!(actual, expected, "{name}");
}

fn json_lines(output: &Output) -> Vec<ingot_diag::json::Json> {
    stdout(output)
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| ingot_diag::json::parse(line).expect("a JSON line"))
        .collect()
}

fn string(value: &ingot_diag::json::Json, key: &str) -> String {
    value
        .get(key)
        .and_then(|value| value.as_str())
        .unwrap_or_else(|| panic!("`{key}` is a string in {value:?}"))
        .to_string()
}

fn kind_of(value: &ingot_diag::json::Json) -> String {
    string(value, "type")
}

#[test]
fn explain_shows_the_catalog_row_and_the_test() {
    for (code, name) in [
        ("E1003", "explain_E1003.jsonl"),
        ("E0101", "explain_E0101.jsonl"),
        ("E0312", "explain_E0312.jsonl"),
        ("E0512", "explain_E0512.jsonl"),
        ("T01", "explain_T01.jsonl"),
        ("T03", "explain_T03.jsonl"),
    ] {
        let output = ingot(&["explain", code, "--json"], &workspace());
        golden(name, &output);
    }
}

#[test]
fn explain_keeps_operators_inside_code_spans() {
    let output = ingot(&["explain", "T03", "--json"], &workspace());
    let line = &json_lines(&output)[0];
    assert!(
        string(line, "condition").contains('*'),
        "{}",
        stdout(&output)
    );
}

#[test]
fn explain_an_unknown_code_is_a_failure() {
    let output = ingot(&["explain", "E9999", "--json"], &workspace());
    assert_eq!(output.status.code(), Some(2));
    let lines = json_lines(&output);
    assert_eq!(kind_of(&lines[0]), "error");
    assert_eq!(kind_of(&lines[1]), "summary");
    assert_eq!(lines[1].get("ok").and_then(|v| v.as_bool()), Some(false));
}

#[test]
fn explain_all_is_catalog_order() {
    let output = ingot(&["explain", "--all", "--json"], &workspace());
    assert!(output.status.success());
    let lines = json_lines(&output);
    assert!(lines.len() > 150);
    assert_eq!(kind_of(&lines[lines.len() - 1]), "summary");
    let codes: Vec<String> = lines[..lines.len() - 1]
        .iter()
        .map(|line| string(line, "code"))
        .collect();
    assert_eq!(codes.first().map(String::as_str), Some("E0101"));
    assert_eq!(codes.last().map(String::as_str), Some("T17"));
    let mut sorted = codes.clone();
    sorted.dedup();
    assert_eq!(sorted.len(), codes.len(), "a code is printed twice");
}

#[test]
fn api_prints_every_item_kind() {
    let output = ingot(
        &["api", "--json", fixture("api_app").to_str().unwrap()],
        &workspace(),
    );
    golden("api_package.jsonl", &output);
    let lines = json_lines(&output);
    let paths: Vec<String> = lines
        .iter()
        .filter(|line| kind_of(line) == "api")
        .map(|line| string(line, "path"))
        .collect();
    assert!(paths.contains(&"api_app.Point".to_string()));
    assert!(paths.contains(&"api_app.Draw.draw".to_string()));
    assert!(paths.contains(&"api_app.Square".to_string()));
    assert!(!paths.iter().any(|path| path.contains("linux_only")));
    assert!(paths.iter().any(|path| path == "api_app.mac_only"));
}

#[test]
fn api_an_item_prints_its_subtree() {
    let output = ingot(&["api", "api_app.Point", "--json"], &fixture("api_app"));
    golden("api_point.jsonl", &output);
}

#[test]
fn api_pkg_adds_the_package_items() {
    let output = ingot(&["api", "--pkg", "--json"], &fixture("api_app"));
    golden("api_package_pkg.jsonl", &output);
    let text = normalize(&stdout(&output));
    assert!(text.contains("\"pub(pkg)\""));
    assert!(text.contains("api_app.package_only"));
}

#[test]
fn api_std_prints_modules_and_items() {
    let output = ingot(&["api", "std.mem", "--json"], &workspace());
    assert!(output.status.success());
    let lines = json_lines(&output);
    assert_eq!(kind_of(&lines[lines.len() - 1]), "summary");
    let items: Vec<_> = lines.iter().filter(|line| kind_of(line) == "api").collect();
    assert!(!items.is_empty());
    assert!(
        items
            .iter()
            .all(|line| string(line, "path").starts_with("std.mem"))
    );
    assert!(
        items
            .iter()
            .any(|line| string(line, "path") == "std.mem.Allocator")
    );
    assert!(items.iter().any(|line| string(line, "item") == "interface"));
}

#[test]
fn api_an_unresolved_dotted_path_is_a_failure() {
    let output = ingot(&["api", "no_such.Item", "--json"], &fixture("api_app"));
    assert_eq!(output.status.code(), Some(2));
    let lines = json_lines(&output);
    assert_eq!(kind_of(&lines[0]), "error");
}

#[test]
fn api_a_private_item_is_a_failure() {
    let output = ingot(&["api", "api_app.puts", "--json"], &fixture("api_app"));
    assert_eq!(output.status.code(), Some(2));
}

#[test]
fn audit_lists_the_build_without_std() {
    let output = ingot(&["audit", "--json"], &fixture("audit_app"));
    golden("audit_app.jsonl", &output);
    let text = normalize(&stdout(&output));
    assert!(text.contains("\"kind\":\"unsafe_block\""));
    assert!(text.contains("\"kind\":\"unsafe_fn\""));
    assert!(text.contains("\"kind\":\"extern\""));
    assert!(text.contains("\"kind\":\"export\""));
    assert!(text.contains("\"kind\":\"forget\""));
    assert!(text.contains("\"kind\":\"debug\""));
    assert!(text.contains("\"kind\":\"unsafe_impl\""));
    assert!(text.contains("dep/main.ingot"));
    assert!(!text.contains("linux_only"));
    assert_eq!(output.status.code(), Some(0));
}

#[test]
fn audit_lists_a_static_with_interior_mutability() {
    let output = ingot(&["audit", "--json"], &fixture("audit_static"));
    assert_eq!(output.status.code(), Some(1));
    let lines = json_lines(&output);
    let text = normalize(&stdout(&output));
    assert!(text.contains("\"kind\":\"static_interior\""));
    assert!(text.contains("\"value_type\":\"Cell[i64]\""));
    assert!(lines.iter().any(|line| kind_of(line) == "diagnostic"));
    assert_eq!(
        lines
            .last()
            .unwrap()
            .get("ok")
            .and_then(|value| value.as_bool()),
        Some(false)
    );
}

#[test]
fn layout_shows_fields_and_padding() {
    for (ty, name) in [
        ("layout_app.Mixed", "layout_mixed.jsonl"),
        ("layout_app.Event", "layout_event.jsonl"),
        ("?&layout_app.Mixed", "layout_optional.jsonl"),
    ] {
        let output = ingot(&["layout", ty, "--json"], &fixture("layout_app"));
        golden(name, &output);
    }
}

#[test]
fn layout_a_generic_needs_its_arguments() {
    let output = ingot(
        &["layout", "api_app.Point[i64]", "--json"],
        &fixture("api_app"),
    );
    golden("layout_generic.jsonl", &output);
    let unresolved = ingot(&["layout", "api_app.Point", "--json"], &fixture("api_app"));
    assert_eq!(unresolved.status.code(), Some(1));
    assert_eq!(kind_of(&json_lines(&unresolved)[0]), "diagnostic");
}

#[test]
fn layout_expands_aliases_and_builtin_shapes() {
    for (ty, size, align, cwd) in [
        ("api_app.Alias", 0, 1, fixture("api_app")),
        ("api_app.GenericAlias[i64]", 24, 8, fixture("api_app")),
        ("[4]u32", 16, 4, fixture("api_app")),
        ("(u8, i64)", 16, 8, fixture("api_app")),
    ] {
        let output = ingot(&["layout", ty, "--json"], &cwd);
        assert!(output.status.success(), "{ty}: {}", stdout(&output));
        let line = &json_lines(&output)[0];
        assert_eq!(
            line.get("size").and_then(|v| v.as_i64()),
            Some(size),
            "{ty}"
        );
        assert_eq!(
            line.get("align").and_then(|v| v.as_i64()),
            Some(align),
            "{ty}"
        );
    }
}

#[test]
fn layout_an_unresolved_name_is_a_diagnostic() {
    let output = ingot(
        &["layout", "layout_app.Nope", "--json"],
        &fixture("layout_app"),
    );
    assert_eq!(output.status.code(), Some(1));
    let lines = json_lines(&output);
    assert_eq!(kind_of(&lines[0]), "diagnostic");
    assert_eq!(string(&lines[0], "code"), "E0305");
    assert_eq!(kind_of(&lines[1]), "summary");
}

#[test]
fn reports_print_check_diagnostics_first() {
    let api = ingot(&["api", "--json"], &fixture("diag_app"));
    assert_eq!(api.status.code(), Some(1));
    let lines = json_lines(&api);
    assert!(lines.iter().any(|line| kind_of(line) == "diagnostic"));
    assert!(
        lines
            .iter()
            .any(|line| { kind_of(line) == "api" && string(line, "path") == "diag_app.Good" })
    );
    let summary = lines.last().unwrap();
    assert_eq!(
        summary.get("ok").and_then(|value| value.as_bool()),
        Some(false)
    );

    let audit = ingot(&["audit", "--json"], &fixture("diag_app"));
    assert_eq!(audit.status.code(), Some(1));
    let lines = json_lines(&audit);
    assert!(lines.iter().any(|line| kind_of(line) == "diagnostic"));
    assert!(lines.iter().any(|line| kind_of(line) == "audit"));
    assert_eq!(
        lines
            .last()
            .unwrap()
            .get("ok")
            .and_then(|value| value.as_bool()),
        Some(false)
    );

    let layout = ingot(&["layout", "diag_app.Good", "--json"], &fixture("diag_app"));
    assert_eq!(layout.status.code(), Some(1));
    let lines = json_lines(&layout);
    assert!(lines.iter().any(|line| kind_of(line) == "diagnostic"));
    assert!(lines.iter().any(|line| kind_of(line) == "layout"));
    assert_eq!(
        lines
            .last()
            .unwrap()
            .get("ok")
            .and_then(|value| value.as_bool()),
        Some(false)
    );
}

#[test]
fn targets_has_three_lines_and_one_host() {
    let output = ingot(&["targets", "--json"], &workspace());
    assert!(output.status.success());
    let lines = json_lines(&output);
    let targets: Vec<_> = lines
        .iter()
        .filter(|line| kind_of(line) == "target")
        .collect();
    assert_eq!(targets.len(), 3);
    let hosts = targets
        .iter()
        .filter(|line| line.get("host").and_then(|value| value.as_bool()) == Some(true))
        .count();
    assert_eq!(hosts, 1);
    for target in &targets {
        let name = string(target, "name");
        let build = target
            .get("build")
            .and_then(|value| value.as_bool())
            .unwrap();
        let missing: Vec<String> = target
            .get("missing")
            .and_then(|value| match value {
                ingot_diag::json::Json::Array(items) => Some(
                    items
                        .iter()
                        .filter_map(|item| item.as_str().map(str::to_string))
                        .collect(),
                ),
                _ => None,
            })
            .unwrap();
        if name != "aarch64-macos-m4" {
            assert!(!build);
            assert!(missing.contains(&"code generation for this target".to_string()));
        }
    }
    assert_eq!(kind_of(&lines[lines.len() - 1]), "summary");
}
