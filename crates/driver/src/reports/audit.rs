//! `ingot audit`: every `unsafe` occurrence of the build (tools.md §4).

use crate::commands::Output;
use crate::files::Overlays;
use crate::pipeline::{self, Analysis, Options};
use ingot_diag::diagnostic::json_string;
use ingot_diag::source::{Sources, Span};
use ingot_sema::hir;
use ingot_sema::{Declarations, DefId, DefKind, ImplOf, ItemSig, Sema, TyKind, TypeId};
use std::path::Path;
use std::time::Instant;

struct Ctx<'a> {
    sema: &'a Sema,
    sources: &'a Sources,
    declarations: &'a mut Declarations,
    packages: &'a [hir::Package],
}

#[derive(Clone, Debug)]
struct Occurrence {
    file: String,
    span: Span,
    kind: &'static str,
    function: Option<String>,
    extra: Vec<(&'static str, String)>,
}

pub fn audit(path: Option<&Path>, started: Instant) -> Output {
    let std_only = path.is_some_and(|path| path.to_string_lossy() == "std");
    let load_path = path
        .filter(|_| !std_only)
        .map(Path::to_path_buf)
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    let mut options = Options::default();
    options.load.overlays = Overlays::default();
    options.load.cache = crate::cache::default_cache();
    let loaded = if std_only {
        pipeline::analyze_std(&options)
    } else {
        pipeline::analyze_path_with(&load_path, &options)
    };
    let mut analysis: Analysis = match loaded {
        Ok(analysis) => analysis,
        Err(error) => return crate::commands::failure("audit", error.to_string(), started),
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
            "audit",
            format!("{} could not be analyzed", load_path.display()),
            started,
        );
    };
    let mut ctx = Ctx {
        sema: &front.resolved.sema,
        sources: &analysis.sources,
        declarations: &mut front.declarations,
        packages: &front.resolved.packages,
    };
    let checked: Vec<usize> = if std_only {
        vec![front.checked]
    } else {
        front.checked_packages.clone()
    };
    let mut found = Vec::new();
    for index in checked {
        collect_package(&mut ctx, index, &mut found);
    }
    found.sort_by(|a, b| {
        a.file
            .as_bytes()
            .cmp(b.file.as_bytes())
            .then(a.span.start.cmp(&b.span.start))
            .then(a.span.end.cmp(&b.span.end))
            .then(a.kind.cmp(b.kind))
    });
    let mut lines = diagnostics;
    let mut human = diagnostic_human;
    for occurrence in &found {
        let mut line = String::from("{\"type\":\"audit\",\"kind\":");
        json_string(&mut line, occurrence.kind);
        if let Some(function) = &occurrence.function {
            line.push_str(",\"fn\":");
            json_string(&mut line, function);
        }
        line.push_str(",\"span\":");
        ingot_diag::diagnostic::write_span(
            &mut line,
            ctx.sources,
            &occurrence.file,
            occurrence.span,
        );
        for (name, value) in &occurrence.extra {
            line.push(',');
            json_string(&mut line, name);
            line.push(':');
            json_string(&mut line, value);
        }
        line.push('}');
        lines.push(line);
        human.push_str(&format!(
            "{} {}:{}",
            occurrence.kind,
            occurrence.file,
            ctx.sources
                .iter()
                .find(|source| source.path == occurrence.file)
                .map(|source| source.position(occurrence.span.start).line)
                .unwrap_or(0)
        ));
        if let Some(function) = &occurrence.function {
            human.push_str(&format!(" in {function}"));
        }
        human.push('\n');
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
        "{{\"type\":\"summary\",\"command\":\"audit\",\"ok\":{ok},\"protocol\":1,\"ms\":{}}}",
        started.elapsed().as_millis()
    )
}

