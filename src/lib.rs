//! MVP Ingot compiler library.
//!
//! The pipeline implemented today follows `docs/impl/pipeline.md` through
//! phase 10: load, lex, parse, resolve, build signatures, type-check bodies,
//! lower to a CFG, check ownership, and remove proven checks. The standard
//! library source is in `std/`. A tree-walking evaluator is included so `run`
//! and `test` work before code generation exists.

pub mod ast;
pub mod cfg;
pub mod cfg_check;
pub mod check;
pub mod conformance;
pub mod cost;
pub mod diagnostic;
pub mod interp;
pub mod lexer;
pub mod loader;
pub mod parser;
pub mod source;
pub mod token;
pub mod types;

pub use diagnostic::Diagnostic;
pub use source::{Source, Sources};
