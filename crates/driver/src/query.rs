//! `ingot query` (tools.md §5): `def`, `type` and `symbols` over the front end that
//! `pipeline::Analysis` keeps for us.

use crate::files::{Files, Overlays};
use crate::pipeline::{self, Analysis, FrontEnd};
use ingot_diag::diagnostic::json_string;
use ingot_diag::json::Json;
use ingot_diag::source::{Source, Sources, Span};
use ingot_sema::defs::{DefId, DefKind, ImportTarget, ModuleId, PackageId, Vis};
use ingot_sema::hir;
use ingot_sema::hir::{ExprId, LocalId, PatId, Res};
use ingot_sema::sigs::Tri;
use ingot_sema::typed::{ExprRes, PatRes, TypedBody};
use ingot_syntax::lexer::lex;
use ingot_syntax::token::{Token, TokenKind};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Instant;

#[derive(Default)]
pub struct QueryOutcome {
    pub lines: Vec<String>,
    pub human: String,
    pub error: Option<String>,
    pub exit: u8,
}

/// Run one query. `started` is when the command began, for `ms`.
pub fn run(
    kind: &str,
    at: Option<&str>,
    path: Option<&str>,
    match_text: Option<&str>,
    overlays: &Overlays,
    started: Instant,
) -> QueryOutcome {
    match kind {
        "def" | "type" => {
            let Some(at) = at else {
                return failure(
                    kind,
                    format!("`query {kind}` needs `FILE:LINE:COL`"),
                    started,
                );
            };
            query_position(kind, at, overlays, started)
        }
        "symbols" => query_symbols(path, match_text, overlays, started),
        other => failure(other, format!("unknown query `{other}`"), started),
    }
}

/// A §1 failure: an error line and the query's summary.
pub fn failure(kind: &str, message: String, started: Instant) -> QueryOutcome {
    let lines = failure_lines(kind, &message, started);
    QueryOutcome {
        lines,
        human: String::new(),
        error: Some(message),
        exit: 2,
    }
}

pub fn failure_lines(kind: &str, message: &str, started: Instant) -> Vec<String> {
    vec![
        crate::commands::error_line(message),
        summary(kind, false, started),
    ]
}

fn summary(kind: &str, ok: bool, started: Instant) -> String {
    let mut name = String::new();
    json_string(&mut name, kind);
    format!(
        "{{\"type\":\"summary\",\"command\":\"query\",\"kind\":{name},\"ok\":{ok},\"protocol\":1,\"ms\":{}}}",
        started.elapsed().as_millis()
    )
}

fn query_position(kind: &str, at: &str, overlays: &Overlays, started: Instant) -> QueryOutcome {
    let (file, line, col) = match parse_at(at) {
        Ok(value) => value,
        Err(message) => return failure(kind, message, started),
    };
    let files = Files::new(overlays.clone());
    if !files.exists(&file) {
        return failure(kind, format!("{} does not exist", file.display()), started);
    }
    let options = crate::pipeline::Options {
        load: crate::loader::LoadOptions {
            overlays: overlays.clone(),
            ..Default::default()
        },
        ..Default::default()
    };
    let mut analysis = match pipeline::analyze_path_with(&file, &options) {
        Ok(analysis) => analysis,
        Err(error) => return failure(kind, error.to_string(), started),
    };
    let Some(source_index) = source_index(&analysis, &file) else {
        return failure(
            kind,
            format!("{} is not part of the checked package", file.display()),
            started,
        );
    };
    let source = analysis.sources.get(source_index);
    let offset = match position_offset(&source.text, line, col) {
        Ok(offset) => offset,
        Err(message) => {
            return failure(
                kind,
                format!("{}:{}:{}: {message}", file.display(), line, col),
                started,
            );
        }
    };
    let Some(token) = token_at(&source.text, offset) else {
        return empty(kind, started);
    };
    let Some(front_end) = analysis.front_end.as_mut() else {
        return empty(kind, started);
    };
    let mut context = Context::new(&analysis.sources, front_end);
    let occurrences = context.occurrences();
    let occurrence = occurrences.iter().find(|occurrence| {
        occurrence.file == source_index
            && occurrence.span.start == token.span.start
            && occurrence.span.end == token.span.end
    });
    if kind == "def" {
        let lines: Vec<String> = occurrence
            .and_then(|occurrence| context.definition(occurrence))
            .map(|line| vec![line.to_text()])
            .unwrap_or_default();
        let human = lines.join("\n");
        let mut lines = lines;
        lines.push(summary(kind, true, started));
        QueryOutcome {
            lines,
            human,
            error: None,
            exit: 0,
        }
    } else {
        let line = context.type_at(occurrence, source_index, token.span);
        let human = line
            .as_ref()
            .map(|line| {
                line.get("of")
                    .and_then(Json::as_str)
                    .unwrap_or("")
                    .to_string()
            })
            .unwrap_or_default();
        let mut lines = Vec::new();
        if let Some(line) = line {
            lines.push(line.to_text());
        }
        lines.push(summary(kind, true, started));
        QueryOutcome {
            lines,
            human,
            error: None,
            exit: 0,
        }
    }
}

fn empty(kind: &str, started: Instant) -> QueryOutcome {
    QueryOutcome {
        lines: vec![summary(kind, true, started)],
        human: String::new(),
        error: None,
        exit: 0,
    }
}

fn query_symbols(
    path: Option<&str>,
    match_text: Option<&str>,
    overlays: &Overlays,
    started: Instant,
) -> QueryOutcome {
    let path = path
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    let files = Files::new(overlays.clone());
    if !files.is_dir(&path) && !files.exists(&path) {
        return failure(
            "symbols",
            format!("{} does not exist", path.display()),
            started,
        );
    }
    let options = crate::pipeline::Options {
        load: crate::loader::LoadOptions {
            overlays: overlays.clone(),
            ..Default::default()
        },
        ..Default::default()
    };
    let mut analysis = match pipeline::analyze_path_with(&path, &options) {
        Ok(analysis) => analysis,
        Err(error) => return failure("symbols", error.to_string(), started),
    };
    let only_file = files
        .is_file(&path)
        .then(|| source_index(&analysis, &path))
        .flatten();
    let mut lines = Vec::new();
    if let Some(front_end) = analysis.front_end.as_mut() {
        let context = Context::new(&analysis.sources, front_end);
        for symbol in context.symbols(only_file, match_text) {
            lines.push(symbol.to_text());
        }
    }
    let human = lines.join("\n");
    lines.push(summary("symbols", true, started));
    QueryOutcome {
        lines,
        human,
        error: None,
        exit: 0,
    }
}

fn source_index(analysis: &Analysis, path: &Path) -> Option<usize> {
    let key = crate::files::normalize(path);
    analysis
        .sources
        .files
        .iter()
        .position(|source| crate::files::normalize(&source.absolute) == key)
}

// ------------------------------------------------------------------ positions

