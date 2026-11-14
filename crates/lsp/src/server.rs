//! The LSP server state: lifecycle, open documents as overlays, checks through
//! `ingot serve`, diagnostics, positions and code actions.

use crate::position::{self, Encoding};
use crate::uri;
use ingot_diag::json::Json;
use ingot_driver::{files, loader, serve};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
struct Document {
    /// Normalised absolute path: the key that span paths are matched with.
    path: PathBuf,
    text: String,
}

#[derive(Clone, Debug)]
struct StoredEdit {
    file: PathBuf,
    start: usize,
    end: usize,
    text: String,
}

#[derive(Clone, Debug)]
struct StoredFix {
    message: String,
    edits: Vec<StoredEdit>,
}

#[derive(Clone, Debug)]
struct StoredDiagnostic {
    /// The published LSP diagnostic, returned in a code action's `diagnostics`.
    lsp: Json,
    start: usize,
    end: usize,
    fixes: Vec<StoredFix>,
}

#[derive(Clone, Debug)]
struct CheckTarget {
    /// The serve `path`: the package directory, or a file with no manifest.
    path: PathBuf,
    /// The package key used for state and published sets.
    key: PathBuf,
    /// The directory that span paths are relative to.
    root: PathBuf,
}

pub struct Server {
    initialized: bool,
    shutdown: bool,
    encoding: Encoding,
    open: BTreeMap<String, Document>,
    compiler: serve::Server,
    next_compiler_id: u64,
    outbox: Vec<Json>,
    /// Per package, the URIs that were published (so stale lists can be cleared).
    published: BTreeMap<PathBuf, BTreeSet<String>>,
    /// Per document URI, the diagnostics of its last publish, for code actions.
    last_diagnostics: BTreeMap<String, Vec<StoredDiagnostic>>,
    logged_not_implemented: BTreeMap<PathBuf, Vec<String>>,
    shown_errors: BTreeMap<PathBuf, BTreeSet<String>>,
    std_root: Option<PathBuf>,
    debug_checks: bool,
}

impl Server {
    pub fn new() -> Self {
        Server {
            initialized: false,
            shutdown: false,
            encoding: Encoding::Utf16,
            open: BTreeMap::new(),
            compiler: serve::Server::new(),
            next_compiler_id: 0,
            outbox: Vec::new(),
            published: BTreeMap::new(),
            last_diagnostics: BTreeMap::new(),
            logged_not_implemented: BTreeMap::new(),
            shown_errors: BTreeMap::new(),
            std_root: loader::std_root().map(|root| files::normalize(&root)),
            debug_checks: std::env::var_os("INGOT_LSP_DEBUG_CHECKS").is_some(),
        }
    }

    /// Messages produced since the last call, in order.
    pub fn take_outbox(&mut self) -> Vec<Json> {
        std::mem::take(&mut self.outbox)
    }

    /// A `window/logMessage` (type 1 = error, 4 = log).
    pub fn log(&mut self, kind: i64, message: &str) {
        self.outbox.push(notification(
            "window/logMessage",
            object(vec![("type", number(kind)), ("message", string(message))]),
        ));
    }

    /// Apply a batch: notifications in order, then one check per touched package, then
    /// the code-action requests. Returns an exit status when `exit` arrived.
    pub fn handle_batch(&mut self, messages: &[String]) -> Option<u8> {
        let mut touched: BTreeMap<PathBuf, CheckTarget> = BTreeMap::new();
        let mut code_actions = Vec::new();
        for text in messages {
            let value = match ingot_diag::json::parse(text) {
                Ok(value) => value,
                Err(error) => {
                    self.log(1, &format!("invalid JSON message: {error}"));
                    continue;
                }
            };
            let Json::Object(members) = &value else {
                self.log(1, "a message is not a JSON object");
                continue;
            };
            let Some(method) = member(members, "method").and_then(Json::as_str) else {
                self.log(1, "a message has no `method`");
                continue;
            };
            let method = method.to_string();
            let params = member(members, "params").cloned().unwrap_or(Json::Null);
            let id = member(members, "id").cloned();
            if method == "exit" {
                return Some(if self.shutdown { 0 } else { 1 });
            }
            match id {
                Some(id) => self.handle_request(&method, id, &params, &mut code_actions),
                None => self.handle_notification(&method, &params, &mut touched),
            }
        }
        // A change can matter to every package that sees the changed file: editing `std`
        // changes what each open package imports. A check is cheap, so every package
        // with an open document is checked again too (LSP.md, "Change handling").
        if !touched.is_empty() {
            let files = self.current_files();
            let paths: Vec<PathBuf> = self.open.values().map(|d| d.path.clone()).collect();
            for path in paths {
                let target = check_target(&path, &files);
                touched.entry(target.key.clone()).or_insert(target);
            }
        }
        for (_, target) in touched {
            self.check(&target);
        }
        for (id, params) in code_actions {
            self.code_action(id, &params);
        }
        None
    }

