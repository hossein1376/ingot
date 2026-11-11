// Generics: explicit type arguments, one monomorphized copy per instance.
import "mod/generic_lib.ig" as gl;

struct Pair[A, B] {
    a: A,
    b: B,
}

fn swap[A, B](p: Pair.[A, B]) -> Pair.[B, A] {
    return Pair.[B, A].{ a: p.b, b: p.a };
}

fn sum[T](xs: []T) -> T {
    var s: T = @zero;
    for i in 0..xs.len {
        s += xs[i];
    }
    return s;
}

fn max_of[T](xs: []T) -> T {
    var m: T = xs[0];
    for i in 1..xs.len {
        m = @select(xs[i] > m, xs[i], m);
    }
    return m;
}

// generic calling generic, and a generic inline helper
inline fn sq[T](x: T) -> T {
    return x * x;
}

fn sum_sq[T](xs: []T) -> T {
    var s: T = @zero;
    for i in 0..xs.len {
        s += sq.[T](xs[i]);
    }
    return s;
}

fn apply[T](xs: []T, f: fn(T) -> T) -> T {
    var s: T = @zero;
    for i in 0..xs.len {
        s += f(xs[i]);
    }
    return s;
}

fn dbl(x: i64) -> i64 {
    return 2 * x;
}

test "generic functions over ints, floats and vectors" {
    var a: [4]f64 = [4]f64.{ 1.5, 2.5, 3.0, -1.0 };
    var b: [3]i32 = [3]i32.{ 7 as i32, -2 as i32, 9 as i32 };
    var u: [3]u8 = [3]u8.{ 200 as u8, 100 as u8, 1 as u8 };
    @assert(sum.[f64](a[0..4]) == 6.0);
    @assert(sum.[i32](b[0..3]) == 14 as i32);
    @assert(sum.[u8](u[0..3]) == 45 as u8); // wraps at 256
    @assert(max_of.[f64](a[0..4]) == 3.0);
    @assert(max_of.[u8](u[0..3]) == 200 as u8);
    @assert(sum_sq.[i32](b[0..3]) == 134 as i32);
    var vs: [2]f32x4 = [2]f32x4.{ @splat(f32x4, 1.0), @splat(f32x4, 2.5) };
    let vt: f32x4 = sum.[f32x4](vs[0..2]);
    @assert(@reduce_add(vt) == 14.0);
    var c: [3]i64 = [3]i64.{ 1, 2, 3 };
    @assert(apply.[i64](c[0..3], dbl) == 12);
}

test "generic structs, nested instances and modules" {
    let p: Pair.[i64, f64] = Pair.[i64, f64].{ a: 3, b: 0.25 };
    let q: Pair.[f64, i64] = swap.[i64, f64](p);
    @assert(q.a == 0.25 && q.b == 3);
    let pp: Pair.[Pair.[i64, f64], bool] = Pair.[Pair.[i64, f64], bool].{ a: p, b: true };
    @assert(pp.a.b == 0.25 && pp.b);
    @assert(@sizeof(Pair.[u8, i64]) == 16);
    var st: gl.Stack.[f64] = @zero;
    gl.push.[f64](&st, 1.25);
    gl.push.[f64](&st, 2.5);
    @assert(gl.pop.[f64](&st) == 2.5);
    @assert(st.len == 1);
    var si: gl.Stack.[*u8] = @zero;
    var byte: u8 = 42 as u8;
    gl.push.[*u8](&si, &byte);
    @assert(gl.pop.[*u8](&si)[0] == 42 as u8);
}
