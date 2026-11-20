//! `ingot test [PATH] [--filter TEXT] [--seed N] [--opt] [--json]` (tools.md section 3).
//!
//! The package's `test` blocks are built into one executable whose generated entry
//! hands them to `std.sys.rt.test_start` as a table. The harness runs that executable
//! once per selected test, with the test's index and a temporary directory it made, and
//! maps the process's end to a status: exit 0 is `ok`, 1 `failed`, 101 `trapped`, and
//! anything else `crashed`.

use crate::build;
use crate::commands;
use crate::conformance::{TrapLine, parse_trap};
use crate::pipeline::{self, Analysis};
use ingot_diag::diagnostic::{json_string, write_span};
use ingot_diag::source::Span;
use ingot_sema::defs::DefId;
use std::hash::{BuildHasher, Hasher};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;

/// What the command line asked for.
#[derive(Clone, Debug)]
pub struct Options {
    pub path: PathBuf,
    pub filter: Option<String>,
    pub seed: Option<u64>,
    /// `--opt`: the optimizing pipeline instead of `--fast`.
    pub opt: bool,
    pub json: bool,
}

/// One test of the executable's table.
struct Test {
    /// Its index in the table.
    index: usize,
    name: String,
    /// The full path of its module.
    module: String,
    file: String,
    span: Span,
}

/// How a test's process ended.
enum Outcome {
    Ok,
    Failed {
        error: String,
    },
    Trapped {
        trap: TrapLine,
    },
    Crashed {
        exit: Option<i32>,
        signal: Option<i32>,
    },
}

/// What running one test produced.
struct Ran {
    outcome: Outcome,
    ms: f64,
    stdout: String,
    stderr: String,
}

/// Run the command; the return value is the exit status (tools.md section 1). A named
/// failure that stops the command before it does its work is `Err`, which the caller
/// prints as `{"type":"error"}` and a summary under `--json`.
pub fn run(options: &Options) -> Result<u8, String> {
    let started = Instant::now();
    let seed = options.seed.unwrap_or_else(draw_seed);
    let mut load = pipeline::Options {
        roots: ingot_cfg::instances::Roots::Test,
        ..pipeline::Options::default()
    };
    load.load.cache = crate::cache::default_cache();
    let mut analysis =
        pipeline::analyze_path_with(&options.path, &load).map_err(|e| e.to_string())?;
    // Diagnostics, as `check` prints them.
    for d in &analysis.diagnostics {
        if options.json {
            println!("{}", d.to_json_for(&analysis.sources, &analysis.target));
        } else {
            print!("{}", d.to_human(&analysis.sources));
        }
    }
    if analysis.workspace_root_without_package {
        return Err("this workspace root is not a package; test one of its members".to_string());
    }
    if !analysis.diagnostics.is_empty() {
        if options.json {
            let errors = analysis.diagnostics.len();
            summary(options, false, errors, 0, 0, seed, started);
        }
        return Ok(1);
    }
    if !analysis.not_implemented.is_empty() {
        for what in &analysis.not_implemented {
            eprintln!("ingot: not implemented: {what}");
        }
        // No tests ran, so the text form has no count to give.
        if options.json {
            summary(options, false, 0, 0, 0, seed, started);
        }
        return Ok(2);
    }
    let tests = list_tests(&analysis);
    if tests.is_empty() {
        // Nothing to build or run.
        summary(options, true, 0, 0, 0, seed, started);
        return Ok(0);
    }
    let root = build::package_root(&options.path);
    let built = match build::build_analysis_as(&mut analysis, &root, None, !options.opt, true) {
        Ok(built) => built,
        Err(e) => {
            // The compiler's own failure, as `build` reports one (exit 2).
            if options.json {
                for line in commands::failure_lines("test", &e.to_string(), started) {
                    println!("{line}");
                }
            } else {
                eprintln!("ingot: {e}");
            }
            return Ok(2);
        }
    };
    let mut passed = 0usize;
    let mut failed = 0usize;
    for test in &tests {
        if let Some(filter) = &options.filter
            && !test.name.contains(filter.as_str())
        {
            continue;
        }
        let ran = run_one(&built.artifact, test.index)?;
        let ok = matches!(ran.outcome, Outcome::Ok);
        if ok {
            passed += 1;
        } else {
            failed += 1;
        }
        if options.json {
            println!("{}", test_line(&analysis, test, &ran, seed));
        } else {
            print_test(test, &ran);
        }
    }
    summary(options, failed == 0, 0, passed, failed, seed, started);
    Ok(if failed == 0 { 0 } else { 1 })
}

