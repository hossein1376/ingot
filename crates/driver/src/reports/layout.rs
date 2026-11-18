//! `ingot layout`: size, alignment, offsets and padding of one type (tools.md §4).

use crate::commands::Output;
use crate::files::Overlays;
use crate::pipeline::{self, Options};
use ingot_diag::diagnostic::Diagnostic;
use ingot_diag::json::Json;
use ingot_diag::source::{Sources, Span};
use ingot_sema::{
    ConstArg, ConstValue, Declarations, DefKind, FnPtrSig, GenericArg, IntTy, Lane, Layout,
    LayoutKind, Sema, TyKind, TypeId,
};
use ingot_syntax::lexer::lex;
use ingot_syntax::token::{Punct, ScalarTy, Token, TokenKind};
use std::path::Path;
use std::time::Instant;

pub fn layout(text: &str, target: Option<&str>, started: Instant) -> Output {
    let mut options = Options::default();
    if let Some(target) = target {
        if ingot_sema::Target::by_name(target).is_none() {
            return crate::commands::failure(
                "layout",
                format!("unknown target `{target}`"),
                started,
            );
        }
        options.target = target.to_string();
    }
    options.load.overlays = Overlays::default();
    options.load.cache = crate::cache::default_cache();
    let std_only =
        crate::loader::manifest_dir(Path::new("."), &crate::files::Files::default()).is_none();
    let loaded = if std_only {
        pipeline::analyze_std(&options)
    } else {
        pipeline::analyze_path_with(Path::new("."), &options)
    };
    let mut analysis = match loaded {
        Ok(analysis) => analysis,
        Err(error) => return crate::commands::failure("layout", error.to_string(), started),
    };
    let (diagnostics, diagnostic_human, any_diagnostic) =
        crate::reports::check_diagnostics(&analysis);
    let Some(front) = analysis.front_end.as_mut() else {
        if any_diagnostic {
            let mut lines = diagnostics;
            lines.push(summary(false, started));
            return Output {
                lines,
                human: diagnostic_human,
                error: None,
                not_implemented: Vec::new(),
                exit: 1,
            };
        }
        return crate::commands::failure(
            "layout",
            "the current directory has no package".to_string(),
            started,
        );
    };
    let mut parser = Parser::new(text, &front.resolved.sema, &mut front.declarations);
    let ty = match parser.parse() {
        Ok(ty) => ty,
        Err(message) => {
            return unresolved(text, &message, diagnostics, diagnostic_human, started);
        }
    };
    let ty = match parser.expand_alias(ty) {
        Ok(ty) => ty,
        Err(message) => {
            return unresolved(text, &message, diagnostics, diagnostic_human, started);
        }
    };
    match parser.declarations.layout_of(ty) {
        Ok(layout) => {
            let value = layout_json(&mut parser, ty, text, &layout);
            let mut human = format!("{}: size {}, align {}\n", text, layout.size, layout.align);
            for field in &layout.fields {
                human.push_str(&format!(
                    "  {}: offset {}, size {}\n",
                    field.name, field.offset, field.size
                ));
            }
            let mut lines = diagnostics;
            lines.push(value.to_text());
            lines.push(summary(!any_diagnostic, started));
            let mut human = diagnostic_human + &human;
            if any_diagnostic {
                human.push('\n');
            }
            Output {
                lines,
                human,
                error: None,
                not_implemented: Vec::new(),
                exit: if any_diagnostic { 1 } else { 0 },
            }
        }
        Err(error) => {
            let message = match error {
                ingot_sema::LayoutError::Params => {
                    "the type has generic parameters; give them all".to_string()
                }
                ingot_sema::LayoutError::Cycle => {
                    "the type contains itself by value (E0516)".to_string()
                }
                ingot_sema::LayoutError::Poison => "the type did not resolve".to_string(),
                ingot_sema::LayoutError::Unspecified(message) => message,
            };
            unresolved(text, &message, diagnostics, diagnostic_human, started)
        }
    }
}

