# The bootstrap compiler

The first Ingot compiler, written in Rust (rationale §14). The specification in `docs/`
defines the language; `docs/impl/` says how a compiler is organised; this file records
how *this* compiler is built and in what order. Where this file and `docs/` disagree,
`docs/` wins and this file is fixed.

## History

An MVP (tag `mvp-reference`) passed the 99 conformance tests but left most of the
language unimplemented: `const` items did not resolve, bounded generics and interface
methods did not type-check, unknown names became an opaque "external" type that
silenced every check, and the ownership checker had soundness holes (an undeclared
`from` origin, loop-carried loans, `return` inside `par for`). Its lexer, parser,
diagnostics, fix engine and conformance runner were kept; the semantic layer is being
rebuilt here from the specification.

## Rules for everyone working on it

1. **The specification is the only source of language rules.** Silent, ambiguous or
   contradictory spec, or a conformance test that disagrees with it: stop and report.
   Never pick a behaviour in code.
2. **Tests first.** A new diagnostic code, trap or fix gets a conformance test (and a
   `.fixed.ingot` for a fix) before it is emitted. `conformance/README.md` has the rules
   for writing tests.
3. **Never special-case a test** by its file, function or variable names.
4. **No placeholder semantics.** There is no "unknown, so accept" type. An unresolved
   name is an error (E0304/E0305) and its declaration is poison; anything else that is
   not implemented yet is a compiler failure (exit 2, `not implemented: …`), never a
   silent acceptance.
5. **Bootstrap subset of Rust:** arenas and indices for trees and graphs, no `Rc`/`Arc`,
   no trait objects where an enum will do, no macros beyond derives, no `async`, no
   dependencies.
6. **Determinism** (pipeline §2): name tables are `BTreeMap` or sorted `Vec`. A
   `HashMap` may be used for lookup only, never iterated into anything observable.
7. **The gate** is `scripts/verify.sh`: fmt, clippy `-D warnings`, all tests, the
   conformance ratchet (`crates/driver/tests/expected_pass.txt` lists exactly the tests
   that pass; a listed test that fails or an unlisted test that passes fails the build),
   and byte-identical `check --json` output over three runs.

## Crates

| Crate | Phases (pipeline §1) | Contents |
|---|---|---|
| `ingot-diag` | — | spans, sources, the diagnostic type and JSON, the code catalog |
| `ingot-syntax` | 2, 3 | tokens, lexer, AST, parser (kept from the MVP) |
| `ingot-sema` | 4–7 | `when`, name resolution, signatures, type properties, body type-checking, constant evaluation |
| `ingot-cfg` | 8–10 | lowering to a CFG, constant evaluation by interpreting CFGs, the ownership checker, check removal and cost items |
| `ingot-codegen` | 11–14 | instantiation, LLVM IR text, linking with `clang` |
| `ingot-driver` | 1 | loading, the pipeline, `ingot` CLI, fix engine, conformance runner |
| `ingot-lsp` | — | the language server: LSP ↔ `ingot serve` adapter (crates/lsp) |

A crate depends only on crates above it in this table.

## Data model of `ingot-sema`

**Definitions.** Every declared thing gets a `DefId(u32)` in one arena: packages,
modules, functions, structs, enums, variants, fields, interfaces, interface members,
associated types, impls, consts, statics, aliases, generic parameters, extern
functions. `Def { kind: DefKind, name, span, module, vis }`. Language items that are
"builtin names" (pipeline §4.3: `Option`, `Result`, the built-in bounds, scalar types)
are pre-made defs, not strings. The exception is what spec §4.4 writes out as Ingot
text: `Ordering`, `Eq`, `Ord`, `Hash`, `Iter` and `Format`. They are declared in
`crates/sema/src/builtin.ingot`, which `resolve` adds as a hidden module (`std.<builtin>`,
file `<builtin>`) to the package `std`, so that the same code that checks a program's
declarations checks them; the names the module declares become the builtin names.
No diagnostic may point into that file (`scrub_builtin` turns one that would into a
compiler failure). The types that implement them without an `impl` (§4.4) are a table
in `traits.rs` (`builtin_implements`), not impls; `method_candidates` is the lookup
phase 7 uses and covers both.

**Scopes.** Each module has two `BTreeMap<String, DefId>` namespaces (values and types,
pipeline §4.3) holding its items and its imports. Body-local scopes live in the type
checker.

**Types** are interned: `TypeId(u32)` into a table of `TyKind`:
`Int(IntTy) | Float(FloatTy) | Bool | Void | Never | Array(TypeId, Const) |
Tuple(Vec<TypeId>) | Adt(DefId, Vec<GenericArg>) | Optional(TypeId) |
Result(TypeId, TypeId) | Ref(Mutability, TypeId) | Slice(Mutability, TypeId) |
RawPtr(Mutability, TypeId) | FnPtr(FnSig) | Dyn(Mutability, DefId) |
Vector(ScalarTy, u32) | Mask(u32) | Param(DefId) | Projection(TypeId, DefId) |
Closure(owner, expr) | Poison`. Type equality is `TypeId` equality (pipeline §4.4). A
closure's type names the body that contains it and its expression; its signature and
captures are in that body's `TypedBody`.

