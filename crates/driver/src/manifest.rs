//! `ingot.toml` (spec section 33): a reader for the subset of TOML that section shows,
//! and the check of its keys.
//!
//! The subset is: comments, `key = value`, `[table]` and `[a.b]` headers, and the values
//! string, integer, boolean, array and inline table. Anything else is not valid TOML
//! for this purpose. Both a syntax error and an unknown or ill-typed key are E1701; a
//! target name that is not one of the three is E1702.

use ingot_diag::diagnostic::Diagnostic;
use ingot_diag::source::{Source, Span};
use ingot_sema::target::TARGETS;
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DepSource {
    Path(String),
    Git { url: String, hash: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Dependency {
    pub name: String,
    pub source: DepSource,
    pub span: Span,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Link {
    pub frameworks: Vec<String>,
    pub libs: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Manifest {
    pub name: String,
    pub targets: Vec<String>,
    pub deps: Vec<Dependency>,
    pub task_stack: Option<String>,
    pub link: BTreeMap<String, Link>,
}

#[derive(Clone, Debug)]
enum Value {
    Str(String),
    Int,
    Bool,
    Array(Vec<Spanned>),
    Table(Table),
}

#[derive(Clone, Debug)]
struct Spanned {
    value: Value,
    span: Span,
}

#[derive(Clone, Debug, Default)]
struct Table {
    entries: Vec<(String, Span, Spanned)>,
}

impl Table {
    fn get(&self, key: &str) -> Option<&(String, Span, Spanned)> {
        self.entries.iter().find(|(k, _, _)| k == key)
    }
}

struct Reader<'a> {
    text: &'a str,
    bytes: &'a [u8],
    pos: usize,
}

type ParseResult<T> = Result<T, (String, Span)>;

impl Reader<'_> {
    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn fail<T>(&self, message: &str) -> ParseResult<T> {
        let end = (self.pos + 1).min(self.bytes.len());
        Err((message.to_string(), Span::new(self.pos.min(end), end)))
    }

    /// Skip spaces, tabs and comments, but not newlines.
    fn skip_inline(&mut self) {
        while let Some(b) = self.peek() {
            match b {
                b' ' | b'\t' => self.pos += 1,
                b'#' => {
                    while self.peek().is_some_and(|c| c != b'\n') {
                        self.pos += 1;
                    }
                }
                _ => break,
            }
        }
    }

    /// Skip spaces, comments and newlines.
    fn skip_all(&mut self) {
        loop {
            self.skip_inline();
            match self.peek() {
                Some(b'\n') | Some(b'\r') => self.pos += 1,
                _ => break,
            }
        }
    }

    fn bare_key(&mut self) -> ParseResult<(String, Span)> {
        let start = self.pos;
        while self
            .peek()
            .is_some_and(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        {
            self.pos += 1;
        }
        if self.pos == start {
            return self.fail("expected a key");
        }
        Ok((
            self.text[start..self.pos].to_string(),
            Span::new(start, self.pos),
        ))
    }

    fn key(&mut self) -> ParseResult<(String, Span)> {
        if self.peek() == Some(b'"') {
            let (s, span) = self.string()?;
            Ok((s, span))
        } else {
            self.bare_key()
        }
    }

    fn string(&mut self) -> ParseResult<(String, Span)> {
        let start = self.pos;
        let quote = self.peek().expect("caller checked the quote");
        self.pos += 1;
        let mut out = String::new();
        loop {
            let Some(b) = self.peek() else {
                return Err((
                    "unterminated string".to_string(),
                    Span::new(start, start + 1),
                ));
            };
            match b {
                b'\n' => {
                    return Err((
                        "unterminated string".to_string(),
                        Span::new(start, start + 1),
                    ));
                }
                c if c == quote => {
                    self.pos += 1;
                    break;
                }
                b'\\' if quote == b'"' => {
                    self.pos += 1;
                    match self.peek() {
                        Some(b'n') => out.push('\n'),
                        Some(b't') => out.push('\t'),
                        Some(b'"') => out.push('"'),
                        Some(b'\\') => out.push('\\'),
                        _ => return self.fail("unknown escape in string"),
                    }
                    self.pos += 1;
                }
                _ => {
                    let ch = self.text[self.pos..]
                        .chars()
                        .next()
                        .expect("not at the end");
                    out.push(ch);
                    self.pos += ch.len_utf8();
                }
            }
        }
        Ok((out, Span::new(start, self.pos)))
    }

    fn value(&mut self) -> ParseResult<Spanned> {
        let start = self.pos;
        match self.peek() {
            Some(b'"') | Some(b'\'') => {
                let (s, span) = self.string()?;
                Ok(Spanned {
                    value: Value::Str(s),
                    span,
                })
            }
            Some(b'[') => {
                self.pos += 1;
                let mut items = Vec::new();
                loop {
                    self.skip_all();
                    if self.peek() == Some(b']') {
                        self.pos += 1;
                        break;
                    }
                    items.push(self.value()?);
                    self.skip_all();
                    match self.peek() {
                        Some(b',') => self.pos += 1,
                        Some(b']') => {
                            self.pos += 1;
                            break;
                        }
                        _ => return self.fail("expected `,` or `]` in an array"),
                    }
                }
                Ok(Spanned {
                    value: Value::Array(items),
                    span: Span::new(start, self.pos),
                })
            }
            Some(b'{') => {
                self.pos += 1;
                let mut table = Table::default();
                loop {
                    self.skip_inline();
                    if self.peek() == Some(b'}') {
                        self.pos += 1;
                        break;
                    }
                    let (key, key_span) = self.key()?;
                    self.skip_inline();
                    if self.peek() != Some(b'=') {
                        return self.fail("expected `=` after a key");
                    }
                    self.pos += 1;
                    self.skip_inline();
                    let value = self.value()?;
                    if table.get(&key).is_some() {
                        return Err((format!("duplicate key `{key}`"), key_span));
                    }
                    table.entries.push((key, key_span, value));
                    self.skip_inline();
                    match self.peek() {
                        Some(b',') => self.pos += 1,
                        Some(b'}') => {
                            self.pos += 1;
                            break;
                        }
                        _ => return self.fail("expected `,` or `}` in an inline table"),
                    }
                }
                Ok(Spanned {
                    value: Value::Table(table),
                    span: Span::new(start, self.pos),
                })
            }
            Some(b) if b.is_ascii_digit() || b == b'-' || b == b'+' => {
                let begin = self.pos;
                self.pos += 1;
                while self.peek().is_some_and(|c| c.is_ascii_digit() || c == b'_') {
                    self.pos += 1;
                }
                let text: String = self.text[begin..self.pos]
                    .chars()
                    .filter(|c| *c != '_')
                    .collect();
                match text.parse::<i64>() {
                    Ok(_) => Ok(Spanned {
                        value: Value::Int,
                        span: Span::new(begin, self.pos),
                    }),
                    Err(_) => Err(("malformed number".to_string(), Span::new(begin, self.pos))),
                }
            }
            Some(b) if b.is_ascii_alphabetic() => {
                let begin = self.pos;
                while self.peek().is_some_and(|c| c.is_ascii_alphabetic()) {
                    self.pos += 1;
                }
                let span = Span::new(begin, self.pos);
                match &self.text[begin..self.pos] {
                    "true" => Ok(Spanned {
                        value: Value::Bool,
                        span,
                    }),
                    "false" => Ok(Spanned {
                        value: Value::Bool,
                        span,
                    }),
                    _ => Err(("expected a value".to_string(), span)),
                }
            }
            _ => self.fail("expected a value"),
        }
    }
}

/// Read the document into a table of tables.
fn read_document(text: &str) -> ParseResult<Table> {
    let mut reader = Reader {
        text,
        bytes: text.as_bytes(),
        pos: 0,
    };
    let mut root = Table::default();
    // Path of the table that `key = value` lines currently go into.
    let mut current: Vec<String> = Vec::new();
    loop {
        reader.skip_all();
        let Some(b) = reader.peek() else {
            break;
        };
        if b == b'[' {
            reader.pos += 1;
            if reader.peek() == Some(b'[') {
                return reader.fail("arrays of tables are not supported");
            }
            let mut path = Vec::new();
            let mut spans = Vec::new();
            loop {
                reader.skip_inline();
                let (name, span) = reader.key()?;
                path.push(name);
                spans.push(span);
                reader.skip_inline();
                match reader.peek() {
                    Some(b'.') => reader.pos += 1,
                    Some(b']') => {
                        reader.pos += 1;
                        break;
                    }
                    _ => return reader.fail("expected `.` or `]` in a table header"),
                }
            }
            reader.skip_inline();
            if !matches!(reader.peek(), None | Some(b'\n') | Some(b'\r')) {
                return reader.fail("unexpected text after a table header");
            }
            ensure_table(&mut root, &path, &spans)?;
            current = path;
        } else {
            let (key, key_span) = reader.key()?;
            reader.skip_inline();
            if reader.peek() != Some(b'=') {
                return reader.fail("expected `=` after a key");
            }
            reader.pos += 1;
            reader.skip_inline();
            let value = reader.value()?;
            reader.skip_inline();
            if !matches!(reader.peek(), None | Some(b'\n') | Some(b'\r')) {
                return reader.fail("unexpected text after a value");
            }
            let table = table_at(&mut root, &current);
            if table.get(&key).is_some() {
                return Err((format!("duplicate key `{key}`"), key_span));
            }
            table.entries.push((key, key_span, value));
        }
    }
    Ok(root)
}

fn ensure_table(root: &mut Table, path: &[String], spans: &[Span]) -> ParseResult<()> {
    let mut table = root;
    for (i, name) in path.iter().enumerate() {
        let index = match table.entries.iter().position(|(k, _, _)| k == name) {
            Some(index) => index,
            None => {
                table.entries.push((
                    name.clone(),
                    spans[i],
                    Spanned {
                        value: Value::Table(Table::default()),
                        span: spans[i],
                    },
                ));
                table.entries.len() - 1
            }
        };
        match &mut table.entries[index].2.value {
            Value::Table(t) => table = t,
            _ => return Err((format!("`{name}` is not a table"), spans[i])),
        }
    }
    Ok(())
}

fn table_at<'t>(root: &'t mut Table, path: &[String]) -> &'t mut Table {
    let mut table = root;
    for name in path {
        let index = table
            .entries
            .iter()
            .position(|(k, _, _)| k == name)
            .expect("the header created this table");
        match &mut table.entries[index].2.value {
            Value::Table(t) => table = t,
            _ => unreachable!("ensure_table checked this"),
        }
    }
    table
}

struct Checker<'a> {
    source: &'a Source,
    diags: Vec<Diagnostic>,
}

