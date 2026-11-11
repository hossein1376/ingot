// Float-heavy kernel: repeated NEON dot products over 64K f32 (L2-resident).
extern fn printf(fmt: *u8, ...) -> i32;
extern fn malloc(n: u64) -> *u8;

fn dot(a: *f32, b: *f32, n: i64) -> f32 {
    var acc0: f32x4 = @zero;
    var acc1: f32x4 = @zero;
    var acc2: f32x4 = @zero;
    var acc3: f32x4 = @zero;
    var i: i64 = 0;
    while i < n {
        acc0 = @fma(@load(f32x4, &a[i]), @load(f32x4, &b[i]), acc0);
        acc1 = @fma(@load(f32x4, &a[i + 4]), @load(f32x4, &b[i + 4]), acc1);
        acc2 = @fma(@load(f32x4, &a[i + 8]), @load(f32x4, &b[i + 8]), acc2);
        acc3 = @fma(@load(f32x4, &a[i + 12]), @load(f32x4, &b[i + 12]), acc3);
        i += 16;
    }
    return @reduce_add((acc0 + acc1) + (acc2 + acc3));
}

pub fn main() -> i32 {
    let n: i64 = 65536;
    let a: *f32 = malloc((n * 4) as u64) as *f32;
    let b: *f32 = malloc((n * 4) as u64) as *f32;
    for i in 0..n {
        a[i] = ((i % 8) as f32) * 0.125;
        b[i] = 1.0;
    }
    var s: f64 = 0.0;
    for r in 0..2000 {
        s = s + (dot(a, b, n) as f64);
    }
    printf(c"%.1f\n", s);
    return 0;
}
