# Writing Ingot: a primer for agents

Read this before writing Ingot. It is the short form of the specification, ordered by
what trips up someone who already knows Rust, Zig, Go or Swift. No model was trained on
Ingot, so do not guess from a similar language: where this primer is silent, check the
[specification](README.md) or run `ingot explain CODE`.

Status: the language is specified, and a compiler is being built. It checks names, types
and function bodies today; ownership checking and code generation come next.

## 1. One complete program

```ingot
import std.Env;
import std.fs;
import std.mem.Arena;
import std.mem.AllocErr;
import std.simd;
import std.sync.Atomic;

enum AppErr { io: from fs.IoErr, alloc: from AllocErr, usage }

/// Counts newline bytes with native-width vectors.
fn count_lines(data: []u8) -> i64 {
    let n = @lanes(simd.U8);
    let nl = @splat(simd.U8, '\n');
    var total: i64 = 0;
    var i: i64 = 0;
    while i + n <= data.len {
        total += @count(@load(simd.U8, data, i) == nl);
        i += n;
    }
    return total + @count(@load_tail(simd.U8, data, i) == nl);
}

pub fn main(env: &Env) -> void ! AppErr {
    if env.args.len != 2 { fail .usage; }

    let arena = try Arena.init(env.vm, 1 << 30);
    defer { arena.deinit(); }

    let data: []u8 = try fs.read_all(env.fs, env.args[1], &arena);

    let total = Atomic[i64].init(0);
    par for chunk in data.chunks(1 << 20) {
        total.add(count_lines(chunk), .relaxed);
    }
    @format(env.stdout, "{}\n", total.load(.relaxed)) catch _ => {};
}
```

## 2. If you know Rust, these are the differences

| Rust | Ingot |
|---|---|
| `fn f<'a>(s: &'a [u8]) -> &'a [u8]` | `fn f(s: []u8) -> []u8 from s`. No lifetime names. Say which parameters the result borrows from |
| `Drop` runs at end of scope | **Nothing runs by itself.** An owning value is *linear*: consume it or it is a compile error. Write `defer { x.deinit(); }` |
| `x as i32` truncates | `as` only for conversions that lose nothing. Else `@wrap`, `@narrow` (traps), `@sat`, `@nearest` |
| overflow wraps in release | `+ - *` **trap** on overflow, always. `+%` wraps, `+\|` saturates |
| `Result<T, E>`, `?`, `Err(e)` | `T ! E`, `try x`, `fail e;`. `return` never carries an error |
| `Some(x)`, `None`, `Option<T>` | `.some(x)`, `.none`, `?T` |
| `let mut x` | `var x`. `let` is immutable. No shadowing, ever |
| `String`, `&str`, `char` | `[]u8` only. `'a'` is an integer |
| `usize` | `i64` for every length and index |
| `S { x: 1 }` | `S.{ x: 1 }` |
| `Vec<T>`, `f::<T>(x)` | `List[T, A]`, `f.[T](x)` |
| `trait`, `impl Trait for T` | `interface`, `impl I for T` |
| a block's last expression is its value | **Blocks have no value.** Use `return`. `if`/`match` are expressions only with one expression per branch |
| `\|x\| x + n` | `fn [n](x: i64) -> i64 { return x + n; }`. Captures are listed |
| `v.len()` | `xs.len`, a field |
| `*r` | `r.*` for the whole value. `r.field` and `r.method()` work through a borrow |
| `Vec::new()` uses the global heap | every container takes an allocator: `List[u8, Heap].init(env.heap)`. Allocating calls return `! AllocErr` |
| `thread::spawn`, `tokio::spawn`, `async` | `scope s { spawn f(x) in s; }`. No `async`. Any function may block |
| `Mutex<&T>` | locks and channels hold only borrow-free (`Owned`) data |
| `unsafe { }` | `unsafe("why this is sound") { }`. The reason is required |
| unused variable is a warning | **There are no warnings.** Unused bindings and imports, and a `var` never mutated, are errors |
| names in any case | types are `UpperCamel`, everything else `lower_snake`. The lexer enforces it |
| `println!` | no global output. `@format(writer, "..", args)` needs a writer from `Env`. `@debug("..", args)` for debugging only |
| `static mut` | does not exist. A `static` must be `Sync` |

## 3. Syntax in one screen

