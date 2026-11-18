//! P4a: `ingot deps`, fetch and update against a local repository, workspaces and
//! `check --all-targets`.

use ingot_driver::sha256;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

fn scratch(label: &str) -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "ingot-packages-{}-{label}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn package(dir: &Path, name: &str, deps: &str, main: &str) {
    fs::create_dir_all(dir).unwrap();
    fs::write(dir.join("ingot.toml"), format!("name = \"{name}\"\n{deps}")).unwrap();
    fs::write(dir.join("main.ingot"), main).unwrap();
}

fn ingot(args: &[&str], dir: &Path, cache: Option<&Path>) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ingot"));
    command.args(args).current_dir(dir);
    if let Some(cache) = cache {
        command.env("INGOT_CACHE", cache);
    }
    command.output().unwrap()
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn json_lines(output: &Output) -> Vec<ingot_diag::json::Json> {
    stdout(output)
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| ingot_diag::json::parse(line).expect("a JSON line"))
        .collect()
}

fn file_of(value: &ingot_diag::json::Json) -> String {
    let span = value.get("span").expect("a span");
    string(span, "file")
}

fn string(value: &ingot_diag::json::Json, key: &str) -> String {
    value
        .get(key)
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| panic!("`{key}` is a string in {value:?}"))
        .to_string()
}

fn git(repo: &Path, args: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["-c", "commit.gpgsign=false"])
        .args(args)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.com")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.com")
        .env("GIT_AUTHOR_DATE", "2020-01-01T00:00:00Z")
        .env("GIT_COMMITTER_DATE", "2020-01-01T00:00:00Z")
        .output()
        .expect("git runs");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn repository(dir: &Path, name: &str) -> String {
    fs::create_dir_all(dir).unwrap();
    fs::write(dir.join("ingot.toml"), format!("name = \"{name}\"\n")).unwrap();
    fs::write(
        dir.join("main.ingot"),
        "pub fn triple(x: i64) -> i64 {\n    return x * 3;\n}\n",
    )
    .unwrap();
    git(dir, &["init", "-q"]);
    git(dir, &["checkout", "-q", "-b", "main"]);
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "one"]);
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["rev-parse", "HEAD"])
        .output()
        .unwrap();
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

