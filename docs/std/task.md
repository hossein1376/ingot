# `std.task`: tasks, scopes and OS threads

```ingot
import std.sync.Cell;
import std.sys.rt;
```

`scope s { ... }` is the only way to make a `Scope`; `spawn` is the only way to make a
`Task`. Both are compiler-known by path (pipeline §8), so the fields below are private.
Updated for the language designer's second review (spec `bd0bbd2`, `720763f`): `s` is a
shared borrow of the block's `Scope`, waitable functions prepare a `Wait` operation,
`Scope.canceled` is split, and `thread.spawn` replaces `thread.group`.

## 1. `Scope`

```ingot
/// The scope of a `scope s { ... }` block. Lowering declares one per block and binds
/// `s = &σ`, a shared borrow (03-concurrency §18), so `s` cannot outlive the block. A
/// program cannot construct one.
pub struct Scope {
    state: Cell[rt.ScopeState],
}

impl Scope {
    /// Sets the cancel flag for this scope and for every scope opened inside it.
    pub fn cancel(self: &Scope) -> void

    /// Reads the cancel flag. Never waits.
    pub fn is_canceled(self: &Scope) -> bool

    /// Completes when the scope is canceled, with no result. A direct call waits for
    /// that; as a `select` arm it is one of the operations (section 21).
    pub waitable fn canceled(self: &Scope) -> void
}
```

Properties of `Scope`:

| Property | Value | Reason |
|---|---|---|
| `Copy` | no | it owns mutable runtime state |
| `Discard` | yes | it owns no value and needs no consuming function |
| linear | no | tasks in it are waited for by the compiler, not by the user |
| `Owned` | yes | it holds no borrow; the state is self-contained |
| `Send` | yes | an `&Scope` may be passed to another task, which may spawn into it |
| `Sync` | yes | every field is written under the runtime's atomics or a lock |
| interior mutability | yes | it contains the `Cell` that makes `&Scope` mutation safe; `cancel` and `spawn` write through it |

`Send` and `Sync` are `unsafe impl`s because `Scope` contains a `Cell` and a
`ScopeState` with raw pointers. The cell is the reason an `&Scope` must not be marked
read-only by the optimizer.

## 2. `Task[R]`

```ingot
/// A spawned task. Linear: it must be joined. It borrows its scope, so it cannot leave
/// the scope's block.
pub linear struct Task[R] {
    scope: &Scope,
    task: rt.TaskRef,
}

impl[R] Task[R] {
    /// Waits for the task, moves its result out and recycles the task record.
    /// Does not observe cancellation (section 22).
    pub fn join(self: Task[R]) -> R from self

    /// Waits until the task is done without consuming the handle. The result is read
    /// by `join`, which then returns at once.
    pub waitable fn wait(self: &Task[R]) -> void
}
```

`join` returns `R` as written. If the spawned call was fallible, `R` is a `T ! E` and
the caller handles it as usual. The result pointer is the `result` field of
`rt.TaskRef`; `join` casts it to `*mut R` after the task is done. The `from self` clause
covers a result that is itself a view; the value lives in scope memory, not in the
handle.

Properties of `Task[R]`:

| Property | Value | Reason |
|---|---|---|
| `Copy` | no | it is a linear handle |
| `Discard` | no | a task must be joined or `@forget`-ed |
| linear | yes | the task record and its result would otherwise leak |
| `Owned` | no | it holds `&Scope` |
| view | yes | it is bound to the scope's block |
| `Send` | if `R: Send` | `unsafe impl`; the runtime synchronizes completion and the result is `Send` |
| `Sync` | no | `wait` is the only shared operation, and sharing a handle is not needed |

The compiler builds a `Task` from the `rt.TaskRef` returned by `spawn`; the fields are
private to `std.task`, and compiler-generated code is not subject to visibility
(01-core §2). A task record is recycled at `join`, or, for a statement-form `spawn`,
when the task finishes (03-concurrency §23), so a long-lived scope does not grow.

## 3. Functions in the module

