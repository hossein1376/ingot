//! `ingot api`: the public surface of a package, module or item (tools.md §4).

use crate::commands::Output;
use crate::files::Overlays;
use crate::pipeline::{self, Options};
use ingot_diag::json::Json;
use ingot_diag::source::{Source, Sources, Span};
use ingot_sema::hir;
use ingot_sema::{
    Bound, Declarations, DefId, DefKind, FnSig, ImplOf, ItemSig, Layout, ModuleId, Sema, Tri,
    TyKind, Vis,
};
use ingot_syntax::lexer::lex;
use ingot_syntax::token::{Kw, Punct, TokenKind};
use std::path::Path;
use std::time::Instant;

struct Ctx<'a> {
    sema: &'a Sema,
    packages: &'a [hir::Package],
    sources: &'a Sources,
    declarations: &'a mut Declarations,
    checked: usize,
    pkg: bool,
}

#[derive(Clone, Copy, Debug)]
enum Node {
    Package(usize),
    Module(ModuleId),
    Item(DefId),
}

pub fn api(
    path: Option<&Path>,
    dotted: Option<&str>,
    pkg: bool,
    target: Option<&str>,
    started: Instant,
) -> Output {
    let (load_path, target) = match path {
        Some(path) => (path.to_path_buf(), target.map(str::to_string)),
        None => (std::path::PathBuf::from("."), target.map(str::to_string)),
    };
    let mut options = Options::default();
    if let Some(target) = &target {
        if ingot_sema::Target::by_name(target).is_none() {
            return crate::commands::failure("api", format!("unknown target `{target}`"), started);
        }
        options.target = target.clone();
    }
    options.load.overlays = Overlays::default();
    options.load.cache = crate::cache::default_cache();
    let std_only = path.is_none()
        && dotted.is_some_and(|text| text.split('.').next() == Some("std"))
        && crate::loader::manifest_dir(Path::new("."), &crate::files::Files::default()).is_none();
    let loaded = if std_only {
        pipeline::analyze_std(&options)
    } else {
        pipeline::analyze_path_with(&load_path, &options)
    };
    let mut analysis = match loaded {
        Ok(analysis) => analysis,
        Err(error) => return crate::commands::failure("api", error.to_string(), started),
    };
    if dotted.is_none() && analysis.workspace_root_without_package {
        return crate::commands::failure(
            "api",
            "this workspace root is not a package; pass a member or a dotted path".to_string(),
            started,
        );
    }
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
            "api",
            format!("{} could not be analyzed", load_path.display()),
            started,
        );
    };
    let mut ctx = Ctx {
        sema: &front.resolved.sema,
        packages: &front.resolved.packages,
        sources: &analysis.sources,
        declarations: &mut front.declarations,
        checked: front.checked,
        pkg,
    };
    let node = match dotted {
        Some(text) => match resolve(&ctx, text) {
            Ok(Node::Item(def)) if !visible(&ctx, def) => {
                return crate::commands::failure("api", format!("`{text}` is not public"), started);
            }
            Ok(node) => node,
            Err(message) => return crate::commands::failure("api", message, started),
        },
        None => Node::Package(ctx.checked),
    };
    let defs = collect(&mut ctx, node);
    let mut lines = diagnostics;
    let mut human = diagnostic_human;
    for def in defs {
        if let Some(line) = line(&mut ctx, def) {
            let text = line.to_text();
            if let Some(path) = line.get("path").and_then(Json::as_str) {
                human.push_str(path);
                human.push('\n');
            }
            lines.push(text);
        }
    }
    lines.push(summary(!any_diagnostic, started));
    Output {
        lines,
        human,
        error: None,
        not_implemented: Vec::new(),
        exit: if any_diagnostic { 1 } else { 0 },
    }
}

fn summary(ok: bool, started: Instant) -> String {
    format!(
        "{{\"type\":\"summary\",\"command\":\"api\",\"ok\":{ok},\"protocol\":1,\"ms\":{}}}",
        started.elapsed().as_millis()
    )
}

