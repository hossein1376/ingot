# Ingot v1 specification, part 2: memory and safety

The memory model has four parts: values are **owned** and move, **borrows** are checked
against the place they came from, **linear** values must be consumed, and **allocation**
is always through an explicit allocator value. None of it needs a garbage collector, a
reference count or a run-time check, with one exception: generational handles
(section 14.4), which are opt-in and cost one compare.

All checking in this part is done **one function body at a time**, using only the
signatures of the functions it calls.

## 10. Type properties

| Property | Meaning | Holds for |
|---|---|---|
| `Copy` | may be duplicated by assignment or passing | scalars, vectors, raw pointers, `fn` pointers, `&T`, `[]T`, `copy struct`/`copy enum`, and arrays, tuples and `?T` of `Copy` types |
| `Discard` | may go out of scope without being consumed | every type that is not linear |
| **linear** (not `Discard`) | must be consumed exactly once on every path | `linear struct`, `T ! E`, `Task[R]`, and any array, tuple, struct or enum containing a linear type |
| **view** | contains a borrow, so it is bound to the lifetime of what it borrows | `&T`, `&mut T`, `[]T`, `[]mut T`, `&dyn I`, closures with `&` captures, and anything containing one |

`&mut T` and `[]mut T` are neither `Copy` nor linear: they move, and they may be dropped.

A value is **consumed** by moving it: passing it by value to a function, returning it,
storing it into a place, or destructuring it. For a linear value that is the only way
to end its life. The author of a linear type provides the functions that take it by
value and take it apart:

```ingot
linear struct File { fd: i32 }

impl File {
    pub fn close(self: File) -> void ! IoErr {
        let File.{ fd } = self;                 // the linear value ends here
        try sys.close(fd);
    }
}

fn first_line(fs: &Fs, path: []u8, out: []mut u8) -> i64 ! IoErr {
    let f = try fs.open(path);
    defer { f.close() catch _ => {}; }          // without this line: error, `f` is not consumed
    return try f.read_until(out, '\n');
}
```

* A linear value that is still owned at the end of its scope, on any path, is an error.
  The fix inserts the `defer`.
* A value consumed inside a `defer` block counts as consumed at scope exit. Until then it
  can be borrowed, but not moved. `errdefer` counts for the error exits only, so the
  usual pattern for "hand it to the caller on success, clean up on failure" checks.
* `@forget(x)` consumes any value without running anything. It is safe, because leaking
  is not a memory-safety problem, and `ingot audit` lists every use.
* The compiler never inserts a call to release anything. If a scope ends and no cleanup
  code is written there, none runs.

## 11. Moves and initialization

* Reading a non-`Copy` variable by value moves it. Using a moved variable is an error.
  Moving out of one field leaves the others usable and the whole unusable.
* A value cannot be moved out of a borrow, out of an array or slice element, or out of a
  `static`. Use `@swap(&mut a, &mut b)`, `@replace(&mut place, new_value)` (returns the
  old value) or an `?T` field with `.take()`.
* A `var` that was moved from can be assigned again and is then usable.
* A variable moved in one branch and not the other is moved after the join. For a linear
  value that is an error at the join, because one path would leak it.
* Uninitialized memory does not exist in safe code. `@uninit(T)` is an `unsafe` builtin.

## 12. Borrows

`&p` creates a shared borrow of place `p` and `&mut p` an exclusive one. Each borrow
expression creates a **loan** on that place.

**Exclusivity.** While a loan is live:

| Loan | The borrowed place and everything under it may be |
|---|---|
| shared | read, and borrowed shared again. Not written, moved or borrowed exclusively |
| exclusive | used only through that borrow |

**Liveness.** A loan is live from the borrow expression until the last use of any value
that may hold it (section 13 defines "may hold"). It does not extend to the end of the
lexical scope.

