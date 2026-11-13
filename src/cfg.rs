//! Phase 8: lower a typed function body to a control-flow graph.
//!
//! The graph is the input to the ownership analyses in `cfg_check`. It keeps
//! places, explicit copies and moves, two-phase borrows, storage starts and ends,
//! deferred-block copies, and call boundaries. Checks and cost items have their
//! own record in the same statements; the ownership pass ignores them.

use crate::ast::*;
use crate::check::{ConstVal, Function, LocalFact, Program};
use crate::source::{Source, Span};
use crate::types::Type;
use std::collections::{HashMap, HashSet};

pub type LocalId = usize;
pub type BlockId = usize;
pub type LoanId = usize;

#[derive(Clone, Debug)]
pub struct Cfg {
    pub module: String,
    pub locals: Vec<CfgLocal>,
    pub blocks: Vec<Block>,
    pub entry: BlockId,
    pub loans: Vec<LoanInfo>,
    /// Moves made while a pending `defer` still uses the value: (move, defer).
    pub defer_conflicts: Vec<(Span, Span)>,
    /// Spans of statements copied out of deferred blocks.
    pub deferred_statements: HashSet<Span>,
    /// `@assert(base.len >= n)` facts, applied after a statement index.
    pub len_facts: Vec<LenFact>,
    /// Counted loop facts, applied at the body block.
    pub loop_facts: Vec<LoopFact>,
}

#[derive(Clone, Debug)]
pub struct LenFact {
    pub block: BlockId,
    pub after: usize,
    pub base: LocalId,
    pub at_least: i64,
}

#[derive(Clone, Debug)]
pub struct LoopFact {
    pub body: BlockId,
    pub counter: LocalId,
    pub base: Option<LocalId>,
}

#[derive(Clone, Debug)]
pub struct CfgLocal {
    pub name: Option<String>,
    pub ty: Type,
    pub span: Span,
    pub is_param: bool,
    pub linear: bool,
}

