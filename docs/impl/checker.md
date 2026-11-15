# The ownership checker

Sections 10 to 13 and 18 to 20 of the specification say which programs are accepted.
This file gives one way to decide it: four dataflow analyses over the CFG of
[pipeline.md](pipeline.md) section 5, run on one function at a time, using only the
signatures of the functions it calls.

An implementation may use a different algorithm. It must accept and reject the same
programs, and the conformance suite is the test of that. Where this file and the
specification disagree, the specification is right and this file has a bug.

## 1. What the checker guarantees

The checker exists to keep these invariants. They are the definition of "sound" for it.

| | Invariant |
|---|---|
| I1 | At every point, the borrows a view value actually holds are a subset of its origin set |
| I2 | While an exclusive loan is live, the memory it covers is reachable only through values that hold it |
| I3 | A place with a live loan is not moved, not assigned as a whole, and its storage does not end |
| I4 | On every path, every linear value is moved exactly once before its storage ends |
| I5 | A value handed to another task is `Send`, a borrow shared between tasks points at a `Sync` type, and every loan given to a task is live until the wait of its scope is over |
| I6 | A function's `from` and `into` clauses, together with type-parameter flow, describe every way its body lets a borrow leave through its result or through its parameters |

I6 is what makes checking one function at a time sufficient: a caller reasons about a
callee from the signature alone.

## 2. Vocabulary

* A **path** is a local followed by projections: `.f` (field or tuple index), `[_]`
  (any element), `.*` (dereference of a borrow), `.v` (payload of variant `v`).
  `root(π)` is its local. `derefs(π)` is the number of `.*` in it. Indexing or slicing a
  slice counts as a dereference of the slice. A path may also start at a `static`; such
  a path takes no loans, and a borrow of it has the empty origin set (§12).
* Two paths **overlap** if one is a prefix of the other. Different fields of one struct
  do not overlap. `[_]` overlaps `[_]`.
* A **loan** is `(id, path, kind, point)` with `kind` shared or exclusive. A loan is
  **on local storage** if its path has no dereference. Such a loan dies with its root
  local.
* An **origin** is one of: a loan; `Param(p)` for a parameter `p`; `Inner(p)`, the
  borrows stored inside what `p` refers to; `Self(s)` for a scope parameter. `static`
  needs no representation: it is the empty set.
* Every local has one **origin set** `O(x)` at each point. It covers all borrows
  anywhere inside the local, and what can be reached through it. For a local of a
  self-contained type it starts empty and grows only by widening (4.4): an `Arena` that
  hands out `&mut T` into its own memory must carry the origins of what is stored there,
  or a borrow of a dead local could be stored into the arena and read back. This is the
  "one origin set per view value" of §13.4, extended to every local. A type that is or
  may be a view includes a type parameter without `Owned` and an associated type such as
  `I.Item`.
* `inner(S)` is what can be read out through the borrows in `S`:

  | Element of `S` | Contributes |
  |---|---|
  | a loan on path `λ` | the origin set of a read of `λ` (§4.3) at the current point: `O(y)` for `λ = y` with no dereference, and `inner` applied once per dereference otherwise, so a loan on `p.*` contributes `Inner(p)`, not `Param(p)` |
  | `Param(p)` or `Inner(p)` | `Inner(p)` |

  `inner*(S)` is `S` closed under `inner`.

## 3. Analysis A: moves and linear values

A forward dataflow. The state maps each path that is ever moved, and each local, to one
of **init**, **moved**, **maybe** (moved on some path). The join of *init* and *moved* is
*maybe*. For each path of enum type, the state also holds the set of variants the path
may hold. It starts as every variant, and the join is the union.

Transfer:

| Statement | Effect |
|---|---|
| storage start of `x` | `x` is *moved* (it holds nothing yet). A parameter and a `scope` local arrive holding their value: *init* |
| `π = ...` | `π` and everything under it become *init*. If the value is a variant aggregate of variant `v`, `π` may hold only `v`; otherwise it may hold every variant |
| the variant record `π is v` (pipeline.md §5) | `π` may hold only `v` |
| the variant record `π is not v` | `π` may no longer hold `v` |
| a use of `π` marked **move** | `π` becomes *moved*. Everything under it too |
| a use marked **copy**, or a borrow | no change |

