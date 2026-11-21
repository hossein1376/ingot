//! Phase 11a: instance discovery, depth and mangled names (pipeline §6a).
//!
//! A body is lowered once, uninstantiated. This module walks the CFGs from the roots
//! of the command, substitutes type and constant arguments at each call, resolves a
//! call through a bound to the impl's method, and records one [`Instance`] per
//! `(body, arguments)`. Depths are shortest distances from the roots; a depth above
//! 64 is E0809. The finished set is sorted by [`Instance::mangled`].

use crate::model::{
    BodyKey, CallTarget, Cfg, CfgId, Cfgs, Operand, PlaceBase, Terminator, ValueKind,
};
use ingot_diag::diagnostic::Diagnostic;
use ingot_diag::source::{Sources, Span};
use ingot_sema::defs::{DefId, DefKind, ModuleId, Sema};
use ingot_sema::sigs::{Bound, Declarations, ItemSig};
use ingot_sema::ty::{ConstArg, ConstValue, GenericArg, Subst, TyKind, TypeId};
use ingot_sema::typed::{Bodies, ExprRes, FnDynCoercion, FnRef, FnSource, InterfaceVia};
use ingot_sema::value::Value as SemValue;
use std::collections::{BTreeMap, BTreeSet, VecDeque};

/// Which command's roots to use (pipeline §6a).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Roots {
    /// Every non-generic function, test, `const` and `static` of the checked package.
    #[default]
    Check,
    /// `main` and every `export fn` of the root package.
    Build,
    /// Every `test` block of the root package (a `bench` block is `ingot bench`'s), or
    /// of one module of it when `discover` is given `test_module`. Tests of a
    /// dependency are never roots.
    Test,
}

/// The body part of an instance.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum InstanceBody {
    /// A function, method or test body.
    Item,
    /// A closure body; the owner carries the arguments.
    Closure(ingot_sema::hir::ExprId),
    /// A `par for` body; the owner carries the arguments.
    ParFor(ingot_sema::hir::StmtId),
    /// A `&dyn I` method table for one type.
    MethodTable {
        ty: TypeId,
        interface: DefId,
        args: Vec<GenericArg>,
    },
    /// A closure's method table for `&dyn Fn…`.
    DynFnTable {
        ty: TypeId,
        kind: ingot_sema::ty::FnKind,
    },
    /// The thunk of the `spawn` that ends block `block` of the owner's body `within`
    /// (`fn(*mut u8)`: it reads the copied arguments and writes the result, rt.md §2).
    /// Every `spawn` has one, because the runtime starts a task through a pointer. The
    /// record layout depends on the types of the operands, so the back end writes the
    /// thunk where it emits the `spawn`; this variant only names it (pipeline §7).
    SpawnThunk { within: BodyKey, block: u32 },
    /// The chunk function of a `par for`: the `fn(captures, lo, hi)` the runtime calls
    /// (rt.md §4). It loops over its part of the iteration space and calls the
    /// per-item body `ParFor` of the same statement. Written and named by the back end,
    /// like a spawn thunk.
    ParForChunk(ingot_sema::hir::StmtId),
}

/// One instance: a body and the substitution of its generic parameters.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct InstanceKey {
    pub owner: DefId,
    pub body: InstanceBody,
    pub subst: Subst,
}

/// A finished instance.
#[derive(Clone, Debug)]
pub struct Instance {
    pub key: InstanceKey,
    pub depth: usize,
    pub mangled: String,
    /// The `instantiation` chain, innermost call site first.
    pub labels: Vec<(usize, Span)>,
}

/// What phase 11a found.
#[derive(Clone, Debug, Default)]
pub struct Instances {
    /// The finished set, sorted by `mangled`.
    pub instances: Vec<Instance>,
    /// Call edges between instances: `(caller, callee, span in the caller)`.
    pub call_edges: Vec<(InstanceKey, InstanceKey, Span)>,
    pub diagnostics: Vec<Diagnostic>,
    pub not_implemented: Vec<String>,
}

/// Discover the instances of the checked packages.
#[allow(clippy::too_many_arguments)]
pub fn discover(
    sources: &Sources,
    sema: &Sema,
    decls: &mut Declarations,
    bodies: &Bodies,
    packages: &[ingot_sema::hir::Package],
    cfgs: &Cfgs,
    roots: Roots,
    root: Option<usize>,
    test_module: Option<ModuleId>,
) -> Instances {
    let mut d = Discovery {
        sources,
        sema,
        decls,
        bodies,
        packages,
        cfgs,
        root,
        test_module,
        index: cfgs.iter().map(|(id, cfg)| (cfg.key, id)).collect(),
        depth: BTreeMap::new(),
        parent: BTreeMap::new(),
        overflow: BTreeMap::new(),
        queue: VecDeque::new(),
        edge_cache: BTreeMap::new(),
        extra_cfgs: BTreeMap::new(),
        table_spans: BTreeMap::new(),
        call_edges: Vec::new(),
        not_implemented: Vec::new(),
    };
    d.add_roots(roots);
    d.run();
    d.finish()
}

/// A resolved function reference: the body's definition, its source, the impl's
/// arguments, the concrete `Self`, and the interface's own arguments.
type ResolvedFn = (DefId, FnSource, Vec<GenericArg>, TypeId, Vec<GenericArg>);

struct Discovery<'a> {
    sources: &'a Sources,
    sema: &'a Sema,
    decls: &'a mut Declarations,
    bodies: &'a Bodies,
    cfgs: &'a Cfgs,
    packages: &'a [ingot_sema::hir::Package],
    /// The root package of the build (`Roots::Build`).
    root: Option<usize>,
    /// `Roots::Test`: only the tests of this module.
    test_module: Option<ModuleId>,
    index: BTreeMap<crate::model::CfgKey, CfgId>,
    depth: BTreeMap<InstanceKey, usize>,
    /// The use that gave an instance its depth: (parent, span in the parent).
    parent: BTreeMap<InstanceKey, (InstanceKey, Span)>,
    /// Instances first reached at depth 65; reported only if no shorter chain exists.
    overflow: BTreeMap<InstanceKey, (InstanceKey, Span)>,
    queue: VecDeque<InstanceKey>,
    /// Edges of one instance, computed on demand.
    edge_cache: BTreeMap<InstanceKey, Vec<(InstanceKey, Span)>>,
    /// CFGs lowered on demand for a `const` or `static` root.
    extra_cfgs: BTreeMap<crate::model::CfgKey, Cfg>,
    /// The source span of a method table, for its methods' `instantiation` labels.
    table_spans: BTreeMap<InstanceKey, Span>,
    /// Edges created by call terminators only (not values and coercions).
    call_edges: Vec<(InstanceKey, InstanceKey, Span)>,
    not_implemented: Vec<String>,
}

impl<'a> Discovery<'a> {
    fn empty(&self) -> Subst {
        Subst::new()
    }

