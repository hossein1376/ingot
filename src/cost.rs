//! Phase 10: delete the checks §31 requires the compiler to remove, and report
//! the ones that survive for `ingot cost`.
//!
//! The pass operates on the CFG produced by phase 8. It carries length lower
//! bounds from `@assert(xs.len >= n)` and counted-loop ranges from
//! `for i in 0..xs.len`, then removes bounds checks those facts prove. Constant
//! operands remove overflow, division and shift checks.

use crate::ast::BinOp;
use crate::cfg::{
    BlockId, Cfg, CfgStmt, CheckKind, LenFact, LocalId, LoopFact, Operand, Rvalue, Term,
};
use crate::check::{ConstVal, Program};
use crate::source::Span;
use crate::types::Type;
use std::collections::{BTreeMap, HashMap, HashSet};

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
    pub file: String,
    pub span: Span,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Facts {
    /// A lower bound on `len(base)`, proven by an `@assert`.
    len_lb: BTreeMap<LocalId, i64>,
    /// A loop counter known to satisfy `0 <= counter < len(base)`.
    counter_base: BTreeMap<LocalId, LocalId>,
    /// Known integer ranges, intersected at joins.
    ranges: BTreeMap<LocalId, (i128, i128)>,
    /// `local < len(base)`, from a branch or `@assert`.
    len_upper: BTreeMap<LocalId, LocalId>,
    /// A temporary holding `len(base)`.
    len_value: BTreeMap<LocalId, LocalId>,
    /// Identical checks that dominate the current point, with their inputs.
    available: BTreeMap<String, Vec<LocalId>>,
}

impl Facts {
    fn merge(&mut self, other: &Facts) -> bool {
        let before = self.clone();
        let mut len_lb = BTreeMap::new();
        for (k, v) in &self.len_lb {
            if let Some(w) = other.len_lb.get(k) {
                len_lb.insert(*k, (*v).min(*w));
            }
        }
        self.len_lb = len_lb;
        self.counter_base
            .retain(|k, v| other.counter_base.get(k) == Some(v));
        self.len_upper
            .retain(|k, v| other.len_upper.get(k) == Some(v));
        self.len_value
            .retain(|k, v| other.len_value.get(k) == Some(v));
        self.available
            .retain(|k, v| other.available.get(k) == Some(v));
        let mut ranges = BTreeMap::new();
        for (k, (lo, hi)) in &self.ranges {
            if let Some((lo2, hi2)) = other.ranges.get(k) {
                ranges.insert(*k, ((*lo).max(*lo2), (*hi).min(*hi2)));
            }
        }
        self.ranges = ranges;
        *self != before
    }
}

fn merge_into(states: &mut HashMap<BlockId, Facts>, block: BlockId, facts: &Facts) -> bool {
    match states.get_mut(&block) {
        Some(existing) => existing.merge(facts),
        None => {
            states.insert(block, facts.clone());
            true
        }
    }
}

fn successors(term: &Term) -> Vec<BlockId> {
    match term {
        Term::Goto(b) => vec![*b],
        Term::Branch { then_b, else_b, .. } => vec![*then_b, *else_b],
        Term::Switch { cases, default, .. } => {
            let mut out: Vec<BlockId> = cases.iter().map(|(_, b)| *b).collect();
            out.push(*default);
            out
        }
        Term::Call { then_b, .. } | Term::Spawn { then_b, .. } => vec![*then_b],
        Term::ScopeWait { then_b, .. } => vec![*then_b],
        Term::Return { .. } | Term::Trap { .. } | Term::Unreachable => Vec::new(),
    }
}

fn statement_facts(len_facts: &[LenFact], block: BlockId, index: usize, facts: &mut Facts) {
    for f in len_facts {
        if f.block == block && f.after <= index {
            let e = facts.len_lb.entry(f.base).or_insert(i64::MIN);
            *e = (*e).max(f.at_least);
        }
    }
    let _ = index;
}