/// A seed from the operating system's entropy: `RandomState` is seeded from it. It
/// stays below 2^32 so that every JSON reader holds it exactly.
fn draw_seed() -> u64 {
    let state = std::collections::hash_map::RandomState::new();
    state.build_hasher().finish() >> 32
}

/// The tests of the executable, in the order of its table.
fn list_tests(analysis: &Analysis) -> Vec<Test> {
    let Some(front) = &analysis.front_end else {
        return Vec::new();
    };
    let sema = &front.resolved.sema;
    ingot_codegen::test_items(sema, &analysis.instances)
        .into_iter()
        .enumerate()
        .map(|(index, def)| {
            let d = sema.def(def);
            Test {
                index,
                name: d.name.clone(),
                module: sema.module(d.module).path.clone(),
                file: d
                    .file
                    .map(|f| analysis.sources.get(f).path.clone())
                    .unwrap_or_default(),
                span: item_span(front, def),
            }
        })
        .collect()
}

/// The whole `test` item: from the keyword to the closing brace.
fn item_span(front: &pipeline::FrontEnd, def: DefId) -> Span {
    for package in &front.resolved.packages {
        if let Some(ingot_sema::hir::Item::Test(t)) = package.items.get(&def) {
            return t.span;
        }
    }
    front.resolved.sema.def(def).span
}