    /// Whether a body has generic parameters of its own or of its impl. `Self` of a
    /// concrete impl is fixed and does not make the body generic.
    fn body_is_generic(&self, owner: DefId) -> bool {
        let mut generic = !self.decls.generics_of(owner).is_empty();
        if let Some(parent) = self.sema.def(owner).parent
            && self.sema.def(parent).kind == DefKind::Impl
        {
            generic |= !self.decls.generics_of(parent).is_empty();
        }
        generic
    }

    /// The root's `Self` binding, when an impl or interface fixes it.
    fn root_subst(&self, owner: DefId) -> Subst {
        let mut subst = Subst::new();
        if let Some(parent) = self.sema.def(owner).parent
            && self.sema.def(parent).kind == DefKind::Impl
            && let Some(sig) = self.decls.impl_sig(parent)
        {
            subst.insert(sig.self_def, GenericArg::Ty(sig.target));
        }
        subst
    }

    fn add_roots(&mut self, roots: Roots) {
        if roots == Roots::Check {
            for body in self.bodies.bodies.values() {
                if matches!(
                    body.kind,
                    ingot_sema::typed::BodyKind::Const | ingot_sema::typed::BodyKind::Static
                ) {
                    self.insert_root(InstanceKey {
                        owner: body.owner,
                        body: InstanceBody::Item,
                        subst: self.empty(),
                    });
                }
            }
        }
        // The generated entry of a build calls `std.sys.rt` (`start`, `trap`,
        // `test_start`, the printers), and no function of the program calls them. When
        // `std` is itself the root package it is a checked package, so phase 11a must
        // report those functions: they are roots too.
        let rt_module = self.sema.module_paths.get("std.sys.rt").copied();
        for (_, cfg) in self.cfgs.iter() {
            if cfg.key.body != BodyKey::Item {
                continue;
            }
            let owner = cfg.key.owner;
            if self.body_is_generic(owner) {
                continue;
            }
            let kind = self.sema.def(owner).kind;
            let runtime = roots != Roots::Check
                && matches!(kind, DefKind::Fn)
                && rt_module == Some(self.sema.def(owner).module)
                && self.root.is_some_and(|root| {
                    root == self.sema.module(self.sema.def(owner).module).package.0 as usize
                });
            let sig = self.decls.fn_sig(owner);
            let yes = match roots {
                Roots::Check => matches!(kind, DefKind::Fn | DefKind::Test { .. }),
                Roots::Build => {
                    let module = self.sema.def(owner).module;
                    let package = self.sema.module(module).package.0 as usize;
                    // The entry is `main` of the root package's root module (05 section
                    // 33), not a `main` in any other module of that package.
                    let entry = self.sema.def(owner).name == "main"
                        && self.root == Some(package)
                        && self.sema.packages[package].root == Some(module);
                    matches!(kind, DefKind::Fn) && (sig.is_some_and(|s| s.mods.export) || entry)
                }
                Roots::Test => {
                    let module = self.sema.def(owner).module;
                    let package = self.sema.module(module).package.0 as usize;
                    matches!(kind, DefKind::Test { bench: false })
                        && self.root.is_none_or(|root| root == package)
                        && self.test_module.is_none_or(|only| only == module)
                }
            };
            if yes || runtime {
                let key = InstanceKey {
                    owner,
                    body: InstanceBody::Item,
                    subst: self.root_subst(owner),
                };
                self.insert_root(key);
            }
        }
        // A `static` or `const` initializer can hold a function address. It has no
        // body root of its own in a build, so the function would otherwise be
        // compiled only if a call happens to reach it.
        if roots != Roots::Check {
            let values: Vec<SemValue> = self
                .decls
                .values
                .values()
                .filter_map(|state| match state {
                    ingot_sema::consteval::ValueState::Done(Ok(value)) => Some(value.clone()),
                    _ => None,
                })
                .collect();
            let empty = Subst::new();
            for value in &values {
                let mut refs = Vec::new();
                value_fn_refs(value, &mut refs);
                for fn_ref in refs {
                    if let Some(key) = callee_key(self.sema, self.decls, &fn_ref, &empty) {
                        self.insert_root(key);
                    }
                }
            }
        }
    }

    fn insert_root(&mut self, key: InstanceKey) {
        if self.depth.contains_key(&key) {
            return;
        }
        self.depth.insert(key.clone(), 0);
        self.queue.push_back(key);
    }

    fn run(&mut self) {
        while let Some(key) = self.queue.pop_front() {
            let depth = self.depth[&key];
            if depth > 64 {
                continue;
            }
            let edges = self.edges(&key);
            for (child, span) in edges {
                let nd = depth + 1;
                if nd > 64 {
                    self.overflow.entry(child).or_insert((key.clone(), span));
                    continue;
                }
                match self.depth.get(&child).copied() {
                    None => {
                        self.depth.insert(child.clone(), nd);
                        self.parent.insert(child.clone(), (key.clone(), span));
                        self.queue.push_back(child);
                    }
                    Some(old) if nd < old => {
                        self.depth.insert(child.clone(), nd);
                        self.parent.insert(child.clone(), (key.clone(), span));
                        self.queue.push_back(child);
                    }
                    Some(old) if nd == old => {
                        let better = self.parent.get(&child).is_none_or(|(p, _)| key < *p);
                        if better {
                            self.parent.insert(child, (key.clone(), span));
                        }
                    }
                    Some(_) => {}
                }
            }
        }
    }

    fn finish(mut self) -> Instances {
        let mut diagnostics = Vec::new();
        // E0809 for an instance that only ever appeared at depth 65.
        for (child, (parent, span)) in std::mem::take(&mut self.overflow) {
            if self.depth.contains_key(&child) {
                continue;
            }
            let source = self.sources.get(self.file_of(parent.owner));
            let mut d = Diagnostic::new(
                "E0809",
                "this instance is more than 64 instantiations deep".to_string(),
                source,
                span,
            );
            self.add_chain(&mut d, &parent, span);
            diagnostics.push(d);
        }
        // E0806 for an operation that is constant only after substitution; the
        // labels name the call sites of its instance.
        for key in self.depth.keys() {
            if !matches!(key.body, InstanceBody::Item)
                || !key
                    .subst
                    .map
                    .values()
                    .any(|a| matches!(a, GenericArg::Const(_)))
            {
                continue;
            }
            let mut found = crate::removal::check_instance(
                self.decls,
                self.sources,
                self.cfgs,
                key.owner,
                &key.subst,
            );
            for d in &mut found {
                self.add_instance_labels(d, key);
            }
            diagnostics.extend(found);
        }
        let call_edges = self.call_edges.clone();
        let mut instances: Vec<Instance> = self
            .depth
            .iter()
            .map(|(key, depth)| Instance {
                key: key.clone(),
                depth: *depth,
                mangled: self.mangle(key),
                labels: self.chain(key),
            })
            .collect();
        instances.sort_by(|a, b| a.mangled.cmp(&b.mangled));
        Instances {
            instances,
            call_edges,
            diagnostics,
            not_implemented: self.not_implemented,
        }
    }