fn parse_at(at: &str) -> Result<(PathBuf, u32, u32), String> {
    let (rest, col) = at
        .rsplit_once(':')
        .ok_or_else(|| format!("`{at}` is not FILE:LINE:COL"))?;
    let (file, line) = rest
        .rsplit_once(':')
        .ok_or_else(|| format!("`{at}` is not FILE:LINE:COL"))?;
    let col: u32 = col
        .parse()
        .map_err(|_| format!("`{col}` is not a column"))?;
    let line: u32 = line
        .parse()
        .map_err(|_| format!("`{line}` is not a line"))?;
    if file.is_empty() || line == 0 || col == 0 {
        return Err(format!("`{at}` is not FILE:LINE:COL"));
    }
    Ok((PathBuf::from(file), line, col))
}

fn position_offset(text: &str, line: u32, col: u32) -> Result<usize, String> {
    let mut starts = vec![0];
    for (i, byte) in text.bytes().enumerate() {
        if byte == b'\n' {
            starts.push(i + 1);
        }
    }
    let index = line as usize - 1;
    if index >= starts.len() {
        return Err(format!("line {line} is past the end of the file"));
    }
    let start = starts[index];
    let end = starts
        .get(index + 1)
        .map(|next| next - 1)
        .unwrap_or(text.len());
    let end = if end > start && text.as_bytes()[end - 1] == b'\r' {
        end - 1
    } else {
        end
    };
    let offset = start + (col as usize - 1);
    if offset > end {
        return Err(format!("column {col} is past the end of line {line}"));
    }
    Ok(offset)
}

/// The token that contains `offset`, or the one that ends there.
pub fn token_span(text: &str, offset: usize) -> Option<Span> {
    token_at(text, offset).map(|token| token.span)
}

fn token_at(text: &str, offset: usize) -> Option<Token> {
    let source = Source::new("<query>", text);
    let (tokens, _) = lex(&source);
    let mut ending = None;
    for token in tokens {
        if token.span.start <= offset && offset < token.span.end {
            return Some(token);
        }
        if token.span.end == offset && !matches!(token.kind, TokenKind::Eof) {
            ending = Some(token);
        }
    }
    ending
}

// ------------------------------------------------------------------ the context

struct Context<'a> {
    sources: &'a Sources,
    sema: &'a ingot_sema::Sema,
    packages: &'a [hir::Package],
    declarations: &'a mut ingot_sema::Declarations,
    bodies: &'a ingot_sema::typed::Bodies,
    checked: usize,
    package_id: PackageId,
    infos: BTreeMap<DefId, DefInfo>,
    impl_targets: BTreeMap<DefId, Res>,
}

#[derive(Clone, Copy)]
struct DefInfo {
    file: usize,
    name: Span,
    extent: Option<Span>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Target {
    Def(DefId),
    Local(LocalId),
}

#[derive(Clone, Copy, Debug)]
struct Occurrence {
    file: usize,
    span: Span,
    target: Target,
}

impl<'a> Context<'a> {
    fn new(sources: &'a Sources, front_end: &'a mut FrontEnd) -> Self {
        let packages = &front_end.resolved.packages;
        let pkg = &packages[front_end.checked];
        let package_id = pkg
            .modules
            .first()
            .map(|module| front_end.resolved.sema.module(module.module).package)
            .unwrap_or(PackageId(u32::MAX));
        let mut context = Context {
            sources,
            sema: &front_end.resolved.sema,
            packages,
            declarations: &mut front_end.declarations,
            bodies: &front_end.bodies,
            checked: front_end.checked,
            package_id,
            infos: BTreeMap::new(),
            impl_targets: BTreeMap::new(),
        };
        context.collect_definitions();
        context
    }

    fn collect_definitions(&mut self) {
        for pkg in self.packages {
            for item in pkg.items.values() {
                match item {
                    hir::Item::Fn(f) => {
                        self.add_item_info(f.def, f.name_span, f.span);
                        for generic in &f.generics {
                            self.add_name_info(generic.def, generic.name_span);
                        }
                    }
                    hir::Item::Struct(s) => {
                        let name = self.sema.def(s.def).span;
                        self.add_item_info(s.def, name, s.span);
                        for field in &s.fields {
                            self.add_member_info(field.def, field.name_span, field.span);
                        }
                        for generic in &s.generics {
                            self.add_name_info(generic.def, generic.name_span);
                        }
                    }
                    hir::Item::Enum(e) => {
                        let name = self.sema.def(e.def).span;
                        self.add_item_info(e.def, name, e.span);
                        for variant in &e.variants {
                            self.add_member_info(variant.def, variant.name_span, variant.span);
                        }
                        for generic in &e.generics {
                            self.add_name_info(generic.def, generic.name_span);
                        }
                    }
                    hir::Item::Interface(i) => {
                        let name = self.sema.def(i.def).span;
                        self.add_item_info(i.def, name, i.span);
                        for generic in &i.generics {
                            self.add_name_info(generic.def, generic.name_span);
                        }
                    }
                    hir::Item::Impl(i) => {
                        self.impl_targets.insert(i.def, i.target_path_res());
                        for generic in &i.generics {
                            self.add_name_info(generic.def, generic.name_span);
                        }
                    }
                    hir::Item::Const(c) => {
                        let name = self.sema.def(c.def).span;
                        self.add_item_info(c.def, name, c.span);
                    }
                    hir::Item::Static(s) => {
                        let name = self.sema.def(s.def).span;
                        self.add_item_info(s.def, name, s.span);
                    }
                    hir::Item::Alias(a) => {
                        let name = self.sema.def(a.def).span;
                        self.add_item_info(a.def, name, a.span);
                        for generic in &a.generics {
                            self.add_name_info(generic.def, generic.name_span);
                        }
                    }
                    hir::Item::Test(t) => {
                        let name = test_name_span(self.sources, self.sema.def(t.def).file, t.span)
                            .unwrap_or_else(|| Span::empty(t.span.start));
                        self.add_item_info(t.def, name, t.span);
                    }
                }
            }
        }
        // Modules have no name in the source; their definitions still map to a file.
        for module in self.sema.modules.iter() {
            if module.package == PackageId(u32::MAX) {
                continue;
            }
            if let Some(file) = module.file {
                self.infos.insert(
                    module.def,
                    DefInfo {
                        file,
                        name: Span::empty(0),
                        extent: None,
                    },
                );
            }
        }
    }

    fn add_item_info(&mut self, def: DefId, name: Span, extent: Span) {
        if let Some(file) = self.sema.def(def).file {
            let text = &self.sources.get(file).text;
            let extent = item_extent(text, extent);
            self.infos.insert(
                def,
                DefInfo {
                    file,
                    name,
                    extent: Some(extent),
                },
            );
        }
    }

    fn add_member_info(&mut self, def: DefId, name: Span, span: Span) {
        if let Some(file) = self.sema.def(def).file {
            let text = &self.sources.get(file).text;
            let extent = member_extent(text, span);
            self.infos.insert(
                def,
                DefInfo {
                    file,
                    name,
                    extent: Some(extent),
                },
            );
        }
    }

    fn add_name_info(&mut self, def: DefId, name: Span) {
        if let Some(file) = self.sema.def(def).file {
            self.infos.insert(
                def,
                DefInfo {
                    file,
                    name,
                    extent: None,
                },
            );
        }
    }

    fn checked_package(&self) -> &'a hir::Package {
        &self.packages[self.checked]
    }

