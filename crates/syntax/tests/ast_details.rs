//! The AST keeps what name resolution and the later phases need: visibility on every
//! kind of item, the `test`/`bench` distinction, label and binding spans, the
//! operator of `reduce`, and the signature of a function-trait bound.

use ingot_diag::source::Source;
use ingot_syntax::ast::*;
use ingot_syntax::lexer::lex;
use ingot_syntax::parser::parse;

fn module(text: &str) -> (Source, Module) {
    let source = Source::new("t.ingot", text);
    let (tokens, lex_diags) = lex(&source);
    let (ast, parse_diags) = parse(&source, tokens);
    assert!(lex_diags.is_empty() && parse_diags.is_empty(), "{text}");
    (source, ast)
}

fn body_of(ast: &Module, name: &str) -> Vec<Stmt> {
    for item in &ast.items {
        if let Item::Fn(f) = item
            && f.name == name
        {
            return f.body.clone().expect("a body").stmts;
        }
    }
    panic!("no function {name}");
}

#[test]
fn visibility_is_kept_on_every_item_kind() {
    let (_, ast) = module(
        "pub struct A {}\nstruct B {}\npub enum C { x }\npub interface D {}\n\
         pub const e: i64 = 1;\npub static f: i64 = 1;\npub type G = i64;\nconst h: i64 = 1;\n\
         extern \"c\" { fn ef(); }\nextern \"c\" { fn eg(); }\n",
    );
    let flags: Vec<bool> = ast
        .items
        .iter()
        .map(|i| match i {
            Item::Struct(x) => x.public,
            Item::Enum(x) => x.public,
            Item::Interface(x) => x.public,
            Item::Const(x) => x.public,
            Item::Static(x) => x.public,
            Item::TypeAlias(x) => x.public,
            Item::Extern(x) => x.public,
            _ => panic!("unexpected item"),
        })
        .collect();
    assert_eq!(
        flags,
        vec![
            true, false, true, true, true, true, true, false, false, false
        ]
    );
}

#[test]
fn a_test_and_a_bench_are_told_apart() {
    let (_, ast) = module("test \"a\" { }\nbench \"b\" { }\n");
    let benches: Vec<bool> = ast
        .items
        .iter()
        .map(|i| match i {
            Item::Test(t) => t.bench,
            _ => panic!(),
        })
        .collect();
    assert_eq!(benches, vec![false, true]);
}

#[test]
fn every_loop_keeps_its_label_and_a_jump_keeps_the_label_it_names() {
    let (source, ast) = module(
        "fn f(c: bool, xs: []i64) {\n\
         a: while c { break a; }\n\
         b: loop { continue b; }\n\
         d: for x in xs { break d; }\n\
         }\n",
    );
    let stmts = body_of(&ast, "f");
    let label_text = |l: &Option<(String, ingot_diag::source::Span)>| {
        l.as_ref()
            .map(|(n, s)| (n.clone(), source.text_of(*s).to_string()))
    };
    let mut seen = Vec::new();
    for s in &stmts {
        match s {
            Stmt::While { label, body, .. } => {
                seen.push(label_text(label));
                let Stmt::Break { label, .. } = &body.stmts[0] else {
                    panic!()
                };
                seen.push(label_text(label));
            }
            Stmt::Loop { label, body, .. } => {
                seen.push(label_text(label));
                let Stmt::Continue { label, .. } = &body.stmts[0] else {
                    panic!()
                };
                seen.push(label_text(label));
            }
            Stmt::For { label, .. } => seen.push(label_text(label)),
            _ => panic!(),
        }
    }
    let some = |n: &str| Some((n.to_string(), n.to_string()));
    assert_eq!(
        seen,
        vec![some("a"), some("a"), some("b"), some("b"), some("d")]
    );
}

#[test]
fn a_catch_binding_keeps_its_span() {
    let (source, ast) =
        module("fn f() { let a = g() catch err => 0; let b = g() catch _ => 0; }\n");
    let stmts = body_of(&ast, "f");
    let binding = |s: &Stmt| match s {
        Stmt::Let { init, .. } => match &init.kind {
            ExprKind::Catch { binding, .. } => binding.clone(),
            _ => panic!(),
        },
        _ => panic!(),
    };
    let (name, span) = binding(&stmts[0]).expect("a binding");
    assert_eq!((name.as_str(), source.text_of(span)), ("err", "err"));
    assert!(binding(&stmts[1]).is_none());
}

