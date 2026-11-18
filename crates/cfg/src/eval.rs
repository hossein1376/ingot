//! Constant evaluation by interpreting CFGs (pipeline section 4.6, S5b).
//!
//! Every constant expression outside the direct evaluator's subset is lowered to a
//! CFG and run. The semantics are those of pipeline section 5: every check runs, a
//! failing check or a trap is E0806, and the trap texts are the catalog's. A read of a
//! `static`'s value, `unsafe`, an `extern` call and the concurrency terminators are
//! E0807. Steps, call depth and live bytes have the fixed limits of E0808.
//!
//! Memory is a value tree per allocation: the locals and parameters of each frame,
//! and the `static`s. A borrow is an allocation and a path; a slice is a borrow plus
//! `lo` and `len`; a string literal is its own read-only bytes.
//!
//! Bodies an evaluation reaches are typed by phase 7 on demand and lowered by phase 8
//! on demand, one CFG per key.

use crate::model::*;
use ingot_diag::source::Span;
use ingot_sema::consteval::{self, Fail, Trap as SeTrap};
use ingot_sema::defs::{DefId, DefKind};
use ingot_sema::hir::ExprId;
use ingot_sema::interp::{ConstLookup, Host, Interpreter};
use ingot_sema::sigs::ItemSig;
use ingot_sema::ty::{
    ConstArg, ConstExprId, ConstValue, FloatTy, GenericArg, IntTy, Subst, TyKind, TypeId,
};
use ingot_sema::typed::{BodyRoot, CastKind, FnRef, FnSource, InterfaceVia, TypedBody};
use ingot_sema::value::{AggKind as ValueAgg, AllocRef, PackElems, PathComp, SliceSource, Value};
use ingot_syntax::ast::{BinOp, UnOp};
use std::collections::{BTreeMap, BTreeSet};

/// At most 2^24 statements and terminators per constant expression.
const STEP_LIMIT: u64 = 1 << 24;
/// Calls nested at most 1024 deep.
const DEPTH_LIMIT: usize = 1024;
/// At most 2^24 bytes of values live at once.
const MEMORY_LIMIT: u64 = 1 << 24;

/// The lowered CFGs every evaluator shares. A body pulled forward goes through phase
/// 8 once (pipeline section 4.6), however many constant expressions reach it.
#[derive(Default)]
struct CacheData {
    cfgs: Vec<Cfg>,
    item_cfg: BTreeMap<DefId, usize>,
    index: BTreeMap<CfgKey, usize>,
}

#[derive(Default)]
pub struct EvaluatorCache {
    data: std::cell::RefCell<CacheData>,
}

impl EvaluatorCache {
    /// The number of CFGs lowered so far (a test observes that a body was shared).
    pub fn cfg_count(&self) -> usize {
        self.data.borrow().cfgs.len()
    }

    pub fn has_body(&self, def: DefId) -> bool {
        self.data.borrow().item_cfg.contains_key(&def)
    }
}

/// The interpreter of the indirect constant-evaluation path.
pub struct Evaluator<'a> {
    cache: &'a EvaluatorCache,
    /// Calls already in progress in the evaluations that pulled this body.
    base_depth: usize,
    /// 1 while the callee of a call is being typed (it is one level deeper than the
    /// frames already pushed).
    call_extra: usize,
    /// The outermost call, inherited from the evaluation that pulled this body.
    base_outer: Option<(usize, Span)>,
    mem: Vec<Option<Alloc>>,
    next_alloc: u32,
    frames: Vec<Frame>,
    /// One counter for the whole constant expression (a constant read inside another
    /// shares it).
    steps: u64,
    live: u64,
    /// The span of the outermost call, for E0808.
    outer_call: Option<(usize, Span)>,
    /// The span to name when there is no call.
    root_at: (usize, Span),
    /// The constant whose initializer is the outermost evaluation, for E0812.
    root_def: Option<DefId>,
    /// The current statement or terminator's span and file.
    at: (usize, Span),
}

struct Alloc {
    value: Value,
    alive: bool,
    size: u64,
}

struct Frame {
    cfg: usize,
    block: Block,
    /// The allocation of every local that has storage.
    locals: BTreeMap<Local, u32>,
    /// The parameters, whose storage starts with their argument in it.
    params: BTreeSet<Local>,
    /// The call in the constant expression that led here (E0806/E0807 `call`).
    label: Option<(usize, Span)>,
    /// Where this frame's `Return` delivers its value and how to continue.
    ret: Option<RetDest>,
    /// The generic arguments of the call, for `Const::Param` and `@sizeof(T)`.
    subst: BTreeMap<DefId, GenericArg>,
    /// The memory size of each local, computed when the frame starts.
    sizes: BTreeMap<Local, u64>,
    /// Whether this frame is a call, for the depth limit.
    is_call: bool,
}

struct RetDest {
    caller: usize,
    dest: Option<Place>,
    /// Wrap the value in `.ok`/`.err`: the callee is fallible.
    wrap: bool,
    next: Block,
}

/// Makes one fresh evaluator per constant expression (pipeline section 4.6), all
/// sharing one lowering cache. A nested evaluation gets its own counters and memory,
/// and inherits the call depth and outermost call of the evaluation that pulled its
/// body.
#[derive(Default)]
pub struct EvaluatorFactory {
    cache: EvaluatorCache,
}

impl EvaluatorFactory {
    pub fn cache(&self) -> &EvaluatorCache {
        &self.cache
    }
}

impl ingot_sema::interp::InterpreterFactory for EvaluatorFactory {
    fn make(&self, depth: usize, outer: Option<(usize, Span)>) -> Box<dyn Interpreter + '_> {
        Box::new(Evaluator::new(&self.cache, depth, outer))
    }
}

impl<'a> Evaluator<'a> {
    fn new(cache: &'a EvaluatorCache, depth: usize, outer: Option<(usize, Span)>) -> Self {
        Evaluator {
            cache,
            base_depth: depth,
            call_extra: 0,
            base_outer: outer,
            mem: Vec::new(),
            next_alloc: 0,
            frames: Vec::new(),
            steps: 0,
            live: 0,
            outer_call: outer,
            root_at: (0, Span::default()),
            root_def: None,
            at: (0, Span::default()),
        }
    }

    fn reset(&mut self, at: (usize, Span)) {
        self.steps = 0;
        self.live = 0;
        self.outer_call = self.base_outer;
        self.mem.clear();
        self.next_alloc = 0;
        self.frames.clear();
        self.root_at = at;
        self.at = at;
    }

    // -------------------------------------------------------------- lowering

    /// Lower a body (and its closures), returning the index of its item CFG.
    /// Tell the host the call depth and outermost call of the evaluation in progress,
    /// so a nested evaluation made during this host call inherits them.
    fn call_depth(&self) -> usize {
        self.frames.iter().filter(|f| f.is_call).count()
    }

    fn context(&self, host: &mut dyn Host) {
        host.set_eval_context(
            self.base_depth + self.call_depth() + self.call_extra,
            self.outer_call,
        );
    }

    /// Lower `body` and add its CFGs to the shared cache. `item`: this is a real item
    /// body, so it is cached by owner and never lowered twice.
    fn lower_body(
        &mut self,
        host: &mut dyn Host,
        body: &TypedBody,
        item: bool,
    ) -> Result<usize, Fail> {
        if item {
            let known = {
                let data = self.cache.data.borrow();
                data.item_cfg.get(&body.owner).copied()
            };
            if let Some(idx) = known {
                return Ok(idx);
            }
        }
        self.context(host);
        let lowered = {
            let parts = host.lowering();
            crate::lower::lower_for_eval(
                parts.sources,
                parts.sema,
                parts.packages,
                parts.declarations,
                body,
            )
        };
        let cfgs = match lowered {
            Ok(c) => c,
            Err(what) => {
                let what = format!("lowering: {what}");
                host.not_implemented(what.clone());
                return Err(Fail::NotImplemented(what));
            }
        };
        let mut data = self.cache.data.borrow_mut();
        if item && let Some(&idx) = data.item_cfg.get(&body.owner) {
            // A nested evaluation lowered the same body while this one was waiting.
            return Ok(idx);
        }
        let mut root = None;
        for cfg in cfgs.into_vec() {
            let key = cfg.key;
            let idx = data.cfgs.len();
            data.cfgs.push(cfg);
            if key.body == BodyKey::Item {
                if key.owner == body.owner {
                    root = Some(idx);
                }
            } else {
                data.index.entry(key).or_insert(idx);
            }
        }
        let root = root.ok_or_else(|| {
            Fail::NotImplemented("constant evaluation of a body with no CFG".to_string())
        })?;
        if item {
            data.item_cfg.insert(body.owner, root);
        }
        Ok(root)
    }

    /// The item CFG of `def`, lowered once per cache.
    fn cfg_of(&mut self, host: &mut dyn Host, def: DefId) -> Result<usize, Fail> {
        let known = {
            let data = self.cache.data.borrow();
            data.item_cfg.get(&def).copied()
        };
        if let Some(idx) = known {
            return Ok(idx);
        }
        self.context(host);
        let body = match host.typed_body(def) {
            Some(body) => body,
            None if host.body_in_progress(def) => return Err(self.cycle_body(host)),
            None => {
                let what = "constant evaluation of a body that was not typed".to_string();
                host.not_implemented(what.clone());
                return Err(Fail::NotImplemented(what));
            }
        };
        // Phase 7 reported the error already; the constant has no value and nothing
        // more is said about it (pipeline section 4.6).
        if body.failed {
            return Err(Fail::Reported);
        }
        self.lower_body(host, &body, true)
    }

    // ------------------------------------------------------------- execution

    #[allow(clippy::too_many_arguments)]
    fn run_cfg(
        &mut self,
        host: &mut dyn Host,
        cfg: usize,
        args: Vec<Value>,
        ret: Option<RetDest>,
        label: Option<(usize, Span)>,
        is_call: bool,
        subst: BTreeMap<DefId, GenericArg>,
    ) -> Result<Value, Fail> {
        let base = self.frames.len();
        self.push_frame(host, cfg, args, ret, label, is_call, subst)?;
        self.execute(host, base)
    }

    #[allow(clippy::too_many_arguments)]
    fn push_frame(
        &mut self,
        host: &mut dyn Host,
        cfg: usize,
        args: Vec<Value>,
        ret: Option<RetDest>,
        label: Option<(usize, Span)>,
        is_call: bool,
        subst: BTreeMap<DefId, GenericArg>,
    ) -> Result<(), Fail> {
        if is_call && self.base_depth + self.call_depth() >= DEPTH_LIMIT {
            let at = self.outer_call.unwrap_or(self.root_at);
            host.report(
                "E0808",
                "constant evaluation nests calls more than 1024 deep".to_string(),
                at.0,
                at.1,
                None,
            );
            return Err(Fail::Reported);
        }
        let (entry, params, local_tys) = {
            let data = self.cache.data.borrow();
            let cfg = &data.cfgs[cfg];
            let local_tys: Vec<(Local, TypeId)> = cfg
                .locals
                .iter()
                .enumerate()
                .map(|(i, l)| (i as Local, l.ty))
                .collect();
            (cfg.entry, cfg.params.clone(), local_tys)
        };
        let base = self.frames.len();
        let mut sizes = BTreeMap::new();
        for (l, ty) in local_tys {
            sizes.insert(l, self.type_size(host, ty));
        }
        self.frames.push(Frame {
            cfg,
            block: entry,
            locals: BTreeMap::new(),
            params: BTreeSet::new(),
            label,
            ret,
            subst,
            is_call,
            sizes,
        });
        for (i, value) in args.into_iter().enumerate() {
            if let Some(&local) = params.get(i) {
                let size = *self.frames[base].sizes.get(&local).unwrap_or(&0);
                let id = self.new_alloc(host, value, size)?;
                self.frames[base].locals.insert(local, id);
                self.frames[base].params.insert(local);
            }
        }
        Ok(())
    }

    /// Run the frame at `base` until it returns.
    fn execute(&mut self, host: &mut dyn Host, base: usize) -> Result<Value, Fail> {
        loop {
            if self.frames.len() <= base {
                return Ok(Value::Unit);
            }
            let fi = self.frames.len() - 1;
            let cfg = self.frames[fi].cfg;
            let block = self.frames[fi].block as usize;
            let (stmt_count, term, term_span, file) = {
                let data = self.cache.data.borrow();
                let Some(block_data) = data.cfgs[cfg].blocks.get(block) else {
                    return Err(self.inconsistent(host, "a block that does not exist"));
                };
                (
                    block_data.stmts.len(),
                    block_data.term.clone(),
                    block_data.term_span,
                    data.cfgs[cfg].file,
                )
            };
            for i in 0..stmt_count {
                // Storage and no-op statements are the hot ones in a loop; they have
                // only `Copy` payloads, so they need no clone of the statement.
                let (span, cheap) = {
                    let data = self.cache.data.borrow();
                    let stmt = &data.cfgs[cfg].blocks[block].stmts[i];
                    let cheap = match &stmt.kind {
                        StmtKind::StorageStart(l) => Some(Cheap::Start(*l)),
                        StmtKind::StorageEnd(l) => Some(Cheap::End(*l)),
                        StmtKind::Cancel(_)
                        | StmtKind::Abandon(_)
                        | StmtKind::Variant { .. }
                        | StmtKind::IterConsumed(_) => Some(Cheap::Nothing),
                        StmtKind::EnterUnsafe => Some(Cheap::Unsafe),
                        _ => None,
                    };
                    (stmt.span, cheap)
                };
                self.tick(host)?;
                self.at = (file, span);
                match cheap {
                    Some(Cheap::Start(l)) => {
                        if !self.frames[fi].params.contains(&l) {
                            let ty = {
                                let data = self.cache.data.borrow();
                                data.cfgs[self.frames[fi].cfg].local(l).ty
                            };
                            self.start_local(host, fi, l, ty)?;
                        }
                    }
                    Some(Cheap::End(l)) => self.end_local(fi, l),
                    Some(Cheap::Nothing) => {}
                    Some(Cheap::Unsafe) => {
                        return Err(self.e0807(host, E0807_UNSAFE));
                    }
                    None => {
                        let stmt = {
                            let data = self.cache.data.borrow();
                            data.cfgs[cfg].blocks[block].stmts[i].clone()
                        };
                        self.exec_stmt(host, fi, &stmt)?;
                    }
                }
            }
            if self.frames.len() <= base {
                return Ok(Value::Unit);
            }
            self.tick(host)?;
            self.at = (file, term_span);
            match self.exec_term(host, fi, &term)? {
                Step::Next(b) => {
                    if let Some(f) = self.frames.get_mut(fi) {
                        f.block = b;
                    }
                }
                Step::Returned(v) => {
                    if self.frames.len() == base {
                        return Ok(v);
                    }
                }
                // A call pushed a frame: the next pass runs it. The caller's block is
                // set when the callee returns.
                Step::Pushed => {}
            }
        }
    }

