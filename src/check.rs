//! Phases 4 to 7: imports, name resolution, signatures, and body type checking.
//!
//! The MVP checks one module at a time. `std` names are treated as opaque
//! external types so programs that only pass `Env` around still check; calls on
//! those opaque values are accepted and evaluated only by the interpreter when
//! the interpreter knows them. The module loader and multi-file package graph
//! are the next front-end milestone.

use crate::ast::*;
use crate::diagnostic::{Diagnostic, Edit, sort_diagnostics};
use crate::lexer::lex;
use crate::parser::{check_operator_mixing, parse};
use crate::source::{Source, Sources, Span};
use crate::token::ScalarTy;
use crate::types::*;
use std::collections::{BTreeMap, HashMap, HashSet};

pub const DEFAULT_INT: ScalarTy = ScalarTy::I64;

#[derive(Clone, Debug)]
pub enum ConstVal {
    Int(i128),
    Float(f64),
    Bool(bool),
    Str(Vec<u8>),
}

#[derive(Clone, Debug)]
pub struct ConstInfo {
    pub ty: Type,
    pub value: Option<ConstVal>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct Function {
    pub decl: FnDecl,
    pub file: String,
    pub owner: Option<Type>,
    pub interface: Option<String>,
    pub external: bool,
    pub signature_ret: Type,
    pub signature_error: Option<Type>,
    pub signature_params: Vec<Type>,
}

// The signature types are stored on the Function itself; the AST keeps the
// written types for tools.

#[derive(Clone, Debug, Default)]
pub struct Program {
    pub module: Module,
    pub defs: TypeDefs,
    pub functions: BTreeMap<String, Function>,
    pub methods: BTreeMap<String, BTreeMap<String, Function>>,
    pub consts: BTreeMap<String, ConstInfo>,
    pub statics: BTreeMap<String, StaticDecl>,
    pub tests: Vec<TestDecl>,
    pub imports: Vec<Import>,
}

impl Program {
    /// Function named by a direct call.
    pub fn function(&self, name: &str) -> Option<&Function> {
        self.functions.get(name)
    }

    /// Method or associated function on the given receiver type.
    pub fn method(&self, ty: &Type, name: &str) -> Option<&Function> {
        let key = method_key(ty);
        self.methods.get(&key).and_then(|m| m.get(name))
    }

    pub fn const_value(&self, name: &str) -> Option<&ConstInfo> {
        self.consts.get(name)
    }
}

pub fn method_key(ty: &Type) -> String {
    format!("{}", ty.strip_borrows())
}

#[derive(Clone, Debug, Default)]
pub struct Analysis {
    pub sources: Sources,
    pub module: Option<Module>,
    pub program: Program,
    pub diagnostics: Vec<Diagnostic>,
}

impl Analysis {
    pub fn has_errors(&self) -> bool {
        !self.diagnostics.is_empty()
    }
}

pub fn analyze(source: Source) -> Analysis {
    let mut sources = Sources::default();
    sources.add(source);
    analyze_sources(sources)
}

pub fn analyze_file(path: &std::path::Path) -> std::io::Result<Analysis> {
    let bytes = std::fs::read(path)?;
    let text = match String::from_utf8(bytes) {
        Ok(t) => t,
        Err(e) => {
            let bad = e.utf8_error().valid_up_to();
            let lossy = String::from_utf8_lossy(e.as_bytes()).into_owned();
            let source = Source::with_absolute(
                path.to_string_lossy().replace('\\', "/"),
                path.to_path_buf(),
                lossy,
            );
            let mut sources = Sources::default();
            sources.add(source);
            let mut a = Analysis {
                sources,
                ..Analysis::default()
            };
            a.diagnostics.push(Diagnostic::new(
                "E0101",
                "file is not valid UTF-8",
                &a.sources.files[0],
                Span::new(bad, bad + 1),
            ));
            return Ok(a);
        }
    };
    let source = Source::with_absolute(
        path.to_string_lossy().replace('\\', "/"),
        path.to_path_buf(),
        text,
    );
    Ok(analyze(source))
}

pub fn analyze_sources(sources: Sources) -> Analysis {
    if sources.files.is_empty() {
        return Analysis::default();
    }
    let source = &sources.files[0];
    let (tokens, mut diagnostics) = lex(source);
    let (module, parse_diagnostics) = parse(source, tokens);
    diagnostics.extend(parse_diagnostics);
    diagnostics.extend(check_operator_mixing(&module, source));

    sort_diagnostics(&mut diagnostics);
    let syntax_gate = diagnostics
        .iter()
        .any(|d| d.code.starts_with("E01") || d.code.starts_with("E02"));
    if syntax_gate {
        return Analysis {
            sources,
            module: Some(module.clone()),
            program: Program {
                module: module.clone(),
                ..Program::default()
            },
            diagnostics,
        };
    }

    let (semantic_diagnostics, program) = {
        let mut analyzer = Analyzer {
            source,
            diagnostics: Vec::new(),
            program: Program {
                module: module.clone(),
                ..Program::default()
            },
            generic_bounds: HashMap::new(),
            self_ty: None,
        };
        analyzer.collect_imports(&module);
        analyzer.collect_types(&module);
        analyzer.collect_values(&module);
        analyzer.check_all_bodies();
        (analyzer.diagnostics, analyzer.program)
    };
    diagnostics.extend(semantic_diagnostics);
    sort_diagnostics(&mut diagnostics);
    Analysis {
        sources,
        module: Some(module),
        program,
        diagnostics,
    }
}

struct Analyzer<'a> {
    source: &'a Source,
    diagnostics: Vec<Diagnostic>,
    program: Program,
    generic_bounds: HashMap<String, Vec<String>>,
    self_ty: Option<Type>,
}

