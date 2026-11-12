# The Ingot language specification

Status: **design draft, 2026-11-11.** This directory defines Ingot. There is no compiler
for it yet; [../ROADMAP.md](../ROADMAP.md) has the order of implementation.

## Files

| File | Contents |
|---|---|
| [01-core.md](01-core.md) | Lexical rules, modules, types, declarations, expressions, statements, errors, generics, compile-time evaluation |
| [02-memory.md](02-memory.md) | Ownership, moves, borrows, origins, linear resources, allocators, `unsafe`, the full list of traps and undefined behavior |
| [03-concurrency.md](03-concurrency.md) | Tasks, `scope`/`spawn`, `par for`, `Send`/`Sync`, channels, `select`, cancellation, the runtime's cost model |
| [04-hardware.md](04-hardware.md) | The three targets, layout, SIMD, kernels and executors, GPU, FFI |
| [05-tooling.md](05-tooling.md) | Diagnostics, cost reports, tests, packages, formatter, standard library outline |
| [grammar.md](grammar.md) | The complete EBNF |
| [rationale.md](rationale.md) | Why each major decision went the way it did, and what was rejected |
| [research.md](research.md) | Sourced facts about the hardware and the platform ABIs, and what is still unsourced |
| [impl/](impl/README.md) | For implementers: glossary, the diagnostic and trap catalog, compiler pipeline constraints, the ownership checker as algorithms |
| [../conformance/](../conformance/README.md) | Programs with an expected outcome, which every implementation must pass |

Sections are numbered once across files 01 to 05, so "section 13" means the same thing
everywhere.

## The goals, and what the design does about each

| Goal | Design response |
|---|---|
| **1. For agents, not humans** | Every function signature is a complete contract: types, errors, what the result borrows from, what it consumes. All checking is local to one function body, so a compiler error never depends on code the agent has not read. Syntax and semantics stay close to what models already know from Rust, Zig, Go and Swift, and leave that ground only where it removes a class of mistakes. Diagnostics and performance reports are JSON with fix edits. There are no warnings: a program is accepted or it is not. |
| **2. Two machines only** | Three targets over two CPUs: `aarch64-macos-m4`, `x86_64-linux-znver4` and `x86_64-windows-znver4`. The compiler assumes every feature of the CPU with no detection and no fallback paths. 64-bit little-endian is part of the language, so there is no `usize`. Cache-line size, page size and native vector width are compile-time constants. The operating system appears only at the FFI boundary and below the standard library. |
| **3. Performance first** | No GC, no hidden allocation, no implicit destructor calls, no operator overloading, no dynamic dispatch unless written `dyn`. Monomorphized generics. Arenas are the primary memory strategy. Every run-time check the compiler could not remove is listed by `ingot cost`. |
| **4. Prevent mistakes** | Safe code has **no undefined behavior**: no dangling borrows, no data races, no uninitialized reads, no out-of-bounds access, no silent integer overflow, no ignored errors, no leaked resources. Each one is either a compile error or a deterministic trap, identical in every build mode. |
| **5. Parallelism at the core** | Structured concurrency is in the language: `scope`/`spawn` for tasks and `par for` for data parallelism. The borrow rules that give memory safety also give data-race freedom, so parallel code may borrow from its parent's stack. A work-stealing runtime is linked only when a program uses it. |
| **Bonus: GPU** | A `kernel fn` is a restricted data-parallel function. The same kernel runs on an explicit executor: scalar, SIMD, threads × SIMD, or a GPU (Metal on the M4 Pro, Vulkan on the RTX 4070 under Linux and Windows). Offload is never automatic. |

## The design on one page

1. **Ownership without lifetime variables.** Values move by default. Borrows are `&T` and
   `&mut T`, with the usual rule that a place is either shared or exclusively borrowed.
   There are no `'a` parameters. A function that returns a borrow names the parameters it
   comes from (`-> []u8 from src`), and a function that stores a borrow says where
   (`item: []u8 into self`). The checker works one function at a time.
2. **Linear resources, explicit cleanup.** The compiler never inserts a destructor call.
   A `linear` type (a file, an arena, a task handle, an owning container) must be consumed
   exactly once on every path, and the compiler rejects a path that forgets. `defer` and
   `errdefer` are how cleanup is written.
3. **Arenas first.** Allocators are explicit values. Containers carry their allocator as
   a type parameter with static dispatch. Memory from an arena is an ordinary borrow of
   that arena, so the borrow checker stops use-after-reset.
4. **Errors are values.** `-> T ! E`, `try`, `catch`, `fail`. An error enum can declare
   which other error types it wraps, which lets `try` convert. Bugs (bounds, overflow, a
   failed assertion) trap and end the process. There is no unwinding.
5. **Arithmetic that cannot surprise.** `+ - *` trap on overflow, `+% -% *%` wrap and
   `+| -| *|` saturate. `as` only performs lossless conversions; lossy ones are named
   builtins. The semantics are the same in every build.
6. **Structured tasks.** `spawn f(x) in s` starts a task that cannot outlive scope `s`.
   Tasks are stackful, so any function may block and there is no `async` coloring.
   Borrowed data reaches a task as a `spawn` argument; locks and channels carry owned
   data only.
   `par for` splits a loop into chunks sized for uneven cores.
7. **Fixed-width SIMD on both CPUs.** `f32x4` to `f32x16` and friends are value types
   everywhere. A width that is wider than the native register is lowered to several
   registers. Comparisons give lane masks.
8. **Kernels.** One restricted function form that the compiler must be able to vectorize
   and to compile for a GPU, with no data races by construction.
9. **No ambient authority.** Safe code cannot call C and there are no global mutable
   variables, so a program can only touch files, the network, the clock or the heap
   through values that `main` received and passed down.
10. **One language on three systems.** The standard library's API is identical on macOS,
    Linux and Windows. `when @target.os == ...` holds what must differ, and
    `ingot check --all-targets` checks every branch.
11. **Tooling is part of the language.** `ingot check`, `fix`, `explain`, `api`, `cost`,
    `audit`, `test`, `fmt`. All speak JSON.

## Settled decisions

These were the open questions of the first draft. They are decided. The reasoning for
each is in [rationale.md](rationale.md).

1. **Name and extension.** *Ingot*, with `.ingot` files. (The earlier `.ig` collides
   with Modula-3.)
2. **Explicit linear cleanup, not implicit drops.** The compiler never inserts a
   destructor call. This costs a `defer` line per owned value and gives code with no
   hidden calls.
3. **No lifetime variables.** `from`/`into` clauses carry borrow flow. A struct that
   holds two borrows with different lifetimes is treated as holding one, which is
   sometimes stricter than Rust.
4. **Stackful tasks.** They need a small runtime (scheduler, stacks), linked only into
   concurrent programs.
5. **Backend.** Optimized builds emit LLVM IR on all three targets.
6. **Operating systems.** macOS on the M4 Pro; Linux and Windows on the 7800X3D.

Deciding these does not make them safe. Section 13 of the rationale lists where the
design is most likely to be wrong and how to test each point with a small prototype
before building the corresponding part.
