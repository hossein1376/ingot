//! Pipeline phase 7: type-checking the bodies of functions, tests, constants and
//! statics against the signatures of phase 6 (pipeline sections 1, 3 and 4.4).
//!
//! A body is checked on its own, by one [`Checker`]: expressions are typed top-down
//! with an optional *hint*, the type the context expects (an annotation, a parameter,
//! a return type, a field, the other operand of a binary operator). Only an untyped
//! literal, an enum or struct shorthand and `@zero` use the hint; everything else
//! has a type of its own and the caller compares it with what it expects. A hint never
//! flows from a later use (pipeline section 4.4). The generic parameters of the
//! function are opaque types with exactly the properties their bounds grant, and a
//! generic body is checked once (section 8).
//!
//! The result is a [`TypedBody`](crate::typed::TypedBody) per body. A failed
//! expression has the poison type and nothing downstream reports about it. Rules that
//! belong to the next step (errors and cleanup, `unsafe`, concurrency, kernels, constant
//! evaluation of calls) are recorded as `not implemented` and never accepted silently.

mod builtin;
mod call;
mod exhaust;
mod expr;
mod ops;
mod pat;
mod place;
mod stmt;

use crate::defs::{DefId, DefKind, ModuleId};
use crate::hir::{self, BlockId, ExprId, LocalId, StmtId};
use crate::lowering::Cx;
use crate::sigs::*;
use crate::ty::*;
use crate::typed::*;
use ingot_diag::diagnostic::{Diagnostic, Edit};
use ingot_diag::source::Span;
use std::collections::{BTreeMap, BTreeSet};

/// What kind of function the code being checked belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CtxKind {
    Fn,
    Closure,
    Test,
    /// A `const` or `static` initializer.
    Item,
}

/// The function (or closure) whose `return` and `fail` the checker is looking at.
#[derive(Clone, Copy, Debug)]
pub(crate) struct FnCtx {
    pub ret: TypeId,
    pub error: Option<TypeId>,
    pub kind: CtxKind,
    /// The length of the loop stack when the function was entered: `break` and
    /// `continue` see only the loops above it.
    pub loop_base: usize,
    /// The value of `defer_depth` when the function was entered.
    pub defer_base: u32,
}

#[derive(Clone, Debug)]
pub(crate) struct LoopCtx {
    pub stmt: StmtId,
    pub has_break: bool,
    pub par: bool,
    /// A `for` over an array value of linear elements: it cannot be left early (E0613).
    pub linear_consume: bool,
    pub defer_depth: u32,
}

pub(crate) struct Checker<'c, 'a> {
    pub cx: &'c mut Cx<'a>,
    pub hir: &'a hir::Package,
    pub pkg: usize,
    pub file: usize,
    pub module: ModuleId,
    pub owner: DefId,
    pub out: TypedBody,
    pub fns: Vec<FnCtx>,
    pub loops: Vec<LoopCtx>,
    /// Locals that are assigned, exclusively borrowed or mutated after initialization
    /// (E0312).
    pub mutated: BTreeSet<LocalId>,
    /// Each `var` statement with the locals its pattern binds.
    pub var_stmts: Vec<(StmtId, Vec<LocalId>)>,
    /// The `let`/`var` statement that binds a local (for the fix of E0606 and E0610).
    pub let_of: BTreeMap<LocalId, StmtId>,
    pub defer_depth: u32,
    pub par_depth: u32,
    /// Patterns that were reported: exhaustiveness says nothing about a `match` that
    /// has one.
    pub failed_pats: BTreeSet<crate::hir::PatId>,
    pub errors: u32,
    /// An expression of the body was poisoned or a rule is not implemented.
    pub poisoned: bool,
    /// The generic parameter definitions in scope (the function's and its impl's).
    pub in_item_init: bool,
}

impl<'c, 'a> Checker<'c, 'a> {
    // ------------------------------------------------------------------ setup

