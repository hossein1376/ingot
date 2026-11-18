# Ingot v1 specification, part 1: the core language

Words used with a fixed meaning throughout the specification:

* **error**: the compiler rejects the program and emits a diagnostic with a stable code.
* **trap**: the running program prints `file:line:col: trap TNN: message` to standard
  error and the whole process exits with status 101. A trap is deterministic and
  happens in every build. There is no unwinding, so no `defer` runs. Section 16.1
  lists the traps, and [impl/diagnostics.md](impl/diagnostics.md) gives each its code
  and exact text.
* **safe code**: any code outside an `unsafe` block.

Design rule: **one obvious spelling, nothing implicit.** When
something is ambiguous the compiler reports an error with a fix edit. There are no
warnings.

## 1. Lexical structure

* Source files are UTF-8 with the extension `.ingot`. Outside string literals and
  comments only ASCII is allowed. Lines end with a line feed on every system. A carriage
  return in a source file is an error whose fix removes it.
* **Value identifiers** match `[a-z_][a-z0-9_]*`. They name variables, functions,
  fields, enum variants, modules, constants and constant generic parameters (`n` in
  `[T, const n: i64]`).
* **Type identifiers** match `[A-Z][A-Za-z0-9]*`. They name types, interfaces and type
  parameters. The two classes are different tokens, so the parser always knows whether
  a name is a type or a value without a symbol table. Constants are lower_snake like
  every other value: `max_len`, never `MAX_LEN`.
* Built-in type names are keywords: `i8 i16 i32 i64 u8 u16 u32 u64 f32 f64 bool void
  never`, and the vector types of section 26 (`f32x4`, `u8x16`, `boolx8`, ...).
* Keywords: `align as bench break catch const continue copy defer dispatch dyn else enum
  errdefer export extern fail false fn for from if impl import in inline interface into
  kernel let linear loop match mut noinline orelse pub return scope select spawn
  static struct test true try type unsafe var waitable when while`. Six more words
  (`par`, `step`, `over`, `on`, `reduce`, `grain`) are keywords only in one position
  each and are ordinary identifiers elsewhere: `par` only where it begins a parallel
  loop (`par for`, `par(grain: n) for`, section 19), so `exec.par` is an ordinary name.
* Builtins are spelled `@name`. They are in their own namespace and are part of the
  language, not the library.
* Comments: `//` to end of line. `///` before a declaration is a doc comment and is
  returned by `ingot api`. There are no block comments.
* Integer literals: `123`, `1_000_000`, `0xff`, `0o17`, `0b1010`. A character literal
  `'a'`, `'\n'`, `'\x7f'` is an integer literal with that byte value.
* Float literals: `1.5`, `2e10`, `1.0e-3`. A digit is required on both sides of the dot.
* After a `.`, a run of digits is a tuple field index: `t.0.1` is two field accesses,
  never a float.
* A `-` written directly before a numeric literal is part of the literal, so `-128` is
  a valid `i8` and `-9223372036854775808` a valid `i64`.
* String literals: `"..."` has type `[]u8` and lives for the whole program. `c"..."` has
  type `*u8`, is NUL-terminated and exists for FFI. Escapes: `\n \t \r \0 \\ \" \' \xHH`.
* Statements end with `;`. Blocks use braces and braces are never optional.

## 2. Modules

* One file is one module. A directory with an `ingot.toml` is a package (section 33).
  The module `net/http.ingot` in package `app` has the path `app.net.http`.
* `main.ingot` at the package root is the package's **root module**. Its path is the
  package name itself, so its public items are reached as `app.name`: `std.Env` is the
  item `Env` of `std/main.ingot`. A package with no `main.ingot` has no root module.
* `import a.b.c;` binds the last segment. That segment is either a module (then its
  public items are reached as `c.name`) or a public item of module `a.b` (then it is
  used directly). A module's submodules are not reached through it: `import a.b.c;`
  and use `c.name`, not `b.c.name`. `import a.b.c as d;` binds it under another name.
* There is no wildcard import and no merged namespace. Two imports that bind the same
  name are an error.
* Modules of one package may import each other freely, cycles included. Names are
  resolved for the whole package at once and no code runs at load time (section 4.5),
  so there is no initialization order to get wrong. Packages form a directed acyclic
  graph.
* An item is private to its module unless it is marked. `pub(pkg)` makes it visible in
  every module of its package, and `pub` everywhere. A struct field is marked the same
  way. Enum variants and interface methods are as visible as their type. A function in
  an `impl T` has its own mark, and a function in an `impl I for T` is as visible as
  `I`. `pkg` is not a keyword: it means something only between those parentheses.
* There are no re-exports: an item has one path, the one tools print. A signature may
  mention a type less visible than itself. The value can then be used where the type
  cannot be named, which is a way to make a token.
* Code the compiler generates (the language items of pipeline.md section 8) is not
  subject to visibility.
* A program is compiled whole. There are no separately compiled Ingot libraries.

## 3. Types