fn collect_package(ctx: &mut Ctx<'_>, index: usize, found: &mut Vec<Occurrence>) {
    let package = &ctx.packages[index];
    let definitions: Vec<DefId> = package.items.keys().copied().collect();
    let cell = cell_def(ctx);
    for def in definitions {
        let Some(item) = package.items.get(&def) else {
            continue;
        };
        match item {
            hir::Item::Fn(function) => {
                if function.mods.unsafe_ && function.abi.is_none() {
                    occurrence(
                        ctx,
                        def,
                        function.name_span,
                        "unsafe_fn",
                        None,
                        Vec::new(),
                        found,
                    );
                }
                if function.abi.is_some() {
                    occurrence(
                        ctx,
                        def,
                        function.name_span,
                        "extern",
                        None,
                        vec![
                            ("abi", function.abi.clone().unwrap_or_default()),
                            ("symbol", function.name.clone()),
                        ],
                        found,
                    );
                }
                if function.mods.export {
                    occurrence(
                        ctx,
                        def,
                        function.name_span,
                        "export",
                        None,
                        vec![("symbol", function.name.clone())],
                        found,
                    );
                }
                if let Some(body) = function.body {
                    let function_path = def_path(ctx, def);
                    let file = ctx
                        .sema
                        .def(def)
                        .file
                        .map(|file| ctx.sources.get(file).path.clone())
                        .unwrap_or_default();
                    walk_block(ctx, package, body, function_path.as_deref(), &file, found);
                }
            }
            hir::Item::Test(test) => {
                let function = test_path(ctx, def, &test.name);
                let file = ctx
                    .sema
                    .def(def)
                    .file
                    .map(|file| ctx.sources.get(file).path.clone())
                    .unwrap_or_default();
                walk_block(ctx, package, test.body, Some(&function), &file, found);
            }
            hir::Item::Impl(item) => {
                if item.unsafe_
                    && let Some(sig) = ctx.declarations.impl_sig(def)
                {
                    let interface = match &sig.of {
                        ImplOf::Interface(interface, _) => def_path(ctx, *interface)
                            .unwrap_or_else(|| ctx.sema.def(*interface).name.clone()),
                        ImplOf::Prop(bound) => bound.name().to_string(),
                        ImplOf::Inherent => ctx.sema.def(def).name.clone(),
                    };
                    let target = ctx.declarations.types.display(ctx.sema, sig.target);
                    occurrence(
                        ctx,
                        def,
                        item.span,
                        "unsafe_impl",
                        None,
                        vec![("interface", interface), ("for", target)],
                        found,
                    );
                }
            }
            hir::Item::Static(_) => {
                let static_ty = match ctx.declarations.item(def) {
                    Some(ItemSig::Static(sig)) => sig.ty,
                    _ => continue,
                };
                if cell.is_some_and(|cell| contains_cell(ctx, static_ty, cell, &mut Vec::new())) {
                    let value = ctx.declarations.types.display(ctx.sema, static_ty);
                    let name_span = ctx.sema.def(def).span;
                    occurrence(
                        ctx,
                        def,
                        name_span,
                        "static_interior",
                        None,
                        vec![("value_type", value)],
                        found,
                    );
                }
            }
            _ => {}
        }
    }
}

fn occurrence(
    ctx: &Ctx<'_>,
    def: DefId,
    span: Span,
    kind: &'static str,
    function: Option<String>,
    extra: Vec<(&'static str, String)>,
    found: &mut Vec<Occurrence>,
) {
    let Some(file) = ctx.sema.def(def).file else {
        return;
    };
    let path = ctx.sources.get(file).path.clone();
    found.push(Occurrence {
        file: path,
        span,
        kind,
        function,
        extra,
    });
}

fn test_path(ctx: &Ctx<'_>, def: DefId, name: &str) -> String {
    format!(
        "{}.{}",
        ctx.sema.module(ctx.sema.def(def).module).path,
        name
    )
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
            Some(parent) => match ctx.sema.def(parent).kind {
                DefKind::Impl => {
                    let sig = ctx.declarations.impl_sig(parent)?;
                    Some(format!("{}.{}", target_path(ctx, sig.target)?, data.name))
                }
                _ => Some(format!("{}.{}", def_path(ctx, parent)?, data.name)),
            },
            None => Some(format!(
                "{}.{}",
                ctx.sema.module(data.module).path,
                data.name
            )),
        },
    }
}

fn target_path(ctx: &Ctx<'_>, target: TypeId) -> Option<String> {
    match ctx.declarations.types.kind(target) {
        TyKind::Adt(def, _) => def_path(ctx, *def),
        _ => Some(ctx.declarations.types.display(ctx.sema, target)),
    }
}

fn cell_def(ctx: &Ctx<'_>) -> Option<DefId> {
    let module = ctx.sema.module_paths.get("std.sync")?;
    ctx.sema.module(*module).types.get("Cell").copied()
}