**The payload of a variant that a path cannot hold is no value.** The checks below
ignore `π.w` when `π` may not hold `w`, and an enum path stands for its payloads: an
enum value whose payloads are moved, absent or not linear holds nothing linear. The
exception is `T ! E`, which is linear in itself (§10 of the specification): each of its
two variants is an obligation, whatever `T` and `E` are, until its payload is moved.
That is why lowering moves the payload of the chosen variant of a `T ! E` (pipeline.md
§5). So
`match o { .some(t) => t.release(), .none => {} }` consumes a linear `o: ?Token` on both
arms, and so does `if let .some(t) = o { t.release(); }`, whose other path holds `none`.
`o` stays usable in the `.none` arm. `let o: ?Token = .none;` needs no consuming,
and a `?Token` returned by a call may hold either variant, so it must be consumed.

Checks:

| At | Condition | Error |
|---|---|---|
| any read, borrow or move of `π` | `π`, a prefix of it, or (for a whole read) anything under it is *moved* or *maybe* | E1001. `move`: the moving statements that reach here. `loop` if one of them is later in the same loop |
| a move out of a path with a dereference, an element, or a `static` | always | E1002 |
| `π = ...` | `π` or something under it has linear type and is *init* or *maybe* | E1004 |
| storage end of `x` | some path under `x` has linear type and is *init* or *maybe* | E1003, reported once per local at its declaration, with one `scope_end` per exit that reaches a failing storage end |

Notes:

* "Linear type" includes a type parameter without `Discard`. The fix for that case is
  the bound, not a `defer`.
* Deferred blocks were copied to every exit by lowering, so a value consumed in a
  `defer` is *moved* at each storage end that follows the copy. If the value is also
  moved earlier on that path, the copy's use reports E1005 instead of E1001, with the
  `defer` role pointing at the deferred block.
* **Exclusive borrows passed on are reborrowed, not moved.** Lowering turns an argument,
  a receiver, a `spawn` or `dispatch` argument or a `for` iterand that is a place of type
  `&mut T` or `[]mut T` into a two-phase reborrow `&mut p.*` (§12). Any other by-value
  use of such a place (binding, returning, storing, capturing) is a move, and a move out
  of a dereference is E1002.
* `@replace(&mut p, v)` and `@swap(&mut a, &mut b)` leave their places *init*; they are
  how a value is moved out from behind a borrow. `p.take()` is `@replace(&mut p, .none)`.
* A linear temporary (§11) is a local whose storage ends at the end of its statement, so
  E1003 for it is reported at the expression that made it.
* A struct with a linear field that is partly moved is consumed field by field. Outside
  the struct's module that is E1006.

## 4. Analysis B: origin sets

A forward dataflow computing `O(x)` for every view-typed local. Sets only grow at joins
(union). Transfer functions are below. `S(e)` is the origin set of the value of `e`.

### 4.1 Entry

For each parameter `p` whose type is or may be a view type: `O(p) = { Param(p) }`. A
scope parameter gets `{ Self(p) }`.

### 4.2 Borrows

For `&π` or `&mut π` with root `x`:

| `derefs(π)` | New loan | Origin set of the result |
|---|---|---|
| 0 | yes, on `π` | `{ loan }` |
| 1 or more, every dereference through a shared borrow | no | `O(x)`, and `inner*(O(x))` if `derefs(π) >= 2` |
| 1 or more, some dereference through an exclusive borrow | yes, on `π`, of the kind being taken | `{ loan } ∪ O(x)`, and `inner*(O(x))` if `derefs(π) >= 2` |

The middle row is why a reborrow of shared data never restricts the borrow it came
from: shared borrows are `Copy`, and what protects the data is the original loan, which
the new value carries in `O(x)`.

The last row is the rule of §13.1 that a reborrow through `p` has origin `p`: for a
parameter `p`, `O(p)` is `{ Param(p) }`, never `Inner(p)`.

### 4.3 Reads

A copy or move of path `π` with root `x`, where the value has a view type:

| `derefs(π)` | Origin set of the value |
|---|---|
| 0 | `O(x)` |
| 1 or more | `inner(O(x))`, closed once more per further dereference |

So a view copied out of what a parameter refers to has origin `Inner(p)`, which is the
`p.*` of a `from` clause.

### 4.4 Stores

`π = v` where `v` has a view type, `S = S(v)`, `x = root(π)`:

* `derefs(π) = 0` and `π` is the whole local: `O(x) := S`. (A strong update. Analysis C
  has already rejected the assignment if a loan on `x` is live.)
