//! The Ingot back end (pipeline phases 11b-14): one LLVM IR text module for a
//! program and the `clang` link step.
//!
//! The IR is text written by this crate (COMPILER.md, "The back end"): no LLVM
//! library, no dependency. The pinned compiler is upstream LLVM 22
//! (`/opt/homebrew/opt/llvm@22/bin/clang` on this Mac); a missing or different
//! major version is a named build failure, never Apple `clang`.
//!
//! B2 lowers instances of generic functions and types, closures, `&dyn`,
//! built-in `Eq`/`Ord`, and `for` over an `Iter`. A construct this step does not
//! lower is an explicit `not implemented` entry naming its later step.

use ingot_cfg::Cfgs;
use ingot_cfg::instances::{
    Instance, InstanceBody, InstanceKey, builtin_comparison_target, closure_instance_key,
    comparison_element_keys, comparison_method_key, interface_method_order, mangle_key,
    method_table_methods, value_fn_refs,
};
use ingot_cfg::model::{
    AggregateKind, Block, BlockData, BoundsKind, CallTarget, CastKind, Cfg, Check, CheckKind,
    Const, Operand, Place, PlaceBase, Projection, Rvalue, StmtKind, Terminator, Trap, ValueKind,
};
use ingot_diag::source::{Sources, Span};
use ingot_sema::defs::{DefId, DefKind, ModuleId, Sema};
use ingot_sema::sigs::{Declarations, FnSig, ItemSig, Layout, LayoutKind, ParamSig, Tri};
use ingot_sema::ty::{
    ConstArg, ConstValue, FloatTy, GenericArg, IntTy, Mutability, Subst, TyKind, TypeId,
};
use ingot_sema::value::{AllocRef, PackElems, PathComp, SliceSource, Value as SemValue};
use ingot_syntax::ast::BinOp;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::PathBuf;

pub mod llvm;
mod show;

/// A reason the back end cannot produce an executable.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BuildError {
    /// A construct this back end does not lower yet. Its text is the
    /// `not implemented: ...` entry the driver prints.
    NotImplemented(String),
    /// The pinned LLVM 22 was not found, or another version was.
    Toolchain(String),
    /// `clang` ran but could not link or compile the IR.
    Link(String),
    /// A build has no `main`. The driver turns this into E1704.
    NoEntryPoint,
    /// Reading or writing a file failed.
    Io(String),
    /// A compiler invariant was broken.
    Internal(String),
}

impl fmt::Display for BuildError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BuildError::NotImplemented(what) => write!(f, "not implemented: {what}"),
            BuildError::Toolchain(message) => write!(f, "{message}"),
            BuildError::Link(message) => write!(f, "linking failed: {message}"),
            BuildError::NoEntryPoint => write!(f, "the package has no `main`"),
            BuildError::Io(message) => write!(f, "{message}"),
            BuildError::Internal(message) => write!(f, "internal compiler error: {message}"),
        }
    }
}

impl std::error::Error for BuildError {}

/// The pieces of an analysis the back end reads.
pub struct Program<'a> {
    pub sources: &'a Sources,
    pub sema: &'a Sema,
    pub decls: &'a mut Declarations,
    pub cfgs: &'a Cfgs,
    /// Phase 11a's instances, sorted by mangled name.
    pub instances: &'a [Instance],
    /// Lowered bodies that are not in `cfgs`: the `std.sys` runtime core.
    pub extra_cfgs: &'a [Cfg],
    /// Index of the root package among `sema.packages`.
    pub checked: usize,
    /// The checked packages among `sema.packages` (every package but `std`).
    pub checked_packages: &'a [usize],
    /// The target name, `aarch64-macos-m4` and so on.
    pub target: &'a str,
    /// `ingot test`: the executable runs the `test` blocks among `instances`, through
    /// `std.sys.rt.test_start`, instead of `main` (pipeline §8, tools.md §3).
    pub test_build: bool,
}

/// What to emit and where.
#[derive(Clone, Debug)]
pub struct BuildOptions {
    pub target: String,
    /// `--fast`: the same IR at `-O0`.
    pub fast: bool,
    /// The artifact path; the caller supplies the default.
    pub output: PathBuf,
    /// The package root, for relative paths in the link step.
    pub root: PathBuf,
}

/// A successful build.
#[derive(Clone, Debug)]
pub struct Artifact {
    pub ir: String,
    pub path: PathBuf,
}

/// Emit the program's IR text. Deterministic: the same `Program` gives the same bytes.
pub fn emit(program: Program<'_>) -> Result<String, BuildError> {
    if program.target != "aarch64-macos-m4" {
        return Err(BuildError::NotImplemented(format!(
            "building for {} (T-steps)",
            program.target
        )));
    }
    if program
        .sema
        .packages
        .get(program.checked)
        .map(|p| p.name.as_str())
        .is_none()
    {
        return Err(BuildError::Internal(
            "the checked package is unknown".to_string(),
        ));
    }
    let mut cg = Codegen::new(program);
    cg.run()?;
    cg.finish()
}

/// Emit, write the IR next to the artifact, and link it with the pinned `clang`.
pub fn build(program: Program<'_>, options: &BuildOptions) -> Result<Artifact, BuildError> {
    let ir = emit(program)?;
    if let Some(parent) = options.output.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| BuildError::Io(format!("could not create {}: {e}", parent.display())))?;
    }
    let ir_path = options.output.with_extension("ll");
    std::fs::write(&ir_path, &ir)
        .map_err(|e| BuildError::Io(format!("could not write {}: {e}", ir_path.display())))?;
    llvm::link(&ir_path, &options.output, options.fast, &options.target).map_err(|e| match e {
        llvm::LinkError::Toolchain(message) => BuildError::Toolchain(message),
        llvm::LinkError::Link(message) => BuildError::Link(message),
    })?;
    Ok(Artifact {
        ir,
        path: options.output.clone(),
    })
}

// ---------------------------------------------------------------- instances

/// One function body the back end emits.
#[derive(Clone)]
struct Body {
    symbol: String,
    sig: FnSig,
    subst: Subst,
    cfg: Cfg,
}

/// How a function returns a `T ! E` or a non-niche `?T`. `Reg` carries the tag and
/// up to one payload register in an LLVM struct; `Ptr` takes a hidden result pointer
/// and returns only the tag. A niche `?T` is `Plain`: its null-pointer form is the
/// whole value.
#[derive(Clone, Debug)]
enum RetAbi {
    Plain { ll: String },
    Result(ResultAbi),
}

#[derive(Clone, Debug)]
struct ResultAbi {
    tag_ll: String,
    payload_offset: u64,
    payload_area: u64,
    payload_align: u64,
    payload_ty: TypeId,
    error_ty: Option<TypeId>,
    optional: bool,
    ptr: bool,
}

impl RetAbi {
    fn ret_ll(&self) -> String {
        match self {
            RetAbi::Plain { ll } => ll.clone(),
            RetAbi::Result(r) if r.ptr => r.tag_ll.clone(),
            RetAbi::Result(r) => format!("{{ {}, i64 }}", r.tag_ll),
        }
    }

    fn result(&self) -> Option<&ResultAbi> {
        match self {
            RetAbi::Result(r) => Some(r),
            RetAbi::Plain { .. } => None,
        }
    }
}

/// The evaluated form of a `ConstExpr` tree.
#[derive(Clone, Debug)]
enum CeVal {
    Int(IntTy, i128),
    Bool(bool),
}

fn body_key(key: &InstanceKey) -> Option<ingot_cfg::BodyKey> {
    match key.body {
        InstanceBody::Item => Some(ingot_cfg::BodyKey::Item),
        InstanceBody::Closure(e) => Some(ingot_cfg::BodyKey::Closure(e)),
        InstanceBody::ParFor(s) => Some(ingot_cfg::BodyKey::ParFor(s)),
        // The table bodies are emitted as globals in `emit_tables`. A `par for`
        // body and a spawn thunk are R2 work.
        InstanceBody::MethodTable { .. }
        | InstanceBody::DynFnTable { .. }
        | InstanceBody::SpawnThunk { .. } => None,
    }
}

fn find_cfg(cfgs: &Cfgs, extra: &[Cfg], key: &InstanceKey) -> Option<Cfg> {
    let body = body_key(key)?;
    cfgs.iter()
        .find(|(_, cfg)| cfg.key.owner == key.owner && cfg.key.body == body)
        .map(|(_, cfg)| cfg.clone())
        .or_else(|| {
            extra
                .iter()
                .find(|cfg| cfg.key.owner == key.owner && cfg.key.body == body)
                .cloned()
        })
}

/// The `test` blocks of a test build, in the order of the executable's table: modules
/// by path, then tests by position in the source (tools.md §3). The driver numbers the
/// tests it runs with the same list.
pub fn test_items(sema: &Sema, instances: &[Instance]) -> Vec<DefId> {
    let mut found: Vec<(String, usize, DefId)> = instances
        .iter()
        .filter(|i| i.key.body == InstanceBody::Item)
        .map(|i| i.key.owner)
        .filter(|def| matches!(sema.def(*def).kind, DefKind::Test { bench: false }))
        .map(|def| {
            let d = sema.def(def);
            (sema.module(d.module).path.clone(), d.span.start, def)
        })
        .collect();
    found.sort();
    found.dedup();
    found.into_iter().map(|(_, _, def)| def).collect()
}

fn find_module_item(sema: &Sema, module: &str, name: &str) -> Option<DefId> {
    let id = *sema.module_paths.get(module)?;
    sema.module(id).values.get(name).copied()
}

// ------------------------------------------------------------------ codegen

struct Codegen<'p> {
    program: Program<'p>,
    target: String,
    /// The substitution of the body being emitted (concrete `Self` for a method).
    subst: Subst,
    bodies: BTreeMap<String, Body>,
    main_symbol: Option<String>,
    rt_start: DefId,
    rt_trap: DefId,
    /// `std.sys.rt.test_start`, in a test build.
    rt_test_start: DefId,
    symbols: BTreeMap<InstanceKey, String>,
    types: BTreeMap<TypeId, TypeInfo>,
    type_defs: Vec<(usize, String)>,
    next_type: usize,
    globals: Vec<String>,
    global_strings: BTreeMap<Vec<u8>, String>,
    /// The private global emitted for a `const` or `static` item.
    item_globals: BTreeMap<DefId, String>,
    sites: Vec<String>,
    next_site: usize,
    /// The `!prof` branch-weight nodes for cold error arms.
    profile_meta: Vec<String>,
    next_global: usize,
    externs: BTreeMap<String, String>,
    functions: Vec<String>,
    /// The generated bodies of the built-in `Eq`/`Ord` methods, by interface and
    /// concrete type.
    builtin_symbols: BTreeMap<(DefId, TypeId), String>,
    builtin_requests: BTreeSet<(DefId, TypeId)>,
    /// The `&dyn` tables emitted so far, by table symbol.
    tables: BTreeMap<String, String>,
    /// The structural-form printers (05 section 35).
    show: show::ShowState,
}

impl<'p> Codegen<'p> {
    fn new(program: Program<'p>) -> Codegen<'p> {
        Codegen {
            target: program.target.to_string(),
            subst: Subst::new(),
            program,
            bodies: BTreeMap::new(),
            main_symbol: None,
            rt_start: DefId(0),
            rt_trap: DefId(0),
            rt_test_start: DefId(0),
            symbols: BTreeMap::new(),
            types: BTreeMap::new(),
            type_defs: Vec::new(),
            next_type: 0,
            globals: Vec::new(),
            global_strings: BTreeMap::new(),
            item_globals: BTreeMap::new(),
            sites: Vec::new(),
            next_site: 0,
            profile_meta: Vec::new(),
            next_global: 0,
            externs: BTreeMap::new(),
            functions: Vec::new(),
            builtin_symbols: BTreeMap::new(),
            builtin_requests: BTreeSet::new(),
            tables: BTreeMap::new(),
            show: show::ShowState::default(),
        }
    }

    fn ni<T>(&mut self, what: impl Into<String>) -> Result<T, BuildError> {
        Err(BuildError::NotImplemented(what.into()))
    }

    fn type_display(&self, ty: TypeId) -> String {
        self.program.decls.types.display(self.program.sema, ty)
    }

    fn run(&mut self) -> Result<(), BuildError> {
        // The compiler's runtime entry points are roots of an executable even
        // though phase 11a's source roots cannot name them.
        self.rt_start = find_module_item(self.program.sema, "std.sys.rt", "start")
            .ok_or_else(|| BuildError::Internal("std.sys.rt.start is missing".to_string()))?;
        self.rt_trap = find_module_item(self.program.sema, "std.sys.rt", "trap")
            .ok_or_else(|| BuildError::Internal("std.sys.rt.trap is missing".to_string()))?;
        if self.program.test_build {
            self.rt_test_start = find_module_item(self.program.sema, "std.sys.rt", "test_start")
                .ok_or_else(|| {
                    BuildError::Internal("std.sys.rt.test_start is missing".to_string())
                })?;
        }
        self.collect_instances()?;
        if !self.program.test_build {
            self.find_main()?;
        }
        // Register every type before emitting, so the type section is self-contained.
        let mut bodies: Vec<Body> = self.bodies.values().cloned().collect();
        for body in &bodies {
            self.register_fn_types(body)?;
        }
        self.emit_builtins()?;
        self.emit_tables()?;
        for body in bodies.drain(..) {
            self.emit_body(&body)?;
        }
        if self.program.test_build {
            self.emit_test_entry()?;
        } else {
            self.emit_entry()?;
        }
        self.emit_process_main()?;
        Ok(())
    }

    /// The symbol a body is emitted under: an `export fn` keeps its source name;
    /// every other Ingot body is mangled.
    fn symbol_for_key(&self, key: &InstanceKey) -> String {
        if key.body == InstanceBody::Item {
            let sig = self.program.decls.fn_sig(key.owner);
            if sig.is_some_and(|s| s.mods.export) {
                return self.program.sema.def(key.owner).name.clone();
            }
        }
        mangle_key(self.program.decls, self.program.sema, key)
    }

    fn collect_instances(&mut self) -> Result<(), BuildError> {
        let mut queue: Vec<InstanceKey> = Vec::new();
        let mut seen: BTreeSet<InstanceKey> = BTreeSet::new();
        for instance in self.program.instances {
            if matches!(
                instance.key.body,
                InstanceBody::ParFor(_) | InstanceBody::SpawnThunk { .. }
            ) {
                return self.ni("par-for bodies and spawn thunks (R2)");
            }
            let key = instance.key.clone();
            if self.is_builtin_method(key.owner) {
                // A built-in `Eq`/`Ord` method has no CFG: the back end generates it
                // when a call requests it.
                continue;
            }
            if seen.insert(key.clone()) {
                let symbol = self.symbol_for_key(&key);
                self.symbols.insert(key.clone(), symbol);
                queue.push(key);
            }
        }
        let start = InstanceKey {
            owner: self.rt_start,
            body: InstanceBody::Item,
            subst: Subst::new(),
        };
        let trap = InstanceKey {
            owner: self.rt_trap,
            body: InstanceBody::Item,
            subst: Subst::new(),
        };
        let mut roots = vec![start, trap];
        // A fallible `main` prints its error through the structural form's helpers
        // (`show.rs`), which `std.sys.rt` provides. So does a `try` that leaves a
        // test; and a test build has no `main`, its entry calls `test_start`.
        let main_fallible = if self.program.test_build {
            roots.push(InstanceKey {
                owner: self.rt_test_start,
                body: InstanceBody::Item,
                subst: Subst::new(),
            });
            true
        } else {
            match self.main_def() {
                Ok(main) => self
                    .program
                    .decls
                    .fn_sig(main)
                    .is_some_and(|sig| sig.error.is_some()),
                Err(_) => false,
            }
        };
        if main_fallible {
            for name in ["write_bytes", "print_i64", "print_u64", "print_str"] {
                let owner = find_module_item(self.program.sema, "std.sys.rt", name)
                    .ok_or_else(|| BuildError::Internal(format!("std.sys.rt.{name} is missing")))?;
                roots.push(InstanceKey {
                    owner,
                    body: InstanceBody::Item,
                    subst: Subst::new(),
                });
            }
        }
        for key in roots {
            if seen.insert(key.clone()) {
                let symbol = self.symbol_for_key(&key);
                self.symbols.insert(key.clone(), symbol);
                queue.push(key);
            }
        }
        // A `static` or `const` initializer can hold a function address. It has no
        // CFG, so phase 11a's call walk does not see it; add the function instance
        // here, because the global's initializer names its symbol.
        let values: Vec<SemValue> = self
            .program
            .decls
            .values
            .values()
            .filter_map(|state| match state {
                ingot_sema::consteval::ValueState::Done(Ok(value)) => Some(value.clone()),
                _ => None,
            })
            .collect();
        for value in &values {
            let mut fns = Vec::new();
            value_fn_refs(value, &mut fns);
            for fn_ref in fns {
                if let Some(key) = ingot_cfg::instances::callee_key(
                    self.program.sema,
                    &mut *self.program.decls,
                    &fn_ref,
                    &Subst::new(),
                ) {
                    self.add_instance(key, &mut queue, &mut seen)?;
                }
            }
        }
        let mut at = 0;
        while at < queue.len() {
            let key = queue[at].clone();
            at += 1;
            let Some(cfg) = find_cfg(self.program.cfgs, self.program.extra_cfgs, &key) else {
                if matches!(
                    key.body,
                    InstanceBody::MethodTable { .. } | InstanceBody::DynFnTable { .. }
                ) {
                    continue;
                }
                let def = self.program.sema.def(key.owner);
                if def.kind != DefKind::ExternFn {
                    return Err(BuildError::Internal(format!(
                        "no CFG for instance {}",
                        self.symbols[&key]
                    )));
                }
                continue;
            };
            let sig = self.body_sig(&key, &cfg)?;
            let symbol = self
                .symbols
                .get(&key)
                .cloned()
                .unwrap_or_else(|| mangle_key(self.program.decls, self.program.sema, &key));
            self.bodies.insert(
                symbol.clone(),
                Body {
                    symbol,
                    sig,
                    subst: key.subst.clone(),
                    cfg: cfg.clone(),
                },
            );
            for block in &cfg.blocks {
                self.collect_calls(&key, &cfg, &block.term, &mut queue, &mut seen)?;
            }
        }
        Ok(())
    }

    /// The signature of a body. A closure has no item signature: its parameters are
    /// the captured-record parameter and the closure's own, from its CFG.
    fn body_sig(&self, key: &InstanceKey, cfg: &Cfg) -> Result<FnSig, BuildError> {
        // A `test` block has no signature; its CFG says `void`, no error, no parameters.
        // `emit_body` gives it the `fn(&TestEnv) -> i32` shape (`is_test_body`).
        let is_test = matches!(self.program.sema.def(key.owner).kind, DefKind::Test { .. });
        if key.body == InstanceBody::Item && !is_test {
            return self
                .program
                .decls
                .fn_sig(key.owner)
                .cloned()
                .ok_or_else(|| BuildError::Internal(format!("no signature for {}", cfg.name)));
        }
        let params = cfg
            .params
            .iter()
            .map(|p| {
                let local = cfg.local(*p);
                ParamSig {
                    name: local.name.clone().unwrap_or_else(|| "self".to_string()),
                    name_span: local.span,
                    ty: local.ty,
                    ty_span: local.span,
                    into: None,
                }
            })
            .collect();
        Ok(FnSig {
            def: key.owner,
            parent: None,
            name: cfg.name.clone(),
            name_span: Span::default(),
            mods: ingot_syntax::ast::FnMods::default(),
            params,
            ret: cfg.ret,
            ret_span: Span::default(),
            error: cfg.error,
            error_span: None,
            from: Vec::new(),
            reduce: None,
            reduce_span: None,
            variadic: false,
            abi: None,
            has_body: true,
            span: Span::default(),
        })
    }

    fn collect_calls(
        &mut self,
        caller_key: &InstanceKey,
        cfg: &Cfg,
        term: &Terminator,
        queue: &mut Vec<InstanceKey>,
        seen: &mut BTreeSet<InstanceKey>,
    ) -> Result<(), BuildError> {
        let target = match term {
            Terminator::Call { target, .. } => target,
            _ => return Ok(()),
        };
        let caller = &caller_key.subst;
        match target {
            CallTarget::Fn { fn_ref, .. } => {
                if let Some((interface, ty)) = self.builtin_target(fn_ref, caller)? {
                    for key in comparison_element_keys(
                        &mut *self.program.decls,
                        self.program.sema,
                        interface,
                        ty,
                    ) {
                        self.add_instance(key, queue, seen)?;
                    }
                    return Ok(());
                }
                if matches!(
                    fn_ref.source,
                    ingot_sema::typed::FnSource::Interface {
                        via: ingot_sema::typed::InterfaceVia::Dyn,
                        ..
                    }
                ) {
                    return Ok(());
                }
                let def = fn_ref.def;
                if self.program.sema.def(def).kind == DefKind::ExternFn {
                    return Ok(());
                }
                let key = ingot_cfg::instances::callee_key(
                    self.program.sema,
                    &mut *self.program.decls,
                    fn_ref,
                    caller,
                )
                .ok_or_else(|| {
                    BuildError::Internal(format!(
                        "no instance for the call of `{}`",
                        self.program.sema.def(def).name
                    ))
                })?;
                self.add_instance(key, queue, seen)?;
            }
            CallTarget::Value { kind, callee, .. } => match kind {
                ValueKind::FnPtr | ValueKind::DynFn => {}
                ValueKind::Closure(e) => {
                    let key = InstanceKey {
                        owner: caller_key.owner,
                        body: InstanceBody::Closure(*e),
                        subst: caller.clone(),
                    };
                    self.add_instance(key, queue, seen)?;
                }
                ValueKind::Bound => {
                    let ty = self.operand_ty(cfg, callee, caller)?;
                    if let Some(key) =
                        closure_instance_key(self.program.sema, self.program.decls, ty)
                    {
                        self.add_instance(key, queue, seen)?;
                    }
                }
            },
        }
        Ok(())
    }

    /// The type of a value operand under a substitution.
    fn operand_ty(&mut self, cfg: &Cfg, op: &Operand, subst: &Subst) -> Result<TypeId, BuildError> {
        let ty = match op {
            Operand::Place(p, _) => match &p.base {
                PlaceBase::Local(l) => cfg.local(*l).ty,
                PlaceBase::Static(def) => match self.program.decls.item(*def) {
                    Some(ItemSig::Static(s)) => s.ty,
                    _ => {
                        return Err(BuildError::Internal(
                            "a place rooted at something that is not a static".to_string(),
                        ));
                    }
                },
            },
            Operand::Const(_) => {
                return Err(BuildError::Internal(
                    "a call through a constant value".to_string(),
                ));
            }
        };
        Ok(self.program.decls.instantiate(ty, subst))
    }

    fn def_package(&self, def: DefId) -> usize {
        let module = self.program.sema.def(def).module;
        self.program.sema.module(module).package.0 as usize
    }

    fn add_instance(
        &mut self,
        key: InstanceKey,
        queue: &mut Vec<InstanceKey>,
        seen: &mut BTreeSet<InstanceKey>,
    ) -> Result<(), BuildError> {
        if seen.insert(key.clone()) {
            // Phase 11a sees every checked package and runs its per-instance checks.
            // A checked-package key it did not report is a bug in the shared
            // discovery code, not a key for this walk to add silently.
            if self
                .program
                .checked_packages
                .contains(&self.def_package(key.owner))
                && !self.program.instances.iter().any(|i| i.key == key)
            {
                return Err(BuildError::Internal(format!(
                    "phase 11a did not discover the instance of `{}`",
                    self.program.sema.def(key.owner).name
                )));
            }
            let symbol = self.symbol_for_key(&key);
            self.symbols.insert(key.clone(), symbol);
            queue.push(key);
        }
        Ok(())
    }

    fn is_builtin_method(&self, def: DefId) -> bool {
        let Some(parent) = self.program.sema.def(def).parent else {
            return false;
        };
        [
            self.program.decls.builtins.eq,
            self.program.decls.builtins.ord,
            self.program.decls.builtins.hash,
            self.program.decls.builtins.format,
        ]
        .into_iter()
        .flatten()
        .any(|i| i == parent)
    }

    /// Whether a call is one of the generated built-in `Eq`/`Ord` methods, and for
    /// which concrete type. The caller's substitution is applied first.
    fn builtin_target(
        &mut self,
        fn_ref: &ingot_sema::typed::FnRef,
        caller: &Subst,
    ) -> Result<Option<(DefId, TypeId)>, BuildError> {
        if let Some((interface, ty)) =
            builtin_comparison_target(&mut *self.program.decls, fn_ref, caller)
        {
            self.builtin_requests.insert((interface, ty));
            return Ok(Some((interface, ty)));
        }
        let ingot_sema::typed::FnSource::Interface {
            interface,
            self_ty,
            via,
            ..
        } = &fn_ref.source
        else {
            return Ok(None);
        };
        if !matches!(
            via,
            ingot_sema::typed::InterfaceVia::Builtin | ingot_sema::typed::InterfaceVia::Bound
        ) {
            return Ok(None);
        }
        let ty = self.program.decls.instantiate(*self_ty, caller);
        if (Some(*interface) == self.program.decls.builtins.hash
            || Some(*interface) == self.program.decls.builtins.format)
            && (matches!(via, ingot_sema::typed::InterfaceVia::Builtin)
                || self.program.decls.builtin_implements(ty, *interface) == Tri::Yes)
        {
            return self.ni("the built-in `Hash`/`Format` (R5)");
        }
        Ok(None)
    }

    /// `main` of the root package.
    fn main_def(&self) -> Result<DefId, BuildError> {
        let package = self
            .program
            .sema
            .packages
            .get(self.program.checked)
            .ok_or_else(|| BuildError::Internal("the checked package is unknown".to_string()))?;
        let root = package.root.ok_or_else(|| {
            BuildError::NotImplemented("a package with no root module".to_string())
        })?;
        self.program
            .sema
            .module(root)
            .values
            .get("main")
            .copied()
            .ok_or(BuildError::NoEntryPoint)
    }

    fn find_main(&mut self) -> Result<(), BuildError> {
        let main = self.main_def()?;
        let key = InstanceKey {
            owner: main,
            body: InstanceBody::Item,
            subst: Subst::new(),
        };
        let symbol =
            self.symbols.get(&key).cloned().ok_or_else(|| {
                BuildError::Internal("phase 11a did not discover `main`".to_string())
            })?;
        self.main_symbol = Some(symbol);
        Ok(())
    }

    fn register_fn_types(&mut self, body: &Body) -> Result<(), BuildError> {
        self.subst = body.subst.clone();
        for p in &body.sig.params {
            self.type_info(p.ty)?;
        }
        self.type_info(body.sig.ret)?;
        if let Some(e) = body.sig.error {
            self.type_info(e)?;
        }
        for local in &body.cfg.locals {
            self.type_info(local.ty)?;
        }
        self.subst = Subst::new();
        Ok(())
    }

    fn finish(self) -> Result<String, BuildError> {
        let mut out = String::new();
        out.push_str("; Ingot back end\n");
        out.push_str("target triple = \"");
        out.push_str(target_triple(&self.target)?);
        out.push_str("\"\n\n");
        for (_, text) in order_type_defs(self.type_defs) {
            out.push_str(&text);
            out.push('\n');
        }
        if !out.ends_with("\n\n") {
            out.push('\n');
        }
        for global in &self.globals {
            out.push_str(global);
            out.push('\n');
        }
        for table in self.tables.values() {
            out.push_str(table);
            out.push('\n');
        }
        for site in &self.sites {
            out.push_str(site);
            out.push('\n');
        }
        out.push('\n');
        for decl in self.externs.values() {
            out.push_str(decl);
            out.push('\n');
        }
        out.push('\n');
        for function in &self.functions {
            out.push_str(function);
            out.push('\n');
        }
        out.push_str(
            "attributes #0 = { \"probe-stack\"=\"inline-asm\" \"stack-probe-size\"=\"16384\" }\n",
        );
        if !self.profile_meta.is_empty() {
            out.push('\n');
            for meta in &self.profile_meta {
                out.push_str(meta);
                out.push('\n');
            }
        }
        Ok(out)
    }
}

/// LLVM needs a named non-struct type before the type that contains it. Type ids are
/// handed out before their fields are visited, so emit a post-order of the textual
/// `%tN` references instead of trusting id order.
fn order_type_defs(defs: Vec<(usize, String)>) -> Vec<(usize, String)> {
    let by_id: BTreeMap<usize, String> = defs.into_iter().collect();
    let mut deps: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for (id, text) in &by_id {
        let mut found = Vec::new();
        let mut rest = text.as_str();
        while let Some(pos) = rest.find("%t") {
            rest = &rest[pos + 2..];
            let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
            if let Ok(n) = digits.parse::<usize>()
                && n != *id
                && by_id.contains_key(&n)
            {
                found.push(n);
            }
        }
        found.sort_unstable();
        found.dedup();
        deps.insert(*id, found);
    }
    fn visit(
        id: usize,
        by_id: &BTreeMap<usize, String>,
        deps: &BTreeMap<usize, Vec<usize>>,
        seen: &mut BTreeSet<usize>,
        out: &mut Vec<(usize, String)>,
    ) {
        if !seen.insert(id) {
            return;
        }
        if let Some(ds) = deps.get(&id) {
            for d in ds {
                visit(*d, by_id, deps, seen, out);
            }
        }
        if let Some(text) = by_id.get(&id) {
            out.push((id, text.clone()));
        }
    }
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for id in by_id.keys() {
        visit(*id, &by_id, &deps, &mut seen, &mut out);
    }
    out
}

fn target_triple(target: &str) -> Result<&'static str, BuildError> {
    match target {
        "aarch64-macos-m4" => Ok("arm64-apple-macosx14.0.0"),
        "x86_64-linux-znver4" => Ok("x86_64-unknown-linux-gnu"),
        "x86_64-windows-znver4" => Ok("x86_64-pc-windows-msvc"),
        other => Err(BuildError::Internal(format!(
            "no target triple for {other}"
        ))),
    }
}

// ------------------------------------------------------------------- types

#[derive(Clone, Debug)]
struct TypeInfo {
    /// The LLVM spelling: a literal type for scalars and pointers, `%tN` otherwise.
    name: String,
    shape: Shape,
}

#[derive(Clone, Debug)]
enum Shape {
    /// An integer, float, bool, void, or pointer. `name` is the literal type.
    Scalar,
    Aggregate {
        fields: Vec<FieldInfo>,
    },
    Array {
        elem: TypeId,
        len: u64,
    },
    Enum {
        tag: String,
        payload_offset: u64,
        variants: Vec<VariantInfo>,
        /// The null-pointer representation of `?&T`, `?[]T` and `?fn(..)`.
        niche: bool,
    },
    Slice,
}

#[derive(Clone, Debug)]
struct FieldInfo {
    offset: u64,
}

#[derive(Clone, Debug)]
struct VariantInfo {
    tag: i128,
    payload: Option<TypeId>,
}

fn align_type(align: u64) -> Option<String> {
    Some(match align {
        1 => return None,
        2 => "[0 x i16]".to_string(),
        4 => "[0 x i32]".to_string(),
        8 => "[0 x i64]".to_string(),
        16 => "[0 x <2 x i64>]".to_string(),
        _ => return None,
    })
}

fn int_for_size(size: u64) -> &'static str {
    match size {
        1 => "i8",
        2 => "i16",
        4 => "i32",
        _ => "i64",
    }
}

/// The storage of an enum's payload area. A payload that can hold a pointer is an
/// array of `i64` words, so a global initializer can put a `ptrtoint` there; every
/// other payload is opaque bytes.
fn payload_storage(size: u64, has_pointer: bool) -> Result<String, BuildError> {
    if has_pointer {
        if !size.is_multiple_of(8) {
            return Err(BuildError::Internal(format!(
                "a pointer-bearing payload of {size} bytes is not a whole number of words"
            )));
        }
        Ok(format!("[{} x i64]", size / 8))
    } else {
        Ok(format!("[{size} x i8]"))
    }
}

impl<'p> Codegen<'p> {
    fn resolve_ty(&mut self, ty: TypeId) -> TypeId {
        if self.subst.is_empty() {
            ty
        } else {
            self.program.decls.instantiate(ty, &self.subst.clone())
        }
    }

    fn llvm_ty(&mut self, ty: TypeId) -> Result<String, BuildError> {
        let ty = self.resolve_ty(ty);
        self.type_info(ty)?;
        Ok(self.types[&ty].name.clone())
    }

