//! `ingot fmt`: the canonical formatter of tools.md §2 and 05 §34.

use crate::commands::Output;
use crate::files::{Files, Overlays};
use crate::loader::{self, LoadOptions};
use ingot_diag::diagnostic::json_string;
use ingot_syntax::fmt;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Instant;

#[derive(Clone, Debug)]
struct FileText {
    /// The path shown to the user.
    path: String,
    absolute: PathBuf,
    text: String,
}

pub fn stdin(_started: Instant) -> (String, u8) {
    let mut text = String::new();
    if std::io::stdin().read_to_string(&mut text).is_err() {
        return (String::new(), 2);
    }
    match fmt::format_text(&text) {
        Ok(formatted) => {
            if fmt::verify(&text, &formatted).is_err() {
                return (text, 2);
            }
            (formatted, 0)
        }
        Err(_) => (text, 1),
    }
}

pub fn run(path: &Path, check: bool, started: Instant) -> Output {
    let files = match files(path) {
        Ok(files) => files,
        Err(message) => return crate::commands::failure("fmt", message, started),
    };
    let mut lines = Vec::new();
    let mut human = String::new();
    let mut changed = Vec::new();
    for file in files {
        let formatted = match fmt::format_text(&file.text) {
            Ok(formatted) => formatted,
            Err(error) => {
                let message = format!("{}: {}", file.path, error.message);
                lines.push(crate::commands::error_line(&message));
                human.push_str(&message);
                human.push('\n');
                continue;
            }
        };
        if formatted == file.text {
            continue;
        }
        if let Err(error) = fmt::verify(&file.text, &formatted) {
            let message = format!("{}: {}", file.path, error.message);
            lines.push(crate::commands::error_line(&message));
            human.push_str(&message);
            human.push('\n');
            return Output {
                lines,
                human,
                error: None,
                not_implemented: Vec::new(),
                exit: 2,
            };
        }
        changed.push((file, formatted));
    }
    let failed = !lines.is_empty();
    if check || failed {
        for (file, _) in &changed {
            let mut line = String::from("{\"type\":\"unformatted\",\"file\":");
            json_string(&mut line, &file.path);
            line.push('}');
            lines.push(line);
            human.push_str(&file.path);
            human.push('\n');
        }
    } else {
        for (file, formatted) in &changed {
            if let Err(error) = std::fs::write(&file.absolute, formatted) {
                let message = format!("{}: {error}", file.path);
                lines.push(crate::commands::error_line(&message));
                human.push_str(&message);
                human.push('\n');
                return Output {
                    lines,
                    human,
                    error: None,
                    not_implemented: Vec::new(),
                    exit: 2,
                };
            }
            let mut line = String::from("{\"type\":\"formatted\",\"file\":");
            json_string(&mut line, &file.path);
            line.push('}');
            lines.push(line);
            human.push_str(&file.path);
            human.push('\n');
        }
    }
    let ok = !failed && (!check || changed.is_empty());
    lines.push(format!(
        "{{\"type\":\"summary\",\"command\":\"fmt\",\"ok\":{ok},\"protocol\":1,\"ms\":{}}}",
        started.elapsed().as_millis()
    ));
    Output {
        lines,
        human,
        error: None,
        not_implemented: Vec::new(),
        exit: if ok { 0 } else { 1 },
    }
}

fn files(path: &Path) -> Result<Vec<FileText>, String> {
    let filesystem = Files::new(Overlays::default());
    if filesystem.is_file(path) {
        return single_file(path);
    }
    if !filesystem.is_dir(path) {
        return Err(format!("{} does not exist", path.display()));
    }
    if !filesystem.exists(&path.join("ingot.toml")) {
        return Err(format!("{} is not a package", path.display()));
    }
    // Not the harness's conventions: in a real package `x.fixed.ingot` is an ordinary
    // module and is formatted like any other (conformance/README.md).
    let options = LoadOptions {
        cache: crate::cache::default_cache(),
        ..LoadOptions::default()
    };
    let graph = loader::load_graph(path, options).map_err(|error| error.to_string())?;
    let mut out = Vec::new();
    for index in graph.checked_in_order() {
        let package = &graph.packages[index];
        for module in &package.modules {
            let source = package.sources.get(module.source_id);
            out.push(FileText {
                path: source.path.clone(),
                absolute: source.absolute.clone(),
                text: source.text.clone(),
            });
        }
    }
    out.sort_by(|a, b| a.absolute.cmp(&b.absolute));
    Ok(out)
}

fn single_file(path: &Path) -> Result<Vec<FileText>, String> {
    let absolute = crate::files::absolute_path(path);
    let text = std::fs::read(&absolute).map_err(|error| format!("{}: {error}", path.display()))?;
    let text =
        String::from_utf8(text).map_err(|_| format!("{} is not valid UTF-8", path.display()))?;
    Ok(vec![FileText {
        path: path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string()),
        absolute,
        text,
    }])
}
