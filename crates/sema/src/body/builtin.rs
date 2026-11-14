//! Builtins (`@name`, spec sections 5.3 to 5.5, 6.5, 7, 9, 26): the ones whose signature
//! the specification gives are typed here. A builtin whose signature it does not give
//! precisely is recorded as not implemented.

use super::Checker;
use crate::defs::DefKind;
use crate::hir::{self, ExprId, ExprKind};
use crate::sigs::{ItemSig, Tri};
use crate::ty::*;
use crate::typed::*;
use ingot_diag::source::Span;

/// A builtin that the specification lists as needing `unsafe` (section 15).
fn unsafe_builtin(name: &str) -> bool {
    matches!(name, "uninit" | "assume")
        || name.starts_with("unchecked_")
        || name.ends_with("_from_raw")
}

/// Why `@zero` is not allowed for a type.
enum Zero {
    Ok,
    Bad(String),
    /// A struct of another module with a field that is not visible, or no fields.
    Private(crate::defs::DefId),
    /// A type parameter that no bound grants `@zero`.
    Param,
    Unknown,
}

impl Checker<'_, '_> {
    pub(crate) fn builtin(
        &mut self,
        e: ExprId,
        name: &str,
        type_arg: Option<&hir::Ty>,
        args: &[ExprId],
        hint: Option<TypeId>,
    ) -> (TypeId, ExprRes) {
        let targ = type_arg.map(|t| self.lower(t));
        let (ty, res_arg) = self.builtin_inner(e, name, targ, args, hint);
        (ty, ExprRes::Builtin { type_arg: res_arg })
    }

    /// Check the number of arguments (E0410); the arguments are typed blind otherwise.
    fn arity(&mut self, e: ExprId, name: &str, args: &[ExprId], n: usize) -> bool {
        if args.len() == n {
            return true;
        }
        let span = self.span_of(e);
        self.err(
            "E0410",
            format!(
                "`@{name}` takes {n} argument(s) besides its type, but {} {} given",
                args.len(),
                if args.len() == 1 { "was" } else { "were" }
            ),
            span,
        );
        self.blind_args(args);
        false
    }

    fn need_type(&mut self, e: ExprId, name: &str, t: Option<TypeId>) -> Option<TypeId> {
        match t {
            Some(t) if !self.is_poison(t) => Some(t),
            Some(_) => None,
            None => {
                let span = self.span_of(e);
                self.err(
                    "E0410",
                    format!("`@{name}` needs a type as its first argument"),
                    span,
                );
                None
            }
        }
    }

