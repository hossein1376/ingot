//! Semantic types and the structural properties of §10.

use crate::ast::{StructKind, Ty};
use crate::token::{ScalarTy, VectorTy};
use std::collections::{BTreeMap, HashMap, HashSet};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Type {
    Error,
    Void,
    Never,
    Bool,
    Int(ScalarTy),
    Float(ScalarTy),
    Vector(VectorTy),
    Array(i64, Box<Type>),
    Tuple(Vec<Type>),
    Struct(String, Vec<Type>),
    Enum(String, Vec<Type>),
    Optional(Box<Type>),
    Result(Box<Type>, Box<Type>),
    Borrow {
        mutable: bool,
        inner: Box<Type>,
    },
    Slice {
        mutable: bool,
        elem: Box<Type>,
    },
    Ptr {
        mutable: bool,
        inner: Box<Type>,
    },
    Fn {
        params: Vec<Type>,
        ret: Box<Type>,
        error: Option<Box<Type>>,
    },
    Dyn(String),
    Param(String),
    External(String),
}

impl Type {
    pub fn i64() -> Type {
        Type::Int(ScalarTy::I64)
    }

    pub fn u8() -> Type {
        Type::Int(ScalarTy::U8)
    }

    pub fn bool() -> Type {
        Type::Bool
    }

    pub fn void() -> Type {
        Type::Void
    }

    pub fn is_error(&self) -> bool {
        matches!(self, Type::Error)
    }

    pub fn is_integer(&self) -> bool {
        matches!(self, Type::Int(_))
    }

    pub fn is_signed_integer(&self) -> bool {
        matches!(self, Type::Int(s) if s.is_signed())
    }

    pub fn is_float(&self) -> bool {
        matches!(self, Type::Float(_))
    }

    pub fn is_numeric(&self) -> bool {
        self.is_integer() || self.is_float()
    }

    pub fn is_void(&self) -> bool {
        matches!(self, Type::Void)
    }

    pub fn is_never(&self) -> bool {
        matches!(self, Type::Never)
    }

    pub fn is_result(&self) -> bool {
        matches!(self, Type::Result(..))
    }

    pub fn is_optional(&self) -> bool {
        matches!(self, Type::Optional(_))
    }

    pub fn is_borrow_like(&self) -> bool {
        matches!(
            self,
            Type::Borrow { .. } | Type::Slice { .. } | Type::Dyn(_)
        )
    }

    pub fn strip_borrows(&self) -> &Type {
        match self {
            Type::Borrow { inner, .. } => inner.strip_borrows(),
            Type::Dyn(_) => self,
            _ => self,
        }
    }

    pub fn as_struct(&self) -> Option<&str> {
        match self {
            Type::Struct(name, _) => Some(name),
            _ => None,
        }
    }

    pub fn as_enum(&self) -> Option<&str> {
        match self {
            Type::Enum(name, _) => Some(name),
            _ => None,
        }
    }

    pub fn is_named(&self, name: &str) -> bool {
        matches!(self, Type::Struct(n, _) | Type::Enum(n, _) if n == name)
    }

    pub fn is_bool(&self) -> bool {
        matches!(self, Type::Bool)
    }

    pub fn is_vector(&self) -> bool {
        matches!(self, Type::Vector(_))
    }

    pub fn int_bits(&self) -> Option<u32> {
        match self {
            Type::Int(s) => s.int_bits(),
            _ => None,
        }
    }

    /// `Copy`: may be duplicated by assignment or passing (§10).
    pub fn is_copy(&self, defs: &TypeDefs, bounds: &HashMap<String, Vec<String>>) -> bool {
        match self {
            Type::Error | Type::Never | Type::Void | Type::Bool => true,
            Type::Int(_) | Type::Float(_) | Type::Vector(_) => true,
            Type::Array(_, e) | Type::Optional(e) => e.is_copy(defs, bounds),
            Type::Tuple(es) => es.iter().all(|t| t.is_copy(defs, bounds)),
            Type::Struct(name, args) => {
                if defs
                    .structs
                    .get(name)
                    .map(|s| s.kind == StructKind::Copy)
                    .unwrap_or(false)
                {
                    defs.structs[name].fields.iter().all(|(_, ty, _)| {
                        substitute(ty, &defs.structs[name].generics, args).is_copy(defs, bounds)
                    })
                } else {
                    false
                }
            }
            Type::Enum(name, args) => {
                let Some(def) = defs.enums.get(name) else {
                    return false;
                };
                def.copy
                    && def.variants.iter().all(|(_, p)| {
                        p.as_ref()
                            .map(|t| substitute(t, &def.generics, args).is_copy(defs, bounds))
                            .unwrap_or(true)
                    })
            }
            Type::Borrow { mutable, inner } => !*mutable && inner.is_copy(defs, bounds),
            Type::Slice { mutable, elem } => !*mutable && elem.is_copy(defs, bounds),
            Type::Ptr { .. } | Type::Fn { .. } => true,
            Type::Result(..) => false,
            Type::Dyn(_) => false,
            Type::External(name) => name == "Scope" || name.starts_with("Idx"),
            Type::Param(name) => bounds
                .get(name)
                .map(|bs| bs.iter().any(|b| b == "Copy"))
                .unwrap_or(false),
        }
    }

