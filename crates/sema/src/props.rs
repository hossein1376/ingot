//! Type properties (spec sections 10 and 20, pipeline section 4.5).
//!
//! `Copy`, linear, view, `Send` and `Sync` are computed once per type from its parts.
//! A recursive type (through a borrow, a generic argument, a raw pointer) is handled by
//! a fixed point over the graph of types reachable from the one asked about: the
//! positive properties start as "holds" and only decrease, the negative ones (linear,
//! view) start as "does not" and only increase.

use crate::sigs::*;
use crate::ty::*;
use std::collections::{BTreeMap, BTreeSet};

/// More types than this in one component means unbounded polymorphic recursion
/// (`struct S[T] { r: &S[?T] }`): the properties are not computed.
const NODE_LIMIT: usize = 4096;

/// What an ADT is made of, for its properties.
struct AdtParts {
    parts: Vec<TypeId>,
    /// Declared `copy` (a `copy struct`, a `copy enum`).
    copy_declared: bool,
    /// Declared `linear`.
    linear_declared: bool,
}

impl Declarations {
    /// The return type is written with a borrow: `&`, `[]`, `&dyn`, a view struct, or
    /// one of those inside a tuple, array, optional or result. A type parameter and an
    /// associated type stop the walk (they are not themselves written borrows).
    pub fn written_with_borrow(&mut self, ty: TypeId) -> bool {
        fn go(d: &mut Declarations, ty: TypeId, seen: &mut Vec<TypeId>) -> bool {
            if seen.contains(&ty) {
                return false;
            }
            seen.push(ty);
            match d.types.kind(ty).clone() {
                TyKind::Ref(..) | TyKind::Slice(..) | TyKind::Dyn(..) | TyKind::DynFn(..) => true,
                TyKind::Param(_) | TyKind::Projection(..) => false,
                TyKind::Adt(_, _) | TyKind::Closure(..) => {
                    if d.props(ty).view == Tri::Yes {
                        return true;
                    }
                    let kids = d.types.children(ty);
                    kids.into_iter().any(|c| go(d, c, seen))
                }
                _ => {
                    let kids = d.types.children(ty);
                    kids.into_iter().any(|c| go(d, c, seen))
                }
            }
        }
        go(self, ty, &mut Vec::new())
    }