    fn add_instance_labels(&self, d: &mut Diagnostic, key: &InstanceKey) {
        for (file, span) in self.chain(key) {
            let source = self.sources.get(file);
            d.relate_in("instantiation", &source.path, span);
        }
    }

    /// The call sites from `key` up to its root, innermost first.
    fn chain(&self, key: &InstanceKey) -> Vec<(usize, Span)> {
        let mut out = Vec::new();
        let mut cur = key;
        while let Some((parent, span)) = self.parent.get(cur) {
            out.push((self.file_of(parent.owner), *span));
            cur = parent;
        }
        out
    }

    /// The `instantiation` labels of the shortest chain, innermost first.
    fn add_chain(&self, d: &mut Diagnostic, parent: &InstanceKey, span: Span) {
        let mut labels = Vec::new();
        let mut cur = Some(parent.clone());
        let mut at = span;
        while let Some(key) = cur {
            labels.push((self.file_of(key.owner), at));
            let Some((p, s)) = self.parent.get(&key) else {
                break;
            };
            cur = Some(p.clone());
            at = *s;
        }
        // `labels` starts with the innermost use, as diagnostics.md §1 asks.
        for (file, span) in labels {
            let source = self.sources.get(file);
            d.relate_in("instantiation", &source.path, span);
        }
    }

    fn file_of(&self, def: DefId) -> usize {
        self.sema.def(def).file.unwrap_or(0)
    }

    /// The CFG key and CFG of one instance body, when it has one.
    fn cfg_of(&self, key: &InstanceKey) -> Option<(crate::model::CfgKey, CfgId, &Cfg)> {
        let body = match key.body {
            InstanceBody::Item => BodyKey::Item,
            InstanceBody::Closure(e) => BodyKey::Closure(e),
            InstanceBody::ParFor(s) => BodyKey::ParFor(s),
            _ => return None,
        };
        let k = crate::model::CfgKey {
            owner: key.owner,
            body,
        };
        if let Some(id) = self.index.get(&k).copied() {
            return Some((k, id, self.cfgs.get(id)));
        }
        self.extra_cfgs.get(&k).map(|c| (k, CfgId(u32::MAX), c))
    }

    fn edges(&mut self, key: &InstanceKey) -> Vec<(InstanceKey, Span)> {
        if let Some(edges) = self.edge_cache.get(key) {
            return edges.clone();
        }
        let mut edges = Vec::new();
        if let InstanceBody::MethodTable {
            ty,
            interface,
            args,
        } = &key.body
        {
            let span = self.table_spans.get(key).copied().unwrap_or_default();
            self.method_table_edges(*ty, *interface, args, span, &mut edges);
            self.edge_cache.insert(key.clone(), edges.clone());
            return edges;
        }
        if self.cfg_of(key).is_none() {
            self.lower_extra(key.owner);
        }
        let Some((_, _, cfg)) = self.cfg_of(key).map(|(k, id, c)| (k, id, c.clone())) else {
            return edges;
        };
        let subst = key.subst.clone();
        for block in cfg.blocks.iter() {
            let span = block.term_span;
            match &block.term {
                Terminator::Call { target, .. } | Terminator::Spawn { target, .. } => {
                    let before = edges.len();
                    self.call_edges(key.owner, &cfg, &subst, target, span, &mut edges);
                    if matches!(&block.term, Terminator::Call { .. }) {
                        for edge in &edges[before..] {
                            self.call_edges.push((key.clone(), edge.0.clone(), edge.1));
                        }
                    }
                }
                Terminator::ParFor { body, .. } => {
                    let Some((_, _, child_cfg)) = self.cfg_by_id(*body) else {
                        continue;
                    };
                    edges.push((
                        InstanceKey {
                            owner: key.owner,
                            body: match child_cfg.key.body {
                                BodyKey::Closure(e) => InstanceBody::Closure(e),
                                BodyKey::ParFor(s) => InstanceBody::ParFor(s),
                                BodyKey::Item => InstanceBody::Item,
                            },
                            subst: subst.clone(),
                        },
                        span,
                    ));
                }
                Terminator::Dispatch {
                    kernel,
                    kernel_args,
                    ..
                } => {
                    let mapped: Vec<GenericArg> = kernel_args
                        .iter()
                        .map(|a| self.decls.types.subst_arg(a, &subst))
                        .collect();
                    let mut kernel_subst = Subst::new();
                    let params: Vec<DefId> = self
                        .decls
                        .generics_of(*kernel)
                        .iter()
                        .map(|g| g.def)
                        .collect();
                    for (p, a) in params.into_iter().zip(mapped) {
                        kernel_subst.insert(p, a);
                    }
                    edges.push((
                        InstanceKey {
                            owner: *kernel,
                            body: InstanceBody::Item,
                            subst: self.canonical_subst(*kernel, &kernel_subst),
                        },
                        span,
                    ));
                }
                _ => {}
            }
        }
        // Coercions, function values and closure method tables in this body.
        let Some(body) = self.bodies.bodies.get(&key.owner).cloned() else {
            self.edge_cache.insert(key.clone(), edges.clone());
            return edges;
        };
        let call_callees = self.call_callee_ids(body.package);
        for (e, info) in &body.exprs {
            let span = self.body_expr_span(&body, *e);
            if let Some(c) = &info.coerce {
                let ty = self.decls.instantiate(c.self_ty, &subst);
                let args: Vec<GenericArg> = c
                    .interface_args
                    .iter()
                    .map(|a| self.decls.types.subst_arg(a, &subst))
                    .collect();
                let table = InstanceKey {
                    owner: c.interface,
                    body: InstanceBody::MethodTable {
                        ty,
                        interface: c.interface,
                        args,
                    },
                    subst: Subst::new(),
                };
                self.table_spans.insert(table.clone(), span);
                edges.push((table, span));
            }
            if let Some(fd) = &info.fn_dyn {
                self.closure_and_fn_table(key.owner, fd, &subst, span, &mut edges);
            }
            if let ExprRes::Fn(fn_ref) = &info.res
                && !call_callees.contains(e)
                && let Some((child, _)) = self.callee(fn_ref, &subst)
            {
                edges.push((child, span));
            }
        }
        self.edge_cache.insert(key.clone(), edges.clone());
        edges
    }

