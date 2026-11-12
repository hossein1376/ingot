# Ingot v1 specification, part 3: concurrency

## 17. The model

* The unit of concurrency is the **task**: a function call running on its own stack,
  scheduled by the runtime onto a fixed set of worker threads, one per core.
* Concurrency is **structured**. A task is always started inside a `scope` and the scope
  does not end until the task has. A task can therefore borrow from the stack frames
  around its scope, and there are no detached tasks.
* Tasks are **stackful**. Any function may block: on a join, a channel, a lock, a timer
  or I/O. Blocking parks the task and frees the worker. There is no `async`/`await` and
  functions are not split into two colors.
* **Data races are compile errors.** The exclusivity rule of section 12 applies across
  tasks without change, together with the `Send` and `Sync` properties of section 20.
* The runtime is part of the standard library, is written in Ingot, and is linked only
  into programs that contain `scope`, `par for` or a blocking standard-library call.

## 18. `scope` and `spawn`

```ingot
fn fib(n: i64) -> i64 {
    if n < 20 { return fib_seq(n); }
    scope s {
        let left = spawn fib(n - 1) in s;      // Task[i64]
        let right = fib(n - 2);                // runs on this task
        return left.join() + right;
    }
}

fn serve(listener: &Listener, db: &Db) -> void ! NetErr {
    scope s {
        loop {
            let conn = try listener.accept();
            spawn handle(conn, db) in s;       // statement form: no handle
        }
    }
}
```

* `scope name { ... }` introduces `name`, a value of type `&Scope` that is a view of this
  block. It can be passed to functions, so a callee can spawn into its caller's scope.
* `spawn call in s` evaluates the callee and every argument **now**, on the current
  task, then starts the call as a new task in scope `s`.
  * As an expression it gives a `Task[R]`, where `R` is the call's result type.
    `Task[R]` is linear: it is consumed by `join()`, which blocks until the task is done
    and returns `R`. If `R` is `T ! E`, the result is handled like any other.
  * As a statement it gives no handle. The call must return `void`, so a fallible task
    handles its own errors.
* **When the block ends** by any path, including `return`, `fail` or `break`, the scope
  first waits for every task spawned in it. Deferred blocks inside the scope run before
  that wait, in the usual order.
* **What a task may be given.** Every argument must be `Send`. Every loan in the
  arguments must be on a place declared outside the `scope` block, so that it outlives
  the scope. Each such loan stays live until the task's handle is joined, or until the
  end of the scope for the statement form. This is what makes these two programs errors:

  ```ingot
  scope s {
      var count: i64 = 0;
      spawn bump(&mut count) in s;     // error: `count` is declared inside the scope
  }

  var total: i64 = 0;
  scope s {
      spawn bump(&mut total) in s;
      spawn bump(&mut total) in s;     // error: `total` is already exclusively borrowed
  }
  ```

* A trap in any task ends the process, as everywhere else.

## 19. `par for`

```ingot
fn scale(xs: []mut f32, k: f32) {
    par for x in xs { x.* = x.* * k; }
}

fn histogram(data: []u8, bins: &[256]Atomic[i64]) {
    par for chunk in data.chunks(64 << 10) {
        var local: [256]i64 = @zero;
        for b in chunk { local[b as i64] += 1; }
        for i in 0..256 { bins[i].add(local[i], .relaxed); }
    }
}
```

* `par for` is a `for` loop whose iterations may run at the same time, in any order, on
  any worker. It ends when every iteration has.
* The iterated expression must be a range, a slice, or a value implementing
  `par.Split` (something that can be divided in two, such as `chunks`, `chunks_mut`,
  `zip`, `enumerate`).
* The body is checked as a closure that is `Sync` and is called once per item: it may
  read outer variables through shared borrows of `Sync` types, and it may not assign to
  or exclusively borrow anything declared outside the loop. Each item (`x`, `chunk`) is
  owned by its iteration, which is how disjoint mutable access is expressed.
* `break`, `continue` to an outer loop, `return` and `try` are not allowed in the body.
  `fail` is not allowed either. A fallible body uses `par.try_each`, which returns the
  first error and cancels the rest.
* **Chunking.** The runtime splits the work recursively and lets idle workers steal
  halves. Because the M4 Pro's performance and efficiency cores run at different speeds,
  a loop is never divided evenly per thread. The default grain aims for at least eight
  chunks per worker. `par(grain: n) for ...` sets the smallest chunk in items.
* Reductions are library functions built on the same splitting: `par.sum(xs)`,
  `par.map_reduce(xs, identity, map, combine)`. They give each chunk a private
  accumulator and combine in a fixed tree, so the result does not depend on scheduling.

## 20. `Send` and `Sync`

| Property | Meaning |
|---|---|
| `Send` | a value of this type may be moved to another task |
| `Sync` | a `&T` may be used from several tasks at once |

Both are computed structurally: a struct, enum, tuple or array has the property if all
its parts do. The base cases:

| Type | `Send` | `Sync` |
|---|---|---|
| scalars, vectors, `fn` pointers | yes | yes |
| `&T` | if `T: Sync` | if `T: Sync` |
| `&mut T` | if `T: Send` | if `T: Sync` |
| `*T`, `*mut T` | no | no |
| `Atomic[T]`, `Mutex[T]`, `RwLock[T]`, `Chan[T, A]` | if `T: Send` | if `T: Send` |
| `Arena`, `FixedBuf` | yes | no |
| `Arc[T, A]` | if `T: Send + Sync` | if `T: Send + Sync` |
| a closure | if every capture is | if every capture is |

