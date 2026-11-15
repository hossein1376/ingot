//! The result of pipeline phase 7: a typed view of every function, test, constant and
//! static body.
//!
//! The HIR (`hir.rs`) is not changed. A [`TypedBody`] sits beside it and is keyed by the
//! same ids (`ExprId`, `PatId`, `LocalId`, `StmtId`; the arenas are per package, and a
//! body knows its package). **Phase 8 (lowering to a CFG) reads the HIR and this and
//! nothing else.** Everything the source leaves implicit is explicit here:
//!
//! * the type of every expression ([`ExprInfo::ty`]), every pattern ([`PatInfo::ty`])
//!   and every local ([`LocalInfo::ty`]). An untyped numeric literal has the type it
//!   took (its context's, or the default `i64` or `f64`); nothing is left to infer. An
//!   integer literal may have type `f32` or `f64` when its value is exactly
//!   representable there, so phase 8 converts it to a float;
//! * what a name or member resolved to ([`ExprRes`]): a local, a function with its
//!   generic arguments, a constant, a static, a field (with how many borrows were
//!   looked through), a tuple field, an enum variant;
//! * every call: the callee, and for a method call the receiver adjustment (by value,
//!   `&`, `&mut`, after looking through `derefs` borrows), the interface and impl the
//!   function was found through ([`FnSource`]);
//! * the conversion a `try` applies ([`TryConversion`]);
//! * the binding mode of every pattern binding ([`BindMode`], stored on the local);
//! * for each `for` loop, how it iterates ([`ForKind`]);
//! * the signature and captures of every closure ([`ClosureInfo`]).
//!
//! A body in which phase 7 reported an error (or met poison) has [`TypedBody::failed`]
//! set; phases 8 to 10 skip it (pipeline section 3, "Phase gates"). The information of a
//! failed body is partial and must not be read. An entry of a failed body whose type is
//! not poison is final once the expression that owns it has been typed (a coercion may
//! rewrite the entry of the expression being coerced while it is typed, and nothing
//! changes it later), and may be read by tools, not by phases 8 to 10.
//!
//! Types in a generic body mention the generic parameters (`TyKind::Param`): the body is
//! checked once, never instantiated here (pipeline section 1).

use crate::defs::DefId;
use crate::hir::{BlockId, ExprId, LocalId, PatId, StmtId};
use crate::ty::{ConstArg, FnKind, FnPtrSig, GenericArg, Mutability, TypeId};
use ingot_diag::source::Span;
use std::collections::BTreeMap;

/// Why a body exists.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BodyKind {
    Fn,
    Test,
    Const,
    Static,
}

/// Where a body starts: a function or test body is a block, a `const` or `static`
/// initializer an expression.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BodyRoot {
    Block(BlockId),
    Expr(ExprId),
}

/// The typed body of one function, test, `const` or `static`. A closure's body is part
/// of the body that contains it (its ids live in the same maps) and is described by
/// [`TypedBody::closures`].
#[derive(Clone, Debug)]
pub struct TypedBody {
    /// The function, test, constant or static.
    pub owner: DefId,
    pub kind: BodyKind,
    /// Index of the package whose HIR arenas the ids refer to.
    pub package: usize,
    pub root: BodyRoot,
    /// The locals of the parameters, in order (functions only).
    pub params: Vec<LocalId>,
    /// The result type the body returns (`void` for a test, the declared type for a
    /// constant or static).
    pub ret: TypeId,
    /// The error type of a fallible function (`T ! E`): `return v` succeeds with `v`
    /// of type `ret` and `fail e` fails with `e` of this type.
    pub error: Option<TypeId>,
    pub exprs: BTreeMap<ExprId, ExprInfo>,
    pub pats: BTreeMap<PatId, PatInfo>,
    pub locals: BTreeMap<LocalId, LocalInfo>,
    pub stmts: BTreeMap<StmtId, StmtInfo>,
    pub closures: BTreeMap<ExprId, ClosureInfo>,
    /// Phase 7 reported an error in this body, or the body mentions poison: phases 8
    /// to 10 do not run for it.
    pub failed: bool,
}