    /// The expressions used as call callees, including the base of a generic
    /// application or parentheses. Their edge is the call's, not a separate value edge.
    fn call_callee_ids(
        &self,
        package: usize,
    ) -> std::collections::BTreeSet<ingot_sema::hir::ExprId> {
        use ingot_sema::hir::ExprKind;
        let mut out = std::collections::BTreeSet::new();
        let Some(pkg) = self.packages.get(package) else {
            return out;
        };
        for expr in &pkg.exprs {
            if let ExprKind::Call { callee, .. } = &expr.kind {
                let mut e = *callee;
                loop {
                    out.insert(e);
                    match &pkg.expr(e).kind {
                        ExprKind::TypeApply { base, .. } | ExprKind::Paren(base) => e = *base,
                        _ => break,
                    }
                }
            }
        }
        out
    }

    fn body_expr_span(
        &self,
        body: &ingot_sema::typed::TypedBody,
        e: ingot_sema::hir::ExprId,
    ) -> Span {
        self.packages
            .get(body.package)
            .and_then(|pkg| pkg.exprs.get(e.0 as usize).map(|x| x.span))
            .unwrap_or_default()
    }

    /// The type of a value operand in a CFG, before substitution.
    fn operand_type(&self, cfg: &Cfg, op: &Operand) -> Option<TypeId> {
        let Operand::Place(p, _) = op else {
            return None;
        };
        match &p.base {
            PlaceBase::Local(l) => cfg.locals.get(*l as usize).map(|l| l.ty),
            PlaceBase::Static(d) => match self.decls.items.get(d) {
                Some(ItemSig::Static(s)) => Some(s.ty),
                _ => None,
            },
        }
    }

    /// A closure instance from a `Closure(owner, index, args)` type.
    fn closure_instance(&self, ty: TypeId) -> Option<InstanceKey> {
        closure_instance_key(self.sema, self.decls, ty)
    }

    fn closure_and_fn_table(
        &mut self,
        owner: DefId,
        fd: &FnDynCoercion,
        subst: &Subst,
        span: Span,
        out: &mut Vec<(InstanceKey, Span)>,
    ) {
        let ty = self.decls.instantiate(fd.self_ty, subst);
        if let Some(child) = self.closure_instance(ty) {
            out.push((child, span));
        }
        let key = InstanceKey {
            owner,
            body: InstanceBody::DynFnTable { ty, kind: fd.kind },
            subst: Subst::new(),
        };
        out.push((key, span));
    }

    /// The method bodies a `&dyn I` table refers to, as instances.
    fn method_table_edges(
        &mut self,
        ty: TypeId,
        interface: DefId,
        args: &[GenericArg],
        span: Span,
        out: &mut Vec<(InstanceKey, Span)>,
    ) {
        for (_, key) in method_table_methods(self.sema, self.decls, ty, interface, args) {
            out.push((key, span));
        }
    }

    fn lower_extra(&mut self, owner: DefId) {
        if self.extra_cfgs.keys().any(|k| k.owner == owner) {
            return;
        }
        let Some(body) = self.bodies.bodies.get(&owner).cloned() else {
            return;
        };
        if !matches!(
            body.kind,
            ingot_sema::typed::BodyKind::Const | ingot_sema::typed::BodyKind::Static
        ) {
            return;
        }
        if let Ok(cfgs) =
            crate::lower_for_eval(self.sources, self.sema, self.packages, self.decls, &body)
        {
            for (_, cfg) in cfgs.iter() {
                self.extra_cfgs.insert(cfg.key, cfg.clone());
            }
        }
    }

    fn cfg_by_id(&self, id: CfgId) -> Option<(crate::model::CfgKey, CfgId, &Cfg)> {
        self.cfgs
            .has(id)
            .then(|| (self.cfgs.get(id).key, id, self.cfgs.get(id)))
    }

    fn call_edges(
        &mut self,
        owner: DefId,
        cfg: &Cfg,
        caller: &Subst,
        target: &CallTarget,
        span: Span,
        out: &mut Vec<(InstanceKey, Span)>,
    ) {
        match target {
            CallTarget::Fn { fn_ref, .. } => {
                if let Some((interface, ty)) = builtin_comparison_target(self.decls, fn_ref, caller)
                {
                    for key in comparison_element_keys(self.decls, self.sema, interface, ty) {
                        out.push((key, span));
                    }
                } else if let Some((key, _)) = self.callee(fn_ref, caller) {
                    out.push((key, span));
                }
            }
            CallTarget::Value { kind, callee, .. } => {
                if let ValueKind::Closure(e) = kind {
                    // The closure body uses its owner's arguments.
                    out.push((
                        InstanceKey {
                            owner,
                            body: InstanceBody::Closure(*e),
                            subst: caller.clone(),
                        },
                        span,
                    ));
                } else if let Some(ty) = self.operand_type(cfg, callee) {
                    // A call through `F: Fn`: the actual value is a closure type.
                    let ty = self.decls.instantiate(ty, caller);
                    if let Some(child) = self.closure_instance(ty) {
                        out.push((child, span));
                    }
                }
            }
        }
    }

    fn callee(&mut self, fn_ref: &FnRef, caller: &Subst) -> Option<(InstanceKey, Span)> {
        callee_key(self.sema, self.decls, fn_ref, caller).map(|key| (key, Span::empty(0)))
    }

    /// Keep only the parameters the body can name, so one body with one set of
    /// effective arguments has one key whichever call path reached it.
    fn canonical_subst(&self, owner: DefId, subst: &Subst) -> Subst {
        canonical_subst(self.sema, self.decls, owner, subst)
    }

    fn mangle(&self, key: &InstanceKey) -> String {
        mangle_key(self.decls, self.sema, key)
    }
}

/// The generic parameters of a body, in written order: an impl's or interface's
/// parameters and `Self`, then the function's own.
fn body_params(sema: &Sema, decls: &Declarations, owner: DefId) -> Vec<DefId> {
    let mut out = Vec::new();
    if let Some(parent) = sema.def(owner).parent {
        match sema.def(parent).kind {
            DefKind::Impl => {
                out.extend(decls.generics_of(parent).iter().map(|g| g.def));
                if let Some(sig) = decls.impl_sig(parent) {
                    out.push(sig.self_def);
                }
            }
            DefKind::Interface => {
                out.extend(decls.generics_of(parent).iter().map(|g| g.def));
                if let Some(sig) = decls.interface_sig(parent) {
                    out.push(sig.self_def);
                }
            }
            _ => {}
        }
    }
    out.extend(decls.generics_of(owner).iter().map(|g| g.def));
    out
}

/// Keep only the parameters the body can name, so one body with one set of
/// effective arguments has one key whichever call path reached it.
fn canonical_subst(sema: &Sema, decls: &Declarations, owner: DefId, subst: &Subst) -> Subst {
    let mut out = Subst::new();
    for p in body_params(sema, decls, owner) {
        if let Some(a) = subst.get(p) {
            out.insert(p, a.clone());
        }
    }
    out
}

fn args_for(decls: &Declarations, owner: DefId, subst: &Subst) -> Vec<GenericArg> {
    decls
        .generics_of(owner)
        .iter()
        .map(|g| {
            subst
                .get(g.def)
                .cloned()
                .unwrap_or(GenericArg::Const(ConstArg::Poison))
        })
        .collect()
}

