//! `ingot explain`: the catalog row and the conformance tests of one code, both
//! embedded at build time (tools.md §2).

use crate::commands::Output;
use ingot_diag::diagnostic::json_string;
use std::time::Instant;

include!(concat!(env!("OUT_DIR"), "/reports_data.rs"));

#[derive(Clone, Debug)]
struct Row {
    code: String,
    name: String,
    condition: String,
    fix: Option<String>,
    retired: bool,
    trap: bool,
}

fn catalog() -> Vec<Row> {
    let mut rows = Vec::new();
    let mut header_seen = 0usize;
    for line in CATALOG.lines() {
        let trimmed = line.trim();
        if !trimmed.starts_with('|') {
            continue;
        }
        let cells = cells(trimmed);
        if cells.len() < 4 {
            continue;
        }
        let code = plain(&cells[0]);
        let trap = code.starts_with('T');
        if !is_code(&code) {
            continue;
        }
        // A table has one header row and one alignment row before the data rows.
        if cells[0].contains("Code") || cells[0].contains("---") {
            header_seen += 1;
            continue;
        }
        let name = plain(&cells[1]);
        let condition = plain(&cells[2]);
        let (condition, retired) = match condition.strip_prefix("retired:") {
            Some(rest) => (rest.trim().to_string(), true),
            None => (condition, false),
        };
        let fix = match cells.get(4) {
            Some(cell) if !trap => {
                let value = plain(cell);
                (value != "—" && !value.is_empty()).then_some(value)
            }
            _ => None,
        };
        rows.push(Row {
            code,
            name,
            condition,
            fix,
            retired,
            trap,
        });
    }
    let _ = header_seen;
    rows
}

/// Split a Markdown table row on unescaped `|`, dropping the empty edges.
fn cells(line: &str) -> Vec<String> {
    let mut cells = Vec::new();
    let mut cell = String::new();
    let mut escaped = false;
    for character in line.chars() {
        if escaped {
            cell.push(character);
            escaped = false;
            continue;
        }
        match character {
            '\\' => {
                escaped = true;
                cell.push(character);
            }
            '|' => {
                cells.push(cell.trim().to_string());
                cell.clear();
            }
            _ => cell.push(character),
        }
    }
    cells.push(cell.trim().to_string());
    if cells.first().is_some_and(|cell| cell.is_empty()) {
        cells.remove(0);
    }
    if cells.last().is_some_and(|cell| cell.is_empty()) {
        cells.pop();
    }
    cells
}

/// A table cell as plain text: backticks and emphasis removed, escapes resolved.
/// A code span keeps its text, so the `*` of T03 stays an operator.
fn plain(cell: &str) -> String {
    let mut out = String::new();
    let mut escaped = false;
    let mut code = false;
    for character in cell.chars() {
        if escaped {
            out.push(character);
            escaped = false;
            continue;
        }
        match character {
            '\\' => escaped = true,
            '`' => code = !code,
            '*' if !code => {}
            _ => out.push(character),
        }
    }
    out.trim().to_string()
}

fn is_code(value: &str) -> bool {
    let mut bytes = value.bytes();
    matches!(bytes.next(), Some(b'E') | Some(b'T'))
        && bytes.clone().count() > 0
        && bytes.all(|byte| byte.is_ascii_digit())
}

fn row(code: &str) -> Option<Row> {
    catalog().into_iter().find(|row| row.code == code)
}

fn sections(code: &str) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    for case in CASES {
        if case.codes.contains(&code) {
            for section in case.sections {
                if !found.iter().any(|item| item == section) {
                    found.push((*section).to_string());
                }
            }
        }
    }
    found.sort_by_key(|value| section_key(value));
    found
}

fn section_key(value: &str) -> Vec<u64> {
    value
        .split('.')
        .map(|part| part.parse().unwrap_or(u64::MAX))
        .collect()
}

/// The first reject test that marks this code and no other; for a trap, the first
/// trap test that marks it.
fn sample(code: &str, trap: bool) -> Option<&'static EmbeddedCase> {
    if trap {
        return CASES
            .iter()
            .find(|case| case.expect == "trap" && case.codes.contains(&code));
    }
    let single = |case: &&EmbeddedCase| {
        case.expect == "reject" && case.codes.len() == 1 && case.codes[0] == code
    };
    CASES
        .iter()
        .find(|case| single(case) && case.fixed.is_some())
        .or_else(|| CASES.iter().find(single))
}

