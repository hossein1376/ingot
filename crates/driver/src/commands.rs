//! Command output shared by the one-shot CLI and `ingot serve` (tools.md §1 and §6).
//!
//! Nothing here prints: the caller writes the lines. Both entry points call the same
//! function, so their lines are byte-identical; `serve` only adds the request `id` and
//! the summary's `exit`.
//!
//! Human (non-`--json`) text is built here too, but the CLI decides when to write it.

use crate::files::{Files, Overlays};
use crate::pipeline::{self, Analysis};
use ingot_diag::diagnostic::{Diagnostic, json_string};
use ingot_diag::source::{Sources, Span};
use ingot_sema::Target;
use std::path::Path;
use std::time::Instant;

/// What a command produced: the JSON lines (summary last), the human text, and the
/// exit status. `error` is the message a failure would print on stderr without
/// `--json`.
#[derive(Clone, Debug, Default)]
pub struct Output {
    pub lines: Vec<String>,
    pub human: String,
    pub error: Option<String>,
    pub not_implemented: Vec<String>,
    pub exit: u8,
}

/// `ingot check PATH [--target T]` as lines and a status, with overlays.
pub fn check(path: &Path, target: Option<&str>, overlays: &Overlays) -> Output {
    let started = Instant::now();
    let files = Files::new(overlays.clone());
    if !files.is_dir(path) && !files.exists(path) {
        return failure(
            "check",
            format!("{} does not exist", path.display()),
            started,
        );
    }
    let mut options = pipeline::Options::default();
    options.load.overlays = overlays.clone();
    if let Some(name) = target {
        if Target::by_name(name).is_none() {
            return unknown_target(name, started);
        }
        options.target = name.to_string();
    }
    let analysis = match pipeline::analyze_path_with(path, &options) {
        Ok(analysis) => analysis,
        Err(error) => return failure("check", error.to_string(), started),
    };
    let ok = analysis.accepted();
    let mut lines = Vec::new();
    for diagnostic in &analysis.diagnostics {
        lines.push(diagnostic.to_json_for(&analysis.sources, &analysis.target));
    }
    lines.push(check_summary(&analysis, ok, started));
    let mut human = String::new();
    for diagnostic in &analysis.diagnostics {
        human.push_str(&diagnostic.to_human(&analysis.sources));
    }
    let exit = if !analysis.diagnostics.is_empty() {
        1
    } else if ok {
        0
    } else {
        2
    };
    Output {
        lines,
        human,
        error: None,
        not_implemented: analysis.not_implemented.clone(),
        exit,
    }
}

fn check_summary(analysis: &Analysis, ok: bool, started: Instant) -> String {
    let target = analysis.target.as_str();
    let not_implemented = if analysis.not_implemented.is_empty() {
        String::new()
    } else {
        let mut list = String::new();
        for (i, what) in analysis.not_implemented.iter().enumerate() {
            if i > 0 {
                list.push(',');
            }
            json_string(&mut list, what);
        }
        format!(",\"not_implemented\":[{list}]")
    };
    format!(
        "{{\"type\":\"summary\",\"command\":\"check\",\"ok\":{ok},\"protocol\":1,\"errors\":{},\"files\":{},\"targets\":[\"{target}\"],\"ms\":{}{not_implemented}}}",
        analysis.diagnostics.len(),
        analysis.files,
        started.elapsed().as_millis()
    )
}

/// E1702 for a `--target` that is not one of the three. The name has no file, so the
/// diagnostic points into the command line.
fn unknown_target(name: &str, started: Instant) -> Output {
    let text = format!("--target {name}");
    let source = pipeline::command_line_source(&text);
    let span = Span::new("--target ".len(), text.len());
    let diagnostic = Diagnostic::new(
        "E1702",
        format!(
            "`{name}` is not a target; the targets are {}",
            ingot_sema::target::TARGETS.join(", ")
        ),
        &source,
        span,
    );
    let mut sources = Sources::default();
    sources.add(source);
    let lines = vec![
        diagnostic.to_json(&sources),
        format!(
            "{{\"type\":\"summary\",\"command\":\"check\",\"ok\":false,\"protocol\":1,\"errors\":1,\"files\":0,\"targets\":[],\"ms\":{}}}",
            started.elapsed().as_millis()
        ),
    ];
    Output {
        lines,
        human: diagnostic.to_human(&sources),
        error: None,
        not_implemented: Vec::new(),
        exit: 1,
    }
}

/// A command that stopped before doing its work (tools.md §1 "Failures").
pub fn failure(command: &str, message: String, started: Instant) -> Output {
    let lines = failure_lines(command, &message, started);
    Output {
        lines,
        human: String::new(),
        error: Some(message),
        not_implemented: Vec::new(),
        exit: 2,
    }
}

/// The `error` line and the summary of a failure.
pub fn failure_lines(command: &str, message: &str, started: Instant) -> Vec<String> {
    vec![error_line(message), failure_summary(command, started)]
}

pub fn error_line(message: &str) -> String {
    let mut out = String::from("{\"type\":\"error\",\"message\":");
    json_string(&mut out, message);
    out.push('}');
    out
}

pub fn failure_summary(command: &str, started: Instant) -> String {
    let mut name = String::new();
    json_string(&mut name, command);
    format!(
        "{{\"type\":\"summary\",\"command\":{name},\"ok\":false,\"protocol\":1,\"ms\":{}}}",
        started.elapsed().as_millis()
    )
}