/// The body and its bindings for a function or method reference, resolving a
/// `Bound` or `Dyn` interface method to the impl's.
fn resolve_fn(
    sema: &Sema,
    decls: &mut Declarations,
    fn_ref: &FnRef,
    caller: &Subst,
) -> Option<ResolvedFn> {
    match &fn_ref.source {
        FnSource::Interface {
            interface,
            interface_args,
            self_ty,
            via: InterfaceVia::Bound | InterfaceVia::Dyn,
        } => {
            let self_ty = decls.instantiate(*self_ty, caller);
            let name = sema.def(fn_ref.def).name.clone();
            let mut found = None;
            for imp in decls
                .impls
                .by_interface
                .get(interface)
                .cloned()
                .unwrap_or_default()
            {
                let Some(sig) = decls.impl_sig(imp).cloned() else {
                    continue;
                };
                let Some(m) = decls.match_impl(imp, self_ty) else {
                    continue;
                };
                if decls.impl_bounds_hold(&m) == ingot_sema::sigs::Tri::No {
                    continue;
                }
                let method = sig.methods.get(&name).copied().unwrap_or(fn_ref.def);
                let impl_args = args_for(decls, imp, &m.subst);
                found = Some((method, impl_args, self_ty, interface_args.clone()));
                break;
            }
            let (def, impl_args, self_ty, interface_args) = found?;
            Some((
                def,
                FnSource::Interface {
                    interface: *interface,
                    interface_args,
                    self_ty,
                    via: InterfaceVia::Impl {
                        impl_def: decls.fn_sig(def)?.parent?,
                        impl_args: impl_args.clone(),
                    },
                },
                impl_args,
                self_ty,
                Vec::new(),
            ))
        }
        FnSource::Inherent {
            impl_def,
            impl_args,
        } => Some((
            fn_ref.def,
            fn_ref.source.clone(),
            impl_args.clone(),
            decls
                .impl_sig(*impl_def)
                .map(|s| s.target)
                .unwrap_or(decls.types.void),
            Vec::new(),
        )),
        FnSource::Interface {
            interface: _,
            interface_args,
            self_ty,
            via: InterfaceVia::Impl { impl_args, .. },
        } => Some((
            fn_ref.def,
            fn_ref.source.clone(),
            impl_args.clone(),
            *self_ty,
            interface_args.clone(),
        )),
        FnSource::Free => Some((
            fn_ref.def,
            fn_ref.source.clone(),
            Vec::new(),
            decls.types.void,
            Vec::new(),
        )),
        _ => Some((
            fn_ref.def,
            fn_ref.source.clone(),
            Vec::new(),
            decls.types.void,
            Vec::new(),
        )),
    }
}

/// A closure instance from a `Closure(owner, index, args)` type (or a reference to
/// one). Phase 11a and the back end must agree on the key.
pub fn closure_instance_key(sema: &Sema, decls: &Declarations, ty: TypeId) -> Option<InstanceKey> {
    let TyKind::Closure(owner, index, args) = decls.types.kind(ty).clone() else {
        if let TyKind::Ref(_, inner) = decls.types.kind(ty) {
            return closure_instance_key(sema, decls, *inner);
        }
        return None;
    };
    let params = body_params(sema, decls, owner);
    let mut subst = Subst::new();
    for (p, a) in params.into_iter().zip(args) {
        subst.insert(p, a);
    }
    Some(InstanceKey {
        owner,
        body: InstanceBody::Closure(ingot_sema::hir::ExprId(index)),
        subst,
    })
}

/// The methods of `interface` and its supertraits, in method-table order: the
/// interface's own methods in declaration order, then each supertrait's methods
/// depth-first in supertrait declaration order, each interface once.
pub fn interface_method_order(decls: &mut Declarations, interface: DefId) -> Vec<DefId> {
    let mut interfaces = Vec::new();
    let mut seen = BTreeSet::new();
    walk_interfaces(decls, interface, &[], &mut seen, &mut interfaces);
    let mut out = Vec::new();
    for (iface, _) in interfaces {
        if let Some(sig) = decls.interface_sig(iface) {
            out.extend(sig.methods.iter().copied());
        }
    }
    out
}

/// The method-table entries of `&dyn I`, in the order of
/// [`interface_method_order`]. Each entry is the interface's declared method and the
/// concrete instance that implements it.
pub fn method_table_methods(
    sema: &Sema,
    decls: &mut Declarations,
    ty: TypeId,
    interface: DefId,
    args: &[GenericArg],
) -> Vec<(DefId, InstanceKey)> {
    let mut interfaces = Vec::new();
    let mut seen = BTreeSet::new();
    walk_interfaces(decls, interface, args, &mut seen, &mut interfaces);
    let mut out = Vec::new();
    for (iface, iface_args) in interfaces {
        let Some(sig) = decls.interface_sig(iface).cloned() else {
            continue;
        };
        let Some((_, _, matched)) = decls
            .interface_impls(ty)
            .into_iter()
            .find(|(i, _, _)| *i == iface)
        else {
            continue;
        };
        let Some(impl_sig) = decls.impl_sig(matched.impl_def).cloned() else {
            continue;
        };
        let mut base = Subst::new();
        for g in decls.generics_of(matched.impl_def) {
            if let Some(a) = matched.subst.get(g.def) {
                base.insert(g.def, a.clone());
            }
        }
        base.insert(impl_sig.self_def, GenericArg::Ty(ty));
        let iface_params: Vec<DefId> = decls.generics_of(iface).iter().map(|g| g.def).collect();
        for (p, a) in iface_params.into_iter().zip(iface_args.iter().cloned()) {
            base.insert(p, a);
        }
        if let Some(iface_sig) = decls.interface_sig(iface) {
            base.insert(iface_sig.self_def, GenericArg::Ty(ty));
        }
        for method in &sig.methods {
            let name = sema.def(*method).name.clone();
            let def = impl_sig.methods.get(&name).copied().unwrap_or(*method);
            out.push((
                *method,
                InstanceKey {
                    owner: def,
                    body: InstanceBody::Item,
                    subst: canonical_subst(sema, decls, def, &base),
                },
            ));
        }
    }
    out
}

fn walk_interfaces(
    decls: &mut Declarations,
    interface: DefId,
    args: &[GenericArg],
    seen: &mut BTreeSet<DefId>,
    out: &mut Vec<(DefId, Vec<GenericArg>)>,
) {
    if !seen.insert(interface) {
        return;
    }
    let Some(sig) = decls.interface_sig(interface).cloned() else {
        return;
    };
    out.push((interface, args.to_vec()));
    let mut subst = Subst::new();
    for (g, a) in decls.generics_of(interface).iter().zip(args) {
        subst.insert(g.def, a.clone());
    }
    for sup in sig.supers {
        if let Bound::Interface(i, iargs) = sup {
            let mapped = iargs
                .iter()
                .map(|a| decls.types.subst_arg(a, &subst))
                .collect::<Vec<_>>();
            walk_interfaces(decls, i, &mapped, seen, out);
        }
    }
}

