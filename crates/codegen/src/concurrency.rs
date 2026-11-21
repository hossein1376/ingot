//! The concurrency terminators of R2: `scope`, `spawn` and `par for` (rt.md §2, §4).
//!
//! The back end only builds records and calls `std.sys.rt`; the scheduler is Ingot code.
//! Two kinds of function are written here, where the terminator that needs them is
//! emitted, because their layout depends on the types of that terminator's operands:
//!
//! * a **spawn thunk**, `fn(*mut u8)`: it reads the copied argument record, calls the
//!   target and writes the result after the record (rt.md §2);
//! * a **`par for` chunk function**, `fn(*mut u8, i64, i64)`: it loops over a part of the
//!   iteration indices and calls the per-item body (rt.md §4).
//!
//! Both are named by `mangle_key` (`InstanceBody::SpawnThunk`, `ParForChunk`) and written
//! once per symbol.

use super::*;
use ingot_cfg::model::{BodyKey, CfgId, CfgKey, CfgLocal, Local, PlaceUse};

/// The scheduler entry points the back end calls, the entry that starts it, and the
/// function the root task's record names as its start, which only a function value in
/// `start_tasks` refers to (the walk of calls does not follow function values).
const SCHEDULER_ROOTS: [&str; 7] = [
    "no_start",
    "start_tasks",
    "scope_enter",
    "scope_cancel",
    "scope_wait",
    "task_spawn",
    "par_for_range",
];

/// What a `par for` iterates, from the types of its operands.
#[derive(Clone)]
enum ParIter {
    /// `lo..hi` or `lo..=hi`, with a step: the loop variable is `lo + index * step`.
    Range { ty: TypeId, int: IntTy },
    /// A slice: the item is the element at the index, or a borrow of it.
    Slice {
        slice_ty: TypeId,
        elem: TypeId,
        exclusive: bool,
    },
}

/// Everything the chunk function needs, known at the `par for` terminator.
struct ChunkPlan {
    /// The capture record: a tuple of the capture tuple, then the range's start and step
    /// or the slice.
    rec_ty: TypeId,
    /// The first parameter of the item body: a tuple of shared borrows.
    tuple_ty: TypeId,
    iter: ParIter,
    /// The types of the item and the index parameters, when the body takes them.
    item_ty: Option<TypeId>,
    index_ty: Option<TypeId>,
    item_symbol: String,
    item_sig: FnSig,
}

/// Everything a spawn thunk needs.
struct ThunkPlan {
    /// The argument record: a tuple of the callee value, if the call is through a value,
    /// then the arguments.
    rec_ty: TypeId,
    field_tys: Vec<TypeId>,
    target: CallTarget,
    has_callee: bool,
    /// The result type, when it is not `void`.
    result: Option<TypeId>,
    /// Where the runtime put the result, from the start of the record.
    result_offset: u64,
}

impl<'p> Codegen<'p> {
    /// Whether a body opens a `scope`, spawns or runs a `par for`.
    pub(super) fn cfg_uses_scheduler(cfg: &Cfg) -> bool {
        cfg.locals.iter().any(|l| l.kind == LocalKind::Scope)
            || cfg
                .blocks
                .iter()
                .any(|b| matches!(b.term, Terminator::Spawn { .. } | Terminator::ParFor { .. }))
    }

    fn rt_def(&self, name: &str) -> Result<DefId, BuildError> {
        find_module_item(self.program.sema, "std.sys.rt", name)
            .ok_or_else(|| BuildError::Internal(format!("std.sys.rt.{name} is missing")))
    }

    fn rt_key(&self, name: &str) -> Result<InstanceKey, BuildError> {
        Ok(InstanceKey {
            owner: self.rt_def(name)?,
            body: InstanceBody::Item,
            subst: Subst::new(),
        })
    }

    /// The runtime functions a program that reaches the scheduler needs.
    pub(super) fn scheduler_roots(&self) -> Result<Vec<InstanceKey>, BuildError> {
        SCHEDULER_ROOTS.iter().map(|n| self.rt_key(n)).collect()
    }

    fn rt_symbol(&self, name: &str) -> Result<String, BuildError> {
        let key = self.rt_key(name)?;
        self.symbols
            .get(&key)
            .cloned()
            .ok_or_else(|| BuildError::Internal(format!("std.sys.rt.{name} was not discovered")))
    }

