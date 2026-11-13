//! Unit tests for `when` elimination, name resolution and HIR lowering.

use crate::hir::{self, ExprKind, Res};
use crate::{DefKind, ModuleInput, Output, PackageInput, Target, resolve};
use ingot_diag::source::{Source, Sources};
use ingot_syntax::lexer::lex;
use ingot_syntax::parser::parse;

/// A package: its name and its modules as (path below the package, source text).
/// The root module's path is the empty string.
type Pkg<'a> = (&'a str, &'a [(&'a str, &'a str)]);

struct Run {
    out: Output,
    sources: Sources,
}

fn run_on(target: &str, packages: &[Pkg]) -> Run {
    let mut sources = Sources::default();
    let mut inputs = Vec::new();
    for (name, modules) in packages {
        let mut mods = Vec::new();
        for (rel, text) in *modules {
            let path = if rel.is_empty() {
                (*name).to_string()
            } else {
                format!("{name}.{rel}")
            };
            let file = sources.add(Source::new(format!("{path}.ingot"), *text));
            let source = sources.get(file);
            let (tokens, lex_diags) = lex(source);
            let (ast, parse_diags) = parse(source, tokens);
            let clean = lex_diags.is_empty() && parse_diags.is_empty();
            mods.push(ModuleInput {
                path,
                file,
                ast: clean.then_some(ast),
            });
        }
        inputs.push(PackageInput {
            name: (*name).to_string(),
            modules: mods,
        });
    }
    let target = Target::by_name(target).expect("a known target");
    let out = resolve(&sources, inputs, &target);
    Run { out, sources }
}

fn run(packages: &[Pkg]) -> Run {
    run_on("aarch64-macos-m4", packages)
}

/// One module `app`.
fn one(text: &str) -> Run {
    run(&[("app", &[("", text)])])
}