impl<'a> Analyzer<'a> {
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

    fn collect_imports(&mut self, module: &Module) {
        self.program.imports = module.imports.clone();
        for import in &module.imports {
            let name = import.bound_name().to_string();
            // An import that names a type introduces that name as an external
            // type. `std.mem` names a module; paths through it are also external.
            if name
                .chars()
                .next()
                .map(|c| c.is_ascii_uppercase())
                .unwrap_or(false)
            {
                self.program.defs.external.insert(name);
            } else {
                for (seg, _) in &import.segments {
                    if seg
                        .chars()
                        .next()
                        .map(|c| c.is_ascii_uppercase())
                        .unwrap_or(false)
                    {
                        self.program.defs.external.insert(seg.clone());
                    }
                }
            }
        }
        // Builtin names that are not imported.
        for name in [
            "Option", "Result", "Self", "Env", "Task", "Scope", "Idx", "Idx2", "Idx3",
        ] {
            self.program.defs.external.insert(name.to_string());
        }
    }

    fn collect_types(&mut self, module: &Module) {
        let mut type_names: HashSet<String> = HashSet::new();
        for item in &module.items {
            if let Some(name) = item.name() {
                let is_type = matches!(
                    item,
                    Item::Struct(_) | Item::Enum(_) | Item::TypeAlias(_) | Item::Interface(_)
                );
                if !is_type {
                    continue;
                }
                if !type_names.insert(name.to_string()) {
                    self.diag(
                        "E0308",
                        format!("type `{name}` is defined more than once"),
                        item.span(),
                    );
                }
            }
        }

        // Struct and enum definitions first: names are enough for fields to
        // refer to types declared later in this module.
        for item in &module.items {
            match item {
                Item::Struct(s) => {
                    let generics: Vec<String> = s.generics.iter().map(|g| g.name.clone()).collect();
                    let old_bounds = self.generic_bounds.clone();
                    self.generic_bounds = s
                        .generics
                        .iter()
                        .map(|g| {
                            (
                                g.name.clone(),
                                g.bounds.iter().map(|b| b.name().to_string()).collect(),
                            )
                        })
                        .collect();
                    let mut fields = Vec::new();
                    let mut field_names = HashSet::new();
                    for f in &s.fields {
                        if !field_names.insert(f.name.clone()) {
                            self.diag(
                                "E0308",
                                format!("field `{}` is declared more than once", f.name),
                                f.name_span,
                            );
                        }
                        let ty = self.resolve_ty(&f.ty);
                        fields.push((f.name.clone(), ty, f.public));
                    }
                    self.generic_bounds = old_bounds;
                    self.program.defs.structs.insert(
                        s.name.clone(),
                        StructDef {
                            kind: s.kind,
                            fields,
                            generics,
                            span: s.name_span,
                            file: self.source.path.clone(),
                        },
                    );
                }
                Item::Enum(e) => {
                    let generics: Vec<String> = e.generics.iter().map(|g| g.name.clone()).collect();
                    let old_bounds = self.generic_bounds.clone();
                    self.generic_bounds = e
                        .generics
                        .iter()
                        .map(|g| {
                            (
                                g.name.clone(),
                                g.bounds.iter().map(|b| b.name().to_string()).collect(),
                            )
                        })
                        .collect();
                    let mut variants = Vec::new();
                    let mut from: HashMap<String, String> = HashMap::new();
                    let mut names = HashSet::new();
                    for v in &e.variants {
                        if !names.insert(v.name.clone()) {
                            self.diag(
                                "E0308",
                                format!("variant `{}` is declared more than once", v.name),
                                v.name_span,
                            );
                        }
                        let payload = v.payload.as_ref().map(|t| self.resolve_ty(t));
                        if v.from {
                            if let Some(p) = &payload {
                                let key = p.to_string();
                                if let Some(prev) = from.get(&key) {
                                    let prev_name = prev.clone();
                                    self.diag(
                                        "E0504",
                                        format!(
                                            "a variant from `{key}` already exists (`{prev_name}`)"
                                        ),
                                        v.name_span,
                                    );
                                } else {
                                    from.insert(key, v.name.clone());
                                }
                            }
                        }
                        variants.push((v.name.clone(), payload));
                    }
                    self.generic_bounds = old_bounds;
                    let tag = e.tag.as_ref().map(|t| self.resolve_ty(t));
                    self.program.defs.enums.insert(
                        e.name.clone(),
                        EnumDef {
                            copy: e.copy,
                            variants,
                            generics,
                            from,
                            tag,
                            span: e.name_span,
                            file: self.source.path.clone(),
                        },
                    );
                }
                Item::TypeAlias(t) => {
                    self.program.defs.aliases.insert(
                        t.name.clone(),
                        (
                            t.ty.clone(),
                            t.generics.iter().map(|g| g.name.clone()).collect(),
                        ),
                    );
                }
                Item::Interface(i) => {
                    // Interfaces are syntactic for the MVP; their names still
                    // resolve as opaque type names in bounds.
                    let _ = i;
                }
                _ => {}
            }
        }

        // Aliases may refer to other aliases; resolve through the written type
        // on demand in `resolve_ty`.

        self.check_declaration_properties(module);
    }

