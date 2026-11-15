//! The value of a constant (pipeline section 4.6, "The value of a constant").
//!
//! A value is a tree: scalars, aggregates (a struct, tuple or array, or an enum
//! variant with its payload), string literals, function values, and the addresses of
//! statics. The interpreter in `ingot-cfg` builds these; the declarations store them
//! per definition, and code generation will read them.
//!
//! `ConstValue` (in `ty.rs`) stays the small, `Eq`/`Ord`/`Hash` value of a type-level
//! constant argument. This tree is the richer value of a `const` or `static`.

use crate::defs::DefId;
use crate::hir::ExprId;
use crate::ty::TypeId;
use crate::typed::FnRef;

/// Which allocation a borrow points into.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AllocRef {
    /// A local or temporary of the evaluation in progress. It cannot appear in the
    /// value of a `const` or `static`: phase 9 rules out a borrow of a local that
    /// outlives it.
    Local(u32),
    /// A `static` item. Its address is a constant; its value is not (E0807).
    Static(DefId),
}

/// One step of a path inside an allocation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PathComp {
    /// A struct field (its declaration index), a tuple field, or an array element.
    Field(usize),
    /// The payload of enum variant `0`.
    Payload(usize),
}

/// What a slice borrows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SliceSource {
    /// A string literal's bytes. A literal is its own read-only allocation.
    Str(Vec<u8>),
    /// A place inside an allocation.
    Alloc {
        alloc: AllocRef,
        path: Vec<PathComp>,
    },
}

/// The shape of an aggregate value.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AggKind {
    Struct(DefId),
    Tuple,
    Array,
    Closure(ExprId),
}

/// The elements of a [`Value::Pack`]: the raw bits of each scalar, by index.
#[derive(Clone, Debug, PartialEq)]
pub enum PackElems {
    Ints(Vec<i128>),
}

/// The value of a constant expression.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Int {
        ty: TypeId,
        value: i128,
    },
    /// The bits of an `f32` (in the low 32) or an `f64`.
    Float {
        ty: TypeId,
        bits: u64,
    },
    Bool(bool),
    Unit,
    Aggregate {
        kind: AggKind,
        ty: TypeId,
        fields: Vec<Value>,
    },
    /// A whole array of a scalar element type. `elems` is `None` while every element
    /// is zero (what `@zero` makes); a written element materializes `Ints`, with the
    /// element's raw bits (`bool` as 0/1, a float's bits). `moved` holds the indices
    /// whose value was moved out. This keeps a large array of scalars compact.
    Pack {
        ty: TypeId,
        len: u64,
        elems: Option<Box<PackElems>>,
        moved: std::collections::BTreeSet<u32>,
    },
    Variant {
        ty: TypeId,
        index: usize,
        payload: Option<Box<Value>>,
    },
    /// A `[]u8` that is a string literal (or a slice of one).
    Str(Vec<u8>),
    /// A `*u8` that is a C string literal.
    CStr(Vec<u8>),
    Fn {
        fn_ref: Box<FnRef>,
        name: String,
    },
    /// `&place` or `&mut place`.
    Borrow {
        alloc: AllocRef,
        path: Vec<PathComp>,
    },
    Slice {
        source: SliceSource,
        lo: u64,
        len: u64,
    },
    /// A `*u8`/`*T` into a string literal (the address of a literal).
    StrPtr {
        bytes: Vec<u8>,
        at: u64,
    },
    /// Storage that holds no value: never read, only written. A read of it stops
    /// evaluation (phase 9 will report the read).
    Uninit,
}

impl Value {
    pub fn int(ty: TypeId, value: i128) -> Value {
        Value::Int { ty, value }
    }
}
