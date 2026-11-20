# Design decisions since the first draft

The specification states the rules; [../rationale.md](../rationale.md) explains the
first draft. This file records the decisions made since, and **why**: what each one
fixed, what was rejected, and where the rule now lives. Most came from two reviews by
the language's designer, made against the compiler as it was being built:

| Review | Reviewed at | Applied as |
|---|---|---|
| 1 | `e88296d` | `228062e` (spec), then S4c `0562994` and S4d `93f6a05` (compiler) |
| 2 | `7b08b3a` | `bd0bbd2`, `720763f`, `8581374` (spec and `docs/std/`), then step P1a onward |

Entries marked *user* are choices the user made between presented options. The others
were recommendations the user accepted as a whole. The reviews themselves are working
notes outside the repository; nothing in them is needed beyond this file, the spec,
[../../COMPILER.md](../../COMPILER.md) (back-end design and the step plan) and
[../std/](../std/).

A decision is changed by changing the spec and adding a line here, never by changing
code alone.

## 1. Names, modules and visibility

* **A lone file belongs to the package above it** (review 1, D2). `ingot check
  src/net/http.ingot` checks the package and prints that file's diagnostics; a file
  with no `ingot.toml` above it is a one-module package. *Why:* that is the command an
  agent runs most, and every sibling import failed. (tools.md §1.)
* **A declaration or import hides a builtin name** (review 1, D4). A user type named
  `Grid` or `Result` must not break when the builtin list grows.
* **The language items are listed** (review 1, S5-6). Every `std` path the compiler
  knows, and what it assumes, is a row of pipeline §8. *Why:* phase 9 and later phases
  depend on these assumptions; scattered mentions had gone stale.
* **Inherent `impl T` only in `T`'s module; for built-in types only in `std`** (review
  1, N4, E0506). `std` writes `impl[T] []mut T { ... }` for `shared`, `take` and the
  like, so there are no "magic members".
* **`impl I for &T` is allowed for a type of the same module** (review 2, S5-13; 01
  §4.4). `impl Allocator for &Arena` needs it.
* **One more visibility level, `pub(pkg)`** (*user*, review 2, U1; 01 §2). *Why:* `std`
  needs items visible to the rest of `std` and not to programs (`std.sys`, `rt`,
  capability constructors), and per item, which a private-module convention cannot
  express. *Spelling:* `pub(pkg)` is closest to what models know (`pub(crate)`), keeps
  `pub` as the one word to search for, and costs no keyword. *Rejected:* `internal`, a
  bare `pkg`, Go's directory convention.
* **No re-exports** (*user*, review 2, U1). An item has one path, the one tools print;
  `ingot rename` and the E0304 fix rewrite importers when an item moves. A signature
  may mention a less visible type, which makes tokens possible.
* **Constants stay `lower_snake`; constant generic parameters are spelled as values**
  (review 1, Q12): `struct Ring[T, const n: i64]`. *Why:* a third lexical class for
  `MAX_LEN` cannot be told from the type class (`N`, `F32`, `U8`), so agents would have
  to remember a rule; the lexer's two-class property is worth more. Making constant
  parameters lowercase removed three special cases in the grammar. E0104's message
  names the `lower_snake` spelling for an all-capitals name. *Hazard kept in check:* a
  pattern binding may not take the name of a visible `const` or `static` (E0309).
* **No grouped imports and no error for an unused private function** (review 1, Q13,
  rejected). One import per line keeps the E0310 fix a line deletion; the unused-function
  error would fire on every half-written module.
* **A struct with no fields cannot be taken apart outside its module** (*user*,
  2026-11-16, found reviewing P1b; 01 §4.2, E0307). Construction already counted a
  fieldless struct as having a private field; destructuring did not, so another module
  could consume a fieldless `linear struct Permit {}` that is meant to come back to its
  own module. *Rejected:* the vacuous reading ("every field is visible").
* **A linear struct is taken apart wherever all its fields are visible** (*user*,
  2026-11-20, found reviewing R1c; 02 §10). The module rule exists so that private fields
  can keep an invariant; a struct any module can build with a literal has none to keep.
  Without it `AllocFail[T]` (all fields `pub`) could not give a linear value back outside
  `std.mem`, which is the whole point of the `_or_return` forms. *Rejected:* an
  `into_parts` method in `std.mem` (every plain-data generic with a linear parameter
  would need one).

## 2. Types, interfaces and generics

* **Arrays, slices, tuples and `?T` implement `Eq`, `Ord` and `Hash` structurally**
  (review 1, D13), element by element, a slice by its elements and never its address.
  *Why:* otherwise `Map[[]u8, V, A]` could not exist. `==` stays IEEE on floats, while
  `eq` uses the total order; both are stated.
