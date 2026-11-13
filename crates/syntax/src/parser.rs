//! Phase 3: recursive-descent parser.
//!
//! The parser keeps a span on every node and reports at most one syntax error per
//! statement or item header. Multi-token operators and the two-token lookahead
//! cases are handled here; the parenthesization rule and chained comparisons are
//! checked once the binary tree for an expression exists.

use crate::ast::*;
use crate::token::Punct::*;
use crate::token::{Kw, Punct, ScalarTy, Token, TokenKind};
use ingot_diag::diagnostic::{Diagnostic, Edit};
use ingot_diag::source::{Source, Span};

pub fn parse(source: &Source, tokens: Vec<Token>) -> (Module, Vec<Diagnostic>) {
    let mut p = Parser {
        source,
        tokens,
        pos: 0,
        diagnostics: Vec::new(),
        in_expr_branch: false,
    };
    let module = p.module();
    (module, p.diagnostics)
}

#[derive(Clone, Copy, Default)]
struct Modifiers {
    public: bool,
    inline: bool,
    noinline: bool,
    unsafe_: bool,
    export: bool,
    kernel: bool,
    waitable: bool,
}

struct Parser<'a> {
    source: &'a Source,
    tokens: Vec<Token>,
    pos: usize,
    diagnostics: Vec<Diagnostic>,
    in_expr_branch: bool,
}

impl<'a> Parser<'a> {
    fn peek(&self) -> &Token {
        &self.tokens[self.pos.min(self.tokens.len() - 1)]
    }

    fn peek_at(&self, n: usize) -> &Token {
        &self.tokens[(self.pos + n).min(self.tokens.len() - 1)]
    }

    fn kind(&self) -> &TokenKind {
        &self.peek().kind
    }

    fn span(&self) -> Span {
        self.peek().span
    }

    fn bump(&mut self) -> Token {
        let t = self.peek().clone();
        if self.pos + 1 < self.tokens.len() {
            self.pos += 1;
        }
        t
    }

    fn at(&self, p: Punct) -> bool {
        self.kind().is_punct(p)
    }

    fn at_kw(&self, k: Kw) -> bool {
        self.kind().is_kw(k)
    }

    fn at_eof(&self) -> bool {
        matches!(self.kind(), TokenKind::Eof)
    }

    fn eat(&mut self, p: Punct) -> bool {
        if self.at(p) {
            self.bump();
            true
        } else {
            false
        }
    }

    fn eat_kw(&mut self, k: Kw) -> bool {
        if self.at_kw(k) {
            self.bump();
            true
        } else {
            false
        }
    }

    fn diag(
        &mut self,
        code: &'static str,
        message: impl Into<String>,
        span: Span,
    ) -> &mut Diagnostic {
        self.diagnostics
            .push(Diagnostic::new(code, message, self.source, span));
        self.diagnostics.last_mut().unwrap()
    }

    fn unexpected(&mut self) -> Span {
        let span = self.span();
        let desc = self.kind().describe();
        self.diag("E0201", format!("unexpected {desc}"), span);
        span
    }

    fn expect(&mut self, p: Punct) -> Span {
        if self.at(p) {
            self.bump().span
        } else {
            let span = self.span();
            let desc = self.kind().describe();
            self.diag(
                "E0201",
                format!("expected `{}`, found {desc}", p.as_str()),
                span,
            );
            Span::empty(span.start)
        }
    }

    fn expect_ident(&mut self) -> (String, Span) {
        match self.kind().clone() {
            TokenKind::Ident(name) => {
                let span = self.bump().span;
                (name, span)
            }
            TokenKind::TypeIdent(name) => {
                let span = self.bump().span;
                self.diag(
                    "E0212",
                    format!("`{name}` is a type identifier, not a value name"),
                    span,
                );
                (name, span)
            }
            TokenKind::Keyword(k) => {
                let span = self.bump().span;
                self.diag(
                    "E0212",
                    format!("`{}` is a keyword and cannot be a name", k.as_str()),
                    span,
                );
                (k.as_str().to_string(), span)
            }
            _ => {
                let span = self.unexpected();
                (String::new(), span)
            }
        }
    }

    /// `copy` is a contextual modifier before `struct` and `enum`; the
    /// standard library also needs it as the name of `mem.copy`.
    fn fn_name(&mut self) -> (String, Span) {
        if self.at_kw(Kw::Copy) {
            let span = self.bump().span;
            return ("copy".to_string(), span);
        }
        self.expect_ident()
    }

    /// `align` is a contextual keyword after a type; it is still a useful
    /// parameter name in allocator signatures.
    fn param_name(&mut self) -> (String, Span) {
        if self.at_kw(Kw::Align) {
            let span = self.bump().span;
            return ("align".to_string(), span);
        }
        self.expect_ident()
    }

    fn expect_type_ident(&mut self) -> (String, Span) {
        match self.kind().clone() {
            TokenKind::TypeIdent(name) => {
                let span = self.bump().span;
                (name, span)
            }
            TokenKind::Ident(name) => {
                let span = self.bump().span;
                self.diag(
                    "E0212",
                    format!("`{name}` is a value identifier, not a type name"),
                    span,
                );
                (name, span)
            }
            TokenKind::Keyword(k) => {
                let span = self.bump().span;
                self.diag(
                    "E0212",
                    format!("`{}` is a keyword and cannot be a type name", k.as_str()),
                    span,
                );
                (k.as_str().to_string(), span)
            }
            _ => {
                let span = self.unexpected();
                (String::new(), span)
            }
        }
    }

    fn expect_semi(&mut self, previous_end: usize) {
        if self.eat(Semicolon) {
            return;
        }
        let file = self.source.path.clone();
        if self.at(RBrace) || self.at_eof() {
            let at = Span::empty(previous_end);
            self.diag("E0202", "missing `;`", at).fix(
                "insert `;`",
                Edit {
                    file,
                    span: at,
                    text: ";".to_string(),
                },
            );
            return;
        }
        let at = Span::empty(previous_end);
        self.diag("E0202", "missing `;` between statements", at)
            .fix(
                "insert `;`",
                Edit {
                    file,
                    span: at,
                    text: ";".to_string(),
                },
            );
    }

    // ---------------------------------------------------------------- module

    fn module(&mut self) -> Module {
        let start = self.span().start;
        let mut imports = Vec::new();
        let mut items = Vec::new();
        let mut seen_item = false;
        while !self.at_eof() {
            if self.at_kw(Kw::Import) {
                if seen_item {
                    self.diag(
                        "E0210",
                        "`import` must come before the first item",
                        self.span(),
                    );
                }
                imports.push(self.import());
            } else {
                seen_item = true;
                let before = self.pos;
                let item = self.item(Modifiers::default());
                if !matches!(item, Item::Poison) {
                    items.push(item);
                }
                if self.pos == before {
                    self.unexpected();
                    self.bump();
                }
            }
        }
        let end = self.span().end;
        Module {
            imports,
            items,
            span: Span::new(start, end),
        }
    }

    fn import(&mut self) -> Import {
        let start = self.bump().span.start; // import
        let mut segments = Vec::new();
        let (first, first_span) = self.expect_ident();
        segments.push((first, first_span));
        while self.eat(Dot) {
            if let TokenKind::TypeIdent(_) = self.kind() {
                let (name, span) = self.expect_type_ident();
                segments.push((name, span));
                break;
            }
            if self.at_kw(Kw::Copy) {
                let span = self.bump().span;
                segments.push(("copy".to_string(), span));
                break;
            }
            let (name, span) = self.expect_ident();
            segments.push((name, span));
        }
        let mut alias = None;
        let mut alias_span = None;
        if self.eat_kw(Kw::As) {
            match self.kind().clone() {
                TokenKind::Ident(_) => {
                    let (name, span) = self.expect_ident();
                    alias = Some(name);
                    alias_span = Some(span);
                }
                TokenKind::TypeIdent(_) => {
                    let (name, span) = self.expect_type_ident();
                    alias = Some(name);
                    alias_span = Some(span);
                }
                _ => {
                    let span = self.unexpected();
                    alias_span = Some(span);
                }
            }
        }
        let end = self.expect(Semicolon).end;
        Import {
            segments,
            alias,
            alias_span,
            span: Span::new(start, end),
        }
    }

    // ----------------------------------------------------------------- items

    fn item(&mut self, mut mods: Modifiers) -> Item {
        let start = self.span().start;
        loop {
            if self.at_kw(Kw::Pub) {
                if mods.public {
                    self.diag("E0213", "duplicate `pub` modifier", self.span());
                }
                mods.public = true;
                self.bump();
                continue;
            }
            let flag = match self.kind() {
                TokenKind::Keyword(Kw::Inline) if !mods.inline => Some(0),
                TokenKind::Keyword(Kw::Noinline) if !mods.noinline => Some(1),
                TokenKind::Keyword(Kw::Unsafe) if !mods.unsafe_ => Some(2),
                TokenKind::Keyword(Kw::Export) if !mods.export => Some(3),
                TokenKind::Keyword(Kw::Kernel) if !mods.kernel => Some(4),
                TokenKind::Keyword(Kw::Waitable) if !mods.waitable => Some(5),
                _ => None,
            };
            if let Some(flag) = flag {
                match flag {
                    0 => mods.inline = true,
                    1 => mods.noinline = true,
                    2 => mods.unsafe_ = true,
                    3 => mods.export = true,
                    4 => mods.kernel = true,
                    5 => mods.waitable = true,
                    _ => {}
                }
                self.bump();
                continue;
            }
            break;
        }
        if mods.inline && mods.noinline {
            self.diag(
                "E0213",
                "`inline` and `noinline` cannot be combined",
                self.span(),
            );
        }
        let start = start.min(self.span().start);
        match self.kind().clone() {
            TokenKind::Keyword(Kw::Fn) => Item::Fn(self.fn_decl(mods, start)),
            TokenKind::Keyword(Kw::Struct) => {
                if mods.export || mods.kernel || mods.waitable || mods.unsafe_ {
                    self.diag("E0213", "modifier is not allowed on a struct", self.span());
                }
                Item::Struct(self.struct_decl(StructKind::Plain, start))
            }
            TokenKind::Keyword(Kw::Enum) => {
                if mods.export || mods.kernel || mods.waitable || mods.unsafe_ {
                    self.diag("E0213", "modifier is not allowed on an enum", self.span());
                }
                Item::Enum(self.enum_decl(false, start))
            }
            TokenKind::Keyword(Kw::Copy) => {
                let copy_span = self.bump().span;
                if self.at_kw(Kw::Struct) {
                    Item::Struct(self.struct_decl(StructKind::Copy, start))
                } else if self.at_kw(Kw::Enum) {
                    Item::Enum(self.enum_decl(true, start))
                } else {
                    self.diag(
                        "E0201",
                        "`copy` must be followed by `struct` or `enum`",
                        copy_span,
                    );
                    self.recover_item();
                    Item::Poison
                }
            }
            TokenKind::Keyword(Kw::Linear) => {
                let span = self.bump().span;
                if self.at_kw(Kw::Struct) {
                    Item::Struct(self.struct_decl(StructKind::Linear, start))
                } else {
                    self.diag("E0201", "`linear` must be followed by `struct`", span);
                    self.recover_item();
                    Item::Poison
                }
            }
            TokenKind::Keyword(Kw::Interface) => Item::Interface(self.interface_decl(start)),
            TokenKind::Keyword(Kw::Impl) => Item::Impl(self.impl_decl(mods.unsafe_, start)),
            TokenKind::Keyword(Kw::Const) => Item::Const(self.const_decl(start)),
            TokenKind::Keyword(Kw::Static) => Item::Static(self.static_decl(start)),
            TokenKind::Keyword(Kw::Type) => Item::TypeAlias(self.type_alias(start)),
            TokenKind::Keyword(Kw::Extern) => Item::Extern(self.extern_block(start)),
            TokenKind::Keyword(Kw::Test) | TokenKind::Keyword(Kw::Bench) => {
                Item::Test(self.test_decl(start))
            }
            TokenKind::Keyword(Kw::When) => Item::When(self.when_items(start)),
            _ => {
                self.unexpected();
                self.recover_item();
                Item::Poison
            }
        }
    }