**Poison** (pipeline §3). A failed name or expression has the poison declaration or
type; every operation with a poison operand yields poison and reports nothing. A body
with any phase-7 error does not enter phases 8–10.

**Type properties** (spec §10, pipeline §4.5): `Copy`, linear, view, `Send`, `Sync`,
computed once per type, by a fixed point over the ADT graph in phase 6. A type parameter
has exactly the properties its bounds grant.

**Signatures** (phase 6) are complete before any body is checked: parameter and result
types, error type, `from`/`into` clauses, generic parameters with bounds, and per-type
`impl` and interface tables for method lookup.

**Typed bodies** (phase 7, `crates/sema/src/body/`, result type in `typed.rs`): the HIR
is not copied or changed. Each function, test, `const` and `static` body gets a
`TypedBody` keyed by the HIR's own ids (`ExprId`, `PatId`, `LocalId`, `StmtId`): the
type of every expression, pattern and local, and everything the source leaves implicit:
the receiver adjustment of a method call, literal types, the resolved callee with its
generic arguments and the interface and impl it was found through, the field or variant
a member name resolved to, the binding mode of every pattern binding, the conversion of a
`try` operand (identity or a `from` variant), how each `for` iterates, and the
signature and captures of each closure. A body in which phase 7 reported an error is
marked `failed` and phases 8 to 10 skip it. Phase 8 reads the HIR and this, nothing else.

**Constant evaluation** (spec §9, pipeline §4.6) has two paths. A direct evaluator in
`consteval.rs` handles literal arithmetic, comparisons, `as` between integers,
`@sizeof`/`@alignof`/`@lanes`, `@target`, and references to other such constants.
Everything else is evaluated by interpreting CFGs, with the bodies it reaches pulled
through phases 7 and 8 on demand and within fixed limits. Both paths share one
arithmetic module with check folding, so a constant and a run-time value of the same
expression never differ.

Sema cannot call lowering (`ingot-cfg` depends on it), so the boundary is two traits in
`crates/sema/src/interp.rs`: `Interpreter`, implemented by `ingot_cfg::eval::Evaluator`,
and `Host`, implemented by phase 6's (and phase 7's) `Cx`. `check_declarations` and
`check_bodies` take `Option<&dyn InterpreterFactory>`; the driver passes an
`EvaluatorFactory`, which makes a fresh `Evaluator` per constant expression, and sema's
own unit tests pass `None`, which leaves `constant evaluation of ...` entries and never
guesses a value. The factory owns the lowering cache, so a body pulled forward is
lowered once (pipeline §4.6: phase 8 at most once). A fresh evaluator per expression
makes evaluation re-entrant: a body pulled forward can evaluate its own constants while
the evaluator that reached it is suspended, and the nested one has its own step and byte
counters. Its calls, however, count toward the 1024-call limit of the evaluation that
pulled its body; the nested evaluator inherits the caller's depth and outermost call, so
a chain of such evaluations stops with one E0808 at the outermost call.
`pipeline::analyze_with` runs on its own thread with a 256 MiB stack
(`PIPELINE_STACK`), so every caller (the binary, the conformance runner, the language
server) has it: 1024 nested levels (typing, lowering, evaluation each use host stack)
need between 16 and 32 MiB in a debug build, and a thread stack is virtual memory. Through `Host` the interpreter asks for the typed body of a
definition, types a signature constant or an `@static_assert` condition on demand
(`Declarations::bodies` and `Declarations::expr_bodies`, reused by phase 7 so no body is
typed or reported twice), reads and writes the value of a constant, and reports
diagnostics. The value of every evaluated `const` and `static` is stored per `DefId` in
`Declarations::values` as `consteval::ValueState`; the scalar ones are mirrored in
`Declarations::consts` for the direct path. The impl tables are built before the first
interpretation; a signature constant that needs the interpreter while they are being
built is E0812. A signature asked for while it is still being built reports E0812 at the
constant whose evaluation asked for it.

**The standard library** is Ingot source in `std/`, checked by this compiler like any
package. It is written only as far as conformance tests and programs need it, and only
with APIs that spec §35 describes. Nothing in `std` is hard-coded in the compiler except
the entry points of pipeline §8.

## Data model of `ingot-cfg`

**One CFG per body.** `CfgKey { owner: DefId, body: Item | Closure(ExprId) | ParFor(StmtId) }`
names a function or test body, a closure body, or a `par for` body. `Cfgs` holds them in
a deterministic order (item bodies first, then the closures and `par for` bodies they
contain, in source order). A key has exactly one CFG: a closure or `par for` inside a
deferred block, which is copied to every exit, is lowered once and every copy names that
CFG. A body phase 7 marked `failed` is not lowered. A construct phase 8 cannot lower yet
is recorded as `lowering: <what>` and the body's CFG is dropped; at S5 there are none in
the suite. An expression that reaches lowering and yields no value is such a construct,
never a silent drop.

