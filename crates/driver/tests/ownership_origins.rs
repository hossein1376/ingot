//! Analysis B (origin sets) and the function boundary, on source, through the pipeline.
//!
//! Each hard case has a rejecting example and, where the case has an accepting twin,
//! an accepting one. Assertions name the code, the primary span's text, the related
//! labels and the fix edit.

use ingot_diag::source::Span;
use ingot_driver::pipeline::{self, Analysis, UNCHECKED_CHECK_REMOVAL};

fn analyze(name: &str, files: &[(&str, &str)]) -> Analysis {
    let dir = std::env::temp_dir().join(format!(
        "ingot-ori-{}-{}-{name}",
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
            .any(|m| m == UNCHECKED_CHECK_REMOVAL),
        "phase 10 must keep the check-removal entry, got {:?}\nE12: {:?}\nother: {:?}",
        analysis.not_implemented,
        e12(analysis),
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

/// `CODE message \`span\` role=\`span\` ...` plus the first fix edit.
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

fn e12(analysis: &Analysis) -> Vec<String> {
    analysis
        .diagnostics
        .iter()
        .filter(|d| d.code.starts_with("E12"))
        .map(|d| fmt_diag(analysis, d))
        .collect()
}

fn others(analysis: &Analysis) -> Vec<String> {
    analysis
        .diagnostics
        .iter()
        .filter(|d| !d.code.starts_with("E12"))
        .map(|d| format!("{}: {}", d.code, d.message))
        .collect()
}

fn expect(name: &str, source: &str, diags: &[&str]) {
    let analysis = analyze_main(name, source);
    assert_unchecked(&analysis);
    let extra: Vec<&String> = analysis
        .not_implemented
        .iter()
        .filter(|m| *m != UNCHECKED_CHECK_REMOVAL)
        .collect();
    assert!(
        extra.is_empty(),
        "origin analysis gave up: {extra:?}\nE12: {:?}\nother: {:?}",
        e12(&analysis),
        others(&analysis)
    );
    assert_eq!(
        e12(&analysis),
        diags.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
        "other diagnostics: {:?}",
        others(&analysis)
    );
    assert!(
        others(&analysis).is_empty(),
        "unexpected diagnostics: {:?}",
        others(&analysis)
    );
}

const PARSER: &str = "\
struct Parser { src: []u8, pos: i64 }
";

const WINDOW: &str = "\
struct Window { buf: []mut u8 }
";

const LEXER: &str = "\
struct Lexer { src: []u8, pos: i64 }
";

#[test]
fn inner_slice_and_field_reborrow_are_distinct() {
    expect(
        "rest",
        &format!(
            "{PARSER}
impl Parser {{
    fn rest(self: &Parser) -> []u8 from self.* {{
        return self.src[self.pos..self.src.len];
    }}
    fn pos_mut(self: &mut Parser) -> &mut i64 from self {{
        return &mut self.pos;
    }}
}}
"
        ),
        &[],
    );
    expect(
        "take",
        &format!(
            "{WINDOW}
impl Window {{
    fn head(self: &mut Window) -> []mut u8 from self.* {{
        return self.buf[0..1];
    }}
}}
"
        ),
        &[
            "E1202 the returned borrow is not covered by the `from` clause `return self.buf[0..1];` signature=`self.*` fix=`, self`",
        ],
    );
}

#[test]
fn local_borrow_and_forwarded_parameter() {
    expect(
        "local-ref",
        "\
fn f() -> &i64 from static {
    let x: i64 = 1;
    return &x;
}
",
        &["E1203 the returned value borrows a local `return &x;` borrow=`&x`"],
    );
    expect(
        "local-slice",
        "\
fn f() -> []u8 from static {
    let a: [4]u8 = @zero;
    return &a[..];
}
",
        &["E1203 the returned value borrows a local `return &a[..];` borrow=`a[..]`"],
    );
    expect(
        "forward",
        "\
fn f(p: []u8) -> []u8 from p {
    let s = p;
    return s[0..1];
}
",
        &[],
    );
}

