//! Phase 2: the lexer.
//!
//! The lexer classifies names by spelling alone, keeps byte spans on every
//! token, and reports every independent E01xx error it can. Comments are
//! skipped; keeping them for `fmt` is a later phase.

use crate::token::Punct::*;
use crate::token::{Kw, Punct, ScalarTy, Token, TokenKind, VectorTy};
use ingot_diag::diagnostic::{Diagnostic, Edit};
use ingot_diag::source::{Source, Span};

pub const BUILTINS: &[&str] = &[
    "abs",
    "alignof",
    "all",
    "any",
    "assert",
    "assume",
    "bits",
    "borrow_from_raw",
    "ceil",
    "count",
    "debug",
    "div_or",
    "enum",
    "first",
    "floor",
    "fma",
    "forget",
    "format",
    "gather",
    "insert",
    "iota",
    "lanes",
    "load",
    "load_tail",
    "lookup",
    "mask_bits",
    "max",
    "min",
    "name",
    "narrow",
    "nearest",
    "offsetof",
    "panic",
    "reduce_add",
    "reduce_and",
    "reduce_max",
    "reduce_min",
    "reduce_or",
    "rem",
    "rem_or",
    "replace",
    "round",
    "sat",
    "scatter",
    "select",
    "shl",
    "shr",
    "shuffle",
    "sizeof",
    "slice_from_raw",
    "splat",
    "sqrt",
    "static_assert",
    "store",
    "store_tail",
    "swap",
    "target",
    "test_env",
    "unchecked_add",
    "unchecked_div",
    "unchecked_index",
    "unchecked_load",
    "unchecked_mul",
    "unchecked_narrow",
    "unchecked_neg",
    "unchecked_rem",
    "unchecked_shl",
    "unchecked_shr",
    "unchecked_slice",
    "unchecked_store",
    "unchecked_sub",
    "uninit",
    "unreachable",
    "wrap",
    "zero",
];

pub fn lex(source: &Source) -> (Vec<Token>, Vec<Diagnostic>) {
    let mut lexer = Lexer {
        source,
        bytes: source.text.as_bytes(),
        pos: 0,
        tokens: Vec::new(),
        diagnostics: Vec::new(),
    };
    lexer.run();
    (lexer.tokens, lexer.diagnostics)
}

struct Lexer<'a> {
    source: &'a Source,
    bytes: &'a [u8],
    pos: usize,
    tokens: Vec<Token>,
    diagnostics: Vec<Diagnostic>,
}

impl<'a> Lexer<'a> {
    fn run(&mut self) {
        while self.pos < self.bytes.len() {
            let start = self.pos;
            let b = self.bytes[self.pos];
            match b {
                b' ' | b'\t' | b'\n' => self.pos += 1,
                b'\r' => {
                    let file = self.source.path.clone();
                    self.error(
                        "E0102",
                        "carriage return in source file",
                        Span::new(start, start + 1),
                    )
                    .fix(
                        "delete the carriage return",
                        Edit {
                            file,
                            span: Span::new(start, start + 1),
                            text: String::new(),
                        },
                    );
                    self.pos += 1;
                }
                b'/' if self.peek(1) == Some(b'/') => {
                    while self.pos < self.bytes.len() && self.bytes[self.pos] != b'\n' {
                        self.pos += 1;
                    }
                }
                b'/' if self.peek(1) == Some(b'*') => {
                    self.error(
                        "E0110",
                        "block comments are not part of Ingot",
                        Span::new(start, start + 2),
                    );
                    self.pos += 2;
                    while self.pos < self.bytes.len() {
                        if self.bytes[self.pos] == b'*' && self.peek(1) == Some(b'/') {
                            self.pos += 2;
                            break;
                        }
                        self.pos += 1;
                    }
                }
                b'"' => self.string(start, false),
                b'\'' => self.char_literal(start),
                b'c' if self.peek(1) == Some(b'"') => self.string(start + 1, true),
                b'0'..=b'9' => self.number(start),
                b'@' => self.builtin(start),
                b'_' | b'a'..=b'z' | b'A'..=b'Z' => self.name(start),
                _ => {
                    if b >= 0x80 {
                        let ch = self.source.text[self.pos..]
                            .chars()
                            .next()
                            .unwrap_or('\u{fffd}');
                        let end = self.pos + ch.len_utf8();
                        self.error(
                            "E0103",
                            "non-ASCII character outside a string literal or comment",
                            Span::new(start, end),
                        );
                        self.pos = end;
                    } else if !self.punct(start) {
                        let end = self.pos + 1;
                        self.error(
                            "E0111",
                            format!("`{}` starts no Ingot token", b as char),
                            Span::new(start, end),
                        );
                        self.pos = end;
                    }
                }
            }
        }
        self.tokens
            .push(Token::new(TokenKind::Eof, Span::empty(self.bytes.len())));
    }