**Locals.** `CfgLocal { kind: Param | User | Temp | Scope, name, source: Option<LocalId>,
ty, ty_name, span }`. A user local keeps the HIR `LocalId`; a temporary has no name.
Every local has an explicit `StorageStart` and `StorageEnd` on every path. The one
implicit end is the temporary that holds a `return`/`fail` value: it must survive the
cleanup, so its storage ends at the `Return` terminator itself (`Return.end`). Header
temporaries of a loop are started in the head each iteration and ended at each
iteration's end; a `for` iterand and a scope local live for the whole statement.

**Places.** `Place { base: Local | Static, proj: [Field { index, name } | TupleField |
Index(Operand) | Deref | Payload { variant }] }`. The index operand is kept for check
removal and code generation; the ownership checker ignores it. A place rooted at a
`Static` takes no loans.

**Statements.** `StorageStart/End`, `Assign { place, value }`, `EnterUnsafe`,
`Cancel(s)`, `Abandon(dest)`, `Check(Check)`, `Use(operand)` (a no-op read for
liveness) and `Variant { place, variant, name, holds }`. A variant record is what a tag
test decided: `π is v` at the start of the block the test chose, `π is not v` at the
start of the block for a failed test (`holds` is that boolean). `EnterUnsafe` marks the
entry of every `unsafe` block or expression: it is a no-op for phases 9 and 10, and the
interpreter's E0807 test. A variant record is also a no-op for the interpreter.
`Abandon` is the one addition to pipeline section 5's list: it says an unchosen
`select` arm's result storage holds no value, so the checker does not report the linear
result as leaked.

**Checks.** `Check { kind, place, lo, hi, operands, places, ty, ty_name, op, span }` with kinds
`Bounds(Index | Slice | Vector { width, tail })`, `Overflow`, `Division`, `Shift`,
`Narrow`, `Gather`, `ScatterDistinct` and `DispatchCover`. `place` is the container
whose `len` matters, `lo`/`hi` the index or range, `width` the lane count of a vector
access, `op` the operator text of an overflow check (for E0806), and the trap code
follows the catalog (`check_trap`).

**Rvalues.** a copy or move of a place, a constant, unary and binary operations, a
borrow or the implicit two-phase reborrow of a `&mut`/`[]mut` place, an aggregate
(struct, tuple, array, enum variant, closure captures), a cast (including the `&dyn`
coercion record), the discriminant of an enum, `.len`/`.ptr` of a slice, a slice of a
slice or an array, one `&mut` element of a `[]mut T` loop, and one `Builtin` value with
its name, type argument and operands. A builtin that takes a place (`@replace`,
`@swap`, `@unchecked_index`, `@unchecked_slice`, the memory builtins) keeps it as a
`BuiltinArg::Place`.

**Terminators.** `Goto`, `Branch`, `Switch` on an enum tag, `Return { value, is_error }`
(the error exits of `fail` and a propagating `try`), `Call` (callee, arguments,
destination, continuation), `Trap`, and the concurrency terminators `Spawn`,
`ScopeWait`, `Select` (one successor per arm, each with the operation's call and result
place), `ParFor` (body CFG, captures as shared borrows whose record the body receives
as a tuple of borrows by value, iterand or range, grain) and
`Dispatch` (kernel, executor, range, inputs and outputs with their borrow kind).
`Unreachable` ends a block no path reaches. `Return` also names the temporary whose
storage it ends (`end`), so a checker ends it after reading the value. `Trap` carries
`source`, the assertion's condition text for T08's trap message.

**Dump and validator.** `dump::dump` prints every CFG as the deterministic text its
module documents: named locals, spans, trap codes, `(error)` returns, and
`[defer L:C exit L:C]` on each copied deferred statement. `validate::validate_all`
checks block targets, declared locals, and that the storage start and end of every local
are paired on every path and every local a statement or terminator uses is inside its
storage; the golden runner and the two conformance sweeps run it.