    fn definition(&self, occurrence: &Occurrence) -> Option<Json> {
        match occurrence.target {
            Target::Local(local) => {
                let local = self.checked_package().local(local);
                let item = match local.kind {
                    hir::LocalKind::Param => "param",
                    _ => "local",
                };
                let file = self.sources.get(occurrence.file).path.clone();
                Some(definition_object(
                    local.name.clone(),
                    item,
                    span_json(self.sources, &file, local.span),
                    None,
                    None,
                    None,
                ))
            }
            Target::Def(def) => self.definition_of(def),
        }
    }

    fn definition_of(&self, def: DefId) -> Option<Json> {
        let mut def = def;
        // `Self` names its interface, or the target type of its impl.
        if matches!(self.sema.def(def).kind, DefKind::SelfTy) {
            def = self.self_target(def)?;
        }
        if matches!(
            self.sema.def(def).kind,
            DefKind::Builtin(_) | DefKind::Poison | DefKind::Package
        ) {
            return None;
        }
        if self.is_hidden(def) {
            return None;
        }
        let info = self.infos.get(&def).copied()?;
        let source = self.sources.get(info.file);
        if source.path == ingot_sema::BUILTIN_PATH {
            return None;
        }
        let item = item_name(self.sema.def(def).kind)?;
        let path = self.def_path(def)?;
        let signature = self.signature(def, info)?;
        let doc = info
            .extent
            .and_then(|extent| doc_comment(&source.text, extent.start));
        Some(definition_object(
            path,
            item,
            span_json(self.sources, &source.path, info.name),
            info.extent
                .map(|extent| span_json(self.sources, &source.path, extent)),
            signature,
            doc,
        ))
    }

    fn self_target(&self, self_def: DefId) -> Option<DefId> {
        let parent = self.sema.def(self_def).parent?;
        match self.sema.def(parent).kind {
            DefKind::Interface => Some(parent),
            DefKind::Impl => match self.impl_targets.get(&parent) {
                Some(Res::Def(def)) => Some(*def),
                _ => None,
            },
            _ => None,
        }
    }

    fn is_hidden(&self, def: DefId) -> bool {
        let module = self.sema.module(self.sema.def(def).module);
        module.path == ingot_sema::BUILTIN_MODULE_PATH
            || self
                .sema
                .def(def)
                .file
                .is_some_and(|file| self.sources.get(file).path == ingot_sema::BUILTIN_PATH)
    }

    fn signature(&self, def: DefId, info: DefInfo) -> Option<Option<String>> {
        let kind = self.sema.def(def).kind;
        let text = &self.sources.get(info.file).text;
        let signature = match kind {
            DefKind::Fn
            | DefKind::ExternFn
            | DefKind::Test { .. }
            | DefKind::Struct
            | DefKind::Enum
            | DefKind::Interface => {
                // The body's `{` comes after the name: a test's name is a string that may
                // hold a `{` of its own.
                let extent = info.extent?;
                let after_name = info.name.end.saturating_sub(extent.start);
                cut_signature(text, extent, Some(after_name))?
            }
            DefKind::Field | DefKind::Variant => cut_member_signature(text, info.extent?)?,
            DefKind::Const | DefKind::Static | DefKind::TypeAlias | DefKind::AssocType => {
                cut_signature(text, info.extent?, None)?
            }
            _ => return Some(None),
        };
        Some(Some(signature))
    }

    fn def_path(&self, def: DefId) -> Option<String> {
        let data = self.sema.def(def);
        match data.kind {
            DefKind::Module => Some(self.sema.module(data.module).path.clone()),
            DefKind::GenericParam { .. } | DefKind::SelfTy => Some(data.name.clone()),
            DefKind::Test { .. } => Some(format!(
                "{}.{}",
                self.sema.module(data.module).path,
                data.name
            )),
            _ => match data.parent {
                Some(parent) => {
                    let parent = if matches!(self.sema.def(parent).kind, DefKind::Impl) {
                        match self.impl_targets.get(&parent) {
                            Some(Res::Def(target)) => *target,
                            _ => return None,
                        }
                    } else {
                        parent
                    };
                    Some(format!("{}.{}", self.def_path(parent)?, data.name))
                }
                None => Some(format!(
                    "{}.{}",
                    self.sema.module(data.module).path,
                    data.name
                )),
            },
        }
    }

    fn type_at(
        &mut self,
        occurrence: Option<&Occurrence>,
        source_index: usize,
        token: Span,
    ) -> Option<Json> {
        let pkg = &self.packages[self.checked];
        // A binding name: the local's type. A use is an expression.
        let local = occurrence.and_then(|occurrence| match occurrence.target {
            Target::Local(local) if same_span(pkg.local(local).span, token) => Some(local),
            _ => None,
        });
        let local = local.or_else(|| {
            pkg.locals
                .iter()
                .position(|local| same_span(local.span, token))
                .map(|index| LocalId(index as u32))
        });
        if let Some(local) = local {
            let mut found = None;
            for body in self.bodies.bodies.values() {
                if body.failed {
                    continue;
                }
                if let Some(info) = body.local(local)
                    && let Some(file) = self.sema.def(body.owner).file
                    && file == source_index
                {
                    found = Some((pkg.local(local).span, info.ty, file));
                    break;
                }
            }
            if let Some((span, ty, file)) = found {
                return self.type_object(span, ty, file);
            }
        }
        // The innermost expression containing the token.
        let mut best: Option<(Span, ingot_sema::TypeId, usize)> = None;
        for body in self.bodies.bodies.values() {
            if body.failed {
                continue;
            }
            let Some(file) = self.sema.def(body.owner).file else {
                continue;
            };
            if file != source_index {
                continue;
            }
            for (expr_id, info) in &body.exprs {
                let span = self.packages[self.checked].expr(*expr_id).span;
                if span.start <= token.start && token.end <= span.end && span.start < span.end {
                    let width = span.end - span.start;
                    let replace = best
                        .as_ref()
                        .is_none_or(|(best_span, _, _)| width < best_span.end - best_span.start);
                    if replace {
                        best = Some((span, info.ty, file));
                    }
                }
            }
        }
        let (span, ty, file) = best?;
        self.type_object(span, ty, file)
    }

    fn type_object(&mut self, span: Span, ty: ingot_sema::TypeId, file: usize) -> Option<Json> {
        if matches!(
            self.declarations.types.kind(ty),
            ingot_sema::ty::TyKind::Poison
        ) {
            return None;
        }
        let of = self.declarations.types.display(self.sema, ty);
        let props = self.props(ty);
        let path = self.sources.get(file).path.clone();
        Some(object(vec![
            ("type", Json::String("type_at".to_string())),
            ("span", span_json(self.sources, &path, span)),
            ("of", Json::String(of)),
            ("properties", props),
        ]))
    }

    fn props(&mut self, ty: ingot_sema::TypeId) -> Json {
        let props = self.declarations.props(ty);
        object(vec![
            ("copy", boolean(props.copy)),
            ("linear", boolean(props.linear)),
            ("view", boolean(props.view)),
            ("send", boolean(props.send)),
            ("sync", boolean(props.sync)),
        ])
    }

