//! `ingot serve` (tools.md §6): identical lines to the one-shot commands, overlays,
//! request ids, error requests, cancel/shutdown, and the conformance suite in both
//! modes.

use ingot_diag::json::{self, Json};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

fn scratch(label: &str) -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "ingot-serve-{}-{label}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir.canonicalize().unwrap()
}

fn package(label: &str, manifest: &str, main: &str) -> PathBuf {
    let dir = scratch(label);
    fs::write(dir.join("ingot.toml"), manifest).unwrap();
    fs::write(dir.join("main.ingot"), main).unwrap();
    dir
}

fn serve(script: &str) -> Output {
    serve_in(script, None)
}

fn serve_in(script: &str, cwd: Option<&Path>) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ingot"));
    command.arg("serve");
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    stdin.write_all(script.as_bytes()).unwrap();
    drop(stdin);
    child.wait_with_output().unwrap()
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn json_string(text: &str) -> String {
    Json::String(text.to_string()).to_text()
}

/// A line with every `"ms":N` written `"ms":0` (tools.md §1 exempts timing fields).
fn zero_ms(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(at) = rest.find("\"ms\":") {
        out.push_str(&rest[..at + 5]);
        rest = &rest[at + 5..];
        let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
        out.push('0');
        rest = &rest[digits..];
    }
    out.push_str(rest);
    out
}

/// A serve line as the one-shot command would print it, checked byte by byte: the
/// leading `"id":ID,` and a summary's trailing `,"exit":N` are removed as text, and
/// nothing else may differ (tools.md §6).
fn as_one_shot(line: &str, id: i64) -> String {
    let prefix = format!("{{\"id\":{id},");
    let body = line
        .strip_prefix(&prefix)
        .unwrap_or_else(|| panic!("id {id} is not the first field: {line}"));
    let mut out = format!("{{{body}");
    if out.starts_with("{\"type\":\"summary\"") {
        let at = out
            .rfind(",\"exit\":")
            .unwrap_or_else(|| panic!("summary without exit: {line}"));
        assert!(out.ends_with('}'), "{line}");
        let digits = &out[at + 8..out.len() - 1];
        assert!(
            !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()),
            "`exit` is not the last field: {line}"
        );
        out.truncate(at);
        out.push('}');
    }
    zero_ms(&out)
}

/// One-shot lines and serve lines with `id`, `exit` and `ms` removed or zeroed.
fn normalize(line: &str) -> String {
    let mut value = json::parse(line).unwrap_or_else(|e| panic!("{line}: {e}"));
    if let Json::Object(members) = &mut value {
        members.retain(|(key, _)| key != "id" && key != "exit");
        for (key, value) in members.iter_mut() {
            if key == "ms" {
                *value = Json::Number("0".to_string());
            }
        }
    }
    value.to_text()
}

fn normalized(lines: &[String]) -> Vec<String> {
    lines.iter().map(|line| normalize(line)).collect()
}

fn summary_of(lines: &[String]) -> Json {
    let last = lines.last().unwrap_or_else(|| panic!("no lines"));
    let value = json::parse(last).unwrap();
    assert_eq!(
        value.get("type").and_then(Json::as_str),
        Some("summary"),
        "{last}"
    );
    value
}

fn exit_of(lines: &[String]) -> i64 {
    summary_of(lines)
        .get("exit")
        .and_then(Json::as_i64)
        .unwrap_or_else(|| panic!("summary has no exit: {:?}", lines.last()))
}

