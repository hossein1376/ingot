//! Phase 6, second half: the checks on declarations, run once every signature, every
//! impl table and every type property exists (pipeline section 1: phase 6 finishes for
//! all items before any body is looked at).
//!
//! An item whose signature mentions poison produces nothing here (pipeline section 3).

use crate::consteval::ValTy;
use crate::defs::{DefId, DefKind};
use crate::hir;
use crate::layout::{LayoutError, valid_align};
use crate::lowering::Cx;
use crate::sigs::*;
use crate::ty::*;
use ingot_diag::diagnostic::Edit;
use ingot_diag::source::Span;
use ingot_syntax::ast::StructKind;
use std::collections::{BTreeMap, BTreeSet};

impl Cx<'_> {
    /// Run all of phase 6.
    pub fn check_all(&mut self) {
        self.d.builtins = BuiltinNames {
            ordering: self.sema.builtin_def("Ordering"),
            eq: self.sema.builtin_def("Eq"),
            ord: self.sema.builtin_def("Ord"),
            hash: self.sema.builtin_def("Hash"),
            iter: self.sema.builtin_def("Iter"),
            format: self.sema.builtin_def("Format"),
            executor: self.sema.builtin_def("Executor"),
        };
        self.lower_all();
        self.eval_constants();
        self.build_impl_tables();
        self.check_impls();
        self.check_functions();
        self.check_externs();
        self.check_types();
        self.check_obligations();
        self.check_main();
        self.note_unchecked_bodies();
        if self.d.gave_up {
            self.ni(
                "the properties of a type with unbounded polymorphic recursion (an instance \
                 that contains an ever larger instance of itself)",
            );
        }
        self.dedup_diagnostics();
    }

    /// Function and test bodies belong to phases 7 to 10, which do not exist yet: a
    /// package that has one is not accepted.
    fn note_unchecked_bodies(&mut self) {
        let Some(package) = self.pkgs.get(self.reported_pkg) else {
            return;
        };
        let any = package.items.values().any(|i| match i {
            hir::Item::Fn(f) => f.body.is_some(),
            hir::Item::Test(_) => true,
            _ => false,
        });
        if any {
            self.ni(crate::UNCHECKED_BODIES);
        }
    }

    fn dedup_diagnostics(&mut self) {
        let mut seen = BTreeSet::new();
        self.d.diagnostics.retain(|d| {
            seen.insert((
                d.code,
                d.file.clone(),
                d.span.start,
                d.span.end,
                d.message.clone(),
            ))
        });
    }

    /// The items of every package in the canonical order.
    fn all_items(&self) -> Vec<DefId> {
        let mut out = Vec::new();
        for package in self.pkgs {
            for module in &package.modules {
                out.extend(module.items.iter().copied());
            }
        }
        out
    }

    fn path_of(&self, file: usize) -> String {
        self.sources.get(file).path.clone()
    }

    // ------------------------------------------------------- constants

    /// Evaluate the initializer of every `const` and `static`. Anything outside the
    /// evaluated subset is `not implemented`, but only for the package being checked.
    fn eval_constants(&mut self) {
        for def in self.all_items() {
            let mut defs = vec![def];
            if let Some(hir::Item::Impl(i)) = self.hir_item(def) {
                defs.extend(i.members.iter().copied());
            }
            for d in defs {
                let quiet = self.pkg_of(d) != self.reported_pkg;
                if quiet {
                    self.quiet += 1;
                }
                match self.hir_item(d) {
                    Some(hir::Item::Const(_)) => {
                        if self.const_has_type(d)
                            && let Err(f) = self.const_value(d)
                        {
                            self.note_failure(&f);
                        }
                    }
                    Some(hir::Item::Static(s)) => self.eval_static(d, s),
                    _ => {}
                }
                if quiet {
                    self.quiet -= 1;
                }
            }
        }
    }

    /// A constant whose type was read (a poisoned one was reported already).
    fn const_has_type(&self, d: DefId) -> bool {
        matches!(self.d.items.get(&d), Some(ItemSig::Const(c)) if !self.d.types.has_poison(c.ty))
    }

    fn eval_static(&mut self, d: DefId, s: &hir::StaticItem) {
        let Some(ItemSig::Static(sig)) = self.d.items.get(&d) else {
            return;
        };
        let ty = sig.ty;
        if self.d.types.has_poison(ty) {
            return;
        }
        let hint = match self.d.types.kind(ty) {
            TyKind::Int(i) => Some(ValTy::Int(*i)),
            TyKind::Bool => Some(ValTy::Bool),
            _ => None,
        };
        let name = self.ty_name(ty);
        let Some(hint) = hint else {
            self.ni(format!(
                "constant evaluation of the initializer of a static of type `{name}`"
            ));
            return;
        };
        let prev = self.cur;
        self.cur = crate::lowering::Cur {
            pkg: self.pkg_of(d),
            file: self.file_of(d),
            impl_def: None,
        };
        let r = self.eval_signature_const(s.value, hint);
        self.cur = prev;
        if let Err(f) = r {
            self.note_failure(&f);
        }
    }

    // ------------------------------------------------------- impl tables

    fn module_of_head(&self, ty: TypeId) -> Option<crate::defs::ModuleId> {
        match self.d.types.kind(ty) {
            TyKind::Adt(d, _) => {
                let def = self.sema.def(*d);
                matches!(def.kind, DefKind::Struct | DefKind::Enum).then_some(def.module)
            }
            _ => None,
        }
    }

    fn build_impl_tables(&mut self) {
        let impls: Vec<DefId> = self
            .all_items()
            .into_iter()
            .filter(|d| matches!(self.hir_item(*d), Some(hir::Item::Impl(_))))
            .collect();
        let mut misplaced: BTreeSet<DefId> = BTreeSet::new();
        for def in &impls {
            let Some(sig) = self.d.impl_sig(*def).cloned() else {
                continue;
            };
            if sig.poisoned {
                continue;
            }
            let Some(hir::Item::Impl(item)) = self.hir_item(*def) else {
                continue;
            };
            let file = self.file_of(*def);
            let target_is_param = matches!(self.d.types.kind(sig.target), TyKind::Param(_));
            if target_is_param {
                self.error(
                    "E0508",
                    "an impl cannot be written for a bare type parameter",
                    file,
                    item.target.span,
                );
                self.poison_impl(*def);
                continue;
            }
            if sig.of != ImplOf::Inherent {
                let here = self.sema.def(*def).module;
                let target_module = self.module_of_head(sig.target);
                let iface_module = match &sig.of {
                    ImplOf::Interface(i, _) if self.sema.def(*i).kind == DefKind::Interface => {
                        Some(self.sema.def(*i).module)
                    }
                    _ => None,
                };
                if target_module != Some(here) && iface_module != Some(here) {
                    self.error(
                        "E0506",
                        "an impl must be in the module that declares the interface or the type",
                        file,
                        sig.header,
                    );
                    misplaced.insert(*def);
                }
            }
            self.d.register_impl(*def);
        }
        // E0507: the same interface twice for overlapping types, one of them the
        // language's own implementation of a built-in interface (section 4.4).
        let registered = self.d.impls.all.clone();
        for &j in &registered {
            let Some(sj) = self.d.impl_sig(j).cloned() else {
                continue;
            };
            let ImplOf::Interface(iface, args) = &sj.of else {
                continue;
            };
            if !misplaced.contains(&j)
                && args.is_empty()
                && self.d.builtin_implements(sj.target, *iface) == Tri::Yes
            {
                let name = self.impl_of_name(&sj.of);
                let ty = self.ty_name(sj.target);
                let fj = self.file_of(j);
                self.error(
                    "E0507",
                    format!("`{ty}` implements `{name}` already, without an impl"),
                    fj,
                    sj.header,
                );
            }
        }
        for (n, &j) in registered.iter().enumerate() {
            let Some(sj) = self.d.impl_sig(j).cloned() else {
                continue;
            };
            if sj.of == ImplOf::Inherent {
                continue;
            }
            for &i in &registered[..n] {
                let Some(si) = self.d.impl_sig(i).cloned() else {
                    continue;
                };
                if !same_interface(&si.of, &sj.of) {
                    continue;
                }
                let mut vars = BTreeSet::new();
                vars.extend(self.d.generics_of(i).iter().map(|g| g.def));
                vars.extend(self.d.generics_of(j).iter().map(|g| g.def));
                let mut a = vec![GenericArg::Ty(si.target)];
                let mut b = vec![GenericArg::Ty(sj.target)];
                if let (ImplOf::Interface(_, x), ImplOf::Interface(_, y)) = (&si.of, &sj.of) {
                    a.extend(x.iter().cloned());
                    b.extend(y.iter().cloned());
                }
                if self.d.unifiable_args(&a, &b, &vars) {
                    let name = self.impl_of_name(&sj.of);
                    let fj = self.file_of(j);
                    let path = self.path_of(self.file_of(i));
                    self.error(
                        "E0507",
                        format!("`{name}` is implemented twice for overlapping types"),
                        fj,
                        sj.header,
                    )
                    .relate_in("previous", &path, si.header);
                    break;
                }
            }
        }
    }

    fn poison_impl(&mut self, def: DefId) {
        if let Some(ItemSig::Impl(i)) = self.d.items.get_mut(&def) {
            i.poisoned = true;
        }
    }

    fn impl_of_name(&self, of: &ImplOf) -> String {
        match of {
            ImplOf::Interface(i, _) => self.sema.def(*i).name.clone(),
            ImplOf::Prop(p) => p.name().to_string(),
            ImplOf::Inherent => String::new(),
        }
    }

    // ------------------------------------------------------- impl contents

    fn check_impls(&mut self) {
        let impls: Vec<DefId> = self.d.impls.all.clone();
        for def in impls {
            let Some(sig) = self.d.impl_sig(def).cloned() else {
                continue;
            };
            let file = self.file_of(def);
            // E1303: property impls state a property and need `unsafe`; every other
            // interface implementation is safe and must not say `unsafe`.
            if let ImplOf::Prop(p) = sig.of {
                let wants_unsafe = matches!(p, PropBound::Send | PropBound::Sync);
                if wants_unsafe != sig.unsafe_ {
                    let what = if wants_unsafe {
                        format!("`impl {}` without `unsafe`", p.name())
                    } else {
                        format!("`unsafe impl {}`", p.name())
                    };
                    self.error(
                        "E1303",
                        format!("{what}: `unsafe` is required only on `Send` and `Sync`"),
                        file,
                        sig.header,
                    );
                }
                continue;
            }
            if sig.unsafe_ && matches!(sig.of, ImplOf::Interface(..)) {
                self.error(
                    "E1303",
                    "`unsafe impl` of an interface that is not `Send` or `Sync`",
                    file,
                    sig.header,
                );
            }
            let ImplOf::Interface(iface, args) = sig.of.clone() else {
                continue;
            };
            if self.sema.def(iface).kind != DefKind::Interface {
                continue;
            }
            self.ensure_sig(iface);
            let Some(isig) = self.d.interface_sig(iface).cloned() else {
                continue;
            };
            if self.d.types.has_poison(sig.target) || args.iter().any(|a| self.arg_has_poison(a)) {
                continue;
            }
            // Associated types.
            for &a in &isig.assoc {
                let aname = self.sema.def(a).name.clone();
                let has_default = matches!(
                    self.d.items.get(&a),
                    Some(ItemSig::Alias(AliasSig { ty: Some(_), .. }))
                );
                if !sig.assoc.contains_key(&aname) && !has_default {
                    let ifile = self.file_of(a);
                    let ipath = self.path_of(ifile);
                    let aspan = self.sema.def(a).span;
                    self.error(
                        "E0519",
                        format!("the impl does not give the associated type `{aname}`"),
                        file,
                        sig.header,
                    )
                    .relate_in("signature", &ipath, aspan);
                }
            }
            let mut declared_assoc: BTreeSet<String> = BTreeSet::new();
            for &a in &isig.assoc {
                declared_assoc.insert(self.sema.def(a).name.clone());
            }
            for (name, &a) in &sig.assoc {
                if !declared_assoc.contains(name) && self.interface_declares(iface, name).is_none()
                {
                    let span = self.sema.def(a).span;
                    self.error(
                        "E0519",
                        format!(
                            "the interface `{}` declares no associated type `{name}`",
                            self.sema.def(iface).name
                        ),
                        file,
                        span,
                    );
                }
            }
            // An interface declares functions and associated types, never a constant.
            for (name, &c) in &sig.consts {
                let span = self.sema.def(c).span;
                self.error(
                    "E0509",
                    format!(
                        "`{name}` is not a function or associated type of `{}`",
                        self.sema.def(iface).name
                    ),
                    file,
                    span,
                );
            }
            // Methods.
            let mut s = Subst::new();
            for (g, a) in self.d.generics_of(iface).iter().zip(&args) {
                s.insert(g.def, a.clone());
            }
            s.insert(isig.self_def, GenericArg::Ty(sig.target));
            let mut iface_methods: BTreeMap<String, DefId> = BTreeMap::new();
            for &m in &isig.methods {
                iface_methods.insert(self.sema.def(m).name.clone(), m);
            }
            for &m in &isig.methods {
                let name = self.sema.def(m).name.clone();
                let Some(ifn) = self.d.fn_sig(m).cloned() else {
                    continue;
                };
                match sig.methods.get(&name) {
                    Some(&im) => {
                        let Some(mfn) = self.d.fn_sig(im).cloned() else {
                            continue;
                        };
                        if let Some(why) = self.compare_methods(&s, &mfn, &ifn) {
                            let ipath = self.path_of(self.file_of(m));
                            self.error(
                                "E0509",
                                format!("`{name}` does not match the interface: {why}"),
                                file,
                                mfn.name_span,
                            )
                            .relate_in(
                                "signature",
                                &ipath,
                                ifn.name_span,
                            );
                        }
                    }
                    None if !ifn.has_body => {
                        self.missing_method(def, &sig, m, &ifn);
                    }
                    None => {}
                }
            }
            for (name, &im) in &sig.methods {
                if !iface_methods.contains_key(name)
                    && let Some(mfn) = self.d.fn_sig(im).cloned()
                {
                    self.error(
                        "E0509",
                        format!(
                            "`{name}` is not a function of `{}`",
                            self.sema.def(iface).name
                        ),
                        file,
                        mfn.name_span,
                    );
                }
            }
            // The type must implement every supertrait of the interface too (§4.4).
            for sup in &isig.supers {
                if *sup == Bound::Poison {
                    continue;
                }
                let bound = self.d.subst_bound(sup, &s);
                match self.d.satisfies(sig.target, &bound) {
                    Tri::No => {
                        let ty = self.ty_name(sig.target);
                        let bname = self.bound_name(&bound);
                        let ipath = self.path_of(self.file_of(iface));
                        let ispan = self.sema.def(iface).span;
                        self.error(
                            "E0801",
                            format!(
                                "`{ty}` implements `{}` but not its supertrait `{bname}`",
                                self.sema.def(iface).name
                            ),
                            file,
                            sig.header,
                        )
                        .relate_in("signature", &ipath, ispan);
                    }
                    Tri::Unknown => {
                        let ty = self.ty_name(sig.target);
                        let bname = self.bound_name(&bound);
                        self.ni(format!(
                            "whether `{ty}` satisfies the supertrait `{bname}` (the \
                             specification does not define it for this type)"
                        ));
                    }
                    Tri::Yes => {}
                }
            }
        }
    }

    fn arg_has_poison(&self, a: &GenericArg) -> bool {
        match a {
            GenericArg::Ty(t) => self.d.types.has_poison(*t),
            GenericArg::Const(c) => *c == ConstArg::Poison,
        }
    }

    /// Why `imp` is not the function `iface` asks for, if it is not.
    fn compare_methods(&mut self, base: &Subst, imp: &FnSig, iface: &FnSig) -> Option<String> {
        let ig = self.d.generics_of(iface.def).to_vec();
        let mg = self.d.generics_of(imp.def).to_vec();
        if ig.len() != mg.len() || ig.iter().zip(&mg).any(|(a, b)| a.is_const != b.is_const) {
            return Some("the generic parameters differ".to_string());
        }
        let mut s = base.clone();
        for (a, b) in ig.iter().zip(&mg) {
            if a.is_const {
                s.insert(a.def, GenericArg::Const(ConstArg::Param(b.def)));
            } else {
                let p = self.d.types.param(b.def);
                s.insert(a.def, GenericArg::Ty(p));
            }
        }
        for (a, b) in ig.iter().zip(&mg) {
            let mut x: Vec<Bound> = a.bounds.iter().map(|b| self.d.subst_bound(b, &s)).collect();
            let mut y = b.bounds.clone();
            x.sort();
            y.sort();
            if x != y {
                return Some(format!("the bounds of `{}` differ", b.name));
            }
        }
        if imp.params.len() != iface.params.len() {
            return Some(format!(
                "it takes {} parameter(s), the interface has {}",
                imp.params.len(),
                iface.params.len()
            ));
        }
        for (i, (p, q)) in imp.params.iter().zip(&iface.params).enumerate() {
            let want = self.d.instantiate(q.ty, &s);
            if p.ty != want && !self.d.types.has_poison(p.ty) && !self.d.types.has_poison(want) {
                return Some(format!(
                    "parameter {} has type `{}`, the interface has `{}`",
                    i + 1,
                    self.ty_name(p.ty),
                    self.ty_name(want)
                ));
            }
            if (p.name == "self") != (q.name == "self") {
                return Some(format!(
                    "parameter {} is not a `self` like the interface's",
                    i + 1
                ));
            }
        }
        let want_ret = self.d.instantiate(iface.ret, &s);
        if imp.ret != want_ret
            && !self.d.types.has_poison(imp.ret)
            && !self.d.types.has_poison(want_ret)
        {
            return Some(format!(
                "it returns `{}`, the interface returns `{}`",
                self.ty_name(imp.ret),
                self.ty_name(want_ret)
            ));
        }
        let want_err = iface.error.map(|e| self.d.instantiate(e, &s));
        if imp.error != want_err
            && imp.error.is_none_or(|e| !self.d.types.has_poison(e))
            && want_err.is_none_or(|e| !self.d.types.has_poison(e))
        {
            return Some("the error type differs".to_string());
        }
        if imp.mods.unsafe_ != iface.mods.unsafe_ {
            return Some("`unsafe` differs".to_string());
        }
        if imp.mods.waitable != iface.mods.waitable {
            return Some("`waitable` differs".to_string());
        }
        // An implementation may declare the same clauses or fewer, never more.
        for o in &imp.from {
            let covered = iface
                .from
                .iter()
                .any(|q| q.param == o.param && q.inner == o.inner && q.is_static == o.is_static);
            if !covered && (o.param.is_some() || o.is_static) {
                return Some(format!(
                    "its `from` clause names `{}`, which the interface's does not",
                    o.name
                ));
            }
        }
        for (i, p) in imp.params.iter().enumerate() {
            if let Some((_, Some(target))) = p.into {
                let has = iface
                    .params
                    .get(i)
                    .is_some_and(|q| matches!(q.into, Some((_, Some(t))) if t == target));
                if !has {
                    return Some(format!(
                        "parameter `{}` has an `into` clause the interface's does not",
                        p.name
                    ));
                }
            }
        }
        None
    }

    /// E0509 for a function the impl lacks, with the fix that inserts it.
    fn missing_method(&mut self, impl_def: DefId, sig: &ImplSig, m: DefId, ifn: &FnSig) {
        let file = self.file_of(impl_def);
        let ifile = self.file_of(m);
        let ipath = self.path_of(ifile);
        let name = ifn.name.clone();
        let fix = self.insertion_for(impl_def, sig, m);
        let d = self.error(
            "E0509",
            format!("the impl lacks the function `{name}` of the interface"),
            file,
            sig.header,
        );
        d.relate_in("signature", &ipath, ifn.name_span);
        if let Some(edit) = fix {
            d.fix(
                format!("add `{name}` with a body of `@panic(\"todo\")`"),
                edit,
            );
        }
    }

    /// The edit that adds the signature of `m` to the impl (diagnostics.md section 1,
    /// "Fixes"), when it can be written exactly.
    fn insertion_for(&mut self, impl_def: DefId, sig: &ImplSig, m: DefId) -> Option<Edit> {
        let file = self.file_of(impl_def);
        let ifile = self.file_of(m);
        // The signature is written in the interface's module: its names mean the same
        // thing in the impl only if the impl is in that module too.
        if file != ifile || self.sema.def(impl_def).module != self.sema.def(m).module {
            return None;
        }
        let Some(hir::Item::Impl(imp)) = self.hir_item(impl_def) else {
            return None;
        };
        let Some(hir::Item::Fn(f)) = self.hir_item(m) else {
            return None;
        };
        // An interface with generic parameters would need its arguments written into
        // the signature: not attempted.
        if !self
            .d
            .generics_of(self.d.impl_interface(impl_def)?)
            .is_empty()
        {
            return None;
        }
        let source = self.sources.get(file);
        let text = &source.text;
        let target_text = source.text_of(imp.target.span).to_string();
        // The span of a function without a body runs one token past its `;`, so the
        // signature is read up to the `;` itself.
        let end = scan_to_semicolon(text, f.span.start)?;
        let mut header = text[f.span.start..end].trim_end().to_string();
        if let Some(rest) = header.strip_prefix("pub ") {
            header = rest.trim_start().to_string();
        }
        let header = replace_self(&header, &target_text);
        // The indentation of the line that holds the opening brace.
        let brace = text[imp.target.span.end..imp.span.end].find('{')? + imp.target.span.end;
        let line_start = text[..brace].rfind('\n').map_or(0, |i| i + 1);
        let base: String = text[line_start..]
            .chars()
            .take_while(|c| *c == ' ' || *c == '\t')
            .collect();
        let (b4, b8) = (format!("{base}    "), format!("{base}        "));
        let member = format!("{b4}{header} {{\n{b8}@panic(\"todo\");\n{b4}}}");
        let last_member_end = sig
            .methods
            .values()
            .chain(sig.assoc.values())
            .chain(sig.consts.values())
            .filter_map(|d| match self.hir_item(*d) {
                Some(hir::Item::Fn(f)) => match f.body {
                    Some(b) => Some(self.pkgs[self.pkg_of(*d)].block(b).span.end),
                    None => scan_to_semicolon(text, f.span.start).map(|e| e + 1),
                },
                Some(hir::Item::Alias(a)) => scan_to_semicolon(text, a.span.start).map(|e| e + 1),
                Some(hir::Item::Const(c)) => scan_to_semicolon(text, c.span.start).map(|e| e + 1),
                _ => None,
            })
            .max();
        let path = self.path_of(file);
        match last_member_end {
            Some(end) => Some(Edit {
                file: path,
                span: Span::empty(end),
                text: format!("\n{member}"),
            }),
            None => {
                let close = imp.span.end;
                let open_to_close = Span::new(brace, close);
                if text[brace..close].trim_end_matches('}').trim() != "{" {
                    return None;
                }
                Some(Edit {
                    file: path,
                    span: open_to_close,
                    text: format!("{{\n{member}\n{base}}}"),
                })
            }
        }
    }

    // ------------------------------------------------------- functions

    fn check_functions(&mut self) {
        let fns: Vec<FnSig> = self
            .d
            .items
            .values()
            .filter_map(|s| match s {
                ItemSig::Fn(f) => Some(f.clone()),
                _ => None,
            })
            .collect();
        let mut exports: BTreeMap<String, (usize, Span)> = BTreeMap::new();
        // Exports in the canonical order, for the shared-symbol check.
        let mut order: Vec<&FnSig> = fns.iter().collect();
        order.sort_by_key(|f| (self.pkg_of(f.def), self.file_of(f.def), f.span.start));
        for f in order {
            let file = self.file_of(f.def);
            self.check_fn(f, file);
            if f.mods.export {
                match exports.get(&f.name) {
                    Some(&(pfile, pspan)) => {
                        let path = self.path_of(pfile);
                        self.error(
                            "E1604",
                            format!("another `export fn` already uses the symbol `{}`", f.name),
                            file,
                            f.name_span,
                        )
                        .relate_in("previous", &path, pspan);
                    }
                    None => {
                        exports.insert(f.name.clone(), (file, f.name_span));
                    }
                }
            }
        }
    }

    fn check_fn(&mut self, f: &FnSig, file: usize) {
        // E0518
        let in_interface = f
            .parent
            .is_some_and(|p| self.sema.def(p).kind == DefKind::Interface);
        if f.abi.is_some() {
            if f.has_body {
                self.error(
                    "E0518",
                    "a function in an `extern` block has no body",
                    file,
                    f.name_span,
                );
            }
        } else if !f.has_body && !in_interface {
            self.error("E0518", "this function has no body", file, f.name_span);
        }
        // A signature that mentions poison has been reported; nothing more is said
        // about its types.
        let poisoned = f.params.iter().any(|p| self.d.types.has_poison(p.ty))
            || self.d.types.has_poison(f.ret)
            || f.error.is_some_and(|e| self.d.types.has_poison(e));
        // E0517
        let allowed: Option<[TypeId; 3]> = match f.parent {
            Some(p) => {
                let target = match self.d.items.get(&p) {
                    Some(ItemSig::Impl(i)) => Some(i.target),
                    Some(ItemSig::Interface(i)) => Some(self.d.types.param(i.self_def)),
                    _ => None,
                };
                target.map(|t| {
                    let shared = self.d.types.intern(TyKind::Ref(Mutability::Shared, t));
                    let excl = self.d.types.intern(TyKind::Ref(Mutability::Mut, t));
                    [t, shared, excl]
                })
            }
            None => None,
        };
        let self_poisoned = poisoned || allowed.is_some_and(|a| self.d.types.has_poison(a[0]));
        for p in &f.params {
            if p.name != "self" || self.d.types.has_poison(p.ty) || self_poisoned {
                continue;
            }
            let ok = allowed.is_some_and(|a| a.contains(&p.ty));
            if !ok {
                let msg = if allowed.is_some() {
                    "the type of `self` must be the implementing type, `&` of it or `&mut` of it"
                } else {
                    "`self` is only a parameter of a function in an `impl` or an interface"
                };
                self.error("E0517", msg, file, p.ty_span);
            }
        }
        // E1410: a waitable function waits without consuming, so its receiver is a shared
        // borrow, and it must have one.
        if f.mods.waitable {
            match f.params.first() {
                Some(p) if p.name == "self" => {
                    if !self.d.types.has_poison(p.ty)
                        && !matches!(self.d.types.kind(p.ty), TyKind::Ref(Mutability::Shared, _))
                    {
                        self.error(
                            "E1410",
                            "the receiver of a `waitable fn` is a shared borrow `&self`",
                            file,
                            p.name_span,
                        );
                    }
                }
                _ => {
                    self.error(
                        "E1410",
                        "a `waitable fn` must take its receiver as a shared borrow `&self`",
                        file,
                        f.name_span,
                    );
                }
            }
        }
        // E1206 (the part about names)
        if poisoned {
            return self.check_fn_symbol(f, file);
        }
        for o in &f.from {
            if o.param.is_none() && !o.is_static {
                self.error(
                    "E1206",
                    format!("`from {}` names something that is not a parameter", o.name),
                    file,
                    o.span,
                );
            }
        }
        for p in &f.params {
            if let Some((span, None)) = p.into {
                self.error(
                    "E1206",
                    "`into` names something that is not a parameter",
                    file,
                    span,
                );
            }
        }
        // E1601, E1604: the C boundary.
        if f.abi.is_some() || f.mods.export {
            for p in &f.params {
                if !self.ffi_ok(p.ty, false, 0) {
                    let name = self.ty_name(p.ty);
                    self.error(
                        "E1601",
                        format!("`{name}` is not a type that can cross the C boundary"),
                        file,
                        p.ty_span,
                    );
                }
            }
            if !self.ffi_ok(f.ret, true, 0) {
                let name = self.ty_name(f.ret);
                self.error(
                    "E1601",
                    format!("`{name}` is not a type that can cross the C boundary"),
                    file,
                    f.ret_span,
                );
            }
        }
        self.check_fn_symbol(f, file);
    }

    /// E1604 for what is wrong with an `export fn` itself, and the gap of a fallible
    /// foreign declaration.
    fn check_fn_symbol(&mut self, f: &FnSig, file: usize) {
        if f.mods.export {
            if !self.d.generics_of(f.def).is_empty() {
                self.error(
                    "E1604",
                    "an `export fn` cannot be generic",
                    file,
                    f.name_span,
                );
            }
            if f.error.is_some() {
                self.error(
                    "E1604",
                    "an `export fn` cannot be fallible",
                    file,
                    f.name_span,
                );
            }
        } else if f.abi.is_some()
            && let Some(error_span) = f.error_span
        {
            self.error(
                "E1601",
                "the result of a foreign function cannot be fallible: a `T ! E` is not a type \
                 that can cross the C boundary",
                file,
                f.ret_span.join(error_span),
            );
        }
    }

    /// Integers, floats, `bool`, raw pointers, `extern fn` pointers and `copy struct`s
    /// of those (section 29). `void` only as a result.
    fn ffi_ok(&mut self, ty: TypeId, result: bool, depth: u32) -> bool {
        if self.d.types.has_poison(ty) || depth > 16 {
            return true;
        }
        match self.d.types.kind(ty).clone() {
            TyKind::Int(_) | TyKind::Float(_) | TyKind::Bool | TyKind::RawPtr(..) => true,
            TyKind::Void => result,
            TyKind::FnPtr(sig) => sig.ext,
            TyKind::Adt(d, args) => {
                let Some(ItemSig::Struct(st)) = self.d.items.get(&d).cloned() else {
                    return false;
                };
                if st.kind != StructKind::Copy {
                    return false;
                }
                let s = self.d.subst_for(d, &args);
                st.fields.iter().all(|f| {
                    let t = self.d.types.subst(f.ty, &s);
                    self.ffi_ok(t, false, depth + 1)
                })
            }
            _ => false,
        }
    }

    /// E1603 on every `extern` block with an ABI string the specification lacks.
    fn check_externs(&mut self) {
        let pkgs = self.pkgs;
        for package in pkgs {
            for module in &package.modules {
                let file = self.sema.module(module.module).file.unwrap_or(0);
                for x in &module.externs {
                    if !["c", "c-blocking", "c-bigstack"].contains(&x.abi.as_str()) {
                        self.error(
                            "E1603",
                            format!(
                                "`\"{}\"` is not an ABI: use \"c\", \"c-blocking\" or \"c-bigstack\"",
                                x.abi
                            ),
                            file,
                            x.abi_span,
                        );
                    }
                }
            }
        }
    }

    /// E0514 for the `main` of the package being checked.
    fn check_main(&mut self) {
        let pkg = self.reported_pkg;
        let Some(package) = self.sema.packages.get(pkg) else {
            return;
        };
        let Some(root) = package.root else {
            return;
        };
        let Some(&def) = self.sema.module(root).values.get("main") else {
            return;
        };
        if self.sema.def(def).kind != DefKind::Fn || self.sema.def(def).module != root {
            return;
        }
        let Some(f) = self.d.fn_sig(def).cloned() else {
            return;
        };
        let file = self.file_of(def);
        let mut why: Vec<&str> = Vec::new();
        if self.sema.def(def).vis != crate::defs::Vis::Pub {
            why.push("it is not `pub`");
        }
        if !self.d.generics_of(def).is_empty() {
            why.push("it is generic");
        }
        let env = self.env_type();
        match f.params.as_slice() {
            [p] => {
                let ok = match env {
                    Some(e) => {
                        let want = self.d.types.intern(TyKind::Ref(Mutability::Shared, e));
                        p.ty == want || self.d.types.has_poison(p.ty)
                    }
                    None => true,
                };
                if !ok {
                    why.push("its parameter is not `env: &Env`");
                }
            }
            _ => why.push("it must take exactly one parameter, `env: &Env`"),
        }
        let u8_ty = self.d.types.int(IntTy::U8);
        if f.ret != self.d.types.void && f.ret != u8_ty && !self.d.types.has_poison(f.ret) {
            why.push("it must return `void` or `u8`");
        }
        if !why.is_empty() {
            self.error(
                "E0514",
                format!("`main` is not the entry point: {}", why.join(", ")),
                file,
                f.name_span,
            );
        }
    }

    /// `std.Env`, from the root module of the package `std` (or of the package being
    /// checked, when that is `std`).
    fn env_type(&mut self) -> Option<TypeId> {
        let root = self.sema.module_paths.get("std")?;
        let &def = self.sema.module(*root).types.get("Env")?;
        if !matches!(self.sema.def(def).kind, DefKind::Struct) {
            return None;
        }
        Some(self.d.types.intern(TyKind::Adt(def, Vec::new())))
    }

    // ------------------------------------------------------- types

    fn check_types(&mut self) {
        let defs: Vec<DefId> = self.d.items.keys().copied().collect();
        // E0516 first: the other checks must not run into a type of infinite size.
        for &d in &defs {
            if matches!(
                self.d.items.get(&d),
                Some(ItemSig::Struct(_) | ItemSig::Enum(_))
            ) {
                self.check_infinite(d);
            }
        }
        for &d in &defs {
            match self.d.items.get(&d).cloned() {
                Some(ItemSig::Struct(s)) => self.check_struct(&s),
                Some(ItemSig::Enum(e)) => self.check_enum(&e),
                Some(ItemSig::Static(s)) => self.check_static(&s),
                _ => {}
            }
        }
    }

    /// E0516: a struct or enum that contains itself by value.
    fn check_infinite(&mut self, def: DefId) {
        let file = self.file_of(def);
        let fields: Vec<(TypeId, Span)> = match self.d.items.get(&def) {
            Some(ItemSig::Struct(s)) => s.fields.iter().map(|f| (f.ty, f.ty_span)).collect(),
            Some(ItemSig::Enum(e)) => e
                .variants
                .iter()
                .filter_map(|v| v.payload.zip(v.payload_span))
                .collect(),
            _ => return,
        };
        for (ty, span) in fields {
            let mut seen = BTreeSet::new();
            if self.reaches_by_value(ty, def, &mut seen, 0) {
                let name = self.sema.def(def).name.clone();
                self.error(
                    "E0516",
                    format!("`{name}` contains itself, so it would have infinite size"),
                    file,
                    span,
                );
                self.d.infinite.insert(def);
                return;
            }
        }
    }

    /// Whether a value of `ty` holds a value of the struct or enum `target`.
    fn reaches_by_value(
        &mut self,
        ty: TypeId,
        target: DefId,
        seen: &mut BTreeSet<TypeId>,
        depth: u32,
    ) -> bool {
        if depth > 32 {
            return false;
        }
        match self.d.types.kind(ty).clone() {
            TyKind::Adt(d, args) => {
                if d == target {
                    return true;
                }
                if !seen.insert(ty) {
                    return false;
                }
                match self.d.adt_part_types(d, &args) {
                    Some(parts) => parts
                        .into_iter()
                        .any(|p| self.reaches_by_value(p, target, seen, depth + 1)),
                    None => false,
                }
            }
            TyKind::Array(e, n) => {
                n.as_len() != Some(0) && self.reaches_by_value(e, target, seen, depth + 1)
            }
            TyKind::Tuple(ts) => ts
                .into_iter()
                .any(|t| self.reaches_by_value(t, target, seen, depth + 1)),
            TyKind::Optional(e) => self.reaches_by_value(e, target, seen, depth + 1),
            TyKind::Result(a, b) => {
                self.reaches_by_value(a, target, seen, depth + 1)
                    || self.reaches_by_value(b, target, seen, depth + 1)
            }
            _ => false,
        }
    }

    fn check_struct(&mut self, s: &StructSig) {
        let file = self.file_of(s.def);
        let name = self.sema.def(s.def).name.clone();
        for f in &s.fields {
            if self.d.types.has_poison(f.ty) {
                continue;
            }
            let p = self.d.props(f.ty);
            if s.kind == StructKind::Copy && p.copy == Tri::No {
                let t = self.ty_name(f.ty);
                self.error(
                    "E0501",
                    format!(
                        "field `{}` of `copy struct {name}` has type `{t}`, which is not `Copy`",
                        f.name
                    ),
                    file,
                    f.ty_span,
                );
            }
            if s.kind == StructKind::Copy && p.copy == Tri::Unknown {
                self.ni(format!(
                    "whether field `{}` of `copy struct {name}` is `Copy` (the properties of its \
                     type are not defined by the specification)",
                    f.name
                ));
            }
        }
        if s.kind == StructKind::Plain
            && let Some(f) = s
                .fields
                .iter()
                .find(|f| !self.d.types.has_poison(f.ty) && self.d.props(f.ty).linear == Tri::Yes)
        {
            // The `struct` keyword is the last word before the name.
            let source = self.sources.get(file);
            let name_start = self.sema.def(s.def).span.start;
            let head = &source.text[s.span.start..name_start];
            if let Some(i) = head.rfind("struct") {
                let at = s.span.start + i;
                let path = self.path_of(file);
                let fname = f.name.clone();
                self.error(
                    "E0502",
                    format!(
                        "`{name}` has the linear field `{fname}`, so it must be a `linear struct`"
                    ),
                    file,
                    Span::new(at, at + "struct".len()),
                )
                .fix(
                    "declare the struct `linear`",
                    Edit {
                        file: path,
                        span: Span::empty(at),
                        text: "linear ".to_string(),
                    },
                );
            }
        }
        if self.d.infinite.contains(&s.def) {
            return;
        }
        // E0515 on the struct and on its fields.
        if let Some(a) = s.align {
            let natural = self.d.natural_struct_align(s.def);
            self.check_align(file, a, natural);
        }
        for f in &s.fields {
            if let Some(a) = f.align {
                let natural = self.d.layout_of(f.ty).map(|l| l.align);
                self.check_align(file, a, natural);
            }
        }
        // The layout of a struct without parameters is kept (and its unspecified
        // parts recorded).
        if self.d.generics_of(s.def).is_empty() {
            let ty = self.d.types.intern(TyKind::Adt(s.def, Vec::new()));
            if !self.d.types.has_poison(ty) {
                let _ = self.layout_of_checked(ty);
            }
        }
    }

    /// E0515: the argument of `align(...)` is a power of two, not below the natural
    /// alignment of what it applies to.
    fn check_align(&mut self, file: usize, a: AlignAttr, natural: Result<u64, LayoutError>) {
        match valid_align(a.value) {
            None => {
                self.error(
                    "E0515",
                    format!("`align({})`: the alignment must be a power of two", a.value),
                    file,
                    a.span,
                );
            }
            Some(v) => match natural {
                Ok(n) if v < n => {
                    self.error(
                        "E0515",
                        format!("`align({v})` is below the natural alignment {n}"),
                        file,
                        a.span,
                    );
                }
                Ok(_) => {}
                Err(LayoutError::Params) => self.ni(
                    "a check of `align(...)` against the natural alignment of a type that \
                     depends on a generic parameter (computed per instance)",
                ),
                Err(LayoutError::Unspecified(m)) => self.ni(m),
                Err(LayoutError::Poison | LayoutError::Cycle) => {}
            },
        }
    }

    fn check_enum(&mut self, e: &EnumSig) {
        let file = self.file_of(e.def);
        let name = self.sema.def(e.def).name.clone();
        // E0504
        let mut first: BTreeMap<TypeId, (Span, String)> = BTreeMap::new();
        for v in &e.variants {
            if !v.from {
                continue;
            }
            let Some(t) = v.payload else {
                continue;
            };
            if self.d.types.has_poison(t) {
                continue;
            }
            match first.get(&t) {
                Some((prev, _)) => {
                    let path = self.path_of(file);
                    let prev = *prev;
                    let tn = self.ty_name(t);
                    self.error(
                        "E0504",
                        format!("two variants of `{name}` are `from` the same type `{tn}`"),
                        file,
                        v.span,
                    )
                    .relate_in("previous", &path, prev);
                }
                None => {
                    first.insert(t, (v.span, v.name.clone()));
                }
            }
        }
        // E0501
        if e.copy {
            for v in &e.variants {
                let (Some(t), Some(span)) = (v.payload, v.payload_span) else {
                    continue;
                };
                if self.d.types.has_poison(t) {
                    continue;
                }
                let p = self.d.props(t);
                if p.copy == Tri::No {
                    let tn = self.ty_name(t);
                    self.error(
                        "E0501",
                        format!(
                            "the payload of `{}` in `copy enum {name}` has type `{tn}`, which is \
                             not `Copy`",
                            v.name
                        ),
                        file,
                        span,
                    );
                } else if p.copy == Tri::Unknown {
                    self.ni(format!(
                        "whether the payload of `{}` in `copy enum {name}` is `Copy` (the \
                         properties of its type are not defined by the specification)",
                        v.name
                    ));
                }
            }
        }
        if self.d.generics_of(e.def).is_empty() && !self.d.infinite.contains(&e.def) {
            let ty = self.d.types.intern(TyKind::Adt(e.def, Vec::new()));
            let _ = self.layout_of_checked(ty);
        }
    }

    fn check_static(&mut self, s: &StaticSig) {
        let file = self.file_of(s.def);
        if self.d.types.has_poison(s.ty) {
            return;
        }
        let p = self.d.props(s.ty);
        match p.sync {
            Tri::No => {
                let t = self.ty_name(s.ty);
                self.error(
                    "E0511",
                    format!("the type `{t}` of a `static` must be `Sync`"),
                    file,
                    s.ty_span,
                );
            }
            Tri::Unknown => self.ni(
                "whether the type of a `static` is `Sync` (the properties of the type are not \
                 defined by the specification)",
            ),
            Tri::Yes => {}
        }
        if let Some(a) = s.align {
            let natural = self.d.layout_of(s.ty).map(|l| l.align);
            self.check_align(file, a, natural);
        }
    }

    // ------------------------------------------------------- obligations

    pub(crate) fn check_obligations(&mut self) {
        let mut obligations = std::mem::take(&mut self.d.obligations);
        obligations.sort_by_key(|o| (o.file, o.span.start, o.span.end));
        for o in obligations {
            match &o.kind {
                ObligationKind::Dyn { interface } => self.check_dyn(&o, *interface),
                ObligationKind::Args { item, args, spans } => {
                    self.check_args(&o, *item, args, spans)
                }
            }
        }
    }

    /// E0510: `&dyn I` needs an interface whose methods do not take or return `Self`
    /// by value and have no generic parameters (section 4.4).
    fn check_dyn(&mut self, o: &Obligation, iface: DefId) {
        self.ensure_sig(iface);
        let Some(isig) = self.d.interface_sig(iface).cloned() else {
            return;
        };
        for &m in &isig.methods {
            let Some(f) = self.d.fn_sig(m).cloned() else {
                continue;
            };
            let mut why = None;
            if !self.d.generics_of(m).is_empty() {
                why = Some("it has generic parameters");
            } else {
                let mut tys: Vec<TypeId> = f.params.iter().map(|p| p.ty).collect();
                tys.push(f.ret);
                tys.extend(f.error);
                if tys
                    .into_iter()
                    .any(|t| self.mentions_self_by_value(t, isig.self_def))
                {
                    why = Some("it takes or returns `Self` by value");
                }
            }
            if let Some(why) = why {
                let ipath = self.path_of(self.file_of(m));
                let mname = f.name.clone();
                let iname = self.sema.def(iface).name.clone();
                self.error(
                    "E0510",
                    format!(
                        "`dyn {iname}` is not allowed: `{mname}` cannot be called through it, {why}"
                    ),
                    o.file,
                    o.span,
                )
                .relate_in("declared", &ipath, f.name_span);
                return;
            }
        }
    }

    fn mentions_self_by_value(&self, ty: TypeId, self_def: DefId) -> bool {
        match self.d.types.kind(ty) {
            TyKind::Param(p) => *p == self_def,
            TyKind::Ref(..) | TyKind::Slice(..) | TyKind::RawPtr(..) => false,
            TyKind::Dyn(..) | TyKind::DynFn(..) | TyKind::Projection(..) => false,
            _ => self
                .d
                .types
                .children(ty)
                .into_iter()
                .any(|c| self.mentions_self_by_value(c, self_def)),
        }
    }

    /// E0801 for the arguments of one use of a generic item.
    fn check_args(&mut self, o: &Obligation, item: DefId, args: &[GenericArg], spans: &[Span]) {
        self.ensure_generics(item);
        let generics = self.d.generics_of(item).to_vec();
        let mut s = Subst::new();
        for (g, a) in generics.iter().zip(args) {
            s.insert(g.def, a.clone());
        }
        let ipath = self.path_of(self.file_of(item));
        for (i, g) in generics.iter().enumerate() {
            let Some(GenericArg::Ty(arg)) = args.get(i).cloned() else {
                continue;
            };
            if self.d.types.has_poison(arg) {
                continue;
            }
            for (j, b) in g.bounds.iter().enumerate() {
                if *b == Bound::Poison {
                    continue;
                }
                let b = self.d.subst_bound(b, &s);
                let span = spans.get(i).copied().unwrap_or(o.span);
                match self.d.satisfies(arg, &b) {
                    Tri::No => {
                        let at = self.ty_name(arg);
                        let bn = self.bound_name(&b);
                        let bspan = g.bound_spans.get(j).copied().unwrap_or(g.span);
                        self.error(
                            "E0801",
                            format!("`{at}` does not satisfy the bound `{bn}` of `{}`", g.name),
                            o.file,
                            span,
                        )
                        .relate_in("signature", &ipath, bspan);
                    }
                    Tri::Unknown => {
                        let at = self.ty_name(arg);
                        let bn = self.bound_name(&b);
                        self.ni(format!(
                            "whether `{at}` satisfies the bound `{bn}` (the specification does \
                             not define it for this type)"
                        ));
                    }
                    Tri::Yes => {}
                }
            }
        }
    }

    pub fn bound_name(&self, b: &Bound) -> String {
        match b {
            Bound::Prop(p) => p.name().to_string(),
            Bound::Fn(k, sig) => {
                format!("{}{}", k.name(), self.d.types.display_sig(self.sema, sig))
            }
            Bound::Interface(d, args) => {
                let mut out = self.sema.def(*d).name.clone();
                if !args.is_empty() {
                    out.push('[');
                    for (i, a) in args.iter().enumerate() {
                        if i > 0 {
                            out.push_str(", ");
                        }
                        match a {
                            GenericArg::Ty(t) => out.push_str(&self.ty_name(*t)),
                            GenericArg::Const(c) => out.push_str(&format!("{c:?}")),
                        }
                    }
                    out.push(']');
                }
                out
            }
            Bound::Poison => "{error}".to_string(),
        }
    }
}

