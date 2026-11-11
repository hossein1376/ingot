# Research notes

This file collects the facts that shaped Ingot's design. Every number here
links to a source. Where nothing public settles a question, it says so
instead of guessing. Sources were read on 2026-11-11.

## 1. The target: Apple M4 Pro

| Fact | Source |
|---|---|
| The M4 Pro has a CPU of up to 14 cores: up to 10 performance (P) cores and 4 efficiency (E) cores. A 12-core bin with 8 P cores and 4 E cores also exists. | Apple newsroom, *Apple introduces M4 Pro and M4 Max* (2024-10-30), <https://www.apple.com/newsroom/2024/10/apple-introduces-m4-pro-and-m4-max/>; MacBook Pro tech specs, <https://support.apple.com/en-us/121553> |
| Unified memory bandwidth is 273 GB/s, with up to 64 GB of memory. | same newsroom post |
| ISA: Apple describes M4 as ARMv9.2-A **without SVE/SVE2**. LLVM's `apple-m4` CPU enables AES, SHA2, SHA3, FP16, FP16FML, BF16, **SME, SME2**, SMEF64F64 and SMEI16I64. | LLVM PR #95478 (`-mcpu=apple-m4`), <https://github.com/llvm/llvm-project/pull/95478>; LLVM PR #98267 (*apple-m4 is armv9.2-a*), <https://github.com/llvm/llvm-project/pull/98267>; Phoronix summary, <https://www.phoronix.com/news/Apple-M4-Added-To-LLVM-Clang> |
| LLVM's M4 tuning flags include fusion of arithmetic with branches (`ArithmeticBccFusion`, `ArithmeticCbzFusion`), address and literal fusion, AES fusion, **zero-cycle register moves** (`ZCRegMove`) and zero-cycle zeroing (`ZCZeroing`). | LLVM PR #95478 (`TuneAppleM4`) |
| E cores and P cores share the same instruction set. All E cores sit in a single cluster that shares an L2 cache. macOS controls cluster frequency in the kernel, so user code cannot set it. | Eclectic Light, *Inside M4 chips: E and P cores* (2024-11-18), <https://eclecticlight.co/2024/11/18/inside-m4-chips-e-and-p-cores/> |
| M4 was the first publicly available silicon with SME. | Remke & Breuer, *Hello SME!* (arXiv:2409.18779), <https://arxiv.org/abs/2409.18779> |
| On M4 the streaming vector length (SVL) is **512 bits**, so ZA is 4096 bytes and one FP32 `FMOPA` is a 16×16 outer product (512 flops). Measured on one M4 P core: Neon FP32 `FMLA` 113 GFLOPS, SME FP32 `FMOPA` 2009 GFLOPS with four ZA tiles in use (502 with one tile, so the latency is about 4 cycles). SSVE (streaming SVE) `FMLA` is only 31 GFLOPS. E cores reach 357 GFLOPS with `FMOPA`. The results point to two shared SME units, one per cluster, with about 2.3 TFLOPS combined. Direct loads into ZA are slower than loading into Z registers and then moving to ZA. These numbers are for the base M4 (an iPad Pro), not measured on an M4 Pro. | Remke & Breuer, *Hello SME!*, Sec. II-B, III-C, III-F, III-G and Table I |

**What this means for codegen**

* **128-bit NEON is the portable SIMD baseline.** M4 has no SVE, so
  Ingot's first-class vector types are fixed 128-bit types (`f32x4`, `i32x4`, ...). Each one
  maps to exactly one `V` register. SME/SME2 (streaming mode and the ZA tile) is the
  path to matrix throughput. It needs an explicit mode switch (`smstart` and `smstop`),
  so it belongs in a separate, explicit language feature. See ROADMAP. Until that exists,
  `--link` lets an Ingot program call a hand-written SME kernel
  (`examples/sme/sgemm_sme.S`). For FP32 matrix work this is the largest single speedup
  on the chip (about 18× a Neon core in the measurements above). The kernel is written for any
  vector length, so qemu can test it at 256 and 512 bits.
* **Register-to-register moves are cheap.** On cores with zero-cycle moves, a
  `mov` between registers is renamed instead of executed. The MVP backend leans on this:
  promoted locals are copied into temporaries with plain `mov`. Removing those copies is
  the job of the future register allocator, not a correctness issue.