fn summary(ok: bool, started: Instant) -> String {
    format!(
        "{{\"type\":\"summary\",\"command\":\"layout\",\"ok\":{ok},\"protocol\":1,\"ms\":{}}}",
        started.elapsed().as_millis()
    )
}

/// An unresolved or unsized type is a diagnostic line, as `check` prints one, after
/// the build's own diagnostics.
fn unresolved(
    text: &str,
    message: &str,
    mut lines: Vec<String>,
    mut human: String,
    started: Instant,
) -> Output {
    let source = pipeline::command_line_source(text);
    let span = Span::new(0, text.len());
    let diagnostic = Diagnostic::new("E0305", message.to_string(), &source, span);
    let mut sources = Sources::default();
    sources.add(source);
    lines.push(diagnostic.to_json(&sources));
    human.push_str(&diagnostic.to_human(&sources));
    lines.push(summary(false, started));
    Output {
        lines,
        human,
        error: None,
        not_implemented: Vec::new(),
        exit: 1,
    }
}

fn layout_json(parser: &mut Parser<'_, '_>, ty: TypeId, text: &str, layout: &Layout) -> Json {
    let mut members = vec![
        ("type".to_string(), Json::String("layout".to_string())),
        ("name".to_string(), Json::String(text.trim().to_string())),
        ("size".to_string(), Json::Number(layout.size.to_string())),
        ("align".to_string(), Json::Number(layout.align.to_string())),
    ];
    match &layout.kind {
        LayoutKind::Enum {
            tag_size,
            payload_offset,
        } => {
            let mut synthetic = Vec::new();
            let (tag_name, variants) = match parser.declarations.types.kind(ty).clone() {
                TyKind::Adt(def, _) => {
                    let tag = parser
                        .declarations
                        .enum_sig(def)
                        .map(|sig| sig.tag_ty.name().to_string())
                        .unwrap_or_else(|| "u8".to_string());
                    let variants = parser
                        .declarations
                        .enum_sig(def)
                        .map(|sig| sig.variants.clone())
                        .unwrap_or_default();
                    (tag, variants)
                }
                TyKind::Optional(inner) => {
                    synthetic = vec![
                        ("none".to_string(), None),
                        ("some".to_string(), Some(inner)),
                    ];
                    ("u8".to_string(), Vec::new())
                }
                TyKind::Result(value, error) => {
                    synthetic = vec![
                        ("ok".to_string(), Some(value)),
                        ("err".to_string(), Some(error)),
                    ];
                    ("u8".to_string(), Vec::new())
                }
                _ => ("u8".to_string(), Vec::new()),
            };
            members.push((
                "tag".to_string(),
                Json::Object(vec![
                    ("type".to_string(), Json::String(tag_name)),
                    ("offset".to_string(), Json::Number("0".to_string())),
                    ("size".to_string(), Json::Number(tag_size.to_string())),
                ]),
            ));
            let mut out = Vec::new();
            let named: Vec<(String, Option<TypeId>)> = if !synthetic.is_empty() {
                synthetic
            } else {
                variants
                    .into_iter()
                    .map(|variant| (variant.name, variant.payload))
                    .collect()
            };
            for (name, payload) in named {
                let found = layout.fields.iter().find(|field| field.name == name);
                let mut object = vec![("name".to_string(), Json::String(name))];
                if let Some(payload) = payload {
                    object.push((
                        "type".to_string(),
                        Json::String(parser.declarations.types.display(parser.sema, payload)),
                    ));
                    object.push((
                        "offset".to_string(),
                        Json::Number(
                            found
                                .map(|field| field.offset)
                                .unwrap_or(*payload_offset)
                                .to_string(),
                        ),
                    ));
                    object.push((
                        "size".to_string(),
                        Json::Number(found.map(|field| field.size).unwrap_or(0).to_string()),
                    ));
                }
                out.push(Json::Object(object));
            }
            members.push(("variants".to_string(), Json::Array(out)));
            let mut intervals = vec![(0u64, *tag_size)];
            intervals.extend(layout.fields.iter().map(|field| (field.offset, field.size)));
            members.push(("padding".to_string(), padding(&intervals, layout.size)));
        }
        _ => {
            if !layout.fields.is_empty() {
                let types = field_types(parser, ty);
                let mut fields = Vec::new();
                for field in &layout.fields {
                    let mut object = vec![("name".to_string(), Json::String(field.name.clone()))];
                    if let Some((_, Some(field_ty))) =
                        types.iter().find(|(name, _)| name == &field.name)
                    {
                        object.push((
                            "type".to_string(),
                            Json::String(parser.declarations.types.display(parser.sema, *field_ty)),
                        ));
                    }
                    object.push(("offset".to_string(), Json::Number(field.offset.to_string())));
                    object.push(("size".to_string(), Json::Number(field.size.to_string())));
                    fields.push(Json::Object(object));
                }
                members.push(("fields".to_string(), Json::Array(fields)));
                let intervals: Vec<(u64, u64)> = layout
                    .fields
                    .iter()
                    .map(|field| (field.offset, field.size))
                    .collect();
                members.push(("padding".to_string(), padding(&intervals, layout.size)));
            }
        }
    }
    Json::Object(members)
}

