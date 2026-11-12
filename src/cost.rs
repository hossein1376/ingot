//! Phase 10, first cut: the checks that survive, for `ingot cost`.
//!
//! The analysis is intentionally small: it proves the index of a counted
//! `for i in 0..xs.len` loop and an `@assert(xs.len >= n)`, and folds constant
//! operands. That is the covered part of §31. The full forward dataflow over the
//! CFG replaces this when lowering lands.

use crate::ast::*;
use crate::check::Program;
use crate::source::Span;
use std::collections::HashMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CostKind {
    Bounds,
    Overflow,
    Div,
    Shift,
}

impl CostKind {
    pub fn name(self) -> &'static str {
        match self {
            CostKind::Bounds => "bounds_check",
            CostKind::Overflow => "overflow_check",
            CostKind::Div => "div_check",
            CostKind::Shift => "shift_check",
        }
    }

    pub fn parse(s: &str) -> Option<CostKind> {
        Some(match s {
            "bounds_check" => CostKind::Bounds,
            "overflow_check" => CostKind::Overflow,
            "div_check" => CostKind::Div,
            "shift_check" => CostKind::Shift,
            _ => return None,
        })
    }
}

#[derive(Clone, Debug)]
pub struct CostItem {
    pub kind: CostKind,
    pub function: String,
    pub span: Span,
}

#[derive(Clone, Default)]
struct Facts {
    /// A lower bound on `len(base)`, proven by an `@assert`.
    len_at_least: HashMap<String, i128>,
    /// A loop counter that is known to be `0 <= i < len(base)`.
    counter: Option<(String, String)>,
}

impl CostItem {
    pub fn to_json(&self, sources: &crate::source::Sources) -> String {
        let (file, line, col) = sources
            .files
            .first()
            .map(|s| {
                let p = s.position(self.span.start);
                (s.path.clone(), p.line, p.col)
            })
            .unwrap_or((String::new(), 1, 1));
        format!(
            "{{\"type\":\"cost\",\"kind\":\"{}\",\"fn\":\"{}\",\"span\":{{\"file\":\"{}\",\"start\":{{\"line\":{},\"col\":{}}},\"end\":{{\"line\":{},\"col\":{}}}}}}}",
            self.kind.name(),
            json_escape(&self.function),
            json_escape(&file),
            line,
            col,
            line,
            col
        )
    }
}

fn json_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

pub fn cost_items(program: &Program) -> Vec<CostItem> {
    let mut out = Vec::new();
    for f in program.functions.values() {
        if f.owner.is_some() || f.external {
            continue;
        }
        if let Some(body) = &f.decl.body {
            let mut facts = Facts::default();
            let canonical = format!("{}.{}.{}", program.package_name, f.module, f.decl.name);
            walk_statements(&body.stmts, &mut facts, &canonical, &mut out);
        }
    }
    for methods in program.methods.values() {
        for f in methods.values() {
            if f.external {
                continue;
            }
            if let Some(body) = &f.decl.body {
                let mut facts = Facts::default();
                let canonical = format!("{}.{}.{}", program.package_name, f.module, f.decl.name);
                walk_statements(&body.stmts, &mut facts, &canonical, &mut out);
            }
        }
    }
    out
}

