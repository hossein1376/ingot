//! Phase 11a's per-instance checks: what depends on a constant generic argument's
//! value, or on the executor of a `dispatch` (pipeline §6a).
//!
//! The generic body is checked once. Here each reached instance re-evaluates the
//! constant expressions that phase 7 left for it: `@static_assert` conditions, vector
//! lane and shuffle indices, array lengths and `align(...)` arguments, and the element
//! and uniform types at a `dispatch`. Constant evaluation uses the interpreter with the
//! instance's substitution, so a call in the expression sees the same arguments.

use crate::hir::{ExprId, ExprKind};
use crate::lowering::Cx;
use crate::sigs::{ItemSig, StructSig};
use crate::ty::{ConstArg, ConstValue, FnPtrSig, GenericArg, IntTy, Subst, TyKind, TypeId};
use crate::typed::{Bodies, ExprRes, TypedBody};
use crate::value::Value;
use ingot_diag::diagnostic::Diagnostic;
use ingot_diag::source::Span;
use std::collections::BTreeSet;

/// `(code, file, start, end)` of a diagnostic already present.
type DiagKey = (&'static str, String, usize, usize);

impl Cx<'_, '_> {
    pub(crate) fn check_instance_checks(
        &mut self,
        bodies: &Bodies,
        instances: &[crate::InstanceArgs],
    ) {
        let mut seen: BTreeSet<DiagKey> = self
            .d
            .diagnostics
            .iter()
            .map(|d| (d.code, d.file.clone(), d.span.start, d.span.end))
            .collect();
        for instance in instances {
            let Some(body) = bodies.bodies.get(&instance.owner) else {
                continue;
            };
            if body.failed {
                continue;
            }
            self.check_body_instance(body, instance, &mut seen);
            self.check_instance_types(instance, &mut seen);
        }
    }

    // ------------------------------------------------------------ expressions

    fn check_body_instance(
        &mut self,
        body: &TypedBody,
        instance: &crate::InstanceArgs,
        seen: &mut BTreeSet<DiagKey>,
    ) {
        let pkg = body.package;
        let exprs: Vec<(ExprId, ExprKind)> = body
            .exprs
            .keys()
            .map(|&e| (e, self.pkgs[pkg].expr(e).kind.clone()))
            .collect();
        for (e, kind) in exprs {
            match &kind {
                ExprKind::Builtin { name, args, .. } if name == "static_assert" => {
                    self.check_static_assert(
                        body,
                        args,
                        self.pkgs[pkg].expr(e).span,
                        instance,
                        seen,
                    );
                }
                ExprKind::Builtin { name, args, .. } if name == "shuffle" => {
                    self.check_shuffle(body, args, instance, seen);
                }
                ExprKind::Builtin { name, args, .. } if name == "insert" => {
                    if let Some(index) = args.get(1) {
                        self.check_index_lane(body, *index, args.first().copied(), instance, seen);
                    }
                }
                ExprKind::Index { base, index, .. } => {
                    self.check_index_lane(body, *index, Some(*base), instance, seen);
                }
                _ => {}
            }
        }
        self.check_dispatches(body, instance, seen);
    }

    fn check_static_assert(
        &mut self,
        body: &TypedBody,
        args: &[ExprId],
        span: Span,
        instance: &crate::InstanceArgs,
        seen: &mut BTreeSet<DiagKey>,
    ) {
        let Some(&condition) = args.first() else {
            return;
        };
        let message = args
            .get(1)
            .and_then(|m| match &self.pkgs[body.package].expr(*m).kind {
                ExprKind::Str(bytes) => Some(String::from_utf8_lossy(bytes).into_owned()),
                _ => None,
            })
            .unwrap_or_default();
        let ty = self.d.types.bool_;
        let before = self.d.diagnostics.len();
        let result = self.eval_in_body_with_subst(body, condition, ty, instance.subst.clone());
        self.label_new(before, &instance.labels);
        match result {
            Ok(Value::Bool(true)) => {}
            Ok(Value::Bool(false)) => {
                let message = if message.is_empty() {
                    "`@static_assert` condition is false".to_string()
                } else {
                    format!("`@static_assert` failed: {message}")
                };
                let file = self.file_of(body.owner);
                self.note("E0810", message, file, span, &instance.labels, seen);
            }
            Ok(_) => {}
            Err(crate::consteval::Fail::Reported) => {}
            Err(crate::consteval::Fail::OutOfRange(sp)) => {
                let file = self.file_of(body.owner);
                self.note(
                    "E0405",
                    "the condition does not fit its type".to_string(),
                    file,
                    sp,
                    &instance.labels,
                    seen,
                );
            }
            Err(crate::consteval::Fail::Generic) => {
                self.ni("a static assertion that still depends on an unresolved parameter");
            }
            Err(crate::consteval::Fail::NotImplemented(m))
            | Err(crate::consteval::Fail::Interpret(m)) => {
                self.ni(format!("{m} (per-instance assertion)"));
            }
        }
    }

    fn check_index_lane(
        &mut self,
        body: &TypedBody,
        index: ExprId,
        base: Option<ExprId>,
        instance: &crate::InstanceArgs,
        seen: &mut BTreeSet<DiagKey>,
    ) {
        let Some(base) = base else {
            return;
        };
        let Some(base_ty) = body.expr(base).map(|i| i.ty) else {
            return;
        };
        let TyKind::Vector(_, lanes) = self.d.types.kind(base_ty) else {
            return;
        };
        let lanes = *lanes;
        let span = self.pkgs[body.package].expr(index).span;
        let before = self.d.diagnostics.len();
        let result =
            self.eval_in_body_with_subst(body, index, self.d.types.i64_, instance.subst.clone());
        self.label_new(before, &instance.labels);
        match result {
            Ok(Value::Int { value, .. }) if value >= 0 && value < i128::from(lanes) => {}
            Ok(_) => {
                let file = self.file_of(body.owner);
                self.note(
                    "E1502",
                    format!("the lane index must be a constant in `0..{lanes}`"),
                    file,
                    span,
                    &instance.labels,
                    seen,
                );
            }
            Err(crate::consteval::Fail::Reported) => {}
            Err(crate::consteval::Fail::Generic) => {
                self.ni("a lane index that still depends on an unresolved parameter");
            }
            Err(crate::consteval::Fail::NotImplemented(m))
            | Err(crate::consteval::Fail::Interpret(m)) => {
                self.ni(format!("{m} (per-instance lane index)"));
            }
            Err(crate::consteval::Fail::OutOfRange(_)) => {
                let file = self.file_of(body.owner);
                self.note(
                    "E1502",
                    format!("the lane index must be a constant in `0..{lanes}`"),
                    file,
                    span,
                    &instance.labels,
                    seen,
                );
            }
        }
    }

    fn check_shuffle(
        &mut self,
        body: &TypedBody,
        args: &[ExprId],
        instance: &crate::InstanceArgs,
        seen: &mut BTreeSet<DiagKey>,
    ) {
        let Some(&vector) = args.first() else {
            return;
        };
        let Some(&idx) = args.last() else {
            return;
        };
        let Some(vector_ty) = body.expr(vector).map(|i| i.ty) else {
            return;
        };
        let TyKind::Vector(_, lanes) = self.d.types.kind(vector_ty) else {
            return;
        };
        let limit = if args.len() == 3 {
            i128::from(*lanes) * 2
        } else {
            i128::from(*lanes)
        };
        let mut inner = idx;
        while let ExprKind::Paren(e) = &self.pkgs[body.package].expr(inner).kind {
            inner = *e;
        }
        let ExprKind::ArrayLit { elems, .. } = &self.pkgs[body.package].expr(inner).kind else {
            return;
        };
        let elems = elems.clone();
        let span = self.pkgs[body.package].expr(idx).span;
        let mut bad = false;
        for element in elems {
            let before = self.d.diagnostics.len();
            let i32_ = self.d.types.int(IntTy::I32);
            let result = self.eval_in_body_with_subst(body, element, i32_, instance.subst.clone());
            self.label_new(before, &instance.labels);
            match result {
                Ok(Value::Int { value, .. }) if value >= 0 && value < limit => {}
                Ok(_) | Err(crate::consteval::Fail::OutOfRange(_)) => bad = true,
                Err(crate::consteval::Fail::Reported) => return,
                Err(crate::consteval::Fail::Generic) => {
                    self.ni("a shuffle index that still depends on an unresolved parameter");
                    return;
                }
                Err(crate::consteval::Fail::NotImplemented(m))
                | Err(crate::consteval::Fail::Interpret(m)) => {
                    self.ni(format!("{m} (per-instance shuffle index)"));
                    return;
                }
            }
        }
        if bad {
            let file = self.file_of(body.owner);
            self.note(
                "E1504",
                format!("every index of `@shuffle` is a constant in `0..{limit}`"),
                file,
                span,
                &instance.labels,
                seen,
            );
        }
    }

    // --------------------------------------------------------------- types

    fn check_instance_types(
        &mut self,
        instance: &crate::InstanceArgs,
        seen: &mut BTreeSet<DiagKey>,
    ) {
        let types = self.d.generic_types.clone();
        for c in types {
            if c.owner != Some(instance.owner) {
                continue;
            }
            let before = self.d.diagnostics.len();
            let resolved = self.resolve_instance_type(c.ty, &instance.subst);
            self.label_new(before, &instance.labels);
            let Ok(ty) = resolved else {
                continue;
            };
            let ty = self.d.instantiate(ty, &Subst::new());
            if self.d.types.has_params(ty) || self.d.types.has_poison(ty) {
                continue;
            }
            let span = c.length_span.unwrap_or(c.span);
            let file = c.file;
            if let Some(trap) = self.instance_expr_trap(ty, &mut BTreeSet::new(), 0) {
                self.note(
                    "E0806",
                    format!(
                        "constant evaluation trapped: {}: {}",
                        trap.code(),
                        trap.text()
                    ),
                    file,
                    span,
                    &instance.labels,
                    seen,
                );
                continue;
            }
            let bad = self.instance_invalid_length(
                ty,
                &instance.subst,
                &instance.labels,
                &mut BTreeSet::new(),
                0,
            );
            if let Some(value) = bad {
                let message = if value < 0 {
                    format!("an array cannot have the negative length {value}")
                } else {
                    format!("an array length is at most 2^62, not {value}")
                };
                self.note("E0521", message, file, span, &instance.labels, seen);
            } else {
                match self.d.layout_of(ty) {
                    Ok(layout) if layout.size > 1 << 62 => {
                        self.note(
                            "E0521",
                            "a type's size is at most 2^62 bytes".to_string(),
                            file,
                            span,
                            &instance.labels,
                            seen,
                        );
                    }
                    Err(crate::LayoutError::Unspecified(m)) if m.contains("larger than 2^64") => {
                        self.note(
                            "E0521",
                            "a type's size is at most 2^62 bytes".to_string(),
                            file,
                            span,
                            &instance.labels,
                            seen,
                        );
                    }
                    _ => {}
                }
            }
            self.check_type_aligns(ty, instance, seen, 0);
        }
    }

    /// Substitute an instance's arguments and evaluate the constant expressions a type
    /// still contains. A parameter that is not bound stays a parameter.
    fn resolve_instance_type(
        &mut self,
        ty: TypeId,
        subst: &Subst,
    ) -> Result<TypeId, crate::consteval::Fail> {
        let ty = self.d.types.subst(ty, subst);
        let kind = self.d.types.kind(ty).clone();
        let rebuilt = match kind {
            TyKind::Array(elem, len) => {
                let elem = self.resolve_instance_type(elem, subst)?;
                let len = self.resolve_const(len, subst)?;
                TyKind::Array(elem, len)
            }
            TyKind::Tuple(ts) => TyKind::Tuple(
                ts.into_iter()
                    .map(|t| self.resolve_instance_type(t, subst))
                    .collect::<Result<Vec<_>, _>>()?,
            ),
            TyKind::Adt(d, args) => TyKind::Adt(d, self.resolve_args(d, args, subst)?),
            TyKind::Dyn(m, d, args) => TyKind::Dyn(m, d, self.resolve_args(d, args, subst)?),
            TyKind::Optional(t) => TyKind::Optional(self.resolve_instance_type(t, subst)?),
            TyKind::Result(a, b) => TyKind::Result(
                self.resolve_instance_type(a, subst)?,
                self.resolve_instance_type(b, subst)?,
            ),
            TyKind::Ref(m, t) => TyKind::Ref(m, self.resolve_instance_type(t, subst)?),
            TyKind::Slice(m, t) => TyKind::Slice(m, self.resolve_instance_type(t, subst)?),
            TyKind::RawPtr(m, t) => TyKind::RawPtr(m, self.resolve_instance_type(t, subst)?),
            TyKind::FnPtr(sig) => {
                let sig = FnPtrSig {
                    ext: sig.ext,
                    params: sig
                        .params
                        .into_iter()
                        .map(|t| self.resolve_instance_type(t, subst))
                        .collect::<Result<Vec<_>, _>>()?,
                    ret: self.resolve_instance_type(sig.ret, subst)?,
                    error: match sig.error {
                        Some(t) => Some(self.resolve_instance_type(t, subst)?),
                        None => None,
                    },
                };
                TyKind::FnPtr(sig)
            }
            TyKind::DynFn(m, k, sig) => {
                let sig = FnPtrSig {
                    ext: sig.ext,
                    params: sig
                        .params
                        .into_iter()
                        .map(|t| self.resolve_instance_type(t, subst))
                        .collect::<Result<Vec<_>, _>>()?,
                    ret: self.resolve_instance_type(sig.ret, subst)?,
                    error: match sig.error {
                        Some(t) => Some(self.resolve_instance_type(t, subst)?),
                        None => None,
                    },
                };
                TyKind::DynFn(m, k, sig)
            }
            TyKind::Closure(d, i, args) => {
                let args = args
                    .into_iter()
                    .map(|a| match a {
                        GenericArg::Ty(t) => {
                            Ok(GenericArg::Ty(self.resolve_instance_type(t, subst)?))
                        }
                        GenericArg::Const(c) => {
                            Ok(GenericArg::Const(self.resolve_const(c, subst)?))
                        }
                    })
                    .collect::<Result<Vec<_>, crate::consteval::Fail>>()?;
                TyKind::Closure(d, i, args)
            }
            TyKind::Projection(t, d) => {
                TyKind::Projection(self.resolve_instance_type(t, subst)?, d)
            }
            other => other,
        };
        Ok(self.d.types.intern(rebuilt))
    }

    fn resolve_args(
        &mut self,
        owner: crate::DefId,
        args: Vec<GenericArg>,
        subst: &Subst,
    ) -> Result<Vec<GenericArg>, crate::consteval::Fail> {
        let _ = owner;
        let mut out = Vec::new();
        for arg in args {
            out.push(match arg {
                GenericArg::Ty(t) => GenericArg::Ty(self.resolve_instance_type(t, subst)?),
                GenericArg::Const(c) => GenericArg::Const(self.resolve_const(c, subst)?),
            });
        }
        Ok(out)
    }

    fn resolve_const(
        &mut self,
        c: ConstArg,
        subst: &Subst,
    ) -> Result<ConstArg, crate::consteval::Fail> {
        let c = match c {
            ConstArg::Param(p) => match subst.get(p) {
                Some(GenericArg::Const(v)) => *v,
                _ => c,
            },
            c => c,
        };
        if let Some(v) = self.d.types.const_arg_value(c) {
            return Ok(ConstArg::Value(v));
        }
        Ok(c)
    }

    fn instance_expr_trap(
        &mut self,
        ty: TypeId,
        seen: &mut BTreeSet<TypeId>,
        depth: u32,
    ) -> Option<crate::consteval::Trap> {
        if depth > 64 || !seen.insert(ty) {
            return None;
        }
        if let TyKind::Array(_, ConstArg::Expr(id)) = self.d.types.kind(ty)
            && let Some(trap) = self.d.types.expr_trap(*id)
        {
            return Some(trap);
        }
        let children: Vec<TypeId> = match self.d.types.kind(ty).clone() {
            TyKind::Adt(def, args) => self.adt_fields(def, &args),
            _ => self.d.types.children(ty),
        };
        children
            .into_iter()
            .find_map(|t| self.instance_expr_trap(t, seen, depth + 1))
    }

    fn adt_fields(&mut self, def: crate::DefId, args: &[GenericArg]) -> Vec<TypeId> {
        let s = self.d.subst_for(def, args);
        match self.d.items.get(&def) {
            Some(ItemSig::Struct(st)) => st
                .fields
                .iter()
                .map(|f| self.d.types.subst(f.ty, &s))
                .collect(),
            Some(ItemSig::Enum(en)) => en
                .variants
                .iter()
                .filter_map(|v| v.payload.map(|t| self.d.types.subst(t, &s)))
                .collect(),
            _ => Vec::new(),
        }
    }

    /// An invalid length anywhere in `ty`, including the fields of an ADT it names.
    fn instance_invalid_length(
        &mut self,
        ty: TypeId,
        subst: &Subst,
        labels: &[(usize, Span)],
        seen: &mut BTreeSet<crate::DefId>,
        depth: u32,
    ) -> Option<i128> {
        if depth > 64 {
            return None;
        }
        match self.d.types.kind(ty).clone() {
            TyKind::Array(elem, len) => {
                let before = self.d.diagnostics.len();
                let resolved = self.resolve_const(len, subst);
                self.label_new(before, labels);
                if let Ok(ConstArg::Value(ConstValue::Int { value, .. })) = resolved
                    && !(0..=(1 << 62)).contains(&value)
                {
                    return Some(value);
                }
                self.instance_invalid_length(elem, subst, labels, seen, depth + 1)
            }
            TyKind::Tuple(ts) => ts
                .into_iter()
                .find_map(|t| self.instance_invalid_length(t, subst, labels, seen, depth + 1)),
            TyKind::Adt(def, args) => {
                if !seen.insert(def) {
                    return None;
                }
                let s = self.d.subst_for(def, &args);
                let fields: Vec<TypeId> = match self.d.items.get(&def) {
                    Some(ItemSig::Struct(st)) => st
                        .fields
                        .iter()
                        .map(|f| self.d.types.subst(f.ty, &s))
                        .collect(),
                    Some(ItemSig::Enum(en)) => en
                        .variants
                        .iter()
                        .filter_map(|v| v.payload.map(|t| self.d.types.subst(t, &s)))
                        .collect(),
                    _ => Vec::new(),
                };
                let found = fields
                    .into_iter()
                    .find_map(|t| self.instance_invalid_length(t, &s, labels, seen, depth + 1));
                seen.remove(&def);
                found
            }
            TyKind::Optional(t)
            | TyKind::Ref(_, t)
            | TyKind::Slice(_, t)
            | TyKind::RawPtr(_, t) => {
                self.instance_invalid_length(t, subst, labels, seen, depth + 1)
            }
            TyKind::Result(a, b) => self
                .instance_invalid_length(a, subst, labels, seen, depth + 1)
                .or_else(|| self.instance_invalid_length(b, subst, labels, seen, depth + 1)),
            other => {
                let args: Vec<GenericArg> = match other {
                    TyKind::Dyn(_, _, a) | TyKind::Closure(_, _, a) => a,
                    TyKind::FnPtr(sig) => {
                        let mut all = sig.params;
                        all.push(sig.ret);
                        all.extend(sig.error);
                        all.into_iter().map(GenericArg::Ty).collect()
                    }
                    TyKind::DynFn(_, _, sig) => {
                        let mut all = sig.params;
                        all.push(sig.ret);
                        all.extend(sig.error);
                        all.into_iter().map(GenericArg::Ty).collect()
                    }
                    _ => Vec::new(),
                };
                args.into_iter().find_map(|a| match a {
                    GenericArg::Ty(t) => {
                        self.instance_invalid_length(t, subst, labels, seen, depth + 1)
                    }
                    GenericArg::Const(_) => None,
                })
            }
        }
    }

    // -------------------------------------------------------------- alignment

    fn check_type_aligns(
        &mut self,
        ty: TypeId,
        instance: &crate::InstanceArgs,
        seen: &mut BTreeSet<DiagKey>,
        depth: u32,
    ) {
        if depth > 16 {
            return;
        }
        let TyKind::Adt(def, args) = self.d.types.kind(ty).clone() else {
            return;
        };
        let Some(sig) = self.d.struct_sig(def).cloned() else {
            return;
        };
        let s = self.d.subst_for(def, &args);
        let file = self.file_of(def);
        if let Some(align) = sig.align {
            self.check_one_align(file, align, None, &sig, &s, instance, seen);
        }
        for field in &sig.fields {
            if let Some(align) = field.align {
                let field_ty = self.d.types.subst(field.ty, &s);
                self.check_one_align(file, align, Some(field_ty), &sig, &s, instance, seen);
            }
            let field_ty = self.d.types.subst(field.ty, &s);
            if !self.d.types.has_params(field_ty) {
                self.check_type_aligns(field_ty, instance, seen, depth + 1);
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn check_one_align(
        &mut self,
        file: usize,
        align: crate::sigs::AlignAttr,
        field_ty: Option<TypeId>,
        sig: &StructSig,
        s: &Subst,
        instance: &crate::InstanceArgs,
        seen: &mut BTreeSet<DiagKey>,
    ) {
        let value = match align.raw {
            Some((owner, expr)) => {
                let before = self.d.diagnostics.len();
                let value = self.eval_signature_expr_with_subst(
                    self.pkg_of(owner),
                    self.file_of(owner),
                    expr,
                    crate::consteval::ValTy::Int(IntTy::I64),
                    owner,
                    s.clone(),
                );
                self.label_new(before, &instance.labels);
                match value {
                    Ok(ConstValue::Int { value, .. }) => value,
                    _ => return,
                }
            }
            None => align.value,
        };
        if crate::valid_align(value).is_none() {
            self.note(
                "E0515",
                format!(
                    "`align({value})`: the alignment must be a power of two no larger than 65536"
                ),
                file,
                align.span,
                &instance.labels,
                seen,
            );
            return;
        }
        let natural = match field_ty {
            Some(t) => self.d.layout_of(t).map(|l| l.align).ok(),
            None => self.natural_align(sig, s, instance),
        };
        if let Some(n) = natural
            && value < i128::from(n)
        {
            self.note(
                "E0515",
                format!("`align({value})` is below the natural alignment {n}"),
                file,
                align.span,
                &instance.labels,
                seen,
            );
        }
    }

    fn natural_align(
        &mut self,
        sig: &StructSig,
        s: &Subst,
        instance: &crate::InstanceArgs,
    ) -> Option<u64> {
        let mut align = 1u64;
        for field in &sig.fields {
            let t = self.d.types.subst(field.ty, s);
            let l = self.d.layout_of(t).ok()?.align;
            let raised = match field.align {
                None => 1,
                Some(a) => match a.raw {
                    Some((owner, expr)) => {
                        let before = self.d.diagnostics.len();
                        let value = self.eval_signature_expr_with_subst(
                            self.pkg_of(owner),
                            self.file_of(owner),
                            expr,
                            crate::consteval::ValTy::Int(IntTy::I64),
                            owner,
                            s.clone(),
                        );
                        self.label_new(before, &instance.labels);
                        match value {
                            Ok(ConstValue::Int { value, .. }) => {
                                crate::valid_align(value).unwrap_or(1)
                            }
                            _ => 1,
                        }
                    }
                    None => crate::valid_align(a.value).unwrap_or(1),
                },
            };
            align = align.max(l).max(raised);
        }
        Some(align)
    }

    // ---------------------------------------------------------------- dispatch

    fn check_dispatches(
        &mut self,
        body: &TypedBody,
        instance: &crate::InstanceArgs,
        seen: &mut BTreeSet<DiagKey>,
    ) {
        let mut dispatches: Vec<(Span, TypeId, crate::DefId)> = Vec::new();
        for (&e, info) in &body.exprs {
            if let ExprRes::Dispatch(d) = &info.res {
                let _ = e;
                dispatches.push((
                    self.pkgs[body.package].expr(e).span,
                    self.d.instantiate(d.executor_ty, &instance.subst),
                    d.kernel,
                ));
            }
        }
        for (&stmt, info) in &body.stmts {
            if let crate::typed::StmtInfo::Dispatch(d) = info {
                dispatches.push((
                    self.pkgs[body.package].stmt(stmt).span,
                    self.d.instantiate(d.executor_ty, &instance.subst),
                    d.kernel,
                ));
            }
        }
        for (span, executor, kernel) in dispatches {
            if self.is_dyn_executor(executor) {
                self.note(
                    "E1514",
                    "the executor of this `dispatch` is `&dyn Executor`; a dispatch needs the \
                     executor's type at compile time"
                        .to_string(),
                    self.file_of(instance.owner),
                    span,
                    &instance.labels,
                    seen,
                );
                continue;
            }
            let Some(sig) = self.d.fn_sig(kernel).cloned() else {
                continue;
            };
            let cpu = self.is_cpu_executor(executor);
            for (i, param) in sig.params.iter().enumerate().skip(1) {
                let ty = match self.d.types.kind(param.ty) {
                    TyKind::Slice(_, elem) => *elem,
                    _ => param.ty,
                };
                if !self.dispatch_type_ok(ty, cpu, 0) {
                    let name = self.d.types.display(self.sema, ty);
                    let at = if param.ty_span.start < param.ty_span.end {
                        param.ty_span
                    } else {
                        span
                    };
                    self.note(
                        "E1514",
                        format!(
                            "the `dispatch` {} type `{name}` is not an element or uniform type for this executor",
                            i
                        ),
                        self.file_of(kernel),
                        at,
                        &instance.labels,
                        seen,
                    );
                }
            }
        }
    }

    fn is_dyn_executor(&self, ty: TypeId) -> bool {
        let ty = match self.d.types.kind(ty) {
            TyKind::Ref(_, inner) => *inner,
            _ => ty,
        };
        matches!(self.d.types.kind(ty), TyKind::Dyn(..))
    }

    fn is_cpu_executor(&self, ty: TypeId) -> bool {
        let ty = match self.d.types.kind(ty) {
            TyKind::Ref(_, inner) => *inner,
            _ => ty,
        };
        let TyKind::Adt(def, _) = self.d.types.kind(ty) else {
            return false;
        };
        let d = self.sema.def(*def);
        self.sema.module(d.module).path == "std.exec"
            && matches!(d.name.as_str(), "Seq" | "Simd" | "Par")
    }

    fn dispatch_type_ok(&mut self, ty: TypeId, cpu: bool, depth: u32) -> bool {
        if depth > 32 {
            return false;
        }
        match self.d.types.kind(ty) {
            TyKind::Int(i) => match i {
                IntTy::I32 | IntTy::U32 => true,
                IntTy::I64 | IntTy::U64 => cpu,
                _ => false,
            },
            TyKind::Float(f) => match f {
                crate::ty::FloatTy::F32 => true,
                crate::ty::FloatTy::F64 => cpu,
            },
            TyKind::Bool => true,
            TyKind::Vector(lane, n) => {
                *n <= 4
                    && match lane {
                        crate::ty::Lane::Int(i) => match i {
                            IntTy::I32 | IntTy::U32 => true,
                            IntTy::I64 | IntTy::U64 => cpu,
                            _ => false,
                        },
                        crate::ty::Lane::Float(f) => match f {
                            crate::ty::FloatTy::F32 => true,
                            crate::ty::FloatTy::F64 => cpu,
                        },
                    }
            }
            TyKind::Adt(def, args) => {
                if let Some(e) = self.d.enum_sig(*def)
                    && e.copy
                    && self.d.is_payload_free_enum(*def)
                {
                    return true;
                }
                let Some(sig) = self.d.struct_sig(*def).cloned() else {
                    return false;
                };
                if !matches!(sig.kind, ingot_syntax::ast::StructKind::Copy) {
                    return false;
                }
                let s = self.d.subst_for(*def, args);
                sig.fields.iter().all(|f| {
                    let t = self.d.types.subst(f.ty, &s);
                    self.dispatch_type_ok(t, cpu, depth + 1)
                })
            }
            _ => false,
        }
    }

    // --------------------------------------------------------------- plumbing

    /// Attach the `instantiation` chain to the diagnostics an evaluation just added.
    fn label_new(&mut self, before: usize, labels: &[(usize, Span)]) {
        if labels.is_empty() {
            return;
        }
        for d in &mut self.d.diagnostics[before..] {
            for (file, span) in labels {
                let path = self.sources.get(*file).path.clone();
                d.relate_in("instantiation", &path, *span);
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn note(
        &mut self,
        code: &'static str,
        message: String,
        file: usize,
        span: Span,
        labels: &[(usize, Span)],
        seen: &mut BTreeSet<DiagKey>,
    ) {
        if !seen.insert((
            code,
            self.sources.get(file).path.clone(),
            span.start,
            span.end,
        )) {
            return;
        }
        let source = self.sources.get(file);
        let mut d = Diagnostic::new(code, message, source, span);
        for (label_file, label_span) in labels {
            d.relate_in(
                "instantiation",
                &self.sources.get(*label_file).path,
                *label_span,
            );
        }
        self.d.diagnostics.push(d);
    }
}
