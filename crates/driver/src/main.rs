use ingot_driver::commands;
use ingot_driver::conformance;
use ingot_driver::files::Overlays;
use ingot_driver::fix;
use ingot_driver::serve;
use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Instant;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let json = has_flag(&args, "--json");
    let started = Instant::now();
    let code = match run(&args) {
        Ok(code) => code,
        Err(message) => {
            // tools.md §1 "Failures": with --json a named failure writes an `error` line
            // and its summary, with the command word as given (none: the empty string).
            if json {
                for line in commands::failure_lines(&command_word(&args), &message, started) {
                    println!("{line}");
                }
            } else {
                eprintln!("ingot: {message}");
            }
            2
        }
    };
    ExitCode::from(code)
}

fn run(args: &[String]) -> Result<u8, String> {
    let Some(command) = args.first().map(String::as_str) else {
        return Err("usage: ingot <check|fix|serve|conformance> [options] [path]".to_string());
    };
    let json = has_flag(args, "--json");
    match command {
        "check" => {
            if has_flag(args, "--all-targets") {
                return Err("not implemented: `check --all-targets`".to_string());
            }
            let path = positional(args, &["--target"]).unwrap_or_else(|| PathBuf::from("."));
            let target = flag_value(args, "--target")?;
            let output = commands::check(&path, target.as_deref(), &Overlays::default());
            if json {
                for line in &output.lines {
                    println!("{line}");
                }
            } else {
                print!("{}", output.human);
                if let Some(message) = &output.error {
                    eprintln!("ingot: {message}");
                }
                if output.exit == 0 {
                    eprintln!("ok: no diagnostics");
                }
            }
            for what in &output.not_implemented {
                eprintln!("ingot: not implemented: {what}");
            }
            Ok(output.exit)
        }
        "fix" => {
            let path = path_arg(args, &[])?;
            let dry_run = has_flag(args, "--dry-run");
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
        "serve" => {
            let stdin = std::io::stdin();
            let mut input = stdin.lock();
            let stdout = std::io::stdout();
            let mut output = stdout.lock();
            let mut server = serve::Server::new();
            let mut line = String::new();
            loop {
                line.clear();
                let read = input.read_line(&mut line).map_err(|e| e.to_string())?;
                // End of input is a shutdown (tools.md §6): exit 0 with nothing written.
                if read == 0 {
                    break;
                }
                let request = line.trim_end_matches(['\n', '\r']);
                for response in server.handle(request) {
                    writeln!(output, "{response}").map_err(|e| e.to_string())?;
                }
                output.flush().map_err(|e| e.to_string())?;
                if server.shutdown_requested() {
                    break;
                }
            }
            Ok(0)
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

/// The command word as the user gave it: the first argument when it is not a flag.
fn command_word(args: &[String]) -> String {
    match args.first() {
        Some(word) if !word.starts_with('-') => word.clone(),
        _ => String::new(),
    }
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
