//! Kernel and kernel-helper body rules (spec section 27.1, pipeline section 1).
//!
//! A kernel body and a kernel helper body each follow the same rules: no allocation,
//! borrowing, blocking, failing, `unsafe`, or trapping integer operation, and calls of
//! kernel helpers only. The checks are written once over a typed body and run in phase 7
//! for every `kernel fn`. The only check across bodies is a cycle among helpers.

use crate::defs::DefKind;
use crate::hir::{self, ExprId, ExprKind, LocalId, LocalKind, StmtKind};
use crate::lowering::Cx;
use crate::sigs::ItemSig;
use crate::ty::*;
use crate::typed::{Bodies, Callee, ExprRes, FnSource, ReceiverMode, TypedBody};
use ingot_diag::source::Span;
use std::collections::{BTreeMap, BTreeSet};

/// What a body needs to know about the kernel whose body it is. `index` is `None` for a
/// kernel helper.
#[derive(Clone, Default)]
struct KernelCtx {
    index: Option<LocalId>,
    slices: BTreeSet<LocalId>,
}

#[derive(Clone)]
struct Finding {
    def: crate::DefId,
    file: usize,
    span: Span,
    code: &'static str,
    message: String,
}

impl Cx<'_> {
    /// Phase 7: the body rules for every `kernel fn` (kernel and helper), each body on
    /// its own.
    pub(crate) fn check_kernel_bodies(&mut self, bodies: &mut Bodies) {
        let kernels: Vec<crate::DefId> = bodies
            .bodies
            .iter()
            .filter_map(|(&d, b)| {
                (b.kind == crate::typed::BodyKind::Fn
                    && matches!(self.hir_item(d), Some(hir::Item::Fn(f)) if f.mods.kernel))
                .then_some(d)
            })
            .collect();
        let mut reported: BTreeSet<(crate::DefId, usize, usize, &'static str)> = BTreeSet::new();
        for def in kernels {
            let Some(body) = bodies.bodies.get(&def) else {
                continue;
            };
            if body.failed {
                continue;
            }
            let ctx = self.kernel_ctx(def);
            let mut found = self.body_findings(def, body, ctx.as_ref());
            if !found.is_empty()
                && let Some(b) = bodies.bodies.get_mut(&def)
            {
                b.failed = true;
            }
            found.sort_by_key(|f| (f.file, f.span.start, f.span.end, f.code));
            for f in found {
                if !reported.insert((f.def, f.span.start, f.span.end, f.code)) {
                    continue;
                }
                self.error(f.code, f.message, f.file, f.span);
            }
        }
    }

    /// The one cross-body check: a cycle of helper-to-helper calls, reported at the call
    /// that closes it.
    pub(crate) fn check_helper_cycles(&mut self, bodies: &mut Bodies) {
        let helpers: BTreeSet<crate::DefId> = bodies
            .bodies
            .iter()
            .filter_map(|(&d, b)| {
                (b.kind == crate::typed::BodyKind::Fn
                    && matches!(self.hir_item(d), Some(hir::Item::Fn(f)) if f.mods.kernel)
                    && self.d.fn_sig(d).is_some_and(|s| self.is_helper_sig(s)))
                .then_some(d)
            })
            .collect();
        let mut edges: BTreeMap<crate::DefId, Vec<(crate::DefId, usize, Span)>> = BTreeMap::new();
        for (&def, body) in &bodies.bodies {
            if !helpers.contains(&def) {
                continue;
            }
            let file = self.file_of(def);
            let mut out = Vec::new();
            for (at, callee) in self.body_calls(body) {
                if helpers.contains(&callee) {
                    out.push((callee, file, self.pkgs[body.package].expr(at).span));
                }
            }
            out.sort_by_key(|(_, _, span)| (span.start, span.end));
            edges.insert(def, out);
        }
        let mut state: BTreeMap<crate::DefId, u8> = BTreeMap::new();
        let mut stack = Vec::new();
        let mut findings: Vec<(crate::DefId, usize, Span, String)> = Vec::new();
        for &def in &helpers {
            if state.get(&def).copied().unwrap_or(0) == 0 {
                self.helper_cycle_dfs(def, &edges, &mut state, &mut stack, &mut findings);
            }
        }
        for (caller, file, span, message) in findings {
            if let Some(b) = bodies.bodies.get_mut(&caller) {
                b.failed = true;
            }
            self.error("E1511", message, file, span);
        }
    }

    fn helper_cycle_dfs(
        &self,
        node: crate::DefId,
        edges: &BTreeMap<crate::DefId, Vec<(crate::DefId, usize, Span)>>,
        state: &mut BTreeMap<crate::DefId, u8>,
        stack: &mut Vec<crate::DefId>,
        out: &mut Vec<(crate::DefId, usize, Span, String)>,
    ) {
        state.insert(node, 1);
        stack.push(node);
        for (callee, file, span) in edges.get(&node).cloned().unwrap_or_default() {
            match state.get(&callee).copied().unwrap_or(0) {
                0 => self.helper_cycle_dfs(callee, edges, state, stack, out),
                1 => {
                    let name = self.sema.def(callee).name.clone();
                    out.push((
                        node,
                        file,
                        span,
                        format!(
                            "`{name}` is already on the helper call chain; a kernel helper \
                             cannot recurse"
                        ),
                    ));
                }
                _ => {}
            }
        }
        stack.pop();
        state.insert(node, 2);
    }

    fn kernel_ctx(&mut self, def: crate::DefId) -> Option<KernelCtx> {
        let f = match self.hir_item(def) {
            Some(hir::Item::Fn(f)) => f.clone(),
            _ => return None,
        };
        if !f.mods.kernel {
            return None;
        }
        self.ensure_sig(def);
        let sig = self.d.fn_sig(def).cloned()?;
        let first_is_idx = sig.params.first().is_some_and(|s| self.is_idx_type(s.ty));
        let mut ctx = KernelCtx {
            index: if first_is_idx {
                f.params.first().map(|p| p.local)
            } else {
                None
            },
            slices: BTreeSet::new(),
        };
        if first_is_idx {
            for (p, s) in f.params.iter().zip(&sig.params).skip(1) {
                if matches!(self.d.types.kind(s.ty), TyKind::Slice(..)) {
                    ctx.slices.insert(p.local);
                }
            }
        }
        Some(ctx)
    }

    /// Direct calls of one typed body, in source order.
    fn body_calls(&self, body: &TypedBody) -> Vec<(ExprId, crate::DefId)> {
        let mut calls: Vec<(usize, ExprId, crate::DefId)> = body
            .exprs
            .iter()
            .filter_map(|(&e, info)| match &info.res {
                ExprRes::Call(c) => match &c.callee {
                    Callee::Fn(f) => {
                        let span = self.pkgs[body.package].expr(e).span;
                        Some((span.start, e, f.def))
                    }
                    _ => None,
                },
                _ => None,
            })
            .collect();
        calls.sort();
        calls.into_iter().map(|(_, e, d)| (e, d)).collect()
    }

    /// Every kernel-rule finding in one typed body.
    fn body_findings(
        &mut self,
        def: crate::DefId,
        body: &TypedBody,
        kernel: Option<&KernelCtx>,
    ) -> Vec<Finding> {
        let pkg = body.package;
        let file = self.file_of(def);
        let mut out = Vec::new();
        let explained = self.explained_locals(pkg, body);
        let add = |out: &mut Vec<Finding>, span: Span, code: &'static str, message: &str| {
            out.push(Finding {
                def,
                file,
                span,
                code,
                message: message.to_string(),
            });
        };
        // Locals: a kernel body may hold `Idx` and a payload-free enum; everything else
        // must be an element or uniform type.
        for (&local, info) in &body.locals {
            let kind = self.pkgs[pkg].local(local).kind;
            if kind == LocalKind::Param {
                continue;
            }
            if !matches!(kind, LocalKind::Let | LocalKind::For) {
                continue;
            }
            if kernel.and_then(|k| k.index) == Some(local) {
                continue;
            }
            // A construct that makes a value of one of these types is reported where it
            // is written; the local that holds it follows from it.
            if explained.contains(&local) {
                continue;
            }
            let ok = self.kernel_uniform(info.ty, 0)
                || self.kernel_payload_free_enum(info.ty)
                || (kernel.and_then(|k| k.index).is_some() && self.is_idx_type(info.ty))
                || info.ty == self.d.types.void
                || info.ty == self.d.types.never;
            if !ok {
                let span = self.pkgs[pkg].local(local).span;
                let name = self.ty_name(info.ty);
                add(
                    &mut out,
                    span,
                    "E1511",
                    &format!(
                        "this local has type `{name}`; a kernel holds only element and uniform \
                         types"
                    ),
                );
            }
        }
        // Expressions.
        for (&e, info) in &body.exprs {
            let expr = self.pkgs[pkg].expr(e).clone();
            match &expr.kind {
                ExprKind::Unary {
                    op: ingot_syntax::ast::UnOp::Borrow | ingot_syntax::ast::UnOp::BorrowMut,
                    ..
                } => add(
                    &mut out,
                    expr.span,
                    "E1511",
                    "a kernel cannot borrow (`&` or `&mut`)",
                ),
                ExprKind::Unary {
                    op: ingot_syntax::ast::UnOp::Try,
                    ..
                } => add(
                    &mut out,
                    expr.span,
                    "E1511",
                    "a kernel cannot use `try` or a fallible operation",
                ),
                ExprKind::Unary {
                    op: ingot_syntax::ast::UnOp::Neg,
                    expr: operand,
                    op_span,
                } => {
                    if let Some(t) = self.ty_of(body, *operand)
                        && matches!(self.d.types.kind(t), TyKind::Int(_))
                    {
                        let name = self.ty_name(t);
                        add(
                            &mut out,
                            *op_span,
                            "E1512",
                            &format!("unary `-` on `{name}` traps in a kernel; write `-%x`"),
                        );
                    }
                }
                ExprKind::Binary {
                    op,
                    lhs,
                    rhs,
                    op_span,
                } => {
                    let int = self
                        .ty_of(body, *lhs)
                        .is_some_and(|t| matches!(self.d.types.kind(t), TyKind::Int(_)));
                    if int
                        && matches!(
                            op,
                            ingot_syntax::ast::BinOp::Add
                                | ingot_syntax::ast::BinOp::Sub
                                | ingot_syntax::ast::BinOp::Mul
                                | ingot_syntax::ast::BinOp::Div
                                | ingot_syntax::ast::BinOp::Rem
                        )
                    {
                        let name = self
                            .ty_of(body, *lhs)
                            .map(|t| self.ty_name(t))
                            .unwrap_or_default();
                        add(
                            &mut out,
                            *op_span,
                            "E1512",
                            &format!(
                                "`{}` on `{name}` traps in a kernel; write `+%`, `+|`, `-%`, \
                                 `@div_or` or `@rem_or`",
                                op.symbol()
                            ),
                        );
                    }
                    if matches!(
                        op,
                        ingot_syntax::ast::BinOp::Shl | ingot_syntax::ast::BinOp::Shr
                    ) {
                        match self.constness(pkg, file, *rhs) {
                            Constness::Runtime => add(
                                &mut out,
                                *op_span,
                                "E1512",
                                "this shift count is not a constant; write `@shl` or `@shr`",
                            ),
                            Constness::Unimplemented => {}
                            _ => {}
                        }
                    }
                }
                ExprKind::Builtin { name, .. }
                    if matches!(name.as_str(), "assert" | "panic" | "unreachable" | "narrow") =>
                {
                    add(
                        &mut out,
                        expr.span,
                        "E1512",
                        &format!("`@{name}` is not available in a kernel"),
                    );
                }
                ExprKind::Closure(_) => add(
                    &mut out,
                    expr.span,
                    "E1511",
                    "a kernel cannot write a closure",
                ),
                ExprKind::Block(b) => self.scan_block(def, file, pkg, body, *b, kernel, &mut out),
                ExprKind::Catch { .. } | ExprKind::Orelse { .. } => add(
                    &mut out,
                    expr.span,
                    "E1511",
                    "a kernel cannot handle errors (`catch`, `orelse`)",
                ),
                ExprKind::Spawn { .. } | ExprKind::Dispatch { .. } => add(
                    &mut out,
                    expr.span,
                    "E1511",
                    "a kernel cannot spawn or dispatch",
                ),
                ExprKind::Unsafe { .. } => {
                    add(&mut out, expr.span, "E1511", "a kernel cannot use `unsafe`")
                }
                ExprKind::Match { scrutinee, .. } => {
                    if self.payload_enum(*scrutinee, body) {
                        let name = self
                            .ty_of(body, *scrutinee)
                            .map(|t| self.ty_name(t))
                            .unwrap_or_default();
                        add(
                            &mut out,
                            expr.span,
                            "E1511",
                            &format!(
                                "this match is on `{name}`, which has a payload; a kernel matches \
                                 only integers, `bool` and payload-free enums"
                            ),
                        );
                    }
                }
                ExprKind::Call { .. } => self.call_findings(e, body, &mut out),
                ExprKind::Index { base, index, .. } => {
                    let base_local = self.local_of(pkg, *base);
                    let index_local = self.local_of(pkg, *index);
                    let index_own =
                        index_local.is_some_and(|l| kernel.and_then(|k| k.index) == Some(l));
                    let index_is_idx = self
                        .ty_of(body, *index)
                        .is_some_and(|t| self.is_idx_type(t));
                    let base_slice =
                        base_local.is_some_and(|l| kernel.is_some_and(|k| k.slices.contains(&l)));
                    if base_slice && !index_own {
                        let base_name = base_local
                            .map(|l| self.pkgs[pkg].local(l).name.clone())
                            .unwrap_or_else(|| "this input or output".to_string());
                        let index_name = index_local
                            .map(|l| format!("`{}`", self.pkgs[pkg].local(l).name))
                            .unwrap_or_else(|| "this index".to_string());
                        let kernel_name = kernel
                            .and_then(|k| k.index)
                            .map(|l| format!("`{}`", self.pkgs[pkg].local(l).name))
                            .unwrap_or_else(|| "the kernel's own index".to_string());
                        add(
                            &mut out,
                            self.pkgs[pkg].expr(*index).span,
                            "E1513",
                            &format!(
                                "`{base_name}` is indexed at {index_name}, not at the kernel's \
                                 own index {kernel_name}"
                            ),
                        );
                    } else if !base_slice && index_is_idx {
                        add(
                            &mut out,
                            self.pkgs[pkg].expr(*index).span,
                            "E1513",
                            "this `Idx` indexes a value that is not a kernel input or output; \
                             `Idx` indexes a kernel input or output only at the kernel's own index",
                        );
                    }
                }
                ExprKind::ArrayLit { .. }
                | ExprKind::Tuple(_)
                | ExprKind::Str(_)
                | ExprKind::CStr(_) => {
                    let message = self
                        .ty_of(body, e)
                        .map(|t| {
                            format!(
                                "this value has type `{}`; a kernel value is an element or uniform \
                                 type",
                                self.ty_name(t)
                            )
                        })
                        .unwrap_or_else(|| {
                            "this value is not an element or uniform type".to_string()
                        });
                    add(&mut out, expr.span, "E1511", &message);
                }
                ExprKind::Slice { .. } => add(
                    &mut out,
                    expr.span,
                    "E1511",
                    "a kernel cannot slice; index the input or output at the kernel's index",
                ),
                ExprKind::StructLit { .. } | ExprKind::StructShorthand { .. } => {
                    if let Some(t) = self.ty_of(body, e)
                        && !self.kernel_uniform(t, 0)
                        && !self.kernel_payload_free_enum(t)
                    {
                        let name = self.ty_name(t);
                        add(
                            &mut out,
                            expr.span,
                            "E1511",
                            &format!(
                                "this value has type `{name}`; a kernel value is an element or \
                                 uniform type"
                            ),
                        );
                    }
                }
                ExprKind::EnumLit { .. } => {
                    if let Some(t) = self.ty_of(body, e)
                        && !self.kernel_payload_free_enum(t)
                    {
                        let name = self.ty_name(t);
                        add(
                            &mut out,
                            expr.span,
                            "E1511",
                            &format!(
                                "`{name}` is not payload-free; a kernel value is an element, \
                                 uniform or payload-free enum type"
                            ),
                        );
                    }
                }
                _ => {}
            }
            let _ = info;
        }
        // Statements, including the ones that are not in `body.stmts`.
        if let crate::typed::BodyRoot::Block(root) = body.root {
            self.scan_block(def, file, pkg, body, root, kernel, &mut out);
        }
        out
    }

    #[allow(clippy::too_many_arguments)]
    fn scan_block(
        &mut self,
        def: crate::DefId,
        file: usize,
        pkg: usize,
        body: &TypedBody,
        block: hir::BlockId,
        kernel: Option<&KernelCtx>,
        out: &mut Vec<Finding>,
    ) {
        let stmts: Vec<hir::StmtId> = self.pkgs[pkg].block(block).stmts.clone();
        for s in stmts {
            self.scan_stmt(def, file, pkg, body, s, kernel, out);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn scan_stmt(
        &mut self,
        def: crate::DefId,
        file: usize,
        pkg: usize,
        body: &TypedBody,
        s: hir::StmtId,
        kernel: Option<&KernelCtx>,
        out: &mut Vec<Finding>,
    ) {
        let stmt = self.pkgs[pkg].stmt(s).clone();
        match &stmt.kind {
            StmtKind::Defer { body: b, .. } => {
                out.push(finding(
                    def,
                    file,
                    stmt.span,
                    "E1511",
                    "a kernel cannot use `defer` or `errdefer`",
                ));
                self.scan_block(def, file, pkg, body, *b, kernel, out);
            }
            StmtKind::Scope { body: b, .. } => {
                out.push(finding(
                    def,
                    file,
                    stmt.span,
                    "E1511",
                    "a kernel cannot open a `scope`",
                ));
                self.scan_block(def, file, pkg, body, *b, kernel, out);
            }
            StmtKind::Spawn { .. } => out.push(finding(
                def,
                file,
                stmt.span,
                "E1511",
                "a kernel cannot spawn",
            )),
            StmtKind::Select { .. } => out.push(finding(
                def,
                file,
                stmt.span,
                "E1511",
                "a kernel cannot block on a `select`",
            )),
            StmtKind::Dispatch { .. } => out.push(finding(
                def,
                file,
                stmt.span,
                "E1511",
                "a kernel cannot dispatch",
            )),
            StmtKind::Unsafe { body: b, .. } => {
                out.push(finding(
                    def,
                    file,
                    stmt.span,
                    "E1511",
                    "a kernel cannot use `unsafe`",
                ));
                self.scan_block(def, file, pkg, body, *b, kernel, out);
            }
            StmtKind::Fail(_) => out.push(finding(
                def,
                file,
                stmt.span,
                "E1511",
                "a kernel cannot fail",
            )),
            StmtKind::Block(b) => self.scan_block(def, file, pkg, body, *b, kernel, out),
            StmtKind::If {
                then, otherwise, ..
            } => {
                self.scan_block(def, file, pkg, body, *then, kernel, out);
                if let Some(o) = otherwise {
                    self.scan_stmt(def, file, pkg, body, *o, kernel, out);
                }
            }
            StmtKind::While { body: b, .. } => {
                self.scan_block(def, file, pkg, body, *b, kernel, out)
            }
            StmtKind::Loop { body: b, .. } => {
                self.scan_block(def, file, pkg, body, *b, kernel, out)
            }
            StmtKind::For {
                par,
                iter,
                range_end,
                step,
                body: b,
                ..
            } => {
                if *par {
                    out.push(finding(
                        def,
                        file,
                        stmt.span,
                        "E1511",
                        "a kernel cannot use `par for`",
                    ));
                } else {
                    match range_end {
                        None => out.push(finding(
                            def,
                            file,
                            stmt.span,
                            "E1511",
                            "this loop range is not constant; a kernel loops only over a constant \
                             range",
                        )),
                        Some((_, hi)) => {
                            let mut bad = false;
                            let mut exprs = vec![*iter, *hi];
                            if let Some(st) = step {
                                exprs.push(*st);
                            }
                            for e in exprs {
                                match self.constness(pkg, file, e) {
                                    Constness::Runtime => bad = true,
                                    Constness::Unimplemented => {}
                                    _ => {}
                                }
                            }
                            if bad {
                                out.push(finding(
                                    def,
                                    file,
                                    stmt.span,
                                    "E1511",
                                    "this loop range is not constant; a kernel loops only over a \
                                     constant range",
                                ));
                            }
                        }
                    }
                }
                self.scan_block(def, file, pkg, body, *b, kernel, out);
            }
            StmtKind::Match { scrutinee, arms } => {
                if self.payload_enum(*scrutinee, body) {
                    let name = self
                        .ty_of(body, *scrutinee)
                        .map(|t| self.ty_name(t))
                        .unwrap_or_default();
                    out.push(finding(
                        def,
                        file,
                        stmt.span,
                        "E1511",
                        &format!(
                            "this match is on `{name}`, which has a payload; a kernel matches only \
                             integers, `bool` and payload-free enums"
                        ),
                    ));
                }
                for arm in arms {
                    match &arm.body {
                        hir::ArmBody::Block(b) => {
                            self.scan_block(def, file, pkg, body, *b, kernel, out)
                        }
                        hir::ArmBody::Expr(_) => {}
                    }
                }
            }
            StmtKind::Assign { place, op, value } => {
                let int = self
                    .ty_of(body, *place)
                    .is_some_and(|t| matches!(self.d.types.kind(t), TyKind::Int(_)));
                if int
                    && matches!(
                        op,
                        ingot_syntax::ast::AssignOp::Add
                            | ingot_syntax::ast::AssignOp::Sub
                            | ingot_syntax::ast::AssignOp::Mul
                            | ingot_syntax::ast::AssignOp::Div
                            | ingot_syntax::ast::AssignOp::Rem
                    )
                {
                    let at = Span::new(
                        self.pkgs[pkg].expr(*place).span.end,
                        self.pkgs[pkg].expr(*value).span.start,
                    );
                    let name = self
                        .ty_of(body, *place)
                        .map(|t| self.ty_name(t))
                        .unwrap_or_default();
                    out.push(finding(
                        def,
                        file,
                        at,
                        "E1512",
                        &format!(
                            "`{}` on `{name}` traps in a kernel; write `+%`, `+|`, `-%`, \
                             `@div_or` or `@rem_or`",
                            op.symbol()
                        ),
                    ));
                }
                if matches!(
                    op,
                    ingot_syntax::ast::AssignOp::Shl | ingot_syntax::ast::AssignOp::Shr
                ) && matches!(self.constness(pkg, file, *value), Constness::Runtime)
                {
                    let at = Span::new(
                        self.pkgs[pkg].expr(*place).span.end,
                        self.pkgs[pkg].expr(*value).span.start,
                    );
                    out.push(finding(
                        def,
                        file,
                        at,
                        "E1512",
                        "this shift count is not a constant; write `@shl` or `@shr`",
                    ));
                }
            }
            _ => {}
        }
    }

    fn call_findings(&mut self, e: ExprId, body: &TypedBody, out: &mut Vec<Finding>) {
        let Some(ExprRes::Call(info)) = body.exprs.get(&e).map(|i| &i.res) else {
            return;
        };
        let file = self.file_of(body.owner);
        let span = self.pkgs[body.package].expr(e).span;
        let mut push = |code: &'static str, message: &str| {
            out.push(Finding {
                def: body.owner,
                file,
                span,
                code,
                message: message.to_string(),
            });
        };
        match &info.callee {
            // A call through a closure follows from the closure, which is reported where
            // it is written.
            Callee::Value(crate::typed::ValueCall::Closure(_)) => {}
            Callee::Value(_) => push(
                "E1511",
                "a kernel cannot call through a function value or closure",
            ),
            Callee::Variant(v) => {
                if v.payload.is_some() {
                    let name = self
                        .ty_of(body, e)
                        .map(|t| self.ty_name(t))
                        .unwrap_or_default();
                    push(
                        "E1511",
                        &format!(
                            "this variant makes `{name}`, which has a payload; a kernel value is \
                             an element or uniform type"
                        ),
                    );
                }
            }
            Callee::Fn(f) => {
                if matches!(
                    f.source,
                    FnSource::Interface {
                        via: crate::typed::InterfaceVia::Bound | crate::typed::InterfaceVia::Dyn,
                        ..
                    }
                ) {
                    push(
                        "E1511",
                        "a kernel cannot call through an interface bound or `&dyn`",
                    );
                    return;
                }
                self.ensure_sig(f.def);
                let Some(sig) = self.d.fn_sig(f.def).cloned() else {
                    return;
                };
                if self.is_kernel_sig(&sig) {
                    // The direct call is E1515 at the call.
                    return;
                }
                if !self.is_helper_sig(&sig) {
                    let name = sig.name.clone();
                    push(
                        "E1511",
                        &format!(
                            "this calls `{name}`, which is not a kernel helper; mark it \
                             `kernel fn` or inline it"
                        ),
                    );
                    return;
                }
                if sig.mods.unsafe_
                    || sig.abi.is_some()
                    || self.sema.def(f.def).kind == DefKind::ExternFn
                {
                    let name = sig.name.clone();
                    push(
                        "E1511",
                        &format!("`{name}` is `unsafe` or `extern`; a kernel cannot call it"),
                    );
                    return;
                }
                if sig.error.is_some() {
                    let name = sig.name.clone();
                    push(
                        "E1511",
                        &format!("`{name}` is fallible; a kernel cannot call it"),
                    );
                    return;
                }
                if info.receiver.is_some_and(|r| r.mode != ReceiverMode::Value) {
                    push(
                        "E1511",
                        "a kernel cannot call a method whose receiver is a borrow",
                    );
                }
            }
        }
    }

    /// The locals whose value is made by a construct that is reported where it is
    /// written: a borrow, a slice, a literal of an aggregate, a closure, a payload enum,
    /// an error handler, or a call with a non-uniform result.
    fn explained_locals(&mut self, pkg: usize, body: &TypedBody) -> BTreeSet<LocalId> {
        let mut out = BTreeSet::new();
        if let crate::typed::BodyRoot::Block(root) = body.root {
            self.collect_explained(pkg, body, root, &mut out);
        }
        out
    }

    fn collect_explained(
        &mut self,
        pkg: usize,
        body: &TypedBody,
        block: hir::BlockId,
        out: &mut BTreeSet<LocalId>,
    ) {
        let stmts: Vec<hir::StmtId> = self.pkgs[pkg].block(block).stmts.clone();
        for s in stmts {
            self.collect_explained_stmt(pkg, body, s, out);
        }
    }

    fn collect_explained_stmt(
        &mut self,
        pkg: usize,
        body: &TypedBody,
        s: hir::StmtId,
        out: &mut BTreeSet<LocalId>,
    ) {
        let stmt = self.pkgs[pkg].stmt(s).clone();
        match &stmt.kind {
            StmtKind::Let { pattern, init, .. } => {
                if self.construct_reported(body, *init) {
                    self.pattern_locals(pkg, *pattern, out);
                }
            }
            StmtKind::Defer { body: b, .. }
            | StmtKind::Scope { body: b, .. }
            | StmtKind::Block(b)
            | StmtKind::Unsafe { body: b, .. } => {
                self.collect_explained(pkg, body, *b, out);
            }
            StmtKind::If {
                then, otherwise, ..
            } => {
                self.collect_explained(pkg, body, *then, out);
                if let Some(o) = otherwise {
                    self.collect_explained_stmt(pkg, body, *o, out);
                }
            }
            StmtKind::While { body: b, .. } | StmtKind::Loop { body: b, .. } => {
                self.collect_explained(pkg, body, *b, out);
            }
            StmtKind::For { body: b, .. } => {
                self.collect_explained(pkg, body, *b, out);
            }
            StmtKind::Match { scrutinee, arms } => {
                if self.payload_enum(*scrutinee, body) {
                    for arm in arms {
                        for p in &arm.patterns {
                            self.pattern_locals(pkg, *p, out);
                        }
                    }
                }
                for arm in arms {
                    if let hir::ArmBody::Block(b) = &arm.body {
                        self.collect_explained(pkg, body, *b, out);
                    }
                }
            }
            StmtKind::Select { arms } => {
                for arm in arms {
                    self.collect_explained(pkg, body, arm.body, out);
                }
            }
            _ => {}
        }
    }

    fn pattern_locals(&self, pkg: usize, p: hir::PatId, out: &mut BTreeSet<LocalId>) {
        match &self.pkgs[pkg].pat(p).kind {
            hir::PatKind::Bind { local } => {
                out.insert(*local);
            }
            hir::PatKind::Variant {
                payload: Some(p), ..
            } => {
                self.pattern_locals(pkg, *p, out);
            }
            hir::PatKind::Tuple(ps) => {
                for p in ps {
                    self.pattern_locals(pkg, *p, out);
                }
            }
            hir::PatKind::Struct { fields, .. } => {
                for f in fields {
                    self.pattern_locals(pkg, f.pattern, out);
                }
            }
            _ => {}
        }
    }

    /// Whether the construct written at `e` is reported by its own rule.
    fn construct_reported(&mut self, body: &TypedBody, e: ExprId) -> bool {
        let expr = self.pkgs[body.package].expr(e).clone();
        match &expr.kind {
            ExprKind::Paren(inner) | ExprKind::TypeApply { base: inner, .. } => {
                self.construct_reported(body, *inner)
            }
            ExprKind::Unary {
                op: ingot_syntax::ast::UnOp::Borrow | ingot_syntax::ast::UnOp::BorrowMut,
                ..
            }
            | ExprKind::Closure(_)
            | ExprKind::Catch { .. }
            | ExprKind::Orelse { .. }
            | ExprKind::Slice { .. }
            | ExprKind::ArrayLit { .. }
            | ExprKind::Tuple(_)
            | ExprKind::Str(_)
            | ExprKind::CStr(_)
            | ExprKind::Unsafe { .. }
            | ExprKind::Spawn { .. }
            | ExprKind::Dispatch { .. }
            | ExprKind::Call { .. } => true,
            ExprKind::StructLit { .. } | ExprKind::StructShorthand { .. } => self
                .ty_of(body, e)
                .is_some_and(|t| !self.kernel_uniform(t, 0) && !self.kernel_payload_free_enum(t)),
            ExprKind::EnumLit { .. } => self
                .ty_of(body, e)
                .is_some_and(|t| !self.kernel_payload_free_enum(t)),
            _ => false,
        }
    }

    fn payload_enum(&self, scrutinee: ExprId, body: &TypedBody) -> bool {
        match self.ty_of(body, scrutinee) {
            Some(t) => match self.d.types.kind(t) {
                TyKind::Optional(_) | TyKind::Result(..) => true,
                TyKind::Adt(d, _) => {
                    matches!(self.d.items.get(d), Some(ItemSig::Enum(_)))
                        && !self.d.is_payload_free_enum(*d)
                }
                _ => false,
            },
            None => false,
        }
    }

    fn ty_of(&self, body: &TypedBody, e: ExprId) -> Option<TypeId> {
        body.exprs.get(&e).map(|i| i.ty)
    }

    fn local_of(&self, pkg: usize, e: ExprId) -> Option<LocalId> {
        let mut e = e;
        while let ExprKind::Paren(inner) = &self.pkgs[pkg].expr(e).kind {
            e = *inner;
        }
        match &self.pkgs[pkg].expr(e).kind {
            ExprKind::Path(p) => match p.res {
                hir::Res::Local(l) => Some(l),
                _ => None,
            },
            _ => None,
        }
    }

    fn constness(&mut self, pkg: usize, file: usize, e: ExprId) -> Constness {
        if !crate::consteval::runtime_reads(&self.pkgs[pkg], self.sema, e).is_empty() {
            return Constness::Runtime;
        }
        match self.eval_expr(pkg, file, e, None) {
            Ok(_) => Constness::Constant,
            Err(crate::consteval::Fail::Generic) => Constness::Generic,
            Err(crate::consteval::Fail::Reported) => Constness::AlreadyReported,
            Err(crate::consteval::Fail::OutOfRange(_)) => Constness::AlreadyReported,
            Err(crate::consteval::Fail::NotImplemented(m)) => {
                self.ni(format!("{m} (S5b)"));
                Constness::Unimplemented
            }
        }
    }
}

fn finding(
    def: crate::DefId,
    file: usize,
    span: Span,
    code: &'static str,
    message: &str,
) -> Finding {
    Finding {
        def,
        file,
        span,
        code,
        message: message.to_string(),
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Constness {
    Constant,
    Generic,
    Runtime,
    Unimplemented,
    AlreadyReported,
}
