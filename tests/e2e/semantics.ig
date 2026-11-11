// Language semantics checked with Ingot's own test blocks (run: ingot test).
extern fn printf(fmt: *u8, ...) -> i32;

var g_counter: i64 = 5;
var g_buf: [32]i32 = @zero;
const LIMIT: i64 = 1 << 10;

struct Inner { a: u8, b: i64 }
struct Outer { tag: u16, inner: Inner, vals: [3]f32 }

fn many(a: i64, b: i64, c: i64, d: i64, e: i64, f: i64, g: i64, h: i64, i: i64, j: i32, k: f64) -> f64 {
    return (a + b + c + d + e + f + g + h + i + (j as i64)) as f64 + k;
}

fn many_f(a: f64, b: f64, c: f64, d: f64, e: f64, f: f64, g: f64, h: f64, i: f32, j: f64) -> f64 {
    return a + b + c + d + e + f + g + h + (i as f64) + j;
}

fn twice(f: fn(i64) -> i64, x: i64) -> i64 { return f(f(x)); }
fn inc(x: i64) -> i64 { return x + 1; }

fn make_outer(t: u16) -> Outer {
    return Outer.{ tag: t, inner: Inner.{ a: 7 as u8, b: -3 }, vals: [3]f32.{ 1.0, 2.0, 3.5 } };
}

fn bump() -> bool { g_counter += 1; return true; }

test "integer wrap-around and narrow types" {
    let a: u8 = 250 as u8;
    let b: u8 = a + 10 as u8;
    @assert(b == 4 as u8);
    let c: i8 = 127 as i8;
    @assert(c + 1 as i8 == -128 as i8);
    let d: i32 = 2147483647;
    @assert(d + 1 == -2147483648);
    let e: u32 = 0 as u32;
    @assert(e - 1 as u32 == 4294967295 as u32);
}

test "signed vs unsigned comparison and division" {
    let x: i64 = -1;
    let y: u64 = 18446744073709551615 as u64;
    @assert(x < 0);
    @assert(y > 0 as u64);
    @assert(-7 / 2 == -3);
    @assert(-7 % 2 == -1);
    @assert(((7 as u64) / (2 as u64)) == 3 as u64);
}

test "shifts and bitwise need parentheses" {
    let m: i64 = (1 << 4) | (1 << 1);
    @assert(m == 18);
    @assert((m & 2) == 2);
    @assert((-16 >> 2) == -4);
    @assert((((0 - 16) as u64) >> (60 as u64)) == 15 as u64);
    @assert(LIMIT == 1024);
    @assert((~0 as i64) == -1);
}

test "floats and casts" {
    let f: f64 = 3.75;
    @assert(f as i64 == 3);
    @assert((-f) as i64 == -3);
    let h: f32 = (f as f32) * 2.0;
    @assert(h == 7.5);
    @assert(@sqrt(16.0) == 4.0);
    @assert(@min(3, 9) == 3);
    @assert(@max(2.5, -1.0) == 2.5);
    @assert((1 as f64) / 4.0 == 0.25);
    @assert(@fma(2.0, 3.0, 1.0) == 7.0);
}

test "short-circuit evaluation" {
    g_counter = 0;
    if false && bump() { @trap(); }
    @assert(g_counter == 0);
    if true || bump() { } else { @trap(); }
    @assert(g_counter == 0);
    let both: bool = bump() && bump();
    @assert(both && g_counter == 2);
}

test "loops, break, continue" {
    var s: i64 = 0;
    for i in 0..100 {
        if i % 2 == 0 { continue; }
        if i > 10 { break; }
        s += i;
    }
    @assert(s == 1 + 3 + 5 + 7 + 9);
    var n: i64 = 0;
    while true {
        n += 1;
        if n == 50 { break; }
    }
    @assert(n == 50);
}

test "globals" {
    g_buf[31] = 9;
    g_buf[0] = g_buf[31] * 2;
    @assert(g_buf[0] == 18);
    @assert(g_buf.len == 32);
}

test "stack-passed arguments" {
    @assert(many(1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 0.5) == 55.5);
    @assert(many_f(1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.5, 2.0) == 11.5);
}

test "function pointers" {
    @assert(twice(inc, 40) == 42);
    let f: fn(i64) -> i64 = inc;
    @assert(f(1) == 2);
}

test "nested aggregates returned by value" {
    let o: Outer = make_outer(9 as u16);
    @assert(o.tag == 9 as u16);
    @assert(o.inner.a == 7 as u8);
    @assert(o.inner.b == -3);
    @assert(o.vals[2] == 3.5);
    @assert(@sizeof(Outer) == 40);
    var p: Outer = o;
    let q: *Outer = &p;
    q.*.inner.b = 100;
    @assert(p.inner.b == 100 && o.inner.b == -3);
}

test "atomics" {
    var v: i64 = 10;
    @assert(@atomic_add(&v, 5) == 10);
    @assert(@atomic_cas(&v, 15, 1) == 15);
    @assert(@atomic_load(&v) == 1);
    @atomic_store(&v, 77);
    @assert(v == 77);
}

