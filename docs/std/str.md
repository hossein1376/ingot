# `std.str`: the owned byte string

```ingot
import std.list.List;
import std.mem.AllocErr;
import std.mem.Allocator;
```

`Str[A]` is an owned, growable byte string (05-tooling §35). Text in Ingot is `[]u8`:
paths, arguments and environment values are bytes on every target, and a `Str` holds no
encoding promise. UTF-8 validation, decoding and number printing belong to `std.text`.
Written for step R1.

## 1. The type

```ingot
/// An owned byte string. Linear, like the list it is built on.
pub linear struct Str[A: Allocator] { ... }
```

The one private field is a `List[u8, A]`. A distinct type rather than an alias, so
that it can offer `append` and `eq` and keep the list's element functions out of its
surface.

## 2. Functions

```ingot
impl[A: Allocator] Str[A] {
    /// An empty string. Allocates nothing.
    pub fn init(a: A) -> Str[A]

    /// A copy of `text`.
    pub fn from_bytes(a: A, text: []u8) -> Str[A] ! AllocErr

    pub fn len(self: &Str[A]) -> i64
    pub fn capacity(self: &Str[A]) -> i64
    pub fn is_empty(self: &Str[A]) -> bool

    pub fn reserve(self: &mut Str[A], extra: i64) -> void ! AllocErr

    pub fn push(self: &mut Str[A], byte: u8) -> void ! AllocErr

    /// Appends a copy of `text`.
    pub fn append(self: &mut Str[A], text: []u8) -> void ! AllocErr

    pub fn bytes(self: &Str[A]) -> []u8 from self
    pub fn bytes_mut(self: &mut Str[A]) -> []mut u8 from self

    /// Byte-wise equality with `text`.
    pub fn eq(self: &Str[A], text: []u8) -> bool

    /// Keeps the first `len` bytes; a larger `len` does nothing, a negative one clears.
    pub fn truncate(self: &mut Str[A], len: i64) -> void
    pub fn clear(self: &mut Str[A]) -> void

    pub fn deinit(self: Str[A]) -> void
}
```

Allocation failures are `AllocErr`, for the reason in list.md §2. `bytes` is named for
what it returns (05-tooling §35: a function returning a borrow into a container is
named for it).

## 3. Properties

The same as `List[u8, A]`, from which they are derived (list.md §3 states the list's
`Send` and `Sync`): linear; `Owned`, `Send` and `Sync` exactly when `A` is; a view when
`A` is.

## Decisions

- **D1. Bytes, not UTF-8.** `Str` makes no encoding promise, as paths and arguments
  do not (05-tooling §35). A UTF-8-checked string type, if wanted, is `std.text`'s.
- **D2. `eq` against `[]u8`.** One function covers a literal and another string
  (`a.eq(b.bytes())`). `Eq`, `Ord` and `Hash` implementations wait for `std.text` and
  `std.hash`, where comparison and hashing of bytes are designed.
