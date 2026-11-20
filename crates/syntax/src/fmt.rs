//! `ingot fmt`: the canonical layout of section 34 of the tooling document.
//!
//! The formatter lays the AST out as a small document tree. A `Choice` uses its flat
//! form when it fits in 80 columns and its broken form otherwise, so the outermost
//! construct that does not fit breaks first. Comments are placed after rendering, by
//! the source token each one followed or preceded.

use crate::ast::{self, ArmBody, BinOp, Expr, ExprKind, Item, Pattern, Stmt, Ty, UnOp};
use crate::lexer::{Comment, lex_with_comments};
use crate::parser::parse;
use ingot_diag::source::{Source, Span};

#[derive(Clone, Debug)]
pub struct FormatError {
    pub message: String,
}

impl FormatError {
    fn new(message: impl Into<String>) -> FormatError {
        FormatError {
            message: message.into(),
        }
    }
}

/// Format one file. A lexical or syntax error leaves it unchanged; the caller decides
/// whether that is a failure or a message.
pub fn format_text(text: &str) -> Result<String, FormatError> {
    let source = Source::new("<fmt>", text);
    format_source(&source)
}

pub fn format_source(source: &Source) -> Result<String, FormatError> {
    let (tokens, comments, diagnostics) = lex_with_comments(source);
    if !diagnostics.is_empty() {
        return Err(FormatError::new("the file has a lexical error"));
    }
    let format_tokens = tokens.clone();
    let (module, problems) = parse(source, tokens);
    if !problems.is_empty() {
        return Err(FormatError::new("the file has a syntax error"));
    }
    let mut formatter = Formatter {
        source,
        comments,
        tokens: format_tokens,
    };
    let doc = formatter.module(&module);
    let lines = render(&doc);
    Ok(formatter.place_comments(lines))
}

/// Compare the tokens and comment texts of an input and a formatted file. White
/// space, trailing commas before a closing bracket and import order within a group do
/// not count.
pub fn verify(before: &str, after: &str) -> Result<(), FormatError> {
    let a = Source::new("<before>", before);
    let b = Source::new("<after>", after);
    let (at, ac, ad) = lex_with_comments(&a);
    let (bt, bc, bd) = lex_with_comments(&b);
    if !ad.is_empty() || !bd.is_empty() {
        return Err(FormatError::new("a file no longer lexes"));
    }
    let at = normalize_tokens(&a, &at);
    let bt = normalize_tokens(&b, &bt);
    if at != bt {
        let at_index = at.iter().zip(&bt).position(|(a, b)| a != b);
        let message = match at_index {
            Some(index) => format!(
                "the formatted file has another token sequence at {}: `{}` != `{}`",
                index,
                at.get(index).map(String::as_str).unwrap_or(""),
                bt.get(index).map(String::as_str).unwrap_or("")
            ),
            None => "the formatted file has another token sequence".to_string(),
        };
        return Err(FormatError::new(message));
    }
    let mut a_text: Vec<&str> = ac.iter().map(|c| c.text.as_str()).collect();
    let mut b_text: Vec<&str> = bc.iter().map(|c| c.text.as_str()).collect();
    a_text.sort();
    b_text.sort();
    if a_text != b_text {
        return Err(FormatError::new("the formatted file lost a comment"));
    }
    Ok(())
}

fn normalize_tokens(source: &Source, tokens: &[crate::token::Token]) -> Vec<String> {
    let mut texts = Vec::new();
    for (index, token) in tokens.iter().enumerate() {
        let text = source
            .text
            .get(token.span.start..token.span.end)
            .unwrap_or("")
            .to_string();
        if text == "," {
            let next = tokens
                .get(index + 1)
                .map(|next| {
                    source
                        .text
                        .get(next.span.start..next.span.end)
                        .unwrap_or("")
                })
                .unwrap_or("");
            if matches!(next, ")" | "]" | "}") {
                continue;
            }
        }
        texts.push(text);
    }
    // Imports are one namespace at the top of a file. Their order is part of the
    // layout, not of the token sequence.
    let mut out = Vec::new();
    let mut imports = Vec::new();
    let mut i = 0;
    while i < texts.len() {
        if texts[i] == "import" {
            let start = i;
            while i < texts.len() && texts[i] != ";" {
                i += 1;
            }
            if i < texts.len() {
                i += 1;
            }
            imports.push(texts[start..i].join(" "));
            continue;
        }
        out.push(texts[i].clone());
        i += 1;
    }
    imports.sort();
    let mut result = imports;
    result.extend(out);
    result
}

// ---------------------------------------------------------------- document

#[derive(Clone, Debug)]
enum Doc {
    Nil,
    Text {
        text: String,
        span: Span,
    },
    HardLine,
    Nest(usize, Box<Doc>),
    Concat(Vec<Doc>),
    Choice {
        flat: Box<Doc>,
        broken: Box<Doc>,
        force: bool,
        /// A list or a one-line block. When a construct has several of them, the last
        /// breaks first: for the text before it, this one counts as broken, so the
        /// line ends at its opening bracket.
        last: bool,
    },
}

fn end_span(span: Span) -> Span {
    Span::new(span.end.saturating_sub(1), span.end)
}

fn text(text: impl Into<String>, span: Span) -> Doc {
    Doc::Text {
        text: text.into(),
        span,
    }
}

fn nil() -> Doc {
    Doc::Nil
}

fn concat(docs: Vec<Doc>) -> Doc {
    Doc::Concat(docs)
}

fn hard() -> Doc {
    Doc::HardLine
}

fn nest(depth: usize, doc: Doc) -> Doc {
    Doc::Nest(depth, Box::new(doc))
}

fn choice(flat: Doc, broken: Doc, force: bool) -> Doc {
    Doc::Choice {
        flat: Box::new(flat),
        broken: Box::new(broken),
        force,
        last: false,
    }
}

fn last_choice(flat: Doc, broken: Doc, force: bool) -> Doc {
    Doc::Choice {
        flat: Box::new(flat),
        broken: Box::new(broken),
        force,
        last: true,
    }
}

// ---------------------------------------------------------------- render

#[derive(Clone, Debug, Default)]
struct Line {
    text: String,
    first_start: usize,
    max_end: usize,
    starts: Vec<u32>,
    has_code: bool,
}

fn width(doc: &Doc, column: usize) -> Option<usize> {
    match doc {
        Doc::Nil => Some(column),
        Doc::HardLine => None,
        Doc::Text { text, .. } => Some(column + text.chars().count()),
        Doc::Nest(_, doc) => width(doc, column),
        Doc::Concat(docs) => {
            let mut column = column;
            for doc in docs {
                column = width(doc, column)?;
            }
            Some(column)
        }
        Doc::Choice { flat, force, .. } => {
            if *force {
                None
            } else {
                width(flat, column)
            }
        }
    }
}

/// The width before the first hard line, and whether a hard line was reached.
fn prefix_width(doc: &Doc, column: usize) -> (usize, bool) {
    match doc {
        Doc::Nil => (column, false),
        Doc::HardLine => (column, true),
        Doc::Text { text, .. } => (column + text.chars().count(), false),
        Doc::Nest(_, doc) => prefix_width(doc, column),
        Doc::Concat(docs) => {
            let mut column = column;
            for doc in docs {
                let (next, ended) = prefix_width(doc, column);
                column = next;
                if ended {
                    return (column, true);
                }
            }
            (column, false)
        }
        Doc::Choice {
            flat,
            broken,
            force,
            last,
        } => {
            if *last {
                prefix_width(broken, column)
            } else if *force {
                (column, false)
            } else {
                prefix_width(flat, column)
            }
        }
    }
}

fn tail_width(docs: &[Doc]) -> (usize, bool) {
    let mut column = 0;
    for doc in docs {
        let (next, ended) = prefix_width(doc, column);
        column = next;
        if ended {
            return (column, true);
        }
    }
    (column, false)
}

fn fits(flat: &Doc, column: usize, tail: usize) -> bool {
    width(flat, column).is_some_and(|width| width + tail <= 80)
}

fn render(doc: &Doc) -> Vec<Line> {
    let mut lines = vec![Line::default()];
    render_doc(doc, 0, 0, 0, &mut lines);
    if lines
        .last()
        .is_some_and(|line| !line.has_code && line.text.is_empty())
    {
        lines.pop();
    }
    lines
}

fn push_text(lines: &mut [Line], depth: usize, text: &str, span: Span) -> usize {
    let line = lines.last_mut().unwrap();
    let mut added = 0;
    if line.text.is_empty() && !text.is_empty() {
        let indent = "    ".repeat(depth);
        added += indent.chars().count();
        line.text.push_str(&indent);
    }
    line.text.push_str(text);
    added += text.chars().count();
    if (span.start != 0 || !text.is_empty()) && !line.starts.contains(&(span.start as u32)) {
        line.starts.push(span.start as u32);
    }
    if !line.has_code {
        line.first_start = span.start;
        line.max_end = span.end;
        line.has_code = true;
    } else {
        line.max_end = line.max_end.max(span.end);
    }
    added
}

fn newline(lines: &mut Vec<Line>, _depth: usize) {
    lines.push(Line {
        text: String::new(),
        first_start: usize::MAX,
        max_end: 0,
        starts: Vec::new(),
        has_code: false,
    });
}

fn render_doc(doc: &Doc, depth: usize, column: usize, tail: usize, lines: &mut Vec<Line>) -> usize {
    match doc {
        Doc::Nil => column,
        Doc::Text { text, span } => column + push_text(lines, depth, text, *span),
        Doc::HardLine => {
            newline(lines, depth);
            0
        }
        Doc::Nest(extra, doc) => render_doc(doc, depth + extra, column, tail, lines),
        Doc::Concat(docs) => {
            let mut column = column;
            for (index, doc) in docs.iter().enumerate() {
                let (local, ended) = tail_width(&docs[index + 1..]);
                let tail = if ended { local } else { local + tail };
                column = render_doc(doc, depth, column, tail, lines);
            }
            column
        }
        Doc::Choice {
            flat,
            broken,
            force,
            ..
        } => {
            if !*force && fits(flat, column, tail) {
                render_doc(flat, depth, column, tail, lines)
            } else {
                render_doc(broken, depth, column, tail, lines)
            }
        }
    }
}