**Granularity.** Loans are tracked per place. Borrowing `s.a` exclusively leaves `s.b`
free. Array and slice elements are not distinguished: `a[i]` and `a[j]` are the same
place to the checker. Disjoint parts of a slice are obtained through library functions
that prove disjointness once (`split_at_mut`, `chunks_mut`, iteration over `[]mut T`).

**Reborrowing.** Passing a `&mut T` variable to a function, or writing `&mut r.*`, lends
it for the duration of that use and the original is usable again afterwards.

**Call arguments.** All arguments of a call are evaluated left to right first. Borrows
written directly as arguments, and the automatic receiver borrow, become active when the
call begins. `list.push(list.len())` is accepted.

**Lifetime.** A place cannot be moved, reassigned as a whole, or go out of scope while a
loan on it is live.

## 13. Origins: borrow flow across functions

Ingot has no lifetime parameters. Instead, each value of a view type has an **origin
set**: the loans it may hold. Within a function body the checker computes origin sets by
following assignments. At a call it uses the callee's signature, which states the flow
in three ways.

### 13.1 `from`: where a returned borrow comes from

A function whose return type is written with a borrow in it (`&`, `[]`, or a view
struct) must name its sources.

```ingot
fn first_word(s: []u8) -> []u8 from s { ... }

impl Parser {                                       // struct Parser { src: []u8, pos: i64 }
    fn rest(self: &Parser) -> []u8 from self.* { return self.src[self.pos..self.src.len]; }
    fn pos_mut(self: &mut Parser) -> &mut i64 from self { return &mut self.pos; }
}

fn longest(a: []u8, b: []u8) -> []u8 from a, b { ... }

fn greeting() -> []u8 from static { return "hello"; }
```

| Source | The result may hold | Effect at the call |
|---|---|---|
| `p` | the borrow `p` itself, and anything reachable through it | the argument's loans stay live while the result is |
| `p.*` | only borrows *stored inside* what `p` refers to | the loan on the argument itself ends with the call. Only the loans it contained stay live |
| `static` | borrows of statics and literals | nothing stays live |

So after `let r = parser.rest();` the parser may be mutated or dropped while `r` is in
use, because `r` borrows the source text and not the parser. After
`let p = parser.pos_mut();` the parser is exclusively borrowed until `p` is dead.

The body is checked against the clause: returning a borrow whose origin is not covered
is an error, and the fix adds the missing source.

### 13.2 `into`: where a stored borrow goes

A function may store a view argument inside something it was given by `&mut`, but only
if the parameter says so.

```ingot
impl Lexer {
    fn set_input(self: &mut Lexer, text: []u8 into self) { self.src = text; self.pos = 0; }
}
```

At the call, the origins of `text` are added to the origin set of the lexer variable, so
the text must outlive the lexer's last use. Without `into`, a view parameter cannot be
stored anywhere that outlives the call.

### 13.3 Type parameters carry their own flow

When a signature mentions a type parameter `T`, borrows hidden inside `T` flow wherever
`T` appears, with no annotation.

```ingot
impl[T, A: Allocator] List[T, A] {
    pub fn push(self: &mut List[T, A], item: T) -> void ! AllocErr { ... }
    pub fn pop(self: &mut List[T, A]) -> ?T { ... }
    pub fn get(self: &List[T, A], i: i64) -> &T from self { ... }
}
```

For `List[[]u8, Heap]`, `push` adds the item's origins to the list, `pop` returns a
value with the list's origins (and does not keep the list borrowed), and `get` returns a
borrow of the list whose pointee has the list's origins. This is the one place flow is
derived instead of written, because a generic body cannot tell a view `T` from any
other `T`.

### 13.4 What the caller and the callee each check

* **Callee.** For each returned view, its origins must be covered by the `from` clause.
  For each store of a view through a `&mut` parameter `p`, the stored origins must be
  `p.*` itself, `static`, or a parameter declared `into p`.
* **Caller.** The result's origin set is the union named by `from`. `into p` and the
  type-parameter rule add origins to the argument variable. Then the rules of section 12
  apply to every loan that is still live.