    pub(crate) fn new(
        cx: &'c mut Cx<'a>,
        owner: DefId,
        kind: BodyKind,
        root: BodyRoot,
        ret: TypeId,
        error: Option<TypeId>,
    ) -> Checker<'c, 'a> {
        let pkg = cx.pkg_of(owner);
        let file = cx.file_of(owner);
        let hir: &'a hir::Package = &cx.pkgs[pkg];
        let module = cx.sema.def(owner).module;
        cx.enter(owner);
        let ctx_kind = match kind {
            BodyKind::Fn => CtxKind::Fn,
            BodyKind::Test => CtxKind::Test,
            BodyKind::Const | BodyKind::Static => CtxKind::Item,
        };
        Checker {
            cx,
            hir,
            pkg,
            file,
            module,
            owner,
            out: TypedBody {
                owner,
                kind,
                package: pkg,
                root,
                params: Vec::new(),
                ret,
                error,
                exprs: BTreeMap::new(),
                pats: BTreeMap::new(),
                locals: BTreeMap::new(),
                stmts: BTreeMap::new(),
                closures: BTreeMap::new(),
                failed: false,
            },
            fns: vec![FnCtx {
                ret,
                error,
                kind: ctx_kind,
                loop_base: 0,
                defer_base: 0,
            }],
            loops: Vec::new(),
            mutated: BTreeSet::new(),
            var_stmts: Vec::new(),
            let_of: BTreeMap::new(),
            defer_depth: 0,
            par_depth: 0,
            failed_pats: BTreeSet::new(),
            errors: 0,
            poisoned: false,
            in_item_init: matches!(kind, BodyKind::Const | BodyKind::Static),
        }
    }

    pub(crate) fn finish(mut self) -> TypedBody {
        let poison_seen = self
            .out
            .exprs
            .values()
            .any(|i| self.cx.d.types.has_poison(i.ty))
            || self
                .out
                .locals
                .values()
                .any(|l| self.cx.d.types.has_poison(l.ty));
        // Which variables are mutated is known only for a body that was understood
        // completely.
        if self.errors == 0 && !self.poisoned && !poison_seen {
            self.report_unmutated_vars();
        }
        self.out.failed = self.errors > 0 || self.poisoned || poison_seen;
        // Obligations of the types written in the body (E0801, E0510).
        self.cx.check_obligations();
        self.out
    }

    // ------------------------------------------------------------ diagnostics

    /// Report an error in the file of the body. Any error marks the body failed.
    pub(crate) fn err(
        &mut self,
        code: &'static str,
        message: impl Into<String>,
        span: Span,
    ) -> &mut Diagnostic {
        self.errors += 1;
        self.cx.err(code, message, span)
    }

    /// Record a part of the language that is not implemented. The body is skipped by
    /// later phases.
    pub(crate) fn ni(&mut self, what: impl Into<String>) {
        self.poisoned = true;
        self.cx.ni(what);
    }

    pub(crate) fn file_path(&self) -> String {
        self.cx.sources.get(self.file).path.clone()
    }

    pub(crate) fn text(&self, span: Span) -> &str {
        self.cx.sources.get(self.file).text_of(span)
    }

    /// An edit in the file of the body.
    pub(crate) fn edit(&self, span: Span, text: impl Into<String>) -> Edit {
        Edit {
            file: self.file_path(),
            span,
            text: text.into(),
        }
    }

    // ------------------------------------------------------------------ types

    pub(crate) fn kind(&self, t: TypeId) -> TyKind {
        self.cx.d.types.kind(t).clone()
    }

    pub(crate) fn mk(&mut self, k: TyKind) -> TypeId {
        self.cx.d.types.intern(k)
    }

    pub(crate) fn name(&self, t: TypeId) -> String {
        self.cx.ty_name(t)
    }

    pub(crate) fn poison(&self) -> TypeId {
        self.cx.d.types.poison
    }

    pub(crate) fn void(&self) -> TypeId {
        self.cx.d.types.void
    }

    pub(crate) fn never(&self) -> TypeId {
        self.cx.d.types.never
    }

    pub(crate) fn bool_(&self) -> TypeId {
        self.cx.d.types.bool_
    }

    pub(crate) fn i64_(&self) -> TypeId {
        self.cx.d.types.i64_
    }

    pub(crate) fn f64_(&mut self) -> TypeId {
        self.mk(TyKind::Float(FloatTy::F64))
    }

    pub(crate) fn is_poison(&self, t: TypeId) -> bool {
        self.cx.d.types.has_poison(t)
    }

    pub(crate) fn is_never(&self, t: TypeId) -> bool {
        t == self.cx.d.types.never
    }

    pub(crate) fn ref_to(&mut self, m: Mutability, t: TypeId) -> TypeId {
        self.mk(TyKind::Ref(m, t))
    }

    pub(crate) fn int_of(&self, t: TypeId) -> Option<IntTy> {
        match self.cx.d.types.kind(t) {
            TyKind::Int(i) => Some(*i),
            _ => None,
        }
    }

    pub(crate) fn is_result(&self, t: TypeId) -> bool {
        matches!(self.cx.d.types.kind(t), TyKind::Result(..))
    }

