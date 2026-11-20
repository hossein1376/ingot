# After v1

Features that were considered and deliberately left for after v1. Nothing here is
rejected: a rejected idea goes in [rationale.md](rationale.md) §12 ("Left out on
purpose") or in [impl/decisions.md](impl/decisions.md) with its reason. Nothing here
is normative either; an entry becomes part of the language only when the specification
says so.

Each entry says what the feature is, why it waits, and what would bring it forward.
Add an entry when a design discussion ends with "later"; move it out (to the
specification, or to the rejected list with a reason) when that changes.

## Language

| Feature | Why it waits | Revisit when |
|---|---|---|
| A built-in struct-of-arrays container | Deferred in the first design | — |
| `requires`/`ensures` contracts on signatures | Deferred in the first design | — |
| `f16`, `i128` | Deferred in the first design | — |
| A borrow type known to be static, and so not a view | Deferred in the first design | — |
| Named origin parameters on structs | Deferred in the first design | — |
| A `packed struct` with bit fields | Deferred in the first design | — |
| A compiler-provided structural `Eq`, `Ord` and `Hash` for a plain struct, on request (not a general derive mechanism) | Only if writing `std` shows the need | `std` is written (R1 onwards) |
| `@config.name` values from `ingot.toml` for `when` | `when` cannot name a constant | — |
| Conversions between raw pointers and `extern fn` pointers (`dlsym`, GPU function tables) | Needed first by the GPU runtime (COMPILER.md, "SIMD and kernels") | Step G1 |

## Hardware

| Feature | Why it waits | Revisit when |
|---|---|---|
| Streaming-mode (SME) regions | The routines of `arch.aarch64.sme` are whole routines in assembly first (COMPILER.md, "SIMD and kernels") | — |
| GPU textures, samplers and multi-pass pipelines | 04 §28 | after G1 |
| Grid kernels (`Idx2`, `Idx3`, `Grid[T]`, `GridMut[T]`) | Reserved names (04 §27.1) | — |

## Packages and builds

| Feature | Why it waits | Revisit when |
|---|---|---|
| Test files beside a module: `x.test.ingot`, part of module `x`, loaded only by `ingot test` (and by `check`, `fmt` and the LSP) | v1 keeps "one module is one file" (*user*, 2026-11-19). Tests that need only public or `pub(pkg)` items can already live in a module of their own | A module's tests crowd out its code |
| Dev-dependencies | Until a package is hurt by the lack (05 §33.2) | — |
| A C-callable static library as a second build output | Needs a decision on who starts the runtime, and a generated header | — |
| Stub libraries for cross-linking written by the compiler from the `extern` declarations | Each `extern` block would have to name its library (COMPILER.md, "Linking") | Cross-target builds (T2, T3) |

The test-file design, as discussed: the file sees every item of its module, private
ones included; its own helper items are visible only inside it, so the module's code
cannot come to depend on something `ingot build` leaves out; it holds `test` and `bench`
blocks, imports and private helpers, and a `pub` item in it is an error; `build`, `api`
and `audit` skip it. The dot in `x.test.ingot` cannot appear in a module name, so the
convention never captures a real module (unlike `x_test.ingot`). It needs codes for a
test file without its module, a `pub` item in a test file, and a name declared in both.

## Tooling and optimization

| Feature | Why it waits | Revisit when |
|---|---|---|
| `ingot build --profile` and an `ingot profile` report, then profile-guided optimization | Needs real programs first; constraints in [impl/pipeline.md](impl/pipeline.md) §10 | The first target runs real programs |
| Post-link layout optimization | Only if a measured program shows the need | A measurement |
| Self-hosting the compiler | — | — |
