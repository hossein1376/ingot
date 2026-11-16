//! Phase 9 scopes and tasks, through the pipeline.
//!
//! Rejecting cases name the code, the primary span's text and the related labels.
//! Accepting twins must stay free of E11xx, E12xx and E14xx. No test calls `join`:
//! std does not declare it, and a call would poison the body.

use ingot_diag::source::Span;
use ingot_driver::pipeline::{self, Analysis};

fn analyze(name: &str, files: &[(&str, &str)]) -> Analysis {
    let dir = std::env::temp_dir().join(format!(
        "ingot-task-{}-{}-{name}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).expect("temp dir");
    std::fs::write(dir.join("ingot.toml"), "name = \"app\"\n").expect("manifest");
    for (file, text) in files {
        let path = dir.join(file);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("module dir");
        }
        std::fs::write(&path, text).expect("source");
    }
    let analysis = pipeline::analyze_path(&dir).expect("analysis");
    let _ = std::fs::remove_dir_all(&dir);
    analysis
}

fn analyze_main(name: &str, source: &str) -> Analysis {
    analyze(name, &[("main.ingot", source)])
}

fn assert_reached_phase_8(analysis: &Analysis) {
    assert!(
        !analysis.cfgs.is_empty(),
        "the body must reach phase 8, got {:?}",
        analysis.not_implemented
    );
}

fn text_of(analysis: &Analysis, file: &str, span: Span) -> String {
    let source = analysis
        .sources
        .files
        .iter()
        .find(|s| s.path == file)
        .unwrap_or_else(|| panic!("no source {file}"));
    source.text_of(span).to_string()
}

fn fmt_diag(analysis: &Analysis, d: &ingot_diag::diagnostic::Diagnostic) -> String {
    let mut labels = Vec::new();
    for r in &d.related {
        labels.push(format!(
            "{}=`{}`",
            r.role,
            text_of(analysis, &r.file, r.span)
        ));
    }
    let mut line = format!(
        "{} {} `{}`",
        d.code,
        d.message,
        text_of(analysis, &d.file, d.span)
    );
    if !labels.is_empty() {
        line.push(' ');
        line.push_str(&labels.join(" "));
    }
    if let Some(e) = d.fixes.first().and_then(|f| f.edits.first()) {
        line.push_str(&format!(" fix=`{}`", e.text));
    }
    line
}

fn fmt_all(analysis: &Analysis) -> String {
    analysis
        .diagnostics
        .iter()
        .map(|d| fmt_diag(analysis, d))
        .collect::<Vec<_>>()
        .join("\n")
}

fn expect(name: &str, source: &str, diags: &[&str]) {
    let analysis = analyze_main(name, source);
    assert_reached_phase_8(&analysis);
    let extra: Vec<&String> = analysis.not_implemented.iter().collect();
    assert!(
        extra.is_empty(),
        "task analysis gave up: {extra:?}\n{}",
        fmt_all(&analysis)
    );
    let got: Vec<String> = analysis
        .diagnostics
        .iter()
        .map(|d| fmt_diag(&analysis, d))
        .collect();
    assert_eq!(got, diags.iter().map(|s| s.to_string()).collect::<Vec<_>>());
}

const BUMP: &str = "\
fn bump(x: &mut i64) {
    x.* += 1;
}
";

#[test]
fn task_borrows_a_local_of_its_scope() {
    expect(
        "scope-local",
        &format!(
            "{BUMP}
fn f() {{
    scope s {{
        var count: i64 = 0;
        spawn bump(&mut count) in s;
    }}
}}
"
        ),
        &[
            "E1403 `count` is borrowed by a task past the end of its scope `&mut count` spawn=`spawn bump(&mut count) in s;` declared=`var count: i64 = 0;`",
        ],
    );
    expect(
        "scope-local-ok",
        &format!(
            "{BUMP}
fn f() -> i64 {{
    var count: i64 = 0;
    scope s {{
        spawn bump(&mut count) in s;
    }}
    return count;
}}
"
        ),
        &[],
    );
}

#[test]
fn two_tasks_one_exclusive_borrow() {
    expect(
        "two-excl",
        &format!(
            "{BUMP}
fn f() -> i64 {{
    var total: i64 = 0;
    scope s {{
        spawn bump(&mut total) in s;
        spawn bump(&mut total) in s;
    }}
    return total;
}}
"
        ),
        &[
            "E1102 `total` is borrowed exclusively while borrowed `&mut total` borrow=`&mut total` later_use=`scope s {\n        spawn bump(&mut total) in s;\n        spawn bump(&mut total) in s;\n    }`",
        ],
    );
    expect(
        "two-shared",
        "\
fn read(x: &i64) {
    _ = x.*;
}

fn f() -> i64 {
    let x: i64 = 1;
    scope s {
        spawn read(&x) in s;
        spawn read(&x) in s;
    }
    return x;
}
",
        &[],
    );
}