    fn handle_request(
        &mut self,
        method: &str,
        id: Json,
        params: &Json,
        code_actions: &mut Vec<(Json, Json)>,
    ) {
        if self.shutdown {
            self.error(id, -32600, "the server has been shut down");
            return;
        }
        if method == "initialize" {
            self.initialize(id, params);
            return;
        }
        if !self.initialized {
            self.error(id, -32002, "the server is not initialized");
            return;
        }
        match method {
            "shutdown" => {
                self.shutdown = true;
                self.result(id, Json::Null);
            }
            "textDocument/codeAction" => code_actions.push((id, params.clone())),
            other => self.error(id, -32601, &format!("unknown request `{other}`")),
        }
    }

    fn handle_notification(
        &mut self,
        method: &str,
        params: &Json,
        touched: &mut BTreeMap<PathBuf, CheckTarget>,
    ) {
        if self.shutdown || !self.initialized {
            return;
        }
        match method {
            "textDocument/didOpen" => self.did_open(params, touched),
            "textDocument/didChange" => self.did_change(params, touched),
            "textDocument/didClose" => self.did_close(params, touched),
            // `initialized`, `$/cancelRequest` and everything unknown: nothing to do.
            _ => {}
        }
    }

    fn initialize(&mut self, id: Json, params: &Json) {
        if self.initialized {
            self.error(id, -32600, "the server is already initialized");
            return;
        }
        self.initialized = true;
        if client_offers_utf8(params) {
            self.encoding = Encoding::Utf8;
        }
        let capabilities = object(vec![
            (
                "textDocumentSync",
                object(vec![("openClose", Json::Bool(true)), ("change", number(1))]),
            ),
            (
                "codeActionProvider",
                object(vec![(
                    "codeActionKinds",
                    Json::Array(vec![string("quickfix")]),
                )]),
            ),
            ("positionEncoding", string(self.encoding.name())),
        ]);
        self.result(
            id,
            object(vec![
                ("capabilities", capabilities),
                (
                    "serverInfo",
                    object(vec![
                        ("name", string("ingot-lsp")),
                        ("version", string(env!("CARGO_PKG_VERSION"))),
                    ]),
                ),
            ]),
        );
    }

    fn did_open(&mut self, params: &Json, touched: &mut BTreeMap<PathBuf, CheckTarget>) {
        let Some(document) = params.get("textDocument") else {
            return;
        };
        let Some(uri) = document.get("uri").and_then(Json::as_str) else {
            return;
        };
        let Some(text) = document.get("text").and_then(Json::as_str) else {
            return;
        };
        let Some(path) = uri::from_uri(uri) else {
            self.log(1, &format!("unsupported document URI `{uri}`"));
            return;
        };
        let path = files::normalize(&path);
        let text = text.to_string();
        self.open.insert(
            uri.to_string(),
            Document {
                path: path.clone(),
                text: text.clone(),
            },
        );
        self.send_overlay(&path, Some(&text));
        self.touch(path, touched);
    }

    fn did_change(&mut self, params: &Json, touched: &mut BTreeMap<PathBuf, CheckTarget>) {
        let Some(uri) = params
            .get("textDocument")
            .and_then(|document| document.get("uri"))
            .and_then(Json::as_str)
        else {
            return;
        };
        let Some(Json::Array(changes)) = params.get("contentChanges") else {
            return;
        };
        let Some(text) = changes
            .last()
            .and_then(|change| change.get("text"))
            .and_then(Json::as_str)
        else {
            return;
        };
        let Some(document) = self.open.get_mut(uri) else {
            self.log(
                1,
                &format!("didChange for a document that is not open: `{uri}`"),
            );
            return;
        };
        document.text = text.to_string();
        let path = document.path.clone();
        self.send_overlay(&path, Some(text));
        self.touch(path, touched);
    }

    fn did_close(&mut self, params: &Json, touched: &mut BTreeMap<PathBuf, CheckTarget>) {
        let Some(uri) = params
            .get("textDocument")
            .and_then(|document| document.get("uri"))
            .and_then(Json::as_str)
        else {
            return;
        };
        let Some(document) = self.open.remove(uri) else {
            return;
        };
        self.send_overlay(&document.path, None);
        self.touch(document.path, touched);
    }

