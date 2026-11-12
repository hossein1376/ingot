//! A first ownership pass: moves, linear consumption, overwrites, and values
//! moved while a deferred block still needs them.
//!
//! This is deliberately smaller than `docs/impl/checker.md`: the CFG lowering
//! and the four dataflow analyses land next. This pass works on the typed AST
//! and covers E1001 to E1005 for whole locals and simple field reads. Borrow
//! conflicts (E11xx) and origin flow (E12xx) are not implemented yet.

use crate::ast::*;
use crate::check::{Function, LocalFact, Program};
use crate::diagnostic::Diagnostic;
use crate::source::{Source, Span};
use crate::types::Type;
use std::collections::{HashMap, HashSet};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum VarState {
    Init,
    Moved,
    Maybe,
}

impl VarState {
    fn join(self, other: VarState) -> VarState {
        if self == other { self } else { VarState::Maybe }
    }
}

struct Own<'a> {
    source: &'a Source,
    program: &'a Program,
    facts: HashMap<String, LocalFact>,
    state: HashMap<String, VarState>,
    last_move: HashMap<String, Span>,
    scopes: Vec<Vec<String>>,
    defers: Vec<Vec<(Block, bool, Span)>>,
    diagnostics: Vec<Diagnostic>,
    reported_linear: HashSet<String>,
    applying_defer: bool,
}

pub fn check_function(
    program: &Program,
    source: &Source,
    f: &FnDecl,
    facts: &[LocalFact],
) -> Vec<Diagnostic> {
    let mut own = Own {
        source,
        program,
        facts: facts.iter().map(|f| (f.name.clone(), f.clone())).collect(),
        state: HashMap::new(),
        last_move: HashMap::new(),
        scopes: Vec::new(),
        defers: Vec::new(),
        diagnostics: Vec::new(),
        reported_linear: HashSet::new(),
        applying_defer: false,
    };
    let mut params = Vec::new();
    for p in &f.params {
        own.state.insert(p.name.clone(), VarState::Init);
        params.push(p.name.clone());
    }
    own.scopes.push(params);
    if let Some(body) = &f.body {
        own.check_block(body);
        own.apply_defers();
        own.check_linear_at_exit(Span::empty(body.span.end.saturating_sub(1)), None);
    }
    own.diagnostics
}

impl<'a> Own<'a> {
    fn diag(
        &mut self,
        code: &'static str,
        message: impl Into<String>,
        span: Span,
    ) -> &mut Diagnostic {
        self.diagnostics
            .push(Diagnostic::new(code, message, self.source, span));
        self.diagnostics.last_mut().unwrap()
    }

    fn fact(&self, name: &str) -> Option<&LocalFact> {
        self.facts.get(name)
    }

    fn is_copy(&self, ty: &Type) -> bool {
        ty.is_copy(&self.program.defs, &HashMap::new())
    }

    fn state(&self, name: &str) -> VarState {
        *self.state.get(name).unwrap_or(&VarState::Init)
    }

    fn move_local(&mut self, name: &str, span: Span, ty: &Type) {
        if self.is_copy(ty) {
            return;
        }
        match self.state(name) {
            VarState::Moved => {
                if !self.applying_defer {
                    let moved_at = self.last_move.get(name).copied();
                    let st = self.diag(
                        "E1001",
                        format!("`{name}` is used after it was moved"),
                        span,
                    );
                    if let Some(m) = moved_at {
                        st.relate("move", m);
                    }
                }
                return;
            }
            VarState::Maybe => {
                if !self.applying_defer {
                    let st = self.diag(
                        "E1001",
                        format!("`{name}` may be moved on some path that reaches this use"),
                        span,
                    );
                    let _ = st;
                }
                self.state.insert(name.to_string(), VarState::Moved);
                self.last_move.insert(name.to_string(), span);
                return;
            }
            VarState::Init => {}
        }
        if !self.applying_defer {
            let defer_conflict = self.defers.iter().rev().find_map(|list| {
                list.iter()
                    .find(|(b, err, _)| !*err && block_uses(b, name))
                    .map(|(_, _, span)| *span)
            });
            if let Some(dspan) = defer_conflict {
                self.diag(
                    "E1005",
                    format!("`{name}` is moved while a deferred block still uses it"),
                    span,
                )
                .relate("defer", dspan);
            }
        }
        self.state.insert(name.to_string(), VarState::Moved);
        self.last_move.insert(name.to_string(), span);
    }