#[test]
fn joined_origins_need_every_source() {
    expect(
        "both",
        "\
fn pick(c: bool, a: []u8, b: []u8) -> []u8 from a, b {
    if c { return a; }
    return b;
}
",
        &[],
    );
    expect(
        "one",
        "\
fn pick(c: bool, a: []u8, b: []u8) -> []u8 from a {
    if c { return a; }
    return b;
}
",
        &[
            "E1202 the returned borrow is not covered by the `from` clause `return b;` signature=`a` fix=`, b`",
        ],
    );
}

#[test]
fn static_origin_covers_literals_and_statics() {
    expect(
        "stat",
        "\
static n: i64 = 1;

fn greeting() -> []u8 from static { return \"hello\"; }

fn item() -> &i64 from static { return &n; }
",
        &[],
    );
}

#[test]
fn store_into_mut_parameter_needs_into() {
    expect(
        "need",
        &format!(
            "{LEXER}
fn set_input(l: &mut Lexer, text: []u8) {{
    l.src = text;
    l.pos = 0;
}}

impl Lexer {{
    fn set_src(self: &mut Lexer, text: []u8) {{
        self.src = text;
    }}
}}
"
        ),
        &[
            "E1204 the view from `text` is stored where it reaches `l` `l.src = text;` signature=`text` fix=` into l`",
            "E1204 the view from `text` is stored where it reaches `self` `self.src = text;` signature=`text` fix=` into self`",
        ],
    );
    expect(
        "has",
        &format!(
            "{LEXER}
fn set_input(l: &mut Lexer, text: []u8 into l) {{
    l.src = text;
    l.pos = 0;
}}

impl Lexer {{
    fn set_src(self: &mut Lexer, text: []u8 into self) {{
        self.src = text;
    }}
}}
"
        ),
        &[],
    );
}

#[test]
fn local_borrow_stored_into_parameter() {
    expect(
        "esc",
        &format!(
            "{LEXER}
fn f(l: &mut Lexer) {{
    let local: [4]u8 = @zero;
    l.src = &local[0..4];
}}
"
        ),
        &[
            "E1205 a borrow of a local is stored where it reaches a parameter `l.src = &local[0..4];` borrow=`local[0..4]`",
        ],
    );
}

#[test]
fn type_parameter_flow_permits_the_generic_store() {
    expect(
        "gen",
        "\
struct Slot[T] { v: T }

fn put[T: Discard](s: &mut Slot[T], x: T) {
    s.v = x;
}
",
        &[],
    );
    expect(
        "mono",
        "\
struct Words { v: []u8 }

fn put(s: &mut Words, x: []u8) {
    s.v = x;
}
",
        &[
            "E1204 the view from `x` is stored where it reaches `s` `s.v = x;` signature=`x` fix=` into s`",
        ],
    );
}

#[test]
fn call_results_follow_the_clause_and_a_value_unions_everything() {
    expect(
        "calls",
        "\
struct Parser { src: []u8, pos: i64 }

impl Parser {
    fn rest(self: &Parser) -> []u8 from self.* {
        return self.src[0..self.src.len];
    }
}

fn id(p: []u8) -> []u8 from p { return p; }

fn greet() -> []u8 from static { return \"hi\"; }

fn pass(a: []u8) -> []u8 from a { return id(a); }

fn deeper(p: &Parser) -> []u8 from p.* { return p.rest(); }

fn hi() -> []u8 from static { return greet(); }

fn id_t[T](x: T) -> T { return x; }

fn pass_t(a: []u8) -> []u8 from a { return id_t(a); }
",
        &[],
    );
    expect(
        "value",
        "\
fn both(a: []u8, b: []u8) -> []u8 from a {
    let g = fn [a, b](flag: bool) -> []u8 {
        if flag { return a; }
        return b;
    };
    return g(true);
}
",
        &[
            "E1202 the returned borrow is not covered by the `from` clause `return g(true);` signature=`a` fix=`, b`",
        ],
    );
    expect(
        "value-ok",
        "\
fn both(a: []u8, b: []u8) -> []u8 from a, b {
    let g = fn [a, b](flag: bool) -> []u8 {
        if flag { return a; }
        return b;
    };
    return g(true);
}
",
        &[],
    );
}