fn range_facts(
    range_facts: &[crate::cfg::RangeFact],
    block: BlockId,
    index: usize,
    facts: &mut Facts,
) {
    for f in range_facts {
        if f.block != block || f.after > index {
            continue;
        }
        let e = facts
            .ranges
            .entry(f.local)
            .or_insert((i128::MIN, i128::MAX));
        if let Some(lo) = f.lo {
            e.0 = e.0.max(lo as i128);
        }
        if let Some(hi) = f.hi {
            e.1 = e.1.min(hi as i128);
        }
    }
}

fn type_range(ty: &Type) -> Option<(i128, i128)> {
    let Type::Int(s) = ty else {
        return None;
    };
    let bits = s.int_bits()?;
    if s.is_signed() {
        Some((-(1i128 << (bits - 1)), (1i128 << (bits - 1)) - 1))
    } else {
        Some((0, (1i128 << bits) - 1))
    }
}

fn initialize_ranges(cfg: &Cfg, facts: &mut Facts) {
    for (i, local) in cfg.locals.iter().enumerate() {
        if let Some(r) = type_range(&local.ty) {
            facts.ranges.insert(i, r);
        }
    }
}

fn operand_deps(op: &Operand, deps: &mut Vec<LocalId>) {
    if let Operand::Place(p) = op {
        deps.push(p.local);
    }
}

fn check_key(check: &crate::cfg::CheckStmt) -> (String, Vec<LocalId>) {
    let mut deps = Vec::new();
    let mut key = format!("{:?}", check.kind);
    if let Some(b) = &check.bounds {
        key.push_str(&format!("|{:?}|{:?}|{}", b.base, b.index, b.width));
        deps.push(b.base.local);
        operand_deps(&b.index, &mut deps);
    }
    if let Some(lhs) = &check.lhs {
        key.push_str(&format!("|{:?}", lhs));
        operand_deps(lhs, &mut deps);
    }
    if let Some(rhs) = &check.rhs {
        key.push_str(&format!("|{:?}", rhs));
        operand_deps(rhs, &mut deps);
    }
    deps.sort_unstable();
    deps.dedup();
    (key, deps)
}

fn invalidate_local(facts: &mut Facts, local: LocalId) {
    facts.available.retain(|_, deps| !deps.contains(&local));
}

fn transfer_available(stmt: &CfgStmt, facts: &mut Facts) {
    match stmt {
        CfgStmt::Check { check } => {
            let (key, deps) = check_key(check);
            facts.available.entry(key).or_insert(deps);
        }
        CfgStmt::Assign { place, .. } => invalidate_local(facts, place.local),
        CfgStmt::StorageStart { local, .. } | CfgStmt::StorageEnd { local, .. } => {
            invalidate_local(facts, *local);
        }
        _ => {}
    }
}

fn invalidate_term(term: &Term, facts: &mut Facts) {
    match term {
        Term::Call { args, dest, .. } | Term::Spawn { args, dest, .. } => {
            for a in args {
                if let Operand::Place(p) = a {
                    invalidate_local(facts, p.local);
                }
            }
            if let Some(d) = dest {
                invalidate_local(facts, d.local);
            }
        }
        Term::Branch {
            cond: Operand::Place(p),
            ..
        } => invalidate_local(facts, p.local),
        Term::Branch { .. } => {}
        Term::Switch { value, .. } | Term::ScopeWait { scope: value, .. } => {
            invalidate_local(facts, value.local);
        }
        _ => {}
    }
}