impl TypedBody {
    pub fn expr(&self, id: ExprId) -> Option<&ExprInfo> {
        self.exprs.get(&id)
    }
    pub fn pat(&self, id: PatId) -> Option<&PatInfo> {
        self.pats.get(&id)
    }
    pub fn local(&self, id: LocalId) -> Option<&LocalInfo> {
        self.locals.get(&id)
    }
    pub fn stmt(&self, id: StmtId) -> Option<&StmtInfo> {
        self.stmts.get(&id)
    }
}

/// The result of phase 7 for a package: one typed body per body-carrying item, keyed by
/// the item's definition.
#[derive(Clone, Debug, Default)]
pub struct Bodies {
    pub bodies: BTreeMap<DefId, TypedBody>,
}

// ------------------------------------------------------------- expressions

#[derive(Clone, Debug)]
pub struct ExprInfo {
    /// The type of the expression. Never `Poison` in a body that did not fail.
    pub ty: TypeId,
    pub res: ExprRes,
    /// The coercion of this expression's value to `&dyn I` or `&mut dyn I`, when its
    /// context expects one and the expression's own type is `&T` or `&mut T`. The value
    /// of `ty` is then the `&dyn I`; phase 8 builds the fat pointer and the method table
    /// from this.
    pub coerce: Option<DynCoercion>,
    /// The coercion of this expression's value to `&dyn Fn(..)` or
    /// `&mut dyn FnMut(..)`, when its context expects one and its own type is a borrow
    /// of a closure. The value of `ty` is then the dynamic type.
    pub fn_dyn: Option<FnDynCoercion>,
    /// The use is a read through a shared borrow, not a move: an argument of `@format`
    /// or `@debug` (section 35).
    pub shared_read: bool,
}

/// A `&T` (`&mut T`) that becomes a `&dyn I` (`&mut dyn I`) because `T` implements
/// `I` (spec sections 3 and 4.4). The target type is the expression's `ty`.
#[derive(Clone, Debug)]
pub struct DynCoercion {
    pub interface: DefId,
    pub interface_args: Vec<GenericArg>,
    /// The `T` under the borrow.
    pub self_ty: TypeId,
    /// How `T` implements `interface`.
    pub via: InterfaceVia,
}

/// A `&closure` (`&mut closure`) that becomes a `&dyn Fn(..)` (`&mut dyn FnMut(..)`)
/// because the closure satisfies the bound with that signature (spec sections 3 and
/// 5.6). The target type is the expression's `ty`.
#[derive(Clone, Debug)]
pub struct FnDynCoercion {
    pub kind: FnKind,
    pub sig: FnPtrSig,
    /// The closure's own type under the borrow.
    pub self_ty: TypeId,
}

