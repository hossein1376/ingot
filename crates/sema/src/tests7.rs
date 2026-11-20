//! Unit tests for phase 7: literal typing, method lookup, receiver adjustment,
//! exhaustiveness, generics and the typed result.

use crate::hir::{self, ExprId, ExprKind};
use crate::typed::*;
use crate::{BodyOutput, Declarations, ModuleInput, Output, PackageInput, Target};
use crate::{check_bodies, check_declarations, resolve};
use ingot_diag::source::{Source, Sources};
use ingot_syntax::lexer::lex;
use ingot_syntax::parser::parse;

const STD: [(&str, &str); 15] = [
    ("", include_str!("../../../std/main.ingot")),
    ("core", include_str!("../../../std/core.ingot")),
    ("exec", include_str!("../../../std/exec.ingot")),
    ("fmt", include_str!("../../../std/fmt.ingot")),
    ("fs", include_str!("../../../std/fs.ingot")),
    ("gpu", include_str!("../../../std/gpu.ingot")),
    ("hash", include_str!("../../../std/hash.ingot")),
    ("io", include_str!("../../../std/io.ingot")),
    ("mem", include_str!("../../../std/mem.ingot")),
    ("net", include_str!("../../../std/net.ingot")),
    ("rand", include_str!("../../../std/rand.ingot")),
    ("sync", include_str!("../../../std/sync.ingot")),
    ("sys.macos", include_str!("../../../std/sys/macos.ingot")),
    ("task", include_str!("../../../std/task.ingot")),
    ("time", include_str!("../../../std/time.ingot")),
];

struct B {
    out: Output,
    decls: Declarations,
    bodies: BodyOutput,
    sources: Sources,
}

fn build(text: &str) -> B {
    let mut sources = Sources::default();
    let add = |name: &str, modules: &[(&str, &str)], sources: &mut Sources| {
        let mut mods = Vec::new();
        for (rel, text) in modules {
            let path = if rel.is_empty() {
                name.to_string()
            } else {
                format!("{name}.{rel}")
            };
            let file = sources.add(Source::new(format!("{path}.ingot"), *text));
            let source = sources.get(file);
            let (tokens, mut diags) = lex(source);
            let (ast, parse_diags) = parse(source, tokens);
            diags.extend(parse_diags);
            assert!(diags.is_empty(), "{path} must parse: {diags:?}");
            mods.push(ModuleInput {
                path,
                file,
                ast: Some(ast),
            });
        }
        PackageInput {
            name: name.to_string(),
            deps: Vec::new(),
            modules: mods,
        }
    };
    let app_pkg = add("app", &[("", text)], &mut sources);
    let inputs = vec![add("std", &STD, &mut sources), app_pkg];
    let mut inputs = inputs;
    let names: Vec<String> = inputs.iter().map(|p| p.name.clone()).collect();
    for input in inputs.iter_mut() {
        input.deps = names
            .iter()
            .filter(|name| **name != input.name && name.as_str() != "std")
            .cloned()
            .collect();
    }
    let target = Target::by_name("aarch64-macos-m4").expect("a target");
    let out = resolve(&mut sources, inputs, &target);
    let decls = check_declarations(&sources, &out, &target, &[1], None);
    // Keep a copy of the phase 6 result for its diagnostics.
    let phase6: Vec<_> = decls.diagnostics.clone();
    let bodies = check_bodies(&sources, &out, decls, &target, &[1], None);
    let mut decls = Declarations::new();
    decls.diagnostics = phase6;
    B {
        out,
        decls,
        bodies,
        sources,
    }
}

impl B {
    /// (code, line) of the diagnostics of phase 7, sorted.
    fn codes(&self) -> Vec<(String, u32)> {
        let mut v: Vec<(String, u32)> = self
            .bodies
            .diagnostics
            .iter()
            .map(|d| {
                let line = self
                    .sources
                    .iter()
                    .find(|s| s.path == d.file)
                    .map(|s| s.position(d.span.start).line)
                    .unwrap_or(0);
                (d.code.to_string(), line)
            })
            .collect();
        v.sort();
        v
    }

    fn only_codes(&self) -> Vec<String> {
        self.codes().into_iter().map(|(c, _)| c).collect()
    }

    fn phase6(&self) -> usize {
        self.decls.diagnostics.len()
    }

    fn ni(&self) -> Vec<String> {
        self.bodies.not_implemented.clone()
    }

    fn package(&self) -> &hir::Package {
        &self.out.packages[1]
    }

    fn def(&self, name: &str) -> crate::DefId {
        let m = self.out.sema.module_paths["app"];
        *self
            .out
            .sema
            .module(m)
            .values
            .get(name)
            .unwrap_or_else(|| panic!("function {name}"))
    }

    fn body(&self, name: &str) -> &TypedBody {
        self.bodies
            .bodies
            .bodies
            .get(&self.def(name))
            .unwrap_or_else(|| panic!("a typed body for {name}"))
    }

    fn display(&self, t: crate::TypeId) -> String {
        self.bodies.declarations.types.display(&self.out.sema, t)
    }

    /// The expressions of a body that satisfy `pred`, in id order.
    fn exprs(&self, name: &str, pred: impl Fn(&hir::Expr) -> bool) -> Vec<ExprId> {
        let body = self.body(name);
        body.exprs
            .keys()
            .copied()
            .filter(|e| pred(self.package().expr(*e)))
            .collect()
    }

    /// The type of each integer or float literal of a function, in source order.
    fn literal_types(&self, name: &str) -> Vec<String> {
        let body = self.body(name);
        let mut ids = self.exprs(name, |e| {
            matches!(e.kind, ExprKind::Int { .. } | ExprKind::Float(_))
        });
        ids.sort_by_key(|e| self.package().expr(*e).span.start);
        ids.into_iter()
            .map(|e| self.display(body.exprs[&e].ty))
            .collect()
    }

    fn calls(&self, name: &str) -> Vec<(ExprId, CallInfo)> {
        let body = self.body(name);
        let mut v: Vec<(ExprId, CallInfo)> = body
            .exprs
            .iter()
            .filter_map(|(e, i)| match &i.res {
                ExprRes::Call(c) => Some((*e, (**c).clone())),
                _ => None,
            })
            .collect();
        v.sort_by_key(|(e, _)| self.package().expr(*e).span.start);
        v
    }
}

// ----------------------------------------------------------------- literals

#[test]
fn a_literal_takes_the_type_of_its_context() {
    let b = build(
        "fn f() -> u8 {\n    let a: u8 = 5;\n    let c = 7;\n    let d = 1.5;\n    let e: f32 = 2.5;\n    return a + 1;\n}\n",
    );
    assert_eq!(b.only_codes(), Vec::<String>::new());
    assert_eq!(b.literal_types("f"), ["u8", "i64", "f64", "f32", "u8"]);
}

#[test]
fn the_expected_type_does_not_flow_from_a_later_use() {
    // `x` is `i64` by default; passing it where a `u8` is wanted is a mismatch.
    let b = build(
        "fn g(a: u8) -> u8 { return a; }\nfn f() -> u8 {\n    let x = 5;\n    return g(x);\n}\n",
    );
    assert_eq!(b.codes(), vec![("E0401".to_string(), 4)]);
}

#[test]
fn a_literal_takes_the_type_of_the_other_operand() {
    let b = build(
        "fn f(a: u8, b: i32) -> bool {\n    let x = 1 + a;\n    let y = b * 2;\n    return x == 3 && y > 4;\n}\n",
    );
    assert_eq!(b.only_codes(), Vec::<String>::new());
    assert_eq!(b.literal_types("f"), ["u8", "i32", "u8", "i32"]);
}

#[test]
fn negative_literals_and_ranges() {
    let b = build("fn f() -> i8 {\n    let a: i8 = -128;\n    return a;\n}\n");
    assert_eq!(b.only_codes(), Vec::<String>::new());
    let b = build("fn f() -> i8 {\n    let a: i8 = 128;\n    return a;\n}\n");
    assert_eq!(b.only_codes(), ["E0405"]);
    let b = build("fn f() -> u8 {\n    let a: u8 = -1;\n    return a;\n}\n");
    assert_eq!(b.only_codes(), ["E0405"]);
    let b = build("fn f() -> i64 {\n    let a = 9223372036854775808;\n    return a;\n}\n");
    assert_eq!(b.only_codes(), ["E0405"]);
}

