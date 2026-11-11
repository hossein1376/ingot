// Namespaced modules: `import "p" as m;` keeps p's names behind `m.`.
import "mod/vec2.ig" as v2;
import "mod/vec3.ig" as v3;
import "mod/vec2.ig" as again; // same file, same module instance
import "std/map.ig" as hm;
import "mod/parse_lib.ig" as p;

fn dot(a: i64, b: i64) -> i64 { // no clash with v2.dot / v3.dot
    return a * b;
}

test "modules keep colliding names apart" {
    let a: v2.Vec = v2.Vec.{ x: 3.0, y: 4.0 };
    let b: v3.Vec = v3.Vec.{ x: 1.0, y: 2.0, z: 2.0 };
    @assert(v2.norm2(a) == 25.0);
    @assert(v3.dot(b, b) == 9.0);
    @assert(dot(6, 7) == 42);
    @assert(v2.DIM == 2 && v3.DIM == 3);
    @assert(v2.pick(a, v2.Axis.y) == 4.0);
    @assert(v2.shadow(5) == 50);
    let f: again.Vec = v3.drop_z(b); // v3's `flat` is the same module as v2
    @assert(again.dot(f, a) == 11.0);
    @assert(v2.calls == 2);
}

fn squares(m: *hm.U64Map) -> u64 ! hm.MapErr {
    try hm.map_init(m, 8);
    for i in 0..100 {
        try hm.map_put(m, i as u64, (i * i) as u64);
    }
    return try hm.map_get(m, 9 as u64);
}

test "std module under an alias" {
    var m: hm.U64Map = @zero;
    defer { hm.map_free(&m); }
    let x: u64 = squares(&m) catch e => 0 as u64;
    @assert(x == 81 as u64);
    let missing: u64 = hm.map_get(&m, 1000 as u64) catch e => 7 as u64;
    @assert(missing == 7 as u64);
    @assert(@sizeof(hm.U64Map) == 40);
}

// error enums cross module boundaries: try propagates p.ParseErr unchanged
fn two_digits(a: u8, b: u8) -> i64 ! p.ParseErr {
    let x: i64 = try p.digit(a);
    let y: i64 = try p.digit(b);
    return x * 10 + y;
}

fn first_digit(a: u8) -> i64 ! p.ParseErr {
    if a == 0 as u8 {
        return @err(p.ParseErr.empty);
    }
    return try p.digit(a);
}

test "error enums shared across modules" {
    @assert((two_digits(52 as u8, 50 as u8) catch _ => -1) == 42);
    @assert((two_digits(52 as u8, 65 as u8) catch e => -(e as i64)) == -2);
    let z: i64 = first_digit(0 as u8) catch e => (e as i64) * 100;
    @assert(z == 100);
    let f: fn(u8) -> i64 ! p.ParseErr = p.digit;
    @assert((f(55 as u8) catch _ => -1) == 7);
}
