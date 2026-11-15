use zed_extension_api::{self as zed, LanguageServerId, Result, Worktree, settings::LspSettings};

/// Starts `ingot-lsp` for Ingot buffers.
struct IngotExtension;

impl zed::Extension for IngotExtension {
    fn new() -> Self {
        IngotExtension
    }

    fn language_server_command(
        &mut self,
        language_server_id: &LanguageServerId,
        worktree: &Worktree,
    ) -> Result<zed::Command> {
        let settings = LspSettings::for_worktree(language_server_id.as_ref(), worktree)?;
        if let Some(binary) = settings.binary
            && let Some(path) = binary.path
        {
            return Ok(zed::Command {
                command: path,
                args: binary.arguments.unwrap_or_default(),
                env: binary
                    .env
                    .map(|env| env.into_iter().collect())
                    .unwrap_or_default(),
            });
        }
        if let Some(path) = worktree.which("ingot-lsp") {
            return Ok(zed::Command {
                command: path,
                args: Vec::new(),
                env: Vec::new(),
            });
        }
        Err("`ingot-lsp` was not found on PATH. Build it with \
             `cargo build --release -p ingot-lsp` in the Ingot checkout, then set \
             `lsp.ingot-lsp.binary.path` to the absolute path of `target/release/ingot-lsp` \
             (or add that directory to PATH). If the standard library is not next to the \
             binary, set `lsp.ingot-lsp.binary.env.INGOT_STD` to the `std` directory."
            .to_string())
    }
}

zed::register_extension!(IngotExtension);