// ---------------------------------------------------------------- formatter

struct Formatter<'a> {
    source: &'a Source,
    comments: Vec<Comment>,
    tokens: Vec<crate::token::Token>,
}

impl<'a> Formatter<'a> {
    fn span(&self, span: Span) -> &str {
        self.source.text.get(span.start..span.end).unwrap_or("")
    }

    fn has_comment(&self, span: Span) -> bool {
        self.comments
            .iter()
            .any(|comment| comment.span.start > span.start && comment.span.end < span.end)
    }

    fn comment_count(&self, span: Span) -> usize {
        self.comments
            .iter()
            .filter(|comment| comment.span.start > span.start && comment.span.end < span.end)
            .count()
    }

    /// The name of a `test` or `bench`, as written: the first string token of the item.
    fn test_name(&self, item: &ast::TestDecl) -> String {
        self.tokens
            .iter()
            .filter(|token| token.span.start >= item.span.start)
            .map(|token| self.span(token.span))
            .find(|text| text.starts_with('"'))
            .map(str::to_string)
            .unwrap_or_default()
    }

    /// The comma that follows a list item, or an empty span where it will be written.
    fn comma_after(&self, span: Span) -> Span {
        match self
            .tokens
            .iter()
            .find(|token| token.span.start >= span.end)
        {
            Some(token) if self.span(token.span) == "," => token.span,
            _ => Span::new(span.end, span.end),
        }
    }

    /// Whether a statement holds a block of its own: a `{` that does not open a
    /// struct, array or pattern literal.
    fn stmt_has_block(&self, stmt: &Stmt) -> bool {
        let span = stmt.span();
        self.tokens.iter().any(|token| {
            token.span.start >= span.start
                && token.span.end <= span.end
                && self.span(token.span) == "{"
        })
    }

    fn module(&mut self, module: &ast::Module) -> Doc {
        let mut docs = Vec::new();
        if !module.imports.is_empty() {
            docs.push(self.imports(&module.imports));
        }
        let last_import = module.imports.last().map(|import| import.span);
        for (index, item) in module.items.iter().enumerate() {
            if index > 0 || !module.imports.is_empty() {
                docs.push(hard());
                let blank = if index == 0 {
                    self.blank_lines(last_import.unwrap_or_default(), item.span())
                } else {
                    self.blank_between(module.items.get(index.wrapping_sub(1)), Some(item))
                };
                if blank {
                    docs.push(hard());
                }
            }
            docs.push(self.item(item, 0));
        }
        concat(docs)
    }

    fn blank_between(&self, previous: Option<&Item>, next: Option<&Item>) -> bool {
        let (Some(previous), Some(next)) = (previous, next) else {
            return false;
        };
        self.blank_lines(previous.span(), next.span())
    }

    fn blank_lines(&self, previous: Span, next: Span) -> bool {
        let target = self
            .comments
            .iter()
            .filter(|comment| {
                comment.span.start >= previous.end
                    && comment.span.start < next.start
                    && self.source.position(comment.span.start).line
                        > self.source.position(previous.end).line
            })
            .map(|comment| comment.span.start)
            .min()
            .unwrap_or(next.start);
        let previous_line = self.source.position(previous.end).line;
        let target_line = self.source.position(target).line;
        target_line >= previous_line + 2
    }

    fn imports(&mut self, imports: &[ast::Import]) -> Doc {
        let mut groups: Vec<Vec<&ast::Import>> = Vec::new();
        let mut group = Vec::new();
        for (index, import) in imports.iter().enumerate() {
            if index > 0 && self.blank_lines(imports[index - 1].span, import.span) {
                groups.push(std::mem::take(&mut group));
            }
            group.push(import);
        }
        if !group.is_empty() {
            groups.push(group);
        }
        let mut docs = Vec::new();
        for (group_index, group) in groups.iter_mut().enumerate() {
            if group_index > 0 {
                docs.push(hard());
                docs.push(hard());
            }
            group.sort_by_key(|import| self.import_sort(import));
            for (index, import) in group.iter().enumerate() {
                if index > 0 {
                    docs.push(hard());
                }
                docs.push(self.import(import));
            }
        }
        concat(docs)
    }

    fn import_sort(&self, import: &ast::Import) -> String {
        let mut text = import.path_text();
        if let Some(alias) = &import.alias {
            text.push_str(" as ");
            text.push_str(alias);
        }
        text
    }

    fn import(&self, import: &ast::Import) -> Doc {
        let mut docs = vec![text("import ", import.span)];
        for (index, (segment, span)) in import.segments.iter().enumerate() {
            if index > 0 {
                docs.push(text(".", *span));
            }
            docs.push(text(segment.clone(), *span));
        }
        if let Some(alias) = &import.alias {
            docs.push(text(" as ", import.span));
            docs.push(text(
                alias.clone(),
                import.alias_span.unwrap_or(import.span),
            ));
        }
        docs.push(text(";", end_span(import.span)));
        concat(docs)
    }
}

// ---------------------------------------------------------------- items

