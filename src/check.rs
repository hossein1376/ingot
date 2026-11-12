//! Phases 4 to 7: imports, name resolution, signatures, and body type checking.
//!
//! A package is loaded as a tree of modules. Names are resolved per module:
//! local items, then imports. `std` names are opaque external types for now;
//! calls through them are permissive and the evaluator understands the few used
//! by the conformance programs.

use crate::ast::*;
use crate::diagnostic::{Diagnostic, Edit, sort_diagnostics};
use crate::lexer::lex;
use crate::loader::{LoadedModule, LoadedPackage};
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
    pub module: String,
    pub owner: Option<Type>,
    pub interface: Option<String>,
    pub external: bool,
    pub signature_ret: Type,
    pub signature_error: Option<Type>,
    pub signature_params: Vec<Type>,
}

// The signature types are stored on the Function itself; the AST keeps the
// written types for tools.

#[derive(Clone, Debug)]
pub enum ImportBinding {
    Type(String),
    Value(String),
    Module(String),
    External(String),
}

#[derive(Clone, Debug, Default)]
pub struct ModuleContext {
    pub name: String,
    pub file: String,
    /// Names bound by imports, in this module.
    pub imports: HashMap<String, ImportBinding>,
    /// Local type names to canonical names.
    pub types: BTreeMap<String, String>,
    /// Local value names to canonical names.
    pub values: BTreeMap<String, String>,
}

#[derive(Clone, Debug)]
pub struct TestInfo {
    pub decl: TestDecl,
    pub module: String,
    pub file: String,
}

#[derive(Clone, Debug, Default)]
pub struct Program {
    pub package_name: String,
    pub root_module: String,
    pub module: Module,
    pub modules: BTreeMap<String, ModuleContext>,
    pub defs: TypeDefs,
    pub functions: BTreeMap<String, Function>,
    pub methods: BTreeMap<String, BTreeMap<String, Function>>,
    pub consts: BTreeMap<String, ConstInfo>,
    pub statics: BTreeMap<String, StaticDecl>,
    pub tests: Vec<TestInfo>,
    pub imports: Vec<Import>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResolvedName {
    Local(String),
    External(String),
}

impl Program {
    pub fn qualify(&self, module: &str, name: &str) -> String {
        if module == self.root_module {
            name.to_string()
        } else {
            format!("{module}.{name}")
        }
    }

    pub fn module_ctx(&self, module: &str) -> Option<&ModuleContext> {
        self.modules.get(module)
    }

    /// Resolve an unqualified type name in a module.
    pub fn resolve_type_name(&self, module: &str, name: &str) -> Option<ResolvedName> {
        let ctx = self.modules.get(module)?;
        if let Some(c) = ctx.types.get(name) {
            return Some(ResolvedName::Local(c.clone()));
        }
        match ctx.imports.get(name) {
            Some(ImportBinding::Type(c)) => Some(ResolvedName::Local(c.clone())),
            Some(ImportBinding::External(prefix)) => Some(ResolvedName::External(prefix.clone())),
            Some(ImportBinding::Module(target)) => self
                .modules
                .get(target)
                .and_then(|c| c.types.get(name))
                .map(|c| ResolvedName::Local(c.clone())),
            _ => None,
        }
    }

    /// Resolve an unqualified value name in a module.
    pub fn resolve_value_name(&self, module: &str, name: &str) -> Option<ResolvedName> {
        let ctx = self.modules.get(module)?;
        if let Some(c) = ctx.values.get(name) {
            return Some(ResolvedName::Local(c.clone()));
        }
        match ctx.imports.get(name) {
            Some(ImportBinding::Value(c)) => Some(ResolvedName::Local(c.clone())),
            Some(ImportBinding::External(prefix)) => Some(ResolvedName::External(prefix.clone())),
            Some(ImportBinding::Module(target)) => self
                .modules
                .get(target)
                .and_then(|c| c.values.get(name))
                .map(|c| ResolvedName::Local(c.clone())),
            _ => None,
        }
    }

