#![allow(dead_code)]

//! Spawning `ingot-lsp` and speaking framed LSP to it, with a timeout so a hung server
//! fails a test instead of hanging the suite.

use ingot_lsp::Json;
use ingot_lsp::json::parse;
use ingot_lsp::transport::{Frame, read_frame};
use std::io::{BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

pub const TIMEOUT: Duration = Duration::from_secs(20);

pub fn obj(members: Vec<(&str, Json)>) -> Json {
    Json::Object(
        members
            .into_iter()
            .map(|(key, value)| (key.to_string(), value))
            .collect(),
    )
}

pub fn s(value: &str) -> Json {
    Json::String(value.to_string())
}

pub fn n(value: impl ToString) -> Json {
    Json::Number(value.to_string())
}

pub fn arr(items: Vec<Json>) -> Json {
    Json::Array(items)
}

pub fn request(id: i64, method: &str, params: Json) -> Json {
    obj(vec![
        ("jsonrpc", s("2.0")),
        ("id", n(id)),
        ("method", s(method)),
        ("params", params),
    ])
}

pub fn notification(method: &str, params: Json) -> Json {
    obj(vec![
        ("jsonrpc", s("2.0")),
        ("method", s(method)),
        ("params", params),
    ])
}

pub fn scratch(label: &str) -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "ingot-lsp-test-{}-{label}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir.canonicalize().unwrap()
}

pub fn package(label: &str, manifest: &str, files: &[(&str, &str)]) -> PathBuf {
    let dir = scratch(label);
    std::fs::write(dir.join("ingot.toml"), manifest).unwrap();
    for (name, text) in files {
        let path = dir.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, text).unwrap();
    }
    dir
}

pub fn uri(path: &Path) -> String {
    ingot_lsp::uri::to_uri(path)
}

pub struct Client {
    child: Child,
    stdin: ChildStdin,
    messages: Receiver<String>,
    next_id: i64,
    pending: Vec<Json>,
}

