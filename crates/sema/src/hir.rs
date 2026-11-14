//! The resolved HIR: the syntax of one package with every value and type path
//! bound to a definition, a local or poison, and with every `when` already decided.
//!
//! One `Package` holds the arenas for all of its bodies. A node id is an index
//! into the arena of the package that owns it, and every node keeps its span.
//!
//! What stays a name here, because only types can resolve it (pipeline section
//! 4.3): field names, the variant in `.name`, method names, members reached through
//! a value, and associated type names.

use crate::defs::{DefId, ModuleId};
use ingot_diag::source::Span;
use ingot_syntax::ast::{AssignOp, BinOp, FnMods, StructKind, UnOp};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ExprId(pub u32);

impl ExprId {
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StmtId(pub u32);

impl StmtId {
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BlockId(pub u32);

impl BlockId {
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PatId(pub u32);

impl PatId {
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LocalId(pub u32);

impl LocalId {
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

/// What a name resolved to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Res {
    Def(DefId),
    Local(LocalId),
    /// The name failed to resolve (and was reported, or came from a poisoned
    /// declaration). Nothing downstream reports about it.
    Poison,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LocalKind {
    Param,
    /// A `let` or `var` binding or a binding inside a pattern.
    Let,
    /// The variable of a `for` loop, or its index.
    For,
    /// The error name of `catch e =>`.
    Catch,
    /// A closure's capture (the name inside the closure).
    Capture,
    /// The name introduced by `scope s { ... }`.
    Scope,
}

#[derive(Clone, Debug)]
pub struct Local {
    pub name: String,
    pub span: Span,
    pub kind: LocalKind,
    /// `var` (and nothing else) is mutable by declaration.
    pub mutable: bool,
    /// Whether the name is read anywhere. A plain `x = ...` is not a read.
    pub read: bool,
}

// ------------------------------------------------------------------ types

#[derive(Clone, Debug)]
pub struct Ty {
    pub kind: TyKind,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub enum TyKind {
    /// A named type: a declared type, a generic parameter, `Self`, or a builtin
    /// (scalar and vector types are builtin definitions too).
    Path(PathRef),
    Borrow {
        mutable: bool,
        inner: Box<Ty>,
    },
    Slice {
        mutable: bool,
        elem: Box<Ty>,
    },
    Array {
        len: ExprId,
        elem: Box<Ty>,
    },
    Ptr {
        mutable: bool,
        inner: Box<Ty>,
    },
    Optional(Box<Ty>),
    Tuple(Vec<Ty>),
    Fn {
        ext: bool,
        params: Vec<Ty>,
        ret: Box<Ty>,
        error: Option<Box<Ty>>,
    },
    Dyn {
        interface: PathRef,
        mutable: bool,
    },
    Poison,
}

/// A resolved path such as `lib.Ticket`, `List[u8, Heap]`, `T.Item` or the bound
/// `Fn(A) -> R`.
#[derive(Clone, Debug)]
pub struct PathRef {
    pub res: Res,
    /// The segments as written (module prefix, then the name).
    pub segments: Vec<(String, Span)>,
    pub args: Vec<GenericArg>,
    /// The bracketed argument list, `[...]` with the brackets, when one was written.
    pub args_span: Option<Span>,
    pub fn_sig: Option<FnSig>,
    /// The trailing `.Item` of an associated type, resolved by type later.
    pub assoc: Option<(String, Span)>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct FnSig {
    pub params: Vec<Ty>,
    pub ret: Option<Box<Ty>>,
    pub error: Option<Box<Ty>>,
    pub span: Span,
}

// A `Ty` is large and a constant argument is an index; boxing the type would only add a
// pointer to every argument list of the arena.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug)]
pub enum GenericArg {
    Ty(Ty),
    Const(ExprId),
}

// ------------------------------------------------------------ expressions

#[derive(Clone, Debug)]
pub struct Expr {
    pub kind: ExprKind,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub enum ExprKind {
    Int {
        value: u128,
        negative: bool,
        text: String,
    },
    Float(f64),
    Str(Vec<u8>),
    CStr(Vec<u8>),
    Bool(bool),
    /// A name used as a value: a local, a function, a constant, a static, or a
    /// type used as a value (`Vec3.new`, `lib.Ticket.{...}`, a const parameter).
    /// `m.name` through an imported module is a path too.
    Path(PathRef),
    /// A block used as an expression branch: it must leave before its end (E0207).
    Block(BlockId),
    Builtin {
        name: String,
        type_arg: Option<Box<Ty>>,
        args: Vec<ExprId>,
    },
    Unary {
        op: UnOp,
        expr: ExprId,
        op_span: Span,
    },
    Binary {
        op: BinOp,
        lhs: ExprId,
        rhs: ExprId,
        op_span: Span,
    },
    Call {
        callee: ExprId,
        args: Vec<ExprId>,
        args_span: Span,
    },
    Index {
        base: ExprId,
        index: ExprId,
        bracket: Span,
    },
    Slice {
        base: ExprId,
        lo: Option<ExprId>,
        hi: Option<ExprId>,
        bracket: Span,
    },
    Field {
        base: ExprId,
        name: String,
        name_span: Span,
    },
    TupleField {
        base: ExprId,
        index: u64,
    },
    Deref {
        base: ExprId,
        op_span: Span,
    },
    TypeApply {
        base: ExprId,
        args: Vec<GenericArg>,
    },
    Cast {
        expr: ExprId,
        ty: Ty,
        as_span: Span,
    },
    StructLit {
        ty: PathRef,
        fields: Vec<FieldInit>,
        braces: Span,
    },
    StructShorthand {
        fields: Vec<FieldInit>,
        braces: Span,
    },
    EnumLit {
        ty: Option<PathRef>,
        variant: String,
        variant_span: Span,
        payload: Option<ExprId>,
    },
    ArrayLit {
        len: ExprId,
        elem: Ty,
        elems: Vec<ExprId>,
    },
    Range {
        lo: ExprId,
        hi: ExprId,
        inclusive: bool,
    },
    Tuple(Vec<ExprId>),
    Paren(ExprId),
    If {
        cond: Cond,
        then: ExprId,
        otherwise: ExprId,
    },
    Match {
        scrutinee: ExprId,
        arms: Vec<Arm>,
    },
    Closure(Box<Closure>),
    Catch {
        value: ExprId,
        binding: Option<LocalId>,
        binding_span: Option<Span>,
        handler: Handler,
        catch_span: Span,
    },
    Orelse {
        value: ExprId,
        handler: Handler,
        orelse_span: Span,
    },
    Spawn {
        call: ExprId,
        scope: ExprId,
    },
    Dispatch {
        call: ExprId,
        domain: ExprId,
        executor: ExprId,
    },
    Unsafe {
        reason: String,
        reason_span: Span,
        body: ExprId,
    },
    Poison,
}

#[derive(Clone, Debug)]
pub struct FieldInit {
    pub name: String,
    pub name_span: Span,
    pub value: ExprId,
}

#[derive(Clone, Debug)]
pub struct Cond {
    pub expr: ExprId,
    pub let_pattern: Option<PatId>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub enum Handler {
    Expr(ExprId),
    Block(BlockId),
}

#[derive(Clone, Debug)]
pub struct Arm {
    pub patterns: Vec<PatId>,
    pub is_else: bool,
    pub guard: Option<ExprId>,
    pub body: ArmBody,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub enum ArmBody {
    Block(BlockId),
    Expr(ExprId),
}

#[derive(Clone, Debug)]
pub struct Capture {
    pub name: String,
    pub span: Span,
    pub shared: bool,
    pub mutable: bool,
    /// The local of the enclosing function that is captured.
    pub outer: Res,
    /// The binding the closure body sees.
    pub local: LocalId,
}

#[derive(Clone, Debug)]
pub struct Closure {
    pub captures: Vec<Capture>,
    pub params: Vec<Param>,
    pub ret: Ty,
    pub error: Option<Ty>,
    pub body: BlockId,
    pub span: Span,
}

// -------------------------------------------------------------- patterns

#[derive(Clone, Debug)]
pub struct Pat {
    pub kind: PatKind,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub enum PatKind {
    Wild,
    Bind {
        local: LocalId,
    },
    Literal(ExprId),
    Range {
        lo: PatId,
        hi: PatId,
        inclusive: bool,
    },
    /// `.name`, `.name(p)` or `Type.name(p)`. The variant is resolved by type.
    Variant {
        ty: Option<PathRef>,
        name: String,
        name_span: Span,
        payload: Option<PatId>,
    },
    Tuple(Vec<PatId>),
    Struct {
        ty: Option<PathRef>,
        fields: Vec<FieldPat>,
        /// The pattern ends in `..` (`grammar.md` `pattern`).
        rest: bool,
        rest_span: Span,
    },
    Poison,
}

#[derive(Clone, Debug)]
pub struct FieldPat {
    pub name: String,
    pub name_span: Span,
    /// For the shorthand `Point.{ x }` this is a binding of `x`.
    pub pattern: PatId,
}

// ------------------------------------------------------------ statements

#[derive(Clone, Debug)]
pub struct Stmt {
    pub kind: StmtKind,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct Block {
    pub stmts: Vec<StmtId>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct Label {
    pub name: String,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct ForVar {
    pub name: String,
    pub span: Span,
    /// `None` for `_`.
    pub local: Option<LocalId>,
}

#[derive(Clone, Debug)]
pub struct SelectArm {
    pub is_else: bool,
    pub pattern: PatId,
    pub call: Option<ExprId>,
    pub body: BlockId,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub enum StmtKind {
    Let {
        mutable: bool,
        pattern: PatId,
        ty: Option<Ty>,
        init: ExprId,
    },
    Assign {
        place: ExprId,
        op: AssignOp,
        value: ExprId,
    },
    /// `_ = e;`
    Discard(ExprId),
    Expr(ExprId),
    Return(Option<ExprId>),
    Fail(ExprId),
    Break {
        label: Option<Label>,
        /// The loop statement a label names.
        target: Option<StmtId>,
    },
    Continue {
        label: Option<Label>,
        target: Option<StmtId>,
    },
    Defer {
        err: bool,
        body: BlockId,
    },
    Block(BlockId),
    If {
        cond: Cond,
        then: BlockId,
        otherwise: Option<StmtId>,
    },
    While {
        label: Option<Label>,
        cond: Cond,
        body: BlockId,
    },
    Loop {
        label: Option<Label>,
        body: BlockId,
    },
    For {
        par: bool,
        grain: Option<ExprId>,
        label: Option<Label>,
        vars: Vec<ForVar>,
        index: Option<ForVar>,
        iter: ExprId,
        range_end: Option<(bool, ExprId)>,
        step: Option<ExprId>,
        body: BlockId,
    },
    Match {
        scrutinee: ExprId,
        arms: Vec<Arm>,
    },
    Scope {
        name: String,
        name_span: Span,
        local: LocalId,
        body: BlockId,
    },
    Spawn {
        call: ExprId,
        scope: ExprId,
    },
    Dispatch {
        call: ExprId,
        domain: ExprId,
        executor: ExprId,
    },
    Select {
        arms: Vec<SelectArm>,
    },
    Unsafe {
        reason: String,
        reason_span: Span,
        body: BlockId,
    },
    Poison,
}

// ----------------------------------------------------------------- items

#[derive(Clone, Debug)]
pub struct GenericParam {
    pub def: DefId,
    pub name: String,
    pub name_span: Span,
    pub is_const: bool,
    pub bounds: Vec<PathRef>,
    pub const_ty: Option<Ty>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct Param {
    pub name: String,
    pub name_span: Span,
    pub ty: Ty,
    /// `into s`: the name and, when it names a parameter, that parameter's index.
    pub into: Option<(String, Span, Option<usize>)>,
    pub local: LocalId,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct Origin {
    pub name: String,
    pub inner: bool,
    pub is_static: bool,
    pub span: Span,
    /// The index of the parameter the origin names, when there is one.
    pub param: Option<usize>,
}

#[derive(Clone, Debug)]
pub struct FnItem {
    pub def: DefId,
    pub name: String,
    pub name_span: Span,
    pub mods: FnMods,
    pub generics: Vec<GenericParam>,
    pub params: Vec<Param>,
    pub ret: Ty,
    pub error: Option<Ty>,
    pub from: Vec<Origin>,
    pub reduce: Option<String>,
    /// The span of the `reduce(op)` clause.
    pub reduce_span: Option<Span>,
    pub variadic: bool,
    /// Extern functions: the ABI string of the block they are declared in.
    pub abi: Option<String>,
    pub body: Option<BlockId>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct FieldItem {
    pub def: DefId,
    pub name: String,
    pub name_span: Span,
    pub public: bool,
    pub ty: Ty,
    pub align: Option<ExprId>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct StructItem {
    pub def: DefId,
    pub kind: StructKind,
    pub generics: Vec<GenericParam>,
    pub fields: Vec<FieldItem>,
    pub align: Option<ExprId>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct VariantItem {
    pub def: DefId,
    pub name: String,
    pub name_span: Span,
    pub payload: Option<Ty>,
    pub from: bool,
    pub value: Option<ExprId>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct EnumItem {
    pub def: DefId,
    pub copy: bool,
    pub tag: Option<Ty>,
    pub generics: Vec<GenericParam>,
    pub variants: Vec<VariantItem>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct InterfaceItem {
    pub def: DefId,
    pub generics: Vec<GenericParam>,
    pub bounds: Vec<PathRef>,
    /// The `Self` of the interface.
    pub self_def: DefId,
    /// Methods and associated types, in source order.
    pub members: Vec<DefId>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct ImplItem {
    pub def: DefId,
    pub unsafe_: bool,
    pub generics: Vec<GenericParam>,
    pub interface: Option<PathRef>,
    pub target: Ty,
    /// The `Self` of the impl.
    pub self_def: DefId,
    pub members: Vec<DefId>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct ConstItem {
    pub def: DefId,
    pub ty: Ty,
    pub value: ExprId,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct StaticItem {
    pub def: DefId,
    pub ty: Ty,
    pub align: Option<ExprId>,
    pub value: ExprId,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct AliasItem {
    pub def: DefId,
    pub generics: Vec<GenericParam>,
    /// `None` for `type Item;` in an interface.
    pub ty: Option<Ty>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct TestItem {
    pub def: DefId,
    pub name: String,
    pub bench: bool,
    pub body: BlockId,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct ExternItem {
    pub abi: String,
    pub abi_span: Span,
    pub functions: Vec<DefId>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub enum Item {
    Fn(FnItem),
    Struct(StructItem),
    Enum(EnumItem),
    Interface(InterfaceItem),
    Impl(ImplItem),
    Const(ConstItem),
    Static(StaticItem),
    Alias(AliasItem),
    Test(TestItem),
}

/// The module-level items of one module, in source order. Members of interfaces and
/// impls, and the functions of an `extern` block, are items of their own too (each
/// keyed by its definition) and are listed by their container.
#[derive(Clone, Debug)]
pub struct ModuleItems {
    pub module: ModuleId,
    pub items: Vec<DefId>,
    pub externs: Vec<ExternItem>,
}

#[derive(Clone, Debug, Default)]
pub struct Package {
    pub name: String,
    pub modules: Vec<ModuleItems>,
    pub items: std::collections::BTreeMap<DefId, Item>,
    pub exprs: Vec<Expr>,
    pub stmts: Vec<Stmt>,
    pub blocks: Vec<Block>,
    pub pats: Vec<Pat>,
    pub locals: Vec<Local>,
}

impl Package {
    pub fn expr(&self, id: ExprId) -> &Expr {
        &self.exprs[id.index()]
    }
    pub fn stmt(&self, id: StmtId) -> &Stmt {
        &self.stmts[id.index()]
    }
    pub fn block(&self, id: BlockId) -> &Block {
        &self.blocks[id.index()]
    }
    pub fn pat(&self, id: PatId) -> &Pat {
        &self.pats[id.index()]
    }
    pub fn local(&self, id: LocalId) -> &Local {
        &self.locals[id.index()]
    }

    /// True when the package declares anything that phases 6 to 10 would have to
    /// check (signatures, bodies, constants).
    pub fn has_items(&self) -> bool {
        !self.items.is_empty()
    }
}