| Type | Size, align | Notes |
|---|---|---|
| `i8 i16 i32 i64`, `u8 u16 u32 u64` | 1, 2, 4, 8 | two's complement. Lengths and indices are `i64` |
| `f32 f64` | 4, 8 | IEEE-754 binary32 and binary64 |
| `bool` | 1 | `true` or `false`, never an integer |
| `void` | 0, 1 | the type with one value. Used for functions that return nothing |
| `never` | 0, 1 | the type with no values: the type of `return`, `fail`, `break`, `@panic(...)` |
| `[N]T` | N·size(T) | array. `N` is a constant expression. Field `.len: i64`, the constant `N` |
| `(A, B, ...)` | struct layout | tuple of two or more. Fields are `.0`, `.1`, ... |
| `struct` | section 25 | fields in declaration order |
| `enum` | section 25 | a tag and at most one payload per variant |
| `?T` | section 25 | optional. The same as the enum `Option[T] { none, some: T }` |
| `T ! E` | section 6 | result. The same as the enum `Result[T, E] { ok: T, err: E }` |
| `&T`, `&mut T` | 8 | shared and exclusive borrow of one `T`. Never null |
| `[]T`, `[]mut T` | 16 | shared and exclusive slice: a borrow of `len` consecutive `T`. Fields `.len: i64` and `.ptr`, a raw pointer to the first element (`*T` or `*mut T`) |
| `*T`, `*mut T` | 8 | raw pointer. May be null. Can be created anywhere and used only in `unsafe` |
| `fn(A, B) -> R ! E` | 8 | pointer to a function with no captures |
| `&dyn I`, `&mut dyn I` | 16 | borrow of some value whose type implements interface `I`, with a method table |
| `f32x4`, `u8x16`, `boolx4`, ... | section 26 | SIMD vectors and lane masks |

There is no `usize`, no `char`, no string type distinct from `[]u8` and no implicit
integer promotion. Text handling in the standard library validates and decodes UTF-8
explicitly.

No array or slice has more than 2^62 elements. An array type with a negative or longer
length is E0521, and so is a type whose size is above 2^62 bytes (`[n][n]u64` can be).
A length or size computed from a constant generic argument is checked where the type
is concrete: in a signature or a type written in a body (phase 6), and inside a generic
body for each instance (phase 11). `@slice_from_raw` requires the same limit. So `i + c` cannot overflow when `i` is at most
a length and `c` is a constant no larger than 2^62.

**Type properties.** Every type has four properties that the compiler computes from its
structure. Section 10 defines them. They are `Copy`, `Discard`, `Send` and `Sync`, and a
type is additionally either a **view type** (it contains a borrow) or **self-contained**.
The bound `Owned` names the self-contained types.

**No implicit conversions.** A value only flows between identical types. There are two
exceptions. Untyped numeric literals take the type their context expects; an integer
literal may take a float type when its value is exactly representable in it (`1` is
`1.0`), and otherwise it is error E0405. When both operands of an operator are untyped
literals, an integer literal takes the type of a float literal beside it, so `1 + 2.0`
is an `f64`. The literal defaults: an integer literal with no context is `i64` and a
float literal is `f64`. And a `&T` or `&mut T` becomes a `&dyn I` or `&mut dyn I`
wherever the expected type reaches and is that type, and `T` implements `I`
(section 4.4). The expected type reaches into the branches of an `if` or `match`, a
`catch` or `orelse` value and the payload of `.some(...)`, as it does for literals.

**Literals under a numeric bound.** Where a literal's expected type is a type parameter,
the literal must be a value of every type the parameter's bound allows: an integer
literal in `0..=127` under `Num` or `Int`, in `-128..=127` under `SInt`, in `0..=255`
under `UInt`, and under `Float` any integer that `f32` holds exactly, or any float
literal (rounded for each instance). Anything else is E0405. A float literal under a
bound that also allows integers is E0401. The check is made once, in the generic body;
only the value differs per instance.

**Expected-type shorthand.** Where the expected type is stated by a declaration (a typed
`let`, a parameter, a field, a return type, the other side of an assignment or `==`),
an enum value may be written `.name` or `.name(payload)` and a struct value `.{ ... }`.
Patterns in a `match` always allow the shorthand because the scrutinee's type is known.

## 4. Declarations

### 4.1 Functions

```ingot
/// Returns the index of the first `needle` in `hay`.
pub fn find(hay: []u8, needle: u8) -> ?i64 {
    for i in 0..hay.len {
        if hay[i] == needle { return .some(i); }
    }
    return .none;
}
```

* Parameters are immutable bindings. The return type is written unless it is `void`.
* A function with a result must end every path in `return`, `fail` or an expression of
  type `never`.
* Modifiers come before `fn`: `pub`, `inline` (always expanded at direct calls),
  `noinline`, `unsafe` (callable only in `unsafe`), `export` (C ABI and an unmangled
  symbol, section 29), `kernel` (section 27), `waitable` (section 21).
* `-> T ! E` declares a fallible function (section 6). `from` and `into` clauses declare
  borrow flow (section 13).
* Functions do not overload. A name in a module means one function.

### 4.2 Structs

```ingot
copy struct Vec3 { pub x: f32, pub y: f32, pub z: f32 }

struct Parser { src: []u8, pos: i64 }          // holds a borrow, so it is a view type

linear struct File { fd: i32 }                  // must be consumed exactly once
```