/// Compare one `check` request through serve with the same one-shot command.
fn check_equals_one_shot(label: &str, path: &Path, target: Option<&str>) {
    let mut one = Command::new(env!("CARGO_BIN_EXE_ingot"));
    one.args(["check", "--json"]);
    if let Some(target) = target {
        one.args(["--target", target]);
    }
    let one = one.arg(path).output().unwrap();
    let mut args = format!("{{\"path\":{}}}", json_string(&path.to_string_lossy()));
    if let Some(target) = target {
        args = format!(
            "{{\"path\":{},\"target\":{}}}",
            json_string(&path.to_string_lossy()),
            json_string(target)
        );
    }
    let script = format!("{{\"id\":1,\"command\":\"check\",\"args\":{args}}}\n");
    let served = serve(&script);
    assert_eq!(
        served.status.code(),
        Some(0),
        "{label}: {}",
        stderr(&served)
    );
    let served_lines: Vec<String> = stdout(&served).lines().map(String::from).collect();
    let one_lines: Vec<String> = stdout(&one).lines().map(String::from).collect();
    assert_eq!(normalized(&served_lines), normalized(&one_lines), "{label}");
    assert_eq!(
        exit_of(&served_lines),
        i64::from(one.status.code().unwrap()),
        "{label}"
    );
}

#[test]
fn check_through_serve_equals_the_one_shot_command() {
    let accepted = package(
        "accepted",
        "name = \"app\"\n",
        "const width: i64 = 2 + 2;\n",
    );
    check_equals_one_shot("accepted package", &accepted, None);

    let rejected = package(
        "rejected",
        "name = \"app\"\n",
        "fn f() -> i64 { return nope; }\n",
    );
    check_equals_one_shot("rejected package", &rejected, None);

    let single = scratch("single");
    let file = single.join("solo.ingot");
    fs::write(&file, "fn f() -> i64 { return true; }\n").unwrap();
    check_equals_one_shot("single file", &file, None);

    let inside = package(
        "inside",
        "name = \"app\"\n",
        "fn f() -> i64 { return 1; }\n",
    );
    let other = inside.join("other.ingot");
    fs::write(&other, "fn g() -> i64 { return true; }\n").unwrap();
    check_equals_one_shot("file inside a package", &other, None);

    let target = package("target", "name = \"app\"\n", "// nothing\n");
    check_equals_one_shot("unknown target", &target, Some("riscv64"));
}

#[test]
fn a_relative_path_is_relative_to_the_server_working_directory() {
    let dir = package(
        "relative",
        "name = \"app\"\n",
        "fn f() -> i64 { return 1; }\n",
    );
    fs::write(dir.join("other.ingot"), "fn g() -> i64 { return true; }\n").unwrap();
    for path in [".", "main.ingot", "other.ingot"] {
        let script = format!(
            "{{\"id\":1,\"command\":\"check\",\"args\":{{\"path\":{}}}}}\n",
            json_string(path)
        );
        let served = serve_in(&script, Some(&dir));
        assert_eq!(served.status.code(), Some(0), "{}", stderr(&served));
        let one = Command::new(env!("CARGO_BIN_EXE_ingot"))
            .current_dir(&dir)
            .args(["check", "--json", path])
            .output()
            .unwrap();
        let served_lines: Vec<String> = stdout(&served).lines().map(String::from).collect();
        let one_lines: Vec<String> = stdout(&one).lines().map(String::from).collect();
        assert_eq!(normalized(&served_lines), normalized(&one_lines), "{path}");
        assert_eq!(
            exit_of(&served_lines),
            i64::from(one.status.code().unwrap()),
            "{path}"
        );
    }
}