/// Resolve a dotted path: an exact package or module, else the longest module prefix
/// and the item below it, with one member after an item (`Type.method`).
fn resolve(ctx: &Ctx<'_>, text: &str) -> Result<Node, String> {
    let segments: Vec<&str> = text.split('.').filter(|s| !s.is_empty()).collect();
    if segments.is_empty() {
        return Err("an empty path".to_string());
    }
    if segments.len() == 1
        && let Some(package) = ctx
            .sema
            .packages
            .iter()
            .position(|package| package.name == segments[0])
    {
        return Ok(Node::Package(package));
    }
    if let Some(module) = ctx.sema.module_paths.get(text) {
        return Ok(Node::Module(*module));
    }
    for split in (1..segments.len()).rev() {
        let prefix = segments[..split].join(".");
        let Some(&module) = ctx.sema.module_paths.get(&prefix) else {
            continue;
        };
        let module_data = ctx.sema.module(module);
        let head = segments[split];
        let item = module_data
            .types
            .get(head)
            .or_else(|| module_data.values.get(head))
            .copied();
        let Some(item) = item else {
            continue;
        };
        if segments.len() == split + 1 {
            return Ok(Node::Item(item));
        }
        if segments.len() != split + 2 {
            return Err(format!("`{text}` does not name a module, item or member"));
        }
        if let Some(member) = member(ctx, item, segments[split + 1]) {
            return Ok(Node::Item(member));
        }
        return Err(format!("`{text}` names no member of `{prefix}.{head}`"));
    }
    Err(format!(
        "`{text}` is not a package, module or item in this build"
    ))
}

fn member(ctx: &Ctx<'_>, item: DefId, name: &str) -> Option<DefId> {
    let kind = ctx.sema.def(item).kind;
    if matches!(kind, DefKind::Struct) {
        for implementation in impls_of(ctx, item) {
            for member in impl_members(ctx, implementation) {
                if ctx.sema.def(member).name == name && is_fn(ctx, member) {
                    return Some(member);
                }
            }
        }
    }
    if matches!(kind, DefKind::Enum)
        && let Some(sig) = ctx.declarations.enum_sig(item)
    {
        for variant in &sig.variants {
            if variant.name == name {
                return Some(variant.def);
            }
        }
    }
    if matches!(kind, DefKind::Interface | DefKind::Impl) {
        for member in impl_members(ctx, item) {
            if ctx.sema.def(member).name == name {
                return Some(member);
            }
        }
    }
    None
}

fn is_fn(ctx: &Ctx<'_>, def: DefId) -> bool {
    matches!(ctx.sema.def(def).kind, DefKind::Fn | DefKind::ExternFn)
}

/// The definitions to print, already in output order.
fn collect(ctx: &mut Ctx<'_>, node: Node) -> Vec<DefId> {
    match node {
        Node::Package(package) => {
            let mut modules: Vec<ModuleId> = ctx.sema.packages[package].modules.clone();
            modules.sort_by(|a, b| {
                ctx.sema
                    .module(*a)
                    .path
                    .as_bytes()
                    .cmp(ctx.sema.module(*b).path.as_bytes())
            });
            let mut out = Vec::new();
            for module in modules {
                if ctx.sema.module(module).path == ingot_sema::BUILTIN_MODULE_PATH {
                    continue;
                }
                collect_module(ctx, module, &mut out);
            }
            out
        }
        Node::Module(module) => {
            let mut out = Vec::new();
            collect_module(ctx, module, &mut out);
            out
        }
        Node::Item(item) => collect_item(ctx, item),
    }
}

fn collect_module(ctx: &mut Ctx<'_>, module: ModuleId, out: &mut Vec<DefId>) {
    let mut items = ctx.sema.module(module).items.clone();
    for package in ctx.packages {
        if let Some(found) = package.modules.iter().find(|items| items.module == module) {
            for block in &found.externs {
                items.extend(block.functions.iter().copied());
            }
        }
    }
    items.sort_by_key(|def| ctx.sema.def(*def).span.start);
    for def in items {
        collect_definition(ctx, def, out);
    }
}

fn collect_definition(ctx: &mut Ctx<'_>, def: DefId, out: &mut Vec<DefId>) {
    let kind = ctx.sema.def(def).kind;
    match kind {
        DefKind::Impl => {
            let Some(sig) = ctx.declarations.impl_sig(def) else {
                return;
            };
            let of = sig.of.clone();
            if matches!(of, ImplOf::Inherent) {
                for member in impl_members(ctx, def) {
                    if is_fn(ctx, member) && visible(ctx, member) {
                        out.push(member);
                    }
                }
                return;
            }
            if matches!(of, ImplOf::Interface(..) | ImplOf::Prop(_))
                && target_visible(ctx, sig.target)
            {
                out.push(def);
                for member in impl_members(ctx, def) {
                    if is_fn(ctx, member) && visible(ctx, member) {
                        out.push(member);
                    }
                }
            }
        }
        DefKind::Interface => {
            if !visible(ctx, def) {
                return;
            }
            out.push(def);
            for member in impl_members(ctx, def) {
                if visible(ctx, member) {
                    out.push(member);
                }
            }
        }
        DefKind::Fn
        | DefKind::ExternFn
        | DefKind::Struct
        | DefKind::Enum
        | DefKind::Const
        | DefKind::Static
        | DefKind::TypeAlias
            if visible(ctx, def) =>
        {
            out.push(def);
        }
        _ => {}
    }
}

