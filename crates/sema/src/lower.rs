//! Phase 5, second half: lower the AST of a package to the resolved HIR, binding
//! every value and type path.
//!
//! Resolution rules (pipeline section 4.3): scopes, innermost first, are block locals,
//! function parameters and generic parameters, module items and imports, builtin
//! names. Values and types are separate namespaces. A name that fails to resolve is
//! reported once and becomes `Res::Poison`.

use crate::defs::*;
use crate::hir::{self, ExprId, LocalId, PatId, Res, StmtId};
use crate::resolve::{DeclaredPackage, ItemLookup, Lookup, Resolver, unique_near};
use crate::target::TARGET_FIELDS;
use ingot_diag::diagnostic::Edit;
use ingot_diag::source::Span;
use ingot_syntax::ast;
use std::collections::{BTreeMap, BTreeSet};

struct LocalScope {
    names: BTreeMap<String, LocalId>,
    /// A function or closure boundary: names outside it are not visible.
    barrier: bool,
}

/// Collects the names a pattern binds, so that the alternatives of one `|` pattern
/// share their locals.
#[derive(Default)]
struct PatBinder {
    shared: BTreeMap<String, LocalId>,
    current: BTreeSet<String>,
}

pub(crate) struct Lowerer<'r, 'a> {
    r: &'r mut Resolver<'a>,
    pkg: hir::Package,
    module: ModuleId,
    file: usize,
    generics: Vec<BTreeMap<String, DefId>>,
    self_ty: Option<DefId>,
    scopes: Vec<LocalScope>,
    /// One frame per function or closure: the labels of the loops around.
    labels: Vec<Vec<(String, StmtId, Span)>>,
    in_test: bool,
    /// One entry per closure being lowered, innermost last: where `&name` is
    /// inserted into its capture list, and whether the list is empty (E0611's fix).
    closures: Vec<(usize, bool)>,
}

/// Lower every module of a declared package.
pub(crate) fn lower_package(r: &mut Resolver<'_>, declared: DeclaredPackage) -> hir::Package {
    let mut lowerer = Lowerer {
        r,
        pkg: hir::Package {
            name: declared.name.clone(),
            ..hir::Package::default()
        },
        module: BUILTIN_MODULE,
        file: 0,
        generics: Vec::new(),
        self_ty: None,
        scopes: Vec::new(),
        labels: Vec::new(),
        in_test: false,
        closures: Vec::new(),
    };
    for (module, file, ast) in &declared.modules {
        lowerer.lower_module(*module, *file, ast);
    }
    lowerer.report_unused_imports(&declared);
    lowerer.pkg
}

impl<'r, 'a> Lowerer<'r, 'a> {
    // ------------------------------------------------------------- plumbing

    fn diag(
        &mut self,
        code: &'static str,
        message: impl Into<String>,
        span: Span,
    ) -> &mut ingot_diag::diagnostic::Diagnostic {
        let file = self.file;
        self.r.diag(code, message, file, span)
    }

    /// `@target.has(.name)` outside a `when`: a name in neither CPU's feature list is
    /// E1606 on the name (section 24.3), as it is inside one.
    fn target_feature_query(&mut self, callee: &ast::Expr, args: &[ast::Expr]) {
        use ast::ExprKind as K;
        let K::Field { base, name, .. } = &callee.kind else {
            return;
        };
        let on_target = matches!(&base.kind, K::Builtin { name: b, type_arg: None, args } if b == "target" && args.is_empty());
        if name != "has" || !on_target {
            return;
        }
        if let [
            ast::Expr {
                kind:
                    K::EnumLit {
                        path: None,
                        variant,
                        variant_span,
                        payload: None,
                    },
                ..
            },
        ] = args
            && self.r.target.has(variant).is_none()
        {
            self.diag(
                "E1606",
                format!("`@target.has` has no feature `{variant}`"),
                *variant_span,
            );
        }
    }

    fn def_at(&self, span_start: usize) -> DefId {
        *self
            .r
            .decl_at
            .get(&(self.file, span_start))
            .expect("every declaration was given a definition")
    }

    fn alloc_expr(&mut self, kind: hir::ExprKind, span: Span) -> ExprId {
        self.pkg.exprs.push(hir::Expr { kind, span });
        ExprId(self.pkg.exprs.len() as u32 - 1)
    }

    fn alloc_stmt(&mut self, kind: hir::StmtKind, span: Span) -> StmtId {
        self.pkg.stmts.push(hir::Stmt { kind, span });
        StmtId(self.pkg.stmts.len() as u32 - 1)
    }

    fn alloc_block(&mut self, stmts: Vec<StmtId>, span: Span) -> hir::BlockId {
        self.pkg.blocks.push(hir::Block { stmts, span });
        hir::BlockId(self.pkg.blocks.len() as u32 - 1)
    }

    fn alloc_pat(&mut self, kind: hir::PatKind, span: Span) -> PatId {
        self.pkg.pats.push(hir::Pat { kind, span });
        PatId(self.pkg.pats.len() as u32 - 1)
    }

    fn alloc_local(
        &mut self,
        name: &str,
        span: Span,
        kind: hir::LocalKind,
        mutable: bool,
    ) -> LocalId {
        self.pkg.locals.push(hir::Local {
            name: name.to_string(),
            span,
            kind,
            mutable,
            read: false,
        });
        LocalId(self.pkg.locals.len() as u32 - 1)
    }

    // --------------------------------------------------------------- scopes

    fn push_scope(&mut self) {
        self.scopes.push(LocalScope {
            names: BTreeMap::new(),
            barrier: false,
        });
    }

    fn pop_scope(&mut self) {
        self.scopes.pop();
    }

    /// The locals visible at this point, innermost scope first.
    fn visible_locals(&self) -> Vec<(&str, LocalId)> {
        let mut out = Vec::new();
        for scope in self.scopes.iter().rev() {
            for (name, id) in &scope.names {
                out.push((name.as_str(), *id));
            }
            if scope.barrier {
                break;
            }
        }
        out
    }

    fn find_local(&self, name: &str) -> Option<LocalId> {
        for scope in self.scopes.iter().rev() {
            if let Some(&id) = scope.names.get(name) {
                return Some(id);
            }
            if scope.barrier {
                break;
            }
        }
        None
    }

    /// A local of an enclosing body that a closure barrier hides: the closure would
    /// have to capture it (E0611).
    fn find_uncaptured_local(&self, name: &str) -> Option<LocalId> {
        if self.closures.is_empty() {
            return None;
        }
        self.scopes
            .iter()
            .rev()
            .find_map(|scope| scope.names.get(name).copied())
    }

    /// Introduce a local in the innermost scope. A name that is already a local or a
    /// parameter in scope is E0309 when `check_shadow` is set.
    fn bind_local(
        &mut self,
        name: &str,
        span: Span,
        kind: hir::LocalKind,
        mutable: bool,
        check_shadow: bool,
    ) -> LocalId {
        if check_shadow && let Some(prev) = self.find_local(name) {
            let prev_span = self.pkg.local(prev).span;
            let path = self.r.source(self.file).path.clone();
            self.diag(
                "E0309",
                format!("`{name}` shadows a local or parameter that is already in scope"),
                span,
            )
            .relate_in("previous", &path, prev_span);
        }
        let id = self.alloc_local(name, span, kind, mutable);
        if let Some(scope) = self.scopes.last_mut() {
            scope.names.insert(name.to_string(), id);
        }
        id
    }

    // -------------------------------------------------------------- modules

