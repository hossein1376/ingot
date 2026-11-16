//! Expressions (spec sections 5 and 6.3): the dispatch on the expression kind, names,
//! members, indexing, struct and enum values, `if`, `match`, `catch`, `orelse`, `try`.

use super::Checker;
use crate::defs::{Builtin, DefId, DefKind};
use crate::hir::{self, ExprId, ExprKind, Handler, Res};
use crate::resolve::unique_near;
use crate::sigs::*;
use crate::traits::{MethodCandidate, MethodSource};
use crate::ty::*;
use crate::typed::*;
use ingot_diag::source::Span;
use ingot_syntax::ast::UnOp;

impl Checker<'_, '_, '_> {
    /// Type `e`. `hint` is the type the context expects; only an untyped literal, a
    /// shorthand and `@zero` use it, and the caller compares the result with it.
    pub(crate) fn expr(&mut self, e: ExprId, hint: Option<TypeId>) -> TypeId {
        let (ty, res) = self.expr_inner(e, hint);
        self.set(e, ty, res)
    }

    /// Type `e` against the type the context requires, reporting a mismatch. A
    /// `&T` (`&mut T`) where a `&dyn I` (`&mut dyn I`) is required is the one implicit
    /// conversion between non-literal values (sections 3 and 4.4).
    pub(crate) fn expect(&mut self, e: ExprId, expected: TypeId) -> TypeId {
        let found = self.expr(e, Some(expected));
        if self.compatible(found, expected) {
            return found;
        }
        if let Some(t) = self.dyn_coerce(e, found, expected) {
            return t;
        }
        self.mismatch(e, found, expected);
        found
    }

    /// A branch of an `if` or `match` expression: its context is the whole expression's,
    /// so a `&T` branch becomes the `&dyn I` the context expects (section 3).
    fn branch(&mut self, e: ExprId, hint: Option<TypeId>) -> TypeId {
        let t = self.expr(e, hint);
        match hint {
            Some(h) if !self.compatible(t, h) => self.dyn_coerce(e, t, h).unwrap_or(t),
            _ => t,
        }
    }

    /// A `&T` or `&mut T` where `&dyn I` or `&mut dyn I` is expected, with the same
    /// mutability and `T` implementing `I`. Records the conversion on the expression
    /// and returns the target type; `None` when this is not that conversion.
    pub(crate) fn dyn_coerce(
        &mut self,
        e: ExprId,
        found: TypeId,
        expected: TypeId,
    ) -> Option<TypeId> {
        if let TyKind::DynFn(target_m, kind, sig) = self.kind(expected).clone() {
            let TyKind::Ref(source_m, inner) = self.kind(found).clone() else {
                return None;
            };
            if source_m != target_m {
                return None;
            }
            let TyKind::Closure(_, idx) = self.kind(inner) else {
                return None;
            };
            let c = self.out.closures.get(&ExprId(idx)).cloned()?;
            let meets = match kind {
                FnKind::Fn => c.kind == FnKind::Fn,
                FnKind::FnMut => c.kind != FnKind::FnOnce,
                FnKind::FnOnce => true,
            };
            if !meets || c.sig != sig {
                return None;
            }
            let info = FnDynCoercion {
                kind,
                sig,
                self_ty: inner,
            };
            if let Some(expr) = self.out.exprs.get_mut(&e) {
                expr.ty = expected;
                expr.fn_dyn = Some(info);
            }
            return Some(expected);
        }
        let TyKind::Dyn(target_m, iface, iargs) = self.kind(expected).clone() else {
            return None;
        };
        let TyKind::Ref(source_m, inner) = self.kind(found).clone() else {
            return None;
        };
        if source_m != target_m || matches!(self.kind(inner), TyKind::Dyn(..)) {
            return None;
        }
        let bound = Bound::Interface(iface, iargs.clone());
        match self.cx.d.satisfies(inner, &bound) {
            Tri::Yes => {
                let iname = self.cx.sema.def(iface).name.clone();
                let Some(via) = self.resolve_interface(inner, iface, &iargs) else {
                    let tn = self.name(inner);
                    self.ni(format!("how `{tn}` implements `{iname}`"));
                    return Some(self.poison());
                };
                let info = DynCoercion {
                    interface: iface,
                    interface_args: iargs,
                    self_ty: inner,
                    via,
                };
                if let Some(expr) = self.out.exprs.get_mut(&e) {
                    expr.ty = expected;
                    expr.coerce = Some(info);
                }
                Some(expected)
            }
            Tri::No => {
                let tn = self.name(inner);
                let iname = self.cx.sema.def(iface).name.clone();
                let borrow = if target_m == Mutability::Mut {
                    "&mut "
                } else {
                    "&"
                };
                let span = self.span_of(e);
                self.err(
                    "E0401",
                    format!(
                        "`{tn}` does not implement `{iname}`, so `{borrow}{tn}` cannot become \
                         `{borrow}dyn {iname}`"
                    ),
                    span,
                );
                // The context gets the type it asked for, so the error is not repeated.
                Some(expected)
            }
            Tri::Unknown => {
                let tn = self.name(inner);
                let iname = self.cx.sema.def(iface).name.clone();
                self.ni(format!(
                    "whether `{tn}` implements `{iname}` (the specification does not define it \
                     for this type)"
                ));
                Some(self.poison())
            }
        }
    }

    /// Whether this function can propagate an error of type `err` with `try`: its own
    /// error type is `err`, or an enum with a variant `from err`; a test body may try any
    /// error (section 4.6).
    pub(crate) fn can_propagate(&mut self, err: TypeId) -> bool {
        let ctx = self.fnctx();
        if ctx.kind == super::CtxKind::Test {
            return true;
        }
        match ctx.error {
            None => false,
            Some(fe) if fe == err || self.is_poison(fe) => true,
            Some(fe) => self.variant_from(fe, err).is_some(),
        }
    }

    /// E0701: a `T ! E` value where `T` is expected, or as a statement. The fix inserts
    /// `try ` only when the enclosing function can propagate `E`.
    pub(crate) fn unhandled_result(&mut self, e: ExprId, t: TypeId) -> bool {
        let TyKind::Result(ok_ty, err_ty) = self.kind(t) else {
            return false;
        };
        let (ok, err) = (self.name(ok_ty), self.name(err_ty));
        let span = self.span_of(e);
        let edit = self
            .can_propagate(err_ty)
            .then(|| self.edit(Span::empty(span.start), "try "));
        let d = self.err(
            "E0701",
            format!(
                "a value of the result type `{ok} ! {err}` is used here; handle it with `try`, \
                 `catch` or `match`"
            ),
            span,
        );
        if let Some(edit) = edit {
            d.fix("propagate the error with `try`", edit);
        }
        true
    }

    /// An expression of type `found` where `expected` is required (E0401 and the more
    /// specific codes that replace it).
    pub(crate) fn mismatch(&mut self, e: ExprId, found: TypeId, expected: TypeId) {
        let span = self.span_of(e);
        let (fk, ek) = (self.kind(found), self.kind(expected));
        match (&fk, &ek) {
            (TyKind::Ref(Mutability::Mut, a), TyKind::Ref(Mutability::Shared, b)) if a == b => {
                let text = self.text(span).to_string();
                let simple = matches!(
                    &self.hir.expr(e).kind,
                    ExprKind::Path(_) | ExprKind::Field { .. } | ExprKind::Index { .. }
                );
                let new = if simple {
                    format!("&{text}.*")
                } else {
                    format!("&({text}).*")
                };
                let edit = self.edit(span, new);
                self.err(
                    "E0419",
                    "an exclusive borrow does not convert to a shared one by itself; reborrow it with `&r.*`",
                    span,
                )
                .fix("reborrow as a shared borrow", edit);
                return;
            }
            (TyKind::Slice(Mutability::Mut, a), TyKind::Slice(Mutability::Shared, b)) if a == b => {
                let edit = self.edit(Span::empty(span.end), ".shared()");
                self.err(
                    "E0419",
                    "an exclusive slice does not convert to a shared one by itself; use `.shared()`",
                    span,
                )
                .fix("reborrow as a shared slice", edit);
                return;
            }
            (TyKind::Result(..), _) if !matches!(ek, TyKind::Result(..)) => {
                self.unhandled_result(e, found);
                return;
            }
            _ => {}
        }
        let (f, x) = (self.name(found), self.name(expected));
        let fix = if self.lossless_to(found, expected) {
            Some(self.append_as(e, expected))
        } else {
            None
        };
        let d = self.err("E0401", format!("expected `{x}`, found `{f}`"), span);
        if let Some(edits) = fix {
            d.fix_edits(format!("convert with `as {x}`"), edits);
        }
    }

    fn expr_inner(&mut self, e: ExprId, hint: Option<TypeId>) -> (TypeId, ExprRes) {
        let hir = self.hir;
        let expr = hir.expr(e);
        match &expr.kind {
            ExprKind::Int {
                value, negative, ..
            } => (self.int_literal(e, *value, *negative, hint), ExprRes::None),
            ExprKind::Float(f) => (self.float_literal(e, *f, hint), ExprRes::None),
            ExprKind::Str(_) => {
                let u8_ = self.mk(TyKind::Int(IntTy::U8));
                (
                    self.mk(TyKind::Slice(Mutability::Shared, u8_)),
                    ExprRes::None,
                )
            }
            ExprKind::CStr(_) => {
                let u8_ = self.mk(TyKind::Int(IntTy::U8));
                (
                    self.mk(TyKind::RawPtr(Mutability::Shared, u8_)),
                    ExprRes::None,
                )
            }
            ExprKind::Block(b) => {
                let tail =
                    self.hir
                        .block(*b)
                        .stmts
                        .last()
                        .and_then(|s| match &self.hir.stmt(*s).kind {
                            hir::StmtKind::Expr(e) => Some(self.span_of(*e)),
                            _ => None,
                        });
                let saved = self.branch_tail;
                self.branch_tail = tail;
                let completes = self.block(*b);
                self.branch_tail = saved;
                if completes {
                    let span = self.span_of(e);
                    self.err(
                        "E0207",
                        "this branch block can reach its end; it must end in `return`, `fail`, \
                         `break`, `continue` or a trap",
                        span,
                    );
                }
                (self.never(), ExprRes::None)
            }
            ExprKind::Bool(_) => (self.bool_(), ExprRes::None),
            ExprKind::Path(p) => self.path_value(e, p),
            ExprKind::Builtin {
                name,
                type_arg,
                args,
            } => self.builtin(e, name, type_arg.as_deref(), args, hint),
            ExprKind::Unary {
                op,
                expr: operand,
                op_span,
            } => self.unary(e, *op, *operand, *op_span, hint),
            ExprKind::Binary {
                op,
                lhs,
                rhs,
                op_span,
            } => self.binary(e, *op, *lhs, *rhs, *op_span, hint),
            ExprKind::Call {
                callee,
                args,
                args_span,
            } => self.call(e, *callee, args, *args_span, hint),
            ExprKind::Index {
                base,
                index,
                bracket,
            } => self.index(e, *base, *index, *bracket),
            ExprKind::Slice {
                base,
                lo,
                hi,
                bracket,
            } => self.slice(e, *base, *lo, *hi, *bracket, None),
            ExprKind::Field {
                base,
                name,
                name_span,
            } => self.field(e, *base, name, *name_span),
            ExprKind::TupleField { base, index } => self.tuple_field(e, *base, *index),
            ExprKind::Deref { base, op_span } => self.deref(e, *base, *op_span),
            ExprKind::TypeApply { base, args } => self.type_apply(e, *base, args),
            ExprKind::Cast {
                expr: operand,
                ty,
                as_span,
            } => self.cast(e, *operand, ty, *as_span),
            ExprKind::StructLit { ty, fields, braces } => {
                let t = self.lower_path(ty);
                self.struct_literal(e, t, fields, *braces)
            }
            ExprKind::StructShorthand { fields, braces } => match hint {
                Some(h) if !self.is_poison(h) => self.struct_literal(e, h, fields, *braces),
                Some(_) => {
                    self.check_field_values_blind(fields);
                    (self.poison(), ExprRes::None)
                }
                None => {
                    let span = self.span_of(e);
                    self.err(
                        "E0404",
                        "the type of `.{ .. }` cannot be inferred here; write the type before the brace",
                        span,
                    );
                    self.check_field_values_blind(fields);
                    (self.poison(), ExprRes::None)
                }
            },
            ExprKind::EnumLit {
                ty,
                variant,
                variant_span,
                payload,
            } => {
                let target = match ty {
                    Some(p) => Some(self.lower_path(p)),
                    None => hint,
                };
                self.enum_literal(e, target, variant, *variant_span, *payload)
            }
            ExprKind::ArrayLit { len, elem, elems } => self.array_literal(e, *len, elem, elems),
            ExprKind::Range { lo, hi, .. } => {
                // Ranges occur only as the domain of a `dispatch`, which types them.
                let (l, h) = self.operands(*lo, *hi, None);
                let _ = (l, h);
                self.ni("a range expression outside a `for`, a slice and a `dispatch` domain");
                (self.poison(), ExprRes::None)
            }
            ExprKind::Tuple(elems) => {
                let hints: Vec<Option<TypeId>> = match hint.map(|h| self.kind(h)) {
                    Some(TyKind::Tuple(ts)) if ts.len() == elems.len() => {
                        ts.into_iter().map(Some).collect()
                    }
                    _ => vec![None; elems.len()],
                };
                let ts: Vec<TypeId> = elems
                    .iter()
                    .zip(hints)
                    .map(|(x, h)| self.expr(*x, h))
                    .collect();
                if ts.iter().any(|t| self.is_poison(*t)) {
                    return (self.poison(), ExprRes::None);
                }
                (self.mk(TyKind::Tuple(ts)), ExprRes::None)
            }
            ExprKind::Paren(inner) => (self.expr(*inner, hint), ExprRes::None),
            ExprKind::If {
                cond,
                then,
                otherwise,
            } => {
                self.if_cond(cond);
                let first = self.branch(*then, hint);
                let second_hint = if self.is_never(first) {
                    hint
                } else {
                    Some(first)
                };
                let second = self.branch(*otherwise, second_hint.or(hint));
                let ty = if self.is_never(first) {
                    second
                } else {
                    if !self.compatible(second, first) {
                        self.mismatch(*otherwise, second, first);
                    }
                    first
                };
                (ty, ExprRes::None)
            }
            ExprKind::Match { scrutinee, arms } => {
                let kw = Span::new(expr.span.start, expr.span.start + 5);
                let ty = self.match_value(*scrutinee, arms, kw, hint);
                (ty, ExprRes::None)
            }
            ExprKind::Closure(c) => self.closure(e, c),
            ExprKind::Catch {
                value,
                binding,
                binding_span,
                handler,
                catch_span,
            } => self.catch_expr(*value, *binding, *binding_span, handler, hint, *catch_span),
            ExprKind::Orelse {
                value,
                handler,
                orelse_span,
            } => self.orelse_expr(*value, handler, hint, *orelse_span),
            ExprKind::Spawn { call, scope } => self.spawn(e, *call, *scope, true),
            ExprKind::Dispatch {
                call,
                domain,
                executor,
            } => self.dispatch(e, *call, *domain, *executor),
            ExprKind::Unsafe {
                reason,
                reason_span,
                body,
            } => {
                self.check_unsafe_reason(reason, *reason_span);
                self.unsafe_used.push(false);
                let t = self.expr(*body, hint);
                let used = self.unsafe_used.pop().expect("an unsafe block");
                if !used {
                    let kw = Span::new(expr.span.start, expr.span.start + 6);
                    self.err(
                        "E1304",
                        "this `unsafe` block contains no operation that needs it",
                        kw,
                    );
                }
                (t, ExprRes::None)
            }
            ExprKind::Poison => (self.poison(), ExprRes::None),
        }
    }

    /// The fields of a literal whose type is unknown: check the values so that their
    /// own errors are reported.
    fn check_field_values_blind(&mut self, fields: &[hir::FieldInit]) {
        for f in fields {
            self.blind_expr(f.value);
        }
    }

    // ------------------------------------------------------------------- names

    fn path_value(&mut self, e: ExprId, p: &hir::PathRef) -> (TypeId, ExprRes) {
        match p.res {
            Res::Poison => (self.poison(), ExprRes::None),
            Res::Local(l) => (self.local_ty(l), ExprRes::Local(l)),
            Res::Def(d) => {
                let kind = self.def_kind(d);
                match kind {
                    DefKind::Fn | DefKind::ExternFn => self.fn_value(e, d, Vec::new()),
                    DefKind::Const => {
                        self.cx.ensure_sig(d);
                        match self.cx.d.items.get(&d) {
                            Some(ItemSig::Const(c)) => (c.ty, ExprRes::Const(d)),
                            _ => (self.poison(), ExprRes::None),
                        }
                    }
                    DefKind::Static => {
                        self.cx.ensure_sig(d);
                        match self.cx.d.items.get(&d) {
                            Some(ItemSig::Static(s)) => (s.ty, ExprRes::Static(d)),
                            _ => (self.poison(), ExprRes::None),
                        }
                    }
                    DefKind::GenericParam { is_const: true } => {
                        let ty = self
                            .cx
                            .d
                            .param_const_ty
                            .get(&d)
                            .copied()
                            .unwrap_or(self.poison());
                        (ty, ExprRes::ConstParam(d))
                    }
                    DefKind::Poison => (self.poison(), ExprRes::None),
                    _ => {
                        let name = p
                            .segments
                            .last()
                            .map(|(n, _)| n.clone())
                            .unwrap_or_default();
                        let span = self.span_of(e);
                        self.err(
                            "E0304",
                            format!("`{name}` is a type, not a value; name one of its members as `{name}.member`"),
                            span,
                        );
                        (self.poison(), ExprRes::None)
                    }
                }
            }
        }
    }

    /// A function named as a value: a `fn` pointer. A generic function needs its
    /// arguments written (`f.[T]`).
    pub(crate) fn fn_value(
        &mut self,
        e: ExprId,
        d: DefId,
        explicit: Vec<GenericArg>,
    ) -> (TypeId, ExprRes) {
        self.fn_value_with(e, d, explicit, FnSource::Free, Subst::new())
    }

    /// A function named as a value, from a `fn` item or an inherent method of a type.
    fn fn_value_with(
        &mut self,
        e: ExprId,
        d: DefId,
        explicit: Vec<GenericArg>,
        source: FnSource,
        base: Subst,
    ) -> (TypeId, ExprRes) {
        let Some(sig) = self.fn_sig(d) else {
            return (self.poison(), ExprRes::None);
        };
        let generics = self.cx.d.generics_of(d).to_vec();
        if generics.len() != explicit.len() {
            if explicit.is_empty() {
                let span = self.span_of(e);
                self.err(
                    "E0803",
                    format!(
                        "the type arguments of `{}` cannot be inferred from a function value; \
                         write them with `.[..]`",
                        sig.name
                    ),
                    span,
                );
            } else {
                let span = self.span_of(e);
                self.err(
                    "E0804",
                    format!(
                        "`{}` takes {} generic argument(s) but {} {} given",
                        sig.name,
                        generics.len(),
                        explicit.len(),
                        if explicit.len() == 1 { "was" } else { "were" }
                    ),
                    span,
                );
            }
            return (self.poison(), ExprRes::None);
        }
        if sig.variadic {
            self.ni("a variadic function used as a value");
            return (self.poison(), ExprRes::None);
        }
        let mut s = base;
        let mut args = Vec::new();
        for (g, arg) in generics.iter().zip(&explicit) {
            match (g.is_const, arg) {
                (true, GenericArg::Const(c)) => {
                    s.insert(g.def, GenericArg::Const(*c));
                    args.push(GenericArg::Const(*c));
                }
                (false, GenericArg::Ty(t)) => {
                    s.insert(g.def, GenericArg::Ty(*t));
                    args.push(GenericArg::Ty(*t));
                }
                (true, GenericArg::Ty(_)) => {
                    self.err(
                        "E0813",
                        "this generic parameter is a constant: give it a value, not a type",
                        self.span_of(e),
                    );
                    return (self.poison(), ExprRes::None);
                }
                (false, GenericArg::Const(_)) => {
                    self.err(
                        "E0813",
                        "this generic parameter is a type: give it a type, not a constant",
                        self.span_of(e),
                    );
                    return (self.poison(), ExprRes::None);
                }
            }
        }
        // An `extern` function's value has the `extern fn` type, which says what it is.
        if self.is_kernel_sig(&sig) {
            let name = sig.name.clone();
            self.err(
                "E1515",
                format!("`{name}` is a kernel; it is run only by `dispatch`, not used as a value"),
                self.span_of(e),
            );
            return (self.poison(), ExprRes::None);
        }
        // An `unsafe fn` cannot be used as a value: the pointer could be called without
        // `unsafe` (section 15).
        if sig.mods.unsafe_ {
            let name = sig.name.clone();
            self.err(
                "E1305",
                format!("`{name}` is an `unsafe fn`; it can only be called, not used as a value"),
                self.span_of(e),
            );
            return (self.poison(), ExprRes::None);
        }
        let params: Vec<TypeId> = sig
            .params
            .iter()
            .map(|p| self.cx.d.instantiate(p.ty, &s))
            .collect();
        let ret = self.cx.d.instantiate(sig.ret, &s);
        let error = sig.error.map(|t| self.cx.d.instantiate(t, &s));
        let ext = sig.mods.export || self.def_kind(d) == DefKind::ExternFn;
        let ty = self.mk(TyKind::FnPtr(FnPtrSig {
            ext,
            params,
            ret,
            error,
        }));
        (
            ty,
            ExprRes::Fn(FnRef {
                def: d,
                args,
                source,
            }),
        )
    }

    /// Calling an `unsafe fn` or an `extern` function needs `unsafe` (E1301, section 15).
    pub(crate) fn note_unsafe_callee(&mut self, sig: &FnSig, span: Span) {
        if sig.mods.unsafe_ || self.def_kind(sig.def) == DefKind::ExternFn {
            let what = if sig.mods.unsafe_ {
                "calling an `unsafe fn`"
            } else {
                "calling an `extern` function"
            };
            self.requires_unsafe(span, what);
        }
    }

    fn type_apply(
        &mut self,
        e: ExprId,
        base: ExprId,
        args: &[hir::GenericArg],
    ) -> (TypeId, ExprRes) {
        let b = self.unparen(base);
        if let ExprKind::Path(p) = &self.hir.expr(b).kind
            && let Res::Def(d) = p.res
            && matches!(self.def_kind(d), DefKind::Fn | DefKind::ExternFn)
        {
            let Some(lowered) = self.lower_explicit_args(args, d) else {
                return (self.poison(), ExprRes::None);
            };
            let (ty, res) = self.fn_value(e, d, lowered);
            self.set(b, ty, res.clone());
            return (ty, res);
        }
        if let ExprKind::Field {
            base: tb,
            name,
            name_span,
        } = &self.hir.expr(b).kind
            && let Some(t) = self.type_of_path_expr(*tb)
        {
            let (ty, res) = self.type_member_with(b, t, name, *name_span, Some(args));
            self.set(b, ty, res.clone());
            return (ty, res);
        }
        self.ni("explicit type arguments `.[..]` on something other than a function name");
        (self.poison(), ExprRes::None)
    }

    // ----------------------------------------------------------------- members

    /// E0314: no member of that name. The fix replaces a name within edit distance 2.
    pub(crate) fn unknown_member(
        &mut self,
        name: &str,
        name_span: Span,
        candidates: &[String],
        on: TypeId,
    ) {
        let near = unique_near(name, candidates.iter().map(String::as_str));
        let tname = self.name(on);
        let edit = near.as_ref().map(|n| self.edit(name_span, n.clone()));
        let d = self.err(
            "E0314",
            format!("`{tname}` has no member `{name}`"),
            name_span,
        );
        if let (Some(n), Some(edit)) = (near, edit) {
            d.fix(format!("replace `{name}` with `{n}`"), edit);
        }
    }

    /// E0802: an operation the bounds of a type parameter do not grant.
    pub(crate) fn missing_bound(&mut self, t: TypeId, span: Span, what: &str) {
        let name = self.name(t);
        let param = match self.kind(t) {
            TyKind::Param(d) => Some(d),
            _ => None,
        };
        self.err(
            "E0802",
            format!("{what} is not granted by the bounds of `{name}`"),
            span,
        );
        if let Some(p) = param {
            let decl = self.cx.sema.def(p).span;
            let file = self.cx.sources.get(self.cx.file_of(p)).path.clone();
            self.cx_diag_related(&file, decl);
        }
    }

    fn cx_diag_related(&mut self, file: &str, span: Span) {
        if let Some(d) = self.cx.d.diagnostics.last_mut() {
            d.relate_in("declared", file, span);
        }
    }

    /// The position of the `.` before a member name.
    pub(crate) fn dot_before(&self, name_span: Span) -> usize {
        let text = self.cx.sources.get(self.file).text.as_bytes();
        let mut i = name_span.start;
        while i > 0 {
            i -= 1;
            if text[i] == b'.' {
                return i;
            }
            if !text[i].is_ascii_whitespace() {
                break;
            }
        }
        name_span.start.saturating_sub(1)
    }

    fn raw_pointer_member(&mut self, at: usize, what: &str) {
        let span = Span::new(at, at + 1);
        let edit = self.edit(Span::empty(at), ".*");
        self.note_raw_access();
        self.err(
            "E0418",
            format!("{what} on a raw pointer: write `.*` first"),
            span,
        )
        .fix("dereference the pointer first", edit);
    }

    fn field(&mut self, e: ExprId, base: ExprId, name: &str, name_span: Span) -> (TypeId, ExprRes) {
        if self.is_target_builtin(base) {
            let ty = self.target_field(name);
            return (ty, ExprRes::None);
        }
        // `Type.member`: a variant, an associated function or constant.
        if let Some(t) = self.type_of_path_expr(base) {
            return self.type_member(e, t, name, name_span);
        }
        let bt = self.expr(base, None);
        if self.is_poison(bt) || self.is_never(bt) {
            return (self.poison(), ExprRes::None);
        }
        let (t, derefs) = self.strip_refs(bt);
        match self.kind(t) {
            TyKind::RawPtr(..) => {
                let at = self.dot_before(name_span);
                self.raw_pointer_member(at, "a member access");
                (self.poison(), ExprRes::None)
            }
            TyKind::Adt(def, args) => {
                let Some(sig) = self.cx.d.struct_sig(def).cloned() else {
                    if matches!(self.def_kind(def), DefKind::Builtin(_)) {
                        let tname = self.name(t);
                        self.ni(format!(
                            "the field `{tname}.{name}` of a built-in type (the specification \
                             declares none)"
                        ));
                    } else {
                        self.unknown_member(name, name_span, &[], t);
                    }
                    return (self.poison(), ExprRes::None);
                };
                let Some((index, f)) = sig.fields.iter().enumerate().find(|(_, f)| f.name == name)
                else {
                    if self.method_without_call(t, name, name_span) {
                        return (self.poison(), ExprRes::None);
                    }
                    let names: Vec<String> = sig.fields.iter().map(|f| f.name.clone()).collect();
                    self.unknown_member(name, name_span, &names, t);
                    return (self.poison(), ExprRes::None);
                };
                let f = f.clone();
                if !self.field_visible(def, &f) {
                    let fpath = self.cx.sources.get(self.cx.file_of(def)).path.clone();
                    let message = if f.pkg {
                        format!(
                            "the field `{name}` is `pub(pkg)`: it is not visible outside its package"
                        )
                    } else {
                        format!("the field `{name}` is private to its module")
                    };
                    self.err("E0307", message, name_span).relate_in(
                        "declared",
                        &fpath,
                        f.name_span,
                    );
                    return (self.poison(), ExprRes::None);
                }
                let s = self.cx.d.subst_for(def, &args);
                let fty = self.cx.d.instantiate(f.ty, &s);
                (
                    fty,
                    ExprRes::Field {
                        derefs,
                        struct_def: def,
                        index,
                        def: f.def,
                    },
                )
            }
            TyKind::Slice(m, elem) => match name {
                "len" => (self.i64_(), ExprRes::SliceLen { derefs }),
                "ptr" => (
                    self.mk(TyKind::RawPtr(m, elem)),
                    ExprRes::SlicePtr { derefs },
                ),
                _ => {
                    if self.method_without_call(t, name, name_span) {
                        return (self.poison(), ExprRes::None);
                    }
                    let cands = vec!["len".to_string(), "ptr".to_string()];
                    self.unknown_member(name, name_span, &cands, t);
                    (self.poison(), ExprRes::None)
                }
            },
            TyKind::Array(_, len) if name == "len" => {
                (self.i64_(), ExprRes::ArrayLen { derefs, len })
            }
            TyKind::Poison => (self.poison(), ExprRes::None),
            _ => {
                if self.method_without_call(t, name, name_span) {
                    return (self.poison(), ExprRes::None);
                }
                self.unknown_member(name, name_span, &[], t);
                (self.poison(), ExprRes::None)
            }
        }
    }

    /// `value.method` without a call: E0314, with the message saying to write a
    /// closure (section 4.4).
    fn method_without_call(&mut self, t: TypeId, name: &str, name_span: Span) -> bool {
        if self.cx.d.method_candidates(t, name).is_empty() {
            return false;
        }
        let tname = self.name(t);
        self.err(
            "E0314",
            format!("`{name}` is a method of `{tname}`, not a value; write a closure"),
            name_span,
        );
        true
    }

    fn tuple_field(&mut self, e: ExprId, base: ExprId, index: u64) -> (TypeId, ExprRes) {
        let bt = self.expr(base, None);
        if self.is_poison(bt) || self.is_never(bt) {
            return (self.poison(), ExprRes::None);
        }
        let (t, derefs) = self.strip_refs(bt);
        match self.kind(t) {
            TyKind::Tuple(ts) if (index as usize) < ts.len() => {
                (ts[index as usize], ExprRes::TupleField { derefs })
            }
            TyKind::RawPtr(..) => {
                let span = self.span_of(e);
                let at = span.end - index.to_string().len() - 1;
                self.raw_pointer_member(at, "a member access");
                (self.poison(), ExprRes::None)
            }
            TyKind::Poison => (self.poison(), ExprRes::None),
            _ => {
                let span = self.span_of(e);
                let at = Span::new(span.end - index.to_string().len(), span.end);
                self.unknown_member(&index.to_string(), at, &[], t);
                (self.poison(), ExprRes::None)
            }
        }
    }

    fn deref(&mut self, e: ExprId, base: ExprId, op_span: Span) -> (TypeId, ExprRes) {
        let _ = e;
        let bt = self.expr(base, None);
        if self.is_poison(bt) || self.is_never(bt) {
            return (self.poison(), ExprRes::None);
        }
        match self.kind(bt) {
            TyKind::Ref(_, t) => (t, ExprRes::Deref { raw: false }),
            TyKind::RawPtr(_, t) => {
                self.requires_unsafe(op_span, "dereferencing a raw pointer");
                (t, ExprRes::Deref { raw: true })
            }
            _ => {
                let name = self.name(bt);
                self.err(
                    "E0406",
                    format!("`.*` is defined for borrows and raw pointers, not `{name}`"),
                    op_span,
                );
                (self.poison(), ExprRes::None)
            }
        }
    }

    /// An index or slice bound: `i64`, or E0415.
    pub(crate) fn index_operand(&mut self, e: ExprId) {
        let i64_ = self.i64_();
        let t = self.expr(e, Some(i64_));
        if self.compatible(t, i64_) {
            return;
        }
        // The index of a kernel (section 27.1) indexes slices and grids.
        if self.is_idx_type(t) {
            if !self.kernel {
                self.err(
                    "E1513",
                    "`Idx` appears outside a kernel; an `Idx` indexes a kernel input or output \
                     only at the kernel's own index",
                    self.span_of(e),
                );
            }
            return;
        }
        if self.unhandled_result(e, t) {
            return;
        }
        let span = self.span_of(e);
        let name = self.name(t);
        let fix = if self.lossless_to(t, i64_) {
            Some(self.append_as(e, i64_))
        } else {
            None
        };
        let d = self.err(
            "E0415",
            format!("an index or slice bound is an `i64`, not `{name}`"),
            span,
        );
        if let Some(edits) = fix {
            d.fix_edits("convert with `as i64`", edits);
        }
    }

    /// E1502: a vector lane index or `@insert` lane must be a constant in `0..lanes`.
    /// The index has already been typed (`index_operand`).
    pub(crate) fn check_lane_index(&mut self, index: ExprId, lanes: u32) -> bool {
        if !self.compatible(self.ty_of(index), self.i64_()) {
            return false;
        }
        let span = self.span_of(index);
        let out_of_range = || format!("the lane index must be a constant in `0..{lanes}`");
        if !crate::consteval::runtime_reads(self.hir, self.cx.sema, index).is_empty() {
            self.err("E1502", out_of_range(), span);
            return false;
        }
        match self.cx.eval_expr(self.pkg, self.file, index, None) {
            Ok(ConstValue::Int { value, .. }) if value >= 0 && value < i128::from(lanes) => true,
            Ok(_) => {
                self.err("E1502", out_of_range(), span);
                false
            }
            Err(crate::consteval::Fail::Reported) => {
                self.errors += 1;
                false
            }
            Err(crate::consteval::Fail::OutOfRange(_)) => {
                self.err("E1502", out_of_range(), span);
                false
            }
            Err(crate::consteval::Fail::NotImplemented(reason))
            | Err(crate::consteval::Fail::Interpret(reason)) => {
                self.ni(format!("{reason} (lane index)"));
                false
            }
            Err(crate::consteval::Fail::Generic) => {
                self.ni("a lane index that depends on a generic parameter");
                false
            }
        }
    }

    fn index(
        &mut self,
        e: ExprId,
        base: ExprId,
        index: ExprId,
        bracket: Span,
    ) -> (TypeId, ExprRes) {
        let _ = e;
        let bt = self.expr(base, None);
        self.index_operand(index);
        if self.is_poison(bt) || self.is_never(bt) {
            return (self.poison(), ExprRes::None);
        }
        let (t, derefs) = self.strip_refs(bt);
        let at = Span::new(bracket.start, bracket.start + 1);
        match self.kind(t) {
            TyKind::Array(el, _) | TyKind::Slice(_, el) => (el, ExprRes::Index { derefs }),
            TyKind::Vector(lane, n) => {
                self.check_lane_index(index, n);
                let el = match lane {
                    Lane::Int(i) => self.mk(TyKind::Int(i)),
                    Lane::Float(f) => self.mk(TyKind::Float(f)),
                };
                (el, ExprRes::Index { derefs })
            }
            TyKind::RawPtr(..) => {
                let edit = self.edit(Span::empty(bracket.start), ".*");
                self.note_raw_access();
                self.err("E0418", "indexing a raw pointer: write `.*` first", at)
                    .fix("dereference the pointer first", edit);
                (self.poison(), ExprRes::None)
            }
            TyKind::Poison => (self.poison(), ExprRes::None),
            _ => {
                let name = self.name(t);
                self.err(
                    "E0416",
                    format!("`{name}` cannot be indexed: only arrays, slices and vectors can"),
                    at,
                );
                (self.poison(), ExprRes::None)
            }
        }
    }

    /// `s[lo..hi]`. `borrow` is the `&` or `&mut` written around it, which is what makes
    /// a slice of an array legal.
    pub(crate) fn slice(
        &mut self,
        e: ExprId,
        base: ExprId,
        lo: Option<ExprId>,
        hi: Option<ExprId>,
        bracket: Span,
        borrow: Option<Mutability>,
    ) -> (TypeId, ExprRes) {
        let bt = self.expr(base, None);
        if let Some(lo) = lo {
            self.index_operand(lo);
        }
        if let Some(hi) = hi {
            self.index_operand(hi);
        }
        let implicit_lo = lo.is_none();
        let implicit_hi = hi.is_none();
        if self.is_poison(bt) || self.is_never(bt) {
            return (self.poison(), ExprRes::None);
        }
        let (t, derefs) = self.strip_refs(bt);
        let at = Span::new(bracket.start, bracket.start + 1);
        match self.kind(t) {
            TyKind::Slice(m, el) => {
                let ty = self.mk(TyKind::Slice(m, el));
                (
                    ty,
                    ExprRes::Slice {
                        derefs,
                        of_array: false,
                        implicit_lo,
                        implicit_hi,
                    },
                )
            }
            TyKind::Array(el, _) => match borrow {
                Some(m) => {
                    let ty = self.mk(TyKind::Slice(m, el));
                    (
                        ty,
                        ExprRes::Slice {
                            derefs,
                            of_array: true,
                            implicit_lo,
                            implicit_hi,
                        },
                    )
                }
                None => {
                    let span = self.span_of(e);
                    let edit = self.edit(Span::empty(span.start), "&");
                    self.err(
                        "E0420",
                        "a slice of an array is a borrow: write `&a[lo..hi]` or `&mut a[lo..hi]`",
                        span,
                    )
                    .fix("borrow the array", edit);
                    (self.poison(), ExprRes::None)
                }
            },
            TyKind::RawPtr(..) => {
                let edit = self.edit(Span::empty(bracket.start), ".*");
                self.note_raw_access();
                self.err("E0418", "slicing a raw pointer: write `.*` first", at)
                    .fix("dereference the pointer first", edit);
                (self.poison(), ExprRes::None)
            }
            TyKind::Poison => (self.poison(), ExprRes::None),
            _ => {
                let name = self.name(t);
                self.err(
                    "E0416",
                    format!("`{name}` cannot be sliced: only arrays and slices can"),
                    at,
                );
                (self.poison(), ExprRes::None)
            }
        }
    }

    // ---------------------------------------------------------- type members

    /// The type a `Path` expression names, when it names a type used before a member
    /// (`Vec3.new`, `Color.red`, `List[u8, Heap].init`).
    pub(crate) fn type_of_path_expr(&mut self, base: ExprId) -> Option<TypeId> {
        let b = self.unparen(base);
        let ExprKind::Path(p) = &self.hir.expr(b).kind else {
            return None;
        };
        let Res::Def(d) = p.res else {
            return None;
        };
        match self.def_kind(d) {
            DefKind::Struct
            | DefKind::Enum
            | DefKind::TypeAlias
            | DefKind::SelfTy
            | DefKind::GenericParam { is_const: false }
            | DefKind::Builtin(
                Builtin::Scalar(_) | Builtin::Vector(_) | Builtin::Option | Builtin::Result,
            ) => {
                let t = self.lower_path(p);
                self.set(b, t, ExprRes::None);
                Some(t)
            }
            _ => None,
        }
    }

    /// `Type.name` as a value: a variant, an associated constant or a function.
    fn type_member(
        &mut self,
        e: ExprId,
        t: TypeId,
        name: &str,
        name_span: Span,
    ) -> (TypeId, ExprRes) {
        self.type_member_with(e, t, name, name_span, None)
    }

    /// The same, with explicit generic arguments (`Type.name.[..]`).
    fn type_member_with(
        &mut self,
        e: ExprId,
        t: TypeId,
        name: &str,
        name_span: Span,
        explicit: Option<&[hir::GenericArg]>,
    ) -> (TypeId, ExprRes) {
        let _ = e;
        if self.is_poison(t) {
            return (self.poison(), ExprRes::None);
        }
        if let Some(v) = self.variant_named(t, name) {
            if explicit.is_some() {
                let tname = self.name(t);
                self.err(
                    "E0805",
                    format!("`{tname}.{name}` is not a function and takes no type arguments"),
                    name_span,
                );
                return (self.poison(), ExprRes::None);
            }
            return match v.payload {
                None => (v.enum_ty, ExprRes::Variant(v)),
                Some(_) => {
                    self.err(
                        "E0410",
                        "a variant with a payload is not a function value; write `T.variant(..)`",
                        name_span,
                    );
                    (self.poison(), ExprRes::None)
                }
            };
        }
        // An associated constant of an inherent impl.
        for m in self.cx.d.inherent_impls(t) {
            let Some(sig) = self.cx.d.impl_sig(m.impl_def).cloned() else {
                continue;
            };
            if let Some(&c) = sig.consts.get(name) {
                if explicit.is_some() {
                    let tname = self.name(t);
                    self.err(
                        "E0805",
                        format!("`{tname}.{name}` is not a function and takes no type arguments"),
                        name_span,
                    );
                    return (self.poison(), ExprRes::None);
                }
                self.cx.ensure_sig(c);
                if let Some(ItemSig::Const(cs)) = self.cx.d.items.get(&c).cloned() {
                    let ty = self.cx.d.instantiate(cs.ty, &m.subst);
                    return (ty, ExprRes::Const(c));
                }
            }
        }
        if let Some(v) = self.inherent_method_value(e, t, name, explicit) {
            return v;
        }
        let cands = self.cx.d.method_candidates(t, name);
        if !cands.is_empty() {
            self.ni("a method named as a function value (`Type.method` without a call)");
            return (self.poison(), ExprRes::None);
        }
        self.unknown_function(t, name, name_span);
        (self.poison(), ExprRes::None)
    }

    /// `T.name` naming an inherent function or method: a `fn` value whose first
    /// parameter is `self` when the function is a method (section 4.4).
    fn inherent_method_value(
        &mut self,
        e: ExprId,
        t: TypeId,
        name: &str,
        explicit: Option<&[hir::GenericArg]>,
    ) -> Option<(TypeId, ExprRes)> {
        let mut found = None;
        for m in self.cx.d.inherent_impls(t) {
            let Some(sig) = self.cx.d.impl_sig(m.impl_def).cloned() else {
                continue;
            };
            if let Some(&method) = sig.methods.get(name) {
                found = Some((method, m));
                break;
            }
        }
        let (method, m) = found?;
        self.cx.ensure_sig(method);
        let c = MethodCandidate {
            method,
            source: MethodSource::Inherent(m),
        };
        let (source, subst, _) = self.fn_ref_from(&c, t);
        let explicit = match explicit {
            Some(args) => match self.lower_explicit_args(args, method) {
                Some(v) => v,
                None => return Some((self.poison(), ExprRes::None)),
            },
            None => Vec::new(),
        };
        Some(self.fn_value_with(e, method, explicit, source, subst))
    }

    /// `Type.name` or `value.name(..)` found nothing. A type the language or the
    /// standard library outline declares without the function is not the program's
    /// error: the function is simply not written yet.
    pub(crate) fn unknown_function(&mut self, t: TypeId, name: &str, name_span: Span) {
        if let TyKind::Adt(d, _) = self.kind(t) {
            let def = self.cx.sema.def(d);
            let builtin = matches!(def.kind, DefKind::Builtin(_));
            let in_std = !builtin && {
                let p = self.cx.sema.module(def.module).package.0 as usize;
                p != self.pkg && self.cx.pkgs[p].name == "std"
            };
            if builtin || in_std {
                let tname = self.name(t);
                self.ni(format!(
                    "the function `{tname}.{name}` (the standard library outline and the \
                     built-in types declare no such function yet)"
                ));
                return;
            }
        }
        let names = self.member_names(t);
        self.unknown_member(name, name_span, &names, t);
    }

    /// The names of the variants and inherent functions of a type, for the fix of E0314.
    fn member_names(&mut self, t: TypeId) -> Vec<String> {
        let mut out = Vec::new();
        match self.kind(t) {
            TyKind::Adt(d, _) => {
                if let Some(e) = self.cx.d.enum_sig(d) {
                    out.extend(e.variants.iter().map(|v| v.name.clone()));
                }
            }
            TyKind::Optional(_) => out.extend(["none".to_string(), "some".to_string()]),
            TyKind::Result(..) => out.extend(["ok".to_string(), "err".to_string()]),
            _ => {}
        }
        for m in self.cx.d.inherent_impls(t) {
            if let Some(s) = self.cx.d.impl_sig(m.impl_def) {
                out.extend(s.methods.keys().cloned());
            }
        }
        out
    }

    /// The variant of an enum-like type (`enum`, `?T`, `T ! E`) with the given name.
    pub(crate) fn variant_named(&mut self, t: TypeId, name: &str) -> Option<VariantRef> {
        match self.kind(t) {
            TyKind::Adt(d, args) => {
                let sig = self.cx.d.enum_sig(d)?.clone();
                let (index, v) = sig
                    .variants
                    .iter()
                    .enumerate()
                    .find(|(_, v)| v.name == name)?;
                let s = self.cx.d.subst_for(d, &args);
                let payload = v.payload.map(|p| self.cx.d.instantiate(p, &s));
                Some(VariantRef {
                    enum_ty: t,
                    index,
                    def: Some(v.def),
                    payload,
                })
            }
            TyKind::Optional(inner) => match name {
                "none" => Some(VariantRef {
                    enum_ty: t,
                    index: 0,
                    def: None,
                    payload: None,
                }),
                "some" => Some(VariantRef {
                    enum_ty: t,
                    index: 1,
                    def: None,
                    payload: Some(inner),
                }),
                _ => None,
            },
            TyKind::Result(ok, err) => match name {
                "ok" => Some(VariantRef {
                    enum_ty: t,
                    index: 0,
                    def: None,
                    payload: Some(ok),
                }),
                "err" => Some(VariantRef {
                    enum_ty: t,
                    index: 1,
                    def: None,
                    payload: Some(err),
                }),
                _ => None,
            },
            _ => None,
        }
    }

    /// Whether `t` is an enum-like type that has variants.
    pub(crate) fn is_enum_like(&self, t: TypeId) -> bool {
        match self.cx.d.types.kind(t) {
            TyKind::Adt(d, _) => matches!(self.cx.d.items.get(d), Some(ItemSig::Enum(_))),
            TyKind::Optional(_) | TyKind::Result(..) => true,
            _ => false,
        }
    }

    // ------------------------------------------------------------ enum values

    fn enum_literal(
        &mut self,
        e: ExprId,
        target: Option<TypeId>,
        variant: &str,
        variant_span: Span,
        payload: Option<ExprId>,
    ) -> (TypeId, ExprRes) {
        let span = self.span_of(e);
        let Some(t) = target else {
            self.err(
                "E0404",
                format!("the enum of `.{variant}` cannot be inferred here; name it, as in `Type.{variant}`"),
                span,
            );
            if let Some(p) = payload {
                self.blind_expr(p);
            }
            return (self.poison(), ExprRes::None);
        };
        if self.is_poison(t) {
            if let Some(p) = payload {
                self.blind_expr(p);
            }
            return (self.poison(), ExprRes::None);
        }
        if !self.is_enum_like(t) {
            let name = self.name(t);
            self.err(
                "E0401",
                format!("expected `{name}`, found the enum value `.{variant}`"),
                span,
            );
            if let Some(p) = payload {
                self.blind_expr(p);
            }
            return (self.poison(), ExprRes::None);
        }
        let Some(v) = self.variant_named(t, variant) else {
            let names = self.member_names(t);
            self.unknown_member(variant, variant_span, &names, t);
            if let Some(p) = payload {
                self.blind_expr(p);
            }
            return (self.poison(), ExprRes::None);
        };
        match (v.payload, payload) {
            (Some(pt), Some(p)) => {
                self.expect(p, pt);
            }
            (None, None) => {}
            (Some(_), None) => {
                self.err(
                    "E0410",
                    format!("the variant `{variant}` carries a payload: write `.{variant}(value)`"),
                    span,
                );
                return (self.poison(), ExprRes::None);
            }
            (None, Some(p)) => {
                self.blind_expr(p);
                self.err(
                    "E0410",
                    format!("the variant `{variant}` has no payload"),
                    span,
                );
                return (self.poison(), ExprRes::None);
            }
        }
        (t, ExprRes::Variant(v))
    }

    // ----------------------------------------------------------- struct values

    fn struct_literal(
        &mut self,
        e: ExprId,
        t: TypeId,
        fields: &[hir::FieldInit],
        braces: Span,
    ) -> (TypeId, ExprRes) {
        let span = self.span_of(e);
        if self.is_poison(t) {
            self.check_field_values_blind(fields);
            return (self.poison(), ExprRes::None);
        }
        let TyKind::Adt(def, args) = self.kind(t) else {
            let name = self.name(t);
            self.err(
                "E0401",
                format!("a struct literal needs a struct type, not `{name}`"),
                span,
            );
            self.check_field_values_blind(fields);
            return (self.poison(), ExprRes::None);
        };
        let Some(sig) = self.cx.d.struct_sig(def).cloned() else {
            let name = self.name(t);
            if matches!(self.cx.d.items.get(&def), Some(ItemSig::Enum(_))) {
                self.err(
                    "E0401",
                    format!("a struct literal needs a struct type, not the enum `{name}`"),
                    span,
                );
            } else {
                self.ni(format!("a struct literal of the built-in type `{name}`"));
            }
            self.check_field_values_blind(fields);
            return (self.poison(), ExprRes::None);
        };
        // E0315: construction respects privacy (section 4.2).
        let foreign = self.cx.sema.def(def).module != self.module;
        let hidden_field = sig.fields.iter().find(|f| !self.field_visible(def, f));
        let hidden = foreign && (sig.fields.is_empty() || hidden_field.is_some());
        if hidden {
            let name = self.name(t);
            let at = hidden_field.map(|f| f.name_span).unwrap_or(sig.span);
            let dpath = self.cx.sources.get(self.cx.file_of(def)).path.clone();
            self.err(
                "E0315",
                format!(
                    "`{name}` cannot be built here: it has a field that is not visible, or no fields"
                ),
                span,
            )
            .relate_in("declared", &dpath, at);
        }
        let s = self.cx.d.subst_for(def, &args);
        let mut order = Vec::new();
        let mut seen: Vec<usize> = Vec::new();
        let mut bad = hidden;
        for f in fields {
            match sig.fields.iter().position(|x| x.name == f.name) {
                None => {
                    self.err(
                        "E0503",
                        format!("`{}` has no field `{}`", self.name(t), f.name),
                        f.name_span,
                    );
                    self.blind_expr(f.value);
                    bad = true;
                }
                Some(i) => {
                    if seen.contains(&i) {
                        self.err(
                            "E0503",
                            format!("the field `{}` is given twice", f.name),
                            f.name_span,
                        );
                        self.blind_expr(f.value);
                        bad = true;
                        continue;
                    }
                    seen.push(i);
                    order.push(i);
                    let fty = self.cx.d.instantiate(sig.fields[i].ty, &s);
                    self.expect(f.value, fty);
                }
            }
        }
        let missing: Vec<String> = sig
            .fields
            .iter()
            .enumerate()
            .filter(|(i, _)| !seen.contains(i))
            .map(|(_, f)| f.name.clone())
            .collect();
        if !hidden && !missing.is_empty() {
            self.err(
                "E0503",
                format!("the literal omits the field(s) {}", missing.join(", ")),
                braces,
            );
            bad = true;
        }
        let _ = bad;
        (t, ExprRes::Struct(StructRes { def, args, order }))
    }

    fn array_literal(
        &mut self,
        e: ExprId,
        len: ExprId,
        elem: &hir::Ty,
        elems: &[ExprId],
    ) -> (TypeId, ExprRes) {
        let span = self.span_of(e);
        let et = self.lower(elem);
        let n = self
            .cx
            .const_arg(len, crate::consteval::ValTy::Int(IntTy::I64));
        for x in elems {
            if self.is_poison(et) {
                self.expr(*x, None);
            } else {
                self.expect(*x, et);
            }
        }
        if self.is_poison(et) || n == ConstArg::Poison {
            return (self.poison(), ExprRes::None);
        }
        if let Some(count) = n.as_len()
            && count != elems.len() as i128
        {
            self.err(
                "E0401",
                format!(
                    "an array of length {count} is given {} element(s)",
                    elems.len()
                ),
                span,
            );
            return (self.poison(), ExprRes::None);
        }
        (self.mk(TyKind::Array(et, n)), ExprRes::None)
    }

    // ------------------------------------------------------- if, match, catch

    /// The condition of `if` and `while`, with its `let` pattern.
    pub(crate) fn if_cond(&mut self, c: &hir::Cond) {
        match c.let_pattern {
            None => self.cond(c.expr),
            Some(p) => {
                let t = self.expr(c.expr, None);
                self.pattern(p, t);
            }
        }
    }

    /// A `match` used as a value.
    fn match_value(
        &mut self,
        scrutinee: ExprId,
        arms: &[hir::Arm],
        kw: Span,
        hint: Option<TypeId>,
    ) -> TypeId {
        let st = self.expr(scrutinee, None);
        let mut result: Option<TypeId> = None;
        for arm in arms {
            self.arm_patterns(arm, st);
            if let Some(g) = arm.guard {
                self.cond(g);
            }
            match &arm.body {
                hir::ArmBody::Expr(x) => {
                    let t = self.branch(*x, result.or(hint));
                    if !self.is_never(t) {
                        match result {
                            None => result = Some(t),
                            Some(r) => {
                                if !self.compatible(t, r) {
                                    self.mismatch(*x, t, r);
                                }
                            }
                        }
                    }
                }
                hir::ArmBody::Block(b) => {
                    let completes = self.block(*b);
                    if completes {
                        let span = self.hir.block(*b).span;
                        self.err(
                            "E0207",
                            "this arm block can reach its end; it must end in `return`, `fail`, \
                             `break`, `continue` or a trap",
                            span,
                        );
                    }
                }
            }
        }
        self.exhaustiveness(st, arms, kw, scrutinee);
        result.unwrap_or(self.never())
    }

    fn catch_expr(
        &mut self,
        value: ExprId,
        binding: Option<hir::LocalId>,
        binding_span: Option<Span>,
        handler: &Handler,
        hint: Option<TypeId>,
        catch_span: Span,
    ) -> (TypeId, ExprRes) {
        let vt = self.expr(value, None);
        let (ok, err) = match self.kind(vt) {
            TyKind::Result(ok, err) => (ok, err),
            TyKind::Poison => {
                self.catch_handler_blind(binding, handler);
                return (self.poison(), ExprRes::None);
            }
            _ => {
                let n = self.name(vt);
                self.err(
                    "E0704",
                    format!("`catch` needs a result (`T ! E`), not `{n}`"),
                    catch_span,
                );
                self.catch_handler_blind(binding, handler);
                return (self.poison(), ExprRes::None);
            }
        };
        if let Some(b) = binding {
            self.bind_local(b, err, BindMode::Move);
        } else if let Some(span) = binding_span {
            let p = self.cx.d.props(err);
            match p.discard() {
                Tri::No => {
                    let name = self.name(err);
                    self.err(
                        "E0414",
                        format!("`catch _` cannot discard a linear error of type `{name}`"),
                        span,
                    );
                }
                Tri::Unknown => self.ni(
                    "whether a `catch _` may discard an error of a type whose properties the \
                     specification does not define",
                ),
                Tri::Yes => {}
            }
        }
        self.handler(handler, ok, hint, "`catch`");
        (ok, ExprRes::None)
    }

    fn catch_handler_blind(&mut self, binding: Option<hir::LocalId>, handler: &Handler) {
        let poison = self.poison();
        if let Some(b) = binding {
            self.bind_local(b, poison, BindMode::Move);
        }
        match handler {
            Handler::Expr(x) => {
                self.blind_expr(*x);
            }
            Handler::Block(b) => {
                self.block(*b);
            }
        }
    }

    fn orelse_expr(
        &mut self,
        value: ExprId,
        handler: &Handler,
        hint: Option<TypeId>,
        orelse_span: Span,
    ) -> (TypeId, ExprRes) {
        let vt = self.expr(value, None);
        let some = match self.kind(vt) {
            TyKind::Optional(t) => t,
            TyKind::Poison => {
                self.catch_handler_blind(None, handler);
                return (self.poison(), ExprRes::None);
            }
            _ => {
                let n = self.name(vt);
                self.err(
                    "E0704",
                    format!("`orelse` needs an optional (`?T`), not `{n}`"),
                    orelse_span,
                );
                self.catch_handler_blind(None, handler);
                return (self.poison(), ExprRes::None);
            }
        };
        self.handler(handler, some, hint, "`orelse`");
        (some, ExprRes::None)
    }

    /// The handler of `catch` or `orelse`: an expression of the success type, or a
    /// block that must leave unless the success type is `void`.
    fn handler(&mut self, h: &Handler, ok: TypeId, hint: Option<TypeId>, what: &str) {
        match h {
            Handler::Expr(x) => {
                let t = self.expr(*x, Some(ok).or(hint));
                if !self.compatible(t, ok) {
                    self.mismatch(*x, t, ok);
                }
            }
            Handler::Block(b) => {
                let completes = self.block(*b);
                if completes && ok != self.void() && !self.is_poison(ok) {
                    let span = self.hir.block(*b).span;
                    let at = Span::new(span.end.saturating_sub(1), span.end);
                    self.err(
                        "E0707",
                        format!(
                            "this {what} block can reach its end although a value is needed; \
                             leave it with `return`, `fail`, `break`, `continue` or a trap"
                        ),
                        at,
                    );
                }
            }
        }
    }

    /// `try e`: the success value, failing the enclosing function with the error.
    pub(crate) fn try_expr(
        &mut self,
        e: ExprId,
        operand: ExprId,
        op_span: Span,
    ) -> (TypeId, ExprRes) {
        let _ = e;
        let t = self.expr(operand, None);
        let (ok, err) = match self.kind(t) {
            TyKind::Result(ok, err) => (ok, err),
            TyKind::Poison => return (self.poison(), ExprRes::None),
            _ => {
                self.err(
                    "E0704",
                    "`try` needs a result (`T ! E`), not this value",
                    op_span,
                );
                return (self.poison(), ExprRes::None);
            }
        };
        let ctx = self.fnctx();
        // 03 §21: a waitable body leaves only by returning its operation.
        if ctx.waitable {
            self.err(
                "E1410",
                "a `try` in a `waitable fn` body would leave the body; an error is kept in the operation and given by its first `poll`",
                op_span,
            );
        }
        self.reject_try_in_consuming_loop(Span::new(op_span.start, op_span.start + 3));
        if self.kernel {
            self.err(
                "E1511",
                "a kernel cannot use `try` or a fallible operation",
                op_span,
            );
            return (ok, ExprRes::None);
        }
        if self.defer_depth > ctx.defer_base {
            self.err(
                "E0708",
                "`try` inside a deferred block: its error cannot be handled after the block leaves",
                op_span,
            );
        }
        if self.par_depth > 0 {
            self.err("E1408", "`try` inside a `par for` body", op_span);
        }
        let conversion = match ctx.kind {
            super::CtxKind::Test => Some(TryConversion::Identity),
            _ => match ctx.error {
                None => {
                    self.err(
                        "E0702",
                        "`try` in a function that is not fallible; there is no error type to propagate it to",
                        op_span,
                    );
                    None
                }
                Some(fe) if fe == err || self.is_poison(fe) => Some(TryConversion::Identity),
                Some(fe) => match self.variant_from(fe, err) {
                    Some(v) => Some(TryConversion::FromVariant(v)),
                    None => {
                        let path = self.file_path();
                        let sig = ctx.error_span;
                        let d = self.err(
                            "E0703",
                            "the error of this `try` is neither the function's error type nor \
                             convertible to it",
                            op_span,
                        );
                        if let Some(sig) = sig {
                            d.relate_in("signature", &path, sig);
                        }
                        None
                    }
                },
            },
        };
        match conversion {
            Some(conversion) => (ok, ExprRes::Try(TryInfo { conversion })),
            None => (ok, ExprRes::None),
        }
    }

    /// The variant of the enum `fe` that is declared `from` the type `err`.
    pub(crate) fn variant_from(&mut self, fe: TypeId, err: TypeId) -> Option<VariantRef> {
        let TyKind::Adt(def, args) = self.kind(fe) else {
            return None;
        };
        let sig = self.cx.d.enum_sig(def)?.clone();
        let s = self.cx.d.subst_for(def, &args);
        for (index, v) in sig.variants.iter().enumerate() {
            if !v.from {
                continue;
            }
            let Some(p) = v.payload else { continue };
            let p = self.cx.d.instantiate(p, &s);
            if p == err {
                return Some(VariantRef {
                    enum_ty: fe,
                    index,
                    def: Some(v.def),
                    payload: Some(p),
                });
            }
        }
        None
    }

    /// Is `un` a `UnOp` that leaves a value in place? (Used by the place analysis.)
    #[allow(dead_code)]
    pub(crate) fn is_borrow_op(un: UnOp) -> bool {
        matches!(un, UnOp::Borrow | UnOp::BorrowMut)
    }
}