**Constant evaluation by interpretation** (S5b, `crates/cfg/src/eval.rs`). The
evaluator lowers a constant expression or a body on demand (its own `Cfgs`, never part
of `Analysis.cfgs`) and runs it with an explicit frame stack: calls push a frame, never
a host call, so deep Ingot recursion cannot overflow the compiler's stack. The value
tree (`sema::value::Value`) is scalars, aggregates, variants, string literals, function
values, borrows (allocation and path), slices (source, `lo`, `len`) and static
addresses. A whole array of scalars is a `Value::Pack`: the element type, a length, and
the elements' raw bits only once one is written (`@zero` of a 16 MB array uses no host
memory per element); `moved` records elements that were moved out. Reads and writes walk
the path by reference, so one element access costs the path, not the array. A body that
is being typed cannot be typed again: a call that reaches it is E0812, as is a signature
asked for while it is in progress. Each frame's locals and parameters are allocations; a move empties a place,
a read of storage with no value stops evaluation with a `not implemented` entry (phase
9 will report it), and session limits are one counter for the whole constant
expression. Every check runs; a failing check or a `Trap` terminator is E0806 with the
catalog's text and values, the primary span is the operation, and a `call` label marks
the call in the constant expression that led there. Reading a `static`'s value, a local
of an enclosing function, `EnterUnsafe`, an `extern` call and the concurrency
terminators are E0807. Steps (2^24), call depth (1024) and live bytes (2^24) are E0808; parameters count like
locals, and a popped frame releases what it still holds. A closure's size is the sum of
its capture fields (the type table has no closure layout, and asking for one would record
an entry).
Generic functions run uninstantiated with the call's type and constant arguments
substituted (`@sizeof(T)` interns the substituted type), floats are rounded at each
operation to `f32`/`f64`, and a `static` may take another's address. A body phase 7
rejected reports nothing more; a failure the interpreter cannot handle is
`constant evaluation of <what>`, never a guess.

**Phase 9** (`crates/cfg/src/check/`). The driver runs `check::check_ownership` after
phase 8, on every CFG phase 8 produced for the checked package: function and test
bodies, and the closure and `par for` bodies they contain. Constant-evaluation CFGs are
a separate set and are not checked. The analysis does not change the CFG. `check/mod.rs`
is the entry point. `check/moves.rs` is Analysis A (moves and linear values, E1001–E1006).
`check/origins.rs` is Analysis B (origin sets). It computes `O(x)` for every local whose
type is or may be a view, and records the loan created at each borrow. Block entry states
and `sets_before` replay a block up to any statement for Analysis C. The function
boundary reports E1202, E1203, E1204 and E1205. Phase 6 reports E1201 from the signature
when a borrow-written return has no `from` clause; phase 9 runs Analysis B against an
empty clause and the driver attaches that clause to the E1201.
A path is a local followed by projections. A field and a tuple index are distinct per
index, every array or slice element is the one step `[_]`, a dereference is `.*` and a
variant payload is `.v`. Two paths overlap when one is a prefix of the other. The
move dataflow tracks init, moved and maybe, plus the variants an enum path may hold and,
separately, which of those variants are still obligations: both variants of a `T ! E`,
and for every other enum only a variant whose payload is linear. The join of a
`match`, `try`, `catch` or `orelse` does not report a leak that every arm consumed.
A consuming `for` over an array moves each element with `for_elem` and, on the normal
exit only, `iter_consumed` of the array temporary.
`check/loans.rs` is Analysis C (loans, E1101–E1105). It runs after B on every CFG whose
origin analysis did not diverge. Use-liveness of locals is a backward dataflow: a use
is a read, a move, a borrow or a storage-dependent terminator, and a whole-local
assignment kills that local. A loan is live where it sits in `O(v)` and `v` is
use-live, or a live loan is rooted at `v` (a borrow of a container keeps what the
container borrows). A two-phase exclusive loan is shared until the call, `spawn`,
`dispatch`, `select` or `par for` that uses its temporary, and exclusive from that
terminator on. Each access is checked against every live loan that overlaps it and was
not created by the same statement. The kill rule of §4.4 drops the loans an assignment
removes, so those do not report E1101. The storage end of a local reports E1105 for a
loan on that local's storage, except when every holder is kept live only by the
`return` or `fail` that follows: that loan is E1203. It reports E1403 instead when a
live holder is a scope: `spawn` stored the loan into that scope, and the local's
storage ends before `scope_wait`. Each report carries `borrow` and `later_use`.
E1105 carries `scope_end` for each exit span Analysis A would use for E1003, and
omits the label when there is none. E1403 carries `spawn` and `declared`.
`check/tasks.rs` checks the task rules. A `spawn` argument and the spawned call's
result must be `Send` (E1401), and so must a `par for` item. A `par for` body that
reads an outer local whose type is not `Sync` is E1402. A body that assigns to, moves
or exclusively borrows an outer local is E1407, and that is the only code for that
use. Analysis B stores a `spawn`'s argument sets through `S(e)`, exactly as `into`
stores through a parameter: `O(σ) ∪= A` for each scope local in `S(e)`, and the §6.2
check for a scope parameter. There is no "every scope in the function" fallback.
The handle gets `S(e)` plus the call's result set; the loan on the scope in `S(e)`
is what keeps the handle, and anything returned `from` it, from leaving the block.
A scope parameter without `into` is E1204. Each `select` arm is applied on its own
successor.

