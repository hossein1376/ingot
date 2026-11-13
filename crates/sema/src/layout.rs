//! Layouts (spec section 25): size, alignment and field offsets of every type that
//! has no generic parameter. A layout that depends on a parameter is computed per
//! instance in a later phase; nothing is invented for it here.

use crate::defs::DefId;
use crate::lowering::Cx;
use crate::sigs::*;
use crate::ty::*;
use std::collections::BTreeSet;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LayoutError {
    /// The type mentions a generic parameter.
    Params,
    /// The type mentions poison (reported elsewhere).
    Poison,
    /// The type contains itself (E0516, reported elsewhere).
    Cycle,
    /// The specification does not define it.
    Unspecified(String),
}

pub fn align_up(offset: u64, align: u64) -> Option<u64> {
    let align = align.max(1);
    offset.checked_add(align - 1).map(|v| v / align * align)
}

/// A valid `align(...)` argument: a positive power of two.
pub fn valid_align(v: i128) -> Option<u64> {
    if v > 0 && (v & (v - 1)) == 0 && v <= 1 << 32 {
        Some(v as u64)
    } else {
        None
    }
}

fn too_big() -> LayoutError {
    LayoutError::Unspecified(
        "the layout of a type larger than 2^64 bytes; the specification gives no limit".to_string(),
    )
}

impl Declarations {
    /// The layout of `ty`, kept for `ingot layout`.
    pub fn layout_of(&mut self, ty: TypeId) -> Result<Layout, LayoutError> {
        if let Some(l) = self.layouts.get(&ty) {
            return Ok(l.clone());
        }
        let mut stack = Vec::new();
        let l = self.layout_inner(ty, &mut stack)?;
        Ok(l)
    }

    fn layout_inner(&mut self, ty: TypeId, stack: &mut Vec<TypeId>) -> Result<Layout, LayoutError> {
        if let Some(l) = self.layouts.get(&ty) {
            return Ok(l.clone());
        }
        if stack.contains(&ty) || stack.len() > 256 {
            return Err(LayoutError::Cycle);
        }
        stack.push(ty);
        let r = self.layout_compute(ty, stack);
        stack.pop();
        if let Ok(l) = &r {
            self.layouts.insert(ty, l.clone());
        }
        r
    }

    fn layout_compute(
        &mut self,
        ty: TypeId,
        stack: &mut Vec<TypeId>,
    ) -> Result<Layout, LayoutError> {
        let kind = self.types.kind(ty).clone();
        match kind {
            TyKind::Int(i) => {
                let n = u64::from(i.bits() / 8);
                Ok(Layout::scalar(n, n))
            }
            TyKind::Float(f) => {
                let n = u64::from(f.bits() / 8);
                Ok(Layout::scalar(n, n))
            }
            TyKind::Bool => Ok(Layout::scalar(1, 1)),
            // Section 3 gives size 0; an alignment of 1 is the only one a size-0 type
            // can have.
            TyKind::Void | TyKind::Never => Ok(Layout::scalar(0, 1)),
            TyKind::Ref(..) | TyKind::RawPtr(..) | TyKind::FnPtr(_) => Ok(Layout::scalar(8, 8)),
            TyKind::Slice(..) | TyKind::Dyn(..) | TyKind::DynFn(..) => Ok(Layout::scalar(16, 8)),
            TyKind::Vector(..) | TyKind::Mask(_) => Err(LayoutError::Unspecified(
                "the alignment of SIMD vector types and the size of masks (the specification \
                 gives neither)"
                    .to_string(),
            )),
            TyKind::Array(elem, n) => {
                let len = match n {
                    ConstArg::Value(ConstValue::Int { value, .. }) if value >= 0 => value as u64,
                    ConstArg::Value(_) => return Err(LayoutError::Poison),
                    ConstArg::Param(_) => return Err(LayoutError::Params),
                    ConstArg::Poison => return Err(LayoutError::Poison),
                };
                let e = self.layout_inner(elem, stack)?;
                let size = e.size.checked_mul(len).ok_or_else(too_big)?;
                Ok(Layout {
                    size,
                    align: e.align,
                    kind: LayoutKind::Aggregate,
                    fields: Vec::new(),
                })
            }
            TyKind::Tuple(ts) => {
                let mut fields = Vec::new();
                for (i, t) in ts.iter().enumerate() {
                    let l = self.layout_inner(*t, stack)?;
                    fields.push((i.to_string(), l, 1u64));
                }
                self.struct_layout(fields, 1)
            }
            TyKind::Optional(inner) => {
                let l = self.layout_inner(inner, stack)?;
                // `?&T`, `?&mut T`, `?[]T` and `?fn(...)` use the null pointer for
                // `.none` (section 25). No other niche is guaranteed.
                let niche = match self.types.kind(inner) {
                    TyKind::Ref(..) | TyKind::Slice(..) => true,
                    TyKind::FnPtr(sig) => !sig.ext,
                    _ => false,
                };
                if niche {
                    return Ok(Layout {
                        size: l.size,
                        align: l.align,
                        kind: LayoutKind::Niche,
                        fields: vec![FieldLayout {
                            name: "some".to_string(),
                            offset: 0,
                            size: l.size,
                            align: l.align,
                        }],
                    });
                }
                self.enum_layout(1, &[("some".to_string(), Some(l))])
            }
            TyKind::Result(t, e) => {
                let a = self.layout_inner(t, stack)?;
                let b = self.layout_inner(e, stack)?;
                self.enum_layout(
                    1,
                    &[("ok".to_string(), Some(a)), ("err".to_string(), Some(b))],
                )
            }
            TyKind::Adt(def, args) => self.adt_layout(ty, def, &args, stack),
            TyKind::Param(_) | TyKind::Projection(..) => Err(LayoutError::Params),
            TyKind::Poison => Err(LayoutError::Poison),
        }
    }