/// What the source left implicit on an expression. Which variant occurs with which
/// `hir::ExprKind` is stated on each.
#[derive(Clone, Debug, Default)]
pub enum ExprRes {
    /// Nothing beyond the type.
    #[default]
    None,
    /// `Path`: a local variable or parameter.
    Local(LocalId),
    /// `Path` to a function, or a method named as a function value; also the callee
    /// expression of a call.
    Fn(FnRef),
    /// `Path`: a `const` item.
    Const(DefId),
    /// `Path`: a `static` item.
    Static(DefId),
    /// `Path`: a constant generic parameter used as a value.
    ConstParam(DefId),
    /// `Field`: a struct field reached after looking through `derefs` borrows of the
    /// base. `index` is the position in the struct's declaration; `def` the field.
    Field {
        derefs: u32,
        struct_def: DefId,
        index: usize,
        def: DefId,
    },
    /// `Field` `.len` on a slice: the base is looked through `derefs` borrows first.
    SliceLen { derefs: u32 },
    /// `Field` `.ptr` on a slice: E1301 outside `unsafe`.
    SlicePtr { derefs: u32 },
    /// `Field` `.len` on an array: the length is the array's constant `N`, which may be
    /// a constant generic parameter.
    ArrayLen { derefs: u32, len: ConstArg },
    /// `TupleField`: element `index` of the tuple reached after `derefs` borrows.
    TupleField { derefs: u32 },
    /// `Index`: the base is looked through `derefs` borrows (an array or slice, or a
    /// vector with a constant lane).
    Index { derefs: u32 },
    /// `Slice` (`s[lo..hi]`, `s[lo..]`, `s[..hi]`, `s[..]`). `of_array`: the base is an
    /// array place and the result is the borrow written around the expression
    /// (`&a[lo..hi]`). `implicit_lo`/`implicit_hi`: the bound was left out and phase 8
    /// inserts `0` or `.len`.
    Slice {
        derefs: u32,
        of_array: bool,
        implicit_lo: bool,
        implicit_hi: bool,
    },
    /// `EnumLit`, or a `Field` on a type naming a payload-free variant (`Color.red`),
    /// or the callee of a variant constructor call.
    Variant(VariantRef),
    /// `StructLit` and `StructShorthand`.
    Struct(StructRes),
    /// `Call`.
    Call(Box<CallInfo>),
    /// `Cast`.
    Cast(CastKind),
    /// `Unary` with `Try`.
    Try(TryInfo),
    /// `Builtin`: the type argument, when the builtin takes one.
    Builtin { type_arg: Option<TypeId> },
    /// `Idx.pos()` or `Idx.offset(d)` (section 27.1).
    IdxMethod(IdxMethod),
    /// `in.at_clamped(j)` or `in.at_or(j, default)` on an input slice.
    SliceAt { clamped: bool },
    /// `Closure`: see `TypedBody::closures`.
    Closure,
    /// `Deref`: the base is a borrow (`Borrow`) or a raw pointer (`Raw`).
    Deref { raw: bool },
    /// `Unary` with `Borrow` or `BorrowMut`.
    Borrow(Mutability),
    /// `Spawn`: the type of the task, `Task[R]`, is the expression's type.
    Spawn,
    /// `@gather(V, s, idx)`: phase 8 inserts the T14 bounds check.
    Gather,
    /// `@scatter(s, idx, v)`: phase 8 inserts the T14 bounds check and the T15
    /// distinctness check.
    Scatter,
    /// `Dispatch`: see [`DispatchInfo`].
    Dispatch(Box<DispatchInfo>),
}

/// `Idx.pos()` gives the position; `Idx.offset(d)` gives a neighboring index.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IdxMethod {
    Pos,
    Offset,
}

/// What later phases need for `dispatch kernel(args) over lo..hi on executor`.
#[derive(Clone, Debug)]
pub struct DispatchInfo {
    pub kernel: DefId,
    /// The kernel's generic arguments, in order.
    pub kernel_args: Vec<GenericArg>,
    pub executor_ty: TypeId,
    /// The executor expression (an `ExprId` of the body's package).
    pub executor: ExprId,
    /// The domain, always one range of `i64` here.
    pub lo: ExprId,
    pub hi: ExprId,
    /// The kernel has a `reduce` clause and therefore returns a value.
    pub reduce: bool,
}

/// A reference to a function that is known at compile time.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FnRef {
    /// The function definition: the impl's own, or the interface's when the impl
    /// inherits a default body.
    pub def: DefId,
    /// The arguments of the function's own generic parameters, in order.
    pub args: Vec<GenericArg>,
    pub source: FnSource,
}

