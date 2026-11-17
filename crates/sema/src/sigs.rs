//! The results of phase 6: the signature of every item, the per-type implementation
//! tables, type properties and layouts. Phase 7 reads these and nothing else about
//! declarations; no function body is looked at to produce them.

use crate::consteval::{ConstState, ValueState};
use crate::defs::{Builtin, DefId};
use crate::hir::ExprId;
use crate::ty::{FnKind, FnPtrSig, GenericArg, IntTy, Subst, TypeId, TypeTable};
use crate::typed::TypedBody;
use ingot_diag::diagnostic::Diagnostic;
use ingot_diag::source::Span;
use ingot_syntax::ast::{FnMods, StructKind};
use std::collections::{BTreeMap, BTreeSet};

/// What a generic parameter, an interface or an impl requires of a type.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Bound {
    /// `Copy`, `Discard`, `Send`, `Sync`, `Owned`, `Int`, `SInt`, `UInt`, `Float`,
    /// `Num`: a property of the type itself.
    Prop(PropBound),
    /// `Fn(A) -> R`, `FnMut(..)`, `FnOnce(..)`.
    Fn(FnKind, FnPtrSig),
    /// An interface, user-declared or built in (`Eq`, `Ord`, `Hash`, ...), with its
    /// generic arguments.
    Interface(DefId, Vec<GenericArg>),
    /// The bound could not be read (and was reported, or is a gap).
    Poison,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PropBound {
    Copy,
    Discard,
    Send,
    Sync,
    Owned,
    AtomicValue,
    Int,
    SInt,
    UInt,
    Float,
    Num,
}

impl PropBound {
    pub fn from_builtin(b: Builtin) -> Option<PropBound> {
        Some(match b {
            Builtin::Copy => PropBound::Copy,
            Builtin::Discard => PropBound::Discard,
            Builtin::Send => PropBound::Send,
            Builtin::Sync => PropBound::Sync,
            Builtin::Owned => PropBound::Owned,
            Builtin::AtomicValue => PropBound::AtomicValue,
            Builtin::Int => PropBound::Int,
            Builtin::SInt => PropBound::SInt,
            Builtin::UInt => PropBound::UInt,
            Builtin::Float => PropBound::Float,
            Builtin::Num => PropBound::Num,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            PropBound::Copy => "Copy",
            PropBound::Discard => "Discard",
            PropBound::Send => "Send",
            PropBound::Sync => "Sync",
            PropBound::Owned => "Owned",
            PropBound::AtomicValue => "AtomicValue",
            PropBound::Int => "Int",
            PropBound::SInt => "SInt",
            PropBound::UInt => "UInt",
            PropBound::Float => "Float",
            PropBound::Num => "Num",
        }
    }
}

/// A generic parameter of an item.
#[derive(Clone, Debug)]
pub struct GenericSig {
    pub def: DefId,
    pub name: String,
    pub is_const: bool,
    /// The type of a constant parameter.
    pub const_ty: Option<TypeId>,
    pub bounds: Vec<Bound>,
    /// The source spans of the bounds, parallel to `bounds`.
    pub bound_spans: Vec<Span>,
    pub span: Span,
}

/// The argument of an `align(...)` attribute, as evaluated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AlignAttr {
    pub value: i128,
    pub span: Span,
    /// Set when the value depends on a constant generic parameter; phase 11 evaluates
    /// it per instance. `value` is then 0 and must not be used for layout.
    pub raw: Option<(DefId, crate::hir::ExprId)>,
}

#[derive(Clone, Debug)]
pub struct ParamSig {
    pub name: String,
    pub name_span: Span,
    pub ty: TypeId,
    pub ty_span: Span,
    /// `into p`: the clause's span and the index of the parameter it names, or `None`
    /// when the name is not a parameter.
    pub into: Option<(Span, Option<usize>)>,
}

#[derive(Clone, Debug)]
pub struct OriginSig {
    pub name: String,
    pub span: Span,
    pub param: Option<usize>,
    pub inner: bool,
    pub is_static: bool,
}