    /// The offset and type of the field `name` of a struct type, from its layout.
    fn field_offset(&mut self, ty: TypeId, name: &str) -> Result<(u64, TypeId), BuildError> {
        let ty = self.resolve_ty(ty);
        let TyKind::Adt(def, args) = self.program.decls.types.kind(ty).clone() else {
            return Err(BuildError::Internal(format!(
                "the field `{name}` of a type that is not a struct"
            )));
        };
        let sig = self
            .program
            .decls
            .struct_sig(def)
            .cloned()
            .ok_or_else(|| BuildError::Internal("a field of a non-struct".to_string()))?;
        let index = sig
            .fields
            .iter()
            .position(|f| f.name == name)
            .ok_or_else(|| BuildError::Internal(format!("no field `{name}`")))?;
        let subst = self.program.decls.subst_for(def, &args);
        let field_ty = self.program.decls.instantiate(sig.fields[index].ty, &subst);
        let layout = self.layout(ty)?;
        let offset = layout
            .fields
            .get(index)
            .map(|f| f.offset)
            .ok_or_else(|| BuildError::Internal("a field with no layout".to_string()))?;
        Ok((offset, field_ty))
    }

    /// The offset of the `rt.ScopeState` inside a `std.task.Scope`
    /// (`Scope.state.value`: the state is the one field of a `Cell`).
    fn scope_state_offset(&mut self, scope_ty: TypeId) -> Result<u64, BuildError> {
        let (state, cell_ty) = self.field_offset(scope_ty, "state")?;
        let (value, _) = self.field_offset(cell_ty, "value")?;
        Ok(state + value)
    }

    fn u8_ptr_ty(&mut self) -> TypeId {
        let u8_ty = self.program.decls.types.intern(TyKind::Int(IntTy::U8));
        self.program
            .decls
            .types
            .intern(TyKind::RawPtr(Mutability::Mut, u8_ty))
    }

    fn is_unit(&self, ty: TypeId) -> bool {
        ty == self.program.decls.types.void || ty == self.program.decls.types.never
    }

    /// A function written by this module: its signature is its CFG's own, and `generate`
    /// writes the blocks. Written once per symbol.
    fn emit_synthetic(
        &mut self,
        symbol: String,
        cfg: Cfg,
        generate: impl FnOnce(&mut FnGen<'_, 'p>) -> Result<(), BuildError>,
    ) -> Result<(), BuildError> {
        if !self.synthetic.insert(symbol.clone()) {
            return Ok(());
        }
        let sig = Self::sig_from_cfg(cfg.key.owner, &cfg);
        let body = Body {
            symbol,
            sig,
            subst: self.subst.clone(),
            cfg,
        };
        let ret_abi = self.ret_abi(&body.sig)?;
        let abi_params = self.abi_params(&body.sig)?;
        let ret_ll = ret_abi.ret_ll();
        let text = {
            let mut g = FnGen {
                cg: self,
                body: &body,
                blocks: Vec::new(),
                cur: 0,
                cold: Vec::new(),
                next_tmp: 0,
                next_label: 0,
                ret_ll,
                ret_abi,
                abi_params,
                entry_allocas: Vec::new(),
                known_variants: BTreeMap::new(),
                block_index: 0,
            };
            generate(&mut g)?;
            g.render()
        };
        self.functions.push(text);
        Ok(())
    }

    /// A CFG with only locals, for a function this module writes by hand.
    fn synthetic_cfg(
        &self,
        owner: DefId,
        file: usize,
        name: String,
        locals: Vec<TypeId>,
        params: usize,
    ) -> Cfg {
        Cfg {
            key: CfgKey {
                owner,
                body: BodyKey::Item,
            },
            name,
            file,
            ret: self.program.decls.types.void,
            ret_name: String::new(),
            error: None,
            error_name: None,
            params: (0..params as Local).collect(),
            locals: locals
                .into_iter()
                .map(|ty| CfgLocal {
                    kind: LocalKind::Temp,
                    name: None,
                    source: None,
                    ty,
                    ty_name: String::new(),
                    span: Span::default(),
                    decl_span: Span::default(),
                })
                .collect(),
            blocks: Vec::new(),
            entry: 0,
            closure_ty: None,
            closure_fields: Vec::new(),
        }
    }
}

