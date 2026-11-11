// Hash join with std/map.ig: build a table of 1M keys, then probe it with
// 2M keys of which half match. Failures are error unions, cleanup is defer.
import "std/libc.ig";
import "std/map.ig";

const BUILD: i64 = 1000000;

pub fn main() -> i32 {
    var m: U64Map = @zero;
    map_init(&m, BUILD * 2) catch _ => {
        printf(c"out of memory\n");
        return 1;
    };
    defer { map_free(&m); }

    let t0: i64 = @ticks();
    for i in 0..BUILD {
        // build side: key = 2*i, payload = i
        map_put(&m, (i * 2) as u64, i as u64) catch _ => {
            return 1;
        };
    }
    let t1: i64 = @ticks();
    var hits: i64 = 0;
    var sum: u64 = 0;
    for i in 0..BUILD * 2 {
        // probe side: every even key matches
        let v: u64 = map_get(&m, i as u64) catch _ => {
            continue;
        };
        hits += 1;
        sum += v;
    }
    let t2: i64 = @ticks();
    let f: f64 = @tick_freq() as f64;
    printf(c"hits=%lld sum=%llu (expect %lld and %lld)\n", hits, sum, BUILD, BUILD * (BUILD - 1) / 2);
    printf(c"build %.1f ms, probe %.1f ms\n", ((t1 - t0) as f64) / f * 1000.0, ((t2 - t1) as f64) / f * 1000.0);
    if hits != BUILD || sum != (BUILD * (BUILD - 1) / 2) as u64 {
        return 1;
    }
    return 0;
}
