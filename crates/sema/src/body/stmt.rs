//! Statements and control flow (spec sections 6.4, 7 and 7.1): bindings, assignment,
//! `if`, `while`, `loop`, `for`, `match`, `return`, `fail`, `break`, `continue`.
//!
//! Every statement function returns whether control can reach the end of the
//! statement (`false` after `return`, `fail`, `break`, `continue`, an expression of
//! type `never`, an endless `loop`): that is how a function that reaches the end of
//! its body without a value is found (E0412).

use super::place::Use;
use super::{Checker, LoopCtx};
use crate::consteval::ValTy;
use crate::hir::{self, BlockId, ExprId, LocalId, PatId, PatKind, StmtId, StmtKind};
use crate::sigs::{Bound, Tri};
use crate::ty::*;
use crate::typed::*;
use ingot_diag::source::Span;
use ingot_syntax::ast::{AssignOp, BinOp};

fn assign_binop(op: AssignOp) -> Option<BinOp> {
    Some(match op {
        AssignOp::Assign => return None,
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
    })
}

impl Checker<'_, '_> {
    /// Check a block. Returns whether control can reach its end.
    pub(crate) fn block(&mut self, b: BlockId) -> bool {
        let hir = self.hir;
        let mut completes = true;
        for &s in &hir.block(b).stmts {
            if !self.stmt(s) {
                completes = false;
            }
        }
        completes
    }

    pub(crate) fn pattern_locals(&self, p: PatId, out: &mut Vec<LocalId>) {
        match &self.hir.pat(p).kind {
            PatKind::Bind { local } => {
                if !out.contains(local) {
                    out.push(*local);
                }
            }
            PatKind::Range { lo, hi, .. } => {
                self.pattern_locals(*lo, out);
                self.pattern_locals(*hi, out);
            }
            PatKind::Variant {
                payload: Some(sub), ..
            } => self.pattern_locals(*sub, out),
            PatKind::Tuple(elems) => {
                for e in elems {
                    self.pattern_locals(*e, out);
                }
            }
            PatKind::Struct { fields, .. } => {
                for f in fields {
                    self.pattern_locals(f.pattern, out);
                }
            }
            _ => {}
        }
    }

