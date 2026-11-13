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
| 1 Load | Package directory trees, module names from file paths, `ingot.toml` name/targets, and the `std` source package attached from `std/`. Dependency graphs and content hashes are not loaded |
| 2 Lex | Complete E01xx set, including maximal munch and builtin spelling |
| 3 Parse | Grammar through expressions, patterns, items, `when`, kernels and `spawn`/`dispatch`; E02xx recovery |
| 4 `when` | Parsed; the first branch is used because target selection is not wired |
| 5 Resolve | Per-module names, imports and aliases, module-qualified items, value/type namespaces, visibility, shadowing and unused bindings |
| 6 Signatures | Structs, enums, aliases, functions, methods, properties, layouts for the checks |
| 7 Type check | Literals and context, arithmetic and traps, structs/enums, `match`, results, `try`/`catch`/`fail`, conversions, SIMD types, receivers |
| 8 Lower | Typed AST to CFG: places, explicit moves, two-phase borrows, storage ranges, deferred copies, cleanups, calls, spawn and scope waits |
| 9 Ownership | Four CFG dataflows: moves, linear storage ends, origin sets, and loan liveness/access checks (E1001 to E1005, E1101 to E1105, E1202/E1203, E1403) |
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
| `src/cfg.rs` | phase 8: lowering to basic blocks, places and cleanup edges |
| `src/cfg_check.rs` | phase 9: moves, linear values, origins, loans and the function boundary |
| `src/types.rs` | semantic types and §10 properties |
| `src/cost.rs` | first phase-10 pass |
| `src/interp.rs` | evaluator for `run`, `test`, `trap` |
| `src/diagnostic.rs` | catalog names, JSON lines, related spans and fixes |
| `src/conformance.rs` | `expect:` runner |

## Deliberate limits

* `std/` is a real package tree with `Env`, `mem`, `io`, `fs`, `net`, `time`,
  `text`, `fmt`, `sys` and `sys.macos`. Modules that do not exist yet
  (`std.sync`, `std.task`, `std.simd`, `std.exec`) and `arch.*` stay opaque
  external names. Dependencies listed in `[deps]` are not loaded.
* `ingot.toml` is read for `name` and `targets`; unknown keys, dependency hashes
  and target selection are not diagnosed yet.
* The CFG checker splits linear aggregates per field and checks origin stores at
  the function boundary (E1204/E1205). Its origin sets remain conservative: one
  set per local, as `docs/impl/checker.md` section 9 allows.
* Phase 10 runs on the CFG and removes counted-loop, `@assert`, branch-range,
  widening-cast, slice-range, duplicate-check dominance, and constant checks. The
  deliberate conservative case left is width>1 vector loads, which keep their check
  until the vector-specific range analysis lands.
* Kernel bodies are checked for the direct-call, foreign-index and trapping-op
  rules. `exec.simd`, `exec.par` and Metal/Vulkan are not implemented.
* The evaluator runs tasks sequentially. `par for` and `dispatch` are correct but
  single-threaded.
* No object code, linker, debug info or platform C ABI yet.

## Next steps

1. Expand the standard library: allocator implementations, `list`/`str`/`map`
   containers, then `fs`, `io` and `fmt` over `sys.macos`.
2. Code generation for `aarch64-macos-m4` through LLVM, starting with arithmetic,
   calls, arrays and slices. The raw-pointer builtins the containers need are part
   of this step.
3. Finish the remaining phase-10 and origin cases once code generation is present.
