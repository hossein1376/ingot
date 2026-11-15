//! Analysis A (moves and linear values) on source, through the pipeline.
//!
//! Each hard case has a rejecting example and, where the case has an accepting
//! twin, an accepting one. Assertions name the code, the primary span's text and
//! the related labels.

use ingot_diag::source::Span;
use ingot_driver::pipeline::{self, Analysis, UNCHECKED_OWNERSHIP};

fn analyze(name: &str, files: &[(&str, &str)]) -> Analysis {
    let dir = std::env::temp_dir().join(format!(
        "ingot-own-{}-{}-{name}",
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
        "phase 9 must keep the ownership entry, got {:?}\nE10: {:?}\nother: {:?}",
        analysis.not_implemented,
        e10(analysis),
        others(analysis)
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

/// `CODE \`span\` role=\`span\` ...` plus a fix edit when the diagnostic has one.
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

fn e10(analysis: &Analysis) -> Vec<String> {
    analysis
        .diagnostics
        .iter()
        .filter(|d| d.code.starts_with("E10"))
        .map(|d| fmt_diag(analysis, d))
        .collect()
}

fn others(analysis: &Analysis) -> Vec<String> {
    analysis
        .diagnostics
        .iter()
        .filter(|d| !d.code.starts_with("E10"))
        .map(|d| format!("{}: {}", d.code, d.message))
        .collect()
}

fn expect(name: &str, source: &str, diags: &[&str]) {
    let analysis = analyze_main(name, source);
    assert_unchecked(&analysis);
    assert_eq!(
        e10(&analysis),
        diags.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
        "other diagnostics: {:?}",
        others(&analysis)
    );
}

fn expect_files(name: &str, files: &[(&str, &str)], diags: &[&str]) {
    let analysis = analyze(name, files);
    assert_unchecked(&analysis);
    assert_eq!(
        e10(&analysis),
        diags.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
        "other diagnostics: {:?}",
        others(&analysis)
    );
}

const TOKEN: &str = "\
linear struct Token { id: i32 }

impl Token {
    fn make(id: i32) -> Token { return Token.{ id: id }; }
    fn release(self: Token) { let Token.{ id: _ } = self; }
}
";

const MOVED: &str = "\
struct M { v: i64 }

struct S { a: M, b: M }
";

#[test]
fn moved_on_one_branch_is_maybe_after_the_join() {
    let src = format!(
        "{MOVED}
fn bad(flag: bool) -> i64 {{
    let m = M.{{ v: 1 }};
    if flag {{
        let x = m;
        let _ = x.v;
    }}
    return m.v;
}}

fn good(flag: bool) -> i64 {{
    var m = M.{{ v: 1 }};
    if flag {{
        let x = m;
        let _ = x.v;
    }} else {{
        let y = m;
        let _ = y.v;
    }}
    m = M.{{ v: 2 }};
    return m.v;
}}
"
    );
    // Spans are filled from the first failure's actual text if these miss.
    expect(
        "branch",
        &src,
        &["E1001 may be moved `return m.v;` move=`let x = m;`"],
    );
}

#[test]
fn move_inside_a_loop_reports_the_second_iteration() {
    let src = format!(
        "{MOVED}
fn bad() -> i64 {{
    let m = M.{{ v: 1 }};
    var i = 0;
    while i < 2 {{
        let x = m;
        let _ = x.v;
        i = i + 1;
    }}
    return 0;
}}

fn good() -> i64 {{
    var i = 0;
    while i < 2 {{
        let m = M.{{ v: 1 }};
        let x = m;
        let _ = x.v;
        i = i + 1;
    }}
    return 0;
}}
"
    );
    expect(
        "loop",
        &src,
        &[
            "E1001 may be moved `let x = m;` loop=`while i < 2 {\n        let x = m;\n        let _ = x.v;\n        i = i + 1;\n    }` move=`let x = m;`",
        ],
    );
}

#[test]
fn field_move_leaves_the_sibling_and_blocks_the_whole() {
    let src = format!(
        "{MOVED}
fn sibling() -> i64 {{
    let s = S.{{ a: M.{{ v: 1 }}, b: M.{{ v: 2 }} }};
    let x = s.a;
    return x.v + s.b.v;
}}

fn whole() -> i64 {{
    let s = S.{{ a: M.{{ v: 1 }}, b: M.{{ v: 2 }} }};
    let x = s.a;
    let _ = x.v;
    let y = s;
    return y.a.v + y.b.v;
}}

fn restored() -> i64 {{
    var s = S.{{ a: M.{{ v: 1 }}, b: M.{{ v: 2 }} }};
    let x = s.a;
    let _ = x.v;
    s.a = M.{{ v: 3 }};
    let y = s;
    return y.a.v + y.b.v;
}}
"
    );
    expect(
        "fields",
        &src,
        &["E1001 was moved `let y = s;` move=`let x = s.a;`"],
    );
}

#[test]
fn linear_struct_field_by_field_is_only_inside_its_module() {
    let pair = "\
pub struct M { pub v: i64 }

pub linear struct Box { pub m: M }

pub linear struct Bit { pub n: i64 }

pub linear struct Two { pub a: M, pub b: Bit }

impl Box {
    pub fn make(v: i64) -> Box { return Box.{ m: M.{ v: v } }; }
    pub fn consume(self: Box) -> i64 {
        let Box.{ m } = self;
        return m.v;
    }
}

fn dropped(t: Two) -> i64 {
    let Two.{ a, .. } = t;
    return a.v;
}
";
    let main = "\
import app.pair.Box;

fn steal() -> i64 {
    let b = Box.make(1);
    let Box.{ m } = b;
    return m.v;
}

fn whole() -> i64 {
    let b = Box.make(1);
    return b.consume();
}
";
    expect_files(
        "module",
        &[("pair.ingot", pair), ("main.ingot", main)],
        &[
            "E1006 cannot move a field of `Box` outside its module `let Box.{ m } = b;`",
            "E1003 `t` is not consumed; no consuming function `t` scope_end=`return a.v;`",
        ],
    );
}

#[test]
fn linear_enum_payloads_follow_the_variant_records() {
    let src = format!(
        "{TOKEN}
enum Tri {{ a: Token, b: Token, c: Token }}

fn matched(o: ?Token) {{
    match o {{
        .some(t) => {{ t.release(); }}
        .none => {{}}
    }}
}}

fn if_let(o: ?Token) {{
    if let .some(t) = o {{
        t.release();
    }}
}}

fn none_literal() -> ?Token {{
    let o: ?Token = .none;
    return o;
}}

fn from_call() -> i32 {{
    let o = make();
    let p = &o;
    let _ = p;
    return 0;
}}

fn make() -> ?Token {{ return .some(Token.make(1)); }}

fn none_arm(o: ?Token) -> ?Token {{
    match o {{
        .some(t) => {{ t.release(); return .none; }}
        .none => {{ return o; }}
    }}
}}

fn one_arm(o: Tri) -> i32 {{
    match o {{
        .a(t) => {{ t.release(); return 1; }}
        .b(held) => {{
            if held.id == 0 {{ return 2; }}
            return 3;
        }}
        .c(t) => {{ t.release(); return 4; }}
    }}
}}
"
    );
    expect(
        "enum",
        &src,
        &[
            "E1003 `o` is not consumed; no consuming function `o` scope_end=`return 0;`",
            "E1003 `held` is not consumed `held` scope_end=`return 2;` scope_end=`return 3;` fix=`\n        defer { held.release(); }`",
        ],
    );
}

#[test]
fn results_are_consumed_on_every_arm() {
    let src = format!(
        "{TOKEN}
enum E {{ no }}

fn parse(ok: bool) -> Token ! E {{
    if !ok {{ fail .no; }}
    return Token.make(1);
}}

fn by_try(ok: bool) -> i32 ! E {{
    let t = try parse(ok);
    let id = t.id;
    t.release();
    return id;
}}

fn by_catch(ok: bool) -> i32 {{
    let t = parse(ok) catch _ => {{ return 0; }};
    let id = t.id;
    t.release();
    return id;
}}

fn by_orelse() -> i32 {{
    let o: ?Token = .some(Token.make(1));
    let t = o orelse {{ return 0; }};
    let id = t.id;
    t.release();
    return id;
}}

fn handed_off(ok: bool) -> Token ! E {{
    let t = Token.make(1);
    errdefer {{ t.release(); }}
    let u = try parse(ok);
    u.release();
    return t;
}}

fn leaked(ok: bool) -> i32 ! E {{
    let t = try parse(ok);
    return t.id;
}}
"
    );
    expect(
        "result",
        &src,
        &[
            "E1003 `t` is not consumed `t` scope_end=`return t.id;` fix=`\n    defer { t.release(); }`",
        ],
    );
}

#[test]
fn defer_moves_are_e1005() {
    let src = format!(
        "{TOKEN}
fn both() {{
    let t = Token.make(1);
    defer {{ t.release(); }}
    t.release();
}}

fn borrowed() -> i32 {{
    let t = Token.make(1);
    defer {{ t.release(); }}
    return t.id;
}}
"
    );
    expect(
        "defer",
        &src,
        &["E1005 moved while a deferred block still uses it `t` defer=`defer { t.release(); }`"],
    );
}

#[test]
fn an_unconsumed_linear_temporary_is_e1003() {
    let src = format!(
        "{TOKEN}
fn leaked() -> i32 {{
    let id = Token.make(1).id;
    return id;
}}

fn bound() -> i32 {{
    let t = Token.make(1);
    let id = t.id;
    t.release();
    return id;
}}
"
    );
    expect(
        "temp",
        &src,
        &["E1003 a temporary is not consumed; consuming functions: release `Token.make(1)`"],
    );
}

#[test]
fn replace_swap_take_and_forget_are_not_moves() {
    let src = format!(
        "{TOKEN}
fn replaced() -> i32 {{
    var t = Token.make(1);
    let old = @replace(&mut t, Token.make(2));
    let a = old.id;
    old.release();
    let b = t.id;
    t.release();
    return a + b;
}}

fn swapped() -> i32 {{
    var a = Token.make(1);
    var b = Token.make(2);
    @swap(&mut a, &mut b);
    let ia = a.id;
    let ib = b.id;
    a.release();
    b.release();
    return ia + ib;
}}

fn forgotten() {{
    let t = Token.make(1);
    @forget(t);
}}

fn taken(p: &mut ?Token) -> i32 {{
    let o = p.take();
    match o {{
        .some(t) => {{
            let id = t.id;
            t.release();
            return id;
        }}
        .none => {{ return 0; }}
    }}
}}
"
    );
    expect("builtins", &src, &[]);
}

#[test]
fn moves_out_of_a_deref_an_element_or_a_static_are_e1002() {
    let src = format!(
        "{MOVED}
{TOKEN}
static item: M = M.{{ v: 1 }};

fn from_ref(r: &M) -> M {{
    return r.*;
}}

fn from_elem(a: [2]M) -> M {{
    return a[0];
}}

fn from_linear_elem(a: [1]Token) -> Token {{
    return a[0];
}}

fn from_static() -> M {{
    return item;
}}

fn reborrowed(p: &mut M) -> i64 {{
    poke(p);
    return p.v;
}}

fn poke(p: &mut M) {{
    p.v = p.v + 1;
}}

fn bound_ref(p: &mut M) -> i64 {{
    let q = p;
    return p.v + q.v;
}}
"
    );
    expect(
        "illegal",
        &src,
        &[
            "E1002 cannot move out of a borrow, an element or a static; use `@replace`, `@swap` or `.take()` `r.*`",
            "E1002 cannot move out of a borrow, an element or a static; use `@replace`, `@swap` or `.take()` `return a[0];`",
            "E1003 `a` is not consumed; no consuming function `a` scope_end=`return a[0];`",
            "E1002 cannot move out of a borrow, an element or a static; use `@replace`, `@swap` or `.take()` `return a[0];`",
            "E1002 cannot move out of a borrow, an element or a static; use `@replace`, `@swap` or `.take()` `return item;`",
            "E1001 was moved `p.v` move=`let q = p;`",
        ],
    );
}

#[test]
fn closures_move_captures_by_the_call_mode() {
    let src = format!(
        "{MOVED}
fn once_ok() -> i64 {{
    let m = M.{{ v: 1 }};
    let f = fn [m]() -> i64 {{
        let x = m;
        return x.v;
    }};
    return f();
}}

fn once_outer() -> i64 {{
    let m = M.{{ v: 1 }};
    let f = fn [m]() -> i64 {{ return m.v; }};
    return m.v + f();
}}

fn mut_ok() -> i64 {{
    var m = M.{{ v: 1 }};
    var f = fn [&mut m]() -> i64 {{ return m.v; }};
    return f();
}}

fn mut_moves() -> i64 {{
    var m = M.{{ v: 1 }};
    var f = fn [&mut m]() -> i64 {{
        let p = m;
        return p.v;
    }};
    return f();
}}
"
    );
    expect(
        "closure",
        &src,
        &[
            "E1001 was moved `m.v` move=`fn [m]() -> i64 { return m.v; }`",
            "E1002 cannot move out of a borrow, an element or a static; use `@replace`, `@swap` or `.take()` `let p = m;`",
        ],
    );
}

#[test]
fn a_type_parameter_without_discard_is_linear() {
    expect(
        "generic",
        "fn drop_it[T](x: T) {}\nfn kept[T: Discard](x: T) {}\n",
        &["E1003 `x` is not consumed `x` scope_end=`}` fix=`: Discard`"],
    );
}

#[test]
fn a_consuming_for_moves_the_array() {
    let src = format!(
        "{MOVED}
fn later() -> [2]M {{
    let ms: [2]M = [2]M.{{ M.{{ v: 1 }}, M.{{ v: 2 }} }};
    for m in ms {{
        let _ = m.v;
    }}
    return pass(ms);
}}

fn pass(a: [2]M) -> [2]M {{ return a; }}

fn copied() -> i64 {{
    let xs: [2]i64 = [2]i64.{{ 1, 2 }};
    var total = 0;
    for x in xs {{
        total = total + x;
    }}
    return total + xs[0];
}}
"
    );
    expect(
        "forloop",
        &src,
        &["E1001 was moved `ms` move=`for m in ms {\n        let _ = m.v;\n    }`"],
    );
}

#[test]
fn overwriting_a_linear_value_is_e1004() {
    let src = format!(
        "{TOKEN}
fn overwritten() {{
    var t = Token.make(1);
    t = Token.make(2);
    t.release();
}}

fn after_move() {{
    var t = Token.make(1);
    let u = t;
    u.release();
    t = Token.make(2);
    t.release();
}}
"
    );
    expect(
        "assign",
        &src,
        &["E1004 assignment overwrites a linear value `t = Token.make(2);`"],
    );
}

fn expect_clean(name: &str, source: &str) {
    let analysis = analyze_main(name, source);
    assert_unchecked(&analysis);
    assert_eq!(
        e10(&analysis),
        Vec::<String>::new(),
        "other diagnostics: {:?}",
        others(&analysis)
    );
}

#[test]
fn match_of_a_result_moves_every_payload() {
    let src = "\
enum E { no }

fn f(ok: bool) -> i64 ! E {
    if !ok { fail .no; }
    return 1;
}

fn g(ok: bool) -> i64 {
    let r = f(ok);
    match r {
        .ok(v) => { return v; }
        .err(_) => { return 0; }
    }
}

fn h(ok: bool) -> i64 {
    let r = f(ok);
    match r {
        .ok(v) => { return v; }
        .err(e) => {
            match e {
                .no => { return 0; }
            }
        }
    }
}

fn w(ok: bool) -> i64 {
    var r = f(ok);
    while let .ok(v) = r {
        return v;
    }
    match r {
        .ok(_) => { return 0; }
        .err(_) => { return 0; }
    }
}

fn k(ok: bool) -> i64 {
    let r = f(ok);
    if let .ok(v) = r {
        return v;
    }
    return 0;
}

fn unused(ok: bool) -> i64 {
    let r = f(ok);
    match &r {
        .ok(_) => { return 1; }
        .err(_) => { return 0; }
    }
}
";
    let analysis = analyze_main("resmatch", src);
    assert_unchecked(&analysis);
    let got = e10(&analysis);
    assert!(
        got.iter().all(|d| d.contains("`r`")),
        "only `r` should be reported, got {got:?}\nother: {:?}",
        others(&analysis)
    );
    assert!(
        got.iter()
            .any(|d| d.starts_with("E1003") && d.contains("`r`")),
        "if let and match &r stay E1003, got {got:?}"
    );
    assert_eq!(got.len(), 2, "k and unused, got {got:?}");
}

#[test]
fn only_a_result_obligates_every_variant() {
    let src = "\
linear struct Token { id: i32 }

impl Token {
    fn make(id: i32) -> Token { return Token.{ id: id }; }
    fn release(self: Token) { let Token.{ id: _ } = self; }
}

enum Two { a: Token, b: i64 }

fn borrowed() {
    let e: Two = .b(5);
    match &e {
        .a(_) => {}
        .b(_) => {}
    }
}

fn taken(e: Two) -> i64 {
    match e {
        .a(t) => { t.release(); return 0; }
        .b(n) => { return n; }
    }
}

fn leaked() {
    let e: Two = .a(Token.make(1));
    match &e {
        .a(_) => {}
        .b(_) => {}
    }
}
";
    expect(
        "enum2",
        src,
        &["E1003 `e` is not consumed; no consuming function `e` scope_end=`}`"],
    );
}

#[test]
fn a_consuming_for_of_linear_elements_is_consumed_at_the_normal_exit() {
    let src = "\
linear struct Token { id: i32 }

impl Token {
    fn make(id: i32) -> Token { return Token.{ id: id }; }
    fn release(self: Token) { let Token.{ id: _ } = self; }
}

fn finished() {
    let ts = [2]Token.{ Token.make(1), Token.make(2) };
    for t in ts {
        t.release();
    }
}

fn early() {
    let ts = [2]Token.{ Token.make(1), Token.make(2) };
    for t in ts {
        t.release();
        break;
    }
}
";
    let analysis = analyze_main("linarray", src);
    assert_unchecked(&analysis);
    assert_eq!(
        e10(&analysis),
        Vec::<String>::new(),
        "other diagnostics: {:?}",
        others(&analysis)
    );
    let codes: Vec<&str> = analysis.diagnostics.iter().map(|d| d.code).collect();
    assert!(
        codes.contains(&"E0613"),
        "an early break of a linear consuming for is E0613, got {codes:?}"
    );
}

#[test]
fn an_fnonce_closure_reports_a_leaked_capture() {
    let src = "\
linear struct Token { id: i32 }

impl Token {
    fn make(id: i32) -> Token { return Token.{ id: id }; }
    fn release(self: Token) { let Token.{ id: _ } = self; }
}

fn f() {
    let a = Token.make(1);
    let b = Token.make(2);
    let c = fn [a, b]() { a.release(); };
    c();
}

fn both() {
    let a = Token.make(1);
    let b = Token.make(2);
    let c = fn [a, b]() {
        a.release();
        b.release();
    };
    c();
}
";
    expect(
        "once2",
        src,
        &["E1003 `b` is not consumed; consuming functions: release `b` scope_end=`}`"],
    );
}

#[test]
fn a_second_use_after_reassign_is_its_own_e1001() {
    expect(
        "twice",
        "\
struct Buf { len: i64 }

fn take(b: Buf) -> i64 { return b.len; }

fn f() -> i64 {
    var b = Buf.{ len: 1 };
    let n = take(b);
    let m = b.len;
    b = Buf.{ len: 2 };
    let k = take(b);
    return n + m + k + b.len;
}
",
        &[
            "E1001 was moved `b.len` move=`b`",
            "E1001 was moved `b.len` move=`b`",
        ],
    );
}

#[test]
fn an_e1003_fix_follows_the_whole_let() {
    let src = "\
linear struct Token { id: i32 }

impl Token {
    fn make(id: i32) -> Token { return Token.{ id: id }; }
    fn release(self: Token) { let Token.{ id: _ } = self; }
}

fn f() -> i32 {
    let t = Token.make(
        1);
    return t.id;
}
";
    let analysis = analyze_main("multiline", src);
    assert_unchecked(&analysis);
    assert_eq!(
        e10(&analysis),
        vec![
            "E1003 `t` is not consumed `t` scope_end=`return t.id;` fix=`\n    defer { t.release(); }`"
                .to_string(),
        ],
        "other diagnostics: {:?}",
        others(&analysis)
    );
    let edit = analysis
        .diagnostics
        .iter()
        .find(|d| d.code == "E1003")
        .and_then(|d| d.fixes.first())
        .and_then(|f| f.edits.first())
        .expect("fix");
    let at = edit.span.start;
    assert_eq!(
        src.as_bytes().get(at - 1).copied(),
        Some(b';'),
        "the defer must follow the let statement, not its first line"
    );
}

#[test]
fn fn_and_fnmut_are_discard_in_sema() {
    expect_clean(
        "fnbound",
        "\
fn g[F: Fn(i64) -> i64](f: F) -> i64 {
    let n = f(1);
    _ = f;
    return n;
}

fn h[F: FnMut(i64) -> i64](f: F) {
    _ = f;
}
",
    );
}

#[test]
fn copy_option_and_copy_array_are_not_consumed() {
    expect_clean(
        "optcopy",
        "\
fn f(o: ?i64) -> i64 {
    match o {
        .some(v) => { return v + (o orelse 0); }
        .none => { return 0; }
    }
}

fn g(o: ?i64) -> i64 {
    let a = o orelse 1;
    let b = o orelse 2;
    return a + b;
}

fn h(xs: [3]i64) -> i64 {
    var s: i64 = 0;
    for x in xs {
        s = s + x;
    }
    return s + xs[0];
}
",
    );
}
