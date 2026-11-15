//! Analysis C (loans, E1101–E1105) on source, through the pipeline.
//!
//! Each hard case has a rejecting example and, where the case has an accepting twin,
//! an accepting one. Assertions name the code, the primary span's text and the
//! related labels.

use ingot_diag::source::Span;
use ingot_driver::pipeline::{self, Analysis, UNCHECKED_OWNERSHIP};

fn analyze(name: &str, files: &[(&str, &str)]) -> Analysis {
    let dir = std::env::temp_dir().join(format!(
        "ingot-loan-{}-{}-{name}",
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

fn assert_unchecked(analysis: &Analysis) {
    assert!(
        analysis
            .not_implemented
            .iter()
            .any(|m| m == UNCHECKED_OWNERSHIP),
        "phase 9 must keep the ownership entry, got {:?}\n{}",
        analysis.not_implemented,
        fmt_all(analysis)
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
    assert_unchecked(&analysis);
    let extra: Vec<&String> = analysis
        .not_implemented
        .iter()
        .filter(|m| *m != UNCHECKED_OWNERSHIP)
        .collect();
    assert!(
        extra.is_empty(),
        "loan analysis gave up: {extra:?}\n{}",
        fmt_all(&analysis)
    );
    let got: Vec<String> = analysis
        .diagnostics
        .iter()
        .map(|d| fmt_diag(&analysis, d))
        .collect();
    assert_eq!(got, diags.iter().map(|s| s.to_string()).collect::<Vec<_>>());
}

const SHOW: &str = "\
fn show(x: i64) -> i64 {
    return x;
}
";

#[test]
fn last_use_ends_a_borrow() {
    expect(
        "last-ok",
        &format!(
            "{SHOW}
fn f() -> i64 {{
    var x: i64 = 1;
    let r = &x;
    let seen = show(r.*);
    x = 2;
    return seen + x;
}}
"
        ),
        &[],
    );
    expect(
        "last-bad",
        &format!(
            "{SHOW}
fn f() -> i64 {{
    var x: i64 = 1;
    let r = &x;
    x = 2;
    return show(r.*);
}}
"
        ),
        &["E1101 `x` is assigned while borrowed `x = 2;` borrow=`&x` later_use=`r.*`"],
    );
}

#[test]
fn a_borrow_of_a_container_keeps_its_loans() {
    expect(
        "words",
        "\
struct Arena { n: i64 }

impl Arena {
    fn reset(self: &mut Arena) { self.n = 0; }
}

fn split(text: []u8, arena: &Arena) -> []u8 from text, arena {
    _ = arena.n;
    return text;
}

fn f(text: []u8) -> i64 {
    var arena = Arena.{ n: 1 };
    let words = split(text, &arena);
    let w = &words;
    arena.reset();
    return w.*.len;
}
",
        &[
            "E1102 `arena` is borrowed exclusively while borrowed `arena` borrow=`&arena` later_use=`w.*.len`",
        ],
    );
}

#[test]
fn from_inner_releases_the_receiver() {
    expect(
        "rest-ok",
        "\
struct Parser { src: []u8, pos: i64 }

impl Parser {
    fn rest(self: &Parser) -> []u8 from self.* {
        return self.src[self.pos..self.src.len];
    }
    fn advance(self: &mut Parser) { self.pos += 1; }
}

fn reassign(text: []u8) -> i64 {
    var p = Parser.{ src: text, pos: 0 };
    let r = p.rest();
    p = Parser.{ src: text, pos: 1 };
    return r.len;
}

fn mutate(text: []u8) -> i64 {
    var p = Parser.{ src: text, pos: 0 };
    let r = p.rest();
    p.advance();
    return r.len;
}
",
        &[],
    );
    expect(
        "rest-bad",
        "\
struct Parser { src: []u8, pos: i64 }

impl Parser {
    fn pos_mut(self: &mut Parser) -> &mut i64 from self {
        return &mut self.pos;
    }
}

fn f(text: []u8) -> i64 {
    var p = Parser.{ src: text, pos: 0 };
    let slot = p.pos_mut();
    let n = p.src.len;
    slot.* = n;
    return p.pos;
}
",
        &[
            "E1103 `p.src` is used while exclusively borrowed `p.src.len` borrow=`p` later_use=`slot.* = n;`",
        ],
    );
}

#[test]
fn two_phase_borrows() {
    expect(
        "two-ok",
        "\
struct Counter { n: i64 }

impl Counter {
    fn get(self: &Counter) -> i64 { return self.n; }
    fn add(self: &mut Counter, k: i64) { self.n += k; }
}

struct List { n: i64, buf: [4]i64 }

impl List {
    fn len(self: &List) -> i64 { return self.n; }
    fn push(self: &mut List, v: i64) {
        self.buf[self.n] = v;
        self.n += 1;
    }
}

fn f() -> i64 {
    var c = Counter.{ n: 1 };
    c.add(c.get());
    var list = List.{ n: 0, buf: @zero };
    list.push(list.len());
    return c.get() + list.n;
}
",
        &[],
    );
    expect(
        "two-bad",
        "\
struct Buf { items: [4]i64, len: i64 }

impl Buf {
    fn view(self: &Buf) -> []i64 from self {
        return &self.items[0..self.len];
    }
    fn push(self: &mut Buf, v: i64) {
        self.items[self.len] = v;
        self.len += 1;
    }
}

fn f() -> i64 {
    var b = Buf.{ items: @zero, len: 0 };
    b.push(1);
    let s = b.view();
    b.push(2);
    return s[0];
}
",
        &["E1102 `b` is borrowed exclusively while borrowed `b` borrow=`b` later_use=`s[0]`"],
    );
}

#[test]
fn granularity_of_fields_and_elements() {
    expect(
        "fields",
        "\
struct Pair { a: i64, b: i64 }

fn f() -> i64 {
    var p = Pair.{ a: 1, b: 2 };
    let x = &mut p.a;
    let y = &mut p.b;
    x.* = 10;
    y.* = 20;
    return p.a + p.b;
}
",
        &[],
    );
    expect(
        "elems",
        "\
fn f() -> i64 {
    var a: [2]i64 = [2]i64.{ 1, 2 };
    let x = &mut a[0];
    let y = &mut a[1];
    x.* = 3;
    y.* = 4;
    return a[0] + a[1];
}
",
        &[
            "E1102 `a[_]` is borrowed exclusively while borrowed `&mut a[1]` borrow=`&mut a[0]` later_use=`x.* = 3;`",
        ],
    );
}

#[test]
fn reborrows_of_an_exclusive_borrow() {
    expect(
        "re-ok",
        "\
fn bump(x: &mut i64) { x.* += 1; }

fn twice(x: &mut i64) -> i64 {
    bump(x);
    bump(x);
    return x.*;
}
",
        &[],
    );
    expect(
        "re-bad",
        "\
fn f() -> i64 {
    var x: i64 = 1;
    let a = &mut x;
    let b = &mut x;
    a.* = 2;
    b.* = 3;
    return x;
}
",
        &[
            "E1102 `x` is borrowed exclusively while borrowed `&mut x` borrow=`&mut x` later_use=`a.* = 2;`",
        ],
    );
}

#[test]
fn storage_end_while_a_loan_is_live() {
    expect(
        "block",
        "\
fn f() -> i64 {
    let a: i64 = 1;
    var r: &i64 = &a;
    {
        let b: i64 = 2;
        r = &b;
    }
    return r.*;
}
",
        &["E1105 `b` is borrowed past the end of its storage `&b` later_use=`r.*` borrow=`&b`"],
    );
    // The storage end is the `break`, so `scope_end` names that exit.
    expect(
        "break",
        "\
fn f(n: i64) -> i64 {
    let seed: i64 = 0;
    var r: &i64 = &seed;
    while n > 0 {
        let b: i64 = 1;
        r = &b;
        break;
    }
    return r.*;
}
",
        &[
            "E1105 `b` is borrowed past the end of its storage `&b` scope_end=`break;` later_use=`r.*` borrow=`&b`",
        ],
    );
    expect(
        "stored",
        "\
struct Lexer { src: []u8, pos: i64 }

fn set_input(l: &mut Lexer, text: []u8 into l) {
    l.src = text;
    l.pos = 0;
}

fn f(a: []u8) -> i64 {
    var lx = Lexer.{ src: a, pos: 0 };
    {
        let local: [4]u8 = @zero;
        set_input(&mut lx, &local[0..4]);
    }
    return lx.src.len;
}
",
        &[
            "E1105 `local` is borrowed past the end of its storage `local[0..4]` later_use=`lx.src.len` borrow=`local[0..4]`",
        ],
    );
    expect(
        "type-param",
        "\
struct Slot[T] { v: T }

fn put[T: Discard](s: &mut Slot[T], x: T) {
    s.v = x;
}

fn f(a: []u8) -> i64 {
    var slot = Slot[[]u8].{ v: a };
    {
        let local: [4]u8 = @zero;
        put(&mut slot, &local[0..4]);
    }
    return slot.v.len;
}
",
        &[
            "E1105 `local` is borrowed past the end of its storage `local[0..4]` later_use=`slot.v.len` borrow=`local[0..4]`",
        ],
    );
}

