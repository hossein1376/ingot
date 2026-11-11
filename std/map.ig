// std/map.ig: an open-addressing hash map from u64 to u64.
// Linear probing over a power-of-two table, rehashed at 70% load. Memory
// comes from malloc/free only when the table is created or grows.
import "std/libc.ig";

enum MapErr: u8 {
    missing = 1,
    out_of_memory = 2,
}

struct U64Map {
    keys: *u64,
    vals: *u64,
    used: *u8, // 1 = slot occupied
    cap: i64,  // power of two
    len: i64,
}

// splitmix64 finalizer: a cheap, well-mixed hash
inline fn map_hash(k: u64) -> u64 {
    var z: u64 = k + 0x9E3779B97F4A7C15;
    z = (z ^ (z >> 30)) * 0xBF58476D1CE4E5B9;
    z = (z ^ (z >> 27)) * 0x94D049BB133111EB;
    return z ^ (z >> 31);
}

fn map_alloc(m: *U64Map, cap: i64) -> void ! MapErr {
    let keys: *u64 = malloc((cap * 8) as u64) as *u64;
    let vals: *u64 = malloc((cap * 8) as u64) as *u64;
    let used: *u8 = calloc(cap as u64, 1);
    if keys == null || vals == null || used == null {
        free(keys as *u8);
        free(vals as *u8);
        free(used);
        return @err(MapErr.out_of_memory);
    }
    m.* = U64Map.{ keys: keys, vals: vals, used: used, cap: cap, len: 0 };
}

// cap is rounded up to a power of two (at least 16).
fn map_init(m: *U64Map, cap: i64) -> void ! MapErr {
    var c: i64 = 16;
    while c < cap {
        c = c * 2;
    }
    try map_alloc(m, c);
}

fn map_free(m: *U64Map) {
    free(m.*.keys as *u8);
    free(m.*.vals as *u8);
    free(m.*.used);
    m.* = @zero;
}

// slot returns the index holding k, or the empty slot where k would go.
inline fn map_slot(m: *U64Map, k: u64) -> i64 {
    let mask: i64 = m.*.cap - 1;
    var i: i64 = (map_hash(k) as i64) & mask;
    while m.*.used[i] != 0 as u8 && m.*.keys[i] != k {
        i = (i + 1) & mask;
    }
    return i;
}

fn map_grow(m: *U64Map) -> void ! MapErr {
    var old: U64Map = m.*;
    try map_alloc(m, old.cap * 2);
    for i in 0..old.cap {
        if old.used[i] != 0 as u8 {
            let j: i64 = map_slot(m, old.keys[i]);
            m.*.keys[j] = old.keys[i];
            m.*.vals[j] = old.vals[i];
            m.*.used[j] = 1 as u8;
        }
    }
    m.*.len = old.len;
    map_free(&old);
}

fn map_put(m: *U64Map, k: u64, v: u64) -> void ! MapErr {
    if (m.*.len + 1) * 10 > m.*.cap * 7 {
        try map_grow(m);
    }
    let i: i64 = map_slot(m, k);
    if m.*.used[i] == 0 as u8 {
        m.*.used[i] = 1 as u8;
        m.*.keys[i] = k;
        m.*.len += 1;
    }
    m.*.vals[i] = v;
}

fn map_get(m: *U64Map, k: u64) -> u64 ! MapErr {
    let i: i64 = map_slot(m, k);
    if m.*.used[i] == 0 as u8 {
        return @err(MapErr.missing);
    }
    return m.*.vals[i];
}

test "map: put, overwrite, get, missing keys, growth" {
    var m: U64Map = @zero;
    map_init(&m, 4) catch _ => {
        @panic("out of memory");
    };
    defer { map_free(&m); }
    for i in 0..10000 {
        map_put(&m, (i * 7) as u64, (i * i) as u64) catch _ => {
            @panic("out of memory");
        };
    }
    map_put(&m, 0, 42) catch _ => {
        @panic("out of memory");
    };
    @assert(m.len == 10000);
    @assert(m.cap == 16384);
    @assert((map_get(&m, 0) catch _ => 1) == 42);
    @assert((map_get(&m, 7 * 9999) catch _ => 1) == 9999 * 9999);
    @assert((map_get(&m, 5) catch e => (e as u64) + 100) == 101);
}
