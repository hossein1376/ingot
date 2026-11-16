# `std.sync`: atomics, locks, channels and waits

```ingot
import std.mem.AllocErr;
import std.mem.Allocator;
import std.sys.rt;
```

The types in this module have interior mutability, so their contents are `Owned`
(section 13.6). `Cell` is the language item that defines interior mutability
(02-memory §13.6, pipeline §8). Updated for the language designer's second review
(spec `720763f`): `Cell` is public with a safe `get`, atomic orders are typed, and
`Atomic[T]` is `Send` and `Sync` only when `T: Send`.

## 1. The interior-mutability cell

```ingot
/// Compiler-known: a type that contains a `Cell` has interior mutability. Public: a
/// program may build its own interior-mutable type on it.
pub struct Cell[T: Owned] {
    value: T,
}

impl[T: Owned] Cell[T] {
    pub fn init(value: T) -> Cell[T]

    /// The only write path. Safe, like every way of making a raw pointer; each use of
    /// the pointer is `unsafe`, and its block must prove exclusive access by the
    /// protocol of the enclosing type (`Mutex`, `Once`, `Atomic`, or an arena).
    pub fn get(self: &Cell[T]) -> *mut T

    pub fn into_inner(self: Cell[T]) -> T
}
```

`Cell` is `Send` if `T: Send`, and never `Sync`. It is `Owned` when `T: Owned`. The
compiler reads the path `std.sync.Cell` to decide that an outer type has interior
mutability; without that marker, an `&T` to a lock or a channel could be marked
read-only by the optimizer. Writing through the pointer `get` gives is the only way to
mutate a `Cell`'s contents through a shared borrow, and it is legal only inside
`unsafe` (02-memory §15).

## 2. `Atomic[T]`

```ingot
/// The orders of each operation, as three types so that an order the operation does
/// not allow cannot be written (02-memory §15).
pub copy enum LoadOrder: u8 { relaxed, acquire, seq_cst }
pub copy enum StoreOrder: u8 { relaxed, release, seq_cst }
pub copy enum AtomicOrder: u8 { relaxed, acquire, release, acq_rel, seq_cst }

/// A value that may be read and written through `&self` by a single instruction.
pub struct Atomic[T: AtomicValue] {
    value: Cell[T],
}

impl[T: AtomicValue] Atomic[T] {
    pub fn init(value: T) -> Atomic[T]

    pub inline fn load(self: &Atomic[T], order: LoadOrder) -> T
    pub inline fn store(self: &Atomic[T], value: T, order: StoreOrder) -> void
    pub inline fn swap(self: &Atomic[T], value: T, order: AtomicOrder) -> T

    /// Success gives the previous value; failure also gives the observed value, so the
    /// caller can retry without a second load.
    pub inline fn compare_exchange(
        self: &Atomic[T],
        expected: T,
        new: T,
        success: AtomicOrder,
        failure: LoadOrder,
    ) -> T ! T

    pub inline fn compare_exchange_weak(
        self: &Atomic[T],
        expected: T,
        new: T,
        success: AtomicOrder,
        failure: LoadOrder,
    ) -> T ! T

    pub fn get_mut(self: &mut Atomic[T]) -> &mut T from self
    pub fn into_inner(self: Atomic[T]) -> T
}

impl[T: AtomicValue + Int] Atomic[T] {
    /// Wrapping addition, as the hardware does. Gives the previous value.
    pub inline fn add(self: &Atomic[T], delta: T, order: AtomicOrder) -> T
    pub inline fn sub(self: &Atomic[T], delta: T, order: AtomicOrder) -> T
    pub inline fn and(self: &Atomic[T], mask: T, order: AtomicOrder) -> T
    pub inline fn or(self: &Atomic[T], mask: T, order: AtomicOrder) -> T
    pub inline fn xor(self: &Atomic[T], mask: T, order: AtomicOrder) -> T
}
```

`Atomic` is instantiable only for the `AtomicValue` types: integers, `bool`, raw
pointers to `Owned` types and payload-free `copy enum`s (section 21; `01-core` §8). `AtomicValue` implies
`Copy`, `Discard`, `Owned` and `Eq`, but not `Send` or `Sync`; a raw pointer is neither.
The `add`, `sub` and bitwise methods do not exist for the other allowed `T`, which is why
they also require `Int`. All operations map to one instruction and carry the memory
order they are given.

Each method is `inline` and passes its order straight to the builtin. The orders are
typed, so `x.load(.release)` is an ordinary "no such variant" error, and the builtins
take any value of the order type: a constant folds to one instruction, and an order that
is still a variable after inlining becomes a branch over its values in the back end
(pipeline §7). There is no clamping and no rule that the failure order of
`compare_exchange` be weaker than the success order.

