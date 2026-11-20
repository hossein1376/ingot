# `std.sys.rt`: the runtime boundary

```ingot
import std.Env;
import std.par.Split;
```

Updated for the language designer's second review (spec `720763f`): the trap ABI,
the generated entry, waitable operations instead of `WaitOp`, chunked `par for`
bodies, the foreign-thread rule, thread-local state and the stack switch.

`std.sys.rt` is the only part of `std` the compiler calls by path. It is written in
Ingot like the rest of the library; the entries below are its ABI, not a public API.
Everything outside this module reaches the runtime through `std.task`, `std.sync`,
`std.io` and `std.time`. The module is split into four link groups so that a program
that never blocks or spawns does not link the scheduler:

| Group | Contains | Linked when |
|---|---|---|
| core | entry, `trap`, stack probe | every executable |
| sched | scope, task, park/wake, select, `par for` | a `scope`, `spawn`, `par for`, `select`, or a blocking std call is reachable |
| test | `test_start`, the manual clock | `ingot test` |
| os | the `extern` declarations of the three systems | the group that uses it |

`std.sys.rt.os` is selected with `when @target.os == ...`. Only its declarations are
`extern`; every signature below is Ingot.

## 1. Core

```ingot
pub copy enum TrapCode: u8 {
    t01, t02, t03, t04, t05, t06, t07, t08, t09,
    t10, t11, t12, t13, t14, t15, t16, t17,
}

/// A static record per trap site, emitted by the compiler (pipeline §7).
pub copy struct TrapSite {
    pub code: TrapCode,
    pub file: []u8,
    pub line: i32,
    pub col: i32,
    /// The fixed text of the message: the operator, the type name, the source text of
    /// an assertion.
    pub text: []u8,
}

/// The compiler's only trap entry. `a`, `b` and `c` are the run-time operands the
/// catalog's message prints (`{i}`, `{len}`, `{lo}`, `{hi}`, `{n}`), in its order.
pub fn trap(site: &TrapSite, a: i64, b: i64, c: i64) -> never

/// Called at function entry when the frame is larger than one page. Touches each page
/// below the caller's stack pointer; a guard-page fault reaches `stack_overflow`.
pub noinline fn stack_probe(bytes: i64) -> void

pub fn stack_overflow() -> never

/// The process entry when no scheduler entry is reachable. `entry` is the
/// compiler-generated `fn(&Env) -> i32` for the root package (for `ingot test`, a
/// wrapper that calls `test_start` with the package's tests). The compiler emits the
/// target's process entry symbol, which calls this (pipeline §8).
pub fn start(argc: i32, argv: *mut *mut u8, envp: *mut *mut u8, entry: fn(&Env) -> i32) -> never

/// Runs the one test the harness named: `env.args[1]` is its index in `tests`, in
/// decimal, and `env.args[2]` its temporary directory. Builds the test's `Env` (the
/// same capabilities, `args` holding `argv[0]` only) and its `TestEnv` on this frame,
/// calls the test, and returns its status: 0 passed, 1 failed.
pub fn test_start(env: &Env, tests: []fn(&testing.TestEnv) -> i32) -> i32
```

A test block is compiled as a function `fn(&testing.TestEnv) -> i32`, and
`@test_env()` is its parameter (tools.md §3, `ingot test`): the `TestEnv` lives in
`test_start`'s frame for the whole test, so no global state holds it. A `try` that
leaves the block prints `error: ` and the structural form of its error to standard
error, then returns 1. Arguments the harness did not write (a wrong count, an index
out of range, a non-decimal index) are a harness bug: `test_start` returns 2.