    fn type_info(&mut self, ty: TypeId) -> Result<(), BuildError> {
        let ty = self.resolve_ty(ty);
        if self.types.contains_key(&ty) {
            return Ok(());
        }
        let kind = self.program.decls.types.kind(ty).clone();
        match kind {
            TyKind::Int(i) => {
                let name = format!("i{}", i.bits());
                self.types.insert(
                    ty,
                    TypeInfo {
                        name,
                        shape: Shape::Scalar,
                    },
                );
                Ok(())
            }
            TyKind::Float(f) => {
                let name = if f == FloatTy::F32 { "float" } else { "double" }.to_string();
                self.types.insert(
                    ty,
                    TypeInfo {
                        name,
                        shape: Shape::Scalar,
                    },
                );
                Ok(())
            }
            TyKind::Bool => {
                self.types.insert(
                    ty,
                    TypeInfo {
                        name: "i1".to_string(),
                        shape: Shape::Scalar,
                    },
                );
                Ok(())
            }
            TyKind::Void | TyKind::Never => {
                self.types.insert(
                    ty,
                    TypeInfo {
                        name: "{}".to_string(),
                        shape: Shape::Scalar,
                    },
                );
                Ok(())
            }
            TyKind::Ref(..) | TyKind::RawPtr(..) | TyKind::FnPtr(_) => {
                self.types.insert(
                    ty,
                    TypeInfo {
                        name: "ptr".to_string(),
                        shape: Shape::Scalar,
                    },
                );
                Ok(())
            }
            TyKind::Slice(..) => {
                let (id, name) = self.new_named();
                let def = format!("{name} = type {{ ptr, i64 }}");
                self.type_defs.push((id, def));
                self.types.insert(
                    ty,
                    TypeInfo {
                        name,
                        shape: Shape::Slice,
                    },
                );
                Ok(())
            }
            TyKind::Dyn(..) | TyKind::DynFn(..) => {
                let (id, name) = self.new_named();
                let def = format!("{name} = type {{ ptr, ptr }}");
                self.type_defs.push((id, def));
                self.types.insert(
                    ty,
                    TypeInfo {
                        name,
                        shape: Shape::Slice,
                    },
                );
                Ok(())
            }
            TyKind::Adt(def, args) => {
                if Some(def) == self.program.decls.builtins.idx {
                    self.types.insert(
                        ty,
                        TypeInfo {
                            name: "i32".to_string(),
                            shape: Shape::Scalar,
                        },
                    );
                    return Ok(());
                }
                let (id, name) = self.new_named();
                let item = self.program.decls.item(def).cloned().ok_or_else(|| {
                    BuildError::Internal(format!(
                        "no signature for a type: {}",
                        self.program.sema.def(def).name
                    ))
                })?;
                let field_subst = self.program.decls.subst_for(def, &args);
                match item {
                    ItemSig::Struct(sig) => {
                        let layout = self.layout(ty)?;
                        let mut fields = Vec::new();
                        let mut ll_fields = Vec::new();
                        let mut offset = 0u64;
                        for (i, f) in sig.fields.iter().enumerate() {
                            let fl = layout.fields.get(i).cloned().ok_or_else(|| {
                                BuildError::Internal(format!(
                                    "no layout entry for the field `{}`",
                                    f.name
                                ))
                            })?;
                            let fty = self.program.decls.instantiate(f.ty, &field_subst);
                            let ll = self.llvm_ty(fty)?;
                            fields.push(FieldInfo { offset: fl.offset });
                            push_pad(&mut ll_fields, &mut offset, fl.offset);
                            ll_fields.push(ll);
                            offset = offset.max(fl.offset.saturating_add(fl.size));
                        }
                        let tail = layout.size.saturating_sub(offset);
                        if tail > 0 {
                            ll_fields.push(format!("[{tail} x i8]"));
                        }
                        if let Some(a) = align_type(layout.align) {
                            ll_fields.push(a);
                        }
                        let ll = if ll_fields.is_empty() {
                            "{}".to_string()
                        } else {
                            format!("{{ {} }}", ll_fields.join(", "))
                        };
                        let def = format!("{name} = type {ll}");
                        self.type_defs.push((id, def));
                        self.types.insert(
                            ty,
                            TypeInfo {
                                name,
                                shape: Shape::Aggregate { fields },
                            },
                        );
                        Ok(())
                    }
                    ItemSig::Enum(sig) => {
                        let layout = self.layout(ty)?;
                        let (tag_size, payload_offset) = match layout.kind {
                            LayoutKind::Enum {
                                tag_size,
                                payload_offset,
                            } => (tag_size, payload_offset),
                            _ => (
                                u64::from(sig.tag_ty.bits() / 8),
                                u64::from(sig.tag_ty.bits() / 8),
                            ),
                        };
                        let mut variants = Vec::new();
                        let mut max_size = 0u64;
                        let mut max_align = 1u64;
                        let mut payload_has_pointer = false;
                        for v in &sig.variants {
                            let payload = v
                                .payload
                                .map(|p| self.program.decls.instantiate(p, &field_subst));
                            variants.push(VariantInfo {
                                tag: v.tag,
                                payload,
                            });
                            if let Some(p) = payload {
                                let l = self.layout(p)?;
                                max_size = max_size.max(l.size);
                                max_align = max_align.max(l.align);
                                payload_has_pointer |=
                                    self.type_has_pointer(p, &mut BTreeSet::new());
                            }
                        }
                        let tag = int_for_size(tag_size).to_string();
                        let mut ll_fields = vec![tag.clone()];
                        let mut offset = tag_size;
                        push_pad(&mut ll_fields, &mut offset, payload_offset);
                        if let Some(a) = align_type(max_align) {
                            ll_fields.push(a);
                        }
                        let payload_size = layout.size.saturating_sub(payload_offset);
                        ll_fields.push(payload_storage(payload_size, payload_has_pointer)?);
                        let ll = format!("{{ {} }}", ll_fields.join(", "));
                        let def = format!("{name} = type {ll}");
                        self.type_defs.push((id, def));
                        self.types.insert(
                            ty,
                            TypeInfo {
                                name,
                                shape: Shape::Enum {
                                    tag,
                                    payload_offset,
                                    variants,
                                    niche: false,
                                },
                            },
                        );
                        Ok(())
                    }
                    _ => self.ni("the layout of a built-in type with no declaration".to_string()),
                }
            }
            TyKind::Optional(inner) => {
                let layout = self.layout(ty)?;
                match layout.kind {
                    LayoutKind::Niche => {
                        let inner_ll = self.llvm_ty(inner)?;
                        self.types.insert(
                            ty,
                            TypeInfo {
                                name: inner_ll,
                                shape: Shape::Enum {
                                    tag: "ptr".to_string(),
                                    payload_offset: 0,
                                    variants: vec![
                                        VariantInfo {
                                            tag: 0,
                                            payload: None,
                                        },
                                        VariantInfo {
                                            tag: 1,
                                            payload: Some(inner),
                                        },
                                    ],
                                    niche: true,
                                },
                            },
                        );
                        Ok(())
                    }
                    _ => {
                        let (id, name) = self.new_named();
                        let (tag_size, payload_offset) = match layout.kind {
                            LayoutKind::Enum {
                                tag_size,
                                payload_offset,
                            } => (tag_size, payload_offset),
                            other => {
                                return Err(BuildError::Internal(format!(
                                    "an optional has layout {other:?}, not an enum"
                                )));
                            }
                        };
                        let pl = self.layout(inner)?;
                        let tag = int_for_size(tag_size).to_string();
                        let mut ll_fields = vec![tag.clone()];
                        let mut offset = tag_size;
                        push_pad(&mut ll_fields, &mut offset, payload_offset);
                        if let Some(a) = align_type(pl.align) {
                            ll_fields.push(a);
                        }
                        let payload_size = layout.size.saturating_sub(payload_offset);
                        let has_pointer = self.type_has_pointer(inner, &mut BTreeSet::new());
                        ll_fields.push(payload_storage(payload_size, has_pointer)?);
                        let ll = format!("{{ {} }}", ll_fields.join(", "));
                        self.type_defs.push((id, format!("{name} = type {ll}")));
                        self.types.insert(
                            ty,
                            TypeInfo {
                                name,
                                shape: Shape::Enum {
                                    tag,
                                    payload_offset,
                                    variants: vec![
                                        VariantInfo {
                                            tag: 0,
                                            payload: None,
                                        },
                                        VariantInfo {
                                            tag: 1,
                                            payload: Some(inner),
                                        },
                                    ],
                                    niche: false,
                                },
                            },
                        );
                        Ok(())
                    }
                }
            }
            TyKind::Result(ok, err) => {
                let layout = self.layout(ty)?;
                let (id, name) = self.new_named();
                let (tag_size, payload_offset) = match layout.kind {
                    LayoutKind::Enum {
                        tag_size,
                        payload_offset,
                    } => (tag_size, payload_offset),
                    other => {
                        return Err(BuildError::Internal(format!(
                            "a result has layout {other:?}, not an enum"
                        )));
                    }
                };
                let al = self.layout(ok)?;
                let bl = self.layout(err)?;
                let align = al.align.max(bl.align);
                let tag = int_for_size(tag_size).to_string();
                let mut ll_fields = vec![tag.clone()];
                let mut offset = tag_size;
                push_pad(&mut ll_fields, &mut offset, payload_offset);
                if let Some(a) = align_type(align) {
                    ll_fields.push(a);
                }
                let payload_size = layout.size.saturating_sub(payload_offset);
                let has_pointer = self.type_has_pointer(ok, &mut BTreeSet::new())
                    || self.type_has_pointer(err, &mut BTreeSet::new());
                ll_fields.push(payload_storage(payload_size, has_pointer)?);
                let ll = format!("{{ {} }}", ll_fields.join(", "));
                self.type_defs.push((id, format!("{name} = type {ll}")));
                self.types.insert(
                    ty,
                    TypeInfo {
                        name,
                        shape: Shape::Enum {
                            tag,
                            payload_offset,
                            variants: vec![
                                VariantInfo {
                                    tag: 0,
                                    payload: Some(ok),
                                },
                                VariantInfo {
                                    tag: 1,
                                    payload: Some(err),
                                },
                            ],
                            niche: false,
                        },
                    },
                );
                Ok(())
            }
            TyKind::Tuple(ts) => {
                let layout = self.layout(ty)?;
                let (id, name) = self.new_named();
                let mut fields = Vec::new();
                let mut ll_fields = Vec::new();
                let mut offset = 0u64;
                for (i, t) in ts.iter().enumerate() {
                    let fl = layout.fields.get(i).cloned().ok_or_else(|| {
                        BuildError::Internal(format!("no layout entry for tuple field {i}"))
                    })?;
                    let ll = self.llvm_ty(*t)?;
                    fields.push(FieldInfo { offset: fl.offset });
                    push_pad(&mut ll_fields, &mut offset, fl.offset);
                    ll_fields.push(ll);
                    offset = offset.max(fl.offset.saturating_add(fl.size));
                }
                let tail = layout.size.saturating_sub(offset);
                if tail > 0 {
                    ll_fields.push(format!("[{tail} x i8]"));
                }
                if let Some(a) = align_type(layout.align) {
                    ll_fields.push(a);
                }
                let ll = if ll_fields.is_empty() {
                    "{}".to_string()
                } else {
                    format!("{{ {} }}", ll_fields.join(", "))
                };
                self.type_defs.push((id, format!("{name} = type {ll}")));
                self.types.insert(
                    ty,
                    TypeInfo {
                        name,
                        shape: Shape::Aggregate { fields },
                    },
                );
                Ok(())
            }
            TyKind::Array(elem, n) => {
                let layout = self.layout(ty)?;
                let (id, name) = self.new_named();
                let len = self
                    .program
                    .decls
                    .types
                    .const_arg_value(n)
                    .and_then(|v| match v {
                        ConstValue::Int { value, .. } if value >= 0 => Some(value as u64),
                        _ => None,
                    });
                let Some(len) = len else {
                    return self.ni("an array whose length has no value");
                };
                let ll = self.llvm_ty(elem)?;
                let _ = layout;
                let def = format!("{name} = type [{len} x {ll}]");
                self.type_defs.push((id, def));
                self.types.insert(
                    ty,
                    TypeInfo {
                        name,
                        shape: Shape::Array { elem, len },
                    },
                );
                Ok(())
            }
            TyKind::Vector(..) | TyKind::Mask(_) => self.ni("vectors (B4)"),
            TyKind::Closure(def, index, args) => {
                let layout = self.layout(ty)?;
                let field_tys = self
                    .program
                    .decls
                    .closure_field_tys(def, index, &args)
                    .ok_or_else(|| {
                        BuildError::Internal(format!(
                            "no capture record for the closure {} of `{}`",
                            index,
                            self.program.sema.def(def).name
                        ))
                    })?;
                let (id, name) = self.new_named();
                let mut fields = Vec::new();
                let mut ll_fields = Vec::new();
                let mut offset = 0u64;
                for (i, fty) in field_tys.iter().enumerate() {
                    let fl = layout.fields.get(i).cloned().ok_or_else(|| {
                        BuildError::Internal(format!("no layout entry for closure field {i}"))
                    })?;
                    let ll = self.llvm_ty(*fty)?;
                    fields.push(FieldInfo { offset: fl.offset });
                    push_pad(&mut ll_fields, &mut offset, fl.offset);
                    ll_fields.push(ll);
                    offset = offset.max(fl.offset.saturating_add(fl.size));
                }
                let tail = layout.size.saturating_sub(offset);
                if tail > 0 {
                    ll_fields.push(format!("[{tail} x i8]"));
                }
                if let Some(a) = align_type(layout.align) {
                    ll_fields.push(a);
                }
                let ll = if ll_fields.is_empty() {
                    "{}".to_string()
                } else {
                    format!("{{ {} }}", ll_fields.join(", "))
                };
                self.type_defs.push((id, format!("{name} = type {ll}")));
                self.types.insert(
                    ty,
                    TypeInfo {
                        name,
                        shape: Shape::Aggregate { fields },
                    },
                );
                Ok(())
            }
            // Every type reaching the back end has been substituted (B2), so a parameter or
            // projection here is a bug in whoever built the substitution.
            TyKind::Param(_) | TyKind::Projection(..) => Err(BuildError::Internal(format!(
                "the type `{}` reached the back end unsubstituted",
                self.type_display(ty)
            ))),
            TyKind::Poison => self.ni("a poisoned type"),
        }
    }

    fn new_named(&mut self) -> (usize, String) {
        let id = self.next_type;
        self.next_type += 1;
        (id, format!("%t{id}"))
    }

    fn layout(&mut self, ty: TypeId) -> Result<Layout, BuildError> {
        let ty = self.resolve_ty(ty);
        self.program
            .decls
            .layout_of(ty)
            .map_err(|e| BuildError::NotImplemented(format!("layout of a type: {e:?}")))
    }
}

fn push_pad(fields: &mut Vec<String>, offset: &mut u64, to: u64) {
    if to > *offset {
        fields.push(format!("[{} x i8]", to - *offset));
        *offset = to;
    }
}

// ------------------------------------------------------------- function generation

struct OutBlock {
    label: String,
    lines: Vec<String>,
}

struct FnGen<'a, 'p> {
    cg: &'a mut Codegen<'p>,
    body: &'a Body,
    blocks: Vec<OutBlock>,
    cur: usize,
    cold: Vec<OutBlock>,
    next_tmp: usize,
    next_label: usize,
    ret_ll: String,
    /// The B2 return convention; `Plain` for every other result.
    ret_abi: RetAbi,
    /// The LLVM parameter types of the definition (slices are two parameters).
    abi_params: Vec<String>,
    /// Allocations made while lowering. They are emitted in the entry block so an
    /// aggregate in a loop does not grow the frame on every iteration at `-O0`.
    entry_allocas: Vec<String>,
    /// The locals that hold a known variant of an enum at this point of the current
    /// block, by the variant's index. Atomic orders are read from it (`order_arg`).
    known_variants: BTreeMap<ingot_cfg::model::Local, usize>,
}

impl<'p> Codegen<'p> {
    fn emit_body(&mut self, body: &Body) -> Result<(), BuildError> {
        if body.sig.mods.export {
            self.check_c_abi(&body.sig)?;
        }
        self.subst = body.subst.clone();
        // A `test` block is `fn(&TestEnv) -> i32` (tools.md §3): 0 when it ends
        // normally, 1 after a `try` printed its error (`emit_test_return`).
        let (ret_abi, abi_params) = if self.is_test_body(body) {
            (
                RetAbi::Plain {
                    ll: "i32".to_string(),
                },
                vec!["ptr".to_string()],
            )
        } else {
            (self.ret_abi(&body.sig)?, self.abi_params(&body.sig)?)
        };
        let ret_ll = ret_abi.ret_ll();
        let mut generator = FnGen {
            cg: self,
            body,
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
        };
        generator.generate()?;
        let text = generator.render();
        drop(generator);
        self.subst = Subst::new();
        self.functions.push(text);
        Ok(())
    }

    /// Whether a body is a `test` block.
    fn is_test_body(&self, body: &Body) -> bool {
        body.cfg.key.body == ingot_cfg::BodyKey::Item
            && matches!(
                self.program.sema.def(body.cfg.key.owner).kind,
                DefKind::Test { .. }
            )
    }

    fn fn_ret_ll(&mut self, body: &Body) -> Result<String, BuildError> {
        self.plain_ret_ll(&body.sig)
    }

    fn plain_ret_ll(&mut self, sig: &FnSig) -> Result<String, BuildError> {
        self.plain_ret_ll_for(sig.ret)
    }

    fn plain_ret_ll_for(&mut self, ret: TypeId) -> Result<String, BuildError> {
        if ret == self.program.decls.types.void || ret == self.program.decls.types.never {
            return Ok("void".to_string());
        }
        if let Some(tag) = self.tag_only_enum(ret) {
            return Ok(tag);
        }
        self.llvm_ty(ret)
    }

    /// The return convention of a signature (B2). A fallible `T ! E`, and a `?T` with
    /// the tag-and-payload layout, return `{ tag, i64 }` when the payload area fits one
    /// register; otherwise the caller passes a hidden result pointer and only the tag
    /// is returned. A niche `?T` keeps its null-pointer representation.
    fn ret_abi(&mut self, sig: &FnSig) -> Result<RetAbi, BuildError> {
        self.ret_abi_for(sig.ret, sig.error)
    }

    /// `ret_abi` for a signature shape that has no `FnSig`, such as a `fn` pointer.
    fn ret_abi_for(&mut self, ret: TypeId, error: Option<TypeId>) -> Result<RetAbi, BuildError> {
        if let Some(err) = error {
            let result_ty = self.program.decls.types.intern(TyKind::Result(ret, err));
            let layout = self.layout(result_ty)?;
            return Ok(RetAbi::Result(self.result_abi(
                &layout,
                ret,
                Some(err),
                false,
            )?));
        }
        if let TyKind::Optional(inner) = self.program.decls.types.kind(ret).clone() {
            let layout = self.layout(ret)?;
            if matches!(layout.kind, LayoutKind::Niche) {
                return Ok(RetAbi::Plain {
                    ll: self.llvm_ty(ret)?,
                });
            }
            return Ok(RetAbi::Result(self.result_abi(&layout, inner, None, true)?));
        }
        Ok(RetAbi::Plain {
            ll: self.plain_ret_ll_for(ret)?,
        })
    }

    fn result_abi(
        &self,
        layout: &Layout,
        payload_ty: TypeId,
        error_ty: Option<TypeId>,
        optional: bool,
    ) -> Result<ResultAbi, BuildError> {
        let (tag_size, payload_offset) = match layout.kind.clone() {
            LayoutKind::Enum {
                tag_size,
                payload_offset,
            } => (tag_size, payload_offset),
            other => {
                return Err(BuildError::Internal(format!(
                    "a result has layout {other:?}, not an enum"
                )));
            }
        };
        let payload_area = layout.size.saturating_sub(payload_offset);
        Ok(ResultAbi {
            tag_ll: int_for_size(tag_size).to_string(),
            payload_offset,
            payload_area,
            payload_align: layout.align,
            payload_ty,
            error_ty,
            optional,
            ptr: payload_area > 8,
        })
    }

    fn const_expr_ty(&mut self, id: ingot_sema::ty::ConstExprId) -> Result<TypeId, BuildError> {
        use ingot_sema::ty::ConstExpr;
        let ty = match self.program.decls.types.const_expr(id).clone() {
            ConstExpr::Int { ty, .. } => self.program.decls.types.int(ty),
            ConstExpr::Bool(_) => self.program.decls.types.bool_,
            ConstExpr::Param(p) => self
                .program
                .decls
                .param_const_ty
                .get(&p)
                .copied()
                .ok_or_else(|| {
                    BuildError::Internal("a constant parameter with no type".to_string())
                })?,
            ConstExpr::Unary { expr, .. } => self.const_expr_ty(expr)?,
            ConstExpr::Binary { op, lhs, .. } => {
                if matches!(
                    op,
                    BinOp::Eq
                        | BinOp::Ne
                        | BinOp::Lt
                        | BinOp::Le
                        | BinOp::Gt
                        | BinOp::Ge
                        | BinOp::And
                        | BinOp::Or
                ) {
                    self.program.decls.types.bool_
                } else {
                    self.const_expr_ty(lhs)?
                }
            }
            ConstExpr::Cast { to, .. } => self.program.decls.types.int(to),
            ConstExpr::Layout { .. } => self.program.decls.types.i64_,
        };
        Ok(ty)
    }

    fn const_expr_value(
        &mut self,
        id: ingot_sema::ty::ConstExprId,
    ) -> Result<Option<CeVal>, BuildError> {
        use ingot_sema::ty::ConstExpr;
        let value = match self.program.decls.types.const_expr(id).clone() {
            ConstExpr::Int { ty, value } => Some(CeVal::Int(ty, value)),
            ConstExpr::Bool(b) => Some(CeVal::Bool(b)),
            ConstExpr::Param(p) => match self.subst.get(p).cloned() {
                Some(GenericArg::Const(ConstArg::Value(ConstValue::Int { ty, value }))) => {
                    Some(CeVal::Int(ty, value))
                }
                Some(GenericArg::Const(ConstArg::Value(ConstValue::Bool(b)))) => {
                    Some(CeVal::Bool(b))
                }
                Some(GenericArg::Const(ConstArg::Expr(inner))) => self.const_expr_value(inner)?,
                _ => None,
            },
            ConstExpr::Unary { op, expr } => match (op, self.const_expr_value(expr)?) {
                (ingot_syntax::ast::UnOp::Not, Some(CeVal::Bool(b))) => Some(CeVal::Bool(!b)),
                (op, Some(CeVal::Int(ty, a))) => ingot_sema::consteval::int_unary(op, ty, a)
                    .ok()
                    .map(|v| CeVal::Int(ty, v)),
                _ => None,
            },
            ConstExpr::Binary { op, lhs, rhs } => {
                let a = self.const_expr_value(lhs)?;
                let b = self.const_expr_value(rhs)?;
                match (a, b) {
                    (Some(CeVal::Int(ty, a)), Some(CeVal::Int(_, b))) => {
                        let value = match op {
                            BinOp::Eq => Some(i128::from(a == b)),
                            BinOp::Ne => Some(i128::from(a != b)),
                            BinOp::Lt => Some(i128::from(if ty.signed() {
                                a < b
                            } else {
                                (a as u128) < (b as u128)
                            })),
                            BinOp::Le => Some(i128::from(if ty.signed() {
                                a <= b
                            } else {
                                (a as u128) <= (b as u128)
                            })),
                            BinOp::Gt => Some(i128::from(if ty.signed() {
                                a > b
                            } else {
                                (a as u128) > (b as u128)
                            })),
                            BinOp::Ge => Some(i128::from(if ty.signed() {
                                a >= b
                            } else {
                                (a as u128) >= (b as u128)
                            })),
                            _ => ingot_sema::consteval::int_binary(op, ty, a, b).ok(),
                        };
                        match value {
                            Some(v)
                                if matches!(
                                    op,
                                    BinOp::Eq
                                        | BinOp::Ne
                                        | BinOp::Lt
                                        | BinOp::Le
                                        | BinOp::Gt
                                        | BinOp::Ge
                                ) =>
                            {
                                Some(CeVal::Bool(v != 0))
                            }
                            Some(v) => Some(CeVal::Int(ty, v)),
                            None => None,
                        }
                    }
                    (Some(CeVal::Bool(a)), Some(CeVal::Bool(b))) => match op {
                        BinOp::And => Some(CeVal::Bool(a && b)),
                        BinOp::Or => Some(CeVal::Bool(a || b)),
                        BinOp::Eq => Some(CeVal::Bool(a == b)),
                        BinOp::Ne => Some(CeVal::Bool(a != b)),
                        _ => None,
                    },
                    _ => None,
                }
            }
            ConstExpr::Cast { to, expr } => match self.const_expr_value(expr)? {
                Some(CeVal::Int(from, value)) if ingot_sema::consteval::lossless(from, to) => {
                    Some(CeVal::Int(to, value))
                }
                _ => None,
            },
            ConstExpr::Layout { size, ty } => {
                let layout = self.layout(ty)?;
                Some(CeVal::Int(
                    IntTy::I64,
                    i128::from(if size { layout.size } else { layout.align }),
                ))
            }
        };
        Ok(value)
    }

    fn const_expr_ll(&mut self, value: &CeVal) -> Result<String, BuildError> {
        Ok(match value {
            CeVal::Int(ty, _) => format!("i{}", ty.bits()),
            CeVal::Bool(_) => "i1".to_string(),
        })
    }

    /// The platform C ABI of an `extern` declaration or an `export fn` is B6 work
    /// unless every parameter and the result is a scalar, a pointer or a payload-free
    /// enum. Slices, `?T`, structs, tuples, arrays and `&dyn` are refused, never
    /// emitted with the internal convention.
    fn check_c_abi(&mut self, sig: &FnSig) -> Result<(), BuildError> {
        for p in &sig.params {
            if !self.c_abi_value(p.ty)? {
                return self.ni("the C ABI for aggregates (B6)");
            }
        }
        if !self.c_abi_value(sig.ret)? {
            return self.ni("the C ABI for aggregates (B6)");
        }
        Ok(())
    }

    fn c_abi_value(&mut self, ty: TypeId) -> Result<bool, BuildError> {
        let ty = self.resolve_ty(ty);
        let kind = self.program.decls.types.kind(ty).clone();
        Ok(match kind {
            TyKind::Int(_)
            | TyKind::Float(_)
            | TyKind::Bool
            | TyKind::Void
            | TyKind::Never
            | TyKind::Ref(..)
            | TyKind::RawPtr(..)
            | TyKind::FnPtr(_) => true,
            TyKind::Adt(def, _) => {
                Some(def) == self.program.decls.builtins.idx || self.tag_only_enum(ty).is_some()
            }
            _ => false,
        })
    }

    fn abi_params(&mut self, sig: &FnSig) -> Result<Vec<String>, BuildError> {
        let tys: Vec<TypeId> = sig.params.iter().map(|p| p.ty).collect();
        self.abi_params_for(&tys)
    }

    fn abi_params_for(&mut self, tys: &[TypeId]) -> Result<Vec<String>, BuildError> {
        let mut out = Vec::new();
        for ty in tys {
            let pty = self.resolve_ty(*ty);
            match self.program.decls.types.kind(pty) {
                TyKind::Slice(..) => {
                    out.push("ptr".to_string());
                    out.push("i64".to_string());
                }
                TyKind::Dyn(..) | TyKind::DynFn(..) => {
                    out.push("ptr".to_string());
                    out.push("ptr".to_string());
                }
                TyKind::Void | TyKind::Never => {}
                TyKind::Int(_)
                | TyKind::Float(_)
                | TyKind::Bool
                | TyKind::Ref(..)
                | TyKind::RawPtr(..)
                | TyKind::FnPtr(_)
                | TyKind::Adt(..)
                | TyKind::Tuple(_)
                | TyKind::Array(..)
                | TyKind::Optional(_)
                | TyKind::Result(..)
                | TyKind::Closure(..) => {
                    if matches!(self.program.decls.types.kind(pty), TyKind::Adt(d, _) if Some(*d) == self.program.decls.builtins.idx)
                    {
                        out.push("i32".to_string());
                    } else if let Some(tag) = self.tag_only_enum(pty) {
                        out.push(tag);
                    } else if self.is_scalar(pty)? {
                        out.push(self.llvm_ty(pty)?);
                    } else if let Some((size, _)) = self.aggregate_size(pty)? {
                        if size > 16 {
                            out.push("ptr".to_string());
                        } else if size > 8 {
                            out.push("i64".to_string());
                            out.push("i64".to_string());
                        } else if size > 0 {
                            out.push("i64".to_string());
                        }
                    } else {
                        return self.ni(format!("parameter type {}", self.type_display(pty)));
                    }
                }
                _ => return self.ni(format!("parameter type {}", self.type_display(pty))),
            }
        }
        Ok(out)
    }

    /// Whether a value of `ty` can contain a pointer and therefore needs a typed
    /// global initializer rather than a byte array.
    fn type_has_pointer(&mut self, ty: TypeId, seen: &mut BTreeSet<TypeId>) -> bool {
        let ty = self.resolve_ty(ty);
        if !seen.insert(ty) {
            return false;
        }
        let kind = self.program.decls.types.kind(ty).clone();
        match kind {
            TyKind::Ref(..)
            | TyKind::RawPtr(..)
            | TyKind::FnPtr(_)
            | TyKind::Slice(..)
            | TyKind::Dyn(..)
            | TyKind::DynFn(..) => true,
            TyKind::Adt(def, args) => {
                let subst = self.program.decls.subst_for(def, &args);
                let tys: Vec<TypeId> = match self.program.decls.item(def).cloned() {
                    Some(ItemSig::Struct(sig)) => sig
                        .fields
                        .iter()
                        .map(|f| self.program.decls.instantiate(f.ty, &subst))
                        .collect(),
                    Some(ItemSig::Enum(sig)) => sig
                        .variants
                        .iter()
                        .filter_map(|v| v.payload)
                        .map(|p| self.program.decls.instantiate(p, &subst))
                        .collect(),
                    _ => Vec::new(),
                };
                tys.into_iter().any(|t| self.type_has_pointer(t, seen))
            }
            TyKind::Array(elem, _) | TyKind::Optional(elem) => self.type_has_pointer(elem, seen),
            TyKind::Result(a, b) => {
                self.type_has_pointer(a, seen) || self.type_has_pointer(b, seen)
            }
            TyKind::Tuple(ts) => ts.into_iter().any(|t| self.type_has_pointer(t, seen)),
            TyKind::Closure(def, index, args) => self
                .program
                .decls
                .closure_field_tys(def, index, &args)
                .is_some_and(|fields| fields.into_iter().any(|t| self.type_has_pointer(t, seen))),
            _ => false,
        }
    }

    fn is_scalar(&mut self, ty: TypeId) -> Result<bool, BuildError> {
        let ty = self.resolve_ty(ty);
        self.type_info(ty)?;
        Ok(matches!(
            self.types[&ty].shape,
            Shape::Scalar | Shape::Enum { niche: true, .. }
        ))
    }

    /// `Some((size, align))` for a by-value aggregate that is not a scalar, a
    /// slice or a payload-free enum.
    fn aggregate_size(&mut self, ty: TypeId) -> Result<Option<(u64, u64)>, BuildError> {
        let ty = self.resolve_ty(ty);
        self.type_info(ty)?;
        let aggregate = matches!(
            self.types[&ty].shape,
            Shape::Aggregate { .. } | Shape::Array { .. } | Shape::Enum { niche: false, .. }
        );
        if !aggregate {
            return Ok(None);
        }
        let layout = self.layout(ty)?;
        Ok(Some((layout.size, layout.align)))
    }

    /// A payload-free enum crosses our own calling convention as its tag.
    fn tag_only_enum(&mut self, ty: TypeId) -> Option<String> {
        let ty = self.resolve_ty(ty);
        let TyKind::Adt(def, _) = self.program.decls.types.kind(ty) else {
            return None;
        };
        let sig = self.program.decls.enum_sig(*def)?;
        if sig.variants.iter().any(|v| v.payload.is_some()) {
            return None;
        }
        Some(int_for_size(u64::from(sig.tag_ty.bits() / 8)).to_string())
    }

    fn local_layout(&mut self, ty: TypeId) -> Result<(u64, u64), BuildError> {
        let ty = self.resolve_ty(ty);
        let l = self.layout(ty)?;
        Ok((l.size, l.align))
    }
}

impl<'a, 'p> FnGen<'a, 'p> {
    fn emit(&mut self, line: impl Into<String>) {
        self.blocks[self.cur].lines.push(line.into());
    }

    fn new_block(&mut self, label: String) -> usize {
        self.blocks.push(OutBlock {
            label,
            lines: Vec::new(),
        });
        self.blocks.len() - 1
    }

    fn fresh_label(&mut self, prefix: &str) -> String {
        let n = self.next_label;
        self.next_label += 1;
        format!("{prefix}{n}")
    }

    fn fresh_tmp(&mut self) -> String {
        let n = self.next_tmp;
        self.next_tmp += 1;
        format!("%tmp{n}")
    }

    fn alloca(&mut self, ll: &str, align: u64) -> String {
        let tmp = self.fresh_tmp();
        self.entry_allocas
            .push(format!("{tmp} = alloca {ll}, align {align}"));
        tmp
    }

    /// A 16-byte slot for an aggregate split into one or two `i64` arguments. The
    /// slot is zeroed first: the caller reads whole `i64`s, and bytes past the
    /// value's size must not be poison. It is 8-aligned because those reads are.
    fn split_slot(&mut self, ll: &str, value: &str, align: u64) -> String {
        let tmp = self.alloca("[16 x i8]", align.max(8));
        self.emit(format!("store [16 x i8] zeroinitializer, ptr {tmp}"));
        self.emit(format!("store {ll} {value}, ptr {tmp}"));
        tmp
    }

    fn label_of(&self, block: Block) -> String {
        format!("bb{block}")
    }

    fn generate(&mut self) -> Result<(), BuildError> {
        let cfg = &self.body.cfg;
        for i in 0..cfg.blocks.len() {
            self.blocks.push(OutBlock {
                label: format!("bb{i}"),
                lines: Vec::new(),
            });
        }
        if cfg.blocks.is_empty() {
            self.blocks.push(OutBlock {
                label: "bb0".to_string(),
                lines: Vec::new(),
            });
        }
        self.cur = cfg.entry as usize;
        // Allocations and incoming parameters live at the top of the entry block.
        self.emit_locals()?;
        self.emit_params()?;
        for (i, block) in cfg.blocks.iter().enumerate() {
            self.cur = i;
            self.emit_block(block)?;
        }
        Ok(())
    }

    fn emit_locals(&mut self) -> Result<(), BuildError> {
        let locals = self.body.cfg.locals.clone();
        for (i, local) in locals.iter().enumerate() {
            let ty = self.cg.resolve_ty(local.ty);
            if ty == self.cg.program.decls.types.void || ty == self.cg.program.decls.types.never {
                continue;
            }
            let ll = self.cg.llvm_ty(ty)?;
            let (size, align) = self.cg.local_layout(ty)?;
            let split = self.cg.aggregate_size(ty)?.is_some() && size > 0 && size <= 16;
            let slot = if split { "[16 x i8]".to_string() } else { ll };
            let slot_align = if split { align.max(8) } else { align };
            self.emit(format!("%l{i} = alloca {slot}, align {slot_align}"));
            if split {
                self.emit(format!("store [16 x i8] zeroinitializer, ptr %l{i}"));
            }
        }
        Ok(())
    }

    fn emit_params(&mut self) -> Result<(), BuildError> {
        let params = self.body.cfg.params.clone();
        let mut arg = 0usize;
        for local in &params {
            let ty = self.cg.resolve_ty(self.body.cfg.local(*local).ty);
            if ty == self.cg.program.decls.types.void || ty == self.cg.program.decls.types.never {
                continue;
            }
            match self.cg.program.decls.types.kind(ty).clone() {
                TyKind::Slice(..) => {
                    let ptr = format!("%arg{arg}");
                    let len = format!("%arg{}", arg + 1);
                    arg += 2;
                    let ll = self.cg.llvm_ty(ty)?;
                    let a = self.fresh_tmp();
                    let b = self.fresh_tmp();
                    self.emit(format!(
                        "{a} = insertvalue {ll} zeroinitializer, ptr {ptr}, 0"
                    ));
                    self.emit(format!("{b} = insertvalue {ll} {a}, i64 {len}, 1"));
                    self.emit(format!("store {ll} {b}, ptr %l{local}"));
                }
                TyKind::Dyn(..) | TyKind::DynFn(..) => {
                    let data = format!("%arg{arg}");
                    let table = format!("%arg{}", arg + 1);
                    arg += 2;
                    let ll = self.cg.llvm_ty(ty)?;
                    let a = self.fresh_tmp();
                    let b = self.fresh_tmp();
                    self.emit(format!(
                        "{a} = insertvalue {ll} zeroinitializer, ptr {data}, 0"
                    ));
                    self.emit(format!("{b} = insertvalue {ll} {a}, ptr {table}, 1"));
                    self.emit(format!("store {ll} {b}, ptr %l{local}"));
                }
                _ => {
                    let ll = self.cg.llvm_ty(ty)?;
                    if let Some(tag) = self.cg.tag_only_enum(ty) {
                        let a = format!("%arg{arg}");
                        arg += 1;
                        let v = self.fresh_tmp();
                        self.emit(format!(
                            "{v} = insertvalue {ll} zeroinitializer, {tag} {a}, 0"
                        ));
                        self.emit(format!("store {ll} {v}, ptr %l{local}"));
                    } else if let Some((size, _)) = self.cg.aggregate_size(ty)? {
                        if size > 16 {
                            let a = format!("%arg{arg}");
                            arg += 1;
                            let v = self.fresh_tmp();
                            self.emit(format!("{v} = load {ll}, ptr {a}"));
                            self.emit(format!("store {ll} {v}, ptr %l{local}"));
                        } else if size > 0 {
                            let first = format!("%arg{arg}");
                            arg += 1;
                            self.emit(format!("store i64 {first}, ptr %l{local}"));
                            if size > 8 {
                                let second = format!("%arg{arg}");
                                arg += 1;
                                let p = self.fresh_tmp();
                                self.emit(format!("{p} = getelementptr i8, ptr %l{local}, i64 8"));
                                self.emit(format!("store i64 {second}, ptr {p}"));
                            }
                        }
                    } else {
                        let a = format!("%arg{arg}");
                        arg += 1;
                        self.emit(format!("store {ll} {a}, ptr %l{local}"));
                    }
                }
            }
        }
        Ok(())
    }

    /// The end of a `test` block: status 0, or, for the error a `try` carries out of
    /// the block, the line `error: <structural form>` on standard error (as the test's
    /// module sees the value) and status 1. The block's defers have already run: phase
    /// 8 puts them before the `Return`. Each site prints the type of its own value,
    /// because a `try` in a test accepts any error type (01 section 4.6).
    fn emit_test_return(
        &mut self,
        value: Option<&Operand>,
        is_error: bool,
    ) -> Result<(), BuildError> {
        if !is_error {
            self.emit("ret i32 0");
            return Ok(());
        }
        let op = value.ok_or_else(|| {
            BuildError::Internal("a test's error return has no value".to_string())
        })?;
        let ty = self.operand_ty(op)?;
        let ptr = match op {
            Operand::Place(place, _) => self.place_addr(place)?,
            Operand::Const(_) => {
                let (v, ll) = self.value(op)?;
                let align = self.cg.layout(ty)?.align;
                let slot = self.alloca(&ll, align);
                self.emit(format!("store {ll} {v}, ptr {slot}"));
                slot
            }
        };
        let from = self.cg.program.sema.def(self.body.cfg.key.owner).module;
        let print = global_ref(&self.cg.error_line_symbol(ty, from));
        self.emit(format!("call void {print}(ptr {ptr})"));
        self.emit("ret i32 1");
        Ok(())
    }

