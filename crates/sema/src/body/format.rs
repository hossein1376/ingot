//! Format strings of `@debug` and `@format` (spec section 35, E0422).
//!
//! The literal is parsed here, once, at compile time: `{}` and `{:spec}` are
//! placeholders, `{{` and `}}` are literal braces. `spec` is
//! `[<|>][0][width][.precision][x|X|b|o]`. What a spec part may be used on is the
//! table of section 35. Every argument is read through a shared borrow, never moved.

use super::Checker;
use crate::hir::ExprKind;
use crate::ty::*;
use ingot_diag::source::Span;

/// The conversion at the end of a spec.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Conv {
    HexLower,
    HexUpper,
    Binary,
    Octal,
}

/// One placeholder of a format string.
struct Placeholder {
    /// The span of `{..}` inside the literal.
    span: Span,
    zero: bool,
    precision: bool,
    conv: Option<Conv>,
}

impl Checker<'_, '_, '_> {
    /// Check a format string and its arguments. `fmt` is the string literal argument,
    /// `args` the values, and `what` the builtin's name for messages. Returns whether
    /// the literal and the argument count were usable.
    pub(crate) fn format_string(
        &mut self,
        fmt: crate::hir::ExprId,
        args: &[crate::hir::ExprId],
        what: &str,
    ) -> bool {
        let fmt = self.unparen(fmt);
        let (bytes, span) = match &self.hir.expr(fmt).kind {
            ExprKind::Str(bytes) => (bytes.clone(), self.span_of(fmt)),
            _ => {
                let at = self.span_of(fmt);
                self.err(
                    "E0422",
                    format!("the format string of `@{what}` must be a string literal"),
                    at,
                );
                return false;
            }
        };
        let placeholders = self.parse_format(&bytes, span, what);
        let Some(placeholders) = placeholders else {
            return false;
        };
        if placeholders.len() < args.len() {
            let at = self.span_of(args[placeholders.len()]);
            self.err(
                "E0422",
                format!(
                    "`@{what}` has {} argument(s) but {} placeholder(s)",
                    args.len(),
                    placeholders.len()
                ),
                at,
            );
            return false;
        }
        if placeholders.len() > args.len() {
            for p in &placeholders[args.len()..] {
                self.err("E0422", "this placeholder has no argument", p.span);
            }
            return false;
        }
        let mut ok = true;
        for (p, &arg) in placeholders.iter().zip(args) {
            let t = self.ty_of(arg);
            // Every argument is read, not consumed (section 35): phase 9 sees a
            // shared borrow, so it is not a move.
            if let Some(info) = self.out.exprs.get_mut(&arg) {
                info.shared_read = true;
            }
            if !self.format_spec_ok(p, t, what) {
                ok = false;
            }
        }
        ok
    }

    /// Parse the literal's decoded bytes. `None` after reporting a malformed placeholder.
    fn parse_format(&mut self, bytes: &[u8], span: Span, what: &str) -> Option<Vec<Placeholder>> {
        let mut out = Vec::new();
        let mut i = 0;
        // The literal's span includes the quotes; the decoded bytes start after the
        // opening quote. With an escape the decoded offsets are not source offsets, so
        // a placeholder is then reported at the whole literal.
        let base = span.start + 1;
        let text = self.cx.sources.get(self.file).text_of(span);
        let exact = text.len() == bytes.len() + 2 && &text.as_bytes()[1..text.len() - 1] == bytes;
        let at = |a: usize, b: usize| if exact { Span::new(a, b) } else { span };
        while i < bytes.len() {
            match bytes[i] {
                b'{' => {
                    if bytes.get(i + 1) == Some(&b'{') {
                        i += 2;
                        continue;
                    }
                    let start = i;
                    i += 1;
                    if bytes.get(i) == Some(&b'}') {
                        out.push(Placeholder {
                            span: at(base + start, base + i + 1),
                            zero: false,
                            precision: false,
                            conv: None,
                        });
                        i += 1;
                        continue;
                    }
                    if bytes.get(i) != Some(&b':') {
                        self.malformed(at(base + start, base + i), what);
                        return None;
                    }
                    i += 1;
                    if matches!(bytes.get(i), Some(b'<') | Some(b'>')) {
                        i += 1;
                    }
                    let mut zero = false;
                    if bytes.get(i) == Some(&b'0') {
                        zero = true;
                        i += 1;
                    }
                    while bytes.get(i).is_some_and(u8::is_ascii_digit) {
                        i += 1;
                    }
                    let mut precision = false;
                    if bytes.get(i) == Some(&b'.') {
                        precision = true;
                        i += 1;
                        if !bytes.get(i).is_some_and(u8::is_ascii_digit) {
                            self.malformed(at(base + start, base + i), what);
                            return None;
                        }
                        while bytes.get(i).is_some_and(u8::is_ascii_digit) {
                            i += 1;
                        }
                    }
                    let conv = match bytes.get(i) {
                        Some(b'x') => Some(Conv::HexLower),
                        Some(b'X') => Some(Conv::HexUpper),
                        Some(b'b') => Some(Conv::Binary),
                        Some(b'o') => Some(Conv::Octal),
                        _ => None,
                    };
                    if conv.is_some() {
                        i += 1;
                    }
                    if bytes.get(i) != Some(&b'}') {
                        self.malformed(at(base + start, base + i), what);
                        return None;
                    }
                    out.push(Placeholder {
                        span: at(base + start, base + i + 1),
                        zero,
                        precision,
                        conv,
                    });
                    i += 1;
                }
                b'}' => {
                    if bytes.get(i + 1) == Some(&b'}') {
                        i += 2;
                        continue;
                    }
                    self.malformed(at(base + i, base + i + 1), what);
                    return None;
                }
                _ => i += 1,
            }
        }
        Some(out)
    }

    fn malformed(&mut self, at: Span, what: &str) {
        self.err(
            "E0422",
            format!("this is not a valid placeholder of `@{what}`"),
            at,
        );
    }

    /// Whether `p`'s spec parts are allowed on `t` (section 35).
    fn format_spec_ok(&mut self, p: &Placeholder, t: TypeId, what: &str) -> bool {
        if self.is_poison(t) || self.is_never(t) {
            return true;
        }
        let kind = self.kind(t);
        let int = matches!(kind, TyKind::Int(_))
            || matches!(kind, TyKind::Param(_))
                && self.poss(t).verdict((true, true, false)) == super::ops::Verdict::Ok;
        let float = matches!(kind, TyKind::Float(_))
            || matches!(kind, TyKind::Param(_))
                && self.poss(t).verdict((false, false, true)) == super::ops::Verdict::Ok;
        let bytes = matches!(
            kind,
            TyKind::Slice(_, el) if matches!(self.kind(el), TyKind::Int(IntTy::U8))
        );
        let mut bad = None;
        if p.zero && !(int || float) {
            bad = Some("zero padding");
        } else if p.precision && !float {
            bad = Some("a precision");
        } else if let Some(c) = p.conv {
            let ok = match c {
                Conv::HexLower | Conv::HexUpper => int || bytes,
                Conv::Binary | Conv::Octal => int,
            };
            if !ok {
                bad = Some("this conversion");
            }
        }
        if let Some(part) = bad {
            let name = self.name(t);
            self.err(
                "E0422",
                format!("`{name}` does not support {part} in a format spec"),
                p.span,
            );
            let _ = what;
            return false;
        }
        true
    }
}