* A plain `struct` is moved when assigned or passed and may be discarded.
* `copy struct` may be duplicated implicitly. Every field must be `Copy`.
* `linear struct` must be consumed (section 10). A struct with a linear field is linear
  whether or not it says so, and the compiler requires the keyword in that case so the
  declaration tells the truth.
* Literal: `Vec3.{ x: 1.0, y: 2.0, z: 3.0 }`. Every field is named and every field is
  given. There are no default field values.
* Destructuring: `let Vec3.{ x, y, z } = v;`. A struct pattern may end in `..` to
  leave the remaining fields out: `let Vec3.{ x, .. } = v;`. A field left out of a pattern
  that moves the value is dropped, so its type must be `Discard`; leaving out a linear
  field is error E1003. Destructuring a struct is only possible where every field is
  visible, and, as for construction below, a struct with no fields counts as having a
  private one. This is how a linear struct is finally taken apart.
* **Construction respects privacy.** Outside a struct's module, a value of it can be
  made from nothing only if every field is visible there. This covers a literal, `@zero` and
  `@bits` alike, and a struct with no fields counts as having a private one. A type
  with a private field is therefore only ever produced by its own module's functions.
  That is what lets a module keep an invariant (a `List` whose pointer and length
  agree), and what stops a capability such as `Heap` or `Fs` from being conjured.
  The same holds for an enum with a payload of such a type.

### 4.3 Enums

```ingot
copy enum Color: u8 { red = 1, green, blue }

enum Shape { circle: f64, rect: Rect, empty }

enum LoadErr { io: from fs.IoErr, parse: from ParseErr, too_large: i64 }
```

* A variant has no payload or one payload type. Use a tuple or struct for several values.
* `: u8` fixes the tag type and allows explicit tag values. Without it the tag is the
  smallest unsigned integer that fits and the values are 0, 1, 2, ...
* Values: `Shape.circle(2.0)`, `Shape.empty`. The payload is read only through `match`
  or `if let`, so a wrong-variant read cannot be written.
* An enum is `Copy` only if declared `copy enum`, and is linear if any payload is.
* `from` on a variant is used by `try` (section 6.3). At most one variant of an enum may
  be `from` any given type.
* An enum with no payloads converts to its tag type with `as`. The other direction is
  `@enum(Color, x)`, which gives `?Color`.

### 4.4 Interfaces and implementations

```ingot
import std.hash.Hasher;

interface Length {
    fn length_squared(self: &Self) -> f32;
}

impl Vec3 {
    pub fn dot(self: Vec3, o: Vec3) -> f32 { return self.x * o.x + self.y * o.y + self.z * o.z; }
}

impl Hash for Vec3 {
    fn hash(self: &Vec3, state: &mut Hasher) {
        state.write_u32(@bits(u32, self.x));
        state.write_u32(@bits(u32, self.y));
        state.write_u32(@bits(u32, self.z));
    }
}

impl Length for Vec3 {
    fn length_squared(self: &Vec3) -> f32 { return self.dot(self.*); }
}
```

* `impl T { ... }` adds functions to the namespace of `T`. A function whose first
  parameter is named `self` is a method and may be called as `value.name(args)`.
  Others are called `T.name(args)`. The type of `self` is `T`, `&T` or `&mut T`.
* `impl T { ... }` must be in the module that declares `T`. For a built-in type that is
  `std` alone, which may write inherent impls for the built-in type constructors
  (`impl[T] []mut T { ... }`, `impl[T] ?T { ... }`); this is where functions such as
  `s.shared()` and `o.take()` are declared, with ordinary signatures.
* The fields, variants, methods, associated functions and associated constants of a
  type share one namespace. Two with one name are E0308, at the second.
* `T.name` without a call names an inherent function or method of `T` as a value, with
  `self` as an ordinary first parameter. A variant with a payload is not a function value
  (E0410), and neither is `value.method` without a call (E0314): write a closure.
* **Receiver passing is the one place a borrow is inserted for you.** `v.dot(o)` passes
  `v`, `&v` or `&mut v` according to the declared type of `self`. Every other argument is
  passed exactly as written, so a callee that mutates an argument shows `&mut` at the
  call. A method that takes `self` by value consumes a non-`Copy` receiver.
* An interface lists required functions, may declare associated types (`type Item;`) and
  may provide default bodies. `Self` is the implementing type.
* `impl I for T` must be in the module that declares `I` or the module that declares `T`.
  If `I` has supertraits (`interface Ord: Eq`), `T` must implement each of them as well,
  by an `impl` or as a built-in type.
  A type implements an interface at most once. `T` is a named type, possibly generic
  (`impl[K: Hash] Hash for Pair[K, K]`), or a shared borrow of a named type declared in
  the same module (`impl Allocator for &Arena`), never a bare type parameter, so two
  implementations can never overlap.
* `value.name(args)` looks in `impl T` first and then in the interfaces `T` implements.
  If two interfaces supply the name, the call is written `I.name(value, args)`. A
  function of `I` without `self` may be called `I.name(args)` when the argument types
  determine `Self`; otherwise it is E0803 and the call is written `T.name(args)`.