    fn symbols(&self, only_file: Option<usize>, match_text: Option<&str>) -> Vec<Json> {
        let pkg = self.checked_package();
        let mut lines = Vec::new();
        let mut seen = BTreeSet::new();
        for module in &pkg.modules {
            if let Some(only) = only_file
                && self.sema.module(module.module).file != Some(only)
            {
                continue;
            }
            let mut defs = Vec::new();
            for def in &module.items {
                self.symbol_defs(*def, &mut defs, &mut seen);
            }
            for externs in &module.externs {
                for def in &externs.functions {
                    self.symbol_defs(*def, &mut defs, &mut seen);
                }
            }
            defs.sort_by_key(|def| {
                let info = self.infos.get(def).copied();
                (info.map(|i| i.name.start).unwrap_or(0), def.0)
            });
            for def in defs {
                if let Some(line) = self.symbol(def, match_text) {
                    lines.push(line);
                }
            }
        }
        lines
    }

    fn symbol_defs(&self, def: DefId, out: &mut Vec<DefId>, seen: &mut BTreeSet<DefId>) {
        if !seen.insert(def) {
            return;
        }
        match self.sema.def(def).kind {
            DefKind::Impl => {
                if let Some(item) = self.item_of(def)
                    && let hir::Item::Impl(imp) = item
                {
                    for member in &imp.members {
                        self.symbol_defs(*member, out, seen);
                    }
                }
            }
            DefKind::Struct => {
                out.push(def);
                if let Some(hir::Item::Struct(st)) = self.item_of(def) {
                    for field in &st.fields {
                        out.push(field.def);
                    }
                }
            }
            DefKind::Enum => {
                out.push(def);
                if let Some(hir::Item::Enum(e)) = self.item_of(def) {
                    for variant in &e.variants {
                        out.push(variant.def);
                    }
                }
            }
            DefKind::Interface => {
                out.push(def);
                if let Some(hir::Item::Interface(i)) = self.item_of(def) {
                    for member in &i.members {
                        out.push(*member);
                    }
                }
            }
            DefKind::Fn
            | DefKind::ExternFn
            | DefKind::Const
            | DefKind::Static
            | DefKind::TypeAlias
            | DefKind::AssocType
            | DefKind::Test { .. } => out.push(def),
            DefKind::Variant | DefKind::Field => {}
            _ => {}
        }
    }

    fn symbol(&self, def: DefId, match_text: Option<&str>) -> Option<Json> {
        if self.is_hidden(def) {
            return None;
        }
        let info = self.infos.get(&def).copied()?;
        let item = item_name(self.sema.def(def).kind)?;
        if !matches!(
            item,
            "fn" | "struct"
                | "enum"
                | "variant"
                | "field"
                | "interface"
                | "type"
                | "const"
                | "static"
                | "test"
        ) {
            return None;
        }
        let path = self.def_path(def)?;
        if let Some(text) = match_text
            && !path.contains(text)
        {
            return None;
        }
        let source = self.sources.get(info.file);
        Some(object(vec![
            ("type", Json::String("symbol".to_string())),
            ("path", Json::String(path)),
            ("item", Json::String(item.to_string())),
            ("span", span_json(self.sources, &source.path, info.name)),
            (
                "extent",
                span_json(self.sources, &source.path, info.extent.unwrap_or(info.name)),
            ),
            ("pub", Json::Bool(self.sema.def(def).vis == Vis::Pub)),
        ]))
    }

    fn typed_body(&self, def: DefId) -> Option<&'a TypedBody> {
        let body = self.bodies.bodies.get(&def)?;
        (!body.failed).then_some(body)
    }

    fn item_of(&self, def: DefId) -> Option<&'a hir::Item> {
        self.packages.iter().find_map(|pkg| pkg.items.get(&def))
    }

    fn occurrences(&self) -> Vec<Occurrence> {
        let mut collector = Collector {
            context: self,
            occurrences: Vec::new(),
            visited: BTreeSet::new(),
            tokens: BTreeMap::new(),
        };
        collector.collect();
        collector.occurrences
    }
}

/// The declaration in `span` without its body: with `brace_after`, cut at the first `{`
/// at or after that offset into the span (the end of the declared name).
fn cut_signature(text: &str, span: Span, brace_after: Option<usize>) -> Option<String> {
    let slice = text.get(span.start..span.end)?;
    let mut end = slice.len();
    if let Some(from) = brace_after
        && let Some(index) = slice.get(from..).and_then(|rest| rest.find('{'))
    {
        end = from + index;
    }
    let mut out = slice[..end].trim_end().to_string();
    while out.ends_with(';') || out.ends_with(',') {
        out.pop();
        out = out.trim_end().to_string();
    }
    (!out.is_empty()).then_some(out)
}

fn item_extent(text: &str, span: Span) -> Span {
    let Some(slice) = text.get(span.start..span.end) else {
        return span;
    };
    let trimmed = slice.trim_end().trim_end_matches([';', ',']).trim_end();
    Span::new(span.start, span.start + trimmed.len())
}

fn member_extent(text: &str, span: Span) -> Span {
    let Some(slice) = text.get(span.start..span.end) else {
        return span;
    };
    let end = member_end(slice);
    let trimmed = slice[..end].trim_end().len();
    Span::new(span.start, span.start + trimmed)
}

fn cut_member_signature(text: &str, span: Span) -> Option<String> {
    let slice = text.get(span.start..span.end)?;
    let out = slice[..member_end(slice)].trim_end().to_string();
    (!out.is_empty()).then_some(out)
}

/// Where a field or variant's declaration ends: the first `,`, `;` or `}` outside
/// brackets and parentheses in its type or value.
fn member_end(slice: &str) -> usize {
    let mut depth = 0usize;
    for (index, character) in slice.char_indices() {
        match character {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' => depth = depth.saturating_sub(1),
            '}' => {
                if depth == 0 {
                    return index;
                }
                depth -= 1;
            }
            ',' | ';' if depth == 0 => return index,
            _ => {}
        }
    }
    slice.len()
}

fn doc_comment(text: &str, start: usize) -> Option<String> {
    if start > text.len() {
        return None;
    }
    let mut line_start = text[..start]
        .rfind('\n')
        .map(|index| index + 1)
        .unwrap_or(0);
    let mut lines = Vec::new();
    loop {
        if line_start == 0 {
            break;
        }
        let newline = line_start - 1;
        let previous_start = text[..newline]
            .rfind('\n')
            .map(|index| index + 1)
            .unwrap_or(0);
        let mut line = &text[previous_start..newline];
        line = line.strip_suffix('\r').unwrap_or(line);
        let trimmed = line.trim_start();
        let Some(rest) = trimmed.strip_prefix("///") else {
            break;
        };
        let rest = rest.strip_prefix(' ').unwrap_or(rest);
        lines.push(rest.to_string());
        line_start = previous_start;
    }
    if lines.is_empty() {
        None
    } else {
        lines.reverse();
        Some(lines.join("\n"))
    }
}

fn same_span(a: Span, b: Span) -> bool {
    a.start == b.start && a.end == b.end
}

fn boolean(value: Tri) -> Json {
    Json::Bool(value == Tri::Yes)
}