    /// Resolve `module_alias.item` in a module.
    pub fn resolve_qualified_name(
        &self,
        module: &str,
        alias: &str,
        item: &str,
    ) -> Option<ResolvedName> {
        let ctx = self.modules.get(module)?;
        match ctx.imports.get(alias)? {
            ImportBinding::Module(target) => {
                let target_ctx = self.modules.get(target)?;
                target_ctx
                    .types
                    .get(item)
                    .or_else(|| target_ctx.values.get(item))
                    .map(|c| ResolvedName::Local(c.clone()))
            }
            ImportBinding::External(prefix) => {
                Some(ResolvedName::External(format!("{prefix}.{item}")))
            }
            _ => None,
        }
    }

    /// Resolve a written type path (`lib.Ticket`, `mem.Arena`) to a local
    /// canonical name or an external spelling.
    pub fn resolve_type_path(&self, module: &str, path: &PathTy) -> Option<ResolvedName> {
        let mut segments: Vec<String> = path.segments.iter().map(|(s, _)| s.clone()).collect();
        if segments
            .first()
            .map(|s| s == &self.package_name)
            .unwrap_or(false)
        {
            segments.remove(0);
        }
        if segments.is_empty() {
            return None;
        }
        if segments.len() == 1 {
            return self.resolve_type_name(module, &segments[0]);
        }
        if matches!(segments[0].as_str(), "std" | "arch") {
            return Some(ResolvedName::External(segments.join(".")));
        }
        let ctx = self.modules.get(module)?;
        match ctx.imports.get(&segments[0]) {
            Some(ImportBinding::External(prefix)) => Some(ResolvedName::External(format!(
                "{}.{}",
                prefix,
                segments[1..].join(".")
            ))),
            Some(ImportBinding::Module(target)) => {
                let item = segments.last()?;
                let target_ctx = self.modules.get(target)?;
                target_ctx
                    .types
                    .get(item)
                    .map(|c| ResolvedName::Local(c.clone()))
            }
            Some(ImportBinding::Type(c)) if segments.len() == 1 => {
                Some(ResolvedName::Local(c.clone()))
            }
            _ => None,
        }
    }

    /// Function named by a direct call in a module.
    pub fn function_in(&self, module: &str, name: &str) -> Option<&Function> {
        match self.resolve_value_name(module, name)? {
            ResolvedName::Local(c) => self.functions.get(&c),
            ResolvedName::External(_) => None,
        }
    }

    /// Function named by `alias.name` in a module.
    pub fn function_qualified(&self, module: &str, alias: &str, name: &str) -> Option<&Function> {
        match self.resolve_qualified_name(module, alias, name)? {
            ResolvedName::Local(c) => self.functions.get(&c),
            ResolvedName::External(_) => None,
        }
    }

    /// Function or method named with a canonical key (used by tools).
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