* Calls through a generic parameter are resolved at compile time. Dynamic dispatch
  happens only through `&dyn I` and `&mut dyn I`, and only for interfaces that declare
  no associated type and whose functions all take `self` by borrow, have no generic
  parameters, and mention `Self` nowhere else. Of the built-in interfaces, `Hash` and
  `Format` qualify. A `&T` (`&mut T`) becomes a `&dyn I` (`&mut dyn I`) wherever the
  expected type reaches and is one, and `T` implements `I` (section 3). This is the only
  implicit conversion between non-literal values. A `&dyn J` does not convert to a
  `&dyn I`, even when `I` is a supertrait of `J`.
* **The built-in interfaces** are declared by the language, as if this were written in
  every module (they need no import, section 8):

  ```ingot
  copy enum Ordering { less, equal, greater }

  interface Eq {
      fn eq(self: &Self, other: &Self) -> bool;
  }
  interface Ord: Eq {
      fn cmp(self: &Self, other: &Self) -> Ordering;
  }
  interface Hash {
      fn hash(self: &Self, state: &mut hash.Hasher);
  }
  interface Iter {
      type Item;
      fn next(self: &mut Self) -> ?Self.Item from self.*;
  }
  interface Format {
      fn format(self: &Self, out: &mut fmt.Formatter);
  }
  ```

  `hash` and `fmt` are `std.hash` and `std.fmt`; a module that implements `Hash` or
  `Format` imports `std.hash.Hasher` or `std.fmt.Formatter` to name them. A `Formatter`
  records the first write error itself, and `@format` returns it, so `format` cannot
  fail. Every integer, `bool`, raw pointer and payload-free enum implements `Eq`, `Ord`
  and `Hash`, comparing and hashing by value. Floats implement `Eq` and `Ord` with the
  IEEE total order, under which NaN equals itself and `-0.0` differs from `0.0`; the
  operator `==` stays IEEE equality. An array, a slice (`[]T` or `[]mut T`), a tuple and
  a `?T` implement `Eq`, `Ord` and `Hash` when their element types do: element by
  element, in order, a shorter slice before a longer one with the same prefix, `.none`
  before `.some`. A slice is compared and hashed by its elements, never by its address. A
  `&T` implements each of them when `T` does, through the borrow. Nothing else
  implements them without an `impl`. Every scalar,
  vector, mask and slice of `u8` (`[]u8` or `[]mut u8`) implements `Format`. `Ordering` is a builtin name like the
  interfaces. `Executor` (section 27.2) has no members a program can call; it is
  implemented only by the executors `std.exec` and `std.gpu` declare, and an `impl
  Executor` anywhere else is an error.
* There is no inheritance and no operator overloading. `+` is never a function call.
  The built-in interfaces `Eq`, `Ord` and `Hash` are ordinary: `a.eq(&b)`, `a.cmp(&b)`.
  The operators `==` and `<` work only on scalars, raw pointers and payload-free enums,
  and lane-wise on vectors (section 26).

### 4.5 Constants, statics and aliases

```ingot
const max_depth: i64 = 64;                          // compile-time value, no address
static hits: Atomic[i64] = Atomic[i64].init(0);     // one address, lives for the program
type Bytes = List[u8, Heap];
```

* A `const` initializer and a `static` initializer are evaluated at compile time
  (section 9). No code runs before `main`.
* A `static` is immutable through its name. Its type must be `Sync`, so any mutation
  goes through an atomic or a lock. There are no mutable global variables.
* A `static` or `const` that holds a borrow may only borrow other statics and literals.

### 4.6 Tests

```ingot
test "find reports the first match" {
    let i = find("hello", 'l') orelse -1;
    @assert(i == 2);
}
```

Test blocks are compiled only by `ingot test`. A test body may use `try`. A test fails
if it traps or if an error reaches the end of the block.

## 5. Expressions

### 5.1 Operators

From lowest to highest precedence:

| Level | Operators | Notes |
|---|---|---|
| 1 | `orelse`, `catch` | section 6 |
| 2 | `\|\|` | short-circuit |
| 3 | `&&` | short-circuit |
| 4 | `== != < <= > >=` | do not chain |
| 5 | `& \| ^` | bitwise |
| 6 | `<< >>` | shifts |
| 7 | `+ - +% -% +\| -\|` | |
| 8 | `* / % *% *\|` | |
| 9 | `as` | lossless conversion |
| 10 | prefix `- -% ! ~ & &mut try` | |
| 11 | postfix: call, index, `.field`, `.*`, `.[T]` | |

**The parenthesization rule.** In one unparenthesized expression, a
bitwise operator or a shift may not be mixed with arithmetic, a comparison, another
shift or a different bitwise operator. `a & b == 0` is an error whose fix inserts the
parentheses.

**Evaluation order** is left to right: operands, then arguments in order. In an
assignment the operands of the target (its index expressions) are evaluated first, then
the value; then the target is located and checked, and the store is made. A compound
assignment `p op= v` reads `p` after `v` is evaluated, by the same rule.

### 5.2 Integer arithmetic

