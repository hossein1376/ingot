# zed-ingot

Zed extension for the Ingot language: tree-sitter highlighting, brackets, code outline,
indentation, and the `ingot-lsp` language server (`crates/lsp`: diagnostics and quick
fixes).

The grammar is `editors/tree-sitter-ingot/` in this repository: Zed clones the repository
at the pinned `rev` and builds the grammar from that `path`.

## Build `ingot-lsp`

From the repository root:

```sh
cargo build --release -p ingot-lsp
```

The binary is `target/release/ingot-lsp`. It finds the standard library next to itself
(`target/release/../../std` in the checkout) or through `INGOT_STD`. If you copy the
binary somewhere else, set `INGOT_STD` to the checkout's `std/` directory.

## Install the extension (dev)

1. Build `ingot-lsp` (above) so the path below exists.
2. Open Zed.
3. `cmd-shift-p` → `zed: install dev extension`
4. Select `editors/zed/` in this repo.
5. Open any `.ingot` file. It should switch to the `Ingot` language, highlight, and
   report compiler diagnostics.

Zed starts `ingot-lsp` from `PATH` when it can find it. Otherwise, point Zed at the
build with this setting (`cmd-,` opens the settings file):

```json
{
  "lsp": {
    "ingot-lsp": {
      "binary": {
        "path": "/absolute/path/to/ingot/target/release/ingot-lsp",
        "arguments": [],
        "env": {
          "INGOT_STD": "/absolute/path/to/ingot/std"
        }
      }
    }
  }
}
```

`arguments` and `env` are optional; `env.INGOT_STD` is only needed when the standard
library is not next to the executable. If the language server cannot start, Zed's
language server log (command palette: `dev: open language server logs`) shows the error
from the extension.

## Publish later

Change `[grammars.ingot] repository` (and `rev`) in `extension.toml` to the public
repository, then submit a PR to zed-industries/extensions (its registry accepts an
extension in a subdirectory). If other editors need a standalone grammar repository, it
can be split out with `git subtree split --prefix=editors/tree-sitter-ingot`.

The extension is built for `wasm32-wasip2`; `Cargo.toml` pins the `zed_extension_api`
version that matches the Zed release it targets.

## Files

- `extension.toml` — grammar and language-server registration
- `Cargo.toml`, `src/lib.rs` — the extension: starts `ingot-lsp` from settings or `PATH`
- `languages/ingot/config.toml` — language metadata (file suffix `.ingot`, `// ` comments, bracket pairs)
- `languages/ingot/highlights.scm` — syntax highlighting, with Zed's capture names. It
  duplicates `editors/tree-sitter-ingot/queries/highlights.scm`; change both together.
- `languages/ingot/brackets.scm` — rainbow bracket matching
- `languages/ingot/outline.scm` — outline view (fn, struct, enum, interface, test, bench, const, static, type)
- `languages/ingot/indents.scm` — auto-indent

The `.scm` files are copies of `tree-sitter-ingot/queries/` kept in sync by hand for now.
A grammar change must be committed first; then `rev` is bumped to that commit in a second
commit.