    fn recover_item(&mut self) {
        let mut depth = 0i32;
        while !self.at_eof() {
            match self.kind() {
                TokenKind::Punct(LBrace) => {
                    depth += 1;
                    self.bump();
                }
                TokenKind::Punct(RBrace) => {
                    if depth == 0 {
                        return;
                    }
                    depth -= 1;
                    self.bump();
                    if depth == 0 {
                        return;
                    }
                }
                TokenKind::Punct(Semicolon) if depth == 0 => {
                    self.bump();
                    return;
                }
                TokenKind::Keyword(
                    Kw::Fn
                    | Kw::Struct
                    | Kw::Enum
                    | Kw::Interface
                    | Kw::Impl
                    | Kw::Const
                    | Kw::Static
                    | Kw::Type
                    | Kw::Extern
                    | Kw::Test
                    | Kw::Bench
                    | Kw::When,
                ) if depth == 0 => return,
                _ => {
                    self.bump();
                }
            }
        }
    }

    fn fn_decl(&mut self, mods: Modifiers, start: usize) -> FnDecl {
        self.fn_decl_in(mods, start, false)
    }

    /// `in_extern` is true for a function inside an `extern` block, whose
    /// parameter list may end in `...`.
    fn fn_decl_in(&mut self, mods: Modifiers, start: usize, in_extern: bool) -> FnDecl {
        self.bump(); // fn
        let (name, name_span) = self.fn_name();
        let generics = self.generics();
        let (params, variadic) = self.params(in_extern);
        let (ret, error, from, reduce) = self.result();
        let body = if self.at(LBrace) {
            Some(self.block())
        } else {
            self.expect_semi(self.span().start);
            None
        };
        let end = body.as_ref().map(|b| b.span.end).unwrap_or(self.span().end);
        FnDecl {
            name,
            name_span,
            params,
            ret,
            error,
            from,
            reduce,
            mods: FnMods {
                public: mods.public,
                inline: mods.inline,
                noinline: mods.noinline,
                unsafe_: mods.unsafe_,
                export: mods.export,
                kernel: mods.kernel,
                waitable: mods.waitable,
            },
            generics,
            body,
            variadic,
            span: Span::new(start, end),
        }
    }

    fn generics(&mut self) -> Vec<GenericParam> {
        if !self.eat(LBracket) {
            return Vec::new();
        }
        let mut out = Vec::new();
        while !self.at(RBracket) && !self.at_eof() {
            let start = self.span().start;
            let is_const = self.eat_kw(Kw::Const);
            let (name, name_span) = self.expect_type_ident();
            let mut bounds = Vec::new();
            let mut const_ty = None;
            if is_const {
                self.expect(Colon);
                const_ty = Some(self.ty());
            } else if self.eat(Colon) {
                loop {
                    bounds.push(self.bound());
                    if !self.eat(Plus) {
                        break;
                    }
                }
            }
            let end = self.span().end;
            out.push(GenericParam {
                name,
                name_span,
                is_const,
                bounds,
                const_ty,
                span: Span::new(start, end),
            });
            if !self.eat(Comma) {
                break;
            }
        }
        self.expect(RBracket);
        out
    }

    fn bound(&mut self) -> PathTy {
        // `Fn(A) -> R` is represented as a path named Fn with the component types
        // as arguments; the checker treats the built-in bounds specially.
        if let TokenKind::TypeIdent(name) = self.kind().clone()
            && matches!(name.as_str(), "Fn" | "FnMut" | "FnOnce")
            && self.peek_at(1).kind.is_punct(LParen)
            && let TokenKind::TypeIdent(name) = self.bump().kind
        {
            let start = self.span().start;
            self.expect(LParen);
            let mut args = Vec::new();
            while !self.at(RParen) && !self.at_eof() {
                args.push(GenericArg::Ty(self.ty()));
                if !self.eat(Comma) {
                    break;
                }
            }
            let close = self.expect(RParen);
            if self.eat(Arrow) {
                args.push(GenericArg::Ty(self.ty()));
                if self.eat(Bang) {
                    args.push(GenericArg::Ty(self.ty()));
                }
            }
            return PathTy {
                segments: vec![(name, close)],
                args,
                args_span: Some(close),
                assoc: None,
                span: Span::new(start, close.end),
            };
        }
        self.path_ty()
    }

    /// Parses a parameter list. With `in_extern`, the list may instead end in `...`
    /// (grammar: `ext_params`); the returned flag records it. The `...` must be the
    /// last item, so anything after it is a syntax error at the closing `)`.
    fn params(&mut self, in_extern: bool) -> (Vec<Param>, bool) {
        let mut out = Vec::new();
        let mut variadic = false;
        self.expect(LParen);
        while !self.at(RParen) && !self.at_eof() {
            if in_extern && self.at(Ellipsis) {
                self.bump();
                variadic = true;
                break;
            }
            let start = self.span().start;
            let (name, name_span) = self.param_name();
            self.expect(Colon);
            let ty = self.ty();
            let mut into = None;
            if self.eat_kw(Kw::Into) {
                let (pname, pspan) = self.expect_ident();
                into = Some((pname, pspan));
            }
            let end = self.span().end;
            out.push(Param {
                name,
                name_span,
                ty,
                into,
                span: Span::new(start, end),
            });
            if !self.eat(Comma) {
                break;
            }
        }
        self.expect(RParen);
        (out, variadic)
    }

    fn result(&mut self) -> (Ty, Option<Ty>, Vec<Origin>, Option<String>) {
        let mut ret = Ty::Scalar(ScalarTy::Void, self.span());
        let mut error = None;
        let mut from = Vec::new();
        let mut reduce = None;
        if self.eat(Arrow) {
            ret = self.ty();
            if self.eat(Bang) {
                error = Some(self.ty());
            }
            if self.eat_kw(Kw::From) {
                loop {
                    let start = self.span().start;
                    let is_static = self.eat_kw(Kw::Static);
                    if is_static {
                        from.push(Origin {
                            name: "static".to_string(),
                            inner: false,
                            is_static: true,
                            span: Span::new(start, self.span().end),
                        });
                    } else {
                        let (name, name_span) = self.expect_ident();
                        let inner = self.eat(DotStar);
                        from.push(Origin {
                            name,
                            inner,
                            is_static: false,
                            span: Span::new(start, name_span.end),
                        });
                    }
                    if !self.eat(Comma) {
                        break;
                    }
                }
            }
            if self.at_kw(Kw::From) {
                // `from` is contextual only after a result; a second one is a syntax error.
                self.unexpected();
            }
            if self.at_ident("reduce") && self.peek_at(1).kind.is_punct(LParen) {
                self.bump();
                self.bump();
                let (op, _) = match self.kind().clone() {
                    TokenKind::Ident(s) => {
                        let s2 = s.clone();
                        self.bump();
                        (s2, ())
                    }
                    TokenKind::Punct(p) => {
                        let s = p.as_str().to_string();
                        self.bump();
                        (s, ())
                    }
                    _ => {
                        let _span = self.unexpected();
                        (String::new(), ())
                    }
                };
                let _ = op;
                reduce = Some(String::new());
                self.expect(RParen);
            }
        }
        (ret, error, from, reduce)
    }

    fn struct_decl(&mut self, kind: StructKind, start: usize) -> StructDecl {
        self.bump(); // struct
        let (name, name_span) = self.expect_type_ident();
        let generics = self.generics();
        let align = if self.at_kw(Kw::Align) {
            Some(self.align_expr())
        } else {
            None
        };
        self.expect(LBrace);
        let mut fields = Vec::new();
        while !self.at(RBrace) && !self.at_eof() {
            let fstart = self.span().start;
            let public = self.eat_kw(Kw::Pub);
            let (fname, fspan) = self.expect_ident();
            self.expect(Colon);
            let ty = self.ty();
            let align = if self.at_kw(Kw::Align) {
                Some(self.align_expr())
            } else {
                None
            };
            let end = self.span().end;
            fields.push(FieldDecl {
                name: fname,
                name_span: fspan,
                public,
                ty,
                align,
                span: Span::new(fstart, end),
            });
            if !self.eat(Comma) {
                break;
            }
        }
        let close = self.expect(RBrace);
        StructDecl {
            name,
            name_span,
            kind,
            generics,
            fields,
            align,
            span: Span::new(start, close.end),
        }
    }

    fn align_expr(&mut self) -> Expr {
        self.bump(); // align
        self.expect(LParen);
        let e = self.expr();
        self.expect(RParen);
        e
    }

    fn enum_decl(&mut self, copy: bool, start: usize) -> EnumDecl {
        self.bump(); // enum
        let (name, name_span) = self.expect_type_ident();
        let generics = self.generics();
        let tag = if self.eat(Colon) {
            Some(self.ty())
        } else {
            None
        };
        self.expect(LBrace);
        let mut variants = Vec::new();
        while !self.at(RBrace) && !self.at_eof() {
            let vstart = self.span().start;
            let (vname, vspan) = self.expect_ident();
            let mut payload = None;
            let mut from = false;
            if self.eat(Colon) {
                from = self.eat_kw(Kw::From);
                payload = Some(self.ty());
            }
            let value = if self.eat(Eq) {
                Some(self.expr())
            } else {
                None
            };
            let end = self.span().end;
            variants.push(VariantDecl {
                name: vname,
                name_span: vspan,
                payload,
                from,
                value,
                span: Span::new(vstart, end),
            });
            if !self.eat(Comma) {
                break;
            }
        }
        let close = self.expect(RBrace);
        EnumDecl {
            name,
            name_span,
            copy,
            tag,
            generics,
            variants,
            span: Span::new(start, close.end),
        }
    }

    fn member_modifiers(&mut self) -> Modifiers {
        let mut mods = Modifiers::default();
        loop {
            if self.eat_kw(Kw::Pub) {
                mods.public = true;
            } else if self.eat_kw(Kw::Inline) {
                mods.inline = true;
            } else if self.eat_kw(Kw::Noinline) {
                mods.noinline = true;
            } else if self.eat_kw(Kw::Unsafe) {
                mods.unsafe_ = true;
            } else if self.eat_kw(Kw::Export) {
                mods.export = true;
            } else if self.eat_kw(Kw::Kernel) {
                mods.kernel = true;
            } else if self.eat_kw(Kw::Waitable) {
                mods.waitable = true;
            } else {
                break;
            }
        }
        mods
    }

    fn interface_decl(&mut self, start: usize) -> InterfaceDecl {
        self.bump();
        let (name, name_span) = self.expect_type_ident();
        let generics = self.generics();
        let mut bounds = Vec::new();
        if self.eat(Colon) {
            loop {
                bounds.push(self.bound());
                if !self.eat(Plus) {
                    break;
                }
            }
        }
        self.expect(LBrace);
        let mut members = Vec::new();
        while !self.at(RBrace) && !self.at_eof() {
            let mods = self.member_modifiers();
            if self.eat_kw(Kw::Type) {
                let (n, sp) = self.expect_type_ident();
                let end = self.expect(Semicolon).end;
                members.push(Item::TypeAlias(TypeAlias {
                    name: n,
                    name_span: sp,
                    generics: Vec::new(),
                    ty: Ty::Poison(sp),
                    span: Span::new(sp.start, end),
                }));
            } else if self.at_kw(Kw::Fn) {
                members.push(Item::Fn(self.fn_decl(mods, self.span().start)));
            } else {
                self.unexpected();
                self.recover_item();
            }
        }
        let close = self.expect(RBrace);
        InterfaceDecl {
            name,
            name_span,
            generics,
            bounds,
            members,
            span: Span::new(start, close.end),
        }
    }

    fn impl_decl(&mut self, unsafe_: bool, start: usize) -> ImplDecl {
        self.bump(); // impl
        let generics = self.generics();
        let first = self.ty();
        let (interface, target) = if self.eat_kw(Kw::For) {
            let target = self.ty();
            let iface = match first {
                Ty::Path(p) => Some(p),
                other => {
                    self.diag(
                        "E0201",
                        "the interface before `for` must be a named type",
                        other.span(),
                    );
                    None
                }
            };
            (iface, target)
        } else {
            (None, first)
        };
        self.expect(LBrace);
        let mut members = Vec::new();
        while !self.at(RBrace) && !self.at_eof() {
            let mods = self.member_modifiers();
            match self.kind().clone() {
                TokenKind::Keyword(Kw::Fn) => {
                    members.push(Item::Fn(self.fn_decl(mods, self.span().start)));
                }
                TokenKind::Keyword(Kw::Const) => {
                    members.push(Item::Const(self.const_decl(self.span().start)));
                }
                TokenKind::Keyword(Kw::Type) => {
                    members.push(Item::TypeAlias(self.type_alias(self.span().start)));
                }
                _ => {
                    self.unexpected();
                    self.recover_item();
                }
            }
        }
        let close = self.expect(RBrace);
        ImplDecl {
            unsafe_,
            generics,
            interface,
            target,
            members,
            span: Span::new(start, close.end),
        }
    }