* A view struct has **one** origin set for all its borrow fields together. A struct
  holding borrows of two different things is treated as borrowing both for as long as
  either is needed.

### 13.5 What this does not express

* A struct that borrows from itself. Use indices or handles (section 14.4).
* A function that returns a borrow whose source depends on run-time data in a way more
  precise than "one of these parameters".
* A long-lived graph of borrows with cycles. Use an arena with indices, or handles.

## 14. Allocation

### 14.1 Allocators are values

No function allocates unless it was given an allocator. The interface:

```ingot
pub interface Allocator: Copy {
    fn alloc(self: Self, size: i64, align: i64) -> *mut u8 ! AllocErr;
    unsafe fn free(self: Self, ptr: *mut u8, size: i64, align: i64);
    unsafe fn resize(self: Self, ptr: *mut u8, old: i64, new: i64, align: i64) -> bool;
}
```

| Allocator | Type | Notes |
|---|---|---|
| General heap | `Heap` (zero-sized) | obtained from `env.heap`. Size-class allocator with per-worker caches |
| Arena | `&Arena` | bump allocation. `free` does nothing. Everything is released by `reset` or `deinit` |
| Fixed buffer | `&FixedBuf` | bump allocation inside a caller-provided `[]mut u8`, typically on the stack |
| Dynamic | `&dyn Allocator` | when the choice is made at run time. Costs an indirect call |

Allocation failure is an error value, `AllocErr`, never a trap. A fixed-size arena
filling up is a normal, recoverable event, and the `try` on an allocating call is also
what makes allocation visible at the call site.

What the operating system does when physical memory runs out is outside the language.
Windows refuses the commit, which arrives as `AllocErr`. Linux and macOS may accept
memory they cannot back and end the process later.

### 14.2 Arenas

```ingot
fn handle(req: &Request, vm: Vm, out: &mut Writer) -> void ! AppErr {
    var arena = try Arena.init(vm, 64 << 20);     // reserves 64 MiB of address space
    defer { arena.deinit(); }
    let words: []mut []u8 = try split_words(req.body, &arena);
    try render(words, out);
}                                                  // one munmap, no per-object work

fn split_words(text: []u8, a: &Arena) -> []mut []u8 ! AllocErr from text, a { ... }
```

* An `Arena` reserves a contiguous range of virtual address space once and commits pages
  as it grows, so allocations never move and never chain. Reserve-then-commit is the
  native model on Windows and is a plain mapping on macOS and Linux.
* `a.alloc(value)` gives `&mut T from a`. `a.alloc_slice(n, fill)` gives
  `[]mut T from a`. They are ordinary borrows of the arena.
* `arena.reset()` and `arena.deinit()` take the arena by `&mut` and by value. Both are
  rejected while anything allocated from it is still live, by the rules of section 12.
  Use-after-free of arena memory is a compile error.
* `Arena` is `Send` and not `Sync`: one task allocates from it at a time. Parallel code
  gives each task its own arena (`par.Arenas`).
* An arena does not run cleanup for what was allocated in it, so linear values cannot be
  moved into arena memory without being consumed later like any other linear value.

### 14.3 Owning containers

`Box[T, A]`, `List[T, A]`, `Map[K, V, A]`, `Str[A]` and the rest own their memory and
are linear. Each stores its allocator, which is zero bytes for `Heap` and one pointer
for `&Arena`, so the same allocator that produced the memory is the one that frees it.

```ingot
var names = List[[]u8, Heap].init(env.heap);
defer { names.deinit(); }
try names.push("ada");
```

* `deinit(self)` requires the element type to be `Discard`. For linear elements,
  `deinit_with(self, f)` consumes each element with `f`. Nested ownership therefore
  shows its teardown cost in the source, which is the intended pressure toward arenas.
* A container whose allocator is `&Arena` is a view of that arena. Its `deinit` compiles
  to nothing.