impl Formatter<'_> {
    fn item(&mut self, item: &Item, depth: usize) -> Doc {
        match item {
            Item::Fn(function) => self.function(function, depth),
            Item::Struct(item) => self.struct_decl(item, depth),
            Item::Enum(item) => self.enum_decl(item),
            Item::Interface(item) => self.interface_decl(item, depth),
            Item::Impl(item) => self.impl_decl(item, depth),
            Item::Const(item) => {
                let mut docs = vec![self.vis(item.public, item.pkg, item.span)];
                docs.push(text("const ", item.span));
                docs.push(text(item.name.clone(), item.name_span));
                docs.push(text(": ", item.span));
                docs.push(self.ty(&item.ty));
                docs.push(text(" = ", item.span));
                docs.push(self.expr(&item.value));
                docs.push(text(";", end_span(item.span)));
                concat(docs)
            }
            Item::Static(item) => {
                let mut docs = vec![self.vis(item.public, item.pkg, item.span)];
                docs.push(text("static ", item.span));
                docs.push(text(item.name.clone(), item.name_span));
                docs.push(text(": ", item.span));
                docs.push(self.ty(&item.ty));
                if let Some(align) = &item.align {
                    docs.push(text(" align(", item.span));
                    docs.push(self.expr(align));
                    docs.push(text(")", item.span));
                }
                docs.push(text(" = ", item.span));
                docs.push(self.expr(&item.value));
                docs.push(text(";", end_span(item.span)));
                concat(docs)
            }
            Item::TypeAlias(item) => {
                let mut docs = vec![self.vis(item.public, item.pkg, item.span)];
                docs.push(text("type ", item.span));
                docs.push(text(item.name.clone(), item.name_span));
                docs.push(self.generics(&item.generics, item.span));
                let written = self.source.text[item.name_span.end.min(self.source.text.len())
                    ..item.span.end.min(self.source.text.len())]
                    .contains('=');
                if written {
                    docs.push(text(" = ", item.span));
                    docs.push(self.ty(&item.ty));
                }
                docs.push(text(";", end_span(item.span)));
                concat(docs)
            }
            Item::Extern(item) => self.extern_block(item, depth),
            Item::Test(item) => {
                let keyword = if item.bench { "bench " } else { "test " };
                concat(vec![
                    text(keyword, item.span),
                    text(self.test_name(item), item.span),
                    text(" ", item.span),
                    self.block(&item.body),
                ])
            }
            Item::When(item) => self.when_items(item),
            Item::Poison => nil(),
        }
    }

    fn vis(&self, public: bool, pkg: bool, span: Span) -> Doc {
        if pkg {
            text("pub(pkg) ", span)
        } else if public {
            text("pub ", span)
        } else {
            nil()
        }
    }

    fn function(&mut self, function: &ast::FnDecl, _depth: usize) -> Doc {
        let mut docs = Vec::new();
        if function.mods.public && !function.mods.pkg {
            docs.push(text("pub ", function.span));
        } else if function.mods.pkg {
            docs.push(text("pub(pkg) ", function.span));
        }
        for (on, name) in [
            (function.mods.inline, "inline"),
            (function.mods.noinline, "noinline"),
            (function.mods.unsafe_, "unsafe"),
            (function.mods.export, "export"),
            (function.mods.kernel, "kernel"),
            (function.mods.waitable, "waitable"),
        ] {
            if on {
                docs.push(text(format!("{name} "), function.span));
            }
        }
        docs.push(text("fn ", function.span));
        docs.push(text(function.name.clone(), function.name_span));
        docs.push(self.generics(&function.generics, function.span));
        let params = self.params(&function.params, function.span);
        docs.push(params);
        docs.push(self.result(
            &function.ret,
            function.error.as_ref(),
            &function.from,
            function.reduce.as_deref(),
            function.span,
        ));
        if function.variadic {
            // Only an extern declaration can be variadic; its parameter list already
            // ends in `...`.
        }
        if let Some(body) = &function.body {
            docs.push(text(" ", function.span));
            docs.push(self.block(body));
        } else {
            docs.push(text(";", end_span(function.span)));
        }
        concat(docs)
    }

    fn return_written(&self, ty: &Ty) -> bool {
        let start = ty.span().start.min(self.source.text.len());
        self.source.text[..start].trim_end().ends_with("->")
    }

    fn result(
        &mut self,
        ret: &Ty,
        error: Option<&Ty>,
        from: &[ast::Origin],
        reduce: Option<&str>,
        span: Span,
    ) -> Doc {
        if ret.is_void()
            && !self.return_written(ret)
            && error.is_none()
            && from.is_empty()
            && reduce.is_none()
        {
            return nil();
        }
        let mut docs = vec![text(" -> ", span), self.ty(ret)];
        if let Some(error) = error {
            docs.push(text(" ! ", span));
            docs.push(self.ty(error));
        }
        if !from.is_empty() {
            docs.push(text(" from ", span));
            for (index, origin) in from.iter().enumerate() {
                if index > 0 {
                    docs.push(text(", ", span));
                }
                if origin.is_static {
                    docs.push(text("static", span));
                } else {
                    docs.push(text(origin.name.clone(), origin.span));
                    if origin.inner {
                        docs.push(text(".*", origin.span));
                    }
                }
            }
        }
        if let Some(reduce) = reduce {
            docs.push(text(" reduce(", span));
            docs.push(text(reduce.to_string(), span));
            docs.push(text(")", span));
        }
        concat(docs)
    }

    fn generics(&mut self, generics: &[ast::GenericParam], _span: Span) -> Doc {
        if generics.is_empty() {
            return nil();
        }
        let mut items = Vec::new();
        for generic in generics {
            let mut docs = Vec::new();
            if generic.is_const {
                docs.push(text("const ", generic.span));
            }
            docs.push(text(generic.name.clone(), generic.name_span));
            if let Some(ty) = &generic.const_ty {
                docs.push(text(": ", generic.span));
                docs.push(self.ty(ty));
            }
            if !generic.bounds.is_empty() {
                docs.push(text(": ", generic.span));
                for (index, bound) in generic.bounds.iter().enumerate() {
                    if index > 0 {
                        docs.push(text(" + ", generic.span));
                    }
                    docs.push(self.path_ty(bound));
                }
            }
            items.push((concat(docs), generic.span.start));
        }
        self.list("[", "]", items, false, generics_span(generics))
    }

    fn params(&mut self, params: &[ast::Param], span: Span) -> Doc {
        self.params_variadic(params, false, span)
    }

    fn params_variadic(&mut self, params: &[ast::Param], variadic: bool, span: Span) -> Doc {
        let mut items = Vec::new();
        for param in params {
            let mut docs = vec![
                text(param.name.clone(), param.name_span),
                text(": ", param.span),
            ];
            docs.push(self.ty(&param.ty));
            if let Some((name, span)) = &param.into {
                docs.push(text(" into ", param.span));
                docs.push(text(name.clone(), *span));
            }
            items.push((concat(docs), param.span.start));
        }
        if variadic {
            items.push((text("...", span), usize::MAX));
        }
        let mut force = false;
        let mut previous_end = span.start;
        for param in params {
            if self.comment_count(Span::new(previous_end, param.span.start)) > 0 {
                force = true;
            }
            previous_end = param.span.end;
        }
        self.list("(", ")", items, force, span)
    }

    fn struct_decl(&mut self, item: &ast::StructDecl, _depth: usize) -> Doc {
        let mut docs = vec![self.vis(item.public, item.pkg, item.span)];
        match item.kind {
            ast::StructKind::Copy => docs.push(text("copy ", item.span)),
            ast::StructKind::Linear => docs.push(text("linear ", item.span)),
            ast::StructKind::Plain => {}
        }
        docs.push(text("struct ", item.span));
        docs.push(text(item.name.clone(), item.name_span));
        docs.push(self.generics(&item.generics, item.span));
        if let Some(align) = &item.align {
            docs.push(text(" align(", item.span));
            docs.push(self.expr(align));
            docs.push(text(")", item.span));
        }
        let mut fields = Vec::new();
        for field in &item.fields {
            fields.push((self.field(field), field.span));
        }
        docs.push(text(" ", item.span));
        docs.push(self.decl_block(fields, item.span));
        concat(docs)
    }

    fn field(&mut self, field: &ast::FieldDecl) -> Doc {
        let mut docs = vec![self.vis(field.public, field.pkg, field.span)];
        docs.push(text(field.name.clone(), field.name_span));
        docs.push(text(": ", field.span));
        docs.push(self.ty(&field.ty));
        if let Some(align) = &field.align {
            docs.push(text(" align(", field.span));
            docs.push(self.expr(align));
            docs.push(text(")", field.span));
        }
        concat(docs)
    }

    fn enum_decl(&mut self, item: &ast::EnumDecl) -> Doc {
        let mut docs = vec![self.vis(item.public, item.pkg, item.span)];
        if item.copy {
            docs.push(text("copy ", item.span));
        }
        docs.push(text("enum ", item.span));
        docs.push(text(item.name.clone(), item.name_span));
        docs.push(self.generics(&item.generics, item.span));
        if let Some(tag) = &item.tag {
            docs.push(text(": ", item.span));
            docs.push(self.ty(tag));
        }
        let mut variants = Vec::new();
        for variant in &item.variants {
            let mut docs = vec![text(variant.name.clone(), variant.name_span)];
            if let Some(payload) = &variant.payload {
                docs.push(text(": ", variant.span));
                if variant.from {
                    docs.push(text("from ", variant.span));
                }
                docs.push(self.ty(payload));
            }
            if let Some(value) = &variant.value {
                docs.push(text(" = ", variant.span));
                docs.push(self.expr(value));
            }
            variants.push((concat(docs), variant.span));
        }
        docs.push(text(" ", item.span));
        docs.push(self.decl_block(variants, item.span));
        concat(docs)
    }

    /// The braces of a `struct` or `enum`: one field or variant per line, each ending
    /// in `,`, with the author's zero or one blank line between them.
    fn decl_block(&mut self, items: Vec<(Doc, Span)>, span: Span) -> Doc {
        if items.is_empty() {
            return if self.has_comment(span) {
                self.comment_only_block(span)
            } else {
                text("{}", span)
            };
        }
        let mut inner = vec![hard()];
        let mut previous: Option<Span> = None;
        for (item, item_span) in items {
            if let Some(previous) = previous {
                inner.push(hard());
                if self.blank_lines(previous, item_span) {
                    inner.push(hard());
                }
            }
            previous = Some(item_span);
            inner.push(item);
            inner.push(text(",", self.comma_after(item_span)));
        }
        concat(vec![
            text("{", Span::new(span.start, span.start.saturating_add(1))),
            nest(1, concat(inner)),
            hard(),
            text("}", Span::new(span.end.saturating_sub(1), span.end)),
        ])
    }

    fn interface_decl(&mut self, item: &ast::InterfaceDecl, depth: usize) -> Doc {
        let mut docs = vec![self.vis(item.public, item.pkg, item.span)];
        docs.push(text("interface ", item.span));
        docs.push(text(item.name.clone(), item.name_span));
        docs.push(self.generics(&item.generics, item.span));
        if !item.bounds.is_empty() {
            docs.push(text(": ", item.span));
            for (index, bound) in item.bounds.iter().enumerate() {
                if index > 0 {
                    docs.push(text(" + ", item.span));
                }
                docs.push(self.path_ty(bound));
            }
        }
        docs.push(text(" ", item.span));
        docs.push(self.member_block(&item.members, depth, item.span));
        concat(docs)
    }

    fn impl_decl(&mut self, item: &ast::ImplDecl, depth: usize) -> Doc {
        let mut docs = Vec::new();
        if item.unsafe_ {
            docs.push(text("unsafe ", item.span));
        }
        docs.push(text("impl", item.span));
        docs.push(self.generics(&item.generics, item.span));
        docs.push(text(" ", item.span));
        if let Some(interface) = &item.interface {
            docs.push(self.path_ty(interface));
            docs.push(text(" for ", item.span));
        }
        docs.push(self.ty(&item.target));
        docs.push(text(" ", item.span));
        docs.push(self.member_block(&item.members, depth, item.span));
        concat(docs)
    }

    fn extern_block(&mut self, item: &ast::ExternBlock, _depth: usize) -> Doc {
        let mut docs = vec![self.vis(item.public, item.pkg, item.span)];
        docs.push(text("extern ", item.span));
        docs.push(text(self.span(item.abi_span).to_string(), item.abi_span));
        docs.push(text(" ", item.span));
        let mut members = Vec::new();
        for (index, function) in item.functions.iter().enumerate() {
            let blank =
                index > 0 && self.blank_lines(item.functions[index - 1].span, function.span);
            let mut docs = vec![text("fn ", function.span)];
            docs.push(text(function.name.clone(), function.name_span));
            docs.push(self.params_variadic(&function.params, function.variadic, function.span));
            docs.push(self.result(
                &function.ret,
                function.error.as_ref(),
                &function.from,
                function.reduce.as_deref(),
                function.span,
            ));
            docs.push(text(";", end_span(function.span)));
            members.push((concat(docs), blank));
        }
        docs.push(self.doc_block(members, item.span));
        concat(docs)
    }

    fn when_items(&mut self, item: &ast::WhenItems) -> Doc {
        let mut docs = vec![
            text("when ", item.span),
            self.expr(&item.condition),
            text(" ", item.span),
        ];
        let (open, close) = self.brace_pair(item.condition.span.end);
        docs.push(self.item_block(&item.then, open, close));
        if !item.otherwise.is_empty() {
            docs.push(text(" else ", item.span));
            if item.otherwise.len() == 1
                && let ast::Item::When(nested) = &item.otherwise[0]
            {
                docs.push(self.when_items(nested));
            } else {
                let (open, close) = self.brace_pair(close.end);
                docs.push(self.item_block(&item.otherwise, open, close));
            }
        }
        concat(docs)
    }

    /// The first `{` at or after `from`, and the `}` that closes it.
    fn brace_pair(&self, from: usize) -> (Span, Span) {
        let first = self.tokens.partition_point(|token| token.span.start < from);
        let mut open = None;
        let mut depth = 0usize;
        for token in &self.tokens[first..] {
            match self.span(token.span) {
                "{" | ".{" => {
                    if open.is_none() {
                        open = Some(token.span);
                    }
                    depth += 1;
                }
                "}" if open.is_some() => {
                    depth -= 1;
                    if depth == 0 {
                        return (open.unwrap_or_default(), token.span);
                    }
                }
                _ => {}
            }
        }
        let at = Span::new(from, from);
        (open.unwrap_or(at), at)
    }

    fn item_block(&mut self, items: &[Item], open: Span, close: Span) -> Doc {
        if items.is_empty() {
            return if self.has_comment(Span::new(open.start, close.end)) {
                self.comment_only_block(Span::new(open.start, close.end))
            } else {
                text("{}", open)
            };
        }
        let mut inner = vec![hard()];
        for (index, item) in items.iter().enumerate() {
            if index > 0 {
                inner.push(hard());
                if self.blank_lines(items[index - 1].span(), item.span()) {
                    inner.push(hard());
                }
            }
            inner.push(self.item(item, 1));
        }
        concat(vec![
            text("{", open),
            nest(1, concat(inner)),
            hard(),
            text("}", close),
        ])
    }

    /// `{` and `}` around nothing but comments, which are placed between them.
    fn comment_only_block(&self, span: Span) -> Doc {
        concat(vec![
            text("{", Span::new(span.start, span.start.saturating_add(1))),
            nest(1, hard()),
            text("}", Span::new(span.end.saturating_sub(1), span.end)),
        ])
    }

    /// Members with whether a blank line precedes each.
    fn doc_block(&mut self, members: Vec<(Doc, bool)>, span: Span) -> Doc {
        if members.is_empty() {
            return if self.has_comment(span) {
                self.comment_only_block(span)
            } else {
                text("{}", span)
            };
        }
        let mut inner = vec![hard()];
        for (index, (member, blank)) in members.into_iter().enumerate() {
            if index > 0 {
                inner.push(hard());
                if blank {
                    inner.push(hard());
                }
            }
            inner.push(member);
        }
        concat(vec![
            text("{", Span::new(span.start, span.start.saturating_add(1))),
            nest(1, concat(inner)),
            hard(),
            text("}", Span::new(span.end.saturating_sub(1), span.end)),
        ])
    }

    fn member_block(&mut self, members: &[Item], _depth: usize, span: Span) -> Doc {
        if members.is_empty() {
            return if self.has_comment(span) {
                self.comment_only_block(span)
            } else {
                text("{}", span)
            };
        }
        let mut inner = vec![hard()];
        for (index, member) in members.iter().enumerate() {
            if index > 0 {
                inner.push(hard());
                if self.blank_lines(members[index - 1].span(), member.span()) {
                    inner.push(hard());
                }
            }
            inner.push(self.item(member, 1));
        }
        concat(vec![
            text("{", Span::new(span.start, span.start.saturating_add(1))),
            nest(1, concat(inner)),
            hard(),
            text("}", Span::new(span.end.saturating_sub(1), span.end)),
        ])
    }

    fn block(&mut self, block: &ast::Block) -> Doc {
        self.block_inner(block, false)
    }

    fn block_branch(&mut self, block: &ast::Block) -> Doc {
        self.block_inner(block, true)
    }

    fn block_inner(&mut self, block: &ast::Block, bare_last: bool) -> Doc {
        if block.stmts.is_empty() {
            return if self.has_comment(block.span) {
                self.comment_only_block(block.span)
            } else {
                text("{}", block.span)
            };
        }
        let mut inner = vec![hard()];
        let last = block.stmts.len().saturating_sub(1);
        for (index, stmt) in block.stmts.iter().enumerate() {
            if index > 0 {
                inner.push(hard());
                if self.blank_lines(block.stmts[index - 1].span(), stmt.span()) {
                    inner.push(hard());
                }
            }
            if bare_last
                && index == last
                && let Stmt::Expr { expr, .. } = stmt
            {
                inner.push(self.expr(expr));
            } else {
                inner.push(self.stmt(stmt));
            }
        }
        concat(vec![
            text(
                "{",
                Span::new(block.span.start, block.span.start.saturating_add(1)),
            ),
            nest(1, concat(inner)),
            hard(),
            text(
                "}",
                Span::new(block.span.end.saturating_sub(1), block.span.end),
            ),
        ])
    }

    /// The block of `defer`, `errdefer`, `unsafe`, an `orelse` or `catch` handler, a
    /// `match` or `select` arm or a closure: `{ stmt }` when it holds one statement without a block of its own
    /// and no comment, and the line fits; several lines otherwise.
    fn one_line_block(&mut self, block: &ast::Block) -> Doc {
        let broken = self.block(block);
        let [stmt] = block.stmts.as_slice() else {
            return broken;
        };
        if self.has_comment(block.span) || self.stmt_has_block(stmt) {
            return broken;
        }
        let flat = concat(vec![
            text(
                "{ ",
                Span::new(block.span.start, block.span.start.saturating_add(1)),
            ),
            self.stmt(stmt),
            text(
                " }",
                Span::new(block.span.end.saturating_sub(1), block.span.end),
            ),
        ]);
        last_choice(flat, broken, false)
    }

    fn char_span(&self, span: Span, character: char, last: bool) -> Span {
        let text = self.source.text.get(span.start..span.end).unwrap_or("");
        let found = if last {
            text.rfind(character).map(|index| span.start + index)
        } else {
            text.find(character).map(|index| span.start + index)
        };
        found
            .map(|start| Span::new(start, start + character.len_utf8()))
            .unwrap_or(span)
    }

    fn list(
        &mut self,
        open: &str,
        close: &str,
        items: Vec<(Doc, usize)>,
        force: bool,
        span: Span,
    ) -> Doc {
        self.list_with(open, close, "", "", items, force, span)
    }

    fn list_spaced(
        &mut self,
        open: &str,
        close: &str,
        items: Vec<(Doc, usize)>,
        force: bool,
        span: Span,
    ) -> Doc {
        self.list_with(open, close, " ", " ", items, force, span)
    }

    /// A list that is written flat when it fits and one item per line otherwise. Each
    /// item comes with the offset where it starts, to find the comma written after it.
    #[allow(clippy::too_many_arguments)]
    fn list_with(
        &mut self,
        open: &str,
        close: &str,
        after_open: &str,
        before_close: &str,
        items: Vec<(Doc, usize)>,
        force: bool,
        span: Span,
    ) -> Doc {
        if items.is_empty() {
            return concat(vec![text(open, span), text(close, span)]);
        }
        let open_span = open
            .chars()
            .next()
            .map(|character| self.char_span(span, character, false))
            .unwrap_or(span);
        let close_span = close
            .chars()
            .next()
            .map(|character| self.char_span(span, character, true))
            .unwrap_or(span);
        let mut flat = vec![text(format!("{open}{after_open}"), open_span)];
        let mut broken_items = vec![hard()];
        for (index, (item, start)) in items.into_iter().enumerate() {
            let comma = self.comma_from(start, span);
            if index > 0 {
                flat.push(text(", ", self.previous_comma(start, span)));
            }
            flat.push(item.clone());
            broken_items.push(item);
            broken_items.push(text(",", comma));
            broken_items.push(hard());
        }
        flat.push(text(format!("{before_close}{close}"), close_span));
        let broken = concat(vec![
            text(open, open_span),
            nest(1, concat(broken_items)),
            text(close, close_span),
        ]);
        last_choice(concat(flat), broken, force)
    }

    /// The comma that ends the item starting at `start`: the first `,` at the item's
    /// own bracket depth. Without one, an empty span at the start of the list.
    fn comma_from(&self, start: usize, list: Span) -> Span {
        if start == usize::MAX {
            return Span::new(list.start, list.start);
        }
        let mut depth = 0usize;
        let first = self
            .tokens
            .partition_point(|token| token.span.start < start);
        for token in &self.tokens[first..] {
            match self.span(token.span) {
                "(" | "[" | "{" | ".{" | ".[" => depth += 1,
                ")" | "]" | "}" => {
                    if depth == 0 {
                        break;
                    }
                    depth -= 1;
                }
                "," if depth == 0 => return token.span,
                ";" if depth == 0 => break,
                _ => {}
            }
        }
        Span::new(list.start, list.start)
    }

    /// The comma that precedes the item starting at `start`, written between two items
    /// of a flat list.
    fn previous_comma(&self, start: usize, list: Span) -> Span {
        match self
            .tokens
            .iter()
            .rev()
            .find(|token| token.span.end <= start)
        {
            Some(token) if self.span(token.span) == "," => token.span,
            _ => Span::new(list.start, list.start),
        }
    }
}

