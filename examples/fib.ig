// Recursive and iterative Fibonacci. printf is variadic: on macOS the
// compiler places variadic arguments on the stack, as the Apple ABI requires.
extern fn printf(fmt: *u8, ...) -> i32;

fn fib(n: i64) -> i64 {
    if n < 2 {
        return n;
    }
    return fib(n - 1) + fib(n - 2);
}

fn fib_iter(n: i64) -> i64 {
    var a: i64 = 0;
    var b: i64 = 1;
    for i in 0..n {
        let t: i64 = a + b;
        a = b;
        b = t;
    }
    return a;
}

pub fn main() -> i32 {
    printf(c"fib(25) = %lld\n", fib(25));
    printf(c"fib_iter(90) = %lld\n", fib_iter(90));
    return 0;
}

test "fib agrees with fib_iter" {
    for n in 0..20 {
        @assert(fib(n) == fib_iter(n));
    }
}
