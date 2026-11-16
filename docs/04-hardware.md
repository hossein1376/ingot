# Ingot v1 specification, part 4: the machine

## 24. Targets

Ingot targets machines, not architectures. A target is one CPU model under one operating
system, and the compiler assumes all of that CPU.

| Target | Machine | Order of implementation |
|---|---|---|
| `aarch64-macos-m4` | Apple M4 Pro, macOS | first |
| `x86_64-linux-znver4` | AMD Ryzen 7 7800X3D, Linux | second |
| `x86_64-windows-znver4` | the same machine, Windows | third |

### 24.1 The two CPUs

| | M4 Pro | 7800X3D |
|---|---|---|
| Cores | up to 10 performance + 4 efficiency, no SMT | 8 cores, 16 threads, all alike |
| Vector ISA | NEON (128-bit), SME/SME2 with 512-bit streaming vectors, no SVE | AVX-512 (F, CD, BW, DQ, VL, IFMA, VBMI, VBMI2, VNNI, BITALG, VPOPCNTDQ, BF16), AVX2, FMA3, BMI1/2 |
| Other assumed features | LSE atomics, CRC32, AES, SHA2/SHA3, FP16, DotProd | POPCNT, LZCNT, MOVBE, ADX, AES-NI, VAES, GFNI, SHA |
| `@target.simd_bits` | 128 | 512 |
| `@target.cache_line` | 128 | 64 |
| Memory | unified, shared with the GPU | DDR5, 96 MB L3 cache |
| GPU | integrated, shares system memory | RTX 4070 with 12 GB of its own memory |

Everything about code generation that depends on the CPU is identical for the two x86-64
targets: the same vector types, the same instruction selection, the same layout.

### 24.2 The three operating systems

| | macOS | Linux | Windows |
|---|---|---|---|
| `@target.os` | `.macos` | `.linux` | `.windows` |
| Object format | Mach-O | ELF | PE/COFF |
| C ABI for `extern` and `export` | Apple arm64 | System V AMD64 | Microsoft x64 |
| `@target.page_size` | 16384 | 4096 | 4096, with address space reserved in 64 KiB units |
| Committing memory | on first touch | on first touch | explicit, and counted against the system commit limit |
| Event I/O | kqueue | io_uring | I/O completion ports |
| Core placement | quality-of-service classes only | per-thread affinity | per-thread affinity |
| GPU API | Metal | Vulkan | Vulkan |
| System interface | libSystem, because macOS has no stable system-call ABI | direct system calls | imports from `kernel32` and `ntdll` |
| Text at the system boundary | UTF-8 | bytes | UTF-16, converted by `std` |

The operating system is not allowed to shape the language. It shows up in exactly four
places: the C ABI at the foreign-function boundary (section 29), the `std.sys` layer and
what is built directly on it, the runtime's I/O and stack handling (section 23), and the
object format.

### 24.3 Rules

* Byte order is little-endian, pointers are 64 bits, and `i64` holds any length. These
  are properties of the language, not of a target.
* There is no run-time feature detection and no generic fallback. A binary built for one
  target is not expected to run on any other CPU.
* `@target` is a compile-time record: `.os`, `.arch`, `.cpu`, `.simd_bits`,
  `.cache_line`, `.page_size`, `.hybrid_cores`, `.smt`, `.gpu_unified`, and
  `.has(.sme)` style feature queries. `.arch` is `.aarch64` or `.x86_64`; `.cpu` is
  `.m4` or `.znver4`. A feature query names a row of the tables in section 24.1 in lower
  case, without punctuation: `.neon`, `.sme`, `.sme2`, `.lse`, `.crc32`, `.aes`,
  `.sha2`, `.sha3`, `.fp16`, `.dotprod` on the M4; `.avx512f`, `.avx512cd`, `.avx512bw`,
  `.avx512dq`, `.avx512vl`, `.avx512ifma`, `.avx512vbmi`, `.avx512vbmi2`,
  `.avx512vnni`, `.avx512bitalg`, `.avx512vpopcntdq`, `.avx512bf16`, `.avx2`, `.fma3`,
  `.bmi1`, `.bmi2`, `.popcnt`, `.lzcnt`, `.movbe`, `.adx`, `.aesni`, `.vaes`, `.gfni`,
  `.sha` on the 7800X3D. A feature that only the other CPU has is `false`; a name in
  neither list is error E1606. Use it with `when` (section 7.3). Core counts are
  not in it: the M4 Pro is sold with different numbers of cores, so the runtime reads
  them at start-up (`task.workers()`).