    pub(crate) fn stmt(&mut self, s: StmtId) -> bool {
        let hir = self.hir;
        let stmt = hir.stmt(s);
        match &stmt.kind {
            StmtKind::Let {
                mutable,
                pattern,
                ty,
                init,
            } => {
                let declared = ty.as_ref().map(|t| self.lower(t));
                let init_ty = match declared {
                    Some(t) => {
                        self.expect(*init, t);
                        t
                    }
                    None => self.expr(*init, None),
                };
                self.pattern(*pattern, init_ty);
                self.check_irrefutable(*pattern, init_ty);
                self.out.stmts.insert(s, StmtInfo::Let { ty: init_ty });
                let mut locals = Vec::new();
                self.pattern_locals(*pattern, &mut locals);
                for l in &locals {
                    self.let_of.insert(*l, s);
                }
                if *mutable {
                    self.var_stmts.push((s, locals));
                }
                !self.is_never(init_ty)
            }
            StmtKind::Assign { place, op, value } => {
                self.assign(s, *place, *op, *value);
                true
            }
            StmtKind::Discard(e) => {
                let t = self.expr(*e, None);
                if !self.is_poison(t) && !self.is_never(t) {
                    let p = self.cx.d.props(t);
                    match p.discard() {
                        Tri::No => {
                            let at = Span::new(stmt.span.start, stmt.span.start + 1);
                            let name = self.name(t);
                            self.err(
                                "E0414",
                                format!("`_ =` cannot discard a value of the linear type `{name}`"),
                                at,
                            );
                        }
                        Tri::Unknown => self.ni("whether the value of a type the specification does not define the properties of can be discarded"),
                        Tri::Yes => {}
                    }
                }
                !self.is_never(t)
            }
            StmtKind::Expr(e) => self.expr_stmt(*e),
            StmtKind::Return(v) => {
                self.return_stmt(stmt.span, *v);
                false
            }
            StmtKind::Fail(e) => {
                self.fail_stmt(stmt.span, *e);
                false
            }
            StmtKind::Break { target, label } => {
                self.jump(stmt.span, *target, label.is_some(), true);
                false
            }
            StmtKind::Continue { target, label } => {
                self.jump(stmt.span, *target, label.is_some(), false);
                false
            }
            StmtKind::Defer { err, body } => {
                if *err && self.fnctx().error.is_none() && self.fnctx().kind != super::CtxKind::Test
                {
                    let kw = Span::new(stmt.span.start, stmt.span.start + 8);
                    let edit = self.edit(kw, "defer");
                    self.err(
                        "E0709",
                        "`errdefer` in a function that is not fallible: it would never run",
                        kw,
                    )
                    .fix("replace `errdefer` with `defer`", edit);
                }
                self.defer_depth += 1;
                self.block(*body);
                self.defer_depth -= 1;
                true
            }
            StmtKind::Block(b) => self.block(*b),
            StmtKind::If {
                cond,
                then,
                otherwise,
            } => {
                self.if_cond(cond);
                let a = self.block(*then);
                let b = match otherwise {
                    Some(o) => self.stmt(*o),
                    None => true,
                };
                a || b
            }
            StmtKind::While { cond, body, .. } => {
                self.loops.push(LoopCtx {
                    stmt: s,
                    has_break: false,
                    par: false,
                    linear_consume: false,
                    defer_depth: self.defer_depth,
                });
                self.if_cond(cond);
                self.block(*body);
                self.loops.pop();
                true
            }
            StmtKind::Loop { body, .. } => {
                self.loops.push(LoopCtx {
                    stmt: s,
                    has_break: false,
                    par: false,
                    linear_consume: false,
                    defer_depth: self.defer_depth,
                });
                self.block(*body);
                let l = self.loops.pop().expect("a loop");
                l.has_break
            }
            StmtKind::For {
                par,
                grain,
                vars,
                index,
                iter,
                range_end,
                step,
                body,
                ..
            } => {
                self.for_stmt(
                    s,
                    *par,
                    *grain,
                    vars,
                    index.as_ref(),
                    *iter,
                    *range_end,
                    *step,
                    *body,
                );
                true
            }
            StmtKind::Match { scrutinee, arms } => {
                let kw = Span::new(stmt.span.start, stmt.span.start + 5);
                let st = self.expr(*scrutinee, None);
                let mut completes = false;
                for arm in arms {
                    self.arm_patterns(arm, st);
                    if let Some(g) = arm.guard {
                        self.cond(g);
                    }
                    let c = match &arm.body {
                        hir::ArmBody::Block(b) => self.block(*b),
                        hir::ArmBody::Expr(x) => self.expr_stmt(*x),
                    };
                    completes |= c;
                }
                self.exhaustiveness(st, arms, kw, *scrutinee);
                completes
            }
            StmtKind::Scope { local, body, .. } => {
                let ty = match self.std_type("std.task", "Scope") {
                    Some(d) => {
                        let t = self.mk(TyKind::Adt(d, Vec::new()));
                        self.ref_to(Mutability::Shared, t)
                    }
                    None => {
                        self.ni("`scope`: the type `std.task.Scope` is not declared");
                        self.poison()
                    }
                };
                self.bind_local(*local, ty, BindMode::Copy);
                self.out.stmts.insert(s, StmtInfo::Scope { ty });
                self.block(*body)
            }
            StmtKind::Spawn { call, scope } => {
                self.spawn_stmt(*call, *scope);
                true
            }
            StmtKind::Dispatch {
                call,
                domain,
                executor,
            } => {
                let (t, info) = self.dispatch_call(*call, *domain, *executor);
                if let Some(info) = info {
                    self.out.stmts.insert(s, StmtInfo::Dispatch(Box::new(info)));
                }
                if t != self.void() && !self.is_poison(t) {
                    let kw = Span::new(stmt.span.start, stmt.span.start + 8);
                    self.err(
                        "E1519",
                        "this `dispatch` is a statement, but a `reduce` kernel returns a value \
                         that must be used",
                        kw,
                    );
                }
                true
            }
            StmtKind::Select { arms } => {
                let mut completes = false;
                for arm in arms {
                    if let Some(call) = arm.call {
                        let t = self.expr(call, None);
                        if !self.is_poison(t) {
                            self.check_waitable_call(call);
                        }
                        self.pattern(arm.pattern, t);
                    }
                    completes |= self.block(arm.body);
                }
                completes
            }
            StmtKind::Unsafe {
                reason,
                reason_span,
                body,
            } => {
                self.check_unsafe_reason(reason, *reason_span);
                self.unsafe_used.push(false);
                let completes = self.block(*body);
                let used = self.unsafe_used.pop().expect("an unsafe block");
                if !used {
                    let kw = Span::new(stmt.span.start, stmt.span.start + 6);
                    self.err(
                        "E1304",
                        "this `unsafe` block contains no operation that needs it",
                        kw,
                    );
                }
                completes
            }
            StmtKind::Poison => true,
        }
    }

