// Body checking for `check.rs`. This file is `include!`d into that module so it
// can use its private types without widening the public API.

#[derive(Clone, Debug)]
pub struct LocalFact {
    pub name: String,
    pub ty: Type,
    pub decl_span: Span,
    pub keyword_span: Span,
    pub mutable: bool,
    pub is_param: bool,
    pub linear: bool,
}

#[derive(Clone, Debug)]
struct Local {
    name: String,
    ty: Type,
    mutable: bool,
    assigned: bool,
    used: bool,
    decl_span: Span,
    keyword_span: Span,
    is_param: bool,
}

pub struct Checker<'a> {
    source: &'a Source,
    program: &'a Program,
    diagnostics: &'a mut Vec<Diagnostic>,
    ret: Type,
    error: Option<Type>,
    generic_bounds: HashMap<String, Vec<String>>,
    self_ty: Option<Type>,
    is_test: bool,
    scopes: Vec<HashMap<String, Local>>,
    finished: Vec<Local>,
    facts: HashMap<String, LocalFact>,
    loop_depth: usize,
    labels: Vec<String>,
    in_defer: bool,
    unsafe_depth: usize,
    in_dispatch: bool,
    kernel_index: Option<String>,
    param_into: HashMap<String, Option<String>>,
    from: Vec<Origin>,
    module: String,
    /// Semantic type of every checked expression, keyed by its start offset.
    expr_types: HashMap<Span, Type>,
}

impl<'a> Checker<'a> {
    #[allow(clippy::too_many_arguments)]
    fn new(
        source: &'a Source,
        program: &'a Program,
        diagnostics: &'a mut Vec<Diagnostic>,
        ret: Type,
        error: Option<Type>,
        generic_bounds: HashMap<String, Vec<String>>,
        self_ty: Option<Type>,
        is_test: bool,
        module: &str,
    ) -> Self {
        Checker {
            source,
            program,
            diagnostics,
            ret,
            error,
            generic_bounds,
            self_ty,
            is_test,
            scopes: Vec::new(),
            finished: Vec::new(),
            facts: HashMap::new(),
            loop_depth: 0,
            labels: Vec::new(),
            in_defer: false,
            unsafe_depth: 0,
            in_dispatch: false,
            kernel_index: None,
            param_into: HashMap::new(),
            from: Vec::new(),
            module: module.to_string(),
            expr_types: HashMap::new(),
        }
    }

    fn diag(&mut self, code: &'static str, message: impl Into<String>, span: Span) -> &mut Diagnostic {
        self.diagnostics
            .push(Diagnostic::new(code, message, self.source, span));
        self.diagnostics.last_mut().unwrap()
    }

    fn push_scope(&mut self) {
        self.scopes.push(HashMap::new());
    }

    fn pop_scope(&mut self, _function: bool) {
        if let Some(scope) = self.scopes.pop() {
            for (_, local) in scope {
                self.finished.push(local);
            }
        }
    }

    fn finish(&mut self) {
        while !self.scopes.is_empty() {
            self.pop_scope(false);
        }
        let finished = std::mem::take(&mut self.finished);
        for local in finished {
            if !local.used && !local.is_param && local.name != "_" {
                self.diag(
                    "E0311",
                    format!("`{}` is never read", local.name),
                    local.decl_span,
                );
            }
            if local.mutable && !local.assigned {
                let d = self.diag(
                    "E0312",
                    format!("`{}` is declared `var` but never mutated", local.name),
                    local.keyword_span,
                );
                d.fix(
                    "replace `var` with `let`",
                    Edit {
                        span: local.keyword_span,
                        text: "let".to_string(),
                    },
                );
            }
        }
    }

    fn check_ownership(&mut self, function: &crate::check::Function) -> Option<crate::cfg::Cfg> {
        let f = &function.decl;
        let facts: Vec<LocalFact> = self.facts.values().cloned().collect();
        let cfg = crate::cfg::lower(
            self.program,
            self.source,
            function,
            &facts,
            &self.expr_types,
        );
        let bounds: HashMap<String, Vec<String>> = f
            .generics
            .iter()
            .map(|g| {
                (
                    g.name.clone(),
                    g.bounds.iter().map(|b| b.name().to_string()).collect(),
                )
            })
            .collect();
        let checked = crate::cfg_check::check(
            self.program,
            self.source,
            &cfg,
            &facts,
            &bounds,
            function,
        );
        self.diagnostics.extend(checked.diagnostics);
        let mut cfg = cfg;
        crate::cost::remove_proven_checks(&mut cfg);
        Some(cfg)
    }

    fn declare_param(&mut self, name: &str, ty: Type, span: Span) {
        let linear = ty.is_linear_value(&self.program.defs, &self.generic_bounds);
        let local = Local {
            name: name.to_string(),
            ty,
            mutable: false,
            assigned: false,
            used: true,
            decl_span: span,
            keyword_span: span,
            is_param: true,
        };
        self.facts.insert(
            name.to_string(),
            LocalFact {
                name: name.to_string(),
                ty: local.ty.clone(),
                decl_span: span,
                keyword_span: span,
                mutable: false,
                is_param: true,
                linear,
            },
        );
        if let Some(scope) = self.scopes.last_mut() {
            scope.insert(name.to_string(), local);
        }
    }

    fn declare(
        &mut self,
        name: &str,
        ty: Type,
        span: Span,
        keyword_span: Span,
        mutable: bool,
    ) {
        if name == "_" {
            return;
        }
        if let Some((_, prev)) = self.find_local(name) {
            let prev_span = prev.decl_span;
            self.diag(
                "E0309",
                format!("`{name}` shadows a binding that is already in scope"),
                span,
            )
            .relate("previous", prev_span);
        }
        let linear = ty.is_linear_value(&self.program.defs, &self.generic_bounds);
        let fact = LocalFact {
            name: name.to_string(),
            ty: ty.clone(),
            decl_span: span,
            keyword_span,
            mutable,
            is_param: false,
            linear,
        };
        self.facts.insert(name.to_string(), fact);
        if let Some(scope) = self.scopes.last_mut() {
            scope.insert(
                name.to_string(),
                Local {
                    name: name.to_string(),
                    ty,
                    mutable,
                    assigned: false,
                    used: false,
                    decl_span: span,
                    keyword_span,
                    is_param: false,
                },
            );
        }
    }

    fn find_local(&self, name: &str) -> Option<(usize, &Local)> {
        self.scopes
            .iter()
            .rev()
            .enumerate()
            .find_map(|(i, s)| s.get(name).map(|l| (i, l)))
    }

    fn local(&mut self, name: &str, span: Span, exclusive: bool) -> Option<Type> {
        let Some((_, found)) = self.find_local(name) else {
            self.diag("E0304", format!("undefined name `{name}`"), span);
            return None;
        };
        let ty = found.ty.clone();
        let ty_mutable = matches!(
            &ty,
            Type::Borrow { mutable: true, .. } | Type::Slice { mutable: true, .. }
        );
        let immutable = !found.mutable && !ty_mutable;
        let is_param = found.is_param;
        let param_span = found.decl_span;
        for scope in self.scopes.iter_mut().rev() {
            if let Some(l) = scope.get_mut(name) {
                l.used = true;
                if exclusive {
                    l.assigned = true;
                }
                break;
            }
        }
        if exclusive && immutable {
            let _ = is_param;
            let name_text = self.source.text_of(Span::new(span.start, span.end)).to_string();
            let d = self.diag(
                "E0610",
                format!("cannot take an exclusive borrow of immutable `{name}`"),
                span,
            );
            d.relate("declared", param_span);
            d.fix(
                "change the binding to `var`",
                Edit {
                    span: param_span,
                    text: format!("var {name_text}"),
                },
            );
        }
        Some(ty)
    }

    fn local_for_assign(&mut self, name: &str, span: Span) -> Option<Type> {
        let Some((_, found)) = self.find_local(name) else {
            self.diag("E0304", format!("undefined name `{name}`"), span);
            return None;
        };
        let ty = found.ty.clone();
        let mutable = found.mutable;
        let ty_mutable = matches!(
            &ty,
            Type::Borrow { mutable: true, .. } | Type::Slice { mutable: true, .. }
        );
        let decl_span = found.decl_span;
        let keyword_span = found.keyword_span;
        let is_param = found.is_param;
        if !mutable && !ty_mutable {
            let d = self.diag(
                "E0606",
                format!("cannot assign to immutable `{name}`"),
                span,
            );
            d.relate("declared", decl_span);
            if !is_param {
                d.fix(
                    "change the binding to `var`",
                    Edit {
                        span: keyword_span,
                        text: "var".to_string(),
                    },
                );
            }
        }
        for scope in self.scopes.iter_mut().rev() {
            if let Some(l) = scope.get_mut(name) {
                l.used = true;
                l.assigned = true;
                break;
            }
        }
        Some(ty)
    }

    fn resolve_ty(&mut self, ty: &Ty) -> Type {
        match ty {
            Ty::Scalar(s, _) => match s {
                ScalarTy::Void => Type::Void,
                ScalarTy::Never => Type::Never,
                ScalarTy::Bool => Type::Bool,
                ScalarTy::F32 | ScalarTy::F64 => Type::Float(*s),
                _ => Type::Int(*s),
            },
            Ty::Vector(v, _) => Type::Vector(*v),
            Ty::Path(p) => self.resolve_path_ty(p),
            Ty::Borrow { mutable, inner, .. } => Type::Borrow {
                mutable: *mutable,
                inner: Box::new(self.resolve_ty(inner)),
            },
            Ty::Slice { mutable, elem, .. } => Type::Slice {
                mutable: *mutable,
                elem: Box::new(self.resolve_ty(elem)),
            },
            Ty::Array { len, elem, .. } => Type::Array(
                self.const_i64(len).unwrap_or(0),
                Box::new(self.resolve_ty(elem)),
            ),
            Ty::Ptr { mutable, inner, .. } => Type::Ptr {
                mutable: *mutable,
                inner: Box::new(self.resolve_ty(inner)),
            },
            Ty::Optional { inner, .. } => Type::Optional(Box::new(self.resolve_ty(inner))),
            Ty::Tuple { elems, .. } => {
                Type::Tuple(elems.iter().map(|t| self.resolve_ty(t)).collect())
            }
            Ty::Fn {
                params,
                ret,
                error,
                ..
            } => Type::Fn {
                params: params.iter().map(|t| self.resolve_ty(t)).collect(),
                ret: Box::new(self.resolve_ty(ret)),
                error: error.as_ref().map(|e| Box::new(self.resolve_ty(e))),
            },
            Ty::Dyn { interface, .. } => Type::Dyn(interface.name().to_string()),
            Ty::Poison(_) => Type::Error,
        }
    }

