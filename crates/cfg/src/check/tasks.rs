//! E1401, E1402 and E1407 (checker.md §7).
//!
//! A `spawn` argument and the spawned call's result must be `Send`, and so must a
//! `par for` item. The `par for` body is a capture record plus the item: a read of
//! a capture whose outer type is not `Sync` is E1402, and an assignment, move or
//! exclusive borrow of a capture is E1407.

use crate::model::*;
use ingot_diag::diagnostic::Diagnostic;
use ingot_diag::source::{Sources, Span};
use ingot_sema::defs::{DefId, Sema};
use ingot_sema::sigs::Declarations;
use ingot_sema::ty::{GenericArg, Mutability, TyKind, TypeId};
use std::collections::{BTreeMap, BTreeSet};

pub(super) fn check(
    sema: &Sema,
    decls: &mut Declarations,
    sources: &Sources,
    cfgs: &Cfgs,
) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    for (id, cfg) in cfgs.iter() {
        if is_kernel(decls, cfg) {
            continue;
        }
        let Some(source) = sources.files.get(cfg.file) else {
            continue;
        };
        let mut cx = Cx {
            sema,
            decls,
            cfg,
            source_file: cfg.file,
            diags: Vec::new(),
            seen: BTreeSet::new(),
        };
        cx.spawns();
        if matches!(cfg.key.body, BodyKey::ParFor(_)) {
            cx.par_for(cfgs, id);
        }
        for diag in cx.finish(source) {
            out.push(diag);
        }
    }
    out
}

fn is_kernel(decls: &Declarations, cfg: &Cfg) -> bool {
    decls.fn_sig(cfg.key.owner).is_some_and(|s| s.mods.kernel)
}

struct Cx<'a> {
    sema: &'a Sema,
    decls: &'a mut Declarations,
    cfg: &'a Cfg,
    source_file: usize,
    diags: Vec<Raw>,
    seen: BTreeSet<(u16, usize, usize, String)>,
}

struct Raw {
    code: &'static str,
    message: String,
    span: Span,
    notes: Vec<(&'static str, Span)>,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Mode {
    Read,
    Move,
    Excl,
    Assign,
}

#[derive(Clone)]
struct Capture {
    name: String,
    decl: Span,
    ty: TypeId,
}

impl<'a> Cx<'a> {
    fn spawns(&mut self) {
        for block in &self.cfg.blocks {
            let Terminator::Spawn {
                target, args, dest, ..
            } = &block.term
            else {
                continue;
            };
            let span = block.term_span;
            for (index, arg) in args.iter().enumerate() {
                let Some(ty) = operand_ty(self.decls, self.cfg, arg) else {
                    continue;
                };
                // The whole argument type has no inner span. Point `declared`
                // at the callee parameter's type when that type is in this
                // file; otherwise omit the label.
                let declared = self.param_ty_span(target, index);
                self.require_send(ty, operand_span(self.cfg, arg, span), declared);
            }
            if let Some(ty) = spawned_result(self.sema, self.decls, target, dest.as_ref(), self.cfg)
            {
                self.require_send(ty, span, Some(span));
            }
        }
    }

    fn par_for(&mut self, cfgs: &Cfgs, id: CfgId) {
        let parent = parent_par(cfgs, id);
        let item_span = parent
            .as_ref()
            .map(|p| p.item_span)
            .unwrap_or_else(|| self.cfg.locals.first().map(|l| l.span).unwrap_or_default());
        let cap_local = self.cfg.params.first().copied();
        for (i, param) in self.cfg.params.iter().enumerate() {
            if i == 0 {
                continue;
            }
            let Some(loc) = self.cfg.locals.get(*param as usize) else {
                continue;
            };
            self.require_send(loc.ty, item_span, Some(loc.span));
        }
        if cap_local.is_none() {
            return;
        }
        let captures = parent
            .as_ref()
            .map(|p| p.captures.clone())
            .unwrap_or_default();
        self.scan_captures(&captures);
    }

    fn require_send(&mut self, ty: TypeId, primary: Span, fallback: Option<Span>) {
        if self.decls.types.has_poison(ty) {
            return;
        }
        if self.decls.props(ty).send.is_yes() {
            return;
        }
        let (name, part) = self.offending(ty, true);
        let mut notes = Vec::new();
        if let Some(declared) = part.or(fallback) {
            notes.push(("declared", declared));
        }
        self.error("E1401", format!("`{name}` is not `Send`"), primary, notes);
    }

