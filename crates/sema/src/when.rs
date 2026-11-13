//! Phase 4: `when` (spec section 7.3).
//!
//! A `when` condition is built only from `@target`, literals, comparisons and
//! `&& || !`, so it is decided here, before name resolution, with no symbol table.
//! The branch that is not taken is removed from the tree. A `when` at module level
//! is replaced by the items of the taken branch; a `when` statement by the taken
//! branch as a block.
//!
//! A condition that cannot be decided (E0612 or E1606 was reported, or the
//! specification does not define a value it needs) removes both branches. The names
//! the branches would have declared are returned so that the module can bind them to
//! poison instead of reporting every use as undefined.

use crate::defs::Ns;
use crate::target::{OS_VARIANTS, TARGET_FIELDS, Target};
use ingot_diag::diagnostic::Diagnostic;
use ingot_diag::source::{Source, Span};
use ingot_syntax::ast::*;

/// A value of a `when` condition.
#[derive(Clone, Debug, PartialEq)]
enum Val {
    Bool(bool),
    Int(i128),
    /// A member of the enum a `@target` field has: the field and the variant.
    Enum(&'static str, &'static str),
    /// `.name` with no type to give it meaning yet.
    Shorthand(String),
    /// `@target` itself.
    Record,
    /// A string or float literal. Allowed by section 7.3, but never comparable here.
    Opaque,
}

pub struct WhenResult {
    /// Names declared in branches that could not be decided.
    pub poisoned_names: Vec<(String, Ns)>,
}

/// Remove the untaken branches of every `when` in `module`.
pub fn eliminate(
    module: &mut Module,
    target: &Target,
    source: &Source,
    diags: &mut Vec<Diagnostic>,
    not_implemented: &mut Vec<String>,
) -> WhenResult {
    let mut cx = Cx {
        target,
        source,
        diags,
        not_implemented,
        poisoned_names: Vec::new(),
    };
    let items = std::mem::take(&mut module.items);
    module.items = cx.items(items);
    WhenResult {
        poisoned_names: cx.poisoned_names,
    }
}

struct Cx<'a> {
    target: &'a Target,
    source: &'a Source,
    diags: &'a mut Vec<Diagnostic>,
    not_implemented: &'a mut Vec<String>,
    poisoned_names: Vec<(String, Ns)>,
}