    fn lower_module(&mut self, module: ModuleId, file: usize, ast: &ast::Module) {
        self.module = module;
        self.file = file;
        let mut items = hir::ModuleItems {
            module,
            items: Vec::new(),
            externs: Vec::new(),
        };
        for item in &ast.items {
            self.lower_item(item, &mut items);
        }
        self.pkg.modules.push(items);
    }

    fn lower_item(&mut self, item: &ast::Item, out: &mut hir::ModuleItems) {
        match item {
            ast::Item::Fn(f) => {
                let def = self.def_at(f.name_span.start);
                let lowered = self.lower_fn(f, def, None);
                self.pkg.items.insert(def, hir::Item::Fn(lowered));
                out.items.push(def);
            }
            ast::Item::Struct(s) => {
                let def = self.def_at(s.name_span.start);
                let lowered = self.lower_struct(s, def);
                self.pkg.items.insert(def, hir::Item::Struct(lowered));
                out.items.push(def);
            }
            ast::Item::Enum(e) => {
                let def = self.def_at(e.name_span.start);
                let lowered = self.lower_enum(e, def);
                self.pkg.items.insert(def, hir::Item::Enum(lowered));
                out.items.push(def);
            }
            ast::Item::Interface(i) => {
                let def = self.def_at(i.name_span.start);
                let lowered = self.lower_interface(i, def);
                self.pkg.items.insert(def, hir::Item::Interface(lowered));
                out.items.push(def);
            }
            ast::Item::Impl(i) => {
                let def = self.def_at(i.span.start);
                let lowered = self.lower_impl(i, def);
                self.pkg.items.insert(def, hir::Item::Impl(lowered));
                out.items.push(def);
            }
            ast::Item::Const(c) => {
                let def = self.def_at(c.name_span.start);
                let lowered = self.lower_const(c, def);
                self.pkg.items.insert(def, hir::Item::Const(lowered));
                out.items.push(def);
            }
            ast::Item::Static(s) => {
                let def = self.def_at(s.name_span.start);
                let lowered = self.lower_static(s, def);
                self.pkg.items.insert(def, hir::Item::Static(lowered));
                out.items.push(def);
            }
            ast::Item::TypeAlias(t) => {
                let def = self.def_at(t.name_span.start);
                let lowered = self.lower_alias(t, def);
                self.pkg.items.insert(def, hir::Item::Alias(lowered));
                out.items.push(def);
            }
            ast::Item::Extern(x) => {
                let mut functions = Vec::new();
                for f in &x.functions {
                    let def = self.def_at(f.name_span.start);
                    let lowered = self.lower_fn(f, def, Some(x.abi.clone()));
                    self.pkg.items.insert(def, hir::Item::Fn(lowered));
                    out.items.push(def);
                    functions.push(def);
                }
                out.externs.push(hir::ExternItem {
                    abi: x.abi.clone(),
                    abi_span: x.abi_span,
                    functions,
                    span: x.span,
                });
            }
            ast::Item::Test(t) => {
                let def = self.def_at(t.span.start);
                let lowered = self.lower_test(t, def);
                self.pkg.items.insert(def, hir::Item::Test(lowered));
                out.items.push(def);
            }
            // Removed by phase 4, or the module has errors and was not lowered.
            ast::Item::When(_) | ast::Item::Poison => {}
        }
    }

    // ---------------------------------------------------------------- bodies

    /// Start a body: a fresh local scope that no outer local can be seen through.
    fn enter_body(&mut self, in_test: bool) -> BodyState {
        let state = BodyState {
            scopes: std::mem::take(&mut self.scopes),
            labels: std::mem::take(&mut self.labels),
            in_test: self.in_test,
            first_local: self.pkg.locals.len(),
        };
        self.scopes.push(LocalScope {
            names: BTreeMap::new(),
            barrier: true,
        });
        self.labels.push(Vec::new());
        self.in_test = in_test;
        state
    }

    /// End a body and report the bindings that were never read (E0311).
    fn exit_body(&mut self, state: BodyState) {
        let unread: Vec<(String, Span)> = self.pkg.locals[state.first_local..]
            .iter()
            .filter(|l| {
                !l.read
                    && l.name != "_"
                    && matches!(
                        l.kind,
                        hir::LocalKind::Let | hir::LocalKind::For | hir::LocalKind::Catch
                    )
            })
            .map(|l| (l.name.clone(), l.span))
            .collect();
        for (name, span) in unread {
            self.diag("E0311", format!("`{name}` is never read"), span);
        }
        self.scopes = state.scopes;
        self.labels = state.labels;
        self.in_test = state.in_test;
    }

    fn lower_fn(&mut self, f: &ast::FnDecl, def: DefId, abi: Option<String>) -> hir::FnItem {
        let generics = self.push_generics(&f.generics);
        let state = self.enter_body(false);
        let mut params = Vec::new();
        for p in &f.params {
            let ty = self.lower_ty(&p.ty);
            let local = self.bind_local(&p.name, p.name_span, hir::LocalKind::Param, false, false);
            params.push(hir::Param {
                name: p.name.clone(),
                name_span: p.name_span,
                ty,
                into: None,
                local,
                span: p.span,
            });
        }
        // `into s` names a parameter of this function.
        for (i, p) in f.params.iter().enumerate() {
            if let Some((name, span)) = &p.into {
                let index = f.params.iter().position(|q| &q.name == name);
                params[i].into = Some((name.clone(), *span, index));
            }
        }
        let ret = self.lower_ty(&f.ret);
        let error = f.error.as_ref().map(|t| self.lower_ty(t));
        let from = f
            .from
            .iter()
            .map(|o| hir::Origin {
                name: o.name.clone(),
                inner: o.inner,
                is_static: o.is_static,
                span: o.span,
                param: if o.is_static {
                    None
                } else {
                    f.params.iter().position(|q| q.name == o.name)
                },
            })
            .collect();
        let body = f.body.as_ref().map(|b| self.lower_block(b));
        self.exit_body(state);
        self.pop_generics();
        hir::FnItem {
            def,
            name: f.name.clone(),
            name_span: f.name_span,
            mods: f.mods,
            generics,
            params,
            ret,
            error,
            from,
            reduce: f.reduce.clone(),
            variadic: f.variadic,
            abi,
            body,
            span: f.span,
        }
    }

    fn lower_test(&mut self, t: &ast::TestDecl, def: DefId) -> hir::TestItem {
        let state = self.enter_body(true);
        let body = self.lower_block(&t.body);
        self.exit_body(state);
        hir::TestItem {
            def,
            name: t.name.clone(),
            bench: t.bench,
            body,
            span: t.span,
        }
    }

    fn lower_const(&mut self, c: &ast::ConstDecl, def: DefId) -> hir::ConstItem {
        let ty = self.lower_ty(&c.ty);
        let state = self.enter_body(false);
        let value = self.lower_expr(&c.value);
        self.exit_body(state);
        hir::ConstItem {
            def,
            ty,
            value,
            span: c.span,
        }
    }

    fn lower_static(&mut self, s: &ast::StaticDecl, def: DefId) -> hir::StaticItem {
        let ty = self.lower_ty(&s.ty);
        let align = s.align.as_ref().map(|e| self.lower_expr(e));
        let state = self.enter_body(false);
        let value = self.lower_expr(&s.value);
        self.exit_body(state);
        hir::StaticItem {
            def,
            ty,
            align,
            value,
            span: s.span,
        }
    }

    fn lower_alias(&mut self, t: &ast::TypeAlias, def: DefId) -> hir::AliasItem {
        let generics = self.push_generics(&t.generics);
        // `type Item;` in an interface carries a poison type as its marker.
        let ty = match &t.ty {
            ast::Ty::Poison(_) => None,
            ty => Some(self.lower_ty(ty)),
        };
        self.pop_generics();
        hir::AliasItem {
            def,
            generics,
            ty,
            span: t.span,
        }
    }