* **Dynamic dispatch only for interfaces with no associated type whose functions take
  `self` by borrow, have no generic parameters and mention `Self` nowhere else**
  (review 1, D13, E0510). `&dyn Eq` and `&mut dyn Iter` had been accepted and could not
  be called soundly.
* **`from` may be written on a return type that mentions `Self`, an associated type or
  a type parameter** (review 1, D13; §13.1). It may hold a borrow in some
  implementation (`Iter.next`).
* **No `comptime`, no types as values, no macros; any function may run in a constant
  expression** (review 1, Q11). *Why:* each of those moves code out of the text an agent
  reads; a fix edit cannot point into an expansion and `fmt` cannot lay it out. Zig-style
  `comptime` also gives up "a generic body is checked once", which keeps phase 11
  small. A macro system is a dialect per project in a language no model was trained on.
  *Rejected:* a `const fn` marker, because it colors every pure helper and a failing
  constant expression already reports at the use. *Deferred:* structural `Eq`/`Ord`/
  `Hash` on request, and `@config` values for `when` (rationale §12).
* **Literals under a numeric bound** (review 1, Q6): an integer literal must be a value
  of every type the bound allows (`0..=127` under `Num`), checked once in the generic
  body.
* **`Fn` and `FnMut` imply `Discard`** (*user*, D11). *Not adopted, optional:* "`Fn`
  implies `Copy`", which would let a generic pass `f` to two callees without `&f`.
* **A type parameter bounded by `Fn(..) -> R` mentions the parameters of that
  signature** (review 2, S-2; 02 §13.3). *Why:* otherwise every higher-order function
  (`map_reduce`, `first_of`, `get_or_init`) needed a `from` clause on the closure.
* **A constant generic argument may be a negative literal** (review 2, N4).
* **E0521 also covers lengths from constant generic arguments and types above 2^62
  bytes** (review 2, H3): in phase 6 for a concrete type, per instance in phase 11.
* **Niches are exact** (review 2, B3; 04 §25): "there is no other niche", since layout
  is visible through `@sizeof` and `ingot layout`. The `?Box` niche was dropped earlier
  (review 1, D15) because `Box` is a library struct.
