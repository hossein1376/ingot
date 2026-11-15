//! The data model of a control-flow graph (pipeline phase 8), the input of phases 9
//! and 10.
//!
//! A [`Cfg`] belongs to one body: a function or test, a closure, or the body of a
//! `par for`. Everything later phases need is here; they never go back to the HIR.
//! `crates/cfg/src/dump.rs` prints a CFG as text and describes the format.

use ingot_diag::source::Span;
use ingot_sema::defs::DefId;
use ingot_sema::hir::{ExprId, LocalId, StmtId};
use ingot_sema::ty::{Mutability, TypeId};
use ingot_sema::typed::{CallMode, DynCoercion, FnDynCoercion, FnRef};
use ingot_syntax::ast::{BinOp, UnOp};

/// The index of a local in its CFG.
pub type Local = u32;
/// The index of a block in its CFG.
pub type Block = u32;

/// Which body of an item a CFG is: the item's own body, a closure's, or a `par for`
/// body's.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum BodyKey {
    Item,
    Closure(ExprId),
    ParFor(StmtId),
}

/// How a CFG is identified: its owner plus which body of it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CfgKey {
    pub owner: DefId,
    pub body: BodyKey,
}

/// The identity of a CFG in a [`Cfgs`] set.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CfgId(pub u32);

/// One control-flow graph.
#[derive(Clone, Debug)]
pub struct Cfg {
    pub key: CfgKey,
    /// The qualified name of the owner, for the dump: `app.main`.
    pub name: String,
    /// The source file of the body (an index into the compilation's sources).
    pub file: usize,
    /// The result type of a successful return.
    pub ret: TypeId,
    pub ret_name: String,
    /// The error type of a fallible function: the type of `Return { is_error: true }`.
    pub error: Option<TypeId>,
    pub error_name: Option<String>,
    /// The indices of the parameters, in order.
    pub params: Vec<Local>,
    /// Every local, by index; `locals[i]` is `%i`.
    pub locals: Vec<CfgLocal>,
    pub blocks: Vec<BlockData>,
    pub entry: Block,
    /// Set on a closure body: the capture record `C`. `None` on every other CFG.
    /// Analysis A reads the captures as fields of this type (checker.md §4.6).
    pub closure_ty: Option<TypeId>,
    /// The captures of `closure_ty`, in field order. Empty on every other CFG.
    pub closure_fields: Vec<ClosureField>,
}

/// One capture of a closure, stored as a field of the capture record.
#[derive(Clone, Debug)]
pub struct ClosureField {
    pub ty: TypeId,
    pub span: Span,
    pub name: String,
}

impl Cfg {
    /// The structural problems of this CFG (empty when it is valid). See
    /// [`crate::validate`].
    pub fn validate(&self) -> Vec<String> {
        crate::validate::validate(self)
    }

    pub fn local(&self, id: Local) -> &CfgLocal {
        &self.locals[id as usize]
    }

    pub fn block(&self, id: Block) -> &BlockData {
        &self.blocks[id as usize]
    }
}

/// What a local is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LocalKind {
    /// A parameter of the body.
    Param,
    /// A `let`/`var`, a pattern binding, a `for` variable or a `catch` binding.
    User,
    /// A compiler-made temporary.
    Temp,
    /// The `&Scope` of `scope s`.
    Scope,
}

#[derive(Clone, Debug)]
pub struct CfgLocal {
    pub kind: LocalKind,
    /// The source name, for user locals.
    pub name: Option<String>,
    /// The HIR local this stands for, when there is one.
    pub source: Option<LocalId>,
    pub ty: TypeId,
    pub ty_name: String,
    /// The binding's name, or the expression that made a temporary.
    pub span: Span,
    /// The `let` statement that declared this local, when lowering has one.
    /// The E1003 edit inserts after this span. Equals `span` otherwise.
    pub decl_span: Span,
}

#[derive(Clone, Debug)]
pub struct BlockData {
    pub stmts: Vec<Stmt>,
    pub term: Terminator,
    /// The source span of the construct the terminator came from.
    pub term_span: Span,
}

