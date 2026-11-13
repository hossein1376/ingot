//! Interfaces and impls: matching a type against an impl's target, the per-type tables
//! that method lookup (pipeline section 4.4) reads in phase 7, and the question "does
//! this type satisfy this bound".

use crate::defs::DefId;
use crate::sigs::*;
use crate::ty::*;
use std::collections::{BTreeMap, BTreeSet};

/// An impl that applies to a type, with the arguments of the impl's own generic
/// parameters that make it apply.
#[derive(Clone, Debug)]
pub struct ImplMatch {
    pub impl_def: DefId,
    pub subst: Subst,
}

/// Where the function a method call resolves to comes from.
#[derive(Clone, Debug)]
pub enum MethodSource {
    /// `impl T { ... }`.
    Inherent(ImplMatch),
    /// `impl I for T { ... }`, written in a program. The function is the impl's own, or
    /// the interface's when the impl leaves out a function that has a default body.
    Impl {
        interface: DefId,
        args: Vec<GenericArg>,
        matched: ImplMatch,
    },
    /// The language's own implementation of a built-in interface for a built-in type
    /// (specification section 4.4: integers, `bool`, raw pointers and payload-free
    /// enums implement `Eq`, `Ord` and `Hash`, floats `Eq` and `Ord`, scalars, vectors
    /// and `[]u8` `Format`). The function is the one the interface declares; it has no
    /// body, and code generation supplies the implementation.
    Builtin { interface: DefId },
    /// A bound of a type parameter: the call is resolved when the generic is
    /// instantiated.
    Bound {
        interface: DefId,
        args: Vec<GenericArg>,
    },
}

/// A function that `value.name(...)` can mean for a receiver of a given type.
#[derive(Clone, Debug)]
pub struct MethodCandidate {
    pub method: DefId,
    pub source: MethodSource,
}

impl Declarations {
    pub fn head_of(&self, ty: TypeId) -> Head {
        match self.types.kind(ty) {
            TyKind::Int(i) => Head::Int(*i),
            TyKind::Float(f) => Head::Float(f.bits()),
            TyKind::Bool => Head::Bool,
            TyKind::Void => Head::Void,
            TyKind::Never => Head::Never,
            TyKind::Array(..) => Head::Array,
            TyKind::Tuple(ts) => Head::Tuple(ts.len()),
            TyKind::Adt(d, _) => Head::Adt(*d),
            TyKind::Optional(_) => Head::Optional,
            TyKind::Result(..) => Head::Result,
            TyKind::Ref(m, _) => Head::Ref(*m == Mutability::Mut),
            TyKind::Slice(m, _) => Head::Slice(*m == Mutability::Mut),
            TyKind::RawPtr(m, _) => Head::RawPtr(*m == Mutability::Mut),
            TyKind::FnPtr(_) => Head::FnPtr,
            TyKind::Dyn(_, d, _) => Head::Dyn(*d),
            TyKind::DynFn(..) => Head::DynFn,
            TyKind::Vector(..) => Head::Vector,
            TyKind::Mask(_) => Head::Mask,
            TyKind::Param(_) | TyKind::Projection(..) | TyKind::Poison => Head::Other,
        }
    }

    /// Put a usable impl into the tables.
    pub fn register_impl(&mut self, def: DefId) {
        let Some(sig) = self.impl_sig(def).cloned() else {
            return;
        };
        if sig.poisoned {
            return;
        }
        let head = self.head_of(sig.target);
        self.impls.all.push(def);
        match &sig.of {
            ImplOf::Inherent => self.impls.inherent.entry(head).or_default().push(def),
            ImplOf::Interface(i, _) => {
                self.impls.by_interface.entry(*i).or_default().push(def);
                self.impls.by_head.entry(head).or_default().push(def);
            }
            // Only an `unsafe impl` states a property directly (section 20). `impl Send`
            // without `unsafe` is E1303, reported with the other `unsafe` rules; it
            // grants nothing.
            ImplOf::Prop(p) => {
                if sig.unsafe_ {
                    self.impls.props.entry((*p, head)).or_default().push(def);
                }
            }
        }
    }

