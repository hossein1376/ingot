//! The `ingot` driver: package loading, the front-end pipeline, the fix engine and
//! the conformance runner. The CLI in `main.rs` is a thin layer over these modules.

pub mod conformance;
pub mod fix;
pub mod loader;
pub mod manifest;
pub mod pipeline;