fn walk_statements(stmts: &[Stmt], facts: &mut Facts, function: &str, out: &mut Vec<CostItem>) {
    for stmt in stmts {
        match stmt {
            Stmt::Let { init, .. } => walk_expr(init, facts, function, out),
            Stmt::Assign {
                place,
                op,
                value,
                span,
            } => {
                walk_expr(place, facts, function, out);
                walk_expr(value, facts, function, out);
                let op_kind = match op {
                    AssignOp::Add | AssignOp::Sub | AssignOp::Mul => Some(CostKind::Overflow),
                    AssignOp::Div | AssignOp::Rem => Some(CostKind::Div),
                    AssignOp::Shl | AssignOp::Shr => Some(CostKind::Shift),
                    _ => None,
                };
                let constant = expr_const_i128(value).is_some();
                if let Some(kind) = op_kind
                    && (!constant || !matches!(op, AssignOp::Add | AssignOp::Sub | AssignOp::Mul))
                {
                    out.push(CostItem {
                        kind,
                        function: function.to_string(),
                        span: *span,
                    });
                }
            }
            Stmt::Expr { expr, .. } => walk_expr(expr, facts, function, out),
            Stmt::Return { value: Some(v), .. } => walk_expr(v, facts, function, out),
            Stmt::Return { value: None, .. } => {}
            Stmt::Fail { value, .. } => walk_expr(value, facts, function, out),
            Stmt::Defer { body, .. } | Stmt::Block(body) => {
                let saved = facts.clone();
                walk_statements(&body.stmts, facts, function, out);
                *facts = saved;
            }
            Stmt::If(x) => {
                walk_expr(&x.cond.expr, facts, function, out);
                let saved = facts.clone();
                walk_statements(&x.then.stmts, facts, function, out);
                *facts = saved.clone();
                if let Some(e) = &x.otherwise {
                    walk_statement(e, facts, function, out);
                }
                *facts = saved;
            }
            Stmt::While { cond, body, .. } => {
                walk_expr(&cond.expr, facts, function, out);
                let saved = facts.clone();
                walk_statements(&body.stmts, facts, function, out);
                *facts = saved;
            }
            Stmt::Loop { body, .. } => {
                let saved = facts.clone();
                walk_statements(&body.stmts, facts, function, out);
                *facts = saved;
            }
            Stmt::For {
                vars,
                index,
                iter,
                range_end,
                step,
                body,
                ..
            } => {
                walk_expr(iter, facts, function, out);
                if let Some((_, e)) = range_end {
                    walk_expr(e, facts, function, out);
                }
                if let Some(e) = step {
                    walk_expr(e, facts, function, out);
                }
                let saved = facts.clone();
                if let (Some((_false_end, end)), Some((var, _))) = (range_end, vars.first())
                    && let Some(base) = len_base(end)
                {
                    facts.counter = Some((var.clone(), base));
                }
                if let Some((_idx, _)) = index {
                    // `for i, x in xs`: i is a valid index into xs.
                    if let Some((var, _)) = vars.first() {
                        facts.counter = Some((var.clone(), "?".to_string()));
                    }
                }
                walk_statements(&body.stmts, facts, function, out);
                *facts = saved;
            }
            Stmt::Match(m) => {
                walk_expr(&m.scrutinee, facts, function, out);
                for arm in &m.arms {
                    if let Some(g) = &arm.guard {
                        walk_expr(g, facts, function, out);
                    }
                    let saved = facts.clone();
                    match &arm.body {
                        ArmBody::Block(b) => walk_statements(&b.stmts, facts, function, out),
                        ArmBody::Expr(e) => walk_expr(e, facts, function, out),
                    }
                    *facts = saved;
                }
            }
            Stmt::Scope { body, .. } => {
                let saved = facts.clone();
                walk_statements(&body.stmts, facts, function, out);
                *facts = saved;
            }
            Stmt::Spawn { call, scope, .. } => {
                walk_expr(call, facts, function, out);
                walk_expr(scope, facts, function, out);
            }
            Stmt::Dispatch {
                call,
                domain,
                executor,
                ..
            } => {
                walk_expr(call, facts, function, out);
                walk_expr(domain, facts, function, out);
                walk_expr(executor, facts, function, out);
            }
            Stmt::Select { arms, .. } => {
                for a in arms {
                    walk_expr(&a.call, facts, function, out);
                    let saved = facts.clone();
                    walk_statements(&a.body.stmts, facts, function, out);
                    *facts = saved;
                }
            }
            Stmt::Unsafe { body, .. } => {
                let saved = facts.clone();
                walk_statements(&body.stmts, facts, function, out);
                *facts = saved;
            }
            Stmt::When {
                then, otherwise, ..
            } => {
                let saved = facts.clone();
                walk_statements(&then.stmts, facts, function, out);
                *facts = saved.clone();
                for s in otherwise {
                    walk_statement(s, facts, function, out);
                }
                *facts = saved;
            }
            _ => {}
        }
        if let Stmt::Expr {
            expr:
                Expr {
                    kind: ExprKind::Builtin { name, args, .. },
                    ..
                },
            ..
        } = stmt
            && name == "assert"
            && let Some(cond) = args.first()
            && let Some((base, n)) = len_assertion(cond)
        {
            let e = facts.len_at_least.entry(base).or_insert(0);
            *e = (*e).max(n);
        }
    }
}

