//! The front-end pipeline (`docs/impl/pipeline.md`), as far as it is implemented.
//!
//! Phases 1 to 3 run for every file of a package: load, lex, parse. The parser's
//! operator-mixing rule (E0204) is part of phase 3 and runs with it. Nothing after
//! parsing is checked yet, so the diagnostics are exactly those of these phases,
//! sorted as `docs/impl/diagnostics.md` §1 requires.

use crate::loader::{self, LoadedPackage};
use ingot_diag::diagnostic::{Diagnostic, sort_diagnostics};
use ingot_diag::source::Sources;
use ingot_syntax::lexer::lex;
use ingot_syntax::parser::{check_operator_mixing, parse};
use std::io;
use std::path::Path;

#[derive(Clone, Debug, Default)]
pub struct Analysis {
    pub sources: Sources,
    pub diagnostics: Vec<Diagnostic>,
}

/// Lex and parse every file of a loaded package and collect the diagnostics.
pub fn analyze(package: LoadedPackage) -> Analysis {
    let mut diagnostics = package.diagnostics;
    for module in &package.modules {
        let source = &package.sources.files[module.source_id];
        let (tokens, mut found) = lex(source);
        let (ast, parse_diags) = parse(source, tokens);
        found.extend(parse_diags);
        found.extend(check_operator_mixing(&ast, source));
        diagnostics.extend(found);
    }
    sort_diagnostics(&mut diagnostics);
    Analysis {
        sources: package.sources,
        diagnostics,
    }
}

/// Load the package at `path` (a directory, or a file inside a package) and analyze it.
pub fn analyze_path(path: &Path) -> io::Result<Analysis> {
    Ok(analyze(loader::load(path)?))
}
