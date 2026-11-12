//! Borrow and origin checks for the MVP.
//!
//! This is a deliberately small, lexical approximation of `docs/impl/checker.md`.
//! Loans are tracked per place path. A loan is live until the last use of the
//! local that holds it, or until the end of its scope when the holder is a
//! `scope` value. That is sufficient for the borrow conformance tests and keeps
//! the first implementation readable. The CFG dataflow version replaces it.

use crate::ast::*;
use crate::check::{Function, LocalFact, Program};
use crate::diagnostic::Diagnostic;
use crate::source::{Source, Span};
use crate::types::Type;
use std::collections::{HashMap, HashSet};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BorrowKind {
    Shared,
    Exclusive,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Holder {
    Local(String),
    Scope(String),
}

#[derive(Clone, Debug)]
struct Loan {
    id: usize,
    path: Vec<String>,
    kind: BorrowKind,
    holder: Holder,
    span: Span,
    active: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Access {
    Read,
    Write,
    SharedBorrow,
    ExclusiveBorrow,
}

struct Borrows<'a> {
    source: &'a Source,
    program: &'a Program,
    facts: HashMap<String, LocalFact>,
    last_use: HashMap<String, (usize, Span)>,
    scope_ends: HashMap<String, usize>,
    scopes: Vec<Vec<String>>,
    loans: Vec<Loan>,
    next_loan: usize,
    diagnostics: Vec<Diagnostic>,
    reported: HashSet<(usize, &'static str)>,
}

pub fn check_function(
    program: &Program,
    source: &Source,
    f: &FnDecl,
    facts: &[LocalFact],
) -> Vec<Diagnostic> {
    let mut last_use = HashMap::new();
    if let Some(body) = &f.body {
        collect_uses_block(body, &mut last_use);
    }
    let mut b = Borrows {
        source,
        program,
        facts: facts.iter().map(|f| (f.name.clone(), f.clone())).collect(),
        last_use,
        scope_ends: HashMap::new(),
        scopes: Vec::new(),
        loans: Vec::new(),
        next_loan: 0,
        diagnostics: Vec::new(),
        reported: HashSet::new(),
    };
    let mut params = Vec::new();
    for p in &f.params {
        params.push(p.name.clone());
    }
    b.scopes.push(params);
    if let Some(body) = &f.body {
        b.block(body);
        let end = Span::empty(body.span.end.saturating_sub(1));
        b.end_scope(body.span.end, end);
    }
    b.diagnostics
}

impl<'a> Borrows<'a> {
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

    fn scope_end(&self, name: &str) -> Option<usize> {
        self.scope_ends.get(name).copied()
    }

    fn holder_last_use(&self, holder: &Holder) -> Option<Span> {
        match holder {
            Holder::Local(n) => self.last_use.get(n).map(|(_, s)| *s),
            Holder::Scope(n) => self
                .last_use
                .get(n)
                .map(|(_, s)| *s)
                .or_else(|| self.scope_end(n).map(|e| Span::empty(e))),
        }
    }

    fn live(&self, loan: &Loan, at: usize) -> bool {
        if !loan.active {
            return false;
        }
        match &loan.holder {
            Holder::Local(n) => self
                .last_use
                .get(n)
                .map(|(end, _)| *end > at)
                .unwrap_or(false),
            Holder::Scope(n) => self.scope_end(n).map(|end| at <= end).unwrap_or(false),
        }
    }

    fn overlapping(&self, a: &[String], b: &[String]) -> bool {
        a.starts_with(b) || b.starts_with(a)
    }

    fn access(&mut self, path: &[String], access: Access, span: Span) {
        let at = span.start;
        for i in 0..self.loans.len() {
            let loan = self.loans[i].clone();
            if !self.live(&loan, at) || !self.overlapping(path, &loan.path) {
                continue;
            }
            let conflict = match access {
                Access::Read => loan.kind == BorrowKind::Exclusive,
                Access::SharedBorrow => loan.kind == BorrowKind::Exclusive,
                Access::Write | Access::ExclusiveBorrow => true,
            };
            if !conflict {
                continue;
            }
            let (code, message) = match access {
                Access::Read => (
                    "E1103",
                    format!(
                        "`{}` is read while it is exclusively borrowed",
                        path.join(".")
                    ),
                ),
                Access::Write => (
                    "E1101",
                    format!("`{}` is assigned while it is borrowed", path.join(".")),
                ),
                Access::SharedBorrow => (
                    "E1103",
                    format!(
                        "`{}` is borrowed shared while an exclusive loan on it is live",
                        path.join(".")
                    ),
                ),
                Access::ExclusiveBorrow => (
                    "E1102",
                    format!(
                        "`{}` is borrowed exclusively while another loan on it is live",
                        path.join(".")
                    ),
                ),
            };
            if !self.reported.insert((loan.id, code)) {
                continue;
            }
            let later = self.holder_last_use(&loan.holder);
            let d = self.diag(code, message, span);
            d.relate("borrow", loan.span);
            if let Some(l) = later {
                d.relate("later_use", l);
            }
        }
    }

    fn borrow(&mut self, path: Vec<String>, kind: BorrowKind, span: Span, holder: Option<Holder>) {
        let access = if kind == BorrowKind::Exclusive {
            Access::ExclusiveBorrow
        } else {
            Access::SharedBorrow
        };
        self.access(&path, access, span);
        if let Some(holder) = holder {
            let id = self.next_loan;
            self.next_loan += 1;
            self.loans.push(Loan {
                id,
                path,
                kind,
                holder,
                span,
                active: true,
            });
        }
    }

    fn block(&mut self, block: &Block) {
        self.scopes.push(Vec::new());
        for stmt in &block.stmts {
            self.stmt(stmt);
        }
        self.end_scope(
            block.span.end,
            Span::empty(block.span.end.saturating_sub(1)),
        );
        self.scopes.pop();
    }

    fn end_scope(&mut self, end: usize, at: Span) {
        let Some(declared) = self.scopes.last().cloned() else {
            return;
        };
        for i in 0..self.loans.len() {
            let loan = &self.loans[i];
            if !loan.active {
                continue;
            }
            let Some(root) = loan.path.first().cloned() else {
                continue;
            };
            let root_ends = declared.contains(&root);
            let holder_ends = match &loan.holder {
                Holder::Local(n) => declared.contains(n),
                Holder::Scope(_) => false,
            };
            if root_ends && self.live(loan, end) {
                let scope_holder = matches!(loan.holder, Holder::Scope(_));
                let (code, message) = if scope_holder {
                    (
                        "E1403",
                        format!(
                            "`{root}` is declared inside the scope and given to a task that outlives it"
                        ),
                    )
                } else {
                    (
                        "E1105",
                        format!("`{root}` ends while a borrow of it is still live"),
                    )
                };
                let id = loan.id;
                let lspan = loan.span;
                let later = self.holder_last_use(&loan.holder);
                if self.reported.insert((id, code)) {
                    let d = self.diag(code, message, lspan);
                    d.relate("scope_end", at);
                    if let Some(l) = later {
                        d.relate("later_use", l);
                    }
                }
            }
            let root_ends = declared.contains(&root);
            if root_ends || holder_ends {
                self.loans[i].active = false;
            }
        }
        // Loans held by values that end keep no reachable holder.
        for loan in &mut self.loans {
            if let Holder::Local(n) = &loan.holder {
                if declared.contains(n) {
                    loan.active = false;
                }
            }
        }
    }

    fn stmt(&mut self, stmt: &Stmt) {
        match stmt {
            Stmt::Let { pattern, init, .. } => {
                let holder = simple_binding(pattern).map(Holder::Local);
                self.init_expr(init, holder);
                let names = pattern_names(pattern);
                if let Some(scope) = self.scopes.last_mut() {
                    scope.extend(names);
                }
            }
            Stmt::Assign {
                place, value, span, ..
            } => {
                let path = place_path(place);
                let root = path.first().cloned();
                let holder = root.as_ref().and_then(|n| {
                    let is_view_local = path.len() == 1
                        && self
                            .fact(n)
                            .map(|f| f.ty.is_view(&self.program.defs))
                            .unwrap_or(false);
                    if is_view_local {
                        Some(Holder::Local(n.clone()))
                    } else {
                        None
                    }
                });
                if let Some(holder) = holder {
                    // Assigning a view to a whole local kills its old loans.
                    for loan in &mut self.loans {
                        if loan.holder == holder {
                            loan.active = false;
                        }
                    }
                    self.init_expr(value, Some(holder));
                } else {
                    self.access(&path, Access::Write, *span);
                    self.expr(value);
                }
            }
            Stmt::Expr { expr, .. } => self.expr(expr),
            Stmt::Return { value, span } => {
                if let Some(v) = value {
                    self.expr(v);
                    self.check_return_local(v, *span);
                }
                self.exit_checks(*span);
            }
            Stmt::Fail { value, span } => {
                self.expr(value);
                self.exit_checks(*span);
            }
            Stmt::Break { .. } | Stmt::Continue { .. } => {}
            Stmt::Defer { body, .. } => self.block(body),
            Stmt::Block(b) => self.block(b),
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
                vars,
                index,
                iter,
                range_end,
                step,
                body,
                ..
            } => {
                self.expr(iter);
                if let Some((_, e)) = range_end {
                    self.expr(e);
                }
                if let Some(e) = step {
                    self.expr(e);
                }
                self.scopes.push(Vec::new());
                let mut names = Vec::new();
                if let Some((n, _)) = index {
                    names.push(n.clone());
                }
                for (n, _) in vars {
                    names.push(n.clone());
                }
                if let Some(scope) = self.scopes.last_mut() {
                    scope.extend(names);
                }
                self.block(body);
                self.scopes.pop();
            }
            Stmt::Match(m) => {
                self.expr(&m.scrutinee);
                for arm in &m.arms {
                    self.scopes.push(Vec::new());
                    for p in &arm.patterns {
                        if let Some(scope) = self.scopes.last_mut() {
                            scope.extend(pattern_names(p));
                        }
                    }
                    if let Some(g) = &arm.guard {
                        self.expr(g);
                    }
                    match &arm.body {
                        ArmBody::Block(b) => self.block(b),
                        ArmBody::Expr(e) => self.expr(e),
                    }
                    self.scopes.pop();
                }
            }
            Stmt::Scope {
                name, body, span, ..
            } => {
                self.scope_ends.insert(name.clone(), body.span.end);
                if let Some(scope) = self.scopes.last_mut() {
                    scope.push(name.clone());
                }
                self.block(body);
                for loan in &mut self.loans {
                    if loan.holder == Holder::Scope(name.clone()) {
                        loan.active = false;
                    }
                }
                self.scope_ends.remove(name);
                let _ = span;
            }
            Stmt::Spawn {
                call, scope, span, ..
            } => {
                self.spawn(call, scope, *span);
            }
            Stmt::Dispatch {
                call,
                domain,
                executor,
                ..
            } => {
                self.expr(call);
                self.expr(domain);
                self.expr(executor);
            }
            Stmt::Select { arms, .. } => {
                for arm in arms {
                    self.expr(&arm.call);
                    self.block(&arm.body);
                }
            }
            Stmt::Unsafe { body, .. } => self.block(body),
            Stmt::When {
                condition,
                then,
                otherwise,
                ..
            } => {
                self.expr(condition);
                self.block(then);
                for s in otherwise {
                    self.stmt(s);
                }
            }
            Stmt::Poison => {}
        }
    }

    fn check_return_local(&mut self, value: &Expr, span: Span) {
        if let ExprKind::Unary {
            op: UnOp::Borrow | UnOp::BorrowMut,
            expr,
            op_span,
        } = &value.kind
        {
            if let Some(root) = root_ident(expr) {
                if let Some(fact) = self.fact(root) {
                    if !fact.is_param {
                        let span = if op_span.is_empty() { span } else { *op_span };
                        self.diag(
                            "E1203",
                            format!("`{root}` is a local, so this borrow cannot be returned"),
                            span,
                        );
                    }
                }
            }
        }
        if matches!(value.kind, ExprKind::Ident(_)) {
            if let Some(root) = root_ident(value) {
                for loan in &self.loans {
                    if !loan.active || loan.holder != Holder::Local(root.to_string()) {
                        continue;
                    }
                    if let Some(r) = loan.path.first() {
                        if self.fact(r).map(|f| !f.is_param).unwrap_or(false) {
                            self.diag(
                                "E1203",
                                format!("the returned value borrows local `{r}`"),
                                span,
                            );
                            break;
                        }
                    }
                }
            }
        }
    }

    fn exit_checks(&mut self, _at: Span) {
        // The function boundary has no storage-end check here; local borrows
        // are diagnosed by E1203 and block ends by E1105.
    }

    fn init_expr(&mut self, expr: &Expr, holder: Option<Holder>) {
        match &expr.kind {
            ExprKind::Unary {
                op: UnOp::Borrow | UnOp::BorrowMut,
                expr: inner,
                op_span,
            } => {
                let kind = if matches!(
                    expr.kind,
                    ExprKind::Unary {
                        op: UnOp::BorrowMut,
                        ..
                    }
                ) {
                    BorrowKind::Exclusive
                } else {
                    BorrowKind::Shared
                };
                let path = place_path(inner);
                self.borrow(path, kind, *op_span, holder);
            }
            ExprKind::Call { callee, args, .. } => self.call(callee, args, holder),
            ExprKind::Paren(inner) => self.init_expr(inner, holder),
            ExprKind::Ident(name) => {
                self.access(&[name.clone()], Access::Read, expr.span);
                if let Some(holder) = holder {
                    // Copying a view copies its origins.
                    let loans: Vec<Loan> = self
                        .loans
                        .iter()
                        .filter(|l| l.active && l.holder == Holder::Local(name.clone()))
                        .cloned()
                        .collect();
                    for mut l in loans {
                        l.id = self.next_loan;
                        self.next_loan += 1;
                        l.holder = holder.clone();
                        l.span = expr.span;
                        self.loans.push(l);
                    }
                }
            }
            _ => self.expr(expr),
        }
    }

    fn expr(&mut self, expr: &Expr) {
        match &expr.kind {
            ExprKind::Ident(name) => self.access(&[name.clone()], Access::Read, expr.span),
            ExprKind::Field { base, name, .. } => {
                let mut p = place_path(base);
                p.push(name.clone());
                self.access(&p, Access::Read, expr.span);
            }
            ExprKind::TupleField { base, index, .. } => {
                let mut p = place_path(base);
                p.push(index.to_string());
                self.access(&p, Access::Read, expr.span);
            }
            ExprKind::Index { base, index, .. } => {
                let mut p = place_path(base);
                p.push("[*]".to_string());
                self.access(&p, Access::Read, expr.span);
                self.expr(index);
            }
            ExprKind::Deref { base, .. } => {
                let mut p = place_path(base);
                p.push("*".to_string());
                self.access(&p, Access::Read, expr.span);
            }
            ExprKind::Paren(inner) => self.expr(inner),
            ExprKind::Unary {
                op: UnOp::Borrow | UnOp::BorrowMut,
                expr: inner,
                op_span,
            } => {
                let kind = if matches!(
                    expr.kind,
                    ExprKind::Unary {
                        op: UnOp::BorrowMut,
                        ..
                    }
                ) {
                    BorrowKind::Exclusive
                } else {
                    BorrowKind::Shared
                };
                self.borrow(place_path(inner), kind, *op_span, None);
            }
            ExprKind::Unary { expr: inner, .. } => self.expr(inner),
            ExprKind::Binary { lhs, rhs, .. } => {
                self.expr(lhs);
                self.expr(rhs);
            }
            ExprKind::Call { callee, args, .. } => self.call(callee, args, None),
            ExprKind::Slice { base, lo, hi, .. } => {
                let p = place_path(expr);
                self.access(&p, Access::Read, expr.span);
                let _ = base;
                self.expr(lo);
                self.expr(hi);
            }
            ExprKind::Cast { expr: inner, .. } => self.expr(inner),
            ExprKind::StructLit { fields, .. } | ExprKind::StructShorthand { fields, .. } => {
                for f in fields {
                    self.expr(&f.value);
                }
            }
            ExprKind::EnumLit { payload, .. } => {
                if let Some(p) = payload {
                    self.expr(p);
                }
            }
            ExprKind::ArrayLit { elems, .. } => {
                for e in elems {
                    self.expr(e);
                }
            }
            ExprKind::Range { lo, hi, .. } => {
                self.expr(lo);
                self.expr(hi);
            }
            ExprKind::Tuple { elems, .. } => {
                for e in elems {
                    self.expr(e);
                }
            }
            ExprKind::If {
                cond,
                then,
                otherwise,
                ..
            } => {
                self.expr(&cond.expr);
                self.expr(then);
                self.expr(otherwise);
            }
            ExprKind::Match {
                scrutinee, arms, ..
            } => {
                self.expr(scrutinee);
                for arm in arms {
                    self.scopes.push(Vec::new());
                    for p in &arm.patterns {
                        if let Some(scope) = self.scopes.last_mut() {
                            scope.extend(pattern_names(p));
                        }
                    }
                    if let Some(g) = &arm.guard {
                        self.expr(g);
                    }
                    match &arm.body {
                        ArmBody::Block(b) => self.block(b),
                        ArmBody::Expr(e) => self.expr(e),
                    }
                    self.scopes.pop();
                }
            }
            ExprKind::Catch { value, handler, .. } => {
                self.expr(value);
                match handler {
                    Handler::Expr(e) => self.expr(e),
                    Handler::Block(b) => self.block(b),
                }
            }
            ExprKind::Orelse { value, handler, .. } => {
                self.expr(value);
                match handler {
                    Handler::Expr(e) => self.expr(e),
                    Handler::Block(b) => self.block(b),
                }
            }
            ExprKind::Spawn { call, scope, span } => self.spawn(call, scope, *span),
            ExprKind::Dispatch {
                call,
                domain,
                executor,
                ..
            } => {
                self.expr(call);
                self.expr(domain);
                self.expr(executor);
            }
            ExprKind::Unsafe { body, .. } => self.expr(body),
            ExprKind::TypeApply { base, .. } => self.expr(base),
            ExprKind::Builtin { args, .. } => {
                for a in args {
                    self.expr(a);
                }
            }
            ExprKind::Closure { .. }
            | ExprKind::Int { .. }
            | ExprKind::Float(_)
            | ExprKind::Str(_)
            | ExprKind::CStr(_)
            | ExprKind::Bool(_)
            | ExprKind::Path(_)
            | ExprKind::Poison => {}
        }
    }

    fn call(&mut self, callee: &Expr, args: &[Expr], holder: Option<Holder>) {
        let (function, receiver) = self.resolve_callee(callee);
        for a in args {
            self.expr(a);
        }
        // Activate receiver and argument borrows now, after every argument has
        // been evaluated (§12, two-phase borrows).
        if let (Some(f), Some(base)) = (&function, receiver.as_ref()) {
            if let Some(self_ty) = f.signature_params.first() {
                let path = place_path(base);
                let access = match self_ty {
                    Type::Borrow { mutable: true, .. } | Type::Slice { mutable: true, .. } => {
                        Some(Access::ExclusiveBorrow)
                    }
                    Type::Borrow { .. } | Type::Slice { .. } => Some(Access::SharedBorrow),
                    _ => None,
                };
                if let Some(a) = access {
                    self.access(&path, a, base.span);
                }
            }
        }
        let Some(f) = function else {
            for a in args {
                if let ExprKind::Unary {
                    op: UnOp::BorrowMut,
                    ..
                } = a.kind
                {
                    // Already evaluated as a temporary above.
                }
            }
            return;
        };
        // `into` stores.
        for p in &f.decl.params {
            let Some((target, _)) = &p.into else {
                continue;
            };
            let Some(src_idx) = f.decl.params.iter().position(|q| q.name == p.name) else {
                continue;
            };
            let Some(dst_idx) = f.decl.params.iter().position(|q| q.name == *target) else {
                continue;
            };
            if let (Some(src), Some(dst)) = (args.get(src_idx), args.get(dst_idx)) {
                if let Some(src_root) = root_ident(src) {
                    if let Some(dst_root) = root_ident(dst) {
                        let path = place_path(src);
                        let holder = if self.fact(dst_root).map(|f| f.is_param).unwrap_or(false)
                            && matches!(
                                self.fact(dst_root).map(|f| &f.ty),
                                Some(Type::Borrow { inner, .. }) if matches!(inner.as_ref(), Type::External(n) if n == "Scope")
                            ) {
                            Holder::Scope(dst_root.to_string())
                        } else {
                            Holder::Local(dst_root.to_string())
                        };
                        let _ = src_root;
                        self.borrow(path, BorrowKind::Shared, src.span, Some(holder));
                    }
                }
            }
        }
        // Type-parameter flow: a `T` argument stored through a `&mut` parameter
        // whose type mentions a type parameter.
        if !f.decl.generics.is_empty() {
            for (i, p) in f.decl.params.iter().enumerate() {
                let is_mut = matches!(
                    p.ty,
                    Ty::Borrow { mutable: true, .. } | Ty::Slice { mutable: true, .. }
                );
                if !is_mut || !ty_mentions_param(&p.ty, &f.decl.generics) {
                    continue;
                }
                let Some(dst) = args.get(i) else {
                    continue;
                };
                let Some(dst_root) = root_ident(dst) else {
                    continue;
                };
                for (j, q) in f.decl.params.iter().enumerate() {
                    if j == i {
                        continue;
                    }
                    if !ty_mentions_param(&q.ty, &f.decl.generics)
                        && !matches!(q.ty, Ty::Borrow { .. } | Ty::Slice { .. })
                    {
                        continue;
                    }
                    let Some(src) = args.get(j) else {
                        continue;
                    };
                    if !self.is_view_expr(src) {
                        continue;
                    }
                    self.borrow(
                        place_path(src),
                        BorrowKind::Shared,
                        src.span,
                        Some(Holder::Local(dst_root.to_string())),
                    );
                }
            }
        }
        // Result origins.
        if let Some(holder) = holder {
            if f.signature_ret.is_view(&self.program.defs) {
                if let Some(base) = &receiver {
                    let from_self_plain = f
                        .decl
                        .from
                        .iter()
                        .any(|o| !o.inner && !o.is_static && o.name == "self");
                    if from_self_plain {
                        let kind = if ret_is_exclusive(&f.signature_ret) {
                            BorrowKind::Exclusive
                        } else {
                            BorrowKind::Shared
                        };
                        self.borrow(place_path(base), kind, base.span, Some(holder));
                    }
                } else {
                    for origin in &f.decl.from {
                        if origin.is_static || origin.inner {
                            continue;
                        }
                        let Some(idx) = f.decl.params.iter().position(|p| p.name == origin.name)
                        else {
                            continue;
                        };
                        if let Some(arg) = args.get(idx) {
                            let kind = if ret_is_exclusive(&f.signature_ret) {
                                BorrowKind::Exclusive
                            } else {
                                BorrowKind::Shared
                            };
                            self.borrow(place_path(arg), kind, arg.span, Some(holder.clone()));
                        }
                    }
                }
            }
        }
    }

    fn resolve_callee(&self, callee: &Expr) -> (Option<Function>, Option<Expr>) {
        match &callee.kind {
            ExprKind::Ident(name) => (self.program.function(name).cloned(), None),
            ExprKind::Field { base, name, .. } => {
                if let ExprKind::Path(p) = &base.kind {
                    let ty = Type::External(p.name().to_string());
                    return (self.program.method(&ty, name).cloned(), None);
                }
                let recv = self.expr_type(base);
                let f = self.program.method(&recv, name).cloned();
                (f, Some((**base).clone()))
            }
            ExprKind::TypeApply { base, .. } => self.resolve_callee(base),
            _ => (None, None),
        }
    }

    fn is_view_expr(&self, e: &Expr) -> bool {
        self.expr_type(e).is_view(&self.program.defs)
    }

    fn expr_type(&self, e: &Expr) -> Type {
        match &e.kind {
            ExprKind::Ident(n) => self.fact(n).map(|f| f.ty.clone()).unwrap_or(Type::Error),
            ExprKind::Paren(x) => self.expr_type(x),
            ExprKind::Unary {
                op: UnOp::Borrow,
                expr,
                ..
            } => Type::Borrow {
                mutable: false,
                inner: Box::new(self.expr_type(expr)),
            },
            ExprKind::Unary {
                op: UnOp::BorrowMut,
                expr,
                ..
            } => Type::Borrow {
                mutable: true,
                inner: Box::new(self.expr_type(expr)),
            },
            ExprKind::Field { base, name, .. } => {
                let b = self.expr_type(base);
                self.program
                    .defs
                    .field(b.strip_borrows(), name)
                    .unwrap_or(Type::Error)
            }
            ExprKind::Index { base, .. } => match self.expr_type(base) {
                Type::Array(_, e) | Type::Slice { elem: e, .. } => (*e).clone(),
                _ => Type::Error,
            },
            ExprKind::Deref { base, .. } => match self.expr_type(base) {
                Type::Borrow { inner, .. } | Type::Ptr { inner, .. } => (*inner).clone(),
                _ => Type::Error,
            },
            ExprKind::Slice { base, .. } => match self.expr_type(base) {
                Type::Array(_, elem) => Type::Slice {
                    mutable: false,
                    elem,
                },
                Type::Slice { mutable, elem } => Type::Slice { mutable, elem },
                _ => Type::Error,
            },
            ExprKind::Path(p) => {
                let name = p.name();
                if self.program.defs.structs.contains_key(name) {
                    Type::Struct(name.to_string(), Vec::new())
                } else {
                    Type::External(name.to_string())
                }
            }
            _ => Type::Error,
        }
    }

    fn spawn(&mut self, call: &Expr, scope: &Expr, _span: Span) {
        let Some(scope_name) = root_ident(scope) else {
            self.expr(call);
            return;
        };
        let Some(ExprKind::Call { args, .. }) = Some(&call.kind) else {
            self.expr(call);
            return;
        };
        for a in args {
            if let ExprKind::Unary {
                op: UnOp::Borrow | UnOp::BorrowMut,
                expr: inner,
                op_span,
            } = &a.kind
            {
                let kind = if matches!(
                    a.kind,
                    ExprKind::Unary {
                        op: UnOp::BorrowMut,
                        ..
                    }
                ) {
                    BorrowKind::Exclusive
                } else {
                    BorrowKind::Shared
                };
                self.borrow(
                    place_path(inner),
                    kind,
                    *op_span,
                    Some(Holder::Scope(scope_name.to_string())),
                );
            } else {
                self.expr(a);
                if self.is_view_expr(a) {
                    self.borrow(
                        place_path(a),
                        BorrowKind::Shared,
                        a.span,
                        Some(Holder::Scope(scope_name.to_string())),
                    );
                }
            }
        }
    }
}

fn ret_is_exclusive(t: &Type) -> bool {
    matches!(
        t,
        Type::Borrow { mutable: true, .. } | Type::Slice { mutable: true, .. }
    )
}

fn ty_mentions_param(ty: &Ty, generics: &[GenericParam]) -> bool {
    match ty {
        Ty::Path(p) => {
            generics.iter().any(|g| g.name == p.name())
                || p.args.iter().any(|a| match a {
                    GenericArg::Ty(t) => ty_mentions_param(t, generics),
                    _ => false,
                })
        }
        Ty::Borrow { inner, .. }
        | Ty::Slice { elem: inner, .. }
        | Ty::Ptr { inner, .. }
        | Ty::Optional { inner, .. } => ty_mentions_param(inner, generics),
        Ty::Array { elem, .. } => ty_mentions_param(elem, generics),
        Ty::Tuple { elems, .. } | Ty::Fn { params: elems, .. } => {
            elems.iter().any(|t| ty_mentions_param(t, generics))
        }
        _ => false,
    }
}

fn place_path(e: &Expr) -> Vec<String> {
    match &e.kind {
        ExprKind::Ident(n) => vec![n.clone()],
        ExprKind::Paren(x) => place_path(x),
        ExprKind::Field { base, name, .. } => {
            let mut p = place_path(base);
            p.push(name.clone());
            p
        }
        ExprKind::TupleField { base, index, .. } => {
            let mut p = place_path(base);
            p.push(index.to_string());
            p
        }
        ExprKind::Deref { base, .. } => {
            let mut p = place_path(base);
            p.push("*".to_string());
            p
        }
        ExprKind::Index { base, .. } | ExprKind::Slice { base, .. } => {
            let mut p = place_path(base);
            p.push("[*]".to_string());
            p
        }
        ExprKind::Unary {
            op: UnOp::Borrow | UnOp::BorrowMut,
            expr,
            ..
        } => place_path(expr),
        _ => Vec::new(),
    }
}

fn root_ident(e: &Expr) -> Option<&str> {
    match &e.kind {
        ExprKind::Ident(n) if n != "_" => Some(n),
        ExprKind::Paren(x) => root_ident(x),
        ExprKind::Field { base, .. }
        | ExprKind::TupleField { base, .. }
        | ExprKind::Deref { base, .. }
        | ExprKind::Index { base, .. }
        | ExprKind::Slice { base, .. } => root_ident(base),
        ExprKind::Unary {
            op: UnOp::Borrow | UnOp::BorrowMut,
            expr,
            ..
        } => root_ident(expr),
        _ => None,
    }
}

fn simple_binding(p: &Pattern) -> Option<String> {
    match p {
        Pattern::Bind { name, .. } if name != "_" => Some(name.clone()),
        _ => None,
    }
}

fn pattern_names(p: &Pattern) -> Vec<String> {
    let mut out = Vec::new();
    fn walk(p: &Pattern, out: &mut Vec<String>) {
        match p {
            Pattern::Bind { name, .. } if name != "_" => out.push(name.clone()),
            Pattern::Tuple { elems, .. } => {
                for e in elems {
                    walk(e, out);
                }
            }
            Pattern::Struct { fields, .. } => {
                for f in fields {
                    match &f.pattern {
                        Some(p) => walk(p, out),
                        None => out.push(f.name.clone()),
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

fn collect_uses_block(block: &Block, out: &mut HashMap<String, (usize, Span)>) {
    for s in &block.stmts {
        collect_uses_stmt(s, out);
    }
}

fn note(out: &mut HashMap<String, (usize, Span)>, name: &str, span: Span) {
    let e = out.entry(name.to_string()).or_insert((0, span));
    if span.end > e.0 {
        *e = (span.end, span);
    }
}

fn collect_uses_stmt(s: &Stmt, out: &mut HashMap<String, (usize, Span)>) {
    match s {
        Stmt::Let { init, .. } => collect_uses_expr(init, out),
        Stmt::Assign { place, value, .. } => {
            collect_uses_expr(place, out);
            collect_uses_expr(value, out);
        }
        Stmt::Expr { expr, .. } => collect_uses_expr(expr, out),
        Stmt::Return { value, .. } => {
            if let Some(v) = value {
                collect_uses_expr(v, out);
            }
        }
        Stmt::Fail { value, .. } => collect_uses_expr(value, out),
        Stmt::Defer { body, .. } | Stmt::Block(body) => collect_uses_block(body, out),
        Stmt::If(x) => {
            collect_uses_expr(&x.cond.expr, out);
            collect_uses_block(&x.then, out);
            if let Some(e) = &x.otherwise {
                collect_uses_stmt(e, out);
            }
        }
        Stmt::While { cond, body, .. } => {
            collect_uses_expr(&cond.expr, out);
            collect_uses_block(body, out);
        }
        Stmt::Loop { body, .. } => collect_uses_block(body, out),
        Stmt::For {
            iter,
            range_end,
            step,
            body,
            ..
        } => {
            collect_uses_expr(iter, out);
            if let Some((_, e)) = range_end {
                collect_uses_expr(e, out);
            }
            if let Some(e) = step {
                collect_uses_expr(e, out);
            }
            collect_uses_block(body, out);
        }
        Stmt::Match(m) => {
            collect_uses_expr(&m.scrutinee, out);
            for a in &m.arms {
                if let Some(g) = &a.guard {
                    collect_uses_expr(g, out);
                }
                match &a.body {
                    ArmBody::Block(b) => collect_uses_block(b, out),
                    ArmBody::Expr(e) => collect_uses_expr(e, out),
                }
            }
        }
        Stmt::Scope { body, .. } => collect_uses_block(body, out),
        Stmt::Spawn { call, scope, .. } => {
            collect_uses_expr(call, out);
            collect_uses_expr(scope, out);
        }
        Stmt::Dispatch {
            call,
            domain,
            executor,
            ..
        } => {
            collect_uses_expr(call, out);
            collect_uses_expr(domain, out);
            collect_uses_expr(executor, out);
        }
        Stmt::Select { arms, .. } => {
            for a in arms {
                collect_uses_expr(&a.call, out);
                collect_uses_block(&a.body, out);
            }
        }
        Stmt::Unsafe { body, .. } => collect_uses_block(body, out),
        Stmt::When {
            condition,
            then,
            otherwise,
            ..
        } => {
            collect_uses_expr(condition, out);
            collect_uses_block(then, out);
            for s in otherwise {
                collect_uses_stmt(s, out);
            }
        }
        Stmt::Break { .. } | Stmt::Continue { .. } | Stmt::Poison => {}
    }
}

fn collect_uses_expr(e: &Expr, out: &mut HashMap<String, (usize, Span)>) {
    match &e.kind {
        ExprKind::Ident(n) => note(out, n, e.span),
        ExprKind::Field { base, .. }
        | ExprKind::TupleField { base, .. }
        | ExprKind::Deref { base, .. } => collect_uses_expr(base, out),
        ExprKind::Index { base, index, .. } => {
            collect_uses_expr(base, out);
            collect_uses_expr(index, out);
        }
        ExprKind::Slice { base, lo, hi, .. } => {
            collect_uses_expr(base, out);
            collect_uses_expr(lo, out);
            collect_uses_expr(hi, out);
        }
        ExprKind::Paren(x) | ExprKind::Unary { expr: x, .. } | ExprKind::Cast { expr: x, .. } => {
            collect_uses_expr(x, out)
        }
        ExprKind::Binary { lhs, rhs, .. } => {
            collect_uses_expr(lhs, out);
            collect_uses_expr(rhs, out);
        }
        ExprKind::Call { callee, args, .. } => {
            collect_uses_expr(callee, out);
            for a in args {
                collect_uses_expr(a, out);
            }
        }
        ExprKind::StructLit { fields, .. } | ExprKind::StructShorthand { fields, .. } => {
            for f in fields {
                collect_uses_expr(&f.value, out);
            }
        }
        ExprKind::EnumLit { payload, .. } => {
            if let Some(p) = payload {
                collect_uses_expr(p, out);
            }
        }
        ExprKind::ArrayLit { elems, .. } | ExprKind::Tuple { elems, .. } => {
            for x in elems {
                collect_uses_expr(x, out);
            }
        }
        ExprKind::Range { lo, hi, .. } => {
            collect_uses_expr(lo, out);
            collect_uses_expr(hi, out);
        }
        ExprKind::If {
            cond,
            then,
            otherwise,
            ..
        } => {
            collect_uses_expr(&cond.expr, out);
            collect_uses_expr(then, out);
            collect_uses_expr(otherwise, out);
        }
        ExprKind::Match {
            scrutinee, arms, ..
        } => {
            collect_uses_expr(scrutinee, out);
            for a in arms {
                if let Some(g) = &a.guard {
                    collect_uses_expr(g, out);
                }
                match &a.body {
                    ArmBody::Block(b) => collect_uses_block(b, out),
                    ArmBody::Expr(e) => collect_uses_expr(e, out),
                }
            }
        }
        ExprKind::Catch { value, handler, .. } | ExprKind::Orelse { value, handler, .. } => {
            collect_uses_expr(value, out);
            match handler {
                Handler::Expr(e) => collect_uses_expr(e, out),
                Handler::Block(b) => collect_uses_block(b, out),
            }
        }
        ExprKind::Spawn { call, scope, .. } => {
            collect_uses_expr(call, out);
            collect_uses_expr(scope, out);
        }
        ExprKind::Dispatch {
            call,
            domain,
            executor,
            ..
        } => {
            collect_uses_expr(call, out);
            collect_uses_expr(domain, out);
            collect_uses_expr(executor, out);
        }
        ExprKind::Unsafe { body, .. } | ExprKind::TypeApply { base: body, .. } => {
            collect_uses_expr(body, out)
        }
        ExprKind::Builtin { args, .. } => {
            for a in args {
                collect_uses_expr(a, out);
            }
        }
        _ => {}
    }
}
