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

* `scope name { ... }` introduces `name`, a `&Scope`: a shared borrow of the block's
  scope, which is declared just outside the block's own variables. It can be passed to
  functions, so a callee can spawn into its caller's scope. Like any borrow of a local
  it holds a loan on the scope, so `name`, a copy of it, and anything a function
  returns `from` it cannot be returned from the block (E1203) or kept in a place that
  is used after the block (E1105).
* `spawn call in s` evaluates the callee and every argument **now**, on the current
  task, then starts the call as a new task in scope `s`.
  * As an expression it gives a `Task[R]`, where `R` is the call's result type.
    `Task[R]` is linear and is a view of the scope, so it cannot leave the block. It is
    consumed by `join()`, which blocks until the task is done and returns `R`. If `R`
    is `T ! E`, the result is handled like any other.
  * As a statement it gives no handle. The call must return `void`, so a fallible task
    handles its own errors.
* **When the block ends** by any path, including `return`, `fail` or `break`, the scope
  first waits for every task spawned in it. Deferred blocks inside the scope run before
  that wait, in the usual order. If the block is left because the function is failing,
  the scope is canceled before the wait (section 22), so that the remaining tasks stop
  at their next blocking call and an error does not hang behind a task that never ends.
* **What a task may be given.** Every argument, and the result type, must be `Send`.
  Spawning stores the arguments in the scope: the origins of every argument are added to
  the origin set of `s`, exactly as for a parameter declared `into s` (section 13.2),
  and they stay there until the scope ends. Joining a handle early does not release
  them. A nested scope does.
* **`s` outlives the block's own variables.** It is live until the wait at the end of
  the block is over, and the block's locals are gone before that wait. So every loan
  held by `s` must be on a place declared outside the `scope` block. That makes these
  two programs errors:

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

* **Spawning into a scope received as a parameter** is a store through that parameter,
  so the signature says so:

  ```ingot
  import std.task.Scope;

  fn start_worker(s: &Scope, jobs: &Queue into s, stats: &mut Stats into s) {
      spawn work(jobs, stats) in s;
  }
  ```

  The caller then knows that `jobs` and `stats` stay borrowed until its scope ends.
  Without `into s` the `spawn` is an error, and the fix adds the clause. A loan on one
  of `start_worker`'s own locals can never be given to the task.
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
  `par.Split`: something that can be divided in two, such as `chunks`, `chunks_mut`,
  `zip`, `enumerate`. The item of a `par for` over a `Split` is its `Self.Item`.

  ```ingot
  pub interface Split: Iter {
      fn len(self: &Self) -> i64;
      fn split_at(self: Self, mid: i64) -> (Self, Self);
  }
  ```

  The exact contract (minimum length, balance) is specified with the runtime.
* The body is checked as a closure that is `Sync` and is called once per item: it may
  read outer variables through shared borrows of `Sync` types, and it may not assign to,
  move or exclusively borrow anything declared outside the loop. Each item (`x`,
  `chunk`) must be `Send` and is owned by its iteration, which is how disjoint mutable
  access is expressed.
* `break`, `continue` to an outer loop, `return` and `try` are not allowed in the body.
  `fail` is not allowed either. A fallible body uses `par.try_each`, which returns the
  first error and cancels the rest.
* **Chunking.** The runtime splits the work recursively and lets idle workers steal
  halves. Because the M4 Pro's performance and efficiency cores run at different speeds,
  a loop is never divided evenly per thread. The default grain aims for at least eight
  chunks per worker. `par(grain: n) for ...` sets the smallest chunk in items.
* Reductions are library functions built on the same splitting: `par.sum(xs)`,
  `par.map_reduce(xs, identity, map, combine)`. They split at sizes that depend only on
  the length, never on the number of workers, give each chunk a private accumulator and
  combine in a fixed tree. The result is the same on every run and on every target.

## 20. `Send` and `Sync`

| Property | Meaning |
|---|---|
| `Send` | a value of this type may be moved to another task |
| `Sync` | a `&T` may be used from several tasks at once |

Both are about tasks, not OS threads. A task may resume on another worker thread after
it parks, and safe code cannot observe that.

Both are computed structurally: a struct, enum, tuple or array has the property if all
its parts do, and a container's allocator counts as a part. The base cases:

