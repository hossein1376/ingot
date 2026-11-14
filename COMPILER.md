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
| `ingot-cfg` | 8–10 | lowering to a CFG, the ownership checker, check removal and cost items |
| `ingot-codegen` | 11–14 | instantiation, LLVM IR text, linking with `clang` |
| `ingot-driver` | 1 | loading, the pipeline, `ingot` CLI, fix engine, conformance runner |

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

**Constant evaluation** (spec §9) interprets typed bodies with a step limit, sharing one
arithmetic module with check folding so a constant and a run-time value of the same
expression never differ.

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

**Statements.** `StorageStart/End`, `Assign { place, value }`, `Cancel(s)`,
`Abandon(dest)`, `Check(Check)` and `Use(operand)` (a no-op read for liveness).
`Abandon` is the one addition to pipeline section 5's list: it says an unchosen
`select` arm's result storage holds no value, so the checker does not report the linear
result as leaked.

**Checks.** `Check { kind, place, lo, hi, operands, places, ty, span }` with kinds
`Bounds(Index | Slice | Vector { width, tail })`, `Overflow`, `Division`, `Shift`,
`Narrow`, `Gather`, `ScatterDistinct` and `DispatchCover`. `place` is the container
whose `len` matters, `lo`/`hi` the index or range, `width` the lane count of a vector
access, and the trap code follows the catalog (`check_trap`).

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
storage it ends (`end`), so a checker ends it after reading the value.

**Dump and validator.** `dump::dump` prints every CFG as the deterministic text its
module documents: named locals, spans, trap codes, `(error)` returns, and
`[defer L:C exit L:C]` on each copied deferred statement. `validate::validate_all`
checks block targets, declared locals, and that the storage start and end of every local
are paired on every path and every local a statement or terminator uses is inside its
storage; the golden runner and the two conformance sweeps run it.

**Defer and evaluation order.** `defer`/`errdefer` are copied at lowering, in reverse
registration order, to every exit of their block, tagged with the defer and the exit;
after that no `defer` exists. Assignment evaluates the target's indices, then the value,
then the target's checks, then the store; a compound assignment reads the place after
the value by the same rule. A `scope` lowers to storage start, body, the body's deferred
code and storage ends, `cancel(s)` on an error exit, `scope_wait(s)`, storage end.

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
| S5b | relay | Constant evaluation of function calls over the CFG (E0806–E0808) | `arith/*` constant tests |
| S6–S9 | Sonnet | Ownership: moves and linear values; loans; origins; scopes and tasks (`docs/impl/checker.md`) | `ownership/*`, `borrows/*`, `origins/*`, `concurrency/*` reject tests |
| S10 | Haiku | Check removal and `ingot cost` (pipeline §6) | `cost/*` |
| B1… | Sonnet | Instantiation, LLVM IR, runtime entry and traps, a minimal `std.Env` | `run` and `trap` tests |

Alongside each S step, a Haiku subagent may write the missing conformance tests for the
*next* step's codes (it touches only `conformance/`). The reviewer checks each test
against the specification before it is used.