    fn lower_struct(&mut self, s: &ast::StructDecl, def: DefId) -> hir::StructItem {
        let generics = self.push_generics(&s.generics);
        let align = s.align.as_ref().map(|e| self.lower_expr(e));
        let fields = s
            .fields
            .iter()
            .map(|f| hir::FieldItem {
                def: self.def_at(f.name_span.start),
                name: f.name.clone(),
                name_span: f.name_span,
                public: f.public,
                ty: self.lower_ty(&f.ty),
                align: f.align.as_ref().map(|e| self.lower_expr(e)),
                span: f.span,
            })
            .collect();
        self.pop_generics();
        hir::StructItem {
            def,
            kind: s.kind,
            generics,
            fields,
            align,
            span: s.span,
        }
    }

    fn lower_enum(&mut self, e: &ast::EnumDecl, def: DefId) -> hir::EnumItem {
        let generics = self.push_generics(&e.generics);
        let tag = e.tag.as_ref().map(|t| self.lower_ty(t));
        let variants = e
            .variants
            .iter()
            .map(|v| hir::VariantItem {
                def: self.def_at(v.name_span.start),
                name: v.name.clone(),
                name_span: v.name_span,
                payload: v.payload.as_ref().map(|t| self.lower_ty(t)),
                from: v.from,
                value: v.value.as_ref().map(|x| self.lower_expr(x)),
                span: v.span,
            })
            .collect();
        self.pop_generics();
        hir::EnumItem {
            def,
            copy: e.copy,
            tag,
            generics,
            variants,
            span: e.span,
        }
    }

    fn lower_interface(&mut self, i: &ast::InterfaceDecl, def: DefId) -> hir::InterfaceItem {
        let generics = self.push_generics(&i.generics);
        let self_def = self.r.self_defs[&def];
        self.self_ty = Some(self_def);
        let bounds = i.bounds.iter().map(|b| self.lower_path(b)).collect();
        let mut members = Vec::new();
        for m in &i.members {
            match m {
                ast::Item::Fn(f) => {
                    let d = self.def_at(f.name_span.start);
                    let lowered = self.lower_fn(f, d, None);
                    self.pkg.items.insert(d, hir::Item::Fn(lowered));
                    members.push(d);
                }
                ast::Item::TypeAlias(t) => {
                    let d = self.def_at(t.name_span.start);
                    let lowered = self.lower_alias(t, d);
                    self.pkg.items.insert(d, hir::Item::Alias(lowered));
                    members.push(d);
                }
                _ => {}
            }
        }
        self.self_ty = None;
        self.pop_generics();
        hir::InterfaceItem {
            def,
            generics,
            bounds,
            self_def,
            members,
            span: i.span,
        }
    }

    fn lower_impl(&mut self, i: &ast::ImplDecl, def: DefId) -> hir::ImplItem {
        let generics = self.push_generics(&i.generics);
        let self_def = self.r.self_defs[&def];
        let interface = i.interface.as_ref().map(|p| self.lower_path(p));
        let target = self.lower_ty(&i.target);
        self.self_ty = Some(self_def);
        let mut members = Vec::new();
        for m in &i.members {
            match m {
                ast::Item::Fn(f) => {
                    let d = self.def_at(f.name_span.start);
                    let lowered = self.lower_fn(f, d, None);
                    self.pkg.items.insert(d, hir::Item::Fn(lowered));
                    members.push(d);
                }
                ast::Item::Const(c) => {
                    let d = self.def_at(c.name_span.start);
                    let lowered = self.lower_const(c, d);
                    self.pkg.items.insert(d, hir::Item::Const(lowered));
                    members.push(d);
                }
                ast::Item::TypeAlias(t) => {
                    let d = self.def_at(t.name_span.start);
                    let lowered = self.lower_alias(t, d);
                    self.pkg.items.insert(d, hir::Item::Alias(lowered));
                    members.push(d);
                }
                _ => {}
            }
        }
        self.self_ty = None;
        self.pop_generics();
        hir::ImplItem {
            def,
            unsafe_: i.unsafe_,
            generics,
            interface,
            target,
            self_def,
            members,
            span: i.span,
        }
    }

    // -------------------------------------------------------------- generics

    /// Bring the generic parameters of an item into scope and lower their bounds.
    /// The caller pops the scope with `pop_generics`.
    fn push_generics(&mut self, params: &[ast::GenericParam]) -> Vec<hir::GenericParam> {
        let mut scope = BTreeMap::new();
        let mut defs = Vec::new();
        for g in params {
            let def = self.def_at(g.name_span.start);
            scope.entry(g.name.clone()).or_insert(def);
            defs.push(def);
        }
        self.generics.push(scope);
        params
            .iter()
            .zip(defs)
            .map(|(g, def)| hir::GenericParam {
                def,
                name: g.name.clone(),
                name_span: g.name_span,
                is_const: g.is_const,
                bounds: g.bounds.iter().map(|b| self.lower_path(b)).collect(),
                const_ty: g.const_ty.as_ref().map(|t| self.lower_ty(t)),
                span: g.span,
            })
            .collect()
    }

    fn pop_generics(&mut self) {
        self.generics.pop();
    }

    // ----------------------------------------------------------------- types

    fn lower_ty(&mut self, ty: &ast::Ty) -> hir::Ty {
        let span = ty.span();
        let kind = match ty {
            ast::Ty::Scalar(s, sp) => hir::TyKind::Path(hir::PathRef {
                res: Res::Def(self.r.sema.scalar_def(*s)),
                segments: vec![(s.name().to_string(), *sp)],
                args: Vec::new(),
                args_span: None,
                fn_sig: None,
                assoc: None,
                span: *sp,
            }),
            ast::Ty::Vector(v, sp) => hir::TyKind::Path(hir::PathRef {
                res: Res::Def(self.r.sema.vector_def(*v)),
                segments: vec![(v.name(), *sp)],
                args: Vec::new(),
                args_span: None,
                fn_sig: None,
                assoc: None,
                span: *sp,
            }),
            ast::Ty::Path(p) => hir::TyKind::Path(self.lower_path(p)),
            ast::Ty::Borrow { mutable, inner, .. } => hir::TyKind::Borrow {
                mutable: *mutable,
                inner: Box::new(self.lower_ty(inner)),
            },
            ast::Ty::Slice { mutable, elem, .. } => hir::TyKind::Slice {
                mutable: *mutable,
                elem: Box::new(self.lower_ty(elem)),
            },
            ast::Ty::Array { len, elem, .. } => hir::TyKind::Array {
                len: self.lower_expr(len),
                elem: Box::new(self.lower_ty(elem)),
            },
            ast::Ty::Ptr { mutable, inner, .. } => hir::TyKind::Ptr {
                mutable: *mutable,
                inner: Box::new(self.lower_ty(inner)),
            },
            ast::Ty::Optional { inner, .. } => {
                hir::TyKind::Optional(Box::new(self.lower_ty(inner)))
            }
            ast::Ty::Tuple { elems, .. } => {
                hir::TyKind::Tuple(elems.iter().map(|t| self.lower_ty(t)).collect())
            }
            ast::Ty::Fn {
                ext,
                params,
                ret,
                error,
                ..
            } => hir::TyKind::Fn {
                ext: *ext,
                params: params.iter().map(|t| self.lower_ty(t)).collect(),
                ret: Box::new(self.lower_ty(ret)),
                error: error.as_ref().map(|t| Box::new(self.lower_ty(t))),
            },
            ast::Ty::Dyn {
                interface, mutable, ..
            } => hir::TyKind::Dyn {
                interface: self.lower_path(interface),
                mutable: *mutable,
            },
            ast::Ty::Poison(_) => hir::TyKind::Poison,
        };
        hir::Ty { kind, span }
    }

