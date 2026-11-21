//! F1: the formatter's goldens, token preservation, idempotence and the repository
//! check.

use ingot_syntax::fmt::{format_text, verify};
use std::fs;
use std::path::{Path, PathBuf};

fn case(name: &str, before: &str, after: &str) {
    let formatted = format_text(before).unwrap_or_else(|error| panic!("{name}: {error:?}"));
    verify(before, &formatted).unwrap_or_else(|error| panic!("{name}: {error:?}"));
    assert_eq!(formatted, after, "{name}");
    let again = format_text(&formatted).unwrap();
    assert_eq!(again, formatted, "{name} is not idempotent");
}

#[test]
fn the_impl_example() {
    case(
        "impl",
        "impl Point { pub fn norm2(self: &Point)->i64 { let x=self.x*self.x; return x+self.y*self.y; } }\n",
        "impl Point {\n    pub fn norm2(self: &Point) -> i64 {\n        let x = self.x * self.x;\n        return x + self.y * self.y;\n    }\n}\n",
    );
}

#[test]
fn a_long_parameter_list_breaks() {
    case(
        "params",
        "pub fn alloc_slice[T: Copy](self: &Arena, n: i64, fill: T) -> []mut T ! AllocErr from self { return self.alloc(n); }\n",
        "pub fn alloc_slice[T: Copy](\n    self: &Arena,\n    n: i64,\n    fill: T,\n) -> []mut T ! AllocErr from self {\n    return self.alloc(n);\n}\n",
    );
}

#[test]
fn a_long_result_breaks_the_parameters_first() {
    case(
        "result",
        "fn tally(arena: &Arena, words: &List[[]u8, &Arena]) -> List[Entry, &Arena] ! AllocErr from arena { return f(); }\n",
        "fn tally(\n    arena: &Arena,\n    words: &List[[]u8, &Arena],\n) -> List[Entry, &Arena] ! AllocErr from arena {\n    return f();\n}\n",
    );
    // Generic parameters still break only after the parameters.
    case(
        "result_generics",
        "fn alloc_or_return[T](self: &FixedBuf, value: T) -> &mut T ! AllocFail[T] from self { return f(); }\n",
        "fn alloc_or_return[T](\n    self: &FixedBuf,\n    value: T,\n) -> &mut T ! AllocFail[T] from self {\n    return f();\n}\n",
    );
}

#[test]
fn a_comment_in_the_body_stays_in_the_body_of_a_broken_signature() {
    // The closing parenthesis of the parameters must not be matched with the last `)`
    // of the body (`f(b)` here), which would place the comment before it.
    case(
        "comment_after_params",
        "fn spawn_one(state: *mut State, start: fn(*mut u8), args: *u8, args_size: i64) -> i64 {\n    g();\n    // why the next line\n    return f(args_size);\n}\n",
        "fn spawn_one(\n    state: *mut State,\n    start: fn(*mut u8),\n    args: *u8,\n    args_size: i64,\n) -> i64 {\n    g();\n    // why the next line\n    return f(args_size);\n}\n",
    );
}

#[test]
fn an_operator_chain_breaks_before_the_operator() {
    case(
        "operator",
        "fn f() { let total = header.len + payload.len + checksum_bytes(header.kind, payload.len); }\n",
        "fn f() {\n    let total = header.len\n        + payload.len\n        + checksum_bytes(header.kind, payload.len);\n}\n",
    );
}

#[test]
fn a_method_chain_breaks_before_each_dot() {
    case(
        "method",
        "fn f() { let counted = very_long_variable_name.split_on_spaces_and_tabs().filter_out_the_empty_pieces().count(); }\n",
        "fn f() {\n    let counted = very_long_variable_name\n        .split_on_spaces_and_tabs()\n        .filter_out_the_empty_pieces()\n        .count();\n}\n",
    );
}

