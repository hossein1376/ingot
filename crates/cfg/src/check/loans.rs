//! Analysis C: loans (checker.md §5).
//!
//! Use-liveness of locals runs backward. A loan is live where it sits in some
//! `O(v)` and `v` is use-live, or a live loan is rooted at `v` (a borrow of a
//! container keeps what the container borrows). A two-phase exclusive loan is
//! shared until the call, `spawn`, `dispatch`, `select` or `par for` that uses
//! its temporary, and exclusive from that terminator on.
//!
//! Access checks use [`Origins`] and the same transfer as [`super::origins::sets_before`].
//! A storage end whose live holder is a scope is E1403, not E1105.

use crate::check::origins::{self, Loan, LoanStep, Origin, OriginMap, Origins};
use crate::model::*;
use ingot_diag::diagnostic::Diagnostic;
use ingot_diag::source::{Source, Sources, Span};
use ingot_sema::defs::{DefId, Sema};
use ingot_sema::sigs::{Declarations, ItemSig};
use ingot_sema::ty::{Mutability, TyKind, TypeId};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

pub(super) struct Output {
    pub diagnostics: Vec<Diagnostic>,
    pub not_implemented: Vec<String>,
}

pub(super) fn check_cfg(
    sema: &Sema,
    decls: &mut Declarations,
    sources: &Sources,
    cfg: &Cfg,
    origins: &Origins,
) -> Output {
    let Some(source) = sources.files.get(cfg.file) else {
        return Output {
            diagnostics: Vec::new(),
            not_implemented: Vec::new(),
        };
    };
    if origins.diverged || cfg.blocks.is_empty() {
        return Output {
            diagnostics: Vec::new(),
            not_implemented: Vec::new(),
        };
    }
    let Some(points) = use_liveness(cfg) else {
        return Output {
            diagnostics: Vec::new(),
            not_implemented: vec!["loan liveness did not converge".to_string()],
        };
    };
    let act = activations(cfg, &origins.loans);
    let by_root = loans_by_root(&origins.loans, cfg.locals.len());
    let n = cfg.blocks.len();
    // One replay per block. `sets_before` of statement 0 is the entry, so it
    // does not replay again. The call stays in release builds.
    let mut flows = Vec::with_capacity(n);
    for b in 0..n {
        if origins.entry.get(b).is_none_or(|s| s.is_none()) {
            flows.push(Vec::new());
            continue;
        }
        let before = origins::sets_before(origins, sema, decls, cfg, b, 0);
        let flow = origins::flow_points(origins, sema, decls, cfg, b);
        debug_assert_eq!(before, flow.first().cloned().unwrap_or_default());
        let _ = before;
        flows.push(flow);
    }
    let mut types = Types { cfg, decls };
    let mut cx = Cx {
        cfg,
        origins,
        source,
        act: &act,
        by_root: &by_root,
        diags: Vec::new(),
        seen: BTreeSet::new(),
    };
    let empty_map = OriginMap::new();
    let empty_live = Live::default();
    for b in 0..n {
        let Some(live) = points.get(b).and_then(|p| p.as_ref()) else {
            continue;
        };
        if flows.get(b).is_none_or(|f| f.is_empty()) {
            continue;
        }
        let nstmt = cfg.blocks.get(b).map(|d| d.stmts.len()).unwrap_or(0);
        for i in 0..=nstmt {
            let map = flows[b].get(i).unwrap_or(&empty_map);
            let at = live.get(i).unwrap_or(&empty_live);
            cx.check_point(&mut types, b, i, nstmt, map, at);
        }
    }
    Output {
        diagnostics: cx.diags,
        not_implemented: Vec::new(),
    }
}

#[derive(Clone, PartialEq, Eq, Default)]
struct Live {
    locals: BTreeSet<Local>,
    spans: BTreeMap<Local, Span>,
}

struct Cx<'a> {
    cfg: &'a Cfg,
    origins: &'a Origins,
    source: &'a Source,
    act: &'a BTreeMap<u32, (usize, BTreeSet<usize>)>,
    /// Loan ids rooted at each local, in id order.
    by_root: &'a [Vec<u32>],
    diags: Vec<Diagnostic>,
    seen: BTreeSet<(u16, usize, usize, u32, usize)>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Copy,
    Shared,
    Excl,
    Assign,
    Move,
    End,
}

struct Access {
    kind: Kind,
    local: Local,
    steps: Vec<LoanStep>,
    name: String,
    span: Span,
    killed: BTreeSet<u32>,
    except: Option<u32>,
}

impl<'a> Cx<'a> {
    fn check_point(
        &mut self,
        types: &mut Types<'_>,
        block: usize,
        index: usize,
        nstmt: usize,
        map: &OriginMap,
        live: &Live,
    ) {
        let loans = self.live_loans(map, live);
        let accesses = self.accesses_at(types, block, index, nstmt);
        for access in accesses {
            for (id, span) in &loans {
                let Some(loan) = self.origins.loans.get(*id as usize) else {
                    continue;
                };
                if access.except == Some(*id) {
                    continue;
                }
                if self.activated_later(&access, loan, block) {
                    continue;
                }
                if !self.hit(&access, loan) {
                    continue;
                }
                self.report(&access, loan, *span, block, index, map, live);
            }
        }
    }

    /// Two-phase loans that activate at the same terminator (`dispatch` of one
    /// slice twice) are checked in creation order: an activation conflicts with
    /// the loans created before it, so a pair is reported once, at the later one.
    fn activated_later(&self, access: &Access, loan: &Loan, block: usize) -> bool {
        let Some(own) = access.except else {
            return false;
        };
        let Some(own_loan) = self.origins.loans.get(own as usize) else {
            return false;
        };
        let here = |id: u32| self.act.get(&id).is_some_and(|(b, _)| *b == block);
        here(own) && here(loan.id) && (loan.block, loan.stmt) > (own_loan.block, own_loan.stmt)
    }

    fn hit(&self, access: &Access, loan: &Loan) -> bool {
        if access.kind == Kind::End {
            return loan.on_local && loan.local == access.local;
        }
        loan.local == access.local && overlaps(&loan.proj, &access.steps)
    }

    /// `local` is the capture record of a `par for` body. Item parameters are later.
    fn par_for_capture(&self, local: Local) -> bool {
        if !matches!(self.cfg.key.body, BodyKey::ParFor(_)) {
            return false;
        }
        self.cfg.params.first().copied() == Some(local)
    }

