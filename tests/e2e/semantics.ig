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

fn vsum_with_calls(xs: []f32) -> f32 {
    // acc stays in v8-v15 across the calls to helper(): saved and reloaded
    var acc: f32x4 = @zero;
    let one: f32x4 = @splat(f32x4, 1.0);
    for k in 0..xs.len / 4 {
        acc = acc + @load(f32x4, &xs.ptr[k * 4]);
        if helper(k) {
            acc = acc + one;
        }
    }
    return @reduce_add(acc);
}

fn helper(k: i64) -> bool {
    var junk: f32x4 = @splat(f32x4, 99.0); // clobbers vector registers
    junk = junk * junk;
    return k % 2 == 0 && junk[0] > 0.0;
}

fn slice_shapes(a: []i64, b: []i64) -> i64 {
    var s: i64 = 0;
    for i in 0..a.len {
        s += a[i] * b[i % b.len];
    }
    var c: []i64 = a;   // reassigned below: not cached
    c = c[1..c.len];
    s += c[0] + c.len;
    let d: []i64 = b;   // never reassigned: cached
    s += d[d.len - 1];
    return s;
}

test "slice caching and vectors kept across calls" {
    var buf: [16]f32 = @zero;
    for i in 0..16 {
        buf[i] = i as f32;
    }
    @assert(vsum_with_calls(buf[0..16]) == 128.0); // 120, plus 4 lanes * 1.0 for k = 0 and 2
    var a: [5]i64 = [5]i64.{ 1, 2, 3, 4, 5 };
    var b: [2]i64 = [2]i64.{ 10, 100 };
    // 1*10 + 2*100 + 3*10 + 4*100 + 5*10 = 690; c = [2..5]: 2 + 4; d[1] = 100
    @assert(slice_shapes(a[0..5], b[0..2]) == 796);
}

test "register-offset loads and stores for every element size" {
    var h: [8]u16 = @zero;
    var w: [8]i32 = @zero;
    var d: [8]f64 = @zero;
    var bb: [8]bool = @zero;
    for i in 0..8 {
        h[i] = (60000 + i) as u16;
        w[i] = (0 - i) as i32;
        d[i] = i as f64 * 0.5;
        bb[i] = i % 3 == 0;
        w[i] -= 1 as i32;
        d[i] += 1.0;
    }
    let p: *i32 = &w[0];
    var j: i64 = 7;
    p[j] *= 2 as i32;
    @assert(h[j] == 60007 as u16);
    @assert(w[7] == -16 as i32 && w[3] == -4 as i32);
    @assert(d[j] == 4.5);
    @assert(bb[6] && !bb[j]);
    let hs: []u16 = h[0..8];
    var s: i64 = 0;
    for i in 0..hs.len {
        s += hs[i] as i64;
    }
    @assert(s == 480028);
}

enum StepErr: u8 {
    bad = 1,
}

var g_log: i64 = 0;

fn may_fail(fail: bool) -> i64 ! StepErr {
    if fail {
        return @err(StepErr.bad);
    }
    return 5;
}

fn guarded(fail_early: bool, fail_late: bool) -> i64 ! StepErr {
    g_log = 0;
    defer { g_log = g_log * 10 + 1; }        // always
    errdefer { g_log = g_log * 10 + 2; }     // only on error exits
    let a: i64 = try may_fail(fail_early);
    errdefer { g_log = g_log * 10 + 3; }     // registered after the first try
    if fail_late {
        return @err(StepErr.bad);
    }
    return a;
}

test "errdefer runs only on error exits, in reverse order with defer" {
    let ok: i64 = guarded(false, false) catch e => -1;
    @assert(ok == 5 && g_log == 1);
    let early: i64 = guarded(true, false) catch e => -1;
    @assert(early == -1 && g_log == 21);     // errdefer 2, then defer 1
    let late: i64 = guarded(false, true) catch e => -1;
    @assert(late == -1 && g_log == 321);     // 3, 2, then 1
}

enum AggErr: u8 {
    bad = 1,
}
struct AggP {
    x: f64,
    y: f64,
    tag: i64,
}

fn agg_mk(fail: bool, k: f64) -> AggP ! AggErr {
    if fail {
        return @err(AggErr.bad);
    }
    return AggP.{ x: k, y: k * 2.0, tag: 7 };
}

fn agg_chain(fail: bool) -> AggP ! AggErr {
    let p: AggP = try agg_mk(fail, 1.5);
    return AggP.{ x: p.y, y: p.x, tag: p.tag + 1 };
}

fn agg_sl(xs: []i64, n: i64) -> []i64 ! AggErr {
    if n > xs.len {
        return @err(AggErr.bad);
    }
    return xs[0..n];
}

