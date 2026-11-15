//! Phase 9, the ownership checker (checker.md).
//!
//! Analysis A (moves and linear values, E1001–E1006) is [`moves`]. Later steps add
//! the origin, loan, scope and task analyses beside it. Phase 9 never changes the
//! CFG. The driver still records [`crate::UNCHECKED_OWNERSHIP`]: the rest of phase 9
//! and phase 10 are not done.

mod moves;

use crate::model::Cfgs;
use ingot_diag::diagnostic::Diagnostic;
use ingot_diag::source::Sources;
use ingot_sema::defs::Sema;
use ingot_sema::sigs::Declarations;

/// Run Analysis A on every CFG phase 8 produced for the checked package.
///
/// `cfgs` is that package only (std is not checked unless it is the package under
/// analysis). Constant-evaluation CFGs are a separate set and are not passed here.
pub fn check_ownership(
    sema: &Sema,
    decls: &mut Declarations,
    sources: &Sources,
    cfgs: &Cfgs,
) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    for (_, cfg) in cfgs.iter() {
        out.extend(moves::check_cfg(sema, decls, sources, cfg));
    }
    out
}
