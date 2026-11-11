// Calls a hand-written SME kernel (sgemm_sme.S) from Ingot and checks it
// against a scalar reference. Build and run:
//
//   ingot run --link examples/sme/sgemm_sme.S examples/sme/sgemm.ig
//
// On M4 the kernel issues one 16x16 FP32 outer product (FMOPA) per k step.
// Under qemu-aarch64 it runs with qemu's default streaming vector length.
import "std/libc.ig";

extern fn sgemm_sme(at: *f32, b: *f32, c: *f32, m: i64, n: i64, k: i64);
extern fn sme_svl_floats() -> i64;

const M: i64 = 64;
const N: i64 = 64;
const K: i64 = 96;

pub fn main() -> i32 {
    let svl: i64 = sme_svl_floats();
    if (M % svl) != 0 || (N % svl) != 0 {
        printf(c"streaming vector length %lld floats does not divide %lld\n", svl, M);
        return 2;
    }
    let at: *f32 = malloc((K * M * 4) as u64) as *f32;
    let b: *f32 = malloc((K * N * 4) as u64) as *f32;
    let c: *f32 = malloc((M * N * 4) as u64) as *f32;
    defer {
        free(at as *u8);
        free(b as *u8);
        free(c as *u8);
    }
    for i in 0..K * M {
        at[i] = ((i % 7) - 3) as f32; // small integers: every sum is exact
    }
    for i in 0..K * N {
        b[i] = ((i % 5) - 2) as f32;
    }
    let t0: i64 = @ticks();
    sgemm_sme(at, b, c, M, N, K);
    let t1: i64 = @ticks();
    var bad: i64 = 0;
    for i in 0..M {
        for j in 0..N {
            var s: f32 = 0.0;
            for k in 0..K {
                s = @fma(at[k * M + i], b[k * N + j], s);
            }
            if s != c[i * N + j] {
                bad += 1;
            }
        }
    }
    let us: f64 = ((t1 - t0) as f64) / (@tick_freq() as f64) * 1000000.0;
    printf(c"SME sgemm %lldx%lldx%lld, SVL %lld floats: %lld mismatches, %.1f us\n", M, N, K, svl, bad, us);
    if bad != 0 {
        return 1;
    }
    return 0;
}
