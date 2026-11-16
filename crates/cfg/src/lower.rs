//! Phase 8: lowering one package's typed bodies to CFGs (pipeline section 5).
//!
//! The lowering reads the HIR and the typed body, nothing else, and reports no
//! diagnostics. Every local, temporaries included, gets an explicit storage range;
//! every check in pipeline section 5's table is an explicit `Check` statement; deferred
//! blocks are copied here, so no `defer` reaches the CFG.

use crate::model::*;
use ingot_diag::source::Span;
use ingot_sema::defs::{DefId, Sema};
use ingot_sema::hir::{
    self, BlockId, ExprId, ExprKind, LocalId, PatId, PatKind, StmtId, StmtKind as HirStmt,
};
use ingot_sema::sigs::{Bound, Declarations, PropBound, Tri};
use ingot_sema::ty::{ConstArg, ConstValue, FnKind, IntTy, Mutability, TyKind, TypeId};
use ingot_sema::typed::{
    BindMode, Bodies, BodyKind, BodyRoot, CallMode, Callee, CaptureMode, ClosureInfo, ExprRes,
    FnRef, ForKind, IdxMethod, ReceiverMode, StmtInfo, TryConversion, TypedBody, ValueCall,
    VariantRef,
};
use ingot_syntax::ast::{AssignOp, BinOp, UnOp};
use std::collections::{BTreeMap, BTreeSet};

/// The result of phase 8 for one package.
#[derive(Debug, Default)]
pub struct Lowered {
    pub cfgs: Cfgs,
    /// `lowering: <what>` for every construct that could not be lowered. The CFG of a
    /// body with such an entry is dropped.
    pub not_implemented: Vec<String>,
}

/// Lower every function and test body of the package that phase 7 checked.
///
/// `bodies` holds the typed bodies of exactly one package (the `checked` one); the
/// HIR packages are indexed by [`TypedBody::package`]. A body with `failed` set is
/// skipped. A body the lowering cannot handle is recorded in `not_implemented` and
/// its CFG is dropped.
pub fn lower_package(
    sources: &ingot_diag::source::Sources,
    sema: &Sema,
    packages: &[hir::Package],
    decls: &mut Declarations,
    bodies: &Bodies,
) -> Lowered {
    let mut cx = LowerCx {
        sources,
        sema,
        packages,
        decls,
        out: Cfgs::default(),
        ni: Vec::new(),
        ni_seen: BTreeSet::new(),
        keys: BTreeMap::new(),
    };
    for (owner, body) in &bodies.bodies {
        if body.failed || !matches!(body.kind, BodyKind::Fn | BodyKind::Test) {
            continue;
        }
        let key = CfgKey {
            owner: *owner,
            body: BodyKey::Item,
        };
        match cx.lower_body(key, body, None) {
            Ok(_) => {}
            Err(what) => cx.unsupported(what),
        }
    }
    Lowered {
        cfgs: cx.out,
        not_implemented: cx.ni,
    }
}

/// Lower one body (a function, test, `const` or `static` initializer) on demand for
/// constant evaluation (S5b). The CFGs of the closures and `par for` bodies it
/// contains come with it. They are not part of [`lower_package`]'s result.
pub fn lower_for_eval(
    sources: &ingot_diag::source::Sources,
    sema: &Sema,
    packages: &[hir::Package],
    decls: &mut Declarations,
    body: &TypedBody,
) -> Result<Cfgs, String> {
    let mut cx = LowerCx {
        sources,
        sema,
        packages,
        decls,
        out: Cfgs::default(),
        ni: Vec::new(),
        ni_seen: BTreeSet::new(),
        keys: BTreeMap::new(),
    };
    let key = CfgKey {
        owner: body.owner,
        body: BodyKey::Item,
    };
    cx.lower_body(key, body, None)?;
    Ok(cx.out)
}

struct LowerCx<'a> {
    sources: &'a ingot_diag::source::Sources,
    sema: &'a Sema,
    packages: &'a [hir::Package],
    decls: &'a mut Declarations,
    out: Cfgs,
    ni: Vec<String>,
    ni_seen: BTreeSet<String>,
    /// Every body that was lowered, or that failed: a defer copy or a guard can ask
    /// for the same body more than once, and it must have exactly one CFG.
    keys: BTreeMap<CfgKey, Option<CfgId>>,
}

impl<'a> LowerCx<'a> {
    fn unsupported(&mut self, what: impl Into<String>) {
        let what = format!("lowering: {}", what.into());
        if self.ni_seen.insert(what.clone()) {
            self.ni.push(what);
        }
    }

    fn position(&self, file: usize, span: Span) -> String {
        let pos = self.sources.get(file).position(span.start);
        format!("{}:{}", pos.line, pos.col)
    }

    fn type_name(&self, ty: TypeId) -> String {
        self.decls.types.display(self.sema, ty)
    }

    fn header(&mut self, key: CfgKey, name: String, ret: TypeId, error: Option<TypeId>) -> CfgId {
        let ret_name = self.type_name(ret);
        let error_name = error.map(|e| self.type_name(e));
        let file = self.sema.def(key.owner).file.unwrap_or(0);
        self.out
            .reserve(key, name, ret, ret_name, error, error_name, file)
    }

    /// Lower one body. `kind` is `None` for the item's own body.
    fn lower_body(
        &mut self,
        key: CfgKey,
        body: &'a TypedBody,
        kind: Option<BodySpec>,
    ) -> Result<CfgId, String> {
        if let Some(existing) = self.keys.get(&key) {
            return match existing {
                Some(id) => Ok(*id),
                None => Err("a body that could not be lowered".to_string()),
            };
        }
        let pkg = &self.packages[body.package];
        let file = self.sema.def(key.owner).file.unwrap_or(0);
        let name = match &kind {
            None => self.sema.qualified_name(key.owner),
            Some(BodySpec::Closure { expr, .. }) => {
                let pos = self.position(file, pkg.expr(*expr).span);
                format!("{}.{{closure {pos}}}", self.sema.qualified_name(key.owner))
            }
            Some(BodySpec::ParFor { stmt, .. }) => {
                let pos = self.position(file, pkg.stmt(*stmt).span);
                format!("{}.{{par for {pos}}}", self.sema.qualified_name(key.owner))
            }
        };
        let (ret, error) = match &kind {
            None => (body.ret, body.error),
            Some(BodySpec::Closure { info, .. }) => (info.sig.ret, info.sig.error),
            Some(BodySpec::ParFor { .. }) => (self.decls.types.void, None),
        };
        let id = self.header(key, name, ret, error);
        self.keys.insert(key, Some(id));
        let mut lw = Lowerer::new(self, body, pkg, id);
        let result = match &kind {
            None => lw.run_item(),
            Some(BodySpec::Closure { expr, info }) => lw.run_closure(*expr, info),
            Some(BodySpec::ParFor {
                stmt,
                block,
                vars,
                index,
                captures,
                tuple_ty,
            }) => lw.run_par_for(*stmt, *block, vars, *index, captures, *tuple_ty),
        };
        let failed = lw.failed.clone();
        let cfg = lw.finish();
        match (result, failed) {
            (Ok(()), None) => {
                self.out.fill(id, cfg);
                Ok(id)
            }
            (Err(what), _) => {
                self.out.drop_at(id);
                self.keys.insert(key, None);
                Err(what)
            }
            (Ok(()), Some(what)) => {
                self.out.drop_at(id);
                self.keys.insert(key, None);
                Err(what)
            }
        }
    }
}

