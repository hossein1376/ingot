//! The `ingot cost` rows that are properties of the checked CFG: `indirect_call`,
//! `task_spawn`, `copy` and `alloc` (spec §31, tools.md `ingot cost`).
//!
//! `copy` reports source-level copies and moves, not the CFG's own storage shuffles:
//! a `let`/assignment whose destination is a user local and whose value is a place, an
//! argument passed by value, and a returned value. A struct literal is constructed in
//! place and is not a copy; passing or returning one is.

use crate::model::*;
use crate::removal::{CostItem, CostKind};
use ingot_diag::source::Span;
use ingot_sema::defs::{DefId, Sema};
use ingot_sema::sigs::{Bound, Declarations, ItemSig};
use ingot_sema::ty::{GenericArg, Subst, TyKind, TypeId};
use ingot_sema::typed::{FnSource, InterfaceVia};
use std::collections::BTreeMap;

/// The large-value threshold in §31: a copy is reported when it is larger than 64 bytes.
const COPY_THRESHOLD: u64 = 64;

pub fn cost_items(decls: &mut Declarations, sema: &Sema, cfgs: &Cfgs) -> Vec<CostItem> {
    let mut items = Vec::new();
    for (id, cfg) in cfgs.iter() {
        let mut sizes = BTreeMap::new();
        let defs = temp_defs(cfg);
        for block in &cfg.blocks {
            for stmt in &block.stmts {
                if let StmtKind::Assign { place, value } = &stmt.kind
                    && let Some(local) = place.root()
                    && matches!(cfg.local(local).kind, LocalKind::User | LocalKind::Param)
                    && let Rvalue::Use(op) = value
                    && source_value(cfg, &defs, op, 0)
                    && let Some(ty) = operand_type(decls, cfg, op)
                {
                    add_copy(decls, id, cfg, ty, stmt.span, &mut sizes, &mut items);
                }
            }
            match &block.term {
                Terminator::Call { target, args, .. } => {
                    add_call(decls, sema, id, cfg, target, block.term_span, &mut items);
                    for arg in args {
                        if let Some(ty) = operand_type(decls, cfg, arg) {
                            add_copy(decls, id, cfg, ty, block.term_span, &mut sizes, &mut items);
                        }
                    }
                }
                Terminator::Spawn { args, .. } => {
                    items.push(item(id, cfg, CostKind::TaskSpawn, block.term_span));
                    for arg in args {
                        if let Some(ty) = operand_type(decls, cfg, arg) {
                            add_copy(decls, id, cfg, ty, block.term_span, &mut sizes, &mut items);
                        }
                    }
                }
                Terminator::Return { value, .. } => {
                    if let Some(op) = value
                        && let Some(ty) = operand_type(decls, cfg, op)
                    {
                        add_copy(decls, id, cfg, ty, block.term_span, &mut sizes, &mut items);
                    }
                }
                Terminator::Dispatch { args, .. } => {
                    for arg in args {
                        if let Some(ty) = operand_type(decls, cfg, &arg.value) {
                            add_copy(decls, id, cfg, ty, block.term_span, &mut sizes, &mut items);
                        }
                    }
                }
                _ => {}
            }
        }
    }
    items
}

fn item(id: CfgId, cfg: &Cfg, kind: CostKind, span: Span) -> CostItem {
    CostItem {
        kind,
        cfg: id,
        fn_name: cfg.name.clone(),
        file: cfg.file,
        span,
        through: None,
        bytes: None,
        allocator: None,
    }
}

fn add_copy(
    decls: &mut Declarations,
    id: CfgId,
    cfg: &Cfg,
    ty: TypeId,
    span: Span,
    sizes: &mut BTreeMap<TypeId, Option<u64>>,
    items: &mut Vec<CostItem>,
) {
    let size = *sizes.entry(ty).or_insert_with(|| {
        decls
            .layout_of(ty)
            .ok()
            .map(|l| l.size)
            .filter(|size| *size > COPY_THRESHOLD)
    });
    if let Some(bytes) = size {
        let mut item = item(id, cfg, CostKind::Copy, span);
        item.bytes = Some(bytes);
        items.push(item);
    }
}