    pub fn const_value_in(&self, module: &str, name: &str) -> Option<&ConstInfo> {
        match self.resolve_value_name(module, name)? {
            ResolvedName::Local(c) => self.consts.get(&c),
            ResolvedName::External(_) => None,
        }
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
    let package = crate::loader::load(path)?;
    Ok(analyze_loaded(package))
}

pub fn analyze_sources(sources: Sources) -> Analysis {
    let package_name = "t".to_string();
    let root = std::env::current_dir().unwrap_or_default();
    let modules = if sources.files.is_empty() {
        Vec::new()
    } else {
        vec![LoadedModule {
            name: "main".to_string(),
            source_id: 0,
            root: true,
        }]
    };
    analyze_loaded(LoadedPackage {
        package_name,
        root,
        manifest: None,
        sources,
        modules,
        diagnostics: Vec::new(),
    })
}

pub fn analyze_loaded(package: LoadedPackage) -> Analysis {
    let mut diagnostics = package.diagnostics.clone();
    let mut modules_ast: BTreeMap<String, (LoadedModule, Module)> = BTreeMap::new();
    for loaded in &package.modules {
        let source = &package.sources.files[loaded.source_id];
        let (tokens, mut lex_diags) = lex(source);
        let (module, parse_diags) = parse(source, tokens);
        lex_diags.extend(parse_diags);
        lex_diags.extend(check_operator_mixing(&module, source));
        diagnostics.extend(lex_diags);
        modules_ast.insert(loaded.name.clone(), (loaded.clone(), module));
    }
    sort_diagnostics(&mut diagnostics);
    let syntax_gate = diagnostics
        .iter()
        .any(|d| d.code.starts_with("E01") || d.code.starts_with("E02"));
    let root_module = package.root_module().to_string();
    let root_ast = modules_ast
        .get(&root_module)
        .map(|(_, m)| m.clone())
        .or_else(|| modules_ast.values().next().map(|(_, m)| m.clone()))
        .unwrap_or_default();
    if syntax_gate {
        return Analysis {
            sources: package.sources,
            module: Some(root_ast.clone()),
            program: Program {
                package_name: package.package_name,
                root_module,
                module: root_ast,
                ..Program::default()
            },
            diagnostics,
        };
    }

    let (semantic_diagnostics, program) = {
        let mut analyzer = Analyzer {
            sources: &package.sources,
            diagnostics: Vec::new(),
            program: Program {
                package_name: package.package_name.clone(),
                root_module: root_module.clone(),
                module: root_ast.clone(),
                ..Program::default()
            },
            current_module: root_module.clone(),
            current_source: modules_ast
                .get(&root_module)
                .map(|(m, _)| m.source_id)
                .or_else(|| modules_ast.values().next().map(|(m, _)| m.source_id))
                .unwrap_or(0),
            generic_bounds: HashMap::new(),
            self_ty: None,
        };
        analyzer.prepare_modules(&modules_ast);
        analyzer.collect_all(&modules_ast);
        analyzer.check_all_bodies();
        (analyzer.diagnostics, analyzer.program)
    };
    diagnostics.extend(semantic_diagnostics);
    sort_diagnostics(&mut diagnostics);
    Analysis {
        sources: package.sources,
        module: Some(root_ast),
        program,
        diagnostics,
    }
}

struct Analyzer<'a> {
    sources: &'a Sources,
    diagnostics: Vec<Diagnostic>,
    program: Program,
    current_module: String,
    current_source: usize,
    generic_bounds: HashMap<String, Vec<String>>,
    self_ty: Option<Type>,
}

impl<'a> Analyzer<'a> {
    fn source(&self) -> &'a Source {
        &self.sources.files[self.current_source]
    }

    fn diag(
        &mut self,
        code: &'static str,
        message: impl Into<String>,
        span: Span,
    ) -> &mut Diagnostic {
        let source = &self.sources.files[self.current_source];
        self.diagnostics
            .push(Diagnostic::new(code, message, source, span));
        self.diagnostics.last_mut().unwrap()
    }

    fn set_module(&mut self, loaded: &LoadedModule) {
        self.current_module = loaded.name.clone();
        self.current_source = loaded.source_id;
    }