    fn lower_generic_arg(&mut self, a: &ast::GenericArg) -> hir::GenericArg {
        match a {
            ast::GenericArg::Ty(t) => hir::GenericArg::Ty(self.lower_ty(t)),
            ast::GenericArg::Const(e) => hir::GenericArg::Const(self.lower_expr(e)),
        }
    }

    /// Resolve a path whose last segment is a type name and lower its arguments.
    fn lower_path(&mut self, p: &ast::PathTy) -> hir::PathRef {
        let res = self.resolve_type_path(&p.segments);
        let args = p.args.iter().map(|a| self.lower_generic_arg(a)).collect();
        let fn_sig = p.fn_sig.as_ref().map(|sig| hir::FnSig {
            params: sig.params.iter().map(|t| self.lower_ty(t)).collect(),
            ret: sig.ret.as_ref().map(|t| Box::new(self.lower_ty(t))),
            error: sig.error.as_ref().map(|t| Box::new(self.lower_ty(t))),
            span: sig.span,
        });
        hir::PathRef {
            res,
            segments: p.segments.clone(),
            args,
            args_span: p.args_span,
            fn_sig,
            assoc: p.assoc.clone(),
            span: p.span,
        }
    }

    fn resolve_type_path(&mut self, segments: &[(String, Span)]) -> Res {
        let Some((last, prefix)) = segments.split_last() else {
            return Res::Poison;
        };
        if prefix.is_empty() {
            return self.resolve_type_name(&last.0, last.1);
        }
        let module = match self.resolve_module_prefix(prefix) {
            Some(m) => m,
            None => return Res::Poison,
        };
        match self
            .r
            .lookup_item_in(module, self.module, Ns::Type, &last.0)
        {
            ItemLookup::Found(d) => Res::Def(d),
            ItemLookup::Private(d) => {
                self.private_item(&last.0, last.1, d);
                Res::Poison
            }
            ItemLookup::Poison => Res::Poison,
            ItemLookup::Missing => {
                let path = self.r.sema.module(module).path.clone();
                self.diag(
                    "E0305",
                    format!("module `{path}` has no public type `{}`", last.0),
                    last.1,
                );
                Res::Poison
            }
        }
    }

    /// The module that `prefix` (the module part of a path) names, or `None` after
    /// reporting (or silently, for poison).
    fn resolve_module_prefix(&mut self, prefix: &[(String, Span)]) -> Option<ModuleId> {
        let (first, first_span) = &prefix[0];
        let module = match self.r.lookup_in_scope(self.module, Ns::Value, first) {
            Lookup::Found(d) => {
                let def = self.r.sema.def(d);
                if def.kind == DefKind::Module {
                    def.module
                } else {
                    self.diag("E0305", format!("`{first}` is not a module"), *first_span);
                    return None;
                }
            }
            Lookup::Poison => return None,
            Lookup::Missing => {
                self.undefined_value(first, *first_span);
                return None;
            }
        };
        if let Some((second, span)) = prefix.get(1) {
            self.diag(
                "E0305",
                format!(
                    "`{first}` has no module `{second}`; its items are reached as `{first}.name`"
                ),
                *span,
            );
            return None;
        }
        Some(module)
    }

    fn resolve_type_name(&mut self, name: &str, span: Span) -> Res {
        for scope in self.generics.iter().rev() {
            if let Some(&d) = scope.get(name) {
                return Res::Def(d);
            }
        }
        if name == "Self"
            && let Some(d) = self.self_ty
        {
            return Res::Def(d);
        }
        match self.r.lookup_in_scope(self.module, Ns::Type, name) {
            Lookup::Found(d) => Res::Def(d),
            Lookup::Poison => Res::Poison,
            Lookup::Missing => {
                self.undefined_type(name, span);
                Res::Poison
            }
        }
    }

    fn private_item(&mut self, name: &str, span: Span, def: DefId) {
        let (dspan, dfile) = {
            let d = self.r.sema.def(def);
            (d.span, d.file.unwrap_or(self.file))
        };
        let dpath = self.r.sources.get(dfile).path.clone();
        self.diag("E0306", format!("`{name}` is private to its module"), span)
            .relate_in("declared", &dpath, dspan);
    }

    // ------------------------------------------------------ undefined names

    fn undefined_value(&mut self, name: &str, span: Span) {
        let fix = self
            .r
            .qualify_edit(self.module, self.file, Ns::Value, name, span)
            .map(|e| (format!("qualify `{name}` with its module"), e))
            .or_else(|| {
                let locals = self.visible_locals();
                let module = self.r.sema.module(self.module);
                let candidates = locals
                    .iter()
                    .map(|(n, _)| *n)
                    .chain(module.values.keys().map(String::as_str));
                unique_near(name, candidates).map(|near| {
                    (
                        format!("replace `{name}` with `{near}`"),
                        Edit {
                            file: self.r.source(self.file).path.clone(),
                            span,
                            text: near,
                        },
                    )
                })
            });
        let d = self.diag("E0304", format!("`{name}` is not defined"), span);
        if let Some((message, edit)) = fix {
            d.fix(message, edit);
        }
    }

    fn undefined_type(&mut self, name: &str, span: Span) {
        let fix = self
            .r
            .qualify_edit(self.module, self.file, Ns::Type, name, span)
            .map(|e| (format!("qualify `{name}` with its module"), e))
            .or_else(|| {
                let module = self.r.sema.module(self.module);
                let mut names: Vec<&str> = Vec::new();
                for scope in &self.generics {
                    names.extend(scope.keys().map(String::as_str));
                }
                if self.self_ty.is_some() {
                    names.push("Self");
                }
                names.extend(module.types.keys().map(String::as_str));
                names.extend(
                    self.r
                        .sema
                        .module(BUILTIN_MODULE)
                        .types
                        .keys()
                        .map(String::as_str),
                );
                unique_near(name, names.into_iter()).map(|near| {
                    (
                        format!("replace `{name}` with `{near}`"),
                        Edit {
                            file: self.r.source(self.file).path.clone(),
                            span,
                            text: near,
                        },
                    )
                })
            });
        let d = self.diag("E0305", format!("`{name}` is not a type"), span);
        if let Some((message, edit)) = fix {
            d.fix(message, edit);
        }
    }

    // ----------------------------------------------------------- statements

    fn lower_block(&mut self, b: &ast::Block) -> hir::BlockId {
        self.push_scope();
        let stmts = b.stmts.iter().map(|s| self.lower_stmt(s)).collect();
        self.pop_scope();
        self.alloc_block(stmts, b.span)
    }

    fn lower_label(&mut self, label: &Option<(String, Span)>) -> Option<hir::Label> {
        label.as_ref().map(|(name, span)| hir::Label {
            name: name.clone(),
            span: *span,
        })
    }