```ingot
import std.mem;                    // a module: use as mem.Arena
import std.sync.Atomic;            // an item: use as Atomic

const limit: i64 = 64;
static hits: Atomic[i64] = Atomic[i64].init(0);

copy struct Point { pub x: f64, pub y: f64 }        // copied implicitly
struct Parser { src: []u8, pos: i64 }                // moved; holds a borrow
linear struct File { fd: i32 }                       // must be consumed

copy enum Color: u8 { red = 1, green, blue }
enum Shape { circle: f64, rect: Point, empty }      // one payload per variant

interface Area { fn area(self: &Self) -> f64; }

impl Area for Shape {
    fn area(self: &Shape) -> f64 {
        return match self.* {
            .circle(r) => 3.14159 * r * r,
            .rect(p) => p.x * p.y,
            .empty => 0.0,
        };
    }
}

fn clamp[T: Ord + Copy](x: T, lo: T, hi: T) -> T {
    if x.cmp(&lo) == .less { return lo; }
    if x.cmp(&hi) == .greater { return hi; }
    return x;
}

test "clamp keeps the middle" {
    @assert(clamp(5, 0, 10) == 5);
}
```

Statements:

```ingot
let a = f();                       // immutable, type from the initializer
var b: [64]u8 = @zero;             // mutable
_ = g();                           // discard a result on purpose
if c { .. } else if d { .. } else { .. }
if let .some(v) = lookup(k) { .. }
while i < n { .. }
loop { .. }                        // forever
for i in 0..n { .. }               // 0 to n-1.  0..=n includes n.  `step 4` allowed
for x in xs { .. }                 // xs: []T with T: Copy gives T;  xs: []mut T gives &mut T
for i, x in xs { .. }
outer: for row in rows { for v in row { if v == 0 { break outer; } } }
match x { .a => { .. } .b(v) => { .. } else => { .. } }
defer { cleanup(); }               // runs at every exit of the enclosing block
```

* Every statement ends with `;`. Braces are never optional.
* `a & b == 0` is an error: put parentheses around bitwise and shift operations when
  they are mixed with anything else.
* Conditions must be `bool`. Write `n != 0`.
* A call whose result is not `void` cannot stand alone. Use it, bind it, or `_ =` it.

## 4. Numbers

```ingot
let a: i32 = 2147483647;
let b = a + 1;                 // traps at run time: integer overflow
let c = a +% 1;                // wraps to -2147483648
let d = a +| 1;                // stays at 2147483647
let e = a as i64;              // fine: lossless
let f = @narrow(i16, a);       // traps if it does not fit
let g = @wrap(u8, a);          // modulo 256
let h = @sat(u8, a);           // 255
let k = @nearest(f32, a);      // rounds
```

* No implicit conversion between any two types. An untyped literal takes the type its
  context needs, else `i64` or `f64`.
* Indices are `i64`. Index with a `u8` by writing `table[b as i64]`.
* `/ %` trap on a zero divisor. Shifts trap when the count is out of range.
* Floats are strict IEEE: nothing is reassociated or fused. `@fma(a, b, c)` is explicit.
* A trap ends the process. It is for bugs, not for conditions a caller should handle.

## 5. Errors

```ingot
enum ParseErr { empty, bad_digit: u8 }
enum LoadErr { io: from fs.IoErr, parse: from ParseErr }      // `from` lets try wrap

fn digit(c: u8) -> u8 ! ParseErr {
    if c < '0' || c > '9' { fail .bad_digit(c); }
    return c - '0';
}

fn load(files: &Fs, path: []u8, a: &Arena) -> u8 ! LoadErr {
    let text = try files.read_all(path, a);     // IoErr is wrapped in LoadErr.io
    if text.len == 0 { fail .parse(.empty); }
    return try digit(text[0]);                  // ParseErr is wrapped in LoadErr.parse
}

let d = digit(c) catch _ => 0;                  // replace the error with a value
let e = digit(c) catch err => {                 // or run a block that must leave
    @debug("bad digit: {}", err);
    return;
};
let first = xs.first() orelse 0;                // the same for ?T
```

* A result cannot be ignored, stored in a `let` and forgotten, or discarded with `_ =`.
* `try` works when the enclosing function's error type is the same, or has a variant
  declared `from` that type.
* `errdefer { .. }` runs only when the function is failing.

## 6. Ownership

**Three kinds of type.**

| Kind | Declared | Behavior |
|---|---|---|
| copy | scalars, `&T`, `[]T`, `copy struct`, `copy enum` | duplicated freely |
| move | plain `struct`, `enum`, `&mut T`, `[]mut T` | one owner. Using it after a move is an error. May be dropped silently |
| linear | `linear struct`, every `T ! E`, `Task[R]`, owning containers | **must be consumed exactly once on every path** |

