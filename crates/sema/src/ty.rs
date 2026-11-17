//! The type table: interned types (pipeline section 4.4).
//!
//! A type is a `TypeId`, an index into a `TypeTable` that holds each distinct `TyKind`
//! once. Type equality is therefore `TypeId` equality: nominal for structs, enums and
//! interfaces (their `DefId`), structural for everything else, and two instances of a
//! generic type are equal when their arguments are.

use crate::defs::{DefId, Sema};
use ingot_syntax::ast::BinOp;
use ingot_syntax::token::ScalarTy;
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TypeId(pub u32);

impl TypeId {
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum IntTy {
    I8,
    I16,
    I32,
    I64,
    U8,
    U16,
    U32,
    U64,
}

impl IntTy {
    pub const ALL: [IntTy; 8] = [
        IntTy::I8,
        IntTy::I16,
        IntTy::I32,
        IntTy::I64,
        IntTy::U8,
        IntTy::U16,
        IntTy::U32,
        IntTy::U64,
    ];

    pub fn bits(self) -> u32 {
        match self {
            IntTy::I8 | IntTy::U8 => 8,
            IntTy::I16 | IntTy::U16 => 16,
            IntTy::I32 | IntTy::U32 => 32,
            IntTy::I64 | IntTy::U64 => 64,
        }
    }

    pub fn signed(self) -> bool {
        matches!(self, IntTy::I8 | IntTy::I16 | IntTy::I32 | IntTy::I64)
    }

    pub fn min(self) -> i128 {
        if self.signed() {
            -(1i128 << (self.bits() - 1))
        } else {
            0
        }
    }

    pub fn max(self) -> i128 {
        if self.signed() {
            (1i128 << (self.bits() - 1)) - 1
        } else {
            (1i128 << self.bits()) - 1
        }
    }

    pub fn contains(self, value: i128) -> bool {
        self.min() <= value && value <= self.max()
    }

    pub fn name(self) -> &'static str {
        match self {
            IntTy::I8 => "i8",
            IntTy::I16 => "i16",
            IntTy::I32 => "i32",
            IntTy::I64 => "i64",
            IntTy::U8 => "u8",
            IntTy::U16 => "u16",
            IntTy::U32 => "u32",
            IntTy::U64 => "u64",
        }
    }