    fn const_decl(&mut self, start: usize) -> ConstDecl {
        self.bump();
        let (name, name_span) = self.expect_ident();
        self.expect(Colon);
        let ty = self.ty();
        self.expect(Eq);
        let value = self.expr();
        let end = self.expect(Semicolon).end;
        ConstDecl {
            name,
            name_span,
            ty,
            value,
            span: Span::new(start, end),
        }
    }

    fn static_decl(&mut self, start: usize) -> StaticDecl {
        self.bump();
        let (name, name_span) = self.expect_ident();
        self.expect(Colon);
        let ty = self.ty();
        let align = if self.at_kw(Kw::Align) {
            Some(self.align_expr())
        } else {
            None
        };
        self.expect(Eq);
        let value = self.expr();
        let end = self.expect(Semicolon).end;
        StaticDecl {
            name,
            name_span,
            ty,
            align,
            value,
            span: Span::new(start, end),
        }
    }

    fn type_alias(&mut self, start: usize) -> TypeAlias {
        self.bump();
        let (name, name_span) = self.expect_type_ident();
        let generics = self.generics();
        self.expect(Eq);
        let ty = self.ty();
        let end = self.expect(Semicolon).end;
        TypeAlias {
            name,
            name_span,
            generics,
            ty,
            span: Span::new(start, end),
        }
    }

    fn extern_block(&mut self, start: usize) -> ExternBlock {
        self.bump();
        let mut abi = String::new();
        let mut abi_span = self.span();
        if let TokenKind::Str(bytes) = self.kind().clone() {
            abi_span = self.bump().span;
            abi = String::from_utf8_lossy(&bytes).into_owned();
        } else {
            self.unexpected();
        }
        self.expect(LBrace);
        let mut functions = Vec::new();
        while !self.at(RBrace) && !self.at_eof() {
            if self.at_kw(Kw::Fn) {
                let fstart = self.span().start;
                functions.push(self.fn_decl_in(Modifiers::default(), fstart, true));
            } else {
                self.unexpected();
                self.recover_item();
            }
        }
        let close = self.expect(RBrace);
        ExternBlock {
            abi,
            abi_span,
            functions,
            span: Span::new(start, close.end),
        }
    }

    fn test_decl(&mut self, start: usize) -> TestDecl {
        self.bump();
        let name = if let TokenKind::Str(bytes) = self.kind().clone() {
            self.bump();
            String::from_utf8_lossy(&bytes).into_owned()
        } else {
            let span = self.unexpected();
            format!("test@{}", span.start)
        };
        let body = self.block();
        TestDecl {
            name,
            span: Span::new(start, body.span.end),
            body,
        }
    }

    fn when_items(&mut self, start: usize) -> WhenItems {
        self.bump();
        let condition = self.expr();
        let then = self.item_block();
        let otherwise = if self.eat_kw(Kw::Else) {
            if self.at_kw(Kw::When) {
                vec![Item::When(self.when_items(self.span().start))]
            } else {
                self.item_block()
            }
        } else {
            Vec::new()
        };
        let end = otherwise
            .last()
            .map(|i| i.span().end)
            .unwrap_or(self.span().end);
        WhenItems {
            condition,
            then,
            otherwise,
            span: Span::new(start, end),
        }
    }

    fn item_block(&mut self) -> Vec<Item> {
        self.expect(LBrace);
        let mut items = Vec::new();
        while !self.at(RBrace) && !self.at_eof() {
            let before = self.pos;
            let item = self.item(Modifiers::default());
            if !matches!(item, Item::Poison) {
                items.push(item);
            }
            if self.pos == before {
                self.bump();
            }
        }
        self.expect(RBrace);
        items
    }

    // ----------------------------------------------------------------- types

    fn path_ty(&mut self) -> PathTy {
        let start = self.span().start;
        let mut segments = Vec::new();
        // Module components are value identifiers. The final component is a type.
        while matches!(self.kind(), TokenKind::Ident(_))
            && self.peek_at(1).kind.is_punct(Dot)
            && matches!(
                self.peek_at(2).kind,
                TokenKind::Ident(_) | TokenKind::TypeIdent(_)
            )
        {
            let (name, span) = self.expect_ident();
            segments.push((name, span));
            self.expect(Dot);
        }
        let (name, name_span) = if let TokenKind::Ident(_) = self.kind() {
            // Fallback for a malformed path: keep the parser moving.
            self.expect_type_ident()
        } else {
            self.expect_type_ident()
        };
        segments.push((name, name_span));
        let mut args = Vec::new();
        let mut args_span = None;
        if self.at(LBracket) {
            let open = self.span();
            self.bump();
            while !self.at(RBracket) && !self.at_eof() {
                args.push(self.generic_arg());
                if !self.eat(Comma) {
                    break;
                }
            }
            let close = self.expect(RBracket);
            args_span = Some(open.join(close));
        }
        let assoc = if self.at(Dot) && matches!(self.peek_at(1).kind, TokenKind::TypeIdent(_)) {
            self.bump();
            let (n, sp) = self.expect_type_ident();
            Some((n, sp))
        } else {
            None
        };
        let end = assoc
            .as_ref()
            .map(|(_, s)| s.end)
            .or_else(|| args_span.map(|s| s.end))
            .unwrap_or(name_span.end);
        PathTy {
            segments,
            args,
            args_span,
            assoc,
            span: Span::new(start, end),
        }
    }

    fn generic_arg(&mut self) -> GenericArg {
        match self.kind() {
            TokenKind::Int(_) | TokenKind::Keyword(Kw::True) | TokenKind::Keyword(Kw::False) => {
                GenericArg::Const(self.expr())
            }
            TokenKind::Punct(LBrace) => {
                let open = self.bump().span;
                let e = self.expr();
                let close = self.expect(RBrace);
                GenericArg::Const(Expr::new(ExprKind::Paren(Box::new(e)), open.join(close)))
            }
            _ => GenericArg::Ty(self.ty()),
        }
    }

    fn ty(&mut self) -> Ty {
        let start = self.span().start;
        match self.kind().clone() {
            TokenKind::Scalar(s) => {
                let sp = self.bump().span;
                Ty::Scalar(s, sp)
            }
            TokenKind::Vector(v) => {
                let sp = self.bump().span;
                Ty::Vector(v, sp)
            }
            TokenKind::Punct(Amp) => {
                self.bump();
                let mutable = self.eat_kw(Kw::Mut);
                if self.eat_kw(Kw::Dyn) {
                    let interface = self.path_ty();
                    let end = self.span().end;
                    return Ty::Dyn {
                        interface: Box::new(interface),
                        mutable,
                        span: Span::new(start, end),
                    };
                }
                let inner = self.ty();
                let end = inner.span().end;
                Ty::Borrow {
                    mutable,
                    inner: Box::new(inner),
                    span: Span::new(start, end),
                }
            }
            TokenKind::Punct(LBracket) => {
                self.bump();
                if self.eat(RBracket) {
                    let mutable = self.eat_kw(Kw::Mut);
                    let elem = self.ty();
                    let end = elem.span().end;
                    Ty::Slice {
                        mutable,
                        elem: Box::new(elem),
                        span: Span::new(start, end),
                    }
                } else {
                    let len = self.expr();
                    self.expect(RBracket);
                    let elem = self.ty();
                    let end = elem.span().end;
                    Ty::Array {
                        len: Box::new(len),
                        elem: Box::new(elem),
                        span: Span::new(start, end),
                    }
                }
            }
            TokenKind::Punct(Star) => {
                self.bump();
                let mutable = self.eat_kw(Kw::Mut);
                let inner = self.ty();
                let end = inner.span().end;
                Ty::Ptr {
                    mutable,
                    inner: Box::new(inner),
                    span: Span::new(start, end),
                }
            }
            TokenKind::Punct(Question) => {
                self.bump();
                let inner = self.ty();
                let end = inner.span().end;
                Ty::Optional {
                    inner: Box::new(inner),
                    span: Span::new(start, end),
                }
            }
            TokenKind::Punct(LParen) => {
                self.bump();
                let mut elems = Vec::new();
                while !self.at(RParen) && !self.at_eof() {
                    elems.push(self.ty());
                    if !self.eat(Comma) {
                        break;
                    }
                }
                let close = self.expect(RParen);
                Ty::Tuple {
                    elems,
                    span: Span::new(start, close.end),
                }
            }
            TokenKind::Keyword(Kw::Extern) => {
                self.bump();
                if !self.at_kw(Kw::Fn) {
                    self.unexpected();
                }
                self.fn_type(start)
            }
            TokenKind::Keyword(Kw::Fn) => self.fn_type(start),
            TokenKind::TypeIdent(_) | TokenKind::Ident(_) => Ty::Path(self.path_ty()),
            _ => {
                let span = self.unexpected();
                Ty::Poison(span)
            }
        }
    }

    fn fn_type(&mut self, start: usize) -> Ty {
        self.bump(); // fn
        self.expect(LParen);
        let mut params = Vec::new();
        while !self.at(RParen) && !self.at_eof() {
            params.push(self.ty());
            if !self.eat(Comma) {
                break;
            }
        }
        self.expect(RParen);
        let mut ret = Ty::Scalar(ScalarTy::Void, Span::empty(self.span().start));
        let mut error = None;
        if self.eat(Arrow) {
            ret = self.ty();
            if self.eat(Bang) {
                error = Some(Box::new(self.ty()));
            }
        }
        let end = error
            .as_ref()
            .map(|t| t.span().end)
            .unwrap_or_else(|| ret.span().end);
        Ty::Fn {
            params,
            ret: Box::new(ret),
            error,
            span: Span::new(start, end),
        }
    }

    // ------------------------------------------------------------ statements

    fn block(&mut self) -> Block {
        let open = self.expect(LBrace);
        let mut stmts = Vec::new();
        while !self.at(RBrace) && !self.at_eof() {
            let before = self.pos;
            let stmt = self.stmt();
            stmts.push(stmt);
            if self.pos == before {
                self.bump();
            }
        }
        let close = self.expect(RBrace);
        Block {
            stmts,
            span: open.join(close),
        }
    }