* `list.items()` gives `[]T from self`, and `list.items_mut()` gives `[]mut T from self`.
  Indexing is done on the slice. Growing the list while a slice of it is live is an
  exclusivity error, which is the iterator-invalidation bug caught at compile time.

### 14.4 Shared and cyclic structures

| Need | Tool | Cost |
|---|---|---|
| A tree or DAG that dies all at once | arena, with `&T` links created in order | none |
| A graph with cycles, or nodes removed individually | `Pool[T, A]` with `Handle[T]` | a generation compare on each access |
| Shared ownership across tasks | `Arc[T, A]` | atomic increment and decrement, both written out |

* `Handle[T]` is a `Copy`, self-contained pair of slot index and generation.
  `pool.get(h)` gives `?&T`: a stale handle yields `.none`, never another object's data.
* `Arc[T, A]` is linear. `a.clone()` makes another owner and `a.release()` gives one up.
  The contents are reached by `&T`, so mutation needs a `Mutex` or an atomic inside.
* There is no non-atomic reference-counted type and no single-threaded interior
  mutability type in the standard library apart from the arena.

## 15. `unsafe`

```ingot
unsafe("ptr came from alloc(len) above and len bytes were initialized by read()") {
    out = @slice_from_raw(ptr, len);
}
```

* `unsafe("reason") { ... }` requires the string. It states the invariant the block
  relies on, and `ingot audit` prints every block with its reason.
* Inside, and only inside, code may: dereference a raw pointer, call an `unsafe fn` or an
  `extern` function, use `@uninit`, `@slice_from_raw`, `@borrow_from_raw`,
  `@unchecked_index`, `@unchecked_add` and the other `@unchecked_*` builtins, use
  `@assume(cond)`, read a slice's `.ptr`, and write `unsafe impl Send`/`Sync`.
* `unsafe` does not turn off any check on safe constructs. Borrows are still checked and
  `a[i]` is still bounds-checked inside the block.
* An `unsafe fn` documents its preconditions in its doc comment. `ingot api` shows them.
* A module that contains `unsafe` is responsible for making its `pub` surface safe for
  every possible safe caller.

## 16. The complete list of run-time failures

### 16.1 Traps (safe code)

| Trap | Where |
|---|---|
| index or slice bounds | `a[i]`, `s[lo..hi]`, vector loads and stores |
| integer overflow | `+ - *`, unary `-`, `@narrow` |
| division by zero, `MIN / -1` | `/ %` |
| shift count out of range | `<< >>` |
| failed assertion | `@assert`, `@panic`, `@unreachable` |
| stack exhaustion | function entry (guard page; large frames are probed) |
| deadlock of every task | the scheduler (section 23) |

A trap is never undefined behavior and never continues.

### 16.2 Undefined behavior

Safe code has none. The following are undefined, and each is reachable only from inside
an `unsafe` block or from foreign code:

1. Dereferencing a raw pointer that is null, dangling, misaligned or outside its object.
2. Creating a borrow or slice with `@borrow_from_raw`/`@slice_from_raw` that breaks
   section 12: a `&mut` that aliases a live borrow, or any borrow that outlives its
   memory.
3. Reading memory obtained from `@uninit` before writing it.
4. Producing an invalid value: a `bool` other than 0 or 1, an enum tag with no variant,
   a null `&T`.
5. A false `@assume`, or an `@unchecked_*` operation whose checked form would trap.
6. A data race through raw pointers, or a wrong `unsafe impl Send`/`Sync`.
7. Calling an `extern` function with a signature that does not match its definition, or
   breaking the documented precondition of an `unsafe fn`.
8. Freeing memory with a different allocator, size or alignment than it was allocated
   with.

### 16.3 What is safe but still wrong

The language does not prevent memory leaks through `@forget` or reference cycles in
`Arc`, deadlock between some but not all tasks, or logic errors. `ingot audit` reports
`@forget`, `unsafe` and `@debug` uses so they can be reviewed.
