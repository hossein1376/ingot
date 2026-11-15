# The rest of `std`: plan and design order

The five base documents beside this file cover `rt`, `task`, `sync`, `mem` and `Env`.
`time.md` is the first module from the remaining list and is now designed; this file
lists the rest of section 35, what each needs from that base, and the order I recommend
designing them. It does not repeat their APIs.

## 1. Modules

**`std.time`.** Designed in `time.md`: `Clock`, `Instant`, `Duration`, `Timestamp` and
timers. Needs the monotonic clock and the park deadline from `rt`, and the manual clock
from `@test_env`. The test-clock step and the result shape of a fallible waitable timer
are still open there. This module is first in the design order and is done.

**`std.sys`.** The `unsafe` OS layer: `std.sys.macos`, `std.sys.linux`,
`std.sys.windows` behind one internal interface. Needs `when @target.os`, `extern`,
`unsafe`, and the OS declarations already listed in `rt.md` §5. It is the only module
that knows about file descriptors, `HANDLE`s, `kqueue`, `io_uring` and IOCP. The
public API above it must not expose those types. Design it together with `std.io`,
`std.fs`, `std.net` and `std.mem`, because each one names a different slice of the OS
surface.

**`std.io`.** `Reader`, `Writer`, buffered wrappers, block-until-parked calls, and
the `Formatter` that `Format` writes to. Needs `rt.park`/`rt.wake`, `std.sys`,
`std.mem` buffers and `std.time` for deadlines. Blocking calls park the task and fail
with the module's `.canceled` error in a canceled scope. The type of `@format` is
deferred to this design: 01-core §4.4 fixes the formatter's error recording, but the
writer types it accepts and the exact error it returns are open (D1).

**`std.fmt`.** `Formatter`, `Format` implementations for every scalar, vector, slice
and structural value, and the compile-time format string. Needs `std.io` for the
writer and error, `std.text` for number printing, and the compiler's checked format
string. The `Format` interface is built in and cannot move. The `@format` result type
and whether it takes a writer or a formatter are the same gap as `std.io` (L1).

**`std.fs`.** Files, directories, metadata, memory mapping and paths. Needs `std.sys`,
`std.io`, `std.mem` and `std.time`; `Fs` is the capability already in `Env`. Paths are
`[]u8`, `/` works everywhere, and Windows converts at the boundary with WTF-8. No
language gap.

**`std.net`.** TCP and UDP sockets, addresses and listeners. Needs `std.sys`, `std.io`,
`std.mem` and `std.time`. `Net` is the capability in `Env`; the public address type is
the same on all systems. No language gap.

**`std.text`.** UTF-8 validation and decoding, number parsing and printing, splitting
and searching (SIMD). Needs `std.mem`, `std.simd` and `std.str`; number printing is what
`std.fmt` calls. No allocation beyond a caller-provided buffer or allocator. No
language gap.

**`std.list`, `std.map`, `std.set`, `std.str`, `std.ring`, `std.heap`.** Owning
containers. `List[T, A]` is a growable array, `Map[K, V, A]` uses open addressing with
SIMD group probing, `Set` is the key-only form, `Str[A]` is a byte string, `ring` is a
fixed ring buffer, `heap` is a binary heap. All need `std.mem` (`Allocator`, `AllocFail`,
`Box`/`Pool` where useful), `std.hash` for hashers and `std.simd` for Map probing.
Every one is linear, carries its allocator, and uses `deinit` for `Discard` elements and
`deinit_with` for linear elements. The linear-argument failure shape of mem.md L2
applies to `push`, `insert` and `reserve`.

**`std.par`.** `Split`, `chunks`, `chunks_mut`, `zip`, `enumerate`, `sum`,
`map_reduce`, `try_each`, `sort`, `Arenas`. Needs `rt.par_for_range`,
`rt.par_for_split`, `std.task` and `std.mem`. The `Split` contract is the one in
`rt.md` §4. Reductions must split by length only, never by worker count, so results are
deterministic. `try_each` and `first_of` share the cancellation rule in task.md D2.