#[test]
fn a_literal_argument_does_not_decide_a_type_parameter() {
    // `T` is fixed by `x`; the literal takes `u8`. With only literals it defaults.
    let b = build(
        "fn pair[T](a: T, b: T) -> T { return a; }\nfn f(x: u8) -> u8 {\n    let p = pair(3, 4);\n    return pair(x, 5);\n}\nfn g() -> i64 { return pair(1, 2); }\n",
    );
    assert_eq!(b.only_codes(), Vec::<String>::new());
    assert_eq!(b.literal_types("f"), ["i64", "i64", "u8"]);
    let c = b.calls("g");
    let ExprRes::Call(_) = &b.body("g").exprs[&c[0].0].res else {
        panic!("a call")
    };
    let Callee::Fn(r) = &c[0].1.callee else {
        panic!("a function")
    };
    assert_eq!(r.args.len(), 1);
}

#[test]
fn an_integer_literal_takes_an_exactly_representable_float_type() {
    let b = build(
        "fn f() -> f64 {\n    let x: f64 = 1;\n    let y: f32 = 16777216;\n    return x + y as f64;\n}\n",
    );
    assert_eq!(b.only_codes(), Vec::<String>::new());
    assert_eq!(b.literal_types("f"), ["f64", "f32"]);
    let b = build("fn f() -> f32 {\n    let x: f32 = 16777217;\n    return x;\n}\n");
    assert_eq!(b.only_codes(), ["E0405"]);
    let b = build("fn f() -> f64 {\n    let x: f64 = 9007199254740992;\n    return x;\n}\n");
    assert_eq!(b.only_codes(), Vec::<String>::new());
    let b = build("fn f() -> f64 {\n    let x: f64 = 9007199254740993;\n    return x;\n}\n");
    assert_eq!(b.only_codes(), ["E0405"]);
}

// -------------------------------------------------------------- method lookup

const METHODS: &str = "\
struct Counter { n: i64 }
impl Counter {
    fn new() -> Counter { return Counter.{ n: 0 }; }
    fn bump(self: &mut Counter) { self.n += 1; }
    fn get(self: &Counter) -> i64 { return self.n; }
    fn take(self: Counter) -> i64 { return self.n; }
}
interface Named { fn name(self: &Self) -> i64; }
impl Named for Counter { fn name(self: &Counter) -> i64 { return 1; } }
";

fn with(text: &str) -> B {
    build(&format!("{METHODS}{text}"))
}

#[test]
fn an_inherent_method_and_the_receiver_borrow() {
    let b = with(
        "fn f(c: Counter, r: &Counter, m: &mut Counter) -> i64 {\n    var v = Counter.new();\n    v.bump();\n    m.bump();\n    let a = c.get();\n    let b = r.get();\n    let t = v.take();\n    return a + b + t;\n}\n",
    );
    assert_eq!(b.only_codes(), Vec::<String>::new());
    let calls = b.calls("f");
    let recv: Vec<Option<Receiver>> = calls.iter().map(|(_, c)| c.receiver).collect();
    // `Counter.new()` has no receiver; then bump (var: exclusive), bump (&mut: exclusive
    // after looking through the borrow), get (value: shared), get (&: shared through the
    // borrow), take (value).
    assert_eq!(
        recv,
        vec![
            None,
            Some(Receiver {
                derefs: 0,
                mode: ReceiverMode::Exclusive
            }),
            Some(Receiver {
                derefs: 1,
                mode: ReceiverMode::Exclusive
            }),
            Some(Receiver {
                derefs: 0,
                mode: ReceiverMode::Shared
            }),
            Some(Receiver {
                derefs: 1,
                mode: ReceiverMode::Shared
            }),
            Some(Receiver {
                derefs: 0,
                mode: ReceiverMode::Value
            }),
        ]
    );
}

#[test]
fn an_exclusive_receiver_needs_a_mutable_place() {
    let b = with("fn f(c: Counter, r: &Counter) {\n    c.bump();\n    r.bump();\n}\n");
    assert_eq!(
        b.codes(),
        vec![("E0610".to_string(), 11), ("E0610".to_string(), 12)]
    );
}

#[test]
fn an_interface_method_is_found_through_the_impl() {
    let b = with("fn f(c: &Counter) -> i64 {\n    return c.name();\n}\n");
    assert_eq!(b.only_codes(), Vec::<String>::new());
    let calls = b.calls("f");
    let Callee::Fn(r) = &calls[0].1.callee else {
        panic!("a function")
    };
    let FnSource::Interface { via, .. } = &r.source else {
        panic!("through an interface")
    };
    assert!(matches!(via, InterfaceVia::Impl { .. }));
}

#[test]
fn a_builtin_interface_method_on_a_scalar() {
    let b = build("fn f(a: i64, b: i64) -> bool {\n    return a.cmp(&b) == .less;\n}\n");
    assert_eq!(b.only_codes(), Vec::<String>::new());
    let calls = b.calls("f");
    let Callee::Fn(r) = &calls[0].1.callee else {
        panic!("a function")
    };
    let FnSource::Interface { via, .. } = &r.source else {
        panic!("through an interface")
    };
    assert!(matches!(via, InterfaceVia::Builtin));
}

#[test]
fn a_method_through_a_bound() {
    let b = build(
        "fn biggest[T: Ord + Copy](a: T, b: T) -> T {\n    if a.cmp(&b) == .less { return b; }\n    return a;\n}\n",
    );
    assert_eq!(b.only_codes(), Vec::<String>::new());
    let calls = b.calls("biggest");
    let Callee::Fn(r) = &calls[0].1.callee else {
        panic!("a function")
    };
    let FnSource::Interface { via, .. } = &r.source else {
        panic!("through an interface")
    };
    assert!(matches!(via, InterfaceVia::Bound));
}

#[test]
fn two_interfaces_with_one_method_are_ambiguous() {
    let b = build(
        "interface A { fn size(self: &Self) -> i64; }\ninterface B { fn size(self: &Self) -> i64; }\nstruct S { n: i64 }\nimpl A for S { fn size(self: &S) -> i64 { return 1; } }\nimpl B for S { fn size(self: &S) -> i64 { return 2; } }\nfn f(s: &S) -> i64 {\n    return s.size();\n}\nfn g(s: &S) -> i64 {\n    return A.size(s);\n}\n",
    );
    assert_eq!(b.codes(), vec![("E0313".to_string(), 7)]);
}

#[test]
fn an_inherent_method_wins_over_an_interface_method() {
    let b = build(
        "interface A { fn size(self: &Self) -> i64; }\nstruct S { n: i64 }\nimpl A for S { fn size(self: &S) -> i64 { return 1; } }\nimpl S { fn size(self: &S) -> i64 { return 2; } }\nfn f(s: &S) -> i64 {\n    return s.size();\n}\n",
    );
    assert_eq!(b.only_codes(), Vec::<String>::new());
    let calls = b.calls("f");
    let Callee::Fn(r) = &calls[0].1.callee else {
        panic!("a function")
    };
    assert!(matches!(r.source, FnSource::Inherent { .. }));
}

#[test]
fn the_bounds_of_an_impl_are_checked_for_the_receiver() {
    // `impl[K: Hash] Show for Pair[K, K]`: `Pair[Plain]` has no `show` because `Plain`
    // is not `Hash`.
    let text = "interface Show { fn show(self: &Self) -> i64; }\nstruct Pair[A, B] { a: A, b: B }\nstruct Plain { n: i64 }\nimpl[K: Hash] Show for Pair[K, K] { fn show(self: &Pair[K, K]) -> i64 { return 1; } }\nfn good(p: &Pair[i64, i64]) -> i64 {\n    return p.show();\n}\nfn bad(p: &Pair[Plain, Plain]) -> i64 {\n    return p.show();\n}\n";
    let b = build(text);
    assert_eq!(b.codes(), vec![("E0314".to_string(), 9)]);
}

#[test]
fn an_unknown_method_and_a_method_the_bounds_do_not_grant() {
    let b = with("fn f(c: &Counter) -> i64 {\n    return c.nope();\n}\n");
    assert_eq!(b.codes(), vec![("E0314".to_string(), 11)]);
    let b = build("fn f[T: Copy](a: T, b: T) -> bool {\n    return a.eq(&b);\n}\n");
    assert_eq!(b.only_codes(), ["E0802"]);
    let b = build("fn f[T](a: T) -> T {\n    return a.nope();\n}\n");
    assert_eq!(b.only_codes(), ["E0314"]);
}

// ------------------------------------------------------------------ patterns

fn matches(text: &str) -> Vec<String> {
    build(text).only_codes()
}

