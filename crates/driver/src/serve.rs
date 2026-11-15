//! `ingot serve` (tools.md §6): one request per line, the one-shot lines back.
//!
//! The server is a cache, not a second implementation: `check` calls the same
//! [`commands::check`] the CLI calls and only decorates its lines with the request
//! `id` and the summary's `exit`. Overlays persist here and apply to every later read.
//! One thread answers requests in order; `cancel` therefore only answers itself, which
//! tools.md §6 allows ("one that is not stopped, or has already finished, is answered
//! in full").

use crate::commands;
use crate::files::Overlays;
use crate::query;
use ingot_diag::diagnostic::json_string;
use ingot_diag::json::Json;
use std::fmt::Write as _;
use std::path::Path;
use std::time::Instant;

/// The one-shot commands serve knows by name but does not implement yet.
const NOT_IN_SERVE: &[&str] = &[
    "fix", "explain", "fmt", "build", "run", "test", "bench", "api", "cost", "audit", "layout",
    "targets", "asm", "rename",
];

#[derive(Clone, Debug, Default)]
pub struct Server {
    overlays: Overlays,
    shutdown: bool,
}

impl Server {
    pub fn new() -> Self {
        Server::default()
    }

    /// True once a `shutdown` request has been answered; the loop then exits 0.
    pub fn shutdown_requested(&self) -> bool {
        self.shutdown
    }

    /// Answer one request: the response lines, without newlines.
    pub fn handle(&mut self, request: &str) -> Vec<String> {
        let started = Instant::now();
        let parsed = ingot_diag::json::parse(request);
        let id = request_id(&parsed);
        // A panic while handling a request must not end the server.
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.dispatch(parsed, &id, started)
        }));
        match result {
            Ok(lines) => lines,
            Err(payload) => error_response(&id, "", &panic_message(payload), started),
        }
    }

    fn dispatch(
        &mut self,
        parsed: Result<Json, ingot_diag::json::Error>,
        id: &Json,
        started: Instant,
    ) -> Vec<String> {
        let request = match parsed {
            Ok(Json::Object(members)) => members,
            Ok(_) => return error_response(id, "", "the request is not a JSON object", started),
            Err(error) => {
                return error_response(id, "", &format!("invalid JSON: {error}"), started);
            }
        };
        let command = match member(&request, "command") {
            Some(Json::String(name)) => name.clone(),
            Some(_) => {
                return error_response(id, "", "`command` is not a string", started);
            }
            None => return error_response(id, "", "the request has no `command`", started),
        };
        let args = match member(&request, "args") {
            None => Vec::new(),
            Some(Json::Object(args)) => args.clone(),
            Some(_) => {
                return error_response(
                    id,
                    &command,
                    &format!("`{command}`: `args` is not an object"),
                    started,
                );
            }
        };
        match command.as_str() {
            "check" => self.check(id, &args, started),
            "overlay" => self.overlay(id, &args, started),
            "cancel" => self.cancel(id, &args, started),
            "query" => self.query(id, &args, started),
            "shutdown" => self.shutdown(id, &args, started),
            other if NOT_IN_SERVE.contains(&other) => error_response(
                id,
                other,
                &format!("not implemented in serve: `{other}`"),
                started,
            ),
            other => error_response(id, other, &format!("unknown command `{other}`"), started),
        }
    }

    fn check(&mut self, id: &Json, args: &[(String, Json)], started: Instant) -> Vec<String> {
        if let Err(message) = only_args(args, "check", &["path", "target", "all_targets"]) {
            return error_response(id, "check", &message, started);
        }
        let path = match optional_string(args, "path") {
            Ok(path) => path.unwrap_or_else(|| ".".to_string()),
            Err(message) => return error_response(id, "check", &message, started),
        };
        let target = match optional_string(args, "target") {
            Ok(target) => target,
            Err(message) => return error_response(id, "check", &message, started),
        };
        let all_targets = match member(args, "all_targets") {
            None => false,
            Some(Json::Bool(value)) => *value,
            Some(_) => {
                return error_response(id, "check", "`all_targets` is not a boolean", started);
            }
        };
        if all_targets {
            return error_response(
                id,
                "check",
                "not implemented: `check --all-targets`",
                started,
            );
        }
        let output = commands::check(Path::new(&path), target.as_deref(), &self.overlays);
        respond(output.lines, id, output.exit)
    }

    fn query(&mut self, id: &Json, args: &[(String, Json)], started: Instant) -> Vec<String> {
        let kind = match optional_string(args, "kind") {
            Ok(Some(kind)) => kind,
            Ok(None) => {
                return query_failure(id, "", "`query` needs `kind`", started);
            }
            Err(message) => {
                return query_failure(id, "", &message, started);
            }
        };
        if args.iter().any(|(name, _)| name == "name") {
            return query_failure(id, &kind, "not implemented: `query --name`", started);
        }
        let allowed: &[&str] = match kind.as_str() {
            "def" | "type" | "complete" => &["kind", "at", "name"],
            "symbols" => &["kind", "path", "match", "name"],
            _ => &["kind", "at", "path", "match", "name"],
        };
        if let Err(message) = only_args(args, "query", allowed) {
            return query_failure(id, &kind, &message, started);
        }
        let at = match optional_string(args, "at") {
            Ok(at) => at,
            Err(message) => return query_failure(id, &kind, &message, started),
        };
        let path = match optional_string(args, "path") {
            Ok(path) => path,
            Err(message) => return query_failure(id, &kind, &message, started),
        };
        let match_text = match optional_string(args, "match") {
            Ok(text) => text,
            Err(message) => return query_failure(id, &kind, &message, started),
        };
        let output = query::run(
            &kind,
            at.as_deref(),
            path.as_deref(),
            match_text.as_deref(),
            &self.overlays,
            started,
        );
        respond(output.lines, id, output.exit)
    }

    fn overlay(&mut self, id: &Json, args: &[(String, Json)], started: Instant) -> Vec<String> {
        if let Err(message) = only_args(args, "overlay", &["file", "text"]) {
            return error_response(id, "overlay", &message, started);
        }
        let file = match required_string(args, "file") {
            Ok(file) => file,
            Err(message) => return error_response(id, "overlay", &message, started),
        };
        let text = match member(args, "text") {
            Some(Json::String(text)) => Some(text.clone()),
            Some(Json::Null) => None,
            Some(_) => {
                return error_response(id, "overlay", "`text` is not a string or null", started);
            }
            None => return error_response(id, "overlay", "`overlay` needs `text`", started),
        };
        match text {
            Some(text) => self.overlays.insert(Path::new(&file), text),
            None => self.overlays.remove(Path::new(&file)),
        }
        vec![extra_summary(id, "overlay", started)]
    }

    fn cancel(&mut self, id: &Json, args: &[(String, Json)], started: Instant) -> Vec<String> {
        if let Err(message) = only_args(args, "cancel", &["target"]) {
            return error_response(id, "cancel", &message, started);
        }
        if member(args, "target").is_none() {
            return error_response(id, "cancel", "`cancel` needs `target`", started);
        }
        // With one thread every earlier request has finished, so there is nothing to
        // stop, and no summary says `canceled` (that field belongs to a stopped target's
        // summary). tools.md §6: the cancel itself always succeeds.
        vec![extra_summary(id, "cancel", started)]
    }

    fn shutdown(&mut self, id: &Json, args: &[(String, Json)], started: Instant) -> Vec<String> {
        if let Err(message) = only_args(args, "shutdown", &[]) {
            return error_response(id, "shutdown", &message, started);
        }
        self.shutdown = true;
        vec![extra_summary(id, "shutdown", started)]
    }
}