#[test]
fn an_overlay_applies_and_null_clears_it() {
    let dir = package(
        "overlay",
        "name = \"app\"\n",
        "fn f() -> i64 { return nope; }\n",
    );
    let file = dir.join("main.ingot");
    let script = format!(
        "{{\"id\":1,\"command\":\"check\",\"args\":{{\"path\":{p}}}}}\n\
         {{\"id\":\"o1\",\"command\":\"overlay\",\"args\":{{\"file\":{f},\"text\":\"// clean\\n\"}}}}\n\
         {{\"id\":2,\"command\":\"check\",\"args\":{{\"path\":{p}}}}}\n\
         {{\"id\":\"o2\",\"command\":\"overlay\",\"args\":{{\"file\":{f},\"text\":null}}}}\n\
         {{\"id\":3,\"command\":\"check\",\"args\":{{\"path\":{p}}}}}\n",
        p = json_string(&dir.to_string_lossy()),
        f = json_string(&file.to_string_lossy()),
    );
    let served = serve(&script);
    assert_eq!(served.status.code(), Some(0), "{}", stderr(&served));
    let out = stdout(&served);

    let checks: Vec<String> = out
        .lines()
        .filter(|line| line.contains("\"type\":\"summary\",\"command\":\"check\""))
        .map(String::from)
        .collect();
    assert_eq!(checks.len(), 3, "{out}");
    assert!(checks[0].contains("\"errors\":1"), "{out}");
    assert!(checks[1].contains("\"errors\":0"), "{out}");
    assert!(checks[2].contains("\"errors\":1"), "{out}");
    assert!(checks[0].contains("\"ok\":false"), "{out}");
    assert!(checks[1].contains("\"ok\":true"), "{out}");

    let overlays: Vec<String> = out
        .lines()
        .filter(|line| line.contains("\"command\":\"overlay\""))
        .map(String::from)
        .collect();
    assert_eq!(overlays.len(), 2, "{out}");
    for line in overlays {
        assert!(line.contains("\"ok\":true"), "{line}");
        assert!(line.contains("\"exit\":0"), "{line}");
    }
}

#[test]
fn an_overlay_creates_a_module_that_is_not_on_disk() {
    let dir = package("overlay-file", "name = \"app\"\n", "// nothing\n");
    let extra = dir.join("extra.ingot");
    let script = format!(
        "{{\"id\":1,\"command\":\"check\",\"args\":{{\"path\":{p}}}}}\n\
         {{\"id\":2,\"command\":\"overlay\",\"args\":{{\"file\":{f},\"text\":\"fn broken() -> i64 {{ return true; }}\\n\"}}}}\n\
         {{\"id\":3,\"command\":\"check\",\"args\":{{\"path\":{p}}}}}\n\
         {{\"id\":4,\"command\":\"overlay\",\"args\":{{\"file\":{f},\"text\":null}}}}\n\
         {{\"id\":5,\"command\":\"check\",\"args\":{{\"path\":{p}}}}}\n",
        p = json_string(&dir.to_string_lossy()),
        f = json_string(&extra.to_string_lossy()),
    );
    let served = serve(&script);
    assert_eq!(served.status.code(), Some(0), "{}", stderr(&served));
    let out = stdout(&served);
    let reports: Vec<&str> = out
        .lines()
        .filter(|line| line.contains("\"type\":\"diagnostic\""))
        .collect();
    assert_eq!(reports.len(), 1, "{out}");
    assert!(reports[0].contains("\"file\":\"extra.ingot\""), "{out}");
    let checks: Vec<&str> = out
        .lines()
        .filter(|line| line.contains("\"type\":\"summary\",\"command\":\"check\""))
        .collect();
    assert_eq!(checks.len(), 3, "{out}");
    assert!(checks[0].contains("\"errors\":0"), "{out}");
    assert!(checks[1].contains("\"errors\":1"), "{out}");
    assert!(checks[2].contains("\"errors\":0"), "{out}");
}

#[test]
fn request_ids_are_written_back_as_given() {
    let script = "\
{\"id\":1,\"command\":\"overlay\",\"args\":{\"file\":\"a\",\"text\":\"x\"}}\n\
{\"id\":-2,\"command\":\"overlay\",\"args\":{\"file\":\"a\",\"text\":\"x\"}}\n\
{\"id\":1.5,\"command\":\"overlay\",\"args\":{\"file\":\"a\",\"text\":\"x\"}}\n\
{\"id\":\"a\\\"b\",\"command\":\"overlay\",\"args\":{\"file\":\"a\",\"text\":\"x\"}}\n\
{\"command\":\"overlay\",\"args\":{\"file\":\"a\",\"text\":\"x\"}}\n";
    let served = serve(script);
    assert_eq!(served.status.code(), Some(0), "{}", stderr(&served));
    let out = stdout(&served);
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 5, "{out}");
    assert!(lines[0].starts_with("{\"id\":1,"), "{out}");
    assert!(lines[1].starts_with("{\"id\":-2,"), "{out}");
    assert!(lines[2].starts_with("{\"id\":1.5,"), "{out}");
    assert!(lines[3].starts_with("{\"id\":\"a\\\"b\","), "{out}");
    assert!(lines[4].starts_with("{\"id\":null,"), "{out}");
}