#[test]
fn enum_matches_are_exhaustive_when_every_variant_is_covered() {
    let defs = "enum E { a, b, c: i64 }\n";
    assert!(
        matches(&format!(
            "{defs}fn f(e: E) -> i64 {{ return match e {{ .a => 1, .b => 2, .c(_) => 3, }}; }}\n"
        ))
        .is_empty()
    );
    assert_eq!(
        matches(&format!(
            "{defs}fn f(e: E) -> i64 {{ return match e {{ .a => 1, .c(_) => 3, }}; }}\n"
        )),
        ["E0601"]
    );
    assert!(
        matches(&format!(
            "{defs}fn f(e: E) -> i64 {{ return match e {{ .a => 1, else => 3, }}; }}\n"
        ))
        .is_empty()
    );
}

#[test]
fn integer_ranges_are_intervals() {
    let m = |arms: &str| {
        matches(&format!(
            "fn f(n: u8) -> i64 {{ return match n {{ {arms} }}; }}\n"
        ))
    };
    assert!(m("0..=127 => 1, 128..=255 => 2,").is_empty());
    assert_eq!(m("0..=127 => 1, 129..=255 => 2,"), ["E0601"]);
    assert!(m("0 => 1, 1..255 => 2, 255 => 3,").is_empty());
    assert_eq!(m("0..=10 => 1, 5..=20 => 2,"), ["E0601"]);
    // An arm inside an earlier one is unreachable.
    assert_eq!(m("0..=10 => 1, 5 => 2, else => 3,"), ["E0602"]);
    // A range that is only partly covered earlier is still reachable.
    assert!(m("0..=10 => 1, 5..=20 => 2, else => 3,").is_empty());
}

#[test]
fn signed_integer_ranges() {
    let m = |arms: &str| {
        matches(&format!(
            "fn f(n: i8) -> i64 {{ return match n {{ {arms} }}; }}\n"
        ))
    };
    assert!(m("-128..0 => 1, 0..=127 => 2,").is_empty());
    assert_eq!(m("-128..0 => 1, 1..=127 => 2,"), ["E0601"]);
}

#[test]
fn nested_patterns() {
    let defs = "enum E { a: ?bool, b }\n";
    let m = |arms: &str| {
        matches(&format!(
            "{defs}fn f(e: E) -> i64 {{ return match e {{ {arms} }}; }}\n"
        ))
    };
    assert!(m(".a(.some(true)) => 1, .a(.some(false)) => 2, .a(.none) => 3, .b => 4,").is_empty());
    assert_eq!(
        m(".a(.some(true)) => 1, .a(.none) => 3, .b => 4,"),
        ["E0601"]
    );
    assert_eq!(m(".a(_) => 1, .b => 2, .a(.none) => 3,"), ["E0602"]);
    let t = |arms: &str| {
        matches(&format!(
            "fn f(p: (bool, bool)) -> i64 {{ return match p {{ {arms} }}; }}\n"
        ))
    };
    assert!(t("(true, _) => 1, (false, true) => 2, (false, false) => 3,").is_empty());
    assert_eq!(t("(true, _) => 1, (false, true) => 2,"), ["E0601"]);
}

#[test]
fn a_guard_makes_its_arm_count_for_nothing() {
    let m = |arms: &str| {
        matches(&format!(
            "fn f(n: i64) -> i64 {{ return match n {{ {arms} }}; }}\n"
        ))
    };
    assert_eq!(m("x if x < 0 => 1, x if x >= 0 => 2,"), ["E0601"]);
    assert!(m("x if x < 0 => 1, x => 2,").is_empty());
    // A guarded arm after full coverage is still unreachable.
    assert_eq!(m("x => 1, y if y < 0 => 2,"), ["E0602"]);
}

#[test]
fn a_refutable_pattern_in_let() {
    assert_eq!(
        matches("fn f(o: ?i64) -> i64 {\n    let .some(v) = o;\n    return v;\n}\n"),
        ["E0604"]
    );
    assert!(matches("enum One { only: i64 }\nfn f(o: One) -> i64 {\n    let .only(v) = o;\n    return v;\n}\n").is_empty());
    assert!(matches("fn f(t: (i64, bool)) -> i64 {\n    let (a, b) = t;\n    if b { return a; }\n    return 0;\n}\n").is_empty());
}

#[test]
fn binding_modes_follow_the_scrutinee() {
    let b = build(
        "struct P { x: i64 }\nenum E { a: P, b }\nfn f(e: &E, m: &mut E, v: E) -> i64 {\n    var t = 0;\n    match e { .a(p) => { t += p.x; } .b => {} }\n    match m { .a(q) => { q.x = 1; } .b => {} }\n    match v { .a(r) => { t += r.x; } .b => {} }\n    return t;\n}\n",
    );
    assert_eq!(b.only_codes(), Vec::<String>::new());
    let body = b.body("f");
    let mut modes: Vec<(String, BindMode)> = body
        .locals
        .iter()
        .filter(|(l, _)| matches!(b.package().local(**l).kind, hir::LocalKind::Let))
        .map(|(l, i)| (b.package().local(*l).name.clone(), i.mode))
        .collect();
    modes.sort_by(|a, c| a.0.cmp(&c.0));
    assert_eq!(
        modes,
        vec![
            ("p".to_string(), BindMode::Ref),
            ("q".to_string(), BindMode::RefMut),
            ("r".to_string(), BindMode::Move),
            ("t".to_string(), BindMode::Copy),
        ]
    );
}

// ------------------------------------------------------------------ generics

#[test]
fn a_type_argument_not_determined_by_the_arguments() {
    let b = build(
        "fn zero[T: Num]() -> T { return @zero; }\nfn f() -> i64 {\n    let z = zero();\n    return z;\n}\nfn g() -> i32 { return zero.[i32](); }\n",
    );
    assert_eq!(b.codes(), vec![("E0803".to_string(), 3)]);
}

#[test]
fn a_bound_that_the_argument_does_not_meet_at_a_call() {
    let b = build(
        "struct Buf { n: i64 }\nfn dup[T: Copy](a: T) -> (T, T) { return (a, a); }\nfn f() -> i64 {\n    let p = dup(Buf.{ n: 1 });\n    return p.0.n;\n}\n",
    );
    assert_eq!(b.codes(), vec![("E0801".to_string(), 4)]);
}

#[test]
fn operators_in_a_generic_body_need_a_bound() {
    let b = build(
        "fn f[T: Copy](a: T, b: T) -> T { return a + b; }\nfn g[T: Num](a: T, b: T) -> T { return a + b; }\nfn h[T: Float](a: T, b: T) -> T { return a % b; }\nfn i[T: Int](a: T, b: T) -> T { return a % b; }\n",
    );
    assert_eq!(
        b.codes(),
        vec![("E0406".to_string(), 3), ("E0802".to_string(), 1)]
    );
}

#[test]
fn zero_and_bits_on_a_type_parameter() {
    let b = build(
        "fn f[T: Copy]() -> T { return @zero; }\nfn g[T: Num]() -> T { return @zero; }\nfn h[T: Copy](x: T) -> u32 { return @bits(u32, x); }\n",
    );
    assert_eq!(
        b.codes(),
        vec![("E0811".to_string(), 1), ("E0811".to_string(), 3)]
    );
}

// ------------------------------------------------------------- typed result

#[test]
fn the_typed_body_records_what_the_source_leaves_implicit() {
    let b = build(
        "struct P { x: i64, y: i64 }\nenum E { a, b: P }\nfn f(p: &P, e: E) -> i64 {\n    let q = P.{ y: 2, x: 1 };\n    let k = E.b(q);\n    let z = p.y + q.x;\n    return z;\n}\n",
    );
    assert_eq!(b.only_codes(), Vec::<String>::new());
    // Field accesses record how many borrows were looked through.
    let fields = b.exprs("f", |e| matches!(e.kind, ExprKind::Field { .. }));
    let body = b.body("f");
    let ds: Vec<u32> = fields
        .iter()
        .filter_map(|e| match &body.exprs[e].res {
            ExprRes::Field { derefs, .. } => Some(*derefs),
            _ => None,
        })
        .collect();
    assert_eq!(ds, [1, 0]);
    // The struct literal records the order of its fields.
    let lits = b.exprs("f", |e| matches!(e.kind, ExprKind::StructLit { .. }));
    let ExprRes::Struct(s) = &body.exprs[&lits[0]].res else {
        panic!("a struct literal")
    };
    assert_eq!(s.order, [1, 0]);
    // The variant constructor.
    let calls = b.calls("f");
    assert!(matches!(&calls[0].1.callee, Callee::Variant(v) if v.index == 1));
}

