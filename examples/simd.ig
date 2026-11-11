// First-class SIMD: f32x4 maps 1:1 to a 128-bit NEON register.
// Dot product of two 1M-element f32 arrays, scalar vs. 4x-unrolled NEON.
extern fn printf(fmt: *u8, ...) -> i32;
extern fn malloc(n: u64) -> *u8;
extern fn free(p: *u8);

const N: i64 = 1048576;

fn dot_scalar(a: *f32, b: *f32, n: i64) -> f32 {
    var acc: f32 = 0.0;
    for i in 0..n {
        acc = @fma(a[i], b[i], acc);
    }
    return acc;
}

// Four independent accumulators hide FMA latency on the wide M-series cores.
fn dot_simd(a: *f32, b: *f32, n: i64) -> f32 {
    var acc0: f32x4 = @zero;
    var acc1: f32x4 = @zero;
    var acc2: f32x4 = @zero;
    var acc3: f32x4 = @zero;
    var i: i64 = 0;
    while i + 16 <= n {
        acc0 = @fma(@load(f32x4, &a[i]), @load(f32x4, &b[i]), acc0);
        acc1 = @fma(@load(f32x4, &a[i + 4]), @load(f32x4, &b[i + 4]), acc1);
        acc2 = @fma(@load(f32x4, &a[i + 8]), @load(f32x4, &b[i + 8]), acc2);
        acc3 = @fma(@load(f32x4, &a[i + 12]), @load(f32x4, &b[i + 12]), acc3);
        i += 16;
    }
    var acc: f32 = @reduce_add((acc0 + acc1) + (acc2 + acc3));
    while i < n {
        acc = @fma(a[i], b[i], acc);
        i += 1;
    }
    return acc;
}

pub fn main() -> i32 {
    let bytes: u64 = (N * @sizeof(f32)) as u64;
    let a: *f32 = malloc(bytes) as *f32;
    let b: *f32 = malloc(bytes) as *f32;
    for i in 0..N {
        a[i] = ((i % 7) as f32) * 0.5;
        b[i] = ((i % 5) as f32) * 0.25;
    }
    let t0: i64 = @ticks();
    let s: f32 = dot_scalar(a, b, N);
    let t1: i64 = @ticks();
    let v: f32 = dot_simd(a, b, N);
    let t2: i64 = @ticks();
    printf(c"scalar=%.1f simd=%.1f\n", s as f64, v as f64);
    printf(c"ticks: scalar=%lld simd=%lld (freq %lld Hz)\n", t1 - t0, t2 - t1, @tick_freq());
    let lanes: f32x4 = @splat(f32x4, 2.0) * @load(f32x4, a);
    printf(c"lanes: %.1f %.1f %.1f %.1f max=%.1f\n", lanes[0] as f64, lanes[1] as f64, lanes[2] as f64, lanes[3] as f64, @reduce_max(lanes) as f64);
    free(a as *u8);
    free(b as *u8);
    return 0;
}

test "simd matches scalar on small input" {
    var xs: [20]f32 = @zero;
    var ys: [20]f32 = @zero;
    for i in 0..20 {
        xs[i] = i as f32;
        ys[i] = 2.0;
    }
    @assert(dot_simd(&xs[0], &ys[0], 20) == 380.0);
    @assert(dot_scalar(&xs[0], &ys[0], 20) == 380.0);
}

test "integer vectors" {
    let v: i32x4 = @splat(i32x4, 3) + @splat(i32x4, 4);
    @assert(@reduce_add(v) == 28);
    @assert(v[2] == 7);
}
