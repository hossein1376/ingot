//! Phase 10: guaranteed check removal and cost items (`docs/impl/pipeline.md` §6,
//! specification §31).
//!
//! The analysis is a forward dataflow over one CFG. It tracks, per integer local, a
//! set of lower and upper bounds, each a constant, a constant generic parameter plus a
//! constant, or `len(place)` plus a constant. A fact about a local dies when the local
//! is assigned; a fact about `len(place)` dies when the place is assigned, exclusively
//! borrowed through an overlapping path, or given by exclusive borrow to a call.
//!
//! The six removals of §6 are applied in [`remove_checks`]. A check that survives is
//! reported as a [`CostItem`] for `ingot cost`; a folded operation whose constants trap
//! is E0806 at the operation. The CFG that the function returns has the removed check
//! statements gone and is still structurally valid; the phase-8 CFG is left alone, so
//! the dump and its goldens are phase 8's output.

use crate::model::*;
use ingot_diag::diagnostic::Diagnostic;
use ingot_diag::source::{Sources, Span};
use ingot_sema::consteval::ValueState;
use ingot_sema::defs::DefId;
use ingot_sema::sigs::{Declarations, ItemSig};
use ingot_sema::ty::{ConstArg, ConstValue, IntTy, Mutability, TyKind, TypeId};
use ingot_sema::value::Value as SemValue;
use ingot_syntax::ast::{BinOp, UnOp};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

/// What `ingot cost` reports for a surviving check.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum CostKind {
    BoundsCheck,
    OverflowCheck,
    DivCheck,
    ShiftCheck,
    ScatterCheck,
}

impl CostKind {
    pub fn name(self) -> &'static str {
        match self {
            CostKind::BoundsCheck => "bounds_check",
            CostKind::OverflowCheck => "overflow_check",
            CostKind::DivCheck => "div_check",
            CostKind::ShiftCheck => "shift_check",
            CostKind::ScatterCheck => "scatter_check",
        }
    }
}

/// One surviving check, with the source span of the expression it guards.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CostItem {
    pub kind: CostKind,
    pub cfg: CfgId,
    /// The qualified name of the function whose body the check is in.
    pub fn_name: String,
    pub file: usize,
    pub span: Span,
}

/// The result of phase 10 for a package.
pub struct Removal {
    /// The phase-8 CFGs with the removed check statements gone.
    pub cfgs: Cfgs,
    pub items: Vec<CostItem>,
    pub diagnostics: Vec<Diagnostic>,
    pub not_implemented: Vec<String>,
}

/// The largest number of elements an array or slice has (spec §3): a bound on any
/// `len(place)` and on a constant added to it.
const MAX_LEN: i128 = 1 << 62;

/// Run phase 10 on every CFG of the checked package.
pub fn remove_checks(decls: &Declarations, sources: &Sources, cfgs: &Cfgs) -> Removal {
    let engine = Engine { decls, sources };
    let mut out = cfgs.clone();
    let mut items = Vec::new();
    let mut diagnostics = Vec::new();
    let mut not_implemented = Vec::new();
    for (id, cfg) in cfgs.iter() {
        let one = engine.analyze_cfg(id, cfg);
        items.extend(one.items);
        diagnostics.extend(one.diagnostics);
        not_implemented.extend(one.not_implemented);
        if let Some((_, target)) = out.iter_mut().find(|(i, _)| *i == id) {
            for (block, removed) in one.removed {
                for index in removed.into_iter().rev() {
                    if index < target.blocks[block as usize].stmts.len() {
                        target.blocks[block as usize].stmts.remove(index);
                    }
                }
            }
        }
    }
    items.sort_by(|a, b| {
        (a.fn_name.clone(), a.file, a.span.start, a.span.end, a.kind).cmp(&(
            b.fn_name.clone(),
            b.file,
            b.span.start,
            b.span.end,
            b.kind,
        ))
    });
    diagnostics.sort_by(|a, b| {
        (a.file.clone(), a.span.start, a.code).cmp(&(b.file.clone(), b.span.start, b.code))
    });
    not_implemented.sort();
    not_implemented.dedup();
    Removal {
        cfgs: out,
        items,
        diagnostics,
        not_implemented,
    }
}

struct Engine<'a> {
    decls: &'a Declarations,
    sources: &'a Sources,
}

struct CfgAnalysis {
    items: Vec<CostItem>,
    diagnostics: Vec<Diagnostic>,
    not_implemented: Vec<String>,
    removed: Vec<(Block, Vec<usize>)>,
}