/// Drop the `//!` directive lines of a conformance test.
fn without_directives(text: &str) -> String {
    let mut out = String::new();
    for line in text.lines() {
        if line.trim_start().starts_with("//!") {
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

fn line(row: &Row) -> String {
    let mut out = String::new();
    out.push_str("{\"type\":\"explain\",\"code\":");
    json_string(&mut out, &row.code);
    out.push_str(",\"name\":");
    json_string(&mut out, &row.name);
    out.push_str(",\"condition\":");
    json_string(&mut out, &row.condition);
    if row.retired {
        out.push_str(",\"retired\":true");
    }
    let sections = sections(&row.code);
    out.push_str(",\"sections\":[");
    for (i, section) in sections.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        json_string(&mut out, section);
    }
    out.push(']');
    if let Some(case) = sample(&row.code, row.trap) {
        out.push_str(",\"bad\":");
        json_string(&mut out, &without_directives(case.text));
        if let Some(fixed) = case.fixed {
            out.push_str(",\"good\":");
            json_string(&mut out, &without_directives(fixed));
        }
    }
    if let Some(fix) = &row.fix {
        out.push_str(",\"fix\":");
        json_string(&mut out, fix);
    }
    out.push('}');
    out
}

fn human(row: &Row) -> String {
    let mut out = format!("{} {}\n", row.code, row.name);
    out.push_str(&format!("  {}\n", row.condition));
    let sections = sections(&row.code);
    if !sections.is_empty() {
        out.push_str(&format!("  sections: {}\n", sections.join(", ")));
    }
    if row.retired {
        out.push_str("  retired\n");
    }
    if let Some(fix) = &row.fix {
        out.push_str(&format!("  fix: {fix}\n"));
    }
    if let Some(case) = sample(&row.code, row.trap) {
        out.push_str(&format!("  test: {}\n", case.path));
    }
    out
}

fn summary(ok: bool, started: Instant) -> String {
    format!(
        "{{\"type\":\"summary\",\"command\":\"explain\",\"ok\":{ok},\"protocol\":1,\"ms\":{}}}",
        started.elapsed().as_millis()
    )
}

pub fn explain(code: &str, started: Instant) -> Output {
    let Some(row) = row(code) else {
        return crate::commands::failure(
            "explain",
            format!("`{code}` is not a code in the catalog"),
            started,
        );
    };
    let human = human(&row);
    Output {
        lines: vec![line(&row), summary(true, started)],
        human,
        error: None,
        not_implemented: Vec::new(),
        exit: 0,
    }
}

pub fn explain_all(started: Instant) -> Output {
    let rows = catalog();
    let mut lines = Vec::with_capacity(rows.len() + 1);
    let mut text = String::new();
    for row in &rows {
        lines.push(line(row));
        text.push_str(&human(row));
    }
    lines.push(summary(true, started));
    Output {
        lines,
        human: text,
        error: None,
        not_implemented: Vec::new(),
        exit: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_known_code_has_a_catalog_row_with_its_name() {
        let rows = catalog();
        for (code, name) in ingot_diag::diagnostic::CODE_NAMES {
            let row = rows
                .iter()
                .find(|row| row.code == *code)
                .unwrap_or_else(|| panic!("`{code}` has no row in diagnostics.md"));
            assert_eq!(row.name, *name, "`{code}` has another name in the catalog");
        }
    }

    #[test]
    fn the_catalog_has_the_retired_and_trap_rows() {
        let rows = catalog();
        assert!(rows.iter().any(|row| row.code == "E0512" && row.retired));
        assert!(rows.iter().any(|row| row.code == "T01" && row.trap));
    }

    #[test]
    fn sections_sort_as_numbers() {
        let mut values = vec!["33.2".to_string(), "10".to_string(), "2".to_string()];
        values.sort_by_key(|value| section_key(value));
        assert_eq!(values, ["2", "10", "33.2"]);
    }
}
