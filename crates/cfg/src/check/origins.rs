//! Analysis B: origin sets (checker.md §4 and §6).
//!
//! One forward dataflow per CFG. `O(x)` is the origin set of local `x`. Joins only
//! union, so entry states grow. A strong update or the kill rule runs in the transfer,
//! not in the join.
//!
//! A copy of a `Copy` view (a shared borrow, a shared slice, or any copy view) resets
//! the dereference cursor: the read rule turns the set into `inner` applied once per
//! dereference so far, and the exclusive chain starts over. That is what makes
//! `self.src` on `&Parser` the set `{Inner(self)}` before the shared slice is taken.
//! An exclusive `&mut` or `[]mut` is not `Copy` and does not reset.
//!
//! [`Origins`] keeps the block entry states and the loans. [`sets_before`] replays a
//! block up to a statement so Analysis C can ask for `O(x)` at a program point.

use crate::model::*;
use ingot_diag::diagnostic::{Diagnostic, Edit};
use ingot_diag::source::{Source, Sources, Span};
use ingot_sema::defs::{DefId, Sema};
use ingot_sema::sigs::{Declarations, FnSig, ItemSig, OriginSig, Tri};
use ingot_sema::ty::{GenericArg, Mutability, TyKind, TypeId};
use ingot_sema::typed::{FnRef, FnSource, InterfaceVia};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

/// One element of an origin set. `Scope` is the specification's `Self`.
/// `Capture` is one field of a closure's capture record, named from inside the body.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Origin {
    Loan(u32),
    Param(u32),
    Inner(u32),
    Scope(u32),
    Capture(u32),
}

/// A projection stored on a loan path. A slice index is [`LoanStep::Deref`].
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum LoanStep {
    Field(u32),
    Tuple(u32),
    Elem,
    Deref,
    Raw,
    Payload(u32),
}

/// A loan created at one program point. The id is the index in [`Origins::loans`].
#[derive(Clone, Debug)]
#[allow(dead_code)] // `kind` and `two_phase` are for Analysis C.
pub struct Loan {
    pub id: u32,
    pub local: Local,
    pub proj: Vec<LoanStep>,
    pub kind: Mutability,
    pub on_local: bool,
    pub two_phase: bool,
    pub span: Span,
    pub block: u32,
    pub stmt: u32,
    pub seq: u32,
}

pub type OriginSet = BTreeSet<Origin>;
pub type OriginMap = BTreeMap<Local, OriginSet>;

/// Entry state of every block, and every loan the body created.
#[derive(Clone, Debug)]
pub struct Origins {
    pub entry: Vec<Option<OriginMap>>,
    pub loans: Vec<Loan>,
    pub diverged: bool,
}

impl Origins {
    fn empty(blocks: usize) -> Origins {
        Origins {
            entry: vec![None; blocks],
            loans: Vec::new(),
            diverged: false,
        }
    }
}

/// The `from` clause phase 9 computed for one phase-6 E1201.
#[derive(Clone, Debug)]
pub struct FromFix {
    pub file: String,
    pub span: Span,
    pub insert_at: usize,
    pub text: String,
}

pub(crate) struct Output {
    pub diagnostics: Vec<Diagnostic>,
    pub not_implemented: Vec<String>,
    pub from_fixes: Vec<FromFix>,
    /// Block entry states and loans. Analysis C reads this; nothing in this step does.
    #[allow(dead_code)]
    pub origins: Origins,
}

/// `O(x)` just before statement `stmt_index` of `block`. `stmt_index == stmts.len()`
/// is the point before the terminator. An empty map means the block is unreachable
/// or the analysis gave up on the body.
#[allow(dead_code)]
pub(crate) fn sets_before(
    origins: &Origins,
    sema: &Sema,
    decls: &mut Declarations,
    cfg: &Cfg,
    block: usize,
    stmt_index: usize,
) -> OriginMap {
    if origins.diverged {
        return OriginMap::new();
    }
    let Some(start) = origins.entry.get(block).and_then(|s| s.clone()) else {
        return OriginMap::new();
    };
    let mut cx = Cx::new(sema, decls, cfg, None, origins.loans.clone());
    cx.report = false;
    let mut flow = start;
    let n = cfg.blocks.get(block).map(|b| b.stmts.len()).unwrap_or(0);
    let stop = stmt_index.min(n);
    cx.block_id = block as u32;
    for i in 0..stop {
        let Some(stmt) = cfg.blocks.get(block).and_then(|b| b.stmts.get(i)).cloned() else {
            break;
        };
        cx.stmt_index = i as u32;
        cx.span = stmt.span;
        cx.transfer_stmt(&mut flow, &stmt);
        if cx.diverged {
            return OriginMap::new();
        }
    }
    flow
}

pub(super) fn check_cfg(
    sema: &Sema,
    decls: &mut Declarations,
    sources: &Sources,
    cfg: &Cfg,
) -> Output {
    let n = cfg.blocks.len();
    let Some(source) = sources.files.get(cfg.file) else {
        return Output {
            diagnostics: Vec::new(),
            not_implemented: Vec::new(),
            from_fixes: Vec::new(),
            origins: Origins::empty(n),
        };
    };
    if decls.fn_sig(cfg.key.owner).is_some_and(|s| s.mods.kernel) {
        return Output {
            diagnostics: Vec::new(),
            not_implemented: Vec::new(),
            from_fixes: Vec::new(),
            origins: Origins::empty(n),
        };
    }
    let mut cx = Cx::new(sema, decls, cfg, Some(source), Vec::new());
    let entry = cx.fixpoint();
    if !cx.diverged {
        cx.report = true;
        cx.seen.clear();
        for b in 0..n {
            let Some(start) = entry.get(b).and_then(|s| s.clone()) else {
                continue;
            };
            let mut flow = start;
            cx.transfer_block(&mut flow, b);
            if cx.diverged {
                break;
            }
        }
        cx.finish_from_fix();
    }
    if cx.diverged {
        cx.diags.clear();
        cx.fixes.clear();
        if cx.blocked.is_empty() {
            cx.blocked.push("origin sets did not converge".to_string());
        }
    }
    let diagnostics = cx.diagnostics_of();
    let origins = Origins {
        entry,
        loans: cx.loans,
        diverged: cx.diverged,
    };
    Output {
        diagnostics,
        not_implemented: cx.blocked,
        from_fixes: cx.fixes,
        origins,
    }
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Need {
    Param(u32),
    Inner(u32),
    Scope(u32),
}

struct Cursor {
    set: OriginSet,
    derefs: u32,
    exclusive: bool,
    ty: TypeId,
    steps: Vec<LoanStep>,
    root: Option<Local>,
    static_root: bool,
    raw: bool,
}

struct Note {
    role: &'static str,
    span: Span,
}

struct RawDiag {
    code: &'static str,
    message: String,
    span: Span,
    notes: Vec<Note>,
    fix_message: Option<String>,
    edits: Vec<Edit>,
}

struct Cx<'a> {
    sema: &'a Sema,
    decls: &'a mut Declarations,
    cfg: &'a Cfg,
    source: Option<&'a Source>,
    report: bool,
    span: Span,
    block_id: u32,
    stmt_index: u32,
    diags: Vec<RawDiag>,
    seen: BTreeSet<(u16, usize, usize, String)>,
    loans: Vec<Loan>,
    loan_at: BTreeMap<(u32, u32, u32), u32>,
    /// Borrows already created in the current statement. Reset at each statement
    /// and terminator, so a replay assigns the same ids.
    loan_seq: u32,
    diverged: bool,
    blocked: Vec<String>,
    steps: usize,
    cap: usize,
    sig: Option<FnSig>,
    suppress_1202: bool,
    ret_carries: bool,
    err_carries: bool,
    needs: BTreeSet<Need>,
    fix_blocked: bool,
    fixes: Vec<FromFix>,
}