/// Run the test executable for the test at `index`: a fresh temporary directory
/// made before and removed after, whatever the outcome; standard input null; output
/// captured.
fn run_one(artifact: &Path, index: usize) -> Result<Ran, String> {
    let dir = make_temp_dir()?;
    let started = Instant::now();
    let output = Command::new(artifact)
        .arg(index.to_string())
        .arg(&dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output();
    let ms = started.elapsed().as_secs_f64() * 1000.0;
    let _ = std::fs::remove_dir_all(&dir);
    let output = output.map_err(|e| format!("could not run {}: {e}", artifact.display()))?;
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    let outcome = match output.status.code() {
        Some(0) => Outcome::Ok,
        Some(1) => Outcome::Failed {
            error: error_of(&stderr),
        },
        Some(101) => match stderr.lines().rev().find_map(parse_trap) {
            Some(trap) => Outcome::Trapped { trap },
            // Status 101 without a trap line is not a trap: `trapped` promises one.
            None => Outcome::Crashed {
                exit: Some(101),
                signal: None,
            },
        },
        Some(code) => Outcome::Crashed {
            exit: Some(code),
            signal: None,
        },
        None => Outcome::Crashed {
            exit: None,
            signal: signal_of(&output.status),
        },
    };
    Ok(Ran {
        outcome,
        ms,
        stdout,
        stderr,
    })
}

#[cfg(unix)]
fn signal_of(status: &std::process::ExitStatus) -> Option<i32> {
    use std::os::unix::process::ExitStatusExt;
    status.signal()
}

#[cfg(not(unix))]
fn signal_of(_status: &std::process::ExitStatus) -> Option<i32> {
    None
}

/// The error of a failed test: what follows `error: ` on its line of standard error.
/// A test that exits 1 without one (it cannot, today) reports its whole standard error.
fn error_of(stderr: &str) -> String {
    stderr
        .lines()
        .find_map(|line| line.strip_prefix("error: "))
        .map(str::to_string)
        .unwrap_or_else(|| stderr.trim_end().to_string())
}

/// A new empty directory under the system temporary directory.
fn make_temp_dir() -> Result<PathBuf, String> {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    for _ in 0..100 {
        let dir = std::env::temp_dir().join(format!(
            "ingot-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        match std::fs::create_dir(&dir) {
            Ok(()) => return Ok(dir),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => {
                return Err(format!(
                    "could not create the temporary directory {}: {e}",
                    dir.display()
                ));
            }
        }
    }
    Err("could not find an unused temporary directory name".to_string())
}

/// The offset of `line:col` (1-based; the column counts bytes) in `text`, kept inside
/// the text.
fn offset_of(text: &str, line: u32, col: u32) -> usize {
    let mut start = 0;
    for _ in 1..line {
        match text[start..].find('\n') {
            Some(at) => start += at + 1,
            None => break,
        }
    }
    (start + col.saturating_sub(1) as usize).min(text.len())
}

/// The span a trap line names. The line carries a position and nothing else, so the
/// span is empty: `end` equals `start`, the byte the line and column name.
fn trap_span(analysis: &Analysis, trap: &TrapLine) -> Span {
    match analysis.sources.iter().find(|s| s.path == trap.file) {
        Some(source) => {
            let at = offset_of(&source.text, trap.line, trap.col);
            Span::new(at, at)
        }
        None => Span::new(0, 0),
    }
}

/// The JSON line of one test.
fn test_line(analysis: &Analysis, test: &Test, ran: &Ran, seed: u64) -> String {
    let mut out = String::from("{\"type\":\"test\",\"name\":");
    json_string(&mut out, &test.name);
    out.push_str(",\"module\":");
    json_string(&mut out, &test.module);
    out.push_str(",\"span\":");
    write_span(&mut out, &analysis.sources, &test.file, test.span);
    out.push_str(",\"status\":");
    match &ran.outcome {
        Outcome::Ok => out.push_str("\"ok\""),
        Outcome::Failed { error } => {
            out.push_str("\"failed\",\"error\":");
            json_string(&mut out, error);
        }
        Outcome::Trapped { trap } => {
            out.push_str("\"trapped\",\"trap\":{\"code\":");
            json_string(&mut out, &trap.code);
            out.push_str(",\"text\":");
            json_string(&mut out, &trap.text);
            out.push_str(",\"span\":");
            write_span(
                &mut out,
                &analysis.sources,
                &trap.file,
                trap_span(analysis, trap),
            );
            out.push('}');
        }
        Outcome::Crashed { exit, signal } => {
            out.push_str("\"crashed\"");
            if let Some(code) = exit {
                out.push_str(&format!(",\"exit\":{code}"));
            }
            if let Some(signal) = signal {
                out.push_str(&format!(",\"signal\":{signal}"));
            }
        }
    }
    out.push_str(&format!(",\"ms\":{:.3}", ran.ms));
    if !matches!(ran.outcome, Outcome::Ok) {
        out.push_str(&format!(",\"seed\":{seed}"));
    }
    out.push('}');
    out
}

/// The text form of one test: its line, and for a test that did not pass the output
/// it wrote.
fn print_test(test: &Test, ran: &Ran) {
    let how = match &ran.outcome {
        Outcome::Ok => "ok".to_string(),
        Outcome::Failed { error } => format!("FAILED  error: {error}"),
        Outcome::Trapped { trap } => format!("TRAPPED {}: {}", trap.code, trap.text),
        Outcome::Crashed {
            exit: Some(code), ..
        } => format!("CRASHED exit {code}"),
        Outcome::Crashed {
            signal: Some(signal),
            ..
        } => format!("CRASHED signal {signal}"),
        Outcome::Crashed { .. } => "CRASHED".to_string(),
    };
    println!("{} {}: {how} ({:.1} ms)", test.module, test.name, ran.ms);
    if !matches!(ran.outcome, Outcome::Ok) {
        for (label, text) in [("stderr", &ran.stderr), ("stdout", &ran.stdout)] {
            for line in text.lines() {
                println!("    {label}: {line}");
            }
        }
    }
}

fn summary(
    options: &Options,
    ok: bool,
    errors: usize,
    passed: usize,
    failed: usize,
    seed: u64,
    started: Instant,
) {
    if options.json {
        println!(
            "{{\"type\":\"summary\",\"command\":\"test\",\"ok\":{ok},\"protocol\":1,\"errors\":{errors},\"passed\":{passed},\"failed\":{failed},\"seed\":{seed},\"ms\":{}}}",
            started.elapsed().as_millis()
        );
    } else {
        println!("{passed} passed, {failed} failed (seed {seed})");
    }
}