    // ---------------------------------------------------------------- matching

    /// Match `pattern` (which may mention the `binders`) against the type `ty`,
    /// extending `s`. One-way: `ty` is never changed.
    pub fn match_ty(&self, pattern: TypeId, ty: TypeId, binders: &[DefId], s: &mut Subst) -> bool {
        let pk = self.types.kind(pattern);
        if let TyKind::Param(p) = pk
            && binders.contains(p)
        {
            return match s.get(*p) {
                Some(GenericArg::Ty(bound)) => *bound == ty,
                Some(_) => false,
                None => {
                    s.insert(*p, GenericArg::Ty(ty));
                    true
                }
            };
        }
        if pattern == ty {
            return true;
        }
        let tk = self.types.kind(ty);
        match (pk, tk) {
            (TyKind::Array(a, n), TyKind::Array(b, m)) => {
                self.match_ty(*a, *b, binders, s) && self.match_const(*n, *m, binders, s)
            }
            (TyKind::Tuple(a), TyKind::Tuple(b)) if a.len() == b.len() => a
                .iter()
                .zip(b)
                .all(|(x, y)| self.match_ty(*x, *y, binders, s)),
            (TyKind::Adt(d, a), TyKind::Adt(e, b)) if d == e => self.match_args(a, b, binders, s),
            (TyKind::Dyn(m, d, a), TyKind::Dyn(n, e, b)) if d == e && m == n => {
                self.match_args(a, b, binders, s)
            }
            (TyKind::Optional(a), TyKind::Optional(b)) => self.match_ty(*a, *b, binders, s),
            (TyKind::Result(a, b), TyKind::Result(c, d)) => {
                self.match_ty(*a, *c, binders, s) && self.match_ty(*b, *d, binders, s)
            }
            (TyKind::Ref(m, a), TyKind::Ref(n, b))
            | (TyKind::Slice(m, a), TyKind::Slice(n, b))
            | (TyKind::RawPtr(m, a), TyKind::RawPtr(n, b))
                if m == n =>
            {
                self.match_ty(*a, *b, binders, s)
            }
            (TyKind::FnPtr(a), TyKind::FnPtr(b)) => self.match_sig(a, b, binders, s),
            (TyKind::DynFn(m, k, a), TyKind::DynFn(n, l, b)) if m == n && k == l => {
                self.match_sig(a, b, binders, s)
            }
            (TyKind::Projection(a, d), TyKind::Projection(b, e)) if d == e => {
                self.match_ty(*a, *b, binders, s)
            }
            _ => false,
        }
    }

    fn match_sig(&self, a: &FnPtrSig, b: &FnPtrSig, binders: &[DefId], s: &mut Subst) -> bool {
        a.ext == b.ext
            && a.params.len() == b.params.len()
            && a.params
                .iter()
                .zip(&b.params)
                .all(|(x, y)| self.match_ty(*x, *y, binders, s))
            && self.match_ty(a.ret, b.ret, binders, s)
            && match (a.error, b.error) {
                (None, None) => true,
                (Some(x), Some(y)) => self.match_ty(x, y, binders, s),
                _ => false,
            }
    }

    fn match_const(
        &self,
        pattern: ConstArg,
        c: ConstArg,
        binders: &[DefId],
        s: &mut Subst,
    ) -> bool {
        if let ConstArg::Param(p) = pattern
            && binders.contains(&p)
        {
            return match s.get(p) {
                Some(GenericArg::Const(bound)) => *bound == c,
                Some(_) => false,
                None => {
                    s.insert(p, GenericArg::Const(c));
                    true
                }
            };
        }
        pattern == c
    }