impl Checker<'_> {
    fn error(&mut self, code: &'static str, message: String, span: Span) {
        self.diags
            .push(Diagnostic::new(code, message, self.source, span));
    }

    fn string(&mut self, key: &str, key_span: Span, v: &Spanned) -> Option<String> {
        match &v.value {
            Value::Str(s) => Some(s.clone()),
            other => {
                self.error(
                    "E1701",
                    format!("`{key}` must be a string, not {}", describe(other)),
                    key_span,
                );
                None
            }
        }
    }

    fn strings(&mut self, key: &str, key_span: Span, v: &Spanned) -> Option<Vec<(String, Span)>> {
        let Value::Array(items) = &v.value else {
            self.error(
                "E1701",
                format!(
                    "`{key}` must be an array of strings, not {}",
                    describe(&v.value)
                ),
                key_span,
            );
            return None;
        };
        let mut out = Vec::new();
        let mut ok = true;
        for item in items {
            match &item.value {
                Value::Str(s) => out.push((s.clone(), item.span)),
                other => {
                    self.error(
                        "E1701",
                        format!("`{key}` holds strings, not {}", describe(other)),
                        item.span,
                    );
                    ok = false;
                }
            }
        }
        ok.then_some(out)
    }

    fn table<'v>(&mut self, key: &str, key_span: Span, v: &'v Spanned) -> Option<&'v Table> {
        match &v.value {
            Value::Table(t) => Some(t),
            other => {
                self.error(
                    "E1701",
                    format!("`{key}` must be a table, not {}", describe(other)),
                    key_span,
                );
                None
            }
        }
    }

    fn unknown(&mut self, scope: &str, key: &str, span: Span) {
        let message = if scope.is_empty() {
            format!("unknown key `{key}` in `ingot.toml`")
        } else {
            format!("unknown key `{key}` in `[{scope}]`")
        };
        self.error("E1701", message, span);
    }

    fn target_name(&mut self, name: &str, span: Span) -> bool {
        if TARGETS.contains(&name) {
            true
        } else {
            self.error(
                "E1702",
                format!(
                    "`{name}` is not a target; the targets are {}",
                    TARGETS.join(", ")
                ),
                span,
            );
            false
        }
    }
}