    /// E1409: a `select` arm must call a `waitable fn` directly.
    fn check_waitable_call(&mut self, call: ExprId) {
        let res = self.out.exprs.get(&call).map(|i| i.res.clone());
        let mut callee = None;
        let ok = match &res {
            Some(ExprRes::Call(info)) => match &info.callee {
                Callee::Fn(f) => {
                    if let Some(sig) = self.fn_sig(f.def) {
                        let waitable = sig.mods.waitable;
                        if !waitable {
                            let file = self.cx.file_of(f.def);
                            callee = Some((self.cx.sources.get(file).path.clone(), sig.name_span));
                        }
                        waitable
                    } else {
                        false
                    }
                }
                _ => false,
            },
            _ => false,
        };
        if ok {
            return;
        }
        let span = self.span_of(call);
        let d = self.err(
            "E1409",
            "a `select` arm's operation is not a call of a `waitable fn`",
            span,
        );
        if let Some((file, at)) = callee {
            d.relate_in("callee", &file, at);
        }
    }

    /// An expression used as a statement: its value must be `void` (E0413).
    pub(crate) fn expr_stmt(&mut self, e: ExprId) -> bool {
        let t = self.expr(e, None);
        if self.is_poison(t) || self.is_never(t) || t == self.void() {
            return !self.is_never(t);
        }
        if self.is_result(t) {
            self.unhandled_result(e, t);
            return true;
        }
        let span = self.span_of(e);
        let name = self.name(t);
        let discardable = self.cx.d.props(t).discard() == Tri::Yes;
        let edit = self.edit(Span::empty(span.start), "_ = ");
        if self.branch_tail == Some(span) {
            return true;
        }
        let d = self.err(
            "E0413",
            format!("this call returns `{name}`, which is not used"),
            span,
        );
        if discardable {
            d.fix("discard the value", edit);
        }
        true
    }

    // ------------------------------------------------------------- assignment

    fn assign(&mut self, s: StmtId, place: ExprId, op: AssignOp, value: ExprId) {
        let pt = self.expr(place, None);
        let poison_place = self.is_poison(pt) || self.is_never(pt);
        match assign_binop(op) {
            None => {
                if poison_place {
                    self.expr(value, None);
                    return;
                }
                self.expect(value, pt);
                self.require_mutable_place(place, Use::Assign);
            }
            Some(b) => {
                let vt = if b.is_shift() {
                    self.expr(value, None)
                } else {
                    self.expr(value, if poison_place { None } else { Some(pt) })
                };
                if poison_place {
                    return;
                }
                if self.unhandled_result(place, pt) || self.unhandled_result(value, vt) {
                    return;
                }
                let op_span = self.hir.stmt(s).span;
                let at = Span::new(self.span_of(place).end, self.span_of(value).start);
                let _ = op_span;
                let r = if b.is_shift() {
                    self.shift(b, pt, vt, at)
                } else {
                    self.arith(b, pt, vt, at)
                };
                let _ = r;
                self.out.stmts.insert(s, StmtInfo::Assign { ty: pt });
                self.require_mutable_place(place, Use::Assign);
            }
        }
    }

    // ---------------------------------------------------------------- jumps

    fn return_stmt(&mut self, span: Span, v: Option<ExprId>) {
        let ctx = self.fnctx();
        let kw = Span::new(span.start, span.start + 6);
        if ctx.kind == super::CtxKind::Item {
            self.ni("`return` in the initializer of a constant or static");
        }
        if self.defer_depth > ctx.defer_base {
            self.err(
                "E0708",
                "`return` inside a deferred block: the block runs on the way out and cannot leave again",
                kw,
            );
        }
        if self.par_depth > 0 {
            self.err("E1408", "`return` inside a `par for` body", kw);
        }
        self.reject_exit_of_consuming_loop(kw);
        let void = self.void();
        match v {
            Some(e) => {
                if ctx.ret == void {
                    self.blind_expr(e);
                    self.err(
                        "E0421",
                        "`return` with a value in a function that returns nothing",
                        kw,
                    );
                    return;
                }
                let t = self.expr(e, Some(ctx.ret));
                if !self.compatible(t, ctx.ret) {
                    let own = ctx.error.map(|er| self.mk(TyKind::Result(ctx.ret, er)));
                    if own == Some(t) {
                        let span = self.span_of(e);
                        let edit = self.edit(Span::empty(span.start), "try ");
                        self.err(
                            "E0710",
                            "`return` of a value of the function's own result type; use `try`",
                            span,
                        )
                        .fix("insert `try`", edit);
                    } else if self.dyn_coerce(e, t, ctx.ret).is_none() {
                        self.mismatch(e, t, ctx.ret);
                    }
                }
            }
            None => {
                if ctx.ret != void && !self.is_poison(ctx.ret) && !self.is_never(ctx.ret) {
                    self.err(
                        "E0421",
                        "`return` without a value in a function that has a result",
                        kw,
                    );
                }
            }
        }
    }

