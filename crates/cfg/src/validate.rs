//! The structural validator of a CFG. The golden runner and the two conformance
//! sweeps run it; a failure is a test failure.
//!
//! It checks the rules of pipeline section 5 that are about structure, not meaning:
//!
//! * every block exists and has a terminator whose targets exist;
//! * every local a statement or terminator names is declared;
//! * no `defer` survives (the model has no such statement, so this holds by type);
//! * the storage start and the storage end of every local are paired on every path
//!   that leaves the function, and every local a statement or terminator uses is
//!   inside its storage there.

use crate::model::*;

/// All the structural problems of one CFG, in a deterministic order.
pub fn validate(cfg: &Cfg) -> Vec<String> {
    let mut errors = Vec::new();
    if cfg.blocks.is_empty() {
        errors.push("the CFG has no blocks".to_string());
        return errors;
    }
    if cfg.entry as usize >= cfg.blocks.len() {
        errors.push(format!("entry block {} does not exist", cfg.entry));
    }
    for (b, block) in cfg.blocks.iter().enumerate() {
        for stmt in &block.stmts {
            check_stmt(cfg, b, stmt, &mut errors);
        }
        check_term(cfg, b, &block.term, &mut errors);
    }
    check_storage(cfg, &mut errors);
    errors.sort();
    errors.dedup();
    errors
}

/// Validate every CFG, including that a `par for` names a CFG in the set.
pub fn validate_all(cfgs: &Cfgs) -> Vec<String> {
    let mut errors = Vec::new();
    for (id, cfg) in cfgs.iter() {
        for e in validate(cfg) {
            errors.push(format!("cfg {} ({}): {e}", id.0, cfg.name));
        }
    }
    // One CFG per key.
    let mut seen_keys: std::collections::BTreeMap<CfgKey, CfgId> =
        std::collections::BTreeMap::new();
    for (id, cfg) in cfgs.iter() {
        if let Some(first) = seen_keys.insert(cfg.key, id) {
            errors.push(format!(
                "cfg {} and cfg {} have the same key {:?}",
                first.0, id.0, cfg.key
            ));
        }
    }
    // `par for` body references.
    for (id, cfg) in cfgs.iter() {
        for (b, block) in cfg.blocks.iter().enumerate() {
            if let Terminator::ParFor { body, .. } = &block.term
                && !cfgs.has(*body)
            {
                errors.push(format!(
                    "cfg {} ({}): block {b}: par for body {} does not exist",
                    id.0, cfg.name, body.0
                ));
            }
        }
    }
    errors
}

fn local_ok(cfg: &Cfg, l: Local) -> bool {
    (l as usize) < cfg.locals.len()
}

fn check_place(cfg: &Cfg, place: &Place, b: usize, errors: &mut Vec<String>) {
    if let PlaceBase::Local(l) = place.base
        && !local_ok(cfg, l)
    {
        errors.push(format!("block {b}: place uses undeclared local {l}"));
    }
    for proj in &place.proj {
        if let Projection::Index(op) = proj {
            check_operand(cfg, op, b, errors);
        }
    }
}

fn check_operand(cfg: &Cfg, op: &Operand, b: usize, errors: &mut Vec<String>) {
    if let Operand::Place(p, _) = op {
        check_place(cfg, p, b, errors);
    }
}