    fn send_overlay(&mut self, path: &Path, text: Option<&str>) {
        let file = path.to_string_lossy().into_owned();
        let text = match text {
            Some(text) => string(text),
            None => Json::Null,
        };
        let lines = self.compiler_request(
            "overlay",
            vec![
                ("file".to_string(), string(&file)),
                ("text".to_string(), text),
            ],
        );
        for line in lines {
            if line.get("type").and_then(Json::as_str) == Some("error") {
                let message = line
                    .get("message")
                    .and_then(Json::as_str)
                    .unwrap_or("overlay failed");
                self.log(1, message);
            }
        }
    }

    fn compiler_request(&mut self, command: &str, args: Vec<(String, Json)>) -> Vec<Json> {
        self.next_compiler_id += 1;
        let request = object(vec![
            ("id", number(self.next_compiler_id)),
            ("command", string(command)),
            ("args", Json::Object(args)),
        ]);
        self.compiler
            .handle(&request.to_text())
            .iter()
            .filter_map(|line| ingot_diag::json::parse(line).ok())
            .collect()
    }

    fn current_files(&self) -> files::Files {
        let mut overlays = files::Overlays::new();
        for document in self.open.values() {
            overlays.insert(&document.path, document.text.clone());
        }
        files::Files::new(overlays)
    }

    fn touch(&mut self, path: PathBuf, touched: &mut BTreeMap<PathBuf, CheckTarget>) {
        let files = self.current_files();
        let target = check_target(&path, &files);
        touched.entry(target.key.clone()).or_insert(target);
    }

    fn check(&mut self, target: &CheckTarget) {
        if self.debug_checks {
            self.log(4, &format!("check {}", target.path.display()));
        }
        let path = target.path.to_string_lossy().into_owned();
        let lines = self.compiler_request("check", vec![("path".to_string(), string(&path))]);
        self.publish(target, &lines);
    }

    fn publish(&mut self, target: &CheckTarget, lines: &[Json]) {
        let mut summary = None;
        let mut error = None;
        let mut grouped: BTreeMap<String, (Vec<Json>, Vec<StoredDiagnostic>)> = BTreeMap::new();
        for line in lines {
            match line.get("type").and_then(Json::as_str) {
                Some("diagnostic") => {
                    if let Some((uri, lsp, stored)) = self.lsp_diagnostic(line, target) {
                        let entry = grouped.entry(uri).or_default();
                        entry.0.push(lsp);
                        entry.1.push(stored);
                    }
                }
                Some("summary") => summary = Some(line.clone()),
                Some("error") => {
                    error = line
                        .get("message")
                        .and_then(Json::as_str)
                        .map(str::to_string);
                }
                _ => {}
            }
        }
        if let Some(message) = error {
            let shown = self.shown_errors.entry(target.key.clone()).or_default();
            if shown.insert(message.clone()) {
                self.show_message(&message);
            }
            return;
        }
        self.shown_errors.remove(&target.key);

        if let Some(summary) = summary {
            let list: Vec<String> = match summary.get("not_implemented") {
                Some(Json::Array(items)) => items
                    .iter()
                    .filter_map(Json::as_str)
                    .map(str::to_string)
                    .collect(),
                _ => Vec::new(),
            };
            let previous = self
                .logged_not_implemented
                .get(&target.key)
                .cloned()
                .unwrap_or_default();
            if list != previous {
                let message = if list.is_empty() {
                    "not implemented: none".to_string()
                } else {
                    format!("not implemented: {}", list.join("; "))
                };
                self.log(4, &message);
                self.logged_not_implemented.insert(target.key.clone(), list);
            }
        }

        let previous = self.published.get(&target.key).cloned().unwrap_or_default();
        let files = self.current_files();
        let mut open_uris = BTreeSet::new();
        for (uri, document) in &self.open {
            if check_target(&document.path, &files).key == target.key {
                open_uris.insert(uri.clone());
            }
        }
        let mut all: BTreeSet<String> = grouped.keys().cloned().collect();
        all.extend(previous);
        all.extend(open_uris);
        let mut published = BTreeSet::new();
        for uri in all {
            let (diagnostics, stored) = grouped.remove(&uri).unwrap_or_default();
            self.publish_uri(&uri, diagnostics);
            self.last_diagnostics.insert(uri.clone(), stored);
            published.insert(uri);
        }
        self.published.insert(target.key.clone(), published);
    }