impl<'a, 'p> FnGen<'a, 'p> {
    /// A call of a `std.sys.rt` function with the LLVM types of its parameters spelled
    /// out, which are checked against its signature. Gives the result, if it has one.
    fn rt_call(
        &mut self,
        name: &str,
        args: &[(&str, String)],
    ) -> Result<Option<String>, BuildError> {
        let def = self.cg.rt_def(name)?;
        let symbol = self.cg.rt_symbol(name)?;
        let sig =
            self.cg.program.decls.fn_sig(def).cloned().ok_or_else(|| {
                BuildError::Internal(format!("std.sys.rt.{name} has no signature"))
            })?;
        let expected = self.cg.abi_params(&sig)?;
        if expected
            .iter()
            .map(String::as_str)
            .ne(args.iter().map(|a| a.0))
        {
            return Err(BuildError::Internal(format!(
                "std.sys.rt.{name} takes {expected:?}, not the parameters the back end passes"
            )));
        }
        let list: Vec<String> = args.iter().map(|(ll, v)| format!("{ll} {v}")).collect();
        let ret = self.cg.plain_ret_ll(&sig)?;
        let callee = global_ref(&symbol);
        if ret == "void" {
            self.emit(format!("call void {callee}({})", list.join(", ")));
            Ok(None)
        } else {
            let out = self.fresh_tmp();
            self.emit(format!("{out} = call {ret} {callee}({})", list.join(", ")));
            Ok(Some(out))
        }
    }

    /// The address of the `rt.ScopeState` inside the `Scope` at `scope_ptr`.
    fn scope_state_at(&mut self, scope_ptr: &str, scope_ty: TypeId) -> Result<String, BuildError> {
        let offset = self.cg.scope_state_offset(scope_ty)?;
        Ok(self.ptr_at(scope_ptr, offset))
    }

    fn scope_state_of_local(&mut self, local: Local) -> Result<String, BuildError> {
        let ty = self.body.cfg.local(local).ty;
        self.scope_state_at(&format!("%l{local}"), ty)
    }

    /// The start of a `scope` local's storage: the runtime initializes its state.
    pub(super) fn scope_enter(&mut self, local: Local) -> Result<(), BuildError> {
        let state = self.scope_state_of_local(local)?;
        self.rt_call("scope_enter", &[("ptr", state)])?;
        Ok(())
    }

    pub(super) fn scope_cancel(&mut self, local: Local) -> Result<(), BuildError> {
        let state = self.scope_state_of_local(local)?;
        self.rt_call("scope_cancel", &[("ptr", state)])?;
        Ok(())
    }

    pub(super) fn scope_wait(&mut self, local: Local, next: Block) -> Result<(), BuildError> {
        let state = self.scope_state_of_local(local)?;
        self.rt_call("scope_wait", &[("ptr", state)])?;
        self.emit(format!("br label %{}", self.label_of(next)));
        Ok(())
    }

    // ------------------------------------------------------------------ spawn

