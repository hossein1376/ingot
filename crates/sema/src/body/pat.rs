//! Patterns (spec section 7.2): typing a pattern against the type it is matched with,
//! and the binding mode of each name it introduces.
//!
//! A pattern other than a name or `_` looks through the borrows of the value it is
//! matched against, and a name bound underneath binds a borrow: `match &v` gives shared
//! borrows of the payloads and `match &mut v` exclusive ones.

use super::Checker;
use crate::hir::{self, ExprId, PatId, PatKind};
use crate::sigs::{ItemSig, Tri};
use crate::ty::*;
use crate::typed::*;
use ingot_diag::source::Span;
use std::collections::BTreeMap;

/// The default binding mode while a pattern descends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Bm {
    Value,
    Ref,
    RefMut,
}

impl Checker<'_, '_, '_> {
    /// Type the pattern `p` against a scrutinee of type `ty`.
    pub(crate) fn pattern(&mut self, p: PatId, ty: TypeId) {
        self.pat_inner(p, ty, Bm::Value);
    }

    /// The alternatives of a `match` arm.
    pub(crate) fn arm_patterns(&mut self, arm: &hir::Arm, st: TypeId) {
        let mut first: Option<BTreeMap<String, TypeId>> = None;
        for &p in &arm.patterns {
            self.pattern(p, st);
            let mut binds = BTreeMap::new();
            self.pattern_binds(p, &mut binds);
            match &first {
                None => first = Some(binds),
                Some(expected) if expected != &binds => {
                    let span = self.hir.pat(p).span;
                    self.err(
                        "E0603",
                        "this alternative binds different names or types; every alternative of \
                         `p | q` binds the same names with the same types",
                        span,
                    );
                }
                Some(_) => {}
            }
        }
    }

    fn pattern_binds(&self, p: PatId, out: &mut BTreeMap<String, TypeId>) {
        match &self.hir.pat(p).kind {
            PatKind::Bind { local } => {
                let name = self.hir.local(*local).name.clone();
                out.insert(name, self.local_ty(*local));
            }
            PatKind::Variant {
                payload: Some(p), ..
            } => self.pattern_binds(*p, out),
            PatKind::Tuple(ps) => {
                for p in ps {
                    self.pattern_binds(*p, out);
                }
            }
            PatKind::Struct { fields, .. } => {
                for f in fields {
                    self.pattern_binds(f.pattern, out);
                }
            }
            _ => {}
        }
    }

    fn peel(&self, mut ty: TypeId, mut bm: Bm) -> (TypeId, u32, Bm) {
        let mut derefs = 0;
        while let TyKind::Ref(m, inner) = self.cx.d.types.kind(ty) {
            bm = match (bm, *m) {
                (Bm::Value, Mutability::Mut) | (Bm::RefMut, Mutability::Mut) => Bm::RefMut,
                _ => Bm::Ref,
            };
            ty = *inner;
            derefs += 1;
        }
        (ty, derefs, bm)
    }

    fn record(&mut self, p: PatId, ty: TypeId, derefs: u32, res: PatRes) {
        self.out.pats.insert(p, PatInfo { ty, derefs, res });
    }

    fn bad_pattern(&mut self, p: PatId, what: &str, ty: TypeId) {
        self.failed_pats.insert(p);
        let span = self.hir.pat(p).span;
        let name = self.name(ty);
        self.err(
            "E0603",
            format!("{what} does not fit a value of type `{name}`"),
            span,
        );
    }

    fn pat_inner(&mut self, p: PatId, ty: TypeId, bm: Bm) {
        let hir = self.hir;
        let pat = hir.pat(p);
        let poison = self.poison();
        // Poison on the scrutinee: bind the names to poison and say nothing.
        if self.is_poison(ty) {
            self.pat_blind(p);
            return;
        }
        match &pat.kind {
            PatKind::Wild => {
                // Under a borrow (`match &v`) the `_` discards a borrow, not the value.
                if bm == Bm::Value && !self.is_poison(ty) && !self.is_never(ty) {
                    let p = self.cx.d.props(ty);
                    match p.discard() {
                        Tri::No => {
                            let name = self.name(ty);
                            self.err(
                                "E0414",
                                format!("`_` cannot discard a value of the linear type `{name}`"),
                                pat.span,
                            );
                        }
                        Tri::Unknown => self.ni(
                            "whether the value of a type the specification does not define the \
                             properties of can be discarded",
                        ),
                        Tri::Yes => {}
                    }
                }
                self.record(p, ty, 0, PatRes::None)
            }
            PatKind::Bind { local } => {
                // A bare name that is a variant of the scrutinee's enum was meant as
                // `.name` (section 7.2).
                let name = self.hir.local(*local).name.clone();
                let (t, _, _) = self.peel(ty, bm);
                if !self.is_poison(t)
                    && self.is_enum_like(t)
                    && self.variant_named(t, &name).is_some()
                {
                    let span = self.hir.pat(p).span;
                    let tname = self.name(t);
                    let edit = self.edit(Span::empty(span.start), ".");
                    self.failed_pats.insert(p);
                    self.err(
                        "E0603",
                        format!(
                            "`{name}` is a variant of `{tname}`; a bare name binds a new local, \
                             so write `.name`"
                        ),
                        span,
                    )
                    .fix("write the variant", edit);
                }
                let (lty, mode) = match bm {
                    Bm::Value => {
                        let mode = if self.is_copy(ty) {
                            BindMode::Copy
                        } else {
                            BindMode::Move
                        };
                        (ty, mode)
                    }
                    Bm::Ref => (self.ref_to(Mutability::Shared, ty), BindMode::Ref),
                    Bm::RefMut => (self.ref_to(Mutability::Mut, ty), BindMode::RefMut),
                };
                self.bind_local(*local, lty, mode);
                self.record(p, ty, 0, PatRes::None);
            }
            PatKind::Literal(lit) => {
                let (t, derefs, _) = self.peel(ty, bm);
                self.literal_pattern(p, *lit, t);
                self.record(p, t, derefs, PatRes::None);
            }
            PatKind::Range { lo, hi, inclusive } => {
                let _ = inclusive;
                let (t, derefs, _) = self.peel(ty, bm);
                for sub in [*lo, *hi] {
                    match &hir.pat(sub).kind {
                        PatKind::Literal(lit) => {
                            self.literal_pattern(sub, *lit, t);
                            self.record(sub, t, 0, PatRes::None);
                        }
                        _ => {
                            self.ni("a range pattern whose bound is not a literal");
                            self.pat_blind(sub);
                        }
                    }
                }
                if !matches!(self.kind(t), TyKind::Int(_)) && !self.is_poison(t) {
                    let span = pat.span;
                    let name = self.name(t);
                    self.err(
                        "E0609",
                        format!("a range pattern needs integers, not `{name}`"),
                        span,
                    );
                }
                self.record(p, t, derefs, PatRes::None);
            }
            PatKind::Variant {
                ty: path,
                name,
                name_span,
                payload,
            } => {
                let (t, derefs, bm2) = self.peel(ty, bm);
                if let Some(path) = path {
                    let pt = self.lower_path(path);
                    if !self.is_poison(pt) && pt != t {
                        self.bad_pattern(p, "this variant", t);
                        self.pat_blind(p);
                        return;
                    }
                }
                if !self.is_enum_like(t) {
                    self.bad_pattern(p, "an enum variant", t);
                    self.pat_blind_children(payload);
                    self.record(p, t, derefs, PatRes::None);
                    return;
                }
                let Some(v) = self.variant_named(t, name) else {
                    let names = self.variant_names(t);
                    self.failed_pats.insert(p);
                    self.unknown_member(name, *name_span, &names, t);
                    self.pat_blind_children(payload);
                    self.record(p, t, derefs, PatRes::None);
                    return;
                };
                match (v.payload, payload) {
                    (Some(pt), Some(sub)) => self.pat_inner(*sub, pt, bm2),
                    (None, None) => {}
                    (Some(_), None) => {
                        self.bad_pattern(p, &format!("`.{name}` without its payload"), t);
                    }
                    (None, Some(sub)) => {
                        self.bad_pattern(
                            p,
                            &format!("`.{name}(..)`, a variant without a payload,"),
                            t,
                        );
                        self.pat_blind(*sub);
                    }
                }
                self.record(p, t, derefs, PatRes::Variant(v));
            }
            PatKind::Tuple(elems) => {
                let (t, derefs, bm2) = self.peel(ty, bm);
                match self.kind(t) {
                    TyKind::Tuple(ts) if ts.len() == elems.len() => {
                        for (sub, et) in elems.iter().zip(ts) {
                            self.pat_inner(*sub, et, bm2);
                        }
                    }
                    _ => {
                        self.bad_pattern(p, "a tuple pattern", t);
                        for sub in elems {
                            self.pat_blind(*sub);
                        }
                    }
                }
                self.record(p, t, derefs, PatRes::None);
            }
            PatKind::Struct {
                ty: path,
                fields,
                rest,
                rest_span,
            } => {
                let (t, derefs, bm2) = self.peel(ty, bm);
                if let Some(path) = path {
                    let pt = self.lower_path(path);
                    if !self.is_poison(pt) && pt != t {
                        self.bad_pattern(p, "this struct pattern", t);
                        for f in fields {
                            self.pat_blind(f.pattern);
                        }
                        self.record(p, t, derefs, PatRes::None);
                        return;
                    }
                }
                self.struct_pattern(
                    p,
                    t,
                    derefs,
                    bm2,
                    fields,
                    if *rest { Some(*rest_span) } else { None },
                );
            }
            PatKind::Poison => self.record(p, poison, 0, PatRes::None),
        }
    }

    fn variant_names(&mut self, t: TypeId) -> Vec<String> {
        match self.kind(t) {
            TyKind::Adt(d, _) => self
                .cx
                .d
                .enum_sig(d)
                .map(|e| e.variants.iter().map(|v| v.name.clone()).collect())
                .unwrap_or_default(),
            TyKind::Optional(_) => vec!["none".to_string(), "some".to_string()],
            TyKind::Result(..) => vec!["ok".to_string(), "err".to_string()],
            _ => Vec::new(),
        }
    }

    fn struct_pattern(
        &mut self,
        p: PatId,
        t: TypeId,
        derefs: u32,
        bm: Bm,
        fields: &[hir::FieldPat],
        rest_span: Option<Span>,
    ) {
        let rest = rest_span.is_some();
        let sig = match self.kind(t) {
            TyKind::Adt(d, args) => self.cx.d.struct_sig(d).cloned().map(|s| (d, args, s)),
            _ => None,
        };
        let Some((def, args, sig)) = sig else {
            self.bad_pattern(p, "a struct pattern", t);
            for f in fields {
                self.pat_blind(f.pattern);
            }
            self.record(p, t, derefs, PatRes::None);
            return;
        };
        let s = self.cx.d.subst_for(def, &args);
        let mut order = Vec::new();
        for f in fields {
            let Some(i) = sig.fields.iter().position(|x| x.name == f.name) else {
                self.failed_pats.insert(p);
                let names: Vec<String> = sig.fields.iter().map(|x| x.name.clone()).collect();
                self.unknown_member(&f.name, f.name_span, &names, t);
                self.pat_blind(f.pattern);
                continue;
            };
            if order.contains(&i) {
                self.bad_pattern(p, &format!("the field `{}` twice; the pattern", f.name), t);
                self.pat_blind(f.pattern);
                continue;
            }
            let field = &sig.fields[i];
            if !field.public && self.cx.sema.def(def).module != self.module {
                self.failed_pats.insert(p);
                let fpath = self.cx.sources.get(self.cx.file_of(def)).path.clone();
                let decl = field.name_span;
                self.err(
                    "E0307",
                    format!("the field `{}` is private to its module", f.name),
                    f.name_span,
                )
                .relate_in("declared", &fpath, decl);
                self.pat_blind(f.pattern);
                continue;
            }
            order.push(i);
            let fty = self.cx.d.instantiate(field.ty, &s);
            self.pat_inner(f.pattern, fty, bm);
        }
        let omitted: Vec<usize> = (0..sig.fields.len())
            .filter(|i| !order.contains(i))
            .collect();
        // Every field the pattern lists was valid (`order.len() == fields.len()`), so a
        // missing field is the only problem.
        if !rest && order.len() == fields.len() && !omitted.is_empty() {
            self.failed_pats.insert(p);
            let names: Vec<&str> = omitted
                .iter()
                .map(|i| sig.fields[*i].name.as_str())
                .collect();
            self.err(
                "E0603",
                format!(
                    "this struct pattern leaves out `{}`; end it with `..` to leave fields out",
                    names.join("`, `")
                ),
                self.hir.pat(p).span,
            );
        }
        // A `..` matches every field it leaves out, so it cannot take apart a struct of
        // another module with a private field (E0307), even when no private field is
        // written in the pattern.
        if rest
            && self.cx.sema.def(def).module != self.module
            && let Some(i) = omitted.iter().find(|i| !sig.fields[**i].public)
        {
            self.failed_pats.insert(p);
            let field = &sig.fields[*i];
            let fpath = self.cx.sources.get(self.cx.file_of(def)).path.clone();
            self.err(
                "E0307",
                format!(
                    "`..` would match the field `{}`, which is private to its module",
                    field.name
                ),
                rest_span.expect("a rest pattern"),
            )
            .relate_in("declared", &fpath, field.name_span);
        }
        self.record(
            p,
            t,
            derefs,
            PatRes::Struct {
                res: StructRes { def, args, order },
                rest,
                omitted,
            },
        );
    }

    fn literal_pattern(&mut self, p: PatId, lit: ExprId, t: TypeId) {
        let lt = self.expr(lit, Some(t));
        if !self.compatible(lt, t) {
            self.bad_pattern(p, "this literal", t);
        }
    }

    /// Bind every name of a pattern to the poison type, after an error.
    pub(crate) fn pat_blind(&mut self, p: PatId) {
        let hir = self.hir;
        let poison = self.poison();
        match &hir.pat(p).kind {
            PatKind::Bind { local } => self.bind_local(*local, poison, BindMode::Move),
            PatKind::Range { lo, hi, .. } => {
                self.pat_blind(*lo);
                self.pat_blind(*hi);
            }
            PatKind::Variant { payload, .. } => self.pat_blind_children(payload),
            PatKind::Tuple(elems) => {
                for e in elems {
                    self.pat_blind(*e);
                }
            }
            PatKind::Struct { fields, .. } => {
                for f in fields {
                    self.pat_blind(f.pattern);
                }
            }
            _ => {}
        }
        self.record(p, poison, 0, PatRes::None);
    }

    /// Whether the pattern or a pattern inside it was reported.
    pub(crate) fn pat_failed(&self, p: PatId) -> bool {
        if self.failed_pats.contains(&p) {
            return true;
        }
        match &self.hir.pat(p).kind {
            PatKind::Range { lo, hi, .. } => self.pat_failed(*lo) || self.pat_failed(*hi),
            PatKind::Variant { payload, .. } => payload.is_some_and(|s| self.pat_failed(s)),
            PatKind::Tuple(elems) => elems.iter().any(|e| self.pat_failed(*e)),
            PatKind::Struct { fields, .. } => fields.iter().any(|f| self.pat_failed(f.pattern)),
            PatKind::Poison => true,
            _ => false,
        }
    }

    fn pat_blind_children(&mut self, payload: &Option<PatId>) {
        if let Some(sub) = payload {
            self.pat_blind(*sub);
        }
    }

    /// Whether `ItemSig::Enum` — kept for symmetry with the other helpers.
    #[allow(dead_code)]
    fn is_user_enum(&self, t: TypeId) -> bool {
        matches!(self.cx.d.types.kind(t), TyKind::Adt(d, _) if matches!(self.cx.d.items.get(d), Some(ItemSig::Enum(_))))
    }
}