| Operator | Result outside the type's range |
|---|---|
| `+ - *`, unary `-` | **trap** |
| `+% -% *%`, unary `-%` | wraps modulo 2ⁿ |
| `+\| -\| *\|` | clamps to the minimum or maximum |
| `/ %` | trap on a zero divisor and on `MIN / -1`. `/` truncates toward zero and `%` has the sign of the dividend |
| `<< >>` | trap if the count is negative or not less than the bit width. `>>` is arithmetic for signed types |

Unary `-` is defined for signed integers and floats, and `-%` for every integer type.
A shift count may have any integer type.

Builtins that cannot trap, for code that must not (kernels, section 27):
`@div_or(a, b, fallback)` and `@rem_or(a, b, fallback)` give `a / b` and `a % b`, or
`fallback` when `b` is zero or the division overflows (`MIN / -1`); `@shl(x, n)` and
`@shr(x, n)` shift by `n` modulo the bit width. The operands of each have one integer
type, and the count of a shift may have any integer type.

The behavior is identical in every build. The compiler removes an overflow check when it
can prove the result is in range (section 31 says exactly when it must), and `ingot cost`
lists the checks that remain. Code that wants wrapping says so with `+%`.

### 5.3 Floating point

IEEE-754, round to nearest even, no traps. The compiler does not reassociate, does not
contract `a * b + c` into a fused multiply-add and does not assume the absence of NaN or
infinities. `@fma(a, b, c)` is the explicit fused form. Comparisons with NaN are false,
except `!=`. `%` is not defined on floats: `@rem(a, b)` is the remainder of truncated
division, with the sign of `a`, as `%` is for integers. `==` is IEEE equality; the `Eq`
and `Ord` implementations of floats use the total order instead (section 4.4). The only
place the language permits a different association order is a `reduce` kernel
(section 27), where it is declared.

### 5.4 Conversions

`x as T` compiles only if every value of the source type is a value of `T`: a narrower
integer to a wider one, unless the source is signed and the target unsigned (`u8` to
`i32` and `i8` to `i64` compile, `i8` to `u64` does not), any 32-bit-or-smaller integer to `f64`,
any 16-bit-or-smaller integer to `f32`, `f32` to `f64`, `bool` to any integer, a
payload-free enum to its tag type, `&T` to `*T`, `&mut T` to `*mut T`, `*T` to `*U` and
`*mut T` to `*mut U` for any `T` and `U`, `*T` and `*mut T` to each other, and a raw
pointer to and from `u64`. A raw pointer's value is its address, so these keep it. Everything
else is a builtin that names what happens to values that do not fit:

| Builtin | Meaning |
|---|---|
| `@wrap(T, x)` | integer to integer, modulo 2ⁿ |
| `@narrow(T, x)` | integer or float to integer. Traps if the value (after truncation toward zero) is not representable. NaN traps |
| `@sat(T, x)` | integer or float to integer, clamped to the range of `T`. NaN gives 0 |
| `@nearest(T, x)` | integer to float, or `f64` to `f32`, rounded to nearest even |
| `@bits(T, x)` | reinterprets the bytes. Both types must have the same size, contain no borrows and no padding, and every bit pattern must be valid for `T` |

### 5.5 Places, indexing and slicing

* A **place** is a variable, a field of a place, an element of a place, or `r.*` where
  `r` is a borrow.
* `r.field`, `r.method()` and `r[i]` reach through a borrow. Reading or writing the
  whole value is explicit: `r.*`. A raw pointer has no shorthand: `p.*.field`, in
  `unsafe`.
* `a[i]` on an array or slice takes an `i64` index and traps if `i < 0 || i >= len`. It
  is a place.
* `s[lo..hi]` on a slice gives a slice of the same kind and traps unless
  `0 <= lo <= hi <= s.len`. Either bound may be left out: `s[lo..]` ends at `s.len`,
  `s[..hi]` starts at 0, and `s[..]` is the whole slice, with the same check. Taking a
  slice of an array is a borrow and is written as one: `&a[lo..hi]` gives `[]T`,
  `&mut a[lo..hi]` gives `[]mut T`.
* `&p` and `&mut p` borrow a place (section 12). `&mut` needs a `var` or a place reached
  through `&mut`. `&e` of a value that is not a place (`&f(x)`, `&(a + b)`, `&"ada"`)
  borrows a temporary that lives to the end of the statement (02-memory section 11);
  `&mut` of one is E0607, because a change made to a temporary would be lost.
* An exclusive borrow does not convert to a shared one by itself. `&r.*` reborrows a
  `&mut T` as `&T`, and `s.shared()` reborrows a `[]mut T` as `[]T`.

### 5.6 Calls and closures

* A call passes each argument by value: a `Copy` value is copied, anything else is moved.
* Type arguments of a generic function are inferred from the argument types alone. An
  untyped literal does not decide a type parameter; if nothing else does, the literal's
  default type is used. When a generic parameter does not appear in a parameter type,
  write its arguments: `f.[T](args)`, `make.[u8, 16](args)`.