    /// `spawn call in s`: copies the callee value and the arguments into a record, hands
    /// it to the runtime with the thunk that will make the call, and builds the `Task`.
    pub(super) fn spawn(
        &mut self,
        target: &CallTarget,
        args: &[Operand],
        scope: &Operand,
        dest: Option<&Place>,
        next: Block,
    ) -> Result<(), BuildError> {
        let has_callee = matches!(target, CallTarget::Value { .. });
        let mut record: Vec<Operand> = Vec::new();
        if let CallTarget::Value { callee, .. } = target {
            record.push(callee.clone());
        }
        record.extend(args.iter().cloned());
        let mut field_tys = Vec::new();
        for op in &record {
            field_tys.push(self.operand_ty(op)?);
        }
        let rec_ty = self
            .cg
            .program
            .decls
            .types
            .intern(TyKind::Tuple(field_tys.clone()));

        // The result is the `R` of the `Task[R]` the spawn gives; a statement-form spawn
        // has none.
        let void = self.cg.program.decls.types.void;
        let (task_ty, result_ty) = match dest {
            Some(place) => {
                let task_ty = self.place_type(place)?;
                let result = match self.cg.program.decls.types.kind(task_ty).clone() {
                    TyKind::Adt(_, args) => match args.first() {
                        Some(GenericArg::Ty(r)) => self.cg.resolve_ty(*r),
                        _ => void,
                    },
                    _ => {
                        return Err(BuildError::Internal(
                            "a spawn whose handle is not a `Task`".to_string(),
                        ));
                    }
                };
                (Some(task_ty), result)
            }
            None => (None, void),
        };
        let (result_size, result_align) = if self.cg.is_unit(result_ty) {
            (0, 1)
        } else {
            let l = self.cg.layout(result_ty)?;
            (l.size, l.align)
        };
        let (args_size, args_align) = if record.is_empty() {
            (0, 1)
        } else {
            let l = self.cg.layout(rec_ty)?;
            (l.size, l.align)
        };
        let result_offset = args_size.div_ceil(result_align) * result_align;

        let key = InstanceKey {
            owner: self.body.cfg.key.owner,
            body: InstanceBody::SpawnThunk {
                within: self.body.cfg.key.body,
                block: self.block_index as u32,
            },
            subst: self.cg.subst.clone(),
        };
        let thunk = mangle_key(self.cg.program.decls, self.cg.program.sema, &key);
        let plan = ThunkPlan {
            rec_ty,
            field_tys,
            target: target.clone(),
            has_callee,
            result: (!self.cg.is_unit(result_ty)).then_some(result_ty),
            result_offset,
        };
        let owner = self.body.cfg.key.owner;
        let file = self.body.cfg.file;
        let rec_local_ty = self.cg.u8_ptr_ty();
        let mut locals = vec![rec_local_ty];
        locals.extend(plan.field_tys.iter().copied());
        if let Some(r) = plan.result {
            locals.push(r);
        }
        let cfg = self.cg.synthetic_cfg(owner, file, thunk.clone(), locals, 1);
        self.cg
            .emit_synthetic(thunk.clone(), cfg, |g| g.generate_thunk(&plan))?;

        let record_ptr = if record.is_empty() {
            "null".to_string()
        } else {
            let (v, ll) = self.aggregate(&AggregateKind::Tuple, rec_ty, &record)?;
            let slot = self.alloca(&ll, args_align);
            self.emit(format!("store {ll} {v}, ptr {slot}"));
            slot
        };
        let (scope_v, _) = self.value(scope)?;
        let scope_ty = self.scope_pointee(scope)?;
        let state = self.scope_state_at(&scope_v, scope_ty)?;
        let task_ref = self
            .rt_call(
                "task_spawn",
                &[
                    ("ptr", state),
                    ("ptr", global_ref(&thunk)),
                    ("ptr", record_ptr),
                    ("i64", args_size.to_string()),
                    ("i64", args_align.to_string()),
                    ("i64", result_size.to_string()),
                    ("i64", result_align.to_string()),
                    (
                        "i1",
                        if dest.is_none() { "true" } else { "false" }.to_string(),
                    ),
                ],
            )?
            .ok_or_else(|| BuildError::Internal("task_spawn returns no handle".to_string()))?;
        if let (Some(place), Some(task_ty)) = (dest, task_ty) {
            let addr = self.place_addr(place)?;
            let (owner_off, _) = self.cg.field_offset(task_ty, "owner")?;
            let (task_off, task_field) = self.cg.field_offset(task_ty, "task")?;
            let at = self.ptr_at(&addr, owner_off);
            self.emit(format!("store ptr {scope_v}, ptr {at}"));
            let at = self.ptr_at(&addr, task_off);
            let ll = self.cg.llvm_ty(task_field)?;
            self.emit(format!("store {ll} {task_ref}, ptr {at}"));
        }
        self.emit(format!("br label %{}", self.label_of(next)));
        Ok(())
    }

    /// The `Scope` type a `&Scope` operand points at.
    fn scope_pointee(&mut self, scope: &Operand) -> Result<TypeId, BuildError> {
        let ty = self.operand_ty(scope)?;
        match self.cg.program.decls.types.kind(ty).clone() {
            TyKind::Ref(_, inner) => Ok(self.cg.resolve_ty(inner)),
            _ => Err(BuildError::Internal(
                "the scope of a spawn is not a borrow".to_string(),
            )),
        }
    }