fn add_call(
    decls: &mut Declarations,
    sema: &Sema,
    id: CfgId,
    cfg: &Cfg,
    target: &CallTarget,
    span: Span,
    items: &mut Vec<CostItem>,
) {
    let indirect = indirect_kind(decls, cfg, target);
    let mut alloc_def = None;
    let mut alloc_args: Vec<GenericArg> = Vec::new();
    match target {
        CallTarget::Fn { fn_ref, .. } => {
            alloc_def = Some(fn_ref.def);
            alloc_args = fn_ref.args.clone();
        }
        CallTarget::Value { .. } => {}
    }
    if let Some(through) = indirect {
        let mut item = item(id, cfg, CostKind::IndirectCall, span);
        item.through = Some(through);
        items.push(item);
    }
    if let Some(def) = alloc_def {
        if let Some(err) = decls.fn_sig(def).and_then(|sig| sig.error)
            && is_alloc_error(decls, sema, err)
        {
            let mut item = item(id, cfg, CostKind::Alloc, span);
            item.allocator = allocator_arg(decls, sema, def, &alloc_args);
            items.push(item);
        }
        if is_lock_call(sema, def) {
            items.push(item(id, cfg, CostKind::Lock, span));
        }
    }
}

/// True when `ty` is `std.mem.AllocErr` or `std.mem.AllocFail[T]`.
fn is_alloc_error(decls: &Declarations, sema: &Sema, ty: TypeId) -> bool {
    let alloc_err = item_in_module(sema, "std.mem", "AllocErr");
    let alloc_fail = item_in_module(sema, "std.mem", "AllocFail");
    match decls.types.kind(ty) {
        TyKind::Adt(def, _) => Some(*def) == alloc_err || Some(*def) == alloc_fail,
        _ => false,
    }
}

/// The callee's one generic argument whose parameter is bound by `Allocator`.
fn allocator_arg(
    decls: &mut Declarations,
    sema: &Sema,
    def: DefId,
    args: &[GenericArg],
) -> Option<String> {
    let allocator = item_in_module(sema, "std.mem", "Allocator")?;
    let generics = decls.generics_of(def).to_vec();
    let mut found = None;
    for (i, g) in generics.iter().enumerate() {
        let Some(bounds) = decls.param_bounds.get(&g.def) else {
            continue;
        };
        if !bounds
            .iter()
            .any(|b| matches!(b, Bound::Interface(iface, _) if *iface == allocator))
        {
            continue;
        }
        if let Some(GenericArg::Ty(ty)) = args.get(i) {
            if found.is_some() {
                return None;
            }
            found = Some(decls.types.display(sema, *ty));
        }
    }
    found
}

/// `fn`, `dyn` or `extern` when a call is indirect (§31), shared by `indirect_call`
/// and `park_point`.
fn indirect_kind(decls: &mut Declarations, cfg: &Cfg, target: &CallTarget) -> Option<&'static str> {
    match target {
        CallTarget::Fn { fn_ref, .. } => match &fn_ref.source {
            FnSource::Interface {
                via: InterfaceVia::Dyn,
                ..
            } => Some("dyn"),
            _ => None,
        },
        CallTarget::Value { kind, callee, .. } => match kind {
            ValueKind::FnPtr => {
                let through = match operand_type(decls, cfg, callee).map(|t| decls.types.kind(t)) {
                    Some(TyKind::FnPtr(sig)) if sig.ext => "extern",
                    _ => "fn",
                };
                Some(through)
            }
            ValueKind::DynFn => Some("dyn"),
            ValueKind::Closure(_) | ValueKind::Bound => None,
        },
    }
}

/// The lock functions of pipeline §8, recognized by path.
fn is_lock_call(sema: &Sema, def: DefId) -> bool {
    lock_path(&sema.qualified_name(def))
}

fn lock_path(q: &str) -> bool {
    matches!(
        q,
        "std.sync.Mutex.lock"
            | "std.sync.Mutex.try_lock"
            | "std.sync.RwLock.read"
            | "std.sync.RwLock.write"
            | "std.sync.RwLock.try_read"
            | "std.sync.RwLock.try_write"
    )
}

/// The runtime entries that park today, and a future `Task.join`.
fn parks_on_lock(q: &str) -> bool {
    matches!(
        q,
        "std.sync.Mutex.lock" | "std.sync.RwLock.read" | "std.sync.RwLock.write"
    )
}

fn is_park_entry(sema: &Sema, def: DefId) -> bool {
    park_path(&sema.qualified_name(def))
}

fn park_path(q: &str) -> bool {
    q == "std.sys.rt.park" || q == "std.task.Task.join"
}

fn item_in_module(sema: &Sema, module: &str, name: &str) -> Option<DefId> {
    let m = sema.module_paths.get(module)?;
    sema.module(*m).types.get(name).copied()
}

