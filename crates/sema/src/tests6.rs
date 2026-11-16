//! Unit tests for phase 6: the type table, signatures, type properties, layouts,
//! constant evaluation and the impl tables.

use crate::consteval::{ConstState, Fail, Trap, int_binary, int_unary, lossless, wrap};
use crate::defs::DefId;
use crate::sigs::{ImplOf, ItemSig, LayoutKind, Tri};
use crate::ty::{
    ConstArg, ConstValue, GenericArg, IntTy, Mutability, Subst, TyKind, TypeId, TypeTable,
};
use crate::{Declarations, ModuleInput, Output, PackageInput, Target, check_declarations, resolve};
use ingot_diag::source::{Source, Sources};
use ingot_syntax::ast::{BinOp, UnOp};
use ingot_syntax::lexer::lex;
use ingot_syntax::parser::parse;

const STD: [(&str, &str); 14] = [
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
    ("task", include_str!("../../../std/task.ingot")),
    ("time", include_str!("../../../std/time.ingot")),
];

/// The part of `std` the builtin declarations need, for tests that do not load all of it.
const STD_MIN: [(&str, &str); 2] = [
    ("fmt", include_str!("../../../std/fmt.ingot")),
    ("hash", include_str!("../../../std/hash.ingot")),
];

struct D {
    out: Output,
    decls: Declarations,
    sources: Sources,
}

fn build(with_std: bool, text: &str) -> D {
    let mut sources = Sources::default();
    let mut inputs = Vec::new();
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
            modules: mods,
        }
    };
    // Every compilation has a `std`, since the builtin interfaces are declared in it.
    // The program's file comes first among the sources, so that its spans are easy to
    // find.
    let app_pkg = add("app", &[("", text)], &mut sources);
    inputs.push(add(
        "std",
        if with_std { &STD } else { &STD_MIN },
        &mut sources,
    ));
    let app = inputs.len();
    inputs.push(app_pkg);
    let target = Target::by_name("aarch64-macos-m4").expect("a target");
    let out = resolve(&mut sources, inputs, &target);
    let decls = check_declarations(&sources, &out, &target, app, None);
    D {
        out,
        decls,
        sources,
    }
}

fn app(text: &str) -> D {
    build(false, text)
}

fn app_with_std(text: &str) -> D {
    build(true, text)
}