/// A function address held in a constant or static value.
pub fn value_fn_refs(value: &SemValue, out: &mut Vec<FnRef>) {
    match value {
        SemValue::Fn { fn_ref, .. } => out.push((**fn_ref).clone()),
        SemValue::Aggregate { fields, .. } => {
            for field in fields {
                value_fn_refs(field, out);
            }
        }
        SemValue::Variant {
            payload: Some(payload),
            ..
        } => value_fn_refs(payload, out),
        _ => {}
    }
}

/// Whether `fn_ref` is a generated built-in `Eq`/`Ord` call, and for which type.
pub fn builtin_comparison_target(
    decls: &mut Declarations,
    fn_ref: &FnRef,
    caller: &Subst,
) -> Option<(DefId, TypeId)> {
    let FnSource::Interface {
        interface,
        self_ty,
        via,
        ..
    } = &fn_ref.source
    else {
        return None;
    };
    if !matches!(via, InterfaceVia::Builtin | InterfaceVia::Bound) {
        return None;
    }
    let ty = decls.instantiate(*self_ty, caller);
    is_comparison_builtin(decls, ty, *interface).then_some((*interface, ty))
}

/// A composite comparison whose elements may need user `Eq`/`Ord` instances.
pub fn comparison_element_keys(
    decls: &mut Declarations,
    sema: &Sema,
    interface: DefId,
    ty: TypeId,
) -> Vec<InstanceKey> {
    let mut out = Vec::new();
    collect_comparison_elements(decls, sema, interface, ty, &mut out);
    out
}

fn collect_comparison_elements(
    decls: &mut Declarations,
    sema: &Sema,
    interface: DefId,
    ty: TypeId,
    out: &mut Vec<InstanceKey>,
) {
    match decls.types.kind(ty).clone() {
        TyKind::Ref(_, inner)
        | TyKind::Array(inner, _)
        | TyKind::Slice(_, inner)
        | TyKind::Optional(inner) => {
            collect_comparison_elements(decls, sema, interface, inner, out);
        }
        TyKind::Tuple(ts) => {
            for t in ts {
                collect_comparison_elements(decls, sema, interface, t, out);
            }
        }
        _ => {
            if decls.builtin_implements(ty, interface) != ingot_sema::sigs::Tri::Yes
                && let Some(key) = comparison_method_key(sema, decls, interface, ty)
            {
                out.push(key);
            }
        }
    }
}

/// The user `impl` method instance for `Eq.eq`/`Ord.cmp` on `ty`.
pub fn comparison_method_key(
    sema: &Sema,
    decls: &mut Declarations,
    interface: DefId,
    ty: TypeId,
) -> Option<InstanceKey> {
    let name = match Some(interface) {
        i if i == decls.builtins.eq => "eq",
        i if i == decls.builtins.ord => "cmp",
        _ => return None,
    };
    let sig = decls.interface_sig(interface).cloned()?;
    let method = sig
        .methods
        .iter()
        .copied()
        .find(|m| sema.def(*m).name == name)?;
    let (_, _, matched) = decls
        .interface_impls(ty)
        .into_iter()
        .find(|(i, _, _)| *i == interface)?;
    let impl_sig = decls.impl_sig(matched.impl_def).cloned()?;
    let mut base = Subst::new();
    for g in decls.generics_of(matched.impl_def) {
        if let Some(a) = matched.subst.get(g.def) {
            base.insert(g.def, a.clone());
        }
    }
    base.insert(impl_sig.self_def, GenericArg::Ty(ty));
    if let Some(iface_sig) = decls.interface_sig(interface) {
        base.insert(iface_sig.self_def, GenericArg::Ty(ty));
    }
    let def = impl_sig.methods.get(name).copied().unwrap_or(method);
    Some(InstanceKey {
        owner: def,
        body: InstanceBody::Item,
        subst: canonical_subst(sema, decls, def, &base),
    })
}

/// Whether the built-in comparison for `ty` is generated (a scalar, a payload-free
/// enum, or a structural composite) rather than a user `impl`.
pub fn is_comparison_builtin(decls: &Declarations, ty: TypeId, interface: DefId) -> bool {
    if Some(interface) != decls.builtins.eq && Some(interface) != decls.builtins.ord {
        return false;
    }
    match decls.types.kind(ty) {
        TyKind::Ref(..)
        | TyKind::Array(..)
        | TyKind::Slice(..)
        | TyKind::Optional(..)
        | TyKind::Tuple(..) => true,
        _ => decls.builtin_implements(ty, interface) == ingot_sema::sigs::Tri::Yes,
    }
}

/// The callee's instance key for a `FnRef` under a caller substitution. Phase 11a
/// and the back end must agree on every call, so this is the only construction of
/// a call key.
pub fn callee_key(
    sema: &Sema,
    decls: &mut Declarations,
    fn_ref: &FnRef,
    caller: &Subst,
) -> Option<InstanceKey> {
    let (def, source, impl_args, _self_ty, _iface_args) = resolve_fn(sema, decls, fn_ref, caller)?;
    let mut subst = Subst::new();
    // The source supplies the impl's or interface's arguments; the method's own
    // arguments are in `fn_ref.args`.
    match &source {
        FnSource::Inherent { impl_def, .. } => {
            if let Some(sig) = decls.impl_sig(*impl_def).cloned() {
                let impl_params: Vec<DefId> =
                    decls.generics_of(*impl_def).iter().map(|g| g.def).collect();
                let mapped: Vec<GenericArg> = impl_args
                    .iter()
                    .map(|a| decls.types.subst_arg(a, caller))
                    .collect();
                for (p, a) in impl_params.into_iter().zip(mapped) {
                    subst.insert(p, a);
                }
                let base = subst.clone();
                let target = decls.instantiate(sig.target, &base);
                subst.insert(sig.self_def, GenericArg::Ty(target));
            }
        }
        FnSource::Interface {
            interface,
            interface_args,
            self_ty,
            via,
        } => {
            let self_ty = decls.instantiate(*self_ty, caller);
            let iface_params: Vec<DefId> = decls
                .generics_of(*interface)
                .iter()
                .map(|g| g.def)
                .collect();
            let mapped: Vec<GenericArg> = interface_args
                .iter()
                .map(|a| decls.types.subst_arg(a, caller))
                .collect();
            for (p, a) in iface_params.into_iter().zip(mapped) {
                subst.insert(p, a);
            }
            if let Some(self_def) = decls.interface_sig(*interface).map(|sig| sig.self_def) {
                subst.insert(self_def, GenericArg::Ty(self_ty));
            }
            if let InterfaceVia::Impl { impl_def, .. } = via
                && let Some(sig) = decls.impl_sig(*impl_def).cloned()
            {
                let impl_params: Vec<DefId> =
                    decls.generics_of(*impl_def).iter().map(|g| g.def).collect();
                let mapped: Vec<GenericArg> = impl_args
                    .iter()
                    .map(|a| decls.types.subst_arg(a, caller))
                    .collect();
                for (p, a) in impl_params.into_iter().zip(mapped) {
                    subst.insert(p, a);
                }
                let base = subst.clone();
                let target = decls.instantiate(sig.target, &base);
                subst.insert(sig.self_def, GenericArg::Ty(target));
            }
        }
        FnSource::Free => {}
    }
    let method_mapped: Vec<GenericArg> = fn_ref
        .args
        .iter()
        .map(|a| decls.types.subst_arg(a, caller))
        .collect();
    let env = subst.clone();
    let method_params: Vec<DefId> = decls.generics_of(def).iter().map(|g| g.def).collect();
    for (p, a) in method_params.into_iter().zip(method_mapped) {
        let a = match a {
            GenericArg::Ty(t) => GenericArg::Ty(decls.types.subst(t, &env)),
            GenericArg::Const(c) => GenericArg::Const(subst_const(c, &env)),
        };
        subst.insert(p, a);
    }
    Some(InstanceKey {
        owner: def,
        body: InstanceBody::Item,
        subst: canonical_subst(sema, decls, def, &subst),
    })
}