**Phase 10** (`crates/cfg/src/removal.rs`). A forward dataflow tracks, per integer
local, a set of lower and upper bounds: a constant, a constant generic parameter plus
a constant, or `len(place)` plus a constant. Type ranges and widening casts seed it;
a branch or `@assert` on `x + c` against a constant or `len(place)` adds facts, and so
does a loop counter, `x += c`, and `place.len`. A fact about a local dies when the
local is assigned (except the kept bound of `x += c`); a fact about `len(place)` dies
when the place is assigned. An exclusive borrow of a place rooted at a local resets
that local's bounds, kills every `len` fact that overlaps the borrowed place and
drops the rule-6 checks it protects, at the borrow itself: whether the borrow is
stored in a struct, reassigned in a branch or passed to a call makes no difference.
A two-phase exclusive borrow also kills them again at the terminator that activates
it, because its root may be read between reservation and activation. Phase 10 has no
raw-pointer rule: 02-memory §15 says a raw pointer made from a borrow of a place is
usable only until the place's next direct use, and every fact here is made by such a
use. Joins intersect the bounds, so a fact one side lacks is dropped. The six §31
removals are applied to the check statements: the `for` element access emits no
check; a bounds check goes when the facts put `e` in `0 .. len(p)` (with the width
for a slice or vector); an overflow check when the operands' ranges put the exact
result in the type's range; a check whose operands are all constants is folded, and
a trap is E0806 at the operation; and an identical check dominated with no assignment
in between is dropped. For rule 5 a branch on a constant condition is folded first:
a block that only an untaken edge reaches is dead, its checks are removed and
nothing in it is reported. A bounds check folds when its index and length are both
constants: in range it is removed, out of range it is E0806 with T01 at the
operation. `@gather`, `@scatter`-range, `@scatter`-distinct and `dispatch` coverage
checks are never removed, and `@narrow` is not a cost item. A surviving check becomes
a `removal::CostItem`; `ingot cost` prints those items in `(function, span)` order.
The phase-8 CFG is unchanged, so the dump and its goldens stay phase 8's output;
`Analysis.removed_cfgs` is the CFG with the removed checks gone. Generic bodies are
analysed uninstantiated, with constant generic parameters as symbols. What only
instantiation can decide is one `per-instance checks (pipeline phase 11)` entry: an
`@static_assert` whose condition depends on a generic parameter, a kernel type that
is a type parameter (E1514), a constant evaluation over a generic parameter, and
polymorphic recursion (E0809: a call cycle with an argument that is not a bare caller
parameter; a closure argument is growing, since its environment makes a new type at
every level).

**Defer and evaluation order.** `defer`/`errdefer` are copied at lowering, in reverse
registration order, to every exit of their block, tagged with the defer and the exit;
after that no `defer` exists. Assignment evaluates the target's indices, then the value,
then the target's checks, then the store; a compound assignment reads the place after
the value by the same rule. A `scope s` lowers to storage start of `σ: Scope`, `s = &σ`,
the body, the body's deferred code and storage ends, `cancel(σ)` on an error exit,
`scope_wait(σ)`, storage end of `s`, storage end of `σ`.

## The back end (phases 11–14): design

Not built yet. This is the design the language's second review settled (the decisions
and their reasons are in `docs/impl/decisions.md`); the normative parts are in
pipeline.md §6a, §7 and §8. The steps that build it are B1 to B7 below. Where the spec
and this section disagree, the spec wins and this section is fixed.

**Instantiation.** 11a (discovery, depth, the per-instance codes) is pipeline §6a and
step P2. 11b (specialization) is one CFG per instance by substitution, with a layout
for each concrete type; only `build` runs it. No sharing of code between instances in
v1, and no drop glue, since the language has no destructors.

**IR and pipelines.**
- LLVM IR as text, compiled by `clang`. It keeps the compiler free of dependencies
  (rule 5) and the IR readable in a review. Textual IR is not stable across LLVM
  versions: support one pinned major version, check it at start-up, and fail with a
  named error otherwise.
- `--fast` is the same path at `-O0`. Everything up to and including the IR is shared,
  so "identical semantics, traps included" holds by construction, as long as the IR
  never relies on undefined behavior (pipeline §7): no `nsw`/`nuw` unless the check is
  present or phase 10 removed it, no `inbounds` on `@offset`, no fast-math flags. A
  separate quick back end is an optimization to consider after measurements.
- What must not depend on LLVM's optimizer, because both pipelines promise it: check
  removal (phase 10), `inline fn` (`alwaysinline`), and kernel vectorization for
  `exec.simd` (our emitter writes vector IR; it never asks LLVM to vectorize).
- For build time, `--fast` may split the program into one IR file per package and
  compile them in parallel, since `inline fn` is the only inlining it promises. The
  optimizing pipeline starts as one file and is split when a measurement says so. No
  LTO is needed: the program is already compiled whole.

**Lowering.**

