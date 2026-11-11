// A module meant to be imported with `as`: its names collide with mod/vec3.ig.
struct Vec { x: f64, y: f64 }
enum Axis: u8 { x = 1, y = 2 }
const DIM: i64 = 2;
var calls: i64 = 0;

fn dot(a: Vec, b: Vec) -> f64 {
    calls += 1;
    return a.x * b.x + a.y * b.y;
}

fn norm2(a: Vec) -> f64 {
    return dot(a, a); // unqualified: this module's own dot
}

fn pick(v: Vec, ax: Axis) -> f64 {
    switch ax {
        Axis.x => { return v.x; }
        Axis.y => { return v.y; }
    }
    return 0.0;
}

// A local that shadows a top-level name still refers to the local.
fn shadow(DIM: i64) -> i64 {
    return DIM * 10;
}