#[test]
fn type_declarations_always_span_several_lines() {
    case(
        "struct",
        "pub struct Point { pub x: i64, pub y: i64 }\n",
        "pub struct Point {\n    pub x: i64,\n    pub y: i64,\n}\n",
    );
    case(
        "long struct",
        "pub struct Long { pub first_field_with_a_long_name: i64, pub second_field_with_a_long_name: i64 }\n",
        "pub struct Long {\n    pub first_field_with_a_long_name: i64,\n    pub second_field_with_a_long_name: i64,\n}\n",
    );
    case(
        "enum",
        "enum Shape: u8 { empty, circle: i64, square = 3 }\n",
        "enum Shape: u8 {\n    empty,\n    circle: i64,\n    square = 3,\n}\n",
    );
    case(
        "one field",
        "struct Wrapper[T] { value: T }\n",
        "struct Wrapper[T] {\n    value: T,\n}\n",
    );
    case(
        "empty",
        "struct Empty { }\nenum Never {}\n",
        "struct Empty {}\nenum Never {}\n",
    );
}

#[test]
fn a_struct_or_enum_keeps_the_authors_blank_lines() {
    case(
        "struct",
        "struct Pair {\n\n    a: i64,\n\n\n    b: i64,\n    c: i64,\n\n}\n",
        "struct Pair {\n    a: i64,\n\n    b: i64,\n    c: i64,\n}\n",
    );
    case(
        "enum",
        "enum Kind {\n    a,\n    b,\n\n    // the rest\n    c: i64,\n}\n",
        "enum Kind {\n    a,\n    b,\n\n    // the rest\n    c: i64,\n}\n",
    );
}

#[test]
fn blank_lines_are_kept_in_function_bodies_and_interfaces() {
    case(
        "body",
        "fn f() {\n    let a = 1;\n\n    g(a);\n\n\n    @assert(a == 1);\n\n    h();\n    return;\n}\n",
        "fn f() {\n    let a = 1;\n\n    g(a);\n\n    @assert(a == 1);\n\n    h();\n    return;\n}\n",
    );
    case(
        "nested block",
        "fn f() {\n    if x {\n        g();\n\n        h();\n    }\n\n    for i in xs {\n        g();\n\n        defer { h(); }\n    }\n}\n",
        "fn f() {\n    if x {\n        g();\n\n        h();\n    }\n\n    for i in xs {\n        g();\n\n        defer { h(); }\n    }\n}\n",
    );
    case(
        "interface",
        "interface Draw {\n    fn a(self: &Self);\n\n    fn b(self: &Self);\n    fn c(self: &Self);\n\n\n    type T;\n}\n",
        "interface Draw {\n    fn a(self: &Self);\n\n    fn b(self: &Self);\n    fn c(self: &Self);\n\n    type T;\n}\n",
    );
    case(
        "impl",
        "impl Point {\n    fn a(self: &Point) {}\n\n    fn b(self: &Point) {}\n}\n",
        "impl Point {\n    fn a(self: &Point) {}\n\n    fn b(self: &Point) {}\n}\n",
    );
    case(
        "extern",
        "extern \"c\" {\n    fn a() -> i32;\n\n    fn b() -> i32;\n    fn c() -> i32;\n}\n",
        "extern \"c\" {\n    fn a() -> i32;\n\n    fn b() -> i32;\n    fn c() -> i32;\n}\n",
    );
}

#[test]
fn a_cleanup_or_handler_block_with_one_statement_stays_on_one_line() {
    case(
        "defer",
        "fn f() { defer { buf.free(); }\n errdefer {\n buf.free();\n } }\n",
        "fn f() {\n    defer { buf.free(); }\n    errdefer { buf.free(); }\n}\n",
    );
    case(
        "orelse and catch",
        "fn f() {\n    let n = parse(text) orelse {\n return .none;\n };\n    let m = parse(text) catch e => { return .none; };\n}\n",
        "fn f() {\n    let n = parse(text) orelse { return .none; };\n    let m = parse(text) catch e => { return .none; };\n}\n",
    );
    case(
        "match arm",
        "fn f() {\n    match order {\n        .less => {\n            return -1;\n        }\n        .equal => { return 0; }\n        else => { return 1; }\n    }\n}\n",
        "fn f() {\n    match order {\n        .less => { return -1; }\n        .equal => { return 0; }\n        else => { return 1; }\n    }\n}\n",
    );
    case(
        "closure",
        "fn f() {\n    xs.each(fn[scale, &mut total](x: i64) { total.* += x * scale; });\n}\n",
        "fn f() {\n    xs.each(fn [scale, &mut total](x: i64) { total.* += x * scale; });\n}\n",
    );
}

