//! Calls (spec sections 4.4, 5.6 and 8): functions, methods, interface functions,
//! variant constructors and calls of values; method lookup and the receiver borrow;
//! inference of type arguments from the argument types; closures.

use super::{Checker, CtxKind, FnCtx};
use crate::defs::{DefId, DefKind};
use crate::hir::{self, ExprId, ExprKind, Res};
use crate::sigs::*;
use crate::traits::{MethodCandidate, MethodSource};
use crate::ty::*;
use crate::typed::*;
use ingot_diag::source::Span;
use std::collections::{BTreeMap, BTreeSet};

/// The function a call resolved to, before its arguments are looked at.
pub(crate) struct CallTarget {
    pub def: DefId,
    pub sig: FnSig,
    /// What is already known: the impl's generic arguments, `Self`, the interface's.
    pub subst: Subst,
    /// Generic parameters that are not the function's own but still have to be
    /// determined by the arguments (impl parameters the target type did not fix).
    pub extra_binders: Vec<DefId>,
    /// Parameters supplied by the receiver of a method call.
    pub skip: usize,
    pub explicit: Vec<TypeId>,
    pub explicit_span: Option<Span>,
}

pub(crate) struct CallSite<'x> {
    pub args: &'x [ExprId],
    pub args_span: Span,
    pub name_span: Span,
    /// Arguments whose type is already known (checked before the call was resolved).
    pub known: BTreeMap<usize, TypeId>,
}

pub(crate) struct CallResult {
    /// The type of the call expression: `R`, or `R ! E` for a fallible function.
    pub ty: TypeId,
    pub fn_args: Vec<GenericArg>,
    pub subst: Subst,
}

impl Checker<'_, '_> {
    // ------------------------------------------------------------------- entry

    pub(crate) fn call(
        &mut self,
        e: ExprId,
        callee: ExprId,
        args: &[ExprId],
        args_span: Span,
        hint: Option<TypeId>,
    ) -> (TypeId, ExprRes) {
        let _ = hint;
        let hir = self.hir;
        match &hir.expr(callee).kind {
            ExprKind::Field {
                base,
                name,
                name_span,
            } => {
                if self.is_target_builtin(*base) && name == "has" {
                    // `@target.has(.feature)`: an unknown feature is E1606 (name resolution).
                    return (self.bool_(), ExprRes::None);
                }
                if let Some(iface) = self.interface_of_path(*base) {
                    return self
                        .interface_call(e, callee, iface, name, *name_span, args, args_span);
                }
                if let Some(t) = self.type_of_path_expr(*base) {
                    return self.type_call(e, callee, t, name, *name_span, args, args_span);
                }
                self.method_call(e, callee, *base, name, *name_span, args, args_span)
            }
            ExprKind::Path(p) => match p.res {
                Res::Poison => {
                    self.blind_args(args);
                    (self.poison(), ExprRes::None)
                }
                Res::Def(d) if matches!(self.def_kind(d), DefKind::Fn | DefKind::ExternFn) => {
                    let name_span = p.segments.last().map(|(_, s)| *s).unwrap_or(p.span);
                    self.fn_call(e, callee, d, Vec::new(), None, args, args_span, name_span)
                }
                Res::Def(d)
                    if !matches!(
                        self.def_kind(d),
                        DefKind::Const | DefKind::Static | DefKind::GenericParam { is_const: true }
                    ) =>
                {
                    let span = self.span_of(callee);
                    let name = p
                        .segments
                        .last()
                        .map(|(n, _)| n.clone())
                        .unwrap_or_default();
                    self.err(
                        "E0411",
                        format!("`{name}` is not a function, closure or variant constructor"),
                        span,
                    );
                    self.blind_args(args);
                    (self.poison(), ExprRes::None)
                }
                _ => self.value_call(e, callee, args, args_span),
            },
            ExprKind::TypeApply { base, args: tys } => {
                let b = self.unparen(*base);
                if let ExprKind::Path(p) = &hir.expr(b).kind
                    && let Res::Def(d) = p.res
                    && matches!(self.def_kind(d), DefKind::Fn | DefKind::ExternFn)
                {
                    let lowered: Vec<TypeId> = tys.iter().map(|t| self.lower(t)).collect();
                    if lowered.iter().any(|t| self.is_poison(*t)) {
                        self.blind_args(args);
                        return (self.poison(), ExprRes::None);
                    }
                    let name_span = p.segments.last().map(|(_, s)| *s).unwrap_or(p.span);
                    let span = self.span_of(callee);
                    return self.fn_call(
                        e,
                        callee,
                        d,
                        lowered,
                        Some(span),
                        args,
                        args_span,
                        name_span,
                    );
                }
                self.ni("explicit type arguments `.[..]` on something other than a function name");
                self.blind_args(args);
                (self.poison(), ExprRes::None)
            }
            _ => self.value_call(e, callee, args, args_span),
        }
    }

    /// Type the arguments of a call that failed, so that their own errors are reported.
    pub(crate) fn blind_args(&mut self, args: &[ExprId]) {
        for a in args {
            self.blind_expr(*a);
        }
    }

    /// The interface a path names, when it names one (`Hash.hash(..)`).
    fn interface_of_path(&self, base: ExprId) -> Option<DefId> {
        let b = self.unparen(base);
        match &self.hir.expr(b).kind {
            ExprKind::Path(p) => match p.res {
                Res::Def(d) if self.def_kind(d) == DefKind::Interface => Some(d),
                _ => None,
            },
            _ => None,
        }
    }

    // --------------------------------------------------------------- functions

    #[allow(clippy::too_many_arguments)]
    fn fn_call(
        &mut self,
        e: ExprId,
        callee: ExprId,
        d: DefId,
        explicit: Vec<TypeId>,
        explicit_span: Option<Span>,
        args: &[ExprId],
        args_span: Span,
        name_span: Span,
    ) -> (TypeId, ExprRes) {
        let _ = e;
        let Some(sig) = self.fn_sig(d) else {
            self.blind_args(args);
            return (self.poison(), ExprRes::None);
        };
        if sig.mods.kernel {
            self.ni("E1515 not_a_kernel: a direct call of a `kernel fn` (S4b)");
        }
        self.note_unsafe_callee(&sig, self.span_of(e));
        let target = CallTarget {
            def: d,
            sig: sig.clone(),
            subst: Subst::new(),
            extra_binders: Vec::new(),
            skip: 0,
            explicit,
            explicit_span,
        };
        let site = CallSite {
            args,
            args_span,
            name_span,
            known: BTreeMap::new(),
        };
        let Some(r) = self.apply_call(&target, &site) else {
            return (self.poison(), ExprRes::None);
        };
        let fnref = FnRef {
            def: d,
            args: r.fn_args.clone(),
            source: FnSource::Free,
        };
        let sigty = self.callee_type(&sig, &r.subst);
        self.set(callee, sigty, ExprRes::Fn(fnref.clone()));
        (
            r.ty,
            ExprRes::Call(Box::new(CallInfo {
                callee: Callee::Fn(fnref),
                receiver: None,
            })),
        )
    }