`start` builds the process's `Env` from `argc`, `argv` and `envp`, installs the
guard-page handler, and calls `entry` once. It exits with status 101 after `trap` and
with the status `entry` returns otherwise. `entry` is generated per package: it prints
`error: ...` for a returned error in the structural form of that error's type, then
returns 1. `start` itself performs no formatting and allocates nothing from an
allocator. The one memory it needs, the table behind `Env.args` (one `[]u8` per
argument, pointing into `argv`'s strings), is a single region reserved from the
operating system through `std.sys` (`mmap` or `VirtualAlloc`), sized for `argc` entries,
filled once and never freed. If the operating system refuses that region, `start` traps
T12 at `<runtime>:0:0` (diagnostics.md §18) before `entry` runs. The capability values
of `Env` are built by their modules' `pub(pkg)` constructors (`env.md`).

On Windows there is no C runtime: the entry symbol takes no arguments and `start`
reads the command line and environment from the system (05-tooling §33.2, review 2
U3). On macOS and Linux the system's start code calls the entry with `argc`, `argv`
and `envp`.

`stack_probe` is `noinline` and is never optimized away. If the back end emits probes
inline in the prologue on every target (to be confirmed in step B1), it is not needed. On Windows it may be an
`extern` alias for `__chkstk`; on macOS and Linux it is an Ingot loop that writes one
byte per page. `stack_overflow` is called from the fault handler and must not allocate.

## 2. Tasks and scopes

```ingot
/// Runtime state behind `std.task.Scope`. Opaque: only `rt` reads its fields.
pub struct ScopeState { ... }

/// One task record. Opaque: the scheduler allocates and recycles it.
pub struct TaskRecord { ... }

/// One live task. `result` points into the scope-owned argument record.
pub copy struct TaskRef {
    pub record: *mut TaskRecord,
    pub result: *mut u8,
}

/// Initializes the state of a `scope` block. Called once, before its body.
pub fn scope_enter(state: *mut ScopeState) -> void

/// Starts `start(args)` as a task in `state`. Arguments are copied into scope memory
/// now; the result slot begins at the first suitably aligned offset after them.
pub fn spawn(
    state: *mut ScopeState,
    start: fn(*mut u8),
    args: *u8,
    args_size: i64,
    args_align: i64,
    result_size: i64,
    result_align: i64,
) -> TaskRef

/// Waits for this task and releases its record. Does not observe cancellation.
pub fn task_join(task: TaskRef) -> void

/// Waits for this task without consuming the handle. The `waitable` primitive behind
/// `Task.wait`.
pub fn task_wait(task: TaskRef) -> void

/// Waits for every task spawned in the scope. Does not observe cancellation.
pub fn scope_wait(state: *mut ScopeState) -> void

/// Sets the scope's cancel flag and every scope opened inside it.
pub fn scope_cancel(state: *mut ScopeState) -> void

pub fn scope_canceled(state: *ScopeState) -> bool

pub fn task_canceled() -> bool

pub fn yield_now() -> void

pub copy enum QosClass: u8 { performance, efficiency }
pub fn set_qos(class: QosClass) -> void

/// The process entry when the scheduler is linked. `entry` runs as the root task.
pub fn start_tasks(
    argc: i32,
    argv: *mut *mut u8,
    envp: *mut *mut u8,
    entry: fn(&Env) -> i32,
) -> never
```

The compiler allocates one `std.task.Scope` (with its `Cell` and `ScopeState`) per
`scope` block, calls `scope_enter` with the address of that state, and uses the address
as the `Scope` behind `s`. `scope_wait` is the lowering of `scope_wait(s)` and is
emitted after the block's locals have ended. On an error exit the compiler emits
`scope_cancel` before it. `spawn` never returns a failure: if the runtime cannot get a
task record or stack, it traps T12.

`spawn` copies `args_size` bytes from `args`, aligned to `args_align`, into memory owned
by the scope, and computes `result` at the next offset aligned to `result_align`. The
generated thunk has type `fn(*mut u8)` and takes the address of that copy; it writes the
return value at `result`. The compiler knows the same offset, so `Task.join` casts
`result` to `*mut R` and moves the value out. Argument and result records are not
dropped by the runtime; the language's move rules require every result to be joined or
forgotten, and statement-form spawns have `void` results.

A task record is recycled at `task_join`, or when the task finishes if it came from a
statement-form `spawn`, so a scope that keeps spawning (an accept loop) does not grow
(03-concurrency §23). A scope child may also be an OS thread, started by
`thread_spawn(state, start, args, ...)` with the same record layout; the scope's wait
joins it.

**Thread-local state.** The current worker and task are found through one OS
thread-local slot: `pthread_getspecific`/`pthread_setspecific`, `TlsGetValue`/
`TlsSetValue`. It is a call, so no address computed before a park is reused after the
task resumes on another worker. **The stack switch** is a short assembly file per
target in `std.sys` (`[link.<target>] asm` in std's manifest, 05-tooling §33.2):
save the callee-saved registers and swap the stack pointer, and on Windows also the
stack bounds in the thread block.

The scheduler owns per-worker caches for task records and stacks. `spawn` and parking
must not call the general heap. A `scope` whose state is left without `scope_wait` is a
compiler bug, not a user-visible condition.

## 3. Park, wake and select

```ingot
/// A waiter is allocated by the caller and stays valid until `wake` or the return of
/// `park`. One waiter is one event; it is not reused after it has fired.
pub struct Waiter { ... }

pub copy enum ParkResult: u8 { woken, canceled, timed_out }

/// The largest `i64`: no deadline. (Zero is a real instant: the test clock starts at
/// monotonic zero.)
pub const no_deadline: i64 = 9223372036854775807;

/// `deadline_ns` is a monotonic deadline in nanoseconds, or `no_deadline`.
pub unsafe fn park(waiter: *mut Waiter, deadline_ns: i64, cancelable: bool) -> ParkResult

pub unsafe fn wake(waiter: *mut Waiter) -> void

/// What a `std.task.Wait` operation's `poll` is given (03-concurrency §21).
pub struct WaitCx { ... }

impl WaitCx {
    /// True when the waiting task's scope is canceled.
    pub fn canceled(self: &WaitCx) -> bool
    /// Wake the task when `queue` is signaled after `ticket`.
    pub fn on(self: &mut WaitCx, queue: &WaitQueue, ticket: u64) -> void
    /// Wake the task at a monotonic deadline.
    pub fn until(self: &mut WaitCx, deadline_ns: i64) -> void
}

/// A queue of waiting tasks with tickets, so that a wake between a poll and the park
/// is not lost: a poll reads the ticket, and the park returns at once if it moved.
pub struct WaitQueue { ... }

/// One arm of a `select`, as the compiler passes it: the prepared operation, the
/// generated poll function for its type, and the place for its result.
pub copy struct SelectArm {
    pub op: *mut u8,
    pub poll: fn(*mut u8, &mut WaitCx, *mut u8) -> bool,
    pub result: *mut u8,
}

/// Polls the arms in a random order each round until one completes; returns its index,
/// or -1 for the `else` arm when `has_else` and nothing completed on the first round.
pub fn select(arms: []SelectArm, has_else: bool) -> i64
```

`park` parks the calling task; the waker need not be a task (a foreign callback may
call `wake`, which is why the pointer is `unsafe`). `cancelable` is true for I/O,
`Chan.send`, `Chan.recv` and timers, and false for `Mutex.lock`, `Task.join`,
`WaitGroup.wait` and the scope wait. A `poll` consumes only when it returns `.some`
(its `true` here), and the polls run one after another on the selecting task, so a
losing arm has had no effect and no commit protocol between arms is needed. A direct
call of a waitable function is a `select` over one arm with no `else`.

## 4. `par for`

```ingot
/// The compiler-generated body runs one chunk: it iterates `lo..hi` (with the loop's
/// step) itself, so the hot loop has no indirect call per item and can be vectorized.
pub fn par_for_range(
    lo: i64,
    hi: i64,
    step: i64,
    grain: i64,
    body: fn(captures: *mut u8, lo: i64, hi: i64),
    captures: *mut u8,
) -> void

/// For a `par.Split`, the leaf body takes a whole part and iterates it with `next`.
/// Captures are shared, so `C: Sync`; the part is moved into the call and must be
/// `Send`.
pub fn par_for_split[S: Split, C: Sync](
    it: S,
    grain: i64,
    body: fn(&C, S),
    captures: &C,
) -> void
```

`exec.par` uses the same chunk shape, each chunk run as `exec.simd`. Both return after
every iteration has finished. The body may run on any worker and in
any order. Iterations are never duplicated. The runtime splits a range or a `Split`
recursively and lets idle workers steal halves; it does not divide evenly per worker.
`grain` is the smallest chunk in items; when it is 0, the runtime picks the default of
at least eight chunks per worker. `Split.split_at(mid)` must return two non-empty parts
when `0 < mid < len`, and `len` of the parts must sum to the original length. The
runtime uses `mid = len / 2` and calls no split for a part of length 0 or 1.

## 5. What is `extern`

The following are the OS layer, declared in `std.sys.rt.os` under `when`:

| Declaration | Used by |
|---|---|
| `write(fd: i32, buf: *u8, len: i64) -> i64` | `trap`, `entry` error line |
| `exit(status: i32) -> never` | `start`, `start_tasks` |
| `mmap` / `munmap` / `mprotect` (three spellings) | stacks, `Vm`, `Arena` |
| `VirtualAlloc` / `VirtualFree` / `VirtualProtect` | Windows stacks, `Vm` |
| `pthread_create` / `pthread_join` / `pthread_attr_*` | worker threads, `thread` |
| `CreateThread` / `WaitForSingleObject` | Windows workers, `thread` |
| futex, `os_unfair_lock`, `WaitOnAddress` | `park`/`wake` |
| `clock_gettime` / `mach_absolute_time` / `QueryPerformanceCounter` | deadlines, `Clock` |
| `sigaction` / `AddVectoredExceptionHandler` | guard-page handler, an `export fn` that runs outside any task |
| `pthread_getspecific` / `pthread_setspecific` / `TlsGetValue` / `TlsSetValue` | the current worker and task |

`trap` itself, `stack_probe`, the scope functions, `spawn`, `task_join`, `task_wait`,
`park`, `wake`, `select` and both `par_for` entries are Ingot. The compiler never emits
an `extern` call directly; it calls the Ingot entry, and only that entry's body is
allowed to be the unsafe OS layer.

## Decisions for the human

All five are decided. The recommendation was accepted in each case.

- **D1. Entry split.** **Decided: accepted.** Two entries, `rt.start` (no scheduler)
  and `rt.start_tasks` (scheduler, `main` as the root task); the compiler selects one
  by reachability.
- **D2. Spawn record.** **Decided: accepted, changed by review 2.** The runtime owns a
  copy of the argument record, with `result` at a compiler-computed aligned offset
  inside it. It is recycled at `join`, or at finish for a statement-form spawn, not at
  the scope's wait.
- **D3. Trap ABI.** **Decided: changed by review 2.** `trap(site, a, b, c)`: the code,
  position and fixed text live in the static `TrapSite`; four arguments per cold block.
- **D4. `select` descriptor.** **Decided: replaced by review 2.** Each arm is a prepared
  `Wait` operation with a generated poll function; `WaitCx` and `WaitQueue` replace
  `WaitOp`.
- **D5. `park` deadline.** **Decided: changed by review 2.** Monotonic nanoseconds;
  `no_deadline` (the largest `i64`) means none, since the test clock starts at zero.
- **D6. No temporary C runtime.** **Decided (review 2).** The entry, `trap`, `write` and
  `exit` are written in Ingot from the first back-end step.

## Language questions

All three are settled by the spec of review 2 (`720763f`):

- **L1. `waitable fn` lowering.** 03-concurrency §21 and pipeline §5: a waitable body
  prepares a `std.task.Wait` operation; `select` prepares every arm and polls.
- **L2. Foreign-thread task state.** 03-concurrency §23: the body of an `export fn` runs
  outside any task; entering a `scope`, `spawn`, `par for`, `select` or parking there is
  T17; `task.run(f)` is the way in. A `Task` or `&Scope` is not an FFI type.
- **L3. Main wrapper ABI.** Pipeline §8: the compiler generates `entry: fn(&Env) -> i32`
  and the process entry symbol, which calls `start` or `start_tasks`.