fn generic_arg_start(arg: &ast::GenericArg) -> usize {
    match arg {
        ast::GenericArg::Ty(ty) => ty.span().start,
        ast::GenericArg::Const(expr) => expr.span.start,
    }
}

fn expr_starts_borrow(expr: &Expr) -> bool {
    matches!(
        &expr.kind,
        ExprKind::Unary {
            op: UnOp::Borrow,
            ..
        }
    )
}

fn generics_span(generics: &[ast::GenericParam]) -> Span {
    match (generics.first(), generics.last()) {
        (Some(first), Some(last)) => Span::new(first.span.start, last.span.end),
        _ => Span::default(),
    }
}

// ---------------------------------------------------------------- types

impl Formatter<'_> {
    fn path_ty(&mut self, path: &ast::PathTy) -> Doc {
        let mut docs = Vec::new();
        for (index, (segment, span)) in path.segments.iter().enumerate() {
            if index > 0 {
                docs.push(text(".", *span));
            }
            docs.push(text(segment.clone(), *span));
        }
        if let Some(signature) = &path.fn_sig {
            docs.push(text("(", signature.span));
            for (index, param) in signature.params.iter().enumerate() {
                if index > 0 {
                    docs.push(text(", ", signature.span));
                }
                docs.push(self.ty(param));
            }
            docs.push(text(")", signature.span));
            if let Some(ret) = &signature.ret {
                docs.push(text(" -> ", signature.span));
                docs.push(self.ty(ret));
            }
            if let Some(error) = &signature.error {
                docs.push(text(" ! ", signature.span));
                docs.push(self.ty(error));
            }
        } else if !path.args.is_empty() {
            let mut args = Vec::new();
            for arg in &path.args {
                args.push((self.generic_arg(arg), generic_arg_start(arg)));
            }
            let args_span = path.args_span.unwrap_or(path.span);
            docs.push(self.list("[", "]", args, self.has_comment(args_span), args_span));
        }
        if let Some((name, span)) = &path.assoc {
            docs.push(text(".", *span));
            docs.push(text(name.clone(), *span));
        }
        concat(docs)
    }

    fn generic_arg(&mut self, arg: &ast::GenericArg) -> Doc {
        match arg {
            ast::GenericArg::Ty(ty) => self.ty(ty),
            ast::GenericArg::Const(expr) => {
                if let ExprKind::Paren(inner) = &expr.kind
                    && self.source.text[expr.span.start..expr.span.end].starts_with('{')
                {
                    return concat(vec![
                        text("{ ", expr.span),
                        self.expr(inner),
                        text(" }", expr.span),
                    ]);
                }
                self.expr(expr)
            }
        }
    }

    fn ty(&mut self, ty: &Ty) -> Doc {
        if let Some(slice) = self.source.text.get(ty.span().start..ty.span().end)
            && matches!(ty, Ty::Scalar(..) | Ty::Vector(..))
        {
            return text(slice.trim(), ty.span());
        }
        match ty {
            Ty::Scalar(_, span) | Ty::Vector(_, span) | Ty::Poison(span) => text("", *span),
            Ty::Path(path) => self.path_ty(path),
            Ty::Borrow {
                mutable,
                inner,
                span,
            } => {
                let prefix = if *mutable { "&mut " } else { "&" };
                let space = if !*mutable && matches!(inner.as_ref(), Ty::Borrow { .. }) {
                    " "
                } else {
                    ""
                };
                concat(vec![
                    text(format!("{prefix}{space}"), *span),
                    self.ty(inner),
                ])
            }
            Ty::Slice {
                mutable,
                elem,
                span,
            } => concat(vec![
                text(if *mutable { "[]mut " } else { "[]" }, *span),
                self.ty(elem),
            ]),
            Ty::Array { len, elem, span } => concat(vec![
                text("[", *span),
                self.expr(len),
                text("]", *span),
                self.ty(elem),
            ]),
            Ty::Ptr {
                mutable,
                inner,
                span,
            } => concat(vec![
                text(if *mutable { "*mut " } else { "*" }, *span),
                self.ty(inner),
            ]),
            Ty::Optional { inner, span } => concat(vec![text("?", *span), self.ty(inner)]),
            Ty::Tuple { elems, span } => {
                let items = elems
                    .iter()
                    .map(|ty| (self.ty(ty), ty.span().start))
                    .collect();
                self.list("(", ")", items, self.has_comment(*span), *span)
            }
            Ty::Fn {
                ext,
                params,
                ret,
                error,
                span,
            } => {
                let mut docs = vec![text(if *ext { "extern fn" } else { "fn" }, *span)];
                let params: Vec<(Doc, usize)> = params
                    .iter()
                    .map(|ty| (self.ty(ty), ty.span().start))
                    .collect();
                docs.push(self.list("(", ")", params, self.has_comment(*span), *span));
                if !ret.is_void() || self.return_written(ret) || error.is_some() {
                    docs.push(text(" -> ", *span));
                    docs.push(self.ty(ret));
                }
                if let Some(error) = error {
                    docs.push(text(" ! ", *span));
                    docs.push(self.ty(error));
                }
                concat(docs)
            }
            Ty::Dyn {
                interface,
                mutable,
                span,
            } => concat(vec![
                text(if *mutable { "&mut dyn " } else { "&dyn " }, *span),
                self.path_ty(interface),
            ]),
        }
    }
}