/// Where a function reference came from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FnSource {
    /// A free function, an `extern` function, or a function of the package's modules.
    Free,
    /// A function of `impl T { .. }`, with the arguments of the impl's generic
    /// parameters (in the order of the impl's generics) that make it apply.
    Inherent {
        impl_def: DefId,
        impl_args: Vec<GenericArg>,
    },
    /// A function found through an interface, for the type `self_ty`.
    Interface {
        interface: DefId,
        interface_args: Vec<GenericArg>,
        self_ty: TypeId,
        via: InterfaceVia,
    },
}

/// How a call through an interface is resolved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InterfaceVia {
    /// `impl I for T { .. }` written in the program, with its generic arguments.
    Impl {
        impl_def: DefId,
        impl_args: Vec<GenericArg>,
    },
    /// The language implements the built-in interface for the type (section 4.4).
    Builtin,
    /// A bound of a type parameter: resolved when the generic is instantiated.
    Bound,
    /// A call through `&dyn I`.
    Dyn,
}

/// An enum variant. For `?T` and `T ! E` there is no definition: `index` is 0 for
/// `none` and `ok`, 1 for `some` and `err`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VariantRef {
    /// The enum type, with its generic arguments.
    pub enum_ty: TypeId,
    pub index: usize,
    /// The variant's definition (user enums only).
    pub def: Option<DefId>,
    /// The type of the payload, with the enum's arguments applied; `None` if the
    /// variant has no payload.
    pub payload: Option<TypeId>,
}

#[derive(Clone, Debug)]
pub struct StructRes {
    pub def: DefId,
    pub args: Vec<GenericArg>,
    /// For each field initializer in source order, the index of the field it sets.
    pub order: Vec<usize>,
}

#[derive(Clone, Debug)]
pub struct CallInfo {
    pub callee: Callee,
    /// Present for `value.name(args)`: how the receiver becomes the first argument.
    pub receiver: Option<Receiver>,
    /// A call through a closure: how the callee is used (shared, exclusive, moved).
    /// Phase 9 checks the exclusive use and the move.
    pub mode: Option<CallMode>,
}

/// How a call through a closure uses it (spec section 5.6).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallMode {
    Shared,
    Exclusive,
    Move,
}

#[derive(Clone, Debug)]
pub enum Callee {
    /// A function known at compile time. For a method call the receiver is the first
    /// argument; `args` of the call expression do not include it.
    Fn(FnRef),
    /// An enum variant constructor `Shape.circle(2.0)`.
    Variant(VariantRef),
    /// A call of a value: a `fn` pointer, a closure, a parameter bounded by
    /// `Fn`/`FnMut`/`FnOnce`, or a `&dyn Fn..`.
    Value(ValueCall),
}

#[derive(Clone, Debug)]
pub enum ValueCall {
    /// `fn(A) -> R` or `extern fn(A) -> R`.
    FnPtr,
    Closure(ExprId),
    /// A value of a type parameter with an `Fn` bound; resolved at instantiation.
    Bound(FnKind),
    /// A `&dyn Fn(A) -> R`.
    DynFn(FnKind),
}

