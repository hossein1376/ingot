// Explicit threads and cores. Ingot has no hidden runtime or scheduler:
// threads are plain pthreads, shared counters use LSE atomics, and @qos
// steers work toward P-cores (user-interactive) or E-cores (background)
// through macOS's Quality-of-Service classes.
extern fn printf(fmt: *u8, ...) -> i32;
extern fn pthread_create(tid: *u64, attr: *u8, start: fn(*u8) -> *u8, arg: *u8) -> i32;
extern fn pthread_join(tid: u64, ret: *u8) -> i32;

const QOS_USER_INTERACTIVE: i32 = 33; // 0x21: prefer performance cores
const QOS_BACKGROUND: i32 = 9;        // 0x09: prefer efficiency cores
const THREADS: i64 = 8;
const ITERS: i64 = 100000;

struct Job {
    counter: *i64,
    sum: i64,
    id: i64,
}

fn worker(arg: *u8) -> *u8 {
    let job: *Job = arg as *Job;
    if job.*.id % 2 == 0 {
        @qos(QOS_USER_INTERACTIVE);
    } else {
        @qos(QOS_BACKGROUND);
    }
    var local: i64 = 0;
    for i in 0..ITERS {
        local += i;
        @atomic_add(job.*.counter, 1);
    }
    job.*.sum = local;
    return null;
}

pub fn main() -> i32 {
    var counter: i64 = 0;
    var tids: [8]u64 = @zero;
    var jobs: [8]Job = @zero;
    for t in 0..THREADS {
        jobs[t] = Job.{ counter: &counter, sum: 0, id: t };
        let rc: i32 = pthread_create(&tids[t], null, worker, &jobs[t] as *u8);
        @assert(rc == 0, "pthread_create failed");
    }
    var total: i64 = 0;
    for t in 0..THREADS {
        pthread_join(tids[t], null);
        total += jobs[t].sum;
    }
    printf(c"counter=%lld (expected %lld), checksum=%lld\n", @atomic_load(&counter), THREADS * ITERS, total);
    if @atomic_load(&counter) != THREADS * ITERS {
        return 1;
    }
    return 0;
}
