//! Analysis A: moves and linear values (checker.md §3, memory.md §10–§11).
//!
//! One forward dataflow per CFG. A path is a local plus projections; every array or
//! slice element is the one step `[_]`. Absence of a move fact means *init*, except
//! where a prefix fact says otherwise. An enum path carries the variants it may hold
//! (absence means every variant; the join of absence with anything is every variant)
//! and, separately, the set of variants whose payloads are still linear obligations.
//! That set is what storage-end reads: joining "payload moved" with "variant absent"
//! must not become "payload maybe and variant possible", which would report E1003 for
//! a `match` or `try` that consumed the value on every arm.
//!
//! A consuming `for` over an array reads each element with `PlaceUse::ForElem`. That
//! read is not E1002 and does not change move state: every element is the one path
//! `[_]`, so tracking one of them would consume them all and hide an early `break`.
//! The loop's normal exit is `IterConsumed`, which moves the array temporary. A
//! `break` jumps past it. Every other element move is E1002.
//!
//! Loop labels are not part of the move lattice. A back edge records the move spans
//! it carries in `Flow::loops`, which join only adds. At a report, a span is a `loop`
//! label when it reached the use only that way. `join` never drops a site.

use crate::model::*;
use ingot_diag::diagnostic::{Diagnostic, Edit};
use ingot_diag::source::{Source, Sources, Span};
use ingot_sema::defs::{DefId, ModuleId, Sema};
use ingot_sema::sigs::{Declarations, Tri};
use ingot_sema::ty::{TyKind, TypeId};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