test "large aggregates copy correctly" {
    var a: [100]i64 = @zero;
    for i in 0..100 { a[i] = i * i; }
    let b: [100]i64 = a;
    a[99] = 0;
    @assert(b[99] == 9801);
    @assert(b[50] == 2500);
}

enum Op: u8 { Add, Sub, Mul = 10, Neg }

fn apply(op: Op, a: i64, b: i64) -> i64 {
    switch op {
        Op.Add => { return a + b; }
        Op.Sub => { return a - b; }
        Op.Mul => { return a * b; }
        Op.Neg => { return -a; }
    }
}

fn classify(n: i64) -> i64 {
    switch n {
        0 => { return 0; }
        1, 2, 3 => { return 1; }
        -5 => { return -1; }
        else => { return 99; }
    }
}

test "enums and exhaustive switch" {
    @assert(apply(Op.Add, 2, 3) == 5);
    @assert(apply(Op.Mul, 4, 5) == 20);
    @assert(apply(Op.Neg, 7, 0) == -7);
    @assert((Op.Neg as i64) == 11);
    @assert(@sizeof(Op) == 1);
    let o: Op = 1 as Op;
    @assert(o == Op.Sub);
    @assert(classify(2) == 1);
    @assert(classify(-5) == -1);
    @assert(classify(1000) == 99);
}

test "immediate forms: logical masks, tst, power-of-two multiply" {
    var x: i64 = 0x1234;
    @assert((x & 0xff) == 0x34);
    @assert((x | 0xf0) == 0x12f4);
    @assert((x ^ 1) == 0x1235);
    @assert(x * 8 == 0x91a0);
    @assert((x & 4) != 0);
    @assert((x & 8) == 0);
    let small: u8 = 200 as u8;
    @assert(small * 2 as u8 == 144 as u8);
    @assert((small & 0x0f as u8) == 8 as u8);
}

var d_log: [16]i64 = @zero;
var d_n: i64 = 0;

fn d_note(v: i64) {
    d_log[d_n] = v;
    d_n += 1;
}

fn d_early(flag: bool) -> i64 {
    defer { d_note(1); }
    var x: i64 = 10;
    defer { d_note(2); }
    if flag {
        defer { d_note(3); }
        return x + 5;
    }
    x = 20;
    return x;
}

fn d_loop() -> f64 {
    var acc: f64 = 0.5;
    for i in 0..4 {
        defer { d_note(100 + i); }
        if i == 1 {
            continue;
        }
        if i == 3 {
            break;
        }
        acc += 1.0;
    }
    defer { d_note(9); }
    return acc * 2.0;
}

test "defer runs in reverse order at every exit, after the return value" {
    d_n = 0;
    @assert(d_early(true) == 15);
    @assert(d_early(false) == 20);
    @assert(d_loop() == 5.0);
    let want: [10]i64 = [10]i64.{ 3, 2, 1, 2, 1, 100, 101, 102, 103, 9 };
    @assert(d_n == 10);
    for i in 0..10 {
        @assert(d_log[i] == want[i]);
    }
}

test "constant on the left of commutative operators; multiply by 2^s+1" {
    var v: i64 = 7;
    @assert(3 * v == 21);
    @assert(9 * v + 1 == 64);
    @assert(v * 17 == 119);
    @assert((0xf0 & v) == 0);
    @assert((1 | v) == 7);
    @assert(100 - v == 93);
    let neg: i32 = -5;
    @assert(5 * neg == -25);
    let b: u8 = 100 as u8;
    @assert(3 as u8 * b == 44 as u8);
}

enum MathErr: u16 {
    negative = 1,
    too_big = 300,
}

var r_defers: i64 = 0;

fn r_sqrtish(v: f64) -> f64 ! MathErr {
    if v < 0.0 {
        return @err(MathErr.negative);
    }
    if v > 1000.0 {
        return @err(MathErr.too_big);
    }
    return v * 0.5;
}

fn r_chain(v: f64) -> f64 ! MathErr {
    defer { r_defers += 1; }
    let a: f64 = try r_sqrtish(v);
    let b: f64 = try r_sqrtish(a * 4000.0);
    return a + b;
}

fn r_count_ok(n: i64) -> i64 ! MathErr {
    var ok: i64 = 0;
    for i in 0..n {
        let x: f64 = r_sqrtish((i * 100) as f64) catch e => {
            if e == MathErr.too_big {
                break;
            }
            continue;
        };
        if x >= 0.0 {
            ok += 1;
        }
    }
    return ok;
}

fn r_wrap(v: f64) -> i32 ! MathErr {
    let x: f64 = try r_sqrtish(v);
    return x as i32;
}