    fn builtin_inner(
        &mut self,
        e: ExprId,
        name: &str,
        targ: Option<TypeId>,
        args: &[ExprId],
        hint: Option<TypeId>,
    ) -> (TypeId, Option<TypeId>) {
        let poison = self.poison();
        let span = self.span_of(e);
        if unsafe_builtin(name) {
            let inside = self.requires_unsafe(span, &format!("`@{name}`"));
            self.blind_args(args);
            if inside && !self.kernel {
                // The specification gives no precise signature for these builtins yet.
                self.ni(format!(
                    "`@{name}`: the specification does not give its signature precisely"
                ));
            }
            return (poison, targ);
        }
        match name {
            "zero" => {
                let t = targ.or(hint);
                let Some(t) = t else {
                    self.err(
                        "E0404",
                        "the type of `@zero` cannot be inferred here; write `@zero(T)` or give the binding a type",
                        span,
                    );
                    return (poison, None);
                };
                if self.is_poison(t) {
                    return (poison, None);
                }
                self.zero_check(e, t);
                (t, Some(t))
            }
            "narrow" | "wrap" | "sat" => {
                let t = self.need_type(e, name, targ);
                if !self.arity(e, name, args, 1) {
                    return (t.unwrap_or(poison), t);
                }
                let x = self.expr(args[0], None);
                let Some(t) = t else {
                    return (poison, None);
                };
                if self.is_poison(x) || self.is_never(x) {
                    return (t, Some(t));
                }
                let tk = self.kind(t);
                let xk = self.kind(x);
                match (&tk, &xk) {
                    (TyKind::Int(_), TyKind::Int(_)) if name == "wrap" => (t, Some(t)),
                    (TyKind::Int(_), TyKind::Int(_) | TyKind::Float(_)) if name == "sat" => {
                        (t, Some(t))
                    }
                    (TyKind::Int(_), TyKind::Int(_) | TyKind::Float(_)) if name == "narrow" => {
                        (t, Some(t))
                    }
                    (TyKind::Vector(tl, tn), TyKind::Vector(sl, sn)) if name != "narrow" => {
                        if tn != sn {
                            self.err(
                                "E1503",
                                format!(
                                    "`@{name}` needs a vector of `{tn}` lanes, and this one has \
                                     `{sn}`"
                                ),
                                span,
                            );
                            return (poison, Some(t));
                        }
                        let target_lane = self.lane_ty(*tl);
                        let source_lane = self.lane_ty(*sl);
                        let (tky, sky) = (self.kind(target_lane), self.kind(source_lane));
                        let ok = matches!(tky, TyKind::Int(_))
                            && match name {
                                "wrap" => matches!(sky, TyKind::Int(_)),
                                _ => matches!(sky, TyKind::Int(_) | TyKind::Float(_)),
                            };
                        if ok {
                            (t, Some(t))
                        } else {
                            self.err(
                                "E0401",
                                format!("`@{name}` is not defined between these lane types"),
                                span,
                            );
                            (poison, Some(t))
                        }
                    }
                    _ => {
                        if matches!(tk, TyKind::Vector(..) | TyKind::Mask(_) | TyKind::Param(_))
                            || matches!(xk, TyKind::Vector(..) | TyKind::Mask(_) | TyKind::Param(_))
                        {
                            self.ni(format!(
                                "`@{name}` between a vector and a non-vector, a mask or a type \
                                 parameter"
                            ));
                            (poison, Some(t))
                        } else if !matches!(tk, TyKind::Int(_)) {
                            self.builtin_type_error(e, name, "an integer type", t);
                            (poison, Some(t))
                        } else {
                            let want = match (name, xk) {
                                ("wrap", _) => "an integer",
                                _ => "an integer or a float",
                            };
                            self.builtin_type_error(e, name, want, x);
                            (poison, Some(t))
                        }
                    }
                }
            }
            "nearest" => {
                let t = self.need_type(e, name, targ);
                if !self.arity(e, name, args, 1) {
                    return (t.unwrap_or(poison), t);
                }
                let x = self.expr(args[0], None);
                let Some(t) = t else {
                    return (poison, None);
                };
                if self.is_poison(x) || self.is_never(x) {
                    return (t, Some(t));
                }
                let tk = self.kind(t);
                let xk = self.kind(x);
                match (&tk, &xk) {
                    (TyKind::Float(_), TyKind::Int(_)) => (t, Some(t)),
                    (TyKind::Float(FloatTy::F32), TyKind::Float(FloatTy::F64)) => (t, Some(t)),
                    (TyKind::Vector(tl, tn), TyKind::Vector(sl, sn)) => {
                        if tn != sn {
                            self.err(
                                "E1503",
                                format!(
                                    "`@nearest` needs a vector of `{tn}` lanes, and this one has \
                                     `{sn}`"
                                ),
                                span,
                            );
                            return (poison, Some(t));
                        }
                        let target_lane = self.lane_ty(*tl);
                        let source_lane = self.lane_ty(*sl);
                        let (tky, sky) = (self.kind(target_lane), self.kind(source_lane));
                        let ok = matches!(
                            (&tky, &sky),
                            (TyKind::Float(_), TyKind::Int(_))
                                | (TyKind::Float(FloatTy::F32), TyKind::Float(FloatTy::F64))
                        );
                        if ok {
                            (t, Some(t))
                        } else {
                            self.err(
                                "E0401",
                                "`@nearest` is not defined between these lane types",
                                span,
                            );
                            (poison, Some(t))
                        }
                    }
                    _ => {
                        if matches!(&tk, TyKind::Vector(..) | TyKind::Param(_))
                            || matches!(&xk, TyKind::Vector(..) | TyKind::Param(_))
                        {
                            self.ni(
                                "`@nearest` between a vector and a non-vector or a type parameter",
                            );
                            (poison, Some(t))
                        } else if !matches!(tk, TyKind::Float(_)) {
                            self.builtin_type_error(e, name, "a float type", t);
                            (poison, Some(t))
                        } else {
                            self.builtin_type_error(
                                e,
                                name,
                                "an integer or an `f64` (for `f32`)",
                                x,
                            );
                            (poison, Some(t))
                        }
                    }
                }
            }
            "bits" => {
                let t = self.need_type(e, name, targ);
                if !self.arity(e, name, args, 1) {
                    return (t.unwrap_or(poison), t);
                }
                let x = self.expr(args[0], None);
                let Some(t) = t else {
                    return (poison, None);
                };
                if self.is_poison(x) {
                    return (t, Some(t));
                }
                self.bits_check(e, t, x);
                (t, Some(t))
            }
            "enum" => {
                let t = self.need_type(e, name, targ);
                if !self.arity(e, name, args, 1) {
                    return (poison, t);
                }
                let x = self.expr(args[0], None);
                let Some(t) = t else {
                    return (poison, None);
                };
                if self.is_poison(x) {
                    return (poison, Some(t));
                }
                let sig = match self.kind(t) {
                    TyKind::Adt(d, _) => self.cx.d.enum_sig(d).cloned(),
                    _ => None,
                };
                let Some(sig) = sig else {
                    let n = self.name(t);
                    self.err(
                        "E0417",
                        format!("`@enum` needs an enum type, not `{n}`"),
                        span,
                    );
                    return (poison, Some(t));
                };
                if sig.variants.iter().any(|v| v.payload.is_some()) {
                    let n = self.name(t);
                    self.err(
                        "E0417",
                        format!("`@enum` does not convert to the enum `{n}`, which has payloads"),
                        span,
                    );
                    return (poison, Some(t));
                }
                let tag = self.mk(TyKind::Int(sig.tag_ty));
                if x != tag && !self.is_never(x) {
                    let n = self.name(tag);
                    let xn = self.name(x);
                    self.err(
                        "E0417",
                        format!("`@enum` takes a value of the tag type `{n}`, not `{xn}`"),
                        span,
                    );
                    return (poison, Some(t));
                }
                (self.mk(TyKind::Optional(t)), Some(t))
            }
            "sizeof" | "alignof" => {
                let t = self.need_type(e, name, targ);
                if !self.arity(e, name, args, 0) {
                    return (self.i64_(), t);
                }
                (self.i64_(), t)
            }
            "offsetof" => {
                self.blind_args(args);
                self.ni(
                    "`@offsetof(T, field)`: the field name is resolved as a value name \
                     (specification section 9 does not say how it is written)",
                );
                (poison, targ)
            }
            "static_assert" => {
                if args.len() != 2 {
                    self.arity(e, name, args, 2);
                    return (self.void(), None);
                }
                self.cond(args[0]);
                let bytes = self.bytes_ty();
                self.expect(args[1], bytes);
                let condition = args[0];
                if self.is_poison(self.ty_of(condition)) {
                    return (self.void(), None);
                }
                // A local, a parameter or a static makes the condition not constant
                // (E0807), at the read.
                let reads = crate::consteval::runtime_reads(self.hir, self.cx.sema, condition);
                if !reads.is_empty() {
                    for at in reads {
                        self.err(
                            "E0807",
                            "a constant expression reads a local variable, a parameter or a static",
                            at,
                        );
                    }
                    return (self.void(), None);
                }
                match self.cx.eval_expr(
                    self.pkg,
                    self.file,
                    condition,
                    Some(crate::consteval::ValTy::Bool),
                ) {
                    Ok(crate::ty::ConstValue::Bool(true)) => {}
                    Ok(crate::ty::ConstValue::Bool(false)) => {
                        let msg = match &self.hir.expr(self.unparen(args[1])).kind {
                            ExprKind::Str(bytes) => String::from_utf8_lossy(bytes).into_owned(),
                            _ => String::new(),
                        };
                        let message = if msg.is_empty() {
                            "`@static_assert` condition is false".to_string()
                        } else {
                            format!("`@static_assert` failed: {msg}")
                        };
                        self.err("E0810", message, span);
                    }
                    Ok(_) => {}
                    Err(crate::consteval::Fail::Reported) => self.errors += 1,
                    Err(crate::consteval::Fail::OutOfRange(sp)) => {
                        self.err("E0405", "the condition does not fit its type", sp);
                    }
                    // A condition over a type or constant generic parameter is checked
                    // for each instance in phase 11 (section 9).
                    Err(crate::consteval::Fail::Generic) => {}
                    Err(crate::consteval::Fail::NotImplemented(reason)) => {
                        self.ni(format!("{reason} (S5b)"));
                    }
                }
                (self.void(), None)
            }
            "assert" => {
                if args.is_empty() || args.len() > 2 {
                    let span = self.span_of(e);
                    self.err(
                        "E0410",
                        "`@assert` takes a condition and, optionally, a message",
                        span,
                    );
                    self.blind_args(args);
                    return (self.void(), None);
                }
                self.cond(args[0]);
                if let Some(m) = args.get(1) {
                    let bytes = self.bytes_ty();
                    self.expect(*m, bytes);
                }
                (self.void(), None)
            }
            "panic" => {
                if self.arity(e, name, args, 1) {
                    let bytes = self.bytes_ty();
                    self.expect(args[0], bytes);
                }
                (self.never(), None)
            }
            "unreachable" => {
                self.arity(e, name, args, 0);
                (self.never(), None)
            }
            "forget" => {
                if self.arity(e, name, args, 1) {
                    self.expr(args[0], None);
                }
                (self.void(), None)
            }
            "replace" => {
                if !self.arity(e, name, args, 2) {
                    return (poison, None);
                }
                let p = self.expr(args[0], None);
                match self.kind(p) {
                    TyKind::Ref(Mutability::Mut, t) => {
                        self.expect(args[1], t);
                        (t, None)
                    }
                    TyKind::Poison => {
                        self.expr(args[1], None);
                        (poison, None)
                    }
                    _ => {
                        self.expr(args[1], None);
                        let n = self.name(p);
                        let s = self.span_of(args[0]);
                        self.err(
                            "E0401",
                            format!("`@replace` takes an exclusive borrow `&mut place`, not `{n}`"),
                            s,
                        );
                        (poison, None)
                    }
                }
            }
            "swap" => {
                if !self.arity(e, name, args, 2) {
                    return (self.void(), None);
                }
                let a = self.expr(args[0], None);
                let b = self.expr(args[1], Some(a));
                if self.is_poison(a) || self.is_poison(b) {
                    return (self.void(), None);
                }
                for (x, t) in [(args[0], a), (args[1], b)] {
                    if !matches!(self.kind(t), TyKind::Ref(Mutability::Mut, _)) {
                        let n = self.name(t);
                        let s = self.span_of(x);
                        self.err(
                            "E0401",
                            format!("`@swap` takes exclusive borrows `&mut place`, not `{n}`"),
                            s,
                        );
                        return (self.void(), None);
                    }
                }
                if a != b {
                    self.mismatch(args[1], b, a);
                }
                (self.void(), None)
            }
            "format" => {
                self.blind_args(args);
                self.ni(
                    "`@format(writer, \"..\", args)`: the error type of the writer, and so the \
                     type of the call, is not specified (section 35)",
                );
                (poison, None)
            }
            "debug" => {
                if args.is_empty() {
                    self.arity(e, name, args, 1);
                    return (self.void(), None);
                }
                let bytes = self.bytes_ty();
                self.expect(args[0], bytes);
                self.blind_args(&args[1..]);
                self.ni("`@debug`: checking the format string against the arguments (the format syntax is not specified)");
                (self.void(), None)
            }
            "test_env" => {
                self.arity(e, name, args, 0);
                match self.std_type("std", "Env") {
                    Some(d) => {
                        let t = self.mk(TyKind::Adt(d, Vec::new()));
                        (self.ref_to(Mutability::Shared, t), None)
                    }
                    None => {
                        self.ni("`@test_env()`: the type `std.Env` is not declared");
                        (poison, None)
                    }
                }
            }
            "target" => {
                self.ni(
                    "`@target` used as a value other than `@target.field` or `@target.has(..)`",
                );
                (poison, None)
            }
            _ => self.simd_or_unspecified(e, name, targ, args),
        }
    }

