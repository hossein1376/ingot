# Ingot

**A low-overhead systems language that AI agents write and Apple silicon runs.**

Ingot compiles straight to AArch64 machine code for the Apple M4 Pro (Mach-O,
Apple arm64 ABI) and for Linux on arm64 (ELF). It has no garbage collector, no runtime
and no implicit behavior, and it reports precise, machine-readable diagnostics with
fixes that can be applied automatically.

```ingot
extern fn printf(fmt: *u8, ...) -> i32;

fn dot(a: *f32, b: *f32, n: i64) -> f32 {
    var acc: f32x4 = @zero;                       // one NEON register
    var i: i64 = 0;
    while i < n {
        acc = @fma(@load(f32x4, &a[i]), @load(f32x4, &b[i]), acc);   // fmla v.4s
        i += 4;
    }
    return @reduce_add(acc);                      // faddp
}

pub fn main() -> i32 {
    var xs: [8]f32 = [8]f32.{ 1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0 };
    printf(c"%.1f\n", dot(&xs[0], &xs[0], 8) as f64);
    return 0;
}

test "dot of ones" {
    var ones: [4]f32 = [4]f32.{ 1.0, 1.0, 1.0, 1.0 };
    @assert(dot(&ones[0], &ones[0], 4) == 4.0);
}
```

## Why Ingot

| For agents | For the machine |
|---|---|
| One obvious spelling and a regular, keyword-led grammar | Native AArch64, no VM, no runtime |
| **No implicit conversions, truthiness, shadowing or precedence puzzles** | No GC; aggregates have C layout; heap use is explicit |
| `--diag json`: stable codes, exact spans, **fix edits**; `ingot fix` applies them | First-class NEON vectors (`f32x4`, `i32x4`, ...) with 1:1 instructions |
| `test` blocks and `@assert` in the language; `ingot test` runs them | LSE atomics, pthreads via FFI, `@qos` for P/E-core steering |
| Errors can't be ignored: `-> T ! E` with `try` / `catch`, and `defer` for cleanup | Error checks cost one `cbz` (code in `x1`), with no unwinding tables |
| Deterministic output (tested) | Darwin ABI details handled: `x18` reserved, variadic args on the stack, packed stack args |

See [docs/design.md](docs/design.md) for why things are this way,
[docs/spec.md](docs/spec.md) for the language and [docs/research.md](docs/research.md)
for the M4 Pro and ABI facts behind it, with sources.

## Quick start

You need Go 1.22+ and an assembler/linker. On an Apple-silicon Mac, Xcode's `clang` works as-is.
Anywhere else, `scripts/setup-toolchain.sh` installs `zig cc` and, on x86, `qemu-aarch64`.

```sh
go build -o ingot ./cmd/ingot

# macOS on Apple silicon (default target there)
./ingot run examples/fib.ig

# Linux / x86 hosts: build Linux arm64 binaries and run them under qemu
eval "$(scripts/setup-toolchain.sh)"
./ingot run  --target linux-arm64 examples/fib.ig
./ingot test --target linux-arm64 tests/e2e/semantics.ig
./ingot build --target macos-arm64 examples/simd.ig -o simd   # cross-links a Mach-O binary
./ingot asm  examples/fib.ig                                  # read the generated assembly
./ingot check --diag json broken.ig                           # JSON Lines diagnostics
./ingot fix broken.ig                                         # apply suggested fixes
./ingot explain E0106                                         # what a code means and how to fix it
```

Environment variables: `INGOT_CC` sets the toolchain command (for example `zig cc` or `clang`),
and `INGOT_RUNNER` sets the emulator for linux-arm64 on non-arm64 hosts.

## Diagnostics an agent can act on

```console
$ ingot check --diag json bad.ig
{"severity":"error","code":"E0106","message":"operator \"&\" mixed with \"==\" requires explicit parentheses",
 "span":{"file":"bad.ig","start":{"line":2,"col":12},"end":{"line":2,"col":17}},
 "notes":["Ingot has no implicit precedence between bitwise/shift operators and other operators"],
 "fixes":[{"message":"parenthesize","edits":[{"span":{...,"start":{"line":2,"col":12},...},"text":"("},
                                              {"span":{...,"start":{"line":2,"col":17},...},"text":")"}]}]}
```

