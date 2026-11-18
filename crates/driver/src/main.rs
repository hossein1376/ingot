use ingot_driver::build;
use ingot_driver::commands;
use ingot_driver::conformance;
use ingot_driver::files::Overlays;
use ingot_driver::fix;
use ingot_driver::pipeline;
use ingot_driver::query;
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
        return Err(
            "usage: ingot <check|fix|query|serve|conformance> [options] [path]".to_string(),
        );
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
        "query" => {
            let kind = args.get(1).map(String::as_str).unwrap_or("");
            let started = Instant::now();
            let outcome = if has_flag(args, "--name") {
                query::failure(kind, "not implemented: `query --name`".to_string(), started)
            } else {
                let at = positional_from(args, 2, &[]);
                let path = positional_from(args, 2, &["--match"]);
                let match_text = flag_value(args, "--match")?;
                query::run(
                    kind,
                    at.as_deref(),
                    path.as_deref(),
                    match_text.as_deref(),
                    &Overlays::default(),
                    started,
                )
            };
            if json {
                for line in &outcome.lines {
                    println!("{line}");
                }
            } else {
                print!("{}", outcome.human);
                if let Some(message) = &outcome.error {
                    eprintln!("ingot: {message}");
                }
            }
            Ok(outcome.exit)
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
        "cost" => {
            let path = path_arg(args, &["--fn"])?;
            let function = flag_value(args, "--fn")?;
            let output = commands::cost(&path, function.as_deref(), &Overlays::default());
            if json {
                for line in &output.lines {
                    println!("{line}");
                }
            } else {
                print!("{}", output.human);
                if let Some(message) = &output.error {
                    eprintln!("ingot: {message}");
                }
            }
            for what in &output.not_implemented {
                eprintln!("ingot: not implemented: {what}");
            }
            Ok(output.exit)
        }
        "build" => {
            let target = flag_value(args, "--target")?;
            let output = flag_value(args, "-o")?;
            let fast = has_flag(args, "--fast");
            let path = path_arg(args, &["--target", "-o"])?;
            build_command(
                &path,
                target.as_deref(),
                output.map(PathBuf::from),
                fast,
                json,
            )
        }
        "run" => {
            if has_flag(args, "--target") {
                return Err("`run` has no `--target`; it builds for the host target".to_string());
            }
            let fast = has_flag(args, "--fast");
            let options = pipeline::Options {
                roots: ingot_cfg::instances::Roots::Build,
                ..pipeline::Options::default()
            };
            let path = PathBuf::from(".");
            let mut analysis =
                pipeline::analyze_path_with(&path, &options).map_err(|e| e.to_string())?;
            print_analysis(&analysis, false);
            if !analysis.diagnostics.is_empty() {
                return Ok(1);
            }
            if !analysis.not_implemented.is_empty() {
                for what in &analysis.not_implemented {
                    eprintln!("ingot: not implemented: {what}");
                }
                return Ok(2);
            }
            let root = package_root(&path);
            let built = match build::build_analysis(&mut analysis, &root, None, fast) {
                Ok(built) => built,
                Err(ingot_codegen::BuildError::NoEntryPoint) => {
                    print_no_entry(&analysis, false);
                    return Ok(1);
                }
                Err(e) => {
                    eprintln!("ingot: {e}");
                    return Ok(2);
                }
            };
            let mut rest = Vec::new();
            if let Some(i) = args.iter().position(|a| a == "--") {
                rest.extend_from_slice(&args[i + 1..]);
            }
            let status = std::process::Command::new(&built.artifact)
                .args(&rest)
                .status()
                .map_err(|e| format!("could not run {}: {e}", built.artifact.display()))?;
            Ok(process_status(status))
        }
        "test" => Err(format!("`{command}` is not implemented yet")),
        "conformance" => {
            let root = args
                .iter()
                .skip(1)
                .find(|a| !a.starts_with('-'))
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("conformance"));
            let report = conformance::run(&root);
            for (path, reason) in &report.skips {
                eprintln!("SKIP {}: {reason}", path.display());
            }
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

/// The child's exit status, or 128 + the signal that killed it. A signal is not a
/// compiler failure, so it must not collapse to status 2.
fn process_status(status: std::process::ExitStatus) -> u8 {
    if let Some(code) = status.code() {
        return (code & 0xff) as u8;
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            return (128 + signal) as u8;
        }
    }
    2
}

fn package_root(path: &std::path::Path) -> PathBuf {
    if path.is_dir() {
        return path.to_path_buf();
    }
    let files = ingot_driver::files::Files::default();
    ingot_driver::loader::manifest_dir(path, &files).unwrap_or_else(|| {
        path.parent()
            .unwrap_or(std::path::Path::new("."))
            .to_path_buf()
    })
}

