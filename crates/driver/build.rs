//! Embed the diagnostic catalog and the conformance suite, so `ingot explain`
//! answers from the same sources the compiler is built with (tools.md §2).
//!
//! The generated table names every test by its path under `conformance/`, with the
//! test's entry file and its `.fixed.ingot` when it has one. Expectation, sections
//! and marked codes are read here; the text is embedded with `include_str!`.

use std::fs;
use std::path::{Path, PathBuf};

fn main() {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let root = manifest.join("../..");
    let catalog = root.join("docs/impl/diagnostics.md");
    let suite = root.join("conformance");
    println!("cargo:rerun-if-changed={}", catalog.display());
    println!("cargo:rerun-if-changed={}", suite.display());
    rerun_dirs(&suite);
    println!("cargo:rerun-if-changed=build.rs");

    let mut cases = Vec::new();
    collect_cases(&suite, &suite, &mut cases);
    cases.sort_by(|a, b| a.path.cmp(&b.path));

    let mut out = String::new();
    out.push_str("pub struct EmbeddedCase {\n");
    out.push_str("    pub path: &'static str,\n");
    out.push_str("    pub expect: &'static str,\n");
    out.push_str("    pub sections: &'static [&'static str],\n");
    out.push_str("    pub codes: &'static [&'static str],\n");
    out.push_str("    pub text: &'static str,\n");
    out.push_str("    pub fixed: Option<&'static str>,\n");
    out.push_str("}\n\n");
    out.push_str(&format!(
        "pub const CATALOG: &str = include_str!({:?});\n\n",
        catalog.display().to_string()
    ));
    out.push_str("pub static CASES: &[EmbeddedCase] = &[\n");
    for case in &cases {
        out.push_str("    EmbeddedCase {\n");
        out.push_str(&format!("        path: {:?},\n", case.path));
        out.push_str(&format!("        expect: {:?},\n", case.expect));
        out.push_str("        sections: &[");
        for (i, section) in case.sections.iter().enumerate() {
            if i > 0 {
                out.push_str(", ");
            }
            out.push_str(&format!("{section:?}"));
        }
        out.push_str("],\n");
        out.push_str("        codes: &[");
        for (i, code) in case.codes.iter().enumerate() {
            if i > 0 {
                out.push_str(", ");
            }
            out.push_str(&format!("{code:?}"));
        }
        out.push_str("],\n");
        out.push_str(&format!(
            "        text: include_str!({:?}),\n",
            case.entry.display().to_string()
        ));
        match &case.fixed {
            Some(fixed) => out.push_str(&format!(
                "        fixed: Some(include_str!({:?})),\n",
                fixed.display().to_string()
            )),
            None => out.push_str("        fixed: None,\n"),
        }
        out.push_str("    },\n");
    }
    out.push_str("];\n");

    let dest = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("reports_data.rs");
    fs::write(dest, out).unwrap();
}

struct Case {
    path: String,
    entry: PathBuf,
    fixed: Option<PathBuf>,
    expect: String,
    sections: Vec<String>,
    codes: Vec<String>,
}

/// A new file changes a nested directory, not the suite root: name every directory.
fn rerun_dirs(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            println!("cargo:rerun-if-changed={}", path.display());
            rerun_dirs(&path);
        }
    }
}

fn collect_cases(root: &Path, dir: &Path, out: &mut Vec<Case>) {
    let entry = dir.join("main.ingot");
    if entry.is_file() {
        out.push(read_case(root, &entry));
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = entries.flatten().collect();
    entries.sort_by_key(|entry| entry.path());
    for entry in entries {
        let path = entry.path();
        if path.is_dir() {
            collect_cases(root, &path, out);
        } else if path.extension().is_some_and(|ext| ext == "ingot")
            && !path
                .file_name()
                .is_some_and(|name| name.to_string_lossy().ends_with(".fixed.ingot"))
        {
            out.push(read_case(root, &path));
        }
    }
}

fn read_case(root: &Path, entry: &Path) -> Case {
    let text = fs::read_to_string(entry).unwrap_or_default();
    let package_test = entry.file_name().is_some_and(|name| name == "main.ingot");
    let mut files = Vec::new();
    if package_test {
        collect_files(entry.parent().unwrap_or(root), &mut files);
    } else {
        files.push(entry.to_path_buf());
    }
    files.sort();
    let mut sections = Vec::new();
    let mut codes = Vec::new();
    for file in &files {
        let Ok(file_text) = fs::read_to_string(file) else {
            continue;
        };
        for line in file_text.lines() {
            let trimmed = line.trim_start();
            if let Some(rest) = trimmed.strip_prefix("//!")
                && let Some(value) = rest.trim().strip_prefix("section:")
            {
                for section in value.split(',') {
                    let section = section.trim();
                    if !section.is_empty() && !sections.iter().any(|s| s == section) {
                        sections.push(section.to_string());
                    }
                }
            }
            let after = trimmed
                .find("//~")
                .map(|at| &trimmed[at + 3..])
                .or_else(|| trimmed.find("#~").map(|at| &trimmed[at + 2..]));
            let Some(rest) = after else {
                continue;
            };
            let Some(word) = rest.split_whitespace().next() else {
                continue;
            };
            if is_code(word) && !codes.iter().any(|code| code == word) {
                codes.push(word.to_string());
            }
        }
    }
    let mut expect = "accept".to_string();
    for line in text.lines() {
        let Some(rest) = line.trim_start().strip_prefix("//!") else {
            continue;
        };
        if let Some(value) = rest.trim().strip_prefix("expect:") {
            expect = value.trim().to_string();
        }
    }
    let fixed = entry.with_file_name(format!(
        "{}.fixed.ingot",
        entry
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_else(|| "test".to_string())
    ));
    let fixed = fixed.is_file().then_some(fixed);
    Case {
        path: entry
            .strip_prefix(root)
            .unwrap_or(entry)
            .to_string_lossy()
            .replace('\\', "/"),
        entry: entry.to_path_buf(),
        fixed,
        expect,
        sections,
        codes,
    }
}

fn collect_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_files(&path, out);
        } else if (path.extension().is_some_and(|ext| ext == "ingot")
            && !path
                .file_name()
                .is_some_and(|name| name.to_string_lossy().ends_with(".fixed.ingot")))
            || path.file_name().is_some_and(|name| name == "ingot.toml")
        {
            out.push(path);
        }
    }
}

fn is_code(word: &str) -> bool {
    let mut bytes = word.bytes();
    matches!(bytes.next(), Some(b'E') | Some(b'T'))
        && bytes.clone().count() > 0
        && bytes.all(|byte| byte.is_ascii_digit())
}