    fn emit_return(&mut self, value: Option<&Operand>, is_error: bool) -> Result<(), BuildError> {
        if self.cg.is_test_body(self.body) {
            return self.emit_test_return(value, is_error);
        }
        let abi = self.ret_abi.clone();
        match abi {
            RetAbi::Plain { ll } => {
                if ll == "void" {
                    if self.body.sig.ret == self.cg.program.decls.types.never {
                        self.emit("unreachable");
                    } else {
                        self.emit("ret void");
                    }
                } else {
                    let op = value.ok_or_else(|| {
                        BuildError::Internal("a return without a value".to_string())
                    })?;
                    let (v, vll) = self.value(op)?;
                    if let Some(tag) = self.cg.tag_only_enum(self.body.sig.ret) {
                        if vll == tag {
                            self.emit(format!("ret {tag} {v}"));
                        } else {
                            let t = self.fresh_tmp();
                            self.emit(format!("{t} = extractvalue {vll} {v}, 0"));
                            self.emit(format!("ret {tag} {t}"));
                        }
                        return Ok(());
                    }
                    if vll != ll {
                        return Err(BuildError::Internal(format!(
                            "return type {vll} is not {ll}"
                        )));
                    }
                    self.emit(format!("ret {vll} {v}"));
                }
                Ok(())
            }
            RetAbi::Result(r) => {
                if r.optional {
                    let op = value.ok_or_else(|| {
                        BuildError::Internal("an optional return without a value".to_string())
                    })?;
                    let (v, vll) = self.value(op)?;
                    let align = self.cg.layout(self.body.sig.ret)?.align;
                    let tmp = self.alloca(&vll, align);
                    self.emit(format!("store {vll} {v}, ptr {tmp}"));
                    let tag = self.fresh_tmp();
                    self.emit(format!("{tag} = load {}, ptr {tmp}", r.tag_ll));
                    if r.ptr {
                        self.copy_region(&tmp, r.payload_offset, "%res", 0, r.payload_area);
                        self.emit(format!("ret {} {tag}", r.tag_ll));
                    } else {
                        let slot = self.zeroed_bytes(8, 8);
                        self.copy_region(&tmp, r.payload_offset, &slot, 0, r.payload_area);
                        let payload = self.fresh_tmp();
                        self.emit(format!("{payload} = load i64, ptr {slot}"));
                        self.return_pair(&r, &tag, &payload);
                    }
                    return Ok(());
                }
                let tag_text = int_literal(&r.tag_ll, i128::from(is_error));
                let payload_ty = if is_error {
                    r.error_ty.ok_or_else(|| {
                        BuildError::Internal(
                            "an error return in a function with no error".to_string(),
                        )
                    })?
                } else {
                    r.payload_ty
                };
                let payload_ll = if payload_ty == self.cg.program.decls.types.void
                    || payload_ty == self.cg.program.decls.types.never
                {
                    None
                } else {
                    Some(self.cg.llvm_ty(payload_ty)?)
                };
                let payload_size = self.cg.layout(payload_ty)?.size;
                if r.ptr {
                    if payload_size > 0 {
                        let op = value.ok_or_else(|| {
                            BuildError::Internal("a return without a payload".to_string())
                        })?;
                        let (v, vll) = self.value(op)?;
                        if Some(vll.as_str()) != payload_ll.as_deref() {
                            return Err(BuildError::Internal(format!(
                                "return payload type {vll} is not {payload_ll:?}"
                            )));
                        }
                        self.emit(format!("store {vll} {v}, ptr %res"));
                    }
                    self.emit(format!("ret {} {tag_text}", r.tag_ll));
                } else {
                    let slot = self.zeroed_bytes(8, 8);
                    if payload_size > 0 {
                        let op = value.ok_or_else(|| {
                            BuildError::Internal("a return without a payload".to_string())
                        })?;
                        let (v, vll) = self.value(op)?;
                        if Some(vll.as_str()) != payload_ll.as_deref() {
                            return Err(BuildError::Internal(format!(
                                "return payload type {vll} is not {payload_ll:?}"
                            )));
                        }
                        self.emit(format!("store {vll} {v}, ptr {slot}"));
                    }
                    let payload = self.fresh_tmp();
                    self.emit(format!("{payload} = load i64, ptr {slot}"));
                    self.return_pair(&r, &tag_text, &payload);
                }
                Ok(())
            }
        }
    }

    fn return_pair(&mut self, r: &ResultAbi, tag: &str, payload: &str) {
        let pair = self.fresh_tmp();
        self.emit(format!(
            "{pair} = insertvalue {{ {}, i64 }} zeroinitializer, {} {tag}, 0",
            r.tag_ll, r.tag_ll
        ));
        let out = self.fresh_tmp();
        self.emit(format!(
            "{out} = insertvalue {{ {}, i64 }} {pair}, i64 {payload}, 1",
            r.tag_ll
        ));
        self.emit(format!("ret {{ {}, i64 }} {out}", r.tag_ll));
    }

    /// A zeroed byte slot of `size` bytes. Used for the one-register result payload.
    fn zeroed_bytes(&mut self, size: u64, align: u64) -> String {
        let slot = self.alloca(&format!("[{size} x i8]"), align.max(1));
        self.emit(format!("store [{size} x i8] zeroinitializer, ptr {slot}"));
        slot
    }

    /// Copy `len` bytes from `src + src_off` to `dst + dst_off`, with align 1 so no
    /// alignment claim is made. `len` is a compile-time size.
    fn copy_region(&mut self, src: &str, src_off: u64, dst: &str, dst_off: u64, len: u64) {
        let mut done = 0u64;
        while done < len {
            let left = len - done;
            let (width, ll) = if left >= 8 {
                (8u64, "i64")
            } else if left >= 4 {
                (4, "i32")
            } else if left >= 2 {
                (2, "i16")
            } else {
                (1, "i8")
            };
            let p = self.ptr_at(src, src_off + done);
            let q = self.ptr_at(dst, dst_off + done);
            let v = self.fresh_tmp();
            self.emit(format!("{v} = load {ll}, ptr {p}, align 1"));
            self.emit(format!("store {ll} {v}, ptr {q}, align 1"));
            done += width;
        }
    }

    fn ptr_at(&mut self, base: &str, offset: u64) -> String {
        if offset == 0 {
            return base.to_string();
        }
        let out = self.fresh_tmp();
        self.emit(format!(
            "{out} = getelementptr i8, ptr {base}, i64 {offset}"
        ));
        out
    }

    fn emit_block(&mut self, block: &BlockData) -> Result<(), BuildError> {
        self.known_variants.clear();
        for stmt in &block.stmts {
            match &stmt.kind {
                StmtKind::StorageStart(_) | StmtKind::StorageEnd(_) => {}
                StmtKind::Assign { place, value } => self.assign(place, value)?,
                StmtKind::EnterUnsafe
                | StmtKind::Variant { .. }
                | StmtKind::Cancel(_)
                | StmtKind::Abandon(_)
                | StmtKind::IterConsumed(_)
                | StmtKind::Use(_) => {}
                StmtKind::Check(check) => self.check(check, stmt.span)?,
            }
        }
        self.terminator(&block.term, block.term_span)
    }

    /// A switch target that starts with a successful variant test and ends in a
    /// `Return`: the error arm of `try`, a `catch`/`orelse` handler that leaves the
    /// function, or the `none` arm of a `for` over an `Iter`. Phase 8 emits `Switch`
    /// only for those three constructs, never for `match`, so this does not mark an
    /// ordinary `match` arm cold.
    fn leaving_variant_arm(&self, block: Block) -> bool {
        let Some(data) = self.body.cfg.blocks.get(block as usize) else {
            return false;
        };
        let variant = matches!(
            data.stmts.first().map(|s| &s.kind),
            Some(StmtKind::Variant { holds: true, .. })
        );
        variant && matches!(data.term, Terminator::Return { .. })
    }

    fn terminator(&mut self, term: &Terminator, span: Span) -> Result<(), BuildError> {
        match term {
            Terminator::Goto(target) => {
                self.emit(format!("br label %{}", self.label_of(*target)));
            }
            Terminator::Branch {
                cond,
                then_block,
                else_block,
            } => {
                let (v, _) = self.value(cond)?;
                self.emit(format!(
                    "br i1 {v}, label %{}, label %{}",
                    self.label_of(*then_block),
                    self.label_of(*else_block)
                ));
            }
            Terminator::Switch {
                value,
                cases,
                default,
            } => {
                let (v, ll) = self.value(value)?;
                let mut arms = String::new();
                let cold: Vec<bool> = cases
                    .iter()
                    .map(|(_, block)| self.leaving_variant_arm(*block))
                    .collect();
                let default_cold = self.leaving_variant_arm(*default);
                for (tag, block) in cases {
                    arms.push_str(&format!(
                        "{} {}, label %{} ",
                        ll,
                        int_literal(&ll, *tag),
                        self.label_of(*block)
                    ));
                }
                let prof = if cold.iter().any(|c| *c) || default_cold {
                    let mut weights: Vec<u32> =
                        cold.iter().map(|c| if *c { 1 } else { 1000 }).collect();
                    weights.push(if default_cold { 1 } else { 1000 });
                    let id = self.cg.add_branch_weights(&weights);
                    format!(", !prof !{id}")
                } else {
                    String::new()
                };
                self.emit(format!(
                    "switch {ll} {v}, label %{} [ {arms}]{prof}",
                    self.label_of(*default)
                ));
            }
            Terminator::Return {
                value, is_error, ..
            } => {
                self.emit_return(value.as_ref(), *is_error)?;
            }
            Terminator::Call {
                target,
                args,
                dest,
                next,
            } => {
                self.call(target, args, dest.as_ref(), *next, span)?;
            }
            Terminator::Trap {
                code,
                operands,
                source,
            } => {
                self.emit_trap(*code, operands, source.as_deref(), span)?;
                self.emit("unreachable");
            }
            Terminator::Unreachable => self.emit("unreachable"),
            Terminator::Spawn { .. }
            | Terminator::ScopeWait { .. }
            | Terminator::Select { .. }
            | Terminator::ParFor { .. }
            | Terminator::Dispatch { .. } => {
                return self.cg.ni("concurrency terminators (R2)");
            }
        }
        Ok(())
    }

    fn assign(&mut self, place: &Place, value: &Rvalue) -> Result<(), BuildError> {
        let ty = self.place_type(place)?;
        if let Rvalue::Use(Operand::Const(Const::Uninit { .. })) = value {
            return Ok(());
        }
        // A `void` or `never` place holds no bytes, and a local of either type has no
        // storage (`emit_locals`): the ok value of `try` on a `void ! E` call is one.
        let resolved = self.cg.resolve_ty(ty);
        if resolved == self.cg.program.decls.types.void
            || resolved == self.cg.program.decls.types.never
        {
            // A builtin of type `void` (`@atomic_store`, `@fence`) is an effect, not a
            // value to drop: it runs, and a builtin the back end lacks is reported.
            if let Rvalue::Builtin { .. } = value {
                self.rvalue(value, ty)?;
            }
            return Ok(());
        }
        let (v, ll) = self.rvalue(value, ty)?;
        let addr = self.place_addr(place)?;
        self.emit(format!("store {ll} {v}, ptr {addr}"));
        self.note_variant(place, value);
        Ok(())
    }

    /// Keeps `known_variants` current after `place = value`: a local set to a variant
    /// with no payload, or to a copy of such a local, is known; any other write to a
    /// local, or a borrow of it, makes it unknown.
    fn note_variant(&mut self, place: &Place, value: &Rvalue) {
        // Any borrow can reach a write: an exclusive one directly, a shared one through
        // a raw pointer made from it.
        if let Rvalue::Borrow {
            place: borrowed, ..
        } = value
            && let PlaceBase::Local(l) = borrowed.base
        {
            self.known_variants.remove(&l);
        }
        let PlaceBase::Local(dest) = place.base else {
            return;
        };
        let known = match value {
            Rvalue::Aggregate {
                kind: AggregateKind::Variant { index, .. },
                fields,
                ..
            } if place.proj.is_empty() && fields.is_empty() => Some(*index),
            Rvalue::Use(Operand::Place(from, _))
                if place.proj.is_empty() && from.proj.is_empty() =>
            {
                match from.base {
                    PlaceBase::Local(l) => self.known_variants.get(&l).copied(),
                    PlaceBase::Static(_) => None,
                }
            }
            _ => None,
        };
        match known {
            Some(index) => self.known_variants.insert(dest, index),
            None => self.known_variants.remove(&dest),
        };
    }

    fn rvalue(&mut self, rvalue: &Rvalue, ty: TypeId) -> Result<(String, String), BuildError> {
        match rvalue {
            Rvalue::Use(op) => self.value(op),
            Rvalue::Unary { op, operand } => {
                let (v, ll) = self.value(operand)?;
                let out = self.fresh_tmp();
                match op {
                    ingot_syntax::ast::UnOp::Neg | ingot_syntax::ast::UnOp::NegWrap => {
                        if ll.starts_with('i') {
                            self.emit(format!("{out} = sub {ll} 0, {v}"));
                        } else {
                            self.emit(format!("{out} = fneg {ll} {v}"));
                        }
                    }
                    ingot_syntax::ast::UnOp::Not => {
                        self.emit(format!("{out} = xor i1 {v}, true"));
                    }
                    ingot_syntax::ast::UnOp::BitNot => {
                        self.emit(format!("{out} = xor {ll} {v}, -1"));
                    }
                    // CFG lowering builds `Unary` for these four only.
                    _ => {
                        return Err(BuildError::Internal(
                            "a unary rvalue that is not `-`, `-%`, `!` or `~`".to_string(),
                        ));
                    }
                }
                Ok((out, ll))
            }
            Rvalue::Binary { op, lhs, rhs } => self.binary(*op, lhs, rhs, ty),
            Rvalue::Borrow { place, .. } => {
                let addr = self.place_addr(place)?;
                Ok((addr, "ptr".to_string()))
            }
            Rvalue::Reborrow { place, .. } => {
                let t = self.place_type(place)?;
                let addr = self.place_addr(place)?;
                let ll = self.cg.llvm_ty(t)?;
                let out = self.fresh_tmp();
                self.emit(format!("{out} = load {ll}, ptr {addr}"));
                Ok((out, ll))
            }
            Rvalue::Aggregate {
                kind,
                ty: agg_ty,
                fields,
                ..
            } => self.aggregate(kind, *agg_ty, fields),
            Rvalue::Cast { kind, operand } => self.cast(*kind, operand, ty),
            Rvalue::DynCast { operand, coercion } => self.dyn_cast(operand, coercion, ty),
            Rvalue::Discriminant(place) => self.discriminant(place),
            Rvalue::SliceLen(place) => self.slice_len(place),
            Rvalue::SlicePtr(place) => self.slice_ptr(place),
            Rvalue::Slice { base, lo, hi, .. } => self.slice_value(base, lo, hi, ty),
            Rvalue::SliceElem { base, index } => self.slice_elem(base, index),
            Rvalue::Builtin {
                name,
                type_arg,
                args,
                ..
            } => self.builtin(name, *type_arg, args, ty),
        }
    }
}

/// Emits a generated built-in `Eq.eq` or `Ord.cmp` body for one concrete type.
/// The methods have no source body and only primitive `Eq`/`Ord` types occur here,
/// so the code is generated directly rather than through a CFG.
struct BuiltinGen<'a, 'p> {
    cg: &'a mut Codegen<'p>,
    ty: TypeId,
    interface: DefId,
    name: String,
    ret_ll: String,
    ordering: (i128, i128, i128),
    blocks: Vec<OutBlock>,
    cur: usize,
    next_tmp: usize,
    next_label: usize,
    entry_allocas: Vec<String>,
}

impl<'a, 'p> BuiltinGen<'a, 'p> {
    fn new(
        cg: &'a mut Codegen<'p>,
        ty: TypeId,
        interface: DefId,
        name: &str,
        ret_ll: String,
        ordering: (i128, i128, i128),
    ) -> Self {
        BuiltinGen {
            cg,
            ty,
            interface,
            name: name.to_string(),
            ret_ll,
            ordering,
            blocks: Vec::new(),
            cur: 0,
            next_tmp: 0,
            next_label: 0,
            entry_allocas: Vec::new(),
        }
    }

    fn run(mut self, symbol: &str) -> Result<(), BuildError> {
        self.blocks.push(OutBlock {
            label: "entry".to_string(),
            lines: Vec::new(),
        });
        self.cur = 0;
        let (v, ll) = if self.name == "eq" {
            self.eq(self.ty, "%a0", "%a1")?
        } else {
            self.cmp(self.ty, "%a0", "%a1")?
        };
        self.emit(format!("ret {ll} {v}"));
        let mut out = String::new();
        out.push_str(&format!(
            "define internal {} {} (ptr %a0, ptr %a1) nounwind #0 {{\n",
            self.ret_ll,
            global_ref(symbol)
        ));
        for (i, block) in self.blocks.iter().enumerate() {
            out.push_str(&format!("{}:\n", block.label));
            if i == 0 {
                for line in &self.entry_allocas {
                    out.push_str("  ");
                    out.push_str(line);
                    out.push('\n');
                }
            }
            for line in &block.lines {
                out.push_str("  ");
                out.push_str(line);
                out.push('\n');
            }
        }
        out.push_str("}\n");
        self.cg.functions.push(out);
        Ok(())
    }

    fn emit(&mut self, line: impl Into<String>) {
        self.blocks[self.cur].lines.push(line.into());
    }

    fn fresh_tmp(&mut self) -> String {
        let n = self.next_tmp;
        self.next_tmp += 1;
        format!("%b{n}")
    }

    fn fresh_label(&mut self, prefix: &str) -> String {
        let n = self.next_label;
        self.next_label += 1;
        format!("{prefix}{n}")
    }

    fn new_block(&mut self, label: String) -> usize {
        self.blocks.push(OutBlock {
            label,
            lines: Vec::new(),
        });
        self.blocks.len() - 1
    }

    fn br(&mut self, label: &str) {
        self.emit(format!("br label %{label}"));
    }

    fn alloca(&mut self, ll: &str, align: u64) -> String {
        let tmp = self.fresh_tmp();
        self.entry_allocas
            .push(format!("{tmp} = alloca {ll}, align {align}"));
        tmp
    }

    fn and(&mut self, a: &str, b: &str) -> String {
        let out = self.fresh_tmp();
        self.emit(format!("{out} = and i1 {a}, {b}"));
        out
    }

    fn gep(&mut self, base: &str, offset: u64) -> String {
        if offset == 0 {
            return base.to_string();
        }
        let out = self.fresh_tmp();
        self.emit(format!(
            "{out} = getelementptr i8, ptr {base}, i64 {offset}"
        ));
        out
    }

    fn load(&mut self, ty: TypeId, ptr: &str) -> Result<(String, String), BuildError> {
        let ll = self.cg.llvm_ty(ty)?;
        let out = self.fresh_tmp();
        self.emit(format!("{out} = load {ll}, ptr {ptr}"));
        Ok((out, ll))
    }

    fn load_ll(&mut self, ll: &str, ptr: &str) -> String {
        let out = self.fresh_tmp();
        self.emit(format!("{out} = load {ll}, ptr {ptr}"));
        out
    }

    fn tag_of(&mut self, ty: TypeId, ptr: &str) -> Result<(String, String), BuildError> {
        self.cg.type_info(ty)?;
        match self.cg.types[&ty].shape.clone() {
            Shape::Enum { tag, .. } => {
                let v = self.load_ll(&tag, ptr);
                Ok((v, tag))
            }
            _ => Err(BuildError::Internal(
                "a built-in comparison of a non-enum".to_string(),
            )),
        }
    }

    fn payload_ptr(&mut self, ty: TypeId, ptr: &str) -> Result<String, BuildError> {
        self.cg.type_info(ty)?;
        match self.cg.types[&ty].shape.clone() {
            Shape::Enum { payload_offset, .. } => Ok(self.gep(ptr, payload_offset)),
            _ => Err(BuildError::Internal("a payload of a non-enum".to_string())),
        }
    }

    fn field_ptr(&mut self, ty: TypeId, ptr: &str, index: usize) -> Result<String, BuildError> {
        let layout = self.cg.layout(ty)?;
        let off = layout
            .fields
            .get(index)
            .map(|f| f.offset)
            .ok_or_else(|| BuildError::Internal("a field index out of range".to_string()))?;
        Ok(self.gep(ptr, off))
    }

    fn slice_parts(&mut self, ptr: &str) -> (String, String) {
        let data = self.load_ll("ptr", ptr);
        let len_ptr = self.gep(ptr, 8);
        let len = self.load_ll("i64", &len_ptr);
        (data, len)
    }

    fn order_value(&mut self, lt: &str, gt: &str) -> String {
        let (less, equal, greater) = self.ordering;
        let less_lit = int_literal(&self.ret_ll, less);
        let equal_lit = int_literal(&self.ret_ll, equal);
        let greater_lit = int_literal(&self.ret_ll, greater);
        let first = self.fresh_tmp();
        self.emit(format!(
            "{first} = select i1 {lt}, {} {less_lit}, {} {equal_lit}",
            self.ret_ll, self.ret_ll
        ));
        let out = self.fresh_tmp();
        self.emit(format!(
            "{out} = select i1 {gt}, {} {greater_lit}, {} {first}",
            self.ret_ll, self.ret_ll
        ));
        out
    }

    fn float_key(&mut self, _ty: TypeId, bits: &str, ll: &str) -> String {
        let width = if ll == "float" { 32 } else { 64 };
        let int_ll = format!("i{width}");
        let key = self.fresh_tmp();
        self.emit(format!("{key} = bitcast {ll} {bits} to {int_ll}"));
        let sign_mask = if width == 32 {
            0x8000_0000u64
        } else {
            0x8000_0000_0000_0000u64
        };
        let all_ones = "-1".to_string();
        let mask = self.fresh_tmp();
        self.emit(format!("{mask} = and {int_ll} {key}, {sign_mask}"));
        let neg = self.fresh_tmp();
        self.emit(format!("{neg} = icmp ne {int_ll} {mask}, 0"));
        let flipped = self.fresh_tmp();
        self.emit(format!("{flipped} = xor {int_ll} {key}, {all_ones}"));
        let pos = self.fresh_tmp();
        self.emit(format!("{pos} = or {int_ll} {key}, {sign_mask}"));
        let out = self.fresh_tmp();
        self.emit(format!(
            "{out} = select i1 {neg}, {int_ll} {flipped}, {int_ll} {pos}"
        ));
        out
    }

    fn eq(&mut self, ty: TypeId, a: &str, b: &str) -> Result<(String, String), BuildError> {
        let kind = self.cg.program.decls.types.kind(ty).clone();
        Ok(match kind {
            TyKind::Int(_) | TyKind::Bool => {
                let (va, ll) = self.load(ty, a)?;
                let (vb, _) = self.load(ty, b)?;
                let out = self.fresh_tmp();
                self.emit(format!("{out} = icmp eq {ll} {va}, {vb}"));
                (out, "i1".to_string())
            }
            TyKind::Float(_) => {
                let (va, ll) = self.load(ty, a)?;
                let (vb, _) = self.load(ty, b)?;
                let ka = self.float_key(ty, &va, &ll);
                let kb = self.float_key(ty, &vb, &ll);
                let int_ll = if ll == "float" { "i32" } else { "i64" };
                let out = self.fresh_tmp();
                self.emit(format!("{out} = icmp eq {int_ll} {ka}, {kb}"));
                (out, "i1".to_string())
            }
            TyKind::RawPtr(..) => {
                let va = self.load_ll("ptr", a);
                let vb = self.load_ll("ptr", b);
                let out = self.fresh_tmp();
                self.emit(format!("{out} = icmp eq ptr {va}, {vb}"));
                (out, "i1".to_string())
            }
            TyKind::Adt(..) if self.cg.tag_only_enum(ty).is_some() => {
                let (va, ll) = self.tag_of(ty, a)?;
                let (vb, _) = self.tag_of(ty, b)?;
                let out = self.fresh_tmp();
                self.emit(format!("{out} = icmp eq {ll} {va}, {vb}"));
                (out, "i1".to_string())
            }
            TyKind::Adt(..) => self.user_method(ty, a, b)?,
            TyKind::Ref(_, inner) => {
                let va = self.load_ll("ptr", a);
                let vb = self.load_ll("ptr", b);
                self.eq(inner, &va, &vb)?
            }
            TyKind::Array(elem, n) => {
                let len = self
                    .cg
                    .program
                    .decls
                    .types
                    .const_arg_value(n)
                    .and_then(|v| match v {
                        ConstValue::Int { value, .. } if value >= 0 => Some(value as u64),
                        _ => None,
                    })
                    .ok_or_else(|| BuildError::Internal("an array with no length".to_string()))?;
                self.eq_array(elem, len, a, b)?
            }
            TyKind::Tuple(ts) => {
                let mut result = "true".to_string();
                for (i, t) in ts.iter().enumerate() {
                    let pa = self.field_ptr(ty, a, i)?;
                    let pb = self.field_ptr(ty, b, i)?;
                    let (v, _) = self.eq(*t, &pa, &pb)?;
                    result = self.and(&result, &v);
                }
                (result, "i1".to_string())
            }
            TyKind::Slice(_, elem) => self.eq_slice(elem, a, b)?,
            TyKind::Optional(inner) => self.eq_optional(inner, a, b)?,
            other => {
                return Err(BuildError::NotImplemented(format!(
                    "the built-in `Eq` for {other:?} (B2)"
                )));
            }
        })
    }

    /// A user `Eq`/`Ord` implementation, called directly. The two arguments are
    /// pointers to the elements, exactly the receiver and second parameter.
    fn user_method(
        &mut self,
        ty: TypeId,
        a: &str,
        b: &str,
    ) -> Result<(String, String), BuildError> {
        let key = comparison_method_key(
            self.cg.program.sema,
            &mut *self.cg.program.decls,
            self.interface,
            ty,
        )
        .ok_or_else(|| {
            BuildError::Internal(format!(
                "no user comparison method for `{}`",
                self.cg.type_display(ty)
            ))
        })?;
        let symbol = self.cg.symbols.get(&key).cloned().ok_or_else(|| {
            BuildError::Internal("a user comparison instance was not discovered".to_string())
        })?;
        let out = self.fresh_tmp();
        self.emit(format!(
            "{out} = call {} {}(ptr {a}, ptr {b})",
            self.ret_ll,
            global_ref(&symbol)
        ));
        Ok((out, self.ret_ll.clone()))
    }

    fn eq_array(
        &mut self,
        elem: TypeId,
        len: u64,
        a: &str,
        b: &str,
    ) -> Result<(String, String), BuildError> {
        let index = self.alloca("i64", 8);
        let same = self.alloca("i1", 1);
        self.emit(format!("store i64 0, ptr {index}"));
        self.emit(format!("store i1 true, ptr {same}"));
        let head = self.fresh_label("head");
        let body = self.fresh_label("body");
        let end = self.fresh_label("end");
        self.br(&head);
        self.cur = self.new_block(head.clone());
        let i = self.load_ll("i64", &index);
        let in_range = self.fresh_tmp();
        self.emit(format!("{in_range} = icmp ult i64 {i}, {len}"));
        self.emit(format!("br i1 {in_range}, label %{body}, label %{end}"));
        self.cur = self.new_block(body);
        let idx = self.load_ll("i64", &index);
        let pa = self.elem_ptr_at(a, elem, &idx)?;
        let pb = self.elem_ptr_at(b, elem, &idx)?;
        let (v, _) = self.eq(elem, &pa, &pb)?;
        let prev = self.load_ll("i1", &same);
        let now = self.and(&prev, &v);
        self.emit(format!("store i1 {now}, ptr {same}"));
        let next = self.fresh_tmp();
        self.emit(format!("{next} = add i64 {idx}, 1"));
        self.emit(format!("store i64 {next}, ptr {index}"));
        self.br(&head);
        self.cur = self.new_block(end);
        Ok((self.load_ll("i1", &same), "i1".to_string()))
    }

    fn cmp_array(
        &mut self,
        elem: TypeId,
        len: u64,
        a: &str,
        b: &str,
    ) -> Result<(String, String), BuildError> {
        let equal = int_literal(&self.ret_ll, self.ordering.1);
        let index = self.alloca("i64", 8);
        let result = self.alloca(&self.ret_ll.clone(), 1);
        self.emit(format!("store i64 0, ptr {index}"));
        self.emit(format!("store {} {equal}, ptr {result}", self.ret_ll));
        let head = self.fresh_label("head");
        let body = self.fresh_label("body");
        let end = self.fresh_label("end");
        self.br(&head);
        self.cur = self.new_block(head.clone());
        let i = self.load_ll("i64", &index);
        let in_range = self.fresh_tmp();
        self.emit(format!("{in_range} = icmp ult i64 {i}, {len}"));
        let r = self.load_ll(&self.ret_ll.clone(), &result);
        let still = self.fresh_tmp();
        self.emit(format!("{still} = icmp eq {} {r}, {equal}", self.ret_ll));
        let go = self.and(&in_range, &still);
        self.emit(format!("br i1 {go}, label %{body}, label %{end}"));
        self.cur = self.new_block(body);
        let idx = self.load_ll("i64", &index);
        let pa = self.elem_ptr_at(a, elem, &idx)?;
        let pb = self.elem_ptr_at(b, elem, &idx)?;
        let (c, _) = self.cmp(elem, &pa, &pb)?;
        self.emit(format!("store {} {c}, ptr {result}", self.ret_ll));
        let next = self.fresh_tmp();
        self.emit(format!("{next} = add i64 {idx}, 1"));
        self.emit(format!("store i64 {next}, ptr {index}"));
        self.br(&head);
        self.cur = self.new_block(end);
        Ok((
            self.load_ll(&self.ret_ll.clone(), &result),
            self.ret_ll.clone(),
        ))
    }

    fn eq_slice(&mut self, elem: TypeId, a: &str, b: &str) -> Result<(String, String), BuildError> {
        let (pa, la) = self.slice_parts(a);
        let (pb, lb) = self.slice_parts(b);
        let index = self.alloca("i64", 8);
        let same = self.alloca("i1", 1);
        self.emit(format!("store i64 0, ptr {index}"));
        self.emit(format!("store i1 true, ptr {same}"));
        let head = self.fresh_label("head");
        let body = self.fresh_label("body");
        let end = self.fresh_label("end");
        self.br(&head);
        self.cur = self.new_block(head.clone());
        let i = self.load_ll("i64", &index);
        let in_a = self.fresh_tmp();
        self.emit(format!("{in_a} = icmp ult i64 {i}, {la}"));
        let in_b = self.fresh_tmp();
        self.emit(format!("{in_b} = icmp ult i64 {i}, {lb}"));
        let both = self.and(&in_a, &in_b);
        self.emit(format!("br i1 {both}, label %{body}, label %{end}"));
        self.cur = self.new_block(body);
        let idx = self.load_ll("i64", &index);
        let ea = self.elem_ptr_at(&pa, elem, &idx)?;
        let eb = self.elem_ptr_at(&pb, elem, &idx)?;
        let (ev, _) = self.eq(elem, &ea, &eb)?;
        let prev = self.load_ll("i1", &same);
        let now = self.and(&prev, &ev);
        self.emit(format!("store i1 {now}, ptr {same}"));
        let next = self.fresh_tmp();
        self.emit(format!("{next} = add i64 {idx}, 1"));
        self.emit(format!("store i64 {next}, ptr {index}"));
        self.br(&head);
        self.cur = self.new_block(end);
        let same_now = self.load_ll("i1", &same);
        let lengths = self.fresh_tmp();
        self.emit(format!("{lengths} = icmp eq i64 {la}, {lb}"));
        let out = self.and(&same_now, &lengths);
        Ok((out, "i1".to_string()))
    }

    fn eq_optional(
        &mut self,
        inner: TypeId,
        a: &str,
        b: &str,
    ) -> Result<(String, String), BuildError> {
        let ty = self.cg.program.decls.types.intern(TyKind::Optional(inner));
        self.cg.type_info(ty)?;
        let shape = self.cg.types[&ty].shape.clone();
        let Shape::Enum {
            niche, variants, ..
        } = shape
        else {
            return Err(BuildError::Internal(
                "an optional that is not an enum".to_string(),
            ));
        };
        if niche {
            // The null pointer is `.none`. Two `.some` values compare as their
            // payload, which occupies the same bytes at offset 0.
            let va = self.load_ll("ptr", a);
            let vb = self.load_ll("ptr", b);
            let a_none = self.fresh_tmp();
            self.emit(format!("{a_none} = icmp eq ptr {va}, null"));
            let b_none = self.fresh_tmp();
            self.emit(format!("{b_none} = icmp eq ptr {vb}, null"));
            let result = self.alloca("i1", 1);
            let none_block = self.fresh_label("bothnone");
            let some_block = self.fresh_label("some");
            let done = self.fresh_label("done");
            self.emit(format!(
                "br i1 {a_none}, label %{none_block}, label %{some_block}"
            ));
            self.cur = self.new_block(none_block);
            self.emit(format!("store i1 {b_none}, ptr {result}"));
            self.br(&done);
            self.cur = self.new_block(some_block);
            let both_some = self.fresh_label("bothsome");
            let b_none_block = self.fresh_label("bnone");
            self.emit(format!(
                "br i1 {b_none}, label %{b_none_block}, label %{both_some}"
            ));
            self.cur = self.new_block(b_none_block);
            self.emit(format!("store i1 false, ptr {result}"));
            self.br(&done);
            self.cur = self.new_block(both_some);
            let (v, _) = self.eq(inner, a, b)?;
            self.emit(format!("store i1 {v}, ptr {result}"));
            self.br(&done);
            self.cur = self.new_block(done);
            return Ok((self.load_ll("i1", &result), "i1".to_string()));
        }
        let tag = match &self.cg.types[&ty].shape {
            Shape::Enum { tag, .. } => tag.clone(),
            _ => unreachable!(),
        };
        let some_tag = variants.get(1).map(|v| v.tag).unwrap_or(1);
        let result = self.alloca("i1", 1);
        self.emit(format!("store i1 false, ptr {result}"));
        let ta = self.load_ll(&tag, a);
        let tb = self.load_ll(&tag, b);
        let same = self.fresh_tmp();
        self.emit(format!("{same} = icmp eq {tag} {ta}, {tb}"));
        let same_block = self.fresh_label("same");
        let done = self.fresh_label("done");
        self.emit(format!("br i1 {same}, label %{same_block}, label %{done}"));
        self.cur = self.new_block(same_block.clone());
        let is_some = self.fresh_tmp();
        self.emit(format!(
            "{is_some} = icmp eq {tag} {ta}, {}",
            int_literal(&tag, some_tag)
        ));
        let some_block = self.fresh_label("some");
        let none_block = self.fresh_label("none");
        self.emit(format!(
            "br i1 {is_some}, label %{some_block}, label %{none_block}"
        ));
        self.cur = self.new_block(some_block);
        let pa = self.payload_ptr(ty, a)?;
        let pb = self.payload_ptr(ty, b)?;
        let (v, _) = self.eq(inner, &pa, &pb)?;
        self.emit(format!("store i1 {v}, ptr {result}"));
        self.br(&done);
        self.cur = self.new_block(none_block);
        self.emit(format!("store i1 true, ptr {result}"));
        self.br(&done);
        self.cur = self.new_block(done);
        let out = self.load_ll("i1", &result);
        Ok((out, "i1".to_string()))
    }

    fn elem_ptr_at(&mut self, data: &str, elem: TypeId, index: &str) -> Result<String, BuildError> {
        let stride = self.cg.layout(elem)?.size;
        let scaled = self.fresh_tmp();
        self.emit(format!("{scaled} = mul i64 {index}, {stride}"));
        let out = self.fresh_tmp();
        self.emit(format!(
            "{out} = getelementptr i8, ptr {data}, i64 {scaled}"
        ));
        Ok(out)
    }