    /// The type of the callee expression: the function pointer type of its signature.
    fn callee_type(&mut self, sig: &FnSig, s: &Subst) -> TypeId {
        let params: Vec<TypeId> = sig
            .params
            .iter()
            .map(|p| self.cx.d.instantiate(p.ty, s))
            .collect();
        let ret = self.cx.d.instantiate(sig.ret, s);
        let error = sig.error.map(|t| self.cx.d.instantiate(t, s));
        self.mk(TyKind::FnPtr(FnPtrSig {
            ext: sig.mods.export || self.def_kind(sig.def) == DefKind::ExternFn,
            params,
            ret,
            error,
        }))
    }

    // ------------------------------------------------------ the call machinery

    fn mentions(&self, t: TypeId, set: &BTreeSet<DefId>) -> bool {
        let mut seen = Vec::new();
        self.mentions_in(t, set, &mut seen)
    }

    fn mentions_in(&self, t: TypeId, set: &BTreeSet<DefId>, seen: &mut Vec<TypeId>) -> bool {
        if seen.contains(&t) {
            return false;
        }
        seen.push(t);
        let consts = |a: &[GenericArg]| {
            a.iter().any(|g| match g {
                GenericArg::Const(ConstArg::Param(p)) => set.contains(p),
                _ => false,
            })
        };
        match self.cx.d.types.kind(t) {
            TyKind::Param(p) if set.contains(p) => return true,
            TyKind::Array(_, ConstArg::Param(p)) if set.contains(p) => return true,
            TyKind::Adt(_, a) | TyKind::Dyn(_, _, a) if consts(a) => return true,
            _ => {}
        }
        self.cx
            .d
            .types
            .children(t)
            .into_iter()
            .any(|c| self.mentions_in(c, set, seen))
    }

    /// Check the arguments of a call against a resolved function, inferring its type
    /// arguments from the argument types alone (section 5.6). Returns `None` after
    /// reporting.
    pub(crate) fn apply_call(&mut self, t: &CallTarget, site: &CallSite<'_>) -> Option<CallResult> {
        let generics = self.cx.d.generics_of(t.def).to_vec();
        let mut binders: Vec<DefId> = generics.iter().map(|g| g.def).collect();
        binders.extend(t.extra_binders.iter().copied());
        let mut s = t.subst.clone();
        let args = site.args;
        let sig = &t.sig;
        let fail = |this: &mut Self| {
            for (i, a) in args.iter().enumerate() {
                if !site.known.contains_key(&i) {
                    this.blind_expr(*a);
                }
            }
            None
        };
        if !t.explicit.is_empty() {
            if t.explicit.len() != generics.len() {
                let span = t.explicit_span.unwrap_or(site.name_span);
                self.err(
                    "E0804",
                    format!(
                        "`{}` takes {} generic argument(s) but {} {} given",
                        sig.name,
                        generics.len(),
                        t.explicit.len(),
                        if t.explicit.len() == 1 { "was" } else { "were" }
                    ),
                    span,
                );
                return fail(self);
            }
            for (g, ty) in generics.iter().zip(&t.explicit) {
                if g.is_const {
                    self.ni("an explicit argument for a constant generic parameter of a call");
                    return fail(self);
                }
                s.insert(g.def, GenericArg::Ty(*ty));
            }
        }
        let params: Vec<ParamSig> = sig.params[t.skip.min(sig.params.len())..].to_vec();
        let variadic_ok = sig.variadic && args.len() >= params.len();
        if args.len() != params.len() && !variadic_ok {
            let dpath = self.cx.sources.get(self.cx.file_of(t.def)).path.clone();
            self.err(
                "E0410",
                format!(
                    "`{}` takes {} argument(s) but {} {} given",
                    sig.name,
                    params.len(),
                    args.len(),
                    if args.len() == 1 { "was" } else { "were" }
                ),
                site.args_span,
            )
            .relate_in("callee", &dpath, sig.name_span);
            return fail(self);
        }

        let mut any_poison = false;
        let mut deferred: Vec<usize> = Vec::new();
        // The first argument that mentions each binder, for the span of E0801.
        let mut first_mention: BTreeMap<DefId, usize> = BTreeMap::new();
        for (i, p) in params.iter().enumerate() {
            for b in &binders {
                let one: BTreeSet<DefId> = std::iter::once(*b).collect();
                if self.mentions(p.ty, &one) {
                    first_mention.entry(*b).or_insert(i);
                }
            }
        }
        // Pass 1: arguments with a type of their own.
        for (i, p) in params.iter().enumerate() {
            let arg = args[i];
            let unresolved: BTreeSet<DefId> = binders
                .iter()
                .copied()
                .filter(|b| s.get(*b).is_none())
                .collect();
            let pty = self.cx.d.instantiate(p.ty, &s);
            let open = self.mentions(pty, &unresolved);
            if let Some(&known) = site.known.get(&i) {
                if open {
                    let list: Vec<DefId> = unresolved.iter().copied().collect();
                    let mut trial = s.clone();
                    if self.cx.d.match_ty(pty, known, &list, &mut trial) {
                        s = trial;
                    } else {
                        self.mismatch(arg, known, pty);
                    }
                } else if !self.compatible(known, pty) {
                    self.mismatch(arg, known, pty);
                }
                continue;
            }
            if !open {
                let at = self.expect(arg, pty);
                any_poison |= self.is_poison(at);
                continue;
            }
            if self.untyped(arg) {
                deferred.push(i);
                continue;
            }
            let at = self.expr(arg, None);
            if self.is_poison(at) {
                any_poison = true;
                continue;
            }
            if self.is_never(at) {
                continue;
            }
            let list: Vec<DefId> = unresolved.iter().copied().collect();
            let mut trial = s.clone();
            if self.cx.d.match_ty(pty, at, &list, &mut trial) {
                s = trial;
            } else {
                // Report against the parameter type with what is known so far.
                self.mismatch(arg, at, pty);
                any_poison = true;
            }
        }
        // Pass 2: untyped literals and shorthands, now that the other arguments have
        // fixed what they can; a literal that decides nothing takes its default type.
        for i in deferred {
            let arg = args[i];
            let unresolved: BTreeSet<DefId> = binders
                .iter()
                .copied()
                .filter(|b| s.get(*b).is_none())
                .collect();
            let pty = self.cx.d.instantiate(params[i].ty, &s);
            if self.mentions(pty, &unresolved) {
                let at = self.expr(arg, None);
                if self.is_poison(at) {
                    any_poison = true;
                    continue;
                }
                let list: Vec<DefId> = unresolved.iter().copied().collect();
                let mut trial = s.clone();
                if self.cx.d.match_ty(pty, at, &list, &mut trial) {
                    s = trial;
                } else {
                    self.mismatch(arg, at, pty);
                    any_poison = true;
                }
            } else {
                let at = self.expect(arg, pty);
                any_poison |= self.is_poison(at);
            }
        }
        // The signature of a closure or function value fixes the parameters that only
        // appear in its `Fn` bound (`F: Fn(T) -> U` fixes `U`).
        if !any_poison {
            loop {
                let mut progress = false;
                for g in &generics {
                    let Some(GenericArg::Ty(arg_ty)) = s.get(g.def).cloned() else {
                        continue;
                    };
                    let Some(actual) = self.fn_sig_of(arg_ty) else {
                        continue;
                    };
                    for b in &g.bounds {
                        let Bound::Fn(_, bsig) = b else { continue };
                        let unresolved: Vec<DefId> = binders
                            .iter()
                            .copied()
                            .filter(|d| s.get(*d).is_none())
                            .collect();
                        if unresolved.is_empty() {
                            continue;
                        }
                        let want = self.cx.d.subst_sig(bsig, &s);
                        let want = self.mk(TyKind::FnPtr(want));
                        let have = self.mk(TyKind::FnPtr(actual.clone()));
                        let mut trial = s.clone();
                        if self.cx.d.match_ty(want, have, &unresolved, &mut trial)
                            && trial.map.len() > s.map.len()
                        {
                            s = trial;
                            progress = true;
                        }
                    }
                }
                if !progress {
                    break;
                }
            }
        }
        // Variadic arguments (E1602).
        for &a in &args[params.len().min(args.len())..] {
            let at = self.expr(a, None);
            self.variadic_argument(a, at);
        }
        // Every type parameter must be determined by the arguments (E0803).
        let missing: Vec<DefId> = binders
            .iter()
            .copied()
            .filter(|b| s.get(*b).is_none())
            .collect();
        if !missing.is_empty() && !any_poison {
            let names: Vec<String> = missing
                .iter()
                .map(|d| self.cx.sema.def(*d).name.clone())
                .collect();
            self.err(
                "E0803",
                format!(
                    "the type argument(s) {} of `{}` are not determined by the argument types; \
                     write them with `.[..]`",
                    names.join(", "),
                    sig.name
                ),
                site.name_span,
            );
            return None;
        }
        if any_poison || !missing.is_empty() {
            return None;
        }
        // The bounds of the function's own parameters (E0801).
        for g in &generics {
            let Some(GenericArg::Ty(arg_ty)) = s.get(g.def).cloned() else {
                continue;
            };
            if self.is_poison(arg_ty) {
                continue;
            }
            for (j, b) in g.bounds.iter().enumerate() {
                if *b == Bound::Poison {
                    continue;
                }
                let bound = self.cx.d.subst_bound(b, &s);
                let span = first_mention
                    .get(&g.def)
                    .map(|i| self.span_of(args[*i]))
                    .unwrap_or(site.name_span);
                match self.satisfies_ext(arg_ty, &bound) {
                    Tri::Yes => {}
                    Tri::No => {
                        let at = self.name(arg_ty);
                        let bn = self.cx.bound_name(&bound);
                        let dpath = self.cx.sources.get(self.cx.file_of(t.def)).path.clone();
                        let bspan = g.bound_spans.get(j).copied().unwrap_or(g.span);
                        self.err(
                            "E0801",
                            format!("`{at}` does not satisfy the bound `{bn}` of `{}`", g.name),
                            span,
                        )
                        .relate_in("signature", &dpath, bspan);
                    }
                    Tri::Unknown => {
                        let at = self.name(arg_ty);
                        let bn = self.cx.bound_name(&bound);
                        self.ni(format!(
                            "whether `{at}` satisfies the bound `{bn}` (the specification does \
                             not define it for this type)"
                        ));
                    }
                }
            }
        }
        let ret = self.cx.d.instantiate(sig.ret, &s);
        let error = sig.error.map(|e| self.cx.d.instantiate(e, &s));
        let ty = match error {
            Some(er) => self.mk(TyKind::Result(ret, er)),
            None => ret,
        };
        let ty = if self.is_poison(ty) {
            self.poison()
        } else {
            ty
        };
        let fn_args = self.args_for(t.def, &s);
        Some(CallResult {
            ty,
            fn_args,
            subst: s,
        })
    }