impl Client {
    pub fn spawn(envs: &[(&str, &str)]) -> Client {
        let mut command = Command::new(env!("CARGO_BIN_EXE_ingot-lsp"));
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        for (key, value) in envs {
            command.env(key, value);
        }
        let mut child = command.spawn().unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                match read_frame(&mut reader) {
                    Some(Frame::Message(message)) => {
                        if sender.send(message).is_err() {
                            return;
                        }
                    }
                    Some(Frame::Malformed { fatal, .. }) => {
                        if fatal {
                            return;
                        }
                    }
                    None => return,
                }
            }
        });
        Client {
            child,
            stdin,
            messages: receiver,
            next_id: 0,
            pending: Vec::new(),
        }
    }

    pub fn send(&mut self, message: &Json) {
        self.send_all(std::slice::from_ref(message));
    }

    pub fn send_all(&mut self, messages: &[Json]) {
        let mut buffer = String::new();
        for message in messages {
            let body = message.to_text();
            buffer.push_str(&format!("Content-Length: {}\r\n\r\n{body}", body.len()));
        }
        self.stdin.write_all(buffer.as_bytes()).unwrap();
        self.stdin.flush().unwrap();
    }

    /// A frame whose body is raw bytes, for malformed-UTF-8 tests.
    pub fn send_frame_bytes(&mut self, body: &[u8]) {
        write!(self.stdin, "Content-Length: {}\r\n\r\n", body.len()).unwrap();
        self.stdin.write_all(body).unwrap();
        self.stdin.flush().unwrap();
    }

    pub fn request(&mut self, method: &str, params: Json) -> i64 {
        self.next_id += 1;
        let id = self.next_id;
        self.send(&request(id, method, params));
        id
    }

    pub fn notify(&mut self, method: &str, params: Json) {
        self.send(&notification(method, params));
    }

    pub fn initialize(&mut self, utf8: bool) -> Json {
        let capabilities = if utf8 {
            obj(vec![(
                "general",
                obj(vec![("positionEncodings", arr(vec![s("utf-8")]))]),
            )])
        } else {
            obj(vec![])
        };
        let id = self.request(
            "initialize",
            obj(vec![
                ("processId", Json::Null),
                ("rootUri", Json::Null),
                ("capabilities", capabilities),
            ]),
        );
        let response = self.response(id);
        assert!(response.get("error").is_none(), "{response:?}");
        self.notify("initialized", obj(vec![]));
        response.get("result").cloned().unwrap()
    }

    pub fn recv(&mut self) -> Json {
        let text = self
            .messages
            .recv_timeout(TIMEOUT)
            .unwrap_or_else(|_| panic!("timed out waiting for a server message"));
        parse(&text).unwrap_or_else(|error| panic!("the server wrote invalid JSON: {error}"))
    }

    /// The response with `id`, storing other messages for later.
    pub fn response(&mut self, id: i64) -> Json {
        loop {
            let message = self.recv();
            let is_response = message.get("id").and_then(Json::as_i64) == Some(id)
                && (message.get("result").is_some() || message.get("error").is_some());
            if is_response {
                return message;
            }
            self.pending.push(message);
        }
    }

    pub fn open(&mut self, document_uri: &str, text: &str) {
        self.notify(
            "textDocument/didOpen",
            obj(vec![(
                "textDocument",
                obj(vec![
                    ("uri", s(document_uri)),
                    ("languageId", s("ingot")),
                    ("version", n(1)),
                    ("text", s(text)),
                ]),
            )]),
        );
    }

    pub fn change(&mut self, document_uri: &str, text: &str) {
        self.notify(
            "textDocument/didChange",
            obj(vec![
                (
                    "textDocument",
                    obj(vec![("uri", s(document_uri)), ("version", n(2))]),
                ),
                ("contentChanges", arr(vec![obj(vec![("text", s(text))])])),
            ]),
        );
    }

    pub fn close(&mut self, document_uri: &str) {
        self.notify(
            "textDocument/didClose",
            obj(vec![("textDocument", obj(vec![("uri", s(document_uri))]))]),
        );
    }

    /// The next published diagnostic list for `document_uri`.
    pub fn published(&mut self, document_uri: &str) -> Vec<Json> {
        for index in 0..self.pending.len() {
            if let Some(diagnostics) = published_diagnostics(&self.pending[index], document_uri) {
                self.pending.remove(index);
                return diagnostics;
            }
        }
        loop {
            let message = self.recv();
            if let Some(diagnostics) = published_diagnostics(&message, document_uri) {
                return diagnostics;
            }
            self.pending.push(message);
        }
    }

    /// Whether a log message already received contains `text`.
    pub fn pending_log_contains(&self, text: &str) -> bool {
        self.pending
            .iter()
            .filter_map(log_message)
            .any(|message| message.contains(text))
    }

    /// The next `window/logMessage` whose text contains `text`.
    pub fn log_containing(&mut self, text: &str) -> String {
        for index in 0..self.pending.len() {
            if let Some(message) = log_message(&self.pending[index])
                && message.contains(text)
            {
                self.pending.remove(index);
                return message;
            }
        }
        loop {
            let message = self.recv();
            if let Some(log) = log_message(&message)
                && log.contains(text)
            {
                return log;
            }
            self.pending.push(message);
        }
    }

    /// Every message that arrives before `quiet` passes with none, including stored
    /// ones. Used to count checks after a burst of changes.
    pub fn drain(&mut self, quiet: Duration) -> Vec<Json> {
        let mut out = std::mem::take(&mut self.pending);
        while let Ok(message) = self.messages.recv_timeout(quiet) {
            out.push(parse(&message).expect("valid JSON"));
        }
        out
    }

    pub fn wait(&mut self) -> i32 {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                return status.code().unwrap_or(-1);
            }
            assert!(
                Instant::now() < deadline,
                "the server did not exit before the timeout"
            );
            thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn published_diagnostics(message: &Json, document_uri: &str) -> Option<Vec<Json>> {
    if message.get("method").and_then(Json::as_str) != Some("textDocument/publishDiagnostics") {
        return None;
    }
    let params = message.get("params")?;
    if params.get("uri").and_then(Json::as_str) != Some(document_uri) {
        return None;
    }
    match params.get("diagnostics")? {
        Json::Array(items) => Some(items.clone()),
        _ => None,
    }
}

fn log_message(message: &Json) -> Option<String> {
    if message.get("method").and_then(Json::as_str) != Some("window/logMessage") {
        return None;
    }
    message
        .get("params")
        .and_then(|params| params.get("message"))
        .and_then(Json::as_str)
        .map(str::to_string)
}

/// Count the debug check logMessage lines in a drained set.
pub fn checks_in(messages: &[Json]) -> usize {
    messages
        .iter()
        .filter_map(log_message)
        .filter(|message| message.starts_with("check "))
        .count()
}

/// A time box for `drain` quiet periods.
pub fn quiet() -> Duration {
    Duration::from_millis(500)
}