    fn bytes_ty(&mut self) -> TypeId {
        let u8_ = self.mk(TyKind::Int(IntTy::U8));
        self.mk(TyKind::Slice(Mutability::Shared, u8_))
    }

    fn builtin_type_error(&mut self, e: ExprId, name: &str, want: &str, got: TypeId) {
        let span = self.span_of(e);
        let n = self.name(got);
        self.err("E0401", format!("`@{name}` needs {want}, not `{n}`"), span);
    }

    // ---------------------------------------------------------------- @zero

    fn zero_check(&mut self, e: ExprId, t: TypeId) {
        let span = self.span_of(e);
        match self.zero_verdict(t, 0) {
            Zero::Ok => {}
            Zero::Bad(why) => {
                self.err("E0409", format!("`@zero` is not allowed: {why}"), span);
            }
            Zero::Private(def) => {
                let n = self.name(t);
                let dpath = self.cx.sources.get(self.cx.file_of(def)).path.clone();
                let at = self.cx.sema.def(def).span;
                self.err(
                    "E0315",
                    format!("`@zero` would build a `{n}` with a field that is not visible here"),
                    span,
                )
                .relate_in("declared", &dpath, at);
            }
            Zero::Param => {
                let n = self.name(t);
                self.err(
                    "E0811",
                    format!("`@zero` of the type parameter `{n}`, which no bound grants it (`Num` and its relatives do)"),
                    span,
                );
            }
            Zero::Unknown => {
                self.ni("whether `@zero` is allowed for this type (the specification does not say)")
            }
        }
    }

