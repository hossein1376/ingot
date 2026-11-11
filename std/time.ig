// std/time.ig: a monotonic clock for timing kernels.
// clock_gettime clock ids differ by OS: CLOCK_MONOTONIC is 6 in the macOS
// SDK's <time.h> (_time.h) and 1 on Linux.
struct Timespec {
    sec: i64,
    nsec: i64,
}

extern fn clock_gettime(clock: i32, ts: *Timespec) -> i32;

// Nanoseconds from an arbitrary fixed point; only differences are meaningful.
fn now_ns() -> i64 {
    var ts: Timespec = @zero;
    if @is_macos() {
        clock_gettime(6, &ts);
    } else {
        clock_gettime(1, &ts);
    }
    return ts.sec * 1000000000 + ts.nsec;
}

// Seconds elapsed since start (a value from now_ns).
fn since_s(start: i64) -> f64 {
    return (now_ns() - start) as f64 / 1000000000.0;
}

test "now_ns is monotonic" {
    let a: i64 = now_ns();
    var x: i64 = 0;
    for i in 0..100000 {
        x += i;
    }
    let b: i64 = now_ns();
    @assert(x == 4999950000);
    @assert(b >= a);
    @assert(since_s(a) >= 0.0);
}