    /// The thunk: loads each field of the record into a local, calls the target with
    /// them, and stores the result at the offset the runtime computed.
    fn generate_thunk(&mut self, plan: &ThunkPlan) -> Result<(), BuildError> {
        self.blocks = vec![
            OutBlock {
                label: "bb0".to_string(),
                lines: Vec::new(),
            },
            OutBlock {
                label: "bb1".to_string(),
                lines: Vec::new(),
            },
        ];
        self.cur = 0;
        self.emit_locals()?;
        self.emit_params()?;
        let rec = self.fresh_tmp();
        self.emit(format!("{rec} = load ptr, ptr %l0"));
        if !plan.field_tys.is_empty() {
            let layout = self.cg.layout(plan.rec_ty)?;
            for (i, ty) in plan.field_tys.iter().enumerate() {
                let resolved = self.cg.resolve_ty(*ty);
                if self.cg.is_unit(resolved) {
                    continue;
                }
                let ll = self.cg.llvm_ty(*ty)?;
                let at = self.ptr_at(&rec, layout.fields[i].offset);
                let v = self.fresh_tmp();
                self.emit(format!("{v} = load {ll}, ptr {at}"));
                self.emit(format!("store {ll} {v}, ptr %l{}", i + 1));
            }
        }
        let local = |i: usize| Operand::Place(Place::local(i as Local), PlaceUse::Copy);
        let skip = usize::from(plan.has_callee);
        let args: Vec<Operand> = (skip..plan.field_tys.len()).map(|i| local(i + 1)).collect();
        let target = match &plan.target {
            CallTarget::Fn { .. } => plan.target.clone(),
            CallTarget::Value { kind, mode, .. } => CallTarget::Value {
                callee: local(1),
                kind: kind.clone(),
                mode: *mode,
            },
        };
        let result_local =
            (plan.result.is_some()).then(|| Place::local((plan.field_tys.len() + 1) as Local));
        self.call(&target, &args, result_local.as_ref(), 1, Span::default())?;
        self.cur = 1;
        if let Some(r) = plan.result {
            let ll = self.cg.llvm_ty(r)?;
            let v = self.fresh_tmp();
            self.emit(format!(
                "{v} = load {ll}, ptr %l{}",
                plan.field_tys.len() + 1
            ));
            let rec = self.fresh_tmp();
            self.emit(format!("{rec} = load ptr, ptr %l0"));
            let at = self.ptr_at(&rec, plan.result_offset);
            self.emit(format!("store {ll} {v}, ptr {at}"));
        }
        self.emit("ret void");
        Ok(())
    }

    // ---------------------------------------------------------------- par for