#[test]
fn try_records_the_conversion() {
    let b = build(
        "enum A { bad }\nenum B { a: from A, other }\nfn g() -> i64 ! A { return 1; }\nfn same() -> i64 ! A {\n    return try g();\n}\nfn wrap() -> i64 ! B {\n    return try g();\n}\n",
    );
    assert_eq!(b.only_codes(), Vec::<String>::new());
    let get = |name: &str| {
        let body = b.body(name);
        let t = b.exprs(name, |e| {
            matches!(
                e.kind,
                ExprKind::Unary {
                    op: ingot_syntax::ast::UnOp::Try,
                    ..
                }
            )
        });
        match &body.exprs[&t[0]].res {
            ExprRes::Try(i) => i.conversion.clone(),
            other => panic!("{other:?}"),
        }
    };
    assert_eq!(get("same"), TryConversion::Identity);
    assert!(matches!(get("wrap"), TryConversion::FromVariant(v) if v.index == 0));
}

#[test]
fn an_array_len_records_its_length() {
    let b = build("fn f(a: [4]u8, r: &[4]u8) -> i64 {\n    return a.len + r.len;\n}\n");
    assert_eq!(b.only_codes(), Vec::<String>::new());
    let body = b.body("f");
    let lenses: Vec<(u32, Option<i128>)> = b
        .exprs(
            "f",
            |e| matches!(&e.kind, ExprKind::Field { name, .. } if name == "len"),
        )
        .into_iter()
        .filter_map(|e| match &body.exprs[&e].res {
            ExprRes::ArrayLen { derefs, len } => Some((*derefs, len.as_len())),
            _ => None,
        })
        .collect();
    assert_eq!(lenses, [(0, Some(4)), (1, Some(4))]);
    assert!(
        b.exprs("f", |e| matches!(e.kind, ExprKind::Field { .. }))
            .iter()
            .all(|e| b.display(body.exprs[e].ty) == "i64")
    );
}

#[test]
fn a_borrow_to_dyn_records_how_the_interface_is_implemented() {
    let b = build(
        "interface Named { fn name(self: &Self) -> i64; }\ncopy struct Item { n: i64 }\nimpl Named for Item { fn name(self: &Item) -> i64 { return self.n; } }\nfn get(x: &dyn Named) -> i64 { return x.name(); }\nfn call(s: &Item) -> i64 { return get(s); }\nfn result(s: &Item) -> &dyn Named { return s; }\n",
    );
    assert_eq!(b.only_codes(), Vec::<String>::new());
    for name in ["call", "result"] {
        let body = b.body(name);
        let (_, info) = body
            .exprs
            .iter()
            .find(|(_, i)| i.coerce.is_some())
            .unwrap_or_else(|| panic!("a coercion in {name}"));
        assert_eq!(b.display(info.ty), "&dyn Named");
        let c = info.coerce.as_ref().expect("a coercion");
        assert_eq!(b.display(c.self_ty), "Item");
        assert!(c.interface_args.is_empty());
        assert!(matches!(c.via, InterfaceVia::Impl { .. }));
    }
}

#[test]
fn a_borrow_to_dyn_needs_the_interface_implemented() {
    let b = build(
        "interface Named { fn name(self: &Self) -> i64; }\ncopy struct Item { n: i64 }\nfn get(x: &dyn Named) -> i64 { return x.name(); }\nfn call(s: &Item) -> i64 { return get(s); }\n",
    );
    assert_eq!(b.only_codes(), ["E0401"]);
}

#[test]
fn if_and_match_branches_become_the_expected_dyn() {
    let b = build(
        "interface Named { fn name(self: &Self) -> i64; }\ncopy struct A { n: i64 }\ncopy struct B { n: i64 }\nimpl Named for A { fn name(self: &A) -> i64 { return self.n; } }\nimpl Named for B { fn name(self: &B) -> i64 { return self.n; } }\nfn by_if(c: bool, a: &A, b: &B) -> &dyn Named { return if c { a } else { b }; }\nfn by_match(c: bool, a: &A, b: &B) -> i64 {\n    let d: &dyn Named = match c { true => a, false => b };\n    return d.name();\n}\n",
    );
    assert_eq!(b.only_codes(), Vec::<String>::new());
    for name in ["by_if", "by_match"] {
        let body = b.body(name);
        let mut selves: Vec<String> = body
            .exprs
            .values()
            .filter_map(|i| i.coerce.as_ref())
            .map(|c| b.display(c.self_ty))
            .collect();
        selves.sort();
        assert_eq!(selves, ["A", "B"], "in {name}");
    }
}

#[test]
fn a_branch_that_does_not_implement_the_interface_is_one_e0401() {
    let b = build(
        "interface Named { fn name(self: &Self) -> i64; }\ncopy struct A { n: i64 }\ncopy struct C { n: i64 }\nimpl Named for A { fn name(self: &A) -> i64 { return self.n; } }\nfn f(c: bool, x: &C, a: &A) -> &dyn Named { return if c { x } else { a }; }\n",
    );
    assert_eq!(b.only_codes(), ["E0401"]);
}

#[test]
fn a_dyn_field_payload_and_exclusive_borrow() {
    let b = build(
        "interface Named { fn name(self: &Self) -> i64; }\ncopy struct Item { n: i64 }\nimpl Named for Item { fn name(self: &Item) -> i64 { return self.n; } }\nstruct Holder { n: &dyn Named }\nenum Slot { named: &dyn Named, empty }\nfn field(s: &Item) -> Holder { return Holder.{ n: s }; }\nfn payload(s: &Item) -> Slot { return Slot.named(s); }\nfn mutable(s: &mut Item) -> &mut dyn Named { return s; }\n",
    );
    assert_eq!(b.only_codes(), Vec::<String>::new());
    for name in ["field", "payload", "mutable"] {
        let body = b.body(name);
        let (_, info) = body
            .exprs
            .iter()
            .find(|(_, i)| i.coerce.is_some())
            .unwrap_or_else(|| panic!("a coercion in {name}"));
        let c = info.coerce.as_ref().expect("a coercion");
        assert_eq!(b.display(c.self_ty), "Item");
        assert!(matches!(c.via, InterfaceVia::Impl { .. }));
    }
}

#[test]
fn par_for_rejects_fail_and_try() {
    let b = build(
        "enum E { e }\nfn f() -> i64 ! E {\n    par for i in 0..2 { _ = i; fail .e; }\n    return 0;\n}\n",
    );
    assert_eq!(b.only_codes(), ["E1408"]);
    let b = build(
        "enum E { e }\nfn g() -> i64 ! E { return 1; }\nfn f() -> i64 ! E {\n    par for i in 0..2 { _ = i; let x = try g(); @assert(x == 1); }\n    return 0;\n}\n",
    );
    assert_eq!(b.only_codes(), ["E1408"]);
}

#[test]
fn a_select_arm_calling_a_waitable_fn_is_accepted() {
    let b = build(
        "import std.task.Wait;\nimport std.task.WaitCx;\n\
         struct Service { n: i64 }\n\
         struct Op { n: i64 }\n\
         impl Wait for Op {\n\
             type Output = i64;\n\
             fn poll(self: &mut Op, cx: &mut WaitCx) -> ?i64 {\n\
                 _ = cx;\n\
                 return .some(self.n);\n\
             }\n\
         }\n\
         impl Service {\n\
             waitable fn wait(self: &Service) -> i64 {\n\
                 return Op.{ n: self.n };\n\
             }\n\
         }\n\
         fn f(s: &Service) -> i64 {\n\
             select {\n\
                 x = s.wait() => { return x; }\n\
             }\n\
         }\n",
    );
    assert_eq!(b.only_codes(), Vec::<String>::new());
}

#[test]
fn a_select_arm_calling_a_value_is_not_waitable() {
    let b = build(
        "fn f() -> i64 {\n    let job = fn []() -> i64 { return 1; };\n    select {\n        x = job() => { return x; }\n    }\n}\n",
    );
    assert_eq!(b.only_codes(), ["E1409"]);
}

#[test]
fn a_test_body_may_try_any_error() {
    let b = build(
        "enum A { a }\nfn g() -> i64 ! A { return 1; }\ntest \"t\" {\n    let v = try g();\n    @assert(v == 1);\n}\n",
    );
    assert_eq!(b.only_codes(), Vec::<String>::new());
}

#[test]
fn an_operation_belongs_only_to_the_innermost_unsafe_block() {
    let b = build(
        "fn f(p: *i64) -> i64 {\n    return unsafe(\"outer\") { unsafe(\"inner\") { p.* } };\n}\n",
    );
    assert_eq!(b.only_codes(), ["E1304"]);
    let b = build("fn f(p: *i64) -> i64 {\n    return unsafe(\"one\") { p.* };\n}\n");
    assert_eq!(b.only_codes(), Vec::<String>::new());
}