/// True when the type contains `std.sync.Cell` by value (glossary, "interior
/// mutability"). Borrows, pointers and function types stop the walk.
fn contains_cell(ctx: &mut Ctx<'_>, ty: TypeId, cell: DefId, seen: &mut Vec<TypeId>) -> bool {
    if seen.contains(&ty) {
        return false;
    }
    seen.push(ty);
    match ctx.declarations.types.kind(ty).clone() {
        TyKind::Adt(def, args) => {
            if def == cell {
                return true;
            }
            let subst = ctx.declarations.subst_for(def, &args);
            match ctx.declarations.item(def).cloned() {
                Some(ItemSig::Struct(sig)) => sig.fields.iter().any(|field| {
                    let field_ty = ctx.declarations.types.subst(field.ty, &subst);
                    contains_cell(ctx, field_ty, cell, seen)
                }),
                Some(ItemSig::Enum(sig)) => sig.variants.iter().any(|variant| {
                    variant.payload.is_some_and(|payload| {
                        let payload = ctx.declarations.types.subst(payload, &subst);
                        contains_cell(ctx, payload, cell, seen)
                    })
                }),
                Some(ItemSig::Alias(sig)) => sig.ty.is_some_and(|target| {
                    let target = ctx.declarations.types.subst(target, &subst);
                    contains_cell(ctx, target, cell, seen)
                }),
                _ => false,
            }
        }
        TyKind::Array(element, _) | TyKind::Optional(element) => {
            contains_cell(ctx, element, cell, seen)
        }
        TyKind::Result(value, error) => {
            contains_cell(ctx, value, cell, seen) || contains_cell(ctx, error, cell, seen)
        }
        TyKind::Tuple(items) => items
            .into_iter()
            .any(|item| contains_cell(ctx, item, cell, seen)),
        _ => false,
    }
}

fn walk_block(
    ctx: &Ctx<'_>,
    package: &hir::Package,
    block: hir::BlockId,
    function: Option<&str>,
    file: &str,
    found: &mut Vec<Occurrence>,
) {
    for statement in &package.block(block).stmts {
        walk_stmt(ctx, package, *statement, function, file, found);
    }
}