* `derefs(π) = 0` otherwise: `O(x) := O(x) ∪ S`.
* `derefs(π) >= 1`: the value lands in memory that `x` only points at. **Widen** every
  place it may land in: for each origin `o` in `O(x)`:

  | `o` | Action |
  |---|---|
  | a loan with path `λ` | perform the store `λ' = v` recursively, where `λ'` is `λ` with the remaining projections. In the common case `λ` has no dereference and this is `O(root(λ)) ∪= S` |
  | `Param(p)`, `Inner(p)` | every element of `S` must be allowed into `p` (section 6.2), else E1204 or E1205 |
  | `Self(p)` | the same, for a scope parameter |

**The kill rule.** When a place `ρ` of borrow type with no dereference in its path (a
local `r`, or a field such as `it.cur`) is assigned as a whole, every loan whose path
starts with `ρ.*` is removed from every origin set first. After the assignment
`r` no longer leads to the old referent, and values that reborrowed through the old
`r` are protected by the origins of the old `r`, which they also hold. This is what
accepts a cursor loop:

```ingot
var cur: &mut Node = head;
while let .some(next) = &mut cur.next {     // loan on cur.*.next, held by `next`
    cur = next.get_mut();                   // kills that loan, then O(cur) := O(next)
}
```

### 4.5 Calls

For `dest = f(a_1, ..., a_n)`, after the arguments are evaluated. `A_i = S(a_i)`.

1. **`into`.** For each parameter `x` declared `into p`: store `A_x` through `a_p`, by
   the widening rule of 4.4, treating `a_p` as a path with one dereference. If `p` is a
   scope, the store is `O(s) ∪= A_x` for the scope local behind `a_p`, or a check
   against `Self` if `a_p` is itself a scope parameter.
2. **Type-parameter flow.** For each type parameter `T` of `f`, let `U_T` be the union,
   over every parameter `q` whose type mentions `T`, of:
   `A_q` if `T` occurs in the type of `q` outside any borrow, and `inner⁺(A_q)` if it
   occurs inside one. `inner⁺(S)` is `inner*(inner(S))`: what is hidden inside the
   borrow, without the borrow's own origins (its loan, `Param(q)`), which are not a `T`. Then for every parameter `p` passed by `&mut` or `[]mut` whose type mentions
   `T`, store `U_T` through `a_p` as in step 1.
3. **Result.** `S(dest)` is the union of:
   * for each `from p`: `A_p ∪ inner*(A_p)`;
   * for each `from p.*`: `inner(A_p)`;
   * for each type parameter `T` that the result type mentions: `U_T`;
   * nothing for `from static`.
4. A call through a `fn` value, a closure or `&dyn Fn` has no clauses (§13.5): the
   result's set is the union of all `A_i ∪ inner*(A_i)` and the set of the callable.
5. `@slice_from_raw`, `@borrow_from_raw` and a raw pointer cast give the empty set.
6. Builtins that move views: the result of `@replace(&mut p, v)` has the origin set of a
   read of `p` before the store, and the store of `v` is a widening store through the
   borrow (4.4). `@swap(&mut a, &mut b)` is two such stores. `@forget(x)` is a move of
   `x`. `@zero(T)` and `@bits(T, x)` give the empty set.
7. **Associated types.** In type-parameter flow (step 2 and §13.4), a type that mentions
   `T.Item` mentions `T`. A function whose result mentions `Self`, an associated type or
   a type parameter may carry a `from` clause (§13.1): `it.next()` under `I: Iter`
   gives `inner(A_it)` by `from self.*`.

### 4.6 Closures

Creating a closure is an aggregate whose fields are the captures. A `&x` or `&mut x`
capture is a borrow as in 4.2. `O(closure)` is the union of the captures' sets. If any
capture is an exclusive borrow of a view-typed place, the union of all captures' sets is
also stored through it, since the body may move one capture's view into another.

The closure body is checked as a separate function over the capture record `C`: its
parameters are the declared ones plus `self`, of type `&C` for an `Fn` closure, `&mut C`
for an `FnMut` one and `C` for an `FnOnce` one (§5.6), and each capture name is a field
of `C`. Moving a `&mut` capture out of an `FnMut` closure is then E1002 by the ordinary
rule, and an `Fn` closure, whose captures are all shared borrows or `Copy`, needs no body
check beyond typing. The body has no clauses, so a store that reaches a parameter or a
capture from a *different* parameter or capture is E1204 with no fix.

## 5. Analysis C: loans

