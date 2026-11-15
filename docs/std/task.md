# `std.task`: tasks, scopes and OS threads

```ingot
import std.sync.Cell;
import std.sys.rt;
```

`scope s { ... }` is the only way to make a `Scope`; `spawn` is the only way to make a
`Task`. Both are compiler-known by path (pipeline §8), so the fields below are private.

## 1. `Scope`

```ingot
/// The view of a `scope s { ... }` block. The compiler allocates the state; a program
/// cannot construct one.
pub struct Scope {
    state: Cell[rt.ScopeState],
}

impl Scope {
    /// Sets the cancel flag for this scope and for every scope opened inside it.
    pub fn cancel(self: &Scope) -> void

    /// True while the scope is canceled. In a `select` arm the same call waits until
    /// it becomes true (section 21); outside one it only reads the flag.
    pub waitable fn canceled(self: &Scope) -> bool
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
    /// Waits for the task, moves its result out and releases the task record.
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
private to `std.task` and the compiler is allowed to use them.

## 3. Functions in the module

```ingot
/// Whether the current task's scope is canceled. For CPU-bound code that cannot block.
pub fn canceled() -> bool

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
    R: Send,
    E: Send,
    A: FnOnce() -> R ! E + Send,
    B: FnOnce() -> R ! E + Send,
](
    a: A,
    b: B,
) -> R ! E
```

`canceled()` is the module-level poll named in section 22. `Scope.canceled()` is the
waitable form used in `select`. They are deliberately two spellings: the first has no
receiver and cannot be an arm, the second can.

`first_of` takes exactly two closures because two closures of different anonymous
types cannot be stored in one slice. Its own nested scope owns the arguments and waits
before it returns, so its callers do not keep their borrows until the caller's scope
ends. A three-way form (`first_of3`) is a later addition if the human wants it.

## 4. `std.thread`

Dedicated OS threads for foreign state that must stay on one thread. The scope is a
library function, not the language's `scope` statement, because `scope` is a keyword
and `std.thread.scope` would not parse.

```ingot
/// A structured group of OS threads. Linear: `group` owns it and waits before return.
pub linear struct Group { ... }

impl Group {
    /// Starts `f` on a dedicated OS thread. The thread is waited for by `group`.
    pub fn spawn[F: FnOnce() + Send](self: &mut Group, f: F into self) -> void

    /// Waits for every thread in the group. Called by `group` before it returns.
    pub fn join_all(self: &mut Group) -> void

    /// Waits for every thread and releases the group. `group` calls this.
    pub fn deinit(self: Group) -> void
}

/// Creates a group, calls `body`, waits for every thread, and returns `body`'s result.
pub fn group[T, F: FnOnce(&mut Group) -> T](body: F) -> T

/// The OS thread that is running the current task.
pub fn id() -> ThreadId

pub copy struct ThreadId {
    id: u64,
}
```

Properties:

| Type | `Copy` | `Discard` | linear | `Owned` | `Send` | `Sync` |
|---|---|---|---|---|---|---|
| `Group` | no | no | yes | yes | no | no |
| `ThreadId` | yes | yes | no | yes | yes | yes |

`Group` is not `Send` or `Sync`: it is created and consumed by one call to `group` and
is never passed to a task. `ThreadId` is a plain value; it is `Copy`, `Owned`, `Send`
and `Sync`.

A thread's closure runs on an OS thread, so it must be `Send`. `into self` makes the
closure's borrows part of the `Group`'s origin set; the borrow checker therefore keeps
them live until `group` has waited. This is the same rule as `spawn` into a `Scope`,
but here the store is through `&mut Group` rather than a compiler-known `&Scope`.

## Decisions for the human

All five are decided. The recommendation was accepted in each case.

- **D1. `first_of` arity.** **Decided: accepted.** Exactly two typed closures.
- **D2. `first_of` error choice.** **Decided: accepted.** If both fail, return the first
  failure that finishes.
- **D3. `Task` `Send`.** **Decided: accepted.** `Task[R]` is `Send` when `R: Send`.
- **D4. `ThreadId` contents.** **Decided: accepted.** The OS thread id as `u64`.
- **D5. `Group` spelling.** **Decided: accepted.** `thread.group`.

## Language questions

- **L1. `waitable fn` lowering.** Same gap as `rt.md` L1. `Task.wait` and
  `Scope.canceled` are only primitives here; the compiler still has no rule that turns
  a waitable call into a registrable operation without running it.
- **L2. `Scope.canceled` has two meanings.** Section 21 lists it as a waitable
  function, while section 22 gives the poll `task.canceled()` as the CPU-bound check.
  The spec never says that one call can both return a boolean and wait for it to become
  true. A separate `wait_canceled` would be unambiguous.
- **L3. No `into` for a `scope` local's own borrows.** `Scope` is the one built-in
  exception to §13.6, and the compiler creates its `Self(s)` origins. A library
  `Group` can only approximate this with `&mut` and `into`; a user-defined structured
  thread type cannot be written with the same guarantees.