    /// Run `body` with `label` registered for the loop statement `id`. A label that
    /// is already used by an enclosing loop of this function is E0308.
    fn with_label<T>(
        &mut self,
        label: &Option<(String, Span)>,
        id: StmtId,
        body: impl FnOnce(&mut Self) -> T,
    ) -> T {
        if let Some((name, span)) = label {
            let previous = self
                .labels
                .last()
                .and_then(|frame| frame.iter().find(|(n, _, _)| n == name))
                .map(|(_, _, s)| *s);
            if let Some(prev) = previous {
                let path = self.r.source(self.file).path.clone();
                self.diag(
                    "E0308",
                    format!("the label `{name}` is already used by an enclosing loop"),
                    *span,
                )
                .relate_in("previous", &path, prev);
            }
            if let Some(frame) = self.labels.last_mut() {
                frame.push((name.clone(), id, *span));
            }
        }
        let result = body(self);
        if label.is_some()
            && let Some(frame) = self.labels.last_mut()
        {
            frame.pop();
        }
        result
    }

    fn jump_target(&mut self, label: &Option<(String, Span)>) -> Option<StmtId> {
        let (name, span) = label.as_ref()?;
        let found = self
            .labels
            .last()
            .and_then(|frame| frame.iter().rev().find(|(n, _, _)| n == name))
            .map(|(_, id, _)| *id);
        if found.is_none() {
            self.diag(
                "E0317",
                format!("no enclosing loop is labelled `{name}`"),
                *span,
            );
        }
        found
    }

    fn lower_cond_expr(&mut self, c: &ast::Cond) -> ExprId {
        self.lower_expr(&c.expr)
    }

    /// The pattern of an `if let` or `while let`, bound in the scope the caller has
    /// pushed for the guarded block.
    fn lower_cond_pattern(&mut self, c: &ast::Cond) -> Option<PatId> {
        c.let_pattern
            .as_ref()
            .map(|p| self.lower_pattern(p, &mut PatBinder::default(), hir::LocalKind::Let, false))
    }

    fn lower_stmt(&mut self, s: &ast::Stmt) -> StmtId {
        let span = s.span();
        let kind = match s {
            ast::Stmt::Let {
                mutable,
                pattern,
                ty,
                init,
                ..
            } => {
                // The initializer cannot see the names the pattern introduces.
                let ty = ty.as_ref().map(|t| self.lower_ty(t));
                let init = self.lower_expr(init);
                let pattern = self.lower_pattern(
                    pattern,
                    &mut PatBinder::default(),
                    hir::LocalKind::Let,
                    *mutable,
                );
                hir::StmtKind::Let {
                    mutable: *mutable,
                    pattern,
                    ty,
                    init,
                }
            }
            ast::Stmt::Assign {
                place, op, value, ..
            } => {
                if *op == ast::AssignOp::Assign
                    && matches!(&place.kind, ast::ExprKind::Ident(n) if n == "_")
                {
                    let value = self.lower_expr(value);
                    hir::StmtKind::Discard(value)
                } else {
                    let place = match &place.kind {
                        // A plain assignment is not a read of the variable.
                        ast::ExprKind::Ident(name) if *op == ast::AssignOp::Assign => {
                            self.lower_ident_expr(name, place.span, false)
                        }
                        _ => self.lower_expr(place),
                    };
                    let value = self.lower_expr(value);
                    hir::StmtKind::Assign {
                        place,
                        op: *op,
                        value,
                    }
                }
            }
            ast::Stmt::Expr { expr, .. } => hir::StmtKind::Expr(self.lower_expr(expr)),
            ast::Stmt::Return { value, .. } => {
                hir::StmtKind::Return(value.as_ref().map(|e| self.lower_expr(e)))
            }
            ast::Stmt::Fail { value, .. } => hir::StmtKind::Fail(self.lower_expr(value)),
            ast::Stmt::Break { label, .. } => hir::StmtKind::Break {
                target: self.jump_target(label),
                label: self.lower_label(label),
            },
            ast::Stmt::Continue { label, .. } => hir::StmtKind::Continue {
                target: self.jump_target(label),
                label: self.lower_label(label),
            },
            ast::Stmt::Defer { err, body, .. } => hir::StmtKind::Defer {
                err: *err,
                body: self.lower_block(body),
            },
            ast::Stmt::Block(b) => hir::StmtKind::Block(self.lower_block(b)),
            ast::Stmt::If(i) => return self.lower_if(i),
            ast::Stmt::While {
                label, cond, body, ..
            } => {
                let id = self.alloc_stmt(hir::StmtKind::Poison, span);
                let kind = self.with_label(label, id, |this| {
                    let cond_expr = this.lower_cond_expr(cond);
                    this.push_scope();
                    let pattern = this.lower_cond_pattern(cond);
                    let body = this.lower_block(body);
                    this.pop_scope();
                    hir::StmtKind::While {
                        label: this.lower_label(label),
                        cond: hir::Cond {
                            expr: cond_expr,
                            let_pattern: pattern,
                            span: cond.span,
                        },
                        body,
                    }
                });
                self.pkg.stmts[id.index()].kind = kind;
                return id;
            }
            ast::Stmt::Loop { label, body, .. } => {
                let id = self.alloc_stmt(hir::StmtKind::Poison, span);
                let kind = self.with_label(label, id, |this| hir::StmtKind::Loop {
                    label: this.lower_label(label),
                    body: this.lower_block(body),
                });
                self.pkg.stmts[id.index()].kind = kind;
                return id;
            }
            ast::Stmt::For {
                par,
                grain,
                label,
                vars,
                index,
                iter,
                range_end,
                step,
                body,
                ..
            } => {
                let id = self.alloc_stmt(hir::StmtKind::Poison, span);
                let kind = self.with_label(label, id, |this| {
                    let grain = grain.as_ref().map(|e| this.lower_expr(e));
                    let iter = this.lower_expr(iter);
                    let range_end = range_end
                        .as_ref()
                        .map(|(inclusive, e)| (*inclusive, this.lower_expr(e)));
                    let step = step.as_ref().map(|e| this.lower_expr(e));
                    this.push_scope();
                    let index = index.as_ref().map(|(n, sp)| this.for_var(n, *sp));
                    let vars = vars.iter().map(|(n, sp)| this.for_var(n, *sp)).collect();
                    let body = this.lower_block(body);
                    this.pop_scope();
                    hir::StmtKind::For {
                        par: *par,
                        grain,
                        label: this.lower_label(label),
                        vars,
                        index,
                        iter,
                        range_end,
                        step,
                        body,
                    }
                });
                self.pkg.stmts[id.index()].kind = kind;
                return id;
            }
            ast::Stmt::Match(m) => {
                let scrutinee = self.lower_expr(&m.scrutinee);
                let arms = m.arms.iter().map(|a| self.lower_arm(a)).collect();
                hir::StmtKind::Match { scrutinee, arms }
            }
            ast::Stmt::Scope {
                name,
                name_span,
                body,
                ..
            } => {
                self.push_scope();
                let local = self.bind_local(name, *name_span, hir::LocalKind::Scope, false, true);
                let body = self.lower_block(body);
                self.pop_scope();
                hir::StmtKind::Scope {
                    name: name.clone(),
                    name_span: *name_span,
                    local,
                    body,
                }
            }
            ast::Stmt::Spawn { call, scope, .. } => hir::StmtKind::Spawn {
                call: self.lower_expr(call),
                scope: self.lower_expr(scope),
            },
            ast::Stmt::Dispatch {
                call,
                domain,
                executor,
                ..
            } => hir::StmtKind::Dispatch {
                call: self.lower_expr(call),
                domain: self.lower_expr(domain),
                executor: self.lower_expr(executor),
            },
            ast::Stmt::Select { arms, .. } => {
                let arms = arms
                    .iter()
                    .map(|arm| {
                        let call = if arm.is_else {
                            None
                        } else {
                            Some(self.lower_expr(&arm.call))
                        };
                        self.push_scope();
                        let pattern = self.lower_pattern(
                            &arm.pattern,
                            &mut PatBinder::default(),
                            hir::LocalKind::Let,
                            false,
                        );
                        let body = self.lower_block(&arm.body);
                        self.pop_scope();
                        hir::SelectArm {
                            is_else: arm.is_else,
                            pattern,
                            call,
                            body,
                            span: arm.span,
                        }
                    })
                    .collect();
                hir::StmtKind::Select { arms }
            }
            ast::Stmt::Unsafe {
                reason,
                reason_span,
                body,
                ..
            } => hir::StmtKind::Unsafe {
                reason: reason.clone(),
                reason_span: *reason_span,
                body: self.lower_block(body),
            },
            ast::Stmt::When { .. } => {
                // Phase 4 removed every `when`; seeing one is a compiler bug.
                self.r
                    .not_implemented("internal: a `when` statement survived phase 4");
                hir::StmtKind::Poison
            }
            ast::Stmt::Poison => hir::StmtKind::Poison,
        };
        self.alloc_stmt(kind, span)
    }