fn walk_stmt(
    ctx: &Ctx<'_>,
    package: &hir::Package,
    statement: hir::StmtId,
    function: Option<&str>,
    file: &str,
    found: &mut Vec<Occurrence>,
) {
    let stmt = package.stmt(statement);
    match &stmt.kind {
        hir::StmtKind::Let { init, pattern, .. } => {
            walk_pattern(ctx, package, *pattern, function, file, found);
            walk_expr(ctx, package, *init, function, file, found);
        }
        hir::StmtKind::Assign { place, value, .. } => {
            walk_expr(ctx, package, *place, function, file, found);
            walk_expr(ctx, package, *value, function, file, found);
        }
        hir::StmtKind::Discard(expr) | hir::StmtKind::Expr(expr) | hir::StmtKind::Fail(expr) => {
            walk_expr(ctx, package, *expr, function, file, found)
        }
        hir::StmtKind::Return(expr) => {
            if let Some(expr) = expr {
                walk_expr(ctx, package, *expr, function, file, found);
            }
        }
        hir::StmtKind::Defer { body, .. } | hir::StmtKind::Block(body) => {
            walk_block(ctx, package, *body, function, file, found)
        }
        hir::StmtKind::If {
            cond,
            then,
            otherwise,
        } => {
            walk_expr(ctx, package, cond.expr, function, file, found);
            walk_block(ctx, package, *then, function, file, found);
            if let Some(otherwise) = otherwise {
                walk_stmt(ctx, package, *otherwise, function, file, found);
            }
        }
        hir::StmtKind::While { cond, body, .. } => {
            walk_expr(ctx, package, cond.expr, function, file, found);
            walk_block(ctx, package, *body, function, file, found);
        }
        hir::StmtKind::Loop { body, .. } => walk_block(ctx, package, *body, function, file, found),
        hir::StmtKind::For {
            iter,
            range_end,
            step,
            body,
            ..
        } => {
            walk_expr(ctx, package, *iter, function, file, found);
            if let Some((_, end)) = range_end {
                walk_expr(ctx, package, *end, function, file, found);
            }
            if let Some(step) = step {
                walk_expr(ctx, package, *step, function, file, found);
            }
            walk_block(ctx, package, *body, function, file, found);
        }
        hir::StmtKind::Match { scrutinee, arms } => {
            walk_expr(ctx, package, *scrutinee, function, file, found);
            for arm in arms {
                for pattern in &arm.patterns {
                    walk_pattern(ctx, package, *pattern, function, file, found);
                }
                if let Some(guard) = arm.guard {
                    walk_expr(ctx, package, guard, function, file, found);
                }
                match &arm.body {
                    hir::ArmBody::Block(block) => {
                        walk_block(ctx, package, *block, function, file, found)
                    }
                    hir::ArmBody::Expr(expr) => {
                        walk_expr(ctx, package, *expr, function, file, found)
                    }
                }
            }
        }
        hir::StmtKind::Scope { body, .. } => walk_block(ctx, package, *body, function, file, found),
        hir::StmtKind::Spawn { call, scope } => {
            walk_expr(ctx, package, *call, function, file, found);
            walk_expr(ctx, package, *scope, function, file, found);
        }
        hir::StmtKind::Dispatch {
            call,
            domain,
            executor,
        } => {
            walk_expr(ctx, package, *call, function, file, found);
            walk_expr(ctx, package, *domain, function, file, found);
            walk_expr(ctx, package, *executor, function, file, found);
        }
        hir::StmtKind::Select { arms } => {
            for arm in arms {
                walk_pattern(ctx, package, arm.pattern, function, file, found);
                if let Some(call) = arm.call {
                    walk_expr(ctx, package, call, function, file, found);
                }
                walk_block(ctx, package, arm.body, function, file, found);
            }
        }
        hir::StmtKind::Unsafe { reason, body, .. } => {
            found.push(Occurrence {
                file: file.to_string(),
                span: stmt.span,
                kind: "unsafe_block",
                function: function.map(str::to_string),
                extra: vec![("reason", reason.clone())],
            });
            walk_block(ctx, package, *body, function, file, found);
        }
        hir::StmtKind::Break { .. } | hir::StmtKind::Continue { .. } | hir::StmtKind::Poison => {}
    }
}

fn walk_pattern(
    ctx: &Ctx<'_>,
    package: &hir::Package,
    pattern: hir::PatId,
    function: Option<&str>,
    file: &str,
    found: &mut Vec<Occurrence>,
) {
    match &package.pat(pattern).kind {
        hir::PatKind::Literal(expr) => walk_expr(ctx, package, *expr, function, file, found),
        hir::PatKind::Range { lo, hi, .. } => {
            walk_pattern(ctx, package, *lo, function, file, found);
            walk_pattern(ctx, package, *hi, function, file, found);
        }
        hir::PatKind::Variant {
            payload: Some(payload),
            ..
        } => walk_pattern(ctx, package, *payload, function, file, found),
        hir::PatKind::Tuple(items) => {
            for item in items {
                walk_pattern(ctx, package, *item, function, file, found);
            }
        }
        hir::PatKind::Struct { fields, .. } => {
            for field in fields {
                walk_pattern(ctx, package, field.pattern, function, file, found);
            }
        }
        _ => {}
    }
}