    fn cmp(&mut self, ty: TypeId, a: &str, b: &str) -> Result<(String, String), BuildError> {
        let kind = self.cg.program.decls.types.kind(ty).clone();
        let (_less, equal, _greater) = self.ordering;
        let equal_lit = int_literal(&self.ret_ll, equal);
        Ok(match kind {
            TyKind::Int(i) => {
                let (va, ll) = self.load(ty, a)?;
                let (vb, _) = self.load(ty, b)?;
                let lt_op = if i.signed() { "slt" } else { "ult" };
                let gt_op = if i.signed() { "sgt" } else { "ugt" };
                let lt = self.fresh_tmp();
                self.emit(format!("{lt} = icmp {lt_op} {ll} {va}, {vb}"));
                let gt = self.fresh_tmp();
                self.emit(format!("{gt} = icmp {gt_op} {ll} {va}, {vb}"));
                let out = self.order_value(&lt, &gt);
                (out, self.ret_ll.clone())
            }
            TyKind::Bool => {
                let va = self.load_ll("i1", a);
                let vb = self.load_ll("i1", b);
                let lt = self.fresh_tmp();
                self.emit(format!("{lt} = icmp ult i1 {va}, {vb}"));
                let gt = self.fresh_tmp();
                self.emit(format!("{gt} = icmp ugt i1 {va}, {vb}"));
                let out = self.order_value(&lt, &gt);
                (out, self.ret_ll.clone())
            }
            TyKind::Float(_) => {
                let (va, ll) = self.load(ty, a)?;
                let (vb, _) = self.load(ty, b)?;
                let ka = self.float_key(ty, &va, &ll);
                let kb = self.float_key(ty, &vb, &ll);
                let int_ll = if ll == "float" { "i32" } else { "i64" };
                let lt = self.fresh_tmp();
                self.emit(format!("{lt} = icmp ult {int_ll} {ka}, {kb}"));
                let gt = self.fresh_tmp();
                self.emit(format!("{gt} = icmp ugt {int_ll} {ka}, {kb}"));
                let out = self.order_value(&lt, &gt);
                (out, self.ret_ll.clone())
            }
            TyKind::RawPtr(..) => {
                let va = self.load_ll("ptr", a);
                let vb = self.load_ll("ptr", b);
                let lt = self.fresh_tmp();
                self.emit(format!("{lt} = icmp ult ptr {va}, {vb}"));
                let gt = self.fresh_tmp();
                self.emit(format!("{gt} = icmp ugt ptr {va}, {vb}"));
                let out = self.order_value(&lt, &gt);
                (out, self.ret_ll.clone())
            }
            TyKind::Adt(..) if self.cg.tag_only_enum(ty).is_some() => {
                let (va, ll) = self.tag_of(ty, a)?;
                let (vb, _) = self.tag_of(ty, b)?;
                let signed = match self.cg.program.decls.types.kind(ty) {
                    TyKind::Adt(def, _) => self
                        .cg
                        .program
                        .decls
                        .enum_sig(*def)
                        .is_some_and(|s| s.tag_ty.signed()),
                    _ => false,
                };
                let op_lt = if signed { "slt" } else { "ult" };
                let op_gt = if signed { "sgt" } else { "ugt" };
                let lt = self.fresh_tmp();
                self.emit(format!("{lt} = icmp {op_lt} {ll} {va}, {vb}"));
                let gt = self.fresh_tmp();
                self.emit(format!("{gt} = icmp {op_gt} {ll} {va}, {vb}"));
                let out = self.order_value(&lt, &gt);
                (out, self.ret_ll.clone())
            }
            TyKind::Adt(..) => self.user_method(ty, a, b)?,
            TyKind::Ref(_, inner) => {
                let va = self.load_ll("ptr", a);
                let vb = self.load_ll("ptr", b);
                self.cmp(inner, &va, &vb)?
            }
            TyKind::Array(elem, n) => {
                let len = self
                    .cg
                    .program
                    .decls
                    .types
                    .const_arg_value(n)
                    .and_then(|v| match v {
                        ConstValue::Int { value, .. } if value >= 0 => Some(value as u64),
                        _ => None,
                    })
                    .ok_or_else(|| BuildError::Internal("an array with no length".to_string()))?;
                self.cmp_array(elem, len, a, b)?
            }
            TyKind::Tuple(ts) => {
                let mut result = equal_lit.clone();
                for (i, t) in ts.iter().enumerate() {
                    let pa = self.field_ptr(ty, a, i)?;
                    let pb = self.field_ptr(ty, b, i)?;
                    let (c, _) = self.cmp(*t, &pa, &pb)?;
                    let undecided = self.fresh_tmp();
                    self.emit(format!(
                        "{undecided} = icmp eq {} {result}, {equal_lit}",
                        self.ret_ll
                    ));
                    let out = self.fresh_tmp();
                    self.emit(format!(
                        "{out} = select i1 {undecided}, {} {c}, {} {result}",
                        self.ret_ll, self.ret_ll
                    ));
                    result = out;
                }
                (result, self.ret_ll.clone())
            }
            TyKind::Slice(_, elem) => self.cmp_slice(elem, a, b)?,
            TyKind::Optional(inner) => self.cmp_optional(inner, a, b)?,
            other => {
                return Err(BuildError::NotImplemented(format!(
                    "the built-in `Ord` for {other:?} (B2)"
                )));
            }
        })
    }

    fn cmp_slice(
        &mut self,
        elem: TypeId,
        a: &str,
        b: &str,
    ) -> Result<(String, String), BuildError> {
        let (pa, la) = self.slice_parts(a);
        let (pb, lb) = self.slice_parts(b);
        let (less, equal, greater) = self.ordering;
        let equal_lit = int_literal(&self.ret_ll, equal);
        let index = self.alloca("i64", 8);
        let result = self.alloca(&self.ret_ll.clone(), 1);
        self.emit(format!("store i64 0, ptr {index}"));
        self.emit(format!("store {} {equal_lit}, ptr {result}", self.ret_ll));
        let head = self.fresh_label("head");
        let body = self.fresh_label("body");
        let tail = self.fresh_label("tail");
        self.br(&head);
        self.cur = self.new_block(head.clone());
        let i = self.load_ll("i64", &index);
        let in_a = self.fresh_tmp();
        self.emit(format!("{in_a} = icmp ult i64 {i}, {la}"));
        let in_b = self.fresh_tmp();
        self.emit(format!("{in_b} = icmp ult i64 {i}, {lb}"));
        let both = self.and(&in_a, &in_b);
        let r = self.load_ll(&self.ret_ll.clone(), &result);
        let still = self.fresh_tmp();
        self.emit(format!(
            "{still} = icmp eq {} {r}, {equal_lit}",
            self.ret_ll
        ));
        let go = self.and(&both, &still);
        self.emit(format!("br i1 {go}, label %{body}, label %{tail}"));
        self.cur = self.new_block(body);
        let idx = self.load_ll("i64", &index);
        let ea = self.elem_ptr_at(&pa, elem, &idx)?;
        let eb = self.elem_ptr_at(&pb, elem, &idx)?;
        let (c, _) = self.cmp(elem, &ea, &eb)?;
        self.emit(format!("store {} {c}, ptr {result}", self.ret_ll));
        let next = self.fresh_tmp();
        self.emit(format!("{next} = add i64 {idx}, 1"));
        self.emit(format!("store i64 {next}, ptr {index}"));
        self.br(&head);
        self.cur = self.new_block(tail);
        let r = self.load_ll(&self.ret_ll.clone(), &result);
        let still = self.fresh_tmp();
        self.emit(format!(
            "{still} = icmp eq {} {r}, {equal_lit}",
            self.ret_ll
        ));
        let by_len = self.fresh_label("bylen");
        let done = self.fresh_label("done");
        self.emit(format!("br i1 {still}, label %{by_len}, label %{done}"));
        self.cur = self.new_block(by_len);
        let lt = self.fresh_tmp();
        self.emit(format!("{lt} = icmp ult i64 {la}, {lb}"));
        let gt = self.fresh_tmp();
        self.emit(format!("{gt} = icmp ugt i64 {la}, {lb}"));
        let out = self.order_value(&lt, &gt);
        self.emit(format!("store {} {out}, ptr {result}", self.ret_ll));
        self.br(&done);
        self.cur = self.new_block(done);
        let out = self.load_ll(&self.ret_ll.clone(), &result);
        let _ = (less, greater);
        Ok((out, self.ret_ll.clone()))
    }

    fn cmp_optional(
        &mut self,
        inner: TypeId,
        a: &str,
        b: &str,
    ) -> Result<(String, String), BuildError> {
        let ty = self.cg.program.decls.types.intern(TyKind::Optional(inner));
        self.cg.type_info(ty)?;
        let shape = self.cg.types[&ty].shape.clone();
        let Shape::Enum {
            niche, variants, ..
        } = shape
        else {
            return Err(BuildError::Internal(
                "an optional that is not an enum".to_string(),
            ));
        };
        let (less, _equal, greater) = self.ordering;
        let result = self.alloca(&self.ret_ll.clone(), 1);
        let done = self.fresh_label("done");
        if niche {
            let va = self.load_ll("ptr", a);
            let vb = self.load_ll("ptr", b);
            let a_none = self.fresh_tmp();
            self.emit(format!("{a_none} = icmp eq ptr {va}, null"));
            let b_none = self.fresh_tmp();
            self.emit(format!("{b_none} = icmp eq ptr {vb}, null"));
            let some_block = self.fresh_label("some");
            let a_none_block = self.fresh_label("anone");
            self.emit(format!(
                "br i1 {a_none}, label %{a_none_block}, label %{some_block}"
            ));
            self.cur = self.new_block(a_none_block);
            let less_lit = int_literal(&self.ret_ll, less);
            let equal_lit = int_literal(&self.ret_ll, self.ordering.1);
            let both_none = self.fresh_label("bothnone");
            let a_less = self.fresh_label("aless");
            self.emit(format!(
                "br i1 {b_none}, label %{both_none}, label %{a_less}"
            ));
            self.cur = self.new_block(both_none);
            self.emit(format!("store {} {equal_lit}, ptr {result}", self.ret_ll));
            self.br(&done);
            self.cur = self.new_block(a_less);
            self.emit(format!("store {} {less_lit}, ptr {result}", self.ret_ll));
            self.br(&done);
            self.cur = self.new_block(some_block);
            let some_done = self.fresh_label("somed");
            let some_b_none = self.fresh_label("sbn");
            self.emit(format!(
                "br i1 {b_none}, label %{some_b_none}, label %{some_done}"
            ));
            self.cur = self.new_block(some_b_none);
            let greater_lit = int_literal(&self.ret_ll, greater);
            self.emit(format!("store {} {greater_lit}, ptr {result}", self.ret_ll));
            self.br(&done);
            self.cur = self.new_block(some_done);
            let (c, _) = self.cmp(inner, a, b)?;
            self.emit(format!("store {} {c}, ptr {result}", self.ret_ll));
            self.br(&done);
            self.cur = self.new_block(done);
            return Ok((
                self.load_ll(&self.ret_ll.clone(), &result),
                self.ret_ll.clone(),
            ));
        }
        let tag = match &self.cg.types[&ty].shape {
            Shape::Enum { tag, .. } => tag.clone(),
            _ => unreachable!(),
        };
        let some_tag = variants.get(1).map(|v| v.tag).unwrap_or(1);
        let ta = self.load_ll(&tag, a);
        let tb = self.load_ll(&tag, b);
        let same = self.fresh_tmp();
        self.emit(format!("{same} = icmp eq {tag} {ta}, {tb}"));
        let same_block = self.fresh_label("same");
        let diff = self.fresh_label("diff");
        self.emit(format!("br i1 {same}, label %{same_block}, label %{diff}"));
        self.cur = self.new_block(same_block);
        let is_some = self.fresh_tmp();
        self.emit(format!(
            "{is_some} = icmp eq {tag} {ta}, {}",
            int_literal(&tag, some_tag)
        ));
        let some_block = self.fresh_label("some");
        let eq_block = self.fresh_label("eq");
        self.emit(format!(
            "br i1 {is_some}, label %{some_block}, label %{eq_block}"
        ));
        self.cur = self.new_block(some_block);
        let pa = self.payload_ptr(ty, a)?;
        let pb = self.payload_ptr(ty, b)?;
        let (c, _) = self.cmp(inner, &pa, &pb)?;
        self.emit(format!("store {} {c}, ptr {result}", self.ret_ll));
        self.br(&done);
        self.cur = self.new_block(eq_block);
        let equal_lit = int_literal(&self.ret_ll, self.ordering.1);
        self.emit(format!("store {} {equal_lit}, ptr {result}", self.ret_ll));
        self.br(&done);
        self.cur = self.new_block(diff);
        let a_less = self.fresh_tmp();
        self.emit(format!("{a_less} = icmp ult {tag} {ta}, {tb}"));
        let less_block = self.fresh_label("less");
        let greater_block = self.fresh_label("greater");
        self.emit(format!(
            "br i1 {a_less}, label %{less_block}, label %{greater_block}"
        ));
        self.cur = self.new_block(less_block);
        let less_lit = int_literal(&self.ret_ll, less);
        self.emit(format!("store {} {less_lit}, ptr {result}", self.ret_ll));
        self.br(&done);
        self.cur = self.new_block(greater_block);
        let greater_lit = int_literal(&self.ret_ll, greater);
        self.emit(format!("store {} {greater_lit}, ptr {result}", self.ret_ll));
        self.br(&done);
        self.cur = self.new_block(done);
        Ok((
            self.load_ll(&self.ret_ll.clone(), &result),
            self.ret_ll.clone(),
        ))
    }
}

/// An LLVM global reference. Most names are bare identifiers; the Ingot mangling
/// contains `:`, which LLVM requires to be quoted.
fn global_ref(name: &str) -> String {
    let bare = name
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_' || c == '.' || c == '$')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '$' | '-'));
    if bare {
        format!("@{name}")
    } else {
        format!("@\"{}\"", name.replace('\\', "\\\\").replace('"', "\\22"))
    }
}

/// Write the low `size` bytes of `value` little-endian into `out` (which starts at the
/// destination offset).
fn write_low(out: &mut [u8], value: i128, size: u64) {
    let bits = value as u128;
    for i in 0..size.min(16) {
        out[i as usize] = (bits >> (i * 8)) as u8;
    }
}

fn int_literal(ll: &str, value: i128) -> String {
    if !ll.starts_with('i') {
        return value.to_string();
    }
    let bits: u32 = ll[1..].parse().unwrap_or(64);
    let mask = if bits >= 128 {
        u128::MAX
    } else {
        (1u128 << bits) - 1
    };
    let raw = (value as u128) & mask;
    let signed = value < 0;
    let v = if signed && bits < 128 && raw >= (1u128 << (bits - 1)) {
        raw as i128 - (1i128 << bits)
    } else {
        raw as i128
    };
    v.to_string()
}

fn llvm_string(bytes: &[u8]) -> String {
    let mut out = String::from("\"");
    for b in bytes {
        match b {
            b'\\' => out.push_str("\\\\"),
            b'"' => out.push_str("\\22"),
            0x20..=0x7e => out.push(*b as char),
            _ => out.push_str(&format!("\\{:02X}", b)),
        }
    }
    out.push('"');
    out
}

impl<'a, 'p> FnGen<'a, 'p> {
    // ------------------------------------------------------------- places

    fn place_type(&mut self, place: &Place) -> Result<TypeId, BuildError> {
        let mut ty = match &place.base {
            PlaceBase::Local(l) => self.cg.resolve_ty(self.body.cfg.local(*l).ty),
            PlaceBase::Static(def) => match self.cg.program.decls.item(*def) {
                Some(ItemSig::Static(s)) => s.ty,
                _ => {
                    return Err(BuildError::Internal(
                        "a place rooted at something that is not a static".to_string(),
                    ));
                }
            },
        };
        for proj in &place.proj {
            ty = self.project_type(ty, proj)?;
            ty = self.cg.resolve_ty(ty);
        }
        Ok(ty)
    }

    fn project_type(&mut self, ty: TypeId, proj: &Projection) -> Result<TypeId, BuildError> {
        let ty = self.cg.resolve_ty(ty);
        let kind = self.cg.program.decls.types.kind(ty).clone();
        match (proj, kind) {
            (Projection::Field { index, .. }, TyKind::Adt(def, args)) => {
                let sig = self.cg.program.decls.struct_sig(def).cloned();
                let Some(sig) = sig else {
                    return Err(BuildError::Internal("a field of a non-struct".to_string()));
                };
                let fty = sig.fields.get(*index).map(|f| f.ty).ok_or_else(|| {
                    BuildError::Internal("a field index out of range".to_string())
                })?;
                let subst = self.cg.program.decls.subst_for(def, &args);
                Ok(self.cg.program.decls.instantiate(fty, &subst))
            }
            (Projection::Field { index, .. }, TyKind::Closure(def, idx, args)) => {
                let fields = self
                    .cg
                    .program
                    .decls
                    .closure_field_tys(def, idx, &args)
                    .ok_or_else(|| {
                        BuildError::Internal("a field of an unknown closure".to_string())
                    })?;
                fields
                    .get(*index)
                    .copied()
                    .ok_or_else(|| BuildError::Internal("a field index out of range".to_string()))
            }
            (Projection::TupleField(index), TyKind::Tuple(ts)) => ts
                .get(*index)
                .copied()
                .ok_or_else(|| BuildError::Internal("a tuple field out of range".to_string())),
            (Projection::Index(_), TyKind::Array(elem, _) | TyKind::Slice(_, elem)) => Ok(elem),
            (Projection::Deref, TyKind::Ref(_, inner) | TyKind::RawPtr(_, inner)) => Ok(inner),
            (Projection::DerefRaw, TyKind::RawPtr(_, inner)) => Ok(inner),
            (Projection::Payload { variant }, _) => {
                self.cg.type_info(ty)?;
                if let Shape::Enum { variants, .. } = &self.cg.types[&ty].shape {
                    variants
                        .get(*variant)
                        .and_then(|v| v.payload)
                        .ok_or_else(|| {
                            BuildError::Internal("a variant without a payload".to_string())
                        })
                } else {
                    Err(BuildError::Internal("a payload of a non-enum".to_string()))
                }
            }
            (proj, kind) => Err(BuildError::Internal(format!(
                "projection {proj:?} on {kind:?}"
            ))),
        }
    }

    fn place_addr(&mut self, place: &Place) -> Result<String, BuildError> {
        let mut addr = match &place.base {
            PlaceBase::Local(l) => format!("%l{l}"),
            PlaceBase::Static(def) => self.cg.static_ref(*def)?,
        };
        let mut ty = match &place.base {
            PlaceBase::Local(l) => self.cg.resolve_ty(self.body.cfg.local(*l).ty),
            PlaceBase::Static(def) => match self.cg.program.decls.item(*def) {
                Some(ItemSig::Static(s)) => s.ty,
                _ => TypeId(0),
            },
        };
        for proj in &place.proj {
            let next = self.project_type(ty, proj)?;
            let next = self.cg.resolve_ty(next);
            match proj {
                Projection::Field { index, .. } | Projection::TupleField(index) => {
                    let layout = self.cg.layout(ty)?;
                    let off = layout.fields.get(*index).map(|f| f.offset).ok_or_else(|| {
                        BuildError::Internal("a field index out of range".to_string())
                    })?;
                    let out = self.fresh_tmp();
                    self.emit(format!("{out} = getelementptr i8, ptr {addr}, i64 {off}"));
                    addr = out;
                }
                Projection::Index(op) => {
                    let idx = self.index_i64(op)?;
                    let out = self.fresh_tmp();
                    match self.cg.program.decls.types.kind(ty).clone() {
                        TyKind::Array(elem, _) => {
                            let arr = self.cg.llvm_ty(ty)?;
                            let e = self.cg.llvm_ty(elem)?;
                            self.emit(format!(
                                "{out} = getelementptr {arr}, ptr {addr}, i64 0, i64 {idx}"
                            ));
                            let _ = e;
                        }
                        TyKind::Slice(_, elem) => {
                            let e = self.cg.llvm_ty(elem)?;
                            let p = self.fresh_tmp();
                            self.emit(format!("{p} = load ptr, ptr {addr}"));
                            self.emit(format!("{out} = getelementptr {e}, ptr {p}, i64 {idx}"));
                        }
                        _ => {
                            return Err(BuildError::Internal(
                                "an index into something that is not an array or slice".to_string(),
                            ));
                        }
                    }
                    addr = out;
                }
                Projection::Deref | Projection::DerefRaw => {
                    let out = self.fresh_tmp();
                    self.emit(format!("{out} = load ptr, ptr {addr}"));
                    addr = out;
                }
                Projection::Payload { variant } => {
                    self.cg.type_info(ty)?;
                    let (off, payload) = if let Shape::Enum {
                        payload_offset,
                        variants,
                        ..
                    } = &self.cg.types[&ty].shape
                    {
                        let v = variants.get(*variant).ok_or_else(|| {
                            BuildError::Internal("a variant index out of range".to_string())
                        })?;
                        (*payload_offset, v.payload)
                    } else {
                        return Err(BuildError::Internal("a payload of a non-enum".to_string()));
                    };
                    if payload.is_none() {
                        return Err(BuildError::Internal(
                            "a payload read of a variant without a payload".to_string(),
                        ));
                    }
                    let out = self.fresh_tmp();
                    self.emit(format!("{out} = getelementptr i8, ptr {addr}, i64 {off}"));
                    addr = out;
                }
            }
            ty = next;
        }
        Ok(addr)
    }

    // -------------------------------------------------------------- values

    fn operand_ty(&mut self, op: &Operand) -> Result<TypeId, BuildError> {
        let ty = match op {
            Operand::Place(p, _) => self.place_type(p)?,
            Operand::Const(c) => self.const_ty(c)?,
        };
        Ok(self.cg.resolve_ty(ty))
    }

    fn const_ty(&mut self, c: &Const) -> Result<TypeId, BuildError> {
        Ok(match c {
            Const::Int { ty, .. } | Const::Float { ty, .. } => *ty,
            Const::Bool(_) => self.cg.program.decls.types.bool_,
            Const::Unit => self.cg.program.decls.types.void,
            Const::Str(_) => self.cg.string_slice_ty(),
            Const::CStr(_) => self.cg.cstr_ty(),
            Const::Zero { ty, .. } | Const::Uninit { ty, .. } => *ty,
            Const::Param { def, .. } => self
                .cg
                .program
                .decls
                .param_const_ty
                .get(def)
                .copied()
                .ok_or_else(|| {
                    BuildError::Internal("a constant parameter with no type".to_string())
                })?,
            Const::Expr { id, .. } => self.cg.const_expr_ty(*id)?,
            Const::Named { def, .. } => match self.cg.program.decls.item(*def) {
                Some(ItemSig::Const(s)) => s.ty,
                _ => {
                    return Err(BuildError::Internal(
                        "a named constant without a signature".to_string(),
                    ));
                }
            },
            Const::Fn { fn_ref, .. } => self.cg.fn_value_ty(fn_ref)?,
        })
    }

    fn value(&mut self, op: &Operand) -> Result<(String, String), BuildError> {
        match op {
            Operand::Place(p, _) => {
                let ty = self.place_type(p)?;
                if ty == self.cg.program.decls.types.void || ty == self.cg.program.decls.types.never
                {
                    return Ok(("zeroinitializer".to_string(), "{}".to_string()));
                }
                let addr = self.place_addr(p)?;
                let ll = self.cg.llvm_ty(ty)?;
                let out = self.fresh_tmp();
                self.emit(format!("{out} = load {ll}, ptr {addr}"));
                Ok((out, ll))
            }
            Operand::Const(c) => self.constant(c),
        }
    }

    fn as_ssa(&mut self, v: &str, ll: &str, align: u64) -> Result<String, BuildError> {
        if v.starts_with('%') && !v.contains(' ') && !v.contains('{') {
            return Ok(v.to_string());
        }
        let tmp = self.alloca(ll, align.max(8));
        self.emit(format!("store {ll} {v}, ptr {tmp}"));
        let out = self.fresh_tmp();
        self.emit(format!("{out} = load {ll}, ptr {tmp}"));
        Ok(out)
    }

    fn named_constant(&mut self, def: DefId, name: &str) -> Result<(String, String), BuildError> {
        let value = self.cg.item_value(def).ok_or_else(|| {
            BuildError::Internal(format!("the constant `{name}` has no evaluated value"))
        })?;
        match &value {
            SemValue::Int { ty, value } => {
                let ll = self.cg.llvm_ty(*ty)?;
                Ok((int_literal(&ll, *value), ll))
            }
            SemValue::Bool(b) => Ok((
                if *b { "true" } else { "false" }.to_string(),
                "i1".to_string(),
            )),
            SemValue::Float { ty, bits } => {
                let ll = self.cg.llvm_ty(*ty)?;
                let value = match self.cg.program.decls.types.kind(*ty) {
                    TyKind::Float(FloatTy::F32) => {
                        f64::from(ingot_sema::consteval::f32_from_bits(*bits))
                    }
                    _ => f64::from_bits(*bits),
                };
                let text = match self.cg.program.decls.types.kind(*ty) {
                    TyKind::Float(FloatTy::F32) => format_float_f32(value),
                    _ => format_float_f64(value),
                };
                Ok((text, ll))
            }
            SemValue::Unit => Ok(("zeroinitializer".to_string(), "{}".to_string())),
            SemValue::Str(bytes) => {
                let sym = self.cg.add_string(bytes);
                let ty = self.cg.string_slice_ty();
                let ll = self.cg.llvm_ty(ty)?;
                let a = self.fresh_tmp();
                let b = self.fresh_tmp();
                self.emit(format!(
                    "{a} = insertvalue {ll} zeroinitializer, ptr @{sym}, 0"
                ));
                self.emit(format!(
                    "{b} = insertvalue {ll} {a}, i64 {}, 1",
                    bytes.len()
                ));
                Ok((b, ll))
            }
            SemValue::CStr(bytes) => {
                let mut b = bytes.clone();
                b.push(0);
                let sym = self.cg.add_string(&b);
                Ok((format!("@{sym}"), "ptr".to_string()))
            }
            SemValue::Fn { fn_ref, .. } => {
                let (symbol, _, _, _) = self.cg.resolve_fn(fn_ref)?;
                Ok((global_ref(&symbol), "ptr".to_string()))
            }
            _ => {
                let ty = match self.cg.program.decls.item(def) {
                    Some(ItemSig::Const(c)) => c.ty,
                    _ => {
                        return Err(BuildError::Internal(
                            "a named constant without a signature".to_string(),
                        ));
                    }
                };
                let sym = self.cg.item_global(def, ty, "const", None, false)?;
                let ll = self.cg.llvm_ty(ty)?;
                let out = self.fresh_tmp();
                self.emit(format!("{out} = load {ll}, ptr {}", global_ref(&sym)));
                Ok((out, ll))
            }
        }
    }

    fn constant(&mut self, c: &Const) -> Result<(String, String), BuildError> {
        match c {
            Const::Int { ty, value } => {
                let ll = self.cg.llvm_ty(*ty)?;
                Ok((int_literal(&ll, *value), ll))
            }
            Const::Float { ty, value } => {
                let ll = self.cg.llvm_ty(*ty)?;
                let text = match self.cg.program.decls.types.kind(*ty) {
                    TyKind::Float(FloatTy::F32) => format_float_f32(*value),
                    _ => format_float_f64(*value),
                };
                Ok((text, ll))
            }
            Const::Bool(b) => Ok((
                if *b { "true" } else { "false" }.to_string(),
                "i1".to_string(),
            )),
            Const::Unit => Ok(("zeroinitializer".to_string(), "{}".to_string())),
            Const::Str(bytes) => {
                let sym = self.cg.add_string(bytes);
                let ty = self.cg.string_slice_ty();
                let ll = self.cg.llvm_ty(ty)?;
                let a = self.fresh_tmp();
                let b = self.fresh_tmp();
                self.emit(format!(
                    "{a} = insertvalue {ll} zeroinitializer, ptr @{sym}, 0"
                ));
                self.emit(format!(
                    "{b} = insertvalue {ll} {a}, i64 {}, 1",
                    bytes.len()
                ));
                Ok((b, ll))
            }
            Const::CStr(bytes) => {
                let mut b = bytes.clone();
                b.push(0);
                let sym = self.cg.add_string(&b);
                Ok((format!("@{sym}"), "ptr".to_string()))
            }
            Const::Zero { ty, .. } => {
                let ll = self.cg.llvm_ty(*ty)?;
                Ok(("zeroinitializer".to_string(), ll))
            }
            Const::Uninit { ty, .. } => {
                let ll = self.cg.llvm_ty(*ty)?;
                Ok(("undef".to_string(), ll))
            }
            Const::Named { def, name } => self.named_constant(*def, name),
            Const::Param { def, name } => {
                let arg = self.cg.subst.get(*def).cloned().ok_or_else(|| {
                    BuildError::Internal(format!(
                        "the constant parameter `{name}` has no value in this instance"
                    ))
                })?;
                match arg {
                    ingot_sema::ty::GenericArg::Const(ingot_sema::ty::ConstArg::Value(
                        ConstValue::Int { ty, value },
                    )) => {
                        let ty = self.cg.program.decls.types.int(ty);
                        let ty = self.cg.resolve_ty(ty);
                        let ll = self.cg.llvm_ty(ty)?;
                        Ok((int_literal(&ll, value), ll))
                    }
                    ingot_sema::ty::GenericArg::Const(ingot_sema::ty::ConstArg::Value(
                        ConstValue::Bool(b),
                    )) => Ok((
                        if b { "true" } else { "false" }.to_string(),
                        "i1".to_string(),
                    )),
                    ingot_sema::ty::GenericArg::Const(ingot_sema::ty::ConstArg::Expr(id)) => {
                        let value = self.cg.const_expr_value(id)?.ok_or_else(|| {
                            BuildError::Internal(format!(
                                "the constant expression `{name}` has no value"
                            ))
                        })?;
                        let ll = self.cg.const_expr_ll(&value)?;
                        Ok(match value {
                            CeVal::Int(_, v) => (int_literal(&ll, v), ll),
                            CeVal::Bool(b) => (if b { "true" } else { "false" }.to_string(), ll),
                        })
                    }
                    _ => self.cg.ni(format!(
                        "the constant parameter `{name}` of an unsupported kind"
                    )),
                }
            }
            Const::Expr { id, name } => {
                let value = self.cg.const_expr_value(*id)?;
                let Some(value) = value else {
                    return self.cg.ni(format!("the constant expression `{name}` (B2)"));
                };
                let ll = self.cg.const_expr_ll(&value)?;
                Ok(match value {
                    CeVal::Int(_, v) => (int_literal(&ll, v), ll),
                    CeVal::Bool(b) => (if b { "true" } else { "false" }.to_string(), ll),
                })
            }
            Const::Fn { fn_ref, .. } => {
                let (symbol, _, _, _) = self.cg.resolve_fn(fn_ref)?;
                Ok((global_ref(&symbol), "ptr".to_string()))
            }
        }
    }

    // ---------------------------------------------------------- aggregates

    fn aggregate(
        &mut self,
        kind: &AggregateKind,
        ty: TypeId,
        fields: &[Operand],
    ) -> Result<(String, String), BuildError> {
        let ty = self.cg.resolve_ty(ty);
        let ll = self.cg.llvm_ty(ty)?;
        let align = self.cg.layout(ty)?.align;
        self.cg.type_info(ty)?;
        let info = self.cg.types[&ty].clone();
        let tmp = self.alloca(&ll, align);
        self.emit(format!("store {ll} zeroinitializer, ptr {tmp}"));
        match (kind, &info.shape) {
            (
                AggregateKind::Struct(_) | AggregateKind::Tuple | AggregateKind::Closure(_),
                Shape::Aggregate { fields: infos, .. },
            ) => {
                if infos.len() != fields.len() {
                    return Err(BuildError::Internal(
                        "an aggregate literal with the wrong number of fields".to_string(),
                    ));
                }
                for (i, op) in fields.iter().enumerate() {
                    let (v, fll) = self.value(op)?;
                    let addr = self.fresh_tmp();
                    self.emit(format!(
                        "{addr} = getelementptr i8, ptr {tmp}, i64 {}",
                        infos[i].offset
                    ));
                    self.emit(format!("store {fll} {v}, ptr {addr}"));
                }
            }
            (AggregateKind::Array, Shape::Array { elem, len, .. }) => {
                let elem_ll = self.cg.llvm_ty(*elem)?;
                for (i, op) in fields.iter().enumerate() {
                    let (v, fll) = self.value(op)?;
                    let addr = self.fresh_tmp();
                    self.emit(format!(
                        "{addr} = getelementptr {ll}, ptr {tmp}, i64 0, i64 {i}"
                    ));
                    if fll != elem_ll {
                        return Err(BuildError::Internal(
                            "an array literal with a field of the wrong type".to_string(),
                        ));
                    }
                    self.emit(format!("store {fll} {v}, ptr {addr}"));
                }
                if fields.len() as u64 > *len {
                    return Err(BuildError::Internal(
                        "an array literal with too many elements".to_string(),
                    ));
                }
            }
            (
                AggregateKind::Variant { index, .. },
                Shape::Enum {
                    tag,
                    payload_offset,
                    variants,
                    niche: true,
                    ..
                },
            ) => {
                let variant = variants.get(*index).ok_or_else(|| {
                    BuildError::Internal("a variant index out of range".to_string())
                })?;
                let _ = tag;
                if let Some(payload) = variant.payload {
                    let (v, pll) = self.value(fields.first().ok_or_else(|| {
                        BuildError::Internal("a variant with no payload value".to_string())
                    })?)?;
                    let addr = self.fresh_tmp();
                    self.emit(format!(
                        "{addr} = getelementptr i8, ptr {tmp}, i64 {payload_offset}"
                    ));
                    self.emit(format!("store {pll} {v}, ptr {addr}"));
                    let _ = payload;
                } else {
                    self.emit(format!("store ptr null, ptr {tmp}"));
                }
            }
            (
                AggregateKind::Variant { index, enum_ty },
                Shape::Enum {
                    tag,
                    payload_offset,
                    variants,
                    ..
                },
            ) => {
                let variant = variants.get(*index).ok_or_else(|| {
                    BuildError::Internal("a variant index out of range".to_string())
                })?;
                let tag_ty = self.cg.llvm_ty(*enum_ty)?;
                let _ = tag_ty;
                let tag_text = int_literal(tag, variant.tag);
                self.emit(format!("store {tag} {tag_text}, ptr {tmp}"));
                if let Some(payload) = variant.payload {
                    let (v, pll) = self.value(fields.first().ok_or_else(|| {
                        BuildError::Internal("a variant with no payload value".to_string())
                    })?)?;
                    let addr = self.fresh_tmp();
                    self.emit(format!(
                        "{addr} = getelementptr i8, ptr {tmp}, i64 {payload_offset}"
                    ));
                    self.emit(format!("store {pll} {v}, ptr {addr}"));
                    let _ = payload;
                }
            }
            _ => return self.cg.ni("this aggregate (B2)"),
        }
        let out = self.fresh_tmp();
        self.emit(format!("{out} = load {ll}, ptr {tmp}"));
        Ok((out, ll))
    }

    // ------------------------------------------------------------- slices

    fn slice_len(&mut self, place: &Place) -> Result<(String, String), BuildError> {
        let ty = self.place_type(place)?;
        let addr = self.place_addr(place)?;
        match self.cg.program.decls.types.kind(ty).clone() {
            TyKind::Slice(..) => {
                let p = self.fresh_tmp();
                self.emit(format!("{p} = getelementptr i8, ptr {addr}, i64 8"));
                let out = self.fresh_tmp();
                self.emit(format!("{out} = load i64, ptr {p}"));
                Ok((out, "i64".to_string()))
            }
            TyKind::Array(_, n) => {
                let len = self
                    .cg
                    .program
                    .decls
                    .types
                    .const_arg_value(n)
                    .and_then(|v| match v {
                        ConstValue::Int { value, .. } if value >= 0 => Some(value),
                        _ => None,
                    })
                    .ok_or_else(|| {
                        BuildError::Internal("an array length with no value".to_string())
                    })?;
                Ok((len.to_string(), "i64".to_string()))
            }
            _ => Err(BuildError::Internal(
                "`.len` of something that is not a slice or array".to_string(),
            )),
        }
    }

    fn slice_ptr(&mut self, place: &Place) -> Result<(String, String), BuildError> {
        let addr = self.place_addr(place)?;
        let out = self.fresh_tmp();
        self.emit(format!("{out} = load ptr, ptr {addr}"));
        Ok((out, "ptr".to_string()))
    }