impl<'a> LowerCx<'a> {
    fn lower_closure(
        &mut self,
        body: &'a TypedBody,
        expr: ExprId,
        info: &ClosureInfo,
    ) -> Result<CfgId, String> {
        let key = CfgKey {
            owner: body.owner,
            body: BodyKey::Closure(expr),
        };
        self.lower_body(
            key,
            body,
            Some(BodySpec::Closure {
                expr,
                info: info.clone(),
            }),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn lower_par_for(
        &mut self,
        body: &'a TypedBody,
        stmt: StmtId,
        block: BlockId,
        vars: Vec<(LocalId, TypeId)>,
        index: Option<(LocalId, TypeId)>,
        captures: Vec<(LocalId, TypeId, String)>,
        tuple_ty: TypeId,
    ) -> Result<CfgId, String> {
        let key = CfgKey {
            owner: body.owner,
            body: BodyKey::ParFor(stmt),
        };
        self.lower_body(
            key,
            body,
            Some(BodySpec::ParFor {
                stmt,
                block,
                vars,
                index,
                captures,
                tuple_ty,
            }),
        )
    }
}

enum BodySpec {
    Closure {
        expr: ExprId,
        info: ClosureInfo,
    },
    ParFor {
        stmt: StmtId,
        block: BlockId,
        vars: Vec<(LocalId, TypeId)>,
        index: Option<(LocalId, TypeId)>,
        captures: Vec<(LocalId, TypeId, String)>,
        tuple_ty: TypeId,
    },
}

// ---------------------------------------------------------------- the lowerer

struct Frame {
    span: Span,
    /// Storage-started named locals, in declaration order (no duplicates).
    locals: Vec<Local>,
    /// Statement temporaries live at this point, in creation order.
    temps: Vec<Local>,
    defers: Vec<DeferEntry>,
    /// A `scope` boundary: wait on this local and end it.
    scope: Option<Local>,
    /// The `&Scope` binding of that boundary (`s = &σ`), ended just before `σ`.
    scope_binding: Option<Local>,
}

#[derive(Clone)]
struct DeferEntry {
    body: BlockId,
    span: Span,
    err: bool,
}

struct LoopEntry {
    stmt: Option<StmtId>,
    /// Index in `frames` of the loop's per-iteration frame.
    frame: usize,
    break_block: Block,
    continue_block: Block,
}

enum ExitKind {
    Return {
        value: Option<Operand>,
        is_error: bool,
    },
    Goto(Block),
}

struct Lowerer<'a, 'b> {
    cx: &'b mut LowerCx<'a>,
    body: &'a TypedBody,
    pkg: &'a hir::Package,
    cfg: Cfg,
    cur: Block,
    reachable: bool,
    frames: Vec<Frame>,
    loops: Vec<LoopEntry>,
    local_map: BTreeMap<LocalId, Local>,
    capture_places: BTreeMap<LocalId, Place>,
    copy: Option<DeferCopy>,
    /// Nonzero while lowering a direct call argument: a borrow written there is
    /// two-phase.
    arg_depth: u32,
    /// A guard reads every place through a shared borrow.
    guard_reads: bool,
    guard_places: BTreeMap<LocalId, Place>,
    failed: Option<String>,
    stmt_span: Span,
    /// Set while lowering a `let`: bindings record the statement span so an E1003
    /// edit can be inserted after the whole declaration, not after its first line.
    let_span: Option<Span>,
}

impl<'a, 'b> Lowerer<'a, 'b> {
    fn new(cx: &'b mut LowerCx<'a>, body: &'a TypedBody, pkg: &'a hir::Package, id: CfgId) -> Self {
        let cfg = cx.out.get(id).clone();
        Lowerer {
            cx,
            body,
            pkg,
            cfg,
            cur: 0,
            reachable: true,
            frames: Vec::new(),
            loops: Vec::new(),
            local_map: BTreeMap::new(),
            capture_places: BTreeMap::new(),
            copy: None,
            arg_depth: 0,
            guard_reads: false,
            guard_places: BTreeMap::new(),
            failed: None,
            stmt_span: Span::default(),
            let_span: None,
        }
    }

    fn finish(mut self) -> Cfg {
        if self.cfg.blocks.is_empty() {
            self.cfg.blocks.push(BlockData {
                stmts: Vec::new(),
                term: Terminator::Unreachable,
                term_span: Span::default(),
            });
        }
        self.cfg
    }

    fn fail(&mut self, what: impl Into<String>) {
        if self.failed.is_none() {
            self.failed = Some(what.into());
        }
    }

    fn run_item(&mut self) -> Result<(), String> {
        let root = match self.body.root {
            BodyRoot::Block(b) => b,
            BodyRoot::Expr(e) => return self.run_initializer(e),
        };
        let span = self.pkg.block(root).span;
        self.cur = self.new_block();
        self.cfg.entry = self.cur;
        self.push_frame(span);
        let params: Vec<LocalId> = self.body.params.clone();
        for p in params {
            let l = self.declare_param(p);
            self.cfg.params.push(l);
        }
        let body_span = self.pkg.block(root).span;
        self.lower_stmts(root);
        if self.reachable {
            self.stmt_span = Span::new(body_span.end.saturating_sub(1), body_span.end);
            // Phase 7 rejects a fall-through with a result, so there is no value.
            self.exit(
                0,
                ExitKind::Return {
                    value: None,
                    is_error: false,
                },
            );
        }
        Ok(())
    }

    /// A `const` or `static` initializer, or a constant expression of a signature:
    /// the body's value is its result.
    fn run_initializer(&mut self, e: ExprId) -> Result<(), String> {
        let span = self.span(e);
        self.cur = self.new_block();
        self.cfg.entry = self.cur;
        self.push_frame(span);
        self.stmt_span = span;
        let ty = self.body.ret;
        let op = match self.lower_expr(e) {
            Some(op) => op,
            None => {
                let what = self
                    .failed
                    .clone()
                    .unwrap_or_else(|| "an initializer that the lowering dropped".to_string());
                return Err(what);
            }
        };
        let op = self.snapshot(op, ty, span);
        self.stmt_span = span;
        self.exit(
            0,
            ExitKind::Return {
                value: Some(op),
                is_error: false,
            },
        );
        Ok(())
    }

    fn run_closure(&mut self, expr: ExprId, info: &ClosureInfo) -> Result<(), String> {
        let closure = match &self.pkg.expr(expr).kind {
            ExprKind::Closure(c) => c.as_ref(),
            _ => {
                self.fail("a closure that is not a closure");
                return Err("a closure that is not a closure".to_string());
            }
        };
        let rec_ty = info.ty;
        let self_ty = match info.kind {
            FnKind::Fn => self.ref_ty(Mutability::Shared, rec_ty),
            FnKind::FnMut => self.ref_ty(Mutability::Mut, rec_ty),
            FnKind::FnOnce => rec_ty,
        };
        self.push_frame(closure.span);
        self.cur = self.new_block();
        self.cfg.entry = self.cur;
        let self_local = self.new_local(
            LocalKind::Param,
            Some("self".to_string()),
            None,
            self_ty,
            closure.span,
        );
        self.start_local(self_local, true);
        self.cfg.params.push(self_local);
        let mut fields = Vec::new();
        for (i, cap) in info.captures.iter().enumerate() {
            let cap_local = self.pkg.local(cap.local);
            let name = cap_local.name.clone();
            let span = cap_local.span;
            let fty = self.local_ty(cap.local);
            fields.push(ClosureField {
                ty: fty,
                span,
                name: name.clone(),
            });
            let mut place = Place::local(self_local);
            if info.kind != FnKind::FnOnce {
                place = place.deref();
            }
            let place = place.field(i, Some(name));
            self.capture_places.insert(cap.local, place);
        }
        self.cfg.closure_ty = Some(rec_ty);
        self.cfg.closure_fields = fields;
        for p in &closure.params {
            let ty = self.local_ty(p.local);
            let l = self.new_local(
                LocalKind::Param,
                Some(p.name.clone()),
                Some(p.local),
                ty,
                p.span,
            );
            self.start_local(l, true);
            self.cfg.params.push(l);
            self.local_map.insert(p.local, l);
        }
        let body_span = self.pkg.block(closure.body).span;
        self.lower_stmts(closure.body);
        if self.reachable {
            self.stmt_span = Span::new(body_span.end.saturating_sub(1), body_span.end);
            self.exit(
                0,
                ExitKind::Return {
                    value: None,
                    is_error: false,
                },
            );
        }
        Ok(())
    }

    fn run_par_for(
        &mut self,
        _stmt: StmtId,
        block: BlockId,
        vars: &[(LocalId, TypeId)],
        index: Option<(LocalId, TypeId)>,
        captures: &[(LocalId, TypeId, String)],
        tuple_ty: TypeId,
    ) -> Result<(), String> {
        let span = self.pkg.block(block).span;
        self.push_frame(span);
        self.cur = self.new_block();
        self.cfg.entry = self.cur;
        // The capture record is a tuple of shared borrows, by value; a capture is read
        // through its field's borrow.
        let self_ty = tuple_ty;
        let self_local = self.new_local(
            LocalKind::Param,
            Some("self".to_string()),
            None,
            self_ty,
            span,
        );
        self.start_local(self_local, true);
        self.cfg.params.push(self_local);
        for (i, (local, _ty, _name)) in captures.iter().enumerate() {
            let place = Place::local(self_local).tuple_field(i).deref();
            self.capture_places.insert(*local, place);
        }
        for (local, ty) in vars {
            let name = self.pkg.local(*local).name.clone();
            let l = self.new_local(
                LocalKind::Param,
                Some(name),
                Some(*local),
                *ty,
                self.pkg.local(*local).span,
            );
            self.start_local(l, true);
            self.cfg.params.push(l);
            self.local_map.insert(*local, l);
        }
        if let Some((local, ty)) = index {
            let name = self.pkg.local(local).name.clone();
            let l = self.new_local(
                LocalKind::Param,
                Some(name),
                Some(local),
                ty,
                self.pkg.local(local).span,
            );
            self.start_local(l, true);
            self.cfg.params.push(l);
            self.local_map.insert(local, l);
        }
        self.lower_stmts(block);
        if self.reachable {
            let body_span = self.pkg.block(block).span;
            self.stmt_span = Span::new(body_span.end.saturating_sub(1), body_span.end);
            self.exit(
                0,
                ExitKind::Return {
                    value: None,
                    is_error: false,
                },
            );
        }
        Ok(())
    }
}

// ------------------------------------------------------------------ small helpers

impl<'a, 'b> Lowerer<'a, 'b> {
    fn ty(&self, e: ExprId) -> TypeId {
        self.body
            .expr(e)
            .map(|i| i.ty)
            .unwrap_or(self.cx.decls.types.poison)
    }

    fn res(&self, e: ExprId) -> ExprRes {
        self.body
            .expr(e)
            .map(|i| i.res.clone())
            .unwrap_or(ExprRes::None)
    }

    fn shared_read(&self, e: ExprId) -> bool {
        self.body.expr(e).is_some_and(|i| i.shared_read)
    }

    fn local_ty(&self, l: LocalId) -> TypeId {
        self.body
            .local(l)
            .map(|i| i.ty)
            .unwrap_or(self.cx.decls.types.poison)
    }

    fn local_mode(&self, l: LocalId) -> BindMode {
        self.body.local(l).map(|i| i.mode).unwrap_or(BindMode::Move)
    }

    fn span(&self, e: ExprId) -> Span {
        self.pkg.expr(e).span
    }

    fn unparen(&self, mut e: ExprId) -> ExprId {
        while let ExprKind::Paren(inner) = &self.pkg.expr(e).kind {
            e = *inner;
        }
        e
    }

    fn ty_name(&self, ty: TypeId) -> String {
        self.cx.decls.types.display(self.cx.sema, ty)
    }

    fn kind(&self, ty: TypeId) -> TyKind {
        self.cx.decls.types.kind(ty).clone()
    }

    fn is_copy(&mut self, ty: TypeId) -> bool {
        self.cx.decls.props(ty).copy == Tri::Yes
    }

    fn int_like(&mut self, ty: TypeId) -> bool {
        match self.kind(ty) {
            TyKind::Int(_) => true,
            TyKind::Param(d) => {
                let mut ok = false;
                if let Some(bs) = self.cx.decls.param_bounds.get(&d).cloned() {
                    for b in bs {
                        if matches!(
                            b,
                            Bound::Prop(
                                PropBound::Int | PropBound::SInt | PropBound::UInt | PropBound::Num
                            )
                        ) {
                            ok = true;
                        }
                    }
                }
                ok
            }
            _ => false,
        }
    }

    fn shift_like(&mut self, ty: TypeId) -> bool {
        match self.kind(ty) {
            TyKind::Int(_) | TyKind::Vector(..) => true,
            TyKind::Param(_) => self.int_like(ty),
            _ => false,
        }
    }

    fn strip_derefs(&self, mut ty: TypeId, mut n: u32) -> TypeId {
        while n > 0 {
            match self.kind(ty) {
                TyKind::Ref(_, inner) => {
                    ty = inner;
                    n -= 1;
                }
                _ => break,
            }
        }
        ty
    }

    fn ref_ty(&mut self, m: Mutability, inner: TypeId) -> TypeId {
        self.cx.decls.types.intern(TyKind::Ref(m, inner))
    }

    fn i64_ty(&mut self) -> TypeId {
        self.cx.decls.types.int(IntTy::I64)
    }

    fn u8_ty(&mut self) -> TypeId {
        self.cx.decls.types.int(IntTy::U8)
    }

    fn bool_ty(&self) -> TypeId {
        self.cx.decls.types.bool_
    }

    fn tag_ty(&mut self, enum_ty: TypeId) -> TypeId {
        match self.kind(enum_ty) {
            TyKind::Adt(d, _) => {
                let tag = self.cx.decls.enum_sig(d).map(|s| s.tag_ty);
                match tag {
                    Some(t) => self.cx.decls.types.int(t),
                    None => self.u8_ty(),
                }
            }
            _ => self.u8_ty(),
        }
    }

    fn variant_tag(&self, v: &VariantRef) -> i128 {
        if let Some(d) = v.def
            && let Some(sig) = self.cx.decls.enum_sig(d)
            && let Some(var) = sig.variants.get(v.index)
        {
            return var.tag;
        }
        v.index as i128
    }

    fn fn_name(&self, f: &FnRef) -> String {
        self.cx.sema.qualified_name(f.def)
    }
}

// ------------------------------------------------------------------ CFG building

impl<'a, 'b> Lowerer<'a, 'b> {
    fn new_block(&mut self) -> Block {
        let id = self.cfg.blocks.len() as Block;
        self.cfg.blocks.push(BlockData {
            stmts: Vec::new(),
            term: Terminator::Unreachable,
            term_span: Span::default(),
        });
        id
    }

    fn term(&mut self, t: Terminator) {
        let span = self.stmt_span;
        self.term_at(t, span);
    }

    fn term_at(&mut self, t: Terminator, span: Span) {
        if !self.reachable {
            return;
        }
        if let Some(block) = self.cfg.blocks.get_mut(self.cur as usize) {
            block.term = t;
            block.term_span = span;
        }
    }

    /// End the current block with a terminator and start an unreachable block.
    fn end_block(&mut self, t: Terminator) {
        self.term(t);
        self.kill();
    }

    fn goto(&mut self, target: Block) {
        self.end_block(Terminator::Goto(target));
    }

    /// Nothing more can be reached in the current block. No block is created; the
    /// next `set_reachable` point starts a fresh one.
    fn kill(&mut self) {
        self.reachable = false;
    }

    fn emit(&mut self, kind: StmtKind) {
        self.emit_at(kind, self.stmt_span);
    }

    fn emit_at(&mut self, kind: StmtKind, span: Span) {
        if !self.reachable {
            return;
        }
        self.emit_reading(kind, span, None);
    }

    fn emit_reading(&mut self, kind: StmtKind, span: Span, read_span: Option<Span>) {
        if !self.reachable {
            return;
        }
        let defer = self.copy;
        if let Some(block) = self.cfg.blocks.get_mut(self.cur as usize) {
            block.stmts.push(Stmt {
                span,
                kind,
                defer,
                read_span,
            });
        }
    }

    /// Span of a copied user, parameter or scope place, when it is narrower than the
    /// statement. Analysis A reports E1001 there. A move, a temporary and a copy whose
    /// span is already the statement keep `None`.
    fn copied_place_span(&self, op: &Operand, init_span: Span) -> Option<Span> {
        let Operand::Place(place, PlaceUse::Copy) = op else {
            return None;
        };
        let PlaceBase::Local(id) = place.base else {
            return None;
        };
        let kind = self.cfg.locals.get(id as usize)?.kind;
        if !matches!(kind, LocalKind::Param | LocalKind::User | LocalKind::Scope) {
            return None;
        }
        if init_span == self.stmt_span {
            return None;
        }
        Some(init_span)
    }

    fn push_frame(&mut self, span: Span) {
        self.frames.push(Frame {
            span,
            locals: Vec::new(),
            temps: Vec::new(),
            defers: Vec::new(),
            scope: None,
            scope_binding: None,
        });
    }

    fn new_local(
        &mut self,
        kind: LocalKind,
        name: Option<String>,
        source: Option<LocalId>,
        ty: TypeId,
        span: Span,
    ) -> Local {
        let id = self.cfg.locals.len() as Local;
        let ty_name = self.ty_name(ty);
        self.cfg.locals.push(CfgLocal {
            kind,
            name,
            source,
            ty,
            ty_name,
            span,
            decl_span: span,
        });
        id
    }

    fn start_local(&mut self, l: Local, in_frame: bool) {
        let span = self.cfg.local(l).span;
        self.emit_at(StmtKind::StorageStart(l), span);
        if in_frame
            && let Some(f) = self.frames.last_mut()
            && !contains(&f.locals, l)
        {
            f.locals.push(l);
        }
    }

    fn new_temp(&mut self, ty: TypeId, span: Span) -> Local {
        let l = self.new_local(LocalKind::Temp, None, None, ty, span);
        self.emit_at(StmtKind::StorageStart(l), span);
        if let Some(f) = self.frames.last_mut()
            && !contains(&f.temps, l)
        {
            f.temps.push(l);
        }
        l
    }

    fn declare_param(&mut self, id: LocalId) -> Local {
        let name = self.pkg.local(id).name.clone();
        let span = self.pkg.local(id).span;
        let ty = self.local_ty(id);
        let l = self.new_local(LocalKind::Param, Some(name), Some(id), ty, span);
        self.start_local(l, true);
        self.local_map.insert(id, l);
        l
    }

    fn ensure_local(&mut self, id: LocalId) -> Local {
        if let Some(&l) = self.local_map.get(&id) {
            return l;
        }
        let name = self.pkg.local(id).name.clone();
        let span = self.pkg.local(id).span;
        let ty = self.local_ty(id);
        // `scope s` binds an ordinary `&Scope` local; lowering declares the `Scope`
        // value it borrows (`lower_scope`, 03-concurrency §18).
        let l = self.new_local(LocalKind::User, Some(name), Some(id), ty, span);
        self.local_map.insert(id, l);
        l
    }

    /// Start the storage of a binding at the current point. A `LocalId` shared by
    /// alternatives of `p | q` gets one start per alternative and one end for the arm.
    fn declare_binding(&mut self, id: LocalId) -> Local {
        let l = self.ensure_local(id);
        self.start_local(l, true);
        if let Some(span) = self.let_span
            && let Some(loc) = self.cfg.locals.get_mut(l as usize)
        {
            loc.decl_span = span;
        }
        l
    }

    fn local_place(&mut self, id: LocalId) -> Place {
        if let Some(p) = self.guard_places.get(&id) {
            return p.clone();
        }
        if let Some(p) = self.capture_places.get(&id) {
            return p.clone();
        }
        if let Some(&l) = self.local_map.get(&id) {
            return Place::local(l);
        }
        Place::local(self.ensure_local(id))
    }

    fn temp_mark(&self) -> usize {
        self.frames.last().map(|f| f.temps.len()).unwrap_or(0)
    }

    fn end_temps(&mut self, mark: usize) {
        let Some(frame) = self.frames.last_mut() else {
            return;
        };
        let temps: Vec<Local> = frame.temps.drain(mark..).collect();
        for l in temps.into_iter().rev() {
            let span = self.cfg.local(l).span;
            self.emit_at(StmtKind::StorageEnd(l), span);
        }
    }

    /// How a parent consumes a place that already holds the expression's value.
    /// A `Copy` type is read again; anything else moves out of the temporary.
    fn consume_use(&mut self, ty: TypeId) -> PlaceUse {
        if self.is_copy(ty) {
            PlaceUse::Copy
        } else {
            PlaceUse::Move
        }
    }

    fn read_place(&mut self, place: Place, ty: TypeId) -> Operand {
        if self.guard_reads || self.is_copy(ty) {
            Operand::Place(place, PlaceUse::Copy)
        } else {
            Operand::Place(place, PlaceUse::Move)
        }
    }

    /// Read one payload of an enum. `try`, `catch`, `orelse` and `for` consume the
    /// enum by taking that payload. A linear enum stays live if the payload is only
    /// copied, so the read is a move even when the payload type is `Copy`.
    fn read_payload(&mut self, place: Place, payload_ty: TypeId, enum_ty: TypeId) -> Operand {
        if !self.guard_reads && self.counts_linear(enum_ty) {
            Operand::Place(place, PlaceUse::Move)
        } else {
            self.read_place(place, payload_ty)
        }
    }

    /// A read of the expression's value: `@debug` and `@format` read their arguments
    /// through a shared borrow, so even a non-`Copy` value is read, not moved.
    fn read_place_at(&mut self, e: ExprId, place: Place, ty: TypeId) -> Operand {
        if self.shared_read(e) || self.guard_reads || self.is_copy(ty) {
            Operand::Place(place, PlaceUse::Copy)
        } else {
            Operand::Place(place, PlaceUse::Move)
        }
    }

    fn snapshot(&mut self, op: Operand, ty: TypeId, span: Span) -> Operand {
        match op {
            Operand::Const(_) => op,
            Operand::Place(..) => {
                let l = self.new_temp(ty, span);
                self.emit_at(
                    StmtKind::Assign {
                        place: Place::local(l),
                        value: Rvalue::Use(op),
                    },
                    span,
                );
                let mode = if self.is_copy(ty) {
                    PlaceUse::Copy
                } else {
                    PlaceUse::Move
                };
                Operand::Place(Place::local(l), mode)
            }
        }
    }

    fn materialize(&mut self, value: Rvalue, ty: TypeId, span: Span) -> Operand {
        let l = self.new_temp(ty, span);
        self.emit_at(
            StmtKind::Assign {
                place: Place::local(l),
                value,
            },
            span,
        );
        let mode = if self.is_copy(ty) {
            PlaceUse::Copy
        } else {
            PlaceUse::Move
        };
        Operand::Place(Place::local(l), mode)
    }

    fn place_of_operand(&mut self, op: Operand, ty: TypeId, span: Span) -> Place {
        match op {
            Operand::Place(p, _) => p,
            Operand::Const(_) => {
                let l = self.new_temp(ty, span);
                self.emit_at(
                    StmtKind::Assign {
                        place: Place::local(l),
                        value: Rvalue::Use(op),
                    },
                    span,
                );
                Place::local(l)
            }
        }
    }

    fn emit_check(&mut self, mut check: Check) {
        if check.ty_name.is_none()
            && let Some(ty) = check.ty
        {
            check.ty_name = Some(self.ty_name(ty));
        }
        let span = check.span;
        self.emit_at(StmtKind::Check(check), span);
    }

    fn check_arith(&mut self, op: BinOp, lhs: &Operand, rhs: &Operand, ty: TypeId, span: Span) {
        let kind = if matches!(op, BinOp::Shl | BinOp::Shr) && self.shift_like(ty) {
            Some(CheckKind::Shift)
        } else if self.int_like(ty) {
            match op {
                BinOp::Add | BinOp::Sub | BinOp::Mul => Some(CheckKind::Overflow),
                BinOp::Div | BinOp::Rem => Some(CheckKind::Division),
                _ => None,
            }
        } else {
            None
        };
        let Some(kind) = kind else {
            return;
        };
        self.emit_check(Check {
            kind,
            place: None,
            lo: None,
            hi: None,
            operands: vec![lhs.clone(), rhs.clone()],
            places: Vec::new(),
            ty: Some(ty),
            ty_name: None,
            op: Some(op.symbol().to_string()),
            span,
        });
    }

    fn bounds_index(&mut self, base: Place, index: Operand, span: Span) -> Check {
        Check {
            kind: CheckKind::Bounds(BoundsKind::Index),
            place: Some(base),
            lo: Some(index),
            hi: None,
            operands: Vec::new(),
            places: Vec::new(),
            ty: None,
            ty_name: None,
            op: None,
            span,
        }
    }

    fn bounds_slice(&mut self, base: Place, lo: Operand, hi: Operand, span: Span) -> Check {
        Check {
            kind: CheckKind::Bounds(BoundsKind::Slice),
            place: Some(base),
            lo: Some(lo),
            hi: Some(hi),
            operands: Vec::new(),
            places: Vec::new(),
            ty: None,
            ty_name: None,
            op: None,
            span,
        }
    }

    fn bounds_vector(
        &mut self,
        base: Place,
        index: Operand,
        width: u32,
        tail: bool,
        span: Span,
    ) -> Check {
        Check {
            kind: CheckKind::Bounds(BoundsKind::Vector { width, tail }),
            place: Some(base),
            lo: Some(index),
            hi: None,
            operands: Vec::new(),
            places: Vec::new(),
            ty: None,
            ty_name: None,
            op: None,
            span,
        }
    }
}

// ------------------------------------------------------------------ statements

impl<'a, 'b> Lowerer<'a, 'b> {
    fn lower_stmts(&mut self, block: BlockId) {
        for &s in &self.pkg.block(block).stmts {
            if !self.reachable {
                break;
            }
            self.lower_stmt(s);
        }
    }

    fn lower_block(&mut self, block: BlockId) -> bool {
        self.lower_block_binding(block, None)
    }

    fn lower_block_binding(&mut self, block: BlockId, binding: Option<(PatId, Place)>) -> bool {
        let span = self.pkg.block(block).span;
        self.push_frame(span);
        if let Some((pat, place)) = binding {
            self.bind_pattern(pat, place);
        }
        self.lower_stmts(block);
        if self.reachable {
            self.finish_frame();
            true
        } else {
            self.frames.pop();
            false
        }
    }

    fn finish_frame(&mut self) {
        let idx = self.frames.len() - 1;
        let span = self.frames[idx].span;
        self.end_frame(idx, false, span);
        self.frames.pop();
    }

    fn lower_stmt(&mut self, id: StmtId) {
        let saved = self.stmt_span;
        self.stmt_span = self.pkg.stmt(id).span;
        self.lower_stmt_inner(id);
        self.stmt_span = saved;
    }

    fn lower_stmt_inner(&mut self, id: StmtId) {
        let stmt = self.pkg.stmt(id);
        let mark = self.temp_mark();
        match &stmt.kind {
            HirStmt::Let { pattern, init, .. } => {
                self.let_span = Some(stmt.span);
                let ty = self.ty(*init);
                let Some(op) = self.lower_expr(*init) else {
                    self.let_span = None;
                    return;
                };
                // A let of one name binds the initializer directly; any other pattern
                // needs the value in a place.
                if let PatKind::Bind { local } = &self.pkg.pat(*pattern).kind {
                    let init_span = self.span(*init);
                    let read_span = self.copied_place_span(&op, init_span);
                    let l = self.declare_binding(*local);
                    let value = match op {
                        Operand::Place(p, use_) => Rvalue::Use(Operand::Place(p, use_)),
                        op => Rvalue::Use(op),
                    };
                    self.emit_reading(
                        StmtKind::Assign {
                            place: Place::local(l),
                            value,
                        },
                        self.stmt_span,
                        read_span,
                    );
                } else {
                    let place = match op {
                        Operand::Place(p, _) => p,
                        _ => self.place_of_operand(op, ty, stmt.span),
                    };
                    self.bind_pattern(*pattern, place);
                }
                self.let_span = None;
                self.end_temps(mark);
            }
            HirStmt::Assign { place, op, value } => {
                self.lower_assign(*place, *op, *value, stmt.span, mark);
            }
            HirStmt::Discard(e) => {
                if let Some(op) = self.lower_expr(*e) {
                    self.emit(StmtKind::Use(op));
                }
                self.end_temps(mark);
            }
            HirStmt::Expr(e) => {
                let ty = self.ty(*e);
                if let Some(op) = self.lower_expr(*e)
                    && ty != self.cx.decls.types.void
                    && !matches!(op, Operand::Const(Const::Unit))
                {
                    self.emit(StmtKind::Use(op));
                }
                self.end_temps(mark);
            }
            HirStmt::Return(e) => {
                let mut value = None;
                if let Some(e) = e {
                    let ty = self.ty(*e);
                    let Some(op) = self.lower_expr(*e) else {
                        return;
                    };
                    value = Some(self.snapshot(op, ty, stmt.span));
                }
                self.exit(
                    0,
                    ExitKind::Return {
                        value,
                        is_error: false,
                    },
                );
            }
            HirStmt::Fail(e) => {
                let ty = self.ty(*e);
                let Some(op) = self.lower_expr(*e) else {
                    return;
                };
                let value = self.snapshot(op, ty, stmt.span);
                self.exit(
                    0,
                    ExitKind::Return {
                        value: Some(value),
                        is_error: true,
                    },
                );
            }
            HirStmt::Break { target, .. } => {
                let Some(entry) = self.find_loop(*target) else {
                    self.fail("a `break` with no loop");
                    return;
                };
                let (frame, dest) = (entry.frame, entry.break_block);
                self.exit(frame, ExitKind::Goto(dest));
            }
            HirStmt::Continue { target, .. } => {
                let Some(entry) = self.find_loop(*target) else {
                    self.fail("a `continue` with no loop");
                    return;
                };
                let (frame, dest) = (entry.frame, entry.continue_block);
                self.exit(frame, ExitKind::Goto(dest));
            }
            HirStmt::Defer { err, body } => {
                let span = stmt.span;
                if let Some(f) = self.frames.last_mut() {
                    f.defers.push(DeferEntry {
                        body: *body,
                        span,
                        err: *err,
                    });
                }
            }
            HirStmt::Block(b) => {
                self.lower_block(*b);
            }
            HirStmt::If {
                cond,
                then,
                otherwise,
            } => {
                self.lower_if_stmt(cond, *then, otherwise.as_ref().map(|s| *s), mark);
            }
            HirStmt::While { cond, body, .. } => {
                self.lower_while(id, cond, *body, stmt.span);
            }
            HirStmt::Loop { body, .. } => {
                self.lower_loop(id, *body, stmt.span);
            }
            HirStmt::For {
                par,
                grain,
                vars,
                index,
                iter,
                range_end,
                step,
                body,
                ..
            } => {
                if *par {
                    self.lower_par_for_stmt(
                        id,
                        *grain,
                        vars,
                        index.as_ref(),
                        *iter,
                        *range_end,
                        *step,
                        *body,
                        stmt.span,
                        mark,
                    );
                } else {
                    let info = match self.body.stmt(id) {
                        Some(StmtInfo::For(info)) => info.clone(),
                        _ => {
                            self.fail("a `for` loop with no iteration information");
                            return;
                        }
                    };
                    self.lower_for(
                        id,
                        &info.kind,
                        vars,
                        index.as_ref(),
                        *iter,
                        *range_end,
                        *step,
                        *body,
                        stmt.span,
                        mark,
                    );
                }
            }
            HirStmt::Match { scrutinee, arms } => {
                self.lower_match(*scrutinee, arms, None, stmt.span);
                self.end_temps(mark);
            }
            HirStmt::Scope { local, body, .. } => {
                self.lower_scope(*local, *body, stmt.span);
            }
            HirStmt::Spawn { call, scope } => {
                let _ = self.lower_spawn(*call, *scope, None, stmt.span);
                self.end_temps(mark);
            }
            HirStmt::Dispatch {
                call,
                domain,
                executor,
            } => {
                let info = match self.body.stmt(id) {
                    Some(StmtInfo::Dispatch(info)) => info.clone(),
                    _ => {
                        self.fail("a `dispatch` with no dispatch information");
                        return;
                    }
                };
                let _ = self.lower_dispatch(*call, *domain, *executor, &info, None, stmt.span);
                self.end_temps(mark);
            }
            HirStmt::Select { arms } => {
                self.lower_select(arms, stmt.span, mark);
            }
            HirStmt::Unsafe { body, .. } => {
                self.emit(StmtKind::EnterUnsafe);
                self.lower_block(*body);
            }
            HirStmt::Poison => {
                self.fail("a poison statement");
            }
        }
    }

    fn find_loop(&self, target: Option<StmtId>) -> Option<&LoopEntry> {
        match target {
            Some(t) => self.loops.iter().find(|l| l.stmt == Some(t)),
            None => self.loops.last(),
        }
    }

    fn lower_if_stmt(
        &mut self,
        cond: &hir::Cond,
        then: BlockId,
        otherwise: Option<StmtId>,
        mark: usize,
    ) {
        let then_block = self.new_block();
        let else_block = self.new_block();
        let Some(bind) = self.lower_cond(cond, then_block, else_block) else {
            return;
        };
        let join = self.new_block();
        // then
        self.cur = then_block;
        self.reachable = true;
        let then_falls = self.lower_block_binding(then, bind);
        if then_falls && self.reachable {
            self.goto(join);
        }
        // else
        self.cur = else_block;
        self.reachable = true;
        let else_falls = match otherwise {
            Some(s) => {
                self.lower_stmt(s);
                self.reachable
            }
            None => true,
        };
        if else_falls && self.reachable {
            self.goto(join);
        }
        self.cur = join;
        self.reachable = then_falls || else_falls;
        if self.reachable {
            self.end_temps(mark);
        } else {
            // The statement's temporaries ended on every path that left the block.
        }
    }

    fn lower_while(&mut self, stmt: StmtId, cond: &hir::Cond, body: BlockId, span: Span) {
        let head = self.new_block();
        self.goto(head);
        self.cur = head;
        self.reachable = true;
        let frame = self.frames.len();
        self.push_frame(self.pkg.block(body).span);
        let exit = self.new_block();
        self.loops.push(LoopEntry {
            stmt: Some(stmt),
            frame,
            break_block: exit,
            continue_block: head,
        });
        let body_block = self.new_block();
        let exit_head = self.new_block();
        let bind = self.lower_cond(cond, body_block, exit_head);
        let Some(bind) = bind else {
            self.frames.pop();
            self.loops.pop();
            return;
        };
        // The condition failed: end this iteration's storage and leave.
        self.cur = exit_head;
        self.reachable = true;
        self.end_iteration(frame);
        self.goto(exit);
        // The body.
        self.cur = body_block;
        self.reachable = true;
        let falls = self.lower_block_binding(body, bind);
        if falls && self.reachable {
            self.stmt_span = span;
            self.end_iteration(frame);
            self.goto(head);
        }
        self.cur = exit;
        self.reachable = true;
        self.stmt_span = span;
        self.frames.pop();
        self.loops.pop();
    }

    fn lower_loop(&mut self, stmt: StmtId, body: BlockId, span: Span) {
        let head = self.new_block();
        self.goto(head);
        self.cur = head;
        self.reachable = true;
        let frame = self.frames.len();
        self.push_frame(self.pkg.block(body).span);
        let exit = self.new_block();
        self.loops.push(LoopEntry {
            stmt: Some(stmt),
            frame,
            break_block: exit,
            continue_block: head,
        });
        let falls = self.lower_block(body);
        if falls && self.reachable {
            self.stmt_span = span;
            self.end_iteration(frame);
            self.goto(head);
        }
        self.cur = exit;
        self.reachable = true;
        self.stmt_span = span;
        self.frames.pop();
        self.loops.pop();
    }

    /// End the per-iteration storage of the loop frame (temporaries and the variables
    /// of a `for`), without popping the frame.
    fn end_iteration(&mut self, frame: usize) {
        let (temps, locals) = {
            let f = &self.frames[frame];
            (f.temps.clone(), f.locals.clone())
        };
        for l in temps.into_iter().rev() {
            let sp = self.cfg.local(l).span;
            self.emit_at(StmtKind::StorageEnd(l), sp);
        }
        for l in locals.into_iter().rev() {
            let sp = self.cfg.local(l).span;
            self.emit_at(StmtKind::StorageEnd(l), sp);
        }
    }

    fn lower_assign(
        &mut self,
        target: ExprId,
        op: AssignOp,
        value: ExprId,
        span: Span,
        mark: usize,
    ) {
        // The target's index operands first, then the value; the checks run after the
        // value. For a compound assignment the place is read after the checks.
        let Some((place, checks)) = self.lower_place(target) else {
            self.fail("an assignment target that is not a place");
            return;
        };
        let value_ty = self.ty(value);
        let Some(value_op) = self.lower_expr(value) else {
            return;
        };
        let value_op = self.snapshot(value_op, value_ty, span);
        for c in checks {
            self.emit_check(c);
        }
        if op == AssignOp::Assign {
            self.emit(StmtKind::Assign {
                place,
                value: Rvalue::Use(value_op),
            });
            self.end_temps(mark);
            return;
        }
        let place_ty = self.ty(self.unparen(target));
        let old = self.read_place(place.clone(), place_ty);
        let old = self.snapshot(old, place_ty, span);
        let bin = match assign_binop(op) {
            Some(b) => b,
            None => {
                self.end_temps(mark);
                return;
            }
        };
        let rhs = value_op.clone();
        let result = self.materialize(
            Rvalue::Binary {
                op: bin,
                lhs: old.clone(),
                rhs: value_op,
            },
            place_ty,
            span,
        );
        self.check_arith(bin, &old, &rhs, place_ty, span);
        self.emit(StmtKind::Assign {
            place,
            value: Rvalue::Use(result),
        });
        self.end_temps(mark);
    }

    /// `scope s { B }` (pipeline §5): declare a `Scope` local `σ`, bind `s = &σ`,
    /// lower `B`, then end `s`, wait on `σ` and end `σ`. The binding is a shared
    /// loan on `σ`, so an escape is an ordinary E1203 or E1105.
    fn lower_scope(&mut self, local: LocalId, body: BlockId, span: Span) {
        let binding = self.ensure_local(local);
        let binding_ty = self.local_ty(local);
        let scope_ty = match self.kind(binding_ty) {
            TyKind::Ref(_, inner) => inner,
            _ => {
                self.fail("a scope binding whose type is not `&Scope`");
                return;
            }
        };
        let name = self.cfg.local(binding).name.clone();
        let binding_span = self.cfg.local(binding).span;
        let scope = self.new_local(LocalKind::Scope, name, None, scope_ty, binding_span);
        self.start_local(scope, false);
        self.start_local(binding, false);
        self.emit_at(
            StmtKind::Assign {
                place: Place::local(binding),
                value: Rvalue::Borrow {
                    place: Place::local(scope),
                    mutability: Mutability::Shared,
                    two_phase: false,
                },
            },
            binding_span,
        );
        let boundary = self.frames.len();
        self.push_frame(span);
        if let Some(f) = self.frames.get_mut(boundary) {
            f.scope = Some(scope);
            f.scope_binding = Some(binding);
        }
        self.lower_block(body);
        if self.reachable {
            self.end_frame(boundary, false, span);
            self.frames.pop();
        } else {
            self.frames.pop();
        }
    }
}

fn assign_binop(op: AssignOp) -> Option<BinOp> {
    Some(match op {
        AssignOp::Assign => return None,
        AssignOp::Add => BinOp::Add,
        AssignOp::Sub => BinOp::Sub,
        AssignOp::Mul => BinOp::Mul,
        AssignOp::Div => BinOp::Div,
        AssignOp::Rem => BinOp::Rem,
        AssignOp::AddWrap => BinOp::AddWrap,
        AssignOp::SubWrap => BinOp::SubWrap,
        AssignOp::MulWrap => BinOp::MulWrap,
        AssignOp::AddSat => BinOp::AddSat,
        AssignOp::SubSat => BinOp::SubSat,
        AssignOp::MulSat => BinOp::MulSat,
        AssignOp::BitAnd => BinOp::BitAnd,
        AssignOp::BitOr => BinOp::BitOr,
        AssignOp::BitXor => BinOp::BitXor,
        AssignOp::Shl => BinOp::Shl,
        AssignOp::Shr => BinOp::Shr,
    })
}

// ------------------------------------------------------------------ exits and defers

impl<'a, 'b> Lowerer<'a, 'b> {
    fn exit(&mut self, stop: usize, kind: ExitKind) {
        let span = self.stmt_span;
        let is_error = matches!(kind, ExitKind::Return { is_error: true, .. });
        // The return value's temporary must survive the cleanup; its storage ends
        // with the terminator, after the value is read.
        let mut end = None;
        if let ExitKind::Return {
            value: Some(Operand::Place(p, _)),
            ..
        } = &kind
        {
            end = p.root();
            if let Some(l) = end {
                for f in &mut self.frames {
                    f.temps.retain(|t| *t != l);
                    f.locals.retain(|t| *t != l);
                    if f.scope_binding == Some(l) {
                        f.scope_binding = None;
                    }
                }
            }
        }
        self.unwind(stop, is_error, span);
        if !self.reachable {
            return;
        }
        match kind {
            ExitKind::Return { value, is_error } => self.end_block(Terminator::Return {
                value,
                is_error,
                end,
            }),
            ExitKind::Goto(b) => self.end_block(Terminator::Goto(b)),
        }
    }

    fn unwind(&mut self, stop: usize, error: bool, exit_span: Span) {
        let top = self.frames.len();
        let first = stop.min(top);
        for i in (first..top).rev() {
            self.end_frame(i, error, exit_span);
        }
    }

    fn end_frame(&mut self, idx: usize, error: bool, exit_span: Span) {
        let (defers, temps, locals, scope, scope_binding, frame_span) = {
            let f = &self.frames[idx];
            (
                f.defers.clone(),
                f.temps.clone(),
                f.locals.clone(),
                f.scope,
                f.scope_binding,
                f.span,
            )
        };
        for d in defers.into_iter().rev() {
            if !d.err || error {
                self.copy_defer(&d, exit_span);
            }
        }
        for l in temps.into_iter().rev() {
            let sp = self.cfg.local(l).span;
            self.emit_at(StmtKind::StorageEnd(l), sp);
        }
        for l in locals.into_iter().rev() {
            let sp = self.cfg.local(l).span;
            self.emit_at(StmtKind::StorageEnd(l), sp);
        }
        if let Some(s) = scope {
            if error {
                self.emit_at(StmtKind::Cancel(s), frame_span);
            }
            let next = self.new_block();
            self.term_at(Terminator::ScopeWait { scope: s, next }, frame_span);
            self.cur = next;
            self.reachable = true;
            if let Some(binding) = scope_binding {
                self.emit_at(StmtKind::StorageEnd(binding), frame_span);
            }
            self.emit_at(StmtKind::StorageEnd(s), frame_span);
        }
    }

    fn copy_defer(&mut self, entry: &DeferEntry, exit_span: Span) {
        let saved = self.copy;
        let saved_span = self.stmt_span;
        self.copy = Some(DeferCopy {
            defer_span: entry.span,
            exit_span,
        });
        let span = self.pkg.block(entry.body).span;
        self.push_frame(span);
        self.lower_stmts(entry.body);
        if self.reachable {
            self.finish_frame();
        } else {
            self.frames.pop();
        }
        self.copy = saved;
        self.stmt_span = saved_span;
    }
}

// ------------------------------------------------------------------ expressions

impl<'a, 'b> Lowerer<'a, 'b> {
    fn lower_expr(&mut self, e: ExprId) -> Option<Operand> {
        if !self.reachable {
            return None;
        }
        let op = match self.lower_expr_value(e) {
            Some(op) => op,
            None => {
                // A reachable expression that yields no value is a lowering gap,
                // never a silent drop. Every construct that legitimately gives no
                // value (a diverging branch, a trap) has already left the block.
                if self.reachable && self.failed.is_none() {
                    self.fail("an expression that the lowering dropped");
                }
                return None;
            }
        };
        self.apply_coercions(e, op)
    }

    fn apply_coercions(&mut self, e: ExprId, op: Operand) -> Option<Operand> {
        // Some compile-time-only arguments (a `@debug` format literal) have no
        // `ExprInfo`; they cannot carry a coercion either.
        let Some(info) = self.body.expr(e) else {
            return Some(op);
        };
        let (fn_dyn, coerce, ty) = (info.fn_dyn.clone(), info.coerce.clone(), info.ty);
        let span = self.span(e);
        if let Some(fn_dyn) = fn_dyn {
            let value = Rvalue::DynCast {
                operand: op,
                coercion: DynCoercionKind::Fn(Box::new(fn_dyn)),
            };
            return Some(self.materialize(value, ty, span));
        }
        if let Some(coerce) = coerce {
            let name = self.cx.sema.qualified_name(coerce.interface);
            let value = Rvalue::DynCast {
                operand: op,
                coercion: DynCoercionKind::Interface {
                    name,
                    coercion: Box::new(coerce),
                },
            };
            return Some(self.materialize(value, ty, span));
        }
        Some(op)
    }

    fn lower_expr_value(&mut self, e: ExprId) -> Option<Operand> {
        let kind = self.pkg.expr(e).kind.clone();
        let span = self.span(e);
        let ty = self.ty(e);
        match kind {
            ExprKind::Paren(inner) => self.lower_expr(inner),
            ExprKind::TypeApply { base, .. } => self.lower_expr(base),
            ExprKind::Unsafe { body, .. } => {
                self.emit_at(StmtKind::EnterUnsafe, self.span(e));
                self.lower_expr(body)
            }
            ExprKind::Int {
                value, negative, ..
            } => {
                let mut v = value as i128;
                if negative {
                    v = -v;
                }
                let c = if matches!(self.kind(ty), TyKind::Float(_)) {
                    Const::Float {
                        ty,
                        value: v as f64,
                    }
                } else {
                    Const::Int { ty, value: v }
                };
                Some(Operand::Const(c))
            }
            ExprKind::Float(f) => Some(Operand::Const(Const::Float { ty, value: f })),
            ExprKind::Bool(b) => Some(Operand::Const(Const::Bool(b))),
            ExprKind::Str(bytes) => Some(Operand::Const(Const::Str(bytes))),
            ExprKind::CStr(bytes) => Some(Operand::Const(Const::CStr(bytes))),
            ExprKind::Path(_) => self.lower_path(e),
            ExprKind::Field { .. } => self.lower_field_read(e),
            ExprKind::TupleField { .. } => self.lower_field_read(e),
            ExprKind::Index { .. } => self.lower_index_read(e),
            ExprKind::Slice { .. } => self.lower_slice(e),
            ExprKind::Deref { .. } => {
                let (place, checks) = self.lower_place(e)?;
                self.emit_checks(checks);
                let read = self.read_place_at(e, place, ty);
                Some(self.snapshot(read, ty, span))
            }
            ExprKind::Unary { op, expr, .. } => self.lower_unary(e, op, expr),
            ExprKind::Binary { op, lhs, rhs, .. } => self.lower_binary(e, op, lhs, rhs),
            ExprKind::Cast { expr, .. } => {
                let kind = match self.res(e) {
                    ExprRes::Cast(k) => k,
                    _ => {
                        self.fail("a cast with no conversion information");
                        return None;
                    }
                };
                let op_ty = self.ty(expr);
                let op = self.lower_expr(expr)?;
                let op = self.snapshot(op, op_ty, self.span(expr));
                Some(self.materialize(Rvalue::Cast { kind, operand: op }, ty, span))
            }
            ExprKind::Call { args, .. } => match self.res(e) {
                ExprRes::Call(_) => self.lower_call(e),
                ExprRes::IdxMethod(m) => self.lower_idx_method(e, &args, m, ty, span),
                ExprRes::SliceAt { clamped } => self.lower_slice_at(e, &args, clamped, ty, span),
                _ => {
                    self.fail("a call with no call information");
                    None
                }
            },
            ExprKind::StructLit { .. } | ExprKind::StructShorthand { .. } => self.lower_struct(e),
            ExprKind::EnumLit { .. } => self.lower_variant(e),
            ExprKind::ArrayLit { elems, .. } => {
                let mut fields = Vec::new();
                for x in elems {
                    let xt = self.ty(x);
                    let op = self.lower_expr(x)?;
                    fields.push(self.snapshot(op, xt, self.span(x)));
                }
                Some(self.materialize(
                    Rvalue::Aggregate {
                        kind: AggregateKind::Array,
                        ty,
                        ty_name: self.ty_name(ty),
                        fields,
                    },
                    ty,
                    span,
                ))
            }
            ExprKind::Tuple(elems) => {
                let mut fields = Vec::new();
                for x in elems {
                    let xt = self.ty(x);
                    let op = self.lower_expr(x)?;
                    fields.push(self.snapshot(op, xt, self.span(x)));
                }
                Some(self.materialize(
                    Rvalue::Aggregate {
                        kind: AggregateKind::Tuple,
                        ty,
                        ty_name: self.ty_name(ty),
                        fields,
                    },
                    ty,
                    span,
                ))
            }
            ExprKind::Builtin { name, args, .. } => {
                if let ExprRes::Index { .. } = self.res(e) {
                    return self.lower_index_read(e);
                }
                match self.res(e) {
                    ExprRes::Gather => self.lower_gather(&name, &args, ty, span),
                    ExprRes::Scatter => self.lower_scatter(&name, &args, span),
                    ExprRes::IdxMethod(m) => self.lower_idx_method(e, &args, m, ty, span),
                    ExprRes::SliceAt { clamped } => {
                        self.lower_slice_at(e, &args, clamped, ty, span)
                    }
                    _ => self.lower_builtin(e, &name, &args, ty, span),
                }
            }
            ExprKind::If {
                cond,
                then,
                otherwise,
            } => self.lower_if_expr(e, &cond, then, otherwise),
            ExprKind::Match { scrutinee, arms } => {
                self.lower_match(scrutinee, &arms, Some(ty), span)
            }
            ExprKind::Catch { .. } => self.lower_catch(e),
            ExprKind::Orelse { .. } => self.lower_orelse(e),
            ExprKind::Spawn { call, scope } => self.lower_spawn(call, scope, Some(ty), span),
            ExprKind::Dispatch {
                call,
                domain,
                executor,
            } => {
                let info = match self.res(e) {
                    ExprRes::Dispatch(info) => info,
                    _ => {
                        self.fail("a dispatch with no dispatch information");
                        return None;
                    }
                };
                self.lower_dispatch(call, domain, executor, &info, Some(ty), span)
            }
            ExprKind::Closure(_) => {
                let info = self.body.closures.get(&e).cloned();
                let Some(info) = info else {
                    self.fail("a closure with no closure information");
                    return None;
                };
                self.lower_closure_expr(e, &info)
            }
            ExprKind::Block(b) => {
                self.lower_block(b);
                None
            }
            ExprKind::Range { .. } => {
                self.fail("a range used as a value");
                None
            }
            ExprKind::Poison => {
                self.fail("a poison expression");
                None
            }
        }
    }

    fn lower_path(&mut self, e: ExprId) -> Option<Operand> {
        let ty = self.ty(e);
        let span = self.span(e);
        match self.res(e) {
            ExprRes::Local(l) => {
                let place = self.local_place(l);
                let read = self.read_place_at(e, place, ty);
                Some(read)
            }
            ExprRes::Static(d) => {
                let place = Place {
                    base: PlaceBase::Static(d),
                    proj: Vec::new(),
                };
                Some(self.read_place_at(e, place, ty))
            }
            ExprRes::Fn(f) => {
                let name = self.fn_name(&f);
                Some(self.materialize(
                    Rvalue::Use(Operand::Const(Const::Fn {
                        fn_ref: Box::new(f),
                        name,
                    })),
                    ty,
                    span,
                ))
            }
            ExprRes::Const(d) => {
                let name = self.cx.sema.qualified_name(d);
                Some(Operand::Const(Const::Named { def: d, name }))
            }
            ExprRes::ConstParam(d) => {
                let name = self.cx.sema.def(d).name.clone();
                Some(Operand::Const(Const::Param { def: d, name }))
            }
            ExprRes::Variant(v) => Some(self.materialize(
                Rvalue::Aggregate {
                    kind: AggregateKind::Variant {
                        index: v.index,
                        enum_ty: v.enum_ty,
                    },
                    ty,
                    ty_name: self.ty_name(ty),
                    fields: Vec::new(),
                },
                ty,
                span,
            )),
            _ => {
                self.fail("a path that is not a value");
                None
            }
        }
    }

    fn lower_field_read(&mut self, e: ExprId) -> Option<Operand> {
        let ty = self.ty(e);
        let span = self.span(e);
        match self.res(e) {
            ExprRes::SliceLen { derefs } => {
                let base = self.field_base(e)?;
                let mut checks = Vec::new();
                let mut place = self.place_base(base, &mut checks)?;
                self.emit_checks(checks);
                for _ in 0..derefs {
                    place = place.deref();
                }
                Some(self.materialize(Rvalue::SliceLen(place), ty, span))
            }
            ExprRes::SlicePtr { derefs } => {
                let base = self.field_base(e)?;
                let mut checks = Vec::new();
                let mut place = self.place_base(base, &mut checks)?;
                self.emit_checks(checks);
                for _ in 0..derefs {
                    place = place.deref();
                }
                Some(self.materialize(Rvalue::SlicePtr(place), ty, span))
            }
            ExprRes::Fn(f) => {
                let name = self.fn_name(&f);
                Some(self.materialize(
                    Rvalue::Use(Operand::Const(Const::Fn {
                        fn_ref: Box::new(f),
                        name,
                    })),
                    ty,
                    span,
                ))
            }
            ExprRes::Variant(v) => Some(self.materialize(
                Rvalue::Aggregate {
                    kind: AggregateKind::Variant {
                        index: v.index,
                        enum_ty: v.enum_ty,
                    },
                    ty,
                    ty_name: self.ty_name(ty),
                    fields: Vec::new(),
                },
                ty,
                span,
            )),
            ExprRes::ArrayLen { len, .. } => {
                let c = match len {
                    ConstArg::Value(ConstValue::Int { value, .. }) => Const::Int { ty, value },
                    ConstArg::Param(d) => {
                        let name = self.cx.sema.def(d).name.clone();
                        Const::Param { def: d, name }
                    }
                    ConstArg::Value(ConstValue::Bool(b)) => Const::Bool(b),
                    ConstArg::Poison => {
                        self.fail("an array length that is poison");
                        return None;
                    }
                };
                Some(Operand::Const(c))
            }
            _ => {
                let (place, checks) = self.lower_place(e)?;
                self.emit_checks(checks);
                let read = self.read_place_at(e, place, ty);
                Some(read)
            }
        }
    }

    fn field_base(&self, e: ExprId) -> Option<ExprId> {
        match &self.pkg.expr(self.unparen(e)).kind {
            ExprKind::Field { base, .. } => Some(*base),
            _ => None,
        }
    }

    fn lower_index_read(&mut self, e: ExprId) -> Option<Operand> {
        let ty = self.ty(e);
        let (place, checks) = self.lower_place(e)?;
        self.emit_checks(checks);
        let read = self.read_place_at(e, place, ty);
        Some(read)
    }

    fn apply_derefs(mut place: Place, derefs: u32) -> Place {
        for _ in 0..derefs {
            place = place.deref();
        }
        place
    }

    fn emit_checks(&mut self, checks: Vec<Check>) {
        for c in checks {
            self.emit_check(c);
        }
    }

    /// Lower a place expression. The checks it needs are returned rather than emitted,
    /// so an assignment can place them after the value (pipeline section 5).
    fn lower_place(&mut self, e: ExprId) -> Option<(Place, Vec<Check>)> {
        let mut checks = Vec::new();
        let place = self.lower_place_into(e, &mut checks)?;
        Some((place, checks))
    }

    fn lower_place_into(&mut self, e: ExprId, checks: &mut Vec<Check>) -> Option<Place> {
        let e = self.unparen(e);
        let kind = self.pkg.expr(e).kind.clone();
        let res = self.res(e);
        match kind {
            ExprKind::Path(_) => match res {
                ExprRes::Local(l) => Some(self.local_place(l)),
                ExprRes::Static(d) => Some(Place {
                    base: PlaceBase::Static(d),
                    proj: Vec::new(),
                }),
                _ => None,
            },
            ExprKind::Field { base, name, .. } => match res {
                ExprRes::Field { derefs, index, .. } => {
                    let mut place = self.place_base(base, checks)?;
                    place = Self::apply_derefs(place, derefs);
                    Some(place.field(index, Some(name)))
                }
                _ => None,
            },
            ExprKind::TupleField { base, index } => match res {
                ExprRes::TupleField { derefs } => {
                    let mut place = self.place_base(base, checks)?;
                    place = Self::apply_derefs(place, derefs);
                    Some(place.tuple_field(index as usize))
                }
                _ => None,
            },
            ExprKind::Index { base, index, .. } => match res {
                ExprRes::Index { derefs } => {
                    let mut place = self.place_base(base, checks)?;
                    place = Self::apply_derefs(place, derefs);
                    let index_ty = self.ty(index);
                    let index_op = self.lower_expr(index)?;
                    let index_op = self.snapshot(index_op, index_ty, self.span(index));
                    let base_ty = self.strip_derefs(self.ty(base), derefs);
                    if Self::is_indexable(&self.kind(base_ty)) && !self.is_vector(base_ty) {
                        checks.push(self.bounds_index(
                            place.clone(),
                            index_op.clone(),
                            self.span(e),
                        ));
                    }
                    Some(place.index(index_op))
                }
                _ => None,
            },
            ExprKind::Deref { base, .. } => {
                let raw = matches!(res, ExprRes::Deref { raw: true });
                let place = self.place_base(base, checks)?;
                Some(if raw {
                    place.deref_raw()
                } else {
                    place.deref()
                })
            }
            ExprKind::Builtin { name, args, .. } if name == "unchecked_index" => {
                let derefs = match res {
                    ExprRes::Index { derefs } => derefs,
                    _ => 0,
                };
                let mut place = self.place_base(args[0], checks)?;
                place = Self::apply_derefs(place, derefs);
                let index_ty = self.ty(args[1]);
                let index_op = self.lower_expr(args[1])?;
                let index_op = self.snapshot(index_op, index_ty, self.span(args[1]));
                Some(place.index(index_op))
            }
            _ => None,
        }
    }

    fn is_vector(&self, ty: TypeId) -> bool {
        matches!(self.kind(ty), TyKind::Vector(..))
    }

    /// Whether the type can be indexed with a bounds check (array or slice).
    fn is_indexable(k: &TyKind) -> bool {
        matches!(k, TyKind::Array(..) | TyKind::Slice(..))
    }

    fn place_base(&mut self, base: ExprId, checks: &mut Vec<Check>) -> Option<Place> {
        if let Some(p) = self.lower_place_into(base, checks) {
            return Some(p);
        }
        let ty = self.ty(base);
        let op = self.lower_expr(base)?;
        Some(self.place_of_operand(op, ty, self.span(base)))
    }

    fn lower_slice(&mut self, e: ExprId) -> Option<Operand> {
        let res = self.res(e);
        let (base, lo, hi, derefs, of_array) = match &self.pkg.expr(e).kind {
            ExprKind::Slice { base, lo, hi, .. } => match res {
                ExprRes::Slice {
                    derefs, of_array, ..
                } => (*base, *lo, *hi, derefs, of_array),
                _ => {
                    self.fail("a slice with no slice information");
                    return None;
                }
            },
            _ => {
                self.fail("a slice that is not a slice");
                return None;
            }
        };
        let span = self.span(e);
        let ty = self.ty(e);
        let mut base_checks = Vec::new();
        let mut place = self.place_base(base, &mut base_checks)?;
        self.emit_checks(base_checks);
        place = Self::apply_derefs(place, derefs);
        let mut checks = Vec::new();
        let i64_ = self.i64_ty();
        let lo_op = match lo {
            Some(lo) => {
                let t = self.ty(lo);
                let op = self.lower_expr(lo)?;
                self.snapshot(op, t, self.span(lo))
            }
            None => Operand::Const(Const::Int { ty: i64_, value: 0 }),
        };
        let hi_op = match hi {
            Some(hi) => {
                let t = self.ty(hi);
                let op = self.lower_expr(hi)?;
                self.snapshot(op, t, self.span(hi))
            }
            None => {
                let len = self.materialize(Rvalue::SliceLen(place.clone()), i64_, span);
                self.snapshot(len, i64_, span)
            }
        };
        checks.push(self.bounds_slice(place.clone(), lo_op.clone(), hi_op.clone(), span));
        self.emit_checks(checks);
        let borrow = if of_array {
            let m = match self.kind(ty) {
                TyKind::Slice(m, _) => m,
                _ => Mutability::Shared,
            };
            Some(m)
        } else {
            None
        };
        Some(self.materialize(
            Rvalue::Slice {
                base: place,
                lo: lo_op,
                hi: hi_op,
                borrow,
                two_phase: borrow == Some(Mutability::Mut) && self.arg_depth > 0,
            },
            ty,
            span,
        ))
    }

    fn lower_unary(&mut self, e: ExprId, op: UnOp, operand: ExprId) -> Option<Operand> {
        let ty = self.ty(e);
        let span = self.span(e);
        match op {
            UnOp::Borrow | UnOp::BorrowMut => {
                let inner = self.unparen(operand);
                if matches!(
                    self.body.expr(inner).map(|i| &i.res),
                    Some(ExprRes::Slice { of_array: true, .. })
                ) {
                    return self.lower_expr(inner);
                }
                let mutability = if op == UnOp::BorrowMut {
                    Mutability::Mut
                } else {
                    Mutability::Shared
                };
                // 02-memory §11: a shared borrow may be taken of a temporary. The
                // temporary lives to the end of the statement, so a borrow that is
                // still live after it is E1105 in phase 9.
                let (place, checks) =
                    if mutability == Mutability::Shared && !self.is_place_expr(operand) {
                        let inner_ty = self.ty(operand);
                        let value = self.lower_expr(operand)?;
                        (
                            self.place_of_operand(value, inner_ty, self.span(operand)),
                            Vec::new(),
                        )
                    } else {
                        self.lower_place(operand)?
                    };
                self.emit_checks(checks);
                let two_phase = mutability == Mutability::Mut && self.arg_depth > 0;
                Some(self.materialize(
                    Rvalue::Borrow {
                        place,
                        mutability,
                        two_phase,
                    },
                    ty,
                    span,
                ))
            }
            UnOp::Try => self.lower_try(e, operand),
            UnOp::Neg | UnOp::NegWrap | UnOp::Not | UnOp::BitNot => {
                let op_ty = self.ty(operand);
                let operand_op = self.lower_expr(operand)?;
                let operand_op = self.snapshot(operand_op, op_ty, self.span(operand));
                let value = self.materialize(
                    Rvalue::Unary {
                        op,
                        operand: operand_op.clone(),
                    },
                    ty,
                    span,
                );
                if matches!(op, UnOp::Neg) && self.int_like(ty) {
                    self.emit_check(Check {
                        kind: CheckKind::Overflow,
                        place: None,
                        lo: None,
                        hi: None,
                        operands: vec![operand_op],
                        places: Vec::new(),
                        ty: Some(ty),
                        ty_name: None,
                        op: Some("-".to_string()),
                        span,
                    });
                }
                Some(value)
            }
        }
    }

    fn lower_binary(&mut self, e: ExprId, op: BinOp, lhs: ExprId, rhs: ExprId) -> Option<Operand> {
        let ty = self.ty(e);
        let span = self.span(e);
        if matches!(op, BinOp::And | BinOp::Or) {
            return self.lower_short_circuit(e, op, lhs, rhs);
        }
        let lhs_ty = self.ty(lhs);
        let lhs_op = self.lower_expr(lhs)?;
        let lhs_op = self.snapshot(lhs_op, lhs_ty, self.span(lhs));
        let rhs_ty = self.ty(rhs);
        let rhs_op = self.lower_expr(rhs)?;
        let rhs_op = self.snapshot(rhs_op, rhs_ty, self.span(rhs));
        let value = self.materialize(
            Rvalue::Binary {
                op,
                lhs: lhs_op.clone(),
                rhs: rhs_op.clone(),
            },
            ty,
            span,
        );
        let arith_ty = if matches!(op, BinOp::Shl | BinOp::Shr) {
            lhs_ty
        } else {
            ty
        };
        self.check_arith(op, &lhs_op, &rhs_op, arith_ty, span);
        Some(value)
    }

    fn lower_short_circuit(
        &mut self,
        _e: ExprId,
        op: BinOp,
        lhs: ExprId,
        rhs: ExprId,
    ) -> Option<Operand> {
        let ty = self.bool_ty();
        let span = self.span(lhs);
        let lhs_ty = self.ty(lhs);
        let lhs_op = self.lower_expr(lhs)?;
        let lhs_op = self.snapshot(lhs_op, lhs_ty, span);
        let result = self.new_temp(ty, span);
        let rhs_block = self.new_block();
        let short_block = self.new_block();
        let (then_block, else_block) = if op == BinOp::And {
            (rhs_block, short_block)
        } else {
            (short_block, rhs_block)
        };
        self.term(Terminator::Branch {
            cond: lhs_op,
            then_block,
            else_block,
        });
        self.kill();
        let join = self.new_block();
        // The short answer: false for `&&`, true for `||`.
        self.cur = short_block;
        self.reachable = true;
        let answer = op == BinOp::Or;
        self.emit(StmtKind::Assign {
            place: Place::local(result),
            value: Rvalue::Use(Operand::Const(Const::Bool(answer))),
        });
        self.goto(join);
        // The right side: its value is the answer.
        self.cur = rhs_block;
        self.reachable = true;
        let rhs_mark = self.temp_mark();
        let rhs_ty = self.ty(rhs);
        let rhs_op = self.lower_expr(rhs)?;
        let rhs_op = self.snapshot(rhs_op, rhs_ty, self.span(rhs));
        self.emit(StmtKind::Assign {
            place: Place::local(result),
            value: Rvalue::Use(rhs_op),
        });
        self.end_temps(rhs_mark);
        let rhs_reach = self.reachable;
        if rhs_reach {
            self.goto(join);
        }
        self.cur = join;
        self.reachable = true;
        Some(Operand::Place(
            Place::local(result),
            self.consume_use(self.bool_ty()),
        ))
    }

    fn lower_if_expr(
        &mut self,
        e: ExprId,
        cond: &hir::Cond,
        then: ExprId,
        otherwise: ExprId,
    ) -> Option<Operand> {
        let ty = self.ty(e);
        let span = self.span(e);
        let then_block = self.new_block();
        let else_block = self.new_block();
        // The destination is started before the branch so that it is live on both.
        let dest = if ty == self.cx.decls.types.never {
            None
        } else {
            let l = self.new_temp(ty, span);
            Some(l)
        };
        let bind = self.lower_cond(cond, then_block, else_block)?;
        let join = self.new_block();
        // then: its temporaries and bindings live in its own frame.
        self.cur = then_block;
        self.reachable = true;
        self.push_frame(self.span(then));
        if let Some((pat, place)) = bind {
            self.bind_pattern(pat, place);
        }
        let then_ok = self.lower_value_branch(then, dest, ty);
        if then_ok && self.reachable {
            self.finish_frame();
        } else {
            self.frames.pop();
        }
        let then_reach = then_ok && self.reachable;
        if then_reach {
            self.goto(join);
        }
        // else
        self.cur = else_block;
        self.reachable = true;
        self.push_frame(self.span(otherwise));
        let else_ok = self.lower_value_branch(otherwise, dest, ty);
        if else_ok && self.reachable {
            self.finish_frame();
        } else {
            self.frames.pop();
        }
        let else_reach = else_ok && self.reachable;
        if else_reach {
            self.goto(join);
        }
        self.cur = join;
        self.reachable = then_reach || else_reach;
        dest.map(|l| Operand::Place(Place::local(l), self.consume_use(ty)))
    }

    /// Lower one branch of an `if` or `match` expression: a value goes to `dest`, or
    /// the branch leaves the function. Returns whether it reaches the end with a value.
    fn lower_value_branch(&mut self, branch: ExprId, dest: Option<Local>, _ty: TypeId) -> bool {
        let Some(op) = self.lower_expr(branch) else {
            return false;
        };
        if let Some(dest) = dest {
            self.emit(StmtKind::Assign {
                place: Place::local(dest),
                value: Rvalue::Use(op),
            });
        } else {
            self.emit(StmtKind::Use(op));
        }
        self.reachable
    }

    fn lower_cond(
        &mut self,
        cond: &hir::Cond,
        then_block: Block,
        else_block: Block,
    ) -> Option<Option<(PatId, Place)>> {
        let expr_ty = self.ty(cond.expr);
        let op = self.lower_expr(cond.expr)?;
        match cond.let_pattern {
            None => {
                let op = self.snapshot(op, expr_ty, self.span(cond.expr));
                self.term(Terminator::Branch {
                    cond: op,
                    then_block,
                    else_block,
                });
                self.kill();
                Some(None)
            }
            Some(pat) => {
                let place = self.place_of_operand(op, expr_ty, self.span(cond.expr));
                // The tests' own temporaries live in a region that ends on the
                // successful and on the failed path; the scrutinee does not.
                self.push_frame(cond.span);
                self.test_pattern(pat, place.clone(), else_block);
                let test_temps = self
                    .frames
                    .last()
                    .map(|f| f.temps.clone())
                    .unwrap_or_default();
                self.frames.pop();
                self.end_temp_list(&test_temps);
                self.goto(then_block);
                Some(Some((pat, place)))
            }
        }
    }
}

// ------------------------------------------------------------------ control flow expressions

impl<'a, 'b> Lowerer<'a, 'b> {
    fn lower_match(
        &mut self,
        scrutinee: ExprId,
        arms: &[hir::Arm],
        dest_ty: Option<TypeId>,
        span: Span,
    ) -> Option<Operand> {
        let st_ty = self.ty(scrutinee);
        let op = self.lower_expr(scrutinee)?;
        let place = self.place_of_operand(op, st_ty, self.span(scrutinee));
        let dest = match dest_ty {
            Some(ty) if ty != self.cx.decls.types.never => Some(self.new_temp(ty, span)),
            _ => None,
        };
        let join = self.new_block();
        let first_entry = self.cur;
        let mut arm_entry = first_entry;
        let mut any_reach = false;
        for arm in arms {
            self.cur = arm_entry;
            self.reachable = true;
            let this_arm_next = self.new_block();
            let mut alt_entry = arm_entry;
            let n_alts = arm.patterns.len().max(1);
            for (i, pat) in arm.patterns.iter().enumerate() {
                let last = i + 1 == n_alts;
                let alt_next = if last {
                    this_arm_next
                } else {
                    self.new_block()
                };
                self.cur = alt_entry;
                self.reachable = true;
                // The tests are their own storage region.
                self.push_frame(arm.span);
                self.test_pattern(*pat, place.clone(), alt_next);
                let test_temps = self
                    .frames
                    .last()
                    .map(|f| f.temps.clone())
                    .unwrap_or_default();
                self.frames.pop();
                // Tests passed.
                self.end_temp_list(&test_temps);
                // The arm body.
                self.reachable = true;
                self.push_frame(arm.span);
                let mut body_ready = true;
                if let Some(guard) = arm.guard {
                    match self.bind_guard(*pat, place.clone(), guard, this_arm_next) {
                        Some(_) => self.bind_pattern(*pat, place.clone()),
                        None => body_ready = false,
                    }
                } else {
                    self.bind_pattern(*pat, place.clone());
                }
                if body_ready {
                    let body_reach = self.lower_arm_body(arm, dest);
                    if body_reach && self.reachable {
                        self.finish_frame();
                        self.stmt_span = arm.span;
                        self.goto(join);
                        any_reach = true;
                    } else {
                        self.frames.pop();
                    }
                } else {
                    self.frames.pop();
                }
                alt_entry = alt_next;
            }
            arm_entry = this_arm_next;
        }
        // No arm matched: exhaustiveness makes this unreachable, but it is well formed.
        self.cur = arm_entry;
        self.reachable = false;
        self.term(Terminator::Unreachable);
        self.cur = join;
        self.reachable = any_reach;
        dest.zip(dest_ty)
            .map(|(l, ty)| Operand::Place(Place::local(l), self.consume_use(ty)))
    }

    /// Bind a guard's names through shared borrows (or copies), lower the guard, and
    /// branch: true to the block returned (with the borrows ended, the real bindings
    /// still to be made), false to `fail` (with the borrows ended).
    fn bind_guard(
        &mut self,
        pat: PatId,
        place: Place,
        guard: ExprId,
        fail: Block,
    ) -> Option<Block> {
        let bindings = self.collect_bindings(pat, place);
        let mark = self.temp_mark();
        for (local, p) in bindings {
            let ty = self.local_ty(local);
            let mode = self.local_mode(local);
            let (shadow_ty, value, through) = match mode {
                BindMode::Copy => (ty, Rvalue::Use(Operand::Place(p, PlaceUse::Copy)), false),
                BindMode::Move => (
                    self.ref_ty(Mutability::Shared, ty),
                    Rvalue::Borrow {
                        place: p,
                        mutability: Mutability::Shared,
                        two_phase: false,
                    },
                    true,
                ),
                BindMode::Ref => (
                    self.ref_ty(Mutability::Shared, ty),
                    Rvalue::Borrow {
                        place: p,
                        mutability: Mutability::Shared,
                        two_phase: false,
                    },
                    false,
                ),
                BindMode::RefMut => (
                    self.ref_ty(Mutability::Mut, ty),
                    Rvalue::Borrow {
                        place: p,
                        mutability: Mutability::Mut,
                        two_phase: false,
                    },
                    false,
                ),
            };
            let shadow = self.new_temp(shadow_ty, self.stmt_span);
            self.emit(StmtKind::Assign {
                place: Place::local(shadow),
                value,
            });
            let gp = if through {
                Place::local(shadow).deref()
            } else {
                Place::local(shadow)
            };
            self.guard_places.insert(local, gp);
        }
        self.guard_reads = true;
        let gop = self.lower_expr(guard);
        self.guard_reads = false;
        let Some(gop) = gop else {
            self.guard_places.clear();
            return None;
        };
        let gty = self.ty(guard);
        let gop = self.snapshot(gop, gty, self.span(guard));
        let temps: Vec<Local> = self
            .frames
            .last()
            .map(|f| f.temps[mark.min(f.temps.len())..].to_vec())
            .unwrap_or_default();
        let ok = self.new_block();
        let bad = self.new_block();
        self.term(Terminator::Branch {
            cond: gop,
            then_block: ok,
            else_block: bad,
        });
        self.kill();
        // Guard false: the borrows die, nothing is bound.
        self.cur = bad;
        self.reachable = true;
        self.end_temp_list(&temps);
        self.goto(fail);
        // Guard true: the borrows die before the real bindings move anything.
        self.cur = ok;
        self.reachable = true;
        self.end_temp_list(&temps);
        if let Some(f) = self.frames.last_mut() {
            f.temps.truncate(mark);
        }
        self.guard_places.clear();
        Some(ok)
    }

    /// Emit storage ends for a captured list of temporaries, in reverse order.
    fn end_temp_list(&mut self, temps: &[Local]) {
        for &l in temps.iter().rev() {
            let span = self.cfg.local(l).span;
            self.emit_at(StmtKind::StorageEnd(l), span);
        }
    }

    fn lower_arm_body(&mut self, arm: &hir::Arm, dest: Option<Local>) -> bool {
        match &arm.body {
            hir::ArmBody::Block(b) => self.lower_block(*b),
            hir::ArmBody::Expr(e) => {
                let Some(op) = self.lower_expr(*e) else {
                    return false;
                };
                match dest {
                    Some(l) => self.emit(StmtKind::Assign {
                        place: Place::local(l),
                        value: Rvalue::Use(op),
                    }),
                    None => {
                        if !matches!(op, Operand::Const(Const::Unit)) {
                            self.emit(StmtKind::Use(op));
                        }
                    }
                }
                self.reachable
            }
        }
    }

    fn lower_catch(&mut self, e: ExprId) -> Option<Operand> {
        let (value, binding, handler, span) = match &self.pkg.expr(e).kind {
            ExprKind::Catch {
                value,
                binding,
                handler,
                catch_span,
                ..
            } => (*value, *binding, handler.clone(), *catch_span),
            _ => return None,
        };
        let ty = self.ty(e);
        let vt = self.ty(value);
        let vop = self.lower_expr(value)?;
        let vplace = self.place_of_operand(vop, vt, self.span(value));
        let dest = self.new_temp(ty, span);
        let ok_block = self.new_block();
        let err_block = self.new_block();
        let join = self.new_block();
        self.switch_result(vplace.clone(), vt, 0, ok_block, 1, err_block);
        // ok: move the payload out. The arm starts with the tag it chose.
        self.cur = ok_block;
        self.reachable = true;
        self.note_variant(vplace.clone(), vt, 0, true);
        self.push_frame(span);
        let payload = vplace.clone().payload(0);
        let op = self.read_payload(payload, ty, vt);
        let op = self.snapshot(op, ty, span);
        self.emit(StmtKind::Assign {
            place: Place::local(dest),
            value: Rvalue::Use(op),
        });
        if self.reachable {
            self.finish_frame();
            self.goto(join);
        } else {
            self.frames.pop();
        }
        // err: bind the error and lower the handler.
        self.cur = err_block;
        self.reachable = true;
        self.note_variant(vplace.clone(), vt, 1, true);
        self.push_frame(span);
        if binding.is_none() {
            let epay = vplace.clone().payload(1);
            self.emit(StmtKind::Use(Operand::Place(epay, PlaceUse::Move)));
        }
        if let Some(local) = binding {
            let ety = self.local_ty(local);
            let epay = vplace.clone().payload(1);
            let l = self.ensure_local(local);
            let sp = self.cfg.local(l).span;
            self.emit_at(StmtKind::StorageStart(l), sp);
            let read = self.read_payload(epay, ety, vt);
            self.emit(StmtKind::Assign {
                place: Place::local(l),
                value: Rvalue::Use(read),
            });
            if let Some(f) = self.frames.last_mut() {
                f.temps.push(l);
            }
        }
        let handler_reach = match &handler {
            hir::Handler::Expr(h) => {
                if let Some(op) = self.lower_expr(*h) {
                    self.emit(StmtKind::Assign {
                        place: Place::local(dest),
                        value: Rvalue::Use(op),
                    });
                    true
                } else {
                    false
                }
            }
            hir::Handler::Block(b) => self.lower_block(*b),
        };
        if handler_reach && self.reachable {
            self.finish_frame();
            self.goto(join);
        } else {
            self.frames.pop();
        }
        self.cur = join;
        self.reachable = true;
        Some(Operand::Place(Place::local(dest), self.consume_use(ty)))
    }

    fn lower_orelse(&mut self, e: ExprId) -> Option<Operand> {
        let (value, handler, span) = match &self.pkg.expr(e).kind {
            ExprKind::Orelse {
                value,
                handler,
                orelse_span,
            } => (*value, handler.clone(), *orelse_span),
            _ => return None,
        };
        let ty = self.ty(e);
        let vt = self.ty(value);
        let vop = self.lower_expr(value)?;
        let vplace = self.place_of_operand(vop, vt, self.span(value));
        let dest = self.new_temp(ty, span);
        let none_block = self.new_block();
        let some_block = self.new_block();
        let join = self.new_block();
        self.switch_result(vplace.clone(), vt, 0, none_block, 1, some_block);
        // none: the handler.
        self.cur = none_block;
        self.reachable = true;
        self.note_variant(vplace.clone(), vt, 0, true);
        self.push_frame(span);
        let handler_reach = match &handler {
            hir::Handler::Expr(h) => {
                if let Some(op) = self.lower_expr(*h) {
                    self.emit(StmtKind::Assign {
                        place: Place::local(dest),
                        value: Rvalue::Use(op),
                    });
                    true
                } else {
                    false
                }
            }
            hir::Handler::Block(b) => self.lower_block(*b),
        };
        if handler_reach && self.reachable {
            self.finish_frame();
            self.goto(join);
        } else {
            self.frames.pop();
        }
        // some: the payload.
        self.cur = some_block;
        self.reachable = true;
        self.note_variant(vplace.clone(), vt, 1, true);
        self.push_frame(span);
        let payload = vplace.clone().payload(1);
        let op = self.read_payload(payload, ty, vt);
        let op = self.snapshot(op, ty, span);
        self.emit(StmtKind::Assign {
            place: Place::local(dest),
            value: Rvalue::Use(op),
        });
        if self.reachable {
            self.finish_frame();
            self.goto(join);
        } else {
            self.frames.pop();
        }
        self.cur = join;
        self.reachable = true;
        Some(Operand::Place(Place::local(dest), self.consume_use(ty)))
    }

    fn switch_result(
        &mut self,
        place: Place,
        enum_ty: TypeId,
        tag0: i128,
        block0: Block,
        tag1: i128,
        block1: Block,
    ) {
        let tag_ty = self.tag_ty(enum_ty);
        let span = self.stmt_span;
        let value = self.materialize(Rvalue::Discriminant(place), tag_ty, span);
        self.term(Terminator::Switch {
            value,
            cases: vec![(tag0, block0), (tag1, block1)],
            default: block1,
        });
        self.kill();
    }

    fn error_of_result_type(&self, ty: TypeId) -> TypeId {
        match self.kind(ty) {
            TyKind::Result(_, e) => e,
            _ => ty,
        }
    }

    fn lower_try(&mut self, e: ExprId, operand: ExprId) -> Option<Operand> {
        let ty = self.ty(e);
        let ot = self.ty(operand);
        let span = self.span(e);
        let op = self.lower_expr(operand)?;
        let temp = self.place_of_operand(op, ot, self.span(operand));
        let dest = self.new_temp(ty, span);
        let ok_block = self.new_block();
        let err_block = self.new_block();
        let enum_ty = ot;
        let tag_ty = self.tag_ty(enum_ty);
        let value = self.materialize(Rvalue::Discriminant(temp.clone()), tag_ty, span);
        self.term(Terminator::Switch {
            value,
            cases: vec![(0, ok_block), (1, err_block)],
            default: err_block,
        });
        self.kill();
        // ok: the payload is the value, in its own storage region.
        self.cur = ok_block;
        self.reachable = true;
        self.note_variant(temp.clone(), enum_ty, 0, true);
        self.push_frame(span);
        let payload = temp.clone().payload(0);
        let read = self.read_payload(payload, ty, enum_ty);
        let read = self.snapshot(read, ty, span);
        self.emit(StmtKind::Assign {
            place: Place::local(dest),
            value: Rvalue::Use(read),
        });
        let ok_end = self.new_block();
        if self.reachable {
            self.finish_frame();
            self.goto(ok_end);
        } else {
            self.frames.pop();
        }
        // err: convert and leave the function.
        self.cur = err_block;
        self.reachable = true;
        self.note_variant(temp.clone(), enum_ty, 1, true);
        self.push_frame(span);
        let conversion = match self.res(e) {
            ExprRes::Try(t) => t.conversion.clone(),
            _ => TryConversion::Identity,
        };
        let err_ty = self.error_of_result_type(ot);
        let epay = temp.clone().payload(1);
        let eread = self.read_payload(epay, err_ty, enum_ty);
        let eread = self.snapshot(eread, err_ty, span);
        let error_value = match conversion {
            TryConversion::Identity => eread,
            TryConversion::FromVariant(v) => {
                let target = v.enum_ty;
                self.materialize(
                    Rvalue::Aggregate {
                        kind: AggregateKind::Variant {
                            index: v.index,
                            enum_ty: v.enum_ty,
                        },
                        ty: target,
                        ty_name: self.ty_name(target),
                        fields: vec![eread],
                    },
                    target,
                    span,
                )
            }
        };
        self.exit(
            0,
            ExitKind::Return {
                value: Some(error_value),
                is_error: true,
            },
        );
        self.frames.pop();
        self.cur = ok_end;
        self.reachable = true;
        Some(Operand::Place(Place::local(dest), self.consume_use(ty)))
    }
}

// ------------------------------------------------------------------ patterns

impl<'a, 'b> Lowerer<'a, 'b> {
    /// Emit the tests of a refutable pattern; control falls through on success and
    /// jumps to `fail` otherwise.
    fn test_pattern(&mut self, pat: PatId, place: Place, fail: Block) {
        let info = match self.body.pat(pat) {
            Some(i) => i.clone(),
            None => return,
        };
        let place = Self::apply_derefs(place, info.derefs);
        match &self.pkg.pat(pat).kind {
            PatKind::Wild | PatKind::Bind { .. } => {}
            PatKind::Literal(e) => {
                let ty = self.ty(*e);
                let lit = match self.lower_expr(*e) {
                    Some(op) => op,
                    None => return,
                };
                let val = self.read_place(place, ty);
                let val = self.snapshot(val, ty, self.span(*e));
                let cond = self.materialize(
                    Rvalue::Binary {
                        op: BinOp::Eq,
                        lhs: val,
                        rhs: lit,
                    },
                    self.bool_ty(),
                    self.span(*e),
                );
                self.test_cond(cond, fail);
            }
            PatKind::Range { lo, hi, inclusive } => {
                let ty = info.ty;
                let lo_val = match &self.pkg.pat(*lo).kind {
                    PatKind::Literal(e) => match self.lower_expr(*e) {
                        Some(op) => op,
                        None => return,
                    },
                    _ => {
                        self.fail("a range bound that is not a literal");
                        return;
                    }
                };
                let hi_val = match &self.pkg.pat(*hi).kind {
                    PatKind::Literal(e) => match self.lower_expr(*e) {
                        Some(op) => op,
                        None => return,
                    },
                    _ => {
                        self.fail("a range bound that is not a literal");
                        return;
                    }
                };
                let val = self.read_place(place.clone(), ty);
                let val = self.snapshot(val, ty, self.stmt_span);
                let ge = self.materialize(
                    Rvalue::Binary {
                        op: BinOp::Ge,
                        lhs: val.clone(),
                        rhs: lo_val,
                    },
                    self.bool_ty(),
                    self.stmt_span,
                );
                self.test_cond(ge, fail);
                let op = if *inclusive { BinOp::Le } else { BinOp::Lt };
                let le = self.materialize(
                    Rvalue::Binary {
                        op,
                        lhs: val,
                        rhs: hi_val,
                    },
                    self.bool_ty(),
                    self.stmt_span,
                );
                self.test_cond(le, fail);
            }
            PatKind::Variant { name, payload, .. } => {
                let res = match &info.res {
                    ingot_sema::typed::PatRes::Variant(v) => v.clone(),
                    _ => {
                        self.fail("a variant pattern with no variant information");
                        return;
                    }
                };
                let tag = self.variant_tag(&res);
                let tag_ty = self.tag_ty(res.enum_ty);
                let disc =
                    self.materialize(Rvalue::Discriminant(place.clone()), tag_ty, self.stmt_span);
                let cond = self.materialize(
                    Rvalue::Binary {
                        op: BinOp::Eq,
                        lhs: disc,
                        rhs: Operand::Const(Const::Int {
                            ty: tag_ty,
                            value: tag,
                        }),
                    },
                    self.bool_ty(),
                    self.stmt_span,
                );
                self.test_variant(place.clone(), res.index as u32, name.clone(), cond, fail);
                if let Some(payload_pat) = payload {
                    let payload_place = place.payload(res.index);
                    self.test_pattern(*payload_pat, payload_place, fail);
                }
            }
            PatKind::Tuple(elems) => {
                for (i, sub) in elems.iter().enumerate() {
                    self.test_pattern(*sub, place.clone().tuple_field(i), fail);
                }
            }
            PatKind::Struct { fields, .. } => {
                let res = match &info.res {
                    ingot_sema::typed::PatRes::Struct { res, .. } => res.clone(),
                    _ => {
                        self.fail("a struct pattern with no struct information");
                        return;
                    }
                };
                for (k, f) in fields.iter().enumerate() {
                    let index = res.order.get(k).copied().unwrap_or(k);
                    self.test_pattern(
                        f.pattern,
                        place.clone().field(index, Some(f.name.clone())),
                        fail,
                    );
                }
            }
            PatKind::Poison => {
                self.fail("a poison pattern");
            }
        }
    }

    fn test_cond(&mut self, cond: Operand, fail: Block) {
        // The tests of one alternative share a storage region. A failure at this test
        // ends only the temporaries created up to here, then goes to `fail`.
        let temps: Vec<Local> = self
            .frames
            .last()
            .map(|f| f.temps.clone())
            .unwrap_or_default();
        let ok = self.new_block();
        let bad = self.new_block();
        self.term(Terminator::Branch {
            cond,
            then_block: ok,
            else_block: bad,
        });
        self.kill();
        self.cur = bad;
        self.reachable = true;
        self.end_temp_list(&temps);
        self.goto(fail);
        self.cur = ok;
        self.reachable = true;
    }

    /// A tag test. The block the test chose starts with `place is name`; the block for
    /// a failed test starts with `place is not name` (pipeline.md §5).
    fn test_variant(
        &mut self,
        place: Place,
        variant: u32,
        name: String,
        cond: Operand,
        fail: Block,
    ) {
        let temps: Vec<Local> = self
            .frames
            .last()
            .map(|f| f.temps.clone())
            .unwrap_or_default();
        let ok = self.new_block();
        let bad = self.new_block();
        self.term(Terminator::Branch {
            cond,
            then_block: ok,
            else_block: bad,
        });
        self.kill();
        self.cur = bad;
        self.reachable = true;
        self.emit(StmtKind::Variant {
            place: place.clone(),
            variant,
            name: name.clone(),
            holds: false,
        });
        self.end_temp_list(&temps);
        self.goto(fail);
        self.cur = ok;
        self.reachable = true;
        self.emit(StmtKind::Variant {
            place,
            variant,
            name,
            holds: true,
        });
    }

    /// `place is <variant>` (or `is not`, when `holds` is false) at the current point.
    fn note_variant(&mut self, place: Place, ty: TypeId, index: u32, holds: bool) {
        let name = self.variant_label(ty, index);
        self.emit(StmtKind::Variant {
            place,
            variant: index,
            name,
            holds,
        });
    }

    fn variant_label(&self, ty: TypeId, index: u32) -> String {
        match self.kind(ty) {
            TyKind::Optional(_) => {
                if index == 0 {
                    "none".to_string()
                } else {
                    "some".to_string()
                }
            }
            TyKind::Result(_, _) => {
                if index == 0 {
                    "ok".to_string()
                } else {
                    "err".to_string()
                }
            }
            TyKind::Adt(d, _) => self
                .cx
                .decls
                .enum_sig(d)
                .and_then(|s| s.variants.get(index as usize))
                .map(|v| v.name.clone())
                .unwrap_or_else(|| format!("v{index}")),
            _ => format!("v{index}"),
        }
    }

    /// `place` is the payload of a by-value `T ! E` (no dereference on the way).
    fn by_value_result_payload(&mut self, place: &Place) -> bool {
        if self.guard_reads {
            return false;
        }
        let Some((Projection::Payload { .. }, parent)) = place.proj.split_last() else {
            return false;
        };
        if place
            .proj
            .iter()
            .any(|p| matches!(p, Projection::Deref | Projection::DerefRaw))
        {
            return false;
        }
        let parent = Place {
            base: place.base.clone(),
            proj: parent.to_vec(),
        };
        self.by_value_result(&parent)
    }

    /// `place` is a by-value `T ! E`.
    fn by_value_result(&mut self, place: &Place) -> bool {
        if self.guard_reads {
            return false;
        }
        if place
            .proj
            .iter()
            .any(|p| matches!(p, Projection::Deref | Projection::DerefRaw))
        {
            return false;
        }
        matches!(
            self.cfg_place_ty(place).map(|t| self.kind(t)),
            Some(TyKind::Result(_, _))
        )
    }

    fn cfg_place_ty(&mut self, place: &Place) -> Option<TypeId> {
        let PlaceBase::Local(l) = place.base else {
            return None;
        };
        let mut ty = self.cfg.locals.get(l as usize)?.ty;
        for proj in &place.proj {
            ty = self.proj_ty(ty, proj)?;
        }
        Some(ty)
    }

    fn proj_ty(&mut self, ty: TypeId, proj: &Projection) -> Option<TypeId> {
        match proj {
            Projection::Field { index, .. } => {
                let TyKind::Adt(def, args) = self.kind(ty) else {
                    return None;
                };
                self.field_type(def, &args, *index)
            }
            Projection::TupleField(i) => match self.kind(ty) {
                TyKind::Tuple(es) => es.get(*i).copied(),
                _ => None,
            },
            Projection::Index(_) => match self.kind(ty) {
                TyKind::Array(e, _) | TyKind::Slice(_, e) => Some(e),
                _ => None,
            },
            Projection::Deref => match self.kind(ty) {
                TyKind::Ref(_, e) | TyKind::Slice(_, e) => Some(e),
                _ => None,
            },
            Projection::DerefRaw => match self.kind(ty) {
                TyKind::RawPtr(_, e) => Some(e),
                _ => None,
            },
            Projection::Payload { variant } => match self.kind(ty) {
                TyKind::Optional(inner) => (*variant == 1).then_some(inner),
                TyKind::Result(ok, err) => match variant {
                    0 => Some(ok),
                    1 => Some(err),
                    _ => None,
                },
                TyKind::Adt(def, args) => {
                    let payload = self
                        .cx
                        .decls
                        .enum_sig(def)?
                        .variants
                        .get(*variant)?
                        .payload?;
                    let subst = self.cx.decls.subst_for(def, &args);
                    Some(self.cx.decls.types.subst(payload, &subst))
                }
                _ => None,
            },
        }
    }

    /// Bind the names of a pattern. The calls are already tested (or the pattern is
    /// irrefutable).
    fn bind_pattern(&mut self, pat: PatId, place: Place) {
        let info = match self.body.pat(pat) {
            Some(i) => i.clone(),
            None => return,
        };
        let place = Self::apply_derefs(place, info.derefs);
        // A by-value tag test of `T ! E` moves the chosen payload, even when it is
        // `Copy` or `_` (pipeline.md §5). `match &r` looks through a dereference, so
        // it stays a copy.
        let force = self.by_value_result_payload(&place);
        match &self.pkg.pat(pat).kind {
            PatKind::Wild => {
                if force {
                    self.emit(StmtKind::Use(Operand::Place(place, PlaceUse::Move)));
                }
            }
            PatKind::Bind { local } => {
                let l = self.declare_binding(*local);
                let mode = self.local_mode(*local);
                let value = match mode {
                    BindMode::Copy => {
                        let use_ = if force {
                            PlaceUse::Move
                        } else {
                            PlaceUse::Copy
                        };
                        Rvalue::Use(Operand::Place(place, use_))
                    }
                    BindMode::Move => Rvalue::Use(Operand::Place(place, PlaceUse::Move)),
                    BindMode::Ref => Rvalue::Borrow {
                        place,
                        mutability: Mutability::Shared,
                        two_phase: false,
                    },
                    BindMode::RefMut => Rvalue::Borrow {
                        place,
                        mutability: Mutability::Mut,
                        two_phase: false,
                    },
                };
                self.emit(StmtKind::Assign {
                    place: Place::local(l),
                    value,
                });
            }
            PatKind::Literal(_) | PatKind::Range { .. } => {}
            PatKind::Variant { payload, .. } => {
                let index = match &info.res {
                    ingot_sema::typed::PatRes::Variant(v) => v.index,
                    _ => 0,
                };
                if let Some(sub) = payload {
                    self.bind_pattern(*sub, place.payload(index));
                } else if self.by_value_result(&place) {
                    self.emit(StmtKind::Use(Operand::Place(
                        place.payload(index),
                        PlaceUse::Move,
                    )));
                }
            }
            PatKind::Tuple(elems) => {
                for (i, sub) in elems.iter().enumerate() {
                    self.bind_pattern(*sub, place.clone().tuple_field(i));
                }
            }
            PatKind::Struct { fields, .. } => {
                let (def, args, order, omitted) = match &info.res {
                    ingot_sema::typed::PatRes::Struct { res, omitted, .. } => (
                        res.def,
                        res.args.clone(),
                        res.order.clone(),
                        omitted.clone(),
                    ),
                    _ => {
                        for (k, f) in fields.iter().enumerate() {
                            self.bind_pattern(
                                f.pattern,
                                place.clone().field(k, Some(f.name.clone())),
                            );
                        }
                        return;
                    }
                };
                let n = self
                    .cx
                    .decls
                    .struct_sig(def)
                    .map(|s| s.fields.len())
                    .unwrap_or(0);
                let names: Vec<String> = self
                    .cx
                    .decls
                    .struct_sig(def)
                    .map(|s| s.fields.iter().map(|f| f.name.clone()).collect())
                    .unwrap_or_default();
                let mut moved = vec![false; n];
                for (k, f) in fields.iter().enumerate() {
                    let index = order.get(k).copied().unwrap_or(k);
                    self.bind_pattern(f.pattern, place.clone().field(index, Some(f.name.clone())));
                    if index < n && self.pattern_moves(f.pattern) {
                        moved[index] = true;
                    }
                }
                self.finish_struct_pattern(
                    place,
                    info.ty,
                    info.derefs,
                    def,
                    &args,
                    &moved,
                    &omitted,
                    &names,
                );
            }
            PatKind::Poison => {}
        }
    }

    /// A binding moves its place when the binding mode is a move. A wildcard, a
    /// literal and a range do not. Nested patterns move when any part of them does.
    fn pattern_moves(&mut self, pat: PatId) -> bool {
        match &self.pkg.pat(pat).kind {
            PatKind::Wild | PatKind::Literal(_) | PatKind::Range { .. } | PatKind::Poison => false,
            PatKind::Bind { local } => self.local_mode(*local) == BindMode::Move,
            PatKind::Variant {
                payload: Some(sub), ..
            } => self.pattern_moves(*sub),
            PatKind::Variant { payload: None, .. } => false,
            PatKind::Tuple(elems) => elems.iter().any(|p| self.pattern_moves(*p)),
            PatKind::Struct { fields, .. } => fields.iter().any(|f| self.pattern_moves(f.pattern)),
        }
    }

    /// Consume a linear struct that a pattern took apart (memory.md §10, checker.md §3).
    ///
    /// Inside the struct's module, a pattern that moves no non-`Copy` field and drops
    /// no linear field becomes one move of the whole place (`Token.{ id: _ }`). A
    /// linear field the pattern does not move stays, so storage end reports E1003
    /// once. Outside the module, each field the pattern did not already move and that
    /// is not itself a dropped linear field is moved on its own, which Analysis A
    /// reports as E1006. Patterns that looked through a borrow are left alone: a move
    /// there would be out of a dereference.
    #[allow(clippy::too_many_arguments)]
    fn finish_struct_pattern(
        &mut self,
        place: Place,
        ty: TypeId,
        derefs: u32,
        def: DefId,
        args: &[ingot_sema::ty::GenericArg],
        moved: &[bool],
        omitted: &[usize],
        names: &[String],
    ) {
        if derefs != 0 || !self.counts_linear(ty) {
            return;
        }
        if moved.is_empty() {
            return;
        }
        let mut dropped_linear = false;
        for (i, was_moved) in moved.iter().enumerate() {
            if *was_moved {
                continue;
            }
            let Some(fty) = self.field_type(def, args, i) else {
                continue;
            };
            if self.counts_linear(fty) {
                dropped_linear = true;
            }
        }
        let _ = omitted;
        let any_moved = moved.iter().any(|m| *m);
        if self.same_module(def) {
            if !dropped_linear && !any_moved {
                self.emit(StmtKind::Use(Operand::Place(place, PlaceUse::Move)));
            }
            return;
        }
        for (i, was_moved) in moved.iter().enumerate() {
            if *was_moved {
                continue;
            }
            let Some(fty) = self.field_type(def, args, i) else {
                continue;
            };
            if self.counts_linear(fty) {
                continue;
            }
            let name = names.get(i).cloned();
            let field = place.clone().field(i, name);
            self.emit(StmtKind::Use(Operand::Place(field, PlaceUse::Move)));
        }
    }

    fn counts_linear(&mut self, ty: TypeId) -> bool {
        self.cx.decls.props(ty).maybe_linear == Tri::Yes
    }

    fn same_module(&self, def: DefId) -> bool {
        self.cx.sema.def(def).module == self.cx.sema.def(self.cfg.key.owner).module
    }

    fn field_type(
        &mut self,
        def: DefId,
        args: &[ingot_sema::ty::GenericArg],
        index: usize,
    ) -> Option<TypeId> {
        let ty = self.cx.decls.struct_sig(def)?.fields.get(index)?.ty;
        let subst = self.cx.decls.subst_for(def, args);
        Some(self.cx.decls.types.subst(ty, &subst))
    }

    /// The binding places of a pattern, without binding them.
    fn collect_bindings(&mut self, pat: PatId, place: Place) -> Vec<(LocalId, Place)> {
        let mut out = Vec::new();
        self.collect_bindings_into(pat, place, &mut out);
        out
    }

    fn collect_bindings_into(&self, pat: PatId, place: Place, out: &mut Vec<(LocalId, Place)>) {
        let info = match self.body.pat(pat) {
            Some(i) => i.clone(),
            None => return,
        };
        let place = Self::apply_derefs(place, info.derefs);
        match &self.pkg.pat(pat).kind {
            PatKind::Bind { local } => out.push((*local, place)),
            PatKind::Variant {
                payload: Some(sub), ..
            } => {
                let index = match &info.res {
                    ingot_sema::typed::PatRes::Variant(v) => v.index,
                    _ => 0,
                };
                self.collect_bindings_into(*sub, place.payload(index), out);
            }
            PatKind::Tuple(elems) => {
                for (i, sub) in elems.iter().enumerate() {
                    self.collect_bindings_into(*sub, place.clone().tuple_field(i), out);
                }
            }
            PatKind::Struct { fields, .. } => {
                let order = match &info.res {
                    ingot_sema::typed::PatRes::Struct { res, .. } => res.order.clone(),
                    _ => Vec::new(),
                };
                for (k, f) in fields.iter().enumerate() {
                    let index = order.get(k).copied().unwrap_or(k);
                    self.collect_bindings_into(
                        f.pattern,
                        place.clone().field(index, Some(f.name.clone())),
                        out,
                    );
                }
            }
            _ => {}
        }
    }
}

// ------------------------------------------------------------------ loops

impl<'a, 'b> Lowerer<'a, 'b> {
    #[allow(clippy::too_many_arguments)]
    fn lower_for(
        &mut self,
        stmt: StmtId,
        kind: &ForKind,
        vars: &[hir::ForVar],
        index: Option<&hir::ForVar>,
        iter: ExprId,
        range_end: Option<(bool, ExprId)>,
        step: Option<ExprId>,
        body: BlockId,
        span: Span,
        mark: usize,
    ) {
        match kind {
            ForKind::Range { ty, inclusive } => self.lower_for_range(
                stmt,
                *ty,
                *inclusive,
                vars.first().and_then(|v| v.local),
                index.and_then(|v| v.local),
                iter,
                range_end,
                step,
                body,
                span,
                mark,
            ),
            ForKind::Copying => self.lower_for_slice(
                stmt,
                vars.first().and_then(|v| v.local),
                index.and_then(|v| v.local),
                iter,
                body,
                span,
                mark,
                false,
                false,
            ),
            ForKind::ExclusiveSlice => self.lower_for_slice(
                stmt,
                vars.first().and_then(|v| v.local),
                index.and_then(|v| v.local),
                iter,
                body,
                span,
                mark,
                false,
                true,
            ),
            ForKind::Consuming { .. } => self.lower_for_slice(
                stmt,
                vars.first().and_then(|v| v.local),
                index.and_then(|v| v.local),
                iter,
                body,
                span,
                mark,
                true,
                false,
            ),
            ForKind::Iter { next, .. } => self.lower_for_iter(
                stmt,
                vars.first().and_then(|v| v.local),
                iter,
                next.clone(),
                body,
                span,
                mark,
            ),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn lower_for_range(
        &mut self,
        stmt: StmtId,
        ty: TypeId,
        inclusive: bool,
        var: Option<LocalId>,
        _index_var: Option<LocalId>,
        iter: ExprId,
        range_end: Option<(bool, ExprId)>,
        step: Option<ExprId>,
        body: BlockId,
        span: Span,
        mark: usize,
    ) {
        // The bounds are evaluated once, before the loop.
        let lo_ty = self.ty(iter);
        let Some(lo) = self.lower_expr(iter) else {
            return;
        };
        let lo = self.snapshot(lo, lo_ty, self.span(iter));
        let (hi, hi_inclusive) = match range_end {
            Some((inc, hi)) => {
                let ht = self.ty(hi);
                let Some(h) = self.lower_expr(hi) else {
                    return;
                };
                (self.snapshot(h, ht, self.span(hi)), inc)
            }
            None => (Operand::Const(Const::Int { ty, value: 0 }), inclusive),
        };
        let step_op = match step {
            Some(s) => {
                let st = self.ty(s);
                let Some(op) = self.lower_expr(s) else {
                    return;
                };
                Some(self.snapshot(op, st, self.span(s)))
            }
            None => None,
        };
        let inclusive = hi_inclusive;
        // The counter lives for the whole loop.
        let counter = self.new_temp(ty, span);
        self.emit(StmtKind::Assign {
            place: Place::local(counter),
            value: Rvalue::Use(lo),
        });
        let loop_frame = self.frames.len();
        self.push_frame(self.pkg.block(body).span);
        let head = self.new_block();
        self.goto(head);
        self.cur = head;
        self.reachable = true;
        let counter_place = Place::local(counter);
        let counter_read = self.read_place(counter_place, ty);
        let counter_read = self.snapshot(counter_read, ty, span);
        let cmp = self.materialize(
            Rvalue::Binary {
                op: if inclusive { BinOp::Le } else { BinOp::Lt },
                lhs: counter_read,
                rhs: hi.clone(),
            },
            self.bool_ty(),
            span,
        );
        let body_block = self.new_block();
        let exit_head = self.new_block();
        let exit = self.new_block();
        let increment = self.new_block();
        self.term(Terminator::Branch {
            cond: cmp,
            then_block: body_block,
            else_block: exit_head,
        });
        self.kill();
        // Condition false: leave, ending nothing per iteration.
        self.cur = exit_head;
        self.reachable = true;
        self.end_iteration(loop_frame);
        self.goto(exit);
        // The body: bind the loop variable and the index.
        self.cur = body_block;
        self.reachable = true;
        if let Some(var) = var {
            let ty = self.local_ty(var);
            let l = self.declare_binding(var);
            let read = self.read_place(Place::local(counter), ty);
            self.emit(StmtKind::Assign {
                place: Place::local(l),
                value: Rvalue::Use(read),
            });
        }
        self.loops.push(LoopEntry {
            stmt: Some(stmt),
            frame: loop_frame,
            break_block: exit,
            continue_block: increment,
        });
        let falls = self.lower_block(body);
        if falls && self.reachable {
            self.end_iteration(loop_frame);
            self.goto(increment);
        }
        self.loops.pop();
        // Increment the counter and go round. The counter never overflows: an
        // exclusive range adds with saturation and keeps `i < hi`; an inclusive one
        // leaves when the rest of the range is smaller than the step.
        self.cur = increment;
        self.reachable = true;
        self.stmt_span = span;
        let inc_mark = self.temp_mark();
        let cur = self.read_place(Place::local(counter), ty);
        let cur = self.snapshot(cur, ty, span);
        let step = step_op.unwrap_or(Operand::Const(Const::Int { ty, value: 1 }));
        if inclusive {
            let rest = self.materialize(
                Rvalue::Binary {
                    op: BinOp::SubSat,
                    lhs: hi,
                    rhs: cur.clone(),
                },
                ty,
                span,
            );
            let enough = self.materialize(
                Rvalue::Binary {
                    op: BinOp::Lt,
                    lhs: rest,
                    rhs: step.clone(),
                },
                self.bool_ty(),
                span,
            );
            let inc_temps: Vec<Local> = self
                .frames
                .last()
                .map(|f| f.temps[inc_mark..].to_vec())
                .unwrap_or_default();
            let do_inc = self.new_block();
            let leave = self.new_block();
            self.term(Terminator::Branch {
                cond: enough,
                then_block: leave,
                else_block: do_inc,
            });
            self.kill();
            // The last value has been seen: leave.
            self.cur = leave;
            self.reachable = true;
            self.stmt_span = span;
            self.end_temp_list(&inc_temps);
            if let Some(f) = self.frames.last_mut() {
                f.temps.truncate(inc_mark);
            }
            self.goto(exit);
            // Otherwise add the step; it cannot pass the range's maximum.
            self.cur = do_inc;
            self.reachable = true;
            self.stmt_span = span;
            let next = self.materialize(
                Rvalue::Binary {
                    op: BinOp::AddWrap,
                    lhs: cur,
                    rhs: step,
                },
                ty,
                span,
            );
            self.emit(StmtKind::Assign {
                place: Place::local(counter),
                value: Rvalue::Use(next),
            });
            self.end_temps(inc_mark);
            self.end_temp_list(&inc_temps);
            self.goto(head);
        } else {
            let next = self.materialize(
                Rvalue::Binary {
                    op: BinOp::AddSat,
                    lhs: cur,
                    rhs: step,
                },
                ty,
                span,
            );
            self.emit(StmtKind::Assign {
                place: Place::local(counter),
                value: Rvalue::Use(next),
            });
            self.end_temps(inc_mark);
            self.goto(head);
        }
        self.cur = exit;
        self.reachable = true;
        self.stmt_span = span;
        self.frames.pop();
        self.end_temps(mark);
    }

    #[allow(clippy::too_many_arguments)]
    fn lower_for_slice(
        &mut self,
        stmt: StmtId,
        var: Option<LocalId>,
        index_var: Option<LocalId>,
        iter: ExprId,
        body: BlockId,
        span: Span,
        mark: usize,
        consuming: bool,
        exclusive: bool,
    ) {
        let iter_ty = self.ty(iter);
        let Some(op) = self.lower_expr(iter) else {
            return;
        };
        let op = if exclusive {
            // A `[]mut T` iterand is reborrowed once for the whole loop.
            if let Some(place) = self.expr_place(iter) {
                self.materialize(
                    Rvalue::Reborrow {
                        place,
                        two_phase: false,
                    },
                    iter_ty,
                    span,
                )
            } else {
                self.snapshot(op, iter_ty, span)
            }
        } else {
            self.snapshot(op, iter_ty, span)
        };
        let base = self.place_of_operand(op, iter_ty, span);
        let i64_ = self.i64_ty();
        let len = self.materialize(Rvalue::SliceLen(base.clone()), i64_, span);
        let index = self.new_temp(i64_, span);
        self.emit(StmtKind::Assign {
            place: Place::local(index),
            value: Rvalue::Use(Operand::Const(Const::Int { ty: i64_, value: 0 })),
        });
        let loop_frame = self.frames.len();
        self.push_frame(self.pkg.block(body).span);
        let head = self.new_block();
        self.goto(head);
        self.cur = head;
        self.reachable = true;
        let read = self.read_place(Place::local(index), i64_);
        let read = self.snapshot(read, i64_, span);
        let cmp = self.materialize(
            Rvalue::Binary {
                op: BinOp::Lt,
                lhs: read,
                rhs: len,
            },
            self.bool_ty(),
            span,
        );
        let body_block = self.new_block();
        let exit_head = self.new_block();
        let exit = self.new_block();
        let increment = self.new_block();
        self.term(Terminator::Branch {
            cond: cmp,
            then_block: body_block,
            else_block: exit_head,
        });
        self.kill();
        self.cur = exit_head;
        self.reachable = true;
        self.stmt_span = span;
        // The loop finished: every element was moved. A `break` targets `exit` and
        // skips this, so the elements still in the array stay an obligation.
        if consuming && var.is_some() {
            self.emit(StmtKind::IterConsumed(base.clone()));
        }
        self.end_iteration(loop_frame);
        self.goto(exit);
        self.cur = body_block;
        self.reachable = true;
        let elem_place = base
            .clone()
            .index(Operand::Place(Place::local(index), PlaceUse::Copy));
        if let Some(var) = var {
            let l = self.declare_binding(var);
            let value = if exclusive {
                Rvalue::SliceElem {
                    base: base.clone(),
                    index: Operand::Place(Place::local(index), PlaceUse::Copy),
                }
            } else if consuming {
                Rvalue::Use(Operand::Place(elem_place, PlaceUse::ForElem))
            } else {
                Rvalue::Use(Operand::Place(elem_place, PlaceUse::Copy))
            };
            self.emit(StmtKind::Assign {
                place: Place::local(l),
                value,
            });
        }
        if let Some(var) = index_var {
            let ty = self.local_ty(var);
            let l = self.declare_binding(var);
            let read = self.read_place(Place::local(index), ty);
            self.emit(StmtKind::Assign {
                place: Place::local(l),
                value: Rvalue::Use(read),
            });
        }
        self.loops.push(LoopEntry {
            stmt: Some(stmt),
            frame: loop_frame,
            break_block: exit,
            continue_block: increment,
        });
        let falls = self.lower_block(body);
        if falls && self.reachable {
            self.stmt_span = span;
            self.end_iteration(loop_frame);
            self.goto(increment);
        }
        self.loops.pop();
        self.cur = increment;
        self.reachable = true;
        self.stmt_span = span;
        let inc_mark = self.temp_mark();
        let read = self.read_place(Place::local(index), i64_);
        let read = self.snapshot(read, i64_, span);
        let next = self.materialize(
            Rvalue::Binary {
                op: BinOp::AddWrap,
                lhs: read,
                rhs: Operand::Const(Const::Int { ty: i64_, value: 1 }),
            },
            i64_,
            span,
        );
        self.emit(StmtKind::Assign {
            place: Place::local(index),
            value: Rvalue::Use(next),
        });
        self.end_temps(inc_mark);
        self.goto(head);
        self.cur = exit;
        self.reachable = true;
        self.stmt_span = span;
        self.frames.pop();
        self.end_temps(mark);
    }

    #[allow(clippy::too_many_arguments)]
    fn lower_for_iter(
        &mut self,
        stmt: StmtId,
        var: Option<LocalId>,
        iter: ExprId,
        next: FnRef,
        body: BlockId,
        span: Span,
        mark: usize,
    ) {
        let iter_ty = self.ty(iter);
        let Some(op) = self.lower_expr(iter) else {
            return;
        };
        let op = self.snapshot(op, iter_ty, span);
        let iter_local = self.place_of_operand(op, iter_ty, span);
        let next_name = self.fn_name(&next);
        let item_ty = var
            .map(|v| self.local_ty(v))
            .unwrap_or(self.cx.decls.types.poison);
        let loop_frame = self.frames.len();
        self.push_frame(self.pkg.block(body).span);
        let head = self.new_block();
        self.goto(head);
        self.cur = head;
        self.reachable = true;
        // `next(&mut it)`
        let ret_ty = self
            .cx
            .decls
            .fn_sig(next.def)
            .map(|s| s.ret)
            .unwrap_or(item_ty);
        let recv_ty = self.ref_ty(Mutability::Mut, iter_ty);
        let recv = self.materialize(
            Rvalue::Borrow {
                place: iter_local.clone(),
                mutability: Mutability::Mut,
                two_phase: true,
            },
            recv_ty,
            span,
        );
        let dest = self.new_temp(ret_ty, span);
        let after = self.new_block();
        self.term(Terminator::Call {
            target: CallTarget::Fn {
                fn_ref: Box::new(next),
                name: next_name,
            },
            args: vec![recv],
            dest: Some(Place::local(dest)),
            next: after,
        });
        self.kill();
        self.cur = after;
        self.reachable = true;
        let some_block = self.new_block();
        let exit = self.new_block();
        let exit_head = self.new_block();
        let tag_ty = self.tag_ty(ret_ty);
        let disc = self.materialize(Rvalue::Discriminant(Place::local(dest)), tag_ty, span);
        self.term(Terminator::Switch {
            value: disc,
            cases: vec![(0, exit_head), (1, some_block)],
            default: exit_head,
        });
        self.kill();
        self.cur = exit_head;
        self.reachable = true;
        self.stmt_span = span;
        self.note_variant(Place::local(dest), ret_ty, 0, true);
        self.end_iteration(loop_frame);
        self.goto(exit);
        // The item is the payload of `.some`.
        self.cur = some_block;
        self.reachable = true;
        self.stmt_span = span;
        self.note_variant(Place::local(dest), ret_ty, 1, true);
        if let Some(var) = var {
            let ty = self.local_ty(var);
            let l = self.declare_binding(var);
            let payload = Place::local(dest).payload(1);
            let read = self.read_payload(payload, ty, ret_ty);
            self.emit(StmtKind::Assign {
                place: Place::local(l),
                value: Rvalue::Use(read),
            });
        }
        let next_iter = self.new_block();
        self.loops.push(LoopEntry {
            stmt: Some(stmt),
            frame: loop_frame,
            break_block: exit,
            continue_block: next_iter,
        });
        let falls = self.lower_block(body);
        if falls && self.reachable {
            self.stmt_span = span;
            self.end_iteration(loop_frame);
            self.goto(next_iter);
        }
        self.loops.pop();
        self.cur = next_iter;
        self.reachable = true;
        self.stmt_span = span;
        self.goto(head);
        self.cur = exit;
        self.reachable = true;
        self.frames.pop();
        self.end_temps(mark);
    }

    fn expr_place(&mut self, e: ExprId) -> Option<Place> {
        if !self.is_place_expr(e) {
            return None;
        }
        let (p, checks) = self.lower_place(e)?;
        self.emit_checks(checks);
        Some(p)
    }

    /// The place a value expression refers to, or a temporary holding it.
    fn value_place(&mut self, e: ExprId) -> Option<Place> {
        if self.is_place_expr(e) {
            return self.expr_place(e);
        }
        let ty = self.ty(e);
        let op = self.lower_expr(e)?;
        Some(self.place_of_operand(op, ty, self.span(e)))
    }

    /// True when the expression is structurally a place. This does not evaluate
    /// anything, so a caller can decide before it commits to a side effect.
    fn is_place_expr(&self, e: ExprId) -> bool {
        let e = self.unparen(e);
        match &self.pkg.expr(e).kind {
            ExprKind::Path(_) => matches!(self.res(e), ExprRes::Local(_) | ExprRes::Static(_)),
            ExprKind::Field { .. } => {
                matches!(self.res(e), ExprRes::Field { .. })
            }
            ExprKind::TupleField { .. } => matches!(self.res(e), ExprRes::TupleField { .. }),
            ExprKind::Index { .. } => matches!(self.res(e), ExprRes::Index { .. }),
            ExprKind::Deref { .. } => true,
            ExprKind::Builtin { name, .. } => {
                name == "unchecked_index" && matches!(self.res(e), ExprRes::Index { .. })
            }
            ExprKind::Unsafe { body, .. } => self.is_place_expr(*body),
            _ => false,
        }
    }
}

// ------------------------------------------------------------------ closures, spawn, dispatch, select

impl<'a, 'b> Lowerer<'a, 'b> {
    fn lower_closure_expr(&mut self, e: ExprId, info: &ClosureInfo) -> Option<Operand> {
        let ty = self.ty(e);
        let span = self.span(e);
        let mut fields = Vec::new();
        for cap in &info.captures {
            let outer = match cap.outer {
                Some(l) => l,
                None => {
                    self.fail("a closure capture that is not a local");
                    return None;
                }
            };
            let place = self.local_place(outer);
            let outer_ty = self.local_ty(outer);
            let value = match cap.mode {
                CaptureMode::Shared => Rvalue::Borrow {
                    place,
                    mutability: Mutability::Shared,
                    two_phase: false,
                },
                CaptureMode::Exclusive => Rvalue::Borrow {
                    place,
                    mutability: Mutability::Mut,
                    two_phase: false,
                },
                CaptureMode::ByValue => {
                    let read = self.read_place(place, outer_ty);
                    Rvalue::Use(read)
                }
            };
            let cty = self.local_ty(cap.local);
            fields.push(self.materialize(value, cty, span));
        }
        let agg = self.materialize(
            Rvalue::Aggregate {
                kind: AggregateKind::Closure(e),
                ty,
                ty_name: self.ty_name(ty),
                fields,
            },
            ty,
            span,
        );
        let body = self.body;
        match self.cx.lower_closure(body, e, info) {
            Ok(_) => Some(agg),
            Err(what) => {
                self.fail(what);
                None
            }
        }
    }

    fn call_target(
        &mut self,
        info: &ingot_sema::typed::CallInfo,
        e: ExprId,
    ) -> Option<(CallTarget, Vec<Operand>)> {
        let mut args = Vec::new();
        // A method receiver is the first argument and is evaluated first.
        if let Some(recv) = &info.receiver {
            let base = self.receiver_base(e)?;
            // A by-value receiver need not be a place (`make().get()`): it is
            // evaluated into a temporary. Phase 7 requires a place for a borrow.
            let mut checks = Vec::new();
            let mut place = self.place_base(base, &mut checks)?;
            self.emit_checks(checks);
            place = Self::apply_derefs(place, recv.derefs);
            let base_ty = self.strip_derefs(self.ty(base), recv.derefs);
            let value = match recv.mode {
                ReceiverMode::Value => {
                    let read = self.read_place(place, base_ty);
                    self.snapshot(read, base_ty, self.span(base))
                }
                ReceiverMode::Shared => {
                    let rty = self.ref_ty(Mutability::Shared, base_ty);
                    self.materialize(
                        Rvalue::Borrow {
                            place,
                            mutability: Mutability::Shared,
                            two_phase: false,
                        },
                        rty,
                        self.span(base),
                    )
                }
                ReceiverMode::Exclusive => {
                    let rty = self.ref_ty(Mutability::Mut, base_ty);
                    self.materialize(
                        Rvalue::Borrow {
                            place,
                            mutability: Mutability::Mut,
                            two_phase: true,
                        },
                        rty,
                        self.span(base),
                    )
                }
            };
            args.push(value);
        }
        // A call of a value evaluates the callable before the arguments.
        let mut value_target = None;
        if let Callee::Value(vc) = &info.callee {
            let kind = match vc {
                ValueCall::FnPtr => ValueKind::FnPtr,
                ValueCall::Closure(x) => ValueKind::Closure(*x),
                ValueCall::Bound(_) => ValueKind::Bound,
                ValueCall::DynFn(_) => ValueKind::DynFn,
            };
            let callee_expr = match &self.pkg.expr(e).kind {
                ExprKind::Call { callee, .. } => *callee,
                _ => {
                    self.fail("a call of a value with no callee");
                    return None;
                }
            };
            let callee = self.lower_callee_value(callee_expr, info.mode)?;
            value_target = Some(CallTarget::Value {
                callee,
                kind,
                mode: info.mode,
            });
        }
        let arg_exprs = match &self.pkg.expr(e).kind {
            ExprKind::Call { args, .. } => args.clone(),
            _ => Vec::new(),
        };
        for a in arg_exprs {
            let op = self.lower_arg(a)?;
            args.push(op);
        }
        let target = match (&info.callee, value_target) {
            (Callee::Fn(f), _) => CallTarget::Fn {
                fn_ref: Box::new(f.clone()),
                name: self.fn_name(f),
            },
            (Callee::Value(_), Some(t)) => t,
            (Callee::Value(_), None) => {
                self.fail("a call of a value with no callable");
                return None;
            }
            (Callee::Variant(_), _) => {
                self.fail("a call of an enum variant through a terminator");
                return None;
            }
        };
        Some((target, args))
    }

    fn receiver_base(&self, e: ExprId) -> Option<ExprId> {
        match &self.pkg.expr(self.unparen(e)).kind {
            ExprKind::Call { callee, .. } => match &self.pkg.expr(self.unparen(*callee)).kind {
                ExprKind::Field { base, .. } => Some(*base),
                _ => None,
            },
            _ => None,
        }
    }

    fn lower_callee_value(&mut self, e: ExprId, mode: Option<CallMode>) -> Option<Operand> {
        let ty = self.ty(e);
        let span = self.span(e);
        let op = self.lower_expr(e)?;
        let place = self.place_of_operand(op, ty, span);
        match mode {
            Some(CallMode::Shared) => {
                let rty = self.ref_ty(Mutability::Shared, ty);
                Some(self.materialize(
                    Rvalue::Borrow {
                        place,
                        mutability: Mutability::Shared,
                        two_phase: false,
                    },
                    rty,
                    span,
                ))
            }
            Some(CallMode::Exclusive) => {
                let rty = self.ref_ty(Mutability::Mut, ty);
                Some(self.materialize(
                    Rvalue::Borrow {
                        place,
                        mutability: Mutability::Mut,
                        two_phase: true,
                    },
                    rty,
                    span,
                ))
            }
            Some(CallMode::Move) => {
                let read = Operand::Place(place, PlaceUse::Move);
                Some(self.snapshot(read, ty, span))
            }
            None => {
                let read = self.read_place(place, ty);
                Some(self.snapshot(read, ty, span))
            }
        }
    }

    fn lower_call(&mut self, e: ExprId) -> Option<Operand> {
        let info = match self.res(e) {
            ExprRes::Call(c) => c,
            _ => {
                self.fail("a call with no call information");
                return None;
            }
        };
        // A variant constructor is a value, not a terminator.
        if let Callee::Variant(v) = &info.callee {
            let args = match &self.pkg.expr(e).kind {
                ExprKind::Call { args, .. } => args.clone(),
                _ => Vec::new(),
            };
            let ty = self.ty(e);
            let span = self.span(e);
            let mut fields = Vec::new();
            for a in args {
                let at = self.ty(a);
                let op = self.lower_expr(a)?;
                fields.push(self.snapshot(op, at, self.span(a)));
            }
            return Some(self.materialize(
                Rvalue::Aggregate {
                    kind: AggregateKind::Variant {
                        index: v.index,
                        enum_ty: v.enum_ty,
                    },
                    ty,
                    ty_name: self.ty_name(ty),
                    fields,
                },
                ty,
                span,
            ));
        }
        let (target, args) = self.call_target(&info, e)?;
        let ty = self.ty(e);
        let span = self.span(e);
        let dest = if ty == self.cx.decls.types.void {
            None
        } else {
            let l = self.new_temp(ty, span);
            Some(Place::local(l))
        };
        let next = self.new_block();
        self.term(Terminator::Call {
            target,
            args,
            dest: dest.clone(),
            next,
        });
        self.kill();
        self.cur = next;
        self.reachable = true;
        match dest {
            Some(p) => Some(self.read_place(p, ty)),
            None => Some(Operand::Const(Const::Unit)),
        }
    }

    fn lower_arg(&mut self, e: ExprId) -> Option<Operand> {
        let ty = self.ty(e);
        let span = self.span(e);
        let inner = self.unparen(e);
        if let ExprKind::Unary {
            op: UnOp::Borrow | UnOp::BorrowMut,
            ..
        } = &self.pkg.expr(inner).kind
        {
            self.arg_depth += 1;
            let r = self.lower_expr(e);
            self.arg_depth -= 1;
            return r;
        }
        let reborrow = matches!(
            self.kind(ty),
            TyKind::Ref(Mutability::Mut, _) | TyKind::Slice(Mutability::Mut, _)
        );
        if reborrow && let Some(place) = self.expr_place(e) {
            return Some(self.materialize(
                Rvalue::Reborrow {
                    place,
                    two_phase: true,
                },
                ty,
                span,
            ));
        }
        let op = self.lower_expr(e)?;
        Some(self.snapshot(op, ty, span))
    }

    fn lower_spawn(
        &mut self,
        call: ExprId,
        scope: ExprId,
        result_ty: Option<TypeId>,
        span: Span,
    ) -> Option<Operand> {
        let info = match self.res(call) {
            ExprRes::Call(c) => c,
            _ => {
                self.fail("a spawn of something that is not a call");
                return None;
            }
        };
        let (target, args) = self.call_target(&info, call)?;
        let scope_ty = self.ty(scope);
        let sop = self.lower_expr(scope)?;
        let sop = self.snapshot(sop, scope_ty, self.span(scope));
        let dest = result_ty.map(|ty| {
            let l = self.new_temp(ty, span);
            Place::local(l)
        });
        let next = self.new_block();
        self.term(Terminator::Spawn {
            target,
            args,
            scope: sop,
            dest: dest.clone(),
            next,
        });
        self.kill();
        self.cur = next;
        self.reachable = true;
        match (result_ty, dest) {
            (Some(ty), Some(p)) => Some(self.read_place(p, ty)),
            _ => Some(Operand::Const(Const::Unit)),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn lower_dispatch(
        &mut self,
        call: ExprId,
        _domain: ExprId,
        executor: ExprId,
        info: &ingot_sema::typed::DispatchInfo,
        result_ty: Option<TypeId>,
        span: Span,
    ) -> Option<Operand> {
        let arg_exprs = match &self.pkg.expr(call).kind {
            ExprKind::Call { args, .. } => args.clone(),
            _ => Vec::new(),
        };
        let mut args = Vec::new();
        let mut places = Vec::new();
        for a in arg_exprs {
            let ty = self.ty(a);
            let borrow = match self.kind(ty) {
                TyKind::Slice(Mutability::Mut, _) => Some(Mutability::Mut),
                TyKind::Slice(Mutability::Shared, _) => Some(Mutability::Shared),
                _ => None,
            };
            if borrow.is_some() && self.is_place_expr(a) {
                // A slice argument is borrowed where it is written; keep that place.
                let place = self.expr_place(a)?;
                let value = match borrow {
                    Some(Mutability::Mut) => self.materialize(
                        Rvalue::Reborrow {
                            place: place.clone(),
                            two_phase: true,
                        },
                        ty,
                        span,
                    ),
                    _ => self.read_place(place.clone(), ty),
                };
                places.push(place.clone());
                args.push(DispatchArg {
                    value,
                    borrow,
                    place: Some(place),
                });
                continue;
            }
            let op = self.lower_arg(a)?;
            let place = if borrow.is_some() {
                let p = self.place_of_operand(op.clone(), ty, self.span(a));
                places.push(p.clone());
                Some(p)
            } else {
                None
            };
            args.push(DispatchArg {
                value: op,
                borrow,
                place,
            });
        }
        let lo_ty = self.ty(info.lo);
        let lo = self.lower_expr(info.lo)?;
        let lo = self.snapshot(lo, lo_ty, self.span(info.lo));
        let hi_ty = self.ty(info.hi);
        let hi = self.lower_expr(info.hi)?;
        let hi = self.snapshot(hi, hi_ty, self.span(info.hi));
        let ex_ty = self.ty(executor);
        let ex = self.lower_expr(executor)?;
        let ex = self.snapshot(ex, ex_ty, self.span(executor));
        self.emit_check(Check {
            kind: CheckKind::DispatchCover,
            place: None,
            lo: Some(lo.clone()),
            hi: Some(hi.clone()),
            operands: Vec::new(),
            places,
            ty: None,
            ty_name: None,
            op: None,
            span,
        });
        let kernel = info.kernel;
        let kernel_name = self.cx.sema.qualified_name(kernel);
        let mut arg_names = Vec::new();
        for arg in &info.kernel_args {
            let text = match arg {
                ingot_sema::ty::GenericArg::Ty(t) => self.ty_name(*t),
                ingot_sema::ty::GenericArg::Const(c) => match c {
                    ingot_sema::ty::ConstArg::Value(ingot_sema::ty::ConstValue::Int {
                        value,
                        ..
                    }) => value.to_string(),
                    ingot_sema::ty::ConstArg::Value(ingot_sema::ty::ConstValue::Bool(b)) => {
                        b.to_string()
                    }
                    ingot_sema::ty::ConstArg::Param(d) => self.cx.sema.def(*d).name.clone(),
                    ingot_sema::ty::ConstArg::Poison => "{error}".to_string(),
                },
            };
            arg_names.push(text);
        }
        let kernel_args_name = if arg_names.is_empty() {
            String::new()
        } else {
            format!("[{}]", arg_names.join(", "))
        };
        let dest = result_ty
            .filter(|t| *t != self.cx.decls.types.void)
            .map(|ty| Place::local(self.new_temp(ty, span)));
        let next = self.new_block();
        self.term(Terminator::Dispatch {
            kernel,
            kernel_name,
            kernel_args: info.kernel_args.clone(),
            kernel_args_name,
            executor: ex,
            lo,
            hi,
            args,
            dest: dest.clone(),
            next,
        });
        self.kill();
        self.cur = next;
        self.reachable = true;
        match (result_ty, dest) {
            (Some(ty), Some(p)) => Some(self.read_place(p, ty)),
            _ => Some(Operand::Const(Const::Unit)),
        }
    }

    fn lower_select(&mut self, arms: &[hir::SelectArm], span: Span, mark: usize) {
        let join = self.new_block();
        let mut ops = Vec::new();
        let mut dests: Vec<Option<Local>> = Vec::new();
        let mut successors: Vec<Block> = Vec::new();
        for arm in arms {
            let successor = self.new_block();
            successors.push(successor);
            if arm.is_else {
                ops.push(SelectOp {
                    call: None,
                    successor,
                    is_else: true,
                    span: arm.span,
                });
                dests.push(None);
                continue;
            }
            let call = match arm.call {
                Some(c) => c,
                None => {
                    self.fail("a `select` arm with no call");
                    dests.push(None);
                    continue;
                }
            };
            let info = match self.res(call) {
                ExprRes::Call(c) => c,
                _ => {
                    self.fail("a `select` arm that is not a call");
                    dests.push(None);
                    continue;
                }
            };
            let (target, args) = match self.call_target(&info, call) {
                Some(t) => t,
                None => {
                    dests.push(None);
                    continue;
                }
            };
            let ret = self.ty(call);
            let dest = self.new_temp(ret, span);
            ops.push(SelectOp {
                call: Some(SelectCall {
                    target,
                    args,
                    dest: Place::local(dest),
                }),
                successor,
                is_else: false,
                span: arm.span,
            });
            dests.push(Some(dest));
        }
        let all_dests: Vec<Local> = dests.iter().flatten().copied().collect();
        self.term(Terminator::Select { ops });
        self.kill();
        let mut all_reach = false;
        for (i, arm) in arms.iter().enumerate() {
            self.cur = successors[i];
            self.reachable = true;
            // Every operation but this one did not run: its destination is empty.
            for &d in &all_dests {
                if dests.get(i).copied().flatten() != Some(d) {
                    self.emit_at(StmtKind::Abandon(d), arm.span);
                }
            }
            self.push_frame(arm.span);
            if let Some(Some(d)) = dests.get(i).copied() {
                self.bind_pattern(arm.pattern, Place::local(d));
            }
            let falls = self.lower_block(arm.body);
            if falls && self.reachable {
                self.finish_frame();
                self.goto(join);
                all_reach = true;
            } else {
                self.frames.pop();
            }
        }
        self.cur = join;
        self.reachable = all_reach;
        if self.reachable {
            self.end_temps(mark);
        }
    }
}

// ------------------------------------------------------------------ builtins

impl<'a, 'b> Lowerer<'a, 'b> {
    fn lower_gather(
        &mut self,
        name: &str,
        args: &[ExprId],
        ty: TypeId,
        span: Span,
    ) -> Option<Operand> {
        let base_expr = args.first().copied()?;
        let Some(base) = self.value_place(base_expr) else {
            self.fail("a `@gather` without a slice");
            return None;
        };
        let idx = args.get(1).copied()?;
        let idx_ty = self.ty(idx);
        let idx_op = self.lower_expr(idx)?;
        let idx_op = self.snapshot(idx_op, idx_ty, self.span(idx));
        self.emit_check(Check {
            kind: CheckKind::Gather,
            place: Some(base.clone()),
            lo: None,
            hi: None,
            operands: vec![idx_op.clone()],
            places: Vec::new(),
            ty: None,
            ty_name: None,
            op: None,
            span,
        });
        Some(self.materialize(
            Rvalue::Builtin {
                name: name.to_string(),
                type_arg: Some(ty),
                type_arg_name: Some(self.ty_name(ty)),
                args: vec![BuiltinArg::Place(base), BuiltinArg::Value(idx_op)],
            },
            ty,
            span,
        ))
    }

    fn lower_scatter(&mut self, name: &str, args: &[ExprId], span: Span) -> Option<Operand> {
        let base_expr = args.first().copied()?;
        let Some(base) = self.value_place(base_expr) else {
            self.fail("a `@scatter` without a slice");
            return None;
        };
        let idx = args.get(1).copied()?;
        let idx_ty = self.ty(idx);
        let idx_op = self.lower_expr(idx)?;
        let idx_op = self.snapshot(idx_op, idx_ty, self.span(idx));
        let val = args.get(2).copied()?;
        let val_ty = self.ty(val);
        let val_op = self.lower_expr(val)?;
        let val_op = self.snapshot(val_op, val_ty, self.span(val));
        self.emit_check(Check {
            kind: CheckKind::Gather,
            place: Some(base.clone()),
            lo: None,
            hi: None,
            operands: vec![idx_op.clone()],
            places: Vec::new(),
            ty: None,
            ty_name: None,
            op: None,
            span,
        });
        self.emit_check(Check {
            kind: CheckKind::ScatterDistinct,
            place: Some(base.clone()),
            lo: None,
            hi: None,
            operands: vec![idx_op.clone()],
            places: Vec::new(),
            ty: None,
            ty_name: None,
            op: None,
            span,
        });
        Some(self.materialize(
            Rvalue::Builtin {
                name: name.to_string(),
                type_arg: None,
                type_arg_name: None,
                args: vec![
                    BuiltinArg::Place(base),
                    BuiltinArg::Value(idx_op),
                    BuiltinArg::Value(val_op),
                ],
            },
            self.cx.decls.types.void,
            span,
        ))
    }

    fn lower_idx_method(
        &mut self,
        e: ExprId,
        args: &[ExprId],
        m: IdxMethod,
        ty: TypeId,
        span: Span,
    ) -> Option<Operand> {
        let base = self.receiver_base(e)?;
        let bty = self.ty(base);
        let bop = self.lower_expr(base)?;
        let bop = self.snapshot(bop, bty, self.span(base));
        let mut operands = vec![BuiltinArg::Value(bop)];
        for a in args {
            let t = self.ty(*a);
            let op = self.lower_expr(*a)?;
            operands.push(BuiltinArg::Value(self.snapshot(op, t, self.span(*a))));
        }
        let name = match m {
            IdxMethod::Pos => "idx.pos",
            IdxMethod::Offset => "idx.offset",
        };
        Some(self.materialize(
            Rvalue::Builtin {
                name: name.to_string(),
                type_arg: None,
                type_arg_name: None,
                args: operands,
            },
            ty,
            span,
        ))
    }

    fn lower_slice_at(
        &mut self,
        e: ExprId,
        args: &[ExprId],
        clamped: bool,
        ty: TypeId,
        span: Span,
    ) -> Option<Operand> {
        let base = self.receiver_base(e)?;
        let bty = self.ty(base);
        let bop = self.lower_expr(base)?;
        let bop = self.snapshot(bop, bty, self.span(base));
        let mut operands = vec![BuiltinArg::Value(bop)];
        for a in args {
            let t = self.ty(*a);
            let op = self.lower_expr(*a)?;
            operands.push(BuiltinArg::Value(self.snapshot(op, t, self.span(*a))));
        }
        let name = if clamped { "at_clamped" } else { "at_or" };
        Some(self.materialize(
            Rvalue::Builtin {
                name: name.to_string(),
                type_arg: None,
                type_arg_name: None,
                args: operands,
            },
            ty,
            span,
        ))
    }

    fn lower_builtin(
        &mut self,
        e: ExprId,
        name: &str,
        args: &[ExprId],
        ty: TypeId,
        span: Span,
    ) -> Option<Operand> {
        let type_arg = match self.res(e) {
            ExprRes::Builtin { type_arg } => type_arg,
            _ => None,
        };
        match name {
            "assert" => {
                let cond = args.first().copied()?;
                let cty = self.ty(cond);
                let cop = self.lower_expr(cond)?;
                let cop = self.snapshot(cop, cty, self.span(cond));
                let ok = self.new_block();
                let bad = self.new_block();
                self.term(Terminator::Branch {
                    cond: cop,
                    then_block: ok,
                    else_block: bad,
                });
                self.kill();
                self.cur = bad;
                self.reachable = true;
                let mut operands = Vec::new();
                if let Some(msg) = args.get(1) {
                    let mt = self.ty(*msg);
                    let mop = self.lower_expr(*msg)?;
                    operands.push(self.snapshot(mop, mt, self.span(*msg)));
                }
                let cond_span = self.span(cond);
                let text = self.cx.sources.get(self.cfg.file).text_of(cond_span);
                self.term_at(
                    Terminator::Trap {
                        code: Trap::T08,
                        operands,
                        source: Some(text.to_string()),
                    },
                    span,
                );
                self.kill();
                self.cur = ok;
                self.reachable = true;
                Some(Operand::Const(Const::Unit))
            }
            "panic" => {
                let msg = args.first().copied()?;
                let mt = self.ty(msg);
                let mop = self.lower_expr(msg)?;
                let mop = self.snapshot(mop, mt, self.span(msg));
                self.term_at(
                    Terminator::Trap {
                        code: Trap::T09,
                        operands: vec![mop],
                        source: None,
                    },
                    span,
                );
                self.kill();
                None
            }
            "unreachable" => {
                self.term_at(
                    Terminator::Trap {
                        code: Trap::T10,
                        operands: Vec::new(),
                        source: None,
                    },
                    span,
                );
                self.kill();
                None
            }
            "forget" => {
                let x = args.first().copied()?;
                let xt = self.ty(x);
                let xop = self.lower_expr(x)?;
                let xop = self.snapshot(xop, xt, self.span(x));
                Some(self.materialize(
                    Rvalue::Builtin {
                        name: "forget".to_string(),
                        type_arg: Some(xt),
                        type_arg_name: Some(self.ty_name(xt)),
                        args: vec![BuiltinArg::Value(xop)],
                    },
                    ty,
                    span,
                ))
            }
            "static_assert" => Some(Operand::Const(Const::Unit)),
            "debug" => {
                // The format literal is checked at compile time and has no
                // `ExprInfo`; only the values are lowered. The literal is kept as a
                // string constant for code generation.
                let fmt = args.first().copied()?;
                let bytes = match &self.pkg.expr(fmt).kind {
                    ExprKind::Str(b) => b.clone(),
                    _ => Vec::new(),
                };
                let mut operands = vec![BuiltinArg::Value(Operand::Const(Const::Str(bytes)))];
                for a in &args[1..] {
                    let at = self.ty(*a);
                    let op = self.lower_expr(*a)?;
                    let op = if self.shared_read(*a) {
                        if self.is_copy(at) {
                            match op {
                                Operand::Place(p, _) => Operand::Place(p, PlaceUse::Copy),
                                other => other,
                            }
                        } else {
                            let place = self.place_of_operand(op, at, self.span(*a));
                            let rty = self.ref_ty(Mutability::Shared, at);
                            self.materialize(
                                Rvalue::Borrow {
                                    place,
                                    mutability: Mutability::Shared,
                                    two_phase: false,
                                },
                                rty,
                                self.span(*a),
                            )
                        }
                    } else {
                        self.snapshot(op, at, self.span(*a))
                    };
                    operands.push(BuiltinArg::Value(op));
                }
                Some(self.materialize(
                    Rvalue::Builtin {
                        name: "debug".to_string(),
                        type_arg: None,
                        type_arg_name: None,
                        args: operands,
                    },
                    ty,
                    span,
                ))
            }
            "zero" => {
                let name = self.ty_name(ty);
                Some(Operand::Const(Const::Zero { ty, name }))
            }
            "uninit" => {
                let name = self.ty_name(ty);
                Some(Operand::Const(Const::Uninit { ty, name }))
            }
            "load" | "load_tail" | "unchecked_load" => self.lower_load(e, name, args, ty, span),
            "store" | "store_tail" | "unchecked_store" => self.lower_store(name, args, ty, span),
            "unchecked_slice" => {
                let base_expr = args.first().copied()?;
                let Some(base) = self.value_place(base_expr) else {
                    self.fail("a `@unchecked_slice` without a slice");
                    return None;
                };
                let i64_ = self.i64_ty();
                let lo = self.lower_index_arg(args.get(1).copied(), i64_)?;
                let hi = self.lower_index_arg(args.get(2).copied(), i64_)?;
                Some(self.materialize(
                    Rvalue::Builtin {
                        name: "unchecked_slice".to_string(),
                        type_arg: Some(ty),
                        type_arg_name: Some(self.ty_name(ty)),
                        args: vec![
                            BuiltinArg::Place(base),
                            BuiltinArg::Value(lo),
                            BuiltinArg::Value(hi),
                        ],
                    },
                    ty,
                    span,
                ))
            }
            "replace" => {
                let target = args.first().copied()?;
                let place = self.place_of_borrow_arg(target)?;
                let v = args.get(1).copied()?;
                let vt = self.ty(v);
                let vop = self.lower_expr(v)?;
                let vop = self.snapshot(vop, vt, self.span(v));
                Some(self.materialize(
                    Rvalue::Builtin {
                        name: "replace".to_string(),
                        type_arg: Some(ty),
                        type_arg_name: Some(self.ty_name(ty)),
                        args: vec![BuiltinArg::Place(place), BuiltinArg::Value(vop)],
                    },
                    ty,
                    span,
                ))
            }
            "swap" => {
                let a = args.first().copied()?;
                let b = args.get(1).copied()?;
                let pa = self.place_of_borrow_arg(a)?;
                let pb = self.place_of_borrow_arg(b)?;
                Some(self.materialize(
                    Rvalue::Builtin {
                        name: "swap".to_string(),
                        type_arg: None,
                        type_arg_name: None,
                        args: vec![BuiltinArg::Place(pa), BuiltinArg::Place(pb)],
                    },
                    ty,
                    span,
                ))
            }
            "narrow" => {
                let x = args.first().copied()?;
                let xt = self.ty(x);
                let xop = self.lower_expr(x)?;
                let xop = self.snapshot(xop, xt, self.span(x));
                self.emit_check(Check {
                    kind: CheckKind::Narrow,
                    place: None,
                    lo: None,
                    hi: None,
                    operands: vec![xop.clone()],
                    places: Vec::new(),
                    ty: Some(ty),
                    ty_name: None,
                    op: None,
                    span,
                });
                Some(self.materialize(
                    Rvalue::Builtin {
                        name: "narrow".to_string(),
                        type_arg: Some(ty),
                        type_arg_name: Some(self.ty_name(ty)),
                        args: vec![BuiltinArg::Value(xop)],
                    },
                    ty,
                    span,
                ))
            }
            _ => {
                let mut operands = Vec::new();
                for a in args {
                    let at = self.ty(*a);
                    let op = self.lower_expr(*a)?;
                    let op = if self.shared_read(*a) {
                        if self.is_copy(at) {
                            match op {
                                Operand::Place(p, _) => Operand::Place(p, PlaceUse::Copy),
                                other => other,
                            }
                        } else {
                            // A read through a shared borrow, not a copy of a
                            // non-`Copy` value.
                            let place = self.place_of_operand(op, at, self.span(*a));
                            let rty = self.ref_ty(Mutability::Shared, at);
                            self.materialize(
                                Rvalue::Borrow {
                                    place,
                                    mutability: Mutability::Shared,
                                    two_phase: false,
                                },
                                rty,
                                self.span(*a),
                            )
                        }
                    } else {
                        self.snapshot(op, at, self.span(*a))
                    };
                    operands.push(BuiltinArg::Value(op));
                }
                Some(self.materialize(
                    Rvalue::Builtin {
                        name: name.to_string(),
                        type_arg,
                        type_arg_name: type_arg.map(|t| self.ty_name(t)),
                        args: operands,
                    },
                    ty,
                    span,
                ))
            }
        }
    }

    fn lower_index_arg(&mut self, e: Option<ExprId>, ty: TypeId) -> Option<Operand> {
        match e {
            Some(e) => {
                let t = self.ty(e);
                let op = self.lower_expr(e)?;
                Some(self.snapshot(op, t, self.span(e)))
            }
            None => Some(Operand::Const(Const::Int { ty, value: 0 })),
        }
    }

    fn lower_load(
        &mut self,
        _e: ExprId,
        name: &str,
        args: &[ExprId],
        ty: TypeId,
        span: Span,
    ) -> Option<Operand> {
        let base_expr = args.first().copied()?;
        let base = self.value_place(base_expr)?;
        let idx = args.get(1).copied()?;
        let idx_ty = self.ty(idx);
        let idx_op = self.lower_expr(idx)?;
        let idx_op = self.snapshot(idx_op, idx_ty, self.span(idx));
        let width = match self.kind(ty) {
            TyKind::Vector(_, n) => n,
            _ => 1,
        };
        let tail = name.ends_with("_tail");
        if !name.starts_with("unchecked") {
            let check = self.bounds_vector(base.clone(), idx_op.clone(), width, tail, span);
            self.emit_check(check);
        }
        Some(self.materialize(
            Rvalue::Builtin {
                name: name.to_string(),
                type_arg: Some(ty),
                type_arg_name: Some(self.ty_name(ty)),
                args: vec![BuiltinArg::Place(base), BuiltinArg::Value(idx_op)],
            },
            ty,
            span,
        ))
    }

    fn lower_store(
        &mut self,
        name: &str,
        args: &[ExprId],
        ty: TypeId,
        span: Span,
    ) -> Option<Operand> {
        let base_expr = args.first().copied()?;
        let base = self.value_place(base_expr)?;
        let idx = args.get(1).copied()?;
        let idx_ty = self.ty(idx);
        let idx_op = self.lower_expr(idx)?;
        let idx_op = self.snapshot(idx_op, idx_ty, self.span(idx));
        let val = args.get(2).copied()?;
        let val_ty = self.ty(val);
        let val_op = self.lower_expr(val)?;
        let val_op = self.snapshot(val_op, val_ty, self.span(val));
        let width = match self.kind(val_ty) {
            TyKind::Vector(_, n) => n,
            _ => 1,
        };
        let tail = name.ends_with("_tail");
        if !name.starts_with("unchecked") {
            let check = self.bounds_vector(base.clone(), idx_op.clone(), width, tail, span);
            self.emit_check(check);
        }
        Some(self.materialize(
            Rvalue::Builtin {
                name: name.to_string(),
                type_arg: None,
                type_arg_name: None,
                args: vec![
                    BuiltinArg::Place(base),
                    BuiltinArg::Value(idx_op),
                    BuiltinArg::Value(val_op),
                ],
            },
            ty,
            span,
        ))
    }

    /// The place behind `&mut p` or a `&mut` value used by `@replace`/`@swap`.
    fn place_of_borrow_arg(&mut self, e: ExprId) -> Option<Place> {
        let inner = self.unparen(e);
        if let ExprKind::Unary {
            op: UnOp::BorrowMut,
            expr,
            ..
        } = &self.pkg.expr(inner).kind
        {
            let (place, checks) = self.lower_place(*expr)?;
            self.emit_checks(checks);
            return Some(place);
        }
        // A `&mut T` value: the place it refers to is `p.*`.
        let ty = self.ty(e);
        let op = self.lower_expr(e)?;
        let span = self.span(e);
        let op = self.snapshot(op, ty, span);
        let place = self.place_of_operand(op, ty, span);
        Some(place.deref())
    }

    fn lower_struct(&mut self, e: ExprId) -> Option<Operand> {
        let ty = self.ty(e);
        let span = self.span(e);
        let res = match self.res(e) {
            ExprRes::Struct(s) => s,
            _ => {
                self.fail("a struct literal with no struct information");
                return None;
            }
        };
        let fields = match &self.pkg.expr(e).kind {
            ExprKind::StructLit { fields, .. } => fields.clone(),
            ExprKind::StructShorthand { fields, .. } => fields.clone(),
            _ => Vec::new(),
        };
        let count = self
            .cx
            .decls
            .struct_sig(res.def)
            .map(|s| s.fields.len())
            .unwrap_or(fields.len());
        let mut values: Vec<Option<Operand>> = vec![None; count];
        for (i, f) in fields.iter().enumerate() {
            let ft = self.ty(f.value);
            let op = self.lower_expr(f.value)?;
            let op = self.snapshot(op, ft, self.span(f.value));
            let index = res.order.get(i).copied().unwrap_or(i);
            if index < values.len() {
                values[index] = Some(op);
            }
        }
        let mut out = Vec::new();
        for v in values.into_iter() {
            match v {
                Some(v) => out.push(v),
                None => {
                    self.fail("a struct literal missing a field");
                    return None;
                }
            }
        }
        Some(self.materialize(
            Rvalue::Aggregate {
                kind: AggregateKind::Struct(res.def),
                ty,
                ty_name: self.ty_name(ty),
                fields: out,
            },
            ty,
            span,
        ))
    }

    fn lower_variant(&mut self, e: ExprId) -> Option<Operand> {
        let ty = self.ty(e);
        let span = self.span(e);
        let variant = match self.res(e) {
            ExprRes::Variant(v) => v,
            _ => {
                self.fail("an enum literal with no variant information");
                return None;
            }
        };
        let payload = match &self.pkg.expr(e).kind {
            ExprKind::EnumLit { payload, .. } => *payload,
            _ => None,
        };
        let mut fields = Vec::new();
        if let Some(p) = payload {
            let pt = self.ty(p);
            let op = self.lower_expr(p)?;
            fields.push(self.snapshot(op, pt, self.span(p)));
        }
        Some(self.materialize(
            Rvalue::Aggregate {
                kind: AggregateKind::Variant {
                    index: variant.index,
                    enum_ty: variant.enum_ty,
                },
                ty,
                ty_name: self.ty_name(ty),
                fields,
            },
            ty,
            span,
        ))
    }

    #[allow(clippy::too_many_arguments)]
    fn lower_par_for_stmt(
        &mut self,
        stmt: StmtId,
        grain: Option<ExprId>,
        vars: &[hir::ForVar],
        index: Option<&hir::ForVar>,
        iter: ExprId,
        range_end: Option<(bool, ExprId)>,
        step: Option<ExprId>,
        body: BlockId,
        span: Span,
        mark: usize,
    ) {
        let info = match self.body.stmt(stmt) {
            Some(StmtInfo::For(info)) => info.clone(),
            _ => {
                self.fail("a `par for` with no iteration information");
                return;
            }
        };
        // Grain, then the iterand.
        let grain_op = match grain {
            Some(g) => {
                let gt = self.ty(g);
                let Some(op) = self.lower_expr(g) else {
                    return;
                };
                Some(self.snapshot(op, gt, self.span(g)))
            }
            None => None,
        };
        let iterand = match &info.kind {
            ForKind::Range { ty, inclusive } => {
                let lo_ty = self.ty(iter);
                let Some(lo) = self.lower_expr(iter) else {
                    return;
                };
                let lo = self.snapshot(lo, lo_ty, self.span(iter));
                let hi = match range_end {
                    Some((_inc, hi)) => {
                        let ht = self.ty(hi);
                        let Some(h) = self.lower_expr(hi) else {
                            return;
                        };
                        self.snapshot(h, ht, self.span(hi))
                    }
                    None => Operand::Const(Const::Int { ty: *ty, value: 0 }),
                };
                let step_op = match step {
                    Some(s) => {
                        let st = self.ty(s);
                        let Some(op) = self.lower_expr(s) else {
                            return;
                        };
                        Some(self.snapshot(op, st, self.span(s)))
                    }
                    None => None,
                };
                ParIterand::Range {
                    lo,
                    hi,
                    inclusive: *inclusive,
                    step: step_op,
                }
            }
            _ => {
                let it = self.ty(iter);
                let Some(op) = self.lower_arg(iter) else {
                    return;
                };
                let op = self.snapshot(op, it, self.span(iter));
                ParIterand::Value(op)
            }
        };
        // The captures: one shared borrow per outer local the body names.
        let mut var_locals: Vec<(LocalId, TypeId)> = Vec::new();
        for v in vars {
            if let Some(l) = v.local {
                var_locals.push((l, self.local_ty(l)));
            }
        }
        let index_local = index.and_then(|v| v.local);
        let captures = self.par_captures(body, &var_locals, index_local);
        let mut capture_ops = Vec::new();
        let mut capture_places = Vec::new();
        let mut capture_names = Vec::new();
        let mut capture_tys = Vec::new();
        for (local, ty, name) in &captures {
            let place = self.local_place(*local);
            let rty = self.ref_ty(Mutability::Shared, *ty);
            let op = self.materialize(
                Rvalue::Borrow {
                    place: place.clone(),
                    mutability: Mutability::Shared,
                    two_phase: false,
                },
                rty,
                span,
            );
            capture_ops.push(op);
            capture_places.push(place);
            capture_names.push(name.clone());
            capture_tys.push(rty);
        }
        let tuple_ty = self.cx.decls.types.intern(TyKind::Tuple(capture_tys));
        let body_ref = self.body;
        let index_spec = index_local.map(|l| (l, self.local_ty(l)));
        let result = self.cx.lower_par_for(
            body_ref,
            stmt,
            body,
            var_locals.clone(),
            index_spec,
            captures,
            tuple_ty,
        );
        let body_id = match result {
            Ok(id) => id,
            Err(what) => {
                self.fail(what);
                return;
            }
        };
        let body_name = self.cx.out.get(body_id).name.clone();
        let next = self.new_block();
        self.term(Terminator::ParFor {
            body: body_id,
            body_name,
            captures: capture_ops,
            capture_places,
            capture_names,
            iterand,
            grain: grain_op,
            next,
        });
        self.kill();
        self.cur = next;
        self.reachable = true;
        self.end_temps(mark);
    }

    /// The outer locals a `par for` body names, in id order, with their names.
    fn par_captures(
        &self,
        body: BlockId,
        vars: &[(LocalId, TypeId)],
        index: Option<LocalId>,
    ) -> Vec<(LocalId, TypeId, String)> {
        let mut uses = Uses::default();
        uses.block(self.pkg, body);
        for (l, _) in vars {
            uses.declared.insert(*l);
        }
        if let Some(l) = index {
            uses.declared.insert(l);
        }
        let mut out = Vec::new();
        for l in &uses.used {
            if uses.declared.contains(l) {
                continue;
            }
            let ty = self.local_ty(*l);
            let name = self.pkg.local(*l).name.clone();
            out.push((*l, ty, name));
        }
        out
    }
}

// -------------------------------------------------------- local use collection

#[derive(Default)]
struct Uses {
    used: BTreeSet<LocalId>,
    declared: BTreeSet<LocalId>,
}

impl Uses {
    fn block(&mut self, pkg: &hir::Package, b: BlockId) {
        for &s in &pkg.block(b).stmts {
            self.stmt(pkg, s);
        }
    }

    fn pattern_decls(&mut self, pkg: &hir::Package, p: PatId) {
        match &pkg.pat(p).kind {
            PatKind::Bind { local } => {
                self.declared.insert(*local);
            }
            PatKind::Variant {
                payload: Some(sub), ..
            } => {
                self.pattern_decls(pkg, *sub);
            }
            PatKind::Tuple(ps) => {
                for p in ps {
                    self.pattern_decls(pkg, *p);
                }
            }
            PatKind::Struct { fields, .. } => {
                for f in fields {
                    self.pattern_decls(pkg, f.pattern);
                }
            }
            PatKind::Range { lo, hi, .. } => {
                self.pattern_decls(pkg, *lo);
                self.pattern_decls(pkg, *hi);
            }
            _ => {}
        }
    }

    fn stmt(&mut self, pkg: &hir::Package, id: StmtId) {
        match &pkg.stmt(id).kind {
            HirStmt::Let { pattern, init, .. } => {
                self.pattern_decls(pkg, *pattern);
                self.expr(pkg, *init);
            }
            HirStmt::Assign { place, value, .. } => {
                self.expr(pkg, *place);
                self.expr(pkg, *value);
            }
            HirStmt::Discard(e) | HirStmt::Expr(e) => self.expr(pkg, *e),
            HirStmt::Return(e) => {
                if let Some(e) = e {
                    self.expr(pkg, *e);
                }
            }
            HirStmt::Fail(e) => self.expr(pkg, *e),
            HirStmt::Break { .. } | HirStmt::Continue { .. } => {}
            HirStmt::Defer { body, .. } | HirStmt::Block(body) => self.block(pkg, *body),
            HirStmt::If {
                cond,
                then,
                otherwise,
            } => {
                self.expr(pkg, cond.expr);
                if let Some(p) = cond.let_pattern {
                    self.pattern_decls(pkg, p);
                }
                self.block(pkg, *then);
                if let Some(s) = otherwise {
                    self.stmt(pkg, *s);
                }
            }
            HirStmt::While { cond, body, .. } => {
                self.expr(pkg, cond.expr);
                if let Some(p) = cond.let_pattern {
                    self.pattern_decls(pkg, p);
                }
                self.block(pkg, *body);
            }
            HirStmt::Loop { body, .. } => self.block(pkg, *body),
            HirStmt::For {
                grain,
                vars,
                index,
                iter,
                range_end,
                step,
                body,
                ..
            } => {
                if let Some(g) = grain {
                    self.expr(pkg, *g);
                }
                self.expr(pkg, *iter);
                if let Some((_, hi)) = range_end {
                    self.expr(pkg, *hi);
                }
                if let Some(s) = step {
                    self.expr(pkg, *s);
                }
                for v in vars {
                    if let Some(l) = v.local {
                        self.declared.insert(l);
                    }
                }
                if let Some(v) = index
                    && let Some(l) = v.local
                {
                    self.declared.insert(l);
                }
                self.block(pkg, *body);
            }
            HirStmt::Match { scrutinee, arms } => {
                self.expr(pkg, *scrutinee);
                for arm in arms {
                    for p in &arm.patterns {
                        self.pattern_decls(pkg, *p);
                    }
                    if let Some(g) = arm.guard {
                        self.expr(pkg, g);
                    }
                    match &arm.body {
                        hir::ArmBody::Block(b) => self.block(pkg, *b),
                        hir::ArmBody::Expr(e) => self.expr(pkg, *e),
                    }
                }
            }
            HirStmt::Scope { local, body, .. } => {
                self.declared.insert(*local);
                self.block(pkg, *body);
            }
            HirStmt::Spawn { call, scope } => {
                self.expr(pkg, *call);
                self.expr(pkg, *scope);
            }
            HirStmt::Dispatch {
                call,
                domain,
                executor,
            } => {
                self.expr(pkg, *call);
                self.expr(pkg, *domain);
                self.expr(pkg, *executor);
            }
            HirStmt::Select { arms } => {
                for arm in arms {
                    self.pattern_decls(pkg, arm.pattern);
                    if let Some(c) = arm.call {
                        self.expr(pkg, c);
                    }
                    self.block(pkg, arm.body);
                }
            }
            HirStmt::Unsafe { body, .. } => self.block(pkg, *body),
            HirStmt::Poison => {}
        }
    }

    fn expr(&mut self, pkg: &hir::Package, id: ExprId) {
        match &pkg.expr(id).kind {
            ExprKind::Path(p) => {
                if let hir::Res::Local(l) = p.res {
                    self.used.insert(l);
                }
            }
            ExprKind::Block(b) => self.block(pkg, *b),
            ExprKind::Builtin { args, .. } => {
                for a in args {
                    self.expr(pkg, *a);
                }
            }
            ExprKind::Unary { expr, .. } => self.expr(pkg, *expr),
            ExprKind::Binary { lhs, rhs, .. } => {
                self.expr(pkg, *lhs);
                self.expr(pkg, *rhs);
            }
            ExprKind::Call { callee, args, .. } => {
                self.expr(pkg, *callee);
                for a in args {
                    self.expr(pkg, *a);
                }
            }
            ExprKind::Index { base, index, .. } => {
                self.expr(pkg, *base);
                self.expr(pkg, *index);
            }
            ExprKind::Slice { base, lo, hi, .. } => {
                self.expr(pkg, *base);
                if let Some(l) = lo {
                    self.expr(pkg, *l);
                }
                if let Some(h) = hi {
                    self.expr(pkg, *h);
                }
            }
            ExprKind::Field { base, .. } | ExprKind::TupleField { base, .. } => {
                self.expr(pkg, *base)
            }
            ExprKind::Deref { base, .. } | ExprKind::TypeApply { base, .. } => {
                self.expr(pkg, *base)
            }
            ExprKind::Cast { expr, .. } => self.expr(pkg, *expr),
            ExprKind::StructLit { fields, .. } | ExprKind::StructShorthand { fields, .. } => {
                for f in fields {
                    self.expr(pkg, f.value);
                }
            }
            ExprKind::EnumLit { payload, .. } => {
                if let Some(p) = payload {
                    self.expr(pkg, *p);
                }
            }
            ExprKind::ArrayLit { elems, .. } | ExprKind::Tuple(elems) => {
                for e in elems {
                    self.expr(pkg, *e);
                }
            }
            ExprKind::Range { lo, hi, .. } => {
                self.expr(pkg, *lo);
                self.expr(pkg, *hi);
            }
            ExprKind::Paren(e) => self.expr(pkg, *e),
            ExprKind::If {
                cond,
                then,
                otherwise,
            } => {
                self.expr(pkg, cond.expr);
                if let Some(p) = cond.let_pattern {
                    self.pattern_decls(pkg, p);
                }
                self.expr(pkg, *then);
                self.expr(pkg, *otherwise);
            }
            ExprKind::Match { scrutinee, arms } => {
                self.expr(pkg, *scrutinee);
                for arm in arms {
                    for p in &arm.patterns {
                        self.pattern_decls(pkg, *p);
                    }
                    if let Some(g) = arm.guard {
                        self.expr(pkg, g);
                    }
                    match &arm.body {
                        hir::ArmBody::Block(b) => self.block(pkg, *b),
                        hir::ArmBody::Expr(e) => self.expr(pkg, *e),
                    }
                }
            }
            ExprKind::Closure(c) => {
                for cap in &c.captures {
                    if let hir::Res::Local(l) = cap.outer {
                        self.used.insert(l);
                    }
                    self.declared.insert(cap.local);
                }
            }
            ExprKind::Catch {
                value,
                binding,
                handler,
                ..
            } => {
                self.expr(pkg, *value);
                if let Some(l) = binding {
                    self.declared.insert(*l);
                }
                match handler {
                    hir::Handler::Expr(e) => self.expr(pkg, *e),
                    hir::Handler::Block(b) => self.block(pkg, *b),
                }
            }
            ExprKind::Orelse { value, handler, .. } => {
                self.expr(pkg, *value);
                match handler {
                    hir::Handler::Expr(e) => self.expr(pkg, *e),
                    hir::Handler::Block(b) => self.block(pkg, *b),
                }
            }
            ExprKind::Spawn { call, scope } => {
                self.expr(pkg, *call);
                self.expr(pkg, *scope);
            }
            ExprKind::Dispatch {
                call,
                domain,
                executor,
            } => {
                self.expr(pkg, *call);
                self.expr(pkg, *domain);
                self.expr(pkg, *executor);
            }
            ExprKind::Unsafe { body, .. } => self.expr(pkg, *body),
            ExprKind::Int { .. }
            | ExprKind::Float(_)
            | ExprKind::Str(_)
            | ExprKind::CStr(_)
            | ExprKind::Bool(_)
            | ExprKind::Poison => {}
        }
    }
}

fn contains(v: &[Local], l: Local) -> bool {
    v.contains(&l)
}