fn collect_item(ctx: &mut Ctx<'_>, item: DefId) -> Vec<DefId> {
    let kind = ctx.sema.def(item).kind;
    let mut out = vec![item];
    match kind {
        DefKind::Struct | DefKind::Enum => {
            let target = item;
            for implementation in impls_of(ctx, target) {
                let Some(sig) = ctx.declarations.impl_sig(implementation) else {
                    continue;
                };
                let of = sig.of.clone();
                if matches!(of, ImplOf::Interface(..) | ImplOf::Prop(_)) {
                    out.push(implementation);
                }
                for member in impl_members(ctx, implementation) {
                    if is_fn(ctx, member) && visible(ctx, member) {
                        out.push(member);
                    }
                }
            }
        }
        DefKind::Interface => {
            for member in impl_members(ctx, item) {
                if visible(ctx, member) {
                    out.push(member);
                }
            }
        }
        _ => {}
    }
    out
}

fn impls_of(ctx: &Ctx<'_>, target: DefId) -> Vec<DefId> {
    ctx.declarations
        .impls
        .all
        .iter()
        .copied()
        .filter(|implementation| {
            ctx.declarations.impl_sig(*implementation).and_then(|sig| {
                match ctx.declarations.types.kind(sig.target) {
                    ingot_sema::TyKind::Adt(def, _) => Some(*def),
                    _ => None,
                }
            }) == Some(target)
        })
        .collect()
}

/// Interface and impl members in source order.
fn impl_members(ctx: &Ctx<'_>, owner: DefId) -> Vec<DefId> {
    ctx.packages
        .iter()
        .find_map(|package| match package.items.get(&owner) {
            Some(hir::Item::Interface(item)) => Some(item.members.clone()),
            Some(hir::Item::Impl(item)) => Some(item.members.clone()),
            _ => None,
        })
        .unwrap_or_default()
}

/// The visibility that counts for an item: a member of an interface impl has the
/// interface's (05 §33: its visibility is the interface's).
fn effective_vis(ctx: &Ctx<'_>, def: DefId) -> Vis {
    let data = ctx.sema.def(def);
    let Some(parent) = data.parent else {
        return data.vis;
    };
    match ctx.sema.def(parent).kind {
        DefKind::Interface => ctx.sema.def(parent).vis,
        DefKind::Impl => match ctx.declarations.impl_sig(parent).map(|sig| sig.of.clone()) {
            Some(ImplOf::Interface(interface, _)) => ctx.sema.def(interface).vis,
            _ => data.vis,
        },
        _ => data.vis,
    }
}

fn visible(ctx: &Ctx<'_>, def: DefId) -> bool {
    match effective_vis(ctx, def) {
        Vis::Pub => true,
        Vis::Pkg => ctx.pkg,
        Vis::Private => false,
    }
}

fn target_visible(ctx: &Ctx<'_>, target: ingot_sema::TypeId) -> bool {
    match ctx.declarations.types.kind(target) {
        ingot_sema::TyKind::Adt(def, _) => visible(ctx, *def),
        _ => true,
    }
}