#[test]
fn a_raw_pointer_member_uses_its_unsafe_block() {
    // The fix of E0418 inserts the dereference the block is for: no E1304 as well.
    let b = build(
        "struct C { x: i64 }\nfn f(p: *C) -> i64 {\n    return unsafe(\"live\") { p.x };\n}\n",
    );
    assert_eq!(b.only_codes(), ["E0418"]);
}

#[test]
fn function_values_of_unsafe_and_extern_functions() {
    // An `extern` function's value has the `extern fn` type: taking it needs nothing.
    let b = build(
        "extern \"c\" { fn puts(s: *u8) -> i32; }\nfn f() -> extern fn(*u8) -> i32 {\n    return puts;\n}\n",
    );
    assert_eq!(b.only_codes(), Vec::<String>::new());
    // An `unsafe fn` cannot be used as a value; the pointer could be called safely.
    let b = build("unsafe fn g() {}\nfn f() {\n    let p = g;\n    _ = p;\n}\n");
    assert_eq!(b.only_codes(), ["E1305"]);
    assert!(
        !b.ni()
            .iter()
            .any(|m| m.contains("`unsafe fn` used as a value")),
        "{:?}",
        b.ni()
    );
}

#[test]
fn catch_and_orelse_on_the_wrong_value_point_at_the_operator() {
    for (src, word) in [
        (
            "fn f(x: i64) -> i64 {\n    return x catch _ => 0;\n}\n",
            "catch",
        ),
        (
            "fn f(x: i64) -> i64 {\n    return x orelse 0;\n}\n",
            "orelse",
        ),
    ] {
        let b = build(src);
        assert_eq!(b.only_codes(), ["E0704"]);
        let at = b.bodies.diagnostics[0].span;
        assert_eq!(&src[at.start..at.end], word);
    }
}

#[test]
fn select_and_conversions_between_vectors_and_other_types() {
    // Same lane count, different lane types: a plain mismatch.
    let b =
        build("fn f(m: boolx4, a: f32x4, b: i32x4) -> f32x4 {\n    return @select(m, a, b);\n}\n");
    assert_eq!(b.only_codes(), ["E0401"]);
    // Different lane counts: E1503, with a mask or with a `bool`.
    let b =
        build("fn f(c: bool, a: f32x4, b: f32x8) -> f32x4 {\n    return @select(c, a, b);\n}\n");
    assert_eq!(b.only_codes(), ["E1503"]);
    let b =
        build("fn f(m: boolx8, a: f32x4, b: f32x4) -> f32x4 {\n    return @select(m, a, b);\n}\n");
    assert_eq!(b.only_codes(), ["E1503"]);
    // A `bool` picks a whole vector.
    let b =
        build("fn f(c: bool, a: f32x4, b: f32x4) -> f32x4 {\n    return @select(c, a, b);\n}\n");
    assert_eq!(b.only_codes(), Vec::<String>::new());
    // Section 5.4 has no `as` between a vector and a scalar.
    let b = build("fn f(x: i32) -> f32x4 {\n    return x as f32x4;\n}\n");
    assert_eq!(b.only_codes(), ["E0402"]);
}

#[test]
fn a_conversion_builtin_names_a_wrong_target_type() {
    for (src, want) in [
        (
            "fn f(x: i32) -> f32 {\n    return @wrap(f32, x);\n}\n",
            "an integer type, not `f32`",
        ),
        (
            "fn f(x: f64) -> f32 {\n    return @sat(f32, x);\n}\n",
            "an integer type, not `f32`",
        ),
        (
            "fn f(x: i32) -> i32 {\n    return @nearest(i32, x);\n}\n",
            "a float type, not `i32`",
        ),
    ] {
        let b = build(src);
        assert_eq!(b.only_codes(), ["E0401"], "{src}");
        let m = &b.bodies.diagnostics[0].message;
        assert!(m.contains(want), "{src}: {m}");
    }
}

#[test]
fn unsafe_is_lexical_inside_a_closure() {
    let b = build(
        "fn f(p: *i64) -> i64 {\n    let g = unsafe(\"p is valid\") { fn [p]() -> i64 { return p.*; } };\n    return g();\n}\n",
    );
    assert_eq!(b.only_codes(), Vec::<String>::new());
    let b = build(
        "fn f(p: *i64) -> i64 {\n    let g = fn [p]() -> i64 { return p.*; };\n    return g();\n}\n",
    );
    assert_eq!(b.only_codes(), ["E1301"]);
}

#[test]
fn closures_reset_defer_and_par_for_exits() {
    let b = build(
        "fn f() -> i64 {\n    defer { let g = fn []() -> i64 { return 1; }; @assert(g() == 1); }\n    return 0;\n}\n",
    );
    assert_eq!(b.only_codes(), Vec::<String>::new());
    let b = build(
        "fn f() -> i64 {\n    par for i in 0..2 {\n        _ = i;\n        let g = fn []() -> i64 { return 1; };\n        @assert(g() == 1);\n    }\n    return 0;\n}\n",
    );
    assert_eq!(b.only_codes(), Vec::<String>::new());
    let b = build("fn f() {\n    par for i in 0..2 { _ = i; continue; }\n}\n");
    assert_eq!(b.only_codes(), Vec::<String>::new());
    let b = build("fn f() {\n    par for i in 0..2 { _ = i; loop { break; } }\n}\n");
    assert_eq!(b.only_codes(), Vec::<String>::new());
}

#[test]
fn a_static_assert_condition_that_depends_on_a_generic_is_left_for_instantiation() {
    let b = build(
        "fn f[T]() {\n    @static_assert(@sizeof(T) > 0, \"nonempty\");\n}\nfn g[const n: i64]() {\n    @static_assert(n > 0, \"positive\");\n}\n",
    );
    assert_eq!(b.only_codes(), Vec::<String>::new());
    assert!(
        !b.ni()
            .iter()
            .any(|m| m.contains("static_assert") || m.contains("S5b")),
        "{:?}",
        b.ni()
    );
    let b = build("fn f() {\n    @static_assert(false, \"boom\");\n}\n");
    assert_eq!(b.only_codes(), ["E0810"]);
    assert!(b.bodies.diagnostics[0].message.contains("boom"));
}

#[test]
fn vector_conversions_and_lane_indices() {
    let b = build(
        "fn widen(v: u8x16) -> u16x16 { return v as u16x16; }\nfn sat(v: f32x4) -> i32x4 { return @sat(i32x4, v); }\nfn wrap(v: u8x16) -> u16x16 { return @wrap(u16x16, v); }\nfn near(v: i32x4) -> f32x4 { return @nearest(f32x4, v); }\nfn same(v: u8x16) -> [16]u8 { return @bits([16]u8, v); }\nfn pick(m: boolx4, a: f32x4, b: f32x4) -> f32x4 { return @select(m, a, b); }\nfn lane(v: f32x4) -> f32 { return v[2]; }\nfn ins(v: f32x4) -> f32x4 { return @insert(v, 2, 1.0); }\n",
    );
    assert_eq!(b.only_codes(), Vec::<String>::new());
}

#[test]
fn kernel_members_and_dispatch_are_typed() {
    let b = build(
        "import std.exec;\nkernel fn dot(i: Idx, x: []f32, y: []f32) -> f32 reduce(+) {\n    let p: i32 = i.pos();\n    let q: Idx = i.offset(-1);\n    return x[i] * y[i] + x.at_clamped(q) + x.at_or(i, 0.0) + @nearest(f32, p);\n}\nfn f(x: []f32, y: []f32) -> f32 {\n    return dispatch dot(x, y) over 0..4 on exec.simd;\n}\n",
    );
    assert_eq!(b.only_codes(), Vec::<String>::new());
    assert!(!b.body("dot").failed);
    let body = b.body("f");
    let dispatch = b.exprs("f", |e| matches!(e.kind, ExprKind::Dispatch { .. }));
    let ExprRes::Dispatch(info) = &body.exprs[&dispatch[0]].res else {
        panic!("dispatch info")
    };
    assert_eq!(info.kernel, b.def("dot"));
    assert!(info.reduce);
    assert_eq!(b.display(body.exprs[&dispatch[0]].ty), "f32");
    assert_eq!(b.display(info.executor_ty), "Simd");
    let _ = &info.lo;
    let _ = &info.hi;
}

