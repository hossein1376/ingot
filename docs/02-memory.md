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
| **view** | contains a borrow, so it is bound to the lifetime of what it borrows | `&T`, `&mut T`, `[]T`, `[]mut T`, `&dyn I`, a `*T` or `*mut T` whose `T` is a view, closures that capture a borrow or a view, and anything containing one |
| `Owned` | contains no borrow, so a value can outlive everything around it | every type that is not a view |

`&mut T` and `[]mut T` are neither `Copy` nor linear: they move, and they may be dropped.

A closure's properties follow its captures: it is `Copy` if every capture is `Copy` (a
`&mut` capture is not), linear if any capture is linear, and a view if any capture is a
borrow or a view. A type parameter that no field mentions does not make a type a view,
so a `Handle[T]` of indices stays self-contained (section 14.4).

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
  When the type has exactly one consuming function that takes nothing else and cannot
  fail, the fix inserts the `defer` that calls it. Otherwise the diagnostic lists the
  candidates, because a fix that swallows a close error is not one to apply blindly.
* A value consumed inside a `defer` block counts as consumed at scope exit. Until then it
  can be borrowed, but not moved. `errdefer` counts for the error exits only, so the
  usual pattern for "hand it to the caller on success, clean up on failure" checks.
* A linear struct is taken apart, by destructuring or by moving a field out of it, only
  in its own module. Elsewhere it can only be passed on whole.
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
* Assigning to a place that still holds a linear value is an error, because the old
  value would be lost. Take it out first with `@replace`.
* A variable moved in one branch and not the other is moved after the join. For a linear
  value that is an error (E1003, at its declaration), because one path would leak it.
* A temporary, a value that is used without being bound, lives until the end of the
  statement that contains it. In the header of an `if`, `match`, `while` or `for` that
  is the end of the whole statement, and for a loop, of each iteration. A borrow of a
  temporary that is still live after that is E1105; bind the value with `let` to keep
  it. A linear temporary that is not consumed by then is E1003 at the expression that
  made it.
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

**Reborrowing.** A place of type `&mut T` or `[]mut T` written as a call argument, a
method receiver, a `spawn` or `dispatch` argument or the iterand of a `for` is lent for
that use, as if `&mut p.*` were written, and is usable again afterwards. For a `spawn`
the use lasts until the scope ends. Any other by-value use of such a place (a binding, a
`return`, a store, a capture) moves it, and moving it out of a borrow is E1002. Writing
`&mut r.*` (or `s[..]` for a slice) reborrows explicitly. Through a shared borrow, an
exclusive borrow stored in the referent can only be reborrowed as shared.

**Statics.** A `static` is never the subject of a loan. A borrow of one, or of a place
inside one, has no origins, as `from static`. `&mut` of a static is E0610 and moving out
of one is E1002.

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

Inside a body, three rules build the sets:

* `&p` and `&mut p` give a value that holds the new loan. If `p` is reached through a
  borrow `r`, the value also holds the origins of `r`.
* Copying or moving a view carries its origin set along. Where control flow joins, the
  sets are united.
* **Storing a view through a borrow widens what the borrow points at.** After
  `r.* = v` or `r.field = v`, the origins of `v` are added to the origin set of every
  variable that `r` may borrow from, transitively. This is what stops a short-lived
  borrow from being left behind in a long-lived container.

### 13.1 `from`: where a returned borrow comes from

A function whose return type is written with a borrow in it (`&`, `[]`, or a view
struct) must name its sources. A return type that mentions `Self`, an associated type or
a type parameter may name them, since it may hold a borrow in some implementation
(`Iter.next` is declared `from self.*`); an implementation whose type holds no borrow
there omits the clause.

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

A view has origin `p.*` only if it was **copied or moved out** of what `p` refers to. A
reborrow made *through* `p` has origin `p`, because it is valid only while `p` is:
`&p.field`, `&mut p.field`, or a slice of an exclusive slice stored in `p.*`. Without
this distinction a method could hand out the same `[]mut u8` twice. Slicing a *shared*
slice stored in `p.*` copies it first, which is why `rest` above is `from self.*`.

The body is checked against the clause: returning a borrow whose origin is not covered
is an error, and the fix adds the missing source. The clause covers the whole result
`T ! E`: a borrow that leaves through `fail e` is checked exactly like one that leaves
through `return v`, and a signature whose `T` or `E` is written with a borrow must have
a clause.

### 13.2 `into`: where a stored borrow goes

A function may store a view argument inside something it was given by `&mut` or `[]mut`, or start
a task with it in a scope it was given (section 18), but only if the parameter says so.

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
  For each store of a view that reaches a parameter `p` (by the widening rule above),
  the stored origins must be `p.*` itself, `static`, a parameter declared `into p`, or
  a value whose type is a type parameter that the type of `p` mentions. A loan on one
  of the callee's own locals can never be stored through a parameter.
* **Caller.** The result's origin set is the union named by `from`. `into p` adds the
  argument's origins to the variable behind `p`. For each type parameter `T`, the
  origins of every argument whose type mentions `T` are united, and that union is added
  to the variable behind every `&mut` argument whose type mentions `T`, and to the
  result if its type mentions `T`. Then the rules of section 12 apply to every loan
  that is still live.
* A view struct has **one** origin set for all its borrow fields together. A struct
  holding borrows of two different things is treated as borrowing both for as long as
  either is needed.

### 13.5 Function values and interface methods

* An interface method is declared with its `from` and `into` clauses like any function.
  An implementation may declare the same clauses or fewer, never more, so a call
  through the interface is checked against the interface alone.