    fn tick(&mut self, host: &mut dyn Host) -> Result<(), Fail> {
        self.steps += 1;
        if self.steps > STEP_LIMIT {
            let at = self.outer_call.unwrap_or(self.root_at);
            host.report(
                "E0808",
                "constant evaluation takes more than 2^24 steps".to_string(),
                at.0,
                at.1,
                None,
            );
            return Err(Fail::Reported);
        }
        Ok(())
    }

    fn exec_stmt(&mut self, host: &mut dyn Host, fi: usize, stmt: &Stmt) -> Result<(), Fail> {
        match &stmt.kind {
            StmtKind::StorageStart(l) => {
                if self.frames[fi].params.contains(l) {
                    return Ok(());
                }
                let ty = {
                    let data = self.cache.data.borrow();
                    data.cfgs[self.frames[fi].cfg].local(*l).ty
                };
                self.start_local(host, fi, *l, ty)?;
                Ok(())
            }
            StmtKind::StorageEnd(l) => {
                self.end_local(fi, *l);
                Ok(())
            }
            StmtKind::Assign { place, value } => {
                let dest_ty = self.place_ty(fi, Some(place));
                let v = self.rvalue(host, fi, value, dest_ty)?;
                let (alloc, path) = self.resolve_place(host, fi, place)?;
                self.write_at(host, alloc, &path, v)
            }
            StmtKind::Cancel(_)
            | StmtKind::Abandon(_)
            | StmtKind::Variant { .. }
            | StmtKind::IterConsumed(_) => Ok(()),
            StmtKind::EnterUnsafe => Err(self.e0807(host, E0807_UNSAFE)),
            StmtKind::Check(check) => self.check(host, fi, check),
            StmtKind::Use(op) => {
                let _ = self.operand(host, fi, op)?;
                Ok(())
            }
        }
    }

    fn exec_term(
        &mut self,
        host: &mut dyn Host,
        fi: usize,
        term: &Terminator,
    ) -> Result<Step, Fail> {
        match term {
            Terminator::Goto(b) => Ok(Step::Next(*b)),
            Terminator::Branch {
                cond,
                then_block,
                else_block,
            } => match self.operand(host, fi, cond)? {
                Value::Bool(true) => Ok(Step::Next(*then_block)),
                Value::Bool(false) => Ok(Step::Next(*else_block)),
                _ => Err(self.inconsistent(host, "a branch on a value that is not bool")),
            },
            Terminator::Switch {
                value,
                cases,
                default,
            } => {
                let v = self.operand(host, fi, value)?;
                let tag = match v {
                    Value::Int { value, .. } => value,
                    _ => {
                        return Err(
                            self.inconsistent(host, "a switch on a value that is not an enum tag")
                        );
                    }
                };
                for (case, block) in cases {
                    if *case == tag {
                        return Ok(Step::Next(*block));
                    }
                }
                Ok(Step::Next(*default))
            }
            Terminator::Return {
                value,
                is_error,
                end,
            } => {
                let v = match value {
                    Some(op) => self.operand(host, fi, op)?,
                    None => Value::Unit,
                };
                if let Some(l) = end {
                    self.end_local(fi, *l);
                }
                self.leave_frame(host, fi, v, *is_error)
            }
            Terminator::Call {
                target,
                args,
                dest,
                next,
            } => {
                if self.frames[fi].label.is_none() && self.outer_call.is_none() {
                    self.outer_call = Some(self.at);
                }
                self.call(host, fi, target, args, dest.as_ref(), *next)
            }
            Terminator::Trap {
                code,
                operands,
                source,
            } => {
                let values = self.operands(host, fi, operands)?;
                let text = trap_text(*code, source.as_deref(), &values);
                let label = self.frames[fi].label;
                host.report(
                    "E0806",
                    format!("constant evaluation trapped: {}: {text}", code.code()),
                    self.at.0,
                    self.at.1,
                    label,
                );
                Err(Fail::Reported)
            }
            Terminator::ScopeWait { next, .. } => Ok(Step::Next(*next)),
            Terminator::Spawn { .. }
            | Terminator::Select { .. }
            | Terminator::ParFor { .. }
            | Terminator::Dispatch { .. } => Err(self.e0807(host, E0807_CONCURRENCY)),
            Terminator::Unreachable => {
                Err(self.inconsistent(host, "control reached an unreachable block"))
            }
        }
    }

    fn call(
        &mut self,
        host: &mut dyn Host,
        fi: usize,
        target: &CallTarget,
        args: &[Operand],
        dest: Option<&Place>,
        next: Block,
    ) -> Result<Step, Fail> {
        self.context(host);
        // The callee of a value call is evaluated first, then the arguments.
        let callee = match target {
            CallTarget::Value { callee, .. } => Some(self.operand(host, fi, callee)?),
            CallTarget::Fn { .. } => None,
        };
        let mut values = self.operands(host, fi, args)?;
        let caller_subst = self.frames[fi].subst.clone();
        // The callee's signature and body are built one call deeper than the frames
        // already pushed: a constant evaluated while building either inherits that depth.
        self.call_extra = 1;
        self.context(host);
        let (def, fn_ref, wrap, closure, mut subst) = match (target, callee) {
            (CallTarget::Fn { fn_ref, .. }, _) => {
                let (fn_ref, subst) = self.effective_fn(host, fn_ref)?;
                let Some(sig) = host.fn_sig(fn_ref.def) else {
                    return Err(self.missing_signature(host, fn_ref.def));
                };
                if host.def_kind(fn_ref.def) == DefKind::ExternFn {
                    return Err(self.e0807(host, E0807_EXTERN));
                }
                if !sig.has_body {
                    return Err(Fail::NotImplemented(
                        "constant evaluation of a call of a function with no body".to_string(),
                    ));
                }
                (fn_ref.def, fn_ref, sig.error.is_some(), None, subst)
            }
            (CallTarget::Value { kind, .. }, Some(v)) => match kind {
                ValueKind::FnPtr => match v {
                    Value::Fn { fn_ref, .. } => {
                        let (fn_ref, subst) = self.effective_fn(host, &fn_ref)?;
                        let Some(sig) = host.fn_sig(fn_ref.def) else {
                            return Err(self.missing_signature(host, fn_ref.def));
                        };
                        if host.def_kind(fn_ref.def) == DefKind::ExternFn {
                            return Err(self.e0807(host, E0807_EXTERN));
                        }
                        (fn_ref.def, fn_ref, sig.error.is_some(), None, subst)
                    }
                    _ => {
                        return Err(Fail::NotImplemented(
                            "constant evaluation of a call through a value that is not a function"
                                .to_string(),
                        ));
                    }
                },
                ValueKind::Closure(e) => {
                    values.insert(0, v);
                    let owner = {
                        let data = self.cache.data.borrow();
                        data.cfgs[self.frames[fi].cfg].key.owner
                    };
                    (
                        owner,
                        FnRef {
                            def: owner,
                            args: Vec::new(),
                            source: FnSource::Free,
                        },
                        false,
                        Some(*e),
                        caller_subst.clone(),
                    )
                }
                ValueKind::Bound | ValueKind::DynFn => {
                    return Err(Fail::NotImplemented(
                        "constant evaluation of a call through a generic bound or `dyn Fn`"
                            .to_string(),
                    ));
                }
            },
            (CallTarget::Value { .. }, None) => {
                return Err(Fail::NotImplemented(
                    "constant evaluation of a call with no callee".to_string(),
                ));
            }
        };
        subst.extend(self.substitution(host, def, &fn_ref.args, &caller_subst));
        let ret = RetDest {
            caller: fi,
            dest: dest.cloned(),
            wrap,
            next,
        };
        let label = self.frames[fi].label.or(Some(self.at));
        let cfg = match closure {
            None => self.cfg_of(host, def)?,
            Some(e) => {
                let owner = {
                    let data = self.cache.data.borrow();
                    data.cfgs[self.frames[fi].cfg].key.owner
                };
                let idx = {
                    let data = self.cache.data.borrow();
                    data.index
                        .get(&CfgKey {
                            owner,
                            body: BodyKey::Closure(e),
                        })
                        .copied()
                };
                idx.ok_or_else(|| {
                    Fail::NotImplemented(
                        "constant evaluation of a closure body with no CFG".to_string(),
                    )
                })?
            }
        };
        self.push_frame(host, cfg, values, Some(ret), label, true, subst)?;
        self.call_extra = 0;
        Ok(Step::Pushed)
    }

    /// A call that needs a body that is being typed is a constant that depends on the
    /// body that asked for it (E0812, pipeline section 4.6).
    fn cycle_body(&mut self, host: &mut dyn Host) -> Fail {
        if let Some(d) = self.root_def {
            host.report_cycle(d);
        } else {
            host.report(
                "E0812",
                "the value of this constant expression depends on the body that is being \
                 typed"
                    .to_string(),
                self.at.0,
                self.at.1,
                None,
            );
        }
        Fail::Reported
    }

    /// A call that needs a signature that is being built is a constant that depends on
    /// the signature that asked for it (E0812, pipeline section 4.6).
    fn missing_signature(&mut self, host: &mut dyn Host, def: DefId) -> Fail {
        if host.sig_in_progress(def) {
            match self.root_def {
                Some(d) => {
                    host.report_cycle(d);
                    Fail::Reported
                }
                None => {
                    host.report(
                        "E0812",
                        "the value of this constant expression depends on a signature that \
                         depends on it"
                            .to_string(),
                        self.at.0,
                        self.at.1,
                        None,
                    );
                    Fail::Reported
                }
            }
        } else {
            Fail::NotImplemented(
                "constant evaluation of a call whose signature is not available".to_string(),
            )
        }
    }

    /// The function a call really runs: a call through a bound is the impl's method
    /// for the concrete type, and an inherent method carries its impl's arguments.
    fn effective_fn(
        &mut self,
        host: &mut dyn Host,
        fn_ref: &FnRef,
    ) -> Result<(FnRef, BTreeMap<DefId, GenericArg>), Fail> {
        let mut subst = BTreeMap::new();
        match &fn_ref.source {
            FnSource::Interface {
                interface,
                self_ty,
                via: InterfaceVia::Bound,
                ..
            } => {
                let self_ty = self.subst_ty(host, *self_ty);
                let name = host.def_name(fn_ref.def);
                let impls = host
                    .declarations()
                    .impls
                    .by_interface
                    .get(interface)
                    .cloned()
                    .unwrap_or_default();
                for imp in impls {
                    let Some(sig) = host.declarations().impl_sig(imp).cloned() else {
                        continue;
                    };
                    let binders: Vec<DefId> = host
                        .declarations()
                        .generics_of(imp)
                        .iter()
                        .map(|g| g.def)
                        .collect();
                    let mut s = Subst::new();
                    if host
                        .declarations()
                        .match_ty(sig.target, self_ty, &binders, &mut s)
                        && let Some(&method) = sig.methods.get(&name)
                    {
                        subst = s.map;
                        return Ok((
                            FnRef {
                                def: method,
                                args: fn_ref.args.clone(),
                                source: FnSource::Free,
                            },
                            subst,
                        ));
                    }
                }
                let ty = host.type_name(self_ty);
                Err(Fail::NotImplemented(format!(
                    "constant evaluation of a call through a bound on `{ty}`"
                )))
            }
            FnSource::Inherent {
                impl_def,
                impl_args,
            } => {
                let params: Vec<DefId> = host
                    .declarations()
                    .generics_of(*impl_def)
                    .iter()
                    .map(|g| g.def)
                    .collect();
                let caller: BTreeMap<DefId, GenericArg> = self
                    .frames
                    .last()
                    .map(|f| f.subst.clone())
                    .unwrap_or_default();
                for (p, a) in params.into_iter().zip(impl_args.iter()) {
                    let a = match a {
                        GenericArg::Ty(t) => GenericArg::Ty(self.subst_with(host, *t, &caller)),
                        GenericArg::Const(c) => GenericArg::Const(subst_const_arg(*c, &caller)),
                    };
                    subst.insert(p, a);
                }
                Ok((fn_ref.clone(), subst))
            }
            _ => Ok((fn_ref.clone(), subst)),
        }
    }