#[test]
fn cursor_loop_kills_the_old_loan() {
    expect(
        "cursor",
        "\
struct Node { nxt: &mut Node }

fn walk(head: &mut Node, steps: i64) {
    var cur = head;
    var i = 0;
    while i < steps {
        let n = &mut cur.nxt.*;
        cur = n;
        i = i + 1;
    }
}
",
        &[],
    );
}

#[test]
fn closure_may_return_a_capture_but_not_its_own_local() {
    expect(
        "cap",
        "\
fn f(p: &i64) -> i64 {
    let c = fn [p]() -> &i64 { return p; };
    _ = c;
    return 0;
}
",
        &[],
    );
    expect(
        "own",
        "\
fn f() -> i64 {
    let c = fn []() -> &i64 {
        let x = 1;
        return &x;
    };
    _ = c;
    return 0;
}
",
        &["E1203 the returned value borrows a local `return &x;` borrow=`&x`"],
    );
    expect(
        "cross",
        "\
struct Lexer { src: []u8, pos: i64 }

fn f(a: []u8) -> i64 {
    var buf = Lexer.{ src: \"x\", pos: 0 };
    let c = fn [&a, &mut buf]() {
        buf.*.src = a.*;
    };
    _ = c;
    return 0;
}
",
        &["E1204 the view from `a` is stored into capture `buf` `buf.*.src = a.*;` signature=`a`"],
    );
}

#[test]
fn builtins_give_the_sets_in_the_call_rules() {
    expect(
        "replace-ok",
        "\
struct Hold { v: []u8 }

fn take(b: &mut Hold) -> []u8 from b.* {
    return @replace(&mut b.v, \"x\");
}

fn trade(a: &mut Hold into b, b: &mut Hold into a) {
    @swap(&mut a.v, &mut b.v);
}

fn raw(p: *u8) -> []u8 from static {
    unsafe(\"p is a live slice of one byte\") {
        return @slice_from_raw(p, 1);
    }
}

fn borrowed(p: *u8) -> &u8 from static {
    unsafe(\"p is live\") {
        return @borrow_from_raw(p);
    }
}

fn cast(s: []u8) -> []u8 from static {
    let p = s.ptr;
    unsafe(\"p still addresses s\") {
        return @slice_from_raw(p, s.len);
    }
}

fn bits_of(n: i64) -> i64 {
    return @bits(i64, n);
}

fn dropped(b: &mut Hold) {
    let old = @replace(&mut b.v, \"y\");
    @forget(old);
}
",
        &[],
    );
    expect(
        "replace-inner",
        "\
struct Hold { v: []u8 }

fn take(b: &mut Hold) -> []u8 from static {
    return @replace(&mut b.v, \"x\");
}
",
        &[
            "E1202 the returned borrow is not covered by the `from` clause `return @replace(&mut b.v, \"x\");` signature=`static` fix=`, b.*`",
        ],
    );
}

#[test]
fn take_does_not_store_the_reference_into_itself() {
    expect(
        "take",
        "\
linear struct Token { id: i32 }

impl Token {
    fn make(id: i32) -> Token { return Token.{ id: id }; }
    fn release(self: Token) { let Token.{ id: _ } = self; }
}

fn f(slot: &mut ?Token) {
    let taken = slot.take();
    match taken {
        .some(v) => { v.release(); }
        .none => {}
    }
}
",
        &[],
    );
}

#[test]
fn missing_from_is_reported_at_the_return_type() {
    expect(
        "miss",
        "\
fn first(s: []u8) -> []u8 {
    return s[0..1];
}
",
        &["E1201 the return type holds a borrow and needs a `from` clause `[]u8` fix=` from s`"],
    );
    expect(
        "lit",
        "\
fn greeting() -> []u8 {
    return \"hello\";
}
",
        &[
            "E1201 the return type holds a borrow and needs a `from` clause `[]u8` fix=` from static`",
        ],
    );
    expect(
        "iface",
        "\
interface Src {
    fn read(self: &Self) -> []u8;
}

fn keep() -> i64 { return 0; }
",
        &["E1201 the return type holds a borrow and needs a `from` clause `[]u8`"],
    );
}

