//! The Ingot back end (pipeline phases 11b-14): one LLVM IR text module for a
//! program and the `clang` link step.
//!
//! The IR is text written by this crate (COMPILER.md, "The back end"): no LLVM
//! library, no dependency. The pinned compiler is upstream LLVM 22
//! (`/opt/homebrew/opt/llvm@22/bin/clang` on this Mac); a missing or different
//! major version is a named build failure, never Apple `clang`.
//!
//! B1 lowers monomorphic instances. A generic instance is a `not implemented:
//! generic instances (B2)` entry, and so is any construct this step does not
//! lower yet.

use ingot_cfg::Cfgs;
use ingot_cfg::instances::{Instance, InstanceBody, InstanceKey, mangle_key};
use ingot_cfg::model::{
    AggregateKind, Block, BlockData, BoundsKind, CallTarget, CastKind, Cfg, Check, CheckKind,
    Const, Operand, Place, PlaceBase, Projection, Rvalue, StmtKind, Terminator, Trap, ValueKind,
};
use ingot_diag::source::{Sources, Span};
use ingot_sema::defs::{DefId, DefKind, Sema};
use ingot_sema::sigs::{Declarations, FnSig, ItemSig, Layout, LayoutKind};
use ingot_sema::ty::{ConstValue, FloatTy, IntTy, Mutability, Subst, TyKind, TypeId};
use ingot_syntax::ast::BinOp;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::PathBuf;

pub mod llvm;

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
    /// Index of the checked package among `sema.packages`.
    pub checked: usize,
    /// The target name, `aarch64-macos-m4` and so on.
    pub target: &'a str,
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