#[derive(Clone, Debug)]
pub struct Stmt {
    pub span: Span,
    pub kind: StmtKind,
    /// Set on a statement copied from a deferred block: which `defer` it came from and
    /// which exit of the block triggered the copy.
    pub defer: Option<DeferCopy>,
}

/// Where a copied deferred statement came from, and why it runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DeferCopy {
    /// The span of the `defer`/`errdefer` statement.
    pub defer_span: Span,
    /// The span of the exit (`return`, `fail`, `break`, `continue`, a `try` that
    /// propagates, or the end of the block).
    pub exit_span: Span,
}

#[derive(Clone, Debug)]
pub enum StmtKind {
    /// The local's storage begins here.
    StorageStart(Local),
    /// The local's storage ends here.
    StorageEnd(Local),
    Assign {
        place: Place,
        value: Rvalue,
    },
    /// The entry of an `unsafe` block or expression. A no-op for phases 9 and 10;
    /// constant evaluation reports E0807 when it executes one (pipeline section 4.6).
    EnterUnsafe,
    /// Sets the cancel flag of `s` (a `scope` local): a use of `s`.
    Cancel(Local),
    /// A local started for a `select` arm's operation that was not chosen: its
    /// storage held no value and none is lost. The checker treats it as moved.
    Abandon(Local),
    Check(Check),
    /// A read with no other effect, kept so that liveness sees it (`_ = x`, and the
    /// discarded value of an expression statement).
    Use(Operand),
    /// A tag test's result (pipeline.md §5): `place` holds variant `variant` (`holds`),
    /// or does not (`!holds`). Emitted after every tag test of `match`, `if let`,
    /// `while let`, `try`, `catch`, `orelse` and a `for` over an iterator. `name` is
    /// the variant's source name, for the dump.
    Variant {
        place: Place,
        variant: u32,
        name: String,
        holds: bool,
    },
    /// The normal exit of a consuming `for` over an array value. Every element was
    /// moved (`PlaceUse::ForElem`), so this consumes the array temporary. A `break`
    /// jumps past it; the elements still in the array stay an obligation.
    IterConsumed(Place),
}

/// A place: a local or a `static`, followed by projections.
#[derive(Clone, Debug)]
pub struct Place {
    pub base: PlaceBase,
    pub proj: Vec<Projection>,
}

impl Place {
    pub fn local(local: Local) -> Place {
        Place {
            base: PlaceBase::Local(local),
            proj: Vec::new(),
        }
    }

    pub fn root(&self) -> Option<Local> {
        match self.base {
            PlaceBase::Local(l) => Some(l),
            PlaceBase::Static(_) => None,
        }
    }

    pub fn field(mut self, index: usize, name: Option<String>) -> Place {
        self.proj.push(Projection::Field { index, name });
        self
    }

    pub fn tuple_field(mut self, index: usize) -> Place {
        self.proj.push(Projection::TupleField(index));
        self
    }

    pub fn index(mut self, operand: Operand) -> Place {
        self.proj.push(Projection::Index(operand));
        self
    }

    pub fn deref(mut self) -> Place {
        self.proj.push(Projection::Deref);
        self
    }

    pub fn deref_raw(mut self) -> Place {
        self.proj.push(Projection::DerefRaw);
        self
    }

    pub fn payload(mut self, variant: usize) -> Place {
        self.proj.push(Projection::Payload { variant });
        self
    }
}

#[derive(Clone, Debug)]
pub enum PlaceBase {
    Local(Local),
    Static(DefId),
}

#[derive(Clone, Debug)]
pub enum Projection {
    /// A struct field: its index in the declaration and, for the dump, its name.
    Field { index: usize, name: Option<String> },
    /// A tuple field.
    TupleField(usize),
    /// An array or slice element. The checker ignores the operand; check removal and
    /// code generation need it.
    Index(Operand),
    /// `p.*`: dereference of a borrow.
    Deref,
    /// `p.*` on a raw pointer: a place outside the borrow model.
    DerefRaw,
    /// The payload of enum variant `variant` (its declaration index).
    Payload { variant: usize },
}

