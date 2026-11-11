// Parallel SIMD matrix multiply (C = A x B, row-major f32), checked against a
// scalar reference. Rows are spread over all cores with std/par.ig, and each
// row update is a broadcast-FMA over 16 columns at a time, using four f32x4
// accumulators that stay in NEON registers.
import "std/libc.ig";
import "std/par.ig";

const N: i64 = 192;

struct Mats {
    a: *f32,
    b: *f32,
    c: *f32,
    n: i64,
}

fn rows(ctx: *u8, lo: i64, hi: i64) {
    let m: *Mats = ctx as *Mats;
    let n: i64 = m.*.n;
    let a: *f32 = m.*.a;
    let b: *f32 = m.*.b;
    let c: *f32 = m.*.c;
    for i in lo..hi {
        var j: i64 = 0;
        while j < n {
            var c0: f32x4 = @zero;
            var c1: f32x4 = @zero;
            var c2: f32x4 = @zero;
            var c3: f32x4 = @zero;
            for k in 0..n {
                let aik: f32x4 = @splat(f32x4, a[i * n + k]);
                let row: i64 = k * n + j;
                c0 = @fma(aik, @load(f32x4, &b[row]), c0);
                c1 = @fma(aik, @load(f32x4, &b[row + 4]), c1);
                c2 = @fma(aik, @load(f32x4, &b[row + 8]), c2);
                c3 = @fma(aik, @load(f32x4, &b[row + 12]), c3);
            }
            let out: i64 = i * n + j;
            @store(&c[out], c0);
            @store(&c[out + 4], c1);
            @store(&c[out + 8], c2);
            @store(&c[out + 12], c3);
            j += 16;
        }
    }
}

fn reference(a: *f32, b: *f32, n: i64, i: i64, j: i64) -> f32 {
    var s: f32 = 0.0;
    for k in 0..n {
        s = @fma(a[i * n + k], b[k * n + j], s);
    }
    return s;
}

pub fn main() -> i32 {
    let bytes: u64 = (N * N * 4) as u64;
    var m: Mats = Mats.{ a: malloc(bytes) as *f32, b: malloc(bytes) as *f32, c: malloc(bytes) as *f32, n: N };
    for i in 0..N * N {
        // small integers keep every partial sum exact in f32
        m.a[i] = ((i % 5) - 2) as f32;
        m.b[i] = ((i % 3) + 1) as f32;
    }
    let t0: i64 = @ticks();
    parallel_for(N, cpu_count(), QOS_USER_INTERACTIVE, &m as *u8, rows);
    let t1: i64 = @ticks();
    var bad: i64 = 0;
    for i in 0..N {
        for j in 0..N {
            if m.c[i * N + j] != reference(m.a, m.b, N, i, j) {
                bad += 1;
            }
        }
    }
    let secs: f64 = ((t1 - t0) as f64) / (@tick_freq() as f64);
    let gflop: f64 = 2.0 * ((N * N * N) as f64) / 1000000000.0;
    printf(c"matmul %lldx%lld: %lld mismatches, %.3f ms, %.2f GFLOP/s\n", N, N, bad, secs * 1000.0, gflop / secs);
    free(m.a as *u8);
    free(m.b as *u8);
    free(m.c as *u8);
    if bad != 0 {
        return 1;
    }
    return 0;
}