fn definition_object(
    path: String,
    item: &str,
    span: Json,
    extent: Option<Json>,
    signature: Option<String>,
    doc: Option<String>,
) -> Json {
    let mut members = vec![
        ("type".to_string(), Json::String("definition".to_string())),
        ("path".to_string(), Json::String(path)),
        ("item".to_string(), Json::String(item.to_string())),
        ("span".to_string(), span),
    ];
    if let Some(extent) = extent {
        members.push(("extent".to_string(), extent));
    }
    if let Some(signature) = signature {
        members.push(("signature".to_string(), Json::String(signature)));
    }
    if let Some(doc) = doc {
        members.push(("doc".to_string(), Json::String(doc)));
    }
    Json::Object(members)
}

fn span_json(sources: &Sources, file: &str, span: Span) -> Json {
    let source = sources.iter().find(|source| source.path == file);
    let (start, end) = source
        .map(|source| (source.position(span.start), source.position(span.end)))
        .unwrap_or((
            ingot_diag::source::Position { line: 1, col: 1 },
            ingot_diag::source::Position { line: 1, col: 1 },
        ));
    object(vec![
        ("file", Json::String(file.to_string())),
        (
            "start",
            object(vec![
                ("line", Json::Number(start.line.to_string())),
                ("col", Json::Number(start.col.to_string())),
                ("offset", Json::Number(span.start.to_string())),
            ]),
        ),
        (
            "end",
            object(vec![
                ("line", Json::Number(end.line.to_string())),
                ("col", Json::Number(end.col.to_string())),
                ("offset", Json::Number(span.end.to_string())),
            ]),
        ),
    ])
}

fn object(members: Vec<(&str, Json)>) -> Json {
    Json::Object(
        members
            .into_iter()
            .map(|(key, value)| (key.to_string(), value))
            .collect(),
    )
}

fn item_name(kind: DefKind) -> Option<&'static str> {
    Some(match kind {
        DefKind::Module => "module",
        DefKind::Fn | DefKind::ExternFn => "fn",
        DefKind::Struct => "struct",
        DefKind::Enum => "enum",
        DefKind::Variant => "variant",
        DefKind::Field => "field",
        DefKind::Interface => "interface",
        DefKind::TypeAlias | DefKind::AssocType => "type",
        DefKind::Const => "const",
        DefKind::Static => "static",
        DefKind::Test { .. } => "test",
        DefKind::GenericParam { .. } => "generic",
        _ => return None,
    })
}

fn test_name_span(sources: &Sources, file: Option<usize>, span: Span) -> Option<Span> {
    let file = file?;
    let text = &sources.get(file).text;
    let slice = text.get(span.start..span.end)?;
    let mut offset = span.start;
    let mut keyword_len = None;
    for word in ["test", "bench"] {
        if let Some(rest) = slice.strip_prefix(word)
            && rest
                .chars()
                .next()
                .is_some_and(|character| character.is_whitespace())
        {
            keyword_len = Some(word.len());
            break;
        }
    }
    offset += keyword_len?;
    let bytes = text.as_bytes();
    while offset < span.end && bytes.get(offset).is_some_and(u8::is_ascii_whitespace) {
        offset += 1;
    }
    if text.as_bytes().get(offset) == Some(&b'"') {
        let start = offset;
        offset += 1;
        while offset < span.end {
            if text.as_bytes()[offset] == b'"' {
                return Some(Span::new(start, offset + 1));
            }
            offset += 1;
        }
        Some(Span::new(start, span.end))
    } else {
        let start = offset;
        while offset < span.end && is_ident_byte(text.as_bytes()[offset]) {
            offset += 1;
        }
        (offset > start).then_some(Span::new(start, offset))
    }
}

fn is_ident_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

// Turn a path's `impl` target into the definition it names, when it is one.
trait ImplTargetRes {
    fn target_path_res(&self) -> Res;
}

impl ImplTargetRes for hir::ImplItem {
    fn target_path_res(&self) -> Res {
        match &self.target.kind {
            hir::TyKind::Path(path) => path.res,
            _ => Res::Poison,
        }
    }
}

// The occurrence collector: every name in the checked package's HIR and typed bodies.

struct Collector<'a, 'b> {
    context: &'a Context<'b>,
    occurrences: Vec<Occurrence>,
    visited: BTreeSet<DefId>,
    tokens: BTreeMap<usize, Vec<Token>>,
}

impl Collector<'_, '_> {
    fn collect(&mut self) {
        let sema = self.context.sema;
        let pkg = self.context.checked_package();
        for (index, def) in sema.defs.iter().enumerate() {
            let def_id = DefId(index as u32);
            if def.module == ModuleId(0) || def.file.is_none() {
                continue;
            }
            if sema.module(def.module).package != self.context.package_id {
                continue;
            }
            if matches!(
                def.kind,
                DefKind::Impl
                    | DefKind::SelfTy
                    | DefKind::Module
                    | DefKind::Package
                    | DefKind::Builtin(_)
                    | DefKind::Poison
            ) {
                continue;
            }
            if let Some(info) = self.context.infos.get(&def_id) {
                self.occurrences.push(Occurrence {
                    file: info.file,
                    span: info.name,
                    target: Target::Def(def_id),
                });
            }
        }
        for module in &pkg.modules {
            if sema.module(module.module).path == ingot_sema::BUILTIN_MODULE_PATH {
                continue;
            }
            for def in &module.items {
                self.item(*def);
            }
            for externs in &module.externs {
                for def in &externs.functions {
                    self.item(*def);
                }
            }
            self.imports(module.module);
        }
    }

    fn item(&mut self, def: DefId) {
        if !self.visited.insert(def) {
            return;
        }
        let Some(item) = self.context.item_of(def).cloned() else {
            return;
        };
        let owner = self.context.sema.def(def);
        let Some(file) = owner.file else {
            return;
        };
        let module = owner.module;
        let body = self.context.typed_body(def);
        match &item {
            hir::Item::Fn(f) => {
                self.generics(&f.generics, file, module);
                for param in &f.params {
                    self.occurrences.push(Occurrence {
                        file,
                        span: param.name_span,
                        target: Target::Local(param.local),
                    });
                    self.ty(file, module, &param.ty);
                }
                self.ty(file, module, &f.ret);
                if let Some(error) = &f.error {
                    self.ty(file, module, error);
                }
                for origin in &f.from {
                    if let Some(index) = origin.param
                        && let Some(param) = f.params.get(index)
                    {
                        self.occurrences.push(Occurrence {
                            file,
                            span: origin.span,
                            target: Target::Local(param.local),
                        });
                    }
                }
                if let Some(block) = f.body {
                    self.block(block, file, module, body);
                }
            }
            hir::Item::Struct(s) => {
                self.generics(&s.generics, file, module);
                for field in &s.fields {
                    self.ty(file, module, &field.ty);
                    if let Some(align) = field.align {
                        self.expr(align, file, module, body);
                    }
                }
                if let Some(align) = s.align {
                    self.expr(align, file, module, body);
                }
            }
            hir::Item::Enum(e) => {
                self.generics(&e.generics, file, module);
                if let Some(tag) = &e.tag {
                    self.ty(file, module, tag);
                }
                for variant in &e.variants {
                    if let Some(payload) = &variant.payload {
                        self.ty(file, module, payload);
                    }
                    if let Some(value) = variant.value {
                        self.expr(value, file, module, body);
                    }
                }
            }
            hir::Item::Interface(i) => {
                self.generics(&i.generics, file, module);
                for bound in &i.bounds {
                    self.path(file, module, bound);
                }
                for member in &i.members {
                    self.item(*member);
                }
            }
            hir::Item::Impl(i) => {
                self.generics(&i.generics, file, module);
                if let Some(interface) = &i.interface {
                    self.path(file, module, interface);
                }
                self.ty(file, module, &i.target);
                for member in &i.members {
                    self.item(*member);
                }
            }
            hir::Item::Const(c) => {
                self.ty(file, module, &c.ty);
                self.expr(c.value, file, module, body);
            }
            hir::Item::Static(s) => {
                self.ty(file, module, &s.ty);
                if let Some(align) = s.align {
                    self.expr(align, file, module, body);
                }
                self.expr(s.value, file, module, body);
            }
            hir::Item::Alias(a) => {
                self.generics(&a.generics, file, module);
                if let Some(ty) = &a.ty {
                    self.ty(file, module, ty);
                }
            }
            hir::Item::Test(t) => {
                self.block(t.body, file, module, body);
            }
        }
    }