fn transfer_ranges(local_ranges: &[Option<(i128, i128)>], stmt: &CfgStmt, facts: &mut Facts) {
    let CfgStmt::Assign { place, value, .. } = stmt else {
        return;
    };
    if !place.proj.is_empty() {
        return;
    }
    let dest = place.local;
    match value {
        Rvalue::Use {
            place: src,
            mode: _,
        } if !src.proj.is_empty() => {
            if src.proj.len() == 1
                && matches!(&src.proj[0], crate::cfg::Proj::Field(n) if n == "len")
            {
                facts.len_value.insert(dest, src.local);
            }
        }
        Rvalue::Use {
            place: src,
            mode: _,
        } if src.proj.is_empty() => {
            if let Some(r) = facts.ranges.get(&src.local).copied() {
                facts.ranges.insert(dest, r);
            }
            if let Some(base) = facts.len_upper.get(&src.local).copied() {
                facts.len_upper.insert(dest, base);
            }
            if let Some(base) = facts.counter_base.get(&src.local).copied() {
                facts.counter_base.insert(dest, base);
            }
            if let Some(base) = facts.len_value.get(&src.local).copied() {
                facts.len_value.insert(dest, base);
            }
        }
        Rvalue::Cast { operand } => {
            if let Operand::Place(src) = operand
                && src.proj.is_empty()
                && let Some(r) = facts
                    .ranges
                    .get(&src.local)
                    .copied()
                    .or_else(|| local_ranges.get(src.local).copied().flatten())
            {
                facts.ranges.insert(dest, r);
            }
        }
        Rvalue::Constant => {
            if let Some(r) = local_ranges.get(dest).copied().flatten() {
                facts.ranges.insert(dest, r);
            }
        }
        _ => {}
    }
}

fn loop_facts_at(loop_facts: &[LoopFact], block: BlockId, facts: &mut Facts) {
    for f in loop_facts {
        if f.body != block {
            continue;
        }
        if let Some(base) = f.base {
            facts.counter_base.insert(f.counter, base);
        }
    }
}

fn operand_int(op: &Operand) -> Option<i128> {
    match op {
        Operand::Const(ConstVal::Int(n)) => Some(*n),
        _ => None,
    }
}

fn proven_bounds(check: &crate::cfg::CheckStmt, facts: &Facts) -> bool {
    let Some(bounds) = &check.bounds else {
        return false;
    };
    let base = bounds.base.local;
    if let Some(upper) = &bounds.upper {
        let lo_ok = facts
            .ranges
            .get(&match &bounds.index {
                Operand::Place(p) if p.proj.is_empty() => p.local,
                _ => usize::MAX,
            })
            .map(|(lo, _)| *lo >= 0)
            .unwrap_or(false)
            || operand_int(&bounds.index).map(|n| n >= 0).unwrap_or(false)
            || matches!(&bounds.index, Operand::Place(p) if p.proj.is_empty() && facts.counter_base.contains_key(&p.local));
        let hi_ok = if let Some(hi) = operand_int(upper) {
            facts
                .len_lb
                .get(&base)
                .map(|lb| *lb as i128 >= hi)
                .unwrap_or(false)
        } else if let Operand::Place(p) = upper {
            p.proj.is_empty() && facts.len_upper.get(&p.local) == Some(&base)
        } else {
            false
        };
        return lo_ok && hi_ok;
    }
    if let Some(n) = operand_int(&bounds.index) {
        if n < 0 {
            return false;
        }
        return facts
            .len_lb
            .get(&base)
            .map(|lb| (n + bounds.width as i128) <= *lb as i128)
            .unwrap_or(false);
    }
    if let Operand::Place(index) = &bounds.index
        && index.proj.is_empty()
    {
        if facts.counter_base.get(&index.local) == Some(&base) {
            return true;
        }
        if facts.len_upper.get(&index.local) == Some(&base)
            && bounds.width <= 1
            && facts
                .ranges
                .get(&index.local)
                .map(|(lo, _)| *lo >= 0)
                .unwrap_or(false)
        {
            return true;
        }
    }
    false
}

fn operand_range(op: &Operand, facts: &Facts) -> Option<(i128, i128)> {
    match op {
        Operand::Const(ConstVal::Int(n)) => Some((*n, *n)),
        Operand::Place(p) if p.proj.is_empty() => facts.ranges.get(&p.local).copied(),
        _ => None,
    }
}

fn proven_overflow(check: &crate::cfg::CheckStmt, facts: &Facts) -> bool {
    let (Some(lhs), Some(rhs), Some(op), Some(ty)) =
        (&check.lhs, &check.rhs, check.op, check.ty.as_ref())
    else {
        return false;
    };
    let Some((amin, amax)) = operand_range(lhs, facts) else {
        return false;
    };
    let Some((bmin, bmax)) = operand_range(rhs, facts) else {
        return false;
    };
    let (rmin, rmax) = match op {
        BinOp::Add => (amin + bmin, amax + bmax),
        BinOp::Sub => (amin - bmax, amax - bmin),
        BinOp::Mul => {
            let candidates = [amin * bmin, amin * bmax, amax * bmin, amax * bmax];
            (
                *candidates.iter().min().unwrap(),
                *candidates.iter().max().unwrap(),
            )
        }
        _ => return false,
    };
    type_range(ty)
        .map(|(lo, hi)| rmin >= lo && rmax <= hi)
        .unwrap_or(false)
}