    fn stmt(&mut self) -> Stmt {
        let start = self.span().start;
        // label: for/while/loop
        if matches!(self.kind(), TokenKind::Ident(_))
            && self.peek_at(1).kind.is_punct(Colon)
            && matches!(
                self.peek_at(2).kind,
                TokenKind::Keyword(Kw::For | Kw::While | Kw::Loop | Kw::Par)
            )
        {
            let (label, _) = self.expect_ident();
            self.expect(Colon);
            let label = Some(label);
            if self.at_kw(Kw::For) {
                return self.for_stmt(label, false, None, start);
            }
            if self.at_kw(Kw::Par) {
                return self.par_for_stmt(label, start);
            }
            if self.at_kw(Kw::While) {
                return self.while_stmt(start);
            }
            return self.loop_stmt(label, start);
        }
        match self.kind().clone() {
            TokenKind::Keyword(Kw::Let) => self.let_stmt(false, start),
            TokenKind::Keyword(Kw::Var) => self.let_stmt(true, start),
            TokenKind::Keyword(Kw::If) => self.if_stmt(start),
            TokenKind::Keyword(Kw::Match) => self.match_stmt(start),
            TokenKind::Keyword(Kw::While) => self.while_stmt(start),
            TokenKind::Keyword(Kw::Loop) => self.loop_stmt(None, start),
            TokenKind::Keyword(Kw::For) => self.for_stmt(None, false, None, start),
            TokenKind::Keyword(Kw::Par) => self.par_for_stmt(None, start),
            TokenKind::Keyword(Kw::Return) => {
                self.bump();
                let value = if self.at(Semicolon) {
                    None
                } else {
                    Some(self.expr())
                };
                let end = self.expect(Semicolon).end;
                Stmt::Return {
                    value,
                    span: Span::new(start, end),
                }
            }
            TokenKind::Keyword(Kw::Fail) => {
                self.bump();
                let value = self.expr();
                let end = self.expect(Semicolon).end;
                Stmt::Fail {
                    value,
                    span: Span::new(start, end),
                }
            }
            TokenKind::Keyword(Kw::Break) => {
                self.bump();
                let label = if matches!(self.kind(), TokenKind::Ident(_)) {
                    Some(self.expect_ident().0)
                } else {
                    None
                };
                let end = self.expect(Semicolon).end;
                Stmt::Break {
                    label,
                    span: Span::new(start, end),
                }
            }
            TokenKind::Keyword(Kw::Continue) => {
                self.bump();
                let label = if matches!(self.kind(), TokenKind::Ident(_)) {
                    Some(self.expect_ident().0)
                } else {
                    None
                };
                let end = self.expect(Semicolon).end;
                Stmt::Continue {
                    label,
                    span: Span::new(start, end),
                }
            }
            TokenKind::Keyword(Kw::Defer) | TokenKind::Keyword(Kw::Errdefer) => {
                let err = self.at_kw(Kw::Errdefer);
                self.bump();
                let body = self.block();
                let end = body.span.end;
                Stmt::Defer {
                    err,
                    body,
                    span: Span::new(start, end),
                }
            }
            TokenKind::Keyword(Kw::Scope) => {
                self.bump();
                let (name, name_span) = self.expect_ident();
                let body = self.block();
                let end = body.span.end;
                Stmt::Scope {
                    name,
                    name_span,
                    body,
                    span: Span::new(start, end),
                }
            }
            TokenKind::Keyword(Kw::Spawn) => {
                self.bump();
                let call = self.call_expr_after_keyword();
                if !self.eat_kw(Kw::In) {
                    self.unexpected();
                }
                let scope = self.expr();
                let end = self.expect(Semicolon).end;
                Stmt::Spawn {
                    call,
                    scope,
                    span: Span::new(start, end),
                }
            }
            TokenKind::Keyword(Kw::Dispatch) => {
                self.bump();
                let call = self.call_expr_after_keyword();
                self.expect_contextual("over");
                let domain = self.domain();
                self.expect_contextual("on");
                let executor = self.expr();
                let end = self.expect(Semicolon).end;
                Stmt::Dispatch {
                    call,
                    domain,
                    executor,
                    span: Span::new(start, end),
                }
            }
            TokenKind::Keyword(Kw::Unsafe) => {
                let (reason, reason_span) = self.unsafe_reason();
                let body = self.block();
                let end = body.span.end;
                Stmt::Unsafe {
                    reason,
                    reason_span,
                    body,
                    span: Span::new(start, end),
                }
            }
            TokenKind::Keyword(Kw::Select) => self.select_stmt(start),
            TokenKind::Keyword(Kw::When) => self.when_stmt(start),
            TokenKind::Punct(LBrace) => Stmt::Block(self.block()),
            TokenKind::Keyword(
                Kw::Fn
                | Kw::Struct
                | Kw::Enum
                | Kw::Interface
                | Kw::Impl
                | Kw::Const
                | Kw::Static
                | Kw::Type
                | Kw::Extern
                | Kw::Test
                | Kw::Bench
                | Kw::Import,
            ) => {
                self.diag(
                    "E0210",
                    "items are not allowed inside a function body",
                    self.span(),
                );
                self.recover_item();
                Stmt::Poison
            }
            _ => self.expr_stmt(start),
        }
    }

    fn let_stmt(&mut self, mutable: bool, start: usize) -> Stmt {
        self.bump();
        let pattern = self.pattern();
        let ty = if self.eat(Colon) {
            Some(self.ty())
        } else {
            None
        };
        self.expect(Eq);
        let init = self.expr();
        let end = self.expect(Semicolon).end;
        Stmt::Let {
            mutable,
            pattern,
            ty,
            init,
            span: Span::new(start, end),
        }
    }

    fn expr_stmt(&mut self, start: usize) -> Stmt {
        let expr = self.expr();
        if let Some(op) = self.assign_op() {
            self.bump();
            let value = self.expr();
            let end = self.expect(Semicolon).end;
            return Stmt::Assign {
                place: expr,
                op,
                value,
                span: Span::new(start, end),
            };
        }
        let trailing_branch_expr = self.in_expr_branch && self.at(RBrace);
        if !is_call_statement(&expr) && !trailing_branch_expr {
            self.diag(
                "E0206",
                "only calls, `spawn` and `dispatch` may be expression statements",
                expr.span,
            );
        }
        let end_span = if self.eat(Semicolon) {
            self.peek().span
        } else {
            if !trailing_branch_expr {
                self.expect_semi(expr.span.end);
            }
            self.peek().span
        };
        let end = expr.span.end.max(end_span.start);
        Stmt::Expr {
            expr,
            span: Span::new(start, end),
        }
    }

    fn assign_op(&self) -> Option<AssignOp> {
        Some(match self.kind() {
            TokenKind::Punct(Eq) => AssignOp::Assign,
            TokenKind::Punct(PlusEq) => AssignOp::Add,
            TokenKind::Punct(MinusEq) => AssignOp::Sub,
            TokenKind::Punct(StarEq) => AssignOp::Mul,
            TokenKind::Punct(SlashEq) => AssignOp::Div,
            TokenKind::Punct(PercentEq) => AssignOp::Rem,
            TokenKind::Punct(PlusWrapEq) => AssignOp::AddWrap,
            TokenKind::Punct(MinusWrapEq) => AssignOp::SubWrap,
            TokenKind::Punct(StarWrapEq) => AssignOp::MulWrap,
            TokenKind::Punct(PlusSatEq) => AssignOp::AddSat,
            TokenKind::Punct(MinusSatEq) => AssignOp::SubSat,
            TokenKind::Punct(StarSatEq) => AssignOp::MulSat,
            TokenKind::Punct(AmpEq) => AssignOp::BitAnd,
            TokenKind::Punct(PipeEq) => AssignOp::BitOr,
            TokenKind::Punct(CaretEq) => AssignOp::BitXor,
            TokenKind::Punct(ShlEq) => AssignOp::Shl,
            TokenKind::Punct(ShrEq) => AssignOp::Shr,
            _ => return None,
        })
    }

    fn if_stmt(&mut self, start: usize) -> Stmt {
        self.bump();
        let cond = self.cond();
        let then = self.block();
        let otherwise = if self.eat_kw(Kw::Else) {
            if self.at_kw(Kw::If) {
                Some(Box::new(self.if_stmt(self.span().start)))
            } else {
                let b = self.block();
                Some(Box::new(Stmt::Block(b)))
            }
        } else {
            None
        };
        let end = otherwise
            .as_ref()
            .map(|s| s.span().end)
            .unwrap_or(then.span.end);
        Stmt::If(IfStmt {
            cond,
            then,
            otherwise,
            span: Span::new(start, end),
        })
    }

    fn cond(&mut self) -> Cond {
        let start = self.span().start;
        if self.eat_kw(Kw::Let) {
            let pattern = self.pattern();
            self.expect(Eq);
            let expr = self.expr();
            let end = expr.span.end;
            Cond {
                expr,
                let_pattern: Some(pattern),
                span: Span::new(start, end),
            }
        } else {
            let expr = self.expr();
            let end = expr.span.end;
            Cond {
                expr,
                let_pattern: None,
                span: Span::new(start, end),
            }
        }
    }

    fn while_stmt(&mut self, start: usize) -> Stmt {
        self.bump();
        let cond = self.cond();
        let body = self.block();
        let end = body.span.end;
        Stmt::While {
            cond,
            body,
            span: Span::new(start, end),
        }
    }

    fn loop_stmt(&mut self, label: Option<String>, start: usize) -> Stmt {
        self.bump();
        let body = self.block();
        let end = body.span.end;
        Stmt::Loop {
            label,
            body,
            span: Span::new(start, end),
        }
    }

    fn par_for_stmt(&mut self, label: Option<String>, start: usize) -> Stmt {
        self.bump(); // par
        let mut grain = None;
        if self.eat(LParen) {
            self.expect_contextual("grain");
            self.expect(Colon);
            grain = Some(self.expr());
            self.expect(RParen);
        }
        if !self.at_kw(Kw::For) {
            self.unexpected();
            return Stmt::Poison;
        }
        self.for_stmt(label, true, grain, start)
    }

    fn for_stmt(
        &mut self,
        label: Option<String>,
        par: bool,
        grain: Option<Expr>,
        start: usize,
    ) -> Stmt {
        self.bump(); // for
        let (first, first_span) = self.expect_ident();
        let mut vars = Vec::new();
        let mut index = None;
        if self.eat(Comma) {
            let (second, second_span) = self.expect_ident();
            index = Some((first, first_span));
            vars.push((second, second_span));
        } else {
            vars.push((first, first_span));
        }
        self.expect_kw_any(vec![Kw::In]);
        let iter = self.expr();
        let mut range_end = None;
        if self.at(DotDot) || self.at(DotDotEq) {
            let inclusive = self.at(DotDotEq);
            self.bump();
            let end = self.expr();
            range_end = Some((inclusive, end));
        }
        let mut step = None;
        if self.at_ident("step") {
            self.bump();
            step = Some(self.expr());
        }
        let body = self.block();
        let end = body.span.end;
        Stmt::For {
            par,
            grain,
            label,
            vars,
            index,
            iter,
            range_end,
            step,
            body,
            span: Span::new(start, end),
        }
    }

    fn match_stmt(&mut self, start: usize) -> Stmt {
        self.bump();
        let scrutinee = self.expr();
        self.expect(LBrace);
        let mut arms = Vec::new();
        while !self.at(RBrace) && !self.at_eof() {
            arms.push(self.arm());
        }
        let close = self.expect(RBrace);
        Stmt::Match(MatchStmt {
            scrutinee,
            arms,
            span: Span::new(start, close.end),
        })
    }

    fn arm(&mut self) -> Arm {
        let start = self.span().start;
        let is_else = self.eat_kw(Kw::Else);
        let mut patterns = Vec::new();
        if !is_else {
            loop {
                patterns.push(self.pattern());
                if !self.eat(Pipe) {
                    break;
                }
            }
        }
        let guard = if !is_else && self.eat_kw(Kw::If) {
            Some(self.expr())
        } else {
            None
        };
        self.expect(FatArrow);
        let body = if self.at(LBrace) {
            ArmBody::Block(self.block())
        } else {
            let expr = self.expr();
            self.eat(Comma);
            ArmBody::Expr(expr)
        };
        let end = match &body {
            ArmBody::Block(b) => b.span.end,
            ArmBody::Expr(e) => e.span.end,
        };
        Arm {
            patterns,
            is_else,
            guard,
            body,
            span: Span::new(start, end),
        }
    }

    fn select_stmt(&mut self, start: usize) -> Stmt {
        self.bump();
        self.expect(LBrace);
        let mut arms = Vec::new();
        while !self.at(RBrace) && !self.at_eof() {
            let astart = self.span().start;
            if self.eat_kw(Kw::Else) {
                self.expect(FatArrow);
                let body = self.block();
                let end = body.span.end;
                arms.push(SelectArm {
                    pattern: Pattern::Wild(Span::empty(astart)),
                    call: Expr::poison(Span::empty(astart)),
                    body,
                    span: Span::new(astart, end),
                });
                continue;
            }
            let pattern = self.pattern();
            self.expect(Eq);
            let call = self.expr();
            self.expect(FatArrow);
            let body = self.block();
            let end = body.span.end;
            arms.push(SelectArm {
                pattern,
                call,
                body,
                span: Span::new(astart, end),
            });
        }
        let close = self.expect(RBrace);
        Stmt::Select {
            arms,
            span: Span::new(start, close.end),
        }
    }

    fn when_stmt(&mut self, start: usize) -> Stmt {
        self.bump();
        let condition = self.expr();
        let then = self.block();
        let otherwise = if self.eat_kw(Kw::Else) {
            if self.at_kw(Kw::When) {
                vec![self.when_stmt(self.span().start)]
            } else {
                vec![Stmt::Block(self.block())]
            }
        } else {
            Vec::new()
        };
        let end = otherwise
            .last()
            .map(|s| s.span().end)
            .unwrap_or(then.span.end);
        Stmt::When {
            condition,
            then,
            otherwise,
            span: Span::new(start, end),
        }
    }

    fn unsafe_reason(&mut self) -> (String, Span) {
        self.bump(); // unsafe
        self.expect(LParen);
        let (reason, span) = if let TokenKind::Str(bytes) = self.kind().clone() {
            let sp = self.bump().span;
            (String::from_utf8_lossy(&bytes).into_owned(), sp)
        } else {
            let sp = self.unexpected();
            (String::new(), sp)
        };
        self.expect(RParen);
        (reason, span)
    }

