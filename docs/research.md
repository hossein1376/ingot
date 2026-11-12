# Research notes

Facts that the specification depends on. Sections 1 to 3 link every number to a source;
those sources were read on 2026-11-11. Section 4 lists what the specification currently
states from general knowledge and still needs a source or a measurement on the machine.

## 1. Apple M4 Pro

| Fact | Source |
|---|---|
| The M4 Pro has a CPU of up to 14 cores: up to 10 performance (P) cores and 4 efficiency (E) cores. A 12-core bin with 8 P cores and 4 E cores also exists. | Apple newsroom, *Apple introduces M4 Pro and M4 Max* (2024-10-30), <https://www.apple.com/newsroom/2024/10/apple-introduces-m4-pro-and-m4-max/>; MacBook Pro tech specs, <https://support.apple.com/en-us/121553> |
| Unified memory bandwidth is 273 GB/s, with up to 64 GB of memory. | same newsroom post |
| ISA: Apple describes M4 as ARMv9.2-A **without SVE/SVE2**. LLVM's `apple-m4` CPU enables AES, SHA2, SHA3, FP16, FP16FML, BF16, **SME, SME2**, SMEF64F64 and SMEI16I64. | LLVM PR #95478 (`-mcpu=apple-m4`), <https://github.com/llvm/llvm-project/pull/95478>; LLVM PR #98267 (*apple-m4 is armv9.2-a*), <https://github.com/llvm/llvm-project/pull/98267>; Phoronix summary, <https://www.phoronix.com/news/Apple-M4-Added-To-LLVM-Clang> |
| LLVM's M4 tuning flags include fusion of arithmetic with branches (`ArithmeticBccFusion`, `ArithmeticCbzFusion`), address and literal fusion, AES fusion, **zero-cycle register moves** (`ZCRegMove`) and zero-cycle zeroing (`ZCZeroing`). | LLVM PR #95478 (`TuneAppleM4`) |
| E cores and P cores share the same instruction set. All E cores sit in a single cluster that shares an L2 cache. macOS controls cluster frequency in the kernel, so user code cannot set it. | Eclectic Light, *Inside M4 chips: E and P cores* (2024-11-18), <https://eclecticlight.co/2024/11/18/inside-m4-chips-e-and-p-cores/> |
| M4 was the first publicly available silicon with SME. | Remke & Breuer, *Hello SME!* (arXiv:2409.18779), <https://arxiv.org/abs/2409.18779> |
| On M4 the streaming vector length (SVL) is **512 bits**, so ZA is 4096 bytes and one FP32 `FMOPA` is a 16×16 outer product (512 flops). Measured on one M4 P core: Neon FP32 `FMLA` 113 GFLOPS, SME FP32 `FMOPA` 2009 GFLOPS with four ZA tiles in use (502 with one tile, so the latency is about 4 cycles). SSVE (streaming SVE) `FMLA` is only 31 GFLOPS. E cores reach 357 GFLOPS with `FMOPA`. The results point to two shared SME units, one per cluster, with about 2.3 TFLOPS combined. Direct loads into ZA are slower than loading into Z registers and then moving to ZA. These numbers are for the base M4 (an iPad Pro), not measured on an M4 Pro. | Remke & Breuer, *Hello SME!*, Sec. II-B, III-C, III-F, III-G and Table I |

**What this means for the design**

* **128-bit NEON is the native vector width**, because M4 has no SVE. Wider vector types
  are lowered to several registers (specification section 26).
* **SME is the path to matrix throughput**: about 18× a Neon core for FP32 in the
  measurements above. It needs an explicit mode switch (`smstart`/`smstop`) and its own
  register state, which is why the specification reaches it through a library first.
* **Overflow and bounds checks are cheap here.** Arithmetic followed by a conditional
  branch fuses, so a trapping `add` is close to free when the branch is predicted.
* **Explicit core placement is impossible, and steering is the sanctioned route.** No
  Apple document gives user code hard thread-to-core affinity on Apple silicon. Apple
  points developers to quality-of-service classes instead (section 3 below).
* We have not found a public, citable figure for M4 P-core decode width, reorder-buffer
  size or cache sizes, so the design does not depend on them.

## 2. The Apple arm64 ABI (differences from AAPCS64)

Source: Apple, *Writing ARM64 code for Apple platforms*,
<https://developer.apple.com/documentation/xcode/writing-arm64-code-for-apple-platforms>.
Base standard: Arm, *Procedure Call Standard for the Arm 64-bit Architecture*,
<https://github.com/ARM-software/abi-aa/blob/main/aapcs64/aapcs64.rst>.

