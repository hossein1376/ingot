import "../e2e/mod/vec2.ig" as v;

fn f() -> f64 {
    let a: Vec = Vec.{ x: 1.0, y: 2.0 };
    return v.norm2(a);
}