impl Cx<'_> {
    // ----------------------------------------------------------- conditions

    fn decide(&mut self, condition: &Expr) -> Option<bool> {
        match self.eval(condition)? {
            Val::Bool(b) => Some(b),
            _ => {
                self.unsupported(
                    "a `when` condition that is not a `bool` (its type error belongs to type checking)",
                );
                None
            }
        }
    }

    fn unsupported(&mut self, what: &str) {
        let message = format!("{what} in a `when` condition");
        if !self.not_implemented.contains(&message) {
            self.not_implemented.push(message);
        }
    }

    fn not_constant(&mut self, e: &Expr) {
        self.diags.push(Diagnostic::new(
            "E0612",
            "a `when` condition may use only `@target`, literals, comparisons and `&&`, `||`, `!`",
            self.source,
            e.span,
        ));
    }

    fn eval(&mut self, e: &Expr) -> Option<Val> {
        match &e.kind {
            ExprKind::Bool(b) => Some(Val::Bool(*b)),
            ExprKind::Int {
                value, negative, ..
            } => {
                let v = *value as i128;
                Some(Val::Int(if *negative { -v } else { v }))
            }
            ExprKind::Float(_) | ExprKind::Str(_) | ExprKind::CStr(_) => Some(Val::Opaque),
            ExprKind::Paren(inner) => self.eval(inner),
            ExprKind::EnumLit {
                path: None,
                variant,
                payload: None,
                ..
            } => Some(Val::Shorthand(variant.clone())),
            ExprKind::Builtin {
                name,
                type_arg: None,
                args,
            } if name == "target" && args.is_empty() => Some(Val::Record),
            ExprKind::Field {
                base,
                name,
                name_span,
            } if is_target(base) => self.target_field(name, *name_span, e),
            ExprKind::Call { callee, args, .. } => {
                if let ExprKind::Field { base, name, .. } = &callee.kind
                    && is_target(base)
                    && name == "has"
                {
                    return self.has_call(args, e);
                }
                self.not_constant(e);
                None
            }
            ExprKind::Unary {
                op: UnOp::Not,
                expr,
                ..
            } => match self.eval(expr)? {
                Val::Bool(b) => Some(Val::Bool(!b)),
                _ => {
                    self.unsupported("`!` applied to something that is not a `bool`");
                    None
                }
            },
            ExprKind::Binary { op, lhs, rhs, .. } if op.is_comparison() => {
                let l = self.eval(lhs);
                let r = self.eval(rhs);
                self.compare(*op, l?, r?)
            }
            ExprKind::Binary {
                op: op @ (BinOp::And | BinOp::Or),
                lhs,
                rhs,
                ..
            } => {
                let l = self.eval(lhs);
                let r = self.eval(rhs);
                match (l?, r?) {
                    (Val::Bool(a), Val::Bool(b)) => {
                        Some(Val::Bool(if *op == BinOp::And { a && b } else { a || b }))
                    }
                    _ => {
                        self.unsupported("`&&` or `||` applied to something that is not a `bool`");
                        None
                    }
                }
            }
            _ => {
                self.not_constant(e);
                None
            }
        }
    }

    fn target_field(&mut self, name: &str, name_span: Span, whole: &Expr) -> Option<Val> {
        if !TARGET_FIELDS.contains(&name) {
            self.diags.push(Diagnostic::new(
                "E1606",
                format!("`@target` has no field `{name}`"),
                self.source,
                name_span,
            ));
            return None;
        }
        let t = self.target;
        match name {
            "os" => Some(Val::Enum("os", t.os)),
            "simd_bits" => Some(Val::Int(t.simd_bits)),
            "cache_line" => Some(Val::Int(t.cache_line)),
            "page_size" => Some(Val::Int(t.page_size)),
            "hybrid_cores" => Some(Val::Bool(t.hybrid_cores)),
            "smt" => Some(Val::Bool(t.smt)),
            "gpu_unified" => Some(Val::Bool(t.gpu_unified)),
            "arch" | "cpu" => {
                self.unsupported(&format!(
                    "`@target.{name}`, whose values the specification does not name,"
                ));
                None
            }
            // `has` is a query, not a value.
            _ => {
                self.not_constant(whole);
                None
            }
        }
    }

    fn has_call(&mut self, args: &[Expr], whole: &Expr) -> Option<Val> {
        match args {
            [
                Expr {
                    kind:
                        ExprKind::EnumLit {
                            path: None,
                            variant,
                            payload: None,
                            ..
                        },
                    ..
                },
            ] => {
                if variant == "sme" {
                    Some(Val::Bool(self.target.sme))
                } else {
                    self.unsupported(&format!(
                        "`@target.has(.{variant})`, since the specification names only the feature `sme`,"
                    ));
                    None
                }
            }
            _ => {
                self.not_constant(whole);
                None
            }
        }
    }

    fn compare(&mut self, op: BinOp, l: Val, r: Val) -> Option<Val> {
        let equal = |a: bool| Some(Val::Bool(if op == BinOp::Eq { a } else { !a }));
        match (op, l, r) {
            (BinOp::Eq | BinOp::Ne, Val::Bool(a), Val::Bool(b)) => equal(a == b),
            (BinOp::Eq | BinOp::Ne, Val::Int(a), Val::Int(b)) => equal(a == b),
            (op, Val::Int(a), Val::Int(b)) => Some(Val::Bool(match op {
                BinOp::Lt => a < b,
                BinOp::Le => a <= b,
                BinOp::Gt => a > b,
                _ => a >= b,
            })),
            (BinOp::Eq | BinOp::Ne, Val::Enum(f, a), Val::Enum(g, b)) if f == g => equal(a == b),
            (BinOp::Eq | BinOp::Ne, Val::Enum(f, a), Val::Shorthand(b))
            | (BinOp::Eq | BinOp::Ne, Val::Shorthand(b), Val::Enum(f, a)) => {
                if f == "os" && OS_VARIANTS.contains(&b.as_str()) {
                    equal(a == b)
                } else {
                    self.unsupported(&format!(
                        "`.{b}`, which is not a value of `@target.{f}` (its unknown-member error belongs to type checking),"
                    ));
                    None
                }
            }
            _ => {
                self.unsupported("a comparison of values of different or unordered types");
                None
            }
        }
    }

    // ----------------------------------------------------------------- items

    fn items(&mut self, items: Vec<Item>) -> Vec<Item> {
        let mut out = Vec::new();
        for mut item in items {
            match item {
                Item::When(w) => out.extend(self.when_items(w)),
                _ => {
                    self.item(&mut item);
                    out.push(item);
                }
            }
        }
        out
    }

    fn when_items(&mut self, w: WhenItems) -> Vec<Item> {
        match self.decide(&w.condition) {
            Some(true) => self.items(w.then),
            Some(false) => self.items(w.otherwise),
            None => {
                collect_item_names(&w.then, &mut self.poisoned_names);
                collect_item_names(&w.otherwise, &mut self.poisoned_names);
                Vec::new()
            }
        }
    }

    fn item(&mut self, item: &mut Item) {
        match item {
            Item::Fn(f) => self.fn_decl(f),
            Item::Interface(i) => {
                for m in &mut i.members {
                    self.item(m);
                }
            }
            Item::Impl(i) => {
                for m in &mut i.members {
                    self.item(m);
                }
            }
            Item::Const(c) => self.expr(&mut c.value),
            Item::Static(s) => self.expr(&mut s.value),
            Item::Extern(x) => {
                for f in &mut x.functions {
                    self.fn_decl(f);
                }
            }
            Item::Test(t) => self.block(&mut t.body),
            _ => {}
        }
    }

    fn fn_decl(&mut self, f: &mut FnDecl) {
        if let Some(body) = &mut f.body {
            self.block(body);
        }
    }

    // ------------------------------------------------------------ statements

    fn block(&mut self, b: &mut Block) {
        self.stmts(&mut b.stmts);
    }

    fn stmts(&mut self, stmts: &mut Vec<Stmt>) {
        let old = std::mem::take(stmts);
        for stmt in old {
            match stmt {
                Stmt::When {
                    condition,
                    then,
                    otherwise,
                    ..
                } => {
                    if let Some(s) = self.when_stmt(condition, then, otherwise) {
                        stmts.push(s);
                    }
                }
                mut other => {
                    self.stmt(&mut other);
                    stmts.push(other);
                }
            }
        }
    }

    fn when_stmt(
        &mut self,
        condition: Expr,
        mut then: Block,
        mut otherwise: Vec<Stmt>,
    ) -> Option<Stmt> {
        match self.decide(&condition) {
            Some(true) => {
                self.block(&mut then);
                Some(Stmt::Block(then))
            }
            Some(false) => {
                self.stmts(&mut otherwise);
                otherwise.into_iter().next()
            }
            None => Some(Stmt::Poison),
        }
    }

    fn arm(&mut self, arm: &mut Arm) {
        if let Some(g) = &mut arm.guard {
            self.expr(g);
        }
        match &mut arm.body {
            ArmBody::Block(b) => self.block(b),
            ArmBody::Expr(e) => self.expr(e),
        }
    }

    fn cond(&mut self, c: &mut Cond) {
        self.expr(&mut c.expr);
    }

    fn stmt(&mut self, s: &mut Stmt) {
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
            Stmt::Defer { body, .. } | Stmt::Scope { body, .. } | Stmt::Unsafe { body, .. } => {
                self.block(body)
            }
            Stmt::Block(b) => self.block(b),
            Stmt::If(i) => {
                self.cond(&mut i.cond);
                self.block(&mut i.then);
                if let Some(o) = &mut i.otherwise {
                    self.stmt(o);
                }
            }
            Stmt::While { cond, body, .. } => {
                self.cond(cond);
                self.block(body);
            }
            Stmt::Loop { body, .. } => self.block(body),
            Stmt::For {
                grain,
                iter,
                range_end,
                step,
                body,
                ..
            } => {
                if let Some(g) = grain {
                    self.expr(g);
                }
                self.expr(iter);
                if let Some((_, e)) = range_end {
                    self.expr(e);
                }
                if let Some(e) = step {
                    self.expr(e);
                }
                self.block(body);
            }
            Stmt::Match(m) => {
                self.expr(&mut m.scrutinee);
                for arm in &mut m.arms {
                    self.arm(arm);
                }
            }
            Stmt::Spawn { call, scope, .. } => {
                self.expr(call);
                self.expr(scope);
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
                    self.expr(&mut arm.call);
                    self.block(&mut arm.body);
                }
            }
            // A `when` is replaced where the statement list is walked.
            Stmt::When { .. } | Stmt::Break { .. } | Stmt::Continue { .. } | Stmt::Poison => {}
        }
    }

    // ----------------------------------------------------------- expressions

    fn handler(&mut self, h: &mut Handler) {
        match h {
            Handler::Expr(e) => self.expr(e),
            Handler::Block(b) => self.block(b),
        }
    }

    fn expr(&mut self, e: &mut Expr) {
        match &mut e.kind {
            ExprKind::Builtin { args, .. } => {
                for a in args {
                    self.expr(a);
                }
            }
            ExprKind::Unary { expr, .. } => self.expr(expr),
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
            ExprKind::Index { base, index, .. } => {
                self.expr(base);
                self.expr(index);
            }
            ExprKind::Slice { base, lo, hi, .. } => {
                self.expr(base);
                self.expr(lo);
                self.expr(hi);
            }
            ExprKind::Field { base, .. }
            | ExprKind::TupleField { base, .. }
            | ExprKind::Deref { base, .. }
            | ExprKind::TypeApply { base, .. } => self.expr(base),
            ExprKind::Cast { expr, .. } => self.expr(expr),
            ExprKind::StructLit { fields, .. } | ExprKind::StructShorthand { fields, .. } => {
                for f in fields {
                    self.expr(&mut f.value);
                }
            }
            ExprKind::EnumLit { payload, .. } => {
                if let Some(p) = payload {
                    self.expr(p);
                }
            }
            ExprKind::ArrayLit { len, elems, .. } => {
                self.expr(len);
                for x in elems {
                    self.expr(x);
                }
            }
            ExprKind::Range { lo, hi, .. } => {
                self.expr(lo);
                self.expr(hi);
            }
            ExprKind::Tuple { elems, .. } => {
                for x in elems {
                    self.expr(x);
                }
            }
            ExprKind::Paren(inner) => self.expr(inner),
            ExprKind::If {
                cond,
                then,
                otherwise,
                ..
            } => {
                self.cond(cond);
                self.expr(then);
                self.expr(otherwise);
            }
            ExprKind::Match {
                scrutinee, arms, ..
            } => {
                self.expr(scrutinee);
                for arm in arms {
                    self.arm(arm);
                }
            }
            ExprKind::Closure { body, .. } => self.block(body),
            ExprKind::Catch { value, handler, .. } | ExprKind::Orelse { value, handler, .. } => {
                self.expr(value);
                self.handler(handler);
            }
            ExprKind::Spawn { call, scope, .. } => {
                self.expr(call);
                self.expr(scope);
            }
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
            ExprKind::Int { .. }
            | ExprKind::Float(_)
            | ExprKind::Str(_)
            | ExprKind::CStr(_)
            | ExprKind::Bool(_)
            | ExprKind::Ident(_)
            | ExprKind::Path(_)
            | ExprKind::Poison => {}
        }
    }
}

fn is_target(e: &Expr) -> bool {
    matches!(
        &e.kind,
        ExprKind::Builtin { name, type_arg: None, args } if name == "target" && args.is_empty()
    )
}

/// The names that `items` would declare at module level.
fn collect_item_names(items: &[Item], out: &mut Vec<(String, Ns)>) {
    for item in items {
        match item {
            Item::Fn(f) => out.push((f.name.clone(), Ns::Value)),
            Item::Const(c) => out.push((c.name.clone(), Ns::Value)),
            Item::Static(s) => out.push((s.name.clone(), Ns::Value)),
            Item::Struct(s) => out.push((s.name.clone(), Ns::Type)),
            Item::Enum(e) => out.push((e.name.clone(), Ns::Type)),
            Item::Interface(i) => out.push((i.name.clone(), Ns::Type)),
            Item::TypeAlias(t) => out.push((t.name.clone(), Ns::Type)),
            Item::Extern(x) => {
                for f in &x.functions {
                    out.push((f.name.clone(), Ns::Value));
                }
            }
            Item::When(w) => {
                collect_item_names(&w.then, out);
                collect_item_names(&w.otherwise, out);
            }
            Item::Impl(_) | Item::Test(_) | Item::Poison => {}
        }
    }
}
