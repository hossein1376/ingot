//! Syntax tree. Every node keeps the source span needed for diagnostics and fixes.
//!
//! AST enums own their children by value. Box every large variant only to satisfy
//! `large_enum_variant` would make every visitor noisier; trees are built once and
//! traversed, not stored in large arrays.
#![allow(clippy::large_enum_variant)]

use crate::token::{ScalarTy, VectorTy};
use ingot_diag::source::Span;

#[derive(Clone, Debug, Default)]
pub struct Module {
    pub imports: Vec<Import>,
    pub items: Vec<Item>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct Import {
    pub segments: Vec<(String, Span)>,
    pub alias: Option<String>,
    pub alias_span: Option<Span>,
    pub span: Span,
}

impl Import {
    pub fn bound_name(&self) -> &str {
        self.alias
            .as_deref()
            .or_else(|| self.segments.last().map(|(s, _)| s.as_str()))
            .unwrap_or("")
    }

    pub fn path_text(&self) -> String {
        self.segments
            .iter()
            .map(|(s, _)| s.as_str())
            .collect::<Vec<_>>()
            .join(".")
    }
}

#[derive(Clone, Debug)]
pub enum Item {
    Fn(FnDecl),
    Struct(StructDecl),
    Enum(EnumDecl),
    Interface(InterfaceDecl),
    Impl(ImplDecl),
    Const(ConstDecl),
    Static(StaticDecl),
    TypeAlias(TypeAlias),
    Extern(ExternBlock),
    Test(TestDecl),
    When(WhenItems),
    Poison,
}

impl Item {
    pub fn span(&self) -> Span {
        match self {
            Item::Fn(x) => x.span,
            Item::Struct(x) => x.span,
            Item::Enum(x) => x.span,
            Item::Interface(x) => x.span,
            Item::Impl(x) => x.span,
            Item::Const(x) => x.span,
            Item::Static(x) => x.span,
            Item::TypeAlias(x) => x.span,
            Item::Extern(x) => x.span,
            Item::Test(x) => x.span,
            Item::When(x) => x.span,
            Item::Poison => Span::default(),
        }
    }