#[test]
fn the_kill_rule_drops_a_cursor_loan() {
    expect(
        "cursor",
        "\
struct Node { value: i64, next: ?&mut Node }

fn get_mut(n: &mut Node) -> &mut Node from n {
    return n;
}

fn last(head: &mut Node) -> i64 {
    var cur: &mut Node = head;
    while let .some(next) = &mut cur.next {
        cur = get_mut(next.*);
    }
    return cur.value;
}
",
        &[],
    );
}

#[test]
fn moving_or_forgetting_while_borrowed() {
    expect(
        "move",
        "\
struct Tok { id: i64 }

fn f() -> i64 {
    let t = Tok.{ id: 1 };
    let r = &t.id;
    let u = t;
    return r.* + u.id;
}
",
        &["E1104 `t` is moved while borrowed `let u = t;` borrow=`&t.id` later_use=`r.*`"],
    );
    expect(
        "forget-copy",
        "\
fn f() -> i64 {
    let x: i64 = 1;
    let r = &x;
    @forget(x);
    return r.*;
}
",
        &["E1104 `x` is moved while borrowed `@forget(x)` borrow=`&x` later_use=`r.*`"],
    );
    expect(
        "forget-lin",
        "\
linear struct Tok { id: i64 }

fn f() -> i64 {
    let t = Tok.{ id: 1 };
    let r = &t.id;
    @forget(t);
    return r.*;
}
",
        &["E1104 `t` is moved while borrowed `t` borrow=`&t.id` later_use=`r.*`"],
    );
}

