// Integer-heavy kernel: total Collatz steps for 1..N.
extern fn printf(fmt: *u8, ...) -> i32;

// inline: expanded into the loop below (C -O2 inlines it on its own; Ingot asks for it explicitly)
inline fn steps(n0: i64) -> i64 {
    var n: i64 = n0;
    var s: i64 = 0;
    while n != 1 {
        n = @select((n & 1) == 0, n >> 1, 3 * n + 1);
        s += 1;
    }
    return s;
}

pub fn main() -> i32 {
    var total: i64 = 0;
    for i in 1..1000000 {
        total += steps(i);
    }
    printf(c"%lld\n", total);
    return 0;
}
