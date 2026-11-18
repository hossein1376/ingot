//! The report commands: `explain`, `api`, `audit`, `layout` and `targets`
//! (tools.md §4). They run the front end as `check` does and print one JSON line per
//! item.

pub mod api;
pub mod audit;
pub mod explain;
pub mod layout;
pub mod targets;

use crate::pipeline::Analysis;

/// The diagnostics `check` would print for the same path (tools.md §4): the report
/// comes after them, and any diagnostic makes its summary `ok` false.
pub(crate) fn check_diagnostics(analysis: &Analysis) -> (Vec<String>, String, bool) {
    let mut lines = Vec::new();
    let mut human = String::new();
    for diagnostic in &analysis.diagnostics {
        lines.push(diagnostic.to_json_for(&analysis.sources, &analysis.target));
        human.push_str(&diagnostic.to_human(&analysis.sources));
    }
    let any = !lines.is_empty();
    (lines, human, any)
}