    pub fn match_args(
        &self,
        a: &[GenericArg],
        b: &[GenericArg],
        binders: &[DefId],
        s: &mut Subst,
    ) -> bool {
        a.len() == b.len()
            && a.iter().zip(b).all(|(x, y)| match (x, y) {
                (GenericArg::Ty(x), GenericArg::Ty(y)) => self.match_ty(*x, *y, binders, s),
                (GenericArg::Const(x), GenericArg::Const(y)) => {
                    self.match_const(*x, *y, binders, s)
                }
                _ => false,
            })
    }

    // ---------------------------------------------------------------- unification

    /// Whether some type is matched by both `a` and `b`, where the parameters in
    /// `vars` are variables on either side (the overlap test of E0507).
    pub fn unifiable_args(
        &self,
        a: &[GenericArg],
        b: &[GenericArg],
        vars: &BTreeSet<DefId>,
    ) -> bool {
        let mut u = Unifier {
            decls: self,
            vars,
            ty: BTreeMap::new(),
            cn: BTreeMap::new(),
        };
        u.args(a, b, 0)
    }
}

struct Unifier<'a> {
    decls: &'a Declarations,
    vars: &'a BTreeSet<DefId>,
    ty: BTreeMap<DefId, TypeId>,
    cn: BTreeMap<DefId, ConstArg>,
}

const UNIFY_DEPTH: u32 = 64;

impl Unifier<'_> {
    fn walk(&self, mut t: TypeId) -> TypeId {
        for _ in 0..UNIFY_DEPTH {
            match self.decls.types.kind(t) {
                TyKind::Param(p) if self.vars.contains(p) => match self.ty.get(p) {
                    Some(next) => t = *next,
                    None => return t,
                },
                _ => return t,
            }
        }
        t
    }

    fn walk_const(&self, mut c: ConstArg) -> ConstArg {
        for _ in 0..UNIFY_DEPTH {
            match c {
                ConstArg::Param(p) if self.vars.contains(&p) => match self.cn.get(&p) {
                    Some(next) => c = *next,
                    None => return c,
                },
                _ => return c,
            }
        }
        c
    }

    fn occurs(&self, var: DefId, t: TypeId, depth: u32) -> bool {
        if depth > UNIFY_DEPTH {
            return true;
        }
        let t = self.walk(t);
        if let TyKind::Param(p) = self.decls.types.kind(t)
            && *p == var
        {
            return true;
        }
        self.decls
            .types
            .children(t)
            .into_iter()
            .any(|c| self.occurs(var, c, depth + 1))
    }

    fn args(&mut self, a: &[GenericArg], b: &[GenericArg], depth: u32) -> bool {
        a.len() == b.len()
            && a.iter().zip(b).all(|(x, y)| match (x, y) {
                (GenericArg::Ty(x), GenericArg::Ty(y)) => self.unify(*x, *y, depth),
                (GenericArg::Const(x), GenericArg::Const(y)) => self.unify_const(*x, *y),
                _ => false,
            })
    }

    fn unify_const(&mut self, a: ConstArg, b: ConstArg) -> bool {
        let a = self.walk_const(a);
        let b = self.walk_const(b);
        if a == b {
            return true;
        }
        match (a, b) {
            (ConstArg::Param(p), c) if self.vars.contains(&p) => {
                self.cn.insert(p, c);
                true
            }
            (c, ConstArg::Param(p)) if self.vars.contains(&p) => {
                self.cn.insert(p, c);
                true
            }
            _ => false,
        }
    }

    fn unify(&mut self, a: TypeId, b: TypeId, depth: u32) -> bool {
        if depth > UNIFY_DEPTH {
            return false;
        }
        let a = self.walk(a);
        let b = self.walk(b);
        if a == b {
            return true;
        }
        let (ak, bk) = (
            self.decls.types.kind(a).clone(),
            self.decls.types.kind(b).clone(),
        );
        if let TyKind::Param(p) = ak
            && self.vars.contains(&p)
        {
            if self.occurs(p, b, depth) {
                return false;
            }
            self.ty.insert(p, b);
            return true;
        }
        if let TyKind::Param(p) = bk
            && self.vars.contains(&p)
        {
            if self.occurs(p, a, depth) {
                return false;
            }
            self.ty.insert(p, a);
            return true;
        }
        let d = depth + 1;
        match (ak, bk) {
            (TyKind::Array(x, n), TyKind::Array(y, m)) => {
                self.unify(x, y, d) && self.unify_const(n, m)
            }
            (TyKind::Tuple(x), TyKind::Tuple(y)) if x.len() == y.len() => {
                x.iter().zip(&y).all(|(p, q)| self.unify(*p, *q, d))
            }
            (TyKind::Adt(e, x), TyKind::Adt(f, y)) if e == f => self.args(&x, &y, d),
            (TyKind::Dyn(m, e, x), TyKind::Dyn(n, f, y)) if e == f && m == n => {
                self.args(&x, &y, d)
            }
            (TyKind::Optional(x), TyKind::Optional(y)) => self.unify(x, y, d),
            (TyKind::Result(x1, x2), TyKind::Result(y1, y2)) => {
                self.unify(x1, y1, d) && self.unify(x2, y2, d)
            }
            (TyKind::Ref(m, x), TyKind::Ref(n, y))
            | (TyKind::Slice(m, x), TyKind::Slice(n, y))
            | (TyKind::RawPtr(m, x), TyKind::RawPtr(n, y))
                if m == n =>
            {
                self.unify(x, y, d)
            }
            (TyKind::FnPtr(x), TyKind::FnPtr(y)) => self.sig(&x, &y, d),
            (TyKind::DynFn(m, k, x), TyKind::DynFn(n, l, y)) if m == n && k == l => {
                self.sig(&x, &y, d)
            }
            (TyKind::Projection(x, e), TyKind::Projection(y, f)) if e == f => self.unify(x, y, d),
            _ => false,
        }
    }

    fn sig(&mut self, a: &FnPtrSig, b: &FnPtrSig, d: u32) -> bool {
        a.ext == b.ext
            && a.params.len() == b.params.len()
            && a.params
                .iter()
                .zip(&b.params)
                .all(|(x, y)| self.unify(*x, *y, d))
            && self.unify(a.ret, b.ret, d)
            && match (a.error, b.error) {
                (None, None) => true,
                (Some(x), Some(y)) => self.unify(x, y, d),
                _ => false,
            }
    }
}