#[test]
fn a_block_that_does_not_fit_spans_several_lines() {
    case(
        "defer",
        "fn f() {\n    defer { buffer.release_everything_that_was_acquired_for_the_long_running_job(handle, flags); }\n}\n",
        "fn f() {\n    defer {\n        buffer.release_everything_that_was_acquired_for_the_long_running_job(\n            handle,\n            flags,\n        );\n    }\n}\n",
    );
    case(
        "orelse",
        "fn f() {\n    let o = parse(text) orelse { return something_with_a_long_name(first_argument, second_argument); };\n}\n",
        "fn f() {\n    let o = parse(text) orelse {\n        return something_with_a_long_name(first_argument, second_argument);\n    };\n}\n",
    );
    case(
        "match arm",
        "fn f() {\n    match order {\n        .greater => { return something_with_a_long_name(first_argument, second_argument, third); }\n    }\n}\n",
        "fn f() {\n    match order {\n        .greater => {\n            return something_with_a_long_name(\n                first_argument,\n                second_argument,\n                third,\n            );\n        }\n    }\n}\n",
    );
    case(
        "closure",
        "fn f() {\n    xs.each(fn [scale, &mut total, another_capture](x: i64) { total.* += x * scale + another_capture; });\n}\n",
        "fn f() {\n    xs.each(\n        fn [scale, &mut total, another_capture](x: i64) {\n            total.* += x * scale + another_capture;\n        },\n    );\n}\n",
    );
}

#[test]
fn a_block_with_several_statements_or_a_comment_or_a_block_stays_broken() {
    case(
        "two statements",
        "fn f() {\n    defer { a(); b(); }\n    xs.each(fn [a](x: i64) { g(x); h(x); });\n}\n",
        "fn f() {\n    defer {\n        a();\n        b();\n    }\n    xs.each(\n        fn [a](x: i64) {\n            g(x);\n            h(x);\n        },\n    );\n}\n",
    );
    case(
        "comment after the brace",
        "fn f() {\n    defer { // why\n        buf.free();\n    }\n}\n",
        "fn f() {\n    defer { // why\n        buf.free();\n    }\n}\n",
    );
    case(
        "comment after the statement",
        "fn f() {\n    let n = parse(text) orelse {\n        return .none; // nothing\n    };\n}\n",
        "fn f() {\n    let n = parse(text) orelse {\n        return .none; // nothing\n    };\n}\n",
    );
    case(
        "comment before the statement",
        "fn f() {\n    match x {\n        .a => {\n            // never\n            return;\n        }\n    }\n}\n",
        "fn f() {\n    match x {\n        .a => {\n            // never\n            return;\n        }\n    }\n}\n",
    );
    case(
        "statement with a block",
        "fn f() {\n    defer { if x { y(); } }\n    let k = parse(text) orelse { if x { return .none; } };\n}\n",
        "fn f() {\n    defer {\n        if x {\n            y();\n        }\n    }\n    let k = parse(text) orelse {\n        if x {\n            return .none;\n        }\n    };\n}\n",
    );
}