fn line(ctx: &mut Ctx<'_>, def: DefId) -> Option<Json> {
    let data = ctx.sema.def(def);
    let kind = data.kind;
    let item = item_name(kind)?;
    let path = def_path(ctx, def)?;
    let mut members = vec![
        ("type".to_string(), Json::String("api".to_string())),
        ("path".to_string(), Json::String(path)),
        ("item".to_string(), Json::String(item.to_string())),
    ];
    let signature = signature(ctx, def);
    if let Some(signature) = &signature {
        members.push(("signature".to_string(), Json::String(signature.clone())));
    }
    let doc = doc(ctx, def);
    if let Some(doc) = doc {
        members.push(("doc".to_string(), Json::String(doc)));
    }
    if let Some(file) = data.file {
        let span = if matches!(kind, DefKind::Impl) {
            ctx.declarations
                .impl_sig(def)
                .map(|sig| sig.header)
                .unwrap_or(data.span)
        } else {
            data.span
        };
        let mut text = String::new();
        let source = &ctx.sources.get(file).path;
        ingot_diag::diagnostic::write_span(&mut text, ctx.sources, source, span);
        members.push(("span".to_string(), ingot_diag::json::parse(&text).ok()?));
    }
    let mut modifiers = Vec::new();
    if effective_vis(ctx, def) == Vis::Pkg {
        modifiers.push("pub(pkg)".to_string());
    }
    match kind {
        DefKind::Fn | DefKind::ExternFn => {
            let sig = ctx.declarations.fn_sig(def)?.clone();
            members.push(("generics".to_string(), generics(ctx, def)));
            members.push(("params".to_string(), params(ctx, def, &sig)));
            members.push(("result".to_string(), result(ctx, &sig)));
            for modifier in fn_modifiers(&sig, sig.mods.public) {
                if !modifiers.contains(&modifier) {
                    modifiers.push(modifier);
                }
            }
        }
        DefKind::Struct => {
            let sig = ctx.declarations.struct_sig(def)?.clone();
            let ty = self_type(ctx, def);
            if ctx.declarations.generics_of(def).is_empty() {
                members.push(("properties".to_string(), properties(ctx, ty)));
            }
            if let Some(layout) = concrete_layout(ctx, def, ty) {
                members.push(("size".to_string(), Json::Number(layout.size.to_string())));
                members.push(("align".to_string(), Json::Number(layout.align.to_string())));
                members.push((
                    "fields".to_string(),
                    struct_fields(ctx, &sig, Some(&layout)),
                ));
            } else {
                members.push(("fields".to_string(), struct_fields(ctx, &sig, None)));
            }
        }
        DefKind::Enum => {
            let sig = ctx.declarations.enum_sig(def)?.clone();
            let ty = self_type(ctx, def);
            if ctx.declarations.generics_of(def).is_empty() {
                members.push(("properties".to_string(), properties(ctx, ty)));
            }
            if let Some(layout) = concrete_layout(ctx, def, ty) {
                members.push(("size".to_string(), Json::Number(layout.size.to_string())));
                members.push(("align".to_string(), Json::Number(layout.align.to_string())));
            }
            members.push((
                "tag_type".to_string(),
                Json::String(sig.tag_ty.name().to_string()),
            ));
            let mut variants = Vec::new();
            for variant in &sig.variants {
                let mut object = vec![("name".to_string(), Json::String(variant.name.clone()))];
                if let Some(payload) = variant.payload {
                    object.push((
                        "payload".to_string(),
                        Json::String(
                            ctx.sema
                                .def(variant.def)
                                .file
                                .and_then(|file| written(ctx, Some(file), variant.payload_span?))
                                .unwrap_or_else(|| {
                                    ctx.declarations.types.display(ctx.sema, payload)
                                }),
                        ),
                    ));
                }
                object.push(("from".to_string(), Json::Bool(variant.from)));
                object.push(("value".to_string(), Json::Number(variant.tag.to_string())));
                variants.push(Json::Object(object));
            }
            members.push(("variants".to_string(), Json::Array(variants)));
        }
        DefKind::Interface => {
            let sig = ctx.declarations.interface_sig(def)?.clone();
            members.push(("generics".to_string(), generics(ctx, def)));
            let mut requires = Vec::new();
            let bounds = match hir_item(ctx, def) {
                Some(hir::Item::Interface(item)) => item.bounds.clone(),
                _ => Vec::new(),
            };
            for (index, bound) in sig.supers.iter().enumerate() {
                requires.push(Json::String(
                    bounds
                        .get(index)
                        .and_then(|path| {
                            let file = ctx.sema.def(def).file?;
                            written(ctx, Some(file), path.span)
                        })
                        .unwrap_or_else(|| bound_text(ctx, bound)),
                ));
            }
            members.push(("requires".to_string(), Json::Array(requires)));
            let mut associated = Vec::new();
            for member in &sig.assoc {
                associated.push(Json::String(ctx.sema.def(*member).name.clone()));
            }
            members.push(("associated_types".to_string(), Json::Array(associated)));
            let mut methods = Vec::new();
            for member in &sig.methods {
                if let Some(path) = def_path(ctx, *member) {
                    methods.push(Json::String(path));
                }
            }
            members.push(("methods".to_string(), Json::Array(methods)));
        }
        DefKind::Impl => {
            let sig = ctx.declarations.impl_sig(def)?.clone();
            let own = hir_item(ctx, def).and_then(|item| match item {
                hir::Item::Impl(item) => Some(item),
                _ => None,
            });
            let file = ctx.sema.def(def).file;
            let interface = match &sig.of {
                ImplOf::Interface(interface, _) => own
                    .and_then(|item| item.interface.as_ref())
                    .and_then(|path| written(ctx, file, path.span))
                    .unwrap_or_else(|| {
                        interface_text(ctx, *interface)
                            .unwrap_or_else(|| ctx.sema.def(*interface).name.clone())
                    }),
                ImplOf::Prop(bound) => own
                    .and_then(|item| item.interface.as_ref())
                    .and_then(|path| written(ctx, file, path.span))
                    .unwrap_or_else(|| bound.name().to_string()),
                ImplOf::Inherent => return None,
            };
            members.push(("interface".to_string(), Json::String(interface)));
            members.push((
                "for".to_string(),
                Json::String(
                    own.and_then(|item| written(ctx, file, item.target.span))
                        .unwrap_or_else(|| ctx.declarations.types.display(ctx.sema, sig.target)),
                ),
            ));
        }
        DefKind::Const => {
            let sig = ctx.declarations.item(def).and_then(|item| match item {
                ItemSig::Const(sig) => Some(sig),
                _ => None,
            })?;
            let span = hir_item(ctx, def).and_then(|item| match item {
                hir::Item::Const(item) => Some(item.ty.span),
                _ => None,
            });
            members.push((
                "value_type".to_string(),
                Json::String(
                    span.and_then(|span| written(ctx, ctx.sema.def(def).file, span))
                        .unwrap_or_else(|| ctx.declarations.types.display(ctx.sema, sig.ty)),
                ),
            ));
        }
        DefKind::Static => {
            let sig = ctx.declarations.item(def).and_then(|item| match item {
                ItemSig::Static(sig) => Some(sig),
                _ => None,
            })?;
            members.push((
                "value_type".to_string(),
                Json::String(
                    written(ctx, ctx.sema.def(def).file, sig.ty_span)
                        .unwrap_or_else(|| ctx.declarations.types.display(ctx.sema, sig.ty)),
                ),
            ));
        }
        DefKind::TypeAlias | DefKind::AssocType => {
            let sig = ctx.declarations.item(def).and_then(|item| match item {
                ItemSig::Alias(sig) => Some(sig),
                _ => None,
            })?;
            members.push(("generics".to_string(), generics(ctx, def)));
            if let Some(ty) = sig.ty {
                let span = hir_item(ctx, def).and_then(|item| match item {
                    hir::Item::Alias(item) => item.ty.as_ref().map(|ty| ty.span),
                    _ => None,
                });
                members.push((
                    "aliases".to_string(),
                    Json::String(
                        span.and_then(|span| written(ctx, ctx.sema.def(def).file, span))
                            .unwrap_or_else(|| ctx.declarations.types.display(ctx.sema, ty)),
                    ),
                ));
            }
        }
        _ => return None,
    }
    if !modifiers.is_empty() {
        members.push((
            "modifiers".to_string(),
            Json::Array(modifiers.into_iter().map(Json::String).collect()),
        ));
    }
    Some(Json::Object(members))
}

