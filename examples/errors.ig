// Runtime safety: bounds checks are on by default and report file:line:col.
// Build with --unchecked to remove them in hot code you have verified.
extern fn printf(fmt: *u8, ...) -> i32;

fn get(xs: []i64, i: i64) -> i64 {
    return xs[i];
}

pub fn main() -> i32 {
    var data: [4]i64 = [4]i64.{ 10, 20, 30, 40 };
    printf(c"data[3] = %lld\n", get(data[0..4], 3));
    printf(c"data[4] = %lld\n", get(data[0..4], 4)); // panics: index out of bounds
    return 0;
}