    fn generics(&mut self, generics: &[hir::GenericParam], file: usize, module: ModuleId) {
        for generic in generics {
            for bound in &generic.bounds {
                self.path(file, module, bound);
            }
            if let Some(ty) = &generic.const_ty {
                self.ty(file, module, ty);
            }
        }
    }

    fn imports(&mut self, module: ModuleId) {
        let data = self.context.sema.module(module);
        let Some(file) = data.file else {
            return;
        };
        for import in &data.imports {
            let tokens = self.tokens(file).to_vec();
            let mut segments: Vec<(String, Span)> = Vec::new();
            let mut after_as = false;
            for token in tokens.iter().filter(|token| {
                token.span.start >= import.span.start && token.span.end <= import.span.end
            }) {
                match &token.kind {
                    TokenKind::Keyword(ingot_syntax::token::Kw::As) => after_as = true,
                    TokenKind::Ident(name) | TokenKind::TypeIdent(name) if !after_as => {
                        segments.push((name.clone(), token.span));
                    }
                    _ => {}
                }
            }
            for (index, (_, span)) in segments.iter().enumerate() {
                if index + 1 == segments.len() {
                    match &import.target {
                        ImportTarget::Module(target) => {
                            self.occurrences.push(Occurrence {
                                file,
                                span: *span,
                                target: Target::Def(self.context.sema.module(*target).def),
                            });
                        }
                        ImportTarget::Item(def) => self.occurrences.push(Occurrence {
                            file,
                            span: *span,
                            target: Target::Def(*def),
                        }),
                        ImportTarget::Poison => {}
                    }
                } else {
                    let dotted = segments[..=index]
                        .iter()
                        .map(|(name, _)| name.as_str())
                        .collect::<Vec<_>>()
                        .join(".");
                    if let Some(target) = self.context.sema.module_paths.get(&dotted) {
                        self.occurrences.push(Occurrence {
                            file,
                            span: *span,
                            target: Target::Def(self.context.sema.module(*target).def),
                        });
                    }
                }
            }
            if !segments
                .iter()
                .any(|(_, span)| same_span(*span, import.name_span))
            {
                let target = match &import.target {
                    ImportTarget::Module(target) => {
                        Some(Target::Def(self.context.sema.module(*target).def))
                    }
                    ImportTarget::Item(def) => Some(Target::Def(*def)),
                    ImportTarget::Poison => None,
                };
                if let Some(target) = target {
                    self.occurrences.push(Occurrence {
                        file,
                        span: import.name_span,
                        target,
                    });
                }
            }
        }
    }

    fn tokens(&mut self, file: usize) -> &[Token] {
        self.tokens
            .entry(file)
            .or_insert_with(|| {
                let source = self.context.sources.get(file);
                lex(source).0
            })
            .as_slice()
    }

    fn path(&mut self, file: usize, module: ModuleId, path: &hir::PathRef) {
        for arg in &path.args {
            match arg {
                hir::GenericArg::Ty(ty) => self.ty(file, module, ty),
                hir::GenericArg::Const(expr) => self.expr(*expr, file, module, None),
            }
        }
        if let Some(sig) = &path.fn_sig {
            for ty in &sig.params {
                self.ty(file, module, ty);
            }
            if let Some(ret) = &sig.ret {
                self.ty(file, module, ret);
            }
            if let Some(error) = &sig.error {
                self.ty(file, module, error);
            }
        }
        if path.segments.is_empty() {
            return;
        }
        let mut current = Some(module);
        for (index, (name, span)) in path.segments.iter().enumerate() {
            if index + 1 == path.segments.len() {
                match path.res {
                    Res::Def(def) => self.occurrences.push(Occurrence {
                        file,
                        span: *span,
                        target: Target::Def(def),
                    }),
                    Res::Local(local) => self.occurrences.push(Occurrence {
                        file,
                        span: *span,
                        target: Target::Local(local),
                    }),
                    Res::Poison => {}
                }
            } else if let Some(module) = current {
                match self.context.module_target(module, name) {
                    Some(target) => {
                        self.occurrences.push(Occurrence {
                            file,
                            span: *span,
                            target: Target::Def(self.context.sema.module(target).def),
                        });
                        current = Some(target);
                    }
                    None => current = None,
                }
            }
        }
    }

    fn ty(&mut self, file: usize, module: ModuleId, ty: &hir::Ty) {
        match &ty.kind {
            hir::TyKind::Path(path) => self.path(file, module, path),
            hir::TyKind::Borrow { inner, .. }
            | hir::TyKind::Slice { elem: inner, .. }
            | hir::TyKind::Ptr { inner, .. }
            | hir::TyKind::Optional(inner) => self.ty(file, module, inner),
            hir::TyKind::Array { len, elem } => {
                self.expr(*len, file, module, None);
                self.ty(file, module, elem);
            }
            hir::TyKind::Tuple(types) => {
                for ty in types {
                    self.ty(file, module, ty);
                }
            }
            hir::TyKind::Fn {
                params, ret, error, ..
            } => {
                for ty in params {
                    self.ty(file, module, ty);
                }
                self.ty(file, module, ret);
                if let Some(error) = error {
                    self.ty(file, module, error);
                }
            }
            hir::TyKind::Dyn { interface, .. } => self.path(file, module, interface),
            hir::TyKind::Poison => {}
        }
    }

    fn block(&mut self, id: hir::BlockId, file: usize, module: ModuleId, body: Option<&TypedBody>) {
        let pkg = self.context.checked_package();
        for stmt in &pkg.block(id).stmts {
            self.stmt(*stmt, file, module, body);
        }
    }