/// The mangled symbol of an instance (pipeline §7). Deterministic and one-to-one;
/// the back end and phase 11a must spell a body the same way.
pub fn mangle_key(decls: &Declarations, sema: &Sema, key: &InstanceKey) -> String {
    let def = sema.def(key.owner);
    let module = sema.module(def.module);
    let package = sema
        .packages
        .get(module.package.0 as usize)
        .map(|p| p.name.clone())
        .unwrap_or_default();
    let mut out = String::from("_I");
    out.push_str(&seg(&package));
    out.push_str(&seg(&module.path));
    let mut item = item_path(sema, decls, key.owner);
    match &key.body {
        InstanceBody::Item => {}
        InstanceBody::Closure(e) => item.push_str(&format!(".{{closure {}}}", e.0)),
        InstanceBody::ParFor(s) => item.push_str(&format!(".{{par for {}}}", s.0)),
        InstanceBody::MethodTable {
            ty,
            interface,
            args,
        } => {
            item = format!(
                "{{method table {} {}{}}}",
                spell_type(decls, sema, *ty),
                item_path(sema, decls, *interface),
                spell_args(decls, sema, args)
            );
        }
        InstanceBody::DynFnTable { ty, kind } => {
            item = format!("{{fn table {} {:?}}}", spell_type(decls, sema, *ty), kind);
        }
        InstanceBody::SpawnThunk { within, block } => {
            match within {
                BodyKey::Item => {}
                BodyKey::Closure(e) => item.push_str(&format!(".{{closure {}}}", e.0)),
                BodyKey::ParFor(s) => item.push_str(&format!(".{{par for {}}}", s.0)),
            }
            item.push_str(&format!(".{{spawn {block}}}"));
        }
        InstanceBody::ParForChunk(s) => item.push_str(&format!(".{{par for {} chunk}}", s.0)),
    }
    out.push_str(&seg(&item));
    for arg in key.subst.map.values() {
        out.push_str(&seg(&spell_arg(decls, sema, arg)));
    }
    out
}

/// The mangled spelling of a type, for the one-to-one test below.
pub fn mangled_type(decls: &Declarations, sema: &Sema, ty: TypeId) -> String {
    spell_type(decls, sema, ty)
}

fn seg(s: &str) -> String {
    format!("{}:{}", s.len(), s)
}

fn item_path(sema: &Sema, decls: &Declarations, def: DefId) -> String {
    let d = sema.def(def);
    if matches!(d.kind, DefKind::GenericParam { .. } | DefKind::SelfTy) {
        return sema.qualified_name(def);
    }
    if let Some(parent) = d.parent {
        if sema.def(parent).kind == DefKind::Impl
            && let Some(sig) = decls.impl_sig(parent)
        {
            return format!("{}.{}", spell_type(decls, sema, sig.target), d.name);
        }
        if sema.def(parent).kind == DefKind::Interface {
            return format!("{}.{}", item_path(sema, decls, parent), d.name);
        }
    }
    sema.qualified_name(def)
}

fn spell_args(decls: &Declarations, sema: &Sema, args: &[GenericArg]) -> String {
    let mut out = String::new();
    for a in args {
        out.push('[');
        out.push_str(&spell_arg(decls, sema, a));
        out.push(']');
    }
    out
}

/// The one-to-one spelling of a constant-expression tree, for mangling.
pub fn spell_const_expr(
    decls: &Declarations,
    sema: &Sema,
    id: ingot_sema::ty::ConstExprId,
) -> String {
    use ingot_sema::ty::ConstExpr;
    match decls.types.const_expr(id) {
        ConstExpr::Int { ty, value } => format!("i{}{}", ty.name(), value),
        ConstExpr::Bool(b) => format!("b{}", u8::from(*b)),
        ConstExpr::Param(p) => format!("g[{}]", item_path(sema, decls, *p)),
        ConstExpr::Unary { op, expr } => {
            format!("u{:?}({})", op, spell_const_expr(decls, sema, *expr))
        }
        ConstExpr::Binary { op, lhs, rhs } => format!(
            "o{:?}({},{})",
            op,
            spell_const_expr(decls, sema, *lhs),
            spell_const_expr(decls, sema, *rhs)
        ),
        ConstExpr::Cast { to, expr } => format!(
            "cast{}({})",
            to.name(),
            spell_const_expr(decls, sema, *expr)
        ),
        ConstExpr::Layout { size, ty } => format!(
            "{}({})",
            if *size { "sizeof" } else { "alignof" },
            spell_type(decls, sema, *ty)
        ),
    }
}

fn spell_arg(decls: &Declarations, sema: &Sema, arg: &GenericArg) -> String {
    match arg {
        GenericArg::Ty(t) => spell_type(decls, sema, *t),
        GenericArg::Const(ConstArg::Value(v)) => match v {
            ConstValue::Int { ty, value } => format!("c{}{}", ty.name(), value),
            ConstValue::Bool(b) => format!("cbool{}", u8::from(*b)),
        },
        GenericArg::Const(ConstArg::Param(p)) => format!("cp{}", sema.def(*p).name),
        GenericArg::Const(ConstArg::Expr(id)) => {
            format!("ce[{}]", spell_const_expr(decls, sema, *id))
        }
        GenericArg::Const(ConstArg::Poison) => "cp?".to_string(),
    }
}