    /// `par for`: builds the capture record, counts the iterations and hands the runtime
    /// the chunk function. The index space is `0..n`; the chunk function turns an index
    /// into the loop's item, so an inclusive end at the type's maximum needs no
    /// arithmetic past it (rt.md §4).
    #[allow(clippy::too_many_arguments)]
    pub(super) fn par_for(
        &mut self,
        body: CfgId,
        captures: &[Operand],
        iterand: &ParIterand,
        grain: Option<&Operand>,
        binds_item: bool,
        binds_index: bool,
        next: Block,
    ) -> Result<(), BuildError> {
        let cfgs = self.cg.program.cfgs;
        if !cfgs.has(body) {
            return Err(BuildError::Internal(
                "a `par for` whose body was dropped".to_string(),
            ));
        }
        let item_cfg = cfgs.get(body);
        let BodyKey::ParFor(stmt) = item_cfg.key.body else {
            return Err(BuildError::Internal(
                "a `par for` whose body is not a `par for` body".to_string(),
            ));
        };
        let param_ty = |i: usize| item_cfg.local(item_cfg.params[i]).ty;
        let tuple_ty = self.cg.resolve_ty(param_ty(0));
        let mut next_param = 1;
        let mut take = |wanted: bool| {
            wanted.then(|| {
                next_param += 1;
                param_ty(next_param - 1)
            })
        };
        let item_ty = take(binds_item);
        let index_ty = take(binds_index);

        // What is iterated, and the record that carries it to the chunk function.
        let (iter, mut record_tys) = match iterand {
            ParIterand::Range { lo, .. } => {
                let ty = self.operand_ty(lo)?;
                let Some(int) = int_of(self.cg.program.decls.types.kind(ty)) else {
                    return Err(BuildError::Internal(
                        "a `par for` range of non-integers".to_string(),
                    ));
                };
                (ParIter::Range { ty, int }, vec![tuple_ty, ty, ty])
            }
            ParIterand::Value(op) => {
                let slice_ty = self.operand_ty(op)?;
                match self.cg.program.decls.types.kind(slice_ty).clone() {
                    TyKind::Slice(m, elem) => (
                        ParIter::Slice {
                            slice_ty,
                            elem,
                            exclusive: m == Mutability::Mut,
                        },
                        vec![tuple_ty, slice_ty],
                    ),
                    _ => return self.cg.ni("a `par for` over a `par.Split` (R4)"),
                }
            }
        };
        let rec_ty = self
            .cg
            .program
            .decls
            .types
            .intern(TyKind::Tuple(std::mem::take(&mut record_tys)));
        let layout = self.cg.layout(rec_ty)?;
        let rec_ll = self.cg.llvm_ty(rec_ty)?;
        let rec = self.alloca(&rec_ll, layout.align);
        let field = |i: usize| layout.fields[i].offset;

        if !captures.is_empty() {
            let (v, ll) = self.aggregate(&AggregateKind::Tuple, tuple_ty, captures)?;
            let at = self.ptr_at(&rec, field(0));
            self.emit(format!("store {ll} {v}, ptr {at}"));
        }
        let count = match iterand {
            ParIterand::Range {
                lo,
                hi,
                inclusive,
                step,
            } => {
                let ParIter::Range { ty, int } = &iter else {
                    unreachable!("a range iterand has a range plan")
                };
                let ll = self.cg.llvm_ty(*ty)?;
                let (lo_v, _) = self.value(lo)?;
                let (hi_v, _) = self.value(hi)?;
                let step_v = match step {
                    Some(op) => self.value(op)?.0,
                    None => "1".to_string(),
                };
                let at = self.ptr_at(&rec, field(1));
                self.emit(format!("store {ll} {lo_v}, ptr {at}"));
                let at = self.ptr_at(&rec, field(2));
                self.emit(format!("store {ll} {step_v}, ptr {at}"));
                self.par_for_count(*int, &ll, &lo_v, &hi_v, &step_v, *inclusive)
            }
            ParIterand::Value(op) => {
                let (v, ll) = self.value(op)?;
                let v = self.as_ssa(&v, &ll, 8)?;
                let at = self.ptr_at(&rec, field(1));
                self.emit(format!("store {ll} {v}, ptr {at}"));
                let len = self.fresh_tmp();
                self.emit(format!("{len} = extractvalue {ll} {v}, 1"));
                len
            }
        };
        let grain = match grain {
            Some(op) => self.value(op)?.0,
            None => "0".to_string(),
        };

        let item_key = InstanceKey {
            owner: self.body.cfg.key.owner,
            body: InstanceBody::ParFor(stmt),
            subst: self.cg.subst.clone(),
        };
        let item_symbol = self.cg.symbols.get(&item_key).cloned().ok_or_else(|| {
            BuildError::Internal("the body of a `par for` was not discovered".to_string())
        })?;
        let item_sig = self
            .cg
            .bodies
            .get(&item_symbol)
            .map(|b| b.sig.clone())
            .ok_or_else(|| {
                BuildError::Internal("the body of a `par for` was not emitted".to_string())
            })?;
        let chunk_key = InstanceKey {
            owner: self.body.cfg.key.owner,
            body: InstanceBody::ParForChunk(stmt),
            subst: self.cg.subst.clone(),
        };
        let chunk = mangle_key(self.cg.program.decls, self.cg.program.sema, &chunk_key);
        let plan = ChunkPlan {
            rec_ty,
            tuple_ty,
            iter,
            item_ty: item_ty.map(|t| self.cg.resolve_ty(t)),
            index_ty: index_ty.map(|t| self.cg.resolve_ty(t)),
            item_symbol,
            item_sig,
        };
        let rec_local_ty = self.cg.u8_ptr_ty();
        let i64_ty = self.cg.program.decls.types.i64_;
        let mut locals = vec![rec_local_ty, i64_ty, i64_ty, tuple_ty];
        locals.extend(plan.item_ty);
        locals.extend(plan.index_ty);
        let cfg = self.cg.synthetic_cfg(
            self.body.cfg.key.owner,
            self.body.cfg.file,
            chunk.clone(),
            locals,
            3,
        );
        self.cg
            .emit_synthetic(chunk.clone(), cfg, |g| g.generate_chunk(&plan))?;

        self.rt_call(
            "par_for_range",
            &[
                ("i64", "0".to_string()),
                ("i64", count),
                ("i64", grain),
                ("ptr", global_ref(&chunk)),
                ("ptr", rec),
            ],
        )?;
        self.emit(format!("br label %{}", self.label_of(next)));
        Ok(())
    }