impl D {
    /// (code, line) of every diagnostic of phase 6, sorted.
    fn codes(&self) -> Vec<(String, u32)> {
        let mut v: Vec<(String, u32)> = self
            .decls
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

    fn codes_only(&self) -> Vec<String> {
        self.codes().into_iter().map(|(c, _)| c).collect()
    }

    fn root(&self, package: &str) -> crate::defs::ModuleId {
        *self
            .out
            .sema
            .module_paths
            .get(package)
            .unwrap_or_else(|| panic!("module {package}"))
    }

    fn type_def(&self, module: &str, name: &str) -> DefId {
        *self
            .out
            .sema
            .module(self.root(module))
            .types
            .get(name)
            .unwrap_or_else(|| panic!("type {name}"))
    }

    fn value_def(&self, name: &str) -> DefId {
        *self
            .out
            .sema
            .module(self.root("app"))
            .values
            .get(name)
            .unwrap_or_else(|| panic!("value {name}"))
    }

    /// The type of a declaration of `app` without generic parameters.
    fn adt(&mut self, name: &str) -> TypeId {
        let d = self.type_def("app", name);
        self.decls.types.intern(TyKind::Adt(d, vec![]))
    }

    fn adt_of(&mut self, module: &str, name: &str, args: Vec<GenericArg>) -> TypeId {
        let d = self.type_def(module, name);
        self.decls.types.intern(TyKind::Adt(d, args))
    }

    fn int(&mut self, i: IntTy) -> TypeId {
        self.decls.types.int(i)
    }

    /// The type of the field `field` of the struct `name` of `app`.
    fn field(&self, name: &str, field: &str) -> TypeId {
        let d = self.type_def("app", name);
        let s = self.decls.struct_sig(d).expect("a struct");
        s.fields
            .iter()
            .find(|f| f.name == field)
            .unwrap_or_else(|| panic!("field {field}"))
            .ty
    }

    fn display(&self, ty: TypeId) -> String {
        self.decls.types.display(&self.out.sema, ty)
    }

    fn const_value(&self, name: &str) -> Option<ConstValue> {
        let d = self.value_def(name);
        match self.decls.consts.get(&d) {
            Some(ConstState::Done(Ok(v))) => Some(*v),
            _ => None,
        }
    }

    fn const_fail(&self, name: &str) -> Option<Fail> {
        let d = self.value_def(name);
        match self.decls.consts.get(&d) {
            Some(ConstState::Done(Err(f))) => Some(f.clone()),
            _ => None,
        }
    }

    fn ni(&self) -> &[String] {
        &self.decls.not_implemented
    }

    /// The gaps recorded other than "there are bodies", which any test with a function
    /// does have.
    fn gaps(&self) -> Vec<&String> {
        self.decls
            .not_implemented
            .iter()
            .filter(|m| m.as_str() != crate::UNCHECKED_BODIES)
            .collect()
    }

    fn has_ni(&self, part: &str) -> bool {
        self.decls.not_implemented.iter().any(|m| m.contains(part))
    }
}

fn int(ty: IntTy, value: i128) -> ConstValue {
    ConstValue::Int { ty, value }
}

// ------------------------------------------------------------- the type table

#[test]
fn equal_types_are_the_same_id() {
    let mut t = TypeTable::new();
    let i = t.int(IntTy::I64);
    assert_eq!(i, t.i64_);
    let a = t.intern(TyKind::Optional(i));
    let b = t.intern(TyKind::Optional(i));
    assert_eq!(a, b);
    let r1 = t.intern(TyKind::Ref(Mutability::Shared, i));
    let r2 = t.intern(TyKind::Ref(Mutability::Mut, i));
    assert_ne!(r1, r2);
    let tup1 = t.intern(TyKind::Tuple(vec![i, t.bool_]));
    let tup2 = t.intern(TyKind::Tuple(vec![i, t.bool_]));
    let tup3 = t.intern(TyKind::Tuple(vec![t.bool_, i]));
    assert_eq!(tup1, tup2);
    assert_ne!(tup1, tup3);
}

#[test]
fn instances_of_a_generic_type_are_equal_when_their_arguments_are() {
    let mut d = app(
        "struct Pair[A, B] { a: A, b: B }\nstruct H { x: Pair[i64, u8], y: Pair[i64, u8], z: Pair[u8, i64] }\n",
    );
    assert_eq!(d.codes(), vec![]);
    assert_eq!(d.field("H", "x"), d.field("H", "y"));
    assert_ne!(d.field("H", "x"), d.field("H", "z"));
    let _ = d.int(IntTy::I8);
}

#[test]
fn types_print_as_source() {
    let d = app("struct Pair[A, B] { a: A, b: B }\n\
         interface Shape { fn area(self: &Self) -> i64; }\n\
         type Bytes = []u8;\n\
         linear struct H {\n\
             a: Pair[i64, ?u8],\n\
             b: &mut []mut Bytes,\n\
             c: [4]*mut u16,\n\
             d: (bool, f32),\n\
             e: fn(i64, u8) -> bool,\n\
             f: extern fn(*u8) -> i32,\n\
             g: &dyn Shape,\n\
             h: Result[i64, u8],\n\
             i: f32x4,\n\
             j: boolx8,\n\
         }\n");
    assert_eq!(d.codes(), vec![]);
    let want = [
        ("a", "Pair[i64, ?u8]"),
        ("b", "&mut []mut []u8"),
        ("c", "[4]*mut u16"),
        ("d", "(bool, f32)"),
        ("e", "fn(i64, u8) -> bool"),
        ("f", "extern fn(*u8) -> i32"),
        ("g", "&dyn Shape"),
        ("h", "i64 ! u8"),
        ("i", "f32x4"),
        ("j", "boolx8"),
    ];
    for (f, text) in want {
        assert_eq!(d.display(d.field("H", f)), text, "field {f}");
    }
}

#[test]
fn an_alias_is_expanded() {
    let d = app("type Bytes = []u8;\n\
         type Pair2[T] = (T, T);\n\
         struct H { a: Bytes, b: []u8, c: Pair2[i64], d: (i64, i64) }\n");
    assert_eq!(d.codes(), vec![]);
    assert_eq!(d.field("H", "a"), d.field("H", "b"));
    assert_eq!(d.field("H", "c"), d.field("H", "d"));
}

#[test]
fn substitution_replaces_parameters_and_constants() {
    let mut d = app("struct Ring[T, const n: i64] { items: [n]T, head: i64 }\n");
    assert_eq!(d.codes(), vec![]);
    let ring = d.type_def("app", "Ring");
    let sig = d.decls.struct_sig(ring).expect("Ring").clone();
    let items = sig.fields[0].ty;
    assert_eq!(d.display(items), "[n]T");
    let g = d.decls.generics_of(ring).to_vec();
    let i16_ty = d.int(IntTy::I16);
    let mut s = Subst::new();
    s.insert(g[0].def, GenericArg::Ty(i16_ty));
    s.insert(
        g[1].def,
        GenericArg::Const(ConstArg::Value(int(IntTy::I64, 3))),
    );
    let got = d.decls.types.subst(items, &s);
    assert_eq!(d.display(got), "[3]i16");
    // Only the parameters in the map change.
    let mut only_t = Subst::new();
    only_t.insert(g[0].def, GenericArg::Ty(i16_ty));
    let got = d.decls.types.subst(items, &only_t);
    assert_eq!(d.display(got), "[n]i16");
    // An empty substitution is the identity, with the same id.
    assert_eq!(d.decls.types.subst(items, &Subst::new()), items);
}

#[test]
fn a_poison_type_in_a_signature_is_silent() {
    let d = app("struct S { a: Missing, b: i64 }\n\
         copy struct C { a: Missing }\n\
         static g: Missing3 = 0;\n\
         impl Missing2 for S {}\n");
    // E0305/E0304 are phase 5's; phase 6 adds nothing about the poisoned items.
    assert_eq!(d.codes(), vec![]);
}

// ------------------------------------------------------------- generic arguments

#[test]
fn argument_counts_are_checked_e0804_e0805() {
    let d = app("struct Pair[A, B] { a: A, b: B }\n\
         struct Plain { x: i64 }\n\
         struct H1 { p: Pair[i64] }\n\
         struct H2 { p: Pair[i64, i64, i64] }\n\
         struct H3 { p: Pair }\n\
         struct H4 { p: Plain[i64] }\n");
    let got = d.codes();
    assert!(got.contains(&("E0804".to_string(), 3)), "{got:?}");
    assert!(got.contains(&("E0804".to_string(), 4)), "{got:?}");
    assert!(got.contains(&("E0804".to_string(), 5)), "{got:?}");
    assert!(got.contains(&("E0805".to_string(), 6)), "{got:?}");
}

#[test]
fn e0805_offers_to_delete_the_arguments() {
    let d = app("struct Plain { x: i64 }\nstruct H { p: Plain[i64] }\n");
    let diag = &d.decls.diagnostics[0];
    assert_eq!(diag.code, "E0805");
    let edit = &diag.fixes[0].edits[0];
    assert_eq!(edit.text, "");
    let text = &d.sources.get(0).text;
    assert_eq!(&text[edit.span.start..edit.span.end], "[i64]");
}

#[test]
fn a_bound_is_checked_on_the_arguments_of_a_type_e0801() {
    let d = app_with_std(
        "import std.sync.Mutex;\n\
         struct Plain { x: i64 }\n\
         copy struct Dot { x: i64 }\n\
         struct Needs[T: Copy] { t: T }\n\
         struct A { m: Mutex[&i64] }\n\
         struct B { m: Mutex[i64] }\n\
         struct C { n: Needs[Plain] }\n\
         struct E { n: Needs[Dot] }\n\
         struct F[T] { n: Needs[T] }\n\
         struct G[T: Copy] { n: Needs[T] }\n",
    );
    assert_eq!(
        d.codes(),
        vec![
            ("E0801".to_string(), 5),
            ("E0801".to_string(), 7),
            ("E0801".to_string(), 9)
        ]
    );
}

#[test]
fn owned_is_not_granted_by_an_unbounded_parameter() {
    let d = app_with_std(
        "import std.sync.Mutex;\n\
         struct Ok1[T: Owned] { m: Mutex[T] }\n\
         struct Bad[T] { m: Mutex[T] }\n",
    );
    assert_eq!(d.codes(), vec![("E0801".to_string(), 3)]);
}

#[test]
fn interface_bounds_use_the_impl_tables() {
    let d = app("interface Tag { fn tag(self: &Self) -> u32; }\n\
         struct Set[K: Tag] { k: K }\n\
         struct NoTag { x: i64 }\n\
         struct Tagged { x: i64 }\n\
         impl Tag for Tagged { fn tag(self: &Tagged) -> u32 { return 0; } }\n\
         struct Pair[A, B] { a: A, b: B }\n\
         impl[K: Tag] Tag for Pair[K, K] { fn tag(self: &Pair[K, K]) -> u32 { return 0; } }\n\
         struct S1 { s: Set[Tagged] }\n\
         struct S2 { s: Set[NoTag] }\n\
         struct S3 { s: Set[Pair[Tagged, Tagged]] }\n\
         struct S4 { s: Set[Pair[Tagged, NoTag]] }\n\
         struct S5 { s: Set[Pair[NoTag, NoTag]] }\n");
    assert_eq!(
        d.codes(),
        vec![
            ("E0801".to_string(), 9),
            ("E0801".to_string(), 11),
            ("E0801".to_string(), 12)
        ]
    );
}

#[test]
fn a_view_type_is_rejected_where_owned_is_required_through_an_alias() {
    let d = app_with_std(
        "import std.sync.Mutex;\n\
         type Slot[T: Owned] = Mutex[T];\n\
         struct A { s: Slot[[]u8] }\n",
    );
    assert_eq!(d.codes(), vec![("E0801".to_string(), 3)]);
}

// ------------------------------------------------------------- properties

#[test]
fn scalars_pointers_and_references_have_the_properties_of_section_10() {
    let mut d = app(
        "struct H { a: *u8, b: &i64, c: &mut i64, d: []u8, e: []mut u8, f: fn(i64) -> i64, g: f32x4 }\n",
    );
    let cases = [
        // field, copy, view, send, sync
        ("a", Tri::Yes, Tri::No, Tri::No, Tri::No),
        ("b", Tri::Yes, Tri::Yes, Tri::Yes, Tri::Yes),
        ("c", Tri::No, Tri::Yes, Tri::Yes, Tri::Yes),
        ("d", Tri::Yes, Tri::Yes, Tri::Yes, Tri::Yes),
        ("e", Tri::No, Tri::Yes, Tri::Yes, Tri::Yes),
        ("f", Tri::Yes, Tri::No, Tri::Yes, Tri::Yes),
        ("g", Tri::Yes, Tri::No, Tri::Yes, Tri::Yes),
    ];
    for (f, copy, view, send, sync) in cases {
        let ty = d.field("H", f);
        let p = d.decls.props(ty);
        assert_eq!(
            (p.copy, p.view, p.send, p.sync, p.linear),
            (copy, view, send, sync, Tri::No),
            "field {f}"
        );
    }
}

#[test]
fn a_result_is_linear_and_an_option_follows_its_payload() {
    let mut d = app("linear struct Token { id: i32 }\n\
         struct H { r: Result[i64, i64], o: ?i64, ot: ?Token, a: [2]i64, t: (i64, Token) }\n");
    let r = d.field("H", "r");
    let o = d.field("H", "o");
    let ot = d.field("H", "ot");
    let a = d.field("H", "a");
    let t = d.field("H", "t");
    let p = d.decls.props(r);
    assert_eq!(
        (p.linear, p.copy, p.discard()),
        (Tri::Yes, Tri::No, Tri::No)
    );
    let p = d.decls.props(o);
    assert_eq!((p.linear, p.copy), (Tri::No, Tri::Yes));
    assert_eq!(d.decls.props(ot).linear, Tri::Yes);
    assert_eq!(d.decls.props(a).copy, Tri::Yes);
    assert_eq!(d.decls.props(t).linear, Tri::Yes);
    // A struct with a linear tuple field must be declared `linear` (E0502 is about
    // structs; tuples and arrays are linear without a keyword).
}

#[test]
fn a_box_like_type_reaches_a_fixed_point_through_its_argument_and_a_raw_pointer() {
    // `Link[T]` holds a raw pointer, so its properties come only from `unsafe impl`
    // (section 20). `Node` contains `?Link[Node]`: `Node: Send` needs `Link[Node]:
    // Send`, which needs `Node: Send`. The greatest fixed point says yes.
    let mut d = app("struct Link[T] { ptr: *mut T }\n\
         unsafe impl[T: Send] Send for Link[T] {}\n\
         struct Node { next: ?Link[Node], v: i64 }\n\
         struct Leaf { next: ?Link[Raw], v: i64 }\n\
         struct Raw { p: *u8 }\n");
    assert_eq!(d.codes(), vec![]);
    let node = d.adt("Node");
    let p = d.decls.props(node);
    assert_eq!(
        (p.send, p.sync, p.copy, p.view),
        (Tri::Yes, Tri::No, Tri::No, Tri::No)
    );
    // `Sync` has no impl: the raw pointer inside decides.
    let link_node = d.field("Node", "next");
    assert_eq!(d.decls.props(link_node).sync, Tri::No);
    // `Raw` is not Send, so `Link[Raw]` is not, so `Leaf` is not.
    let leaf = d.adt("Leaf");
    assert_eq!(d.decls.props(leaf).send, Tri::No);
}

#[test]
fn only_an_unsafe_impl_states_send_or_sync() {
    let mut d = app("struct Holder { p: *u8 }\n\
         impl Send for Holder {}\n\
         struct Safe { p: *u8 }\n\
         unsafe impl Send for Safe {}\n");
    // Without `unsafe` the impl is E1303 and grants nothing.
    assert_eq!(d.codes(), vec![("E1303".to_string(), 2)]);
    let holder = d.adt("Holder");
    assert_eq!(d.decls.props(holder).send, Tri::No);
    let safe = d.adt("Safe");
    assert_eq!(d.decls.props(safe).send, Tri::Yes);
    assert_eq!(d.decls.props(safe).sync, Tri::No);
}

#[test]
fn a_type_parameter_has_exactly_what_its_bounds_grant() {
    let mut d = app(
        "struct S[A, B: Copy, C: Num, D: Send + Sync + Owned, E: Discard] { a: A, b: B, c: C, d: D, e: E }\n\
         interface Cheap: Copy { fn f(self: &Self) -> i64; }\n\
         struct T[X: Cheap] { x: X }\n",
    );
    assert_eq!(d.codes(), vec![]);
    let s = d.type_def("app", "S");
    let g: Vec<DefId> = d.decls.generics_of(s).iter().map(|g| g.def).collect();
    let mut prop = |i: usize| {
        let p = d.decls.types.param(g[i]);
        d.decls.props(p)
    };
    let a = prop(0);
    assert_eq!(
        (a.copy, a.maybe_linear, a.maybe_view, a.send, a.sync),
        (Tri::No, Tri::Yes, Tri::Yes, Tri::No, Tri::No)
    );
    // `linear` is the definite property: an unbounded parameter is not definitely
    // linear, only maybe.
    assert_eq!((a.linear, a.view), (Tri::No, Tri::No));
    let b = prop(1);
    assert_eq!((b.copy, b.maybe_linear), (Tri::Yes, Tri::No));
    let c = prop(2);
    assert_eq!((c.copy, c.maybe_linear), (Tri::Yes, Tri::No));
    // `Num` and the other numeric bounds grant `Owned`, `Send` and `Sync` as well
    // (section 8): only scalars satisfy them.
    assert_eq!(
        (c.discard(), c.owned(), c.send, c.sync),
        (Tri::Yes, Tri::Yes, Tri::Yes, Tri::Yes)
    );
    let dd = prop(3);
    assert_eq!(
        (dd.send, dd.sync, dd.owned(), dd.copy),
        (Tri::Yes, Tri::Yes, Tri::Yes, Tri::No)
    );
    let e = prop(4);
    assert_eq!((e.discard(), e.copy), (Tri::Yes, Tri::No));
    // A bound on an interface grants what the interface's own bounds grant.
    let t = d.type_def("app", "T");
    let x = d.decls.generics_of(t)[0].def;
    let xp = d.decls.types.param(x);
    assert_eq!(d.decls.props(xp).copy, Tri::Yes);
}

#[test]
fn recursive_types_reach_a_fixed_point_through_references_and_arguments() {
    let mut d = app("struct Wrapper[T] { w: &T }\n\
         struct Tree { kids: Wrapper[Tree], v: i64 }\n\
         struct Node { parent: &Node, v: i64 }\n\
         linear struct Resource { fd: i32 }\n\
         struct Chain { next: Wrapper[Chain], r: *Resource }\n");
    assert_eq!(d.codes(), vec![]);
    let tree = d.adt("Tree");
    let p = d.decls.props(tree);
    // The greatest fixed point: Tree is Send and Sync because nothing says otherwise,
    // and a view because it holds a borrow.
    assert_eq!(
        (p.send, p.sync, p.view, p.linear, p.copy),
        (Tri::Yes, Tri::Yes, Tri::Yes, Tri::No, Tri::No)
    );
    let node = d.adt("Node");
    let p = d.decls.props(node);
    assert_eq!((p.send, p.sync, p.view), (Tri::Yes, Tri::Yes, Tri::Yes));
    // The raw pointer stops the properties of its pointee but is itself neither
    // `Send` nor `Sync`, and it is not linear whatever it points at.
    let chain = d.adt("Chain");
    let p = d.decls.props(chain);
    assert_eq!((p.send, p.sync, p.linear), (Tri::No, Tri::No, Tri::No));
}

#[test]
fn a_recursive_type_through_a_raw_pointer_is_copy_and_not_view() {
    let mut d = app("copy struct Link { next: *Link, v: i64 }\n\
         struct List { head: *Node, len: i64 }\n\
         struct Node { list: *List, v: i64 }\n");
    assert_eq!(d.codes(), vec![]);
    let link = d.adt("Link");
    let p = d.decls.props(link);
    assert_eq!((p.copy, p.view, p.send), (Tri::Yes, Tri::No, Tri::No));
    let list = d.adt("List");
    let node = d.adt("Node");
    assert_eq!(d.decls.props(list).view, Tri::No);
    assert_eq!(d.decls.props(node).copy, Tri::No);
}

#[test]
fn an_infinite_instance_chain_does_not_loop() {
    let mut d = app("struct Grow[T] { r: ?&Grow[?T], v: i64 }\nstruct Use { g: Grow[i64] }\n");
    // No E0516: the chain is through a borrow. The properties are not computed.
    assert_eq!(d.codes(), vec![]);
    let g = d.field("Use", "g");
    let p = d.decls.props(g);
    assert_eq!(p.copy, Tri::Unknown);
    assert!(d.decls.gave_up);
    assert!(d.has_ni("polymorphic recursion"), "{:?}", d.ni());
}

#[test]
fn the_standard_library_has_the_properties_the_specification_states() {
    let mut d = app_with_std("struct H { x: i64 }\n");
    assert_eq!(d.codes(), vec![]);
    let i64_ty = d.int(IntTy::I64);
    let ptr = {
        let u8_ty = d.int(IntTy::U8);
        d.decls
            .types
            .intern(TyKind::RawPtr(Mutability::Shared, u8_ty))
    };
    // Task[R]: linear, a view of its scope (section 18).
    let task = d.adt_of("std.task", "Task", vec![GenericArg::Ty(i64_ty)]);
    let p = d.decls.props(task);
    assert_eq!((p.linear, p.view, p.copy), (Tri::Yes, Tri::Yes, Tri::No));
    // Atomic[T]: Send and Sync when T: Send (section 20).
    for (t, send, sync) in [(i64_ty, Tri::Yes, Tri::Yes), (ptr, Tri::No, Tri::No)] {
        let atomic = d.adt_of("std.sync", "Atomic", vec![GenericArg::Ty(t)]);
        let p = d.decls.props(atomic);
        assert_eq!((p.send, p.sync), (send, sync));
    }
    // Mutex[T]: both when T: Send (sections 20 and 21), neither otherwise.
    let mutex_i64 = d.adt_of("std.sync", "Mutex", vec![GenericArg::Ty(i64_ty)]);
    let p = d.decls.props(mutex_i64);
    assert_eq!((p.send, p.sync, p.view), (Tri::Yes, Tri::Yes, Tri::No));
    let mutex_ptr = d.adt_of("std.sync", "Mutex", vec![GenericArg::Ty(ptr)]);
    let p = d.decls.props(mutex_ptr);
    assert_eq!((p.send, p.sync), (Tri::No, Tri::No));
    // The allocator and the address-space reservation are copied out of `&Env`.
    let heap = d.adt_of("std.mem", "Heap", vec![]);
    let vm = d.adt_of("std.mem", "Vm", vec![]);
    assert_eq!(d.decls.props(heap).copy, Tri::Yes);
    assert_eq!(d.decls.props(vm).copy, Tri::Yes);
    // A capability behind a borrow can be shared between tasks; `Env` holds borrows.
    let env = d.adt_of("std", "Env", vec![]);
    let p = d.decls.props(env);
    assert_eq!((p.view, p.copy, p.send), (Tri::Yes, Tri::No, Tri::Yes));
}

#[test]
fn the_standard_library_checks_without_a_diagnostic() {
    let d = app_with_std("");
    assert_eq!(d.codes(), vec![]);
    // The only unimplemented parts are the executors' struct-valued constants, and they
    // are not reported for a package that merely uses std.
    assert!(d.ni().is_empty(), "{:?}", d.ni());
}

#[test]
fn a_static_must_be_sync_e0511() {
    let d = app_with_std(
        "import std.sync.Mutex;\n\
         static a: *u8 = 0;\n\
         static b: Mutex[i64] = 0;\n\
         static c: i64 = 4;\n\
         static d: Mutex[*u8] = 0;\n",
    );
    assert_eq!(
        d.codes(),
        vec![("E0511".to_string(), 2), ("E0511".to_string(), 5)]
    );
}

#[test]
fn copy_and_linear_declarations_are_checked_e0501_e0502() {
    let d = app("linear struct Token { id: i32 }\n\
         struct Holder { x: i64 }\n\
         copy struct A { h: Holder }\n\
         copy struct B { r: &Holder, p: *Holder, n: i64 }\n\
         copy enum E1 { a: Holder, b }\n\
         copy enum E2 { a: i64, b }\n\
         struct L1 { t: Token }\n\
         struct L2 { w: [2]Token }\n\
         struct L3 { r: Result[i64, i64] }\n\
         linear struct L4 { t: Token }\n\
         struct L5 { x: i64 }\n\
         enum Free { a: Token, b }\n");
    assert_eq!(
        d.codes(),
        vec![
            ("E0501".to_string(), 3),
            ("E0501".to_string(), 5),
            ("E0502".to_string(), 7),
            ("E0502".to_string(), 8),
            ("E0502".to_string(), 9)
        ]
    );
}

#[test]
fn e0502_inserts_linear_before_struct() {
    let d = app("linear struct Token { id: i32 }\npub struct Holder { t: Token }\n");
    let diag = &d.decls.diagnostics[0];
    assert_eq!(diag.code, "E0502");
    let edit = &diag.fixes[0].edits[0];
    assert_eq!(edit.text, "linear ");
    let text = &d.sources.get(0).text;
    // `pub struct Holder`: the keyword is `struct`, so `pub linear struct`.
    assert_eq!(&text[edit.span.start..edit.span.start + 6], "struct");
    assert!(edit.span.is_empty());
}

#[test]
fn infinite_types_are_found_through_arguments_and_cycles_e0516() {
    let d = app("struct Wrapper[T] { w: T }\n\
         struct A { x: Wrapper[A] }\n\
         struct B { x: Wrapper[*B] }\n\
         struct C { x: ?C }\n\
         struct D1 { e: D2 }\n\
         struct D2 { d: [2]D1 }\n\
         struct F { x: [0]F }\n\
         enum G { nil, cons: (i64, G) }\n\
         struct H { x: [4]*H, y: &H }\n");
    let got = d.codes();
    let lines: Vec<u32> = got
        .iter()
        .filter(|(c, _)| c == "E0516")
        .map(|(_, l)| *l)
        .collect();
    assert_eq!(lines, vec![2, 4, 5, 6, 8], "{got:?}");
    // An infinite type has no layout and no other diagnostic.
    assert_eq!(got.len(), 5, "{got:?}");
}

// ------------------------------------------------------------- layouts

#[test]
fn struct_layout_is_c_layout_in_declaration_order() {
    let mut d = app("struct S { a: u8, b: i64, c: u16 }\n\
         struct T { a: u8, b: u8 }\n\
         struct E {}\n\
         struct P { a: u8, b: S }\n");
    assert_eq!(d.codes(), vec![]);
    let s = d.adt("S");
    let l = d.decls.layout_of(s).expect("a layout");
    assert_eq!((l.size, l.align, l.padding()), (24, 8, 13));
    let offsets: Vec<(&str, u64)> = l
        .fields
        .iter()
        .map(|f| (f.name.as_str(), f.offset))
        .collect();
    assert_eq!(offsets, vec![("a", 0), ("b", 8), ("c", 16)]);
    let t = d.adt("T");
    let l = d.decls.layout_of(t).expect("a layout");
    assert_eq!((l.size, l.align), (2, 1));
    let e = d.adt("E");
    let l = d.decls.layout_of(e).expect("a layout");
    assert_eq!((l.size, l.align), (0, 1));
    let p = d.adt("P");
    let l = d.decls.layout_of(p).expect("a layout");
    assert_eq!((l.size, l.align), (32, 8));
    assert_eq!(l.fields[1].offset, 8);
}

#[test]
fn tuples_arrays_and_pointers_have_the_layouts_of_section_3() {
    let mut d = app(
        "struct H { t: (u8, i32), a: [3]i32, z: [0]i64, s: []u8, d: &i64, f: fn(i64), p: *u8, u: void, n: never }\n",
    );
    assert_eq!(d.codes(), vec![]);
    let mut check = |f: &str, size: u64, align: u64| {
        let ty = d.field("H", f);
        let l = d
            .decls
            .layout_of(ty)
            .unwrap_or_else(|e| panic!("{f}: {e:?}"));
        assert_eq!((l.size, l.align), (size, align), "field {f}");
        l
    };
    let t = check("t", 8, 4);
    assert_eq!(
        t.fields.iter().map(|f| f.offset).collect::<Vec<_>>(),
        vec![0, 4]
    );
    check("a", 12, 4);
    check("z", 0, 8);
    check("s", 16, 8);
    check("d", 8, 8);
    check("f", 8, 8);
    check("p", 8, 8);
    check("u", 0, 1);
    check("n", 0, 1);
}

#[test]
fn enum_layout_has_the_tag_first_then_the_payload() {
    let mut d = app("enum Color: u8 { red = 1, green, blue }\n\
         enum Small { a, b, c }\n\
         enum Wide: u32 { a, b }\n\
         struct Rect { w: f64, h: f64 }\n\
         enum Shape { circle: f64, rect: Rect, empty }\n\
         enum Mixed: u16 { a: u8, b: u32 }\n");
    assert_eq!(d.codes(), vec![]);
    let mut check = |name: &str, size: u64, align: u64| {
        let ty = d.adt(name);
        let l = d
            .decls
            .layout_of(ty)
            .unwrap_or_else(|e| panic!("{name}: {e:?}"));
        assert_eq!((l.size, l.align), (size, align), "{name}");
        l
    };
    check("Color", 1, 1);
    check("Small", 1, 1);
    check("Wide", 4, 4);
    let shape = check("Shape", 24, 8);
    assert_eq!(
        shape.kind,
        LayoutKind::Enum {
            tag_size: 1,
            payload_offset: 8
        }
    );
    // Tag u16 at 0, payload (align 4) at 4, size 8.
    let mixed = check("Mixed", 8, 4);
    assert_eq!(
        mixed.kind,
        LayoutKind::Enum {
            tag_size: 2,
            payload_offset: 4
        }
    );
}

#[test]
fn explicit_tag_values_count_up_and_default_to_the_smallest_tag_type() {
    let d = app("enum Color: u8 { red = 1, green, blue = 10, alpha }\n\
         enum Plain { a, b, c }\n\
         enum Signed: i8 { neg = -2, zero = 0, one }\n");
    assert_eq!(d.codes(), vec![]);
    let tags = |name: &str| -> Vec<i128> {
        let e = d.decls.enum_sig(d.type_def("app", name)).expect("an enum");
        e.variants.iter().map(|v| v.tag).collect()
    };
    assert_eq!(tags("Color"), vec![1, 2, 10, 11]);
    assert_eq!(tags("Plain"), vec![0, 1, 2]);
    assert_eq!(tags("Signed"), vec![-2, 0, 1]);
    let e = d
        .decls
        .enum_sig(d.type_def("app", "Plain"))
        .expect("an enum");
    assert_eq!((e.tag_ty, e.tag_declared), (IntTy::U8, false));
}

#[test]
fn bad_tag_values_are_e0505() {
    let d = app("enum A { x = 1, y }\n\
         enum B: u8 { x = 300 }\n\
         enum C: u8 { x = 255, y }\n\
         enum D: u8 { x = 1, y = 1 }\n\
         enum E: u8 { x = -1 }\n\
         enum F: u8 { x = 200 + 100 }\n\
         enum G: u8 { x = 2, y = 3, z }\n");
    let got = d.codes();
    // A: the explicit value without a tag type. B: does not fit. C: `y` would be 256.
    // D: the same value twice. E: negative for an unsigned tag. F: the expression traps
    // (E0806 at the addition).
    assert_eq!(
        got,
        vec![
            ("E0505".to_string(), 1),
            ("E0505".to_string(), 2),
            ("E0505".to_string(), 3),
            ("E0505".to_string(), 4),
            ("E0505".to_string(), 5),
            ("E0806".to_string(), 6)
        ]
    );
}

#[test]
fn optional_layout_uses_the_null_niche_only_where_section_25_says() {
    let mut d = app(
        "linear struct H { a: ?i64, b: ?u8, c: ?&i64, d: ?&mut i64, e: ?[]u8, f: ?fn(i64), g: ?extern fn(i64), h: ??&i64, i: Result[i64, u8], j: Result[void, u8] }\n",
    );
    assert_eq!(d.codes(), vec![]);
    let mut check = |f: &str, size: u64, align: u64, niche: bool| {
        let ty = d.field("H", f);
        let l = d
            .decls
            .layout_of(ty)
            .unwrap_or_else(|e| panic!("{f}: {e:?}"));
        assert_eq!((l.size, l.align), (size, align), "field {f}");
        assert_eq!(l.kind == LayoutKind::Niche, niche, "field {f}");
    };
    check("a", 16, 8, false);
    check("b", 2, 1, false);
    check("c", 8, 8, true);
    check("d", 8, 8, true);
    check("e", 16, 8, true);
    check("f", 8, 8, true);
    // 04 §25 lists `?extern fn(...)`: the nullable C pointer uses the null niche.
    check("g", 8, 8, true);
    // `??&T`: the outer option has no niche to use.
    check("h", 16, 8, false);
    check("i", 16, 8, false);
    check("j", 2, 1, false);
}

#[test]
fn align_raises_alignment_and_size() {
    let mut d = app("struct Slot align(64) { x: i64 }\n\
         struct Padded { a: u8, hits: i64 align(32), b: u8 }\n\
         struct Cache align(@target.cache_line) { x: i64 }\n");
    assert_eq!(d.codes(), vec![]);
    let slot = d.adt("Slot");
    let l = d.decls.layout_of(slot).expect("a layout");
    assert_eq!((l.size, l.align), (64, 64));
    let padded = d.adt("Padded");
    let l = d.decls.layout_of(padded).expect("a layout");
    assert_eq!(
        l.fields.iter().map(|f| f.offset).collect::<Vec<_>>(),
        vec![0, 32, 40]
    );
    assert_eq!((l.size, l.align), (64, 32));
    // 128 on the default target (the M4).
    let cache = d.adt("Cache");
    let l = d.decls.layout_of(cache).expect("a layout");
    assert_eq!((l.size, l.align), (128, 128));
}

#[test]
fn align_must_be_a_power_of_two_not_below_the_natural_alignment_e0515() {
    let d = app("struct A align(24) { x: i64 }\n\
         struct B align(4) { x: i64 }\n\
         struct C align(8) { x: i64 }\n\
         struct D { x: i64 align(4) }\n\
         struct E { x: u8 align(0) }\n\
         struct F align(1) { x: u8 }\n\
         static g: i64 align(2) = 0;\n\
         struct G[T] align(2) { x: T }\n");
    assert_eq!(
        d.codes(),
        vec![
            ("E0515".to_string(), 1),
            ("E0515".to_string(), 2),
            ("E0515".to_string(), 4),
            ("E0515".to_string(), 5),
            ("E0515".to_string(), 7)
        ]
    );
    // Checked per instance, and recorded as such.
    assert!(d.has_ni("natural alignment"), "{:?}", d.ni());
}

#[test]
fn a_layout_that_depends_on_a_parameter_is_not_invented() {
    let mut d = app("struct Box1[T] { x: T }\n\
         struct Ring[T, const n: i64] { items: [n]T }\n\
         struct Use { a: Box1[u16], b: Ring[u8, 5] }\n");
    assert_eq!(d.codes(), vec![]);
    let b1 = d.decls.generics_of(d.type_def("app", "Box1"))[0].def;
    let p = d.decls.types.param(b1);
    let generic = d.adt_of("app", "Box1", vec![GenericArg::Ty(p)]);
    assert_eq!(
        d.decls.layout_of(generic).unwrap_err(),
        crate::LayoutError::Params
    );
    let a = d.field("Use", "a");
    let l = d.decls.layout_of(a).expect("a layout");
    assert_eq!((l.size, l.align), (2, 2));
    let b = d.field("Use", "b");
    let l = d.decls.layout_of(b).expect("a layout");
    assert_eq!((l.size, l.align), (5, 1));
}

#[test]
fn vectors_and_masks_are_as_large_as_their_width_and_aligned_to_their_size() {
    // Sections 3, 25 and 26: a vector is 16, 32 or 64 bytes and aligned to its size; a
    // mask is one byte per lane, aligned to its size.
    let mut d = app("struct V { a: f32x4, b: u8x32, c: f64x8, d: i64x2 }\n\
         struct M { a: boolx2, b: boolx4, c: boolx16, d: boolx64 }\n\
         struct Mixed { a: u8, v: f32x4, m: boolx8 }\n");
    assert_eq!(d.codes(), vec![]);
    assert!(d.ni().is_empty(), "{:?}", d.ni());
    for (field, size) in [("a", 16), ("b", 32), ("c", 64), ("d", 16)] {
        let t = d.field("V", field);
        let l = d.decls.layout_of(t).expect("a layout");
        assert_eq!((l.size, l.align), (size, size), "{field}");
    }
    for (field, size) in [("a", 2), ("b", 4), ("c", 16), ("d", 64)] {
        let t = d.field("M", field);
        let l = d.decls.layout_of(t).expect("a layout");
        assert_eq!((l.size, l.align), (size, size), "{field}");
    }
    // The vector is aligned to its size inside a struct, which then has that alignment.
    let v = d.adt("V");
    let l = d.decls.layout_of(v).expect("a layout");
    let offsets: Vec<u64> = l.fields.iter().map(|f| f.offset).collect();
    assert_eq!(offsets, vec![0, 32, 64, 128]);
    assert_eq!((l.size, l.align), (192, 64));
    let mixed = d.adt("Mixed");
    let l = d.decls.layout_of(mixed).expect("a layout");
    let offsets: Vec<u64> = l.fields.iter().map(|f| f.offset).collect();
    assert_eq!(offsets, vec![0, 16, 32]);
    assert_eq!((l.size, l.align), (48, 16));
}

#[test]
fn an_enum_with_no_variants_has_size_0_and_alignment_1() {
    let mut d = app("enum Never {}\nstruct S { a: u8, n: [3]Never }\n");
    assert_eq!(d.codes(), vec![]);
    assert!(d.ni().is_empty(), "{:?}", d.ni());
    let never = d.adt("Never");
    let l = d.decls.layout_of(never).expect("a layout");
    assert_eq!((l.size, l.align), (0, 1));
    let s = d.adt("S");
    let l = d.decls.layout_of(s).expect("a layout");
    assert_eq!((l.size, l.align), (1, 1));
}

#[test]
fn vector_widths_are_checked_e1501() {
    let d = app(
        "struct V { a: f32x3, b: u8x16, c: boolx3, d: boolx8, e: i64x8, f: f64x1, g: u16x64 }\n",
    );
    // f32x3 is 96 bits, boolx3 is no mask, f64x1 is 64 bits, u16x64 is 1024 bits.
    assert_eq!(d.codes_only(), vec!["E1501"; 4]);
}

// ------------------------------------------------------------- constants

#[test]
fn integer_operations_follow_section_5_2() {
    let ov = |op: BinOp, ty, a, b| int_binary(op, ty, a, b);
    assert_eq!(ov(BinOp::Add, IntTy::I32, 2147483646, 1), Ok(2147483647));
    assert_eq!(
        ov(BinOp::Add, IntTy::I32, 2147483647, 1),
        Err(Trap::Overflow("+"))
    );
    assert_eq!(ov(BinOp::Sub, IntTy::U8, 0, 1), Err(Trap::Overflow("-")));
    assert_eq!(
        ov(BinOp::Mul, IntTy::U64, u64::MAX as i128, 2),
        Err(Trap::Overflow("*"))
    );
    assert_eq!(
        ov(BinOp::Mul, IntTy::U64, u64::MAX as i128, u64::MAX as i128),
        Err(Trap::Overflow("*"))
    );
    assert_eq!(ov(BinOp::AddWrap, IntTy::U8, 255, 1), Ok(0));
    assert_eq!(ov(BinOp::SubWrap, IntTy::I8, -128, 1), Ok(127));
    assert_eq!(
        ov(
            BinOp::MulWrap,
            IntTy::U64,
            u64::MAX as i128,
            u64::MAX as i128
        ),
        Ok(1)
    );
    assert_eq!(ov(BinOp::AddSat, IntTy::U8, 250, 10), Ok(255));
    assert_eq!(ov(BinOp::SubSat, IntTy::I8, -100, 100), Ok(-128));
    assert_eq!(ov(BinOp::MulSat, IntTy::I8, 100, -2), Ok(-128));
    // `/` truncates toward zero and `%` has the sign of the dividend.
    assert_eq!(ov(BinOp::Div, IntTy::I32, -7, 2), Ok(-3));
    assert_eq!(ov(BinOp::Rem, IntTy::I32, -7, 2), Ok(-1));
    assert_eq!(ov(BinOp::Rem, IntTy::I32, 7, -2), Ok(1));
    assert_eq!(ov(BinOp::Div, IntTy::I32, 1, 0), Err(Trap::DivisionByZero));
    assert_eq!(ov(BinOp::Rem, IntTy::I32, 1, 0), Err(Trap::DivisionByZero));
    assert_eq!(
        ov(BinOp::Div, IntTy::I32, -2147483648, -1),
        Err(Trap::DivisionOverflow)
    );
    assert_eq!(
        ov(BinOp::Rem, IntTy::I8, -128, -1),
        Err(Trap::DivisionOverflow)
    );
    assert_eq!(ov(BinOp::Div, IntTy::U8, 255, 1), Ok(255));
    // Shifts: the count must be below the width; `>>` is arithmetic when signed.
    assert_eq!(ov(BinOp::Shl, IntTy::U8, 1, 7), Ok(128));
    assert_eq!(ov(BinOp::Shl, IntTy::I8, 1, 7), Ok(-128));
    assert_eq!(ov(BinOp::Shl, IntTy::U8, 255, 4), Ok(240));
    assert_eq!(ov(BinOp::Shr, IntTy::I8, -128, 7), Ok(-1));
    assert_eq!(ov(BinOp::Shr, IntTy::U8, 128, 7), Ok(1));
    assert_eq!(
        ov(BinOp::Shl, IntTy::U8, 1, 8),
        Err(Trap::ShiftRange { count: 8, bits: 8 })
    );
    assert_eq!(
        ov(BinOp::Shr, IntTy::I64, 1, -1),
        Err(Trap::ShiftRange {
            count: -1,
            bits: 64
        })
    );
    assert_eq!(ov(BinOp::BitAnd, IntTy::I8, -1, 15), Ok(15));
    assert_eq!(ov(BinOp::BitXor, IntTy::U8, 255, 15), Ok(240));
    // Unary operators.
    assert_eq!(int_unary(UnOp::Neg, IntTy::I8, 5), Ok(-5));
    assert_eq!(
        int_unary(UnOp::Neg, IntTy::I8, -128),
        Err(Trap::Overflow("-"))
    );
    assert_eq!(int_unary(UnOp::NegWrap, IntTy::I8, -128), Ok(-128));
    assert_eq!(int_unary(UnOp::NegWrap, IntTy::U8, 1), Ok(255));
    assert_eq!(int_unary(UnOp::BitNot, IntTy::U8, 15), Ok(240));
    assert_eq!(int_unary(UnOp::BitNot, IntTy::I8, 0), Ok(-1));
    assert_eq!(wrap(IntTy::I8, 200), -56);
    assert_eq!(wrap(IntTy::U8, -1), 255);
    assert!(lossless(IntTy::U8, IntTy::I32));
    assert!(!lossless(IntTy::I8, IntTy::U64));
    assert!(lossless(IntTy::I8, IntTy::I64));
    assert!(!lossless(IntTy::U64, IntTy::I64));
    // Trap codes and texts are those of the catalog.
    assert_eq!(Trap::Overflow("+").code(), "T03");
    assert_eq!(Trap::Overflow("+").text(), "integer overflow in +");
    assert_eq!(Trap::DivisionByZero.code(), "T04");
    assert_eq!(
        Trap::DivisionOverflow.text(),
        "integer overflow in division"
    );
    assert_eq!(
        Trap::ShiftRange { count: 9, bits: 8 }.text(),
        "shift count 9 out of range for 8-bit value"
    );
}

#[test]
fn constants_are_evaluated_with_the_type_their_context_gives_them() {
    let d = app("const a: i64 = 3 * (4 + 5) - 7 / 2;\n\
         const b: u8 = 250 +| 10;\n\
         const c: u8 = 255 +% 1;\n\
         const d: bool = a > 20 && b == 255 || false;\n\
         const e: i32 = 1 << 31 >> 31;\n\
         const f: i64 = a as i64;\n\
         const g: i64 = b as i64 + (true as i64);\n\
         const h: bool = !(a == 24);\n\
         const i: i8 = -128;\n\
         const j: i64 = ~0 & 0xff;\n\
         const k: u64 = 18446744073709551615;\n\
         const l: i64 = -9223372036854775808;\n");
    // Line 5 mixes a shift and another shift: E0204 is the parser's, so this is split
    // below. Here only the values.
    let _ = d.codes();
    assert_eq!(d.const_value("a"), Some(int(IntTy::I64, 24)));
    assert_eq!(d.const_value("b"), Some(int(IntTy::U8, 255)));
    assert_eq!(d.const_value("c"), Some(int(IntTy::U8, 0)));
    assert_eq!(d.const_value("d"), Some(ConstValue::Bool(true)));
    assert_eq!(d.const_value("f"), Some(int(IntTy::I64, 24)));
    assert_eq!(d.const_value("g"), Some(int(IntTy::I64, 256)));
    assert_eq!(d.const_value("h"), Some(ConstValue::Bool(false)));
    assert_eq!(d.const_value("i"), Some(int(IntTy::I8, -128)));
    assert_eq!(d.const_value("j"), Some(int(IntTy::I64, 255)));
    assert_eq!(d.const_value("k"), Some(int(IntTy::U64, u64::MAX as i128)));
    assert_eq!(d.const_value("l"), Some(int(IntTy::I64, i64::MIN as i128)));
}

#[test]
fn a_trap_in_a_constant_is_e0806_with_the_trap_code() {
    let d = app("const zero: i64 = 0;\n\
         const a: i32 = 2147483647 + 1;\n\
         const b: i64 = 1 / zero;\n\
         const c: i32 = -2147483648 / -1;\n\
         const e: i64 = 1 << 64;\n\
         const f: u8 = 0 - 1;\n\
         const g: i8 = -(-128);\n\
         const h: i64 = a + 1;\n");
    assert_eq!(
        d.codes(),
        vec![
            ("E0806".to_string(), 2),
            ("E0806".to_string(), 3),
            ("E0806".to_string(), 4),
            ("E0806".to_string(), 5),
            ("E0806".to_string(), 6),
            ("E0806".to_string(), 7)
        ]
    );
    let text = |code: &str| {
        d.decls
            .diagnostics
            .iter()
            .any(|x| x.code == "E0806" && x.message.contains(code))
    };
    assert!(text("T03: integer overflow in +"));
    assert!(text("T04: division by zero"));
    assert!(text("T05: integer overflow in division"));
    assert!(text("T06: shift count 64 out of range for 64-bit value"));
    // `h` uses the poisoned `a`: nothing more is reported for it.
    assert_eq!(d.const_fail("a"), Some(Fail::Reported));
}

#[test]
fn operands_of_different_types_are_a_type_error_not_a_guess() {
    let d = app("const small: u8 = 1;\n\
         const wide: i64 = 1;\n\
         const a: i64 = wide + small;\n\
         const b: i64 = small + wide;\n\
         const c: bool = small == wide;\n\
         const e: bool = true + 1 == 2;\n\
         const f: i64 = wide + true;\n");
    assert_eq!(d.codes(), vec![]);
    for name in ["a", "b", "c", "e", "f"] {
        assert!(d.const_value(name).is_none(), "{name}");
    }
    assert!(
        d.has_ni("type checking of a constant expression"),
        "{:?}",
        d.ni()
    );
}

#[test]
fn a_short_circuit_does_not_evaluate_the_right_operand() {
    let d = app("const z: i64 = 0;\n\
         const a: bool = false && 1 / z == 1;\n\
         const b: bool = true || 1 / z == 1;\n\
         const c: bool = true && 1 / z == 1;\n");
    assert_eq!(d.const_value("a"), Some(ConstValue::Bool(false)));
    assert_eq!(d.const_value("b"), Some(ConstValue::Bool(true)));
    assert_eq!(d.codes(), vec![("E0806".to_string(), 4)]);
}

#[test]
fn sizeof_alignof_and_target_fields_are_constants() {
    let d = app("struct Row { a: u8, b: i64, c: u16 }\n\
         const s: i64 = @sizeof(Row);\n\
         const a: i64 = @alignof(Row);\n\
         const lines: i64 = @target.cache_line;\n\
         const bits: i64 = @target.simd_bits / 8;\n\
         const mac: bool = @target.os == .macos;\n\
         const lin: bool = @target.os != .macos;\n\
         const arm: bool = @target.arch == .aarch64 && @target.cpu == .m4;\n\
         const neon: bool = @target.has(.neon);\n\
         const avx: bool = @target.has(.avx2);\n\
         const unified: bool = @target.gpu_unified;\n\
         const arr: i64 = @sizeof([3]Row);\n\
         const sum: i64 = @sizeof(Row) + @sizeof(u8);\n");
    assert_eq!(d.codes(), vec![]);
    assert_eq!(d.const_value("s"), Some(int(IntTy::I64, 24)));
    assert_eq!(d.const_value("a"), Some(int(IntTy::I64, 8)));
    assert_eq!(d.const_value("lines"), Some(int(IntTy::I64, 128)));
    assert_eq!(d.const_value("bits"), Some(int(IntTy::I64, 16)));
    assert_eq!(d.const_value("mac"), Some(ConstValue::Bool(true)));
    assert_eq!(d.const_value("lin"), Some(ConstValue::Bool(false)));
    assert_eq!(d.const_value("arm"), Some(ConstValue::Bool(true)));
    assert_eq!(d.const_value("neon"), Some(ConstValue::Bool(true)));
    assert_eq!(d.const_value("avx"), Some(ConstValue::Bool(false)));
    assert_eq!(d.const_value("unified"), Some(ConstValue::Bool(true)));
    assert_eq!(d.const_value("arr"), Some(int(IntTy::I64, 72)));
    assert_eq!(d.const_value("sum"), Some(int(IntTy::I64, 25)));
}

#[test]
fn constants_feed_array_lengths_and_generic_arguments() {
    let mut d = app("const width: i64 = 2 + 2;\n\
         const area: i64 = width * width;\n\
         struct Ring[T, const n: i64] { items: [n]T }\n\
         struct Grid { cells: [area]u8, r: Ring[u8, { width }], q: Ring[u8, 4], s: [@sizeof(Ring[u8, 4])]u8 }\n");
    assert_eq!(d.codes(), vec![]);
    let cells = d.field("Grid", "cells");
    assert_eq!(d.display(cells), "[16]u8");
    // `Ring[u8, width]` and `Ring[u8, 4]` are the same type.
    assert_eq!(d.field("Grid", "r"), d.field("Grid", "q"));
    let s = d.field("Grid", "s");
    assert_eq!(d.display(s), "[4]u8");
    let grid = d.adt("Grid");
    let l = d.decls.layout_of(grid).expect("a layout");
    assert_eq!(l.size, 16 + 4 + 4 + 4);
}

#[test]
fn what_the_evaluator_does_not_cover_is_not_implemented_never_guessed() {
    let d = app("struct P { x: i64 }\n\
         const a: P = P.{ x: 1 };\n\
         const b: f64 = 1.5;\n\
         const c: i64 = zero();\n\
         const e: []u8 = \"hi\";\n\
         static s: i64 = 1;\n\
         const f: i64 = s;\n\
         const g: i64 = 300 as u8 as i64;\n");
    let _ = d.codes();
    for part in [
        "a value of type `P`",
        "a value of type `f64`",
        "a function call",
        "a value of type `[]u8`",
        "a reference to a static",
    ] {
        assert!(d.has_ni(part), "{part}: {:?}", d.ni());
    }
    for name in ["a", "b", "c", "e", "f"] {
        assert!(d.const_value(name).is_none(), "{name}");
    }
}

#[test]
fn a_static_with_an_integer_initializer_is_evaluated() {
    let d = app(
        "static n: i64 = 2 * 8;\nstatic bad: u8 = 255 + 1;\nstatic m: i64 = zero();\nfn zero() -> i64 { return 0; }\n",
    );
    assert_eq!(d.codes(), vec![("E0806".to_string(), 2)]);
    assert!(d.has_ni("a function call"), "{:?}", d.ni());
}

#[test]
fn a_cycle_between_constants_is_reported_not_followed() {
    let d = app("const a: i64 = b + 1;\n\
         const b: i64 = c + 1;\n\
         const c: i64 = a + 1;\n\
         const self_ref: i64 = self_ref;\n\
         const fine: i64 = 4;\n\
         struct S { x: [a]u8, y: [fine]u8 }\n");
    // E0812 at the constant the evaluation came back to: `a` for the cycle, once, and
    // `self_ref` for the constant that names itself.
    assert_eq!(
        d.codes(),
        vec![("E0812".to_string(), 1), ("E0812".to_string(), 4)]
    );
    assert!(d.ni().is_empty(), "{:?}", d.ni());
    assert!(d.const_value("fine").is_some());
    assert!(d.const_value("a").is_none());
    assert!(d.const_value("self_ref").is_none());
}

#[test]
fn a_cycle_between_aliases_is_reported_not_followed() {
    let d = app("type A = B;\n\
         type B = ?A;\n\
         type C = C;\n\
         type Ok1 = []u8;\n\
         struct S { a: A, b: C, c: Ok1 }\n");
    // E0520 at the alias the expansion came back to: `A` for the pair, `C` for the
    // alias that names itself.
    assert_eq!(
        d.codes(),
        vec![("E0520".to_string(), 1), ("E0520".to_string(), 3)]
    );
    assert!(d.ni().is_empty(), "{:?}", d.ni());
}

#[test]
fn a_type_that_depends_on_its_own_layout_is_a_cycle_not_a_loop() {
    let d = app("struct S { a: [@sizeof(S)]u8 }\n");
    assert_eq!(d.codes(), vec![]);
    assert!(d.has_ni("layout depends on itself"), "{:?}", d.ni());
}

#[test]
fn unimplemented_constants_of_std_do_not_make_a_user_package_unchecked() {
    let d = app_with_std("import std.exec;\nstruct H { x: i64 }\n");
    assert!(d.ni().is_empty(), "{:?}", d.ni());
    // The evaluation is asked for only when the program needs the value.
    let d = app_with_std("import std.exec;\nconst x: i64 = exec.seq;\n");
    assert!(d.has_ni("Seq"), "{:?}", d.ni());
}

// ------------------------------------------------------------- impls

#[test]
fn method_tables_find_inherent_and_interface_impls_by_type() {
    let mut d = app("interface Tag { fn tag(self: &Self) -> u32; }\n\
         struct Pair[A, B] { a: A, b: B }\n\
         copy struct P { x: i64 }\n\
         impl Tag for P { fn tag(self: &P) -> u32 { return 1; } }\n\
         impl Pair[i64, u8] { fn first(self: &Pair[i64, u8]) -> i64 { return 0; } }\n\
         impl[K: Tag] Tag for Pair[K, K] { fn tag(self: &Pair[K, K]) -> u32 { return 0; } }\n\
         struct Use { a: Pair[i64, u8], b: Pair[u8, u8], c: Pair[P, P], d: Pair[i64, i64] }\n");
    assert_eq!(d.codes(), vec![]);
    let a = d.field("Use", "a");
    let b = d.field("Use", "b");
    let c = d.field("Use", "c");
    let dd = d.field("Use", "d");
    assert_eq!(d.decls.inherent_impls(a).len(), 1);
    assert_eq!(d.decls.inherent_impls(b).len(), 0);
    let tag = d.type_def("app", "Tag");
    // `Pair[P, P]` implements Tag through the generic impl, with K = P.
    let found = d.decls.interface_impls(c);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].0, tag);
    let m = found[0].2.clone();
    let k = d.decls.generics_of(m.impl_def)[0].def;
    let p = d.adt("P");
    assert_eq!(m.subst.get(k), Some(&GenericArg::Ty(p)));
    assert_eq!(d.decls.impl_bounds_hold(&m), Tri::Yes);
    // `Pair[i64, i64]` matches the impl's target, but K = i64 is not Tag.
    let found = d.decls.interface_impls(dd);
    assert_eq!(found.len(), 1);
    let m = found[0].2.clone();
    assert_eq!(d.decls.impl_bounds_hold(&m), Tri::No);
    // `Pair[i64, u8]` has no Tag impl, `P` has one.
    assert!(d.decls.interface_impls(a).is_empty());
    assert_eq!(d.decls.interface_impls(p).len(), 1);
    // The impl's methods are in its table.
    let imp = d.decls.impl_sig(m.impl_def).expect("an impl");
    assert!(imp.methods.contains_key("tag"));
    assert_eq!(imp.of, ImplOf::Interface(tag, vec![]));
}