* Code that differs per operating system sits under `when @target.os == ...`. Because
  only the taken branch of a `when` is compiled, `ingot check --all-targets` checks a
  package once per target it lists, so a branch for another system cannot rot unseen.
  Checking for a target never needs that target's machine or libraries.
* The standard library's public API is the same on all three targets. A module that
  exists for one system only (`std.sys.windows`, `arch.aarch64`) says so in its path.
* The feature, cache-line and page values above must be confirmed on the real machines
  before they are frozen. [research.md](research.md) holds the sourced facts for the M4
  and lists what is still unsourced for the 7800X3D and for Windows.

## 25. Layout

* **Structs** are laid out in declaration order. Each field sits at the next offset that
  satisfies its alignment and the size is rounded up to the largest alignment. This is
  the C layout, so a struct of C-compatible fields can cross the FFI. The compiler does
  not reorder fields. `ingot layout T` prints offsets, sizes and padding bytes so that an
  agent can reorder them itself.
* **Alignment** can be raised: `struct Slot align(@target.cache_line) { ... }` on a
  type, `hits: Atomic[i64] align(128)` on a field, and on a `static`. This is how false
  sharing is avoided. The argument is a constant power of two, no smaller than the
  natural alignment of what it applies to and no larger than 65536.
* **Enums** store the tag first, then the payload at the alignment of the most aligned
  payload. `?&T`, `?&mut T`, `?[]T`, `?fn(...)` and `?extern fn(...)` use a null pointer
  for `.none` and are the size of the type without `?`. There is no other niche: layout
  is visible through `@sizeof` and `ingot layout`, so it is not a compiler's choice. An
  enum with no variants has size 0 and alignment 1.
* **Vectors** are as large as their width (16, 32 or 64 bytes) and aligned to their
  size. A **mask** `boolxN` in memory is `N` bytes, one per lane holding 0 or 1, aligned
  to its size.
* **Slices** are `{ ptr, len }`, 16 bytes. **Tuples** are structs with fields `0`, `1`...
* **`&dyn I`** is `{ data pointer, method table pointer }`.
* The calling convention between Ingot functions is not specified and may change. Small
  aggregates and slices travel in registers, and a `T ! E` returns its tag in a register
  next to the value so that `try` is one compare-and-branch. Only `export fn` and
  `extern` declarations use the platform C ABI.

## 26. SIMD

### 26.1 Types

A vector type is `<lane>x<count>`: lanes `i8 i16 i32 i64 u8 u16 u32 u64 f32 f64`, total
width 128, 256 or 512 bits. `f32x4`, `f32x8`, `f32x16`, `u8x16`, `u8x64`, `i64x2`,
`f64x8` and so on. `boolxN` is a mask of `N` lanes.

All of these exist on both CPUs:

| Width | M4 | znver4 |
|---|---|---|
| 128 | one NEON register | `xmm` |
| 256 | two NEON registers | `ymm` |
| 512 | four NEON registers | `zmm` |
| `boolxN` | lanes of all-ones or all-zeros | a `k` mask register |

A vector wider than the native width costs proportionally more instructions and nothing
else. `std.simd` provides `simd.F32`, `simd.U8` and so on as aliases for the widest
native vector of each lane type, and `@lanes(V)` is the lane count of a vector type, so a
loop can be written once and run at 4 lanes on the M4 and 16 on the 7800X3D.

Vectors are `Copy` values. They are passed and returned in registers.

### 26.2 Operations