* **A length expression over constant parameters is part of the type, compared as a
  tree** (*user*, 2026-11-16, found reviewing P2; 01 §8). `[n + 1]T` and `[n+1]T` are one
  type, `[1 + n]T` another; substitution rewrites the tree, and a fully concrete one is
  evaluated to an ordinary length. `@sizeof(T)` and `@alignof(T)` of a type parameter are
  allowed in the tree too, folded once `T` is concrete. *Rejected:* normalizing `+` and `*` (a longer rule
  for little gain) and forbidding expressions (contradicts "a length computed from a
  constant generic argument"). The first implementation keyed the type by a hash of
  the text and its owner, which rejected `Buf[3].{ data: @zero([4]u8) }`.

## 3. Bodies and expressions

* **A branch of an expression-form `if`/`match` may be a block that does not fall
  through** (review 1, Q13): `.b => return 0`. E0207 now means "a branch block that can
  reach its end".
* **Open slice bounds** `s[lo..]`, `s[..hi]`, `s[..]` (review 1, Q13), and `.ptr` of a
  slice is safe to read (review 1, Q2): an `unsafe` block whose reason is "reading a
  pointer" dilutes `ingot audit`. The same argument made `Cell.get` and `@offset` safe
  (review 2).
* **An assignment locates and checks its place after the value** (review 1, S5-1; §5.1).
  `s[i] = shrink(&mut s)` was an out-of-bounds write in safe code otherwise.
* **A guard reads its bindings through a shared borrow; a binding moves only once its
  arm is chosen** (review 1, S5-2).
* **A bare variant name in a pattern is E0603 with a fix that inserts `.`** (review 1,
  Q13): `red =>` silently bound a new name.
* **Temporaries live to the end of their statement** (review 1, G5) **and a shared borrow
  of one is legal** (*user*, review 2, D15; 02 §11). *Why the change:* `Eq`, `Ord` and
  `Hash` take `&Self`, so `p.eq(&"ada")` and `(p + q).cmp(&r)` needed a `let` first.
  E1105 already catches a borrow that outlives the statement. `&mut` of a temporary
  stays E0607: the change would be lost.

## 4. Ownership and borrows

* **Every local has an origin set** (review 1, O1). A self-contained `Arena` hands out
  `&mut` into memory it reaches through a raw pointer, so storing a view through that
  borrow must widen the arena's set.
* **A raw pointer to a view is a view** (review 1, N1). Containers keep elements behind
  raw pointers; without this, `List[[]u8, Heap]` passed `Owned`.
* **Closures** (review 1, G1): the class (`Fn`, `FnMut`, `FnOnce`) is read off the
  capture list; the body is checked as a function over the capture record; calling an
  `FnMut` needs an exclusive use but not a `var`.
* **Reborrowing on use** (review 1, G4): a `&mut T` place written as an argument,
  receiver, `spawn`/`dispatch` argument or `for` iterand is lent for that use.
* **A by-value `match` tracks the variant a path holds** (*user*, D9): lowering records
  `π is v`, so a linear enum is consumed on every arm. *Rejected:* treating every arm's
  payload as possibly present.
* **`T ! E` is an obligation on both variants** (D10); **the `from` clause covers
  `T ! E` results** (*user*, D13).
* **A scope is a borrowed local** (review 2, H1; 03 §18, checker §7). `scope s` binds
  `s = &σ`, so `s`, its copies and what is returned `from` it escape only as an ordinary
  E1203 or E1105. *Replaced:* treating an untraceable `&Scope` as every scope in the
  function, which over-approximated and still let a scope and a helper's handle escape.
  *Rejected:* restricting where a `&Scope` may be stored, which could not catch the
  helper case.
* **A widening fix for shared origins needs "frozen" types** (review 2, Q3; checker
  §10): no `Cell`, raw pointer, `dyn` or type parameter at any depth. "No `Cell` by
  value" is not enough, because a type can reach an arena through a borrow field. Not
  built yet; the precise fix (two origin sets per value) is not planned.

## 5. `unsafe`, raw pointers and interior mutability

* **The body of an `unsafe fn` is not an `unsafe` context; an `unsafe` block's region is
  lexical** (review 1, D32; §15).
* **What a raw pointer may touch** (review 2, Q7; 02 §15): a shared borrow covers its
  referent's bytes against writes; an exclusive borrow against any other access; a
  pointer made from a borrow of a place is usable only until the place's next direct
  use. *Why:* it is the rule an author can follow without a model of the optimizer ("a
  pointer into a variable is good until you touch the variable again"), it lets phase 10
  trust its facts across calls (every fact is made by a direct use), and it justifies
  the back end's non-aliasing and read-only marks. *Rejected:* "until the loan ends",
  because a raw pointer has no origins and the loan ends at the cast. *Replaced:* the
  phase-10 stopgap that gave no facts to a local whose borrow became a raw pointer; it
  could not see a callee that keeps the pointer.
* **`std.sync.Cell[T: Owned]` is the one language item of interior mutability**
  (review 1, B1; `f3e7d2a`). A type has interior mutability when it contains a `Cell`
  by value. `Cell` is public and `get` is safe (review 2). A type that keeps such
  contents behind a raw pointer (a channel buffer) bounds them with `Owned` itself.
* **Raw pointer casts by `as` and a safe `@offset`** (`f3e7d2a`). Creating a raw pointer
  is safe everywhere; using it is not.
* **Atomics: an `AtomicValue` bound and `unsafe` builtins; `@fence` is safe**
  (`f3e7d2a`). **Orders are three enums** (*user*, review 2, S4c): `LoadOrder`,
  `StoreOrder`, `AtomicOrder`, so an order the operation does not allow cannot be
  written. *Replaced:* clamping an invalid order at run time, which accepted
  `x.load(.release)` silently. The orders need not be constants; a non-constant one is a
  branch in the back end.
* **`AtomicValue` is satisfied by raw pointers to `Owned` types and payload-free `copy
  enum`s**, besides the integers and `bool` (found reviewing P1b, 2026-11-16; 01 §8). The
  bound implies `Copy` and `Owned`, but an enum is `Copy` only when declared `copy enum`,
  and a `*T` whose `T` is a view is a view; the old wording ("raw pointers and
  payload-free enums") let `Atomic[*&T]` put a borrow in a `Cell`, which §13.6 forbids.
* **`Atomic[T]` is `Send` and `Sync` only if `T: Send`** (review 2, Q4, H6). *Why:* raw
  pointers are neither so that a type built on them gets the properties only through an
  `unsafe impl` that `ingot audit` lists; `Atomic[*mut Node]` undid that.

## 6. Concurrency

* **`waitable` is a small rule plus a library interface** (review 2, Q1; 03 §21). A
  waitable body prepares a `std.task.Wait` operation; a direct call waits for it;
  `select` prepares every arm and then polls in a random order. *Why:* a losing
  `Chan.recv` consumes nothing because a poll consumes only when it completes, with no
  commit protocol between arms; the timer's deadline is fixed at preparation; std and
  programs add waitable operations without compiler changes. *Rejected:* a
  compiler-known set of waitable functions.
* **A waitable body cannot fail** (*user*, 2026-11-16, found reviewing P1b; 03 §21,
  E1410). `fail`, a propagating `try` and falling off the end are errors in it; an error
  found while preparing is kept in the operation and given by its first `poll`. *Why:*
  every arm of a `select` then always has an operation, and the lowering of a direct
  call stays "prepare, then wait". *Rejected:* an error exit counting as an operation
  already complete with that error.
* **`try` in a `select` arm** (review 2, L1): `_ = try clock.after_ms(250) => ...`. The
  timer's `.canceled` error must stay, so the arm needed a way to propagate it.
* **`Scope.canceled` is split** into `is_canceled` (a read) and the waitable `canceled`
  (review 2, S5-11). A direct call of a waitable waits, so `if s.canceled()` is a type
  error, not a silent block.
* **`export fn` bodies run outside any task** (review 2, L3; 03 §23): entering a
  `scope`, `spawn`, `par for`, `select` or parking there is T17, whatever thread calls;
  `task.run(f)` is the way in. *Why:* one rule instead of "a thread the runtime did not
  create", and it covers a callback from foreign code on a worker, where parking would
  move foreign frames.
* **Dedicated OS threads are scope children: `thread.spawn(s, f)`** (review 2, S5-4).
  *Rejected:* a library `thread.group(body)`: a closure body cannot store a borrowing
  closure into a parameter (E1204), `group` cannot return its body's result (E1203), and
  a linear group in the caller's frame could be `@forget`-ed while threads run.
* **Task records are recycled at `join` or at finish, not at the scope's end** (review
  2, S5-10). An accept loop in a long-lived scope would otherwise grow without bound.
* **`select` evaluates every arm's operands before waiting** (D2), now "runs every arm's
  preparation".
* **Earlier-iteration loans conflict, and a compound assignment is one access** (D16;
  checker §5.2).
* **Guards are not `Send`** (*user*, std design). The language cannot say "not `Send`",
  so a guard holds a `[0]*u8` marker and `unsafe impl Sync` adds back what is wanted.

## 7. Check removal and cost

* **Facts and their sources** (review 1, C1–C5; pipeline §6): an assignment of a
  constant, `x += c` keeping a bound, a loop counter, lengths at most 2^62 (so `i + c`
  cannot overflow under `i <= len`), constant generic parameters as symbols, and rule 6
  by dominance.
* **An exclusive borrow kills facts at the borrow, and a two-phase borrow again at its
  activation** (review 2, D21). *Why:* the borrow is a write the analysis does not
  follow; while its loan is live nothing can read the local, and when it is dead nothing
  can write through it.
* **Rule 5 folds constant operands into E0806, including a constant index against a
  constant length; a branch on a constant condition is folded first and its dead side
  reports nothing** (review 2, D17). *Why:* `when` cannot name a constant, so an `if` on
  a `const` is the only switch, and reporting a trap in code that can never run is
  wrong. Only literals, `const` items and operations on them count; a local never does,
  whatever the analysis knows about it.
* **The cost items are defined from signatures and the CFG** (review 2, B8; 05 §31):
  `alloc` is a call whose error type is `AllocErr` or converts to it, so it stays a
  property of the checked program; `park_point` is the one non-local item.

## 8. Constant evaluation and instantiation

* **Constant evaluation interprets CFGs, pulling called bodies through phases 7–8 on
  demand** (*user*, D5); nested evaluations share the depth limit (*user*, D6).
  *Rejected:* a separate tree-walking evaluator.
* **Phase 11 reports only what depends on a constant generic argument's value, on
  instantiation depth, or on a `dispatch`'s executor** (review 2, B1, Q8; pipeline §1,
  §6a). Any argument that must be constant may be an expression over constant
  parameters, checked per instance. *Why:* the language has no bounds on constant
  parameters, so there is nowhere else to check; an author who wants the error at the
  call writes `@static_assert` in the body.
* **Depth is one more than the smallest depth of a user** (review 2, B1), so E0809 does
  not depend on discovery order. *Replaced:* the phase-10 polymorphic-recursion
  detector, which missed recursion through impl generic arguments (H5).
* **`ingot check` runs phase 11a (discovery) and nothing else of phase 11** (review 2,
  B1, Q5). It comes before the back end, as step P2.

## 9. Kernels and SIMD

* **Kernel helpers instead of a whole-program phase 7k** (*user*, review 1, D35). A
  `kernel fn` whose first parameter is not an index is a helper; its body follows the
  kernel rules and is checked alone. *Why:* phase 7k was the only place where an error
  in one function depended on another's body, against rationale §1, and across packages
  a dependency's edit broke your kernel in their file. Kernels and helpers have no
  generic parameters (H5 of review 1: a generic `+` could trap per instance).
* **`@min`/`@max` on floats return the other operand when one is NaN and order `-0.0`
  below `0.0`** (review 1, D41), so a reduction's result does not depend on its order.
  The x86 cost was never measured.
* **`offset` on an index saturates at the `i32` limits** (review 1, D37).
* **Grid kernels are deferred** (review 1, Q8) until 1-D kernels run on `exec.simd`;
  grid layout and GPU buffers and textures are to be decided together. Small integer
  element types in kernels (`i8 u8 i16 u16`) wait for the same point.
* **The kernel IR is structured, not a CFG** (review 2, B6): two of the three emitters
  (SPIR-V, Metal source) want structured control flow. Its document,
  `docs/impl/kernels.md`, is written with step B5 (COMPILER.md).

* **A dispatch's executor is concrete at each instance** (*user*, 2026-11-16, found
  reviewing P2; 04 §27.2, E1514). `E: Executor` resolves per instance; `&dyn Executor`
  at a dispatch is E1514. *Why:* the source names the executor, and `exec.simd` must be
  vectorized, which needs the executor known when the instance is compiled.
  *Rejected:* forbidding the type `dyn Executor` everywhere.

