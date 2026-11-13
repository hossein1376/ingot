//! Places (spec section 5.5): what may be assigned or borrowed, and whether it may be
//! changed (E0606, E0607, E0610), and which variables are mutated (E0312).

use super::Checker;
use crate::defs::DefId;
use crate::hir::{ExprId, ExprKind, LocalId, LocalKind, Res, StmtKind};
use crate::sigs::ItemSig;
use crate::ty::*;
use crate::typed::*;
use ingot_diag::source::Span;
use ingot_syntax::ast::UnOp;

/// Why a place cannot be changed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Why {
    /// A `let`, a parameter or another immutable binding.
    Binding(LocalId),
    Static(DefId),
    /// Reached through a shared borrow.
    ThroughShared,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Access {
    Mutable,
    Immutable(Why),
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct PlaceInfo {
    pub access: Access,
    /// The variable whose own storage this place is part of (no borrow in between).
    pub root: Option<LocalId>,
}

/// What a use of a place needs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Use {
    Assign,
    BorrowMut,
}

impl Checker<'_, '_> {
    /// The place `e` denotes, or `None` if it is not a place (a value).
    pub(crate) fn place(&self, e: ExprId) -> Option<PlaceInfo> {
        let e = self.unparen(e);
        match &self.hir.expr(e).kind {
            ExprKind::Path(p) => match p.res {
                Res::Local(l) => {
                    let loc = self.hir.local(l);
                    let mutable = loc.mutable && loc.kind != LocalKind::Capture;
                    Some(PlaceInfo {
                        access: if mutable {
                            Access::Mutable
                        } else {
                            Access::Immutable(Why::Binding(l))
                        },
                        root: Some(l),
                    })
                }
                Res::Def(d) if self.cx.sema.def(d).kind == crate::defs::DefKind::Static => {
                    Some(PlaceInfo {
                        access: Access::Immutable(Why::Static(d)),
                        root: None,
                    })
                }
                _ => None,
            },
            ExprKind::Field { base, .. } | ExprKind::TupleField { base, .. } => {
                // The length of a slice can be read but not assigned.
                if matches!(
                    self.out.exprs.get(&e).map(|i| &i.res),
                    Some(ExprRes::SliceLen { .. })
                ) {
                    return None;
                }
                self.place_through(*base, self.ty_of(*base), false)
            }
            ExprKind::Index { base, .. } => {
                let bt = self.ty_of(*base);
                self.place_through(*base, bt, true)
            }
            ExprKind::Deref { base, .. } => {
                let bt = self.ty_of(*base);
                let base_place = self.place(*base);
                match self.cx.d.types.kind(bt) {
                    TyKind::Ref(m, _) | TyKind::RawPtr(m, _) => Some(Self::through(base_place, *m)),
                    _ => None,
                }
            }
            _ => None,
        }
    }

    /// The place of a member or element of `base`, whose type is `bt`: looking through
    /// every borrow of the base first. `index`: the last container may be a slice,
    /// which is itself a borrow.
    fn place_through(&self, base: ExprId, bt: TypeId, index: bool) -> Option<PlaceInfo> {
        let base_place = self.place(base);
        let mut t = bt;
        let mut info = base_place;
        let mut through = false;
        while let TyKind::Ref(m, inner) = self.cx.d.types.kind(t) {
            info = Some(Self::through(info, *m));
            through = true;
            t = *inner;
        }
        if index && let TyKind::Slice(m, _) = self.cx.d.types.kind(t) {
            return Some(Self::through(info, *m));
        }
        if index && matches!(self.cx.d.types.kind(t), TyKind::Vector(..)) {
            return None;
        }
        if through {
            return info.map(|i| PlaceInfo { root: None, ..i });
        }
        info
    }

    /// The place reached by looking through a borrow of mutability `m` from a base.
    fn through(base: Option<PlaceInfo>, m: Mutability) -> PlaceInfo {
        let shared_before = matches!(
            base,
            Some(PlaceInfo {
                access: Access::Immutable(Why::ThroughShared),
                ..
            })
        );
        let access = if m == Mutability::Shared || shared_before {
            Access::Immutable(Why::ThroughShared)
        } else {
            Access::Mutable
        };
        PlaceInfo { access, root: None }
    }

    /// The place a method receiver denotes after looking through `derefs` borrows.
    fn receiver_place(&self, recv: ExprId, derefs: u32) -> Option<PlaceInfo> {
        let mut info = self.place(recv);
        let mut t = self.ty_of(recv);
        for _ in 0..derefs {
            if let TyKind::Ref(m, inner) = self.cx.d.types.kind(t) {
                info = Some(Self::through(info, *m));
                t = *inner;
            }
        }
        info
    }

    /// Check that `e` may be assigned or exclusively borrowed, and note the mutation.
    pub(crate) fn require_mutable_place(&mut self, e: ExprId, use_: Use) {
        let info = self.place(e);
        self.require_mutable(e, info, use_);
    }

    /// The same for the receiver of a method call that takes `&mut self`.
    pub(crate) fn require_mutable_receiver(&mut self, recv: ExprId, derefs: u32) {
        let info = self.receiver_place(recv, derefs);
        self.require_mutable(recv, info, Use::BorrowMut);
    }