#[test]
fn use_before_the_scope_ends() {
    expect(
        "use-inside",
        &format!(
            "{BUMP}
fn f() -> i64 {{
    var total: i64 = 0;
    scope s {{
        spawn bump(&mut total) in s;
        total = 1;
    }}
    return total;
}}
"
        ),
        &[
            "E1101 `total` is assigned while borrowed `total = 1;` borrow=`&mut total` later_use=`scope s {\n        spawn bump(&mut total) in s;\n        total = 1;\n    }`",
        ],
    );
    expect(
        "use-after",
        &format!(
            "{BUMP}
fn f() -> i64 {{
    var total: i64 = 0;
    scope s {{
        spawn bump(&mut total) in s;
    }}
    total += 1;
    return total;
}}
"
        ),
        &[],
    );
}

#[test]
fn nested_scope_releases_the_borrow() {
    expect(
        "nested",
        &format!(
            "{BUMP}
fn f() -> i64 {{
    var total: i64 = 0;
    scope outer {{
        scope inner {{
            spawn bump(&mut total) in inner;
        }}
        total += 1;
        spawn bump(&mut total) in outer;
    }}
    return total;
}}
"
        ),
        &[],
    );
}

#[test]
fn a_handle_keeps_a_loan_on_its_scope() {
    expect(
        "forget-ok",
        "\
fn work() -> i64 {
    return 1;
}

fn f() {
    scope s {
        let h = spawn work() in s;
        @forget(h);
    }
}
",
        &[],
    );
    expect(
        "return-handle",
        "\
import std.task.Task;

fn work() -> i64 {
    return 1;
}

fn f() -> Task[i64] {
    scope s {
        let h = spawn work() in s;
        return h;
    }
}
",
        &[
            "E1201 the return type holds a borrow and needs a `from` clause `Task[i64]`",
            "E1203 the returned value borrows a local `return h;` borrow=`s`",
        ],
    );
}

#[test]
fn spawn_into_a_scope_parameter() {
    expect(
        "needs-into",
        "\
import std.task.Scope;

fn bump(x: &mut i64) {
    x.* += 1;
}

fn start(s: &Scope, x: &mut i64) {
    spawn bump(x) in s;
}
",
        &[
            "E1204 the view from `x` is stored where it reaches `s` `spawn bump(x) in s;` signature=`x` fix=` into s`",
        ],
    );
    expect(
        "with-into",
        "\
import std.task.Scope;

fn bump(x: &mut i64) {
    x.* += 1;
}

fn start(s: &Scope, x: &mut i64 into s) {
    spawn bump(x) in s;
}

fn f() -> i64 {
    var n: i64 = 0;
    scope s {
        start(s, &mut n);
    }
    return n;
}
",
        &[],
    );
}

#[test]
fn spawn_arguments_and_results_must_be_send() {
    expect(
        "arg",
        "\
fn poke(p: *mut i64) {
}

fn f() {
    var x: i64 = 0;
    let p = &mut x as *mut i64;
    scope s {
        spawn poke(p) in s;
    }
}
",
        &["E1401 `*mut i64` is not `Send` `p` declared=`*mut i64`"],
    );
    expect(
        "result",
        "\
struct Boxy { p: *mut i64 }

fn make() -> Boxy {
    return Boxy.{ p: @zero };
}

fn f() {
    scope s {
        let h = spawn make() in s;
        @forget(h);
    }
}
",
        &["E1401 `*mut i64` is not `Send` `let h = spawn make() in s;` declared=`*mut i64`"],
    );
    expect(
        "bounded",
        "\
fn take[T: Discard](x: T) {
}

fn bounded[T: Send + Discard](x: T) {
    scope s {
        spawn take(x) in s;
    }
}

fn open[T: Discard](x: T) {
    scope s {
        spawn take(x) in s;
    }
}
",
        &["E1401 `T` is not `Send` `x` declared=`T`"],
    );
}

#[test]
fn par_for_item_must_be_send() {
    expect(
        "item",
        "\
fn f(xs: []*mut i64) {
    par for x in xs {
        _ = x;
    }
}
",
        &["E1401 `*mut i64` is not `Send` `xs` declared=`x`"],
    );
    expect(
        "item-ok",
        "\
fn scale(xs: []mut i64, k: i64) {
    par for x in xs {
        x.* = x.* * k;
    }
}
",
        &[],
    );
}

#[test]
fn par_for_capture_must_be_sync() {
    expect(
        "not-sync",
        "\
struct Boxy { p: *mut i64 }

fn f(b: Boxy) {
    par for i in 0..4 {
        _ = b.p;
        _ = i;
    }
}
",
        &["E1402 `b` is not `Sync` (`*mut i64`) `_ = b.p;` declared=`*mut i64`"],
    );
    expect(
        "twice",
        "\
struct Boxy { p: *mut i64 }

fn f(b: Boxy) {
    par for i in 0..4 {
        _ = b.p;
        _ = b.p;
        _ = i;
    }
}
",
        &["E1402 `b` is not `Sync` (`*mut i64`) `_ = b.p;` declared=`*mut i64`"],
    );
    expect(
        "sync-ok",
        "\
import std.sync.Atomic;

fn touch(a: &Atomic[i64], k: i64) {
    _ = a;
    _ = k;
}

fn f(a: &Atomic[i64], n: i64) {
    par for i in 0..4 {
        touch(a, i + n);
    }
}
",
        &[],
    );
}

