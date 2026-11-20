//! Every ```ingot block in the specification must lex and parse without a lexical or
//! syntax error (E01xx or E02xx).
//!
//! A block passes if it parses as a file, or if it parses as the body of
//! `fn wrapper() { ... }`. Blocks that mix top-level items with statements can
//! pass neither way; they are listed in `ALLOWLIST` with a reason. The list must be
//! exact: an allowlisted block that now passes fails the test, so the list stays
//! honest.

use ingot_diag::source::Source;
use ingot_syntax::lexer::lex;
use ingot_syntax::parser::{check_operator_mixing, parse};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

/// `(file relative to the repository root, index of the block among that file's
/// ```ingot blocks, reason)`.
const ALLOWLIST: &[(&str, usize, &str)] = &[
    (
        "docs/05-tooling.md",
        1,
        "§34.2 lists one-line blocks side by side, among them a `match` arm, which is neither a statement nor an item",
    ),
    (
        "docs/05-tooling.md",
        4,
        "§34.3 shows a broken function header alone, without its body or closing brace",
    ),
    (
        "docs/04-hardware.md",
        1,
        "kernel items followed by top-level `dispatch` statements; neither a file nor a fn body holds both",
    ),
    (
        "docs/primer.md",
        4,
        "items followed by top-level `let ... catch` statements; neither a file nor a fn body holds both",
    ),
];

struct Snippet {
    file: String,
    index: usize,
    text: String,
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// `docs/*.md`, `docs/impl/*.md` and `README.md`, in a fixed order.
fn spec_files() -> Vec<PathBuf> {
    let root = repo_root();
    let mut files = Vec::new();
    for dir in [root.join("docs"), root.join("docs/impl")] {
        let mut found: Vec<PathBuf> = fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|entry| entry.path())
            .filter(|p| p.is_file() && p.extension().is_some_and(|x| x == "md"))
            .collect();
        found.sort();
        files.extend(found);
    }
    files.push(root.join("README.md"));
    files
}

/// The body of every fenced block whose opening fence starts with ```ingot.
fn ingot_blocks(text: &str) -> Result<Vec<String>, String> {
    let mut blocks = Vec::new();
    let mut current: Option<String> = None;
    for line in text.lines() {
        if let Some(body) = current.as_mut() {
            if line.trim() == "```" {
                blocks.push(current.take().unwrap_or_default());
            } else {
                body.push_str(line);
                body.push('\n');
            }
        } else if line.trim_start().starts_with("```ingot") {
            current = Some(String::new());
        }
    }
    if current.is_some() {
        return Err("a ```ingot block is never closed".to_string());
    }
    Ok(blocks)
}

/// The specification writes an elided body as `{ .. }` or `{ ... }`, with any
/// spacing. Both mean `{ }` for the purpose of parsing.
fn normalize(block: &str) -> String {
    let mut out = String::with_capacity(block.len());
    let mut rest = block;
    while let Some(at) = rest.find('{') {
        out.push_str(&rest[..at]);
        let after = &rest[at + 1..];
        let trimmed = after.trim_start_matches([' ', '\t']);
        let elided = trimmed
            .strip_prefix("...")
            .or_else(|| trimmed.strip_prefix(".."))
            .map(|body| body.trim_start_matches([' ', '\t']));
        match elided.and_then(|body| body.strip_prefix('}')) {
            Some(after_brace) => {
                out.push_str("{ }");
                rest = after_brace;
            }
            None => {
                out.push('{');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// The E01xx and E02xx diagnostics of lexing and parsing `text` as a file.
fn syntax_errors(text: &str) -> Vec<String> {
    let source = Source::new("snippet.ingot", text);
    let (tokens, mut diagnostics) = lex(&source);
    let (module, parse_diagnostics) = parse(&source, tokens);
    diagnostics.extend(parse_diagnostics);
    diagnostics.extend(check_operator_mixing(&module, &source));
    diagnostics
        .iter()
        .filter(|d| d.code.starts_with("E01") || d.code.starts_with("E02"))
        .map(|d| {
            let line = source.position(d.span.start).line;
            format!("{} at block line {line}", d.code)
        })
        .collect()
}

fn passes(block: &str) -> Result<(), Vec<String>> {
    let as_file = syntax_errors(block);
    if as_file.is_empty() {
        return Ok(());
    }
    let wrapped = syntax_errors(&format!("fn wrapper() {{\n{block}\n}}"));
    if wrapped.is_empty() {
        Ok(())
    } else {
        Err(as_file)
    }
}

#[test]
fn spec_snippets_parse() {
    let root = repo_root();
    let mut snippets = Vec::new();
    for path in spec_files() {
        let file = path
            .strip_prefix(&root)
            .unwrap_or(&path)
            .to_string_lossy()
            .into_owned();
        let text = fs::read_to_string(&path).unwrap();
        let blocks = ingot_blocks(&text).unwrap_or_else(|e| panic!("{file}: {e}"));
        for (index, body) in blocks.into_iter().enumerate() {
            snippets.push(Snippet {
                file: file.clone(),
                index,
                text: body,
            });
        }
    }
    assert!(!snippets.is_empty(), "no ```ingot blocks found");

    let mut failures = Vec::new();
    let mut unexpected_passes = Vec::new();
    let mut matched = BTreeSet::new();
    for snippet in &snippets {
        let listed = ALLOWLIST
            .iter()
            .find(|(file, index, _)| *file == snippet.file && *index == snippet.index);
        if listed.is_some() {
            matched.insert((snippet.file.clone(), snippet.index));
        }
        let result = passes(&normalize(&snippet.text));
        match (result, listed) {
            (Err(errors), None) => failures.push(format!(
                "{} block {}: {}",
                snippet.file,
                snippet.index,
                errors.join(", ")
            )),
            (Ok(()), Some(_)) => unexpected_passes.push(format!(
                "{} block {} now parses; remove it from ALLOWLIST",
                snippet.file, snippet.index
            )),
            _ => {}
        }
    }
    let mut stale = Vec::new();
    for (file, index, _) in ALLOWLIST {
        if !matched.contains(&(file.to_string(), *index)) {
            stale.push(format!("{file} block {index} does not exist"));
        }
    }

    for line in failures.iter().chain(&unexpected_passes).chain(&stale) {
        eprintln!("{line}");
    }
    assert!(
        failures.is_empty() && unexpected_passes.is_empty() && stale.is_empty(),
        "{} snippet(s) fail, {} allowlisted snippet(s) pass, {} allowlist entries are stale",
        failures.len(),
        unexpected_passes.len(),
        stale.len()
    );
}
