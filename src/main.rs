use ingotc::check::{Analysis, analyze_file};
use ingotc::conformance;
use ingotc::diagnostic::DEFAULT_TARGET;
use ingotc::source::Sources;
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
        return Err(
            "usage: ingot <check|fix|run|test|cost|conformance|explain> [options] [path]"
                .to_string(),
        );
    };
    let json = has_flag(&args, "--json");
    match command {
        "check" => {
            let path = path_arg(&args, &[])?;
            let started = Instant::now();
            let analysis = analyze_file(&path).map_err(|e| e.to_string())?;
            let ok = analysis.diagnostics.is_empty();
            if json {
                for d in &analysis.diagnostics {
                    println!("{}", d.to_json(&analysis.sources));
                }
                let files = count_files(&analysis);
                println!(
                    "{{\"type\":\"summary\",\"command\":\"check\",\"ok\":{ok},\"protocol\":1,\"errors\":{},\"files\":{files},\"targets\":[\"{DEFAULT_TARGET}\"],\"ms\":{}}}",
                    analysis.diagnostics.len(),
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
            let outcome = ingotc::fix::fix_path(&path).map_err(|e| e.to_string())?;
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
                write_changed(&outcome.sources).map_err(|e| e.to_string())?;
            }
            Ok(if ok { 0 } else { 1 })
        }
        "run" => {
            let path = path_arg(&args, &[])?;
            let analysis = analyze_file(&path).map_err(|e| e.to_string())?;
            if !analysis.diagnostics.is_empty() {
                for d in &analysis.diagnostics {
                    print!("{}", d.to_human(&analysis.sources));
                }
                return Ok(1);
            }
            match ingotc::interp::run_main(&analysis) {
                Ok(status) => Ok(status as u8),
                Err(e) => {
                    eprintln!("{e}");
                    Ok(2)
                }
            }
        }
        "test" => {
            let path = path_arg(&args, &[])?;
            let analysis = analyze_file(&path).map_err(|e| e.to_string())?;
            if !analysis.diagnostics.is_empty() {
                for d in &analysis.diagnostics {
                    print!("{}", d.to_human(&analysis.sources));
                }
                return Ok(1);
            }
            match ingotc::interp::run_tests(&analysis.program, &analysis.sources) {
                Ok((passed, failed)) => {
                    eprintln!("{passed} passed, {failed} failed");
                    Ok(if failed == 0 { 0 } else { 1 })
                }
                Err(e) => Err(e),
            }
        }
        "cost" => {
            let path = path_arg(&args, &["--fn"])?;
            let started = Instant::now();
            let analysis = analyze_file(&path).map_err(|e| e.to_string())?;
            let ok = analysis.diagnostics.is_empty();
            let fn_filter = args
                .iter()
                .position(|a| a == "--fn")
                .and_then(|i| args.get(i + 1));
            if json {
                for d in &analysis.diagnostics {
                    println!("{}", d.to_json(&analysis.sources));
                }
                if ok {
                    for item in ingotc::cost::cost_items(&analysis.program) {
                        if let Some(name) = fn_filter
                            && !item.function.ends_with(name)
                        {
                            continue;
                        }
                        println!("{}", item.to_json(&analysis.sources));
                    }
                }
                println!(
                    "{{\"type\":\"summary\",\"command\":\"cost\",\"ok\":{ok},\"protocol\":1,\"ms\":{}}}",
                    started.elapsed().as_millis()
                );
            } else if ok {
                for item in ingotc::cost::cost_items(&analysis.program) {
                    if let Some(name) = fn_filter
                        && !item.function.ends_with(name)
                    {
                        continue;
                    }
                    println!("{}", item.to_json(&analysis.sources));
                }
            } else {
                for d in &analysis.diagnostics {
                    print!("{}", d.to_human(&analysis.sources));
                }
            }
            Ok(if ok { 0 } else { 1 })
        }
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
        "explain" => {
            let Some(code) = args.get(1) else {
                return Err("usage: ingot explain CODE".to_string());
            };
            println!("{} {}", code, ingotc::diagnostic::name_of(code));
            Ok(0)
        }
        other => Err(format!("unknown command `{other}`")),
    }
}

fn has_flag(args: &[String], flag: &str) -> bool {
    args.iter().any(|a| a == flag)
}

fn count_files(analysis: &Analysis) -> usize {
    analysis
        .sources
        .iter()
        .filter(|s| !s.path.starts_with("std/"))
        .count()
}

fn write_changed(sources: &Sources) -> std::io::Result<()> {
    for source in sources.iter() {
        if let Ok(on_disk) = std::fs::read_to_string(&source.absolute)
            && on_disk == source.text
        {
            continue;
        }
        std::fs::write(&source.absolute, &source.text)?;
    }
    Ok(())
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
