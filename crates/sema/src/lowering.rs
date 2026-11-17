//! Phase 6, first half: lower every HIR type to a `TypeId` and build the signature of
//! every item (pipeline section 1, phase 6).
//!
//! The work is demand-driven: asking for a signature builds it, and building one may
//! ask for another (an alias, a constant for an array length, the layout behind
//! `@sizeof`). A request that reaches an item that is still being built is a cycle.
//! Nothing here looks at a function body.

use crate::consteval::{Fail, ValTy};
use crate::defs::{Builtin, DefId, DefKind, Sema};
use crate::hir::{self, Res};
use crate::sigs::*;
use crate::target::Target;
use crate::ty::*;
use ingot_diag::diagnostic::{Diagnostic, Edit};
use ingot_diag::source::{Sources, Span};
use ingot_syntax::ast::StructKind;
use ingot_syntax::token::{ScalarTy, VectorTy};
use std::collections::BTreeSet;

/// Where the item being processed lives.
#[derive(Clone, Copy, Debug)]
pub struct Cur {
    /// Index of the package in the list of HIR packages.
    pub pkg: usize,
    pub file: usize,
    /// The impl or interface whose member is being lowered (or that is being lowered).
    pub impl_def: Option<DefId>,
}

/// The checker of phase 6: shared references to the resolved program and the
/// `Declarations` it is filling.
pub struct Cx<'a, 'i> {
    pub sema: &'a Sema,
    pub pkgs: &'a [hir::Package],
    pub sources: &'a Sources,
    pub target: &'a Target,
    pub d: Declarations,
    pub cur: Cur,
    /// The package whose unimplemented parts are reported (the one being checked).
    pub reported_pkg: usize,
    /// Greater than zero: unimplemented parts are not recorded.
    pub(crate) quiet: u32,
    busy: BTreeSet<DefId>,
    generics_busy: BTreeSet<DefId>,
    /// The factory of the indirect constant-evaluation path. `None` in sema's own
    /// unit tests; the direct path then records `constant evaluation of ...`.
    pub(crate) interp: Option<&'i dyn crate::interp::InterpreterFactory>,
    /// The bodies currently being typed, so a constant that calls one is E0812.
    pub(crate) typing: BTreeSet<DefId>,
    /// The call depth of the nested evaluation in progress and its outermost call.
    pub(crate) eval_depth: usize,
    pub(crate) eval_outer: Option<(usize, Span)>,
    /// The signatures being built, innermost last (a signature constant belongs to the
    /// innermost).
    pub(crate) sig_stack: Vec<DefId>,
    /// The body being type-checked, for a constant expression written in it.
    pub(crate) body_owner: Option<DefId>,
    /// The constants whose initializers the interpreter is evaluating, outermost
    /// first. A signature asked for while it is in progress is E0812 at the first.
    pub(crate) const_stack: Vec<DefId>,
    /// The impl tables are complete.
    pub(crate) impl_tables_ready: bool,
    /// The impl tables are being built: a constant they need must stay on the direct
    /// path (E0812 otherwise, pipeline section 4.6).
    pub(crate) impl_tables_building: bool,
}

/// A generic parameter as the lowering of arguments needs it.
#[derive(Clone, Copy)]
struct Desc {
    def: Option<DefId>,
    is_const: bool,
}

impl<'a, 'i> Cx<'a, 'i> {
    pub fn new(
        sema: &'a Sema,
        pkgs: &'a [hir::Package],
        sources: &'a Sources,
        target: &'a Target,
        reported_pkg: usize,
        interp: Option<&'i dyn crate::interp::InterpreterFactory>,
    ) -> Cx<'a, 'i> {
        Cx {
            sema,
            pkgs,
            sources,
            target,
            d: Declarations::new(),
            cur: Cur {
                pkg: 0,
                file: 0,
                impl_def: None,
            },
            reported_pkg,
            quiet: 0,
            busy: BTreeSet::new(),
            generics_busy: BTreeSet::new(),
            interp,
            typing: BTreeSet::new(),
            eval_depth: 0,
            eval_outer: None,
            sig_stack: Vec::new(),
            body_owner: None,
            const_stack: Vec::new(),
            impl_tables_ready: false,
            impl_tables_building: false,
        }
    }

    /// Continue with the `Declarations` phase 6 produced (phase 7 lowers the types
    /// written in bodies with the same checker, against the same type table).
    pub fn resume(
        d: Declarations,
        sema: &'a Sema,
        pkgs: &'a [hir::Package],
        sources: &'a Sources,
        target: &'a Target,
        reported_pkg: usize,
        interp: Option<&'i dyn crate::interp::InterpreterFactory>,
    ) -> Cx<'a, 'i> {
        let mut cx = Cx::new(sema, pkgs, sources, target, reported_pkg, interp);
        cx.d = d;
        // Phase 6 built the impl tables; phase 7 must not build them again.
        cx.impl_tables_ready = true;
        cx
    }

    // --------------------------------------------------------------- plumbing

    /// True while the signature of `def` is being built.
    pub(crate) fn is_busy(&self, def: DefId) -> bool {
        self.busy.contains(&def)
    }

    pub fn pkg_of(&self, def: DefId) -> usize {
        self.sema.module(self.sema.def(def).module).package.0 as usize
    }

    pub fn file_of(&self, def: DefId) -> usize {
        self.sema.def(def).file.unwrap_or(0)
    }