    /// Fields in order, each with the alignment it is raised to (1: not raised), then
    /// the alignment of the whole.
    fn struct_layout(
        &self,
        fields: Vec<(String, Layout, u64)>,
        whole: u64,
    ) -> Result<Layout, LayoutError> {
        let mut offset: u64 = 0;
        let mut align: u64 = whole.max(1);
        let mut out = Vec::new();
        for (name, l, raised) in fields {
            let a = l.align.max(raised);
            offset = align_up(offset, a).ok_or_else(too_big)?;
            out.push(FieldLayout {
                name,
                offset,
                size: l.size,
                align: a,
            });
            offset = offset.checked_add(l.size).ok_or_else(too_big)?;
            align = align.max(a);
        }
        let size = align_up(offset, align).ok_or_else(too_big)?;
        Ok(Layout {
            size,
            align,
            kind: LayoutKind::Aggregate,
            fields: out,
        })
    }

    /// A tag of `tag_size` bytes, then the payload at the alignment of the most
    /// aligned payload (section 25).
    fn enum_layout(
        &self,
        tag_size: u64,
        variants: &[(String, Option<Layout>)],
    ) -> Result<Layout, LayoutError> {
        let mut max_size = 0u64;
        let mut max_align = 1u64;
        let mut any = false;
        for (_, p) in variants {
            if let Some(l) = p {
                any = true;
                max_size = max_size.max(l.size);
                max_align = max_align.max(l.align);
            }
        }
        if !any {
            return Ok(Layout {
                size: tag_size,
                align: tag_size,
                kind: LayoutKind::Enum {
                    tag_size,
                    payload_offset: tag_size,
                },
                fields: Vec::new(),
            });
        }
        let payload_offset = align_up(tag_size, max_align).ok_or_else(too_big)?;
        let align = tag_size.max(max_align);
        let size = align_up(
            payload_offset.checked_add(max_size).ok_or_else(too_big)?,
            align,
        )
        .ok_or_else(too_big)?;
        let fields = variants
            .iter()
            .filter_map(|(n, p)| {
                p.as_ref().map(|l| FieldLayout {
                    name: n.clone(),
                    offset: payload_offset,
                    size: l.size,
                    align: l.align,
                })
            })
            .collect();
        Ok(Layout {
            size,
            align,
            kind: LayoutKind::Enum {
                tag_size,
                payload_offset,
            },
            fields,
        })
    }