    /// E1602: a variadic argument is an integer of at least 32 bits, `f64` or a raw
    /// pointer (section 29).
    fn variadic_argument(&mut self, a: ExprId, t: TypeId) {
        if self.is_poison(t) || self.is_never(t) {
            return;
        }
        let ok = match self.kind(t) {
            TyKind::Int(i) => i.bits() >= 32,
            TyKind::Float(f) => f == FloatTy::F64,
            TyKind::RawPtr(..) => true,
            _ => false,
        };
        if ok {
            return;
        }
        let name = self.name(t);
        let span = self.span_of(a);
        // C's default argument promotions: every integer narrower than `int`, signed or
        // not, and `bool` become `int`; `float` becomes `double`.
        let to = match self.kind(t) {
            TyKind::Int(_) | TyKind::Bool => Some(self.mk(TyKind::Int(IntTy::I32))),
            TyKind::Float(FloatTy::F32) => Some(self.f64_()),
            _ => None,
        };
        let fix = to.map(|to| self.append_as(a, to));
        let d = self.err(
            "E1602",
            format!("a variadic argument must be an integer of at least 32 bits, an `f64` or a raw pointer, not `{name}`"),
            span,
        );
        if let Some(edits) = fix {
            d.fix_edits("convert the argument", edits);
        }
    }

    /// The signature of a value that can be called: a closure, a `fn` pointer, a
    /// `&dyn Fn`, or a type parameter with an `Fn` bound.
    pub(crate) fn fn_sig_of(&mut self, ty: TypeId) -> Option<FnPtrSig> {
        match self.kind(ty) {
            TyKind::Closure(_, idx) => self.out.closures.get(&ExprId(idx)).map(|c| c.sig.clone()),
            TyKind::FnPtr(s) | TyKind::DynFn(_, _, s) => Some(FnPtrSig { ext: false, ..s }),
            TyKind::Param(p) => self
                .cx
                .d
                .param_bounds
                .get(&p)
                .into_iter()
                .flatten()
                .find_map(|b| match b {
                    Bound::Fn(_, s) => Some(s.clone()),
                    _ => None,
                }),
            _ => None,
        }
    }

    /// `satisfies`, with the function-like types that `Bound::Fn` is about.
    pub(crate) fn satisfies_ext(&mut self, ty: TypeId, b: &Bound) -> Tri {
        if let Bound::Fn(kind, sig) = b {
            let found: Option<FnPtrSig> = match self.kind(ty) {
                TyKind::Closure(_, idx) => {
                    self.out.closures.get(&ExprId(idx)).map(|c| c.sig.clone())
                }
                TyKind::FnPtr(s) if !s.ext => Some(s),
                TyKind::DynFn(_, k, s) if k == *kind || k == FnKind::Fn => Some(s),
                TyKind::Ref(_, inner) => return self.satisfies_ext(inner, b),
                _ => None,
            };
            if let Some(f) = found {
                if matches!(self.kind(ty), TyKind::Closure(..)) && *kind != FnKind::FnOnce {
                    self.ni(format!(
                        "whether a closure satisfies `{}` (it needs the move and mutation \
                         analysis of the ownership checker)",
                        kind.name()
                    ));
                }
                return Tri::from_bool(
                    f.params == sig.params && f.ret == sig.ret && f.error == sig.error,
                );
            }
        }
        self.cx.d.satisfies(ty, b)
    }

    // -------------------------------------------------------------- value calls