#[test]
fn impl_checks_e0506_e0507_e0508() {
    let d = app(
        "interface Tag { fn tag(self: &Self) -> u32 { return 0; } }\n\
         struct W[T] { x: T }\n\
         impl[T] Tag for T {}\n\
         impl[T] Tag for W[T] {}\n\
         impl Tag for W[i64] {}\n\
         struct Q { x: i64 }\n\
         impl Tag for Q {}\n\
         impl Tag for Q {}\n\
         impl Tag for [4]u8 {}\n\
         impl Tag for W[u8] {}\n",
    );
    assert_eq!(
        d.codes(),
        vec![
            ("E0507".to_string(), 5),
            ("E0507".to_string(), 8),
            ("E0507".to_string(), 10),
            ("E0508".to_string(), 3)
        ]
    );
}

#[test]
fn e0509_e0519_report_what_the_impl_gets_wrong() {
    let d = app("interface Source {\n\
             type Item;\n\
             fn next(self: &mut Self) -> ?Self.Item;\n\
             fn peek(self: &Self) -> ?Self.Item { return .none; }\n\
         }\n\
         struct A { n: i64 }\n\
         impl Source for A {}\n\
         struct B { n: i64 }\n\
         impl Source for B {\n\
             type Item = i64;\n\
             type Extra = i64;\n\
             fn next(self: &mut B) -> ?i32 { return .none; }\n\
             fn other(self: &B) -> i64 { return 1; }\n\
         }\n\
         struct C { n: i64 }\n\
         impl Source for C {\n\
             type Item = i64;\n\
             fn next(self: &mut C) -> ?i64 { return .none; }\n\
         }\n");
    let got = d.codes();
    // A: lacks `Item` (E0519) and `next` (E0509), both on the impl. B: an associated
    // type the interface lacks (E0519), `next` returns `?i32`, `other` is extra. C is
    // correct: `peek` has a default.
    assert_eq!(
        got,
        vec![
            ("E0509".to_string(), 7),
            ("E0509".to_string(), 12),
            ("E0509".to_string(), 13),
            ("E0519".to_string(), 7),
            ("E0519".to_string(), 11)
        ]
    );
}