    /// Linear (not `Discard`): must be consumed exactly once on every path.
    pub fn is_linear(&self, defs: &TypeDefs, bounds: &HashMap<String, Vec<String>>) -> bool {
        match self {
            Type::Error | Type::Void | Type::Never | Type::Bool => false,
            Type::Int(_) | Type::Float(_) | Type::Vector(_) => false,
            Type::Array(_, e) | Type::Optional(e) => e.is_linear(defs, bounds),
            Type::Tuple(es) => es.iter().any(|t| t.is_linear(defs, bounds)),
            Type::Struct(name, args) => {
                if let Some(def) = defs.structs.get(name) {
                    def.kind == StructKind::Linear
                        || def.fields.iter().any(|(_, ty, _)| {
                            substitute(ty, &def.generics, args).is_linear(defs, bounds)
                        })
                } else {
                    false
                }
            }
            Type::Enum(name, args) => defs
                .enums
                .get(name)
                .map(|def| {
                    def.variants.iter().any(|(_, p)| {
                        p.as_ref()
                            .map(|t| substitute(t, &def.generics, args).is_linear(defs, bounds))
                            .unwrap_or(false)
                    })
                })
                .unwrap_or(false),
            Type::Borrow { .. }
            | Type::Slice { .. }
            | Type::Ptr { .. }
            | Type::Fn { .. }
            | Type::Dyn(_)
            | Type::External(_) => false,
            Type::Result(..) => true,
            // A bare `T` is not structurally linear; whether a value must be
            // consumed is decided by `is_linear_value` from its bounds.
            Type::Param(_) => false,
        }
    }

    /// The value-level linearity rule: an unbounded `T` might be linear, so a
    /// value of that type must be consumed unless `Discard` is granted.
    pub fn is_linear_value(&self, defs: &TypeDefs, bounds: &HashMap<String, Vec<String>>) -> bool {
        match self {
            Type::Param(name) => !bounds
                .get(name)
                .map(|bs| bs.iter().any(|b| b == "Discard"))
                .unwrap_or(false),
            _ => self.is_linear(defs, bounds),
        }
    }

    pub fn contains_param(&self) -> bool {
        match self {
            Type::Param(_) => true,
            Type::Array(_, e) | Type::Optional(e) => e.contains_param(),
            Type::Result(a, b) => a.contains_param() || b.contains_param(),
            Type::Tuple(es) => es.iter().any(|t| t.contains_param()),
            Type::Struct(_, args) | Type::Enum(_, args) => args.iter().any(|t| t.contains_param()),
            Type::Borrow { inner, .. } | Type::Ptr { inner, .. } => inner.contains_param(),
            Type::Slice { elem, .. } => elem.contains_param(),
            Type::Fn { params, ret, error } => {
                params.iter().any(|t| t.contains_param())
                    || ret.contains_param()
                    || error.as_ref().map(|e| e.contains_param()).unwrap_or(false)
            }
            _ => false,
        }
    }

    /// View: contains a borrow, so it is tied to the borrowed place.
    pub fn is_view(&self, defs: &TypeDefs) -> bool {
        match self {
            Type::Borrow { .. } | Type::Slice { .. } | Type::Dyn(_) => true,
            Type::Array(_, e) | Type::Optional(e) => e.is_view(defs),
            Type::Result(a, b) => a.is_view(defs) || b.is_view(defs),
            Type::Tuple(es) => es.iter().any(|t| t.is_view(defs)),
            Type::Struct(name, _) => defs
                .structs
                .get(name)
                .map(|d| d.fields.iter().any(|(_, t, _)| t.is_view(defs)))
                .unwrap_or(false),
            Type::Enum(name, _) => defs
                .enums
                .get(name)
                .map(|d| {
                    d.variants
                        .iter()
                        .any(|(_, p)| p.as_ref().map(|t| t.is_view(defs)).unwrap_or(false))
                })
                .unwrap_or(false),
            _ => false,
        }
    }