| Type | `Send` | `Sync` |
|---|---|---|
| scalars, vectors, `fn` pointers | yes | yes |
| `&T` | if `T: Sync` | if `T: Sync` |
| `&mut T` | if `T: Send` | if `T: Sync` |
| `*T`, `*mut T` | no | no |
| `Atomic[T]` | if `T: Send` | if `T: Send`, so that `Atomic[*mut T]` gets neither without an `unsafe impl` |
| `Mutex[T]`, `Chan[T, A]` | if `T: Send` | if `T: Send` |
| `RwLock[T]` | if `T: Send` | if `T: Send + Sync`, because readers share a `&T` |
| `Arena`, `FixedBuf` | yes | no |
| `Arc[T, A]` | if `T: Send + Sync` | if `T: Send + Sync` |
| a closure | if every capture is | if every capture is |
| `&dyn I` | if `Sync` is a supertrait of `I` | if `Sync` is a supertrait of `I` |
| `&mut dyn I` | if `Send` is a supertrait of `I` | if `Sync` is a supertrait of `I` |
| `&dyn Fn(..)`, `&mut dyn FnMut(..)` | no | no |

`Mutex`, `RwLock`, `Chan` and `Once` also require their contents to be `Owned`
(section 13.6).

A type built on raw pointers gets the properties only through `unsafe impl Send for T {}`
and `unsafe impl Sync for T {}`. Because safe code has no unsynchronized interior
mutability except the arena, almost every type an agent writes is both, and the checker
has something to say only when a borrow of an arena or a raw pointer crosses a task
boundary.

## 21. Shared state and communication

All of these are in `std.sync`. None of them is special to the compiler apart from the
atomics, which map to single instructions.

**Atomics.** `Atomic[T: AtomicValue]`: integers, `bool`, raw pointers and payload-free
enums (section 8). It is built on the atomic builtins of section 15.
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

**Channels.** `Chan[T, A]` is a bounded multi-producer multi-consumer queue of `Owned`
values.
`send(&self, v: T)` blocks while the channel is full and returns the value inside the
error if the channel is closed, because `T` may be linear. `recv(&self)` blocks while it
is empty and fails with `.closed` after `close()` once the queue has drained.
`try_send` and `try_recv` never block.

**Waiting on several things.** A function declared `waitable fn` is an operation the
scheduler can wait for without committing to it. `select` waits for the first of several:

```ingot
loop {
    select {
        r = inbox.recv() => {
            let msg = r catch _ => { break; };       // the channel was closed
            try process(msg);
        }
        _ = stop.wait() => { break; }
        _ = try clock.after_ms(250) => { try flush(out); }
    }
}
```

**Operations.** `std.task.Wait` is the interface of an operation the scheduler can
wait for:

```ingot
pub interface Wait {
    type Output;
    /// One attempt. It must not block. `.some(v)` completes the operation with `v`.
    /// `.none` means it is not ready: nothing was consumed, nothing that another task
    /// can observe was changed, and `cx` was told what to wait for.
    fn poll(self: &mut Self, cx: &mut WaitCx) -> ?Self.Output;
}
```

A `waitable fn` declared `-> R` has a body that **prepares** an operation: every
`return` gives a value of one type `W`, where `W: Wait + Discard` and `W.Output` is `R`.
The preparation runs once, must not block, and changes nothing another task can
observe. Its receiver is a shared borrow and its other parameters are `Discard`, so an
operation that is not chosen is simply dropped (E1410 otherwise).

* A direct call `x.f(args)` of a `waitable fn` runs the preparation, waits until the
  operation completes, and has type `R`.
* Each arm of a `select` is `pattern = call => block`, and the call may be written under
  `try`. `select` runs the preparation of every arm, arm by arm from the first, each
  left to right (section 5.1). Then it polls the operations, in a random order each
  time, until one completes. That arm's block runs, with the pattern bound to the
  result; the other operations are dropped, having had no effect. So a losing
  `Chan.recv` consumes nothing, and if several operations are ready, one is picked
  uniformly at random.
* Under `try`, the arm's call applies `try` to the result when the arm is chosen, as it
  would in a `let`: the pattern binds the success value, and an error leaves the
  function as any propagating `try` does.
* An optional last arm `else => block` is taken when no operation completes on the
  first round of polls, which makes the `select` non-blocking.
* `cx.canceled()` is true when the scope of the waiting task is canceled. An operation
  that observes cancellation (section 22) then completes with its `.canceled` error;
  one that does not is unaffected.