    fn prepare_modules(&mut self, modules: &BTreeMap<String, (LoadedModule, Module)>) {
        for (name, (loaded, module)) in modules {
            let mut ctx = ModuleContext {
                name: name.clone(),
                file: self.sources.files[loaded.source_id].path.clone(),
                ..ModuleContext::default()
            };
            for item in &module.items {
                match item {
                    Item::Struct(s) => {
                        ctx.types
                            .insert(s.name.clone(), self.program.qualify(name, &s.name));
                    }
                    Item::Enum(e) => {
                        ctx.types
                            .insert(e.name.clone(), self.program.qualify(name, &e.name));
                    }
                    Item::TypeAlias(t) => {
                        ctx.types
                            .insert(t.name.clone(), self.program.qualify(name, &t.name));
                    }
                    Item::Interface(i) => {
                        ctx.types
                            .insert(i.name.clone(), self.program.qualify(name, &i.name));
                    }
                    Item::Fn(f) => {
                        ctx.values
                            .insert(f.name.clone(), self.program.qualify(name, &f.name));
                    }
                    Item::Const(c) => {
                        ctx.values
                            .insert(c.name.clone(), self.program.qualify(name, &c.name));
                    }
                    Item::Static(s) => {
                        ctx.values
                            .insert(s.name.clone(), self.program.qualify(name, &s.name));
                    }
                    _ => {}
                }
            }
            self.program.modules.insert(name.clone(), ctx);
        }
        // External names are always available as types when no local or import
        // binds them.
        for name in [
            "Option", "Result", "Self", "Env", "Task", "Scope", "Idx", "Idx2", "Idx3",
        ] {
            self.program.defs.external.insert(name.to_string());
        }
        for (name, (loaded, module)) in modules {
            self.set_module(loaded);
            self.resolve_module_imports(name, module);
        }
    }

    fn resolve_module_imports(&mut self, module_name: &str, module: &Module) {
        if module_name == self.program.root_module {
            self.program.imports = module.imports.clone();
        }
        for import in &module.imports {
            let binding = self.resolve_import(import);
            let Some(binding) = binding else {
                continue;
            };
            let bound = import.bound_name().to_string();
            let mut duplicate = false;
            {
                let ctx = self.program.modules.get_mut(module_name).unwrap();
                match ctx.imports.entry(bound.clone()) {
                    std::collections::hash_map::Entry::Vacant(slot) => {
                        slot.insert(binding);
                    }
                    std::collections::hash_map::Entry::Occupied(_) => duplicate = true,
                }
            }
            if duplicate {
                self.diag(
                    "E0302",
                    format!("`{bound}` is imported more than once"),
                    import
                        .alias_span
                        .unwrap_or_else(|| import.segments.last().unwrap().1),
                );
            }
        }
    }

    fn resolve_import(&mut self, import: &Import) -> Option<ImportBinding> {
        let mut segments: Vec<(String, Span)> = import.segments.clone();
        if segments
            .first()
            .map(|(s, _)| s == &self.program.package_name)
            .unwrap_or(false)
        {
            segments.remove(0);
        }
        if segments.is_empty() {
            return None;
        }
        let first = segments[0].clone();
        if matches!(first.0.as_str(), "std" | "arch") {
            let bound = import.bound_name().to_string();
            self.program.defs.external.insert(bound.clone());
            return Some(ImportBinding::External(bound));
        }
        let full = segments
            .iter()
            .map(|(s, _)| s.as_str())
            .collect::<Vec<_>>()
            .join(".");
        if self.program.modules.contains_key(&full) && !segments.is_empty() {
            // A path that names both a module and an item is ambiguous (§E0316).
            let bound = import.bound_name().to_string();
            let _ = bound;
            return Some(ImportBinding::Module(full));
        }
        for i in (1..segments.len()).rev() {
            let prefix = segments[..i]
                .iter()
                .map(|(s, _)| s.as_str())
                .collect::<Vec<_>>()
                .join(".");
            if let Some(ctx) = self.program.modules.get(&prefix) {
                let (item, item_span) = segments.last().unwrap();
                let ty = ctx.types.get(item).cloned();
                let value = ctx.values.get(item).cloned();
                match (ty, value) {
                    (Some(c), None) => return Some(ImportBinding::Type(c)),
                    (None, Some(c)) => return Some(ImportBinding::Value(c)),
                    (Some(_), Some(_)) => {
                        self.diag(
                            "E0316",
                            format!("`{item}` names both a type and a value"),
                            *item_span,
                        );
                        return None;
                    }
                    (None, None) => {
                        self.diag(
                            "E0301",
                            format!("`{prefix}` has no item `{item}`"),
                            *item_span,
                        );
                        return None;
                    }
                }
            }
        }
        self.diag("E0301", format!("no module named `{}`", first.0), first.1);
        None
    }