    fn read_local(&mut self, name: &str, span: Span, mode: Use) {
        let Some(fact) = self.fact(name).cloned() else {
            return;
        };
        match mode {
            Use::Move => self.move_local(name, span, &fact.ty),
            Use::Copy => {
                if self.state(name) == VarState::Moved {
                    let moved_at = self.last_move.get(name).copied();
                    let st = self.diag(
                        "E1001",
                        format!("`{name}` is used after it was moved"),
                        span,
                    );
                    if let Some(m) = moved_at {
                        st.relate("move", m);
                    }
                }
            }
            Use::Borrow => {
                if matches!(self.state(name), VarState::Moved | VarState::Maybe) {
                    let moved_at = self.last_move.get(name).copied();
                    let st = self.diag(
                        "E1001",
                        format!("`{name}` is used after it was moved"),
                        span,
                    );
                    if let Some(m) = moved_at {
                        st.relate("move", m);
                    }
                }
            }
        }
    }

    fn check_block(&mut self, block: &Block) {
        self.scopes.push(Vec::new());
        self.defers.push(Vec::new());
        for stmt in &block.stmts {
            self.check_stmt(stmt);
        }
        let normal_end = Span::empty(block.span.end.saturating_sub(1));
        self.apply_defers();
        self.check_linear_at_exit(normal_end, Some(block));
        self.defers.pop();
        self.scopes.pop();
    }

    fn apply_defers(&mut self) {
        self.apply_defers_for(false)
    }

    fn apply_defers_for(&mut self, error_exit: bool) {
        let Some(list) = self.defers.last().cloned() else {
            return;
        };
        let old = self.applying_defer;
        self.applying_defer = true;
        for (block, err, _) in list.into_iter().rev() {
            if err && !error_exit {
                continue;
            }
            self.scopes.push(Vec::new());
            self.defers.push(Vec::new());
            for stmt in &block.stmts {
                self.check_stmt(stmt);
            }
            self.apply_defers();
            self.scopes.pop();
            self.defers.pop();
        }
        self.applying_defer = old;
    }

    fn check_linear_at_exit(&mut self, at: Span, block: Option<&Block>) {
        let names: Vec<String> = self.scopes.iter().flat_map(|s| s.iter().cloned()).collect();
        for name in names {
            let Some(fact) = self.fact(&name).cloned() else {
                continue;
            };
            if !fact.linear || !matches!(self.state(&name), VarState::Init | VarState::Maybe) {
                continue;
            }
            if !self.reported_linear.insert(name.clone()) {
                continue;
            }
            let d = self.diag(
                "E1003",
                format!("`{name}` is a linear value that is not consumed on every path"),
                fact.decl_span,
            );
            d.relate("scope_end", at);
            let _ = block;
        }
    }

