// C interop: Ingot functions are plain C-ABI functions, so they can be
// passed to libc as callbacks (qsort) and exported with 'pub fn'.
extern fn printf(fmt: *u8, ...) -> i32;
extern fn snprintf(buf: *u8, n: u64, fmt: *u8, ...) -> i32;
extern fn strlen(s: *u8) -> u64;
extern fn qsort(base: *u8, n: u64, size: u64, cmp: fn(*u8, *u8) -> i32);

fn cmp_i32(a: *u8, b: *u8) -> i32 {
    let x: i32 = (a as *i32).*;
    let y: i32 = (b as *i32).*;
    if x < y {
        return -1;
    }
    if x > y {
        return 1;
    }
    return 0;
}

// Callable from C as: int32_t ingot_square(int32_t);
pub fn ingot_square(x: i32) -> i32 {
    return x * x;
}

pub fn main() -> i32 {
    var xs: [6]i32 = [6]i32.{ 42, -7, 19, 0, 3, -100 };
    qsort(&xs[0] as *u8, 6 as u64, @sizeof(i32) as u64, cmp_i32);
    for i in 0..6 {
        printf(c"%d ", xs[i]);
    }
    printf(c"\n");

    var buf: [64]u8 = @zero;
    let n: i32 = snprintf(&buf[0], 64 as u64, c"%s-%d-%.3f", c"ingot", ingot_square(12), 2.5);
    printf(c"formatted %d bytes: %s (strlen=%llu)\n", n, &buf[0], strlen(&buf[0]));
    return 0;
}