    fn zero_verdict(&mut self, t: TypeId, depth: u32) -> Zero {
        if depth > 32 {
            return Zero::Unknown;
        }
        match self.kind(t) {
            TyKind::Int(_)
            | TyKind::Float(_)
            | TyKind::Bool
            | TyKind::RawPtr(..)
            | TyKind::Vector(..)
            | TyKind::Mask(_) => Zero::Ok,
            TyKind::Optional(_) => Zero::Ok,
            TyKind::Array(el, _) => self.zero_verdict(el, depth + 1),
            TyKind::Tuple(ts) => {
                for x in ts {
                    match self.zero_verdict(x, depth + 1) {
                        Zero::Ok => {}
                        other => return other,
                    }
                }
                Zero::Ok
            }
            TyKind::Adt(d, args) => match self.cx.d.items.get(&d).cloned() {
                Some(ItemSig::Struct(sig)) => {
                    let foreign = self.cx.sema.def(d).module != self.module;
                    if foreign && (sig.fields.is_empty() || sig.fields.iter().any(|f| !f.public)) {
                        return Zero::Private(d);
                    }
                    let s = self.cx.d.subst_for(d, &args);
                    for f in &sig.fields {
                        let ft = self.cx.d.instantiate(f.ty, &s);
                        match self.zero_verdict(ft, depth + 1) {
                            Zero::Ok => {}
                            other => return other,
                        }
                    }
                    Zero::Ok
                }
                Some(ItemSig::Enum(sig)) => {
                    // The variant with tag value 0, whose payload (if any) allows it.
                    match sig.variants.iter().find(|v| v.tag == 0) {
                        None => Zero::Bad(
                            "an enum with no variant at tag value 0 has no all-zero value"
                                .to_string(),
                        ),
                        Some(v) => match v.payload {
                            None => Zero::Ok,
                            Some(p) => {
                                let s = self.cx.d.subst_for(d, &args);
                                let p = self.cx.d.instantiate(p, &s);
                                self.zero_verdict(p, depth + 1)
                            }
                        },
                    }
                }
                _ => Zero::Unknown,
            },
            TyKind::Param(p) => {
                let g = self.cx.d.granted(p);
                use crate::sigs::PropBound::*;
                if [Num, Int, SInt, UInt, Float].iter().any(|b| g.contains(b)) {
                    Zero::Ok
                } else {
                    Zero::Param
                }
            }
            TyKind::Ref(..) | TyKind::Slice(..) | TyKind::Dyn(..) | TyKind::DynFn(..) => {
                Zero::Bad("the type contains a borrow".to_string())
            }
            TyKind::FnPtr(_) => Zero::Bad("a function pointer cannot be null".to_string()),
            // `T ! E` is the enum `Result { ok: T, err: E }` (section 3): its first variant
            // is `ok`, so it allows `@zero` when `T` does (section 7).
            TyKind::Result(ok, _) => self.zero_verdict(ok, depth + 1),
            TyKind::Void | TyKind::Never | TyKind::Closure(..) | TyKind::Projection(..) => {
                Zero::Bad("the type has no all-zero value".to_string())
            }
            TyKind::Poison => Zero::Ok,
        }
    }