    fn substitution(
        &mut self,
        host: &mut dyn Host,
        def: DefId,
        args: &[GenericArg],
        caller: &BTreeMap<DefId, GenericArg>,
    ) -> BTreeMap<DefId, GenericArg> {
        let params: Vec<DefId> = host
            .declarations()
            .generics_of(def)
            .iter()
            .map(|g| g.def)
            .collect();
        let mut m = BTreeMap::new();
        for (p, a) in params.into_iter().zip(args.iter()) {
            let a = match a {
                GenericArg::Ty(t) => GenericArg::Ty(self.subst_with(host, *t, caller)),
                GenericArg::Const(c) => GenericArg::Const(subst_const_arg(*c, caller)),
            };
            m.insert(p, a);
        }
        m
    }

    /// Pop `fi` and deliver its value to the caller (or to the root evaluation).
    fn leave_frame(
        &mut self,
        host: &mut dyn Host,
        fi: usize,
        value: Value,
        is_error: bool,
    ) -> Result<Step, Fail> {
        let frame = self.frames.pop().expect("a frame to leave");
        debug_assert_eq!(self.frames.len(), fi);
        self.release_frame(&frame);
        let Some(ret) = frame.ret else {
            return Ok(Step::Returned(value));
        };
        let mut value = value;
        if ret.wrap {
            let Some(ty) = self.place_ty(ret.caller, ret.dest.as_ref()) else {
                return Err(Fail::NotImplemented(
                    "constant evaluation of a fallible call with no destination".to_string(),
                ));
            };
            let payload = match value {
                Value::Unit => None,
                other => Some(Box::new(other)),
            };
            value = Value::Variant {
                ty,
                index: usize::from(is_error),
                payload,
            };
        }
        if let Some(dest) = ret.dest {
            let (alloc, path) = self.resolve_place(host, ret.caller, &dest)?;
            self.write_at(host, alloc, &path, value)?;
        }
        if let Some(caller) = self.frames.get_mut(ret.caller) {
            caller.block = ret.next;
        }
        Ok(Step::Returned(Value::Unit))
    }

    /// Release every allocation the frame still holds (parameters included). A borrow
    /// of a local cannot be a constant value (phase 9 rejects it), so nothing live can
    /// point into them after the return.
    fn release_frame(&mut self, frame: &Frame) {
        for id in frame.locals.values() {
            if let Some(Some(a)) = self.mem.get_mut(*id as usize)
                && a.alive
            {
                a.alive = false;
                a.value = Value::Uninit;
                self.live = self.live.saturating_sub(a.size);
            }
        }
    }

    fn place_ty(&self, fi: usize, place: Option<&Place>) -> Option<TypeId> {
        let place = place?;
        let cfg = self.frames.get(fi)?.cfg;
        match &place.base {
            PlaceBase::Local(l) => {
                let data = self.cache.data.borrow();
                Some(data.cfgs[cfg].local(*l).ty)
            }
            PlaceBase::Static(_) => None,
        }
    }

    // -------------------------------------------------------------- memory

    fn new_alloc(&mut self, host: &mut dyn Host, value: Value, size: u64) -> Result<u32, Fail> {
        self.grow(host, size)?;
        self.live += size;
        let id = self.next_alloc;
        self.next_alloc += 1;
        let slot = id as usize;
        if slot >= self.mem.len() {
            self.mem.resize_with(slot + 1, || None);
        }
        self.mem[slot] = Some(Alloc {
            value,
            alive: true,
            size,
        });
        Ok(id)
    }

    fn start_local(
        &mut self,
        host: &mut dyn Host,
        fi: usize,
        l: Local,
        _ty: TypeId,
    ) -> Result<u32, Fail> {
        let size = *self.frames[fi].sizes.get(&l).unwrap_or(&0);
        if let Some(&id) = self.frames[fi].locals.get(&l) {
            let size = self
                .mem
                .get(id as usize)
                .and_then(|a| a.as_ref())
                .map(|a| a.size)
                .unwrap_or(0);
            if !self
                .mem
                .get(id as usize)
                .and_then(|a| a.as_ref())
                .is_some_and(|a| a.alive)
            {
                self.grow(host, size)?;
                self.live += size;
            }
            if let Some(Some(a)) = self.mem.get_mut(id as usize) {
                a.value = Value::Uninit;
                a.alive = true;
            }
            return Ok(id);
        }
        let id = self.new_alloc(host, Value::Uninit, size)?;
        self.frames[fi].locals.insert(l, id);
        Ok(id)
    }

    fn end_local(&mut self, fi: usize, l: Local) {
        let Some(&id) = self.frames.get(fi).and_then(|f| f.locals.get(&l)) else {
            return;
        };
        if let Some(Some(a)) = self.mem.get_mut(id as usize)
            && a.alive
        {
            a.alive = false;
            a.value = Value::Uninit;
            self.live = self.live.saturating_sub(a.size);
        }
    }

    fn grow(&mut self, host: &mut dyn Host, bytes: u64) -> Result<(), Fail> {
        if self.live + bytes > MEMORY_LIMIT {
            let at = self.outer_call.unwrap_or(self.root_at);
            host.report(
                "E0808",
                "constant evaluation holds more than 2^24 bytes of values".to_string(),
                at.0,
                at.1,
                None,
            );
            return Err(Fail::Reported);
        }
        Ok(())
    }

    /// The size of a closure record: the sum of its capture fields' sizes. The type
    /// table has no layout for a closure, and asking for one would record an entry.
    fn closure_size(&mut self, host: &mut dyn Host, owner: DefId, expr: ExprId) -> u64 {
        let tys: Vec<TypeId> = {
            let Some(body) = host.declarations().bodies.get(&owner) else {
                return 0;
            };
            let Some(info) = body.closures.get(&expr) else {
                return 0;
            };
            info.captures
                .iter()
                .filter_map(|c| body.locals.get(&c.local).map(|l| l.ty))
                .collect()
        };
        let mut total = 0;
        for ty in tys {
            total += self.type_size(host, ty);
        }
        total
    }

    /// `ty` with the current call's type and constant arguments substituted.
    fn subst_ty(&mut self, host: &mut dyn Host, ty: TypeId) -> TypeId {
        match self.frames.last() {
            Some(f) if !f.subst.is_empty() => {
                let subst = f.subst.clone();
                self.subst_with(host, ty, &subst)
            }
            _ => ty,
        }
    }

    fn subst_with(
        &mut self,
        host: &mut dyn Host,
        ty: TypeId,
        subst: &BTreeMap<DefId, GenericArg>,
    ) -> TypeId {
        let kind = host.declarations().types.kind(ty).clone();
        match kind {
            TyKind::Param(p) => match subst.get(&p) {
                Some(GenericArg::Ty(t)) => *t,
                _ => ty,
            },
            TyKind::Array(t, len) => {
                let t = self.subst_with(host, t, subst);
                let len = subst_const_arg(len, subst);
                intern(host, TyKind::Array(t, len))
            }
            TyKind::Tuple(ts) => {
                let ts: Vec<_> = ts
                    .into_iter()
                    .map(|t| self.subst_with(host, t, subst))
                    .collect();
                intern(host, TyKind::Tuple(ts))
            }
            TyKind::Adt(d, args) => {
                let args: Vec<_> = args
                    .into_iter()
                    .map(|a| subst_arg(host, a, subst, self))
                    .collect();
                intern(host, TyKind::Adt(d, args))
            }
            TyKind::Optional(t) => {
                let t = self.subst_with(host, t, subst);
                intern(host, TyKind::Optional(t))
            }
            TyKind::Result(t, e) => {
                let t = self.subst_with(host, t, subst);
                let e = self.subst_with(host, e, subst);
                intern(host, TyKind::Result(t, e))
            }
            TyKind::Ref(m, t) => {
                let t = self.subst_with(host, t, subst);
                intern(host, TyKind::Ref(m, t))
            }
            TyKind::Slice(m, t) => {
                let t = self.subst_with(host, t, subst);
                intern(host, TyKind::Slice(m, t))
            }
            TyKind::RawPtr(m, t) => {
                let t = self.subst_with(host, t, subst);
                intern(host, TyKind::RawPtr(m, t))
            }
            TyKind::Projection(t, d) => {
                let t = self.subst_with(host, t, subst);
                intern(host, TyKind::Projection(t, d))
            }
            TyKind::FnPtr(mut sig) => {
                sig.params = sig
                    .params
                    .into_iter()
                    .map(|t| self.subst_with(host, t, subst))
                    .collect();
                sig.ret = self.subst_with(host, sig.ret, subst);
                sig.error = sig.error.map(|t| self.subst_with(host, t, subst));
                intern(host, TyKind::FnPtr(sig))
            }
            _ => ty,
        }
    }

    fn type_size(&mut self, host: &mut dyn Host, ty: TypeId) -> u64 {
        let ty = self.subst_ty(host, ty);
        let kind = host.declarations().types.kind(ty).clone();
        match kind {
            TyKind::Slice(..) | TyKind::Dyn(..) => 16,
            TyKind::Ref(..) | TyKind::RawPtr(..) | TyKind::FnPtr(_) => 8,
            TyKind::Void | TyKind::Never => 0,
            TyKind::Closure(owner, expr, _) => self.closure_size(host, owner, ExprId(expr)),
            _ => {
                self.context(host);
                host.layout(ty).map(|l| l.size).unwrap_or(0)
            }
        }
    }

    // ------------------------------------------------------------ places

    fn resolve_place(
        &mut self,
        host: &mut dyn Host,
        fi: usize,
        place: &Place,
    ) -> Result<(AllocRef, Vec<PathComp>), Fail> {
        let mut alloc = match place.base {
            PlaceBase::Local(l) => {
                let id = *self
                    .frames
                    .get(fi)
                    .and_then(|f| f.locals.get(&l))
                    .ok_or_else(|| self.inconsistent(host, "a local that has no storage"))?;
                AllocRef::Local(id)
            }
            PlaceBase::Static(d) => AllocRef::Static(d),
        };
        let mut path = Vec::new();
        for proj in &place.proj {
            match proj {
                Projection::Field { index, .. } | Projection::TupleField(index) => {
                    path.push(PathComp::Field(*index));
                }
                Projection::Payload { variant } => path.push(PathComp::Payload(*variant)),
                Projection::Index(op) => {
                    let i = match self.operand(host, fi, op)? {
                        Value::Int { value, .. } => value,
                        _ => {
                            return Err(self.inconsistent(host, "an index that is not an integer"));
                        }
                    };
                    if i < 0 {
                        return Err(self.inconsistent(host, "a negative index"));
                    }
                    path.push(PathComp::Field(i as usize));
                }
                Projection::Deref | Projection::DerefRaw => {
                    let v = self.read_at(host, alloc, &path)?;
                    match v {
                        Value::Borrow {
                            alloc: a2,
                            path: p2,
                        } => {
                            alloc = a2;
                            path = p2;
                        }
                        _ => {
                            return Err(self.inconsistent(
                                host,
                                "a dereference of a value that is not a borrow",
                            ));
                        }
                    }
                }
            }
        }
        Ok((alloc, path))
    }

    fn read_place(&mut self, host: &mut dyn Host, fi: usize, place: &Place) -> Result<Value, Fail> {
        let (alloc, path) = self.resolve_place(host, fi, place)?;
        self.read_at(host, alloc, &path)
    }