### 5.1 Liveness

Run a backward liveness analysis on locals, where a *use* is any read, move, borrow or
storage-dependent terminator (`scope_wait(s)` uses `s`). A local is **use-live** at a
point if some path from there reads it before it is assigned as a whole.

A loan `L` is **live** at a point if it is in the least set satisfying:

* `L ∈ O(v)` at that point, and `v` is use-live or some live loan has root `v`.

The second half matters: a borrow of a container keeps the container's own borrows
alive, even though the container variable is never named again.

A two-phase exclusive borrow counts as a **shared** loan from its creation until the
call terminator that activates it, and as exclusive from then on.

### 5.2 Access checks

At every statement and terminator, each access to a path `π` is checked against every
loan `L` that is live at that point, was not created by this same statement, and whose
path overlaps `π`:

| Access to `π` | Conflicts when | Error |
|---|---|---|
| copy, or shared borrow | `L` is exclusive | E1103 |
| exclusive borrow | always | E1102 |
| assignment | always, except loans removed by the kill rule | E1101 |
| move, `@forget` | always | E1104 |
| storage end of `root(π)` | `L` is on local storage of that local | E1105, or E1403 if a holder of `L` that keeps it live is a scope |

Every one of these reports `borrow` (the point of `L`) and `later_use` (a use of a
holder that keeps `L` live; for a scope, the `scope_wait`).

An access *through* the borrow that holds an exclusive loan is an access to a path
rooted at a different local (`r.*...`), so it does not overlap and needs no exception.

## 6. The function boundary

### 6.1 Returns

For `return v`, and for `fail e`, where the value has a view type, each origin in `S(v)`:

| Origin | Requirement | Error |
|---|---|---|
| a loan on local storage | never allowed | E1203 |
| any other loan | none. It is a reborrow through a borrow, and the other origins in the set cover it | |
| `Param(p)` | `from p`, or the result type mentions a type parameter that occurs in the type of `p` outside any borrow | E1202, fix adds `p` |
| `Inner(p)` | `from p` or `from p.*`, or the result type mentions a type parameter that the type of `p` mentions | E1202, fix adds `p.*` |
| `Self(s)` | `from s` | E1202 |

**In a closure body** a returned view may hold any parameter or capture origin, because
§13.5 assumes exactly that at the call. Only a loan on the body's own storage is an error
(E1203).

E1201 (no `from` clause at all) is found in phase 6 from the signature. Its fix needs
the body: run this analysis with an empty clause and emit the clause that would make
every return pass.

### 6.2 Stores that reach a parameter

When widening reaches `Param(p)`, `Inner(p)` or `Self(p)` with a set `S` to store, each
element of `S` must be one of:

* `Inner(p)` itself (moving things around inside `p`);
* `Param(x)` or `Inner(x)` where `x` is declared `into p`;
* `Param(x)` where a type parameter that the type of `p` mentions occurs in the type
  of `x` outside any borrow, or `Inner(x)` where the types of `x` and `p` mention a
  common type parameter. This is exactly what the caller adds by type-parameter flow
  (§4.5 step 2): `A_x` when `T` is outside a borrow in `x`'s type, only `inner*(A_x)`
  when it is inside one. So `fn set[T](h: &mut Holder[T], x: &T) { h.r = x; }` stores
  the borrow `x` itself, which the caller does not add, and needs `into h`;
* a loan that is not on local storage.

`Param(x)` or `Inner(x)` otherwise is E1204, with the fix ` into p` on `x` when `p` is
`&mut`, `[]mut` or `&Scope` (E1206 rejects `into` on anything else, so there is then no fix). A
loan on local storage is E1205.

### 6.3 Interface implementations

Checked in phase 6, not here: an implementation's `from` and `into` clauses must be a
subset of the interface method's (E0509). The body is then checked against the
implementation's own clauses.

## 7. Scopes and tasks

Lowering gives `scope s { B }` this shape (pipeline.md section 5):

```
storage_start(s) ; B ; deferred code of B ; storage_end(locals of B) ; scope_wait(s) ; storage_end(s)
```

* `O(s)` starts empty.
* `spawn f(a_1..a_n) in s`:
  1. every argument type and the result type must be `Send`, else E1401;
  2. the effects of the call on its arguments (4.5 steps 1 and 2) are applied now;
  3. `O(s) ∪= A_1 ∪ ... ∪ A_n`. If `s` is a scope parameter, this is a store that
     reaches `Self(s)`, checked by 6.2;
  4. the handle, if any, gets `{ a shared loan on s } ∪ (the result set of step 3 of
     4.5)`. That loan is what keeps a handle from leaving the block.