fn check_stmt(cfg: &Cfg, b: usize, stmt: &Stmt, errors: &mut Vec<String>) {
    match &stmt.kind {
        StmtKind::StorageStart(l) | StmtKind::StorageEnd(l) => {
            if !local_ok(cfg, *l) {
                errors.push(format!("block {b}: storage of undeclared local {l}"));
            }
        }
        StmtKind::Assign { place, value } => {
            check_place(cfg, place, b, errors);
            check_rvalue(cfg, value, b, errors);
        }
        StmtKind::Cancel(l) | StmtKind::Abandon(l) => {
            if !local_ok(cfg, *l) {
                errors.push(format!("block {b}: undeclared local {l}"));
            }
        }
        StmtKind::Check(check) => {
            if let Some(p) = &check.place {
                check_place(cfg, p, b, errors);
            }
            if let Some(o) = &check.lo {
                check_operand(cfg, o, b, errors);
            }
            if let Some(o) = &check.hi {
                check_operand(cfg, o, b, errors);
            }
            for o in &check.operands {
                check_operand(cfg, o, b, errors);
            }
            for p in &check.places {
                check_place(cfg, p, b, errors);
            }
        }
        StmtKind::EnterUnsafe => {}
        StmtKind::Use(op) => check_operand(cfg, op, b, errors),
        StmtKind::Variant { place, .. } => check_place(cfg, place, b, errors),
        StmtKind::IterConsumed(place) => check_place(cfg, place, b, errors),
    }
}

fn check_rvalue(cfg: &Cfg, value: &Rvalue, b: usize, errors: &mut Vec<String>) {
    match value {
        Rvalue::Use(op) => check_operand(cfg, op, b, errors),
        Rvalue::Unary { operand, .. } => check_operand(cfg, operand, b, errors),
        Rvalue::Binary { lhs, rhs, .. } => {
            check_operand(cfg, lhs, b, errors);
            check_operand(cfg, rhs, b, errors);
        }
        Rvalue::Borrow { place, .. } | Rvalue::Reborrow { place, .. } => {
            check_place(cfg, place, b, errors)
        }
        Rvalue::Aggregate { fields, .. } => {
            for f in fields {
                check_operand(cfg, f, b, errors);
            }
        }
        Rvalue::Cast { operand, .. } | Rvalue::DynCast { operand, .. } => {
            check_operand(cfg, operand, b, errors)
        }
        Rvalue::Discriminant(p) | Rvalue::SliceLen(p) | Rvalue::SlicePtr(p) => {
            check_place(cfg, p, b, errors)
        }
        Rvalue::Slice { base, lo, hi, .. } => {
            check_place(cfg, base, b, errors);
            check_operand(cfg, lo, b, errors);
            check_operand(cfg, hi, b, errors);
        }
        Rvalue::SliceElem { base, index } => {
            check_place(cfg, base, b, errors);
            check_operand(cfg, index, b, errors);
        }
        Rvalue::Builtin { args, .. } => {
            for a in args {
                match a {
                    BuiltinArg::Value(op) => check_operand(cfg, op, b, errors),
                    BuiltinArg::Place(p) => check_place(cfg, p, b, errors),
                }
            }
        }
    }
}