    /// Type span of argument `index` when the callee is a function defined in
    /// this file.
    fn param_ty_span(&self, target: &CallTarget, index: usize) -> Option<Span> {
        let CallTarget::Fn { fn_ref, .. } = target else {
            return None;
        };
        let def = fn_ref.def;
        let span = self.decls.fn_sig(def)?.params.get(index)?.ty_span;
        let file = self.sema.def(def).file?;
        (file == self.source_file && span.end > span.start).then_some(span)
    }

    fn scan_captures(&mut self, captures: &[Capture]) {
        // One E1402 per capture, at the first read in block order.
        let mut reported = BTreeSet::new();
        for block in &self.cfg.blocks {
            for stmt in &block.stmts {
                let mut hits: BTreeMap<usize, Mode> = BTreeMap::new();
                self.stmt_hits(&stmt.kind, &mut hits);
                self.emit_hits(stmt.span, &hits, captures, &mut reported);
            }
            let mut hits = BTreeMap::new();
            self.term_hits(&block.term, &mut hits);
            self.emit_hits(block.term_span, &hits, captures, &mut reported);
        }
    }

    fn emit_hits(
        &mut self,
        span: Span,
        hits: &BTreeMap<usize, Mode>,
        captures: &[Capture],
        reported: &mut BTreeSet<usize>,
    ) {
        for (index, mode) in hits {
            let (name, decl, ty) = capture_info(self.cfg, self.decls, captures, *index);
            if *mode == Mode::Read {
                if self.decls.types.has_poison(ty) || self.decls.props(ty).sync.is_yes() {
                    continue;
                }
                if !reported.insert(*index) {
                    continue;
                }
                let (type_name, part) = self.offending(ty, false);
                let declared = part.unwrap_or(decl);
                self.error(
                    "E1402",
                    format!("`{name}` is not `Sync` (`{type_name}`)",),
                    span,
                    vec![("declared", declared)],
                );
                continue;
            }
            let what = match mode {
                Mode::Assign => "assigns to",
                Mode::Move => "moves",
                Mode::Excl => "borrows exclusively",
                Mode::Read => "reads",
            };
            self.error(
                "E1407",
                format!("a `par for` body {what} `{name}`"),
                span,
                vec![("declared", decl)],
            );
        }
    }

    fn stmt_hits(&self, kind: &StmtKind, hits: &mut BTreeMap<usize, Mode>) {
        match kind {
            StmtKind::Assign { place, value } => {
                self.rvalue_hits(value, hits);
                self.place_hit(place, Mode::Assign, hits);
            }
            StmtKind::Use(op) => self.operand_hit(op, Mode::Read, hits),
            StmtKind::Check(c) => {
                if let Some(p) = &c.place {
                    self.place_hit(p, Mode::Read, hits);
                }
                for p in &c.places {
                    self.place_hit(p, Mode::Read, hits);
                }
                for op in c.operands.iter().chain(c.lo.as_ref()).chain(c.hi.as_ref()) {
                    self.operand_hit(op, Mode::Read, hits);
                }
            }
            StmtKind::IterConsumed(p) => self.place_hit(p, Mode::Move, hits),
            StmtKind::StorageStart(_)
            | StmtKind::StorageEnd(_)
            | StmtKind::EnterUnsafe
            | StmtKind::Cancel(_)
            | StmtKind::Abandon(_)
            | StmtKind::Variant { .. } => {}
        }
    }

    fn term_hits(&self, term: &Terminator, hits: &mut BTreeMap<usize, Mode>) {
        match term {
            Terminator::Branch { cond, .. } | Terminator::Switch { value: cond, .. } => {
                self.operand_hit(cond, Mode::Read, hits);
            }
            Terminator::Return { value, .. } => {
                if let Some(op) = value {
                    self.operand_hit(op, Mode::Read, hits);
                }
            }
            Terminator::Call { target, args, .. } | Terminator::Spawn { target, args, .. } => {
                if let CallTarget::Value { callee, .. } = target {
                    self.operand_hit(callee, Mode::Read, hits);
                }
                for arg in args {
                    self.operand_hit(arg, Mode::Read, hits);
                }
            }
            Terminator::Trap { operands, .. } => {
                for op in operands {
                    self.operand_hit(op, Mode::Read, hits);
                }
            }
            Terminator::Select { ops } => {
                for op in ops {
                    let Some(call) = &op.call else {
                        continue;
                    };
                    if let CallTarget::Value { callee, .. } = &call.target {
                        self.operand_hit(callee, Mode::Read, hits);
                    }
                    for arg in &call.args {
                        self.operand_hit(arg, Mode::Read, hits);
                    }
                }
            }
            Terminator::Goto(_)
            | Terminator::Unreachable
            | Terminator::ScopeWait { .. }
            | Terminator::ParFor { .. }
            | Terminator::Dispatch { .. } => {}
        }
    }