    fn for_var(&mut self, name: &str, span: Span) -> hir::ForVar {
        let local = if name == "_" {
            None
        } else {
            Some(self.bind_local(name, span, hir::LocalKind::For, false, true))
        };
        hir::ForVar {
            name: name.to_string(),
            span,
            local,
        }
    }

    fn lower_if(&mut self, i: &ast::IfStmt) -> StmtId {
        let cond_expr = self.lower_cond_expr(&i.cond);
        self.push_scope();
        let pattern = self.lower_cond_pattern(&i.cond);
        let then = self.lower_block(&i.then);
        self.pop_scope();
        let otherwise = i.otherwise.as_ref().map(|s| self.lower_stmt(s));
        self.alloc_stmt(
            hir::StmtKind::If {
                cond: hir::Cond {
                    expr: cond_expr,
                    let_pattern: pattern,
                    span: i.cond.span,
                },
                then,
                otherwise,
            },
            i.span,
        )
    }

    fn lower_arm(&mut self, a: &ast::Arm) -> hir::Arm {
        self.push_scope();
        let mut binder = PatBinder::default();
        let patterns = a
            .patterns
            .iter()
            .map(|p| {
                binder.current.clear();
                self.lower_pattern(p, &mut binder, hir::LocalKind::Let, false)
            })
            .collect();
        let guard = a.guard.as_ref().map(|g| self.lower_expr(g));
        let body = match &a.body {
            ast::ArmBody::Block(b) => hir::ArmBody::Block(self.lower_block(b)),
            ast::ArmBody::Expr(e) => hir::ArmBody::Expr(self.lower_expr(e)),
        };
        self.pop_scope();
        hir::Arm {
            patterns,
            is_else: a.is_else,
            guard,
            body,
            span: a.span,
        }
    }

    // -------------------------------------------------------------- patterns

    fn bind_pattern_name(
        &mut self,
        name: &str,
        span: Span,
        binder: &mut PatBinder,
        kind: hir::LocalKind,
        mutable: bool,
    ) -> LocalId {
        // The second alternative of `a | b` binds the same locals as the first.
        if !binder.current.contains(name)
            && let Some(&local) = binder.shared.get(name)
        {
            binder.current.insert(name.to_string());
            return local;
        }
        binder.current.insert(name.to_string());
        let local = self.bind_local(name, span, kind, mutable, true);
        binder.shared.insert(name.to_string(), local);
        local
    }

    fn lower_pattern(
        &mut self,
        p: &ast::Pattern,
        binder: &mut PatBinder,
        kind: hir::LocalKind,
        mutable: bool,
    ) -> PatId {
        let span = p.span();
        let pk = match p {
            ast::Pattern::Wild(_) => hir::PatKind::Wild,
            ast::Pattern::Bind { name, span } => hir::PatKind::Bind {
                local: self.bind_pattern_name(name, *span, binder, kind, mutable),
            },
            ast::Pattern::Literal(e) => hir::PatKind::Literal(self.lower_expr(e)),
            ast::Pattern::Range {
                lo, hi, inclusive, ..
            } => hir::PatKind::Range {
                lo: self.lower_pattern(lo, binder, kind, mutable),
                hi: self.lower_pattern(hi, binder, kind, mutable),
                inclusive: *inclusive,
            },
            ast::Pattern::Variant {
                path,
                name,
                name_span,
                payload,
            } => hir::PatKind::Variant {
                ty: path.as_ref().map(|p| self.lower_path(p)),
                name: name.clone(),
                name_span: *name_span,
                payload: payload
                    .as_ref()
                    .map(|p| self.lower_pattern(p, binder, kind, mutable)),
            },
            ast::Pattern::Tuple { elems, .. } => hir::PatKind::Tuple(
                elems
                    .iter()
                    .map(|p| self.lower_pattern(p, binder, kind, mutable))
                    .collect(),
            ),
            ast::Pattern::Struct {
                ty,
                fields,
                rest,
                rest_span,
                ..
            } => {
                let ty = ty.as_ref().map(|p| self.lower_path(p));
                let fields = fields
                    .iter()
                    .map(|f| {
                        let pattern = match &f.pattern {
                            Some(p) => self.lower_pattern(p, binder, kind, mutable),
                            None => {
                                let local = self.bind_pattern_name(
                                    &f.name,
                                    f.name_span,
                                    binder,
                                    kind,
                                    mutable,
                                );
                                self.alloc_pat(hir::PatKind::Bind { local }, f.name_span)
                            }
                        };
                        hir::FieldPat {
                            name: f.name.clone(),
                            name_span: f.name_span,
                            pattern,
                        }
                    })
                    .collect();
                hir::PatKind::Struct {
                    ty,
                    fields,
                    rest: *rest,
                    rest_span: *rest_span,
                }
            }
            ast::Pattern::Poison => hir::PatKind::Poison,
        };
        self.alloc_pat(pk, span)
    }

    // ----------------------------------------------------------- expressions

    /// A value name: a local, then a module item or import.
    fn resolve_value_name(&mut self, name: &str, span: Span, read: bool) -> Res {
        if let Some(local) = self.find_local(name) {
            if read {
                self.pkg.locals[local.index()].read = true;
            }
            return Res::Local(local);
        }
        if let Some(outer) = self.find_uncaptured_local(name) {
            // The use stands for the outer variable, so it is not also unread.
            self.pkg.locals[outer.index()].read = true;
            let (at, empty) = *self.closures.last().expect("inside a closure");
            let text = if empty {
                format!("&{name}")
            } else {
                format!(", &{name}")
            };
            let file = self.r.source(self.file).path.clone();
            self.diag(
                "E0611",
                format!("the closure uses `{name}` but does not capture it"),
                span,
            )
            .fix(
                format!("capture `{name}` by shared borrow"),
                Edit {
                    file,
                    span: Span::empty(at),
                    text,
                },
            );
            return Res::Poison;
        }
        match self.r.lookup_in_scope(self.module, Ns::Value, name) {
            Lookup::Found(d) => {
                if self.r.sema.def(d).kind == DefKind::Module {
                    self.diag(
                        "E0304",
                        format!("`{name}` is a module, not a value; name one of its items as `{name}.item`"),
                        span,
                    );
                    Res::Poison
                } else {
                    Res::Def(d)
                }
            }
            Lookup::Poison => Res::Poison,
            Lookup::Missing => {
                self.undefined_value(name, span);
                Res::Poison
            }
        }
    }

    fn path_expr(&mut self, res: Res, segments: Vec<(String, Span)>, span: Span) -> ExprId {
        self.alloc_expr(
            hir::ExprKind::Path(hir::PathRef {
                res,
                segments,
                args: Vec::new(),
                args_span: None,
                fn_sig: None,
                assoc: None,
                span,
            }),
            span,
        )
    }