    fn value_call(
        &mut self,
        e: ExprId,
        callee: ExprId,
        args: &[ExprId],
        args_span: Span,
    ) -> (TypeId, ExprRes) {
        let _ = e;
        let ct = self.expr(callee, None);
        if self.is_poison(ct) || self.is_never(ct) {
            self.blind_args(args);
            return (self.poison(), ExprRes::None);
        }
        let (t, _) = self.strip_refs(ct);
        let found: Option<(FnPtrSig, ValueCall)> = match self.kind(t) {
            TyKind::FnPtr(s) => {
                if s.ext {
                    self.ni(
                        "whether a call through an `extern fn` pointer needs `unsafe` (section \
                         15 names calls of `extern` functions, section 29 the pointer type)",
                    );
                }
                Some((s, ValueCall::FnPtr))
            }
            TyKind::Closure(_, idx) => self
                .out
                .closures
                .get(&ExprId(idx))
                .map(|c| (c.sig.clone(), ValueCall::Closure(ExprId(idx)))),
            TyKind::DynFn(_, k, s) => Some((s, ValueCall::DynFn(k))),
            TyKind::Param(p) => self
                .cx
                .d
                .param_bounds
                .get(&p)
                .into_iter()
                .flatten()
                .find_map(|b| match b {
                    Bound::Fn(k, s) => Some((s.clone(), ValueCall::Bound(*k))),
                    _ => None,
                }),
            _ => None,
        };
        let Some((sig, kind)) = found else {
            let span = self.span_of(callee);
            let name = self.name(ct);
            self.err(
                "E0411",
                format!(
                    "a value of type `{name}` is not a function, closure or variant constructor"
                ),
                span,
            );
            self.blind_args(args);
            return (self.poison(), ExprRes::None);
        };
        if args.len() != sig.params.len() {
            self.err(
                "E0410",
                format!(
                    "this function value takes {} argument(s) but {} {} given",
                    sig.params.len(),
                    args.len(),
                    if args.len() == 1 { "was" } else { "were" }
                ),
                args_span,
            );
            self.blind_args(args);
            return (self.poison(), ExprRes::None);
        }
        for (a, p) in args.iter().zip(&sig.params) {
            self.expect(*a, *p);
        }
        let ty = match sig.error {
            Some(er) => self.mk(TyKind::Result(sig.ret, er)),
            None => sig.ret,
        };
        (
            ty,
            ExprRes::Call(Box::new(CallInfo {
                callee: Callee::Value(kind),
                receiver: None,
            })),
        )
    }

    // ---------------------------------------------------------- type members

    /// `Type.name(args)`: a variant constructor or an associated function.
    #[allow(clippy::too_many_arguments)]
    fn type_call(
        &mut self,
        e: ExprId,
        callee: ExprId,
        t: TypeId,
        name: &str,
        name_span: Span,
        args: &[ExprId],
        args_span: Span,
    ) -> (TypeId, ExprRes) {
        if self.is_poison(t) {
            self.blind_args(args);
            return (self.poison(), ExprRes::None);
        }
        let inherent = self.cx.d.inherent_impls(t).into_iter().any(|m| {
            self.cx
                .d
                .impl_sig(m.impl_def)
                .is_some_and(|s| s.methods.contains_key(name))
        });
        if let Some(v) = self.variant_named(t, name) {
            if inherent {
                self.ni("a variant and an associated function with one name");
                self.blind_args(args);
                return (self.poison(), ExprRes::None);
            }
            return self.variant_call(callee, v, name, args, args_span);
        }
        let Some(c) = self.select_candidate(t, name, name_span, false) else {
            self.blind_args(args);
            return (self.poison(), ExprRes::None);
        };
        self.finish_candidate_call(e, callee, c, t, args, args_span, name_span, None, 0)
    }

    fn variant_call(
        &mut self,
        callee: ExprId,
        v: VariantRef,
        name: &str,
        args: &[ExprId],
        args_span: Span,
    ) -> (TypeId, ExprRes) {
        let t = v.enum_ty;
        self.set(callee, t, ExprRes::Variant(v.clone()));
        let want = usize::from(v.payload.is_some());
        if args.len() != want {
            self.err(
                "E0410",
                format!(
                    "the variant `{name}` takes {want} argument(s) but {} {} given",
                    args.len(),
                    if args.len() == 1 { "was" } else { "were" }
                ),
                args_span,
            );
            self.blind_args(args);
            return (self.poison(), ExprRes::None);
        }
        if let (Some(p), Some(a)) = (v.payload, args.first()) {
            self.expect(*a, p);
        }
        (
            t,
            ExprRes::Call(Box::new(CallInfo {
                callee: Callee::Variant(v),
                receiver: None,
            })),
        )
    }

    // ------------------------------------------------------- method resolution

    /// The function `name` on a receiver of type `t0`: the inherent impls first, then
    /// the interfaces the type implements, then, for a type parameter, its bounds
    /// (pipeline section 4.4). E0313 when two interfaces supply it, E0314 or E0802 when
    /// none does.
    pub(crate) fn select_candidate(
        &mut self,
        t0: TypeId,
        name: &str,
        name_span: Span,
        need_method: bool,
    ) -> Option<MethodCandidate> {
        let mut found: Vec<MethodCandidate> = Vec::new();
        // `&dyn I`: the interface's own functions.
        if let TyKind::Dyn(_, iface, args) = self.kind(t0) {
            let mut seen = BTreeSet::new();
            let mut stack = vec![(iface, args)];
            while let Some((i, a)) = stack.pop() {
                if !seen.insert(i) {
                    continue;
                }
                if let Some(f) = self.cx.d.interface_method(i, name) {
                    found.push(MethodCandidate {
                        method: f,
                        source: MethodSource::Bound {
                            interface: i,
                            args: a.clone(),
                        },
                    });
                }
                if let Some(sig) = self.cx.d.interface_sig(i).cloned() {
                    for sup in sig.supers {
                        if let Bound::Interface(j, ja) = sup {
                            stack.push((j, ja));
                        }
                    }
                }
            }
        } else {
            found = self.cx.d.method_candidates(t0, name);
        }
        // Drop candidates whose impl's own bounds do not hold (method_candidates does
        // not check them), and functions that are not methods when one is needed.
        let mut kept: Vec<MethodCandidate> = Vec::new();
        for c in found {
            let is_method = self.cx.d.fn_sig(c.method).is_some_and(|f| f.is_method());
            if need_method && !is_method {
                continue;
            }
            match &c.source {
                MethodSource::Inherent(m) | MethodSource::Impl { matched: m, .. } => {
                    match self.cx.d.impl_bounds_hold(m) {
                        Tri::Yes => kept.push(c),
                        Tri::No => {}
                        Tri::Unknown => {
                            self.ni(format!(
                                "whether the bounds of an `impl` hold for `{}` (the specification does not define it)",
                                self.name(t0)
                            ));
                        }
                    }
                }
                _ => kept.push(c),
            }
        }
        if let Some(i) = kept
            .iter()
            .position(|c| matches!(c.source, MethodSource::Inherent(_)))
        {
            return Some(kept.swap_remove(i));
        }
        // Distinct interfaces.
        let mut ifaces: Vec<DefId> = Vec::new();
        for c in &kept {
            let i = match &c.source {
                MethodSource::Impl { interface, .. }
                | MethodSource::Builtin { interface }
                | MethodSource::Bound { interface, .. } => *interface,
                MethodSource::Inherent(_) => continue,
            };
            if !ifaces.contains(&i) {
                ifaces.push(i);
            }
        }
        match ifaces.len() {
            1 => Some(kept.swap_remove(0)),
            0 => {
                self.no_such_method(t0, name, name_span);
                None
            }
            _ => {
                let names: Vec<String> = ifaces
                    .iter()
                    .map(|i| self.cx.sema.def(*i).name.clone())
                    .collect();
                let mut rel: Vec<(String, Span)> = Vec::new();
                for c in &kept {
                    if let Some(f) = self.cx.d.fn_sig(c.method) {
                        rel.push((
                            self.cx.sources.get(self.cx.file_of(c.method)).path.clone(),
                            f.name_span,
                        ));
                    }
                }
                let d = self.err(
                    "E0313",
                    format!(
                        "`{name}` is supplied by more than one interface ({}): write `I.{name}(value, ..)`",
                        names.join(", ")
                    ),
                    name_span,
                );
                for (file, span) in rel {
                    d.relate_in("candidate", &file, span);
                }
                None
            }
        }
    }