test "error unions: try propagates through defers, catch binds the code" {
    r_defers = 0;
    @assert((r_chain(0.25) catch _ => -1.0) == 250.125);
    let big: f64 = r_chain(2.0) catch e => (e as i64) as f64;
    @assert(big == 300.0);
    let neg: f64 = r_chain(-3.0) catch e => (e as i64) as f64;
    @assert(neg == 1.0);
    @assert(r_defers == 3);
    @assert((r_count_ok(50) catch _ => -1) == 11);
    @assert((r_wrap(9.0) catch _ => -1 as i32) == 4 as i32);
    @assert((r_wrap(-9.0) catch _ => -1 as i32) == -1 as i32);
}

inline fn i_sq(x: i64) -> i64 {
    return x * x;
}

inline fn i_clamp(v: f64, lo: f64, hi: f64) -> f64 {
    if v < lo {
        return lo;
    }
    if v > hi {
        return hi;
    }
    return v;
}

inline fn i_sum_to(n: i64) -> i64 {
    var s: i64 = 0;
    for i in 0..n {
        if i == 7 {
            return s + 1000; // early return from inside a loop
        }
        s += i;
    }
    return s;
}

inline fn i_axpy(a: f32x4, x: f32x4, y: f32x4) -> f32x4 {
    return @fma(a, x, y);
}

var i_hits: i64 = 0;

inline fn i_bump(limit: i64) {
    defer { i_hits += 1; }
    if i_hits > limit {
        return;
    }
    i_hits += 10;
}

fn i_twice(f: fn(i64) -> i64, v: i64) -> i64 { return f(f(v)); }

test "inline fns: nesting, early returns, loops, defers, vectors, address taken" {
    @assert(i_sq(i_sq(3)) + i_sq(2) == 85);
    @assert(i_sq(i_sq(2) + i_sq(1)) == 25);
    @assert(i_clamp(5.0, 0.0, 1.0) + i_clamp(-2.0, 0.0, 1.0) + i_clamp(0.25, 0.0, 1.0) == 1.25);
    @assert(i_sum_to(5) == 10);
    @assert(i_sum_to(100) == 1021);
    let v: f32x4 = i_axpy(@splat(f32x4, 2.0), @splat(f32x4, 3.0), @splat(f32x4, 1.0));
    @assert(v[3] == 7.0);
    i_hits = 0;
    i_bump(100);
    i_bump(5);
    @assert(i_hits == 12);
    var acc: i64 = 0;
    for k in 0..10 {
        acc += i_sq(k);
    }
    @assert(acc == 285);
    @assert(i_twice(i_sq, 3) == 81); // an inline fn still has an address
}

test "@select is a branchless choice for ints, floats, pointers and bools" {
    var n: i64 = 7;
    @assert(@select((n & 1) == 0, n >> 1, 3 * n + 1) == 22);
    n = 10;
    @assert(@select((n & 1) == 0, n >> 1, 3 * n + 1) == 5);
    let f: f32 = @select(n > 3, 1.5, -1.5);
    @assert(f == 1.5);
    let u: u8 = @select(n < 3, 200 as u8, 100 as u8);
    @assert(u == 100 as u8);
    let flag: bool = n == 10;
    @assert(@select(flag, 1, 2) == 1);
    @assert(@select(flag && n > 100, 1, 2) == 2);
    var a: i64 = 1;
    var b: i64 = 2;
    let p: *i64 = @select(flag, &a, &b);
    @assert(p.* == 1);
}

fn lane_clamp(x: f32x4, lo: f32x4, hi: f32x4) -> f32x4 {
    let a: f32x4 = @select(@lanes_lt(x, lo), lo, x);
    return @select(@lanes_lt(hi, a), hi, a);
}

test "lane masks and per-lane @select (fcmgt/cmhi/bsl)" {
    var buf: [4]f32 = [4]f32.{ -2.0, 0.5, 3.0, 1.0 };
    let x: f32x4 = @load(f32x4, &buf[0]);
    let c: f32x4 = lane_clamp(x, @splat(f32x4, 0.0), @splat(f32x4, 1.0));
    @assert(c[0] == 0.0);
    @assert(c[1] == 0.5);
    @assert(c[2] == 1.0);
    @assert(c[3] == 1.0);
    let m: i32x4 = @lanes_le(x, @splat(f32x4, 1.0));
    @assert(m[0] == -1 as i32);
    @assert(m[2] == 0 as i32);
    @assert(@reduce_add(m) == -3 as i32); // three lanes are <= 1.0
    let eq: i32x4 = @lanes_eq(@splat(i32x4, 7 as i32), m);
    @assert(@reduce_max(eq) == 0 as i32);
    var bytes: [16]u8 = @zero;
    bytes[3] = 200 as u8;
    bytes[9] = 100 as u8;
    let bv: u8x16 = @load(u8x16, &bytes[0]);
    let big: u8x16 = @lanes_lt(@splat(u8x16, 150 as u8), bv); // unsigned compare
    @assert(big[3] == 255 as u8);
    @assert(big[9] == 0 as u8);
    @assert(@reduce_max(big) == 255 as u8);
}