Golden examples for each error class are in [`tests/diag/`](tests/diag). Every code has an entry in
`ingot explain` (`ingot explain --json` prints the whole catalog, one object per line), and a test
fails if the compiler emits a code that the catalog does not document.

## Examples

| File | Shows |
|---|---|
| [`examples/hello.ig`](examples/hello.ig) | libc call, C strings |
| [`examples/fib.ig`](examples/fib.ig) | recursion, loops, variadic `printf`, a test block |
| [`examples/structs.ig`](examples/structs.ig) | structs, pointers (`p.*.x`), arrays, slices, value semantics |
| [`examples/simd.ig`](examples/simd.ig) | `f32x4` dot product, 4 accumulators, `@ticks` timing |
| [`examples/threads.ig`](examples/threads.ig) | pthreads, `@atomic_add` (LSE), `@qos` P/E-core steering |
| [`examples/ffi.ig`](examples/ffi.ig) | Ingot function as a `qsort` callback, `snprintf`, exported `pub fn` |
| [`examples/errors.ig`](examples/errors.ig) | a bounds-check panic with `file:line:col` |
| [`examples/parallel.ig`](examples/parallel.ig) | `import "std/par.ig"`: dynamic-chunk `parallel_for` on all cores, NEON in each chunk |
| [`examples/result.ig`](examples/result.ig) | error unions: `-> i64 ! ParseErr`, `try` through `defer`, `catch e => value` and `catch e => { break; }` |
| [`examples/matmul.ig`](examples/matmul.ig) | parallel NEON matrix multiply (4 FMA accumulators in registers), `defer` cleanup, checked against a scalar reference |

The standard library (`std/libc.ig`, `std/mem.ig`, `std/par.ig`) is embedded in the compiler,
so `import "std/par.ig";` works with no setup.

## Status and test results

`go test ./...` covers JSON diagnostic goldens, the `ingot fix` round trip, e2e stdout and exit-code goldens
(run under qemu-aarch64), in-language `test` blocks (18 semantics tests plus the example tests),
unit tests for the lexer, parser, checker rules, C struct layout and fix application,
a mutation fuzzer (`internal/fz`: no compiler panics, no failure without a diagnostic; a 40,000-mutant
run put 9,798 mutants through codegen, and a sample of 244 of them assembled cleanly for both targets),
Mach-O header and load-command checks for every program, and byte-for-byte determinism on both targets.

Compile speed: a generated 35,000-line file (5,000 functions) type-checks and emits 281,000 lines
of assembly in about 0.19 s on the build box, an x86-64 Linux VM.

Benchmarks (`bench/run.sh`): **these ran under qemu-user on x86-64, so they are emulated**. Only the
relative order means anything:

| kernel | Ingot | C -O0 | C -O2 (zig cc / clang) |
|---|---|---|---|
| collatz (integer, branchy; the hot helper is an `inline fn`) | 0.42 s | 2.41 s | 0.33 s |
| dot (NEON f32x4 FMA) | 1.30 s | 3.06 s | 1.28 s |

**Not verified on real Apple hardware yet:** running the Mach-O binaries (they link, carry
`LC_MAIN`, `LC_CODE_SIGNATURE` and `LC_BUILD_VERSION`, and pass header checks), actual P/E
placement from `@qos`, and real M4 Pro timings. See [ROADMAP.md](ROADMAP.md).

## Layout

```
cmd/ingot          CLI driver
internal/syntax    lexer, AST, parser (precedence-mixing rule lives here)
internal/sema      types, C layouts, type checker, builtins
internal/codegen   AArch64 backend: targets (Mach-O/ELF), frames, register promotion, ABI
internal/diag      diagnostics, JSON output, fix application
internal/driver    phases + toolchain invocation
tests/             diag goldens, e2e goldens, in-language semantics tests
bench/             Ingot vs C kernels
```
