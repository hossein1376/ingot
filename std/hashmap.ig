// std/hashmap.ig: a generic open-addressing hash map HashMap[K, V].
// K is an integer type (keys are hashed as `k as u64` with splitmix64);
// V is any type. Linear probing over a power-of-two table, rehashed at 70%
// load. Memory comes from malloc/free only when the table is created or grows.
//   var m: HashMap.[u32, f64] = @zero;
//   try hm_init.[u32, f64](&m, 1024);
//   try hm_put.[u32, f64](&m, 7 as u32, 1.5);
//   let p: *f64 = hm_ptr.[u32, f64](&m, 7 as u32); // null if missing; update in place
import "std/libc.ig";

enum HashMapErr: u8 {
    missing = 1,
    out_of_memory = 2,
}

struct HashMap[K, V] {
    keys: *K,
    vals: *V,
    used: *u8, // 1 = slot occupied
    cap: i64,  // power of two
    len: i64,
}

inline fn hm_hash(k: u64) -> u64 {
    var z: u64 = k + 0x9E3779B97F4A7C15;
    z = (z ^ (z >> 30)) * 0xBF58476D1CE4E5B9;
    z = (z ^ (z >> 27)) * 0x94D049BB133111EB;
    return z ^ (z >> 31);
}

fn hm_alloc[K, V](m: *HashMap.[K, V], cap: i64) -> void ! HashMapErr {
    let keys: *u8 = malloc((cap * @sizeof(K)) as u64);
    let vals: *u8 = malloc((cap * @sizeof(V)) as u64);
    let used: *u8 = calloc(cap as u64, 1);
    if keys == null || vals == null || used == null {
        free(keys);
        free(vals);
        free(used);
        return @err(HashMapErr.out_of_memory);
    }
    m.* = HashMap.[K, V].{ keys: keys as *K, vals: vals as *V, used: used, cap: cap, len: 0 };
}

// cap is rounded up to a power of two (at least 16).
fn hm_init[K, V](m: *HashMap.[K, V], cap: i64) -> void ! HashMapErr {
    var c: i64 = 16;
    while c < cap {
        c *= 2;
    }
    try hm_alloc.[K, V](m, c);
}

fn hm_free[K, V](m: *HashMap.[K, V]) {
    free(m.*.keys as *u8);
    free(m.*.vals as *u8);
    free(m.*.used);
    m.* = @zero;
}

// The slot holding k, or the empty slot where k would go.
inline fn hm_slot[K, V](m: *HashMap.[K, V], k: K) -> i64 {
    let mask: i64 = m.*.cap - 1;
    var i: i64 = (hm_hash(k as u64) as i64) & mask;
    while m.*.used[i] != 0 as u8 && m.*.keys[i] != k {
        i = (i + 1) & mask;
    }
    return i;
}

fn hm_grow[K, V](m: *HashMap.[K, V]) -> void ! HashMapErr {
    var old: HashMap.[K, V] = m.*;
    try hm_alloc.[K, V](m, old.cap * 2);
    for i in 0..old.cap {
        if old.used[i] != 0 as u8 {
            let j: i64 = hm_slot.[K, V](m, old.keys[i]);
            m.*.keys[j] = old.keys[i];
            m.*.vals[j] = old.vals[i];
            m.*.used[j] = 1 as u8;
        }
    }
    m.*.len = old.len;
    hm_free.[K, V](&old);
}

// Inserts or overwrites. The map must have been set up with hm_init.
fn hm_put[K, V](m: *HashMap.[K, V], k: K, v: V) -> void ! HashMapErr {
    if (m.*.len + 1) * 10 > m.*.cap * 7 {
        try hm_grow.[K, V](m);
    }
    let i: i64 = hm_slot.[K, V](m, k);
    if m.*.used[i] == 0 as u8 {
        m.*.used[i] = 1 as u8;
        m.*.keys[i] = k;
        m.*.len += 1;
    }
    m.*.vals[i] = v;
}

fn hm_get[K, V](m: *HashMap.[K, V], k: K) -> V ! HashMapErr {
    let i: i64 = hm_slot.[K, V](m, k);
    if m.*.used[i] == 0 as u8 {
        return @err(HashMapErr.missing);
    }
    return m.*.vals[i];
}

// A pointer to k's value, or null when k is absent. Valid until the next put.
fn hm_ptr[K, V](m: *HashMap.[K, V], k: K) -> *V {
    let i: i64 = hm_slot.[K, V](m, k);
    if m.*.used[i] == 0 as u8 {
        return null;
    }
    return &m.*.vals[i];
}

struct HmTestStat {
    count: i64,
    sum: f64,
}

fn hm_test_fill(m: *HashMap.[u32, HmTestStat]) -> void ! HashMapErr {
    try hm_init.[u32, HmTestStat](m, 4);
    for i in 0..5000 {
        let k: u32 = (i % 97) as u32;
        let p: *HmTestStat = hm_ptr.[u32, HmTestStat](m, k);
        if p == null {
            try hm_put.[u32, HmTestStat](m, k, HmTestStat.{ count: 1, sum: i as f64 });
        } else {
            p.*.count += 1;
            p.*.sum += i as f64;
        }
    }
}

test "HashMap: struct values updated in place, growth, missing keys" {
    var m: HashMap.[u32, HmTestStat] = @zero;
    defer { hm_free.[u32, HmTestStat](&m); }
    hm_test_fill(&m) catch _ => {
        @panic("out of memory");
    };
    @assert(m.len == 97);
    var total: i64 = 0;
    for k in 0..97 {
        let s: HmTestStat = hm_get.[u32, HmTestStat](&m, k as u32) catch _ => HmTestStat.{ count: 0, sum: 0.0 };
        total += s.count;
    }
    @assert(total == 5000);
    let miss: HmTestStat = hm_get.[u32, HmTestStat](&m, 1000 as u32) catch _ => HmTestStat.{ count: -1, sum: 0.0 };
    @assert(miss.count == -1);
    @assert(hm_ptr.[u32, HmTestStat](&m, 98 as u32) == null);
}

test "HashMap with i64 keys and f64 values" {
    var m: HashMap.[i64, f64] = @zero;
    defer { hm_free.[i64, f64](&m); }
    hm_init.[i64, f64](&m, 16) catch _ => {
        @panic("out of memory");
    };
    for i in 0..20000 {
        hm_put.[i64, f64](&m, i * -3, i as f64 * 0.5) catch _ => {
            @panic("out of memory");
        };
    }
    @assert(m.len == 20000 && m.cap == 32768);
    let v: f64 = hm_get.[i64, f64](&m, -300) catch _ => -1.0;
    @assert(v == 50.0);
}