```ingot
fn first_line(files: &Fs, path: []u8, out: []mut u8) -> i64 ! IoErr {
    let f = try files.open(path);
    defer { f.close() catch _ => {}; }     // without this: E1003, `f` is not consumed
    return try f.read_until(out, '\n');
}

fn open_checked(files: &Fs, path: []u8) -> File ! IoErr {
    let f = try files.open(path);
    errdefer { f.close() catch _ => {}; }  // only on failure
    try f.check_header();
    return f;                              // on success the caller owns it
}
```

* Consuming means passing by value, returning, storing, or destructuring.
* A linear value assigned over is an error. Take the old one out with `@replace`.
* You cannot move out of a borrow or an array element. Use `@replace`, `@swap`, or an
  `?T` field with `.take()`.
* `@forget(x)` consumes without cleanup. It is listed by `ingot audit`.
* In a generic, an unbounded `T` might be linear. Add `T: Discard` to let one go, or
  `T: Copy` to duplicate it.

## 7. Borrows

`&x` shares, `&mut x` is exclusive. While something is borrowed exclusively nothing else
may touch it, and while it is shared nothing may change or move it. A borrow ends at its
last use, not at the end of the block.

Only the receiver is borrowed for you: `list.push(x)`. Every other borrow is written:
`fill(&mut buf, 0)`.

**Returning a borrow: say where it comes from.**

```ingot
fn first_word(s: []u8) -> []u8 from s { .. }
fn longest(a: []u8, b: []u8) -> []u8 from a, b { .. }
fn greeting() -> []u8 from static { return "hello"; }

impl Parser {
    // borrows the parser itself: the parser stays borrowed while the result lives
    fn pos_mut(self: &mut Parser) -> &mut i64 from self { return &mut self.pos; }

    // borrows only what the parser holds: the parser is free again at once
    fn rest(self: &Parser) -> []u8 from self.* { return self.src[self.pos..self.src.len]; }
}
```

**Storing a borrow: say where it goes.**

```ingot
fn set_input(l: &mut Lexer, text: []u8 into l) { l.src = text; l.pos = 0; }
```

Generic containers need neither clause: borrows inside a `T` follow `T`.

Common shapes:

| You want | Write |
|---|---|
| a slice of an array | `&a[0..n]` or `&mut a[0..n]` |
| a shared view of an exclusive borrow | `&r.*`, or `s.shared()` for a slice |
| two exclusive parts of one slice | `s.split_at_mut(k)`, `s.chunks_mut(n)`, or iterate `for x in s` |
| a struct that points into itself, or a graph | indices, or `Pool[T, A]` with `Handle[T]` |

## 8. Memory

No function allocates unless it was handed an allocator. Prefer an arena: allocate
freely, release everything at once.

```ingot
fn handle(req: &Request, vm: Vm, out: &mut Writer) -> void ! AppErr {
    let arena = try Arena.init(vm, 64 << 20);
    defer { arena.deinit(); }
    let words: []mut []u8 = try split_words(req.body, &arena);
    try render(words, out);
}

fn split_words(text: []u8, a: &Arena) -> []mut []u8 ! AllocErr from text, a { .. }
```

* Arena memory is an ordinary borrow of the arena, so using it after `deinit` or
  `reset` is a compile error.
* Owning containers are linear and carry their allocator in the type:

  ```ingot
  var names = List[[]u8, Heap].init(env.heap);
  defer { names.deinit(); }
  try names.push("ada");
  let all = names.items();               // []T, borrowed from the list
  ```

* Growing a list while a slice of it is alive is a compile error.
* Everything from outside (`heap`, `vm`, `fs`, `net`, `clock`, `stdout`) arrives through
  `main`'s `env` and is passed down as parameters. There are no global functions for I/O.

## 9. Concurrency

```ingot
fn fib(n: i64) -> i64 {
    if n < 20 { return fib_seq(n); }
    scope s {
        let left = spawn fib(n - 1) in s;      // a Task[i64], must be joined
        let right = fib(n - 2);
        return left.join() + right;
    }
}

fn scale(xs: []mut f32, k: f32) {
    par for x in xs { x.* = x.* * k; }         // each iteration owns its item
}
```

* A task cannot outlive its `scope`, so it may borrow from the function around it.
* What a task is given stays borrowed **until the scope ends**, even after `join`. Use a
  nested scope to get it back earlier.
* A task cannot borrow a variable declared inside its own `scope` block. Declare shared
  things before the scope.
* A function that spawns into a scope it received says so:
  `fn start(s: &Scope, jobs: &Queue into s) { spawn work(jobs) in s; }`.
* A `par for` body may read outer variables, and may not assign, move or exclusively
  borrow them. Collect results through an `Atomic`, a `Mutex`, or `par.map_reduce`.
* `Mutex[T]`, `RwLock[T]`, `Chan[T, A]` take only borrow-free `T`. Send indices,
  handles or owned buffers, not slices.