    // ---------------------------------------------------------------- @bits

    fn bits_check(&mut self, e: ExprId, to: TypeId, from: TypeId) {
        let span = self.span_of(e);
        let mentions_param = |this: &Self, t: TypeId| this.cx.d.types.has_params(t);
        if mentions_param(self, to) || mentions_param(self, from) {
            self.err(
                "E0811",
                "`@bits` on a type that depends on a type parameter: nothing grants it",
                span,
            );
            return;
        }
        let vectorish = matches!(self.kind(to), TyKind::Vector(..) | TyKind::Mask(_))
            || matches!(self.kind(from), TyKind::Vector(..) | TyKind::Mask(_));
        if vectorish
            && let (Some(lt), Some(lf)) = (
                self.cx.layout_of_checked(to),
                self.cx.layout_of_checked(from),
            )
            && lt.size != lf.size
        {
            self.err(
                "E1503",
                "`@bits` needs one total size for a vector or mask and the other type",
                span,
            );
            return;
        }
        if let Some(def) = self.hidden_struct(to, 0) {
            let n = self.name(to);
            let dpath = self.cx.sources.get(self.cx.file_of(def)).path.clone();
            let at = self.cx.sema.def(def).span;
            self.err(
                "E0315",
                format!("`@bits` would build a `{n}` with a field that is not visible here"),
                span,
            )
            .relate_in("declared", &dpath, at);
            return;
        }
        let (Some(lt), Some(lf)) = (
            self.cx.layout_of_checked(to),
            self.cx.layout_of_checked(from),
        ) else {
            return;
        };
        let (a, b) = (self.name(to), self.name(from));
        if lt.size != lf.size {
            self.err(
                "E0408",
                format!(
                    "`@bits` needs two types of one size: `{a}` is {} bytes, `{b}` is {}",
                    lt.size, lf.size
                ),
                span,
            );
            return;
        }
        for (t, n) in [(to, &a), (from, &b)] {
            let p = self.cx.d.props(t);
            if p.view != Tri::No {
                self.err(
                    "E0408",
                    format!("`@bits` cannot involve a borrow, and `{n}` contains one"),
                    span,
                );
                return;
            }
            if self.has_padding(t, 0) {
                self.err(
                    "E0408",
                    format!("`@bits` cannot involve padding, and `{n}` has some"),
                    span,
                );
                return;
            }
        }
        if !self.every_pattern_valid(to, 0) {
            self.err(
                "E0408",
                format!("`@bits` needs a target type for which every bit pattern is valid, and `{a}` has invalid ones"),
                span,
            );
        }
    }

    /// A struct of another module, with a field that is not visible here or with no
    /// fields, among the parts of `t` (section 4.2: construction respects privacy).
    fn hidden_struct(&mut self, t: TypeId, depth: u32) -> Option<crate::defs::DefId> {
        if depth > 16 {
            return None;
        }
        match self.kind(t) {
            TyKind::Array(el, _) => self.hidden_struct(el, depth + 1),
            TyKind::Tuple(ts) => ts
                .into_iter()
                .find_map(|x| self.hidden_struct(x, depth + 1)),
            TyKind::Optional(inner) => self.hidden_struct(inner, depth + 1),
            TyKind::Adt(d, args) => {
                let s = self.cx.d.subst_for(d, &args);
                match self.cx.d.items.get(&d).cloned() {
                    Some(ItemSig::Struct(sig)) => {
                        let foreign = self.cx.sema.def(d).module != self.module;
                        if foreign
                            && (sig.fields.is_empty() || sig.fields.iter().any(|f| !f.public))
                        {
                            return Some(d);
                        }
                        sig.fields.iter().find_map(|f| {
                            let ft = self.cx.d.instantiate(f.ty, &s);
                            self.hidden_struct(ft, depth + 1)
                        })
                    }
                    Some(ItemSig::Enum(sig)) => sig.variants.iter().find_map(|v| {
                        let p = v.payload?;
                        let pt = self.cx.d.instantiate(p, &s);
                        self.hidden_struct(pt, depth + 1)
                    }),
                    _ => None,
                }
            }
            _ => None,
        }
    }