* The waitable functions of the standard library are `Chan.recv`, `Task.wait` (then
  `join()` returns at once), `Clock.after`, `Clock.after_ms` and `Scope.canceled`, which
  completes when the scope is canceled. `Scope.is_canceled()` reads the flag without
  waiting. The runtime half (`WaitCx`, wait queues, the `select` entry) is `std`'s and
  is not language.
* I/O operations are not waitable. Two of the three systems complete I/O instead of
  reporting readiness, and a started read cannot always be withdrawn without losing
  data. Code that must wait for I/O or something else gives the I/O to a child task and
  selects on a channel or on `Task.wait`.

## 22. Cancellation

* Every scope has a cancel flag. `s.cancel()` sets it for the scope and for every scope
  opened by code running inside it. A scope is also canceled when its block is left by
  a failing function (section 18).
* Cancellation is cooperative and is delivered as an error value. In a canceled scope
  these operations fail with their module's `.canceled` error instead of blocking: I/O,
  `Chan.send` and `Chan.recv`, and timers, on their own or as a `select` arm.
  CPU-bound code polls `task.canceled()`.
* `Mutex.lock`, `Task.join` and the wait at the end of a scope do not observe
  cancellation. Each waits for something that is itself bound to finish.
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
* **Spawn** must not allocate from the general heap. A new task is first a small
  record, in memory the runtime manages per worker, pushed on the spawning worker's
  deque. If nobody steals it before the parent reaches `join()`, the parent runs it
  directly on its own stack, so an unstolen spawn costs about as much as a function
  call plus two deque operations.
* **Task memory.** `spawn` is not a fallible operation. If the runtime cannot get
  memory for a task record or a stack, the program traps (section 16.1), in the same
  class as stack exhaustion. A program that must bound its memory bounds the number of
  tasks it starts *at once*: a task's record is recycled when the task has finished
  and nothing can join it (a statement-form `spawn`), or at its `join`, not at the end
  of the scope, so a long-lived scope that keeps spawning (a server's accept loop) does
  not grow.
* **Stacks.** A stolen or parked task runs on a stack taken from a per-worker cache.
  Stacks are fixed-size regions of reserved address space with a guard page, committed
  on demand: by first touch on macOS and Linux, and on Windows by a guard page that the
  runtime's fault handler moves down as the stack grows. On Windows the runtime also
  updates the thread's recorded stack bounds at every task switch, which the system
  needs for exception dispatch. Stacks never move, so borrows into a stack stay valid.
  The default reservation is 1 MiB and is set per package (`[runtime] task_stack`).
  Whole-program compilation lets the compiler bound the stack a task needs when its
  call graph has no recursion or indirect calls, and the runtime then reserves that
  bound instead.
* **Blocking.** Parking a task switches stacks in user space and saves only callee-saved
  registers. I/O uses kqueue on macOS, io_uring on Linux and I/O completion ports on
  Windows, behind one `std.io` API: a call that parks the task and returns the result.
* **Foreign calls** run on the calling task's stack unless the declaration is marked
  `extern "c-bigstack"`, which switches to the worker's own stack first. A foreign call
  that blocks in the kernel blocks its worker. Declaring it `extern "c-blocking"` tells
  the runtime to hand the worker's queue to a spare thread first.
* **Calls from foreign code.** The body of an `export fn` runs outside any task,
  whichever thread calls it, including a worker that is running foreign code. Outside
  a task, entering a `scope`, `spawn`, `par for`, `select` and every operation that
  would park trap (T17), whether or not anything would be spawned. `task.run(f)` is the
  way in: it runs `f` as a task and blocks the calling thread until `f` returns. A
  `Task` or a `&Scope` cannot cross the boundary: neither is an FFI type.
* **Assembly in std.** The stack switch is a short assembly file per target in
  `std.sys`, named by `std`'s manifest (`[link.<target>] asm`, section 33). No other
  package may list assembly sources.
* **No preemption.** A task runs until it blocks or returns. The OS still preempts
  worker threads, but a task that spins never yields to its siblings. `task.yield()`
  exists for the rare loop that needs it.
* **Deadlock.** If every task is parked and no timer, I/O or blocking foreign call is
  pending, the runtime
  traps with the list of parked tasks and what each waits for.
* **Thread-bound foreign state.** A task may resume on a different worker after it
  parks. Foreign libraries that require one OS thread are used from a dedicated OS
  thread: `thread.spawn(s, f)` runs `f` on its own thread as a child of scope `s`, with
  the borrowing rules of `spawn` (`f` is stored `into s`) and the scope's wait.
