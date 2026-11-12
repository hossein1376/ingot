# Ingot

**A systems language written by AI agents, for two specific machines.**

Ingot is designed for an Apple M4 Pro and an AMD Ryzen 7 7800X3D, and nothing else. It
has no garbage collector, no hidden allocation and no implicit behavior. Safe code has
no undefined behavior: memory errors and data races are compile errors, and bugs such as
an out-of-range index or an integer overflow are deterministic traps. Parallelism is
part of the language.

> **Status: bootstrap compiler.** The front end in `src/` implements lexing,
> parsing, name resolution, type checking and a first ownership pass, and a
> tree-walking evaluator runs the conformance suite. Code generation is the next
> milestone; [ROADMAP.md](ROADMAP.md) tracks it.
>
> ```sh
> cargo run -- check conformance/names/undefined_name.ingot
> cargo run -- run conformance/errors/try_propagates.ingot
> cargo run -- conformance
> ```
>
> The language itself is defined in [docs/](docs/README.md). The compiler's
> layout and current limits are in [COMPILER.md](COMPILER.md).

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

## Reading the specification

To write Ingot, read [docs/primer.md](docs/primer.md): a few hundred lines that cover
what differs from the languages a model already knows.

Start with [docs/README.md](docs/README.md). It maps the goals to the design, gives the
whole design on one page and records the decisions that shaped it.

To implement the language, read [docs/impl/](docs/impl/README.md) next: the glossary,
the catalog of every error code and trap, the compiler pipeline, and the ownership
checker written out as algorithms. [conformance/](conformance/README.md) holds the
programs every implementation must agree on.

## History

An earlier prototype compiler for a smaller language of the same name was removed from
the tree when the specification in `docs/` became the definition of Ingot. It is in git
history at commit `ab090de`.