fn check_term(cfg: &Cfg, b: usize, term: &Terminator, errors: &mut Vec<String>) {
    let target = |t: Block, errors: &mut Vec<String>| {
        if t as usize >= cfg.blocks.len() {
            errors.push(format!("block {b}: target block {t} does not exist"));
        }
    };
    match term {
        Terminator::Goto(t) => target(*t, errors),
        Terminator::Branch {
            cond,
            then_block,
            else_block,
        } => {
            check_operand(cfg, cond, b, errors);
            target(*then_block, errors);
            target(*else_block, errors);
        }
        Terminator::Switch {
            value,
            cases,
            default,
        } => {
            check_operand(cfg, value, b, errors);
            for (_, t) in cases {
                target(*t, errors);
            }
            target(*default, errors);
        }
        Terminator::Return { value, end, .. } => {
            if let Some(v) = value {
                check_operand(cfg, v, b, errors);
            }
            if let Some(l) = end
                && !local_ok(cfg, *l)
            {
                errors.push(format!("block {b}: return ends undeclared local {l}"));
            }
        }
        Terminator::Call {
            target: call,
            args,
            dest,
            next,
        } => {
            check_call(cfg, call, b, errors);
            for a in args {
                check_operand(cfg, a, b, errors);
            }
            if let Some(d) = dest {
                check_place(cfg, d, b, errors);
            }
            target(*next, errors);
        }
        Terminator::Trap { operands, .. } => {
            for o in operands {
                check_operand(cfg, o, b, errors);
            }
        }
        Terminator::Spawn {
            target: call,
            args,
            scope,
            dest,
            next,
        } => {
            check_call(cfg, call, b, errors);
            for a in args {
                check_operand(cfg, a, b, errors);
            }
            check_operand(cfg, scope, b, errors);
            if let Some(d) = dest {
                check_place(cfg, d, b, errors);
            }
            target(*next, errors);
        }
        Terminator::ScopeWait { scope, next } => {
            if !local_ok(cfg, *scope) {
                errors.push(format!("block {b}: scope_wait on undeclared local {scope}"));
            }
            target(*next, errors);
        }
        Terminator::Select { ops } => {
            for op in ops {
                target(op.successor, errors);
                if let Some(call) = &op.call {
                    check_call(cfg, &call.target, b, errors);
                    for a in &call.args {
                        check_operand(cfg, a, b, errors);
                    }
                    check_place(cfg, &call.dest, b, errors);
                }
            }
        }
        Terminator::ParFor {
            captures,
            capture_places,
            iterand,
            grain,
            next,
            ..
        } => {
            for c in captures {
                check_operand(cfg, c, b, errors);
            }
            for p in capture_places {
                check_place(cfg, p, b, errors);
            }
            match iterand {
                ParIterand::Value(v) => check_operand(cfg, v, b, errors),
                ParIterand::Range { lo, hi, step, .. } => {
                    check_operand(cfg, lo, b, errors);
                    check_operand(cfg, hi, b, errors);
                    if let Some(s) = step {
                        check_operand(cfg, s, b, errors);
                    }
                }
            }
            if let Some(g) = grain {
                check_operand(cfg, g, b, errors);
            }
            target(*next, errors);
        }
        Terminator::Dispatch {
            executor,
            lo,
            hi,
            args,
            dest,
            next,
            ..
        } => {
            check_operand(cfg, executor, b, errors);
            check_operand(cfg, lo, b, errors);
            check_operand(cfg, hi, b, errors);
            for a in args {
                check_operand(cfg, &a.value, b, errors);
                if let Some(p) = &a.place {
                    check_place(cfg, p, b, errors);
                }
            }
            if let Some(d) = dest {
                check_place(cfg, d, b, errors);
            }
            target(*next, errors);
        }
        Terminator::Unreachable => {}
    }
}

fn check_call(cfg: &Cfg, call: &CallTarget, b: usize, errors: &mut Vec<String>) {
    if let CallTarget::Value { callee, .. } = call {
        check_operand(cfg, callee, b, errors);
    }
}