fn hir_item<'a>(ctx: &'_ Ctx<'a>, def: DefId) -> Option<&'a hir::Item> {
    ctx.packages
        .iter()
        .find_map(|package| package.items.get(&def))
}

/// A span of source as the declaration wrote it, spaced as `signature` is.
fn written(ctx: &Ctx<'_>, file: Option<usize>, span: Span) -> Option<String> {
    if span.start >= span.end {
        return None;
    }
    let file = file?;
    let text = &ctx.sources.get(file).text;
    let slice = text.get(span.start..span.end)?;
    Some(collapse(slice))
}

fn item_name(kind: DefKind) -> Option<&'static str> {
    Some(match kind {
        DefKind::Fn | DefKind::ExternFn => "fn",
        DefKind::Struct => "struct",
        DefKind::Enum => "enum",
        DefKind::Interface => "interface",
        DefKind::Impl => "impl",
        DefKind::Const => "const",
        DefKind::Static => "static",
        DefKind::TypeAlias | DefKind::AssocType => "type",
        _ => return None,
    })
}

/// The type of the declaration itself: its generic parameters as parameters.
fn self_type(ctx: &mut Ctx<'_>, def: DefId) -> ingot_sema::TypeId {
    let params: Vec<DefId> = ctx
        .declarations
        .generics_of(def)
        .iter()
        .map(|generic| generic.def)
        .collect();
    let args: Vec<ingot_sema::GenericArg> = params
        .into_iter()
        .map(|param| ingot_sema::GenericArg::Ty(ctx.declarations.types.param(param)))
        .collect();
    ctx.declarations
        .types
        .intern(ingot_sema::TyKind::Adt(def, args))
}

fn concrete_layout(ctx: &mut Ctx<'_>, def: DefId, ty: ingot_sema::TypeId) -> Option<Layout> {
    if !ctx.declarations.generics_of(def).is_empty() {
        return None;
    }
    ctx.declarations.layout_of(ty).ok()
}