    fn rvalue_hits(&self, value: &Rvalue, hits: &mut BTreeMap<usize, Mode>) {
        match value {
            Rvalue::Use(op) => self.operand_hit(op, Mode::Read, hits),
            Rvalue::Unary { operand, .. }
            | Rvalue::Cast { operand, .. }
            | Rvalue::DynCast { operand, .. } => self.operand_hit(operand, Mode::Read, hits),
            Rvalue::Binary { lhs, rhs, .. } => {
                self.operand_hit(lhs, Mode::Read, hits);
                self.operand_hit(rhs, Mode::Read, hits);
            }
            Rvalue::Borrow {
                place, mutability, ..
            } => {
                let mode = if *mutability == Mutability::Mut {
                    Mode::Excl
                } else {
                    Mode::Read
                };
                self.place_hit(place, mode, hits);
            }
            Rvalue::Reborrow { place, .. } => self.place_hit(place, Mode::Excl, hits),
            Rvalue::Aggregate { fields, .. } => {
                for field in fields {
                    self.operand_hit(field, Mode::Read, hits);
                }
            }
            Rvalue::Slice {
                base,
                lo,
                hi,
                borrow,
                ..
            } => {
                let mode = if *borrow == Some(Mutability::Mut) {
                    Mode::Excl
                } else {
                    Mode::Read
                };
                self.place_hit(base, mode, hits);
                self.operand_hit(lo, Mode::Read, hits);
                self.operand_hit(hi, Mode::Read, hits);
            }
            Rvalue::SliceElem { base, index } => {
                self.place_hit(base, Mode::Read, hits);
                self.operand_hit(index, Mode::Read, hits);
            }
            Rvalue::Discriminant(p) | Rvalue::SliceLen(p) | Rvalue::SlicePtr(p) => {
                self.place_hit(p, Mode::Read, hits);
            }
            Rvalue::Builtin { args, .. } => {
                for arg in args {
                    match arg {
                        BuiltinArg::Value(op) => self.operand_hit(op, Mode::Read, hits),
                        BuiltinArg::Place(p) => self.place_hit(p, Mode::Read, hits),
                    }
                }
            }
        }
    }

    fn operand_hit(&self, op: &Operand, mode: Mode, hits: &mut BTreeMap<usize, Mode>) {
        let Operand::Place(place, use_) = op else {
            return;
        };
        let mode = if *use_ == PlaceUse::Move && mode == Mode::Read {
            Mode::Move
        } else {
            mode
        };
        self.place_hit(place, mode, hits);
    }

    fn place_hit(&self, place: &Place, mode: Mode, hits: &mut BTreeMap<usize, Mode>) {
        if let Some(index) = capture_index(self.cfg, place) {
            let slot = hits.entry(index).or_insert(Mode::Read);
            if mode > *slot {
                *slot = mode;
            }
        }
        for proj in &place.proj {
            if let Projection::Index(op) = proj {
                self.operand_hit(op, Mode::Read, hits);
            }
        }
    }