pub(super) fn check_cfg(
    sema: &Sema,
    decls: &mut Declarations,
    sources: &Sources,
    cfg: &Cfg,
) -> Vec<Diagnostic> {
    let Some(source) = sources.files.get(cfg.file) else {
        return Vec::new();
    };
    let owner_module = sema.def(cfg.key.owner).module;
    let mut cx = Cx {
        sema,
        decls,
        cfg,
        owner_module,
        report: false,
        diags: Vec::new(),
        seen: BTreeSet::new(),
        leaks: BTreeMap::new(),
    };
    let entry = cx.fixpoint();
    cx.report = true;
    cx.seen.clear();
    let n = cx.cfg.blocks.len();
    for b in 0..n {
        let Some(start) = entry.get(b).and_then(|s| s.clone()) else {
            continue;
        };
        let mut flow = start;
        cx.transfer_block(&mut flow, b);
    }
    cx.emit_leaks(source);
    cx.into_diagnostics(source)
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Step {
    Field(u32),
    Tuple(u32),
    Elem,
    Deref,
    Raw,
    Payload(u32),
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
struct PathKey {
    local: Local,
    proj: Vec<Step>,
}

impl PathKey {
    fn local(local: Local) -> PathKey {
        PathKey {
            local,
            proj: Vec::new(),
        }
    }

    fn parent(&self) -> Option<PathKey> {
        if self.proj.is_empty() {
            None
        } else {
            let mut p = self.clone();
            p.proj.pop();
            Some(p)
        }
    }

    fn push(&self, step: Step) -> PathKey {
        let mut p = self.clone();
        p.proj.push(step);
        p
    }
}

fn under(parent: &PathKey, child: &PathKey) -> bool {
    child.local == parent.local
        && child.proj.len() > parent.proj.len()
        && child.proj.starts_with(&parent.proj)
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum St {
    Init,
    Moved,
    Maybe,
}

fn join_st(a: St, b: St) -> St {
    match (a, b) {
        (St::Init, St::Init) => St::Init,
        (St::Moved, St::Moved) => St::Moved,
        _ => St::Maybe,
    }
}

/// A move span: `(start, end)`.
type MoveSpan = (usize, usize);
/// The loop a move span crossed: `(start, end)` of the loop.
type LoopSpan = (usize, usize);

#[derive(Clone, PartialEq, Eq)]
struct Fact {
    st: St,
    /// Move spans that reach this path without having crossed a back edge since the
    /// move. Join only adds.
    sites: BTreeSet<MoveSpan>,
}

#[derive(Clone, PartialEq, Eq, Default)]
struct Flow {
    facts: BTreeMap<PathKey, Fact>,
    /// Absent means every variant of the enum.
    vars: BTreeMap<PathKey, BTreeSet<u32>>,
    /// Variants whose payloads are still linear obligations. Absent means none.
    live: BTreeMap<PathKey, BTreeSet<u32>>,
    poisoned: BTreeSet<PathKey>,
    /// Move spans that reach this path only across a back edge, with the loop they
    /// crossed. Join only adds. Read at report time; not a reason to drop `sites`.
    loops: BTreeMap<PathKey, BTreeMap<MoveSpan, LoopSpan>>,
}

impl Flow {
    fn effective(&self, path: &PathKey) -> (St, BTreeSet<MoveSpan>) {
        if let Some(f) = self.facts.get(path) {
            return (f.st, f.sites.clone());
        }
        let mut p = path.clone();
        while p.proj.pop().is_some() {
            if let Some(f) = self.facts.get(&p) {
                return (f.st, f.sites.clone());
            }
        }
        (St::Init, BTreeSet::new())
    }

    /// Loop labels for `path`. An entry on this path wins. A fact with no entry
    /// means the move arrived directly. Otherwise inherit from the prefix fact.
    fn loops_at(&self, path: &PathKey) -> BTreeMap<MoveSpan, LoopSpan> {
        if let Some(m) = self.loops.get(path)
            && !m.is_empty()
        {
            return m.clone();
        }
        if self.facts.contains_key(path) {
            return BTreeMap::new();
        }
        let mut p = path.clone();
        while p.proj.pop().is_some() {
            if let Some(m) = self.loops.get(&p)
                && !m.is_empty()
            {
                return m.clone();
            }
            if self.facts.contains_key(&p) {
                return BTreeMap::new();
            }
        }
        BTreeMap::new()
    }

    fn poisoned_covering(&self, path: &PathKey) -> bool {
        if self.poisoned.contains(path) {
            return true;
        }
        let mut p = path.clone();
        while p.proj.pop().is_some() {
            if self.poisoned.contains(&p) {
                return true;
            }
        }
        false
    }
}

fn remove_under(flow: &mut Flow, path: &PathKey) {
    flow.facts.retain(|k, _| !under(path, k));
    flow.vars.retain(|k, _| !under(path, k));
    flow.live.retain(|k, _| !under(path, k));
    flow.poisoned.retain(|k| !under(path, k));
    flow.loops.retain(|k, _| !under(path, k));
}

#[derive(Clone)]
struct Brought {
    vars: Option<BTreeSet<u32>>,
    live: BTreeSet<u32>,
}

struct Note {
    role: &'static str,
    span: Span,
}

struct Leak {
    ty: TypeId,
    kind: LocalKind,
    name: Option<String>,
    decl: Span,
    exits: BTreeSet<(usize, usize)>,
}

struct RawDiag {
    code: &'static str,
    message: String,
    span: Span,
    notes: Vec<Note>,
    fix_message: Option<String>,
    edit: Option<Edit>,
}

struct Cx<'a> {
    sema: &'a Sema,
    decls: &'a mut Declarations,
    cfg: &'a Cfg,
    owner_module: ModuleId,
    report: bool,
    diags: Vec<RawDiag>,
    /// Identical `(code, span)` pairs. One local can report the same code twice.
    seen: BTreeSet<(u16, usize, usize)>,
    leaks: BTreeMap<(usize, usize), Leak>,
}

fn code_id(code: &str) -> u16 {
    match code {
        "E1001" => 1,
        "E1002" => 2,
        "E1003" => 3,
        "E1004" => 4,
        "E1005" => 5,
        "E1006" => 6,
        _ => 0,
    }
}

impl<'a> Cx<'a> {
    fn fixpoint(&mut self) -> Vec<Option<Flow>> {
        let n = self.cfg.blocks.len();
        let mut entry = vec![None; n];
        if n == 0 {
            return entry;
        }
        let start = self.cfg.entry as usize;
        if start >= n {
            return entry;
        }
        let back = back_edges(self.cfg);
        entry[start] = Some(Flow::default());
        let mut queue = VecDeque::new();
        let mut queued = BTreeSet::new();
        queue.push_back(start);
        queued.insert(start);
        // Terminates. Each path is a local plus projections that occur in this CFG.
        // Each fact is one of three states. Site sets are drawn from the move spans
        // in the body, variant sets from the type's variants, live sets the same.
        // `join` only adds sites, loop records, variants, live slots and poison
        // marks, and a state's join only moves toward *maybe*. A conflicting use
        // resets that path to *init* on the transfer; that reset is not a join, and
        // it cannot shrink a block's stored entry. The worklist stops when no entry
        // grows.
        while let Some(b) = queue.pop_front() {
            queued.remove(&b);
            let Some(start_state) = entry[b].clone() else {
                continue;
            };
            let mut flow = start_state;
            self.transfer_block(&mut flow, b);
            let term = self.cfg.blocks[b].term.clone();
            let term_span = self.cfg.blocks[b].term_span;
            for (succ, mut inc) in self.successor_states(&flow, &term) {
                let succ_i = succ as usize;
                if succ_i >= n {
                    continue;
                }
                if back.contains(&(b as u32, succ)) {
                    mark_loop(&mut inc, term_span);
                }
                if join_into(&mut entry[succ_i], inc) && queued.insert(succ_i) {
                    queue.push_back(succ_i);
                }
            }
        }
        entry
    }

    fn transfer_block(&mut self, flow: &mut Flow, block: usize) {
        let stmts = self.cfg.blocks[block].stmts.clone();
        let n = stmts.len();
        for (i, stmt) in stmts.iter().enumerate() {
            self.transfer_stmt(flow, block, i, stmt);
        }
        let term = self.cfg.blocks[block].term.clone();
        let term_span = self.cfg.blocks[block].term_span;
        self.transfer_term(flow, block, &term, term_span, n);
    }

    fn transfer_stmt(&mut self, flow: &mut Flow, block: usize, index: usize, stmt: &Stmt) {
        let span = stmt.span;
        let defer = stmt.defer;
        match &stmt.kind {
            StmtKind::StorageStart(l) => self.storage_start(flow, *l),
            StmtKind::StorageEnd(l) => self.end_storage(flow, *l, block, index),
            StmtKind::Assign { place, value } => {
                self.assign(flow, place, value, span, stmt.read_span, defer)
            }
            StmtKind::Use(op) => self.use_operand(flow, op, span, defer),
            StmtKind::Cancel(_) | StmtKind::EnterUnsafe | StmtKind::Check(_) => {}
            StmtKind::Abandon(l) => self.mark_gone(flow, &PathKey::local(*l)),
            StmtKind::Variant {
                place,
                variant,
                holds,
                ..
            } => self.narrow(flow, place, *variant, *holds),
            StmtKind::IterConsumed(place) => {
                if let Some(path) = path_key(place) {
                    self.mark_moved(flow, &path, span);
                }
            }
        }
    }

    fn transfer_term(
        &mut self,
        flow: &mut Flow,
        block: usize,
        term: &Terminator,
        span: Span,
        stmt_len: usize,
    ) {
        let defer = None;
        match term {
            Terminator::Goto(_) | Terminator::Unreachable | Terminator::ScopeWait { .. } => {}
            Terminator::Branch { cond, .. } => self.use_operand(flow, cond, span, defer),
            Terminator::Switch { value, .. } => self.use_operand(flow, value, span, defer),
            Terminator::Return { value, end, .. } => {
                if let Some(v) = value {
                    self.use_operand(flow, v, span, defer);
                }
                if let Some(l) = end {
                    self.end_storage(flow, *l, block, stmt_len);
                }
            }
            Terminator::Call {
                target, args, dest, ..
            } => {
                self.use_target(flow, target, span);
                for a in args {
                    self.use_operand(flow, a, span, defer);
                }
                if let Some(d) = dest {
                    self.init_result(flow, d, span);
                }
            }
            Terminator::Trap { operands, .. } => {
                for o in operands {
                    self.use_operand(flow, o, span, defer);
                }
            }
            Terminator::Spawn {
                target,
                args,
                scope,
                dest,
                ..
            } => {
                self.use_target(flow, target, span);
                for a in args {
                    self.use_operand(flow, a, span, defer);
                }
                self.use_operand(flow, scope, span, defer);
                if let Some(d) = dest {
                    self.init_result(flow, d, span);
                }
            }
            Terminator::ParFor {
                captures,
                iterand,
                grain,
                ..
            } => {
                for c in captures {
                    self.use_operand(flow, c, span, defer);
                }
                match iterand {
                    ParIterand::Value(op) => self.use_operand(flow, op, span, defer),
                    ParIterand::Range { lo, hi, step, .. } => {
                        self.use_operand(flow, lo, span, defer);
                        self.use_operand(flow, hi, span, defer);
                        if let Some(s) = step {
                            self.use_operand(flow, s, span, defer);
                        }
                    }
                }
                if let Some(g) = grain {
                    self.use_operand(flow, g, span, defer);
                }
            }
            Terminator::Dispatch {
                executor,
                lo,
                hi,
                args,
                dest,
                ..
            } => {
                self.use_operand(flow, executor, span, defer);
                self.use_operand(flow, lo, span, defer);
                self.use_operand(flow, hi, span, defer);
                for a in args {
                    self.use_operand(flow, &a.value, span, defer);
                }
                if let Some(d) = dest {
                    self.init_result(flow, d, span);
                }
            }
            Terminator::Select { ops } => {
                for op in ops {
                    let Some(call) = &op.op else {
                        continue;
                    };
                    self.use_operand(
                        flow,
                        &Operand::Place(call.value.clone(), PlaceUse::Move),
                        op.span,
                        defer,
                    );
                }
            }
        }
    }

    /// Select writes each arm's result only on that arm's edge. Other terminators
    /// share one end state.
    fn successor_states(&mut self, flow: &Flow, term: &Terminator) -> Vec<(Block, Flow)> {
        match term {
            Terminator::Select { ops } => ops
                .iter()
                .map(|op| {
                    let mut s = flow.clone();
                    if let Some(call) = &op.op {
                        self.init_result(&mut s, &call.result, op.span);
                    }
                    (op.successor, s)
                })
                .collect(),
            other => successors(other)
                .into_iter()
                .map(|b| (b, flow.clone()))
                .collect(),
        }
    }

    fn storage_start(&mut self, flow: &mut Flow, local: Local) {
        let path = PathKey::local(local);
        let Some(loc) = self.cfg.locals.get(local as usize) else {
            return;
        };
        let kind = loc.kind;
        let ty = loc.ty;
        remove_under(flow, &path);
        flow.facts.remove(&path);
        flow.vars.remove(&path);
        flow.live.remove(&path);
        flow.poisoned.remove(&path);
        flow.loops.remove(&path);
        // A parameter arrives holding its argument. A `scope` local arrives holding
        // `&Scope`. Every other storage start is still empty.
        if kind == LocalKind::Param || kind == LocalKind::Scope {
            let brought = self.fresh(ty);
            self.mark_init(flow, &path, brought);
        } else {
            flow.facts.insert(
                path,
                Fact {
                    st: St::Moved,
                    sites: BTreeSet::new(),
                },
            );
        }
    }

    fn end_storage(&mut self, flow: &mut Flow, local: Local, block: usize, index: usize) {
        let path = PathKey::local(local);
        if self.report && self.obligation_local(flow, local) {
            let exits = self.exit_spans(block, index);
            let primaries = self.leak_primaries(flow, local);
            for (span, ty, kind, name, decl) in primaries {
                let rec = self
                    .leaks
                    .entry((span.start, span.end))
                    .or_insert_with(|| Leak {
                        ty,
                        kind,
                        name,
                        decl,
                        exits: BTreeSet::new(),
                    });
                for s in &exits {
                    rec.exits.insert((s.start, s.end));
                }
            }
        }
        self.mark_gone(flow, &path);
    }

    /// Where an unconsumed value is reported. A split closure record reports each
    /// capture that is still an obligation, at that capture's name in the capture
    /// list. Anything else reports once, at the local.
    fn leak_primaries(
        &mut self,
        flow: &Flow,
        local: Local,
    ) -> Vec<(Span, TypeId, LocalKind, Option<String>, Span)> {
        let Some(loc) = self.cfg.locals.get(local as usize) else {
            return Vec::new();
        };
        let loc_ty = loc.ty;
        let loc_span = loc.span;
        let loc_kind = loc.kind;
        let loc_name = loc.name.clone();
        let loc_decl = loc.decl_span;
        if self.cfg.closure_ty == Some(loc_ty) && !self.cfg.closure_fields.is_empty() {
            let fields: Vec<(TypeId, Span, String)> = self
                .cfg
                .closure_fields
                .iter()
                .map(|f| (f.ty, f.span, f.name.clone()))
                .collect();
            let path = PathKey::local(local);
            let mut out = Vec::new();
            for (i, (fty, span, name)) in fields.into_iter().enumerate() {
                if self.obligation(flow, &path.push(Step::Field(i as u32)), fty, 1) {
                    out.push((span, fty, LocalKind::Param, Some(name), span));
                }
            }
            if !out.is_empty() {
                return out;
            }
        }
        vec![(loc_span, loc_ty, loc_kind, loc_name, loc_decl)]
    }

    fn obligation_local(&mut self, flow: &Flow, local: Local) -> bool {
        let Some(ty) = self.local_ty(local) else {
            return false;
        };
        self.obligation(flow, &PathKey::local(local), ty, 0)
    }

    fn assign(
        &mut self,
        flow: &mut Flow,
        place: &Place,
        value: &Rvalue,
        span: Span,
        read_span: Option<Span>,
        defer: Option<DeferCopy>,
    ) {
        self.use_indices(flow, place, span, defer);
        if let Rvalue::Builtin { name, args, .. } = value {
            if name == "replace" {
                self.assign_replace(flow, place, args, span, defer);
                return;
            }
            if name == "swap" {
                self.assign_swap(flow, place, args, span, defer);
                return;
            }
        }
        let dest_ty = self.place_ty(place);
        let brought = match dest_ty {
            Some(ty) => self.brought(flow, value, ty),
            None => Brought {
                vars: None,
                live: BTreeSet::new(),
            },
        };
        // The overwrite and the move of the destination stay on the statement. A
        // copied place read is reported at the place, when lowering recorded one.
        self.use_rvalue(flow, value, read_span.unwrap_or(span), defer);
        if let Some(path) = path_key(place) {
            if let Some(ty) = dest_ty
                && self.obligation(flow, &path, ty, 0)
            {
                self.error(
                    flow,
                    "E1004",
                    "assignment overwrites a linear value",
                    span,
                    &path,
                    Vec::new(),
                    None,
                );
            }
            self.mark_init(flow, &path, brought);
        }
    }

    fn assign_replace(
        &mut self,
        flow: &mut Flow,
        dest: &Place,
        args: &[BuiltinArg],
        span: Span,
        defer: Option<DeferCopy>,
    ) {
        let Some(BuiltinArg::Place(slot)) = args.first() else {
            return;
        };
        let new_val = args.get(1);
        // A slot behind a dereference is not a path this function owns, so the
        // value that comes out is a fresh value of the slot's type (any variant).
        let old = self.captured_slot(flow, slot);
        let new_brought = match new_val {
            Some(BuiltinArg::Value(op)) => self.brought_operand(flow, op),
            _ => Brought {
                vars: None,
                live: BTreeSet::new(),
            },
        };
        // The place is read, then overwritten. Not a move, and not E1004.
        self.touch(flow, slot, span, defer, Touch::Read { whole: true });
        if let Some(BuiltinArg::Value(op)) = new_val {
            self.use_operand(flow, op, span, defer);
        }
        if let Some(path) = path_key(dest) {
            let ty = self.place_ty(dest);
            if let Some(ty) = ty
                && self.obligation(flow, &path, ty, 0)
            {
                self.error(
                    flow,
                    "E1004",
                    "assignment overwrites a linear value",
                    span,
                    &path,
                    Vec::new(),
                    None,
                );
            }
            self.mark_init(flow, &path, old);
        }
        if let Some(path) = path_key(slot) {
            self.mark_init(flow, &path, new_brought);
        }
    }

    fn assign_swap(
        &mut self,
        flow: &mut Flow,
        dest: &Place,
        args: &[BuiltinArg],
        span: Span,
        defer: Option<DeferCopy>,
    ) {
        let Some(BuiltinArg::Place(a)) = args.first() else {
            return;
        };
        let Some(BuiltinArg::Place(b)) = args.get(1) else {
            return;
        };
        let ba = self.captured_slot(flow, a);
        let bb = self.captured_slot(flow, b);
        self.touch(flow, a, span, defer, Touch::Read { whole: true });
        self.touch(flow, b, span, defer, Touch::Read { whole: true });
        if let Some(path) = path_key(dest) {
            self.mark_init(flow, &path, self.fresh_empty());
        }
        if let Some(path) = path_key(a) {
            self.mark_init(flow, &path, bb);
        }
        if let Some(path) = path_key(b) {
            self.mark_init(flow, &path, ba);
        }
    }

    fn init_result(&mut self, flow: &mut Flow, place: &Place, span: Span) {
        self.use_indices(flow, place, span, None);
        let Some(path) = path_key(place) else {
            return;
        };
        let brought = match self.place_ty(place) {
            Some(ty) => {
                if self.obligation(flow, &path, ty, 0) {
                    self.error(
                        flow,
                        "E1004",
                        "assignment overwrites a linear value",
                        span,
                        &path,
                        Vec::new(),
                        None,
                    );
                }
                self.fresh(ty)
            }
            None => self.fresh_empty(),
        };
        self.mark_init(flow, &path, brought);
    }

    fn use_rvalue(
        &mut self,
        flow: &mut Flow,
        value: &Rvalue,
        span: Span,
        defer: Option<DeferCopy>,
    ) {
        match value {
            Rvalue::Use(op)
            | Rvalue::Unary { operand: op, .. }
            | Rvalue::Cast { operand: op, .. }
            | Rvalue::DynCast { operand: op, .. } => self.use_operand(flow, op, span, defer),
            Rvalue::Binary { lhs, rhs, .. } => {
                self.use_operand(flow, lhs, span, defer);
                self.use_operand(flow, rhs, span, defer);
            }
            Rvalue::Borrow { place, .. } | Rvalue::Reborrow { place, .. } => {
                self.touch(flow, place, span, defer, Touch::Read { whole: true });
            }
            Rvalue::Aggregate { fields, .. } => {
                for f in fields {
                    self.use_operand(flow, f, span, defer);
                }
            }
            Rvalue::Discriminant(p) | Rvalue::SliceLen(p) | Rvalue::SlicePtr(p) => {
                self.touch(flow, p, span, defer, Touch::Read { whole: false });
            }
            Rvalue::Slice { base, lo, hi, .. } => {
                self.touch(flow, base, span, defer, Touch::Read { whole: false });
                self.use_operand(flow, lo, span, defer);
                self.use_operand(flow, hi, span, defer);
            }
            Rvalue::SliceElem { base, index } => {
                self.touch(flow, base, span, defer, Touch::Read { whole: false });
                self.use_operand(flow, index, span, defer);
            }
            Rvalue::Builtin { args, .. } => {
                for a in args {
                    match a {
                        BuiltinArg::Value(op) => self.use_operand(flow, op, span, defer),
                        BuiltinArg::Place(p) => {
                            self.touch(flow, p, span, defer, Touch::Read { whole: true });
                        }
                    }
                }
            }
        }
    }

    fn use_operand(&mut self, flow: &mut Flow, op: &Operand, span: Span, defer: Option<DeferCopy>) {
        let Operand::Place(p, use_) = op else {
            return;
        };
        let touch = match use_ {
            PlaceUse::Copy => Touch::Read { whole: true },
            PlaceUse::Move => Touch::Move,
            // The element step of a consuming `for`. The array is consumed by
            // `IterConsumed` on the normal exit, not by this read.
            PlaceUse::ForElem => Touch::ForElem,
        };
        self.touch(flow, p, span, defer, touch);
    }

    fn use_target(&mut self, flow: &mut Flow, target: &CallTarget, span: Span) {
        if let CallTarget::Value { callee, .. } = target {
            self.use_operand(flow, callee, span, None);
        }
    }

    fn use_indices(
        &mut self,
        flow: &mut Flow,
        place: &Place,
        span: Span,
        defer: Option<DeferCopy>,
    ) {
        for proj in &place.proj {
            if let Projection::Index(op) = proj {
                self.use_operand(flow, op, span, defer);
            }
        }
    }

    fn touch(
        &mut self,
        flow: &mut Flow,
        place: &Place,
        span: Span,
        defer: Option<DeferCopy>,
        kind: Touch,
    ) {
        self.use_indices(flow, place, span, defer);
        if self.payload_absent(flow, place) {
            return;
        }
        match kind {
            Touch::Read { whole } => {
                if let Some(path) = path_key(place)
                    && let Some(conflict) = self.conflict(flow, &path, whole)
                {
                    self.report_conflict(flow, &path, span, defer, &conflict);
                    self.poison(flow, &path);
                }
            }
            Touch::ForElem => {
                if let Some(path) = path_key(place)
                    && let Some(conflict) = self.conflict(flow, &path, true)
                {
                    self.report_conflict(flow, &path, span, defer, &conflict);
                    self.poison(flow, &path);
                }
            }
            Touch::Move => match self.move_class(place) {
                MoveClass::Illegal => {
                    // The body sees an outer local through a shared borrow, so this
                    // move would be E1002. checker.md §7 names it E1407, and that is
                    // the only code (`check::tasks`).
                    if self.par_for_capture(place) {
                        return;
                    }
                    self.error(
                        flow,
                        "E1002",
                        "cannot move out of a borrow, an element or a static; use `@replace`, `@swap` or `.take()`",
                        span,
                        &path_key(place).unwrap_or_else(|| PathKey::local(0)),
                        Vec::new(),
                        None,
                    );
                }
                MoveClass::Partial | MoveClass::Normal => {
                    let Some(path) = path_key(place) else {
                        return;
                    };
                    if let Some(conflict) = self.conflict(flow, &path, true) {
                        self.report_conflict(flow, &path, span, defer, &conflict);
                        self.poison(flow, &path);
                    } else if !flow.poisoned_covering(&path) {
                        if matches!(self.move_class(place), MoveClass::Partial) {
                            let ty_name = self.linear_struct_name(place);
                            self.error(
                                flow,
                                "E1006",
                                &format!("cannot move a field of `{ty_name}` outside its module"),
                                span,
                                &path,
                                Vec::new(),
                                None,
                            );
                        }
                        self.mark_moved(flow, &path, span);
                    }
                }
            },
        }
    }

    fn report_conflict(
        &mut self,
        flow: &Flow,
        path: &PathKey,
        span: Span,
        defer: Option<DeferCopy>,
        conflict: &Conflict,
    ) {
        if !self.report {
            return;
        }
        if let Some(defer) = defer {
            let primary = conflict
                .sites
                .iter()
                .chain(conflict.loops.keys())
                .min()
                .copied()
                .map(|(s, e)| Span::new(s, e))
                .unwrap_or(span);
            let notes = vec![Note {
                role: "defer",
                span: defer.defer_span,
            }];
            self.error(
                flow,
                "E1005",
                "moved while a deferred block still uses it",
                primary,
                path,
                notes,
                None,
            );
            return;
        }
        let mut notes = Vec::new();
        let mut direct = BTreeSet::new();
        for (start, end) in &conflict.sites {
            if *start != 0 || *end != 0 {
                direct.insert((*start, *end));
                notes.push(Note {
                    role: "move",
                    span: Span::new(*start, *end),
                });
            }
        }
        let mut loops = BTreeSet::new();
        for ((start, end), (loop_start, loop_end)) in &conflict.loops {
            // A move that also arrives without going around the loop is not a
            // `loop` label.
            if direct.contains(&(*start, *end)) {
                continue;
            }
            if *start != 0 || *end != 0 {
                notes.push(Note {
                    role: "move",
                    span: Span::new(*start, *end),
                });
            }
            if *loop_start != 0 || *loop_end != 0 {
                loops.insert((*loop_start, *loop_end));
            }
        }
        for (start, end) in loops {
            notes.push(Note {
                role: "loop",
                span: Span::new(start, end),
            });
        }
        notes.sort_by(|a, b| {
            (a.span.start, a.span.end, a.role).cmp(&(b.span.start, b.span.end, b.role))
        });
        let message = if conflict.maybe {
            "may be moved"
        } else {
            "was moved"
        };
        self.error(flow, "E1001", message, span, path, notes, None);
    }

    fn conflict(&self, flow: &Flow, path: &PathKey, whole: bool) -> Option<Conflict> {
        let (st, mut sites) = flow.effective(path);
        let mut loops = flow.loops_at(path);
        // Poison resets the path to init so a later use of it is not a second
        // report. A joined *maybe* or *moved* is still an error: the flag rides
        // back around a loop and must not hide the use that poisoned it.
        if flow.poisoned_covering(path) && st == St::Init {
            let desc = whole
                && flow
                    .facts
                    .iter()
                    .any(|(k, f)| under(path, k) && f.st != St::Init && !flow.poisoned_covering(k));
            if !desc {
                return None;
            }
        }
        let mut bad = st != St::Init;
        let mut maybe = st == St::Maybe;
        if whole {
            for (k, f) in &flow.facts {
                if under(path, k) && f.st != St::Init && !flow.poisoned_covering(k) {
                    bad = true;
                    maybe |= f.st == St::Maybe;
                    sites.extend(f.sites.iter().copied());
                    if let Some(m) = flow.loops.get(k) {
                        for (s, l) in m {
                            loops.entry(*s).or_insert(*l);
                        }
                    }
                }
            }
        }
        if !bad {
            return None;
        }
        Some(Conflict {
            maybe,
            sites,
            loops,
        })
    }

    /// Root is the capture record of a `par for` body. Item parameters are later.
    fn par_for_capture(&self, place: &Place) -> bool {
        if !matches!(self.cfg.key.body, BodyKey::ParFor(_)) {
            return false;
        }
        self.cfg.params.first().copied() == place.root()
    }

    fn move_class(&mut self, place: &Place) -> MoveClass {
        if matches!(place.base, PlaceBase::Static(_)) {
            return MoveClass::Illegal;
        }
        let has_deref = place.proj.iter().any(|p| matches!(p, Projection::Deref));
        let has_index = place.proj.iter().any(|p| matches!(p, Projection::Index(_)));
        if has_deref {
            return MoveClass::Illegal;
        }
        if has_index {
            return MoveClass::Illegal;
        }
        if self.field_of_linear_outside(place) {
            return MoveClass::Partial;
        }
        MoveClass::Normal
    }

    fn field_of_linear_outside(&mut self, place: &Place) -> bool {
        let Some(path) = path_key(place) else {
            return false;
        };
        let Some(mut ty) = self.local_ty(path.local) else {
            return false;
        };
        for step in &path.proj {
            if matches!(step, Step::Field(_)) && self.linear_struct_outside(ty) {
                return true;
            }
            let Some(next) = self.step_ty(ty, *step) else {
                return false;
            };
            ty = next;
        }
        false
    }

    /// The nearest linear struct a field projection steps through, named for E1006.
    fn linear_struct_name(&mut self, place: &Place) -> String {
        let Some(path) = path_key(place) else {
            return "a linear struct".to_string();
        };
        let Some(mut ty) = self.local_ty(path.local) else {
            return "a linear struct".to_string();
        };
        for step in &path.proj {
            if matches!(step, Step::Field(_)) && self.linear_struct_outside(ty) {
                return self.decls.types.display(self.sema, ty);
            }
            let Some(next) = self.step_ty(ty, *step) else {
                break;
            };
            ty = next;
        }
        "a linear struct".to_string()
    }

    /// What `@replace` / `@swap` take out of `slot`. A dereference is not state
    /// this function tracks, so the value is fresh: the caller may see any variant.
    fn captured_slot(&mut self, flow: &Flow, slot: &Place) -> Brought {
        let through_deref = slot
            .proj
            .iter()
            .any(|p| matches!(p, Projection::Deref | Projection::DerefRaw));
        let Some(ty) = self.place_ty(slot) else {
            return self.fresh_empty();
        };
        if through_deref {
            return self.fresh(ty);
        }
        match path_key(slot) {
            Some(path) => self.capture_path(flow, &path, ty),
            None => self.fresh(ty),
        }
    }

    fn linear_struct_outside(&mut self, ty: TypeId) -> bool {
        let TyKind::Adt(def, _) = self.kind(ty) else {
            return false;
        };
        if self.decls.struct_sig(def).is_none() {
            return false;
        }
        if !self.counts_linear(ty) {
            return false;
        }
        self.sema.def(def).module != self.owner_module
    }

    fn payload_absent(&mut self, flow: &Flow, place: &Place) -> bool {
        let Some(path) = path_key(place) else {
            return false;
        };
        let Some(Step::Payload(v)) = path.proj.last().copied() else {
            return false;
        };
        let Some(parent) = path.parent() else {
            return false;
        };
        let Some(set) = flow.vars.get(&parent) else {
            return false;
        };
        !set.contains(&v)
    }

    fn narrow(&mut self, flow: &mut Flow, place: &Place, variant: u32, holds: bool) {
        let Some(path) = path_key(place) else {
            return;
        };
        if holds {
            let mut set = BTreeSet::new();
            set.insert(variant);
            flow.vars.insert(path.clone(), set);
            if let Some(live) = flow.live.get_mut(&path) {
                live.retain(|v| *v == variant);
            }
        } else {
            let count = self
                .place_ty(place)
                .map(|t| self.variant_count(t))
                .unwrap_or(0);
            let mut set = flow
                .vars
                .get(&path)
                .cloned()
                .unwrap_or_else(|| (0..count).collect());
            set.remove(&variant);
            flow.vars.insert(path.clone(), set);
            if let Some(live) = flow.live.get_mut(&path) {
                live.remove(&variant);
            }
        }
        if flow.live.get(&path).is_some_and(|s| s.is_empty()) {
            flow.live.remove(&path);
        }
    }

    fn poison(&mut self, flow: &mut Flow, path: &PathKey) {
        remove_under(flow, path);
        flow.vars.remove(path);
        flow.live.remove(path);
        flow.loops.remove(path);
        flow.facts.insert(
            path.clone(),
            Fact {
                st: St::Init,
                sites: BTreeSet::new(),
            },
        );
        flow.poisoned.insert(path.clone());
    }

    fn mark_moved(&mut self, flow: &mut Flow, path: &PathKey, span: Span) {
        remove_under(flow, path);
        flow.vars.remove(path);
        flow.live.remove(path);
        flow.loops.remove(path);
        if let Some(Step::Payload(v)) = path.proj.last().copied()
            && let Some(parent) = path.parent()
            && let Some(live) = flow.live.get_mut(&parent)
        {
            live.remove(&v);
            if live.is_empty() {
                flow.live.remove(&parent);
            }
        }
        let mut sites = BTreeSet::new();
        if span != Span::default() {
            sites.insert((span.start, span.end));
        }
        flow.facts.insert(
            path.clone(),
            Fact {
                st: St::Moved,
                sites,
            },
        );
        flow.poisoned.remove(path);
    }

    fn mark_gone(&mut self, flow: &mut Flow, path: &PathKey) {
        remove_under(flow, path);
        flow.vars.remove(path);
        flow.live.remove(path);
        flow.poisoned.remove(path);
        flow.loops.remove(path);
        flow.facts.insert(
            path.clone(),
            Fact {
                st: St::Moved,
                sites: BTreeSet::new(),
            },
        );
    }

    fn mark_init(&mut self, flow: &mut Flow, path: &PathKey, brought: Brought) {
        remove_under(flow, path);
        flow.poisoned.remove(path);
        flow.loops.remove(path);
        let prefix_st = path
            .parent()
            .map(|p| flow.effective(&p).0)
            .unwrap_or(St::Init);
        if prefix_st == St::Init {
            flow.facts.remove(path);
        } else {
            flow.facts.insert(
                path.clone(),
                Fact {
                    st: St::Init,
                    sites: BTreeSet::new(),
                },
            );
        }
        match brought.vars {
            Some(v) => {
                flow.vars.insert(path.clone(), v);
            }
            None => {
                flow.vars.remove(path);
            }
        }
        if brought.live.is_empty() {
            flow.live.remove(path);
        } else {
            flow.live.insert(path.clone(), brought.live);
        }
    }

    fn obligation(&mut self, flow: &Flow, path: &PathKey, ty: TypeId, depth: u32) -> bool {
        if depth > 64 || flow.poisoned_covering(path) {
            return false;
        }
        let st = flow.effective(path).0;
        if st == St::Moved {
            return false;
        }
        if self.is_enum(ty) {
            return flow.live.get(path).is_some_and(|s| !s.is_empty());
        }
        if self.split(flow, path) {
            for (step, fty) in self.children(ty) {
                if self.obligation(flow, &path.push(step), fty, depth + 1) {
                    return true;
                }
            }
            return false;
        }
        self.counts_linear(ty) && matches!(st, St::Init | St::Maybe)
    }

    fn split(&self, flow: &Flow, path: &PathKey) -> bool {
        flow.facts
            .iter()
            .any(|(k, f)| under(path, k) && f.st != St::Init && !flow.poisoned_covering(k))
    }

    fn brought(&mut self, flow: &Flow, value: &Rvalue, dest_ty: TypeId) -> Brought {
        match value {
            Rvalue::Use(Operand::Place(p, _)) => self.brought_place(flow, p),
            Rvalue::Aggregate {
                kind: AggregateKind::Variant { index, .. },
                ..
            } => {
                let index = *index as u32;
                let mut vars = BTreeSet::new();
                vars.insert(index);
                let mut live = BTreeSet::new();
                if self.linear_slots(dest_ty).contains(&index) {
                    live.insert(index);
                }
                Brought {
                    vars: Some(vars),
                    live,
                }
            }
            _ => self.fresh(dest_ty),
        }
    }

    fn brought_operand(&mut self, flow: &Flow, op: &Operand) -> Brought {
        match op {
            Operand::Place(p, _) => self.brought_place(flow, p),
            Operand::Const(_) => self.fresh_empty(),
        }
    }

    fn brought_place(&mut self, flow: &Flow, place: &Place) -> Brought {
        match path_key(place).zip(self.place_ty(place)) {
            Some((path, ty)) => self.capture_path(flow, &path, ty),
            None => self.fresh_empty(),
        }
    }

    fn capture_path(&mut self, flow: &Flow, path: &PathKey, ty: TypeId) -> Brought {
        if self.is_enum(ty) {
            Brought {
                vars: flow.vars.get(path).cloned(),
                live: flow.live.get(path).cloned().unwrap_or_default(),
            }
        } else {
            self.fresh_empty()
        }
    }

    fn fresh(&mut self, ty: TypeId) -> Brought {
        Brought {
            vars: None,
            live: self.linear_slots(ty),
        }
    }

    fn fresh_empty(&self) -> Brought {
        Brought {
            vars: None,
            live: BTreeSet::new(),
        }
    }

    fn linear_slots(&mut self, ty: TypeId) -> BTreeSet<u32> {
        let mut out = BTreeSet::new();
        match self.kind(ty) {
            TyKind::Optional(inner) => {
                // Only the `.some` payload, and only when that payload is linear.
                // `?T` is not an obligation in every variant the way `T ! E` is.
                if self.counts_linear(inner) {
                    out.insert(1);
                }
            }
            TyKind::Result(_, _) => {
                // `T ! E` is linear in itself: both variants are obligations,
                // whatever the payloads are (checker.md §3).
                out.insert(0);
                out.insert(1);
            }
            TyKind::Adt(def, args) => {
                let Some(sig_payloads) = self
                    .decls
                    .enum_sig(def)
                    .map(|s| s.variants.iter().map(|v| v.payload).collect::<Vec<_>>())
                else {
                    return out;
                };
                let subst = self.decls.subst_for(def, &args);
                for (i, payload) in sig_payloads.into_iter().enumerate() {
                    let Some(payload) = payload else {
                        continue;
                    };
                    let payload = self.decls.types.subst(payload, &subst);
                    if self.counts_linear(payload) {
                        out.insert(i as u32);
                    }
                }
            }
            _ => {}
        }
        out
    }

    fn counts_linear(&mut self, ty: TypeId) -> bool {
        self.decls.props(ty).maybe_linear == Tri::Yes
    }

    fn is_enum(&self, ty: TypeId) -> bool {
        match self.kind_ref(ty) {
            TyKind::Optional(_) | TyKind::Result(_, _) => true,
            TyKind::Adt(def, _) => self.decls.enum_sig(*def).is_some(),
            _ => false,
        }
    }

    fn variant_count(&self, ty: TypeId) -> u32 {
        match self.kind_ref(ty) {
            TyKind::Optional(_) | TyKind::Result(_, _) => 2,
            TyKind::Adt(def, _) => self
                .decls
                .enum_sig(*def)
                .map(|s| s.variants.len() as u32)
                .unwrap_or(0),
            _ => 0,
        }
    }

    fn kind(&self, ty: TypeId) -> TyKind {
        self.decls.types.kind(ty).clone()
    }

    fn kind_ref(&self, ty: TypeId) -> &TyKind {
        self.decls.types.kind(ty)
    }

    fn local_ty(&self, local: Local) -> Option<TypeId> {
        self.cfg.locals.get(local as usize).map(|l| l.ty)
    }

    fn place_ty(&mut self, place: &Place) -> Option<TypeId> {
        let path = path_key(place)?;
        self.path_ty(&path)
    }

    fn path_ty(&mut self, path: &PathKey) -> Option<TypeId> {
        let mut ty = self.local_ty(path.local)?;
        for step in &path.proj {
            ty = self.step_ty(ty, *step)?;
        }
        Some(ty)
    }

    fn step_ty(&mut self, ty: TypeId, step: Step) -> Option<TypeId> {
        match step {
            Step::Field(i) => self
                .struct_field_ty(ty, i)
                .or_else(|| self.closure_field_ty(ty, i)),
            Step::Tuple(i) => match self.kind(ty) {
                TyKind::Tuple(es) => es.get(i as usize).copied(),
                _ => None,
            },
            Step::Elem => match self.kind(ty) {
                TyKind::Array(e, _) | TyKind::Slice(_, e) => Some(e),
                _ => None,
            },
            Step::Deref => match self.kind(ty) {
                TyKind::Ref(_, e) | TyKind::Slice(_, e) => Some(e),
                _ => None,
            },
            Step::Raw => match self.kind(ty) {
                TyKind::RawPtr(_, e) => Some(e),
                _ => None,
            },
            Step::Payload(v) => self.payload_ty(ty, v),
        }
    }

    fn struct_field_ty(&mut self, ty: TypeId, index: u32) -> Option<TypeId> {
        let TyKind::Adt(def, args) = self.kind(ty) else {
            return None;
        };
        let field_ty = self.decls.struct_sig(def)?.fields.get(index as usize)?.ty;
        let subst = self.decls.subst_for(def, &args);
        Some(self.decls.types.subst(field_ty, &subst))
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

    fn closure_field_ty(&self, ty: TypeId, index: u32) -> Option<TypeId> {
        if self.cfg.closure_ty != Some(ty) {
            return None;
        }
        self.cfg.closure_fields.get(index as usize).map(|f| f.ty)
    }

    fn children(&mut self, ty: TypeId) -> Vec<(Step, TypeId)> {
        match self.kind(ty) {
            TyKind::Closure(_, _) if self.cfg.closure_ty == Some(ty) => self
                .cfg
                .closure_fields
                .iter()
                .enumerate()
                .map(|(i, f)| (Step::Field(i as u32), f.ty))
                .collect(),
            TyKind::Tuple(es) => es
                .into_iter()
                .enumerate()
                .map(|(i, t)| (Step::Tuple(i as u32), t))
                .collect(),
            TyKind::Adt(def, args) => {
                if self.decls.enum_sig(def).is_some() {
                    return Vec::new();
                }
                let fields: Vec<TypeId> = match self.decls.struct_sig(def) {
                    Some(s) => s.fields.iter().map(|f| f.ty).collect(),
                    None => return Vec::new(),
                };
                let subst = self.decls.subst_for(def, &args);
                let mut out = Vec::new();
                for (i, field) in fields.into_iter().enumerate() {
                    out.push((Step::Field(i as u32), self.decls.types.subst(field, &subst)));
                }
                out
            }
            _ => Vec::new(),
        }
    }

    fn exit_spans(&self, block: usize, stmt_idx: usize) -> Vec<Span> {
        exit_spans(self.cfg, block, stmt_idx)
    }

    #[allow(clippy::too_many_arguments)]
    fn error(
        &mut self,
        _flow: &Flow,
        code: &'static str,
        message: &str,
        span: Span,
        _path: &PathKey,
        notes: Vec<Note>,
        fix: Option<(String, Edit)>,
    ) {
        if !self.report {
            return;
        }
        let key = (code_id(code), span.start, span.end);
        if !self.seen.insert(key) {
            return;
        }
        let (fix_message, edit) = match fix {
            Some((m, e)) => (Some(m), Some(e)),
            None => (None, None),
        };
        self.diags.push(RawDiag {
            code,
            message: message.to_string(),
            span,
            notes,
            fix_message,
            edit,
        });
    }

    fn emit_leaks(&mut self, source: &Source) {
        let keys: Vec<(usize, usize)> = self.leaks.keys().copied().collect();
        for key in keys {
            let Some(rec) = self.leaks.get(&key) else {
                continue;
            };
            let span = Span::new(key.0, key.1);
            let ty = rec.ty;
            let kind = rec.kind;
            let name = rec.name.clone();
            let decl = rec.decl;
            let exits = rec.exits.clone();
            let mut notes: Vec<Note> = exits
                .iter()
                .map(|(s, e)| Note {
                    role: "scope_end",
                    span: Span::new(*s, *e),
                })
                .collect();
            notes.sort_by_key(|n| (n.span.start, n.span.end));
            let (message, fix) = self.leak_fix(source, ty, kind, name.as_deref(), decl);
            self.error_leak(message, span, notes, fix);
        }
    }

    fn error_leak(
        &mut self,
        message: String,
        span: Span,
        notes: Vec<Note>,
        fix: Option<(String, Edit)>,
    ) {
        let (fix_message, edit) = match fix {
            Some((m, e)) => (Some(m), Some(e)),
            None => (None, None),
        };
        self.diags.push(RawDiag {
            code: "E1003",
            message,
            span,
            notes,
            fix_message,
            edit,
        });
    }

    fn leak_fix(
        &mut self,
        source: &Source,
        ty: TypeId,
        kind: LocalKind,
        name: Option<&str>,
        span: Span,
    ) -> (String, Option<(String, Edit)>) {
        let who = match name {
            Some(n) => format!("`{n}` is not consumed"),
            None => "a temporary is not consumed".to_string(),
        };
        if let TyKind::Param(p) = self.kind(ty)
            && self.counts_linear(ty)
            && let Some(edit) = self.discard_edit(source, p)
        {
            return (who, Some(("add the bound `Discard`".to_string(), edit)));
        }
        let candidates = self.consuming_methods(ty);
        if kind == LocalKind::User
            && candidates.len() == 1
            && let Some(name) = name
        {
            let method = candidates[0].0.clone();
            let edit = defer_edit(source, span, name, &method);
            return (
                who,
                Some((format!("insert `defer {{ {name}.{method}(); }}`"), edit)),
            );
        }
        let listed = if candidates.is_empty() {
            "no consuming function".to_string()
        } else {
            let names: Vec<&str> = candidates.iter().map(|(n, _)| n.as_str()).collect();
            format!("consuming functions: {}", names.join(", "))
        };
        (format!("{who}; {listed}"), None)
    }

    fn discard_edit(&self, source: &Source, param: DefId) -> Option<Edit> {
        let owner = self.sema.def(param).parent?;
        let g = self
            .decls
            .generics_of(owner)
            .iter()
            .find(|g| g.def == param)?;
        if g.is_const {
            return None;
        }
        let (at, text) = if g.bounds.is_empty() {
            (g.span.start + g.name.len(), ": Discard".to_string())
        } else {
            let end = g.bound_spans.last().map(|s| s.end).unwrap_or(g.span.end);
            (end, " + Discard".to_string())
        };
        Some(Edit {
            file: source.path.clone(),
            span: Span::empty(at),
            text,
        })
    }

    /// Inherent methods that take `self` by value, with no other parameter and no
    /// error type. Sorted by the method name's span, then the name.
    fn consuming_methods(&mut self, ty: TypeId) -> Vec<(String, Span)> {
        let matches = self.decls.inherent_impls(ty);
        let mut raw: Vec<(String, Span, TypeId, ingot_sema::ty::Subst)> = Vec::new();
        for m in &matches {
            let Some(sig) = self.decls.impl_sig(m.impl_def) else {
                continue;
            };
            let methods: Vec<(String, DefId)> =
                sig.methods.iter().map(|(n, d)| (n.clone(), *d)).collect();
            for (name, def) in methods {
                let Some(fn_sig) = self.decls.fn_sig(def) else {
                    continue;
                };
                if fn_sig.variadic || fn_sig.error.is_some() || fn_sig.params.len() != 1 {
                    continue;
                }
                let param = &fn_sig.params[0];
                if param.name != "self" {
                    continue;
                }
                raw.push((name, fn_sig.name_span, param.ty, m.subst.clone()));
            }
        }
        let mut out = Vec::new();
        for (name, span, self_ty, subst) in raw {
            let self_ty = self.decls.types.subst(self_ty, &subst);
            if self_ty != ty {
                continue;
            }
            if matches!(
                self.decls.types.kind(self_ty),
                TyKind::Ref(_, _) | TyKind::Slice(_, _)
            ) {
                continue;
            }
            out.push((span.start, span, name));
        }
        out.sort_by(|a, b| (a.0, a.2.as_str()).cmp(&(b.0, b.2.as_str())));
        out.dedup_by(|a, b| a.2 == b.2 && a.0 == b.0);
        out.into_iter()
            .map(|(_, span, name)| (name, span))
            .collect()
    }

    fn into_diagnostics(self, source: &Source) -> Vec<Diagnostic> {
        let mut out = Vec::new();
        for d in self.diags {
            let mut diag = Diagnostic::new(d.code, d.message, source, d.span);
            for n in d.notes {
                diag.relate(n.role, n.span);
            }
            if let (Some(message), Some(edit)) = (d.fix_message, d.edit) {
                diag.fix(message, edit);
            }
            out.push(diag);
        }
        out
    }
}

struct Conflict {
    maybe: bool,
    sites: BTreeSet<MoveSpan>,
    loops: BTreeMap<MoveSpan, LoopSpan>,
}

enum Touch {
    Read {
        whole: bool,
    },
    Move,
    /// Element step of a consuming `for`. Not E1002, not a tracked move.
    ForElem,
}

enum MoveClass {
    Normal,
    Illegal,
    Partial,
}

fn path_key(place: &Place) -> Option<PathKey> {
    let PlaceBase::Local(local) = place.base else {
        return None;
    };
    let mut proj = Vec::new();
    for p in &place.proj {
        let step = match p {
            Projection::Field { index, .. } => Step::Field(*index as u32),
            Projection::TupleField(i) => Step::Tuple(*i as u32),
            Projection::Index(_) => Step::Elem,
            Projection::Deref => Step::Deref,
            Projection::DerefRaw => Step::Raw,
            Projection::Payload { variant } => Step::Payload(*variant as u32),
        };
        proj.push(step);
    }
    Some(PathKey { local, proj })
}

/// Spans that name the exit ending storage at `stmt_idx`, the same ones E1003
/// uses. Empty when this storage end is not that exit: a fall-through has no
/// span here, and a `scope_wait` does not name one either.
pub(crate) fn exit_spans(cfg: &Cfg, block: usize, stmt_idx: usize) -> Vec<Span> {
    let Some(data) = cfg.blocks.get(block) else {
        return Vec::new();
    };
    // Only a storage end with nothing but cleanup after it, leaving through
    // a return or a goto. A `scope_wait` has no span that names the exit.
    let after = data.stmts.get(stmt_idx.saturating_add(1)..).unwrap_or(&[]);
    if !after.iter().all(|s| is_cleanup(&s.kind)) {
        return Vec::new();
    }
    if !matches!(data.term, Terminator::Return { .. } | Terminator::Goto(_)) {
        return Vec::new();
    }
    let mut spans = Vec::new();
    if data.term_span != Span::default() {
        spans.push(data.term_span);
    }
    for s in &data.stmts {
        if let Some(d) = s.defer
            && d.exit_span != Span::default()
            && !spans.contains(&d.exit_span)
        {
            spans.push(d.exit_span);
        }
    }
    spans
}

fn is_cleanup(kind: &StmtKind) -> bool {
    matches!(
        kind,
        StmtKind::StorageStart(_)
            | StmtKind::StorageEnd(_)
            | StmtKind::Cancel(_)
            | StmtKind::Abandon(_)
            | StmtKind::EnterUnsafe
            | StmtKind::Check(_)
            | StmtKind::Variant { .. }
    )
}

fn defer_edit(source: &Source, span: Span, name: &str, method: &str) -> Edit {
    let text = &source.text;
    let at = span.start.min(text.len());
    let line_start = text[..at].rfind('\n').map(|i| i + 1).unwrap_or(0);
    let indent: String = text[line_start..]
        .chars()
        .take_while(|c| *c == ' ' || *c == '\t')
        .collect();
    let line_end = text[at..].find('\n').map(|i| at + i).unwrap_or(text.len());
    // A `let` that spans lines ends after the first newline. Insert there.
    // A one-line declaration, or a span that is only a name, still inserts at
    // the end of its line.
    let end = span.end.min(text.len());
    let insert_at = if end > line_end { end } else { line_end };
    Edit {
        file: source.path.clone(),
        span: Span::empty(insert_at),
        text: format!("\n{indent}defer {{ {name}.{method}(); }}"),
    }
}

/// Move direct sites onto the loop map. The clone is what joins; the block's
/// stored entry is untouched. Sites are not deleted from the lattice: a later
/// direct arrival is unioned back in `join`.
fn mark_loop(flow: &mut Flow, loop_span: Span) {
    let paths: Vec<PathKey> = flow.facts.keys().cloned().collect();
    let loop_at = (loop_span.start, loop_span.end);
    for path in paths {
        let Some(fact) = flow.facts.get(&path) else {
            continue;
        };
        let old: Vec<MoveSpan> = fact.sites.iter().copied().collect();
        if old.is_empty() {
            continue;
        }
        let entry = flow.loops.entry(path.clone()).or_default();
        for site in old {
            entry.entry(site).or_insert(loop_at);
        }
        if let Some(fact) = flow.facts.get_mut(&path) {
            fact.sites.clear();
        }
    }
}

fn join_into(slot: &mut Option<Flow>, inc: Flow) -> bool {
    match slot {
        None => {
            *slot = Some(inc);
            true
        }
        Some(cur) => {
            let joined = join(cur, &inc);
            if &joined == cur {
                false
            } else {
                *cur = joined;
                true
            }
        }
    }
}

fn join(a: &Flow, b: &Flow) -> Flow {
    let mut keys = BTreeSet::new();
    for k in a.facts.keys().chain(b.facts.keys()) {
        let mut p = k.clone();
        keys.insert(p.clone());
        while p.proj.pop().is_some() {
            keys.insert(p.clone());
        }
    }
    let mut eff: BTreeMap<PathKey, (St, BTreeSet<MoveSpan>)> = BTreeMap::new();
    for k in &keys {
        let (sa, mut sites) = a.effective(k);
        let (sb, sites_b) = b.effective(k);
        sites.extend(sites_b);
        let st = join_st(sa, sb);
        if st == St::Init {
            sites.clear();
        }
        eff.insert(k.clone(), (st, sites));
    }
    let mut out = Flow::default();
    for (k, (st, sites)) in &eff {
        let prefix = k
            .parent()
            .and_then(|p| eff.get(&p))
            .cloned()
            .unwrap_or((St::Init, BTreeSet::new()));
        if *st != prefix.0 || sites != &prefix.1 {
            out.facts.insert(
                k.clone(),
                Fact {
                    st: *st,
                    sites: sites.clone(),
                },
            );
        }
    }
    let mut vkeys = BTreeSet::new();
    vkeys.extend(a.vars.keys().cloned());
    vkeys.extend(b.vars.keys().cloned());
    for k in vkeys {
        if let (Some(x), Some(y)) = (a.vars.get(&k), b.vars.get(&k)) {
            let mut u = x.clone();
            u.extend(y.iter().copied());
            out.vars.insert(k, u);
        }
    }
    let mut lkeys = BTreeSet::new();
    lkeys.extend(a.live.keys().cloned());
    lkeys.extend(b.live.keys().cloned());
    for k in lkeys {
        let mut u = BTreeSet::new();
        if let Some(x) = a.live.get(&k) {
            u.extend(x.iter().copied());
        }
        if let Some(y) = b.live.get(&k) {
            u.extend(y.iter().copied());
        }
        if !u.is_empty() {
            out.live.insert(k, u);
        }
    }
    let mut lkeys = BTreeSet::new();
    lkeys.extend(a.loops.keys().cloned());
    lkeys.extend(b.loops.keys().cloned());
    for k in lkeys {
        let st = match eff.get(&k) {
            Some((st, _)) => *st,
            None => {
                let (sa, _) = a.effective(&k);
                let (sb, _) = b.effective(&k);
                join_st(sa, sb)
            }
        };
        if st == St::Init {
            continue;
        }
        let mut m = BTreeMap::new();
        if let Some(x) = a.loops.get(&k) {
            m.extend(x.iter().map(|(s, l)| (*s, *l)));
        }
        if let Some(y) = b.loops.get(&k) {
            for (s, l) in y {
                m.entry(*s).or_insert(*l);
            }
        }
        if !m.is_empty() {
            out.loops.insert(k, m);
        }
    }
    out.poisoned = a.poisoned.union(&b.poisoned).cloned().collect();
    out
}

fn back_edges(cfg: &Cfg) -> BTreeSet<(u32, u32)> {
    let n = cfg.blocks.len();
    let mut color = vec![0u8; n];
    let mut edges = BTreeSet::new();
    if n == 0 || cfg.entry as usize >= n {
        return edges;
    }
    let mut stack = vec![(cfg.entry as usize, false)];
    while let Some((b, done)) = stack.pop() {
        if done {
            color[b] = 2;
            continue;
        }
        if color[b] != 0 {
            continue;
        }
        color[b] = 1;
        stack.push((b, true));
        for s in successors(&cfg.blocks[b].term) {
            let s = s as usize;
            if s >= n {
                continue;
            }
            if color[s] == 1 {
                edges.insert((b as u32, s as u32));
            } else if color[s] == 0 {
                stack.push((s, false));
            }
        }
    }
    edges
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