#[test]
fn a_kernel_forbids_blocking_borrowing_closures_and_unsafe() {
    let b = build(
        "fn work() { }\nkernel fn k(i: Idx, x: []f32, y: []mut f32) {\n    scope s { spawn work() in s; }\n    let r = &x[i];\n    let f = fn []() -> i32 { return 1; };\n    let n = f();\n    let u = unsafe(\"x is valid\") { x.ptr.* };\n    _ = u;\n    y[i] = r.* + @nearest(f32, n);\n}\n",
    );
    let codes = b.only_codes();
    assert!(codes.iter().all(|c| c == "E1511"), "{codes:?}");
    assert!(codes.len() >= 4, "{codes:?}");
    assert!(
        !b.ni()
            .iter()
            .any(|m| m.contains("spawn") || m.contains("par for")),
        "{:?}",
        b.ni()
    );
}

#[test]
fn a_closure_in_a_helper_is_reported_once() {
    let b = build(
        "fn h(a: f32) -> f32 { let c = fn[]() -> f32 { return 1.0; }; return a + c(); }\nkernel fn k(i: Idx, y: []mut f32) { y[i] = h(1.0); }\n",
    );
    assert_eq!(b.only_codes(), ["E1511"]);
}

#[test]
fn a_kernel_statement_dispatch_is_recorded() {
    let b = build(
        "import std.exec;\nkernel fn saxpy(i: Idx, a: f32, x: []f32, y: []mut f32) {\n    y[i] = a * x[i] + y[i];\n}\nfn f(x: []f32, y: []mut f32) {\n    dispatch saxpy(2.0, x, y) over 0..4 on exec.simd;\n}\n",
    );
    assert_eq!(b.only_codes(), Vec::<String>::new());
    let body = b.body("f");
    assert!(
        body.stmts
            .values()
            .any(|s| matches!(s, StmtInfo::Dispatch(info) if !info.reduce))
    );
}

#[test]
fn kernel_calls_of_trapping_helpers_fail_only_the_helper() {
    let b = build(
        "kernel fn inc(a: i32) -> i32 { return a + 1; }\nkernel fn k(i: Idx, n: i32) -> i32 reduce(+) { return inc(n); }\nkernel fn k2(i: Idx, n: i32) -> i32 reduce(+) { return inc(n); }\n",
    );
    assert_eq!(b.only_codes(), ["E1512"]);
    assert!(!b.body("k").failed);
    assert!(!b.body("k2").failed);
    assert!(b.body("inc").failed);
}

#[test]
fn non_trapping_kernel_operations_are_accepted() {
    let b = build(
        "kernel fn k(i: Idx, n: i32, x: []i32, y: []mut i32) {\n    let a = @div_or(x[i], n, 0);\n    let b = @rem_or(a, n, 0);\n    let c = @shl(b, n);\n    y[i] = c +% 1;\n    let f: f32 = @nearest(f32, c);\n    _ = f;\n}\n",
    );
    assert_eq!(b.only_codes(), Vec::<String>::new());
}

#[test]
fn dispatch_storage_mismatch_is_e1518() {
    let b = build(
        "import std.exec;\nkernel fn k(i: Idx, x: []f32) { }\nfn f(x: f32) {\n    dispatch k(x) over 0..4 on exec.seq;\n}\n",
    );
    assert_eq!(b.only_codes(), ["E0401", "E1518"]);
}

#[test]
fn lanes_is_a_constant_in_an_index() {
    let b = build("fn f(v: f32x4) -> f32 {\n    return v[@lanes(f32x4) - 1];\n}\n");
    assert_eq!(b.only_codes(), Vec::<String>::new());
}

#[test]
fn an_extern_fn_pointer_call_uses_its_unsafe_block() {
    let b = build(
        "extern \"c\" { fn puts(s: *u8) -> i32; }\nfn f(s: *u8) -> i32 {\n    let p = puts;\n    return unsafe(\"p is puts\") { p(s) };\n}\n",
    );
    assert_eq!(b.only_codes(), Vec::<String>::new());
}

#[test]
fn an_unsafe_fn_value_inside_unsafe_is_e1305_and_e1304() {
    let b = build(
        "unsafe fn g() { }\nfn f() {\n    let p = unsafe(\"nothing\") { g };\n    _ = p;\n}\n",
    );
    assert_eq!(b.only_codes(), ["E1304", "E1305"]);
}

#[test]
fn an_index_type_written_in_a_non_kernel_body_is_e1510() {
    let b = build("fn f() {\n    let i: Idx = @zero;\n    _ = i;\n}\n");
    assert_eq!(b.only_codes(), ["E1510"]);
}

#[test]
fn float_arithmetic_and_constant_shifts_are_allowed_in_a_kernel() {
    let b = build(
        "kernel fn k(i: Idx, n: i32, x: []f32, y: []mut f32) {\n    let a = -x[i];\n    let b = x[i] / 2.0;\n    let c = n << 3;\n    y[i] = a + b + @nearest(f32, c);\n}\n",
    );
    assert_eq!(b.only_codes(), Vec::<String>::new());
}

#[test]
fn idx_has_the_properties_and_layout_of_a_kernel_index() {
    let mut b = build("kernel fn k(i: Idx) {\n    let n: i64 = @sizeof(Idx);\n    _ = n;\n}\n");
    assert_eq!(b.only_codes(), Vec::<String>::new());
    let body = b.body("k");
    let ty = body
        .locals
        .iter()
        .find(|(l, _)| b.package().local(**l).name == "i")
        .map(|(_, i)| i.ty)
        .expect("the index parameter");
    let p = b.bodies.declarations.props(ty);
    assert_eq!(p.copy, crate::sigs::Tri::Yes);
    assert_eq!(p.discard(), crate::sigs::Tri::Yes);
    assert_eq!(p.send, crate::sigs::Tri::Yes);
    assert_eq!(p.sync, crate::sigs::Tri::Yes);
    assert_eq!(p.view, crate::sigs::Tri::No);
    let layout = b.bodies.declarations.layout_of(ty).expect("a layout");
    assert_eq!((layout.size, layout.align), (4, 4));
}

#[test]
fn a_kernel_helper_called_from_a_kernel_is_accepted() {
    let b = build(
        "kernel fn scale(a: f32) -> f32 { return a * 2.0; }\nkernel fn k(i: Idx, x: []f32, y: []mut f32) {\n    y[i] = scale(x[i]);\n}\n",
    );
    assert_eq!(b.only_codes(), Vec::<String>::new());
}

#[test]
fn a_plain_function_called_from_a_kernel_is_e1511() {
    let b = build(
        "fn scale(a: f32) -> f32 { return a * 2.0; }\nkernel fn k(i: Idx, x: []f32, y: []mut f32) {\n    y[i] = scale(x[i]);\n}\n",
    );
    assert_eq!(b.only_codes(), ["E1511"]);
}

#[test]
fn a_wildcard_under_a_borrow_discards_only_the_borrow() {
    let b = build(
        "linear struct L { x: i64 }\nenum E { a: L, b }\nfn f(e: &E, o: &?L) -> i64 {\n    let n = match e { .a(_) => 1, .b => 2 };\n    let m = match o { .some(_) => 1, .none => 2 };\n    return n + m;\n}\nfn g(e: E) -> i64 {\n    return match e { .a(_) => 1, .b => 2 };\n}\n",
    );
    // Only the `_` in `g`, which matches the linear payload by value.
    assert_eq!(b.only_codes(), ["E0414"]);
}

#[test]
fn a_kernel_matches_a_payload_free_enum() {
    let b = build(
        "enum E { a, b }\nkernel fn k(i: Idx) {\n    let e: E = .a;\n    match e { .a => { } .b => { } }\n}\n",
    );
    assert_eq!(b.only_codes(), Vec::<String>::new());
}

#[test]
fn a_rest_pattern_records_the_fields_it_leaves_out() {
    let b = build(
        "copy struct P { x: i64, y: i64, z: i64 }\nfn f(p: P) -> i64 {\n    let P.{ x, .. } = p;\n    return x;\n}\n",
    );
    assert_eq!(b.only_codes(), Vec::<String>::new());
    let info = b
        .body("f")
        .pats
        .values()
        .find(|i| matches!(i.res, PatRes::Struct { rest: true, .. }))
        .expect("a rest pattern");
    let PatRes::Struct { res, rest, omitted } = &info.res else {
        panic!("a struct pattern")
    };
    assert!(*rest);
    assert_eq!(res.order, [0]);
    assert_eq!(omitted, &[1, 2]);
    // Exhaustiveness treats the left-out fields as wildcards.
    let b = build(
        "copy struct P { a: bool, b: bool }\nfn f(p: P) -> i64 {\n    return match p { P.{ a: true, .. } => 1, P.{ a: false, .. } => 2 };\n}\n",
    );
    assert_eq!(b.only_codes(), Vec::<String>::new());
    // Without `..`, leaving a field out is E0603 on the pattern.
    let b = build(
        "copy struct P { a: i64, b: i64 }\nfn f(p: P) -> i64 {\n    let P.{ a } = p;\n    return a;\n}\n",
    );
    assert_eq!(b.only_codes(), ["E0603"]);
}