#[test]
fn par_for_body_does_not_mutate_an_outer_local() {
    expect(
        "assign",
        "\
fn f() -> i64 {
    var total: i64 = 0;
    par for i in 0..10 {
        total += i;
    }
    return total;
}
",
        &["E1407 a `par for` body assigns to `total` `total += i;` declared=`total`"],
    );
    expect(
        "move",
        "\
struct Buf { len: i64 }

fn f(b: Buf) {
    par for i in 0..4 {
        let c = b;
        _ = c.len + i;
    }
}
",
        &["E1407 a `par for` body moves `b` `let c = b;` declared=`b`"],
    );
    expect(
        "excl",
        "\
fn f() {
    var x: i64 = 0;
    par for i in 0..4 {
        let r = &mut x;
        r.* += i;
    }
}
",
        &["E1407 a `par for` body borrows exclusively `x` `&mut x` declared=`x`"],
    );
}

#[test]
fn dispatch_one_slice_as_input_and_output() {
    expect(
        "same",
        "\
import std.exec;

kernel fn both(i: Idx, a: []mut i64, b: []mut i64) {
    a[i] = b[i];
}

fn f() {
    var xs: [4]i64 = @zero;
    dispatch both(&mut xs[0..4], &mut xs[0..4]) over 0..4 on exec.seq;
}
",
        &[
            "E1102 `xs` is borrowed exclusively while borrowed `xs[0..4]` borrow=`xs[0..4]` later_use=`dispatch both(&mut xs[0..4], &mut xs[0..4]) over 0..4 on exec.seq;`",
        ],
    );
    expect(
        "distinct",
        "\
import std.exec;

kernel fn both(i: Idx, a: []mut i64, b: []mut i64) {
    a[i] = b[i];
}

fn f() {
    var xs: [4]i64 = @zero;
    var ys: [4]i64 = @zero;
    dispatch both(&mut xs[0..4], &mut ys[0..4]) over 0..4 on exec.seq;
}
",
        &[],
    );
}

#[test]
fn a_loan_from_an_earlier_iteration_conflicts() {
    expect(
        "spawn-loop",
        &format!(
            "{BUMP}
fn f() {{
    var total: i64 = 0;
    scope s {{
        for i in 0..3 {{
            _ = i;
            spawn bump(&mut total) in s;
        }}
    }}
}}
"
        ),
        &[
            "E1102 `total` is borrowed exclusively while borrowed `&mut total` borrow=`&mut total` later_use=`scope s {\n        for i in 0..3 {\n            _ = i;\n            spawn bump(&mut total) in s;\n        }\n    }`",
        ],
    );
    expect(
        "ref-array",
        "\
fn f() {
    var x: i64 = 0;
    var y: i64 = 0;
    var z: i64 = 0;
    var i: i64 = 0;
    var refs: [2]&mut i64 = [2]&mut i64.{ &mut y, &mut z };
    while i < 2 {
        refs[i] = &mut x;
        i = i + 1;
    }
    _ = refs[0].*;
    _ = refs[1].*;
}
",
        &[
            "E1102 `x` is borrowed exclusively while borrowed `&mut x` borrow=`&mut x` later_use=`refs[i]`",
        ],
    );
}

#[test]
fn an_if_scope_reaches_both_scopes() {
    let body = |arms: &str| {
        format!(
            "\
import std.task.Scope;

{BUMP}
fn f(c: bool) {{
    scope a {{
        var count: i64 = 0;
        scope b {{
            let t: &Scope = if c {{ {arms} }};
            spawn bump(&mut count) in t;
        }}
    }}
}}
"
        )
    };
    let e1403 = "E1403 `count` is borrowed by a task past the end of its scope `&mut count` spawn=`spawn bump(&mut count) in t;` declared=`var count: i64 = 0;`";
    expect("if-b-else-a", &body("b } else { a"), &[e1403]);
    expect("if-a-else-b", &body("a } else { b"), &[e1403]);
}

#[test]
fn a_handle_from_a_scope_parameter_is_not_a_local_loan() {
    expect(
        "return-spawn",
        "\
import std.task.Scope;
import std.task.Task;

fn work() -> i64 {
    return 1;
}

fn start(s: &Scope) -> Task[i64] from s {
    return spawn work() in s;
}
",
        &[],
    );
    expect(
        "return-let",
        "\
import std.task.Scope;
import std.task.Task;

fn work() -> i64 {
    return 1;
}

fn start(s: &Scope) -> Task[i64] from s {
    let h = spawn work() in s;
    return h;
}
",
        &[],
    );
}