**`std.simd`.** `simd.F32`, `simd.U8` and the other native-width aliases, plus chunked
iteration helpers. Needs `@target.simd_bits`, `@lanes`, `@load`/`@store` and the vector
builtins. It has no runtime dependency. No language gap beyond the builtin reference.

**`std.sort`.** Stable and unstable sorting over slices and `Split`. Needs `std.mem`
(for scratch space) and `std.par` for the parallel form. Comparisons use `Ord`; the
sort is generic over `T: Ord` and is linear in the scratch it takes. No language gap.

**`std.math`.** Correctly rounded elementary functions with the same bits on every
target. Needs scalar floating point and the compiler's math library; `@fma` is explicit.
It must not use a target's libm, because that would make results differ. No runtime
dependency. No language gap.

**`std.hash`.** `Hasher` (built-in interface parameter) and concrete hashers for bytes,
integers and slices. Needs `std.mem` only if a hasher streams; the default hashers are
allocation-free. No language gap.

**`std.rand`.** `Rand` capability and PRNGs. Needs `std.sys` for OS entropy,
`std.mem` for state buffers, and the `Rand` in `Env`. The PRNG algorithms are fixed and
documented; no language gap.

**`std.exec`, `std.gpu`.** `exec.seq`, `exec.simd`, `exec.par` are the CPU executors;
`gpu.Device`, `Queue`, `Buffer`, `Grid` are the GPU side. Needs the kernel ABI and the
compiler's kernel IR, `rt.par_for` for `exec.par`, `std.sys` for Metal/Vulkan, and
`std.mem` for buffers. The GPU API is the least mature part of the specification;
`f64` is excluded from Metal, `view` exists only under `when @target.gpu_unified`, and
grid kernels are reserved and must not be accepted. The kernel IR and its emitters are
a pipeline gap (pipeline §11).

**`std.linalg`.** Dense vectors and matrices, `matmul` using SME on the M4 and AVX-512
on the 7800X3D. Needs `std.simd`, `std.exec`, `std.gpu` and `std.math`; it is the first
consumer of `arch.aarch64.sme`. No language gap beyond the SIMD and kernel gaps.

**`arch.aarch64`, `arch.x86_64`.** Per-instruction intrinsics, under `when`. Needs the
target feature queries of `@target` and the `extern`/`unsafe` machinery. A program that
imports one compiles only for that target. No runtime dependency.

## 2. Order

1. `std.time`: every I/O and timer design needs `Duration`, `Instant` and `Clock`.
2. `std.sys`: settle the OS surface and the `when` split before `io`, `fs`, `net` and
   `gpu` each invent one.
3. `std.io` and `std.fmt`: together, because `@format` is the boundary between them.
   `std.text` comes with the number printing `fmt` needs.
4. `std.fs` and `std.net`: the first real users of `io` and `sys`.
5. `std.str` and the containers: `list`, `str`, `ring`, `heap`, then `map`/`set`
   (Map needs the most design: SIMD probing and linear keys).
6. `std.simd`, `std.par`, `std.sort`: data-parallel primitives after the allocators
   and containers exist.
7. `std.math`, `std.hash`, `std.rand`: independent leaves; can be done as needed.
8. `std.exec`, `std.gpu`, `std.linalg`: last, because they depend on the kernel ABI
   and on every layer below them.
9. `arch.aarch64`, `arch.x86_64`: with the first intrinsic a module needs, not before.

The ordering is a dependency order, not a release order. A module may be designed
earlier if a user needs it; the documents should still name the base it assumes.

## Decisions for the human

- **D1. `@format`'s type.** **Decided: deferred** to the `std.io` design. 01-core §4.4
  already says a `Formatter` records the first write error itself and `@format` returns
  it, so `format` cannot fail. What remains open is which writer types `@format` accepts
  and the exact error type it returns. No note from the earlier review applies.