fn walk_statement(stmt: &Stmt, facts: &mut Facts, function: &str, out: &mut Vec<CostItem>) {
    walk_statements(std::slice::from_ref(stmt), facts, function, out);
}

fn walk_expr(e: &Expr, facts: &Facts, function: &str, out: &mut Vec<CostItem>) {
    match &e.kind {
        ExprKind::Index { base, index, .. } => {
            if let Some(root) = root_ident(base)
                && !index_proven(facts, root, index)
            {
                out.push(CostItem {
                    kind: CostKind::Bounds,
                    function: function.to_string(),
                    span: e.span,
                });
            }
            walk_expr(index, facts, function, out);
        }
        ExprKind::Slice { base, lo, hi, .. } => {
            if let Some(root) = root_ident(base) {
                let len_ok = facts
                    .len_at_least
                    .get(root)
                    .map(|lb| expr_const_i128(hi).map(|h| h <= *lb).unwrap_or(false))
                    .unwrap_or(false)
                    || facts
                        .counter
                        .as_ref()
                        .map(|(_, b)| b == root)
                        .unwrap_or(false);
                if !len_ok {
                    out.push(CostItem {
                        kind: CostKind::Bounds,
                        function: function.to_string(),
                        span: e.span,
                    });
                }
            }
            walk_expr(lo, facts, function, out);
            walk_expr(hi, facts, function, out);
        }
        ExprKind::Binary {
            op,
            lhs,
            rhs,
            op_span,
            ..
        } => {
            walk_expr(lhs, facts, function, out);
            walk_expr(rhs, facts, function, out);
            let kind = match op {
                BinOp::Add | BinOp::Sub | BinOp::Mul => Some(CostKind::Overflow),
                BinOp::Div | BinOp::Rem => Some(CostKind::Div),
                BinOp::Shl | BinOp::Shr => Some(CostKind::Shift),
                _ => None,
            };
            if let Some(kind) = kind {
                let both_const = expr_const_i128(lhs).is_some()
                    && (expr_const_i128(rhs).is_some() || expr_const_i128(lhs).is_some());
                let remove = match kind {
                    CostKind::Overflow => both_const,
                    CostKind::Div | CostKind::Shift => expr_const_i128(rhs).is_some(),
                    _ => false,
                };
                if !remove {
                    out.push(CostItem {
                        kind,
                        function: function.to_string(),
                        span: *op_span,
                    });
                }
            }
        }
        ExprKind::Unary {
            op: UnOp::Neg,
            expr,
            op_span,
            ..
        } => {
            walk_expr(expr, facts, function, out);
            if expr_const_i128(expr).is_none() {
                out.push(CostItem {
                    kind: CostKind::Overflow,
                    function: function.to_string(),
                    span: *op_span,
                });
            }
        }
        ExprKind::Unary { expr, .. } => walk_expr(expr, facts, function, out),
        ExprKind::Call { callee, args, .. } => {
            walk_expr(callee, facts, function, out);
            for a in args {
                walk_expr(a, facts, function, out);
            }
        }
        ExprKind::Cast { expr, .. } | ExprKind::TypeApply { base: expr, .. } => {
            walk_expr(expr, facts, function, out)
        }
        ExprKind::StructLit { fields, .. } | ExprKind::StructShorthand { fields, .. } => {
            for f in fields {
                walk_expr(&f.value, facts, function, out);
            }
        }
        ExprKind::EnumLit {
            payload: Some(p), ..
        } => walk_expr(p, facts, function, out),
        ExprKind::EnumLit { payload: None, .. } => {}
        ExprKind::ArrayLit { elems, .. } | ExprKind::Tuple { elems, .. } => {
            for e in elems {
                walk_expr(e, facts, function, out);
            }
        }
        ExprKind::Range { lo, hi, .. } => {
            walk_expr(lo, facts, function, out);
            walk_expr(hi, facts, function, out);
        }
        ExprKind::Paren(x) => walk_expr(x, facts, function, out),
        ExprKind::If {
            cond,
            then,
            otherwise,
            ..
        } => {
            walk_expr(&cond.expr, facts, function, out);
            walk_expr(then, facts, function, out);
            walk_expr(otherwise, facts, function, out);
        }
        ExprKind::Match {
            scrutinee, arms, ..
        } => {
            walk_expr(scrutinee, facts, function, out);
            for arm in arms {
                if let Some(g) = &arm.guard {
                    walk_expr(g, facts, function, out);
                }
                match &arm.body {
                    ArmBody::Expr(e) => walk_expr(e, facts, function, out),
                    ArmBody::Block(b) => {
                        let mut f = facts.clone();
                        walk_statements(&b.stmts, &mut f, function, out);
                    }
                }
            }
        }
        ExprKind::Catch { value, handler, .. } | ExprKind::Orelse { value, handler, .. } => {
            walk_expr(value, facts, function, out);
            match handler {
                Handler::Expr(e) => walk_expr(e, facts, function, out),
                Handler::Block(b) => {
                    let mut f = facts.clone();
                    walk_statements(&b.stmts, &mut f, function, out);
                }
            }
        }
        ExprKind::Spawn { call, scope, .. } => {
            walk_expr(call, facts, function, out);
            walk_expr(scope, facts, function, out);
        }
        ExprKind::Dispatch {
            call,
            domain,
            executor,
            ..
        } => {
            walk_expr(call, facts, function, out);
            walk_expr(domain, facts, function, out);
            walk_expr(executor, facts, function, out);
        }
        ExprKind::Unsafe { body, .. } => walk_expr(body, facts, function, out),
        ExprKind::Builtin { args, .. } => {
            for a in args {
                walk_expr(a, facts, function, out);
            }
        }
        _ => {}
    }
}