/// A value read from a place: a copy, a move, or the element step of a consuming
/// `for`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlaceUse {
    Copy,
    Move,
    /// `for t in array`. Not a tracked move and not E1002: the loop's normal exit
    /// carries [`StmtKind::IterConsumed`], which consumes the array. Evaluation still
    /// moves the element.
    ForElem,
}

#[derive(Clone, Debug)]
pub enum Operand {
    Place(Place, PlaceUse),
    Const(Const),
}

#[derive(Clone, Debug)]
pub enum Const {
    Int {
        ty: TypeId,
        value: i128,
    },
    Float {
        ty: TypeId,
        value: f64,
    },
    Bool(bool),
    Unit,
    /// A string literal: a `[]u8` that lives for the whole program.
    Str(Vec<u8>),
    /// A C string literal: a `*u8` that lives for the whole program.
    CStr(Vec<u8>),
    /// `@zero(T)`.
    Zero {
        ty: TypeId,
        name: String,
    },
    /// `@uninit(T)`.
    Uninit {
        ty: TypeId,
        name: String,
    },
    /// A constant generic parameter used as a value.
    Param {
        def: DefId,
        name: String,
    },
    /// A `const` item: its value is evaluated from its typed body (S5b) or folded.
    Named {
        def: DefId,
        name: String,
    },
    /// A function used as a `fn` value.
    Fn {
        fn_ref: Box<FnRef>,
        name: String,
    },
}