    /// E0314, or E0802 for a function that an interface declares but the bounds of a
    /// type parameter do not grant.
    fn no_such_method(&mut self, t0: TypeId, name: &str, name_span: Span) {
        if let TyKind::Param(p) = self.kind(t0) {
            // The interfaces that declare a function of this name.
            let mut declaring: Vec<DefId> = Vec::new();
            let all: Vec<DefId> = self
                .cx
                .d
                .items
                .iter()
                .filter_map(|(d, s)| matches!(s, ItemSig::Interface(_)).then_some(*d))
                .collect();
            for i in all {
                if self.cx.d.interface_method(i, name).is_some() {
                    declaring.push(i);
                }
            }
            if !declaring.is_empty() {
                let b = self.cx.d.builtins;
                let builtin_ifaces: Vec<DefId> = [b.eq, b.ord, b.hash, b.iter, b.format]
                    .into_iter()
                    .flatten()
                    .filter(|i| declaring.contains(i))
                    .collect();
                let fix = if builtin_ifaces.len() == 1 && declaring.len() == 1 {
                    self.add_bound_edit(p, builtin_ifaces[0])
                } else {
                    None
                };
                let tname = self.name(t0);
                let ifname = self.cx.sema.def(declaring[0]).name.clone();
                self.missing_bound(
                    t0,
                    name_span,
                    &format!(
                        "the function `{name}` (declared by the interface `{ifname}`) on `{tname}`"
                    ),
                );
                if let Some((message, edit)) = fix
                    && let Some(d) = self.cx.d.diagnostics.last_mut()
                {
                    d.fix(message, edit);
                }
                return;
            }
        }
        if matches!(self.kind(t0), TyKind::Adt(..)) {
            self.unknown_function(t0, name, name_span);
            return;
        }
        // The specification names these functions of slices and optionals (sections 5.5,
        // 7.1 and 11) without a signature.
        if matches!(self.kind(t0), TyKind::Slice(..) | TyKind::Optional(_))
            && matches!(name, "shared" | "refs" | "take")
        {
            self.ni(format!(
                "`.{name}()`: the specification names it but gives no signature"
            ));
            return;
        }
        let names = self.method_names(t0);
        self.unknown_member(name, name_span, &names, t0);
    }

    /// The edit that adds the bound `iface` to the generic parameter `p`.
    fn add_bound_edit(
        &self,
        p: DefId,
        iface: DefId,
    ) -> Option<(String, ingot_diag::diagnostic::Edit)> {
        // Find the generic parameter in the HIR of the function being checked or its
        // impl: the owner of `p` is its parent definition.
        let owner = self.cx.sema.def(p).parent?;
        let generics: &[hir::GenericParam] = match self.cx.hir_item(owner)? {
            hir::Item::Fn(f) => &f.generics,
            hir::Item::Impl(i) => &i.generics,
            hir::Item::Struct(s) => &s.generics,
            hir::Item::Enum(e) => &e.generics,
            _ => return None,
        };
        let g = generics.iter().find(|g| g.def == p)?;
        let name = self.cx.sema.def(iface).name.clone();
        let edit = match g.bounds.last() {
            Some(last) => self.edit(Span::empty(last.span.end), format!(" + {name}")),
            None => self.edit(Span::empty(g.name_span.end), format!(": {name}")),
        };
        Some((format!("add the bound `{name}`"), edit))
    }

    /// Names of methods that exist on a type, for the fix of E0314.
    fn method_names(&mut self, t0: TypeId) -> Vec<String> {
        let mut out = Vec::new();
        for m in self.cx.d.inherent_impls(t0) {
            if let Some(s) = self.cx.d.impl_sig(m.impl_def) {
                out.extend(s.methods.keys().cloned());
            }
        }
        for (iface, _, _) in self.cx.d.interface_impls(t0) {
            if let Some(sig) = self.cx.d.interface_sig(iface) {
                for &m in &sig.methods {
                    if let Some(f) = self.cx.d.fn_sig(m) {
                        out.push(f.name.clone());
                    }
                }
            }
        }
        for iface in self.cx.d.builtin_interface_impls(t0) {
            if let Some(sig) = self.cx.d.interface_sig(iface) {
                for &m in &sig.methods {
                    if let Some(f) = self.cx.d.fn_sig(m) {
                        out.push(f.name.clone());
                    }
                }
            }
        }
        if let TyKind::Param(p) = self.kind(t0) {
            for b in self.cx.d.param_bounds.get(&p).cloned().unwrap_or_default() {
                if let Bound::Interface(i, _) = b
                    && let Some(sig) = self.cx.d.interface_sig(i)
                {
                    for &m in &sig.methods.clone() {
                        if let Some(f) = self.cx.d.fn_sig(m) {
                            out.push(f.name.clone());
                        }
                    }
                }
            }
        }
        out
    }

    /// The substitution that gives `Self` and the interface's own generic arguments.
    fn iface_subst(&mut self, iface: DefId, args: &[GenericArg], self_ty: TypeId) -> Subst {
        let mut s = self.cx.d.subst_for(iface, args);
        if let Some(sig) = self.cx.d.interface_sig(iface) {
            s.insert(sig.self_def, GenericArg::Ty(self_ty));
        }
        s
    }