* `scope_wait(s)` is a use of `s`. Since it comes after the storage ends of the block's
  locals, any loan in `O(s)` on a local of `B` is live at that storage end: E1403.
* `h.join()` does not shrink `O(s)`. Loans given to a task end with the scope.
* Two tasks given overlapping exclusive borrows conflict by 5.2, because the first
  loan is live (held by `s`) when the second is taken.

`par for` is checked as a closure with one parameter (the item) and one shared-borrow
capture per outer local it names:

| Check | Error |
|---|---|
| the item type is `Send` | E1401 |
| each captured local's type is `Sync` | E1402 |
| the body does not assign, move or exclusively borrow a captured local | E1407 |

In the enclosing function the whole loop is one terminator that shared-borrows every
captured local and consumes or reborrows the iterated value, so the ordinary access
checks apply to it.

## 8. Worked examples

**Ending a borrow by last use.**

```ingot
var x: i64 = 1;
let r = &x;          // loan L1 on x, O(r) = {L1}
show(r.*);           // last use of r
x = 2;               // r is not use-live, so L1 is not live: accepted
```

Swap the last two lines and `x = 2` is an assignment overlapping live `L1`: E1101.

**A borrow of a container protects what the container borrows.**

```ingot
let words = split(text, &arena);     // O(words) = {L_text.., L_arena}
let w = &words;                      // L_w on words, O(w) = {L_w}
arena.reset();                       // needs &mut arena
show(w.*[0]);
```

`w` is use-live, so `L_w` is live. `L_w` has root `words`, so the loans in `O(words)`
are live by the second half of 5.1, including the one on `arena`: E1102.

**`from self.*` releases the receiver.**

```ingot
fn rest(self: &Parser) -> []u8 from self.* { return self.src[self.pos..self.src.len]; }
```

In the body, `self.src` is a copy of a view out of path `self.*.src`, one dereference,
so its set is `inner({Param(self)}) = {Inner(self)}`. Slicing a shared slice
dereferences a shared borrow and adds no loan. The return check finds `Inner(self)`,
which `from self.*` allows. At a call `parser.rest()`, the result's set is
`inner({L_parser}) = O(parser)`, which does not contain `L_parser`, so the parser is
free again as soon as the call returns.

**A reborrow is not an inner borrow.**

```ingot
fn take(self: &mut W) -> []mut u8 from self.* { return self.buf[0..1]; }
```

`self.buf[0..1]` reborrows through `self.*.buf` and then through the slice, two
dereferences with an exclusive one: a new loan, and the set `{loan, Param(self),
Inner(self)}`. `Param(self)` is not covered by `from self.*`: E1202.

**A task keeps its arguments until the scope ends.**

```ingot
var total: i64 = 0;
scope s {
    let h = spawn bump(&mut total) in s;   // L on total, O(s) = {L}
    h.join();
    total += 1;                            // s is use-live until scope_wait: E1101
}
total += 1;                                // accepted
```

## 9. What this algorithm is not precise about

These are accepted consequences of the design, not bugs. Each rejects some programs
that are in fact safe.

* One origin set per local, for all the borrows inside it.
* All elements of an array or slice are one path.
* A handle's `join` does not release the task's borrows.
* A closure is assumed to move any capture's view into any exclusively captured place.
* A call through a function value is assumed to return anything it was given.

## 10. Open points

* The algorithm has not been run. The worked examples were traced by hand.
* No proof of I1 to I6 exists. The places most worth attacking are the kill rule
  (4.4), the transitive liveness rule (5.1), and widening through chains of borrows.
* Kernel and kernel-helper bodies need no ownership analysis: they cannot borrow. Their
  rules (§27.1) are checked in phase 7, each body on its own.
* **Widening through shared origins is too strict.** One set per value mixes where a
  slice lives with what its elements borrow, so storing into `words[i]` of a slice that
  also holds `Inner(req)`, for a `req: &Request`, is reported although nothing can be
  stored through `req`. The fix is for widening to skip `Param(p)` and `Inner(p)` when
  `p` is a shared borrow or shared slice of a type without interior mutability, and to
  skip a shared loan on such a local. It waits for the definition of interior
  mutability (pipeline.md §11); until then the rule stays as written.