#[test]
fn requests_that_cannot_be_read_are_errors() {
    let dir = package("bad", "name = \"app\"\n", "// nothing\n");
    let script = format!(
        "not json\n\
         [1,2]\n\
         {{\"id\":3}}\n\
         {{\"id\":4,\"command\":\"bogus\"}}\n\
         {{\"id\":5,\"command\":\"check\",\"args\":{{\"bogus\":1}}}}\n\
         {{\"id\":6,\"command\":\"fix\"}}\n\
         \n\
         {{\"id\":7,\"command\":\"check\",\"args\":{{\"path\":{p}}}}}\n",
        p = json_string(&dir.to_string_lossy()),
    );
    let served = serve(&script);
    assert_eq!(served.status.code(), Some(0), "{}", stderr(&served));
    let out = stdout(&served);
    let lines: Vec<&str> = out.lines().collect();
    // Seven bad requests, two lines each, then the accepted check: one summary line.
    assert_eq!(lines.len(), 15, "{out}");
    let expected = [("null", ""), ("null", ""), ("3", ""), ("4", "bogus")];
    for (i, (id, command)) in expected.iter().enumerate() {
        let error = json::parse(lines[2 * i]).unwrap();
        assert_eq!(
            error.get("type").and_then(Json::as_str),
            Some("error"),
            "{out}"
        );
        let summary = json::parse(lines[2 * i + 1]).unwrap();
        assert_eq!(
            summary.get("id").map(Json::to_text).unwrap_or_default(),
            id.to_string(),
            "{out}"
        );
        assert_eq!(
            summary.get("command").and_then(Json::as_str),
            Some(*command),
            "{out}"
        );
        assert_eq!(summary.get("ok").and_then(Json::as_bool), Some(false));
        assert_eq!(summary.get("exit").and_then(Json::as_i64), Some(2));
    }
    // id 5: an unknown argument; id 6: `fix` is not implemented in serve.
    for (i, command) in [(4, "check"), (5, "fix"), (6, "")] {
        let summary = json::parse(lines[2 * i + 1]).unwrap();
        assert_eq!(
            summary.get("command").and_then(Json::as_str),
            Some(command),
            "{out}"
        );
        assert_eq!(summary.get("exit").and_then(Json::as_i64), Some(2));
    }
    // The real check after them still runs.
    let last: Vec<String> = lines[14..].iter().map(|l| l.to_string()).collect();
    let summary = summary_of(&last);
    assert_eq!(summary.get("command").and_then(Json::as_str), Some("check"));
    assert_eq!(summary.get("id").and_then(Json::as_i64), Some(7));
}

#[test]
fn cancel_answers_its_own_summary_and_the_server_goes_on() {
    let dir = package(
        "cancel",
        "name = \"app\"\n",
        "fn f() -> i64 { return 1; }\n",
    );
    let path = json_string(&dir.to_string_lossy());
    let script = format!(
        "{{\"id\":1,\"command\":\"check\",\"args\":{{\"path\":{path}}}}}\n\
         {{\"id\":2,\"command\":\"cancel\",\"args\":{{\"target\":1}}}}\n\
         {{\"id\":3,\"command\":\"check\",\"args\":{{\"path\":{path}}}}}\n"
    );
    let served = serve(&script);
    assert_eq!(served.status.code(), Some(0), "{}", stderr(&served));
    let out = stdout(&served);
    assert!(!out.contains("canceled"), "{out}");
    assert!(out.contains("\"id\":2,\"type\":\"summary\",\"command\":\"cancel\",\"ok\":true,\"protocol\":1,\"exit\":0,"), "{out}");
    let checks: Vec<String> = out
        .lines()
        .filter(|line| line.contains("\"command\":\"check\""))
        .map(normalize)
        .collect();
    assert_eq!(checks.len(), 2, "{out}");
    assert_eq!(checks[0], checks[1], "no state leaked through the cancel");
}