    /// The properties of `ty`, computed once.
    pub fn props(&mut self, ty: TypeId) -> Props {
        if let Some(p) = self.props.get(&ty) {
            return *p;
        }
        let mut nodes: Vec<TypeId> = Vec::new();
        let mut seen: BTreeSet<TypeId> = BTreeSet::new();
        let mut stack = vec![ty];
        while let Some(t) = stack.pop() {
            if self.props.contains_key(&t) || !seen.insert(t) {
                continue;
            }
            if seen.len() > NODE_LIMIT {
                self.gave_up = true;
                for n in seen {
                    self.props.insert(n, Props::unknown());
                }
                return Props::unknown();
            }
            stack.extend(self.prop_deps(t));
            nodes.push(t);
        }
        nodes.sort();
        let mut cur: BTreeMap<TypeId, Props> =
            nodes.iter().map(|t| (*t, Props::optimistic())).collect();
        loop {
            let mut changed = false;
            for &t in &nodes {
                let p = self.prop_rule(t, &cur);
                if cur[&t] != p {
                    cur.insert(t, p);
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        self.props.extend(cur);
        self.props[&ty]
    }

    fn lookup(&self, cur: &BTreeMap<TypeId, Props>, t: TypeId) -> Props {
        cur.get(&t)
            .or_else(|| self.props.get(&t))
            .copied()
            .unwrap_or_else(Props::unknown)
    }

    /// The parts of a struct or enum instance, substituted.
    fn adt_parts(&mut self, def: crate::defs::DefId, args: &[GenericArg]) -> Option<AdtParts> {
        let s = self.subst_for(def, args);
        match self.items.get(&def)? {
            ItemSig::Struct(st) => {
                let kind = st.kind;
                let tys: Vec<TypeId> = st.fields.iter().map(|f| f.ty).collect();
                let parts = tys.into_iter().map(|t| self.types.subst(t, &s)).collect();
                Some(AdtParts {
                    parts,
                    copy_declared: kind == ingot_syntax::ast::StructKind::Copy,
                    linear_declared: kind == ingot_syntax::ast::StructKind::Linear,
                })
            }
            ItemSig::Enum(en) => {
                let copy = en.copy;
                let tys: Vec<TypeId> = en.variants.iter().filter_map(|v| v.payload).collect();
                let parts = tys.into_iter().map(|t| self.types.subst(t, &s)).collect();
                Some(AdtParts {
                    parts,
                    copy_declared: copy,
                    linear_declared: false,
                })
            }
            _ => None,
        }
    }

    /// The types a struct or enum instance is made of: its fields' or payloads' types
    /// with the arguments substituted.
    pub fn adt_part_types(
        &mut self,
        def: crate::defs::DefId,
        args: &[GenericArg],
    ) -> Option<Vec<TypeId>> {
        self.adt_parts(def, args).map(|p| p.parts)
    }

    /// The types whose properties `t`'s properties are computed from.
    fn prop_deps(&mut self, t: TypeId) -> Vec<TypeId> {
        match self.types.kind(t).clone() {
            TyKind::Adt(def, args) => {
                let mut out = Vec::new();
                if let Some(p) = self.adt_parts(def, &args) {
                    out.extend(p.parts);
                }
                // An `unsafe impl Send/Sync` with bounds reads the arguments.
                let head = Head::Adt(def);
                let has_impl = [PropBound::Send, PropBound::Sync]
                    .iter()
                    .any(|pb| self.impls.props.contains_key(&(*pb, head.clone())));
                if has_impl {
                    for a in &args {
                        if let GenericArg::Ty(x) = a {
                            out.push(*x);
                        }
                    }
                }
                out
            }
            TyKind::Param(_) | TyKind::Projection(..) => Vec::new(),
            _ => self.types.children(t),
        }
    }

    fn prop_rule(&mut self, t: TypeId, cur: &BTreeMap<TypeId, Props>) -> Props {
        let kind = self.types.kind(t).clone();
        let leaf = Props::optimistic();
        match kind {
            TyKind::Int(_)
            | TyKind::Float(_)
            | TyKind::Bool
            | TyKind::Void
            | TyKind::Never
            | TyKind::Vector(..)
            | TyKind::Mask(_)
            | TyKind::FnPtr(_) => leaf,
            TyKind::Array(e, _) | TyKind::Optional(e) => {
                let p = self.lookup(cur, e);
                self.combine(&[p])
            }
            TyKind::Tuple(es) => {
                let ps: Vec<Props> = es.iter().map(|e| self.lookup(cur, *e)).collect();
                self.combine(&ps)
            }
            TyKind::Result(a, b) => {
                let ps = [self.lookup(cur, a), self.lookup(cur, b)];
                let mut p = self.combine(&ps);
                // A result is linear (section 6.2) and is not `Copy`.
                p.copy = Tri::No;
                p.linear = Tri::Yes;
                p.maybe_linear = Tri::Yes;
                p
            }
            TyKind::Ref(m, e) | TyKind::Slice(m, e) => {
                let inner = self.lookup(cur, e);
                Props {
                    copy: Tri::from_bool(m == Mutability::Shared),
                    linear: Tri::No,
                    maybe_linear: Tri::No,
                    view: Tri::Yes,
                    maybe_view: Tri::Yes,
                    send: if m == Mutability::Shared {
                        inner.sync
                    } else {
                        inner.send
                    },
                    sync: inner.sync,
                }
            }
            TyKind::RawPtr(_, e) => {
                let inner = self.lookup(cur, e);
                Props {
                    send: Tri::No,
                    sync: Tri::No,
                    view: inner.view,
                    maybe_view: inner.maybe_view,
                    ..leaf
                }
            }
            // `&dyn I` is `Send`/`Sync` from `I`'s supertraits (section 20); the
            // dynamic `Fn` forms are neither.
            TyKind::Dyn(m, def, _) => {
                let send = self.interface_has_prop(def, PropBound::Send);
                let sync = self.interface_has_prop(def, PropBound::Sync);
                Props {
                    copy: Tri::from_bool(m == Mutability::Shared),
                    view: Tri::Yes,
                    maybe_view: Tri::Yes,
                    send: if m == Mutability::Shared {
                        Tri::from_bool(sync)
                    } else {
                        Tri::from_bool(send)
                    },
                    sync: Tri::from_bool(sync),
                    ..leaf
                }
            }
            TyKind::DynFn(m, ..) => Props {
                copy: Tri::from_bool(m == Mutability::Shared),
                view: Tri::Yes,
                maybe_view: Tri::Yes,
                send: Tri::No,
                sync: Tri::No,
                ..leaf
            },
            TyKind::Param(p) => {
                let g = self.granted(p);
                // A bound that failed to resolve is `Poison`. Linearity is unknown,
                // and unknown is not a linear obligation. `Fn` and `FnMut` grant
                // `Discard` (spec §8), so those parameters are not linear either.
                let poison = self
                    .param_bounds
                    .get(&p)
                    .into_iter()
                    .flatten()
                    .any(|b| matches!(b, Bound::Poison));
                let maybe_linear = if poison {
                    Tri::Unknown
                } else {
                    Tri::from_bool(!g.contains(&PropBound::Discard))
                };
                Props {
                    copy: Tri::from_bool(g.contains(&PropBound::Copy)),
                    linear: Tri::No,
                    maybe_linear,
                    view: Tri::No,
                    maybe_view: Tri::from_bool(!g.contains(&PropBound::Owned)),
                    send: Tri::from_bool(g.contains(&PropBound::Send)),
                    sync: Tri::from_bool(g.contains(&PropBound::Sync)),
                }
            }
            TyKind::Projection(..) => Props {
                copy: Tri::No,
                linear: Tri::No,
                maybe_linear: Tri::Yes,
                view: Tri::No,
                maybe_view: Tri::Yes,
                send: Tri::No,
                sync: Tri::No,
            },
            TyKind::Poison => Props::unknown(),
            // A closure's properties come from its captures and are entered into the
            // table when the closure is typed (phase 7), before any use of the type.
            TyKind::Closure(..) => self.props.get(&t).copied().unwrap_or_else(Props::unknown),
            TyKind::Adt(def, args) => {
                if self.builtins.idx == Some(def) && args.is_empty() {
                    return Props {
                        copy: Tri::Yes,
                        linear: Tri::No,
                        maybe_linear: Tri::No,
                        view: Tri::No,
                        maybe_view: Tri::No,
                        send: Tri::Yes,
                        sync: Tri::Yes,
                    };
                }
                let Some(parts) = self.adt_parts(def, &args) else {
                    // A built-in type without a declaration (`Idx`, `Grid[T]`): its
                    // properties are not defined by the specification.
                    return Props::unknown();
                };
                let ps: Vec<Props> = parts.parts.iter().map(|p| self.lookup(cur, *p)).collect();
                let mut p = self.combine(&ps);
                p.copy = Tri::from_bool(parts.copy_declared).and(p.copy);
                if parts.linear_declared {
                    p.linear = Tri::Yes;
                    p.maybe_linear = Tri::Yes;
                }
                if let Some(s) = self.unsafe_impl(t, PropBound::Send, def, &args, cur) {
                    p.send = s;
                }
                if let Some(s) = self.unsafe_impl(t, PropBound::Sync, def, &args, cur) {
                    p.sync = s;
                }
                // `std.sync.Cell` is never `Sync`: its contents change through a
                // shared borrow (02-memory §13.6).
                if self.builtins.cell == Some(def) {
                    p.sync = Tri::No;
                }
                p
            }
        }
    }

    /// Whether `iface` or one of its superinterfaces is `Send` or `Sync`.
    fn interface_has_prop(&self, iface: crate::DefId, prop: PropBound) -> bool {
        fn go(
            this: &Declarations,
            def: crate::DefId,
            prop: PropBound,
            seen: &mut Vec<crate::DefId>,
        ) -> bool {
            if seen.contains(&def) {
                return false;
            }
            seen.push(def);
            let Some(sig) = this.interface_sig(def) else {
                return false;
            };
            for b in &sig.supers {
                match b {
                    Bound::Prop(p) if *p == prop => return true,
                    Bound::Interface(i, _) if go(this, *i, prop, seen) => return true,
                    _ => {}
                }
            }
            false
        }
        go(self, iface, prop, &mut Vec::new())
    }

    /// The properties of an aggregate from its parts.
    fn combine(&self, parts: &[Props]) -> Props {
        let mut p = Props::optimistic();
        for q in parts {
            p.copy = p.copy.and(q.copy);
            p.linear = p.linear.or(q.linear);
            p.maybe_linear = p.maybe_linear.or(q.maybe_linear);
            p.view = p.view.or(q.view);
            p.maybe_view = p.maybe_view.or(q.maybe_view);
            p.send = p.send.and(q.send);
            p.sync = p.sync.and(q.sync);
        }
        p
    }

    /// What `unsafe impl Send/Sync for ...` says about `ty` directly. `None`: no such
    /// impl exists for this type constructor, so the property is structural.
    fn unsafe_impl(
        &mut self,
        ty: TypeId,
        pb: PropBound,
        def: crate::defs::DefId,
        _args: &[GenericArg],
        cur: &BTreeMap<TypeId, Props>,
    ) -> Option<Tri> {
        let impls: Vec<crate::defs::DefId> = self
            .impls
            .props
            .get(&(pb, Head::Adt(def)))
            .cloned()
            .unwrap_or_default();
        if impls.is_empty() {
            return None;
        }
        let mut result = Tri::No;
        for i in impls {
            let Some(m) = self.match_impl(i, ty) else {
                continue;
            };
            result = result.or(self.impl_bounds_hold_with(&m, cur));
        }
        Some(result)
    }

    /// `impl_bounds_hold`, reading property bounds from the assumptions of a fixed
    /// point in progress.
    fn impl_bounds_hold_with(
        &mut self,
        m: &crate::traits::ImplMatch,
        cur: &BTreeMap<TypeId, Props>,
    ) -> Tri {
        let generics: Vec<GenericSig> = self.generics_of(m.impl_def).to_vec();
        let mut result = Tri::Yes;
        for g in &generics {
            let Some(GenericArg::Ty(ty)) = m.subst.get(g.def).cloned() else {
                continue;
            };
            for b in &g.bounds {
                let r = match b {
                    Bound::Prop(
                        pb @ (PropBound::Copy
                        | PropBound::Discard
                        | PropBound::Send
                        | PropBound::Sync
                        | PropBound::Owned),
                    ) => {
                        let p = self.lookup(cur, ty);
                        match pb {
                            PropBound::Copy => p.copy,
                            PropBound::Discard => p.discard(),
                            PropBound::Send => p.send,
                            PropBound::Sync => p.sync,
                            _ => p.owned(),
                        }
                    }
                    other => {
                        let other = self.subst_bound(other, &m.subst);
                        self.satisfies(ty, &other)
                    }
                };
                result = result.and(r);
            }
        }
        result
    }
}