fn same_interface(a: &ImplOf, b: &ImplOf) -> bool {
    match (a, b) {
        (ImplOf::Interface(x, _), ImplOf::Interface(y, _)) => x == y,
        (ImplOf::Prop(x), ImplOf::Prop(y)) => x == y,
        _ => false,
    }
}

/// `text` with every word `Self` that is not followed by `.` replaced by `target`.
fn replace_self(text: &str, target: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = String::new();
    let mut i = 0;
    while i < bytes.len() {
        let rest = &text[i..];
        let before_ok = i == 0 || !is_word(bytes[i - 1]);
        if rest.starts_with("Self")
            && before_ok
            && !rest[4..]
                .chars()
                .next()
                .is_some_and(|c| is_word(c as u8) && c.is_ascii())
            && !rest[4..].starts_with('.')
        {
            out.push_str(target);
            i += 4;
        } else {
            let ch = rest.chars().next().expect("a character");
            out.push(ch);
            i += ch.len_utf8();
        }
    }
    out
}

fn is_word(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// The offset of the first `;` at or after `start` that is not inside brackets.
fn scan_to_semicolon(text: &str, start: usize) -> Option<usize> {
    let mut depth = 0i32;
    for (i, c) in text[start..].char_indices() {
        match c {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            ';' if depth <= 0 => return Some(start + i),
            _ => {}
        }
        if depth < 0 {
            return None;
        }
    }
    None
}
