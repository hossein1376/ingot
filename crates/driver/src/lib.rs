//! The `ingot` driver: package loading, the front-end pipeline, the fix engine and
//! the conformance runner. The CLI in `main.rs` is a thin layer over these modules.

pub mod build;
pub mod cache;
pub mod commands;
pub mod conformance;
pub mod deps;
pub mod files;
pub mod fix;
pub mod fmt;
pub mod loader;
pub mod manifest;
pub mod pipeline;
pub mod query;
pub mod reports;
pub mod serve;
pub mod sha256;
pub mod test_cmd;