#[test]
fn a_missing_function_is_inserted_where_section_1_says() {
    // Empty braces are opened; a second function goes after the last member.
    let d = app("interface Two {\n\
             fn a(self: &Self) -> i64;\n\
             fn b(self: &Self, n: i64) -> Self.Out;\n\
             type Out;\n\
         }\n\
         struct S { n: i64 }\n\
         impl Two for S {}\n\
         struct T { n: i64 }\n\
         impl Two for T {\n\
             type Out = i64;\n\
             fn a(self: &T) -> i64 { return 0; }\n\
         }\n");
    let fixes: Vec<(&str, &str)> = d
        .decls
        .diagnostics
        .iter()
        .filter(|x| x.code == "E0509")
        .flat_map(|x| {
            x.fixes
                .iter()
                .map(|f| (f.message.as_str(), f.edits[0].text.as_str()))
        })
        .collect();
    assert_eq!(fixes.len(), 3, "{fixes:?}");
    let texts: Vec<&str> = fixes.iter().map(|(_, t)| *t).collect();
    assert!(texts.contains(&"{\n    fn a(self: &S) -> i64 {\n        @panic(\"todo\");\n    }\n}"));
    assert!(
        texts.contains(
            &"\n    fn b(self: &T, n: i64) -> Self.Out {\n        @panic(\"todo\");\n    }"
        )
    );
}