    fn fail_stmt(&mut self, stmt_span: Span, e: ExprId) {
        let ctx = self.fnctx();
        if self.defer_depth > ctx.defer_base {
            self.err(
                "E0708",
                "`fail` inside a deferred block: the block runs on the way out and cannot leave again",
                Span::new(stmt_span.start, stmt_span.start + 4),
            );
        }
        if self.par_depth > 0 {
            self.err(
                "E1408",
                "`fail` inside a `par for` body",
                Span::new(stmt_span.start, stmt_span.start + 4),
            );
        }
        let kw = Span::new(stmt_span.start, stmt_span.start + 4);
        self.reject_exit_of_consuming_loop(kw);
        if self.kernel {
            self.err("E1511", "a kernel cannot fail", kw);
            self.expr(e, None);
            return;
        }
        match ctx.error {
            Some(et) => {
                let t = self.expr(e, Some(et));
                if !self.compatible(t, et) {
                    let span = self.span_of(e);
                    let path = self.file_path();
                    let sig = ctx.error_span;
                    let fix = self.variant_from(et, t).map(|v| {
                        let text = self.text(span).to_string();
                        let name = v
                            .def
                            .map(|d| self.cx.sema.def(d).name.clone())
                            .unwrap_or_default();
                        self.edit(span, format!(".{name}({text})"))
                    });
                    let d = self.err(
                        "E0706",
                        "the operand of `fail` is not the function's error type",
                        span,
                    );
                    if let Some(edit) = fix {
                        d.fix("wrap it in the `from` variant", edit);
                    }
                    if let Some(sig) = sig {
                        d.relate_in("signature", &path, sig);
                    }
                }
            }
            None => {
                self.blind_expr(e);
                self.err("E0705", "`fail` in a function that is not fallible", kw);
            }
        }
    }

    /// E0613: leaving a `for` that consumes an array of linear elements.
    fn reject_exit_of_consuming_loop(&mut self, at: Span) {
        let base = self.fnctx().loop_base;
        if self.loops[base..].iter().any(|l| l.linear_consume) {
            self.err(
                "E0613",
                "the loop consumes an array of linear elements, so it cannot be left early",
                at,
            );
        }
    }

    pub(crate) fn reject_try_in_consuming_loop(&mut self, at: Span) {
        self.reject_exit_of_consuming_loop(at);
    }

    fn jump(&mut self, span: Span, target: Option<StmtId>, labelled: bool, is_break: bool) {
        let word = if is_break { "break" } else { "continue" };
        let kw = Span::new(span.start, span.start + word.len());
        let base = self.fnctx().loop_base;
        let idx = match target {
            Some(t) => self.loops.iter().rposition(|l| l.stmt == t),
            None => {
                if self.loops.len() > base {
                    Some(self.loops.len() - 1)
                } else {
                    None
                }
            }
        };
        let Some(idx) = idx.filter(|i| *i >= base) else {
            // A label that names no loop was reported by name resolution (E0317).
            if !labelled {
                self.err("E0605", format!("`{word}` outside a loop"), kw);
            }
            return;
        };
        if is_break {
            self.loops[idx].has_break = true;
        }
        let leaves = if is_break { idx } else { idx + 1 };
        if self.loops[leaves.min(self.loops.len())..]
            .iter()
            .any(|l| l.linear_consume)
        {
            self.err(
                "E0613",
                "the loop consumes an array of linear elements, so it cannot be left early",
                kw,
            );
        }
        let par_leave =
            self.loops[idx + 1..].iter().any(|l| l.par) || (is_break && self.loops[idx].par);
        if par_leave {
            self.err("E1408", format!("`{word}` leaves a `par for` body"), kw);
        }
        if self.loops[idx].defer_depth < self.defer_depth {
            self.err("E0708", format!("`{word}` leaves a deferred block"), kw);
        }
    }