fn find_cfg(cfgs: &Cfgs, extra: &[Cfg], key: &InstanceKey) -> Option<Cfg> {
    if key.body != InstanceBody::Item {
        return None;
    }
    cfgs.iter()
        .find(|(_, cfg)| cfg.key.owner == key.owner && cfg.key.body == ingot_cfg::BodyKey::Item)
        .map(|(_, cfg)| cfg.clone())
        .or_else(|| {
            extra
                .iter()
                .find(|cfg| cfg.key.owner == key.owner && cfg.key.body == ingot_cfg::BodyKey::Item)
                .cloned()
        })
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
    symbols: BTreeMap<InstanceKey, String>,
    types: BTreeMap<TypeId, TypeInfo>,
    type_defs: Vec<(usize, String)>,
    next_type: usize,
    globals: Vec<String>,
    global_strings: BTreeMap<Vec<u8>, String>,
    sites: Vec<String>,
    next_site: usize,
    next_global: usize,
    externs: BTreeMap<String, String>,
    functions: Vec<String>,
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
            symbols: BTreeMap::new(),
            types: BTreeMap::new(),
            type_defs: Vec::new(),
            next_type: 0,
            globals: Vec::new(),
            global_strings: BTreeMap::new(),
            sites: Vec::new(),
            next_site: 0,
            next_global: 0,
            externs: BTreeMap::new(),
            functions: Vec::new(),
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
        self.collect_instances()?;
        self.find_main()?;
        // Register every type before emitting, so the type section is self-contained.
        let mut bodies: Vec<Body> = self.bodies.values().cloned().collect();
        for body in &bodies {
            self.register_fn_types(body)?;
        }
        for body in bodies.drain(..) {
            self.emit_body(&body)?;
        }
        self.emit_entry()?;
        self.emit_process_main()?;
        Ok(())
    }

    /// The symbol a body is emitted under: an `export fn` keeps its source name;
    /// every other Ingot body is mangled.
    fn symbol_for_key(&self, key: &InstanceKey) -> String {
        let sig = self.program.decls.fn_sig(key.owner);
        if sig.is_some_and(|s| s.mods.export) {
            return self.program.sema.def(key.owner).name.clone();
        }
        mangle_key(self.program.decls, self.program.sema, key)
    }

    /// A concrete `impl P` method has `Self = P` in its key; that is not a generic
    /// instance. A key with a type parameter or a generic impl still is.
    fn concrete_method_subst(&self, key: &InstanceKey) -> bool {
        if !self.program.decls.generics_of(key.owner).is_empty() {
            return false;
        }
        if let Some(parent) = self.program.sema.def(key.owner).parent {
            if self.program.sema.def(parent).kind != DefKind::Impl {
                return false;
            }
            if !self.program.decls.generics_of(parent).is_empty() {
                return false;
            }
        }
        key.subst
            .map
            .values()
            .all(|a| !matches!(a, ingot_sema::ty::GenericArg::Ty(t) if matches!(self.program.decls.types.kind(*t), TyKind::Param(_))))
    }

    fn collect_instances(&mut self) -> Result<(), BuildError> {
        let mut queue: Vec<InstanceKey> = Vec::new();
        let mut seen: BTreeSet<InstanceKey> = BTreeSet::new();
        for instance in self.program.instances {
            if instance.key.body != InstanceBody::Item {
                return self.ni("closures, par-for bodies and method tables (B2)");
            }
            if !instance.key.subst.is_empty() && !self.concrete_method_subst(&instance.key) {
                return Err(BuildError::NotImplemented(
                    "instances with substitution (B2)".to_string(),
                ));
            }
            let key = instance.key.clone();
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
        for key in [start, trap] {
            if seen.insert(key.clone()) {
                let symbol = self.symbol_for_key(&key);
                self.symbols.insert(key.clone(), symbol);
                queue.push(key);
            }
        }
        let mut at = 0;
        while at < queue.len() {
            let key = queue[at].clone();
            at += 1;
            let Some(cfg) = find_cfg(self.program.cfgs, self.program.extra_cfgs, &key) else {
                let def = self.program.sema.def(key.owner);
                if def.kind != DefKind::ExternFn {
                    return Err(BuildError::Internal(format!(
                        "no CFG for instance {}",
                        self.symbols[&key]
                    )));
                }
                continue;
            };
            let sig = self
                .program
                .decls
                .fn_sig(key.owner)
                .cloned()
                .ok_or_else(|| BuildError::Internal(format!("no signature for {}", cfg.name)))?;
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
                self.collect_calls(&key.subst, &block.term, &mut queue, &mut seen)?;
            }
        }
        Ok(())
    }

    fn def_package(&self, def: DefId) -> usize {
        let module = self.program.sema.def(def).module;
        self.program.sema.module(module).package.0 as usize
    }

    fn collect_calls(
        &mut self,
        caller: &Subst,
        term: &Terminator,
        queue: &mut Vec<InstanceKey>,
        seen: &mut BTreeSet<InstanceKey>,
    ) -> Result<(), BuildError> {
        let target = match term {
            Terminator::Call { target, .. } => target,
            _ => return Ok(()),
        };
        let CallTarget::Fn { fn_ref, .. } = target else {
            if matches!(
                target,
                CallTarget::Value {
                    kind: ValueKind::FnPtr,
                    ..
                }
            ) {
                return Ok(());
            }
            return self.ni("calls through closures and dynamic values (B2)");
        };
        if !fn_ref.args.is_empty() {
            return self.ni("generic instances (B2)");
        }
        let def = fn_ref.def;
        if self.program.sema.def(def).kind == DefKind::ExternFn {
            return Ok(());
        }
        // A method of a generic impl is a B2 instance.
        if let Some(parent) = self.program.sema.def(def).parent
            && self.program.sema.def(parent).kind == DefKind::Impl
            && !self.program.decls.generics_of(parent).is_empty()
        {
            return self.ni("generic method instances (B2)");
        }
        let key = ingot_cfg::instances::callee_key(
            self.program.sema,
            &mut *self.program.decls,
            fn_ref,
            caller,
        )
        .ok_or_else(|| {
            BuildError::NotImplemented("instances with substitution (B2)".to_string())
        })?;
        if !key.subst.is_empty() && !self.concrete_method_subst(&key) {
            return self.ni("instances with substitution (B2)");
        }
        if seen.insert(key.clone()) {
            // Phase 11a sees the checked package. A key it did not report there is
            // a bug; a std body is what this walk exists to add.
            if self.def_package(def) == self.program.checked {
                return Err(BuildError::Internal(format!(
                    "phase 11a did not discover the instance of `{}`",
                    self.program.sema.def(def).name
                )));
            }
            let symbol = self.symbol_for_key(&key);
            self.symbols.insert(key.clone(), symbol);
            queue.push(key);
        }
        Ok(())
    }

    fn find_main(&mut self) -> Result<(), BuildError> {
        let package = self
            .program
            .sema
            .packages
            .get(self.program.checked)
            .ok_or_else(|| BuildError::Internal("the checked package is unknown".to_string()))?;
        let root = package.root.ok_or_else(|| {
            BuildError::NotImplemented("a package with no root module".to_string())
        })?;
        let main = self
            .program
            .sema
            .module(root)
            .values
            .get("main")
            .copied()
            .ok_or(BuildError::NoEntryPoint)?;
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
                if !args.is_empty() {
                    return self.ni("generic instances (B2)");
                }
                let (id, name) = self.new_named();
                let item = self.program.decls.item(def).cloned().ok_or_else(|| {
                    BuildError::Internal(format!(
                        "no signature for a type: {}",
                        self.program.sema.def(def).name
                    ))
                })?;
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
                            let ll = self.llvm_ty(f.ty)?;
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
                        for v in &sig.variants {
                            variants.push(VariantInfo {
                                tag: v.tag,
                                payload: v.payload,
                            });
                            if let Some(p) = v.payload {
                                let l = self.layout(p)?;
                                max_size = max_size.max(l.size);
                                max_align = max_align.max(l.align);
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
                        ll_fields.push(format!("[{payload_size} x i8]"));
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
                        ll_fields.push(format!("[{payload_size} x i8]"));
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
                ll_fields.push(format!("[{payload_size} x i8]"));
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
                    return self.ni("an array whose length depends on a parameter (B2)");
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
            TyKind::Closure(..) => self.ni("closures (B2)"),
            TyKind::Param(_) | TyKind::Projection(..) => self.ni("generic instances (B2)"),
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
    /// The LLVM parameter types of the definition (slices are two parameters).
    abi_params: Vec<String>,
    /// Allocations made while lowering. They are emitted in the entry block so an
    /// aggregate in a loop does not grow the frame on every iteration at `-O0`.
    entry_allocas: Vec<String>,
}

impl<'p> Codegen<'p> {
    fn emit_body(&mut self, body: &Body) -> Result<(), BuildError> {
        if body.sig.mods.export {
            self.check_c_abi(&body.sig)?;
        }
        self.subst = body.subst.clone();
        let ret_ll = self.fn_ret_ll(body)?;
        let abi_params = self.abi_params(&body.sig)?;
        let mut generator = FnGen {
            cg: self,
            body,
            blocks: Vec::new(),
            cur: 0,
            cold: Vec::new(),
            next_tmp: 0,
            next_label: 0,
            ret_ll,
            abi_params,
            entry_allocas: Vec::new(),
        };
        generator.generate()?;
        let text = generator.render();
        drop(generator);
        self.subst = Subst::new();
        self.functions.push(text);
        Ok(())
    }

    fn fn_ret_ll(&mut self, body: &Body) -> Result<String, BuildError> {
        if body.sig.ret == self.program.decls.types.void
            || body.sig.ret == self.program.decls.types.never
        {
            return Ok("void".to_string());
        }
        self.llvm_ty(body.sig.ret)
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
            TyKind::Adt(def, ref args) if args.is_empty() => {
                Some(def) == self.program.decls.builtins.idx || self.tag_only_enum(ty).is_some()
            }
            _ => false,
        })
    }

    fn abi_params(&mut self, sig: &FnSig) -> Result<Vec<String>, BuildError> {
        let mut out = Vec::new();
        for p in &sig.params {
            let pty = self.resolve_ty(p.ty);
            match self.program.decls.types.kind(pty) {
                TyKind::Slice(..) => {
                    out.push("ptr".to_string());
                    out.push("i64".to_string());
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
                | TyKind::Result(..) => {
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
                _ => return self.ni(format!("parameter type {}", self.type_display(p.ty))),
            }
        }
        Ok(out)
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
        let TyKind::Adt(def, args) = self.program.decls.types.kind(ty) else {
            return None;
        };
        if !args.is_empty() {
            return None;
        }
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

    fn emit_block(&mut self, block: &BlockData) -> Result<(), BuildError> {
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
                for (tag, block) in cases {
                    arms.push_str(&format!(
                        "{} {}, label %{} ",
                        ll,
                        int_literal(&ll, *tag),
                        self.label_of(*block)
                    ));
                }
                self.emit(format!(
                    "switch {ll} {v}, label %{} [ {arms}]",
                    self.label_of(*default)
                ));
            }
            Terminator::Return {
                value, is_error, ..
            } => {
                if *is_error {
                    return self.cg.ni("error returns (B2)");
                }
                if self.ret_ll == "void" {
                    if self.body.sig.ret == self.cg.program.decls.types.never {
                        self.emit("unreachable");
                    } else {
                        self.emit("ret void");
                    }
                } else {
                    let op = value.as_ref().ok_or_else(|| {
                        BuildError::Internal("a return without a value".to_string())
                    })?;
                    let (v, ll) = self.value(op)?;
                    if ll != self.ret_ll {
                        return Err(BuildError::Internal(format!(
                            "return type {ll} is not {ll_v}",
                            ll_v = self.ret_ll
                        )));
                    }
                    self.emit(format!("ret {ll} {v}"));
                }
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
        let (v, ll) = self.rvalue(value, ty)?;
        let addr = self.place_addr(place)?;
        self.emit(format!("store {ll} {v}, ptr {addr}"));
        Ok(())
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
                    _ => return self.cg.ni("this unary operation (B2)"),
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
            Rvalue::DynCast { .. } => self.cg.ni("dyn values (B2)"),
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
            (Projection::Field { index, .. }, TyKind::Adt(def, _)) => {
                let sig = self.cg.program.decls.struct_sig(def).cloned();
                let Some(sig) = sig else {
                    return Err(BuildError::Internal("a field of a non-struct".to_string()));
                };
                sig.fields
                    .get(*index)
                    .map(|f| f.ty)
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
            PlaceBase::Static(def) => {
                return self.cg.ni(format!(
                    "a static (`{}`) (B2)",
                    self.cg.program.sema.def(*def).name
                ));
            }
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
            Const::Param { .. } => return self.cg.ni("constant generic parameters (B2)"),
            Const::Expr { .. } => return self.cg.ni("constant expressions (B2)"),
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
            Const::Named { name, .. } => self.cg.ni(format!("the constant `{name}` (B2)")),
            Const::Param { name, .. } | Const::Expr { name, .. } => {
                self.cg.ni(format!("the constant expression `{name}` (B2)"))
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
                AggregateKind::Struct(_) | AggregateKind::Tuple,
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
            _ => return self.cg.ni("this saturating operation (B2)"),
        }
        Ok((out, ll.to_string()))
    }

    // --------------------------------------------------------------- casts

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
            "wrap" | "narrow" | "sat" | "nearest" | "bits" => {
                let target = type_arg.ok_or_else(|| {
                    BuildError::Internal(format!("`@{name}` without a type argument"))
                })?;
                let op = arg(0)
                    .ok_or_else(|| BuildError::Internal(format!("`@{name}` without a value")))?;
                self.convert_builtin(name, op, target)
            }
            "debug" => self.cg.ni("`@debug` (B2)"),
            other => self.cg.ni(format!("the builtin `@{other}` (B2)")),
        }
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

    fn call_value(
        &mut self,
        callee: &Operand,
        args: &[Operand],
        dest: Option<&Place>,
        next: Block,
    ) -> Result<(), BuildError> {
        let cty = self.operand_ty(callee)?;
        let sig = match self.cg.program.decls.types.kind(cty).clone() {
            TyKind::FnPtr(sig) => sig,
            _ => return self.cg.ni("a call of a value that is not `fn` (B2)"),
        };
        let (cv, _) = self.value(callee)?;
        let ret_ll = if sig.ret == self.cg.program.decls.types.void
            || sig.ret == self.cg.program.decls.types.never
        {
            "void".to_string()
        } else {
            self.cg.llvm_ty(sig.ret)?
        };
        let mut tys = Vec::new();
        for (i, p) in sig.params.iter().enumerate() {
            let op = args.get(i).ok_or_else(|| {
                BuildError::Internal("a function-pointer call with too few arguments".to_string())
            })?;
            let pty = self.cg.resolve_ty(*p);
            match self.cg.program.decls.types.kind(pty).clone() {
                TyKind::Slice(..) => {
                    let (v, vll) = self.value(op)?;
                    let v = self.as_ssa(&v, &vll, 8)?;
                    let a = self.fresh_tmp();
                    let b = self.fresh_tmp();
                    self.emit(format!("{a} = extractvalue {vll} {v}, 0"));
                    self.emit(format!("{b} = extractvalue {vll} {v}, 1"));
                    tys.push(format!("ptr {a}"));
                    tys.push(format!("i64 {b}"));
                }
                _ => {
                    if let Some(tag) = self.cg.tag_only_enum(pty) {
                        let (v, vll) = self.value(op)?;
                        let v = self.as_ssa(&v, &vll, 8)?;
                        let t = self.fresh_tmp();
                        self.emit(format!("{t} = extractvalue {vll} {v}, 0"));
                        tys.push(format!("{tag} {t}"));
                    } else if let Some((size, align)) = self.cg.aggregate_size(pty)? {
                        let (v, vll) = self.value(op)?;
                        let v = self.as_ssa(&v, &vll, align)?;
                        if size > 16 {
                            let tmp = self.alloca(&vll, align);
                            self.emit(format!("store {vll} {v}, ptr {tmp}"));
                            tys.push(format!("ptr {tmp}"));
                        } else if size > 0 {
                            let tmp = self.split_slot(&vll, &v, align);
                            let a = self.fresh_tmp();
                            self.emit(format!("{a} = load i64, ptr {tmp}"));
                            tys.push(format!("i64 {a}"));
                            if size > 8 {
                                let p = self.fresh_tmp();
                                self.emit(format!("{p} = getelementptr i8, ptr {tmp}, i64 8"));
                                let b = self.fresh_tmp();
                                self.emit(format!("{b} = load i64, ptr {p}"));
                                tys.push(format!("i64 {b}"));
                            }
                        }
                    } else {
                        let (v, vll) = self.value(op)?;
                        tys.push(format!("{vll} {v}"));
                    }
                }
            }
        }
        let call = if ret_ll == "void" {
            self.emit(format!("call void {cv}({})", tys.join(", ")));
            String::new()
        } else {
            let out = self.fresh_tmp();
            self.emit(format!("{out} = call {ret_ll} {cv}({})", tys.join(", ")));
            out
        };
        if ret_ll != "void"
            && let Some(dest) = dest
        {
            let dty = self.place_type(dest)?;
            if dty != self.cg.program.decls.types.never {
                let addr = self.place_addr(dest)?;
                self.emit(format!("store {ret_ll} {call}, ptr {addr}"));
            }
        }
        self.emit(format!("br label %{}", self.label_of(next)));
        Ok(())
    }

    fn call(
        &mut self,
        target: &CallTarget,
        args: &[Operand],
        dest: Option<&Place>,
        next: Block,
        _span: Span,
    ) -> Result<(), BuildError> {
        if let CallTarget::Value {
            callee,
            kind: ValueKind::FnPtr,
            ..
        } = target
        {
            return self.call_value(callee, args, dest, next);
        }
        let CallTarget::Fn { fn_ref, .. } = target else {
            return self.cg.ni("calls through closures and dynamic values (B2)");
        };
        let (symbol, external, ret, params) = self.cg.resolve_fn(fn_ref)?;
        let variadic = self
            .cg
            .program
            .decls
            .fn_sig(fn_ref.def)
            .is_some_and(|s| s.variadic);
        if variadic && args.len() > params.len() {
            return self.cg.ni("variadic extern calls (B2)");
        }
        let ret_ll = if ret == self.cg.program.decls.types.void
            || ret == self.cg.program.decls.types.never
        {
            "void".to_string()
        } else {
            self.cg.llvm_ty(ret)?
        };
        let mut abi_types = Vec::new();
        let mut values = Vec::new();
        if external {
            for (i, p) in params.iter().enumerate() {
                let ll = self.cg.llvm_ty(*p)?;
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
        } else {
            for (i, p) in params.iter().enumerate() {
                let op = args.get(i).ok_or_else(|| {
                    BuildError::Internal("a call with too few arguments".to_string())
                })?;
                let pty = self.cg.resolve_ty(*p);
                match self.cg.program.decls.types.kind(pty).clone() {
                    TyKind::Slice(..) => {
                        let (v, vll) = self.value(op)?;
                        let v = self.as_ssa(&v, &vll, 8)?;
                        let p0 = self.fresh_tmp();
                        let p1 = self.fresh_tmp();
                        self.emit(format!("{p0} = extractvalue {vll} {v}, 0"));
                        self.emit(format!("{p1} = extractvalue {vll} {v}, 1"));
                        abi_types.push("ptr".to_string());
                        abi_types.push("i64".to_string());
                        values.push(p0);
                        values.push(p1);
                    }
                    _ => {
                        if let Some(tag) = self.cg.tag_only_enum(pty) {
                            let (v, vll) = self.value(op)?;
                            let v = self.as_ssa(&v, &vll, 8)?;
                            let t = self.fresh_tmp();
                            self.emit(format!("{t} = extractvalue {vll} {v}, 0"));
                            abi_types.push(tag);
                            values.push(t);
                        } else if let Some((size, align)) = self.cg.aggregate_size(pty)? {
                            let (v, vll) = self.value(op)?;
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
                            let (v, vll) = self.value(op)?;
                            abi_types.push(vll.clone());
                            values.push(v);
                        }
                    }
                }
            }
        }
        let tys: Vec<String> = values
            .iter()
            .enumerate()
            .map(|(i, v)| format!("{} {}", abi_types[i], v))
            .collect();
        let call = if ret_ll == "void" {
            self.emit(format!(
                "call void {}({})",
                global_ref(&symbol),
                tys.join(", ")
            ));
            String::new()
        } else {
            let out = self.fresh_tmp();
            self.emit(format!(
                "{out} = call {ret_ll} {}({})",
                global_ref(&symbol),
                tys.join(", ")
            ));
            out
        };
        if ret_ll != "void"
            && let Some(dest) = dest
        {
            let dty = self.place_type(dest)?;
            if dty != self.cg.program.decls.types.never {
                let addr = self.place_addr(dest)?;
                self.emit(format!("store {ret_ll} {call}, ptr {addr}"));
            }
        }
        self.emit(format!("br label %{}", self.label_of(next)));
        Ok(())
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
    ) -> Result<(String, bool, TypeId, Vec<TypeId>), BuildError> {
        let def = fn_ref.def;
        let sig = self.program.decls.fn_sig(def).cloned().ok_or_else(|| {
            BuildError::Internal("a call of a function with no signature".to_string())
        })?;
        let external = self.program.sema.def(def).kind == DefKind::ExternFn;
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
                BuildError::NotImplemented("instances with substitution (B2)".to_string())
            })?;
            if !key.subst.is_empty() && !self.concrete_method_subst(&key) {
                return self.ni("instances with substitution (B2)");
            }
            self.symbols.get(&key).cloned().ok_or_else(|| {
                BuildError::Internal(format!(
                    "phase 11a did not discover the instance of `{}`",
                    self.program.sema.def(def).name
                ))
            })?
        };
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
        Ok((
            symbol,
            external,
            sig.ret,
            sig.params.iter().map(|p| p.ty).collect(),
        ))
    }

    fn fn_value_ty(&mut self, fn_ref: &ingot_sema::typed::FnRef) -> Result<TypeId, BuildError> {
        let sig = self
            .program
            .decls
            .fn_sig(fn_ref.def)
            .cloned()
            .ok_or_else(|| {
                BuildError::Internal("a function value with no signature".to_string())
            })?;
        let ext = self.program.sema.def(fn_ref.def).kind == DefKind::ExternFn || sig.mods.export;
        Ok(self
            .program
            .decls
            .types
            .intern(TyKind::FnPtr(ingot_sema::ty::FnPtrSig {
                ext,
                params: sig.params.iter().map(|p| p.ty).collect(),
                ret: sig.ret,
                error: sig.error,
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
        let ret = body.sig.ret;
        let call = if body.sig.params.is_empty() {
            format!("call {} {}()", self.fn_ret_ll(&body)?, global_ref(&symbol))
        } else if body.sig.params.len() == 1 {
            let p = body.sig.params[0].ty;
            if !matches!(self.program.decls.types.kind(p), TyKind::Ref(..)) {
                return self.ni("main's parameter is not `&Env` (B1)");
            }
            format!(
                "call {} {}(ptr %env)",
                self.fn_ret_ll(&body)?,
                global_ref(&symbol)
            )
        } else {
            return self.ni("main with more than one parameter (B1)");
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
                _ => return self.ni("main's result (B1: `void`, `u8` or `i32`)"),
            }
        }
        out.push_str("}\n");
        self.functions.push(out);
        Ok(())
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