    /// The reference to the function of a candidate, and the substitution that applies
    /// to its signature before its own generics are solved.
    pub(crate) fn fn_ref_from(
        &mut self,
        c: &MethodCandidate,
        self_ty: TypeId,
    ) -> (FnSource, Subst, Vec<DefId>) {
        match &c.source {
            MethodSource::Inherent(m) => {
                let unmatched: Vec<DefId> = self
                    .cx
                    .d
                    .generics_of(m.impl_def)
                    .iter()
                    .map(|g| g.def)
                    .filter(|d| m.subst.get(*d).is_none())
                    .collect();
                let impl_args = self.args_for(m.impl_def, &m.subst);
                (
                    FnSource::Inherent {
                        impl_def: m.impl_def,
                        impl_args,
                    },
                    m.subst.clone(),
                    unmatched,
                )
            }
            MethodSource::Impl {
                interface,
                args,
                matched,
            } => {
                let own = self.cx.sema.def(c.method).parent == Some(matched.impl_def);
                let subst = if own {
                    matched.subst.clone()
                } else {
                    self.iface_subst(*interface, args, self_ty)
                };
                let impl_args = self.args_for(matched.impl_def, &matched.subst);
                (
                    FnSource::Interface {
                        interface: *interface,
                        interface_args: args.clone(),
                        self_ty,
                        via: InterfaceVia::Impl {
                            impl_def: matched.impl_def,
                            impl_args,
                        },
                    },
                    subst,
                    Vec::new(),
                )
            }
            MethodSource::Builtin { interface } => {
                let subst = self.iface_subst(*interface, &[], self_ty);
                (
                    FnSource::Interface {
                        interface: *interface,
                        interface_args: Vec::new(),
                        self_ty,
                        via: InterfaceVia::Builtin,
                    },
                    subst,
                    Vec::new(),
                )
            }
            MethodSource::Bound { interface, args } => {
                let subst = self.iface_subst(*interface, args, self_ty);
                let via = if matches!(self.kind(self_ty), TyKind::Dyn(..)) {
                    InterfaceVia::Dyn
                } else {
                    InterfaceVia::Bound
                };
                (
                    FnSource::Interface {
                        interface: *interface,
                        interface_args: args.clone(),
                        self_ty,
                        via,
                    },
                    subst,
                    Vec::new(),
                )
            }
        }
    }

    /// A reference to a candidate that takes no generic arguments of its own (used for
    /// `next` of an `Iter`).
    pub(crate) fn fn_ref_of_candidate(&mut self, c: &MethodCandidate, self_ty: TypeId) -> FnRef {
        let (source, _, _) = self.fn_ref_from(c, self_ty);
        FnRef {
            def: c.method,
            args: Vec::new(),
            source,
        }
    }

    // ------------------------------------------------------------ method calls

    #[allow(clippy::too_many_arguments)]
    fn method_call(
        &mut self,
        e: ExprId,
        callee: ExprId,
        base: ExprId,
        name: &str,
        name_span: Span,
        args: &[ExprId],
        args_span: Span,
    ) -> (TypeId, ExprRes) {
        let bt = self.expr(base, None);
        if self.is_poison(bt) || self.is_never(bt) {
            self.blind_args(args);
            return (self.poison(), ExprRes::None);
        }
        let (t0, derefs) = self.strip_refs(bt);
        if let TyKind::RawPtr(..) = self.kind(t0) {
            let at = self.dot_before(name_span);
            let edit = self.edit(Span::empty(at), ".*");
            self.note_raw_access();
            self.err(
                "E0418",
                "a method call on a raw pointer: write `.*` first",
                Span::new(at, at + 1),
            )
            .fix("dereference the pointer first", edit);
            self.blind_args(args);
            return (self.poison(), ExprRes::None);
        }
        let Some(c) = self.select_candidate(t0, name, name_span, true) else {
            self.blind_args(args);
            return (self.poison(), ExprRes::None);
        };
        self.finish_candidate_call(
            e,
            callee,
            c,
            t0,
            args,
            args_span,
            name_span,
            Some((base, derefs)),
            1,
        )
    }

    /// Resolve the signature of a chosen candidate, adjust the receiver (for a method
    /// call), check the arguments and record the call.
    #[allow(clippy::too_many_arguments)]
    fn finish_candidate_call(
        &mut self,
        e: ExprId,
        callee: ExprId,
        c: MethodCandidate,
        self_ty: TypeId,
        args: &[ExprId],
        args_span: Span,
        name_span: Span,
        receiver: Option<(ExprId, u32)>,
        skip: usize,
    ) -> (TypeId, ExprRes) {
        let _ = e;
        let Some(sig) = self.fn_sig(c.method) else {
            self.blind_args(args);
            return (self.poison(), ExprRes::None);
        };
        self.note_unsafe_callee(&sig, self.span_of(e));
        if sig.mods.kernel {
            self.ni("E1515 not_a_kernel: a direct call of a `kernel fn` (S4b)");
        }
        let (source, subst, extra) = self.fn_ref_from(&c, self_ty);
        // The receiver: look through `derefs` borrows, then pass it as `self` says.
        let recv = match receiver {
            Some((base, derefs)) => {
                let mode = match sig.params.first().map(|p| self.kind(p.ty)) {
                    Some(TyKind::Ref(Mutability::Shared, _)) => ReceiverMode::Shared,
                    Some(TyKind::Ref(Mutability::Mut, _)) => ReceiverMode::Exclusive,
                    _ => ReceiverMode::Value,
                };
                match mode {
                    ReceiverMode::Exclusive => self.require_mutable_receiver(base, derefs),
                    ReceiverMode::Shared => {
                        if derefs == 0 && self.place(base).is_none() {
                            let span = self.span_of(base);
                            self.err(
                                "E0607",
                                "a method that takes `&self` borrows its receiver, which is not a place",
                                span,
                            );
                        }
                    }
                    ReceiverMode::Value => {}
                }
                Some(Receiver { derefs, mode })
            }
            None => None,
        };
        let target = CallTarget {
            def: c.method,
            sig: sig.clone(),
            subst,
            extra_binders: extra,
            skip,
            explicit: Vec::new(),
            explicit_span: None,
        };
        let site = CallSite {
            args,
            args_span,
            name_span,
            known: BTreeMap::new(),
        };
        let Some(r) = self.apply_call(&target, &site) else {
            return (self.poison(), ExprRes::None);
        };
        let source = match source {
            FnSource::Inherent { impl_def, .. } => FnSource::Inherent {
                impl_def,
                impl_args: self.args_for(impl_def, &r.subst),
            },
            other => other,
        };
        let fnref = FnRef {
            def: c.method,
            args: r.fn_args.clone(),
            source,
        };
        let sigty = self.callee_type(&sig, &r.subst);
        self.set(callee, sigty, ExprRes::Fn(fnref.clone()));
        (
            r.ty,
            ExprRes::Call(Box::new(CallInfo {
                callee: Callee::Fn(fnref),
                receiver: recv,
            })),
        )
    }

