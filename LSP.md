# The language server (MVP)

An editor adapter for writing Ingot and drafting `std` while the compiler is still
being built. It follows `docs/impl/tools.md`: §6 (`ingot serve`), §7 (the LSP table)
and §5 (queries). Where this file and `docs/` disagree, `docs/` wins. The rules of
COMPILER.md (spec is the only source of rules, tests first, determinism, the gate
`scripts/verify.sh`) apply here too.

## Goal

Open an `.ingot` file in the editor and get, on every keystroke:

1. the diagnostics phases 1–7 report today (more as S5–S10 land), with related spans;
2. the diagnostic's fixes as quick fixes;
3. go to definition, hover (type, signature, doc comment) and the outline.

Not in the MVP: references, rename, inlay hints, borrows (needs S6–S9), formatting (no
formatter spec yet), completion (tools.md §7: not specified for v1), incremental
checking (tools.md §6 says *should*; a full re-check of a `std`-sized package is fast
enough for now).

## Architecture

```
editor ──LSP (JSON-RPC, stdio)──▶ ingot-lsp ──serve lines, in-process──▶ serve::Server ──▶ pipeline
```

* **`ingot serve`** (driver, `serve.rs`): the request loop of tools.md §6. The engine is
  `Server::handle(&mut self, request_line: &str) -> Vec<String>`; `ingot serve` is a
  stdin loop around it. It holds the overlays and runs the same code as the one-shot
  commands, so its lines are byte-identical to theirs (tested).
* **`ingot-lsp`** (new crate `crates/lsp`, binary `ingot-lsp`): translates LSP to serve
  requests and serve lines back to LSP. It links the driver and calls `Server` in
  process, but sees only JSON lines, so the boundary is the serve protocol as tools.md
  §7 requires. A separate crate keeps it out of the driver files the S-steps edit, and
  below the driver in the crate table.
* **JSON** (`ingot-diag`, `json.rs`): a small value type, parser and writer. Serve needs
  a reader for its requests anyway; the LSP adapter reuses it. No dependencies.

## Steps

| Step | Content | Done when |
|---|---|---|
| L1 | `json.rs`; loader overlays (`LoadOptions` gets a map path → text, used by every file read, `std` included); the `error` line of one-shot failures (tools.md §1); `ingot serve` with `check`, `overlay`, `cancel`, `shutdown` (other commands answer "not implemented in serve" for now) | serve tests: a request script gives exactly the one-shot lines; the conformance suite's `check` run through serve is byte-identical |
| L2 | `ingot-lsp`: framing, lifecycle, full-text sync, `publishDiagnostics`, code actions, position encodings; editor wiring | an LSP transcript test (spawn the binary, golden responses); diagnostics live in the editor |
| L3 | `ingot query def`, `query type`, `query symbols` (one-shot and serve); signatures and `///` doc comments cut from the source; LSP definition, hover, document and workspace symbols | query tests over fixture packages; hover and jump work in the editor |
| L4 | `ingot query complete` (one-shot and serve), working on incomplete code; LSP completion | completion tests for each context of tools.md §5, including `t.` with nothing after the dot; completion works in the editor |

The MVP is usable after L2; L3 makes it pleasant. Later steps (refs, rename, inlay
hints, borrows, fmt, completion, incremental) wait for the compiler and spec parts they
need.

## Decisions inside the steps

**Settled with the user (2026-11-14).** No dependencies, as for the compiler. Zed is
the only editor wired up. Its extension is `editors/zed/` and the grammar
`editors/tree-sitter-ingot/`, both in this repository (moved in from untracked folders
on 2026-11-14). The branch `lsp`
starts at 775fdde and is rebased onto `compiler` once S5 is committed. The serve
protocol gaps (error lines, `exit` in serve summaries, request paths, cancel, end of
input) are filled in tools.md §1 and §6.

**Which package is checked.** The package of the file: the nearest `ingot.toml` at or
above it, else the file alone (tools.md §1). Editing `std/*.ingot` checks `std` itself.
Diagnostics are published for every file of that package, and an empty list is sent
for a file that had diagnostics and no longer does.

**Change handling.** A reader thread parses frames into a channel (a thread and `mpsc`,
no `Arc`). The main loop takes every message already waiting, applies all `didChange`s
as overlays, then runs one check per package: typing fast gives one check per pause,
not per key. A batch that changes anything checks every package with an open document,
not only the changed file's: editing `std` changes what every open package imports, and
a check costs about a millisecond.
`didClose` clears the overlay (the file on disk is used again).

**Sync kind** is full text. Incremental text sync is a later optimisation.

**Positions.** The adapter offers `utf-8` in `positionEncodings` (LSP 3.17); otherwise
it converts each position between UTF-16 units and byte columns of that line.

**Severity.** Every diagnostic is an error unless the catalog says otherwise.
`not_implemented` reasons are not diagnostics: they are logged (`window/logMessage`),
since today almost every accepted body ends in one. A compiler failure (exit status 2
or a panic, caught with `catch_unwind`) is shown once with `window/showMessage` and the
server keeps running.

**Code actions.** The fixes of the last published diagnostics whose span meets the
requested range, each as a `quickfix` with a `WorkspaceEdit`.

**Queries (L3).** Definition, hover and symbol requests are answered after the batch's
checks, with the same overlays, like code actions. Each runs the front end again; at a
few milliseconds per run that needs no cache yet.

**Go to definition** returns one `Location` per `definition` line, at its `span` (the
name). No line gives `null`.

**Hover** asks `query def` and `query type` at the position. The content is Markdown: the
`signature` in an `ingot` code block (for a parameter or local: `name: type` from the
type answer), then the `doc`, then `type: T` when the position is an expression whose
type is not already the signature's subject. Its range is the `type_at` span, else the
token's. Nothing found gives `null`.

**Document symbols** come from `query symbols` on the document's file, as a flat list of
`SymbolInformation` with `containerName` = the path without its last segment. (Zed's
outline panel uses the grammar's `outline.scm`; LSP symbols serve the symbol search.)

**Completion (L4)** comes from `query complete` at the cursor, answered after the
batch's changes like the other queries. `completionProvider` has `triggerCharacters:
["."]`. Each line becomes a `CompletionItem`: `label` = `name`, `detail` = `signature`
(or `of` for a local), `documentation` = `doc` as Markdown, `kind` by `item`: `fn` 3
(2 after a `.` on a value), `field` 5, `variant` 20, `struct` 22, `enum` 13, `interface`
8, `type` 25, `generic` 25, `const` 21, `static` 6, `local` and `param` 6, `module` 9,
`builtin` 7. The list is complete (`isIncomplete: false`); the client filters further as
the user types.

**Workspace symbols** run `query symbols --match TEXT` on each package that has an open
document, in package order, and concatenate the results.

## Status

| Step | State |
|---|---|
| L1 | done |
| L2 | done |
| L3 | done |
| L4 | done |

Known gaps after L4: `query def` gives nothing on the `.Item` of `T.Item` (the HIR keeps
no definition for it); an ambiguous name gives one line, not several; `query refs`,
`query borrows`, `rename` and `--name` are not built.
