//! MVP Ingot compiler library.
//!
//! The pipeline implemented today is the front end from
//! `docs/impl/pipeline.md`: load, lex, parse, resolve, build signatures, and
//! type-check bodies. A tree-walking evaluator is included so `run` and `test`
//! can exercise the front end before a code generator exists.

pub mod ast;
pub mod borrow;
pub mod check;
pub mod conformance;
pub mod cost;
pub mod diagnostic;
pub mod interp;
pub mod lexer;
pub mod ownership;
pub mod parser;
pub mod source;
pub mod token;
pub mod types;

pub use diagnostic::Diagnostic;
pub use source::{Source, Sources};
