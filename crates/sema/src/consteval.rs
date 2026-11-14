//! Constant evaluation, first part (spec section 9).
//!
//! This evaluates the constants that signatures need: array lengths, enum tag values,
//! `align(...)` arguments, constant generic arguments and `const` initializers, as long
//! as they are built only from integer and `bool` literals, arithmetic, comparisons,
//! `&& || !`, parentheses, `as` between integer types, references to other such
//! constants, `@sizeof`, `@alignof` and `@target`. Arithmetic follows section 5.2: an
//! operation that traps is error E0806 with the trap's code and text.
//!
//! Everything else (a function call, a struct value, a float, a `static`, ...) is
//! recorded as `not implemented: constant evaluation of <what>`; a value is never
//! guessed. `ConstValue` is the value type; a later step adds floats, aggregates and
//! enum values to it.

use crate::defs::{DefId, DefKind, Sema};
use crate::hir::{self, ExprId, ExprKind, Res};
use crate::lowering::{Cur, Cx};
use crate::sigs::ItemSig;
use crate::ty::{ConstArg, ConstValue, IntTy, TyKind};
use ingot_diag::source::Span;
use ingot_syntax::ast::{BinOp, UnOp};

/// Why a constant has no value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Fail {
    /// An error was reported already; nothing more is said about this constant.
    Reported,
    /// The step that evaluates this does not exist yet.
    NotImplemented(String),
    /// The value depends on a type or constant generic parameter, so it is known only
    /// for each instance (phase 11).
    Generic,
    /// An integer literal does not fit the type its context gives it (E0405 belongs to
    /// the expression checker; enum tags report it as E0505).
    OutOfRange(Span),
}

#[derive(Clone, Debug)]
pub enum ConstState {
    InProgress,
    Done(Result<ConstValue, Fail>),
}

/// The type of a constant expression in the evaluated subset.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ValTy {
    Int(IntTy),
    Bool,
}

impl ValTy {
    pub fn of(v: ConstValue) -> ValTy {
        match v {
            ConstValue::Int { ty, .. } => ValTy::Int(ty),
            ConstValue::Bool(_) => ValTy::Bool,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            ValTy::Int(i) => i.name(),
            ValTy::Bool => "bool",
        }
    }
}

// ------------------------------------------------------------------ arithmetic

/// The traps constant evaluation can meet (diagnostics.md section 18).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Trap {
    /// T03, with the operator.
    Overflow(&'static str),
    /// T04
    DivisionByZero,
    /// T05
    DivisionOverflow,
    /// T06
    ShiftRange { count: i128, bits: u32 },
}

impl Trap {
    pub fn code(&self) -> &'static str {
        match self {
            Trap::Overflow(_) => "T03",
            Trap::DivisionByZero => "T04",
            Trap::DivisionOverflow => "T05",
            Trap::ShiftRange { .. } => "T06",
        }
    }

    pub fn text(&self) -> String {
        match self {
            Trap::Overflow(op) => format!("integer overflow in {op}"),
            Trap::DivisionByZero => "division by zero".to_string(),
            Trap::DivisionOverflow => "integer overflow in division".to_string(),
            Trap::ShiftRange { count, bits } => {
                format!("shift count {count} out of range for {bits}-bit value")
            }
        }
    }
}

/// `v` reduced modulo 2^bits into the range of `ty`.
pub fn wrap(ty: IntTy, v: i128) -> i128 {
    let m = 1i128 << ty.bits();
    let mut r = v.rem_euclid(m);
    if ty.signed() && r >= m / 2 {
        r -= m;
    }
    r
}

fn clamp(ty: IntTy, v: i128) -> i128 {
    v.clamp(ty.min(), ty.max())
}

/// An integer operation of section 5.2 on two values of `ty`. Comparisons and the
/// short-circuit operators are not here; shifts take any count.
pub fn int_binary(op: BinOp, ty: IntTy, a: i128, b: i128) -> Result<i128, Trap> {
    let checked = |r: Option<i128>| match r {
        Some(r) if ty.contains(r) => Ok(r),
        _ => Err(Trap::Overflow(op.symbol())),
    };
    match op {
        BinOp::Add => checked(a.checked_add(b)),
        BinOp::Sub => checked(a.checked_sub(b)),
        BinOp::Mul => checked(a.checked_mul(b)),
        BinOp::AddWrap => Ok(wrap(ty, a.wrapping_add(b))),
        BinOp::SubWrap => Ok(wrap(ty, a.wrapping_sub(b))),
        BinOp::MulWrap => Ok(wrap(ty, a.wrapping_mul(b))),
        BinOp::AddSat => Ok(clamp(ty, a.saturating_add(b))),
        BinOp::SubSat => Ok(clamp(ty, a.saturating_sub(b))),
        BinOp::MulSat => Ok(clamp(ty, a.saturating_mul(b))),
        BinOp::Div | BinOp::Rem => {
            if b == 0 {
                return Err(Trap::DivisionByZero);
            }
            if ty.signed() && a == ty.min() && b == -1 {
                return Err(Trap::DivisionOverflow);
            }
            // Truncation toward zero; `%` has the sign of the dividend.
            Ok(if op == BinOp::Div { a / b } else { a % b })
        }
        BinOp::BitAnd => Ok(a & b),
        BinOp::BitOr => Ok(a | b),
        BinOp::BitXor => Ok(a ^ b),
        BinOp::Shl | BinOp::Shr => {
            let bits = ty.bits();
            if b < 0 || b >= i128::from(bits) {
                return Err(Trap::ShiftRange { count: b, bits });
            }
            Ok(if op == BinOp::Shl {
                wrap(ty, a << b)
            } else {
                a >> b
            })
        }
        BinOp::Eq
        | BinOp::Ne
        | BinOp::Lt
        | BinOp::Le
        | BinOp::Gt
        | BinOp::Ge
        | BinOp::And
        | BinOp::Or => unreachable!("not an integer operation"),
    }
}