| Question | Answer |
|---|---|
| Calling convention | The target's C convention with our own argument lowering: scalars and pointers as they are; a slice as two arguments; an aggregate of up to two registers split into scalars; a larger one by a pointer to a copy the callee may not change (a moved value needs no copy). Vectors in vector registers |
| `T ! E`, `?T` in memory | The enum layout of §25, nothing else |
| `T ! E`, `?T` returned | The tag in one register, the payload in another when it fits; otherwise the payload through a result pointer and the tag still in a register, so `try` stays one compare and branch |
| Closures | A closure value is its capture record. A call is a direct call of the body with the record by `&`, `&mut` or value |
| `&dyn I` | `{ data, table }`; the table is a static array of the interface's functions for that type, each taking the data pointer as `self`. No size, alignment or cleanup entries |
| Linear values, `defer` | Nothing to do: moves are explicit in the CFG and nothing runs at a scope end. No drop flags |
| Checks | A compare and a branch to a cold block that calls `rt.trap(site, a, b, c)` (pipeline §7) |
| `@uninit`, padding | Storage is not initialized at `StorageStart`; phase 9 guarantees a write before any read. Aggregates are copied whole, padding included. `@uninit(T)` is storage with no write |
| Borrow parameters | `&mut T`: non-aliasing. `&T` of a type with no `Cell` by value: non-aliasing and read-only. Both: non-null, aligned, dereferenceable for the call. These rest on 02 §15's pointer rules and §16.2 item 2 |
| Mangling | Pipeline §7's scheme, completed: a fixed spelling for every type constructor, and names for the bodies that have none in the source (closure and `par for` bodies by owner and source index, `spawn` thunks, method tables, trap sites). Only "deterministic and one-to-one" is normative |
| Stack probes | In the function prologue for frames above a page, with the target's page size (16 KiB on macOS). If LLVM emits them inline on every target, `rt.stack_probe` is not needed |

Float builtins with an obvious but wrong LLVM spelling: `@min` and `@max` are IEEE
`minimumNumber`/`maximumNumber` (not `minnum`/`maxnum`, and not a compare and select);
float `@reduce_add` is the explicit pairwise tree of §26.2, never LLVM's reduction
intrinsic.

**The runtime in Ingot** (`docs/std/rt.md`). No temporary C runtime: B1 writes the
entry, `trap` with its integer printing, `write` and `exit` in Ingot through `extern`.
Thread-local state is one OS slot read by a call (`pthread_getspecific`,
`TlsGetValue`), so no address computed before a park is reused after the task resumes
on another worker. The stack switch is one assembly file per target in `std.sys`,
listed in std's manifest (`[link.<target>] asm`) and assembled by `clang`. The
guard-page handler and thread entry are `export fn`s (they run outside any task). A
function that is not reachable from the roots is not compiled, so the scheduler is
absent from a program that never reaches it, and the entry calls `rt.start` or
`rt.start_tasks` accordingly. The scheduler is built in the three stages of
`docs/std/sched.md`.

**SIMD and kernels.**
- A vector type is an LLVM vector; a 256- or 512-bit one on NEON is split by LLVM's
  legalizer (the "two or four registers" of §26.1). A mask is a vector of bits in
  registers and `N` bytes in memory (§25), converted at loads and stores. `@lookup` and
  the exact shuffles need per-target intrinsics: keep a table per target.
- SME is not in the first back end. The routines of `arch.aarch64.sme` are whole
  routines that enter and leave streaming mode themselves, written in assembly first.
  When they arrive, state the rule that a streaming routine calls nothing that can park.
- The kernel IR is structured, not a CFG: typed expression trees and `if`, loops,
  `break`, `continue`, `return`, lowered from phase 7's typed tree. Two of the three
  emitters want that (SPIR-V needs structured control flow; Metal's input is source
  text). The masked-vector emitter converts branches to masks: an active mask per loop,
  masked stores at the kernel's own index, gathers for `at_clamped` and `at_or`, one
  accumulator per lane for `reduce`. Every integer result, and every float result
  outside `reduce`, is equal across executors, with contraction off in all three
  emitters. Its document, `docs/impl/kernels.md`, is written at step B5. Order:
  `exec.seq` (kernel IR to a scalar loop), `exec.simd`, `exec.par` once the scheduler
  has workers, then the GPU after both CPU targets. The GPU runtime needs a conversion
  between raw pointers and `extern fn` pointers (`dlsym`, the Objective-C runtime,
  Vulkan's function tables); it is deferred to step G1 (rationale §12).

**Linking.**

| | macOS | Linux | Windows |
|---|---|---|---|
| Driver | `clang`, the system linker | `clang`, with `lld` when present | `clang` with `lld-link` |
| C library | libSystem, dynamic (the only choice) | glibc, dynamic | none; imports from `kernel32` and `ntdll` |
| Process entry | `main`, called by the system's start code | `main` | our own entry symbol, with no arguments |
| Arguments | `argc`, `argv`, `envp` | the same | read from the system by `rt.start` |

On Linux, §24.2's "direct system calls" means `std` does not use libc's I/O: the
process still starts, makes threads and loads shared libraries through glibc, and
system calls go through its `syscall` function. A static executable with our own
`_start` is possible later but cannot load a Vulkan driver, so it can never be the only
mode. Position-independent executables always. Every Ingot symbol is local; only the
`export fn`s and the entry are global; symbols are kept unless `--strip`. Unwind tables
are emitted; every function is marked as not unwinding. A build for another target
needs that target's libraries by `--sysroot`; checking never does. (Recorded for later:
the `extern` declarations name every imported symbol, so the compiler could write the
stub libraries a linker needs itself, if each `extern` block named its library.)
`ingot build` needs the pinned `clang` and the system's linker inputs (the Command Line
Tools on macOS; a C library's development files on Linux; an SDK's import libraries on
Windows); `ingot targets` reports what it found.

