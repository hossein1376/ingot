//! Exhaustiveness and reachability of `match` (spec section 7.2, pipeline section 4.4):
//! the usual matrix algorithm over constructors. Integer ranges are intervals; a guard
//! makes its arm count for nothing. Also the irrefutability of the pattern of a `let`.

use super::Checker;
use crate::hir::{self, ExprId, ExprKind, PatId, PatKind};
use crate::sigs::ItemSig;
use crate::ty::*;
use crate::typed::*;
use ingot_diag::diagnostic::Edit;
use ingot_diag::source::Span;

/// A constructor of a pattern.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Ctor {
    /// The only constructor of a tuple or struct.
    Single,
    Variant(usize),
    Bool(bool),
    /// An inclusive range of integers (empty when `lo > hi`).
    Int(i128, i128),
    /// A literal of a type with no finite set of values (floats, strings): it covers
    /// only itself.
    Opaque(String),
}

/// A pattern reduced to what the algorithm needs.
#[derive(Clone, Debug)]
pub(crate) enum Pt {
    Wild,
    Ctor(Ctor, Vec<Pt>),
}

impl Checker<'_, '_, '_> {
    // ------------------------------------------------------------ pattern -> Pt

    fn int_literal_value(&self, e: ExprId) -> Option<i128> {
        match &self.hir.expr(self.unparen(e)).kind {
            ExprKind::Int {
                value, negative, ..
            } => {
                let v = i128::try_from(*value).ok()?;
                Some(if *negative { -v } else { v })
            }
            _ => None,
        }
    }

    fn to_pt(&self, p: PatId) -> Pt {
        let hir = self.hir;
        let Some(info) = self.out.pats.get(&p) else {
            return Pt::Wild;
        };
        match &hir.pat(p).kind {
            PatKind::Wild | PatKind::Bind { .. } | PatKind::Poison => Pt::Wild,
            PatKind::Literal(lit) => {
                let lit = self.unparen(*lit);
                match &hir.expr(lit).kind {
                    ExprKind::Bool(b) => Pt::Ctor(Ctor::Bool(*b), Vec::new()),
                    ExprKind::Int { .. }
                        if matches!(self.cx.d.types.kind(info.ty), TyKind::Int(_)) =>
                    {
                        match self.int_literal_value(lit) {
                            Some(v) => Pt::Ctor(Ctor::Int(v, v), Vec::new()),
                            None => Pt::Ctor(
                                Ctor::Opaque(self.text(hir.expr(lit).span).to_string()),
                                Vec::new(),
                            ),
                        }
                    }
                    _ => Pt::Ctor(
                        Ctor::Opaque(self.text(hir.expr(lit).span).to_string()),
                        Vec::new(),
                    ),
                }
            }
            PatKind::Range { lo, hi, inclusive } => {
                let bound = |s: &Self, q: PatId| match &hir.pat(q).kind {
                    PatKind::Literal(l) => s.int_literal_value(*l),
                    _ => None,
                };
                match (bound(self, *lo), bound(self, *hi)) {
                    (Some(a), Some(b)) => {
                        let b = if *inclusive { b } else { b - 1 };
                        Pt::Ctor(Ctor::Int(a, b), Vec::new())
                    }
                    _ => Pt::Ctor(
                        Ctor::Opaque(format!("range@{}", hir.pat(p).span.start)),
                        Vec::new(),
                    ),
                }
            }
            PatKind::Variant { payload, .. } => match &info.res {
                PatRes::Variant(v) => {
                    let args = match payload {
                        Some(sub) => vec![self.to_pt(*sub)],
                        None => Vec::new(),
                    };
                    Pt::Ctor(Ctor::Variant(v.index), args)
                }
                _ => Pt::Wild,
            },
            PatKind::Tuple(elems) => {
                Pt::Ctor(Ctor::Single, elems.iter().map(|e| self.to_pt(*e)).collect())
            }
            PatKind::Struct { fields, .. } => match &info.res {
                PatRes::Struct { res: s, .. } => {
                    let arity = self
                        .cx
                        .d
                        .struct_sig(s.def)
                        .map(|x| x.fields.len())
                        .unwrap_or(0);
                    let mut args = vec![Pt::Wild; arity];
                    for (f, &i) in fields.iter().zip(&s.order) {
                        if i < arity {
                            args[i] = self.to_pt(f.pattern);
                        }
                    }
                    Pt::Ctor(Ctor::Single, args)
                }
                _ => Pt::Wild,
            },
        }
    }

    // ------------------------------------------------------------- the algorithm

    fn norm(&self, t: TypeId) -> TypeId {
        self.strip_refs(t).0
    }

    fn int_domain(&self, t: TypeId) -> Option<(i128, i128)> {
        match self.cx.d.types.kind(t) {
            TyKind::Int(i) => Some(((*i).min(), (*i).max())),
            _ => None,
        }
    }

    /// The types of the sub-patterns of constructor `c` at type `t`.
    fn sub_tys(&mut self, t: TypeId, c: &Ctor) -> Vec<TypeId> {
        match c {
            Ctor::Single => match self.kind(t) {
                TyKind::Tuple(ts) => ts,
                TyKind::Adt(d, args) => match self.cx.d.struct_sig(d).cloned() {
                    Some(sig) => {
                        let s = self.cx.d.subst_for(d, &args);
                        sig.fields
                            .iter()
                            .map(|f| self.cx.d.instantiate(f.ty, &s))
                            .collect()
                    }
                    None => Vec::new(),
                },
                _ => Vec::new(),
            },
            Ctor::Variant(i) => match self.variant_payload(t, *i) {
                Some(p) => vec![p],
                None => Vec::new(),
            },
            _ => Vec::new(),
        }
    }

    fn variant_payload(&mut self, t: TypeId, index: usize) -> Option<TypeId> {
        match self.kind(t) {
            TyKind::Adt(d, args) => {
                let sig = self.cx.d.enum_sig(d)?.clone();
                let v = sig.variants.get(index)?;
                let s = self.cx.d.subst_for(d, &args);
                v.payload.map(|p| self.cx.d.instantiate(p, &s))
            }
            TyKind::Optional(inner) => (index == 1).then_some(inner),
            TyKind::Result(ok, err) => Some(if index == 0 { ok } else { err }),
            _ => None,
        }
    }

    /// The constructors a wildcard stands for at type `t`, given the constructors the
    /// rows use. `None`: the type has no finite set of constructors.
    fn split(&mut self, t: TypeId, heads: &[&Ctor]) -> Option<Vec<Ctor>> {
        match self.kind(t) {
            TyKind::Bool => Some(vec![Ctor::Bool(true), Ctor::Bool(false)]),
            TyKind::Tuple(_) => Some(vec![Ctor::Single]),
            TyKind::Optional(_) => Some(vec![Ctor::Variant(0), Ctor::Variant(1)]),
            TyKind::Result(..) => Some(vec![Ctor::Variant(0), Ctor::Variant(1)]),
            TyKind::Adt(d, _) => match self.cx.d.items.get(&d) {
                Some(ItemSig::Enum(e)) => Some((0..e.variants.len()).map(Ctor::Variant).collect()),
                Some(ItemSig::Struct(_)) => Some(vec![Ctor::Single]),
                _ => None,
            },
            TyKind::Int(i) => Some(self.partition(i.min(), i.max(), heads)),
            _ => None,
        }
    }

    /// Split `[lo, hi]` at the boundaries of the integer ranges among `heads`.
    fn partition(&self, lo: i128, hi: i128, heads: &[&Ctor]) -> Vec<Ctor> {
        let mut cuts: Vec<i128> = vec![lo];
        for h in heads {
            if let Ctor::Int(a, b) = h
                && a <= b
            {
                if *a > lo && *a <= hi {
                    cuts.push(*a);
                }
                if *b >= lo && *b < hi {
                    cuts.push(*b + 1);
                }
            }
        }
        cuts.sort();
        cuts.dedup();
        let mut out = Vec::new();
        for (i, &start) in cuts.iter().enumerate() {
            let end = cuts.get(i + 1).map(|n| *n - 1).unwrap_or(hi);
            out.push(Ctor::Int(start, end));
        }
        out
    }

    /// Whether a row with head constructor `row` matches every value that `c` stands for.
    fn covers(row: &Ctor, c: &Ctor) -> bool {
        match (row, c) {
            (Ctor::Int(a, b), Ctor::Int(x, y)) => a <= x && y <= b && a <= b,
            _ => row == c,
        }
    }

    /// Whether the vector `v` matches some value that no row of `rows` matches.
    fn useful(&mut self, rows: &[Vec<Pt>], v: &[Pt], tys: &[TypeId]) -> bool {
        if v.is_empty() {
            return rows.is_empty();
        }
        let rows: Vec<Vec<Pt>> = rows.to_vec();
        let ty = self.norm(tys[0]);
        match &v[0] {
            Pt::Ctor(c, args) => {
                let c = c.clone();
                let args = args.clone();
                if let Ctor::Int(lo, hi) = c {
                    // A range in `v` may straddle the boundaries of the rows' ranges.
                    let heads: Vec<Ctor> = rows
                        .iter()
                        .filter_map(|r| match &r[0] {
                            Pt::Ctor(c2, _) => Some(c2.clone()),
                            _ => None,
                        })
                        .collect();
                    let refs: Vec<&Ctor> = heads.iter().collect();
                    if lo > hi {
                        return false;
                    }
                    for part in self.partition(lo, hi, &refs) {
                        if self.useful_ctor(&rows, &part, &args, &v[1..], tys) {
                            return true;
                        }
                    }
                    return false;
                }
                self.useful_ctor(&rows, &c, &args, &v[1..], tys)
            }
            Pt::Wild => {
                let heads: Vec<Ctor> = rows
                    .iter()
                    .filter_map(|r| match &r[0] {
                        Pt::Ctor(c, _) => Some(c.clone()),
                        _ => None,
                    })
                    .collect();
                let refs: Vec<&Ctor> = heads.iter().collect();
                // With no constructor in the column the default matrix decides, except for a
                // type with no values at all, which a wildcard cannot be useful for.
                let ctors = match self.split(ty, &refs) {
                    Some(c) if c.is_empty() => Some(c),
                    Some(_) if heads.is_empty() => None,
                    other => other,
                };
                match ctors {
                    Some(ctors) => {
                        for c in ctors {
                            let arity = self.sub_tys(ty, &c).len();
                            let args = vec![Pt::Wild; arity];
                            if self.useful_ctor(&rows, &c, &args, &v[1..], tys) {
                                return true;
                            }
                        }
                        false
                    }
                    None => {
                        // The default matrix: the rows that start with a wildcard.
                        let default: Vec<Vec<Pt>> = rows
                            .iter()
                            .filter(|r| matches!(r[0], Pt::Wild))
                            .map(|r| r[1..].to_vec())
                            .collect();
                        self.useful(&default, &v[1..], &tys[1..])
                    }
                }
            }
        }
    }

    fn useful_ctor(
        &mut self,
        rows: &[Vec<Pt>],
        c: &Ctor,
        args: &[Pt],
        tail: &[Pt],
        tys: &[TypeId],
    ) -> bool {
        let ty = self.norm(tys[0]);
        let mut sub = self.sub_tys(ty, c);
        if sub.len() != args.len() {
            // A pattern of the wrong shape was reported already.
            sub = vec![self.poison(); args.len()];
        }
        let arity = args.len();
        let mut spec: Vec<Vec<Pt>> = Vec::new();
        for r in rows {
            match &r[0] {
                Pt::Wild => {
                    let mut nr = vec![Pt::Wild; arity];
                    nr.extend_from_slice(&r[1..]);
                    spec.push(nr);
                }
                Pt::Ctor(rc, rargs) if Self::covers(rc, c) => {
                    let mut nr = rargs.clone();
                    nr.resize(arity, Pt::Wild);
                    nr.extend_from_slice(&r[1..]);
                    spec.push(nr);
                }
                _ => {}
            }
        }
        let mut nv = args.to_vec();
        nv.extend_from_slice(tail);
        let mut ntys = sub;
        ntys.extend_from_slice(&tys[1..]);
        self.useful(&spec, &nv, &ntys)
    }

    // ----------------------------------------------------------------- reports

    /// E0604: the pattern of a `let` or `var` can fail to match.
    pub(crate) fn check_irrefutable(&mut self, p: PatId, ty: TypeId) {
        if self.is_poison(ty) || !self.out.pats.contains_key(&p) || self.pat_failed(p) {
            return;
        }
        let pt = self.to_pt(p);
        if self.useful(&[vec![pt]], &[Pt::Wild], &[ty]) {
            let span = self.hir.pat(p).span;
            self.err(
                "E0604",
                "this pattern can fail to match: use `if let` or `match` for it",
                span,
            );
        }
    }

    /// E0601 and E0602 for the arms of a `match`.
    pub(crate) fn exhaustiveness(
        &mut self,
        st: TypeId,
        arms: &[hir::Arm],
        kw: Span,
        scrutinee: ExprId,
    ) {
        let _ = scrutinee;
        if self.is_poison(st) || self.is_never(st) {
            return;
        }
        if arms
            .iter()
            .any(|a| a.patterns.iter().any(|p| self.pat_failed(*p)))
        {
            return;
        }
        let mut rows: Vec<Vec<Pt>> = Vec::new();
        for arm in arms {
            let alts: Vec<(Pt, Span)> = if arm.is_else {
                vec![(Pt::Wild, Span::new(arm.span.start, arm.span.start + 4))]
            } else {
                arm.patterns
                    .iter()
                    .map(|&p| (self.to_pt(p), self.hir.pat(p).span))
                    .collect()
            };
            let mut reported = false;
            for (pt, span) in &alts {
                if !self.useful(&rows, std::slice::from_ref(pt), &[st]) && !reported {
                    reported = alts.len() == 1;
                    let edit = if alts.len() == 1 {
                        self.delete_arm_edit(arm)
                    } else {
                        None
                    };
                    let d = self.err(
                        "E0602",
                        "this arm can never be chosen: earlier arms cover everything it matches",
                        *span,
                    );
                    if let Some(edit) = edit {
                        d.fix("delete the arm", edit);
                    }
                }
            }
            if arm.guard.is_none() {
                for (pt, _) in alts {
                    rows.push(vec![pt]);
                }
            }
        }
        if self.useful(&rows, &[Pt::Wild], &[st]) {
            let fix = self.exhaustive_fix(st, arms, &rows);
            let d = self.err(
                "E0601",
                "this `match` does not cover every value of its scrutinee",
                kw,
            );
            if let Some((message, edits)) = fix {
                d.fix_edits(message, edits);
            }
        }
    }

    // -------------------------------------------------------------------- fixes

    /// The extent of an arm including a trailing comma.
    fn arm_extent(&self, arm: &hir::Arm) -> Span {
        let text = self.cx.sources.get(self.file).text.as_bytes();
        let mut end = arm.span.end;
        let mut i = end;
        while i < text.len() && (text[i] == b' ' || text[i] == b'\t') {
            i += 1;
        }
        if i < text.len() && text[i] == b',' {
            end = i + 1;
        }
        Span::new(arm.span.start, end)
    }

    fn delete_arm_edit(&self, arm: &hir::Arm) -> Option<Edit> {
        let extent = self.arm_extent(arm);
        let text = &self.cx.sources.get(self.file).text;
        let bytes = text.as_bytes();
        let line_start = text[..extent.start].rfind('\n').map(|i| i + 1).unwrap_or(0);
        let line_end = text[extent.end..]
            .find('\n')
            .map(|i| extent.end + i + 1)
            .unwrap_or(text.len());
        let before_blank = bytes[line_start..extent.start]
            .iter()
            .all(|b| *b == b' ' || *b == b'\t');
        // Only white space, or a comment, follows the arm on its line.
        let rest = text[extent.end..line_end].trim_start();
        let after_blank = rest.is_empty() || rest.starts_with("//");
        if before_blank && after_blank {
            Some(self.edit(Span::new(line_start, line_end), ""))
        } else {
            Some(self.edit(extent, ""))
        }
    }

    /// Arms for the missing variants of an enum scrutinee, or an `else` arm.
    fn exhaustive_fix(
        &mut self,
        st: TypeId,
        arms: &[hir::Arm],
        rows: &[Vec<Pt>],
    ) -> Option<(String, Vec<Edit>)> {
        let last = arms.last()?;
        let extent = self.arm_extent(last);
        let text = &self.cx.sources.get(self.file).text;
        let line_start = text[..last.span.start]
            .rfind('\n')
            .map(|i| i + 1)
            .unwrap_or(0);
        let indent: String = text[line_start..]
            .chars()
            .take_while(|c| *c == ' ' || *c == '\t')
            .collect();
        let block_form = matches!(last.body, hir::ArmBody::Block(_));
        let body = if block_form {
            "{ @panic(\"todo\"); }".to_string()
        } else {
            "@panic(\"todo\"),".to_string()
        };
        let ty = self.norm(st);
        let mut patterns: Vec<String> = Vec::new();
        let enum_like = self.is_enum_like(ty);
        if enum_like {
            let names = self.variant_name_list(ty);
            for (i, name) in names.iter().enumerate() {
                let arity = self.sub_tys(ty, &Ctor::Variant(i)).len();
                let args = vec![Pt::Wild; arity];
                let probe = Pt::Ctor(Ctor::Variant(i), args);
                if self.useful(rows, &[probe], &[st]) {
                    let payload = if arity == 1 { "(_)" } else { "" };
                    patterns.push(format!(".{name}{payload}"));
                }
            }
        }
        if patterns.is_empty() {
            patterns.push("else".to_string());
        }
        let mut insert = String::new();
        // The last arm of a `match` used as a value may lack its comma.
        if !block_form && extent.end == last.span.end {
            insert.push(',');
        }
        for p in &patterns {
            insert.push_str(&format!("\n{indent}{p} => {body}"));
        }
        let message = if enum_like && patterns[0] != "else" {
            "add an arm for each missing variant"
        } else {
            "add an `else` arm"
        };
        Some((
            message.to_string(),
            vec![self.edit(Span::empty(extent.end), insert)],
        ))
    }

    fn variant_name_list(&mut self, t: TypeId) -> Vec<String> {
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

    /// The value range of an integer type, for the unit tests.
    #[allow(dead_code)]
    pub(crate) fn domain_of(&self, t: TypeId) -> Option<(i128, i128)> {
        self.int_domain(t)
    }
}