fn padding(intervals: &[(u64, u64)], size: u64) -> Json {
    let mut sorted: Vec<(u64, u64)> = intervals.to_vec();
    sorted.sort();
    let mut out = Vec::new();
    let mut cursor = 0u64;
    for (offset, length) in sorted {
        if offset > cursor {
            out.push(Json::Object(vec![
                ("offset".to_string(), Json::Number(cursor.to_string())),
                (
                    "size".to_string(),
                    Json::Number((offset - cursor).to_string()),
                ),
            ]));
        }
        cursor = cursor.max(offset.saturating_add(length));
    }
    if cursor < size {
        out.push(Json::Object(vec![
            ("offset".to_string(), Json::Number(cursor.to_string())),
            (
                "size".to_string(),
                Json::Number((size - cursor).to_string()),
            ),
        ]));
    }
    Json::Array(out)
}

/// The declared type of each layout field, by name, when the type has them.
fn field_types(parser: &mut Parser<'_, '_>, ty: TypeId) -> Vec<(String, Option<TypeId>)> {
    match parser.declarations.types.kind(ty).clone() {
        TyKind::Adt(def, args) => {
            let subst = parser.declarations.subst_for(def, &args);
            match parser.declarations.item(def).cloned() {
                Some(ingot_sema::ItemSig::Struct(sig)) => sig
                    .fields
                    .iter()
                    .map(|field| {
                        (
                            field.name.clone(),
                            Some(parser.declarations.types.subst(field.ty, &subst)),
                        )
                    })
                    .collect(),
                _ => Vec::new(),
            }
        }
        TyKind::Tuple(items) => items
            .iter()
            .enumerate()
            .map(|(index, item)| (index.to_string(), Some(*item)))
            .collect(),
        TyKind::Optional(inner) => vec![("some".to_string(), Some(inner))],
        TyKind::Result(value, error) => vec![
            ("ok".to_string(), Some(value)),
            ("err".to_string(), Some(error)),
        ],
        _ => Vec::new(),
    }
}

struct Parser<'a, 'b> {
    tokens: Vec<Token>,
    pos: usize,
    sema: &'a Sema,
    declarations: &'b mut Declarations,
}

impl<'a, 'b> Parser<'a, 'b> {
    fn new(text: &str, sema: &'a Sema, declarations: &'b mut Declarations) -> Parser<'a, 'b> {
        let source = ingot_diag::source::Source::new("<layout>", text);
        let (tokens, _) = lex(&source);
        Parser {
            tokens,
            pos: 0,
            sema,
            declarations,
        }
    }

    fn parse(&mut self) -> Result<TypeId, String> {
        let ty = self.ty()?;
        if self.peek() != Some(&TokenKind::Eof) {
            return Err(format!(
                "unexpected {} after the type",
                self.peek().map(TokenKind::describe).unwrap_or_default()
            ));
        }
        Ok(ty)
    }