    fn check_stmt(&mut self, stmt: &Stmt) {
        match stmt {
            Stmt::Let { pattern, init, .. } => {
                self.check_expr(init, Use::Move);
                let names = pattern_bindings(pattern);
                for (name, span) in names {
                    if let Some(scope) = self.scopes.last_mut() {
                        scope.push(name.clone());
                    }
                    self.state.insert(name.clone(), VarState::Init);
                    let _ = span;
                }
            }
            Stmt::Assign {
                place, value, span, ..
            } => {
                self.check_expr(value, Use::Move);
                match &place.kind {
                    ExprKind::Ident(name) if name == "_" => {}
                    ExprKind::Ident(name) => {
                        if let Some(fact) = self.fact(name).cloned() {
                            if fact.linear && self.state(name) == VarState::Init {
                                self.diag(
                                    "E1004",
                                    format!("`{name}` is overwritten while it still holds a linear value"),
                                    *span,
                                );
                            }
                            self.state.insert(name.clone(), VarState::Init);
                            self.last_move.remove(name);
                        }
                    }
                    _ => {
                        self.check_expr(place, Use::Borrow);
                    }
                }
            }
            Stmt::Expr { expr, .. } => self.check_expr(expr, Use::Move),
            Stmt::Return { value, span } => {
                if let Some(v) = value {
                    self.check_expr(v, Use::Move);
                }
                self.exit_path(*span, false);
            }
            Stmt::Fail { value, span } => {
                self.check_expr(value, Use::Move);
                self.exit_path(*span, true);
            }
            Stmt::Break { span, .. } | Stmt::Continue { span, .. } => {
                self.exit_path(*span, false);
            }
            Stmt::Defer { err, body, span } => {
                if let Some(list) = self.defers.last_mut() {
                    list.push((body.clone(), *err, *span));
                }
            }
            Stmt::Block(b) => self.check_block(b),
            Stmt::If(x) => {
                self.check_expr(&x.cond.expr, Use::Move);
                let entry = self.state.clone();
                self.check_block(&x.then);
                let then_state = self.state.clone();
                let then_diverges = crate::check::block_diverges(&x.then);
                self.state = entry.clone();
                if let Some(e) = &x.otherwise {
                    self.check_stmt(e);
                }
                let else_diverges = x
                    .otherwise
                    .as_ref()
                    .map(|e| stmt_diverges_local(e))
                    .unwrap_or(false);
                if then_diverges && !else_diverges {
                    // the fallthrough path is the else path
                } else if else_diverges && !then_diverges {
                    self.state = then_state;
                } else {
                    merge_states(&mut self.state, &then_state);
                }
            }
            Stmt::While { cond, body, .. } => {
                self.check_expr(&cond.expr, Use::Move);
                let entry = self.state.clone();
                self.check_block(body);
                merge_states(&mut self.state, &entry);
            }
            Stmt::Loop { body, .. } => {
                let entry = self.state.clone();
                self.check_block(body);
                merge_states(&mut self.state, &entry);
            }
            Stmt::For {
                iter,
                range_end,
                step,
                body,
                ..
            } => {
                self.check_expr(iter, Use::Move);
                if let Some((_, e)) = range_end {
                    self.check_expr(e, Use::Move);
                }
                if let Some(e) = step {
                    self.check_expr(e, Use::Move);
                }
                let entry = self.state.clone();
                self.check_block(body);
                merge_states(&mut self.state, &entry);
            }
            Stmt::Match(m) => {
                self.check_expr(&m.scrutinee, Use::Move);
                let entry = self.state.clone();
                let mut merged: Option<HashMap<String, VarState>> = None;
                for arm in &m.arms {
                    self.state = entry.clone();
                    match &arm.body {
                        ArmBody::Block(b) => self.check_block(b),
                        ArmBody::Expr(e) => self.check_expr(e, Use::Move),
                    }
                    merged = Some(match merged {
                        None => self.state.clone(),
                        Some(mut acc) => {
                            merge_states(&mut acc, &self.state);
                            acc
                        }
                    });
                }
                self.state = merged.unwrap_or(entry);
            }
            Stmt::Scope { body, .. } => self.check_block(body),
            Stmt::Spawn { call, scope, .. } => {
                self.check_expr(call, Use::Move);
                self.check_expr(scope, Use::Move);
            }
            Stmt::Dispatch {
                call,
                domain,
                executor,
                ..
            } => {
                self.check_expr(call, Use::Move);
                self.check_expr(domain, Use::Move);
                self.check_expr(executor, Use::Move);
            }
            Stmt::Select { arms, .. } => {
                for arm in arms {
                    self.check_expr(&arm.call, Use::Move);
                    self.check_block(&arm.body);
                }
            }
            Stmt::Unsafe { body, .. } => self.check_block(body),
            Stmt::When {
                condition,
                then,
                otherwise,
                ..
            } => {
                self.check_expr(condition, Use::Move);
                let entry = self.state.clone();
                self.check_block(then);
                let then_state = self.state.clone();
                self.state = entry;
                for s in otherwise {
                    self.check_stmt(s);
                }
                merge_states(&mut self.state, &then_state);
            }
            Stmt::Poison => {}
        }
    }