    pub fn is_owned(&self, defs: &TypeDefs) -> bool {
        !self.is_view(defs)
    }

    pub fn is_send(&self, defs: &TypeDefs) -> bool {
        match self {
            Type::Ptr { .. } => false,
            Type::External(name) => !name.starts_with("Raw"),
            Type::Borrow { inner, .. } | Type::Slice { elem: inner, .. } => inner.is_sync(defs),
            Type::Array(_, e) | Type::Optional(e) => e.is_send(defs),
            Type::Result(a, b) => a.is_send(defs) && b.is_send(defs),
            Type::Tuple(es) => es.iter().all(|t| t.is_send(defs)),
            Type::Struct(name, _) => defs
                .structs
                .get(name)
                .map(|d| d.fields.iter().all(|(_, t, _)| t.is_send(defs)))
                .unwrap_or(false),
            Type::Enum(name, _) => defs
                .enums
                .get(name)
                .map(|d| {
                    d.variants
                        .iter()
                        .all(|(_, p)| p.as_ref().map(|t| t.is_send(defs)).unwrap_or(true))
                })
                .unwrap_or(false),
            _ => true,
        }
    }

    pub fn is_sync(&self, defs: &TypeDefs) -> bool {
        match self {
            Type::Ptr { .. } => false,
            Type::External(name) => !name.starts_with("Raw") && !name.contains("Arena"),
            Type::Borrow { inner, .. } | Type::Slice { elem: inner, .. } => inner.is_sync(defs),
            Type::Array(_, e) | Type::Optional(e) => e.is_sync(defs),
            Type::Result(a, b) => a.is_sync(defs) && b.is_sync(defs),
            Type::Tuple(es) => es.iter().all(|t| t.is_sync(defs)),
            Type::Struct(name, _) => defs
                .structs
                .get(name)
                .map(|d| d.fields.iter().all(|(_, t, _)| t.is_sync(defs)))
                .unwrap_or(false),
            Type::Enum(name, _) => defs
                .enums
                .get(name)
                .map(|d| {
                    d.variants
                        .iter()
                        .all(|(_, p)| p.as_ref().map(|t| t.is_sync(defs)).unwrap_or(true))
                })
                .unwrap_or(false),
            _ => true,
        }
    }
}

impl std::fmt::Display for Type {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Type::Error => write!(f, "<error>"),
            Type::Void => write!(f, "void"),
            Type::Never => write!(f, "never"),
            Type::Bool => write!(f, "bool"),
            Type::Int(s) => write!(f, "{}", s.name()),
            Type::Float(s) => write!(f, "{}", s.name()),
            Type::Vector(v) => write!(f, "{}", v.name()),
            Type::Array(n, e) => write!(f, "[{n}]{e}"),
            Type::Tuple(es) => {
                write!(f, "(")?;
                for (i, e) in es.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{e}")?;
                }
                write!(f, ")")
            }
            Type::Struct(n, args) | Type::Enum(n, args) => {
                write!(f, "{n}")?;
                if !args.is_empty() {
                    write!(f, "[")?;
                    for (i, a) in args.iter().enumerate() {
                        if i > 0 {
                            write!(f, ", ")?;
                        }
                        write!(f, "{a}")?;
                    }
                    write!(f, "]")?;
                }
                Ok(())
            }
            Type::Optional(e) => write!(f, "?{e}"),
            Type::Result(t, e) => write!(f, "{t} ! {e}"),
            Type::Borrow { mutable, inner } => {
                write!(f, "&{}{}", if *mutable { "mut " } else { "" }, inner)
            }
            Type::Slice { mutable, elem } => {
                write!(f, "[]{}{}", if *mutable { "mut " } else { "" }, elem)
            }
            Type::Ptr { mutable, inner } => {
                write!(f, "*{}{}", if *mutable { "mut " } else { "" }, inner)
            }
            Type::Fn { params, ret, error } => {
                write!(f, "fn(")?;
                for (i, p) in params.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{p}")?;
                }
                write!(f, ") -> {ret}")?;
                if let Some(e) = error {
                    write!(f, " ! {e}")?;
                }
                Ok(())
            }
            Type::Dyn(i) => write!(f, "dyn {i}"),
            Type::Param(n) => write!(f, "{n}"),
            Type::External(n) => write!(f, "{n}"),
        }
    }
}