Properties: `Atomic[T]` is not `Copy`, is `Discard`, is not linear, is `Owned`, and is
`Send` and `Sync` **only when `T: Send`** (03-concurrency §20): `unsafe impl[T:
AtomicValue + Send] Send for Atomic[T] {}` and the same for `Sync`. Every integer,
`bool` and payload-free enum is `Send`, so only raw pointers are affected: a type built
on `Atomic[*mut T]` (a lock-free stack) gets `Send` and `Sync` only through its own
`unsafe impl`, which `ingot audit` lists.

## 3. `Mutex[T]` and its guard

```ingot
pub struct Mutex[T: Owned] {
    cell: Cell[T],
    state: Atomic[u8],
}

impl[T: Owned] Mutex[T] {
    pub fn init(value: T) -> Mutex[T]
    pub fn lock(self: &Mutex[T]) -> Guard[T] from self
    pub fn try_lock(self: &Mutex[T]) -> ?Guard[T] from self
    pub fn get_mut(self: &mut Mutex[T]) -> &mut T from self
    pub fn into_inner(self: Mutex[T]) -> T
}

impl[T: Discard + Owned] Mutex[T] {
    pub fn deinit(self: Mutex[T]) -> void
}

/// A linear view of the lock. `unlock` is the only way to release it.
pub linear struct Guard[T: Owned] {
    mutex: &Mutex[T],
    value: &mut T,
    not_send: [0]*u8,      // the language cannot say "not `Send`"; a raw pointer can
}

impl[T: Owned] Guard[T] {
    pub fn get(self: &mut Guard[T]) -> &mut T from self
    pub fn unlock(self: Guard[T]) -> void
}
```

`lock` does not observe cancellation (section 22). `try_lock` returns `.none` when the
lock is held. A guard is a view of its mutex; moving or dropping the mutex while a
guard is live is already an exclusivity error. Guards are **not `Send`**: a guard cannot
be a `spawn` argument or a `par for` item. A task that holds a guard across a park and
resumes on another worker is not sending it anywhere, so no task identity is needed for
this. The language has no way to say "not `Send`": `&Mutex[T]` and `&mut T` are both
`Send` when `T: Send`, so the guard holds a zero-sized `[0]*u8` field to lose the
property, and `unsafe impl[T: Owned + Send + Sync] Sync for Guard[T]` adds back what the
table says.

## 4. `RwLock[T]`

```ingot
pub struct RwLock[T: Owned] {
    cell: Cell[T],
    state: Atomic[u32],
}

impl[T: Owned] RwLock[T] {
    pub fn init(value: T) -> RwLock[T]
    pub fn read(self: &RwLock[T]) -> ReadGuard[T] from self
    pub fn try_read(self: &RwLock[T]) -> ?ReadGuard[T] from self
    pub fn write(self: &RwLock[T]) -> WriteGuard[T] from self
    pub fn try_write(self: &RwLock[T]) -> ?WriteGuard[T] from self
    pub fn get_mut(self: &mut RwLock[T]) -> &mut T from self
    pub fn into_inner(self: RwLock[T]) -> T
}

impl[T: Discard + Owned] RwLock[T] {
    pub fn deinit(self: RwLock[T]) -> void
}

pub linear struct ReadGuard[T: Owned] {
    lock: &RwLock[T],
    value: &T,
}

impl[T: Owned] ReadGuard[T] {
    pub fn get(self: &ReadGuard[T]) -> &T from self
    pub fn unlock(self: ReadGuard[T]) -> void
}

pub linear struct WriteGuard[T: Owned] {
    lock: &RwLock[T],
    value: &mut T,
}

impl[T: Owned] WriteGuard[T] {
    pub fn get(self: &mut WriteGuard[T]) -> &mut T from self
    pub fn unlock(self: WriteGuard[T]) -> void
}
```

Readers share `&T`, so `RwLock[T]` is `Sync` only when `T: Send + Sync` (section 20).
Like `Guard`, the read and write guards are **not `Send`** (each has a `[0]*u8` field);
they are `Sync` when `T: Send + Sync`, by `unsafe impl`, because each holds `&RwLock[T]`
as well as `&T` or `&mut T`.

## 5. `Once[T]`

```ingot
pub struct Once[T: Owned] {
    cell: Cell[?T],
    state: Atomic[u8],
}

impl[T: Owned] Once[T] {
    /// The unset value. It is a constant expression, so a `static` can hold it.
    pub fn uninit() -> Once[T]

    /// Runs `f` at most once. Concurrent callers wait for the winner.
    /// `f` is `Discard`: it is dropped uncalled when another task initialized first.
    pub fn get_or_init[F: FnOnce() -> T + Discard](self: &Once[T], f: F) -> &T from self

    /// `.none` until initialized.
    pub fn get(self: &Once[T]) -> ?&T from self
    pub fn is_set(self: &Once[T]) -> bool
}

impl[T: Discard + Owned] Once[T] {
    pub fn deinit(self: Once[T]) -> void
}

impl[T: Owned] Once[T] {
    pub fn deinit_with[F: FnMut(T)](self: Once[T], f: F) -> void
}
```

