// Structs, pointers, arrays and slices. Layout is C-compatible.
extern fn printf(fmt: *u8, ...) -> i32;

struct Vec2 {
    x: f64,
    y: f64,
}

struct Particle {
    pos: Vec2,
    vel: Vec2,
    id: i32,
    alive: bool,
}

fn add(a: Vec2, b: Vec2) -> Vec2 {
    return Vec2.{ x: a.x + b.x, y: a.y + b.y };
}

// Explicit pointer: the caller sees the mutation.
fn step(p: *Particle, dt: f64) {
    p.*.pos.x = p.*.pos.x + p.*.vel.x * dt;
    p.*.pos.y = p.*.pos.y + p.*.vel.y * dt;
}

fn sum(xs: []i64) -> i64 {
    var total: i64 = 0;
    for i in 0..xs.len {
        total += xs[i];
    }
    return total;
}

pub fn main() -> i32 {
    var p: Particle = Particle.{
        pos: Vec2.{ x: 0.0, y: 0.0 },
        vel: Vec2.{ x: 1.5, y: -2.0 },
        id: 7,
        alive: true,
    };
    for i in 0..4 {
        step(&p, 0.5);
    }
    let c: Vec2 = add(p.pos, Vec2.{ x: 10.0, y: 10.0 });
    printf(c"particle %d at (%.2f, %.2f), shifted (%.2f, %.2f)\n", p.id, p.pos.x, p.pos.y, c.x, c.y);

    var arr: [8]i64 = [8]i64.{ 1, 2, 3, 4, 5, 6, 7, 8 };
    arr[0] = 100;
    let s: []i64 = arr[2..6];
    printf(c"sum(all)=%lld sum(2..6)=%lld len=%lld sizeof(Particle)=%lld\n", sum(arr[0..8]), sum(s), s.len, @sizeof(Particle));

    var grid: [16]u8 = @zero;
    let q: *u8 = &grid[3];
    q.* = 255 as u8;
    printf(c"grid[3]=%u wraps to %u\n", grid[3] as u32, (grid[3] + 1 as u8) as u32);
    return 0;
}

test "struct by value is a copy" {
    let a: Vec2 = Vec2.{ x: 1.0, y: 2.0 };
    var b: Vec2 = a;
    b.x = 5.0;
    @assert(a.x == 1.0);
    @assert(b.x == 5.0);
}

test "slices see the array" {
    var a: [4]i32 = [4]i32.{ 1, 2, 3, 4 };
    let s: []i32 = a[1..3];
    s[0] = 20;
    @assert(a[1] == 20);
    @assert(s.len == 2);
}