    fn adt_layout(
        &mut self,
        ty: TypeId,
        def: DefId,
        args: &[GenericArg],
        stack: &mut Vec<TypeId>,
    ) -> Result<Layout, LayoutError> {
        if self.types.has_params(ty) {
            return Err(LayoutError::Params);
        }
        if self.infinite.contains(&def) {
            return Err(LayoutError::Cycle);
        }
        let s = self.subst_for(def, args);
        match self.items.get(&def).cloned() {
            Some(ItemSig::Struct(st)) => {
                let mut fields = Vec::new();
                for f in &st.fields {
                    let t = self.types.subst(f.ty, &s);
                    let l = self.layout_inner(t, stack)?;
                    let raised = f.align.and_then(|a| valid_align(a.value)).unwrap_or(1);
                    fields.push((f.name.clone(), l, raised));
                }
                let whole = st.align.and_then(|a| valid_align(a.value)).unwrap_or(1);
                self.struct_layout(fields, whole)
            }
            Some(ItemSig::Enum(en)) => {
                let mut variants = Vec::new();
                if en.variants.is_empty() {
                    return Err(LayoutError::Unspecified(
                        "the layout of an enum with no variants (the specification gives none)"
                            .to_string(),
                    ));
                }
                for v in &en.variants {
                    let p = match v.payload {
                        Some(t) => {
                            let t = self.types.subst(t, &s);
                            Some(self.layout_inner(t, stack)?)
                        }
                        None => None,
                    };
                    variants.push((v.name.clone(), p));
                }
                self.enum_layout(u64::from(en.tag_ty.bits() / 8), &variants)
            }
            _ => Err(LayoutError::Unspecified(
                "the layout of a built-in type with no declaration (`Idx`, `Grid`)".to_string(),
            )),
        }
    }

    /// The alignment a struct has without its own `align(...)`: the largest alignment
    /// of its fields.
    pub fn natural_struct_align(&mut self, def: DefId) -> Result<u64, LayoutError> {
        let Some(ItemSig::Struct(st)) = self.items.get(&def).cloned() else {
            return Err(LayoutError::Poison);
        };
        let mut align = 1u64;
        for f in &st.fields {
            let l = self.layout_of(f.ty)?;
            let raised = f.align.and_then(|a| valid_align(a.value)).unwrap_or(1);
            align = align.max(l.align).max(raised);
        }
        Ok(align)
    }
}

impl Cx<'_> {
    /// The layout of `ty`, after making sure every struct and enum it is made of has a
    /// signature. A part of it the specification does not define is recorded.
    pub(crate) fn layout_of_checked(&mut self, ty: TypeId) -> Option<Layout> {
        if !self.ensure_layout_sigs(ty, &mut BTreeSet::new()) {
            return None;
        }
        match self.d.layout_of(ty) {
            Ok(l) => Some(l),
            Err(LayoutError::Unspecified(m)) => {
                self.ni(m);
                None
            }
            Err(_) => None,
        }
    }

    fn ensure_layout_sigs(&mut self, ty: TypeId, seen: &mut BTreeSet<DefId>) -> bool {
        if let TyKind::Adt(def, _) = self.d.types.kind(ty).clone()
            && seen.insert(def)
        {
            if self.is_busy(def) {
                self.ni(format!(
                    "a type whose layout depends on itself (`{}`); the specification defines no \
                     error for it",
                    self.sema.def(def).name
                ));
                return false;
            }
            self.ensure_sig(def);
            let fields: Vec<TypeId> = match self.d.items.get(&def) {
                Some(ItemSig::Struct(s)) => s.fields.iter().map(|f| f.ty).collect(),
                Some(ItemSig::Enum(e)) => e.variants.iter().filter_map(|v| v.payload).collect(),
                _ => Vec::new(),
            };
            for f in fields {
                if !self.ensure_layout_sigs(f, seen) {
                    return false;
                }
            }
        }
        for c in self.d.types.children(ty) {
            if !self.ensure_layout_sigs(c, seen) {
                return false;
            }
        }
        true
    }
}