/// Unary `-` (traps on the minimum) and `-%` of a signed integer, `~` of any integer.
pub fn int_unary(op: UnOp, ty: IntTy, a: i128) -> Result<i128, Trap> {
    match op {
        UnOp::Neg => {
            if a == ty.min() {
                Err(Trap::Overflow("-"))
            } else {
                Ok(-a)
            }
        }
        UnOp::NegWrap => Ok(wrap(ty, -a)),
        UnOp::BitNot => Ok(if ty.signed() { !a } else { ty.max() - a }),
        _ => unreachable!("not an integer operation"),
    }
}

/// Every value of `from` is a value of `to` (section 5.4, for integers).
pub fn lossless(from: IntTy, to: IntTy) -> bool {
    to.min() <= from.min() && from.max() <= to.max()
}

// ------------------------------------------------------------------- evaluator

impl Cx<'_> {
    /// A short description of an expression outside the evaluated subset.
    fn describe(&self, pkg: usize, e: ExprId) -> String {
        let p = &self.pkgs[pkg];
        match &p.expr(e).kind {
            ExprKind::Float(_) => "a floating-point value".to_string(),
            ExprKind::Str(_) => "a string literal".to_string(),
            ExprKind::CStr(_) => "a C string literal".to_string(),
            ExprKind::Call { .. } => "a function call".to_string(),
            ExprKind::StructLit { .. } | ExprKind::StructShorthand { .. } => {
                "a struct value".to_string()
            }
            ExprKind::EnumLit { .. } => "an enum value".to_string(),
            ExprKind::ArrayLit { .. } => "an array value".to_string(),
            ExprKind::Tuple(_) => "a tuple value".to_string(),
            ExprKind::Range { .. } => "a range".to_string(),
            ExprKind::If { .. } => "an `if` expression".to_string(),
            ExprKind::Match { .. } => "a `match` expression".to_string(),
            ExprKind::Closure(_) => "a closure".to_string(),
            ExprKind::Index { .. } | ExprKind::Slice { .. } => "an index or slice".to_string(),
            ExprKind::Field { .. } | ExprKind::TupleField { .. } => "a field access".to_string(),
            ExprKind::Deref { .. } => "a dereference".to_string(),
            ExprKind::TypeApply { .. } => "a type application".to_string(),
            ExprKind::Unary {
                op: UnOp::Borrow | UnOp::BorrowMut,
                ..
            } => "a borrow".to_string(),
            ExprKind::Unary {
                op: UnOp::Try | UnOp::Neg | UnOp::NegWrap | UnOp::Not | UnOp::BitNot,
                ..
            } => "an operator".to_string(),
            ExprKind::Builtin { name, .. } => format!("`@{name}`"),
            ExprKind::Path(path) => format!(
                "the name `{}`",
                path.segments.last().map(|(n, _)| n.as_str()).unwrap_or("")
            ),
            ExprKind::Catch { .. } | ExprKind::Orelse { .. } => "an error handler".to_string(),
            ExprKind::Spawn { .. } | ExprKind::Dispatch { .. } => "a spawn or dispatch".to_string(),
            ExprKind::Unsafe { .. } => "an `unsafe` block".to_string(),
            _ => "an expression".to_string(),
        }
    }

    fn not_in_subset<T>(&self, pkg: usize, e: ExprId) -> Result<T, Fail> {
        Err(Fail::NotImplemented(format!(
            "constant evaluation of {}",
            self.describe(pkg, e)
        )))
    }

    fn mismatch<T>(&self, what: &str) -> Result<T, Fail> {
        Err(Fail::NotImplemented(format!(
            "type checking of a constant expression ({what})"
        )))
    }

    fn trapped<T>(&mut self, file: usize, span: Span, trap: &Trap) -> Result<T, Fail> {
        let message = format!(
            "constant evaluation trapped: {}: {}",
            trap.code(),
            trap.text()
        );
        self.error("E0806", message, file, span);
        Err(Fail::Reported)
    }

    /// The type an expression has by itself, or `None` for an untyped integer
    /// literal expression, which takes the type its context expects (pipeline
    /// section 4.4).
    fn const_type(&mut self, pkg: usize, e: ExprId) -> Result<Option<ValTy>, Fail> {
        let p = &self.pkgs[pkg];
        let expr = p.expr(e);
        match &expr.kind {
            ExprKind::Int { .. } => Ok(None),
            ExprKind::Bool(_) => Ok(Some(ValTy::Bool)),
            ExprKind::Paren(inner) => self.const_type(pkg, *inner),
            ExprKind::Path(path) => match path.res {
                Res::Poison => Err(Fail::Reported),
                Res::Def(d) if self.sema.def(d).kind == DefKind::Const => {
                    self.const_decl_type(d).map(Some)
                }
                _ => Ok(Some(self.path_value_type(pkg, e)?)),
            },
            ExprKind::Cast { ty, .. } => {
                let t = self.lower_ty_in(pkg, ty);
                match self.d.types.kind(t) {
                    TyKind::Int(i) => Ok(Some(ValTy::Int(*i))),
                    TyKind::Poison => Err(Fail::Reported),
                    _ => Err(Fail::NotImplemented(
                        "constant evaluation of a conversion to a type that is not an integer"
                            .to_string(),
                    )),
                }
            }
            ExprKind::Builtin { name, .. }
                if name == "sizeof" || name == "alignof" || name == "lanes" =>
            {
                Ok(Some(ValTy::Int(IntTy::I64)))
            }
            ExprKind::Field { base, name, .. } if self.is_target(pkg, *base) => {
                match name.as_str() {
                    "simd_bits" | "cache_line" | "page_size" => Ok(Some(ValTy::Int(IntTy::I64))),
                    "hybrid_cores" | "smt" | "gpu_unified" => Ok(Some(ValTy::Bool)),
                    _ => self.not_in_subset(pkg, e),
                }
            }
            ExprKind::Call { callee, .. } if self.is_target_has(pkg, *callee) => {
                Ok(Some(ValTy::Bool))
            }
            ExprKind::Unary { op: UnOp::Not, .. } => Ok(Some(ValTy::Bool)),
            ExprKind::Unary {
                op: UnOp::Neg | UnOp::NegWrap | UnOp::BitNot,
                expr,
                ..
            } => self.const_type(pkg, *expr),
            ExprKind::Binary { op, lhs, rhs, .. } => {
                if op.is_comparison() || matches!(op, BinOp::And | BinOp::Or) {
                    Ok(Some(ValTy::Bool))
                } else if op.is_shift() {
                    self.const_type(pkg, *lhs)
                } else {
                    let l = self.const_type(pkg, *lhs)?;
                    match l {
                        Some(t) => Ok(Some(t)),
                        None => self.const_type(pkg, *rhs),
                    }
                }
            }
            _ => self.not_in_subset(pkg, e),
        }
    }

    /// The type of a value named by a path other than a `const` item.
    fn path_value_type(&mut self, pkg: usize, e: ExprId) -> Result<ValTy, Fail> {
        let ExprKind::Path(path) = &self.pkgs[pkg].expr(e).kind else {
            return self.not_in_subset(pkg, e);
        };
        match path.res {
            Res::Def(d) => match self.sema.def(d).kind {
                DefKind::Static => Err(Fail::NotImplemented(
                    "constant evaluation of a reference to a static".to_string(),
                )),
                DefKind::GenericParam { .. } => Err(Fail::Generic),
                _ => self.not_in_subset(pkg, e),
            },
            _ => self.not_in_subset(pkg, e),
        }
    }

    fn is_target(&self, pkg: usize, e: ExprId) -> bool {
        matches!(
            &self.pkgs[pkg].expr(e).kind,
            ExprKind::Builtin { name, type_arg: None, args } if name == "target" && args.is_empty()
        )
    }

    fn is_target_has(&self, pkg: usize, callee: ExprId) -> bool {
        matches!(
            &self.pkgs[pkg].expr(callee).kind,
            ExprKind::Field { base, name, .. } if name == "has" && self.is_target(pkg, *base)
        )
    }

    /// The value of a `@target.os`-style enum field and the variant `.name` it is
    /// compared with, if `e` is one of them.
    fn target_enum_field(&self, pkg: usize, e: ExprId) -> Option<(&'static str, &'static str)> {
        let p = &self.pkgs[pkg];
        match &p.expr(e).kind {
            ExprKind::Paren(inner) => self.target_enum_field(pkg, *inner),
            ExprKind::Field { base, name, .. } if self.is_target(pkg, *base) => {
                match name.as_str() {
                    "os" => Some(("os", self.target.os)),
                    "arch" => Some(("arch", self.target.arch)),
                    "cpu" => Some(("cpu", self.target.cpu)),
                    _ => None,
                }
            }
            _ => None,
        }
    }

    fn shorthand_variant(&self, pkg: usize, e: ExprId) -> Option<&str> {
        match &self.pkgs[pkg].expr(e).kind {
            ExprKind::Paren(inner) => self.shorthand_variant(pkg, *inner),
            ExprKind::EnumLit {
                ty: None,
                variant,
                payload: None,
                ..
            } => Some(variant.as_str()),
            _ => None,
        }
    }

    /// The type of a `const` item, which must be an integer or `bool` for its value
    /// to be evaluated here.
    fn const_decl_type(&mut self, d: DefId) -> Result<ValTy, Fail> {
        self.ensure_sig(d);
        let Some(ItemSig::Const(c)) = self.d.items.get(&d) else {
            return Err(Fail::Reported);
        };
        let ty = c.ty;
        match self.d.types.kind(ty) {
            TyKind::Int(i) => Ok(ValTy::Int(*i)),
            TyKind::Bool => Ok(ValTy::Bool),
            TyKind::Poison => Err(Fail::Reported),
            _ => {
                let name = self.d.types.display(self.sema, ty);
                Err(Fail::NotImplemented(format!(
                    "constant evaluation of a value of type `{name}`"
                )))
            }
        }
    }

    /// Evaluate `e` of package `pkg` in the file `file`. `hint` is the type the
    /// context expects, which an untyped literal takes.
    pub(crate) fn eval_expr(
        &mut self,
        pkg: usize,
        file: usize,
        e: ExprId,
        hint: Option<ValTy>,
    ) -> Result<ConstValue, Fail> {
        let value = self.eval_inner(pkg, file, e, hint)?;
        if let Some(h) = hint
            && ValTy::of(value) != h
        {
            return self.mismatch(&format!(
                "a value of type {} where {} is expected",
                ValTy::of(value).name(),
                h.name()
            ));
        }
        Ok(value)
    }

    fn eval_inner(
        &mut self,
        pkg: usize,
        file: usize,
        e: ExprId,
        hint: Option<ValTy>,
    ) -> Result<ConstValue, Fail> {
        let (kind, span) = {
            let ex = self.pkgs[pkg].expr(e);
            (&ex.kind, ex.span)
        };
        match kind {
            ExprKind::Int {
                value, negative, ..
            } => {
                let ty = match hint {
                    Some(ValTy::Int(i)) => i,
                    Some(ValTy::Bool) => return self.mismatch("an integer where bool is expected"),
                    None => IntTy::I64,
                };
                let v = i128::try_from(*value).map_err(|_| Fail::OutOfRange(span))?;
                let v = if *negative { -v } else { v };
                if !ty.contains(v) {
                    return Err(Fail::OutOfRange(span));
                }
                Ok(ConstValue::Int { ty, value: v })
            }
            ExprKind::Bool(b) => Ok(ConstValue::Bool(*b)),
            ExprKind::Paren(inner) => self.eval_inner(pkg, file, *inner, hint),
            ExprKind::Path(path) => match path.res {
                Res::Poison => Err(Fail::Reported),
                Res::Def(d) if self.sema.def(d).kind == DefKind::Const => self.const_value(d),
                _ => {
                    self.path_value_type(pkg, e)?;
                    self.not_in_subset(pkg, e)
                }
            },
            ExprKind::Cast { expr, .. } => {
                let target = match self.const_type(pkg, e)? {
                    Some(ValTy::Int(i)) => i,
                    _ => return self.not_in_subset(pkg, e),
                };
                let inner_ty = self
                    .const_type(pkg, *expr)?
                    .unwrap_or(ValTy::Int(IntTy::I64));
                let v = self.eval_expr(pkg, file, *expr, Some(inner_ty))?;
                match v {
                    ConstValue::Bool(b) => Ok(ConstValue::Int {
                        ty: target,
                        value: i128::from(b),
                    }),
                    ConstValue::Int { ty, value } => {
                        if !lossless(ty, target) {
                            return Err(Fail::NotImplemented(format!(
                                "type checking of a constant expression (`as {}` from {} loses values, E0402)",
                                target.name(),
                                ty.name()
                            )));
                        }
                        Ok(ConstValue::Int { ty: target, value })
                    }
                }
            }
            ExprKind::Builtin {
                name,
                type_arg,
                args,
            } if (name == "sizeof" || name == "alignof") && args.is_empty() => {
                let Some(t) = type_arg else {
                    return self.not_in_subset(pkg, e);
                };
                let ty = self.lower_ty_in(pkg, t);
                let layout = self.layout_for_const(ty)?;
                let value = if name == "sizeof" { layout.0 } else { layout.1 };
                Ok(ConstValue::Int {
                    ty: IntTy::I64,
                    value: i128::from(value),
                })
            }
            ExprKind::Builtin {
                name,
                type_arg,
                args,
            } if name == "lanes" && args.is_empty() => {
                let Some(t) = type_arg else {
                    return self.not_in_subset(pkg, e);
                };
                let ty = self.lower_ty_in(pkg, t);
                if self.d.types.has_params(ty) {
                    return Err(Fail::Generic);
                }
                match self.d.types.kind(ty) {
                    TyKind::Vector(_, n) | TyKind::Mask(n) => Ok(ConstValue::Int {
                        ty: IntTy::I64,
                        value: i128::from(*n),
                    }),
                    TyKind::Poison => Err(Fail::Reported),
                    _ => {
                        self.error(
                            "E0401",
                            "`@lanes` needs a vector or mask type",
                            file,
                            t.span,
                        );
                        Err(Fail::Reported)
                    }
                }
            }
            ExprKind::Field { base, name, .. } if self.is_target(pkg, *base) => {
                let t = self.target;
                match name.as_str() {
                    "simd_bits" => Ok(ConstValue::Int {
                        ty: IntTy::I64,
                        value: t.simd_bits,
                    }),
                    "cache_line" => Ok(ConstValue::Int {
                        ty: IntTy::I64,
                        value: t.cache_line,
                    }),
                    "page_size" => Ok(ConstValue::Int {
                        ty: IntTy::I64,
                        value: t.page_size,
                    }),
                    "hybrid_cores" => Ok(ConstValue::Bool(t.hybrid_cores)),
                    "smt" => Ok(ConstValue::Bool(t.smt)),
                    "gpu_unified" => Ok(ConstValue::Bool(t.gpu_unified)),
                    // An unknown field is E1606, reported when the path was resolved.
                    n if !crate::target::TARGET_FIELDS.contains(&n) => Err(Fail::Reported),
                    _ => self.not_in_subset(pkg, e),
                }
            }
            ExprKind::Call { callee, args, .. } if self.is_target_has(pkg, *callee) => {
                if let [arg] = args.as_slice()
                    && let Some(name) = self.shorthand_variant(pkg, *arg)
                {
                    return match self.target.has(name) {
                        Some(b) => Ok(ConstValue::Bool(b)),
                        // E1606 was reported when the path was resolved.
                        None => Err(Fail::Reported),
                    };
                }
                self.not_in_subset(pkg, e)
            }
            ExprKind::Unary { op, expr, op_span } => match op {
                UnOp::Not => match self.eval_expr(pkg, file, *expr, Some(ValTy::Bool))? {
                    ConstValue::Bool(b) => Ok(ConstValue::Bool(!b)),
                    _ => self.mismatch("`!` applied to an integer"),
                },
                UnOp::Neg | UnOp::NegWrap | UnOp::BitNot => {
                    let t = match self.const_type(pkg, *expr)? {
                        Some(ValTy::Int(i)) => i,
                        Some(ValTy::Bool) => return self.mismatch("an integer operator on bool"),
                        None => match hint {
                            Some(ValTy::Int(i)) => i,
                            Some(ValTy::Bool) => {
                                return self.mismatch("an integer where bool is expected");
                            }
                            None => IntTy::I64,
                        },
                    };
                    if *op == UnOp::Neg && !t.signed() {
                        return self.mismatch("unary `-` on an unsigned integer, E0406");
                    }
                    let ConstValue::Int { value, .. } =
                        self.eval_expr(pkg, file, *expr, Some(ValTy::Int(t)))?
                    else {
                        return self.mismatch("an integer operator on bool");
                    };
                    match int_unary(*op, t, value) {
                        Ok(v) => Ok(ConstValue::Int { ty: t, value: v }),
                        Err(trap) => self.trapped(file, *op_span, &trap),
                    }
                }
                _ => self.not_in_subset(pkg, e),
            },
            ExprKind::Binary {
                op,
                lhs,
                rhs,
                op_span,
            } => self.eval_binary(pkg, file, *op, *lhs, *rhs, *op_span, hint),
            _ => self.not_in_subset(pkg, e),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn eval_binary(
        &mut self,
        pkg: usize,
        file: usize,
        op: BinOp,
        lhs: ExprId,
        rhs: ExprId,
        op_span: Span,
        hint: Option<ValTy>,
    ) -> Result<ConstValue, Fail> {
        // `@target.os == .linux` and the like.
        if matches!(op, BinOp::Eq | BinOp::Ne) {
            let pair = match (
                self.target_enum_field(pkg, lhs),
                self.shorthand_variant(pkg, rhs),
                self.target_enum_field(pkg, rhs),
                self.shorthand_variant(pkg, lhs),
            ) {
                (Some(f), Some(v), _, _) | (_, _, Some(f), Some(v)) => Some((f, v.to_string())),
                _ => None,
            };
            if let Some(((field, actual), variant)) = pair {
                let known: &[&str] = match field {
                    "os" => &crate::target::OS_VARIANTS,
                    "arch" => &crate::target::ARCH_VARIANTS,
                    _ => &crate::target::CPU_VARIANTS,
                };
                if !known.contains(&variant.as_str()) {
                    return self.mismatch(&format!(
                        "`.{variant}` is not a value of `@target.{field}`, E0314"
                    ));
                }
                let equal = actual == variant;
                return Ok(ConstValue::Bool(if op == BinOp::Eq {
                    equal
                } else {
                    !equal
                }));
            }
        }
        if matches!(op, BinOp::And | BinOp::Or) {
            let ConstValue::Bool(l) = self.eval_expr(pkg, file, lhs, Some(ValTy::Bool))? else {
                return self.mismatch("`&&` or `||` on an integer, E0403");
            };
            // Short-circuit: the right operand is evaluated only when it matters.
            if (op == BinOp::And && !l) || (op == BinOp::Or && l) {
                return Ok(ConstValue::Bool(l));
            }
            let ConstValue::Bool(r) = self.eval_expr(pkg, file, rhs, Some(ValTy::Bool))? else {
                return self.mismatch("`&&` or `||` on an integer, E0403");
            };
            return Ok(ConstValue::Bool(r));
        }
        if op.is_comparison() {
            let operand = match self.const_type(pkg, lhs)? {
                Some(t) => t,
                None => self.const_type(pkg, rhs)?.unwrap_or(ValTy::Int(IntTy::I64)),
            };
            let l = self.eval_expr(pkg, file, lhs, Some(operand))?;
            let r = self.eval_expr(pkg, file, rhs, Some(operand))?;
            return match (l, r) {
                (ConstValue::Int { value: a, .. }, ConstValue::Int { value: b, .. }) => {
                    Ok(ConstValue::Bool(match op {
                        BinOp::Eq => a == b,
                        BinOp::Ne => a != b,
                        BinOp::Lt => a < b,
                        BinOp::Le => a <= b,
                        BinOp::Gt => a > b,
                        _ => a >= b,
                    }))
                }
                (ConstValue::Bool(a), ConstValue::Bool(b))
                    if matches!(op, BinOp::Eq | BinOp::Ne) =>
                {
                    Ok(ConstValue::Bool(if op == BinOp::Eq {
                        a == b
                    } else {
                        a != b
                    }))
                }
                _ => self.mismatch("an operand of a comparison that cannot be compared, E0406"),
            };
        }
        // Arithmetic, bitwise and shifts on integers.
        let operand = match self.const_type(pkg, lhs)? {
            Some(ValTy::Int(i)) => i,
            Some(ValTy::Bool) => return self.mismatch("an integer operator on bool, E0406"),
            None => {
                let from_rhs = if op.is_shift() {
                    None
                } else {
                    self.const_type(pkg, rhs)?
                };
                match from_rhs.or(hint) {
                    Some(ValTy::Int(i)) => i,
                    Some(ValTy::Bool) => return self.mismatch("an integer where bool is expected"),
                    None => IntTy::I64,
                }
            }
        };
        let ConstValue::Int { value: a, .. } =
            self.eval_expr(pkg, file, lhs, Some(ValTy::Int(operand)))?
        else {
            return self.mismatch("an integer operator on bool, E0406");
        };
        let rhs_hint = if op.is_shift() {
            // The count may have any integer type (section 5.2).
            match self.const_type(pkg, rhs)? {
                Some(t) => t,
                None => ValTy::Int(IntTy::I64),
            }
        } else {
            ValTy::Int(operand)
        };
        let ConstValue::Int { value: b, .. } = self.eval_expr(pkg, file, rhs, Some(rhs_hint))?
        else {
            return self.mismatch("an integer operator on bool, E0406");
        };
        match int_binary(op, operand, a, b) {
            Ok(v) => Ok(ConstValue::Int {
                ty: operand,
                value: v,
            }),
            Err(trap) => self.trapped(file, op_span, &trap),
        }
    }

    /// Size and alignment of a type for `@sizeof` and `@alignof`.
    fn layout_for_const(&mut self, ty: crate::ty::TypeId) -> Result<(u64, u64), Fail> {
        if self.d.types.has_poison(ty) {
            return Err(Fail::Reported);
        }
        if self.d.types.has_params(ty) {
            return Err(Fail::Generic);
        }
        match self.layout_of_checked(ty) {
            Some(l) => Ok((l.size, l.align)),
            None => Err(Fail::Reported),
        }
    }

    /// The value of the `const` item `d`, evaluated once.
    pub(crate) fn const_value(&mut self, d: DefId) -> Result<ConstValue, Fail> {
        match self.d.consts.get(&d) {
            Some(ConstState::Done(r)) => return r.clone(),
            Some(ConstState::InProgress) => {
                // E0812, at the constant the evaluation came back to.
                let name = self.sema.def(d).name.clone();
                let (file, span) = (self.file_of(d), self.sema.def(d).span);
                self.error(
                    "E0812",
                    format!("the value of the constant `{name}` depends on itself"),
                    file,
                    span,
                );
                // Everything on the cycle is poisoned.
                self.d
                    .consts
                    .insert(d, ConstState::Done(Err(Fail::Reported)));
                return Err(Fail::Reported);
            }
            None => {}
        }
        let (pkg, file) = (self.pkg_of(d), self.sema.def(d).file.unwrap_or(0));
        let Some(hir::Item::Const(item)) = self.hir_item(d) else {
            return Err(Fail::Reported);
        };
        let value_expr = item.value;
        self.d.consts.insert(d, ConstState::InProgress);
        let saved = self.cur;
        self.cur = Cur {
            pkg,
            file,
            impl_def: self.sema.def(d).parent,
        };
        let result = match self.const_decl_type(d) {
            Err(f) => Err(f),
            Ok(t) => self.eval_expr(pkg, file, value_expr, Some(t)),
        };
        self.cur = saved;
        // A cycle already stored its verdict.
        if !matches!(self.d.consts.get(&d), Some(ConstState::Done(_))) {
            self.d.consts.insert(d, ConstState::Done(result.clone()));
        }
        result
    }

    /// An integer constant expression of a signature (array length, tag value,
    /// alignment), reporting what cannot be evaluated. `Err` means no value; the
    /// reason has been recorded unless it is `OutOfRange`, which the caller reports.
    pub(crate) fn eval_signature_const(
        &mut self,
        e: ExprId,
        hint: ValTy,
    ) -> Result<ConstValue, Fail> {
        let (pkg, file) = (self.cur.pkg, self.cur.file);
        self.eval_expr(pkg, file, e, Some(hint))
    }

    /// Record a failure that is not for the caller to turn into a diagnostic.
    pub(crate) fn note_failure(&mut self, f: &Fail) {
        match f {
            Fail::Reported => {}
            Fail::NotImplemented(m) => self.ni(m.clone()),
            Fail::Generic => self.ni(
                "constant evaluation of an expression over a generic parameter (computed per \
                 instance)",
            ),
            Fail::OutOfRange(_) => self.ni(
                "type checking of a constant expression (an integer literal that does not fit \
                 its type, E0405)",
            ),
        }
    }

    /// The constant argument or array length written as `e`: a constant generic
    /// parameter by itself, or a value of the evaluated subset.
    pub(crate) fn const_arg(&mut self, e: ExprId, hint: ValTy) -> ConstArg {
        let pkg = self.cur.pkg;
        let p = &self.pkgs[pkg];
        let mut inner = e;
        while let ExprKind::Paren(i) = &p.expr(inner).kind {
            inner = *i;
        }
        if let ExprKind::Path(path) = &p.expr(inner).kind
            && let Res::Def(d) = path.res
            && self.sema.def(d).kind == (DefKind::GenericParam { is_const: true })
        {
            return ConstArg::Param(d);
        }
        match self.eval_signature_const(e, hint) {
            Ok(v) => ConstArg::Value(v),
            Err(f) => {
                self.note_failure(&f);
                ConstArg::Poison
            }
        }
    }
}

// ------------------------------------------------------- runtime reads

/// The spans of the locals, parameters and statics read inside `e`: values that make it
/// not a constant expression (E0807).
pub(crate) fn runtime_reads(pkg: &hir::Package, sema: &Sema, e: ExprId) -> Vec<Span> {
    let mut out = Vec::new();
    collect_runtime_reads(pkg, sema, e, &mut out);
    out
}

fn collect_runtime_reads(pkg: &hir::Package, sema: &Sema, e: ExprId, out: &mut Vec<Span>) {
    let expr = pkg.expr(e);
    match &expr.kind {
        ExprKind::Path(p) => {
            let runtime = matches!(p.res, Res::Local(_))
                || matches!(p.res, Res::Def(d) if sema.def(d).kind == DefKind::Static);
            if runtime {
                out.push(expr.span);
            }
        }
        ExprKind::Builtin { args, .. } => {
            for a in args {
                collect_runtime_reads(pkg, sema, *a, out);
            }
        }
        ExprKind::Unary { expr, .. }
        | ExprKind::TypeApply { base: expr, .. }
        | ExprKind::Field { base: expr, .. }
        | ExprKind::TupleField { base: expr, .. }
        | ExprKind::Deref { base: expr, .. }
        | ExprKind::Cast { expr, .. } => collect_runtime_reads(pkg, sema, *expr, out),
        ExprKind::Binary { lhs, rhs, .. }
        | ExprKind::Range {
            lo: lhs, hi: rhs, ..
        } => {
            collect_runtime_reads(pkg, sema, *lhs, out);
            collect_runtime_reads(pkg, sema, *rhs, out);
        }
        ExprKind::Call { callee, args, .. } => {
            collect_runtime_reads(pkg, sema, *callee, out);
            for a in args {
                collect_runtime_reads(pkg, sema, *a, out);
            }
        }
        ExprKind::Index { base, index, .. } => {
            collect_runtime_reads(pkg, sema, *base, out);
            collect_runtime_reads(pkg, sema, *index, out);
        }
        ExprKind::Slice { base, lo, hi, .. } => {
            collect_runtime_reads(pkg, sema, *base, out);
            collect_runtime_reads(pkg, sema, *lo, out);
            collect_runtime_reads(pkg, sema, *hi, out);
        }
        ExprKind::StructLit { fields, .. } | ExprKind::StructShorthand { fields, .. } => {
            for f in fields {
                collect_runtime_reads(pkg, sema, f.value, out);
            }
        }
        ExprKind::EnumLit { payload, .. } => {
            if let Some(p) = payload {
                collect_runtime_reads(pkg, sema, *p, out);
            }
        }
        ExprKind::ArrayLit { len, elems, .. } => {
            collect_runtime_reads(pkg, sema, *len, out);
            for x in elems {
                collect_runtime_reads(pkg, sema, *x, out);
            }
        }
        ExprKind::Tuple(elems) => {
            for x in elems {
                collect_runtime_reads(pkg, sema, *x, out);
            }
        }
        ExprKind::Paren(inner) => collect_runtime_reads(pkg, sema, *inner, out),
        ExprKind::If {
            cond,
            then,
            otherwise,
        } => {
            collect_runtime_reads(pkg, sema, cond.expr, out);
            collect_runtime_reads(pkg, sema, *then, out);
            collect_runtime_reads(pkg, sema, *otherwise, out);
        }
        ExprKind::Match { scrutinee, arms } => {
            collect_runtime_reads(pkg, sema, *scrutinee, out);
            for arm in arms {
                if let Some(g) = arm.guard {
                    collect_runtime_reads(pkg, sema, g, out);
                }
                match &arm.body {
                    hir::ArmBody::Expr(x) => collect_runtime_reads(pkg, sema, *x, out),
                    hir::ArmBody::Block(b) => collect_runtime_reads_block(pkg, sema, *b, out),
                }
            }
        }
        ExprKind::Closure(c) => collect_runtime_reads_block(pkg, sema, c.body, out),
        ExprKind::Catch { value, handler, .. } | ExprKind::Orelse { value, handler, .. } => {
            collect_runtime_reads(pkg, sema, *value, out);
            match handler {
                hir::Handler::Expr(x) => collect_runtime_reads(pkg, sema, *x, out),
                hir::Handler::Block(b) => collect_runtime_reads_block(pkg, sema, *b, out),
            }
        }
        ExprKind::Spawn { call, scope } => {
            collect_runtime_reads(pkg, sema, *call, out);
            collect_runtime_reads(pkg, sema, *scope, out);
        }
        ExprKind::Dispatch {
            call,
            domain,
            executor,
        } => {
            collect_runtime_reads(pkg, sema, *call, out);
            collect_runtime_reads(pkg, sema, *domain, out);
            collect_runtime_reads(pkg, sema, *executor, out);
        }
        ExprKind::Unsafe { body, .. } => collect_runtime_reads(pkg, sema, *body, out),
        ExprKind::Int { .. }
        | ExprKind::Float(_)
        | ExprKind::Str(_)
        | ExprKind::CStr(_)
        | ExprKind::Bool(_)
        | ExprKind::Poison => {}
    }
}

fn collect_runtime_reads_block(
    pkg: &hir::Package,
    sema: &Sema,
    b: hir::BlockId,
    out: &mut Vec<Span>,
) {
    for s in &pkg.block(b).stmts {
        collect_runtime_reads_stmt(pkg, sema, *s, out);
    }
}

fn collect_runtime_reads_stmt(
    pkg: &hir::Package,
    sema: &Sema,
    s: hir::StmtId,
    out: &mut Vec<Span>,
) {
    match &pkg.stmt(s).kind {
        hir::StmtKind::Let { init, .. } => collect_runtime_reads(pkg, sema, *init, out),
        hir::StmtKind::Assign { place, value, .. } => {
            collect_runtime_reads(pkg, sema, *place, out);
            collect_runtime_reads(pkg, sema, *value, out);
        }
        hir::StmtKind::Discard(e) | hir::StmtKind::Expr(e) => {
            collect_runtime_reads(pkg, sema, *e, out)
        }
        hir::StmtKind::Return(Some(e)) | hir::StmtKind::Fail(e) => {
            collect_runtime_reads(pkg, sema, *e, out)
        }
        hir::StmtKind::Return(None)
        | hir::StmtKind::Break { .. }
        | hir::StmtKind::Continue { .. } => {}
        hir::StmtKind::Defer { body, .. } | hir::StmtKind::Block(body) => {
            collect_runtime_reads_block(pkg, sema, *body, out)
        }
        hir::StmtKind::If {
            cond,
            then,
            otherwise,
        } => {
            collect_runtime_reads(pkg, sema, cond.expr, out);
            collect_runtime_reads_block(pkg, sema, *then, out);
            if let Some(o) = otherwise {
                collect_runtime_reads_stmt(pkg, sema, *o, out);
            }
        }
        hir::StmtKind::While { cond, body, .. } => {
            collect_runtime_reads(pkg, sema, cond.expr, out);
            collect_runtime_reads_block(pkg, sema, *body, out);
        }
        hir::StmtKind::Loop { body, .. } => collect_runtime_reads_block(pkg, sema, *body, out),
        hir::StmtKind::For {
            iter,
            range_end,
            step,
            body,
            ..
        } => {
            collect_runtime_reads(pkg, sema, *iter, out);
            if let Some((_, hi)) = range_end {
                collect_runtime_reads(pkg, sema, *hi, out);
            }
            if let Some(st) = step {
                collect_runtime_reads(pkg, sema, *st, out);
            }
            collect_runtime_reads_block(pkg, sema, *body, out);
        }
        hir::StmtKind::Match { scrutinee, arms } => {
            collect_runtime_reads(pkg, sema, *scrutinee, out);
            for arm in arms {
                if let Some(g) = arm.guard {
                    collect_runtime_reads(pkg, sema, g, out);
                }
                match &arm.body {
                    hir::ArmBody::Expr(x) => collect_runtime_reads(pkg, sema, *x, out),
                    hir::ArmBody::Block(b) => collect_runtime_reads_block(pkg, sema, *b, out),
                }
            }
        }
        hir::StmtKind::Scope { body, .. } => collect_runtime_reads_block(pkg, sema, *body, out),
        hir::StmtKind::Spawn { call, scope } => {
            collect_runtime_reads(pkg, sema, *call, out);
            collect_runtime_reads(pkg, sema, *scope, out);
        }
        hir::StmtKind::Dispatch {
            call,
            domain,
            executor,
        } => {
            collect_runtime_reads(pkg, sema, *call, out);
            collect_runtime_reads(pkg, sema, *domain, out);
            collect_runtime_reads(pkg, sema, *executor, out);
        }
        hir::StmtKind::Select { arms } => {
            for arm in arms {
                if let Some(call) = arm.call {
                    collect_runtime_reads(pkg, sema, call, out);
                }
                collect_runtime_reads_block(pkg, sema, arm.body, out);
            }
        }
        hir::StmtKind::Unsafe { body, .. } => collect_runtime_reads_block(pkg, sema, *body, out),
        hir::StmtKind::Poison => {}
    }
}