#[derive(Clone, Debug)]
pub struct FnSig {
    pub def: DefId,
    /// The impl or interface that contains the function.
    pub parent: Option<DefId>,
    pub name: String,
    pub name_span: Span,
    pub mods: FnMods,
    pub params: Vec<ParamSig>,
    pub ret: TypeId,
    pub ret_span: Span,
    pub error: Option<TypeId>,
    pub error_span: Option<Span>,
    pub from: Vec<OriginSig>,
    pub reduce: Option<String>,
    /// The span of the `reduce(op)` clause.
    pub reduce_span: Option<Span>,
    pub variadic: bool,
    pub abi: Option<String>,
    pub has_body: bool,
    pub span: Span,
}

impl FnSig {
    /// A method: its first parameter is named `self`.
    pub fn is_method(&self) -> bool {
        self.params.first().is_some_and(|p| p.name == "self")
    }
}

#[derive(Clone, Debug)]
pub struct FieldSig {
    pub def: DefId,
    pub name: String,
    pub name_span: Span,
    pub public: bool,
    /// `pub(pkg)`: visible in every module of the declaring package.
    pub pkg: bool,
    pub ty: TypeId,
    pub ty_span: Span,
    /// The argument of `align(...)` on the field.
    pub align: Option<AlignAttr>,
}