fn agg_vv(fail: bool) -> f32x4 ! AggErr {
    if fail {
        return @err(AggErr.bad);
    }
    return @splat(f32x4, 2.0);
}

test "aggregate and vector results from fallible functions" {
    let a: AggP = agg_chain(false) catch e => AggP.{ x: 0.0, y: 0.0, tag: -1 };
    @assert(a.x == 3.0 && a.y == 1.5 && a.tag == 8);
    let b: AggP = agg_chain(true) catch e => AggP.{ x: 0.0, y: 0.0, tag: -1 };
    @assert(b.tag == -1);
    var arr: [5]i64 = [5]i64.{ 1, 2, 3, 4, 5 };
    let s: []i64 = agg_sl(arr[0..5], 3) catch e => arr[0..0];
    @assert(s.len == 3 && s[2] == 3);
    let t: []i64 = agg_sl(arr[0..5], 9) catch e => arr[0..1];
    @assert(t.len == 1);
    let v: f32x4 = agg_vv(false) catch e => @splat(f32x4, 0.0);
    @assert(@reduce_add(v) == 8.0);
    let w: f32x4 = agg_vv(true) catch e => @splat(f32x4, -1.0);
    @assert(@reduce_add(w) == -4.0);
    for i in 0..3 {
        let q: AggP = agg_mk(i == 1, i as f64) catch e => {
            continue;
        };
        @assert(q.x == i as f64);
    }
}

enum JtOp: u8 {
    push = 1,
    add,
    sub,
    mul,
    dup,
    swap,
    jmpz,
    halt,
}
fn jt_step(op: JtOp, x: i64) -> i64 {
    switch op {
        JtOp.push => { return x + 1; }
        JtOp.add => { return x + 2; }
        JtOp.sub => { return x - 3; }
        JtOp.mul => { return x * 4; }
        JtOp.dup, JtOp.swap => { return x * 100; }
        else => { return -1; }
    }
    return 0;
}
fn jt_neg(k: i64) -> i64 {
    switch k {
        -2 => { return 20; }
        -1 => { return 10; }
        0 => { return 0; }
        1 => { return 11; }
        3 => { return 33; }
        else => { return 99; }
    }
    return 99;
}
test "dense switches use a jump table" {
    @assert(jt_step(JtOp.push, 5) == 6);
    @assert(jt_step(JtOp.mul, 5) == 20);
    @assert(jt_step(JtOp.swap, 5) == 500);
    @assert(jt_step(JtOp.halt, 5) == -1);
    @assert(jt_step(JtOp.jmpz, 5) == -1);
    @assert(jt_neg(-2) == 20 && jt_neg(-1) == 10 && jt_neg(0) == 0 && jt_neg(1) == 11 && jt_neg(3) == 33);
    @assert(jt_neg(2) == 99 && jt_neg(-3) == 99 && jt_neg(4) == 99 && jt_neg(1000000) == 99);
}

// fallible function pointers: '! E' is part of the fn type
fn r_halve(v: f64) -> f64 ! MathErr {
    if v < 0.0 {
        return @err(MathErr.negative);
    }
    return v * 0.5;
}

fn r_apply(f: fn(f64) -> f64 ! MathErr, v: f64) -> f64 ! MathErr {
    let a: f64 = try f(v);
    return try f(a);
}

test "fallible function pointers: try and catch through fn(..) -> T ! E" {
    var f: fn(f64) -> f64 ! MathErr = r_sqrtish;
    @assert((f(8.0) catch _ => -1.0) == 4.0);
    @assert((f(-1.0) catch e => (e as i64) as f64) == 1.0);
    f = r_halve;
    @assert((r_apply(f, 8.0) catch _ => -1.0) == 2.0);
    @assert((r_apply(r_sqrtish, 4000.0) catch e => (e as i64) as f64) == 300.0);
}

// call-free statements in a function that calls borrow caller-saved x2..x7;
// values must still survive the calls in neighbouring statements
fn nl_helper(x: i64) -> i64 {
    return x + 1;
}

fn nl_mix(xs: []i64) -> i64 {
    var acc: i64 = 0;
    for i in 0..xs.len {
        let t: i64 = (xs[i] * 3 + (acc ^ (i << 2))) - xs[i] / 2;
        acc = nl_helper(t) + acc;
    }
    return acc;
}

test "caller-saved temporaries in call-free statements of non-leaf functions" {
    let xs: [10]i64 = [10]i64.{ 5, -7, 12, 3, 100, -41, 8, 0, 9, 77 };
    @assert(nl_mix(xs[0..10]) == 13962);
}

// for-loop counters bounded by s.len (s never reassigned) or by a constant
// no larger than an array's length skip the bounds check; anything else keeps it
fn bc_sum(s: []i64) -> i64 {
    var acc: i64 = 0;
    for i in 0..s.len {
        acc += s[i] * (i + 1);
    }
    for j in 1..s.len {
        acc += s[j - 1]; // not a bare counter: still checked
    }
    return acc;
}