    pub fn name(&self) -> Option<&str> {
        match self {
            Item::Fn(x) => Some(&x.name),
            Item::Struct(x) => Some(&x.name),
            Item::Enum(x) => Some(&x.name),
            Item::Interface(x) => Some(&x.name),
            Item::Const(x) => Some(&x.name),
            Item::Static(x) => Some(&x.name),
            Item::TypeAlias(x) => Some(&x.name),
            Item::Test(x) => Some(&x.name),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct FnMods {
    pub public: bool,
    pub inline: bool,
    pub noinline: bool,
    pub unsafe_: bool,
    pub export: bool,
    pub kernel: bool,
    pub waitable: bool,
}

#[derive(Clone, Debug)]
pub struct GenericParam {
    pub name: String,
    pub name_span: Span,
    pub is_const: bool,
    pub bounds: Vec<PathTy>,
    pub const_ty: Option<Ty>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct FnDecl {
    pub name: String,
    pub name_span: Span,
    pub params: Vec<Param>,
    pub ret: Ty,
    pub error: Option<Ty>,
    pub from: Vec<Origin>,
    pub reduce: Option<String>,
    /// The span of the `reduce(op)` clause.
    pub reduce_span: Option<Span>,
    pub mods: FnMods,
    pub generics: Vec<GenericParam>,
    pub body: Option<Block>,
    /// True when an extern function's parameter list ends in `...` (grammar:
    /// `ext_params`). Only functions inside an `extern` block can be variadic.
    pub variadic: bool,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct Param {
    pub name: String,
    pub name_span: Span,
    pub ty: Ty,
    pub into: Option<(String, Span)>,
    pub span: Span,
}

impl Param {
    pub fn is_self(&self) -> bool {
        self.name == "self"
    }
}

#[derive(Clone, Debug)]
pub struct Origin {
    pub name: String,
    pub inner: bool,
    pub is_static: bool,
    pub span: Span,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StructKind {
    Plain,
    Copy,
    Linear,
}

#[derive(Clone, Debug)]
pub struct StructDecl {
    pub name: String,
    pub name_span: Span,
    pub public: bool,
    pub kind: StructKind,
    pub generics: Vec<GenericParam>,
    pub fields: Vec<FieldDecl>,
    pub align: Option<Expr>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct FieldDecl {
    pub name: String,
    pub name_span: Span,
    pub public: bool,
    pub ty: Ty,
    pub align: Option<Expr>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct EnumDecl {
    pub name: String,
    pub name_span: Span,
    pub public: bool,
    pub copy: bool,
    pub tag: Option<Ty>,
    pub generics: Vec<GenericParam>,
    pub variants: Vec<VariantDecl>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct VariantDecl {
    pub name: String,
    pub name_span: Span,
    pub payload: Option<Ty>,
    pub from: bool,
    pub value: Option<Expr>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct InterfaceDecl {
    pub name: String,
    pub name_span: Span,
    pub public: bool,
    pub generics: Vec<GenericParam>,
    pub bounds: Vec<PathTy>,
    pub members: Vec<Item>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct ImplDecl {
    pub unsafe_: bool,
    pub generics: Vec<GenericParam>,
    pub interface: Option<PathTy>,
    pub target: Ty,
    pub members: Vec<Item>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct ConstDecl {
    pub name: String,
    pub name_span: Span,
    pub public: bool,
    pub ty: Ty,
    pub value: Expr,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct StaticDecl {
    pub name: String,
    pub name_span: Span,
    pub public: bool,
    pub ty: Ty,
    pub align: Option<Expr>,
    pub value: Expr,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct TypeAlias {
    pub name: String,
    pub name_span: Span,
    pub public: bool,
    pub generics: Vec<GenericParam>,
    pub ty: Ty,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct ExternBlock {
    pub public: bool,
    pub abi: String,
    pub abi_span: Span,
    pub functions: Vec<FnDecl>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct TestDecl {
    /// True for a `bench` block, false for a `test` block.
    pub bench: bool,
    pub name: String,
    pub body: Block,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct WhenItems {
    pub condition: Expr,
    pub then: Vec<Item>,
    pub otherwise: Vec<Item>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct Block {
    pub stmts: Vec<Stmt>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub enum Stmt {
    Let {
        mutable: bool,
        pattern: Pattern,
        ty: Option<Ty>,
        init: Expr,
        span: Span,
    },
    Assign {
        place: Expr,
        op: AssignOp,
        value: Expr,
        span: Span,
    },
    Expr {
        expr: Expr,
        span: Span,
    },
    Return {
        value: Option<Expr>,
        span: Span,
    },
    Fail {
        value: Expr,
        span: Span,
    },
    Break {
        label: Option<(String, Span)>,
        span: Span,
    },
    Continue {
        label: Option<(String, Span)>,
        span: Span,
    },
    Defer {
        err: bool,
        body: Block,
        span: Span,
    },
    Block(Block),
    If(IfStmt),
    While {
        label: Option<(String, Span)>,
        cond: Cond,
        body: Block,
        span: Span,
    },
    Loop {
        label: Option<(String, Span)>,
        body: Block,
        span: Span,
    },
    For {
        par: bool,
        grain: Option<Expr>,
        label: Option<(String, Span)>,
        vars: Vec<(String, Span)>,
        index: Option<(String, Span)>,
        iter: Expr,
        /// `Some((inclusive, end))` for `lo..hi` and `lo..=hi`.
        range_end: Option<(bool, Expr)>,
        step: Option<Expr>,
        body: Block,
        span: Span,
    },
    Match(MatchStmt),
    Scope {
        name: String,
        name_span: Span,
        body: Block,
        span: Span,
    },
    Spawn {
        call: Expr,
        scope: Expr,
        span: Span,
    },
    Dispatch {
        call: Expr,
        domain: Expr,
        executor: Expr,
        span: Span,
    },
    Select {
        arms: Vec<SelectArm>,
        span: Span,
    },
    Unsafe {
        reason: String,
        reason_span: Span,
        body: Block,
        span: Span,
    },
    When {
        condition: Expr,
        then: Block,
        otherwise: Vec<Stmt>,
        span: Span,
    },
    Poison,
}

impl Stmt {
    pub fn span(&self) -> Span {
        match self {
            Stmt::Let { span, .. }
            | Stmt::Assign { span, .. }
            | Stmt::Expr { span, .. }
            | Stmt::Return { span, .. }
            | Stmt::Fail { span, .. }
            | Stmt::Break { span, .. }
            | Stmt::Continue { span, .. }
            | Stmt::Defer { span, .. }
            | Stmt::While { span, .. }
            | Stmt::Loop { span, .. }
            | Stmt::For { span, .. }
            | Stmt::Scope { span, .. }
            | Stmt::Spawn { span, .. }
            | Stmt::Dispatch { span, .. }
            | Stmt::Select { span, .. }
            | Stmt::Unsafe { span, .. }
            | Stmt::When { span, .. } => *span,
            Stmt::Block(b) => b.span,
            Stmt::If(x) => x.span,
            Stmt::Match(x) => x.span,
            Stmt::Poison => Span::default(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct IfStmt {
    pub cond: Cond,
    pub then: Block,
    pub otherwise: Option<Box<Stmt>>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct Cond {
    pub expr: Expr,
    pub let_pattern: Option<Pattern>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct MatchStmt {
    pub scrutinee: Expr,
    pub arms: Vec<Arm>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct Arm {
    pub patterns: Vec<Pattern>,
    pub is_else: bool,
    pub guard: Option<Expr>,
    pub body: ArmBody,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub enum ArmBody {
    Block(Block),
    Expr(Expr),
}

#[derive(Clone, Debug)]
pub struct SelectArm {
    /// True for the `else => { ... }` arm. Its pattern is a wildcard and its call a
    /// poison expression, both with empty spans.
    pub is_else: bool,
    pub pattern: Pattern,
    pub call: Expr,
    pub body: Block,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub enum Pattern {
    Wild(Span),
    Bind {
        name: String,
        span: Span,
    },
    Literal(Expr),
    Range {
        lo: Box<Pattern>,
        hi: Box<Pattern>,
        inclusive: bool,
        span: Span,
    },
    Variant {
        path: Option<PathTy>,
        name: String,
        name_span: Span,
        payload: Option<Box<Pattern>>,
    },
    Tuple {
        elems: Vec<Pattern>,
        span: Span,
    },
    Struct {
        ty: Option<PathTy>,
        fields: Vec<FieldPattern>,
        /// The pattern ends in `..` and leaves its other fields out (`grammar.md`).
        rest: bool,
        rest_span: Span,
        span: Span,
    },
    Poison,
}

impl Pattern {
    pub fn span(&self) -> Span {
        match self {
            Pattern::Wild(s) => *s,
            Pattern::Bind { span, .. } => *span,
            Pattern::Literal(e) => e.span,
            Pattern::Range { span, .. } => *span,
            Pattern::Variant { name_span, .. } => *name_span,
            Pattern::Tuple { span, .. } => *span,
            Pattern::Struct { span, .. } => *span,
            Pattern::Poison => Span::default(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct FieldPattern {
    pub name: String,
    pub name_span: Span,
    pub pattern: Option<Pattern>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AssignOp {
    Assign,
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    AddWrap,
    SubWrap,
    MulWrap,
    AddSat,
    SubSat,
    MulSat,
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
}

impl AssignOp {
    pub fn symbol(self) -> &'static str {
        match self {
            AssignOp::Assign => "=",
            AssignOp::Add => "+",
            AssignOp::Sub => "-",
            AssignOp::Mul => "*",
            AssignOp::Div => "/",
            AssignOp::Rem => "%",
            AssignOp::AddWrap => "+%",
            AssignOp::SubWrap => "-%",
            AssignOp::MulWrap => "*%",
            AssignOp::AddSat => "+|",
            AssignOp::SubSat => "-|",
            AssignOp::MulSat => "*|",
            AssignOp::BitAnd => "&",
            AssignOp::BitOr => "|",
            AssignOp::BitXor => "^",
            AssignOp::Shl => "<<",
            AssignOp::Shr => ">>",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnOp {
    Neg,
    NegWrap,
    Not,
    BitNot,
    Borrow,
    BorrowMut,
    Try,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    AddWrap,
    SubWrap,
    MulWrap,
    AddSat,
    SubSat,
    MulSat,
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    And,
    Or,
}

impl BinOp {
    pub fn symbol(self) -> &'static str {
        match self {
            BinOp::Add => "+",
            BinOp::Sub => "-",
            BinOp::Mul => "*",
            BinOp::Div => "/",
            BinOp::Rem => "%",
            BinOp::AddWrap => "+%",
            BinOp::SubWrap => "-%",
            BinOp::MulWrap => "*%",
            BinOp::AddSat => "+|",
            BinOp::SubSat => "-|",
            BinOp::MulSat => "*|",
            BinOp::BitAnd => "&",
            BinOp::BitOr => "|",
            BinOp::BitXor => "^",
            BinOp::Shl => "<<",
            BinOp::Shr => ">>",
            BinOp::Eq => "==",
            BinOp::Ne => "!=",
            BinOp::Lt => "<",
            BinOp::Le => "<=",
            BinOp::Gt => ">",
            BinOp::Ge => ">=",
            BinOp::And => "&&",
            BinOp::Or => "||",
        }
    }

    pub fn is_comparison(self) -> bool {
        matches!(
            self,
            BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge
        )
    }

    pub fn is_bitwise(self) -> bool {
        matches!(self, BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor)
    }

    pub fn is_shift(self) -> bool {
        matches!(self, BinOp::Shl | BinOp::Shr)
    }

    pub fn is_arithmetic(self) -> bool {
        matches!(
            self,
            BinOp::Add
                | BinOp::Sub
                | BinOp::Mul
                | BinOp::Div
                | BinOp::Rem
                | BinOp::AddWrap
                | BinOp::SubWrap
                | BinOp::MulWrap
                | BinOp::AddSat
                | BinOp::SubSat
                | BinOp::MulSat
        )
    }

    pub fn traps_on_overflow(self) -> bool {
        matches!(self, BinOp::Add | BinOp::Sub | BinOp::Mul)
    }
}

#[derive(Clone, Debug)]
pub struct Expr {
    pub kind: ExprKind,
    pub span: Span,
}

impl Expr {
    pub fn new(kind: ExprKind, span: Span) -> Self {
        Expr { kind, span }
    }

    pub fn poison(span: Span) -> Self {
        Expr::new(ExprKind::Poison, span)
    }
}

#[derive(Clone, Debug)]
pub enum ExprKind {
    Int {
        value: u128,
        negative: bool,
        /// Text as written, for trap messages and better diagnostics.
        text: String,
    },
    Float(f64),
    Str(Vec<u8>),
    CStr(Vec<u8>),
    Bool(bool),
    Ident(String),
    Path(PathTy),
    Builtin {
        name: String,
        type_arg: Option<Box<Ty>>,
        args: Vec<Expr>,
    },
    Unary {
        op: UnOp,
        expr: Box<Expr>,
        op_span: Span,
    },
    Binary {
        op: BinOp,
        lhs: Box<Expr>,
        rhs: Box<Expr>,
        op_span: Span,
    },
    Call {
        callee: Box<Expr>,
        args: Vec<Expr>,
        args_span: Span,
    },
    Index {
        base: Box<Expr>,
        index: Box<Expr>,
        bracket: Span,
    },
    Slice {
        base: Box<Expr>,
        lo: Box<Expr>,
        hi: Box<Expr>,
        bracket: Span,
    },
    Field {
        base: Box<Expr>,
        name: String,
        name_span: Span,
    },
    TupleField {
        base: Box<Expr>,
        index: u64,
        span: Span,
    },
    Deref {
        base: Box<Expr>,
        op_span: Span,
    },
    TypeApply {
        base: Box<Expr>,
        args: Vec<Ty>,
        span: Span,
    },
    Cast {
        expr: Box<Expr>,
        ty: Ty,
        as_span: Span,
    },
    StructLit {
        ty: PathTy,
        fields: Vec<FieldInit>,
        braces: Span,
    },
    StructShorthand {
        fields: Vec<FieldInit>,
        braces: Span,
    },
    EnumLit {
        path: Option<PathTy>,
        variant: String,
        variant_span: Span,
        payload: Option<Box<Expr>>,
    },
    ArrayLit {
        len: Box<Expr>,
        elem: Ty,
        elems: Vec<Expr>,
        span: Span,
    },
    Range {
        lo: Box<Expr>,
        hi: Box<Expr>,
        inclusive: bool,
        span: Span,
    },
    Tuple {
        elems: Vec<Expr>,
        span: Span,
    },
    Paren(Box<Expr>),
    If {
        cond: Box<Cond>,
        then: Box<Expr>,
        otherwise: Box<Expr>,
        span: Span,
    },
    Match {
        scrutinee: Box<Expr>,
        arms: Vec<Arm>,
        span: Span,
    },
    Closure {
        captures: Vec<Capture>,
        params: Vec<Param>,
        ret: Ty,
        error: Option<Ty>,
        body: Block,
        span: Span,
    },
    Catch {
        value: Box<Expr>,
        /// The bound error name, or `None` for `catch _ =>`.
        binding: Option<(String, Span)>,
        handler: Handler,
        catch_span: Span,
    },
    Orelse {
        value: Box<Expr>,
        handler: Handler,
        orelse_span: Span,
    },
    Spawn {
        call: Box<Expr>,
        scope: Box<Expr>,
        span: Span,
    },
    Dispatch {
        call: Box<Expr>,
        domain: Box<Expr>,
        executor: Box<Expr>,
        span: Span,
    },
    Unsafe {
        reason: String,
        reason_span: Span,
        body: Box<Expr>,
        span: Span,
    },
    Poison,
}

#[derive(Clone, Debug)]
pub enum Handler {
    Expr(Box<Expr>),
    Block(Block),
}

#[derive(Clone, Debug)]
pub struct FieldInit {
    pub name: String,
    pub name_span: Span,
    pub value: Expr,
}

#[derive(Clone, Debug)]
pub struct Capture {
    pub name: String,
    pub span: Span,
    pub shared: bool,
    pub mutable: bool,
}

#[derive(Clone, Debug)]
pub struct PathTy {
    pub segments: Vec<(String, Span)>,
    pub args: Vec<GenericArg>,
    /// The signature of a function-trait bound: `Fn(A, B) -> R ! E`. When it is
    /// present, `segments` is the single name `Fn`, `FnMut` or `FnOnce` and `args`
    /// is empty.
    pub fn_sig: Option<FnBoundSig>,
    pub args_span: Option<Span>,
    pub assoc: Option<(String, Span)>,
    pub span: Span,
}

impl PathTy {
    pub fn name(&self) -> &str {
        self.segments.last().map(|(s, _)| s.as_str()).unwrap_or("")
    }

    pub fn module_prefix(&self) -> &[(String, Span)] {
        if self.segments.len() > 1 {
            &self.segments[..self.segments.len() - 1]
        } else {
            &[]
        }
    }
}

#[derive(Clone, Debug)]
pub struct FnBoundSig {
    pub params: Vec<Ty>,
    pub ret: Option<Box<Ty>>,
    pub error: Option<Box<Ty>>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub enum GenericArg {
    Ty(Ty),
    Const(Expr),
}

#[derive(Clone, Debug)]
pub enum Ty {
    Scalar(ScalarTy, Span),
    Vector(VectorTy, Span),
    Path(PathTy),
    Borrow {
        mutable: bool,
        inner: Box<Ty>,
        span: Span,
    },
    Slice {
        mutable: bool,
        elem: Box<Ty>,
        span: Span,
    },
    Array {
        len: Box<Expr>,
        elem: Box<Ty>,
        span: Span,
    },
    Ptr {
        mutable: bool,
        inner: Box<Ty>,
        span: Span,
    },
    Optional {
        inner: Box<Ty>,
        span: Span,
    },
    Tuple {
        elems: Vec<Ty>,
        span: Span,
    },
    Fn {
        /// True for `extern fn(...)`, the C-ABI function pointer.
        ext: bool,
        params: Vec<Ty>,
        ret: Box<Ty>,
        error: Option<Box<Ty>>,
        span: Span,
    },
    Dyn {
        interface: Box<PathTy>,
        mutable: bool,
        span: Span,
    },
    Poison(Span),
}

impl Ty {
    pub fn span(&self) -> Span {
        match self {
            Ty::Scalar(_, span) | Ty::Vector(_, span) => *span,
            Ty::Path(p) => p.span,
            Ty::Borrow { span, .. }
            | Ty::Slice { span, .. }
            | Ty::Array { span, .. }
            | Ty::Ptr { span, .. }
            | Ty::Optional { span, .. }
            | Ty::Tuple { span, .. }
            | Ty::Fn { span, .. }
            | Ty::Dyn { span, .. }
            | Ty::Poison(span) => *span,
        }
    }

    pub fn is_void(&self) -> bool {
        matches!(self, Ty::Scalar(ScalarTy::Void, _))
    }

    pub fn is_never(&self) -> bool {
        matches!(self, Ty::Scalar(ScalarTy::Never, _))
    }

    pub fn is_borrow(&self) -> bool {
        matches!(self, Ty::Borrow { .. } | Ty::Slice { .. } | Ty::Dyn { .. })
    }
}