* A lock guard is linear: `var g = m.lock(); defer { g.unlock(); }`.
* Cancellation arrives as an error from blocking calls. Nothing is killed from outside.

## 10. SIMD and kernels

```ingot
let v = @load(f32x4, xs, i);                       // one bounds check for 4 lanes
let m = v > @splat(f32x4, 0.0);                    // a mask: boolx4
let w = @select(m, v, @splat(f32x4, 0.0));
@store(out, i, w);
```

* Vector types are `f32x4`, `u8x16`, `i32x8` and so on, up to 512 bits. `simd.F32`,
  `simd.U8` are the widest native ones, and `@lanes(V)` is the lane count.
* Integer vectors have no trapping `+`: write `+%` or `+|`.
* A `kernel fn` is applied to every index of a range and may run on SIMD, threads or a
  GPU: `dispatch saxpy(2.0, xs, ys) over 0..xs.len on exec.par;`. Inside one: write only
  `out[i]`, read `in[i]` or `in.at_clamped(j)`, and use only non-trapping arithmetic.
  It may call only kernel helpers: a `kernel fn` whose first parameter is not an index,
  which follows the same rules and takes and returns plain values.

## 11. `unsafe` and C

```ingot
extern "c" { fn strlen(s: *u8) -> i64; }

fn hello_len() -> i64 {
    return unsafe("the argument is a NUL-terminated literal") { strlen(c"hello") };
}
```

Raw pointers (`*T`, `*mut T`) can be made anywhere and used only in `unsafe`. Safe code
has no undefined behavior, and an `unsafe` block that needs nothing unsafe is an error.

## 12. Working with the compiler

| Step | Command |
|---|---|
| check | `ingot check --json` |
| apply the fixes it offers | `ingot fix` |
| understand a code | `ingot explain E1102` |
| see what is still borrowed at a line | `ingot query borrows src/io.ingot:41` |
| find a definition or all uses | `ingot query def FILE:L:C`, `ingot query refs FILE:L:C` |
| learn a library | `ingot api std.mem` |
| run tests | `ingot test --json` |
| see remaining checks and allocations in a hot function | `ingot cost --fn NAME` |

Every diagnostic has a stable code, a primary span, related spans with roles (`borrow`,
`move`, `later_use`, ...) and often an exact fix. Match on the code, not the message.

## 13. The errors you will meet first

| Code | What happened | What to do |
|---|---|---|
| E0104 | a name like `myVar` or `My_Type` | `my_var`, `MyType` |
| E0204 | `a & b == 0` | `(a & b) == 0` |
| E0309 | a local reuses a name in scope | pick another name |
| E0311, E0312 | unused binding; `var` never mutated | remove it or use `_`; change to `let` |
| E0401 | a value of the wrong type, including a different integer width | `x as T` if lossless, else a conversion builtin |
| E0402 | `as` would lose values | `@wrap`, `@narrow`, `@sat` or `@nearest` |
| E0413 | a call's result is dropped | bind it or write `_ =` |
| E0701 | a result used as a plain value | `try`, or `catch` |
| E0703 | `try` on an error type this function cannot return | add a `from` variant to your error enum, or `catch` and convert |
| E1001 | used after it was moved | borrow instead of moving, or restructure |
| E1003 | a linear value is not consumed on some path | `defer { x.deinit(); }`, or `errdefer` if it is returned on success |
| E1101 to E1103 | assigned, borrowed or read while a conflicting borrow is live | end the earlier borrow first: move its last use up, or copy what you need out of it |
| E1105 | something borrowed goes out of scope too early | declare it in an outer block |
| E1201 | a function returns a borrow without `from` | add `from param` |
| E1204 | a borrow is stored through a parameter | add `into param` to the argument |
| E1403 | a task borrows a variable declared inside its scope | declare it before `scope` |
| E1407 | a `par for` body changes an outer variable | use an `Atomic`, a `Mutex`, or a reduction |
| E0801 | often: a borrow inside a `Mutex` or `Chan` | store owned data or indices |

## 14. Habits that make Ingot code fast

* One arena per request or per phase. Avoid containers of containers on the heap.
* `for x in xs` and `for i in 0..xs.len` have no bounds checks. Other indexing does,
  unless an `@assert(i + k <= xs.len)` or a loop condition proves it.
* Use `+%` where wrapping is what you mean. It also has no overflow check.
* Pass small `copy` values by value and large ones by `&`.
* Put data that different tasks write into `align(@target.cache_line)` slots.
* Reach for `par for` over chunks before `spawn` per item.
* Ask `ingot cost`. Do not guess what the compiler removed.
