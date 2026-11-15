# Editor support

- `tree-sitter-ingot/`: the Tree-sitter grammar, written from `docs/grammar.md`
  (tools.md §7). Imported with its history from its former standalone repository.
- `zed/`: the Zed extension (highlighting, brackets, outline, indentation, and the
  `ingot-lsp` language server from `crates/lsp`).

A change to `docs/grammar.md` should come with the matching grammar change here.