**Reproducible output** (pipeline §2). No timestamps (`/Brepro` for PE), a content
build id (ELF build-id, Mach-O UUID), relative input paths (on macOS the debug map of
object files is the known leak: `-oso_prefix`, or not keeping it), a fixed input
order. This machine's `ld` documents `-reproducible`, `-no_uuid` and `-oso_prefix`;
which of them are needed is found by B3's three-build comparison, not decided ahead.

**Cost items and debug information.** `indirect_call`, `task_spawn`, `copy` and
`alloc` need no back end (step P3); `lock` after `std.sync`; `park_point` from 11a's
call graph. `not_inlined`, `loop` and `frame` come from LLVM's optimization remarks
(missed inlining, the loop vectorizer) and its stack-usage output, mapped back by line
and column (step B7). Debug information is not needed for the first back end: trap
messages carry their position in the static trap site. Line and column per instruction
and a name per function come with B7; variables and types later; the format is the
platform's.

**To confirm, and when.**

| Unknown | Settle in |
|---|---|
| The pinned LLVM version, and whether Apple `clang` reads its textual IR (this machine has Apple clang 21). It decides what a user must install | the first week of B1 |
| `alwaysinline` honored at `-O0` | B1 |
| Inline stack probes on aarch64 and on Windows | B1 (aarch64), T3 (Windows) |
| LLVM no longer requiring a compare-exchange failure order no stronger than success | R2 |
| `minimumNumber`/`maximumNumber` intrinsics in the pinned version; the x86 cost of the float `@min`/`@max` rule | B4 |
| Which linker flags make a Mach-O byte-identical | B3 |

**The first milestone** (B1 to B3) is the twelve `run`/`trap` tests that need no
vectors and no tasks (six `arith`, two `types`, three `errors`,
`unsafe/raw_round_trip`), plus three programs of real size, written as `run` tests that
report through `@assert`, all under both pipelines with the trap tests printing
byte-identical lines:
1. **SHA-256 of a literal**, compared with the known digest: wrapping arithmetic,
   shifts, arrays, `for` loops, and check removal in a hot loop.
2. **A parser and evaluator for arithmetic expressions** over a byte literal: enums
   with payloads, `match`, `T ! E` with `from` variants, `try`, slices, recursion.
3. **A generic sort and a binary heap over arrays**, used at two element types with a
   closure comparator and with `Ord`: instances, calls through bounds, closures.

Then an arena-backed tree at R1, and a pipeline of tasks over a channel at R3.

## Steps

Each step ends with `scripts/verify.sh` passing and the reviewer having read the full
diff. "Haiku" and "Sonnet" name the subagent that does the work.