fn properties(ctx: &mut Ctx<'_>, ty: ingot_sema::TypeId) -> Json {
    let props = ctx.declarations.props(ty);
    Json::Object(vec![
        ("copy".to_string(), Json::Bool(props.copy == Tri::Yes)),
        ("linear".to_string(), Json::Bool(props.linear == Tri::Yes)),
        ("view".to_string(), Json::Bool(props.view == Tri::Yes)),
        ("send".to_string(), Json::Bool(props.send == Tri::Yes)),
        ("sync".to_string(), Json::Bool(props.sync == Tri::Yes)),
    ])
}

fn struct_fields(ctx: &Ctx<'_>, sig: &ingot_sema::StructSig, layout: Option<&Layout>) -> Json {
    let mut out = Vec::new();
    for field in &sig.fields {
        if !field.public && !(ctx.pkg && field.pkg) {
            continue;
        }
        let mut object = vec![
            ("name".to_string(), Json::String(field.name.clone())),
            (
                "type".to_string(),
                Json::String(
                    ctx.sema
                        .def(field.def)
                        .file
                        .and_then(|file| written(ctx, Some(file), field.ty_span))
                        .unwrap_or_else(|| ctx.declarations.types.display(ctx.sema, field.ty)),
                ),
            ),
        ];
        if let Some(layout) = layout
            && let Some(found) = layout.fields.iter().find(|item| item.name == field.name)
        {
            object.push(("offset".to_string(), Json::Number(found.offset.to_string())));
        }
        out.push(Json::Object(object));
    }
    Json::Array(out)
}

fn generics(ctx: &Ctx<'_>, def: DefId) -> Json {
    let mut out = Vec::new();
    for generic in ctx.declarations.generics_of(def) {
        let bounds = generic
            .bounds
            .iter()
            .enumerate()
            .map(|(index, bound)| {
                let file = ctx.sema.def(def).file;
                Json::String(
                    generic
                        .bound_spans
                        .get(index)
                        .and_then(|span| written(ctx, file, *span))
                        .unwrap_or_else(|| bound_text(ctx, bound)),
                )
            })
            .collect();
        out.push(Json::Object(vec![
            ("name".to_string(), Json::String(generic.name.clone())),
            ("bounds".to_string(), Json::Array(bounds)),
        ]));
    }
    Json::Array(out)
}

fn params(ctx: &Ctx<'_>, def: DefId, sig: &FnSig) -> Json {
    let mut out = Vec::new();
    for param in &sig.params {
        let mut object = vec![
            ("name".to_string(), Json::String(param.name.clone())),
            (
                "type".to_string(),
                Json::String(
                    written(ctx, ctx.sema.def(def).file, param.ty_span)
                        .unwrap_or_else(|| ctx.declarations.types.display(ctx.sema, param.ty)),
                ),
            ),
        ];
        if let Some((_, Some(index))) = param.into
            && let Some(target) = sig.params.get(index)
        {
            object.push(("into".to_string(), Json::String(target.name.clone())));
        }
        out.push(Json::Object(object));
    }
    Json::Array(out)
}

fn result(ctx: &Ctx<'_>, sig: &FnSig) -> Json {
    let file = ctx.sema.def(sig.def).file;
    let return_type = if matches!(ctx.declarations.types.kind(sig.ret), TyKind::Void) {
        "void".to_string()
    } else {
        written(ctx, file, sig.ret_span)
            .unwrap_or_else(|| ctx.declarations.types.display(ctx.sema, sig.ret))
    };
    let mut object = vec![("type".to_string(), Json::String(return_type))];
    if let Some(error) = sig.error {
        object.push((
            "error".to_string(),
            Json::String(
                sig.error_span
                    .and_then(|span| written(ctx, file, span))
                    .unwrap_or_else(|| ctx.declarations.types.display(ctx.sema, error)),
            ),
        ));
    }
    if !sig.from.is_empty() {
        let from = sig
            .from
            .iter()
            .map(|origin| {
                let mut text = if origin.is_static {
                    "static".to_string()
                } else {
                    origin.name.clone()
                };
                if origin.inner {
                    text.push_str(".*");
                }
                Json::String(text)
            })
            .collect();
        object.push(("from".to_string(), Json::Array(from)));
    }
    if let Some(reduce) = &sig.reduce {
        object.push(("reduce".to_string(), Json::String(reduce.clone())));
    }
    Json::Object(object)
}