fn walk_expr(
    ctx: &Ctx<'_>,
    package: &hir::Package,
    expr: hir::ExprId,
    function: Option<&str>,
    file: &str,
    found: &mut Vec<Occurrence>,
) {
    let node = package.expr(expr);
    match &node.kind {
        hir::ExprKind::Block(block) => walk_block(ctx, package, *block, function, file, found),
        hir::ExprKind::Builtin { name, args, .. } => {
            if name == "forget" || name == "debug" {
                found.push(Occurrence {
                    file: file.to_string(),
                    span: node.span,
                    kind: if name == "forget" { "forget" } else { "debug" },
                    function: function.map(str::to_string),
                    extra: Vec::new(),
                });
            }
            for arg in args {
                walk_expr(ctx, package, *arg, function, file, found);
            }
        }
        hir::ExprKind::Unary { expr, .. }
        | hir::ExprKind::Deref { base: expr, .. }
        | hir::ExprKind::TupleField { base: expr, .. }
        | hir::ExprKind::Field { base: expr, .. }
        | hir::ExprKind::TypeApply { base: expr, .. }
        | hir::ExprKind::Cast { expr, .. }
        | hir::ExprKind::Paren(expr) => walk_expr(ctx, package, *expr, function, file, found),
        hir::ExprKind::Binary { lhs, rhs, .. }
        | hir::ExprKind::Range {
            lo: lhs, hi: rhs, ..
        } => {
            walk_expr(ctx, package, *lhs, function, file, found);
            walk_expr(ctx, package, *rhs, function, file, found);
        }
        hir::ExprKind::Call { callee, args, .. } => {
            walk_expr(ctx, package, *callee, function, file, found);
            for arg in args {
                walk_expr(ctx, package, *arg, function, file, found);
            }
        }
        hir::ExprKind::Index { base, index, .. } => {
            walk_expr(ctx, package, *base, function, file, found);
            walk_expr(ctx, package, *index, function, file, found);
        }
        hir::ExprKind::Slice { base, lo, hi, .. } => {
            walk_expr(ctx, package, *base, function, file, found);
            if let Some(lo) = lo {
                walk_expr(ctx, package, *lo, function, file, found);
            }
            if let Some(hi) = hi {
                walk_expr(ctx, package, *hi, function, file, found);
            }
        }
        hir::ExprKind::StructLit { fields, .. } | hir::ExprKind::StructShorthand { fields, .. } => {
            for field in fields {
                walk_expr(ctx, package, field.value, function, file, found);
            }
        }
        hir::ExprKind::EnumLit {
            payload: Some(payload),
            ..
        } => walk_expr(ctx, package, *payload, function, file, found),
        hir::ExprKind::ArrayLit { len, elems, .. } => {
            walk_expr(ctx, package, *len, function, file, found);
            for element in elems {
                walk_expr(ctx, package, *element, function, file, found);
            }
        }
        hir::ExprKind::Tuple(items) => {
            for item in items {
                walk_expr(ctx, package, *item, function, file, found);
            }
        }
        hir::ExprKind::If {
            cond,
            then,
            otherwise,
        } => {
            walk_expr(ctx, package, cond.expr, function, file, found);
            walk_expr(ctx, package, *then, function, file, found);
            walk_expr(ctx, package, *otherwise, function, file, found);
        }
        hir::ExprKind::Match { scrutinee, arms } => {
            walk_expr(ctx, package, *scrutinee, function, file, found);
            for arm in arms {
                for pattern in &arm.patterns {
                    walk_pattern(ctx, package, *pattern, function, file, found);
                }
                if let Some(guard) = arm.guard {
                    walk_expr(ctx, package, guard, function, file, found);
                }
                match &arm.body {
                    hir::ArmBody::Block(block) => {
                        walk_block(ctx, package, *block, function, file, found)
                    }
                    hir::ArmBody::Expr(expr) => {
                        walk_expr(ctx, package, *expr, function, file, found)
                    }
                }
            }
        }
        hir::ExprKind::Closure(closure) => {
            walk_block(ctx, package, closure.body, function, file, found);
        }
        hir::ExprKind::Catch { value, handler, .. }
        | hir::ExprKind::Orelse { value, handler, .. } => {
            walk_expr(ctx, package, *value, function, file, found);
            match handler {
                hir::Handler::Expr(expr) => walk_expr(ctx, package, *expr, function, file, found),
                hir::Handler::Block(block) => {
                    walk_block(ctx, package, *block, function, file, found)
                }
            }
        }
        hir::ExprKind::Spawn { call, scope } => {
            walk_expr(ctx, package, *call, function, file, found);
            walk_expr(ctx, package, *scope, function, file, found);
        }
        hir::ExprKind::Dispatch {
            call,
            domain,
            executor,
        } => {
            walk_expr(ctx, package, *call, function, file, found);
            walk_expr(ctx, package, *domain, function, file, found);
            walk_expr(ctx, package, *executor, function, file, found);
        }
        hir::ExprKind::Unsafe { reason, body, .. } => {
            found.push(Occurrence {
                file: file.to_string(),
                span: node.span,
                kind: "unsafe_block",
                function: function.map(str::to_string),
                extra: vec![("reason", reason.clone())],
            });
            walk_expr(ctx, package, *body, function, file, found);
        }
        _ => {}
    }
}