#[test]
fn loan_on_a_deref_reads_as_inner() {
    expect(
        "inner-ok",
        "\
struct S { v: []u8 }

fn through(p: &mut S) -> []u8 from p.* {
    let r = &mut p.*;
    return r.v;
}
",
        &[],
    );
    expect(
        "inner-static",
        "\
struct S { v: []u8 }

fn through(p: &mut S) -> []u8 from static {
    let r = &mut p.*;
    return r.v;
}
",
        &[
            "E1202 the returned borrow is not covered by the `from` clause `return r.v;` signature=`static` fix=`, p.*`",
        ],
    );
}

#[test]
fn type_parameter_outside_a_borrow() {
    expect(
        "get-ok",
        "\
struct List[T] { item: T }

impl[T] List[T] {
    fn get(self: &List[T]) -> &T from self {
        return &self.item;
    }
}
",
        &[],
    );
    expect(
        "get-miss",
        "\
struct List[T] { item: T }

impl[T] List[T] {
    fn get(self: &List[T]) -> &T {
        return &self.item;
    }
}
",
        &["E1201 the return type holds a borrow and needs a `from` clause `&T` fix=` from self`"],
    );
    expect(
        "set",
        "\
struct Holder[T] { r: &T }

fn set[T](h: &mut Holder[T], x: &T) {
    h.r = x;
}
",
        &[
            "E1204 the view from `x` is stored where it reaches `h` `h.r = x;` signature=`x` fix=` into h`",
        ],
    );
    expect(
        "set-ok",
        "\
struct Holder[T] { r: &T }

fn set[T](h: &mut Holder[T], x: &T into h) {
    h.r = x;
}
",
        &[],
    );
}

#[test]
fn slice_mut_widens_at_the_call() {
    expect(
        "fill",
        "\
fn fill[T: Copy](xs: []mut T, v: T) {
    xs[0] = v;
}

fn use_fill(xs: []mut []u8, v: []u8) {
    fill(xs, v);
}
",
        &[
            "E1204 the view from `v` is stored where it reaches `xs` `fill(xs, v);\n` signature=`v` fix=` into xs`",
        ],
    );
    expect(
        "fill-ok",
        "\
fn fill[T: Copy](xs: []mut T, v: T) {
    xs[0] = v;
}

fn use_fill(xs: []mut []u8, v: []u8 into xs) -> []mut []u8 from xs, v {
    fill(xs, v);
    return xs;
}
",
        &[],
    );
    expect(
        "fill-miss",
        "\
fn fill[T: Copy](xs: []mut T, v: T) {
    xs[0] = v;
}

fn use_fill(xs: []mut []u8, v: []u8 into xs) -> []mut []u8 from xs {
    fill(xs, v);
    return xs;
}
",
        &[],
    );
}

#[test]
fn slice_index_is_a_dereference() {
    expect(
        "put",
        "\
fn put(xs: []mut []u8, v: []u8) {
    xs[0] = v;
}
",
        &[
            "E1204 the view from `v` is stored where it reaches `xs` `xs[0] = v;` signature=`v` fix=` into xs`",
        ],
    );
    expect(
        "put-ok",
        "\
fn put(xs: []mut []u8, v: []u8 into xs) {
    xs[0] = v;
}
",
        &[],
    );
    expect(
        "local",
        "\
fn local_into_param(xs: []mut []u8) {
    let a = [2]u8.{ 1, 2 };
    xs[0] = &a[0..2];
}
",
        &[
            "E1205 a borrow of a local is stored where it reaches a parameter `xs[0] = &a[0..2];` borrow=`a[0..2]`",
        ],
    );
    expect(
        "idx-ok",
        "\
fn first(s: []u8) -> &u8 from s {
    return &s[0];
}

