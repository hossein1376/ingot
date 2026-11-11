// std/vec.ig: a growable array, generic over its element type.
//   var v: Vec.[f64] = @zero;
//   try vec_push.[f64](&v, 1.5);
//   let xs: []f64 = vec_items.[f64](&v);
//   vec_free.[f64](&v);
// Capacity doubles (from 8) through realloc. Elements are copied in by value.
import "std/libc.ig";

enum VecErr: u8 {
    out_of_memory = 1,
    empty = 2,
}

struct Vec[T] {
    ptr: *T,
    len: i64,
    cap: i64,
}

// Makes room for at least n elements in total.
fn vec_reserve[T](v: *Vec.[T], n: i64) -> void ! VecErr {
    if n <= v.*.cap {
        return;
    }
    var c: i64 = @select(v.*.cap == 0, 8, v.*.cap);
    while c < n {
        c *= 2;
    }
    let p: *u8 = realloc(v.*.ptr as *u8, (c * @sizeof(T)) as u64);
    if p == null {
        return @err(VecErr.out_of_memory);
    }
    v.*.ptr = p as *T;
    v.*.cap = c;
}

fn vec_push[T](v: *Vec.[T], x: T) -> void ! VecErr {
    if v.*.len == v.*.cap {
        try vec_reserve.[T](v, v.*.len + 1);
    }
    v.*.ptr[v.*.len] = x;
    v.*.len += 1;
}

// Removes and returns the last element (scalar and vector element types).
fn vec_pop[T](v: *Vec.[T]) -> T ! VecErr {
    if v.*.len == 0 {
        return @err(VecErr.empty);
    }
    v.*.len -= 1;
    return v.*.ptr[v.*.len];
}

// A slice over the current elements; valid until the next push or free.
fn vec_items[T](v: *Vec.[T]) -> []T {
    return v.*.ptr[0..v.*.len];
}

fn vec_free[T](v: *Vec.[T]) {
    free(v.*.ptr as *u8);
    v.* = @zero;
}

struct VecTestPoint {
    x: f64,
    y: f64,
}

fn vec_test_fill(v: *Vec.[i64], n: i64) -> i64 ! VecErr {
    for i in 0..n {
        try vec_push.[i64](v, i * i);
    }
    var s: i64 = 0;
    let xs: []i64 = vec_items.[i64](v);
    for i in 0..xs.len {
        s += xs[i];
    }
    return s;
}

test "Vec grows, pops and reports empty" {
    var v: Vec.[i64] = @zero;
    defer { vec_free.[i64](&v); }
    let s: i64 = vec_test_fill(&v, 1000) catch e => -1;
    @assert(s == 332833500);
    @assert(v.len == 1000 && v.cap == 1024);
    let last: i64 = vec_pop.[i64](&v) catch e => -1;
    @assert(last == 998001);
    var w: Vec.[f32] = @zero;
    let none: f32 = vec_pop.[f32](&w) catch e => {
        @assert(e == VecErr.empty);
        return;
    };
    @assert(false);
}

test "Vec of structs" {
    var v: Vec.[VecTestPoint] = @zero;
    defer { vec_free.[VecTestPoint](&v); }
    for i in 0..20 {
        vec_push.[VecTestPoint](&v, VecTestPoint.{ x: i as f64, y: 1.0 }) catch e => {
            @assert(false);
        };
    }
    let ps: []VecTestPoint = vec_items.[VecTestPoint](&v);
    var sx: f64 = 0.0;
    for i in 0..ps.len {
        sx += ps[i].x * ps[i].y;
    }
    @assert(sx == 190.0);
}