#[test]
fn an_unsafe_statement_block_follows_the_one_line_rule() {
    case(
        "fits",
        "fn f() {\n    unsafe(\"p points at a live i64\") {\n        @atomic_store(p, y, s);\n    }\n}\n",
        "fn f() {\n    unsafe(\"p points at a live i64\") { @atomic_store(p, y, s); }\n}\n",
    );
    case(
        "does not fit",
        "fn f() {\n    unsafe(\"a very long reason that takes most of the line up front\") { @atomic_store(p, yyyyyy, s); }\n}\n",
        "fn f() {\n    unsafe(\"a very long reason that takes most of the line up front\") {\n        @atomic_store(p, yyyyyy, s);\n    }\n}\n",
    );
    case(
        "several statements, a comment, a block",
        "fn f() {\n    unsafe(\"r\") { a(); b(); }\n    unsafe(\"r\") { // c\n        a();\n    }\n    unsafe(\"r\") { if x { a(); } }\n}\n",
        "fn f() {\n    unsafe(\"r\") {\n        a();\n        b();\n    }\n    unsafe(\"r\") { // c\n        a();\n    }\n    unsafe(\"r\") {\n        if x {\n            a();\n        }\n    }\n}\n",
    );
}

#[test]
fn a_select_arm_block_follows_the_one_line_rule() {
    case(
        "fits",
        "fn f() {\n    select {\n        v = try s.wait() => {\n            return v;\n        }\n        else => {\n            return 0;\n        }\n    }\n}\n",
        "fn f() {\n    select {\n        v = try s.wait() => { return v; }\n        else => { return 0; }\n    }\n}\n",
    );
    case(
        "does not fit, or has several statements",
        "fn f() {\n    select {\n        v = a_function_with_a_long_name(argument_one, argument_two) => { return v; }\n        w = s.get() => { g(); h(); }\n        else => { return 0; }\n    }\n}\n",
        "fn f() {\n    select {\n        v = a_function_with_a_long_name(argument_one, argument_two) => {\n            return v;\n        }\n        w = s.get() => {\n            g();\n            h();\n        }\n        else => { return 0; }\n    }\n}\n",
    );
}

#[test]
fn blank_lines_between_arms_are_kept() {
    case(
        "match",
        "fn f() {\n    match x {\n        .a => { g(); }\n\n        .b => 1,\n\n\n        .c => { g(); h(); }\n        else => { return; }\n    }\n}\n",
        "fn f() {\n    match x {\n        .a => { g(); }\n\n        .b => 1,\n\n        .c => {\n            g();\n            h();\n        }\n        else => { return; }\n    }\n}\n",
    );
    case(
        "select",
        "fn f() {\n    select {\n\n        v = s.wait() => { return v; }\n\n        else => { return 0; }\n\n    }\n}\n",
        "fn f() {\n    select {\n        v = s.wait() => { return v; }\n\n        else => { return 0; }\n    }\n}\n",
    );
}

#[test]
fn string_literals_are_copied_from_the_source() {
    case(
        "test name and unsafe reason",
        "test \"a\\x41 \\\"q\\\" é\" {\n    unsafe(\"it's \\\"ok\\\" \\x41\") {\n        let x = 1;\n    }\n}\n",
        "test \"a\\x41 \\\"q\\\" é\" {\n    unsafe(\"it's \\\"ok\\\" \\x41\") { let x = 1; }\n}\n",
    );
    case(
        "bench name, unsafe expression and extern",
        "bench \"b\\tq\" { let v = unsafe(\"r\\\\s\") { p.* }; }\nextern \"c\" { fn f(); }\n",
        "bench \"b\\tq\" {\n    let v = unsafe(\"r\\\\s\") { p.* };\n}\nextern \"c\" {\n    fn f();\n}\n",
    );
}

#[test]
fn a_comment_after_a_comma_stays_after_that_item() {
    case(
        "parameters",
        "fn g(a: i64, // first\n    b: i64) -> i64 { return a; }\n",
        "fn g(\n    a: i64, // first\n    b: i64,\n) -> i64 {\n    return a;\n}\n",
    );
    case(
        "arguments",
        "fn h() {\n    foo(1, // one\n        2);\n}\n",
        "fn h() {\n    foo(\n        1, // one\n        2,\n    );\n}\n",
    );
    case(
        "struct literal",
        "fn h() {\n    let p = Point.{ x: 1, // the x\n        y: 2 };\n}\n",
        "fn h() {\n    let p = Point.{\n        x: 1, // the x\n        y: 2,\n    };\n}\n",
    );
    case(
        "tuple, generics, last item",
        "fn h() {\n    let t = (1, // a\n        2); // b\n    let u = f.[A, // c\n        B](x, // d\n        y, // e\n    );\n}\n",
        "fn h() {\n    let t = (\n        1, // a\n        2,\n    ); // b\n    let u = f.[\n        A, // c\n        B,\n    ](\n        x, // d\n        y, // e\n    );\n}\n",
    );
}

