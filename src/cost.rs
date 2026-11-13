//! Phase 10: delete the checks §31 requires the compiler to remove, and report
//! the ones that survive for `ingot cost`.
//!
//! The pass operates on the CFG produced by phase 8. It carries length lower
//! bounds from `@assert(xs.len >= n)` and counted-loop ranges from
//! `for i in 0..xs.len`, then removes bounds checks those facts prove. Constant
//! operands remove overflow, division and shift checks.

use crate::cfg::{BlockId, Cfg, CfgStmt, CheckKind, LenFact, LocalId, LoopFact, Operand, Term};
use crate::check::{ConstVal, Program};
use crate::source::Span;
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
    if let Some(n) = operand_int(&bounds.index) {
        if n < 0 {
            return false;
        }
        return facts
            .len_lb
            .get(&base)
            .map(|lb| n < *lb as i128)
            .unwrap_or(false);
    }
    if let Operand::Place(index) = &bounds.index
        && index.proj.is_empty()
    {
        return facts.counter_base.get(&index.local) == Some(&base);
    }
    false
}

fn proven_constant(check: &crate::cfg::CheckStmt) -> bool {
    let (Some(lhs), Some(rhs)) = (&check.lhs, &check.rhs) else {
        return false;
    };
    match check.kind {
        CheckKind::Overflow => operand_int(lhs).is_some() && operand_int(rhs).is_some(),
        CheckKind::Division => operand_int(rhs).map(|n| n != 0).unwrap_or(false),
        CheckKind::Shift => {
            // Without the left operand's width in the check record, only a
            // zero count is universally safe.
            operand_int(rhs) == Some(0)
        }
        CheckKind::Bounds => proven_bounds(check, &Facts::default()),
        CheckKind::Narrow => false,
    }
}

/// Run phase 10 on one CFG: delete checks proven not to fail.
pub fn remove_proven_checks(cfg: &mut Cfg) {
    let mut entries: HashMap<BlockId, Facts> = HashMap::new();
    entries.insert(cfg.entry, Facts::default());
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
        for i in 0..block.stmts.len() {
            statement_facts(&cfg.len_facts, b, i, &mut facts);
        }
        for succ in successors(&block.term) {
            if merge_into(&mut entries, succ, &facts) && queued.insert(succ) {
                queue.push(succ);
            }
        }
    }

    let len_facts = cfg.len_facts.clone();
    let loop_facts = cfg.loop_facts.clone();
    for (b, block) in cfg.blocks.iter_mut().enumerate() {
        let mut facts = entries.get(&b).cloned().unwrap_or_default();
        loop_facts_at(&loop_facts, b, &mut facts);
        let mut kept = Vec::with_capacity(block.stmts.len());
        for (i, stmt) in std::mem::take(&mut block.stmts).into_iter().enumerate() {
            statement_facts(&len_facts, b, i, &mut facts);
            match &stmt {
                CfgStmt::Check { check } => {
                    let remove = if check.kind == CheckKind::Bounds {
                        proven_bounds(check, &facts)
                    } else {
                        proven_constant(check)
                    };
                    if !remove {
                        kept.push(stmt);
                    }
                }
                _ => kept.push(stmt),
            }
        }
        block.stmts = kept;
    }
}

pub fn cost_items(program: &Program) -> Vec<CostItem> {
    let mut out = Vec::new();
    for (function, cfg) in &program.cfgs {
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
        let (file, line, col) = sources
            .iter()
            .find(|s| s.path == self.file)
            .or_else(|| sources.files.first())
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