    /// The type that makes `ty` fail `Send` or `Sync`, and a span of that part
    /// when the part is a field or a type parameter in this file.
    fn offending(&mut self, ty: TypeId, send: bool) -> (String, Option<Span>) {
        fn holds(decls: &mut Declarations, ty: TypeId, send: bool) -> bool {
            if decls.types.has_poison(ty) {
                return true;
            }
            let props = decls.props(ty);
            if send {
                props.send.is_yes()
            } else {
                props.sync.is_yes()
            }
        }
        fn go(cx: &mut Cx<'_>, ty: TypeId, send: bool, depth: u32) -> (String, Option<Span>) {
            if depth > 16 || holds(cx.decls, ty, send) {
                return (cx.decls.types.display(cx.sema, ty), None);
            }
            let kind = cx.decls.types.kind(ty).clone();
            let name = cx.decls.types.display(cx.sema, ty);
            match kind {
                TyKind::Ref(_, inner) | TyKind::Slice(_, inner) | TyKind::Array(inner, _) => {
                    go(cx, inner, send, depth.saturating_add(1))
                }
                TyKind::Tuple(parts) => {
                    for part in &parts {
                        if !holds(cx.decls, *part, send) {
                            return go(cx, *part, send, depth.saturating_add(1));
                        }
                    }
                    (name, None)
                }
                TyKind::Adt(def, args) => {
                    if let Some(sig) = cx.decls.struct_sig(def).cloned() {
                        let subst = cx.decls.subst_for(def, &args);
                        for field in &sig.fields {
                            let ft = cx.decls.types.subst(field.ty, &subst);
                            if holds(cx.decls, ft, send) {
                                continue;
                            }
                            let (inner_name, inner_span) =
                                go(cx, ft, send, depth.saturating_add(1));
                            let span = inner_span.or(cx.field_span(def, field.ty_span));
                            return (inner_name, span);
                        }
                    }
                    (name, None)
                }
                TyKind::Param(p) => {
                    let def = cx.sema.def(p);
                    let span = (def.file == Some(cx.source_file)).then_some(def.span);
                    (name, span.filter(|s| s.end > s.start))
                }
                _ => (name, None),
            }
        }
        if holds(self.decls, ty, send) {
            return (self.decls.types.display(self.sema, ty), None);
        }
        go(self, ty, send, 0)
    }

    fn field_span(&self, def: DefId, span: Span) -> Option<Span> {
        let file = self.sema.def(def).file?;
        (file == self.source_file && span.end > span.start).then_some(span)
    }