    // ------------------------------------------------------------------- for

    #[allow(clippy::too_many_arguments)]
    fn for_stmt(
        &mut self,
        s: StmtId,
        par: bool,
        grain: Option<ExprId>,
        vars: &[hir::ForVar],
        index: Option<&hir::ForVar>,
        iter: ExprId,
        range_end: Option<(bool, ExprId)>,
        step: Option<ExprId>,
        body: BlockId,
    ) {
        if par && !self.kernel {
            self.ni_unchecked(
                "E1401 not_send, E1402 capture_not_sync and E1407 par_for_outer_mutation \
                 (phase 9, S6 to S9)",
            );
        }
        if let Some(g) = grain {
            let i64_ = self.i64_();
            self.expect(g, i64_);
        }
        let poison = self.poison();
        let (info, linear) = match range_end {
            Some((inclusive, hi)) => (self.for_range(iter, hi, inclusive, step), false),
            None => {
                if let Some(st) = step {
                    self.expr(st, None);
                }
                if par {
                    let t = self.expr(iter, None);
                    if self.is_poison(t) || self.is_never(t) {
                        (None, false)
                    } else if matches!(self.kind(t), TyKind::Slice(..)) {
                        self.for_collection_typed(iter, t)
                    } else {
                        if !self.kernel {
                            self.ni(
                                "E1406 par_for_not_splittable: whether this type implements \
                                 `par.Split`, which the standard library does not declare yet",
                            );
                        }
                        (None, false)
                    }
                } else {
                    self.for_collection(iter)
                }
            }
        };
        let item = info.as_ref().map(|i| i.item).unwrap_or(poison);
        for v in vars {
            if let Some(l) = v.local {
                let mode = if self.is_copy(item) {
                    BindMode::Copy
                } else {
                    BindMode::Move
                };
                self.bind_local(l, item, mode);
            }
        }
        if let Some(ix) = index
            && let Some(l) = ix.local
        {
            let ty = if info.is_some() { self.i64_() } else { poison };
            self.bind_local(l, ty, BindMode::Copy);
        }
        if let Some(i) = info {
            self.out.stmts.insert(s, StmtInfo::For(i));
        }
        self.loops.push(LoopCtx {
            stmt: s,
            has_break: false,
            par,
            linear_consume: linear,
            defer_depth: self.defer_depth,
        });
        if par {
            self.par_depth += 1;
        }
        self.block(body);
        if par {
            self.par_depth -= 1;
        }
        self.loops.pop();
    }

    fn for_range(
        &mut self,
        lo: ExprId,
        hi: ExprId,
        inclusive: bool,
        step: Option<ExprId>,
    ) -> Option<ForInfo> {
        let (l, r) = self.operands(lo, hi, None);
        if self.is_poison(l) || self.is_poison(r) {
            if let Some(st) = step {
                self.expr(st, None);
            }
            return None;
        }
        if l != r {
            let (a, b) = (self.name(l), self.name(r));
            let span = self.span_of(hi);
            self.err(
                "E0609",
                format!("the bounds of a range have different types, `{a}` and `{b}`"),
                span,
            );
            return None;
        }
        let Some(it) = self.int_of(l) else {
            let name = self.name(l);
            let span = self.span_of(lo);
            if matches!(self.kind(l), TyKind::Param(_)) {
                self.ni("a range over a type parameter");
                return None;
            }
            self.err(
                "E0609",
                format!("a range is over integers, not `{name}`"),
                span,
            );
            return None;
        };
        if let Some(st) = step {
            self.expr(st, Some(l));
            if self.uses_local(st) {
                let span = self.span_of(st);
                self.err(
                    "E0609",
                    "the step of a range is a positive constant, and this uses a variable",
                    span,
                );
            } else {
                self.check_step(st, it);
            }
        }
        Some(ForInfo {
            kind: ForKind::Range { ty: l, inclusive },
            item: l,
        })
    }

    /// Whether the expression names a local variable anywhere (then it is not constant).
    fn uses_local(&self, e: ExprId) -> bool {
        use crate::hir::{ExprKind, Res};
        match &self.hir.expr(e).kind {
            ExprKind::Path(p) => matches!(p.res, Res::Local(_)),
            ExprKind::Paren(i) => self.uses_local(*i),
            ExprKind::Unary { expr, .. } => self.uses_local(*expr),
            ExprKind::Cast { expr, .. } => self.uses_local(*expr),
            ExprKind::Binary { lhs, rhs, .. } => self.uses_local(*lhs) || self.uses_local(*rhs),
            ExprKind::Call { args, .. } => args.iter().any(|a| self.uses_local(*a)),
            _ => false,
        }
    }

