use ingot_diag::diagnostic::DEFAULT_TARGET;
use ingot_driver::conformance;
use ingot_driver::fix;
use ingot_driver::pipeline;
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
            let path = path_arg(&args, &[])?;
            let started = Instant::now();
            let analysis = pipeline::analyze_path(&path).map_err(|e| e.to_string())?;
            let ok = analysis.diagnostics.is_empty();
            if json {
                for d in &analysis.diagnostics {
                    println!("{}", d.to_json(&analysis.sources));
                }
                println!(
                    "{{\"type\":\"summary\",\"command\":\"check\",\"ok\":{ok},\"protocol\":1,\"errors\":{},\"files\":{},\"targets\":[\"{DEFAULT_TARGET}\"],\"ms\":{}}}",
                    analysis.diagnostics.len(),
                    analysis.sources.files.len(),
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
            Ok(if ok { 0 } else { 1 })
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