fn describe(v: &Value) -> &'static str {
    match v {
        Value::Str(_) => "a string",
        Value::Int => "an integer",
        Value::Bool => "a boolean",
        Value::Array(_) => "an array",
        Value::Table(_) => "a table",
    }
}

/// True for a package or dependency name: a value identifier.
fn is_package_name(name: &str) -> bool {
    crate::loader::is_value_identifier(name)
}

/// Read and check a manifest. The diagnostics are E1701 and E1702; with any of them
/// the returned manifest must not be used.
pub fn parse(source: &Source) -> (Manifest, Vec<Diagnostic>) {
    let mut c = Checker {
        source,
        diags: Vec::new(),
    };
    let mut manifest = Manifest::default();
    let root = match read_document(&source.text) {
        Ok(t) => t,
        Err((message, span)) => {
            c.error("E1701", format!("not valid TOML: {message}"), span);
            return (manifest, c.diags);
        }
    };
    let mut name = None;
    for (key, key_span, value) in &root.entries {
        match key.as_str() {
            "name" => {
                if let Some(n) = c.string(key, *key_span, value) {
                    if is_package_name(&n) {
                        name = Some(n);
                    } else {
                        c.error(
                            "E1701",
                            format!("`{n}` is not a package name; use a lowercase identifier"),
                            value.span,
                        );
                    }
                }
            }
            "targets" => {
                if let Some(list) = c.strings(key, *key_span, value) {
                    for (t, span) in list {
                        if c.target_name(&t, span) {
                            manifest.targets.push(t);
                        }
                    }
                }
            }
            "deps" => {
                let Some(table) = c.table(key, *key_span, value) else {
                    continue;
                };
                for (dep_name, dep_span, dep) in &table.entries {
                    if !is_package_name(dep_name) {
                        c.error(
                            "E1701",
                            format!("`{dep_name}` is not a package name"),
                            *dep_span,
                        );
                        continue;
                    }
                    let Some(fields) = c.table(dep_name, *dep_span, dep) else {
                        continue;
                    };
                    let mut path = None;
                    let mut git = None;
                    let mut hash = None;
                    for (fkey, fspan, fvalue) in &fields.entries {
                        match fkey.as_str() {
                            "path" => path = c.string(fkey, *fspan, fvalue),
                            "git" => git = c.string(fkey, *fspan, fvalue),
                            "hash" => hash = c.string(fkey, *fspan, fvalue),
                            other => c.unknown("deps", other, *fspan),
                        }
                    }
                    let source = match (path, git, hash) {
                        (Some(p), None, None) => Some(DepSource::Path(p)),
                        (None, Some(url), Some(hash)) => Some(DepSource::Git { url, hash }),
                        (None, Some(_), None) => {
                            c.error(
                                "E1701",
                                format!("the git dependency `{dep_name}` needs a `hash`"),
                                *dep_span,
                            );
                            None
                        }
                        _ => {
                            c.error(
                                "E1701",
                                format!(
                                    "the dependency `{dep_name}` needs either `path` or `git` with `hash`"
                                ),
                                *dep_span,
                            );
                            None
                        }
                    };
                    if let Some(source) = source {
                        manifest.deps.push(Dependency {
                            name: dep_name.clone(),
                            source,
                            span: *dep_span,
                        });
                    }
                }
            }
            "runtime" => {
                let Some(table) = c.table(key, *key_span, value) else {
                    continue;
                };
                for (rkey, rspan, rvalue) in &table.entries {
                    match rkey.as_str() {
                        "task_stack" => manifest.task_stack = c.string(rkey, *rspan, rvalue),
                        other => c.unknown("runtime", other, *rspan),
                    }
                }
            }
            "link" => {
                let Some(table) = c.table(key, *key_span, value) else {
                    continue;
                };
                for (target, tspan, tvalue) in &table.entries {
                    let known = c.target_name(target, *tspan);
                    let Some(fields) = c.table(target, *tspan, tvalue) else {
                        continue;
                    };
                    let mut link = Link::default();
                    for (lkey, lspan, lvalue) in &fields.entries {
                        match lkey.as_str() {
                            "frameworks" => {
                                if let Some(v) = c.strings(lkey, *lspan, lvalue) {
                                    link.frameworks = v.into_iter().map(|(s, _)| s).collect();
                                }
                            }
                            "libs" => {
                                if let Some(v) = c.strings(lkey, *lspan, lvalue) {
                                    link.libs = v.into_iter().map(|(s, _)| s).collect();
                                }
                            }
                            other => c.unknown(&format!("link.{target}"), other, *lspan),
                        }
                    }
                    if known {
                        manifest.link.insert(target.clone(), link);
                    }
                }
            }
            other => c.unknown("", other, *key_span),
        }
    }
    match name {
        Some(n) => manifest.name = n,
        None => {
            if root.get("name").is_none() {
                c.error(
                    "E1701",
                    "`ingot.toml` has no `name`".to_string(),
                    Span::new(0, 0),
                );
            }
        }
    }
    (manifest, c.diags)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(text: &str) -> (Manifest, Vec<(String, usize)>) {
        let source = Source::new("ingot.toml", text);
        let (m, d) = parse(&source);
        let codes = d
            .iter()
            .map(|d| {
                (
                    d.code.to_string(),
                    source.position(d.span.start).line as usize,
                )
            })
            .collect();
        (m, codes)
    }

    #[test]
    fn the_manifest_of_section_33_is_accepted() {
        let (m, codes) = check(
            "# ingot.toml\n\
             name = \"linecount\"\n\
             targets = [\"aarch64-macos-m4\", \"x86_64-linux-znver4\", \"x86_64-windows-znver4\"]\n\
             \n\
             [deps]\n\
             csv = { path = \"../csv\" }\n\
             zstd = { git = \"https://example.com/zstd-ingot\", hash = \"b3:9f2c...\" }\n\
             \n\
             [runtime]\n\
             task_stack = \"1MiB\"\n\
             \n\
             [link.aarch64-macos-m4]\n\
             frameworks = [\"Metal\"]\n\
             \n\
             [link.x86_64-linux-znver4]\n\
             libs = [\"vulkan\"]\n\
             \n\
             [link.x86_64-windows-znver4]\n\
             libs = [\"vulkan-1\"]\n",
        );
        assert_eq!(codes, vec![]);
        assert_eq!(m.name, "linecount");
        assert_eq!(m.targets.len(), 3);
        assert_eq!(m.deps.len(), 2);
        assert_eq!(m.deps[0].source, DepSource::Path("../csv".to_string()));
        assert_eq!(m.task_stack.as_deref(), Some("1MiB"));
        assert_eq!(m.link["aarch64-macos-m4"].frameworks, vec!["Metal"]);
        assert_eq!(m.link["x86_64-windows-znver4"].libs, vec!["vulkan-1"]);
    }

    #[test]
    fn a_minimal_manifest_has_a_name() {
        let (m, codes) = check("name = \"app\"\n");
        assert_eq!(codes, vec![]);
        assert_eq!(m.name, "app");
    }

    #[test]
    fn syntax_errors_are_e1701() {
        for text in [
            "name = \n",
            "name \"app\"\n",
            "name = \"app\n",
            "[deps\n",
            "name = \"a\" extra\n",
            "name = \"a\"\nname = \"b\"\n",
            "name = 1.5\n",
        ] {
            let (_, codes) = check(text);
            assert!(
                codes.iter().any(|(c, _)| c == "E1701"),
                "{text:?} gave {codes:?}"
            );
        }
    }

    #[test]
    fn unknown_and_ill_typed_keys_are_e1701_on_the_key() {
        let (_, codes) = check("name = \"app\"\nversion = \"1\"\n");
        assert_eq!(codes, vec![("E1701".to_string(), 2)]);
        let (_, codes) = check("name = \"app\"\ntargets = \"aarch64-macos-m4\"\n");
        assert_eq!(codes, vec![("E1701".to_string(), 2)]);
        let (_, codes) = check("name = 3\n");
        assert_eq!(codes, vec![("E1701".to_string(), 1)]);
        let (_, codes) = check("name = \"app\"\n[runtime]\nstack = \"1MiB\"\n");
        assert_eq!(codes, vec![("E1701".to_string(), 3)]);
        let (_, codes) = check("name = \"app\"\n[deps]\ncsv = { pth = \"../csv\" }\n");
        assert!(codes.iter().any(|(c, l)| c == "E1701" && *l == 3));
    }

    #[test]
    fn a_missing_name_is_e1701_at_the_start() {
        let (_, codes) = check("targets = [\"aarch64-macos-m4\"]\n");
        assert_eq!(codes, vec![("E1701".to_string(), 1)]);
    }

    #[test]
    fn an_unknown_target_is_e1702_on_the_name() {
        let (_, codes) = check("name = \"app\"\ntargets = [\"aarch64-macos-m4\", \"riscv\"]\n");
        assert_eq!(codes, vec![("E1702".to_string(), 2)]);
        let (_, codes) = check("name = \"app\"\n[link.vax]\nlibs = []\n");
        assert_eq!(codes, vec![("E1702".to_string(), 2)]);
    }

    #[test]
    fn multi_line_arrays_and_comments_are_read() {
        let (m, codes) = check(
            "name = \"app\" # the package\ntargets = [\n  \"aarch64-macos-m4\", # first\n  \"x86_64-linux-znver4\",\n]\n",
        );
        assert_eq!(codes, vec![]);
        assert_eq!(m.targets.len(), 2);
    }
}