## 10. Packages, ABI and builds

* **Dependencies** (*user*, review 2, U2; 05 §33.2). Each choice and its reason:
  * a SHA-256 hash over exactly the files a build reads; `rev` says where to fetch,
    `hash` is what is checked, so a mirror or moved tag cannot change a build;
  * no lock file: every git dependency is pinned in the manifest that names it, and a
    dependency's manifest is inside its hash, so the root determines the whole graph;
  * one package per name, conflicts resolved by the root's `[override]` (E1708).
    *Rejected:* both revisions as distinct packages, which gives "expected
    `zlib.Stream`, found `zlib.Stream`";
  * no registry and no version numbers the compiler reads; a compatibility promise can
    later be checked by comparing two `ingot api --json` outputs;
  * no dev-dependencies: `test` blocks live in modules, so a test-only import is still
    an import; unreached code costs nothing in the executable;
  * `std` is tied to the compiler, because the language items make any other
    arrangement unsafe.
* **Packages in a build** (*user*, 2026-11-18, before P4a; 05 §33.2). A dependency's
  diagnostics are reported like the root's (`std` excepted), and its files are shown as
  `name/path`, as `std`'s are. *Why:* a dependency that does not check stops the build
  anyway, and one spelling works for path and git dependencies without an absolute path.
  *Rejected:* one summary code at the `[deps]` entry (hides where the error is); paths
  relative to the root (`../csv/x.ingot`), which git dependencies cannot have.