    fn slice_value(
        &mut self,
        base: &Place,
        lo: &Operand,
        hi: &Operand,
        ty: TypeId,
    ) -> Result<(String, String), BuildError> {
        let base_ty = self.place_type(base)?;
        let addr = self.place_addr(base)?;
        let (elem, start) = match self.cg.program.decls.types.kind(base_ty).clone() {
            TyKind::Array(elem, _) => {
                let arr = self.cg.llvm_ty(base_ty)?;
                let p = self.fresh_tmp();
                self.emit(format!(
                    "{p} = getelementptr {arr}, ptr {addr}, i64 0, i64 0"
                ));
                (elem, p)
            }
            TyKind::Slice(_, elem) => {
                let p = self.fresh_tmp();
                self.emit(format!("{p} = load ptr, ptr {addr}"));
                (elem, p)
            }
            _ => {
                return Err(BuildError::Internal(
                    "a slice of something that is not an array or slice".to_string(),
                ));
            }
        };
        let lo = self.index_i64(lo)?;
        let hi = self.index_i64(hi)?;
        let elem_ll = self.cg.llvm_ty(elem)?;
        let p = self.fresh_tmp();
        self.emit(format!(
            "{p} = getelementptr {elem_ll}, ptr {start}, i64 {lo}"
        ));
        let len = self.fresh_tmp();
        self.emit(format!("{len} = sub i64 {hi}, {lo}"));
        let ll = self.cg.llvm_ty(ty)?;
        let a = self.fresh_tmp();
        let b = self.fresh_tmp();
        self.emit(format!(
            "{a} = insertvalue {ll} zeroinitializer, ptr {p}, 0"
        ));
        self.emit(format!("{b} = insertvalue {ll} {a}, i64 {len}, 1"));
        Ok((b, ll))
    }

    fn slice_elem(
        &mut self,
        base: &Place,
        index: &Operand,
    ) -> Result<(String, String), BuildError> {
        let base_ty = self.place_type(base)?;
        let addr = self.place_addr(base)?;
        let elem = match self.cg.program.decls.types.kind(base_ty).clone() {
            TyKind::Slice(_, elem) => elem,
            _ => {
                return Err(BuildError::Internal(
                    "an element of something that is not a slice".to_string(),
                ));
            }
        };
        let p = self.fresh_tmp();
        self.emit(format!("{p} = load ptr, ptr {addr}"));
        let idx = self.index_i64(index)?;
        let elem_ll = self.cg.llvm_ty(elem)?;
        let out = self.fresh_tmp();
        self.emit(format!(
            "{out} = getelementptr {elem_ll}, ptr {p}, i64 {idx}"
        ));
        Ok((out, "ptr".to_string()))
    }

    fn index_i64(&mut self, op: &Operand) -> Result<String, BuildError> {
        let ty = self.operand_ty(op)?;
        let (v, ll) = self.value(op)?;
        if ll == "i64" {
            return Ok(v);
        }
        let out = self.fresh_tmp();
        if let TyKind::Int(i) = self.cg.program.decls.types.kind(ty) {
            if i.bits() < 64 {
                let opcode = if i.signed() { "sext" } else { "zext" };
                self.emit(format!("{out} = {opcode} {ll} {v} to i64"));
                return Ok(out);
            }
            self.emit(format!("{out} = trunc {ll} {v} to i64"));
            return Ok(out);
        }
        Err(BuildError::Internal(
            "an index that is not an integer".to_string(),
        ))
    }

    fn discriminant(&mut self, place: &Place) -> Result<(String, String), BuildError> {
        let ty = self.place_type(place)?;
        let addr = self.place_addr(place)?;
        self.cg.type_info(ty)?;
        let info = self.cg.types[&ty].clone();
        match info.shape {
            Shape::Enum { niche: true, .. } => {
                let v = self.fresh_tmp();
                self.emit(format!("{v} = load ptr, ptr {addr}"));
                let c = self.fresh_tmp();
                self.emit(format!("{c} = icmp eq ptr {v}, null"));
                let out = self.fresh_tmp();
                self.emit(format!("{out} = select i1 {c}, i8 0, i8 1"));
                Ok((out, "i8".to_string()))
            }
            Shape::Enum { tag, .. } => {
                let out = self.fresh_tmp();
                self.emit(format!("{out} = load {tag}, ptr {addr}"));
                Ok((out, tag))
            }
            _ => Err(BuildError::Internal(
                "a discriminant of something that is not an enum".to_string(),
            )),
        }
    }
}

fn format_float_f64(v: f64) -> String {
    if v.is_nan() {
        "0x7FF8000000000000".to_string()
    } else if v == f64::INFINITY {
        "0x7FF0000000000000".to_string()
    } else if v == f64::NEG_INFINITY {
        "0xFFF0000000000000".to_string()
    } else {
        float_text(&format!("{v:?}"))
    }
}

fn float_text(text: &str) -> String {
    if let Some(e) = text.find(['e', 'E'])
        && !text[..e].contains('.')
    {
        let mut out = String::new();
        out.push_str(&text[..e]);
        out.push_str(".0");
        out.push_str(&text[e..]);
        return out;
    }
    text.to_string()
}

fn format_float_f32(v: f64) -> String {
    let v = v as f32;
    if v.is_nan() {
        "0x7FC00000".to_string()
    } else if v == f32::INFINITY {
        "0x7F800000".to_string()
    } else if v == f32::NEG_INFINITY {
        "0xFF800000".to_string()
    } else {
        float_text(&format!("{:?}", v as f64))
    }
}

impl<'a, 'p> FnGen<'a, 'p> {
    // ------------------------------------------------------------ arithmetic

    fn binary(
        &mut self,
        op: BinOp,
        lhs: &Operand,
        rhs: &Operand,
        result_ty: TypeId,
    ) -> Result<(String, String), BuildError> {
        let lty = self.operand_ty(lhs)?;
        let rty = self.operand_ty(rhs)?;
        let (a, all) = self.value(lhs)?;
        let (b, bll) = self.value(rhs)?;
        let float = matches!(self.cg.program.decls.types.kind(lty), TyKind::Float(_));
        let out = self.fresh_tmp();
        let bool_ty = self.cg.program.decls.types.bool_;
        match op {
            BinOp::Add
            | BinOp::Sub
            | BinOp::Mul
            | BinOp::AddWrap
            | BinOp::SubWrap
            | BinOp::MulWrap => {
                let opcode = match op {
                    BinOp::Add | BinOp::AddWrap => "add",
                    BinOp::Sub | BinOp::SubWrap => "fsub",
                    _ => "mul",
                };
                let opcode = if float {
                    match op {
                        BinOp::Add | BinOp::AddWrap => "fadd",
                        BinOp::Sub | BinOp::SubWrap => "fsub",
                        _ => "fmul",
                    }
                } else if matches!(op, BinOp::Sub | BinOp::SubWrap) {
                    "sub"
                } else {
                    opcode
                };
                self.emit(format!("{out} = {opcode} {all} {a}, {b}"));
                Ok((out, all))
            }
            BinOp::AddSat | BinOp::SubSat | BinOp::MulSat => {
                self.saturating(op, &a, &b, &all, &lty)
            }
            BinOp::Div | BinOp::Rem => {
                let signed = self.int_signed(lty);
                if float {
                    let opcode = if matches!(op, BinOp::Div) {
                        "fdiv"
                    } else {
                        "frem"
                    };
                    self.emit(format!("{out} = {opcode} {all} {a}, {b}"));
                    return Ok((out, all));
                }
                // The check statement is emitted after the operation by phase 8, so
                // the division here must be safe for every operand the check can
                // still reject: LLVM's `sdiv` is undefined for 0 and MIN/-1.
                let zero = self.fresh_tmp();
                self.emit(format!("{zero} = icmp eq {all} {b}, 0"));
                let mut bad = zero;
                if signed {
                    let minus_one = int_literal(&all, -1);
                    let one = self.fresh_tmp();
                    self.emit(format!("{one} = icmp eq {all} {b}, {minus_one}"));
                    let min = int_literal(&all, int_min_i128(&all));
                    let at_min = self.fresh_tmp();
                    self.emit(format!("{at_min} = icmp eq {all} {a}, {min}"));
                    let overflow = self.fresh_tmp();
                    self.emit(format!("{overflow} = and i1 {one}, {at_min}"));
                    let both = self.fresh_tmp();
                    self.emit(format!("{both} = or i1 {bad}, {overflow}"));
                    bad = both;
                }
                let safe = self.fresh_tmp();
                self.emit(format!("{safe} = select i1 {bad}, {all} 1, {all} {b}"));
                let opcode = match (op, signed) {
                    (BinOp::Div, true) => "sdiv",
                    (BinOp::Div, false) => "udiv",
                    (BinOp::Rem, true) => "srem",
                    (BinOp::Rem, false) => "urem",
                    _ => unreachable!(),
                };
                self.emit(format!("{out} = {opcode} {all} {a}, {safe}"));
                Ok((out, all))
            }
            BinOp::Shl | BinOp::Shr => {
                let shift = self.coerce_int(&b, &bll, &all)?;
                let bits = int_bits(&all);
                let too_big = self.fresh_tmp();
                self.emit(format!("{too_big} = icmp uge {all} {shift}, {bits}"));
                let mut bad = too_big;
                if self.int_signed(rty) {
                    let neg = self.fresh_tmp();
                    self.emit(format!("{neg} = icmp slt {all} {shift}, 0"));
                    let both = self.fresh_tmp();
                    self.emit(format!("{both} = or i1 {neg}, {bad}"));
                    bad = both;
                }
                let safe = self.fresh_tmp();
                self.emit(format!("{safe} = select i1 {bad}, {all} 0, {all} {shift}"));
                let opcode = match op {
                    BinOp::Shl => "shl",
                    BinOp::Shr if self.int_signed(lty) => "ashr",
                    _ => "lshr",
                };
                self.emit(format!("{out} = {opcode} {all} {a}, {safe}"));
                Ok((out, all))
            }
            BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor => {
                let opcode = match op {
                    BinOp::BitAnd => "and",
                    BinOp::BitOr => "or",
                    _ => "xor",
                };
                self.emit(format!("{out} = {opcode} {all} {a}, {b}"));
                Ok((out, all))
            }
            BinOp::And | BinOp::Or => {
                let opcode = if matches!(op, BinOp::And) {
                    "and"
                } else {
                    "or"
                };
                self.emit(format!("{out} = {opcode} i1 {a}, {b}"));
                Ok((out, "i1".to_string()))
            }
            BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
                let _ = result_ty == bool_ty;
                if let Some(tag) = self.cg.tag_only_enum(lty) {
                    let tag_of = |this: &mut Self, v: &str, ll: &str| {
                        if ll == tag {
                            v.to_string()
                        } else {
                            let t = this.fresh_tmp();
                            this.emit(format!("{t} = extractvalue {ll} {v}, 0"));
                            t
                        }
                    };
                    let ta = tag_of(self, &a, &all);
                    let tb = tag_of(self, &b, &bll);
                    let signed = match self.cg.program.decls.types.kind(lty) {
                        TyKind::Adt(def, _) => self
                            .cg
                            .program
                            .decls
                            .enum_sig(*def)
                            .is_some_and(|s| s.tag_ty.signed()),
                        _ => false,
                    };
                    let pred = match (op, signed) {
                        (BinOp::Eq, _) => "eq",
                        (BinOp::Ne, _) => "ne",
                        (BinOp::Lt, true) => "slt",
                        (BinOp::Lt, false) => "ult",
                        (BinOp::Le, true) => "sle",
                        (BinOp::Le, false) => "ule",
                        (BinOp::Gt, true) => "sgt",
                        (BinOp::Gt, false) => "ugt",
                        (BinOp::Ge, true) => "sge",
                        (BinOp::Ge, false) => "uge",
                        _ => unreachable!(),
                    };
                    self.emit(format!("{out} = icmp {pred} {tag} {ta}, {tb}"));
                    return Ok((out, "i1".to_string()));
                }
                if float {
                    let pred = match op {
                        BinOp::Eq => "oeq",
                        BinOp::Ne => "une",
                        BinOp::Lt => "olt",
                        BinOp::Le => "ole",
                        BinOp::Gt => "ogt",
                        _ => "oge",
                    };
                    self.emit(format!("{out} = fcmp {pred} {all} {a}, {b}"));
                } else {
                    let signed = self.int_signed(lty);
                    let pred = match (op, signed) {
                        (BinOp::Eq, _) => "eq",
                        (BinOp::Ne, _) => "ne",
                        (BinOp::Lt, true) => "slt",
                        (BinOp::Lt, false) => "ult",
                        (BinOp::Le, true) => "sle",
                        (BinOp::Le, false) => "ule",
                        (BinOp::Gt, true) => "sgt",
                        (BinOp::Gt, false) => "ugt",
                        (BinOp::Ge, true) => "sge",
                        (BinOp::Ge, false) => "uge",
                        _ => unreachable!(),
                    };
                    self.emit(format!("{out} = icmp {pred} {all} {a}, {b}"));
                }
                Ok((out, "i1".to_string()))
            }
        }
    }

    fn coerce_int(&mut self, v: &str, from: &str, to: &str) -> Result<String, BuildError> {
        if from == to {
            return Ok(v.to_string());
        }
        let out = self.fresh_tmp();
        let (fb, tb) = (int_bits(from), int_bits(to));
        if fb < tb {
            self.emit(format!("{out} = sext {from} {v} to {to}"));
        } else if fb > tb {
            self.emit(format!("{out} = trunc {from} {v} to {to}"));
        } else {
            self.emit(format!("{out} = bitcast {from} {v} to {to}"));
        }
        Ok(out)
    }

    fn int_signed(&self, ty: TypeId) -> bool {
        matches!(
            self.cg.program.decls.types.kind(ty),
            TyKind::Int(i) if i.signed()
        )
    }

    fn saturating(
        &mut self,
        op: BinOp,
        a: &str,
        b: &str,
        ll: &str,
        ty: &TypeId,
    ) -> Result<(String, String), BuildError> {
        let signed = self.int_signed(*ty);
        let kind = match op {
            BinOp::AddSat => "add",
            BinOp::SubSat => "sub",
            _ => "mul",
        };
        let prefix = if signed { "s" } else { "u" };
        let intrinsic = format!("llvm.{prefix}{kind}.with.overflow.{ll}");
        self.cg.declare_overflow(&intrinsic, ll)?;
        let pair = self.fresh_tmp();
        self.emit(format!(
            "{pair} = call {{ {ll}, i1 }} @{intrinsic}({ll} {a}, {ll} {b})"
        ));
        let value = self.fresh_tmp();
        self.emit(format!("{value} = extractvalue {{ {ll}, i1 }} {pair}, 0"));
        let over = self.fresh_tmp();
        self.emit(format!("{over} = extractvalue {{ {ll}, i1 }} {pair}, 1"));
        let out = self.fresh_tmp();
        let min = int_min_literal(ll, signed);
        let max = int_max_literal(ll, signed);
        match (kind, signed) {
            ("add", true) => {
                let neg = self.fresh_tmp();
                self.emit(format!("{neg} = icmp slt {ll} {b}, 0"));
                let lim = self.fresh_tmp();
                self.emit(format!("{lim} = select i1 {neg}, {ll} {min}, {ll} {max}"));
                self.emit(format!(
                    "{out} = select i1 {over}, {ll} {lim}, {ll} {value}"
                ));
            }
            ("mul", true) => {
                let b_neg = self.fresh_tmp();
                self.emit(format!("{b_neg} = icmp slt {ll} {b}, 0"));
                let a_neg = self.fresh_tmp();
                self.emit(format!("{a_neg} = icmp slt {ll} {a}, 0"));
                let differ = self.fresh_tmp();
                self.emit(format!("{differ} = xor i1 {b_neg}, {a_neg}"));
                let lim = self.fresh_tmp();
                self.emit(format!(
                    "{lim} = select i1 {differ}, {ll} {min}, {ll} {max}"
                ));
                self.emit(format!(
                    "{out} = select i1 {over}, {ll} {lim}, {ll} {value}"
                ));
            }
            ("sub", true) => {
                let pos = self.fresh_tmp();
                self.emit(format!("{pos} = icmp sgt {ll} {b}, 0"));
                let lim = self.fresh_tmp();
                self.emit(format!("{lim} = select i1 {pos}, {ll} {min}, {ll} {max}"));
                self.emit(format!(
                    "{out} = select i1 {over}, {ll} {lim}, {ll} {value}"
                ));
            }
            (_, false) => {
                let lim = if kind == "sub" { min } else { max };
                self.emit(format!(
                    "{out} = select i1 {over}, {ll} {lim}, {ll} {value}"
                ));
            }
            // `kind` is `add`, `sub` or `mul`, and each is handled for both signednesses.
            _ => {
                return Err(BuildError::Internal(
                    "a saturating operation that is not `add`, `sub` or `mul`".to_string(),
                ));
            }
        }
        Ok((out, ll.to_string()))
    }

    // --------------------------------------------------------------- casts

    fn dyn_cast(
        &mut self,
        operand: &Operand,
        coercion: &ingot_cfg::DynCoercionKind,
        ty: TypeId,
    ) -> Result<(String, String), BuildError> {
        let (data, dll) = self.value(operand)?;
        if dll != "ptr" {
            return Err(BuildError::Internal(
                "a dynamic value from something that is not a borrow".to_string(),
            ));
        }
        let key = match coercion {
            ingot_cfg::DynCoercionKind::Interface { coercion, .. } => {
                let self_ty = self
                    .cg
                    .program
                    .decls
                    .instantiate(coercion.self_ty, &self.cg.subst);
                let args: Vec<ingot_sema::ty::GenericArg> = coercion
                    .interface_args
                    .iter()
                    .map(|a| self.cg.program.decls.types.subst_arg(a, &self.cg.subst))
                    .collect();
                InstanceKey {
                    owner: coercion.interface,
                    body: InstanceBody::MethodTable {
                        ty: self_ty,
                        interface: coercion.interface,
                        args,
                    },
                    subst: Subst::new(),
                }
            }
            ingot_cfg::DynCoercionKind::Fn(fd) => InstanceKey {
                owner: self.body.cfg.key.owner,
                body: InstanceBody::DynFnTable {
                    ty: self
                        .cg
                        .program
                        .decls
                        .instantiate(fd.self_ty, &self.cg.subst),
                    kind: fd.kind,
                },
                subst: Subst::new(),
            },
        };
        let table = self.cg.symbols.get(&key).cloned().ok_or_else(|| {
            BuildError::Internal("a dynamic table was not discovered".to_string())
        })?;
        let ll = self.cg.llvm_ty(ty)?;
        let a = self.fresh_tmp();
        let b = self.fresh_tmp();
        self.emit(format!(
            "{a} = insertvalue {ll} zeroinitializer, ptr {data}, 0"
        ));
        self.emit(format!(
            "{b} = insertvalue {ll} {a}, ptr {}, 1",
            global_ref(&table)
        ));
        Ok((b, ll))
    }

    fn cast(
        &mut self,
        kind: CastKind,
        operand: &Operand,
        target: TypeId,
    ) -> Result<(String, String), BuildError> {
        let sty = self.operand_ty(operand)?;
        let target = self.cg.resolve_ty(target);
        let (v, sll) = self.value(operand)?;
        let tll = self.cg.llvm_ty(target)?;
        let out = self.fresh_tmp();
        match kind {
            CastKind::Identity => Ok((v, sll)),
            CastKind::IntWiden => {
                if sll == tll {
                    return Ok((v, sll));
                }
                let signed =
                    matches!(self.cg.program.decls.types.kind(sty), TyKind::Int(i) if i.signed());
                let opcode = if signed { "sext" } else { "zext" };
                self.emit(format!("{out} = {opcode} {sll} {v} to {tll}"));
                Ok((out, tll))
            }
            CastKind::ToFloat => {
                if matches!(self.cg.program.decls.types.kind(sty), TyKind::Float(_)) {
                    self.emit(format!("{out} = fpext {sll} {v} to {tll}"));
                } else {
                    let signed = matches!(self.cg.program.decls.types.kind(sty), TyKind::Int(i) if i.signed());
                    let opcode = if signed { "sitofp" } else { "uitofp" };
                    self.emit(format!("{out} = {opcode} {sll} {v} to {tll}"));
                }
                Ok((out, tll))
            }
            CastKind::BoolToInt => {
                self.emit(format!("{out} = zext i1 {v} to {tll}"));
                Ok((out, tll))
            }
            CastKind::EnumToTag => {
                let (tag, tag_ll) = match operand {
                    Operand::Place(p, _) => self.discriminant(p)?,
                    Operand::Const(Const::Zero { ty, .. }) => {
                        let (t, ll) = self.discriminant_const(*ty)?;
                        (t, ll)
                    }
                    _ => return self.cg.ni("this enum conversion (B2)"),
                };
                let out = if tag_ll == tll {
                    tag
                } else {
                    let o = self.fresh_tmp();
                    if int_bits(&tag_ll) < int_bits(&tll) {
                        self.emit(format!("{o} = zext {tag_ll} {tag} to {tll}"));
                    } else {
                        self.emit(format!("{o} = trunc {tag_ll} {tag} to {tll}"));
                    }
                    o
                };
                Ok((out, tll))
            }
            CastKind::RefToPtr | CastKind::PtrToPtr => Ok((v, tll)),
            CastKind::PtrToInt => {
                self.emit(format!("{out} = ptrtoint {sll} {v} to {tll}"));
                Ok((out, tll))
            }
            CastKind::IntToPtr => {
                self.emit(format!("{out} = inttoptr {sll} {v} to {tll}"));
                Ok((out, tll))
            }
        }
    }

    fn discriminant_const(&mut self, ty: TypeId) -> Result<(String, String), BuildError> {
        self.cg.type_info(ty)?;
        match &self.cg.types[&ty].shape {
            // A niche `?&T` has no declared tag; its discriminant is the i8 the
            // run-time path produces.
            Shape::Enum { niche: true, .. } => Ok(("0".to_string(), "i8".to_string())),
            Shape::Enum { tag, .. } => Ok(("0".to_string(), tag.clone())),
            _ => Err(BuildError::Internal(
                "a discriminant of a non-enum".to_string(),
            )),
        }
    }
}

fn int_bits(ll: &str) -> u32 {
    ll.strip_prefix('i')
        .and_then(|s| s.parse().ok())
        .unwrap_or(1)
}

fn int_min_literal(ll: &str, signed: bool) -> String {
    let bits = int_bits(ll);
    if !signed || bits == 0 {
        return "0".to_string();
    }
    if bits >= 128 {
        return i128::MIN.to_string();
    }
    (-(1i128 << (bits - 1))).to_string()
}

fn int_max_literal(ll: &str, signed: bool) -> String {
    let bits = int_bits(ll);
    if bits >= 128 {
        return if signed {
            i128::MAX.to_string()
        } else {
            u128::MAX.to_string()
        };
    }
    if signed {
        ((1i128 << (bits - 1)) - 1).to_string()
    } else {
        ((1i128 << bits) - 1).to_string()
    }
}

/// How a value of an `AtomicValue` type goes through an atomic instruction. The
/// instruction works on the value's bytes as a plain integer or pointer of the type's
/// size, and the register form of some types is not that: `bool` is `i1` in registers
/// and one byte in memory, and a payload-free enum is a struct around its tag.
struct AtomicRep {
    /// The LLVM type of the value in registers.
    reg: String,
    /// The LLVM type the atomic instruction works on.
    mem: String,
    /// The alignment given to the instruction: the type's size, its natural alignment.
    align: u64,
    wrap: AtomicWrap,
}

enum AtomicWrap {
    /// An integer or a pointer: the same in registers and in memory.
    Plain,
    /// `bool`: `zext` to a byte, `trunc` back.
    Bool,
    /// A payload-free enum: the tag is field 0 of the register struct.
    Enum,
}

/// An `order` operand of an atomic builtin, as far as the back end knows it.
enum OrderArg {
    /// A variant known when the code is generated: its LLVM ordering.
    Known(&'static str),
    /// A value of the enum, read at run time: the tag, its LLVM type, and the LLVM
    /// ordering of each tag.
    Dynamic {
        tag: String,
        tag_ll: String,
        arms: Vec<(i128, &'static str)>,
    },
}

/// What `atomic_dispatch` calls for each combination of orders: it emits one atomic
/// instruction and gives its value, if it has one.
type AtomicLeaf<'f, 'a, 'p> =
    dyn FnMut(&mut FnGen<'a, 'p>, &[&'static str]) -> Result<Option<String>, BuildError> + 'f;

/// The LLVM ordering of an order variant of `std.sync` (02 §15).
fn llvm_ordering(variant: &str) -> Option<&'static str> {
    Some(match variant {
        "relaxed" => "monotonic",
        "acquire" => "acquire",
        "release" => "release",
        "acq_rel" => "acq_rel",
        "seq_cst" => "seq_cst",
        _ => return None,
    })
}

/// The value an atomic instruction gave, when the builtin has a result.
fn atomic_value(value: Option<String>) -> Result<String, BuildError> {
    value
        .ok_or_else(|| BuildError::Internal("an atomic instruction that gave no value".to_string()))
}

fn atomic_operand<'x>(
    name: &str,
    args: &'x [ingot_cfg::BuiltinArg],
    i: usize,
) -> Result<&'x Operand, BuildError> {
    match args.get(i) {
        Some(ingot_cfg::BuiltinArg::Value(v)) => Ok(v),
        _ => Err(BuildError::Internal(format!(
            "`@{name}` without its operand {i}"
        ))),
    }
}

impl<'a, 'p> FnGen<'a, 'p> {
    // ------------------------------------------------------------- builtins

    fn builtin(
        &mut self,
        name: &str,
        type_arg: Option<TypeId>,
        args: &[ingot_cfg::BuiltinArg],
        result_ty: TypeId,
    ) -> Result<(String, String), BuildError> {
        let arg = |i: usize| {
            args.get(i).and_then(|a| match a {
                ingot_cfg::BuiltinArg::Value(v) => Some(v),
                ingot_cfg::BuiltinArg::Place(_) => None,
            })
        };
        let type_arg = type_arg.map(|t| self.cg.resolve_ty(t));
        match name {
            "forget" => {
                let op = arg(0)
                    .ok_or_else(|| BuildError::Internal("forget without a value".to_string()))?;
                self.value(op)
            }
            "sizeof" | "alignof" => {
                let target = type_arg.ok_or_else(|| {
                    BuildError::Internal(format!("`@{name}` without a type argument"))
                })?;
                let layout = self.cg.layout(target)?;
                let value = if name == "sizeof" {
                    layout.size
                } else {
                    layout.align
                };
                Ok((value.to_string(), "i64".to_string()))
            }
            "assume" => Ok(("zeroinitializer".to_string(), "{}".to_string())),
            "slice_from_raw" => {
                let p = arg(0).ok_or_else(|| {
                    BuildError::Internal("`@slice_from_raw` without a pointer".to_string())
                })?;
                let n = arg(1).ok_or_else(|| {
                    BuildError::Internal("`@slice_from_raw` without a length".to_string())
                })?;
                let (pv, pll) = self.value(p)?;
                if pll != "ptr" {
                    return Err(BuildError::Internal(
                        "`@slice_from_raw` got a non-pointer".to_string(),
                    ));
                }
                let (nv, nll) = self.value(n)?;
                if nll != "i64" {
                    return Err(BuildError::Internal(
                        "`@slice_from_raw` got a length that is not i64".to_string(),
                    ));
                }
                let ll = self.cg.llvm_ty(result_ty)?;
                let a = self.fresh_tmp();
                let b = self.fresh_tmp();
                self.emit(format!(
                    "{a} = insertvalue {ll} zeroinitializer, ptr {pv}, 0"
                ));
                self.emit(format!("{b} = insertvalue {ll} {a}, i64 {nv}, 1"));
                Ok((b, ll))
            }
            "borrow_from_raw" => {
                let p = arg(0).ok_or_else(|| {
                    BuildError::Internal("`@borrow_from_raw` without a pointer".to_string())
                })?;
                self.value(p)
            }
            "offset" => {
                // `p + n * @sizeof(T)`, wrapping and never trapping (02 section 15): a
                // byte `getelementptr` without `inbounds` (pipeline section 7), scaled by
                // the layout's size, which is what `@sizeof` says.
                let p = arg(0).ok_or_else(|| {
                    BuildError::Internal("`@offset` without a pointer".to_string())
                })?;
                let n = arg(1)
                    .ok_or_else(|| BuildError::Internal("`@offset` without a count".to_string()))?;
                let result_ty = self.cg.resolve_ty(result_ty);
                let elem = match self.cg.program.decls.types.kind(result_ty) {
                    TyKind::RawPtr(_, elem) => *elem,
                    _ => {
                        return Err(BuildError::Internal(
                            "`@offset` with a result that is not a raw pointer".to_string(),
                        ));
                    }
                };
                let size = self.cg.layout(elem)?.size;
                let (pv, pll) = self.value(p)?;
                let (nv, nll) = self.value(n)?;
                if pll != "ptr" || nll != "i64" {
                    return Err(BuildError::Internal(
                        "`@offset` got a pointer or a count of the wrong type".to_string(),
                    ));
                }
                let bytes = if size == 1 {
                    nv
                } else {
                    let scaled = self.fresh_tmp();
                    self.emit(format!("{scaled} = mul i64 {nv}, {size}"));
                    scaled
                };
                let out = self.fresh_tmp();
                self.emit(format!("{out} = getelementptr i8, ptr {pv}, i64 {bytes}"));
                Ok((out, "ptr".to_string()))
            }
            "wrap" | "narrow" | "sat" | "nearest" | "bits" => {
                let target = type_arg.ok_or_else(|| {
                    BuildError::Internal(format!("`@{name}` without a type argument"))
                })?;
                let op = arg(0)
                    .ok_or_else(|| BuildError::Internal(format!("`@{name}` without a value")))?;
                self.convert_builtin(name, op, target)
            }
            "test_env" => {
                // The test's one parameter (`render` names it `%arg0`): repeated calls
                // give the same pointer, and no global holds it (rt.md §1).
                if !self.cg.is_test_body(self.body) {
                    return Err(BuildError::Internal(
                        "`@test_env()` outside a test body".to_string(),
                    ));
                }
                Ok(("%arg0".to_string(), "ptr".to_string()))
            }
            "atomic_load"
            | "atomic_store"
            | "atomic_swap"
            | "atomic_cmpxchg"
            | "atomic_cmpxchg_weak"
            | "atomic_add"
            | "atomic_sub"
            | "atomic_and"
            | "atomic_or"
            | "atomic_xor"
            | "fence" => self.atomic_builtin(name, args, result_ty),
            "debug" => self.cg.ni("`@debug` (R5)"),
            other => self.cg.ni(format!("the builtin `@{other}` (B2)")),
        }
    }

    // -------------------------------------------------------------- atomics

    /// `@atomic_*` and `@fence` (02 §15, pipeline §7): one atomic instruction with the
    /// given ordering. An order that is not known here becomes a branch over its values
    /// with one instruction in each arm (`atomic_dispatch`).
    fn atomic_builtin(
        &mut self,
        name: &str,
        args: &[ingot_cfg::BuiltinArg],
        result_ty: TypeId,
    ) -> Result<(String, String), BuildError> {
        let void = ("zeroinitializer".to_string(), "{}".to_string());
        if name == "fence" {
            let order = self.order_arg(atomic_operand(name, args, 0)?)?;
            // A relaxed fence orders nothing, so it emits nothing.
            self.atomic_dispatch(&[order], &mut Vec::new(), None, &mut |fg, ord| {
                if ord[0] != "monotonic" {
                    fg.emit(format!("fence {}", ord[0]));
                }
                Ok(None)
            })?;
            return Ok(void);
        }
        let (ptr, ptr_ll) = self.value(atomic_operand(name, args, 0)?)?;
        if ptr_ll != "ptr" {
            return Err(BuildError::Internal(format!(
                "`@{name}` got a pointer operand that is not a pointer"
            )));
        }
        // `T` is the loaded value's type, or the type of the first value operand.
        let value_ty = if name == "atomic_load" {
            result_ty
        } else {
            self.operand_ty(atomic_operand(name, args, 1)?)?
        };
        let rep = self.atomic_rep(value_ty)?;
        let (mem, align) = (rep.mem.clone(), rep.align);
        match name {
            "atomic_load" => {
                let order = self.order_arg(atomic_operand(name, args, 1)?)?;
                let loaded =
                    self.atomic_dispatch(&[order], &mut Vec::new(), Some(&mem), &mut |fg, ord| {
                        let out = fg.fresh_tmp();
                        fg.emit(format!(
                            "{out} = load atomic {mem}, ptr {ptr} {}, align {align}",
                            ord[0]
                        ));
                        Ok(Some(out))
                    })?;
                let v = self.atomic_from_mem(&rep, &atomic_value(loaded)?);
                Ok((v, rep.reg))
            }
            "atomic_store" => {
                let (v, _) = self.value(atomic_operand(name, args, 1)?)?;
                let v = self.atomic_to_mem(&rep, &v)?;
                let order = self.order_arg(atomic_operand(name, args, 2)?)?;
                self.atomic_dispatch(&[order], &mut Vec::new(), None, &mut |fg, ord| {
                    fg.emit(format!(
                        "store atomic {mem} {v}, ptr {ptr} {}, align {align}",
                        ord[0]
                    ));
                    Ok(None)
                })?;
                Ok(void)
            }
            "atomic_swap" | "atomic_add" | "atomic_sub" | "atomic_and" | "atomic_or"
            | "atomic_xor" => {
                let op = match name {
                    "atomic_swap" => "xchg",
                    other => &other["atomic_".len()..],
                };
                let (v, _) = self.value(atomic_operand(name, args, 1)?)?;
                let v = self.atomic_to_mem(&rep, &v)?;
                let order = self.order_arg(atomic_operand(name, args, 2)?)?;
                let previous =
                    self.atomic_dispatch(&[order], &mut Vec::new(), Some(&mem), &mut |fg, ord| {
                        let out = fg.fresh_tmp();
                        fg.emit(format!(
                            "{out} = atomicrmw {op} ptr {ptr}, {mem} {v} {}, align {align}",
                            ord[0]
                        ));
                        Ok(Some(out))
                    })?;
                let v = self.atomic_from_mem(&rep, &atomic_value(previous)?);
                Ok((v, rep.reg))
            }
            "atomic_cmpxchg" | "atomic_cmpxchg_weak" => {
                let weak = if name == "atomic_cmpxchg_weak" {
                    "weak "
                } else {
                    ""
                };
                let (expected, _) = self.value(atomic_operand(name, args, 1)?)?;
                let expected = self.atomic_to_mem(&rep, &expected)?;
                let (new, _) = self.value(atomic_operand(name, args, 2)?)?;
                let new = self.atomic_to_mem(&rep, &new)?;
                let success = self.order_arg(atomic_operand(name, args, 3)?)?;
                let failure = self.order_arg(atomic_operand(name, args, 4)?)?;
                // LLVM accepts a failure order stronger than the success order, so
                // neither is adjusted for the other.
                let pair_ll = format!("{{ {mem}, i1 }}");
                let pair = self.atomic_dispatch(
                    &[success, failure],
                    &mut Vec::new(),
                    Some(&pair_ll),
                    &mut |fg, ord| {
                        let out = fg.fresh_tmp();
                        let (success, failure) = (ord[0], ord[1]);
                        fg.emit(format!(
                            "{out} = cmpxchg {weak}ptr {ptr}, {mem} {expected}, {mem} {new} \
                             {success} {failure}, align {align}"
                        ));
                        Ok(Some(out))
                    },
                )?;
                let pair = atomic_value(pair)?;
                let observed = self.fresh_tmp();
                self.emit(format!("{observed} = extractvalue {pair_ll} {pair}, 0"));
                let swapped = self.fresh_tmp();
                self.emit(format!("{swapped} = extractvalue {pair_ll} {pair}, 1"));
                let observed = self.atomic_from_mem(&rep, &observed);
                self.cmpxchg_result(result_ty, &swapped, &observed, &rep.reg)
            }
            other => Err(BuildError::Internal(format!(
                "`@{other}` is not an atomic builtin"
            ))),
        }
    }