    fn domain(&mut self) -> Expr {
        let start = self.span().start;
        let mut ranges = Vec::new();
        loop {
            let lo = self.expr();
            let inclusive = false;
            if self.at(DotDot) || self.at(DotDotEq) {
                self.bump();
                let hi = self.expr();
                let span = lo.span.join(hi.span);
                ranges.push(Expr::new(
                    ExprKind::Range {
                        lo: Box::new(lo),
                        hi: Box::new(hi),
                        inclusive,
                        span,
                    },
                    span,
                ));
            } else {
                // The domain grammar requires a range; recover by using the
                // expression as both bounds.
                ranges.push(lo);
            }
            if !self.eat(Comma) {
                break;
            }
        }
        if ranges.len() == 1 {
            ranges.pop().unwrap()
        } else {
            let span = Span::new(start, self.span().end);
            Expr::new(
                ExprKind::Tuple {
                    elems: ranges,
                    span,
                },
                span,
            )
        }
    }

    fn expect_contextual(&mut self, word: &str) -> Span {
        if self.at_ident(word) {
            self.bump().span
        } else {
            let span = self.span();
            self.diag("E0201", format!("expected `{word}`"), span);
            Span::empty(span.start)
        }
    }

    fn expect_kw_any(&mut self, kws: Vec<Kw>) -> Span {
        if kws.iter().any(|k| self.at_kw(*k)) {
            self.bump().span
        } else {
            let names: Vec<&str> = kws.iter().map(|k| k.as_str()).collect();
            let span = self.span();
            self.diag(
                "E0201",
                format!("expected `{}`", names.join("` or `")),
                span,
            );
            Span::empty(span.start)
        }
    }

    fn at_ident(&self, text: &str) -> bool {
        matches!(self.kind(), TokenKind::Ident(s) if s == text)
    }

    fn call_expr_after_keyword(&mut self) -> Expr {
        let start = self.span().start;
        let base = self.postfix();
        if matches!(base.kind, ExprKind::Call { .. }) {
            base
        } else {
            self.diag(
                "E0201",
                "expected a call",
                Span::new(start, self.span().end),
            );
            base
        }
    }

    // ------------------------------------------------------------ patterns

    fn pattern(&mut self) -> Pattern {
        let start = self.span().start;
        match self.kind().clone() {
            TokenKind::Ident(name) if name == "_" => {
                let span = self.bump().span;
                Pattern::Wild(span)
            }
            TokenKind::Ident(_) => {
                let (name, span) = self.expect_ident();
                Pattern::Bind { name, span }
            }
            TokenKind::Int(_) | TokenKind::Float(_) | TokenKind::Str(_) => {
                let expr = self.primary();
                let pattern = Pattern::Literal(expr);
                self.maybe_range_pattern(pattern)
            }
            TokenKind::Keyword(Kw::True) | TokenKind::Keyword(Kw::False) => {
                let expr = self.primary();
                Pattern::Literal(expr)
            }
            TokenKind::Punct(Minus) => {
                let minus = self.bump().span;
                match self.kind().clone() {
                    TokenKind::Int(v) => {
                        let tok = self.bump();
                        let expr = Expr::new(
                            ExprKind::Int {
                                value: v,
                                negative: true,
                                text: format!("-{v}"),
                            },
                            minus.join(tok.span),
                        );
                        self.maybe_range_pattern(Pattern::Literal(expr))
                    }
                    _ => {
                        self.unexpected();
                        Pattern::Poison
                    }
                }
            }
            TokenKind::TypeIdent(_) => {
                let path = self.path_ty();
                if self.eat(Dot) {
                    let (name, name_span) = self.expect_ident();
                    let payload = if self.eat(LParen) {
                        let p = self.pattern();
                        self.expect(RParen);
                        Some(Box::new(p))
                    } else {
                        None
                    };
                    Pattern::Variant {
                        path: Some(path),
                        name,
                        name_span,
                        payload,
                    }
                } else if self.at(DotLBrace) {
                    self.struct_pattern(start)
                } else {
                    Pattern::Struct {
                        ty: Some(path),
                        fields: Vec::new(),
                        span: Span::new(start, self.span().end),
                    }
                }
            }
            TokenKind::Punct(Dot) => {
                self.bump();
                let (name, name_span) = self.expect_ident();
                let payload = if self.eat(LParen) {
                    let p = self.pattern();
                    self.expect(RParen);
                    Some(Box::new(p))
                } else {
                    None
                };
                Pattern::Variant {
                    path: None,
                    name,
                    name_span,
                    payload,
                }
            }
            TokenKind::Punct(DotLBrace) => self.struct_pattern(start),
            TokenKind::Punct(LParen) => {
                self.bump();
                let mut elems = Vec::new();
                while !self.at(RParen) && !self.at_eof() {
                    elems.push(self.pattern());
                    if !self.eat(Comma) {
                        break;
                    }
                }
                let close = self.expect(RParen);
                Pattern::Tuple {
                    elems,
                    span: Span::new(start, close.end),
                }
            }
            _ => {
                self.unexpected();
                Pattern::Poison
            }
        }
    }

    fn maybe_range_pattern(&mut self, pattern: Pattern) -> Pattern {
        if self.at(DotDot) || self.at(DotDotEq) {
            let inclusive = self.at(DotDotEq);
            let op = self.bump().span;
            let hi = self.pattern();
            let span = pattern.span().join(hi.span());
            return Pattern::Range {
                lo: Box::new(pattern),
                hi: Box::new(hi),
                inclusive,
                span: span.join(op),
            };
        }
        pattern
    }

    fn struct_pattern(&mut self, start: usize) -> Pattern {
        self.bump(); // .{
        let mut fields = Vec::new();
        while !self.at(RBrace) && !self.at_eof() {
            let (name, name_span) = self.expect_ident();
            let pattern = if self.eat(Colon) {
                Some(self.pattern())
            } else {
                None
            };
            fields.push(FieldPattern {
                name,
                name_span,
                pattern,
            });
            if !self.eat(Comma) {
                break;
            }
        }
        let close = self.expect(RBrace);
        Pattern::Struct {
            ty: None,
            fields,
            span: Span::new(start, close.end),
        }
    }

    // ----------------------------------------------------------- expressions

    fn expr(&mut self) -> Expr {
        let mut expr = self.or_expr();
        loop {
            if self.at_kw(Kw::Orelse) {
                let op = self.bump().span;
                let handler = self.handler();
                let span = expr.span.join(handler_span(&handler));
                expr = Expr::new(
                    ExprKind::Orelse {
                        value: Box::new(expr),
                        handler,
                        orelse_span: op,
                    },
                    span,
                );
            } else if self.at_kw(Kw::Catch) {
                let op = self.bump().span;
                let binding = match self.kind().clone() {
                    TokenKind::Ident(name) if name == "_" => {
                        self.bump();
                        None
                    }
                    TokenKind::Ident(_) => Some(self.expect_ident().0),
                    _ => {
                        let sp = self.unexpected();
                        let _ = sp;
                        None
                    }
                };
                self.expect(FatArrow);
                let handler = self.handler();
                let span = expr.span.join(handler_span(&handler));
                expr = Expr::new(
                    ExprKind::Catch {
                        value: Box::new(expr),
                        binding,
                        handler,
                        catch_span: op,
                    },
                    span,
                );
            } else {
                break;
            }
        }
        expr
    }

    fn handler(&mut self) -> Handler {
        if self.at(LBrace) {
            Handler::Block(self.block())
        } else {
            Handler::Expr(Box::new(self.or_expr()))
        }
    }

    fn or_expr(&mut self) -> Expr {
        let mut lhs = self.and_expr();
        while self.at(PipePipe) {
            let op = self.bump().span;
            let rhs = self.and_expr();
            let span = lhs.span.join(rhs.span);
            lhs = Expr::new(
                ExprKind::Binary {
                    op: BinOp::Or,
                    lhs: Box::new(lhs),
                    rhs: Box::new(rhs),
                    op_span: op,
                },
                span,
            );
        }
        lhs
    }

    fn and_expr(&mut self) -> Expr {
        let mut lhs = self.cmp_expr();
        while self.at(AmpAmp) {
            let op = self.bump().span;
            let rhs = self.cmp_expr();
            let span = lhs.span.join(rhs.span);
            lhs = Expr::new(
                ExprKind::Binary {
                    op: BinOp::And,
                    lhs: Box::new(lhs),
                    rhs: Box::new(rhs),
                    op_span: op,
                },
                span,
            );
        }
        lhs
    }

    fn cmp_expr(&mut self) -> Expr {
        let lhs = self.bit_expr();
        let Some(op) = self.cmp_op() else {
            return lhs;
        };
        let op_span = self.bump().span;
        let rhs = self.bit_expr();
        let span = lhs.span.join(rhs.span);
        let expr = Expr::new(
            ExprKind::Binary {
                op,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
                op_span,
            },
            span,
        );
        if self.cmp_op().is_some() {
            let second = self.span();
            self.diag(
                "E0205",
                "comparisons do not chain; write `a < b && b < c`",
                second,
            );
            // Consume the rest of the chain so one error covers it.
            while self.cmp_op().is_some() {
                self.bump();
                let _ = self.bit_expr();
            }
        }
        expr
    }

    fn cmp_op(&self) -> Option<BinOp> {
        Some(match self.kind() {
            TokenKind::Punct(EqEq) => BinOp::Eq,
            TokenKind::Punct(BangEq) => BinOp::Ne,
            TokenKind::Punct(Lt) => BinOp::Lt,
            TokenKind::Punct(LtEq) => BinOp::Le,
            TokenKind::Punct(Gt) => BinOp::Gt,
            TokenKind::Punct(GtEq) => BinOp::Ge,
            _ => return None,
        })
    }

    fn bit_expr(&mut self) -> Expr {
        let mut lhs = self.shift_expr();
        loop {
            let op = match self.kind() {
                TokenKind::Punct(Amp) => BinOp::BitAnd,
                TokenKind::Punct(Pipe) => BinOp::BitOr,
                TokenKind::Punct(Caret) => BinOp::BitXor,
                _ => break,
            };
            let op_span = self.bump().span;
            let rhs = self.shift_expr();
            let span = lhs.span.join(rhs.span);
            lhs = Expr::new(
                ExprKind::Binary {
                    op,
                    lhs: Box::new(lhs),
                    rhs: Box::new(rhs),
                    op_span,
                },
                span,
            );
        }
        lhs
    }

    fn shift_expr(&mut self) -> Expr {
        let mut lhs = self.add_expr();
        loop {
            let op = match self.kind() {
                TokenKind::Punct(Shl) => BinOp::Shl,
                TokenKind::Punct(Shr) => BinOp::Shr,
                _ => break,
            };
            let op_span = self.bump().span;
            let rhs = self.add_expr();
            let span = lhs.span.join(rhs.span);
            lhs = Expr::new(
                ExprKind::Binary {
                    op,
                    lhs: Box::new(lhs),
                    rhs: Box::new(rhs),
                    op_span,
                },
                span,
            );
        }
        lhs
    }

    fn add_expr(&mut self) -> Expr {
        let mut lhs = self.mul_expr();
        loop {
            let op = match self.kind() {
                TokenKind::Punct(Plus) => BinOp::Add,
                TokenKind::Punct(Minus) => BinOp::Sub,
                TokenKind::Punct(PlusWrap) => BinOp::AddWrap,
                TokenKind::Punct(MinusWrap) => BinOp::SubWrap,
                TokenKind::Punct(PlusSat) => BinOp::AddSat,
                TokenKind::Punct(MinusSat) => BinOp::SubSat,
                _ => break,
            };
            let op_span = self.bump().span;
            let rhs = self.mul_expr();
            let span = lhs.span.join(rhs.span);
            lhs = Expr::new(
                ExprKind::Binary {
                    op,
                    lhs: Box::new(lhs),
                    rhs: Box::new(rhs),
                    op_span,
                },
                span,
            );
        }
        lhs
    }

    fn mul_expr(&mut self) -> Expr {
        let mut lhs = self.cast_expr();
        loop {
            let op = match self.kind() {
                TokenKind::Punct(Star) => BinOp::Mul,
                TokenKind::Punct(Slash) => BinOp::Div,
                TokenKind::Punct(Percent) => BinOp::Rem,
                TokenKind::Punct(StarWrap) => BinOp::MulWrap,
                TokenKind::Punct(StarSat) => BinOp::MulSat,
                _ => break,
            };
            let op_span = self.bump().span;
            let rhs = self.cast_expr();
            let span = lhs.span.join(rhs.span);
            lhs = Expr::new(
                ExprKind::Binary {
                    op,
                    lhs: Box::new(lhs),
                    rhs: Box::new(rhs),
                    op_span,
                },
                span,
            );
        }
        lhs
    }