* **Git dependencies come from the system `git` into a cache keyed by hash**, and one
  missing from the cache is its own code, E1712 (*user*, 2026-11-18). *Why:* no Rust
  dependency, and "not fetched" is a different fix from "content changed" (E1703).
* **The dependency hash is SHA-256, not BLAKE3** (*user*, 2026-11-18, before P4a).
  *Why:* the compiler takes no crates, so the hash is written by hand, and SHA-256 is the
  smaller one to get right; `shasum -a 256` ships with every system for checking it; and
  `conformance/programs/sha256.ingot` already implements it in Ingot for a self-hosted
  compiler. *Rejected:* BLAKE3, whose speed does not matter for source trees and whose
  tool is not installed by default. The `sha256:` prefix leaves room for another
  algorithm later.
* **`deps update` writes the commit it fetched** (*user*, 2026-11-18, P4a review), even
  for `--rev main`. *Why:* `rev` and `hash` then always name the same content; a branch
  in a manifest turns into E1703 for everyone once it moves. *Rejected:* writing the
  branch name back.
* **Inside a workspace member, paths are the member's** (*user*, 2026-11-18, P4a
  review): the member is the package, as tools.md §1 says; workspace-relative paths only
  at the workspace root. *Rejected:* workspace-relative everywhere.
* **Smaller package rules** (*reviewer*, 2026-11-18, for P4a; 01 §2, 05 §33, §33.2):
  a nested `ingot.toml` ends a package (workspaces and in-tree path dependencies need
  it); a module imports only its own package, `std` and its own `[deps]` (a transitive
  dependency is an implementation detail of the package that names it); no `targets`
  key means all three (a single file has no manifest and must check everywhere); the
  hash frames lengths as 8 bytes little-endian; graph order is breadth-first with
  `[deps]` by name; a `[deps]` cycle is E0303 at the entry; an override that matches
  nothing is inert; `ingot` is `MAJOR.MINOR`.