    fn error(
        &mut self,
        code: &'static str,
        message: String,
        span: Span,
        notes: Vec<(&'static str, Span)>,
    ) {
        let key = (code_id(code), span.start, span.end, message.clone());
        if !self.seen.insert(key) {
            return;
        }
        self.diags.push(Raw {
            code,
            message,
            span,
            notes,
        });
    }

    fn finish(self, source: &ingot_diag::source::Source) -> Vec<Diagnostic> {
        let mut out = Vec::new();
        for raw in self.diags {
            let mut diag = Diagnostic::new(raw.code, raw.message, source, raw.span);
            for (role, span) in raw.notes {
                diag.relate(role, span);
            }
            out.push(diag);
        }
        out
    }
}

struct Parent {
    captures: Vec<Capture>,
    item_span: Span,
}

fn parent_par(cfgs: &Cfgs, body: CfgId) -> Option<Parent> {
    for (_, cfg) in cfgs.iter() {
        for block in &cfg.blocks {
            let Terminator::ParFor {
                body: id,
                capture_places,
                capture_names,
                iterand,
                ..
            } = &block.term
            else {
                continue;
            };
            if *id != body {
                continue;
            }
            let mut captures = Vec::new();
            for (i, place) in capture_places.iter().enumerate() {
                let Some(local) = place.root() else {
                    continue;
                };
                let Some(loc) = cfg.locals.get(local as usize) else {
                    continue;
                };
                let name = capture_names
                    .get(i)
                    .cloned()
                    .unwrap_or_else(|| "capture".to_string());
                captures.push(Capture {
                    name,
                    decl: loc.span,
                    ty: loc.ty,
                });
            }
            return Some(Parent {
                captures,
                item_span: iterand_span(cfg, iterand, block.term_span),
            });
        }
    }
    None
}

fn capture_info(
    cfg: &Cfg,
    decls: &mut Declarations,
    captures: &[Capture],
    index: usize,
) -> (String, Span, TypeId) {
    if index == usize::MAX {
        let name = if captures.len() == 1 {
            captures[0].name.clone()
        } else {
            "captures".to_string()
        };
        let decl = captures.first().map(|c| c.decl).unwrap_or_default();
        let ty = captures.first().map(|c| c.ty).unwrap_or(decls.types.poison);
        return (name, decl, ty);
    }
    if let Some(cap) = captures.get(index) {
        return (cap.name.clone(), cap.decl, cap.ty);
    }
    let ty = capture_field_ty(cfg, decls, index).unwrap_or(decls.types.poison);
    ("capture".to_string(), Span::default(), ty)
}

fn capture_field_ty(cfg: &Cfg, decls: &mut Declarations, index: usize) -> Option<TypeId> {
    let local = cfg.params.first().copied()?;
    let ty = cfg.locals.get(local as usize)?.ty;
    let TyKind::Tuple(parts) = decls.types.kind(ty).clone() else {
        return None;
    };
    let field = *parts.get(index)?;
    match decls.types.kind(field).clone() {
        TyKind::Ref(_, inner) => Some(inner),
        _ => Some(field),
    }
}

fn capture_index(cfg: &Cfg, place: &Place) -> Option<usize> {
    let cap = cfg.params.first().copied()?;
    if place.root() != Some(cap) {
        return None;
    }
    match place.proj.first() {
        Some(Projection::TupleField(i)) => Some(*i),
        None => Some(usize::MAX),
        Some(_) => None,
    }
}

fn iterand_span(cfg: &Cfg, iterand: &ParIterand, fallback: Span) -> Span {
    match iterand {
        ParIterand::Value(op) => operand_span(cfg, op, fallback),
        ParIterand::Range { lo, hi, .. } => {
            let a = operand_span(cfg, lo, fallback);
            let b = operand_span(cfg, hi, fallback);
            Span::new(a.start.min(b.start), a.end.max(b.end))
        }
    }
}

fn operand_span(cfg: &Cfg, op: &Operand, fallback: Span) -> Span {
    let Operand::Place(place, _) = op else {
        return fallback;
    };
    place
        .root()
        .and_then(|l| cfg.locals.get(l as usize).map(|loc| loc.span))
        .unwrap_or(fallback)
}

fn operand_ty(decls: &mut Declarations, cfg: &Cfg, op: &Operand) -> Option<TypeId> {
    let Operand::Place(place, _) = op else {
        return None;
    };
    place_ty(decls, cfg, place)
}

fn place_ty(decls: &mut Declarations, cfg: &Cfg, place: &Place) -> Option<TypeId> {
    let mut ty = match &place.base {
        PlaceBase::Local(l) => cfg.locals.get(*l as usize)?.ty,
        PlaceBase::Static(d) => match decls.item(*d) {
            Some(ingot_sema::sigs::ItemSig::Static(s)) => s.ty,
            _ => return None,
        },
    };
    for proj in &place.proj {
        ty = proj_ty(decls, ty, proj)?;
    }
    Some(ty)
}

fn proj_ty(decls: &mut Declarations, ty: TypeId, proj: &Projection) -> Option<TypeId> {
    let kind = decls.types.kind(ty).clone();
    match proj {
        Projection::Field { index, .. } => {
            let TyKind::Adt(def, args) = kind else {
                return None;
            };
            let field = decls.struct_sig(def)?.fields.get(*index)?.ty;
            let subst = decls.subst_for(def, &args);
            Some(decls.types.subst(field, &subst))
        }
        Projection::TupleField(i) => match kind {
            TyKind::Tuple(parts) => parts.get(*i).copied(),
            _ => None,
        },
        Projection::Index(_) => match kind {
            TyKind::Array(e, _) | TyKind::Slice(_, e) => Some(e),
            _ => None,
        },
        Projection::Deref => match kind {
            TyKind::Ref(_, e) | TyKind::Slice(_, e) => Some(e),
            _ => None,
        },
        Projection::DerefRaw => match kind {
            TyKind::RawPtr(_, e) => Some(e),
            _ => None,
        },
        Projection::Payload { .. } => None,
    }
}

fn spawned_result(
    sema: &Sema,
    decls: &mut Declarations,
    target: &CallTarget,
    dest: Option<&Place>,
    cfg: &Cfg,
) -> Option<TypeId> {
    if let Some(place) = dest
        && let Some(ty) = place_ty(decls, cfg, place)
        && let Some(inner) = task_result(sema, decls, ty)
    {
        return Some(inner);
    }
    let CallTarget::Fn { fn_ref, .. } = target else {
        return None;
    };
    let ret = decls.fn_sig(fn_ref.def)?.ret;
    let subst = decls.subst_for(fn_ref.def, &fn_ref.args);
    Some(decls.types.subst(ret, &subst))
}

fn task_result(sema: &Sema, decls: &Declarations, ty: TypeId) -> Option<TypeId> {
    let TyKind::Adt(def, args) = decls.types.kind(ty).clone() else {
        return None;
    };
    if sema.qualified_name(def) != "std.task.Task" {
        return None;
    }
    match args.first() {
        Some(GenericArg::Ty(t)) => Some(*t),
        _ => None,
    }
}

fn code_id(code: &str) -> u16 {
    match code {
        "E1401" => 1401,
        "E1402" => 1402,
        "E1407" => 1407,
        _ => 0,
    }
}
