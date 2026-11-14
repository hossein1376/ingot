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
| S4b | relay | Kernels and `dispatch` (E1510–E1519), after the `Idx`/`Grid` members are specified | `kernels/*` |
| S5 | Sonnet | `ingot-cfg`: lowering (pipeline §5) with a textual CFG dump and golden tests | dumps for every lowering rule |
| S5b | relay | Constant evaluation of function calls over the CFG (E0806–E0808) | `arith/*` constant tests |
| S6–S9 | Sonnet | Ownership: moves and linear values; loans; origins; scopes and tasks (`docs/impl/checker.md`) | `ownership/*`, `borrows/*`, `origins/*`, `concurrency/*` reject tests |
| S10 | Haiku | Check removal and `ingot cost` (pipeline §6) | `cost/*` |
| B1… | Sonnet | Instantiation, LLVM IR, runtime entry and traps, a minimal `std.Env` | `run` and `trap` tests |

Alongside each S step, a Haiku subagent may write the missing conformance tests for the
*next* step's codes (it touches only `conformance/`). The reviewer checks each test
against the specification before it is used.