    fn lsp_diagnostic(
        &mut self,
        diagnostic: &Json,
        target: &CheckTarget,
    ) -> Option<(String, Json, StoredDiagnostic)> {
        let span = diagnostic.get("span")?;
        let file = span.get("file").and_then(Json::as_str)?;
        let path = match self.place(file, target) {
            Some(path) => path,
            None => {
                self.log(1, &format!("cannot place a diagnostic in `{file}`"));
                return None;
            }
        };
        let Some(text) = self.text_for_path(&path) else {
            self.log(
                1,
                &format!("cannot read `{}` for a diagnostic", path.display()),
            );
            return None;
        };
        let Some((start, end)) = offsets(span) else {
            self.log(1, "a diagnostic has no byte offsets");
            return None;
        };
        let range = range_for(&text, start, end, self.encoding);
        let code = diagnostic.get("code").and_then(Json::as_str).unwrap_or("");
        let message = diagnostic
            .get("message")
            .and_then(Json::as_str)
            .unwrap_or("");
        let mut members = vec![
            ("range", range),
            ("severity", number(1)),
            ("code", string(code)),
            ("source", string("ingot")),
            ("message", string(message)),
        ];
        let mut related = Vec::new();
        if let Some(Json::Array(items)) = diagnostic.get("related") {
            for item in items {
                let role = item.get("role").and_then(Json::as_str).unwrap_or("");
                let Some(span) = item.get("span") else {
                    continue;
                };
                let Some(file) = span.get("file").and_then(Json::as_str) else {
                    continue;
                };
                let Some(path) = self.place(file, target) else {
                    self.log(1, &format!("cannot place related information in `{file}`"));
                    continue;
                };
                let Some(text) = self.text_for_path(&path) else {
                    continue;
                };
                let Some((start, end)) = offsets(span) else {
                    continue;
                };
                related.push(object(vec![
                    (
                        "location",
                        object(vec![
                            ("uri", string(&self.uri_for_path(&path))),
                            ("range", range_for(&text, start, end, self.encoding)),
                        ]),
                    ),
                    ("message", string(role)),
                ]));
            }
        }
        if !related.is_empty() {
            members.push(("relatedInformation", Json::Array(related)));
        }
        let lsp = object(members);
        let mut fixes = Vec::new();
        if let Some(Json::Array(items)) = diagnostic.get("fixes") {
            for fix in items {
                let message = fix
                    .get("message")
                    .and_then(Json::as_str)
                    .unwrap_or("fix")
                    .to_string();
                let Some(Json::Array(edits)) = fix.get("edits") else {
                    continue;
                };
                let mut stored = Vec::new();
                let mut usable = true;
                for edit in edits {
                    let Some(span) = edit.get("span") else {
                        usable = false;
                        break;
                    };
                    let Some(file) = span.get("file").and_then(Json::as_str) else {
                        usable = false;
                        break;
                    };
                    let Some(path) = self.place(file, target) else {
                        usable = false;
                        break;
                    };
                    let Some((start, end)) = offsets(span) else {
                        usable = false;
                        break;
                    };
                    stored.push(StoredEdit {
                        file: path,
                        start,
                        end,
                        text: edit
                            .get("text")
                            .and_then(Json::as_str)
                            .unwrap_or("")
                            .to_string(),
                    });
                }
                if usable {
                    fixes.push(StoredFix {
                        message,
                        edits: stored,
                    });
                }
            }
        }
        let stored = StoredDiagnostic {
            lsp: lsp.clone(),
            start,
            end,
            fixes,
        };
        Some((self.uri_for_path(&path), lsp, stored))
    }