    fn lower_ident_expr(&mut self, name: &str, span: Span, read: bool) -> ExprId {
        let res = self.resolve_value_name(name, span, read);
        self.path_expr(res, vec![(name.to_string(), span)], span)
    }

    /// `m.name` where `m` is an imported module (and not a local): the item `name`
    /// of that module.
    fn module_member(
        &mut self,
        base: &ast::Expr,
        name: &str,
        name_span: Span,
        whole: Span,
    ) -> Option<ExprId> {
        let ast::ExprKind::Ident(m) = &base.kind else {
            return None;
        };
        if self.find_local(m).is_some() {
            return None;
        }
        let module = match self.r.lookup_in_scope(self.module, Ns::Value, m) {
            Lookup::Found(d) if self.r.sema.def(d).kind == DefKind::Module => {
                self.r.sema.def(d).module
            }
            Lookup::Poison => {
                return Some(self.path_expr(
                    Res::Poison,
                    vec![(m.clone(), base.span), (name.to_string(), name_span)],
                    whole,
                ));
            }
            _ => return None,
        };
        let res = match self.r.lookup_item_in(module, self.module, Ns::Value, name) {
            ItemLookup::Found(d) => Res::Def(d),
            ItemLookup::Private(d) => {
                self.private_item(name, name_span, d);
                Res::Poison
            }
            ItemLookup::Poison => Res::Poison,
            ItemLookup::Missing => {
                let path = self.r.sema.module(module).path.clone();
                self.diag(
                    "E0304",
                    format!("module `{path}` has no public item `{name}`"),
                    name_span,
                );
                Res::Poison
            }
        };
        Some(self.path_expr(
            res,
            vec![(m.clone(), base.span), (name.to_string(), name_span)],
            whole,
        ))
    }

    fn lower_exprs(&mut self, es: &[ast::Expr]) -> Vec<ExprId> {
        es.iter().map(|e| self.lower_expr(e)).collect()
    }

    fn lower_field_inits(&mut self, fields: &[ast::FieldInit]) -> Vec<hir::FieldInit> {
        fields
            .iter()
            .map(|f| hir::FieldInit {
                name: f.name.clone(),
                name_span: f.name_span,
                value: self.lower_expr(&f.value),
            })
            .collect()
    }

    fn lower_handler(&mut self, h: &ast::Handler) -> hir::Handler {
        match h {
            ast::Handler::Expr(e) => hir::Handler::Expr(self.lower_expr(e)),
            ast::Handler::Block(b) => hir::Handler::Block(self.lower_block(b)),
        }
    }

    fn lower_expr(&mut self, e: &ast::Expr) -> ExprId {
        use ast::ExprKind as K;
        let span = e.span;
        let kind = match &e.kind {
            K::Int {
                value,
                negative,
                text,
            } => hir::ExprKind::Int {
                value: *value,
                negative: *negative,
                text: text.clone(),
            },
            K::Float(f) => hir::ExprKind::Float(*f),
            K::Str(s) => hir::ExprKind::Str(s.clone()),
            K::CStr(s) => hir::ExprKind::CStr(s.clone()),
            K::Bool(b) => hir::ExprKind::Bool(*b),
            K::Ident(name) => return self.lower_ident_expr(name, span, true),
            K::Path(p) => hir::ExprKind::Path(self.lower_path(p)),
            K::Builtin {
                name,
                type_arg,
                args,
            } => {
                if name == "test_env" && !self.in_test {
                    self.diag(
                        "E1706",
                        "`@test_env()` is available only inside a `test` or `bench` block",
                        span,
                    );
                }
                hir::ExprKind::Builtin {
                    name: name.clone(),
                    type_arg: type_arg.as_ref().map(|t| Box::new(self.lower_ty(t))),
                    args: self.lower_exprs(args),
                }
            }
            K::Unary { op, expr, op_span } => hir::ExprKind::Unary {
                op: *op,
                expr: self.lower_expr(expr),
                op_span: *op_span,
            },
            K::Binary {
                op,
                lhs,
                rhs,
                op_span,
            } => hir::ExprKind::Binary {
                op: *op,
                lhs: self.lower_expr(lhs),
                rhs: self.lower_expr(rhs),
                op_span: *op_span,
            },
            K::Call {
                callee,
                args,
                args_span,
            } => {
                self.target_feature_query(callee, args);
                hir::ExprKind::Call {
                    callee: self.lower_expr(callee),
                    args: self.lower_exprs(args),
                    args_span: *args_span,
                }
            }
            K::Index {
                base,
                index,
                bracket,
            } => hir::ExprKind::Index {
                base: self.lower_expr(base),
                index: self.lower_expr(index),
                bracket: *bracket,
            },
            K::Slice {
                base,
                lo,
                hi,
                bracket,
            } => hir::ExprKind::Slice {
                base: self.lower_expr(base),
                lo: self.lower_expr(lo),
                hi: self.lower_expr(hi),
                bracket: *bracket,
            },
            K::Field {
                base,
                name,
                name_span,
            } => {
                if let Some(id) = self.module_member(base, name, *name_span, span) {
                    return id;
                }
                if matches!(&base.kind, K::Builtin { name: b, type_arg: None, args } if b == "target" && args.is_empty())
                    && !TARGET_FIELDS.contains(&name.as_str())
                {
                    self.diag(
                        "E1606",
                        format!("`@target` has no field `{name}`"),
                        *name_span,
                    );
                }
                hir::ExprKind::Field {
                    base: self.lower_expr(base),
                    name: name.clone(),
                    name_span: *name_span,
                }
            }
            K::TupleField { base, index, .. } => hir::ExprKind::TupleField {
                base: self.lower_expr(base),
                index: *index,
            },
            K::Deref { base, op_span } => hir::ExprKind::Deref {
                base: self.lower_expr(base),
                op_span: *op_span,
            },
            K::TypeApply { base, args, .. } => hir::ExprKind::TypeApply {
                base: self.lower_expr(base),
                args: args.iter().map(|t| self.lower_ty(t)).collect(),
            },
            K::Cast { expr, ty, as_span } => hir::ExprKind::Cast {
                expr: self.lower_expr(expr),
                ty: self.lower_ty(ty),
                as_span: *as_span,
            },
            K::StructLit { ty, fields, braces } => hir::ExprKind::StructLit {
                ty: self.lower_path(ty),
                fields: self.lower_field_inits(fields),
                braces: *braces,
            },
            K::StructShorthand { fields, braces } => hir::ExprKind::StructShorthand {
                fields: self.lower_field_inits(fields),
                braces: *braces,
            },
            K::EnumLit {
                path,
                variant,
                variant_span,
                payload,
            } => hir::ExprKind::EnumLit {
                ty: path.as_ref().map(|p| self.lower_path(p)),
                variant: variant.clone(),
                variant_span: *variant_span,
                payload: payload.as_ref().map(|p| self.lower_expr(p)),
            },
            K::ArrayLit {
                len, elem, elems, ..
            } => hir::ExprKind::ArrayLit {
                len: self.lower_expr(len),
                elem: self.lower_ty(elem),
                elems: self.lower_exprs(elems),
            },
            K::Range {
                lo, hi, inclusive, ..
            } => hir::ExprKind::Range {
                lo: self.lower_expr(lo),
                hi: self.lower_expr(hi),
                inclusive: *inclusive,
            },
            K::Tuple { elems, .. } => hir::ExprKind::Tuple(self.lower_exprs(elems)),
            K::Paren(inner) => hir::ExprKind::Paren(self.lower_expr(inner)),
            K::If {
                cond,
                then,
                otherwise,
                ..
            } => {
                let cond_expr = self.lower_cond_expr(cond);
                self.push_scope();
                let pattern = self.lower_cond_pattern(cond);
                let then = self.lower_expr(then);
                self.pop_scope();
                let otherwise = self.lower_expr(otherwise);
                hir::ExprKind::If {
                    cond: hir::Cond {
                        expr: cond_expr,
                        let_pattern: pattern,
                        span: cond.span,
                    },
                    then,
                    otherwise,
                }
            }
            K::Match {
                scrutinee, arms, ..
            } => {
                let scrutinee = self.lower_expr(scrutinee);
                let arms = arms.iter().map(|a| self.lower_arm(a)).collect();
                hir::ExprKind::Match { scrutinee, arms }
            }
            K::Closure {
                captures,
                params,
                ret,
                error,
                body,
                span: cspan,
            } => hir::ExprKind::Closure(Box::new(
                self.lower_closure(captures, params, ret, error, body, *cspan),
            )),
            K::Catch {
                value,
                binding,
                handler,
                catch_span,
            } => {
                let value = self.lower_expr(value);
                self.push_scope();
                let binding = binding.as_ref().map(|(name, sp)| {
                    self.bind_local(name, *sp, hir::LocalKind::Catch, false, true)
                });
                let handler = self.lower_handler(handler);
                self.pop_scope();
                hir::ExprKind::Catch {
                    value,
                    binding,
                    handler,
                    catch_span: *catch_span,
                }
            }
            K::Orelse {
                value,
                handler,
                orelse_span,
            } => hir::ExprKind::Orelse {
                value: self.lower_expr(value),
                handler: self.lower_handler(handler),
                orelse_span: *orelse_span,
            },
            K::Spawn { call, scope, .. } => hir::ExprKind::Spawn {
                call: self.lower_expr(call),
                scope: self.lower_expr(scope),
            },
            K::Dispatch {
                call,
                domain,
                executor,
                ..
            } => hir::ExprKind::Dispatch {
                call: self.lower_expr(call),
                domain: self.lower_expr(domain),
                executor: self.lower_expr(executor),
            },
            K::Unsafe {
                reason,
                reason_span,
                body,
                ..
            } => hir::ExprKind::Unsafe {
                reason: reason.clone(),
                reason_span: *reason_span,
                body: self.lower_expr(body),
            },
            K::Poison => hir::ExprKind::Poison,
        };
        self.alloc_expr(kind, span)
    }