    fn exit_path(&mut self, at: Span, error: bool) {
        // Every deferred block of every enclosing block runs on the way out.
        let mut saved = std::mem::take(&mut self.defers);
        let old = self.applying_defer;
        self.applying_defer = true;
        while let Some(list) = saved.pop() {
            for (block, err, _) in list.into_iter().rev() {
                if err && !error {
                    continue;
                }
                self.scopes.push(Vec::new());
                self.defers.push(Vec::new());
                for stmt in &block.stmts {
                    self.check_stmt(stmt);
                }
                self.apply_defers_for(error);
                self.scopes.pop();
                self.defers.pop();
            }
        }
        self.applying_defer = old;
        self.check_linear_at_exit(at, None);
        self.defers = saved;
    }

    fn check_expr(&mut self, expr: &Expr, mode: Use) {
        match &expr.kind {
            ExprKind::Ident(name) => {
                if name != "_" {
                    self.read_local(name, expr.span, mode);
                }
            }
            ExprKind::Field {
                base,
                name,
                name_span,
                ..
            } => self.check_field(base, name, expr.span, mode, *name_span),
            ExprKind::TupleField { base, .. } => self.check_expr(base, mode),
            ExprKind::Index { base, index, .. } => {
                let elem = root_ident(base)
                    .and_then(|n| self.fact(n).map(|f| f.ty.clone()))
                    .and_then(|t| match t {
                        Type::Array(_, e) | Type::Slice { elem: e, .. } => Some(*e),
                        Type::Vector(v) => Some(Type::Int(v.elem)),
                        _ => None,
                    });
                if mode == Use::Move && elem.as_ref().map(|e| !self.is_copy(e)).unwrap_or(true) {
                    self.diag(
                        "E1002",
                        "cannot move out of an array or slice element; use `@replace`, `@swap`, or `.take()`",
                        expr.span,
                    );
                }
                self.check_expr(base, Use::Borrow);
                self.check_expr(index, Use::Move);
            }
            ExprKind::Deref { base, .. } => {
                let pointee = root_ident(base)
                    .and_then(|n| self.fact(n).map(|f| f.ty.clone()))
                    .and_then(|t| match t {
                        Type::Borrow { inner, .. } => Some((*inner, true)),
                        Type::Ptr { inner, .. } => Some((*inner, false)),
                        _ => None,
                    });
                if mode == Use::Move
                    && pointee
                        .as_ref()
                        .map(|(t, borrow)| *borrow && !self.is_copy(t))
                        .unwrap_or(false)
                {
                    self.diag(
                        "E1002",
                        "cannot move out of a borrow; use `@replace`, `@swap`, or `.take()`",
                        expr.span,
                    );
                }
                self.check_expr(base, Use::Borrow);
            }
            ExprKind::Paren(inner) => self.check_expr(inner, mode),
            ExprKind::Unary {
                op, expr: inner, ..
            } => match op {
                UnOp::Borrow | UnOp::BorrowMut => self.check_expr(inner, Use::Borrow),
                _ => self.check_expr(inner, Use::Move),
            },
            ExprKind::Binary { lhs, rhs, .. } => {
                self.check_expr(lhs, Use::Move);
                self.check_expr(rhs, Use::Move);
            }
            ExprKind::Call { callee, args, .. } => self.check_call(callee, args),
            ExprKind::Slice { base, lo, hi, .. } => {
                self.check_expr(base, Use::Borrow);
                self.check_expr(lo, Use::Move);
                self.check_expr(hi, Use::Move);
            }
            ExprKind::Cast { expr, .. } => self.check_expr(expr, Use::Move),
            ExprKind::StructLit { fields, .. } | ExprKind::StructShorthand { fields, .. } => {
                for f in fields {
                    self.check_expr(&f.value, Use::Move);
                }
            }
            ExprKind::EnumLit { payload, .. } => {
                if let Some(p) = payload {
                    self.check_expr(p, Use::Move);
                }
            }
            ExprKind::ArrayLit { elems, .. } => {
                for e in elems {
                    self.check_expr(e, Use::Move);
                }
            }
            ExprKind::Range { lo, hi, .. } => {
                self.check_expr(lo, Use::Move);
                self.check_expr(hi, Use::Move);
            }
            ExprKind::Tuple { elems, .. } => {
                for e in elems {
                    self.check_expr(e, Use::Move);
                }
            }
            ExprKind::If {
                cond,
                then,
                otherwise,
                ..
            } => {
                self.check_expr(&cond.expr, Use::Move);
                let entry = self.state.clone();
                self.check_expr(then, Use::Move);
                let then_state = self.state.clone();
                self.state = entry;
                self.check_expr(otherwise, Use::Move);
                merge_states(&mut self.state, &then_state);
            }
            ExprKind::Match {
                scrutinee, arms, ..
            } => {
                self.check_expr(scrutinee, Use::Move);
                let entry = self.state.clone();
                let mut merged = None;
                for arm in arms {
                    self.state = entry.clone();
                    match &arm.body {
                        ArmBody::Expr(e) => self.check_expr(e, Use::Move),
                        ArmBody::Block(b) => self.check_block(b),
                    }
                    merged = Some(match merged {
                        None => self.state.clone(),
                        Some(mut acc) => {
                            merge_states(&mut acc, &self.state);
                            acc
                        }
                    });
                }
                self.state = merged.unwrap_or(entry);
            }
            ExprKind::Catch { value, handler, .. } => {
                self.check_expr(value, Use::Move);
                let entry = self.state.clone();
                match handler {
                    Handler::Expr(e) => self.check_expr(e, Use::Move),
                    Handler::Block(b) => self.check_block(b),
                }
                merge_states(&mut self.state, &entry);
            }
            ExprKind::Orelse { value, handler, .. } => {
                self.check_expr(value, Use::Move);
                let entry = self.state.clone();
                match handler {
                    Handler::Expr(e) => self.check_expr(e, Use::Move),
                    Handler::Block(b) => self.check_block(b),
                }
                merge_states(&mut self.state, &entry);
            }
            ExprKind::Spawn { call, scope, .. } => {
                self.check_expr(call, Use::Move);
                self.check_expr(scope, Use::Move);
            }
            ExprKind::Dispatch {
                call,
                domain,
                executor,
                ..
            } => {
                self.check_expr(call, Use::Move);
                self.check_expr(domain, Use::Move);
                self.check_expr(executor, Use::Move);
            }
            ExprKind::Unsafe { body, .. } => self.check_expr(body, mode),
            ExprKind::TypeApply { base, .. } => self.check_expr(base, mode),
            ExprKind::Path(_)
            | ExprKind::Int { .. }
            | ExprKind::Float(_)
            | ExprKind::Str(_)
            | ExprKind::CStr(_)
            | ExprKind::Bool(_)
            | ExprKind::Closure { .. } => {}
            ExprKind::Builtin { name, args, .. } => {
                for a in args {
                    let m = if name == "replace" && std::ptr::eq(a, &args[0]) {
                        Use::Borrow
                    } else if matches!(name.as_str(), "swap" | "replace") {
                        Use::Borrow
                    } else {
                        Use::Move
                    };
                    self.check_expr(a, m);
                }
            }
            ExprKind::Poison => {}
        }
    }