fn index_proven(facts: &Facts, base: &str, index: &Expr) -> bool {
    if let Some(n) = expr_const_i128(index) {
        return n >= 0
            && facts
                .len_at_least
                .get(base)
                .map(|lb| n < *lb)
                .unwrap_or(false);
    }
    if let ExprKind::Ident(n) = &index.kind
        && let Some((_, b)) = &facts.counter
    {
        return b == base || b == "?" || n == b;
    }
    false
}

fn len_base(e: &Expr) -> Option<String> {
    match &e.kind {
        ExprKind::Field { base, name, .. } if name == "len" => root_ident(base).map(str::to_string),
        _ => None,
    }
}

fn len_assertion(cond: &Expr) -> Option<(String, i128)> {
    if let ExprKind::Binary { op, lhs, rhs, .. } = &cond.kind {
        match op {
            BinOp::Ge | BinOp::Gt => {
                let base = len_base(lhs)?;
                let n = expr_const_i128(rhs)?;
                return Some((base, if *op == BinOp::Gt { n + 1 } else { n }));
            }
            BinOp::Le | BinOp::Lt => {
                let base = len_base(rhs)?;
                let n = expr_const_i128(lhs)?;
                return Some((base, if *op == BinOp::Lt { n + 1 } else { n }));
            }
            _ => {}
        }
    }
    None
}

fn expr_const_i128(e: &Expr) -> Option<i128> {
    match &e.kind {
        ExprKind::Int {
            value, negative, ..
        } => Some(if *negative {
            -(*value as i128)
        } else {
            *value as i128
        }),
        ExprKind::Paren(x) => expr_const_i128(x),
        ExprKind::Unary {
            op: UnOp::Neg,
            expr,
            ..
        } => expr_const_i128(expr).map(|v| -v),
        ExprKind::Binary { op, lhs, rhs, .. } => {
            let a = expr_const_i128(lhs)?;
            let b = expr_const_i128(rhs)?;
            match op {
                BinOp::Add => a.checked_add(b),
                BinOp::Sub => a.checked_sub(b),
                BinOp::Mul => a.checked_mul(b),
                BinOp::Div => a.checked_div(b),
                BinOp::Rem => a.checked_rem(b),
                _ => None,
            }
        }
        _ => None,
    }
}

fn root_ident(e: &Expr) -> Option<&str> {
    match &e.kind {
        ExprKind::Ident(n) if n != "_" => Some(n),
        ExprKind::Paren(x) => root_ident(x),
        ExprKind::Field { base, .. }
        | ExprKind::TupleField { base, .. }
        | ExprKind::Deref { base, .. }
        | ExprKind::Index { base, .. } => root_ident(base),
        _ => None,
    }
}
