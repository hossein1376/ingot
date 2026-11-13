//! Phase 5, first half: declare the definitions of a package, give every module its
//! two namespaces, and bind imports.

use crate::defs::*;
use crate::when;
use crate::{ModuleInput, PackageInput};
use ingot_diag::diagnostic::{Diagnostic, Edit};
use ingot_diag::source::{Source, Span};
use ingot_syntax::ast;
use std::collections::{BTreeMap, BTreeSet};

pub(crate) enum Lookup {
    Found(DefId),
    Poison,
    Missing,
}

pub(crate) enum ItemLookup {
    Found(DefId),
    /// The item exists but is not `pub` and the lookup is from another module.
    Private(DefId),
    Poison,
    Missing,
}

pub(crate) struct Resolver<'a> {
    pub sources: &'a ingot_diag::source::Sources,
    pub sema: Sema,
    pub diags: Vec<Diagnostic>,
    pub not_implemented: Vec<String>,
    /// Definitions by the file and start offset of their name (an impl or a test:
    /// of the declaration).
    pub decl_at: BTreeMap<(usize, usize), DefId>,
    /// The `Self` definition of each interface and impl.
    pub self_defs: BTreeMap<DefId, DefId>,
    pub target: &'a crate::Target,
}

pub(crate) fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0; b.len() + 1];
    for i in 1..=a.len() {
        cur[0] = i;
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            cur[j] = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

/// The candidate nearest to `name` when it is within edit distance 2 and no other
/// candidate is as near (the fix of E0304 and E0305).
pub(crate) fn unique_near<'n>(
    name: &str,
    candidates: impl Iterator<Item = &'n str>,
) -> Option<String> {
    let mut best: Vec<&str> = Vec::new();
    let mut best_distance = usize::MAX;
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    for c in candidates {
        if c == name || !seen.insert(c) {
            continue;
        }
        let d = levenshtein(name, c);
        if d > 2 {
            continue;
        }
        if d < best_distance {
            best_distance = d;
            best.clear();
        }
        if d == best_distance {
            best.push(c);
        }
    }
    match best.as_slice() {
        [only] => Some((*only).to_string()),
        _ => None,
    }
}