`Once` has interior mutability and holds `?T`; `T` must be `Owned`. A `static` of
`Once` is `Sync` only when `T: Send + Sync`, because `get` hands out `&T` to every task
that reads it. `get_or_init` may run `f` on any task; `F` is not required to be `Send`
because the closure is called on the task that wins the initialization, but it must not
borrow data that is not live there. This is why the bound is `FnOnce`, not `Send`.

## 6. `Chan[T, A]`

```ingot
/// A bounded multi-producer, multi-consumer queue. Linear: it owns its buffer.
pub linear struct Chan[T: Owned, A: Allocator] { ... }

pub copy enum ChanErr: u8 { alloc: from AllocErr, bad_capacity }

pub enum SendErr[T] {
    closed: T,
    canceled: T,
}

pub copy enum RecvErr: u8 { closed, canceled }

pub enum TrySendErr[T] {
    full: T,
    closed: T,
}

pub copy enum TryRecvErr: u8 { closed }

impl[T: Owned, A: Allocator] Chan[T, A] {
    /// `capacity` must be at least 1. Allocation failure is an error, never a trap.
    pub fn init(a: A, capacity: i64) -> Chan[T, A] ! ChanErr

    /// Blocks while full. Gives the value back if the channel is closed or the scope is
    /// canceled.
    pub fn send(self: &Chan[T, A], value: T) -> void ! SendErr[T]

    /// Blocks while empty. A closed, drained channel fails with `.closed`; a canceled
    /// scope fails with `.canceled`.
    pub waitable fn recv(self: &Chan[T, A]) -> T ! RecvErr

    pub fn try_send(self: &Chan[T, A], value: T) -> void ! TrySendErr[T]

    /// `.none` means empty. A closed, drained channel fails with `.closed`.
    pub fn try_recv(self: &Chan[T, A]) -> ?T ! TryRecvErr

    pub fn close(self: &Chan[T, A]) -> void
    pub fn is_closed(self: &Chan[T, A]) -> bool
    pub fn len(self: &Chan[T, A]) -> i64
    pub fn capacity(self: &Chan[T, A]) -> i64
}

impl[T: Discard + Owned, A: Allocator] Chan[T, A] {
    pub fn deinit(self: Chan[T, A]) -> void
}

impl[T: Owned, A: Allocator] Chan[T, A] {
    /// Consumes every queued value with `f`, then frees the buffer. For linear `T`. A
    /// closure, not a `fn` pointer, so the cleanup can capture what it needs.
    pub fn deinit_with[F: FnMut(T)](self: Chan[T, A], f: F) -> void
}
```

`Chan` keeps its queued values behind a raw pointer, not in a `Cell`, so it bounds them
with `Owned` itself (02-memory §15). `Chan` is `Send` when `T: Send` and `A: Send`;
`Sync` when `T: Send` and `A: Sync`.
`A` is part of the type because the buffer must be freed by the allocator that made it
(section 14.3). `close` only stops new values and wakes waiters; it does not discard
queued values. `send` and `recv` are the cancellation-aware operations; `try_send` and
`try_recv` never observe cancellation.

## 7. `WaitGroup`

```ingot
/// A counter that a task waits on. No allocation and no linear resource.
pub struct WaitGroup {
    count: Atomic[i64],
}

impl WaitGroup {
    pub fn init() -> WaitGroup

    /// Adds `n` to the count. A negative `n` is `.negative`, and the count is unchanged.
    pub fn add(self: &WaitGroup, n: i64) -> void ! WaitGroupErr

    /// Decrements the count and wakes the waiter at zero.
    pub fn done(self: &WaitGroup) -> void

    /// Blocks until the count is zero. Does not observe cancellation.
    pub fn wait(self: &WaitGroup) -> void
}
```

```ingot
pub copy enum WaitGroupErr: u8 { negative }
```

`WaitGroup` is `Send`, `Sync`, `Owned` and `Discard`; it is not `Copy`. A negative `n`
in `add` is an error the caller sees, not a trap and not a silent no-op (review 2,
S5-7: an error is visible, a no-op hides the bug).

## 8. Properties