#[test]
fn a_comment_after_a_semicolon_stays_on_its_line() {
    case(
        "items",
        "import std.Env; // ci\nconst x: i64 = 1; // cx\nstatic y: i64 = 2; // cy\ntype T = i64; // ct\n",
        "import std.Env; // ci\nconst x: i64 = 1; // cx\nstatic y: i64 = 2; // cy\ntype T = i64; // ct\n",
    );
    case(
        "members",
        "extern \"c\" { // open\n    fn a() -> i32; // ca\n    fn b() -> i32; // cb\n}\ninterface I {\n    fn m(self: &Self); // cm\n\n    fn n(self: &Self); // cn\n}\n",
        "extern \"c\" { // open\n    fn a() -> i32; // ca\n    fn b() -> i32; // cb\n}\ninterface I {\n    fn m(self: &Self); // cm\n\n    fn n(self: &Self); // cn\n}\n",
    );
    case(
        "when and select",
        "when @target.os == .linux { // lin\n    fn a() {} // ca\n    // end\n} else {\n    fn b() {}\n}\nfn f() {\n    select { // s\n        v = s.wait() => { return v; } // sw\n    } // after\n}\n",
        "when @target.os == .linux { // lin\n    fn a() {} // ca\n    // end\n} else {\n    fn b() {}\n}\nfn f() {\n    select { // s\n        v = s.wait() => { return v; } // sw\n    } // after\n}\n",
    );
}

#[test]
fn a_blank_line_after_an_import_with_a_comment_is_kept() {
    case(
        "same-line comment",
        "import std.Env; // why\n\nfn f() {}\n",
        "import std.Env; // why\n\nfn f() {}\n",
    );
    case(
        "blank line before a comment",
        "import std.Env;\n\n// doc\nfn f() {}\n",
        "import std.Env;\n\n// doc\nfn f() {}\n",
    );
    case(
        "comment on the next line",
        "import std.Env;\n// doc\nfn f() {}\n",
        "import std.Env;\n// doc\nfn f() {}\n",
    );
}

#[test]
fn a_comment_between_a_brace_and_else_stays_after_else() {
    case(
        "else",
        "fn f() {\n    if a {\n        return a;\n    } // after then\n    else {\n        return b;\n    }\n}\n",
        "fn f() {\n    if a {\n        return a;\n    } else { // after then\n        return b;\n    }\n}\n",
    );
}

#[test]
fn a_comment_in_a_tuple_forces_it_to_break() {
    case(
        "tuple",
        "fn h() {\n    let t = (1, // a\n        2);\n}\n",
        "fn h() {\n    let t = (\n        1, // a\n        2,\n    );\n}\n",
    );
}

#[test]
fn no_other_block_is_one_line() {
    case(
        "if while for function plain",
        "fn f() { if x { g(); } while x { g(); } for i in xs { g(); } { g(); } }\n",
        "fn f() {\n    if x {\n        g();\n    }\n    while x {\n        g();\n    }\n    for i in xs {\n        g();\n    }\n    {\n        g();\n    }\n}\n",
    );
}