    /// Strip every borrow from the front of `t`: the type under them, and how many
    /// borrows there were.
    pub(crate) fn strip_refs(&self, mut t: TypeId) -> (TypeId, u32) {
        let mut n = 0;
        while let TyKind::Ref(_, inner) = self.cx.d.types.kind(t) {
            t = *inner;
            n += 1;
        }
        (t, n)
    }

    /// Whether a value of type `found` may stand where `expected` is wanted. `never`
    /// stands for anything; poison on either side was reported already.
    pub(crate) fn compatible(&self, found: TypeId, expected: TypeId) -> bool {
        found == expected
            || self.is_never(found)
            || self.cx.d.types.has_poison(found)
            || self.cx.d.types.has_poison(expected)
    }

    /// Whether a value of `t` is `Copy`. A type the specification does not define it
    /// for is not.
    pub(crate) fn is_copy(&mut self, t: TypeId) -> bool {
        self.cx.d.props(t).copy == Tri::Yes
    }

    /// Type an expression whose context failed, so that its own errors are reported but
    /// nothing is said about the type it was supposed to have (a shorthand, `@zero` and a
    /// literal take the poison type as their context).
    pub(crate) fn blind_expr(&mut self, e: ExprId) -> TypeId {
        let poison = self.poison();
        self.expr(e, Some(poison))
    }

    pub(crate) fn span_of(&self, e: ExprId) -> Span {
        self.hir.expr(e).span
    }

    /// The expression without parentheses around it.
    pub(crate) fn unparen(&self, mut e: ExprId) -> ExprId {
        while let hir::ExprKind::Paren(inner) = &self.hir.expr(e).kind {
            e = *inner;
        }
        e
    }

    // ----------------------------------------------------------------- records

    pub(crate) fn set(&mut self, e: ExprId, ty: TypeId, res: ExprRes) -> TypeId {
        self.out.exprs.insert(
            e,
            ExprInfo {
                ty,
                res,
                coerce: None,
            },
        );
        ty
    }

    pub(crate) fn ty_of(&self, e: ExprId) -> TypeId {
        self.out
            .exprs
            .get(&e)
            .map(|i| i.ty)
            .unwrap_or(self.cx.d.types.poison)
    }

    pub(crate) fn bind_local(&mut self, l: LocalId, ty: TypeId, mode: BindMode) {
        self.out.locals.insert(l, LocalInfo { ty, mode });
    }

    pub(crate) fn local_ty(&self, l: LocalId) -> TypeId {
        self.out
            .locals
            .get(&l)
            .map(|i| i.ty)
            .unwrap_or(self.cx.d.types.poison)
    }

    pub(crate) fn fnctx(&self) -> FnCtx {
        *self.fns.last().expect("a function context")
    }

    // -------------------------------------------------------------- std items

    /// A type declared in a module of `std`, by module path and name.
    pub(crate) fn std_type(&self, module: &str, name: &str) -> Option<DefId> {
        let m = self.cx.sema.module_paths.get(module)?;
        self.cx.sema.module(*m).types.get(name).copied()
    }

    /// Lower a type written in the body.
    pub(crate) fn lower(&mut self, t: &hir::Ty) -> TypeId {
        self.cx.lower_ty(t)
    }

    /// The type a path names (a struct, enum, alias, generic parameter, `Self`, scalar).
    pub(crate) fn lower_path(&mut self, p: &hir::PathRef) -> TypeId {
        let ty = hir::Ty {
            kind: hir::TyKind::Path(p.clone()),
            span: p.span,
        };
        self.cx.lower_ty(&ty)
    }

    /// The generic arguments of `def`'s generics from a substitution, in order. A
    /// parameter missing from `s` becomes the poison type.
    pub(crate) fn args_for(&mut self, owner: DefId, s: &Subst) -> Vec<GenericArg> {
        let generics = self.cx.d.generics_of(owner).to_vec();
        let poison = self.poison();
        generics
            .iter()
            .map(|g| {
                s.get(g.def).cloned().unwrap_or(if g.is_const {
                    GenericArg::Const(ConstArg::Poison)
                } else {
                    GenericArg::Ty(poison)
                })
            })
            .collect()
    }

    // ----------------------------------------------------- E0312 var_never_mutated

    fn report_unmutated_vars(&mut self) {
        let vars = std::mem::take(&mut self.var_stmts);
        for (stmt, locals) in vars {
            if locals.is_empty() || locals.iter().any(|l| self.mutated.contains(l)) {
                continue;
            }
            let span = self.hir.stmt(stmt).span;
            let kw = Span::new(span.start, span.start + 3);
            let edit = self.edit(kw, "let");
            self.err(
                "E0312",
                "this `var` is never assigned or exclusively borrowed after its initialization",
                kw,
            )
            .fix("replace `var` with `let`", edit);
        }
    }