    fn require_mutable(&mut self, e: ExprId, info: Option<PlaceInfo>, use_: Use) {
        let span = self.span_of(e);
        let Some(info) = info else {
            let what = match use_ {
                Use::Assign => "assignment to",
                Use::BorrowMut => "an exclusive borrow of",
            };
            self.err(
                "E0607",
                format!("{what} something that is not a place"),
                span,
            );
            return;
        };
        match info.access {
            Access::Mutable => {
                if let Some(root) = info.root {
                    self.mutated.insert(root);
                }
            }
            Access::Immutable(why) => {
                let code = match use_ {
                    Use::Assign => "E0606",
                    Use::BorrowMut => "E0610",
                };
                let (message, declared, fix) = match why {
                    Why::Binding(l) => {
                        let loc = self.hir.local(l);
                        let name = loc.name.clone();
                        let kind = loc.kind;
                        let decl = loc.span;
                        let fix = self.let_to_var(l);
                        let what = if kind == LocalKind::Param {
                            "a parameter"
                        } else {
                            "an immutable binding"
                        };
                        (
                            format!("`{name}` is {what}; it cannot be {}", verb(use_)),
                            Some((self.file_path(), decl)),
                            fix,
                        )
                    }
                    Why::Static(d) => {
                        let name = self.cx.sema.def(d).name.clone();
                        let decl = self.cx.sema.def(d).span;
                        let file = self.cx.sources.get(self.cx.file_of(d)).path.clone();
                        (
                            format!("the static `{name}` is immutable through its name"),
                            Some((file, decl)),
                            None,
                        )
                    }
                    Why::ThroughShared => (
                        format!(
                            "this place is reached through a shared borrow; it cannot be {}",
                            verb(use_)
                        ),
                        None,
                        None,
                    ),
                };
                let d = self.err(code, message, span);
                if let Some((file, decl)) = declared {
                    d.relate_in("declared", &file, decl);
                }
                if let Some(edit) = fix {
                    d.fix("declare it with `var`", edit);
                }
            }
        }
    }

    /// The edit that turns the `let` that declares `l` into `var`.
    fn let_to_var(&self, l: LocalId) -> Option<ingot_diag::diagnostic::Edit> {
        let stmt = *self.let_of.get(&l)?;
        let s = self.hir.stmt(stmt);
        match &s.kind {
            StmtKind::Let { mutable: false, .. } => {
                Some(self.edit(Span::new(s.span.start, s.span.start + 3), "var"))
            }
            _ => None,
        }
    }

    // ----------------------------------------------------------------- borrows

    /// `&place` and `&mut place`.
    pub(crate) fn borrow(
        &mut self,
        e: ExprId,
        op: UnOp,
        operand: ExprId,
        op_span: Span,
    ) -> (TypeId, ExprRes) {
        let _ = (e, op_span);
        let m = if op == UnOp::BorrowMut {
            Mutability::Mut
        } else {
            Mutability::Shared
        };
        // `&a[lo..hi]` and `&mut a[lo..hi]`: the slice of an array is written as a borrow.
        if let ExprKind::Slice {
            base,
            lo,
            hi,
            bracket,
        } = &self.hir.expr(operand).kind
        {
            let (ty, res) = self.slice(operand, *base, *lo, *hi, *bracket, Some(m));
            self.set(operand, ty, res.clone());
            if let ExprRes::Slice { of_array: true, .. } = res {
                if m == Mutability::Mut {
                    let derefs = match res {
                        ExprRes::Slice { derefs, .. } => derefs,
                        _ => 0,
                    };
                    self.require_mutable_receiver(*base, derefs);
                } else if self.place(*base).is_none()
                    && !matches!(self.kind(self.ty_of(*base)), TyKind::Ref(..))
                {
                    let span = self.span_of(*base);
                    self.err("E0607", "a borrow of something that is not a place", span);
                }
                return (ty, ExprRes::Borrow(m));
            }
            // A slice of a slice is already a value: borrowing it again is a borrow of a
            // value that is not a place.
            if !self.is_poison(ty) {
                let span = self.span_of(operand);
                self.err("E0607", "a borrow of something that is not a place", span);
            }
            return (self.poison(), ExprRes::None);
        }
        let t = self.expr(operand, None);
        if self.is_poison(t) || self.is_never(t) {
            return (self.poison(), ExprRes::None);
        }
        if self.in_item_init {
            // A static or const that holds a borrow may only borrow other statics and
            // literals (section 4.5).
            let inner = self.unparen(operand);
            let ok = match &self.hir.expr(inner).kind {
                ExprKind::Path(p) => {
                    matches!(p.res, Res::Def(d) if self.cx.sema.def(d).kind == crate::defs::DefKind::Static)
                }
                ExprKind::Int { .. }
                | ExprKind::Float(_)
                | ExprKind::Bool(_)
                | ExprKind::Str(_)
                | ExprKind::CStr(_) => true,
                _ => false,
            };
            if !ok {
                let span = self.span_of(operand);
                self.err(
                    "E0607",
                    "an initializer may borrow only statics and literals",
                    span,
                );
                return (self.poison(), ExprRes::None);
            }
            if m == Mutability::Mut {
                let span = self.span_of(operand);
                self.err(
                    "E0610",
                    "an initializer cannot take an exclusive borrow",
                    span,
                );
                return (self.poison(), ExprRes::None);
            }
        } else if m == Mutability::Mut {
            self.require_mutable_place(operand, Use::BorrowMut);
        } else if self.place(operand).is_none() {
            let span = self.span_of(operand);
            self.err("E0607", "a borrow of something that is not a place", span);
            return (self.poison(), ExprRes::None);
        }
        (self.ref_to(m, t), ExprRes::Borrow(m))
    }

    /// Whether a `ItemSig` is a static (for messages).
    #[allow(dead_code)]
    pub(crate) fn is_static(&self, d: DefId) -> bool {
        matches!(self.cx.d.items.get(&d), Some(ItemSig::Static(_)))
    }
}

fn verb(u: Use) -> &'static str {
    match u {
        Use::Assign => "assigned",
        Use::BorrowMut => "borrowed exclusively",
    }
}