    /// The `T ! T` of a compare-exchange: the previous value is `Ok` when the exchange
    /// happened and the observed value is `Err` when it did not, and both are the one
    /// value the instruction returned, so only the tag depends on `swapped`.
    fn cmpxchg_result(
        &mut self,
        result_ty: TypeId,
        swapped: &str,
        observed: &str,
        observed_ll: &str,
    ) -> Result<(String, String), BuildError> {
        let result_ty = self.cg.resolve_ty(result_ty);
        let ll = self.cg.llvm_ty(result_ty)?;
        let align = self.cg.layout(result_ty)?.align;
        let Shape::Enum {
            tag,
            payload_offset,
            variants,
            ..
        } = self.cg.types[&result_ty].shape.clone()
        else {
            return Err(BuildError::Internal(
                "a compare-exchange whose result is not a result type".to_string(),
            ));
        };
        let (ok, err) = match variants.as_slice() {
            [ok, err] => (ok.tag, err.tag),
            _ => {
                return Err(BuildError::Internal(
                    "a result type without two variants".to_string(),
                ));
            }
        };
        let slot = self.alloca(&ll, align);
        self.emit(format!("store {ll} zeroinitializer, ptr {slot}"));
        let chosen = self.fresh_tmp();
        self.emit(format!(
            "{chosen} = select i1 {swapped}, {tag} {}, {tag} {}",
            int_literal(&tag, ok),
            int_literal(&tag, err)
        ));
        self.emit(format!("store {tag} {chosen}, ptr {slot}"));
        let payload = self.fresh_tmp();
        self.emit(format!(
            "{payload} = getelementptr i8, ptr {slot}, i64 {payload_offset}"
        ));
        self.emit(format!("store {observed_ll} {observed}, ptr {payload}"));
        let out = self.fresh_tmp();
        self.emit(format!("{out} = load {ll}, ptr {slot}"));
        Ok((out, ll))
    }

    /// The register and memory forms of an atomic value's type.
    fn atomic_rep(&mut self, ty: TypeId) -> Result<AtomicRep, BuildError> {
        let ty = self.cg.resolve_ty(ty);
        let reg = self.cg.llvm_ty(ty)?;
        let layout = self.cg.layout(ty)?;
        let (mem, wrap) = match self.cg.program.decls.types.kind(ty) {
            TyKind::Int(_) | TyKind::RawPtr(..) => (reg.clone(), AtomicWrap::Plain),
            TyKind::Bool => ("i8".to_string(), AtomicWrap::Bool),
            _ => match self.cg.tag_only_enum(ty) {
                Some(tag) => (tag, AtomicWrap::Enum),
                None => {
                    return Err(BuildError::Internal(
                        "an atomic builtin on a type that is not an atomic value".to_string(),
                    ));
                }
            },
        };
        // The cell of an atomic holds a value at its natural alignment, so the
        // alignment the instruction promises is the size.
        if layout.align < layout.size {
            return self
                .cg
                .ni("an atomic value whose alignment is smaller than its size");
        }
        Ok(AtomicRep {
            reg,
            mem,
            align: layout.size,
            wrap,
        })
    }

    fn atomic_to_mem(&mut self, rep: &AtomicRep, v: &str) -> Result<String, BuildError> {
        Ok(match rep.wrap {
            AtomicWrap::Plain => v.to_string(),
            AtomicWrap::Bool => {
                let out = self.fresh_tmp();
                self.emit(format!("{out} = zext i1 {v} to i8"));
                out
            }
            AtomicWrap::Enum => {
                let v = self.as_ssa(v, &rep.reg, rep.align)?;
                let out = self.fresh_tmp();
                self.emit(format!("{out} = extractvalue {} {v}, 0", rep.reg));
                out
            }
        })
    }

    fn atomic_from_mem(&mut self, rep: &AtomicRep, v: &str) -> String {
        match rep.wrap {
            AtomicWrap::Plain => v.to_string(),
            AtomicWrap::Bool => {
                let out = self.fresh_tmp();
                self.emit(format!("{out} = trunc i8 {v} to i1"));
                out
            }
            AtomicWrap::Enum => {
                let out = self.fresh_tmp();
                self.emit(format!(
                    "{out} = insertvalue {} zeroinitializer, {} {v}, 0",
                    rep.reg, rep.mem
                ));
                out
            }
        }
    }