    pub fn from_scalar(s: ScalarTy) -> Option<IntTy> {
        Some(match s {
            ScalarTy::I8 => IntTy::I8,
            ScalarTy::I16 => IntTy::I16,
            ScalarTy::I32 => IntTy::I32,
            ScalarTy::I64 => IntTy::I64,
            ScalarTy::U8 => IntTy::U8,
            ScalarTy::U16 => IntTy::U16,
            ScalarTy::U32 => IntTy::U32,
            ScalarTy::U64 => IntTy::U64,
            _ => return None,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FloatTy {
    F32,
    F64,
}

impl FloatTy {
    pub fn bits(self) -> u32 {
        match self {
            FloatTy::F32 => 32,
            FloatTy::F64 => 64,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            FloatTy::F32 => "f32",
            FloatTy::F64 => "f64",
        }
    }
}

/// The element type of a vector (`f32x4`, `u8x16`). A mask is its own kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Lane {
    Int(IntTy),
    Float(FloatTy),
}

impl Lane {
    pub fn bits(self) -> u32 {
        match self {
            Lane::Int(i) => i.bits(),
            Lane::Float(f) => f.bits(),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Lane::Int(i) => i.name(),
            Lane::Float(f) => f.name(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Mutability {
    Shared,
    Mut,
}

/// A value of a constant expression. Constant evaluation (`consteval`) produces these;
/// a later step extends the set (floats, structs, enum values).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ConstValue {
    Int { ty: IntTy, value: i128 },
    Bool(bool),
}

/// The identity of an interned constant-expression tree.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ConstExprId(pub u32);

/// A length or constant argument written as an expression over constant parameters.
/// Interning makes two types equal exactly when their trees are equal.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ConstExpr {
    Int {
        ty: IntTy,
        value: i128,
    },
    Bool(bool),
    /// A constant generic parameter, known per instance.
    Param(DefId),
    Unary {
        op: ingot_syntax::ast::UnOp,
        expr: ConstExprId,
    },
    Binary {
        op: ingot_syntax::ast::BinOp,
        lhs: ConstExprId,
        rhs: ConstExprId,
    },
    /// `e as ty` between integer types.
    Cast {
        to: IntTy,
        expr: ConstExprId,
    },
    /// `@sizeof(T)` (`size`) or `@alignof(T)`; folded once `ty` is concrete.
    Layout {
        size: bool,
        ty: TypeId,
    },
}

/// The length of an array or the value of a constant generic argument.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ConstArg {
    Value(ConstValue),
    /// A constant generic parameter (`N` in `[N]T`), known per instance.
    Param(DefId),
    /// An expression over constant generic parameters, as an interned tree.
    Expr(ConstExprId),
    Poison,
}

impl ConstArg {
    /// The value as a length: an integer, whatever its type.
    pub fn as_len(self) -> Option<i128> {
        match self {
            ConstArg::Value(ConstValue::Int { value, .. }) => Some(value),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum GenericArg {
    Ty(TypeId),
    Const(ConstArg),
}

/// The signature of a function pointer or of an `Fn`-family bound.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FnPtrSig {
    /// `extern fn(...)`, the C-ABI pointer.
    pub ext: bool,
    pub params: Vec<TypeId>,
    pub ret: TypeId,
    pub error: Option<TypeId>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FnKind {
    Fn,
    FnMut,
    FnOnce,
}

impl FnKind {
    pub fn name(self) -> &'static str {
        match self {
            FnKind::Fn => "Fn",
            FnKind::FnMut => "FnMut",
            FnKind::FnOnce => "FnOnce",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum TyKind {
    Int(IntTy),
    Float(FloatTy),
    Bool,
    Void,
    Never,
    Array(TypeId, ConstArg),
    Tuple(Vec<TypeId>),
    /// A struct or enum, or a built-in generic type without a declaration in source
    /// (`Idx`, `Grid[T]`). `Option` and `Result` are `Optional` and `Result`.
    Adt(DefId, Vec<GenericArg>),
    Optional(TypeId),
    Result(TypeId, TypeId),
    Ref(Mutability, TypeId),
    Slice(Mutability, TypeId),
    RawPtr(Mutability, TypeId),
    FnPtr(FnPtrSig),
    /// `&dyn I` and `&mut dyn I`: the borrow itself (16 bytes), for an interface `I`.
    Dyn(Mutability, DefId, Vec<GenericArg>),
    /// `&dyn Fn(A) -> R` and its relatives (section 5.6).
    DynFn(Mutability, FnKind, FnPtrSig),
    Vector(Lane, u32),
    Mask(u32),
    /// A generic parameter, or the `Self` of an interface.
    Param(DefId),
    /// `T.Item`: the associated type `Item` (a definition) of a type.
    Projection(TypeId, DefId),
    /// The anonymous type of a closure expression (section 5.6): the function (or
    /// constant, or test) whose body contains it, and the index of the closure's
    /// expression in that package's arena. Its signature and captures are in the typed
    /// body (`typed::ClosureInfo`).
    Closure(DefId, u32, Vec<GenericArg>),
    /// The type of something that failed to type-check. Reports nothing downstream.
    Poison,
}

/// A mapping from generic parameters to arguments.
#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Subst {
    pub map: BTreeMap<DefId, GenericArg>,
}

impl Subst {
    pub fn new() -> Subst {
        Subst::default()
    }

    pub fn insert(&mut self, param: DefId, arg: GenericArg) {
        self.map.insert(param, arg);
    }

    pub fn get(&self, param: DefId) -> Option<&GenericArg> {
        self.map.get(&param)
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

pub struct TypeTable {
    kinds: Vec<TyKind>,
    map: BTreeMap<TyKind, TypeId>,
    pub poison: TypeId,
    pub bool_: TypeId,
    pub void: TypeId,
    pub never: TypeId,
    pub i64_: TypeId,
    const_exprs: Vec<ConstExpr>,
    const_expr_ids: BTreeMap<ConstExpr, ConstExprId>,
}

impl Default for TypeTable {
    fn default() -> Self {
        TypeTable::new()
    }
}

impl TypeTable {
    pub fn new() -> TypeTable {
        let mut t = TypeTable {
            kinds: Vec::new(),
            map: BTreeMap::new(),
            poison: TypeId(0),
            bool_: TypeId(0),
            void: TypeId(0),
            never: TypeId(0),
            i64_: TypeId(0),
            const_exprs: Vec::new(),
            const_expr_ids: BTreeMap::new(),
        };
        t.poison = t.intern(TyKind::Poison);
        t.bool_ = t.intern(TyKind::Bool);
        t.void = t.intern(TyKind::Void);
        t.never = t.intern(TyKind::Never);
        t.i64_ = t.intern(TyKind::Int(IntTy::I64));
        t
    }

    /// Intern a constant-expression tree; equal trees have one id.
    pub fn intern_expr(&mut self, expr: ConstExpr) -> ConstExprId {
        if let Some(&id) = self.const_expr_ids.get(&expr) {
            return id;
        }
        let id = ConstExprId(self.const_exprs.len() as u32);
        self.const_exprs.push(expr.clone());
        self.const_expr_ids.insert(expr, id);
        id
    }

    pub fn const_expr(&self, id: ConstExprId) -> &ConstExpr {
        &self.const_exprs[id.0 as usize]
    }

    /// The value of a constant argument, when it has one (an expression tree with a
    /// folded value).
    pub fn const_arg_value(&self, c: ConstArg) -> Option<ConstValue> {
        match c {
            ConstArg::Value(v) => Some(v),
            ConstArg::Expr(id) if !self.expr_has_params(id) => self.fold_expr(id),
            _ => None,
        }
    }

    /// Whether a constant-expression tree still mentions a parameter.
    fn expr_has_params(&self, id: ConstExprId) -> bool {
        match self.const_expr(id) {
            ConstExpr::Param(_) => true,
            ConstExpr::Unary { expr, .. } => self.expr_has_params(*expr),
            ConstExpr::Binary { lhs, rhs, .. } => {
                self.expr_has_params(*lhs) || self.expr_has_params(*rhs)
            }
            ConstExpr::Cast { expr, .. } => self.expr_has_params(*expr),
            ConstExpr::Layout { ty, .. } => self.has_params(*ty),
            ConstExpr::Int { .. } | ConstExpr::Bool(_) => false,
        }
    }

    fn expr_of_value(&mut self, value: ConstValue) -> ConstExprId {
        self.intern_expr(match value {
            ConstValue::Int { ty, value } => ConstExpr::Int { ty, value },
            ConstValue::Bool(b) => ConstExpr::Bool(b),
        })
    }

    fn finish_expr(&mut self, id: ConstExprId) -> ConstArg {
        if !self.expr_has_params(id)
            && let Some(value) = self.fold_expr(id)
        {
            return ConstArg::Value(value);
        }
        ConstArg::Expr(id)
    }

    /// Substitute constant parameters in a tree and fold it when no parameter is left.
    fn subst_const(&mut self, c: ConstArg, s: &Subst) -> ConstArg {
        match c {
            ConstArg::Param(p) => match s.get(p) {
                Some(GenericArg::Const(ConstArg::Value(v))) => ConstArg::Value(*v),
                Some(GenericArg::Const(ConstArg::Param(q))) => ConstArg::Param(*q),
                Some(GenericArg::Const(ConstArg::Expr(id))) => self.subst_expr(*id, s),
                Some(GenericArg::Const(ConstArg::Poison)) => ConstArg::Poison,
                _ => ConstArg::Param(p),
            },
            ConstArg::Expr(id) => self.subst_expr(id, s),
            other => other,
        }
    }

    fn subst_expr(&mut self, id: ConstExprId, s: &Subst) -> ConstArg {
        let node = self.const_expr(id).clone();
        let rebuilt = match node {
            ConstExpr::Int { .. } | ConstExpr::Bool(_) => return ConstArg::Expr(id),
            ConstExpr::Param(p) => {
                return self.subst_const(ConstArg::Param(p), s);
            }
            ConstExpr::Unary { op, expr } => {
                let expr = self.subst_expr_id(expr, s);
                ConstExpr::Unary { op, expr }
            }
            ConstExpr::Binary { op, lhs, rhs } => ConstExpr::Binary {
                op,
                lhs: self.subst_expr_id(lhs, s),
                rhs: self.subst_expr_id(rhs, s),
            },
            ConstExpr::Cast { to, expr } => ConstExpr::Cast {
                to,
                expr: self.subst_expr_id(expr, s),
            },
            ConstExpr::Layout { size, ty } => ConstExpr::Layout {
                size,
                ty: self.subst(ty, s),
            },
        };
        let id = self.intern_expr(rebuilt);
        self.finish_expr(id)
    }

    fn subst_expr_id(&mut self, id: ConstExprId, s: &Subst) -> ConstExprId {
        match self.subst_expr(id, s) {
            ConstArg::Expr(id) => id,
            ConstArg::Value(v) => self.expr_of_value(v),
            ConstArg::Param(p) => self.intern_expr(ConstExpr::Param(p)),
            ConstArg::Poison => id,
        }
    }

    /// The value of a tree with no parameters, when its operations do not trap.
    fn fold_expr(&self, id: ConstExprId) -> Option<ConstValue> {
        self.fold_expr_result(id)?.ok()
    }

    /// A trap in a tree with no parameters, for E0806 at a concrete type.
    pub fn expr_trap(&self, id: ConstExprId) -> Option<crate::consteval::Trap> {
        match self.fold_expr_result(id) {
            Some(Err(trap)) => Some(trap),
            _ => None,
        }
    }

    fn fold_expr_result(
        &self,
        id: ConstExprId,
    ) -> Option<Result<ConstValue, crate::consteval::Trap>> {
        match self.const_expr(id) {
            ConstExpr::Int { ty, value } => Some(Ok(ConstValue::Int {
                ty: *ty,
                value: *value,
            })),
            ConstExpr::Bool(b) => Some(Ok(ConstValue::Bool(*b))),
            ConstExpr::Param(_) => None,
            ConstExpr::Unary { op, expr } => {
                let v = self.fold_expr_result(*expr)?.ok()?;
                match v {
                    ConstValue::Int { ty, value } => {
                        match crate::consteval::int_unary(*op, ty, value) {
                            Ok(value) => Some(Ok(ConstValue::Int { ty, value })),
                            Err(trap) => Some(Err(trap)),
                        }
                    }
                    ConstValue::Bool(b) if *op == ingot_syntax::ast::UnOp::Not => {
                        Some(Ok(ConstValue::Bool(!b)))
                    }
                    ConstValue::Bool(_) => None,
                }
            }
            ConstExpr::Binary { op, lhs, rhs } => {
                let a = self.fold_expr_result(*lhs)?.ok()?;
                let b = self.fold_expr_result(*rhs)?.ok()?;
                match (a, b) {
                    (ConstValue::Int { ty, value: x, .. }, ConstValue::Int { value: y, .. }) => {
                        match op {
                            BinOp::Eq => Some(Ok(ConstValue::Bool(x == y))),
                            BinOp::Ne => Some(Ok(ConstValue::Bool(x != y))),
                            BinOp::Lt => Some(Ok(ConstValue::Bool(x < y))),
                            BinOp::Le => Some(Ok(ConstValue::Bool(x <= y))),
                            BinOp::Gt => Some(Ok(ConstValue::Bool(x > y))),
                            BinOp::Ge => Some(Ok(ConstValue::Bool(x >= y))),
                            _ => match crate::consteval::int_binary(*op, ty, x, y) {
                                Ok(value) => Some(Ok(ConstValue::Int { ty, value })),
                                Err(trap) => Some(Err(trap)),
                            },
                        }
                    }
                    (ConstValue::Bool(x), ConstValue::Bool(y)) => match op {
                        BinOp::Eq => Some(Ok(ConstValue::Bool(x == y))),
                        BinOp::Ne => Some(Ok(ConstValue::Bool(x != y))),
                        BinOp::And => Some(Ok(ConstValue::Bool(x && y))),
                        BinOp::Or => Some(Ok(ConstValue::Bool(x || y))),
                        _ => None,
                    },
                    _ => None,
                }
            }
            ConstExpr::Cast { to, expr } => match self.fold_expr_result(*expr)?.ok()? {
                ConstValue::Int { ty, value } => {
                    if crate::consteval::lossless(ty, *to) {
                        Some(Ok(ConstValue::Int { ty: *to, value }))
                    } else {
                        None
                    }
                }
                ConstValue::Bool(b) => Some(Ok(ConstValue::Int {
                    ty: *to,
                    value: i128::from(b),
                })),
            },
            ConstExpr::Layout { .. } => None,
        }
    }

    pub fn intern(&mut self, kind: TyKind) -> TypeId {
        if let Some(&id) = self.map.get(&kind) {
            return id;
        }
        let id = TypeId(self.kinds.len() as u32);
        self.kinds.push(kind.clone());
        self.map.insert(kind, id);
        id
    }

    pub fn kind(&self, id: TypeId) -> &TyKind {
        &self.kinds[id.index()]
    }

    pub fn len(&self) -> usize {
        self.kinds.len()
    }

    pub fn is_empty(&self) -> bool {
        self.kinds.is_empty()
    }

    pub fn int(&mut self, i: IntTy) -> TypeId {
        self.intern(TyKind::Int(i))
    }

    pub fn is_poison(&self, id: TypeId) -> bool {
        id == self.poison
    }

    pub fn param(&mut self, def: DefId) -> TypeId {
        self.intern(TyKind::Param(def))
    }

    /// Every type directly inside `id` (not the arguments that are constants).
    pub fn children(&self, id: TypeId) -> Vec<TypeId> {
        fn args(out: &mut Vec<TypeId>, a: &[GenericArg]) {
            for g in a {
                if let GenericArg::Ty(t) = g {
                    out.push(*t);
                }
            }
        }
        let mut out = Vec::new();
        match self.kind(id) {
            TyKind::Array(t, _)
            | TyKind::Optional(t)
            | TyKind::Ref(_, t)
            | TyKind::Slice(_, t)
            | TyKind::RawPtr(_, t)
            | TyKind::Projection(t, _) => out.push(*t),
            TyKind::Tuple(ts) => out.extend(ts.iter().copied()),
            TyKind::Adt(_, a) | TyKind::Dyn(_, _, a) | TyKind::Closure(_, _, a) => {
                args(&mut out, a)
            }
            TyKind::Result(a, b) => {
                out.push(*a);
                out.push(*b);
            }
            TyKind::FnPtr(s) | TyKind::DynFn(_, _, s) => {
                out.extend(s.params.iter().copied());
                out.push(s.ret);
                out.extend(s.error);
            }
            _ => {}
        }
        out
    }

    /// True when `id` mentions the poison type or a poison constant anywhere.
    pub fn has_poison(&self, id: TypeId) -> bool {
        let mut seen = Vec::new();
        self.any(id, &mut seen, &mut |k| match k {
            TyKind::Poison => true,
            TyKind::Array(_, ConstArg::Poison) => true,
            TyKind::Adt(_, a) | TyKind::Dyn(_, _, a) | TyKind::Closure(_, _, a) => a
                .iter()
                .any(|g| matches!(g, GenericArg::Const(ConstArg::Poison))),
            _ => false,
        })
    }

    /// True when `id` mentions a generic parameter (or `Self` of an interface).
    pub fn has_params(&self, id: TypeId) -> bool {
        let mut seen = Vec::new();
        self.any(id, &mut seen, &mut |k| match k {
            TyKind::Param(_) => true,
            TyKind::Array(_, ConstArg::Param(_)) => true,
            TyKind::Array(_, ConstArg::Expr(id)) => self.expr_has_params(*id),
            TyKind::Adt(_, a) | TyKind::Dyn(_, _, a) | TyKind::Closure(_, _, a) => {
                a.iter().any(|g| match g {
                    GenericArg::Const(ConstArg::Param(_)) => true,
                    GenericArg::Const(ConstArg::Expr(id)) => self.expr_has_params(*id),
                    _ => false,
                })
            }
            _ => false,
        })
    }

    fn any(&self, id: TypeId, seen: &mut Vec<TypeId>, f: &mut impl FnMut(&TyKind) -> bool) -> bool {
        if seen.contains(&id) {
            return false;
        }
        seen.push(id);
        if f(self.kind(id)) {
            return true;
        }
        self.children(id).into_iter().any(|c| self.any(c, seen, f))
    }

    /// Rebuild `id` bottom-up: children first, then `f` may replace the rebuilt node.
    pub fn fold<F>(&mut self, id: TypeId, f: &mut F) -> TypeId
    where
        F: FnMut(&mut TypeTable, TypeId) -> Option<TypeId>,
    {
        let kind = self.kind(id).clone();
        let rebuilt = match kind {
            TyKind::Array(t, n) => {
                let t = self.fold(t, f);
                TyKind::Array(t, n)
            }
            TyKind::Tuple(ts) => TyKind::Tuple(ts.into_iter().map(|t| self.fold(t, f)).collect()),
            TyKind::Adt(d, a) => {
                let a = self.fold_args(a, f);
                TyKind::Adt(d, a)
            }
            TyKind::Optional(t) => TyKind::Optional(self.fold(t, f)),
            TyKind::Result(a, b) => {
                let a = self.fold(a, f);
                let b = self.fold(b, f);
                TyKind::Result(a, b)
            }
            TyKind::Ref(m, t) => TyKind::Ref(m, self.fold(t, f)),
            TyKind::Slice(m, t) => TyKind::Slice(m, self.fold(t, f)),
            TyKind::RawPtr(m, t) => TyKind::RawPtr(m, self.fold(t, f)),
            TyKind::FnPtr(s) => TyKind::FnPtr(self.fold_sig(s, f)),
            TyKind::DynFn(m, k, s) => TyKind::DynFn(m, k, self.fold_sig(s, f)),
            TyKind::Dyn(m, d, a) => {
                let a = self.fold_args(a, f);
                TyKind::Dyn(m, d, a)
            }
            TyKind::Closure(d, i, a) => {
                let a = self.fold_args(a, f);
                TyKind::Closure(d, i, a)
            }
            TyKind::Projection(t, d) => TyKind::Projection(self.fold(t, f), d),
            other => other,
        };
        let new = self.intern(rebuilt);
        f(self, new).unwrap_or(new)
    }

    fn fold_args<F>(&mut self, args: Vec<GenericArg>, f: &mut F) -> Vec<GenericArg>
    where
        F: FnMut(&mut TypeTable, TypeId) -> Option<TypeId>,
    {
        args.into_iter()
            .map(|g| match g {
                GenericArg::Ty(t) => GenericArg::Ty(self.fold(t, f)),
                c => c,
            })
            .collect()
    }

    fn fold_sig<F>(&mut self, s: FnPtrSig, f: &mut F) -> FnPtrSig
    where
        F: FnMut(&mut TypeTable, TypeId) -> Option<TypeId>,
    {
        FnPtrSig {
            ext: s.ext,
            params: s.params.into_iter().map(|t| self.fold(t, f)).collect(),
            ret: self.fold(s.ret, f),
            error: s.error.map(|t| self.fold(t, f)),
        }
    }

    /// Replace the parameters of `s` in `id`, in types and in constant arguments.
    pub fn subst(&mut self, id: TypeId, s: &Subst) -> TypeId {
        if s.is_empty() {
            return id;
        }
        let id = self.subst_consts(id, s);
        self.fold(id, &mut |table, t| match table.kind(t).clone() {
            TyKind::Param(p) => match s.get(p) {
                Some(GenericArg::Ty(r)) => Some(*r),
                _ => None,
            },
            _ => None,
        })
    }

    /// Substitute constant parameters in array lengths and constant arguments. Types
    /// are handled by `subst`; this walks the same structure for the constants.
    fn subst_consts(&mut self, id: TypeId, s: &Subst) -> TypeId {
        let kind = self.kind(id).clone();
        let rebuilt = match kind {
            TyKind::Array(t, n) => {
                let t = self.subst_consts(t, s);
                let n = self.subst_const(n, s);
                TyKind::Array(t, n)
            }
            TyKind::Tuple(ts) => {
                TyKind::Tuple(ts.into_iter().map(|t| self.subst_consts(t, s)).collect())
            }
            TyKind::Adt(d, a) => TyKind::Adt(d, self.subst_const_args(a, s)),
            TyKind::Dyn(m, d, a) => TyKind::Dyn(m, d, self.subst_const_args(a, s)),
            TyKind::Optional(t) => TyKind::Optional(self.subst_consts(t, s)),
            TyKind::Result(a, b) => {
                let a = self.subst_consts(a, s);
                let b = self.subst_consts(b, s);
                TyKind::Result(a, b)
            }
            TyKind::Ref(m, t) => TyKind::Ref(m, self.subst_consts(t, s)),
            TyKind::Slice(m, t) => TyKind::Slice(m, self.subst_consts(t, s)),
            TyKind::RawPtr(m, t) => TyKind::RawPtr(m, self.subst_consts(t, s)),
            TyKind::FnPtr(sig) => TyKind::FnPtr(self.subst_sig_consts(sig, s)),
            TyKind::DynFn(m, k, sig) => TyKind::DynFn(m, k, self.subst_sig_consts(sig, s)),
            TyKind::Closure(d, i, args) => TyKind::Closure(
                d,
                i,
                args.into_iter()
                    .map(|a| match a {
                        GenericArg::Ty(t) => GenericArg::Ty(self.subst(t, s)),
                        GenericArg::Const(c) => GenericArg::Const(self.subst_const(c, s)),
                    })
                    .collect(),
            ),
            TyKind::Projection(t, d) => TyKind::Projection(self.subst_consts(t, s), d),
            other => other,
        };
        self.intern(rebuilt)
    }

    fn subst_const_args(&mut self, args: Vec<GenericArg>, s: &Subst) -> Vec<GenericArg> {
        args.into_iter()
            .map(|g| match g {
                GenericArg::Ty(t) => GenericArg::Ty(self.subst_consts(t, s)),
                GenericArg::Const(c) => GenericArg::Const(self.subst_const(c, s)),
            })
            .collect()
    }

    fn subst_sig_consts(&mut self, sig: FnPtrSig, s: &Subst) -> FnPtrSig {
        FnPtrSig {
            ext: sig.ext,
            params: sig
                .params
                .into_iter()
                .map(|t| self.subst_consts(t, s))
                .collect(),
            ret: self.subst_consts(sig.ret, s),
            error: sig.error.map(|t| self.subst_consts(t, s)),
        }
    }

    pub fn subst_arg(&mut self, arg: &GenericArg, s: &Subst) -> GenericArg {
        match arg {
            GenericArg::Ty(t) => GenericArg::Ty(self.subst(*t, s)),
            GenericArg::Const(c) => GenericArg::Const(self.subst_const(*c, s)),
        }
    }

    // ------------------------------------------------------------ printing

    /// The type as Ingot source, for messages.
    pub fn display(&self, sema: &Sema, id: TypeId) -> String {
        let mut out = String::new();
        self.write(sema, id, &mut out);
        out
    }

    fn write(&self, sema: &Sema, id: TypeId, out: &mut String) {
        let m = |m: &Mutability| if *m == Mutability::Mut { "mut " } else { "" };
        match self.kind(id) {
            TyKind::Int(i) => out.push_str(i.name()),
            TyKind::Float(f) => out.push_str(f.name()),
            TyKind::Bool => out.push_str("bool"),
            TyKind::Void => out.push_str("void"),
            TyKind::Never => out.push_str("never"),
            TyKind::Array(t, n) => {
                out.push('[');
                self.write_const(sema, n, out);
                out.push(']');
                self.write(sema, *t, out);
            }
            TyKind::Tuple(ts) => {
                out.push('(');
                for (i, t) in ts.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    self.write(sema, *t, out);
                }
                out.push(')');
            }
            TyKind::Adt(d, a) => {
                out.push_str(&sema.def(*d).name);
                self.write_args(sema, a, out);
            }
            TyKind::Optional(t) => {
                out.push('?');
                self.write(sema, *t, out);
            }
            TyKind::Result(t, e) => {
                self.write(sema, *t, out);
                out.push_str(" ! ");
                self.write(sema, *e, out);
            }
            TyKind::Ref(mu, t) => {
                out.push('&');
                out.push_str(m(mu));
                self.write(sema, *t, out);
            }
            TyKind::Slice(mu, t) => {
                out.push_str("[]");
                out.push_str(m(mu));
                self.write(sema, *t, out);
            }
            TyKind::RawPtr(mu, t) => {
                out.push('*');
                out.push_str(m(mu));
                self.write(sema, *t, out);
            }
            TyKind::FnPtr(s) => {
                out.push_str(if s.ext { "extern fn" } else { "fn" });
                self.write_sig(sema, s, out);
            }
            TyKind::Dyn(mu, d, a) => {
                out.push('&');
                out.push_str(m(mu));
                out.push_str("dyn ");
                out.push_str(&sema.def(*d).name);
                self.write_args(sema, a, out);
            }
            TyKind::DynFn(mu, k, s) => {
                out.push('&');
                out.push_str(m(mu));
                out.push_str("dyn ");
                out.push_str(k.name());
                self.write_sig(sema, s, out);
            }
            TyKind::Vector(l, n) => {
                out.push_str(&format!("{}x{}", l.name(), n));
            }
            TyKind::Mask(n) => out.push_str(&format!("boolx{n}")),
            TyKind::Param(d) => out.push_str(&sema.def(*d).name),
            TyKind::Projection(t, d) => {
                self.write(sema, *t, out);
                out.push('.');
                out.push_str(&sema.def(*d).name);
            }
            TyKind::Closure(..) => out.push_str("{closure}"),
            TyKind::Poison => out.push_str("{error}"),
        }
    }

    /// `(A, B) -> R ! E`, the part of a function type after `fn`.
    pub fn display_sig(&self, sema: &Sema, s: &FnPtrSig) -> String {
        let mut out = String::new();
        self.write_sig(sema, s, &mut out);
        out
    }

    fn write_sig(&self, sema: &Sema, s: &FnPtrSig, out: &mut String) {
        out.push('(');
        for (i, t) in s.params.iter().enumerate() {
            if i > 0 {
                out.push_str(", ");
            }
            self.write(sema, *t, out);
        }
        out.push(')');
        if s.ret != self.void {
            out.push_str(" -> ");
            self.write(sema, s.ret, out);
        }
        if let Some(e) = s.error {
            out.push_str(" ! ");
            self.write(sema, e, out);
        }
    }

    fn write_args(&self, sema: &Sema, args: &[GenericArg], out: &mut String) {
        if args.is_empty() {
            return;
        }
        out.push('[');
        for (i, a) in args.iter().enumerate() {
            if i > 0 {
                out.push_str(", ");
            }
            match a {
                GenericArg::Ty(t) => self.write(sema, *t, out),
                GenericArg::Const(c) => self.write_const(sema, c, out),
            }
        }
        out.push(']');
    }

    /// Print a constant-expression tree as written.
    pub fn display_const_expr(&self, sema: &Sema, id: ConstExprId) -> String {
        let mut out = String::new();
        self.write_expr(sema, id, &mut out);
        out
    }

    fn write_expr(&self, sema: &Sema, id: ConstExprId, out: &mut String) {
        self.write_expr_at(sema, id, 0, out);
    }

    fn write_expr_at(&self, sema: &Sema, id: ConstExprId, parent: u8, out: &mut String) {
        let prec = |op: BinOp| match op {
            BinOp::Or => 1,
            BinOp::And => 2,
            BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => 3,
            BinOp::BitOr => 4,
            BinOp::BitXor => 5,
            BinOp::BitAnd => 6,
            BinOp::Shl | BinOp::Shr => 7,
            BinOp::Add
            | BinOp::Sub
            | BinOp::AddWrap
            | BinOp::SubWrap
            | BinOp::AddSat
            | BinOp::SubSat => 8,
            BinOp::Mul | BinOp::Div | BinOp::Rem | BinOp::MulWrap | BinOp::MulSat => 9,
        };
        let mine = match self.const_expr(id) {
            ConstExpr::Binary { op, .. } => prec(*op),
            ConstExpr::Unary { .. } | ConstExpr::Cast { .. } => 10,
            _ => 11,
        };
        let parens = mine < parent;
        if parens {
            out.push('(');
        }
        match self.const_expr(id) {
            ConstExpr::Int { value, .. } => out.push_str(&value.to_string()),
            ConstExpr::Bool(b) => out.push_str(&b.to_string()),
            ConstExpr::Param(d) => out.push_str(&sema.def(*d).name),
            ConstExpr::Unary { op, expr } => {
                out.push_str(match op {
                    ingot_syntax::ast::UnOp::Neg => "-",
                    ingot_syntax::ast::UnOp::NegWrap => "-%",
                    ingot_syntax::ast::UnOp::Not => "!",
                    ingot_syntax::ast::UnOp::BitNot => "~",
                    _ => "?",
                });
                self.write_expr_at(sema, *expr, 10, out);
            }
            ConstExpr::Binary { op, lhs, rhs } => {
                self.write_expr_at(sema, *lhs, prec(*op), out);
                out.push(' ');
                out.push_str(op.symbol());
                out.push(' ');
                self.write_expr_at(sema, *rhs, prec(*op) + 1, out);
            }
            ConstExpr::Cast { to, expr } => {
                self.write_expr_at(sema, *expr, 10, out);
                out.push_str(" as ");
                out.push_str(to.name());
            }
            ConstExpr::Layout { size, ty } => {
                out.push_str(if *size { "@sizeof(" } else { "@alignof(" });
                out.push_str(&self.display(sema, *ty));
                out.push(')');
            }
        }
        if parens {
            out.push(')');
        }
    }

    fn write_const(&self, sema: &Sema, c: &ConstArg, out: &mut String) {
        match c {
            ConstArg::Value(ConstValue::Int { value, .. }) => out.push_str(&value.to_string()),
            ConstArg::Value(ConstValue::Bool(b)) => out.push_str(&b.to_string()),
            ConstArg::Param(d) => out.push_str(&sema.def(*d).name),
            ConstArg::Expr(id) => self.write_expr(sema, *id, out),
            ConstArg::Poison => out.push_str("{error}"),
        }
    }
}

/// The `Lane` of a scalar, when a vector may have it.
pub fn lane_of(s: ScalarTy) -> Option<Lane> {
    if let Some(i) = IntTy::from_scalar(s) {
        return Some(Lane::Int(i));
    }
    match s {
        ScalarTy::F32 => Some(Lane::Float(FloatTy::F32)),
        ScalarTy::F64 => Some(Lane::Float(FloatTy::F64)),
        _ => None,
    }
}