    /// `I.name(value, args)`: a function of an interface, with `Self` taken from the
    /// first argument.
    #[allow(clippy::too_many_arguments)]
    fn interface_call(
        &mut self,
        e: ExprId,
        callee: ExprId,
        iface: DefId,
        name: &str,
        name_span: Span,
        args: &[ExprId],
        args_span: Span,
    ) -> (TypeId, ExprRes) {
        let _ = e;
        self.cx.ensure_sig(iface);
        let Some(m) = self.cx.d.interface_method(iface, name) else {
            let iname = self.cx.sema.def(iface).name.clone();
            self.err(
                "E0314",
                format!("the interface `{iname}` has no function `{name}`"),
                name_span,
            );
            self.blind_args(args);
            return (self.poison(), ExprRes::None);
        };
        let Some(sig) = self.fn_sig(m) else {
            self.blind_args(args);
            return (self.poison(), ExprRes::None);
        };
        self.note_unsafe_callee(&sig, self.span_of(e));
        let Some(isig) = self.cx.d.interface_sig(iface).cloned() else {
            self.blind_args(args);
            return (self.poison(), ExprRes::None);
        };
        if !sig.is_method() || args.is_empty() {
            self.ni("a call `I.name(..)` of an interface function that takes no `self`");
            self.blind_args(args);
            return (self.poison(), ExprRes::None);
        }
        // `Self` is read off the type of the first argument.
        let first = self.expr(args[0], None);
        if self.is_poison(first) {
            self.blind_args(&args[1..]);
            return (self.poison(), ExprRes::None);
        }
        let mut binders = vec![isig.self_def];
        binders.extend(self.cx.d.generics_of(iface).iter().map(|g| g.def));
        let mut s = Subst::new();
        if !self
            .cx
            .d
            .match_ty(sig.params[0].ty, first, &binders, &mut s)
        {
            self.mismatch(args[0], first, sig.params[0].ty);
            self.blind_args(&args[1..]);
            return (self.poison(), ExprRes::None);
        }
        let Some(GenericArg::Ty(self_ty)) = s.get(isig.self_def).cloned() else {
            self.blind_args(&args[1..]);
            return (self.poison(), ExprRes::None);
        };
        let iargs: Vec<GenericArg> = self
            .cx
            .d
            .generics_of(iface)
            .to_vec()
            .iter()
            .map(|g| {
                s.get(g.def)
                    .cloned()
                    .unwrap_or(GenericArg::Ty(self.poison()))
            })
            .collect();
        let Some(source) = self.resolve_interface(self_ty, iface, &iargs) else {
            let tn = self.name(self_ty);
            let iname = self.cx.sema.def(iface).name.clone();
            self.err(
                "E0801",
                format!("`{tn}` does not implement `{iname}`"),
                self.span_of(args[0]),
            );
            self.blind_args(&args[1..]);
            return (self.poison(), ExprRes::None);
        };
        let (func, subst) = match &source {
            InterfaceVia::Impl { impl_def, .. } => {
                let own = self
                    .cx
                    .d
                    .impl_sig(*impl_def)
                    .and_then(|i| i.methods.get(name).copied());
                match own {
                    Some(f) => {
                        let ms = self
                            .cx
                            .d
                            .match_impl(*impl_def, self_ty)
                            .map(|m| m.subst)
                            .unwrap_or_default();
                        (f, ms)
                    }
                    None => (m, self.iface_subst(iface, &iargs, self_ty)),
                }
            }
            _ => (m, self.iface_subst(iface, &iargs, self_ty)),
        };
        let Some(fsig) = self.fn_sig(func) else {
            self.blind_args(&args[1..]);
            return (self.poison(), ExprRes::None);
        };
        let mut known = BTreeMap::new();
        known.insert(0, first);
        let target = CallTarget {
            def: func,
            sig: fsig.clone(),
            subst,
            extra_binders: Vec::new(),
            skip: 0,
            explicit: Vec::new(),
            explicit_span: None,
        };
        let site = CallSite {
            args,
            args_span,
            name_span,
            known,
        };
        let Some(r) = self.apply_call(&target, &site) else {
            return (self.poison(), ExprRes::None);
        };
        let fnref = FnRef {
            def: func,
            args: r.fn_args.clone(),
            source: FnSource::Interface {
                interface: iface,
                interface_args: iargs,
                self_ty,
                via: source,
            },
        };
        let sigty = self.callee_type(&fsig, &r.subst);
        self.set(callee, sigty, ExprRes::Fn(fnref.clone()));
        (
            r.ty,
            ExprRes::Call(Box::new(CallInfo {
                callee: Callee::Fn(fnref),
                receiver: None,
            })),
        )
    }

    /// How `ty` implements the interface, if it does.
    pub(crate) fn resolve_interface(
        &mut self,
        ty: TypeId,
        iface: DefId,
        args: &[GenericArg],
    ) -> Option<InterfaceVia> {
        match self.kind(ty) {
            TyKind::Param(_) => {
                let b = Bound::Interface(iface, args.to_vec());
                (self.cx.d.satisfies(ty, &b) == Tri::Yes).then_some(InterfaceVia::Bound)
            }
            TyKind::Dyn(_, d, a) if d == iface && a == args => Some(InterfaceVia::Dyn),
            _ => {
                for (i, a, m) in self.cx.d.interface_impls(ty) {
                    if i == iface && a == args && self.cx.d.impl_bounds_hold(&m) == Tri::Yes {
                        let impl_args = self.args_for(m.impl_def, &m.subst);
                        return Some(InterfaceVia::Impl {
                            impl_def: m.impl_def,
                            impl_args,
                        });
                    }
                }
                (args.is_empty() && self.cx.d.builtin_implements(ty, iface) == Tri::Yes)
                    .then_some(InterfaceVia::Builtin)
            }
        }
    }

    // ---------------------------------------------------------------- closures

    pub(crate) fn closure(&mut self, e: ExprId, c: &hir::Closure) -> (TypeId, ExprRes) {
        let params: Vec<TypeId> = c.params.iter().map(|p| self.lower(&p.ty)).collect();
        let ret = self.lower(&c.ret);
        let error = c.error.as_ref().map(|t| self.lower(t));
        let sig = FnPtrSig {
            ext: false,
            params: params.clone(),
            ret,
            error,
        };
        let ty = self.mk(TyKind::Closure(self.owner, e.0));
        // The captures: the name inside the body has the captured type.
        let mut caps = Vec::new();
        let mut part_tys = Vec::new();
        for cap in &c.captures {
            let outer_ty = match cap.outer {
                Res::Local(l) => self.local_ty(l),
                _ => self.poison(),
            };
            let (lty, mode, cm) = if cap.shared && cap.mutable {
                (
                    self.ref_to(Mutability::Mut, outer_ty),
                    BindMode::Move,
                    CaptureMode::Exclusive,
                )
            } else if cap.shared {
                (
                    self.ref_to(Mutability::Shared, outer_ty),
                    BindMode::Copy,
                    CaptureMode::Shared,
                )
            } else {
                let mode = if self.is_copy(outer_ty) {
                    BindMode::Copy
                } else {
                    BindMode::Move
                };
                (outer_ty, mode, CaptureMode::ByValue)
            };
            if cm == CaptureMode::Exclusive
                && let Res::Local(l) = cap.outer
            {
                self.exclusive_capture(l, cap.span);
            }
            self.bind_local(cap.local, lty, mode);
            part_tys.push(lty);
            caps.push(CaptureInfo {
                local: cap.local,
                outer: match cap.outer {
                    Res::Local(l) => Some(l),
                    _ => None,
                },
                mode: cm,
            });
        }
        self.register_closure_props(ty, &part_tys);
        self.out.closures.insert(
            e,
            ClosureInfo {
                sig: sig.clone(),
                ty,
                captures: caps,
                span: c.span,
            },
        );
        for (p, pt) in c.params.iter().zip(&params) {
            let mode = if self.is_copy(*pt) {
                BindMode::Copy
            } else {
                BindMode::Move
            };
            self.bind_local(p.local, *pt, mode);
        }
        let saved_par = std::mem::take(&mut self.par_depth);
        self.fns.push(FnCtx {
            ret,
            error,
            error_span: c.error.as_ref().map(|t| t.span),
            kind: CtxKind::Closure,
            loop_base: self.loops.len(),
            defer_base: self.defer_depth,
        });
        let completes = self.block(c.body);
        self.fns.pop();
        self.par_depth = saved_par;
        if completes && ret != self.void() && !self.is_never(ret) && !self.is_poison(ret) {
            let b = self.hir.block(c.body).span;
            let at = Span::new(b.end.saturating_sub(1), b.end);
            self.err(
                "E0412",
                "a path through this closure reaches the end of its body without returning a value",
                at,
            );
        }
        (ty, ExprRes::Closure)
    }