    #[allow(clippy::too_many_arguments)]
    fn report(
        &mut self,
        access: &Access,
        loan: &Loan,
        later: Span,
        block: usize,
        index: usize,
        map: &OriginMap,
        live: &Live,
    ) {
        if access.kind == Kind::End && held_by_scope(self.cfg, loan.id, map, live) {
            self.report_1403(access, loan);
            return;
        }
        // Assigning, moving or exclusively borrowing a `par for` capture is
        // E1407 (`check::tasks`), not E1101, E1102 or E1104.
        if self.par_for_capture(access.local)
            && matches!(access.kind, Kind::Assign | Kind::Excl | Kind::Move)
        {
            return;
        }
        let code = match access.kind {
            Kind::Copy | Kind::Shared => {
                if !self.exclusive_now(loan, block, index) {
                    return;
                }
                "E1103"
            }
            Kind::Excl => "E1102",
            Kind::Assign => {
                if access.killed.contains(&loan.id) {
                    return;
                }
                "E1101"
            }
            Kind::Move => "E1104",
            Kind::End => {
                // The return or `fail` that consumes this loan is E1203.
                // pipeline.md §3: do not also report the storage end.
                if self.only_return(block, index, map, live, loan.id) {
                    return;
                }
                "E1105"
            }
        };
        let primary = if access.kind == Kind::End {
            if loan.span == Span::default() {
                access.span
            } else {
                loan.span
            }
        } else {
            access.span
        };
        let exits = if access.kind == Kind::End {
            crate::check::moves::exit_spans(self.cfg, block, index)
        } else {
            Vec::new()
        };
        let key = (
            code_id(code),
            primary.start,
            primary.end,
            loan.id,
            exit_key(&exits),
        );
        if !self.seen.insert(key) {
            return;
        }
        let message = message(code, &access.name);
        let mut diag = Diagnostic::new(code, message, self.source, primary);
        if access.kind == Kind::End {
            for span in &exits {
                diag.relate("scope_end", *span);
            }
            diag.relate("later_use", later);
            diag.relate("borrow", loan.span);
        } else {
            diag.relate("borrow", loan.span);
            diag.relate("later_use", later);
        }
        self.diags.push(diag);
    }

    /// The loan is on a local of the scope block and a live scope holds it
    /// (checker.md §7). Primary is the borrow. `spawn` is the terminator that
    /// stored the loan into the scope. `declared` is the local.
    fn report_1403(&mut self, access: &Access, loan: &Loan) {
        let primary = if loan.span == Span::default() {
            access.span
        } else {
            loan.span
        };
        let key = (
            code_id("E1403"),
            primary.start,
            primary.end,
            loan.id,
            0usize,
        );
        if !self.seen.insert(key) {
            return;
        }
        let mut diag = Diagnostic::new(
            "E1403",
            format!(
                "`{}` is borrowed by a task past the end of its scope",
                access.name
            ),
            self.source,
            primary,
        );
        if let Some(span) = self.origins.captured.get(&loan.id).copied() {
            diag.relate("spawn", span);
        }
        let declared = self
            .cfg
            .locals
            .get(loan.local as usize)
            .map(|l| l.decl_span)
            .unwrap_or(loan.span);
        diag.relate("declared", declared);
        self.diags.push(diag);
    }

    /// True when every holder that keeps `loan_id` live at this storage end is
    /// kept live only by the `return` or `fail` terminator of this block.
    fn only_return(
        &self,
        block: usize,
        index: usize,
        map: &OriginMap,
        live: &Live,
        loan_id: u32,
    ) -> bool {
        let Some(data) = self.cfg.blocks.get(block) else {
            return false;
        };
        let Terminator::Return { value, .. } = &data.term else {
            return false;
        };
        let ret = match value {
            Some(op) => operand_locals(op),
            None => BTreeSet::new(),
        };
        let mut later = BTreeSet::new();
        for stmt in data.stmts.get(index.saturating_add(1)..).unwrap_or(&[]) {
            for (local, _) in stmt_effect(stmt).uses {
                later.insert(local);
            }
        }
        let mut reduced = Live {
            locals: live.locals.clone(),
            spans: live.spans.clone(),
        };
        for local in &ret {
            if !later.contains(local) {
                reduced.locals.remove(local);
                reduced.spans.remove(local);
            }
        }
        !self
            .live_loans(map, &reduced)
            .iter()
            .any(|(id, _)| *id == loan_id)
    }

    fn exclusive_now(&self, loan: &Loan, block: usize, index: usize) -> bool {
        if loan.kind != Mutability::Mut {
            return false;
        }
        if !loan.two_phase {
            return true;
        }
        let Some((act, after)) = self.act.get(&loan.id) else {
            return true;
        };
        if after.contains(&block) {
            return true;
        }
        let nstmt = self
            .cfg
            .blocks
            .get(block)
            .map(|d| d.stmts.len())
            .unwrap_or(0);
        *act == block && index == nstmt
    }

    /// One walk from the use-live locals. Each local is expanded once; each live
    /// loan then queues its root, whose set is expanded the same way. `by_root`
    /// is id order, so a root reached only through loans takes the span of the
    /// lowest id among those already live.
    fn live_loans(&self, map: &OriginMap, live: &Live) -> Vec<(u32, Span)> {
        let nloans = self.origins.loans.len();
        let nloc = self.by_root.len();
        let mut span_of: Vec<Option<Span>> = vec![None; nloans];
        let mut seen = vec![false; nloc];
        let mut queue = VecDeque::new();
        for &local in map.keys() {
            if !live.locals.contains(&local) {
                continue;
            }
            let idx = local as usize;
            if idx >= nloc || seen[idx] {
                continue;
            }
            seen[idx] = true;
            // The use that keeps a scope live may be a copy of its name. The
            // holder is the scope, so `later_use` is the `scope_wait` (§5.2).
            let span = scope_wait_span(self.cfg, local)
                .or_else(|| live.spans.get(&local).copied())
                .unwrap_or_default();
            queue.push_back((local, span, true));
        }
        while let Some((local, span, direct)) = queue.pop_front() {
            let via = if direct {
                None
            } else {
                self.via_span(local, &span_of)
            };
            let span = via.unwrap_or(span);
            let Some(set) = map.get(&local) else {
                continue;
            };
            for origin in set {
                let Origin::Loan(id) = *origin else {
                    continue;
                };
                let iu = id as usize;
                if iu >= nloans || span_of[iu].is_some() {
                    continue;
                }
                span_of[iu] = Some(span);
                let Some(loan) = self.origins.loans.get(iu) else {
                    continue;
                };
                let root = loan.local as usize;
                if root >= nloc || seen[root] || !map.contains_key(&loan.local) {
                    continue;
                }
                seen[root] = true;
                queue.push_back((loan.local, span, false));
            }
        }
        let mut out = Vec::new();
        for (id, span) in span_of.into_iter().enumerate() {
            if let Some(span) = span {
                out.push((id as u32, span));
            }
        }
        out
    }

    /// Span of the lowest-id live loan rooted at `local`.
    fn via_span(&self, local: Local, span_of: &[Option<Span>]) -> Option<Span> {
        let ids = self.by_root.get(local as usize)?;
        for id in ids {
            if let Some(span) = span_of.get(*id as usize).and_then(|s| *s) {
                return Some(span);
            }
        }
        None
    }

