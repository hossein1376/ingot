<h1 align="center">
  <img src="assets/logo.jpg" alt="Ingot" width="520">
</h1>

<p align="center">
  <strong>A systems language written by AI agents, for two specific machines.</strong>
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
        total.add(count_lines(chunk), .relaxed);
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

**Ingot does not generate code yet.** The bootstrap compiler, written in Rust, checks
programs; it does not build or run them.

| Works today | Not yet |
|---|---|
| Lexing, parsing and error recovery | Monomorphization and per-instance checks (phase 11) |
| Names, types, generics, interfaces and constant evaluation | Code generation for the three targets |
| Lowering to a CFG, and the ownership checker: moves, origins, loans, tasks | `ingot run` and `ingot test` |
| Check removal, and the bounds and overflow checks `ingot cost` reports | The other cost items: allocations, indirect calls, copies, spawns, locks, park points |
| `check`, `fix`, `query`, `serve` and an LSP server for editors | Most of the standard library |

The example above does not compile yet either: it needs `std.fs`, `std.simd` and
`std.sync`, which are still being designed in [docs/std/](docs/std/). Most of the
[conformance suite](conformance/README.md) passes; the tests that still fail need code
generation or standard library pieces that are still missing. [COMPILER.md](COMPILER.md)
lists the build steps and which are done.

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
target/release/ingot conformance
```

`check` prints diagnostics (add `--json` for the machine form), `cost` lists the
checks that survived check removal, and `conformance` runs the whole suite. Before a
commit, `scripts/verify.sh` runs formatting, Clippy, the tests, the conformance
ratchet and a determinism check.

For editors, `cargo build --release --bin ingot-lsp` builds the language server, and
[editors/](editors/README.md) has the Tree-sitter grammar and a Zed extension.

## Repository layout

| Path | What it holds |
|---|---|
| [docs/](docs/README.md) | The specification: the definition of the language |
| [docs/impl/](docs/impl/README.md) | How to implement it: glossary, error and trap catalog, compiler pipeline, ownership checker, tools |
| [docs/std/](docs/std/) | Standard library design drafts, under review |
| [conformance/](conformance/README.md) | The programs every implementation must agree on |
| `crates/` | The compiler: `syntax`, `sema`, `cfg` (CFG, ownership, check removal), `diag`, `driver` (the `ingot` command) and `lsp` |
| `std/` | Early standard library sources |
| [editors/](editors/README.md) | Tree-sitter grammar and Zed extension |
| [COMPILER.md](COMPILER.md) | How this compiler is built, its rules and its step plan |
| [LSP.md](LSP.md) | The language server plan |
| [ROADMAP.md](ROADMAP.md) | The longer-term plan |

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