fn spell_type(decls: &Declarations, sema: &Sema, ty: TypeId) -> String {
    match decls.types.kind(ty) {
        TyKind::Int(i) => i.name().to_string(),
        TyKind::Float(f) => f.name().to_string(),
        TyKind::Bool => "bool".to_string(),
        TyKind::Void => "void".to_string(),
        TyKind::Never => "never".to_string(),
        TyKind::Array(t, n) => format!(
            "a[{}][{}]",
            spell_const(decls, sema, n),
            spell_type(decls, sema, *t)
        ),
        TyKind::Tuple(ts) => {
            let mut out = format!("t{}", ts.len());
            for t in ts {
                out.push_str(&spell_type(decls, sema, *t));
            }
            out
        }
        TyKind::Adt(d, args) => format!(
            "{}[{}]",
            item_path(sema, decls, *d),
            spell_args(decls, sema, args)
        ),
        TyKind::Optional(t) => format!("o[{}]", spell_type(decls, sema, *t)),
        TyKind::Result(a, b) => format!(
            "r[{}][{}]",
            spell_type(decls, sema, *a),
            spell_type(decls, sema, *b)
        ),
        TyKind::Ref(m, t) => format!(
            "{}[{}]",
            if *m == ingot_sema::ty::Mutability::Mut {
                "bm"
            } else {
                "b"
            },
            spell_type(decls, sema, *t)
        ),
        TyKind::Slice(m, t) => format!(
            "{}[{}]",
            if *m == ingot_sema::ty::Mutability::Mut {
                "sm"
            } else {
                "s"
            },
            spell_type(decls, sema, *t)
        ),
        TyKind::RawPtr(m, t) => format!(
            "{}[{}]",
            if *m == ingot_sema::ty::Mutability::Mut {
                "pm"
            } else {
                "p"
            },
            spell_type(decls, sema, *t)
        ),
        TyKind::FnPtr(sig) => spell_sig("f", sig, decls, sema),
        TyKind::Dyn(_, d, args) => format!(
            "d[{}][{}]",
            item_path(sema, decls, *d),
            spell_args(decls, sema, args)
        ),
        TyKind::DynFn(m, k, sig) => format!(
            "{}[{:?}]{}",
            if *m == ingot_sema::ty::Mutability::Mut {
                "dfm"
            } else {
                "df"
            },
            k,
            spell_sig("", sig, decls, sema)
        ),
        TyKind::Vector(l, n) => format!("v[{}][{}]", l.name(), n),
        TyKind::Mask(n) => format!("m[{n}]"),
        TyKind::Param(p) => format!("g[{}]", item_path(sema, decls, *p)),
        TyKind::Projection(t, d) => {
            format!("x[{}][{}]", spell_type(decls, sema, *t), sema.def(*d).name)
        }
        TyKind::Closure(d, i, args) => format!(
            "c[{}][{}][{}]",
            item_path(sema, decls, *d),
            i,
            spell_args(decls, sema, args)
        ),
        TyKind::Poison => "?".to_string(),
    }
}

fn spell_sig(
    prefix: &str,
    sig: &ingot_sema::ty::FnPtrSig,
    decls: &Declarations,
    sema: &Sema,
) -> String {
    let mut out = format!("{}[{}]", prefix, if sig.ext { "x" } else { "c" });
    out.push_str(&format!("[{}]", sig.params.len()));
    for p in &sig.params {
        out.push('[');
        out.push_str(&spell_type(decls, sema, *p));
        out.push(']');
    }
    out.push('[');
    out.push_str(&spell_type(decls, sema, sig.ret));
    out.push(']');
    out.push('[');
    out.push_str(
        &sig.error
            .map(|e| spell_type(decls, sema, e))
            .unwrap_or_else(|| "n".to_string()),
    );
    out.push(']');
    out
}

fn spell_const(decls: &Declarations, sema: &Sema, c: &ConstArg) -> String {
    match c {
        ConstArg::Value(ConstValue::Int { ty, value }) => format!("{}{}", ty.name(), value),
        ConstArg::Value(ConstValue::Bool(b)) => format!("bool{}", u8::from(*b)),
        ConstArg::Param(p) => format!("g[{}]", item_path(sema, decls, *p)),
        ConstArg::Expr(id) => format!("e[{}]", spell_const_expr(decls, sema, *id)),
        ConstArg::Poison => "?".to_string(),
    }
}

fn subst_const(c: ConstArg, subst: &Subst) -> ConstArg {
    match c {
        ConstArg::Param(p) => match subst.get(p) {
            Some(GenericArg::Const(v)) => *v,
            _ => c,
        },
        _ => c,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ingot_sema::ty::{ConstValue, FnPtrSig, IntTy, Mutability, TyKind};
    use std::collections::BTreeSet;

    #[test]
    fn type_spellings_are_one_to_one() {
        let mut decls = Declarations::new();
        let sema = Sema::new();
        let i64_ = decls.types.int(IntTy::I64);
        let u8_ = decls.types.int(IntTy::U8);
        let f_i64 = decls.types.intern(TyKind::FnPtr(FnPtrSig {
            ext: false,
            params: vec![i64_],
            ret: i64_,
            error: None,
        }));
        let f_u8 = decls.types.intern(TyKind::FnPtr(FnPtrSig {
            ext: false,
            params: vec![u8_],
            ret: i64_,
            error: None,
        }));
        let f_ext = decls.types.intern(TyKind::FnPtr(FnPtrSig {
            ext: true,
            params: vec![i64_],
            ret: i64_,
            error: None,
        }));
        let r = decls.types.intern(TyKind::Ref(Mutability::Shared, i64_));
        let rm = decls.types.intern(TyKind::Ref(Mutability::Mut, i64_));
        let s = decls.types.intern(TyKind::Slice(Mutability::Shared, i64_));
        let sm = decls.types.intern(TyKind::Slice(Mutability::Mut, i64_));
        let t1 = decls.types.intern(TyKind::Tuple(vec![i64_, i64_]));
        let inner = decls.types.intern(TyKind::Tuple(vec![i64_, i64_]));
        let t2 = decls.types.intern(TyKind::Tuple(vec![i64_, inner]));
        let a64 = decls.types.intern(TyKind::Array(
            i64_,
            ConstArg::Value(ConstValue::Int {
                ty: IntTy::I64,
                value: 2,
            }),
        ));
        let a8 = decls.types.intern(TyKind::Array(
            u8_,
            ConstArg::Value(ConstValue::Int {
                ty: IntTy::U8,
                value: 2,
            }),
        ));
        let all = [i64_, u8_, f_i64, f_u8, f_ext, r, rm, s, sm, t1, t2, a64, a8];
        let names: BTreeSet<String> = all
            .iter()
            .map(|t| mangled_type(&decls, &sema, *t))
            .collect();
        assert_eq!(names.len(), all.len());
    }
}
