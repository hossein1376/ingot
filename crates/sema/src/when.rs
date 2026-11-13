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
//!
//! Every removed branch, taken on this target or not, is also searched for the names it
//! mentions. A use inside a removed branch is still a use, so an import used only there
//! is not unused (E0310).

use crate::defs::Ns;
use crate::target::{ARCH_VARIANTS, CPU_VARIANTS, OS_VARIANTS, TARGET_FIELDS, Target};
use ingot_diag::diagnostic::Diagnostic;
use ingot_diag::source::{Source, Span};
use ingot_syntax::ast::*;
use std::collections::BTreeSet;

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
    /// The first segment of every path and every plain identifier that a removed branch
    /// mentions. Over-approximate: a mention is counted whether or not it would resolve.
    pub mentioned: BTreeSet<String>,
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
        mentioned: BTreeSet::new(),
    };
    let items = std::mem::take(&mut module.items);
    module.items = cx.items(items);
    WhenResult {
        poisoned_names: cx.poisoned_names,
        mentioned: cx.mentioned,
    }
}

struct Cx<'a> {
    target: &'a Target,
    source: &'a Source,
    diags: &'a mut Vec<Diagnostic>,
    not_implemented: &'a mut Vec<String>,
    poisoned_names: Vec<(String, Ns)>,
    mentioned: BTreeSet<String>,
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
            "arch" => Some(Val::Enum("arch", t.arch)),
            "cpu" => Some(Val::Enum("cpu", t.cpu)),
            "simd_bits" => Some(Val::Int(t.simd_bits)),
            "cache_line" => Some(Val::Int(t.cache_line)),
            "page_size" => Some(Val::Int(t.page_size)),
            "hybrid_cores" => Some(Val::Bool(t.hybrid_cores)),
            "smt" => Some(Val::Bool(t.smt)),
            "gpu_unified" => Some(Val::Bool(t.gpu_unified)),
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
                            variant_span,
                            payload: None,
                        },
                    ..
                },
            ] => match self.target.has(variant) {
                Some(present) => Some(Val::Bool(present)),
                None => {
                    self.diags.push(Diagnostic::new(
                        "E1606",
                        format!("`@target.has` has no feature `{variant}`"),
                        self.source,
                        *variant_span,
                    ));
                    None
                }
            },
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
                if variants_of(f).contains(&b.as_str()) {
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
            Some(true) => {
                mentions_items(&w.otherwise, &mut self.mentioned);
                self.items(w.then)
            }
            Some(false) => {
                mentions_items(&w.then, &mut self.mentioned);
                self.items(w.otherwise)
            }
            None => {
                collect_item_names(&w.then, &mut self.poisoned_names);
                collect_item_names(&w.otherwise, &mut self.poisoned_names);
                mentions_items(&w.then, &mut self.mentioned);
                mentions_items(&w.otherwise, &mut self.mentioned);
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
                mentions_stmts(&otherwise, &mut self.mentioned);
                self.block(&mut then);
                Some(Stmt::Block(then))
            }
            Some(false) => {
                mentions_block(&then, &mut self.mentioned);
                self.stmts(&mut otherwise);
                otherwise.into_iter().next()
            }
            None => {
                mentions_block(&then, &mut self.mentioned);
                mentions_stmts(&otherwise, &mut self.mentioned);
                Some(Stmt::Poison)
            }
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

/// The values of an enum-valued `@target` field (section 24).
fn variants_of(field: &str) -> &'static [&'static str] {
    match field {
        "os" => &OS_VARIANTS,
        "arch" => &ARCH_VARIANTS,
        "cpu" => &CPU_VARIANTS,
        _ => &[],
    }
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

// --------------------------------------------------------------- mentions
//
// The walkers below collect what a removed branch mentions. Every match lists its
// variants, so a new syntax node cannot be left out without a compile error. Field and
// variant names are members, not names in scope, and are not collected.

fn mentions_items(items: &[Item], out: &mut BTreeSet<String>) {
    for item in items {
        match item {
            Item::Fn(f) => mentions_fn(f, out),
            Item::Struct(s) => {
                mentions_generics(&s.generics, out);
                for f in &s.fields {
                    mentions_ty(&f.ty, out);
                    opt_expr(f.align.as_ref(), out);
                }
                opt_expr(s.align.as_ref(), out);
            }
            Item::Enum(e) => {
                mentions_generics(&e.generics, out);
                opt_ty(e.tag.as_ref(), out);
                for v in &e.variants {
                    opt_ty(v.payload.as_ref(), out);
                    opt_expr(v.value.as_ref(), out);
                }
            }
            Item::Interface(i) => {
                mentions_generics(&i.generics, out);
                mentions_paths(&i.bounds, out);
                mentions_items(&i.members, out);
            }
            Item::Impl(i) => {
                mentions_generics(&i.generics, out);
                if let Some(interface) = &i.interface {
                    mentions_path(interface, out);
                }
                mentions_ty(&i.target, out);
                mentions_items(&i.members, out);
            }
            Item::Const(c) => {
                mentions_ty(&c.ty, out);
                mentions_expr(&c.value, out);
            }
            Item::Static(s) => {
                mentions_ty(&s.ty, out);
                opt_expr(s.align.as_ref(), out);
                mentions_expr(&s.value, out);
            }
            Item::TypeAlias(t) => {
                mentions_generics(&t.generics, out);
                mentions_ty(&t.ty, out);
            }
            Item::Extern(x) => {
                for f in &x.functions {
                    mentions_fn(f, out);
                }
            }
            Item::Test(t) => mentions_block(&t.body, out),
            Item::When(w) => {
                mentions_expr(&w.condition, out);
                mentions_items(&w.then, out);
                mentions_items(&w.otherwise, out);
            }
            Item::Poison => {}
        }
    }
}

fn mentions_fn(f: &FnDecl, out: &mut BTreeSet<String>) {
    mentions_generics(&f.generics, out);
    for p in &f.params {
        mentions_param(p, out);
    }
    mentions_ty(&f.ret, out);
    opt_ty(f.error.as_ref(), out);
    for origin in &f.from {
        out.insert(origin.name.clone());
    }
    if let Some(body) = &f.body {
        mentions_block(body, out);
    }
}

fn mentions_param(p: &Param, out: &mut BTreeSet<String>) {
    mentions_ty(&p.ty, out);
    if let Some((name, _)) = &p.into {
        out.insert(name.clone());
    }
}

fn mentions_generics(generics: &[GenericParam], out: &mut BTreeSet<String>) {
    for g in generics {
        mentions_paths(&g.bounds, out);
        opt_ty(g.const_ty.as_ref(), out);
    }
}

fn mentions_block(b: &Block, out: &mut BTreeSet<String>) {
    mentions_stmts(&b.stmts, out);
}

fn mentions_stmts(stmts: &[Stmt], out: &mut BTreeSet<String>) {
    for s in stmts {
        mentions_stmt(s, out);
    }
}

fn mentions_stmt(s: &Stmt, out: &mut BTreeSet<String>) {
    match s {
        Stmt::Let {
            pattern, ty, init, ..
        } => {
            mentions_pattern(pattern, out);
            opt_ty(ty.as_ref(), out);
            mentions_expr(init, out);
        }
        Stmt::Assign { place, value, .. } => {
            mentions_expr(place, out);
            mentions_expr(value, out);
        }
        Stmt::Expr { expr, .. } => mentions_expr(expr, out),
        Stmt::Return { value, .. } => opt_expr(value.as_ref(), out),
        Stmt::Fail { value, .. } => mentions_expr(value, out),
        Stmt::Break { .. } | Stmt::Continue { .. } | Stmt::Poison => {}
        Stmt::Defer { body, .. } | Stmt::Scope { body, .. } | Stmt::Unsafe { body, .. } => {
            mentions_block(body, out)
        }
        Stmt::Block(b) => mentions_block(b, out),
        Stmt::If(i) => {
            mentions_cond(&i.cond, out);
            mentions_block(&i.then, out);
            if let Some(otherwise) = &i.otherwise {
                mentions_stmt(otherwise, out);
            }
        }
        Stmt::While { cond, body, .. } => {
            mentions_cond(cond, out);
            mentions_block(body, out);
        }
        Stmt::Loop { body, .. } => mentions_block(body, out),
        Stmt::For {
            grain,
            iter,
            range_end,
            step,
            body,
            ..
        } => {
            opt_expr(grain.as_ref(), out);
            mentions_expr(iter, out);
            if let Some((_, end)) = range_end {
                mentions_expr(end, out);
            }
            opt_expr(step.as_ref(), out);
            mentions_block(body, out);
        }
        Stmt::Match(m) => {
            mentions_expr(&m.scrutinee, out);
            mentions_arms(&m.arms, out);
        }
        Stmt::Spawn { call, scope, .. } => {
            mentions_expr(call, out);
            mentions_expr(scope, out);
        }
        Stmt::Dispatch {
            call,
            domain,
            executor,
            ..
        } => {
            mentions_expr(call, out);
            mentions_expr(domain, out);
            mentions_expr(executor, out);
        }
        Stmt::Select { arms, .. } => {
            for arm in arms {
                mentions_pattern(&arm.pattern, out);
                mentions_expr(&arm.call, out);
                mentions_block(&arm.body, out);
            }
        }
        Stmt::When {
            condition,
            then,
            otherwise,
            ..
        } => {
            mentions_expr(condition, out);
            mentions_block(then, out);
            mentions_stmts(otherwise, out);
        }
    }
}

fn mentions_cond(c: &Cond, out: &mut BTreeSet<String>) {
    mentions_expr(&c.expr, out);
    if let Some(p) = &c.let_pattern {
        mentions_pattern(p, out);
    }
}

fn mentions_arms(arms: &[Arm], out: &mut BTreeSet<String>) {
    for arm in arms {
        for p in &arm.patterns {
            mentions_pattern(p, out);
        }
        opt_expr(arm.guard.as_ref(), out);
        match &arm.body {
            ArmBody::Block(b) => mentions_block(b, out),
            ArmBody::Expr(e) => mentions_expr(e, out),
        }
    }
}

fn mentions_pattern(p: &Pattern, out: &mut BTreeSet<String>) {
    match p {
        Pattern::Wild(_) | Pattern::Poison => {}
        Pattern::Bind { name, .. } => {
            out.insert(name.clone());
        }
        Pattern::Literal(e) => mentions_expr(e, out),
        Pattern::Range { lo, hi, .. } => {
            mentions_pattern(lo, out);
            mentions_pattern(hi, out);
        }
        Pattern::Variant { path, payload, .. } => {
            if let Some(path) = path {
                mentions_path(path, out);
            }
            if let Some(payload) = payload {
                mentions_pattern(payload, out);
            }
        }
        Pattern::Tuple { elems, .. } => {
            for e in elems {
                mentions_pattern(e, out);
            }
        }
        Pattern::Struct { ty, fields, .. } => {
            if let Some(ty) = ty {
                mentions_path(ty, out);
            }
            for f in fields {
                match &f.pattern {
                    Some(p) => mentions_pattern(p, out),
                    // `Point { x }` binds `x`.
                    None => {
                        out.insert(f.name.clone());
                    }
                }
            }
        }
    }
}

fn mentions_expr(e: &Expr, out: &mut BTreeSet<String>) {
    match &e.kind {
        ExprKind::Int { .. }
        | ExprKind::Float(_)
        | ExprKind::Str(_)
        | ExprKind::CStr(_)
        | ExprKind::Bool(_)
        | ExprKind::Poison => {}
        ExprKind::Ident(name) => {
            out.insert(name.clone());
        }
        ExprKind::Path(p) => mentions_path(p, out),
        ExprKind::Builtin { type_arg, args, .. } => {
            if let Some(t) = type_arg {
                mentions_ty(t, out);
            }
            for a in args {
                mentions_expr(a, out);
            }
        }
        ExprKind::Unary { expr, .. } => mentions_expr(expr, out),
        ExprKind::Binary { lhs, rhs, .. } => {
            mentions_expr(lhs, out);
            mentions_expr(rhs, out);
        }
        ExprKind::Call { callee, args, .. } => {
            mentions_expr(callee, out);
            for a in args {
                mentions_expr(a, out);
            }
        }
        ExprKind::Index { base, index, .. } => {
            mentions_expr(base, out);
            mentions_expr(index, out);
        }
        ExprKind::Slice { base, lo, hi, .. } => {
            mentions_expr(base, out);
            mentions_expr(lo, out);
            mentions_expr(hi, out);
        }
        ExprKind::Field { base, .. }
        | ExprKind::TupleField { base, .. }
        | ExprKind::Deref { base, .. } => mentions_expr(base, out),
        ExprKind::TypeApply { base, args, .. } => {
            mentions_expr(base, out);
            for t in args {
                mentions_ty(t, out);
            }
        }
        ExprKind::Cast { expr, ty, .. } => {
            mentions_expr(expr, out);
            mentions_ty(ty, out);
        }
        ExprKind::StructLit { ty, fields, .. } => {
            mentions_path(ty, out);
            for f in fields {
                mentions_expr(&f.value, out);
            }
        }
        ExprKind::StructShorthand { fields, .. } => {
            for f in fields {
                mentions_expr(&f.value, out);
            }
        }
        ExprKind::EnumLit { path, payload, .. } => {
            if let Some(path) = path {
                mentions_path(path, out);
            }
            opt_expr(payload.as_deref(), out);
        }
        ExprKind::ArrayLit {
            len, elem, elems, ..
        } => {
            mentions_expr(len, out);
            mentions_ty(elem, out);
            for x in elems {
                mentions_expr(x, out);
            }
        }
        ExprKind::Range { lo, hi, .. } => {
            mentions_expr(lo, out);
            mentions_expr(hi, out);
        }
        ExprKind::Tuple { elems, .. } => {
            for x in elems {
                mentions_expr(x, out);
            }
        }
        ExprKind::Paren(inner) => mentions_expr(inner, out),
        ExprKind::If {
            cond,
            then,
            otherwise,
            ..
        } => {
            mentions_cond(cond, out);
            mentions_expr(then, out);
            mentions_expr(otherwise, out);
        }
        ExprKind::Match {
            scrutinee, arms, ..
        } => {
            mentions_expr(scrutinee, out);
            mentions_arms(arms, out);
        }
        ExprKind::Closure {
            captures,
            params,
            ret,
            error,
            body,
            ..
        } => {
            for c in captures {
                out.insert(c.name.clone());
            }
            for p in params {
                mentions_param(p, out);
            }
            mentions_ty(ret, out);
            opt_ty(error.as_ref(), out);
            mentions_block(body, out);
        }
        ExprKind::Catch { value, handler, .. } | ExprKind::Orelse { value, handler, .. } => {
            mentions_expr(value, out);
            match handler {
                Handler::Expr(e) => mentions_expr(e, out),
                Handler::Block(b) => mentions_block(b, out),
            }
        }
        ExprKind::Spawn { call, scope, .. } => {
            mentions_expr(call, out);
            mentions_expr(scope, out);
        }
        ExprKind::Dispatch {
            call,
            domain,
            executor,
            ..
        } => {
            mentions_expr(call, out);
            mentions_expr(domain, out);
            mentions_expr(executor, out);
        }
        ExprKind::Unsafe { body, .. } => mentions_expr(body, out),
    }
}

fn mentions_ty(t: &Ty, out: &mut BTreeSet<String>) {
    match t {
        Ty::Scalar(..) | Ty::Vector(..) | Ty::Poison(_) => {}
        Ty::Path(p) => mentions_path(p, out),
        Ty::Borrow { inner, .. } | Ty::Ptr { inner, .. } | Ty::Optional { inner, .. } => {
            mentions_ty(inner, out)
        }
        Ty::Slice { elem, .. } => mentions_ty(elem, out),
        Ty::Array { len, elem, .. } => {
            mentions_expr(len, out);
            mentions_ty(elem, out);
        }
        Ty::Tuple { elems, .. } => {
            for x in elems {
                mentions_ty(x, out);
            }
        }
        Ty::Fn {
            params, ret, error, ..
        } => {
            for p in params {
                mentions_ty(p, out);
            }
            mentions_ty(ret, out);
            opt_ty(error.as_deref(), out);
        }
        Ty::Dyn { interface, .. } => mentions_path(interface, out),
    }
}

/// A path mentions its first segment: `a` in `a.b.c`, whichever name it resolves to.
fn mentions_path(p: &PathTy, out: &mut BTreeSet<String>) {
    if let Some((first, _)) = p.segments.first() {
        out.insert(first.clone());
    }
    for arg in &p.args {
        match arg {
            GenericArg::Ty(t) => mentions_ty(t, out),
            GenericArg::Const(e) => mentions_expr(e, out),
        }
    }
    if let Some(sig) = &p.fn_sig {
        for t in &sig.params {
            mentions_ty(t, out);
        }
        opt_ty(sig.ret.as_deref(), out);
        opt_ty(sig.error.as_deref(), out);
    }
}

fn mentions_paths(ps: &[PathTy], out: &mut BTreeSet<String>) {
    for p in ps {
        mentions_path(p, out);
    }
}

fn opt_expr(e: Option<&Expr>, out: &mut BTreeSet<String>) {
    if let Some(e) = e {
        mentions_expr(e, out);
    }
}

fn opt_ty(t: Option<&Ty>, out: &mut BTreeSet<String>) {
    if let Some(t) = t {
        mentions_ty(t, out);
    }
}