/// Check the storage range of every local on every path.
///
/// The state of one local at a point is one of three bits: not started (0), running
/// (1), ended (2). A local may be started again after it ended (a binding in a loop).
/// The analysis visits reachable states and requires that the local be *running*
/// wherever a statement or terminator uses it. A `Return` may name the temporary whose
/// value it returns (`end`): that one ends at the terminator, after the value is read.
/// A trap ends the process and runs nothing, so it does not require storage ends; an
/// unreachable block has no paths.
fn check_storage(cfg: &Cfg, errors: &mut Vec<String>) {
    let n = cfg.locals.len();
    if n == 0 || cfg.blocks.is_empty() {
        return;
    }
    let entry = cfg.entry as usize;
    if entry >= cfg.blocks.len() {
        return;
    }
    // One state per local per block: bit 0 not started, bit 1 running, bit 2 ended.
    for l in 0..n {
        let l = l as Local;
        let mut mask = vec![0u8; cfg.blocks.len()];
        mask[entry] = 1;
        let mut changed = true;
        let mut rounds = 0usize;
        while changed {
            changed = false;
            rounds += 1;
            if rounds > cfg.blocks.len() * 8 + 16 {
                errors.push("storage analysis did not settle".to_string());
                return;
            }
            for (b, block) in cfg.blocks.iter().enumerate() {
                let m = mask[b];
                if m == 0 {
                    continue;
                }
                let leaves = matches!(block.term, Terminator::Return { .. });
                let end_at_term =
                    matches!(&block.term, Terminator::Return { end: Some(x), .. } if *x == l);
                let mut out: u8 = 0;
                for state in 0..3u8 {
                    if m & (1 << state) == 0 {
                        continue;
                    }
                    let mut st = state;
                    for stmt in &block.stmts {
                        match &stmt.kind {
                            StmtKind::StorageStart(x) if *x == l => {
                                if st == 1 {
                                    errors.push(format!(
                                        "block {b}: storage_start of %{l} while it is running"
                                    ));
                                }
                                st = 1;
                            }
                            StmtKind::StorageEnd(x) if *x == l => {
                                if st != 1 {
                                    errors.push(format!(
                                        "block {b}: storage_end of %{l} without a start on some path"
                                    ));
                                }
                                st = 2;
                            }
                            _ => {
                                if stmt_uses_local(stmt, l) && st != 1 {
                                    errors.push(format!(
                                        "block {b}: %{l} is used outside its storage"
                                    ));
                                }
                            }
                        }
                    }
                    if term_uses_local(&block.term, l) && st != 1 {
                        errors.push(format!(
                            "block {b}: %{l} is used by a terminator outside its storage"
                        ));
                    }
                    if end_at_term {
                        if st != 1 {
                            errors.push(format!("block {b}: return ends %{l} outside its storage"));
                        }
                        st = 2;
                    }
                    if leaves {
                        if st == 1 {
                            errors.push(format!(
                                "block {b}: %{l} is still running at a terminator that leaves"
                            ));
                        }
                    } else {
                        out |= 1 << st;
                    }
                }
                if out == 0 {
                    continue;
                }
                for t in successors(&block.term) {
                    let t = t as usize;
                    if t >= cfg.blocks.len() {
                        continue;
                    }
                    if out & !mask[t] != 0 {
                        mask[t] |= out;
                        changed = true;
                    }
                }
            }
        }
    }
}

fn place_uses_local(p: &Place, l: Local) -> bool {
    let root = matches!(p.base, PlaceBase::Local(x) if x == l);
    root || p.proj.iter().any(|proj| match proj {
        Projection::Index(op) => operand_uses_local(op, l),
        _ => false,
    })
}

fn operand_uses_local(op: &Operand, l: Local) -> bool {
    match op {
        Operand::Place(p, _) => place_uses_local(p, l),
        Operand::Const(_) => false,
    }
}

fn rvalue_uses_local(value: &Rvalue, l: Local) -> bool {
    match value {
        Rvalue::Use(op) => operand_uses_local(op, l),
        Rvalue::Unary { operand, .. } => operand_uses_local(operand, l),
        Rvalue::Binary { lhs, rhs, .. } => operand_uses_local(lhs, l) || operand_uses_local(rhs, l),
        Rvalue::Borrow { place, .. } | Rvalue::Reborrow { place, .. } => place_uses_local(place, l),
        Rvalue::Aggregate { fields, .. } => fields.iter().any(|f| operand_uses_local(f, l)),
        Rvalue::Cast { operand, .. } | Rvalue::DynCast { operand, .. } => {
            operand_uses_local(operand, l)
        }
        Rvalue::Discriminant(p) | Rvalue::SliceLen(p) | Rvalue::SlicePtr(p) => {
            place_uses_local(p, l)
        }
        Rvalue::Slice { base, lo, hi, .. } => {
            place_uses_local(base, l) || operand_uses_local(lo, l) || operand_uses_local(hi, l)
        }
        Rvalue::SliceElem { base, index } => {
            place_uses_local(base, l) || operand_uses_local(index, l)
        }
        Rvalue::Builtin { args, .. } => args.iter().any(|a| match a {
            BuiltinArg::Value(op) => operand_uses_local(op, l),
            BuiltinArg::Place(p) => place_uses_local(p, l),
        }),
    }
}