| Operation | Notes |
|---|---|
| `+ - * /` and unary `-` on float vectors | lane-wise IEEE |
| `+% -% *%`, `+\| -\|` on integer vectors | lane-wise. Plain `+ - *` are **errors** on integer vectors because a per-lane overflow trap has no cheap form: the code chooses wrapping or saturating |
| `& \| ^ ~`, `<< >>` by a scalar count | integer vectors |
| `== != < <= > >=` | lane-wise, result `boolxN` |
| `& \| ^ !` on masks | |
| `v[k]` with constant `k` | read one lane. `@insert(v, k, x)` gives a copy with lane `k` replaced. `k` may depend on a constant generic parameter, checked per instance |
| `@splat(V, x)`, `@iota(V)` | broadcast; `0, 1, 2, ...` |
| `@lanes(V)` | the lane count of vector type `V`, a constant |
| `@select(mask, a, b)` | per-lane choice |
| `@any(m)`, `@all(m)`, `@count(m)`, `@first(m)` | mask queries. `@count` and `@first` give `i64` |
| `@mask_bits(m)` | the mask as a `u64`, lane 0 in bit 0 |
| `@fma(a, b, c)`, `@sqrt`, `@abs`, `@min`, `@max`, `@floor`, `@ceil`, `@round` | lane-wise. Each is also defined on a float scalar (`f32`, `f64`), with the same type for its operands and result. On integers, scalar or vector, they are not specified yet (the builtin reference, a known gap). On floats, scalars and vectors alike, `@min` and `@max` return the other operand when exactly one is NaN, and order `-0.0` below `0.0`, so the result does not depend on the order of a reduction |
| `@reduce_add`, `@reduce_min`, `@reduce_max`, `@reduce_and`, `@reduce_or` | float `@reduce_add` adds in a fixed pairwise tree, the same on both CPUs |
| `@shuffle(v, idx)`, `@shuffle(a, b, idx)` | constant permutation. `v`, `a`, `b`: `LxN`; `idx` a constant `[M]i32` literal, each in `0..N` (in `0..2N` for two sources, the upper half picking from `b`); the result is `LxM`, which must be a vector type (E1501). An index that is not constant or out of range is E1504 |
| `@lookup(table, idx)` | byte table lookup (`tbl` / `vpermb`): `table: u8xN`, `idx: u8xM`, result `u8xM`; a lane is `table[idx]`, or 0 when `idx >= N` |
| `as`, `@wrap`, `@sat`, `@nearest`, `@bits` | the conversions of section 5.4, lane-wise between vectors of the same lane count (`@bits`: same total size) |

### 26.3 Memory

| Builtin | Bounds check |
|---|---|
| `@load(V, s, i)` | one check that `0 <= i` and `i + lanes <= s.len` |
| `@store(s, i, v)` | the same, `s` is `[]mut T` |
| `@load_tail(V, s, i)` | reads the lanes that exist from `i` to `s.len` and zero-fills the rest. One check that `0 <= i <= s.len` |
| `@store_tail(s, i, v)` | writes only the lanes that fit |
| `@gather(V, s, idx)` | `V = LxN`, `s: []L`, `idx: i32xN` or `i64xN`; result `V`. One check that every index is in `0..s.len` (T14) |
| `@scatter(s, idx, v)` | `s: []mut L`, `idx` as for `@gather`, `v: LxN`; `void`. The same check (T14), then a check that the indices are distinct (T15) |

No alignment is required. `@load` in a loop whose condition is `i + lanes <= s.len` has
its check removed (section 31).

```ingot
pub fn dot(a: []f32, b: []f32) -> f32 {
    @assert(a.len == b.len);
    let n = @lanes(simd.F32);
    var acc = @splat(simd.F32, 0.0);
    var i: i64 = 0;
    while i + n <= a.len {
        acc = @fma(@load(simd.F32, a, i), @load(simd.F32, b, i), acc);
        i += n;
    }
    acc = @fma(@load_tail(simd.F32, a, i), @load_tail(simd.F32, b, i), acc);
    return @reduce_add(acc);
}
```

### 26.4 Exact instructions

`arch.aarch64` and `arch.x86_64` expose individual instructions as functions with the
vendor's names for the cases the portable set does not cover. A module that imports one
of them compiles only for that target, so it sits under a `when`.

SME on the M4 is the largest single lever for matrix work (see research.md). It needs a
processor mode switch and its own register state, so v1 reaches it through
`arch.aarch64.sme` and through `std.linalg`, whose matrix routines use it. Streaming-mode
regions as a language feature are deferred.

## 27. Kernels

A kernel is a function that is applied to every index of a range, with the guarantee
that the applications are independent. That guarantee is what lets one source function
run as a scalar loop, as a vectorized loop, across cores, or on a GPU.