    fn check_field(&mut self, base: &Expr, name: &str, span: Span, mode: Use, _field_span: Span) {
        let root = root_ident(base);
        let base_ty = root
            .and_then(|n| self.fact(n).map(|f| f.ty.clone()))
            .unwrap_or(Type::Error);
        let (field_ty, through_borrow) = field_type(&base_ty, name, &self.program);
        if mode == Use::Move {
            if through_borrow && !self.is_copy(&field_ty) {
                self.diag(
                    "E1002",
                    "cannot move a value out of a borrow; use `@replace` or `@swap`",
                    span,
                );
                return;
            }
            if through_borrow {
                self.check_expr(base, Use::Borrow);
                return;
            }
            if !self.is_copy(&field_ty) {
                if let Some(root) = root {
                    if let Some(fact) = self.fact(root).cloned() {
                        if fact.linear {
                            self.diag(
                                "E1006",
                                "a field of a linear struct cannot be moved outside its module",
                                span,
                            );
                            return;
                        }
                    }
                }
            }
        }
        self.check_expr(base, Use::Borrow);
    }

    fn check_call(&mut self, callee: &Expr, args: &[Expr]) {
        match &callee.kind {
            ExprKind::Ident(name) => {
                if let Some(f) = self.program.function(name).cloned() {
                    self.check_call_args(&f, args, false);
                } else {
                    for a in args {
                        self.check_expr(a, Use::Move);
                    }
                }
            }
            ExprKind::Field { base, name, .. } => {
                let root = root_ident(base);
                let recv_ty = root
                    .and_then(|n| self.fact(n).map(|f| f.ty.clone()))
                    .unwrap_or(Type::Error);
                if let Some(f) = self.program.method(&recv_ty, name).cloned() {
                    // Receiver: a by-value `self` consumes it.
                    let by_value = f
                        .decl
                        .params
                        .first()
                        .map(|p| p.is_self() && matches!(&p.ty, Ty::Path(p) if p.name() == recv_ty.as_struct().unwrap_or("")))
                        .unwrap_or(false)
                        || f
                            .decl
                            .params
                            .first()
                            .map(|p| p.is_self() && !p.ty.is_borrow())
                            .unwrap_or(false);
                    if by_value {
                        if let Some(root) = root {
                            if let Some(fact) = self.fact(root).cloned() {
                                self.read_local(root, base.span, Use::Move);
                                let _ = fact;
                            }
                        } else {
                            self.check_expr(base, Use::Move);
                        }
                    } else {
                        self.check_expr(base, Use::Borrow);
                    }
                    self.check_call_args(&f, args, true);
                } else {
                    self.check_expr(base, Use::Borrow);
                    for a in args {
                        self.check_expr(a, Use::Move);
                    }
                }
            }
            ExprKind::TypeApply { base, .. } => self.check_call(base, args),
            _ => {
                self.check_expr(callee, Use::Borrow);
                for a in args {
                    self.check_expr(a, Use::Move);
                }
            }
        }
    }