* A **closure** is written with an explicit capture list:

  ```ingot
  let scale = 3;
  var total: i64 = 0;
  xs.each(fn [scale, &mut total](x: i64) { total.* += x * scale; });
  ```

  A capture is `name` (copy or move the variable in), `&name` or `&mut name`. Inside the
  body the name has the captured type and is an immutable binding. Nothing is captured
  that is not listed. Each closure has its own anonymous type, so it is passed to a
  generic parameter bounded by `Fn(A) -> R` (callable through `&`), `FnMut(A) -> R`
  (through `&mut`) or `FnOnce(A) -> R` (consumed by the call), and the call is direct.
  Because a closure's type has no name, a closure is stored in a field or returned only
  through a type parameter or as `&dyn`. A closure never converts to a `fn` type (E0401):
  a named function is the spelling for that.
* A closure satisfies `Fn` when every capture is `&name` or a by-value capture of a
  `Copy` type; `FnMut` when its other captures are all `&mut name`; and only `FnOnce`
  when it captures a non-`Copy` value by value. `Fn` implies `FnMut`, which implies
  `FnOnce`. A function or `fn` pointer satisfies all three.
* `f(args)` uses `f` by shared borrow when its type is `Fn`, by exclusive borrow when it
  is only `FnMut`, and moves it when it is only `FnOnce`. The exclusive use is allowed on
  a `let` or a parameter: a closure never changes its own bytes, only what its `&mut`
  captures point at. A `&F` is callable when `F: Fn` and a `&mut F` when `F: FnMut`. The
  dynamic forms are `&dyn Fn(A) -> R` and `&mut dyn FnMut(A) -> R`, and they convert as
  `&dyn I` does (section 4.4): a `&T` becomes a `&dyn Fn(A) -> R` when `T: Fn(A) -> R`,
  and a `&mut T` becomes a `&mut dyn FnMut(A) -> R` when `T: FnMut(A) -> R`, wherever the
  expected type reaches and is one. `T` may be a closure type, a function or a `fn`
  pointer type.
* A closure's value is its **capture record**: a struct whose fields are the captures in
  the order of the capture list, each of its captured type (`T`, `&T` or `&mut T`), laid
  out as a struct (04 §25). A closure with no captures has size 0 and alignment 1.

### 5.7 Conditional and match expressions

`if c { a } else { b }` and `match` are expressions when every branch is a single
expression, or a block that ends in `return`, `fail`, `break`, `continue` or a trap and
so never gives a value. Blocks do not have values: a branch that needs statements and
then a value belongs in a statement-form `if` or in a function. A branch block that can
reach its end is E0207. `unsafe("reason") { expr }` is the third expression form of this
kind (section 29).

```ingot
let sign: i64 = if x < 0 { -1 } else { 1 };
let area: f64 = match shape {
    .circle(r) => 3.14159 * r * r,
    .rect(q) => q.w * q.h,
    .empty => 0.0,
};
let v = match x { .a(y) => y, .b => { return 0; } };
```

`@select(c, a, b)` evaluates both `a` and `b` and picks without a branch.

## 6. Errors

### 6.1 Three kinds of failure

| Kind | Mechanism | Example |
|---|---|---|
| A condition the caller should handle | an error value, `T ! E` | file not found, parse error, arena full |
| A bug | a trap | index out of range, overflow, failed `@assert` |
| Absence | `?T` | lookup miss |

### 6.2 Fallible functions

```ingot
enum ParseErr { empty, bad_digit: u8, overflow }

fn parse_u32(s: []u8) -> u32 ! ParseErr {
    if s.len == 0 { fail .empty; }
    var n: u32 = 0;
    for c in s {
        if c < '0' || c > '9' { fail .bad_digit(c); }
        let d = (c - '0') as u32;
        if n > (0xffff_ffff - d) / 10 { fail .overflow; }
        n = n * 10 + d;
    }
    return n;
}
```

* In a function declared `-> T ! E`, `return v;` succeeds and `fail e;` fails. `return`
  never carries an error and `fail` never carries a result.
* `T ! E` is an ordinary type, `Result[T, E]`, so results can be stored, sent through a
  channel and returned from a task. A `Result` is linear: it cannot be discarded, not
  even with `_ =`. It is consumed by `try`, `catch` or `match`.

### 6.3 Handling

```ingot
let n = try parse_u32(text);                         // propagate
let m = parse_u32(text) catch _ => 0;                // replace with a value
let k = parse_u32(text) catch e => {                 // run a block that leaves
    @debug("bad number: {}", e);
    return;
};
```

* `try x` gives the success value, or fails the enclosing function with the error. The
  enclosing function's error type must be the same type `E`, or an enum with a variant
  declared `from E`, in which case the error is wrapped in that variant. No other
  conversion happens.
* `x catch e => expr` gives `expr` on failure. `x catch e => { block }` runs the block,
  which must end in `return`, `fail`, `break`, `continue` or a trap unless the success
  type is `void`. `_` in place of `e` discards the error value, so the error type must
  be `Discard` (E0414 otherwise).
* `x orelse y` and `x orelse { block }` do the same for `?T`.
* `try` binds to the whole postfix expression after it: `try a.b().c()` tries the result
  of `c()`.

### 6.4 Cleanup

* `defer { ... }` runs the block when the enclosing block exits by any path: falling off
  the end, `return`, `fail`, `break`, `continue` or a propagating `try`. Deferred blocks
  run in reverse order. A `return` value is computed before they run.
* `errdefer { ... }` runs only when its block is left because the function is failing:
  a `fail`, or a `try` that propagates.