    fn read_at(
        &self,
        host: &mut dyn Host,
        alloc: AllocRef,
        path: &[PathComp],
    ) -> Result<Value, Fail> {
        if let AllocRef::Static(_) = alloc {
            host.report(
                "E0807",
                E0807_STATIC.to_string(),
                self.at.0,
                self.at.1,
                self.cur_label(),
            );
            return Err(Fail::Reported);
        }
        let mut alloc = alloc;
        let mut comps: Vec<PathComp> = path.to_vec();
        'outer: loop {
            let AllocRef::Local(id) = alloc else {
                return Err(self.no_value());
            };
            let Some(Some(a)) = self.mem.get(id as usize) else {
                return Err(self.no_value());
            };
            if !a.alive {
                return Err(self.no_value());
            }
            let mut v: &Value = &a.value;
            for i in 0..comps.len() {
                match (v, comps[i]) {
                    (Value::Uninit, _) => return Err(self.no_value()),
                    (Value::Aggregate { fields, .. }, PathComp::Field(j)) => match fields.get(j) {
                        Some(field) => v = field,
                        None => return Err(self.no_value()),
                    },
                    (
                        Value::Variant {
                            payload: Some(p), ..
                        },
                        PathComp::Payload(_),
                    ) => {
                        v = p;
                    }
                    (Value::Variant { payload: None, .. }, PathComp::Payload(_)) => {
                        return Err(self.no_value());
                    }
                    (
                        Value::Pack {
                            ty,
                            len,
                            elems,
                            moved,
                        },
                        PathComp::Field(j),
                    ) => {
                        if j as u64 >= *len || i + 1 != comps.len() {
                            return Err(self.no_value());
                        }
                        if moved.contains(&(j as u32)) {
                            return Err(self.no_value());
                        }
                        return Ok(pack_element(host, *ty, elems.as_deref(), j));
                    }
                    (Value::Str(bytes), PathComp::Field(j)) => {
                        if i + 1 != comps.len() {
                            return Err(self.no_value());
                        }
                        return match bytes.get(j) {
                            Some(b) => Ok(byte_value(host, *b)),
                            None => Err(self.no_value()),
                        };
                    }
                    (Value::Slice { source, lo, len }, PathComp::Field(j)) => {
                        if j as u64 >= *len {
                            return Err(self.no_value());
                        }
                        match source {
                            SliceSource::Str(bytes) => {
                                if i + 1 != comps.len() {
                                    return Err(self.no_value());
                                }
                                return match bytes.get(*lo as usize + j) {
                                    Some(b) => Ok(byte_value(host, *b)),
                                    None => Err(self.no_value()),
                                };
                            }
                            SliceSource::Alloc {
                                alloc: a2,
                                path: p2,
                            } => {
                                let mut full = p2.clone();
                                full.push(PathComp::Field(*lo as usize + j));
                                full.extend_from_slice(&comps[i + 1..]);
                                alloc = *a2;
                                comps = full;
                                continue 'outer;
                            }
                        }
                    }
                    (
                        Value::Borrow {
                            alloc: a2,
                            path: p2,
                        },
                        _,
                    ) => {
                        let mut full = p2.clone();
                        full.extend_from_slice(&comps[i..]);
                        alloc = *a2;
                        comps = full;
                        continue 'outer;
                    }
                    _ => return Err(self.no_value()),
                }
            }
            return Ok(v.clone());
        }
    }

    /// The value at `path`, by reference, for a prefix that is an aggregate or a
    /// slice. Used by writes to find the allocation a slice points into without
    /// copying the value.
    fn value_ref(&self, alloc: AllocRef, path: &[PathComp]) -> Result<&Value, Fail> {
        let mut alloc = alloc;
        let mut comps: Vec<PathComp> = path.to_vec();
        'outer: loop {
            let AllocRef::Local(id) = alloc else {
                return Err(self.no_value());
            };
            let Some(Some(a)) = self.mem.get(id as usize) else {
                return Err(self.no_value());
            };
            if !a.alive {
                return Err(self.no_value());
            }
            let mut v: &Value = &a.value;
            for i in 0..comps.len() {
                match (v, comps[i]) {
                    (Value::Uninit, _) => return Err(self.no_value()),
                    (Value::Aggregate { fields, .. }, PathComp::Field(j)) => match fields.get(j) {
                        Some(field) => v = field,
                        None => return Err(self.no_value()),
                    },
                    (
                        Value::Variant {
                            payload: Some(p), ..
                        },
                        PathComp::Payload(_),
                    ) => {
                        v = p;
                    }
                    (Value::Variant { payload: None, .. }, PathComp::Payload(_)) => {
                        return Err(self.no_value());
                    }
                    (Value::Slice { source, lo, len }, PathComp::Field(j)) => {
                        if j as u64 >= *len {
                            return Err(self.no_value());
                        }
                        match source {
                            SliceSource::Str(_) => return Err(self.no_value()),
                            SliceSource::Alloc {
                                alloc: a2,
                                path: p2,
                            } => {
                                let mut full = p2.clone();
                                full.push(PathComp::Field(*lo as usize + j));
                                full.extend_from_slice(&comps[i + 1..]);
                                alloc = *a2;
                                comps = full;
                                continue 'outer;
                            }
                        }
                    }
                    (
                        Value::Borrow {
                            alloc: a2,
                            path: p2,
                        },
                        _,
                    ) => {
                        let mut full = p2.clone();
                        full.extend_from_slice(&comps[i..]);
                        alloc = *a2;
                        comps = full;
                        continue 'outer;
                    }
                    _ => return Err(self.no_value()),
                }
            }
            return Ok(v);
        }
    }

    fn cur_label(&self) -> Option<(usize, Span)> {
        self.frames.last().and_then(|f| f.label)
    }

    fn write_at(
        &mut self,
        host: &mut dyn Host,
        alloc: AllocRef,
        path: &[PathComp],
        value: Value,
    ) -> Result<(), Fail> {
        if let AllocRef::Static(_) = alloc {
            host.report(
                "E0807",
                E0807_STATIC.to_string(),
                self.at.0,
                self.at.1,
                self.cur_label(),
            );
            return Err(Fail::Reported);
        }
        let (alloc, path) = self.splice(host, alloc, path)?;
        let AllocRef::Local(id) = alloc else {
            host.report(
                "E0807",
                E0807_STATIC.to_string(),
                self.at.0,
                self.at.1,
                self.cur_label(),
            );
            return Err(Fail::Reported);
        };
        let Some(Some(a)) = self.mem.get_mut(id as usize) else {
            return Err(self.no_value());
        };
        if !a.alive {
            return Err(self.no_value());
        }
        set_in(&mut a.value, &path, value)
    }

    /// Replace every slice in the prefix of `path` by its source.
    fn splice(
        &self,
        _host: &mut dyn Host,
        alloc: AllocRef,
        path: &[PathComp],
    ) -> Result<(AllocRef, Vec<PathComp>), Fail> {
        let mut alloc = alloc;
        let mut path = path.to_vec();
        let mut i = 0;
        while i < path.len() {
            let found = match self.value_ref(alloc, &path[..i])? {
                Value::Slice {
                    source:
                        SliceSource::Alloc {
                            alloc: a2,
                            path: p2,
                        },
                    lo,
                    ..
                } => Some((*a2, p2.clone(), *lo)),
                _ => None,
            };
            if let Some((a2, p2, lo)) = found
                && let PathComp::Field(j) = path[i]
            {
                let mut np = p2.clone();
                np.push(PathComp::Field(lo as usize + j));
                np.extend_from_slice(&path[i + 1..]);
                path = np;
                alloc = a2;
                i = p2.len();
                continue;
            }
            i += 1;
        }
        Ok((alloc, path))
    }

    fn clear_place(&mut self, host: &mut dyn Host, fi: usize, place: &Place) -> Result<(), Fail> {
        let (alloc, path) = self.resolve_place(host, fi, place)?;
        self.write_at(host, alloc, &path, Value::Uninit)
    }

    // ----------------------------------------------------------- operands

    fn const_expr_value(
        &mut self,
        host: &mut dyn Host,
        fi: usize,
        id: ConstExprId,
    ) -> Result<Value, Fail> {
        use ingot_sema::ty::ConstExpr;
        match host.declarations().types.const_expr(id).clone() {
            ConstExpr::Int { ty, value } => {
                let ty = host.declarations_mut().types.int(ty);
                Ok(Value::Int { ty, value })
            }
            ConstExpr::Bool(b) => Ok(Value::Bool(b)),
            ConstExpr::Param(p) => {
                let arg = self.frames.get(fi).and_then(|f| f.subst.get(&p)).cloned();
                match arg {
                    Some(GenericArg::Const(ConstArg::Value(v))) => Ok(value_of_const(host, v)),
                    Some(GenericArg::Const(ConstArg::Expr(id))) => {
                        self.const_expr_value(host, fi, id)
                    }
                    _ => Err(Fail::NotImplemented(
                        "constant evaluation of a constant generic parameter with no value"
                            .to_string(),
                    )),
                }
            }
            ConstExpr::Unary { op, expr } => match self.const_expr_value(host, fi, expr)? {
                Value::Int { ty, value } => {
                    let it = match host.declarations().types.kind(ty) {
                        TyKind::Int(i) => *i,
                        _ => return Err(self.inconsistent(host, "a non-integer constant")),
                    };
                    let value = consteval::int_unary(op, it, value)
                        .map_err(|t| self.tree_trap(host, &t))?;
                    Ok(Value::Int { ty, value })
                }
                Value::Bool(b) if op == UnOp::Not => Ok(Value::Bool(!b)),
                _ => Err(self.inconsistent(host, "an invalid unary constant operation")),
            },
            ConstExpr::Binary { op, lhs, rhs } => {
                let a = self.const_expr_value(host, fi, lhs)?;
                let b = self.const_expr_value(host, fi, rhs)?;
                match (a, b) {
                    (Value::Int { ty, value: x }, Value::Int { value: y, .. }) => {
                        let it = match host.declarations().types.kind(ty) {
                            TyKind::Int(i) => *i,
                            _ => return Err(self.inconsistent(host, "a non-integer constant")),
                        };
                        match op {
                            BinOp::Eq => Ok(Value::Bool(x == y)),
                            BinOp::Ne => Ok(Value::Bool(x != y)),
                            BinOp::Lt => Ok(Value::Bool(x < y)),
                            BinOp::Le => Ok(Value::Bool(x <= y)),
                            BinOp::Gt => Ok(Value::Bool(x > y)),
                            BinOp::Ge => Ok(Value::Bool(x >= y)),
                            _ => {
                                let value = consteval::int_binary(op, it, x, y)
                                    .map_err(|t| self.tree_trap(host, &t))?;
                                Ok(Value::Int { ty, value })
                            }
                        }
                    }
                    (Value::Bool(x), Value::Bool(y)) => match op {
                        BinOp::Eq => Ok(Value::Bool(x == y)),
                        BinOp::Ne => Ok(Value::Bool(x != y)),
                        BinOp::And => Ok(Value::Bool(x && y)),
                        BinOp::Or => Ok(Value::Bool(x || y)),
                        _ => Err(self.inconsistent(host, "an invalid boolean constant operation")),
                    },
                    _ => Err(self.inconsistent(host, "mismatched constant operands")),
                }
            }
            ConstExpr::Cast { to, expr } => match self.const_expr_value(host, fi, expr)? {
                Value::Int { ty, value } => {
                    let from = match host.declarations().types.kind(ty) {
                        TyKind::Int(i) => *i,
                        _ => return Err(self.inconsistent(host, "a non-integer constant")),
                    };
                    if !consteval::lossless(from, to) {
                        return Err(Fail::NotImplemented(
                            "constant evaluation of a lossy integer cast".to_string(),
                        ));
                    }
                    let ty = host.declarations_mut().types.int(to);
                    Ok(Value::Int { ty, value })
                }
                Value::Bool(b) => {
                    let ty = host.declarations_mut().types.int(to);
                    Ok(Value::Int {
                        ty,
                        value: i128::from(b),
                    })
                }
                _ => Err(self.inconsistent(host, "an invalid constant cast")),
            },
            ConstExpr::Layout { size, ty } => {
                let ty = self.subst_ty(host, ty);
                let Some(layout) = host.layout(ty) else {
                    return Err(Fail::NotImplemented(
                        "constant evaluation of `@sizeof` of a type with no layout".to_string(),
                    ));
                };
                let ty = self.i64_ty(host);
                Ok(Value::Int {
                    ty,
                    value: i128::from(if size { layout.size } else { layout.align }),
                })
            }
        }
    }

    fn tree_trap(&self, host: &mut dyn Host, trap: &ingot_sema::consteval::Trap) -> Fail {
        host.report(
            "E0806",
            format!(
                "constant evaluation trapped: {}: {}",
                trap.code(),
                trap.text()
            ),
            self.at.0,
            self.at.1,
            self.cur_label(),
        );
        Fail::Reported
    }

    fn operands(
        &mut self,
        host: &mut dyn Host,
        fi: usize,
        ops: &[Operand],
    ) -> Result<Vec<Value>, Fail> {
        let mut out = Vec::with_capacity(ops.len());
        for op in ops {
            out.push(self.operand(host, fi, op)?);
        }
        Ok(out)
    }

    fn operand(&mut self, host: &mut dyn Host, fi: usize, op: &Operand) -> Result<Value, Fail> {
        match op {
            Operand::Const(c) => self.constant(host, fi, c),
            Operand::Place(p, use_) => {
                let v = self.read_place(host, fi, p)?;
                if matches!(*use_, PlaceUse::Move | PlaceUse::ForElem) {
                    self.clear_place(host, fi, p)?;
                }
                Ok(v)
            }
        }
    }

    fn constant(&mut self, host: &mut dyn Host, fi: usize, c: &Const) -> Result<Value, Fail> {
        match c {
            Const::Int { ty, value } => Ok(Value::Int {
                ty: *ty,
                value: *value,
            }),
            Const::Float { ty, value } => {
                let bits = match host.declarations().types.kind(*ty) {
                    TyKind::Float(FloatTy::F32) => consteval::f32_bits(*value as f32),
                    _ => value.to_bits(),
                };
                Ok(Value::Float { ty: *ty, bits })
            }
            Const::Bool(b) => Ok(Value::Bool(*b)),
            Const::Unit => Ok(Value::Unit),
            Const::Str(b) => Ok(Value::Str(b.clone())),
            Const::CStr(b) => Ok(Value::CStr(b.clone())),
            Const::Zero { ty, .. } => self.zero(host, *ty),
            Const::Uninit { .. } => Ok(Value::Uninit),
            Const::Param { def, .. } => {
                let arg = self.frames.get(fi).and_then(|f| f.subst.get(def)).cloned();
                match arg {
                    Some(GenericArg::Const(ConstArg::Value(v))) => Ok(value_of_const(host, v)),
                    _ => Err(Fail::NotImplemented(
                        "constant evaluation of a constant generic parameter with no value"
                            .to_string(),
                    )),
                }
            }
            Const::Expr { id, .. } => self.const_expr_value(host, fi, *id),
            Const::Named { def, .. } => self.named_const(host, *def),
            Const::Fn { fn_ref, name } => Ok(Value::Fn {
                fn_ref: fn_ref.clone(),
                name: name.clone(),
            }),
        }
    }

    /// The value of another `const`, evaluated once.
    fn named_const(&mut self, host: &mut dyn Host, def: DefId) -> Result<Value, Fail> {
        match host.const_lookup(def) {
            ConstLookup::Value(v) => Ok(v),
            ConstLookup::Scalar(v) => Ok(value_of_const(host, v)),
            ConstLookup::Failed(f) => Err(f),
            ConstLookup::NeedInitializer => {
                host.mark_const_in_progress(def);
                host.enter_const(def);
                self.context(host);
                let typed = host.typed_body(def);
                let body = match typed {
                    Some(body) => body,
                    None => {
                        host.leave_const();
                        if host.body_in_progress(def) {
                            return Err(self.cycle_body(host));
                        }
                        let what =
                            "constant evaluation of a constant whose initializer was not typed"
                                .to_string();
                        host.not_implemented(what.clone());
                        return Err(Fail::NotImplemented(what));
                    }
                };
                if body.failed {
                    host.leave_const();
                    return Err(Fail::Reported);
                }
                let idx = match self.lower_body(host, &body, true) {
                    Ok(idx) => idx,
                    Err(f) => {
                        host.leave_const();
                        return Err(f);
                    }
                };
                let result =
                    self.run_cfg(host, idx, Vec::new(), None, None, false, BTreeMap::new());
                host.leave_const();
                host.set_const_value(def, result.clone());
                result
            }
        }
    }

    fn zero(&mut self, host: &mut dyn Host, ty: TypeId) -> Result<Value, Fail> {
        let ty = self.subst_ty(host, ty);
        let kind = host.declarations().types.kind(ty).clone();
        match kind {
            TyKind::Int(_) => Ok(Value::Int { ty, value: 0 }),
            TyKind::Float(_) => Ok(Value::Float { ty, bits: 0 }),
            TyKind::Bool => Ok(Value::Bool(false)),
            TyKind::Void => Ok(Value::Unit),
            TyKind::Array(elem, len) => {
                let n = self.const_len(host, len)?;
                if is_scalar_type(host, elem) {
                    return Ok(Value::Pack {
                        ty: elem,
                        len: n as u64,
                        elems: None,
                        moved: BTreeSet::new(),
                    });
                }
                let mut fields = Vec::with_capacity(n);
                for _ in 0..n {
                    fields.push(self.zero(host, elem)?);
                }
                Ok(Value::Aggregate {
                    kind: ValueAgg::Array,
                    ty,
                    fields,
                })
            }
            TyKind::Tuple(elems) => {
                let mut fields = Vec::new();
                for e in elems {
                    fields.push(self.zero(host, e)?);
                }
                Ok(Value::Aggregate {
                    kind: ValueAgg::Tuple,
                    ty,
                    fields,
                })
            }
            TyKind::Adt(def, _) => {
                let fields = match host.declarations().items.get(&def) {
                    Some(ItemSig::Struct(s)) => s.fields.iter().map(|f| f.ty).collect::<Vec<_>>(),
                    _ => {
                        return Err(Fail::NotImplemented(
                            "constant evaluation of `@zero` of an enum".to_string(),
                        ));
                    }
                };
                let mut vals = Vec::new();
                for f in fields {
                    vals.push(self.zero(host, f)?);
                }
                Ok(Value::Aggregate {
                    kind: ValueAgg::Struct(def),
                    ty,
                    fields: vals,
                })
            }
            _ => Err(Fail::NotImplemented(
                "constant evaluation of `@zero` of a type that has no zero value".to_string(),
            )),
        }
    }

    fn const_len(&mut self, host: &mut dyn Host, len: ConstArg) -> Result<usize, Fail> {
        match len {
            ConstArg::Value(ConstValue::Int { value, .. }) if value >= 0 => Ok(value as usize),
            ConstArg::Expr(id) => {
                let fi = self.frames.len().saturating_sub(1);
                match self.const_expr_value(host, fi, id)? {
                    Value::Int { value, .. } if value >= 0 => Ok(value as usize),
                    _ => Err(Fail::NotImplemented(
                        "constant evaluation of an array length that is not an integer".to_string(),
                    )),
                }
            }
            ConstArg::Param(p) => {
                for f in self.frames.iter().rev() {
                    if let Some(GenericArg::Const(ConstArg::Value(ConstValue::Int {
                        value, ..
                    }))) = f.subst.get(&p)
                    {
                        return Ok(*value as usize);
                    }
                }
                Err(Fail::Generic)
            }
            _ => Err(Fail::NotImplemented(
                "constant evaluation of an array length that is not an integer".to_string(),
            )),
        }
    }

    // ------------------------------------------------------------- rvalues

    fn rvalue(
        &mut self,
        host: &mut dyn Host,
        fi: usize,
        rv: &Rvalue,
        dest_ty: Option<TypeId>,
    ) -> Result<Value, Fail> {
        match rv {
            Rvalue::Use(op) => self.operand(host, fi, op),
            Rvalue::Unary { op, operand } => {
                let v = self.operand(host, fi, operand)?;
                self.unary(host, *op, v)
            }
            Rvalue::Binary { op, lhs, rhs } => {
                let a = self.operand(host, fi, lhs)?;
                let b = self.operand(host, fi, rhs)?;
                self.binary(host, *op, a, b)
            }
            Rvalue::Borrow { place, .. } | Rvalue::Reborrow { place, .. } => {
                let (alloc, path) = self.resolve_place(host, fi, place)?;
                Ok(Value::Borrow { alloc, path })
            }
            Rvalue::Aggregate {
                kind, ty, fields, ..
            } => {
                let mut vals = Vec::with_capacity(fields.len());
                for f in fields {
                    vals.push(self.operand(host, fi, f)?);
                }
                match kind {
                    AggregateKind::Variant { index, .. } => {
                        let mut it = vals.into_iter();
                        let payload = it.next().map(Box::new);
                        Ok(Value::Variant {
                            ty: *ty,
                            index: *index,
                            payload,
                        })
                    }
                    AggregateKind::Struct(def) => Ok(Value::Aggregate {
                        kind: ValueAgg::Struct(*def),
                        ty: *ty,
                        fields: vals,
                    }),
                    AggregateKind::Tuple => Ok(Value::Aggregate {
                        kind: ValueAgg::Tuple,
                        ty: *ty,
                        fields: vals,
                    }),
                    AggregateKind::Array => {
                        if let Some(elem) = array_element(host, *ty)
                            && is_scalar_type(host, elem)
                        {
                            let mut raw = Vec::with_capacity(vals.len());
                            for v in &vals {
                                match scalar_raw(v) {
                                    Some(x) => raw.push(x),
                                    None => {
                                        return Err(self.inconsistent(
                                            host,
                                            "an array element that is not a scalar",
                                        ));
                                    }
                                }
                            }
                            let zero = raw.iter().all(|x| *x == 0);
                            return Ok(Value::Pack {
                                ty: elem,
                                len: vals.len() as u64,
                                elems: if zero {
                                    None
                                } else {
                                    Some(Box::new(PackElems::Ints(raw)))
                                },
                                moved: BTreeSet::new(),
                            });
                        }
                        Ok(Value::Aggregate {
                            kind: ValueAgg::Array,
                            ty: *ty,
                            fields: vals,
                        })
                    }
                    AggregateKind::Closure(e) => Ok(Value::Aggregate {
                        kind: ValueAgg::Closure(*e),
                        ty: *ty,
                        fields: vals,
                    }),
                }
            }
            Rvalue::Cast { kind, operand } => {
                let v = self.operand(host, fi, operand)?;
                self.cast(host, *kind, v, dest_ty)
            }
            Rvalue::DynCast { operand, .. } => self.operand(host, fi, operand),
            Rvalue::Discriminant(place) => {
                let v = self.read_place(host, fi, place)?;
                self.tag_of(host, v, dest_ty)
            }
            Rvalue::SliceLen(place) => {
                let v = self.read_place(host, fi, place)?;
                let len = match &v {
                    Value::Slice { len, .. } => *len,
                    Value::Str(bytes) => bytes.len() as u64,
                    Value::Pack { len, .. } => *len,
                    Value::Aggregate {
                        kind: ValueAgg::Array,
                        fields,
                        ..
                    } => fields.len() as u64,
                    _ => {
                        return Err(self.inconsistent(
                            host,
                            "the length of a value that is not a slice or an array",
                        ));
                    }
                };
                Ok(Value::Int {
                    ty: dest_ty.unwrap_or(self.i64_ty(host)),
                    value: i128::from(len),
                })
            }
            Rvalue::SlicePtr(_) => Err(Fail::NotImplemented(
                "constant evaluation of a slice's pointer".to_string(),
            )),
            Rvalue::Slice {
                base,
                lo,
                hi,
                borrow,
                ..
            } => self.slice(host, fi, base, lo, hi, borrow.is_some()),
            Rvalue::SliceElem { base, index } => {
                let (alloc, path) = self.resolve_place(host, fi, base)?;
                let v = self.read_at(host, alloc, &path)?;
                let i = self.int_operand(host, fi, index)?;
                let Value::Slice { source, lo, .. } = v else {
                    return Err(
                        self.inconsistent(host, "an element of a value that is not a slice")
                    );
                };
                let j = lo as usize + i as usize;
                match source {
                    SliceSource::Alloc { alloc, path } => {
                        let mut p = path;
                        p.push(PathComp::Field(j));
                        Ok(Value::Borrow { alloc, path: p })
                    }
                    SliceSource::Str(bytes) => Ok(Value::StrPtr {
                        bytes,
                        at: j as u64,
                    }),
                }
            }
            Rvalue::Builtin {
                name,
                type_arg,
                args,
                ..
            } => self.builtin(host, fi, name, *type_arg, args, dest_ty),
        }
    }

    fn i64_ty(&mut self, host: &mut dyn Host) -> TypeId {
        host.declarations_mut().types.int(IntTy::I64)
    }

    fn int_operand(&mut self, host: &mut dyn Host, fi: usize, op: &Operand) -> Result<i128, Fail> {
        match self.operand(host, fi, op)? {
            Value::Int { value, .. } => Ok(value),
            _ => Err(self.inconsistent(host, "a value that is not an integer")),
        }
    }

    fn slice(
        &mut self,
        host: &mut dyn Host,
        fi: usize,
        base: &Place,
        lo: &Operand,
        hi: &Operand,
        array_borrow: bool,
    ) -> Result<Value, Fail> {
        let lo = self.int_operand(host, fi, lo)?;
        let hi = self.int_operand(host, fi, hi)?;
        let (alloc, path) = self.resolve_place(host, fi, base)?;
        if array_borrow {
            return Ok(Value::Slice {
                source: SliceSource::Alloc { alloc, path },
                lo: lo as u64,
                len: (hi - lo) as u64,
            });
        }
        let v = self.read_at(host, alloc, &path)?;
        match v {
            Value::Slice {
                source,
                lo: base_lo,
                ..
            } => Ok(Value::Slice {
                source,
                lo: base_lo + lo as u64,
                len: (hi - lo) as u64,
            }),
            Value::Str(bytes) => Ok(Value::Slice {
                source: SliceSource::Str(bytes),
                lo: lo as u64,
                len: (hi - lo) as u64,
            }),
            Value::Aggregate {
                kind: ValueAgg::Array,
                ..
            }
            | Value::Pack { .. } => Ok(Value::Slice {
                source: SliceSource::Alloc { alloc, path },
                lo: lo as u64,
                len: (hi - lo) as u64,
            }),
            _ => Err(self.inconsistent(host, "a slice of a value that is not a slice")),
        }
    }

    fn unary(&mut self, host: &mut dyn Host, op: UnOp, v: Value) -> Result<Value, Fail> {
        match v {
            Value::Int { ty, value } => {
                let it = match host.declarations().types.kind(ty) {
                    TyKind::Int(i) => *i,
                    _ => {
                        return Err(self
                            .inconsistent(host, "an integer operator on a value of another type"));
                    }
                };
                match consteval::int_unary(op, it, value) {
                    Ok(value) => Ok(Value::Int { ty, value }),
                    Err(t) => Err(self.trap(host, &t)),
                }
            }
            Value::Float { ty, bits } => match op {
                UnOp::Neg | UnOp::NegWrap => {
                    let v = self.float_of(host, ty, bits);
                    Ok(Value::Float {
                        ty,
                        bits: self.float_bits(host, ty, -v),
                    })
                }
                _ => Err(self.inconsistent(host, "a floating-point operator that is not `-`")),
            },
            Value::Bool(b) if op == UnOp::Not => Ok(Value::Bool(!b)),
            _ => Err(self.inconsistent(host, "an operator on an unsupported value")),
        }
    }

    fn binary(
        &mut self,
        host: &mut dyn Host,
        op: BinOp,
        a: Value,
        b: Value,
    ) -> Result<Value, Fail> {
        match (&a, &b) {
            (Value::Int { ty, value: x }, Value::Int { value: y, .. }) => {
                let it = match host.declarations().types.kind(*ty) {
                    TyKind::Int(i) => *i,
                    _ => {
                        return Err(self
                            .inconsistent(host, "an integer operator on a value of another type"));
                    }
                };
                if op.is_comparison() {
                    let r = match op {
                        BinOp::Eq => x == y,
                        BinOp::Ne => x != y,
                        BinOp::Lt => x < y,
                        BinOp::Le => x <= y,
                        BinOp::Gt => x > y,
                        _ => x >= y,
                    };
                    return Ok(Value::Bool(r));
                }
                match consteval::int_binary(op, it, *x, *y) {
                    Ok(value) => Ok(Value::Int { ty: *ty, value }),
                    Err(t) => Err(self.trap(host, &t)),
                }
            }
            (Value::Float { ty, bits: x }, Value::Float { bits: y, .. }) => {
                let ft = match host.declarations().types.kind(*ty) {
                    TyKind::Float(f) => *f,
                    _ => {
                        return Err(
                            self.inconsistent(host, "a float operator on a value of another type")
                        );
                    }
                };
                let xv = self.float_of(host, *ty, *x);
                let yv = self.float_of(host, *ty, *y);
                if op.is_comparison() {
                    let r = match op {
                        BinOp::Eq => xv == yv,
                        BinOp::Ne => xv != yv,
                        BinOp::Lt => xv < yv,
                        BinOp::Le => xv <= yv,
                        BinOp::Gt => xv > yv,
                        _ => xv >= yv,
                    };
                    return Ok(Value::Bool(r));
                }
                let r = consteval::float_binary(op, ft, xv, yv);
                Ok(Value::Float {
                    ty: *ty,
                    bits: self.float_bits(host, *ty, r),
                })
            }
            (Value::Bool(x), Value::Bool(y)) if matches!(op, BinOp::Eq | BinOp::Ne) => {
                Ok(Value::Bool(if op == BinOp::Eq { x == y } else { x != y }))
            }
            _ => Err(self.inconsistent(host, "an operator on values of different kinds")),
        }
    }

    fn cast(
        &mut self,
        host: &mut dyn Host,
        kind: CastKind,
        v: Value,
        dest_ty: Option<TypeId>,
    ) -> Result<Value, Fail> {
        match kind {
            CastKind::Identity => Ok(v),
            CastKind::IntWiden => match v {
                Value::Int { ty, value } => Ok(Value::Int {
                    ty: dest_ty.unwrap_or(ty),
                    value,
                }),
                _ => Err(self.inconsistent(host, "an integer conversion of a non-integer")),
            },
            CastKind::BoolToInt => match v {
                Value::Bool(b) => Ok(Value::Int {
                    ty: dest_ty.unwrap_or_else(|| self.i64_ty(host)),
                    value: i128::from(b),
                }),
                _ => Err(self.inconsistent(host, "a bool conversion of a value that is not bool")),
            },
            CastKind::EnumToTag => self.tag_of(host, v, dest_ty),
            CastKind::ToFloat => match v {
                Value::Int { value, .. } => {
                    let ty = dest_ty.ok_or_else(|| {
                        Fail::NotImplemented(
                            "constant evaluation of a conversion to a float with no type"
                                .to_string(),
                        )
                    })?;
                    Ok(Value::Float {
                        ty,
                        bits: self.float_bits(host, ty, value as f64),
                    })
                }
                Value::Float { ty: from, bits } => {
                    let ty = dest_ty.ok_or_else(|| {
                        Fail::NotImplemented(
                            "constant evaluation of a conversion to a float with no type"
                                .to_string(),
                        )
                    })?;
                    let value = self.float_of(host, from, bits);
                    Ok(Value::Float {
                        ty,
                        bits: self.float_bits(host, ty, value),
                    })
                }
                _ => Err(self.inconsistent(host, "a float conversion of an unsupported value")),
            },
            CastKind::RefToPtr | CastKind::PtrToPtr => Ok(v),
            // An address is not a constant value in the evaluated subset yet.
            CastKind::PtrToInt | CastKind::IntToPtr => Err(Fail::NotImplemented(
                "constant evaluation of a raw pointer's address".to_string(),
            )),
        }
    }

    fn tag_of(
        &mut self,
        host: &mut dyn Host,
        v: Value,
        dest_ty: Option<TypeId>,
    ) -> Result<Value, Fail> {
        match v {
            Value::Variant { ty, index, .. } => {
                let tag = variant_tag(host, ty, index).unwrap_or(index as i128);
                Ok(Value::Int {
                    ty: dest_ty.unwrap_or_else(|| self.i64_ty(host)),
                    value: tag,
                })
            }
            Value::Int { .. } => Ok(v),
            _ => Err(self.inconsistent(host, "the tag of a value that is not an enum")),
        }
    }

    fn float_of(&self, host: &mut dyn Host, ty: TypeId, bits: u64) -> f64 {
        match host.declarations().types.kind(ty) {
            TyKind::Float(FloatTy::F32) => f64::from(consteval::f32_from_bits(bits)),
            _ => f64::from_bits(bits),
        }
    }

    fn float_bits(&self, host: &mut dyn Host, ty: TypeId, v: f64) -> u64 {
        match host.declarations().types.kind(ty) {
            TyKind::Float(FloatTy::F32) => consteval::f32_bits(v as f32),
            _ => v.to_bits(),
        }
    }

    // -------------------------------------------------------------- checks

    fn check(&mut self, host: &mut dyn Host, fi: usize, check: &Check) -> Result<(), Fail> {
        match check.kind {
            CheckKind::Bounds(BoundsKind::Index) => {
                let (Some(p), Some(lo)) = (&check.place, &check.lo) else {
                    return Err(Fail::NotImplemented(
                        "a bounds check without a container or index".to_string(),
                    ));
                };
                let len = self.len_of(host, fi, p)?;
                let i = self.int_operand(host, fi, lo)?;
                if i < 0 || i >= len as i128 {
                    let text = format!("index {i} out of bounds for length {len}");
                    return Err(self.check_trap(host, Trap::T01, text));
                }
                Ok(())
            }
            CheckKind::Bounds(BoundsKind::Slice) => {
                let (Some(p), Some(lo), Some(hi)) = (&check.place, &check.lo, &check.hi) else {
                    return Err(Fail::NotImplemented(
                        "a slice check without a container and a range".to_string(),
                    ));
                };
                let len = self.len_of(host, fi, p)?;
                let lo = self.int_operand(host, fi, lo)?;
                let hi = self.int_operand(host, fi, hi)?;
                if lo < 0 || hi < lo || hi > len as i128 {
                    let text = format!("slice {lo}..{hi} out of bounds for length {len}");
                    return Err(self.check_trap(host, Trap::T02, text));
                }
                Ok(())
            }
            CheckKind::Bounds(BoundsKind::Vector { width, tail }) => {
                let (Some(p), Some(lo)) = (&check.place, &check.lo) else {
                    return Err(Fail::NotImplemented(
                        "a vector check without a container or index".to_string(),
                    ));
                };
                let len = self.len_of(host, fi, p)?;
                let i = self.int_operand(host, fi, lo)?;
                let fits = if tail {
                    i >= 0 && (i as u64) <= len
                } else {
                    i >= 0 && (i as u64) + u64::from(width) <= len
                };
                if !fits {
                    let text = format!("vector access at {i} out of bounds for length {len}");
                    return Err(self.check_trap(host, Trap::T14, text));
                }
                Ok(())
            }
            CheckKind::Overflow => {
                let (op, a, b) = self.arith_operands(host, fi, check)?;
                let Some(ty) = check.ty else {
                    return Err(Fail::NotImplemented(
                        "an overflow check with no type".to_string(),
                    ));
                };
                let it = match host.declarations().types.kind(ty) {
                    TyKind::Int(i) => *i,
                    _ => return Ok(()),
                };
                let r = if let Some(b) = b {
                    consteval::int_binary(op_binop(&op), it, a, b)
                } else {
                    consteval::int_unary(UnOp::Neg, it, a)
                };
                if let Err(t) = r {
                    return Err(self.trap(host, &t));
                }
                Ok(())
            }
            CheckKind::Division | CheckKind::Shift => {
                let (_, a, b) = self.arith_operands(host, fi, check)?;
                let (Some(b), Some(ty)) = (b, check.ty) else {
                    return Ok(());
                };
                let it = match host.declarations().types.kind(ty) {
                    TyKind::Int(i) => *i,
                    _ => return Ok(()),
                };
                let op = if check.kind == CheckKind::Division {
                    BinOp::Div
                } else {
                    BinOp::Shl
                };
                if let Err(t) = consteval::int_binary(op, it, a, b) {
                    return Err(self.trap(host, &t));
                }
                Ok(())
            }
            CheckKind::Narrow => {
                let Some(op) = check.operands.first() else {
                    return Ok(());
                };
                let v = self.int_operand(host, fi, op)?;
                let Some(ty) = check.ty else {
                    return Ok(());
                };
                if let TyKind::Int(i) = host.declarations().types.kind(ty)
                    && !i.contains(v)
                {
                    let name = check.ty_name.clone().unwrap_or_else(|| host.type_name(ty));
                    let text = format!("value does not fit in {name}");
                    return Err(self.check_trap(host, Trap::T07, text));
                }
                Ok(())
            }
            CheckKind::Gather | CheckKind::ScatterDistinct | CheckKind::DispatchCover => Err(
                Fail::NotImplemented("constant evaluation of a vector access check".to_string()),
            ),
        }
    }

    fn arith_operands(
        &mut self,
        host: &mut dyn Host,
        fi: usize,
        check: &Check,
    ) -> Result<(String, i128, Option<i128>), Fail> {
        let mut vals = Vec::new();
        for op in &check.operands {
            vals.push(self.int_operand(host, fi, op)?);
        }
        let op = check.op.clone().unwrap_or_else(|| "+".to_string());
        match vals.as_slice() {
            [a] => Ok((op, *a, None)),
            [a, b] => Ok((op, *a, Some(*b))),
            _ => Err(Fail::NotImplemented(
                "constant evaluation of an arithmetic check with no operands".to_string(),
            )),
        }
    }

    fn len_of(&mut self, host: &mut dyn Host, fi: usize, place: &Place) -> Result<u64, Fail> {
        let v = self.read_place(host, fi, place)?;
        match v {
            Value::Slice { len, .. } => Ok(len),
            Value::Str(bytes) => Ok(bytes.len() as u64),
            Value::Aggregate {
                kind: ValueAgg::Array | ValueAgg::Tuple,
                fields,
                ..
            } => Ok(fields.len() as u64),
            Value::Pack { len, .. } => Ok(len),
            _ => Err(self.inconsistent(host, "the length of a value that is not a slice or array")),
        }
    }

    fn check_trap(&mut self, host: &mut dyn Host, code: Trap, text: String) -> Fail {
        let label = self.cur_label();
        host.report(
            "E0806",
            format!("constant evaluation trapped: {}: {text}", code.code()),
            self.at.0,
            self.at.1,
            label,
        );
        Fail::Reported
    }

    fn trap(&mut self, host: &mut dyn Host, trap: &SeTrap) -> Fail {
        let label = self.cur_label();
        host.report(
            "E0806",
            format!(
                "constant evaluation trapped: {}: {}",
                trap.code(),
                trap.text()
            ),
            self.at.0,
            self.at.1,
            label,
        );
        Fail::Reported
    }

    // ------------------------------------------------------------- builtins

    fn builtin(
        &mut self,
        host: &mut dyn Host,
        fi: usize,
        name: &str,
        type_arg: Option<TypeId>,
        args: &[BuiltinArg],
        dest_ty: Option<TypeId>,
    ) -> Result<Value, Fail> {
        match name {
            "sizeof" | "alignof" => {
                let Some(ty) = type_arg.map(|t| self.subst_ty(host, t)) else {
                    return Err(Fail::NotImplemented(format!(
                        "constant evaluation of `@{name}` without a type"
                    )));
                };
                let Some(l) = host.layout(ty) else {
                    return Err(Fail::NotImplemented(format!(
                        "constant evaluation of `@{name}` of a type with no layout"
                    )));
                };
                Ok(Value::Int {
                    ty: dest_ty.unwrap_or_else(|| self.i64_ty(host)),
                    value: i128::from(if name == "sizeof" { l.size } else { l.align }),
                })
            }
            "lanes" => {
                let Some(ty) = type_arg.map(|t| self.subst_ty(host, t)) else {
                    return Err(Fail::NotImplemented(
                        "constant evaluation of `@lanes` without a type".to_string(),
                    ));
                };
                let n = match host.declarations().types.kind(ty) {
                    TyKind::Vector(_, n) | TyKind::Mask(n) => *n as i128,
                    _ => {
                        return Err(Fail::NotImplemented(
                            "constant evaluation of `@lanes` of a type that is not a vector"
                                .to_string(),
                        ));
                    }
                };
                Ok(Value::Int {
                    ty: dest_ty.unwrap_or_else(|| self.i64_ty(host)),
                    value: n,
                })
            }
            "narrow" => {
                let x = self.value_arg(host, fi, args.first())?;
                let ty = type_arg.map(|t| self.subst_ty(host, t)).ok_or_else(|| {
                    Fail::NotImplemented(
                        "constant evaluation of `@narrow` without a type".to_string(),
                    )
                })?;
                match (x, host.declarations().types.kind(ty).clone()) {
                    (Value::Int { value, .. }, TyKind::Int(_)) => Ok(Value::Int { ty, value }),
                    (Value::Int { value, .. }, TyKind::Float(_)) => Ok(Value::Float {
                        ty,
                        bits: self.float_bits(host, ty, value as f64),
                    }),
                    (Value::Float { ty: from, bits }, TyKind::Float(_)) => {
                        let value = self.float_of(host, from, bits);
                        Ok(Value::Float {
                            ty,
                            bits: self.float_bits(host, ty, value),
                        })
                    }
                    (Value::Bool(b), TyKind::Int(_)) => Ok(Value::Int {
                        ty,
                        value: i128::from(b),
                    }),
                    _ => Err(self.inconsistent(host, "`@narrow` of a value that does not fit")),
                }
            }
            "abs" => {
                let v = self.value_arg(host, fi, args.first())?;
                match v {
                    Value::Int { ty, value } => {
                        let it = match host.declarations().types.kind(ty) {
                            TyKind::Int(i) => *i,
                            _ => {
                                return Err(self.inconsistent(host, "`@abs` of a non-integer"));
                            }
                        };
                        let value = if value < 0 {
                            match consteval::int_unary(UnOp::Neg, it, value) {
                                Ok(v) => v,
                                Err(t) => return Err(self.trap(host, &t)),
                            }
                        } else {
                            value
                        };
                        Ok(Value::Int { ty, value })
                    }
                    Value::Float { ty, bits } => {
                        let v = self.float_of(host, ty, bits).abs();
                        Ok(Value::Float {
                            ty,
                            bits: self.float_bits(host, ty, v),
                        })
                    }
                    _ => Err(self.inconsistent(host, "`@abs` of an unsupported value")),
                }
            }
            "sqrt" | "floor" | "ceil" | "round" => {
                let v = self.value_arg(host, fi, args.first())?;
                let Value::Float { ty, bits } = v else {
                    return Err(self.inconsistent(host, "a float builtin of a non-float"));
                };
                let x = self.float_of(host, ty, bits);
                let r = match name {
                    "sqrt" => x.sqrt(),
                    "floor" => x.floor(),
                    "ceil" => x.ceil(),
                    _ => x.round(),
                };
                Ok(Value::Float {
                    ty,
                    bits: self.float_bits(host, ty, r),
                })
            }
            "fma" => {
                let a = self.value_arg(host, fi, args.first())?;
                let b = self.value_arg(host, fi, args.get(1))?;
                let c = self.value_arg(host, fi, args.get(2))?;
                let (
                    Value::Float { ty, bits },
                    Value::Float { bits: bb, .. },
                    Value::Float { bits: cc, .. },
                ) = (a, b, c)
                else {
                    return Err(self.inconsistent(host, "`@fma` of non-floats"));
                };
                let x = self.float_of(host, ty, bits);
                let y = self.float_of(host, ty, bb);
                let z = self.float_of(host, ty, cc);
                Ok(Value::Float {
                    ty,
                    bits: self.float_bits(host, ty, x.mul_add(y, z)),
                })
            }
            "min" | "max" => {
                let a = self.value_arg(host, fi, args.first())?;
                let b = self.value_arg(host, fi, args.get(1))?;
                match (a, b) {
                    (Value::Int { ty, value: x }, Value::Int { value: y, .. }) => Ok(Value::Int {
                        ty,
                        value: if name == "min" { x.min(y) } else { x.max(y) },
                    }),
                    (Value::Float { ty, bits: x }, Value::Float { bits: y, .. }) => {
                        let x = self.float_of(host, ty, x);
                        let y = self.float_of(host, ty, y);
                        let r = if name == "min" { x.min(y) } else { x.max(y) };
                        Ok(Value::Float {
                            ty,
                            bits: self.float_bits(host, ty, r),
                        })
                    }
                    _ => Err(self.inconsistent(host, "`@min`/`@max` of mixed values")),
                }
            }
            "forget" | "debug" | "static_assert" => {
                for a in args {
                    let _ = self.builtin_arg(host, fi, a)?;
                }
                Ok(Value::Unit)
            }
            "replace" => {
                let Some(BuiltinArg::Place(p)) = args.first() else {
                    return Err(Fail::NotImplemented(
                        "constant evaluation of `@replace` without a place".to_string(),
                    ));
                };
                let v = self.value_arg(host, fi, args.get(1))?;
                let old = self.read_place(host, fi, p)?;
                let (alloc, path) = self.resolve_place(host, fi, p)?;
                self.write_at(host, alloc, &path, v)?;
                Ok(old)
            }
            "swap" => {
                let (Some(BuiltinArg::Place(a)), Some(BuiltinArg::Place(b))) =
                    (args.first(), args.get(1))
                else {
                    return Err(Fail::NotImplemented(
                        "constant evaluation of `@swap` without two places".to_string(),
                    ));
                };
                let va = self.read_place(host, fi, a)?;
                let vb = self.read_place(host, fi, b)?;
                let (aa, pa) = self.resolve_place(host, fi, a)?;
                let (ab, pb) = self.resolve_place(host, fi, b)?;
                self.write_at(host, aa, &pa, vb)?;
                self.write_at(host, ab, &pb, va)?;
                Ok(Value::Unit)
            }
            "unchecked_div" | "unchecked_neg" | "unchecked_shr" => {
                let a = self.value_arg(host, fi, args.first())?;
                let Value::Int { ty, value } = a else {
                    return Err(
                        self.inconsistent(host, "an unchecked integer builtin of a non-integer")
                    );
                };
                let it = match host.declarations().types.kind(ty) {
                    TyKind::Int(i) => *i,
                    _ => {
                        return Err(self
                            .inconsistent(host, "an unchecked integer builtin of a non-integer"));
                    }
                };
                if name == "unchecked_neg" {
                    let value = consteval::wrap(it, -value);
                    return Ok(Value::Int { ty, value });
                }
                let b = match args.get(1) {
                    Some(BuiltinArg::Value(op)) => self.int_operand(host, fi, op)?,
                    _ => {
                        return Err(Fail::NotImplemented(
                            "constant evaluation of an unchecked builtin without an argument"
                                .to_string(),
                        ));
                    }
                };
                let op = if name == "unchecked_div" {
                    BinOp::Div
                } else {
                    BinOp::Shr
                };
                match consteval::int_binary(op, it, value, b) {
                    Ok(value) => Ok(Value::Int { ty, value }),
                    Err(t) => Err(self.trap(host, &t)),
                }
            }
            _ => Err(Fail::NotImplemented(format!(
                "constant evaluation of `@{name}`"
            ))),
        }
    }

    fn builtin_arg(
        &mut self,
        host: &mut dyn Host,
        fi: usize,
        arg: &BuiltinArg,
    ) -> Result<Value, Fail> {
        match arg {
            BuiltinArg::Value(op) => self.operand(host, fi, op),
            BuiltinArg::Place(p) => self.read_place(host, fi, p),
        }
    }

    fn value_arg(
        &mut self,
        host: &mut dyn Host,
        fi: usize,
        arg: Option<&BuiltinArg>,
    ) -> Result<Value, Fail> {
        match arg {
            Some(a) => self.builtin_arg(host, fi, a),
            None => Err(Fail::NotImplemented(
                "constant evaluation of a builtin without an argument".to_string(),
            )),
        }
    }

    // ------------------------------------------------------------ diagnostics

    fn no_value(&self) -> Fail {
        Fail::NotImplemented(
            "constant evaluation of a read of storage that holds no value (phase 9 will \
             report the read)"
                .to_string(),
        )
    }

    /// An operation outside the constant-expression rules (E0807): one of pipeline
    /// section 4.6's five.
    fn e0807(&self, host: &mut dyn Host, what: &str) -> Fail {
        host.report(
            "E0807",
            what.to_string(),
            self.at.0,
            self.at.1,
            self.cur_label(),
        );
        Fail::Reported
    }

    /// A mismatch between lowering and the interpreter. It is a lowering bug, not a
    /// user error: evaluation stops with a `constant evaluation of ...` entry and no
    /// diagnostic.
    fn inconsistent(&self, _host: &mut dyn Host, what: &str) -> Fail {
        Fail::NotImplemented(format!("constant evaluation of {what}"))
    }
}