#[test]
fn an_impl_may_declare_fewer_origins_never_more() {
    let d = app("interface Lend {\n\
             fn get(self: &Self, other: []u8) -> []u8 from self, other;\n\
             fn put(self: &mut Self, x: []u8 into self);\n\
         }\n\
         struct A { n: i64 }\n\
         impl Lend for A {\n\
             fn get(self: &A, other: []u8) -> []u8 from self { return other; }\n\
             fn put(self: &mut A, x: []u8 into self) { }\n\
         }\n\
         struct B { n: i64 }\n\
         impl Lend for B {\n\
             fn get(self: &B, other: []u8) -> []u8 from self.* { return other; }\n\
             fn put(self: &mut B, x: []u8) { }\n\
         }\n\
         interface Plain { fn get(self: &Self, other: []u8) -> []u8 from other; fn put(self: &mut Self, x: []u8); }\n\
         struct C { n: i64 }\n\
         impl Plain for C {\n\
             fn get(self: &C, other: []u8) -> []u8 from self, other { return other; }\n\
             fn put(self: &mut C, x: []u8 into self) { }\n\
         }\n");
    assert_eq!(
        d.codes(),
        vec![
            ("E0509".to_string(), 12),
            ("E0509".to_string(), 18),
            ("E0509".to_string(), 19)
        ]
    );
}

#[test]
fn self_has_the_implementing_type_e0517() {
    let d = app("struct S { n: i64 }\n\
         impl S {\n\
             fn a(self: S) -> i64 { return 0; }\n\
             fn b(self: &S) -> i64 { return 0; }\n\
             fn c(self: &mut S) -> i64 { return 0; }\n\
             fn d(self: &Self) -> i64 { return 0; }\n\
             fn e(self: i64) -> i64 { return 0; }\n\
             fn f(self: &i64) -> i64 { return 0; }\n\
             fn g(self: []S) -> i64 { return 0; }\n\
         }\n\
         interface I { fn a(self: &Self); fn b(self: Self); fn c(self: i64); }\n\
         fn free(self: i64) { }\n");
    assert_eq!(
        d.codes(),
        vec![
            ("E0517".to_string(), 7),
            ("E0517".to_string(), 8),
            ("E0517".to_string(), 9),
            ("E0517".to_string(), 11),
            ("E0517".to_string(), 12)
        ]
    );
}

#[test]
fn a_dyn_interface_may_not_take_or_return_self_by_value_or_be_generic_e0510() {
    let d = app(
        "interface Ok1 { fn a(self: &Self, other: &Self) -> i64; fn b(self: &mut Self) -> []u8 from self; }\n\
         interface ByValue { fn a(self: Self) -> i64; }\n\
         interface Returns { fn dup(self: &Self) -> ?Self; }\n\
         interface Generic { fn map[T](self: &Self, x: T); }\n\
         interface Ptr { fn p(self: &Self) -> *Self; }\n\
         struct H { a: &dyn Ok1, b: &dyn ByValue, c: &mut dyn Returns, d: &dyn Generic, e: &dyn Ptr }\n",
    );
    let got = d.codes();
    assert_eq!(
        got,
        vec![
            ("E0510".to_string(), 6),
            ("E0510".to_string(), 6),
            ("E0510".to_string(), 6),
            ("E0510".to_string(), 6),
            ("E0510".to_string(), 6)
        ]
    );
}