/// The last member named `name`.
fn member<'a>(members: &'a [(String, Json)], name: &str) -> Option<&'a Json> {
    members
        .iter()
        .rev()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value)
}

fn only_args(args: &[(String, Json)], command: &str, allowed: &[&str]) -> Result<(), String> {
    for (name, _) in args {
        if !allowed.contains(&name.as_str()) {
            return Err(format!("`{command}` does not take `{name}`"));
        }
    }
    Ok(())
}

fn optional_string(args: &[(String, Json)], name: &str) -> Result<Option<String>, String> {
    match member(args, name) {
        None => Ok(None),
        Some(Json::String(text)) => Ok(Some(text.clone())),
        Some(_) => Err(format!("`{name}` is not a string")),
    }
}

fn required_string(args: &[(String, Json)], name: &str) -> Result<String, String> {
    match optional_string(args, name)? {
        Some(text) => Ok(text),
        None => Err(format!("`{name}` is required")),
    }
}

/// The `id` a response carries: the request's own, or null when it has none or cannot
/// be read.
fn request_id(parsed: &Result<Json, ingot_diag::json::Error>) -> Json {
    match parsed {
        Ok(Json::Object(members)) => member(members, "id").cloned().unwrap_or(Json::Null),
        _ => Json::Null,
    }
}

/// Decorate one-shot lines: summaries get `"exit":N`, and every line gets the `id`.
fn respond(lines: Vec<String>, id: &Json, exit: u8) -> Vec<String> {
    lines
        .into_iter()
        .map(|line| {
            let line = with_exit(line, exit);
            with_id(&line, id)
        })
        .collect()
}

fn with_exit(line: String, exit: u8) -> String {
    if !line.starts_with("{\"type\":\"summary\"") {
        return line;
    }
    let mut line = line;
    line.pop();
    let _ = write!(line, ",\"exit\":{exit}}}");
    line
}

fn with_id(line: &str, id: &Json) -> String {
    let mut out = String::with_capacity(line.len() + 16);
    out.push('{');
    out.push_str("\"id\":");
    id.write(&mut out);
    out.push(',');
    out.push_str(&line[1..]);
    out
}

/// The summary of an extra request (tools.md §6), `exit` 0.
fn extra_summary(id: &Json, command: &str, started: Instant) -> String {
    let mut line = String::from("{\"type\":\"summary\",\"command\":");
    json_string(&mut line, command);
    line.push_str(",\"ok\":true");
    let _ = write!(
        line,
        ",\"protocol\":1,\"exit\":0,\"ms\":{}}}",
        started.elapsed().as_millis()
    );
    with_id(&line, id)
}

fn error_response(id: &Json, command: &str, message: &str, started: Instant) -> Vec<String> {
    respond(commands::failure_lines(command, message, started), id, 2)
}

fn query_failure(id: &Json, kind: &str, message: &str, started: Instant) -> Vec<String> {
    respond(query::failure_lines(kind, message, started), id, 2)
}

fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        (*message).to_string()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        "panic".to_string()
    }
}