| Type | `Copy` | `Discard` | linear | `Owned` | `Send` | `Sync` |
|---|---|---|---|---|---|---|
| `Cell[T]` | no | if `T: Discard` | if `T` is | if `T: Owned` | if `T: Send` | no |
| `Atomic[T]` | no | yes | no | yes | if `T: Send` | if `T: Send` |
| `Mutex[T]` | no | if `T: Discard` | if `T` is | if `T: Owned` | if `T: Send` | if `T: Send` |
| `Guard[T]` | no | no | yes | no | no | if `T: Send + Sync` |
| `RwLock[T]` | no | if `T: Discard` | if `T` is | if `T: Owned` | if `T: Send` | if `T: Send + Sync` |
| `ReadGuard[T]` | no | no | yes | no | no | if `T: Send + Sync` |
| `WriteGuard[T]` | no | no | yes | no | no | if `T: Send + Sync` |
| `Once[T]` | no | if `T: Discard` | if `T` is | if `T: Owned` | if `T: Send` | if `T: Send + Sync` |
| `Chan[T, A]` | no | no | yes | if `T`, `A: Owned` | if `T`, `A: Send` | if `T: Send`, `A: Sync` |
| `WaitGroup` | no | yes | no | yes | yes | yes |
| `LoadOrder`, `StoreOrder`, `AtomicOrder`, `RecvErr`, `TryRecvErr`, `ChanErr`, `WaitGroupErr` | yes | yes | no | yes | yes | yes |

The `Cell`, `Atomic`, guard, lock, `Once` and `Chan` rows need `unsafe impl`s; the
fields include `Cell`, raw pointers and atomics. No row is `Sync` unless every operation
that reads the contents synchronizes or is a read-only `&T`.

## 9. What is `unsafe` inside

- Every use of the pointer `Cell.get` gives, the `Atomic` operations, the lock state
  machines, the channel queue and its wakeups, and `WaitGroup`'s counter are `unsafe`
  blocks with a reason.
- The pointer from `Cell.get` must never be written through by two tasks at once for a
  `T` that is not atomic; `Mutex`, `RwLock`, `Once` and `Atomic` are the types that
  establish the protocol.
- A waitable operation (`Chan.recv`) is a `std.task.Wait` value: its `poll` registers on
  a `WaitQueue` through `WaitCx` and consumes a value only when it returns `.some`
  (03-concurrency §21, `rt.md` §3). A `Chan` keeps its queues in its own state.

## Decisions for the human

- **D1. Cell.** **Decided: accepted, then changed by review 2.** The compiler-known
  `std.sync.Cell[T: Owned]` is the interior-mutability marker; it is public, and `get`
  is a safe function giving `*mut T`.
- **D2. Atomic-able bound.** **Decided: accepted.** `Atomic[T: AtomicValue]`; the new
  spec binds exactly the allowed types.
- **D3. `compare_exchange` result.** **Decided: accepted.** `T ! T`: the previous value
  on success, the observed value on failure.
- **D4. Guard crossing tasks.** **Decided: changed.** Guards are not `Send`; they
  cannot be `spawn` arguments or `par for` items. A task that parks and resumes on
  another worker is not sending its guard. `Sync` follows the table.
- **D5. `Chan` errors.** **Decided: accepted.** Four error enums, split by operation and
  cancellation behavior.
- **D6. `Once` spelling.** **Decided: accepted.** `Once[T].uninit()` and `get_or_init`.
- **D7. Atomic orders.** **Decided (review 2, user): typed.** `LoadOrder`, `StoreOrder`
  and `AtomicOrder` replace run-time clamping of one `AtomicOrder`.
- **D8. `WaitGroup.add` with a negative `n`.** **Decided (review 2, user): an error**,
  `WaitGroupErr.negative`, replacing the no-op.

## Language questions

### Settled by the spec (`f3e7d2a`, `720763f`)

- **L1. The interior-mutability primitive.** Settled: `02-memory.md` §13.6 now makes
  `std.sync.Cell[T: Owned]` the one language item and defines "contains a `Cell`".
  `Atomic`, `Mutex`, `RwLock`, `Once`, `Chan` and `Arena` contain one.
- **L2. Atomic operations have no builtins.** Settled: `02-memory.md` §15 now defines
  `@atomic_load`, `@atomic_store`, `@atomic_swap`, `@atomic_cmpxchg`,
  `@atomic_cmpxchg_weak`, the five integer read-modify-write builtins, and the safe
  `@fence`. Their order arguments are typed: `LoadOrder`, `StoreOrder`, `AtomicOrder`
  (`720763f`).
- **L3. No atomic-able bound.** Settled: `01-core.md` §8 defines `AtomicValue` and
  E0423 rejects any other type.

- **L4. `waitable fn` and `Chan.recv`.** Settled by `720763f` (03-concurrency §21):
  `recv` prepares a `Wait` operation whose `poll` consumes only when it completes, so an
  unchosen arm consumes nothing.