    fn peek(&self, n: usize) -> Option<u8> {
        self.bytes.get(self.pos + n).copied()
    }

    fn error(
        &mut self,
        code: &'static str,
        message: impl Into<String>,
        span: Span,
    ) -> &mut Diagnostic {
        self.diagnostics
            .push(Diagnostic::new(code, message, self.source, span));
        self.diagnostics.last_mut().unwrap()
    }

    fn push(&mut self, kind: TokenKind, start: usize) {
        self.tokens
            .push(Token::new(kind, Span::new(start, self.pos)));
    }

    fn string(&mut self, quote: usize, c_string: bool) {
        self.pos = quote + 1;
        let mut out = Vec::new();
        loop {
            if self.pos >= self.bytes.len() || self.bytes[self.pos] == b'\n' {
                self.error(
                    "E0107",
                    "string literal is not closed on this line",
                    Span::new(quote, quote + 1),
                );
                break;
            }
            match self.bytes[self.pos] {
                b'"' => {
                    self.pos += 1;
                    break;
                }
                b'\\' => {
                    let esc_start = self.pos;
                    self.pos += 1;
                    let Some(esc) = self.bytes.get(self.pos).copied() else {
                        break;
                    };
                    self.pos += 1;
                    match esc {
                        b'n' => out.push(b'\n'),
                        b't' => out.push(b'\t'),
                        b'r' => out.push(b'\r'),
                        b'0' => out.push(0),
                        b'\\' => out.push(b'\\'),
                        b'"' => out.push(b'"'),
                        b'\'' => out.push(b'\''),
                        b'x' => {
                            let hi = self.hex_digit(self.pos);
                            let lo = self.hex_digit(self.pos + 1);
                            if let (Some(hi), Some(lo)) = (hi, lo) {
                                out.push(hi * 16 + lo);
                                self.pos += 2;
                            } else {
                                let end = (self.pos + 2).min(self.bytes.len());
                                self.error(
                                    "E0106",
                                    "`\\x` needs exactly two hexadecimal digits",
                                    Span::new(esc_start, end),
                                );
                                self.pos = end;
                            }
                        }
                        _ => {
                            self.error(
                                "E0106",
                                format!("unknown escape `\\{}`", esc as char),
                                Span::new(esc_start, self.pos),
                            );
                        }
                    }
                }
                b => {
                    out.push(b);
                    self.pos += 1;
                }
            }
        }
        let end = self.pos;
        self.tokens.push(Token::new(
            if c_string {
                TokenKind::CStr(out)
            } else {
                TokenKind::Str(out)
            },
            Span::new(quote, end),
        ));
    }

    fn hex_digit(&self, i: usize) -> Option<u8> {
        self.bytes.get(i).and_then(|b| match b {
            b'0'..=b'9' => Some(b - b'0'),
            b'a'..=b'f' => Some(b - b'a' + 10),
            b'A'..=b'F' => Some(b - b'A' + 10),
            _ => None,
        })
    }