* **Report tools** (*user*, 2026-11-18, before P4b; tools.md §2, §4).
  * `ingot explain` reads the catalog row and the conformance suite, both embedded
    when the compiler is built. *Why:* one source of truth that cannot drift.
    *Rejected:* a hand-written table in the compiler.
  * `ingot api PATH`: a path on disk first, else a dotted path in the current build; an
    item prints its whole subtree. A `signature` is one line: the tokens as written,
    comments dropped, white space collapsed. *Rejected:* verbatim header text, which
    carries newlines and comments into a field tools compare.
  * `ingot audit` covers the build minus `std` by default, so a dependency's `unsafe` is
    visible; `ingot audit std` audits `std`. *Rejected:* the root package only, and the
    whole build with `std`, whose runtime would dominate the list.
* **`ingot fmt` is canonical, rustfmt-style, 80 columns** (*user*, 2026-11-18; 05 §34).
  The output ignores the author's line breaks (keeping only their 0-or-1 blank lines and
  import groups); a list that does not fit breaks one item per line with a trailing
  comma; operator and method chains break before the operator or `.`; no column
  alignment; imports sorted within the author's groups. `struct` and `enum`
  declarations always span several lines (*user*, 2026-11-19, reversing their one-line
  form after seeing it on `std`). The block of `defer`, `errdefer`, `unsafe`, an `orelse` or
  `catch` handler, a `match` or `select` arm and a closure body stays on one line when it holds one
  statement without a block of its own and fits (*user*): cleanup and error handling
  read better as one line than three. `select` arms follow `match` arms. *Rejected*
  (*user*): one-line function bodies, and one-line `if` and loop bodies, which stay
  multi-line as in Go and Rust. Closures are spaced `fn [caps](params)` as in the
  spec's examples (*user*). *Why:* one layout for any
  input, and a one-item change is a one-line diff. *Rejected:* gofmt's kept line
  breaks (two layouts of one program), packing several items per line, and gofmt's
  aligned columns (renaming one field re-indents its neighbours).
* **The C ABI** (review 2, U3; 04 §29): `bool` is C's `_Bool`; `copy struct`s cross by
  value by each system's rule; `?extern fn` crosses as a nullable function pointer;
  other optionals, results and enums do not. No unwinding; unwind tables are still
  emitted. Foreign unwinding through Ingot frames is undefined.
* **Executables only in v1** (review 2, U3). A C-callable library is deferred
  (rationale §12): it needs a decision on who starts the runtime and a header.
* **No temporary C runtime** (review 2, B4): the entry, `trap`, `write` and `exit` are a
  hundred lines of Ingot and the first real test of `extern`, `unsafe` and the atomics.
* **Assembly sources in `std`'s manifest only** (review 2, B4): the stack switch.
  *Rejected:* builtins (the text should live in `std` and survive self-hosting) and
  inline assembly (a language feature of its own).
* **Thread-local state through the OS calls, not a language feature** (review 2, B4). A
  call cannot be cached across a park by the optimizer; a `thread_local` variable would
  need that rule stated and enforced.
* **`rt.start` takes the table behind `Env.args` from one OS mapping** (*user*,
  2026-11-17, asked in B1; `docs/std/rt.md` §1). It is sized for `argc`, filled once, never
  freed; `start` still uses no allocator. *Rejected:* a fixed static table (an arbitrary
  limit) and an empty `args` until B3 (a value that is not true).
* **The `,` after an expression arm may be omitted only before the closing `}`**
  (*user*, 2026-11-18; grammar.md). As every other comma list. *Rejected:* always
  required (`match c { true => o, false => s }` reads naturally), and optional
  everywhere (`.a => 1 .b => 2`, which the parser had accepted).
* **`&dyn Fn` converts as `&dyn I` does** (*user*, 2026-11-18, found in P2, settled before
  B2; 01 §5.6). A `&T` becomes `&dyn Fn(A) -> R` when `T: Fn(A) -> R`, and a `&mut T`
  becomes `&mut dyn FnMut(A) -> R` when `T: FnMut(A) -> R`. Closures, functions and `fn`
  pointers all convert. *Rejected:* closures only (a callable taken by `&dyn` would need a
  second signature for a `fn` pointer, though 01 §5.6 says both satisfy `Fn`).
* **A closure is laid out as a struct of its captures in capture-list order** (*user*,
  2026-11-18, before B2; 01 §5.6, 04 §25). Its size is visible through a generic
  `@sizeof(T)`, so it is not left to the compiler. *Rejected:* an unspecified order.
* **A refused `Env.args` mapping is T12 at `<runtime>:0:0`** (*user*, 2026-11-17, found in
  the B1 review; rt.md §1, diagnostics.md §18). The runtime cannot get memory for the root
  task, which is T12's meaning; a trap always prints its one line. *Rejected:* a silent
  exit 101 (a trap with no line).
