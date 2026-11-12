//! Tokens produced by the lexer.

use crate::source::Span;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kw {
    Align,
    As,
    Bench,
    Break,
    Catch,
    Const,
    Continue,
    Copy,
    Defer,
    Dispatch,
    Dyn,
    Else,
    Enum,
    Errdefer,
    Export,
    Extern,
    Fail,
    False,
    Fn,
    For,
    From,
    If,
    Impl,
    Import,
    In,
    Inline,
    Interface,
    Into,
    Kernel,
    Let,
    Linear,
    Loop,
    Match,
    Mut,
    Noinline,
    Orelse,
    Par,
    Pub,
    Return,
    Scope,
    Select,
    Spawn,
    Static,
    Struct,
    Test,
    True,
    Try,
    Type,
    Unsafe,
    Var,
    Waitable,
    When,
    While,
}

impl Kw {
    pub fn as_str(self) -> &'static str {
        match self {
            Kw::Align => "align",
            Kw::As => "as",
            Kw::Bench => "bench",
            Kw::Break => "break",
            Kw::Catch => "catch",
            Kw::Const => "const",
            Kw::Continue => "continue",
            Kw::Copy => "copy",
            Kw::Defer => "defer",
            Kw::Dispatch => "dispatch",
            Kw::Dyn => "dyn",
            Kw::Else => "else",
            Kw::Enum => "enum",
            Kw::Errdefer => "errdefer",
            Kw::Export => "export",
            Kw::Extern => "extern",
            Kw::Fail => "fail",
            Kw::False => "false",
            Kw::Fn => "fn",
            Kw::For => "for",
            Kw::From => "from",
            Kw::If => "if",
            Kw::Impl => "impl",
            Kw::Import => "import",
            Kw::In => "in",
            Kw::Inline => "inline",
            Kw::Interface => "interface",
            Kw::Into => "into",
            Kw::Kernel => "kernel",
            Kw::Let => "let",
            Kw::Linear => "linear",
            Kw::Loop => "loop",
            Kw::Match => "match",
            Kw::Mut => "mut",
            Kw::Noinline => "noinline",
            Kw::Orelse => "orelse",
            Kw::Par => "par",
            Kw::Pub => "pub",
            Kw::Return => "return",
            Kw::Scope => "scope",
            Kw::Select => "select",
            Kw::Spawn => "spawn",
            Kw::Static => "static",
            Kw::Struct => "struct",
            Kw::Test => "test",
            Kw::True => "true",
            Kw::Try => "try",
            Kw::Type => "type",
            Kw::Unsafe => "unsafe",
            Kw::Var => "var",
            Kw::Waitable => "waitable",
            Kw::When => "when",
            Kw::While => "while",
        }
    }

    pub fn from_str(s: &str) -> Option<Kw> {
        Some(match s {
            "align" => Kw::Align,
            "as" => Kw::As,
            "bench" => Kw::Bench,
            "break" => Kw::Break,
            "catch" => Kw::Catch,
            "const" => Kw::Const,
            "continue" => Kw::Continue,
            "copy" => Kw::Copy,
            "defer" => Kw::Defer,
            "dispatch" => Kw::Dispatch,
            "dyn" => Kw::Dyn,
            "else" => Kw::Else,
            "enum" => Kw::Enum,
            "errdefer" => Kw::Errdefer,
            "export" => Kw::Export,
            "extern" => Kw::Extern,
            "fail" => Kw::Fail,
            "false" => Kw::False,
            "fn" => Kw::Fn,
            "for" => Kw::For,
            "from" => Kw::From,
            "if" => Kw::If,
            "impl" => Kw::Impl,
            "import" => Kw::Import,
            "in" => Kw::In,
            "inline" => Kw::Inline,
            "interface" => Kw::Interface,
            "into" => Kw::Into,
            "kernel" => Kw::Kernel,
            "let" => Kw::Let,
            "linear" => Kw::Linear,
            "loop" => Kw::Loop,
            "match" => Kw::Match,
            "mut" => Kw::Mut,
            "noinline" => Kw::Noinline,
            "orelse" => Kw::Orelse,
            "par" => Kw::Par,
            "pub" => Kw::Pub,
            "return" => Kw::Return,
            "scope" => Kw::Scope,
            "select" => Kw::Select,
            "spawn" => Kw::Spawn,
            "static" => Kw::Static,
            "struct" => Kw::Struct,
            "test" => Kw::Test,
            "true" => Kw::True,
            "try" => Kw::Try,
            "type" => Kw::Type,
            "unsafe" => Kw::Unsafe,
            "var" => Kw::Var,
            "waitable" => Kw::Waitable,
            "when" => Kw::When,
            "while" => Kw::While,
            _ => return None,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Punct {
    LParen,
    RParen,
    LBrace,
    RBrace,
    LBracket,
    RBracket,
    Comma,
    Semicolon,
    Colon,
    Dot,
    DotDot,
    DotDotEq,
    DotStar,
    DotLBrace,
    DotLBracket,
    Arrow,
    FatArrow,
    Amp,
    AmpAmp,
    Pipe,
    PipePipe,
    Caret,
    Tilde,
    Bang,
    Eq,
    EqEq,
    BangEq,
    Lt,
    LtEq,
    Gt,
    GtEq,
    Shl,
    Shr,
    ShlEq,
    ShrEq,
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    PlusWrap,
    MinusWrap,
    StarWrap,
    PlusSat,
    MinusSat,
    StarSat,
    PlusEq,
    MinusEq,
    StarEq,
    SlashEq,
    PercentEq,
    PlusWrapEq,
    MinusWrapEq,
    StarWrapEq,
    PlusSatEq,
    MinusSatEq,
    StarSatEq,
    AmpEq,
    PipeEq,
    CaretEq,
    Question,
    PlusPlus,
}

impl Punct {
    pub fn as_str(self) -> &'static str {
        match self {
            Punct::LParen => "(",
            Punct::RParen => ")",
            Punct::LBrace => "{",
            Punct::RBrace => "}",
            Punct::LBracket => "[",
            Punct::RBracket => "]",
            Punct::Comma => ",",
            Punct::Semicolon => ";",
            Punct::Colon => ":",
            Punct::Dot => ".",
            Punct::DotDot => "..",
            Punct::DotDotEq => "..=",
            Punct::DotStar => ".*",
            Punct::DotLBrace => ".{",
            Punct::DotLBracket => ".[",
            Punct::Arrow => "->",
            Punct::FatArrow => "=>",
            Punct::Amp => "&",
            Punct::AmpAmp => "&&",
            Punct::Pipe => "|",
            Punct::PipePipe => "||",
            Punct::Caret => "^",
            Punct::Tilde => "~",
            Punct::Bang => "!",
            Punct::Eq => "=",
            Punct::EqEq => "==",
            Punct::BangEq => "!=",
            Punct::Lt => "<",
            Punct::LtEq => "<=",
            Punct::Gt => ">",
            Punct::GtEq => ">=",
            Punct::Shl => "<<",
            Punct::Shr => ">>",
            Punct::ShlEq => "<<=",
            Punct::ShrEq => ">>=",
            Punct::Plus => "+",
            Punct::Minus => "-",
            Punct::Star => "*",
            Punct::Slash => "/",
            Punct::Percent => "%",
            Punct::PlusWrap => "+%",
            Punct::MinusWrap => "-%",
            Punct::StarWrap => "*%",
            Punct::PlusSat => "+|",
            Punct::MinusSat => "-|",
            Punct::StarSat => "*|",
            Punct::PlusEq => "+=",
            Punct::MinusEq => "-=",
            Punct::StarEq => "*=",
            Punct::SlashEq => "/=",
            Punct::PercentEq => "%=",
            Punct::PlusWrapEq => "+%=",
            Punct::MinusWrapEq => "-%=",
            Punct::StarWrapEq => "*%=",
            Punct::PlusSatEq => "+|=",
            Punct::MinusSatEq => "-|=",
            Punct::StarSatEq => "*|=",
            Punct::AmpEq => "&=",
            Punct::PipeEq => "|=",
            Punct::CaretEq => "^=",
            Punct::Question => "?",
            Punct::PlusPlus => "++",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ScalarTy {
    I8,
    I16,
    I32,
    I64,
    U8,
    U16,
    U32,
    U64,
    F32,
    F64,
    Bool,
    Void,
    Never,
}

impl ScalarTy {
    pub fn from_name(s: &str) -> Option<ScalarTy> {
        Some(match s {
            "i8" => ScalarTy::I8,
            "i16" => ScalarTy::I16,
            "i32" => ScalarTy::I32,
            "i64" => ScalarTy::I64,
            "u8" => ScalarTy::U8,
            "u16" => ScalarTy::U16,
            "u32" => ScalarTy::U32,
            "u64" => ScalarTy::U64,
            "f32" => ScalarTy::F32,
            "f64" => ScalarTy::F64,
            "bool" => ScalarTy::Bool,
            "void" => ScalarTy::Void,
            "never" => ScalarTy::Never,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            ScalarTy::I8 => "i8",
            ScalarTy::I16 => "i16",
            ScalarTy::I32 => "i32",
            ScalarTy::I64 => "i64",
            ScalarTy::U8 => "u8",
            ScalarTy::U16 => "u16",
            ScalarTy::U32 => "u32",
            ScalarTy::U64 => "u64",
            ScalarTy::F32 => "f32",
            ScalarTy::F64 => "f64",
            ScalarTy::Bool => "bool",
            ScalarTy::Void => "void",
            ScalarTy::Never => "never",
        }
    }

    pub fn int_bits(self) -> Option<u32> {
        Some(match self {
            ScalarTy::I8 | ScalarTy::U8 => 8,
            ScalarTy::I16 | ScalarTy::U16 => 16,
            ScalarTy::I32 | ScalarTy::U32 => 32,
            ScalarTy::I64 | ScalarTy::U64 => 64,
            _ => return None,
        })
    }

    pub fn is_signed(self) -> bool {
        matches!(
            self,
            ScalarTy::I8 | ScalarTy::I16 | ScalarTy::I32 | ScalarTy::I64
        )
    }

    pub fn is_float(self) -> bool {
        matches!(self, ScalarTy::F32 | ScalarTy::F64)
    }

    pub fn is_integer(self) -> bool {
        self.int_bits().is_some()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct VectorTy {
    pub elem: ScalarTy,
    pub lanes: u16,
}

impl VectorTy {
    pub fn name(self) -> String {
        format!("{}x{}", self.elem.name(), self.lanes)
    }

    pub fn bits(self) -> u32 {
        self.elem
            .int_bits()
            .unwrap_or(if self.elem == ScalarTy::Bool { 1 } else { 32 })
            * self.lanes as u32
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum TokenKind {
    Ident(String),
    TypeIdent(String),
    Int(u128),
    Float(f64),
    Str(Vec<u8>),
    CStr(Vec<u8>),
    Builtin(String),
    Keyword(Kw),
    Scalar(ScalarTy),
    Vector(VectorTy),
    Punct(Punct),
    Eof,
}

impl TokenKind {
    pub fn is_punct(&self, p: Punct) -> bool {
        matches!(self, TokenKind::Punct(q) if *q == p)
    }

    pub fn is_kw(&self, k: Kw) -> bool {
        matches!(self, TokenKind::Keyword(q) if *q == k)
    }

    pub fn describe(&self) -> String {
        match self {
            TokenKind::Ident(s) => format!("identifier `{s}`"),
            TokenKind::TypeIdent(s) => format!("type identifier `{s}`"),
            TokenKind::Int(n) => format!("integer `{n}`"),
            TokenKind::Float(n) => format!("float `{n}`"),
            TokenKind::Str(_) => "string literal".to_string(),
            TokenKind::CStr(_) => "C string literal".to_string(),
            TokenKind::Builtin(s) => format!("builtin `@{s}`"),
            TokenKind::Keyword(k) => format!("keyword `{}`", k.as_str()),
            TokenKind::Scalar(s) => format!("type `{}`", s.name()),
            TokenKind::Vector(v) => format!("vector type `{}`", v.name()),
            TokenKind::Punct(p) => format!("`{}`", p.as_str()),
            TokenKind::Eof => "end of file".to_string(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
}

impl Token {
    pub fn new(kind: TokenKind, span: Span) -> Self {
        Token { kind, span }
    }
}