impl<'a> Resolver<'a> {
    pub fn source(&self, file: usize) -> &'a Source {
        self.sources.get(file)
    }

    pub fn diag(
        &mut self,
        code: &'static str,
        message: impl Into<String>,
        file: usize,
        span: Span,
    ) -> &mut Diagnostic {
        let d = Diagnostic::new(code, message, self.sources.get(file), span);
        self.diags.push(d);
        self.diags.last_mut().expect("a diagnostic was just pushed")
    }

    pub fn not_implemented(&mut self, what: impl Into<String>) {
        let what = what.into();
        if !self.not_implemented.contains(&what) {
            self.not_implemented.push(what);
        }
    }

    // ------------------------------------------------------------ packages

    /// Declare a package and resolve its imports. Returns the lowering inputs.
    pub fn declare_package(&mut self, input: PackageInput) -> DeclaredPackage {
        let pkg_id = PackageId(self.sema.packages.len() as u32);
        let pkg_def = self.sema.add_def(Def {
            kind: DefKind::Package,
            name: input.name.clone(),
            span: Span::default(),
            file: None,
            module: BUILTIN_MODULE,
            vis: Vis::Pub,
            parent: None,
        });
        self.sema.packages.push(PackageData {
            name: input.name.clone(),
            def: pkg_def,
            root: None,
            modules: Vec::new(),
        });

        // Modules first, so that every import can see every module of the package.
        let mut declared = Vec::new();
        for m in input.modules {
            let ModuleInput { path, file, ast } = m;
            let id = ModuleId(self.sema.modules.len() as u32);
            let name = path.rsplit('.').next().unwrap_or(&path).to_string();
            let def = self.sema.add_def(Def {
                kind: DefKind::Module,
                name,
                span: Span::default(),
                file: Some(file),
                module: id,
                vis: Vis::Pub,
                parent: None,
            });
            self.sema.modules.push(ModuleData {
                id,
                path: path.clone(),
                package: pkg_id,
                def,
                file: Some(file),
                poisoned: ast.is_none(),
                values: BTreeMap::new(),
                types: BTreeMap::new(),
                poison_names: BTreeSet::new(),
                poison_items: BTreeSet::new(),
                imports: Vec::new(),
                import_names: BTreeMap::new(),
                when_mentions: BTreeSet::new(),
                items: Vec::new(),
            });
            self.sema.module_paths.insert(path.clone(), id);
            let pkg = &mut self.sema.packages[pkg_id.0 as usize];
            pkg.modules.push(id);
            if path == input.name {
                pkg.root = Some(id);
            }
            declared.push((id, file, ast));
        }

        // Phase 4, then the declarations of every module.
        let mut modules = Vec::new();
        for (id, file, ast) in declared {
            let Some(mut ast) = ast else {
                continue;
            };
            let source = self.sources.get(file);
            let result = when::eliminate(
                &mut ast,
                self.target,
                source,
                &mut self.diags,
                &mut self.not_implemented,
            );
            self.declare_items(id, file, &ast.items);
            if self.sema.module(id).path == crate::BUILTIN_MODULE_PATH {
                self.sema.register_builtin_module(id);
            }
            for (name, ns) in result.poisoned_names {
                let module = self.sema.module_mut(id);
                let taken = match ns {
                    Ns::Value => module.values.contains_key(&name),
                    Ns::Type => module.types.contains_key(&name),
                };
                if !taken {
                    module.poison_items.insert(name);
                }
            }
            self.sema.module_mut(id).when_mentions = result.mentioned;
            modules.push((id, file, ast));
        }

        // Imports of every module, now that all declarations exist.
        for (id, file, ast) in &modules {
            for import in &ast.imports {
                self.bind_import(*id, *file, import);
            }
        }
        DeclaredPackage {
            name: input.name,
            modules,
        }
    }

    // ---------------------------------------------------------- declarations

    #[allow(clippy::too_many_arguments)]
    fn new_def(
        &mut self,
        kind: DefKind,
        name: &str,
        span: Span,
        file: usize,
        module: ModuleId,
        public: bool,
        parent: Option<DefId>,
    ) -> DefId {
        let id = self.sema.add_def(Def {
            kind,
            name: name.to_string(),
            span,
            file: Some(file),
            module,
            vis: if public { Vis::Pub } else { Vis::Private },
            parent,
        });
        self.decl_at.insert((file, span.start), id);
        id
    }

    /// Declare a module-level name. On a duplicate, report E0308 on the new
    /// definition and leave the first one in the namespace.
    #[allow(clippy::too_many_arguments)]
    fn declare_in_module(
        &mut self,
        module: ModuleId,
        file: usize,
        kind: DefKind,
        name: &str,
        name_span: Span,
        public: bool,
    ) -> DefId {
        let id = self.new_def(kind, name, name_span, file, module, public, None);
        let ns = kind
            .namespace()
            .expect("module-level definitions are named");
        let existing = {
            let m = self.sema.module(module);
            match ns {
                Ns::Value => m.values.get(name).copied(),
                Ns::Type => m.types.get(name).copied(),
            }
        };
        match existing {
            Some(first) => {
                let (first_span, first_file) = {
                    let d = self.sema.def(first);
                    (d.span, d.file.unwrap_or(file))
                };
                let first_path = self.sources.get(first_file).path.clone();
                self.diag(
                    "E0308",
                    format!("`{name}` is defined twice in this module"),
                    file,
                    name_span,
                )
                .relate_in("previous", &first_path, first_span);
            }
            None => {
                let m = self.sema.module_mut(module);
                match ns {
                    Ns::Value => m.values.insert(name.to_string(), id),
                    Ns::Type => m.types.insert(name.to_string(), id),
                };
            }
        }
        self.sema.module_mut(module).items.push(id);
        id
    }

    /// Report E0308 when `name` is already in `seen`; otherwise remember it.
    fn check_unique(
        &mut self,
        file: usize,
        seen: &mut BTreeMap<String, Span>,
        what: &str,
        name: &str,
        span: Span,
    ) {
        if let Some(prev) = seen.get(name).copied() {
            let path = self.sources.get(file).path.clone();
            self.diag(
                "E0308",
                format!("{what} `{name}` is defined twice"),
                file,
                span,
            )
            .relate_in("previous", &path, prev);
        } else {
            seen.insert(name.to_string(), span);
        }
    }

    fn declare_generics(
        &mut self,
        module: ModuleId,
        file: usize,
        parent: DefId,
        generics: &[ast::GenericParam],
    ) {
        let mut seen = BTreeMap::new();
        for g in generics {
            self.new_def(
                DefKind::GenericParam {
                    is_const: g.is_const,
                },
                &g.name,
                g.name_span,
                file,
                module,
                false,
                Some(parent),
            );
            self.check_unique(file, &mut seen, "generic parameter", &g.name, g.name_span);
        }
    }

    fn declare_fn_members(
        &mut self,
        module: ModuleId,
        file: usize,
        parent: DefId,
        f: &ast::FnDecl,
        public: bool,
    ) -> DefId {
        let id = self.new_def(
            DefKind::Fn,
            &f.name,
            f.name_span,
            file,
            module,
            public,
            Some(parent),
        );
        self.declare_fn_parts(module, file, id, f);
        id
    }

    /// Generic parameters and the duplicate-parameter check of a function.
    fn declare_fn_parts(&mut self, module: ModuleId, file: usize, id: DefId, f: &ast::FnDecl) {
        self.declare_generics(module, file, id, &f.generics);
        let mut seen = BTreeMap::new();
        for p in &f.params {
            self.check_unique(file, &mut seen, "parameter", &p.name, p.name_span);
        }
    }

    fn declare_items(&mut self, module: ModuleId, file: usize, items: &[ast::Item]) {
        let mut tests: BTreeMap<(bool, String), Span> = BTreeMap::new();
        for item in items {
            match item {
                ast::Item::Fn(f) => {
                    let id = self.declare_in_module(
                        module,
                        file,
                        DefKind::Fn,
                        &f.name,
                        f.name_span,
                        f.mods.public,
                    );
                    self.declare_fn_parts(module, file, id, f);
                }
                ast::Item::Struct(s) => {
                    let id = self.declare_in_module(
                        module,
                        file,
                        DefKind::Struct,
                        &s.name,
                        s.name_span,
                        s.public,
                    );
                    self.declare_generics(module, file, id, &s.generics);
                    let mut seen = BTreeMap::new();
                    for f in &s.fields {
                        self.new_def(
                            DefKind::Field,
                            &f.name,
                            f.name_span,
                            file,
                            module,
                            f.public,
                            Some(id),
                        );
                        self.check_unique(file, &mut seen, "field", &f.name, f.name_span);
                    }
                }
                ast::Item::Enum(e) => {
                    let id = self.declare_in_module(
                        module,
                        file,
                        DefKind::Enum,
                        &e.name,
                        e.name_span,
                        e.public,
                    );
                    self.declare_generics(module, file, id, &e.generics);
                    let mut seen = BTreeMap::new();
                    for v in &e.variants {
                        // Variants are as visible as their enum.
                        self.new_def(
                            DefKind::Variant,
                            &v.name,
                            v.name_span,
                            file,
                            module,
                            e.public,
                            Some(id),
                        );
                        self.check_unique(file, &mut seen, "variant", &v.name, v.name_span);
                    }
                }
                ast::Item::Interface(i) => {
                    let id = self.declare_in_module(
                        module,
                        file,
                        DefKind::Interface,
                        &i.name,
                        i.name_span,
                        i.public,
                    );
                    self.declare_generics(module, file, id, &i.generics);
                    let self_def = self.sema.add_def(Def {
                        kind: DefKind::SelfTy,
                        name: "Self".to_string(),
                        span: i.name_span,
                        file: Some(file),
                        module,
                        vis: Vis::Private,
                        parent: Some(id),
                    });
                    self.self_defs.insert(id, self_def);
                    let mut values = BTreeMap::new();
                    let mut types = BTreeMap::new();
                    for m in &i.members {
                        match m {
                            ast::Item::Fn(f) => {
                                self.check_unique(
                                    file,
                                    &mut values,
                                    "method",
                                    &f.name,
                                    f.name_span,
                                );
                                // Interface methods are as visible as the interface.
                                self.declare_fn_members(module, file, id, f, i.public);
                            }
                            ast::Item::TypeAlias(t) => {
                                self.check_unique(
                                    file,
                                    &mut types,
                                    "associated type",
                                    &t.name,
                                    t.name_span,
                                );
                                self.new_def(
                                    DefKind::AssocType,
                                    &t.name,
                                    t.name_span,
                                    file,
                                    module,
                                    i.public,
                                    Some(id),
                                );
                            }
                            _ => {}
                        }
                    }
                }
                ast::Item::Impl(i) => {
                    let id = self.new_def(
                        DefKind::Impl,
                        "impl",
                        Span::new(i.span.start, i.span.start),
                        file,
                        module,
                        false,
                        None,
                    );
                    self.sema.module_mut(module).items.push(id);
                    self.declare_generics(module, file, id, &i.generics);
                    let self_def = self.sema.add_def(Def {
                        kind: DefKind::SelfTy,
                        name: "Self".to_string(),
                        span: i.span,
                        file: Some(file),
                        module,
                        vis: Vis::Private,
                        parent: Some(id),
                    });
                    self.self_defs.insert(id, self_def);
                    let mut values = BTreeMap::new();
                    let mut types = BTreeMap::new();
                    for m in &i.members {
                        match m {
                            ast::Item::Fn(f) => {
                                self.check_unique(
                                    file,
                                    &mut values,
                                    "function",
                                    &f.name,
                                    f.name_span,
                                );
                                self.declare_fn_members(module, file, id, f, f.mods.public);
                            }
                            ast::Item::Const(c) => {
                                self.check_unique(
                                    file,
                                    &mut values,
                                    "constant",
                                    &c.name,
                                    c.name_span,
                                );
                                self.new_def(
                                    DefKind::Const,
                                    &c.name,
                                    c.name_span,
                                    file,
                                    module,
                                    c.public,
                                    Some(id),
                                );
                            }
                            ast::Item::TypeAlias(t) => {
                                self.check_unique(
                                    file,
                                    &mut types,
                                    "associated type",
                                    &t.name,
                                    t.name_span,
                                );
                                let a = self.new_def(
                                    DefKind::AssocType,
                                    &t.name,
                                    t.name_span,
                                    file,
                                    module,
                                    t.public,
                                    Some(id),
                                );
                                self.declare_generics(module, file, a, &t.generics);
                            }
                            _ => {}
                        }
                    }
                }
                ast::Item::Const(c) => {
                    self.declare_in_module(
                        module,
                        file,
                        DefKind::Const,
                        &c.name,
                        c.name_span,
                        c.public,
                    );
                }
                ast::Item::Static(s) => {
                    self.declare_in_module(
                        module,
                        file,
                        DefKind::Static,
                        &s.name,
                        s.name_span,
                        s.public,
                    );
                }
                ast::Item::TypeAlias(t) => {
                    let id = self.declare_in_module(
                        module,
                        file,
                        DefKind::TypeAlias,
                        &t.name,
                        t.name_span,
                        t.public,
                    );
                    self.declare_generics(module, file, id, &t.generics);
                }
                ast::Item::Extern(x) => {
                    for f in &x.functions {
                        let id = self.declare_in_module(
                            module,
                            file,
                            DefKind::ExternFn,
                            &f.name,
                            f.name_span,
                            x.public,
                        );
                        self.declare_fn_parts(module, file, id, f);
                    }
                }
                ast::Item::Test(t) => {
                    let id = self.new_def(
                        DefKind::Test { bench: t.bench },
                        &t.name,
                        Span::new(t.span.start, t.span.start),
                        file,
                        module,
                        false,
                        None,
                    );
                    self.sema.module_mut(module).items.push(id);
                    let key = (t.bench, t.name.clone());
                    if let Some(prev) = tests.get(&key).copied() {
                        let path = self.sources.get(file).path.clone();
                        let kind = if t.bench { "bench" } else { "test" };
                        self.diag(
                            "E1707",
                            format!(
                                "there are two {kind} blocks named \"{}\" in this module",
                                t.name
                            ),
                            file,
                            header_span(self.sources.get(file), t.span),
                        )
                        .relate_in("previous", &path, prev);
                    } else {
                        tests.insert(key, header_span(self.sources.get(file), t.span));
                    }
                }
                ast::Item::When(_) | ast::Item::Poison => {}
            }
        }
    }

    // --------------------------------------------------------------- imports

    fn bind_import(&mut self, module: ModuleId, file: usize, imp: &ast::Import) {
        let name = imp.bound_name().to_string();
        let name_span = imp
            .alias_span
            .or_else(|| imp.segments.last().map(|(_, s)| *s))
            .unwrap_or(imp.span);
        let target = self.resolve_import(module, file, imp);
        let (ns, target) = match target {
            Some(Imported::Module(m)) => (Ns::Value, ImportTarget::Module(m)),
            Some(Imported::Item(d)) => {
                let ns = self.sema.def(d).kind.namespace().unwrap_or(Ns::Value);
                (ns, ImportTarget::Item(d))
            }
            Some(Imported::Poison) | None => {
                // Whatever the import would have bound resolves silently.
                self.sema
                    .module_mut(module)
                    .poison_names
                    .insert(name.clone());
                return;
            }
        };
        // Two imports of one name: E0302 on the second.
        if let Some(&first) = self
            .sema
            .module(module)
            .import_names
            .get(&(ns, name.clone()))
        {
            let first_span = self.sema.module(module).imports[first].name_span;
            let path = self.sources.get(file).path.clone();
            self.diag(
                "E0302",
                format!("`{name}` is already bound by an earlier import"),
                file,
                name_span,
            )
            .relate_in("previous", &path, first_span);
            self.sema.module_mut(module).poison_names.insert(name);
            return;
        }
        // An import and an item of one name: the item is the later declaration.
        let item = {
            let m = self.sema.module(module);
            match ns {
                Ns::Value => m.values.get(&name).copied(),
                Ns::Type => m.types.get(&name).copied(),
            }
        };
        if let Some(item) = item {
            let item_span = self.sema.def(item).span;
            let path = self.sources.get(file).path.clone();
            self.diag(
                "E0308",
                format!("`{name}` is both imported and defined in this module"),
                file,
                item_span,
            )
            .relate_in("previous", &path, name_span);
            return;
        }
        let m = self.sema.module_mut(module);
        let index = m.imports.len();
        m.imports.push(ImportBinding {
            name: name.clone(),
            span: imp.span,
            name_span,
            target: target.clone(),
            used: false,
        });
        m.import_names.insert((ns, name.clone()), index);
        let bound = match &target {
            ImportTarget::Module(mid) => self.sema.module(*mid).def,
            ImportTarget::Item(d) => *d,
            ImportTarget::Poison => unreachable!("poison imports return early"),
        };
        let m = self.sema.module_mut(module);
        match ns {
            Ns::Value => m.values.insert(name, bound),
            Ns::Type => m.types.insert(name, bound),
        };
    }

    fn resolve_import(
        &mut self,
        module: ModuleId,
        file: usize,
        imp: &ast::Import,
    ) -> Option<Imported> {
        let segs = &imp.segments;
        let n = segs.len();
        if n == 0 {
            return None;
        }
        let join = |upto: usize| -> String {
            segs[..upto]
                .iter()
                .map(|(s, _)| s.as_str())
                .collect::<Vec<_>>()
                .join(".")
        };
        // The longest prefix that is a module.
        let mut longest = 0;
        for k in 1..=n {
            if self.sema.module_paths.contains_key(&join(k)) {
                longest = k;
            }
        }
        let module_at = |this: &Self, k: usize| this.sema.module_paths.get(&join(k)).copied();
        let last = &segs[n - 1];
        let ns = if last.0.starts_with(|c: char| c.is_ascii_uppercase()) {
            Ns::Type
        } else {
            Ns::Value
        };
        // The last segment as an item of the module before it.
        let item = if n >= 2 && module_at(self, n - 1).is_some() {
            let parent = module_at(self, n - 1).expect("checked above");
            Some((parent, self.lookup_item_in(parent, module, ns, &last.0)))
        } else {
            None
        };
        if longest == n {
            let as_module = module_at(self, n).expect("longest is a module");
            if let Some((_, ItemLookup::Found(_) | ItemLookup::Private(_))) = &item {
                self.diag(
                    "E0316",
                    format!("`{}` names both a module and an item", imp.path_text()),
                    file,
                    last.1,
                );
                return None;
            }
            return Some(Imported::Module(as_module));
        }
        if longest + 1 == n {
            let Some((parent, found)) = item else {
                // The package or the module before the last segment is missing.
                return self.unresolved_import(file, imp, longest);
            };
            return match found {
                ItemLookup::Found(d) => Some(Imported::Item(d)),
                ItemLookup::Private(d) => {
                    let declared = self.sema.def(d).span;
                    let dfile = self.sema.def(d).file.unwrap_or(file);
                    let dpath = self.sources.get(dfile).path.clone();
                    self.diag(
                        "E0306",
                        format!("`{}` is private to its module", last.0),
                        file,
                        last.1,
                    )
                    .relate_in("declared", &dpath, declared);
                    None
                }
                ItemLookup::Poison => Some(Imported::Poison),
                ItemLookup::Missing => {
                    let _ = parent;
                    self.unresolved_import(file, imp, longest)
                }
            };
        }
        self.unresolved_import(file, imp, longest)
    }

    /// E0301 on the first segment that names no module or item.
    fn unresolved_import(
        &mut self,
        file: usize,
        imp: &ast::Import,
        failing: usize,
    ) -> Option<Imported> {
        // A poisoned module before the failing segment might declare the item.
        if failing > 0 {
            let path = imp.segments[..failing]
                .iter()
                .map(|(s, _)| s.as_str())
                .collect::<Vec<_>>()
                .join(".");
            if let Some(&m) = self.sema.module_paths.get(&path)
                && self.sema.module(m).poisoned
            {
                return Some(Imported::Poison);
            }
        }
        let (name, span) = &imp.segments[failing.min(imp.segments.len() - 1)];
        self.diag(
            "E0301",
            format!("`{name}` is not a module or a public item here"),
            file,
            *span,
        );
        None
    }

    // --------------------------------------------------------------- lookups

    /// Look `name` up in the scope of `module`: its items and imports, then the
    /// builtins. Marks the import that binds the name as used.
    pub fn lookup_in_scope(&mut self, module: ModuleId, ns: Ns, name: &str) -> Lookup {
        let m = self.sema.module_mut(module);
        let found = match ns {
            Ns::Value => m.values.get(name).copied(),
            Ns::Type => m.types.get(name).copied(),
        };
        if let Some(d) = found {
            if let Some(&i) = m.import_names.get(&(ns, name.to_string())) {
                m.imports[i].used = true;
            }
            return Lookup::Found(d);
        }
        if m.poison_names.contains(name) || m.poison_items.contains(name) {
            return Lookup::Poison;
        }
        if ns == Ns::Type
            && let Some(&d) = self.sema.module(BUILTIN_MODULE).types.get(name)
        {
            return Lookup::Found(d);
        }
        Lookup::Missing
    }

    /// Look `name` up among the items declared in `target` (imports of that module
    /// are not items of it), as seen from the module `from`.
    pub fn lookup_item_in(
        &mut self,
        target: ModuleId,
        from: ModuleId,
        ns: Ns,
        name: &str,
    ) -> ItemLookup {
        let t = self.sema.module(target);
        if t.poisoned {
            return ItemLookup::Poison;
        }
        if t.import_names.contains_key(&(ns, name.to_string())) {
            return ItemLookup::Missing;
        }
        let found = match ns {
            Ns::Value => t.values.get(name).copied(),
            Ns::Type => t.types.get(name).copied(),
        };
        match found {
            Some(d) => {
                if self.sema.def(d).vis == Vis::Pub || target == from {
                    ItemLookup::Found(d)
                } else {
                    ItemLookup::Private(d)
                }
            }
            None if t.poison_items.contains(name) => ItemLookup::Poison,
            None => ItemLookup::Missing,
        }
    }

    /// The edit that qualifies `name` with the one imported module that exports it
    /// (the fix of E0304 and E0305), if exactly one does.
    pub fn qualify_edit(
        &self,
        module: ModuleId,
        file: usize,
        ns: Ns,
        name: &str,
        at: Span,
    ) -> Option<Edit> {
        let m = self.sema.module(module);
        let mut owners = Vec::new();
        for imp in &m.imports {
            let ImportTarget::Module(target) = imp.target else {
                continue;
            };
            let t = self.sema.module(target);
            let found = match ns {
                Ns::Value => t.values.get(name),
                Ns::Type => t.types.get(name),
            };
            if let Some(&d) = found
                && !t.import_names.contains_key(&(ns, name.to_string()))
                && self.sema.def(d).vis == Vis::Pub
            {
                owners.push(imp.name.clone());
            }
        }
        owners.sort();
        owners.dedup();
        match owners.as_slice() {
            [only] => Some(Edit {
                file: self.sources.get(file).path.clone(),
                span: Span::empty(at.start),
                text: format!("{only}."),
            }),
            _ => None,
        }
    }
}

enum Imported {
    Module(ModuleId),
    Item(DefId),
    Poison,
}

pub(crate) struct DeclaredPackage {
    pub name: String,
    pub modules: Vec<(ModuleId, usize, ast::Module)>,
}

/// The first line of a declaration, up to its opening brace, as a span.
fn header_span(source: &Source, span: Span) -> Span {
    let text = source.text_of(span);
    let end = text
        .find('{')
        .map(|i| span.start + i)
        .unwrap_or(span.end)
        .max(span.start);
    Span::new(span.start, end)
}