/// The statements that need no clone to execute.
enum Cheap {
    Start(Local),
    End(Local),
    Nothing,
    Unsafe,
}

enum Step {
    Next(Block),
    Returned(Value),
    /// A call pushed the callee; the caller resumes when it returns.
    Pushed,
}

const E0807_STATIC: &str = "a constant expression reads a `static`'s value";
const E0807_UNSAFE: &str = "a constant expression enters `unsafe`";
const E0807_EXTERN: &str = "a constant expression calls an `extern` function";
const E0807_CONCURRENCY: &str =
    "a constant expression runs `spawn`, `par for`, `select` or `dispatch`";

impl Interpreter for Evaluator<'_> {
    fn eval_initializer(
        &mut self,
        host: &mut dyn Host,
        def: DefId,
        file: usize,
        span: Span,
    ) -> Result<Value, Fail> {
        self.reset((file, span));
        self.root_def = Some(def);
        host.mark_const_in_progress(def);
        let r = self.cfg_of(host, def).and_then(|idx| {
            self.run_cfg(host, idx, Vec::new(), None, None, false, BTreeMap::new())
        });
        self.frames.clear();
        host.set_const_value(def, r.clone());
        r
    }

    fn eval_expr(
        &mut self,
        host: &mut dyn Host,
        body: &TypedBody,
        expr: ingot_sema::hir::ExprId,
        ty: TypeId,
        file: usize,
        span: Span,
    ) -> Result<Value, Fail> {
        self.eval_expr_in(host, body, expr, ty, file, span, BTreeMap::new())
    }

    fn eval_expr_with_subst(
        &mut self,
        host: &mut dyn Host,
        body: &TypedBody,
        expr: ingot_sema::hir::ExprId,
        ty: TypeId,
        file: usize,
        span: Span,
        subst: Subst,
    ) -> Result<Value, Fail> {
        self.eval_expr_in(host, body, expr, ty, file, span, subst.map)
    }
}