#[derive(Clone, Debug)]
pub struct StructSig {
    pub def: DefId,
    pub kind: StructKind,
    pub fields: Vec<FieldSig>,
    pub align: Option<AlignAttr>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct VariantSig {
    pub def: DefId,
    pub name: String,
    pub name_span: Span,
    pub payload: Option<TypeId>,
    pub payload_span: Option<Span>,
    pub from: bool,
    /// The tag value (explicit, or counted up from the previous variant).
    pub tag: i128,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct EnumSig {
    pub def: DefId,
    pub copy: bool,
    pub tag_ty: IntTy,
    /// The tag type was written (`enum Color: u8`).
    pub tag_declared: bool,
    pub variants: Vec<VariantSig>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct InterfaceSig {
    pub def: DefId,
    /// `interface I: A + B`.
    pub supers: Vec<Bound>,
    pub self_def: DefId,
    /// Methods, in source order.
    pub methods: Vec<DefId>,
    /// Associated types, in source order.
    pub assoc: Vec<DefId>,
    pub span: Span,
}

/// What an impl implements.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ImplOf {
    /// `impl T { ... }`
    Inherent,
    /// `impl I for T`, `I` an interface with its generic arguments.
    Interface(DefId, Vec<GenericArg>),
    /// `impl Send for T`, `impl Sync for T`.
    Prop(PropBound),
}

#[derive(Clone, Debug)]
pub struct ImplSig {
    pub def: DefId,
    pub unsafe_: bool,
    pub of: ImplOf,
    pub target: TypeId,
    pub self_def: DefId,
    pub methods: BTreeMap<String, DefId>,
    /// `type Item = T;`: the alias definition of each associated type.
    pub assoc: BTreeMap<String, DefId>,
    pub consts: BTreeMap<String, DefId>,
    /// The impl had an error that makes it unusable: it is in no table.
    pub poisoned: bool,
    pub span: Span,
    pub header: Span,
}

#[derive(Clone, Debug)]
pub struct ConstSig {
    pub def: DefId,
    pub parent: Option<DefId>,
    pub ty: TypeId,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct StaticSig {
    pub def: DefId,
    pub ty: TypeId,
    pub ty_span: Span,
    pub align: Option<AlignAttr>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct AliasSig {
    pub def: DefId,
    pub parent: Option<DefId>,
    /// The aliased type, or `None` for `type Item;` of an interface.
    pub ty: Option<TypeId>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub enum ItemSig {
    Fn(FnSig),
    Struct(StructSig),
    Enum(EnumSig),
    Interface(InterfaceSig),
    Impl(ImplSig),
    Const(ConstSig),
    Static(StaticSig),
    Alias(AliasSig),
}

// ------------------------------------------------------------- properties

/// Three-valued truth: `Unknown` is a property the specification does not define for
/// the type. Order: `No < Unknown < Yes`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Tri {
    No,
    Unknown,
    Yes,
}

impl Tri {
    pub fn and(self, o: Tri) -> Tri {
        self.min(o)
    }
    pub fn or(self, o: Tri) -> Tri {
        self.max(o)
    }
    pub fn from_bool(b: bool) -> Tri {
        if b { Tri::Yes } else { Tri::No }
    }
    pub fn is_yes(self) -> bool {
        self == Tri::Yes
    }
    pub fn is_no(self) -> bool {
        self == Tri::No
    }
}

/// The properties of section 10 and 20 of one type.
///
/// `linear` and `view` are definite: they hold because of what the type is made of.
/// `maybe_linear` and `maybe_view` also hold for a type parameter that no bound
/// rules out (an unbounded `T` "might be linear" and "might be a view"); they are what
/// a generic body is checked against. For a type without parameters they are equal to
/// the definite ones. `Discard` is `!maybe_linear` and `Owned` is `!maybe_view`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Props {
    pub copy: Tri,
    pub linear: Tri,
    pub maybe_linear: Tri,
    pub view: Tri,
    pub maybe_view: Tri,
    pub send: Tri,
    pub sync: Tri,
}

impl Props {
    /// The starting point of the fixed point: every positive property holds, no
    /// negative one does.
    pub fn optimistic() -> Props {
        Props {
            copy: Tri::Yes,
            linear: Tri::No,
            maybe_linear: Tri::No,
            view: Tri::No,
            maybe_view: Tri::No,
            send: Tri::Yes,
            sync: Tri::Yes,
        }
    }

    pub fn unknown() -> Props {
        Props {
            copy: Tri::Unknown,
            linear: Tri::Unknown,
            maybe_linear: Tri::Unknown,
            view: Tri::Unknown,
            maybe_view: Tri::Unknown,
            send: Tri::Unknown,
            sync: Tri::Unknown,
        }
    }

    /// May go out of scope unconsumed.
    pub fn discard(&self) -> Tri {
        self.maybe_linear.negate()
    }

    /// Holds no borrow.
    pub fn owned(&self) -> Tri {
        self.maybe_view.negate()
    }
}

impl Tri {
    pub fn negate(self) -> Tri {
        match self {
            Tri::Yes => Tri::No,
            Tri::No => Tri::Yes,
            Tri::Unknown => Tri::Unknown,
        }
    }
}

// ---------------------------------------------------------------- layout

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FieldLayout {
    pub name: String,
    pub offset: u64,
    pub size: u64,
    pub align: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LayoutKind {
    Scalar,
    /// A struct, a tuple or an array: fields at offsets.
    Aggregate,
    /// A tag followed by the payload area.
    Enum {
        tag_size: u64,
        payload_offset: u64,
    },
    /// `?&T` and the like: the null pointer is `.none`, no tag.
    Niche,
}

/// Size, alignment and field offsets of a type without generic parameters (section 25).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Layout {
    pub size: u64,
    pub align: u64,
    pub kind: LayoutKind,
    pub fields: Vec<FieldLayout>,
}

impl Layout {
    pub fn scalar(size: u64, align: u64) -> Layout {
        Layout {
            size,
            align,
            kind: LayoutKind::Scalar,
            fields: Vec::new(),
        }
    }

    /// Bytes inside the type that belong to no field (struct layout only).
    pub fn padding(&self) -> u64 {
        if self.kind != LayoutKind::Aggregate {
            return 0;
        }
        self.size - self.fields.iter().map(|f| f.size).sum::<u64>()
    }
}

// ----------------------------------------------------------- impl tables

/// The outermost constructor of a type, the key of the per-type tables.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Head {
    Int(IntTy),
    Float(u32),
    Bool,
    Void,
    Never,
    Array,
    Tuple(usize),
    Adt(DefId),
    Optional,
    Result,
    Ref(bool),
    Slice(bool),
    RawPtr(bool),
    FnPtr,
    Dyn(DefId),
    DynFn,
    Vector,
    Mask,
    /// A generic parameter, a projection or poison: the impl applies to many types.
    Other,
}

#[derive(Clone, Debug, Default)]
pub struct ImplTables {
    /// All impls in declaration order (poisoned ones excluded).
    pub all: Vec<DefId>,
    /// Inherent impls by the head of their type.
    pub inherent: BTreeMap<Head, Vec<DefId>>,
    /// Interface impls by the interface (user-declared or built in).
    pub by_interface: BTreeMap<DefId, Vec<DefId>>,
    /// Interface impls by the head of their type.
    pub by_head: BTreeMap<Head, Vec<DefId>>,
    /// `impl Send/Sync` of each property by the head of their type.
    pub props: BTreeMap<(PropBound, Head), Vec<DefId>>,
}

// ---------------------------------------------------------- obligations

/// A check postponed until every signature and every property is known.
#[derive(Clone, Debug)]
pub enum ObligationKind {
    /// The generic arguments of a use of `item` (a struct, enum, alias or interface)
    /// must satisfy its bounds (E0801).
    Args {
        item: DefId,
        args: Vec<GenericArg>,
        spans: Vec<Span>,
    },
    /// `&dyn I` must be allowed (E0510).
    Dyn { interface: DefId },
}

#[derive(Clone, Debug)]
pub struct Obligation {
    pub file: usize,
    pub span: Span,
    pub kind: ObligationKind,
}

/// A concrete type written in a signature or a body. E0521 is reported when an
/// instantiated length is out of range or the type is above 2^62 bytes.
#[derive(Clone, Copy, Debug)]
pub struct ConcreteType {
    pub file: usize,
    pub span: Span,
    /// The item whose signature or body wrote the type; a per-instance check runs
    /// only for that owner's instances.
    pub owner: Option<DefId>,
    /// The argument that became an invalid array length, when there is one.
    pub length_span: Option<Span>,
    pub ty: TypeId,
}

// ----------------------------------------------------------- the result

/// The definitions the builtin names of section 4.4 and 27.2 stand for. `Ordering`,
/// `Eq`, `Ord`, `Hash`, `Iter` and `Format` are declarations (`builtin.ingot`);
/// `Executor` has none. A field is `None` only when the compilation has no `std`.
#[derive(Clone, Copy, Debug, Default)]
pub struct BuiltinNames {
    pub ordering: Option<DefId>,
    pub eq: Option<DefId>,
    pub ord: Option<DefId>,
    pub hash: Option<DefId>,
    pub iter: Option<DefId>,
    pub format: Option<DefId>,
    pub executor: Option<DefId>,
    /// `Idx`, the 1-D kernel index type.
    pub idx: Option<DefId>,
    /// `std.sync.Cell`, the one language item of interior mutability (02 §13.6).
    pub cell: Option<DefId>,
}

/// Everything phase 6 knows. Phase 7 starts from this.
pub struct Declarations {
    pub types: TypeTable,
    /// The signature of every item, member and generic owner, keyed by definition.
    pub items: BTreeMap<DefId, ItemSig>,
    /// What `Self` is: a type for an impl, the parameter `Self` for an interface.
    pub self_types: BTreeMap<DefId, TypeId>,
    /// The generic parameters of every item that has some.
    pub generics: BTreeMap<DefId, Vec<GenericSig>>,
    /// The bounds of every generic parameter.
    pub param_bounds: BTreeMap<DefId, Vec<Bound>>,
    /// The type of every constant generic parameter.
    pub param_const_ty: BTreeMap<DefId, TypeId>,
    pub impls: ImplTables,
    /// The state of every constant whose value was asked for.
    pub consts: BTreeMap<DefId, ConstState>,
    /// The richer value tree of every `const` and `static` that the interpreter
    /// evaluated (pipeline section 4.6).
    pub values: BTreeMap<DefId, ValueState>,
    /// The bodies phase 7 typed early, because a constant that phase 6 evaluated
    /// reached them. Phase 7 reuses them instead of typing them twice.
    pub bodies: BTreeMap<DefId, TypedBody>,
    /// Constant expressions of signatures (array lengths, tag values, alignments)
    /// typed on demand, keyed by the item and the expression.
    pub expr_bodies: BTreeMap<(DefId, ExprId), TypedBody>,
    pub props: BTreeMap<TypeId, Props>,
    pub layouts: BTreeMap<TypeId, Layout>,
    pub obligations: Vec<Obligation>,
    /// Concrete types that need the E0521 length and size check (01-core §3).
    pub concrete_types: Vec<ConcreteType>,
    /// Types that still mention a generic parameter (or an expression over one), for
    /// the per-instance E0521 and E0515 checks of phase 11a.
    pub generic_types: Vec<ConcreteType>,
    /// For each associated type of an interface: the interface and the name.
    pub assoc_info: BTreeMap<DefId, (DefId, String)>,
    /// The definitions behind the builtin names `Eq`, `Ord`, ... (section 4.4).
    pub builtins: BuiltinNames,
    /// Structs and enums that contain themselves by value (E0516).
    pub infinite: BTreeSet<DefId>,
    pub diagnostics: Vec<Diagnostic>,
    pub not_implemented: Vec<String>,
    /// Types the property computation gave up on (unbounded polymorphic recursion).
    pub gave_up: bool,
    /// For each `waitable fn` whose body was checked: the `Wait` type every `return`
    /// gives (03-concurrency §21).
    pub wait_types: BTreeMap<DefId, TypeId>,
}

impl Declarations {
    pub fn new() -> Declarations {
        Declarations {
            types: TypeTable::new(),
            items: BTreeMap::new(),
            self_types: BTreeMap::new(),
            generics: BTreeMap::new(),
            param_bounds: BTreeMap::new(),
            param_const_ty: BTreeMap::new(),
            impls: ImplTables::default(),
            consts: BTreeMap::new(),
            values: BTreeMap::new(),
            bodies: BTreeMap::new(),
            expr_bodies: BTreeMap::new(),
            props: BTreeMap::new(),
            layouts: BTreeMap::new(),
            obligations: Vec::new(),
            concrete_types: Vec::new(),
            generic_types: Vec::new(),
            assoc_info: BTreeMap::new(),
            builtins: BuiltinNames::default(),
            infinite: BTreeSet::new(),
            diagnostics: Vec::new(),
            not_implemented: Vec::new(),
            gave_up: false,
            wait_types: BTreeMap::new(),
        }
    }

    pub fn generics_of(&self, def: DefId) -> &[GenericSig] {
        self.generics.get(&def).map(Vec::as_slice).unwrap_or(&[])
    }

    /// The interface an impl implements, when it implements a user-declared one.
    pub fn impl_interface(&self, impl_def: DefId) -> Option<DefId> {
        match &self.impl_sig(impl_def)?.of {
            ImplOf::Interface(i, _) => Some(*i),
            _ => None,
        }
    }

    pub fn item(&self, def: DefId) -> Option<&ItemSig> {
        self.items.get(&def)
    }

    pub fn fn_sig(&self, def: DefId) -> Option<&FnSig> {
        match self.items.get(&def) {
            Some(ItemSig::Fn(f)) => Some(f),
            _ => None,
        }
    }

    pub fn struct_sig(&self, def: DefId) -> Option<&StructSig> {
        match self.items.get(&def) {
            Some(ItemSig::Struct(f)) => Some(f),
            _ => None,
        }
    }

    pub fn enum_sig(&self, def: DefId) -> Option<&EnumSig> {
        match self.items.get(&def) {
            Some(ItemSig::Enum(f)) => Some(f),
            _ => None,
        }
    }

    pub fn interface_sig(&self, def: DefId) -> Option<&InterfaceSig> {
        match self.items.get(&def) {
            Some(ItemSig::Interface(f)) => Some(f),
            _ => None,
        }
    }

    pub fn impl_sig(&self, def: DefId) -> Option<&ImplSig> {
        match self.items.get(&def) {
            Some(ItemSig::Impl(f)) => Some(f),
            _ => None,
        }
    }

    /// The substitution that instantiates the generics of `owner` with `args`.
    pub fn subst_for(&self, owner: DefId, args: &[GenericArg]) -> Subst {
        let mut s = Subst::new();
        for (g, a) in self.generics_of(owner).iter().zip(args) {
            s.insert(g.def, a.clone());
        }
        s
    }
}

impl Default for Declarations {
    fn default() -> Self {
        Declarations::new()
    }
}
