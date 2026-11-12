use ingotc::check::analyze_file;
use ingotc::conformance;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

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
            "usage: ingot <check|run|test|conformance|explain> [options] [path]".to_string(),
        );
    };
    match command {
        "check" => {
            let json = args.iter().any(|a| a == "--json");
            let path = path_arg(&args)?;
            let analysis = analyze_file(&path).map_err(|e| e.to_string())?;
            if json {
                for d in &analysis.diagnostics {
                    println!("{}", d.to_json(&analysis.sources));
                }
            } else {
                for d in &analysis.diagnostics {
                    print!("{}", d.to_human(&analysis.sources));
                }
                if analysis.diagnostics.is_empty() {
                    eprintln!("ok: no diagnostics");
                }
            }
            Ok(if analysis.diagnostics.is_empty() {
                0
            } else {
                1
            })
        }
        "run" => {
            let path = path_arg(&args)?;
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
            let path = path_arg(&args)?;
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
            let path = path_arg(&args)?;
            let analysis = analyze_file(&path).map_err(|e| e.to_string())?;
            if !analysis.diagnostics.is_empty() {
                for d in &analysis.diagnostics {
                    print!("{}", d.to_human(&analysis.sources));
                }
                return Ok(1);
            }
            let fn_filter = args
                .iter()
                .position(|a| a == "--fn")
                .and_then(|i| args.get(i + 1));
            for item in ingotc::cost::cost_items(&analysis.program) {
                if let Some(name) = fn_filter
                    && !item.function.ends_with(name)
                {
                    continue;
                }
                println!("{}", item.to_json(&analysis.sources));
            }
            Ok(0)
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

fn path_arg(args: &[String]) -> Result<PathBuf, String> {
    let Some(path) = args
        .iter()
        .skip(1)
        .find(|a| !a.starts_with('-'))
        .map(PathBuf::from)
    else {
        return Err("give a source file".to_string());
    };
    if path.is_dir() {
        return Ok(path);
    }
    if !Path::new(&path).exists() {
        return Err(format!("{} does not exist", path.display()));
    }
    Ok(path)
}