#[test]
fn a_closure_keeps_its_capture_borrowed() {
    expect(
        "after",
        "\
fn f() -> i64 {
    var x: i64 = 1;
    var c = fn [&mut x]() { x.* = x.* + 1; };
    c();
    x = 2;
    return x;
}
",
        &[],
    );
    expect(
        "during",
        "\
fn f() -> i64 {
    var x: i64 = 1;
    var c = fn [&mut x]() { x.* = x.* + 1; };
    x = 2;
    c();
    return x;
}
",
        &[
            "E1101 `x` is assigned while borrowed `x = 2;` borrow=`fn [&mut x]() { x.* = x.* + 1; }` later_use=`c`",
        ],
    );
}

#[test]
fn a_loan_held_across_a_loop_conflicts() {
    expect(
        "fresh",
        "\
fn f(n: i64) -> i64 {
    var x: i64 = 0;
    var i: i64 = 0;
    var s: i64 = 0;
    while i < n {
        let r = &x;
        s = r.*;
        x = i;
        i = i + 1;
    }
    return s;
}
",
        &[],
    );
    expect(
        "held",
        "\
fn f(n: i64) -> i64 {
    var x: i64 = 0;
    let y: i64 = 1;
    var r: &i64 = &y;
    var i: i64 = 0;
    while i < n {
        x = i;
        let v = r.*;
        r = &x;
        i = i + 1;
        _ = v;
    }
    return r.*;
}
",
        &["E1101 `x` is assigned while borrowed `x = i;` borrow=`&x` later_use=`r.*`"],
    );
}

#[test]
fn a_loan_on_a_parameter_copy_ends_at_exit() {
    expect(
        "param-ok",
        "\
copy struct P { n: i64 }

fn f(p: P) -> i64 {
    let q = p;
    let r = &q.n;
    let v = r.*;
    return v + p.n;
}
",
        &[],
    );
    expect(
        "param-bad",
        "\
copy struct P { n: i64 }

fn f(p: P) -> &i64 from static {
    let q = p;
    let r = &q.n;
    return r;
}
",
        &["E1203 the returned value borrows a local `return r;` borrow=`&q.n`"],
    );
}
