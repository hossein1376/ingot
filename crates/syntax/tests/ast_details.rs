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