#[derive(Clone, Debug)]
pub struct LoanInfo {
    pub place: Place,
    pub kind: BorrowKind,
    pub two_phase: bool,
    pub span: Span,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BorrowKind {
    Shared,
    Exclusive,
}

#[derive(Clone, Debug)]
pub struct Block {
    pub stmts: Vec<CfgStmt>,
    pub term: Term,
}

#[derive(Clone, Debug)]
pub enum CfgStmt {
    Assign {
        place: Place,
        value: Rvalue,
        span: Span,
    },
    StorageStart {
        local: LocalId,
        span: Span,
    },
    StorageEnd {
        local: LocalId,
        span: Span,
    },
    /// A read or move kept for its effect (for example `@forget(x)`).
    Use {
        place: Place,
        mode: UseMode,
        span: Span,
    },
    Check {
        check: CheckStmt,
    },
    CancelScope {
        local: LocalId,
        span: Span,
    },
    /// Consume a discriminated union after its payload has been moved out.
    ConsumeUnion {
        place: Place,
        span: Span,
    },
}

#[derive(Clone, Debug)]
pub struct CheckStmt {
    pub kind: CheckKind,
    pub span: Span,
    pub lhs: Option<Operand>,
    pub rhs: Option<Operand>,
    pub bounds: Option<BoundsCheck>,
}

#[derive(Clone, Debug)]
pub struct BoundsCheck {
    pub base: Place,
    pub index: Operand,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CheckKind {
    Bounds,
    Overflow,
    Division,
    Shift,
    Narrow,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UseMode {
    Copy,
    Move,
}

#[derive(Clone, Debug)]
pub enum Rvalue {
    Use {
        place: Place,
        mode: UseMode,
    },
    Borrow {
        loan: LoanId,
        place: Place,
        mutable: bool,
        two_phase: bool,
    },
    /// A slice value made from an existing place. It copies the base's origins
    /// and, for an exclusive base, reborrows it.
    Slice {
        place: Place,
        mutable: bool,
    },
    Binary {
        op: BinOp,
        lhs: Operand,
        rhs: Operand,
    },
    Unary {
        op: UnOp,
        operand: Operand,
    },
    Aggregate {
        kind: AggKind,
        fields: Vec<Operand>,
    },
    Cast {
        operand: Operand,
    },
    Len {
        place: Place,
    },
    Constant,
    Zero,
    /// Convert an error to the enclosing function's error type for `try`.
    ConvertError {
        operand: Operand,
    },
}

#[derive(Clone, Debug)]
pub enum AggKind {
    Struct(String),
    Tuple,
    Array,
    Enum { ty: String, variant: String },
    ResultOk,
    ResultErr,
    OptionalSome,
    OptionalNone,
}

#[derive(Clone, Debug)]
pub enum Operand {
    Place(Place),
    Const(ConstVal),
    Unit,
}

#[derive(Clone, Debug)]
pub struct Place {
    pub local: LocalId,
    pub proj: Vec<Proj>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Proj {
    Field(String),
    Index,
    Deref,
    Variant(String),
}

impl Place {
    pub fn root(&self) -> LocalId {
        self.local
    }

    pub fn derefs(&self) -> usize {
        self.proj.iter().filter(|p| **p == Proj::Deref).count()
    }
}

#[derive(Clone, Debug)]
pub enum Term {
    Goto(BlockId),
    Branch {
        cond: Operand,
        then_b: BlockId,
        else_b: BlockId,
        span: Span,
    },
    Switch {
        value: Place,
        cases: Vec<(String, BlockId)>,
        default: BlockId,
        span: Span,
    },
    Call {
        function: Option<String>,
        args: Vec<Operand>,
        dest: Option<Place>,
        then_b: BlockId,
        span: Span,
    },
    Spawn {
        function: Option<String>,
        args: Vec<Operand>,
        scope: Place,
        dest: Option<Place>,
        handle_loan: Option<LoanId>,
        then_b: BlockId,
        span: Span,
    },
    ScopeWait {
        scope: Place,
        then_b: BlockId,
        span: Span,
    },
    Return {
        value: Option<Operand>,
        is_error: bool,
        span: Span,
    },
    Trap {
        span: Span,
    },
    Unreachable,
}

#[derive(Clone, Debug)]
struct DeferBlock {
    body: crate::ast::Block,
    err: bool,
    span: Span,
}

#[derive(Clone, Debug)]
struct LowerScope {
    locals: Vec<LocalId>,
    defers: Vec<DeferBlock>,
    scope_local: Option<LocalId>,
    loop_break: Option<BlockId>,
    loop_continue: Option<BlockId>,
    label: Option<String>,
}

struct Lower<'a> {
    program: &'a Program,
    function: &'a Function,
    module: String,
    expr_types: &'a HashMap<Span, Type>,
    cfg: Cfg,
    current: Option<BlockId>,
    current_scope: usize,
    scopes: Vec<LowerScope>,
    local_by_name: HashMap<String, LocalId>,
    current_span: Span,
    bounds: HashMap<String, Vec<String>>,
    in_defer_copy: bool,
}

pub fn lower(
    program: &Program,
    _source: &Source,
    function: &Function,
    _facts: &[LocalFact],
    expr_types: &HashMap<Span, Type>,
) -> Cfg {
    let mut l = Lower {
        program,
        function,
        module: function.module.clone(),
        expr_types,
        cfg: Cfg {
            module: function.module.clone(),
            locals: Vec::new(),
            blocks: Vec::new(),
            entry: 0,
            loans: Vec::new(),
            defer_conflicts: Vec::new(),
            deferred_statements: HashSet::new(),
            len_facts: Vec::new(),
            loop_facts: Vec::new(),
        },
        current: None,
        current_scope: 0,
        scopes: Vec::new(),
        local_by_name: HashMap::new(),
        current_span: Span::default(),
        bounds: function
            .decl
            .generics
            .iter()
            .map(|g| {
                (
                    g.name.clone(),
                    g.bounds.iter().map(|b| b.name().to_string()).collect(),
                )
            })
            .collect(),
        in_defer_copy: false,
    };
    l.run();
    l.cfg
}

impl<'a> Lower<'a> {
    fn run(&mut self) {
        let entry = self.new_block();
        self.current = Some(entry);
        self.cfg.entry = entry;
        // Parameters are in the function scope, so every exit ends their storage.
        self.scopes.push(LowerScope {
            locals: Vec::new(),
            defers: Vec::new(),
            scope_local: None,
            loop_break: None,
            loop_continue: None,
            label: None,
        });
        let function_scope = self.scopes.len() - 1;
        self.current_scope = function_scope;
        let params: Vec<(String, Type, Span)> = self
            .function
            .decl
            .params
            .iter()
            .enumerate()
            .map(|(i, p)| {
                (
                    p.name.clone(),
                    self.function
                        .signature_params
                        .get(i)
                        .cloned()
                        .unwrap_or(Type::Error),
                    p.name_span,
                )
            })
            .collect();
        for (name, ty, span) in params {
            let id = self.add_named_local(&name, ty, span, true);
            self.emit(CfgStmt::StorageStart { local: id, span });
            self.emit(CfgStmt::Assign {
                place: Place {
                    local: id,
                    proj: Vec::new(),
                },
                value: Rvalue::Constant,
                span,
            });
            self.scopes[function_scope].locals.push(id);
        }
        let end = self.new_block();
        if let Some(body) = &self.function.decl.body {
            self.lower_block(body, Some(end));
        } else {
            self.goto(end);
        }
        self.current = Some(end);
        // End the function-level scope, then return.
        let scope = self.scopes[function_scope].clone();
        self.finish_scope(&scope, false, true);
        self.terminate(Term::Return {
            value: None,
            is_error: false,
            span: self.function.decl.span,
        });
    }

    // ------------------------------------------------------------- blocks

    fn new_block(&mut self) -> BlockId {
        let id = self.cfg.blocks.len();
        self.cfg.blocks.push(Block {
            stmts: Vec::new(),
            term: Term::Unreachable,
        });
        id
    }

    fn emit(&mut self, stmt: CfgStmt) {
        if self.in_defer_copy
            && let Some(span) = stmt_span(&stmt)
        {
            self.cfg.deferred_statements.insert(span);
        }
        if let Some(b) = self.current {
            self.cfg.blocks[b].stmts.push(stmt);
        }
    }

    fn terminate(&mut self, term: Term) {
        if let Some(b) = self.current.take() {
            self.cfg.blocks[b].term = term;
        }
    }

    fn goto(&mut self, target: BlockId) {
        self.terminate(Term::Goto(target));
    }

    fn set_current(&mut self, id: BlockId) {
        self.current = Some(id);
    }

    fn declare_local(&mut self, name: String, ty: Type, span: Span, mutable: bool) -> LocalId {
        let id = self.add_named_local(&name, ty, span, false);
        self.emit(CfgStmt::StorageStart { local: id, span });
        self.scopes[self.current_scope].locals.push(id);
        let _ = mutable;
        id
    }

    fn add_named_local(&mut self, name: &str, ty: Type, span: Span, is_param: bool) -> LocalId {
        let id = self.cfg.locals.len();
        let linear = self.is_linear(&ty);
        self.cfg.locals.push(CfgLocal {
            name: Some(name.to_string()),
            ty,
            span,
            is_param,
            linear,
        });
        self.local_by_name.insert(name.to_string(), id);
        id
    }

    fn add_temp(&mut self, ty: Type, span: Span) -> LocalId {
        let span = if span.start == 0 && span.end == 0 {
            self.current_span
        } else {
            span
        };
        let id = self.cfg.locals.len();
        let linear = self.is_linear(&ty);
        self.cfg.locals.push(CfgLocal {
            name: None,
            ty,
            span,
            is_param: false,
            linear,
        });
        self.emit(CfgStmt::StorageStart { local: id, span });
        self.scopes[self.current_scope].locals.push(id);
        id
    }

    fn note_move(&mut self, place: &Place, span: Span) {
        if self.in_defer_copy {
            return;
        }
        let Some(name) = self.cfg.locals[place.local].name.clone() else {
            return;
        };
        for scope in self.scopes.iter().rev() {
            for d in scope.defers.iter().rev() {
                if d.err {
                    continue;
                }
                if block_uses_name(&d.body, &name) {
                    self.cfg.defer_conflicts.push((span, d.span));
                }
            }
        }
    }

    fn add_loan(&mut self, place: &Place, kind: BorrowKind, two_phase: bool, span: Span) -> LoanId {
        let id = self.cfg.loans.len();
        self.cfg.loans.push(LoanInfo {
            place: place.clone(),
            kind,
            two_phase,
            span,
        });
        id
    }

    fn type_of(&self, e: &Expr) -> Type {
        self.expr_types.get(&e.span).cloned().unwrap_or(Type::Error)
    }

    fn place_type(&self, place: &Place) -> Type {
        let mut ty = self.cfg.locals[place.local].ty.clone();
        for p in &place.proj {
            ty = match (p, &ty) {
                (Proj::Field(name), t) => self.program.defs.field(t, name).unwrap_or(Type::Error),
                (Proj::Index, Type::Array(_, e)) => (**e).clone(),
                (Proj::Index, Type::Slice { elem, .. }) => (**elem).clone(),
                (Proj::Index, Type::Vector(v)) => Type::Int(v.elem),
                (Proj::Deref, Type::Borrow { inner, .. }) => (**inner).clone(),
                (Proj::Deref, Type::Ptr { inner, .. }) => (**inner).clone(),
                (Proj::Variant(_), Type::Result(a, _)) => (**a).clone(),
                (Proj::Variant(_), Type::Optional(a)) => (**a).clone(),
                (Proj::Variant(_), Type::Enum(_, _)) => Type::Error,
                _ => Type::Error,
            };
        }
        ty
    }

    fn is_copy(&self, ty: &Type) -> bool {
        ty.is_copy(&self.program.defs, &self.bounds)
    }

    fn is_linear(&self, ty: &Type) -> bool {
        ty.is_linear_value(&self.program.defs, &self.bounds)
    }

    // ------------------------------------------------------------- scopes

    fn push_scope(&mut self) -> usize {
        self.scopes.push(LowerScope {
            locals: Vec::new(),
            defers: Vec::new(),
            scope_local: None,
            loop_break: None,
            loop_continue: None,
            label: None,
        });
        self.current_scope = self.scopes.len() - 1;
        self.current_scope
    }

    fn pop_scope(&mut self) {
        self.scopes.pop();
        self.current_scope = self.scopes.len().saturating_sub(1);
    }

    fn lower_block(&mut self, block: &crate::ast::Block, target: Option<BlockId>) {
        self.push_scope();
        self.lower_stmts(&block.stmts);
        if self.current.is_some() {
            let scope = self.scopes[self.current_scope].clone();
            self.finish_scope(&scope, false, false);
        }
        if let Some(t) = target
            && self.current.is_some()
        {
            self.goto(t);
        }
        self.pop_scope();
    }

    fn finish_scope(&mut self, scope: &LowerScope, error: bool, include_wait: bool) {
        for d in scope.defers.iter().rev() {
            if d.err && !error {
                continue;
            }
            self.lower_defer_body(&d.body);
        }
        for id in scope.locals.iter().rev() {
            let span = self.cfg.locals[*id].span;
            self.emit(CfgStmt::StorageEnd { local: *id, span });
        }
        if let Some(scope_local) = scope.scope_local {
            let span = self.cfg.locals[scope_local].span;
            if error {
                self.emit(CfgStmt::CancelScope {
                    local: scope_local,
                    span,
                });
            }
            if include_wait {
                let next = self.new_block();
                self.terminate(Term::ScopeWait {
                    scope: Place {
                        local: scope_local,
                        proj: Vec::new(),
                    },
                    then_b: next,
                    span,
                });
                self.set_current(next);
            }
            self.emit(CfgStmt::StorageEnd {
                local: scope_local,
                span,
            });
        }
    }

    fn cleanup_all(&mut self, error: bool) {
        let scopes = self.scopes.clone();
        for scope in scopes.iter().rev() {
            self.finish_scope(scope, error, true);
        }
    }

    /// Lower a deferred body inline at an exit. It has its own storage and
    /// defers; it never contains a legal `return`, `fail` or `try`.
    fn lower_defer_body(&mut self, body: &crate::ast::Block) {
        let old = self.in_defer_copy;
        self.in_defer_copy = true;
        self.push_scope();
        self.lower_stmts(&body.stmts);
        if self.current.is_some() {
            let scope = self.scopes[self.current_scope].clone();
            self.finish_scope(&scope, false, false);
        }
        self.pop_scope();
        self.in_defer_copy = old;
    }

    // ---------------------------------------------------------- statements

    fn lower_stmts(&mut self, stmts: &[crate::ast::Stmt]) {
        for stmt in stmts {
            if self.current.is_none() {
                break;
            }
            self.lower_stmt(stmt);
        }
    }

    fn lower_stmt(&mut self, stmt: &crate::ast::Stmt) {
        self.current_span = stmt.span();
        match stmt {
            Stmt::Let {
                pattern,
                ty,
                init,
                span,
                ..
            } => {
                let expected = ty.as_ref().map(|t| self.resolve_ty(t));
                let op = self.lower_expr(init);
                let bind_ty = expected.unwrap_or_else(|| self.type_of(init));
                self.bind_pattern(pattern, &bind_ty, Some(op), *span);
            }
            Stmt::Assign {
                place,
                op,
                value,
                span,
            } => {
                let dest = self.lower_place(place);
                let rhs = self.lower_expr(value);
                let value = if *op == AssignOp::Assign {
                    self.make_rvalue(rhs)
                } else {
                    let rhs = self.consume_operand(rhs, UseMode::Move);
                    let kind = match op {
                        AssignOp::Add | AssignOp::Sub | AssignOp::Mul => Some(CheckKind::Overflow),
                        AssignOp::Div | AssignOp::Rem => Some(CheckKind::Division),
                        AssignOp::Shl | AssignOp::Shr => Some(CheckKind::Shift),
                        _ => None,
                    };
                    if let Some(kind) = kind {
                        self.emit(CfgStmt::Check {
                            check: CheckStmt {
                                kind,
                                span: *span,
                                lhs: Some(Operand::Place(dest.clone())),
                                rhs: Some(rhs.clone()),
                                bounds: None,
                            },
                        });
                    }
                    Rvalue::Binary {
                        op: assign_binop(*op),
                        lhs: Operand::Place(dest.clone()),
                        rhs,
                    }
                };
                self.emit(CfgStmt::Assign {
                    place: dest,
                    value,
                    span: *span,
                });
            }
            Stmt::Expr { expr, .. } => {
                let op = self.lower_expr(expr);
                // A call's result is already in a temp and its storage ends with
                // the block; no extra operation is needed.
                let _ = op;
            }
            Stmt::Return { value, span } => {
                let op = value.as_ref().map(|e| self.lower_expr(e));
                let op = op.map(|o| self.consume_operand(o, UseMode::Move));
                if let Some(Operand::Place(p)) = &op {
                    self.emit(CfgStmt::Use {
                        place: p.clone(),
                        mode: UseMode::Move,
                        span: *span,
                    });
                }
                self.cleanup_all(false);
                if self.current.is_some() {
                    self.terminate(Term::Return {
                        value: op,
                        is_error: false,
                        span: *span,
                    });
                }
            }
            Stmt::Fail { value, span } => {
                let op = self.lower_expr(value);
                let op = self.consume_operand(op, UseMode::Move);
                self.cleanup_all(true);
                if self.current.is_some() {
                    self.terminate(Term::Return {
                        value: Some(op),
                        is_error: true,
                        span: *span,
                    });
                }
            }
            Stmt::Break { label, span } => self.lower_break(label.as_deref(), *span),
            Stmt::Continue { label, span } => self.lower_continue(label.as_deref(), *span),
            Stmt::Defer { err, body, span } => {
                self.scopes[self.current_scope].defers.push(DeferBlock {
                    body: body.clone(),
                    err: *err,
                    span: *span,
                });
            }
            Stmt::Block(b) => {
                let join = self.new_block();
                self.lower_block(b, Some(join));
                self.set_current(join);
            }
            Stmt::If(x) => self.lower_if_stmt(x),
            Stmt::While { cond, body, .. } => self.lower_while(cond, body),
            Stmt::Loop { label, body, .. } => self.lower_loop(label.as_deref(), body),
            Stmt::For {
                par: _,
                grain: _,
                label,
                vars,
                index,
                iter,
                range_end,
                step,
                body,
                span,
            } => self.lower_for(
                label.as_deref(),
                vars,
                index.as_ref(),
                iter,
                range_end.as_ref(),
                step.as_ref(),
                body,
                *span,
            ),
            Stmt::Match(m) => {
                self.lower_match(&m.scrutinee, &m.arms, None, false, m.span);
            }
            Stmt::Scope {
                name,
                name_span,
                body,
                span,
            } => self.lower_scope(name, *name_span, body, *span),
            Stmt::Spawn { call, scope, span } => {
                self.lower_spawn(call, scope, None, *span);
            }
            Stmt::Dispatch {
                call,
                domain,
                executor,
                ..
            } => {
                let _ = domain;
                let _ = executor;
                self.lower_call_expr(call);
            }
            Stmt::Select { arms, .. } => {
                // Select is a runtime construct. For ownership, model each
                // waitable call, joined together.
                for arm in arms {
                    if self.current.is_none() {
                        break;
                    }
                    let entry = self.current.unwrap();
                    let _ = self.lower_expr(&arm.call);
                    self.lower_block(&arm.body, Some(entry));
                }
            }
            Stmt::Unsafe { body, .. } => self.lower_block(body, None),
            Stmt::When {
                then, otherwise, ..
            } => {
                if !then.stmts.is_empty() {
                    let join = self.new_block();
                    self.lower_block(then, Some(join));
                    self.set_current(join);
                } else if let Some(first) = otherwise.first() {
                    self.lower_stmt(first);
                }
            }
            Stmt::Poison => {}
        }
    }

    fn lower_break(&mut self, label: Option<&str>, span: Span) {
        if let Some(target) = self.find_loop(label, true) {
            let idx = self.loop_scope_index(label).unwrap_or(0);
            self.cleanup_to(idx, false);
            if self.current.is_some() {
                self.goto(target);
            }
        } else {
            self.terminate(Term::Trap { span });
        }
    }

    fn lower_continue(&mut self, label: Option<&str>, span: Span) {
        if let Some(target) = self.find_loop(label, false) {
            let idx = self.loop_scope_index(label).unwrap_or(0);
            // Continue keeps the loop's own header scope; clean inner blocks.
            self.cleanup_to(idx + 1, false);
            if self.current.is_some() {
                self.goto(target);
            }
        } else {
            self.terminate(Term::Trap { span });
        }
    }

    fn loop_scope_index(&self, label: Option<&str>) -> Option<usize> {
        for (i, s) in self.scopes.iter().enumerate().rev() {
            if s.loop_break.is_some()
                && label.map(|l| s.label.as_deref() == Some(l)).unwrap_or(true)
            {
                return Some(i);
            }
        }
        None
    }

    fn find_loop(&self, label: Option<&str>, is_break: bool) -> Option<BlockId> {
        for s in self.scopes.iter().rev() {
            if label.map(|l| s.label.as_deref() == Some(l)).unwrap_or(true) {
                if is_break {
                    if let Some(t) = s.loop_break {
                        return Some(t);
                    }
                } else if let Some(t) = s.loop_continue {
                    return Some(t);
                }
            }
        }
        None
    }

    fn cleanup_to(&mut self, scope_index: usize, error: bool) {
        let scopes = self.scopes.clone();
        for scope in scopes.iter().skip(scope_index).rev() {
            self.finish_scope(scope, error, true);
        }
    }

    fn lower_if_stmt(&mut self, x: &IfStmt) {
        let join = self.new_block();
        let then_b = self.new_block();
        let else_b = self.new_block();
        let cond_place = if let Some(pattern) = &x.cond.let_pattern {
            let value = self.lower_expr(&x.cond.expr);
            let place = self.operand_place(value, UseMode::Copy);
            self.switch_to(pattern, &place, then_b, else_b, x.cond.span);
            Some((pattern.clone(), place))
        } else {
            let cond = self.lower_expr(&x.cond.expr);
            self.terminate(Term::Branch {
                cond,
                then_b,
                else_b,
                span: x.cond.span,
            });
            None
        };
        self.set_current(then_b);
        if let Some((pattern, place)) = &cond_place {
            self.bind_from_place(pattern, place, UseMode::Copy);
        }
        self.lower_block(&x.then, Some(join));
        self.set_current(else_b);
        if let Some(e) = &x.otherwise {
            self.lower_stmt(e);
        }
        if self.current.is_some() {
            self.goto(join);
        }
        self.set_current(join);
    }

    fn lower_while(&mut self, cond: &Cond, body: &crate::ast::Block) {
        let header = self.new_block();
        let body_b = self.new_block();
        let exit = self.new_block();
        self.goto(header);
        self.set_current(header);
        let cond_place = if let Some(pattern) = &cond.let_pattern {
            let value = self.lower_expr(&cond.expr);
            let place = self.operand_place(value, UseMode::Copy);
            self.switch_to(pattern, &place, body_b, exit, cond.span);
            Some((pattern.clone(), place))
        } else {
            let op = self.lower_expr(&cond.expr);
            self.terminate(Term::Branch {
                cond: op,
                then_b: body_b,
                else_b: exit,
                span: cond.span,
            });
            None
        };
        self.set_current(body_b);
        self.push_scope();
        self.scopes[self.current_scope].loop_break = Some(exit);
        self.scopes[self.current_scope].loop_continue = Some(header);
        if let Some((pattern, place)) = &cond_place {
            self.bind_from_place(pattern, place, UseMode::Copy);
        }
        self.lower_block(body, None);
        if self.current.is_some() {
            self.goto(header);
        }
        let loop_scope = self.scopes[self.current_scope].clone();
        self.set_current(exit);
        self.finish_scope(&loop_scope, false, false);
        self.pop_scope();
    }

    fn lower_loop(&mut self, label: Option<&str>, body: &crate::ast::Block) {
        let header = self.new_block();
        let body_b = self.new_block();
        let exit = self.new_block();
        self.goto(header);
        self.set_current(header);
        self.terminate(Term::Goto(body_b));
        self.set_current(body_b);
        self.push_scope();
        self.scopes[self.current_scope].loop_break = Some(exit);
        self.scopes[self.current_scope].loop_continue = Some(header);
        self.scopes[self.current_scope].label = label.map(str::to_string);
        self.lower_block(body, None);
        if self.current.is_some() {
            self.goto(header);
        }
        let loop_scope = self.scopes[self.current_scope].clone();
        self.set_current(exit);
        self.finish_scope(&loop_scope, false, false);
        self.pop_scope();
    }

    #[allow(clippy::too_many_arguments)]
    fn lower_for(
        &mut self,
        label: Option<&str>,
        vars: &[(String, Span)],
        index: Option<&(String, Span)>,
        iter: &Expr,
        range_end: Option<&(bool, Expr)>,
        step: Option<&Expr>,
        body: &crate::ast::Block,
        span: Span,
    ) {
        let header = self.new_block();
        let body_b = self.new_block();
        let step_b = self.new_block();
        let exit = self.new_block();
        self.push_scope();
        self.scopes[self.current_scope].loop_break = Some(exit);
        self.scopes[self.current_scope].loop_continue = Some(step_b);
        self.scopes[self.current_scope].label = label.map(str::to_string);
        if let Some((inclusive, end)) = range_end {
            let lo = self.lower_expr(iter);
            let hi = self.lower_expr(end);
            let counter_name = index
                .map(|(n, _)| n.as_str())
                .or_else(|| vars.first().map(|(n, _)| n.as_str()))
                .unwrap_or("__i");
            let counter_span = index
                .map(|(_, s)| *s)
                .or_else(|| vars.first().map(|(_, s)| *s))
                .unwrap_or(span);
            let counter = self.declare_local(
                counter_name.to_string(),
                self.type_of(iter),
                counter_span,
                true,
            );
            self.assign_operand(
                Place {
                    local: counter,
                    proj: Vec::new(),
                },
                lo,
                span,
            );
            let hi_local = self.add_temp(self.type_of(end), span);
            self.assign_operand(
                Place {
                    local: hi_local,
                    proj: Vec::new(),
                },
                hi,
                span,
            );
            self.goto(header);
            self.set_current(header);
            let cmp = self.add_temp(Type::Bool, span);
            self.emit(CfgStmt::Assign {
                place: Place {
                    local: cmp,
                    proj: Vec::new(),
                },
                value: Rvalue::Binary {
                    op: if *inclusive { BinOp::Le } else { BinOp::Lt },
                    lhs: Operand::Place(Place {
                        local: counter,
                        proj: Vec::new(),
                    }),
                    rhs: Operand::Place(Place {
                        local: hi_local,
                        proj: Vec::new(),
                    }),
                },
                span,
            });
            self.terminate(Term::Branch {
                cond: Operand::Place(Place {
                    local: cmp,
                    proj: Vec::new(),
                }),
                then_b: body_b,
                else_b: exit,
                span,
            });
            self.set_current(body_b);
            self.push_scope();
            if let Some((var, vspan)) = vars.first() {
                // `for i in lo..hi` binds the counter itself.
                if index.is_none() {
                    let id = self.declare_local(var.clone(), self.type_of(iter), *vspan, false);
                    self.assign_operand(
                        Place {
                            local: id,
                            proj: Vec::new(),
                        },
                        Operand::Place(Place {
                            local: counter,
                            proj: Vec::new(),
                        }),
                        span,
                    );
                    if let Some(base) = len_root_expr(end)
                        && let Some(base) = self.local_by_name.get(&base).copied()
                    {
                        self.cfg.loop_facts.push(LoopFact {
                            body: body_b,
                            counter: id,
                            base: Some(base),
                        });
                    }
                }
            }
            self.lower_stmts(&body.stmts);
            if self.current.is_some() {
                let scope = self.scopes[self.current_scope].clone();
                self.finish_scope(&scope, false, false);
                self.goto(step_b);
            }
            self.pop_scope();
            self.set_current(step_b);
            let one = Operand::Const(ConstVal::Int(1));
            let step_op = step
                .map(|s| self.lower_expr(s))
                .unwrap_or_else(|| one.clone());
            let inc = self.add_temp(self.type_of(iter), span);
            let rhs = self.consume_operand(step_op, UseMode::Move);
            self.emit(CfgStmt::Assign {
                place: Place {
                    local: inc,
                    proj: Vec::new(),
                },
                value: Rvalue::Binary {
                    op: BinOp::Add,
                    lhs: Operand::Place(Place {
                        local: counter,
                        proj: Vec::new(),
                    }),
                    rhs,
                },
                span,
            });
            self.assign_operand(
                Place {
                    local: counter,
                    proj: Vec::new(),
                },
                Operand::Place(Place {
                    local: inc,
                    proj: Vec::new(),
                }),
                span,
            );
            self.goto(header);
        } else {
            let iter_op = self.lower_expr(iter);
            let iter_place = self.operand_place(iter_op, UseMode::Copy);
            let iter_ty = self.place_type(&iter_place);
            let len = self.add_temp(Type::i64(), span);
            self.emit(CfgStmt::Assign {
                place: Place {
                    local: len,
                    proj: Vec::new(),
                },
                value: Rvalue::Len {
                    place: iter_place.clone(),
                },
                span,
            });
            let idx = self.add_temp(Type::i64(), span);
            self.emit(CfgStmt::Assign {
                place: Place {
                    local: idx,
                    proj: Vec::new(),
                },
                value: Rvalue::Constant,
                span,
            });
            let index_local = index
                .map(|(name, ispan)| self.declare_local(name.clone(), Type::i64(), *ispan, false));
            self.goto(header);
            self.set_current(header);
            let cmp = self.add_temp(Type::Bool, span);
            self.emit(CfgStmt::Assign {
                place: Place {
                    local: cmp,
                    proj: Vec::new(),
                },
                value: Rvalue::Binary {
                    op: BinOp::Lt,
                    lhs: Operand::Place(Place {
                        local: idx,
                        proj: Vec::new(),
                    }),
                    rhs: Operand::Place(Place {
                        local: len,
                        proj: Vec::new(),
                    }),
                },
                span,
            });
            self.terminate(Term::Branch {
                cond: Operand::Place(Place {
                    local: cmp,
                    proj: Vec::new(),
                }),
                then_b: body_b,
                else_b: exit,
                span,
            });
            self.set_current(body_b);
            if let Some(id) = index_local {
                self.assign_operand(
                    Place {
                        local: id,
                        proj: Vec::new(),
                    },
                    Operand::Place(Place {
                        local: idx,
                        proj: Vec::new(),
                    }),
                    span,
                );
            }
            self.push_scope();
            if let Some((var, vspan)) = vars.first() {
                let elem_ty = match &iter_ty {
                    Type::Array(_, e) => (**e).clone(),
                    Type::Slice { mutable, elem } => {
                        if *mutable {
                            Type::Borrow {
                                mutable: true,
                                inner: elem.clone(),
                            }
                        } else {
                            (**elem).clone()
                        }
                    }
                    _ => Type::Error,
                };
                let id = self.declare_local(var.clone(), elem_ty, *vspan, false);
                let mut proj = iter_place.proj.clone();
                proj.push(Proj::Index);
                self.emit(CfgStmt::Assign {
                    place: Place {
                        local: id,
                        proj: Vec::new(),
                    },
                    value: Rvalue::Use {
                        place: Place {
                            local: iter_place.local,
                            proj,
                        },
                        mode: UseMode::Copy,
                    },
                    span: *vspan,
                });
            }
            self.lower_stmts(&body.stmts);
            if self.current.is_some() {
                let scope = self.scopes[self.current_scope].clone();
                self.finish_scope(&scope, false, false);
                self.goto(step_b);
            }
            self.pop_scope();
            self.set_current(step_b);
            let inc = self.add_temp(Type::i64(), span);
            self.emit(CfgStmt::Assign {
                place: Place {
                    local: inc,
                    proj: Vec::new(),
                },
                value: Rvalue::Binary {
                    op: BinOp::Add,
                    lhs: Operand::Place(Place {
                        local: idx,
                        proj: Vec::new(),
                    }),
                    rhs: Operand::Const(ConstVal::Int(1)),
                },
                span,
            });
            self.assign_operand(
                Place {
                    local: idx,
                    proj: Vec::new(),
                },
                Operand::Place(Place {
                    local: inc,
                    proj: Vec::new(),
                }),
                span,
            );
            self.goto(header);
        }
        let loop_scope = self.scopes[self.current_scope].clone();
        self.set_current(exit);
        self.finish_scope(&loop_scope, false, false);
        self.pop_scope();
    }

    fn lower_scope(&mut self, name: &str, name_span: Span, body: &crate::ast::Block, span: Span) {
        let scope_local =
            self.add_named_local(name, Type::External("Scope".to_string()), name_span, false);
        self.emit(CfgStmt::StorageStart {
            local: scope_local,
            span: name_span,
        });
        self.emit(CfgStmt::Assign {
            place: Place {
                local: scope_local,
                proj: Vec::new(),
            },
            value: Rvalue::Constant,
            span: name_span,
        });
        self.push_scope();
        self.scopes[self.current_scope].scope_local = Some(scope_local);
        self.lower_stmts(&body.stmts);
        if self.current.is_some() {
            let scope = self.scopes[self.current_scope].clone();
            self.finish_scope(&scope, false, true);
        }
        self.pop_scope();
        let _ = span;
    }

    fn lower_match(
        &mut self,
        scrutinee: &Expr,
        arms: &[Arm],
        result: Option<LocalId>,
        _statement: bool,
        span: Span,
    ) {
        let op = self.lower_expr(scrutinee);
        let place = self.operand_place(op, UseMode::Copy);
        let join = self.new_block();
        let trap = self.new_block();
        self.set_current(trap);
        self.terminate(Term::Trap { span });

        let test_blocks: Vec<BlockId> = (0..arms.len()).map(|_| self.new_block()).collect();
        let arm_blocks: Vec<BlockId> = (0..arms.len()).map(|_| self.new_block()).collect();
        let entry = test_blocks.first().copied().unwrap_or(trap);
        self.set_current(entry);
        for (i, arm) in arms.iter().enumerate() {
            if i > 0 {
                self.set_current(test_blocks[i]);
            }
            let next = test_blocks.get(i + 1).copied().unwrap_or(trap);
            if arm.is_else {
                self.terminate(Term::Goto(arm_blocks[i]));
            } else {
                self.emit_pattern_tests(&arm.patterns, &place, arm_blocks[i], next, span);
            }
        }

        for (i, arm) in arms.iter().enumerate() {
            let next = test_blocks.get(i + 1).copied().unwrap_or(trap);
            self.set_current(arm_blocks[i]);
            self.push_scope();
            for p in &arm.patterns {
                self.bind_from_place(p, &place, UseMode::Move);
            }
            if self.is_linear(&self.place_type(&place)) {
                self.emit(CfgStmt::ConsumeUnion {
                    place: place.clone(),
                    span,
                });
            }
            if let Some(g) = &arm.guard {
                let body_b = self.new_block();
                let cond = self.lower_expr(g);
                self.terminate(Term::Branch {
                    cond,
                    then_b: body_b,
                    else_b: next,
                    span: g.span,
                });
                self.set_current(body_b);
            }
            match &arm.body {
                ArmBody::Block(b) => self.lower_block(b, Some(join)),
                ArmBody::Expr(e) => {
                    let v = self.lower_expr(e);
                    if let Some(dest) = result {
                        self.assign_operand(
                            Place {
                                local: dest,
                                proj: Vec::new(),
                            },
                            v,
                            span,
                        );
                    }
                }
            }
            if self.current.is_some() {
                self.goto(join);
            }
            self.pop_scope();
        }
        self.set_current(join);
    }

    fn emit_pattern_tests(
        &mut self,
        patterns: &[Pattern],
        place: &Place,
        arm: BlockId,
        failure: BlockId,
        span: Span,
    ) {
        if patterns.is_empty() {
            self.terminate(Term::Goto(failure));
            return;
        }
        for (i, pattern) in patterns.iter().enumerate() {
            let last = i + 1 == patterns.len();
            let fail = if last { failure } else { self.new_block() };
            if self.emit_pattern_test(pattern, place, arm, fail, span) {
                return;
            }
            if !last {
                self.set_current(fail);
            }
        }
    }

    /// Returns true when the pattern is irrefutable.
    fn emit_pattern_test(
        &mut self,
        pattern: &Pattern,
        place: &Place,
        arm: BlockId,
        failure: BlockId,
        span: Span,
    ) -> bool {
        match pattern {
            Pattern::Wild(_) | Pattern::Bind { .. } => {
                self.terminate(Term::Goto(arm));
                true
            }
            Pattern::Variant { name, .. } => {
                self.terminate(Term::Switch {
                    value: place.clone(),
                    cases: vec![(name.clone(), arm)],
                    default: failure,
                    span,
                });
                false
            }
            Pattern::Literal(Expr {
                kind: ExprKind::Bool(b),
                ..
            }) => {
                let (then_b, else_b) = if *b { (arm, failure) } else { (failure, arm) };
                self.terminate(Term::Branch {
                    cond: Operand::Place(place.clone()),
                    then_b,
                    else_b,
                    span,
                });
                false
            }
            Pattern::Literal(Expr {
                kind: ExprKind::Int {
                    value, negative, ..
                },
                ..
            }) => {
                let n = if *negative {
                    -(*value as i128)
                } else {
                    *value as i128
                };
                let cond = self.add_temp(Type::Bool, span);
                self.emit(CfgStmt::Assign {
                    place: Place {
                        local: cond,
                        proj: Vec::new(),
                    },
                    value: Rvalue::Binary {
                        op: BinOp::Eq,
                        lhs: Operand::Place(place.clone()),
                        rhs: Operand::Const(ConstVal::Int(n)),
                    },
                    span,
                });
                self.terminate(Term::Branch {
                    cond: Operand::Place(Place {
                        local: cond,
                        proj: Vec::new(),
                    }),
                    then_b: arm,
                    else_b: failure,
                    span,
                });
                false
            }
            Pattern::Range {
                lo, hi, inclusive, ..
            } => {
                let (Some(lo), Some(hi)) = (pattern_const(lo), pattern_const(hi)) else {
                    self.terminate(Term::Goto(failure));
                    return false;
                };
                let lower = self.add_temp(Type::Bool, span);
                self.emit(CfgStmt::Assign {
                    place: Place {
                        local: lower,
                        proj: Vec::new(),
                    },
                    value: Rvalue::Binary {
                        op: BinOp::Ge,
                        lhs: Operand::Place(place.clone()),
                        rhs: Operand::Const(ConstVal::Int(lo)),
                    },
                    span,
                });
                let upper = self.add_temp(Type::Bool, span);
                self.emit(CfgStmt::Assign {
                    place: Place {
                        local: upper,
                        proj: Vec::new(),
                    },
                    value: Rvalue::Binary {
                        op: if *inclusive { BinOp::Le } else { BinOp::Lt },
                        lhs: Operand::Place(place.clone()),
                        rhs: Operand::Const(ConstVal::Int(hi)),
                    },
                    span,
                });
                let both = self.add_temp(Type::Bool, span);
                self.emit(CfgStmt::Assign {
                    place: Place {
                        local: both,
                        proj: Vec::new(),
                    },
                    value: Rvalue::Binary {
                        op: BinOp::And,
                        lhs: Operand::Place(Place {
                            local: lower,
                            proj: Vec::new(),
                        }),
                        rhs: Operand::Place(Place {
                            local: upper,
                            proj: Vec::new(),
                        }),
                    },
                    span,
                });
                self.terminate(Term::Branch {
                    cond: Operand::Place(Place {
                        local: both,
                        proj: Vec::new(),
                    }),
                    then_b: arm,
                    else_b: failure,
                    span,
                });
                false
            }
            _ => {
                self.terminate(Term::Goto(arm));
                true
            }
        }
    }

    // ------------------------------------------------------------ expressions

    fn lower_expr(&mut self, e: &Expr) -> Operand {
        let saved = self.current_span;
        self.current_span = e.span;
        let result = self.lower_expr_inner(e);
        self.current_span = saved;
        result
    }

    fn lower_expr_inner(&mut self, e: &Expr) -> Operand {
        match &e.kind {
            ExprKind::Int {
                value, negative, ..
            } => {
                let n = if *negative {
                    -(*value as i128)
                } else {
                    *value as i128
                };
                Operand::Const(ConstVal::Int(n))
            }
            ExprKind::Float(v) => Operand::Const(ConstVal::Float(*v)),
            ExprKind::Bool(b) => Operand::Const(ConstVal::Bool(*b)),
            ExprKind::Str(b) => Operand::Const(ConstVal::Str(b.clone())),
            ExprKind::CStr(_) => Operand::Unit,
            ExprKind::Ident(name) => match self.local_by_name.get(name) {
                Some(id) => Operand::Place(Place {
                    local: *id,
                    proj: Vec::new(),
                }),
                None => self
                    .program
                    .const_value_in(&self.module, name)
                    .and_then(|c| c.value.clone())
                    .map(Operand::Const)
                    .unwrap_or(Operand::Unit),
            },
            ExprKind::Path(_) => Operand::Unit,
            ExprKind::Paren(inner) => self.lower_expr(inner),
            ExprKind::Field { base, name, .. } => {
                if let ExprKind::Ident(alias) = &base.kind
                    && let Some(crate::check::ResolvedName::Local(canonical)) = self
                        .program
                        .resolve_qualified_name(&self.module, alias, name)
                    && let Some(c) = self.program.consts.get(&canonical)
                    && let Some(v) = &c.value
                {
                    return Operand::Const(v.clone());
                }
                if let ExprKind::Path(p) = &base.kind {
                    let ty = self.resolve_path_ty(p);
                    if let Type::Enum(..) = &ty
                        && let Some(payload) = self.program.defs.variant(&ty, name)
                    {
                        let dest = self.add_temp(self.type_of(e), e.span);
                        let kind = AggKind::Enum {
                            ty: ty.to_string(),
                            variant: name.clone(),
                        };
                        let mut ops = Vec::new();
                        if payload.is_some() {
                            // A payload needs `Variant.payload`; leave it to
                            // the call form and record no payload here.
                        }
                        self.emit(CfgStmt::Assign {
                            place: Place {
                                local: dest,
                                proj: Vec::new(),
                            },
                            value: Rvalue::Aggregate {
                                kind,
                                fields: std::mem::take(&mut ops),
                            },
                            span: e.span,
                        });
                        return Operand::Place(Place {
                            local: dest,
                            proj: Vec::new(),
                        });
                    }
                }
                let mut place = self.lower_place(base);
                if matches!(self.place_type(&place), Type::Borrow { .. }) {
                    place.proj.push(Proj::Deref);
                }
                place.proj.push(Proj::Field(name.clone()));
                Operand::Place(place)
            }
            ExprKind::TupleField { base, index, .. } => {
                let mut place = self.lower_place(base);
                place.proj.push(Proj::Field(index.to_string()));
                Operand::Place(place)
            }
            ExprKind::Index { .. } => Operand::Place(self.lower_place(e)),
            ExprKind::Slice { base, lo, hi, .. } => {
                let mut place = self.lower_place(base);
                let _ = self.lower_expr(lo);
                let _ = self.lower_expr(hi);
                place.proj.push(Proj::Index);
                let ty = self.type_of(e);
                let dest = self.add_temp(ty, e.span);
                self.emit(CfgStmt::Assign {
                    place: Place {
                        local: dest,
                        proj: Vec::new(),
                    },
                    value: Rvalue::Slice {
                        place,
                        mutable: false,
                    },
                    span: e.span,
                });
                Operand::Place(Place {
                    local: dest,
                    proj: Vec::new(),
                })
            }
            ExprKind::Deref { base, .. } => {
                let mut place = self.lower_place(base);
                place.proj.push(Proj::Deref);
                Operand::Place(place)
            }
            ExprKind::Unary {
                op: UnOp::Borrow | UnOp::BorrowMut,
                expr: inner,
                ..
            } => {
                let mutable = matches!(
                    e.kind,
                    ExprKind::Unary {
                        op: UnOp::BorrowMut,
                        ..
                    }
                );
                self.lower_borrow(inner, mutable, false, e.span)
            }
            ExprKind::Unary {
                op: UnOp::Try,
                expr: inner,
                op_span,
            } => self.lower_try(inner, *op_span, e.span),
            ExprKind::Unary {
                op, expr: inner, ..
            } => {
                let opd = self.lower_expr(inner);
                let opd = self.consume_operand(opd, UseMode::Move);
                let ty = self.type_of(e);
                let dest = self.add_temp(ty, e.span);
                self.emit(CfgStmt::Assign {
                    place: Place {
                        local: dest,
                        proj: Vec::new(),
                    },
                    value: Rvalue::Unary {
                        op: *op,
                        operand: opd,
                    },
                    span: e.span,
                });
                Operand::Place(Place {
                    local: dest,
                    proj: Vec::new(),
                })
            }
            ExprKind::Binary {
                op: BinOp::And | BinOp::Or,
                lhs,
                rhs,
                ..
            } => {
                let dest = self.add_temp(Type::Bool, e.span);
                let rhs_b = self.new_block();
                let join = self.new_block();
                let lhs_op = self.lower_expr(lhs);
                self.terminate(Term::Branch {
                    cond: lhs_op,
                    then_b: rhs_b,
                    else_b: join,
                    span: e.span,
                });
                self.set_current(rhs_b);
                let rhs_op = self.lower_expr(rhs);
                self.assign_operand(
                    Place {
                        local: dest,
                        proj: Vec::new(),
                    },
                    rhs_op,
                    e.span,
                );
                self.goto(join);
                self.set_current(join);
                Operand::Place(Place {
                    local: dest,
                    proj: Vec::new(),
                })
            }
            ExprKind::Binary {
                op,
                lhs,
                rhs,
                op_span,
                ..
            } => {
                let l = self.lower_expr(lhs);
                let l = self.consume_operand(l, UseMode::Move);
                let r = self.lower_expr(rhs);
                let r = self.consume_operand(r, UseMode::Move);
                let dest = self.add_temp(self.type_of(e), e.span);
                let check = match op {
                    BinOp::Add | BinOp::Sub | BinOp::Mul => Some(CheckKind::Overflow),
                    BinOp::Div | BinOp::Rem => Some(CheckKind::Division),
                    BinOp::Shl | BinOp::Shr => Some(CheckKind::Shift),
                    _ => None,
                };
                if let Some(kind) = check {
                    self.emit(CfgStmt::Check {
                        check: CheckStmt {
                            kind,
                            span: *op_span,
                            lhs: Some(l.clone()),
                            rhs: Some(r.clone()),
                            bounds: None,
                        },
                    });
                }
                self.emit(CfgStmt::Assign {
                    place: Place {
                        local: dest,
                        proj: Vec::new(),
                    },
                    value: Rvalue::Binary {
                        op: *op,
                        lhs: l,
                        rhs: r,
                    },
                    span: e.span,
                });
                Operand::Place(Place {
                    local: dest,
                    proj: Vec::new(),
                })
            }
            ExprKind::Call { callee, args, .. } => self.lower_call_expr_parts(callee, args, e.span),
            ExprKind::TypeApply { base, .. } => self.lower_expr(base),
            ExprKind::Cast { expr: inner, .. } => {
                let opd = self.lower_expr(inner);
                let opd = self.consume_operand(opd, UseMode::Move);
                let dest = self.add_temp(self.type_of(e), e.span);
                self.emit(CfgStmt::Assign {
                    place: Place {
                        local: dest,
                        proj: Vec::new(),
                    },
                    value: Rvalue::Cast { operand: opd },
                    span: e.span,
                });
                Operand::Place(Place {
                    local: dest,
                    proj: Vec::new(),
                })
            }
            ExprKind::StructLit { fields, .. } => {
                let mut ops = Vec::new();
                for f in fields {
                    let op = self.lower_expr(&f.value);
                    ops.push(self.consume_operand(op, UseMode::Move));
                }
                let ty = self.type_of(e);
                let name = ty.as_struct().unwrap_or("").to_string();
                let dest = self.add_temp(ty, e.span);
                self.emit(CfgStmt::Assign {
                    place: Place {
                        local: dest,
                        proj: Vec::new(),
                    },
                    value: Rvalue::Aggregate {
                        kind: AggKind::Struct(name),
                        fields: ops,
                    },
                    span: e.span,
                });
                Operand::Place(Place {
                    local: dest,
                    proj: Vec::new(),
                })
            }
            ExprKind::StructShorthand { fields, .. } => {
                let mut ops = Vec::new();
                for f in fields {
                    let op = self.lower_expr(&f.value);
                    ops.push(self.consume_operand(op, UseMode::Move));
                }
                let ty = self.type_of(e);
                let name = ty.as_struct().unwrap_or("").to_string();
                let dest = self.add_temp(ty, e.span);
                self.emit(CfgStmt::Assign {
                    place: Place {
                        local: dest,
                        proj: Vec::new(),
                    },
                    value: Rvalue::Aggregate {
                        kind: AggKind::Struct(name),
                        fields: ops,
                    },
                    span: e.span,
                });
                Operand::Place(Place {
                    local: dest,
                    proj: Vec::new(),
                })
            }
            ExprKind::EnumLit {
                variant, payload, ..
            } => {
                let ty = self.type_of(e);
                let kind = match &ty {
                    Type::Result(..) if variant == "ok" => AggKind::ResultOk,
                    Type::Result(..) if variant == "err" => AggKind::ResultErr,
                    Type::Optional(_) if variant == "some" => AggKind::OptionalSome,
                    Type::Optional(_) => AggKind::OptionalNone,
                    _ => AggKind::Enum {
                        ty: ty.to_string(),
                        variant: variant.clone(),
                    },
                };
                let mut ops = Vec::new();
                if let Some(p) = payload {
                    let op = self.lower_expr(p);
                    ops.push(self.consume_operand(op, UseMode::Move));
                }
                let dest = self.add_temp(ty, e.span);
                self.emit(CfgStmt::Assign {
                    place: Place {
                        local: dest,
                        proj: Vec::new(),
                    },
                    value: Rvalue::Aggregate { kind, fields: ops },
                    span: e.span,
                });
                Operand::Place(Place {
                    local: dest,
                    proj: Vec::new(),
                })
            }
            ExprKind::ArrayLit { elems, .. } => {
                let mut ops = Vec::new();
                for x in elems {
                    let op = self.lower_expr(x);
                    ops.push(self.consume_operand(op, UseMode::Move));
                }
                let dest = self.add_temp(self.type_of(e), e.span);
                self.emit(CfgStmt::Assign {
                    place: Place {
                        local: dest,
                        proj: Vec::new(),
                    },
                    value: Rvalue::Aggregate {
                        kind: AggKind::Array,
                        fields: ops,
                    },
                    span: e.span,
                });
                Operand::Place(Place {
                    local: dest,
                    proj: Vec::new(),
                })
            }
            ExprKind::Tuple { elems, .. } => {
                let mut ops = Vec::new();
                for x in elems {
                    let op = self.lower_expr(x);
                    ops.push(self.consume_operand(op, UseMode::Move));
                }
                let dest = self.add_temp(self.type_of(e), e.span);
                self.emit(CfgStmt::Assign {
                    place: Place {
                        local: dest,
                        proj: Vec::new(),
                    },
                    value: Rvalue::Aggregate {
                        kind: AggKind::Tuple,
                        fields: ops,
                    },
                    span: e.span,
                });
                Operand::Place(Place {
                    local: dest,
                    proj: Vec::new(),
                })
            }
            ExprKind::Range { .. } => Operand::Unit,
            ExprKind::If {
                cond,
                then,
                otherwise,
                span,
            } => {
                let dest = self.add_temp(self.type_of(e), *span);
                let then_b = self.new_block();
                let else_b = self.new_block();
                let join = self.new_block();
                if let Some(pattern) = &cond.let_pattern {
                    let value = self.lower_expr(&cond.expr);
                    let place = self.operand_place(value, UseMode::Copy);
                    self.switch_to(pattern, &place, then_b, else_b, cond.span);
                    self.set_current(then_b);
                    self.bind_from_place(pattern, &place, UseMode::Copy);
                } else {
                    let cond_op = self.lower_expr(&cond.expr);
                    self.terminate(Term::Branch {
                        cond: cond_op,
                        then_b,
                        else_b,
                        span: cond.span,
                    });
                    self.set_current(then_b);
                }
                let v = self.lower_expr(then);
                self.assign_operand(
                    Place {
                        local: dest,
                        proj: Vec::new(),
                    },
                    v,
                    then.span,
                );
                self.goto(join);
                self.set_current(else_b);
                let v = self.lower_expr(otherwise);
                self.assign_operand(
                    Place {
                        local: dest,
                        proj: Vec::new(),
                    },
                    v,
                    otherwise.span,
                );
                self.goto(join);
                self.set_current(join);
                Operand::Place(Place {
                    local: dest,
                    proj: Vec::new(),
                })
            }
            ExprKind::Match {
                scrutinee,
                arms,
                span,
            } => {
                let dest = self.add_temp(self.type_of(e), *span);
                self.lower_match(scrutinee, arms, Some(dest), false, *span);
                Operand::Place(Place {
                    local: dest,
                    proj: Vec::new(),
                })
            }
            ExprKind::Catch {
                value,
                binding,
                handler,
                ..
            } => self.lower_catch(value, binding.as_deref(), handler, false, e),
            ExprKind::Orelse { value, handler, .. } => {
                self.lower_catch(value, None, handler, true, e)
            }
            ExprKind::Builtin {
                name,
                type_arg,
                args,
            } => self.lower_builtin(name, type_arg.as_deref(), args, e),
            ExprKind::Spawn { call, scope, span } => {
                self.lower_spawn(call, scope, Some(self.type_of(e)), *span)
            }
            ExprKind::Dispatch { call, .. } => self.lower_call_expr(call),
            ExprKind::Unsafe { body, .. } => self.lower_expr(body),
            ExprKind::Closure { .. } => Operand::Unit,
            ExprKind::Poison => Operand::Unit,
        }
    }

    fn lower_place(&mut self, e: &Expr) -> Place {
        match &e.kind {
            ExprKind::Ident(name) => match self.local_by_name.get(name) {
                Some(id) => Place {
                    local: *id,
                    proj: Vec::new(),
                },
                None => self.fallback_place(e),
            },
            ExprKind::Paren(inner) => self.lower_place(inner),
            ExprKind::Field { base, name, .. } => {
                let mut place = self.lower_place(base);
                if matches!(self.place_type(&place), Type::Borrow { .. }) {
                    place.proj.push(Proj::Deref);
                }
                place.proj.push(Proj::Field(name.clone()));
                place
            }
            ExprKind::TupleField { base, index, .. } => {
                let mut place = self.lower_place(base);
                place.proj.push(Proj::Field(index.to_string()));
                place
            }
            ExprKind::Index { base, index, .. } => {
                let mut place = self.lower_place(base);
                let idx = self.lower_expr(index);
                self.emit(CfgStmt::Check {
                    check: CheckStmt {
                        kind: CheckKind::Bounds,
                        span: e.span,
                        lhs: None,
                        rhs: None,
                        bounds: Some(BoundsCheck {
                            base: place.clone(),
                            index: idx,
                        }),
                    },
                });
                if matches!(self.place_type(&place), Type::Slice { .. }) {
                    place.proj.push(Proj::Deref);
                }
                place.proj.push(Proj::Index);
                place
            }
            ExprKind::Deref { base, .. } => {
                let mut place = self.lower_place(base);
                place.proj.push(Proj::Deref);
                place
            }
            _ => self.fallback_place(e),
        }
    }

    fn fallback_place(&mut self, e: &Expr) -> Place {
        let op = self.lower_expr(e);
        self.operand_place(op, UseMode::Copy)
    }

    fn operand_place(&mut self, op: Operand, mode: UseMode) -> Place {
        match op {
            Operand::Place(p) => p,
            Operand::Const(c) => {
                let ty = match &c {
                    ConstVal::Int(_) => Type::i64(),
                    ConstVal::Float(_) => Type::Float(crate::token::ScalarTy::F64),
                    ConstVal::Bool(_) => Type::Bool,
                    ConstVal::Str(_) => Type::Slice {
                        mutable: false,
                        elem: Box::new(Type::u8()),
                    },
                };
                let span = self.current_span;
                let dest = self.add_temp(ty, span);
                self.emit(CfgStmt::Assign {
                    place: Place {
                        local: dest,
                        proj: Vec::new(),
                    },
                    value: Rvalue::Constant,
                    span,
                });
                let _ = mode;
                Place {
                    local: dest,
                    proj: Vec::new(),
                }
            }
            Operand::Unit => {
                let dest = self.add_temp(Type::Void, Span::default());
                Place {
                    local: dest,
                    proj: Vec::new(),
                }
            }
        }
    }

    fn consume_operand(&mut self, op: Operand, mode: UseMode) -> Operand {
        match op {
            Operand::Place(place) => {
                let ty = self.place_type(&place);
                let effective = if self.is_copy(&ty) {
                    UseMode::Copy
                } else {
                    mode
                };
                let span = self.current_span;
                if effective == UseMode::Move {
                    self.note_move(&place, span);
                }
                let dest = self.add_temp(ty, span);
                self.emit(CfgStmt::Assign {
                    place: Place {
                        local: dest,
                        proj: Vec::new(),
                    },
                    value: Rvalue::Use {
                        place,
                        mode: effective,
                    },
                    span,
                });
                Operand::Place(Place {
                    local: dest,
                    proj: Vec::new(),
                })
            }
            other => other,
        }
    }

    fn make_rvalue(&mut self, op: Operand) -> Rvalue {
        match op {
            Operand::Place(place) => {
                let ty = self.place_type(&place);
                let mode = if self.is_copy(&ty) {
                    UseMode::Copy
                } else {
                    self.note_move(&place, self.current_span);
                    UseMode::Move
                };
                Rvalue::Use { place, mode }
            }
            Operand::Const(_) => Rvalue::Constant,
            Operand::Unit => Rvalue::Constant,
        }
    }

    fn assign_operand(&mut self, dest: Place, op: Operand, span: Span) {
        match op {
            Operand::Place(place) => {
                let ty = self.place_type(&place);
                let mode = if self.is_copy(&ty) {
                    UseMode::Copy
                } else {
                    UseMode::Move
                };
                if mode == UseMode::Move {
                    self.note_move(&place, span);
                }
                self.emit(CfgStmt::Assign {
                    place: dest,
                    value: Rvalue::Use { place, mode },
                    span,
                });
            }
            Operand::Const(_) => self.emit(CfgStmt::Assign {
                place: dest,
                value: Rvalue::Constant,
                span,
            }),
            Operand::Unit => {}
        }
    }

    fn lower_borrow(
        &mut self,
        inner: &Expr,
        mutable: bool,
        two_phase: bool,
        span: Span,
    ) -> Operand {
        let place = if let ExprKind::Slice { base, lo, hi, .. } = &inner.kind {
            let mut p = self.lower_place(base);
            let _ = self.lower_expr(lo);
            let _ = self.lower_expr(hi);
            p.proj.push(Proj::Index);
            p
        } else {
            self.lower_place(inner)
        };
        let kind = if mutable {
            BorrowKind::Exclusive
        } else {
            BorrowKind::Shared
        };
        let loan = self.add_loan(&place, kind, two_phase, span);
        let ty = self.expr_types.get(&span).cloned().unwrap_or_else(|| {
            if let ExprKind::Slice { .. } = &inner.kind {
                Type::Slice {
                    mutable,
                    elem: Box::new(Type::Error),
                }
            } else {
                Type::Borrow {
                    mutable,
                    inner: Box::new(self.place_type(&place)),
                }
            }
        });
        let dest = self.add_temp(ty, span);
        self.emit(CfgStmt::Assign {
            place: Place {
                local: dest,
                proj: Vec::new(),
            },
            value: Rvalue::Borrow {
                loan,
                place,
                mutable,
                two_phase,
            },
            span,
        });
        Operand::Place(Place {
            local: dest,
            proj: Vec::new(),
        })
    }

    fn lower_try(&mut self, inner: &Expr, op_span: Span, span: Span) -> Operand {
        let value = self.lower_expr(inner);
        let place = self.operand_place(value, UseMode::Move);
        let out_ty = self.type_of(&Expr {
            kind: ExprKind::Unary {
                op: UnOp::Try,
                expr: Box::new(inner.clone()),
                op_span,
            },
            span,
        });
        let out = self.add_temp(out_ty, span);
        let ok_b = self.new_block();
        let err_b = self.new_block();
        let join = self.new_block();
        self.terminate(Term::Switch {
            value: place.clone(),
            cases: vec![("ok".to_string(), ok_b), ("err".to_string(), err_b)],
            default: err_b,
            span,
        });
        self.set_current(ok_b);
        let mut payload = place.clone();
        payload.proj.push(Proj::Variant("ok".to_string()));
        self.assign_operand(
            Place {
                local: out,
                proj: Vec::new(),
            },
            Operand::Place(payload),
            span,
        );
        self.emit(CfgStmt::ConsumeUnion {
            place: place.clone(),
            span,
        });
        self.goto(join);
        self.set_current(err_b);
        let mut err_place = place.clone();
        err_place.proj.push(Proj::Variant("err".to_string()));
        let converted = self.consume_operand(Operand::Place(err_place), UseMode::Move);
        self.emit(CfgStmt::ConsumeUnion {
            place: place.clone(),
            span,
        });
        let err_dest = self.add_temp(self.function_error_type(), span);
        self.emit(CfgStmt::Assign {
            place: Place {
                local: err_dest,
                proj: Vec::new(),
            },
            value: Rvalue::ConvertError { operand: converted },
            span,
        });
        self.cleanup_all(true);
        if self.current.is_some() {
            self.terminate(Term::Return {
                value: Some(Operand::Place(Place {
                    local: err_dest,
                    proj: Vec::new(),
                })),
                is_error: true,
                span,
            });
        }
        self.set_current(join);
        Operand::Place(Place {
            local: out,
            proj: Vec::new(),
        })
    }

    fn lower_catch(
        &mut self,
        value: &Expr,
        binding: Option<&str>,
        handler: &Handler,
        optional: bool,
        e: &Expr,
    ) -> Operand {
        let value_op = self.lower_expr(value);
        let place = self.operand_place(value_op, UseMode::Move);
        let out = self.add_temp(self.type_of(e), e.span);
        let good_b = self.new_block();
        let bad_b = self.new_block();
        let join = self.new_block();
        let (good, bad) = if optional {
            ("some", "none")
        } else {
            ("ok", "err")
        };
        self.terminate(Term::Switch {
            value: place.clone(),
            cases: vec![(good.to_string(), good_b), (bad.to_string(), bad_b)],
            default: bad_b,
            span: e.span,
        });
        self.set_current(good_b);
        let mut payload = place.clone();
        payload.proj.push(Proj::Variant(good.to_string()));
        self.assign_operand(
            Place {
                local: out,
                proj: Vec::new(),
            },
            Operand::Place(payload),
            e.span,
        );
        self.emit(CfgStmt::ConsumeUnion {
            place: place.clone(),
            span: e.span,
        });
        self.goto(join);
        self.set_current(bad_b);
        if optional {
            self.emit(CfgStmt::ConsumeUnion {
                place: place.clone(),
                span: e.span,
            });
            let v = self.lower_expr_handler(handler, None);
            if let Some(v) = v {
                self.assign_operand(
                    Place {
                        local: out,
                        proj: Vec::new(),
                    },
                    v,
                    e.span,
                );
            }
        } else {
            let mut err_place = place.clone();
            err_place.proj.push(Proj::Variant("err".to_string()));
            let err_op = self.consume_operand(Operand::Place(err_place), UseMode::Move);
            self.emit(CfgStmt::ConsumeUnion {
                place: place.clone(),
                span: e.span,
            });
            let v = self.lower_expr_handler(handler, binding);
            let _ = err_op;
            if let Some(v) = v {
                self.assign_operand(
                    Place {
                        local: out,
                        proj: Vec::new(),
                    },
                    v,
                    e.span,
                );
            }
        }
        if self.current.is_some() {
            self.goto(join);
        }
        self.set_current(join);
        Operand::Place(Place {
            local: out,
            proj: Vec::new(),
        })
    }

    fn lower_expr_handler(&mut self, handler: &Handler, binding: Option<&str>) -> Option<Operand> {
        match handler {
            Handler::Expr(e) => Some(self.lower_expr(e)),
            Handler::Block(b) => {
                self.push_scope();
                if let Some(name) = binding {
                    let _ = self.declare_local(
                        name.to_string(),
                        Type::External("Error".to_string()),
                        b.span,
                        false,
                    );
                }
                self.lower_stmts(&b.stmts);
                if self.current.is_some() {
                    let scope = self.scopes[self.current_scope].clone();
                    self.finish_scope(&scope, false, false);
                }
                self.pop_scope();
                None
            }
        }
    }

    fn lower_builtin(
        &mut self,
        name: &str,
        _type_arg: Option<&Ty>,
        args: &[Expr],
        e: &Expr,
    ) -> Operand {
        match name {
            "forget" => {
                if let Some(a) = args.first() {
                    let place = self.lower_place(a);
                    self.emit(CfgStmt::Use {
                        place,
                        mode: UseMode::Move,
                        span: a.span,
                    });
                }
                Operand::Unit
            }
            "replace" => {
                if let Some(place_expr) = args.first() {
                    let place = match &place_expr.kind {
                        ExprKind::Unary {
                            op: UnOp::BorrowMut | UnOp::Borrow,
                            expr,
                            ..
                        } => self.lower_place(expr),
                        _ => self.lower_place(place_expr),
                    };
                    let old = self.add_temp(self.type_of(e), e.span);
                    self.emit(CfgStmt::Assign {
                        place: Place {
                            local: old,
                            proj: Vec::new(),
                        },
                        value: Rvalue::Use {
                            place: place.clone(),
                            mode: UseMode::Move,
                        },
                        span: e.span,
                    });
                    if let Some(v) = args.get(1) {
                        let op = self.lower_expr(v);
                        self.assign_operand(place, op, e.span);
                    }
                    return Operand::Place(Place {
                        local: old,
                        proj: Vec::new(),
                    });
                }
                Operand::Unit
            }
            "swap" => {
                for a in args {
                    let _ = self.lower_expr(a);
                }
                Operand::Unit
            }
            "assert" => {
                if let Some(cond) = args.first() {
                    let _ = self.lower_expr(cond);
                    if let Some((base, at_least)) = self.len_assertion(cond)
                        && let Some(block) = self.current
                    {
                        let after = self.cfg.blocks[block].stmts.len();
                        self.cfg.len_facts.push(LenFact {
                            block,
                            after,
                            base,
                            at_least,
                        });
                    }
                }
                Operand::Unit
            }
            "panic" | "unreachable" => {
                self.terminate(Term::Trap { span: e.span });
                Operand::Unit
            }
            "zero" => {
                let dest = self.add_temp(self.type_of(e), e.span);
                self.emit(CfgStmt::Assign {
                    place: Place {
                        local: dest,
                        proj: Vec::new(),
                    },
                    value: Rvalue::Zero,
                    span: e.span,
                });
                Operand::Place(Place {
                    local: dest,
                    proj: Vec::new(),
                })
            }
            _ => {
                let mut ops = Vec::new();
                for a in args {
                    let op = self.lower_expr(a);
                    let ty = self.type_of(a);
                    let mode = if self.is_copy(&ty) {
                        UseMode::Copy
                    } else {
                        UseMode::Move
                    };
                    ops.push(self.consume_operand(op, mode));
                }
                let ty = self.type_of(e);
                if ty.is_void() {
                    return Operand::Unit;
                }
                let dest = self.add_temp(ty, e.span);
                self.emit(CfgStmt::Assign {
                    place: Place {
                        local: dest,
                        proj: Vec::new(),
                    },
                    value: Rvalue::Aggregate {
                        kind: AggKind::Tuple,
                        fields: ops,
                    },
                    span: e.span,
                });
                Operand::Place(Place {
                    local: dest,
                    proj: Vec::new(),
                })
            }
        }
    }

    // --------------------------------------------------------------- calls

    fn lower_call_expr(&mut self, call: &Expr) -> Operand {
        if let ExprKind::Call { callee, args, .. } = &call.kind {
            self.lower_call_expr_parts(callee, args, call.span)
        } else {
            Operand::Unit
        }
    }

    fn lower_call_expr_parts(&mut self, callee: &Expr, args: &[Expr], span: Span) -> Operand {
        let target = self.resolve_callee(callee);
        match target {
            CallTarget::Constructor {
                ty,
                variant,
                has_payload,
            } => {
                let mut ops = Vec::new();
                if has_payload && let Some(p) = args.first() {
                    let op = self.lower_expr(p);
                    ops.push(self.consume_operand(op, UseMode::Move));
                }
                let dest = self.add_temp(ty.clone(), span);
                self.emit(CfgStmt::Assign {
                    place: Place {
                        local: dest,
                        proj: Vec::new(),
                    },
                    value: Rvalue::Aggregate {
                        kind: variant,
                        fields: ops,
                    },
                    span,
                });
                Operand::Place(Place {
                    local: dest,
                    proj: Vec::new(),
                })
            }
            CallTarget::Direct { function, receiver } => {
                let has_receiver = receiver.is_some();
                let mut ops = Vec::new();
                if let Some(recv) = receiver {
                    let recv_op = self.lower_receiver(&recv, &function, span);
                    ops.push(recv_op);
                }
                for (i, a) in args.iter().enumerate() {
                    let pi = if function
                        .decl
                        .params
                        .first()
                        .map(|p| p.is_self())
                        .unwrap_or(false)
                        && has_receiver
                    {
                        i + 1
                    } else {
                        i
                    };
                    let expected = function.signature_params.get(pi).cloned();
                    let op = self.lower_argument(a, expected.as_ref(), span);
                    ops.push(op);
                }
                let ty = self.type_of(&Expr {
                    kind: ExprKind::Call {
                        callee: Box::new(callee.clone()),
                        args: args.to_vec(),
                        args_span: span,
                    },
                    span,
                });
                let dest = if ty.is_void() {
                    None
                } else {
                    Some(self.add_temp(ty, span))
                };
                let then_b = self.new_block();
                self.terminate(Term::Call {
                    function: Some(function.key.clone()),
                    args: ops,
                    dest: dest.map(|id| Place {
                        local: id,
                        proj: Vec::new(),
                    }),
                    then_b,
                    span,
                });
                self.set_current(then_b);
                dest.map(|id| {
                    Operand::Place(Place {
                        local: id,
                        proj: Vec::new(),
                    })
                })
                .unwrap_or(Operand::Unit)
            }
            CallTarget::Indirect => {
                let mut ops = Vec::new();
                for a in args {
                    let op = self.lower_expr(a);
                    let ty = self.type_of(a);
                    let mode = if self.is_copy(&ty) {
                        UseMode::Copy
                    } else {
                        UseMode::Move
                    };
                    ops.push(self.consume_operand(op, mode));
                }
                let ty = self.type_of(&Expr {
                    kind: ExprKind::Call {
                        callee: Box::new(callee.clone()),
                        args: args.to_vec(),
                        args_span: span,
                    },
                    span,
                });
                let dest = if ty.is_void() {
                    None
                } else {
                    Some(self.add_temp(ty, span))
                };
                let then_b = self.new_block();
                self.terminate(Term::Call {
                    function: None,
                    args: ops,
                    dest: dest.map(|id| Place {
                        local: id,
                        proj: Vec::new(),
                    }),
                    then_b,
                    span,
                });
                self.set_current(then_b);
                dest.map(|id| {
                    Operand::Place(Place {
                        local: id,
                        proj: Vec::new(),
                    })
                })
                .unwrap_or(Operand::Unit)
            }
        }
    }

    fn lower_receiver(&mut self, recv: &Expr, function: &Function, span: Span) -> Operand {
        let self_ty = function.signature_params.first().cloned();
        match self_ty {
            Some(Type::Borrow { mutable, .. }) => self.lower_borrow(recv, mutable, true, recv.span),
            Some(Type::Slice { mutable, .. }) => {
                let place = self.lower_place(recv);
                let kind = if mutable {
                    BorrowKind::Exclusive
                } else {
                    BorrowKind::Shared
                };
                let loan = self.add_loan(&place, kind, true, span);
                let ty = Type::Borrow {
                    mutable,
                    inner: Box::new(self.place_type(&place)),
                };
                let dest = self.add_temp(ty, span);
                self.emit(CfgStmt::Assign {
                    place: Place {
                        local: dest,
                        proj: Vec::new(),
                    },
                    value: Rvalue::Borrow {
                        loan,
                        place,
                        mutable,
                        two_phase: true,
                    },
                    span,
                });
                Operand::Place(Place {
                    local: dest,
                    proj: Vec::new(),
                })
            }
            _ => {
                let op = self.lower_expr(recv);
                self.consume_operand(op, UseMode::Move)
            }
        }
    }

    fn lower_argument(&mut self, arg: &Expr, expected: Option<&Type>, span: Span) -> Operand {
        if let ExprKind::Unary {
            op: UnOp::Borrow | UnOp::BorrowMut,
            expr: inner,
            ..
        } = &arg.kind
        {
            let mutable = matches!(
                arg.kind,
                ExprKind::Unary {
                    op: UnOp::BorrowMut,
                    ..
                }
            );
            return self.lower_borrow(inner, mutable, true, arg.span);
        }
        match expected {
            Some(Type::Borrow { mutable, .. }) => {
                let mut place = self.lower_place(arg);
                place.proj.push(Proj::Deref);
                self.lower_borrow_place(place, *mutable, true, span)
            }
            Some(Type::Slice { mutable, .. }) if *mutable => {
                let mut place = self.lower_place(arg);
                place.proj.push(Proj::Deref);
                self.lower_borrow_place(place, true, true, span)
            }
            _ => {
                let op = self.lower_expr(arg);
                let ty = expected.cloned().unwrap_or_else(|| self.type_of(arg));
                let mode = if self.is_copy(&ty) {
                    UseMode::Copy
                } else {
                    UseMode::Move
                };
                self.consume_operand(op, mode)
            }
        }
    }

    fn lower_borrow_place(
        &mut self,
        place: Place,
        mutable: bool,
        two_phase: bool,
        span: Span,
    ) -> Operand {
        let kind = if mutable {
            BorrowKind::Exclusive
        } else {
            BorrowKind::Shared
        };
        let loan = self.add_loan(&place, kind, two_phase, span);
        let ty = Type::Borrow {
            mutable,
            inner: Box::new(self.place_type(&place)),
        };
        let dest = self.add_temp(ty, span);
        self.emit(CfgStmt::Assign {
            place: Place {
                local: dest,
                proj: Vec::new(),
            },
            value: Rvalue::Borrow {
                loan,
                place,
                mutable,
                two_phase,
            },
            span,
        });
        Operand::Place(Place {
            local: dest,
            proj: Vec::new(),
        })
    }

    fn lower_spawn(
        &mut self,
        call: &Expr,
        scope: &Expr,
        result_ty: Option<Type>,
        span: Span,
    ) -> Operand {
        let scope_place = self.lower_place(scope);
        let ExprKind::Call { callee, args, .. } = &call.kind else {
            return Operand::Unit;
        };
        let target = self.resolve_callee(callee);
        let (function, receiver) = match target {
            CallTarget::Direct { function, receiver } => (Some(function), receiver),
            _ => (None, None),
        };
        let mut ops = Vec::new();
        if let Some(recv) = receiver
            && let Some(f) = &function
        {
            ops.push(self.lower_receiver(&recv, f, span));
        }
        for (i, a) in args.iter().enumerate() {
            let pi = if function
                .as_ref()
                .and_then(|f| f.decl.params.first())
                .map(|p| p.is_self())
                .unwrap_or(false)
            {
                i + 1
            } else {
                i
            };
            let expected = function
                .as_ref()
                .and_then(|f| f.signature_params.get(pi))
                .cloned();
            ops.push(self.lower_argument(a, expected.as_ref(), span));
        }
        let (dest, handle_loan) = match result_ty {
            Some(ty) => {
                let dest = self.add_temp(ty, span);
                let loan = self.add_loan(&scope_place, BorrowKind::Shared, false, span);
                (
                    Some(Place {
                        local: dest,
                        proj: Vec::new(),
                    }),
                    Some(loan),
                )
            }
            None => (None, None),
        };
        let then_b = self.new_block();
        self.terminate(Term::Spawn {
            function: function.map(|f| f.key.clone()),
            args: ops,
            scope: scope_place,
            dest: dest.clone(),
            handle_loan,
            then_b,
            span,
        });
        self.set_current(then_b);
        dest.map(Operand::Place).unwrap_or(Operand::Unit)
    }

    fn resolve_callee(&self, callee: &Expr) -> CallTarget {
        match &callee.kind {
            ExprKind::Ident(name) => {
                if let Some(f) = self.program.function_in(&self.module, name) {
                    CallTarget::Direct {
                        function: f.clone(),
                        receiver: None,
                    }
                } else {
                    CallTarget::Indirect
                }
            }
            ExprKind::Field { base, name, .. } => {
                if let ExprKind::Ident(alias) = &base.kind
                    && let Some(f) = self.program.function_qualified(&self.module, alias, name)
                {
                    return CallTarget::Direct {
                        function: f.clone(),
                        receiver: None,
                    };
                }
                if let ExprKind::Path(p) = &base.kind {
                    let ty = self.resolve_path_ty(p);
                    if let Type::Enum(..) = &ty
                        && let Some(payload) = self.program.defs.variant(&ty, name)
                    {
                        let kind = AggKind::Enum {
                            ty: ty.to_string(),
                            variant: name.clone(),
                        };
                        return CallTarget::Constructor {
                            ty,
                            variant: kind,
                            has_payload: payload.is_some(),
                        };
                    }
                    if let Some(f) = self.program.method(&ty, name) {
                        return CallTarget::Direct {
                            function: f.clone(),
                            receiver: None,
                        };
                    }
                    return CallTarget::Indirect;
                }
                let recv_ty = self.type_of(base);
                if let Some(f) = self.program.method(&recv_ty, name) {
                    return CallTarget::Direct {
                        function: f.clone(),
                        receiver: Some((**base).clone()),
                    };
                }
                CallTarget::Indirect
            }
            ExprKind::TypeApply { base, .. } => self.resolve_callee(base),
            _ => CallTarget::Indirect,
        }
    }

    fn resolve_ty(&self, ty: &Ty) -> Type {
        match ty {
            Ty::Scalar(s, _) => match s {
                crate::token::ScalarTy::Void => Type::Void,
                crate::token::ScalarTy::Never => Type::Never,
                crate::token::ScalarTy::Bool => Type::Bool,
                crate::token::ScalarTy::F32 | crate::token::ScalarTy::F64 => Type::Float(*s),
                _ => Type::Int(*s),
            },
            Ty::Vector(v, _) => Type::Vector(*v),
            Ty::Path(p) => self.resolve_path_ty(p),
            Ty::Borrow { mutable, inner, .. } => Type::Borrow {
                mutable: *mutable,
                inner: Box::new(self.resolve_ty(inner)),
            },
            Ty::Slice { mutable, elem, .. } => Type::Slice {
                mutable: *mutable,
                elem: Box::new(self.resolve_ty(elem)),
            },
            Ty::Array { len, elem, .. } => {
                let n = self.expr_const(len).unwrap_or(0);
                Type::Array(n, Box::new(self.resolve_ty(elem)))
            }
            Ty::Ptr { mutable, inner, .. } => Type::Ptr {
                mutable: *mutable,
                inner: Box::new(self.resolve_ty(inner)),
            },
            Ty::Optional { inner, .. } => Type::Optional(Box::new(self.resolve_ty(inner))),
            Ty::Tuple { elems, .. } => {
                Type::Tuple(elems.iter().map(|t| self.resolve_ty(t)).collect())
            }
            Ty::Fn {
                params, ret, error, ..
            } => Type::Fn {
                params: params.iter().map(|t| self.resolve_ty(t)).collect(),
                ret: Box::new(self.resolve_ty(ret)),
                error: error.as_ref().map(|e| Box::new(self.resolve_ty(e))),
            },
            Ty::Dyn { interface, .. } => Type::Dyn(interface.name().to_string()),
            Ty::Poison(_) => Type::Error,
        }
    }

    fn resolve_path_ty(&self, p: &PathTy) -> Type {
        match self.program.resolve_type_path(&self.module, p) {
            Some(crate::check::ResolvedName::Local(c)) => {
                if self.program.defs.structs.contains_key(&c) {
                    Type::Struct(c, Vec::new())
                } else if self.program.defs.enums.contains_key(&c) {
                    Type::Enum(c, Vec::new())
                } else {
                    Type::External(c)
                }
            }
            Some(crate::check::ResolvedName::External(b)) => Type::External(b),
            None => Type::Error,
        }
    }

    fn expr_const(&self, e: &Expr) -> Option<i64> {
        match &e.kind {
            ExprKind::Int {
                value, negative, ..
            } => Some(if *negative {
                -(*value as i64)
            } else {
                *value as i64
            }),
            ExprKind::Paren(x) => self.expr_const(x),
            _ => None,
        }
    }

    fn len_assertion(&self, cond: &Expr) -> Option<(LocalId, i64)> {
        let ExprKind::Binary { op, lhs, rhs, .. } = &cond.kind else {
            return None;
        };
        let (len_expr, n_expr, gt) = match op {
            BinOp::Ge => (lhs.as_ref(), rhs.as_ref(), false),
            BinOp::Gt => (lhs.as_ref(), rhs.as_ref(), true),
            BinOp::Le => (rhs.as_ref(), lhs.as_ref(), false),
            BinOp::Lt => (rhs.as_ref(), lhs.as_ref(), true),
            _ => return None,
        };
        let ExprKind::Field { base, name, .. } = &len_expr.kind else {
            return None;
        };
        if name != "len" {
            return None;
        }
        let ExprKind::Ident(root) = &base.kind else {
            return None;
        };
        let base = *self.local_by_name.get(root)?;
        let ExprKind::Int {
            value, negative, ..
        } = &n_expr.kind
        else {
            return None;
        };
        let mut n = *value as i64;
        if *negative {
            n = -n;
        }
        if gt {
            n += 1;
        }
        Some((base, n))
    }

    fn function_error_type(&self) -> Type {
        self.function.signature_error.clone().unwrap_or(Type::Error)
    }

    // -------------------------------------------------------------- patterns

    fn bind_pattern(&mut self, pattern: &Pattern, ty: &Type, init: Option<Operand>, span: Span) {
        match pattern {
            Pattern::Bind { name, span: nspan } => {
                let id = self.declare_local(name.clone(), ty.clone(), *nspan, false);
                if let Some(op) = init {
                    self.assign_operand(
                        Place {
                            local: id,
                            proj: Vec::new(),
                        },
                        op,
                        span,
                    );
                }
            }
            Pattern::Wild(_) => {
                if let Some(op) = init {
                    let _ = self.consume_operand(op, UseMode::Move);
                }
            }
            Pattern::Struct { fields, .. } => {
                if let Some(op) = init {
                    let place = self.operand_place(op, UseMode::Move);
                    self.bind_from_place(pattern, &place, UseMode::Move);
                    if self.is_linear(ty) {
                        self.emit(CfgStmt::Use {
                            place,
                            mode: UseMode::Move,
                            span,
                        });
                    }
                }
                let _ = fields;
            }
            Pattern::Tuple { elems, .. } => {
                if let Some(op) = init {
                    let place = self.operand_place(op, UseMode::Move);
                    for (i, p) in elems.iter().enumerate() {
                        let mut fp = place.clone();
                        fp.proj.push(Proj::Field(i.to_string()));
                        let ty = self.place_type(&fp);
                        self.bind_pattern(p, &ty, Some(Operand::Place(fp)), span);
                    }
                }
            }
            Pattern::Variant { payload, .. } => {
                if let Some(op) = init {
                    let place = self.operand_place(op, UseMode::Move);
                    self.bind_from_place(pattern, &place, UseMode::Move);
                }
                let _ = payload;
            }
            Pattern::Literal(_) | Pattern::Range { .. } | Pattern::Poison => {}
        }
    }

    fn bind_from_place(&mut self, pattern: &Pattern, place: &Place, mode: UseMode) {
        match pattern {
            Pattern::Bind { name, span } => {
                let ty = self.place_type(place);
                let id = self.declare_local(name.clone(), ty, *span, false);
                self.emit(CfgStmt::Assign {
                    place: Place {
                        local: id,
                        proj: Vec::new(),
                    },
                    value: Rvalue::Use {
                        place: place.clone(),
                        mode,
                    },
                    span: *span,
                });
            }
            Pattern::Wild(_) => {}
            Pattern::Struct { fields, .. } => {
                for f in fields {
                    let mut fp = place.clone();
                    fp.proj.push(Proj::Field(f.name.clone()));
                    match &f.pattern {
                        Some(p) => self.bind_from_place(p, &fp, mode),
                        None => {
                            let ty = self.place_type(&fp);
                            let id = self.declare_local(f.name.clone(), ty, f.name_span, false);
                            self.emit(CfgStmt::Assign {
                                place: Place {
                                    local: id,
                                    proj: Vec::new(),
                                },
                                value: Rvalue::Use { place: fp, mode },
                                span: f.name_span,
                            });
                        }
                    }
                }
            }
            Pattern::Tuple { elems, .. } => {
                for (i, p) in elems.iter().enumerate() {
                    let mut fp = place.clone();
                    fp.proj.push(Proj::Field(i.to_string()));
                    self.bind_from_place(p, &fp, mode);
                }
            }
            Pattern::Variant { name, payload, .. } => {
                if let Some(p) = payload {
                    let mut fp = place.clone();
                    fp.proj.push(Proj::Variant(name.clone()));
                    let ty = self.place_type(place);
                    let payload_ty = match &ty {
                        Type::Result(a, b) if name == "ok" => (**a).clone(),
                        Type::Result(_, b) if name == "err" => (**b).clone(),
                        Type::Optional(a) => (**a).clone(),
                        Type::Enum(n, _) => self
                            .program
                            .defs
                            .enums
                            .get(n)
                            .and_then(|d| {
                                d.variants
                                    .iter()
                                    .find(|(v, _)| v == name)
                                    .and_then(|(_, t)| t.clone())
                            })
                            .unwrap_or(Type::Error),
                        _ => Type::Error,
                    };
                    self.bind_pattern(p, &payload_ty, Some(Operand::Place(fp)), Span::default());
                }
            }
            Pattern::Literal(_) | Pattern::Range { .. } | Pattern::Poison => {}
        }
    }

    fn switch_to(
        &mut self,
        pattern: &Pattern,
        place: &Place,
        then_b: BlockId,
        else_b: BlockId,
        span: Span,
    ) {
        match pattern {
            Pattern::Variant { name, .. } => {
                self.terminate(Term::Switch {
                    value: place.clone(),
                    cases: vec![(name.clone(), then_b)],
                    default: else_b,
                    span,
                });
            }
            Pattern::Literal(Expr {
                kind: ExprKind::Bool(b),
                ..
            }) => {
                let cond = self.operand_place(Operand::Const(ConstVal::Bool(*b)), UseMode::Copy);
                self.terminate(Term::Branch {
                    cond: Operand::Place(cond),
                    then_b,
                    else_b,
                    span,
                });
            }
            Pattern::Literal(Expr {
                kind: ExprKind::Int {
                    value, negative, ..
                },
                ..
            }) => {
                let n = if *negative {
                    -(*value as i128)
                } else {
                    *value as i128
                };
                let cond = self.add_temp(Type::Bool, span);
                self.emit(CfgStmt::Assign {
                    place: Place {
                        local: cond,
                        proj: Vec::new(),
                    },
                    value: Rvalue::Binary {
                        op: BinOp::Eq,
                        lhs: Operand::Place(place.clone()),
                        rhs: Operand::Const(ConstVal::Int(n)),
                    },
                    span,
                });
                self.terminate(Term::Branch {
                    cond: Operand::Place(Place {
                        local: cond,
                        proj: Vec::new(),
                    }),
                    then_b,
                    else_b,
                    span,
                });
            }
            Pattern::Wild(_) | Pattern::Bind { .. } => {
                self.terminate(Term::Goto(then_b));
            }
            _ => {
                self.terminate(Term::Branch {
                    cond: Operand::Const(ConstVal::Bool(true)),
                    then_b,
                    else_b,
                    span,
                });
            }
        }
    }
}

#[derive(Clone, Debug)]
#[allow(clippy::large_enum_variant)]
enum CallTarget {
    Direct {
        function: Function,
        receiver: Option<Expr>,
    },
    Constructor {
        ty: Type,
        variant: AggKind,
        has_payload: bool,
    },
    Indirect,
}

fn stmt_span(stmt: &CfgStmt) -> Option<Span> {
    match stmt {
        CfgStmt::Assign { span, .. }
        | CfgStmt::StorageStart { span, .. }
        | CfgStmt::StorageEnd { span, .. }
        | CfgStmt::Use { span, .. }
        | CfgStmt::CancelScope { span, .. }
        | CfgStmt::ConsumeUnion { span, .. } => Some(*span),
        CfgStmt::Check { check } => Some(check.span),
    }
}

fn block_uses_name(block: &crate::ast::Block, name: &str) -> bool {
    fn expr(e: &Expr, name: &str) -> bool {
        match &e.kind {
            ExprKind::Ident(n) if n == name => true,
            ExprKind::Field { base, .. }
            | ExprKind::Deref { base, .. }
            | ExprKind::TupleField { base, .. } => expr(base, name),
            ExprKind::Index { base, index, .. } => expr(base, name) || expr(index, name),
            ExprKind::Slice { base, lo, hi, .. } => {
                expr(base, name) || expr(lo, name) || expr(hi, name)
            }
            ExprKind::Unary { expr: x, .. } | ExprKind::Cast { expr: x, .. } => expr(x, name),
            ExprKind::Binary { lhs, rhs, .. } => expr(lhs, name) || expr(rhs, name),
            ExprKind::Call { callee, args, .. } => {
                expr(callee, name) || args.iter().any(|a| expr(a, name))
            }
            ExprKind::Builtin { args, .. } => args.iter().any(|a| expr(a, name)),
            ExprKind::StructLit { fields, .. } | ExprKind::StructShorthand { fields, .. } => {
                fields.iter().any(|f| expr(&f.value, name))
            }
            ExprKind::EnumLit { payload, .. } => {
                payload.as_ref().map(|p| expr(p, name)).unwrap_or(false)
            }
            ExprKind::ArrayLit { elems, .. } | ExprKind::Tuple { elems, .. } => {
                elems.iter().any(|e| expr(e, name))
            }
            ExprKind::Paren(x) | ExprKind::TypeApply { base: x, .. } => expr(x, name),
            _ => false,
        }
    }
    fn stmt(s: &crate::ast::Stmt, name: &str) -> bool {
        match s {
            crate::ast::Stmt::Let { init, .. } => expr(init, name),
            crate::ast::Stmt::Assign { place, value, .. } => expr(place, name) || expr(value, name),
            crate::ast::Stmt::Expr { expr: e, .. } => expr(e, name),
            crate::ast::Stmt::Return { value, .. } => {
                value.as_ref().map(|e| expr(e, name)).unwrap_or(false)
            }
            crate::ast::Stmt::Fail { value, .. } => expr(value, name),
            crate::ast::Stmt::Defer { body, .. } | crate::ast::Stmt::Block(body) => {
                block_uses_name(body, name)
            }
            crate::ast::Stmt::If(x) => {
                expr(&x.cond.expr, name)
                    || block_uses_name(&x.then, name)
                    || x.otherwise.as_ref().map(|e| stmt(e, name)).unwrap_or(false)
            }
            crate::ast::Stmt::While { cond, body, .. } => {
                expr(&cond.expr, name) || block_uses_name(body, name)
            }
            crate::ast::Stmt::Loop { body, .. } => block_uses_name(body, name),
            crate::ast::Stmt::For {
                iter, body, step, ..
            } => {
                expr(iter, name)
                    || step.as_ref().map(|e| expr(e, name)).unwrap_or(false)
                    || block_uses_name(body, name)
            }
            crate::ast::Stmt::Match(m) => {
                expr(&m.scrutinee, name)
                    || m.arms.iter().any(|a| match &a.body {
                        ArmBody::Block(b) => block_uses_name(b, name),
                        ArmBody::Expr(e) => expr(e, name),
                    })
            }
            crate::ast::Stmt::Scope { body, .. } => block_uses_name(body, name),
            _ => false,
        }
    }
    block.stmts.iter().any(|s| stmt(s, name))
}

fn len_root_expr(e: &Expr) -> Option<String> {
    let ExprKind::Field { base, name, .. } = &e.kind else {
        return None;
    };
    if name != "len" {
        return None;
    }
    let ExprKind::Ident(root) = &base.kind else {
        return None;
    };
    Some(root.clone())
}

fn pattern_const(p: &Pattern) -> Option<i128> {
    match p {
        Pattern::Literal(Expr {
            kind: ExprKind::Int {
                value, negative, ..
            },
            ..
        }) => Some(if *negative {
            -(*value as i128)
        } else {
            *value as i128
        }),
        _ => None,
    }
}

fn assign_binop(op: AssignOp) -> BinOp {
    match op {
        AssignOp::Assign => BinOp::Add,
        AssignOp::Add => BinOp::Add,
        AssignOp::Sub => BinOp::Sub,
        AssignOp::Mul => BinOp::Mul,
        AssignOp::Div => BinOp::Div,
        AssignOp::Rem => BinOp::Rem,
        AssignOp::AddWrap => BinOp::AddWrap,
        AssignOp::SubWrap => BinOp::SubWrap,
        AssignOp::MulWrap => BinOp::MulWrap,
        AssignOp::AddSat => BinOp::AddSat,
        AssignOp::SubSat => BinOp::SubSat,
        AssignOp::MulSat => BinOp::MulSat,
        AssignOp::BitAnd => BinOp::BitAnd,
        AssignOp::BitOr => BinOp::BitOr,
        AssignOp::BitXor => BinOp::BitXor,
        AssignOp::Shl => BinOp::Shl,
        AssignOp::Shr => BinOp::Shr,
    }
}