fn fn_modifiers(sig: &FnSig, public: bool) -> Vec<String> {
    let mut out = Vec::new();
    if public {
        out.push("pub".to_string());
    }
    if sig.mods.unsafe_ {
        out.push("unsafe".to_string());
    }
    if sig.mods.inline {
        out.push("inline".to_string());
    }
    if sig.mods.noinline {
        out.push("noinline".to_string());
    }
    if sig.mods.export {
        out.push("export".to_string());
    }
    if sig.mods.kernel {
        out.push("kernel".to_string());
    }
    if sig.mods.waitable {
        out.push("waitable".to_string());
    }
    out
}

fn bound_text(ctx: &Ctx<'_>, bound: &Bound) -> String {
    match bound {
        Bound::Prop(prop) => prop.name().to_string(),
        Bound::Fn(kind, sig) => format!(
            "{}{}",
            kind.name(),
            ctx.declarations.types.display_sig(ctx.sema, sig)
        ),
        Bound::Interface(def, args) => {
            let mut out =
                interface_text(ctx, *def).unwrap_or_else(|| ctx.sema.def(*def).name.clone());
            if !args.is_empty() {
                out.push('[');
                for (i, arg) in args.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    out.push_str(&match arg {
                        ingot_sema::GenericArg::Ty(ty) => {
                            ctx.declarations.types.display(ctx.sema, *ty)
                        }
                        ingot_sema::GenericArg::Const(value) => format!("{value:?}"),
                    });
                }
                out.push(']');
            }
            out
        }
        Bound::Poison => "?".to_string(),
    }
}

fn interface_text(ctx: &Ctx<'_>, def: DefId) -> Option<String> {
    def_path(ctx, def)
}

fn def_path(ctx: &Ctx<'_>, def: DefId) -> Option<String> {
    let data = ctx.sema.def(def);
    match data.kind {
        DefKind::Module => Some(ctx.sema.module(data.module).path.clone()),
        DefKind::Impl => {
            let sig = ctx.declarations.impl_sig(def)?;
            target_path(ctx, sig.target)
        }
        DefKind::GenericParam { .. } | DefKind::SelfTy => Some(data.name.clone()),
        DefKind::Test { .. } => Some(format!(
            "{}.{}",
            ctx.sema.module(data.module).path,
            data.name
        )),
        _ => match data.parent {
            Some(parent) => {
                let parent = match ctx.sema.def(parent).kind {
                    DefKind::Impl => {
                        let sig = ctx.declarations.impl_sig(parent)?;
                        return Some(format!("{}.{}", target_path(ctx, sig.target)?, data.name));
                    }
                    _ => parent,
                };
                Some(format!("{}.{}", def_path(ctx, parent)?, data.name))
            }
            None => Some(format!(
                "{}.{}",
                ctx.sema.module(data.module).path,
                data.name
            )),
        },
    }
}

fn target_path(ctx: &Ctx<'_>, target: ingot_sema::TypeId) -> Option<String> {
    match ctx.declarations.types.kind(target) {
        ingot_sema::TyKind::Adt(def, _) => def_path(ctx, *def),
        _ => Some(ctx.declarations.types.display(ctx.sema, target)),
    }
}

fn doc(ctx: &Ctx<'_>, def: DefId) -> Option<String> {
    let file = ctx.sema.def(def).file?;
    let source = ctx.sources.get(file);
    let span = ctx
        .packages
        .iter()
        .find_map(|package| package.items.get(&def).map(item_span))?;
    crate::query::doc_comment(&source.text, declaration_start(ctx, def, span))
}

fn item_span(item: &hir::Item) -> Span {
    match item {
        hir::Item::Fn(item) => item.span,
        hir::Item::Struct(item) => item.span,
        hir::Item::Enum(item) => item.span,
        hir::Item::Interface(item) => item.span,
        hir::Item::Impl(item) => item.span,
        hir::Item::Const(item) => item.span,
        hir::Item::Static(item) => item.span,
        hir::Item::Alias(item) => item.span,
        hir::Item::Test(item) => item.span,
    }
}