impl Engine<'_> {
    fn analyze_cfg(&self, id: CfgId, cfg: &Cfg) -> CfgAnalysis {
        let two_phase = two_phase_borrows(cfg);
        let n = cfg.blocks.len();
        let mut ins: Vec<Option<State>> = vec![None; n];
        let mut incoming: Vec<Vec<(Block, usize)>> = vec![Vec::new(); n];
        for (block, data) in cfg.blocks.iter().enumerate() {
            for (slot, succ) in self.successors(&data.term).into_iter().enumerate() {
                incoming[succ as usize].push((block as Block, slot));
            }
        }
        let mut edge_states: Vec<Vec<Option<State>>> = cfg
            .blocks
            .iter()
            .map(|block| vec![None; self.successors(&block.term).len()])
            .collect();
        let entry = cfg.entry as usize;
        if entry < n {
            ins[entry] = Some(State::seed(cfg, self.decls));
        }
        let mut queue: VecDeque<Block> = VecDeque::new();
        if entry < n {
            queue.push_back(cfg.entry);
        }
        let mut visits = vec![0usize; n];
        let cap = 64 * n + 64;
        let mut converged = true;
        while let Some(block) = queue.pop_front() {
            visits[block as usize] += 1;
            if visits[block as usize] > cap {
                converged = false;
                break;
            }
            let Some(state) = ins[block as usize].clone() else {
                continue;
            };
            let step = self.transfer(cfg, block, state, &two_phase, false);
            let mut touched = Vec::new();
            for (slot, (succ, edge)) in step.edges.into_iter().enumerate() {
                if step.skipped.contains(&slot) {
                    // A branch on a constant condition: the untaken edge is dead.
                    // Nothing it reaches is reported, and its checks are removed.
                    if let Some(cell) = edge_states[block as usize].get_mut(slot) {
                        *cell = None;
                    }
                    continue;
                }
                if let Some(cell) = edge_states[block as usize].get_mut(slot) {
                    *cell = Some(edge);
                }
                touched.push(succ);
            }
            for succ in touched {
                let available: Vec<State> = incoming[succ as usize]
                    .iter()
                    .filter_map(|(pred, slot)| {
                        edge_states[*pred as usize].get(*slot).cloned().flatten()
                    })
                    .collect();
                let Some((first, rest)) = available.split_first() else {
                    continue;
                };
                let merged = rest
                    .iter()
                    .fold(first.clone(), |current, edge| current.join(edge));
                if ins[succ as usize].as_ref() != Some(&merged) {
                    ins[succ as usize] = Some(merged);
                    queue.push_back(succ);
                }
            }
        }
        let mut analysis = CfgAnalysis {
            items: Vec::new(),
            diagnostics: Vec::new(),
            not_implemented: Vec::new(),
            removed: Vec::new(),
        };
        if !converged {
            analysis.not_implemented.push(format!(
                "phase 10 dataflow did not converge in {}",
                cfg.name
            ));
            return analysis;
        }
        // Rule 5: a block no taken edge reaches is dead. Its operations are not
        // reported, and its checks are removed.
        for (block, state) in ins.iter().enumerate() {
            if state.is_some() {
                continue;
            }
            let removed: Vec<usize> = cfg.blocks[block]
                .stmts
                .iter()
                .enumerate()
                .filter(|(_, s)| matches!(s.kind, StmtKind::Check(_)))
                .map(|(i, _)| i)
                .collect();
            if !removed.is_empty() {
                analysis.removed.push((block as Block, removed));
            }
        }
        for (block, state) in ins.iter().enumerate() {
            let Some(state) = state.clone() else {
                continue;
            };
            let step = self.transfer(cfg, block as Block, state, &two_phase, true);
            for (_, kind, span) in step.items {
                let fn_name = match cfg.key.body {
                    BodyKey::Item => cfg.name.clone(),
                    BodyKey::Closure(_) | BodyKey::ParFor(_) => {
                        cfg.name.split(".{").next().unwrap_or(&cfg.name).to_string()
                    }
                };
                analysis.items.push(CostItem {
                    kind,
                    cfg: id,
                    fn_name,
                    file: cfg.file,
                    span,
                });
            }
            analysis.diagnostics.extend(step.diagnostics);
            if !step.removed.is_empty() {
                analysis.removed.push((block as Block, step.removed));
            }
        }
        analysis
    }

    fn successors(&self, term: &Terminator) -> Vec<Block> {
        match term {
            Terminator::Goto(next) => vec![*next],
            Terminator::Branch {
                then_block,
                else_block,
                ..
            } => vec![*then_block, *else_block],
            Terminator::Switch { cases, default, .. } => {
                let mut out = vec![*default];
                out.extend(cases.iter().map(|(_, block)| *block));
                out
            }
            Terminator::Call { next, .. }
            | Terminator::Spawn { next, .. }
            | Terminator::ScopeWait { next, .. }
            | Terminator::ParFor { next, .. }
            | Terminator::Dispatch { next, .. } => vec![*next],
            Terminator::Select { ops } => ops.iter().map(|op| op.successor).collect(),
            Terminator::Return { .. } | Terminator::Trap { .. } | Terminator::Unreachable => {
                Vec::new()
            }
        }
    }

    fn transfer(
        &self,
        cfg: &Cfg,
        block: Block,
        state: State,
        two_phase: &BTreeMap<Local, Place>,
        report: bool,
    ) -> Step {
        let mut state = state;
        let mut defs: BTreeMap<Local, Rvalue> = BTreeMap::new();
        let mut step = Step::default();
        for (index, stmt) in cfg.blocks[block as usize].stmts.iter().enumerate() {
            match &stmt.kind {
                StmtKind::Assign { place, value } => {
                    self.apply_assign(cfg, &mut state, &mut defs, place, value);
                }
                StmtKind::Check(check) => {
                    let key = self.check_key(cfg, check, &defs);
                    let first = match &key {
                        Some(k) => state.avail.insert(k.clone()),
                        None => true,
                    };
                    match self.decide(cfg, check, &state, &defs, first) {
                        Decision::Keep(kind) => {
                            if let Some(item) = kind {
                                step.items.push((index, item, check.span));
                            }
                        }
                        Decision::Remove => step.removed.push(index),
                        Decision::Trap(message) => {
                            step.removed.push(index);
                            if report && let Some(source) = self.sources.files.get(cfg.file) {
                                step.diagnostics
                                    .push(Diagnostic::new("E0806", message, source, check.span));
                            }
                        }
                    }
                }
                StmtKind::StorageStart(_)
                | StmtKind::StorageEnd(_)
                | StmtKind::EnterUnsafe
                | StmtKind::Cancel(_)
                | StmtKind::Abandon(_)
                | StmtKind::Use(_)
                | StmtKind::Variant { .. }
                | StmtKind::IterConsumed(_) => {}
            }
        }
        self.terminator(cfg, block, &mut state, &mut step, &defs, two_phase);
        step.out = state;
        step
    }

    /// The facts after `place = value`, and the exclusivity effects of a borrow.
    fn apply_assign(
        &self,
        cfg: &Cfg,
        state: &mut State,
        defs: &mut BTreeMap<Local, Rvalue>,
        place: &Place,
        value: &Rvalue,
    ) {
        let target = if place.proj.is_empty() {
            place.root()
        } else {
            None
        };
        let written = place_key(place);
        self.kill_len_facts(state, &written);
        state.avail.retain(|check| !check.place_overlaps(&written));
        if let Some(local) = target {
            state.avail.retain(|check| !check.mentions(local));
        }
        if let Some(x) = target {
            let range = state.type_bounds(cfg, self.decls, x);
            let new = self.value_bounds(cfg, value, x, state, defs);
            state.locals.insert(x, new.normalized(range));
            defs.insert(x, value.clone());
        }
        match value {
            Rvalue::Borrow {
                place,
                mutability: Mutability::Mut,
                ..
            }
            | Rvalue::Reborrow { place, .. } => {
                self.kill_exclusive_borrow(cfg, state, place);
            }
            Rvalue::Slice {
                base,
                borrow: Some(Mutability::Mut),
                ..
            } => {
                self.kill_exclusive_borrow(cfg, state, base);
            }
            _ => {}
        }
    }

    /// An exclusive borrow of `place` resets the bounds of its root local, kills every
    /// `len` fact that overlaps the place, and drops the rule-6 checks it protects.
    /// Phase 9 forbids reading the root while the loan is live, so no fact about it can
    /// be re-established until the borrow ends. This is the one place exclusivity is
    /// handled, however the borrow flows (a struct field, a closure capture, a call).
    fn kill_exclusive_borrow(&self, cfg: &Cfg, state: &mut State, place: &Place) {
        let key = place_key(place);
        self.kill_len_facts_overlapping(state, &key);
        state.avail.retain(|check| !check.place_overlaps(&key));
        if let Some(local) = place.root() {
            state
                .locals
                .insert(local, state.type_bounds(cfg, self.decls, local));
            state.avail.retain(|check| !check.mentions(local));
        }
    }

    /// The bounds a local gets when `value` is assigned to it.
    fn value_bounds(
        &self,
        cfg: &Cfg,
        value: &Rvalue,
        dest: Local,
        state: &State,
        defs: &BTreeMap<Local, Rvalue>,
    ) -> Bounds {
        let range = state.type_bounds(cfg, self.decls, dest);
        match value {
            Rvalue::Use(Operand::Const(Const::Int { value, .. })) => {
                Bounds::exact(Val::constant(*value))
            }
            Rvalue::Use(Operand::Const(Const::Param { def, .. })) => {
                Bounds::exact(Val::symbol(Sym::Param(*def), 0))
            }
            Rvalue::Use(Operand::Const(Const::Named { def, .. })) => match self.const_named(*def) {
                Some(value) => Bounds::exact(Val::constant(value)),
                None => range,
            },
            Rvalue::Use(Operand::Place(src, _)) if src.proj.is_empty() => {
                if let Some(source) = src.root() {
                    if cfg.local(source).kind == LocalKind::Temp
                        && let Some(def) = defs.get(&source)
                    {
                        return self.assigned_bounds(cfg, def, dest, state, defs, 0);
                    }
                    if let Some(bounds) = state.locals.get(&source) {
                        return bounds.clone().normalized(range);
                    }
                }
                range
            }
            Rvalue::Cast {
                kind: ingot_sema::typed::CastKind::IntWiden,
                operand,
            } => match self.operand_type(cfg, operand) {
                Some(ty) => match self.decls.types.kind(ty) {
                    TyKind::Int(int) => Bounds {
                        lo: BTreeSet::from([Val::constant(IntTy::min(*int))]),
                        hi: BTreeSet::from([Val::constant(IntTy::max(*int))]),
                    },
                    _ => range,
                },
                None => range,
            },
            Rvalue::SliceLen(place) => {
                let key = place_key(place);
                Bounds::exact(Val::symbol(Sym::Len(key), 0))
            }
            _ => range,
        }
    }

    /// The bounds of a value produced by a nested assignment. Only a copy and the
    /// compiler's `x += c` shape keep a bound; everything else is the type's range.
    fn assigned_bounds(
        &self,
        cfg: &Cfg,
        value: &Rvalue,
        dest: Local,
        state: &State,
        defs: &BTreeMap<Local, Rvalue>,
        depth: u32,
    ) -> Bounds {
        let range = state.type_bounds(cfg, self.decls, dest);
        if depth > 32 {
            return range;
        }
        match value {
            Rvalue::Use(Operand::Const(Const::Int { value, .. })) => {
                Bounds::exact(Val::constant(*value))
            }
            Rvalue::Use(Operand::Const(Const::Param { def, .. })) => {
                Bounds::exact(Val::symbol(Sym::Param(*def), 0))
            }
            Rvalue::Use(Operand::Const(Const::Named { def, .. })) => match self.const_named(*def) {
                Some(value) => Bounds::exact(Val::constant(value)),
                None => range,
            },
            Rvalue::Use(Operand::Place(src, _)) if src.proj.is_empty() => {
                if let Some(source) = src.root() {
                    if cfg.local(source).kind == LocalKind::Temp
                        && let Some(def) = defs.get(&source)
                    {
                        return self.assigned_bounds(cfg, def, dest, state, defs, depth + 1);
                    }
                    if let Some(bounds) = state.locals.get(&source) {
                        return bounds.clone().normalized(range);
                    }
                }
                range
            }
            Rvalue::SliceLen(place) => {
                let key = place_key(place);
                Bounds::exact(Val::symbol(Sym::Len(key), 0))
            }
            Rvalue::Binary { op, lhs, rhs }
                if matches!(op, BinOp::Add | BinOp::Sub | BinOp::AddSat | BinOp::SubSat) =>
            {
                let pair = if let Some(c) = self.constant_operand(rhs) {
                    Some((lhs, *op, c))
                } else if *op == BinOp::Add {
                    self.constant_operand(lhs).map(|c| (rhs, *op, c))
                } else {
                    None
                };
                if let Some((other, op, c)) = pair
                    && self.copies_local(cfg, other, dest, defs, 0)
                    && let Some(old) = state.locals.get(&dest)
                {
                    return self.compound_bounds(old, op, c).normalized(range);
                }
                range
            }
            Rvalue::Cast {
                kind: ingot_sema::typed::CastKind::IntWiden,
                operand,
            } => match self.operand_type(cfg, operand) {
                Some(ty) => match self.decls.types.kind(ty) {
                    TyKind::Int(int) => Bounds {
                        lo: BTreeSet::from([Val::constant(IntTy::min(*int))]),
                        hi: BTreeSet::from([Val::constant(IntTy::max(*int))]),
                    },
                    _ => range,
                },
                None => range,
            },
            _ => range,
        }
    }

    /// Whether `op` is a copy of local `dest` (through compiler temporaries).
    fn copies_local(
        &self,
        cfg: &Cfg,
        op: &Operand,
        dest: Local,
        defs: &BTreeMap<Local, Rvalue>,
        depth: u32,
    ) -> bool {
        if depth > 32 {
            return false;
        }
        let Operand::Place(place, _) = op else {
            return false;
        };
        if !place.proj.is_empty() {
            return false;
        }
        let Some(local) = place.root() else {
            return false;
        };
        if local == dest {
            return true;
        }
        if cfg.local(local).kind != LocalKind::Temp {
            return false;
        }
        match defs.get(&local) {
            Some(Rvalue::Use(next)) => self.copies_local(cfg, next, dest, defs, depth + 1),
            _ => false,
        }
    }

    fn compound_bounds(&self, old: &Bounds, op: BinOp, c: i128) -> Bounds {
        let delta = match op {
            BinOp::Add | BinOp::AddSat => c,
            BinOp::Sub | BinOp::SubSat => -c,
            _ => return old.clone(),
        };
        Bounds {
            lo: if delta >= 0 {
                old.lo.clone()
            } else {
                BTreeSet::new()
            },
            hi: if delta <= 0 {
                old.hi.clone()
            } else {
                BTreeSet::new()
            },
        }
    }

    fn constant_operand(&self, op: &Operand) -> Option<i128> {
        match op {
            Operand::Const(Const::Int { value, .. }) => Some(*value),
            Operand::Const(Const::Named { def, .. }) => self.const_named(*def),
            _ => None,
        }
    }

    fn operand_type(&self, cfg: &Cfg, op: &Operand) -> Option<TypeId> {
        match op {
            Operand::Const(Const::Int { ty, .. }) => Some(*ty),
            Operand::Const(Const::Param { def, .. }) => self.decls.param_const_ty.get(def).cloned(),
            Operand::Const(Const::Named { def, .. }) => match self.decls.items.get(def) {
                Some(ItemSig::Const(c)) => Some(c.ty),
                _ => None,
            },
            Operand::Place(place, _) => self.place_ty(cfg, place),
            Operand::Const(_) => None,
        }
    }

    fn const_bool_named(&self, def: DefId) -> Option<bool> {
        match self.decls.consts.get(&def) {
            Some(ingot_sema::consteval::ConstState::Done(Ok(ConstValue::Bool(b)))) => Some(*b),
            _ => match self.decls.values.get(&def) {
                Some(ValueState::Done(Ok(SemValue::Bool(b)))) => Some(*b),
                _ => None,
            },
        }
    }

    fn const_named(&self, def: DefId) -> Option<i128> {
        match self.decls.consts.get(&def) {
            Some(ingot_sema::consteval::ConstState::Done(Ok(ConstValue::Int {
                value, ..
            }))) => Some(*value),
            _ => match self.decls.values.get(&def) {
                Some(ValueState::Done(Ok(SemValue::Int { value, .. }))) => Some(*value),
                _ => None,
            },
        }
    }

    fn terminator(
        &self,
        cfg: &Cfg,
        block: Block,
        state: &mut State,
        step: &mut Step,
        defs: &BTreeMap<Local, Rvalue>,
        two_phase: &BTreeMap<Local, Place>,
    ) {
        let term = &cfg.blocks[block as usize].term;
        // A two-phase exclusive borrow lets its root be read until it activates here,
        // so a fact and a rule-6 check can be re-established after reservation and
        // must die at activation.
        let mut activated = Vec::new();
        match term {
            Terminator::Call { args, .. } | Terminator::Spawn { args, .. } => {
                for arg in args {
                    if let Some(place) = two_phase_arg(arg, defs, two_phase) {
                        activated.push(place);
                    }
                }
            }
            _ => {}
        }
        for place in &activated {
            self.kill_exclusive_borrow(cfg, state, place);
        }
        // A `dispatch` borrow has no `Borrow` statement of its own: the terminator is
        // the borrow. Its exclusive outputs can change a length.
        if let Terminator::Dispatch { args, .. } = term {
            for arg in args {
                if arg.borrow == Some(Mutability::Mut)
                    && let Some(place) = &arg.place
                {
                    self.kill_exclusive_borrow(cfg, state, place);
                }
            }
        }
        match term {
            Terminator::Goto(next) => step.edges.push((*next, state.clone())),
            Terminator::Branch {
                cond,
                then_block,
                else_block,
            } => {
                let mut yes = state.clone();
                let mut no = state.clone();
                let mut constant = None;
                if let Some((op, lhs, rhs)) = self.comparison(cfg, cond, defs) {
                    self.edge_facts(cfg, &mut yes, op, &lhs, &rhs, true);
                    self.edge_facts(cfg, &mut no, op, &lhs, &rhs, false);
                }
                // Rule 5's constants are literals, `const` items and operations on
                // those (through compiler temporaries). A dataflow fact about a user
                // local is not a constant condition and must not make a block dead for
                // reporting.
                if let Some((op, a, b)) = self.constant_comparison(cfg, cond, defs) {
                    constant = constant_compare(op, a, b);
                } else if let Some(value) = self.bool_constant(cfg, cond, defs) {
                    constant = Some(value);
                }
                step.edges.push((*then_block, yes));
                step.edges.push((*else_block, no));
                // Rule 5: a branch on a constant condition is folded first. The
                // untaken edge is unreachable in the dataflow.
                if let Some(value) = constant {
                    step.skipped.push(if value { 1 } else { 0 });
                }
            }
            Terminator::Switch { cases, default, .. } => {
                step.edges.push((*default, state.clone()));
                for (_, next) in cases {
                    step.edges.push((*next, state.clone()));
                }
            }
            Terminator::Call { next, .. }
            | Terminator::Spawn { next, .. }
            | Terminator::ScopeWait { next, .. }
            | Terminator::ParFor { next, .. }
            | Terminator::Dispatch { next, .. } => step.edges.push((*next, state.clone())),
            Terminator::Select { ops } => {
                for op in ops {
                    step.edges.push((op.successor, state.clone()));
                }
            }
            Terminator::Return { .. } | Terminator::Trap { .. } | Terminator::Unreachable => {}
        }
    }

    /// The operands of a comparison expression, before any dataflow facts are used.
    fn comparison_operands(
        &self,
        cond: &Operand,
        defs: &BTreeMap<Local, Rvalue>,
    ) -> Option<(BinOp, Operand, Operand)> {
        let Operand::Place(place, _) = cond else {
            return None;
        };
        if !place.proj.is_empty() {
            return None;
        }
        let mut local = place.root()?;
        let mut depth = 0;
        let (op, lhs, rhs) = loop {
            depth += 1;
            if depth > 64 {
                return None;
            }
            match defs.get(&local)? {
                Rvalue::Use(Operand::Place(src, _)) if src.proj.is_empty() => {
                    local = src.root()?;
                }
                Rvalue::Binary { op, lhs, rhs } => break (*op, lhs.clone(), rhs.clone()),
                _ => return None,
            }
        };
        if !matches!(
            op,
            BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge | BinOp::Eq | BinOp::Ne
        ) {
            return None;
        }
        Some((op, lhs, rhs))
    }

    fn comparison(
        &self,
        cfg: &Cfg,
        cond: &Operand,
        defs: &BTreeMap<Local, Rvalue>,
    ) -> Option<(BinOp, Lx, Lx)> {
        let (op, lhs, rhs) = self.comparison_operands(cond, defs)?;
        let left = self.lin_expr(cfg, &lhs, defs, 0)?;
        let right = self.lin_expr(cfg, &rhs, defs, 0)?;
        Some((op, left, right))
    }

    /// A comparison whose two operands are rule-5 constants: literals, `const`
    /// items, or operations on those, through compiler temporaries.
    fn constant_comparison(
        &self,
        cfg: &Cfg,
        cond: &Operand,
        defs: &BTreeMap<Local, Rvalue>,
    ) -> Option<(BinOp, i128, i128)> {
        let (op, lhs, rhs) = self.comparison_operands(cond, defs)?;
        Some((
            op,
            self.const_int(cfg, &lhs, defs, 0)?,
            self.const_int(cfg, &rhs, defs, 0)?,
        ))
    }

    /// The value of a constant `bool` condition, through compiler temporaries.
    fn bool_constant(
        &self,
        cfg: &Cfg,
        cond: &Operand,
        defs: &BTreeMap<Local, Rvalue>,
    ) -> Option<bool> {
        match cond {
            Operand::Const(Const::Bool(b)) => Some(*b),
            Operand::Const(Const::Named { def, .. }) => self.const_bool_named(*def),
            Operand::Const(_) => None,
            Operand::Place(place, _) => {
                let root = place.root()?;
                if cfg.local(root).kind != LocalKind::Temp {
                    return None;
                }
                match defs.get(&root)? {
                    Rvalue::Use(op) => self.bool_constant(cfg, op, defs),
                    Rvalue::Unary {
                        op: UnOp::Not,
                        operand,
                    } => self.bool_constant(cfg, operand, defs).map(|b| !b),
                    _ => None,
                }
            }
        }
    }

    /// A linear expression of the values the analysis tracks: a constant, a constant
    /// generic parameter, `len(place)`, or a local, each plus an offset.
    fn lin_expr(
        &self,
        cfg: &Cfg,
        op: &Operand,
        defs: &BTreeMap<Local, Rvalue>,
        depth: u32,
    ) -> Option<Lx> {
        if depth > 32 {
            return None;
        }
        match op {
            Operand::Const(Const::Int { value, .. }) => Some(Lx::Val(Val::constant(*value))),
            Operand::Const(Const::Param { def, .. }) => {
                Some(Lx::Val(Val::symbol(Sym::Param(*def), 0)))
            }
            Operand::Const(Const::Named { def, .. }) => {
                self.const_named(*def).map(|v| Lx::Val(Val::constant(v)))
            }
            Operand::Place(place, _) if place.proj.is_empty() => {
                let local = place.root()?;
                if cfg.local(local).kind == LocalKind::Temp
                    && let Some(def) = defs.get(&local)
                {
                    return match def {
                        Rvalue::Use(Operand::Const(Const::Int { value, .. })) => {
                            Some(Lx::Val(Val::constant(*value)))
                        }
                        Rvalue::Use(Operand::Const(Const::Param { def, .. })) => {
                            Some(Lx::Val(Val::symbol(Sym::Param(*def), 0)))
                        }
                        Rvalue::Use(Operand::Const(Const::Named { def, .. })) => {
                            self.const_named(*def).map(|v| Lx::Val(Val::constant(v)))
                        }
                        Rvalue::Use(Operand::Place(src, _)) if src.proj.is_empty() => {
                            let next = Operand::Place(src.clone(), PlaceUse::Copy);
                            self.lin_expr(cfg, &next, defs, depth + 1)
                        }
                        Rvalue::SliceLen(place) => {
                            Some(Lx::Val(Val::symbol(Sym::Len(place_key(place)), 0)))
                        }
                        Rvalue::Binary { op, lhs, rhs }
                            if matches!(op, BinOp::Add | BinOp::Sub) =>
                        {
                            self.linear_binary(cfg, *op, lhs, rhs, defs, depth + 1)
                        }
                        _ => None,
                    };
                }
                Some(Lx::Local(local, 0))
            }
            _ => None,
        }
    }

    fn linear_binary(
        &self,
        cfg: &Cfg,
        op: BinOp,
        lhs: &Operand,
        rhs: &Operand,
        defs: &BTreeMap<Local, Rvalue>,
        depth: u32,
    ) -> Option<Lx> {
        if depth > 32 {
            return None;
        }
        if let Some(c) = self.constant_operand(rhs)
            && let Some(left) = self.lin_expr(cfg, lhs, defs, depth + 1)
        {
            return Some(left.add(match op {
                BinOp::Add => c,
                BinOp::Sub => -c,
                _ => return None,
            }));
        }
        if op == BinOp::Add
            && let Some(c) = self.constant_operand(lhs)
            && let Some(right) = self.lin_expr(cfg, rhs, defs, depth + 1)
        {
            return Some(right.add(c));
        }
        None
    }

    /// Add the facts of `lhs op rhs` being true (or false) to `state`.
    fn edge_facts(&self, cfg: &Cfg, state: &mut State, op: BinOp, lhs: &Lx, rhs: &Lx, truth: bool) {
        let op = if truth { op } else { invert(op) };
        let (op, lhs, rhs) = normalize(op, lhs.clone(), rhs.clone());
        match op {
            BinOp::Lt | BinOp::Le => {
                let adjust = if op == BinOp::Lt { -1 } else { 0 };
                if let Lx::Local(x, ox) = &lhs
                    && let Some(value) = self.symbolic_value(state, &rhs)
                {
                    let bound = value.add(adjust - ox);
                    let range = state.type_bounds(cfg, self.decls, *x);
                    state.add_upper(*x, bound, range);
                }
                if let Lx::Local(y, oy) = &rhs
                    && let Some(value) = self.symbolic_value(state, &lhs)
                {
                    let bound = value.add(-adjust - oy);
                    let range = state.type_bounds(cfg, self.decls, *y);
                    state.add_lower(*y, bound, range);
                }
                if let Lx::Val(a) = &lhs
                    && let Lx::Val(b) = &rhs
                {
                    if let Some(Sym::Len(key)) = b.sym.clone() {
                        let bound = a.clone().add(-adjust - b.off);
                        state.add_len_lower(key, bound);
                    }
                    if let Some(Sym::Len(key)) = a.sym.clone() {
                        let bound = b.clone().add(adjust - a.off);
                        state.add_len_upper(key, bound);
                    }
                }
            }
            BinOp::Eq => {
                if let Lx::Local(x, ox) = &lhs
                    && let Some(value) = self.symbolic_value(state, &rhs)
                {
                    state.set_exact(*x, value.add(-ox));
                }
                if let Lx::Local(y, oy) = &rhs
                    && let Some(value) = self.symbolic_value(state, &lhs)
                {
                    state.set_exact(*y, value.add(-oy));
                }
                if let Lx::Val(a) = &lhs
                    && let Lx::Val(b) = &rhs
                {
                    if let Some(Sym::Len(key)) = b.sym.clone() {
                        let bound = a.clone().add(-b.off);
                        state.add_len_lower(key.clone(), bound.clone());
                        state.add_len_upper(key, bound);
                    }
                    if let Some(Sym::Len(key)) = a.sym.clone() {
                        let bound = b.clone().add(-a.off);
                        state.add_len_lower(key.clone(), bound.clone());
                        state.add_len_upper(key, bound);
                    }
                }
            }
            _ => {}
        }
    }

    /// The symbolic value of an expression, when it is a constant, a constant generic
    /// parameter, `len(place)`, or a local whose bounds are one exact such value.
    fn symbolic_value(&self, state: &State, expr: &Lx) -> Option<Val> {
        match expr {
            Lx::Val(v) => Some(v.clone()),
            Lx::Local(local, offset) => {
                let bounds = state.locals.get(local)?;
                let shared: Vec<&Val> = bounds.lo.intersection(&bounds.hi).collect();
                match shared.as_slice() {
                    [value] => Some((*value).clone().add(*offset)),
                    _ => None,
                }
            }
        }
    }

    fn decide(
        &self,
        cfg: &Cfg,
        check: &Check,
        state: &State,
        defs: &BTreeMap<Local, Rvalue>,
        first: bool,
    ) -> Decision {
        // Rule 5 for a bounds check: a constant index against a constant length is
        // folded. In range it is removed; out of range it is E0806 with T01.
        if let CheckKind::Bounds(BoundsKind::Index) = check.kind
            && let Some((index, len)) = self.constant_bounds(cfg, check, defs)
        {
            return if index >= 0 && index < len {
                Decision::Remove
            } else {
                Decision::Trap(format!(
                    "this operation always traps: T01: index {index} out of bounds for length {len}"
                ))
            };
        }
        // Rule 5 first: a folded operation that traps is E0806, not a check removal.
        if let Some(result) = self.fold_check(cfg, check, defs) {
            return match result {
                Ok(()) => Decision::Remove,
                Err(trap) => Decision::Trap(format!(
                    "this operation always traps: {}: {}",
                    trap.code(),
                    trap.text()
                )),
            };
        }
        if !first {
            // Rule 6: an identical check dominated this one.
            return Decision::Remove;
        }
        match check.kind {
            CheckKind::Bounds(kind) => {
                if self.prove_bounds(cfg, check, kind, state, defs) {
                    Decision::Remove
                } else {
                    Decision::Keep(Some(CostKind::BoundsCheck))
                }
            }
            CheckKind::Overflow => {
                if self.prove_overflow(cfg, check, state, defs) {
                    Decision::Remove
                } else {
                    Decision::Keep(Some(CostKind::OverflowCheck))
                }
            }
            CheckKind::Division => Decision::Keep(Some(CostKind::DivCheck)),
            CheckKind::Shift => Decision::Keep(Some(CostKind::ShiftCheck)),
            CheckKind::Gather | CheckKind::DispatchCover => {
                Decision::Keep(Some(CostKind::BoundsCheck))
            }
            CheckKind::ScatterDistinct => Decision::Keep(Some(CostKind::ScatterCheck)),
            // `@narrow` is written in the source and is not a cost item (§31).
            CheckKind::Narrow => Decision::Keep(None),
        }
    }

    /// The index and length of a bounds check when both are constants.
    fn constant_bounds(
        &self,
        cfg: &Cfg,
        check: &Check,
        defs: &BTreeMap<Local, Rvalue>,
    ) -> Option<(i128, i128)> {
        let index = self.const_int(cfg, check.lo.as_ref()?, defs, 0)?;
        let place = check.place.as_ref()?;
        let ty = self.place_ty(cfg, place)?;
        let TyKind::Array(_, len) = self.decls.types.kind(ty) else {
            return None;
        };
        let ConstArg::Value(ConstValue::Int { value, .. }) = len else {
            return None;
        };
        Some((index, *value))
    }

    fn fold_check(
        &self,
        cfg: &Cfg,
        check: &Check,
        defs: &BTreeMap<Local, Rvalue>,
    ) -> Option<Result<(), ingot_sema::consteval::Trap>> {
        if !matches!(
            check.kind,
            CheckKind::Overflow | CheckKind::Division | CheckKind::Shift
        ) {
            return None;
        }
        let ty = check.ty.and_then(|t| match self.decls.types.kind(t) {
            TyKind::Int(i) => Some(*i),
            _ => None,
        })?;
        let mut values = Vec::new();
        for op in &check.operands {
            values.push(self.const_int(cfg, op, defs, 0)?);
        }
        let op = check.op.as_deref()?;
        let result = if values.len() == 1 && op == "-" {
            ingot_sema::consteval::int_unary(UnOp::Neg, ty, values[0])
        } else if values.len() == 2 {
            let bin = match op {
                "+" => BinOp::Add,
                "-" => BinOp::Sub,
                "*" => BinOp::Mul,
                "/" => BinOp::Div,
                "%" => BinOp::Rem,
                "<<" => BinOp::Shl,
                ">>" => BinOp::Shr,
                _ => return None,
            };
            ingot_sema::consteval::int_binary(bin, ty, values[0], values[1])
        } else {
            return None;
        };
        Some(result.map(|_| ()))
    }

    fn const_int(
        &self,
        cfg: &Cfg,
        op: &Operand,
        defs: &BTreeMap<Local, Rvalue>,
        depth: u32,
    ) -> Option<i128> {
        if depth > 32 {
            return None;
        }
        match op {
            Operand::Const(Const::Int { value, .. }) => Some(*value),
            Operand::Const(Const::Named { def, .. }) => self.const_named(*def),
            Operand::Const(_) => None,
            Operand::Place(place, _) => {
                if !place.proj.is_empty() {
                    return None;
                }
                let local = place.root()?;
                // A user local is never a constant for rule 5, even when its last
                // assignment was a literal.
                if cfg.local(local).kind != LocalKind::Temp {
                    return None;
                }
                let value = defs.get(&local)?;
                match value {
                    Rvalue::Use(op) => self.const_int(cfg, op, defs, depth + 1),
                    Rvalue::Unary { op, operand } => {
                        let a = self.const_int(cfg, operand, defs, depth + 1)?;
                        let ty = self.local_int(cfg, local)?;
                        ingot_sema::consteval::int_unary(*op, ty, a).ok()
                    }
                    Rvalue::Binary { op, lhs, rhs } => {
                        if !arithmetic(*op) {
                            return None;
                        }
                        let a = self.const_int(cfg, lhs, defs, depth + 1)?;
                        let b = self.const_int(cfg, rhs, defs, depth + 1)?;
                        let ty = self.local_int(cfg, local)?;
                        ingot_sema::consteval::int_binary(*op, ty, a, b).ok()
                    }
                    Rvalue::Cast {
                        kind: ingot_sema::typed::CastKind::IntWiden,
                        operand,
                    } => self.const_int(cfg, operand, defs, depth + 1),
                    _ => None,
                }
            }
        }
    }

    fn local_int(&self, cfg: &Cfg, local: Local) -> Option<IntTy> {
        match self.decls.types.kind(cfg.local(local).ty) {
            TyKind::Int(i) => Some(*i),
            _ => None,
        }
    }

    fn prove_bounds(
        &self,
        cfg: &Cfg,
        check: &Check,
        kind: BoundsKind,
        state: &State,
        defs: &BTreeMap<Local, Rvalue>,
    ) -> bool {
        let Some(place) = &check.place else {
            return false;
        };
        let key = place_key(place);
        match kind {
            BoundsKind::Index => {
                let Some(index) = &check.lo else {
                    return false;
                };
                let Some(e) = self.lin_expr(cfg, index, defs, 0) else {
                    return false;
                };
                self.non_negative(state, &e) && self.below_len(cfg, state, &key, &e)
            }
            BoundsKind::Slice => {
                let (Some(lo), Some(hi)) = (&check.lo, &check.hi) else {
                    return false;
                };
                let (Some(lo), Some(hi)) = (
                    self.lin_expr(cfg, lo, defs, 0),
                    self.lin_expr(cfg, hi, defs, 0),
                ) else {
                    return false;
                };
                self.non_negative(state, &lo)
                    && self.at_most_len(cfg, state, &key, &hi)
                    && self.at_most(state, &lo, &hi)
            }
            BoundsKind::Vector { width, tail } => {
                if tail {
                    // A tail access may load at the very end; no rule moves it.
                    return false;
                }
                let Some(index) = &check.lo else {
                    return false;
                };
                let Some(e) = self.lin_expr(cfg, index, defs, 0) else {
                    return false;
                };
                let hi = e.clone().add(i128::from(width));
                self.non_negative(state, &e) && self.at_most_len(cfg, state, &key, &hi)
            }
        }
    }

    fn prove_overflow(
        &self,
        cfg: &Cfg,
        check: &Check,
        state: &State,
        defs: &BTreeMap<Local, Rvalue>,
    ) -> bool {
        let ty = match check.ty.and_then(|t| match self.decls.types.kind(t) {
            TyKind::Int(i) => Some(*i),
            _ => None,
        }) {
            Some(ty) => ty,
            None => return false,
        };
        let (min, max) = if check.operands.len() == 1 {
            let Some((lo, hi)) = self.operand_range(cfg, &check.operands[0], state, defs) else {
                return false;
            };
            match (hi.checked_neg(), lo.checked_neg()) {
                (Some(min), Some(max)) => (min, max),
                _ => return false,
            }
        } else if check.operands.len() == 2 {
            let (Some((la, ua)), Some((lb, ub))) = (
                self.operand_range(cfg, &check.operands[0], state, defs),
                self.operand_range(cfg, &check.operands[1], state, defs),
            ) else {
                return false;
            };
            match check.op.as_deref() {
                Some("+") => match (la.checked_add(lb), ua.checked_add(ub)) {
                    (Some(lo), Some(hi)) => (lo, hi),
                    _ => return false,
                },
                Some("-") => match (la.checked_sub(ub), ua.checked_sub(lb)) {
                    (Some(lo), Some(hi)) => (lo, hi),
                    _ => return false,
                },
                Some("*") => {
                    let mut lo = i128::MAX;
                    let mut hi = i128::MIN;
                    for a in [la, ua] {
                        for b in [lb, ub] {
                            match a.checked_mul(b) {
                                Some(v) => {
                                    lo = lo.min(v);
                                    hi = hi.max(v);
                                }
                                None => return false,
                            }
                        }
                    }
                    (lo, hi)
                }
                _ => return false,
            }
        } else {
            return false;
        };
        ty.min() <= min && max <= ty.max()
    }

    fn operand_range(
        &self,
        cfg: &Cfg,
        op: &Operand,
        state: &State,
        defs: &BTreeMap<Local, Rvalue>,
    ) -> Option<(i128, i128)> {
        match op {
            Operand::Const(Const::Int { value, .. }) => Some((*value, *value)),
            Operand::Const(Const::Named { def, .. }) => self.const_named(*def).map(|v| (v, v)),
            Operand::Const(Const::Param { def, .. }) => self
                .decls
                .param_const_ty
                .get(def)
                .and_then(|ty| match self.decls.types.kind(*ty) {
                    TyKind::Int(i) => Some((IntTy::min(*i), IntTy::max(*i))),
                    _ => None,
                }),
            Operand::Const(_) => None,
            Operand::Place(place, _) => {
                if !place.proj.is_empty() {
                    let ty = self.place_ty(cfg, place)?;
                    return match self.decls.types.kind(ty) {
                        TyKind::Int(i) => Some((IntTy::min(*i), IntTy::max(*i))),
                        _ => None,
                    };
                }
                let local = place.root()?;
                if let Some(Rvalue::Cast {
                    kind: ingot_sema::typed::CastKind::IntWiden,
                    operand,
                }) = defs.get(&local)
                    && let Some(ty) = self.operand_type(cfg, operand)
                    && let TyKind::Int(i) = self.decls.types.kind(ty)
                {
                    return Some((IntTy::min(*i), IntTy::max(*i)));
                }
                let bounds = state
                    .locals
                    .get(&local)
                    .cloned()
                    .unwrap_or_else(|| state.type_bounds(cfg, self.decls, local));
                let lo = self.strongest_lower(&bounds.lo)?;
                let hi = self.strongest_upper(&bounds.hi)?;
                Some((lo, hi))
            }
        }
    }

    fn non_negative(&self, state: &State, expr: &Lx) -> bool {
        self.lower_values(state, expr)
            .iter()
            .any(|v| self.numeric_lower(v).is_some_and(|value| value >= 0))
    }

    fn below_len(&self, cfg: &Cfg, state: &State, key: &PlaceKey, expr: &Lx) -> bool {
        let Some((len_lo, len_hi)) = self.len_bounds(cfg, state, key) else {
            return false;
        };
        let len_number = self.numeric_lower(&len_lo);
        self.upper_values(state, expr).iter().any(|value| {
            if let Some(Sym::Len(same)) = &value.sym
                && same == key
            {
                return value.off < 0;
            }
            for len_value in [&len_lo, &len_hi] {
                if let (Some(a), Some(b)) = (&value.sym, &len_value.sym)
                    && a == b
                {
                    return value.off < len_value.off;
                }
            }
            match (self.numeric_upper(value), len_number) {
                (Some(upper), Some(lower)) => upper < lower,
                _ => false,
            }
        })
    }

    fn at_most_len(&self, cfg: &Cfg, state: &State, key: &PlaceKey, expr: &Lx) -> bool {
        let Some((len_lo, len_hi)) = self.len_bounds(cfg, state, key) else {
            return false;
        };
        let len_number = self.numeric_lower(&len_lo);
        self.upper_values(state, expr).iter().any(|value| {
            if let Some(Sym::Len(same)) = &value.sym
                && same == key
            {
                return value.off <= 0;
            }
            for len_value in [&len_lo, &len_hi] {
                if let (Some(a), Some(b)) = (&value.sym, &len_value.sym)
                    && a == b
                {
                    return value.off <= len_value.off;
                }
            }
            match (self.numeric_upper(value), len_number) {
                (Some(upper), Some(lower)) => upper <= lower,
                _ => false,
            }
        })
    }

    fn at_most(&self, state: &State, lo: &Lx, hi: &Lx) -> bool {
        if let (Lx::Local(a, oa), Lx::Local(b, ob)) = (lo, hi)
            && a == b
        {
            return oa <= ob;
        }
        let (Some(lo_u), Some(hi_l)) = (
            self.strongest_upper(&self.upper_values(state, lo)),
            self.strongest_lower(&self.lower_values(state, hi)),
        ) else {
            return false;
        };
        lo_u <= hi_l
    }

    fn lower_values(&self, state: &State, expr: &Lx) -> BTreeSet<Val> {
        match expr {
            Lx::Val(v) => BTreeSet::from([v.clone()]),
            Lx::Local(local, offset) => state
                .locals
                .get(local)
                .map(|b| b.lo.iter().map(|v| v.clone().add(*offset)).collect())
                .unwrap_or_default(),
        }
    }

    fn upper_values(&self, state: &State, expr: &Lx) -> BTreeSet<Val> {
        match expr {
            Lx::Val(v) => BTreeSet::from([v.clone()]),
            Lx::Local(local, offset) => state
                .locals
                .get(local)
                .map(|b| b.hi.iter().map(|v| v.clone().add(*offset)).collect())
                .unwrap_or_default(),
        }
    }

    fn strongest_lower(&self, values: &BTreeSet<Val>) -> Option<i128> {
        values.iter().filter_map(|v| self.numeric_lower(v)).max()
    }

    fn strongest_upper(&self, values: &BTreeSet<Val>) -> Option<i128> {
        values.iter().filter_map(|v| self.numeric_upper(v)).min()
    }

    fn numeric_lower(&self, value: &Val) -> Option<i128> {
        match &value.sym {
            None => Some(value.off),
            Some(Sym::Len(_)) => Some(value.off),
            Some(Sym::Param(def)) => {
                let min = self
                    .decls
                    .param_const_ty
                    .get(def)
                    .and_then(|ty| match self.decls.types.kind(*ty) {
                        TyKind::Int(i) => Some(IntTy::min(*i)),
                        _ => None,
                    })
                    .unwrap_or(i128::MIN);
                min.checked_add(value.off)
            }
        }
    }

    fn numeric_upper(&self, value: &Val) -> Option<i128> {
        match &value.sym {
            None => Some(value.off),
            Some(Sym::Len(_)) => MAX_LEN.checked_add(value.off),
            Some(Sym::Param(def)) => {
                let max = self
                    .decls
                    .param_const_ty
                    .get(def)
                    .and_then(|ty| match self.decls.types.kind(*ty) {
                        TyKind::Int(i) => Some(IntTy::max(*i)),
                        _ => None,
                    })
                    .unwrap_or(i128::MAX);
                max.checked_add(value.off)
            }
        }
    }

    /// The lower and upper bound of `len(place)`.
    fn len_bounds(&self, cfg: &Cfg, state: &State, key: &PlaceKey) -> Option<(Val, Val)> {
        if let Some(ty) = key_ty(key, cfg, self.decls)
            && let TyKind::Array(_, length) = self.decls.types.kind(ty)
        {
            if let Some(value) = length.as_len() {
                return Some((Val::constant(value), Val::constant(value)));
            }
            if let ingot_sema::ty::ConstArg::Param(def) = length {
                let lower = state
                    .lens
                    .get(key)
                    .and_then(|b| b.lo.iter().filter(|v| v.sym.is_none()).map(|v| v.off).max())
                    .map(|value| Val::constant(value.max(0)))
                    .unwrap_or(Val::constant(0));
                return Some((lower, Val::symbol(Sym::Param(*def), 0)));
            }
        }
        let lower = state
            .lens
            .get(key)
            .and_then(|b| b.lo.iter().filter(|v| v.sym.is_none()).map(|v| v.off).max())
            .map(|value| Val::constant(value.max(0)))
            .unwrap_or(Val::constant(0));
        let upper = state
            .lens
            .get(key)
            .and_then(|b| b.hi.iter().filter(|v| v.sym.is_none()).map(|v| v.off).min())
            .map(|value| Val::constant(value.clamp(0, MAX_LEN)))
            .unwrap_or(Val::constant(MAX_LEN));
        Some((lower, upper))
    }

    fn place_ty(&self, cfg: &Cfg, place: &Place) -> Option<TypeId> {
        key_ty(&place_key(place), cfg, self.decls)
    }

    fn check_key(
        &self,
        cfg: &Cfg,
        check: &Check,
        defs: &BTreeMap<Local, Rvalue>,
    ) -> Option<CheckKey> {
        Some(CheckKey {
            tag: check_tag(check.kind),
            place: check.place.as_ref().map(place_key),
            lo: check.lo.as_ref().map(|o| self.op_key(cfg, o, defs, 0)),
            hi: check.hi.as_ref().map(|o| self.op_key(cfg, o, defs, 0)),
            operands: check
                .operands
                .iter()
                .map(|o| self.op_key(cfg, o, defs, 0))
                .collect(),
        })
    }

    fn op_key(&self, cfg: &Cfg, op: &Operand, defs: &BTreeMap<Local, Rvalue>, depth: u32) -> OpKey {
        if depth > 32 {
            return OpKey::Unknown;
        }
        match op {
            Operand::Const(Const::Int { value, ty }) => OpKey::Const(*value, *ty),
            Operand::Const(Const::Named { def, .. }) => match self.const_named(*def) {
                Some(value) => OpKey::Const(value, self.decls.types.i64_),
                None => OpKey::Unknown,
            },
            Operand::Const(Const::Param { def, .. }) => OpKey::Param(*def),
            Operand::Const(_) => OpKey::Unknown,
            Operand::Place(place, _) => {
                if place.proj.is_empty()
                    && let Some(local) = place.root()
                    && cfg.local(local).kind == LocalKind::Temp
                {
                    return match defs.get(&local) {
                        Some(Rvalue::Use(next)) => self.op_key(cfg, next, defs, depth + 1),
                        Some(Rvalue::Binary { op, lhs, rhs }) => OpKey::Binary(
                            bin_tag(*op),
                            Box::new(self.op_key(cfg, lhs, defs, depth + 1)),
                            Box::new(self.op_key(cfg, rhs, defs, depth + 1)),
                        ),
                        Some(Rvalue::Unary { op, operand }) => OpKey::Unary(
                            un_tag(*op),
                            Box::new(self.op_key(cfg, operand, defs, depth + 1)),
                        ),
                        Some(Rvalue::SliceLen(place)) => OpKey::SliceLen(place_key(place)),
                        _ => OpKey::Temp(local),
                    };
                }
                OpKey::Place(place_key(place))
            }
        }
    }

    /// Every `len(place)` fact whose place overlaps `written` dies. This is the
    /// borrow form: an exclusive borrow of an overlapping path may change the length.
    fn kill_len_facts_overlapping(&self, state: &mut State, written: &PlaceKey) {
        state.lens.retain(|place, _| !overlaps(place, written));
        for bounds in state.locals.values_mut() {
            bounds.lo.retain(|v| {
                !v.sym
                    .clone()
                    .is_some_and(|s| len_overlaps_overlapping(s, written))
            });
            bounds.hi.retain(|v| {
                !v.sym
                    .clone()
                    .is_some_and(|s| len_overlaps_overlapping(s, written))
            });
        }
    }

    /// Every `len(place)` fact whose place overlaps `written` dies.
    fn kill_len_facts(&self, state: &mut State, written: &PlaceKey) {
        state.lens.retain(|place, _| !covers(written, place));
        for bounds in state.locals.values_mut() {
            bounds
                .lo
                .retain(|v| !v.sym.clone().is_some_and(|s| len_overlaps(s, written)));
            bounds
                .hi
                .retain(|v| !v.sym.clone().is_some_and(|s| len_overlaps(s, written)));
        }
    }
}