    /// A `step` is a positive constant.
    fn check_step(&mut self, st: ExprId, it: IntTy) {
        let span = self.span_of(st);
        match self
            .cx
            .eval_expr(self.pkg, self.file, st, Some(ValTy::Int(it)))
        {
            Ok(crate::ty::ConstValue::Int { value, .. }) => {
                if value <= 0 {
                    self.err("E0609", "the step of a range is a positive constant", span);
                }
            }
            Ok(_) => {}
            Err(crate::consteval::Fail::Reported) => {
                self.errors += 1;
            }
            Err(crate::consteval::Fail::NotImplemented(m)) => {
                self.ni(m);
            }
            Err(crate::consteval::Fail::Generic) => {
                self.ni("a `step` that depends on a generic parameter");
            }
            Err(crate::consteval::Fail::OutOfRange(sp)) => {
                self.err("E0405", "the step does not fit the type of the range", sp);
            }
        }
    }

    /// `for x in e`: what `e` is, and the type of `x`. The flag: the loop consumes an
    /// array of linear elements.
    fn for_collection(&mut self, iter: ExprId) -> (Option<ForInfo>, bool) {
        let t = self.expr(iter, None);
        self.for_collection_typed(iter, t)
    }

    fn for_collection_typed(&mut self, iter: ExprId, t: TypeId) -> (Option<ForInfo>, bool) {
        if self.is_poison(t) || self.is_never(t) {
            return (None, false);
        }
        let span = self.span_of(iter);
        match self.kind(t) {
            TyKind::Slice(Mutability::Mut, el) => {
                let item = self.ref_to(Mutability::Mut, el);
                (
                    Some(ForInfo {
                        kind: ForKind::ExclusiveSlice,
                        item,
                    }),
                    false,
                )
            }
            TyKind::Slice(Mutability::Shared, el) => {
                if self.is_copy(el) {
                    (
                        Some(ForInfo {
                            kind: ForKind::Copying,
                            item: el,
                        }),
                        false,
                    )
                } else {
                    let name = self.name(el);
                    let edit = self.edit(Span::empty(span.end), ".refs()");
                    self.err(
                        "E0608",
                        format!("a shared slice of `{name}` is iterated by copy, and `{name}` is not `Copy`"),
                        span,
                    )
                    .fix("iterate by shared borrow", edit);
                    (None, false)
                }
            }
            TyKind::Array(el, _) => {
                if self.is_copy(el) {
                    (
                        Some(ForInfo {
                            kind: ForKind::Copying,
                            item: el,
                        }),
                        false,
                    )
                } else {
                    let linear = self.cx.d.props(el).maybe_linear != Tri::No;
                    (
                        Some(ForInfo {
                            kind: ForKind::Consuming { linear },
                            item: el,
                        }),
                        linear,
                    )
                }
            }
            _ => match self.iter_item(t) {
                Some(info) => (Some(info), false),
                None => {
                    let name = self.name(t);
                    self.err(
                        "E0608",
                        format!("`{name}` is not a range, array, slice or `Iter`"),
                        span,
                    );
                    (None, false)
                }
            },
        }
    }

    /// The item type and `next` function of a value whose type implements `Iter`.
    fn iter_item(&mut self, t: TypeId) -> Option<ForInfo> {
        let iter = self.cx.d.builtins.iter?;
        if self.cx.d.satisfies(t, &Bound::Interface(iter, Vec::new())) != Tri::Yes {
            return None;
        }
        let assoc = *self.cx.d.interface_sig(iter)?.assoc.first()?;
        let proj = self.mk(TyKind::Projection(t, assoc));
        let item = self.cx.d.normalize(proj);
        // The `next` of `Iter` (section 7.1), not an inherent function that happens to
        // have the same name.
        let cands = self.cx.d.method_candidates(t, "next");
        let c = cands.into_iter().find(|c| match &c.source {
            crate::traits::MethodSource::Impl { interface, .. }
            | crate::traits::MethodSource::Builtin { interface }
            | crate::traits::MethodSource::Bound { interface, .. } => *interface == iter,
            crate::traits::MethodSource::Inherent(_) => false,
        })?;
        let next = self.fn_ref_of_candidate(&c, t);
        Some(ForInfo {
            kind: ForKind::Iter { iter_ty: t, next },
            item,
        })
    }
}
