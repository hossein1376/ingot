use ingot_diag::diagnostic::{Diagnostic, json_string};
use ingot_diag::source::{Sources, Span};
use ingot_driver::conformance;
use ingot_driver::fix;
use ingot_driver::pipeline;
use ingot_sema::Target;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Instant;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = match run(args) {
        Ok(code) => code,
        Err(message) => {
            eprintln!("ingot: {message}");
            2
        }
    };
    ExitCode::from(code)
}

fn run(args: Vec<String>) -> Result<u8, String> {
    let Some(command) = args.first().map(String::as_str) else {
        return Err("usage: ingot <check|fix|conformance> [options] [path]".to_string());
    };
    let json = has_flag(&args, "--json");
    match command {
        "check" => {
            if has_flag(&args, "--all-targets") {
                return Err("not implemented: `check --all-targets`".to_string());
            }
            let path = path_arg(&args, &["--target"])?;
            let started = Instant::now();
            let mut options = pipeline::Options::default();
            if let Some(name) = flag_value(&args, "--target")? {
                if Target::by_name(&name).is_none() {
                    return Ok(unknown_target(&name, json, started));
                }
                options.target = name;
            }
            let analysis =
                pipeline::analyze_path_with(&path, &options).map_err(|e| e.to_string())?;
            let ok = analysis.accepted();
            let target = analysis.target.as_str();
            if json {
                for d in &analysis.diagnostics {
                    println!("{}", d.to_json_for(&analysis.sources, target));
                }
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
                println!(
                    "{{\"type\":\"summary\",\"command\":\"check\",\"ok\":{ok},\"protocol\":1,\"errors\":{},\"files\":{},\"targets\":[\"{target}\"],\"ms\":{}{not_implemented}}}",
                    analysis.diagnostics.len(),
                    analysis.files,
                    started.elapsed().as_millis()
                );
            } else {
                for d in &analysis.diagnostics {
                    print!("{}", d.to_human(&analysis.sources));
                }
                if ok {
                    eprintln!("ok: no diagnostics");
                }
            }
            for what in &analysis.not_implemented {
                eprintln!("ingot: not implemented: {what}");
            }
            // 0: accepted. 1: diagnostics (a rejection stands whatever is unchecked).
            // 2: nothing found, but part of the check does not exist yet, so the
            // compiler cannot claim the package is correct.
            Ok(if !analysis.diagnostics.is_empty() {
                1
            } else if ok {
                0
            } else {
                2
            })
        }
        "fix" => {
            let path = path_arg(&args, &[])?;
            let dry_run = has_flag(&args, "--dry-run");
            let started = Instant::now();
            let outcome = fix::fix_path(&path).map_err(|e| e.to_string())?;
            let ok = outcome.diagnostics.is_empty();
            if json {
                for edit in &outcome.edits {
                    println!("{}", edit.to_json());
                }
                for d in &outcome.diagnostics {
                    println!("{}", d.to_json(&outcome.sources));
                }
                println!(
                    "{{\"type\":\"summary\",\"command\":\"fix\",\"ok\":{ok},\"protocol\":1,\"applied\":{},\"rounds\":{},\"errors\":{},\"ms\":{}}}",
                    outcome.edits.len(),
                    outcome.rounds,
                    outcome.diagnostics.len(),
                    started.elapsed().as_millis()
                );
            } else {
                for d in &outcome.diagnostics {
                    print!("{}", d.to_human(&outcome.sources));
                }
                eprintln!(
                    "fixed {} edit(s) in {} round(s), {} error(s) remain",
                    outcome.edits.len(),
                    outcome.rounds,
                    outcome.diagnostics.len()
                );
            }
            if !dry_run {
                fix::write_back(&outcome).map_err(|e| e.to_string())?;
            }
            Ok(if ok { 0 } else { 1 })
        }
        "run" | "test" | "cost" => Err(format!("`{command}` is not implemented yet")),
        "conformance" => {
            let root = args
                .iter()
                .skip(1)
                .find(|a| !a.starts_with('-'))
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("conformance"));
            let report = conformance::run(&root);
            for f in &report.failures {
                eprintln!("FAIL {}: {}", f.path.display(), f.message);
            }
            eprintln!(
                "{} passed, {} failed, {} skipped",
                report.passed, report.failed, report.skipped
            );
            Ok(if report.failed == 0 { 0 } else { 1 })
        }
        other => Err(format!("unknown command `{other}`")),
    }
}

/// E1702 for a `--target` that is not one of the three. The name has no file, so the
/// diagnostic points into the command line.
fn unknown_target(name: &str, json: bool, started: Instant) -> u8 {
    let text = format!("--target {name}");
    let source = pipeline::command_line_source(&text);
    let span = Span::new("--target ".len(), text.len());
    let d = Diagnostic::new(
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
    if json {
        println!("{}", d.to_json(&sources));
        println!(
            "{{\"type\":\"summary\",\"command\":\"check\",\"ok\":false,\"protocol\":1,\"errors\":1,\"files\":0,\"targets\":[],\"ms\":{}}}",
            started.elapsed().as_millis()
        );
    } else {
        print!("{}", d.to_human(&sources));
    }
    1
}

/// The value of `--flag VALUE`, if the flag is given.
fn flag_value(args: &[String], flag: &str) -> Result<Option<String>, String> {
    match args.iter().position(|a| a == flag) {
        None => Ok(None),
        Some(i) => match args.get(i + 1) {
            Some(v) if !v.starts_with("--") => Ok(Some(v.clone())),
            _ => Err(format!("{flag} needs a value")),
        },
    }
}

fn has_flag(args: &[String], flag: &str) -> bool {
    args.iter().any(|a| a == flag)
}

fn path_arg(args: &[String], value_flags: &[&str]) -> Result<PathBuf, String> {
    if let Some(path) = positional(args, value_flags) {
        if path.is_dir() {
            return Ok(path);
        }
        if !path.exists() {
            return Err(format!("{} does not exist", path.display()));
        }
        return Ok(path);
    }
    // tools.md §2: PATH defaults to the current package.
    Ok(PathBuf::from("."))
}

fn positional(args: &[String], value_flags: &[&str]) -> Option<PathBuf> {
    let mut i = 1;
    while i < args.len() {
        let arg = &args[i];
        if value_flags.contains(&arg.as_str()) {
            i += 2;
            continue;
        }
        if arg.starts_with('-') {
            i += 1;
            continue;
        }
        return Some(PathBuf::from(arg));
    }
    None
}