    fn code_action(&mut self, id: Json, params: &Json) {
        let mut actions = Vec::new();
        if let Some(uri) = params
            .get("textDocument")
            .and_then(|document| document.get("uri"))
            .and_then(Json::as_str)
        {
            let text = self
                .open
                .get(uri)
                .map(|document| document.text.clone())
                .or_else(|| uri::from_uri(uri).and_then(|path| std::fs::read_to_string(path).ok()));
            if let (Some(text), Some(range)) = (text, params.get("range")) {
                let start = point_offset(range.get("start"), &text, self.encoding);
                let end = point_offset(range.get("end"), &text, self.encoding);
                let stored = self.last_diagnostics.get(uri).cloned().unwrap_or_default();
                for diagnostic in stored {
                    if !intersects((diagnostic.start, diagnostic.end), (start, end)) {
                        continue;
                    }
                    for fix in &diagnostic.fixes {
                        let mut changes: BTreeMap<String, Vec<Json>> = BTreeMap::new();
                        let mut usable = true;
                        for edit in &fix.edits {
                            let Some(text) = self.text_for_path(&edit.file) else {
                                usable = false;
                                break;
                            };
                            let range = range_for(&text, edit.start, edit.end, self.encoding);
                            changes
                                .entry(self.uri_for_path(&edit.file))
                                .or_default()
                                .push(object(vec![
                                    ("range", range),
                                    ("newText", string(&edit.text)),
                                ]));
                        }
                        if !usable {
                            continue;
                        }
                        let changes = Json::Object(
                            changes
                                .into_iter()
                                .map(|(uri, edits)| (uri, Json::Array(edits)))
                                .collect(),
                        );
                        actions.push(object(vec![
                            ("title", string(&fix.message)),
                            ("kind", string("quickfix")),
                            ("diagnostics", Json::Array(vec![diagnostic.lsp.clone()])),
                            ("edit", object(vec![("changes", changes)])),
                        ]));
                    }
                }
            }
        }
        self.result(id, Json::Array(actions));
    }

    fn text_for_path(&self, path: &Path) -> Option<String> {
        if let Some(document) = self.open.values().find(|document| document.path == path) {
            return Some(document.text.clone());
        }
        std::fs::read_to_string(path).ok()
    }

    fn uri_for_path(&self, path: &Path) -> String {
        if let Some((uri, _)) = self.open.iter().find(|(_, document)| document.path == path) {
            return uri.clone();
        }
        uri::to_uri(path)
    }

    fn place(&self, file: &str, target: &CheckTarget) -> Option<PathBuf> {
        if file.starts_with('<') {
            return None;
        }
        if let Some(rest) = file.strip_prefix("std/") {
            let root = self.std_root.as_ref()?;
            if root == &target.root {
                return Some(target.root.join(rest));
            }
            return Some(root.join(rest));
        }
        Some(target.root.join(file))
    }

    fn show_message(&mut self, message: &str) {
        self.outbox.push(notification(
            "window/showMessage",
            object(vec![("type", number(1)), ("message", string(message))]),
        ));
    }

    fn result(&mut self, id: Json, result: Json) {
        self.outbox.push(response(id, result));
    }

    fn error(&mut self, id: Json, code: i64, message: &str) {
        self.outbox.push(object(vec![
            ("jsonrpc", string("2.0")),
            ("id", id),
            (
                "error",
                object(vec![("code", number(code)), ("message", string(message))]),
            ),
        ]));
    }

    fn publish_uri(&mut self, uri: &str, diagnostics: Vec<Json>) {
        self.outbox.push(notification(
            "textDocument/publishDiagnostics",
            object(vec![
                ("uri", string(uri)),
                ("diagnostics", Json::Array(diagnostics)),
            ]),
        ));
    }
}

impl Default for Server {
    fn default() -> Self {
        Server::new()
    }
}

fn check_target(path: &Path, files: &files::Files) -> CheckTarget {
    if let Some(dir) = loader::manifest_dir(path, files) {
        let root = files::normalize(&dir);
        CheckTarget {
            path: root.clone(),
            key: root.clone(),
            root,
        }
    } else {
        let path = files::normalize(path);
        let root = path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("/"));
        CheckTarget {
            path: path.clone(),
            key: path,
            root,
        }
    }
}

fn client_offers_utf8(params: &Json) -> bool {
    params
        .get("capabilities")
        .and_then(|capabilities| capabilities.get("general"))
        .and_then(|general| general.get("positionEncodings"))
        .and_then(|encodings| match encodings {
            Json::Array(items) => Some(items.iter().any(|item| item.as_str() == Some("utf-8"))),
            _ => None,
        })
        .unwrap_or(false)
}

fn member<'a>(members: &'a [(String, Json)], name: &str) -> Option<&'a Json> {
    members
        .iter()
        .rev()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value)
}

fn offsets(span: &Json) -> Option<(usize, usize)> {
    let start = span.get("start")?.get("offset")?.as_i64()?;
    let end = span.get("end")?.get("offset")?.as_i64()?;
    (start >= 0 && end >= 0).then_some((start as usize, end as usize))
}