    fn stmt(&mut self, id: hir::StmtId, file: usize, module: ModuleId, body: Option<&TypedBody>) {
        let pkg = self.context.checked_package();
        match &pkg.stmt(id).kind {
            hir::StmtKind::Let {
                pattern, ty, init, ..
            } => {
                if let Some(ty) = ty {
                    self.ty(file, module, ty);
                }
                self.pat(*pattern, file, module, body);
                self.expr(*init, file, module, body);
            }
            hir::StmtKind::Assign { place, value, .. } => {
                self.expr(*place, file, module, body);
                self.expr(*value, file, module, body);
            }
            hir::StmtKind::Discard(expr)
            | hir::StmtKind::Expr(expr)
            | hir::StmtKind::Fail(expr) => self.expr(*expr, file, module, body),
            hir::StmtKind::Return(Some(expr)) => self.expr(*expr, file, module, body),
            hir::StmtKind::Return(None)
            | hir::StmtKind::Break { .. }
            | hir::StmtKind::Continue { .. } => {}
            hir::StmtKind::Defer { body: block, .. } => self.block(*block, file, module, body),
            hir::StmtKind::Block(block) => self.block(*block, file, module, body),
            hir::StmtKind::If {
                cond,
                then,
                otherwise,
            } => {
                if let Some(pattern) = cond.let_pattern {
                    self.pat(pattern, file, module, body);
                }
                self.expr(cond.expr, file, module, body);
                self.block(*then, file, module, body);
                if let Some(otherwise) = otherwise {
                    self.stmt(*otherwise, file, module, body);
                }
            }
            hir::StmtKind::While {
                cond, body: block, ..
            } => {
                if let Some(pattern) = cond.let_pattern {
                    self.pat(pattern, file, module, body);
                }
                self.expr(cond.expr, file, module, body);
                self.block(*block, file, module, body);
            }
            hir::StmtKind::Loop { body: block, .. } => self.block(*block, file, module, body),
            hir::StmtKind::For {
                grain,
                vars,
                index,
                iter,
                range_end,
                step,
                body: block,
                ..
            } => {
                if let Some(grain) = grain {
                    self.expr(*grain, file, module, body);
                }
                for var in vars.iter().chain(index.iter()) {
                    if let Some(local) = var.local {
                        self.occurrences.push(Occurrence {
                            file,
                            span: var.span,
                            target: Target::Local(local),
                        });
                    }
                }
                self.expr(*iter, file, module, body);
                if let Some((_, end)) = range_end {
                    self.expr(*end, file, module, body);
                }
                if let Some(step) = step {
                    self.expr(*step, file, module, body);
                }
                self.block(*block, file, module, body);
            }
            hir::StmtKind::Match { scrutinee, arms } => {
                self.expr(*scrutinee, file, module, body);
                self.arms(arms, file, module, body);
            }
            hir::StmtKind::Scope {
                local, body: block, ..
            } => {
                self.occurrences.push(Occurrence {
                    file,
                    span: pkg.local(*local).span,
                    target: Target::Local(*local),
                });
                self.block(*block, file, module, body);
            }
            hir::StmtKind::Spawn { call, scope } => {
                self.expr(*call, file, module, body);
                self.expr(*scope, file, module, body);
            }
            hir::StmtKind::Dispatch {
                call,
                domain,
                executor,
            } => {
                self.expr(*call, file, module, body);
                self.expr(*domain, file, module, body);
                self.expr(*executor, file, module, body);
            }
            hir::StmtKind::Select { arms } => {
                for arm in arms {
                    self.pat(arm.pattern, file, module, body);
                    if let Some(call) = arm.call {
                        self.expr(call, file, module, body);
                    }
                    self.block(arm.body, file, module, body);
                }
            }
            hir::StmtKind::Unsafe { body: block, .. } => self.block(*block, file, module, body),
            hir::StmtKind::Poison => {}
        }
    }

    fn arms(&mut self, arms: &[hir::Arm], file: usize, module: ModuleId, body: Option<&TypedBody>) {
        for arm in arms {
            for pattern in &arm.patterns {
                self.pat(*pattern, file, module, body);
            }
            if let Some(guard) = arm.guard {
                self.expr(guard, file, module, body);
            }
            match arm.body {
                hir::ArmBody::Block(block) => self.block(block, file, module, body),
                hir::ArmBody::Expr(expr) => self.expr(expr, file, module, body),
            }
        }
    }

    fn pat(&mut self, id: PatId, file: usize, module: ModuleId, body: Option<&TypedBody>) {
        let pkg = self.context.checked_package();
        let pat = pkg.pat(id);
        match &pat.kind {
            hir::PatKind::Wild => {}
            hir::PatKind::Bind { local } => {
                self.occurrences.push(Occurrence {
                    file,
                    span: pkg.local(*local).span,
                    target: Target::Local(*local),
                });
            }
            hir::PatKind::Literal(expr) => self.expr(*expr, file, module, body),
            hir::PatKind::Range { lo, hi, .. } => {
                self.pat(*lo, file, module, body);
                self.pat(*hi, file, module, body);
            }
            hir::PatKind::Variant {
                ty,
                name_span,
                payload,
                ..
            } => {
                if let Some(ty) = ty {
                    self.path(file, module, ty);
                }
                if let Some(info) = body.and_then(|body| body.pat(id))
                    && let PatRes::Variant(variant) = &info.res
                    && let Some(def) = variant.def
                {
                    self.occurrences.push(Occurrence {
                        file,
                        span: *name_span,
                        target: Target::Def(def),
                    });
                }
                if let Some(payload) = payload {
                    self.pat(*payload, file, module, body);
                }
            }
            hir::PatKind::Tuple(inner) => {
                for pat in inner {
                    self.pat(*pat, file, module, body);
                }
            }
            hir::PatKind::Struct { ty, fields, .. } => {
                if let Some(ty) = ty {
                    self.path(file, module, ty);
                }
                let order = body
                    .and_then(|body| body.pat(id))
                    .and_then(|info| match &info.res {
                        PatRes::Struct { res, .. } => Some(res.clone()),
                        _ => None,
                    });
                for (index, field) in fields.iter().enumerate() {
                    if let Some(res) = &order
                        && let Some(field_index) = res.order.get(index)
                        && let Some(sig) = self.context.declarations.struct_sig(res.def)
                        && let Some(field_sig) = sig.fields.get(*field_index)
                    {
                        self.occurrences.push(Occurrence {
                            file,
                            span: field.name_span,
                            target: Target::Def(field_sig.def),
                        });
                    }
                    self.pat(field.pattern, file, module, body);
                }
            }
            hir::PatKind::Poison => {}
        }
    }