#[test]
fn bad_main_e0514() {
    let d = app_with_std(
        "import std.Env;\n\
         fn main(env: &Env) -> u8 { return 0; }\n",
    );
    assert_eq!(d.codes(), vec![("E0514".to_string(), 2)]);
    for ok in [
        "pub fn main(env: &Env) { }",
        "pub fn main(env: &Env) -> u8 { return 0; }",
        "pub fn main(env: &Env) -> void ! i64 { }",
        "pub fn main(env: &Env) -> u8 ! i64 { return 0; }",
    ] {
        let d = app_with_std(&format!("import std.Env;\n{ok}\n"));
        assert_eq!(d.codes(), vec![], "{ok}");
    }
    for bad in [
        "pub fn main() { }",
        "pub fn main(env: &mut Env) { }",
        "pub fn main(env: &Env, n: i64) { }",
        "pub fn main(env: &Env) -> i64 { return 0; }",
        "pub fn main[T](env: &Env) { }",
        "pub fn main(env: Env) { }",
    ] {
        let d = app_with_std(&format!("import std.Env;\n{bad}\n"));
        assert_eq!(d.codes(), vec![("E0514".to_string(), 2)], "{bad}");
    }
}

#[test]
fn missing_and_unwanted_bodies_e0518() {
    let d = app("fn a(x: i64) -> i64;\n\
         interface I { fn m(self: &Self) -> i64; fn n(self: &Self) -> i64 { return 0; } }\n\
         struct S { x: i64 }\n\
         impl S { fn k(self: &S) -> i64; }\n\
         extern \"c\" { fn ok1(x: i64) -> i64; fn bad(x: i64) -> i64 { return x; } }\n");
    assert_eq!(
        d.codes(),
        vec![
            ("E0518".to_string(), 1),
            ("E0518".to_string(), 4),
            ("E0518".to_string(), 5)
        ]
    );
}

#[test]
fn foreign_functions_e1601_e1603_e1604() {
    let d = app("copy struct Pt { x: i32, y: i32 }\n\
         copy struct Gen[T: Copy] { x: T }\n\
         struct Plain { x: i32 }\n\
         extern \"c\" {\n\
             fn ok1(a: i32, b: u8, c: f64, d: bool, p: *mut Plain, f: extern fn(*u8) -> i32, pt: Pt) -> Pt;\n\
             fn ok2(a: Gen[i32]);\n\
             fn bad1(a: Plain);\n\
             fn bad2(a: &i32);\n\
             fn bad3(a: []u8) -> ?i32;\n\
             fn bad4(f: fn(i64) -> i64);\n\
             fn bad5(a: Gen[&i32]);\n\
             fn bad6(a: void);\n\
         }\n\
         extern \"cdecl\" { fn weird(a: i32); }\n\
         extern \"c-blocking\" { fn slow(a: i32); }\n\
         extern \"c-bigstack\" { fn deep(a: i32); }\n\
         export fn e1(a: i32) -> i32 { return a; }\n\
         export fn e1(a: i32) -> i32 { return a; }\n\
         export fn e2(a: Plain) { }\n\
         export fn e3[T](a: i32) { }\n\
         export fn e4(a: i32) -> i32 ! i32 { return a; }\n");
    let got = d.codes();
    let lines_of = |code: &str| -> Vec<u32> {
        got.iter()
            .filter(|(c, _)| c == code)
            .map(|(_, l)| *l)
            .collect()
    };
    assert_eq!(lines_of("E1601"), vec![7, 8, 9, 9, 10, 11, 12, 19]);
    assert_eq!(lines_of("E1603"), vec![14]);
    assert_eq!(lines_of("E1604"), vec![18, 20, 21]);
    assert!(
        got.iter()
            .all(|(c, _)| ["E1601", "E1603", "E1604", "E0308"].contains(&c.as_str())),
        "{got:?}"
    );
}

#[test]
fn origin_clauses_name_parameters_e1206() {
    let d = app("fn a(x: []u8) -> []u8 from x { return x; }\n\
         fn b(x: []u8) -> []u8 from y { return x; }\n\
         fn c(x: []u8) -> []u8 from static { return x; }\n\
         struct S { n: []u8 }\n\
         fn d(s: &mut S, x: []u8 into t) { }\n\
         fn e(s: &mut S, x: []u8 into s) { }\n\
         impl S { fn f(self: &S) -> []u8 from self.* { return self.n; } fn g(self: &S) -> []u8 from selfish { return self.n; } }\n");
    assert_eq!(
        d.codes(),
        vec![
            ("E1206".to_string(), 2),
            ("E1206".to_string(), 5),
            ("E1206".to_string(), 7)
        ]
    );
}

#[test]
fn generic_interfaces_and_their_impls_are_matched_with_their_arguments() {
    let d = app("interface Conv[T] { fn conv(self: &Self) -> T; }\n\
         struct S { n: i64 }\n\
         impl Conv[i64] for S { fn conv(self: &S) -> i64 { return 0; } }\n\
         impl Conv[u8] for S { fn conv(self: &S) -> u8 { return 0; } }\n\
         impl Conv[u8] for S { fn conv(self: &S) -> u8 { return 1; } }\n\
         struct T { n: i64 }\n\
         impl Conv[i64] for T { fn conv(self: &T) -> u8 { return 0; } }\n\
         struct H { a: Conv }\n");
    let got = d.codes();
    assert!(got.contains(&("E0507".to_string(), 5)), "{got:?}");
    assert!(got.contains(&("E0509".to_string(), 7)), "{got:?}");
    assert!(!got.iter().any(|(c, l)| c == "E0507" && *l == 4), "{got:?}");
}

#[test]
fn associated_types_are_projected_and_normalized() {
    let mut d = app(
        "interface Source { type Item; fn next(self: &mut Self) -> ?Self.Item; }\n\
         struct Counter { n: i64 }\n\
         impl Source for Counter {\n\
             type Item = u8;\n\
             fn next(self: &mut Counter) -> ?Self.Item { return .none; }\n\
         }\n\
         fn first[S: Source](s: &mut S) -> ?S.Item { return .none; }\n",
    );
    assert_eq!(d.codes(), vec![]);
    let first = d.value_def("first");
    let sig = d.decls.fn_sig(first).expect("a fn").clone();
    assert_eq!(d.display(sig.ret), "?S.Item");
    // Instantiating S = Counter normalizes the projection through the impl.
    let s_param = d.decls.generics_of(first)[0].def;
    let counter = d.adt("Counter");
    let mut s = Subst::new();
    s.insert(s_param, GenericArg::Ty(counter));
    let ret = d.decls.instantiate(sig.ret, &s);
    assert_eq!(d.display(ret), "?u8");
    // The impl's own `next` already says `?u8` for `?Self.Item`.
    let counter_impl = d
        .decls
        .items
        .values()
        .find_map(|i| match i {
            ItemSig::Impl(i) => Some(i.clone()),
            _ => None,
        })
        .expect("an impl");
    let next = d
        .decls
        .fn_sig(counter_impl.methods["next"])
        .expect("fn")
        .clone();
    assert_eq!(d.display(next.ret), "?u8");
}

#[test]
fn builtin_interface_bounds_follow_section_4_4() {
    // Integers hash, floats do not; a user type needs an impl; nothing is unknown.
    let d = app("struct Keyed[K: Hash] { k: K }\n\
         struct Plain { x: i64 }\n\
         struct A { a: Keyed[i64] }\n\
         struct B { b: Keyed[Plain] }\n\
         struct C { c: Keyed[f64] }\n");
    assert_eq!(
        d.codes(),
        vec![("E0801".to_string(), 4), ("E0801".to_string(), 5)]
    );
    assert!(d.ni().is_empty(), "{:?}", d.ni());
}

#[test]
fn packages_with_bodies_are_not_accepted_and_others_are() {
    let d = app("struct S { x: i64 }\nconst c: i64 = 3;\ntype T = S;\n");
    assert_eq!(d.codes(), vec![]);
    assert!(d.ni().is_empty(), "{:?}", d.ni());
    let d = app("fn f() -> i64 { return 1; }\n");
    assert_eq!(d.ni(), [crate::UNCHECKED_BODIES.to_string()]);
    let d = app("test \"t\" { }\n");
    assert_eq!(d.ni(), [crate::UNCHECKED_BODIES.to_string()]);
    let d = app("interface I { fn f(self: &Self) -> i64 { return 0; } }\n");
    assert_eq!(d.ni(), [crate::UNCHECKED_BODIES.to_string()]);
    let d = app("extern \"c\" { fn f(a: i32) -> i32; }\n");
    assert!(d.ni().is_empty(), "{:?}", d.ni());
}

#[test]
fn the_order_of_everything_is_deterministic() {
    let text = "struct A { b: B, c: Missing }\nstruct B { x: i64 }\nconst k: i64 = 1 / 0;\nimpl Z for A {}\n";
    let one = app(text);
    let two = app(text);
    let render = |d: &D| -> Vec<String> {
        d.decls
            .diagnostics
            .iter()
            .map(|x| x.to_json(&d.sources))
            .collect()
    };
    assert_eq!(render(&one), render(&two));
}

// ------------------------------------------------ the built-in interfaces (4.4)

impl D {
    /// The definition behind a builtin name such as `Eq`.
    fn builtin(&self, name: &str) -> DefId {
        self.out
            .sema
            .builtin_def(name)
            .unwrap_or_else(|| panic!("builtin {name}"))
    }

    fn satisfies(&mut self, ty: TypeId, interface: &str) -> Tri {
        let i = self.builtin(interface);
        self.decls
            .satisfies(ty, &crate::sigs::Bound::Interface(i, vec![]))
    }

    fn kind(&mut self, kind: TyKind) -> TypeId {
        self.decls.types.intern(kind)
    }

    fn method_sources(&mut self, ty: TypeId, name: &str) -> Vec<(String, DefId)> {
        use crate::MethodSource as S;
        self.decls
            .method_candidates(ty, name)
            .into_iter()
            .map(|c| {
                let how = match &c.source {
                    S::Inherent(_) => "inherent".to_string(),
                    S::Impl { interface, .. } => {
                        format!("impl {}", self.out.sema.def(*interface).name)
                    }
                    S::Builtin { interface } => {
                        format!("builtin {}", self.out.sema.def(*interface).name)
                    }
                    S::Bound { interface, .. } => {
                        format!("bound {}", self.out.sema.def(*interface).name)
                    }
                };
                (how, c.method)
            })
            .collect()
    }
}

#[test]
fn the_builtin_interfaces_are_declarations_with_the_members_of_section_4_4() {
    let mut d = app_with_std("");
    assert_eq!(d.codes(), vec![]);
    assert!(d.out.diagnostics.is_empty());
    assert!(
        d.out.not_implemented.is_empty(),
        "{:?}",
        d.out.not_implemented
    );
    assert!(d.gaps().is_empty(), "{:?}", d.gaps());
    // Each is a declaration of the hidden module, found through the builtin scope.
    for (name, methods, assoc) in [
        ("Eq", vec!["eq"], vec![]),
        ("Ord", vec!["cmp"], vec![]),
        ("Hash", vec!["hash"], vec![]),
        ("Iter", vec!["next"], vec!["Item"]),
        ("Format", vec!["format"], vec![]),
    ] {
        let i = d.builtin(name);
        assert_eq!(d.out.sema.def(i).kind, crate::DefKind::Interface, "{name}");
        let sig = d.decls.interface_sig(i).expect("a signature");
        let got: Vec<String> = sig
            .methods
            .iter()
            .map(|m| d.out.sema.def(*m).name.clone())
            .collect();
        assert_eq!(got, methods, "{name}");
        let got: Vec<String> = sig
            .assoc
            .iter()
            .map(|m| d.out.sema.def(*m).name.clone())
            .collect();
        assert_eq!(got, assoc, "{name}");
    }
    // `Ord: Eq`.
    let (eq, ord) = (d.builtin("Eq"), d.builtin("Ord"));
    assert_eq!(
        d.decls.interface_sig(ord).expect("Ord").supers,
        vec![crate::sigs::Bound::Interface(eq, vec![])]
    );
    // The signatures.
    let sig_of = |d: &D, iface: &str| {
        let i = d.builtin(iface);
        let m = d.decls.interface_sig(i).expect("an interface").methods[0];
        d.decls.fn_sig(m).expect("a function").clone()
    };
    let shape = |d: &D, f: &crate::FnSig| -> (Vec<String>, String) {
        (
            f.params.iter().map(|p| d.display(p.ty)).collect(),
            d.display(f.ret),
        )
    };
    let f = sig_of(&d, "Eq");
    assert_eq!(
        shape(&d, &f),
        (
            vec!["&Self".to_string(), "&Self".to_string()],
            "bool".to_string()
        )
    );
    let f = sig_of(&d, "Ord");
    assert_eq!(
        shape(&d, &f),
        (
            vec!["&Self".to_string(), "&Self".to_string()],
            "Ordering".to_string()
        )
    );
    let f = sig_of(&d, "Hash");
    let (params, ret) = shape(&d, &f);
    assert_eq!(
        (
            params[0].as_str(),
            params[1].ends_with("Hasher"),
            ret.as_str()
        ),
        ("&Self", true, "void")
    );
    assert!(params[1].starts_with("&mut "), "{params:?}");
    let f = sig_of(&d, "Format");
    let (params, ret) = shape(&d, &f);
    assert_eq!(
        (
            params[0].as_str(),
            params[1].ends_with("Formatter"),
            ret.as_str()
        ),
        ("&Self", true, "void")
    );
    let f = sig_of(&d, "Iter");
    assert_eq!(
        shape(&d, &f),
        (vec!["&mut Self".to_string()], "?Self.Item".to_string())
    );
    // `from self.*`: the result borrows from the interior of the receiver.
    assert_eq!(f.from.len(), 1);
    assert_eq!(
        (f.from[0].name.as_str(), f.from[0].param, f.from[0].inner),
        ("self", Some(0), true)
    );
    // `Ordering` is a `copy enum` with the variants of the specification, in order.
    let ordering = d.builtin("Ordering");
    let e = d.decls.enum_sig(ordering).expect("an enum");
    let names: Vec<(&str, i128)> = e
        .variants
        .iter()
        .map(|v| (d.out.sema.def(v.def).name.as_str(), v.tag))
        .collect();
    assert_eq!(names, vec![("less", 0), ("equal", 1), ("greater", 2)]);
    assert!(e.copy && e.variants.iter().all(|v| v.payload.is_none()));
    let t = d.kind(TyKind::Adt(ordering, vec![]));
    let l = d.decls.layout_of(t).expect("a layout");
    assert_eq!((l.size, l.align), (1, 1));
    assert_eq!(d.decls.props(t).copy, Tri::Yes);
}