    fn accesses_at(
        &self,
        types: &mut Types<'_>,
        block: usize,
        index: usize,
        nstmt: usize,
    ) -> Vec<Access> {
        let Some(data) = self.cfg.blocks.get(block) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        if index < nstmt {
            if let Some(stmt) = data.stmts.get(index) {
                self.stmt_accesses(types, block, index, stmt, &mut out);
            }
        } else {
            self.term_accesses(types, &data.term, data.term_span, &mut out);
            self.activate(types, block, &mut out);
        }
        out
    }

    fn activate(&self, types: &mut Types<'_>, block: usize, out: &mut Vec<Access>) {
        for loan in &self.origins.loans {
            if !loan.two_phase || loan.kind != Mutability::Mut {
                continue;
            }
            let Some((act, _)) = self.act.get(&loan.id) else {
                continue;
            };
            if *act != block {
                continue;
            }
            out.push(Access {
                kind: Kind::Excl,
                local: loan.local,
                steps: loan.proj.clone(),
                name: types.loan_name(loan),
                span: loan.span,
                killed: BTreeSet::new(),
                except: Some(loan.id),
            });
        }
    }

    fn stmt_accesses(
        &self,
        types: &mut Types<'_>,
        block: usize,
        index: usize,
        stmt: &Stmt,
        out: &mut Vec<Access>,
    ) {
        let span = stmt.span;
        match &stmt.kind {
            StmtKind::Assign { place, value } => {
                // `π op= e` reads and writes `π` as one access, checked as the
                // store (§5.2). The read is the copy into a temporary.
                if !self.compound_read(block, index, stmt) {
                    self.rvalue_accesses(types, block, index, value, span, out);
                }
                self.place_access(types, place, Kind::Assign, span, true, out);
            }
            StmtKind::Use(op) => self.operand_access(types, op, span, out),
            StmtKind::StorageEnd(l) => out.push(self.end_access(*l, span)),
            StmtKind::Cancel(l) => {
                out.push(self.local_access(*l, Kind::Copy, span));
            }
            StmtKind::Check(c) => {
                // A check reads for liveness. The guarded operation is the access
                // the table names, and it is the next statement.
                let _ = c;
            }
            StmtKind::IterConsumed(place) => {
                self.place_access(types, place, Kind::Move, span, false, out);
            }
            StmtKind::StorageStart(_)
            | StmtKind::EnterUnsafe
            | StmtKind::Abandon(_)
            | StmtKind::Variant { .. } => {}
        }
    }

    fn term_accesses(
        &self,
        types: &mut Types<'_>,
        term: &Terminator,
        span: Span,
        out: &mut Vec<Access>,
    ) {
        match term {
            Terminator::Goto(_) | Terminator::Unreachable => {}
            Terminator::Branch { cond, .. } => self.operand_access(types, cond, span, out),
            Terminator::Switch { value, .. } => self.operand_access(types, value, span, out),
            Terminator::Return { value, end, .. } => {
                if let Some(v) = value {
                    self.operand_access(types, v, span, out);
                }
                if let Some(l) = end {
                    out.push(self.end_access(*l, span));
                }
            }
            Terminator::Call { target, args, .. } => {
                self.target_access(types, target, span, out);
                for a in args {
                    self.operand_access(types, a, span, out);
                }
            }
            Terminator::Trap { operands, .. } => {
                for o in operands {
                    self.operand_access(types, o, span, out);
                }
            }
            Terminator::Spawn {
                target,
                args,
                scope,
                ..
            } => {
                self.target_access(types, target, span, out);
                for a in args {
                    self.operand_access(types, a, span, out);
                }
                self.operand_access(types, scope, span, out);
            }
            Terminator::ScopeWait { scope, .. } => {
                out.push(self.local_access(*scope, Kind::Copy, span));
            }
            Terminator::Select { ops } => {
                for op in ops {
                    let Some(call) = &op.op else {
                        continue;
                    };
                    self.place_access(types, &call.value, Kind::Move, op.span, false, out);
                }
            }
            Terminator::ParFor {
                captures,
                capture_places,
                iterand,
                grain,
                ..
            } => {
                for c in captures {
                    self.operand_access(types, c, span, out);
                }
                for p in capture_places {
                    self.place_access(types, p, Kind::Shared, span, false, out);
                }
                match iterand {
                    ParIterand::Value(op) => self.operand_access(types, op, span, out),
                    ParIterand::Range { lo, hi, step, .. } => {
                        self.operand_access(types, lo, span, out);
                        self.operand_access(types, hi, span, out);
                        if let Some(s) = step {
                            self.operand_access(types, s, span, out);
                        }
                    }
                }
                if let Some(g) = grain {
                    self.operand_access(types, g, span, out);
                }
            }
            Terminator::Dispatch {
                args,
                executor,
                lo,
                hi,
                ..
            } => {
                self.operand_access(types, executor, span, out);
                self.operand_access(types, lo, span, out);
                self.operand_access(types, hi, span, out);
                for a in args {
                    self.operand_access(types, &a.value, span, out);
                    // An exclusive slice argument is the two-phase reborrow
                    // materialized just above. `activate` checks that borrow
                    // here and excludes its own loan. A second exclusive access
                    // would conflict with that loan.
                    if let (Some(Mutability::Shared), Some(p)) = (a.borrow, a.place.as_ref()) {
                        self.place_access(types, p, Kind::Shared, span, false, out);
                    }
                }
            }
        }
    }

    fn target_access(
        &self,
        types: &mut Types<'_>,
        target: &CallTarget,
        span: Span,
        out: &mut Vec<Access>,
    ) {
        if let CallTarget::Value { callee, .. } = target {
            self.operand_access(types, callee, span, out);
        }
    }