    fn expr(&mut self, id: ExprId, file: usize, module: ModuleId, body: Option<&TypedBody>) {
        let pkg = self.context.checked_package();
        let expr = pkg.expr(id);
        match &expr.kind {
            hir::ExprKind::Int { .. }
            | hir::ExprKind::Float(_)
            | hir::ExprKind::Str(_)
            | hir::ExprKind::CStr(_)
            | hir::ExprKind::Bool(_)
            | hir::ExprKind::Poison => {}
            hir::ExprKind::Path(path) => self.path(file, module, path),
            hir::ExprKind::Block(block) => self.block(*block, file, module, body),
            hir::ExprKind::Builtin { type_arg, args, .. } => {
                if let Some(ty) = type_arg {
                    self.ty(file, module, ty);
                }
                for arg in args {
                    self.expr(*arg, file, module, body);
                }
            }
            hir::ExprKind::Unary { expr, .. } | hir::ExprKind::Deref { base: expr, .. } => {
                self.expr(*expr, file, module, body);
            }
            hir::ExprKind::Binary { lhs, rhs, .. } => {
                self.expr(*lhs, file, module, body);
                self.expr(*rhs, file, module, body);
            }
            hir::ExprKind::Call { callee, args, .. } => {
                self.expr(*callee, file, module, body);
                for arg in args {
                    self.expr(*arg, file, module, body);
                }
            }
            hir::ExprKind::Index { base, index, .. } => {
                self.expr(*base, file, module, body);
                self.expr(*index, file, module, body);
            }
            hir::ExprKind::Slice { base, lo, hi, .. } => {
                self.expr(*base, file, module, body);
                if let Some(lo) = lo {
                    self.expr(*lo, file, module, body);
                }
                if let Some(hi) = hi {
                    self.expr(*hi, file, module, body);
                }
            }
            hir::ExprKind::Field {
                base, name_span, ..
            } => {
                self.expr(*base, file, module, body);
                if let Some(target) =
                    body.and_then(|body| body.expr(id))
                        .and_then(|info| match &info.res {
                            ExprRes::Field { def, .. } => Some(*def),
                            ExprRes::Variant(variant) => variant.def,
                            ExprRes::Fn(function) => Some(function.def),
                            _ => None,
                        })
                {
                    self.occurrences.push(Occurrence {
                        file,
                        span: *name_span,
                        target: Target::Def(target),
                    });
                }
            }
            hir::ExprKind::TupleField { base, .. } => self.expr(*base, file, module, body),
            hir::ExprKind::TypeApply { base, args } => {
                self.expr(*base, file, module, body);
                for arg in args {
                    match arg {
                        hir::GenericArg::Ty(ty) => self.ty(file, module, ty),
                        hir::GenericArg::Const(expr) => self.expr(*expr, file, module, body),
                    }
                }
            }
            hir::ExprKind::Cast { expr, ty, .. } => {
                self.expr(*expr, file, module, body);
                self.ty(file, module, ty);
            }
            hir::ExprKind::StructLit { ty, .. } => {
                self.path(file, module, ty);
                self.struct_fields(id, file, module, body);
            }
            hir::ExprKind::StructShorthand { .. } => {
                self.struct_fields(id, file, module, body);
            }
            hir::ExprKind::EnumLit {
                ty,
                variant_span,
                payload,
                ..
            } => {
                if let Some(ty) = ty {
                    self.path(file, module, ty);
                }
                if let Some(info) = body.and_then(|body| body.expr(id))
                    && let ExprRes::Variant(variant) = &info.res
                    && let Some(def) = variant.def
                {
                    self.occurrences.push(Occurrence {
                        file,
                        span: *variant_span,
                        target: Target::Def(def),
                    });
                }
                if let Some(payload) = payload {
                    self.expr(*payload, file, module, body);
                }
            }
            hir::ExprKind::ArrayLit {
                len, elem, elems, ..
            } => {
                self.expr(*len, file, module, body);
                self.ty(file, module, elem);
                for elem in elems {
                    self.expr(*elem, file, module, body);
                }
            }
            hir::ExprKind::Range { lo, hi, .. } => {
                self.expr(*lo, file, module, body);
                self.expr(*hi, file, module, body);
            }
            hir::ExprKind::Tuple(items) => {
                for item in items {
                    self.expr(*item, file, module, body);
                }
            }
            hir::ExprKind::Paren(inner) => self.expr(*inner, file, module, body),
            hir::ExprKind::If {
                cond,
                then,
                otherwise,
            } => {
                if let Some(pattern) = cond.let_pattern {
                    self.pat(pattern, file, module, body);
                }
                self.expr(cond.expr, file, module, body);
                self.expr(*then, file, module, body);
                self.expr(*otherwise, file, module, body);
            }
            hir::ExprKind::Match { scrutinee, arms } => {
                self.expr(*scrutinee, file, module, body);
                self.arms(arms, file, module, body);
            }
            hir::ExprKind::Closure(closure) => {
                for capture in &closure.captures {
                    if let Res::Local(local) = capture.outer {
                        self.occurrences.push(Occurrence {
                            file,
                            span: capture.span,
                            target: Target::Local(local),
                        });
                    }
                }
                for param in &closure.params {
                    self.occurrences.push(Occurrence {
                        file,
                        span: param.name_span,
                        target: Target::Local(param.local),
                    });
                    self.ty(file, module, &param.ty);
                }
                self.ty(file, module, &closure.ret);
                if let Some(error) = &closure.error {
                    self.ty(file, module, error);
                }
                self.block(closure.body, file, module, body);
            }
            hir::ExprKind::Catch {
                value,
                binding,
                binding_span,
                handler,
                ..
            } => {
                self.expr(*value, file, module, body);
                if let (Some(local), Some(span)) = (binding, binding_span) {
                    self.occurrences.push(Occurrence {
                        file,
                        span: *span,
                        target: Target::Local(*local),
                    });
                }
                self.handler(handler, file, module, body);
            }
            hir::ExprKind::Orelse { value, handler, .. } => {
                self.expr(*value, file, module, body);
                self.handler(handler, file, module, body);
            }
            hir::ExprKind::Spawn { call, scope } => {
                self.expr(*call, file, module, body);
                self.expr(*scope, file, module, body);
            }
            hir::ExprKind::Dispatch {
                call,
                domain,
                executor,
            } => {
                self.expr(*call, file, module, body);
                self.expr(*domain, file, module, body);
                self.expr(*executor, file, module, body);
            }
            hir::ExprKind::Unsafe { body: inner, .. } => self.expr(*inner, file, module, body),
        }
    }

    fn struct_fields(
        &mut self,
        id: ExprId,
        file: usize,
        module: ModuleId,
        body: Option<&TypedBody>,
    ) {
        let pkg = self.context.checked_package();
        let expr = pkg.expr(id);
        let fields = match &expr.kind {
            hir::ExprKind::StructLit { fields, .. }
            | hir::ExprKind::StructShorthand { fields, .. } => fields,
            _ => return,
        };
        let res = body
            .and_then(|body| body.expr(id))
            .and_then(|info| match &info.res {
                ExprRes::Struct(res) => Some(res.clone()),
                _ => None,
            });
        for (index, field) in fields.iter().enumerate() {
            if let Some(res) = &res
                && let Some(field_index) = res.order.get(index)
                && let Some(sig) = self.context.declarations.struct_sig(res.def)
                && let Some(field_sig) = sig.fields.get(*field_index)
            {
                self.occurrences.push(Occurrence {
                    file,
                    span: field.name_span,
                    target: Target::Def(field_sig.def),
                });
            }
            self.expr(field.value, file, module, body);
        }
    }

    fn handler(
        &mut self,
        handler: &hir::Handler,
        file: usize,
        module: ModuleId,
        body: Option<&TypedBody>,
    ) {
        match handler {
            hir::Handler::Expr(expr) => self.expr(*expr, file, module, body),
            hir::Handler::Block(block) => self.block(*block, file, module, body),
        }
    }
}

impl Context<'_> {
    fn module_target(&self, module: ModuleId, name: &str) -> Option<ModuleId> {
        let data = self.sema.module(module);
        for import in &data.imports {
            if import.name == name
                && let ImportTarget::Module(target) = import.target
            {
                return Some(target);
            }
        }
        if let Some(def) = data
            .values
            .get(name)
            .copied()
            .or_else(|| data.types.get(name).copied())
            && self.sema.def(def).kind == DefKind::Module
        {
            return Some(self.sema.def(def).module);
        }
        None
    }
}