    fn check_declaration_properties(&mut self, module: &Module) {
        for item in &module.items {
            match item {
                Item::Struct(s) => {
                    let fields: Vec<(String, Type)> = self
                        .program
                        .defs
                        .structs
                        .get(&s.name)
                        .map(|d| {
                            d.fields
                                .iter()
                                .map(|(n, t, _)| (n.clone(), t.clone()))
                                .collect()
                        })
                        .unwrap_or_default();
                    if s.kind == StructKind::Copy {
                        for (_, t) in &fields {
                            if !t.is_copy(&self.program.defs, &self.generic_bounds) {
                                self.diag(
                                    "E0501",
                                    format!("field of type `{t}` is not `Copy`"),
                                    s.name_span,
                                );
                            }
                        }
                    }
                    let needs_linear = fields
                        .iter()
                        .any(|(_, t)| t.is_linear(&self.program.defs, &self.generic_bounds));
                    if needs_linear && s.kind != StructKind::Linear {
                        let d = self.diag(
                            "E0502",
                            format!(
                                "struct `{}` contains a linear field and must be declared `linear`",
                                s.name
                            ),
                            s.name_span,
                        );
                        d.fix(
                            "insert `linear ` before `struct`",
                            Edit {
                                span: Span::empty(s.span.start),
                                text: "linear ".to_string(),
                            },
                        );
                    }
                }
                Item::Enum(e) => {
                    if e.copy {
                        for v in &e.variants {
                            if let Some(t) = &v.payload {
                                let ty = self.resolve_ty(t);
                                if !ty.is_copy(&self.program.defs, &self.generic_bounds) {
                                    self.diag(
                                        "E0501",
                                        format!("payload of type `{ty}` is not `Copy`"),
                                        v.name_span,
                                    );
                                }
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        // A value type that contains itself by value is infinite.
        for (name, def) in self.program.defs.structs.clone() {
            for (_, ty, _) in &def.fields {
                if direct_contains(ty, &name, &self.program.defs, &mut HashSet::new()) {
                    self.diag(
                        "E0516",
                        format!("`{name}` contains itself by value"),
                        def.span,
                    );
                }
            }
        }
        for (name, def) in self.program.defs.enums.clone() {
            for (_, payload) in &def.variants {
                if let Some(ty) = payload {
                    if direct_contains(ty, &name, &self.program.defs, &mut HashSet::new()) {
                        self.diag(
                            "E0516",
                            format!("`{name}` contains itself by value"),
                            def.span,
                        );
                    }
                }
            }
        }
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
            Ty::Vector(v, span) => {
                let bits = v.bits();
                let bool_ok =
                    v.elem != ScalarTy::Bool || matches!(v.lanes, 2 | 4 | 8 | 16 | 32 | 64);
                if !matches!(bits, 128 | 256 | 512) || !bool_ok {
                    self.diag(
                        "E1501",
                        format!(
                            "vector type `{}` must be 128, 256 or 512 bits wide",
                            v.name()
                        ),
                        *span,
                    );
                }
                Type::Vector(*v)
            }
            Ty::Path(p) => self.resolve_path_ty(p),
            Ty::Borrow { mutable, inner, .. } => Type::Borrow {
                mutable: *mutable,
                inner: Box::new(self.resolve_ty(inner)),
            },
            Ty::Slice { mutable, elem, .. } => Type::Slice {
                mutable: *mutable,
                elem: Box::new(self.resolve_ty(elem)),
            },
            Ty::Array { len, elem, span } => {
                let n = self.const_int(len).unwrap_or_else(|| {
                    self.diag("E0405", "array length must be a constant integer", *span);
                    0
                });
                if n < 0 {
                    self.diag("E0405", "array length cannot be negative", *span);
                }
                Type::Array(n, Box::new(self.resolve_ty(elem)))
            }
            Ty::Ptr { mutable, inner, .. } => Type::Ptr {
                mutable: *mutable,
                inner: Box::new(self.resolve_ty(inner)),
            },
            Ty::Optional { inner, .. } => Type::Optional(Box::new(self.resolve_ty(inner))),
            Ty::Tuple { elems, .. } => {
                Type::Tuple(elems.iter().map(|t| self.resolve_ty(t)).collect())
            }
            Ty::Fn {
                params, ret, error, ..
            } => Type::Fn {
                params: params.iter().map(|t| self.resolve_ty(t)).collect(),
                ret: Box::new(self.resolve_ty(ret)),
                error: error.as_ref().map(|e| Box::new(self.resolve_ty(e))),
            },
            Ty::Dyn {
                interface, mutable, ..
            } => {
                let _ = mutable;
                Type::Dyn(interface.name().to_string())
            }
            Ty::Poison(_) => Type::Error,
        }
    }

    fn resolve_path_ty(&mut self, p: &PathTy) -> Type {
        let name = p.name().to_string();
        if p.segments.len() == 1 {
            if let Some(bounds) = self.generic_bounds.get(&name) {
                let _ = bounds;
                return Type::Param(name);
            }
            if name == "Self" {
                if let Some(t) = &self.self_ty {
                    return t.clone();
                }
            }
        }
        if let Some(_s) = self.program.defs.structs.get(&name) {
            let args = self.resolve_args(&p.args);
            return Type::Struct(name, args);
        }
        if let Some(_e) = self.program.defs.enums.get(&name) {
            let args = self.resolve_args(&p.args);
            return Type::Enum(name, args);
        }
        if let Some((ty, params)) = self.program.defs.aliases.get(&name).cloned() {
            if params.is_empty() {
                let _old = self.generic_bounds.clone();
                return self.resolve_ty(&ty);
            }
            let old = self.generic_bounds.clone();
            let args = self.resolve_args(&p.args);
            let target = crate::types::substitute(&self.resolve_ty(&ty), &params, &args);
            self.generic_bounds = old;
            return target;
        }
        if p.segments.len() > 1 || self.program.defs.external.contains(&name) {
            let args = self.resolve_args(&p.args);
            let base = if p.segments.len() > 1 {
                p.segments
                    .iter()
                    .map(|(s, _)| s.as_str())
                    .collect::<Vec<_>>()
                    .join(".")
            } else {
                name
            };
            if args.is_empty() {
                return Type::External(base);
            }
            let args_text = args
                .iter()
                .map(|t| t.to_string())
                .collect::<Vec<_>>()
                .join(", ");
            return Type::External(format!("{base}[{args_text}]"));
        }
        if name == "Option" {
            let args = self.resolve_args(&p.args);
            if let Some(t) = args.into_iter().next() {
                return Type::Optional(Box::new(t));
            }
        }
        if name == "Result" {
            let mut args = self.resolve_args(&p.args).into_iter();
            if let (Some(t), Some(e)) = (args.next(), args.next()) {
                return Type::Result(Box::new(t), Box::new(e));
            }
        }
        self.diag("E0305", format!("undefined type `{}`", p.name()), p.span);
        Type::Error
    }

    fn resolve_args(&mut self, args: &[GenericArg]) -> Vec<Type> {
        let mut out = Vec::new();
        for a in args {
            match a {
                GenericArg::Ty(t) => out.push(self.resolve_ty(t)),
                GenericArg::Const(_) => {
                    // Const generic arguments are kept out of the type identity
                    // in the MVP; their value is checked where it matters.
                }
            }
        }
        out
    }

    fn collect_values(&mut self, module: &Module) {
        let mut value_names: HashSet<String> = HashSet::new();
        for item in &module.items {
            match item {
                Item::Fn(f) => {
                    if !value_names.insert(f.name.clone()) {
                        self.diag(
                            "E0308",
                            format!("`{}` is defined more than once", f.name),
                            f.name_span,
                        );
                    }
                    self.register_fn(f, None, None, false);
                }
                Item::Const(c) => {
                    if !value_names.insert(c.name.clone()) {
                        self.diag(
                            "E0308",
                            format!("`{}` is defined more than once", c.name),
                            c.name_span,
                        );
                    }
                    let ty = self.resolve_ty(&c.ty);
                    self.program.consts.insert(
                        c.name.clone(),
                        ConstInfo {
                            ty,
                            value: None,
                            span: c.name_span,
                        },
                    );
                }
                Item::Static(s) => {
                    if !value_names.insert(s.name.clone()) {
                        self.diag(
                            "E0308",
                            format!("`{}` is defined more than once", s.name),
                            s.name_span,
                        );
                    }
                    let _ = self.resolve_ty(&s.ty);
                    self.program.statics.insert(s.name.clone(), s.clone());
                }
                Item::Impl(i) => {
                    self.register_impl(i);
                }
                Item::Extern(e) => {
                    for f in &e.functions {
                        let mut f2 = f.clone();
                        f2.mods.unsafe_ = true;
                        self.register_fn(&f2, None, None, true);
                    }
                }
                Item::Test(t) => self.program.tests.push(t.clone()),
                Item::When(w) => {
                    // `when` items are resolved by phase 4. The MVP accepts the
                    // first branch without evaluating @target; later branches
                    // are deliberately ignored until target selection lands.
                    self.collect_types(&Module {
                        imports: Vec::new(),
                        items: w.then.clone(),
                        span: w.span,
                    });
                    for sub in &w.then {
                        if let Item::Fn(f) = sub {
                            self.register_fn(f, None, None, false);
                        }
                    }
                }
                _ => {}
            }
        }
        self.check_main(module);
        self.check_consts(module);
    }

    fn register_fn(
        &mut self,
        f: &FnDecl,
        owner: Option<Type>,
        interface: Option<String>,
        external: bool,
    ) {
        let old_bounds = self.generic_bounds.clone();
        let old_self = self.self_ty.clone();
        self.generic_bounds = f
            .generics
            .iter()
            .map(|g| {
                (
                    g.name.clone(),
                    g.bounds.iter().map(|b| b.name().to_string()).collect(),
                )
            })
            .collect();
        if let Some(o) = &owner {
            self.self_ty = Some(o.clone());
        }
        let params = f.params.iter().map(|p| self.resolve_ty(&p.ty)).collect();
        let ret = self.resolve_ty(&f.ret);
        let error = f.error.as_ref().map(|e| self.resolve_ty(e));
        // §13.1: a returned view needs a `from` clause.
        if ret.is_view(&self.program.defs) && f.from.is_empty() && !external {
            self.diag(
                "E1201",
                format!(
                    "return type `{ret}` contains a borrow and the signature has no `from` clause"
                ),
                f.ret.span(),
            );
        }
        self.generic_bounds = old_bounds;
        self.self_ty = old_self;

        let function = Function {
            decl: f.clone(),
            file: self.source.path.clone(),
            owner: owner.clone(),
            interface,
            external,
            signature_ret: ret,
            signature_error: error,
            signature_params: params,
        };
        if let Some(o) = owner {
            self.program
                .methods
                .entry(method_key(&o))
                .or_default()
                .insert(f.name.clone(), function.clone());
            // Associated functions are also reachable as `Type.name`.
            self.program
                .functions
                .insert(format!("{}.{}", method_key(&o), f.name), function);
        } else {
            self.program.functions.insert(f.name.clone(), function);
        }
    }

    fn register_impl(&mut self, i: &ImplDecl) {
        let old_bounds = self.generic_bounds.clone();
        self.generic_bounds = i
            .generics
            .iter()
            .map(|g| {
                (
                    g.name.clone(),
                    g.bounds.iter().map(|b| b.name().to_string()).collect(),
                )
            })
            .collect();
        let target = self.resolve_ty(&i.target);
        self.generic_bounds = old_bounds;
        if matches!(target, Type::Param(_)) {
            self.diag(
                "E0508",
                "an interface cannot be implemented for a bare type parameter",
                i.target.span(),
            );
        }
        for member in &i.members {
            if let Item::Fn(f) = member {
                self.register_fn(
                    f,
                    Some(target.clone()),
                    i.interface.as_ref().map(|p| p.name().to_string()),
                    false,
                );
            }
        }
    }

    fn check_main(&mut self, module: &Module) {
        let Some(main) = module.items.iter().find_map(|i| match i {
            Item::Fn(f) if f.name == "main" => Some(f),
            _ => None,
        }) else {
            return;
        };
        let mut ok = main.mods.public;
        if main.params.len() != 1 {
            ok = false;
        } else {
            let p = &main.params[0];
            let ty = self.resolve_ty(&p.ty);
            let ok_param = matches!(
                &ty,
                Type::Borrow { mutable: false, inner }
                    if matches!(inner.as_ref(), Type::External(n) if n == "Env")
            );
            if !ok_param {
                ok = false;
            }
        }
        let ret = self.resolve_ty(&main.ret);
        let ok_ret = ret == Type::Void
            || ret == Type::Int(ScalarTy::U8)
            || matches!(&ret, Type::Result(t, _) if **t == Type::Void || **t == Type::Int(ScalarTy::U8));
        if !ok_ret {
            ok = false;
        }
        if !ok {
            self.diag(
                "E0514",
                "`main` must be `pub`, take one `env: &Env`, and return `void` or `u8`",
                main.name_span,
            );
        }
    }

    fn check_consts(&mut self, module: &Module) {
        for item in &module.items {
            match item {
                Item::Const(c) => {
                    let ty = self.program.consts[&c.name].ty.clone();
                    let val = self.const_value(&c.value, Some(&ty));
                    if let Some(v) = val {
                        if let Some(info) = self.program.consts.get_mut(&c.name) {
                            info.value = Some(v);
                        }
                    }
                }
                Item::Static(s) => {
                    let ty = self.resolve_ty(&s.ty);
                    let _ = self.const_value(&s.value, Some(&ty));
                }
                _ => {}
            }
        }
    }

    // ------------------------------------------------------------- constants

    fn const_int(&mut self, e: &Expr) -> Option<i64> {
        match self.const_value(e, Some(&Type::i64())) {
            Some(ConstVal::Int(n)) => Some(n as i64),
            Some(ConstVal::Bool(b)) => Some(b as i64),
            _ => None,
        }
    }

    fn const_value(&mut self, e: &Expr, expected: Option<&Type>) -> Option<ConstVal> {
        match &e.kind {
            ExprKind::Int {
                value, negative, ..
            } => {
                let mut n = *value as i128;
                if *negative {
                    n = -n;
                }
                if let Some(ty) = expected {
                    if let Some(v) = fit_int(n, ty) {
                        return Some(ConstVal::Int(v));
                    }
                }
                Some(ConstVal::Int(n))
            }
            ExprKind::Float(v) => Some(ConstVal::Float(*v)),
            ExprKind::Bool(b) => Some(ConstVal::Bool(*b)),
            ExprKind::Str(b) => Some(ConstVal::Str(b.clone())),
            ExprKind::Paren(inner) => self.const_value(inner, expected),
            ExprKind::Ident(name) => {
                if let Some(info) = self.program.consts.get(name) {
                    info.value.clone()
                } else {
                    None
                }
            }
            ExprKind::Unary { op, expr, op_span } => {
                let v = self.const_value(expr, expected)?;
                match (op, v) {
                    (UnOp::Neg, ConstVal::Int(n)) => Some(ConstVal::Int(-n)),
                    (UnOp::Neg, ConstVal::Float(f)) => Some(ConstVal::Float(-f)),
                    (UnOp::NegWrap, ConstVal::Int(n)) => {
                        let ty = expected.cloned().unwrap_or(Type::i64());
                        Some(ConstVal::Int(wrap_int(-n, &ty)))
                    }
                    (UnOp::Not, ConstVal::Bool(b)) => Some(ConstVal::Bool(!b)),
                    (UnOp::BitNot, ConstVal::Int(n)) => Some(ConstVal::Int(!n)),
                    _ => {
                        let _ = op_span;
                        None
                    }
                }
            }
            ExprKind::Binary {
                op,
                lhs,
                rhs,
                op_span,
            } => {
                let ty = expected.cloned().unwrap_or(Type::i64());
                let a = self.const_value(lhs, Some(&ty))?;
                let b = self.const_value(rhs, Some(&ty))?;
                let mut trap = |code: &str, text: &str| {
                    self.diag(
                        "E0806",
                        format!("constant evaluation trapped with {code}: {text}"),
                        *op_span,
                    );
                };
                match (a, b) {
                    (ConstVal::Int(a), ConstVal::Int(b)) => {
                        let n = match op {
                            BinOp::Add | BinOp::AddWrap | BinOp::AddSat => a
                                .checked_add(b)
                                .map(|v| if *op == BinOp::AddSat { v } else { v })
                                .unwrap_or(a.wrapping_add(b)),
                            BinOp::Sub | BinOp::SubWrap | BinOp::SubSat => a.wrapping_sub(b),
                            BinOp::Mul | BinOp::MulWrap | BinOp::MulSat => a.wrapping_mul(b),
                            BinOp::Div => {
                                if b == 0 {
                                    trap("T04", "division by zero");
                                    return None;
                                }
                                let min = int_min(&ty);
                                let _max = int_max(&ty);
                                if a == min && b == -1 {
                                    trap("T05", "integer overflow in division");
                                    return None;
                                }
                                a / b
                            }
                            BinOp::Rem => {
                                if b == 0 {
                                    trap("T04", "division by zero");
                                    return None;
                                }
                                a % b
                            }
                            BinOp::Shl => {
                                let bits = ty.int_bits().unwrap_or(64);
                                if b < 0 || b >= bits as i128 {
                                    trap(
                                        "T06",
                                        &format!(
                                            "shift count {b} out of range for {bits}-bit value"
                                        ),
                                    );
                                    return None;
                                }
                                a << b
                            }
                            BinOp::Shr => {
                                let bits = ty.int_bits().unwrap_or(64);
                                if b < 0 || b >= bits as i128 {
                                    trap(
                                        "T06",
                                        &format!(
                                            "shift count {b} out of range for {bits}-bit value"
                                        ),
                                    );
                                    return None;
                                }
                                a >> b
                            }
                            BinOp::BitAnd => a & b,
                            BinOp::BitOr => a | b,
                            BinOp::BitXor => a ^ b,
                            _ => return None,
                        };
                        let in_range = n >= int_min(&ty) && n <= int_max(&ty);
                        if !in_range && matches!(op, BinOp::Add | BinOp::Sub | BinOp::Mul) {
                            trap("T03", "integer overflow");
                            return None;
                        }
                        let wrapped = match op {
                            BinOp::AddWrap | BinOp::SubWrap | BinOp::MulWrap => {
                                apply_int_width(n, &ty)
                            }
                            BinOp::AddSat | BinOp::SubSat | BinOp::MulSat => {
                                n.clamp(int_min(&ty), int_max(&ty))
                            }
                            _ => n,
                        };
                        Some(ConstVal::Int(wrapped))
                    }
                    (ConstVal::Float(a), ConstVal::Float(b)) => match op {
                        BinOp::Add => Some(ConstVal::Float(a + b)),
                        BinOp::Sub => Some(ConstVal::Float(a - b)),
                        BinOp::Mul => Some(ConstVal::Float(a * b)),
                        BinOp::Div => Some(ConstVal::Float(a / b)),
                        _ => None,
                    },
                    _ => None,
                }
            }
            ExprKind::Builtin { name, args, .. } => match name.as_str() {
                "zero" => Some(ConstVal::Int(0)),
                "assert" => {
                    if let Some(Expr {
                        kind: ExprKind::Bool(false),
                        span,
                    }) = args.first()
                    {
                        self.diag(
                            "E0806",
                            "assertion failed during constant evaluation",
                            *span,
                        );
                    }
                    None
                }
                "static_assert" => {
                    if let Some(ConstVal::Bool(false)) = args
                        .first()
                        .and_then(|a| self.const_value(a, Some(&Type::Bool)))
                    {
                        let msg = args.get(1).and_then(|a| match &a.kind {
                            ExprKind::Str(b) => Some(String::from_utf8_lossy(b).into_owned()),
                            _ => None,
                        });
                        self.diag(
                            "E0810",
                            msg.unwrap_or_else(|| "static assertion failed".to_string()),
                            e.span,
                        );
                    }
                    None
                }
                _ => None,
            },
            _ => None,
        }
    }

    // --------------------------------------------------------------- bodies

    fn check_all_bodies(&mut self) {
        let functions: Vec<(String, Function)> = self
            .program
            .functions
            .iter()
            .filter(|(name, f)| !name.contains('.') && !f.external && f.decl.body.is_some())
            .map(|(n, f)| (n.clone(), f.clone()))
            .collect();
        for (_, f) in functions {
            self.check_function(&f);
        }
        let methods: Vec<Function> = self
            .program
            .methods
            .values()
            .flat_map(|m| m.values().cloned())
            .filter(|f| !f.external && f.decl.body.is_some())
            .collect();
        let mut seen = HashSet::new();
        for f in methods {
            let key = format!("{}::{}", method_key(f.owner.as_ref().unwrap()), f.decl.name);
            if seen.insert(key) {
                self.check_function(&f);
            }
        }
        let tests = self.program.tests.clone();
        for t in tests {
            self.check_test(&t);
        }
    }

    fn check_function(&mut self, f: &Function) {
        self.check_body_function(
            &f.decl,
            f.signature_ret.clone(),
            f.signature_error.clone(),
            f.decl
                .generics
                .iter()
                .map(|g| {
                    (
                        g.name.clone(),
                        g.bounds.iter().map(|b| b.name().to_string()).collect(),
                    )
                })
                .collect(),
            f.owner.clone(),
            false,
        );
    }

    fn check_test(&mut self, t: &TestDecl) {
        let mut checker = Checker::new(
            self.source,
            &self.program,
            &mut self.diagnostics,
            Type::Void,
            None,
            HashMap::new(),
            None,
            true,
        );
        checker.push_scope();
        let ret = checker.check_block(&t.body);
        if ret.is_never() {
            // fine
        }
        checker.pop_scope(true);
        checker.finish();
    }

    fn check_body_function(
        &mut self,
        f: &FnDecl,
        ret: Type,
        error: Option<Type>,
        bounds: HashMap<String, Vec<String>>,
        self_ty: Option<Type>,
        is_test: bool,
    ) {
        let mut checker = Checker::new(
            self.source,
            &self.program,
            &mut self.diagnostics,
            ret.clone(),
            error,
            bounds,
            self_ty.clone(),
            is_test,
        );
        checker.param_into = f
            .params
            .iter()
            .map(|p| (p.name.clone(), p.into.as_ref().map(|(t, _)| t.clone())))
            .collect();
        checker.from = f.from.clone();
        checker.kernel_index = if f.mods.kernel {
            f.params.first().map(|p| p.name.clone())
        } else {
            None
        };
        checker.push_scope();
        for (i, p) in f.params.iter().enumerate() {
            let ty = checker
                .program
                .methods
                .get(&self_ty.as_ref().map(method_key).unwrap_or_default())
                .and_then(|m| m.get(&f.name))
                .map(|func| func.signature_params.get(i).cloned())
                .flatten()
                .unwrap_or_else(|| checker.resolve_ty(&p.ty));
            checker.declare_param(&p.name, ty, p.name_span);
        }
        let body_ty = checker.check_block(&f.body.clone().unwrap_or(Block {
            stmts: Vec::new(),
            span: f.span,
        }));
        let diverges = body_ty.is_never();
        if !ret.is_void() && !diverges && !block_diverges(f.body.as_ref().unwrap()) {
            let span = Span::empty(f.body.as_ref().unwrap().span.end.saturating_sub(1));
            checker.diag(
                "E0412",
                "not every path through this function returns a value",
                span,
            );
        }
        checker.pop_scope(true);
        checker.finish();
        checker.check_ownership(f);
        let facts: Vec<LocalFact> = checker.facts.values().cloned().collect();
        let borrow_diags =
            crate::borrow::check_function(checker.program, checker.source, f, &facts);
        checker.diagnostics.extend(borrow_diags);
    }
}

fn direct_contains(ty: &Type, name: &str, defs: &TypeDefs, seen: &mut HashSet<String>) -> bool {
    match ty {
        Type::Struct(n, _) if n == name => true,
        Type::Struct(n, _) => {
            if !seen.insert(n.clone()) {
                return false;
            }
            defs.structs
                .get(n)
                .map(|d| {
                    d.fields
                        .iter()
                        .any(|(_, t, _)| direct_contains(t, name, defs, seen))
                })
                .unwrap_or(false)
        }
        Type::Enum(n, _) if n == name => true,
        Type::Enum(n, _) => {
            if !seen.insert(n.clone()) {
                return false;
            }
            defs.enums
                .get(n)
                .map(|d| {
                    d.variants.iter().any(|(_, p)| {
                        p.as_ref()
                            .map(|t| direct_contains(t, name, defs, seen))
                            .unwrap_or(false)
                    })
                })
                .unwrap_or(false)
        }
        Type::Array(_, e) | Type::Optional(e) => direct_contains(e, name, defs, seen),
        Type::Tuple(es) => es.iter().any(|t| direct_contains(t, name, defs, seen)),
        Type::Result(a, b) => {
            direct_contains(a, name, defs, seen) || direct_contains(b, name, defs, seen)
        }
        _ => false,
    }
}

fn int_min(ty: &Type) -> i128 {
    match ty {
        Type::Int(s) if s.is_signed() => {
            let bits = s.int_bits().unwrap_or(64);
            -(1i128 << (bits - 1))
        }
        _ => 0,
    }
}

fn int_max(ty: &Type) -> i128 {
    match ty {
        Type::Int(s) if s.is_signed() => {
            let bits = s.int_bits().unwrap_or(64);
            (1i128 << (bits - 1)) - 1
        }
        Type::Int(s) => {
            let bits = s.int_bits().unwrap_or(64);
            (1i128 << bits) - 1
        }
        _ => i128::MAX,
    }
}

fn apply_int_width(n: i128, ty: &Type) -> i128 {
    if let Type::Int(s) = ty {
        let bits = s.int_bits().unwrap_or(64);
        let mask = (1i128 << bits) - 1;
        let mut v = n & mask;
        if s.is_signed() && v >= (1i128 << (bits - 1)) {
            v -= 1i128 << bits;
        }
        v
    } else {
        n
    }
}

fn wrap_int(n: i128, ty: &Type) -> i128 {
    apply_int_width(n, ty)
}

fn fit_int(n: i128, ty: &Type) -> Option<i128> {
    match ty {
        Type::Int(_) => {
            let min = int_min(ty);
            let max = int_max(ty);
            if n >= min && n <= max {
                Some(apply_int_width(n, ty))
            } else {
                None
            }
        }
        Type::Float(_) => Some(n),
        _ => None,
    }
}

pub(crate) fn block_diverges(block: &Block) -> bool {
    block.stmts.iter().any(stmt_diverges)
}

fn stmt_diverges(stmt: &Stmt) -> bool {
    match stmt {
        Stmt::Return { .. } | Stmt::Fail { .. } => true,
        Stmt::If(x) => {
            stmt_diverges(&Stmt::Block(x.then.clone()))
                && x.otherwise
                    .as_ref()
                    .map(|e| stmt_diverges(e))
                    .unwrap_or(false)
        }
        Stmt::Match(m) => {
            !m.arms.is_empty()
                && m.arms.iter().all(|a| match &a.body {
                    ArmBody::Block(b) => block_diverges(b),
                    ArmBody::Expr(e) => matches!(e.kind, ExprKind::Builtin { .. }),
                })
        }
        Stmt::Loop { .. } => true,
        Stmt::Block(b) => block_diverges(b),
        Stmt::Scope { body, .. } => block_diverges(body),
        Stmt::Expr { expr, .. } => expr_is_never(expr),
        _ => false,
    }
}

fn expr_is_never(e: &Expr) -> bool {
    matches!(
        &e.kind,
        ExprKind::Builtin { name, .. } if matches!(name.as_str(), "panic" | "unreachable")
    )
}

// The body checker lives in `check_body.rs` to keep this file focused on
// declarations. It is included here so the public API stays one module.
include!("check_body.rs");