/// The receiver of a method call: start from the receiver expression, look through
/// `derefs` borrows (`v.*` that many times), then pass it as `mode` says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Receiver {
    pub derefs: u32,
    pub mode: ReceiverMode,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReceiverMode {
    /// `self: T`: a `Copy` value is copied and anything else is moved.
    Value,
    /// `self: &T`: a shared borrow of the place (a reborrow when `derefs > 0`).
    Shared,
    /// `self: &mut T`: an exclusive borrow of the place.
    Exclusive,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CastKind {
    /// Integer to a wider integer.
    IntWiden,
    /// Integer to float, or `f32` to `f64`.
    ToFloat,
    /// `bool` to an integer.
    BoolToInt,
    /// A payload-free enum to its tag type.
    EnumToTag,
    /// `&T` to `*T`, `&mut T` to `*mut T`.
    RefToPtr,
    /// The source and target types are the same.
    Identity,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TryInfo {
    pub conversion: TryConversion,
}

/// How the error of a `try` operand becomes the error of the enclosing function.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TryConversion {
    /// The error types are the same.
    Identity,
    /// The enclosing function's error is an enum, and the error is wrapped in this
    /// variant (declared `from` the operand's error type).
    FromVariant(VariantRef),
}

// ---------------------------------------------------------------- patterns

#[derive(Clone, Debug)]
pub struct PatInfo {
    /// The type of the value the pattern is matched against, after looking through
    /// `derefs` borrows (a pattern other than a binding or `_` sees through borrows).
    pub ty: TypeId,
    /// How many borrows of the scrutinee the pattern looked through.
    pub derefs: u32,
    pub res: PatRes,
}

#[derive(Clone, Debug, Default)]
pub enum PatRes {
    #[default]
    None,
    Variant(VariantRef),
    /// A struct pattern. `res.order` holds, for each field pattern in source order, the
    /// field's index. `rest`: the pattern ended in `..`. `omitted`: the fields it left
    /// out, in declaration order (wildcards for exhaustiveness).
    ///
    /// Phase 9 (the ownership checker, S6) must drop every omitted field when the
    /// pattern moves the value, run the `Discard` check on each dropped field, and
    /// report E1003 when an omitted field's type is linear.
    Struct {
        res: StructRes,
        rest: bool,
        omitted: Vec<usize>,
    },
}

/// How a pattern binding gets its value (section 7.2): a copy or move of the value, or
/// a borrow of the place because the scrutinee is a borrow (`match &v`, `match &mut v`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BindMode {
    Copy,
    Move,
    Ref,
    RefMut,
}

#[derive(Clone, Debug)]
pub struct LocalInfo {
    pub ty: TypeId,
    pub mode: BindMode,
}

// -------------------------------------------------------------- statements

#[derive(Clone, Debug)]
pub enum StmtInfo {
    /// `for`: how the loop iterates and what its variable (and index) are.
    For(ForInfo),
    /// A `let` or `var`: the declared or inferred type of the initializer.
    Let { ty: TypeId },
    /// `scope s { .. }`: the type of `s` (`&Scope`).
    Scope { ty: TypeId },
    /// A compound assignment `place op= value`: the type of the place.
    Assign { ty: TypeId },
    /// A statement-form `dispatch` (a kernel without `reduce`).
    Dispatch(Box<DispatchInfo>),
}

#[derive(Clone, Debug)]
pub struct ForInfo {
    pub kind: ForKind,
    /// The type of the loop variable.
    pub item: TypeId,
}

#[derive(Clone, Debug)]
pub enum ForKind {
    /// `lo..hi` or `lo..=hi` over integers of one type (counted loop).
    Range { ty: TypeId, inclusive: bool },
    /// An array or slice of `Copy` elements: the item is a copy of each element.
    Copying,
    /// A `[]mut T`: the item is `&mut T`.
    ExclusiveSlice,
    /// An array value of non-`Copy` elements: moved out, the array is consumed.
    Consuming { linear: bool },
    /// A value whose type implements `Iter`: `next` is called until `.none`.
    Iter { iter_ty: TypeId, next: FnRef },
}

// ----------------------------------------------------------------- closures

#[derive(Clone, Debug)]
pub struct ClosureInfo {
    pub sig: FnPtrSig,
    /// The type of the closure expression: `TyKind::Closure`.
    pub ty: TypeId,
    /// `Fn`, `FnMut` or `FnOnce`, from the capture list (spec section 5.6).
    pub kind: FnKind,
    pub captures: Vec<CaptureInfo>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct CaptureInfo {
    /// The binding the body sees.
    pub local: LocalId,
    /// The captured outer local.
    pub outer: Option<LocalId>,
    /// Shared borrow, exclusive borrow, or copy/move of the variable.
    pub mode: CaptureMode,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaptureMode {
    Shared,
    Exclusive,
    ByValue,
}