    /// The number of iterations of `lo..hi` or `lo..=hi` by `step`, as an `i64` in
    /// `0..=i64::MAX` (a range with more iterations than that cannot finish). The
    /// difference is taken in 64 bits after extending both ends the way the type is
    /// extended, so it is exact for every width; nothing here overflows, whatever the
    /// ends are, and an inclusive end equal to the maximum is the same as any other.
    fn par_for_count(
        &mut self,
        int: IntTy,
        ll: &str,
        lo: &str,
        hi: &str,
        step: &str,
        inclusive: bool,
    ) -> String {
        let signed = int.signed();
        let extend = |g: &mut Self, v: &str| -> String {
            if int.bits() == 64 {
                return v.to_string();
            }
            let out = g.fresh_tmp();
            let op = if signed { "sext" } else { "zext" };
            g.emit(format!("{out} = {op} {ll} {v} to i64"));
            out
        };
        let lo64 = extend(self, lo);
        let hi64 = extend(self, hi);
        let step64 = extend(self, step);
        let pred = match (inclusive, signed) {
            (false, true) => "sle",
            (false, false) => "ule",
            (true, true) => "slt",
            (true, false) => "ult",
        };
        let empty = self.fresh_tmp();
        self.emit(format!("{empty} = icmp {pred} {ll} {hi}, {lo}"));
        let diff = self.fresh_tmp();
        self.emit(format!("{diff} = sub i64 {hi64}, {lo64}"));
        let span = if inclusive {
            diff
        } else {
            let t = self.fresh_tmp();
            self.emit(format!("{t} = sub i64 {diff}, 1"));
            t
        };
        let q = self.fresh_tmp();
        self.emit(format!("{q} = udiv i64 {span}, {step64}"));
        let big = self.fresh_tmp();
        self.emit(format!("{big} = icmp uge i64 {q}, 9223372036854775807"));
        let q1 = self.fresh_tmp();
        self.emit(format!("{q1} = add i64 {q}, 1"));
        let capped = self.fresh_tmp();
        self.emit(format!(
            "{capped} = select i1 {big}, i64 9223372036854775807, i64 {q1}"
        ));
        let n = self.fresh_tmp();
        self.emit(format!("{n} = select i1 {empty}, i64 0, i64 {capped}"));
        n
    }