    /// A `&mut name` capture is an exclusive borrow of the variable (E0610).
    fn exclusive_capture(&mut self, l: hir::LocalId, span: Span) {
        let loc = self.hir.local(l);
        if loc.mutable && loc.kind != hir::LocalKind::Capture {
            self.mutated.insert(l);
            return;
        }
        let name = loc.name.clone();
        let decl = loc.span;
        let path = self.file_path();
        let edit = self.let_var_edit(l);
        let d = self.err(
            "E0610",
            format!("`{name}` is not a `var`, so it cannot be captured by `&mut`"),
            span,
        );
        d.relate_in("declared", &path, decl);
        if let Some(edit) = edit {
            d.fix("declare it with `var`", edit);
        }
    }

    fn let_var_edit(&self, l: hir::LocalId) -> Option<ingot_diag::diagnostic::Edit> {
        let stmt = *self.let_of.get(&l)?;
        let s = self.hir.stmt(stmt);
        match &s.kind {
            hir::StmtKind::Let { mutable: false, .. } => {
                Some(self.edit(Span::new(s.span.start, s.span.start + 3), "var"))
            }
            _ => None,
        }
    }

    /// Enter the properties of a closure type: they come from the captured types
    /// (section 20). A closure is not `Copy` (section 10 lists the types that are).
    fn register_closure_props(&mut self, ty: TypeId, parts: &[TypeId]) {
        let tuple = self.mk(TyKind::Tuple(parts.to_vec()));
        let mut p = self.cx.d.props(tuple);
        p.copy = Tri::No;
        self.cx.d.props.insert(ty, p);
    }

    // ------------------------------------------------- spawn, dispatch (typed)

    /// `spawn call in scope`. The rules of `spawn` that need the ownership checker
    /// (E1401, E1403) belong to phase 9; E1404 and E1405 are decided here.
    pub(crate) fn spawn(
        &mut self,
        e: ExprId,
        call: ExprId,
        scope: ExprId,
        value: bool,
    ) -> (TypeId, ExprRes) {
        let _ = e;
        self.ni("the rules of `spawn`: E1401 and E1403 (phase 9, S6 to S9)");
        let ct = self.expr(call, None);
        let st = self.expr(scope, None);
        if !self.is_poison(st) && !self.is_never(st) {
            let expected = self
                .std_type("std.task", "Scope")
                .map(|d| {
                    let t = self.mk(TyKind::Adt(d, Vec::new()));
                    self.ref_to(Mutability::Shared, t)
                })
                .unwrap_or_else(|| self.poison());
            if !self.compatible(st, expected) {
                let sn = self.name(st);
                self.err(
                    "E1405",
                    format!("the scope of a `spawn` is a `&Scope`, not `{sn}`"),
                    self.span_of(scope),
                );
            }
        }
        if !value {
            if !self.is_poison(ct) && !self.is_never(ct) && ct != self.void() {
                let n = self.name(ct);
                self.err(
                    "E1404",
                    format!("a statement-form `spawn` needs a call that returns `void`, not `{n}`"),
                    self.span_of(call),
                );
            }
            return (self.void(), ExprRes::Spawn);
        }
        match self.std_type("std.task", "Task") {
            Some(d) if !self.is_poison(ct) => (
                self.mk(TyKind::Adt(d, vec![GenericArg::Ty(ct)])),
                ExprRes::Spawn,
            ),
            _ => {
                self.ni("`spawn`: the type `std.task.Task` is not declared");
                (self.poison(), ExprRes::None)
            }
        }
    }

    pub(crate) fn spawn_stmt(&mut self, call: ExprId, scope: ExprId) {
        self.spawn(call, call, scope, false);
    }

    pub(crate) fn dispatch(
        &mut self,
        e: ExprId,
        call: ExprId,
        domain: ExprId,
        executor: ExprId,
    ) -> (TypeId, ExprRes) {
        let _ = e;
        let t = self.dispatch_value(call, domain, executor);
        (t, ExprRes::Dispatch)
    }

    /// The value of `dispatch k(args) over domain on executor`: the kernel's reduced
    /// value, or `void`. The rules of kernels (E1510 to E1519) belong to the next step.
    pub(crate) fn dispatch_value(
        &mut self,
        call: ExprId,
        domain: ExprId,
        executor: ExprId,
    ) -> TypeId {
        self.ni("the rules of `dispatch` and kernels: E1510 to E1519 (S4b)");
        let hir = self.hir;
        // The domain: a range, or a tuple of ranges, of `i64`.
        let mut ranges = Vec::new();
        match &hir.expr(domain).kind {
            ExprKind::Range { lo, hi, .. } => ranges.push((*lo, *hi)),
            ExprKind::Tuple(items) => {
                for x in items {
                    if let ExprKind::Range { lo, hi, .. } = &hir.expr(*x).kind {
                        ranges.push((*lo, *hi));
                    }
                }
            }
            _ => {}
        }
        let i64_ = self.i64_();
        for (lo, hi) in ranges {
            self.expect(lo, i64_);
            self.expect(hi, i64_);
        }
        self.expr(executor, None);
        let ExprKind::Call {
            callee,
            args,
            args_span,
        } = &hir.expr(call).kind
        else {
            self.expr(call, None);
            return self.poison();
        };
        let ExprKind::Path(p) = &hir.expr(*callee).kind else {
            self.blind_args(args);
            self.expr(*callee, None);
            return self.poison();
        };
        let Res::Def(d) = p.res else {
            self.blind_args(args);
            return self.poison();
        };
        let Some(sig) = self.fn_sig(d) else {
            self.blind_args(args);
            return self.poison();
        };
        let name_span = p.segments.last().map(|(_, s)| *s).unwrap_or(p.span);
        let target = CallTarget {
            def: d,
            sig: sig.clone(),
            subst: Subst::new(),
            extra_binders: Vec::new(),
            skip: 1,
            explicit: Vec::new(),
            explicit_span: None,
        };
        let site = CallSite {
            args,
            args_span: *args_span,
            name_span,
            known: BTreeMap::new(),
        };
        let reduce = matches!(self.cx.hir_item(d), Some(hir::Item::Fn(f)) if f.reduce.is_some());
        match self.apply_call(&target, &site) {
            Some(r) => {
                if reduce {
                    r.ty
                } else {
                    self.void()
                }
            }
            None => self.poison(),
        }
    }

    /// The type the body of a function mentions: used by the unit tests.
    #[allow(dead_code)]
    pub(crate) fn expr_kind_name(&self, e: ExprId) -> &'static str {
        match &self.hir.expr(e).kind {
            ExprKind::Call { .. } => "call",
            _ => "other",
        }
    }
}
