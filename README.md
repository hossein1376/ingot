<h1 align="center">
  <img src="assets/logo.jpg" alt="Ingot" width="520">
</h1>

<p align="center">
  <strong>A safe systems language for AI agents to write, tuned for two specific machines.</strong>
</p>

Ingot is designed for an Apple M4 Pro and an AMD Ryzen 7 7800X3D, and nothing else. It
has no garbage collector, no hidden allocation and no implicit behavior. Safe code has
no undefined behavior: memory errors and data races are compile errors, and bugs such as
an out-of-range index or an integer overflow are deterministic traps. Parallelism is
part of the language.

```ingot
import std.Env;
import std.fs;
import std.mem.Arena;
import std.mem.AllocErr;
import std.simd;
import std.sync.Atomic;

enum AppErr { io: from fs.IoErr, alloc: from AllocErr, usage }

/// Counts newline bytes with native-width vectors: 16 lanes on the M4, 64 on the 7800X3D.
fn count_lines(data: []u8) -> i64 {
    let n = @lanes(simd.U8);
    let nl = @splat(simd.U8, '\n');
    var total: i64 = 0;
    var i: i64 = 0;
    while i + n <= data.len {
        total += @count(@load(simd.U8, data, i) == nl);    // bounds check removed by the loop condition
        i += n;
    }
    return total + @count(@load_tail(simd.U8, data, i) == nl);
}

pub fn main(env: &Env) -> void ! AppErr {
    if env.args.len != 2 { fail .usage; }

    let arena = try Arena.init(env.vm, 1 << 30);
    defer { arena.deinit(); }                              // forgetting this line is a compile error

    let data: []u8 = try fs.read_all(env.fs, env.args[1], &arena);

    let total = Atomic[i64].init(0);
    par for chunk in data.chunks(1 << 20) {                // all cores; `data` is shared read-only
        _ = total.add(count_lines(chunk), .relaxed);
    }
    @format(env.stdout, "{}\n", total.load(.relaxed)) catch _ => {};
}
```

## What the language is built around

| For the agent writing it | For the machine running it |
|---|---|
| A signature is a complete contract: errors, what the result borrows from, what is consumed | Three targets over two CPUs, every CPU feature assumed, no run-time detection |
| Every check is local to one function body | No GC, no hidden calls, no dynamic dispatch unless written `dyn` |
| Syntax close to Rust, Zig, Go and Swift, new only where it removes a class of mistake | Arenas as the primary memory strategy, allocators with static dispatch |
| JSON diagnostics with fix edits, and no warnings | Vector types up to 512 bits on both CPUs, with comparisons giving lane masks |
| `ingot cost` lists every check, allocation and indirect call that survived | Structured tasks and `par for` on a work-stealing runtime that is linked only when used |
| The same semantics in every build mode | One kernel form that runs as SIMD, across cores, or on the GPU (Metal, Vulkan) |

## Targets

| Target | Machine |
|---|---|
| `aarch64-macos-m4` | Apple M4 Pro, macOS |
| `x86_64-linux-znver4` | AMD Ryzen 7 7800X3D, Linux |
| `x86_64-windows-znver4` | AMD Ryzen 7 7800X3D, Windows |

## Status

**Ingot builds, runs and tests small programs on the M4 Pro.** The bootstrap compiler,
written in Rust, checks programs for all three targets, and compiles them to native code
for `aarch64-macos-m4` through LLVM 22: generics, closures, `&dyn`, errors and `try`
included. Tasks, vectors and most of the standard library are still missing.

| Works today | Not yet |
|---|---|
| Lexing, parsing and error recovery | Tasks, `spawn`, `par for` and `select` at run time (the scheduler) |
| Names, types, generics, interfaces and constant evaluation | Vectors, masks and kernels in generated code |
| Visibility (`pub(pkg)`), `unsafe`, raw pointers, atomics, waitable operations and `select` | Atomics, `Cell` and locks in generated code |
| Lowering to a CFG, and the ownership checker: moves, origins, loans, tasks | The Linux and Windows targets |
| Check removal, and every cost item `ingot cost` reports: checks, allocations, indirect calls, copies, spawns, locks, park points | Most of the standard library, `@format`, `Hash` and printing floats |
| Instance discovery (phase 11a): every generic instance, its depth and its symbol name | |
| `ingot build` and `ingot run` on `aarch64-macos-m4`: generic instances, closures, `&dyn`, `T ! E` with `try`/`catch`, constants and statics, every check with its trap line, and the `error:` line of a `main` that fails | |
| `ingot test`: `test` blocks, `@test_env()`, each test in its own process, under either pipeline | |
| Byte-identical executables from either pipeline, wherever the package sits | |
| Packages: path and git dependencies pinned by hash, workspaces, `ingot deps` | |
| `check`, `fix`, `fmt`, `query`, `serve`, the reports (`api`, `audit`, `layout`, `explain`, `targets`) and an LSP server for editors | |