#[test]
fn the_reduce_operator_is_kept() {
    let (_, ast) = module(
        "kernel fn a(i: Idx, x: []f32) -> f32 reduce(+) { return x[i]; }\n\
         kernel fn b(i: Idx, x: []f32) -> f32 reduce(max) { return x[i]; }\n",
    );
    let ops: Vec<Option<String>> = ast
        .items
        .iter()
        .map(|i| match i {
            Item::Fn(f) => f.reduce.clone(),
            _ => panic!(),
        })
        .collect();
    assert_eq!(ops, vec![Some("+".to_string()), Some("max".to_string())]);
}

#[test]
fn a_function_bound_keeps_parameters_result_and_error_apart() {
    let (_, ast) = module(
        "fn a[F: Fn(i64, i64) -> i64](f: F) { }\n\
         fn b[F: FnOnce(i64)](f: F) { }\n\
         fn c[F: FnMut() -> i64 ! u8](f: F) { }\n",
    );
    let sigs: Vec<(usize, bool, bool)> = ast
        .items
        .iter()
        .map(|i| match i {
            Item::Fn(f) => {
                let sig = f.generics[0].bounds[0].fn_sig.as_ref().expect("fn_sig");
                (sig.params.len(), sig.ret.is_some(), sig.error.is_some())
            }
            _ => panic!(),
        })
        .collect();
    assert_eq!(
        sigs,
        vec![(2, true, false), (1, false, false), (0, true, true)]
    );
}

#[test]
fn an_extern_function_type_is_told_from_a_plain_one() {
    let (_, ast) = module("fn f(a: extern fn(*u8) -> i32, b: fn(*u8) -> i32) { }\n");
    let Item::Fn(f) = &ast.items[0] else { panic!() };
    let ext: Vec<bool> = f
        .params
        .iter()
        .map(|p| match &p.ty {
            Ty::Fn { ext, .. } => *ext,
            _ => panic!(),
        })
        .collect();
    assert_eq!(ext, vec![true, false]);
}

#[test]
fn the_else_arm_of_a_select_is_marked() {
    let (_, ast) = module("fn f(a: i64) { select { x = g() => { h(x); } else => { } } }\n");
    let stmts = body_of(&ast, "f");
    let Stmt::Select { arms, .. } = &stmts[0] else {
        panic!()
    };
    assert_eq!(
        arms.iter().map(|a| a.is_else).collect::<Vec<_>>(),
        vec![false, true]
    );
}

#[test]
fn a_static_origin_spans_only_its_keyword() {
    let (source, ast) = module("fn f(a: []u8) -> []u8 from static, a { return a; }\n");
    let Item::Fn(f) = &ast.items[0] else {
        panic!("a function");
    };
    let texts: Vec<&str> = f.from.iter().map(|o| source.text_of(o.span)).collect();
    assert_eq!(texts, vec!["static", "a"]);
}

fn text_of(source: &Source, span: ingot_diag::source::Span) -> &str {
    &source.text[span.start..span.end]
}

#[test]
fn an_expression_statement_ends_at_its_semicolon() {
    let text = "fn f() {\n    g(1);\n\n    // a comment\n    h();\n}\n";
    let (source, ast) = module(text);
    let body = body_of(&ast, "f");
    assert_eq!(text_of(&source, body[0].span()), "g(1);");
    assert_eq!(text_of(&source, body[1].span()), "h();");
}

#[test]
fn a_function_without_a_body_ends_at_its_semicolon() {
    let text = "interface I {\n    fn a(self: &Self);\n\n    // next\n    fn b(self: &Self) -> i64;\n}\n\
                extern \"c\" {\n    fn x();\n\n    fn y(...);\n}\n";
    let (source, ast) = module(text);
    let Item::Interface(interface) = &ast.items[0] else {
        panic!("an interface");
    };
    let spans: Vec<&str> = interface
        .members
        .iter()
        .map(|m| text_of(&source, m.span()))
        .collect();
    assert_eq!(spans, ["fn a(self: &Self);", "fn b(self: &Self) -> i64;"]);
    let Item::Extern(block) = &ast.items[1] else {
        panic!("an extern block");
    };
    let spans: Vec<&str> = block
        .functions
        .iter()
        .map(|f| text_of(&source, f.span))
        .collect();
    assert_eq!(spans, ["fn x();", "fn y(...);"]);
}