    fn rvalue_accesses(
        &self,
        types: &mut Types<'_>,
        block: usize,
        index: usize,
        value: &Rvalue,
        span: Span,
        out: &mut Vec<Access>,
    ) {
        match value {
            Rvalue::Use(op) => self.operand_access(types, op, span, out),
            Rvalue::Unary { operand, .. }
            | Rvalue::Cast { operand, .. }
            | Rvalue::DynCast { operand, .. } => {
                self.operand_access(types, operand, span, out);
            }
            Rvalue::Binary { lhs, rhs, .. } => {
                self.operand_access(types, lhs, span, out);
                self.operand_access(types, rhs, span, out);
            }
            Rvalue::Borrow {
                place, mutability, ..
            } => {
                let kind = if *mutability == Mutability::Mut {
                    Kind::Excl
                } else {
                    Kind::Shared
                };
                self.place_access(types, place, kind, span, false, out);
            }
            Rvalue::Reborrow { place, .. } => {
                let mut p = place.clone();
                p.proj.push(Projection::Deref);
                self.place_access(types, &p, Kind::Excl, span, false, out);
            }
            Rvalue::Aggregate { fields, .. } => {
                for f in fields {
                    self.operand_access(types, f, span, out);
                }
            }
            Rvalue::Discriminant(p) | Rvalue::SliceLen(p) | Rvalue::SlicePtr(p) => {
                self.place_access(types, p, Kind::Copy, span, false, out);
            }
            Rvalue::Slice {
                base,
                lo,
                hi,
                borrow,
                ..
            } => {
                self.operand_access(types, lo, span, out);
                self.operand_access(types, hi, span, out);
                let kind = match borrow {
                    Some(Mutability::Mut) => Kind::Excl,
                    Some(Mutability::Shared) => Kind::Shared,
                    None => {
                        if types.slice_is_mut(base) {
                            Kind::Excl
                        } else {
                            Kind::Shared
                        }
                    }
                };
                if borrow.is_none() {
                    let mut p = base.clone();
                    p.proj.push(Projection::Deref);
                    self.place_access(types, &p, kind, span, false, out);
                } else {
                    self.place_access(types, base, kind, span, false, out);
                }
            }
            Rvalue::SliceElem { base, index } => {
                self.operand_access(types, index, span, out);
                self.place_access(types, base, Kind::Copy, span, false, out);
            }
            Rvalue::Builtin { name, args, .. } => {
                self.builtin_accesses(types, block, index, name, args, span, out);
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn builtin_accesses(
        &self,
        types: &mut Types<'_>,
        block: usize,
        index: usize,
        name: &str,
        args: &[BuiltinArg],
        span: Span,
        out: &mut Vec<Access>,
    ) {
        if name == "forget" {
            if let Some(BuiltinArg::Value(op)) = args.first() {
                self.forget_access(types, block, index, op, span, out);
            }
            return;
        }
        if name == "replace" {
            if let Some(BuiltinArg::Place(p)) = args.first() {
                self.place_access(types, p, Kind::Move, span, false, out);
                self.place_access(types, p, Kind::Assign, span, true, out);
            }
            if let Some(BuiltinArg::Value(op)) = args.get(1) {
                self.operand_access(types, op, span, out);
            }
            return;
        }
        if name == "swap" {
            for a in args.iter().take(2) {
                if let BuiltinArg::Place(p) = a {
                    self.place_access(types, p, Kind::Move, span, false, out);
                    self.place_access(types, p, Kind::Assign, span, true, out);
                }
            }
            return;
        }
        let store = name.contains("store") || name.contains("scatter");
        for a in args {
            match a {
                BuiltinArg::Value(op) => self.operand_access(types, op, span, out),
                BuiltinArg::Place(p) if store => {
                    self.place_access(types, p, Kind::Assign, span, true, out);
                }
                BuiltinArg::Place(p) => self.place_access(types, p, Kind::Copy, span, false, out),
            }
        }
    }

    /// The statement copies `π` into a temporary, a later binary at the same span
    /// uses that temporary, and a later store at that span writes `π` back.
    /// That is the read half of a compound assignment.
    fn compound_read(&self, block: usize, index: usize, stmt: &Stmt) -> bool {
        let StmtKind::Assign { place: dest, value } = &stmt.kind else {
            return false;
        };
        let Some(temp) = whole_local(dest) else {
            return false;
        };
        let Rvalue::Use(Operand::Place(src, _)) = value else {
            return false;
        };
        let Some(stmts) = self.cfg.blocks.get(block).map(|b| &b.stmts) else {
            return false;
        };
        let mut saw_bin = false;
        for later in stmts.iter().skip(index + 1) {
            if later.span != stmt.span {
                continue;
            }
            let StmtKind::Assign { place, value } = &later.kind else {
                continue;
            };
            match value {
                Rvalue::Binary { lhs, .. } => {
                    if operand_root(lhs) == Some(temp) {
                        saw_bin = true;
                    }
                }
                Rvalue::Use(_) if saw_bin && place_same(place, src) => return true,
                _ => {}
            }
        }
        false
    }

    /// `@forget` of a `Copy` place is snapshotted as a copy. The copy is not a
    /// move, so the move is reported here, on the source place.
    fn forget_access(
        &self,
        types: &mut Types<'_>,
        block: usize,
        index: usize,
        op: &Operand,
        span: Span,
        out: &mut Vec<Access>,
    ) {
        self.operand_access(types, op, span, out);
        let Operand::Place(temp, _) = op else {
            return;
        };
        if !temp.proj.is_empty() {
            self.place_access(types, temp, Kind::Move, span, false, out);
            return;
        }
        let Some(src) = temp.root() else {
            return;
        };
        let Some((mode, place)) = self.copy_source(block, index, src) else {
            return;
        };
        if mode == PlaceUse::Copy {
            self.place_access(types, &place, Kind::Move, span, false, out);
        }
    }

    /// The assignment that put `local` in this block, before `index`. A move was
    /// already checked there. A copy is `@forget` of a `Copy` place.
    fn copy_source(&self, block: usize, index: usize, local: Local) -> Option<(PlaceUse, Place)> {
        let stmts = &self.cfg.blocks.get(block)?.stmts;
        for stmt in stmts.iter().take(index).rev() {
            let StmtKind::Assign { place, value } = &stmt.kind else {
                continue;
            };
            if whole_local(place) != Some(local) {
                continue;
            }
            let Rvalue::Use(Operand::Place(src, mode)) = value else {
                return None;
            };
            return Some((*mode, src.clone()));
        }
        None
    }

    fn operand_access(
        &self,
        types: &mut Types<'_>,
        op: &Operand,
        span: Span,
        out: &mut Vec<Access>,
    ) {
        let Operand::Place(place, mode) = op else {
            return;
        };
        let kind = match mode {
            PlaceUse::Copy => Kind::Copy,
            PlaceUse::Move | PlaceUse::ForElem => Kind::Move,
        };
        self.place_access(types, place, kind, span, false, out);
    }

    fn place_access(
        &self,
        types: &mut Types<'_>,
        place: &Place,
        kind: Kind,
        span: Span,
        assign: bool,
        out: &mut Vec<Access>,
    ) {
        for proj in &place.proj {
            if let Projection::Index(op) = proj {
                self.operand_access(types, op, span, out);
            }
        }
        let Some(local) = place.root() else {
            return;
        };
        let steps = types.steps(place);
        let killed = if assign && kind == Kind::Assign {
            types.killed(place, &self.origins.loans)
        } else {
            BTreeSet::new()
        };
        out.push(Access {
            kind,
            local,
            steps,
            name: place_name(self.cfg, place),
            span,
            killed,
            except: None,
        });
    }

    fn end_access(&self, local: Local, span: Span) -> Access {
        Access {
            kind: Kind::End,
            local,
            steps: Vec::new(),
            name: local_name(self.cfg, local),
            span,
            killed: BTreeSet::new(),
            except: None,
        }
    }

    fn local_access(&self, local: Local, kind: Kind, span: Span) -> Access {
        Access {
            kind,
            local,
            steps: Vec::new(),
            name: local_name(self.cfg, local),
            span,
            killed: BTreeSet::new(),
            except: None,
        }
    }
}

/// Span of the `scope_wait` for a scope local. Other locals have none.
fn scope_wait_span(cfg: &Cfg, local: Local) -> Option<Span> {
    if cfg
        .locals
        .get(local as usize)
        .is_none_or(|l| l.kind != LocalKind::Scope)
    {
        return None;
    }
    for block in &cfg.blocks {
        if let Terminator::ScopeWait { scope, .. } = &block.term
            && *scope == local
        {
            return Some(block.term_span);
        }
    }
    None
}

/// A scope holds `id` and is use-live. The storage end is then E1403.
fn held_by_scope(cfg: &Cfg, id: u32, map: &OriginMap, live: &Live) -> bool {
    let origin = Origin::Loan(id);
    for (local, set) in map {
        if !set.contains(&origin) || !live.locals.contains(local) {
            continue;
        }
        if cfg
            .locals
            .get(*local as usize)
            .is_some_and(|l| l.kind == LocalKind::Scope)
        {
            return true;
        }
    }
    false
}

fn message(code: &str, place: &str) -> String {
    match code {
        "E1101" => format!("`{place}` is assigned while borrowed"),
        "E1102" => format!("`{place}` is borrowed exclusively while borrowed"),
        "E1103" => format!("`{place}` is used while exclusively borrowed"),
        "E1104" => format!("`{place}` is moved while borrowed"),
        _ => format!("`{place}` is borrowed past the end of its storage"),
    }
}

fn code_id(code: &str) -> u16 {
    match code {
        "E1101" => 1101,
        "E1102" => 1102,
        "E1103" => 1103,
        "E1104" => 1104,
        "E1105" => 1105,
        "E1403" => 1403,
        _ => 0,
    }
}

fn overlaps(a: &[LoanStep], b: &[LoanStep]) -> bool {
    a.starts_with(b) || b.starts_with(a)
}

fn whole_local(place: &Place) -> Option<Local> {
    if place.proj.is_empty() {
        place.root()
    } else {
        None
    }
}

fn operand_root(op: &Operand) -> Option<Local> {
    let Operand::Place(place, _) = op else {
        return None;
    };
    place.root()
}

fn place_same(a: &Place, b: &Place) -> bool {
    let base = match (&a.base, &b.base) {
        (PlaceBase::Local(x), PlaceBase::Local(y)) => x == y,
        (PlaceBase::Static(x), PlaceBase::Static(y)) => x == y,
        _ => false,
    };
    base && a.proj.len() == b.proj.len() && a.proj.iter().zip(&b.proj).all(|(p, q)| proj_same(p, q))
}

fn proj_same(a: &Projection, b: &Projection) -> bool {
    match (a, b) {
        (Projection::Field { index: i, .. }, Projection::Field { index: j, .. }) => i == j,
        (Projection::TupleField(i), Projection::TupleField(j)) => i == j,
        (Projection::Index(_), Projection::Index(_)) => true,
        (Projection::Deref, Projection::Deref) | (Projection::DerefRaw, Projection::DerefRaw) => {
            true
        }
        (Projection::Payload { variant: i }, Projection::Payload { variant: j }) => i == j,
        _ => false,
    }
}

fn local_name(cfg: &Cfg, local: Local) -> String {
    cfg.locals
        .get(local as usize)
        .and_then(|l| l.name.clone())
        .unwrap_or_else(|| "temporary".to_string())
}

fn place_name(cfg: &Cfg, place: &Place) -> String {
    let mut name = match &place.base {
        PlaceBase::Local(l) => local_name(cfg, *l),
        PlaceBase::Static(_) => "static".to_string(),
    };
    for proj in &place.proj {
        match proj {
            Projection::Field { name: Some(n), .. } => {
                name.push('.');
                name.push_str(n);
            }
            Projection::Field { index, name: None } => name.push_str(&format!(".{index}")),
            Projection::TupleField(i) => name.push_str(&format!(".{i}")),
            Projection::Index(_) => name.push_str("[_]"),
            Projection::Deref | Projection::DerefRaw => name.push_str(".*"),
            Projection::Payload { .. } => name.push_str(".?"),
        }
    }
    name
}

fn loans_by_root(loans: &[Loan], nlocals: usize) -> Vec<Vec<u32>> {
    let mut by_root = vec![Vec::new(); nlocals];
    for loan in loans {
        let idx = loan.local as usize;
        if idx < nlocals {
            by_root[idx].push(loan.id);
        }
    }
    by_root
}

fn exit_key(spans: &[Span]) -> usize {
    let mut key = spans.len();
    for span in spans {
        key = key
            .wrapping_mul(0x9E37_79B1)
            .wrapping_add(span.start)
            .wrapping_add(span.end.wrapping_mul(2));
    }
    key
}

fn operand_locals(op: &Operand) -> BTreeSet<Local> {
    let mut uses = Vec::new();
    touch_operand(op, Span::default(), &mut uses);
    uses.into_iter().map(|(local, _)| local).collect()
}

fn use_liveness(cfg: &Cfg) -> Option<Vec<Option<Vec<Live>>>> {
    let n = cfg.blocks.len();
    let reach = reachable(cfg);
    let preds = predecessors(cfg, &reach);
    let mut live_in = vec![Live::default(); n];
    let mut queue: VecDeque<usize> = reach.iter().copied().collect();
    let mut queued: BTreeSet<usize> = reach.clone();
    let stmts: usize = cfg
        .blocks
        .iter()
        .map(|b| b.stmts.len().saturating_add(1))
        .sum();
    let cap = n
        .saturating_mul(cfg.locals.len().saturating_add(1))
        .saturating_mul(stmts.saturating_add(1))
        .saturating_add(1);
    let mut steps: usize = 0;
    while let Some(b) = queue.pop_front() {
        queued.remove(&b);
        steps = steps.saturating_add(1);
        if steps > cap {
            return None;
        }
        let out = join_succ(cfg, b, &live_in);
        let inn = block_in(cfg, b, &out);
        if inn != live_in[b] {
            live_in[b] = inn;
            for p in preds.get(&b).into_iter().flatten() {
                if reach.contains(p) && queued.insert(*p) {
                    queue.push_back(*p);
                }
            }
        }
    }
    let mut points = Vec::with_capacity(n);
    for b in 0..n {
        if !reach.contains(&b) {
            points.push(None);
            continue;
        }
        let out = join_succ(cfg, b, &live_in);
        points.push(Some(points_before(cfg, b, &out)));
    }
    Some(points)
}

fn block_in(cfg: &Cfg, block: usize, live_out: &Live) -> Live {
    points_before(cfg, block, live_out)
        .first()
        .cloned()
        .unwrap_or_default()
}

fn points_before(cfg: &Cfg, block: usize, live_out: &Live) -> Vec<Live> {
    let Some(data) = cfg.blocks.get(block) else {
        return vec![live_out.clone()];
    };
    let n = data.stmts.len();
    let mut cur = apply(&term_effect(&data.term, data.term_span), live_out);
    let mut pts = vec![Live::default(); n.saturating_add(1)];
    pts[n] = cur.clone();
    for i in (0..n).rev() {
        cur = apply(&stmt_effect(&data.stmts[i]), &cur);
        pts[i] = cur.clone();
    }
    pts
}

fn apply(effect: &Effect, after: &Live) -> Live {
    let mut locals = after.locals.clone();
    let mut spans = after.spans.clone();
    for k in &effect.kills {
        locals.remove(k);
        spans.remove(k);
    }
    for (local, span) in &effect.uses {
        locals.insert(*local);
        spans.insert(*local, *span);
    }
    Live { locals, spans }
}

struct Effect {
    uses: Vec<(Local, Span)>,
    kills: Vec<Local>,
}

fn stmt_effect(stmt: &Stmt) -> Effect {
    let span = stmt.span;
    let mut uses = Vec::new();
    let mut kills = Vec::new();
    match &stmt.kind {
        StmtKind::Assign { place, value } => {
            if let Some(l) = whole_local(place) {
                kills.push(l);
            } else {
                // `a.* = 1` and `p.a = 1` read the root to form the address. That
                // read keeps a loan held by the root live. A whole assignment does
                // not: it kills the local.
                touch_place(place, span, &mut uses);
            }
            touch_place_indices(place, span, &mut uses);
            touch_rvalue(value, span, &mut uses);
        }
        StmtKind::Use(op) => touch_operand(op, span, &mut uses),
        StmtKind::Cancel(l) => uses.push((*l, span)),
        StmtKind::Check(c) => touch_check(c, span, &mut uses),
        StmtKind::IterConsumed(p) => touch_place(p, span, &mut uses),
        StmtKind::StorageStart(_)
        | StmtKind::StorageEnd(_)
        | StmtKind::EnterUnsafe
        | StmtKind::Abandon(_)
        | StmtKind::Variant { .. } => {}
    }
    Effect { uses, kills }
}

fn term_effect(term: &Terminator, span: Span) -> Effect {
    let mut uses = Vec::new();
    let mut kills = Vec::new();
    match term {
        Terminator::Goto(_) | Terminator::Unreachable => {}
        Terminator::Branch { cond, .. } => touch_operand(cond, span, &mut uses),
        Terminator::Switch { value, .. } => touch_operand(value, span, &mut uses),
        Terminator::Return { value, .. } => {
            if let Some(v) = value {
                touch_operand(v, span, &mut uses);
            }
        }
        Terminator::Call {
            target, args, dest, ..
        } => {
            touch_target(target, span, &mut uses);
            for a in args {
                touch_operand(a, span, &mut uses);
            }
            if let Some(d) = dest.as_ref().and_then(whole_local) {
                kills.push(d);
            }
        }
        Terminator::Trap { operands, .. } => {
            for o in operands {
                touch_operand(o, span, &mut uses);
            }
        }
        Terminator::Spawn {
            target,
            args,
            scope,
            dest,
            ..
        } => {
            touch_target(target, span, &mut uses);
            for a in args {
                touch_operand(a, span, &mut uses);
            }
            touch_operand(scope, span, &mut uses);
            if let Some(d) = dest.as_ref().and_then(whole_local) {
                kills.push(d);
            }
        }
        Terminator::ScopeWait { scope, .. } => uses.push((*scope, span)),
        Terminator::Select { ops } => {
            for op in ops {
                let Some(call) = &op.op else {
                    continue;
                };
                touch_place(&call.value, op.span, &mut uses);
            }
        }
        Terminator::ParFor {
            captures,
            capture_places,
            iterand,
            grain,
            ..
        } => {
            for c in captures {
                touch_operand(c, span, &mut uses);
            }
            for p in capture_places {
                touch_place(p, span, &mut uses);
            }
            match iterand {
                ParIterand::Value(op) => touch_operand(op, span, &mut uses),
                ParIterand::Range { lo, hi, step, .. } => {
                    touch_operand(lo, span, &mut uses);
                    touch_operand(hi, span, &mut uses);
                    if let Some(s) = step {
                        touch_operand(s, span, &mut uses);
                    }
                }
            }
            if let Some(g) = grain {
                touch_operand(g, span, &mut uses);
            }
        }
        Terminator::Dispatch {
            args,
            executor,
            lo,
            hi,
            dest,
            ..
        } => {
            touch_operand(executor, span, &mut uses);
            touch_operand(lo, span, &mut uses);
            touch_operand(hi, span, &mut uses);
            for a in args {
                touch_operand(&a.value, span, &mut uses);
                if let Some(p) = &a.place {
                    touch_place(p, span, &mut uses);
                }
            }
            if let Some(d) = dest.as_ref().and_then(whole_local) {
                kills.push(d);
            }
        }
    }
    Effect { uses, kills }
}

fn touch_target(target: &CallTarget, span: Span, uses: &mut Vec<(Local, Span)>) {
    if let CallTarget::Value { callee, .. } = target {
        touch_operand(callee, span, uses);
    }
}

fn touch_check(check: &Check, span: Span, uses: &mut Vec<(Local, Span)>) {
    if let Some(p) = &check.place {
        touch_place(p, span, uses);
    }
    for p in &check.places {
        touch_place(p, span, uses);
    }
    if let Some(o) = &check.lo {
        touch_operand(o, span, uses);
    }
    if let Some(o) = &check.hi {
        touch_operand(o, span, uses);
    }
    for o in &check.operands {
        touch_operand(o, span, uses);
    }
}

fn touch_rvalue(value: &Rvalue, span: Span, uses: &mut Vec<(Local, Span)>) {
    match value {
        Rvalue::Use(op)
        | Rvalue::Unary { operand: op, .. }
        | Rvalue::Cast { operand: op, .. }
        | Rvalue::DynCast { operand: op, .. } => touch_operand(op, span, uses),
        Rvalue::Binary { lhs, rhs, .. } => {
            touch_operand(lhs, span, uses);
            touch_operand(rhs, span, uses);
        }
        Rvalue::Borrow { place, .. } | Rvalue::Reborrow { place, .. } => {
            touch_place(place, span, uses)
        }
        Rvalue::Aggregate { fields, .. } => {
            for f in fields {
                touch_operand(f, span, uses);
            }
        }
        Rvalue::Discriminant(p) | Rvalue::SliceLen(p) | Rvalue::SlicePtr(p) => {
            touch_place(p, span, uses);
        }
        Rvalue::Slice { base, lo, hi, .. } => {
            touch_place(base, span, uses);
            touch_operand(lo, span, uses);
            touch_operand(hi, span, uses);
        }
        Rvalue::SliceElem { base, index } => {
            touch_place(base, span, uses);
            touch_operand(index, span, uses);
        }
        Rvalue::Builtin { args, .. } => {
            for a in args {
                match a {
                    BuiltinArg::Value(op) => touch_operand(op, span, uses),
                    BuiltinArg::Place(p) => touch_place(p, span, uses),
                }
            }
        }
    }
}

fn touch_operand(op: &Operand, span: Span, uses: &mut Vec<(Local, Span)>) {
    if let Operand::Place(p, _) = op {
        touch_place(p, span, uses);
    }
}

fn touch_place(place: &Place, span: Span, uses: &mut Vec<(Local, Span)>) {
    if let Some(l) = place.root() {
        uses.push((l, span));
    }
    touch_place_indices(place, span, uses);
}

fn touch_place_indices(place: &Place, span: Span, uses: &mut Vec<(Local, Span)>) {
    for proj in &place.proj {
        if let Projection::Index(op) = proj {
            touch_operand(op, span, uses);
        }
    }
}

fn join_succ(cfg: &Cfg, block: usize, live_in: &[Live]) -> Live {
    let mut acc = Live::default();
    let Some(data) = cfg.blocks.get(block) else {
        return acc;
    };
    for s in successors(&data.term) {
        let Some(live) = live_in.get(s as usize) else {
            continue;
        };
        acc.locals.extend(live.locals.iter().copied());
        for (local, span) in &live.spans {
            acc.spans
                .entry(*local)
                .and_modify(|have| {
                    if (span.start, span.end) < (have.start, have.end) {
                        *have = *span;
                    }
                })
                .or_insert(*span);
        }
    }
    acc
}

fn reachable(cfg: &Cfg) -> BTreeSet<usize> {
    let mut out = BTreeSet::new();
    let mut q = VecDeque::new();
    let start = cfg.entry as usize;
    if start >= cfg.blocks.len() {
        return out;
    }
    q.push_back(start);
    out.insert(start);
    while let Some(b) = q.pop_front() {
        let Some(data) = cfg.blocks.get(b) else {
            continue;
        };
        for s in successors(&data.term) {
            let s = s as usize;
            if s < cfg.blocks.len() && out.insert(s) {
                q.push_back(s);
            }
        }
    }
    out
}

fn predecessors(cfg: &Cfg, reach: &BTreeSet<usize>) -> BTreeMap<usize, Vec<usize>> {
    let mut preds: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for b in reach {
        let Some(data) = cfg.blocks.get(*b) else {
            continue;
        };
        for s in successors(&data.term) {
            let s = s as usize;
            if reach.contains(&s) {
                preds.entry(s).or_default().push(*b);
            }
        }
    }
    for v in preds.values_mut() {
        v.sort_unstable();
        v.dedup();
    }
    preds
}

fn successors(term: &Terminator) -> Vec<Block> {
    match term {
        Terminator::Goto(t) => vec![*t],
        Terminator::Branch {
            then_block,
            else_block,
            ..
        } => vec![*then_block, *else_block],
        Terminator::Switch { cases, default, .. } => {
            let mut v: Vec<Block> = cases.iter().map(|(_, b)| *b).collect();
            v.push(*default);
            v
        }
        Terminator::Call { next, .. }
        | Terminator::Spawn { next, .. }
        | Terminator::ScopeWait { next, .. }
        | Terminator::ParFor { next, .. }
        | Terminator::Dispatch { next, .. } => vec![*next],
        Terminator::Select { ops } => ops.iter().map(|o| o.successor).collect(),
        Terminator::Return { .. } | Terminator::Trap { .. } | Terminator::Unreachable => Vec::new(),
    }
}

/// `(activation block, blocks strictly after that terminator)`.
fn activations(cfg: &Cfg, loans: &[Loan]) -> BTreeMap<u32, (usize, BTreeSet<usize>)> {
    let mut out = BTreeMap::new();
    for loan in loans {
        if !loan.two_phase || loan.kind != Mutability::Mut {
            continue;
        }
        let Some(block) = activation_block(cfg, loan) else {
            continue;
        };
        let after = blocks_after(cfg, block);
        out.insert(loan.id, (block, after));
    }
    out
}

fn creation_holder(cfg: &Cfg, loan: &Loan) -> Option<Local> {
    let stmt = cfg
        .blocks
        .get(loan.block as usize)?
        .stmts
        .get(loan.stmt as usize)?;
    let StmtKind::Assign { place, .. } = &stmt.kind else {
        return None;
    };
    whole_local(place)
}

fn activation_block(cfg: &Cfg, loan: &Loan) -> Option<usize> {
    let holder = creation_holder(cfg, loan)?;
    let mut queue = VecDeque::new();
    let mut seen = BTreeSet::new();
    queue.push_back((
        loan.block as usize,
        loan.stmt as usize + 1,
        BTreeSet::from([holder]),
    ));
    let cap = cfg.blocks.len().saturating_mul(8).saturating_add(1);
    let mut steps: usize = 0;
    while let Some((block, from, mut holders)) = queue.pop_front() {
        steps = steps.saturating_add(1);
        if steps > cap {
            return None;
        }
        let key = (block, from, holders.iter().copied().collect::<Vec<_>>());
        if !seen.insert(key) {
            continue;
        }
        let Some(data) = cfg.blocks.get(block) else {
            continue;
        };
        let n = data.stmts.len();
        let start = from.min(n);
        for stmt in data.stmts.iter().skip(start) {
            let StmtKind::Assign { place, value } = &stmt.kind else {
                continue;
            };
            let Some(dest) = whole_local(place) else {
                continue;
            };
            let Rvalue::Use(Operand::Place(src, _)) = value else {
                continue;
            };
            if src.proj.is_empty() && src.root().is_some_and(|r| holders.contains(&r)) {
                holders.insert(dest);
            }
        }
        if term_mentions(&data.term, &holders) {
            return Some(block);
        }
        for s in successors(&data.term) {
            let s = s as usize;
            if s < cfg.blocks.len() {
                queue.push_back((s, 0, holders.clone()));
            }
        }
    }
    None
}

fn term_mentions(term: &Terminator, holders: &BTreeSet<Local>) -> bool {
    let mut uses = Vec::new();
    term_effect(term, Span::default())
        .uses
        .iter()
        .for_each(|(l, _)| {
            uses.push(*l);
        });
    // A kill of the holder (the call destination) is not the activating use.
    uses.iter().any(|l| holders.contains(l))
}

fn blocks_after(cfg: &Cfg, block: usize) -> BTreeSet<usize> {
    let mut out = BTreeSet::new();
    let mut q = VecDeque::new();
    if let Some(data) = cfg.blocks.get(block) {
        for s in successors(&data.term) {
            q.push_back(s as usize);
        }
    }
    while let Some(b) = q.pop_front() {
        if b >= cfg.blocks.len() || !out.insert(b) {
            continue;
        }
        let Some(data) = cfg.blocks.get(b) else {
            continue;
        };
        for s in successors(&data.term) {
            q.push_back(s as usize);
        }
    }
    out
}

struct Types<'a> {
    cfg: &'a Cfg,
    decls: &'a mut Declarations,
}

impl<'a> Types<'a> {
    fn steps(&mut self, place: &Place) -> Vec<LoanStep> {
        let mut steps = Vec::new();
        let mut ty = match &place.base {
            PlaceBase::Local(l) => self.local_ty(*l),
            PlaceBase::Static(d) => self.static_ty(*d),
        };
        for proj in &place.proj {
            let step = match ty {
                Some(t) => origins::loan_step(&self.kind(t), proj),
                None => origins::loan_step(&self.kind(self.decls.types.poison), proj),
            };
            steps.push(step);
            ty = ty.and_then(|t| self.proj_ty(t, proj));
        }
        steps
    }

    fn killed(&mut self, place: &Place, loans: &[Loan]) -> BTreeSet<u32> {
        let Some(ty) = self.place_ty(place) else {
            return BTreeSet::new();
        };
        if !is_borrow_ty(&self.kind(ty)) {
            return BTreeSet::new();
        }
        let steps = self.steps(place);
        if steps
            .iter()
            .any(|s| matches!(s, LoanStep::Deref | LoanStep::Raw))
        {
            return BTreeSet::new();
        }
        let Some(root) = place.root() else {
            return BTreeSet::new();
        };
        let mut ids = BTreeSet::new();
        for loan in loans {
            if loan.local != root || loan.proj.len() <= steps.len() {
                continue;
            }
            if loan.proj.get(..steps.len()) != Some(steps.as_slice()) {
                continue;
            }
            if loan.proj.get(steps.len()) == Some(&LoanStep::Deref) {
                ids.insert(loan.id);
            }
        }
        ids
    }

    fn slice_is_mut(&mut self, place: &Place) -> bool {
        let Some(ty) = self.place_ty(place) else {
            return false;
        };
        matches!(
            self.kind(ty),
            TyKind::Slice(Mutability::Mut, _) | TyKind::Ref(Mutability::Mut, _)
        )
    }

    fn loan_name(&mut self, loan: &Loan) -> String {
        let mut name = local_name(self.cfg, loan.local);
        let mut ty = self.local_ty(loan.local);
        for step in &loan.proj {
            match step {
                LoanStep::Field(i) => {
                    let label = ty
                        .and_then(|t| self.field_label(t, *i as usize))
                        .unwrap_or_else(|| i.to_string());
                    name.push('.');
                    name.push_str(&label);
                    ty = ty.and_then(|t| self.field_ty(t, *i as usize));
                }
                LoanStep::Tuple(i) => {
                    name.push_str(&format!(".{i}"));
                    ty = ty.and_then(|t| match self.kind(t) {
                        TyKind::Tuple(es) => es.get(*i as usize).copied(),
                        _ => None,
                    });
                }
                LoanStep::Elem => {
                    name.push_str("[_]");
                    ty = ty.and_then(|t| self.elem_ty(t));
                }
                LoanStep::Deref => {
                    name.push_str(".*");
                    ty = ty.and_then(|t| self.peeled(t));
                }
                LoanStep::Raw => {
                    name.push_str(".*");
                    ty = None;
                }
                LoanStep::Payload(_) => {
                    name.push_str(".?");
                    ty = None;
                }
            }
        }
        name
    }

    fn field_label(&self, ty: TypeId, index: usize) -> Option<String> {
        if self.cfg.closure_ty == Some(ty) {
            return self.cfg.closure_fields.get(index).map(|f| f.name.clone());
        }
        let TyKind::Adt(def, _) = self.kind(ty) else {
            return None;
        };
        self.decls
            .struct_sig(def)?
            .fields
            .get(index)
            .map(|f| f.name.clone())
    }

    fn kind(&self, ty: TypeId) -> TyKind {
        self.decls.types.kind(ty).clone()
    }

    fn local_ty(&self, local: Local) -> Option<TypeId> {
        self.cfg.locals.get(local as usize).map(|l| l.ty)
    }

    fn static_ty(&self, def: DefId) -> Option<TypeId> {
        match self.decls.item(def) {
            Some(ItemSig::Static(s)) => Some(s.ty),
            _ => None,
        }
    }

    fn place_ty(&mut self, place: &Place) -> Option<TypeId> {
        let mut ty = match &place.base {
            PlaceBase::Local(l) => self.local_ty(*l)?,
            PlaceBase::Static(d) => self.static_ty(*d)?,
        };
        for proj in &place.proj {
            ty = self.proj_ty(ty, proj)?;
        }
        Some(ty)
    }

    fn proj_ty(&mut self, ty: TypeId, proj: &Projection) -> Option<TypeId> {
        match proj {
            Projection::Field { index, .. } => self.field_ty(ty, *index),
            Projection::TupleField(i) => match self.kind(ty) {
                TyKind::Tuple(es) => es.get(*i).copied(),
                _ => None,
            },
            Projection::Index(_) => self.elem_ty(ty),
            Projection::Deref => self.peeled(ty),
            Projection::DerefRaw => match self.kind(ty) {
                TyKind::RawPtr(_, e) => Some(e),
                _ => None,
            },
            Projection::Payload { variant } => self.payload_ty(ty, *variant as u32),
        }
    }

    fn peeled(&self, ty: TypeId) -> Option<TypeId> {
        match self.kind(ty) {
            TyKind::Ref(_, e) | TyKind::Slice(_, e) => Some(e),
            _ => None,
        }
    }

    fn elem_ty(&self, ty: TypeId) -> Option<TypeId> {
        match self.kind(ty) {
            TyKind::Array(e, _) | TyKind::Slice(_, e) => Some(e),
            _ => None,
        }
    }

    fn field_ty(&mut self, ty: TypeId, index: usize) -> Option<TypeId> {
        if self.cfg.closure_ty == Some(ty) {
            return self.cfg.closure_fields.get(index).map(|f| f.ty);
        }
        let TyKind::Adt(def, args) = self.kind(ty) else {
            return None;
        };
        let field = self.decls.struct_sig(def)?.fields.get(index)?.ty;
        let subst = self.decls.subst_for(def, &args);
        Some(self.decls.types.subst(field, &subst))
    }

    fn payload_ty(&mut self, ty: TypeId, variant: u32) -> Option<TypeId> {
        match self.kind(ty) {
            TyKind::Optional(inner) => (variant == 1).then_some(inner),
            TyKind::Result(ok, err) => match variant {
                0 => Some(ok),
                1 => Some(err),
                _ => None,
            },
            TyKind::Adt(def, args) => {
                let payload = self
                    .decls
                    .enum_sig(def)?
                    .variants
                    .get(variant as usize)?
                    .payload?;
                let subst = self.decls.subst_for(def, &args);
                Some(self.decls.types.subst(payload, &subst))
            }
            _ => None,
        }
    }
}

fn is_borrow_ty(kind: &TyKind) -> bool {
    matches!(
        kind,
        TyKind::Ref(..) | TyKind::Slice(..) | TyKind::Dyn(..) | TyKind::DynFn(..)
    )
}
