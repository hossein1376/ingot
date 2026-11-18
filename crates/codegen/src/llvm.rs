//! The pinned upstream LLVM 22: found by path, checked by major version, and used
//! as the driver for one IR text file.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The candidate paths of the pinned `clang`, in order. Apple's `/usr/bin/clang`
/// reads a different textual IR and is never one of them.
pub fn candidates() -> Vec<PathBuf> {
    vec![
        PathBuf::from("/opt/homebrew/opt/llvm@22/bin/clang"),
        PathBuf::from("/usr/local/opt/llvm@22/bin/clang"),
    ]
}

/// Find the pinned clang. The message names what was found, so a user can install
/// the right toolchain.
pub fn find() -> Result<PathBuf, String> {
    for path in candidates() {
        if path.is_file() {
            let version = version_of(&path)?;
            if version.0 == 22 {
                return Ok(path);
            }
            return Err(format!(
                "the LLVM 22 toolchain is required, but {} is LLVM {}.{}.{}; install Homebrew's `llvm@22`",
                path.display(),
                version.0,
                version.1,
                version.2
            ));
        }
    }
    Err(
        "the LLVM 22 toolchain is required, but /opt/homebrew/opt/llvm@22/bin/clang and /usr/local/opt/llvm@22/bin/clang were not found; install Homebrew's `llvm@22`"
            .to_string(),
    )
}

fn version_of(path: &Path) -> Result<(u32, u32, u32), String> {
    let output = Command::new(path)
        .arg("--version")
        .output()
        .map_err(|e| format!("could not run {}: {e}", path.display()))?;
    let text = String::from_utf8_lossy(&output.stdout);
    for word in text.split_whitespace() {
        let clean = word.trim_matches(|c: char| !c.is_ascii_digit() && c != '.');
        let mut parts = clean.split('.');
        if let (Some(a), Some(b), Some(c)) = (parts.next(), parts.next(), parts.next())
            && let (Ok(a), Ok(b), Ok(c)) = (a.parse(), b.parse(), c.parse())
        {
            return Ok((a, b, c));
        }
    }
    Err(format!(
        "could not read the LLVM version of {}: {}",
        path.display(),
        text.lines().next().unwrap_or("")
    ))
}

/// Why linking failed: the toolchain is missing, or `clang` rejected the IR or
/// could not link it.
#[derive(Clone, Debug)]
pub enum LinkError {
    Toolchain(String),
    Link(String),
}

/// Compile one IR file to an executable with the pinned clang. `fast` is `-O0`;
/// the optimizing pipeline is `-O2`. The program is one module, so there is no
/// LTO and no object-file intermediate. The target is passed explicitly, because
/// the driver must not silently override the IR's triple.
pub fn link(ir: &Path, output: &Path, fast: bool, target: &str) -> Result<(), LinkError> {
    let (triple, cpu) = target_flags(target)?;
    let clang = find().map_err(LinkError::Toolchain)?;
    let mut command = Command::new(clang);
    command.arg(if fast { "-O0" } else { "-O2" });
    command.arg("-fPIC");
    command.arg(format!("--target={triple}"));
    command.arg(format!("-mcpu={cpu}"));
    command.arg("-o").arg(output).arg(ir);
    let result = command
        .output()
        .map_err(|e| LinkError::Link(format!("could not run clang: {e}")))?;
    if result.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&result.stderr);
    Err(LinkError::Link(format!(
        "clang failed with status {}: {}",
        result.status,
        stderr.lines().take(12).collect::<Vec<_>>().join("\n")
    )))
}

/// The target's clang triple and CPU. B1 builds only the host target.
fn target_flags(target: &str) -> Result<(&'static str, &'static str), LinkError> {
    match target {
        "aarch64-macos-m4" => Ok(("arm64-apple-macosx14.0.0", "apple-m4")),
        other => Err(LinkError::Toolchain(format!(
            "not implemented: building for {other} (T-steps)"
        ))),
    }
}