* **A `-> never` function whose end is reachable is E0412** (*user*, 2026-11-17, found in
  the B1 review; diagnostics.md). `never` is a result type with no values, so reaching the
  end of the body cannot produce one. The runtime's `exit` is declared `-> never`.
  *Rejected:* a separate code (E0412 already names the condition).
* **Phase 10 removes the division checks a constant divisor rules out** (*user*,
  2026-11-17, found in the B1 review; pipeline §6, rule 7). `v / 10` can neither divide by
  zero nor overflow, and `ingot cost` must not report it. *Rejected:* leaving it to LLVM
  (the report states what phase 10 guarantees, and both pipelines must agree).
* **The back end pins upstream LLVM 22** (*user*, 2026-11-17). On macOS that is
  Homebrew's `llvm@22`, found by path; Linux and Windows use the same major version.
  *Why:* one version on all three targets, checked at start-up. *Rejected:* Apple clang,
  which needs no install on macOS but is a fork that rejects LLVM 22's textual IR (and
  LLVM 22 rejects Apple's: `frame-pointer` and stack-probe attributes), and has no Linux
  or Windows counterpart. Found: `alwaysinline` is honored at `-O0` on LLVM 22.
* **`ingot test` builds with `--fast`; `--opt` optimizes** (*user*, 2026-11-19, B3;
  05 §33, tools.md §3). Tests are the edit-and-test loop, and both pipelines promise the
  same semantics. *Rejected:* the optimizing default of `ingot build` (slower for the
  common case), and `--fast` with no way out.
* **Without `--seed`, `ingot test` draws a seed and reports it** (*user*, 2026-11-19,
  B3; tools.md §1, §3). Once the scheduler exists, a different seed each run explores
  other `select` orders and placements; a failure still names its seed. The cost:
  `seed` joins `ms` as a field exempt from determinism. *Rejected:* a fixed default
  (every run explores the same choices), and a seed only under `--seed` (a failure
  would name nothing to replay).
* **The structural form in B3 covers every type but floats** (*user*, 2026-11-19).
  Integers, `bool`, enums, structs, tuples, arrays, slices (`[]u8` as a string
  literal), optionals. Floats, pointers, references, `&dyn` and closures are `not
  implemented` until `std.fmt` (R5), whose shortest round-trip float printing is a
  project of its own; error types rarely hold them. Types are named as declared with
  their generic arguments, without the module path (05 §35).
* **The `main` error line prints as `main`'s module sees the value** (*user*,
  2026-11-19; 05 §35), and a failed test as its module does: one visibility rule for
  every use of the structural form. *Rejected:* printing every field because the line is
  the process's own report.
* **A test receives its `TestEnv` as a hidden parameter** (B3, 2026-11-19; rt.md §1,
  tools.md §3). `rt.test_env() -> &TestEnv` would have needed process-global mutable
  state, which `std` cannot have before atomics (R2), and 05 §32 already says the result
  borrows the harness's storage for the block. `rt.test_start(env, tests)` builds the
  `TestEnv` on its frame and calls the selected test. The harness passes the index and
  the temporary directory as arguments and owns the directory.
* **`@test_env()` in a closure inside a test is E1706** (*user*, 2026-11-19, found in the
  B3 review; 05 §32). A closure reaches only what its capture list names, and the test
  environment is the test's parameter, so `let t = @test_env();` and capture `t`.
  *Rejected:* an implicit capture (the only one in the language). `par for` and `spawn`
  bodies are lowered to functions too and need the same answer when R2 builds them.
* **An assignment through a raw pointer is not E1004** (*user*, 2026-11-19, found
  planning R1; 02 §10). `p.* = v` in `unsafe` is how a container stores a value into
  fresh memory; allocator memory is not a place, so the checker cannot know what it
  holds, and overwriting a live linear value there only leaks it, which is safe.
  *Rejected:* a separate `@write(p, v)` builtin (one more builtin for what the
  assignment already says inside `unsafe`). Moving out with `p.*` was already accepted.
* **`align` is a contextual keyword** (*user*, 2026-11-19, found planning R1; 01 §1),
  like `par` and `grain`: a keyword only where `align(n)` follows a struct header or
  the type of a field or `static`. The parser already accepted it as a parameter name,
  and `size, align` is the allocator vocabulary. *Rejected:* reserving it everywhere
  and renaming `Allocator`'s parameters to `alignment`.
* **`Heap` is the system allocator** (*user*, 2026-11-19, R1; mem.md D8). *Rejected
  for now:* a size-class allocator written in Ingot over `Vm`.