* **Compare and branch can fuse.** Ingot lowers `if`, `while` and `for`
  conditions straight to `cmp` + `b.cond`, or to `cbz`/`cbnz`, and never
  materializes a boolean first. Loops are rotated so each iteration has exactly one
  backward branch.
* **Explicit core placement is impossible, and steering is the sanctioned route.** No
  Apple document gives user code hard thread-to-core affinity on Apple silicon. Apple
  points developers to Quality-of-Service classes instead (§3). We have not found a
  public, citable figure for M4 P-core decode width, ROB size or cache sizes, so
  Ingot makes no design decision that depends on them.

## 2. The Darwin arm64 ABI (differences from AAPCS64)

Source: Apple, *Writing ARM64 code for Apple platforms*,
<https://developer.apple.com/documentation/xcode/writing-arm64-code-for-apple-platforms>.
Base standard: Arm, *Procedure Call Standard for the Arm 64-bit Architecture*,
<https://github.com/ARM-software/abi-aa/blob/main/aapcs64/aapcs64.rst>.

| Rule | How Ingot handles it |
|---|---|
| **`x18` is reserved.** Don't use it. | The backend never allocates `x18`, on any target. |
| **`x29` must always address a valid frame record.** | Every function sets up `stp x29, x30` / `mov x29, sp`, so stack traces always work. |
| **Variadic arguments always go on the stack, in 8-byte slots** (and `va_list` is `char*`). | `emitCall` puts every variadic argument on the stack for `macos-arm64`. On Linux, AAPCS64 passes them in registers. This is the most common cause of wrong `printf` output on Apple silicon. |
| **Non-variadic stack arguments are packed at their natural alignment**, not rounded up to 8 bytes. | `stackSlot()` packs on Darwin and rounds to 8 on Linux, for both incoming and outgoing arguments. |
| **The caller sign- or zero-extends arguments narrower than 32 bits.** (AAPCS64 makes the callee do it.) | Ingot keeps every integer extended to 64 bits in registers, which satisfies the Darwin rule. Callees still re-extend promoted narrow parameters for Linux callers. |
| `char` is signed and `long` is 64 bits. `long double` is the same as `double`. | Ingot has no `char` or `long`; you write `i8`/`u8` and `i64`. FFI declarations say exactly what they mean. |
| Red zone: 128 bytes below `sp` that the OS won't touch. | Not used yet; every function has a frame. It's a possible leaf-function optimization. |
| `sp` is 16-byte aligned. | The frame size is rounded to 16. |
| C symbols have a leading `_`, and `adrp`/`add` use `sym@PAGE` / `sym@PAGEOFF`. External addresses go through the GOT (`@GOTPAGE`/`@GOTPAGEOFF`). | `codegen.Target` abstracts symbols, relocations and section names (`__TEXT,__text`, `__TEXT,__cstring`, `__DATA,__bss` via `.zerofill`). |

### Memory, pages and W^X

* **Pages are 16 KB on Apple silicon (4 KB is typical on x86 Linux).** Apple lists
  "virtual memory page sizes" and "cache line sizes" as known differences when porting,
  and says to query them at run time (`sysconf(_SC_PAGESIZE)`, `vm_page_size`).
  Sources: Apple, *Porting your macOS apps to Apple silicon*,
  <https://developer.apple.com/documentation/apple-silicon/porting-your-macos-apps-to-apple-silicon>.
  Ingot doesn't bake in a page size. Allocation goes through libc (`malloc`, `mmap`), and a
  future `std.mem.page_size()` will query it.
* **W^X:** Apple silicon enforces that memory is never writable and executable *at the
  same time*. JIT code must use `mmap(MAP_JIT)` and switch write access per thread with
  `pthread_jit_write_protect_np`, then invalidate the instruction cache.
  Hardened-runtime apps also need the `com.apple.security.cs.allow-jit` entitlement.
  Source: Apple, *Porting just-in-time compilers to Apple silicon*,
  <https://developer.apple.com/documentation/apple-silicon/porting-just-in-time-compilers-to-apple-silicon>.
  Ingot's MVP compiles ahead of time, so W^X doesn't apply yet. The planned in-process JIT
  mode (`ingot run --jit`) has to follow this protocol.
* **Mach-O executables on arm64 must be code-signed** (ad hoc at minimum). This is
  general macOS knowledge, not something we could exercise here. The `zig cc` link
  used on this box emits an `LC_CODE_SIGNATURE` load command, and `TestMachO` checks
  for it together with `LC_MAIN` and `LC_BUILD_VERSION`. Whether the kernel accepts
  that signature at exec time is *not verified*, because the build box is Linux.

