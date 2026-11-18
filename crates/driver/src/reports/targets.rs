//! `ingot targets`: the three targets, which one is the host, and what a build for
//! each still needs (tools.md §4).

use crate::commands::Output;
use ingot_diag::json::Json;
use ingot_sema::target::TARGETS;
use std::path::PathBuf;
use std::process::Command;
use std::time::Instant;

pub fn targets(started: Instant) -> Output {
    let host = host_target();
    let mut lines = Vec::new();
    let mut human = String::new();
    for name in TARGETS {
        let mut missing = Vec::new();
        if name == "aarch64-macos-m4" {
            if ingot_codegen::llvm::find().is_err() {
                missing.push("the pinned LLVM 22 clang".to_string());
            }
            if macos_sdk().is_none() {
                missing.push("the macOS SDK".to_string());
            }
        } else {
            missing.push("code generation for this target".to_string());
        }
        let build = missing.is_empty();
        let value = Json::Object(vec![
            ("type".to_string(), Json::String("target".to_string())),
            ("name".to_string(), Json::String(name.to_string())),
            (
                "host".to_string(),
                Json::Bool(host.as_deref() == Some(name)),
            ),
            ("build".to_string(), Json::Bool(build)),
            (
                "missing".to_string(),
                Json::Array(missing.into_iter().map(Json::String).collect()),
            ),
        ]);
        human.push_str(&format!(
            "{name}: {}{}\n",
            if build { "build" } else { "cannot build" },
            if host.as_deref() == Some(name) {
                ", host"
            } else {
                ""
            }
        ));
        lines.push(value.to_text());
    }
    lines.push(format!(
        "{{\"type\":\"summary\",\"command\":\"targets\",\"ok\":true,\"protocol\":1,\"ms\":{}}}",
        started.elapsed().as_millis()
    ));
    Output {
        lines,
        human,
        error: None,
        not_implemented: Vec::new(),
        exit: 0,
    }
}

/// The target of the machine this compiler runs on, when it is one of the three.
fn host_target() -> Option<String> {
    if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        Some("aarch64-macos-m4".to_string())
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        Some("x86_64-linux-znver4".to_string())
    } else if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        Some("x86_64-windows-znver4".to_string())
    } else {
        None
    }
}

/// The macOS SDK the pinned clang's link step reads. `xcrun` is the same lookup the
/// system's clang uses, so a present path means the link inputs exist.
fn macos_sdk() -> Option<PathBuf> {
    let output = Command::new("/usr/bin/xcrun")
        .args(["--show-sdk-path"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let path = PathBuf::from(path);
    path.is_dir().then_some(path)
}