fn print_no_entry(analysis: &pipeline::Analysis, json: bool) {
    let file = analysis
        .front_end
        .as_ref()
        .and_then(|front| {
            let package = front.resolved.sema.packages.get(front.checked)?;
            let module = package.root?;
            front.resolved.sema.module(module).file
        })
        .and_then(|id| analysis.sources.files.get(id))
        .or_else(|| analysis.sources.files.first());
    let fallback = ingot_diag::source::Source::new("ingot.toml", "");
    let source = file.unwrap_or(&fallback);
    let diagnostic = ingot_diag::diagnostic::Diagnostic::new(
        "E1704",
        "`main` is missing: a build needs `pub fn main` in `main.ingot`",
        source,
        ingot_diag::source::Span::new(0, 0),
    );
    if json {
        println!(
            "{}",
            diagnostic.to_json_for(&analysis.sources, &analysis.target)
        );
        println!(
            "{{\"type\":\"summary\",\"command\":\"build\",\"ok\":false,\"protocol\":1,\"errors\":1,\"files\":{},\"target\":\"{}\",\"ms\":0}}",
            analysis.files, analysis.target
        );
    } else {
        print!("{}", diagnostic.to_human(&analysis.sources));
    }
}

fn print_analysis(analysis: &pipeline::Analysis, json: bool) {
    if json {
        for line in analysis
            .diagnostics
            .iter()
            .map(|d| d.to_json_for(&analysis.sources, &analysis.target))
        {
            println!("{line}");
        }
    } else {
        for d in &analysis.diagnostics {
            print!("{}", d.to_human(&analysis.sources));
        }
    }
}

fn build_command(
    path: &std::path::Path,
    target: Option<&str>,
    output: Option<PathBuf>,
    fast: bool,
    json: bool,
) -> Result<u8, String> {
    let started = Instant::now();
    let mut options = pipeline::Options {
        roots: ingot_cfg::instances::Roots::Build,
        ..pipeline::Options::default()
    };
    if let Some(name) = target {
        if ingot_sema::Target::by_name(name).is_none() {
            return Err(format!("`{name}` is not a target"));
        }
        options.target = name.to_string();
    }
    let mut analysis = match pipeline::analyze_path_with(path, &options) {
        Ok(a) => a,
        Err(e) => {
            if json {
                for line in commands::failure_lines("build", &e.to_string(), started) {
                    println!("{line}");
                }
            }
            return Err(e.to_string());
        }
    };
    print_analysis(&analysis, json);
    if !analysis.diagnostics.is_empty() {
        if json {
            println!(
                "{{\"type\":\"summary\",\"command\":\"build\",\"ok\":false,\"protocol\":1,\"errors\":{},\"files\":{},\"target\":\"{}\",\"ms\":{}}}",
                analysis.diagnostics.len(),
                analysis.files,
                analysis.target,
                started.elapsed().as_millis()
            );
        }
        return Ok(1);
    }
    if !analysis.not_implemented.is_empty() {
        for what in &analysis.not_implemented {
            eprintln!("ingot: not implemented: {what}");
        }
        if json {
            println!(
                "{{\"type\":\"summary\",\"command\":\"build\",\"ok\":false,\"protocol\":1,\"errors\":0,\"files\":{},\"target\":\"{}\",\"ms\":{}}}",
                analysis.files,
                analysis.target,
                started.elapsed().as_millis()
            );
        }
        return Ok(2);
    }
    let root = package_root(path);
    let built = match build::build_analysis(&mut analysis, &root, output.as_deref(), fast) {
        Ok(built) => built,
        Err(ingot_codegen::BuildError::NoEntryPoint) => {
            print_no_entry(&analysis, json);
            return Ok(1);
        }
        Err(e) => {
            if json {
                for line in commands::failure_lines("build", &e.to_string(), started) {
                    println!("{line}");
                }
            } else {
                eprintln!("ingot: {e}");
            }
            return Ok(2);
        }
    };
    if json {
        println!(
            "{{\"type\":\"summary\",\"command\":\"build\",\"ok\":true,\"protocol\":1,\"errors\":0,\"files\":{},\"target\":\"{}\",\"artifact\":{},\"ms\":{}}}",
            analysis.files,
            analysis.target,
            {
                let mut s = String::new();
                ingot_diag::diagnostic::json_string(&mut s, &built.artifact.display().to_string());
                s
            },
            started.elapsed().as_millis()
        );
    } else {
        println!("built {}", built.artifact.display());
    }
    Ok(0)
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

fn positional_from(args: &[String], start: usize, value_flags: &[&str]) -> Option<String> {
    let mut i = start;
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
        return Some(arg.clone());
    }
    None
}