    fn cast_expr(&mut self) -> Expr {
        let mut expr = self.prefix();
        while self.at_kw(Kw::As) {
            let as_span = self.bump().span;
            let ty = self.ty();
            let span = expr.span.join(ty.span());
            expr = Expr::new(
                ExprKind::Cast {
                    expr: Box::new(expr),
                    ty,
                    as_span,
                },
                span,
            );
        }
        expr
    }

    fn prefix(&mut self) -> Expr {
        let start = self.span().start;
        let op = match self.kind() {
            TokenKind::Punct(Minus) => Some(UnOp::Neg),
            TokenKind::Punct(MinusWrap) => Some(UnOp::NegWrap),
            TokenKind::Punct(Bang) => Some(UnOp::Not),
            TokenKind::Punct(Tilde) => Some(UnOp::BitNot),
            TokenKind::Punct(Amp) => {
                self.bump();
                let mutable = self.eat_kw(Kw::Mut);
                let expr = self.prefix();
                let span = Span::new(start, expr.span.end);
                return Expr::new(
                    ExprKind::Unary {
                        op: if mutable {
                            UnOp::BorrowMut
                        } else {
                            UnOp::Borrow
                        },
                        expr: Box::new(expr),
                        op_span: Span::new(start, start + 1),
                    },
                    span,
                );
            }
            TokenKind::Keyword(Kw::Try) => {
                self.bump();
                let expr = self.prefix();
                let span = Span::new(start, expr.span.end);
                return Expr::new(
                    ExprKind::Unary {
                        op: UnOp::Try,
                        expr: Box::new(expr),
                        op_span: Span::new(start, start + 3),
                    },
                    span,
                );
            }
            _ => None,
        };
        if let Some(op) = op {
            let op_span = self.bump().span;
            if op == UnOp::Neg
                && let TokenKind::Int(value) = self.kind().clone()
            {
                let tok = self.bump();
                return Expr::new(
                    ExprKind::Int {
                        value,
                        negative: true,
                        text: format!("-{value}"),
                    },
                    Span::new(start, tok.span.end),
                );
            }
            let expr = self.prefix();
            let span = Span::new(start, expr.span.end);
            return Expr::new(
                ExprKind::Unary {
                    op,
                    expr: Box::new(expr),
                    op_span,
                },
                span,
            );
        }
        self.postfix()
    }

    fn postfix(&mut self) -> Expr {
        let mut expr = self.primary();
        loop {
            match self.kind().clone() {
                TokenKind::Punct(LParen) => {
                    let args_span = self.paren_span();
                    let args = self.call_args();
                    let span = expr.span.join(args_span);
                    expr = Expr::new(
                        ExprKind::Call {
                            callee: Box::new(expr),
                            args,
                            args_span,
                        },
                        span,
                    );
                }
                TokenKind::Punct(LBracket) => {
                    let bracket = self.bump().span;
                    let lo = self.expr();
                    if self.eat(DotDot) {
                        let hi = self.expr();
                        let close = self.expect(RBracket);
                        let span = expr.span.join(close);
                        expr = Expr::new(
                            ExprKind::Slice {
                                base: Box::new(expr),
                                lo: Box::new(lo),
                                hi: Box::new(hi),
                                bracket: bracket.join(close),
                            },
                            span,
                        );
                    } else {
                        let close = self.expect(RBracket);
                        let span = expr.span.join(close);
                        expr = Expr::new(
                            ExprKind::Index {
                                base: Box::new(expr),
                                index: Box::new(lo),
                                bracket: bracket.join(close),
                            },
                            span,
                        );
                    }
                }
                TokenKind::Punct(Dot) => {
                    self.bump();
                    match self.kind().clone() {
                        TokenKind::Ident(name) => {
                            let sp = self.bump().span;
                            let span = expr.span.join(sp);
                            expr = Expr::new(
                                ExprKind::Field {
                                    base: Box::new(expr),
                                    name,
                                    name_span: sp,
                                },
                                span,
                            );
                        }
                        TokenKind::Keyword(Kw::Copy) => {
                            let sp = self.bump().span;
                            let span = expr.span.join(sp);
                            expr = Expr::new(
                                ExprKind::Field {
                                    base: Box::new(expr),
                                    name: "copy".to_string(),
                                    name_span: sp,
                                },
                                span,
                            );
                        }
                        TokenKind::Int(n) => {
                            let sp = self.bump().span;
                            let span = expr.span.join(sp);
                            expr = Expr::new(
                                ExprKind::TupleField {
                                    base: Box::new(expr),
                                    index: n as u64,
                                    span,
                                },
                                span,
                            );
                        }
                        _ => {
                            self.unexpected();
                        }
                    }
                }
                TokenKind::Punct(DotStar) => {
                    let op = self.bump().span;
                    let span = expr.span.join(op);
                    expr = Expr::new(
                        ExprKind::Deref {
                            base: Box::new(expr),
                            op_span: op,
                        },
                        span,
                    );
                }
                TokenKind::Punct(DotLBrace) => {
                    let braces_start = self.span();
                    self.bump();
                    let fields = self.inits();
                    let close = self.expect(RBrace);
                    let braces = braces_start.join(close);
                    let span = expr.span.join(close);
                    if let ExprKind::Path(path) = &expr.kind {
                        expr = Expr::new(
                            ExprKind::StructLit {
                                ty: path.clone(),
                                fields,
                                braces,
                            },
                            span,
                        );
                    } else {
                        expr = Expr::new(ExprKind::StructShorthand { fields, braces }, span);
                    }
                }
                TokenKind::Punct(LBrace) => {
                    if !matches!(expr.kind, ExprKind::Path(_)) {
                        break;
                    }
                    let brace = self.bump().span;
                    let file = self.source.path.clone();
                    self.diag("E0209", "a struct literal is written with `.{`", brace)
                        .fix(
                            "insert `.` before `{`",
                            Edit {
                                file,
                                span: Span::new(expr.span.end, brace.start),
                                text: ".".to_string(),
                            },
                        );
                    let fields = self.inits();
                    let close = self.expect(RBrace);
                    let span = expr.span.join(close);
                    if let ExprKind::Path(path) = &expr.kind {
                        expr = Expr::new(
                            ExprKind::StructLit {
                                ty: path.clone(),
                                fields,
                                braces: brace.join(close),
                            },
                            span,
                        );
                    } else {
                        expr = Expr::poison(span);
                    }
                }
                TokenKind::Punct(DotLBracket) => {
                    self.bump();
                    let mut args = Vec::new();
                    while !self.at(RBracket) && !self.at_eof() {
                        args.push(self.ty());
                        if !self.eat(Comma) {
                            break;
                        }
                    }
                    let close = self.expect(RBracket);
                    let span = expr.span.join(close);
                    expr = Expr::new(
                        ExprKind::TypeApply {
                            base: Box::new(expr),
                            args,
                            span,
                        },
                        span,
                    );
                }
                _ => break,
            }
        }
        expr
    }

    fn paren_span(&self) -> Span {
        let open = self.span();
        // Find the matching close without changing the cursor.
        let mut depth = 0i32;
        let mut i = self.pos;
        while i < self.tokens.len() {
            match &self.tokens[i].kind {
                TokenKind::Punct(LParen) => depth += 1,
                TokenKind::Punct(RParen) => {
                    depth -= 1;
                    if depth == 0 {
                        return open.join(self.tokens[i].span);
                    }
                }
                _ => {}
            }
            i += 1;
        }
        open
    }

    fn call_args(&mut self) -> Vec<Expr> {
        self.expect(LParen);
        let mut args = Vec::new();
        while !self.at(RParen) && !self.at_eof() {
            args.push(self.expr());
            if !self.eat(Comma) {
                break;
            }
        }
        self.expect(RParen);
        args
    }

    fn inits(&mut self) -> Vec<FieldInit> {
        let mut fields = Vec::new();
        while !self.at(RBrace) && !self.at_eof() {
            let (name, name_span) = self.expect_ident();
            self.expect(Colon);
            let value = self.expr();
            fields.push(FieldInit {
                name,
                name_span,
                value,
            });
            if !self.eat(Comma) {
                break;
            }
        }
        fields
    }

    fn primary(&mut self) -> Expr {
        let start = self.span().start;
        match self.kind().clone() {
            TokenKind::Int(value) => {
                let span = self.bump().span;
                Expr::new(
                    ExprKind::Int {
                        value,
                        negative: false,
                        text: self.source.text_of(span).to_string(),
                    },
                    span,
                )
            }
            TokenKind::Float(value) => {
                let span = self.bump().span;
                Expr::new(ExprKind::Float(value), span)
            }
            TokenKind::Str(bytes) => {
                let span = self.bump().span;
                Expr::new(ExprKind::Str(bytes), span)
            }
            TokenKind::CStr(bytes) => {
                let span = self.bump().span;
                Expr::new(ExprKind::CStr(bytes), span)
            }
            TokenKind::Keyword(Kw::True) => {
                let span = self.bump().span;
                Expr::new(ExprKind::Bool(true), span)
            }
            TokenKind::Keyword(Kw::False) => {
                let span = self.bump().span;
                Expr::new(ExprKind::Bool(false), span)
            }
            TokenKind::Keyword(Kw::Copy) => {
                let span = self.bump().span;
                Expr::new(ExprKind::Ident("copy".to_string()), span)
            }
            TokenKind::Ident(name) => {
                if matches!(self.peek_at(1).kind, TokenKind::Punct(Dot))
                    && matches!(self.peek_at(2).kind, TokenKind::TypeIdent(_))
                {
                    let path = self.path_ty();
                    let span = path.span;
                    Expr::new(ExprKind::Path(path), span)
                } else {
                    let span = self.bump().span;
                    Expr::new(ExprKind::Ident(name), span)
                }
            }
            TokenKind::TypeIdent(_) | TokenKind::Scalar(_) | TokenKind::Vector(_) => {
                let path = self.path_ty();
                let span = path.span;
                Expr::new(ExprKind::Path(path), span)
            }
            TokenKind::Builtin(name) => {
                let name_span = self.bump().span;
                let mut type_arg = None;
                let mut args = Vec::new();
                if self.eat(LBracket) {
                    let ty = self.ty();
                    type_arg = Some(Box::new(ty));
                    self.expect(RBracket);
                }
                if self.at(LParen) {
                    self.bump();
                    if let Some(first_ty) = self.builtin_first_type(&name) {
                        type_arg = Some(Box::new(first_ty));
                        if self.eat(Comma) {
                            while !self.at(RParen) && !self.at_eof() {
                                args.push(self.expr());
                                if !self.eat(Comma) {
                                    break;
                                }
                            }
                        }
                    } else {
                        while !self.at(RParen) && !self.at_eof() {
                            args.push(self.expr());
                            if !self.eat(Comma) {
                                break;
                            }
                        }
                    }
                    let close = self.expect(RParen);
                    let span = name_span.join(close);
                    return Expr::new(
                        ExprKind::Builtin {
                            name,
                            type_arg,
                            args,
                        },
                        span,
                    );
                }
                Expr::new(
                    ExprKind::Builtin {
                        name,
                        type_arg,
                        args,
                    },
                    name_span,
                )
            }
            TokenKind::Punct(LParen) => {
                self.bump();
                let first = self.expr();
                if self.eat(Comma) {
                    let mut elems = vec![first];
                    while !self.at(RParen) && !self.at_eof() {
                        elems.push(self.expr());
                        if !self.eat(Comma) {
                            break;
                        }
                    }
                    let close = self.expect(RParen);
                    let span = Span::new(start, close.end);
                    Expr::new(ExprKind::Tuple { elems, span }, span)
                } else {
                    let close = self.expect(RParen);
                    Expr::new(
                        ExprKind::Paren(Box::new(first)),
                        Span::new(start, close.end),
                    )
                }
            }
            TokenKind::Punct(LBracket) => {
                self.bump();
                let len = self.expr();
                self.expect(RBracket);
                let elem = self.ty();
                self.expect(DotLBrace);
                let mut elems = Vec::new();
                while !self.at(RBrace) && !self.at_eof() {
                    elems.push(self.expr());
                    if !self.eat(Comma) {
                        break;
                    }
                }
                let close = self.expect(RBrace);
                let span = Span::new(start, close.end);
                Expr::new(
                    ExprKind::ArrayLit {
                        len: Box::new(len),
                        elem,
                        elems,
                        span,
                    },
                    span,
                )
            }
            TokenKind::Punct(Dot) => {
                self.bump();
                if let TokenKind::Ident(name) = self.kind().clone() {
                    let variant_span = self.bump().span;
                    let payload = if self.eat(LParen) {
                        let e = self.expr();
                        self.expect(RParen);
                        Some(Box::new(e))
                    } else {
                        None
                    };
                    let span = Span::new(start, self.span().end);
                    Expr::new(
                        ExprKind::EnumLit {
                            path: None,
                            variant: name,
                            variant_span,
                            payload,
                        },
                        span,
                    )
                } else {
                    self.unexpected();
                    Expr::poison(Span::new(start, self.span().end))
                }
            }
            TokenKind::Punct(DotLBrace) => {
                let braces_start = self.span();
                self.bump();
                let fields = self.inits();
                let close = self.expect(RBrace);
                let span = Span::new(start, close.end);
                Expr::new(
                    ExprKind::StructShorthand {
                        fields,
                        braces: braces_start.join(close),
                    },
                    span,
                )
            }
            TokenKind::Keyword(Kw::Fn) => self.closure(start),
            TokenKind::Keyword(Kw::If) => self.if_expr(start),
            TokenKind::Keyword(Kw::Match) => self.match_expr(start),
            TokenKind::Keyword(Kw::Spawn) => {
                self.bump();
                let call = self.call_expr_after_keyword();
                self.expect_kw_any(vec![Kw::In]);
                let scope = self.expr();
                let span = Span::new(start, scope.span.end);
                Expr::new(
                    ExprKind::Spawn {
                        call: Box::new(call),
                        scope: Box::new(scope),
                        span,
                    },
                    span,
                )
            }
            TokenKind::Keyword(Kw::Dispatch) => {
                self.bump();
                let call = self.call_expr_after_keyword();
                self.expect_contextual("over");
                let domain = self.domain();
                self.expect_contextual("on");
                let executor = self.expr();
                let span = Span::new(start, executor.span.end);
                Expr::new(
                    ExprKind::Dispatch {
                        call: Box::new(call),
                        domain: Box::new(domain),
                        executor: Box::new(executor),
                        span,
                    },
                    span,
                )
            }
            TokenKind::Keyword(Kw::Unsafe) => {
                let (reason, reason_span) = self.unsafe_reason();
                self.expect(LBrace);
                let body = self.expr();
                let close = self.expect(RBrace);
                let span = Span::new(start, close.end);
                Expr::new(
                    ExprKind::Unsafe {
                        reason,
                        reason_span,
                        body: Box::new(body),
                        span,
                    },
                    span,
                )
            }
            TokenKind::Keyword(Kw::Let) => {
                let span = self.span();
                self.diag("E0201", "`let` starts a statement, not an expression", span);
                self.bump();
                Expr::poison(span)
            }
            _ => {
                let span = self.unexpected();
                Expr::poison(span)
            }
        }
    }