// ------------------------------------------------------------------- queries

impl Declarations {
    /// The impls (inherent) that apply to `ty`, with the substitution for each impl's
    /// generics. Their bounds are not checked.
    pub fn inherent_impls(&self, ty: TypeId) -> Vec<ImplMatch> {
        let head = self.head_of(ty);
        let mut out = Vec::new();
        if head == Head::Other {
            return out;
        }
        for &i in self.impls.inherent.get(&head).into_iter().flatten() {
            if let Some(m) = self.match_impl(i, ty) {
                out.push(m);
            }
        }
        out
    }

    /// Every interface impl that applies to `ty`: (interface, its arguments, match).
    pub fn interface_impls(&mut self, ty: TypeId) -> Vec<(DefId, Vec<GenericArg>, ImplMatch)> {
        let head = self.head_of(ty);
        let mut out = Vec::new();
        if head == Head::Other {
            return out;
        }
        let candidates: Vec<DefId> = self.impls.by_head.get(&head).cloned().unwrap_or_default();
        for i in candidates {
            let Some(m) = self.match_impl(i, ty) else {
                continue;
            };
            let Some(ImplOf::Interface(iface, args)) = self.impl_sig(i).map(|s| s.of.clone())
            else {
                continue;
            };
            let args = args
                .iter()
                .map(|a| self.types.subst_arg(a, &m.subst))
                .collect();
            out.push((iface, args, m));
        }
        out
    }

    /// Match the target of impl `i` against `ty`.
    pub fn match_impl(&self, i: DefId, ty: TypeId) -> Option<ImplMatch> {
        let sig = self.impl_sig(i)?;
        let binders: Vec<DefId> = self.generics_of(i).iter().map(|g| g.def).collect();
        let mut subst = Subst::new();
        if self.match_ty(sig.target, ty, &binders, &mut subst) {
            Some(ImplMatch { impl_def: i, subst })
        } else {
            None
        }
    }