fn proven_constant(check: &crate::cfg::CheckStmt, facts: &Facts) -> bool {
    let (Some(lhs), Some(rhs)) = (&check.lhs, &check.rhs) else {
        return false;
    };
    match check.kind {
        CheckKind::Overflow => {
            (operand_int(lhs).is_some() && operand_int(rhs).is_some())
                || proven_overflow(check, facts)
        }
        CheckKind::Division => operand_int(rhs).map(|n| n != 0).unwrap_or(false),
        CheckKind::Shift => {
            // Without the left operand's width in the check record, only a
            // zero count is universally safe.
            operand_int(rhs) == Some(0)
        }
        CheckKind::Bounds => proven_bounds(check, facts),
        CheckKind::Narrow => false,
    }
}

fn comparison_def<'a>(
    cfg: &'a Cfg,
    block: BlockId,
    cond: &Operand,
) -> Option<(BinOp, &'a Operand, &'a Operand)> {
    for stmt in cfg.blocks[block].stmts.iter().rev() {
        let CfgStmt::Assign { place, value, .. } = stmt else {
            continue;
        };
        if place.proj.is_empty()
            && place.local == cond_local(cond)?
            && let Rvalue::Binary { op, lhs, rhs } = value
        {
            return Some((*op, lhs, rhs));
        }
    }
    None
}

fn cond_local(op: &Operand) -> Option<LocalId> {
    match op {
        Operand::Place(p) if p.proj.is_empty() => Some(p.local),
        _ => None,
    }
}

fn tighten_range(facts: &mut Facts, local: LocalId, lo: Option<i128>, hi: Option<i128>) {
    let entry = facts.ranges.entry(local).or_insert((i128::MIN, i128::MAX));
    if let Some(lo) = lo {
        entry.0 = entry.0.max(lo);
    }
    if let Some(hi) = hi {
        entry.1 = entry.1.min(hi);
    }
}

fn len_of(op: &Operand, facts: &Facts) -> Option<LocalId> {
    let Operand::Place(p) = op else {
        return None;
    };
    if p.proj.len() == 1 && matches!(&p.proj[0], crate::cfg::Proj::Field(n) if n == "len") {
        return Some(p.local);
    }
    if p.proj.is_empty() {
        return facts.len_value.get(&p.local).copied();
    }
    None
}

fn apply_comparison(facts: &mut Facts, op: BinOp, lhs: &Operand, rhs: &Operand, truth: bool) {
    if let (Some(left), Some(right)) = (cond_local(lhs), operand_int(rhs)) {
        let (lo, hi) = match (op, truth) {
            (BinOp::Lt, true) => (None, Some(right - 1)),
            (BinOp::Lt, false) => (Some(right), None),
            (BinOp::Le, true) => (None, Some(right)),
            (BinOp::Le, false) => (Some(right + 1), None),
            (BinOp::Gt, true) => (Some(right + 1), None),
            (BinOp::Gt, false) => (None, Some(right)),
            (BinOp::Ge, true) => (Some(right), None),
            (BinOp::Ge, false) => (None, Some(right - 1)),
            (BinOp::Eq, true) => (Some(right), Some(right)),
            (BinOp::Ne, false) => (Some(right), Some(right)),
            _ => (None, None),
        };
        tighten_range(facts, left, lo, hi);
        return;
    }
    if let (Some(right), Some(left)) = (cond_local(rhs), operand_int(lhs)) {
        let (lo, hi) = match (op, truth) {
            (BinOp::Lt, true) => (Some(left + 1), None), // left < right
            (BinOp::Lt, false) => (None, Some(left)),
            (BinOp::Le, true) => (Some(left), None),
            (BinOp::Le, false) => (None, Some(left - 1)),
            (BinOp::Gt, true) => (None, Some(left)),
            (BinOp::Gt, false) => (Some(left + 1), None),
            (BinOp::Ge, true) => (None, Some(left - 1)),
            (BinOp::Ge, false) => (Some(left), None),
            _ => (None, None),
        };
        tighten_range(facts, right, lo, hi);
        return;
    }
    if truth
        && matches!(op, BinOp::Lt | BinOp::Le)
        && let Some(index) = cond_local(lhs)
        && let Some(base) = len_of(rhs, facts)
    {
        facts.len_upper.insert(index, base);
    }
}