    fn has_padding(&mut self, t: TypeId, depth: u32) -> bool {
        if depth > 16 {
            return true;
        }
        if let Some(l) = self.cx.layout_of_checked(t)
            && l.padding() > 0
            && !matches!(self.kind(t), TyKind::Array(..))
        {
            return true;
        }
        match self.kind(t) {
            TyKind::Array(el, _) => self.has_padding(el, depth + 1),
            TyKind::Tuple(ts) => ts.into_iter().any(|x| self.has_padding(x, depth + 1)),
            TyKind::Adt(d, args) => match self.cx.d.struct_sig(d).cloned() {
                Some(sig) => {
                    let s = self.cx.d.subst_for(d, &args);
                    sig.fields.iter().any(|f| {
                        let ft = self.cx.d.instantiate(f.ty, &s);
                        self.has_padding(ft, depth + 1)
                    })
                }
                None => false,
            },
            _ => false,
        }
    }

    fn every_pattern_valid(&mut self, t: TypeId, depth: u32) -> bool {
        if depth > 16 {
            return false;
        }
        match self.kind(t) {
            TyKind::Int(_) | TyKind::Float(_) | TyKind::RawPtr(..) => true,
            TyKind::Vector(..) | TyKind::Mask(_) => true,
            TyKind::Array(el, _) => self.every_pattern_valid(el, depth + 1),
            TyKind::Tuple(ts) => ts
                .into_iter()
                .all(|x| self.every_pattern_valid(x, depth + 1)),
            TyKind::Adt(d, args) => match self.cx.d.struct_sig(d).cloned() {
                Some(sig) => {
                    let s = self.cx.d.subst_for(d, &args);
                    sig.fields.iter().all(|f| {
                        let ft = self.cx.d.instantiate(f.ty, &s);
                        self.every_pattern_valid(ft, depth + 1)
                    })
                }
                None => false,
            },
            _ => false,
        }
    }

    // -------------------------------------------------- SIMD and the rest

    pub(crate) fn lane_ty(&mut self, l: Lane) -> TypeId {
        match l {
            Lane::Int(i) => self.mk(TyKind::Int(i)),
            Lane::Float(f) => self.mk(TyKind::Float(f)),
        }
    }

