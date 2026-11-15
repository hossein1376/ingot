//! `ingot-cfg`: pipeline phase 8, lowering the typed bodies of a package to the
//! control-flow graph of `docs/impl/pipeline.md` section 5, and phase 9's first
//! analysis.
//!
//! [`lower_package`] takes the result of phase 7 ([`ingot_sema::typed::Bodies`]) and
//! produces one [`Cfg`] per function and test body, plus one for each closure body and
//! each `par for` body. [`dump::dump`] prints a CFG as deterministic text; the format
//! is described at the top of `dump.rs`. [`validate`] checks the structural rules every
//! CFG must satisfy. [`check`] is phase 9: Analysis A (moves and linear values) lives
//! in `check::moves`, Analysis B (origin sets) in `check::origins`. Loans, scopes and
//! phase 10 are not implemented; a body that reached phase 8 still contributes
//! [`UNCHECKED_OWNERSHIP`].
//!
//! The lowering reports no diagnostics. A body that phase 7 rejected is not lowered
//! (the phase gate of pipeline section 3). A construct the lowering does not handle
//! yet is recorded in [`Lowered::not_implemented`] as `lowering: <what>`, and that
//! body's CFG is dropped.

pub mod check;
pub mod dump;
pub mod eval;
mod lower;
pub mod model;
pub mod validate;

pub use lower::{Lowered, lower_for_eval, lower_package};
pub use model::*;

/// What is left after phase 8 for a body that passed phases 7 and 8: phases 9 and 10
/// do not exist yet. This replaces the phase-7 message once the body has a CFG.
pub const UNCHECKED_OWNERSHIP: &str =
    "ownership checking and check removal (pipeline phases 9 and 10)";