```ingot
kernel fn saxpy(i: Idx, a: f32, x: []f32, y: []mut f32) {
    y[i] = a * x[i] + y[i];
}

kernel fn dot(i: Idx, x: []f32, y: []f32) -> f32 reduce(+) {
    return x[i] * y[i];
}

kernel fn threshold(i: Idx, cut: f32, src: []f32, dst: []mut u32) {
    if src[i] > cut { dst[i] = 1; } else { dst[i] = 0; }
}

dispatch saxpy(2.0, xs, ys) over 0..xs.len on exec.par;
let d: f32 = dispatch dot(xs, ys) over 0..xs.len on exec.simd;
```

### 27.1 Rules for a kernel

* A `kernel fn` whose first parameter is an index is a **kernel**. Any other `kernel fn`
  is a **kernel helper**: its parameters and result have uniform types (below), its body
  follows the same body rules, and it may be called from kernels, from helpers and from
  ordinary code alike. A kernel is never called: `dispatch` runs it (section 27.2).
* The first parameter of a kernel is the index: `Idx` for one dimension. `Idx` is a
  built-in type and needs no import. An index is an opaque `Copy` value: `i.pos()` gives
  its position as an `i32`, and `i.offset(d)` (with `d: i32`) gives the `Idx` that is
  `d` positions away, for use with `at_clamped` and `at_or`. `offset` saturates at the
  limits of `i32`, so a far offset reads as outside rather than wrapping to a valid
  position. Only the kernel's own index parameter, unchanged, may be used with `[]`.
* An index type exists only inside a kernel: as its first parameter, and as the type of
  a local or an expression in its body. A field, another parameter, a result, a helper
  or a function that is not a kernel cannot mention it.
* Kernels and helpers have no generic parameters. A body checked once against a bound
  such as `Num` cannot know whether `+` would trap for the type it is given.
* **Grid kernels are reserved.** `Idx2`, `Idx3`, `Grid[T]`, `GridMut[T]`, the members
  `.x()`, `.y()` and `.z()`, and a domain of more than one range are for kernels over two
  and three dimensions. Their layout and members are not specified yet, and a program
  that uses them is not accepted.
* The other parameters are **uniforms** (`Copy` scalars, vectors and `copy struct`s of
  them, by value), **inputs** (`[]T`) and **outputs** (`[]mut T`).
* A kernel has a result only with `reduce` (below). It has no error type.
* An output may be written only at the kernel's own index: `out[i] = ...`. It may be
  read at its own index.
* An input may be read at the kernel's own index with `in[i]`, and elsewhere only with
  `in.at_clamped(j)`, the element at `j`'s position clamped to `0..in.len`, or
  `in.at_or(j, default)`, the element at `j`'s position or `default` when that position
  is outside the input. Both take an `Idx`, so only kernels can use them. There is no way
  to write an out-of-bounds access, so no bounds trap is needed on any executor.
* Element and uniform types are built from `i32 u32 f32 bool`, vectors of up to four
  lanes of those, payload-free `copy enum`s, and `copy struct`s of those. `i64 u64 f64`
  are allowed if the executor
  supports them, which is known at compile time: `exec.seq`, `exec.simd` and `exec.par`
  support all three, and section 28 says which a GPU supports (Metal does not have
  `f64`). Every local and every value computed in a kernel body has one of these types:
  there are no arrays, borrows, optionals or results in a kernel.
* The body may use `let`, `var`, `if`, `match` on integers, `bool` and payload-free
  enums, `for` over a constant range, `while`, `loop`, `break`, `continue`, `return`, the
  builtins of sections 5 and 26 that cannot trap, and calls of kernel helpers. It may
  not allocate, borrow (`&` or `&mut`), call an ordinary function, call through a
  function value, a closure, `&dyn` or an interface bound, write a closure, spawn or
  open a `scope`, use `par for`, `dispatch` or `select`, block, fail (`try`, `fail`,
  `catch` or `orelse`), use `defer` or `unsafe`, or loop with `for` over a range that is
  not constant.
* Each kernel and helper body is checked on its own, so "this function is safe in a
  kernel" is part of its signature and an error is reported at the function that breaks
  the rules. The one check over more than one body is recursion: helpers that call each
  other in a cycle are an error at the call that closes it.
