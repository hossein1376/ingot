//! Phase 9, the ownership checker (checker.md).
//!
//! Analysis A (moves and linear values, E1001–E1006) is [`moves`]. Analysis B
//! (origin sets, E1202–E1205) is [`origins`]. Loans, scopes and phase 10 are not
//! done. Phase 9 never changes the CFG. The driver still records
//! [`crate::UNCHECKED_OWNERSHIP`].

mod moves;
mod origins;

pub use origins::FromFix;

use crate::model::Cfgs;
use ingot_diag::diagnostic::Diagnostic;
use ingot_diag::source::{Sources, Span};
use ingot_sema::defs::Sema;
use ingot_sema::sigs::Declarations;

/// Diagnostics from analyses A and B, plus `from` clauses for phase 6's E1201.
pub struct OwnershipReport {
    pub diagnostics: Vec<Diagnostic>,
    pub not_implemented: Vec<String>,
    pub from_fixes: Vec<FromFix>,
}

/// Run analyses A and B on every CFG phase 8 produced for the checked package.
///
/// `cfgs` is that package only (std is not checked unless it is the package under
/// analysis). Constant-evaluation CFGs are a separate set and are not passed here.
pub fn check_ownership(
    sema: &Sema,
    decls: &mut Declarations,
    sources: &Sources,
    cfgs: &Cfgs,
) -> OwnershipReport {
    let mut diagnostics = Vec::new();
    let mut not_implemented = Vec::new();
    let mut from_fixes = Vec::new();
    for (_, cfg) in cfgs.iter() {
        diagnostics.extend(moves::check_cfg(sema, decls, sources, cfg));
        let out = origins::check_cfg(sema, decls, sources, cfg);
        diagnostics.extend(out.diagnostics);
        not_implemented.extend(out.not_implemented);
        from_fixes.extend(out.from_fixes);
    }
    not_implemented.sort();
    not_implemented.dedup();
    OwnershipReport {
        diagnostics,
        not_implemented,
        from_fixes,
    }
}

/// Attach a phase-9 `from` clause to the phase-6 E1201 that names the same return type.
pub fn attach_from_fixes(diagnostics: &mut [Diagnostic], fixes: &[FromFix]) {
    for fix in fixes {
        let Some(diag) = diagnostics.iter_mut().find(|d| {
            d.code == "E1201" && d.file == fix.file && d.span == fix.span && d.fixes.is_empty()
        }) else {
            continue;
        };
        diag.fix(
            "add the `from` clause",
            ingot_diag::diagnostic::Edit {
                file: fix.file.clone(),
                span: Span::empty(fix.insert_at),
                text: fix.text.clone(),
            },
        );
    }
}