fn range_for(text: &str, start: usize, end: usize, encoding: Encoding) -> Json {
    let (start_line, start_character) = position::offset_to_position(text, start, encoding);
    let (end_line, end_character) = position::offset_to_position(text, end, encoding);
    object(vec![
        (
            "start",
            object(vec![
                ("line", number(start_line)),
                ("character", number(start_character)),
            ]),
        ),
        (
            "end",
            object(vec![
                ("line", number(end_line)),
                ("character", number(end_character)),
            ]),
        ),
    ])
}

fn point_offset(point: Option<&Json>, text: &str, encoding: Encoding) -> usize {
    let Some(point) = point else {
        return 0;
    };
    let line = point.get("line").and_then(Json::as_i64).unwrap_or(0).max(0) as u32;
    let character = point
        .get("character")
        .and_then(Json::as_i64)
        .unwrap_or(0)
        .max(0) as u32;
    position::position_to_offset(text, line, character, encoding)
}

fn intersects(a: (usize, usize), b: (usize, usize)) -> bool {
    let (a_start, a_end) = a;
    let (b_start, b_end) = b;
    if a_start == a_end || b_start == b_end {
        a_start <= b_end && b_start <= a_end
    } else {
        a_start < b_end && b_start < a_end
    }
}

fn notification(method: &str, params: Json) -> Json {
    object(vec![
        ("jsonrpc", string("2.0")),
        ("method", string(method)),
        ("params", params),
    ])
}

fn response(id: Json, result: Json) -> Json {
    object(vec![
        ("jsonrpc", string("2.0")),
        ("id", id),
        ("result", result),
    ])
}

fn object(members: Vec<(&str, Json)>) -> Json {
    Json::Object(
        members
            .into_iter()
            .map(|(key, value)| (key.to_string(), value))
            .collect(),
    )
}

fn string(value: &str) -> Json {
    Json::String(value.to_string())
}

fn number(value: impl ToString) -> Json {
    Json::Number(value.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(id: i64, method: &str, params: Json) -> String {
        object(vec![
            ("jsonrpc", string("2.0")),
            ("id", number(id)),
            ("method", string(method)),
            ("params", params),
        ])
        .to_text()
    }

    fn notification_text(method: &str, params: Json) -> String {
        notification(method, params).to_text()
    }

    fn server() -> Server {
        let mut server = Server::new();
        let init = request(1, "initialize", object(vec![]));
        assert!(server.handle_batch(&[init]).is_none());
        let out = server.take_outbox();
        assert!(
            out.iter()
                .any(|m| m.get("id").and_then(Json::as_i64) == Some(1))
        );
        server
    }

    fn did_open(uri: &str, text: &str) -> String {
        notification_text(
            "textDocument/didOpen",
            object(vec![(
                "textDocument",
                object(vec![
                    ("uri", string(uri)),
                    ("languageId", string("ingot")),
                    ("version", number(1)),
                    ("text", string(text)),
                ]),
            )]),
        )
    }

    fn did_change(uri: &str, text: &str) -> String {
        notification_text(
            "textDocument/didChange",
            object(vec![
                (
                    "textDocument",
                    object(vec![("uri", string(uri)), ("version", number(2))]),
                ),
                (
                    "contentChanges",
                    Json::Array(vec![object(vec![("text", string(text))])]),
                ),
            ]),
        )
    }

    #[test]
    fn positions_and_intersections() {
        assert!(intersects((0, 5), (5, 5)));
        assert!(intersects((0, 5), (4, 9)));
        assert!(!intersects((0, 5), (6, 9)));
        assert!(!intersects((0, 5), (5, 9)));
    }

    #[test]
    fn a_batch_of_changes_runs_one_check() {
        let dir = std::env::temp_dir().join(format!("ingot-lsp-batch-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("ingot.toml"), "name = \"app\"\n").unwrap();
        std::fs::write(dir.join("main.ingot"), "// nothing\n").unwrap();
        let path = files::normalize(&dir.join("main.ingot"));
        let uri = uri::to_uri(&path);
        let mut server = server();
        server.debug_checks = true;
        server.handle_batch(&[
            did_open(&uri, "// nothing\n"),
            did_change(&uri, "// a\n"),
            did_change(&uri, "// b\n"),
            did_change(&uri, "// c\n"),
        ]);
        let out = server.take_outbox();
        let checks = out
            .iter()
            .filter(|message| {
                message.get("method").and_then(Json::as_str) == Some("window/logMessage")
                    && message
                        .get("params")
                        .and_then(|params| params.get("message"))
                        .and_then(Json::as_str)
                        .is_some_and(|message| message.starts_with("check "))
            })
            .count();
        assert_eq!(checks, 1, "{out:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