- **D2. Design order.** **Decided: accepted.** The order in §2 above.
- **D3. `std.sys` visibility.** **Decided: accepted.** `pub` only inside `std` in
  practice, with every item documented as an unsafe OS primitive.
- **D4. Container error names.** **Decided: accepted.** Per-container error enums with a
  `from AllocErr` variant for the no-value case and an `AllocFail[T]` payload for the
  value case.
- **D5. `std.math` scope.** **Decided: accepted.** A fixed correctly rounded set, not a
  full libm.

## Language questions

- **L1. `@format`'s type.** The format string is checked at compile time (§35), and
  01-core §4.4 fixes where the first write error is recorded, but the writer types and
  the returned error are still not specified. This must be settled before `Formatter`
  can be designed.
- **L2. Kernel IR and the GPU emitters.** Pipeline §11 says the kernel IR and its
  three emitters (masked vector code, Metal Shading Language, SPIR-V) are not
  specified. `std.exec` and `std.gpu` cannot be implemented against a contract that
  does not exist. Sections 27 and 28 are involved.
- **L3. `@format` and the structural `Format` form.** The compiler generates a
  structural form for a type that does not implement `Format`, and `main`'s error line
  uses it. The language does not say where that generated implementation lives, whether
  it is visible to `ingot api`, or how a user type opts out. Section 33.1 and §35 are
  involved.
- **L4. `std.sys` in `ingot api`.** Section 35 says the public API is the same on all
  targets, and `std.sys` is the unsafe layer. It does not say whether `std.sys` items
  are part of the public API that `ingot api std` prints. The plan D3 choice needs a
  rule so the tool and the library agree.

## Traps in std

Section 35: "Nothing in `std` panics on bad input. A trap in `std` is a bug in the
caller's use of indices or in `std`." The named runtime failures of section 16.1
(T11–T13 and T17) are allowed. Every other case below is an error or a no-op, never a
trap.

| Place | What the document said | Classification |
|---|---|---|
| `rt.md` §1 `trap` | the compiler's trap entry, codes T01–T17 | named runtime failure, section 16.1 |
| `rt.md` §1 `stack_probe` / `stack_overflow` | guard page | T11, named in section 16.1 |
| `rt.md` §2 `spawn` | no task record or stack | T12, named in section 16.1 |
| `rt.md` L2 | `scope`/`Task` on a foreign thread | T17, named in section 16.1 |
| `mem.md` §9 `copy` | short destination | T02, index/slice misuse; allowed by section 35 |
| `mem.md` §1 `Allocator` | negative size or non-power-of-two alignment | neither; now `AllocErr.bad_request` |
| `mem.md` §3 `Vm.reserve` | negative or too-large size | neither; now `.bad_request` |
| `mem.md` §4 `Arena.init` | negative or too-large size | neither; now `.bad_request` |
| `mem.md` §11 `Allocator::free`/`resize` | broken safety precondition | section 16.2 undefined behavior, not a trap |
| `sync.md` §2 `Atomic` order | invalid or too-strong order | neither; now clamped, never trapped |
| `sync.md` §7 `WaitGroup.add` | negative `n` | neither; now a no-op |
| `sync.md` §6 `Chan.init` | capacity less than 1 | neither; now `.bad_capacity` |
| `time.md` §3 `after`, `after_ms`, `sleep` | negative delay | neither; now a zero delay |
| `time.md` §4 `@test_clock_step` | negative step | neither; now a zero step |
| `time.md` §4 manual test clock | every task parked with a timer that can never fire | T13, named in section 16.1 |
| `time.md` §1 `Duration` | arithmetic overflow | no trap; checked methods return `.none` and saturating methods clamp |

`T01`, `T03`–`T10`, `T14`, `T15` and `T16` are not raised by any design above; they
come from user expressions and remain the compiler's traps. T02 is the slice check on
the short destination in `mem.copy`, not a library trap entry. `T12` and `T13` are the
only runtime failures the library itself initiates.