impl Evaluator<'_> {
    #[allow(clippy::too_many_arguments)]
    fn eval_expr_in(
        &mut self,
        host: &mut dyn Host,
        body: &TypedBody,
        expr: ingot_sema::hir::ExprId,
        ty: TypeId,
        file: usize,
        span: Span,
        subst: BTreeMap<DefId, GenericArg>,
    ) -> Result<Value, Fail> {
        self.reset((file, span));
        self.root_def = None;
        // The expression is the root of its own CFG, with the caller's type; the
        // enclosing body's statement list is not run.
        let mut root = body.clone();
        root.root = BodyRoot::Expr(expr);
        root.ret = ty;
        root.params.clear();
        root.error = None;
        let r = self
            .lower_body(host, &root, false)
            .and_then(|idx| self.run_cfg(host, idx, Vec::new(), None, None, false, subst));
        self.frames.clear();
        r
    }
}

// ---------------------------------------------------------------- helpers

/// One element of a packed array: a zero when nothing was written, otherwise the raw
/// bits of the element, read as the element's type.
fn pack_element(host: &mut dyn Host, ty: TypeId, elems: Option<&PackElems>, j: usize) -> Value {
    let raw = match elems {
        None => 0,
        Some(PackElems::Ints(values)) => values.get(j).copied().unwrap_or(0),
    };
    match host.declarations().types.kind(ty) {
        TyKind::Int(_) => Value::Int { ty, value: raw },
        TyKind::Bool => Value::Bool(raw != 0),
        TyKind::Float(_) => Value::Float {
            ty,
            bits: raw as u64,
        },
        _ => Value::Uninit,
    }
}

