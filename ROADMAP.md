# Roadmap

The specification in [docs/](docs/README.md) is the definition of the language.

**Where it stands.** The bootstrap compiler in `crates/` covers item 1 steps 1 to 3
and the check-removal half of step 8: the front end, types and constant evaluation, the
CFG and the whole ownership checker, and phase-10 check removal with the bounds and
overflow part of `ingot cost`. `query`, `serve` and an LSP adapter work. Nothing is
generated or run yet: code generation (step 4) is next, and the backend design is under
review. [COMPILER.md](COMPILER.md) has the step-by-step record.

The standard library is being designed in [docs/std/](docs/std/) (`rt`, `task`, `sync`,
`mem`, `env`, `time`), with the language features it needs specified first (raw pointer
casts, `Cell`, atomics). `std/` holds only the declaration stubs the compiler loads as
`std` today.

## 0. Before writing the compiler

Test the bets listed in section 13 of [docs/rationale.md](docs/rationale.md) on
paper, by writing real programs in the language as specified and checking them by hand:

1. A hash map and a JSON parser, to see whether linear cleanup is tolerable.
2. Iterator adaptors, to see whether one origin set per struct is enough.
3. A recursive-descent parser running as a task, to size task stacks.
4. An integer-heavy kernel (a hash function, a bytecode interpreter), to see whether the
   required check removals in section 31 are enough.
5. A stencil and a histogram as kernels, to see whether the kernel subset is too narrow.
6. A pipeline of tasks joined by channels, to see what the `Owned` rule and
   scope-long borrows cost.

Source the facts in section 4 of [docs/research.md](docs/research.md) on the real
machines.

## 1. First target: `aarch64-macos-m4`

Follow [docs/impl/](docs/impl/README.md). Each step is done when the conformance tests
for its area pass, and each new rule or error code gets a test before it is
implemented.

1. **Done.** Front end, in Rust: lexer, parser, modules, name resolution, JSON
   diagnostics with fix edits.
2. **Done.** Types and checking: structs, enums with payloads, `match`, `?T`, `T ! E`, tuples,
   interfaces and `impl`, generics with bounds, trapping arithmetic and the conversion
   builtins, compile-time evaluation.
3. **Done.** A control-flow-graph IR, and on it the ownership checker: moves, linear
   consumption, borrows, origins, scopes and tasks. Everything in the standard library
   depends on this.
4. **Next.** Code generation through LLVM IR, linking for Mach-O, and the Apple arm64 C ABI for
   `extern` and `export`.
5. Standard library core: `sys.macos`, `mem` (arena, heap), `list`, `map`, `str`, `fmt`,
   `fs`, `text`. The design of `rt`, `task`, `sync`, `mem`, `env` and `time` is drafted
   in [docs/std/](docs/std/); bodies wait for code generation.
6. SIMD types and builtins.
7. Runtime: scheduler, task stacks, `scope`/`spawn`, `par for`, `sync`, kqueue I/O.
8. `ingot cost`, required check removal, `audit`, `layout`, `api`, `query`, `serve`, as
   [docs/impl/tools.md](docs/impl/tools.md) defines them. Done: check removal, the
   bounds and overflow items of `cost`, `query` and `serve`. Open: the other `cost`
   items (allocations, indirect calls, copies, spawns, locks, park points), `audit`,
   `layout`, `api`. `fmt` needs its layout rules written first.

## 2. Second target: `x86_64-linux-znver4`

1. x86-64 code generation with AVX-512, ELF, the System V AMD64 C ABI.
2. `sys.linux` with direct system calls, io_uring in the runtime, thread affinity.
3. `ingot check --all-targets`.

## 3. Third target: `x86_64-windows-znver4`

1. PE/COFF and the Microsoft x64 C ABI. Code generation is shared with the Linux target.
2. `sys.windows`: `kernel32`/`ntdll` imports, UTF-16 conversion at the boundary.
3. Runtime: stack switching that follows the Windows rules, I/O completion ports.

## 4. Kernels and the GPU

1. Kernel IR, and `exec.seq`, `exec.simd`, `exec.par` on all targets.
2. Metal backend on the M4 Pro.
3. Vulkan backend for the RTX 4070, on Linux and Windows.

## 5. Later

Once the first target runs real programs: `ingot build --profile` and an
`ingot profile` report for the author, then profile-guided optimization, under the
constraints in section 10 of [docs/impl/pipeline.md](docs/impl/pipeline.md).
Post-link layout optimization only if a measured program shows the need.

A formatter specification. (The Tree-sitter grammar and the Language Server adapter
over `ingot serve` exist: see [editors/](editors/README.md) and [LSP.md](LSP.md).)

Features deferred past v1, with why each waits: [docs/later.md](docs/later.md).
Self-hosting the compiler.
