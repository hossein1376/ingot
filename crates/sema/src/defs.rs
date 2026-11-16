//! The definition arena: every declared thing, and every builtin name, is a `DefId`.

use ingot_diag::source::Span;
use ingot_syntax::token::{ScalarTy, VectorTy};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DefId(pub u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ModuleId(pub u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PackageId(pub u32);

/// The pseudo-module that holds the builtin names.
pub const BUILTIN_MODULE: ModuleId = ModuleId(0);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Vis {
    Private,
    /// `pub(pkg)`: every module of the declaring package (01-core §2).
    Pkg,
    Pub,
}

impl Vis {
    pub fn from_flags(public: bool, pkg: bool) -> Vis {
        if pkg {
            Vis::Pkg
        } else if public {
            Vis::Pub
        } else {
            Vis::Private
        }
    }

    pub fn is_pub(self) -> bool {
        self == Vis::Pub
    }

    pub fn name(self) -> &'static str {
        match self {
            Vis::Private => "private",
            Vis::Pkg => "pub(pkg)",
            Vis::Pub => "pub",
        }
    }
}

/// The two namespaces of pipeline section 4.3.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Ns {
    Value,
    Type,
}

/// Language items that are names but are declared by no source file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Builtin {
    Scalar(ScalarTy),
    Vector(VectorTy),
    /// `Option[T]`, the type behind `?T` (section 3).
    Option,
    /// `Result[T, E]`, the type behind `T ! E`.
    Result,
    /// `Idx`, `Idx2`, `Idx3`: kernel indices (section 27.1).
    Idx,
    Idx2,
    Idx3,
    /// The `Executor` interface (section 27.2). It has no members a program can
    /// call, so unlike the other built-in interfaces it has no declaration.
    Executor,
    /// `Grid` and `GridMut`: the kernel grids (section 27).
    Grid,
    GridMut,
    /// The built-in bounds of section 8.
    Copy,
    Discard,
    Send,
    Sync,
    Owned,
    Int,
    SInt,
    UInt,
    Float,
    Num,
    Fn,
    FnMut,
    FnOnce,
}

/// The names that resolve to builtins that have no declaration (the scalar and vector
/// types are keywords and never looked up by name). Together with the names the
/// builtin declarations (`builtin.ingot`) give, these are exactly the builtin names of
/// specification section 8 (and pipeline section 4.3).
const NAMED_BUILTINS: [(&str, Builtin); 21] = [
    ("Option", Builtin::Option),
    ("Result", Builtin::Result),
    ("Idx", Builtin::Idx),
    ("Idx2", Builtin::Idx2),
    ("Idx3", Builtin::Idx3),
    ("Executor", Builtin::Executor),
    ("Grid", Builtin::Grid),
    ("GridMut", Builtin::GridMut),
    ("Copy", Builtin::Copy),
    ("Discard", Builtin::Discard),
    ("Send", Builtin::Send),
    ("Sync", Builtin::Sync),
    ("Owned", Builtin::Owned),
    ("Int", Builtin::Int),
    ("SInt", Builtin::SInt),
    ("UInt", Builtin::UInt),
    ("Float", Builtin::Float),
    ("Num", Builtin::Num),
    ("Fn", Builtin::Fn),
    ("FnMut", Builtin::FnMut),
    ("FnOnce", Builtin::FnOnce),
];