#[test]
fn hasher_and_formatter_are_capability_types_with_no_public_field() {
    let d = app_with_std("");
    for (module, name) in [("std.hash", "Hasher"), ("std.fmt", "Formatter")] {
        let def = d.type_def(module, name);
        assert_eq!(d.out.sema.def(def).vis, crate::Vis::Pub);
        let s = d.decls.struct_sig(def).expect("a struct");
        assert!(s.fields.iter().all(|f| !f.public), "{name}");
    }
}

#[test]
fn a_user_declaration_hides_a_builtin_interface_without_changing_the_builtin() {
    let mut d = app("interface Eq { fn same(self: &Self) -> bool; }\n\
         struct S[T: Eq] { t: T }\n\
         struct U { s: S[i64] }\n");
    // The program's `Eq` is the one in scope, and `i64` does not implement it.
    assert_eq!(d.codes(), vec![("E0801".to_string(), 3)]);
    let mine = d.type_def("app", "Eq");
    let builtin = d.builtin("Eq");
    assert_ne!(mine, builtin);
    let i64_ty = d.int(IntTy::I64);
    assert_eq!(d.satisfies(i64_ty, "Eq"), Tri::Yes);
}

#[test]
fn impls_of_the_builtin_interfaces_are_checked_against_their_declarations() {
    let d = app_with_std(
        "import std.fmt.Formatter;\n\
         import std.hash.Hasher;\n\
         struct P { x: i64 }\n\
         impl Eq for P { fn eq(self: &P, other: &P) -> bool { return true; } }\n\
         impl Ord for P { fn cmp(self: &P, other: &P) -> Ordering { return .equal; } }\n\
         impl Hash for P { fn hash(self: &P, state: &mut Hasher) { } }\n\
         impl Format for P { fn format(self: &P, out: &mut Formatter) { } }\n\
         struct It { n: i64 }\n\
         impl Iter for It {\n\
             type Item = i64;\n\
             fn next(self: &mut It) -> ?i64 { return .none; }\n\
         }\n",
    );
    assert_eq!(d.codes(), vec![]);
    assert!(d.gaps().is_empty(), "{:?}", d.gaps());
    let d = app_with_std(
        "import std.fmt.Formatter;\n\
         import std.hash.Hasher;\n\
         struct Q { x: i64 }\n\
         impl Ord for Q {}\n\
         impl Eq for Q { fn eq(self: &Q, other: &Q) -> i64 { return 0; } }\n\
         struct R { x: i64 }\n\
         impl Hash for R { fn hash(self: &R, state: &i64) { } }\n\
         impl Format for R { fn format(self: &R, out: &mut Formatter) { } fn extra(self: &R) { } }\n\
         struct It { n: i64 }\n\
         impl Iter for It {\n\
             fn next(self: &mut It) -> ?i64 { return .none; }\n\
         }\n\
         struct Jt { n: i64 }\n\
         impl Iter for Jt {\n\
             type Item = i64;\n\
             type Other = i64;\n\
             const k: i64 = 1;\n\
             fn next(self: &mut Jt) -> ?i64 { return .none; }\n\
         }\n",
    );
    // Ord lacks `cmp`, Eq returns `i64`, Hash takes `&i64`, Format has a function
    // the interface lacks, Iter lacks `Item` and its `next` returns `?i64` (the
    // projection cannot be decided without `Item`), Jt: an unknown associated type, a
    // constant, and no `from self.*` (which an impl may leave out: fewer clauses).
    assert_eq!(
        d.codes(),
        vec![
            ("E0509".to_string(), 4),
            ("E0509".to_string(), 5),
            ("E0509".to_string(), 7),
            ("E0509".to_string(), 8),
            ("E0509".to_string(), 11),
            ("E0509".to_string(), 17),
            ("E0519".to_string(), 10),
            ("E0519".to_string(), 16),
        ]
    );
    // Nothing points into the declaration of a built-in interface.
    for diag in d.decls.diagnostics.iter() {
        assert!(diag.related.iter().all(|r| r.file != crate::BUILTIN_PATH));
        assert_ne!(diag.file, crate::BUILTIN_PATH);
    }
}

#[test]
fn no_diagnostic_points_into_the_builtin_declarations() {
    let d = app_with_std(
        "struct Q { x: i64 }\n\
         impl Ord for Q {}\n\
         impl Eq for Q { fn eq(self: &Q, other: &Q) -> bool { return true; } fn x(self: &Q) { } }\n\
         impl Iter for Q {}\n\
         fn f(a: &dyn Hash, b: &dyn Format, c: &dyn Iter, d: &dyn Ord) { }\n",
    );
    for diag in d.decls.diagnostics.iter().chain(d.out.diagnostics.iter()) {
        assert_ne!(diag.file, crate::BUILTIN_PATH, "{diag:?}");
        assert!(diag.related.iter().all(|r| r.file != crate::BUILTIN_PATH));
        assert!(
            diag.fixes
                .iter()
                .all(|f| f.edits.iter().all(|e| e.file != crate::BUILTIN_PATH))
        );
    }
    // The fix of E0509 would write the signature of a function declared in another
    // file, whose names may not mean the same here: there is none.
    let missing: Vec<_> = d
        .decls
        .diagnostics
        .iter()
        .filter(|x| x.code == "E0509" && x.message.contains("lacks"))
        .collect();
    assert_eq!(missing.len(), 1 + 1);
    assert!(
        missing
            .iter()
            .all(|x| x.fixes.is_empty() && x.related.is_empty())
    );
    assert!(
        d.out.not_implemented.is_empty(),
        "{:?}",
        d.out.not_implemented
    );
}

#[test]
fn a_diagnostic_inside_the_builtin_declarations_is_a_compiler_failure_not_an_error() {
    // A `std` without `hash` cannot resolve the declaration of `Hash`.
    let mut sources = Sources::default();
    let mut modules = Vec::new();
    for (path, text) in [
        ("std", "pub struct Env {}\n"),
        ("std.fmt", include_str!("../../../std/fmt.ingot")),
    ] {
        let file = sources.add(Source::new(format!("{path}.ingot"), text));
        let source = sources.get(file);
        let (tokens, _) = lex(source);
        let (ast, _) = parse(source, tokens);
        modules.push(ModuleInput {
            path: path.to_string(),
            file,
            ast: Some(ast),
        });
    }
    let target = Target::by_name("aarch64-macos-m4").expect("a target");
    let out = resolve(
        &mut sources,
        vec![PackageInput {
            name: "std".to_string(),
            modules,
        }],
        &target,
    );
    assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
    assert!(
        out.not_implemented
            .iter()
            .any(|m| m.starts_with("internal error: E0301 points into the builtin declarations")),
        "{:?}",
        out.not_implemented
    );
}

#[test]
fn builtin_types_implement_what_section_4_4_lists() {
    let mut d = app("enum Color { red, green }\n\
         enum Empty {}\n\
         enum Gen[T] { a, b }\n\
         enum Shape { dot, circle: i64 }\n\
         struct P { x: i64 }\n");
    assert_eq!(d.codes(), vec![]);
    let ints = [
        IntTy::I8,
        IntTy::I16,
        IntTy::I32,
        IntTy::I64,
        IntTy::U8,
        IntTy::U16,
        IntTy::U32,
        IntTy::U64,
    ];
    let (y, n) = (Tri::Yes, Tri::No);
    // (Eq, Ord, Hash, Format, Iter)
    let mut table: Vec<(String, TypeId, [Tri; 5])> = Vec::new();
    for i in ints {
        let t = d.int(i);
        table.push((i.name().to_string(), t, [y, y, y, y, n]));
    }
    let bool_ty = d.kind(TyKind::Bool);
    table.push(("bool".into(), bool_ty, [y, y, y, y, n]));
    for f in [crate::FloatTy::F32, crate::FloatTy::F64] {
        let t = d.kind(TyKind::Float(f));
        table.push((f.name().to_string(), t, [y, y, n, y, n]));
    }
    let u8_ty = d.int(IntTy::U8);
    let i64_ty = d.int(IntTy::I64);
    for m in [Mutability::Shared, Mutability::Mut] {
        let t = d.kind(TyKind::RawPtr(m, u8_ty));
        table.push(("raw pointer".into(), t, [y, y, y, n, n]));
    }
    for name in ["Color", "Empty"] {
        let t = d.adt(name);
        table.push((name.into(), t, [y, y, y, n, n]));
    }
    let gen_i64 = d.adt_of("app", "Gen", vec![GenericArg::Ty(i64_ty)]);
    table.push(("Gen[i64]".into(), gen_i64, [y, y, y, n, n]));
    let ordering = d.builtin("Ordering");
    let t = d.kind(TyKind::Adt(ordering, vec![]));
    table.push(("Ordering".into(), t, [y, y, y, n, n]));
    for name in ["Shape", "P"] {
        let t = d.adt(name);
        table.push((name.into(), t, [n, n, n, n, n]));
    }
    let void = d.kind(TyKind::Void);
    table.push(("void".into(), void, [n; 5]));
    let tuple = d.kind(TyKind::Tuple(vec![i64_ty, i64_ty]));
    table.push(("tuple".into(), tuple, [y, y, y, n, n]));
    let r = d.kind(TyKind::Ref(Mutability::Shared, i64_ty));
    table.push(("&i64".into(), r, [y, y, y, n, n]));
    let o = d.kind(TyKind::Optional(i64_ty));
    table.push(("?i64".into(), o, [y, y, y, n, n]));
    let arr = d.kind(TyKind::Array(
        u8_ty,
        ConstArg::Value(ConstValue::Int {
            ty: IntTy::I64,
            value: 4,
        }),
    ));
    table.push(("[4]u8".into(), arr, [y, y, y, n, n]));
    // Slices of `u8` format, `[]u8` and `[]mut u8` alike (section 4.4).
    let s = d.kind(TyKind::Slice(Mutability::Shared, u8_ty));
    table.push(("[]u8".into(), s, [y, y, y, y, n]));
    let s = d.kind(TyKind::Slice(Mutability::Mut, u8_ty));
    table.push(("[]mut u8".into(), s, [y, y, y, y, n]));
    let s = d.kind(TyKind::Slice(Mutability::Shared, i64_ty));
    table.push(("[]i64".into(), s, [y, y, y, n, n]));
    // Vectors and masks format, and nothing else (section 4.4).
    let v = d.kind(TyKind::Vector(crate::Lane::Float(crate::FloatTy::F32), 4));
    table.push(("f32x4".into(), v, [n, n, n, y, n]));
    let v = d.kind(TyKind::Vector(crate::Lane::Int(IntTy::U8), 16));
    table.push(("u8x16".into(), v, [n, n, n, y, n]));
    let m = d.kind(TyKind::Mask(4));
    table.push(("boolx4".into(), m, [n, n, n, y, n]));
    for (name, ty, want) in table {
        let got = [
            d.satisfies(ty, "Eq"),
            d.satisfies(ty, "Ord"),
            d.satisfies(ty, "Hash"),
            d.satisfies(ty, "Format"),
            d.satisfies(ty, "Iter"),
        ];
        assert_eq!(got, want, "{name}");
    }
}

#[test]
fn a_user_impl_still_makes_a_type_satisfy_a_builtin_interface() {
    let mut d = app_with_std(
        "import std.hash.Hasher;\n\
         struct Keyed[K: Hash] { k: K }\n\
         struct P { x: i64 }\n\
         struct Q { x: i64 }\n\
         impl Hash for P { fn hash(self: &P, state: &mut Hasher) { } }\n\
         struct A { a: Keyed[P] }\n\
         struct B { b: Keyed[Q] }\n\
         struct C { c: Keyed[i64] }\n\
         struct D { d: Keyed[u8x16] }\n",
    );
    assert_eq!(
        d.codes(),
        vec![("E0801".to_string(), 7), ("E0801".to_string(), 9)]
    );
    assert!(d.gaps().is_empty(), "{:?}", d.gaps());
    let p = d.adt("P");
    assert_eq!(d.satisfies(p, "Hash"), Tri::Yes);
    assert_eq!(d.satisfies(p, "Eq"), Tri::No);
}

