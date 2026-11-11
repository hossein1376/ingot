import "vec2.ig" as flat; // a module can import modules too
struct Vec { x: f64, y: f64, z: f64 }
const DIM: i64 = 3;

fn dot(a: Vec, b: Vec) -> f64 {
    return a.x * b.x + a.y * b.y + a.z * b.z;
}

fn drop_z(v: Vec) -> flat.Vec {
    return flat.Vec.{ x: v.x, y: v.y };
}