    fn char_literal(&mut self, start: usize) {
        self.pos += 1;
        let mut value: Option<u8> = None;
        let mut count = 0usize;
        loop {
            if self.pos >= self.bytes.len() || self.bytes[self.pos] == b'\n' {
                self.error(
                    "E0107",
                    "character literal is not closed on this line",
                    Span::new(start, start + 1),
                );
                self.pos += 1;
                break;
            }
            match self.bytes[self.pos] {
                b'\'' => {
                    self.pos += 1;
                    break;
                }
                b'\\' => {
                    let esc_start = self.pos;
                    self.pos += 1;
                    let Some(esc) = self.bytes.get(self.pos).copied() else {
                        break;
                    };
                    self.pos += 1;
                    let byte = match esc {
                        b'n' => Some(b'\n'),
                        b't' => Some(b'\t'),
                        b'r' => Some(b'\r'),
                        b'0' => Some(0),
                        b'\\' => Some(b'\\'),
                        b'"' => Some(b'"'),
                        b'\'' => Some(b'\''),
                        b'x' => {
                            let hi = self.hex_digit(self.pos);
                            let lo = self.hex_digit(self.pos + 1);
                            if let (Some(hi), Some(lo)) = (hi, lo) {
                                self.pos += 2;
                                Some(hi * 16 + lo)
                            } else {
                                let end = (self.pos + 2).min(self.bytes.len());
                                self.error(
                                    "E0106",
                                    "`\\x` needs exactly two hexadecimal digits",
                                    Span::new(esc_start, end),
                                );
                                self.pos = end;
                                None
                            }
                        }
                        _ => {
                            self.error(
                                "E0106",
                                format!("unknown escape `\\{}`", esc as char),
                                Span::new(esc_start, self.pos),
                            );
                            None
                        }
                    };
                    count += 1;
                    if byte.is_some() {
                        value = byte;
                    }
                }
                b => {
                    count += 1;
                    if b < 0x80 {
                        value = Some(b);
                    } else {
                        let ch = self.source.text[self.pos..]
                            .chars()
                            .next()
                            .unwrap_or('\u{fffd}');
                        self.pos += ch.len_utf8();
                        count += 1;
                        continue;
                    }
                    self.pos += 1;
                }
            }
        }
        let span = Span::new(start, self.pos.min(self.bytes.len()).max(start + 1));
        if count != 1 {
            self.error(
                "E0108",
                "a character literal must hold exactly one byte",
                span,
            );
            self.tokens.push(Token::new(TokenKind::Int(0), span));
        } else {
            self.tokens
                .push(Token::new(TokenKind::Int(value.unwrap_or(0) as u128), span));
        }
    }

    fn number(&mut self, start: usize) {
        let mut radix = 10u32;
        if self.bytes[start] == b'0' {
            match self.peek(1) {
                Some(b'x') => {
                    radix = 16;
                    self.pos = start + 2;
                }
                Some(b'o') => {
                    radix = 8;
                    self.pos = start + 2;
                }
                Some(b'b') => {
                    radix = 2;
                    self.pos = start + 2;
                }
                _ => self.pos = start,
            }
        } else {
            self.pos = start;
        }
        let digits_start = self.pos;
        let mut digits = String::new();
        let mut last_underscore = false;
        while let Some(b) = self.peek(0) {
            let valid = if radix == 16 {
                b.is_ascii_hexdigit()
            } else {
                b.is_ascii_digit()
            };
            if valid {
                digits.push(b as char);
                last_underscore = false;
                self.pos += 1;
            } else if b == b'_' {
                if digits.is_empty() || last_underscore {
                    self.bad_number(start);
                }
                last_underscore = true;
                self.pos += 1;
            } else {
                break;
            }
        }
        if digits.is_empty() {
            self.bad_number(start);
        }
        let mut is_float = false;
        let tuple_field = start > 0 && self.bytes[start - 1] == b'.';
        if radix == 10 && !tuple_field {
            // `1..2` is an integer followed by a range, never a float.
            if self.peek(0) == Some(b'.') && self.peek(1) != Some(b'.') {
                is_float = true;
                self.pos += 1;
                let frac_start = self.pos;
                while let Some(b) = self.peek(0) {
                    if b.is_ascii_digit() || b == b'_' {
                        self.pos += 1;
                    } else {
                        break;
                    }
                }
                if self.pos == frac_start {
                    self.bad_number(start);
                }
            }
            if matches!(self.peek(0), Some(b'e') | Some(b'E')) {
                is_float = true;
                self.pos += 1;
                if matches!(self.peek(0), Some(b'+') | Some(b'-')) {
                    self.pos += 1;
                }
                let exp_start = self.pos;
                while let Some(b) = self.peek(0) {
                    if b.is_ascii_digit() {
                        self.pos += 1;
                    } else {
                        break;
                    }
                }
                if self.pos == exp_start {
                    self.bad_number(start);
                }
            }
        }
        let span = Span::new(start, self.pos);
        let text: String = self.source.text[start..self.pos].replace('_', "");
        if is_float {
            match text.parse::<f64>() {
                Ok(v) => self.tokens.push(Token::new(TokenKind::Float(v), span)),
                Err(_) => {
                    self.bad_number(start);
                    self.tokens.push(Token::new(TokenKind::Float(0.0), span));
                }
            }
        } else {
            match u128::from_str_radix(&text[if radix == 10 { 0 } else { 2 }..], radix) {
                Ok(v) => self.tokens.push(Token::new(TokenKind::Int(v), span)),
                Err(_) => {
                    self.bad_number(start);
                    self.tokens.push(Token::new(TokenKind::Int(0), span));
                }
            }
        }
        let _ = digits_start;
    }