#[derive(Clone, Debug)]
pub enum Rvalue {
    Use(Operand),
    /// A unary operation; `-` traps outside the range (with a check before it).
    Unary {
        op: UnOp,
        operand: Operand,
    },
    Binary {
        op: BinOp,
        lhs: Operand,
        rhs: Operand,
    },
    /// `&p` or `&mut p`. `two_phase` is set for a borrow written directly as a call,
    /// `spawn`, `dispatch` or `select` argument or as a receiver: it becomes active at
    /// that terminator.
    Borrow {
        place: Place,
        mutability: Mutability,
        two_phase: bool,
    },
    /// The implicit reborrow of a place of type `&mut T` or `[]mut T` used as an
    /// argument, receiver, `spawn`/`dispatch` argument or `for` iterand: `&mut p.*`,
    /// with the place's own type as the result.
    Reborrow {
        place: Place,
        two_phase: bool,
    },
    Aggregate {
        kind: AggregateKind,
        ty: TypeId,
        ty_name: String,
        fields: Vec<Operand>,
    },
    Cast {
        kind: CastKind,
        operand: Operand,
    },
    /// `&T`/`&mut T` to `&dyn I`/`&mut dyn I`, or `&closure` to `&dyn Fn(..)`.
    DynCast {
        operand: Operand,
        coercion: DynCoercionKind,
    },
    /// The tag of an enum value, for a `switch`.
    Discriminant(Place),
    /// `place.len` of a slice.
    SliceLen(Place),
    /// `place.ptr` of a slice (unsafe).
    SlicePtr(Place),
    /// `s[lo..hi]` on a slice (`borrow: None`), or `&a[lo..hi]`/`&mut a[lo..hi]` on an
    /// array (`borrow: Some(m)`): for the checker, the latter is a loan on `base`.
    Slice {
        base: Place,
        lo: Operand,
        hi: Operand,
        borrow: Option<Mutability>,
        /// An exclusive borrow of an array written directly as a call argument: it
        /// becomes active at that terminator.
        two_phase: bool,
    },
    /// One element of a `[]mut T` in a `for` loop: a `&mut T` that holds the loop's
    /// single reborrow and takes no loan of its own.
    SliceElem {
        base: Place,
        index: Operand,
    },
    /// A value builtin: `@splat`, `@sqrt`, `@select`, `@narrow`, ...
    Builtin {
        name: String,
        type_arg: Option<TypeId>,
        type_arg_name: Option<String>,
        args: Vec<BuiltinArg>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AggregateKind {
    Struct(DefId),
    Tuple,
    Array,
    /// An enum variant: `Option`, `Result` or a declared enum.
    Variant {
        index: usize,
        enum_ty: TypeId,
    },
    /// The captures of a closure, in capture-list order.
    Closure(ExprId),
}

/// A `&T` to `&dyn I`, or a borrow of a closure to `&dyn Fn(..)`.
#[derive(Clone, Debug)]
pub enum DynCoercionKind {
    Interface(Box<DynCoercion>),
    Fn(Box<FnDynCoercion>),
}

/// Re-exported so that later phases name the same kinds phase 7 recorded.
pub use ingot_sema::typed::CastKind;

/// A builtin argument: most are values, but some builtins take a place (they store
/// through it or take its length).
#[derive(Clone, Debug)]
pub enum BuiltinArg {
    Value(Operand),
    Place(Place),
}

/// One explicit check statement: what can trap, its operands, and what phase 10 reads.
#[derive(Clone, Debug)]
pub struct Check {
    pub kind: CheckKind,
    /// The container whose length the check is against (`a` in `a[i]`, `s` in
    /// `@load(V, s, i)`, the output in a `dispatch` check).
    pub place: Option<Place>,
    /// The lower operand: the index, the slice start, or the dispatch start.
    pub lo: Option<Operand>,
    /// The upper operand of a slice or a dispatch range.
    pub hi: Option<Operand>,
    /// The other operands (the two sides of an arithmetic operation, the index vector
    /// of a gather, the buffers of a dispatch).
    pub operands: Vec<Operand>,
    /// Further containers whose lengths matter (the buffers of a `dispatch`).
    pub places: Vec<Place>,
    /// The type the check's arithmetic is on, for phase 10's range facts.
    pub ty: Option<TypeId>,
    /// That type as text, for the dump.
    pub ty_name: Option<String>,
    /// The operator of an `overflow` check (`+`, `-`, `*` or `-` unary), for the trap
    /// text of E0806.
    pub op: Option<String>,
    /// The source span the check is reported at.
    pub span: Span,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CheckKind {
    Bounds(BoundsKind),
    Overflow,
    Division,
    Shift,
    Narrow,
    Gather,
    ScatterDistinct,
    DispatchCover,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BoundsKind {
    /// T01: an index into an array or slice.
    Index,
    /// T02: a slice range.
    Slice,
    /// T14: a vector access of `width` lanes. `tail`: `@load_tail`/`@store_tail`, where
    /// `i` may equal the length.
    Vector { width: u32, tail: bool },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Trap {
    T01,
    T02,
    T03,
    T04,
    T05,
    T06,
    T07,
    T08,
    T09,
    T10,
    T14,
    T15,
    T16,
    T17,
}

impl Trap {
    pub fn code(self) -> &'static str {
        match self {
            Trap::T01 => "T01",
            Trap::T02 => "T02",
            Trap::T03 => "T03",
            Trap::T04 => "T04",
            Trap::T05 => "T05",
            Trap::T06 => "T06",
            Trap::T07 => "T07",
            Trap::T08 => "T08",
            Trap::T09 => "T09",
            Trap::T10 => "T10",
            Trap::T14 => "T14",
            Trap::T15 => "T15",
            Trap::T16 => "T16",
            Trap::T17 => "T17",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Trap::T01 => "index_out_of_bounds",
            Trap::T02 => "slice_out_of_bounds",
            Trap::T03 => "integer_overflow",
            Trap::T04 => "division_by_zero",
            Trap::T05 => "division_overflow",
            Trap::T06 => "shift_out_of_range",
            Trap::T07 => "narrow_out_of_range",
            Trap::T08 => "assertion_failed",
            Trap::T09 => "panic",
            Trap::T10 => "unreachable",
            Trap::T14 => "vector_access_out_of_bounds",
            Trap::T15 => "scatter_index_repeated",
            Trap::T16 => "dispatch_out_of_bounds",
            Trap::T17 => "not_on_a_task",
        }
    }
}

#[derive(Clone, Debug)]
pub enum Terminator {
    Goto(Block),
    Branch {
        cond: Operand,
        then_block: Block,
        else_block: Block,
    },
    /// A switch on an enum tag: `cases` are tag values, `default` catches the rest.
    Switch {
        value: Operand,
        cases: Vec<(i128, Block)>,
        default: Block,
    },
    Return {
        value: Option<Operand>,
        /// `fail e`, or a `try` that propagates: this return runs `errdefer` code.
        is_error: bool,
        /// The temporary that holds the return value: its storage ends at this
        /// terminator, after the value is read. No `StorageEnd` statement precedes it,
        /// because the value must survive the deferred code and every scope wait.
        end: Option<Local>,
    },
    Call {
        target: CallTarget,
        args: Vec<Operand>,
        dest: Option<Place>,
        next: Block,
    },
    /// `@assert`, `@panic` or `@unreachable`: always traps. `assert` reaches this
    /// through a branch.
    Trap {
        code: Trap,
        operands: Vec<Operand>,
        /// The source text of an assertion's condition, for T08's trap text.
        source: Option<String>,
    },
    Spawn {
        target: CallTarget,
        args: Vec<Operand>,
        scope: Operand,
        dest: Option<Place>,
        next: Block,
    },
    ScopeWait {
        scope: Local,
        next: Block,
    },
    /// Wait for one of several operations; each successor is one arm, bound to the
    /// operation's result through `SelectOp::dest`.
    Select {
        ops: Vec<SelectOp>,
    },
    ParFor {
        body: CfgId,
        /// The body's qualified dump name.
        body_name: String,
        /// The captured outer locals, each as a shared borrow, in field order.
        captures: Vec<Operand>,
        /// The outer places the terminator shared-borrows, parallel to `captures`.
        capture_places: Vec<Place>,
        /// The names of the captures, parallel to `captures`.
        capture_names: Vec<String>,
        iterand: ParIterand,
        grain: Option<Operand>,
        next: Block,
    },
    Dispatch {
        kernel: DefId,
        kernel_name: String,
        /// The kernel's generic arguments, in order.
        kernel_args: Vec<ingot_sema::ty::GenericArg>,
        /// The same arguments as text, for the dump.
        kernel_args_name: String,
        executor: Operand,
        lo: Operand,
        hi: Operand,
        /// Every input and output, in source order. `borrow` says how the terminator
        /// borrows it (`[]T` shared, `[]mut T` exclusive).
        args: Vec<DispatchArg>,
        dest: Option<Place>,
        next: Block,
    },
    /// No successor: control cannot reach here.
    Unreachable,
}

#[derive(Clone, Debug)]
pub struct DispatchArg {
    pub value: Operand,
    pub borrow: Option<Mutability>,
    /// The place the terminator borrows (the argument's source place, or the
    /// temporary that holds it).
    pub place: Option<Place>,
}

/// What a `par for` runs over: a value (a slice or a `par.Split`), or a range.
#[derive(Clone, Debug)]
pub enum ParIterand {
    Value(Operand),
    Range {
        lo: Operand,
        hi: Operand,
        inclusive: bool,
        step: Option<Operand>,
    },
}

#[derive(Clone, Debug)]
pub struct SelectOp {
    /// The waitable call of the arm; `None` for `else`.
    pub call: Option<SelectCall>,
    pub successor: Block,
    pub is_else: bool,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct SelectCall {
    pub target: CallTarget,
    pub args: Vec<Operand>,
    /// Where the chosen operation writes its result; the arm's pattern binds from it.
    pub dest: Place,
}

#[derive(Clone, Debug)]
pub enum CallTarget {
    /// A function known at compile time.
    Fn { fn_ref: Box<FnRef>, name: String },
    /// A call of a `fn` pointer, a closure, a type parameter with an `Fn` bound, or a
    /// `&dyn Fn(..)`.
    Value {
        callee: Operand,
        kind: ValueKind,
        mode: Option<CallMode>,
    },
}

#[derive(Clone, Debug)]
pub enum ValueKind {
    FnPtr,
    Closure(ExprId),
    /// A call of a value of a type parameter, resolved at instantiation.
    Bound,
    DynFn,
}

impl CallTarget {
    pub fn name(&self) -> String {
        match self {
            CallTarget::Fn { name, .. } => name.clone(),
            CallTarget::Value { .. } => String::from("<value>"),
        }
    }
}

/// The result of phase 8 for one package.
#[derive(Clone, Debug, Default)]
pub struct Cfgs {
    entries: Vec<Option<Cfg>>,
}

impl Cfgs {
    /// Reserve the slot of a body before it is lowered, so that a body that refers to
    /// it (a closure capture, a `par for`) can name its id. Filled by [`Cfgs::fill`]
    /// or dropped by [`Cfgs::drop_at`].
    #[allow(clippy::too_many_arguments)]
    pub fn reserve(
        &mut self,
        key: CfgKey,
        name: String,
        ret: TypeId,
        ret_name: String,
        error: Option<TypeId>,
        error_name: Option<String>,
        file: usize,
    ) -> CfgId {
        let id = CfgId(self.entries.len() as u32);
        self.entries.push(Some(Cfg {
            key,
            name,
            file,
            ret,
            ret_name,
            error,
            error_name,
            params: Vec::new(),
            locals: Vec::new(),
            blocks: Vec::new(),
            entry: 0,
            closure_ty: None,
            closure_fields: Vec::new(),
        }));
        id
    }

    pub fn fill(&mut self, id: CfgId, cfg: Cfg) {
        self.entries[id.0 as usize] = Some(cfg);
    }

    /// Drop a body the lowering could not finish.
    pub fn drop_at(&mut self, id: CfgId) {
        self.entries[id.0 as usize] = None;
    }

    /// The reserved but not yet lowered header of a body.
    pub fn get(&self, id: CfgId) -> &Cfg {
        self.entries[id.0 as usize]
            .as_ref()
            .expect("a reserved CFG is present until it is dropped")
    }

    pub fn has(&self, id: CfgId) -> bool {
        self.entries.get(id.0 as usize).is_some_and(|e| e.is_some())
    }

    pub fn iter(&self) -> impl Iterator<Item = (CfgId, &Cfg)> {
        self.entries
            .iter()
            .enumerate()
            .filter_map(|(i, c)| c.as_ref().map(|c| (CfgId(i as u32), c)))
    }

    pub fn is_empty(&self) -> bool {
        self.iter().next().is_none()
    }

    pub fn len(&self) -> usize {
        self.entries.iter().filter(|e| e.is_some()).count()
    }

    /// Every CFG, in id order (the interpreter's own lowering result).
    pub fn into_vec(self) -> Vec<Cfg> {
        self.entries.into_iter().flatten().collect()
    }
}

/// The trap a check guards (the `bounds` kind has three).
pub fn check_trap(kind: CheckKind) -> Trap {
    match kind {
        CheckKind::Bounds(BoundsKind::Index) => Trap::T01,
        CheckKind::Bounds(BoundsKind::Slice) => Trap::T02,
        CheckKind::Bounds(BoundsKind::Vector { tail: true, .. }) => Trap::T14,
        CheckKind::Bounds(BoundsKind::Vector { tail: false, .. }) => Trap::T14,
        CheckKind::Overflow => Trap::T03,
        CheckKind::Division => Trap::T04,
        CheckKind::Shift => Trap::T06,
        CheckKind::Narrow => Trap::T07,
        CheckKind::Gather => Trap::T14,
        CheckKind::ScatterDistinct => Trap::T15,
        CheckKind::DispatchCover => Trap::T16,
    }
}
