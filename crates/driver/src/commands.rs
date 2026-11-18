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

/// `ingot check PATH [--target T | --all-targets]` as lines and a status, with
/// overlays (tools.md §2).
pub fn check(path: &Path, target: Option<&str>, all_targets: bool, overlays: &Overlays) -> Output {
    let started = Instant::now();
    let files = Files::new(overlays.clone());
    if !files.is_dir(path) && !files.exists(path) {
        return failure(
            "check",
            format!("{} does not exist", path.display()),
            started,
        );
    }
    if !all_targets {
        let mut options = pipeline::Options::default();
        options.load.overlays = overlays.clone();
        options.load.cache = crate::cache::default_cache();
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
        return single_check_output(analysis, started);
    }
    let targets = match root_targets(path, overlays) {
        Ok(targets) => targets,
        Err(error) => return failure("check", error.to_string(), started),
    };
    let mut merged: Vec<(Diagnostic, Vec<String>)> = Vec::new();
    let mut all_sources = Sources::default();
    let mut files_count = 0usize;
    let mut not_implemented: Vec<String> = Vec::new();
    let mut accepted = true;
    for name in &targets {
        let mut options = pipeline::Options::default();
        options.load.overlays = overlays.clone();
        options.load.cache = crate::cache::default_cache();
        options.target = name.clone();
        let analysis = match pipeline::analyze_path_with(path, &options) {
            Ok(analysis) => analysis,
            Err(error) => return failure("check", error.to_string(), started),
        };
        files_count = analysis.files;
        accepted &= analysis.accepted();
        for what in &analysis.not_implemented {
            if !not_implemented.contains(what) {
                not_implemented.push(what.clone());
            }
        }
        if all_sources.files.is_empty() {
            all_sources = analysis.sources.clone();
        }
        for diagnostic in &analysis.diagnostics {
            let key = format!("{diagnostic:?}");
            if let Some((_, names)) = merged.iter_mut().find(|(d, _)| format!("{d:?}") == key) {
                if !names.contains(name) {
                    names.push(name.clone());
                }
            } else {
                merged.push((diagnostic.clone(), vec![name.clone()]));
            }
        }
    }
    merged.sort_by(|(a, _), (b, _)| {
        (a.file.as_str(), a.span.start, a.span.end, a.code).cmp(&(
            b.file.as_str(),
            b.span.start,
            b.span.end,
            b.code,
        ))
    });
    let diagnostics: Vec<&Diagnostic> = merged.iter().map(|(d, _)| d).collect();
    let mut lines = Vec::new();
    for (diagnostic, names) in &merged {
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        lines.push(diagnostic.to_json_for_targets(&all_sources, &names));
    }
    let target_list: Vec<&str> = targets.iter().map(String::as_str).collect();
    let target_json = {
        let mut out = String::new();
        for (i, name) in target_list.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            json_string(&mut out, name);
        }
        out
    };
    let not_implemented_json = if not_implemented.is_empty() {
        String::new()
    } else {
        let mut list = String::new();
        for (i, what) in not_implemented.iter().enumerate() {
            if i > 0 {
                list.push(',');
            }
            json_string(&mut list, what);
        }
        format!(",\"not_implemented\":[{list}]")
    };
    let ok = accepted && diagnostics.is_empty();
    lines.push(format!(
        "{{\"type\":\"summary\",\"command\":\"check\",\"ok\":{ok},\"protocol\":1,\"errors\":{},\"files\":{files_count},\"targets\":[{target_json}],\"ms\":{}{}}}",
        diagnostics.len(),
        started.elapsed().as_millis(),
        not_implemented_json
    ));
    let mut human = String::new();
    for diagnostic in &diagnostics {
        human.push_str(&diagnostic.to_human(&all_sources));
    }
    let exit = if !diagnostics.is_empty() {
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
        not_implemented,
        exit,
    }
}