    fn bad_number(&mut self, start: usize) {
        self.error(
            "E0105",
            "malformed numeric literal",
            Span::new(start, self.pos.max(start + 1)),
        );
    }

    fn builtin(&mut self, start: usize) {
        self.pos += 1;
        let name_start = self.pos;
        while let Some(b) = self.peek(0) {
            if b.is_ascii_alphanumeric() || b == b'_' {
                self.pos += 1;
            } else {
                break;
            }
        }
        let name = &self.source.text[name_start..self.pos];
        let span = Span::new(start, self.pos);
        if name.is_empty() {
            self.error("E0111", "`@` must be followed by a builtin name", span);
            return;
        }
        if BUILTINS.contains(&name) {
            self.tokens
                .push(Token::new(TokenKind::Builtin(name.to_string()), span));
        } else {
            let name_end = self.pos;
            let file = self.source.path.clone();
            let diag = self.error(
                "E0109",
                format!("`@{name}` is not a builtin"),
                Span::new(name_start, name_end),
            );
            if let Some((candidate, distance)) = closest(name, BUILTINS)
                && distance <= 2
            {
                let fix = Edit {
                    file,
                    span: Span::new(name_start, name_end),
                    text: candidate.to_string(),
                };
                diag.fix(format!("replace with `@{candidate}`"), fix);
            }
            self.tokens
                .push(Token::new(TokenKind::Builtin(name.to_string()), span));
        }
    }

    fn name(&mut self, start: usize) {
        while let Some(b) = self.peek(0) {
            if b.is_ascii_alphanumeric() || b == b'_' {
                self.pos += 1;
            } else {
                break;
            }
        }
        let text = &self.source.text[start..self.pos];
        let span = Span::new(start, self.pos);
        if let Some(scalar) = ScalarTy::from_name(text) {
            self.tokens
                .push(Token::new(TokenKind::Scalar(scalar), span));
            return;
        }
        if let Some(v) = vector_type(text) {
            self.tokens.push(Token::new(TokenKind::Vector(v), span));
            return;
        }
        let first = text.as_bytes()[0];
        if first.is_ascii_uppercase() {
            if text.contains('_') {
                if text
                    .bytes()
                    .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
                {
                    let lower = text.to_ascii_lowercase();
                    let file = self.source.path.clone();
                    let diag = self.error(
                        "E0104",
                        format!(
                            "`{text}` is an all-capitals name; constants are lower_snake: `{lower}`"
                        ),
                        span,
                    );
                    diag.fix(
                        format!("write `{lower}`"),
                        Edit {
                            file,
                            span,
                            text: lower,
                        },
                    );
                } else {
                    self.error(
                        "E0104",
                        format!("type identifier `{text}` must not contain `_`"),
                        span,
                    );
                }
            }
            self.tokens
                .push(Token::new(TokenKind::TypeIdent(text.to_string()), span));
        } else if first.is_ascii_lowercase() || first == b'_' {
            if text.bytes().any(|b| b.is_ascii_uppercase()) {
                self.error(
                    "E0104",
                    format!("value identifier `{text}` must be lowercase and must not contain uppercase letters"),
                    span,
                );
            }
            if let Some(kw) = Kw::from_spelling(text) {
                self.tokens.push(Token::new(TokenKind::Keyword(kw), span));
            } else {
                self.tokens
                    .push(Token::new(TokenKind::Ident(text.to_string()), span));
            }
        }
    }

