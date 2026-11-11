// A checked @load of a whole f32x4 past the end of a slice panics (exit 101).
extern fn printf(fmt: *u8, ...) -> i32;

fn sum4(a: []f32) -> f32 {
    var acc: f32x4 = @zero;
    for i in 0..a.len step 4 {
        acc = acc + @load(f32x4, &a[i]);
    }
    return @reduce_add(acc);
}
pub fn main() -> i32 {
    var xs: [8]f32 = [8]f32.{ 1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0 };
    let ok: f32 = sum4(xs[0..8]);
    if ok != 36.0 { return 1; }
    printf(c"ok %.0f\n", ok as f64);
    let bad: f32 = sum4(xs[0..6]); // 6 is not a multiple of 4: lanes 4..7 run past the end
    return 2;
}