The example above does not compile yet either: it needs `std.fs`, `std.simd` and the
arena of `std.mem`, which are still being designed in [docs/std/](docs/std/). `Atomic`
and `Cell` in `std.sync` and the runtime core in `std.sys.rt` are the first standard
library code with real bodies. Most of the [conformance suite](conformance/README.md)
passes, its `run` and `trap` tests under both build pipelines; the tests that still fail
need the scheduler, vectors or `@format`. [COMPILER.md](COMPILER.md) lists the build
steps and which are done, and [docs/later.md](docs/later.md) what waits until after v1.

## Trying it

```sh
cargo build --release --bin ingot
```

```sh
target/release/ingot check conformance/names/undefined_name.ingot
```

```sh
target/release/ingot cost conformance/cost/call_borrow_keeps_second_check.ingot
```

```sh
target/release/ingot build conformance/types/aggregate_arguments.ingot -o target/aggregate_arguments
```

```sh
target/release/ingot test conformance/names/testing_env.ingot
```

```sh
target/release/ingot fmt --check std
```

```sh
target/release/ingot conformance
```

`check` prints diagnostics (add `--json` for the machine form), `cost` lists the
checks that survived check removal, `build` writes an executable, `test` builds and
runs the `test` blocks, `fmt` puts source in the canonical layout, and `conformance`
runs the whole suite. `build`, `test` and the suite's `run` tests need Homebrew's
`llvm@22` (`brew install llvm@22`). Before a commit, `scripts/verify.sh` runs
formatting, Clippy, the tests, the conformance ratchet and a determinism check.

For editors, `cargo build --release --bin ingot-lsp` builds the language server, and
[editors/](editors/README.md) has the Tree-sitter grammar and a Zed extension.

## Repository layout

| Path | What it holds |
|---|---|
| [docs/](docs/README.md) | The specification: the definition of the language |
| [docs/impl/](docs/impl/README.md) | How to implement it: glossary, error and trap catalog, compiler pipeline, ownership checker, tools |
| [docs/std/](docs/std/) | Standard library design drafts, under review |
| [conformance/](conformance/README.md) | The programs every implementation must agree on |
| `crates/` | The compiler: `syntax`, `sema`, `cfg` (CFG, ownership, check removal), `codegen` (LLVM IR), `diag`, `driver` (the `ingot` command) and `lsp` |
| `std/` | The standard library the compiler loads as `std`: the runtime core and `std.sync` have bodies, the rest are declaration stubs |
| [editors/](editors/README.md) | Tree-sitter grammar and Zed extension |
| [COMPILER.md](COMPILER.md) | How this compiler is built, its rules and its step plan |
| [LSP.md](LSP.md) | The language server plan |
| [ROADMAP.md](ROADMAP.md) | The longer-term plan; [docs/later.md](docs/later.md) has what waits until after v1 |

## Reading the specification

To write Ingot, read [docs/primer.md](docs/primer.md): a few hundred lines that cover
what differs from the languages a model already knows.

Start with [docs/README.md](docs/README.md). It maps the goals to the design, gives the
whole design on one page and records the decisions that shaped it.

To implement the language, read [docs/impl/](docs/impl/README.md) next: the glossary,
the catalog of every error code and trap, the compiler pipeline, and the ownership
checker written out as algorithms.

## History

An earlier prototype compiler for a smaller language of the same name was removed from
the tree when the specification in `docs/` became the definition of Ingot. It is in git
history at commit `ab090de`. A first MVP of the current compiler is tagged
`mvp-reference`; its lexer, parser, diagnostics and fix engine were kept, and the rest
was rebuilt from the specification.

## License

Licensed under either of

* Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or
  <https://www.apache.org/licenses/LICENSE-2.0>)
* MIT license ([LICENSE-MIT](LICENSE-MIT) or <https://opensource.org/licenses/MIT>)

at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted for
inclusion in the work by you, as defined in the Apache-2.0 license, shall be dual
licensed as above, without any additional terms or conditions.