    /// What is known about an `order` operand: a variant spelled out in this block (a
    /// literal such as `.relaxed`, or a copy of one) or a `const` of the enum, else its
    /// tag read at run time. The tag is field 0 of the enum's struct (`discriminant`).
    fn order_arg(&mut self, op: &Operand) -> Result<OrderArg, BuildError> {
        let ty = self.operand_ty(op)?;
        let TyKind::Adt(def, _) = self.cg.program.decls.types.kind(ty).clone() else {
            return Err(BuildError::Internal(
                "an atomic order that is not an enum".to_string(),
            ));
        };
        let sig = self.cg.program.decls.enum_sig(def).ok_or_else(|| {
            BuildError::Internal("an atomic order that is not an enum".to_string())
        })?;
        let tag_ll = int_for_size(u64::from(sig.tag_ty.bits() / 8)).to_string();
        let arms = sig
            .variants
            .iter()
            .map(|v| {
                llvm_ordering(&v.name)
                    .map(|ordering| (v.tag, ordering))
                    .ok_or_else(|| {
                        BuildError::Internal(format!("`{}` is not an atomic order", v.name))
                    })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let known = match op {
            Operand::Place(p, _) if p.proj.is_empty() => match p.base {
                PlaceBase::Local(l) => self.known_variants.get(&l).copied(),
                PlaceBase::Static(_) => None,
            },
            Operand::Const(Const::Named { def, .. }) => match self.cg.item_value(*def) {
                Some(SemValue::Variant { index, .. }) => Some(index),
                _ => None,
            },
            _ => None,
        };
        if let Some((_, ordering)) = known.and_then(|index| arms.get(index)) {
            return Ok(OrderArg::Known(ordering));
        }
        let (v, ll) = self.value(op)?;
        let v = self.as_ssa(&v, &ll, 1)?;
        let tag = self.fresh_tmp();
        self.emit(format!("{tag} = extractvalue {ll} {v}, 0"));
        Ok(OrderArg::Dynamic { tag, tag_ll, arms })
    }

    /// Calls `leaf` with the LLVM orderings, once if every order is known, and
    /// otherwise once in each arm of a `switch` over the tag of the first order that is
    /// not (recursively, so two unknown orders give every pair). `leaf` emits one
    /// instruction and gives its value, which is a `phi` of type `result_ll` where the
    /// arms meet. The last variant is the default arm, so no arm is unreachable.
    fn atomic_dispatch(
        &mut self,
        orders: &[OrderArg],
        chosen: &mut Vec<&'static str>,
        result_ll: Option<&str>,
        leaf: &mut AtomicLeaf<'_, 'a, 'p>,
    ) -> Result<Option<String>, BuildError> {
        let Some(order) = orders.get(chosen.len()) else {
            return leaf(self, chosen);
        };
        let (tag, tag_ll, arms) = match order {
            OrderArg::Known(ordering) => {
                chosen.push(ordering);
                let out = self.atomic_dispatch(orders, chosen, result_ll, leaf);
                chosen.pop();
                return out;
            }
            OrderArg::Dynamic { tag, tag_ll, arms } => (tag, tag_ll, arms),
        };
        let join = self.fresh_label("atomic_join");
        let labels: Vec<String> = arms
            .iter()
            .map(|_| self.fresh_label("atomic_arm"))
            .collect();
        let cases: String = arms
            .iter()
            .zip(&labels)
            .take(arms.len().saturating_sub(1))
            .map(|((value, _), label)| {
                format!("{tag_ll} {}, label %{label} ", int_literal(tag_ll, *value))
            })
            .collect();
        let default = labels.last().cloned().unwrap_or_else(|| join.clone());
        self.emit(format!(
            "switch {tag_ll} {tag}, label %{default} [ {cases}]"
        ));
        let mut incoming = Vec::new();
        for ((_, ordering), label) in arms.iter().zip(labels) {
            self.cur = self.new_block(label);
            chosen.push(ordering);
            let value = self.atomic_dispatch(orders, chosen, result_ll, leaf);
            chosen.pop();
            let value = value?;
            // A nested dispatch ends in its own join block, not the one it began in.
            incoming.push((value, self.blocks[self.cur].label.clone()));
            self.emit(format!("br label %{join}"));
        }
        self.cur = self.new_block(join);
        let Some(ll) = result_ll else {
            return Ok(None);
        };
        let mut edges = Vec::new();
        for (value, from) in incoming {
            edges.push(format!("[ {}, %{from} ]", atomic_value(value)?));
        }
        let out = self.fresh_tmp();
        self.emit(format!("{out} = phi {ll} {}", edges.join(", ")));
        Ok(Some(out))
    }

    fn convert_builtin(
        &mut self,
        name: &str,
        operand: &Operand,
        target: TypeId,
    ) -> Result<(String, String), BuildError> {
        let sty = self.operand_ty(operand)?;
        let target = self.cg.resolve_ty(target);
        let (v, sll) = self.value(operand)?;
        let tll = self.cg.llvm_ty(target)?;
        match name {
            "wrap" => self.wrap_int(&v, &sll, &sty, target),
            "narrow" => self.narrow_convert(&v, &sll, &sty, target),
            "sat" => self.sat_convert(&v, &sll, &sty, target),
            "nearest" => self.nearest_convert(&v, &sll, &sty, &tll),
            "bits" => {
                if sll == tll {
                    Ok((v, tll))
                } else {
                    let out = self.fresh_tmp();
                    self.emit(format!("{out} = bitcast {sll} {v} to {tll}"));
                    Ok((out, tll))
                }
            }
            _ => unreachable!(),
        }
    }

    fn wrap_int(
        &mut self,
        v: &str,
        sll: &str,
        sty: &TypeId,
        target: TypeId,
    ) -> Result<(String, String), BuildError> {
        let tll = self.cg.llvm_ty(target)?;
        if sll == tll {
            return Ok((v.to_string(), tll));
        }
        let signed = matches!(self.cg.program.decls.types.kind(*sty), TyKind::Int(i) if i.signed());
        let out = self.fresh_tmp();
        let (sb, tb) = (int_bits(sll), int_bits(&tll));
        if sb < tb {
            let opcode = if signed { "sext" } else { "zext" };
            self.emit(format!("{out} = {opcode} {sll} {v} to {tll}"));
        } else if sb > tb {
            self.emit(format!("{out} = trunc {sll} {v} to {tll}"));
        } else {
            self.emit(format!("{out} = bitcast {sll} {v} to {tll}"));
        }
        Ok((out, tll))
    }

    fn narrow_convert(
        &mut self,
        v: &str,
        sll: &str,
        sty: &TypeId,
        target: TypeId,
    ) -> Result<(String, String), BuildError> {
        let tll = self.cg.llvm_ty(target)?;
        let float_src = matches!(self.cg.program.decls.types.kind(*sty), TyKind::Float(_));
        let signed_target =
            matches!(self.cg.program.decls.types.kind(target), TyKind::Int(i) if i.signed());
        if float_src {
            // The check above traps an out-of-range value, but `fptosi` is poison
            // outside its range; the saturating form is defined for every input and
            // gives the same result once the check has passed (pipeline §7).
            let opcode = if signed_target { "fptosi" } else { "fptoui" };
            let intrinsic = format!("llvm.{opcode}.sat.{tll}.{sll}");
            self.cg.declare_sat(&intrinsic, sll, &tll)?;
            let out = self.fresh_tmp();
            self.emit(format!("{out} = call {tll} @{intrinsic}({sll} {v})"));
            return Ok((out, tll));
        }
        let signed_src =
            matches!(self.cg.program.decls.types.kind(*sty), TyKind::Int(i) if i.signed());
        let out = self.fresh_tmp();
        let (sb, tb) = (int_bits(sll), int_bits(&tll));
        if sb < tb {
            let opcode = if signed_src { "sext" } else { "zext" };
            self.emit(format!("{out} = {opcode} {sll} {v} to {tll}"));
        } else if sb > tb {
            self.emit(format!("{out} = trunc {sll} {v} to {tll}"));
        } else {
            self.emit(format!("{out} = bitcast {sll} {v} to {tll}"));
        }
        Ok((out, tll))
    }

    fn sat_convert(
        &mut self,
        v: &str,
        sll: &str,
        sty: &TypeId,
        target: TypeId,
    ) -> Result<(String, String), BuildError> {
        let tll = self.cg.llvm_ty(target)?;
        let float_src = matches!(self.cg.program.decls.types.kind(*sty), TyKind::Float(_));
        if float_src {
            return self.sat_float(v, sll, target, &tll);
        }
        let Some(src_int) = int_of(self.cg.program.decls.types.kind(*sty)) else {
            return self.cg.ni("`@sat` on a non-integer (B2)");
        };
        let Some(dst_int) = int_of(self.cg.program.decls.types.kind(target)) else {
            return self.cg.ni("`@sat` to a non-integer (B2)");
        };
        let mut cur = v.to_string();
        let src_ll = sll.to_string();
        if src_int.signed() {
            if !dst_int.signed() && src_int.min() < 0 {
                let zero = self.fresh_tmp();
                self.emit(format!("{zero} = icmp slt {src_ll} {cur}, 0"));
                let z: String = "0".to_string();
                let out = self.fresh_tmp();
                self.emit(format!(
                    "{out} = select i1 {zero}, {src_ll} {z}, {src_ll} {cur}"
                ));
                cur = out;
            }
            if dst_int.signed() {
                if dst_int.min() > src_int.min() {
                    let c = self.fresh_tmp();
                    self.emit(format!(
                        "{c} = icmp slt {src_ll} {cur}, {}",
                        int_literal(&src_ll, dst_int.min())
                    ));
                    let out = self.fresh_tmp();
                    self.emit(format!(
                        "{out} = select i1 {c}, {src_ll} {}, {src_ll} {cur}",
                        int_literal(&src_ll, dst_int.min())
                    ));
                    cur = out;
                }
                if dst_int.max() < src_int.max() {
                    let c = self.fresh_tmp();
                    self.emit(format!(
                        "{c} = icmp sgt {src_ll} {cur}, {}",
                        int_literal(&src_ll, dst_int.max())
                    ));
                    let out = self.fresh_tmp();
                    self.emit(format!(
                        "{out} = select i1 {c}, {src_ll} {}, {src_ll} {cur}",
                        int_literal(&src_ll, dst_int.max())
                    ));
                    cur = out;
                }
            } else {
                if dst_int.max() < src_int.max() {
                    let c = self.fresh_tmp();
                    self.emit(format!(
                        "{c} = icmp sgt {src_ll} {cur}, {}",
                        int_literal(&src_ll, dst_int.max())
                    ));
                    let out = self.fresh_tmp();
                    self.emit(format!(
                        "{out} = select i1 {c}, {src_ll} {}, {src_ll} {cur}",
                        int_literal(&src_ll, dst_int.max())
                    ));
                    cur = out;
                }
            }
        } else {
            if dst_int.min() > src_int.min() {
                let c = self.fresh_tmp();
                self.emit(format!(
                    "{c} = icmp ult {sll} {cur}, {}",
                    int_literal(sll, dst_int.min())
                ));
                let out = self.fresh_tmp();
                self.emit(format!(
                    "{out} = select i1 {c}, {sll} {}, {sll} {cur}",
                    int_literal(sll, dst_int.min())
                ));
                cur = out;
            }
            if dst_int.max() < src_int.max() {
                let c = self.fresh_tmp();
                self.emit(format!(
                    "{c} = icmp ugt {sll} {cur}, {}",
                    int_literal(sll, dst_int.max())
                ));
                let out = self.fresh_tmp();
                self.emit(format!(
                    "{out} = select i1 {c}, {sll} {}, {sll} {cur}",
                    int_literal(sll, dst_int.max())
                ));
                cur = out;
            }
        }
        self.wrap_int(&cur, &src_ll, sty, target)
    }

    fn sat_float(
        &mut self,
        v: &str,
        sll: &str,
        target: TypeId,
        tll: &str,
    ) -> Result<(String, String), BuildError> {
        let Some(dst) = int_of(self.cg.program.decls.types.kind(target)) else {
            return self.cg.ni("`@sat` to a non-integer (B2)");
        };
        // The saturating conversion intrinsic clamps to the target's range and maps
        // NaN to zero, which is exactly `@sat` (01-core §5.4).
        let opcode = if dst.signed() { "fptosi" } else { "fptoui" };
        let intrinsic = format!("llvm.{opcode}.sat.{tll}.{sll}");
        self.cg.declare_sat(&intrinsic, sll, tll)?;
        let out = self.fresh_tmp();
        self.emit(format!("{out} = call {tll} @{intrinsic}({sll} {v})"));
        Ok((out, tll.to_string()))
    }

    fn nearest_convert(
        &mut self,
        v: &str,
        sll: &str,
        sty: &TypeId,
        tll: &str,
    ) -> Result<(String, String), BuildError> {
        let out = self.fresh_tmp();
        if matches!(self.cg.program.decls.types.kind(*sty), TyKind::Float(_)) {
            let (sb, tb) = (int_bits(sll), int_bits(tll));
            let _ = (sb, tb);
            if sll == "float" && tll == "double" {
                self.emit(format!("{out} = fpext float {v} to double"));
            } else if sll == "double" && tll == "float" {
                self.emit(format!("{out} = fptrunc double {v} to float"));
            } else {
                return Ok((v.to_string(), tll.to_string()));
            }
        } else {
            let signed =
                matches!(self.cg.program.decls.types.kind(*sty), TyKind::Int(i) if i.signed());
            let opcode = if signed { "sitofp" } else { "uitofp" };
            self.emit(format!("{out} = {opcode} {sll} {v} to {tll}"));
        }
        Ok((out, tll.to_string()))
    }

    // --------------------------------------------------------------- checks

    fn check(&mut self, check: &Check, span: Span) -> Result<(), BuildError> {
        let at = if check.span.is_empty() {
            span
        } else {
            check.span
        };
        match check.kind {
            CheckKind::Bounds(BoundsKind::Index) => {
                let place = check.place.as_ref().ok_or_else(|| {
                    BuildError::Internal("a bounds check without a container".to_string())
                })?;
                let len = self.container_len(place)?;
                let index = check.lo.as_ref().ok_or_else(|| {
                    BuildError::Internal("a bounds check without an index".to_string())
                })?;
                let it = self.operand_ty(index)?;
                let iv = self.index_i64(index)?;
                let signed =
                    matches!(self.cg.program.decls.types.kind(it), TyKind::Int(i) if i.signed());
                let mut fail = None;
                if signed {
                    let neg = self.fresh_tmp();
                    self.emit(format!("{neg} = icmp slt i64 {iv}, 0"));
                    fail = Some(neg);
                }
                let over = self.fresh_tmp();
                self.emit(format!("{over} = icmp uge i64 {iv}, {len}"));
                let cond = match fail {
                    Some(neg) => {
                        let c = self.fresh_tmp();
                        self.emit(format!("{c} = or i1 {neg}, {over}"));
                        c
                    }
                    None => over,
                };
                let text = b"index \x01 out of bounds for length \x02";
                self.guard(&cond, Trap::T01, text, at, &[iv, len])?;
            }
            CheckKind::Bounds(BoundsKind::Slice) => {
                let place = check.place.as_ref().ok_or_else(|| {
                    BuildError::Internal("a slice check without a container".to_string())
                })?;
                let len = self.container_len(place)?;
                let lo = check.lo.as_ref().ok_or_else(|| {
                    BuildError::Internal("a slice check without a lower bound".to_string())
                })?;
                let hi = check.hi.as_ref().ok_or_else(|| {
                    BuildError::Internal("a slice check without an upper bound".to_string())
                })?;
                let lo = self.index_i64(lo)?;
                let hi = self.index_i64(hi)?;
                let c1 = self.fresh_tmp();
                self.emit(format!("{c1} = icmp slt i64 {lo}, 0"));
                let c2 = self.fresh_tmp();
                self.emit(format!("{c2} = icmp slt i64 {hi}, {lo}"));
                let c3 = self.fresh_tmp();
                self.emit(format!("{c3} = icmp ugt i64 {hi}, {len}"));
                let c12 = self.fresh_tmp();
                self.emit(format!("{c12} = or i1 {c1}, {c2}"));
                let cond = self.fresh_tmp();
                self.emit(format!("{cond} = or i1 {c12}, {c3}"));
                let text = b"slice \x01..\x02 out of bounds for length \x03";
                self.guard(&cond, Trap::T02, text, at, &[lo, hi, len])?;
            }
            CheckKind::Bounds(BoundsKind::Vector { .. }) => {
                return self.cg.ni("vector accesses (B4)");
            }
            CheckKind::Overflow => {
                let ty = self.cg.resolve_ty(check.ty.ok_or_else(|| {
                    BuildError::Internal("an overflow check without a type".to_string())
                })?);
                let op = check.op.as_deref().unwrap_or("?");
                let text = format!("integer overflow in {op}");
                let (cond, _a) = self.overflow_condition(check, ty)?;
                self.guard(&cond, Trap::T03, text.as_bytes(), at, &[])?;
            }
            CheckKind::Division => {
                let ty = self.cg.resolve_ty(check.ty.ok_or_else(|| {
                    BuildError::Internal("a division check without a type".to_string())
                })?);
                let signed =
                    matches!(self.cg.program.decls.types.kind(ty), TyKind::Int(i) if i.signed());
                let ll = self.cg.llvm_ty(ty)?;
                let lhs = check.operands.first().ok_or_else(|| {
                    BuildError::Internal("a division check without a dividend".to_string())
                })?;
                let rhs = check.operands.get(1).ok_or_else(|| {
                    BuildError::Internal("a division check without a divisor".to_string())
                })?;
                let (lv, _) = self.value(lhs)?;
                let (rv, _) = self.value(rhs)?;
                let closed = self.fresh_tmp();
                self.emit(format!("{closed} = icmp eq {ll} {rv}, 0"));
                self.guard(&closed, Trap::T04, b"division by zero", at, &[])?;
                if signed {
                    let one = self.fresh_tmp();
                    let neg_one = int_literal(&ll, -1);
                    self.emit(format!("{one} = icmp eq {ll} {rv}, {neg_one}"));
                    let min = int_literal(&ll, int_min_i128(&ll));
                    let at_min = self.fresh_tmp();
                    self.emit(format!("{at_min} = icmp eq {ll} {lv}, {min}"));
                    let both = self.fresh_tmp();
                    self.emit(format!("{both} = and i1 {one}, {at_min}"));
                    self.guard(&both, Trap::T05, b"integer overflow in division", at, &[])?;
                }
            }
            CheckKind::Shift => {
                let ty = self.cg.resolve_ty(check.ty.ok_or_else(|| {
                    BuildError::Internal("a shift check without a type".to_string())
                })?);
                let bits = match self.cg.program.decls.types.kind(ty) {
                    TyKind::Int(i) => i.bits(),
                    _ => {
                        return self.cg.ni("a shift check on a non-integer (B2)");
                    }
                };
                let rhs = check.operands.get(1).ok_or_else(|| {
                    BuildError::Internal("a shift check without a count".to_string())
                })?;
                let rt = self.operand_ty(rhs)?;
                let n = self.index_i64(rhs)?;
                let signed =
                    matches!(self.cg.program.decls.types.kind(rt), TyKind::Int(i) if i.signed());
                let mut fail = None;
                if signed {
                    let neg = self.fresh_tmp();
                    self.emit(format!("{neg} = icmp slt i64 {n}, 0"));
                    fail = Some(neg);
                }
                let big = self.fresh_tmp();
                self.emit(format!("{big} = icmp uge i64 {n}, {bits}"));
                let cond = match fail {
                    Some(neg) => {
                        let c = self.fresh_tmp();
                        self.emit(format!("{c} = or i1 {neg}, {big}"));
                        c
                    }
                    None => big,
                };
                let mut text = b"shift count ".to_vec();
                text.push(1);
                text.extend_from_slice(format!(" out of range for {bits}-bit value").as_bytes());
                self.guard(&cond, Trap::T06, &text, at, &[n, bits.to_string()])?;
            }
            CheckKind::Narrow => {
                let operand = check.operands.first().ok_or_else(|| {
                    BuildError::Internal("a narrow check without a value".to_string())
                })?;
                let target = self.cg.resolve_ty(check.ty.ok_or_else(|| {
                    BuildError::Internal("a narrow check without a target".to_string())
                })?);
                let (cond, _) = self.narrow_condition(operand, target)?;
                let name = self.cg.type_display(target);
                let text = format!("value does not fit in {name}");
                self.guard(&cond, Trap::T07, text.as_bytes(), at, &[])?;
            }
            CheckKind::Gather | CheckKind::ScatterDistinct | CheckKind::DispatchCover => {
                return self.cg.ni("this check (B4)");
            }
        }
        Ok(())
    }

    fn container_len(&mut self, place: &Place) -> Result<String, BuildError> {
        let ty = self.place_type(place)?;
        match self.cg.program.decls.types.kind(ty).clone() {
            TyKind::Array(_, n) => {
                let len = self
                    .cg
                    .program
                    .decls
                    .types
                    .const_arg_value(n)
                    .and_then(|v| match v {
                        ConstValue::Int { value, .. } if value >= 0 => Some(value),
                        _ => None,
                    })
                    .ok_or_else(|| {
                        BuildError::Internal("an array length with no value".to_string())
                    })?;
                Ok(len.to_string())
            }
            TyKind::Slice(..) => {
                let addr = self.place_addr(place)?;
                let p = self.fresh_tmp();
                self.emit(format!("{p} = getelementptr i8, ptr {addr}, i64 8"));
                let out = self.fresh_tmp();
                self.emit(format!("{out} = load i64, ptr {p}"));
                Ok(out)
            }
            _ => Err(BuildError::Internal(
                "a check against something that is not an array or slice".to_string(),
            )),
        }
    }

    fn overflow_condition(
        &mut self,
        check: &Check,
        ty: TypeId,
    ) -> Result<(String, String), BuildError> {
        let ll = self.cg.llvm_ty(ty)?;
        let signed = matches!(self.cg.program.decls.types.kind(ty), TyKind::Int(i) if i.signed());
        let prefix = if signed { "s" } else { "u" };
        if check.operands.len() == 1 {
            let (x, _) = self.value(&check.operands[0])?;
            let zero = self.fresh_tmp();
            self.emit(format!(
                "{zero} = icmp eq {ll} {x}, {}",
                int_literal(&ll, int_min_i128(&ll))
            ));
            return Ok((zero, x));
        }
        let lhs = check.operands.first().ok_or_else(|| {
            BuildError::Internal("an overflow check without operands".to_string())
        })?;
        let op = check.op.as_deref().ok_or_else(|| {
            BuildError::Internal("an overflow check without an operator".to_string())
        })?;
        let kind = match op {
            "+" => "add",
            "-" => "sub",
            "*" => "mul",
            other => {
                return Err(BuildError::Internal(format!(
                    "an overflow check with the operator `{other}`"
                )));
            }
        };
        let (a, _) = self.value(lhs)?;
        let rhs = check.operands.get(1).ok_or_else(|| {
            BuildError::Internal("an overflow check without a right operand".to_string())
        })?;
        let (b, _) = self.value(rhs)?;
        let intrinsic = format!("llvm.{}{}.with.overflow.{}", prefix, kind, ll);
        self.cg.declare_overflow(&intrinsic, &ll)?;
        let pair = self.fresh_tmp();
        self.emit(format!(
            "{pair} = call {{ {ll}, i1 }} @{intrinsic}({ll} {a}, {ll} {b})"
        ));
        let over = self.fresh_tmp();
        self.emit(format!("{over} = extractvalue {{ {ll}, i1 }} {pair}, 1"));
        Ok((over, a))
    }

    fn narrow_condition(
        &mut self,
        operand: &Operand,
        target: TypeId,
    ) -> Result<(String, String), BuildError> {
        let sty = self.operand_ty(operand)?;
        let target = self.cg.resolve_ty(target);
        let (v, sll) = self.value(operand)?;
        let Some(src) = int_of(self.cg.program.decls.types.kind(sty)) else {
            let Some(dst) = int_of(self.cg.program.decls.types.kind(target)) else {
                return self.cg.ni("`@narrow` to a non-integer (B2)");
            };
            let lo = dst.min() as f64;
            let hi = dst.max() as f64;
            // Compare in double: every f32 is exact there, and for an f64 source the
            // only values that can round to `min` or `max + 1` are the bounds
            // themselves, which the cutoff function chooses.
            let (cmp_ll, cmp_v) = if sll == "float" {
                let d = self.fresh_tmp();
                self.emit(format!("{d} = fpext float {v} to double"));
                ("double".to_string(), d)
            } else {
                (sll.clone(), v.clone())
            };
            let (low_cmp, low_text) = trunc_low_bound(lo, "double");
            let hi1 = format_float_f64(hi + 1.0);
            let nan = self.fresh_tmp();
            self.emit(format!("{nan} = fcmp uno {cmp_ll} {cmp_v}, 0.0"));
            let low = self.fresh_tmp();
            self.emit(format!(
                "{low} = fcmp {low_cmp} {cmp_ll} {cmp_v}, {low_text}"
            ));
            let high = self.fresh_tmp();
            self.emit(format!("{high} = fcmp oge {cmp_ll} {cmp_v}, {hi1}"));
            let c1 = self.fresh_tmp();
            self.emit(format!("{c1} = or i1 {nan}, {low}"));
            let cond = self.fresh_tmp();
            self.emit(format!("{cond} = or i1 {c1}, {high}"));
            return Ok((cond, sll));
        };
        let Some(dst) = int_of(self.cg.program.decls.types.kind(target)) else {
            return self.cg.ni("`@narrow` to a non-integer (B2)");
        };
        let mut cond: Option<String> = None;
        let mut add = |this: &mut Self, c: String| {
            if let Some(prev) = cond.clone() {
                let out = this.fresh_tmp();
                this.emit(format!("{out} = or i1 {prev}, {c}"));
                cond = Some(out);
            } else {
                cond = Some(c);
            }
        };
        if src.signed() && dst.min() > src.min() {
            let c = self.fresh_tmp();
            self.emit(format!(
                "{c} = icmp slt {sll} {v}, {}",
                int_literal(&sll, dst.min())
            ));
            add(self, c);
        } else if !src.signed() && dst.signed() && dst.min() > 0 {
            let c = self.fresh_tmp();
            self.emit(format!(
                "{c} = icmp ult {sll} {v}, {}",
                int_literal(&sll, dst.min())
            ));
            add(self, c);
        }
        if dst.max() < src.max() {
            let c = self.fresh_tmp();
            let pred = if src.signed() { "sgt" } else { "ugt" };
            self.emit(format!(
                "{c} = icmp {pred} {sll} {v}, {}",
                int_literal(&sll, dst.max())
            ));
            add(self, c);
        }
        match cond {
            Some(c) => Ok((c, sll)),
            None => {
                let never = self.fresh_tmp();
                self.emit(format!("{never} = icmp eq i1 false, true"));
                Ok((never, sll))
            }
        }
    }

    fn guard(
        &mut self,
        cond: &str,
        code: Trap,
        text: &[u8],
        at: Span,
        operands: &[String],
    ) -> Result<(), BuildError> {
        let site = self.cg.add_site(self.body.cfg.file, at, code, text)?;
        let trap_label = self.fresh_label("trap");
        let cont = self.fresh_label("cont");
        self.emit(format!("br i1 {cond}, label %{trap_label}, label %{cont}"));
        let mut lines = Vec::new();
        let a = operands.first().cloned().unwrap_or_else(|| "0".to_string());
        let b = operands.get(1).cloned().unwrap_or_else(|| "0".to_string());
        let c = operands.get(2).cloned().unwrap_or_else(|| "0".to_string());
        let trap_symbol = global_ref(&self.cg.trap_symbol()?);
        lines.push(format!(
            "call void {trap_symbol}(ptr @{site}, i64 {a}, i64 {b}, i64 {c})"
        ));
        lines.push("unreachable".to_string());
        self.cold.push(OutBlock {
            label: trap_label,
            lines,
        });
        let next = self.new_block(cont);
        self.cur = next;
        Ok(())
    }

    fn emit_trap(
        &mut self,
        code: Trap,
        operands: &[Operand],
        source: Option<&str>,
        span: Span,
    ) -> Result<(), BuildError> {
        let mut text = Vec::new();
        let mut a = "0".to_string();
        let mut b = "0".to_string();
        let mut c = "0".to_string();
        match code {
            Trap::T08 => {
                text.extend_from_slice(b"assertion failed: ");
                text.extend_from_slice(source.unwrap_or("").as_bytes());
                if let Some(msg) = operands.first() {
                    let (p, l) = self.string_operand(msg)?;
                    a = p;
                    b = l;
                    c = "1".to_string();
                }
            }
            Trap::T09 => {
                if let Some(msg) = operands.first() {
                    let (p, l) = self.string_operand(msg)?;
                    a = p;
                    b = l;
                }
            }
            Trap::T10 => text.extend_from_slice(b"entered unreachable code"),
            _ => return self.cg.ni("this trap (B2)"),
        }
        let site = self.cg.add_site(self.body.cfg.file, span, code, &text)?;
        let trap_symbol = global_ref(&self.cg.trap_symbol()?);
        self.emit(format!(
            "call void {trap_symbol}(ptr @{site}, i64 {a}, i64 {b}, i64 {c})"
        ));
        Ok(())
    }

    fn string_operand(&mut self, op: &Operand) -> Result<(String, String), BuildError> {
        let (v, ll) = self.value(op)?;
        if ll == "ptr" {
            let p = self.fresh_tmp();
            self.emit(format!("{p} = ptrtoint ptr {v} to i64"));
            return Ok((p, "0".to_string()));
        }
        let v = self.as_ssa(&v, &ll, 8)?;
        let p = self.fresh_tmp();
        let len = self.fresh_tmp();
        self.emit(format!("{p} = extractvalue {ll} {v}, 0"));
        self.emit(format!("{len} = extractvalue {ll} {v}, 1"));
        let p64 = self.fresh_tmp();
        self.emit(format!("{p64} = ptrtoint ptr {p} to i64"));
        Ok((p64, len))
    }
}

fn int_of(kind: &TyKind) -> Option<IntTy> {
    match kind {
        TyKind::Int(i) => Some(*i),
        _ => None,
    }
}

/// The condition that catches a float below the `@narrow` range: truncation toward
/// zero of a value in `(min - 1, min)` is `min`, so the boundary is `min - 1` when it
/// is representable in the source float, and `min` otherwise.
fn trunc_low_bound(lo: f64, ll: &str) -> (&'static str, String) {
    let candidate = lo - 1.0;
    let repr = if ll == "float" {
        (candidate as f32) as f64 == candidate
    } else {
        true
    } && candidate != lo;
    if repr {
        (
            "ole",
            if ll == "float" {
                format_float_f32(candidate)
            } else {
                format_float_f64(candidate)
            },
        )
    } else {
        (
            "olt",
            if ll == "float" {
                format_float_f32(lo)
            } else {
                format_float_f64(lo)
            },
        )
    }
}

fn int_min_i128(ll: &str) -> i128 {
    let bits = int_bits(ll);
    if bits >= 128 {
        i128::MIN
    } else {
        -(1i128 << (bits - 1))
    }
}

impl<'a, 'p> FnGen<'a, 'p> {
    // ----------------------------------------------------------------- calls

    /// Store a call's result into `dest`. A plain result is one aggregate store; a
    /// tag-and-payload result stores the tag and copies the payload bytes from the
    /// register spill slot or the caller's result slot.
    fn store_call_result(
        &mut self,
        abi: &RetAbi,
        ret_ll: &str,
        call: &str,
        result_slot: Option<&str>,
        dest: Option<&Place>,
    ) -> Result<(), BuildError> {
        if ret_ll == "void" {
            return Ok(());
        }
        let Some(dest) = dest else {
            return Ok(());
        };
        let dty = self.place_type(dest)?;
        if dty == self.cg.program.decls.types.never {
            return Ok(());
        }
        let addr = self.place_addr(dest)?;
        if let Some(r) = abi.result() {
            let tag = if r.ptr {
                call.to_string()
            } else {
                let t = self.fresh_tmp();
                self.emit(format!(
                    "{t} = extractvalue {{ {}, i64 }} {call}, 0",
                    r.tag_ll
                ));
                t
            };
            self.emit(format!("store {} {tag}, ptr {addr}", r.tag_ll));
            if r.payload_area > 0 {
                if r.ptr {
                    let slot = result_slot.ok_or_else(|| {
                        BuildError::Internal("a result pointer call without a slot".to_string())
                    })?;
                    self.copy_region(slot, 0, &addr, r.payload_offset, r.payload_area);
                } else {
                    let payload = self.fresh_tmp();
                    self.emit(format!(
                        "{payload} = extractvalue {{ {}, i64 }} {call}, 1",
                        r.tag_ll
                    ));
                    let slot = self.zeroed_bytes(8, 8);
                    self.emit(format!("store i64 {payload}, ptr {slot}"));
                    self.copy_region(&slot, 0, &addr, r.payload_offset, r.payload_area);
                }
            }
        } else {
            self.emit(format!("store {ret_ll} {call}, ptr {addr}"));
        }
        Ok(())
    }

    /// Lower one value to the ABI pieces of a parameter of type `pty`.
    fn abi_arg(
        &mut self,
        v: String,
        vll: String,
        pty: TypeId,
        abi_types: &mut Vec<String>,
        values: &mut Vec<String>,
    ) -> Result<(), BuildError> {
        let pty = self.cg.resolve_ty(pty);
        match self.cg.program.decls.types.kind(pty).clone() {
            TyKind::Slice(..) => {
                let v = self.as_ssa(&v, &vll, 8)?;
                let a = self.fresh_tmp();
                let b = self.fresh_tmp();
                self.emit(format!("{a} = extractvalue {vll} {v}, 0"));
                self.emit(format!("{b} = extractvalue {vll} {v}, 1"));
                abi_types.push("ptr".to_string());
                abi_types.push("i64".to_string());
                values.push(a);
                values.push(b);
            }
            TyKind::Dyn(..) | TyKind::DynFn(..) => {
                let v = self.as_ssa(&v, &vll, 8)?;
                let a = self.fresh_tmp();
                let b = self.fresh_tmp();
                self.emit(format!("{a} = extractvalue {vll} {v}, 0"));
                self.emit(format!("{b} = extractvalue {vll} {v}, 1"));
                abi_types.push("ptr".to_string());
                abi_types.push("ptr".to_string());
                values.push(a);
                values.push(b);
            }
            _ => {
                if let Some(tag) = self.cg.tag_only_enum(pty) {
                    let v = self.as_ssa(&v, &vll, 8)?;
                    let t = self.fresh_tmp();
                    self.emit(format!("{t} = extractvalue {vll} {v}, 0"));
                    abi_types.push(tag);
                    values.push(t);
                } else if let Some((size, align)) = self.cg.aggregate_size(pty)? {
                    let v = self.as_ssa(&v, &vll, align)?;
                    if size > 16 {
                        let tmp = self.alloca(&vll, align);
                        self.emit(format!("store {vll} {v}, ptr {tmp}"));
                        abi_types.push("ptr".to_string());
                        values.push(tmp);
                    } else if size > 0 {
                        let tmp = self.split_slot(&vll, &v, align);
                        let a = self.fresh_tmp();
                        self.emit(format!("{a} = load i64, ptr {tmp}"));
                        abi_types.push("i64".to_string());
                        values.push(a);
                        if size > 8 {
                            let p = self.fresh_tmp();
                            self.emit(format!("{p} = getelementptr i8, ptr {tmp}, i64 8"));
                            let b = self.fresh_tmp();
                            self.emit(format!("{b} = load i64, ptr {p}"));
                            abi_types.push("i64".to_string());
                            values.push(b);
                        }
                    }
                } else {
                    abi_types.push(vll);
                    values.push(v);
                }
            }
        }
        Ok(())
    }

    fn lower_call_args(
        &mut self,
        params: &[TypeId],
        args: &[Operand],
    ) -> Result<(Vec<String>, Vec<String>), BuildError> {
        let mut abi_types = Vec::new();
        let mut values = Vec::new();
        for (i, pty) in params.iter().enumerate() {
            let op = args
                .get(i)
                .ok_or_else(|| BuildError::Internal("a call with too few arguments".to_string()))?;
            let (v, vll) = self.value(op)?;
            self.abi_arg(v, vll, *pty, &mut abi_types, &mut values)?;
        }
        Ok((abi_types, values))
    }

    fn emit_call(
        &mut self,
        callee: &str,
        ret_abi: &RetAbi,
        abi_types: &mut Vec<String>,
        values: &mut Vec<String>,
        dest: Option<&Place>,
    ) -> Result<(), BuildError> {
        let ret_ll = ret_abi.ret_ll();
        let result_abi = ret_abi.result().cloned();
        let result_slot = match &result_abi {
            Some(r) if r.ptr => {
                let slot = self.zeroed_bytes(r.payload_area, r.payload_align.max(1));
                abi_types.push("ptr".to_string());
                values.push(slot.clone());
                Some(slot)
            }
            _ => None,
        };
        let tys: Vec<String> = values
            .iter()
            .enumerate()
            .map(|(i, v)| format!("{} {}", abi_types[i], v))
            .collect();
        let call = if ret_ll == "void" {
            self.emit(format!("call void {callee}({})", tys.join(", ")));
            String::new()
        } else {
            let out = self.fresh_tmp();
            self.emit(format!(
                "{out} = call {ret_ll} {callee}({})",
                tys.join(", ")
            ));
            out
        };
        self.store_call_result(ret_abi, &ret_ll, &call, result_slot.as_deref(), dest)?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn call_direct_symbol(
        &mut self,
        symbol: &str,
        sig: &FnSig,
        subst: &Subst,
        args: &[Operand],
        dest: Option<&Place>,
        next: Block,
        external: bool,
    ) -> Result<(), BuildError> {
        let params: Vec<TypeId> = sig
            .params
            .iter()
            .map(|p| {
                if external {
                    p.ty
                } else {
                    self.cg.program.decls.instantiate(p.ty, subst)
                }
            })
            .collect();
        if sig.variadic && args.len() > params.len() {
            return self.cg.ni("variadic extern calls (B2)");
        }
        let ret = if external {
            sig.ret
        } else {
            self.cg.program.decls.instantiate(sig.ret, subst)
        };
        let error = sig.error.map(|e| {
            if external {
                e
            } else {
                self.cg.program.decls.instantiate(e, subst)
            }
        });
        let ret_abi = if external {
            RetAbi::Plain {
                ll: if ret == self.cg.program.decls.types.void
                    || ret == self.cg.program.decls.types.never
                {
                    "void".to_string()
                } else {
                    self.cg.llvm_ty(ret)?
                },
            }
        } else {
            self.cg.ret_abi_for(ret, error)?
        };
        let (mut abi_types, mut values) = if external {
            let mut abi_types = Vec::new();
            let mut values = Vec::new();
            for (i, pty) in params.iter().enumerate() {
                let ll = self.cg.llvm_ty(*pty)?;
                abi_types.push(ll.clone());
                let op = args.get(i).ok_or_else(|| {
                    BuildError::Internal("an extern call with too few arguments".to_string())
                })?;
                let (v, vll) = self.value(op)?;
                if vll != ll {
                    return Err(BuildError::Internal(format!(
                        "an extern argument of type {vll}, expected {ll}"
                    )));
                }
                values.push(v);
            }
            (abi_types, values)
        } else {
            self.lower_call_args(&params, args)?
        };
        self.emit_call(
            &global_ref(symbol),
            &ret_abi,
            &mut abi_types,
            &mut values,
            dest,
        )?;
        self.emit(format!("br label %{}", self.label_of(next)));
        Ok(())
    }

    /// A call through a function pointer value already in SSA form.
    #[allow(clippy::too_many_arguments)]
    fn call_indirect(
        &mut self,
        fp: &str,
        params: &[TypeId],
        args: &[Operand],
        ret: TypeId,
        error: Option<TypeId>,
        ext: bool,
        prepend: Option<String>,
        dest: Option<&Place>,
        next: Block,
    ) -> Result<(), BuildError> {
        let ret_abi = if ext {
            RetAbi::Plain {
                ll: if ret == self.cg.program.decls.types.void
                    || ret == self.cg.program.decls.types.never
                {
                    "void".to_string()
                } else {
                    self.cg.llvm_ty(ret)?
                },
            }
        } else {
            self.cg.ret_abi_for(ret, error)?
        };
        let (mut abi_types, mut values) = if let Some(data) = prepend {
            let (mut abi_types, mut values) = self.lower_call_args(params, args)?;
            abi_types.insert(0, "ptr".to_string());
            values.insert(0, data);
            (abi_types, values)
        } else {
            self.lower_call_args(params, args)?
        };
        self.emit_call(fp, &ret_abi, &mut abi_types, &mut values, dest)?;
        self.emit(format!("br label %{}", self.label_of(next)));
        Ok(())
    }

    fn closure_call(
        &mut self,
        callee: &Operand,
        args: &[Operand],
        dest: Option<&Place>,
        next: Block,
    ) -> Result<(), BuildError> {
        let cty = self.operand_ty(callee)?;
        let (v, _) = self.value(callee)?;
        let mut ty = cty;
        let mut record = v;
        loop {
            if let TyKind::Closure(..) = self.cg.program.decls.types.kind(ty) {
                break;
            }
            let TyKind::Ref(_, inner) = self.cg.program.decls.types.kind(ty).clone() else {
                return self
                    .cg
                    .ni("a call of a value that is not a closure or `fn` (B2)");
            };
            if let TyKind::Closure(..) = self.cg.program.decls.types.kind(inner) {
                ty = inner;
                break;
            }
            let out = self.fresh_tmp();
            self.emit(format!("{out} = load ptr, ptr {record}"));
            record = out;
            ty = inner;
        }
        let key = closure_instance_key(self.cg.program.sema, self.cg.program.decls, ty)
            .ok_or_else(|| BuildError::Internal("a closure with no instance".to_string()))?;
        let symbol = self.cg.symbols.get(&key).cloned().ok_or_else(|| {
            BuildError::Internal("the closure instance was not discovered".to_string())
        })?;
        let sig = self
            .cg
            .bodies
            .get(&symbol)
            .map(|b| b.sig.clone())
            .ok_or_else(|| BuildError::Internal("the closure body was not emitted".to_string()))?;
        let params: Vec<TypeId> = sig
            .params
            .iter()
            .map(|p| self.cg.program.decls.instantiate(p.ty, &key.subst))
            .collect();
        let ret = self.cg.program.decls.instantiate(sig.ret, &key.subst);
        let error = sig
            .error
            .map(|e| self.cg.program.decls.instantiate(e, &key.subst));
        let ret_abi = self.cg.ret_abi_for(ret, error)?;
        let mut abi_types = Vec::new();
        let mut values = Vec::new();
        let first = params
            .first()
            .copied()
            .ok_or_else(|| BuildError::Internal("a closure with no record".to_string()))?;
        let record_ll = self.cg.llvm_ty(first)?;
        self.abi_arg(record, record_ll, first, &mut abi_types, &mut values)?;
        for (i, pty) in params.iter().enumerate().skip(1) {
            let op = args.get(i - 1).ok_or_else(|| {
                BuildError::Internal("a closure call with too few arguments".to_string())
            })?;
            let (v, vll) = self.value(op)?;
            self.abi_arg(v, vll, *pty, &mut abi_types, &mut values)?;
        }
        self.emit_call(
            &global_ref(&symbol),
            &ret_abi,
            &mut abi_types,
            &mut values,
            dest,
        )?;
        self.emit(format!("br label %{}", self.label_of(next)));
        Ok(())
    }

    fn fn_ptr_call(
        &mut self,
        callee: &Operand,
        args: &[Operand],
        dest: Option<&Place>,
        next: Block,
    ) -> Result<(), BuildError> {
        let mut ty = self.operand_ty(callee)?;
        let (mut v, _) = self.value(callee)?;
        loop {
            match self.cg.program.decls.types.kind(ty).clone() {
                TyKind::FnPtr(sig) => {
                    return self.call_indirect(
                        &v,
                        &sig.params,
                        args,
                        sig.ret,
                        sig.error,
                        sig.ext,
                        None,
                        dest,
                        next,
                    );
                }
                TyKind::Ref(_, inner) => {
                    let out = self.fresh_tmp();
                    self.emit(format!("{out} = load ptr, ptr {v}"));
                    v = out;
                    ty = inner;
                }
                _ => return self.cg.ni("a call of a value that is not `fn` (B2)"),
            }
        }
    }

    fn dyn_fn_call(
        &mut self,
        callee: &Operand,
        args: &[Operand],
        dest: Option<&Place>,
        next: Block,
    ) -> Result<(), BuildError> {
        let mut ty = self.operand_ty(callee)?;
        let (mut v, mut vll) = self.value(callee)?;
        let sig = loop {
            match self.cg.program.decls.types.kind(ty).clone() {
                TyKind::DynFn(_, _, sig) => break sig,
                TyKind::Ref(_, inner) => {
                    if matches!(self.cg.program.decls.types.kind(inner), TyKind::DynFn(..)) {
                        let out = self.fresh_tmp();
                        self.emit(format!("{out} = load {{ ptr, ptr }}, ptr {v}"));
                        v = out;
                        vll = "{ ptr, ptr }".to_string();
                    } else {
                        let out = self.fresh_tmp();
                        self.emit(format!("{out} = load ptr, ptr {v}"));
                        v = out;
                        vll = "ptr".to_string();
                    }
                    ty = inner;
                }
                _ => return self.cg.ni("a call of a value that is not `&dyn Fn` (B2)"),
            }
        };
        let v = self.as_ssa(&v, &vll, 8)?;
        let data = self.fresh_tmp();
        let table = self.fresh_tmp();
        self.emit(format!("{data} = extractvalue {vll} {v}, 0"));
        self.emit(format!("{table} = extractvalue {vll} {v}, 1"));
        let slot = self.fresh_tmp();
        self.emit(format!("{slot} = getelementptr ptr, ptr {table}, i64 0"));
        let fp = self.fresh_tmp();
        self.emit(format!("{fp} = load ptr, ptr {slot}"));
        self.call_indirect(
            &fp,
            &sig.params,
            args,
            sig.ret,
            sig.error,
            false,
            Some(data),
            dest,
            next,
        )
    }

    fn dyn_interface_call(
        &mut self,
        fn_ref: &ingot_sema::typed::FnRef,
        args: &[Operand],
        dest: Option<&Place>,
        next: Block,
    ) -> Result<(), BuildError> {
        let ingot_sema::typed::FnSource::Interface {
            interface,
            interface_args,
            self_ty,
            via: ingot_sema::typed::InterfaceVia::Dyn,
            ..
        } = &fn_ref.source
        else {
            return self.cg.ni("a dynamic call outside an interface");
        };
        let isig = self
            .cg
            .program
            .decls
            .interface_sig(*interface)
            .cloned()
            .ok_or_else(|| {
                BuildError::Internal("a dynamic call of an unknown interface".to_string())
            })?;
        let dyn_interface = match self.cg.program.decls.types.kind(*self_ty) {
            TyKind::Dyn(_, d, _) => *d,
            _ => *interface,
        };
        let index = interface_method_order(&mut *self.cg.program.decls, dyn_interface)
            .iter()
            .position(|m| *m == fn_ref.def)
            .ok_or_else(|| {
                BuildError::Internal("a dynamic call of an unknown method".to_string())
            })?;
        let sig = self
            .cg
            .program
            .decls
            .fn_sig(fn_ref.def)
            .cloned()
            .ok_or_else(|| {
                BuildError::Internal("a dynamic method with no signature".to_string())
            })?;
        let iface_args: Vec<ingot_sema::ty::GenericArg> = interface_args
            .iter()
            .map(|a| self.cg.program.decls.types.subst_arg(a, &self.cg.subst))
            .collect();
        let self_ty = self.cg.program.decls.instantiate(*self_ty, &self.cg.subst);
        let mut subst = self.cg.program.decls.subst_for(*interface, &iface_args);
        subst.insert(isig.self_def, ingot_sema::ty::GenericArg::Ty(self_ty));
        let params: Vec<TypeId> = sig
            .params
            .iter()
            .skip(1)
            .map(|p| self.cg.program.decls.instantiate(p.ty, &subst))
            .collect();
        let ret = self.cg.program.decls.instantiate(sig.ret, &subst);
        let error = sig
            .error
            .map(|e| self.cg.program.decls.instantiate(e, &subst));
        let receiver = args
            .first()
            .ok_or_else(|| BuildError::Internal("a dynamic call without a receiver".to_string()))?;
        let rty = self.operand_ty(receiver)?;
        let (fat, fat_ll) = self.value(receiver)?;
        let (fat, fat_ty) = match self.cg.program.decls.types.kind(rty).clone() {
            TyKind::Dyn(..) | TyKind::DynFn(..) => {
                let v = self.as_ssa(&fat, &fat_ll, 8)?;
                (v, fat_ll)
            }
            _ => {
                let out = self.fresh_tmp();
                self.emit(format!("{out} = load {{ ptr, ptr }}, ptr {fat}"));
                (out, "{ ptr, ptr }".to_string())
            }
        };
        let data = self.fresh_tmp();
        let table = self.fresh_tmp();
        self.emit(format!("{data} = extractvalue {fat_ty} {fat}, 0"));
        self.emit(format!("{table} = extractvalue {fat_ty} {fat}, 1"));
        let slot = self.fresh_tmp();
        self.emit(format!(
            "{slot} = getelementptr ptr, ptr {table}, i64 {index}"
        ));
        let fp = self.fresh_tmp();
        self.emit(format!("{fp} = load ptr, ptr {slot}"));
        self.call_indirect(
            &fp,
            &params,
            &args[1..],
            ret,
            error,
            false,
            Some(data),
            dest,
            next,
        )
    }

    fn call(
        &mut self,
        target: &CallTarget,
        args: &[Operand],
        dest: Option<&Place>,
        next: Block,
        _span: Span,
    ) -> Result<(), BuildError> {
        match target {
            CallTarget::Value { callee, kind, .. } => match kind {
                ValueKind::FnPtr => self.fn_ptr_call(callee, args, dest, next),
                ValueKind::Closure(_) => self.closure_call(callee, args, dest, next),
                ValueKind::Bound => {
                    let cty = self.operand_ty(callee)?;
                    let mut ty = cty;
                    while let TyKind::Ref(_, inner) = self.cg.program.decls.types.kind(ty).clone() {
                        ty = inner;
                    }
                    match self.cg.program.decls.types.kind(ty) {
                        TyKind::Closure(..) => self.closure_call(callee, args, dest, next),
                        TyKind::FnPtr(_) => self.fn_ptr_call(callee, args, dest, next),
                        _ => self.cg.ni("a call through a bound that is not `Fn` (B2)"),
                    }
                }
                ValueKind::DynFn => self.dyn_fn_call(callee, args, dest, next),
            },
            CallTarget::Fn { fn_ref, .. } => {
                let caller_subst = self.cg.subst.clone();
                if let Some((interface, ty)) = self.cg.builtin_target(fn_ref, &caller_subst)? {
                    let name = self.cg.program.sema.def(fn_ref.def).name.clone();
                    let method = self
                        .cg
                        .program
                        .decls
                        .interface_sig(interface)
                        .and_then(|s| {
                            s.methods
                                .iter()
                                .copied()
                                .find(|m| self.cg.program.sema.def(*m).name == name)
                        })
                        .ok_or_else(|| {
                            BuildError::Internal("a built-in method with no definition".to_string())
                        })?;
                    let sig = self
                        .cg
                        .program
                        .decls
                        .fn_sig(method)
                        .cloned()
                        .ok_or_else(|| {
                            BuildError::Internal("a built-in method with no signature".to_string())
                        })?;
                    let mut subst = Subst::new();
                    if let Some(isig) = self.cg.program.decls.interface_sig(interface) {
                        subst.insert(isig.self_def, ingot_sema::ty::GenericArg::Ty(ty));
                    }
                    let symbol = self
                        .cg
                        .builtin_symbols
                        .get(&(interface, ty))
                        .cloned()
                        .ok_or_else(|| {
                            BuildError::Internal("a built-in method was not emitted".to_string())
                        })?;
                    return self.call_direct_symbol(&symbol, &sig, &subst, args, dest, next, false);
                }
                if matches!(
                    fn_ref.source,
                    ingot_sema::typed::FnSource::Interface {
                        via: ingot_sema::typed::InterfaceVia::Dyn,
                        ..
                    }
                ) {
                    return self.dyn_interface_call(fn_ref, args, dest, next);
                }
                let (symbol, external, sig, subst) = self.cg.resolve_fn(fn_ref)?;
                self.call_direct_symbol(&symbol, &sig, &subst, args, dest, next, external)
            }
        }
    }
}

impl<'p> Codegen<'p> {
    fn trap_symbol(&self) -> Result<String, BuildError> {
        let key = InstanceKey {
            owner: self.rt_trap,
            body: InstanceBody::Item,
            subst: Subst::new(),
        };
        self.symbols.get(&key).cloned().ok_or_else(|| {
            BuildError::Internal("the runtime trap function was not discovered".to_string())
        })
    }

    fn start_symbol(&self) -> Result<String, BuildError> {
        let key = InstanceKey {
            owner: self.rt_start,
            body: InstanceBody::Item,
            subst: Subst::new(),
        };
        self.symbols.get(&key).cloned().ok_or_else(|| {
            BuildError::Internal("the runtime start function was not discovered".to_string())
        })
    }

    fn resolve_fn(
        &mut self,
        fn_ref: &ingot_sema::typed::FnRef,
    ) -> Result<(String, bool, FnSig, Subst), BuildError> {
        let def = fn_ref.def;
        let external = self.program.sema.def(def).kind == DefKind::ExternFn;
        let mut call_subst = Subst::new();
        // The signature that goes with the substitution is the one of the body the call
        // reaches. A call through an interface bound names the interface's method, whose
        // signature mentions the interface's `Self`; the instance is the impl's method,
        // whose key binds the impl's parameters and not that `Self`.
        let mut sig_def = def;
        let symbol = if external {
            self.program.sema.def(def).name.clone()
        } else {
            let key = ingot_cfg::instances::callee_key(
                self.program.sema,
                &mut *self.program.decls,
                fn_ref,
                &self.subst,
            )
            .ok_or_else(|| {
                BuildError::Internal(format!(
                    "no instance for the call of `{}`",
                    self.program.sema.def(def).name
                ))
            })?;
            sig_def = key.owner;
            call_subst = key.subst.clone();
            self.symbols.get(&key).cloned().ok_or_else(|| {
                BuildError::Internal(format!(
                    "phase 11a did not discover the instance of `{}`",
                    self.program.sema.def(def).name
                ))
            })?
        };
        let sig = self.program.decls.fn_sig(sig_def).cloned().ok_or_else(|| {
            BuildError::Internal("a call of a function with no signature".to_string())
        })?;
        if external {
            self.check_c_abi(&sig)?;
            let ret = sig.ret;
            let ret_ll =
                if ret == self.program.decls.types.void || ret == self.program.decls.types.never {
                    "void".to_string()
                } else {
                    self.llvm_ty(ret)?
                };
            let mut params_ll = Vec::new();
            for p in &sig.params {
                params_ll.push(self.llvm_ty(p.ty)?);
            }
            let noreturn = sig.ret == self.program.decls.types.never;
            let variadic = if sig.variadic { ", ..." } else { "" };
            let decl = format!(
                "declare {ret_ll} {}({}{variadic}){} nounwind",
                global_ref(&symbol),
                params_ll.join(", "),
                if noreturn { " noreturn" } else { "" }
            );
            self.externs.entry(symbol.clone()).or_insert(decl);
        }
        Ok((symbol, external, sig, call_subst))
    }

    fn fn_value_ty(&mut self, fn_ref: &ingot_sema::typed::FnRef) -> Result<TypeId, BuildError> {
        // As in `resolve_fn`: the signature of the body the value reaches goes with the
        // substitution of its key.
        let key = ingot_cfg::instances::callee_key(
            self.program.sema,
            &mut *self.program.decls,
            fn_ref,
            &self.subst,
        );
        let sig_def = key.as_ref().map_or(fn_ref.def, |k| k.owner);
        let subst = key.map(|k| k.subst).unwrap_or_default();
        let sig = self.program.decls.fn_sig(sig_def).cloned().ok_or_else(|| {
            BuildError::Internal("a function value with no signature".to_string())
        })?;
        let ext = self.program.sema.def(fn_ref.def).kind == DefKind::ExternFn || sig.mods.export;
        let params = sig
            .params
            .iter()
            .map(|p| self.program.decls.instantiate(p.ty, &subst))
            .collect();
        let ret = self.program.decls.instantiate(sig.ret, &subst);
        let error = sig.error.map(|e| self.program.decls.instantiate(e, &subst));
        Ok(self
            .program
            .decls
            .types
            .intern(TyKind::FnPtr(ingot_sema::ty::FnPtrSig {
                ext,
                params,
                ret,
                error,
            })))
    }

    fn declare_overflow(&mut self, intrinsic: &str, ll: &str) -> Result<(), BuildError> {
        self.externs
            .entry(intrinsic.to_string())
            .or_insert_with(|| {
                format!("declare {{ {ll}, i1 }} @{intrinsic}({ll}, {ll}) nounwind readnone")
            });
        Ok(())
    }

    fn declare_sat(&mut self, intrinsic: &str, src: &str, dst: &str) -> Result<(), BuildError> {
        self.externs
            .entry(intrinsic.to_string())
            .or_insert_with(|| format!("declare {dst} @{intrinsic}({src}) nounwind readnone"));
        Ok(())
    }

    fn string_slice_ty(&mut self) -> TypeId {
        let u8_ = self.program.decls.types.int(IntTy::U8);
        self.program
            .decls
            .types
            .intern(TyKind::Slice(Mutability::Shared, u8_))
    }

    fn cstr_ty(&mut self) -> TypeId {
        let u8_ = self.program.decls.types.int(IntTy::U8);
        self.program
            .decls
            .types
            .intern(TyKind::RawPtr(Mutability::Shared, u8_))
    }

    fn add_string(&mut self, bytes: &[u8]) -> String {
        if let Some(sym) = self.global_strings.get(bytes) {
            return sym.clone();
        }
        let sym = format!("str.{}", self.next_global);
        self.next_global += 1;
        let init = if bytes.is_empty() {
            "zeroinitializer".to_string()
        } else {
            format!("c{}", llvm_string(bytes))
        };
        self.globals.push(format!(
            "@{sym} = private unnamed_addr constant [{} x i8] {init}",
            bytes.len()
        ));
        self.global_strings.insert(bytes.to_vec(), sym.clone());
        sym
    }

    fn item_value(&self, def: DefId) -> Option<SemValue> {
        match self.program.decls.values.get(&def) {
            Some(ingot_sema::consteval::ValueState::Done(Ok(value))) => Some(value.clone()),
            _ => None,
        }
    }

    /// The bytes of a compile-time value in the §25 layout of `ty`.
    fn value_bytes(&mut self, value: &SemValue, ty: TypeId) -> Result<Vec<u8>, BuildError> {
        let ty = self.resolve_ty(ty);
        let size = self.layout(ty)?.size;
        let mut bytes = vec![0u8; size as usize];
        self.write_value(value, ty, &mut bytes, 0)?;
        Ok(bytes)
    }

    fn write_value(
        &mut self,
        value: &SemValue,
        ty: TypeId,
        bytes: &mut [u8],
        base: usize,
    ) -> Result<(), BuildError> {
        let mut relocs = Vec::new();
        self.write_value_relocs(value, ty, bytes, base, &mut relocs)
    }

    fn write_value_relocs(
        &mut self,
        value: &SemValue,
        ty: TypeId,
        bytes: &mut [u8],
        base: usize,
        relocs: &mut Vec<(usize, String)>,
    ) -> Result<(), BuildError> {
        let ty = self.resolve_ty(ty);
        let layout = self.layout(ty)?;
        let size = layout.size;
        let at = |off: u64| base + off as usize;
        match value {
            SemValue::Int { ty: vty, value } => {
                let n = self.layout(*vty)?.size;
                if at(0) + n as usize > bytes.len() {
                    return Err(BuildError::Internal(
                        "a constant integer does not fit its place".to_string(),
                    ));
                }
                write_low(&mut bytes[at(0)..], *value, n);
            }
            SemValue::Float { ty: vty, bits } => {
                let n = self.layout(*vty)?.size;
                if at(0) + n as usize > bytes.len() {
                    return Err(BuildError::Internal(
                        "a constant float does not fit its place".to_string(),
                    ));
                }
                write_low(&mut bytes[at(0)..], i128::from(*bits), n);
            }
            SemValue::Bool(b) => {
                if at(0) >= bytes.len() {
                    return Err(BuildError::Internal(
                        "a constant bool does not fit its place".to_string(),
                    ));
                }
                bytes[at(0)] = u8::from(*b);
            }
            SemValue::Unit => {}
            SemValue::Aggregate {
                kind,
                ty: vty,
                fields,
            } => {
                let ty = self.resolve_ty(*vty);
                let layout = self.layout(ty)?;
                match (kind, self.program.decls.types.kind(ty).clone()) {
                    (ingot_sema::value::AggKind::Struct(_), TyKind::Adt(def, args)) => {
                        let sig = self.program.decls.struct_sig(def).cloned().ok_or_else(|| {
                            BuildError::Internal("a constant of a non-struct".to_string())
                        })?;
                        let subst = self.program.decls.subst_for(def, &args);
                        for (i, field) in fields.iter().enumerate() {
                            let fl = layout.fields.get(i).ok_or_else(|| {
                                BuildError::Internal(
                                    "a constant struct has no field layout".to_string(),
                                )
                            })?;
                            let fty = sig.fields.get(i).ok_or_else(|| {
                                BuildError::Internal(
                                    "a constant struct has a field with no type".to_string(),
                                )
                            })?;
                            let fty = self.program.decls.instantiate(fty.ty, &subst);
                            self.write_value_relocs(field, fty, bytes, at(fl.offset), relocs)?;
                        }
                    }
                    (ingot_sema::value::AggKind::Closure(_), TyKind::Closure(def, idx, args)) => {
                        let ftys = self
                            .program
                            .decls
                            .closure_field_tys(def, idx, &args)
                            .ok_or_else(|| {
                                BuildError::Internal("a constant of an unknown closure".to_string())
                            })?;
                        for (i, field) in fields.iter().enumerate() {
                            let fl = layout.fields.get(i).ok_or_else(|| {
                                BuildError::Internal(
                                    "a constant closure has no field layout".to_string(),
                                )
                            })?;
                            let fty = ftys.get(i).copied().ok_or_else(|| {
                                BuildError::Internal(
                                    "a constant closure has a field with no type".to_string(),
                                )
                            })?;
                            self.write_value_relocs(field, fty, bytes, at(fl.offset), relocs)?;
                        }
                    }
                    (ingot_sema::value::AggKind::Tuple, TyKind::Tuple(ts)) => {
                        for (i, field) in fields.iter().enumerate() {
                            let fl = layout.fields.get(i).ok_or_else(|| {
                                BuildError::Internal(
                                    "a constant tuple has no field layout".to_string(),
                                )
                            })?;
                            let fty = ts.get(i).ok_or_else(|| {
                                BuildError::Internal(
                                    "a constant tuple has a field with no type".to_string(),
                                )
                            })?;
                            self.write_value_relocs(field, *fty, bytes, at(fl.offset), relocs)?;
                        }
                    }
                    (ingot_sema::value::AggKind::Array, TyKind::Array(elem, _)) => {
                        let stride = self.layout(elem)?.size;
                        for (i, field) in fields.iter().enumerate() {
                            self.write_value_relocs(
                                field,
                                elem,
                                bytes,
                                at(stride * i as u64),
                                relocs,
                            )?;
                        }
                    }
                    _ => {
                        return self.ni(format!(
                            "a constant aggregate of type {} (B2)",
                            self.type_display(ty)
                        ));
                    }
                }
            }
            SemValue::Pack {
                ty: vty,
                len,
                elems,
                moved,
            } => {
                let elem = self.resolve_ty(*vty);
                let stride = self.layout(elem)?.size;
                let vals = elems.as_deref().map(|PackElems::Ints(vals)| vals);
                for i in 0..*len {
                    if moved.contains(&(i as u32)) {
                        continue;
                    }
                    let value = vals.and_then(|v| v.get(i as usize)).copied().unwrap_or(0);
                    write_low(&mut bytes[at(stride * i)..], value, stride);
                }
            }
            SemValue::Variant {
                ty: vty,
                index,
                payload,
            } => {
                let ty = self.resolve_ty(*vty);
                let layout = self.layout(ty)?;
                let (tag, payload_ty): (i128, Option<TypeId>) =
                    match self.program.decls.types.kind(ty).clone() {
                        TyKind::Result(ok, err) => (
                            i128::from(*index as u8),
                            Some(if *index == 0 { ok } else { err }),
                        ),
                        TyKind::Optional(inner) => (i128::from(*index as u8), Some(inner)),
                        TyKind::Adt(def, args) => {
                            let sig =
                                self.program.decls.enum_sig(def).cloned().ok_or_else(|| {
                                    BuildError::Internal("a constant of a non-enum".to_string())
                                })?;
                            let v = sig.variants.get(*index).ok_or_else(|| {
                                BuildError::Internal(
                                    "a constant variant index out of range".to_string(),
                                )
                            })?;
                            let subst = self.program.decls.subst_for(def, &args);
                            let payload =
                                v.payload.map(|p| self.program.decls.instantiate(p, &subst));
                            (v.tag, payload)
                        }
                        _ => {
                            return Err(BuildError::Internal(
                                "a constant variant of a non-enum".to_string(),
                            ));
                        }
                    };
                match layout.kind {
                    LayoutKind::Niche => {
                        if let Some(p) = payload {
                            self.write_value_relocs(
                                p,
                                payload_ty.ok_or_else(|| {
                                    BuildError::Internal(
                                        "a niche variant without a payload type".to_string(),
                                    )
                                })?,
                                bytes,
                                at(0),
                                relocs,
                            )?;
                        }
                    }
                    LayoutKind::Enum {
                        tag_size,
                        payload_offset,
                    } => {
                        if at(0) + tag_size as usize > bytes.len() {
                            return Err(BuildError::Internal(
                                "a constant tag does not fit its place".to_string(),
                            ));
                        }
                        write_low(&mut bytes[at(0)..], tag, tag_size);
                        if let Some(p) = payload {
                            self.write_value_relocs(
                                p,
                                payload_ty.ok_or_else(|| {
                                    BuildError::Internal(
                                        "a variant without a payload type".to_string(),
                                    )
                                })?,
                                bytes,
                                at(payload_offset),
                                relocs,
                            )?;
                        }
                    }
                    other => {
                        return Err(BuildError::Internal(format!(
                            "a constant variant has layout {other:?}"
                        )));
                    }
                }
            }
            SemValue::Str(_)
            | SemValue::CStr(_)
            | SemValue::Fn { .. }
            | SemValue::Borrow { .. }
            | SemValue::Slice { .. }
            | SemValue::StrPtr { .. } => {
                let Some(expr) = self.pointer_initializer(value, ty)? else {
                    return self.ni("this constant pointer value");
                };
                relocs.push((at(0), expr));
            }
            SemValue::Uninit => {
                return self.ni("a constant that is uninitialized");
            }
        }
        let _ = size;
        Ok(())
    }

    fn value_has_pointer(value: &SemValue) -> bool {
        match value {
            SemValue::Str(_)
            | SemValue::CStr(_)
            | SemValue::StrPtr { .. }
            | SemValue::Fn { .. }
            | SemValue::Borrow { .. }
            | SemValue::Slice { .. } => true,
            SemValue::Aggregate { fields, .. } => fields.iter().any(Self::value_has_pointer),
            SemValue::Variant { payload, .. } => {
                payload.as_deref().is_some_and(Self::value_has_pointer)
            }
            _ => false,
        }
    }

    /// The address expression for a place inside a static, for a `&` or a slice
    /// source that points at one.
    fn static_path_expr(
        &mut self,
        def: DefId,
        path: &[PathComp],
    ) -> Result<Option<String>, BuildError> {
        let Some(ItemSig::Static(sig)) = self.program.decls.item(def).cloned() else {
            return Ok(None);
        };
        let mut ty = sig.ty;
        let mut addr = self.static_ref(def)?;
        for comp in path {
            let (offset, next) = self.path_step(ty, *comp)?;
            if offset != 0 {
                addr = format!("getelementptr i8, ptr {addr}, i64 {offset}");
            }
            ty = next;
        }
        Ok(Some(addr))
    }

    /// The byte offset and type of one `PathComp` inside `ty`.
    fn path_step(&mut self, ty: TypeId, comp: PathComp) -> Result<(u64, TypeId), BuildError> {
        let ty = self.resolve_ty(ty);
        let layout = self.layout(ty)?;
        match (comp, self.program.decls.types.kind(ty).clone()) {
            (PathComp::Field(i), TyKind::Adt(def, args)) => {
                let off = layout.fields.get(i).map(|f| f.offset).ok_or_else(|| {
                    BuildError::Internal("a static path field out of range".to_string())
                })?;
                let sig = self.program.decls.struct_sig(def).cloned().ok_or_else(|| {
                    BuildError::Internal("a field path into a non-struct".to_string())
                })?;
                let subst = self.program.decls.subst_for(def, &args);
                let fty = sig.fields.get(i).ok_or_else(|| {
                    BuildError::Internal("a static path field out of range".to_string())
                })?;
                Ok((off, self.program.decls.instantiate(fty.ty, &subst)))
            }
            (PathComp::Field(i), TyKind::Tuple(ts)) => {
                let off = layout.fields.get(i).map(|f| f.offset).ok_or_else(|| {
                    BuildError::Internal("a static path tuple field out of range".to_string())
                })?;
                let next = ts.get(i).copied().ok_or_else(|| {
                    BuildError::Internal("a static path tuple field out of range".to_string())
                })?;
                Ok((off, next))
            }
            (PathComp::Field(i), TyKind::Array(elem, _)) => {
                let stride = self.layout(elem)?.size;
                Ok((stride * i as u64, elem))
            }
            (PathComp::Payload(v), TyKind::Optional(inner)) => {
                let off = match layout.kind {
                    LayoutKind::Niche => 0,
                    LayoutKind::Enum { payload_offset, .. } => payload_offset,
                    _ => {
                        return Err(BuildError::Internal(
                            "a payload path into an optional with no payload".to_string(),
                        ));
                    }
                };
                let _ = v;
                Ok((off, inner))
            }
            (PathComp::Payload(v), TyKind::Result(ok, err)) => {
                let off = match layout.kind {
                    LayoutKind::Enum { payload_offset, .. } => payload_offset,
                    _ => {
                        return Err(BuildError::Internal(
                            "a payload path into a result with no payload".to_string(),
                        ));
                    }
                };
                Ok((off, if v == 0 { ok } else { err }))
            }
            (PathComp::Payload(v), TyKind::Adt(def, args)) => {
                let sig = self.program.decls.enum_sig(def).cloned().ok_or_else(|| {
                    BuildError::Internal("a payload path into a non-enum".to_string())
                })?;
                let subst = self.program.decls.subst_for(def, &args);
                let variant = sig.variants.get(v).ok_or_else(|| {
                    BuildError::Internal("a static path variant out of range".to_string())
                })?;
                let payload = variant.payload.ok_or_else(|| {
                    BuildError::Internal(
                        "a payload path into a variant with no payload".to_string(),
                    )
                })?;
                let off = match layout.kind {
                    LayoutKind::Enum { payload_offset, .. } => payload_offset,
                    _ => {
                        return Err(BuildError::Internal(
                            "a payload path into an enum with no payload".to_string(),
                        ));
                    }
                };
                Ok((off, self.program.decls.instantiate(payload, &subst)))
            }
            (comp, kind) => Err(BuildError::Internal(format!(
                "a static path step {comp:?} on {kind:?}"
            ))),
        }
    }

    /// The initializer of a value that contains a pointer, where a byte-array global
    /// cannot carry the relocation.
    fn pointer_initializer(
        &mut self,
        value: &SemValue,
        ty: TypeId,
    ) -> Result<Option<String>, BuildError> {
        let ty = self.resolve_ty(ty);
        let kind = self.program.decls.types.kind(ty).clone();
        Ok(match (value, kind) {
            (SemValue::Str(bytes), TyKind::Slice(..)) => {
                let sym = self.add_string(bytes);
                Some(format!("{{ ptr @{sym}, i64 {} }}", bytes.len()))
            }
            (SemValue::CStr(bytes), TyKind::RawPtr(..)) => {
                let mut b = bytes.clone();
                b.push(0);
                let sym = self.add_string(&b);
                Some(format!("@{sym}"))
            }
            (SemValue::StrPtr { bytes, at }, TyKind::RawPtr(..)) => {
                let sym = self.add_string(bytes);
                Some(format!("getelementptr i8, ptr @{sym}, i64 {at}"))
            }
            (SemValue::Fn { fn_ref, .. }, TyKind::FnPtr(_)) => {
                let (symbol, _, _, _) = self.resolve_fn(fn_ref)?;
                Some(global_ref(&symbol))
            }
            (
                SemValue::Borrow {
                    alloc: AllocRef::Static(def),
                    path,
                },
                TyKind::Ref(..) | TyKind::RawPtr(..),
            ) => self.static_path_expr(*def, path)?,
            (
                SemValue::Slice {
                    source: SliceSource::Str(bytes),
                    lo,
                    len,
                },
                TyKind::Slice(..),
            ) => {
                let at = (*lo as usize).min(bytes.len());
                let sym = self.add_string(&bytes[at..]);
                Some(format!("{{ ptr @{sym}, i64 {len} }}"))
            }
            (
                SemValue::Slice {
                    source:
                        SliceSource::Alloc {
                            alloc: AllocRef::Static(def),
                            path,
                        },
                    lo,
                    len,
                },
                TyKind::Slice(..),
            ) if *lo == 0 => {
                let p = self.static_path_expr(*def, path)?.ok_or_else(|| {
                    BuildError::Internal("a slice source with no static address".to_string())
                })?;
                Some(format!("{{ ptr {p}, i64 {len} }}"))
            }
            (
                SemValue::Variant {
                    index: 0,
                    payload: None,
                    ..
                },
                TyKind::Optional(inner),
            ) => match self.program.decls.types.kind(inner).clone() {
                TyKind::Slice(..) => Some("{ ptr null, i64 0 }".to_string()),
                _ => Some("null".to_string()),
            },
            (
                SemValue::Variant {
                    index: 1,
                    payload: Some(p),
                    ..
                },
                TyKind::Optional(inner),
            ) => self.pointer_initializer(p, inner)?,
            _ => None,
        })
    }

    /// A typed LLVM initializer for a constant value, for a global that must carry a
    /// relocation. Aggregates are built field by field; an enum payload that can hold
    /// a pointer is an array of `i64` words with `ptrtoint` expressions.
    fn const_init(&mut self, value: &SemValue, ty: TypeId) -> Result<Option<String>, BuildError> {
        let ty = self.resolve_ty(ty);
        let kind = self.program.decls.types.kind(ty).clone();
        if let Some(p) = self.pointer_initializer(value, ty)? {
            let ll = self.llvm_ty(ty)?;
            return Ok(Some(format!("{ll} {p}")));
        }
        Ok(match kind {
            TyKind::Int(i) => {
                let ll = format!("i{}", i.bits());
                match value {
                    SemValue::Int { value, .. } => {
                        Some(format!("{ll} {}", int_literal(&ll, *value)))
                    }
                    _ => Some(format!("{ll} 0")),
                }
            }
            TyKind::Float(f) => {
                let ll = if f == FloatTy::F32 { "float" } else { "double" };
                match value {
                    SemValue::Float { ty: vty, bits } => {
                        let text = match self.program.decls.types.kind(*vty) {
                            TyKind::Float(FloatTy::F32) => format_float_f32(f64::from(
                                ingot_sema::consteval::f32_from_bits(*bits),
                            )),
                            _ => format_float_f64(f64::from_bits(*bits)),
                        };
                        Some(format!("{ll} {text}"))
                    }
                    _ => Some(format!("{ll} 0.0")),
                }
            }
            TyKind::Bool => match value {
                SemValue::Bool(b) => Some(format!("i1 {}", if *b { "true" } else { "false" })),
                _ => Some("i1 false".to_string()),
            },
            TyKind::Void => Some("zeroinitializer".to_string()),
            TyKind::Adt(def, args) => {
                let item = self.program.decls.item(def).cloned();
                let subst = self.program.decls.subst_for(def, &args);
                match item {
                    Some(ItemSig::Struct(sig)) => {
                        let values = match value {
                            SemValue::Aggregate { fields, .. } => fields,
                            _ => return Ok(None),
                        };
                        let tys: Vec<TypeId> = sig
                            .fields
                            .iter()
                            .map(|f| self.program.decls.instantiate(f.ty, &subst))
                            .collect();
                        let Some(init) = self.struct_init(values, &tys, ty)? else {
                            return Ok(None);
                        };
                        Some(init)
                    }
                    Some(ItemSig::Enum(sig)) => {
                        let variants: Vec<(i128, Option<TypeId>)> = sig
                            .variants
                            .iter()
                            .map(|v| {
                                (
                                    v.tag,
                                    v.payload.map(|p| self.program.decls.instantiate(p, &subst)),
                                )
                            })
                            .collect();
                        self.enum_init(value, ty, &variants)?
                    }
                    _ => None,
                }
            }
            TyKind::Optional(inner) => {
                let variants = vec![(0, None), (1, Some(inner))];
                self.enum_init(value, ty, &variants)?
            }
            TyKind::Result(ok, err) => {
                let variants = vec![(0, Some(ok)), (1, Some(err))];
                self.enum_init(value, ty, &variants)?
            }
            TyKind::Tuple(ts) => {
                let values = match value {
                    SemValue::Aggregate { fields, .. } => fields,
                    _ => return Ok(None),
                };
                self.struct_init(values, &ts, ty)?
            }
            TyKind::Array(elem, n) => {
                let values = match value {
                    SemValue::Aggregate { fields, .. } => fields,
                    _ => return Ok(None),
                };
                let len = self
                    .program
                    .decls
                    .types
                    .const_arg_value(n)
                    .and_then(|v| match v {
                        ConstValue::Int { value, .. } if value >= 0 => Some(value as usize),
                        _ => None,
                    })
                    .unwrap_or(values.len());
                let elem_ll = self.llvm_ty(elem)?;
                let mut parts = Vec::new();
                for i in 0..len {
                    let part = match values.get(i) {
                        Some(v) => self.const_init(v, elem)?,
                        None => Some(format!("{elem_ll} zeroinitializer")),
                    };
                    let Some(part) = part else {
                        return Ok(None);
                    };
                    parts.push(part);
                }
                let ll = self.llvm_ty(ty)?;
                if parts.is_empty() {
                    Some(format!("{ll} zeroinitializer"))
                } else {
                    Some(format!("{ll} [{}]", parts.join(", ")))
                }
            }
            TyKind::Closure(def, idx, args) => {
                let values = match value {
                    SemValue::Aggregate { fields, .. } => fields,
                    _ => return Ok(None),
                };
                let Some(tys) = self.program.decls.closure_field_tys(def, idx, &args) else {
                    return Ok(None);
                };
                self.struct_init(values, &tys, ty)?
            }
            _ => None,
        })
    }

    /// A struct or tuple initializer, with the pads the LLVM type has.
    fn struct_init(
        &mut self,
        values: &[SemValue],
        field_tys: &[TypeId],
        ty: TypeId,
    ) -> Result<Option<String>, BuildError> {
        let layout = self.layout(ty)?;
        let mut parts = Vec::new();
        let mut offset = 0u64;
        for (i, fty) in field_tys.iter().enumerate() {
            let Some(fl) = layout.fields.get(i) else {
                return Ok(None);
            };
            if fl.offset > offset {
                parts.push(format!("[{} x i8] zeroinitializer", fl.offset - offset));
                offset = fl.offset;
            }
            let Some(v) = values.get(i) else {
                return Ok(None);
            };
            let Some(part) = self.const_init(v, *fty)? else {
                return Ok(None);
            };
            parts.push(part);
            offset = offset.max(fl.offset.saturating_add(fl.size));
        }
        let tail = layout.size.saturating_sub(offset);
        if tail > 0 {
            parts.push(format!("[{tail} x i8] zeroinitializer"));
        }
        if let Some(a) = align_type(layout.align) {
            parts.push(format!("{a} []"));
        }
        let ll = self.llvm_ty(ty)?;
        if parts.is_empty() {
            Ok(Some(format!("{ll} {{}}")))
        } else {
            Ok(Some(format!("{ll} {{ {} }}", parts.join(", "))))
        }
    }

    /// An enum initializer. `variants` gives each tag and payload type.
    fn enum_init(
        &mut self,
        value: &SemValue,
        ty: TypeId,
        variants: &[(i128, Option<TypeId>)],
    ) -> Result<Option<String>, BuildError> {
        let layout = self.layout(ty)?;
        if matches!(layout.kind, LayoutKind::Niche) {
            return self.pointer_initializer(value, ty);
        }
        let SemValue::Variant { index, payload, .. } = value else {
            return Ok(None);
        };
        let (tag, payload_ty) = variants
            .get(*index)
            .copied()
            .ok_or_else(|| BuildError::Internal("a constant variant out of range".to_string()))?;
        let (tag_size, payload_offset) = match layout.kind {
            LayoutKind::Enum {
                tag_size,
                payload_offset,
            } => (tag_size, payload_offset),
            _ => return Ok(None),
        };
        let tag_ll = int_for_size(tag_size);
        let mut parts = vec![format!("{tag_ll} {}", int_literal(tag_ll, tag))];
        let offset = tag_size;
        if payload_offset > offset {
            parts.push(format!(
                "[{} x i8] zeroinitializer",
                payload_offset - offset
            ));
        }
        let area = layout.size.saturating_sub(payload_offset);
        let mut has_pointer = false;
        let mut max_align = 1u64;
        for (_, p) in variants {
            if let Some(p) = p {
                has_pointer |= self.type_has_pointer(*p, &mut BTreeSet::new());
                max_align = max_align.max(self.layout(*p)?.align);
            }
        }
        if let Some(a) = align_type(max_align) {
            parts.push(format!("{a} []"));
        }
        if has_pointer {
            let area = area as usize;
            let mut bytes = vec![0u8; area];
            let mut relocs: Vec<(usize, String)> = Vec::new();
            if let (Some(p), Some(pty)) = (payload, payload_ty) {
                self.write_value_relocs(p, pty, &mut bytes, 0, &mut relocs)?;
            }
            let mut words = Vec::new();
            for w in 0..(area / 8) {
                let off = w * 8;
                if let Some((_, expr)) = relocs.iter().find(|(o, _)| *o == off) {
                    words.push(format!("i64 ptrtoint (ptr {expr} to i64)"));
                } else {
                    let mut v = 0u64;
                    for b in 0..8 {
                        v |= u64::from(bytes[off + b]) << (b * 8);
                    }
                    words.push(format!("i64 {v}"));
                }
            }
            parts.push(format!("[{}]", words.join(", ")));
        } else if area > 0 {
            let mut bytes = vec![0u8; area as usize];
            if let (Some(p), Some(pty)) = (payload, payload_ty) {
                self.write_value(p, pty, &mut bytes, 0)?;
            }
            parts.push(format!("[{} x i8] c{}", area, llvm_string(&bytes)));
        } else {
            parts.push("[0 x i8] []".to_string());
        }
        let ll = self.llvm_ty(ty)?;
        Ok(Some(format!("{ll} {{ {} }}", parts.join(", "))))
    }

    /// Emit the private constant array behind each `&dyn` value.
    fn emit_tables(&mut self) -> Result<(), BuildError> {
        let mut keys: Vec<InstanceKey> = self
            .symbols
            .keys()
            .filter(|k| {
                matches!(
                    k.body,
                    InstanceBody::MethodTable { .. } | InstanceBody::DynFnTable { .. }
                )
            })
            .cloned()
            .collect();
        keys.sort_by_key(|k| self.symbols.get(k).cloned().unwrap_or_default());
        for key in keys {
            self.emit_table(&key)?;
        }
        Ok(())
    }

    fn emit_table(&mut self, key: &InstanceKey) -> Result<(), BuildError> {
        let table =
            self.symbols.get(key).cloned().ok_or_else(|| {
                BuildError::Internal("a dynamic table without a symbol".to_string())
            })?;
        let entries: Vec<String> = match &key.body {
            InstanceBody::MethodTable {
                ty,
                interface,
                args,
            } => {
                let methods = method_table_methods(
                    self.program.sema,
                    &mut *self.program.decls,
                    *ty,
                    *interface,
                    args,
                );
                let mut out = Vec::new();
                for (_, method) in methods {
                    let symbol = self.symbols.get(&method).cloned().ok_or_else(|| {
                        BuildError::Internal("a method table entry was not discovered".to_string())
                    })?;
                    out.push(format!("ptr {}", global_ref(&symbol)));
                }
                out
            }
            InstanceBody::DynFnTable { ty, .. } => {
                let entry = if let Some(closure) =
                    closure_instance_key(self.program.sema, self.program.decls, *ty)
                {
                    let symbol = self.symbols.get(&closure).cloned().ok_or_else(|| {
                        BuildError::Internal("a closure table entry was not discovered".to_string())
                    })?;
                    format!("ptr {}", global_ref(&symbol))
                } else {
                    let TyKind::FnPtr(sig) = self.program.decls.types.kind(*ty).clone() else {
                        return self.ni("a dynamic function table of this type (B2)");
                    };
                    if sig.ext {
                        return self.ni("a dynamic function table of an `extern fn` (B2)");
                    }
                    let shim = format!("{table}.call");
                    self.emit_fnptr_shim(&shim, &sig)?;
                    format!("ptr {}", global_ref(&shim))
                };
                vec![entry]
            }
            _ => return Ok(()),
        };
        let text = format!(
            "{} = private constant [{} x ptr] [{}]",
            global_ref(&table),
            entries.len(),
            entries.join(", ")
        );
        self.tables.insert(table, text);
        Ok(())
    }

    /// The one-entry table shim for a `fn` pointer behind `&dyn Fn`: the data pointer
    /// points at a stored `fn` pointer, so the shim loads it and forwards its own
    /// ABI arguments unchanged.
    fn emit_fnptr_shim(
        &mut self,
        symbol: &str,
        sig: &ingot_sema::ty::FnPtrSig,
    ) -> Result<(), BuildError> {
        let ret_abi = self.ret_abi_for(sig.ret, sig.error)?;
        let ret_ll = ret_abi.ret_ll();
        let abi_params = self.abi_params_for(&sig.params)?;
        let mut params = vec!["ptr %data".to_string()];
        for (i, ll) in abi_params.iter().enumerate() {
            params.push(format!("{ll} %arg{}", i + 1));
        }
        let has_res = ret_abi.result().is_some_and(|r| r.ptr);
        if has_res {
            params.push("ptr %res".to_string());
        }
        let mut args: Vec<String> = abi_params
            .iter()
            .enumerate()
            .map(|(i, ll)| format!("{ll} %arg{}", i + 1))
            .collect();
        if has_res {
            args.push("ptr %res".to_string());
        }
        let mut out = String::new();
        out.push_str(&format!(
            "define internal {ret_ll} {} ({}) nounwind #0 {{\nentry:\n",
            global_ref(symbol),
            params.join(", ")
        ));
        out.push_str("  %fp = load ptr, ptr %data\n");
        if ret_ll == "void" {
            out.push_str(&format!("  call void %fp({})\n", args.join(", ")));
            out.push_str("  ret void\n");
        } else {
            out.push_str(&format!(
                "  %r = call {ret_ll} %fp({})\n  ret {ret_ll} %r\n",
                args.join(", ")
            ));
        }
        out.push_str("}\n");
        self.functions.push(out);
        Ok(())
    }

    /// Emit the generated built-in `Eq`/`Ord` method for each requested type.
    fn emit_builtins(&mut self) -> Result<(), BuildError> {
        loop {
            let next = self
                .builtin_requests
                .iter()
                .find(|k| !self.builtin_symbols.contains_key(*k))
                .copied();
            let Some((interface, ty)) = next else {
                break;
            };
            let isig = self
                .program
                .decls
                .interface_sig(interface)
                .cloned()
                .ok_or_else(|| BuildError::Internal("an unknown built-in interface".to_string()))?;
            let name = if Some(interface) == self.program.decls.builtins.eq {
                "eq"
            } else {
                "cmp"
            };
            let method = isig
                .methods
                .iter()
                .copied()
                .find(|m| self.program.sema.def(*m).name == name)
                .ok_or_else(|| BuildError::Internal(format!("the built-in `{name}` is missing")))?;
            let mut subst = Subst::new();
            subst.insert(isig.self_def, ingot_sema::ty::GenericArg::Ty(ty));
            let key = InstanceKey {
                owner: method,
                body: InstanceBody::Item,
                subst,
            };
            let symbol = mangle_key(self.program.decls, self.program.sema, &key);
            self.builtin_symbols.insert((interface, ty), symbol.clone());
            self.emit_builtin_body(interface, ty, &symbol, name)?;
        }
        Ok(())
    }

    fn emit_builtin_body(
        &mut self,
        interface: DefId,
        ty: TypeId,
        symbol: &str,
        name: &str,
    ) -> Result<(), BuildError> {
        let isig = self
            .program
            .decls
            .interface_sig(interface)
            .cloned()
            .ok_or_else(|| BuildError::Internal("an unknown built-in interface".to_string()))?;
        let method = isig
            .methods
            .iter()
            .copied()
            .find(|m| self.program.sema.def(*m).name == name)
            .ok_or_else(|| BuildError::Internal("a built-in method is missing".to_string()))?;
        let ret = self
            .program
            .decls
            .fn_sig(method)
            .map(|s| s.ret)
            .ok_or_else(|| {
                BuildError::Internal("a built-in method with no signature".to_string())
            })?;
        let ret = self
            .program
            .decls
            .instantiate(ret, &self.program.decls.subst_for(interface, &[]));
        let ret_ll = if name == "eq" {
            "i1".to_string()
        } else {
            self.tag_only_enum(ret).ok_or_else(|| {
                BuildError::Internal("`Ord.cmp` does not return `Ordering`".to_string())
            })?
        };
        let ordering = self.ordering_tags()?;
        BuiltinGen::new(self, ty, interface, name, ret_ll, ordering).run(symbol)
    }

    fn ordering_tags(&mut self) -> Result<(i128, i128, i128), BuildError> {
        let Some(def) = self.program.decls.builtins.ordering else {
            return Err(BuildError::Internal(
                "the builtin `Ordering` is missing".to_string(),
            ));
        };
        let sig = self
            .program
            .decls
            .enum_sig(def)
            .cloned()
            .ok_or_else(|| BuildError::Internal("`Ordering` has no signature".to_string()))?;
        let tag = |name: &str| {
            sig.variants
                .iter()
                .find(|v| v.name == name)
                .map(|v| v.tag)
                .ok_or_else(|| BuildError::Internal(format!("`Ordering.{name}` is missing")))
        };
        Ok((tag("less")?, tag("equal")?, tag("greater")?))
    }

    /// A private global for a `const` or `static` item, initialized from the
    /// compile-time value. The global has the item's LLVM type, so a place rooted at
    /// a static indexes with the ordinary typed GEPs.
    fn item_global(
        &mut self,
        def: DefId,
        ty: TypeId,
        kind: &str,
        align: Option<u64>,
        writable: bool,
    ) -> Result<String, BuildError> {
        if let Some(sym) = self.item_globals.get(&def) {
            return Ok(sym.clone());
        }
        let value = self.item_value(def).ok_or_else(|| {
            BuildError::Internal(format!(
                "the {} `{}` has no evaluated value",
                kind,
                self.program.sema.def(def).name
            ))
        })?;
        let ty = self.resolve_ty(ty);
        let layout = self.layout(ty)?;
        let sym = format!("{kind}.{}", self.next_global);
        self.next_global += 1;
        let align = align.unwrap_or(layout.align).max(1);
        // A static with interior mutability is written through `&`, so its global is
        // not `constant` (the optimizer would fold every read).
        let storage = if writable { "global" } else { "constant" };
        if Self::value_has_pointer(&value) {
            let ll = self.llvm_ty(ty)?;
            let init = self.const_init(&value, ty)?.ok_or_else(|| {
                BuildError::NotImplemented(
                    "a static of a pointer-containing aggregate (B2)".to_string(),
                )
            })?;
            let _ = ll;
            self.globals
                .push(format!("@{sym} = private {storage} {init}, align {align}"));
            self.item_globals.insert(def, sym.clone());
            return Ok(sym);
        }
        let bytes = self.value_bytes(&value, ty)?;
        let init = if layout.size == 0 {
            "zeroinitializer".to_string()
        } else {
            format!("c{}", llvm_string(&bytes))
        };
        self.globals.push(format!(
            "@{sym} = private {storage} [{} x i8] {init}, align {align}",
            layout.size
        ));
        self.item_globals.insert(def, sym.clone());
        Ok(sym)
    }

    /// Whether a type contains `std.sync.Cell` by value (the one interior-mutable
    /// language item). A borrow or pointer to a `Cell` does not count, and the
    /// fields of a struct, the payloads of an enum and the captures of a closure do.
    fn interior_mutable(
        &mut self,
        ty: TypeId,
        seen: &mut BTreeSet<TypeId>,
    ) -> Result<bool, BuildError> {
        let ty = self.resolve_ty(ty);
        if !seen.insert(ty) {
            return Ok(false);
        }
        let kind = self.program.decls.types.kind(ty).clone();
        if let TyKind::Adt(def, _) = &kind
            && Some(*def) == self.program.decls.builtins.cell
        {
            return Ok(true);
        }
        // A borrow or pointer to a `Cell` is not interior mutability by value.
        if matches!(
            kind,
            TyKind::Ref(..)
                | TyKind::RawPtr(..)
                | TyKind::Slice(..)
                | TyKind::Dyn(..)
                | TyKind::DynFn(..)
                | TyKind::FnPtr(_)
        ) {
            return Ok(false);
        }
        // The parts held by value: the type arguments of `children` do not include the
        // fields of a struct or the payloads of an enum, so those are walked here.
        let parts: Vec<TypeId> = match kind {
            TyKind::Adt(def, args) => {
                let subst = self.program.decls.subst_for(def, &args);
                match self.program.decls.item(def).cloned() {
                    Some(ItemSig::Struct(sig)) => sig
                        .fields
                        .iter()
                        .map(|f| self.program.decls.instantiate(f.ty, &subst))
                        .collect(),
                    Some(ItemSig::Enum(sig)) => sig
                        .variants
                        .iter()
                        .filter_map(|v| v.payload)
                        .map(|p| self.program.decls.instantiate(p, &subst))
                        .collect(),
                    _ => Vec::new(),
                }
            }
            TyKind::Closure(def, index, args) => self
                .program
                .decls
                .closure_field_tys(def, index, &args)
                .unwrap_or_default(),
            _ => self.program.decls.types.children(ty),
        };
        for part in parts {
            if self.interior_mutable(part, seen)? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn static_ref(&mut self, def: DefId) -> Result<String, BuildError> {
        let (ty, align) = match self.program.decls.item(def) {
            Some(ItemSig::Static(s)) => (
                s.ty,
                s.align
                    .as_ref()
                    .map(|a| a.value)
                    .filter(|v| *v > 0)
                    .map(|v| v as u64),
            ),
            _ => {
                return Err(BuildError::Internal(
                    "a place rooted at something that is not a static".to_string(),
                ));
            }
        };
        let writable = self.interior_mutable(ty, &mut BTreeSet::new())?;
        let sym = self.item_global(def, ty, "static", align, writable)?;
        Ok(global_ref(&sym))
    }

    fn add_branch_weights(&mut self, weights: &[u32]) -> usize {
        let id = self.profile_meta.len();
        let body: Vec<String> = weights.iter().map(|w| format!("i32 {w}")).collect();
        self.profile_meta.push(format!(
            "!{id} = !{{!\"branch_weights\", {}}}",
            body.join(", ")
        ));
        id
    }

    fn add_site(
        &mut self,
        file: usize,
        span: Span,
        code: Trap,
        text: &[u8],
    ) -> Result<String, BuildError> {
        let source = self.program.sources.get(file);
        let pos = source.position(span.start);
        let file_sym = self.add_string(source.path.as_bytes());
        let text_sym = self.add_string(text);
        let sym = format!("site.{}", self.next_site);
        self.next_site += 1;
        let code_num = trap_number(code);
        self.sites.push(format!(
            "@{sym} = private constant {{ i8, [7 x i8], {{ ptr, i64 }}, i32, i32, {{ ptr, i64 }} }} {{ i8 {code_num}, [7 x i8] zeroinitializer, {{ ptr, i64 }} {{ ptr @{file_sym}, i64 {} }}, i32 {}, i32 {}, {{ ptr, i64 }} {{ ptr @{text_sym}, i64 {} }} }}",
            source.path.len(),
            pos.line,
            pos.col,
            text.len()
        ));
        Ok(sym)
    }
}

fn trap_number(t: Trap) -> u8 {
    match t {
        Trap::T01 => 0,
        Trap::T02 => 1,
        Trap::T03 => 2,
        Trap::T04 => 3,
        Trap::T05 => 4,
        Trap::T06 => 5,
        Trap::T07 => 6,
        Trap::T08 => 7,
        Trap::T09 => 8,
        Trap::T10 => 9,
        Trap::T14 => 13,
        Trap::T15 => 14,
        Trap::T16 => 15,
        Trap::T17 => 16,
    }
}

impl<'a, 'p> FnGen<'a, 'p> {
    fn render(&self) -> String {
        let mut out = String::new();
        let symbol = global_ref(&self.body.symbol);
        let mut attrs = String::from(" nounwind");
        if self.body.sig.mods.inline {
            attrs.push_str(" alwaysinline");
        }
        if self.body.sig.mods.noinline {
            attrs.push_str(" noinline");
        }
        let ret_never = self.body.sig.ret == self.cg.program.decls.types.never;
        if ret_never {
            attrs.push_str(" cold noreturn");
        }
        let params: Vec<String> = self
            .abi_params
            .iter()
            .enumerate()
            .map(|(i, ll)| format!("{ll} %arg{i}"))
            .collect();
        let mut params = params;
        if self.ret_abi.result().is_some_and(|r| r.ptr) {
            params.push("ptr %res".to_string());
        }
        let linkage = if self.body.sig.mods.export {
            ""
        } else {
            "internal "
        };
        out.push_str(&format!(
            "define {linkage}{} {symbol}({}){attrs} #0 {{\n",
            self.ret_ll,
            params.join(", ")
        ));
        for (index, block) in self.blocks.iter().chain(self.cold.iter()).enumerate() {
            out.push_str(&format!("{}:\n", block.label));
            if index == 0 {
                for line in &self.entry_allocas {
                    out.push_str("  ");
                    out.push_str(line);
                    out.push('\n');
                }
            }
            for line in &block.lines {
                out.push_str("  ");
                out.push_str(line);
                out.push('\n');
            }
        }
        out.push_str("}\n");
        out
    }
}

impl<'p> Codegen<'p> {
    fn emit_entry(&mut self) -> Result<(), BuildError> {
        let symbol = self
            .main_symbol
            .clone()
            .ok_or_else(|| BuildError::Internal("no main symbol".to_string()))?;
        let body = self
            .bodies
            .get(&symbol)
            .cloned()
            .ok_or_else(|| BuildError::Internal("the main body was not emitted".to_string()))?;
        if let Some(error) = body.sig.error {
            let main = self.main_def()?;
            let from = self.program.sema.def(main).module;
            return self.emit_fallible_entry(&symbol, &body, error, from);
        }
        let ret = body.sig.ret;
        let call = if body.sig.params.is_empty() {
            format!("call {} {}()", self.fn_ret_ll(&body)?, global_ref(&symbol))
        } else if body.sig.params.len() == 1 {
            let p = body.sig.params[0].ty;
            if !matches!(self.program.decls.types.kind(p), TyKind::Ref(..)) {
                return Err(BuildError::Internal(
                    "main's parameter is not a reference (E0514 should have rejected it)"
                        .to_string(),
                ));
            }
            format!(
                "call {} {}(ptr %env)",
                self.fn_ret_ll(&body)?,
                global_ref(&symbol)
            )
        } else {
            return Err(BuildError::Internal(
                "main has more than one parameter (E0514 should have rejected it)".to_string(),
            ));
        };
        let mut out = String::new();
        out.push_str("define internal i32 @_I.entry(ptr %env) nounwind #0 {\n");
        out.push_str("entry:\n");
        if ret == self.program.decls.types.void || ret == self.program.decls.types.never {
            out.push_str(&format!("  {call}\n"));
            out.push_str("  ret i32 0\n");
        } else {
            out.push_str(&format!("  %r = {call}\n"));
            match self.program.decls.types.kind(ret).clone() {
                TyKind::Int(IntTy::U8) => {
                    out.push_str("  %status = zext i8 %r to i32\n");
                    out.push_str("  ret i32 %status\n");
                }
                TyKind::Int(IntTy::I32) => {
                    out.push_str("  ret i32 %r\n");
                }
                _ => {
                    return Err(BuildError::Internal(
                        "main's result is not `void` or `u8` (E0514 should have rejected it)"
                            .to_string(),
                    ));
                }
            }
        }
        out.push_str("}\n");
        self.functions.push(out);
        Ok(())
    }

    /// `_I.entry` of a test build: `rt.test_start(env, tests)`, where `tests` is the
    /// static table of the test functions in `test_items` order (tools.md §3).
    fn emit_test_entry(&mut self) -> Result<(), BuildError> {
        let items = test_items(self.program.sema, self.program.instances);
        let mut symbols = Vec::new();
        for def in &items {
            let key = InstanceKey {
                owner: *def,
                body: InstanceBody::Item,
                subst: Subst::new(),
            };
            let symbol = self.symbols.get(&key).ok_or_else(|| {
                BuildError::Internal("a test has no symbol in a test build".to_string())
            })?;
            symbols.push(format!("ptr {}", global_ref(symbol)));
        }
        let count = symbols.len();
        let init = if symbols.is_empty() {
            "zeroinitializer".to_string()
        } else {
            format!("[{}]", symbols.join(", "))
        };
        self.globals.push(format!(
            "@_I.tests = private unnamed_addr constant [{count} x ptr] {init}"
        ));
        let key = InstanceKey {
            owner: self.rt_test_start,
            body: InstanceBody::Item,
            subst: Subst::new(),
        };
        let test_start = self
            .symbols
            .get(&key)
            .map(|s| global_ref(s))
            .ok_or_else(|| BuildError::Internal("rt.test_start was not rooted".to_string()))?;
        let sig = self
            .program
            .decls
            .fn_sig(self.rt_test_start)
            .cloned()
            .ok_or_else(|| BuildError::Internal("no signature for rt.test_start".to_string()))?;
        if self.abi_params(&sig)? != ["ptr", "ptr", "i64"] || self.plain_ret_ll(&sig)? != "i32" {
            return Err(BuildError::Internal(
                "rt.test_start is not `fn(&Env, []fn(&TestEnv) -> i32) -> i32`".to_string(),
            ));
        }
        let mut out = String::new();
        out.push_str("define internal i32 @_I.entry(ptr %env) nounwind #0 {\n");
        out.push_str("entry:\n");
        out.push_str(&format!(
            "  %status = call i32 {test_start}(ptr %env, ptr @_I.tests, i64 {count})\n"
        ));
        out.push_str("  ret i32 %status\n");
        out.push_str("}\n");
        self.functions.push(out);
        self.generate_printers()
    }

    fn emit_process_main(&mut self) -> Result<(), BuildError> {
        let start = global_ref(&self.start_symbol()?);
        let start_sig = self
            .program
            .decls
            .fn_sig(self.rt_start)
            .cloned()
            .ok_or_else(|| BuildError::Internal("no signature for rt.start".to_string()))?;
        if start_sig.params.len() != 4 {
            return Err(BuildError::Internal(
                "rt.start does not have four parameters".to_string(),
            ));
        }
        let mut out = String::new();
        out.push_str("define i32 @main(i32 %argc, ptr %argv, ptr %envp) nounwind #0 {\n");
        out.push_str("entry:\n");
        out.push_str(&format!(
            "  call void {start}(i32 %argc, ptr %argv, ptr %envp, ptr @_I.entry)\n"
        ));
        out.push_str("  unreachable\n");
        out.push_str("}\n");
        self.functions.push(out);
        Ok(())
    }
}