```ingot
/// Whether the current task's scope is canceled. For CPU-bound code that cannot block.
pub fn canceled() -> bool

/// Runs `f` as a task and blocks the calling thread until it returns. The way into the
/// runtime from the body of an `export fn`, which runs outside any task (section 23).
pub fn run[R, F: FnOnce() -> R + Send](f: F) -> R

/// The operation a `waitable fn` prepares (section 21). `WaitCx` is `rt.md` §3's.
pub interface Wait {
    type Output;
    fn poll(self: &mut Self, cx: &mut WaitCx) -> ?Self.Output;
}

/// Offers the worker to another task.
pub fn yield() -> void

pub copy enum Qos: u8 { performance, efficiency }

/// Moves the current task and the tasks it spawns afterwards to a quality-of-service
/// class. On the x86 target it only changes thread priority (section 23).
pub fn set_qos(class: Qos) -> void

/// Runs `a` and `b` in a nested scope; the first success cancels the other. If both
/// fail, returns the first failure, and if the scope is canceled, returns the
/// cancellation of the first task that observes it.
pub fn first_of[
    R: Send + Discard,
    E: Send + Discard,
    A: FnOnce() -> R ! E + Send,
    B: FnOnce() -> R ! E + Send,
](
    a: A,
    b: B,
) -> R ! E
```

`canceled()` is the module-level poll named in section 22, and `Scope.is_canceled()`
reads one scope's flag. `Scope.canceled()` is the waitable form: a direct call waits,
so `if s.canceled()` is a type error, not a silent block.

`first_of` needs `R: Discard` and `E: Discard` because when both closures finish, one
result, or the loser's error, is dropped. It needs no `from a, b`: a type parameter
bounded by `FnOnce() -> R ! E` mentions `R` and `E` (02-memory §13.3), so the caller
adds the closures' origins to the result.

`first_of` takes exactly two closures because two closures of different anonymous
types cannot be stored in one slice. Its own nested scope owns the arguments and waits
before it returns, so its callers do not keep their borrows until the caller's scope
ends. A three-way form (`first_of3`) is a later addition if the human wants it.

## 4. `std.thread`

Dedicated OS threads for foreign state that must stay on one thread. A thread is a
child of a scope, like a task: the only store a signature can declare for a closure is
`into` a scope (review 2, S5-4; a library `Group` cannot hold borrowing closures, and a
linear group in the caller's frame could be `@forget`-ed while threads still run).

```ingot
/// Runs `f` on a dedicated OS thread. The scope waits for the thread as it waits for
/// its tasks.
pub fn spawn[F: FnOnce() + Send](s: &Scope, f: F into s) -> void

/// The OS thread that is running the current task.
pub fn id() -> ThreadId

pub copy struct ThreadId {
    id: u64,
}
```

Used as `scope s { thread.spawn(s, fn [&data]() { ... }); }`. The borrow rules are those
of `spawn`, and the wait is the block's. The runtime needs one more kind of scope child,
a thread (`rt.md` §2). `ThreadId` is `Copy`, `Discard`, `Owned`, `Send` and `Sync`.

## Decisions for the human

All five are decided. The recommendation was accepted in each case.

- **D1. `first_of` arity.** **Decided: accepted.** Exactly two typed closures.
- **D2. `first_of` error choice.** **Decided: accepted.** If both fail, return the first
  failure that finishes.
- **D3. `Task` `Send`.** **Decided: accepted.** `Task[R]` is `Send` when `R: Send`.
- **D4. `ThreadId` contents.** **Decided: accepted.** The OS thread id as `u64`.
- **D5. `Group` spelling.** **Decided: accepted, then replaced by review 2.**
  `thread.spawn(s, f)` into a scope; `thread.group` could not be written or used.
- **D6. `Scope.canceled`.** **Decided (review 2).** Split into `is_canceled` (a read)
  and the waitable `canceled` (completes on cancellation).

## Language questions

All three are settled by the spec of review 2:

- **L1. `waitable fn` lowering.** 03-concurrency §21: a waitable body prepares a `Wait`
  operation; a direct call waits for it, and `select` polls the prepared operations.
- **L2. `Scope.canceled` has two meanings.** Split (D6).
- **L3. No `into` for a `scope` local's own borrows.** A scope is a borrowed local
  (§18), and `thread.spawn` stores into a scope, so no library thread group is needed.
