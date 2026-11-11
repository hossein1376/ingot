# Roadmap

## Done (v0.1)
- Lexer, parser, type checker, AArch64 backend (Mach-O and ELF assembly), driver.
- Integers, floats, bool, pointers, arrays, slices, structs, fn pointers and NEON vector types.
- Checked indexing and division. `@assert`, `@panic`, `test` blocks, `ingot test`.
- JSON diagnostics with fix edits, and `ingot fix`.
- C FFI with the Darwin variadic and stack-packing rules. pthreads, LSE atomics, `@qos`.
- Register promotion of locals, immediate operands, rotated loops.
- `import`, an embedded `std` (libc, mem arena, par `parallel_for` with QoS), and compile-time `@is_macos()` branches.
- Tests: diagnostic goldens, e2e goldens under qemu, in-language tests, Mach-O header checks, determinism.

## Next
1. **Verify on real M4 Pro hardware.** Run `go test ./...` with `INGOT_CC=clang`, run `bench/run.sh TARGET=macos-arm64`,
   and confirm `@qos` placement with `powermetrics`.
2. **SSA IR and a real register allocator** (linear scan). This removes the temp copies of promoted
   locals, keeps expression results in registers across statements, and adds spilling instead of the
   "expression too deep" error (E0900).
3. **Error unions**: `!T`, `try`, `catch`, and exhaustive `switch` on integer and enum values.
4. **Enums and tagged unions**, plus `switch` with exhaustiveness checking (matching the diagnostics style).
5. **Modules**: namespaced imports (`par.parallel_for`), visibility, and a deterministic package hash. v0.1 merges all imports into one namespace.
6. **Generics**: explicit monomorphized `fn sum[T: Num](xs: []T) -> T`, with no inference across calls.
7. **`std`**: `std.mem` (arenas, `page_size()` from `sysconf`), `std.io`, `std.par` (a work-stealing pool
   with QoS-tagged workers, ≥3× chunks per core as Apple recommends), and `std.time` (`mach_absolute_time`).
8. **SME/SME2 blocks**: `streaming { ... }` regions that emit `smstart`/`smstop`, with ZA tile types for matrix
   kernels, gated by `--cpu apple-m4`.
9. **Direct Mach-O and ELF object writers** for faster builds with no external assembler. Keep `ingot asm` for inspection.
10. **JIT mode** (`ingot run --jit`) that follows Apple's W^X protocol (`MAP_JIT`, `pthread_jit_write_protect_np`,
    `sys_icache_invalidate`).
11. **Optional region and lifetime annotations** for pointers that can't outlive a scope, as warnings first.
12. **Self-hosting** the compiler in Ingot.
13. **Struct-by-value C ABI** (HFA and composite rules) and `extern` globals.