    // ------------------------------------------------------------------ names

    /// The signature of a function-like definition.
    pub(crate) fn fn_sig(&mut self, def: DefId) -> Option<FnSig> {
        self.cx.ensure_sig(def);
        self.cx.d.fn_sig(def).cloned()
    }

    pub(crate) fn def_kind(&self, d: DefId) -> DefKind {
        self.cx.sema.def(d).kind
    }
}

// ----------------------------------------------------------------- the driver

impl Cx<'_> {
    /// Phase 7 for the package being checked: the typed body of every function with a
    /// body, test, constant and static. Diagnostics and `not implemented` entries go
    /// to `self.d`.
    pub fn check_bodies(&mut self) -> Bodies {
        let mut bodies = Bodies::default();
        let pkgs: &[hir::Package] = self.pkgs;
        let Some(package) = pkgs.get(self.reported_pkg) else {
            return bodies;
        };
        for (&def, item) in &package.items {
            let typed = match item {
                hir::Item::Fn(f) => match f.body {
                    Some(b) => self.check_fn_body(def, f, b),
                    None => None,
                },
                hir::Item::Test(t) => self.check_test_body(def, t),
                hir::Item::Const(c) => self.check_const_body(def, c),
                hir::Item::Static(s) => self.check_static_body(def, s),
                _ => None,
            };
            if let Some(t) = typed {
                bodies.bodies.insert(def, t);
            }
        }
        self.check_obligations();
        bodies
    }

    fn check_fn_body(&mut self, def: DefId, f: &hir::FnItem, body: BlockId) -> Option<TypedBody> {
        self.ensure_sig(def);
        let sig = self.d.fn_sig(def)?.clone();
        let error = sig.error;
        let mut c = Checker::new(
            self,
            def,
            BodyKind::Fn,
            BodyRoot::Block(body),
            sig.ret,
            error,
        );
        if f.mods.kernel {
            c.ni("kernel shape rules E1510 to E1519 of a `kernel fn` body (S4)");
        }
        if f.mods.waitable {
            c.ni("the `waitable fn` rule E1410 (S4)");
        }
        if f.mods.unsafe_ {
            c.ni("the rules of an `unsafe fn` body, E1301 and E1304 (S4)");
        }
        for (p, s) in f.params.iter().zip(&sig.params) {
            let mode = if c.is_copy(s.ty) {
                BindMode::Copy
            } else {
                BindMode::Move
            };
            c.bind_local(p.local, s.ty, mode);
            c.out.params.push(p.local);
        }
        let completes = c.block(body);
        if completes && sig.ret != c.void() && !c.is_never(sig.ret) && !c.is_poison(sig.ret) {
            let b = c.hir.block(body).span;
            let at = Span::new(b.end.saturating_sub(1), b.end);
            c.err(
                "E0412",
                "a path through this function reaches the end of its body without returning a value",
                at,
            );
        }
        Some(c.finish())
    }

    fn check_test_body(&mut self, def: DefId, t: &hir::TestItem) -> Option<TypedBody> {
        let void = self.d.types.void;
        let mut c = Checker::new(
            self,
            def,
            BodyKind::Test,
            BodyRoot::Block(t.body),
            void,
            None,
        );
        c.block(t.body);
        Some(c.finish())
    }

    fn check_const_body(&mut self, def: DefId, item: &hir::ConstItem) -> Option<TypedBody> {
        self.ensure_sig(def);
        let ty = match self.d.items.get(&def) {
            Some(ItemSig::Const(c)) => c.ty,
            _ => return None,
        };
        let mut c = Checker::new(
            self,
            def,
            BodyKind::Const,
            BodyRoot::Expr(item.value),
            ty,
            None,
        );
        if !c.is_poison(ty) {
            c.expect(item.value, ty);
        }
        Some(c.finish())
    }

    fn check_static_body(&mut self, def: DefId, item: &hir::StaticItem) -> Option<TypedBody> {
        self.ensure_sig(def);
        let ty = match self.d.items.get(&def) {
            Some(ItemSig::Static(s)) => s.ty,
            _ => return None,
        };
        let mut c = Checker::new(
            self,
            def,
            BodyKind::Static,
            BodyRoot::Expr(item.value),
            ty,
            None,
        );
        if !c.is_poison(ty) {
            c.expect(item.value, ty);
        }
        Some(c.finish())
    }
}