#[test]
fn fetch_fills_a_two_level_git_chain_in_one_run() {
    let dir = scratch("deps-two-level");
    let leaf = dir.join("leaf");
    let mid = dir.join("mid");
    let app = dir.join("app");
    let cache = dir.join("cache");
    let leaf_rev = repository(&leaf, "leaf");
    let leaf_hash = sha256::hash_tree(&leaf, &[]).unwrap();
    package(
        &mid,
        "mid",
        &format!(
            "[deps]\nleaf = {{ git = \"file://{}\", rev = \"{leaf_rev}\", hash = \"{leaf_hash}\" }}\n",
            leaf.display()
        ),
        "import leaf;\npub fn mid() -> i64 { return leaf.triple(2); }\n",
    );
    git(&mid, &["init", "-q"]);
    git(&mid, &["checkout", "-q", "-b", "main"]);
    git(&mid, &["add", "-A"]);
    git(&mid, &["commit", "-q", "-m", "one"]);
    let mid_rev = Command::new("git")
        .arg("-C")
        .arg(&mid)
        .args(["rev-parse", "HEAD"])
        .output()
        .unwrap();
    let mid_rev = String::from_utf8_lossy(&mid_rev.stdout).trim().to_string();
    let mid_hash = sha256::hash_tree(&mid, &[]).unwrap();
    package(
        &app,
        "app",
        &format!(
            "[deps]\nmid = {{ git = \"file://{}\", rev = \"{mid_rev}\", hash = \"{mid_hash}\" }}\n",
            mid.display()
        ),
        "import mid;\nfn f() -> i64 { return mid.mid(); }\n",
    );

    let fetch = ingot(&["deps", "fetch", "--json"], &app, Some(&cache));
    assert_eq!(fetch.status.code(), Some(0), "{}", stdout(&fetch));
    assert!(!stdout(&fetch).contains("E1712"), "{}", stdout(&fetch));
    let check = ingot(&["check", "--json"], &app, Some(&cache));
    assert_eq!(check.status.code(), Some(0), "{}", stdout(&check));
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn fetch_reports_a_pin_mismatch_even_when_the_true_content_is_cached() {
    let dir = scratch("deps-cached-mismatch");
    let repo = dir.join("repo");
    let app = dir.join("app");
    let cache = dir.join("cache");
    let rev = repository(&repo, "lib");
    let hash = sha256::hash_tree(&repo, &[]).unwrap();
    let hex = hash.strip_prefix("sha256:").unwrap();
    let cached = cache.join(hex);
    fs::create_dir_all(&cached).unwrap();
    for file in sha256::tree_files(&repo, &[]).unwrap() {
        let path = cached.join(&file.path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, file.bytes).unwrap();
    }
    package(
        &app,
        "app",
        &format!(
            "[deps]\nlib = {{ git = \"file://{}\", rev = \"{rev}\", hash = \"sha256:{}\" }}\n",
            repo.display(),
            "0".repeat(64)
        ),
        "import lib;\nfn f() -> i64 { return lib.triple(2); }\n",
    );
    let fetch = ingot(&["deps", "fetch", "--json"], &app, Some(&cache));
    assert_eq!(fetch.status.code(), Some(1), "{}", stdout(&fetch));
    let lines = json_lines(&fetch);
    assert!(
        lines.iter().any(|line| string(line, "code") == "E1703"),
        "{}",
        stdout(&fetch)
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn update_writes_the_fetched_commit_for_a_branch() {
    let dir = scratch("deps-branch-update");
    let repo = dir.join("repo");
    let app = dir.join("app");
    let cache = dir.join("cache");
    let rev = repository(&repo, "lib");
    let hash = sha256::hash_tree(&repo, &[]).unwrap();
    package(
        &app,
        "app",
        &format!(
            "[deps]\nlib = {{ git = \"file://{}\", rev = \"main\", hash = \"sha256:{}\" }}\n",
            repo.display(),
            "0".repeat(64)
        ),
        "import lib;\nfn f() -> i64 { return lib.triple(2); }\n",
    );
    let update = ingot(
        &["deps", "update", "lib", "--rev", "main", "--json"],
        &app,
        Some(&cache),
    );
    assert_eq!(update.status.code(), Some(0), "{}", stdout(&update));
    let lines = json_lines(&update);
    assert_eq!(string(&lines[0], "rev"), rev);
    assert_eq!(string(&lines[0], "hash"), hash);
    let manifest = fs::read_to_string(app.join("ingot.toml")).unwrap();
    assert!(manifest.contains(&format!("rev = \"{rev}\"")), "{manifest}");
    assert!(manifest.contains(&hash), "{manifest}");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn deps_lists_the_graph_in_order_with_via_and_override() {
    let dir = scratch("deps-list");
    let app = dir.join("app");
    let csv = dir.join("csv");
    let zstd = dir.join("zstd");
    let zstd2 = dir.join("zstd2");
    package(
        &app,
        "app",
        "[deps]\ncsv = { path = \"../csv\" }\n\n[override]\nzstd = { path = \"../zstd2\" }\n",
        "fn f() { }\n",
    );
    package(
        &csv,
        "csv",
        "[deps]\nzstd = { path = \"../zstd\" }\n",
        "pub fn f() { }\n",
    );
    package(&zstd, "zstd", "", "pub fn f() { }\n");
    package(&zstd2, "zstd", "", "pub fn f() { }\n");
    let output = ingot(&["deps", "--json"], &app, None);
    assert_eq!(output.status.code(), Some(0), "{}", stdout(&output));
    let lines = json_lines(&output);
    assert_eq!(lines.len(), 3, "{}", stdout(&output));
    assert_eq!(string(&lines[0], "type"), "dep");
    assert_eq!(string(&lines[0], "name"), "csv");
    assert_eq!(string(&lines[0], "path"), "../csv");
    assert_eq!(
        lines[0].get("via"),
        Some(&ingot_diag::json::Json::Array(vec![
            ingot_diag::json::Json::String("app".to_string())
        ]))
    );
    assert_eq!(string(&lines[1], "name"), "zstd");
    assert_eq!(string(&lines[1], "path"), "../zstd2");
    assert_eq!(
        lines[1].get("override"),
        Some(&ingot_diag::json::Json::Bool(true))
    );
    assert_eq!(
        lines[1].get("via"),
        Some(&ingot_diag::json::Json::Array(vec![
            ingot_diag::json::Json::String("app".to_string()),
            ingot_diag::json::Json::String("csv".to_string())
        ]))
    );
    assert_eq!(string(&lines[2], "type"), "summary");
    assert_eq!(lines[2].get("packages").and_then(|v| v.as_i64()), Some(2));
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn fetch_finds_the_content_and_update_rewrites_only_two_values() {
    let dir = scratch("deps-fetch");
    let repo = dir.join("repo");
    let app = dir.join("app");
    let cache = dir.join("cache");
    let rev = repository(&repo, "lib");
    let url = format!("file://{}", repo.display());
    package(
        &app,
        "app",
        &format!(
            "[deps]\nlib = {{ git = \"{url}\", rev = \"{rev}\", hash = \"sha256:{}\" }}\n",
            "0".repeat(64)
        ),
        "import lib;\nfn f() -> i64 { return lib.triple(2); }\n",
    );
    let before = fs::read_to_string(app.join("ingot.toml")).unwrap();

    let fetch = ingot(&["deps", "fetch", "--json"], &app, Some(&cache));
    assert_eq!(fetch.status.code(), Some(1), "{}", stdout(&fetch));
    let lines = json_lines(&fetch);
    assert_eq!(string(&lines[0], "code"), "E1703");
    let fetched: Vec<PathBuf> = fs::read_dir(&cache)
        .unwrap()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect();
    assert_eq!(fetched.len(), 1, "{fetched:?}");
    let hash = sha256::hash_tree(&fetched[0], &[]).unwrap();

    let update = ingot(&["deps", "update", "lib", "--json"], &app, Some(&cache));
    assert_eq!(update.status.code(), Some(0), "{}", stdout(&update));
    let lines = json_lines(&update);
    assert_eq!(string(&lines[0], "name"), "lib");
    assert_eq!(string(&lines[0], "rev"), rev);
    assert_eq!(string(&lines[0], "hash"), hash);
    assert_eq!(string(&lines[1], "type"), "summary");

    let after = fs::read_to_string(app.join("ingot.toml")).unwrap();
    let expected = before.replace(&"0".repeat(64), hash.strip_prefix("sha256:").unwrap());
    assert_eq!(after, expected);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_workspace_shares_one_graph_and_reports_shared_errors_once() {
    let dir = scratch("workspace");
    let shared = dir.join("shared");
    fs::create_dir_all(dir.join("tools/a")).unwrap();
    fs::create_dir_all(dir.join("tools/b")).unwrap();
    fs::write(
        dir.join("ingot.toml"),
        "[workspace]\nmembers = [\"tools/*\"]\n\n[override]\nshared = { path = \"shared\" }\n",
    )
    .unwrap();
    package(
        &dir.join("tools/a"),
        "a",
        "[deps]\nb = { path = \"../b\" }\nshared = { path = \"../../shared2\" }\n",
        "fn a() -> i64 { return true; }\n",
    );
    package(
        &dir.join("tools/b"),
        "b",
        "[deps]\nshared = { path = \"../../shared2\" }\n",
        "fn b() -> i64 { return true; }\n",
    );
    package(
        &shared,
        "shared",
        "",
        "pub fn x() -> i64 {\n    return true;\n}\n",
    );
    package(
        &dir.join("shared2"),
        "shared",
        "",
        "pub fn x() -> i64 { return 2; }\n",
    );

    let output = ingot(&["check", "--json"], &dir, None);
    assert_eq!(output.status.code(), Some(1), "{}", stdout(&output));
    let lines = json_lines(&output);
    let diagnostics: Vec<_> = lines
        .iter()
        .filter(|line| string(line, "type") == "diagnostic")
        .collect();
    let files: Vec<String> = diagnostics.iter().map(|d| file_of(d)).collect();
    assert_eq!(
        files,
        [
            "shared/main.ingot",
            "tools/a/main.ingot",
            "tools/b/main.ingot"
        ],
        "{}",
        stdout(&output)
    );
    let summary = lines.last().unwrap();
    assert_eq!(summary.get("files").and_then(|v| v.as_i64()), Some(3));
    assert_eq!(summary.get("errors").and_then(|v| v.as_i64()), Some(3));

    // Inside a member the paths are the member's own: its files as `main.ingot`, and
    // the other member reached as a dependency as `b/main.ingot`.
    let inside = ingot(&["check", "--json"], &dir.join("tools/a"), None);
    assert_eq!(inside.status.code(), Some(1), "{}", stdout(&inside));
    let inside_lines = json_lines(&inside);
    let inside_files: Vec<String> = inside_lines
        .iter()
        .filter(|line| string(line, "type") == "diagnostic")
        .map(file_of)
        .collect();
    assert_eq!(
        inside_files,
        ["b/main.ingot", "main.ingot", "shared/main.ingot"],
        "{}",
        stdout(&inside)
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn build_at_a_workspace_root_that_is_not_a_package_is_a_named_failure() {
    let dir = scratch("workspace-build");
    fs::write(
        dir.join("ingot.toml"),
        "[workspace]\nmembers = [\"tools/*\"]\n",
    )
    .unwrap();
    package(
        &dir.join("tools/a"),
        "a",
        "",
        "import std.Env;\npub fn main(env: &Env) -> void { }\n",
    );
    let output = ingot(&["build", "--json"], &dir, None);
    assert_eq!(output.status.code(), Some(2), "{}", stdout(&output));
    let lines = json_lines(&output);
    assert_eq!(string(&lines[0], "type"), "error");
    assert!(
        string(&lines[0], "message").contains("not a package"),
        "{}",
        stdout(&output)
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn all_targets_merges_identical_diagnostics() {
    let dir = scratch("all-targets");
    package(&dir, "app", "", "fn f() -> i64 { return true; }\n");
    let output = ingot(&["check", "--json", "--all-targets"], &dir, None);
    assert_eq!(output.status.code(), Some(1), "{}", stdout(&output));
    let lines = json_lines(&output);
    assert_eq!(lines.len(), 2, "{}", stdout(&output));
    let diagnostic = &lines[0];
    assert_eq!(string(diagnostic, "code"), "E0401");
    let Some(ingot_diag::json::Json::Array(targets)) = diagnostic.get("targets") else {
        panic!("targets is an array");
    };
    assert_eq!(targets.len(), 3);
    let summary = &lines[1];
    let Some(ingot_diag::json::Json::Array(targets)) = summary.get("targets") else {
        panic!("summary targets is an array");
    };
    assert_eq!(targets.len(), 3);
    let _ = fs::remove_dir_all(&dir);
}