#[test]
fn the_last_list_of_a_construct_breaks_first() {
    case(
        "max heap",
        "fn f() { var heap = MaxHeap[i64, 8].{ items: [8]i64.{ 0, 0, 0, 0, 0, 0, 0, 0 }, len: 0 }; }\n",
        "fn f() {\n    var heap = MaxHeap[i64, 8].{\n        items: [8]i64.{ 0, 0, 0, 0, 0, 0, 0, 0 },\n        len: 0,\n    };\n}\n",
    );
    case(
        "generic parameters and parameters",
        "pub fn insertion_sort_by[T: Copy, F: FnMut(&T, &T) -> bool](xs: []mut T, before: F) {}\n",
        "pub fn insertion_sort_by[T: Copy, F: FnMut(&T, &T) -> bool](\n    xs: []mut T,\n    before: F,\n) {}\n",
    );
    case(
        "earlier list breaks when the line still does not fit",
        "pub fn insertion_sort_by_with_a_very_long_name[Element: Copy + Ord, Comparator: FnMut(&Element, &Element) -> bool](xs: []mut Element) {}\n",
        "pub fn insertion_sort_by_with_a_very_long_name[\n    Element: Copy + Ord,\n    Comparator: FnMut(&Element, &Element) -> bool,\n](xs: []mut Element) {}\n",
    );
}

#[test]
fn a_closure_is_spaced_fn_captures_parameters() {
    case(
        "captures",
        "fn f() { let g = fn[x](a: i64) -> i64 { return a + x; }; }\n",
        "fn f() {\n    let g = fn [x](a: i64) -> i64 { return a + x; };\n}\n",
    );
    case(
        "no captures",
        "fn f() { let g = fn  [ ](a: i64) { h(a); }; }\n",
        "fn f() {\n    let g = fn [](a: i64) { h(a); };\n}\n",
    );
}

#[test]
fn comments_stay_with_their_lines() {
    case(
        "comments",
        "/// doc\npub fn f() -> i64 {\n    let x = 1; //~ E0001\n    return x;\n}\n",
        "/// doc\npub fn f() -> i64 {\n    let x = 1; //~ E0001\n    return x;\n}\n",
    );
    case(
        "field comments",
        "pub struct Documented {\n    /// x\n    pub x: i64,\n    pub y: i64, // trailing\n}\n",
        "pub struct Documented {\n    /// x\n    pub x: i64,\n    pub y: i64, // trailing\n}\n",
    );
}

#[test]
fn a_call_that_starts_mid_line_breaks_its_arguments() {
    case(
        "nested call",
        "fn f() { let value = outer_call(first_argument, inner_call(one, two, three, four, five, six, seven, eight, nine, ten), last); }\n",
        "fn f() {\n    let value = outer_call(\n        first_argument,\n        inner_call(one, two, three, four, five, six, seven, eight, nine, ten),\n        last,\n    );\n}\n",
    );
}

#[test]
fn blank_lines_collapse_and_do_not_touch_block_edges() {
    case(
        "blanks",
        "\n\nfn f() {\n\n\n    let x = 1;\n\n\n\n    return x;\n}\n\n\n",
        "fn f() {\n    let x = 1;\n\n    return x;\n}\n",
    );
}

#[test]
fn a_long_string_is_not_wrapped() {
    case(
        "long string",
        "fn f() {\n    let text = \"this string is deliberately much longer than the eighty column limit and cannot be broken\";\n}\n",
        "fn f() {\n    let text = \"this string is deliberately much longer than the eighty column limit and cannot be broken\";\n}\n",
    );
}

#[test]
fn non_ascii_counts_as_characters_and_survives() {
    case(
        "non ascii",
        "pub const greeting: []u8 = \"héllo\";\n",
        "pub const greeting: []u8 = \"héllo\";\n",
    );
}

#[test]
fn imports_keep_their_groups_and_are_sorted() {
    case(
        "imports",
        "import z.Z;\nimport a.A;\n\nimport m.M;\n\nfn f() {}\n",
        "import a.A;\nimport z.Z;\n\nimport m.M;\n\nfn f() {}\n",
    );
}

#[test]
fn a_large_fixture_is_stable() {
    let before = r#"copy enum Color: u8 { red, green, blue }

interface Draw[State] {
    type Output;
    fn draw(self: &Self, state: State) -> Self.Output;
}

impl Draw[i64] for Color {
    type Output = bool;
    fn draw(self: &Color, state: i64) -> bool { return state > 0; }
}

fn choose(flag: bool, xs: []i64) -> i64 {
    var out: i64 = 0;
    for i, x in xs { //~ note
        out += x;
        if flag { out -= 1; } else { out += 1; }
    }
    match flag {
        true => { return out; }
        false => { return 0; }
    }
}
"#;
    let formatted = format_text(before).unwrap();
    verify(before, &formatted).unwrap();
    assert_eq!(format_text(&formatted).unwrap(), formatted);
}