    fn collect_all(&mut self, modules: &BTreeMap<String, (LoadedModule, Module)>) {
        for (loaded, module) in modules.values() {
            self.set_module(loaded);
            self.collect_types(module);
            self.collect_values(module);
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
                    let canonical = self.program.qualify(&self.current_module, &s.name);
                    self.program.defs.structs.insert(
                        canonical,
                        StructDef {
                            kind: s.kind,
                            fields,
                            generics,
                            span: s.name_span,
                            file: self.source().path.clone(),
                            module: self.current_module.clone(),
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
                        if v.from
                            && let Some(p) = &payload
                        {
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
                        variants.push((v.name.clone(), payload));
                    }
                    self.generic_bounds = old_bounds;
                    let tag = e.tag.as_ref().map(|t| self.resolve_ty(t));
                    let canonical = self.program.qualify(&self.current_module, &e.name);
                    self.program.defs.enums.insert(
                        canonical,
                        EnumDef {
                            copy: e.copy,
                            variants,
                            generics,
                            from,
                            tag,
                            span: e.name_span,
                            file: self.source().path.clone(),
                            module: self.current_module.clone(),
                        },
                    );
                }
                Item::TypeAlias(t) => {
                    let canonical = self.program.qualify(&self.current_module, &t.name);
                    self.program.defs.aliases.insert(
                        canonical,
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
                    let canonical = self.program.qualify(&self.current_module, &s.name);
                    let fields: Vec<(String, Type)> = self
                        .program
                        .defs
                        .structs
                        .get(&canonical)
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
                    let needs_linear = fields.iter().any(|(_, t)| t.is_linear(&self.program.defs));
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
                Item::Enum(e) if e.copy => {
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
                _ => {}
            }
        }
        // A value type that contains itself by value is infinite.
        for (name, def) in self.program.defs.structs.clone() {
            if def.module != self.current_module {
                continue;
            }
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
            if def.module != self.current_module {
                continue;
            }
            for (_, payload) in &def.variants {
                if let Some(ty) = payload
                    && direct_contains(ty, &name, &self.program.defs, &mut HashSet::new())
                {
                    self.diag(
                        "E0516",
                        format!("`{name}` contains itself by value"),
                        def.span,
                    );
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
            if name == "Self"
                && let Some(t) = &self.self_ty
            {
                return t.clone();
            }
        }
        if name == "Option" && p.segments.len() == 1 {
            let args = self.resolve_args(&p.args);
            if let Some(t) = args.into_iter().next() {
                return Type::Optional(Box::new(t));
            }
        }
        if name == "Result" && p.segments.len() == 1 {
            let mut args = self.resolve_args(&p.args).into_iter();
            if let (Some(t), Some(e)) = (args.next(), args.next()) {
                return Type::Result(Box::new(t), Box::new(e));
            }
        }
        let resolved = self
            .program
            .resolve_type_path(&self.current_module, p)
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
                if let Some((ty, params)) = self.program.defs.aliases.get(&canonical).cloned() {
                    let old = self.generic_bounds.clone();
                    if params.is_empty() {
                        let t = self.resolve_ty(&ty);
                        self.generic_bounds = old;
                        return t;
                    }
                    let args = self.resolve_args(&p.args);
                    let t = crate::types::substitute(&self.resolve_ty(&ty), &params, &args);
                    self.generic_bounds = old;
                    return t;
                }
                // An interface name is an opaque type in this MVP.
                return Type::External(canonical);
            }
            Some(ResolvedName::External(base)) => {
                let base = base.strip_prefix("External:").unwrap_or(&base).to_string();
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
                let args_text = args
                    .iter()
                    .map(|t| t.to_string())
                    .collect::<Vec<_>>()
                    .join(", ");
                return Type::External(format!("{base}[{args_text}]"));
            }
            None => {}
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
                    let canonical = self.program.qualify(&self.current_module, &c.name);
                    self.program.consts.insert(
                        canonical,
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
                    let canonical = self.program.qualify(&self.current_module, &s.name);
                    self.program.statics.insert(canonical, s.clone());
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
                Item::Test(t) => self.program.tests.push(TestInfo {
                    decl: t.clone(),
                    module: self.current_module.clone(),
                    file: self.source().path.clone(),
                }),
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
        if self.current_module == self.program.root_module {
            self.check_main(module);
        }
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
            file: self.source().path.clone(),
            module: self.current_module.clone(),
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
            let canonical = self.program.qualify(&self.current_module, &f.name);
            self.program.functions.insert(canonical, function);
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
                    let canonical = self.program.qualify(&self.current_module, &c.name);
                    let ty = self.program.consts[&canonical].ty.clone();
                    let val = self.const_value(&c.value, Some(&ty));
                    if let Some(v) = val
                        && let Some(info) = self.program.consts.get_mut(&canonical)
                    {
                        info.value = Some(v);
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
                if let Some(ty) = expected
                    && let Some(v) = fit_int(n, ty)
                {
                    return Some(ConstVal::Int(v));
                }
                Some(ConstVal::Int(n))
            }
            ExprKind::Float(v) => Some(ConstVal::Float(*v)),
            ExprKind::Bool(b) => Some(ConstVal::Bool(*b)),
            ExprKind::Str(b) => Some(ConstVal::Str(b.clone())),
            ExprKind::Paren(inner) => self.const_value(inner, expected),
            ExprKind::Ident(name) => {
                let info = self.program.const_value_in(&self.current_module, name);
                info.and_then(|i| i.value.clone())
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
                            BinOp::Add | BinOp::AddWrap | BinOp::AddSat => {
                                a.checked_add(b).unwrap_or_else(|| a.wrapping_add(b))
                            }
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
        let functions: Vec<Function> = self
            .program
            .functions
            .values()
            .filter(|f| f.owner.is_none() && !f.external && f.decl.body.is_some())
            .cloned()
            .collect();
        for f in functions {
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
        let source_id = self
            .sources
            .iter()
            .position(|s| s.path == f.file)
            .unwrap_or(self.current_source);
        self.current_source = source_id;
        self.current_module = f.module.clone();
        self.check_body_function(
            f,
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

    fn check_test(&mut self, t: &TestInfo) {
        let source_id = self
            .sources
            .iter()
            .position(|s| s.path == t.file)
            .unwrap_or(self.current_source);
        self.current_source = source_id;
        self.current_module = t.module.clone();
        let source = &self.sources.files[source_id];
        let mut checker = Checker::new(
            source,
            &self.program,
            &mut self.diagnostics,
            Type::Void,
            Some(Type::External("TestErr".to_string())),
            HashMap::new(),
            None,
            true,
            &t.module,
        );
        checker.push_scope();
        let _ret = checker.check_block(&t.decl.body);
        checker.pop_scope(true);
        checker.finish();
    }

    fn check_body_function(
        &mut self,
        function: &Function,
        ret: Type,
        error: Option<Type>,
        bounds: HashMap<String, Vec<String>>,
        self_ty: Option<Type>,
        is_test: bool,
    ) {
        let f = &function.decl;
        let module = function.module.clone();
        let source = &self.sources.files[self.current_source];
        let mut checker = Checker::new(
            source,
            &self.program,
            &mut self.diagnostics,
            ret.clone(),
            error,
            bounds,
            self_ty.clone(),
            is_test,
            &module,
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
            let ty = function
                .signature_params
                .get(i)
                .cloned()
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