    /// Whether the generics of impl `m` are satisfied by its match.
    pub fn impl_bounds_hold(&mut self, m: &ImplMatch) -> Tri {
        let generics: Vec<GenericSig> = self.generics_of(m.impl_def).to_vec();
        let mut result = Tri::Yes;
        for g in &generics {
            let Some(arg) = m.subst.get(g.def).cloned() else {
                // A parameter the target does not mention is not decided by matching.
                continue;
            };
            let GenericArg::Ty(ty) = arg else {
                continue;
            };
            for b in &g.bounds {
                let b = self.subst_bound(b, &m.subst);
                result = result.and(self.satisfies(ty, &b));
            }
        }
        result
    }

    pub fn subst_bound(&mut self, b: &Bound, s: &Subst) -> Bound {
        match b {
            Bound::Prop(_) | Bound::Poison => b.clone(),
            Bound::Fn(k, sig) => Bound::Fn(*k, self.subst_sig(sig, s)),
            Bound::Interface(d, args) => Bound::Interface(
                *d,
                args.iter().map(|a| self.types.subst_arg(a, s)).collect(),
            ),
        }
    }

    pub fn subst_sig(&mut self, sig: &FnPtrSig, s: &Subst) -> FnPtrSig {
        FnPtrSig {
            ext: sig.ext,
            params: sig.params.iter().map(|t| self.types.subst(*t, s)).collect(),
            ret: self.types.subst(sig.ret, s),
            error: sig.error.map(|t| self.types.subst(t, s)),
        }
    }

    /// `subst`, then replace the projections that the impls can now decide.
    pub fn instantiate(&mut self, ty: TypeId, s: &Subst) -> TypeId {
        let t = self.types.subst(ty, s);
        self.normalize(t)
    }

    /// Replace `Base.Item` by the type the impl of the interface for `Base` gives.
    pub fn normalize(&mut self, ty: TypeId) -> TypeId {
        let mut reduce: Vec<(TypeId, DefId)> = Vec::new();
        let mut seen = Vec::new();
        collect_projections(&self.types, ty, &mut reduce, &mut seen);
        if reduce.is_empty() {
            return ty;
        }
        let mut replacements: BTreeMap<TypeId, TypeId> = BTreeMap::new();
        for (proj, _) in reduce.clone() {
            if let TyKind::Projection(base, assoc) = self.types.kind(proj).clone()
                && let Some(r) = self.project(base, assoc)
            {
                replacements.insert(proj, r);
            }
        }
        if replacements.is_empty() {
            return ty;
        }
        self.types
            .fold(ty, &mut |_, t| replacements.get(&t).copied())
    }

    /// The type `base.assoc` is, when an impl for `base` declares it.
    fn project(&mut self, base: TypeId, assoc: DefId) -> Option<TypeId> {
        if matches!(
            self.types.kind(base),
            TyKind::Param(_) | TyKind::Projection(..) | TyKind::Poison
        ) {
            return None;
        }
        let (iface, name) = self.assoc_info.get(&assoc)?.clone();
        for (i, _, m) in self.interface_impls(base) {
            if i != iface {
                continue;
            }
            let imp = self.impl_sig(m.impl_def)?;
            let alias = *imp.assoc.get(&name)?;
            if let Some(ItemSig::Alias(a)) = self.items.get(&alias)
                && let Some(t) = a.ty
            {
                return Some(self.types.subst(t, &m.subst));
            }
        }
        None
    }

    // ------------------------------------------------------------------ bounds

    /// The properties granted to the type parameter `p` by its bounds.
    pub fn granted(&self, p: DefId) -> BTreeSet<PropBound> {
        let mut out = BTreeSet::new();
        let mut seen = BTreeSet::new();
        self.grant_from(p, &mut out, &mut seen);
        out
    }

