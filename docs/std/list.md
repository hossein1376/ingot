# `std.list`: the growable array

```ingot
import std.mem.AllocErr;
import std.mem.AllocFail;
import std.mem.Allocator;
```

`List[T, A]` is a contiguous, growable array of `T` in memory from the allocator `A`,
which it stores (02-memory §14.3). It is the first real user of the allocator interface
and of the `_or_return` convention (05-tooling §35, mem.md §1). Written for step R1.

## 1. The type

```ingot
/// A growable array. Linear: it owns its memory, so `deinit` or `deinit_with` is
/// required. A view of whatever `T` and `A` borrow.
pub linear struct List[T, A: Allocator] { ... }
```

The fields are private: the allocator, a `*mut T` to the first element, the length
and the capacity, all three counts in elements. A list that has never allocated holds
no memory: its pointer is non-null, aligned for `T`, and never dereferenced, and its
capacity is 0. Every function below keeps `0 <= len <= capacity` and the first `len`
elements initialized; that invariant is why the fields are private (01-core §4.2).

## 2. Functions

```ingot
impl[T, A: Allocator] List[T, A] {
    /// An empty list. Allocates nothing, so it cannot fail.
    pub fn init(a: A) -> List[T, A]

    /// An empty list with room for `n` elements. A negative `n`, or a byte size above
    /// 2^62, is `.bad_request`.
    pub fn with_capacity(a: A, n: i64) -> List[T, A] ! AllocErr

    pub fn len(self: &List[T, A]) -> i64
    pub fn capacity(self: &List[T, A]) -> i64
    pub fn is_empty(self: &List[T, A]) -> bool

    /// Makes room for at least `extra` more elements without another allocation.
    /// A negative `extra` does nothing.
    pub fn reserve(self: &mut List[T, A], extra: i64) -> void ! AllocErr

    /// Appends `item`; on failure gives it back.
    pub fn push_or_return(self: &mut List[T, A], item: T) -> void ! AllocFail[T]

    /// Inserts `item` at index `i`, shifting the rest up. `i` may equal `len`.
    pub fn insert_or_return(
        self: &mut List[T, A],
        i: i64,
        item: T,
    ) -> void ! AllocFail[T]

    /// Removes and returns the last element, or `.none` when the list is empty.
    pub fn pop(self: &mut List[T, A]) -> ?T

    /// Removes the element at `i` and shifts the rest down.
    pub fn remove(self: &mut List[T, A], i: i64) -> T

    /// Removes the element at `i` and moves the last element into its place.
    pub fn swap_remove(self: &mut List[T, A], i: i64) -> T

    pub fn get(self: &List[T, A], i: i64) -> &T from self
    pub fn get_mut(self: &mut List[T, A], i: i64) -> &mut T from self

    pub fn items(self: &List[T, A]) -> []T from self
    pub fn items_mut(self: &mut List[T, A]) -> []mut T from self

    /// Consumes the list: `f` receives each element, first to last, then the memory is
    /// freed. The form for a linear `T`.
    pub fn deinit_with[F: FnMut(T)](self: List[T, A], f: F) -> void
}

impl[T: Discard, A: Allocator] List[T, A] {
    /// Appends `item`; on failure it is dropped.
    pub fn push(self: &mut List[T, A], item: T) -> void ! AllocErr

    pub fn insert(self: &mut List[T, A], i: i64, item: T) -> void ! AllocErr

    /// Drops the elements from index `len` on. A `len` at or above the length does
    /// nothing; a negative one clears the list.
    pub fn truncate(self: &mut List[T, A], len: i64) -> void

    pub fn clear(self: &mut List[T, A]) -> void

    pub fn deinit(self: List[T, A]) -> void
}

impl[T: Copy, A: Allocator] List[T, A] {
    /// Appends a copy of every element of `items`.
    pub fn extend(self: &mut List[T, A], items: []T) -> void ! AllocErr
}
```

**Errors.** Allocating functions return `AllocErr` or `AllocFail[T]` directly, not a
`ListErr` (plan.md D4): those two types are what `ingot cost` reports as `alloc`
(05-tooling §31), and allocation is the only way a list operation fails.

**Indices.** `get`, `get_mut`, `remove`, `swap_remove` and an `insert` index outside
the list (`i < 0`, `i >= len`, or `i > len` for `insert`) are the caller's misuse of an
index and trap T01, as `items()[i]` would (05-tooling §35, plan.md "Traps in std").
`pop` on an empty list is not misuse; it gives `.none`.

**Growth.** When a push needs room, the capacity becomes the larger of 4, twice the
old capacity, and the length needed. The list first asks the allocator to `resize` in
place; when that refuses, it allocates, copies the elements, and frees the old block.
Growth that would exceed 2^62 bytes is `.bad_request`. A zero-sized `T` allocates
zero-sized blocks, which needs no special case (mem.md D7).

**Borrows.** `items()` and `get` borrow the list shared, `items_mut()`, `get_mut` and
every growing call borrow it exclusively, so pushing while a slice of the list is live
is an exclusivity error (02-memory §14.3). Flow through `T` follows 02-memory §13.3:
`push` adds the item's origins to the list, `pop` returns a value with the list's
origins.

**`deinit`.** `deinit` drops the elements (they are `Discard`, so nothing runs) and
frees the block. With `A = &Arena` the free does nothing and the whole call compiles to
nothing (02-memory §14.3).

## 3. Properties

The storage is behind a raw pointer, so `Send` and `Sync` are stated, not derived
(mem.md §10):

```ingot
unsafe impl[T: Send, A: Allocator + Send] Send for List[T, A] {}
unsafe impl[T: Sync, A: Allocator + Sync] Sync for List[T, A] {}
```

| Property | `List[T, A]` |
|---|---|
| `Copy` | no |
| `Discard` | no |
| linear | yes |
| `Owned` | if `T` and `A` are `Owned` |
| view | if `T` or `A` is a view |
| `Send` | if `T` and `A` are `Send` |
| `Sync` | if `T` and `A` are `Sync` |

## 4. What is `unsafe` inside

The element storage is raw allocator memory. Reading and writing elements uses `@offset`
on the element pointer and a raw dereference; storing into a slot past `len` is an
assignment through a raw pointer, which is not checked for a linear value already there
(02-memory §10), because there is none. `items`, `items_mut`, `get` and `get_mut` make
their borrows with `@slice_from_raw` and `@borrow_from_raw`; they are sound because the
first `len` elements are initialized and every call that could move them takes
`&mut self`.

## Decisions

- **D1. `get` traps out of range.** `get(i) -> &T from self`, as 02-memory §13.3 shows
  it, and indexing on `items()` already traps. *Rejected:* `get(i) -> ?&T`, which would
  make every index a `match` for what is the caller's own bug.
- **D2. `init` allocates nothing**, so `List[T, A].init(a)` needs no `try`, as 02-memory
  §14.3 and the primer write it.
- **D3. `extend` for `Copy` elements only.** A linear or non-`Copy` source would need a
  consuming iterator, which waits for the iterator design.