/// The raw bits a scalar value packs as (`bool` 0/1, a float's bits).
fn scalar_raw(value: &Value) -> Option<i128> {
    match value {
        Value::Int { value, .. } => Some(*value),
        Value::Bool(b) => Some(i128::from(*b)),
        Value::Float { bits, .. } => Some(i128::from(*bits)),
        _ => None,
    }
}

fn is_scalar_type(host: &mut dyn Host, ty: TypeId) -> bool {
    matches!(
        host.declarations().types.kind(ty),
        TyKind::Int(_) | TyKind::Bool | TyKind::Float(_)
    )
}

fn array_element(host: &mut dyn Host, ty: TypeId) -> Option<TypeId> {
    match host.declarations().types.kind(ty) {
        TyKind::Array(elem, _) => Some(*elem),
        _ => None,
    }
}

fn byte_value(host: &mut dyn Host, b: u8) -> Value {
    Value::Int {
        ty: host.declarations_mut().types.int(IntTy::U8),
        value: i128::from(b),
    }
}

fn value_of_const(host: &mut dyn Host, v: ConstValue) -> Value {
    match v {
        ConstValue::Int { ty, value } => Value::Int {
            ty: host.declarations_mut().types.int(ty),
            value,
        },
        ConstValue::Bool(b) => Value::Bool(b),
    }
}

fn intern(host: &mut dyn Host, kind: TyKind) -> TypeId {
    host.declarations_mut().types.intern(kind)
}

fn subst_const_arg(arg: ConstArg, subst: &BTreeMap<DefId, GenericArg>) -> ConstArg {
    match arg {
        ConstArg::Param(p) => match subst.get(&p) {
            Some(GenericArg::Const(c)) => *c,
            _ => ConstArg::Param(p),
        },
        other => other,
    }
}

fn subst_arg(
    host: &mut dyn Host,
    arg: GenericArg,
    subst: &BTreeMap<DefId, GenericArg>,
    ev: &mut Evaluator,
) -> GenericArg {
    match arg {
        GenericArg::Ty(t) => GenericArg::Ty(ev.subst_with(host, t, subst)),
        GenericArg::Const(c) => GenericArg::Const(subst_const_arg(c, subst)),
    }
}

fn variant_tag(host: &mut dyn Host, ty: TypeId, index: usize) -> Option<i128> {
    match host.declarations().types.kind(ty) {
        TyKind::Optional(_) | TyKind::Result(..) => Some(index as i128),
        TyKind::Adt(d, _) => match host.declarations().items.get(d) {
            Some(ItemSig::Enum(e)) => e.variants.get(index).map(|v| v.tag),
            _ => None,
        },
        _ => None,
    }
}

fn op_binop(text: &str) -> BinOp {
    match text {
        "-" => BinOp::Sub,
        "*" => BinOp::Mul,
        "/" => BinOp::Div,
        "%" => BinOp::Rem,
        "<<" => BinOp::Shl,
        ">>" => BinOp::Shr,
        _ => BinOp::Add,
    }
}