* A `fn(...)` type and an `Fn`, `FnMut` or `FnOnce` bound have unnamed parameters and
  carry no clauses. A view returned by a call through one is assumed to hold the
  origins of every argument and of the callable itself, which covers a closure's
  captures. A function with an `into` clause does not convert to a `fn` type and does
  not satisfy an `Fn` bound.

### 13.6 Interior mutability holds only owned data

Everything above rests on one fact: a view is stored into a place only through an
exclusive borrow, and the checker follows exclusive borrows. A type whose contents can
be replaced through a *shared* borrow breaks that. With a `Mutex[[]u8]`, one task could
store a slice that another task reads back and keeps after the slice's source is gone,
and no analysis of one function at a time can see it.

So every type with interior mutability bounds its contents with `Owned`:
`Mutex[T]`, `RwLock[T]`, `Chan[T, A]` and `Once[T]` accept no view types. Borrowed data
reaches other tasks as arguments to `spawn` and as the items of `par for`, where the
flow is written down. Channels and locks carry owned values, indices and handles.
A `Scope` is the one built-in exception, and `into` covers it (section 18). An arena is
not such a type: allocating through `&Arena` hands out new memory and offers no way to
read back what another holder stored.

### 13.7 What this does not express

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

Allocation failure is an error value, `AllocErr`, never a trap. A fixed-size arena
filling up is a normal, recoverable event, and the `try` on an allocating call is also
what makes allocation visible at the call site.

What the operating system does when physical memory runs out is outside the language.
Windows refuses the commit, which arrives as `AllocErr`. Linux and macOS may accept
memory they cannot back and end the process later.

### 14.2 Arenas

```ingot
fn handle(req: &Request, vm: Vm, out: &mut Writer) -> void ! AppErr {
    let arena = try Arena.init(vm, 64 << 20);     // reserves 64 MiB of address space
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
  `extern` function or call through an `extern fn` pointer, use the builtins below, and
  write `unsafe impl Send`/`Sync`. Creating a raw pointer is safe (`&x as *T`, a slice's
  `.ptr`); using it is not.

  | Builtin | Signature |
  |---|---|
  | `@uninit(T)` | a `T` whose bytes are unspecified |
  | `@assume(c)` | `c: bool`; `void` |
  | `@unchecked_index(s, i)` | the place `s[i]`, with no bounds check |
  | `@unchecked_slice(s, lo, hi)` | `s[lo..hi]`, with no bounds check |
  | `@unchecked_add`, `@unchecked_sub`, `@unchecked_mul` `(a, b)`, `@unchecked_neg(a)` | as the operator, on one integer type, with no overflow check |
  | `@unchecked_div`, `@unchecked_rem` `(a, b)`, `@unchecked_shl`, `@unchecked_shr` `(x, n)` | as the operator, with no check |
  | `@unchecked_narrow(T, x)` | as `@narrow`, with no check |
  | `@unchecked_load(V, s, i)`, `@unchecked_store(s, i, v)` | as `@load` and `@store` (section 26.3), with no check |
  | `@slice_from_raw(p, len)` | `p: *T` gives `[]T`, `p: *mut T` gives `[]mut T`; `len: i64`, at most 2^62 |
  | `@borrow_from_raw(p)` | `p: *T` gives `&T`, `p: *mut T` gives `&mut T` |

  The `@unchecked_*` list is closed: one per check kind of section 31.
* `unsafe` does not turn off any check on safe constructs. Borrows are still checked and
  `a[i]` is still bounds-checked inside the block.
* An `unsafe` block that contains nothing that needs it is an error, so that the list
  `ingot audit` prints is exactly the list of places that matter. An operation needs only
  the innermost `unsafe` block around it, so a block that only encloses another one is
  also an error.
* The body of an `unsafe fn` is not an `unsafe` context: its unsafe operations sit in
  `unsafe` blocks like any others. The region of an `unsafe` block is lexical, so a
  closure written inside one is inside it.
* An `unsafe fn` documents its preconditions in its doc comment. `ingot api` shows them.
* An `unsafe fn` can only be called. It cannot be used as a value, because a `fn` pointer
  to it could then be called without `unsafe`. A closure or a safe function that calls
  it inside an `unsafe` block is the value to pass instead.
* A module that contains `unsafe` is responsible for making its `pub` surface safe for
  every possible safe caller.
* A borrow made from a raw pointer has no origins. The checker accepts it wherever a
  `from` or `into` clause is needed, and keeping it valid for as long as it is used is
  the block's obligation.
* A type that builds interior mutability out of `unsafe` must bound its contents with
  `Owned` (section 13.6), and must not be `Sync` unless it synchronizes every access.
* A type that keeps a `T` behind an untyped pointer has a field whose type mentions `T`
  (a `[0]T` field costs nothing), so that its properties follow `T`.

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
| no memory for a task record or a task stack | `spawn`, `par for`, parking a task (section 23) |
| a repeated index | `@scatter` (section 26.3) |
| a dispatch range that a buffer does not cover | `dispatch` (section 27.2) |
| blocking, `spawn` or `par for` outside a task | an `export fn` called on a foreign thread (section 23) |

A trap is never undefined behavior and never continues. Each trap has a stable code
(`T01` to `T17`) that appears in its message.

### 16.2 Undefined behavior

Safe code has none. The following are undefined, and each is reachable only from inside
an `unsafe` block or from foreign code:

1. Dereferencing a raw pointer that is null, dangling, misaligned or outside its object.
2. Creating a borrow or slice with `@borrow_from_raw`/`@slice_from_raw` that breaks
   section 12: a `&mut` that aliases a live borrow, or any borrow that outlives its
   memory.
3. Reading memory obtained from `@uninit` before writing it.
4. Producing an invalid value: a `bool` other than 0 or 1, an enum tag with no variant,
   a null `&T` or slice pointer.
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
