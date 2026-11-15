# `std.sys.rt`: the runtime boundary

```ingot
import std.Env;
import std.par.Split;
```

`std.sys.rt` is the only part of `std` the compiler calls by path. It is written in
Ingot like the rest of the library; the entries below are its ABI, not a public API.
Everything outside this module reaches the runtime through `std.task`, `std.sync`,
`std.io` and `std.time`. The module is split into four link groups so that a program
that never blocks or spawns does not link the scheduler:

| Group | Contains | Linked when |
|---|---|---|
| core | entry, `trap`, stack probe | every executable |
| sched | scope, task, park/wake, select, `par for` | a `scope`, `spawn`, `par for`, `select`, or a blocking std call is reachable |
| test | `test_env` | `ingot test` |
| os | the `extern` declarations of the three systems | the group that uses it |

`std.sys.rt.os` is selected with `when @target.os == ...`. Only its declarations are
`extern`; every signature below is Ingot.

## 1. Core

```ingot
pub copy enum TrapCode: u8 {
    t01, t02, t03, t04, t05, t06, t07, t08, t09,
    t10, t11, t12, t13, t14, t15, t16, t17,
}

pub copy struct TrapSite {
    pub file: []u8,
    pub line: i32,
    pub col: i32,
}

/// The compiler's only trap entry. `a`, `b` and `c` are the trap's operands in the
/// order of diagnostics.md; `text` is the op name, assertion source or message.
pub fn trap(code: TrapCode, site: &TrapSite, a: i64, b: i64, c: i64, text: []u8) -> never

/// Called at function entry when the frame is larger than one page. Touches each page
/// below the caller's stack pointer; a guard-page fault reaches `stack_overflow`.
pub noinline fn stack_probe(bytes: i64) -> void

pub fn stack_overflow() -> never

/// The process entry. `entry` is the compiler-generated wrapper that calls the
/// program's `main` and maps its result and error to an exit status.
pub fn start(argc: i32, argv: *mut *mut u8, envp: *mut *mut u8, entry: fn(&Env) -> i32) -> never

/// The test harness's environment. Only reachable through the `@test_env` builtin;
/// linked only into `ingot test` binaries.
pub fn test_env() -> &Env
```

`start` builds the process's `Env` from `argc`, `argv` and `envp`, installs the
guard-page handler, and calls `entry` once. It exits with status 101 after `trap` and
with the status `entry` returns otherwise. `entry` is generated per package: it prints
`error: ...` for a returned error in the structural form of that error's type, then
returns 1. `start` itself performs no formatting and allocates nothing.

`stack_probe` is `noinline` and is never optimized away. On Windows it may be an
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

The scheduler owns per-worker caches for task records and stacks. `spawn` and parking
must not call the general heap. A `scope` whose state is left without `scope_wait` is a
compiler bug, not a user-visible condition.

## 3. Park, wake and select

```ingot
/// A waiter is allocated by the caller and stays valid until `wake` or the return of
/// `park`. One waiter is one event; it is not reused after it has fired.
pub struct Waiter { ... }

pub copy enum ParkResult: u8 { woken, canceled, timed_out }

/// `deadline_ns` is a monotonic deadline in nanoseconds, or 0 for none.
pub unsafe fn park(waiter: *mut Waiter, deadline_ns: i64, cancelable: bool) -> ParkResult

pub unsafe fn wake(waiter: *mut Waiter) -> void

/// One arm of a `select`. Construction is private to the waitable primitive; `select`
/// treats it as opaque and returns the chosen index, or -1 for the `else` arm.
pub struct WaitOp { ... }

pub fn select(ops: []WaitOp) -> i64
```

`park` parks the calling task; the waker need not be a task (a foreign callback may
call `wake`, which is why the pointer is `unsafe`). `cancelable` is true for I/O,
`Chan.send`, `Chan.recv` and timers, and false for `Mutex.lock`, `Task.join`,
`WaitGroup.wait` and the scope wait. A waitable arm that is not chosen must be
unregistered before `select` returns and must leave no value consumed; that obligation
belongs to the primitive that built its `WaitOp`.

## 4. `par for`

```ingot
/// The compiler-generated body for a range. `item` points to an i64 in the worker's
/// frame; the body must not retain it.
pub fn par_for_range(
    lo: i64,
    hi: i64,
    step: i64,
    grain: i64,
    body: fn(*mut u8, *mut u8),
    captures: *mut u8,
) -> void

/// The compiler-generated body for a `par.Split`. Captures are shared, so `C: Sync`;
/// the item is moved into the call and must be `Send`.
pub fn par_for_split[S: Split, C: Sync](
    it: S,
    grain: i64,
    body: fn(&C, S.Item),
    captures: &C,
) -> void
```

Both return after every iteration has finished. The body may run on any worker and in
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
| `sigaction` / `AddVectoredExceptionHandler` | guard-page handler |

`trap` itself, `stack_probe`, the scope functions, `spawn`, `task_join`, `task_wait`,
`park`, `wake`, `select` and both `par_for` entries are Ingot. The compiler never emits
an `extern` call directly; it calls the Ingot entry, and only that entry's body is
allowed to be the unsafe OS layer.

## Decisions for the human

All five are decided. The recommendation was accepted in each case.

- **D1. Entry split.** **Decided: accepted.** Two entries, `rt.start` (no scheduler)
  and `rt.start_tasks` (scheduler, `main` as the root task); the compiler selects one
  by reachability.
- **D2. Spawn record.** **Decided: accepted.** The runtime owns a copy of the argument
  record, with `result` at a compiler-computed aligned offset inside it. The scope owns
  the record until its wait.
- **D3. Trap ABI.** **Decided: accepted.** `trap(code, site, three operands, text)`,
  with `site` a static and operands in registers.
- **D4. `select` descriptor.** **Decided: accepted.** An opaque `WaitOp` built by the
  waitable primitive; `select` returns the chosen index, or -1 for `else`.
- **D5. `park` deadline.** **Decided: accepted.** Monotonic nanoseconds; 0 means none.

## Language questions

The waitable and foreign-thread questions remain open. L3 is an implementation
contract rather than a language feature.

- **L1. `waitable fn` lowering.** The specification does not say how an arbitrary
  waitable function becomes a `WaitOp`, how `Scope.canceled()` can be a poll in a normal
  call and a wait in `select`, or how a losing `Chan.recv` is prevented from consuming.
  This blocks a correct `select`. Sections 18, 21 and 22 are involved.
- **L2. Foreign-thread task state.** T17 says blocking, `spawn` and `par for` outside a
  task trap. It does not say whether a plain `scope` block or a `Task` handle may cross
  an `export fn` boundary. The runtime needs one rule; the spec has none.
- **L3. Main wrapper ABI (implementation contract).** `Env` has private capability
  fields, so only `std` can build it. The compiler needs a stable path and signature for
  the wrapper that calls `main`; the spec only says the runtime calls `main`.