fn tail(s: []u8) -> []u8 from s {
    return s[1..s.len];
}

fn arr(a: &[4]u8) -> []u8 from a {
    return &a[..];
}
",
        &[],
    );
}

#[test]
fn fail_uses_the_error_type() {
    expect(
        "parse",
        "\
struct Bad { text: []u8 }

fn parse(s: []u8) -> i64 ! Bad {
    if s.len == 0 {
        fail Bad.{ text: s };
    }
    return 1;
}
",
        &["E1201 the return type holds a borrow and needs a `from` clause `i64` fix=` from s`"],
    );
    expect(
        "local-fail",
        "\
struct Bad { text: []u8 }

fn local_fail() -> i64 ! Bad from static {
    let a = [2]u8.{ 1, 2 };
    fail Bad.{ text: &a[0..2] };
}
",
        &[
            "E1203 the returned value borrows a local `fail Bad.{ text: &a[0..2] };` borrow=`a[0..2]`",
        ],
    );
}

#[test]
fn closure_capture_flows_through_a_call() {
    expect(
        "call",
        "\
struct Lexer { src: []u8, pos: i64 }

fn id(x: []u8) -> []u8 from x {
    return x;
}

fn cross_call(a: []u8) -> i64 {
    var buf = Lexer.{ src: \"x\", pos: 0 };
    let c = fn [&a, &mut buf]() {
        buf.*.src = id(a.*);
    };
    _ = c;
    return 0;
}
",
        &[
            "E1204 the view from `a` is stored into capture `buf` `buf.*.src = id(a.*);` signature=`a`",
        ],
    );
    expect(
        "call-ok",
        "\
struct Lexer { src: []u8, pos: i64 }

fn cross_ok(a: []u8) -> i64 {
    var buf = Lexer.{ src: \"x\", pos: 0 };
    let c = fn [&a, &mut buf]() {
        buf.*.pos = 1;
    };
    _ = c;
    return 0;
}
",
        &[],
    );
}

#[test]
fn closure_creation_stores_through_the_capture() {
    expect(
        "hole",
        "\
struct Lexer { src: []u8, pos: i64 }

fn hole(h: &mut Lexer) -> []u8 from h {
    var target = h;
    let c = fn [&mut target]() {
        target.*.pos = 1;
    };
    _ = c;
    return target.src;
}
",
        // One capture: nothing of another capture is stored through `target`, so the
        // returned `target.src` is `h`'s (checker.md §4.6, "into another").
        &[],
    );
    expect(
        "local",
        "\
struct Lexer { src: []u8, pos: i64 }

fn local_buf(a: []u8) -> i64 {
    var buf = Lexer.{ src: \"x\", pos: 0 };
    let c = fn [&a, &mut buf]() {
        buf.*.pos = 1;
    };
    _ = c;
    return 0;
}
",
        &[],
    );
}

#[test]
fn two_borrows_in_one_store_stay_distinct() {
    expect(
        "two",
        "\
fn two(p: &mut (&i64, &i64)) -> i64 {
    let a = 1;
    let b = 2;
    p.* = (&a, &b);
    return 0;
}
",
        &[
            "E1205 a borrow of a local is stored where it reaches a parameter `p.* = (&a, &b);` borrow=`&a` borrow=`&b`",
        ],
    );
    expect(
        "kept",
        "\
fn kept() -> i64 {
    let a = 1;
    let b = 2;
    let t = (&a, &b);
    return t.0.* + t.1.*;
}
",
        &[],
    );
}