fn single_check_output(analysis: Analysis, started: Instant) -> Output {
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

/// The root's `targets` list (`--all-targets`): its manifest's, or all three.
fn root_targets(path: &Path, overlays: &Overlays) -> std::io::Result<Vec<String>> {
    let mut load = crate::loader::LoadOptions {
        overlays: overlays.clone(),
        ..Default::default()
    };
    load.target = None;
    let graph = crate::loader::load_graph(path, load)?;
    let manifest = graph
        .root
        .and_then(|index| graph.packages[index].manifest.as_ref())
        .or(graph.root_manifest.as_ref());
    if let Some(manifest) = manifest
        && manifest.targets_written
    {
        return Ok(manifest.targets.clone());
    }
    Ok(ingot_sema::target::TARGETS
        .iter()
        .map(|name| (*name).to_string())
        .collect())
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

/// `ingot cost [PATH] [--fn NAME]` as lines and a status.
pub fn cost(path: &Path, function: Option<&str>, overlays: &Overlays) -> Output {
    let started = Instant::now();
    let files = Files::new(overlays.clone());
    if !files.is_dir(path) && !files.exists(path) {
        return failure(
            "cost",
            format!("{} does not exist", path.display()),
            started,
        );
    }
    let mut options = pipeline::Options::default();
    options.load.overlays = overlays.clone();
    options.load.cache = crate::cache::default_cache();
    let analysis = match pipeline::analyze_path_with(path, &options) {
        Ok(analysis) => analysis,
        Err(error) => return failure("cost", error.to_string(), started),
    };
    let mut lines = Vec::new();
    let mut human = String::new();
    for diagnostic in &analysis.diagnostics {
        lines.push(diagnostic.to_json_for(&analysis.sources, &analysis.target));
        human.push_str(&diagnostic.to_human(&analysis.sources));
    }
    let mut shown = 0usize;
    for item in &analysis.cost {
        if let Some(name) = function
            && item.fn_name != name
        {
            continue;
        }
        let file = analysis
            .sources
            .files
            .get(item.file)
            .map(|s| s.path.as_str())
            .unwrap_or("");
        if let Some(only) = &analysis.only_file
            && file != only
        {
            continue;
        }
        shown += 1;
        let mut line = String::from("{\"type\":\"cost\",\"kind\":");
        json_string(&mut line, item.kind.name());
        line.push_str(",\"fn\":");
        json_string(&mut line, &item.fn_name);
        line.push_str(",\"span\":");
        ingot_diag::diagnostic::write_span(&mut line, &analysis.sources, file, item.span);
        if let Some(through) = item.through {
            line.push_str(",\"through\":");
            json_string(&mut line, through);
        }
        if let Some(bytes) = item.bytes {
            line.push_str(&format!(",\"bytes\":{bytes}"));
        }
        if let Some(allocator) = &item.allocator {
            line.push_str(",\"allocator\":");
            json_string(&mut line, allocator);
        }
        line.push('}');
        lines.push(line);
        let pos = analysis
            .sources
            .files
            .get(item.file)
            .map(|source| source.position(item.span.start));
        match pos {
            Some(pos) => human.push_str(&format!(
                "{} {}:{}:{}\n",
                item.kind.name(),
                file,
                pos.line,
                pos.col
            )),
            None => human.push_str(&format!("{} {}\n", item.kind.name(), item.fn_name)),
        }
    }
    let mut not_implemented = analysis.not_implemented.clone();
    not_implemented.sort();
    not_implemented.dedup();
    let mut list = String::new();
    for (i, what) in not_implemented.iter().enumerate() {
        if i > 0 {
            list.push(',');
        }
        json_string(&mut list, what);
    }
    let ok = analysis.diagnostics.is_empty() && not_implemented.is_empty();
    lines.push(format!(
        "{{\"type\":\"summary\",\"command\":\"cost\",\"ok\":{ok},\"protocol\":1,\"errors\":{},\"checks\":{},\"not_implemented\":[{}],\"ms\":{}}}",
        analysis.diagnostics.len(),
        shown,
        list,
        started.elapsed().as_millis()
    ));
    let exit = if !analysis.diagnostics.is_empty() {
        1
    } else {
        0
    };
    Output {
        lines,
        human,
        error: None,
        not_implemented,
        exit,
    }
}