    fn builtin_first_type(&mut self, name: &str) -> Option<Ty> {
        let type_first = matches!(
            name,
            "bits"
                | "sizeof"
                | "alignof"
                | "offsetof"
                | "lanes"
                | "load"
                | "store"
                | "load_tail"
                | "store_tail"
                | "gather"
                | "scatter"
                | "splat"
                | "narrow"
                | "wrap"
                | "sat"
                | "nearest"
                | "enum"
                | "uninit"
                | "zero"
                | "shuffle"
                | "mask_bits"
                | "iota"
        );
        if !type_first {
            return None;
        }
        if matches!(
            self.kind(),
            TokenKind::Scalar(_)
                | TokenKind::Vector(_)
                | TokenKind::TypeIdent(_)
                | TokenKind::Punct(LBracket | Amp | Star | Question | LParen)
        ) {
            Some(self.ty())
        } else {
            None
        }
    }

    fn closure(&mut self, start: usize) -> Expr {
        self.bump(); // fn
        let mut captures = Vec::new();
        if !self.at(LBracket) {
            let file = self.source.path.clone();
            self.diag(
                "E0208",
                "a closure must list its captures in `[...]`",
                Span::new(start, start + 2),
            )
            .fix(
                "insert an empty capture list",
                Edit {
                    file,
                    span: Span::empty(start + 2),
                    text: "[]".to_string(),
                },
            );
        } else {
            self.bump();
            while !self.at(RBracket) && !self.at_eof() {
                let cstart = self.span().start;
                let shared = self.eat(Amp);
                let mutable = self.eat_kw(Kw::Mut);
                let (name, span) = self.expect_ident();
                captures.push(Capture {
                    name,
                    span,
                    shared,
                    mutable,
                });
                if !self.eat(Comma) {
                    break;
                }
                let _ = cstart;
            }
            self.expect(RBracket);
        }
        let (params, _) = self.params(false);
        let (ret, error, _, _) = self.result();
        let body = self.block();
        let span = Span::new(start, body.span.end);
        Expr::new(
            ExprKind::Closure {
                captures,
                params,
                ret,
                error,
                body,
                span,
            },
            span,
        )
    }

    fn if_expr(&mut self, start: usize) -> Expr {
        self.bump();
        let cond = self.cond();
        let then = self.expr_branch();
        self.expect_kw_any(vec![Kw::Else]);
        let otherwise = if self.at_kw(Kw::If) {
            self.if_expr(self.span().start)
        } else {
            self.expr_branch()
        };
        let span = Span::new(start, otherwise.span.end);
        Expr::new(
            ExprKind::If {
                cond: Box::new(cond),
                then: Box::new(then),
                otherwise: Box::new(otherwise),
                span,
            },
            span,
        )
    }

    fn expr_branch(&mut self) -> Expr {
        let open = self.expect(LBrace);
        if self.at(RBrace) {
            let close = self.bump().span;
            self.diag(
                "E0207",
                "an expression branch must hold exactly one expression",
                open.join(close),
            );
            return Expr::poison(open.join(close));
        }
        let can_expr = !matches!(
            self.kind(),
            TokenKind::Keyword(
                Kw::Let
                    | Kw::Var
                    | Kw::Return
                    | Kw::Fail
                    | Kw::Break
                    | Kw::Continue
                    | Kw::Defer
                    | Kw::Errdefer
                    | Kw::Scope
                    | Kw::For
                    | Kw::While
                    | Kw::Loop
                    | Kw::Select
                    | Kw::When
                    | Kw::Fn
            ) | TokenKind::Punct(LBrace)
        );
        if can_expr {
            let saved = self.pos;
            let expr = self.expr();
            if self.at(RBrace) {
                let close = self.bump().span;
                let _ = close;
                return expr;
            }
            self.pos = saved;
        }
        let old_flag = self.in_expr_branch;
        self.in_expr_branch = true;
        let block = self.block_after_open(open);
        self.in_expr_branch = old_flag;
        self.diag(
            "E0207",
            "a branch of an expression-form `if` must hold exactly one expression",
            block.span,
        );
        Expr::poison(block.span)
    }

    fn block_after_open(&mut self, open: Span) -> Block {
        let mut stmts = Vec::new();
        while !self.at(RBrace) && !self.at_eof() {
            let before = self.pos;
            stmts.push(self.stmt());
            if self.pos == before {
                self.bump();
            }
        }
        let close = self.expect(RBrace);
        Block {
            stmts,
            span: open.join(close),
        }
    }

    fn match_expr(&mut self, start: usize) -> Expr {
        self.bump();
        let scrutinee = self.expr();
        self.expect(LBrace);
        let mut arms = Vec::new();
        while !self.at(RBrace) && !self.at_eof() {
            let arm_start = self.span().start;
            let is_else = self.eat_kw(Kw::Else);
            let mut patterns = Vec::new();
            if !is_else {
                loop {
                    patterns.push(self.pattern());
                    if !self.eat(Pipe) {
                        break;
                    }
                }
            }
            let guard = if self.eat_kw(Kw::If) {
                Some(self.expr())
            } else {
                None
            };
            self.expect(FatArrow);
            let body = if self.at(LBrace) {
                let block = self.block();
                self.diag(
                    "E0207",
                    "an arm of an expression-form `match` must hold exactly one expression",
                    block.span,
                );
                ArmBody::Expr(Expr::poison(block.span))
            } else {
                let e = self.expr();
                self.eat(Comma);
                ArmBody::Expr(e)
            };
            let end = match &body {
                ArmBody::Expr(e) => e.span.end,
                ArmBody::Block(b) => b.span.end,
            };
            arms.push(Arm {
                patterns,
                is_else,
                guard,
                body,
                span: Span::new(arm_start, end),
            });
        }
        let close = self.expect(RBrace);
        let span = Span::new(start, close.end);
        Expr::new(
            ExprKind::Match {
                scrutinee: Box::new(scrutinee),
                arms,
                span,
            },
            span,
        )
    }
}

fn handler_span(h: &Handler) -> Span {
    match h {
        Handler::Expr(e) => e.span,
        Handler::Block(b) => b.span,
    }
}

fn is_call_statement(expr: &Expr) -> bool {
    match &expr.kind {
        ExprKind::Call { .. } => true,
        ExprKind::Unary {
            op: UnOp::Try,
            expr,
            ..
        } => is_call_statement(expr),
        ExprKind::Catch { value, .. } => is_call_statement(value),
        ExprKind::Builtin { .. } | ExprKind::Spawn { .. } | ExprKind::Dispatch { .. } => true,
        _ => false,
    }
}

/// Check the parenthesization rule from §5.1 on a parsed expression.
///
/// `Paren` nodes are barriers: everything inside is a separate group.
pub fn check_operator_mixing(module: &Module, source: &Source) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    check_items(&module.items, source, &mut out);
    out
}

fn check_items(items: &[Item], source: &Source, out: &mut Vec<Diagnostic>) {
    for item in items {
        match item {
            Item::Fn(f) => {
                if let Some(body) = &f.body {
                    check_block(body, source, out);
                }
            }
            Item::Struct(s) => {
                for f in &s.fields {
                    check_ty(&f.ty, source, out);
                }
            }
            Item::Enum(e) => {
                for v in &e.variants {
                    if let Some(t) = &v.payload {
                        check_ty(t, source, out);
                    }
                }
            }
            Item::Impl(i) => {
                check_ty(&i.target, source, out);
                check_items(&i.members, source, out);
            }
            Item::Interface(i) => check_items(&i.members, source, out),
            Item::Const(c) => check_expr(&c.value, source, out),
            Item::Static(s) => check_expr(&s.value, source, out),
            Item::TypeAlias(t) => check_ty(&t.ty, source, out),
            Item::Extern(e) => {
                for f in &e.functions {
                    check_fn(f, source, out);
                }
            }
            Item::Test(t) => check_block(&t.body, source, out),
            Item::When(w) => {
                check_items(&w.then, source, out);
                check_items(&w.otherwise, source, out);
            }
            Item::Poison => {}
        }
    }
}

fn check_fn(f: &FnDecl, source: &Source, out: &mut Vec<Diagnostic>) {
    if let Some(b) = &f.body {
        check_block(b, source, out);
    }
}