#[test]
fn a_failed_body_is_marked_and_a_good_one_is_not() {
    let b = build("fn good() -> i64 { return 1; }\nfn bad() -> i64 { return true; }\n");
    assert!(!b.body("good").failed);
    assert!(b.body("bad").failed);
}

#[test]
fn a_var_that_is_never_mutated() {
    let b =
        build("fn f() -> i64 {\n    var a = 1;\n    var b = 2;\n    b += a;\n    return b;\n}\n");
    assert_eq!(b.codes(), vec![("E0312".to_string(), 2)]);
    // A `var` mutated through a method receiver, a borrow or a part.
    let b = with(
        "fn f() -> i64 {\n    var c = Counter.new();\n    c.bump();\n    var d = Counter.new();\n    let r = &mut d;\n    r.bump();\n    var e = (1, 2);\n    e.0 = 3;\n    return c.get() + e.0 + d.get();\n}\n",
    );
    assert_eq!(b.only_codes(), Vec::<String>::new());
    // A `var` holding a borrow that is only used through it is not mutated.
    let b = with(
        "fn f(c: &mut Counter) -> i64 {\n    var r = c;\n    r.bump();\n    return r.get();\n}\n",
    );
    assert_eq!(b.codes(), vec![("E0312".to_string(), 11)]);
}

#[test]
fn phase_six_errors_are_not_repeated() {
    let b = build("struct S { x: i64 }\nstruct S { y: i64 }\nfn f() -> i64 { return 1; }\n");
    assert!(!b.out.diagnostics.is_empty());
    assert_eq!(b.phase6(), 0);
    assert!(b.only_codes().is_empty());
}

// ---------------------------------------------------- closures, errors, the rest

#[test]
fn a_function_value_fixes_the_parameters_of_its_bound() {
    let b = build(
        "fn run[T, U, F: Fn(T) -> U](xs: []T, f: F) -> U {\n    return f(xs[0]);\n}\nfn a(xs: []i64) -> bool {\n    return run(xs, fn [](x: i64) -> bool { return x > 0; });\n}\nfn b(xs: []u8) -> i64 {\n    return run(xs, fn [](x: u8) -> i64 { return x as i64; });\n}\n",
    );
    assert_eq!(b.only_codes(), Vec::<String>::new());
    // A closure whose signature does not match the bound.
    let b = build(
        "fn apply[F: Fn(i64) -> i64](f: F, x: i64) -> i64 {\n    return f(x);\n}\nfn a() -> i64 {\n    return apply(fn [](x: i64) -> bool { return true; }, 1);\n}\n",
    );
    assert_eq!(b.only_codes(), ["E0801"]);
}

#[test]
fn a_closure_has_its_own_type_and_the_properties_of_its_captures() {
    let b = build(
        "fn f() -> i64 {\n    var total: i64 = 0;\n    let k = 2;\n    let add = fn [k, &mut total](x: i64) { total.* += x * k; };\n    add(1);\n    return total;\n}\n",
    );
    assert_eq!(b.only_codes(), Vec::<String>::new());
    let body = b.body("f");
    assert_eq!(body.closures.len(), 1);
    let info = body.closures.values().next().unwrap();
    assert_eq!(info.captures.len(), 2);
    assert_eq!(info.captures[0].mode, CaptureMode::ByValue);
    assert_eq!(info.captures[1].mode, CaptureMode::Exclusive);
    let props = b.bodies.declarations.props[&info.ty];
    assert_eq!(props.copy, crate::sigs::Tri::No);
    assert_eq!(props.view, crate::sigs::Tri::Yes);
    let calls = b.calls("f");
    assert!(matches!(
        &calls[0].1.callee,
        Callee::Value(ValueCall::Closure(_))
    ));
}

#[test]
fn a_closure_layout_is_its_capture_record() {
    let mut b = build(
        "fn f() -> i64 {\n    let k = 2;\n    var total: i64 = 0;\n    let add = fn [k, &mut total](x: i64) -> i64 { total.* += x * k; return total.*; };\n    return add(1);\n}\n",
    );
    assert_eq!(b.only_codes(), Vec::<String>::new());
    let info = b.body("f").closures.values().next().cloned().unwrap();
    let l = b
        .bodies
        .declarations
        .layout_of(info.ty)
        .expect("a closure layout");
    assert_eq!((l.size, l.align), (16, 8));
    assert_eq!(
        l.fields.iter().map(|f| f.offset).collect::<Vec<_>>(),
        vec![0, 8]
    );
    let mut empty = build(
        "fn f() -> i64 {\n    let k = fn [](x: i64) -> i64 { return x; };\n    return k(1);\n}\n",
    );
    let info = empty.body("f").closures.values().next().cloned().unwrap();
    let l = empty
        .bodies
        .declarations
        .layout_of(info.ty)
        .expect("an empty closure layout");
    assert_eq!((l.size, l.align), (0, 1));
}

#[test]
fn a_capture_by_exclusive_borrow_needs_a_var() {
    let b = build(
        "fn f() -> i64 {\n    let total: i64 = 0;\n    let add = fn [&mut total](x: i64) { total.* += x; };\n    add(1);\n    return total;\n}\n",
    );
    assert_eq!(b.codes(), vec![("E0610".to_string(), 3)]);
}

#[test]
fn the_rules_of_this_step_are_errors() {
    let b = build(
        "enum E { bad }\nfn g() -> i64 ! E { return 1; }\nfn f() -> i64 {\n    return try g();\n}\n",
    );
    assert_eq!(b.only_codes(), ["E0702"]);
    assert!(b.body("f").exprs.values().any(|_| true));
    let b = build("fn f() -> i64 {\n    return unsafe(\"nothing\") { 1 };\n}\n");
    assert_eq!(b.only_codes(), ["E1304"]);
    let b = build("enum E { bad }\nfn g() -> i64 ! E { return 1; }\nfn f() {\n    g();\n}\n");
    assert_eq!(b.only_codes(), ["E0701"]);
}

#[test]
fn a_body_that_passes_is_left_for_the_ownership_phases() {
    // Phase 7 leaves every body it checked to phase 8; that phase's own message is
    // recorded by `ingot-cfg`, not here.
    let b = build("fn f() -> i64 { return 1; }\n");
    assert!(b.ni().is_empty(), "{:?}", b.ni());
    let b = build("const c: i64 = 3;\nstatic s: i64 = 4;\n");
    assert!(b.ni().is_empty(), "{:?}", b.ni());
    let b = build("fn f() -> i64 { return true; }\n");
    assert!(b.ni().is_empty(), "{:?}", b.ni());
}

#[test]
fn target_values_have_types() {
    let b =
        build("fn f() -> bool {\n    return @target.os == .macos && @target.simd_bits > 64;\n}\n");
    assert_eq!(b.only_codes(), Vec::<String>::new());
    let b = build("fn f() -> bool {\n    return @target.os == .plan9;\n}\n");
    assert_eq!(b.only_codes(), ["E0314"]);
}

#[test]
fn optionals_and_results_in_patterns() {
    let m = |arms: &str| {
        matches(&format!(
            "fn f(r: ?i64) -> i64 {{ return match r {{ {arms} }}; }}\n"
        ))
    };
    assert!(m(".some(v) => v, .none => 0,").is_empty());
    assert_eq!(m(".some(v) => v,"), ["E0601"]);
    let m = |arms: &str| {
        matches(&format!(
            "fn g() -> i64 ! u8 {{ return 1; }}\nfn f() -> i64 {{ return match g() {{ {arms} }}; }}\n"
        ))
    };
    assert!(m(".ok(v) => v, .err(e) => e as i64,").is_empty());
    assert_eq!(m(".ok(v) => v,"), ["E0601"]);
}

#[test]
fn spawn_gives_a_task_of_the_result_type() {
    let b = build(
        "import std.task.Scope;\nfn work(n: i64) -> i64 { return n; }\nfn f() -> i64 {\n    scope s {\n        let t = spawn work(1) in s;\n        return 0;\n    }\n}\n",
    );
    // The scope rules are the next step's; the type is decided here.
    assert!(b.only_codes().is_empty(), "{:?}", b.codes());
    let body = b.body("f");
    let spawn = b.exprs("f", |e| matches!(e.kind, ExprKind::Spawn { .. }));
    assert_eq!(b.display(body.exprs[&spawn[0]].ty), "Task[i64]");
}