    fn simd_or_unspecified(
        &mut self,
        e: ExprId,
        name: &str,
        targ: Option<TypeId>,
        args: &[ExprId],
    ) -> (TypeId, Option<TypeId>) {
        let poison = self.poison();
        match name {
            "lanes" => {
                let t = self.need_type(e, name, targ);
                self.arity(e, name, args, 0);
                if let Some(t) = t
                    && !matches!(self.kind(t), TyKind::Vector(..) | TyKind::Mask(_))
                {
                    self.builtin_type_error(e, name, "a vector or mask type", t);
                    return (poison, Some(t));
                }
                (self.i64_(), t)
            }
            "splat" => {
                let t = self.need_type(e, name, targ);
                if !self.arity(e, name, args, 1) {
                    return (t.unwrap_or(poison), t);
                }
                match t.map(|t| (t, self.kind(t))) {
                    Some((t, TyKind::Vector(l, _))) => {
                        let el = self.lane_ty(l);
                        self.expect(args[0], el);
                        (t, Some(t))
                    }
                    Some((t, _)) => {
                        self.expr(args[0], None);
                        self.builtin_type_error(e, name, "a vector type", t);
                        (poison, Some(t))
                    }
                    None => {
                        self.expr(args[0], None);
                        (poison, None)
                    }
                }
            }
            "iota" => {
                let t = self.need_type(e, name, targ);
                self.arity(e, name, args, 0);
                match t.map(|t| (t, self.kind(t))) {
                    Some((t, TyKind::Vector(..))) => (t, Some(t)),
                    Some((t, _)) => {
                        self.builtin_type_error(e, name, "a vector type", t);
                        (poison, Some(t))
                    }
                    None => (poison, None),
                }
            }
            "select" => {
                if !self.arity(e, name, args, 3) {
                    return (poison, None);
                }
                let span = self.span_of(e);
                let c = self.expr(args[0], Some(self.bool_()));
                let a = self.expr(args[1], None);
                let b = self.expr(args[2], Some(a));
                if self.is_poison(c) || self.is_poison(a) || self.is_poison(b) {
                    return (poison, None);
                }
                // Two vector choices of different lane counts, or a mask whose lane count
                // is not theirs, are E1503; other different choices are E0401; anything
                // else must be a `bool` condition.
                if a != b {
                    if let (TyKind::Vector(_, an), TyKind::Vector(_, bn)) =
                        (self.kind(a), self.kind(b))
                        && an != bn
                    {
                        self.err(
                            "E1503",
                            format!("`@select` between vectors of {an} and {bn} lanes"),
                            span,
                        );
                    } else {
                        self.mismatch(args[2], b, a);
                    }
                    return (poison, None);
                }
                match (self.kind(c), self.kind(a)) {
                    (TyKind::Bool, _) => {}
                    (TyKind::Mask(n), TyKind::Vector(_, m)) if m == n => {}
                    (TyKind::Mask(n), ak) => {
                        let lanes = match ak {
                            TyKind::Vector(_, m) => format!("vectors of {m} lanes"),
                            _ => format!("`{}`, which is not a vector", self.name(a)),
                        };
                        self.err(
                            "E1503",
                            format!("`@select` with a mask of {n} lanes needs vectors of {n} lanes, not {lanes}"),
                            span,
                        );
                        return (poison, None);
                    }
                    _ => {
                        self.not_bool(args[0], c);
                        return (poison, None);
                    }
                }
                (a, None)
            }
            "any" | "all" | "count" | "first" | "mask_bits" => {
                if !self.arity(e, name, args, 1) {
                    return (poison, None);
                }
                let m = self.expr(args[0], None);
                if self.is_poison(m) {
                    return (poison, None);
                }
                if !matches!(self.kind(m), TyKind::Mask(_)) {
                    self.builtin_type_error(e, name, "a lane mask", m);
                    return (poison, None);
                }
                let ty = match name {
                    "any" | "all" => self.bool_(),
                    "mask_bits" => self.mk(TyKind::Int(IntTy::U64)),
                    _ => self.i64_(),
                };
                (ty, None)
            }
            "div_or" | "rem_or" => {
                if !self.arity(e, name, args, 3) {
                    return (poison, None);
                }
                let (a, b) = self.operands(args[0], args[1], None);
                if self.is_poison(a) || self.is_poison(b) {
                    self.expr(args[2], None);
                    return (poison, None);
                }
                if matches!(self.kind(a), TyKind::Vector(..))
                    || matches!(self.kind(b), TyKind::Vector(..))
                {
                    self.expr(args[2], None);
                    self.ni(format!(
                        "`@{name}` on an integer vector (the specification gives it for integer \
                         types)"
                    ));
                    return (poison, None);
                }
                let c = self.expr(args[2], Some(a));
                if a != b {
                    self.mismatch(args[1], b, a);
                    return (poison, None);
                }
                if a != c {
                    self.mismatch(args[2], c, a);
                    return (poison, None);
                }
                if !matches!(self.kind(a), TyKind::Int(_)) {
                    if matches!(self.kind(a), TyKind::Vector(..)) {
                        self.ni(format!(
                            "`@{name}` on an integer vector (the specification gives it for \
                             integer types)"
                        ));
                    } else {
                        self.builtin_type_error(e, name, "integers", a);
                    }
                    return (poison, None);
                }
                (a, None)
            }
            "fma" | "sqrt" | "abs" | "min" | "max" | "floor" | "ceil" | "round" | "rem" => {
                let n = match name {
                    "fma" => 3,
                    "sqrt" | "abs" | "floor" | "ceil" | "round" => 1,
                    _ => 2,
                };
                if !self.arity(e, name, args, n) {
                    return (poison, None);
                }
                let first = self.expr(args[0], None);
                let mut ok = !self.is_poison(first);
                for a in &args[1..] {
                    let t = self.expr(*a, Some(first));
                    if self.is_poison(t) {
                        ok = false;
                    } else if t != first && !self.is_never(t) && ok {
                        self.mismatch(*a, t, first);
                        ok = false;
                    }
                }
                if !ok {
                    return (poison, None);
                }
                let scalar_float = matches!(self.kind(first), TyKind::Float(_));
                let valid = match (name, self.kind(first)) {
                    ("fma" | "rem", TyKind::Float(_)) => true,
                    (_, TyKind::Vector(..)) => name != "rem",
                    _ => false,
                };
                if !valid {
                    if scalar_float {
                        self.ni(format!(
                            "`@{name}` on a scalar (the specification gives it for vectors)"
                        ));
                    } else {
                        let n2 = self.name(first);
                        self.err(
                            "E0401",
                            format!("`@{name}` is not defined for `{n2}`"),
                            self.span_of(e),
                        );
                    }
                    return (poison, None);
                }
                (first, None)
            }
            "shl" | "shr" => {
                if !self.arity(e, name, args, 2) {
                    return (poison, None);
                }
                let x = self.expr(args[0], None);
                let n = self.expr(args[1], None);
                if self.is_poison(x) || self.is_poison(n) {
                    return (poison, None);
                }
                let (xk, nk) = (self.kind(x), self.kind(n));
                if matches!(xk, TyKind::Vector(..)) || matches!(nk, TyKind::Vector(..)) {
                    self.ni(format!(
                        "`@{name}` on an integer vector (the specification gives it for \
                         integer types)"
                    ));
                    return (poison, None);
                }
                if !matches!(xk, TyKind::Int(_)) || !matches!(nk, TyKind::Int(_)) {
                    self.builtin_type_error(e, name, "integers", x);
                    return (poison, None);
                }
                (x, None)
            }
            "reduce_add" | "reduce_min" | "reduce_max" | "reduce_and" | "reduce_or" => {
                if !self.arity(e, name, args, 1) {
                    return (poison, None);
                }
                let v = self.expr(args[0], None);
                if self.is_poison(v) {
                    return (poison, None);
                }
                match self.kind(v) {
                    TyKind::Vector(l, _) => {
                        let int_only = matches!(name, "reduce_and" | "reduce_or");
                        if int_only && !matches!(l, Lane::Int(_)) {
                            self.builtin_type_error(e, name, "an integer vector", v);
                            return (poison, None);
                        }
                        (self.lane_ty(l), None)
                    }
                    _ => {
                        self.builtin_type_error(e, name, "a vector", v);
                        (poison, None)
                    }
                }
            }
            "load" | "load_tail" => {
                let t = self.need_type(e, name, targ);
                if !self.arity(e, name, args, 2) {
                    return (t.unwrap_or(poison), t);
                }
                let s = self.expr(args[0], None);
                self.index_operand(args[1]);
                match (t.map(|t| (t, self.kind(t))), self.kind(s)) {
                    (Some((t, TyKind::Vector(l, _))), TyKind::Slice(_, el)) => {
                        let le = self.lane_ty(l);
                        if el != le {
                            let want = self.slice_of(le, false);
                            self.mismatch(args[0], s, want);
                            return (poison, Some(t));
                        }
                        (t, Some(t))
                    }
                    (_, TyKind::Poison) | (None, _) => (poison, t),
                    (Some((t, _)), _) => {
                        self.builtin_type_error(e, name, "a vector type and a slice", t);
                        (poison, Some(t))
                    }
                }
            }
            "store" | "store_tail" => {
                if !self.arity(e, name, args, 3) {
                    return (self.void(), None);
                }
                let s = self.expr(args[0], None);
                self.index_operand(args[1]);
                let v = self.expr(args[2], None);
                match (self.kind(s), self.kind(v)) {
                    (TyKind::Slice(Mutability::Mut, el), TyKind::Vector(l, _)) => {
                        let le = self.lane_ty(l);
                        if el != le {
                            let want = self.slice_of(le, true);
                            self.mismatch(args[0], s, want);
                        }
                    }
                    (TyKind::Poison, _) | (_, TyKind::Poison) => {}
                    _ => {
                        let n = self.name(s);
                        self.err(
                            "E0401",
                            format!("`@{name}` needs a `[]mut` slice and a vector, not `{n}`"),
                            self.span_of(e),
                        );
                    }
                }
                (self.void(), None)
            }
            "insert" => {
                if !self.arity(e, name, args, 3) {
                    return (poison, None);
                }
                let v = self.expr(args[0], None);
                self.index_operand(args[1]);
                match self.kind(v) {
                    TyKind::Vector(l, n) => {
                        let le = self.lane_ty(l);
                        self.expect(args[2], le);
                        self.check_lane_index(args[1], n);
                        (v, None)
                    }
                    TyKind::Poison => {
                        self.expr(args[2], None);
                        (poison, None)
                    }
                    _ => {
                        self.expr(args[2], None);
                        self.builtin_type_error(e, name, "a vector", v);
                        (poison, None)
                    }
                }
            }
            _ => {
                // gather, scatter, shuffle, lookup, uninit, assume, unchecked_*,
                // slice_from_raw, borrow_from_raw, name: the specification does not give
                // a precise signature.
                self.blind_args(args);
                self.ni(format!(
                    "`@{name}`: the specification does not give its signature precisely"
                ));
                (poison, targ)
            }
        }
    }