## 3. Threads and P/E cores

Apple's guidance (*Tuning your code's performance for Apple silicon*,
<https://developer.apple.com/documentation/apple-silicon/tuning-your-code-s-performance-for-apple-silicon>,
and *Optimize for Apple Silicon with performance and efficiency cores*,
<https://developer.apple.com/news/?id=vk3m204o>):

* Placement on P or E cores is driven by **QoS classes**: user-interactive,
  user-initiated, utility, background. For pthreads, `pthread_set_qos_class_self_np` sets them.
* Don't statically split work evenly across threads, because P and E cores progress at
  different speeds. Instead, use many small chunks with work stealing (GCD's
  `dispatch_apply`), and at least 3× as many iterations as there are cores.

Ingot exposes this with no runtime of its own:

* `@qos(class)` calls `pthread_set_qos_class_self_np(class, 0)` on macOS and does
  nothing on Linux. The class values are the `qos_class_t` constants (0x21 interactive,
  0x19 user-initiated, 0x11 utility, 0x09 background) from `<sys/qos.h>`.
* Threads are plain pthreads through FFI. `fn` values are C function pointers, so
  `pthread_create(&t, null, worker, arg)` works directly. A work-stealing `std.par` is on
  the roadmap.
* Atomics use the ARMv8.1 LSE instructions (`ldaddal`, `casal`) plus `ldar` and `stlr`.
  These are single instructions, not LL/SC loops.

## 4. What languages do well or badly for AI-agent authors

This section is the design argument, drawn from known language properties rather than
measured studies. No rigorous public benchmark of LLM error rates across languages
exists, so we don't quote one.

| Property | Helps agents | Hurts agents | Ingot's choice |
|---|---|---|---|
| Grammar | Regular, keyword-led statements (Go, Zig) | Context-sensitive parsing (C declarators, C++ templates, Rust turbofish) | Every declaration and statement starts with a keyword or is an expression statement. Types are written `name: T`, never with C-style declarator syntax. |
| Implicit conversions | None (Go, Zig, Rust) | C integer promotions and usual arithmetic conversions | **None.** Mixing types is error `E0205`, with a machine-applicable `as` fix. |
| Operator precedence | Few levels | C's bitwise and comparison trap (`a & b == 0`) | Mixing bitwise, shift and arithmetic or comparison operators **requires parentheses** (`E0106`). Comparisons don't chain. |
| Truthiness | Must be explicit | `if (n)`, `if (ptr)` | Conditions must be `bool` (`E0307`), with a fix that adds `!= 0` or `!= null`. |
| Shadowing | None | Rust-style rebinding hides bugs in long generated code | Shadowing a local is error `E0211`. |
| Memory | Explicit and visible (C, Zig) | GC pauses (Go, Java); hidden allocations (C++, Swift) | No GC and no hidden allocation. Every heap byte comes from an explicit call. |
| Errors at compile time | Structured, stable codes (Rust's `--error-format=json`, `rustc --explain`) | Prose-only diagnostics (classic C compilers) | JSON Lines output with stable codes, exact spans, notes and **fix edits** that tools can apply without reading prose. |
| Lifetimes and borrowing | Safety | Borrow-checker fights cost agents many iterations | Deliberately left out of v0. Bounds checks and explicit pointers are the safety net instead. See ROADMAP for optional region annotations. |
| Build system | One command, deterministic | Make, CMake, autotools | `ingot build file.ig`. Output is byte-for-byte deterministic, and a test checks this. |
| Self-checking | Built-in tests (Zig, Rust, Go) | External frameworks | `test "name" { ... }` blocks and `@assert` are in the language; `ingot test` runs them. |

## 5. Toolchain reality for this repository

* The development box is **x86-64 Linux**. Ingot generates AArch64 assembly. Linux
  binaries are linked statically against musl with `zig cc` and run under
  **qemu-aarch64** user-mode emulation. macOS binaries are linked with `zig cc -target
  aarch64-macos`, which ships Apple's libSystem stubs, and are checked by inspecting
  the Mach-O header.
* **Not verified on real Apple hardware:** running the Mach-O output, ad hoc code
  signing, `@qos` placement behavior, `@ticks` frequency on macOS, and every timing
  number. The benchmark numbers in the README come from emulation and only show relative
  order.