#[test]
fn shutdown_answers_then_the_process_exits_zero() {
    let dir = package("shutdown", "name = \"app\"\n", "// nothing\n");
    let path = json_string(&dir.to_string_lossy());
    let script = format!(
        "{{\"id\":1,\"command\":\"shutdown\"}}\n\
         {{\"id\":2,\"command\":\"check\",\"args\":{{\"path\":{path}}}}}\n"
    );
    let served = serve(&script);
    assert_eq!(served.status.code(), Some(0), "{}", stderr(&served));
    let out = stdout(&served);
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 1, "{out}");
    assert!(lines[0].contains("\"command\":\"shutdown\""), "{out}");
    assert!(lines[0].contains("\"exit\":0"), "{out}");
}

#[test]
fn the_end_of_input_is_a_shutdown() {
    let dir = package("eof", "name = \"app\"\n", "// nothing\n");
    let path = json_string(&dir.to_string_lossy());
    let script = format!("{{\"id\":1,\"command\":\"check\",\"args\":{{\"path\":{path}}}}}\n");
    let served = serve(&script);
    assert_eq!(served.status.code(), Some(0), "{}", stderr(&served));
    let out = stdout(&served);
    let lines: Vec<String> = out.lines().map(String::from).collect();
    assert_eq!(
        summary_of(&lines).get("exit").and_then(Json::as_i64),
        Some(0)
    );
}

#[test]
fn the_conformance_suite_is_the_same_in_both_modes() {
    let started = Instant::now();
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let conformance = root.join("conformance");
    let mut entries = Vec::new();
    let mut categories: Vec<PathBuf> = fs::read_dir(&conformance)
        .unwrap()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect();
    categories.sort();
    for category in categories {
        let mut found: Vec<PathBuf> = fs::read_dir(&category)
            .unwrap()
            .flatten()
            .map(|entry| entry.path())
            .collect();
        found.sort();
        for path in found {
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let is_ingot =
                path.extension().is_some_and(|x| x == "ingot") && !name.ends_with(".fixed.ingot");
            if is_ingot || path.is_dir() {
                entries.push(path);
            }
        }
    }
    assert!(!entries.is_empty());

    let mut expected = Vec::new();
    for entry in &entries {
        let output = Command::new(env!("CARGO_BIN_EXE_ingot"))
            .args(["check", "--json"])
            .arg(entry)
            .output()
            .unwrap();
        let lines: Vec<String> = stdout(&output).lines().map(String::from).collect();
        expected.push((lines, i64::from(output.status.code().unwrap_or(2))));
    }

    let mut script = String::new();
    for (i, entry) in entries.iter().enumerate() {
        let path = json_string(&entry.to_string_lossy());
        script.push_str(&format!(
            "{{\"id\":{},\"command\":\"check\",\"args\":{{\"path\":{path}}}}}\n",
            i + 1
        ));
    }
    let served = serve(&script);
    assert_eq!(served.status.code(), Some(0), "{}", stderr(&served));
    let lines: Vec<String> = stdout(&served).lines().map(String::from).collect();
    let mut index = 0;
    for (i, (one_shot, status)) in expected.iter().enumerate() {
        let id = (i + 1) as i64;
        let mut block = Vec::new();
        while index < lines.len() {
            let value = json::parse(&lines[index]).unwrap();
            if value.get("id").and_then(Json::as_i64) != Some(id) {
                break;
            }
            let is_summary = value.get("type").and_then(Json::as_str) == Some("summary");
            block.push(lines[index].clone());
            index += 1;
            if is_summary {
                break;
            }
        }
        let entry = &entries[i];
        assert!(!block.is_empty(), "no response for {}", entry.display());
        let summary = summary_of(&block);
        assert_eq!(
            summary.get("id").and_then(Json::as_i64),
            Some(id),
            "{}",
            entry.display()
        );
        assert_eq!(exit_of(&block), *status, "{}", entry.display());
        let served: Vec<String> = block.iter().map(|l| as_one_shot(l, id)).collect();
        let one_shot: Vec<String> = one_shot.iter().map(|l| zero_ms(l)).collect();
        assert_eq!(served, one_shot, "{}", entry.display());
    }
    assert_eq!(index, lines.len(), "lines after the last response");
    eprintln!(
        "conformance in both modes: {:.2?} for {} entries ({} serve requests)",
        started.elapsed(),
        entries.len(),
        entries.len()
    );
}