/// The first token of a declaration, including modifiers that a member's HIR span
/// leaves out (`pub(pkg) unsafe fn`, `type Item`).
fn declaration_start(ctx: &Ctx<'_>, def: DefId, span: Span) -> usize {
    let Some(file) = ctx.sema.def(def).file else {
        return span.start;
    };
    let text = &ctx.sources.get(file).text;
    let source = Source::new("<api>", text);
    let (tokens, _) = lex(&source);
    let Some(mut index) = tokens
        .iter()
        .position(|token| token.span.start == span.start)
        .or_else(|| {
            tokens
                .iter()
                .position(|token| token.span.start <= span.start && span.start < token.span.end)
        })
    else {
        return span.start;
    };
    loop {
        if index == 0 {
            break;
        }
        let previous = &tokens[index - 1].kind;
        let modifier = matches!(
            previous,
            TokenKind::Keyword(
                Kw::Inline
                    | Kw::Noinline
                    | Kw::Unsafe
                    | Kw::Export
                    | Kw::Kernel
                    | Kw::Waitable
                    | Kw::Pub
                    | Kw::Type
            )
        );
        if modifier {
            index -= 1;
            continue;
        }
        if index >= 4
            && tokens[index - 1].kind.is_punct(Punct::RParen)
            && matches!(&tokens[index - 2].kind, TokenKind::Ident(name) if name == "pkg")
            && tokens[index - 3].kind.is_punct(Punct::LParen)
            && tokens[index - 4].kind.is_kw(Kw::Pub)
        {
            index -= 4;
            continue;
        }
        break;
    }
    tokens[index].span.start
}

/// The declaration's tokens from its first token to the header's `{` or `;`: comments
/// dropped, white space collapsed to one space (tools.md §4).
fn signature(ctx: &Ctx<'_>, def: DefId) -> Option<String> {
    let file = ctx.sema.def(def).file?;
    let text = &ctx.sources.get(file).text;
    let span = ctx
        .packages
        .iter()
        .find_map(|package| package.items.get(&def).map(item_span))?;
    let start = declaration_start(ctx, def, span);
    let end = header_end(text, start, span.end)?;
    Some(collapse(&text[start..end]))
}

fn header_end(text: &str, start: usize, limit: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let limit = limit.min(bytes.len());
    let mut i = start;
    let mut depth = 0usize;
    while i < limit {
        match bytes[i] {
            b'(' | b'[' => depth += 1,
            b')' | b']' => depth = depth.saturating_sub(1),
            b'{' if depth == 0 => return Some(i),
            b';' if depth == 0 => return Some(i),
            b'"' => {
                i += 1;
                while i < limit {
                    if bytes[i] == b'\\' {
                        i += 2;
                        continue;
                    }
                    if bytes[i] == b'"' {
                        break;
                    }
                    i += 1;
                }
            }
            b'\'' => {
                i += 1;
                while i < limit {
                    if bytes[i] == b'\\' {
                        i += 2;
                        continue;
                    }
                    if bytes[i] == b'\'' {
                        break;
                    }
                    i += 1;
                }
            }
            b'/' if i + 1 < limit && bytes[i + 1] == b'/' => {
                while i < limit && bytes[i] != b'\n' {
                    i += 1;
                }
                continue;
            }
            b'/' if i + 1 < limit && bytes[i + 1] == b'*' => {
                i += 2;
                while i + 1 < limit && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                    i += 1;
                }
                i += 1;
            }
            _ => {}
        }
        i += 1;
    }
    None
}

fn collapse(slice: &str) -> String {
    let chars: Vec<char> = slice.chars().collect();
    let mut out = String::new();
    let mut i = 0usize;
    let mut space = false;
    while i < chars.len() {
        let character = chars[i];
        if character.is_whitespace() {
            space = true;
            i += 1;
            continue;
        }
        if character == '/' && chars.get(i + 1) == Some(&'/') {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
            space = true;
            continue;
        }
        if character == '/' && chars.get(i + 1) == Some(&'*') {
            i += 2;
            while i + 1 < chars.len() && !(chars[i] == '*' && chars[i + 1] == '/') {
                i += 1;
            }
            i += 2;
            space = true;
            continue;
        }
        if space && !out.is_empty() {
            out.push(' ');
        }
        space = false;
        if character == '"' || character == '\'' {
            let quote = character;
            out.push(character);
            i += 1;
            while i < chars.len() {
                let current = chars[i];
                out.push(current);
                if current == '\\' && i + 1 < chars.len() {
                    i += 1;
                    out.push(chars[i]);
                } else if current == quote {
                    i += 1;
                    break;
                }
                i += 1;
            }
            continue;
        }
        out.push(character);
        i += 1;
    }
    let mut cleaned = String::new();
    let chars: Vec<char> = out.chars().collect();
    let mut j = 0usize;
    while j < chars.len() {
        let character = chars[j];
        if character == ' '
            && let Some(previous) = cleaned.chars().last()
            && matches!(previous, '(' | '[')
        {
            j += 1;
            continue;
        }
        if character == ' '
            && chars
                .get(j + 1)
                .is_some_and(|next| matches!(next, ')' | ']' | ','))
        {
            j += 1;
            continue;
        }
        cleaned.push(character);
        j += 1;
    }
    cleaned.trim().to_string()
}