#[test]
fn variadic_arguments_are_checked() {
    let b = build(
        "extern \"c\" {\n    fn printf(fmt: *u8, ...) -> i32;\n}\nfn f(a: u8, b: f32, c: i64, d: bool) -> i32 {\n    return unsafe(\"literal format\") { printf(c\"%d\", a, b, c, d, 1.5) };\n}\n",
    );
    assert_eq!(
        b.codes(),
        vec![
            ("E1602".to_string(), 5),
            ("E1602".to_string(), 5),
            ("E1602".to_string(), 5)
        ]
    );
}

#[test]
fn a_match_on_a_type_without_values_is_exhaustive() {
    assert!(
        matches("enum Never2 { }\nfn f(n: Never2) -> i64 { return match n { }; }\n").is_empty()
    );
}

#[test]
fn a_literal_operand_does_not_hide_a_missing_bound() {
    // `a + 1` on an unbounded `T` is the missing bound, not a mismatch of `T` and `i64`.
    let b =
        build("fn f[T](a: T) -> T { return a + 1; }\nfn g[T: Num](a: T) -> T { return a + 1; }\n");
    assert_eq!(b.codes(), vec![("E0802".to_string(), 1)]);
}

#[test]
fn format_arguments_are_shared_reads() {
    let b = build("fn f(x: i64) { @debug(\"{} {}\", x, x); }\n");
    assert_eq!(b.only_codes(), Vec::<String>::new());
    let body = b.body("f");
    let args = b.exprs("f", |e| matches!(e.kind, ExprKind::Path(_)));
    assert_eq!(args.len(), 2);
    assert!(args.iter().all(|e| body.exprs[e].shared_read));
}

#[test]
fn gather_and_scatter_record_their_memory_checks() {
    let b = build(
        "fn f(s: []f32, t: []mut f32, idx: i32x4) {\n    let v = @gather(f32x4, s, idx);\n    @scatter(t, idx, v);\n}\n",
    );
    assert_eq!(b.only_codes(), Vec::<String>::new());
    let body = b.body("f");
    assert!(
        body.exprs
            .values()
            .any(|i| matches!(i.res, ExprRes::Gather))
    );
    assert!(
        body.exprs
            .values()
            .any(|i| matches!(i.res, ExprRes::Scatter))
    );
}

#[test]
fn an_inherent_method_named_as_a_value_is_an_inherent_fn_ref() {
    let b = build(
        "struct P { x: i64 }\nimpl P { fn get(self: &P) -> i64 { return self.x; } }\nfn f(p: &P) -> i64 { let g = P.get; return g(p); }\n",
    );
    assert_eq!(b.only_codes(), Vec::<String>::new());
    let body = b.body("f");
    let fns: Vec<&FnRef> = body
        .exprs
        .values()
        .filter_map(|i| match &i.res {
            ExprRes::Fn(f) => Some(f),
            _ => None,
        })
        .collect();
    assert!(
        fns.iter()
            .any(|f| matches!(f.source, FnSource::Inherent { .. }))
    );
}

#[test]
fn std_declares_shared_refs_and_take_on_builtin_types() {
    let b = build(
        "struct Item { v: i64 }\nfn total(xs: []Item) -> i64 {\n    var t: i64 = 0;\n    for x in xs.refs() { t += x.v; }\n    return t;\n}\nfn g(ys: []mut Item) -> i64 {\n    var t: i64 = 0;\n    for y in ys.shared().refs() { t += y.v; }\n    return t;\n}\nfn f(s: []mut i64, o: &mut ?i64) -> i64 {\n    let a = s.shared();\n    let b = o.take();\n    _ = a;\n    _ = b;\n    return 0;\n}\n",
    );
    assert_eq!(b.only_codes(), Vec::<String>::new());
}

#[test]
fn unchecked_index_is_a_place() {
    let b = build(
        "fn f(xs: []mut i64) {\n    unsafe(\"the indices are in bounds\") {\n        @unchecked_index(xs, 0) = 1;\n        let p = &mut @unchecked_index(xs, 1);\n        p.* = 2;\n    }\n}\n",
    );
    assert_eq!(b.only_codes(), Vec::<String>::new());
    let body = b.body("f");
    assert!(
        body.exprs
            .values()
            .any(|i| matches!(i.res, ExprRes::Index { .. }))
    );
}

#[test]
fn zero_takes_no_argument_besides_its_type() {
    let b = build("fn f() -> i64 { return @zero(i64, 1); }\n");
    assert_eq!(b.only_codes(), vec!["E0410".to_string()]);
}

#[test]
fn a_format_placeholder_after_an_escape_is_reported_at_the_literal() {
    let b = build("fn f(x: i64) { @debug(\"\\n{:q}\", x); }\n");
    assert_eq!(b.only_codes(), vec!["E0422".to_string()]);
    let d = &b.bodies.diagnostics[0];
    // The literal starts at byte 22 and is 8 bytes long with its quotes.
    assert_eq!((d.span.start, d.span.end), (22, 30));
}

#[test]
fn an_interface_call_types_each_argument_once() {
    let b = build(
        "interface Two { fn two(a: &Self, n: i32) -> i32; }\nstruct P { x: i64 }\nimpl Two for P { fn two(a: &P, n: i32) -> i32 { return n; } }\nfn f(p: &P) -> i32 { return Two.two(p, 5); }\n",
    );
    assert_eq!(b.only_codes(), Vec::<String>::new());
    let body = b.body("f");
    // `5` is typed once, against the parameter: `i32`.
    let lits = b.exprs("f", |e| matches!(e.kind, ExprKind::Int { .. }));
    assert_eq!(lits.len(), 1);
    let i32_ = body.exprs[&lits[0]].ty;
    assert_eq!(b.display(i32_), "i32");
}

#[test]
fn pub_pkg_fields_are_not_visible_from_another_package() {
    let mut sources = Sources::default();
    let add = |name: &str, modules: &[(&str, &str)], sources: &mut Sources| {
        let mut mods = Vec::new();
        for (rel, text) in modules {
            let path = if rel.is_empty() {
                name.to_string()
            } else {
                format!("{name}.{rel}")
            };
            let file = sources.add(Source::new(format!("{path}.ingot"), *text));
            let source = sources.get(file);
            let (tokens, mut diags) = lex(source);
            let (ast, parse_diags) = parse(source, tokens);
            diags.extend(parse_diags);
            assert!(diags.is_empty(), "{path} must parse: {diags:?}");
            mods.push(ModuleInput {
                path,
                file,
                ast: Some(ast),
            });
        }
        PackageInput {
            name: name.to_string(),
            deps: Vec::new(),
            modules: mods,
        }
    };
    let inputs = vec![
        add("std", &STD, &mut sources),
        add(
            "lib",
            &[("", "pub struct S { pub(pkg) x: i64 }\n")],
            &mut sources,
        ),
        add(
            "app",
            &[(
                "",
                "import lib.S;\n\
                 fn read(s: &S) -> i64 { return s.x; }\n\
                 fn write(s: &mut S) { s.x = 1; }\n\
                 fn literal() -> S { return S.{ x: 1 }; }\n",
            )],
            &mut sources,
        ),
    ];
    let mut inputs = inputs;
    let names: Vec<String> = inputs.iter().map(|p| p.name.clone()).collect();
    for input in inputs.iter_mut() {
        input.deps = names
            .iter()
            .filter(|name| **name != input.name && name.as_str() != "std")
            .cloned()
            .collect();
    }
    let target = Target::by_name("aarch64-macos-m4").expect("a target");
    let out = resolve(&mut sources, inputs, &target);
    let decls = check_declarations(&sources, &out, &target, &[2], None);
    let bodies = check_bodies(&sources, &out, decls, &target, &[2], None);
    let mut codes: Vec<(String, u32)> = bodies
        .diagnostics
        .iter()
        .map(|d| {
            let line = sources
                .iter()
                .find(|s| s.path == d.file)
                .map(|s| s.position(d.span.start).line)
                .unwrap_or(0);
            (d.code.to_string(), line)
        })
        .collect();
    codes.sort();
    assert_eq!(
        codes,
        vec![
            ("E0307".to_string(), 2),
            ("E0307".to_string(), 3),
            ("E0315".to_string(), 4)
        ]
    );
}

#[test]
fn a_math_builtin_on_a_type_parameter_is_not_implemented() {
    let b = build("fn f[T: Float](x: T) -> T { return @sqrt(x); }\n");
    assert_eq!(b.only_codes(), Vec::<String>::new());
    assert!(b.ni().iter().any(|m| m.contains("on a type parameter")));
}
