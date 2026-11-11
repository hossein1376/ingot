# Roadmap

## Done (v0.1)
- Lexer, parser, type checker, AArch64 backend (Mach-O and ELF assembly), driver.
- Integers, floats, bool, pointers, arrays, slices, structs, fn pointers and NEON vector types.
- Checked indexing and division. `@assert`, `@panic`, `test` blocks, `ingot test`.
- JSON diagnostics with fix edits, and `ingot fix`.
- C FFI with the Darwin variadic and stack-packing rules. pthreads, LSE atomics, `@qos`.
- Register promotion of locals, immediate operands, rotated loops.
- `enum` (with an explicit base type) and `switch` with exhaustiveness checking and fixes.
- Error unions: `-> T ! E`, `return @err(E.x)`, `try`, `catch e => value | { block }`; error code in x1, one `cbz` per check.
- `--link FILE` for hand-written assembly or C. `examples/sme/` is an SME FMOPA sgemm kernel called from Ingot, checked under qemu at SVL 256 and 512.
- `std/map.ig`: open-addressing u64 hash map with error-union API; `examples/hashjoin.ig`.
- Leaf functions take caller-saved `x2`–`x7` as extra temporaries (no save/restore), and promote up to 8 locals.
- Slice `{ptr,len}` register caching, dense-`switch` jump tables, tagged unions, generics, namespaced imports, `errdefer`, `ingot api`.
- Spilling under register pressure: binary operands and call arguments are parked in the frame, so deep expressions (tested to 120 levels) compile instead of failing with E0900.
- Lane masks (`@lanes_lt/le/eq`) and per-lane `@select` (BSL) for branchless SIMD.
- `@select(c, a, b)`: branchless `csel`/`fcsel`, written straight into promoted registers.
- `inline fn`: explicit call-site expansion; callee locals join the caller's register promotion.
- `ingot explain`: documented catalog of every diagnostic code.
- `defer` blocks (inline at every exit, LIFO, checked: no return/escape inside).
- In-place FMA accumulation into promoted NEON registers, logical immediates, `tst`, single-add addressing.
- `import`, an embedded `std` (libc, mem arena, par `parallel_for` with QoS), and compile-time `@is_macos()` branches.
- Examples include a parallel NEON matmul checked against a scalar reference.
- Tests: diagnostic goldens, e2e goldens under qemu, in-language tests, Mach-O header checks, determinism.

## Next
1. **Verify on real M4 Pro hardware.** Run `go test ./...` with `INGOT_CC=clang`, run `bench/run.sh TARGET=macos-arm64`,
   and confirm `@qos` placement with `powermetrics`.
2. **SSA IR and a real register allocator** (linear scan). This keeps values in registers across
   statements, allocates across inlined bodies, and generalizes today's pressure-driven spilling.
3. **Error unions, part 2**: `errdefer` and aggregate/vector results are done. Still to do: `!` in fn-pointer types, and error enums shared across modules.
4. **Tagged unions, part 2**: `union` with payloads, exhaustive `switch` and payload binding are done. Next are mutable payload access through a pointer binding (`U.v(*p)`), a niche-optimized `Option` for pointers. Union switches already use jump tables when dense.
4b. **`ingot fmt`, part 2**: today it re-indents by bracket depth and trims whitespace while keeping every token and comment (and verifies that by re-lexing). Next it should also normalize spacing inside lines and wrap long lines.
5. **Modules, part 2**: namespaced imports (`import "p" as m;`) are done. Still to do: `pub` visibility for non-exported names, a module-relative suggestion when the alias differs from the module prefix, and a deterministic package hash.
6. **Generics, part 2**: explicit monomorphized templates (`fn sum[T]`, `struct S[T]`, `sum.[f64](xs)`) are done. Next are optional bounds (`T: Num`), which would check a template once rather than per instance, and constant parameters (`[N: i64]`).
7. **`std`**: `std.mem` (arenas, `page_size()` from `sysconf`), `std.io`, `std.par` (a work-stealing pool
   with QoS-tagged workers, ≥3× chunks per core as Apple recommends), and `std.time` (`mach_absolute_time`).
8. **SME/SME2 in the language** (today it is reached through `--link` kernels): `streaming { ... }` regions that emit `smstart`/`smstop`, with ZA tile types for matrix
   kernels, gated by `--cpu apple-m4`.
9. **Direct Mach-O and ELF object writers** for faster builds with no external assembler. Keep `ingot asm` for inspection.
10. **JIT mode** (`ingot run --jit`) that follows Apple's W^X protocol (`MAP_JIT`, `pthread_jit_write_protect_np`,
    `sys_icache_invalidate`).
11. **Optional region and lifetime annotations** for pointers that can't outlive a scope, as warnings first.
12. **Self-hosting** the compiler in Ingot.
13. **Struct-by-value C ABI** (HFA and composite rules) and `extern` globals.
