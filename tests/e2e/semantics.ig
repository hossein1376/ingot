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