/// `park_point` rows over phase 11a's instance call graph: a terminator or call that
/// may park, and every instance that can reach one. Indirect calls are assumed to park.
pub fn park_points(
    decls: &mut Declarations,
    sema: &Sema,
    cfgs: &Cfgs,
    instances: &crate::instances::Instances,
) -> Vec<CostItem> {
    use std::collections::{BTreeSet, VecDeque};
    let mut direct = BTreeSet::new();
    for i in &instances.instances {
        if cfg_for_key(cfgs, &i.key)
            .is_some_and(|(_, cfg)| cfg_has_park(decls, sema, cfg, &i.key.subst))
        {
            direct.insert(i.key.clone());
        }
    }
    let mut park = direct;
    let mut queue: VecDeque<_> = park.iter().cloned().collect();
    while let Some(callee) = queue.pop_front() {
        for (caller, target, _) in &instances.call_edges {
            if *target == callee && park.insert(caller.clone()) {
                queue.push_back(caller.clone());
            }
        }
    }
    let mut items = Vec::new();
    for key in &park {
        let Some((id, cfg)) = cfg_for_key(cfgs, key) else {
            continue;
        };
        for block in &cfg.blocks {
            if term_park(decls, sema, cfg, &key.subst, &block.term) {
                items.push(item(id, cfg, CostKind::ParkPoint, block.term_span));
            }
        }
    }
    for (caller, callee, span) in &instances.call_edges {
        if !park.contains(callee) {
            continue;
        }
        let Some((id, cfg)) = cfg_for_key(cfgs, caller) else {
            continue;
        };
        items.push(item(id, cfg, CostKind::ParkPoint, *span));
    }
    let mut seen = BTreeSet::new();
    items.retain(|i| seen.insert((i.fn_name.clone(), i.file, i.span.start)));
    items
}

fn cfg_for_key<'a>(
    cfgs: &'a Cfgs,
    key: &crate::instances::InstanceKey,
) -> Option<(CfgId, &'a Cfg)> {
    let body = match key.body {
        crate::instances::InstanceBody::Item => BodyKey::Item,
        crate::instances::InstanceBody::Closure(e) => BodyKey::Closure(e),
        crate::instances::InstanceBody::ParFor(s) => BodyKey::ParFor(s),
        _ => return None,
    };
    let want = CfgKey {
        owner: key.owner,
        body,
    };
    cfgs.iter().find(|(_, cfg)| cfg.key == want)
}

fn cfg_has_park(decls: &mut Declarations, sema: &Sema, cfg: &Cfg, subst: &Subst) -> bool {
    cfg.blocks
        .iter()
        .any(|block| term_park(decls, sema, cfg, subst, &block.term))
}

fn term_park(
    decls: &mut Declarations,
    sema: &Sema,
    cfg: &Cfg,
    subst: &Subst,
    term: &Terminator,
) -> bool {
    match term {
        Terminator::ScopeWait { .. } | Terminator::Select { .. } | Terminator::ParFor { .. } => {
            true
        }
        Terminator::Dispatch { executor, .. } => {
            let ty = operand_type(decls, cfg, executor).map(|ty| decls.instantiate(ty, subst));
            ty.is_some_and(|ty| is_exec_par(decls, sema, ty))
        }
        Terminator::Call { target, .. } => {
            if let CallTarget::Fn { fn_ref, .. } = target
                && (is_park_entry(sema, fn_ref.def)
                    || parks_on_lock(&sema.qualified_name(fn_ref.def)))
            {
                return true;
            }
            indirect_kind(decls, cfg, target).is_some()
        }
        _ => false,
    }
}

fn is_exec_par(decls: &Declarations, sema: &Sema, ty: TypeId) -> bool {
    let ty = match decls.types.kind(ty) {
        TyKind::Ref(_, inner) => *inner,
        _ => ty,
    };
    let TyKind::Adt(def, _) = decls.types.kind(ty) else {
        return false;
    };
    let d = sema.def(*def);
    sema.module(d.module).path == "std.exec" && d.name == "Par"
}

fn operand_type(decls: &mut Declarations, cfg: &Cfg, op: &Operand) -> Option<TypeId> {
    match op {
        Operand::Place(p, _) => place_type(decls, cfg, p),
        Operand::Const(c) => const_type(decls, c),
    }
}

fn const_type(decls: &Declarations, c: &Const) -> Option<TypeId> {
    match c {
        Const::Int { ty, .. } | Const::Float { ty, .. } => Some(*ty),
        Const::Bool(_) => Some(decls.types.bool_),
        Const::Unit => Some(decls.types.void),
        Const::Str(_) | Const::CStr(_) => None,
        Const::Zero { ty, .. } | Const::Uninit { ty, .. } => Some(*ty),
        Const::Param { def, .. } => decls.param_const_ty.get(def).copied(),
        Const::Named { def, .. } => match decls.items.get(def) {
            Some(ItemSig::Const(c)) => Some(c.ty),
            _ => None,
        },
        Const::Expr { .. } => Some(decls.types.i64_),
        Const::Fn { .. } => None,
    }
}