fn stmt_uses_local(stmt: &Stmt, l: Local) -> bool {
    match &stmt.kind {
        StmtKind::StorageStart(_) | StmtKind::StorageEnd(_) => false,
        StmtKind::Assign { place, value } => {
            place_uses_local(place, l) || rvalue_uses_local(value, l)
        }
        StmtKind::Cancel(x) | StmtKind::Abandon(x) => *x == l,
        StmtKind::Check(check) => {
            check.place.as_ref().is_some_and(|p| place_uses_local(p, l))
                || check.lo.as_ref().is_some_and(|o| operand_uses_local(o, l))
                || check.hi.as_ref().is_some_and(|o| operand_uses_local(o, l))
                || check.operands.iter().any(|o| operand_uses_local(o, l))
                || check.places.iter().any(|p| place_uses_local(p, l))
        }
        StmtKind::Use(op) => operand_uses_local(op, l),
        StmtKind::EnterUnsafe => false,
        StmtKind::Variant { place, .. } => place_uses_local(place, l),
        StmtKind::IterConsumed(place) => place_uses_local(place, l),
    }
}

fn term_uses_local(term: &Terminator, l: Local) -> bool {
    match term {
        Terminator::Goto(_) | Terminator::Unreachable => false,
        Terminator::Branch {
            cond,
            then_block: _,
            else_block: _,
        } => operand_uses_local(cond, l),
        Terminator::Switch { value, .. } => operand_uses_local(value, l),
        Terminator::Return { value, .. } => {
            value.as_ref().is_some_and(|o| operand_uses_local(o, l))
        }
        Terminator::Call {
            target, args, dest, ..
        } => {
            call_uses_local(target, l)
                || args.iter().any(|a| operand_uses_local(a, l))
                || dest.as_ref().is_some_and(|p| place_uses_local(p, l))
        }
        Terminator::Trap { operands, .. } => operands.iter().any(|o| operand_uses_local(o, l)),
        Terminator::Spawn {
            target,
            args,
            scope,
            dest,
            ..
        } => {
            call_uses_local(target, l)
                || args.iter().any(|a| operand_uses_local(a, l))
                || operand_uses_local(scope, l)
                || dest.as_ref().is_some_and(|p| place_uses_local(p, l))
        }
        Terminator::ScopeWait { scope, .. } => *scope == l,
        Terminator::Select { ops } => ops.iter().any(|op| {
            op.call.as_ref().is_some_and(|c| {
                call_uses_local(&c.target, l)
                    || c.args.iter().any(|a| operand_uses_local(a, l))
                    || place_uses_local(&c.dest, l)
            })
        }),
        Terminator::ParFor {
            captures,
            capture_places,
            iterand,
            grain,
            ..
        } => {
            captures.iter().any(|c| operand_uses_local(c, l))
                || capture_places.iter().any(|p| place_uses_local(p, l))
                || match iterand {
                    ParIterand::Value(v) => operand_uses_local(v, l),
                    ParIterand::Range { lo, hi, step, .. } => {
                        operand_uses_local(lo, l)
                            || operand_uses_local(hi, l)
                            || step.as_ref().is_some_and(|s| operand_uses_local(s, l))
                    }
                }
                || grain.as_ref().is_some_and(|g| operand_uses_local(g, l))
        }
        Terminator::Dispatch {
            executor,
            lo,
            hi,
            args,
            dest,
            ..
        } => {
            operand_uses_local(executor, l)
                || operand_uses_local(lo, l)
                || operand_uses_local(hi, l)
                || args.iter().any(|a| {
                    operand_uses_local(&a.value, l)
                        || a.place.as_ref().is_some_and(|p| place_uses_local(p, l))
                })
                || dest.as_ref().is_some_and(|p| place_uses_local(p, l))
        }
    }
}

fn call_uses_local(call: &CallTarget, l: Local) -> bool {
    match call {
        CallTarget::Fn { .. } => false,
        CallTarget::Value { callee, .. } => operand_uses_local(callee, l),
    }
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
