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
use ingot_sema::ty::{GenericArg, TyKind, TypeId};
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
    let mut indirect = None;
    let mut alloc_def = None;
    let mut alloc_args: Vec<GenericArg> = Vec::new();
    match target {
        CallTarget::Fn { fn_ref, .. } => {
            if let FnSource::Interface {
                via: InterfaceVia::Dyn,
                ..
            } = &fn_ref.source
            {
                indirect = Some("dyn");
            }
            alloc_def = Some(fn_ref.def);
            alloc_args = fn_ref.args.clone();
        }
        CallTarget::Value { kind, callee, .. } => match kind {
            ValueKind::FnPtr => {
                let through = match operand_type(decls, cfg, callee).map(|t| decls.types.kind(t)) {
                    Some(TyKind::FnPtr(sig)) if sig.ext => "extern",
                    _ => "fn",
                };
                indirect = Some(through);
            }
            ValueKind::DynFn => indirect = Some("dyn"),
            ValueKind::Closure(_) | ValueKind::Bound => {}
        },
    }
    if let Some(through) = indirect {
        let mut item = item(id, cfg, CostKind::IndirectCall, span);
        item.through = Some(through);
        items.push(item);
    }
    if let Some(def) = alloc_def
        && let Some(err) = decls.fn_sig(def).and_then(|sig| sig.error)
        && is_alloc_error(decls, sema, err)
    {
        let mut item = item(id, cfg, CostKind::Alloc, span);
        item.allocator = allocator_arg(decls, sema, def, &alloc_args);
        items.push(item);
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

fn item_in_module(sema: &Sema, module: &str, name: &str) -> Option<DefId> {
    let m = sema.module_paths.get(module)?;
    sema.module(*m).types.get(name).copied()
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
        Const::Param { .. } | Const::Named { .. } => None,
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
