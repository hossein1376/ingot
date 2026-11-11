// Tagged unions: a u32 tag plus the largest payload; switch binds payloads.
struct Rect {
    w: f64,
    h: f64,
}

union Shape {
    circle: f64,
    rect: Rect,
    tri: [3]f64,
    point,
}

fn area(s: Shape) -> f64 {
    switch s {
        Shape.circle(r) => { return 3.0 * r * r; }
        Shape.rect(q) => { return q.w * q.h; }
        Shape.tri(t) => { return t[0] + t[1] + t[2]; }
        Shape.point => { return 0.0; }
    }
    return -1.0;
}

// generic union
union Option[T] {
    some: T,
    none,
}

fn find[T](xs: []T, want: T) -> Option.[i64] {
    for i in 0..xs.len {
        if xs[i] == want {
            return Option.[i64].some(i);
        }
    }
    return Option.[i64].none();
}

union Tok {
    num: i64,
    op: u8,
    vec: f32x4,
    end,
}

struct Lexed {
    line: i64,
    tok: Tok, // a union inside a struct
}

enum EvalErr: u8 {
    bad = 1,
}

// a tiny RPN evaluator over unions
fn eval(toks: []Tok) -> i64 ! EvalErr {
    var stack: [16]i64 = @zero;
    var sp: i64 = 0;
    for i in 0..toks.len {
        switch toks[i] {
            Tok.num(n) => {
                stack[sp] = n;
                sp += 1;
            }
            Tok.op(o) => {
                if sp < 2 {
                    return @err(EvalErr.bad);
                }
                let b: i64 = stack[sp - 1];
                let a: i64 = stack[sp - 2];
                sp -= 1;
                stack[sp - 1] = @select(o == 43 as u8, a + b, a * b); // '+' or '*'
            }
            Tok.end => {
                break;
            }
            else => {
                return @err(EvalErr.bad);
            }
        }
    }
    return stack[0];
}

test "union construction, switch and payload binding" {
    var shapes: [4]Shape = @zero;
    shapes[0] = Shape.circle(2.0);
    shapes[1] = Shape.rect(Rect.{ w: 3.0, h: 4.0 });
    shapes[2] = Shape.tri([3]f64.{ 1.0, 2.0, 3.5 });
    shapes[3] = Shape.point();
    var total: f64 = 0.0;
    for i in 0..4 {
        total += area(shapes[i]);
    }
    @assert(total == 30.5);
    @assert(@sizeof(Shape) == 32);
    let z: Shape = @zero; // tag 0: the first variant, zero payload
    @assert(area(z) == 0.0);
}

test "generic unions, unions in structs, unions through error unions" {
    var xs: [4]u16 = [4]u16.{ 5 as u16, 9 as u16, 2 as u16, 9 as u16 };
    var hit: i64 = -1;
    switch find.[u16](xs[0..4], 2 as u16) {
        Option.[i64].some(i) => { hit = i; }
        Option.[i64].none => { hit = -2; }
    }
    @assert(hit == 2);
    switch find.[u16](xs[0..4], 7 as u16) {
        Option.[i64].some(i) => { hit = i; }
        Option.[i64].none => { hit = -2; }
    }
    @assert(hit == -2);
    let l: Lexed = Lexed.{ line: 3, tok: Tok.vec(@splat(f32x4, 1.5)) };
    var lanes: f32 = 0.0;
    switch l.tok {
        Tok.vec(v) => { lanes = @reduce_add(v); }
        else => { }
    }
    @assert(lanes == 6.0);
    // (2 + 3) * 4 = 20
    var prog: [6]Tok = @zero;
    prog[0] = Tok.num(2);
    prog[1] = Tok.num(3);
    prog[2] = Tok.op(43 as u8);
    prog[3] = Tok.num(4);
    prog[4] = Tok.op(42 as u8);
    prog[5] = Tok.end();
    let r: i64 = eval(prog[0..6]) catch e => -1;
    @assert(r == 20);
    prog[1] = Tok.op(43 as u8);
    let bad: i64 = eval(prog[0..6]) catch e => -1;
    @assert(bad == -1);
}

// U.v(&p) binds a pointer to the payload, so the case can update it in place
fn grow(s: *Shape, k: f64) {
    switch s.* {
        Shape.circle(&r) => { r.* = r.* * k; }
        Shape.rect(&q) => {
            q.*.w = q.*.w * k;
            q.*.h = q.*.h * k;
        }
        Shape.tri(&t) => { t.*[0] = t.*[0] * k; }
        Shape.point => {}
    }
}

test "pointer payload bindings update the union in place" {
    var c: Shape = Shape.circle(2.0);
    var r: Shape = Shape.rect(Rect.{ w: 1.0, h: 3.0 });
    var t: Shape = Shape.tri([3]f64.{ 1.0, 2.0, 3.0 });
    grow(&c, 2.0);
    grow(&r, 2.0);
    grow(&t, 10.0);
    @assert(area(c) == 48.0);
    @assert(area(r) == 12.0);
    @assert(area(t) == 15.0);
    switch r {
        Shape.rect(&q) => { q.*.h = 0.5; }
        Shape.circle, Shape.tri, Shape.point => {}
    }
    @assert(area(r) == 1.0);
}
