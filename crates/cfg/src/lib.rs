//! `ingot-cfg`: pipeline phase 8, lowering the typed bodies of a package to the
//! control-flow graph of `docs/impl/pipeline.md` section 5, and phase 9.
//!
//! [`lower_package`] takes the result of phase 7 ([`ingot_sema::typed::Bodies`]) and
//! produces one [`Cfg`] per function and test body, plus one for each closure body and
//! each `par for` body. [`dump::dump`] prints a CFG as deterministic text; the format
//! is described at the top of `dump.rs`. [`validate`] checks the structural rules every
//! CFG must satisfy. [`check`] is phase 9: Analysis A (moves and linear values) lives
//! in `check::moves`, Analysis B (origin sets) in `check::origins`, and Analysis C
//! (loans, E1101–E1105 and E1403) in `check::loans`. Task rules (E1401, E1402 and
//! E1407) live in `check::tasks`. Phase 10 is not implemented; a body that reached
//! phase 8 still contributes [`UNCHECKED_CHECK_REMOVAL`].
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

/// What phase 10 still does not check, once a body has a CFG. Phase 9 records its
/// own diagnostics and does not add an entry here.
pub const UNCHECKED_CHECK_REMOVAL: &str = "check removal and cost items (pipeline phase 10)";