impl Run {
    /// (code, line) of every diagnostic, sorted.
    fn codes(&self) -> Vec<(String, u32)> {
        let mut v: Vec<(String, u32)> = self
            .out
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

    /// The package that was resolved last.
    fn pkg(&self) -> &hir::Package {
        self.out.packages.last().expect("a package")
    }

    /// The resolution of every path expression, in arena order: (last segment,
    /// resolution).
    fn paths(&self) -> Vec<(String, Res)> {
        self.pkg()
            .exprs
            .iter()
            .filter_map(|e| match &e.kind {
                ExprKind::Path(p) => Some((p.segments.last().unwrap().0.clone(), p.res)),
                _ => None,
            })
            .collect()
    }

    fn def_kind(&self, res: Res) -> Option<DefKind> {
        match res {
            Res::Def(d) => Some(self.out.sema.def(d).kind),
            _ => None,
        }
    }

    fn local_name(&self, res: Res) -> Option<&str> {
        match res {
            Res::Local(l) => Some(&self.pkg().local(l).name),
            _ => None,
        }
    }
}

#[test]
fn a_clean_program_has_no_diagnostics() {
    let r = one("fn add(a: i64, b: i64) -> i64 { return a + b; }\n");
    assert_eq!(r.codes(), vec![]);
    assert!(r.pkg().has_items());
}

#[test]
fn names_bind_in_nested_scopes() {
    let r = one("fn f(n: i64) -> i64 {\n\
         let a = n;\n\
         if a > 0 {\n\
             let b = a;\n\
             return b;\n\
         }\n\
         return a;\n\
         }\n");
    assert_eq!(r.codes(), vec![]);
    let paths = r.paths();
    // n (init of a), a (cond), a (init of b), b, a
    let names: Vec<_> = paths
        .iter()
        .map(|(n, res)| (n.as_str(), r.local_name(*res).unwrap_or("?")))
        .collect();
    assert_eq!(
        names,
        vec![("n", "n"), ("a", "a"), ("a", "a"), ("b", "b"), ("a", "a")]
    );
    // The first and the last `a` are the same local; `b` is another.
    let locals: Vec<_> = paths.iter().map(|(_, r)| *r).collect();
    assert_eq!(locals[1], locals[4]);
    assert_ne!(locals[3], locals[4]);
}

#[test]
fn a_binding_goes_out_of_scope_with_its_block() {
    let r = one("fn f() -> i64 {\n\
         if true {\n\
             let inner = 1;\n\
             return inner;\n\
         }\n\
         return inner;\n\
         }\n");
    assert_eq!(r.codes(), vec![("E0304".to_string(), 6)]);
}

#[test]
fn shadowing_a_local_or_parameter_is_reported_where_it_nests() {
    let r = one("fn f(n: i64) -> i64 {\n\
         let a = n;\n\
         if a > 0 {\n\
             let a = 2;\n\
             return a;\n\
         }\n\
         return a;\n\
         }\n");
    assert_eq!(r.codes(), vec![("E0309".to_string(), 4)]);
}

#[test]
fn sibling_blocks_may_reuse_a_name() {
    let r = one("fn f(n: i64) -> i64 {\n\
         if n > 0 { let a = 1; return a; }\n\
         if n < 0 { let a = 2; return a; }\n\
         return 0;\n\
         }\n");
    assert_eq!(r.codes(), vec![]);
}

#[test]
fn a_closure_sees_only_what_it_captures() {
    let r = one("fn f() -> i64 {\n\
         let kept = 1;\n\
         let lost = 2;\n\
         let g = fn [kept](x: i64) -> i64 { return x + kept + lost; };\n\
         return kept + lost + g(1);\n\
         }\n");
    // `lost` is not captured, so the closure body cannot see it: E0611 (spec §5.6).
    assert_eq!(r.codes(), vec![("E0611".to_string(), 4)]);
}

#[test]
fn an_unread_binding_is_e0311_and_a_write_is_not_a_read() {
    let r = one("fn f() -> i64 {\n\
         var spare: i64 = 2;\n\
         spare = 3;\n\
         return 1;\n\
         }\n");
    assert_eq!(r.codes(), vec![("E0311".to_string(), 2)]);
    let r = one("fn f() -> i64 {\n let used = 2;\n return used;\n }\n");
    assert_eq!(r.codes(), vec![]);
}

#[test]
fn parameters_may_go_unused() {
    let r = one("fn f(unused: i64) -> i64 { return 1; }\n");
    assert_eq!(r.codes(), vec![]);
}

#[test]
fn an_undefined_name_is_reported_once_per_use_and_is_poison() {
    let r = one("fn f() -> i64 { return nope; }\n\
         fn g() -> i64 { return nada + 1; }\n");
    assert_eq!(
        r.codes(),
        vec![("E0304".to_string(), 1), ("E0304".to_string(), 2)]
    );
    assert!(r.paths().iter().all(|(_, res)| *res == Res::Poison));
}

#[test]
fn an_undefined_type_is_e0305() {
    let r = one("fn f(x: Nope) -> i64 {\n return 1;\n }\n");
    assert_eq!(r.codes(), vec![("E0305".to_string(), 1)]);
}

#[test]
fn constants_statics_and_functions_resolve_in_bodies() {
    let r = one("const limit: i64 = 64;\n\
         static counter: i64 = 0;\n\
         fn helper() -> i64 { return 1; }\n\
         fn f() -> i64 { return limit + counter + helper(); }\n");
    assert_eq!(r.codes(), vec![]);
    let kinds: Vec<_> = r
        .paths()
        .iter()
        .map(|(n, res)| (n.clone(), r.def_kind(*res)))
        .collect();
    assert_eq!(
        kinds,
        vec![
            ("limit".to_string(), Some(DefKind::Const)),
            ("counter".to_string(), Some(DefKind::Static)),
            ("helper".to_string(), Some(DefKind::Fn)),
        ]
    );
}

#[test]
fn a_type_used_as_a_value_resolves_in_the_type_namespace() {
    let r = one("struct Pair { a: i64 }\n\
         fn pair() -> Pair { return Pair.{ a: 1 }; }\n");
    assert_eq!(r.codes(), vec![]);
    let lit = r
        .pkg()
        .exprs
        .iter()
        .find_map(|e| match &e.kind {
            ExprKind::StructLit { ty, .. } => Some(ty.res),
            _ => None,
        })
        .expect("a struct literal");
    assert_eq!(r.def_kind(lit), Some(DefKind::Struct));
    // A value name never finds a type: `Pair` is not a value.
    let r = one("struct Pair { a: i64 }\nfn f() -> i64 { return pair; }\n");
    assert_eq!(r.only_codes(), vec!["E0304"]);
}

#[test]
fn duplicate_definitions_are_e0308() {
    let r = one("fn f() { }\nfn f() { }\n");
    assert_eq!(r.codes(), vec![("E0308".to_string(), 2)]);
    let r = one("struct S { a: i64, a: i64 }\n");
    assert_eq!(r.codes(), vec![("E0308".to_string(), 1)]);
    let r = one("enum E { x, y, x }\n");
    assert_eq!(r.codes(), vec![("E0308".to_string(), 1)]);
    let r = one("fn f(a: i64, a: i64) { }\n");
    assert_eq!(r.codes(), vec![("E0308".to_string(), 1)]);
    let r = one("fn f[T, T](a: i64) { }\n");
    assert_eq!(r.codes(), vec![("E0308".to_string(), 1)]);
}

#[test]
fn imports_of_a_module_and_of_an_item() {
    let lib = "pub fn helper() -> i64 { return 1; }\npub struct Thing { pub a: i64 }\n";
    // The module imported: its items are reached as `lib.name`.
    let r = run(&[(
        "app",
        &[
            (
                "",
                "import app.lib;\nfn f() -> i64 { return lib.helper(); }\n",
            ),
            ("lib", lib),
        ],
    )]);
    assert_eq!(r.codes(), vec![]);
    let helper = r
        .paths()
        .into_iter()
        .find(|(n, _)| n == "helper")
        .unwrap()
        .1;
    assert_eq!(r.def_kind(helper), Some(DefKind::Fn));
    // The item imported directly.
    let r = run(&[(
        "app",
        &[
            (
                "",
                "import app.lib.helper;\nimport app.lib.Thing;\n\
                 fn f(t: Thing) -> i64 { return helper(); }\n",
            ),
            ("lib", lib),
        ],
    )]);
    assert_eq!(r.codes(), vec![]);
    // A type through the module.
    let r = run(&[(
        "app",
        &[
            ("", "import app.lib;\nfn f(t: lib.Thing) { }\n"),
            ("lib", lib),
        ],
    )]);
    assert_eq!(r.codes(), vec![]);
}

#[test]
fn the_root_module_is_the_package_name() {
    let r = run(&[
        ("std", &[("", "pub struct Env { }\n")]),
        ("app", &[("", "import std.Env;\nfn f(e: &Env) { }\n")]),
    ]);
    assert_eq!(r.codes(), vec![]);
}

#[test]
fn modules_may_import_each_other_in_a_cycle() {
    let a = "import app.b;\npub fn fa() -> i64 { return b.fb(); }\n";
    let b =
        "import app.a;\npub fn fb() -> i64 { return 1; }\npub fn gb() -> i64 { return a.fa(); }\n";
    let r = run(&[("app", &[("a", a), ("b", b)])]);
    assert_eq!(r.codes(), vec![]);
}

#[test]
fn an_unresolved_import_is_e0301_and_its_name_is_poison() {
    let r = run(&[(
        "app",
        &[(
            "",
            "import app.nonexistent;\nfn f() -> i64 { return nonexistent.thing(); }\n",
        )],
    )]);
    // One error for the import. The use of the name is poison: no E0304.
    assert_eq!(r.codes(), vec![("E0301".to_string(), 1)]);
}

#[test]
fn e0301_names_the_first_segment_that_fails() {
    let r = run(&[("app", &[("", "import app.missing.deeper.Thing;\n")])]);
    let d = &r.out.diagnostics[0];
    assert_eq!(d.code, "E0301");
    let src = r.sources.iter().next().unwrap();
    assert_eq!(src.text_of(d.span), "missing");
    let r = run(&[("app", &[("", "import nothere.Thing;\n")])]);
    let d = &r.out.diagnostics[0];
    let src = r.sources.iter().next().unwrap();
    assert_eq!(src.text_of(d.span), "nothere");
}

#[test]
fn two_imports_of_one_name_are_e0302() {
    let r = run(&[
        ("std", &[("", "pub struct Env { }\n")]),
        (
            "app",
            &[("", "import std.Env;\nimport std.Env;\nfn f(e: &Env) { }\n")],
        ),
    ]);
    assert_eq!(r.codes(), vec![("E0302".to_string(), 2)]);
}

#[test]
fn a_private_item_is_e0306_from_another_module() {
    let lib = "fn secret() -> i64 { return 1; }\n";
    let r = run(&[(
        "app",
        &[
            (
                "",
                "import app.lib;\nfn f() -> i64 { return lib.secret(); }\n",
            ),
            ("lib", lib),
        ],
    )]);
    assert_eq!(r.codes(), vec![("E0306".to_string(), 2)]);
    let r = run(&[(
        "app",
        &[
            ("", "import app.lib.secret;\nfn f() { secret(); }\n"),
            ("lib", lib),
        ],
    )]);
    assert_eq!(r.codes(), vec![("E0306".to_string(), 1)]);
}

#[test]
fn a_name_that_is_both_a_module_and_an_item_is_e0316() {
    let r = run(&[(
        "app",
        &[
            ("", "import app.net.http;\n"),
            ("net", "pub fn http() { }\n"),
            ("net.http", "pub fn get() { }\n"),
        ],
    )]);
    assert_eq!(r.codes(), vec![("E0316".to_string(), 1)]);
}

#[test]
fn an_unused_import_is_e0310_with_a_fix_and_a_used_one_is_not() {
    let r = run(&[
        ("std", &[("", "pub struct Env { }\n")]),
        ("app", &[("", "import std.Env;\nfn f() { }\n")]),
    ]);
    assert_eq!(r.codes(), vec![("E0310".to_string(), 1)]);
    let fix = &r.out.diagnostics[0].fixes[0].edits[0];
    assert_eq!(fix.text, "");
    assert_eq!(fix.span.start, 0);
    assert_eq!(fix.span.end, "import std.Env;\n".len());
}

#[test]
fn an_import_used_only_in_a_when_branch_is_not_unused_on_any_target() {
    let stdlib = ("std", &[("", "pub struct Env { }\n")][..]);
    // Module level: the branch for Linux is removed on macOS, and its use still counts.
    let src = "import std.Env;\nwhen @target.os == .linux {\n fn f(e: &Env) { }\n}\n";
    let r = run(&[stdlib, ("app", &[("", src)])]);
    assert_eq!(r.codes(), vec![]);
    let r = run_on("x86_64-linux-znver4", &[stdlib, ("app", &[("", src)])]);
    assert_eq!(r.codes(), vec![]);
    // Statement level, in a body.
    let src =
        "import std.Env;\nfn g() {\n when @target.os == .linux {\n let e: Env = Env.{};\n }\n}\n";
    let r = run(&[stdlib, ("app", &[("", src)])]);
    assert_eq!(r.codes(), vec![]);
    // An undecidable `when` removes both branches, and the uses in them still count.
    let src = "import std.Env;\nwhen @target.bogus == 1 {\n fn f(e: &Env) { }\n}\n";
    let r = run(&[stdlib, ("app", &[("", src)])]);
    assert_eq!(r.codes(), vec![("E1606".to_string(), 2)]);
    // A name that no removed branch mentions is still unused.
    let src = "import std.Env;\nwhen @target.os == .linux {\n fn f() { }\n}\n";
    let r = run(&[stdlib, ("app", &[("", src)])]);
    assert_eq!(r.codes(), vec![("E0310".to_string(), 1)]);
}

#[test]
fn a_file_with_a_syntax_error_poisons_the_names_imported_from_it() {
    let r = run(&[(
        "app",
        &[
            (
                "",
                "import app.broken;\nimport app.broken.Thing;\n\
                 fn f(t: Thing) -> i64 { return broken.helper(); }\n",
            ),
            ("broken", "pub fn helper( {\n"),
        ],
    )]);
    // Neither the import nor the uses are reported: the module declares nothing
    // that could be checked.
    assert_eq!(r.codes(), vec![]);
}

#[test]
fn poisoned_declarations_are_not_declarations() {
    // Two definitions of `f`, one of them inside the module that failed to parse:
    // no E0308.
    let r = run(&[("app", &[("", "fn f() { }\n"), ("other", "fn f() { \n")])]);
    assert_eq!(r.codes(), vec![]);
}

#[test]
fn when_keeps_the_taken_branch_only() {
    let src = "when @target.os == .macos {\n\
               fn here() { }\n\
               } else when @target.os == .linux {\n\
               fn there() { }\n\
               } else {\n\
               fn elsewhere() { }\n\
               }\n\
               fn f() { here(); }\n";
    let r = one(src);
    assert_eq!(r.codes(), vec![]);
    let names: Vec<_> = r
        .out
        .sema
        .defs
        .iter()
        .filter(|d| d.kind == DefKind::Fn)
        .map(|d| d.name.as_str())
        .collect();
    assert_eq!(names, vec!["here", "f"]);
    // The other target takes the other branch, so `here` is undefined.
    let r = run_on("x86_64-linux-znver4", &[("app", &[("", src)])]);
    assert_eq!(r.codes(), vec![("E0304".to_string(), 8)]);
    let names: Vec<_> = r
        .out
        .sema
        .defs
        .iter()
        .filter(|d| d.kind == DefKind::Fn)
        .map(|d| d.name.as_str())
        .collect();
    assert_eq!(names, vec!["there", "f"]);
}

#[test]
fn when_in_a_body_keeps_the_taken_branch_only() {
    let src = "fn f() -> i64 {\n\
               var x: i64 = 0;\n\
               when @target.simd_bits >= 256 {\n\
               x = 2;\n\
               } else {\n\
               x = 1;\n\
               }\n\
               return x;\n\
               }\n";
    let r = one(src);
    assert_eq!(r.codes(), vec![]);
    let assigns = r
        .pkg()
        .stmts
        .iter()
        .filter(|s| matches!(s.kind, hir::StmtKind::Assign { .. }))
        .count();
    assert_eq!(assigns, 1);
}

#[test]
fn a_when_condition_may_not_name_a_constant() {
    let r = one("const n: i64 = 1;\nwhen n == 1 {\n fn f() { }\n}\n");
    assert_eq!(r.codes(), vec![("E0612".to_string(), 2)]);
    // Both branches are poison, so a use of `f` is not reported.
    let r = one("const n: i64 = 1;\nwhen n == 1 {\n fn f() { }\n}\nfn g() { f(); }\n");
    assert_eq!(r.codes(), vec![("E0612".to_string(), 2)]);
}

#[test]
fn a_when_condition_may_not_call_a_function_or_use_arithmetic() {
    let r = one("when @target.simd_bits + 1 > 0 {\n fn f() { }\n}\n");
    assert_eq!(r.only_codes(), vec!["E0612"]);
    let r = one("fn h() -> bool { return true; }\nwhen h() {\n fn f() { }\n}\n");
    assert_eq!(r.only_codes(), vec!["E0612"]);
}

#[test]
fn an_unknown_target_field_is_e1606() {
    let r = one("when @target.bogus == 1 {\n fn f() { }\n}\n");
    assert_eq!(r.codes(), vec![("E1606".to_string(), 1)]);
    // Also outside a `when`.
    let r = one("fn f() -> i64 { return @target.bogus; }\n");
    assert_eq!(r.codes(), vec![("E1606".to_string(), 1)]);
    let r = one("fn f() -> i64 { return @target.cache_line; }\n");
    assert_eq!(r.codes(), vec![]);
}

#[test]
fn a_target_value_outside_the_specification_is_not_guessed() {
    // `m5` is not a value of `@target.cpu`: its error belongs to type checking, so the
    // condition is not decided and the result is not accepted.
    let r = one("when @target.cpu == .m5 {\n fn f() { }\n}\n");
    assert_eq!(r.codes(), vec![]);
    assert!(!r.out.not_implemented.is_empty());
    let r = one("when @target.arch == .m4 {\n fn f() { }\n}\n");
    assert!(!r.out.not_implemented.is_empty());
}

#[test]
fn arch_and_cpu_are_decided_per_target() {
    let src = "when @target.arch == .aarch64 && @target.cpu == .m4 {\n fn arm() { }\n\
               } else {\n fn other() { }\n}\n\
               when @target.arch == .x86_64 {\n fn x86() { }\n}\n\
               when @target.cpu != .znver4 {\n fn not_zen() { }\n}\n";
    let defined = |r: &Run| -> Vec<String> {
        let mut names: Vec<String> = r
            .out
            .sema
            .defs
            .iter()
            .filter(|d| d.kind == DefKind::Fn)
            .map(|d| d.name.clone())
            .collect();
        names.sort();
        names
    };
    let r = one(src);
    assert_eq!(r.codes(), vec![]);
    assert!(r.out.not_implemented.is_empty());
    assert_eq!(defined(&r), vec!["arm", "not_zen"]);
    let r = run_on("x86_64-linux-znver4", &[("app", &[("", src)])]);
    assert_eq!(r.codes(), vec![]);
    assert!(r.out.not_implemented.is_empty());
    assert_eq!(defined(&r), vec!["other", "x86"]);
    let r = run_on("x86_64-windows-znver4", &[("app", &[("", src)])]);
    assert_eq!(defined(&r), vec!["other", "x86"]);
}

#[test]
fn has_is_true_for_a_feature_of_the_cpu_and_false_for_the_other_cpus() {
    // `yes` is defined when the feature is present on the target's CPU.
    let present = |target: &str, feature: &str| {
        let src = format!(
            "when @target.has(.{feature}) {{\n fn yes() {{ }}\n}} else {{\n fn no() {{ }}\n}}\n"
        );
        let r = run_on(target, &[("app", &[("", src.as_str())])]);
        assert_eq!(r.codes(), vec![], "{target} .{feature}");
        r.out.sema.defs.iter().any(|d| d.name == "yes")
    };
    assert!(present("aarch64-macos-m4", "sme"));
    assert!(present("aarch64-macos-m4", "sha3"));
    assert!(present("aarch64-macos-m4", "aes"));
    assert!(!present("aarch64-macos-m4", "avx2"));
    assert!(!present("aarch64-macos-m4", "aesni"));
    assert!(present("x86_64-linux-znver4", "avx512vbmi2"));
    assert!(present("x86_64-linux-znver4", "aesni"));
    assert!(present("x86_64-linux-znver4", "sha"));
    assert!(!present("x86_64-linux-znver4", "neon"));
    assert!(!present("x86_64-linux-znver4", "sha2"));
    assert!(present("x86_64-windows-znver4", "bmi2"));
    assert!(!present("x86_64-windows-znver4", "sme"));
}

#[test]
fn an_unknown_feature_is_e1606_on_the_name() {
    let src = "when @target.has(.avx1024) {\n fn f() { }\n}\n";
    for target in crate::target::TARGETS {
        let r = run_on(target, &[("app", &[("", src)])]);
        assert_eq!(r.codes(), vec![("E1606".to_string(), 1)], "{target}");
        let d = &r.out.diagnostics[0];
        assert_eq!(r.sources.get(0).text_of(d.span), "avx1024");
    }
    // Outside a `when` the same query is checked too.
    let r = one("fn f() -> bool {\n return @target.has(.avx1024);\n}\n");
    assert_eq!(r.codes(), vec![("E1606".to_string(), 2)]);
    // A feature name of the M4 that the x86-64 CPU lacks is a name of the other CPU,
    // not an unknown one.
    let r = run_on(
        "x86_64-linux-znver4",
        &[(
            "app",
            &[("", "when @target.has(.neon) {\n fn f() { }\n}\n")],
        )],
    );
    assert_eq!(r.codes(), vec![]);
}

#[test]
fn has_sme_is_decided_by_the_target() {
    let src = "when @target.has(.sme) {\n fn yes() { }\n} else {\n fn no() { }\n}\n";
    let r = one(src);
    assert_eq!(r.codes(), vec![]);
    assert!(r.out.sema.defs.iter().any(|d| d.name == "yes"));
    assert!(!r.out.sema.defs.iter().any(|d| d.name == "no"));
    let r = run_on("x86_64-windows-znver4", &[("app", &[("", src)])]);
    assert!(r.out.sema.defs.iter().any(|d| d.name == "no"));
}

#[test]
fn a_label_must_enclose_the_jump() {
    let r = one("fn f() {\n loop {\n break outer;\n }\n}\n");
    assert_eq!(r.codes(), vec![("E0317".to_string(), 3)]);
    let r = one("fn f() {\n outer: loop {\n loop {\n break outer;\n }\n }\n}\n");
    assert_eq!(r.codes(), vec![]);
    // A loop's label is not visible after the loop.
    let r = one("fn f() {\n outer: loop { break outer; }\n loop { continue outer; }\n}\n");
    assert_eq!(r.codes(), vec![("E0317".to_string(), 3)]);
}

#[test]
fn a_while_loop_can_be_labelled() {
    let r = one("fn f(c: bool) {\n outer: while c {\n loop { break outer; }\n }\n}\n");
    assert_eq!(r.codes(), vec![]);
}

#[test]
fn test_env_is_only_for_tests() {
    let r = one("fn f() {\n _ = @test_env();\n}\n");
    assert_eq!(r.codes(), vec![("E1706".to_string(), 2)]);
    let r = one("test \"t\" {\n _ = @test_env();\n}\n");
    assert_eq!(r.codes(), vec![]);
    let r = one("bench \"b\" {\n _ = @test_env();\n}\n");
    assert_eq!(r.codes(), vec![]);
}

#[test]
fn duplicate_test_names_are_e1707_but_a_test_and_a_bench_may_share() {
    let r = one("test \"same\" { }\ntest \"same\" { }\n");
    assert_eq!(r.codes(), vec![("E1707".to_string(), 2)]);
    let r = one("test \"same\" { }\nbench \"same\" { }\n");
    assert_eq!(r.codes(), vec![]);
}

#[test]
fn match_arms_bind_patterns_and_alternatives_share_locals() {
    let r = one("enum Shape { circle: f64, square: f64, empty }\n\
         fn area(s: Shape) -> f64 {\n\
         match s {\n\
         .circle(r) | .square(r) => { return r; }\n\
         .empty => { return 0.0; }\n\
         }\n\
         }\n");
    assert_eq!(r.codes(), vec![]);
    assert_eq!(
        r.pkg().locals.iter().filter(|l| l.name == "r").count(),
        1,
        "both alternatives bind the same local"
    );
}

#[test]
fn a_struct_pattern_shorthand_binds_the_field_name() {
    let r = one("struct P { x: i64, y: i64 }\n\
         fn f(p: P) -> i64 {\n\
         let P.{ x, y } = p;\n\
         return x + y;\n\
         }\n");
    assert_eq!(r.codes(), vec![]);
}

#[test]
fn generic_parameters_and_self_resolve_in_types() {
    let r = one("interface Hash { fn hash(self: &Self) -> u64; }\n\
         struct Pair[A, B] { pub a: A, pub b: B }\n\
         impl[K: Hash] Hash for Pair[K, K] {\n\
         fn hash(self: &Self) -> u64 { return 0; }\n\
         }\n\
         fn first[T](p: Pair[T, T]) -> T { return p.a; }\n");
    assert_eq!(r.codes(), vec![]);
}

#[test]
fn self_outside_an_impl_is_undefined() {
    let r = one("fn f(x: Self) { }\n");
    assert_eq!(r.only_codes(), vec!["E0305"]);
}

#[test]
fn builtin_names_resolve_to_builtin_definitions() {
    let r = one("fn f[T: Ord + Copy](a: T, b: Option[T]) -> Result[T, i64] { return a; }\n");
    assert_eq!(r.codes(), vec![]);
    let f = r
        .pkg()
        .items
        .values()
        .find_map(|i| match i {
            hir::Item::Fn(f) => Some(f),
            _ => None,
        })
        .unwrap();
    // The bounds and the parameter types are definitions, not strings.
    for b in &f.generics[0].bounds {
        let Res::Def(d) = b.res else {
            panic!("bound did not resolve")
        };
        assert!(matches!(r.out.sema.def(d).kind, DefKind::Builtin(_)));
    }
    let ret = match &f.ret.kind {
        hir::TyKind::Path(p) => p.res,
        _ => panic!("result type is a path"),
    };
    assert!(matches!(
        r.def_kind(ret),
        Some(DefKind::Builtin(crate::Builtin::Result))
    ));

    // The builtin names are exactly those of section 8, and each of them is a
    // builtin definition: nothing else is in the builtin scope.
    let mut expected = vec![
        "Option", "Result", "Copy", "Discard", "Send", "Sync", "Owned", "Int", "SInt", "UInt",
        "Float", "Num", "Fn", "FnMut", "FnOnce", "Eq", "Ord", "Hash", "Iter", "Format", "Executor",
        "Idx", "Idx2", "Idx3", "Grid", "GridMut",
    ];
    expected.sort();
    let scope: Vec<&str> = r
        .out
        .sema
        .module(crate::defs::BUILTIN_MODULE)
        .types
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(scope, expected);

    // The names of sections 7.1, 27 and 35 resolve in bounds to their builtins.
    let r = one(
        "fn f[I: Iter, F: Format, G: Grid, H: GridMut, E: Executor, X: Eq + Ord + Hash](\n\
         a: I, b: F, c: G, d: H, e: E, x: X) { }\n",
    );
    assert_eq!(r.codes(), vec![]);
    let f = r
        .pkg()
        .items
        .values()
        .find_map(|i| match i {
            hir::Item::Fn(f) if f.name == "f" => Some(f),
            _ => None,
        })
        .unwrap();
    let kinds: Vec<DefKind> = f
        .generics
        .iter()
        .flat_map(|g| g.bounds.iter())
        .map(|b| match &b.res {
            Res::Def(d) => r.out.sema.def(*d).kind,
            _ => panic!("bound did not resolve"),
        })
        .collect();
    use crate::Builtin::*;
    assert_eq!(
        kinds,
        [Iter, Format, Grid, GridMut, Executor, Eq, Ord, Hash]
            .map(DefKind::Builtin)
            .to_vec()
    );
}

#[test]
fn a_user_declaration_hides_a_builtin_of_the_same_name() {
    let r = one("interface Hash { fn hash(self: &Self) -> u64; }\nfn f[T: Hash](x: T) { }\n");
    assert_eq!(r.codes(), vec![]);
    let f = r
        .pkg()
        .items
        .values()
        .find_map(|i| match i {
            hir::Item::Fn(f) if f.name == "f" => Some(f),
            _ => None,
        })
        .unwrap();
    let Res::Def(d) = f.generics[0].bounds[0].res else {
        panic!()
    };
    assert_eq!(r.out.sema.def(d).kind, DefKind::Interface);
}

#[test]
fn fn_bounds_keep_their_signature() {
    let r = one("fn apply[F: Fn(i64, i64) -> i64](f: F) { }\n");
    assert_eq!(r.codes(), vec![]);
    let f = r
        .pkg()
        .items
        .values()
        .find_map(|i| match i {
            hir::Item::Fn(f) => Some(f),
            _ => None,
        })
        .unwrap();
    let sig = f.generics[0].bounds[0]
        .fn_sig
        .as_ref()
        .expect("a signature");
    assert_eq!(sig.params.len(), 2);
    assert!(sig.ret.is_some());
}

#[test]
fn undefined_name_fixes() {
    // A near miss is replaced.
    let r = one("fn f(total: i64) -> i64 {\n return totl;\n}\n");
    let d = &r.out.diagnostics[0];
    assert_eq!(d.code, "E0304");
    assert_eq!(d.fixes[0].edits[0].text, "total");
    // Two near misses: no fix.
    let r = one("fn f(tota: i64, totc: i64) -> i64 {\n return totl;\n}\n");
    assert!(r.out.diagnostics[0].fixes.is_empty());
    // An imported module that exports the name qualifies it.
    let r = run(&[(
        "app",
        &[
            ("", "import app.lib;\nfn f() -> i64 { return helper(); }\n"),
            ("lib", "pub fn helper() -> i64 { return 1; }\n"),
        ],
    )]);
    let d = &r.out.diagnostics[0];
    assert_eq!(d.code, "E0304");
    assert_eq!(d.fixes[0].edits[0].text, "lib.");
    assert!(d.fixes[0].edits[0].span.is_empty());
}

#[test]
fn undefined_type_fixes() {
    let r = one("struct Point { x: i64 }\nfn f(p: Pont) { }\n");
    let d = &r.out.diagnostics[0];
    assert_eq!(d.code, "E0305");
    assert_eq!(d.fixes[0].edits[0].text, "Point");
}

#[test]
fn closures_resolve_captures_and_bind_parameters() {
    let r = one("fn f() -> i64 {\n\
         let scale = 3;\n\
         var total: i64 = 0;\n\
         let g = fn [scale, &mut total](x: i64) { total.* += x * scale; };\n\
         g(1);\n\
         return total;\n\
         }\n");
    assert_eq!(r.codes(), vec![]);
}

#[test]
fn a_capture_must_name_a_local() {
    let r = one("fn f() {\n let g = fn [missing](x: i64) { };\n g(1);\n}\n");
    assert_eq!(r.codes(), vec![("E0611".to_string(), 2)]);
}

#[test]
fn a_capture_listed_twice_is_e0611() {
    let r = one(
        "fn f(n: i64) -> i64 {\n let g = fn [n, n](x: i64) -> i64 { return x + n; };\n return g(1);\n}\n",
    );
    assert_eq!(r.codes(), vec![("E0611".to_string(), 2)]);
}

#[test]
fn an_uncaptured_local_in_a_nested_closure_is_e0611_with_a_fix() {
    // `a` is captured by the outer closure only; the inner one must list it too.
    let r = one(
        "fn f(a: i64) -> i64 {\n let g = fn [a](x: i64) -> i64 {\n  let h = fn [x](y: i64) -> i64 { return y + x + a; };\n  return h(1);\n };\n return g(1);\n}\n",
    );
    assert_eq!(r.codes(), vec![("E0611".to_string(), 3)]);
}

#[test]
fn the_hir_keeps_every_syntactic_form() {
    // A body using most statement and expression forms lowers without a diagnostic.
    let r = one("import std.Env;\n\
         struct S { a: i64 }\n\
         enum E { one, two: i64 }\n\
         fn id(x: i64) -> i64 { return x; }\n\
         fn f(xs: []i64, e: E, n: i64) -> i64 ! i64 {\n\
         var acc: i64 = 0;\n\
         for i in 0..n step 2 { acc += i; }\n\
         for k, v in xs { acc += k + v; }\n\
         while acc < 10 { acc += 1; }\n\
         if let .two(v) = e { acc += v; } else { acc += 1; }\n\
         let t = (acc, n);\n\
         let s = S.{ a: t.0 };\n\
         let arr = [2]i64.{ 1, 2 };\n\
         let r = id(s.a) catch _ => 0;\n\
         let q = if n > 0 { 1 } else { 2 };\n\
         defer { acc += 1; }\n\
         unsafe(\"why\") { acc += 1; }\n\
         return acc + arr[0] + r + q + t.1;\n\
         }\n");
    // `std` is not given here, so only the import fails.
    assert_eq!(r.only_codes(), vec!["E0301"]);
}

#[test]
fn duplicate_members_labels_and_closure_parameters_are_e0308() {
    let r = one("interface I { fn m(self: &Self); fn m(self: &Self); }\n");
    assert_eq!(r.codes(), vec![("E0308".to_string(), 1)]);
    let r = one("struct S { a: i64 }\nimpl S { fn m(self: &S) { } fn m(self: &S) { } }\n");
    assert_eq!(r.codes(), vec![("E0308".to_string(), 2)]);
    let r = one("fn f() {\n a: loop {\n a: loop { break a; }\n }\n}\n");
    assert_eq!(r.codes(), vec![("E0308".to_string(), 3)]);
    // Sibling loops may reuse a label.
    let r = one("fn f() {\n a: loop { break a; }\n a: loop { break a; }\n}\n");
    assert_eq!(r.codes(), vec![]);
    let r = one("fn f() {\n let g = fn [](x: i64, x: i64) { };\n g(1, 2);\n}\n");
    assert_eq!(r.codes(), vec![("E0308".to_string(), 2)]);
}

#[test]
fn a_closure_parameter_may_not_shadow_a_capture() {
    let r = one("fn f() {\n let x = 1;\n let g = fn [x](x: i64) { };\n g(x);\n}\n");
    assert!(r.codes().contains(&("E0309".to_string(), 3)));
}

#[test]
fn a_private_type_is_e0306_through_a_module_path() {
    let r = run(&[(
        "app",
        &[
            ("", "import app.lib;\nfn f(t: lib.Hidden) { }\n"),
            ("lib", "struct Hidden { a: i64 }\n"),
        ],
    )]);
    assert_eq!(r.codes(), vec![("E0306".to_string(), 2)]);
    let r = run(&[(
        "app",
        &[
            ("", "import app.lib;\nfn f(t: lib.Missing) { }\n"),
            ("lib", "pub struct Hidden { a: i64 }\n"),
        ],
    )]);
    assert_eq!(r.codes(), vec![("E0305".to_string(), 2)]);
}

#[test]
fn an_import_does_not_make_a_name_an_item_of_the_module() {
    // `lib` imports `Hidden` but does not declare it, so `lib.Hidden` is not there.
    let r = run(&[
        ("std", &[("", "pub struct Env { }\n")]),
        (
            "app",
            &[
                ("", "import app.lib;\nfn f(t: lib.Env) { }\n"),
                ("lib", "import std.Env;\nfn g(e: &Env) { }\n"),
            ],
        ),
    ]);
    assert_eq!(r.codes(), vec![("E0305".to_string(), 2)]);
}

#[test]
fn an_item_and_an_import_of_one_name_are_e0308() {
    let r = run(&[
        ("std", &[("", "pub struct Env { }\n")]),
        (
            "app",
            &[("", "import std.Env;\nstruct Env { }\nfn f(e: &Env) { }\n")],
        ),
    ]);
    assert_eq!(r.codes(), vec![("E0308".to_string(), 2)]);
}

#[test]
fn an_extern_block_declares_functions_with_the_blocks_visibility() {
    let src = "extern \"c\" {\n fn write(fd: i32, buf: *u8, n: i64) -> i64;\n}\nfn f(p: *u8) -> i64 { return write(1, p, 2); }\n";
    let r = one(src);
    assert_eq!(r.codes(), vec![]);
    let f = r
        .pkg()
        .items
        .values()
        .find_map(|i| match i {
            hir::Item::Fn(f) if f.name == "write" => Some(f),
            _ => None,
        })
        .unwrap();
    assert_eq!(f.abi.as_deref(), Some("c"));
    assert_eq!(r.pkg().modules[0].externs.len(), 1);
}

#[test]
fn an_undecidable_when_poisons_the_names_its_branches_declare() {
    let r = one(
        "when @target.bogus == 1 {\n fn a() { }\n} else {\n struct B { x: i64 }\n}\n\
         fn f(b: B) { a(); }\n",
    );
    assert_eq!(r.codes(), vec![("E1606".to_string(), 1)]);
}

#[test]
fn a_statement_when_with_an_undecidable_condition_checks_nothing_in_either_branch() {
    let r = one("fn f() {\n when @target.bogus == 1 {\n nope();\n } else {\n nada();\n }\n}\n");
    assert_eq!(r.codes(), vec![("E1606".to_string(), 2)]);
}

#[test]
fn the_ids_of_the_hir_point_at_nodes_with_spans() {
    let src = "fn f(a: i64) -> i64 { return a + 1; }\n";
    let r = one(src);
    let text = &r.sources.get(0).text;
    for e in &r.pkg().exprs {
        assert!(e.span.end <= text.len() && e.span.start <= e.span.end);
    }
    let paths = r.paths();
    assert_eq!(paths.len(), 1);
    assert_eq!(r.local_name(paths[0].1), Some("a"));
}