    fn check_call_args(&mut self, f: &Function, args: &[Expr], method: bool) {
        let skip = if method && f.decl.params.first().map(|p| p.is_self()).unwrap_or(false) {
            1
        } else {
            0
        };
        for (i, a) in args.iter().enumerate() {
            let param = f.signature_params.get(i + skip);
            match a.kind {
                ExprKind::Unary {
                    op: UnOp::Borrow | UnOp::BorrowMut,
                    ..
                } => self.check_expr(a, Use::Borrow),
                _ => {
                    let borrowed = param
                        .map(|t| matches!(t, Type::Borrow { .. } | Type::Slice { .. }))
                        .unwrap_or(false);
                    let copy = param.map(|t| self.is_copy(t)).unwrap_or(false);
                    let use_ = if borrowed {
                        Use::Borrow
                    } else if copy {
                        Use::Copy
                    } else {
                        Use::Move
                    };
                    self.check_expr(a, use_);
                }
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Use {
    Move,
    Copy,
    Borrow,
}

fn stmt_diverges_local(stmt: &Stmt) -> bool {
    match stmt {
        Stmt::Return { .. } | Stmt::Fail { .. } => true,
        Stmt::Block(b) => crate::check::block_diverges(b),
        Stmt::If(x) => {
            crate::check::block_diverges(&x.then)
                && x.otherwise
                    .as_ref()
                    .map(|e| stmt_diverges_local(e))
                    .unwrap_or(false)
        }
        _ => false,
    }
}

fn merge_states(state: &mut HashMap<String, VarState>, other: &HashMap<String, VarState>) {
    let keys: HashSet<String> = state.keys().chain(other.keys()).cloned().collect();
    for k in keys {
        let a = *state.get(&k).unwrap_or(&VarState::Init);
        let b = *other.get(&k).unwrap_or(&VarState::Init);
        state.insert(k, a.join(b));
    }
}

fn pattern_bindings(p: &Pattern) -> Vec<(String, Span)> {
    let mut out = Vec::new();
    fn walk(p: &Pattern, out: &mut Vec<(String, Span)>) {
        match p {
            Pattern::Bind { name, span } => {
                if name != "_" {
                    out.push((name.clone(), *span));
                }
            }
            Pattern::Tuple { elems, .. } => {
                for e in elems {
                    walk(e, out);
                }
            }
            Pattern::Struct { fields, .. } => {
                for f in fields {
                    match &f.pattern {
                        Some(p) => walk(p, out),
                        None => out.push((f.name.clone(), f.name_span)),
                    }
                }
            }
            Pattern::Variant { payload, .. } => {
                if let Some(p) = payload {
                    walk(p, out);
                }
            }
            _ => {}
        }
    }
    walk(p, &mut out);
    out
}

fn root_ident(e: &Expr) -> Option<&str> {
    match &e.kind {
        ExprKind::Ident(name) if name != "_" => Some(name),
        ExprKind::Paren(inner) => root_ident(inner),
        ExprKind::Field { base, .. } | ExprKind::TupleField { base, .. } => root_ident(base),
        ExprKind::Index { base, .. } => root_ident(base),
        _ => None,
    }
}

fn field_type(base: &Type, name: &str, program: &Program) -> (Type, bool) {
    match base {
        Type::Borrow { inner, .. } => {
            let (t, _) = field_type(inner, name, program);
            (t, !matches!(inner.as_ref(), Type::Borrow { .. }))
        }
        Type::Error => (Type::Error, false),
        other => (
            program.defs.field(other, name).unwrap_or(Type::Error),
            false,
        ),
    }
}

fn block_uses(block: &Block, name: &str) -> bool {
    struct Find<'a> {
        name: &'a str,
        found: bool,
    }
    impl Find<'_> {
        fn expr(&mut self, e: &Expr) {
            if self.found {
                return;
            }
            match &e.kind {
                ExprKind::Ident(n) if n == self.name => self.found = true,
                ExprKind::Field { base, .. } | ExprKind::Deref { base, .. } => self.expr(base),
                ExprKind::Index { base, index, .. } => {
                    self.expr(base);
                    self.expr(index);
                }
                ExprKind::Slice { base, lo, hi, .. } => {
                    self.expr(base);
                    self.expr(lo);
                    self.expr(hi);
                }
                ExprKind::Unary { expr, .. } | ExprKind::Cast { expr, .. } => self.expr(expr),
                ExprKind::Binary { lhs, rhs, .. } => {
                    self.expr(lhs);
                    self.expr(rhs);
                }
                ExprKind::Call { callee, args, .. } => {
                    self.expr(callee);
                    for a in args {
                        self.expr(a);
                    }
                }
                ExprKind::Builtin { args, .. } => {
                    for a in args {
                        self.expr(a);
                    }
                }
                ExprKind::StructLit { fields, .. } | ExprKind::StructShorthand { fields, .. } => {
                    for f in fields {
                        self.expr(&f.value);
                    }
                }
                ExprKind::Tuple { elems, .. } | ExprKind::ArrayLit { elems, .. } => {
                    for e in elems {
                        self.expr(e);
                    }
                }
                ExprKind::Paren(e) | ExprKind::TupleField { base: e, .. } => self.expr(e),
                _ => {}
            }
        }
        fn stmt(&mut self, s: &Stmt) {
            match s {
                Stmt::Let { init, .. } => self.expr(init),
                Stmt::Assign { place, value, .. } => {
                    self.expr(place);
                    self.expr(value);
                }
                Stmt::Expr { expr, .. } => self.expr(expr),
                Stmt::Return { value, .. } => {
                    if let Some(v) = value {
                        self.expr(v);
                    }
                }
                Stmt::Fail { value, .. } => self.expr(value),
                Stmt::Defer { body, .. } | Stmt::Block(body) => self.block(body),
                Stmt::If(x) => {
                    self.expr(&x.cond.expr);
                    self.block(&x.then);
                    if let Some(e) = &x.otherwise {
                        self.stmt(e);
                    }
                }
                Stmt::While { cond, body, .. } => {
                    self.expr(&cond.expr);
                    self.block(body);
                }
                Stmt::Loop { body, .. } => self.block(body),
                Stmt::For {
                    iter, body, step, ..
                } => {
                    self.expr(iter);
                    if let Some(s) = step {
                        self.expr(s);
                    }
                    self.block(body);
                }
                Stmt::Match(m) => {
                    self.expr(&m.scrutinee);
                    for a in &m.arms {
                        match &a.body {
                            ArmBody::Block(b) => self.block(b),
                            ArmBody::Expr(e) => self.expr(e),
                        }
                    }
                }
                Stmt::Scope { body, .. } => self.block(body),
                _ => {}
            }
        }
        fn block(&mut self, b: &Block) {
            for s in &b.stmts {
                self.stmt(s);
            }
        }
    }
    let mut f = Find { name, found: false };
    f.block(block);
    f.found
}
