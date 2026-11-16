# `std.mem`: allocators, arenas, owning pointers and bulk operations

```ingot
import std.sync.Cell;
```

`std.mem` is the only module that defines allocation. A call whose error type is
`AllocErr`, or converts to it by `from`, is what `ingot cost` reports as `alloc`, with
the callee's `Allocator` argument as its allocator (05-tooling §31, pipeline §8).
Updated for the language designer's second review: the `_or_return` convention,
`Arc.release_last`, `FnMut` cleanups, an 8-byte `Handle`, and zero-size requests. Every allocating function either
takes an allocator or is a method of a container that stores one.

## 1. Errors

```ingot
/// Allocation failed, or the request was invalid. A negative size or an alignment
/// that is not a power of two is reported as `.bad_request`, never as a trap.
pub copy enum AllocErr: u8 {
    out_of_memory,
    bad_request,
}

/// The error of the `_or_return` forms: the value an allocating function had already
/// received when allocation failed, given back.
pub struct AllocFail[T] {
    pub error: AllocErr,
    pub value: T,
}
```

Every allocating function that takes a `T` by value comes in two forms (05-tooling
§35). The ordinary one (`Box.init`, `Arc.init`, `Pool.insert`, `Arena.alloc`,
`FixedBuf.alloc`, and a container's `push`, `insert` and `reserve`) requires
`T: Discard`, returns `! AllocErr`, and drops the value on failure, so `try` converts
its error into any enum with a `from AllocErr` variant. The second, named
`_or_return` (`init_or_return`, `insert_or_return`, `alloc_or_return`,
`push_or_return`), takes any `T` and returns `! AllocFail[T]`: a caller with a linear
value must decide what happens to it on failure anyway, so the explicit `catch` is the
honest form there. `AllocFail` is a library type, never the error of the default name.
The allocator interface itself returns `AllocErr`, because it has no value to give back.
`AllocFail` is linear when `T` is, `Discard` when `T` is, `Owned` when `T` is, and
`Send` when `T` is.

## 2. `Allocator`

```ingot
pub interface Allocator: Copy {
    fn alloc(self: Self, size: i64, align: i64) -> *mut u8 ! AllocErr;

    /// Releases a block from `alloc` with exactly the same `size` and `align`.
    unsafe fn free(self: Self, ptr: *mut u8, size: i64, align: i64);

    /// Tries to resize in place. `true` means the block now has `new` bytes at the same
    /// address; `false` means it was not changed and the caller must allocate and copy.
    unsafe fn resize(
        self: Self,
        ptr: *mut u8,
        old: i64,
        new: i64,
        align: i64,
    ) -> bool;
}
```

The trait is `Copy` because an allocator value is a handle, not an owner. A valid
request has `size >= 0`, `align` a power of two, and both at most `2^62`; an invalid
request is `AllocErr.bad_request`. A request of size 0 gives a non-null pointer aligned
to `align` that is never dereferenced, and freeing it does nothing, so containers need
no special case for zero-sized elements. The caller of `free` and `resize` guarantees that
`ptr` came from this allocator with that `size` and `align`; that is the `unsafe`
precondition the implementation documents. `resize` does not report why it refused; a
container that needs a reason allocates and copies instead.

## 3. `Heap` and `Vm`

```ingot
/// The general allocator. Zero-sized; obtained from `env.heap`.
pub copy struct Heap {}

impl Allocator for Heap { ... }

/// Address-space reservation, obtained from `env.vm`. Zero-sized and `Copy`.
pub copy struct Vm {}

impl Vm {
    /// Reserves `size` bytes of address space, aligned to a page. No pages are
    /// committed. `size` is rounded up to a page; a negative or too-large size is
    /// `.bad_request`.
    pub fn reserve(self: Vm, size: i64) -> *mut u8 ! AllocErr

    /// Releases a reservation from `reserve`. The whole range must still be reserved.
    pub unsafe fn release(self: Vm, base: *mut u8, size: i64) -> void

    /// Commits `size` bytes inside a reservation. `false` means the OS refused.
    pub unsafe fn commit(self: Vm, base: *mut u8, size: i64) -> bool

    /// Returns committed pages to the OS; the address space stays reserved.
    pub unsafe fn decommit(self: Vm, base: *mut u8, size: i64) -> void

    pub fn page_size(self: Vm) -> i64
}
```

`Heap` is `Copy`, `Send`, `Sync`, `Owned` and `Discard`. `Vm` is the same. `Vm` is not
an `Allocator`: it reserves address space and does not hand out typed storage.

## 4. `Arena`

```ingot
/// A bump allocator over a reserved address range. Linear: `deinit` releases the
/// reservation. `Send`, not `Sync`: one task allocates from it at a time.
pub linear struct Arena { ... }

impl Arena {
    /// Reserves and starts committing from `vm`. `bytes` is the reservation size; a
    /// negative or too-large size is `.bad_request`.
    pub fn init(vm: Vm, bytes: i64) -> Arena ! AllocErr

    /// A new `T` in arena memory. The result is an ordinary borrow of the arena.
    pub fn alloc[T: Discard](self: &Arena, value: T) -> &mut T ! AllocErr from self
    pub fn alloc_or_return[T](self: &Arena, value: T) -> &mut T ! AllocFail[T] from self

    /// `n` copies of `fill`. For non-`Copy` types use `alloc` in a loop or an unsafe
    /// uninitialized-slice function in a later revision.
    pub fn alloc_slice[T: Copy](
        self: &Arena,
        n: i64,
        fill: T,
    ) -> []mut T ! AllocErr from self

    /// Invalidates every borrow of the arena. Rejects while any is live by the ordinary
    /// borrow rules.
    pub fn reset(self: &mut Arena) -> void

    /// Consumes the arena and releases the whole reservation.
    pub fn deinit(self: Arena) -> void

    pub fn used(self: &Arena) -> i64
    pub fn capacity(self: &Arena) -> i64
}

impl Allocator for &Arena { ... }
```

`Arena` is `linear`, `Send`, `Owned`, not `Sync`, not `Copy`, not `Discard`. The
allocation entry point is `&Arena`, so several shared borrows may allocate; the bump
pointer is interior mutable. The arena contains its bump pointer in a `Cell` (02-memory
§13.6), so `&Arena` is not marked read-only by the optimizer. The `&mut Arena` of
`reset` and `deinit` is what ends the borrows. `impl Allocator for &Arena` is the form
that makes `Arena` usable where an allocator is expected; its `free` does nothing and
its `resize` always returns `false`.

## 5. `FixedBuf`

```ingot
/// A bump allocator inside a caller-provided buffer, usually on the stack. A view of
/// that buffer; it does not own it. `Send`, not `Sync`.
pub struct FixedBuf { ... }

impl FixedBuf {
    pub fn init(buf: []mut u8) -> FixedBuf from buf

    pub fn alloc[T: Discard](self: &FixedBuf, value: T) -> &mut T ! AllocErr from self
    pub fn alloc_or_return[T](self: &FixedBuf, value: T) -> &mut T ! AllocFail[T] from self

    pub fn alloc_slice[T: Copy](
        self: &FixedBuf,
        n: i64,
        fill: T,
    ) -> []mut T ! AllocErr from self

    pub fn reset(self: &mut FixedBuf) -> void
    pub fn used(self: &FixedBuf) -> i64
    pub fn capacity(self: &FixedBuf) -> i64
}

impl Allocator for &FixedBuf { ... }
```

`FixedBuf` is a view (it holds `[]mut u8`), `Discard`, `Send`, not `Sync`. It cannot
outlive the buffer. Its offset lives in a `Cell`, so `alloc(&FixedBuf)` is sound and
`&FixedBuf` is not marked read-only. `free` is a no-op and `resize` returns `false`.

## 6. `Box[T, A]`

```ingot
/// One value in allocator memory. Linear: `deinit` or `into_inner` is required.
pub linear struct Box[T, A: Allocator] { ... }

impl[T: Discard, A: Allocator] Box[T, A] {
    /// On failure the value is dropped.
    pub fn init(a: A, value: T) -> Box[T, A] ! AllocErr
}

impl[T, A: Allocator] Box[T, A] {
    /// On failure the value is returned in `AllocFail`.
    pub fn init_or_return(a: A, value: T) -> Box[T, A] ! AllocFail[T]

    pub fn get(self: &Box[T, A]) -> &T from self
    pub fn get_mut(self: &mut Box[T, A]) -> &mut T from self

    /// Frees the allocation and returns the value. The caller then owns `T` directly.
    pub fn into_inner(self: Box[T, A]) -> T
}

impl[T: Discard, A: Allocator] Box[T, A] {
    pub fn deinit(self: Box[T, A]) -> void
}
```

`Box` is `linear` even when `T` is `Copy`, because it owns the allocation. It is
`Owned` when `T` and `A` are, and a view when `A` is a view (for example `&Arena`).
`Send` if `T` and `A` are `Send`; `Sync` if both are `Sync`.

## 7. `Arc[T, A]`

```ingot
/// Shared ownership. Linear: every owner is released explicitly.
pub linear struct Arc[T, A: Allocator] { ... }

impl[T: Owned + Discard, A: Allocator] Arc[T, A] {
    /// On failure the value is dropped.
    pub fn init(a: A, value: T) -> Arc[T, A] ! AllocErr
}

impl[T: Owned, A: Allocator] Arc[T, A] {
    /// On failure the value is returned in `AllocFail`.
    pub fn init_or_return(a: A, value: T) -> Arc[T, A] ! AllocFail[T]

    /// Adds one owner. The refcount is atomic.
    pub fn clone(self: &Arc[T, A]) -> Arc[T, A]

    pub fn get(self: &Arc[T, A]) -> &T from self
}

impl[T: Owned + Discard, A: Allocator] Arc[T, A] {
    /// Gives up one owner. The last owner drops `T` and frees.
    pub fn release(self: Arc[T, A]) -> void
}

impl[T: Owned, A: Allocator] Arc[T, A] {
    /// Gives up one owner. The last owner gets the value and the allocation is freed;
    /// every other owner gets `.none`. For a linear `T`: the value comes back instead
    /// of going to a callback, so its cleanup can use anything the caller has.
    pub fn release_last(self: Arc[T, A]) -> ?T
}
```

`Arc` is `Send` and `Sync` when `T: Send + Sync` and `A` is too. It is `Owned` when
`T` and `A` are. Mutating the contents requires a `Mutex` or an atomic inside, because
`get` only gives `&T`.

## 8. `Pool[T, A]` and `Handle[T]`

```ingot
/// A generation-checked index, 8 bytes. Copy and self-contained: it holds no `T`, so
/// it is not a view and does not borrow the pool.
pub copy struct Handle[T] {
    index: u32,
    generation: u32,
}

/// Slot storage with generations. Linear: it owns the slots and the allocator memory.
pub linear struct Pool[T, A: Allocator] { ... }

impl[T, A: Allocator] Pool[T, A] {
    pub fn init(a: A) -> Pool[T, A] ! AllocErr

    /// On failure the value is returned in `AllocFail`.
    pub fn insert_or_return(self: &mut Pool[T, A], value: T) -> Handle[T] ! AllocFail[T]

    /// A stale handle gives `.none`; it never returns another slot's value.
    pub fn get(self: &Pool[T, A], h: Handle[T]) -> ?&T from self
    pub fn get_mut(self: &mut Pool[T, A], h: Handle[T]) -> ?&mut T from self

    /// Removes the value, if the handle is live, and returns it.
    pub fn remove(self: &mut Pool[T, A], h: Handle[T]) -> ?T
}

impl[T: Discard, A: Allocator] Pool[T, A] {
    /// On failure the value is dropped.
    pub fn insert(self: &mut Pool[T, A], value: T) -> Handle[T] ! AllocErr
    pub fn deinit(self: Pool[T, A]) -> void
}

impl[T, A: Allocator] Pool[T, A] {
    /// A closure, not a `fn` pointer, so the cleanup can capture what it needs.
    pub fn deinit_with[F: FnMut(T)](self: Pool[T, A], f: F) -> void
}
```

`Handle[T]` is `Copy`, `Discard`, `Owned`, `Send`, `Sync`, and 8 bytes: handles replace
pointers in graph code and are stored everywhere. A pool has at most 2^32 slots, and a
slot whose generation counter is used up (2^32 reuses) is retired, never reused, so a
stale handle can never match. The type parameter is not a field, so a handle stays
self-contained (section 14.4). `Pool` is `linear`; `Send` if
`T` and `A` are; `Sync` if `T` is `Sync` and `A` is `Sync`; `Owned` if both are.

## 9. Bulk operations

```ingot
/// Copies `src.len` elements. `dst.len` must be at least `src.len`; a short destination
/// is a caller bug and traps (T02).
pub fn copy[T: Copy](dst: []mut T, src: []T) -> void

/// Fills every element with `value`.
pub fn fill[T: Copy](dst: []mut T, value: T) -> void

/// Element-wise equality. Different lengths are `false`, not an error.
pub fn eq[T: Eq](a: []T, b: []T) -> bool
```

`copy` and `fill` use the vector moves where the types allow it; their signatures are
the portable ones. `copy` does not overlap: the borrow checker already rejects an
exclusive destination slice and a shared source slice of the same place.

## 10. Properties

| Type | `Copy` | `Discard` | linear | `Owned` | `Send` | `Sync` |
|---|---|---|---|---|---|---|
| `AllocErr` | yes | yes | no | yes | yes | yes |
| `AllocFail[T]` | no | if `T: Discard` | if `T` is | if `T: Owned` | if `T: Send` | if `T: Sync` |
| `Heap`, `Vm` | yes | yes | no | yes | yes | yes |
| `Arena` | no | no | yes | yes | yes | no |
| `FixedBuf` | no | yes | no | no | yes | no |
| `Box[T, A]` | no | no | yes | if `T`, `A: Owned` | if `T`, `A: Send` | if `T`, `A: Sync` |
| `Arc[T, A]` | no | no | yes | if `T`, `A: Owned` | if `T: Send + Sync`, `A: Send + Sync` | if `T: Send + Sync`, `A: Send + Sync` |
| `Handle[T]` | yes | yes | no | yes | yes | yes |
| `Pool[T, A]` | no | no | yes | if `T`, `A: Owned` | if `T`, `A: Send` | if `T`, `A: Sync` |

`Arena` and `FixedBuf` are `Send` and not `Sync`; `&Arena` and `&FixedBuf` are neither.
`Arc`'s contents are `Owned`: shared ownership of a borrow cannot outlive the borrow.

## 11. What is `unsafe` inside

- `Heap.alloc/free/resize` call the OS memory layer.
- `Vm.reserve/release/commit/decommit` call `mmap`/`VirtualAlloc` and friends.
- `Arena` and `FixedBuf` advance a bump pointer through `Cell` and create
  `&mut T`/`[]mut T` from raw memory with `@borrow_from_raw` and `@slice_from_raw`.
  Raw-pointer casts and `@offset` are safe now (01-core §5.4, 02-memory §15); the
  dereference and the borrow construction are not. The arena's allocation protocol is
  the reason they are sound: memory is never reused while a borrow is live because
  `reset`/`deinit` take `&mut self`.
- `Box`, `Arc` and `Pool` create references from their allocation with
  `@borrow_from_raw`; `Handle` access checks the generation before doing so. They use
  `as` for `*mut u8` to `*mut T` and `@offset` for field and slot addresses.

## Decisions for the human

All six are decided. The recommendation was accepted in each case.

- **D1. `AllocErr` variants.** **Decided: changed.** `out_of_memory` for a failed
  request and `bad_request` for an invalid size or alignment; neither traps. The
  original recommendation had only `out_of_memory`; the human kept `bad_request` so
  that std never traps on bad input (§35).
- **D2. Allocation failure with a linear value.** **Decided: accepted, changed by
  review 2.** The ordinary function requires `T: Discard` and returns `AllocErr`, so
  `try` works; the `_or_return` form returns `AllocFail[T]` for any `T`.
- **D3. `copy` on a short destination.** **Decided: accepted.** It traps (T02); a short
  destination is a caller bug.
- **D4. `Arc` contents.** **Decided: accepted.** `T: Owned`.
- **D5. `Arc` and linear contents.** **Decided: changed by review 2.** `release` for
  `T: Discard`; `release_last() -> ?T` for linear `T` (a `fn` pointer could not
  capture what a cleanup needs).
- **D6. `Handle` size.** **Decided (review 2, user): 8 bytes**, `index: u32` and
  `generation: u32`; a slot whose generation is used up is retired.
- **D7. Zero-size requests.** **Decided (review 2).** Size 0 is valid and gives a
  non-null aligned pointer that is never dereferenced.

## Language questions

### Settled by the spec (`f3e7d2a`, `720763f`)

- **L1. Pointer casts and pointer arithmetic have no builtins.** Settled: `01-core.md`
  §5.4 makes `as` convert between raw pointer types, between `*T` and `*mut T`, and
  between a pointer and `u64`. `02-memory.md` §15 adds the safe `@offset(p, n)`. The
  dereference and `@borrow_from_raw`/`@slice_from_raw` remain `unsafe`, as they should.
- **L3. Arena and interior mutability.** Settled: `02-memory.md` §13.6 says an arena
  contains a `Cell`, so `&Arena` has interior mutability and is not read-only. It may
  still hold views because nothing stored in it can be read back except through a
  borrow it handed out (reworded by review 2).
- **L2. Allocation failure and linear arguments.** Settled by review 2 as a library
  convention, not a language rule (05-tooling §35): the `_or_return` forms.