fn len_overlaps(sym: Sym, written: &PlaceKey) -> bool {
    match sym {
        Sym::Len(key) => covers(written, &key),
        _ => false,
    }
}

fn len_overlaps_overlapping(sym: Sym, written: &PlaceKey) -> bool {
    match sym {
        Sym::Len(key) => overlaps(&key, written),
        _ => false,
    }
}

/// Whether assigning or exclusively borrowing `written` can change `len(place)`.
/// Only a write to the container itself, or to a prefix of it, can.
fn covers(written: &PlaceKey, place: &PlaceKey) -> bool {
    written.base == place.base && is_prefix(&written.proj, &place.proj)
}

fn invert(op: BinOp) -> BinOp {
    match op {
        BinOp::Lt => BinOp::Ge,
        BinOp::Le => BinOp::Gt,
        BinOp::Gt => BinOp::Le,
        BinOp::Ge => BinOp::Lt,
        BinOp::Eq => BinOp::Ne,
        BinOp::Ne => BinOp::Eq,
        other => other,
    }
}

/// Put a comparison into `<=` form by swapping its sides when needed.
fn normalize(op: BinOp, lhs: Lx, rhs: Lx) -> (BinOp, Lx, Lx) {
    match op {
        BinOp::Gt => (BinOp::Lt, rhs, lhs),
        BinOp::Ge => (BinOp::Le, rhs, lhs),
        BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le => (op, lhs, rhs),
        other => (other, lhs, rhs),
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Sym {
    Param(DefId),
    Len(PlaceKey),
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Val {
    sym: Option<Sym>,
    off: i128,
}

impl Val {
    fn constant(value: i128) -> Val {
        Val {
            sym: None,
            off: value,
        }
    }

    fn symbol(sym: Sym, off: i128) -> Val {
        Val {
            sym: Some(sym),
            off,
        }
    }

    fn add(self, offset: i128) -> Val {
        Val {
            sym: self.sym,
            off: self.off.saturating_add(offset),
        }
    }
}

/// The lower and upper bounds known for one integer local. Every entry is a valid
/// bound; a path may know more than one at a time (`i <= 1000` and `i < n`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Bounds {
    lo: BTreeSet<Val>,
    hi: BTreeSet<Val>,
}

impl Bounds {
    fn exact(value: Val) -> Bounds {
        let mut lo = BTreeSet::new();
        lo.insert(value.clone());
        let mut hi = BTreeSet::new();
        hi.insert(value);
        Bounds { lo, hi }
    }

    fn normalized(mut self, range: Bounds) -> Bounds {
        self.lo.extend(range.lo);
        self.hi.extend(range.hi);
        self
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct State {
    locals: BTreeMap<Local, Bounds>,
    lens: BTreeMap<PlaceKey, Bounds>,
    avail: BTreeSet<CheckKey>,
}

impl State {
    fn seed(cfg: &Cfg, decls: &Declarations) -> State {
        let mut state = State::default();
        for (index, _) in cfg.locals.iter().enumerate() {
            let local = index as Local;
            if let TyKind::Int(int) = decls.types.kind(cfg.local(local).ty) {
                state.locals.insert(
                    local,
                    Bounds {
                        lo: BTreeSet::from([Val::constant(IntTy::min(*int))]),
                        hi: BTreeSet::from([Val::constant(IntTy::max(*int))]),
                    },
                );
            }
        }
        state
    }

    fn type_range(cfg: &Cfg, decls: &Declarations, local: Local) -> Bounds {
        if let TyKind::Int(int) = decls.types.kind(cfg.local(local).ty) {
            Bounds {
                lo: BTreeSet::from([Val::constant(IntTy::min(*int))]),
                hi: BTreeSet::from([Val::constant(IntTy::max(*int))]),
            }
        } else {
            Bounds::default()
        }
    }

    /// The type's range, or nothing for a non-integer.
    fn type_bounds(&self, cfg: &Cfg, decls: &Declarations, local: Local) -> Bounds {
        State::type_range(cfg, decls, local)
    }

    fn add_lower(&mut self, local: Local, value: Val, range: Bounds) {
        let entry = self.locals.entry(local).or_insert(range);
        entry.lo.insert(value);
    }

    fn add_upper(&mut self, local: Local, value: Val, range: Bounds) {
        let entry = self.locals.entry(local).or_insert(range);
        entry.hi.insert(value);
    }

    fn set_exact(&mut self, local: Local, value: Val) {
        self.locals.insert(local, Bounds::exact(value));
    }

    fn add_len_lower(&mut self, key: PlaceKey, value: Val) {
        self.lens.entry(key).or_default().lo.insert(value);
    }

    fn add_len_upper(&mut self, key: PlaceKey, value: Val) {
        self.lens.entry(key).or_default().hi.insert(value);
    }

    fn join(&self, other: &State) -> State {
        let mut locals = BTreeMap::new();
        for (local, a) in &self.locals {
            let b = other.locals.get(local).cloned().unwrap_or_default();
            let lo: BTreeSet<Val> = a.lo.intersection(&b.lo).cloned().collect();
            let hi: BTreeSet<Val> = a.hi.intersection(&b.hi).cloned().collect();
            if !lo.is_empty() || !hi.is_empty() {
                locals.insert(*local, Bounds { lo, hi });
            }
        }
        let mut lens = BTreeMap::new();
        for (key, a) in &self.lens {
            let b = other.lens.get(key).cloned().unwrap_or_default();
            let lo: BTreeSet<Val> = a.lo.intersection(&b.lo).cloned().collect();
            let hi: BTreeSet<Val> = a.hi.intersection(&b.hi).cloned().collect();
            if !lo.is_empty() || !hi.is_empty() {
                lens.insert(key.clone(), Bounds { lo, hi });
            }
        }
        let avail = self
            .avail
            .iter()
            .filter(|check| other.avail.contains(*check))
            .cloned()
            .collect();
        State {
            locals,
            lens,
            avail,
        }
    }
}

/// `a op b` for two constants: `None` for an operator rule 5 does not fold.
fn constant_compare(op: BinOp, a: i128, b: i128) -> Option<bool> {
    Some(match op {
        BinOp::Lt => a < b,
        BinOp::Le => a <= b,
        BinOp::Gt => a > b,
        BinOp::Ge => a >= b,
        BinOp::Eq => a == b,
        BinOp::Ne => a != b,
        _ => return None,
    })
}

fn key_ty(key: &PlaceKey, cfg: &Cfg, decls: &Declarations) -> Option<TypeId> {
    let mut ty = match key.base {
        BaseKey::Local(local) => cfg.local(local).ty,
        BaseKey::Static(def) => match decls.items.get(&def) {
            Some(ItemSig::Static(s)) => s.ty,
            _ => return None,
        },
    };
    for proj in &key.proj {
        ty = match proj {
            ProjKey::Field(index) => {
                let TyKind::Adt(def, args) = decls.types.kind(ty) else {
                    return None;
                };
                if !args.is_empty() {
                    return None;
                }
                match decls.items.get(def) {
                    Some(ItemSig::Struct(s)) => s.fields.get(*index)?.ty,
                    _ => return None,
                }
            }
            ProjKey::TupleField(index) => match decls.types.kind(ty) {
                TyKind::Tuple(fields) => *fields.get(*index)?,
                _ => return None,
            },
            ProjKey::Index => match decls.types.kind(ty) {
                TyKind::Array(element, _) | TyKind::Slice(_, element) => *element,
                _ => return None,
            },
            ProjKey::Deref | ProjKey::DerefRaw => match decls.types.kind(ty) {
                TyKind::Ref(_, inner) | TyKind::RawPtr(_, inner) | TyKind::Slice(_, inner) => {
                    *inner
                }
                _ => return None,
            },
            ProjKey::Payload(_) => return None,
        };
    }
    Some(ty)
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct PlaceKey {
    base: BaseKey,
    proj: Vec<ProjKey>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum BaseKey {
    Local(Local),
    Static(DefId),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum ProjKey {
    Field(usize),
    TupleField(usize),
    Index,
    Deref,
    DerefRaw,
    Payload(usize),
}

fn place_key(place: &Place) -> PlaceKey {
    PlaceKey {
        base: base_key(place),
        proj: place
            .proj
            .iter()
            .map(|p| match p {
                Projection::Field { index, .. } => ProjKey::Field(*index),
                Projection::TupleField(index) => ProjKey::TupleField(*index),
                Projection::Index(_) => ProjKey::Index,
                Projection::Deref => ProjKey::Deref,
                Projection::DerefRaw => ProjKey::DerefRaw,
                Projection::Payload { variant } => ProjKey::Payload(*variant),
            })
            .collect(),
    }
}

fn base_key(place: &Place) -> BaseKey {
    match place.base {
        PlaceBase::Local(local) => BaseKey::Local(local),
        PlaceBase::Static(def) => BaseKey::Static(def),
    }
}

fn overlaps(a: &PlaceKey, b: &PlaceKey) -> bool {
    a.base == b.base && (is_prefix(&a.proj, &b.proj) || is_prefix(&b.proj, &a.proj))
}

fn is_prefix(a: &[ProjKey], b: &[ProjKey]) -> bool {
    a.len() <= b.len() && a.iter().zip(b).all(|(x, y)| x == y)
}

fn two_phase_borrows(cfg: &Cfg) -> BTreeMap<Local, Place> {
    let mut out = BTreeMap::new();
    let mut assigned: BTreeMap<Local, Rvalue> = BTreeMap::new();
    for block in &cfg.blocks {
        for stmt in &block.stmts {
            let StmtKind::Assign { place, value } = &stmt.kind else {
                continue;
            };
            if !place.proj.is_empty() {
                continue;
            }
            let Some(local) = place.root() else {
                continue;
            };
            assigned.insert(local, value.clone());
            match value {
                Rvalue::Borrow {
                    place,
                    mutability: Mutability::Mut,
                    two_phase: true,
                }
                | Rvalue::Reborrow {
                    place,
                    two_phase: true,
                } => {
                    let target = resolve_borrow_place(place, &out);
                    out.insert(local, target);
                }
                Rvalue::Slice {
                    base,
                    borrow: Some(Mutability::Mut),
                    two_phase: true,
                    ..
                } => {
                    let target = resolve_borrow_place(base, &out);
                    out.insert(local, target);
                }
                _ => {}
            }
        }
    }
    // `let b = &mut i; call(b)` names the reservation through a user local.
    let mut changed = true;
    while changed {
        changed = false;
        for (local, value) in &assigned {
            if out.contains_key(local) {
                continue;
            }
            if let Rvalue::Use(Operand::Place(src, _)) = value
                && src.proj.is_empty()
                && let Some(root) = src.root()
                && let Some(place) = out.get(&root).cloned()
            {
                out.insert(*local, place);
                changed = true;
            }
        }
    }
    out
}

/// A reborrow of a reservation is the reservation of the original place.
fn resolve_borrow_place(place: &Place, known: &BTreeMap<Local, Place>) -> Place {
    if place.proj.is_empty()
        && let Some(root) = place.root()
        && let Some(target) = known.get(&root)
    {
        return target.clone();
    }
    let PlaceBase::Local(root) = place.base else {
        return place.clone();
    };
    let Some(target) = known.get(&root) else {
        return place.clone();
    };
    let mut out = target.clone();
    let rest = match place.proj.split_first() {
        Some((Projection::Deref | Projection::DerefRaw, rest)) => rest,
        _ => &place.proj[..],
    };
    out.proj.extend_from_slice(rest);
    out
}

fn two_phase_arg(
    op: &Operand,
    defs: &BTreeMap<Local, Rvalue>,
    two_phase: &BTreeMap<Local, Place>,
) -> Option<Place> {
    let Operand::Place(place, _) = op else {
        return None;
    };
    if !place.proj.is_empty() {
        return None;
    }
    let mut local = place.root()?;
    for _ in 0..32 {
        if let Some(found) = two_phase.get(&local) {
            return Some(found.clone());
        }
        match defs.get(&local) {
            Some(Rvalue::Use(Operand::Place(src, _)))
                if src.proj.is_empty() && src.root().is_some_and(|next| next != local) =>
            {
                local = src.root().unwrap_or(local);
            }
            _ => return None,
        }
    }
    None
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct CheckKey {
    tag: CheckTag,
    place: Option<PlaceKey>,
    lo: Option<OpKey>,
    hi: Option<OpKey>,
    operands: Vec<OpKey>,
}

impl CheckKey {
    fn mentions(&self, local: Local) -> bool {
        self.place
            .as_ref()
            .is_some_and(|p| p.base == BaseKey::Local(local))
            || self.lo.as_ref().is_some_and(|o| o.mentions(local))
            || self.hi.as_ref().is_some_and(|o| o.mentions(local))
            || self.operands.iter().any(|o| o.mentions(local))
    }

    fn place_overlaps(&self, written: &PlaceKey) -> bool {
        self.place.as_ref().is_some_and(|p| overlaps(p, written))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum OpKey {
    Const(i128, TypeId),
    Param(DefId),
    Place(PlaceKey),
    SliceLen(PlaceKey),
    Binary(u8, Box<OpKey>, Box<OpKey>),
    Unary(u8, Box<OpKey>),
    Temp(Local),
    Unknown,
}

impl OpKey {
    fn mentions(&self, local: Local) -> bool {
        match self {
            OpKey::Place(key) | OpKey::SliceLen(key) => key.base == BaseKey::Local(local),
            OpKey::Binary(_, a, b) => a.mentions(local) || b.mentions(local),
            OpKey::Unary(_, a) => a.mentions(local),
            OpKey::Temp(other) => *other == local,
            _ => false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum CheckTag {
    BoundsIndex,
    BoundsSlice,
    BoundsVector(u32, bool),
    Overflow,
    Division,
    Shift,
    Narrow,
    Gather,
    Scatter,
    Dispatch,
}

fn check_tag(kind: CheckKind) -> CheckTag {
    match kind {
        CheckKind::Bounds(BoundsKind::Index) => CheckTag::BoundsIndex,
        CheckKind::Bounds(BoundsKind::Slice) => CheckTag::BoundsSlice,
        CheckKind::Bounds(BoundsKind::Vector { width, tail }) => {
            CheckTag::BoundsVector(width, tail)
        }
        CheckKind::Overflow => CheckTag::Overflow,
        CheckKind::Division => CheckTag::Division,
        CheckKind::Shift => CheckTag::Shift,
        CheckKind::Narrow => CheckTag::Narrow,
        CheckKind::Gather => CheckTag::Gather,
        CheckKind::ScatterDistinct => CheckTag::Scatter,
        CheckKind::DispatchCover => CheckTag::Dispatch,
    }
}

fn arithmetic(op: BinOp) -> bool {
    matches!(
        op,
        BinOp::Add
            | BinOp::Sub
            | BinOp::Mul
            | BinOp::Div
            | BinOp::Rem
            | BinOp::AddWrap
            | BinOp::SubWrap
            | BinOp::MulWrap
            | BinOp::AddSat
            | BinOp::SubSat
            | BinOp::MulSat
            | BinOp::BitAnd
            | BinOp::BitOr
            | BinOp::BitXor
            | BinOp::Shl
            | BinOp::Shr
    )
}

fn bin_tag(op: BinOp) -> u8 {
    match op {
        BinOp::Add => 0,
        BinOp::Sub => 1,
        BinOp::Mul => 2,
        BinOp::Div => 3,
        BinOp::Rem => 4,
        BinOp::AddWrap => 5,
        BinOp::SubWrap => 6,
        BinOp::MulWrap => 7,
        BinOp::AddSat => 8,
        BinOp::SubSat => 9,
        BinOp::MulSat => 10,
        BinOp::BitAnd => 11,
        BinOp::BitOr => 12,
        BinOp::BitXor => 13,
        BinOp::Shl => 14,
        BinOp::Shr => 15,
        BinOp::Eq => 16,
        BinOp::Ne => 17,
        BinOp::Lt => 18,
        BinOp::Le => 19,
        BinOp::Gt => 20,
        BinOp::Ge => 21,
        BinOp::And => 22,
        BinOp::Or => 23,
    }
}

fn un_tag(op: UnOp) -> u8 {
    match op {
        UnOp::Neg => 0,
        UnOp::NegWrap => 1,
        UnOp::Not => 2,
        UnOp::BitNot => 3,
        UnOp::Borrow => 4,
        UnOp::BorrowMut => 5,
        UnOp::Try => 6,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Lx {
    Val(Val),
    Local(Local, i128),
}

impl Lx {
    fn add(self, offset: i128) -> Lx {
        match self {
            Lx::Val(v) => Lx::Val(v.add(offset)),
            Lx::Local(local, o) => Lx::Local(local, o.saturating_add(offset)),
        }
    }
}

#[derive(Clone, Debug, Default)]
struct Step {
    out: State,
    edges: Vec<(Block, State)>,
    /// Successor slots a constant branch does not take (rule 5).
    skipped: Vec<usize>,
    items: Vec<(usize, CostKind, Span)>,
    diagnostics: Vec<Diagnostic>,
    removed: Vec<usize>,
}

#[derive(Clone, Debug)]
enum Decision {
    Keep(Option<CostKind>),
    Remove,
    Trap(String),
}