#[test]
fn a_type_parameter_satisfies_a_builtin_interface_through_its_bounds() {
    let mut d = app("fn f[T: Ord](a: T) { }\n\
         fn g[T: Eq](a: T) { }\n\
         fn h[T](a: T) { }\n");
    assert_eq!(d.codes(), vec![]);
    let param = |d: &mut D, f: &str| {
        let def = d.value_def(f);
        let g = d.decls.generics_of(def)[0].def;
        d.decls.types.param(g)
    };
    let (t_f, t_g, t_h) = (param(&mut d, "f"), param(&mut d, "g"), param(&mut d, "h"));
    // `Ord: Eq`, so an `Ord` parameter is `Eq`; not the other way round.
    assert_eq!(d.satisfies(t_f, "Ord"), Tri::Yes);
    assert_eq!(d.satisfies(t_f, "Eq"), Tri::Yes);
    assert_eq!(d.satisfies(t_f, "Hash"), Tri::No);
    assert_eq!(d.satisfies(t_g, "Ord"), Tri::No);
    assert_eq!(d.satisfies(t_h, "Eq"), Tri::No);
}

#[test]
fn method_lookup_finds_the_builtin_implementations_and_user_impls() {
    let mut d = app("struct P { x: i64 }\n\
         impl P { fn cmp(self: &P) -> i64 { return 0; } fn make() -> P { return P.{ x: 1 }; } }\n\
         impl Eq for P { fn eq(self: &P, other: &P) -> bool { return true; } }\n\
         impl Ord for P { fn cmp(self: &P, other: &P) -> Ordering { return .equal; } }\n\
         interface Shown { fn show(self: &Self) -> i64 { return 0; } }\n\
         impl Shown for P {}\n\
         enum Color { red, green }\n\
         fn f[T: Ord](a: T) { }\n");
    assert_eq!(d.codes(), vec![]);
    let (eq, ord, hash, format) = (
        d.builtin("Eq"),
        d.builtin("Ord"),
        d.builtin("Hash"),
        d.builtin("Format"),
    );
    let method = |d: &D, i: DefId, name: &str| d.decls.interface_method(i, name).expect("a method");
    // `cmp` on `i64` is the builtin implementation of `Ord`, and the function is the
    // one the interface declares.
    let i64_ty = d.int(IntTy::I64);
    let cmp_ord = method(&d, ord, "cmp");
    assert_eq!(
        d.method_sources(i64_ty, "cmp"),
        vec![("builtin Ord".to_string(), cmp_ord)]
    );
    assert_eq!(
        d.method_sources(i64_ty, "eq"),
        vec![("builtin Eq".to_string(), method(&d, eq, "eq"))]
    );
    assert_eq!(
        d.method_sources(i64_ty, "hash"),
        vec![("builtin Hash".to_string(), method(&d, hash, "hash"))]
    );
    assert!(d.method_sources(i64_ty, "len").is_empty());
    // Floats compare but do not hash; a payload-free enum does both; `Format` is for
    // scalars and vectors.
    let f64_ty = d.kind(TyKind::Float(crate::FloatTy::F64));
    assert_eq!(d.method_sources(f64_ty, "cmp").len(), 1);
    assert!(d.method_sources(f64_ty, "hash").is_empty());
    assert_eq!(d.method_sources(f64_ty, "format").len(), 1);
    let color = d.adt("Color");
    assert_eq!(d.method_sources(color, "cmp").len(), 1);
    assert_eq!(d.method_sources(color, "hash").len(), 1);
    assert!(d.method_sources(color, "format").is_empty());
    let v = d.kind(TyKind::Vector(crate::Lane::Int(IntTy::U8), 16));
    assert_eq!(
        d.method_sources(v, "format"),
        vec![("builtin Format".to_string(), method(&d, format, "format"))]
    );
    // A user type: the inherent `cmp` first, then the interface's. A function an impl
    // leaves out but the interface gives a body is the interface's own.
    let p = d.adt("P");
    let got = d.method_sources(p, "cmp");
    assert_eq!(got.len(), 2);
    assert_eq!(got[0].0, "inherent");
    assert_eq!(got[1].0, "impl Ord");
    assert_ne!(got[0].1, got[1].1);
    let shown = d.type_def("app", "Shown");
    let show = d.decls.interface_method(shown, "show").expect("show");
    assert_eq!(
        d.method_sources(p, "show"),
        vec![("impl Shown".to_string(), show)]
    );
    // A type parameter: through its bounds and their supertraits.
    let fdef = d.value_def("f");
    let g = d.decls.generics_of(fdef)[0].def;
    let t = d.decls.types.param(g);
    assert_eq!(
        d.method_sources(t, "cmp"),
        vec![("bound Ord".to_string(), cmp_ord)]
    );
    assert_eq!(
        d.method_sources(t, "eq"),
        vec![("bound Eq".to_string(), method(&d, eq, "eq"))]
    );
}

#[test]
fn only_std_implements_executor() {
    let mut d = app_with_std("struct Mine {}\nimpl Executor for Mine {}\n");
    assert_eq!(d.codes(), vec![("E0525".to_string(), 2)]);
    for name in ["Seq", "Simd", "Par"] {
        let t = d.adt_of("std.exec", name, vec![]);
        assert_eq!(d.satisfies(t, "Executor"), Tri::Yes, "{name}");
    }
    let i64_ty = d.int(IntTy::I64);
    assert_eq!(d.satisfies(i64_ty, "Executor"), Tri::No);
    let mine = d.adt("Mine");
    assert_eq!(d.satisfies(mine, "Executor"), Tri::No);
    // Within `std` the same impl is allowed: the library checks cleanly.
    let d = app_with_std("");
    assert_eq!(d.codes(), vec![]);
}

#[test]
fn an_impl_of_a_builtin_implementation_or_for_a_builtin_type_is_e0507_e0506() {
    let d = app("enum Color { red, green }\n\
         impl Eq for Color { fn eq(self: &Color, other: &Color) -> bool { return true; } }\n\
         impl Eq for i64 { fn eq(self: &i64, other: &i64) -> bool { return true; } }\n\
         struct P { x: i64 }\n\
         impl Eq for P { fn eq(self: &P, other: &P) -> bool { return true; } }\n\
         impl Ord for P { fn cmp(self: &P, other: &P) -> Ordering { return .equal; } }\n");
    assert_eq!(
        d.codes(),
        vec![("E0506".to_string(), 3), ("E0507".to_string(), 2)]
    );
}

// --------------------------------------------------------------- the new codes

#[test]
fn e0520_alias_cycles() {
    let d = app(
        "type A = B;\ntype B = [4]A;\ntype C = (i64, C);\ntype D[T] = ?D[T];\n\
         type Fine[T] = ?T;\nstruct S { x: Fine[i64] }\n",
    );
    assert_eq!(
        d.codes(),
        vec![
            ("E0520".to_string(), 1),
            ("E0520".to_string(), 3),
            ("E0520".to_string(), 4)
        ]
    );
    assert!(d.gaps().is_empty(), "{:?}", d.gaps());
}

#[test]
fn e0521_e0522_negative_lengths_and_bad_tag_types() {
    let d = app("const n: i64 = 0 - 2;\n\
         struct A { a: [-1]u8, b: [n]u8, c: [0]u8, d: [4 - 4]u8 }\n\
         enum E1: f32 { a }\n\
         enum E2: bool { a }\n\
         enum E3: []u8 { a }\n\
         enum E4: u8 { a = 255 }\n\
         enum E5: Missing { a }\n");
    assert_eq!(
        d.codes(),
        vec![
            ("E0521".to_string(), 2),
            ("E0521".to_string(), 2),
            ("E0522".to_string(), 3),
            ("E0522".to_string(), 4),
            ("E0522".to_string(), 5)
        ]
    );
}

#[test]
fn e0523_e0524_e0525_interfaces_bounds_and_types() {
    let d = app("struct Point { x: i64 }\n\
         interface Shape { fn area(self: &Self) -> i64; }\n\
         impl Point for Point {}\n\
         impl Option for Point {}\n\
         fn a[T: Point](x: T) { }\n\
         fn b[T: Fn](x: T) { }\n\
         fn c[T: Option](x: T) { }\n\
         fn d(x: &dyn Point, y: &dyn Fn, z: &dyn Copy) { }\n\
         struct H { a: Shape, b: Ord, c: Copy, d: Num, f: Executor, g: Eq }\n\
         impl Copy for Point {}\n\
         impl Discard for Point {}\n\
         impl Owned for Point {}\n\
         impl Num for Point {}\n\
         impl Int for Point {}\n\
         impl Fn for Point {}\n\
         unsafe impl Send for Point {}\n\
         unsafe impl Sync for Point {}\n");
    let got = d.codes();
    let on = |line: u32| -> Vec<&str> {
        got.iter()
            .filter(|(_, l)| *l == line)
            .map(|(c, _)| c.as_str())
            .collect()
    };
    for line in 3..=7 {
        assert_eq!(on(line), vec!["E0523"], "line {line}");
    }
    assert_eq!(on(8), vec!["E0523", "E0523", "E0523"]);
    assert_eq!(on(9), vec!["E0524"; 6]);
    for line in 10..=14 {
        assert_eq!(on(line), vec!["E0525"], "line {line}");
    }
    // `Fn` with no signature is not an interface at all (E0523); `Send` and `Sync` are
    // the properties an `unsafe impl` states.
    assert_eq!(on(15), vec!["E0523"]);
    assert_eq!(got.len(), 5 + 3 + 6 + 5 + 1, "{got:?}");
    assert!(d.gaps().is_empty(), "{:?}", d.gaps());
}

#[test]
fn e0508_e0509_the_new_conditions() {
    let d = app("interface M { fn m(self: &Self) -> i64; }\n\
         struct S { n: i64 }\n\
         impl[T] T { fn get(self: &T) -> i64 { return 0; } }\n\
         impl[T] M for T {}\n\
         impl M for S {\n\
             const k: i64 = 1;\n\
             fn m(self: &S) -> i64 { return 0; }\n\
         }\n\
         impl S { const fine: i64 = 1; }\n");
    assert_eq!(
        d.codes(),
        vec![
            ("E0508".to_string(), 3),
            ("E0508".to_string(), 4),
            ("E0509".to_string(), 6)
        ]
    );
    assert!(d.gaps().is_empty(), "{:?}", d.gaps());
}

#[test]
fn e0812_e0813_constants_and_generic_argument_kinds() {
    let d = app("struct Ring[T, const n: i64] { items: [n]T }\n\
         struct W[T] { t: T }\n\
         struct A { a: Ring[u8, i64], b: Ring[u8, []u8], c: Ring[u8, 4], d: Ring[u8, { 2 + 2 }] }\n\
         struct B { a: W[4], b: W[{ 1 }], c: W[u8] }\n\
         struct G[const n: i64] { a: Ring[u8, n], b: Ring[n, u8] }\n");
    assert_eq!(
        d.codes(),
        vec![
            ("E0813".to_string(), 3),
            ("E0813".to_string(), 3),
            ("E0813".to_string(), 4),
            ("E0813".to_string(), 4),
            // `Ring[n, u8]`: a constant for the type parameter, a type for the constant.
            ("E0813".to_string(), 5),
            ("E0813".to_string(), 5)
        ]
    );
    assert!(d.gaps().is_empty(), "{:?}", d.gaps());
}

#[test]
fn e1601_a_fallible_foreign_result() {
    let d = app("enum Oops { bad }\n\
         extern \"c\" {\n\
             fn a() -> i32 ! Oops;\n\
             fn b() -> i32;\n\
             fn c(x: i32) -> void ! Oops;\n\
         }\n");
    assert_eq!(
        d.codes(),
        vec![("E1601".to_string(), 3), ("E1601".to_string(), 5)]
    );
    assert!(d.gaps().is_empty(), "{:?}", d.gaps());
}

#[test]
fn numeric_bounds_grant_owned_send_sync_and_discard() {
    let mut d = app(
        "struct S[A: Int, B: SInt, C: UInt, D: Float, E: Num] { a: A, b: B, c: C, d: D, e: E }\n",
    );
    assert_eq!(d.codes(), vec![]);
    let s = d.type_def("app", "S");
    let g: Vec<DefId> = d.decls.generics_of(s).iter().map(|g| g.def).collect();
    for (i, def) in g.into_iter().enumerate() {
        let p = d.decls.types.param(def);
        let p = d.decls.props(p);
        assert_eq!(
            (
                p.copy,
                p.discard(),
                p.owned(),
                p.send,
                p.sync,
                p.linear,
                p.view
            ),
            (
                Tri::Yes,
                Tri::Yes,
                Tri::Yes,
                Tri::Yes,
                Tri::Yes,
                Tri::No,
                Tri::No
            ),
            "parameter {i}"
        );
    }
    // So a numeric parameter fits where `Owned`, `Send` and `Sync` are required.
    let d = app_with_std(
        "import std.sync.Mutex;\n\
         struct A[T: Num] { m: Mutex[T] }\n\
         struct B[T: Copy] { m: Mutex[T] }\n",
    );
    assert_eq!(d.codes(), vec![("E0801".to_string(), 3)]);
}

#[test]
fn the_associated_type_of_iter_is_projected_and_other_builtins_declare_none() {
    let d = app("struct W[I: Iter] { x: ?I.Item }\n\
         struct X[E: Executor] { x: E.Item }\n\
         struct Y[T: Eq] { x: T.Item }\n\
         struct Z[T: Ord + Iter] { x: T.Item }\n");
    assert_eq!(
        d.codes(),
        vec![("E0519".to_string(), 2), ("E0519".to_string(), 3)]
    );
    assert!(d.ni().is_empty(), "{:?}", d.ni());
    let x = d.field("W", "x");
    assert_eq!(d.display(x), "?I.Item");
}