#[derive(Clone, Debug)]
pub struct StructDef {
    pub kind: StructKind,
    pub fields: Vec<(String, Type, bool)>,
    pub generics: Vec<String>,
    pub span: crate::source::Span,
    pub file: String,
}

#[derive(Clone, Debug)]
pub struct EnumDef {
    pub copy: bool,
    pub variants: Vec<(String, Option<Type>)>,
    pub generics: Vec<String>,
    pub from: HashMap<String, String>,
    pub tag: Option<Type>,
    pub span: crate::source::Span,
    pub file: String,
}

#[derive(Clone, Debug, Default)]
pub struct TypeDefs {
    pub structs: BTreeMap<String, StructDef>,
    pub enums: BTreeMap<String, EnumDef>,
    pub aliases: BTreeMap<String, (Ty, Vec<String>)>,
    pub external: HashSet<String>,
}

impl TypeDefs {
    pub fn field(&self, ty: &Type, name: &str) -> Option<Type> {
        match ty {
            Type::Struct(n, args) => {
                let def = self.structs.get(n)?;
                let (_, fty, _) = def.fields.iter().find(|(f, _, _)| f == name)?;
                Some(substitute(fty, &def.generics, args))
            }
            Type::Tuple(es) => name.parse::<usize>().ok().and_then(|i| es.get(i).cloned()),
            Type::Slice { elem, .. } if name == "len" => Some(Type::i64()),
            Type::Slice { mutable, elem } if name == "ptr" => Some(Type::Ptr {
                mutable: *mutable,
                inner: elem.clone(),
            }),
            _ => None,
        }
    }

    pub fn variant(&self, ty: &Type, name: &str) -> Option<Option<Type>> {
        let Type::Enum(n, args) = ty else {
            return None;
        };
        let def = self.enums.get(n)?;
        let (_, payload) = def.variants.iter().find(|(v, _)| v == name)?;
        Some(payload.as_ref().map(|t| substitute(t, &def.generics, args)))
    }

    pub fn variant_index(&self, ty: &Type, name: &str) -> Option<usize> {
        let Type::Enum(n, _) = ty else {
            return None;
        };
        self.enums
            .get(n)?
            .variants
            .iter()
            .position(|(v, _)| v == name)
    }
}

pub fn substitute(ty: &Type, params: &[String], args: &[Type]) -> Type {
    if params.is_empty() || args.is_empty() {
        return ty.clone();
    }
    let map: HashMap<&str, &Type> = params.iter().map(|s| s.as_str()).zip(args.iter()).collect();
    substitute_map(ty, &map)
}

fn substitute_map(ty: &Type, map: &HashMap<&str, &Type>) -> Type {
    match ty {
        Type::Param(n) => map
            .get(n.as_str())
            .map(|t| (*t).clone())
            .unwrap_or_else(|| ty.clone()),
        Type::Array(n, e) => Type::Array(*n, Box::new(substitute_map(e, map))),
        Type::Tuple(es) => Type::Tuple(es.iter().map(|t| substitute_map(t, map)).collect()),
        Type::Struct(n, args) => Type::Struct(
            n.clone(),
            args.iter().map(|t| substitute_map(t, map)).collect(),
        ),
        Type::Enum(n, args) => Type::Enum(
            n.clone(),
            args.iter().map(|t| substitute_map(t, map)).collect(),
        ),
        Type::Optional(e) => Type::Optional(Box::new(substitute_map(e, map))),
        Type::Result(a, b) => Type::Result(
            Box::new(substitute_map(a, map)),
            Box::new(substitute_map(b, map)),
        ),
        Type::Borrow { mutable, inner } => Type::Borrow {
            mutable: *mutable,
            inner: Box::new(substitute_map(inner, map)),
        },
        Type::Slice { mutable, elem } => Type::Slice {
            mutable: *mutable,
            elem: Box::new(substitute_map(elem, map)),
        },
        Type::Ptr { mutable, inner } => Type::Ptr {
            mutable: *mutable,
            inner: Box::new(substitute_map(inner, map)),
        },
        Type::Fn { params, ret, error } => Type::Fn {
            params: params.iter().map(|t| substitute_map(t, map)).collect(),
            ret: Box::new(substitute_map(ret, map)),
            error: error.as_ref().map(|e| Box::new(substitute_map(e, map))),
        },
        _ => ty.clone(),
    }
}