// ---------------------------------------------------------------- statements

impl Formatter<'_> {
    fn stmt(&mut self, stmt: &Stmt) -> Doc {
        match stmt {
            Stmt::Let {
                mutable,
                pattern,
                ty,
                init,
                span,
            } => {
                let mut docs = vec![text(if *mutable { "var " } else { "let " }, *span)];
                docs.push(self.pattern(pattern));
                if let Some(ty) = ty {
                    docs.push(text(": ", *span));
                    docs.push(self.ty(ty));
                }
                docs.push(text(" = ", *span));
                docs.push(self.expr(init));
                docs.push(text(";", end_span(*span)));
                concat(docs)
            }
            Stmt::Assign {
                place,
                op,
                value,
                span,
            } => concat(vec![
                self.expr(place),
                text(
                    format!(
                        " {} ",
                        if *op == ast::AssignOp::Assign {
                            "=".to_string()
                        } else {
                            format!("{}=", op.symbol())
                        }
                    ),
                    *span,
                ),
                self.expr(value),
                text(";", end_span(*span)),
            ]),
            Stmt::Expr { expr, .. } => {
                let span = stmt.span();
                concat(vec![self.expr(expr), text(";", end_span(span))])
            }
            Stmt::Return { value, span } => {
                let mut docs = vec![text("return", *span)];
                if let Some(value) = value {
                    docs.push(text(" ", *span));
                    docs.push(self.expr(value));
                }
                docs.push(text(";", end_span(*span)));
                concat(docs)
            }
            Stmt::Fail { value, span } => concat(vec![
                text("fail ", *span),
                self.expr(value),
                text(";", end_span(*span)),
            ]),
            Stmt::Break { label, span } => {
                let mut docs = vec![text("break", *span)];
                if let Some((name, label_span)) = label {
                    docs.push(text(" ", *span));
                    docs.push(text(name.clone(), *label_span));
                }
                docs.push(text(";", end_span(*span)));
                concat(docs)
            }
            Stmt::Continue { label, span } => {
                let mut docs = vec![text("continue", *span)];
                if let Some((name, label_span)) = label {
                    docs.push(text(" ", *span));
                    docs.push(text(name.clone(), *label_span));
                }
                docs.push(text(";", end_span(*span)));
                concat(docs)
            }
            Stmt::Defer { err, body, span } => concat(vec![
                text(if *err { "errdefer " } else { "defer " }, *span),
                self.one_line_block(body),
            ]),
            Stmt::Block(block) => self.block(block),
            Stmt::If(item) => self.if_stmt(item),
            Stmt::While {
                label,
                cond,
                body,
                span,
            } => {
                let mut docs = self.label(label, *span);
                docs.push(text("while ", *span));
                docs.push(self.cond(cond));
                docs.push(text(" ", *span));
                docs.push(self.block(body));
                concat(docs)
            }
            Stmt::Loop { label, body, span } => {
                let mut docs = self.label(label, *span);
                docs.push(text("loop ", *span));
                docs.push(self.block(body));
                concat(docs)
            }
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
                span,
            } => self.for_stmt(
                *par, grain, label, vars, index, iter, range_end, step, body, *span,
            ),
            Stmt::Match(item) => self.match_stmt(item),
            Stmt::Scope {
                name,
                name_span,
                body,
                span,
            } => concat(vec![
                text("scope ", *span),
                text(name.clone(), *name_span),
                text(" ", *span),
                self.block(body),
            ]),
            Stmt::Spawn { call, scope, span } => concat(vec![
                text("spawn ", *span),
                self.call_expr(call),
                text(" in ", *span),
                self.expr(scope),
                text(";", end_span(*span)),
            ]),
            Stmt::Dispatch {
                call,
                domain,
                executor,
                span,
            } => concat(vec![
                text("dispatch ", *span),
                self.call_expr(call),
                text(" over ", *span),
                self.domain(domain),
                text(" on ", *span),
                self.expr(executor),
                text(";", end_span(*span)),
            ]),
            Stmt::Select { arms, span } => self.select_stmt(arms, *span),
            Stmt::Unsafe {
                reason_span,
                body,
                span,
                ..
            } => concat(vec![
                text("unsafe(", *span),
                text(self.span(*reason_span).to_string(), *reason_span),
                text(") ", *span),
                self.one_line_block(body),
            ]),
            Stmt::When {
                condition,
                then,
                otherwise,
                span,
            } => {
                let mut docs = vec![
                    text("when ", *span),
                    self.expr(condition),
                    text(" ", *span),
                    self.block(then),
                ];
                if !otherwise.is_empty() {
                    docs.push(text(" else ", *span));
                    if otherwise.len() == 1
                        && let Stmt::When {
                            condition,
                            then,
                            otherwise,
                            span,
                        } = &otherwise[0]
                    {
                        docs.push(self.when_stmt_else(condition, then, otherwise, *span));
                    } else {
                        docs.push(self.else_block(otherwise, *span));
                    }
                }
                concat(docs)
            }
            Stmt::Poison => nil(),
        }
    }

    fn when_stmt_else(
        &mut self,
        condition: &Expr,
        then: &ast::Block,
        otherwise: &[Stmt],
        span: Span,
    ) -> Doc {
        let mut docs = vec![
            text("when ", span),
            self.expr(condition),
            text(" ", span),
            self.block(then),
        ];
        if !otherwise.is_empty() {
            docs.push(text(" else ", span));
            if otherwise.len() == 1
                && let Stmt::When {
                    condition,
                    then,
                    otherwise,
                    span,
                } = &otherwise[0]
            {
                docs.push(self.when_stmt_else(condition, then, otherwise, *span));
            } else {
                docs.push(self.else_block(otherwise, span));
            }
        }
        concat(docs)
    }

    fn else_block(&mut self, otherwise: &[Stmt], span: Span) -> Doc {
        if otherwise.len() == 1
            && let Stmt::Block(block) = &otherwise[0]
        {
            return self.block(block);
        }
        let block = ast::Block {
            stmts: otherwise.to_vec(),
            span,
        };
        self.block(&block)
    }

    fn label(&self, label: &Option<(String, Span)>, span: Span) -> Vec<Doc> {
        match label {
            Some((name, label_span)) => vec![text(name.clone(), *label_span), text(": ", span)],
            None => Vec::new(),
        }
    }

    fn cond(&mut self, cond: &ast::Cond) -> Doc {
        if let Some(pattern) = &cond.let_pattern {
            concat(vec![
                text("let ", cond.span),
                self.pattern(pattern),
                text(" = ", cond.span),
                self.expr(&cond.expr),
            ])
        } else {
            self.expr(&cond.expr)
        }
    }

    fn if_stmt(&mut self, item: &ast::IfStmt) -> Doc {
        let mut docs = vec![
            text("if ", item.span),
            self.cond(&item.cond),
            text(" ", item.span),
            self.block(&item.then),
        ];
        if let Some(otherwise) = &item.otherwise {
            docs.push(text(" else ", item.span));
            match otherwise.as_ref() {
                Stmt::If(nested) => docs.push(self.if_stmt(nested)),
                stmt => docs.push(self.block_as_else(stmt)),
            }
        }
        concat(docs)
    }

    fn block_as_else(&mut self, stmt: &Stmt) -> Doc {
        match stmt {
            Stmt::Block(block) => self.block(block),
            _ => {
                let block = ast::Block {
                    stmts: vec![stmt.clone()],
                    span: stmt.span(),
                };
                self.block(&block)
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn for_stmt(
        &mut self,
        par: bool,
        grain: &Option<Expr>,
        label: &Option<(String, Span)>,
        vars: &[(String, Span)],
        index: &Option<(String, Span)>,
        iter: &Expr,
        range_end: &Option<(bool, Expr)>,
        step: &Option<Expr>,
        body: &ast::Block,
        span: Span,
    ) -> Doc {
        let mut head = self.label(label, span);
        if par {
            head.push(text("par ", span));
            if let Some(grain) = grain {
                head.push(text("(grain: ", span));
                head.push(self.expr(grain));
                head.push(text(") ", span));
            }
        }
        head.push(text("for ", span));
        if let Some((name, name_span)) = index {
            head.push(text(name.clone(), *name_span));
            head.push(text(", ", span));
        }
        for (index, (name, name_span)) in vars.iter().enumerate() {
            if index > 0 {
                head.push(text(", ", span));
            }
            head.push(text(name.clone(), *name_span));
        }
        head.push(text(" in ", span));
        head.push(self.expr(iter));
        if let Some((inclusive, end)) = range_end {
            head.push(text(if *inclusive { "..=" } else { ".." }, span));
            head.push(self.expr(end));
        }
        if let Some(step) = step {
            head.push(text(" step ", span));
            head.push(self.expr(step));
        }
        head.push(text(" ", span));
        head.push(self.block(body));
        concat(head)
    }

    fn match_stmt(&mut self, item: &ast::MatchStmt) -> Doc {
        let mut docs = vec![
            text("match ", item.span),
            self.expr(&item.scrutinee),
            text(" ", item.span),
        ];
        docs.push(self.arms(&item.arms, item.span));
        concat(docs)
    }

    fn arms(&mut self, arms: &[ast::Arm], span: Span) -> Doc {
        if arms.is_empty() {
            return text("{}", span);
        }
        let mut inner = vec![hard()];
        for (index, arm) in arms.iter().enumerate() {
            if index > 0 {
                inner.push(hard());
                if self.blank_lines(arms[index - 1].span, arm.span) {
                    inner.push(hard());
                }
            }
            inner.push(self.arm(arm));
        }
        concat(vec![
            text("{", Span::new(span.start, span.start.saturating_add(1))),
            nest(1, concat(inner)),
            hard(),
            text("}", Span::new(span.end.saturating_sub(1), span.end)),
        ])
    }

    fn arm(&mut self, arm: &ast::Arm) -> Doc {
        let mut docs = Vec::new();
        if arm.is_else {
            docs.push(text("else", arm.span));
        } else {
            for (index, pattern) in arm.patterns.iter().enumerate() {
                if index > 0 {
                    docs.push(text(" | ", arm.span));
                }
                docs.push(self.pattern(pattern));
            }
        }
        if let Some(guard) = &arm.guard {
            docs.push(text(" if ", arm.span));
            docs.push(self.expr(guard));
        }
        docs.push(text(" => ", arm.span));
        match &arm.body {
            ArmBody::Block(block) => docs.push(self.one_line_block(block)),
            ArmBody::Expr(expr) if matches!(&expr.kind, ExprKind::Block(_)) => {
                if let ExprKind::Block(block) = &expr.kind {
                    docs.push(self.one_line_block(block));
                }
            }
            ArmBody::Expr(expr) => {
                docs.push(self.expr(expr));
                docs.push(text(",", arm.span));
            }
        }
        concat(docs)
    }

    fn select_stmt(&mut self, arms: &[ast::SelectArm], span: Span) -> Doc {
        if arms.is_empty() {
            return text("select {}", span);
        }
        let mut inner = vec![hard()];
        for (index, arm) in arms.iter().enumerate() {
            if index > 0 {
                inner.push(hard());
                if self.blank_lines(arms[index - 1].span, arm.span) {
                    inner.push(hard());
                }
            }
            let mut line = Vec::new();
            if arm.is_else {
                line.push(text("else", arm.span));
            } else {
                line.push(self.pattern(&arm.pattern));
                line.push(text(" = ", arm.span));
                line.push(self.call_expr(&arm.call));
            }
            line.push(text(" => ", arm.span));
            line.push(self.one_line_block(&arm.body));
            inner.push(concat(line));
        }
        concat(vec![
            text("select {", span),
            nest(1, concat(inner)),
            hard(),
            text("}", Span::new(span.end.saturating_sub(1), span.end)),
        ])
    }

    fn domain(&mut self, expr: &Expr) -> Doc {
        if let ExprKind::Tuple { elems, .. } = &expr.kind {
            let mut docs = Vec::new();
            for (index, elem) in elems.iter().enumerate() {
                if index > 0 {
                    docs.push(text(", ", expr.span));
                }
                docs.push(self.expr(elem));
            }
            return concat(docs);
        }
        self.expr(expr)
    }

    fn call_expr(&mut self, expr: &Expr) -> Doc {
        self.expr(expr)
    }
}

// ---------------------------------------------------------------- expressions

impl Formatter<'_> {
    fn expr(&mut self, expr: &Expr) -> Doc {
        if let Some(doc) = self.method_chain(expr) {
            return doc;
        }
        match &expr.kind {
            ExprKind::Int { .. } | ExprKind::Float(_) | ExprKind::Str(_) => {
                text(self.span(expr.span).to_string(), expr.span)
            }
            ExprKind::CStr(_) => {
                let mut literal = self.span(expr.span).to_string();
                if !literal.starts_with('c') {
                    literal.insert(0, 'c');
                }
                text(literal, expr.span)
            }
            ExprKind::Bool(value) => text(if *value { "true" } else { "false" }, expr.span),
            ExprKind::Ident(name) => text(name.clone(), expr.span),
            ExprKind::Path(path) => self.path_ty(path),
            ExprKind::Block(block) => self.block(block),
            ExprKind::Builtin {
                name,
                type_arg,
                args,
            } => {
                let mut docs = vec![text(format!("@{name}"), expr.span)];
                let written_parens = self.source.text[expr.span.start..expr.span.end].contains('(');
                if type_arg.is_some() || !args.is_empty() || written_parens {
                    docs.push(text("(", expr.span));
                    let mut items = Vec::new();
                    if let Some(ty) = type_arg {
                        items.push((self.ty(ty), ty.span().start));
                    }
                    for arg in args {
                        items.push((self.expr(arg), arg.span.start));
                    }
                    docs.push(self.list("", "", items, self.has_comment(expr.span), expr.span));
                    docs.push(text(")", expr.span));
                }
                concat(docs)
            }
            ExprKind::Unary {
                op, expr: inner, ..
            } => {
                let prefix = match op {
                    UnOp::Neg => "-",
                    UnOp::NegWrap => "-%",
                    UnOp::Not => "!",
                    UnOp::BitNot => "~",
                    UnOp::Borrow => "&",
                    UnOp::BorrowMut => "&mut ",
                    UnOp::Try => "try ",
                };
                let space = if matches!(op, UnOp::Borrow) && expr_starts_borrow(inner) {
                    " "
                } else {
                    ""
                };
                concat(vec![
                    text(format!("{prefix}{space}"), expr.span),
                    self.expr(inner),
                ])
            }
            ExprKind::Binary { .. } => self.binary(expr),
            ExprKind::Call {
                callee,
                args,
                args_span,
            } => {
                let mut docs = vec![self.expr(callee)];
                let items = args
                    .iter()
                    .map(|arg| (self.expr(arg), arg.span.start))
                    .collect();
                docs.push(self.list("(", ")", items, self.has_comment(*args_span), *args_span));
                concat(docs)
            }
            ExprKind::Index {
                base,
                index,
                bracket,
            } => concat(vec![
                self.expr(base),
                text("[", *bracket),
                self.expr(index),
                text("]", *bracket),
            ]),
            ExprKind::Slice {
                base,
                lo,
                hi,
                bracket,
            } => {
                let mut docs = vec![self.expr(base), text("[", *bracket)];
                if let Some(lo) = lo {
                    docs.push(self.expr(lo));
                }
                docs.push(text("..", *bracket));
                if let Some(hi) = hi {
                    docs.push(self.expr(hi));
                }
                docs.push(text("]", *bracket));
                concat(docs)
            }
            ExprKind::Field {
                base,
                name,
                name_span,
            } => concat(vec![
                self.expr(base),
                text(".", *name_span),
                text(name.clone(), *name_span),
            ]),
            ExprKind::TupleField { base, index, span } => concat(vec![
                self.expr(base),
                text(".", *span),
                text(index.to_string(), *span),
            ]),
            ExprKind::Deref { base, op_span } => {
                concat(vec![self.expr(base), text(".*", *op_span)])
            }
            ExprKind::TypeApply { base, args, span } => {
                let items = args
                    .iter()
                    .map(|arg| (self.generic_arg(arg), generic_arg_start(arg)))
                    .collect();
                concat(vec![
                    self.expr(base),
                    text(".[", *span),
                    self.list("", "", items, self.has_comment(*span), *span),
                    text("]", *span),
                ])
            }
            ExprKind::Cast {
                expr: inner,
                ty,
                as_span,
            } => concat(vec![self.expr(inner), text(" as ", *as_span), self.ty(ty)]),
            ExprKind::StructLit { ty, fields, braces } => {
                let mut docs = vec![self.path_ty(ty), text(".{", *braces)];
                let items = fields
                    .iter()
                    .map(|field| (self.field_init(field), field.name_span.start))
                    .collect();
                docs.push(self.list_spaced("", "}", items, self.has_comment(*braces), *braces));
                if docs.len() == 2 {
                    docs.pop();
                    docs.push(text("{}", *braces));
                }
                concat(docs)
            }
            ExprKind::StructShorthand { fields, braces } => {
                let mut docs = vec![text(".{", *braces)];
                let items = fields
                    .iter()
                    .map(|field| (self.field_init(field), field.name_span.start))
                    .collect();
                docs.push(self.list_spaced("", "}", items, self.has_comment(*braces), *braces));
                if docs.len() == 1 {
                    docs.push(text("}", *braces));
                }
                concat(docs)
            }
            ExprKind::EnumLit {
                path,
                variant,
                variant_span,
                payload,
            } => {
                let mut docs = Vec::new();
                if let Some(path) = path {
                    docs.push(self.path_ty(path));
                    docs.push(text(".", *variant_span));
                } else {
                    docs.push(text(".", *variant_span));
                }
                docs.push(text(variant.clone(), *variant_span));
                if let Some(payload) = payload {
                    docs.push(text("(", *variant_span));
                    docs.push(self.expr(payload));
                    docs.push(text(")", *variant_span));
                }
                concat(docs)
            }
            ExprKind::ArrayLit {
                len,
                elem,
                elems,
                span,
            } => {
                let mut docs = vec![
                    text("[", *span),
                    self.expr(len),
                    text("]", *span),
                    self.ty(elem),
                    text(".{", *span),
                ];
                let items = elems
                    .iter()
                    .map(|item| (self.expr(item), item.span.start))
                    .collect();
                docs.push(self.list_spaced("", "}", items, self.has_comment(*span), *span));
                concat(docs)
            }
            ExprKind::Range {
                lo,
                hi,
                inclusive,
                span,
            } => concat(vec![
                self.expr(lo),
                text(if *inclusive { "..=" } else { ".." }, *span),
                self.expr(hi),
            ]),
            ExprKind::Tuple { elems, span } => {
                let items = elems
                    .iter()
                    .map(|item| (self.expr(item), item.span.start))
                    .collect();
                self.list("(", ")", items, self.has_comment(*span), *span)
            }
            ExprKind::Paren(inner) => concat(vec![
                text("(", expr.span),
                self.expr(inner),
                text(")", expr.span),
            ]),
            ExprKind::If {
                cond,
                then,
                otherwise,
                span,
            } => self.if_expr(cond, then, otherwise, *span),
            ExprKind::Match {
                scrutinee,
                arms,
                span,
            } => concat(vec![
                text("match ", *span),
                self.expr(scrutinee),
                text(" ", *span),
                self.arms(arms, *span),
            ]),
            ExprKind::Closure {
                captures,
                params,
                ret,
                error,
                body,
                span,
            } => {
                let mut docs = vec![text("fn [", *span)];
                for (index, capture) in captures.iter().enumerate() {
                    if index > 0 {
                        docs.push(text(", ", *span));
                    }
                    if capture.shared {
                        docs.push(text("&", capture.span));
                    }
                    if capture.mutable {
                        docs.push(text("mut ", capture.span));
                    }
                    docs.push(text(capture.name.clone(), capture.span));
                }
                docs.push(text("]", *span));
                docs.push(self.params(params, *span));
                docs.push(self.result(ret, error.as_ref(), &[], None, *span));
                docs.push(text(" ", *span));
                docs.push(self.one_line_block(body));
                concat(docs)
            }
            ExprKind::Catch {
                value,
                binding,
                handler,
                catch_span,
                ..
            } => {
                let mut docs = vec![self.expr(value), text(" catch ", *catch_span)];
                if let Some((name, span)) = binding {
                    docs.push(text(name.clone(), *span));
                } else {
                    docs.push(text("_", *catch_span));
                }
                docs.push(text(" => ", *catch_span));
                docs.push(self.handler(handler, *catch_span));
                concat(docs)
            }
            ExprKind::Orelse {
                value,
                handler,
                orelse_span,
            } => concat(vec![
                self.expr(value),
                text(" orelse ", *orelse_span),
                self.handler(handler, *orelse_span),
            ]),
            ExprKind::Spawn { call, scope, span } => concat(vec![
                text("spawn ", *span),
                self.expr(call),
                text(" in ", *span),
                self.expr(scope),
            ]),
            ExprKind::Dispatch {
                call,
                domain,
                executor,
                span,
            } => concat(vec![
                text("dispatch ", *span),
                self.expr(call),
                text(" over ", *span),
                self.domain(domain),
                text(" on ", *span),
                self.expr(executor),
            ]),
            ExprKind::Unsafe {
                reason_span,
                body,
                span,
                ..
            } => concat(vec![
                text("unsafe(", *span),
                text(self.span(*reason_span).to_string(), *reason_span),
                text(") ", *span),
                self.branch_expr(body),
            ]),
            ExprKind::Poison => nil(),
        }
    }

    fn method_chain(&mut self, expr: &Expr) -> Option<Doc> {
        let (base, segments) = decompose_chain(expr)?;
        if segments
            .iter()
            .filter(|segment| matches!(segment, ChainSegment::Call(..)))
            .count()
            < 2
        {
            return None;
        }
        let first_is_field = matches!(
            segments.first(),
            Some(ChainSegment::Field(..) | ChainSegment::TupleField(..))
        );
        let mut flat = vec![self.expr(base)];
        let mut broken = vec![self.expr(base)];
        let mut rest = Vec::new();
        for (index, segment) in segments.iter().enumerate() {
            let segment_doc = self.chain_segment(segment, expr.span);
            flat.push(segment_doc.clone());
            let break_before = matches!(
                segment,
                ChainSegment::Field(..) | ChainSegment::TupleField(..)
            ) && (index > 0 || first_is_field);
            if break_before {
                rest.push(hard());
                rest.push(segment_doc);
            } else {
                rest.push(segment_doc);
            }
        }
        broken.push(nest(1, concat(rest)));
        Some(choice(concat(flat), concat(broken), false))
    }

    fn chain_segment(&mut self, segment: &ChainSegment<'_>, _span: Span) -> Doc {
        match segment {
            ChainSegment::Field(name, name_span) => concat(vec![
                text(".", *name_span),
                text((*name).to_string(), *name_span),
            ]),
            ChainSegment::TupleField(index, span) => {
                concat(vec![text(".", *span), text(index.to_string(), *span)])
            }
            ChainSegment::Call(args, args_span) => {
                let items = args
                    .iter()
                    .map(|arg| (self.expr(arg), arg.span.start))
                    .collect();
                self.list("(", ")", items, self.has_comment(*args_span), *args_span)
            }
            ChainSegment::Index(index, bracket) => concat(vec![
                text("[", *bracket),
                self.expr(index),
                text("]", *bracket),
            ]),
            ChainSegment::Deref(op_span) => text(".*", *op_span),
            ChainSegment::TypeApply(args, span) => {
                let items = args
                    .iter()
                    .map(|arg| (self.generic_arg(arg), generic_arg_start(arg)))
                    .collect();
                concat(vec![
                    text(".[", *span),
                    self.list("", "", items, self.has_comment(*span), *span),
                    text("]", *span),
                ])
            }
        }
    }

    fn field_init(&mut self, field: &ast::FieldInit) -> Doc {
        concat(vec![
            text(field.name.clone(), field.name_span),
            text(": ", field.name_span),
            self.expr(&field.value),
        ])
    }

    fn handler(&mut self, handler: &ast::Handler, _span: Span) -> Doc {
        match handler {
            ast::Handler::Expr(expr) => self.expr(expr),
            ast::Handler::Block(block) => self.one_line_block(block),
        }
    }

    fn branch_expr(&mut self, expr: &Expr) -> Doc {
        if let ExprKind::Block(block) = &expr.kind {
            return self.block_branch(block);
        }
        let flat = concat(vec![
            text("{ ", expr.span),
            self.expr(expr),
            text(" }", expr.span),
        ]);
        let broken = concat(vec![
            text("{", expr.span),
            nest(1, concat(vec![hard(), self.expr(expr)])),
            hard(),
            text("}", expr.span),
        ]);
        choice(flat, broken, false)
    }

    fn if_expr(&mut self, cond: &ast::Cond, then: &Expr, otherwise: &Expr, span: Span) -> Doc {
        let else_flat = self.else_expr(otherwise, false);
        let else_broken = self.else_expr(otherwise, true);
        let flat = concat(vec![
            text("if ", span),
            self.cond(cond),
            text(" ", span),
            self.branch_expr(then),
            text(" ", span),
            else_flat,
        ]);
        let broken = concat(vec![
            text("if ", span),
            self.cond(cond),
            text(" ", span),
            self.branch_block(then),
            text(" ", span),
            else_broken,
        ]);
        choice(flat, broken, false)
    }

    fn else_expr(&mut self, expr: &Expr, broken: bool) -> Doc {
        if let ExprKind::If {
            cond,
            then,
            otherwise,
            span,
        } = &expr.kind
        {
            return concat(vec![
                text("else ", *span),
                self.if_expr(cond, then, otherwise, *span),
            ]);
        }
        let body = if broken {
            self.branch_block(expr)
        } else {
            self.branch_expr(expr)
        };
        concat(vec![text("else ", expr.span), body])
    }

    fn branch_block(&mut self, expr: &Expr) -> Doc {
        if let ExprKind::Block(block) = &expr.kind {
            return self.block_branch(block);
        }
        concat(vec![
            text("{", expr.span),
            nest(1, concat(vec![hard(), self.expr(expr)])),
            hard(),
            text("}", expr.span),
        ])
    }

    fn binary(&mut self, expr: &Expr) -> Doc {
        let ExprKind::Binary { op, .. } = &expr.kind else {
            return self.expr(expr);
        };
        let precedence = precedence(*op);
        let mut operands = Vec::new();
        let mut operators = Vec::new();
        self.flatten_binary(expr, precedence, &mut operands, &mut operators);
        let mut flat = Vec::new();
        for (index, operand) in operands.iter().enumerate() {
            if index > 0 {
                flat.push(text(
                    format!(" {} ", operators[index - 1].symbol()),
                    expr.span,
                ));
            }
            flat.push(operand.clone());
        }
        let mut broken = vec![operands[0].clone()];
        let mut rest = Vec::new();
        for (index, operand) in operands.iter().enumerate().skip(1) {
            rest.push(hard());
            rest.push(text(
                format!("{} ", operators[index - 1].symbol()),
                expr.span,
            ));
            rest.push(operand.clone());
        }
        broken.push(nest(1, concat(rest)));
        choice(concat(flat), concat(broken), false)
    }

    fn flatten_binary(
        &mut self,
        expr: &Expr,
        precedence: u8,
        operands: &mut Vec<Doc>,
        operators: &mut Vec<BinOp>,
    ) {
        if let ExprKind::Binary { op, lhs, rhs, .. } = &expr.kind
            && precedence_of(*op) == precedence
        {
            self.flatten_binary(lhs, precedence, operands, operators);
            operators.push(*op);
            self.flatten_binary(rhs, precedence, operands, operators);
            return;
        }
        operands.push(self.expr(expr));
    }
}

enum ChainSegment<'a> {
    Field(&'a str, Span),
    TupleField(u64, Span),
    Call(&'a [Expr], Span),
    Index(&'a Expr, Span),
    Deref(Span),
    TypeApply(&'a [ast::GenericArg], Span),
}

fn decompose_chain<'a>(expr: &'a Expr) -> Option<(&'a Expr, Vec<ChainSegment<'a>>)> {
    let (base, segments) = match &expr.kind {
        ExprKind::Field {
            base,
            name,
            name_span,
        } => {
            let (root, mut segments) = decompose_chain(base)?;
            segments.push(ChainSegment::Field(name, *name_span));
            (root, segments)
        }
        ExprKind::TupleField { base, index, span } => {
            let (root, mut segments) = decompose_chain(base)?;
            segments.push(ChainSegment::TupleField(*index, *span));
            (root, segments)
        }
        ExprKind::Call {
            callee,
            args,
            args_span,
        } => {
            let (root, mut segments) = decompose_chain(callee)?;
            segments.push(ChainSegment::Call(args, *args_span));
            (root, segments)
        }
        ExprKind::Index {
            base,
            index,
            bracket,
        } => {
            let (root, mut segments) = decompose_chain(base)?;
            segments.push(ChainSegment::Index(index, *bracket));
            (root, segments)
        }
        ExprKind::Deref { base, op_span } => {
            let (root, mut segments) = decompose_chain(base)?;
            segments.push(ChainSegment::Deref(*op_span));
            (root, segments)
        }
        ExprKind::TypeApply { base, args, span } => {
            let (root, mut segments) = decompose_chain(base)?;
            segments.push(ChainSegment::TypeApply(args, *span));
            (root, segments)
        }
        _ => return Some((expr, Vec::new())),
    };
    if segments.is_empty() {
        None
    } else {
        Some((base, segments))
    }
}

fn precedence(op: BinOp) -> u8 {
    precedence_of(op)
}

fn precedence_of(op: BinOp) -> u8 {
    match op {
        BinOp::Or => 1,
        BinOp::And => 2,
        BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => 3,
        BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor => 4,
        BinOp::Shl | BinOp::Shr => 5,
        BinOp::Add
        | BinOp::Sub
        | BinOp::AddWrap
        | BinOp::SubWrap
        | BinOp::AddSat
        | BinOp::SubSat => 6,
        BinOp::Mul | BinOp::Div | BinOp::Rem | BinOp::MulWrap | BinOp::MulSat => 7,
    }
}

// ---------------------------------------------------------------- patterns

impl Formatter<'_> {
    fn pattern(&mut self, pattern: &Pattern) -> Doc {
        match pattern {
            Pattern::Wild(span) => text("_", *span),
            Pattern::Bind { name, span } => text(name.clone(), *span),
            Pattern::Literal(expr) => self.expr(expr),
            Pattern::Range {
                lo,
                hi,
                inclusive,
                span,
            } => concat(vec![
                self.pattern(lo),
                text(if *inclusive { "..=" } else { ".." }, *span),
                self.pattern(hi),
            ]),
            Pattern::Variant {
                path,
                name,
                name_span,
                payload,
            } => {
                let mut docs = Vec::new();
                if let Some(path) = path {
                    docs.push(self.path_ty(path));
                    docs.push(text(".", *name_span));
                } else {
                    docs.push(text(".", *name_span));
                }
                docs.push(text(name.clone(), *name_span));
                if let Some(payload) = payload {
                    docs.push(text("(", *name_span));
                    docs.push(self.pattern(payload));
                    docs.push(text(")", *name_span));
                }
                concat(docs)
            }
            Pattern::Tuple { elems, span } => {
                let items = elems
                    .iter()
                    .map(|item| (self.pattern(item), item.span().start))
                    .collect();
                self.list("(", ")", items, self.has_comment(*span), *span)
            }
            Pattern::Struct {
                ty,
                fields,
                rest,
                span,
                ..
            } => {
                let mut docs = Vec::new();
                if let Some(ty) = ty {
                    docs.push(self.path_ty(ty));
                    docs.push(text(".", *span));
                    docs.push(text("{", *span));
                } else {
                    docs.push(text(".{", *span));
                }
                let mut items = Vec::new();
                for field in fields {
                    let mut item = vec![text(field.name.clone(), field.name_span)];
                    if let Some(pattern) = &field.pattern {
                        item.push(text(": ", field.name_span));
                        item.push(self.pattern(pattern));
                    }
                    items.push((concat(item), field.name_span.start));
                }
                if *rest {
                    items.push((text("..", *span), usize::MAX));
                }
                docs.push(self.list_spaced("", "}", items, self.has_comment(*span), *span));
                concat(docs)
            }
            Pattern::Poison => nil(),
        }
    }
}

// ---------------------------------------------------------------- comments

impl Formatter<'_> {
    fn place_comments(&self, mut lines: Vec<Line>) -> String {
        for comment in &self.comments {
            let start = comment.span.start.min(self.source.text.len());
            let line_start = self.source.text[..start]
                .rfind('\n')
                .map(|index| index + 1)
                .unwrap_or(0);
            let own_line = self.source.text[line_start..start].trim().is_empty();
            let source_line = self.source.position(start).line;
            if !own_line
                && let Some(index) =
                    best_previous(&lines, start, source_line, self.source, &self.tokens)
            {
                let line = &mut lines[index];
                line.text.push(' ');
                line.text.push_str(&comment.text);
                line.max_end = line.max_end.max(comment.span.end);
                continue;
            }
            let index = lines
                .iter()
                .position(|line| line.has_code && line.first_start >= start)
                .unwrap_or(lines.len());
            let indent = if index < lines.len() {
                let next = &lines[index];
                let trimmed = next.text.trim_start();
                if trimmed.starts_with('}') {
                    match index.checked_sub(1).map(|previous| &lines[previous].text) {
                        // Nothing but comments between the braces.
                        Some(previous) if previous.trim_end().ends_with('{') => {
                            leading_spaces(&next.text) + 4
                        }
                        Some(previous) => leading_spaces(previous),
                        None => 0,
                    }
                } else {
                    leading_spaces(&next.text)
                }
            } else {
                index
                    .checked_sub(1)
                    .map(|previous| leading_spaces(&lines[previous].text))
                    .unwrap_or(0)
            };
            let mut line = Line::default();
            line.text.push_str(&" ".repeat(indent));
            line.text.push_str(&comment.text);
            line.first_start = comment.span.start;
            line.max_end = comment.span.end;
            line.has_code = true;
            let blank_before = blank_line_before(self.source.text.as_str(), start);
            let blank_after =
                !comment.doc && blank_line_after(self.source.text.as_str(), comment.span.end);
            let mut at = index;
            if blank_before && !is_blank(&lines, at.saturating_sub(1)) {
                lines.insert(at, Line::default());
                at += 1;
            }
            lines.insert(at, line);
            if blank_after && !is_blank(&lines, at + 1) {
                lines.insert(at + 1, Line::default());
            }
        }
        if lines.is_empty() {
            return String::new();
        }
        let mut out = lines
            .into_iter()
            .map(|line| line.text)
            .collect::<Vec<_>>()
            .join("\n");
        out.push('\n');
        out
    }
}

fn is_blank(lines: &[Line], index: usize) -> bool {
    lines
        .get(index)
        .is_some_and(|line| !line.has_code && line.text.trim().is_empty())
}

fn blank_line_before(text: &str, start: usize) -> bool {
    let line_start = text[..start.min(text.len())]
        .rfind('\n')
        .map(|index| index + 1)
        .unwrap_or(0);
    if line_start == 0 {
        return false;
    }
    let previous_end = line_start - 1;
    let previous_start = text[..previous_end]
        .rfind('\n')
        .map(|index| index + 1)
        .unwrap_or(0);
    text[previous_start..previous_end].trim().is_empty()
}

fn blank_line_after(text: &str, end: usize) -> bool {
    let end = end.min(text.len());
    let Some(newline) = text[end..].find('\n') else {
        return false;
    };
    let next_start = end + newline + 1;
    if next_start >= text.len() {
        return false;
    }
    let next_end = text[next_start..]
        .find('\n')
        .map(|index| next_start + index)
        .unwrap_or(text.len());
    text[next_start..next_end].trim().is_empty()
}

fn leading_spaces(text: &str) -> usize {
    text.len() - text.trim_start_matches(' ').len()
}

fn best_previous(
    lines: &[Line],
    start: usize,
    source_line: u32,
    source: &Source,
    tokens: &[crate::token::Token],
) -> Option<usize> {
    if let Some(token) = tokens
        .iter()
        .filter(|token| token.span.end <= start)
        .max_by_key(|token| token.span.end)
        && let Some(index) = lines
            .iter()
            .rposition(|line| line.starts.contains(&(token.span.start as u32)))
    {
        return Some(index);
    }
    let mut same_line = None;
    let mut before: Option<(usize, usize)> = None;
    for (index, line) in lines.iter().enumerate() {
        if !line.has_code {
            continue;
        }
        if line
            .starts
            .iter()
            .any(|offset| source.position(*offset as usize).line == source_line)
        {
            same_line = Some(index);
        }
        if line.max_end <= start && before.is_none_or(|(_, end)| line.max_end > end) {
            before = Some((index, line.max_end));
        }
    }
    same_line.or(before.map(|(index, _)| index))
}