    fn grant_from(&self, p: DefId, out: &mut BTreeSet<PropBound>, seen: &mut BTreeSet<DefId>) {
        for b in self.param_bounds.get(&p).into_iter().flatten() {
            self.grant_bound(b, out, seen);
        }
    }

    fn grant_bound(&self, b: &Bound, out: &mut BTreeSet<PropBound>, seen: &mut BTreeSet<DefId>) {
        match b {
            Bound::Prop(pb) => {
                out.insert(*pb);
                // Only scalars satisfy `Int`, `SInt`, `UInt`, `Float` and `Num`, so each
                // implies `Copy`, `Discard`, `Owned`, `Send` and `Sync` (section 8).
                if matches!(
                    pb,
                    PropBound::Int
                        | PropBound::SInt
                        | PropBound::UInt
                        | PropBound::Float
                        | PropBound::Num
                ) {
                    out.insert(PropBound::Copy);
                    out.insert(PropBound::Owned);
                    out.insert(PropBound::Send);
                    out.insert(PropBound::Sync);
                }
                if *pb == PropBound::Copy || out.contains(&PropBound::Copy) {
                    out.insert(PropBound::Discard);
                }
            }
            Bound::Interface(i, _) => {
                if !seen.insert(*i) {
                    return;
                }
                if let Some(sig) = self.interface_sig(*i) {
                    for s in &sig.supers {
                        self.grant_bound(s, out, seen);
                    }
                }
            }
            Bound::Fn(..) | Bound::Poison => {}
        }
    }

    /// Whether `ty` satisfies `bound`. `Unknown`: the specification does not say, and
    /// the caller records it as not implemented.
    pub fn satisfies(&mut self, ty: TypeId, bound: &Bound) -> Tri {
        if self.types.has_poison(ty) {
            return Tri::Unknown;
        }
        match bound {
            Bound::Poison => Tri::Unknown,
            Bound::Prop(pb) => self.satisfies_prop(ty, *pb),
            Bound::Fn(kind, sig) => self.satisfies_fn(ty, *kind, sig),
            Bound::Interface(i, args) => self.satisfies_interface(ty, *i, args),
        }
    }

    pub fn satisfies_prop(&mut self, ty: TypeId, pb: PropBound) -> Tri {
        match pb {
            PropBound::Copy
            | PropBound::Discard
            | PropBound::Send
            | PropBound::Sync
            | PropBound::Owned => {
                let p = self.props(ty);
                match pb {
                    PropBound::Copy => p.copy,
                    PropBound::Discard => p.discard(),
                    PropBound::Send => p.send,
                    PropBound::Sync => p.sync,
                    _ => p.owned(),
                }
            }
            PropBound::Int
            | PropBound::SInt
            | PropBound::UInt
            | PropBound::Float
            | PropBound::Num => match self.types.kind(ty) {
                TyKind::Param(p) => {
                    let g = self.granted(*p);
                    // `Int`, `SInt`, `UInt`, `Float` and `Num` are each satisfied by
                    // exactly the bound of that name; `Num` also by `Int`/`Float`.
                    Tri::from_bool(
                        g.contains(&pb)
                            || (pb == PropBound::Num
                                && (g.contains(&PropBound::Int)
                                    || g.contains(&PropBound::SInt)
                                    || g.contains(&PropBound::UInt)
                                    || g.contains(&PropBound::Float)))
                            || (pb == PropBound::Int
                                && (g.contains(&PropBound::SInt) || g.contains(&PropBound::UInt))),
                    )
                }
                TyKind::Int(i) => Tri::from_bool(match pb {
                    PropBound::SInt => i.signed(),
                    PropBound::UInt => !i.signed(),
                    PropBound::Float => false,
                    _ => true,
                }),
                TyKind::Float(_) => Tri::from_bool(matches!(pb, PropBound::Float | PropBound::Num)),
                TyKind::Projection(..) => Tri::No,
                _ => Tri::No,
            },
        }
    }