| Step | Who | Content | Done when |
|---|---|---|---|
| R0 | Haiku | Workspace restructure; old semantic layer removed; harness fixes; ratchet; spec-snippet parse test | lex/syntax tests pass, gate green |
| S1 | Sonnet | `ingot-sema` skeleton, defs, module scopes, imports and visibility, consts/statics/aliases as names, `when` (phase 4) for the selected target, poisoning; E03xx, E0612, E1606, E17xx | `names/*`; new tests for the uncovered E03xx/E17xx codes |
| S2 | Sonnet | Type table, signatures, generics and bounds, interfaces and impls, type properties, layout (§25); E05xx, E0516, signature-level E08xx, E16xx | new E05xx/E16xx tests |
| S3 | Sonnet | Bodies: expressions, expected types and literals, operators, conversions, calls and method lookup, structs, enums, `match` exhaustiveness, statements; E04xx, E06xx | `types/*`, `syntax/*` semantics |
| S3b | relay | Phase-7 spec decisions: integer literal as float, array `.len`, `&T` to `&dyn I`, `..` in struct patterns, module-qualified builtin type argument | done (78d86f5) |
| S4 | relay | Bodies: errors and cleanup (E07xx), `unsafe` (E13xx), the phase-7 concurrency shape rules (E1404–E1406, E1408–E1410), `@static_assert` (E0810), vector lane and shape rules (E1502, E1503) | `errors/*`, `unsafe/*` reject tests |
| S4b | relay | Kernels and `dispatch` (E1510–E1519), after the `Idx`/`Grid` members are specified | done (e88296d) |
| S4c | relay | The spec review (`228062e`) in phases 1–7: holes H1–H5, lowercase constant generics, open slice bounds, diverging branch blocks, closure bounds, kernel helpers instead of phase 7k, and the smaller rule changes | done (0562994) |
| S4d | relay | The rest of `228062e` in phase 7: builtin signatures (SIMD, `unsafe`), format strings (E0422), function values, `I.name(..)`, `std`'s inherent impls on built-in types | done (93f6a05) |
| S5 | relay | `ingot-cfg`: lowering (pipeline §5) with a textual CFG dump and golden tests | done (dec58aa) |
| S5b | relay | Constant evaluation by interpreting CFGs, with bodies pulled forward (pipeline §4.6, E0806–E0808, E0812) | done (e073e7f) |
| S6 | grok + relay | Ownership, Analysis A: moves and linear values (checker.md §3, E1001–E1006) | done (46ad614) |
| S7 | grok + relay | Ownership, Analysis B: origin sets and the function boundary (checker.md §4, §6, E1201–E1205) | done (ef74f22) |
| S8 | grok + relay | Ownership, Analysis C: loans (checker.md §5, E1101–E1105) | done (12eecab) |
| S9 | grok + relay | Ownership: scopes and tasks (checker.md §7, E1401–E1403, E1407) | done (0747185) |
| S10 | relay | Check removal and `ingot cost` (pipeline §6) | done (633b48e) |
| P1a | relay | Review 2's holes in phases 6–10: the scope loan (H1), `shared` from `self` (H2), E0521 for concrete lengths and sizes (H3), Q7's pointer rule replacing the raw-pointer stopgap (H4), `Atomic` `Send` only if `T: Send` (H6), rule 5 with dead branches and constant bounds checks, shared borrows of temporaries (D15), `Fn`-bound type-parameter flow (S-2), negative literal generic arguments (N4) | done (571c2f8) |
| P1b | relay | Review 2's phase-7 language features: `pub(pkg)`, D18 as changed (safe `Cell.get`, typed orders, `AtomicValue`, atomic builtins, casts, `@offset`), `waitable` operations and `try` in a `select` arm, `@test_env` giving a `TestEnv`, `?extern fn`, `impl I for &T` | conformance tests per feature |
| P2 | relay | Phase 11a: instance discovery, depth, the per-instance codes (E0809, E0806, E0810, E1514, E0521, E0515, E1504); removes the polymorphic-recursion detector and the `per-instance checks` entries | `generics/const_parameter_values`; a `reject` test with an `instantiation` label per code |
| P3 | relay | The cost items that need no back end: `indirect_call`, `task_spawn`, `copy`, `alloc` | `cost` tests per kind; `ingot cost` no longer exits 2 |
| B1 | — | `ingot-codegen`: 11b, layouts per instance, mangling, IR for scalars, control flow, calls, aggregates, checks and trap sites; `rt` core in Ingot (entry, `trap`, `write`, `exit`); linking on `aarch64-macos-m4` | the six `arith` and two `types` run/trap tests |
| B2 | — | Results and optionals, `try`, `catch`, `defer`, generic instances, closures, calls through bounds, `&dyn`, slices and strings, `for` forms, constants and statics as data | the `errors` tests, `unsafe/raw_round_trip`, three programs of real size (review 2 Q6) |
| B3 | — | `ingot test`, `@test_env` with a minimal `Env`, the `main` error line; both pipelines in the gate; byte-identical executables | the gate |
| R1 | — | `std.mem` (`Heap`, `Vm`, `Arena`, `FixedBuf`, `Box`), `list`, `str` | `test` blocks in std; a `run` test with an arena and a list |
| B4, B5 | — | Vectors, masks and the §26 builtins; the kernel IR, `exec.seq`, `exec.simd` | `simd/*`, `kernels/saxpy` |
| R2 | — | Atomics in the back end; `Cell`, `Atomic`, `Mutex`; the inline scheduler | all 19 run/trap tests |
| R3, R4 | — | Parking (stacks, switch, wait queues, `select`, `Chan`, timers, `--seed`, T11, T13); workers and `exec.par` | new concurrency run tests |
| B6, B7, R5 | — | The full C ABI; line tables and the optimizer cost items; `io`, `fmt`, `fs`, `net` | their tests |
| T2, T3, G1 | — | The Linux and Windows targets; Metal, then Vulkan | the suite on each target |

Each B and R step ends with the `run` and `trap` tests it names passing under both
pipelines. A std design document is accepted only when its declarations pass `ingot
check` as stubs, and is written one step before the step that implements it.

Alongside each S step, a Haiku subagent may write the missing conformance tests for the
*next* step's codes (it touches only `conformance/`). The reviewer checks each test
against the specification before it is used.