    fn peek(&self) -> Option<&TokenKind> {
        self.tokens.get(self.pos).map(|token| &token.kind)
    }

    fn bump(&mut self) -> Option<TokenKind> {
        let token = self.tokens.get(self.pos)?.kind.clone();
        self.pos += 1;
        Some(token)
    }

    fn eat_punct(&mut self, punct: Punct) -> bool {
        if self.peek().is_some_and(|kind| kind.is_punct(punct)) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn eat_keyword(&mut self, keyword: ingot_syntax::token::Kw) -> bool {
        if self.peek().is_some_and(|kind| kind.is_kw(keyword)) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn expect_punct(&mut self, punct: Punct) -> Result<(), String> {
        if self.eat_punct(punct) {
            Ok(())
        } else {
            Err(format!("expected `{}`", punct.as_str()))
        }
    }

    fn ty(&mut self) -> Result<TypeId, String> {
        let Some(kind) = self.bump() else {
            return Err("expected a type".to_string());
        };
        match kind {
            TokenKind::Scalar(scalar) => self.scalar(scalar),
            TokenKind::Vector(vector) => {
                if vector.elem == ScalarTy::Bool {
                    return Ok(self
                        .declarations
                        .types
                        .intern(TyKind::Mask(u32::from(vector.lanes))));
                }
                let lane = match vector.elem {
                    ScalarTy::I8 => Lane::Int(IntTy::I8),
                    ScalarTy::I16 => Lane::Int(IntTy::I16),
                    ScalarTy::I32 => Lane::Int(IntTy::I32),
                    ScalarTy::I64 => Lane::Int(IntTy::I64),
                    ScalarTy::U8 => Lane::Int(IntTy::U8),
                    ScalarTy::U16 => Lane::Int(IntTy::U16),
                    ScalarTy::U32 => Lane::Int(IntTy::U32),
                    ScalarTy::U64 => Lane::Int(IntTy::U64),
                    ScalarTy::F32 => Lane::Float(ingot_sema::FloatTy::F32),
                    ScalarTy::F64 => Lane::Float(ingot_sema::FloatTy::F64),
                    _ => {
                        return Err("a vector's lanes must be integers, floats or bool".to_string());
                    }
                };
                Ok(self
                    .declarations
                    .types
                    .intern(TyKind::Vector(lane, u32::from(vector.lanes))))
            }
            TokenKind::Punct(Punct::Amp) => {
                let mutable = self.eat_keyword(ingot_syntax::token::Kw::Mut);
                let inner = self.ty()?;
                Ok(self
                    .declarations
                    .types
                    .intern(TyKind::Ref(mutability(mutable), inner)))
            }
            TokenKind::Punct(Punct::Question) => {
                let inner = self.ty()?;
                Ok(self.declarations.types.intern(TyKind::Optional(inner)))
            }
            TokenKind::Punct(Punct::Star) => {
                let mutable = self.eat_keyword(ingot_syntax::token::Kw::Mut);
                let inner = self.ty()?;
                Ok(self
                    .declarations
                    .types
                    .intern(TyKind::RawPtr(mutability(mutable), inner)))
            }
            TokenKind::Punct(Punct::LBracket) => {
                if self.eat_punct(Punct::RBracket) {
                    let mutable = self.eat_keyword(ingot_syntax::token::Kw::Mut);
                    let inner = self.ty()?;
                    return Ok(self
                        .declarations
                        .types
                        .intern(TyKind::Slice(mutability(mutable), inner)));
                }
                let length = match self.bump() {
                    Some(TokenKind::Int(value)) => value as i128,
                    other => {
                        return Err(format!(
                            "an array length must be an integer, not {}",
                            other.map_or("nothing".to_string(), |kind| kind.describe())
                        ));
                    }
                };
                self.expect_punct(Punct::RBracket)?;
                let inner = self.ty()?;
                Ok(self.declarations.types.intern(TyKind::Array(
                    inner,
                    ConstArg::Value(ConstValue::Int {
                        ty: IntTy::I64,
                        value: length,
                    }),
                )))
            }
            TokenKind::Punct(Punct::LParen) => {
                let mut items = Vec::new();
                loop {
                    items.push(self.ty()?);
                    if self.eat_punct(Punct::Comma) {
                        if self.peek().is_some_and(|kind| kind.is_punct(Punct::RParen)) {
                            break;
                        }
                        continue;
                    }
                    break;
                }
                self.expect_punct(Punct::RParen)?;
                Ok(self.declarations.types.intern(TyKind::Tuple(items)))
            }
            TokenKind::Keyword(ingot_syntax::token::Kw::Fn)
            | TokenKind::Keyword(ingot_syntax::token::Kw::Extern) => {
                let ext = matches!(kind, TokenKind::Keyword(ingot_syntax::token::Kw::Extern));
                if ext && !self.eat_keyword(ingot_syntax::token::Kw::Fn) {
                    return Err("expected `fn` after `extern`".to_string());
                }
                self.expect_punct(Punct::LParen)?;
                let mut params = Vec::new();
                while !self.peek().is_some_and(|kind| kind.is_punct(Punct::RParen)) {
                    params.push(self.ty()?);
                    if !self.eat_punct(Punct::Comma) {
                        break;
                    }
                }
                self.expect_punct(Punct::RParen)?;
                let mut ret = self.declarations.types.void;
                let mut error = None;
                if self.eat_punct(Punct::Arrow) {
                    ret = self.ty()?;
                    if self.eat_punct(Punct::Bang) {
                        error = Some(self.ty()?);
                    }
                }
                Ok(self.declarations.types.intern(TyKind::FnPtr(FnPtrSig {
                    ext,
                    params,
                    ret,
                    error,
                })))
            }
            TokenKind::Ident(name) => self.path_type(name, false),
            TokenKind::TypeIdent(name) => self.path_type(name, true),
            other => Err(format!("expected a type, found {}", other.describe())),
        }
    }

    fn scalar(&mut self, scalar: ScalarTy) -> Result<TypeId, String> {
        Ok(match scalar {
            ScalarTy::I8 => self.declarations.types.int(IntTy::I8),
            ScalarTy::I16 => self.declarations.types.int(IntTy::I16),
            ScalarTy::I32 => self.declarations.types.int(IntTy::I32),
            ScalarTy::I64 => self.declarations.types.int(IntTy::I64),
            ScalarTy::U8 => self.declarations.types.int(IntTy::U8),
            ScalarTy::U16 => self.declarations.types.int(IntTy::U16),
            ScalarTy::U32 => self.declarations.types.int(IntTy::U32),
            ScalarTy::U64 => self.declarations.types.int(IntTy::U64),
            ScalarTy::F32 => self
                .declarations
                .types
                .intern(TyKind::Float(ingot_sema::FloatTy::F32)),
            ScalarTy::F64 => self
                .declarations
                .types
                .intern(TyKind::Float(ingot_sema::FloatTy::F64)),
            ScalarTy::Bool => self.declarations.types.intern(TyKind::Bool),
            ScalarTy::Void => self.declarations.types.void,
            ScalarTy::Never => self.declarations.types.intern(TyKind::Never),
        })
    }

    fn path_type(&mut self, first: String, mut last_is_type: bool) -> Result<TypeId, String> {
        let mut segments = vec![first];
        while self.eat_punct(Punct::Dot) {
            match self.bump() {
                Some(TokenKind::Ident(name)) => {
                    segments.push(name);
                    last_is_type = false;
                }
                Some(TokenKind::TypeIdent(name)) => {
                    segments.push(name);
                    last_is_type = true;
                }
                other => {
                    return Err(format!(
                        "expected a name after `.`, found {}",
                        other.map_or("nothing".to_string(), |kind| kind.describe())
                    ));
                }
            }
        }
        if !last_is_type {
            return Err(format!("`{}` does not name a type", segments.join(".")));
        }
        let mut args = Vec::new();
        if self.eat_punct(Punct::LBracket) {
            while !self
                .peek()
                .is_some_and(|kind| kind.is_punct(Punct::RBracket))
            {
                if self.peek().is_some_and(is_type_start) {
                    args.push(GenericArg::Ty(self.ty()?));
                } else {
                    match self.bump() {
                        Some(TokenKind::Int(value)) => {
                            args.push(GenericArg::Const(ConstArg::Value(ConstValue::Int {
                                ty: IntTy::I64,
                                value: value as i128,
                            })))
                        }
                        Some(TokenKind::Keyword(ingot_syntax::token::Kw::True)) => {
                            args.push(GenericArg::Const(ConstArg::Value(ConstValue::Bool(true))))
                        }
                        Some(TokenKind::Keyword(ingot_syntax::token::Kw::False)) => {
                            args.push(GenericArg::Const(ConstArg::Value(ConstValue::Bool(false))))
                        }
                        other => {
                            return Err(format!(
                                "a type argument must be a type or a constant, not {}",
                                other.map_or("nothing".to_string(), |kind| kind.describe())
                            ));
                        }
                    }
                }
                if !self.eat_punct(Punct::Comma) {
                    break;
                }
            }
            self.expect_punct(Punct::RBracket)?;
        }
        // A trailing `.[Assoc]` type is not a layout request.
        if self.peek().is_some_and(|kind| kind.is_punct(Punct::Dot)) {
            return Err("an associated type has no layout of its own".to_string());
        }
        let name = segments.last().cloned().unwrap_or_default();
        let module_path = segments[..segments.len() - 1].join(".");
        let module = self
            .sema
            .module_paths
            .get(&module_path)
            .copied()
            .ok_or_else(|| format!("`{module_path}` is not a module in this build"))?;
        let def = self
            .sema
            .module(module)
            .types
            .get(&name)
            .copied()
            .ok_or_else(|| format!("`{}` is not a type in `{module_path}`", name))?;
        if !matches!(
            self.sema.def(def).kind,
            DefKind::Struct | DefKind::Enum | DefKind::TypeAlias
        ) {
            return Err(format!("`{}` is not a struct or enum", segments.join(".")));
        }
        let expected = self.declarations.generics_of(def).len();
        if args.len() != expected {
            return Err(format!(
                "`{}` takes {expected} type argument(s), not {}",
                segments.join("."),
                args.len()
            ));
        }
        Ok(self.declarations.types.intern(TyKind::Adt(def, args)))
    }

    fn expand_alias(&mut self, ty: TypeId) -> Result<TypeId, String> {
        match self.declarations.types.kind(ty).clone() {
            TyKind::Adt(def, args) if self.sema.def(def).kind == DefKind::TypeAlias => {
                let Some(sig) = self.declarations.item(def).and_then(|item| match item {
                    ingot_sema::ItemSig::Alias(sig) => Some(sig),
                    _ => None,
                }) else {
                    return Err("the alias has no target".to_string());
                };
                let Some(target) = sig.ty else {
                    return Err("the alias has no target".to_string());
                };
                let subst = self.declarations.subst_for(def, &args);
                let target = self.declarations.types.subst(target, &subst);
                self.expand_alias(target)
            }
            _ => Ok(ty),
        }
    }
}

fn is_type_start(kind: &TokenKind) -> bool {
    matches!(
        kind,
        TokenKind::Scalar(_) | TokenKind::Vector(_) | TokenKind::Ident(_) | TokenKind::TypeIdent(_)
    ) || matches!(
        kind,
        TokenKind::Punct(Punct::Amp)
            | TokenKind::Punct(Punct::Question)
            | TokenKind::Punct(Punct::Star)
            | TokenKind::Punct(Punct::LBracket)
            | TokenKind::Punct(Punct::LParen)
    ) || matches!(
        kind,
        TokenKind::Keyword(ingot_syntax::token::Kw::Fn)
            | TokenKind::Keyword(ingot_syntax::token::Kw::Extern)
    )
}

fn mutability(mutable: bool) -> ingot_sema::Mutability {
    if mutable {
        ingot_sema::Mutability::Mut
    } else {
        ingot_sema::Mutability::Shared
    }
}