fn copy_edges(cfg: &Cfg, block: BlockId) -> Vec<(LocalId, LocalId)> {
    let mut out = Vec::new();
    for stmt in &cfg.blocks[block].stmts {
        if let CfgStmt::Assign { place, value, .. } = stmt
            && place.proj.is_empty()
            && let Rvalue::Use {
                place: src,
                mode: crate::cfg::UseMode::Copy,
            } = value
            && src.proj.is_empty()
        {
            out.push((place.local, src.local));
        }
    }
    out
}

fn propagate_copies(facts: &mut Facts, edges: &[(LocalId, LocalId)]) {
    loop {
        let mut changed = false;
        for (dest, src) in edges {
            let a = facts.ranges.get(dest).copied();
            let b = facts.ranges.get(src).copied();
            if let (Some((alo, ahi)), Some((blo, bhi))) = (a, b) {
                let merged = (alo.max(blo), ahi.min(bhi));
                if merged != (alo, ahi) {
                    facts.ranges.insert(*dest, merged);
                    changed = true;
                }
                if merged != (blo, bhi) {
                    facts.ranges.insert(*src, merged);
                    changed = true;
                }
            } else if let Some(r) = a {
                facts.ranges.insert(*src, r);
                changed = true;
            } else if let Some(r) = b {
                facts.ranges.insert(*dest, r);
                changed = true;
            }
            if let Some(base) = facts.len_upper.get(dest).copied()
                && facts.len_upper.insert(*src, base) != Some(base)
            {
                changed = true;
            }
            if let Some(base) = facts.len_upper.get(src).copied()
                && facts.len_upper.insert(*dest, base) != Some(base)
            {
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
}

fn facts_for_branch(
    cfg: &Cfg,
    block: BlockId,
    cond: &Operand,
    truth: bool,
    facts: &Facts,
) -> Facts {
    let mut out = facts.clone();
    if let Some((op, lhs, rhs)) = comparison_def(cfg, block, cond) {
        apply_comparison(&mut out, op, lhs, rhs, truth);
    }
    propagate_copies(&mut out, &copy_edges(cfg, block));
    out
}

/// Run phase 10 on one CFG: delete checks proven not to fail.
pub fn remove_proven_checks(cfg: &mut Cfg) {
    let mut entries: HashMap<BlockId, Facts> = HashMap::new();
    let local_ranges: Vec<Option<(i128, i128)>> =
        cfg.locals.iter().map(|l| type_range(&l.ty)).collect();
    let mut entry_facts = Facts::default();
    initialize_ranges(cfg, &mut entry_facts);
    entries.insert(cfg.entry, entry_facts);
    let mut queue = vec![cfg.entry];
    let mut queued: HashSet<BlockId> = HashSet::from([cfg.entry]);
    while let Some(b) = queue.pop() {
        queued.remove(&b);
        let Some(entry) = entries.get(&b).cloned() else {
            continue;
        };
        let mut facts = entry;
        loop_facts_at(&cfg.loop_facts, b, &mut facts);
        let block = &cfg.blocks[b];
        for (i, stmt) in block.stmts.iter().enumerate() {
            statement_facts(&cfg.len_facts, b, i, &mut facts);
            range_facts(&cfg.range_facts, b, i, &mut facts);
            if let CfgStmt::StorageStart { local, .. } = stmt
                && let Some(r) = type_range(&cfg.locals[*local].ty)
            {
                facts.ranges.insert(*local, r);
            }
            transfer_ranges(&local_ranges, stmt, &mut facts);
            transfer_available(stmt, &mut facts);
        }
        invalidate_term(&block.term, &mut facts);
        if let Term::Branch {
            cond,
            then_b,
            else_b,
            ..
        } = &block.term
        {
            let then_facts = facts_for_branch(cfg, b, cond, true, &facts);
            let else_facts = facts_for_branch(cfg, b, cond, false, &facts);
            if merge_into(&mut entries, *then_b, &then_facts) && queued.insert(*then_b) {
                queue.push(*then_b);
            }
            if merge_into(&mut entries, *else_b, &else_facts) && queued.insert(*else_b) {
                queue.push(*else_b);
            }
        } else {
            for succ in successors(&block.term) {
                if merge_into(&mut entries, succ, &facts) && queued.insert(succ) {
                    queue.push(succ);
                }
            }
        }
    }

    let len_facts = cfg.len_facts.clone();
    let loop_facts = cfg.loop_facts.clone();
    let range_facts_snapshot = cfg.range_facts.clone();
    let local_ranges: Vec<Option<(i128, i128)>> =
        cfg.locals.iter().map(|l| type_range(&l.ty)).collect();
    for (b, block) in cfg.blocks.iter_mut().enumerate() {
        let mut facts = entries.get(&b).cloned().unwrap_or_default();
        loop_facts_at(&loop_facts, b, &mut facts);
        let mut kept = Vec::with_capacity(block.stmts.len());
        for (i, stmt) in std::mem::take(&mut block.stmts).into_iter().enumerate() {
            statement_facts(&len_facts, b, i, &mut facts);
            range_facts(&range_facts_snapshot, b, i, &mut facts);
            transfer_ranges(&local_ranges, &stmt, &mut facts);
            match &stmt {
                CfgStmt::Check { check } => {
                    let (key, _) = check_key(check);
                    let duplicate = facts.available.contains_key(&key);
                    let remove = duplicate
                        || if check.kind == CheckKind::Bounds {
                            proven_bounds(check, &facts)
                        } else {
                            proven_constant(check, &facts)
                        };
                    if !remove {
                        transfer_available(&stmt, &mut facts);
                        kept.push(stmt);
                    }
                }
                _ => {
                    transfer_available(&stmt, &mut facts);
                    kept.push(stmt);
                }
            }
        }
        block.stmts = kept;
    }
}

pub fn cost_items(program: &Program) -> Vec<CostItem> {
    let mut out = Vec::new();
    for (function, cfg) in &program.cfgs {
        // `ingot cost FILE` reports the program, not its dependencies.
        if program.package_name != "std" && (function.starts_with("std.") || function == "std") {
            continue;
        }
        for block in &cfg.blocks {
            for stmt in &block.stmts {
                let CfgStmt::Check { check } = stmt else {
                    continue;
                };
                let kind = match check.kind {
                    CheckKind::Bounds => CostKind::Bounds,
                    CheckKind::Overflow | CheckKind::Narrow => CostKind::Overflow,
                    CheckKind::Division => CostKind::Div,
                    CheckKind::Shift => CostKind::Shift,
                };
                out.push(CostItem {
                    kind,
                    function: function.clone(),
                    file: program
                        .module_ctx(&cfg.module)
                        .map(|m| m.file.clone())
                        .unwrap_or_default(),
                    span: check.span,
                });
            }
        }
    }
    out.sort_by(|a, b| {
        a.function
            .cmp(&b.function)
            .then(a.span.start.cmp(&b.span.start))
            .then(a.kind.name().cmp(b.kind.name()))
    });
    out
}

impl CostItem {
    pub fn to_json(&self, sources: &crate::source::Sources) -> String {
        let file = sources
            .iter()
            .find(|s| s.path == self.file)
            .or_else(|| sources.files.first())
            .map(|s| s.path.clone())
            .unwrap_or_default();
        let mut out = format!(
            "{{\"type\":\"cost\",\"kind\":\"{}\",\"fn\":\"{}\",\"span\":",
            self.kind.name(),
            json_escape(&self.function),
        );
        crate::diagnostic::write_span(&mut out, sources, &file, self.span);
        out.push('}');
        out
    }
}

fn json_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}