* Nothing in a kernel traps. Integer `+ - *`, unary `-` and `/ %` are errors inside a
  kernel: write `+%`, `+|`, `-%x`, `@div_or(a, b, fallback)`, `@rem_or(a, b, fallback)`
  (section 5.2). A shift by a count that is not a constant is written `@shl(x, n)` or
  `@shr(x, n)`, which take the count modulo the width. `@assert`, `@panic`,
  `@unreachable` and `@narrow` are not available.
* A kernel with `reduce(op)` returns one value per index, and the dispatch combines them
  with `op`, one of `+ * min max & | ^`. `+` and `*` take numbers and wrap for integers,
  `min` and `max` take numbers, and `& | ^` take integers and `bool`; on vectors each is
  lane-wise. The combination order is chosen by the executor. For floats this is the one
  place in the language where the association order is not fixed, and the signature says
  so. A dispatch over an empty range gives the identity of `op`: `0` for `+ | ^`, `1` for
  `*`, all bits set (`true`) for `&`, the largest value (`+inf` for floats) for `min` and
  the smallest (`-inf`) for `max`.

### 27.2 Dispatch

`dispatch k(args) over range on executor` first checks, once, that every input and
output covers the range (a trap otherwise), then runs the kernel for every index, and
returns when all of them are done. The executor expression has a type that implements
the built-in `Executor` interface.

* `k` is a kernel, named directly; a kernel helper or an ordinary function is E1515. A
  kernel is never called directly or used as a value; `dispatch` is the only way to run
  one.
* The range is `lo..hi` of `i64` with `0 <= lo <= hi`. Since a position is an `i32`, `hi`
  must not exceed `2^31 - 1`, and every input and output must have a length of at least
  `hi`. The check traps (T16) otherwise.
* On `exec.seq`, `exec.simd` and `exec.par`, an input is passed as `[]T` and an output as
  `[]mut T`; a GPU dispatch takes buffers instead (section 28).
* The value of a `dispatch` is `void` for a kernel without `reduce`, and the combined
  result for one with it. A `dispatch` of a `reduce` kernel is an expression whose value
  must be used: it cannot be a statement.

The executors:

| Executor | Runs the kernel as |
|---|---|
| `exec.seq` | a scalar loop on the current task |
| `exec.simd` | a loop over native-width vectors on the current task. Branches become masked operations |
| `exec.par` | `par for` over chunks, each chunk as `exec.simd` |
| a `&gpu.Queue` | a compute pass on that GPU (section 28) |

* The compiler **must** vectorize a kernel for `exec.simd`. The subset above is defined
  so that every construct has a masked vector form. There is no "could not vectorize".
* Which executor is fastest depends on the data size and where the data already is. The
  language never chooses. The source names the executor at each dispatch.
* Ordinary Ingot code is not auto-vectorized as a guarantee. The optimizer may vectorize
  a plain loop, and `ingot cost` reports whether it did, but only a kernel or explicit
  vector types make it certain.

## 28. GPU

Status: specified so that the kernel subset is designed against it. Implementation comes
after both CPU targets.

```ingot
fn blur_frame(dev: &gpu.Device, pixels: []mut f32) -> void ! gpu.Err {
    let q = try dev.queue();
    defer { q.deinit(); }
    var src = try dev.buffer.[f32](pixels.len);
    defer { src.deinit(); }
    var dst = try dev.buffer.[f32](pixels.len);
    defer { dst.deinit(); }

    try src.write(pixels);
    try dispatch blur1d(&src, &mut dst) over 0..pixels.len on &q;
    try dst.read(pixels);
}
```

* `env.gpu` is `?gpu.Device`. A program that is handed no device cannot reach the GPU.
* `gpu.Buffer[T]` is a linear resource owned by a device. In a GPU dispatch, an input
  parameter takes `&gpu.Buffer[T]` and an output takes `&mut gpu.Buffer[T]`, so the same
  exclusivity rule that prevents CPU data races prevents a buffer from being an input and
  an output of one dispatch.
* A GPU dispatch has type `T ! gpu.Err`. It parks the calling task until the pass is
  done, and the worker runs other tasks in the meantime.