#[test]
fn every_syntactic_construct_has_a_layout() {
    let source = r#"extern "c" {
    fn puts(s: *u8) -> i32;
    fn printf(fmt: *u8, ...) -> i32;
}

const answer: i64 = 42;
static table: [4]u8 = [4]u8.{ 1, 2, 3, 4 };
type Alias[T: Copy] = [4]T;

copy struct Point { pub x: i64, pub y: i64 }
linear struct Token { id: i64 }
enum Shape: u8 { empty, circle: i64, square = 3 }
interface Draw: Copy { type Color; fn draw(self: &Self, color: Self.Color) -> bool; }
impl Draw for Point { type Color = i64; fn draw(self: &Point, color: i64) -> bool { return color > 0; } }
impl Point { fn norm(self: &Point) -> i64 { return self.x * self.x + self.y * self.y; } }

test "body" { let p = Point.{ x: 1, y: 2 }; @assert(p.x == 1); }
bench "speed" { let p = Point.{ x: 1, y: 2 }; _ = p; }

when @target.os == .macos { fn mac() -> i64 { return 1; } } else { fn other() -> i64 { return 0; } }

fn all(flag: bool, xs: []i64, p: *mut i64) -> i64 {
    let a = 1;
    var b: i64 = 2;
    b = a + b;
    _ = b;
    f();
    let t = (a, b);
    let i = t.0;
    let s = xs[0..1];
    let n = xs.len;
    let cast = a as i64;
    let call = all(flag, xs, p);
    let lit = Point.{ x: a, y: b };
    let arr = [2]i64.{ a, b };
    let shape = Shape.circle(a);
    let got = some_opt() orelse 0;
    let caught = some_result() catch _ => 0;
    let closure = fn[&a](x: i64) -> i64 { return x + a; };
    let safe = unsafe("the pointer is valid") { p.* };
    let ifv = if flag { a } else { b };
    let matchv = match flag { true => a, false => b };
    if flag { b += 1; } else if a == b { b -= 1; } else { b = 0; }
    while flag { break; }
    loop { continue; }
    for i, x in xs { _ = i; _ = x; }
    for j in 0..n step 2 { _ = j; }
    outer: for k in 0..n { break outer; }
    match b {
        .some(v) => { _ = v; }
        .none => { _ = 0; }
    }
    defer { _ = a; }
    errdefer { _ = b; }
    scope sc { spawn f() in sc; }
    {
        let nested = 1;
        _ = nested;
    }
    unsafe("nothing") { }
    when @target.os == .macos { _ = 1; } else { _ = 0; }
    return a;
}

fn f() -> void { }
"#;
    let formatted = format_text(source).unwrap_or_else(|error| panic!("{error:?}"));
    verify(source, &formatted).unwrap();
    assert_eq!(format_text(&formatted).unwrap(), formatted);
}

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn formatted_files() -> Vec<PathBuf> {
    let mut files = Vec::new();
    for dir in [root().join("std"), root().join("conformance")] {
        collect(&dir, &mut files);
    }
    files.sort();
    files
}

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if path.file_name().is_some_and(|name| name == "cache") {
                continue;
            }
            collect(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "ingot") {
            out.push(path);
        }
    }
}

#[test]
fn every_valid_file_in_std_and_conformance_is_formatted() {
    for path in formatted_files() {
        let text = fs::read_to_string(&path).unwrap();
        let Ok(formatted) = format_text(&text) else {
            // A test with a lexical or syntax error is left unchanged by contract.
            continue;
        };
        assert_eq!(
            formatted,
            text,
            "{} is not formatted",
            path.strip_prefix(root()).unwrap().display()
        );
        assert_eq!(
            format_text(&formatted).unwrap(),
            formatted,
            "{}",
            path.display()
        );
    }
}