const ALL_SCALARS: [ScalarTy; 13] = [
    ScalarTy::I8,
    ScalarTy::I16,
    ScalarTy::I32,
    ScalarTy::I64,
    ScalarTy::U8,
    ScalarTy::U16,
    ScalarTy::U32,
    ScalarTy::U64,
    ScalarTy::F32,
    ScalarTy::F64,
    ScalarTy::Bool,
    ScalarTy::Void,
    ScalarTy::Never,
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DefKind {
    Package,
    Module,
    Fn,
    ExternFn,
    Struct,
    Enum,
    Variant,
    Field,
    Interface,
    /// `type Item;` in an interface, or `type Item = T;` in an impl.
    AssocType,
    Impl,
    Const,
    Static,
    TypeAlias,
    GenericParam {
        is_const: bool,
    },
    /// `Self` inside an interface or an impl.
    SelfTy,
    Test {
        bench: bool,
    },
    Builtin(Builtin),
    /// A declaration that exists but could not be read (a file with syntax errors,
    /// a `when` with an unreadable condition). Names bound to it resolve to
    /// `Res::Poison`.
    Poison,
}

impl DefKind {
    /// The namespace the definition's name lives in, for kinds that are named in
    /// a module scope.
    pub fn namespace(self) -> Option<Ns> {
        match self {
            DefKind::Module
            | DefKind::Fn
            | DefKind::ExternFn
            | DefKind::Const
            | DefKind::Static => Some(Ns::Value),
            DefKind::Struct | DefKind::Enum | DefKind::Interface | DefKind::TypeAlias => {
                Some(Ns::Type)
            }
            _ => None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Def {
    pub kind: DefKind,
    pub name: String,
    pub span: Span,
    /// Index into the `Sources` the pipeline was given. `None` for builtins and
    /// packages.
    pub file: Option<usize>,
    pub module: ModuleId,
    pub vis: Vis,
    /// The definition that contains this one: a struct for its fields, an
    /// interface or impl for its members, an item for its generic parameters.
    pub parent: Option<DefId>,
}

#[derive(Clone, Debug)]
pub enum ImportTarget {
    Module(ModuleId),
    Item(DefId),
    /// The import failed (and was reported) or came from a poisoned module.
    Poison,
}

#[derive(Clone, Debug)]
pub struct ImportBinding {
    pub name: String,
    /// The whole `import ...;` declaration.
    pub span: Span,
    /// The bound name (the alias, or the last segment).
    pub name_span: Span,
    pub target: ImportTarget,
    pub used: bool,
}

#[derive(Clone, Debug)]
pub struct ModuleData {
    pub id: ModuleId,
    /// Dotted path, starting with the package name: `app`, `app.net.http`.
    pub path: String,
    pub package: PackageId,
    pub def: DefId,
    pub file: Option<usize>,
    /// The module's file had a lexical or syntax error: it declares nothing.
    pub poisoned: bool,
    pub values: BTreeMap<String, DefId>,
    pub types: BTreeMap<String, DefId>,
    /// Names that must resolve to `Res::Poison` without a report: imports that
    /// failed, and anything imported from a poisoned module.
    pub poison_names: BTreeSet<String>,
    /// Items the module would declare in a `when` branch that could not be decided.
    /// Unlike `poison_names`, these are names of the module itself, so other modules
    /// reach them too.
    pub poison_items: BTreeSet<String>,
    pub imports: Vec<ImportBinding>,
    /// Which import binds a name, so that a lookup can mark it used and so that
    /// imports can be told from declared items.
    pub import_names: BTreeMap<(Ns, String), usize>,
    /// Names that the module's removed `when` branches mention. A use in a branch that
    /// is not taken on this target still counts, so an import of such a name is not
    /// unused (E0310).
    pub when_mentions: BTreeSet<String>,
    /// The module-level definitions in source order.
    pub items: Vec<DefId>,
}

#[derive(Clone, Debug)]
pub struct PackageData {
    pub name: String,
    pub def: DefId,
    pub root: Option<ModuleId>,
    pub modules: Vec<ModuleId>,
}

/// All definitions, modules and packages of one compilation.
#[derive(Clone, Debug)]
pub struct Sema {
    pub defs: Vec<Def>,
    pub modules: Vec<ModuleData>,
    pub packages: Vec<PackageData>,
    /// Module lookup by dotted path.
    pub module_paths: BTreeMap<String, ModuleId>,
    scalar_defs: Vec<(ScalarTy, DefId)>,
    vector_defs: Vec<(VectorTy, DefId)>,
}

impl Default for Sema {
    fn default() -> Self {
        Sema::new()
    }
}

impl Sema {
    pub fn new() -> Sema {
        let mut sema = Sema {
            defs: Vec::new(),
            modules: Vec::new(),
            packages: Vec::new(),
            module_paths: BTreeMap::new(),
            scalar_defs: Vec::new(),
            vector_defs: Vec::new(),
        };
        // Module 0 is the builtin scope. Its package is a placeholder.
        let builtin_def = DefId(0);
        sema.defs.push(Def {
            kind: DefKind::Module,
            name: "<builtin>".to_string(),
            span: Span::default(),
            file: None,
            module: BUILTIN_MODULE,
            vis: Vis::Pub,
            parent: None,
        });
        sema.modules.push(ModuleData {
            id: BUILTIN_MODULE,
            path: "<builtin>".to_string(),
            package: PackageId(u32::MAX),
            def: builtin_def,
            file: None,
            poisoned: false,
            values: BTreeMap::new(),
            types: BTreeMap::new(),
            poison_names: BTreeSet::new(),
            poison_items: BTreeSet::new(),
            imports: Vec::new(),
            import_names: BTreeMap::new(),
            when_mentions: BTreeSet::new(),
            items: Vec::new(),
        });
        for (name, builtin) in NAMED_BUILTINS.iter() {
            let id = sema.new_builtin(name, *builtin);
            sema.modules[0].types.insert((*name).to_string(), id);
        }
        for scalar in ALL_SCALARS {
            let id = sema.new_builtin(scalar.name(), Builtin::Scalar(scalar));
            sema.scalar_defs.push((scalar, id));
        }
        sema
    }

    fn new_builtin(&mut self, name: &str, builtin: Builtin) -> DefId {
        self.add_def(Def {
            kind: DefKind::Builtin(builtin),
            name: name.to_string(),
            span: Span::default(),
            file: None,
            module: BUILTIN_MODULE,
            vis: Vis::Pub,
            parent: None,
        })
    }

    pub fn add_def(&mut self, def: Def) -> DefId {
        let id = DefId(self.defs.len() as u32);
        self.defs.push(def);
        id
    }

    pub fn def(&self, id: DefId) -> &Def {
        &self.defs[id.0 as usize]
    }

    pub fn module(&self, id: ModuleId) -> &ModuleData {
        &self.modules[id.0 as usize]
    }

    pub fn module_mut(&mut self, id: ModuleId) -> &mut ModuleData {
        &mut self.modules[id.0 as usize]
    }

    /// 01-core §2 and §4.2: whether a field declared in `declared` is visible from
    /// `from`. `pub(pkg)` is visible in the declaring package; a private field only
    /// in its module.
    pub fn field_visible(
        &self,
        declared: ModuleId,
        from: ModuleId,
        public: bool,
        pkg: bool,
    ) -> bool {
        declared == from
            || if pkg {
                self.module(declared).package == self.module(from).package
            } else {
                public
            }
    }

    /// Phase 5's rule for `target.name` as seen from `from`: the name is an item of
    /// `target` (not an import bound there) and `from` may name it. Completion reads
    /// this; `resolve::Resolver::lookup_item_in` keeps the same rule.
    pub fn may_name_item(
        &self,
        target: ModuleId,
        from: ModuleId,
        ns: Ns,
        name: &str,
    ) -> Option<DefId> {
        let t = self.module(target);
        if t.poisoned || t.import_names.contains_key(&(ns, name.to_string())) {
            return None;
        }
        let found = match ns {
            Ns::Value => t.values.get(name),
            Ns::Type => t.types.get(name),
        };
        let def = *found?;
        let visible = target == from
            || match self.def(def).vis {
                Vis::Pub => true,
                Vis::Pkg => self.module(target).package == self.module(from).package,
                Vis::Private => false,
            };
        visible.then_some(def)
    }

    /// The pre-made definition of a scalar type name.
    pub fn scalar_def(&self, scalar: ScalarTy) -> DefId {
        self.scalar_defs
            .iter()
            .find(|(s, _)| *s == scalar)
            .map(|(_, d)| *d)
            .expect("every scalar type has a builtin definition")
    }

    /// The definition of a vector type name, made on first use.
    pub fn vector_def(&mut self, vector: VectorTy) -> DefId {
        if let Some((_, d)) = self.vector_defs.iter().find(|(v, _)| *v == vector) {
            return *d;
        }
        let id = self.new_builtin(&vector.name(), Builtin::Vector(vector));
        self.vector_defs.push((vector, id));
        id
    }

    /// Make the types the builtin declarations (`builtin.ingot`) declare in `module`
    /// the builtin names `Ordering`, `Eq`, ... A declaration or import of the same
    /// name in a module still hides them (lookups try the module first).
    pub fn register_builtin_module(&mut self, module: ModuleId) {
        let declared: Vec<(String, DefId)> = self.modules[module.0 as usize]
            .types
            .iter()
            .map(|(n, d)| (n.clone(), *d))
            .collect();
        for (name, def) in declared {
            // `TargetOs`, `TargetArch` and `TargetCpu` type `@target`; they have no name.
            if matches!(name.as_str(), "TargetOs" | "TargetArch" | "TargetCpu") {
                continue;
            }
            self.modules[0].types.insert(name, def);
        }
    }

    /// The definition of a builtin name such as `Option`.
    pub fn builtin_def(&self, name: &str) -> Option<DefId> {
        self.modules[0].types.get(name).copied()
    }

    /// The qualified name `package.module.Item` of a definition, for messages.
    pub fn qualified_name(&self, id: DefId) -> String {
        let def = self.def(id);
        match def.parent {
            Some(parent) => format!("{}.{}", self.qualified_name(parent), def.name),
            None => {
                let module = self.module(def.module);
                if matches!(def.kind, DefKind::Module | DefKind::Package) {
                    def.name.clone()
                } else {
                    format!("{}.{}", module.path, def.name)
                }
            }
        }
    }
}