    /// The chunk function: for each index in `lo..hi`, forms the item and the index the
    /// body asked for, and calls the per-item body with the capture tuple.
    fn generate_chunk(&mut self, plan: &ChunkPlan) -> Result<(), BuildError> {
        // Locals: 0 the record, 1 `lo`, 2 `hi`, 3 the capture tuple, then the item and
        // the index when the body takes them.
        const REC: usize = 0;
        const LO: usize = 1;
        const HI: usize = 2;
        const CAPTURES: usize = 3;
        let item_local = plan.item_ty.map(|_| 4);
        let index_local = plan
            .index_ty
            .map(|_| 4 + usize::from(plan.item_ty.is_some()));

        self.blocks = (0..5)
            .map(|i| OutBlock {
                label: format!("bb{i}"),
                lines: Vec::new(),
            })
            .collect();
        self.cur = 0;
        self.emit_locals()?;
        self.emit_params()?;
        let rec = self.fresh_tmp();
        self.emit(format!("{rec} = load ptr, ptr %l{REC}"));
        let layout = self.cg.layout(plan.rec_ty)?;
        let tuple_ll = self.cg.llvm_ty(plan.tuple_ty)?;
        let at = self.ptr_at(&rec, layout.fields[0].offset);
        let tuple = self.fresh_tmp();
        self.emit(format!("{tuple} = load {tuple_ll}, ptr {at}"));
        self.emit(format!("store {tuple_ll} {tuple}, ptr %l{CAPTURES}"));
        // What the loop reads from the record, loaded once before it.
        let (first, second) = match &plan.iter {
            ParIter::Range { ty, .. } => {
                let ll = self.cg.llvm_ty(*ty)?;
                let at = self.ptr_at(&rec, layout.fields[1].offset);
                let lo = self.fresh_tmp();
                self.emit(format!("{lo} = load {ll}, ptr {at}"));
                let at = self.ptr_at(&rec, layout.fields[2].offset);
                let step = self.fresh_tmp();
                self.emit(format!("{step} = load {ll}, ptr {at}"));
                (lo, step)
            }
            ParIter::Slice { slice_ty, .. } => {
                let ll = self.cg.llvm_ty(*slice_ty)?;
                let at = self.ptr_at(&rec, layout.fields[1].offset);
                let slice = self.fresh_tmp();
                self.emit(format!("{slice} = load {ll}, ptr {at}"));
                let data = self.fresh_tmp();
                self.emit(format!("{data} = extractvalue {ll} {slice}, 0"));
                (data, String::new())
            }
        };
        let k_slot = self.alloca("i64", 8);
        let lo = self.fresh_tmp();
        self.emit(format!("{lo} = load i64, ptr %l{LO}"));
        self.emit(format!("store i64 {lo}, ptr {k_slot}"));
        self.emit("br label %bb1");

        // The loop head.
        self.cur = 1;
        let k = self.fresh_tmp();
        self.emit(format!("{k} = load i64, ptr {k_slot}"));
        let hi = self.fresh_tmp();
        self.emit(format!("{hi} = load i64, ptr %l{HI}"));
        let more = self.fresh_tmp();
        self.emit(format!("{more} = icmp slt i64 {k}, {hi}"));
        self.emit(format!("br i1 {more}, label %bb2, label %bb4"));

        // The body: the item, the index, the call.
        self.cur = 2;
        let k = self.fresh_tmp();
        self.emit(format!("{k} = load i64, ptr {k_slot}"));
        if let Some(local) = item_local {
            match &plan.iter {
                ParIter::Range { ty, int } => {
                    let ll = self.cg.llvm_ty(*ty)?;
                    let index = if int.bits() < 64 {
                        let t = self.fresh_tmp();
                        self.emit(format!("{t} = trunc i64 {k} to {ll}"));
                        t
                    } else {
                        k.clone()
                    };
                    let scaled = self.fresh_tmp();
                    self.emit(format!("{scaled} = mul {ll} {index}, {second}"));
                    let v = self.fresh_tmp();
                    self.emit(format!("{v} = add {ll} {first}, {scaled}"));
                    self.emit(format!("store {ll} {v}, ptr %l{local}"));
                }
                ParIter::Slice {
                    elem, exclusive, ..
                } => {
                    let elem_ll = self.cg.llvm_ty(*elem)?;
                    let p = self.fresh_tmp();
                    self.emit(format!(
                        "{p} = getelementptr {elem_ll}, ptr {first}, i64 {k}"
                    ));
                    if *exclusive {
                        self.emit(format!("store ptr {p}, ptr %l{local}"));
                    } else {
                        let v = self.fresh_tmp();
                        self.emit(format!("{v} = load {elem_ll}, ptr {p}"));
                        self.emit(format!("store {elem_ll} {v}, ptr %l{local}"));
                    }
                }
            }
        }
        if let Some(local) = index_local {
            self.emit(format!("store i64 {k}, ptr %l{local}"));
        }
        let local = |i: usize| Operand::Place(Place::local(i as Local), PlaceUse::Copy);
        let mut args = vec![local(CAPTURES)];
        args.extend(item_local.map(local));
        args.extend(index_local.map(local));
        let subst = self.cg.subst.clone();
        self.call_direct_symbol(
            &plan.item_symbol,
            &plan.item_sig,
            &subst,
            &args,
            None,
            3,
            false,
        )?;

        // The next index.
        self.cur = 3;
        let k = self.fresh_tmp();
        self.emit(format!("{k} = load i64, ptr {k_slot}"));
        let next = self.fresh_tmp();
        self.emit(format!("{next} = add i64 {k}, 1"));
        self.emit(format!("store i64 {next}, ptr {k_slot}"));
        self.emit("br label %bb1");

        self.cur = 4;
        self.emit("ret void");
        Ok(())
    }
}