test "proven for-loop indices drop bounds checks without changing results" {
    var a: [6]i64 = @zero;
    for i in 0..6 {
        a[i] = i * i;
    }
    var u: u64 = 0;
    for k in 2 as u64..5 as u64 {
        u += k;
    }
    @assert(u == 9 as u64);
    @assert(bc_sum(a[0..6]) == 280 + 30);
}

// for x in s: element loops over arrays and slices (desugared to a counted loop)
struct FeP { x: i64, y: i64 }

fn fe_total(s: []i64) -> i64 {
    var acc: i64 = 0;
    for v in s {
        acc += v;
    }
    return acc;
}

fn fe_mk() -> [3]FeP {
    return [3]FeP.{ FeP.{ x: 1, y: 2 }, FeP.{ x: 3, y: 4 }, FeP.{ x: 5, y: 6 } };
}

test "for x in s iterates arrays and slices" {
    let a: [5]i64 = [5]i64.{ 1, 2, 3, 4, 5 };
    @assert(fe_total(a[0..5]) == 15);
    @assert(fe_total(a[1..3]) == 5);
    var n: i64 = 0;
    for v in a {
        if v == 4 {
            break;
        }
        n += v;
    }
    @assert(n == 6);
    var sx: i64 = 0;
    for p in fe_mk() {
        sx += p.x * p.y;
    }
    @assert(sx == 44);
    for v in a[2..5] {
        for w in a[0..2] {
            n += v * w;
        }
    }
    @assert(n == 6 + 36);
}

test "for i, x in s binds the index and the element" {
    let a: [4]i64 = [4]i64.{ 10, 20, 30, 40 };
    var acc: i64 = 0;
    for i, x in a {
        acc += i * x;
    }
    @assert(acc == 200);
    for i, x in a[1..4] {
        if i == 1 {
            @assert(x == 30);
        }
    }
}

fn st_dot(a: []f32, b: []f32) -> f32 {
    var acc: f32x4 = @zero;
    for i in 0..a.len step 4 {
        acc = @fma(@load(f32x4, &a[i]), @load(f32x4, &b[i]), acc);
    }
    return @reduce_add(acc);
}

test "for ... step K advances the counter by K" {
    var n: i64 = 0;
    var last: i64 = 0;
    for i in 3..20 step 5 {
        n += 1;
        last = i;
    }
    @assert(n == 4 && last == 18);
    for i in 0..0 step 7 {
        n += 100;
    }
    @assert(n == 4);
    var xs: [8]f32 = [8]f32.{ 1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0 };
    @assert(st_dot(xs[0..8], xs[0..8]) == 204.0);
}

test "bit builtins: @clz, @ctz, @popcount, @abs" {
    @assert(@clz(1) == 63);
    @assert(@clz(0) == 64);
    @assert(@ctz(8) == 3);
    @assert(@ctz(0) == 64);
    @assert(@popcount(255) == 8);
    @assert(@popcount(-1) == 64);
    let b: u8 = 16 as u8;
    @assert(@clz(b) == 3 as u8);
    @assert(@ctz(0 as u8) == 8 as u8);
    let n: i8 = -1 as i8;
    @assert(@popcount(n) == 8 as i8);
    @assert(@clz(n) == 0 as i8);
    let m: i32 = -2 as i32;
    @assert(@popcount(m) == 31 as i32);
    @assert(@ctz(m) == 1 as i32);
    @assert(@clz(1 as u16) == 15 as u16);
    var k: i64 = -8;
    var t: i64 = 0;
    for i in 0..3 {
        t += @ctz(k) + @abs(k) + @popcount(k);
    }
    @assert(k == -8 && t == 3 * (3 + 8 + 61)); // the argument is not modified
    @assert(@abs(-7) == 7);
    @assert(@abs(5 as i32) == 5 as i32);
    @assert(@abs(-2.5) == 2.5);
    let v: i32x4 = @splat(i32x4, -3 as i32);
    @assert(@reduce_add(@abs(v)) == 12 as i32);
    let fv: f32x4 = @splat(f32x4, -1.5 as f32);
    @assert(@reduce_add(@abs(fv)) == 6.0 as f32);
}

test "@bswap reverses byte order" {
    @assert(@bswap(0x0102030405060708) == 0x0807060504030201);
    @assert(@bswap(0x11223344 as u32) == 0x44332211 as u32);
    @assert(@bswap(0x1234 as u16) == 0x3412 as u16);
    let s: i16 = 0x00ff as i16;
    @assert(@bswap(s) == -256 as i16); // 0xff00, sign-extended
}