    fn satisfies_fn(&mut self, ty: TypeId, kind: FnKind, sig: &FnPtrSig) -> Tri {
        if let TyKind::Param(p) = self.types.kind(ty).clone() {
            let has = self
                .param_bounds
                .get(&p)
                .into_iter()
                .flatten()
                .any(|b| matches!(b, Bound::Fn(k, s) if *k == kind && s == sig));
            return Tri::from_bool(has);
        }
        Tri::Unknown
    }

    fn satisfies_interface(&mut self, ty: TypeId, iface: DefId, args: &[GenericArg]) -> Tri {
        match self.types.kind(ty).clone() {
            TyKind::Param(p) => {
                let mut seen = BTreeSet::new();
                return Tri::from_bool(self.param_has_interface(p, iface, args, &mut seen));
            }
            TyKind::Projection(..) => return Tri::No,
            TyKind::Dyn(_, d, a) if d == iface && a == args => return Tri::Yes,
            _ => {}
        }
        let mut result = Tri::No;
        for (i, a, m) in self.interface_impls(ty) {
            if i == iface && a == args {
                result = result.or(self.impl_bounds_hold(&m));
            }
        }
        if result != Tri::No {
            return result;
        }
        if args.is_empty() {
            self.builtin_implements(ty, iface)
        } else {
            Tri::No
        }
    }

    /// Whether the language itself implements the built-in interface `iface` for `ty`
    /// (specification section 4.4), with no `impl` written. `No` for a user-declared
    /// interface, and for a built-in one the section does not list `ty` under.
    ///
    /// `Unknown`: the section says "every scalar, vector and slice of `u8` implements
    /// `Format`" and does not say whether a mask `boolxN` is a vector for this, nor
    /// whether `[]mut u8` is a slice of `u8`.
    pub fn builtin_implements(&self, ty: TypeId, iface: DefId) -> Tri {
        let b = self.builtins;
        let is = |d: Option<DefId>| d == Some(iface);
        if is(b.eq) || is(b.ord) || is(b.hash) {
            let hash = is(b.hash);
            return match self.types.kind(ty) {
                TyKind::Int(_) | TyKind::Bool | TyKind::RawPtr(..) => Tri::Yes,
                // The IEEE total order: `Eq` and `Ord`, not `Hash`.
                TyKind::Float(_) => Tri::from_bool(!hash),
                TyKind::Adt(d, _) => Tri::from_bool(self.is_payload_free_enum(*d)),
                _ => Tri::No,
            };
        }
        if is(b.format) {
            return match self.types.kind(ty) {
                TyKind::Int(_) | TyKind::Float(_) | TyKind::Bool | TyKind::Vector(..) => Tri::Yes,
                TyKind::Mask(_) => Tri::Unknown,
                TyKind::Slice(m, e) if matches!(self.types.kind(*e), TyKind::Int(IntTy::U8)) => {
                    if *m == Mutability::Shared {
                        Tri::Yes
                    } else {
                        Tri::Unknown
                    }
                }
                _ => Tri::No,
            };
        }
        Tri::No
    }

    /// An enum none of whose variants has a payload.
    pub fn is_payload_free_enum(&self, def: DefId) -> bool {
        matches!(
            self.items.get(&def),
            Some(ItemSig::Enum(e)) if e.variants.iter().all(|v| v.payload.is_none())
        )
    }

    /// The built-in interfaces the language implements for `ty` (the `Yes` answers of
    /// [`Declarations::builtin_implements`]).
    pub fn builtin_interface_impls(&self, ty: TypeId) -> Vec<DefId> {
        let b = self.builtins;
        [b.eq, b.ord, b.hash, b.format]
            .into_iter()
            .flatten()
            .filter(|i| self.builtin_implements(ty, *i) == Tri::Yes)
            .collect()
    }

    /// The function called `name` that interface `iface` declares.
    pub fn interface_method(&self, iface: DefId, name: &str) -> Option<DefId> {
        self.interface_sig(iface)?
            .methods
            .iter()
            .copied()
            .find(|m| self.fn_sig(*m).is_some_and(|f| f.name == name))
    }