#[test]
fn list_items_and_when_items_end_with_their_last_token() {
    let text = "struct S { a: i64, b: [2]i64 }\nenum E { x, y: i64 = 3 }\n\
                fn f[T: Copy, U](p: T, q: U) {}\n\
                when @target.os == .linux { fn a() {} } else { fn b() {} }\nfn g() {}\n";
    let (source, ast) = module(text);
    let Item::Struct(s) = &ast.items[0] else {
        panic!("a struct");
    };
    let fields: Vec<&str> = s.fields.iter().map(|f| text_of(&source, f.span)).collect();
    assert_eq!(fields, ["a: i64", "b: [2]i64"]);
    let Item::Enum(e) = &ast.items[1] else {
        panic!("an enum");
    };
    let variants: Vec<&str> = e
        .variants
        .iter()
        .map(|v| text_of(&source, v.span))
        .collect();
    assert_eq!(variants, ["x", "y: i64 = 3"]);
    let Item::Fn(f) = &ast.items[2] else {
        panic!("a function");
    };
    let generics: Vec<&str> = f
        .generics
        .iter()
        .map(|g| text_of(&source, g.span))
        .collect();
    assert_eq!(generics, ["T: Copy", "U"]);
    let params: Vec<&str> = f.params.iter().map(|p| text_of(&source, p.span)).collect();
    assert_eq!(params, ["p: T", "q: U"]);
    assert_eq!(
        text_of(&source, ast.items[3].span()),
        "when @target.os == .linux { fn a() {} } else { fn b() {} }"
    );
}

fn struct_named<'a>(ast: &'a Module, name: &str) -> &'a StructDecl {
    ast.items
        .iter()
        .find_map(|item| match item {
            Item::Struct(s) if s.name == name => Some(s),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no struct {name}"))
}

#[test]
fn align_is_a_keyword_after_a_struct_header_a_field_type_and_a_static_type() {
    let (_, ast) = module(
        "struct Plain align(16) { a: i64 }\n\
         struct Generic[T] align(32) { a: T }\n\
         struct Field { a: i64 align(8), b: u8 }\n\
         static table: [4]u8 align(64) = @zero;\n",
    );
    assert!(struct_named(&ast, "Plain").align.is_some());
    assert!(struct_named(&ast, "Generic").align.is_some());
    let field = struct_named(&ast, "Field");
    assert!(field.fields[0].align.is_some());
    assert!(field.fields[1].align.is_none());
    assert!(ast.items.iter().any(|item| matches!(
        item,
        Item::Static(s) if s.name == "table" && s.align.is_some()
    )));
}

#[test]
fn align_is_an_ordinary_name_elsewhere() {
    let (_, ast) = module(
        "struct Pair { size: i64, align: i64 }\n\
         struct Both align(8) { align: i64 align(8) }\n\
         fn align(size: i64, align: i64) -> i64 { return size + align; }\n\
         fn f(p: &Pair) -> i64 {\n\
         let align = p.align;\n\
         let g = fn [](align: i64) -> i64 { return align; };\n\
         return g(align) + align(1, align);\n}\n",
    );
    let pair = struct_named(&ast, "Pair");
    assert_eq!(pair.fields[1].name, "align");
    assert!(pair.fields[1].align.is_none());
    let both = struct_named(&ast, "Both");
    assert!(both.align.is_some());
    assert_eq!(both.fields[0].name, "align");
    assert!(both.fields[0].align.is_some());
    let function = ast
        .items
        .iter()
        .find_map(|item| match item {
            Item::Fn(f) if f.name == "align" => Some(f),
            _ => None,
        })
        .expect("a function named align");
    assert_eq!(function.params[1].name, "align");
    let stmts = body_of(&ast, "f");
    assert!(matches!(
        &stmts[0],
        Stmt::Let { pattern: Pattern::Bind { name, .. }, init, .. }
            if name == "align"
                && matches!(&init.kind, ExprKind::Field { name, .. } if name == "align")
    ));
}

#[test]
fn align_without_its_parentheses_is_a_syntax_error() {
    let source = Source::new("t.ingot", "struct S align { a: i64 }\n");
    let (tokens, _) = lex(&source);
    let (_, diagnostics) = parse(&source, tokens);
    assert!(!diagnostics.is_empty());
}
