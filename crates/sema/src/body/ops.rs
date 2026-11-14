//! Literals, operators and conversions (spec sections 5.1 to 5.4, 26.2).

use super::Checker;
use crate::consteval::lossless;
use crate::defs::DefKind;
use crate::hir::{self, ExprId, ExprKind};
use crate::sigs::{ItemSig, PropBound, Tri};
use crate::ty::*;
use crate::typed::*;
use ingot_diag::source::Span;
use ingot_syntax::ast::{BinOp, UnOp};

/// The scalar types a value of some type may be: one concrete type, or, for a type
/// parameter, every type its bounds allow (section 8).
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Poss {
    pub signed: bool,
    pub unsigned: bool,
    pub float: bool,
    /// May be something that is not a number (an unbounded parameter).
    pub other: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Verdict {
    Ok,
    /// The operation is not defined for any of the types.
    Undefined,
    /// Defined for some of the types, or for none that the bounds name: not granted.
    NotGranted,
}

/// Which kinds of number an operator accepts: (signed, unsigned, float).
fn accepts(op: BinOp) -> (bool, bool, bool) {
    match op {
        BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div => (true, true, true),
        BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => (true, true, true),
        _ => (true, true, false),
    }
}

/// Whether the non-negative integer `value` is exactly representable in `f`: its
/// significant bits must fit in the float's significand (`u128` has enough bits for the
/// comparison at both widths).
fn int_fits_float(value: u128, f: FloatTy) -> bool {
    if value == 0 {
        return true;
    }
    let significand = match f {
        FloatTy::F32 => 24,
        FloatTy::F64 => 53,
    };
    (value >> value.trailing_zeros()) < (1u128 << significand)
}

impl Poss {
    pub(crate) fn verdict(self, allowed: (bool, bool, bool)) -> Verdict {
        let present = [
            (self.signed, allowed.0),
            (self.unsigned, allowed.1),
            (self.float, allowed.2),
        ];
        let any_present = present.iter().any(|(p, _)| *p);
        if self.other || !any_present {
            return Verdict::NotGranted;
        }
        let all_ok = present.iter().all(|(p, a)| !*p || *a);
        let some_ok = present.iter().any(|(p, a)| *p && *a);
        if all_ok {
            Verdict::Ok
        } else if some_ok {
            Verdict::NotGranted
        } else {
            Verdict::Undefined
        }
    }
}

impl Checker<'_, '_> {
    // ----------------------------------------------------------- classification

    /// The scalars `t` may be.
    pub(crate) fn poss(&mut self, t: TypeId) -> Poss {
        match self.kind(t) {
            TyKind::Int(i) => Poss {
                signed: i.signed(),
                unsigned: !i.signed(),
                ..Poss::default()
            },
            TyKind::Float(_) => Poss {
                float: true,
                ..Poss::default()
            },
            TyKind::Param(p) => {
                // The numeric bounds intersect: `Int + SInt` is `SInt`.
                let g = self.cx.d.granted(p);
                let (mut signed, mut unsigned, mut float) = (true, true, true);
                let mut any = false;
                for b in [
                    PropBound::Num,
                    PropBound::Int,
                    PropBound::SInt,
                    PropBound::UInt,
                    PropBound::Float,
                ] {
                    if !g.contains(&b) {
                        continue;
                    }
                    any = true;
                    match b {
                        PropBound::Int => float = false,
                        PropBound::SInt => {
                            unsigned = false;
                            float = false;
                        }
                        PropBound::UInt => {
                            signed = false;
                            float = false;
                        }
                        PropBound::Float => {
                            signed = false;
                            unsigned = false;
                        }
                        _ => {}
                    }
                }
                if any {
                    Poss {
                        signed,
                        unsigned,
                        float,
                        other: false,
                    }
                } else {
                    Poss {
                        other: true,
                        ..Poss::default()
                    }
                }
            }
            _ => Poss {
                other: true,
                ..Poss::default()
            },
        }
    }

    /// Whether `e` has no type of its own: an untyped numeric literal (possibly negated
    /// or in arithmetic with other such literals), a `.name` or `.{..}` shorthand, or
    /// `@zero`. Its type comes from the context (pipeline section 4.4).
    pub(crate) fn untyped(&self, e: ExprId) -> bool {
        match &self.hir.expr(e).kind {
            ExprKind::Int { .. } | ExprKind::Float(_) | ExprKind::StructShorthand { .. } => true,
            ExprKind::EnumLit { ty: None, .. } => true,
            ExprKind::Builtin {
                name,
                type_arg: None,
                ..
            } => name == "zero",
            ExprKind::Paren(i) => self.untyped(*i),
            ExprKind::Unary {
                op: UnOp::Neg | UnOp::NegWrap | UnOp::BitNot,
                expr,
                ..
            } => self.untyped(*expr),
            ExprKind::Binary { op, lhs, rhs, .. } if op.is_arithmetic() || op.is_bitwise() => {
                self.untyped(*lhs) && self.untyped(*rhs)
            }
            _ => false,
        }
    }

    // ----------------------------------------------------------------- literals

    pub(crate) fn int_literal(
        &mut self,
        e: ExprId,
        value: u128,
        negative: bool,
        hint: Option<TypeId>,
    ) -> TypeId {
        let span = self.span_of(e);
        let signed_value: i128 = if negative {
            -(value.min(i128::MAX as u128) as i128)
        } else {
            value.min(i128::MAX as u128) as i128
        };
        let target = hint.map(|h| self.kind(h));
        match target {
            Some(TyKind::Int(it)) => {
                let h = hint.expect("a hint");
                if value > i128::MAX as u128 || !it.contains(signed_value) {
                    let text = self.text(span).to_string();
                    self.err(
                        "E0405",
                        format!("the literal {text} does not fit `{}`", it.name()),
                        span,
                    );
                    return self.poison();
                }
                h
            }
            Some(TyKind::Float(f)) => {
                let h = hint.expect("a hint");
                if !int_fits_float(value, f) {
                    let text = self.text(span).to_string();
                    self.err(
                        "E0405",
                        format!(
                            "the literal {text} is not exactly representable in `{}`",
                            f.name()
                        ),
                        span,
                    );
                    return self.poison();
                }
                h
            }
            Some(TyKind::Param(p)) => {
                let h = hint.expect("a hint");
                let poss = self.poss(h);
                let _ = p;
                if poss.other || !(poss.signed || poss.unsigned || poss.float) {
                    return self.default_int_literal(e, value, negative);
                }
                if poss.float && !(poss.signed || poss.unsigned) {
                    let h = hint.expect("a hint");
                    if !int_fits_float(value, FloatTy::F32) {
                        let text = self.text(span).to_string();
                        self.err(
                            "E0405",
                            format!(
                                "the literal {text} is not a value of every type the bound \
                                 `Float` allows"
                            ),
                            span,
                        );
                        return self.poison();
                    }
                    return h;
                }
                // The literal must fit every type the bounds allow.
                let lo: i128 = if poss.unsigned {
                    0
                } else if poss.signed {
                    -128
                } else {
                    0
                };
                let hi: i128 = if poss.signed { 127 } else { 255 };
                if value > i128::MAX as u128 || signed_value < lo || signed_value > hi {
                    let text = self.text(span).to_string();
                    self.err(
                        "E0405",
                        format!(
                            "the literal {text} is not a value of every type the numeric bound \
                             allows"
                        ),
                        span,
                    );
                    return self.poison();
                }
                h
            }
            _ => self.default_int_literal(e, value, negative),
        }
    }

    /// Whether `e` contains a float literal, through parentheses and unary or binary
    /// untyped arithmetic.
    fn has_float_literal(&self, e: ExprId) -> bool {
        match &self.hir.expr(e).kind {
            ExprKind::Float(_) => true,
            ExprKind::Paren(i) => self.has_float_literal(*i),
            ExprKind::Unary {
                op: UnOp::Neg | UnOp::NegWrap,
                expr,
                ..
            } => self.has_float_literal(*expr),
            ExprKind::Binary { lhs, rhs, .. } => {
                self.has_float_literal(*lhs) || self.has_float_literal(*rhs)
            }
            _ => false,
        }
    }

    fn default_int_literal(&mut self, e: ExprId, value: u128, negative: bool) -> TypeId {
        let span = self.span_of(e);
        let signed_value: i128 = if negative {
            -(value.min(i128::MAX as u128) as i128)
        } else {
            value.min(i128::MAX as u128) as i128
        };
        if value > i128::MAX as u128 || !IntTy::I64.contains(signed_value) {
            let text = self.text(span).to_string();
            self.err(
                "E0405",
                format!(
                    "the literal {text} does not fit `i64`, the type of a literal without context"
                ),
                span,
            );
            return self.poison();
        }
        self.i64_()
    }

    pub(crate) fn float_literal(&mut self, e: ExprId, value: f64, hint: Option<TypeId>) -> TypeId {
        let span = self.span_of(e);
        match hint.map(|h| (h, self.kind(h))) {
            Some((h, TyKind::Float(f))) => {
                if f == FloatTy::F32 && value.is_finite() && (value as f32).is_infinite() {
                    let text = self.text(span).to_string();
                    self.err(
                        "E0405",
                        format!("the literal {text} does not fit `f32`"),
                        span,
                    );
                    return self.poison();
                }
                h
            }
            Some((h, TyKind::Param(_))) => {
                let poss = self.poss(h);
                if poss.float && !(poss.signed || poss.unsigned) {
                    h
                } else if poss.float {
                    self.err(
                        "E0401",
                        "a float literal where the numeric bound allows integer types",
                        span,
                    );
                    self.poison()
                } else {
                    self.f64_()
                }
            }
            _ => {
                let f = self.f64_();
                if !value.is_finite() {
                    self.err("E0405", "the literal does not fit `f64`", span);
                    return self.poison();
                }
                f
            }
        }
    }

    // ---------------------------------------------------------------- operators

    /// Check both operands of a binary operator. A literal takes the type of the other
    /// operand; with two literals the first takes the hint.
    pub(crate) fn operands(
        &mut self,
        lhs: ExprId,
        rhs: ExprId,
        hint: Option<TypeId>,
    ) -> (TypeId, TypeId) {
        let (lu, ru) = (self.untyped(lhs), self.untyped(rhs));
        let (l, r) = if lu && ru {
            // When both operands are untyped, an integer literal takes the type of a
            // float literal beside it (section 3).
            if self.has_float_literal(lhs) && !self.has_float_literal(rhs) {
                let l = self.expr(lhs, hint);
                let r = self.expr(rhs, Some(l));
                (l, r)
            } else {
                let r = self.expr(rhs, hint);
                let l = self.expr(lhs, Some(r));
                (l, r)
            }
        } else if lu && !ru {
            let r = self.expr(rhs, hint);
            let l = self.expr(lhs, Some(r));
            (l, r)
        } else {
            let l = self.expr(lhs, hint);
            let r = self.expr(rhs, Some(l));
            (l, r)
        };
        let poison = self.poison();
        let l = if self.unhandled_result(lhs, l) {
            poison
        } else {
            l
        };
        let r = if self.unhandled_result(rhs, r) {
            poison
        } else {
            r
        };
        (l, r)
    }

    /// A condition: `bool`, or E0403.
    pub(crate) fn cond(&mut self, e: ExprId) {
        let bool_ = self.bool_();
        let t = self.expr(e, Some(bool_));
        if !self.compatible(t, bool_) {
            self.not_bool(e, t);
        }
    }

    pub(crate) fn not_bool(&mut self, e: ExprId, t: TypeId) {
        if self.unhandled_result(e, t) {
            return;
        }
        let span = self.span_of(e);
        let name = self.name(t);
        let is_int = self.int_of(t).is_some();
        let needs_parens = self.needs_parens_for_compare(e);
        let (open, close) = if needs_parens { ("(", ")") } else { ("", "") };
        let mut edits = Vec::new();
        if needs_parens {
            edits.push(self.edit(Span::empty(span.start), open));
        }
        edits.push(self.edit(Span::empty(span.end), format!("{close} != 0")));
        let d = self.err(
            "E0403",
            format!("a condition must be `bool`, not `{name}`"),
            span,
        );
        if is_int {
            d.fix_edits("compare with zero", edits);
        }
    }

    fn needs_parens_for_compare(&self, e: ExprId) -> bool {
        match &self.hir.expr(e).kind {
            ExprKind::Binary { op, .. } => !op.is_arithmetic(),
            ExprKind::Catch { .. } | ExprKind::Orelse { .. } => true,
            _ => false,
        }
    }

    pub(crate) fn unary(
        &mut self,
        e: ExprId,
        op: UnOp,
        operand: ExprId,
        op_span: Span,
        hint: Option<TypeId>,
    ) -> (TypeId, ExprRes) {
        match op {
            UnOp::Borrow | UnOp::BorrowMut => self.borrow(e, op, operand, op_span),
            UnOp::Try => self.try_expr(e, operand, op_span),
            UnOp::Not => {
                let t = self.expr(operand, Some(self.bool_()));
                match self.kind(t) {
                    TyKind::Bool | TyKind::Mask(_) => (t, ExprRes::None),
                    _ if self.compatible(t, self.bool_()) => (self.bool_(), ExprRes::None),
                    _ => {
                        self.not_bool(operand, t);
                        (self.poison(), ExprRes::None)
                    }
                }
            }
            UnOp::Neg | UnOp::NegWrap | UnOp::BitNot => {
                let t = self.expr(operand, hint);
                if self.unhandled_result(operand, t) {
                    return (self.poison(), ExprRes::None);
                }
                (self.unary_arith(op, t, op_span), ExprRes::None)
            }
        }
    }

    /// An operand that is a type parameter must be granted the operator by its bounds
    /// (E0802), before anything else is said about the operands. True after reporting.
    fn param_operand_fails(
        &mut self,
        t: TypeId,
        allowed: (bool, bool, bool),
        sym: &str,
        op_span: Span,
    ) -> bool {
        if !matches!(self.kind(t), TyKind::Param(_)) {
            return false;
        }
        let name = self.name(t);
        match self.poss(t).verdict(allowed) {
            Verdict::Ok => false,
            Verdict::Undefined => {
                self.err(
                    "E0406",
                    format!("`{sym}` is not defined for any type `{name}` can be"),
                    op_span,
                );
                true
            }
            Verdict::NotGranted => {
                self.missing_bound(t, op_span, &format!("the operator `{sym}`"));
                true
            }
        }
    }

    fn unary_arith(&mut self, op: UnOp, t: TypeId, op_span: Span) -> TypeId {
        if self.is_poison(t) || self.is_never(t) {
            return self.poison();
        }
        let sym = match op {
            UnOp::Neg => "-",
            UnOp::NegWrap => "-%",
            _ => "~",
        };
        let name = self.name(t);
        match self.kind(t) {
            TyKind::Int(i) => {
                let ok = match op {
                    UnOp::Neg => i.signed(),
                    _ => true,
                };
                if ok {
                    t
                } else {
                    self.err(
                        "E0406",
                        format!(
                            "unary `-` is not defined for the unsigned type `{name}`; `-%` wraps"
                        ),
                        op_span,
                    );
                    self.poison()
                }
            }
            TyKind::Float(_) => {
                if op == UnOp::Neg {
                    t
                } else {
                    self.err(
                        "E0406",
                        format!("`{sym}` is not defined for the floating-point type `{name}`"),
                        op_span,
                    );
                    self.poison()
                }
            }
            TyKind::Vector(l, _) => {
                if op == UnOp::BitNot && matches!(l, Lane::Int(_)) {
                    t
                } else {
                    self.ni(format!(
                        "unary `{sym}` on a vector (the specification lists only `~` for \
                         integer vectors)"
                    ));
                    self.poison()
                }
            }
            TyKind::Param(_) => {
                let poss = self.poss(t);
                let allowed = match op {
                    UnOp::Neg => (true, false, true),
                    UnOp::NegWrap | UnOp::BitNot => (true, true, false),
                    _ => (false, false, false),
                };
                match poss.verdict(allowed) {
                    Verdict::Ok => t,
                    Verdict::Undefined => {
                        self.err(
                            "E0406",
                            format!("`{sym}` is not defined for any type `{name}` can be"),
                            op_span,
                        );
                        self.poison()
                    }
                    Verdict::NotGranted => {
                        self.missing_bound(t, op_span, &format!("the operator `{sym}`"));
                        self.poison()
                    }
                }
            }
            _ => {
                self.err(
                    "E0406",
                    format!("`{sym}` is not defined for `{name}`"),
                    op_span,
                );
                self.poison()
            }
        }
    }

    pub(crate) fn binary(
        &mut self,
        e: ExprId,
        op: BinOp,
        lhs: ExprId,
        rhs: ExprId,
        op_span: Span,
        hint: Option<TypeId>,
    ) -> (TypeId, ExprRes) {
        let _ = e;
        match op {
            BinOp::And | BinOp::Or => {
                self.cond(lhs);
                self.cond(rhs);
                (self.bool_(), ExprRes::None)
            }
            BinOp::Shl | BinOp::Shr => {
                let l = self.expr(lhs, hint);
                let r = self.expr(rhs, None);
                if self.unhandled_result(lhs, l) || self.unhandled_result(rhs, r) {
                    return (self.poison(), ExprRes::None);
                }
                (self.shift(op, l, r, op_span), ExprRes::None)
            }
            _ if op.is_comparison() => {
                let (l, r) = self.operands(lhs, rhs, None);
                (self.compare(op, lhs, rhs, l, r, op_span), ExprRes::None)
            }
            _ => {
                let (l, r) = self.operands(lhs, rhs, hint);
                (self.arith(op, l, r, op_span), ExprRes::None)
            }
        }
    }

    /// The type of `l op r` for an arithmetic or bitwise operator, or poison after an
    /// error. Used by compound assignment too.
    pub(crate) fn arith(&mut self, op: BinOp, l: TypeId, r: TypeId, op_span: Span) -> TypeId {
        if self.is_poison(l) || self.is_poison(r) {
            return self.poison();
        }
        if self.is_never(l) || self.is_never(r) {
            return self.poison();
        }
        let sym = op.symbol();
        let allowed = accepts(op);
        if self.param_operand_fails(l, allowed, sym, op_span)
            || self.param_operand_fails(r, allowed, sym, op_span)
        {
            return self.poison();
        }
        if l != r {
            let (a, b) = (self.name(l), self.name(r));
            self.err(
                "E0406",
                format!("the operands of `{sym}` have different types, `{a}` and `{b}`"),
                op_span,
            );
            return self.poison();
        }
        let name = self.name(l);
        match self.kind(l) {
            TyKind::Int(_) | TyKind::Float(_) => match self.poss(l).verdict(allowed) {
                Verdict::Ok => l,
                _ => {
                    self.err(
                        "E0406",
                        format!("`{sym}` is not defined for `{name}`"),
                        op_span,
                    );
                    self.poison()
                }
            },
            TyKind::Vector(lane, _) => {
                let is_float = matches!(lane, Lane::Float(_));
                let ok = if is_float {
                    matches!(op, BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div)
                } else {
                    // Section 26.2 lists `+% -% *%` and `+| -|`: there is no saturating
                    // multiply on integer vectors.
                    matches!(
                        op,
                        BinOp::AddWrap
                            | BinOp::SubWrap
                            | BinOp::MulWrap
                            | BinOp::AddSat
                            | BinOp::SubSat
                            | BinOp::BitAnd
                            | BinOp::BitOr
                            | BinOp::BitXor
                    )
                };
                if ok {
                    l
                } else if !is_float && matches!(op, BinOp::Add | BinOp::Sub | BinOp::Mul) {
                    self.err(
                        "E0407",
                        format!(
                            "`{sym}` on the integer vector `{name}` has no cheap per-lane overflow \
                             trap: write `{sym}%` (wrapping) or `{sym}|` (saturating)"
                        ),
                        op_span,
                    );
                    self.poison()
                } else {
                    self.err(
                        "E0406",
                        format!("`{sym}` is not defined for the vector `{name}`"),
                        op_span,
                    );
                    self.poison()
                }
            }
            TyKind::Mask(_) => {
                if op.is_bitwise() {
                    l
                } else {
                    self.err(
                        "E0406",
                        format!("`{sym}` is not defined for the mask `{name}`"),
                        op_span,
                    );
                    self.poison()
                }
            }
            TyKind::Param(_) => match self.poss(l).verdict(allowed) {
                Verdict::Ok => l,
                Verdict::Undefined => {
                    self.err(
                        "E0406",
                        format!("`{sym}` is not defined for any type `{name}` can be"),
                        op_span,
                    );
                    self.poison()
                }
                Verdict::NotGranted => {
                    self.missing_bound(l, op_span, &format!("the operator `{sym}`"));
                    self.poison()
                }
            },
            _ => {
                self.err(
                    "E0406",
                    format!("`{sym}` is not defined for `{name}`"),
                    op_span,
                );
                self.poison()
            }
        }
    }

    pub(crate) fn shift(&mut self, op: BinOp, l: TypeId, r: TypeId, op_span: Span) -> TypeId {
        if self.is_poison(l) || self.is_poison(r) || self.is_never(l) || self.is_never(r) {
            return self.poison();
        }
        let sym = op.symbol();
        // The count may have any integer type.
        let count_ok = match self.kind(r) {
            TyKind::Int(_) => true,
            TyKind::Param(_) => {
                let p = self.poss(r);
                if p.verdict((true, true, false)) == Verdict::Ok {
                    true
                } else {
                    self.missing_bound(r, op_span, "a shift count");
                    return self.poison();
                }
            }
            _ => false,
        };
        if !count_ok {
            let name = self.name(r);
            self.err(
                "E0406",
                format!("the count of `{sym}` must be an integer, not `{name}`"),
                op_span,
            );
            return self.poison();
        }
        let name = self.name(l);
        match self.kind(l) {
            TyKind::Int(_) => l,
            TyKind::Vector(Lane::Int(_), _) => l,
            TyKind::Param(_) => match self.poss(l).verdict((true, true, false)) {
                Verdict::Ok => l,
                Verdict::Undefined => {
                    self.err(
                        "E0406",
                        format!("`{sym}` is not defined for any type `{name}` can be"),
                        op_span,
                    );
                    self.poison()
                }
                Verdict::NotGranted => {
                    self.missing_bound(l, op_span, &format!("the operator `{sym}`"));
                    self.poison()
                }
            },
            _ => {
                self.err(
                    "E0406",
                    format!("`{sym}` is not defined for `{name}`"),
                    op_span,
                );
                self.poison()
            }
        }
    }

    fn compare(
        &mut self,
        op: BinOp,
        lhs: ExprId,
        rhs: ExprId,
        l: TypeId,
        r: TypeId,
        op_span: Span,
    ) -> TypeId {
        if self.is_poison(l) || self.is_poison(r) || self.is_never(l) || self.is_never(r) {
            return self.poison();
        }
        let sym = op.symbol();
        if self.param_operand_fails(l, accepts(op), sym, op_span)
            || self.param_operand_fails(r, accepts(op), sym, op_span)
        {
            return self.poison();
        }
        if l != r {
            let (a, b) = (self.name(l), self.name(r));
            self.err(
                "E0406",
                format!("the operands of `{sym}` have different types, `{a}` and `{b}`"),
                op_span,
            );
            return self.poison();
        }
        let name = self.name(l);
        match self.kind(l) {
            TyKind::Int(_) | TyKind::Float(_) | TyKind::Bool | TyKind::RawPtr(..) => self.bool_(),
            TyKind::Vector(_, n) => self.mk(TyKind::Mask(n)),
            TyKind::Adt(d, _) if self.cx.d.is_payload_free_enum(d) => self.bool_(),
            TyKind::Param(_) => match self.poss(l).verdict(accepts(op)) {
                Verdict::Ok => self.bool_(),
                Verdict::Undefined => {
                    self.err(
                        "E0406",
                        format!("`{sym}` is not defined for any type `{name}` can be"),
                        op_span,
                    );
                    self.poison()
                }
                Verdict::NotGranted => {
                    self.missing_bound(l, op_span, &format!("the operator `{sym}`"));
                    self.poison()
                }
            },
            _ => {
                let eq_fix = self.eq_rewrite(op, lhs, rhs, l);
                let d = self.err(
                    "E0406",
                    format!(
                        "`{sym}` works only on scalars, raw pointers and payload-free enums, \
                         not `{name}`"
                    ),
                    op_span,
                );
                if let Some(edit) = eq_fix {
                    d.fix("call `eq` instead", edit);
                }
                self.poison()
            }
        }
    }

    /// The rewrite of `a == b` to `a.eq(&b)` when the type implements `Eq`.
    fn eq_rewrite(
        &mut self,
        op: BinOp,
        lhs: ExprId,
        rhs: ExprId,
        ty: TypeId,
    ) -> Option<ingot_diag::diagnostic::Edit> {
        if op != BinOp::Eq {
            return None;
        }
        let eq = self.cx.d.builtins.eq?;
        if self
            .cx
            .d
            .satisfies(ty, &crate::sigs::Bound::Interface(eq, Vec::new()))
            != Tri::Yes
        {
            return None;
        }
        let whole = self.span_of(lhs).join(self.span_of(rhs));
        let l = self.text(self.span_of(lhs)).to_string();
        let r = self.text(self.span_of(rhs)).to_string();
        let lhs_simple = matches!(
            &self.hir.expr(lhs).kind,
            ExprKind::Path(_)
                | ExprKind::Field { .. }
                | ExprKind::TupleField { .. }
                | ExprKind::Call { .. }
                | ExprKind::Index { .. }
                | ExprKind::Paren(_)
        );
        let l = if lhs_simple { l } else { format!("({l})") };
        Some(self.edit(whole, format!("{l}.eq(&{r})")))
    }

    // ------------------------------------------------------------------- casts

    pub(crate) fn cast(
        &mut self,
        e: ExprId,
        operand: ExprId,
        ty: &hir::Ty,
        as_span: Span,
    ) -> (TypeId, ExprRes) {
        let _ = e;
        let target = self.lower(ty);
        let from = self.expr(operand, None);
        if self.is_poison(from) || self.is_poison(target) || self.is_never(from) {
            return (target, ExprRes::Cast(CastKind::Identity));
        }
        match self.cast_kind(from, target) {
            Ok(k) => (target, ExprRes::Cast(k)),
            Err(CastError::Lossy) => {
                let (a, b) = (self.name(from), self.name(target));
                let advice = self.cast_advice(from, target);
                self.err(
                    "E0402",
                    format!(
                        "`as` converts only without loss, and not every `{a}` is a `{b}`{advice}"
                    ),
                    as_span,
                );
                (target, ExprRes::Cast(CastKind::Identity))
            }
            Err(CastError::PayloadEnum) => {
                let a = self.name(from);
                self.err(
                    "E0417",
                    format!("`as` does not convert the enum `{a}`, which has payloads"),
                    as_span,
                );
                (target, ExprRes::Cast(CastKind::Identity))
            }
            Err(CastError::Shape) => {
                self.err(
                    "E1503",
                    "`as` between vectors needs the same lane count",
                    as_span,
                );
                (target, ExprRes::Cast(CastKind::Identity))
            }
            Err(CastError::Unspecified(what)) => {
                self.ni(what);
                (self.poison(), ExprRes::Cast(CastKind::Identity))
            }
        }
    }

    fn cast_advice(&self, from: TypeId, to: TypeId) -> String {
        let f = self.cx.d.types.kind(from).clone();
        let t = self.cx.d.types.kind(to).clone();
        let numeric = |k: &TyKind| matches!(k, TyKind::Int(_) | TyKind::Float(_));
        if !numeric(&f) || !numeric(&t) {
            return String::new();
        }
        let mut options = Vec::new();
        if matches!(t, TyKind::Int(_)) {
            options.push("`@wrap`");
            options.push("`@narrow`");
            options.push("`@sat`");
        } else if matches!(t, TyKind::Float(_)) {
            options.push("`@nearest`");
        }
        format!("; use {}", options.join(", "))
    }

    /// Which conversion `from as to` is, if it is lossless (section 5.4).
    pub(crate) fn cast_kind(&mut self, from: TypeId, to: TypeId) -> Result<CastKind, CastError> {
        if from == to {
            return Ok(CastKind::Identity);
        }
        let (fk, tk) = (self.kind(from), self.kind(to));
        match (&fk, &tk) {
            (TyKind::Int(a), TyKind::Int(b)) => {
                if lossless(*a, *b) {
                    Ok(CastKind::IntWiden)
                } else {
                    Err(CastError::Lossy)
                }
            }
            (TyKind::Int(a), TyKind::Float(FloatTy::F64)) if a.bits() <= 32 => {
                Ok(CastKind::ToFloat)
            }
            (TyKind::Int(a), TyKind::Float(FloatTy::F32)) if a.bits() <= 16 => {
                Ok(CastKind::ToFloat)
            }
            (TyKind::Float(FloatTy::F32), TyKind::Float(FloatTy::F64)) => Ok(CastKind::ToFloat),
            (TyKind::Bool, TyKind::Int(_)) => Ok(CastKind::BoolToInt),
            (TyKind::Adt(d, _), TyKind::Int(t)) => {
                let d = *d;
                let payload_free = self.cx.d.is_payload_free_enum(d);
                let is_enum = matches!(self.cx.d.items.get(&d), Some(ItemSig::Enum(_)));
                if !is_enum {
                    return Err(CastError::Lossy);
                }
                if !payload_free {
                    return Err(CastError::PayloadEnum);
                }
                let tag = match self.cx.d.items.get(&d) {
                    Some(ItemSig::Enum(e)) => e.tag_ty,
                    _ => return Err(CastError::Lossy),
                };
                // "A payload-free enum to its tag type": exactly that type.
                if tag == *t {
                    Ok(CastKind::EnumToTag)
                } else {
                    Err(CastError::Lossy)
                }
            }
            (TyKind::Ref(m, a), TyKind::RawPtr(n, b))
                if m == n || *m == Mutability::Mut && *n == Mutability::Shared =>
            {
                if m == n && a == b {
                    Ok(CastKind::RefToPtr)
                } else {
                    Err(CastError::Lossy)
                }
            }
            (TyKind::Param(_), _) | (_, TyKind::Param(_)) => Err(CastError::Unspecified(
                "`as` between a type parameter and another type (the specification does not say \
                 when a conversion is lossless for every instance)"
                    .to_string(),
            )),
            (TyKind::Vector(a, n), TyKind::Vector(b, m)) => {
                if n != m {
                    return Err(CastError::Shape);
                }
                let (from_lane, to_lane) = (self.lane_ty(*a), self.lane_ty(*b));
                match self.cast_kind(from_lane, to_lane) {
                    Ok(k) => Ok(k),
                    Err(CastError::Lossy) => Err(CastError::Lossy),
                    Err(CastError::PayloadEnum) => Err(CastError::PayloadEnum),
                    Err(CastError::Shape) => Err(CastError::Shape),
                    Err(CastError::Unspecified(_)) => Err(CastError::Unspecified(
                        "`as` between vectors of one lane count with an undefined lane \
                         conversion"
                            .to_string(),
                    )),
                }
            }
            (TyKind::Mask(_), _) | (_, TyKind::Mask(_)) => Err(CastError::Unspecified(
                "`as` from or to a mask (section 26.2 gives the conversions between vectors \
                 only)"
                    .to_string(),
            )),
            // Section 5.4 lists no conversion between a vector and a scalar or another
            // kind of type, so `as` does not compile (E0402).
            (TyKind::Vector(..), _) | (_, TyKind::Vector(..)) => Err(CastError::Lossy),
            _ => Err(CastError::Lossy),
        }
    }

    /// Whether `from` converts to `to` without loss, for the fix of E0401 and E0415.
    pub(crate) fn lossless_to(&mut self, from: TypeId, to: TypeId) -> bool {
        matches!(
            self.cast_kind(from, to),
            Ok(CastKind::IntWiden | CastKind::ToFloat | CastKind::BoolToInt)
        )
    }

    /// An edit that appends ` as T` to the expression (parenthesizing it when it binds
    /// looser than `as`).
    pub(crate) fn append_as(&mut self, e: ExprId, to: TypeId) -> Vec<ingot_diag::diagnostic::Edit> {
        let span = self.span_of(e);
        let name = self.name(to);
        let loose = matches!(
            &self.hir.expr(e).kind,
            ExprKind::Binary { .. } | ExprKind::Catch { .. } | ExprKind::Orelse { .. }
        );
        if loose {
            vec![
                self.edit(Span::empty(span.start), "("),
                self.edit(Span::empty(span.end), format!(") as {name}")),
            ]
        } else {
            vec![self.edit(Span::empty(span.end), format!(" as {name}"))]
        }
    }

    /// The `DefKind` of a type's definition, for messages.
    #[allow(dead_code)]
    pub(crate) fn kind_word(&self, t: TypeId) -> &'static str {
        match self.cx.d.types.kind(t) {
            TyKind::Adt(d, _) => match self.cx.sema.def(*d).kind {
                DefKind::Struct => "struct",
                DefKind::Enum => "enum",
                _ => "type",
            },
            _ => "type",
        }
    }
}

#[derive(Debug)]
pub(crate) enum CastError {
    Lossy,
    PayloadEnum,
    /// Two vectors with different lane counts (E1503).
    Shape,
    Unspecified(String),
}
