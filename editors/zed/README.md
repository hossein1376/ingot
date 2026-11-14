# zed-ingot

Zed extension for the Ingot language: tree-sitter highlighting, brackets, code outline, indentation.

Language-only extension so far; the language server (`ingot-lsp`, `crates/lsp`) is being
added (LSP.md step L2). The grammar is `editors/tree-sitter-ingot/` in this repository: Zed
clones the repository at the pinned `rev` and builds the grammar from that `path`.

## Install (dev)

1. Open Zed.
2. `cmd-shift-p` → `zed: install dev extension`
3. Select `editors/zed/` in this repo.
4. Open any `.ingot` file. It should switch to the `Ingot` language and highlight.

Zed clones this repository from the `file://` URL in `extension.toml` at the pinned `rev`
and compiles `editors/tree-sitter-ingot/src/parser.c` itself. A grammar change must be
committed first; then `rev` is bumped to that commit in a second commit.

## Publish later

Change `[grammars.ingot] repository` (and `rev`) in `extension.toml` to the public
repository, then submit a PR to zed-industries/extensions (its registry accepts an
extension in a subdirectory). If other editors need a standalone grammar repository, it
can be split out with `git subtree split --prefix=editors/tree-sitter-ingot`.

## Files

- `extension.toml` — grammar registration
- `languages/ingot/config.toml` — language metadata (file suffix `.ingot`, `// ` comments, bracket pairs)
- `languages/ingot/highlights.scm` — syntax highlighting, with Zed's capture names. It
  duplicates `editors/tree-sitter-ingot/queries/highlights.scm`; change both together.
- `languages/ingot/brackets.scm` — rainbow bracket matching
- `languages/ingot/outline.scm` — outline view (fn, struct, enum, interface, test, bench, const, static, type)
- `languages/ingot/indents.scm` — auto-indent

The `.scm` files are copies of `tree-sitter-ingot/queries/` kept in sync by hand for now.
