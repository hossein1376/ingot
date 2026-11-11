// linecount.ig: count lines in a file with NEON byte compares.
// Uses namespaced std modules (io, time), error unions, and lane masks:
// @lanes_eq gives 0xFF in each matching byte lane, and subtracting the mask
// adds 1 to that lane's counter. Lane counters are flushed every 255 blocks
// so they cannot wrap.
import "std/libc.ig";
import "std/io.ig" as io;
import "std/time.ig" as time;

fn count_scalar(b: []u8) -> i64 {
    var n: i64 = 0;
    for i in 0..b.len {
        if b[i] == 10 as u8 {
            n += 1;
        }
    }
    return n;
}

fn flush(acc: u8x16) -> i64 {
    var s: i64 = 0;
    var tmp: [16]u8 = @zero;
    @store(&tmp[0], acc);
    for i in 0..16 {
        s += tmp[i] as i64;
    }
    return s;
}

fn count_neon(b: []u8) -> i64 {
    let nl: u8x16 = @splat(u8x16, 10 as u8);
    let blocks: i64 = b.len / 16;
    var total: i64 = 0;
    var acc: u8x16 = @zero;
    var run: i64 = 0;
    for k in 0..blocks {
        let v: u8x16 = @load(u8x16, &b.ptr[k * 16]);
        acc = acc - @lanes_eq(v, nl);
        run += 1;
        if run == 255 {
            total += flush(acc);
            acc = @zero;
            run = 0;
        }
    }
    total += flush(acc);
    for i in blocks * 16..b.len {
        if b[i] == 10 as u8 {
            total += 1;
        }
    }
    return total;
}

// Builds a pseudo-random text of n bytes (about one line per 40 bytes).
fn make_text(p: *u8, n: i64) -> i64 {
    var x: u64 = 0x9E3779B97F4A7C15 as u64;
    var lines: i64 = 0;
    for i in 0..n {
        x = x ^ (x << 13);
        x = x ^ (x >> 7);
        x = x ^ (x << 17);
        if x % (40 as u64) == 0 as u64 {
            p[i] = 10 as u8;
            lines += 1;
        } else {
            p[i] = (97 + (x % (26 as u64)) as i64) as u8;
        }
    }
    return lines;
}

fn run(path: *u8, n: i64) -> i32 ! io.IoErr {
    let p: *u8 = malloc(n as u64);
    defer { free(p); }
    let want: i64 = make_text(p, n);
    try io.write_file(path, p[0..n]);
    var data: []u8 = p[0..0];
    try io.read_file(path, &data);
    defer { free(data.ptr); }

    let t0: i64 = time.now_ns();
    let a: i64 = count_scalar(data);
    let t1: i64 = time.now_ns();
    let b: i64 = count_neon(data);
    let t2: i64 = time.now_ns();
    printf(c"bytes=%lld lines: expected=%lld scalar=%lld neon=%lld\n", data.len, want, a, b);
    printf(c"scalar %.3f ms, neon %.3f ms\n", (t1 - t0) as f64 / 1e6, (t2 - t1) as f64 / 1e6);
    if a != want || b != want {
        return 1;
    }
    return 0;
}

pub fn main() -> i32 {
    return run(c"/tmp/ingot_linecount.txt", 8000003) catch e => {
        printf(c"I/O error %d\n", e as i32);
        return 2;
    };
}