    fn lower_closure(
        &mut self,
        captures: &[ast::Capture],
        params: &[ast::Param],
        ret: &ast::Ty,
        error: &Option<ast::Ty>,
        body: &ast::Block,
        span: Span,
    ) -> hir::Closure {
        // A capture names a local of the enclosing function, once (E0611).
        let mut outers = Vec::new();
        let mut listed: BTreeSet<&str> = BTreeSet::new();
        for c in captures {
            let outer = if !listed.insert(c.name.as_str()) {
                self.diag("E0611", format!("`{}` is captured twice", c.name), c.span);
                Res::Poison
            } else {
                match self.find_local(&c.name) {
                    Some(local) => {
                        self.pkg.locals[local.index()].read = true;
                        Res::Local(local)
                    }
                    None => {
                        self.diag(
                            "E0611",
                            format!("`{}` is not a local, so it cannot be captured", c.name),
                            c.span,
                        );
                        Res::Poison
                    }
                }
            };
            outers.push(outer);
        }
        // Where E0611's fix inserts a capture: after the last one, or just inside an
        // empty `[]`.
        let insert_at = match captures.last() {
            Some(c) => (c.span.end, false),
            None => {
                let text = &self.r.source(self.file).text;
                let open = text[span.start..].find('[').map(|i| span.start + i + 1);
                (open.unwrap_or(span.start), true)
            }
        };
        self.closures.push(insert_at);
        // The body sees only what it captures, plus module items.
        self.scopes.push(LocalScope {
            names: BTreeMap::new(),
            barrier: true,
        });
        self.labels.push(Vec::new());
        let captures = captures
            .iter()
            .zip(outers)
            .map(|(c, outer)| {
                let local =
                    self.bind_local(&c.name, c.span, hir::LocalKind::Capture, c.mutable, false);
                hir::Capture {
                    name: c.name.clone(),
                    span: c.span,
                    shared: c.shared,
                    mutable: c.mutable,
                    outer,
                    local,
                }
            })
            .collect();
        let mut seen: BTreeMap<String, Span> = BTreeMap::new();
        let params = params
            .iter()
            .map(|p| {
                // A repeated parameter is E0308, not also a shadowing of the first.
                let repeated = if let Some(prev) = seen.get(&p.name).copied() {
                    let path = self.r.source(self.file).path.clone();
                    self.diag(
                        "E0308",
                        format!("parameter `{}` is defined twice", p.name),
                        p.name_span,
                    )
                    .relate_in("previous", &path, prev);
                    true
                } else {
                    seen.insert(p.name.clone(), p.name_span);
                    false
                };
                let ty = self.lower_ty(&p.ty);
                let local = self.bind_local(
                    &p.name,
                    p.name_span,
                    hir::LocalKind::Param,
                    false,
                    !repeated,
                );
                hir::Param {
                    name: p.name.clone(),
                    name_span: p.name_span,
                    ty,
                    into: None,
                    local,
                    span: p.span,
                }
            })
            .collect();
        let ret = self.lower_ty(ret);
        let error = error.as_ref().map(|t| self.lower_ty(t));
        let body = self.lower_block(body);
        self.closures.pop();
        self.labels.pop();
        self.scopes.pop();
        hir::Closure {
            captures,
            params,
            ret,
            error,
            body,
            span,
        }
    }

    // --------------------------------------------------------------- imports

    fn report_unused_imports(&mut self, declared: &DeclaredPackage) {
        for (module, file, _) in &declared.modules {
            // A use inside a `when` branch that is not taken still counts (E0310).
            let data = self.r.sema.module(*module);
            let unused: Vec<Span> = data
                .imports
                .iter()
                .filter(|i| !i.used && !data.when_mentions.contains(&i.name))
                .map(|i| i.span)
                .collect();
            for span in unused {
                let source = self.r.source(*file);
                let name = source.text_of(span).to_string();
                let edit = Edit {
                    file: source.path.clone(),
                    span: whole_line(&source.text, span),
                    text: String::new(),
                };
                self.r
                    .diag(
                        "E0310",
                        format!("`{}` is never used", name.trim_end_matches(';')),
                        *file,
                        span,
                    )
                    .fix("delete the import", edit);
            }
        }
    }
}

struct BodyState {
    scopes: Vec<LocalScope>,
    labels: Vec<Vec<(String, StmtId, Span)>>,
    in_test: bool,
    first_local: usize,
}

/// The span of the whole line(s) of `span` when nothing else is on them, so that
/// deleting it leaves no blank line; otherwise `span` itself.
fn whole_line(text: &str, span: Span) -> Span {
    let line_start = text[..span.start].rfind('\n').map_or(0, |i| i + 1);
    let line_end = text[span.end..]
        .find('\n')
        .map_or(text.len(), |i| span.end + i + 1);
    let before_blank = text[line_start..span.start].trim().is_empty();
    // Only white space, or a comment, may follow on the line.
    let rest = text[span.end..line_end].trim();
    let after_blank = rest.is_empty() || rest.starts_with("//");
    if before_blank && after_blank {
        Span::new(line_start, line_end)
    } else {
        span
    }
}