fn trap_text(code: Trap, source: Option<&str>, values: &[Value]) -> String {
    let int = |i: usize| match values.get(i) {
        Some(Value::Int { value, .. }) => *value,
        _ => 0,
    };
    let text = |i: usize| match values.get(i) {
        Some(Value::Str(b)) | Some(Value::CStr(b)) => String::from_utf8_lossy(b).into_owned(),
        _ => String::new(),
    };
    match code {
        Trap::T03 => format!("integer overflow in {}", source.unwrap_or("+")),
        Trap::T04 => "division by zero".to_string(),
        Trap::T05 => "integer overflow in division".to_string(),
        Trap::T06 => "shift count out of range".to_string(),
        Trap::T07 => format!("value does not fit in {}", source.unwrap_or("the type")),
        Trap::T08 => match source {
            Some(cond) if values.is_empty() => format!("assertion failed: {cond}"),
            Some(cond) => format!("assertion failed: {cond}: {}", text(0)),
            None => "assertion failed".to_string(),
        },
        Trap::T09 => text(0),
        Trap::T10 => "entered unreachable code".to_string(),
        Trap::T01 => format!("index {} out of bounds for length {}", int(0), int(1)),
        Trap::T02 => format!(
            "slice {}..{} out of bounds for length {}",
            int(0),
            int(1),
            int(2)
        ),
        Trap::T14 => format!(
            "vector access at {} out of bounds for length {}",
            int(0),
            int(1)
        ),
        Trap::T15 => format!("scatter index {} occurs twice", int(0)),
        Trap::T16 => format!(
            "dispatch range {}..{} is outside its buffers",
            int(0),
            int(1)
        ),
        Trap::T17 => "blocking operation outside a task".to_string(),
    }
}

/// Store `value` at `path` inside a value tree.
fn set_in(v: &mut Value, path: &[PathComp], value: Value) -> Result<(), Fail> {
    if path.is_empty() {
        *v = value;
        return Ok(());
    }
    match (&mut *v, path[0]) {
        (Value::Aggregate { fields, .. }, PathComp::Field(i)) => match fields.get_mut(i) {
            Some(slot) => set_in(slot, &path[1..], value),
            None => Err(Fail::NotImplemented(
                "constant evaluation of a write outside a value".to_string(),
            )),
        },
        (Value::Variant { payload, .. }, PathComp::Payload(_)) => match payload {
            Some(p) => set_in(p, &path[1..], value),
            None => Err(Fail::NotImplemented(
                "constant evaluation of a write to a variant with no payload".to_string(),
            )),
        },
        (
            Value::Pack {
                ty,
                len,
                elems,
                moved,
            },
            PathComp::Field(i),
        ) => {
            let _ = ty;
            if i as u64 >= *len {
                return Err(Fail::NotImplemented(
                    "constant evaluation of a write outside an array".to_string(),
                ));
            }
            if matches!(value, Value::Uninit) {
                moved.insert(i as u32);
                return Ok(());
            }
            let Some(raw) = scalar_raw(&value) else {
                return Err(Fail::NotImplemented(
                    "constant evaluation of a write of a non-scalar into a packed array"
                        .to_string(),
                ));
            };
            moved.remove(&(i as u32));
            match elems {
                Some(boxed) => match &mut **boxed {
                    PackElems::Ints(values) => {
                        if let Some(slot) = values.get_mut(i) {
                            *slot = raw;
                        } else {
                            return Err(Fail::NotImplemented(
                                "constant evaluation of a write outside a packed array".to_string(),
                            ));
                        }
                    }
                },
                None => {
                    if raw == 0 {
                        return Ok(());
                    }
                    let mut values = vec![0i128; *len as usize];
                    values[i] = raw;
                    *elems = Some(Box::new(PackElems::Ints(values)));
                }
            }
            Ok(())
        }
        _ => Err(Fail::NotImplemented(
            "constant evaluation of a write into a value that is not an aggregate".to_string(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ingot_diag::source::Source;
    use ingot_sema::hir::Item;
    use ingot_sema::ty::ConstValue;
    use ingot_sema::value::Value;
    use ingot_sema::{
        ModuleInput, PackageInput, Target, check_bodies, check_declarations, resolve,
    };
    use ingot_syntax::lexer::lex;
    use ingot_syntax::parser::parse;

    struct Built {
        decls: ingot_sema::Declarations,
        out: ingot_sema::Output,
        phase6: Vec<ingot_diag::diagnostic::Diagnostic>,
        phase7: Vec<ingot_diag::diagnostic::Diagnostic>,
        typed: ingot_sema::typed::Bodies,
        factory: EvaluatorFactory,
    }

    impl Built {
        fn diagnostics(&self) -> impl Iterator<Item = &ingot_diag::diagnostic::Diagnostic> {
            self.phase6.iter().chain(self.phase7.iter())
        }
    }

    fn build(text: &str) -> Built {
        let mut sources = ingot_diag::source::Sources::default();
        let file = sources.add(Source::new("app.ingot", text));
        let source = sources.get(file);
        let (tokens, mut diags) = lex(source);
        let (ast, parse_diags) = parse(source, tokens);
        diags.extend(parse_diags);
        assert!(diags.is_empty(), "{diags:?}");
        let inputs = vec![PackageInput {
            name: "app".to_string(),
            deps: Vec::new(),
            modules: vec![ModuleInput {
                path: "app".to_string(),
                file,
                ast: Some(ast),
            }],
        }];
        let target = Target::by_name("aarch64-macos-m4").expect("a target");
        let out = resolve(&mut sources, inputs, &target);
        let factory = EvaluatorFactory::default();
        let decls = check_declarations(&sources, &out, &target, &[0], Some(&factory));
        let phase6: Vec<_> = decls.diagnostics.clone();
        let body_out = check_bodies(&sources, &out, decls, &target, &[0], Some(&factory));
        Built {
            decls: body_out.declarations,
            out,
            phase6,
            phase7: body_out.diagnostics,
            typed: body_out.bodies,
            factory,
        }
    }

    fn def(b: &Built, name: &str) -> DefId {
        b.out.packages[0]
            .items
            .keys()
            .copied()
            .find(|d| {
                b.out.sema.def(*d).name == name
                    && matches!(
                        b.out.packages[0].items.get(d),
                        Some(Item::Const(_) | Item::Static(_))
                    )
            })
            .unwrap_or_else(|| panic!("no constant `{name}`"))
    }

    fn value(b: &Built, name: &str) -> Value {
        match b.decls.values.get(&def(b, name)) {
            Some(ingot_sema::consteval::ValueState::Done(Ok(v))) => v.clone(),
            other => panic!("`{name}` has no value: {other:?}"),
        }
    }

    #[test]
    fn a_struct_constant_is_a_value_tree() {
        let b = build("struct P { x: i64 }\nconst p: P = P.{ x: 3 };\n");
        match value(&b, "p") {
            Value::Aggregate { fields, .. } => {
                assert!(matches!(fields[0], Value::Int { value: 3, .. }));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_called_function_is_interpreted() {
        let b =
            build("fn add(a: i64, b: i64) -> i64 { return a + b; }\nconst n: i64 = add(2, 3);\n");
        assert!(matches!(value(&b, "n"), Value::Int { value: 5, .. }));
    }

    #[test]
    fn floats_round_at_each_operation() {
        let b =
            build("fn lost() -> f32 { return (1.0 + 1.0e-8) - 1.0; }\nconst n: f32 = lost();\n");
        match value(&b, "n") {
            Value::Float { bits, .. } => assert_eq!(bits, 0),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn generic_arguments_are_substituted() {
        let b = build("fn size[T]() -> i64 { return @sizeof(T); }\nconst n: i64 = size.[i32]();\n");
        assert!(matches!(value(&b, "n"), Value::Int { value: 4, .. }));
    }

    #[test]
    fn a_trap_is_e0806_with_the_call_label() {
        let b =
            build("fn boom() -> i64 { return 9223372036854775807 + 1; }\nconst n: i64 = boom();\n");
        let d = b
            .diagnostics()
            .find(|d| d.code == "E0806")
            .unwrap_or_else(|| panic!("{:?}", b.diagnostics().collect::<Vec<_>>()));
        assert!(d.message.contains("T03"));
        assert!(d.related.iter().any(|r| r.role == "call"));
    }

    #[test]
    fn a_cycle_is_e0812_once() {
        let b = build("const a: i64 = b;\nconst b: i64 = a;\n");
        let cycles = b.diagnostics().filter(|d| d.code == "E0812").count();
        assert_eq!(cycles, 1, "{:?}", b.diagnostics().collect::<Vec<_>>());
    }

    #[test]
    fn a_pulled_forward_body_is_typed_once_and_reported_once() {
        let b = build("fn bad() -> i64 { return \"no\"; }\nconst n: i64 = bad();\n");
        // Phase 6 typed `bad` on demand; phase 7 reused it.
        assert!(
            b.decls.bodies.contains_key(
                &b.out.packages[0]
                    .items
                    .keys()
                    .copied()
                    .find(|d| {
                        b.out.sema.def(*d).name == "bad"
                            && matches!(b.out.packages[0].items.get(d), Some(Item::Fn(_)))
                    })
                    .unwrap()
            )
        );
        assert!(
            b.typed.bodies.contains_key(
                &b.decls
                    .bodies
                    .keys()
                    .find(|d| { b.out.sema.def(**d).name == "bad" })
                    .copied()
                    .unwrap()
            )
        );
        let count = b
            .diagnostics()
            .filter(|d| d.code.starts_with("E04"))
            .count();
        assert_eq!(count, 1, "{:?}", b.diagnostics().collect::<Vec<_>>());
        assert!(matches!(
            b.decls.values.get(&def(&b, "n")),
            Some(ingot_sema::consteval::ValueState::Done(Err(Fail::Reported)))
        ));
    }

    #[test]
    fn a_function_called_twice_is_evaluated_in_both_constants() {
        let b = build(
            "fn add(a: i64, b: i64) -> i64 { return a + b; }\nconst a: i64 = add(1, 2);\nconst b: i64 = add(3, 4);\n",
        );
        assert!(matches!(value(&b, "a"), Value::Int { value: 3, .. }));
        assert!(matches!(value(&b, "b"), Value::Int { value: 7, .. }));
    }

    #[test]
    fn a_cycle_through_a_called_body_is_e0812() {
        let b = build(
            "const a: i64 = f();\nfn f() -> i64 { return a; }\nfn g() { @static_assert(a == 0, \"a\"); }\n",
        );
        assert_eq!(
            b.diagnostics().filter(|d| d.code == "E0812").count(),
            1,
            "{:?}",
            b.diagnostics().collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_read_of_a_moved_value_is_not_guessed() {
        let b = build(
            "struct N { x: i64 }\nfn moved() -> i64 { let n = N.{ x: 1 }; let m = n; @forget(m); return n.x; }\nconst v: i64 = moved();\n",
        );
        assert!(
            b.diagnostics().next().is_none(),
            "{:?}",
            b.diagnostics().collect::<Vec<_>>()
        );
        assert!(matches!(
            b.decls.values.get(&def(&b, "v")),
            Some(ingot_sema::consteval::ValueState::Done(Err(Fail::NotImplemented(m)))) if m.contains("holds no value")
        ));
    }

    #[test]
    fn a_direct_constant_still_gets_a_value_tree() {
        let b = build("const n: i64 = 2 * 8;\nstatic s: u8 = 255;\n");
        assert!(matches!(value(&b, "n"), Value::Int { value: 16, .. }));
        assert!(matches!(value(&b, "s"), Value::Int { value: 255, .. }));
    }

    #[test]
    fn a_body_reached_by_two_constants_is_lowered_once() {
        let b = build(
            "fn add(a: i64, b: i64) -> i64 { return a + b; }\nconst a: i64 = add(1, 2);\nconst b: i64 = add(3, 4);\n",
        );
        // `add`, the initializer of `a`, and the initializer of `b`: three bodies,
        // however many evaluators read `add`.
        assert_eq!(b.factory.cache().cfg_count(), 3);
        assert!(matches!(value(&b, "a"), Value::Int { value: 3, .. }));
        assert!(matches!(value(&b, "b"), Value::Int { value: 7, .. }));
    }

    #[test]
    fn a_scalar_value_is_also_visible_to_the_direct_path() {
        let b = build("fn one() -> i64 { return 1; }\nconst n: i64 = one();\n");
        assert!(matches!(
            b.decls.consts.get(&def(&b, "n")),
            Some(ingot_sema::consteval::ConstState::Done(Ok(
                ConstValue::Int { value: 1, .. }
            )))
        ));
    }

    #[test]
    fn an_array_length_from_a_call_reaches_the_type() {
        let b = build(
            "fn four() -> i64 { return 4; }\nstruct A { xs: [four()]u8 }\nconst a: A = A.{ xs: [4]u8.{ 1, 2, 3, 4 } };\n",
        );
        match value(&b, "a") {
            Value::Aggregate { fields, .. } => match &fields[0] {
                Value::Pack { len, elems, .. } => {
                    assert_eq!(*len, 4);
                    assert!(matches!(elems.as_deref(), Some(PackElems::Ints(v)) if v.len() == 4));
                }
                other => panic!("{other:?}"),
            },
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_static_may_take_another_statics_address() {
        let b = build("static a: i64 = 1;\nstatic p: &i64 = &a;\n");
        match value(&b, "p") {
            Value::Borrow {
                alloc: AllocRef::Static(d),
                ..
            } => assert_eq!(d, def(&b, "a")),
            other => panic!("{other:?}"),
        }
    }
}
