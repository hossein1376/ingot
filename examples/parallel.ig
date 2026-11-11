// Data-parallel sum of squares with std/par.ig: dynamic chunking across all
// cores, P-core QoS, one atomic add per chunk, and SIMD inside each chunk.
import "std/libc.ig";
import "std/par.ig";

struct Ctx {
    data: *f32,
    total: i64, // updated atomically, once per chunk
}

fn body(ctx: *u8, lo: i64, hi: i64) {
    let c: *Ctx = ctx as *Ctx;
    var acc: f32x4 = @zero;
    var i: i64 = lo;
    while i + 4 <= hi {
        let v: f32x4 = @load(f32x4, &c.*.data[i]);
        acc = @fma(v, v, acc);
        i += 4;
    }
    var s: f32 = @reduce_add(acc);
    while i < hi {
        s = @fma(c.*.data[i], c.*.data[i], s);
        i += 1;
    }
    @atomic_add(&c.*.total, s as i64); // exact: per-chunk sums stay below 2^24
}

pub fn main() -> i32 {
    let n: i64 = 4000000;
    let data: *f32 = malloc((n * 4) as u64) as *f32;
    for i in 0..n {
        data[i] = (i % 10) as f32;
    }
    var c: Ctx = Ctx.{ data: data, total: 0 };
    parallel_for(n, cpu_count(), QOS_USER_INTERACTIVE, &c as *u8, body);
    // one period (k = 0..9) sums to 285; there are n/10 periods
    printf(c"sum of squares = %lld (expected %lld) on %lld CPUs\n", c.total, 285 * (n / 10), cpu_count());
    if c.total != 285 * (n / 10) {
        return 1;
    }
    free(data as *u8);
    return 0;
}