* A deferred block may not contain `return`, `fail`, `try`, or a `break`/`continue` that
  leaves it. A fallible call inside it must be handled with `catch`.
* Deferred blocks are expanded at each exit. A trap does not run them.

### 6.5 Assertions

`@assert(cond)` and `@assert(cond, "msg")` are checked in every build. `@panic("msg")`
and `@unreachable()` trap unconditionally and have type `never`. An `@assert` is also
information: the compiler uses a dominating assertion to remove later checks
(section 31).

## 7. Statements

```ingot
let x = f();              // immutable, type inferred from the initializer
var buf: [64]u8 = @zero;  // mutable, explicit type
_ = g();                  // discard a result on purpose (the type must be Discard)
```

* Every binding is initialized. `@zero` is the all-zero value. It is allowed for
  numbers, `bool`, raw pointers and vectors, for arrays, tuples and structs of types
  that allow it (subject to the privacy rule of section 4.2), and for an enum that has a
  variant with tag value 0 (the first, unless tags are explicit) whose payload, if any,
  allows it. For any `?T` it is `.none`. It is an error for a type that contains a
  borrow.
* A name may not shadow another local, a parameter, or a `const` or `static` visible in
  its module (E0309). This is what keeps a binding in a pattern from being read as the
  constant it was meant to compare with.
* The pattern of a `let` or `var` cannot fail to match: a name, `_`, a tuple pattern
  or a struct pattern. Anything that can fail belongs in `if let` or `match`.
* Only a call may be an expression statement, optionally under `try` or followed by
  `catch`, and what is left after that must be `void`. Any other result is used, bound,
  or discarded with `_ =`.
* Conditions are `bool`. There is no truthiness.
* An unused import, an unused local binding, and a `var` that is never assigned or
  exclusively borrowed after its initialization are errors, each with a fix. A
  parameter may go unused.

### 7.1 Control flow

```ingot
if c { ... } else if d { ... } else { ... }
if let .some(v) = lookup(k) { ... } else { ... }
while i < n { ... }
while let .some(item) = it.next() { ... }
loop { ... }                                   // forever; leave with break or return
for i in lo..hi { ... }                        // i: the type of lo and hi, immutable
for i in lo..=hi { ... }                       // hi included
for i in lo..hi step 4 { ... }
for x in xs { ... }
for i, x in xs { ... }                         // with the i64 index
outer: for row in rows { for c in row { if c == 0 { break outer; } } }
```

`for x in e` accepts:

| `e` is | `x` is |
|---|---|
| `[]T` or an array, with `T: Copy` | `T`, a copy of each element |
| `[]mut T` | `&mut T` |
| an array value `[N]T`, with `T` not `Copy` | `T`, moved out. The array is consumed, and if `T` is linear the loop cannot be left early |
| a value whose type implements `Iter` | `Iter.Item`, by calling `next` until `.none` |

`xs.refs()` iterates a shared slice by `&T`, for elements that are not `Copy`. Loops
over arrays, slices and ranges are counted loops with no iterator object. A range whose
start is not below its end runs zero times. The counter never overflows: the loop ends
after the last value of the range, also when that value is the largest of its type or
the next step would pass it. A `for` over a range has no second variable:
the value is the counter (E0609 otherwise).

### 7.2 Match and patterns

```ingot
match msg {
    .quit => { return; }
    .move(Point.{ x, y }) if x == y => { diagonal(x); }
    .move(p) => { go(p); }
    .write(text) => { out.write(text); }
}
```

* A `match` must cover every value. For an enum that means every variant or an `else`
  arm. For integers, an `else` arm is required. The fix for a missing variant inserts
  the arms.
* Patterns: `_`, a binding name, a literal, a range `a..b` or `a..=b` with literal
  bounds, `.variant`, `.variant(p)`, `(p, q)`, `T.{ field, field: p }`, and `p | q`.
* `p | q` matches either. Every alternative binds the same names with the same types
  (E0603 at the alternative that differs).
* A bare name in a pattern binds a new local. When the scrutinee is an enum with a
  variant of that name, it is E0603 with a fix that inserts the `.`: `red =>` was meant
  as `.red =>`.
* `_` discards what it matches, so the type there must be `Discard` (E0414 otherwise):
  a linear payload is bound and consumed.
* Bindings follow the scrutinee. `match v` moves or copies payloads out of `v`.
  `match &v` binds shared borrows of the payloads and `match &mut v` binds exclusive
  borrows, so a variant can be updated in place. A binding is moved out only once its
  arm is chosen; in a guard it is read through a shared borrow.
* There is no fallthrough. `break` and `continue` inside a `match` refer to the
  enclosing loop.

### 7.3 Compile-time branches

```ingot
when @target.os == .macos {
    type Poller = kqueue.Queue;
} else when @target.os == .linux {
    type Poller = uring.Ring;
} else {
    type Poller = iocp.Port;
}
```

`when` takes a constant condition built only from `@target`, literals, comparisons and
`&& || !`. It cannot name a constant or call a function, because it is decided before
names are resolved. Only the taken branch is type-checked and compiled.
It may appear as a statement or around items at module level. `ingot check
--all-targets` (section 33) is how the branches for other targets get checked.