    fn punct(&mut self, start: usize) -> bool {
        let rest = &self.bytes[self.pos..];
        // Longest first. `.` variants are part of the maximal-munch list.
        let table: &[(&[u8], Punct)] = &[
            (b".{", DotLBrace),
            (b".[", DotLBracket),
            (b".*", DotStar),
            (b"...", Ellipsis),
            (b"..=", DotDotEq),
            (b"..", DotDot),
            (b"->", Arrow),
            (b"=>", FatArrow),
            (b"<<=", ShlEq),
            (b">>=", ShrEq),
            (b"==", EqEq),
            (b"!=", BangEq),
            (b"<=", LtEq),
            (b">=", GtEq),
            (b"&&", AmpAmp),
            (b"||", PipePipe),
            (b"<<", Shl),
            (b">>", Shr),
            (b"+%=", PlusWrapEq),
            (b"-%=", MinusWrapEq),
            (b"*%=", StarWrapEq),
            (b"+|=", PlusSatEq),
            (b"-|=", MinusSatEq),
            (b"*|=", StarSatEq),
            (b"+%", PlusWrap),
            (b"-%", MinusWrap),
            (b"*%", StarWrap),
            (b"+|", PlusSat),
            (b"-|", MinusSat),
            (b"*|", StarSat),
            (b"+=", PlusEq),
            (b"-=", MinusEq),
            (b"*=", StarEq),
            (b"/=", SlashEq),
            (b"%=", PercentEq),
            (b"&=", AmpEq),
            (b"|=", PipeEq),
            (b"^=", CaretEq),
            (b"(", LParen),
            (b")", RParen),
            (b"{", LBrace),
            (b"}", RBrace),
            (b"[", LBracket),
            (b"]", RBracket),
            (b",", Comma),
            (b";", Semicolon),
            (b":", Colon),
            (b".", Dot),
            (b"&", Amp),
            (b"|", Pipe),
            (b"^", Caret),
            (b"~", Tilde),
            (b"!", Bang),
            (b"=", Eq),
            (b"<", Lt),
            (b">", Gt),
            (b"+", Plus),
            (b"-", Minus),
            (b"*", Star),
            (b"/", Slash),
            (b"%", Percent),
            (b"?", Question),
        ];
        for (text, punct) in table {
            if rest.starts_with(text) {
                self.pos += text.len();
                self.push(TokenKind::Punct(*punct), start);
                return true;
            }
        }
        false
    }
}

fn vector_type(text: &str) -> Option<VectorTy> {
    let (elem, lanes) = text.split_once('x')?;
    let elem = ScalarTy::from_name(elem)?;
    if !(elem.is_integer() || elem.is_float() || elem == ScalarTy::Bool) {
        return None;
    }
    if lanes.is_empty() || !lanes.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let lanes: u16 = lanes.parse().ok()?;
    Some(VectorTy { elem, lanes })
}

fn closest<'a>(name: &str, choices: &'a [&'a str]) -> Option<(&'a str, usize)> {
    choices
        .iter()
        .map(|c| (*c, levenshtein(name, c)))
        .min_by_key(|(_, d)| *d)
}

fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0; b.len() + 1];
    for i in 1..=a.len() {
        cur[0] = i;
        for j in 1..=b.len() {
            let cost = if a[i - 1] == b[j - 1] { 0 } else { 1 };
            cur[j] = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}