/// The `Rvalue` of every temporary local, so a source copy that lowering routed
/// through a temporary can be recognized.
fn temp_defs(cfg: &Cfg) -> BTreeMap<Local, Rvalue> {
    let mut defs = BTreeMap::new();
    for block in &cfg.blocks {
        for stmt in &block.stmts {
            if let StmtKind::Assign { place, value } = &stmt.kind
                && place.proj.is_empty()
                && let Some(local) = place.root()
                && cfg.local(local).kind == LocalKind::Temp
            {
                defs.insert(local, value.clone());
            }
        }
    }
    defs
}

/// A value written in the source: a user local, parameter or static place, possibly
/// reached through a compiler temporary that only copies that place.
fn source_value(cfg: &Cfg, defs: &BTreeMap<Local, Rvalue>, op: &Operand, depth: u32) -> bool {
    if depth > 32 {
        return false;
    }
    match op {
        Operand::Place(p, _) => match p.base {
            PlaceBase::Static(_) => true,
            PlaceBase::Local(l) => match cfg.local(l).kind {
                LocalKind::User | LocalKind::Param => true,
                LocalKind::Temp => match defs.get(&l) {
                    Some(Rvalue::Use(inner)) => source_value(cfg, defs, inner, depth + 1),
                    _ => false,
                },
                LocalKind::Scope => false,
            },
        },
        Operand::Const(_) => false,
    }
}

fn place_type(decls: &mut Declarations, cfg: &Cfg, place: &Place) -> Option<TypeId> {
    let mut ty = match place.base {
        PlaceBase::Local(l) => cfg.local(l).ty,
        PlaceBase::Static(d) => match decls.items.get(&d) {
            Some(ItemSig::Static(s)) => s.ty,
            _ => return None,
        },
    };
    for proj in &place.proj {
        ty = match proj {
            Projection::Field { index, .. } => {
                let TyKind::Adt(def, args) = decls.types.kind(ty).clone() else {
                    return None;
                };
                let sig = decls.struct_sig(def)?.clone();
                let s = decls.subst_for(def, &args);
                decls.types.subst(sig.fields.get(*index)?.ty, &s)
            }
            Projection::TupleField(i) => {
                let TyKind::Tuple(ts) = decls.types.kind(ty).clone() else {
                    return None;
                };
                *ts.get(*i)?
            }
            Projection::Index(_) => match decls.types.kind(ty).clone() {
                TyKind::Array(t, _) | TyKind::Slice(_, t) => t,
                _ => return None,
            },
            Projection::Deref | Projection::DerefRaw => match decls.types.kind(ty).clone() {
                TyKind::Ref(_, t)
                | TyKind::Slice(_, t)
                | TyKind::RawPtr(_, t)
                | TyKind::Optional(t) => t,
                _ => return None,
            },
            Projection::Payload { variant } => {
                let TyKind::Adt(def, args) = decls.types.kind(ty).clone() else {
                    return None;
                };
                let sig = decls.enum_sig(def)?.clone();
                let payload = sig.variants.get(*variant)?.payload?;
                let s = decls.subst_for(def, &args);
                decls.types.subst(payload, &s)
            }
        };
    }
    Some(ty)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lock_functions_are_recognized_by_path() {
        for q in [
            "std.sync.Mutex.lock",
            "std.sync.Mutex.try_lock",
            "std.sync.RwLock.read",
            "std.sync.RwLock.write",
            "std.sync.RwLock.try_read",
            "std.sync.RwLock.try_write",
        ] {
            assert!(lock_path(q), "{q}");
        }
        assert!(!lock_path("app.Mutex.lock"));
        assert!(!lock_path("std.sync.Cell.lock"));
        assert!(parks_on_lock("std.sync.Mutex.lock"));
        assert!(parks_on_lock("std.sync.RwLock.read"));
        assert!(parks_on_lock("std.sync.RwLock.write"));
        assert!(!parks_on_lock("std.sync.Mutex.try_lock"));
        assert!(!parks_on_lock("std.sync.RwLock.try_read"));
        assert!(!parks_on_lock("std.sync.RwLock.try_write"));
    }

    #[test]
    fn park_entries_are_recognized_by_path() {
        assert!(park_path("std.sys.rt.park"));
        assert!(park_path("std.task.Task.join"));
        assert!(!park_path("std.sync.Mutex.lock"));
    }
}