    fn resolve_path_ty(&mut self, p: &PathTy) -> Type {
        let name = p.name().to_string();
        if p.segments.len() == 1 {
            if self.generic_bounds.contains_key(&name) {
                return Type::Param(name);
            }
            if name == "Self"
                && let Some(t) = &self.self_ty {
                    return t.clone();
                }
        }
        if name == "Option" && p.segments.len() == 1
            && let Some(t) = self.resolve_args(&p.args).into_iter().next() {
                return Type::Optional(Box::new(t));
            }
        if name == "Result" && p.segments.len() == 1 {
            let mut it = self.resolve_args(&p.args).into_iter();
            if let (Some(t), Some(e)) = (it.next(), it.next()) {
                return Type::Result(Box::new(t), Box::new(e));
            }
        }
        let resolved = self
            .program
            .resolve_type_path(&self.module, p)
            .or_else(|| {
                if p.segments.len() == 1 && self.program.defs.external.contains(&name) {
                    Some(ResolvedName::External(name.clone()))
                } else {
                    None
                }
            });
        match resolved {
            Some(ResolvedName::Local(canonical)) => {
                if self.program.defs.structs.contains_key(&canonical) {
                    return Type::Struct(canonical, self.resolve_args(&p.args));
                }
                if self.program.defs.enums.contains_key(&canonical) {
                    return Type::Enum(canonical, self.resolve_args(&p.args));
                }
                return Type::External(canonical);
            }
            Some(ResolvedName::External(mut base)) => {
                if let Some(stripped) = base.strip_prefix("External:") {
                    base = stripped.to_string();
                }
                let args = self.resolve_args(&p.args);
                if matches!(base.as_str(), "Mutex" | "RwLock" | "Chan" | "Once")
                    && args.iter().any(|t| t.is_view(&self.program.defs))
                {
                    self.diag(
                        "E0801",
                        format!("{base} requires `Owned` contents, but this argument is a view"),
                        p.span,
                    );
                }
                if args.is_empty() {
                    return Type::External(base);
                }
                return Type::External(format!(
                    "{base}[{}]",
                    args.iter()
                        .map(|t| t.to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
            None => {}
        }
        self.diag("E0305", format!("undefined type `{name}`"), p.span);
        Type::Error
    }

    fn resolve_args(&mut self, args: &[GenericArg]) -> Vec<Type> {
        args.iter()
            .filter_map(|a| match a {
                GenericArg::Ty(t) => Some(self.resolve_ty(t)),
                GenericArg::Const(_) => None,
            })
            .collect()
    }

    fn const_i64(&mut self, e: &Expr) -> Option<i64> {
        match &e.kind {
            ExprKind::Int {
                value, negative, ..
            } => Some(if *negative {
                -(*value as i64)
            } else {
                *value as i64
            }),
            ExprKind::Paren(x) => self.const_i64(x),
            ExprKind::Ident(n) => self
                .program
                .const_value_in(&self.module, n)
                .and_then(|c| match c.value {
                    Some(ConstVal::Int(v)) => Some(v as i64),
                    _ => None,
                }),
            ExprKind::Binary { op, lhs, rhs, .. } => {
                let a = self.const_i64(lhs)?;
                let b = self.const_i64(rhs)?;
                match op {
                    BinOp::Add => a.checked_add(b),
                    BinOp::Sub => a.checked_sub(b),
                    BinOp::Mul => a.checked_mul(b),
                    BinOp::Div if b != 0 => a.checked_div(b),
                    BinOp::Rem if b != 0 => a.checked_rem(b),
                    _ => None,
                }
            }
            _ => None,
        }
    }

    // ---------------------------------------------------------------- blocks

    fn check_block(&mut self, block: &Block) -> Type {
        self.push_scope();
        let ty = self.check_stmts(&block.stmts);
        self.pop_scope(false);
        ty
    }

    fn check_block_with_bindings(
        &mut self,
        block: &Block,
        bindings: Vec<(String, Span, Type)>,
        mutable: bool,
        keyword_span: Span,
    ) -> Type {
        self.push_scope();
        for (name, span, ty) in bindings {
            self.declare(&name, ty, span, keyword_span, mutable);
        }
        let ty = self.check_stmts(&block.stmts);
        self.pop_scope(false);
        ty
    }

    fn check_stmts(&mut self, stmts: &[Stmt]) -> Type {
        let mut result = Type::Void;
        for stmt in stmts {
            let ty = self.check_stmt(stmt);
            if ty.is_never() {
                result = Type::Never;
            }
        }
        result
    }

    fn check_stmt(&mut self, stmt: &Stmt) -> Type {
        match stmt {
            Stmt::Let {
                mutable,
                pattern,
                ty,
                init,
                span,
            } => {
                let expected = ty.as_ref().map(|t| self.resolve_ty(t));
                let actual = self.check_expr(init, expected.as_ref());
                if expected.is_none() && needs_context(init) {
                    self.diag(
                        "E0404",
                        "this initializer needs an annotated type",
                        init.span,
                    );
                }
                let bind_ty = expected.clone().unwrap_or_else(|| actual.clone());
                if let Some(exp) = &expected {
                    self.expect_type(actual, exp, init.span);
                }
                let bindings = self.bind_pattern(pattern, &bind_ty);
                if self.pattern_is_refutable(pattern) {
                    self.diag(
                        "E0604",
                        "this pattern can fail to match; use `if let` or `match`",
                        pattern.span(),
                    );
                }
                let kw = Span::new(span.start, (span.start + 3).min(span.end));
                for (name, nspan, bty) in bindings {
                    self.declare(&name, bty, nspan, kw, *mutable);
                }
                Type::Void
            }
            Stmt::Assign {
                place,
                op,
                value,
                span,
            } => {
                let is_discard = matches!(&place.kind, ExprKind::Ident(n) if n == "_");
                let place_ty = self.check_assign_place(place);
                if is_discard {
                    let actual = self.check_expr(value, None);
                    if actual.is_linear_value(&self.program.defs, &self.generic_bounds) {
                        self.diag(
                            "E0414",
                            "a linear value cannot be discarded with `_ =`; consume it instead",
                            place.span,
                        );
                    }
                } else if let Some(pt) = &place_ty {
                    let expected = if *op == AssignOp::Assign {
                        Some(pt.clone())
                    } else {
                        let bin = assign_binop(*op);
                        if let Some(b) = bin {
                            let _ = self.check_binary_types(b, pt, pt, Span::empty(span.start));
                        }
                        Some(pt.clone())
                    };
                    let actual = self.check_expr(value, expected.as_ref());
                    if let Some(exp) = &expected {
                        self.expect_type(actual.clone(), exp, value.span);
                    }
                    self.check_into_store(place, value, &actual, *span);
                } else {
                    self.check_expr(value, None);
                }
                Type::Void
            }
            Stmt::Expr { expr, span } => {
                let ty = self.check_expr(expr, None);
                match &expr.kind {
                    ExprKind::Ident(name) if name == "_" => {
                        let _ = span;
                    }
                    _ => {}
                }
                if ty.is_result() {
                    self.diag(
                        "E0701",
                        "a result value cannot be ignored; use `try`, `catch`, or `match`",
                        expr.span,
                    );
                } else if !ty.is_void() && !ty.is_never() && !ty.is_error() {
                    let discardable = ty.is_copy(&self.program.defs, &self.generic_bounds)
                        || !ty.is_linear_value(&self.program.defs, &self.generic_bounds);
                    let d = self.diag(
                        "E0413",
                        format!("the value of this call (`{ty}`) is not used"),
                        expr.span,
                    );
                    if discardable {
                        d.fix(
                            "discard the value explicitly",
                            Edit {
                                span: Span::empty(expr.span.start),
                                text: "_ = ".to_string(),
                            },
                        );
                    }
                }
                Type::Void
            }
            Stmt::Return { value, span } => {
                self.check_return(value.as_ref(), *span);
                Type::Never
            }
            Stmt::Fail { value, span } => {
                self.check_fail(value, *span);
                Type::Never
            }
            Stmt::Break { label, span } => {
                if let Some(label) = label {
                    if !self.labels.contains(label) {
                        self.diag(
                            "E0317",
                            format!("no enclosing loop is labelled `{label}`"),
                            *span,
                        );
                    }
                } else if self.loop_depth == 0 {
                    self.diag("E0605", "`break` outside a loop", *span);
                }
                Type::Never
            }
            Stmt::Continue { label, span } => {
                if let Some(label) = label {
                    if !self.labels.contains(label) {
                        self.diag(
                            "E0317",
                            format!("no enclosing loop is labelled `{label}`"),
                            *span,
                        );
                    }
                } else if self.loop_depth == 0 {
                    self.diag("E0605", "`continue` outside a loop", *span);
                }
                Type::Never
            }
            Stmt::Defer { err, body, span } => {
                if *err && self.error.is_none() && !self.is_test {
                    self.diag("E0709", "`errdefer` in a function that cannot fail", *span)
                        .fix(
                            "use `defer`",
                            Edit {
                                span: Span::new(span.start, span.start + 7),
                                text: "defer".to_string(),
                            },
                        );
                }
                let old = self.in_defer;
                self.in_defer = true;
                self.diag_exit_in_defer(body, *span);
                self.check_block(body);
                self.in_defer = old;
                Type::Void
            }
            Stmt::Block(b) => self.check_block(b),
            Stmt::If(x) => {
                let bindings = self.check_cond(&x.cond);
                if bindings.is_empty() {
                    self.check_block(&x.then);
                } else {
                    self.check_block_with_bindings(
                        &x.then,
                        bindings,
                        false,
                        Span::empty(x.then.span.start),
                    );
                }
                if let Some(e) = &x.otherwise {
                    self.check_stmt(e);
                }
                Type::Void
            }
            Stmt::While { cond, body, .. } => {
                self.loop_depth += 1;
                let bindings = self.check_cond(cond);
                if bindings.is_empty() {
                    self.check_block(body);
                } else {
                    self.check_block_with_bindings(
                        body,
                        bindings,
                        false,
                        Span::empty(body.span.start),
                    );
                }
                self.loop_depth -= 1;
                Type::Void
            }
            Stmt::Loop { label, body, .. } => {
                self.loop_depth += 1;
                if let Some(l) = label {
                    self.labels.push(l.clone());
                }
                self.check_block(body);
                if label.is_some() {
                    self.labels.pop();
                }
                self.loop_depth -= 1;
                Type::Void
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
            } => {
                if *par {
                    let outer: HashSet<String> = self
                        .scopes
                        .iter()
                        .flat_map(|s| s.keys().cloned())
                        .collect();
                    let mut declared = HashSet::new();
                    let mut ops = Vec::new();
                    find_outer_mutation(&body.stmts, &outer, &mut declared, &mut ops);
                    for sp in ops {
                        self.diag(
                            "E1407",
                            "a `par for` body cannot assign, move or exclusively borrow an outer value",
                            sp,
                        );
                    }
                }
                self.check_for(
                    *par,
                    grain.as_ref(),
                    label.as_deref(),
                    vars,
                    index.as_ref(),
                    iter,
                    range_end.as_ref(),
                    step.as_ref(),
                    body,
                    *span,
                );
                Type::Void
            }
            Stmt::Match(m) => {
                self.check_match(&m.scrutinee, &m.arms, None, m.span, true);
                Type::Void
            }
            Stmt::Scope {
                name,
                name_span,
                body,
                ..
            } => {
                self.push_scope();
                self.declare(
                    name,
                    Type::External("Scope".to_string()),
                    *name_span,
                    *name_span,
                    false,
                );
                self.check_stmts(&body.stmts);
                self.pop_scope(false);
                Type::Void
            }
            Stmt::Spawn { call, scope, span } => {
                let result = self.check_expr(call, None);
                let _ = result;
                self.check_spawn(call, scope, *span);
                self.check_expr(scope, None);
                Type::Void
            }
            Stmt::Dispatch {
                call,
                domain,
                executor,
                ..
            } => {
                let old = self.in_dispatch;
                self.in_dispatch = true;
                self.check_dispatch_call(call);
                self.in_dispatch = old;
                self.check_expr(domain, None);
                let _ = executor;
                Type::Void
            }
            Stmt::Select { arms, .. } => {
                for arm in arms {
                    let _ = self.bind_pattern(&arm.pattern, &Type::Error);
                    self.check_expr(&arm.call, None);
                    self.check_block(&arm.body);
                }
                Type::Void
            }
            Stmt::Unsafe {
                reason,
                reason_span,
                body,
                ..
            } => {
                if reason.trim().is_empty() {
                    self.diag(
                        "E1302",
                        "an `unsafe` block needs a non-empty reason",
                        *reason_span,
                    );
                }
                if !block_needs_unsafe(body) {
                    self.diag(
                        "E1304",
                        "this `unsafe` block contains no operation that needs it",
                        Span::new(body.span.start.saturating_sub(1), (body.span.start + 5).min(body.span.end)),
                    );
                }
                self.unsafe_depth += 1;
                self.check_block(body);
                self.unsafe_depth -= 1;
                Type::Void
            }
            Stmt::When {
                condition,
                then,
                otherwise,
                ..
            } => {
                self.check_when_condition(condition);
                self.check_block(then);
                for s in otherwise {
                    self.check_stmt(s);
                }
                Type::Void
            }
            Stmt::Poison => Type::Error,
        }
    }

    fn diag_exit_in_defer(&mut self, body: &Block, span: Span) -> Option<()> {
        let mut found = false;
        fn walk(
            stmts: &[Stmt],
            loop_depth: usize,
            found: &mut bool,
            out: &mut Vec<(String, Span)>,
        ) {
            for s in stmts {
                match s {
                    Stmt::Return { span, .. } => {
                        *found = true;
                        out.push(("`return` cannot leave a deferred block".to_string(), *span));
                    }
                    Stmt::Fail { span, .. } => {
                        *found = true;
                        out.push(("`fail` cannot leave a deferred block".to_string(), *span));
                    }
                    Stmt::Expr { expr, .. } => {
                        if let ExprKind::Unary {
                            op: UnOp::Try, ..
                        } = expr.kind
                        {
                            *found = true;
                            out.push((
                                "`try` cannot leave a deferred block".to_string(),
                                expr.span,
                            ));
                        }
                    }
                    Stmt::Break { span, .. } if loop_depth == 0 => {
                        *found = true;
                        out.push((
                            "`break` cannot leave a deferred block".to_string(),
                            *span,
                        ));
                    }
                    Stmt::Continue { span, .. } if loop_depth == 0 => {
                        *found = true;
                        out.push((
                            "`continue` cannot leave a deferred block".to_string(),
                            *span,
                        ));
                    }
                    Stmt::While { body, .. }
                    | Stmt::Loop { body, .. }
                    | Stmt::For { body, .. } => walk(&body.stmts, loop_depth + 1, found, out),
                    Stmt::Block(b) => walk(&b.stmts, loop_depth, found, out),
                    Stmt::If(x) => {
                        walk(&x.then.stmts, loop_depth, found, out);
                        if let Some(e) = &x.otherwise {
                            walk(std::slice::from_ref(e), loop_depth, found, out);
                        }
                    }
                    Stmt::Match(m) => {
                        for a in &m.arms {
                            match &a.body {
                                ArmBody::Block(b) => walk(&b.stmts, loop_depth, found, out),
                                ArmBody::Expr(_) => {}
                            }
                        }
                    }
                    Stmt::Scope { body, .. } => walk(&body.stmts, loop_depth, found, out),
                    _ => {}
                }
            }
        }
        let mut out = Vec::new();
        walk(&body.stmts, 0, &mut found, &mut out);
        for (msg, sp) in out {
            self.diag("E0708", msg, sp);
        }
        let _ = span;
        if found {
            Some(())
        } else {
            None
        }
    }

    fn check_cond(&mut self, cond: &Cond) -> Vec<(String, Span, Type)> {
        if let Some(pattern) = &cond.let_pattern {
            let scrutinee = self.check_expr(&cond.expr, None);
            self.bind_pattern(pattern, &scrutinee)
        } else {
            self.check_condition(&cond.expr);
            Vec::new()
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn check_for(
        &mut self,
        par: bool,
        grain: Option<&Expr>,
        label: Option<&str>,
        vars: &[(String, Span)],
        index: Option<&(String, Span)>,
        iter: &Expr,
        range_end: Option<&(bool, Expr)>,
        step: Option<&Expr>,
        body: &Block,
        span: Span,
    ) {
        let _ = par;
        let iter_ty = self.check_expr(iter, None);
        let item_ty = if let Some((_inclusive, end)) = range_end {
            let end_ty = self.check_expr(end, Some(&iter_ty));
            self.expect_type(end_ty, &iter_ty, end.span);
            if !iter_ty.is_integer() {
                self.diag("E0609", "range bounds must be integers", iter.span);
            }
            iter_ty.clone()
        } else {
            match &iter_ty {
                Type::Array(_, e) => (**e).clone(),
                Type::Slice { mutable, elem } => {
                    if *mutable {
                        Type::Borrow {
                            mutable: true,
                            inner: elem.clone(),
                        }
                    } else if !elem.is_copy(&self.program.defs, &self.generic_bounds) {
                        let d = self.diag(
                            "E0608",
                            "a shared slice of a non-`Copy` type needs `.refs()` to iterate",
                            iter.span,
                        );
                        d.fix(
                            "append `.refs()`",
                            Edit {
                                span: Span::empty(iter.span.end),
                                text: ".refs()".to_string(),
                            },
                        );
                        Type::Error
                    } else {
                        (**elem).clone()
                    }
                }
                Type::External(_) | Type::Error => Type::Error,
                _ => {
                    self.diag(
                        "E0608",
                        format!("`{iter_ty}` is not a range, array, slice or `Iter`"),
                        iter.span,
                    );
                    Type::Error
                }
            }
        };
        if let Some(step) = step {
            match self.const_i64(step) {
                Some(n) if n > 0 => {}
                _ => {
                    self.diag("E0609", "`step` must be a positive constant", step.span);
                }
            }
        }
        if let Some(g) = grain {
            let ty = self.check_expr(g, None);
            self.expect_type(ty, &Type::i64(), g.span);
        }
        self.loop_depth += 1;
        if let Some(l) = label {
            self.labels.push(l.to_string());
        }
        let mut bindings = Vec::new();
        if let Some((idx, ispan)) = index {
            bindings.push((idx.clone(), *ispan, Type::i64()));
        }
        if let Some((var, vspan)) = vars.first() {
            if vars.len() > 1 {
                self.diag("E0213", "`for` takes one item binding", vars[1].1);
            }
            bindings.push((var.clone(), *vspan, item_ty));
        }
        self.check_block_with_bindings(body, bindings, false, Span::empty(span.start));
        if label.is_some() {
            self.labels.pop();
        }
        self.loop_depth -= 1;
    }

    fn check_when_condition(&mut self, condition: &Expr) {
        fn walk(e: &Expr, ok: &mut bool) {
            match &e.kind {
                ExprKind::Binary {
                    op: BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge,
                    lhs,
                    rhs,
                    ..
                } => {
                    walk(lhs, ok);
                    walk(rhs, ok);
                }
                ExprKind::Binary {
                    op: BinOp::And | BinOp::Or,
                    lhs,
                    rhs,
                    ..
                } => {
                    walk(lhs, ok);
                    walk(rhs, ok);
                }
                ExprKind::Unary {
                    op: UnOp::Not, expr, ..
                } => walk(expr, ok),
                ExprKind::Bool(_)
                | ExprKind::Int { .. }
                | ExprKind::Str(_)
                | ExprKind::Paren(_)
                | ExprKind::EnumLit { .. }
                | ExprKind::Field { .. } => {}
                ExprKind::Builtin { name, args, .. } if name == "target" => {
                    for a in args {
                        walk(a, ok);
                    }
                }
                _ => *ok = false,
            }
        }
        let mut ok = true;
        walk(condition, &mut ok);
        if !ok {
            self.diag(
                "E0612",
                "a `when` condition may only use `@target`, literals, comparisons and `&& || !`",
                condition.span,
            );
        }
    }

    // -------------------------------------------------------------- statements

    fn check_return(&mut self, value: Option<&Expr>, span: Span) {
        let ret = self.ret.clone();
        if let Some(e) = value {
            if ret.is_void() {
                self.diag("E0421", "this function has no result type", e.span);
                self.check_expr(e, None);
                return;
            }
            let actual = self.check_expr(e, Some(&ret));
            self.check_return_origin(e, &ret);
            if let Type::Result(inner, err) = &actual
                && (&ret == inner.as_ref() || ret == *inner.as_ref()) {
                    let d = self.diag(
                        "E0710",
                        "return a result with `try` or return its error explicitly",
                        e.span,
                    );
                    d.fix(
                        "insert `try `",
                        Edit {
                            span: Span::empty(e.span.start),
                            text: "try ".to_string(),
                        },
                    );
                    let _ = err;
                    return;
                }
            self.expect_type(actual, &ret, e.span);
        } else if !ret.is_void() {
            self.diag("E0421", "this function returns a value", span);
        }
    }

    fn check_fail(&mut self, value: &Expr, span: Span) {
        let Some(error_ty) = self.error.clone() else {
            self.diag("E0705", "`fail` in a function that cannot fail", span);
            self.check_expr(value, None);
            return;
        };
        let actual = self.check_expr(value, Some(&error_ty));
        if !types_equal(&actual, &error_ty) {
            // A `from` variant may wrap the operand.
            if let Type::Enum(name, _) = &error_ty
                && let Some(def) = self.program.defs.enums.get(name)
                    && let Some(variant) = def.from.get(&actual.to_string()) {
                        let variant = variant.clone();
                        let d = self.diag(
                            "E0706",
                            format!(
                                "`fail` needs `{error_ty}`, but this value is `{actual}`"
                            ),
                            value.span,
                        );
                        let diff = d.message.len();
                        let _ = diff;
                        d.fix(
                            format!("wrap it as `.{variant}(...)`"),
                            Edit {
                                span: Span::empty(value.span.start),
                                text: format!(".{variant}("),
                            },
                        );
                        return;
                    }
            self.expect_type(actual, &error_ty, value.span);
        }
    }

    // --------------------------------------------------------------- patterns

    fn bind_pattern(&mut self, pattern: &Pattern, expected: &Type) -> Vec<(String, Span, Type)> {
        let mut bindings = Vec::new();
        let mut names = HashSet::new();
        self.bind_pattern_into(pattern, expected, &mut bindings, &mut names);
        bindings
    }

    fn bind_pattern_into(
        &mut self,
        pattern: &Pattern,
        expected: &Type,
        out: &mut Vec<(String, Span, Type)>,
        names: &mut HashSet<String>,
    ) {
        match pattern {
            Pattern::Wild(_) => {}
            Pattern::Bind { name, span } => {
                if !names.insert(name.clone()) {
                    self.diag(
                        "E0308",
                        format!("`{name}` is bound twice in one pattern"),
                        *span,
                    );
                }
                out.push((name.clone(), *span, expected.clone()));
            }
            Pattern::Literal(e) => {
                let ty = self.check_expr(e, Some(expected));
                if !types_equal(&ty, expected) && !types_equal(expected, &Type::Error) {
                    self.diag(
                        "E0603",
                        format!("pattern of type `{ty}` does not fit `{expected}`"),
                        e.span,
                    );
                }
            }
            Pattern::Range { lo, hi, .. } => {
                self.bind_pattern_into(lo, expected, out, names);
                self.bind_pattern_into(hi, expected, out, names);
                if !expected.is_integer() && !expected.is_error() {
                    self.diag(
                        "E0609",
                        "a range pattern needs an integer type",
                        pattern.span(),
                    );
                }
            }
            Pattern::Variant {
                path,
                name,
                name_span,
                payload,
            } => {
                let enum_ty = if let Some(p) = path {
                    self.resolve_path_ty(p)
                } else {
                    expected.clone()
                };
                if let Type::Optional(inner) = &enum_ty {
                    match name.as_str() {
                        "none" => {
                            if payload.is_some() {
                                self.diag(
                                    "E0410",
                                    "variant `none` has no payload",
                                    *name_span,
                                );
                            }
                        }
                        "some" => match payload {
                            Some(p) => self.bind_pattern_into(p, inner, out, names),
                            None => {
                                self.diag(
                                    "E0410",
                                    "variant `some` needs a payload",
                                    *name_span,
                                );
                            }
                        },
                        _ => {
                            self.diag(
                                "E0314",
                                format!("an optional has no variant `{name}`"),
                                *name_span,
                            );
                        }
                    }
                    return;
                }
                if let Type::Result(ok_ty, err_ty) = &enum_ty {
                    let payload_ty = match name.as_str() {
                        "ok" => Some((**ok_ty).clone()),
                        "err" => Some((**err_ty).clone()),
                        _ => None,
                    };
                    let Some(payload_ty) = payload_ty else {
                        self.diag(
                            "E0314",
                            format!("a result has no variant `{name}`"),
                            *name_span,
                        );
                        return;
                    };
                    match payload {
                        Some(p) => self.bind_pattern_into(p, &payload_ty, out, names),
                        None => {
                            self.diag(
                                "E0410",
                                format!("variant `{name}` needs a payload"),
                                *name_span,
                            );
                        }
                    }
                    return;
                }
                let Type::Enum(ename, _) = &enum_ty else {
                    if !enum_ty.is_error() {
                        self.diag(
                            "E0603",
                            format!("`{enum_ty}` is not an enum"),
                            *name_span,
                        );
                    }
                    return;
                };
                let Some(_def) = self.program.defs.enums.get(ename) else {
                    return;
                };
                let Some(payload_ty) = self.program.defs.variant(&enum_ty, name) else {
                    self.diag(
                        "E0314",
                        format!("`{ename}` has no variant `{name}`"),
                        *name_span,
                    );
                    return;
                };
                match (payload, payload_ty) {
                    (Some(p), Some(t)) => self.bind_pattern_into(p, &t, out, names),
                    (None, Some(t)) => {
                        let _ = t;
                        self.diag(
                            "E0410",
                            format!("variant `{name}` needs a payload"),
                            *name_span,
                        );
                    }
                    (Some(_), None) => {
                        self.diag(
                            "E0410",
                            format!("variant `{name}` has no payload"),
                            *name_span,
                        );
                    }
                    (None, None) => {}
                }
            }
            Pattern::Tuple { elems, span } => {
                let Type::Tuple(types) = expected else {
                    if !expected.is_error() {
                        self.diag(
                            "E0603",
                            format!("tuple pattern against `{expected}`"),
                            *span,
                        );
                    }
                    return;
                };
                if elems.len() != types.len() {
                    self.diag(
                        "E0603",
                        format!("pattern has {} fields, but `{expected}` has {}", elems.len(), types.len()),
                        *span,
                    );
                }
                for (p, t) in elems.iter().zip(types) {
                    self.bind_pattern_into(p, t, out, names);
                }
            }
            Pattern::Struct { ty, fields, span } => {
                let struct_ty = if let Some(p) = ty {
                    self.resolve_path_ty(p)
                } else {
                    expected.clone()
                };
                let Type::Struct(_sname, _) = &struct_ty else {
                    if !struct_ty.is_error() {
                        self.diag(
                            "E0603",
                            format!("struct pattern against `{struct_ty}`"),
                            *span,
                        );
                    }
                    return;
                };
                let mut seen = HashSet::new();
                for f in fields {
                    if !seen.insert(f.name.clone()) {
                        self.diag(
                            "E0503",
                            format!("field `{}` appears twice", f.name),
                            f.name_span,
                        );
                    }
                    let Some(fty) = self.program.defs.field(&struct_ty, &f.name) else {
                        self.diag(
                            "E0314",
                            format!("`{struct_ty}` has no field `{}`", f.name),
                            f.name_span,
                        );
                        continue;
                    };
                    match &f.pattern {
                        Some(p) => self.bind_pattern_into(p, &fty, out, names),
                        None => self.bind_pattern_into(
                            &Pattern::Bind {
                                name: f.name.clone(),
                                span: f.name_span,
                            },
                            &fty,
                            out,
                            names,
                        ),
                    }
                }
            }
            Pattern::Poison => {}
        }
    }

    fn pattern_is_refutable(&self, p: &Pattern) -> bool {
        match p {
            Pattern::Wild(_) | Pattern::Bind { .. } => false,
            Pattern::Tuple { elems, .. } => elems.iter().any(|p| self.pattern_is_refutable(p)),
            Pattern::Struct { fields, .. } => fields
                .iter()
                .filter_map(|f| f.pattern.as_ref())
                .any(|p| self.pattern_is_refutable(p)),
            Pattern::Variant { .. } | Pattern::Literal(_) | Pattern::Range { .. } => true,
            Pattern::Poison => false,
        }
    }

    // ------------------------------------------------------------------ match

    fn check_match(
        &mut self,
        scrutinee: &Expr,
        arms: &[Arm],
        expected: Option<&Type>,
        span: Span,
        statement: bool,
    ) -> Type {
        let scrutinee_ty = self.check_expr(scrutinee, None);
        let mut result = expected.cloned().unwrap_or(Type::Void);
        let mut first = true;
        for arm in arms {
            self.push_scope();
            for p in &arm.patterns {
                let bindings = self.bind_pattern(p, &scrutinee_ty);
                for (name, nspan, ty) in bindings {
                    self.declare(&name, ty, nspan, nspan, false);
                }
            }
            if let Some(g) = &arm.guard {
                self.check_condition(g);
            }
            match &arm.body {
                ArmBody::Block(b) => {
                    self.check_stmts(&b.stmts);
                }
                ArmBody::Expr(e) => {
                    let ty = self.check_expr(e, expected);
                    if !statement {
                        if first {
                            result = ty.clone();
                            first = false;
                        } else if expected.is_none() && !ty.is_never() && !types_equal(&ty, &result) {
                            if result.is_void() {
                                result = ty;
                            } else {
                                self.expect_type(ty, &result, e.span);
                            }
                        }
                    }
                }
            }
            self.pop_scope(false);
        }
        self.check_exhaustive(&scrutinee_ty, arms, span);
        result
    }

    fn check_exhaustive(&mut self, ty: &Type, arms: &[Arm], span: Span) {
        let has_else = arms.iter().any(|a| {
            a.is_else
                || a.patterns.iter().any(|p| matches!(p, Pattern::Wild(_) | Pattern::Bind { .. }))
        });
        if has_else {
            return;
        }
        let mut missing: Vec<String> = Vec::new();
        match ty {
            Type::Enum(name, _) => {
                if let Some(def) = self.program.defs.enums.get(name) {
                    let present: HashSet<&str> = arms
                        .iter()
                        .flat_map(|a| a.patterns.iter())
                        .filter_map(|p| match p {
                            Pattern::Variant { name, .. } => Some(name.as_str()),
                            _ => None,
                        })
                        .collect();
                    for (v, _) in &def.variants {
                        if !present.contains(v.as_str()) {
                            missing.push(v.clone());
                        }
                    }
                }
            }
            Type::Bool => {
                let present: HashSet<bool> = arms
                    .iter()
                    .flat_map(|a| a.patterns.iter())
                    .filter_map(|p| match p {
                        Pattern::Literal(Expr {
                            kind: ExprKind::Bool(b),
                            ..
                        }) => Some(*b),
                        _ => None,
                    })
                    .collect();
                if !present.contains(&true) {
                    missing.push("true".to_string());
                }
                if !present.contains(&false) {
                    missing.push("false".to_string());
                }
            }
            Type::Result(..) => {
                let present: HashSet<&str> = arms
                    .iter()
                    .flat_map(|a| a.patterns.iter())
                    .filter_map(|p| match p {
                        Pattern::Variant { name, .. } => Some(name.as_str()),
                        _ => None,
                    })
                    .collect();
                if !present.contains("ok") {
                    missing.push("ok".to_string());
                }
                if !present.contains("err") {
                    missing.push("err".to_string());
                }
            }
            Type::Optional(inner) => {
                let present: HashSet<&str> = arms
                    .iter()
                    .flat_map(|a| a.patterns.iter())
                    .filter_map(|p| match p {
                        Pattern::Variant { name, .. } => Some(name.as_str()),
                        _ => None,
                    })
                    .collect();
                let _ = inner;
                if !present.contains("none") {
                    missing.push("none".to_string());
                }
                if !present.contains("some") {
                    missing.push("some".to_string());
                }
            }
            Type::Int(_) | Type::External(_) | Type::Error => return,
            _ => {
                missing.push("_".to_string());
            }
        }
        if !missing.is_empty() {
            self.diag(
                "E0601",
                format!("this match does not cover: {}", missing.join(", ")),
                span,
            );
        }
    }

    // ------------------------------------------------------------- expressions

    fn check_expr(&mut self, expr: &Expr, expected: Option<&Type>) -> Type {
        let ty = self.check_expr_inner(expr, expected);
        self.expr_types.insert(expr.span, ty.clone());
        ty
    }

    fn check_expr_inner(&mut self, expr: &Expr, expected: Option<&Type>) -> Type {
        match &expr.kind {
            ExprKind::Int {
                value, negative, ..
            } => {
                let mut n = *value as i128;
                if *negative {
                    n = -n;
                }
                match expected {
                    Some(Type::Int(_)) => {
                        if fit_int(n, expected.unwrap()).is_none() {
                            self.diag(
                                "E0405",
                                format!("literal `{n}` does not fit in `{}`", expected.unwrap()),
                                expr.span,
                            );
                        }
                        expected.unwrap().clone()
                    }
                    Some(Type::Float(_)) => expected.unwrap().clone(),
                    Some(t) if !t.is_error() && !matches!(t, Type::Param(_) | Type::External(_)) => {
                        self.diag(
                            "E0401",
                            format!("expected `{t}`, found an integer literal"),
                            expr.span,
                        );
                        t.clone()
                    }
                    _ => {
                        if fit_int(n, &Type::i64()).is_none() {
                            self.diag(
                                "E0405",
                                format!("literal `{n}` does not fit in `i64`"),
                                expr.span,
                            );
                        }
                        Type::i64()
                    }
                }
            }
            ExprKind::Float(v) => match expected {
                Some(Type::Float(s)) => Type::Float(*s),
                Some(t) if !t.is_error() => {
                    let _ = v;
                    self.diag(
                        "E0401",
                        format!("expected `{t}`, found a float literal"),
                        expr.span,
                    );
                    t.clone()
                }
                _ => Type::Float(ScalarTy::F64),
            },
            ExprKind::Str(_) => Type::Slice {
                mutable: false,
                elem: Box::new(Type::u8()),
            },
            ExprKind::CStr(_) => Type::Ptr {
                mutable: false,
                inner: Box::new(Type::u8()),
            },
            ExprKind::Bool(_) => Type::Bool,
            ExprKind::Ident(name) => {
                if name == "_" {
                    return Type::Error;
                }
                if let Some(ty) = self.local(name, expr.span, false) {
                    return ty;
                }
                match self.program.resolve_value_name(&self.module, name) {
                    Some(ResolvedName::External(_)) => Type::External(name.clone()),
                    Some(ResolvedName::Local(canonical)) => {
                        if let Some(info) = self.program.consts.get(&canonical) {
                            return info.ty.clone();
                        }
                        Type::External(canonical)
                    }
                    None => Type::Error,
                }
            }
            ExprKind::Path(p) => {
                let ty = self.resolve_path_ty(p);
                Type::External(format!("type {ty}"))
            }
            ExprKind::Paren(inner) => self.check_expr(inner, expected),
            ExprKind::Unary { op, expr: inner, .. } => self.check_unary(*op, inner, expected, expr.span),
            ExprKind::Binary {
                op,
                lhs,
                rhs,
                op_span,
            } => self.check_binary(*op, lhs, rhs, *op_span, expected, expr.span),
            ExprKind::Call {
                callee,
                args,
                args_span,
            } => self.check_call(callee, args, *args_span, expr.span),
            ExprKind::Index {
                base,
                index,
                bracket,
            } => {
                if let Some(kernel_idx) = self.kernel_index.clone() {
                    let own = matches!(&index.kind, ExprKind::Ident(n) if n == &kernel_idx);
                    if !own {
                        self.diag(
                            "E1513",
                            "a kernel may index only at its own index",
                            index.span,
                        );
                    }
                }
                let base_ty = self.check_expr(base, None);
                let idx_ty = self.check_expr(index, Some(&Type::i64()));
                self.expect_type(idx_ty, &Type::i64(), index.span);
                match &base_ty {
                    Type::Array(_, elem) => (**elem).clone(),
                    Type::Slice { elem, .. } => (**elem).clone(),
                    Type::Vector(v) => match v.elem {
                        ScalarTy::F32 | ScalarTy::F64 => Type::Float(v.elem),
                        ScalarTy::Bool => Type::Bool,
                        other => Type::Int(other),
                    },
                    Type::Ptr { .. } => {
                        self.diag(
                            "E0418",
                            "index a raw pointer through `p.*` in an `unsafe` block",
                            *bracket,
                        );
                        Type::Error
                    }
                    Type::Error => Type::Error,
                    _ => {
                        self.diag(
                            "E0416",
                            format!("`{base_ty}` is not indexable"),
                            *bracket,
                        );
                        Type::Error
                    }
                }
            }
            ExprKind::Slice {
                base,
                lo,
                hi,
                bracket,
            } => {
                let base_ty = self.check_expr(base, None);
                for b in [lo, hi] {
                    let t = self.check_expr(b, Some(&Type::i64()));
                    self.expect_type(t, &Type::i64(), b.span);
                }
                match &base_ty {
                    Type::Slice { mutable, elem } => Type::Slice {
                        mutable: *mutable,
                        elem: elem.clone(),
                    },
                    Type::Array(_, elem) => {
                        self.diag(
                            "E0420",
                            "slice an array through `&` or `&mut`",
                            *bracket,
                        );
                        Type::Slice {
                            mutable: false,
                            elem: elem.clone(),
                        }
                    }
                    Type::External(_) | Type::Error => Type::Error,
                    _ => {
                        self.diag(
                            "E0416",
                            format!("`{base_ty}` cannot be sliced"),
                            *bracket,
                        );
                        Type::Error
                    }
                }
            }
            ExprKind::Field {
                base,
                name,
                name_span,
            } => {
                if let ExprKind::Ident(alias) = &base.kind
                    && let Some(crate::check::ResolvedName::Local(canonical)) =
                        self.program.resolve_qualified_name(&self.module, alias, name)
                        && let Some(info) = self.program.consts.get(&canonical) {
                            return info.ty.clone();
                        }
                let base_ty = self.check_expr(base, None);
                if base_ty.is_error() {
                    return Type::Error;
                }
                let lookup_ty = base_ty.strip_borrows().clone();
                if let Some(ft) = self.program.defs.field(&lookup_ty, name) {
                    self.check_member_visibility(&lookup_ty, name, *name_span);
                    return ft;
                }
                match &base_ty {
                    Type::External(_) => Type::Error,
                    Type::Enum(name2, _) => {
                        self.diag(
                            "E0314",
                            format!("read enum payload `{name}` with `match`, not `.` on `{name2}`"),
                            *name_span,
                        );
                        Type::Error
                    }
                    Type::Tuple(_) => {
                        self.diag(
                            "E0314",
                            format!("use `.0`, `.1`, ... for tuple fields, not `.{}`", name),
                            *name_span,
                        );
                        Type::Error
                    }
                    _ => {
                        self.diag(
                            "E0314",
                            format!("`{base_ty}` has no member `{name}`"),
                            *name_span,
                        );
                        Type::Error
                    }
                }
            }
            ExprKind::TupleField {
                base,
                index,
                span,
            } => {
                let base_ty = self.check_expr(base, None);
                match &base_ty {
                    Type::Tuple(es) => es.get(*index as usize).cloned().unwrap_or_else(|| {
                        self.diag(
                            "E0314",
                            format!("`{base_ty}` has no field `{index}`"),
                            *span,
                        );
                        Type::Error
                    }),
                    Type::Error => Type::Error,
                    _ => {
                        self.diag(
                            "E0314",
                            format!("`{base_ty}` is not a tuple"),
                            *span,
                        );
                        Type::Error
                    }
                }
            }
            ExprKind::Deref { base, op_span } => {
                let ty = self.check_expr(base, None);
                match ty {
                    Type::Borrow { inner, .. } => (*inner).clone(),
                    Type::Ptr { .. } => {
                        if self.unsafe_depth == 0 {
                            self.diag(
                                "E1301",
                                "dereferencing a raw pointer needs an `unsafe` block",
                                *op_span,
                            );
                        }
                        match ty {
                            Type::Ptr { inner, .. } => (*inner).clone(),
                            _ => Type::Error,
                        }
                    }
                    Type::Error => Type::Error,
                    _ => {
                        self.diag(
                            "E0418",
                            format!("`{ty}` is not a pointer or borrow"),
                            *op_span,
                        );
                        Type::Error
                    }
                }
            }
            ExprKind::TypeApply { base, args, span } => {
                for t in args {
                    self.resolve_ty(t);
                }
                let _ = span;
                self.check_expr(base, expected)
            }
            ExprKind::Cast {
                expr: inner,
                ty,
                as_span,
            } => {
                let from = self.check_expr(inner, None);
                let to = self.resolve_ty(ty);
                if !lossless(&from, &to) {
                    self.diag(
                        "E0402",
                        format!(
                            "`as` cannot convert `{from}` to `{to}` without losing values; use `@wrap`, `@narrow`, `@sat` or `@nearest`"
                        ),
                        *as_span,
                    );
                }
                to
            }
            ExprKind::StructLit { ty, fields, braces } => {
                let structural = self.resolve_path_ty(ty);
                self.check_struct_lit(&structural, fields, *braces)
            }
            ExprKind::StructShorthand { fields, braces } => {
                let Some(expected) = expected else {
                    self.diag(
                        "E0404",
                        "`.\u{7b}...\u{7d}` needs an expected struct type",
                        *braces,
                    );
                    return Type::Error;
                };
                self.check_struct_lit(expected, fields, *braces)
            }
            ExprKind::EnumLit {
                path,
                variant,
                variant_span,
                payload,
            } => {
                let enum_ty = if let Some(p) = path {
                    self.resolve_path_ty(p)
                } else if let Some(e) = expected {
                    e.clone()
                } else {
                    self.diag(
                        "E0404",
                        "`.variant` needs an expected enum type",
                        *variant_span,
                    );
                    return Type::Error;
                };
                let Type::Enum(name, _) = &enum_ty else {
                    if !enum_ty.is_error() {
                        self.diag(
                            "E0603",
                            format!("`{enum_ty}` is not an enum"),
                            *variant_span,
                        );
                    }
                    return Type::Error;
                };
                let Some(_def) = self.program.defs.enums.get(name) else {
                    return Type::Error;
                };
                let Some(payload_ty) = self.program.defs.variant(&enum_ty, variant) else {
                    self.diag(
                        "E0314",
                        format!("`{name}` has no variant `{variant}`"),
                        *variant_span,
                    );
                    return Type::Error;
                };
                match (payload, payload_ty) {
                    (Some(p), Some(t)) => {
                        let actual = self.check_expr(p, Some(&t));
                        self.expect_type(actual, &t, p.span);
                    }
                    (None, Some(_)) => {
                        self.diag(
                            "E0410",
                            format!("variant `{variant}` needs a payload"),
                            *variant_span,
                        );
                    }
                    (Some(p), None) => {
                        self.diag(
                            "E0410",
                            format!("variant `{variant}` has no payload"),
                            p.span,
                        );
                        self.check_expr(p, None);
                    }
                    (None, None) => {}
                }
                enum_ty
            }
            ExprKind::ArrayLit {
                len,
                elem,
                elems,
                span,
            } => {
                let n = self.const_i64(len).unwrap_or(0);
                let elem_ty = self.resolve_ty(elem);
                for e in elems {
                    let actual = self.check_expr(e, Some(&elem_ty));
                    self.expect_type(actual, &elem_ty, e.span);
                }
                if n >= 0 && elems.len() as i64 != n {
                    self.diag(
                        "E0401",
                        format!("array literal has {} elements, but its type has {n}", elems.len()),
                        *span,
                    );
                }
                Type::Array(n, Box::new(elem_ty))
            }
            ExprKind::Range { lo, hi, .. } => {
                let lo_ty = self.check_expr(lo, None);
                self.check_expr(hi, Some(&lo_ty));
                lo_ty
            }
            ExprKind::Tuple { elems, .. } => {
                let expected_tuple = match expected {
                    Some(Type::Tuple(ts)) if ts.len() == elems.len() => Some(ts.clone()),
                    _ => None,
                };
                Type::Tuple(
                    elems
                        .iter()
                        .enumerate()
                        .map(|(i, e)| {
                            self.check_expr(e, expected_tuple.as_ref().map(|t| &t[i]))
                        })
                        .collect(),
                )
            }
            ExprKind::If {
                cond,
                then,
                otherwise,
                ..
            } => {
                let bindings = self.check_cond(cond);
                let then_ty = if bindings.is_empty() {
                    self.check_expr(then, expected)
                } else {
                    self.push_scope();
                    for (name, nspan, ty) in bindings {
                        self.declare(&name, ty, nspan, nspan, false);
                    }
                    let t = self.check_expr(then, expected);
                    self.pop_scope(false);
                    t
                };
                let else_ty = self.check_expr(otherwise, expected);
                if expected.is_none() {
                    if then_ty.is_never() {
                        else_ty
                    } else if else_ty.is_never()
                        || then_ty.is_error()
                        || types_equal(&then_ty, &else_ty)
                    {
                        then_ty
                    } else {
                        self.expect_type(else_ty, &then_ty, otherwise.span);
                        Type::Error
                    }
                } else {
                    expected.cloned().unwrap_or(Type::Error)
                }
            }
            ExprKind::Match {
                scrutinee,
                arms,
                span,
            } => self.check_match(scrutinee, arms, expected, *span, false),
            ExprKind::Closure {
                captures,
                params,
                ret,
                error: _,
                body,
                span,
            } => {
                let old_ret = self.ret.clone();
                let old_error = self.error.clone();
                self.ret = self.resolve_ty(ret);
                self.error = None;
                self.push_scope();
                for (name, _) in captures.iter().map(|c| (c.name.clone(), c.span)) {
                    self.declare(&name, Type::Error, Span::empty(span.start), Span::empty(span.start), false);
                }
                for p in params {
                    let ty = self.resolve_ty(&p.ty);
                    self.declare_param(&p.name, ty, p.name_span);
                }
                self.check_block(body);
                self.pop_scope(false);
                self.ret = old_ret;
                self.error = old_error;
                Type::External("closure".to_string())
            }
            ExprKind::Catch {
                value,
                binding,
                handler,
                ..
            } => {
                let value_ty = self.check_expr(value, None);
                let (ok_ty, err_ty) = match &value_ty {
                    Type::Result(a, b) => ((**a).clone(), (**b).clone()),
                    Type::Error => (Type::Error, Type::Error),
                    _ => {
                        self.diag(
                            "E0704",
                            format!("`catch` needs a result, found `{value_ty}`"),
                            value.span,
                        );
                        return Type::Error;
                    }
                };
                self.check_handler(handler, binding.as_deref(), &err_ty, &ok_ty);
                ok_ty
            }
            ExprKind::Orelse {
                value,
                handler,
                ..
            } => {
                let value_ty = self.check_expr(value, None);
                let (ok_ty, _) = match &value_ty {
                    Type::Optional(a) => ((**a).clone(), ()),
                    Type::Error => (Type::Error, ()),
                    _ => {
                        self.diag(
                            "E0704",
                            format!("`orelse` needs an optional, found `{value_ty}`"),
                            value.span,
                        );
                        return Type::Error;
                    }
                };
                self.check_handler(handler, None, &Type::Error, &ok_ty);
                ok_ty
            }
            ExprKind::Builtin {
                name,
                type_arg,
                args,
            } => self.check_builtin(name, type_arg.as_deref(), args, expr.span, expected),
            ExprKind::Spawn { call, scope, span } => {
                self.check_expr(call, None);
                self.check_spawn(call, scope, *span);
                self.check_expr(scope, None);
                Type::External("Task".to_string())
            }
            ExprKind::Dispatch {
                call,
                domain,
                executor,
                ..
            } => {
                let old = self.in_dispatch;
                self.in_dispatch = true;
                self.check_dispatch_call(call);
                self.in_dispatch = old;
                self.check_expr(domain, None);
                let _ = executor;
                Type::Void
            }
            ExprKind::Unsafe { reason, body, .. } => {
                if reason.trim().is_empty() {
                    self.diag("E1302", "an `unsafe` block needs a non-empty reason", expr.span);
                }
                if !expr_needs_unsafe(body) {
                    self.diag(
                        "E1304",
                        "this `unsafe` block contains no operation that needs it",
                        Span::new(expr.span.start, (expr.span.start + 6).min(expr.span.end)),
                    );
                }
                self.unsafe_depth += 1;
                let ty = self.check_expr(body, expected);
                self.unsafe_depth -= 1;
                ty
            }
            ExprKind::Poison => Type::Error,
        }
    }

    fn check_handler(
        &mut self,
        handler: &Handler,
        binding: Option<&str>,
        err_ty: &Type,
        ok_ty: &Type,
    ) {
        match handler {
            Handler::Expr(e) => {
                let ty = self.check_expr(e, Some(ok_ty));
                self.expect_type(ty, ok_ty, e.span);
            }
            Handler::Block(b) => {
                self.push_scope();
                if let Some(name) = binding {
                    self.declare(name, err_ty.clone(), Span::empty(b.span.start), Span::empty(b.span.start), false);
                }
                self.check_stmts(&b.stmts);
                self.pop_scope(false);
                if !ok_ty.is_void() && !block_diverges(b) {
                    self.diag(
                        "E0707",
                        "this handler can fall through, but a value is needed",
                        Span::empty(b.span.end.saturating_sub(1)),
                    );
                }
            }
        }
    }

    fn check_condition(&mut self, expr: &Expr) -> Type {
        let ty = self.check_expr(expr, Some(&Type::Bool));
        if ty.is_bool() || ty.is_error() || ty.is_never() {
            return ty;
        }
        if ty.is_integer() {
            let d = self.diag(
                "E0403",
                format!("condition must be `bool`, found `{ty}`"),
                expr.span,
            );
            d.fix(
                "compare with zero",
                Edit {
                    span: Span::empty(expr.span.end),
                    text: " != 0".to_string(),
                },
            );
        } else {
            self.diag(
                "E0403",
                format!("condition must be `bool`, found `{ty}`"),
                expr.span,
            );
        }
        Type::Bool
    }

    fn check_unary(
        &mut self,
        op: UnOp,
        inner: &Expr,
        expected: Option<&Type>,
        span: Span,
    ) -> Type {
        match op {
            UnOp::Borrow | UnOp::BorrowMut => {
                let mutable = op == UnOp::BorrowMut;
                if let ExprKind::Slice {
                    base,
                    lo,
                    hi,
                    bracket,
                } = &inner.kind
                {
                    let base_ty = if mutable {
                        self.check_place(base, true)
                    } else {
                        self.check_expr(base, None)
                    };
                    for b in [lo, hi] {
                        let t = self.check_expr(b, Some(&Type::i64()));
                        self.expect_type(t, &Type::i64(), b.span);
                    }
                    let elem = match base_ty {
                        Type::Array(_, elem) => Some(elem),
                        Type::Slice { elem, .. } => Some(elem),
                        Type::Error => return Type::Error,
                        other => {
                            self.diag(
                                "E0416",
                                format!("`{other}` cannot be sliced"),
                                *bracket,
                            );
                            return Type::Error;
                        }
                    };
                    return Type::Slice {
                        mutable,
                        elem: elem.unwrap_or_else(|| Box::new(Type::Error)),
                    };
                }
                let ty = self.check_place(inner, mutable);
                Type::Borrow {
                    mutable,
                    inner: Box::new(ty),
                }
            }
            UnOp::Try => {
                let value_ty = self.check_expr(inner, None);
                let (ok, err) = match &value_ty {
                    Type::Result(a, b) => ((**a).clone(), (**b).clone()),
                    Type::Error => return Type::Error,
                    _ => {
                        self.diag(
                            "E0704",
                            format!("`try` needs a result, found `{value_ty}`"),
                            inner.span,
                        );
                        return Type::Error;
                    }
                };
                let Some(fn_err) = self.error.clone() else {
                    if self.is_test {
                        return ok;
                    }
                    self.diag(
                        "E0702",
                        "`try` in a function that cannot propagate an error",
                        span,
                    );
                    return ok;
                };
                if types_equal(&err, &fn_err) {
                    ok
                } else if let Type::Enum(name, _) = &fn_err {
                    if self
                        .program
                        .defs
                        .enums
                        .get(name)
                        .map(|d| d.from.contains_key(&err.to_string()))
                        .unwrap_or(false)
                    {
                        ok
                    } else {
                        let d = self.diag(
                            "E0703",
                            format!(
                                "`try` produces `{err}`, which this function's error type `{fn_err}` cannot hold"
                            ),
                            span,
                        );
                        d.relate("signature", Span::empty(span.start));
                        ok
                    }
                } else {
                    self.diag(
                        "E0703",
                        format!(
                            "`try` produces `{err}`, but this function fails with `{fn_err}`"
                        ),
                        span,
                    );
                    ok
                }
            }
            UnOp::Neg => {
                let ty = self.check_expr(inner, expected);
                if ty.is_integer() && !ty.is_signed_integer() {
                    self.diag("E0406", "unary `-` is not defined for unsigned integers", span);
                } else if !ty.is_numeric() && !ty.is_error() {
                    self.diag("E0406", format!("unary `-` is not defined for `{ty}`"), span);
                }
                ty
            }
            UnOp::NegWrap => {
                let ty = self.check_expr(inner, expected);
                if !ty.is_integer() && !ty.is_error() {
                    self.diag("E0406", format!("unary `-%` needs an integer, found `{ty}`"), span);
                }
                ty
            }
            UnOp::Not => {
                let ty = self.check_expr(inner, Some(&Type::Bool));
                if !ty.is_bool() && !ty.is_error() {
                    self.diag("E0403", format!("`!` needs `bool`, found `{ty}`"), span);
                }
                Type::Bool
            }
            UnOp::BitNot => {
                let ty = self.check_expr(inner, expected);
                if !ty.is_integer() && !ty.is_vector() && !ty.is_error() {
                    self.diag("E0406", format!("`~` needs an integer, found `{ty}`"), span);
                }
                ty
            }
        }
    }

    fn check_binary(
        &mut self,
        op: BinOp,
        lhs: &Expr,
        rhs: &Expr,
        op_span: Span,
        expected: Option<&Type>,
        _span: Span,
    ) -> Type {
        match op {
            BinOp::And | BinOp::Or => {
                self.check_condition(lhs);
                self.check_condition(rhs);
                return Type::Bool;
            }
            _ => {}
        }
        let hint = expected.filter(|t| t.is_numeric() || t.is_vector());
        let lhs_lit = is_literal(lhs);
        let rhs_lit = is_literal(rhs);
        let (lty, rty) = if lhs_lit && !rhs_lit {
            let r = self.check_expr(rhs, hint);
            let l = self.check_expr(lhs, Some(&r));
            (l, r)
        } else {
            let l = self.check_expr(lhs, hint);
            let rh = if l.is_numeric() || l.is_vector() {
                Some(&l)
            } else {
                hint
            };
            let r = self.check_expr(rhs, rh);
            (l, r)
        };
        self.check_binary_types(op, &lty, &rty, op_span)
    }

    fn check_binary_types(&mut self, op: BinOp, lty: &Type, rty: &Type, op_span: Span) -> Type {
        let _ = op_span;
        if lty.is_error() || rty.is_error() {
            return if op.is_comparison() { Type::Bool } else { lty.clone() };
        }
        if op.is_comparison() {
            if !types_equal(lty, rty) {
                // Literal inference may still need one more unification.
                self.diag(
                    "E0406",
                    format!("`{}` operands have different types: `{lty}` and `{rty}`", op.symbol()),
                    op_span,
                );
                return Type::Bool;
            }
            if matches!(op, BinOp::Eq | BinOp::Ne) {
                match lty {
                    Type::Struct(..) | Type::Enum(..) if !self.payload_free_enum(lty) => {
                        let d = self.diag(
                            "E0406",
                            format!("`==` is not defined for `{lty}`; use `.eq(&other)`"),
                            op_span,
                        );
                        if lty.is_named(&interface_eq_type(lty).unwrap_or_default()) {
                            let _ = d;
                        }
                        return Type::Bool;
                    }
                    Type::Bool
                    | Type::Int(_)
                    | Type::Float(_)
                    | Type::Vector(_)
                    | Type::Ptr { .. }
                    | Type::Enum(..) => {}
                    Type::External(_) => {}
                    _ => {
                        self.diag(
                            "E0406",
                            format!("`==` is not defined for `{lty}`"),
                            op_span,
                        );
                    }
                }
            } else if !(lty.is_numeric() || lty.is_bool() || lty.is_vector() || lty.is_error()) {
                self.diag(
                    "E0406",
                    format!("`{}` is not defined for `{lty}`", op.symbol()),
                    op_span,
                );
            }
            return Type::Bool;
        }
        if op.is_shift() {
            if !lty.is_integer() && !lty.is_vector() {
                self.diag(
                    "E0406",
                    format!("`{}` needs an integer left operand, found `{lty}`", op.symbol()),
                    op_span,
                );
            }
            if !rty.is_integer() {
                self.diag(
                    "E0406",
                    format!("shift count must be an integer, found `{rty}`"),
                    op_span,
                );
            }
            return lty.clone();
        }
        if !types_equal(lty, rty) {
            self.diag(
                "E0406",
                format!(
                    "`{}` operands have different types: `{lty}` and `{rty}`",
                    op.symbol()
                ),
                op_span,
            );
            return lty.clone();
        }
        if op.is_shift() {
            if !lty.is_integer() && !lty.is_vector() {
                self.diag(
                    "E0406",
                    format!("`{}` needs an integer left operand, found `{lty}`", op.symbol()),
                    op_span,
                );
            }
            if !rty.is_integer() {
                self.diag(
                    "E0406",
                    format!("shift count must be an integer, found `{rty}`"),
                    op_span,
                );
            }
            return lty.clone();
        }
        if op.is_bitwise() {
            if !lty.is_integer() && !lty.is_vector() {
                self.diag(
                    "E0406",
                    format!("`{}` needs integer operands, found `{lty}`", op.symbol()),
                    op_span,
                );
            }
            return lty.clone();
        }
        // Arithmetic.
        if let Type::Vector(v) = lty {
            if v.elem.is_integer()
                && matches!(op, BinOp::Add | BinOp::Sub | BinOp::Mul)
            {
                self.diag(
                    "E0407",
                    "integer vectors have no trapping `+ - *`; use `+% -% *%` or `+| -| *|`",
                    op_span,
                );
            }
            return lty.clone();
        }
        if self.kernel_index.is_some() {
            let trapping = match op {
                BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Rem => {
                    lty.is_integer()
                }
                BinOp::Shl | BinOp::Shr => true,
                _ => false,
            };
            if trapping {
                self.diag(
                    "E1512",
                    "this operation may trap and is not allowed in a kernel",
                    op_span,
                );
            }
        }
        if !lty.is_numeric() {
            self.diag(
                "E0406",
                format!("`{}` is not defined for `{lty}`", op.symbol()),
                op_span,
            );
            return lty.clone();
        }
        if matches!(op, BinOp::Rem) && lty.is_float() {
            self.diag(
                "E0406",
                "`%` is not defined on floats; use `@rem`",
                op_span,
            );
        }
        lty.clone()
    }

    fn payload_free_enum(&self, ty: &Type) -> bool {
        if let Type::Enum(n, _) = ty {
            self.program
                .defs
                .enums
                .get(n)
                .map(|d| d.variants.iter().all(|(_, p)| p.is_none()))
                .unwrap_or(false)
        } else {
            false
        }
    }

    fn constructible(&self, ty: &Type) -> bool {
        match ty {
            Type::Struct(name, _) => self
                .program
                .defs
                .structs
                .get(name)
                .map(|d| {
                    d.module == self.module
                        || (!d.fields.is_empty() && d.fields.iter().all(|(_, _, public)| *public))
                })
                .unwrap_or(true),
            Type::Enum(name, _) => self.program.defs.enums.contains_key(name),
            _ => true,
        }
    }

    fn check_member_visibility(&mut self, ty: &Type, name: &str, span: Span) {
        if let Type::Struct(struct_name, _) = ty
            && let Some(def) = self.program.defs.structs.get(struct_name)
                && def.module != self.module
                    && let Some((_, _, public)) =
                        def.fields.iter().find(|(f, _, _)| f == name)
                        && !*public {
                            self.diag(
                                "E0307",
                                format!("field `{name}` of `{struct_name}` is private"),
                                span,
                            )
                            .relate_in("declared", &def.file, def.span);
                        }
    }

    fn check_struct_lit(&mut self, structural: &Type, fields: &[FieldInit], braces: Span) -> Type {
        if structural.is_error() {
            for f in fields {
                self.check_expr(&f.value, None);
            }
            return Type::Error;
        }
        let Type::Struct(name, _) = structural else {
            if let Type::External(_) = structural {
                self.diag(
                    "E0315",
                    format!("cannot construct `{structural}`: its fields are not visible here"),
                    braces,
                );
                for f in fields {
                    self.check_expr(&f.value, None);
                }
                return Type::Error;
            }
            self.diag(
                "E0401",
                format!("`{structural}` is not a struct"),
                braces,
            );
            for f in fields {
                self.check_expr(&f.value, None);
            }
            return structural.clone();
        };
        let def = self.program.defs.structs.get(name).cloned();
        let Some(def) = def else {
            return structural.clone();
        };
        if def.module != self.module
            && (def.fields.is_empty() || def.fields.iter().any(|(_, _, public)| !public))
        {
            self.diag(
                "E0315",
                format!("cannot construct `{name}`: a field is not visible here"),
                braces,
            )
            .relate_in("declared", &def.file, def.span);
            for f in fields {
                self.check_expr(&f.value, None);
            }
            return structural.clone();
        }
        let mut seen = HashSet::new();
        for f in fields {
            if !seen.insert(f.name.clone()) {
                self.diag(
                    "E0503",
                    format!("field `{}` is given more than once", f.name),
                    f.name_span,
                );
            }
            if let Some(fty) = self.program.defs.field(structural, &f.name) {
                let actual = self.check_expr(&f.value, Some(&fty));
                self.expect_type(actual, &fty, f.value.span);
            } else {
                self.diag(
                    "E0503",
                    format!("`{name}` has no field `{}`", f.name),
                    f.name_span,
                );
                self.check_expr(&f.value, None);
            }
        }
        for (fname, _, _) in &def.fields {
            if !seen.contains(fname) {
                self.diag(
                    "E0503",
                    format!("field `{fname}` is missing from the literal"),
                    braces,
                );
            }
        }
        structural.clone()
    }

    fn check_call(
        &mut self,
        callee: &Expr,
        args: &[Expr],
        args_span: Span,
        call_span: Span,
    ) -> Type {
        match &callee.kind {
            ExprKind::Ident(name) => {
                let Some(f) = self.program.function_in(&self.module, name).cloned() else {
                    if let Some((_, local)) = self.find_local(name) {
                        let ty = local.ty.clone();
                        return self.check_fn_value_call(&ty, args, args_span, call_span);
                    }
                    self.diag("E0304", format!("undefined function `{name}`"), callee.span);
                    for a in args {
                        self.check_expr(a, None);
                    }
                    return Type::Error;
                };
                if f.decl.mods.kernel && !self.in_dispatch {
                    self.diag(
                        "E1515",
                        "a `kernel fn` is not called directly; use `dispatch`",
                        callee.span,
                    );
                }
                self.check_args(&f, args, args_span, false, call_span)
            }
            ExprKind::Field {
                base,
                name,
                name_span,
            } => {
                if let ExprKind::Ident(alias) = &base.kind {
                    if let Some(f) = self
                        .program
                        .function_qualified(&self.module, alias, name)
                        .cloned()
                    {
                        return self.check_args(&f, args, args_span, false, call_span);
                    }
                    if matches!(
                        self.program.resolve_qualified_name(&self.module, alias, name),
                        Some(ResolvedName::External(_))
                    ) {
                        for a in args {
                            self.check_expr(a, Some(&Type::Error));
                        }
                        return Type::Error;
                    }
                }
                if let ExprKind::Path(path) = &base.kind {
                    let target = self.resolve_path_ty(path);
                    if let Type::Enum(..) = target {
                        return self.check_variant_call(&target, name, *name_span, args, args_span, call_span);
                    }
                    if let Some(f) = self.program.method(&target, name).cloned() {
                        return self.check_args(&f, args, args_span, true, call_span);
                    }
                    if matches!(target, Type::External(_)) {
                        for a in args {
                            self.check_expr(a, Some(&Type::Error));
                        }
                        return Type::Error;
                    }
                } else {
                    let receiver_ty = self.check_expr(base, None);
                    if let Some(f) = self.program.method(&receiver_ty, name).cloned() {
                        if receiver_is_mut(&f) {
                            let _ = self.check_place(base, true);
                        }
                        return self.check_args(&f, args, args_span, true, call_span);
                    }
                    if matches!(receiver_ty, Type::External(_) | Type::Error) {
                        for a in args {
                            self.check_expr(a, Some(&Type::Error));
                        }
                        return Type::Error;
                    }
                    // A field holding a function value.
                    if let Type::Fn { .. } = receiver_ty {
                        let ft = receiver_ty.clone();
                        return self.check_fn_value_call(&ft, args, args_span, call_span);
                    }
                    self.diag(
                        "E0314",
                        format!("`{receiver_ty}` has no method `{name}`"),
                        *name_span,
                    );
                    for a in args {
                        self.check_expr(a, None);
                    }
                    return Type::Error;
                }
                self.diag(
                    "E0314",
                    format!("`{}` has no associated function `{name}`", base.span.text_or("")),
                    *name_span,
                );
                for a in args {
                    self.check_expr(a, None);
                }
                Type::Error
            }
            ExprKind::TypeApply { base, .. } => self.check_call(base, args, args_span, call_span),
            _ => {
                let ty = self.check_expr(callee, None);
                self.check_fn_value_call(&ty, args, args_span, call_span)
            }
        }
    }

    fn check_variant_call(
        &mut self,
        enum_ty: &Type,
        variant: &str,
        variant_span: Span,
        args: &[Expr],
        args_span: Span,
        call_span: Span,
    ) -> Type {
        let payload = self.program.defs.variant(enum_ty, variant);
        let Some(payload) = payload else {
            self.diag(
                "E0314",
                format!("`{enum_ty}` has no variant `{variant}`"),
                variant_span,
            );
            for a in args {
                self.check_expr(a, None);
            }
            return Type::Error;
        };
        match payload {
            Some(t) => {
                if args.len() != 1 {
                    self.diag(
                        "E0410",
                        format!("variant `{variant}` takes one argument"),
                        args_span,
                    );
                }
                for a in args {
                    let actual = self.check_expr(a, Some(&t));
                    self.expect_type(actual, &t, a.span);
                }
            }
            None => {
                if !args.is_empty() {
                    self.diag(
                        "E0410",
                        format!("variant `{variant}` takes no arguments"),
                        args_span,
                    );
                }
                for a in args {
                    self.check_expr(a, None);
                }
            }
        }
        let _ = call_span;
        enum_ty.clone()
    }

    fn check_fn_value_call(
        &mut self,
        ty: &Type,
        args: &[Expr],
        args_span: Span,
        call_span: Span,
    ) -> Type {
        match ty {
            Type::Fn {
                params,
                ret,
                error,
            } => {
                if params.len() != args.len() {
                    self.diag(
                        "E0410",
                        format!("this call takes {} arguments, but {} were given", params.len(), args.len()),
                        args_span,
                    );
                }
                for (a, p) in args.iter().zip(params) {
                    let actual = self.check_expr(a, Some(p));
                    self.expect_type(actual, p, a.span);
                }
                let _ = call_span;
                if let Some(e) = error {
                    Type::Result(ret.clone(), e.clone())
                } else {
                    (**ret).clone()
                }
            }
            Type::External(_) | Type::Error => {
                for a in args {
                    self.check_expr(a, None);
                }
                Type::Error
            }
            _ => {
                self.diag(
                    "E0411",
                    format!("`{ty}` is not callable"),
                    call_span,
                );
                for a in args {
                    self.check_expr(a, None);
                }
                Type::Error
            }
        }
    }

    fn check_args(
        &mut self,
        f: &Function,
        args: &[Expr],
        args_span: Span,
        method: bool,
        call_span: Span,
    ) -> Type {
        let skip = if method
            && f
                .decl
                .params
                .first()
                .map(|p| p.is_self())
                .unwrap_or(false)
        {
            1
        } else {
            0
        };
        let params: Vec<Type> = f.signature_params.iter().skip(skip).cloned().collect();
        if params.len() != args.len() {
            self.diag(
                "E0410",
                format!(
                    "`{}` takes {} arguments, but {} were given",
                    f.decl.name,
                    params.len(),
                    args.len()
                ),
                args_span,
            )
            .relate("callee", f.decl.name_span);
        }
        for (a, p) in args.iter().zip(params.iter()) {
            let actual = self.check_expr(a, Some(p));
            self.expect_type(actual, p, a.span);
        }
        for a in args.iter().skip(params.len()) {
            self.check_expr(a, None);
        }
        let _ = call_span;
        if let Some(e) = &f.signature_error {
            Type::Result(Box::new(f.signature_ret.clone()), Box::new(e.clone()))
        } else {
            f.signature_ret.clone()
        }
    }

    fn check_builtin(
        &mut self,
        name: &str,
        type_arg: Option<&Ty>,
        args: &[Expr],
        span: Span,
        expected: Option<&Type>,
    ) -> Type {
        let targ = type_arg.map(|t| self.resolve_ty(t));
        match name {
            "assert" => {
                if let Some(c) = args.first() {
                    self.check_condition(c);
                } else {
                    self.diag("E0410", "`@assert` needs a condition", span);
                }
                if let Some(msg) = args.get(1) {
                    let ty = self.check_expr(msg, None);
                    if !matches!(ty, Type::Slice { .. } | Type::Error) {
                        self.diag("E0401", "`@assert` message must be a string", msg.span);
                    }
                }
                Type::Void
            }
            "panic" | "unreachable" => {
                for a in args {
                    self.check_expr(a, None);
                }
                Type::Never
            }
            "debug" => {
                for a in args {
                    self.check_expr(a, None);
                }
                Type::Void
            }
            "zero" => {
                let Some(t) = expected.cloned().or(targ) else {
                    self.diag("E0404", "`@zero` needs an expected type", span);
                    return Type::Error;
                };
                if matches!(t, Type::External(_)) || !self.constructible(&t) {
                    self.diag(
                        "E0315",
                        format!("cannot construct `{t}`: its fields are not visible here"),
                        span,
                    );
                } else if t.is_view(&self.program.defs) {
                    self.diag("E0409", format!("`@zero` is not allowed for `{t}`"), span);
                }
                t
            }
            "narrow" | "wrap" | "sat" | "nearest" => {
                let Some(t) = targ else {
                    self.diag("E0410", format!("`@{name}` needs a target type"), span);
                    return Type::Error;
                };
                if let Some(a) = args.first() {
                    let at = self.check_expr(a, None);
                    if !at.is_numeric() && !at.is_error() {
                        self.diag(
                            "E0406",
                            format!("`@{name}` needs a numeric argument, found `{at}`"),
                            a.span,
                        );
                    }
                } else {
                    self.diag("E0410", format!("`@{name}` needs a value"), span);
                }
                t
            }
            "bits" => {
                let Some(t) = targ else {
                    self.diag("E0410", "`@bits` needs a target type", span);
                    return Type::Error;
                };
                if let Some(a) = args.first() {
                    let at = self.check_expr(a, None);
                    if type_size(&at, &self.program.defs) != type_size(&t, &self.program.defs) {
                        self.diag(
                            "E0408",
                            format!("`@bits` needs equal sizes, found `{at}` and `{t}`"),
                            span,
                        );
                    }
                    if at.is_view(&self.program.defs) || t.is_view(&self.program.defs) {
                        self.diag("E0408", "`@bits` cannot convert a borrow", span);
                    }
                }
                t
            }
            "sizeof" | "alignof" | "offsetof" => {
                if let Some(t) = targ {
                    let _ = type_size(&t, &self.program.defs);
                }
                for a in args {
                    self.check_expr(a, None);
                }
                Type::i64()
            }
            "lanes" => {
                if let Some(t) = targ
                    && let Type::Vector(_v) = t {
                        return Type::Int(ScalarTy::I64);
                    }
                self.diag("E0410", "`@lanes` needs a vector type", span);
                Type::i64()
            }
            "splat" => {
                let Some(t) = targ else {
                    self.diag("E0410", "`@splat` needs a vector type", span);
                    return Type::Error;
                };
                let elem = match &t {
                    Type::Vector(v) => Type::Int(v.elem),
                    _ => Type::Error,
                };
                if let Some(a) = args.first() {
                    let at = self.check_expr(a, Some(&elem));
                    self.expect_type(at, &elem, a.span);
                }
                t
            }
            "load" | "load_tail" => {
                let Some(t) = targ else {
                    self.diag("E0410", "a vector load needs a vector type", span);
                    return Type::Error;
                };
                if let Some(s) = args.first() {
                    self.check_expr(s, None);
                }
                if let Some(i) = args.get(1) {
                    let it = self.check_expr(i, Some(&Type::i64()));
                    self.expect_type(it, &Type::i64(), i.span);
                }
                t
            }
            "store" | "store_tail" => {
                for a in args {
                    self.check_expr(a, None);
                }
                Type::Void
            }
            "count" => {
                if let Some(a) = args.first() {
                    self.check_expr(a, None);
                }
                Type::i64()
            }
            "reduce_add" | "reduce_min" | "reduce_max" | "reduce_and" | "reduce_or" => {
                let Some(a) = args.first() else {
                    return Type::Error;
                };
                let ty = self.check_expr(a, None);
                match ty {
                    Type::Vector(v) => match v.elem {
                        ScalarTy::F32 | ScalarTy::F64 => Type::Float(v.elem),
                        ScalarTy::Bool => Type::Bool,
                        other => Type::Int(other),
                    },
                    Type::Error => Type::Error,
                    other => {
                        self.diag(
                            "E0406",
                            format!("`@{name}` needs a vector, found `{other}`"),
                            a.span,
                        );
                        Type::Error
                    }
                }
            }
            "select" => {
                let ty = args.first().map(|a| self.check_expr(a, None));
                let _ = ty;
                let t = args
                    .get(1)
                    .map(|a| self.check_expr(a, expected))
                    .unwrap_or(Type::Error);
                if let Some(b) = args.get(2) {
                    let bt = self.check_expr(b, Some(&t));
                    self.expect_type(bt, &t, b.span);
                }
                t
            }
            "insert" => {
                if let Some(v) = args.first() {
                    let ty = self.check_expr(v, expected);
                    if let Some(i) = args.get(1) {
                        let it = self.check_expr(i, Some(&Type::i64()));
                        self.expect_type(it, &Type::i64(), i.span);
                    }
                    return ty;
                }
                Type::Error
            }
            "shuffle" => args
                .first()
                .map(|a| self.check_expr(a, expected))
                .unwrap_or(Type::Error),
            "fma" | "rem" | "min" | "max" | "sqrt" | "abs" | "floor" | "ceil" | "round" => {
                let ty = args
                    .first()
                    .map(|a| self.check_expr(a, expected))
                    .unwrap_or(Type::Error);
                for a in args.iter().skip(1) {
                    let at = self.check_expr(a, Some(&ty));
                    self.expect_type(at, &ty, a.span);
                }
                ty
            }
            "forget" => {
                if let Some(a) = args.first() {
                    self.check_expr(a, None);
                }
                Type::Void
            }
            "replace" => {
                if let Some(a) = args.first() {
                    let ty = self.check_expr(a, None);
                    if let Type::Borrow { mutable: true, inner } = ty {
                        (*inner).clone()
                    } else {
                        self.diag("E0406", "`@replace` needs `&mut place`", a.span);
                        Type::Error
                    }
                } else {
                    Type::Error
                }
            }
            "swap" => {
                for a in args {
                    self.check_expr(a, None);
                }
                Type::Void
            }
            "enum" => {
                let Some(t) = targ else {
                    self.diag("E0410", "`@enum` needs an enum type", span);
                    return Type::Error;
                };
                if let Some(a) = args.first() {
                    self.check_expr(a, None);
                }
                Type::Optional(Box::new(t))
            }
            "target" => Type::External("target".to_string()),
            "format" => Type::External("format".to_string()),
            _ => {
                for a in args {
                    self.check_expr(a, None);
                }
                Type::Error
            }
        }
    }

    fn check_dispatch_call(&mut self, call: &Expr) {
        if let ExprKind::Call { callee, args, .. } = &call.kind
            && let ExprKind::Ident(n) = &callee.kind
                && let Some(f) = self.program.function_in(&self.module, n).cloned() {
                    for (i, a) in args.iter().enumerate() {
                        let expected = f.signature_params.get(i + 1);
                        let actual = self.check_expr(a, expected);
                        if let Some(e) = expected {
                            self.expect_type(actual, e, a.span);
                        }
                    }
                    return;
                }
        self.check_expr(call, None);
    }

    fn check_into_store(&mut self, place: &Expr, value: &Expr, actual: &Type, span: Span) {
        if !actual.is_view(&self.program.defs) || actual.contains_param() {
            return;
        }
        let Some(target) = expr_root_ident(place) else {
            return;
        };
        let Some(source) = expr_root_ident(value) else {
            return;
        };
        if target == source {
            return;
        }
        let target_local = self.find_local(target).map(|(_, l)| (l.is_param, l.ty.clone()));
        let source_local = self.find_local(source).map(|(_, l)| (l.is_param, l.ty.clone()));
        let (true, target_ty) = target_local.unwrap_or((false, Type::Error)) else {
            return;
        };
        let (true, _) = source_local.unwrap_or((false, Type::Error)) else {
            return;
        };
        if !matches!(target_ty, Type::Borrow { mutable: true, .. } | Type::Slice { mutable: true, .. }) {
            return;
        }
        if self.param_into.get(source).and_then(|x| x.as_ref()) == Some(&target.to_string()) {
            return;
        }
        self.diag(
            "E1204",
            format!(
                "the borrow in `{source}` is stored through `{target}`, but `{source}` is not declared `into {target}`"
            ),
            span,
        );
    }

    fn check_spawn(&mut self, call: &Expr, scope: &Expr, span: Span) {
        let ExprKind::Call { args, .. } = &call.kind else {
            return;
        };
        let scope_name = expr_root_ident(scope);
        // Every argument must be `Send`.
        for a in args {
            let ty = self.check_expr(a, None);
            if !ty.is_send(&self.program.defs) && !ty.is_error() {
                self.diag(
                    "E1401",
                    format!("`{ty}` is not `Send` and cannot cross into a task"),
                    a.span,
                );
            }
        }
        // A view parameter stored into a scope parameter needs `into s`.
        if let Some(scope_name) = scope_name {
            let scope_param = self
                .find_local(scope_name)
                .map(|(_, l)| (l.is_param, l.ty.clone()))
                .unwrap_or((false, Type::Error));
            if scope_param.0
                && let Type::Borrow { inner, .. } = &scope_param.1
                    && matches!(inner.as_ref(), Type::External(n) if n == "Scope") {
                        for a in args {
                            let ty = self.check_expr(a, None);
                            if !ty.is_view(&self.program.defs) {
                                continue;
                            }
                            let Some(source) = expr_root_ident(a) else {
                                continue;
                            };
                            if source == scope_name {
                                continue;
                            }
                            if self.param_into.get(source).and_then(|x| x.as_ref())
                                == Some(&scope_name.to_string())
                            {
                                continue;
                            }
                            self.diag(
                                "E1204",
                                format!(
                                    "`{source}` is given to a task in scope `{scope_name}` without `into {scope_name}`"
                                ),
                                a.span,
                            );
                        }
                    }
        }
        let _ = span;
    }

    fn check_return_origin(&mut self, value: &Expr, ret: &Type) {
        if !ret.is_view(&self.program.defs) {
            return;
        }
        let Some(root) = expr_root_ident(value) else {
            return;
        };
        let Some((_, local)) = self.find_local(root) else {
            return;
        };
        if !local.is_param {
            return;
        }
        let is_exclusive = matches!(
            &local.ty,
            Type::Borrow { mutable: true, .. } | Type::Slice { mutable: true, .. }
        );
        if !is_exclusive {
            return;
        }
        let plain = self
            .from
            .iter()
            .any(|o| !o.is_static && !o.inner && o.name == root);
        if !plain {
            self.diag(
                "E1202",
                format!(
                    "this reborrow through `{root}` has origin `{root}`, which the `from` clause does not cover"
                ),
                value.span,
            );
        }
    }

    fn check_place(&mut self, expr: &Expr, mutable: bool) -> Type {
        let ty = self.check_place_inner(expr, mutable);
        self.expr_types.insert(expr.span, ty.clone());
        ty
    }

    fn check_place_inner(&mut self, expr: &Expr, mutable: bool) -> Type {
        match &expr.kind {
            ExprKind::Ident(name) => {
                if mutable {
                    self.local(name, expr.span, true).unwrap_or(Type::Error)
                } else {
                    self.local(name, expr.span, false).unwrap_or(Type::Error)
                }
            }
            ExprKind::Paren(inner) => self.check_place(inner, mutable),
            ExprKind::Field {
                base,
                name,
                name_span,
            } => {
                let base_ty = self.check_place(base, mutable);
                let lookup_ty = base_ty.strip_borrows().clone();
                if let Some(ft) = self.program.defs.field(&lookup_ty, name) {
                    self.check_member_visibility(&lookup_ty, name, *name_span);
                    ft
                } else {
                    if !base_ty.is_error() {
                        self.diag(
                            "E0314",
                            format!("`{base_ty}` has no field `{name}`"),
                            *name_span,
                        );
                    }
                    Type::Error
                }
            }
            ExprKind::TupleField {
                base,
                index,
                span,
            } => {
                let base_ty = self.check_place(base, mutable);
                match base_ty {
                    Type::Tuple(es) => es.get(*index as usize).cloned().unwrap_or_else(|| {
                        self.diag("E0314", format!("no tuple field `{index}`"), *span);
                        Type::Error
                    }),
                    Type::Error => Type::Error,
                    other => {
                        self.diag("E0314", format!("`{other}` is not a tuple"), *span);
                        Type::Error
                    }
                }
            }
            ExprKind::Index { base, index, .. } => {
                let base_ty = self.check_place(base, mutable);
                let idx_ty = self.check_expr(index, Some(&Type::i64()));
                self.expect_type(idx_ty, &Type::i64(), index.span);
                match base_ty {
                    Type::Array(_, elem) | Type::Slice { elem, .. } => (*elem).clone(),
                    Type::Vector(v) => Type::Int(v.elem),
                    other => {
                        self.diag("E0416", format!("`{other}` is not indexable"), expr.span);
                        Type::Error
                    }
                }
            }
            ExprKind::Deref { base, .. } => {
                let ty = self.check_expr(base, None);
                match ty {
                    Type::Borrow { mutable: m, inner } => {
                        if mutable && !m {
                            self.diag(
                                "E0606",
                                "cannot write through a shared borrow",
                                expr.span,
                            );
                        }
                        (*inner).clone()
                    }
                    Type::Ptr { inner, .. } => (*inner).clone(),
                    Type::Error => Type::Error,
                    other => {
                        self.diag("E0607", format!("`{other}` is not a place"), expr.span);
                        Type::Error
                    }
                }
            }
            _ => {
                self.diag("E0607", "this expression is not a place", expr.span);
                self.check_expr(expr, None);
                Type::Error
            }
        }
    }

    fn check_assign_place(&mut self, expr: &Expr) -> Option<Type> {
        let ty = self.check_assign_place_inner(expr);
        if let Some(t) = &ty {
            self.expr_types.insert(expr.span, t.clone());
        }
        ty
    }

    fn check_assign_place_inner(&mut self, expr: &Expr) -> Option<Type> {
        match &expr.kind {
            ExprKind::Ident(name) if name == "_" => None,
            ExprKind::Ident(name) => self.local_for_assign(name, expr.span),
            ExprKind::Paren(inner) => self.check_assign_place(inner),
            ExprKind::Field {
                base,
                name,
                name_span,
            } => {
                let base_ty = self.check_assign_place(base)?;
                let lookup_ty = base_ty.strip_borrows().clone();
                if let Some(ft) = self.program.defs.field(&lookup_ty, name) {
                    self.check_member_visibility(&lookup_ty, name, *name_span);
                    Some(ft)
                } else {
                    if !base_ty.is_error() {
                        self.diag(
                            "E0314",
                            format!("`{base_ty}` has no field `{name}`"),
                            *name_span,
                        );
                    }
                    Some(Type::Error)
                }
            }
            ExprKind::TupleField {
                base,
                index,
                span,
            } => {
                let base_ty = self.check_assign_place(base)?;
                match base_ty {
                    Type::Tuple(es) => Some(es.get(*index as usize).cloned().unwrap_or(Type::Error)),
                    _ => {
                        self.diag("E0314", format!("no tuple field `{index}`"), *span);
                        Some(Type::Error)
                    }
                }
            }
            ExprKind::Index { base, index, .. } => {
                let base_ty = self.check_place(base, true);
                let idx_ty = self.check_expr(index, Some(&Type::i64()));
                self.expect_type(idx_ty, &Type::i64(), index.span);
                match base_ty {
                    Type::Array(_, elem) | Type::Slice { elem, .. } => Some((*elem).clone()),
                    _ => Some(Type::Error),
                }
            }
            ExprKind::Deref { base, .. } => {
                let ty = self.check_expr(base, None);
                match ty {
                    Type::Borrow { mutable: true, inner } => Some((*inner).clone()),
                    Type::Borrow { mutable: false, .. } => {
                        self.diag(
                            "E0606",
                            "cannot write through a shared borrow",
                            expr.span,
                        );
                        Some(Type::Error)
                    }
                    Type::Ptr { inner, .. } => Some((*inner).clone()),
                    _ => {
                        self.diag("E0607", "this expression is not a place", expr.span);
                        Some(Type::Error)
                    }
                }
            }
            _ => {
                self.diag("E0607", "this expression is not a place", expr.span);
                self.check_expr(expr, None);
                Some(Type::Error)
            }
        }
    }

    fn expect_type(&mut self, actual: Type, expected: &Type, span: Span) -> Type {
        if expected.is_error()
            || actual.is_error()
            || actual.is_never()
            || types_equal(&actual, expected)
            || expected.contains_param()
            || actual.contains_param()
            || is_idx(&actual)
            || is_idx(expected)
        {
            return actual;
        }
        if let Type::Result(inner, _) = &actual
            && types_equal(inner, expected) {
                let d = self.diag(
                    "E0701",
                    "a result value cannot be used as its success type; handle it with `try` or `catch`",
                    span,
                );
                d.fix(
                    "insert `try `",
                    Edit {
                        span: Span::empty(span.start),
                        text: "try ".to_string(),
                    },
                );
                return inner.as_ref().clone();
            }
        let conversion = format!(" as {expected}");
        let can_convert = lossless(&actual, expected);
        let d = self.diag(
            "E0401",
            format!("expected `{expected}`, found `{actual}`"),
            span,
        );
        if can_convert {
            d.fix(
                format!("convert with `{conversion}`"),
                Edit {
                    span: Span::empty(span.end),
                    text: conversion,
                },
            );
        }
        let _ = d;
        expected.clone()
    }
}

fn find_outer_mutation(
    stmts: &[Stmt],
    outer: &HashSet<String>,
    declared: &mut HashSet<String>,
    out: &mut Vec<Span>,
) {
    for stmt in stmts {
        match stmt {
            Stmt::Let { pattern, .. } => {
                for n in pattern_binding_names(pattern) {
                    declared.insert(n);
                }
            }
            Stmt::Assign { place, span, .. } => {
                if let Some(root) = expr_root_ident(place)
                    && outer.contains(root) && !declared.contains(root) {
                        out.push(*span);
                    }
            }
            Stmt::Expr { expr, .. } => find_outer_mutation_expr(expr, outer, declared, out),
            Stmt::Block(b) => find_outer_mutation(&b.stmts, outer, declared, out),
            Stmt::If(x) => {
                find_outer_mutation(&x.then.stmts, outer, declared, out);
                if let Some(e) = &x.otherwise {
                    find_outer_mutation(std::slice::from_ref(e), outer, declared, out);
                }
            }
            Stmt::While { body, .. } | Stmt::Loop { body, .. } | Stmt::Scope { body, .. } => {
                find_outer_mutation(&body.stmts, outer, declared, out)
            }
            Stmt::For { body, .. } => find_outer_mutation(&body.stmts, outer, declared, out),
            Stmt::Match(m) => {
                for a in &m.arms {
                    match &a.body {
                        ArmBody::Block(b) => find_outer_mutation(&b.stmts, outer, declared, out),
                        ArmBody::Expr(e) => find_outer_mutation_expr(e, outer, declared, out),
                    }
                }
            }
            _ => {}
        }
    }
}

fn find_outer_mutation_expr(
    e: &Expr,
    outer: &HashSet<String>,
    declared: &HashSet<String>,
    out: &mut Vec<Span>,
) {
    match &e.kind {
        ExprKind::Unary {
            op: UnOp::BorrowMut,
            expr,
            op_span,
        } => {
            if let Some(root) = expr_root_ident(expr)
                && outer.contains(root) && !declared.contains(root) {
                    out.push(*op_span);
                }
        }
        ExprKind::Call { args, .. } => {
            for a in args {
                find_outer_mutation_expr(a, outer, declared, out);
            }
        }
        ExprKind::Paren(x) => find_outer_mutation_expr(x, outer, declared, out),
        _ => {}
    }
}

fn pattern_binding_names(p: &Pattern) -> Vec<String> {
    let mut out = Vec::new();
    fn walk(p: &Pattern, out: &mut Vec<String>) {
        match p {
            Pattern::Bind { name, .. } if name != "_" => out.push(name.clone()),
            Pattern::Tuple { elems, .. } => {
                for e in elems {
                    walk(e, out);
                }
            }
            Pattern::Struct { fields, .. } => {
                for f in fields {
                    match &f.pattern {
                        Some(p) => walk(p, out),
                        None => out.push(f.name.clone()),
                    }
                }
            }
            Pattern::Variant {
                payload: Some(p), ..
            } => walk(p, out),
            Pattern::Variant { payload: None, .. } => {}
            _ => {}
        }
    }
    walk(p, &mut out);
    out
}

fn expr_root_ident(e: &Expr) -> Option<&str> {
    match &e.kind {
        ExprKind::Ident(name) if name != "_" => Some(name),
        ExprKind::Paren(x) => expr_root_ident(x),
        ExprKind::Field { base, .. }
        | ExprKind::TupleField { base, .. }
        | ExprKind::Deref { base, .. }
        | ExprKind::Index { base, .. } => expr_root_ident(base),
        ExprKind::Unary {
            op: UnOp::Borrow | UnOp::BorrowMut,
            expr,
            ..
        } => expr_root_ident(expr),
        ExprKind::Slice { base, .. } => expr_root_ident(base),
        _ => None,
    }
}

fn is_idx(t: &Type) -> bool {
    matches!(t, Type::External(n) if n == "Idx" || n == "Idx2" || n == "Idx3")
}

fn expr_needs_unsafe(e: &Expr) -> bool {
    match &e.kind {
        ExprKind::Deref { base, .. } => matches!(
            &base.kind,
            ExprKind::Ident(_) | ExprKind::Paren(_)
        ),
        ExprKind::Field { name, .. } if name == "ptr" => true,
        ExprKind::Unary { expr, .. } | ExprKind::Cast { expr, .. } => expr_needs_unsafe(expr),
        ExprKind::Binary { lhs, rhs, .. } => expr_needs_unsafe(lhs) || expr_needs_unsafe(rhs),
        ExprKind::Call { callee, args, .. } => {
            matches!(&callee.kind, ExprKind::Ident(n) if n.starts_with("unchecked"))
                || expr_needs_unsafe(callee)
                || args.iter().any(expr_needs_unsafe)
        }
        ExprKind::Builtin { name, args, .. } => {
            matches!(
                name.as_str(),
                "uninit"
                    | "assume"
                    | "slice_from_raw"
                    | "borrow_from_raw"
                    | "unchecked_index"
                    | "unchecked_add"
                    | "unchecked_sub"
                    | "unchecked_mul"
                    | "unchecked_div"
            ) || args.iter().any(expr_needs_unsafe)
        }
        ExprKind::Index { base, index, .. } => expr_needs_unsafe(base) || expr_needs_unsafe(index),
        ExprKind::Slice { base, lo, hi, .. } => {
            expr_needs_unsafe(base) || expr_needs_unsafe(lo) || expr_needs_unsafe(hi)
        }
        ExprKind::Tuple { elems, .. } | ExprKind::ArrayLit { elems, .. } => {
            elems.iter().any(expr_needs_unsafe)
        }
        ExprKind::StructLit { fields, .. } | ExprKind::StructShorthand { fields, .. } => {
            fields.iter().any(|f| expr_needs_unsafe(&f.value))
        }
        ExprKind::EnumLit { payload, .. } => {
            payload.as_ref().map(|p| expr_needs_unsafe(p)).unwrap_or(false)
        }
        ExprKind::Paren(x) => expr_needs_unsafe(x),
        _ => false,
    }
}

fn block_needs_unsafe(b: &Block) -> bool {
    fn stmt(s: &Stmt) -> bool {
        match s {
            Stmt::Let { init, .. } => expr_needs_unsafe(init),
            Stmt::Assign { place, value, .. } => {
                expr_needs_unsafe(place) || expr_needs_unsafe(value)
            }
            Stmt::Expr { expr, .. } => expr_needs_unsafe(expr),
            Stmt::Return { value, .. } => value.as_ref().map(expr_needs_unsafe).unwrap_or(false),
            Stmt::Fail { value, .. } => expr_needs_unsafe(value),
            Stmt::Defer { body, .. } | Stmt::Block(body) => body.stmts.iter().any(stmt),
            Stmt::If(x) => {
                expr_needs_unsafe(&x.cond.expr)
                    || x.then.stmts.iter().any(stmt)
                    || x.otherwise.as_ref().map(|e| stmt(e)).unwrap_or(false)
            }
            Stmt::While { cond, body, .. } => {
                expr_needs_unsafe(&cond.expr) || body.stmts.iter().any(stmt)
            }
            Stmt::Loop { body, .. } | Stmt::Scope { body, .. } => body.stmts.iter().any(stmt),
            Stmt::For { body, .. } => body.stmts.iter().any(stmt),
            Stmt::Match(m) => m.arms.iter().any(|a| match &a.body {
                ArmBody::Block(b) => b.stmts.iter().any(stmt),
                ArmBody::Expr(e) => expr_needs_unsafe(e),
            }),
            Stmt::Unsafe { .. } => true,
            _ => false,
        }
    }
    b.stmts.iter().any(stmt)
}

fn receiver_is_mut(f: &Function) -> bool {
    f.decl
        .params
        .first()
        .map(|p| {
            p.is_self()
                && matches!(
                    p.ty,
                    Ty::Borrow { mutable: true, .. } | Ty::Slice { mutable: true, .. }
                )
        })
        .unwrap_or(false)
}

fn types_equal(a: &Type, b: &Type) -> bool {
    if a.is_error() || b.is_error() {
        return true;
    }
    a == b
}

fn needs_context(e: &Expr) -> bool {
    match &e.kind {
        ExprKind::EnumLit { path: None, .. } | ExprKind::StructShorthand { .. } => true,
        ExprKind::Builtin { name, .. } if name == "zero" => true,
        ExprKind::Paren(inner) => needs_context(inner),
        _ => false,
    }
}

fn is_literal(e: &Expr) -> bool {
    matches!(e.kind, ExprKind::Int { .. } | ExprKind::Float(_))
}

fn assign_binop(op: AssignOp) -> Option<BinOp> {
    Some(match op {
        AssignOp::Assign => return None,
        AssignOp::Add => BinOp::Add,
        AssignOp::Sub => BinOp::Sub,
        AssignOp::Mul => BinOp::Mul,
        AssignOp::Div => BinOp::Div,
        AssignOp::Rem => BinOp::Rem,
        AssignOp::AddWrap => BinOp::AddWrap,
        AssignOp::SubWrap => BinOp::SubWrap,
        AssignOp::MulWrap => BinOp::MulWrap,
        AssignOp::AddSat => BinOp::AddSat,
        AssignOp::SubSat => BinOp::SubSat,
        AssignOp::MulSat => BinOp::MulSat,
        AssignOp::BitAnd => BinOp::BitAnd,
        AssignOp::BitOr => BinOp::BitOr,
        AssignOp::BitXor => BinOp::BitXor,
        AssignOp::Shl => BinOp::Shl,
        AssignOp::Shr => BinOp::Shr,
    })
}

fn interface_eq_type(_ty: &Type) -> Option<String> {
    None
}

fn lossless(from: &Type, to: &Type) -> bool {
    if from == to || from.is_error() || to.is_error() || from.is_never() {
        return true;
    }
    match (from, to) {
        (Type::Bool, Type::Int(_)) => true,
        (Type::Int(a), Type::Int(b)) => {
            let amin = int_min(from);
            let amax = int_max(from);
            let bmin = int_min(to);
            let bmax = int_max(to);
            let _ = (a, b);
            amin >= bmin && amax <= bmax
        }
        (Type::Int(a), Type::Float(b)) => {
            let bits = a.int_bits().unwrap_or(64);
            if *b == ScalarTy::F32 {
                bits <= 16
            } else {
                bits <= 32
            }
        }
        (Type::Float(ScalarTy::F32), Type::Float(ScalarTy::F64)) => true,
        (Type::Borrow { mutable: false, inner: a }, Type::Ptr { inner: b, .. })
        | (Type::Borrow { mutable: true, inner: a }, Type::Ptr { inner: b, .. }) => a == b,
        (Type::Enum(n, _), Type::Int(_)) => {
            // A payload-free enum converts to its tag type. The checker knows
            // the enum is payload-free through `payload_free_enum` elsewhere;
            // this conservative arm is refined by the caller.
            let _ = n;
            false
        }
        _ => false,
    }
}

fn type_size(ty: &Type, defs: &TypeDefs) -> Option<i64> {
    match ty {
        Type::Bool | Type::Int(ScalarTy::I8) | Type::Int(ScalarTy::U8) => Some(1),
        Type::Int(ScalarTy::I16) | Type::Int(ScalarTy::U16) => Some(2),
        Type::Int(ScalarTy::I32) | Type::Int(ScalarTy::U32) | Type::Float(ScalarTy::F32) => Some(4),
        Type::Int(ScalarTy::I64) | Type::Int(ScalarTy::U64) | Type::Float(ScalarTy::F64) => Some(8),
        Type::Ptr { .. } | Type::Borrow { .. } | Type::Fn { .. } => Some(8),
        Type::Slice { .. } | Type::Dyn(_) => Some(16),
        Type::Vector(v) => Some((v.bits() / 8) as i64),
        Type::Array(n, e) => type_size(e, defs).map(|s| s * n),
        Type::Tuple(es) => Some(es.iter().filter_map(|t| type_size(t, defs)).sum()),
        Type::Optional(e) | Type::Result(e, _) => {
            type_size(e, defs).map(|s| s + 8)
        }
        Type::Struct(n, _) => defs.structs.get(n).map(|d| {
            d.fields
                .iter()
                .filter_map(|(_, t, _)| type_size(t, defs))
                .sum()
        }),
        Type::Enum(..) => Some(8),
        _ => None,
    }
}

trait SpanText {
    fn text_or<'a>(&self, fallback: &'a str) -> &'a str;
}

impl SpanText for Span {
    fn text_or<'a>(&self, fallback: &'a str) -> &'a str {
        fallback
    }
}
