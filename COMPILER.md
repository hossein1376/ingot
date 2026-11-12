# Bootstrap compiler

`src/` is the first Ingot compiler. It is a Rust crate with one binary:

```sh
cargo run -- check FILE        # diagnostics, human or --json
cargo run -- run FILE          # check, then evaluate main
cargo run -- test FILE         # check and run test blocks
cargo run -- cost FILE         # surviving checks from phase 10
cargo run -- conformance       # the whole suite
```

`cargo test` runs the conformance suite as an integration test.

## What is implemented

| Phase | State |
|---|---|
| 1 Load | Package directory trees, module names from file paths, `ingot.toml` name/targets. Dependency graphs and content hashes are not loaded |
| 2 Lex | Complete E01xx set, including maximal munch and builtin spelling |
| 3 Parse | Grammar through expressions, patterns, items, `when`, kernels and `spawn`/`dispatch`; E02xx recovery |
| 4 `when` | Parsed; the first branch is used because target selection is not wired |
| 5 Resolve | Per-module names, imports and aliases, module-qualified items, value/type namespaces, visibility, shadowing and unused bindings |
| 6 Signatures | Structs, enums, aliases, functions, methods, properties, layouts for the checks |
| 7 Type check | Literals and context, arithmetic and traps, structs/enums, `match`, results, `try`/`catch`/`fail`, conversions, SIMD types, receivers |
| 8 Lower | Not yet: ownership and borrow checks run on the typed AST |
| 9 Ownership | Moves, linear consumption (E1001 to E1005), plus a lexical borrow/origin pass for the conformance cases (E1101 to E1105, E1202 to E1204, E1403) |
| 10 Check removal | The covered `ingot cost` cases: counted loops and `@assert` length facts |
| 11 to 14 | Not yet |

The evaluator in `src/interp.rs` is a tree-walking second implementation of the
semantics. It supplies real traps, deferred blocks, results, slices, SIMD values,
scope tasks and sequential `par for`/`dispatch`. It exists so the front end can be
tested before LLVM is linked in; it is not a code generator and does not claim the
performance model.

## Layout

| File | Role |
|---|---|
| `src/lexer.rs`, `src/token.rs` | phase 2 |
| `src/ast.rs`, `src/parser.rs` | phase 3 and the parenthesization rule |
| `src/check.rs`, `src/check_body.rs` | phases 5 to 7 |
| `src/ownership.rs` | moves and linear values (E10xx) |
| `src/borrow.rs` | first borrow/origin pass (E11xx, E12xx, E1403) |
| `src/types.rs` | semantic types and §10 properties |
| `src/cost.rs` | first phase-10 pass |
| `src/interp.rs` | evaluator for `run`, `test`, `trap` |
| `src/diagnostic.rs` | catalog names, JSON lines, related spans and fixes |
| `src/conformance.rs` | `expect:` runner |

## Deliberate limits

* `import std.*` and `arch.*` names are opaque external types, so calls through
  them check permissively and evaluate only where the evaluator has a built-in.
  There is no `std` source tree yet, and dependencies listed in `[deps]` are not
  loaded.
* `ingot.toml` is read for `name` and `targets`; unknown keys, dependency hashes
  and target selection are not diagnosed yet.
* Borrow and origin checking is a lexical approximation, not the four dataflow
  analyses in `docs/impl/checker.md`. It is tuned to the conformance suite and is
  the first thing to replace with the CFG implementation.
* Kernel bodies are checked for the direct-call, foreign-index and trapping-op
  rules. `exec.simd`, `exec.par` and Metal/Vulkan are not implemented.
* The evaluator runs tasks sequentially. `par for` and `dispatch` are correct but
  single-threaded.
* No object code, linker, debug info or platform C ABI yet.

## Next steps

1. Lowering to the CFG, then the four analyses from `docs/impl/checker.md`.
2. A real `std` package tree, starting with `sys`, `mem`, `list` and `fmt`.
3. Code generation for `aarch64-macos-m4` through LLVM, starting with arithmetic,
   calls, arrays and slices.