impl<'a> Cx<'a> {
    fn new(
        sema: &'a Sema,
        decls: &'a mut Declarations,
        cfg: &'a Cfg,
        source: Option<&'a Source>,
        loans: Vec<Loan>,
    ) -> Cx<'a> {
        let mut loan_at = BTreeMap::new();
        for (i, loan) in loans.iter().enumerate() {
            loan_at.insert((loan.block, loan.stmt, loan.seq), i as u32);
        }
        let stmts: usize = cfg
            .blocks
            .iter()
            .map(|b| b.stmts.len().saturating_add(1))
            .sum();
        let cap = cfg
            .blocks
            .len()
            .saturating_mul(cfg.locals.len().saturating_add(1))
            .saturating_mul(stmts.saturating_add(1))
            .saturating_add(1);
        let sig = if cfg.key.body == BodyKey::Item {
            decls.fn_sig(cfg.key.owner).cloned()
        } else {
            None
        };
        let suppress_1202 = sig.as_ref().is_some_and(|s| {
            s.from.is_empty()
                && (decls.written_with_borrow(s.ret)
                    || s.error.is_some_and(|e| decls.written_with_borrow(e)))
        });
        let mut carries = |ty: TypeId| {
            !decls.types.has_poison(ty) && {
                let p = decls.props(ty);
                p.view == Tri::Yes || !p.maybe_view.is_no()
            }
        };
        let ret_carries = carries(cfg.ret);
        let err_carries = cfg.error.is_some_and(carries);
        Cx {
            sema,
            decls,
            cfg,
            source,
            report: false,
            span: Span::default(),
            block_id: 0,
            stmt_index: 0,
            diags: Vec::new(),
            seen: BTreeSet::new(),
            loans,
            loan_at,
            loan_seq: 0,
            diverged: false,
            blocked: Vec::new(),
            steps: 0,
            cap,
            sig,
            suppress_1202,
            ret_carries,
            err_carries,
            needs: BTreeSet::new(),
            fix_blocked: false,
            fixes: Vec::new(),
        }
    }

    fn block_reason(&mut self, what: &str) {
        self.diverged = true;
        if !self.blocked.iter().any(|m| m == what) {
            self.blocked.push(what.to_string());
        }
    }

    fn open_body(&self) -> bool {
        self.cfg.closure_ty.is_some() || matches!(self.cfg.key.body, BodyKey::ParFor(_))
    }

    fn fixpoint(&mut self) -> Vec<Option<OriginMap>> {
        let n = self.cfg.blocks.len();
        let mut entry = vec![None; n];
        if n == 0 {
            return entry;
        }
        let start = self.cfg.entry as usize;
        if start >= n {
            return entry;
        }
        entry[start] = Some(self.initial());
        let mut queue = VecDeque::new();
        let mut queued = BTreeSet::new();
        queue.push_back(start);
        queued.insert(start);
        while let Some(b) = queue.pop_front() {
            queued.remove(&b);
            if self.diverged {
                break;
            }
            self.steps = self.steps.saturating_add(1);
            if self.steps > self.cap {
                self.block_reason("origin sets did not converge");
                break;
            }
            let Some(start_state) = entry.get(b).and_then(|s| s.clone()) else {
                continue;
            };
            let mut flow = start_state;
            self.transfer_block(&mut flow, b);
            for s in self.successors(b) {
                if s >= n {
                    continue;
                }
                let changed = match entry.get_mut(s) {
                    Some(Some(have)) => merge(have, &flow),
                    _ => {
                        entry[s] = Some(flow.clone());
                        true
                    }
                };
                if changed && queued.insert(s) {
                    queue.push_back(s);
                }
            }
        }
        entry
    }

    fn initial(&mut self) -> OriginMap {
        let mut map = OriginMap::new();
        for (i, local) in self.cfg.params.iter().copied().enumerate() {
            let Some(loc) = self.cfg.locals.get(local as usize) else {
                continue;
            };
            let origin = if self.is_scope_ty(loc.ty) {
                Origin::Scope(i as u32)
            } else if self.carries(loc.ty) {
                Origin::Param(i as u32)
            } else {
                continue;
            };
            map.entry(local).or_default().insert(origin);
        }
        map
    }

    fn successors(&self, block: usize) -> Vec<usize> {
        let Some(data) = self.cfg.blocks.get(block) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        match &data.term {
            Terminator::Goto(b) => out.push(*b as usize),
            Terminator::Branch {
                then_block,
                else_block,
                ..
            } => {
                out.push(*then_block as usize);
                out.push(*else_block as usize);
            }
            Terminator::Switch { cases, default, .. } => {
                for (_, b) in cases {
                    out.push(*b as usize);
                }
                out.push(*default as usize);
            }
            Terminator::Call { next, .. }
            | Terminator::Spawn { next, .. }
            | Terminator::ScopeWait { next, .. }
            | Terminator::ParFor { next, .. }
            | Terminator::Dispatch { next, .. } => out.push(*next as usize),
            Terminator::Select { ops } => {
                for op in ops {
                    out.push(op.successor as usize);
                }
            }
            Terminator::Return { .. } | Terminator::Trap { .. } | Terminator::Unreachable => {}
        }
        out
    }

    fn transfer_block(&mut self, flow: &mut OriginMap, block: usize) {
        self.block_id = block as u32;
        let n = self
            .cfg
            .blocks
            .get(block)
            .map(|b| b.stmts.len())
            .unwrap_or(0);
        for i in 0..n {
            if self.diverged {
                return;
            }
            let Some(stmt) = self
                .cfg
                .blocks
                .get(block)
                .and_then(|b| b.stmts.get(i))
                .cloned()
            else {
                break;
            };
            self.stmt_index = i as u32;
            self.span = stmt.span;
            self.transfer_stmt(flow, &stmt);
        }
        if self.diverged {
            return;
        }
        let Some(term) = self.cfg.blocks.get(block).map(|b| b.term.clone()) else {
            return;
        };
        self.stmt_index = n as u32;
        self.span = self
            .cfg
            .blocks
            .get(block)
            .map(|b| b.term_span)
            .unwrap_or_default();
        self.transfer_term(flow, &term);
    }

    fn transfer_stmt(&mut self, flow: &mut OriginMap, stmt: &Stmt) {
        self.loan_seq = 0;
        match &stmt.kind {
            StmtKind::Assign { place, value } => {
                let dest_ty = self.place_ty(place);
                let set = self.eval_rvalue(flow, value, dest_ty);
                self.store_place(flow, place, &set, true);
            }
            StmtKind::StorageStart(_)
            | StmtKind::StorageEnd(_)
            | StmtKind::EnterUnsafe
            | StmtKind::Cancel(_)
            | StmtKind::Abandon(_)
            | StmtKind::Check(_)
            | StmtKind::Use(_)
            | StmtKind::Variant { .. }
            | StmtKind::IterConsumed(_) => {}
        }
    }

    fn transfer_term(&mut self, flow: &mut OriginMap, term: &Terminator) {
        self.loan_seq = 0;
        match term {
            Terminator::Return {
                value, is_error, ..
            } => {
                let carries = if *is_error {
                    self.err_carries
                } else {
                    self.ret_carries
                };
                if !carries {
                    return;
                }
                let Some(op) = value else {
                    return;
                };
                let set = self.eval_operand(flow, op);
                self.check_return(&set, *is_error);
            }
            Terminator::Call {
                target, args, dest, ..
            } => self.transfer_call(flow, target, args, dest.as_ref()),
            // Spawn, par for, dispatch and select are later steps. Leaving their
            // results empty under-approximates; it does not invent an E12xx.
            Terminator::Goto(_)
            | Terminator::Branch { .. }
            | Terminator::Switch { .. }
            | Terminator::Trap { .. }
            | Terminator::Spawn { .. }
            | Terminator::ScopeWait { .. }
            | Terminator::Select { .. }
            | Terminator::ParFor { .. }
            | Terminator::Dispatch { .. }
            | Terminator::Unreachable => {}
        }
    }

    fn eval_rvalue(
        &mut self,
        flow: &mut OriginMap,
        value: &Rvalue,
        dest_ty: Option<TypeId>,
    ) -> OriginSet {
        match value {
            Rvalue::Use(op) => self.eval_operand(flow, op),
            Rvalue::Unary { operand, .. }
            | Rvalue::Cast {
                kind: CastKind::Identity,
                operand,
            }
            | Rvalue::DynCast { operand, .. } => self.eval_operand(flow, operand),
            Rvalue::Cast {
                kind: CastKind::RefToPtr,
                ..
            } => OriginSet::new(),
            Rvalue::Cast { operand, .. } => self.eval_operand(flow, operand),
            Rvalue::Binary { lhs, rhs, .. } => {
                let a = self.eval_operand(flow, lhs);
                let b = self.eval_operand(flow, rhs);
                union2(&a, &b)
            }
            Rvalue::Borrow {
                place,
                mutability,
                two_phase,
            } => {
                let c = self.walk(flow, place);
                self.finish_borrow(flow, &c, *mutability, *two_phase)
            }
            Rvalue::Reborrow { place, two_phase } => {
                let mut c = self.walk(flow, place);
                self.push_deref(&mut c, false);
                self.finish_borrow(flow, &c, Mutability::Mut, *two_phase)
            }
            Rvalue::Slice {
                base,
                borrow,
                two_phase,
                ..
            } => self.eval_slice(flow, base, *borrow, *two_phase),
            Rvalue::SliceElem { base, .. } => self.read_place(flow, base),
            Rvalue::Aggregate { kind, fields, .. } => {
                let sets: Vec<OriginSet> =
                    fields.iter().map(|f| self.eval_operand(flow, f)).collect();
                let all = union_all(&sets);
                if matches!(kind, AggregateKind::Closure(_)) {
                    // The body may move one capture's view into another, so every
                    // exclusive borrow of a view receives the union of the *other*
                    // captures' sets (checker.md §4.6). Its own loan is not stored
                    // into the place it borrows.
                    for (i, f) in fields.iter().enumerate() {
                        if !self.exclusive_view_operand(f) {
                            continue;
                        }
                        let others: Vec<OriginSet> = sets
                            .iter()
                            .enumerate()
                            .filter(|(j, _)| *j != i)
                            .map(|(_, s)| s.clone())
                            .collect();
                        let others = union_all(&others);
                        self.store_through(flow, f, &others);
                    }
                }
                all
            }
            Rvalue::Discriminant(_) | Rvalue::SliceLen(_) | Rvalue::SlicePtr(_) => OriginSet::new(),
            Rvalue::Builtin { name, args, .. } => self.eval_builtin(flow, name, args, dest_ty),
        }
    }

    fn eval_operand(&mut self, flow: &OriginMap, op: &Operand) -> OriginSet {
        match op {
            Operand::Place(p, _) => self.read_place(flow, p),
            Operand::Const(_) => OriginSet::new(),
        }
    }

    /// `&mut` or `[]mut` of a view: the closure may store another capture through it.
    fn exclusive_view_operand(&mut self, op: &Operand) -> bool {
        let Operand::Place(place, _) = op else {
            return false;
        };
        let Some(ty) = self.place_ty(place) else {
            return false;
        };
        let inner = match self.kind(ty) {
            TyKind::Ref(Mutability::Mut, t) | TyKind::Slice(Mutability::Mut, t) => t,
            _ => return false,
        };
        self.carries(inner)
    }

    fn eval_slice(
        &mut self,
        flow: &OriginMap,
        base: &Place,
        borrow: Option<Mutability>,
        two_phase: bool,
    ) -> OriginSet {
        let mut c = self.walk(flow, base);
        if let Some(m) = borrow {
            return self.finish_borrow(flow, &c, m, two_phase);
        }
        let shared = !matches!(
            self.kind(c.ty),
            TyKind::Slice(Mutability::Mut, _) | TyKind::Ref(Mutability::Mut, _)
        );
        self.push_deref(&mut c, shared);
        let kind = if shared {
            Mutability::Shared
        } else {
            Mutability::Mut
        };
        self.finish_borrow(flow, &c, kind, two_phase)
    }

    fn eval_builtin(
        &mut self,
        flow: &mut OriginMap,
        name: &str,
        args: &[BuiltinArg],
        dest_ty: Option<TypeId>,
    ) -> OriginSet {
        match name {
            "replace" => self.eval_replace(flow, args),
            "swap" => {
                self.eval_swap(flow, args);
                OriginSet::new()
            }
            "forget" | "bits" | "slice_from_raw" | "borrow_from_raw" => OriginSet::new(),
            "unchecked_slice" => {
                let Some(BuiltinArg::Place(base)) = args.first() else {
                    return OriginSet::new();
                };
                let borrow = dest_ty.and_then(|t| match self.kind(t) {
                    TyKind::Slice(m, _) => Some(m),
                    _ => None,
                });
                let base_ty = self.place_ty(base);
                let of_array = base_ty.is_some_and(|t| matches!(self.kind(t), TyKind::Array(..)));
                if of_array {
                    self.eval_slice(flow, base, borrow.or(Some(Mutability::Shared)), false)
                } else {
                    self.eval_slice(flow, base, None, false)
                }
            }
            "unchecked_index" => {
                let Some(BuiltinArg::Place(base)) = args.first() else {
                    return OriginSet::new();
                };
                self.read_index(flow, base)
            }
            _ => {
                if dest_ty.is_some_and(|t| self.carries(t)) {
                    self.block_reason(&format!("origin set of @{name}"));
                }
                OriginSet::new()
            }
        }
    }

    fn eval_replace(&mut self, flow: &mut OriginMap, args: &[BuiltinArg]) -> OriginSet {
        let Some(BuiltinArg::Place(slot)) = args.first() else {
            return OriginSet::new();
        };
        let old = self.read_place(flow, slot);
        let vset = match args.get(1) {
            Some(BuiltinArg::Value(op)) => self.eval_operand(flow, op),
            _ => OriginSet::new(),
        };
        self.store_place(flow, slot, &vset, false);
        old
    }

    fn eval_swap(&mut self, flow: &mut OriginMap, args: &[BuiltinArg]) {
        let Some(BuiltinArg::Place(a)) = args.first() else {
            return;
        };
        let Some(BuiltinArg::Place(b)) = args.get(1) else {
            return;
        };
        let sa = self.read_place(flow, a);
        let sb = self.read_place(flow, b);
        self.store_place(flow, a, &sb, false);
        self.store_place(flow, b, &sa, false);
    }

    fn read_index(&mut self, flow: &OriginMap, place: &Place) -> OriginSet {
        let mut c = self.walk(flow, place);
        if matches!(self.kind(c.ty), TyKind::Slice(..)) {
            let shared = matches!(self.kind(c.ty), TyKind::Slice(Mutability::Shared, _));
            self.push_deref(&mut c, shared);
        }
        if c.raw || c.static_root {
            return OriginSet::new();
        }
        if !self.carries(c.ty) && c.derefs == 0 {
            return OriginSet::new();
        }
        self.finish_read(flow, &c)
    }

    fn read_place(&mut self, flow: &OriginMap, place: &Place) -> OriginSet {
        let c = self.walk(flow, place);
        if c.raw || c.static_root {
            return OriginSet::new();
        }
        if !self.carries(c.ty) && c.derefs == 0 {
            return OriginSet::new();
        }
        self.finish_read(flow, &c)
    }

    fn walk(&mut self, flow: &OriginMap, place: &Place) -> Cursor {
        let (set, root, static_root, ty) = match &place.base {
            PlaceBase::Local(l) => {
                let ty = self.local_ty(*l).unwrap_or(self.decls.types.poison);
                let set = flow.get(l).cloned().unwrap_or_default();
                (set, Some(*l), false, ty)
            }
            PlaceBase::Static(d) => {
                let ty = self.static_ty(*d).unwrap_or(self.decls.types.poison);
                (OriginSet::new(), None, true, ty)
            }
        };
        let mut c = Cursor {
            set,
            derefs: 0,
            exclusive: false,
            ty,
            steps: Vec::new(),
            root,
            static_root,
            raw: false,
        };
        for proj in &place.proj {
            self.project(flow, &mut c, proj);
        }
        c
    }

    fn project(&mut self, flow: &OriginMap, c: &mut Cursor, proj: &Projection) {
        if c.raw {
            return;
        }
        match proj {
            Projection::Deref => self.push_deref(c, self.shared_borrow(c.ty)),
            Projection::DerefRaw => {
                c.raw = true;
                c.set.clear();
                c.derefs = 0;
                c.exclusive = false;
                c.steps.push(LoanStep::Raw);
            }
            Projection::Index(_) => {
                let step = self.loan_step(c.ty, proj);
                if matches!(step, LoanStep::Deref) {
                    self.push_deref(c, self.shared_borrow(c.ty));
                } else {
                    c.steps.push(step);
                    if let Some(t) = self.elem_ty(c.ty) {
                        c.ty = t;
                    }
                    self.maybe_reset(flow, c);
                }
            }
            Projection::Field { index, .. } => {
                let capture = self.at_capture(c);
                c.steps.push(self.loan_step(c.ty, proj));
                if let Some(t) = self.field_ty(c.ty, *index) {
                    c.ty = t;
                }
                self.maybe_reset(flow, c);
                if capture {
                    c.set.insert(Origin::Capture(*index as u32));
                }
            }
            Projection::TupleField(i) => {
                c.steps.push(LoanStep::Tuple(*i as u32));
                if let TyKind::Tuple(es) = self.kind(c.ty)
                    && let Some(t) = es.get(*i).copied()
                {
                    c.ty = t;
                }
                self.maybe_reset(flow, c);
            }
            Projection::Payload { variant } => {
                c.steps.push(LoanStep::Payload(*variant as u32));
                if let Some(t) = self.payload_ty(c.ty, *variant as u32) {
                    c.ty = t;
                }
                self.maybe_reset(flow, c);
            }
        }
    }

    fn push_deref(&mut self, c: &mut Cursor, shared: bool) {
        if !shared {
            c.exclusive = true;
        }
        c.derefs = c.derefs.saturating_add(1);
        c.steps.push(LoanStep::Deref);
        if let Some(t) = self.peeled(c.ty) {
            c.ty = t;
        }
    }

    fn maybe_reset(&mut self, flow: &OriginMap, c: &mut Cursor) {
        if c.derefs == 0 || !self.is_copy_view(c.ty) {
            return;
        }
        c.set = self.inner_n(&c.set, flow, c.derefs);
        c.derefs = 0;
        c.exclusive = false;
    }

    fn finish_read(&mut self, flow: &OriginMap, c: &Cursor) -> OriginSet {
        if c.derefs == 0 {
            c.set.clone()
        } else {
            self.inner_n(&c.set, flow, c.derefs)
        }
    }

    fn finish_borrow(
        &mut self,
        flow: &OriginMap,
        c: &Cursor,
        kind: Mutability,
        two_phase: bool,
    ) -> OriginSet {
        if c.static_root || c.raw || c.root.is_none() {
            return OriginSet::new();
        }
        if c.derefs == 0 {
            let loan = self.make_loan(c, kind, two_phase);
            let mut s = OriginSet::new();
            s.insert(loan);
            return s;
        }
        if !c.exclusive {
            if c.derefs >= 2 {
                return self.inner_star(&c.set, flow);
            }
            return c.set.clone();
        }
        let loan = self.make_loan(c, kind, two_phase);
        let mut s = c.set.clone();
        if c.derefs >= 2 {
            s.extend(self.inner_star(&c.set, flow));
        }
        s.insert(loan);
        s
    }

    fn make_loan(&mut self, c: &Cursor, kind: Mutability, two_phase: bool) -> Origin {
        let seq = self.loan_seq;
        self.loan_seq = self.loan_seq.saturating_add(1);
        let key = (self.block_id, self.stmt_index, seq);
        if let Some(&id) = self.loan_at.get(&key) {
            return Origin::Loan(id);
        }
        let id = self.loans.len() as u32;
        let on_local = !c
            .steps
            .iter()
            .any(|s| matches!(s, LoanStep::Deref | LoanStep::Raw));
        self.loans.push(Loan {
            id,
            local: c.root.unwrap_or(0),
            proj: c.steps.clone(),
            kind,
            on_local,
            two_phase,
            span: self.span,
            block: self.block_id,
            stmt: self.stmt_index,
            seq,
        });
        self.loan_at.insert(key, id);
        Origin::Loan(id)
    }

    fn store_place(&mut self, flow: &mut OriginMap, place: &Place, set: &OriginSet, strong: bool) {
        if self.diverged {
            return;
        }
        if !set.is_empty() {
            self.check_capture_store(place, set);
        }
        let Some(root) = place.root() else {
            return;
        };
        let killed = self.killed_loans(place);
        self.apply_kill(flow, &killed);
        let mut set = set.clone();
        strip(&mut set, &killed);
        let steps = self.loan_steps_of(place);
        let derefs = deref_count(&steps);
        if derefs == 0 {
            if !self.carries_local(root) {
                return;
            }
            if strong && place.proj.is_empty() {
                strong_update(flow, root, &set);
            } else {
                union_into(flow, root, &set);
            }
            return;
        }
        if set.is_empty() {
            return;
        }
        let origins = flow.get(&root).cloned().unwrap_or_default();
        let rest = after_first_deref(&steps);
        self.widen(flow, &origins, &rest, &set, 0);
    }

    fn store_through(&mut self, flow: &mut OriginMap, op: &Operand, set: &OriginSet) {
        let Operand::Place(place, _) = op else {
            return;
        };
        let Some(root) = place.root() else {
            return;
        };
        let mut steps = self.loan_steps_of(place);
        steps.push(LoanStep::Deref);
        let origins = flow.get(&root).cloned().unwrap_or_default();
        let rest = after_first_deref(&steps);
        self.widen(flow, &origins, &rest, set, 0);
    }

    fn widen(
        &mut self,
        flow: &mut OriginMap,
        origins: &OriginSet,
        remaining: &[LoanStep],
        set: &OriginSet,
        depth: usize,
    ) {
        if self.diverged || set.is_empty() {
            return;
        }
        let limit = self
            .loans
            .len()
            .saturating_add(self.cfg.locals.len())
            .saturating_add(8);
        if depth > limit {
            self.block_reason("origin sets did not converge");
            return;
        }
        let origins: Vec<Origin> = origins.iter().copied().collect();
        for origin in origins {
            if self.diverged {
                return;
            }
            match origin {
                Origin::Loan(id) => {
                    let Some(loan) = self.loans.get(id as usize).cloned() else {
                        continue;
                    };
                    let mut combined = loan.proj.clone();
                    combined.extend_from_slice(remaining);
                    if deref_count(&combined) == 0 {
                        union_into(flow, loan.local, set);
                    } else {
                        self.store_steps(flow, loan.local, &combined, set, depth.saturating_add(1));
                    }
                }
                Origin::Param(p) | Origin::Inner(p) | Origin::Scope(p) => {
                    self.check_store_param(p, set);
                }
                Origin::Capture(_) => {}
            }
        }
    }

    fn store_steps(
        &mut self,
        flow: &mut OriginMap,
        local: Local,
        steps: &[LoanStep],
        set: &OriginSet,
        depth: usize,
    ) {
        if self.diverged {
            return;
        }
        let limit = self
            .loans
            .len()
            .saturating_add(self.cfg.locals.len())
            .saturating_add(8);
        if depth > limit {
            self.block_reason("origin sets did not converge");
            return;
        }
        if deref_count(steps) == 0 {
            if self.carries_local(local) {
                union_into(flow, local, set);
            }
            return;
        }
        let origins = flow.get(&local).cloned().unwrap_or_default();
        let rest = after_first_deref(steps);
        self.widen(flow, &origins, &rest, set, depth.saturating_add(1));
    }

    fn killed_loans(&mut self, place: &Place) -> BTreeSet<u32> {
        let Some(ty) = self.place_ty(place) else {
            return BTreeSet::new();
        };
        if !is_borrow_ty(&self.kind(ty)) {
            return BTreeSet::new();
        }
        let steps = self.loan_steps_of(place);
        if deref_count(&steps) > 0 {
            return BTreeSet::new();
        }
        let Some(root) = place.root() else {
            return BTreeSet::new();
        };
        let mut ids = BTreeSet::new();
        for loan in &self.loans {
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

    fn apply_kill(&self, flow: &mut OriginMap, killed: &BTreeSet<u32>) {
        if killed.is_empty() {
            return;
        }
        for set in flow.values_mut() {
            strip(set, killed);
        }
    }

    /// A store whose set holds `Capture(i)` and whose place reaches a different
    /// exclusive capture (checker.md §4.6). No fix: a closure has no clauses.
    fn check_capture_store(&mut self, place: &Place, set: &OriginSet) {
        if self.cfg.closure_ty.is_none() {
            return;
        }
        let Some(dst) = self.capture_index(place) else {
            return;
        };
        if !self.field_exclusive(dst) {
            return;
        }
        let mut srcs = BTreeSet::new();
        for origin in set {
            if let Origin::Capture(src) = *origin
                && src != dst
            {
                srcs.insert(src);
            }
        }
        if srcs.is_empty() {
            return;
        }
        let src_names: Vec<String> = srcs.iter().map(|s| self.capture_name(*s)).collect();
        let dst_name = self.capture_name(dst);
        let mut notes = Vec::new();
        for src in &srcs {
            if let Some(span) = self.cfg.closure_fields.get(*src as usize).map(|f| f.span) {
                notes.push(Note {
                    role: "signature",
                    span,
                });
            }
        }
        self.error(
            "E1204",
            format!(
                "the view from `{}` is stored into capture `{dst_name}`",
                src_names.join("`, `")
            ),
            notes,
            None,
            Vec::new(),
        );
    }

    fn capture_name(&self, index: u32) -> String {
        self.cfg
            .closure_fields
            .get(index as usize)
            .map(|f| f.name.clone())
            .unwrap_or_else(|| "capture".to_string())
    }

    fn capture_index(&self, place: &Place) -> Option<u32> {
        let root = place.root()?;
        if self.cfg.params.first().copied() != Some(root) {
            return None;
        }
        let rest: &[Projection] = match place.proj.first() {
            Some(Projection::Deref) => &place.proj[1..],
            _ => &place.proj,
        };
        match rest.first() {
            Some(Projection::Field { index, .. }) => Some(*index as u32),
            _ => None,
        }
    }

    fn field_exclusive(&self, index: u32) -> bool {
        let Some(ty) = self.cfg.closure_fields.get(index as usize).map(|f| f.ty) else {
            return false;
        };
        matches!(
            self.kind(ty),
            TyKind::Ref(Mutability::Mut, _) | TyKind::Slice(Mutability::Mut, _)
        )
    }

    fn check_store_param(&mut self, dest: u32, set: &OriginSet) {
        if !self.report || self.diverged {
            return;
        }
        let mut srcs = BTreeSet::new();
        let mut loans = Vec::new();
        let mut caps = BTreeSet::new();
        for origin in set {
            if self.store_ok(dest, *origin) {
                continue;
            }
            match *origin {
                Origin::Loan(id) => loans.push(id),
                Origin::Param(p) | Origin::Inner(p) | Origin::Scope(p) => {
                    srcs.insert(p);
                }
                Origin::Capture(i) => {
                    caps.insert(i);
                }
            }
        }
        if !srcs.is_empty() {
            self.emit_1204(dest, &srcs);
        }
        if !caps.is_empty() {
            let names: Vec<String> = caps.iter().map(|i| self.capture_name(*i)).collect();
            let dst = self.param_name(dest);
            let mut notes = Vec::new();
            for cap in &caps {
                if let Some(span) = self.cfg.closure_fields.get(*cap as usize).map(|f| f.span) {
                    notes.push(Note {
                        role: "signature",
                        span,
                    });
                }
            }
            self.error(
                "E1204",
                format!(
                    "the view from `{}` is stored where it reaches `{dst}`",
                    names.join("`, `")
                ),
                notes,
                None,
                Vec::new(),
            );
        }
        if !loans.is_empty() {
            self.emit_1205(&loans);
        }
    }

    fn store_ok(&self, dest: u32, origin: Origin) -> bool {
        match origin {
            Origin::Loan(id) => self.loans.get(id as usize).is_none_or(|l| !l.on_local),
            Origin::Inner(p) if p == dest => true,
            Origin::Scope(p) if p == dest => true,
            // Reaching the capture record is the path check_capture_store already judged.
            Origin::Capture(_) => self.cfg.closure_ty.is_some() && dest == 0,
            Origin::Param(p) | Origin::Inner(p) | Origin::Scope(p) => {
                self.declared_into(p, dest) || self.type_param_store(origin, dest)
            }
        }
    }

    fn declared_into(&self, src: u32, dest: u32) -> bool {
        let Some(sig) = &self.sig else {
            return false;
        };
        sig.params.get(src as usize).is_some_and(|p| {
            p.into
                .as_ref()
                .is_some_and(|(_, idx)| *idx == Some(dest as usize))
        })
    }

    /// `Param(src)` passes into `dest` when a type parameter of `dest` occurs in
    /// `src` outside any borrow. `Inner(src)` passes when the two types share any
    /// type parameter (checker.md §6.2).
    fn type_param_store(&self, origin: Origin, dest: u32) -> bool {
        let (src, inner) = match origin {
            Origin::Param(p) => (p, false),
            Origin::Inner(p) => (p, true),
            _ => return false,
        };
        let Some(src_ty) = self.param_ty(src) else {
            return false;
        };
        let Some(dest_ty) = self.param_ty(dest) else {
            return false;
        };
        if inner {
            return share_params(&self.param_defs(src_ty), &self.param_defs(dest_ty));
        }
        self.param_defs(dest_ty)
            .iter()
            .any(|t| self.mentions(src_ty, *t).0)
    }

    fn check_return(&mut self, set: &OriginSet, is_error: bool) {
        let mut missing = Vec::new();
        let mut local_loans = Vec::new();
        for origin in set {
            match *origin {
                Origin::Loan(id) => {
                    if self.loans.get(id as usize).is_some_and(|l| l.on_local) {
                        local_loans.push(id);
                    }
                }
                Origin::Param(p) => {
                    if !self.open_body() && !self.return_ok_param(p, is_error) {
                        missing.push(Need::Param(p));
                    }
                }
                Origin::Inner(p) => {
                    if !self.open_body() && !self.return_ok_inner(p, is_error) {
                        missing.push(Need::Inner(p));
                    }
                }
                Origin::Scope(p) => {
                    if !self.open_body() && !self.return_ok_scope(p) {
                        missing.push(Need::Scope(p));
                    }
                }
                // A closure may return any capture. Nowhere else produces one.
                Origin::Capture(_) => {}
            }
        }
        if !local_loans.is_empty() {
            self.fix_blocked = true;
            self.emit_1203(&local_loans);
        }
        if self.suppress_1202 {
            if self.report {
                self.needs.extend(missing);
            }
            return;
        }
        // `from c` is already E1206 and names no parameter. E1202 would only
        // restate that. A clause that also names a real parameter is checked.
        if self.clause_names_nothing() {
            return;
        }
        if !missing.is_empty() {
            self.emit_1202(&missing);
        }
    }

    /// Every `from` origin is a name that is not a parameter and not `static`.
    fn clause_names_nothing(&self) -> bool {
        self.sig.as_ref().is_some_and(|s| {
            !s.from.is_empty() && s.from.iter().all(|o| o.param.is_none() && !o.is_static)
        })
    }

    fn return_ok_param(&self, p: u32, is_error: bool) -> bool {
        self.has_from(p, false) || self.result_mentions_outside(p, is_error)
    }

    fn return_ok_inner(&self, p: u32, is_error: bool) -> bool {
        self.has_from(p, false) || self.has_from(p, true) || self.result_mentions_any(p, is_error)
    }

    /// The type of the value being returned: `T` for `return`, `E` for `fail`.
    fn result_ty(&self, is_error: bool) -> Option<TypeId> {
        let sig = self.sig.as_ref()?;
        if is_error { sig.error } else { Some(sig.ret) }
    }

    /// A type parameter of the result occurs in `p` outside any borrow (§6.1).
    fn result_mentions_outside(&self, p: u32, is_error: bool) -> bool {
        let Some(ret) = self.result_ty(is_error) else {
            return false;
        };
        let Some(pt) = self.param_ty(p) else {
            return false;
        };
        self.param_defs(ret).iter().any(|t| self.mentions(pt, *t).0)
    }

    /// The result and `p` mention a common type parameter, inside a borrow or not.
    fn result_mentions_any(&self, p: u32, is_error: bool) -> bool {
        let Some(ret) = self.result_ty(is_error) else {
            return false;
        };
        let Some(pt) = self.param_ty(p) else {
            return false;
        };
        share_params(&self.param_defs(ret), &self.param_defs(pt))
    }

    fn return_ok_scope(&self, p: u32) -> bool {
        self.has_from(p, false)
    }

    fn has_from(&self, p: u32, inner: bool) -> bool {
        let Some(sig) = &self.sig else {
            return false;
        };
        sig.from
            .iter()
            .any(|o| o.param == Some(p as usize) && o.inner == inner)
    }

    fn transfer_call(
        &mut self,
        flow: &mut OriginMap,
        target: &CallTarget,
        args: &[Operand],
        dest: Option<&Place>,
    ) {
        let arg_sets: Vec<OriginSet> = args.iter().map(|a| self.eval_operand(flow, a)).collect();
        let dest_ty = dest.and_then(|p| self.place_ty(p));
        let result = match target {
            CallTarget::Fn { fn_ref, .. } => self.call_fn(flow, fn_ref, args, &arg_sets, dest_ty),
            CallTarget::Value { callee, .. } => {
                // Lowering borrows the callable for the call. That loan is not an
                // origin of the result (checker.md §4.5 step 4): replace it with
                // the origin set of the value it borrows.
                let cset = self.eval_operand(flow, callee);
                let cset = self.unwrap_call_borrow(flow, &cset);
                self.value_result(flow, &arg_sets, &cset)
            }
        };
        if let Some(dest) = dest {
            self.store_place(flow, dest, &result, true);
        }
    }

    /// `inner⁺(A) = inner*(inner(A))` (checker.md §4.5 step 2): the borrows hidden
    /// inside what `A` points at, without `A`'s own loan and `Param` origins.
    fn inner_plus(&mut self, set: &OriginSet, flow: &OriginMap) -> OriginSet {
        let first = self.inner(set, flow);
        self.inner_star(&first, flow)
    }

    /// Drop loans on the callable's own storage. Each is replaced by a read of
    /// its path, which for a borrow of the closure is `O(closure)`.
    fn unwrap_call_borrow(&self, flow: &OriginMap, set: &OriginSet) -> OriginSet {
        let mut out = OriginSet::new();
        let mut stack = BTreeSet::new();
        for origin in set {
            match *origin {
                Origin::Loan(id) if self.loans.get(id as usize).is_some_and(|l| l.on_local) => {
                    let mut inner = self.loan_read(id, flow, &mut stack);
                    inner.remove(&Origin::Loan(id));
                    out.extend(inner);
                }
                other => {
                    out.insert(other);
                }
            }
        }
        out
    }

    fn value_result(
        &mut self,
        flow: &OriginMap,
        args: &[OriginSet],
        callee: &OriginSet,
    ) -> OriginSet {
        let mut s = callee.clone();
        s.extend(self.inner_star(callee, flow));
        for a in args {
            s.extend(a.iter().copied());
            s.extend(self.inner_star(a, flow));
        }
        s
    }

    fn call_fn(
        &mut self,
        flow: &mut OriginMap,
        fn_ref: &FnRef,
        args: &[Operand],
        arg_sets: &[OriginSet],
        dest_ty: Option<TypeId>,
    ) -> OriginSet {
        let Some(sig) = self.decls.fn_sig(fn_ref.def).cloned() else {
            if dest_ty.is_some_and(|t| self.carries(t)) {
                self.block_reason("origin set of a call with no signature");
            }
            return OriginSet::new();
        };
        for (i, p) in sig.params.iter().enumerate() {
            let Some((_, Some(dest_i))) = p.into else {
                continue;
            };
            let Some(set) = arg_sets.get(i) else {
                continue;
            };
            let Some(arg) = args.get(dest_i) else {
                continue;
            };
            self.store_through(flow, arg, set);
        }
        let type_params = self.callee_type_params(fn_ref);
        let mut u_of: Vec<(DefId, OriginSet)> = Vec::new();
        for t in &type_params {
            let mut parts = Vec::new();
            for (i, p) in sig.params.iter().enumerate() {
                let mut part = OriginSet::new();
                if let Some(a) = arg_sets.get(i) {
                    let (outside, inside) = self.mentions(p.ty, *t);
                    if outside {
                        part.extend(a.iter().copied());
                    }
                    if inside {
                        // Borrows hidden inside `T`, not the borrow itself: the
                        // argument's own loan and `Param` are not a `T` (§4.5 step 2).
                        part.extend(self.inner_plus(a, flow));
                    }
                }
                parts.push(part);
            }
            let mut u = OriginSet::new();
            for part in &parts {
                u.extend(part.iter().copied());
            }
            for (i, p) in sig.params.iter().enumerate() {
                if self.is_mut_ref(p.ty)
                    && mentions_any(&self.mentions(p.ty, *t))
                    && let Some(arg) = args.get(i)
                {
                    self.store_through(flow, arg, &u);
                }
            }
            u_of.push((*t, u));
        }
        let mut result = OriginSet::new();
        for o in &sig.from {
            if o.is_static {
                continue;
            }
            let Some(i) = o.param else {
                continue;
            };
            let Some(a) = arg_sets.get(i) else {
                continue;
            };
            if o.inner {
                result.extend(self.inner(a, flow));
            } else {
                result.extend(a.iter().copied());
                result.extend(self.inner_star(a, flow));
            }
        }
        for (t, u) in &u_of {
            if mentions_any(&self.mentions(sig.ret, *t)) {
                result.extend(u.iter().copied());
            }
        }
        result
    }

    fn callee_type_params(&self, fn_ref: &FnRef) -> Vec<DefId> {
        let mut out = Vec::new();
        let push_owner = |owner: DefId, out: &mut Vec<DefId>, decls: &Declarations| {
            for g in decls.generics_of(owner) {
                if !g.is_const && !out.contains(&g.def) {
                    out.push(g.def);
                }
            }
        };
        push_owner(fn_ref.def, &mut out, self.decls);
        match &fn_ref.source {
            FnSource::Free => {}
            FnSource::Inherent { impl_def, .. } => push_owner(*impl_def, &mut out, self.decls),
            FnSource::Interface { interface, via, .. } => {
                push_owner(*interface, &mut out, self.decls);
                if let InterfaceVia::Impl { impl_def, .. } = via {
                    push_owner(*impl_def, &mut out, self.decls);
                }
                if let Some(sig) = self.decls.interface_sig(*interface)
                    && !out.contains(&sig.self_def)
                {
                    out.push(sig.self_def);
                }
            }
        }
        out
    }

    fn finish_from_fix(&mut self) {
        if !self.report || self.diverged || self.fix_blocked || !self.suppress_1202 {
            return;
        }
        if self.cfg.key.body != BodyKey::Item {
            return;
        }
        let Some(sig) = self.sig.clone() else {
            return;
        };
        let Some(source) = self.source else {
            return;
        };
        let text = self.clause_text(&sig);
        let at = sig.error_span.map(|s| s.end).unwrap_or(sig.ret_span.end);
        self.fixes.push(FromFix {
            file: source.path.clone(),
            span: sig.ret_span,
            insert_at: at,
            text,
        });
    }

    fn clause_text(&self, sig: &FnSig) -> String {
        let mut parts = Vec::new();
        for (i, p) in sig.params.iter().enumerate() {
            let i = i as u32;
            let param =
                self.needs.contains(&Need::Param(i)) || self.needs.contains(&Need::Scope(i));
            let inner = self.needs.contains(&Need::Inner(i));
            if param {
                parts.push(p.name.clone());
            } else if inner {
                parts.push(format!("{}.*", p.name));
            }
        }
        if parts.is_empty() {
            " from static".to_string()
        } else {
            format!(" from {}", parts.join(", "))
        }
    }

    fn emit_1202(&mut self, missing: &[Need]) {
        let Some(sig) = self.sig.clone() else {
            return;
        };
        let mut parts = Vec::new();
        for (i, p) in sig.params.iter().enumerate() {
            let i = i as u32;
            let param = missing.contains(&Need::Param(i)) || missing.contains(&Need::Scope(i));
            let inner = missing.contains(&Need::Inner(i));
            if param {
                parts.push(p.name.clone());
            } else if inner {
                parts.push(format!("{}.*", p.name));
            }
        }
        let edits = if parts.is_empty() {
            Vec::new()
        } else {
            vec![Edit {
                file: self.file_name(),
                span: Span::empty(self.clause_end(&sig)),
                text: format!(", {}", parts.join(", ")),
            }]
        };
        let mut notes = Vec::new();
        if !sig.from.is_empty() {
            notes.push(Note {
                role: "signature",
                span: self.clause_span(&sig),
            });
        }
        self.error(
            "E1202",
            "the returned borrow is not covered by the `from` clause",
            notes,
            Some("add the missing origin".to_string()),
            edits,
        );
    }

    fn emit_1203(&mut self, loans: &[u32]) {
        let mut notes: Vec<Note> = loans
            .iter()
            .filter_map(|id| self.loans.get(*id as usize).map(|l| l.span))
            .map(|span| Note {
                role: "borrow",
                span,
            })
            .collect();
        notes.sort_by_key(|n| (n.span.start, n.span.end));
        notes.dedup_by(|a, b| a.span == b.span);
        self.error(
            "E1203",
            "the returned value borrows a local",
            notes,
            None,
            Vec::new(),
        );
    }

    fn emit_1204(&mut self, dest: u32, srcs: &BTreeSet<u32>) {
        if self.open_body() {
            let dst = self.param_name(dest);
            let mut notes = Vec::new();
            for src in srcs {
                if let Some(span) = self.param_name_span(*src) {
                    notes.push(Note {
                        role: "signature",
                        span,
                    });
                }
            }
            self.error(
                "E1204",
                format!("the view is stored where it reaches `{dst}`"),
                notes,
                None,
                Vec::new(),
            );
            return;
        }
        let dst = self.param_name(dest);
        let src_names: Vec<String> = srcs.iter().map(|s| self.param_name(*s)).collect();
        let message = if src_names.is_empty() {
            format!("a view is stored where it reaches `{dst}`")
        } else {
            format!(
                "the view from `{}` is stored where it reaches `{dst}`",
                src_names.join("`, `")
            )
        };
        let mut notes = Vec::new();
        for src in srcs {
            if let Some(span) = self.param_name_span(*src) {
                notes.push(Note {
                    role: "signature",
                    span,
                });
            }
        }
        let edits = self.edits_for(dest, srcs);
        let fix = if edits.is_empty() {
            None
        } else {
            Some(format!("declare `into {dst}`"))
        };
        self.error("E1204", message, notes, fix, edits);
    }

    fn emit_1205(&mut self, loans: &[u32]) {
        let mut notes: Vec<Note> = loans
            .iter()
            .filter_map(|id| self.loans.get(*id as usize).map(|l| l.span))
            .map(|span| Note {
                role: "borrow",
                span,
            })
            .collect();
        notes.sort_by_key(|n| (n.span.start, n.span.end));
        notes.dedup_by(|a, b| a.span == b.span);
        self.error(
            "E1205",
            "a borrow of a local is stored where it reaches a parameter",
            notes,
            None,
            Vec::new(),
        );
    }

    fn edits_for(&self, dest: u32, srcs: &BTreeSet<u32>) -> Vec<Edit> {
        if !self.fix_allowed(dest) {
            return Vec::new();
        }
        let Some(sig) = &self.sig else {
            return Vec::new();
        };
        let Some(dest_name) = sig.params.get(dest as usize).map(|p| p.name.clone()) else {
            return Vec::new();
        };
        let mut edits = Vec::new();
        for src in srcs {
            let Some(p) = sig.params.get(*src as usize) else {
                continue;
            };
            if p.into
                .as_ref()
                .is_some_and(|(_, idx)| *idx == Some(dest as usize))
            {
                continue;
            }
            edits.push(Edit {
                file: self.file_name(),
                span: Span::empty(p.ty_span.end),
                text: format!(" into {dest_name}"),
            });
        }
        edits
    }

    fn fix_allowed(&self, dest: u32) -> bool {
        let Some(ty) = self.param_ty(dest) else {
            return false;
        };
        // `into` is legal on `&mut`, `[]mut` and `&Scope` (E1206 rejects the rest).
        matches!(
            self.kind(ty),
            TyKind::Ref(Mutability::Mut, _) | TyKind::Slice(Mutability::Mut, _)
        ) || self.is_scope_ty(ty)
    }

    /// End of one `from` origin in the source.
    ///
    /// A parameter origin's span stops before `.*`; the two bytes are included when
    /// they follow. `static` ends at the keyword (the parser records that span).
    fn origin_end(&self, o: &OriginSig) -> usize {
        let bytes = self.source.map(|s| s.text.as_bytes()).unwrap_or(b"");
        let mut end = o.span.end;
        if bytes.get(end) == Some(&b'.') && bytes.get(end.saturating_add(1)) == Some(&b'*') {
            end = end.saturating_add(2);
        }
        end
    }

    fn clause_end(&self, sig: &FnSig) -> usize {
        let mut at = sig.ret_span.end;
        for o in &sig.from {
            at = at.max(self.origin_end(o));
        }
        at
    }

    fn clause_span(&self, sig: &FnSig) -> Span {
        let start = sig
            .from
            .iter()
            .map(|o| o.span.start)
            .min()
            .unwrap_or(sig.ret_span.start);
        Span::new(start, self.clause_end(sig))
    }

    fn error(
        &mut self,
        code: &'static str,
        message: impl Into<String>,
        notes: Vec<Note>,
        fix_message: Option<String>,
        edits: Vec<Edit>,
    ) {
        if !self.report || self.diverged {
            return;
        }
        let message = message.into();
        // Same span can hold two stores (`@swap`). Distinct messages both stay.
        let key = (
            code_id(code),
            self.span.start,
            self.span.end,
            message.clone(),
        );
        if !self.seen.insert(key) {
            return;
        }
        let (fix_message, edits) = if edits.is_empty() {
            (None, Vec::new())
        } else {
            (fix_message, edits)
        };
        self.diags.push(RawDiag {
            code,
            message,
            span: self.span,
            notes,
            fix_message,
            edits,
        });
    }

    fn diagnostics_of(&self) -> Vec<Diagnostic> {
        let Some(source) = self.source else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for d in &self.diags {
            let mut diag = Diagnostic::new(d.code, d.message.clone(), source, d.span);
            for n in &d.notes {
                diag.relate(n.role, n.span);
            }
            if let Some(message) = &d.fix_message {
                if d.edits.len() == 1 {
                    diag.fix(message.clone(), d.edits[0].clone());
                } else if !d.edits.is_empty() {
                    diag.fix_edits(message.clone(), d.edits.clone());
                }
            }
            out.push(diag);
        }
        out
    }

    fn file_name(&self) -> String {
        self.source.map(|s| s.path.clone()).unwrap_or_default()
    }

    /// `inner(S)` (checker.md §2). A loan contributes a read of its path: `O(y)`
    /// when the path has no dereference, and `inner` once per dereference otherwise.
    /// The same loan is not re-entered, so a loan stored in its own root cannot loop.
    fn inner(&self, set: &OriginSet, map: &OriginMap) -> OriginSet {
        self.inner_from(set, map, &mut BTreeSet::new())
    }

    fn inner_from(&self, set: &OriginSet, map: &OriginMap, stack: &mut BTreeSet<u32>) -> OriginSet {
        let mut out = OriginSet::new();
        for origin in set {
            match *origin {
                Origin::Loan(id) => out.extend(self.loan_read(id, map, stack)),
                Origin::Param(p) | Origin::Inner(p) => {
                    out.insert(Origin::Inner(p));
                }
                Origin::Scope(_) => {}
                // A capture is a label, not a borrow to look through.
                Origin::Capture(i) => {
                    out.insert(Origin::Capture(i));
                }
            }
        }
        out
    }

    fn loan_read(&self, id: u32, map: &OriginMap, stack: &mut BTreeSet<u32>) -> OriginSet {
        if !stack.insert(id) {
            return OriginSet::new();
        }
        let Some(loan) = self.loans.get(id as usize) else {
            stack.remove(&id);
            return OriginSet::new();
        };
        let derefs = deref_count(&loan.proj) as u32;
        let base = map.get(&loan.local).cloned().unwrap_or_default();
        let out = if derefs == 0 {
            base
        } else {
            let mut s = base;
            for _ in 0..derefs {
                s = self.inner_from(&s, map, stack);
            }
            s
        };
        stack.remove(&id);
        out
    }

    fn inner_n(&mut self, set: &OriginSet, map: &OriginMap, n: u32) -> OriginSet {
        let mut s = set.clone();
        for _ in 0..n {
            s = self.inner(&s, map);
        }
        s
    }

    fn inner_star(&mut self, set: &OriginSet, map: &OriginMap) -> OriginSet {
        let mut s = set.clone();
        let limit = self
            .loans
            .len()
            .saturating_add(self.cfg.params.len().saturating_mul(3))
            .saturating_add(1);
        for _ in 0..limit {
            let extra = self.inner(&s, map);
            let before = s.len();
            s.extend(extra);
            if s.len() == before {
                return s;
            }
        }
        self.block_reason("origin sets did not converge");
        s
    }

    fn mentions(&self, ty: TypeId, param: DefId) -> (bool, bool) {
        let mut seen = Vec::new();
        mention_rec(self, ty, param, false, &mut seen)
    }

    fn param_defs(&self, ty: TypeId) -> BTreeSet<DefId> {
        let mut out = BTreeSet::new();
        let mut seen = Vec::new();
        collect_params(self, ty, &mut out, &mut seen);
        out
    }

    fn param_ty(&self, index: u32) -> Option<TypeId> {
        if let Some(sig) = &self.sig
            && let Some(p) = sig.params.get(index as usize)
        {
            return Some(p.ty);
        }
        let local = self.cfg.params.get(index as usize).copied()?;
        self.cfg.locals.get(local as usize).map(|l| l.ty)
    }

    fn param_name(&self, index: u32) -> String {
        if let Some(sig) = &self.sig
            && let Some(p) = sig.params.get(index as usize)
        {
            return p.name.clone();
        }
        self.cfg
            .params
            .get(index as usize)
            .and_then(|l| self.cfg.locals.get(*l as usize))
            .and_then(|l| l.name.clone())
            .unwrap_or_else(|| format!("p{index}"))
    }

    fn param_name_span(&self, index: u32) -> Option<Span> {
        if let Some(sig) = &self.sig
            && let Some(p) = sig.params.get(index as usize)
        {
            return Some(p.name_span);
        }
        let local = self.cfg.params.get(index as usize).copied()?;
        self.cfg.locals.get(local as usize).map(|l| l.span)
    }

    fn carries(&mut self, ty: TypeId) -> bool {
        if self.decls.types.has_poison(ty) {
            return false;
        }
        let p = self.decls.props(ty);
        p.view == Tri::Yes || !p.maybe_view.is_no()
    }

    fn carries_local(&mut self, local: Local) -> bool {
        let Some(ty) = self.local_ty(local) else {
            return false;
        };
        self.carries(ty)
    }

    fn is_copy_view(&mut self, ty: TypeId) -> bool {
        if self.decls.types.has_poison(ty) {
            return false;
        }
        let p = self.decls.props(ty);
        p.copy == Tri::Yes && (p.view == Tri::Yes || !p.maybe_view.is_no())
    }

    fn is_scope_ty(&self, ty: TypeId) -> bool {
        if self.is_scope_adt(ty) {
            return true;
        }
        if let TyKind::Ref(_, inner) = self.kind(ty) {
            return self.is_scope_adt(inner);
        }
        false
    }

    fn is_scope_adt(&self, ty: TypeId) -> bool {
        let TyKind::Adt(def, _) = self.kind(ty) else {
            return false;
        };
        self.sema.qualified_name(def) == "std.task.Scope"
    }

    /// Passed by `&mut` or `[]mut` (checker.md §4.5 step 2).
    fn is_mut_ref(&self, ty: TypeId) -> bool {
        matches!(
            self.kind(ty),
            TyKind::Ref(Mutability::Mut, _) | TyKind::Slice(Mutability::Mut, _)
        )
    }

    /// The first field of the capture record: `self.f` or `self.*.f`.
    fn at_capture(&self, c: &Cursor) -> bool {
        let Some(rec) = self.cfg.closure_ty else {
            return false;
        };
        let Some(root) = c.root else {
            return false;
        };
        if self.cfg.params.first().copied() != Some(root) {
            return false;
        }
        let record =
            c.ty == rec || matches!(self.kind(c.ty), TyKind::Ref(_, inner) if inner == rec);
        record
            && c.steps
                .iter()
                .all(|s| matches!(s, LoanStep::Deref | LoanStep::Raw))
    }

    /// One projection, as a loan step. An index or slice element of a slice is a
    /// dereference; an index of an array is not (checker.md §2).
    fn loan_step(&self, ty: TypeId, proj: &Projection) -> LoanStep {
        match proj {
            Projection::Field { index, .. } => LoanStep::Field(*index as u32),
            Projection::TupleField(i) => LoanStep::Tuple(*i as u32),
            Projection::Index(_) if matches!(self.kind(ty), TyKind::Slice(..)) => LoanStep::Deref,
            Projection::Index(_) => LoanStep::Elem,
            Projection::Deref => LoanStep::Deref,
            Projection::DerefRaw => LoanStep::Raw,
            Projection::Payload { variant } => LoanStep::Payload(*variant as u32),
        }
    }

    fn loan_steps_of(&mut self, place: &Place) -> Vec<LoanStep> {
        let mut steps = Vec::new();
        let mut ty = match &place.base {
            PlaceBase::Local(l) => self.local_ty(*l),
            PlaceBase::Static(d) => self.static_ty(*d),
        };
        for proj in &place.proj {
            let step = match ty {
                Some(t) => self.loan_step(t, proj),
                None => self.loan_step(self.decls.types.poison, proj),
            };
            steps.push(step);
            ty = ty.and_then(|t| self.proj_ty(t, proj));
        }
        steps
    }

    fn shared_borrow(&self, ty: TypeId) -> bool {
        matches!(
            self.kind(ty),
            TyKind::Ref(Mutability::Shared, _) | TyKind::Slice(Mutability::Shared, _)
        )
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

fn mention_rec(
    cx: &Cx<'_>,
    ty: TypeId,
    param: DefId,
    inside: bool,
    seen: &mut Vec<TypeId>,
) -> (bool, bool) {
    if seen.contains(&ty) {
        return (false, false);
    }
    seen.push(ty);
    let kind = cx.kind(ty);
    match kind {
        TyKind::Param(p) if p == param => {
            if inside {
                (false, true)
            } else {
                (true, false)
            }
        }
        TyKind::Param(_) => (false, false),
        TyKind::Ref(_, t) | TyKind::Slice(_, t) => mention_rec(cx, t, param, true, seen),
        TyKind::RawPtr(_, t)
        | TyKind::Optional(t)
        | TyKind::Array(t, _)
        | TyKind::Projection(t, _) => mention_rec(cx, t, param, inside, seen),
        TyKind::Tuple(ts) => fold_mentions(cx, &ts, param, inside, seen),
        TyKind::Result(a, b) => {
            let l = mention_rec(cx, a, param, inside, seen);
            let r = mention_rec(cx, b, param, inside, seen);
            (l.0 || r.0, l.1 || r.1)
        }
        TyKind::Adt(_, args) => mention_args(cx, &args, param, inside, seen),
        TyKind::Dyn(_, _, args) => mention_args(cx, &args, param, true, seen),
        TyKind::FnPtr(sig) => mention_sig(cx, &sig.params, sig.ret, sig.error, param, inside, seen),
        TyKind::DynFn(_, _, sig) => {
            mention_sig(cx, &sig.params, sig.ret, sig.error, param, true, seen)
        }
        _ => (false, false),
    }
}

fn mention_args(
    cx: &Cx<'_>,
    args: &[GenericArg],
    param: DefId,
    inside: bool,
    seen: &mut Vec<TypeId>,
) -> (bool, bool) {
    let mut out = (false, false);
    for a in args {
        if let GenericArg::Ty(t) = a {
            let m = mention_rec(cx, *t, param, inside, seen);
            out = (out.0 || m.0, out.1 || m.1);
        }
    }
    out
}

fn mention_sig(
    cx: &Cx<'_>,
    params: &[TypeId],
    ret: TypeId,
    error: Option<TypeId>,
    param: DefId,
    inside: bool,
    seen: &mut Vec<TypeId>,
) -> (bool, bool) {
    let mut ts = params.to_vec();
    ts.push(ret);
    if let Some(e) = error {
        ts.push(e);
    }
    fold_mentions(cx, &ts, param, inside, seen)
}

fn fold_mentions(
    cx: &Cx<'_>,
    ts: &[TypeId],
    param: DefId,
    inside: bool,
    seen: &mut Vec<TypeId>,
) -> (bool, bool) {
    let mut out = (false, false);
    for t in ts {
        let m = mention_rec(cx, *t, param, inside, seen);
        out = (out.0 || m.0, out.1 || m.1);
    }
    out
}

fn mentions_any(m: &(bool, bool)) -> bool {
    m.0 || m.1
}

fn collect_params(cx: &Cx<'_>, ty: TypeId, out: &mut BTreeSet<DefId>, seen: &mut Vec<TypeId>) {
    if seen.contains(&ty) {
        return;
    }
    seen.push(ty);
    match cx.kind(ty) {
        TyKind::Param(p) => {
            out.insert(p);
        }
        _ => {
            for c in cx.decls.types.children(ty) {
                collect_params(cx, c, out, seen);
            }
        }
    }
}

fn share_params(a: &BTreeSet<DefId>, b: &BTreeSet<DefId>) -> bool {
    a.iter().any(|p| b.contains(p))
}

fn is_borrow_ty(kind: &TyKind) -> bool {
    matches!(
        kind,
        TyKind::Ref(..) | TyKind::Slice(..) | TyKind::Dyn(..) | TyKind::DynFn(..)
    )
}

fn deref_count(steps: &[LoanStep]) -> usize {
    steps
        .iter()
        .filter(|s| matches!(s, LoanStep::Deref | LoanStep::Raw))
        .count()
}

fn after_first_deref(steps: &[LoanStep]) -> Vec<LoanStep> {
    let mut seen = false;
    let mut rest = Vec::new();
    for s in steps {
        if !seen && matches!(s, LoanStep::Deref | LoanStep::Raw) {
            seen = true;
            continue;
        }
        if seen {
            rest.push(s.clone());
        }
    }
    rest
}

fn strip(set: &mut OriginSet, killed: &BTreeSet<u32>) {
    if killed.is_empty() {
        return;
    }
    set.retain(|o| match o {
        Origin::Loan(id) => !killed.contains(id),
        _ => true,
    });
}

fn strong_update(flow: &mut OriginMap, local: Local, set: &OriginSet) {
    if set.is_empty() {
        flow.remove(&local);
    } else {
        flow.insert(local, set.clone());
    }
}

fn union_into(flow: &mut OriginMap, local: Local, set: &OriginSet) {
    if set.is_empty() {
        return;
    }
    flow.entry(local).or_default().extend(set.iter().copied());
}

fn union2(a: &OriginSet, b: &OriginSet) -> OriginSet {
    let mut s = a.clone();
    s.extend(b.iter().copied());
    s
}

fn union_all(sets: &[OriginSet]) -> OriginSet {
    let mut s = OriginSet::new();
    for a in sets {
        s.extend(a.iter().copied());
    }
    s
}

fn merge(dst: &mut OriginMap, src: &OriginMap) -> bool {
    let mut changed = false;
    for (k, v) in src {
        if v.is_empty() {
            continue;
        }
        let e = dst.entry(*k).or_default();
        let before = e.len();
        e.extend(v.iter().copied());
        if e.len() != before {
            changed = true;
        }
    }
    changed
}

fn code_id(code: &str) -> u16 {
    match code {
        "E1202" => 1202,
        "E1203" => 1203,
        "E1204" => 1204,
        "E1205" => 1205,
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::{Cursor, Cx, LoanStep, Origin, OriginSet};
    use crate::model::{BodyKey, Cfg, CfgKey};
    use ingot_sema::defs::{DefId, Sema};
    use ingot_sema::sigs::Declarations;
    use ingot_sema::ty::{Mutability, TypeId};

    fn cursor(steps: Vec<LoanStep>, ty: TypeId) -> Cursor {
        Cursor {
            set: OriginSet::new(),
            derefs: 0,
            exclusive: false,
            ty,
            steps,
            root: Some(0),
            static_root: false,
            raw: false,
        }
    }

    #[test]
    fn two_borrows_in_one_statement_get_distinct_ids() {
        let sema = Sema::new();
        let mut decls = Declarations::new();
        let poison = decls.types.poison;
        let cfg = Cfg {
            key: CfgKey {
                owner: DefId(0),
                body: BodyKey::Item,
            },
            name: "t".to_string(),
            file: 0,
            ret: poison,
            ret_name: "void".to_string(),
            error: None,
            error_name: None,
            params: Vec::new(),
            locals: Vec::new(),
            blocks: Vec::new(),
            entry: 0,
            closure_ty: None,
            closure_fields: Vec::new(),
        };
        let mut cx = Cx::new(&sema, &mut decls, &cfg, None, Vec::new());
        cx.block_id = 3;
        cx.stmt_index = 1;
        let first = cx.make_loan(&cursor(Vec::new(), poison), Mutability::Shared, false);
        let second = cx.make_loan(
            &cursor(vec![LoanStep::Deref], poison),
            Mutability::Mut,
            false,
        );
        match (first, second) {
            (Origin::Loan(a), Origin::Loan(b)) => assert_ne!(a, b),
            _ => panic!("loans"),
        }
        assert_eq!(cx.loans.len(), 2);
        assert_eq!(cx.loans[0].seq, 0);
        assert_eq!(cx.loans[1].seq, 1);
        assert_eq!(cx.loans[0].stmt, 1);
        assert_eq!(cx.loans[1].stmt, 1);
        // A replay of the same statement hands back the same ids.
        cx.loan_seq = 0;
        let again = cx.make_loan(&cursor(Vec::new(), poison), Mutability::Shared, false);
        assert_eq!(again, first);
        assert_eq!(cx.loans.len(), 2);
    }
}