A type built on raw pointers gets the properties only through `unsafe impl Send for T {}`
and `unsafe impl Sync for T {}`. Because safe code has no unsynchronized interior
mutability except the arena, almost every type an agent writes is both, and the checker
has something to say only when a borrow of an arena or a raw pointer crosses a task
boundary.

## 21. Shared state and communication

All of these are in `std.sync`. None of them is special to the compiler apart from the
atomics, which map to single instructions.

**Atomics.** `Atomic[T]` for integers, `bool`, raw pointers and payload-free enums.
Every operation names its ordering: `x.load(.acquire)`, `x.store(v, .release)`,
`x.add(n, .relaxed)`, `x.compare_exchange(old, new, .seq_cst, .relaxed)`.

**Locks.** A `Mutex[T]` owns the data it protects, so the data cannot be reached
without the lock:

```ingot
static table: Mutex[Stats] = Mutex[Stats].init(Stats.zero());

fn record(n: i64) {
    var g = table.lock();
    defer { g.unlock(); }
    g.get().total += n;
}
```

The guard is a linear view of the mutex. `get(self: &mut Guard[T])` gives
`&mut T from self`. Forgetting to unlock is a compile error. `RwLock[T]` is the same with `read()` and `write()`.

**Channels.** `Chan[T, A]` is a bounded multi-producer multi-consumer queue.
`send(&self, v: T)` blocks while the channel is full and returns the value inside the
error if the channel is closed, because `T` may be linear. `recv(&self)` blocks while it
is empty and fails with `.closed` after `close()` once the queue has drained.
`try_send` and `try_recv` never block.

**Waiting on several things.** A function declared `waitable fn` is an operation the
scheduler can wait for without committing to it. `select` waits for the first of several:

```ingot
select {
    msg = inbox.recv() => { try process(msg); }
    _ = done.wait() => { break; }
    _ = clock.after_ms(250) => { try flush(out); }
    else => { idle(); }                          // optional: taken if nothing is ready now
}
```

* Each arm is `pattern = waitable_call => block`. Exactly one arm's operation completes
  and only its block runs. The other operations have no effect.
* A `waitable fn` takes its receiver by shared borrow, so an arm that is not chosen
  consumes nothing.
* If several operations are ready, one is picked uniformly at random.
* The waitable functions in the standard library are `Chan.recv`, `Task.wait` (then
  `join()` returns at once), `Clock.after_ms` and `Scope.canceled`.
* I/O operations are not waitable. Two of the three systems complete I/O instead of
  reporting readiness, and a started read cannot always be withdrawn without losing
  data. Code that must wait for I/O or something else gives the I/O to a child task and
  selects on a channel or on `Task.wait`.

## 22. Cancellation

* Every scope has a cancel flag. `s.cancel()` sets it for the scope and every scope
  nested inside its tasks.
* Cancellation is cooperative and is delivered as an error value. A blocking
  standard-library operation called from a task in a canceled scope fails with its
  module's `.canceled` error instead of blocking. CPU-bound code polls
  `task.canceled()`.
* Nothing is ever stopped from outside. A task ends by returning, so its `defer` blocks
  always run and linear values are always consumed.
* `par.try_each` and `task.first_of` are built on this: the first failure or the first
  success cancels the siblings.

## 23. The runtime and its costs

This section is normative only where it says "must".

* **Workers.** One worker thread per logical core, created on first use. On macOS the
  workers are split into a performance group and an efficiency group by quality-of-service
  class, since the OS offers no thread affinity. `task.set_qos(class)` moves the current
  task and the tasks it spawns afterwards to the matching group. On the 7800X3D all cores
  are alike and workers are pinned one per logical core, under Linux and Windows alike,
  and `set_qos` only adjusts thread priority.
* **Spawn** must not allocate from the general heap. A new task is first a small record
  pushed on the spawning worker's deque. If nobody steals it before the parent reaches
  `join()`, the parent runs it directly on its own stack, so an unstolen spawn costs
  about as much as a function call plus two deque operations.
* **Stacks.** A stolen or parked task runs on a stack taken from a per-worker cache.
  Stacks are fixed-size regions of reserved address space with a guard page, committed
  on demand: by first touch on macOS and Linux, and on Windows by a guard page that the
  runtime's fault handler moves down as the stack grows. On Windows the runtime also
  updates the thread's recorded stack bounds at every task switch, which the system
  needs for exception dispatch. Stacks never move, so borrows into a stack stay valid. Whole-program
  compilation lets the compiler bound the stack a task needs when its call graph has no
  recursion or indirect calls, and the runtime uses that bound for the reservation.
* **Blocking.** Parking a task switches stacks in user space and saves only callee-saved
  registers. I/O uses kqueue on macOS, io_uring on Linux and I/O completion ports on
  Windows, behind one `std.io` API: a call that parks the task and returns the result.
* **Foreign calls** run on the calling task's stack unless the declaration is marked
  `extern "c-bigstack"`, which switches to the worker's own stack first. A foreign call
  that blocks in the kernel blocks its worker. Declaring it `extern "c-blocking"` tells
  the runtime to hand the worker's queue to a spare thread first.
* **No preemption.** A task runs until it blocks or returns. The OS still preempts
  worker threads, but a task that spins never yields to its siblings. `task.yield()`
  exists for the rare loop that needs it.
* **Deadlock.** If every task is parked and no timer or I/O is pending, the runtime
  traps with the list of parked tasks and what each waits for.
* **Thread-bound foreign state.** A task may resume on a different worker after it
  parks. Foreign libraries that require one OS thread are used from `thread.scope`,
  which gives structured, dedicated OS threads with the same borrowing rules.