## 8. Generics

```ingot
struct Pair[A, B] { pub a: A, pub b: B }

fn max[T: Ord + Copy](a: T, b: T) -> T {
    return if a.cmp(&b) == .less { b } else { a };
}

fn sum[T: Num](xs: []T) -> T {
    var s: T = @zero;
    for x in xs { s = s + x; }
    return s;
}

struct Ring[T, const n: i64] { items: [n]T, head: i64 }
```

* Generic parameters are types or `const` values of integer, `bool` or enum type. A
  constant parameter is a value and is spelled like one (`const n: i64`); an argument for
  it is a literal (a negative number included: `Ring[u8, -1]` is written as in an
  expression, section 1), a constant name or `{ expr }`: `Ring[u8, max_depth]`.
* A length or constant argument may be an expression over generic parameters: constant
  parameters, and `@sizeof(T)` or `@alignof(T)` of a type parameter (`[n + 1]u8`,
  `Ring[u8, {2 * n}]`, `[@sizeof(T)]u8`). Inside the generic it is part of the type as
  written. Two such types are the same when their expressions are the same tree after
  the parameters are substituted: the same operators, literals and parameters in the
  same places. So `[n + 1]T` and `[n+1]T` are one type, `[1 + n]T` is another, and a
  field `[n + 1]u8` of `Buf[n]` is `[m + 1]u8` in `Buf[m]`. When every parameter has a
  value the expression is evaluated, and the type is the ordinary one: that field of
  `Buf[3]` is `[4]u8`. Evaluating it may trap (E0806) or give a length out of range
  (E0521).
* A generic body is checked **once**, against its bounds. An operation not granted by a
  bound is an error in the generic, not in some instantiation.
* Each distinct instantiation is compiled as its own copy. Checking the generic once is
  the whole check: an instantiation can fail only because a constant expression that
  depends on an argument traps or is out of range (a length, an alignment, a lane
  index), because instantiation nests too deep, or because a kernel is dispatched on an
  executor that lacks one of its types.
* `@zero` and `@bits` on a type parameter need a bound that grants them. `Num` and its
  relatives grant `@zero`. Nothing grants `@bits`, which needs concrete types.
* **Builtin names** need no import, because the compiler itself gives them meaning:
  `Option` and `Result` (section 3), the bounds below, the interfaces `Eq`, `Ord`,
  `Hash`, `Ordering` (section 4.4), `Iter` (section 7.1) and `Format` (section 35), and the kernel
  names `Executor`, `Idx`, `Idx2`, `Idx3`, `Grid` and `GridMut` (section 27). Every other
  name is declared in the program or imported. A declaration or import of the same name
  hides a builtin name.
* Built-in bounds:

  | Bound | Grants |
  |---|---|
  | `Copy` | implicit duplication |
  | `Discard` | letting a value go out of scope unused |
  | `Send`, `Sync` | crossing task boundaries (section 20) |
  | `Owned` | holding no borrow, so a value may outlive everything around it (section 13.6) |
  | `AtomicValue` | the atomic builtins (section 15). Satisfied by the integers, `bool`, raw pointers to `Owned` types and payload-free `copy enum`s, and nothing else. Implies `Copy`, `Discard`, `Owned` and `Eq`, but not `Send` or `Sync` (a raw pointer is neither) |
  | `Int`, `SInt`, `UInt`, `Float`, `Num` | the arithmetic and comparison operators, `@zero`, literals. Only scalars satisfy them, so each implies `Copy`, `Discard`, `Owned`, `Send` and `Sync` |
  | `Fn(..)`, `FnMut(..)`, `FnOnce(..)` | calling the value. Only non-linear types satisfy `Fn` and `FnMut` (section 5.6: a closure that owns a non-`Copy` capture is only `FnOnce`), so each of those two implies `Discard` |

  With no bound, a `T` can only be moved, borrowed and passed on, and each `T` value
  must be consumed exactly once, because `T` might be linear.
* No specialization, no higher-kinded types, no variadic generics, no macros.

## 9. Compile-time evaluation

* A constant expression is evaluated by the compiler: `const` initializers, array
  lengths, `const` generic arguments, enum tag values, `when` conditions, `align(...)`.
* Any function may be called in a constant expression if its execution does not read a
  `static`'s value, enter `unsafe`, call an `extern` function, spawn, run a `par for`,
  `select` or `dispatch`, or exceed the evaluation limits. Taking the address of a static
  is allowed: it is how one static borrows another. A generic function is evaluated with
  its arguments, without being instantiated for code generation. A trap during
  evaluation is a compile error at the operation that trapped, which also names the call
  in the constant expression that led there.
* Compile-time builtins: `@sizeof(T)`, `@alignof(T)`, `@offsetof(T, field)` (each an
  `i64`),
  `@static_assert(cond, "msg")`, and the `@target` record (section 24).
* The condition of `@static_assert` is a constant expression. In a generic function it
  may depend on the type and constant parameters, and is then checked for each
  instantiation that is reached from a non-generic function, test, `const` or `static`
  of the packages being checked.
* There is no reflection over fields and no code generation inside the language.
  `ingot api --json` is the supported way for a tool to learn the shape of a program.