    /// Every function `value.name(...)` can mean for a receiver of type `ty` (pipeline
    /// section 4.4): the inherent impls first, then the interfaces `ty` implements,
    /// whether by an impl or by the language (`MethodSource::Builtin`), or, for a type
    /// parameter, the interfaces its bounds name. More than one interface candidate
    /// is the ambiguity the call has to resolve by writing `I.name(value, args)`.
    pub fn method_candidates(&mut self, ty: TypeId, name: &str) -> Vec<MethodCandidate> {
        let mut out = Vec::new();
        for m in self.inherent_impls(ty) {
            if let Some(&f) = self.impl_sig(m.impl_def).and_then(|s| s.methods.get(name)) {
                out.push(MethodCandidate {
                    method: f,
                    source: MethodSource::Inherent(m),
                });
            }
        }
        if let TyKind::Param(p) = self.types.kind(ty).clone() {
            let mut seen = BTreeSet::new();
            let mut found = Vec::new();
            for b in self.param_bounds.get(&p).cloned().unwrap_or_default() {
                self.bound_interfaces(&b, &mut seen, &mut found);
            }
            for (iface, args) in found {
                if let Some(f) = self.interface_method(iface, name) {
                    out.push(MethodCandidate {
                        method: f,
                        source: MethodSource::Bound {
                            interface: iface,
                            args,
                        },
                    });
                }
            }
            return out;
        }
        for (iface, args, m) in self.interface_impls(ty) {
            let own = self
                .impl_sig(m.impl_def)
                .and_then(|s| s.methods.get(name))
                .copied();
            if let Some(f) = own.or_else(|| self.interface_method(iface, name)) {
                out.push(MethodCandidate {
                    method: f,
                    source: MethodSource::Impl {
                        interface: iface,
                        args,
                        matched: m,
                    },
                });
            }
        }
        for iface in self.builtin_interface_impls(ty) {
            if let Some(f) = self.interface_method(iface, name) {
                out.push(MethodCandidate {
                    method: f,
                    source: MethodSource::Builtin { interface: iface },
                });
            }
        }
        out
    }

    /// The interfaces a bound names, with their supertraits, each once.
    fn bound_interfaces(
        &self,
        b: &Bound,
        seen: &mut BTreeSet<DefId>,
        out: &mut Vec<(DefId, Vec<GenericArg>)>,
    ) {
        if let Bound::Interface(i, args) = b
            && seen.insert(*i)
        {
            out.push((*i, args.clone()));
            if let Some(sig) = self.interface_sig(*i) {
                for s in &sig.supers {
                    self.bound_interfaces(s, seen, out);
                }
            }
        }
    }

    fn param_has_interface(
        &self,
        p: DefId,
        iface: DefId,
        args: &[GenericArg],
        seen: &mut BTreeSet<DefId>,
    ) -> bool {
        for b in self.param_bounds.get(&p).into_iter().flatten() {
            if self.bound_has_interface(b, iface, args, seen) {
                return true;
            }
        }
        false
    }

    fn bound_has_interface(
        &self,
        b: &Bound,
        iface: DefId,
        args: &[GenericArg],
        seen: &mut BTreeSet<DefId>,
    ) -> bool {
        if let Bound::Interface(i, a) = b {
            if *i == iface && a == args {
                return true;
            }
            if seen.insert(*i)
                && let Some(sig) = self.interface_sig(*i)
            {
                return sig
                    .supers
                    .iter()
                    .any(|s| self.bound_has_interface(s, iface, args, seen));
            }
        }
        false
    }
}

fn collect_projections(
    types: &TypeTable,
    ty: TypeId,
    out: &mut Vec<(TypeId, DefId)>,
    seen: &mut Vec<TypeId>,
) {
    if seen.contains(&ty) {
        return;
    }
    seen.push(ty);
    if let TyKind::Projection(_, d) = types.kind(ty) {
        out.push((ty, *d));
    }
    for c in types.children(ty) {
        collect_projections(types, c, out, seen);
    }
}
