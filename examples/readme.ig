// The example from README.md (kept in sync by hand).
extern fn printf(fmt: *u8, ...) -> i32;

fn dot(a: *f32, b: *f32, n: i64) -> f32 {
    var acc: f32x4 = @zero;                       // one NEON register
    var i: i64 = 0;
    while i < n {
        acc = @fma(@load(f32x4, &a[i]), @load(f32x4, &b[i]), acc);   // fmla v.4s
        i += 4;
    }
    return @reduce_add(acc);                      // faddp
}

pub fn main() -> i32 {
    var xs: [8]f32 = [8]f32.{ 1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0 };
    printf(c"%.1f\n", dot(&xs[0], &xs[0], 8) as f64);
    return 0;
}

test "dot of ones" {
    var ones: [4]f32 = [4]f32.{ 1.0, 1.0, 1.0, 1.0 };
    @assert(dot(&ones[0], &ones[0], 4) == 4.0);
}