fn obj(members: Vec<(&str, Json)>) -> Json {
    Json::Object(
        members
            .into_iter()
            .map(|(key, value)| (key.to_string(), value))
            .collect(),
    )
}

fn s(value: &str) -> Json {
    Json::String(value.to_string())
}

fn query_at(path: &Path, needle: &str) -> String {
    let text = fs::read_to_string(path).unwrap();
    let offset = text.find(needle).unwrap();
    let line = text[..offset].matches('\n').count() + 1;
    let start = text[..offset].rfind('\n').map(|i| i + 1).unwrap_or(0);
    format!("{}:{}:{}", path.display(), line, offset - start + 1)
}

fn one_shot_query(args: &[&str]) -> (Vec<String>, i32) {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ingot"));
    command.arg("query");
    command.args(args);
    command.arg("--json");
    let output = command.output().unwrap();
    (
        stdout(&output).lines().map(String::from).collect(),
        output.status.code().unwrap(),
    )
}

#[test]
fn query_through_serve_equals_the_one_shot_command() {
    let dir = package(
        "query",
        "name = \"app\"\n",
        "import app.lib;\n\nfn f(t: lib.Ticket) -> i64 {\n    return t.seat;\n}\n",
    );
    fs::write(
        dir.join("lib.ingot"),
        "pub struct Ticket { pub seat: i64 }\n",
    )
    .unwrap();
    let main = dir.join("main.ingot");

    let def_at = query_at(&main, "seat");
    let type_at = query_at(&main, "t: lib.Ticket");
    let dir_string = dir.to_string_lossy().into_owned();
    let cases: Vec<(Vec<&str>, Json)> = vec![
        (
            vec!["def", &def_at],
            obj(vec![("kind", s("def")), ("at", s(&def_at))]),
        ),
        (
            vec!["type", &type_at],
            obj(vec![("kind", s("type")), ("at", s(&type_at))]),
        ),
        (
            vec!["symbols", &dir_string],
            obj(vec![("kind", s("symbols")), ("path", s(&dir_string))]),
        ),
        (
            vec!["symbols", &dir_string, "--match", "Ticket"],
            obj(vec![
                ("kind", s("symbols")),
                ("path", s(&dir_string)),
                ("match", s("Ticket")),
            ]),
        ),
        (
            vec!["def", "--name", "app.lib.Ticket"],
            obj(vec![("kind", s("def")), ("name", s("app.lib.Ticket"))]),
        ),
    ];
    for (args, request_args) in cases {
        let (one_shot, status) = one_shot_query(&args);
        let script = format!(
            "{{\"id\":1,\"command\":\"query\",\"args\":{}}}\n",
            request_args.to_text()
        );
        let served = serve(&script);
        assert_eq!(served.status.code(), Some(0), "{}", stderr(&served));
        let lines: Vec<String> = stdout(&served).lines().map(String::from).collect();
        assert!(!lines.is_empty(), "{args:?}");
        assert_eq!(
            summary_of(&lines).get("type").and_then(Json::as_str),
            Some("summary")
        );
        assert_eq!(exit_of(&lines), i64::from(status), "{args:?}");
        let served: Vec<String> = lines.iter().map(|line| as_one_shot(line, 1)).collect();
        let one_shot: Vec<String> = one_shot.iter().map(|line| zero_ms(line)).collect();
        assert_eq!(served, one_shot, "{args:?}");
    }
}
