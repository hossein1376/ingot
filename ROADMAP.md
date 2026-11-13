# Roadmap

The specification in [docs/](docs/README.md) is the definition of the language. The
bootstrap front end, CFG ownership checker, CFG phase-10 pruning and evaluator in `src/`
cover item 1 steps 1 to 3 for the cases in [conformance/](conformance/README.md),
including package and module loading. Code generation and the standard library are still
open.

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

1. Front end, in Rust: lexer, parser, modules, name resolution, JSON diagnostics with fix
   edits.
2. Types and checking: structs, enums with payloads, `match`, `?T`, `T ! E`, tuples,
   interfaces and `impl`, generics with bounds, trapping arithmetic and the conversion
   builtins, compile-time evaluation.
3. A control-flow-graph IR, and on it the ownership checker: moves, linear consumption,
   borrows, origins. Everything in the standard library depends on this.
4. Code generation through LLVM IR, linking for Mach-O, and the Apple arm64 C ABI for
   `extern` and `export`.
5. Standard library core: `sys.macos`, `mem` (arena, heap), `list`, `map`, `str`, `fmt`,
   `fs`, `text`.
6. SIMD types and builtins.
7. Runtime: scheduler, task stacks, `scope`/`spawn`, `par for`, `sync`, kqueue I/O.
8. `ingot cost`, required check removal, `audit`, `layout`, `api`, `query`, `serve`, as
   [docs/impl/tools.md](docs/impl/tools.md) defines them. `fmt` needs its layout rules
   written first.

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

A formatter specification, a Tree-sitter grammar derived from `docs/grammar.md`, and a
Language Server adapter over `ingot serve`.

The deferred list in section 12 of the rationale: a struct-of-arrays container,
contracts, static borrow types, named origin parameters on structs, SME streaming
regions, GPU textures and multi-pass pipelines. Self-hosting the compiler.
