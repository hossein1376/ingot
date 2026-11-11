# Design decisions

## Name: Ingot

An ingot is refined metal cast into a simple, regular block that is ready to be worked.
The name fits the goal: plain, regular source code that becomes "bare metal"
(Apple *silicon*) code with nothing in between. It's also short and easy to
pronounce, and the extension `.ig` doesn't collide with any common language tooling.

## Who the language is for: AI agents first

Agents produce a lot of code quickly and fix it from compiler feedback. Human
convenience features that create ambiguity cost agents iterations. So Ingot trades
brevity for regularity:

1. **The grammar is regular and keyword-led.** It is LL(1)-ish recursive descent
   with no declarator syntax, no significant whitespace, and no context-sensitive
   tokens. Struct literals are written `T.{...}`, which removes Rust's
   `if x == S { .. } {` ambiguity. Dereference is postfix `.*`, which reads left to
   right, so a tool can write `p.*.field` mechanically.
2. **Nothing is implicit.** There are no conversions, truthiness, shadowing,
   operator-precedence puzzles (mixing bitwise and other operators needs
   parentheses), uninitialized variables, auto-deref, or hidden allocation or
   copying. Each of these is a compile error with a fix edit.
3. **Diagnostics are an API.** Stable codes, exact spans, and fix edits come out
   as JSON Lines. `ingot fix` closes the loop without a model having to read prose.
4. **Self-checking is built in.** `test` blocks and `@assert` live in the language,
   so an agent can write code and its tests in one file and run `ingot test file.ig`.
5. **Builds are deterministic.** The same input gives byte-identical assembly (tested), so caches and diffs can be trusted.

## Performance model: what you write is what runs

* There is no GC, runtime, or scheduler. A binary is your code, a 10-instruction
  panic helper, and libc.
* Value types have C layout. Aggregates are passed by reference to a copy and
  returned through `x8`.
* SIMD types are first-class, map 1:1 to NEON registers, and builtins map 1:1 to
  instructions (`@fma` to `fmla`, `@reduce_add` to `faddp`/`addv`).
* Loops are rotated and conditions compile to compare and branch. Scalar locals are
  promoted to callee-saved registers. Vector locals stay in `v8`–`v15`. AAPCS64 keeps
  only their low 64 bits across a call, so a function that calls stores each promoted vector
  before a `bl` and reloads it after. That is cheap when calls are off the hot path. A slice
  that is never reassigned keeps `{ptr, len}` in a register pair, so `s[i]` is a
  `cmp`/`b.hs` against the length register plus one `add`, with no reloads from the frame.
  A leaf function (one that makes no calls) also uses the caller-saved `x2`–`x7` as
  temporaries. They need no save or restore, and they leave room to promote up to eight
  locals or slice registers.
  In a function that does make calls, a statement with no call in it (`let`,
  assignment or expression statement) still borrows `x2`–`x7`. If a temporary would
  outlive the statement, the function is regenerated without the borrowing.
* A dense `switch` (at least 4 values spanning at most 3× their count) compiles to one
  bounds check and a `br` through a table of 32-bit offsets. A sparse one becomes a
  compare chain.
* Safety checks (bounds and division) are explicit, cheap (`cmp` + `b.hs` to an
  out-of-line stub) and can be removed with `--unchecked`.
* Errors are values with a fixed cost. A fallible function (`-> T ! E`) returns its
  error code in `x1` next to the result, and each `try` or `catch` is one `cbz`. There are no
  exceptions, unwind tables or hidden allocation, and an error cannot be silently dropped
  (E0250).
* `defer` is expanded inline at every exit (fallthrough, `return`, `break`,
  `continue`, and a failing `try`), so cleanup costs exactly the deferred code.
* Inlining is opt-in (`inline fn`) and never heuristic, so the code an agent reads in
  `ingot asm` is predictable. `@select` gives branchless `csel`/`fcsel` when the agent
  knows a branch is unpredictable.
* Under register pressure, temporaries are parked in the frame instead of failing the
  build. A deep expression costs a store and a load, not an error.
* The heaviest M4 lever, SME (one FP32 `FMOPA` is a 16×16 outer product, about 18× a Neon
  core per the research notes), is reachable today through `extern fn` and `--link`
  kernels. It is planned as a language feature.

## Implementation language: Go

* Hossein, the first maintainer, is a Go engineer.
* It builds in seconds into one static binary that cross-compiles to `darwin/arm64`
  with `GOOS=darwin GOARCH=arm64 go build`, so the compiler itself runs natively on an M4 Pro.
* The standard library has what a compiler needs (`encoding/json` for diagnostics,
  `os/exec` for the toolchain) and there are no third-party dependencies.
* The compiler is meant to be self-hosted eventually (ROADMAP). Go is the bootstrap.

## Backend strategy

The backend emits AArch64 assembly text and then assembles and links with the
platform toolchain: clang on macOS, `zig cc` elsewhere. It does this because:

* It gives native code from day one on both object formats (Mach-O and ELF), with
  the system linker handling dyld stubs, code signing, and the GOT.
* The output is readable (`ingot asm`), which helps agents and humans check codegen.
* A direct Mach-O object writer is a later, mechanical optimization for compile
  speed (ROADMAP), not something correctness depends on.

The backend has two `Target`s that differ only in ABI details: symbol prefix,
relocation syntax, sections, variadic arguments on the stack, and stack argument
packing (see research.md §2).

## Deliberate omissions in v0.1

* **Inference and borrow checking.** Generics exist, but they are explicit: every
  instantiation is written `f.[T](x)`, and each one is a plain monomorphized copy with no
  inference, bounds or trait solving. An agent can predict exactly which code exists.
  Borrow checking is still out of scope (ROADMAP).
* **Struct-by-value across the C ABI.** The HFA and composite rules are
  error-prone, so the compiler rejects these cases rather than miscompile them.
* **Package management.** `import "p" as m;` gives a file its own namespace, but
  there are no packages, versions or hashes yet.
* **A full formatter.** `ingot fmt` only re-indents by bracket depth and trims whitespace. It
  never changes a token, so it is always safe for an agent to run.