| Rule | What the compiler must do |
|---|---|
| **`x18` is reserved.** | Never allocate `x18`. |
| **`x29` must always address a valid frame record.** | Keep a frame pointer in every function. |
| **Variadic arguments always go on the stack, in 8-byte slots** (and `va_list` is `char*`). | Put every variadic argument of an `extern` call on the stack. This is the most common cause of wrong `printf` output on Apple silicon. |
| **Non-variadic stack arguments are packed at their natural alignment**, not rounded up to 8 bytes. | Pack stack arguments for `extern` and `export` functions. |
| **The caller sign- or zero-extends arguments narrower than 32 bits.** (AAPCS64 makes the callee do it.) | Extend narrow integers before an `extern` call. |
| `char` is signed and `long` is 64 bits. `long double` is the same as `double`. | Nothing: Ingot declarations state exact widths. |
| Red zone: 128 bytes below `sp` that the OS will not touch. | Optional leaf-function optimization. |
| `sp` is 16-byte aligned. | Round frame sizes to 16. |
| C symbols have a leading `_`. External addresses go through the GOT. | Handled by the object writer or by LLVM. |

### Memory, pages and W^X

* **Pages are 16 KB on Apple silicon.** Source: Apple, *Porting your macOS apps to Apple
  silicon*,
  <https://developer.apple.com/documentation/apple-silicon/porting-your-macos-apps-to-apple-silicon>.
* **W^X:** memory is never writable and executable at the same time. JIT code must use
  `mmap(MAP_JIT)` and `pthread_jit_write_protect_np`. Source: Apple, *Porting
  just-in-time compilers to Apple silicon*,
  <https://developer.apple.com/documentation/apple-silicon/porting-just-in-time-compilers-to-apple-silicon>.
  Ingot compiles ahead of time, so this matters only if a JIT is ever added.
* **Mach-O executables on arm64 must be code-signed**, ad hoc at minimum.

## 3. Threads and P/E cores on macOS

Apple's guidance (*Tuning your code's performance for Apple silicon*,
<https://developer.apple.com/documentation/apple-silicon/tuning-your-code-s-performance-for-apple-silicon>,
and *Optimize for Apple Silicon with performance and efficiency cores*,
<https://developer.apple.com/news/?id=vk3m204o>):

* Placement on P or E cores is driven by **QoS classes**: user-interactive,
  user-initiated, utility, background. For pthreads, `pthread_set_qos_class_self_np`
  sets them. The values are the `qos_class_t` constants from `<sys/qos.h>` (0x21, 0x19,
  0x11, 0x09).
* Do not split work evenly across threads, because P and E cores progress at different
  speeds. Use many small chunks with work stealing, and at least 3× as many chunks as
  there are cores.

This is the basis for the worker groups and the chunking rule in specification sections
19 and 23.

## 4. Not yet sourced

The specification states the following from general knowledge. Each needs a primary
source or a measurement on the machine before the value is frozen.

**M4 Pro**

* `hw.cachelinesize` is 128 (specification: `@target.cache_line`).
* The exact feature list assumed for the target: LSE, CRC32, DotProd and the rest of
  the "other assumed features" row. `sysctl hw.optional.arm` on the machine settles it.
* Whether macOS on arm64 offers large pages to user code.
* Metal: shared-storage buffers created over page-aligned memory without a copy, and
  the absence of `double` in Metal Shading Language.

**Ryzen 7 7800X3D**

* 8 cores, 16 threads, 96 MB L3, 64-byte cache lines.
* The AVX-512 subsets of Zen 4 as listed in the targets table, and that 512-bit
  operations run on a 256-bit datapath without a frequency penalty.
* LLVM's `znver4` CPU model and what it enables.

**Linux on x86-64**

* io_uring as the event interface, and which kernel version the runtime requires.
* System V AMD64 calling convention details for variadic calls and struct
  classification.
* Transparent and explicit 2 MiB huge pages.

**Windows on x86-64**

* The Microsoft x64 calling convention: four register arguments, 32 bytes of shadow
  space, `xmm6`–`xmm15` callee-saved, variadic floating-point arguments duplicated in
  integer registers.
* Stack rules for code that switches stacks: updating the stack base and limit in the
  thread information block, the guard-page mechanism, and `__chkstk` probing for frames
  larger than a page.
* `VirtualAlloc` reserve and commit semantics, the 64 KiB allocation granularity and
  the commit limit.
* I/O completion ports, and whether the newer I/O ring API is worth using instead.
* Thread affinity and CPU sets.
* Large pages needing the lock-pages privilege.

**RTX 4070**

* 12 GB of memory, Vulkan compute support under both systems, and `shaderFloat64`
  availability.

**Tools**

* Which object formats and architectures post-link optimizers (BOLT, Propeller) support
  today. The design assumes ELF only.
* Cranelift as a `--fast` backend: its support for Mach-O, ELF and COFF output on the
  two CPUs, and for 256- and 512-bit vectors.
* Whether Zen 4 exposes branch records to `perf` on Linux, and what sampling macOS
  allows on the M4 without special entitlements.