    fn slice_of(&mut self, el: TypeId, m: bool) -> TypeId {
        self.mk(TyKind::Slice(
            if m {
                Mutability::Mut
            } else {
                Mutability::Shared
            },
            el,
        ))
    }

    /// `@target.field` and `@target.has(.feature)`.
    pub(crate) fn target_field(&mut self, name: &str) -> TypeId {
        match name {
            "simd_bits" | "cache_line" | "page_size" => self.i64_(),
            "hybrid_cores" | "smt" | "gpu_unified" => self.bool_(),
            "os" | "arch" | "cpu" => {
                let ty = match name {
                    "os" => "TargetOs",
                    "arch" => "TargetArch",
                    _ => "TargetCpu",
                };
                match self.std_type("std.<builtin>", ty) {
                    Some(d) => self.mk(TyKind::Adt(d, Vec::new())),
                    None => {
                        self.ni(
                            "`@target.os`, `.arch` and `.cpu`: the builtin enum is not declared",
                        );
                        self.poison()
                    }
                }
            }
            // An unknown field is E1606, reported by name resolution.
            _ => self.poison(),
        }
    }

    pub(crate) fn is_target_builtin(&self, e: ExprId) -> bool {
        matches!(
            &self.hir.expr(self.unparen(e)).kind,
            ExprKind::Builtin { name, type_arg: None, args } if name == "target" && args.is_empty()
        )
    }

    #[allow(dead_code)]
    fn is_enum_def(&self, d: crate::defs::DefId) -> bool {
        self.cx.sema.def(d).kind == DefKind::Enum
    }

    #[allow(dead_code)]
    fn span_text(&self, s: Span) -> &str {
        self.text(s)
    }
}
