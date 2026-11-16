//! The type table: interned types (pipeline section 4.4).
//!
//! A type is a `TypeId`, an index into a `TypeTable` that holds each distinct `TyKind`
//! once. Type equality is therefore `TypeId` equality: nominal for structs, enums and
//! interfaces (their `DefId`), structural for everything else, and two instances of a
//! generic type are equal when their arguments are.

use crate::defs::{DefId, Sema};
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

/// The length of an array or the value of a constant generic argument.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ConstArg {
    Value(ConstValue),
    /// A constant generic parameter (`N` in `[N]T`), known per instance.
    Param(DefId),
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

fn subst_const(c: ConstArg, s: &Subst) -> ConstArg {
    match c {
        ConstArg::Param(p) => match s.get(p) {
            Some(GenericArg::Const(v)) => *v,
            _ => c,
        },
        _ => c,
    }
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
        };
        t.poison = t.intern(TyKind::Poison);
        t.bool_ = t.intern(TyKind::Bool);
        t.void = t.intern(TyKind::Void);
        t.never = t.intern(TyKind::Never);
        t.i64_ = t.intern(TyKind::Int(IntTy::I64));
        t
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
            TyKind::Adt(_, a) | TyKind::Dyn(_, _, a) | TyKind::Closure(_, _, a) => a
                .iter()
                .any(|g| matches!(g, GenericArg::Const(ConstArg::Param(_)))),
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
                let n = match n {
                    ConstArg::Param(p) => match s.get(p) {
                        Some(GenericArg::Const(c)) => *c,
                        _ => n,
                    },
                    n => n,
                };
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
                        GenericArg::Const(c) => GenericArg::Const(subst_const(c, s)),
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
                GenericArg::Const(ConstArg::Param(p)) => match s.get(p) {
                    Some(GenericArg::Const(c)) => GenericArg::Const(*c),
                    _ => GenericArg::Const(ConstArg::Param(p)),
                },
                c => c,
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
            GenericArg::Const(ConstArg::Param(p)) => match s.get(*p) {
                Some(GenericArg::Const(c)) => GenericArg::Const(*c),
                _ => arg.clone(),
            },
            GenericArg::Const(_) => arg.clone(),
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

    fn write_const(&self, sema: &Sema, c: &ConstArg, out: &mut String) {
        match c {
            ConstArg::Value(ConstValue::Int { value, .. }) => out.push_str(&value.to_string()),
            ConstArg::Value(ConstValue::Bool(b)) => out.push_str(&b.to_string()),
            ConstArg::Param(d) => out.push_str(&sema.def(*d).name),
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