* **Moving data is always explicit.** `write` and `read` copy between a slice and a
  buffer.
  * On the M4, CPU and GPU share memory. `write` and `read` are a `memcpy`, and
    `buf.view()` / `buf.view_mut()` give `[]T` / `[]mut T` straight into the buffer with
    no copy. Buffers are page-aligned shared-storage Metal buffers.
  * On the RTX 4070, `write` and `read` cross PCIe to the card's own 12 GB. `view` does
    not exist there: it is a compile error under that target, so code that needs it sits
    under `when @target.gpu_unified`.
* **Backends.** Metal on macOS: each kernel is translated to Metal Shading Language at
  build time. Vulkan compute on Linux and Windows: each kernel is compiled to SPIR-V at
  build time, and the one backend serves the RTX 4070 under both systems. The compiler
  emits both from the same kernel IR that feeds `exec.simd`.
* **What does not carry over.** `f64` kernels are rejected for Metal. Integer and
  `f32` arithmetic give the same results as on the CPU, except where the GPU's
  `@fma`/division rounding is documented by the vendor to differ, and `reduce` order.
* Deferred: textures and samplers, multi-pass pipelines without a round trip to the
  CPU, asynchronous submission, kernels that write a variable number of outputs.

## 29. Foreign functions

```ingot
extern "c" {
    fn write(fd: i32, buf: *u8, n: i64) -> i64;
    fn qsort(base: *mut u8, n: i64, size: i64, cmp: extern fn(*u8, *u8) -> i32);
    fn printf(fmt: *u8, ...) -> i32;
}

export fn ingot_checksum(data: *u8, len: i64) -> u32 {
    let bytes = unsafe("C caller guarantees len readable bytes at data") {
        @slice_from_raw(data, len)
    };
    return crc32(bytes);
}
```

* An `extern` block declares C functions. Calling one is allowed only in `unsafe`, and
  so is a call through an `extern fn` pointer, which may point to any C function. The
  name of a function declared in an `extern` block may be used as a value of its
  `extern fn` type: creating it is safe, calling it is not. A variadic one may not be
  used as a value.
* `export fn` defines a function with the platform C ABI and an unmangled symbol. Its
  parameters and result must be FFI types. The exported names of a program are one
  namespace (E1711 for two with one name); a package should prefix its exported names
  with its own name, and `std` uses `ingot_rt_`. The body runs outside any task
  (section 23).
* **The platform C ABI** is the target's: Apple arm64 on macOS, System V AMD64 on Linux,
  Microsoft x64 on Windows. The compiler implements all three, including their different
  rules for variadic calls and for structs passed by value.
* FFI types: integers, floats, `bool`, raw pointers, `extern fn(...)` pointers,
  `?extern fn(...)` (a nullable C function pointer, with the null niche), and
  `copy struct`s of FFI types with no raised alignment, passed and returned by each
  system's rule for a C struct of the same layout. Other optionals, `T ! E` and enums do
  not cross; a payload-free enum is converted in the source with `as` and `@enum`.
* `bool` is one byte holding 0 or 1, C's `_Bool`. A foreign function that returns
  anything else has produced an invalid value (section 16.2); C code that uses `int` as
  a truth value is declared with `i32`.
* Ingot has no unwinding. Every function is marked as not unwinding, and unwind tables
  are still emitted on all three systems, for the system's own exception dispatch and
  for debuggers and profilers.
* A declaration states exact widths. C's `long` is 64 bits on macOS and Linux and 32 on
  Windows, so a function with `long` in its C prototype is declared once per system,
  under `when`. The same goes for any function that exists on one system only, such as
  `write` above.
* `extern fn(A) -> R` is the type of a C function pointer. It is a different type from
  `fn(A) -> R`, whose convention is Ingot's own. The name of an `export fn` has the
  `extern fn` type.
* Variadic arguments must be integers of at least 32 bits, `f64`, or raw pointers. There
  is no implicit promotion: a narrower value is converted in the source.
* The ABI string selects how the runtime treats the call: `"c"`, `"c-blocking"` and
  `"c-bigstack"` (section 23).
* `unsafe("...") { expr }` with a single trailing expression is the one block form that
  has a value, so that a raw operation can be wrapped at the point of use.
* Libraries and frameworks to link are listed per target in `ingot.toml` (section 33),
  not in the source.