* **A file with no manifest is a package named after its stem, made an identifier**
  (*user*, 2026-11-19, found in the B3 review; tools.md §1). Normalized as Cargo does for
  crate names, plus lowering (a capitalized name is a type) and a `_` after a keyword:
  `my-tool` → `my_tool`, `parse.fixed` → `parse_fixed`, `test` → `test_`. The loader had
  named every such package `t`. *Rejected:* E1705 for a stem that is not a name (`ingot
  run my-tool.ingot` would fail), and a fixed fallback name.
* **A test process that ends other than by 0, 1 or 101 is `crashed`** (B3,
  2026-11-19; tools.md §3), with its `exit` or `signal`. A fourth status rather than
  folding it into `trapped`, which promises a trap line.

## 11. Standard library conventions

* **Nothing in `std` traps on bad input** (*user*): `AllocErr.bad_request`,
  `Chan.init`'s `.bad_capacity`, a negative delay is zero. **An error is visible, a
  silent no-op is not** (*user*, review 2, S5-7): `WaitGroup.add` with a negative `n`
  returns `WaitGroupErr.negative`.
* **`_or_return`** (review 2, S5-1; 05 §35): the ordinary allocating function requires
  `T: Discard` and returns `AllocErr`, so `try` works; the `_or_return` form gives the
  value back in `AllocFail[T]`. *Rejected:* `AllocFail[T]` as the error of the default
  name, which broke `try` and the `alloc` cost definition.
* **Cleanups take a closure** (`F: FnMut(T)`), and `Arc.release_last` returns the value
  (review 2, S5-5): a `fn` pointer cannot capture what a cleanup needs.
* **`@test_env()` gives a `TestEnv` capability** (review 2, L2; 05 §32): test-only
  operations are its fields and methods (`clock.step(d)`). *Rejected:* builtins such
  as `@test_clock_step`, and a notion of a test-only function.
* **The structural `Format` form is not an `impl`** (review 2, L4): generated per printed
  instance, writes the value as the language writes it, and prints only visible fields.
* **The `alloc` cost item is a call whose error type is `AllocErr` or `AllocFail[T]`**
  (found reviewing P3, 2026-11-16; 05 §31). The old words, "`AllocErr` or converts to
  it by `from`", matched nothing beyond `AllocErr` (it has no `from` variants), so every
  `_or_return` allocation was invisible to `ingot cost`.
* **`Handle[T]` is 8 bytes** (*user*, review 2, S5-14): handles are stored everywhere in
  graph code; a slot whose generation is used up is retired.
* **Zero-size allocation requests are valid** (review 2, S5-15), so containers need no
  special case for zero-sized elements.
* **How std is designed** (review 2, S6): a design document is accepted only when its
  declarations pass `ingot check` as stubs, and modules are implemented (with `test`
  blocks) before more are designed. Three of review 2's std findings would have been
  found by compiling the declarations, none by reading.

## 12. Open points and risks

* **Nothing is proved.** The checker's invariants I1–I6 have no proof. Both reviews found
  their worst holes by attacking rules by hand, next to rules already reviewed: the
  origin set on every local (review 1), the scope escape and `shared` (review 2). The
  next ones are likely around scopes, `unsafe` signatures in `std`, and `from` clauses
  on functions whose bodies the checker cannot see.
* **Holes found so far, as a pattern:** a property computed structurally that missed a
  case (a raw pointer to a view; `Atomic[*mut T]`); a value whose borrow the analysis
  did not model (a scope; `shared` declared `from self.*`); a check that only one phase
  could make, deferred to nowhere (a length from a constant argument; recursion through
  impl arguments). Look for these first.
* **Holes found in P1b and P2, the same pattern:** a waitable call's result lost its
  operation's origins when the lowering changed shape; `AtomicValue` was satisfied by
  types its own implications excluded; a closure type did not carry its owner's
  arguments, so instances of different owners merged.
* **To confirm, before they are relied on** (review 2): that LLVM no longer requires a
  compare-exchange's failure order to be no stronger than its success order;
  `alwaysinline` at `-O0`; inline stack probes on aarch64 and Windows; the
  `minimumNumber`/`maximumNumber` intrinsics in the pinned LLVM; which linker flags make a Mach-O byte-identical;
  the x86 cost of the float `@min`/`@max` rule. COMPILER.md's back-end section assigns
  each to a step.
* **Still unspecified** (pipeline §11): the builtin reference, `@format`'s type (with
  `std.io`, which also decides whether `Reader` and `Writer` are interfaces), the kernel
  IR, the scheduler's algorithms beyond `docs/std/sched.md`, debug information beyond
  line tables.