fn check_block(block: &Block, source: &Source, out: &mut Vec<Diagnostic>) {
    for stmt in &block.stmts {
        match stmt {
            Stmt::Let { init, ty, .. } => {
                if let Some(t) = ty {
                    check_ty(t, source, out);
                }
                check_expr(init, source, out);
            }
            Stmt::Assign { place, value, .. } => {
                check_expr(place, source, out);
                check_expr(value, source, out);
            }
            Stmt::Expr { expr, .. } => check_expr(expr, source, out),
            Stmt::Return { value, .. } => {
                if let Some(e) = value {
                    check_expr(e, source, out);
                }
            }
            Stmt::Fail { value, .. } => check_expr(value, source, out),
            Stmt::Defer { body, .. } => check_block(body, source, out),
            Stmt::Block(b) => check_block(b, source, out),
            Stmt::If(x) => {
                check_expr(&x.cond.expr, source, out);
                check_block(&x.then, source, out);
                if let Some(e) = &x.otherwise {
                    check_stmt(e, source, out);
                }
            }
            Stmt::While { cond, body, .. } => {
                check_expr(&cond.expr, source, out);
                check_block(body, source, out);
            }
            Stmt::Loop { body, .. } => check_block(body, source, out),
            Stmt::For {
                iter,
                range_end,
                step,
                body,
                ..
            } => {
                check_expr(iter, source, out);
                if let Some((_, e)) = range_end {
                    check_expr(e, source, out);
                }
                if let Some(e) = step {
                    check_expr(e, source, out);
                }
                check_block(body, source, out);
            }
            Stmt::Match(m) => {
                check_expr(&m.scrutinee, source, out);
                for arm in &m.arms {
                    for p in &arm.patterns {
                        check_pattern(p, source, out);
                    }
                    if let Some(g) = &arm.guard {
                        check_expr(g, source, out);
                    }
                    match &arm.body {
                        ArmBody::Block(b) => check_block(b, source, out),
                        ArmBody::Expr(e) => check_expr(e, source, out),
                    }
                }
            }
            Stmt::Scope { body, .. } => check_block(body, source, out),
            Stmt::Spawn { call, scope, .. } => {
                check_expr(call, source, out);
                check_expr(scope, source, out);
            }
            Stmt::Dispatch {
                call,
                domain,
                executor,
                ..
            } => {
                check_expr(call, source, out);
                check_expr(domain, source, out);
                check_expr(executor, source, out);
            }
            Stmt::Select { arms, .. } => {
                for arm in arms {
                    check_pattern(&arm.pattern, source, out);
                    check_expr(&arm.call, source, out);
                    check_block(&arm.body, source, out);
                }
            }
            Stmt::Unsafe { body, .. } => check_block(body, source, out),
            Stmt::When {
                condition,
                then,
                otherwise,
                ..
            } => {
                check_expr(condition, source, out);
                check_block(then, source, out);
                for s in otherwise {
                    check_stmt(s, source, out);
                }
            }
            Stmt::Break { .. } | Stmt::Continue { .. } | Stmt::Poison => {}
        }
    }
}

fn check_stmt(stmt: &Stmt, source: &Source, out: &mut Vec<Diagnostic>) {
    let block = Block {
        stmts: vec![stmt.clone()],
        span: stmt.span(),
    };
    check_block(&block, source, out);
}

fn check_ty(ty: &Ty, source: &Source, out: &mut Vec<Diagnostic>) {
    match ty {
        Ty::Array { len, elem, .. } => {
            check_expr(len, source, out);
            check_ty(elem, source, out);
        }
        Ty::Borrow { inner, .. }
        | Ty::Slice { elem: inner, .. }
        | Ty::Ptr { inner, .. }
        | Ty::Optional { inner, .. } => check_ty(inner, source, out),
        Ty::Tuple { elems, .. } => {
            for t in elems {
                check_ty(t, source, out);
            }
        }
        Ty::Fn {
            params, ret, error, ..
        } => {
            for t in params {
                check_ty(t, source, out);
            }
            check_ty(ret, source, out);
            if let Some(e) = error {
                check_ty(e, source, out);
            }
        }
        Ty::Path(p) => {
            for a in &p.args {
                if let GenericArg::Ty(t) = a {
                    check_ty(t, source, out);
                } else if let GenericArg::Const(e) = a {
                    check_expr(e, source, out);
                }
            }
        }
        _ => {}
    }
}

fn check_pattern(p: &Pattern, source: &Source, out: &mut Vec<Diagnostic>) {
    match p {
        Pattern::Literal(e) => check_expr(e, source, out),
        Pattern::Range { lo, hi, .. } => {
            check_pattern(lo, source, out);
            check_pattern(hi, source, out);
        }
        Pattern::Variant {
            payload: Some(p), ..
        } => check_pattern(p, source, out),
        Pattern::Variant { payload: None, .. } => {}
        Pattern::Tuple { elems, .. } => {
            for e in elems {
                check_pattern(e, source, out);
            }
        }
        Pattern::Struct { fields, .. } => {
            for f in fields {
                if let Some(p) = &f.pattern {
                    check_pattern(p, source, out);
                }
            }
        }
        _ => {}
    }
}

fn check_expr(expr: &Expr, source: &Source, out: &mut Vec<Diagnostic>) {
    match &expr.kind {
        ExprKind::Unary { expr, .. } => check_expr(expr, source, out),
        ExprKind::Binary {
            lhs,
            rhs,
            op,
            op_span,
            ..
        } => {
            check_expr(lhs, source, out);
            check_expr(rhs, source, out);
            if op.is_bitwise() || op.is_shift() {
                check_mixing(lhs, *op, *op_span, source, out);
                check_mixing(rhs, *op, *op_span, source, out);
            } else if op.is_arithmetic() || op.is_comparison() {
                check_bitwise_child(lhs, source, out);
                check_bitwise_child(rhs, source, out);
            }
        }
        ExprKind::Call { callee, args, .. } => {
            check_expr(callee, source, out);
            for a in args {
                check_expr(a, source, out);
            }
        }
        ExprKind::Index { base, index, .. } => {
            check_expr(base, source, out);
            check_expr(index, source, out);
        }
        ExprKind::Slice { base, lo, hi, .. } => {
            check_expr(base, source, out);
            check_expr(lo, source, out);
            check_expr(hi, source, out);
        }
        ExprKind::Field { base, .. } | ExprKind::Deref { base, .. } => {
            check_expr(base, source, out)
        }
        ExprKind::TupleField { base, .. } => check_expr(base, source, out),
        ExprKind::TypeApply { base, args, .. } => {
            check_expr(base, source, out);
            for t in args {
                check_ty(t, source, out);
            }
        }
        ExprKind::Cast { expr, .. } => check_expr(expr, source, out),
        ExprKind::StructLit { fields, .. } | ExprKind::StructShorthand { fields, .. } => {
            for f in fields {
                check_expr(&f.value, source, out);
            }
        }
        ExprKind::EnumLit {
            payload: Some(p), ..
        } => check_expr(p, source, out),
        ExprKind::EnumLit { payload: None, .. } => {}
        ExprKind::ArrayLit { len, elems, .. } => {
            check_expr(len, source, out);
            for e in elems {
                check_expr(e, source, out);
            }
        }
        ExprKind::Range { lo, hi, .. } => {
            check_expr(lo, source, out);
            check_expr(hi, source, out);
        }
        ExprKind::Tuple { elems, .. } => {
            for e in elems {
                check_expr(e, source, out);
            }
        }
        ExprKind::Paren(e) => check_expr(e, source, out),
        ExprKind::If {
            cond,
            then,
            otherwise,
            ..
        } => {
            check_expr(&cond.expr, source, out);
            check_expr(then, source, out);
            check_expr(otherwise, source, out);
        }
        ExprKind::Match {
            scrutinee, arms, ..
        } => {
            check_expr(scrutinee, source, out);
            for arm in arms {
                for p in &arm.patterns {
                    check_pattern(p, source, out);
                }
                if let Some(g) = &arm.guard {
                    check_expr(g, source, out);
                }
                match &arm.body {
                    ArmBody::Expr(e) => check_expr(e, source, out),
                    ArmBody::Block(b) => check_block(b, source, out),
                }
            }
        }
        ExprKind::Closure { body, .. } => check_block(body, source, out),
        ExprKind::Catch { value, handler, .. } => {
            check_expr(value, source, out);
            check_handler(handler, source, out);
        }
        ExprKind::Orelse { value, handler, .. } => {
            check_expr(value, source, out);
            check_handler(handler, source, out);
        }
        ExprKind::Spawn { call, scope, .. } => {
            check_expr(call, source, out);
            check_expr(scope, source, out);
        }
        ExprKind::Dispatch {
            call,
            domain,
            executor,
            ..
        } => {
            check_expr(call, source, out);
            check_expr(domain, source, out);
            check_expr(executor, source, out);
        }
        ExprKind::Unsafe { body, .. } => check_expr(body, source, out),
        ExprKind::Builtin { type_arg, args, .. } => {
            if let Some(t) = type_arg {
                check_ty(t, source, out);
            }
            for a in args {
                check_expr(a, source, out);
            }
        }
        ExprKind::Path(p) => {
            for a in &p.args {
                match a {
                    GenericArg::Ty(t) => check_ty(t, source, out),
                    GenericArg::Const(e) => check_expr(e, source, out),
                }
            }
        }
        _ => {}
    }
}

fn check_handler(h: &Handler, source: &Source, out: &mut Vec<Diagnostic>) {
    match h {
        Handler::Expr(e) => check_expr(e, source, out),
        Handler::Block(b) => check_block(b, source, out),
    }
}

fn check_mixing(
    child: &Expr,
    parent: BinOp,
    parent_span: Span,
    source: &Source,
    out: &mut Vec<Diagnostic>,
) {
    if let ExprKind::Binary { op, .. } = &child.kind {
        let conflict = if parent.is_bitwise() || parent.is_shift() {
            *op != parent
                && (op.is_bitwise() || op.is_shift() || op.is_arithmetic() || op.is_comparison())
        } else {
            op.is_bitwise() || op.is_shift()
        };
        if conflict {
            let span = if child.span.is_empty() {
                parent_span
            } else {
                child.span
            };
            let mut d = Diagnostic::new(
                "E0204",
                "bitwise and shift operators must be parenthesized when mixed with other operators",
                source,
                span,
            );
            d.fix_edits(
                "parenthesize the mixed sub-expression",
                vec![
                    Edit {
                        file: source.path.clone(),
                        span: Span::empty(span.start),
                        text: "(".to_string(),
                    },
                    Edit {
                        file: source.path.clone(),
                        span: Span::empty(span.end),
                        text: ")".to_string(),
                    },
                ],
            );
            out.push(d);
        }
    }
}

fn check_bitwise_child(child: &Expr, source: &Source, out: &mut Vec<Diagnostic>) {
    if let ExprKind::Binary { op, .. } = &child.kind
        && (op.is_bitwise() || op.is_shift())
    {
        let mut d = Diagnostic::new(
            "E0204",
            "bitwise and shift operators must be parenthesized when mixed with other operators",
            source,
            child.span,
        );
        d.fix_edits(
            "parenthesize the mixed sub-expression",
            vec![
                Edit {
                    file: source.path.clone(),
                    span: Span::empty(child.span.start),
                    text: "(".to_string(),
                },
                Edit {
                    file: source.path.clone(),
                    span: Span::empty(child.span.end),
                    text: ")".to_string(),
                },
            ],
        );
        out.push(d);
    }
}

#[cfg(test)]
mod tests {
    use crate::ast::{Item, Module};
    use crate::lexer::lex;
    use crate::parser::parse;
    use ingot_diag::diagnostic::Diagnostic;
    use ingot_diag::source::Source;

    fn parse_text(text: &str) -> (Module, Vec<Diagnostic>) {
        let source = Source::new("t.ingot", text);
        let (tokens, mut diagnostics) = lex(&source);
        let (module, parse_diagnostics) = parse(&source, tokens);
        diagnostics.extend(parse_diagnostics);
        (module, diagnostics)
    }

    fn extern_functions(text: &str) -> Vec<crate::ast::FnDecl> {
        let (module, diagnostics) = parse_text(text);
        assert!(
            diagnostics.is_empty(),
            "unexpected diagnostics: {diagnostics:?}"
        );
        let Some(Item::Extern(block)) = module.items.first() else {
            panic!("expected an extern block");
        };
        block.functions.clone()
    }

    #[test]
    fn variadic_parameter_after_fixed_ones_is_recorded() {
        let functions = extern_functions("extern \"c\" { fn printf(fmt: *u8, ...) -> i32; }");
        assert_eq!(functions.len(), 1);
        assert!(functions[0].variadic);
        assert_eq!(functions[0].params.len(), 1);
    }

    #[test]
    fn variadic_alone_is_an_empty_list_with_dots() {
        let functions = extern_functions("extern \"c\" { fn f(...); }");
        assert!(functions[0].variadic);
        assert!(functions[0].params.is_empty());
    }

    #[test]
    fn extern_function_without_dots_is_not_variadic() {
        let functions = extern_functions("extern \"c\" { fn g(a: i32, b: i32) -> i32; }");
        assert!(!functions[0].variadic);
        assert_eq!(functions[0].params.len(), 2);
    }

    #[test]
    fn dots_must_be_the_last_extern_parameter() {
        let (_, diagnostics) = parse_text("extern \"c\" { fn h(..., a: i32); }");
        assert!(diagnostics.iter().any(|d| d.code == "E0201"));
        let (_, diagnostics) = parse_text("extern \"c\" { fn m(a: i32, ...,); }");
        assert!(diagnostics.iter().any(|d| d.code == "E0201"));
    }

    #[test]
    fn dots_outside_an_extern_block_are_a_syntax_error() {
        let (_, diagnostics) = parse_text("fn k(...) {}");
        assert!(diagnostics.iter().any(|d| d.code == "E0201"));
    }
}
