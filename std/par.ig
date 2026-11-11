// std/par.ig: data parallelism over pthreads with no hidden runtime.
//
// Apple recommends against splitting work evenly across threads on hybrid
// P/E-core chips. Hand out many small chunks dynamically instead (at least 3x
// the core count). parallel_for does that with one LSE atomic counter.
import "std/libc.ig";

extern fn pthread_create(tid: *u64, attr: *u8, start: fn(*u8) -> *u8, arg: *u8) -> i32;
extern fn pthread_join(tid: u64, ret: *u8) -> i32;
extern fn sysctlbyname(name: *u8, out: *u8, outlen: *u64, newp: *u8, newlen: u64) -> i32;

const QOS_USER_INTERACTIVE: i32 = 33; // 0x21: favors performance cores
const QOS_USER_INITIATED: i32 = 25;   // 0x19
const QOS_UTILITY: i32 = 17;          // 0x11
const QOS_BACKGROUND: i32 = 9;        // 0x09: favors efficiency cores
const PAR_MAX_THREADS: i64 = 64;

// Online logical CPUs (_SC_NPROCESSORS_ONLN is 58 on macOS, 84 on Linux).
fn cpu_count() -> i64 {
    if @is_macos() {
        return sysconf(58);
    }
    return sysconf(84);
}

// Performance-core count on Apple silicon (hw.perflevel0.physicalcpu);
// falls back to cpu_count() elsewhere.
fn perf_core_count() -> i64 {
    if @is_macos() {
        var n: i32 = 0;
        var len: u64 = 4 as u64;
        if sysctlbyname(c"hw.perflevel0.physicalcpu", &n as *u8, &len, null, 0 as u64) == 0 {
            return n as i64;
        }
    }
    return cpu_count();
}

struct ParJob {
    body: fn(*u8, i64, i64),
    ctx: *u8,
    n: i64,
    chunk: i64,
    next: i64,
    qos: i32,
}

fn par_worker(arg: *u8) -> *u8 {
    let job: *ParJob = arg as *ParJob;
    @qos(job.*.qos);
    while true {
        let lo: i64 = @atomic_add(&job.*.next, job.*.chunk);
        if lo >= job.*.n {
            break;
        }
        let hi: i64 = @min(lo + job.*.chunk, job.*.n);
        job.*.body(job.*.ctx, lo, hi);
    }
    return null;
}

// Runs body(ctx, lo, hi) over [0, n) on `threads` workers (the caller is one
// of them) at QoS class `qos`. It returns after every chunk has finished.
fn parallel_for(n: i64, threads: i64, qos: i32, ctx: *u8, body: fn(*u8, i64, i64)) {
    let t: i64 = @min(@max(threads, 1), PAR_MAX_THREADS);
    let chunk: i64 = @max(n / (t * 8), 1);
    var job: ParJob = ParJob.{ body: body, ctx: ctx, n: n, chunk: chunk, next: 0, qos: qos };
    var tids: [64]u64 = @zero;
    for i in 1..t {
        let rc: i32 = pthread_create(&tids[i], null, par_worker, &job as *u8);
        @assert(rc == 0, "pthread_create failed");
    }
    par_worker(&job as *u8);
    for i in 1..t {
        pthread_join(tids[i], null);
    }
}

struct ParTestCtx {
    hits: *i64,
}

fn par_test_body(ctx: *u8, lo: i64, hi: i64) {
    let c: *ParTestCtx = ctx as *ParTestCtx;
    @atomic_add(c.*.hits, hi - lo);
}

test "parallel_for covers every index exactly once" {
    var hits: i64 = 0;
    var c: ParTestCtx = ParTestCtx.{ hits: &hits };
    parallel_for(100003, 6, QOS_USER_INITIATED, &c as *u8, par_test_body);
    @assert(hits == 100003);
    @assert(cpu_count() >= 1);
}