    pub fn hir_item(&self, def: DefId) -> Option<&'a hir::Item> {
        let pkgs: &'a [hir::Package] = self.pkgs;
        pkgs.get(self.pkg_of(def))?.items.get(&def)
    }

    pub fn error(
        &mut self,
        code: &'static str,
        message: impl Into<String>,
        file: usize,
        span: Span,
    ) -> &mut Diagnostic {
        let d = Diagnostic::new(code, message, self.sources.get(file), span);
        self.d.diagnostics.push(d);
        self.d
            .diagnostics
            .last_mut()
            .expect("a diagnostic was just pushed")
    }

    /// Report an error at a span of the file being processed.
    pub fn err(
        &mut self,
        code: &'static str,
        message: impl Into<String>,
        span: Span,
    ) -> &mut Diagnostic {
        let file = self.cur.file;
        self.error(code, message, file, span)
    }

    /// Record a part of the language that is not checked yet.
    pub fn ni(&mut self, what: impl Into<String>) {
        if self.quiet > 0 {
            return;
        }
        let what = what.into();
        if !self.d.not_implemented.contains(&what) {
            self.d.not_implemented.push(what);
        }
    }

    pub fn ty_name(&self, ty: TypeId) -> String {
        self.d.types.display(self.sema, ty)
    }

    pub(crate) fn enter(&mut self, def: DefId) -> Cur {
        let prev = self.cur;
        let parent = self.sema.def(def).parent;
        let container = match self.sema.def(def).kind {
            DefKind::Impl | DefKind::Interface => Some(def),
            _ => parent
                .filter(|p| matches!(self.sema.def(*p).kind, DefKind::Impl | DefKind::Interface)),
        };
        self.cur = Cur {
            pkg: self.pkg_of(def),
            file: self.file_of(def),
            impl_def: container,
        };
        prev
    }

    pub(crate) fn lower_ty_in(&mut self, pkg: usize, ty: &hir::Ty) -> TypeId {
        let prev = self.cur.pkg;
        self.cur.pkg = pkg;
        let t = self.lower_ty(ty);
        self.cur.pkg = prev;
        t
    }

    fn hir_generics(&self, def: DefId) -> &'a [hir::GenericParam] {
        match self.hir_item(def) {
            Some(hir::Item::Fn(f)) => &f.generics,
            Some(hir::Item::Struct(s)) => &s.generics,
            Some(hir::Item::Enum(e)) => &e.generics,
            Some(hir::Item::Interface(i)) => &i.generics,
            Some(hir::Item::Impl(i)) => &i.generics,
            Some(hir::Item::Alias(a)) => &a.generics,
            _ => &[],
        }
    }

    // --------------------------------------------------------------- drivers

    /// Build the signature of every item of every package, in the canonical order.
    pub fn lower_all(&mut self) {
        let pkgs: &'a [hir::Package] = self.pkgs;
        for package in pkgs {
            for module in &package.modules {
                for &def in &module.items {
                    self.ensure_item_tree(def);
                }
            }
        }
    }

    fn ensure_item_tree(&mut self, def: DefId) {
        self.ensure_sig(def);
        match self.hir_item(def) {
            Some(hir::Item::Impl(i)) => {
                for &m in &i.members {
                    self.ensure_sig(m);
                }
            }
            Some(hir::Item::Interface(i)) => {
                for &m in &i.members {
                    self.ensure_sig(m);
                }
            }
            _ => {}
        }
    }

    /// Lower the generic parameters of `owner` (their bounds and the types of constant
    /// parameters) once.
    pub fn ensure_generics(&mut self, owner: DefId) {
        if self.d.generics.contains_key(&owner) || self.generics_busy.contains(&owner) {
            return;
        }
        let hg = self.hir_generics(owner);
        if hg.is_empty() {
            self.d.generics.insert(owner, Vec::new());
            return;
        }
        self.generics_busy.insert(owner);
        let prev = self.enter(owner);
        // The parameters exist before their bounds are read, so a bound may mention
        // any of them.
        let mut sigs: Vec<GenericSig> = Vec::new();
        for g in hg {
            let const_ty = g.const_ty.as_ref().map(|t| self.lower_ty(t));
            if let Some(t) = const_ty {
                self.d.param_const_ty.insert(g.def, t);
            }
            self.d.param_bounds.insert(g.def, Vec::new());
            sigs.push(GenericSig {
                def: g.def,
                name: g.name.clone(),
                is_const: g.is_const,
                const_ty,
                bounds: Vec::new(),
                bound_spans: Vec::new(),
                span: g.span,
            });
        }
        self.d.generics.insert(owner, sigs.clone());
        for (i, g) in hg.iter().enumerate() {
            let mut bounds = Vec::new();
            let mut spans = Vec::new();
            for b in &g.bounds {
                bounds.push(self.lower_bound(b));
                spans.push(b.span);
            }
            self.d.param_bounds.insert(g.def, bounds.clone());
            sigs[i].bounds = bounds;
            sigs[i].bound_spans = spans;
        }
        self.d.generics.insert(owner, sigs);
        self.cur = prev;
        self.generics_busy.remove(&owner);
    }

    /// Build the signature of `def` if it has none yet.
    pub fn ensure_sig(&mut self, def: DefId) {
        if self.d.items.contains_key(&def) {
            return;
        }
        if self.busy.contains(&def) {
            // A signature that is asked for while it is being built, from inside a
            // constant evaluation, is a constant that depends on the signature that
            // asked for it (pipeline section 4.6, E0812).
            if let Some(&root) = self.const_stack.first()
                && !matches!(
                    self.d.values.get(&root),
                    Some(crate::consteval::ValueState::Done(_))
                )
            {
                let _ = self.cycle(root);
            }
            return;
        }
        let Some(item) = self.hir_item(def) else {
            return;
        };
        if matches!(item, hir::Item::Test(_)) {
            return;
        }
        if let Some(parent) = self.sema.def(def).parent
            && matches!(
                self.sema.def(parent).kind,
                DefKind::Impl | DefKind::Interface
            )
        {
            self.ensure_sig(parent);
        }
        self.ensure_generics(def);
        self.busy.insert(def);
        self.sig_stack.push(def);
        let prev = self.enter(def);
        let sig = match item {
            hir::Item::Fn(f) => Some(ItemSig::Fn(self.build_fn(f))),
            hir::Item::Struct(s) => Some(ItemSig::Struct(self.build_struct(s))),
            hir::Item::Enum(e) => Some(ItemSig::Enum(self.build_enum(e))),
            hir::Item::Interface(i) => Some(ItemSig::Interface(self.build_interface(i))),
            hir::Item::Impl(i) => Some(ItemSig::Impl(self.build_impl(i))),
            hir::Item::Const(c) => Some(ItemSig::Const(ConstSig {
                def,
                parent: self.sema.def(def).parent,
                ty: self.lower_ty(&c.ty),
                span: c.span,
            })),
            hir::Item::Static(s) => Some(ItemSig::Static(self.build_static(s))),
            hir::Item::Alias(a) => Some(ItemSig::Alias(self.build_alias(a))),
            hir::Item::Test(_) => None,
        };
        self.cur = prev;
        self.sig_stack.pop();
        self.busy.remove(&def);
        if let Some(sig) = sig {
            self.d.items.insert(def, sig);
        }
    }

    // ------------------------------------------------------------ item sigs

    fn build_fn(&mut self, f: &hir::FnItem) -> FnSig {
        let params = f
            .params
            .iter()
            .map(|p| ParamSig {
                name: p.name.clone(),
                name_span: p.name_span,
                ty: self.lower_ty(&p.ty),
                ty_span: p.ty.span,
                into: p.into.as_ref().map(|(_, span, idx)| (*span, *idx)),
            })
            .collect();
        let ret = self.lower_ty(&f.ret);
        let error = f.error.as_ref().map(|t| self.lower_ty(t));
        let error_span = f.error.as_ref().map(|t| t.span);
        FnSig {
            def: f.def,
            parent: self
                .sema
                .def(f.def)
                .parent
                .filter(|p| matches!(self.sema.def(*p).kind, DefKind::Impl | DefKind::Interface)),
            name: f.name.clone(),
            name_span: f.name_span,
            mods: f.mods,
            params,
            ret,
            ret_span: f.ret.span,
            error,
            error_span,
            from: f
                .from
                .iter()
                .map(|o| OriginSig {
                    name: o.name.clone(),
                    param: o.param,
                    inner: o.inner,
                    is_static: o.is_static,
                    span: o.span,
                })
                .collect(),
            reduce: f.reduce.clone(),
            reduce_span: f.reduce_span,
            variadic: f.variadic,
            abi: f.abi.clone(),
            has_body: f.body.is_some(),
            span: f.span,
        }
    }

    fn build_struct(&mut self, s: &hir::StructItem) -> StructSig {
        let fields = s
            .fields
            .iter()
            .map(|f| FieldSig {
                def: f.def,
                name: f.name.clone(),
                name_span: f.name_span,
                public: f.public,
                pkg: f.pkg,
                ty: self.lower_ty(&f.ty),
                ty_span: f.ty.span,
                align: f.align.and_then(|e| self.align_attr(e)),
            })
            .collect();
        StructSig {
            def: s.def,
            kind: s.kind,
            fields,
            align: s.align.and_then(|e| self.align_attr(e)),
            span: s.span,
        }
    }

    /// The value of an `align(...)` argument. Whether it is a valid alignment is
    /// checked with the layouts.
    fn align_attr(&mut self, e: hir::ExprId) -> Option<AlignAttr> {
        let span = self.pkgs[self.cur.pkg].expr(e).span;
        match self.eval_signature_const(e, ValTy::Int(IntTy::I64)) {
            Ok(ConstValue::Int { value, .. }) => Some(AlignAttr {
                value,
                span,
                raw: None,
            }),
            Ok(_) => None,
            Err(Fail::Generic) => {
                let owner = self.sig_stack.last().copied().or(self.body_owner)?;
                Some(AlignAttr {
                    value: 0,
                    span,
                    raw: Some((owner, e)),
                })
            }
            Err(Fail::OutOfRange(_)) => {
                self.note_failure(&Fail::OutOfRange(span));
                None
            }
            Err(f) => {
                self.note_failure(&f);
                None
            }
        }
    }

    fn build_enum(&mut self, e: &hir::EnumItem) -> EnumSig {
        let mut declared: Option<IntTy> = None;
        if let Some(t) = &e.tag {
            let ty = self.lower_ty(t);
            match self.d.types.kind(ty) {
                TyKind::Int(i) => declared = Some(*i),
                TyKind::Poison => {}
                _ => {
                    let name = self.ty_name(ty);
                    self.err(
                        "E0522",
                        format!("the tag type of an enum is an integer type, not `{name}`"),
                        t.span,
                    );
                }
            }
        }
        let n = e.variants.len();
        let tag_ty = declared.unwrap_or(if n <= 1 << 8 {
            IntTy::U8
        } else if n <= 1 << 16 {
            IntTy::U16
        } else if n <= 1 << 32 {
            IntTy::U32
        } else {
            IntTy::U64
        });
        let tag_declared = e.tag.is_some();
        let mut variants: Vec<VariantSig> = Vec::new();
        let mut next: i128 = 0;
        let mut used: Vec<(i128, Span)> = Vec::new();
        for v in &e.variants {
            let payload = v.payload.as_ref().map(|t| self.lower_ty(t));
            let mut value = next;
            let mut at = v.name_span;
            if let Some(expr) = v.value {
                at = self.pkgs[self.cur.pkg].expr(expr).span;
                if !tag_declared {
                    self.err(
                        "E0505",
                        "an explicit tag value needs a tag type: write `enum Name: u8 { ... }`",
                        at,
                    );
                } else if declared.is_some() {
                    match self.eval_signature_const(expr, ValTy::Int(tag_ty)) {
                        Ok(ConstValue::Int { value: x, .. }) => value = x,
                        Ok(_) => {}
                        Err(Fail::OutOfRange(sp)) => {
                            self.err(
                                "E0505",
                                format!("the tag value does not fit `{}`", tag_ty.name()),
                                sp,
                            );
                        }
                        Err(f) => self.note_failure(&f),
                    }
                }
            }
            if tag_declared && declared.is_some() && !tag_ty.contains(value) {
                self.err(
                    "E0505",
                    format!("the tag value {value} does not fit `{}`", tag_ty.name()),
                    at,
                );
                value = next;
            } else if let Some(&(_, prev)) = used.iter().find(|(x, _)| *x == value) {
                let path = self.sources.get(self.cur.file).path.clone();
                self.err(
                    "E0505",
                    format!("two variants have the tag value {value}"),
                    at,
                )
                .relate_in("previous", &path, prev);
            }
            used.push((value, at));
            next = value.saturating_add(1);
            variants.push(VariantSig {
                def: v.def,
                name: v.name.clone(),
                name_span: v.name_span,
                payload,
                payload_span: v.payload.as_ref().map(|t| t.span),
                from: v.from,
                tag: value,
                span: v.span,
            });
        }
        EnumSig {
            def: e.def,
            copy: e.copy,
            tag_ty,
            tag_declared,
            variants,
            span: e.span,
        }
    }

    fn build_interface(&mut self, i: &hir::InterfaceItem) -> InterfaceSig {
        let self_ty = self.d.types.param(i.self_def);
        self.d.self_types.insert(i.self_def, self_ty);
        let supers: Vec<Bound> = i.bounds.iter().map(|b| self.lower_bound(b)).collect();
        // `Self` is bounded by the interface itself and by its supertraits.
        let generics = self.d.generics.get(&i.def).cloned().unwrap_or_default();
        let own = Bound::Interface(
            i.def,
            generics
                .iter()
                .map(|g| {
                    if g.is_const {
                        GenericArg::Const(ConstArg::Param(g.def))
                    } else {
                        GenericArg::Ty(self.d.types.param(g.def))
                    }
                })
                .collect(),
        );
        let mut self_bounds = vec![own];
        self_bounds.extend(supers.iter().cloned());
        self.d.param_bounds.insert(i.self_def, self_bounds);
        let mut methods = Vec::new();
        let mut assoc = Vec::new();
        for &m in &i.members {
            match self.hir_item(m) {
                Some(hir::Item::Fn(_)) => methods.push(m),
                Some(hir::Item::Alias(_)) => {
                    assoc.push(m);
                    let name = self.sema.def(m).name.clone();
                    self.d.assoc_info.insert(m, (i.def, name));
                }
                _ => {}
            }
        }
        InterfaceSig {
            def: i.def,
            supers,
            self_def: i.self_def,
            methods,
            assoc,
            span: i.span,
        }
    }

    fn build_impl(&mut self, i: &hir::ImplItem) -> ImplSig {
        let target = self.lower_ty(&i.target);
        self.d.self_types.insert(i.self_def, target);
        let mut poisoned = self.d.types.has_poison(target);
        let of = match &i.interface {
            None => ImplOf::Inherent,
            Some(p) => match self.lower_impl_interface(p) {
                Some(of) => of,
                None => {
                    poisoned = true;
                    ImplOf::Inherent
                }
            },
        };
        let mut methods = std::collections::BTreeMap::new();
        let mut assoc = std::collections::BTreeMap::new();
        let mut consts = std::collections::BTreeMap::new();
        for &m in &i.members {
            let name = self.sema.def(m).name.clone();
            match self.hir_item(m) {
                Some(hir::Item::Fn(_)) => {
                    methods.insert(name, m);
                }
                Some(hir::Item::Alias(_)) => {
                    assoc.insert(name, m);
                }
                Some(hir::Item::Const(_)) => {
                    consts.insert(name, m);
                }
                _ => {}
            }
        }
        ImplSig {
            def: i.def,
            unsafe_: i.unsafe_,
            of,
            target,
            self_def: i.self_def,
            methods,
            assoc,
            consts,
            poisoned,
            span: i.span,
            header: Span::new(i.span.start, i.target.span.end.max(i.span.start)),
        }
    }

    /// What `impl <path> for T` implements, or `None` after reporting.
    fn lower_impl_interface(&mut self, p: &hir::PathRef) -> Option<ImplOf> {
        let Res::Def(def) = p.res else {
            return None;
        };
        let name = self.sema.def(def).name.clone();
        match self.sema.def(def).kind {
            DefKind::Builtin(b @ (Builtin::Send | Builtin::Sync)) => {
                if !p.args.is_empty() {
                    self.not_generic(p);
                    return None;
                }
                Some(ImplOf::Prop(
                    PropBound::from_builtin(b).expect("Send and Sync are property bounds"),
                ))
            }
            DefKind::Builtin(Builtin::Executor) => {
                if !p.args.is_empty() {
                    self.not_generic(p);
                    return None;
                }
                if self.pkgs[self.cur.pkg].name != "std" {
                    self.err(
                        "E0525",
                        "`Executor` is implemented only by the executors `std.exec` and \
                         `std.gpu` declare",
                        p.span,
                    );
                    return None;
                }
                Some(ImplOf::Interface(def, Vec::new()))
            }
            DefKind::Builtin(Builtin::Fn | Builtin::FnMut | Builtin::FnOnce) => {
                if p.fn_sig.is_some() {
                    self.err(
                        "E0525",
                        format!("`{name}` is a property of a function value, not an interface"),
                        p.span,
                    );
                } else {
                    self.err(
                        "E0523",
                        format!(
                            "`{name}` is only a bound with a signature, as in `{name}(A) -> R`"
                        ),
                        p.span,
                    );
                }
                None
            }
            DefKind::Builtin(
                Builtin::Copy
                | Builtin::Discard
                | Builtin::Owned
                | Builtin::Int
                | Builtin::SInt
                | Builtin::UInt
                | Builtin::Float
                | Builtin::Num,
            ) => {
                self.err(
                    "E0525",
                    format!("`{name}` is a property of a type, which the compiler computes, not an interface"),
                    p.span,
                );
                None
            }
            DefKind::Interface => {
                let descs = self.generic_descs(def);
                let args = self.lower_args(p, def, &descs)?;
                Some(ImplOf::Interface(def, args))
            }
            DefKind::Poison => None,
            _ => {
                self.err(
                    "E0523",
                    format!("`{name}` is not an interface, so nothing can implement it"),
                    p.span,
                );
                None
            }
        }
    }

    fn build_static(&mut self, s: &hir::StaticItem) -> StaticSig {
        StaticSig {
            def: s.def,
            ty: self.lower_ty(&s.ty),
            ty_span: s.ty.span,
            align: s.align.and_then(|e| self.align_attr(e)),
            span: s.span,
        }
    }

    fn build_alias(&mut self, a: &hir::AliasItem) -> AliasSig {
        let ty = a.ty.as_ref().map(|t| self.lower_ty(t));
        AliasSig {
            def: a.def,
            parent: self.sema.def(a.def).parent,
            ty,
            span: a.span,
        }
    }

    // ----------------------------------------------------------------- bounds

    /// Lower a bound written in a generic parameter list or after an interface name.
    pub fn lower_bound(&mut self, p: &hir::PathRef) -> Bound {
        let Res::Def(def) = p.res else {
            return Bound::Poison;
        };
        match self.sema.def(def).kind {
            DefKind::Builtin(b) => {
                if let Some(pb) = PropBound::from_builtin(b) {
                    if !p.args.is_empty() || p.fn_sig.is_some() {
                        self.not_generic(p);
                        return Bound::Poison;
                    }
                    return Bound::Prop(pb);
                }
                match b {
                    Builtin::Fn | Builtin::FnMut | Builtin::FnOnce => {
                        let kind = match b {
                            Builtin::Fn => FnKind::Fn,
                            Builtin::FnMut => FnKind::FnMut,
                            _ => FnKind::FnOnce,
                        };
                        match &p.fn_sig {
                            Some(sig) => Bound::Fn(kind, self.lower_fn_bound_sig(sig)),
                            None => {
                                self.fn_without_signature(p, kind);
                                Bound::Poison
                            }
                        }
                    }
                    Builtin::Executor => {
                        if !p.args.is_empty() {
                            self.not_generic(p);
                            return Bound::Poison;
                        }
                        Bound::Interface(def, Vec::new())
                    }
                    _ => {
                        self.not_a_bound(p, def);
                        Bound::Poison
                    }
                }
            }
            DefKind::Interface => {
                let descs = self.generic_descs(def);
                match self.lower_args(p, def, &descs) {
                    Some(args) => Bound::Interface(def, args),
                    None => Bound::Poison,
                }
            }
            DefKind::Poison => Bound::Poison,
            _ => {
                self.not_a_bound(p, def);
                Bound::Poison
            }
        }
    }

    /// E0523: a bound that names something that is not an interface or a built-in bound.
    fn not_a_bound(&mut self, p: &hir::PathRef, def: DefId) {
        let name = self.sema.def(def).name.clone();
        self.err(
            "E0523",
            format!("`{name}` is neither an interface nor a built-in bound"),
            p.span,
        );
    }

    /// E0523: `dyn` of something that is not an interface.
    fn not_an_interface(&mut self, p: &hir::PathRef, def: DefId) {
        let name = self.sema.def(def).name.clone();
        self.err(
            "E0523",
            format!("`dyn` needs an interface, and `{name}` is not one"),
            p.span,
        );
    }

    /// E0523: `Fn`, `FnMut` or `FnOnce` without a signature.
    fn fn_without_signature(&mut self, p: &hir::PathRef, kind: FnKind) {
        let name = kind.name();
        self.err(
            "E0523",
            format!("`{name}` is a bound only with a signature, as in `{name}(A) -> R`"),
            p.span,
        );
    }

    fn lower_fn_bound_sig(&mut self, sig: &hir::FnSig) -> FnPtrSig {
        FnPtrSig {
            ext: false,
            params: sig.params.iter().map(|t| self.lower_ty(t)).collect(),
            ret: match &sig.ret {
                Some(t) => self.lower_ty(t),
                None => self.d.types.void,
            },
            error: sig.error.as_ref().map(|t| self.lower_ty(t)),
        }
    }

    // ------------------------------------------------------------------ types

    pub fn lower_ty(&mut self, ty: &hir::Ty) -> TypeId {
        let lowered = match &ty.kind {
            hir::TyKind::Path(p) => self.lower_path_ty(p),
            hir::TyKind::Borrow { mutable, inner } => {
                let t = self.lower_ty(inner);
                self.d.types.intern(TyKind::Ref(mutability(*mutable), t))
            }
            hir::TyKind::Slice { mutable, elem } => {
                let t = self.lower_ty(elem);
                self.d.types.intern(TyKind::Slice(mutability(*mutable), t))
            }
            hir::TyKind::Ptr { mutable, inner } => {
                let t = self.lower_ty(inner);
                self.d.types.intern(TyKind::RawPtr(mutability(*mutable), t))
            }
            hir::TyKind::Optional(inner) => {
                let t = self.lower_ty(inner);
                self.d.types.intern(TyKind::Optional(t))
            }
            hir::TyKind::Tuple(elems) => {
                let ts = elems.iter().map(|t| self.lower_ty(t)).collect();
                self.d.types.intern(TyKind::Tuple(ts))
            }
            hir::TyKind::Array { len, elem } => {
                let t = self.lower_ty(elem);
                let n = self.array_len(*len);
                self.d.types.intern(TyKind::Array(t, n))
            }
            hir::TyKind::Fn {
                ext,
                params,
                ret,
                error,
            } => {
                let params = params.iter().map(|t| self.lower_ty(t)).collect();
                let ret = self.lower_ty(ret);
                let error = error.as_ref().map(|t| self.lower_ty(t));
                self.d.types.intern(TyKind::FnPtr(FnPtrSig {
                    ext: *ext,
                    params,
                    ret,
                    error,
                }))
            }
            hir::TyKind::Dyn { interface, mutable } => self.lower_dyn(interface, *mutable, ty.span),
            hir::TyKind::Poison => self.d.types.poison,
        };
        // 01-core §3: a length or size that depends on a constant generic argument is
        // checked where the type is concrete. The check runs after every signature
        // exists (check_obligations) for a concrete type, and per instance in phase 11a
        // for one that still mentions a parameter.
        if !self.d.types.has_poison(lowered)
            && matches!(
                self.d.types.kind(lowered),
                TyKind::Array(..) | TyKind::Adt(..)
            )
        {
            let length_span = match &ty.kind {
                hir::TyKind::Array { len, .. } => Some(self.pkgs[self.cur.pkg].expr(*len).span),
                _ => self.invalid_argument_span(ty, lowered),
            };
            let c = crate::sigs::ConcreteType {
                file: self.cur.file,
                span: ty.span,
                owner: self.body_owner.or(self.sig_stack.last().copied()),
                length_span,
                ty: lowered,
            };
            if self.d.types.has_params(lowered) {
                self.d.generic_types.push(c);
            } else {
                self.d.concrete_types.push(c);
            }
        }
        lowered
    }

    /// The span of a constant generic argument whose value is not a valid length. It
    /// is the span E0521 uses when that argument made the array length invalid.
    fn invalid_argument_span(&mut self, ty: &hir::Ty, lowered: TypeId) -> Option<Span> {
        let hir::TyKind::Path(path) = &ty.kind else {
            return None;
        };
        let TyKind::Adt(_, args) = self.d.types.kind(lowered).clone() else {
            return None;
        };
        for (arg, lowered) in path.args.iter().zip(&args) {
            let GenericArg::Const(ConstArg::Value(ConstValue::Int { value, .. })) = lowered else {
                continue;
            };
            if (0..=(1 << 62)).contains(value) {
                continue;
            }
            return Some(match arg {
                hir::GenericArg::Ty(t) => t.span,
                hir::GenericArg::Const(e) => self.pkgs[self.cur.pkg].expr(*e).span,
            });
        }
        None
    }

    /// The length of an array type: a constant generic parameter or a value.
    fn array_len(&mut self, e: hir::ExprId) -> ConstArg {
        let arg = self.const_arg(e, ValTy::Int(IntTy::I64));
        if let ConstArg::Value(ConstValue::Int { value, .. }) = arg {
            let span = self.pkgs[self.cur.pkg].expr(e).span;
            if value < 0 {
                self.err(
                    "E0521",
                    format!("an array cannot have the negative length {value}"),
                    span,
                );
                return ConstArg::Poison;
            }
            if value > 1 << 62 {
                self.err(
                    "E0521",
                    format!("an array length is at most 2^62, not {value}"),
                    span,
                );
                return ConstArg::Poison;
            }
        }
        arg
    }

    fn scalar_type(&mut self, s: ScalarTy) -> TypeId {
        let kind = match s {
            ScalarTy::F32 => TyKind::Float(FloatTy::F32),
            ScalarTy::F64 => TyKind::Float(FloatTy::F64),
            ScalarTy::Bool => TyKind::Bool,
            ScalarTy::Void => TyKind::Void,
            ScalarTy::Never => TyKind::Never,
            other => TyKind::Int(IntTy::from_scalar(other).expect("an integer scalar")),
        };
        self.d.types.intern(kind)
    }

    /// E1501 unless the vector has a width the specification allows.
    fn vector_type(&mut self, v: VectorTy, span: Span) -> TypeId {
        if v.elem == ScalarTy::Bool {
            if ![2, 4, 8, 16, 32, 64].contains(&v.lanes) {
                self.err(
                    "E1501",
                    format!(
                        "`{}` is not a mask type: a mask has 2, 4, 8, 16, 32 or 64 lanes",
                        v.name()
                    ),
                    span,
                );
                return self.d.types.poison;
            }
            return self.d.types.intern(TyKind::Mask(u32::from(v.lanes)));
        }
        let lane = lane_of(v.elem).expect("a lane type");
        let bits = lane.bits() * u32::from(v.lanes);
        if ![128, 256, 512].contains(&bits) {
            self.err(
                "E1501",
                format!(
                    "`{}` is {bits} bits wide: a vector is 128, 256 or 512 bits",
                    v.name()
                ),
                span,
            );
            return self.d.types.poison;
        }
        self.d
            .types
            .intern(TyKind::Vector(lane, u32::from(v.lanes)))
    }

    fn generic_descs(&self, def: DefId) -> Vec<Desc> {
        match self.sema.def(def).kind {
            DefKind::Builtin(Builtin::Option | Builtin::Grid | Builtin::GridMut) => vec![Desc {
                def: None,
                is_const: false,
            }],
            DefKind::Builtin(Builtin::Result) => vec![
                Desc {
                    def: None,
                    is_const: false
                };
                2
            ],
            DefKind::Builtin(_) => Vec::new(),
            _ => self
                .hir_generics(def)
                .iter()
                .map(|g| Desc {
                    def: Some(g.def),
                    is_const: g.is_const,
                })
                .collect(),
        }
    }

    fn not_generic(&mut self, p: &hir::PathRef) {
        let name = p
            .segments
            .last()
            .map(|(n, _)| n.clone())
            .unwrap_or_default();
        let span = p.args_span.unwrap_or(p.span);
        let file = self.cur.file;
        let path = self.sources.get(file).path.clone();
        let d = self.err(
            "E0805",
            format!("`{name}` takes no generic arguments"),
            span,
        );
        if p.args_span.is_some() {
            d.fix(
                "delete the generic arguments",
                Edit {
                    file: path,
                    span,
                    text: String::new(),
                },
            );
        }
    }

    /// Lower the generic arguments written on `p` for the item `item` with
    /// parameters `descs`: E0804 or E0805 on a wrong count. Also records the bound
    /// check (E0801) of the arguments.
    fn lower_args(
        &mut self,
        p: &hir::PathRef,
        item: DefId,
        descs: &[Desc],
    ) -> Option<Vec<GenericArg>> {
        let name = p
            .segments
            .last()
            .map(|(n, _)| n.clone())
            .unwrap_or_default();
        if descs.is_empty() {
            if !p.args.is_empty() {
                self.not_generic(p);
                return None;
            }
            return Some(Vec::new());
        }
        if p.args.len() != descs.len() {
            let span = p.args_span.unwrap_or(p.span);
            self.err(
                "E0804",
                format!(
                    "`{name}` takes {} generic argument(s) but {} {} given",
                    descs.len(),
                    p.args.len(),
                    if p.args.len() == 1 { "was" } else { "were" }
                ),
                span,
            );
            return None;
        }
        let mut out = Vec::new();
        let mut spans = Vec::new();
        let mut ok = true;
        for (arg, desc) in p.args.iter().zip(descs) {
            let (lowered, span) = self.lower_generic_arg(arg, *desc);
            spans.push(span);
            match lowered {
                Some(g) => out.push(g),
                None => ok = false,
            }
        }
        if !ok {
            return None;
        }
        if matches!(
            self.sema.def(item).kind,
            DefKind::Struct | DefKind::Enum | DefKind::TypeAlias | DefKind::Interface
        ) {
            let file = self.cur.file;
            self.d.obligations.push(Obligation {
                file,
                span: p.span,
                kind: ObligationKind::Args {
                    item,
                    args: out.clone(),
                    spans,
                },
            });
        }
        Some(out)
    }

    fn lower_generic_arg(
        &mut self,
        arg: &hir::GenericArg,
        desc: Desc,
    ) -> (Option<GenericArg>, Span) {
        match arg {
            hir::GenericArg::Ty(t) => {
                if !desc.is_const {
                    // `Ring[N, u8]` writes the constant parameter `N` where a type is due.
                    if let hir::TyKind::Path(p) = &t.kind
                        && let Res::Def(d) = p.res
                        && self.sema.def(d).kind == (DefKind::GenericParam { is_const: true })
                    {
                        self.err(
                            "E0813",
                            "this generic parameter is a type: give it a type, not a constant",
                            t.span,
                        );
                        return (None, t.span);
                    }
                    return (Some(GenericArg::Ty(self.lower_ty(t))), t.span);
                }
                // `Ring[T, N]` writes the constant parameter `N` as a type name.
                if let hir::TyKind::Path(p) = &t.kind
                    && let Res::Def(d) = p.res
                    && self.sema.def(d).kind == (DefKind::GenericParam { is_const: true })
                    && p.args.is_empty()
                {
                    return (Some(GenericArg::Const(ConstArg::Param(d))), t.span);
                }
                if let hir::TyKind::Poison = t.kind {
                    return (None, t.span);
                }
                if let hir::TyKind::Path(p) = &t.kind
                    && matches!(p.res, Res::Poison)
                {
                    return (None, t.span);
                }
                self.err(
                    "E0813",
                    "this generic parameter is a constant: give it a value, not a type",
                    t.span,
                );
                (None, t.span)
            }
            hir::GenericArg::Const(e) => {
                let span = self.pkgs[self.cur.pkg].expr(*e).span;
                if desc.is_const {
                    let hint = desc
                        .def
                        .and_then(|d| self.d.param_const_ty.get(&d).copied())
                        .and_then(|t| match self.d.types.kind(t) {
                            TyKind::Int(i) => Some(ValTy::Int(*i)),
                            TyKind::Bool => Some(ValTy::Bool),
                            _ => None,
                        });
                    let Some(hint) = hint else {
                        self.ni(
                            "a constant generic argument for a parameter whose type is not an \
                             integer or bool",
                        );
                        return (None, span);
                    };
                    let c = self.const_arg(*e, hint);
                    (Some(GenericArg::Const(c)), span)
                } else {
                    self.err(
                        "E0813",
                        "this generic parameter is a type: give it a type, not a constant",
                        span,
                    );
                    (None, span)
                }
            }
        }
    }

    /// The type a path names, with its generic arguments applied.
    fn lower_path_ty(&mut self, p: &hir::PathRef) -> TypeId {
        let poison = self.d.types.poison;
        let Res::Def(def) = p.res else {
            return poison;
        };
        let kind = self.sema.def(def).kind;
        let base = match kind {
            DefKind::Builtin(Builtin::Scalar(s)) => {
                if !p.args.is_empty() {
                    self.not_generic(p);
                    return poison;
                }
                self.scalar_type(s)
            }
            DefKind::Builtin(Builtin::Vector(v)) => {
                if !p.args.is_empty() {
                    self.not_generic(p);
                    return poison;
                }
                self.vector_type(v, p.span)
            }
            DefKind::Builtin(Builtin::Option) => {
                let descs = self.generic_descs(def);
                match self.lower_args(p, def, &descs).as_deref() {
                    Some([GenericArg::Ty(t)]) => self.d.types.intern(TyKind::Optional(*t)),
                    _ => poison,
                }
            }
            DefKind::Builtin(Builtin::Result) => {
                let descs = self.generic_descs(def);
                match self.lower_args(p, def, &descs).as_deref() {
                    Some([GenericArg::Ty(t), GenericArg::Ty(e)]) => {
                        self.d.types.intern(TyKind::Result(*t, *e))
                    }
                    _ => poison,
                }
            }
            DefKind::Builtin(Builtin::Idx) => {
                if !p.args.is_empty() {
                    self.not_generic(p);
                    return poison;
                }
                self.d.types.intern(TyKind::Adt(def, Vec::new()))
            }
            DefKind::Builtin(Builtin::Idx2 | Builtin::Idx3) => {
                if !p.args.is_empty() {
                    self.not_generic(p);
                }
                self.ni("grid kernels (`Idx2` and `Idx3`) are reserved for a later step");
                poison
            }
            DefKind::Builtin(Builtin::Grid | Builtin::GridMut) => {
                let descs = self.generic_descs(def);
                let _ = self.lower_args(p, def, &descs);
                self.ni("grid kernels (`Grid` and `GridMut`) are reserved for a later step");
                poison
            }
            DefKind::Builtin(_) | DefKind::Interface => {
                let name = self.sema.def(def).name.clone();
                self.err(
                    "E0524",
                    format!("`{name}` is an interface or a bound, not a type"),
                    p.span,
                );
                poison
            }
            DefKind::Struct | DefKind::Enum => {
                let descs = self.generic_descs(def);
                match self.lower_args(p, def, &descs) {
                    Some(args) => self.d.types.intern(TyKind::Adt(def, args)),
                    None => poison,
                }
            }
            DefKind::TypeAlias => {
                let descs = self.generic_descs(def);
                match self.lower_args(p, def, &descs) {
                    Some(args) => self.alias_expansion(def, &args),
                    None => poison,
                }
            }
            DefKind::GenericParam { is_const } => {
                if !p.args.is_empty() {
                    self.not_generic(p);
                    return poison;
                }
                if is_const {
                    self.ni(
                        "a constant generic parameter used as a type; the specification \
                         defines no error for it",
                    );
                    return poison;
                }
                self.d.types.param(def)
            }
            DefKind::SelfTy => {
                if !p.args.is_empty() {
                    self.not_generic(p);
                    return poison;
                }
                self.d.self_types.get(&def).copied().unwrap_or(poison)
            }
            DefKind::Poison => poison,
            _ => {
                self.ni(format!("`{}` used as a type", self.sema.def(def).name));
                poison
            }
        };
        match &p.assoc {
            Some((name, span)) => self.projection(p, def, base, name, *span),
            None => base,
        }
    }

    /// The type an alias stands for, with `args` for its parameters.
    pub fn alias_expansion(&mut self, def: DefId, args: &[GenericArg]) -> TypeId {
        let poison = self.d.types.poison;
        if self.busy.contains(&def) {
            let name = self.sema.def(def).name.clone();
            let (file, span) = (self.file_of(def), self.sema.def(def).span);
            self.error(
                "E0520",
                format!("the type alias `{name}` expands to a type that contains itself"),
                file,
                span,
            );
            return poison;
        }
        self.ensure_sig(def);
        let Some(ItemSig::Alias(a)) = self.d.items.get(&def) else {
            return poison;
        };
        let Some(ty) = a.ty else {
            return poison;
        };
        let s = self.d.subst_for(def, args);
        self.d.types.subst(ty, &s)
    }

    /// `T.Item`, `Self.Item`.
    fn projection(
        &mut self,
        p: &hir::PathRef,
        base_def: DefId,
        base: TypeId,
        name: &str,
        span: Span,
    ) -> TypeId {
        let poison = self.d.types.poison;
        if self.d.types.has_poison(base) {
            return poison;
        }
        // `Self.Item` in an impl: the impl's own `type Item = ...`.
        if self.sema.def(base_def).kind == DefKind::SelfTy
            && let Some(owner) = self.sema.def(base_def).parent
            && self.sema.def(owner).kind == DefKind::Impl
        {
            self.ensure_sig(owner);
            let Some(ItemSig::Impl(imp)) = self.d.items.get(&owner) else {
                return poison;
            };
            if let Some(&alias) = imp.assoc.get(name) {
                self.ensure_sig(alias);
                return match self.d.items.get(&alias) {
                    Some(ItemSig::Alias(a)) => a.ty.unwrap_or(poison),
                    _ => poison,
                };
            }
            // The interface declares it but the impl omits it: reported at the impl.
            let of = imp.of.clone();
            if let ImplOf::Interface(i, _) = of
                && self.interface_declares(i, name).is_some()
            {
                return poison;
            }
            self.bad_assoc(name, span);
            return poison;
        }
        // `T.Item` for a type parameter, or `Self.Item` in an interface: found through
        // the bounds.
        if let TyKind::Param(param) = self.d.types.kind(base).clone() {
            let owner = self.sema.def(param).parent;
            if let Some(o) = owner {
                self.ensure_generics(o);
                if self.sema.def(o).kind == DefKind::Interface {
                    self.ensure_sig(o);
                }
            }
            let bounds = self.d.param_bounds.get(&param).cloned().unwrap_or_default();
            let mut found = Vec::new();
            for b in &bounds {
                if let Bound::Interface(i, _) = b {
                    // `Executor` is a bound without a declaration and declares nothing.
                    if let Some(a) = self.interface_declares(*i, name)
                        && !found.contains(&a)
                    {
                        found.push(a);
                    }
                }
            }
            return match found.as_slice() {
                [a] => self.d.types.intern(TyKind::Projection(base, *a)),
                [] => {
                    self.bad_assoc(name, span);
                    poison
                }
                _ => {
                    self.ni(format!(
                        "an associated type `{name}` declared by more than one bound of the \
                         same parameter"
                    ));
                    poison
                }
            };
        }
        let _ = p;
        self.ni(format!(
            "the associated type `{name}` of a type that is not a type parameter or `Self`"
        ));
        poison
    }

    /// E0519: a path names an associated type that no interface in reach declares.
    fn bad_assoc(&mut self, name: &str, span: Span) {
        self.err(
            "E0519",
            format!("no interface in scope declares an associated type `{name}`"),
            span,
        );
    }

    /// The associated type `name` declared by interface `i` or one of its supertraits.
    pub fn interface_declares(&mut self, i: DefId, name: &str) -> Option<DefId> {
        self.ensure_sig(i);
        let sig = self.d.interface_sig(i)?.clone();
        for &a in &sig.assoc {
            if self.sema.def(a).name == name {
                return Some(a);
            }
        }
        for s in &sig.supers {
            if let Bound::Interface(j, _) = s
                && *j != i
                && let Some(a) = self.interface_declares(*j, name)
            {
                return Some(a);
            }
        }
        None
    }

    fn lower_dyn(&mut self, interface: &hir::PathRef, mutable: bool, span: Span) -> TypeId {
        let poison = self.d.types.poison;
        let Res::Def(def) = interface.res else {
            return poison;
        };
        let m = mutability(mutable);
        match self.sema.def(def).kind {
            DefKind::Interface => {
                let descs = self.generic_descs(def);
                let Some(args) = self.lower_args(interface, def, &descs) else {
                    return poison;
                };
                let file = self.cur.file;
                self.d.obligations.push(Obligation {
                    file,
                    span,
                    kind: ObligationKind::Dyn { interface: def },
                });
                self.d.types.intern(TyKind::Dyn(m, def, args))
            }
            DefKind::Builtin(b @ (Builtin::Fn | Builtin::FnMut | Builtin::FnOnce)) => {
                let kind = match b {
                    Builtin::Fn => FnKind::Fn,
                    Builtin::FnMut => FnKind::FnMut,
                    _ => FnKind::FnOnce,
                };
                match &interface.fn_sig {
                    Some(sig) => {
                        let sig = self.lower_fn_bound_sig(sig);
                        self.d.types.intern(TyKind::DynFn(m, kind, sig))
                    }
                    None => {
                        self.fn_without_signature(interface, kind);
                        poison
                    }
                }
            }
            DefKind::Builtin(Builtin::Executor) => {
                if !interface.args.is_empty() {
                    self.not_generic(interface);
                    return poison;
                }
                self.d.types.intern(TyKind::Dyn(m, def, Vec::new()))
            }
            DefKind::Poison => poison,
            _ => {
                self.not_an_interface(interface, def);
                poison
            }
        }
    }
}

fn mutability(mutable: bool) -> Mutability {
    if mutable {
        Mutability::Mut
    } else {
        Mutability::Shared
    }
}

/// The `StructKind` as a word, for messages.
pub fn kind_word(k: StructKind) -> &'static str {
    match k {
        StructKind::Plain => "struct",
        StructKind::Copy => "copy struct",
        StructKind::Linear => "linear struct",
    }
}
