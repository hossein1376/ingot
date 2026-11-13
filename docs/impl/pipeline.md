# Compiler pipeline and implementation constraints

This file fixes the order in which a compiler does things, what each phase may assume
about the phases before it, and the behavior that the specification requires but that is
not about the meaning of programs: ordering, determinism, error recovery, lowering and
code generation. "Must" marks a requirement. Everything else is advice.

## 1. Phases

```
 source files ─► 1 load ─► 2 lex ─► 3 parse ─► 4 when ─► 5 resolve ─► 6 signatures
                                                                          │
   10 remove checks ◄─ 9 ownership ◄─ 8 lower to CFG ◄─ 7 type-check bodies
          │
          ▼
   11 instantiate ─► 12 optimize ─► 13 emit ─► 14 link
```

| # | Phase | Input | Output | Error groups |
|---|---|---|---|---|
| 1 | Load | `ingot.toml`, the directory tree, dependencies | the set of packages and module files | E17xx, E0303 |
| 2 | Lex | bytes of one file | tokens with spans, comments kept | E01xx |
| 3 | Parse | tokens | AST | E02xx |
| 4 | `when` | AST, target | AST with untaken `when` branches removed | E0612, E1606 |
| 5 | Resolve | all ASTs of a package | every name bound to a declaration | E03xx |
| 6 | Signatures | item headers | types of all items, layouts, interface tables, type properties | E05xx, E08xx, E1206, E15xx (signatures), E16xx |
| 7 | Type-check | function bodies | typed tree | E04xx, E06xx, E07xx, E08xx, E13xx, E14xx (shape rules), E15xx |
| 8 | Lower | typed tree | one CFG per function, checks explicit | none |
| 9 | Ownership | CFG | the same CFG, verified | E10xx, E11xx, E12xx, E1401 to E1403, E1407 |
| 10 | Check removal | CFG | CFG without the checks §31 requires gone, and the cost items | none |
| 11 | Instantiate | generic CFGs, roots | one CFG per concrete instance | E0806, E0809, E1514 only |
| 12 | Optimize | CFGs | CFGs or back-end IR | none |
| 13 | Emit | | object code, kernel shaders | none |
| 14 | Link | | executable | linker failures, reported as the compiler's own exit status 2 |

`ingot check` runs phases 1 to 10 and, for the three post-instantiation codes, as much
of phase 11 as is needed to find them. It needs no `main` and no linker.

### Requirements on the order

* **Phases 2 and 3 are per file. A file with an E01xx or E02xx error contributes no
  declarations** to later phases, and names that would have resolved into it are
  poisoned (section 3), not reported as undefined.
* **Phase 4 runs before name resolution.** That is why a `when` condition may use only
  `@target`, literals, comparisons and `&& || !` (§7.3): such a condition is evaluated
  with no symbol table at all.
* **Phase 5 resolves a whole package at once**, since modules of one package may import
  each other in cycles (§2). Packages are processed in dependency order.
* **Phase 6 finishes for all items before phase 7 starts for any body.** A body is
  checked against signatures only, never against another body. This is what "all
  checking is local to one function" means in the implementation: phases 7 to 10 can run
  for every function independently and in any order.
* **Generic bodies go through phases 7 to 10 once, uninstantiated**, with type
  parameters as opaque types that have exactly the properties their bounds grant.
  Phase 11 must not be able to produce any diagnostic except E0806 (a constant
  expression that depends on a generic argument traps), E0809 and E1514 (a kernel type
  the chosen executor lacks). If an implementation finds another error only after
  instantiation, the bound system in the specification has a hole: fix it there.
* **Phase 9 never changes the program.** It accepts or rejects.
* **Phase 10 is required in every build mode**, including `--fast`, because the
  specification promises those removals and `ingot cost` reports them.

## 2. Determinism

A build must be a pure function of the source bytes, the compiler version, the target
and the manifest.

* No output may depend on hash-map iteration order, pointer values, thread scheduling,
  the current time, the host name, absolute paths or the environment.
* Declarations are processed in a canonical order: packages by name, modules by path,
  items by source position. Instances are ordered by their mangled name.
* Diagnostics are sorted as [diagnostics.md](diagnostics.md) section 1 says, after all
  phases that will run have run.
* Parallelism inside the compiler is allowed and expected (phases 7 to 10 per function).
  It must not be observable.
* Paths in debug information and trap messages are relative to the package root.

## 3. Error recovery and poisoning

The compiler reports every independent error and nothing that merely follows from one.

* **Parser recovery.** On E0201 the parser skips to the next `;` or to the token that
  closes the innermost open bracket, whichever comes first, and resumes. It reports at
  most one E02xx per statement and per item header.
* **Poison.** An expression that failed to type-check has the poison type. A name that
  failed to resolve is bound to a poison declaration. Any operation with a poison
  operand yields poison and reports nothing. A function whose signature contains poison
  is callable, and calls to it yield poison.
* **Phase gates.** A function body with any error from phase 7 does not enter phases 8
  to 10. Other functions do. So one type error never produces ownership errors in the
  same function, and never hides ownership errors in others.
* **One diagnostic per cause.** For a given loan and a given conflicting access, one
  E11xx is reported, at the first conflicting access in source order. For a given moved
  place, one E1001 per use is reported, but uses that are only reachable through an
  earlier reported use in the same statement are skipped.

## 4. Front-end requirements

### 4.1 Lexing

* Maximal munch, with these exceptions: after a `.` token a digit run is an integer
  (tuple field), and `..` and `..=` are single tokens, so `0..n` is `0` `..` `n`.
* Operators that must be single tokens: `+% -% *% +| -| *|`, their assignment forms,
  `.*`, `.{`, `.[`, `..`, `..=`, `=>`, `->`, `<<=`, `>>=`.
  `-|` followed by an operand is the saturating subtraction. A closure cannot make this
  ambiguous because closures begin with `fn`.
* `&mut` is two tokens, `&` and `mut`.
* The lexer classifies names into `IDENT`, `TYPE`, keyword, scalar type, vector type and
  contextual word by spelling alone.
* Comments are kept on the token stream's side channel with their spans, for `fmt` and
  for doc comments.

### 4.2 Parsing

* The AST keeps every token's span, including parentheses and trailing commas, so that
  fix edits and `ingot fmt` can be exact.
* The parenthesization rule and the ban on chained comparisons are checked while
  parsing binary expressions, not afterwards.
* A statement that begins with `if`, `match`, `spawn`, `dispatch` or `unsafe` is parsed
  as the statement form. The same keyword anywhere else begins the expression form.

### 4.3 Name resolution

* Scopes, innermost first: block locals, function parameters and generic parameters,
  module items and imports, builtin types. There is no prelude. The builtin names are
  exactly the names the compiler itself must recognize, listed in section 8 of the
  specification; everything else is imported.
* Value identifiers and type identifiers are separate namespaces. Variants, fields and
  methods are resolved by type during phase 7, not here.

### 4.4 Types and literals

* Type equality is nominal for structs, enums and interfaces, and structural for
  everything else. Two instances of a generic type are equal if their arguments are.
* An untyped literal is typed by unifying with the expected type if there is one, then
  by defaulting (`i64`, `f64`). The expected type flows down from annotations,
  parameter types, return types, field types and the other operand of a binary
  operator. It never flows from a later use of the variable.
* Method lookup for `v.name(...)`: strip borrows from the type of `v` to get `T`; search
  `impl T`; then the interfaces `T` implements; then, if `T` is a type parameter, its
  bounds. Then insert the receiver borrow the method's `self` type asks for.
* Exhaustiveness of `match` uses the usual matrix algorithm over constructors. Integer
  ranges are intervals. A guard makes its arm count for nothing.

### 4.5 Type properties

Computed once per type in phase 6, by structural recursion with a fixed point for
recursive types (through `Box`, pointers and generic arguments), starting from "has the
property" for `Copy`, `Send`, `Sync` and `Owned` and from "not linear":

| Property | A struct, tuple, array or enum has it when |
|---|---|
| `Copy` | it is declared `copy` (structs and enums) and every part is `Copy` |
| linear | it is declared `linear` or any part is linear |
| view | any part is a view |
| `Send`, `Sync` | every part is, unless an `unsafe impl` says so directly |

A type parameter has a property only if a bound grants it. That makes an unbounded `T`
"maybe linear, maybe a view, not `Copy`, not `Send`", which is the most restrictive
reading and is what makes one check of a generic body valid for every instance.

## 5. The CFG

Phase 8 lowers each function body to a control-flow graph that phases 9 and 10 share.
Its exact form is the implementer's choice. It must have these properties.

* **Locals.** One per parameter, per `let`/`var`, and per temporary. Each has a type and
  a storage range: the points between its declaration and the exit of its block.
* **Places** are a local followed by projections: field, tuple index, element (index
  not recorded), dereference of a borrow, enum payload.
* **Statements** are one of: assign `place = rvalue`; storage start and storage end of a
  local; a check (`bounds`, `overflow`, `division`, `shift`, `narrow`) with its operands
  and its source span; and "no-op with a use", which marks a read for liveness.
* **Rvalues**: a use of a place (marked *copy* or *move* by type), a constant, a unary
  or binary operation, a borrow of a place (shared or exclusive, marked two-phase or
  not), an aggregate, a cast.
* **Terminators**: goto, branch on a value, switch on an enum tag, return, call (callee,
  arguments, destination place, the block to continue in), trap, and the concurrency
  terminators `spawn`, `scope_wait`, `park` and `select`.

Lowering must do the following, because the checker's rules are stated on the result.

* **Evaluation order** is left to right as §5.1 says, made explicit with temporaries.
* **Deferred blocks are copied to every exit of their block**, in reverse order of
  registration. `errdefer` bodies are copied only to error exits. After this there is no
  `defer` in the CFG. A value consumed in a deferred block is therefore simply moved on
  each exit path, and the linear check needs no special case for it.
* **`try e`** becomes: evaluate `e` to a temporary; switch on its tag; on `err`, convert
  (identity or a `from` variant), run the block's deferred code for an error exit, and
  return the error; on `ok`, move the payload out.
* **`catch` and `orelse`** become a switch whose error or `none` arm is the handler.
* **Receiver borrows and argument borrows** written directly in a call are created as
  *two-phase* borrows: created where they appear in evaluation order, activated at the
  call terminator.
* **`match`** becomes switches on tags and comparisons. Bindings are moves, copies or
  borrows of the payload place according to the scrutinee (`v`, `&v`, `&mut v`).
* **`for`** over a range, array or slice becomes a counted loop with no check on the
  element access. Over an `Iter` it becomes a loop of `next` calls.
* **`scope s { B }`** becomes: storage start of `s`; `B`; the deferred code of the block;
  storage end of every local of `B`; a `scope_wait(s)` terminator; storage end of `s`.
  An error exit inserts `cancel(s)` before the wait.
* **Checks are explicit statements**, one per place the specification says a trap can
  occur, placed immediately before the operation they guard. Phase 10 deletes
  statements; it never changes operations.
* **Every statement and terminator keeps the span of the source construct** it came
  from. Compiler-generated moves for deferred code keep the span of the statement inside
  the deferred block, and carry the span of the exit that triggered them as well.

## 6. Check removal (phase 10)

The six required removals of §31 are defined here as rules on the CFG. A compiler may
remove more, but must remove at least these, and only checks that cannot fail.

Facts the analysis tracks at each point, per local of integer type: a lower bound and an
upper bound, each a constant or `len(place) + constant`, valid while neither the local
nor the place is assigned. Facts come from:

* the type's range, and widening casts (`u8 as i64` is in `0..=255`);
* a loop counter of `for i in lo..hi`: `lo <= i < hi` in the body;
* a branch taken on a comparison between a local plus a constant and a constant or
  `len(place)`, and the same comparison inside an `@assert`, from that point on;
* `x.len >= 0` for every slice and array;
* a lower bound on `len(place)` from a comparison of it with a constant, in a branch
  or an `@assert`.

| §31 rule | Remove the check when |
|---|---|
| 1 | it guards the element access generated for `for x in xs` |
| 2, 3 | it is a bounds check on index `e` against `len(p)` and the facts give `0 <= e` and `e < len(p)`. For a slice range or vector access of width `k`: `0 <= e` and `e + k <= len(p)` |
| 4 | it is an overflow check on `a op b` and the facts bound the exact result inside the type's range |
| 5 | both operands are constants. The operation is folded; an overflow is E0806 |
| 6 | an identical check on the same operands dominates it with no assignment to them in between |

The analysis is a forward dataflow with intersection at joins. It does not need
widening: bounds are only ever taken from the listed sources, never computed by
iterating arithmetic around a loop, so the set of facts is finite.

`ingot cost` reports each check statement that survives, with the span of the
expression. Because phase 10 is the same in both pipelines, so is that part of the
report. The optimizer may delete more checks afterwards. Those are not reflected in
`ingot cost`: the report states what is guaranteed, not what happened to be optimized.

## 7. Code generation requirements

These hold for any back end. With LLVM they translate as noted.

* **Integer arithmetic.** `+ - *` are a check statement followed by a wrapping
  operation. A back end must not mark the operation as "cannot overflow" unless the
  check is present or phase 10 removed it. (LLVM: the `*.with.overflow` intrinsics, or
  plain `add` with `nsw`/`nuw` only where proven.) `+% -% *%` never carry such a mark.
* **Shifts** are a check followed by the shift. The shift itself must be defined for
  every count the check allows.
* **Division** is the check for zero, the check for `MIN / -1`, then the division.
* **Floating point.** No reassociation, no contraction of multiply-add, no assumption
  that values are finite or not NaN, no approximate reciprocals. (LLVM: emit no
  fast-math flags; emit `llvm.fma` only for `@fma`.) Float-to-integer conversion goes
  through the semantics of `@narrow` or `@sat`, never through an operation that is
  undefined out of range.
* **Traps** call one cold, non-returning runtime function with the trap code and a
  pointer to a static record holding the position and any operands. Check failures
  branch to an out-of-line block, so the hot path is a compare and a not-taken branch.
* **Cold paths need no profile.** Trap blocks are marked cold. The error arm of a `try`,
  and a `catch` or `orelse` handler that leaves the function, are laid out as unlikely
  by default. A handler that only supplies a value gets no hint.
* **Borrows.** An exclusive borrow parameter may be marked non-aliasing for the
  optimizer. A shared borrow of a type without interior mutability may be marked
  read-only. A shared borrow of a type with interior mutability must not be.
* **Uninitialized values do not exist** in the generated code for safe functions:
  padding bytes are unspecified but every field is written before any read.
* **Layout** follows §25 exactly. The compiler's own calling convention is free, except
  that `extern` and `export` use the platform C ABI of §24.2.
* **Stack frames** larger than one page are probed, so that the guard page is always
  hit before memory beyond it (T11). On Windows this is `__chkstk` or its equivalent.
* **Inlining.** `inline fn` is expanded at every direct call. `noinline` never is.
  Everything else is the optimizer's choice.
* **Symbols.** Ingot functions are mangled as `_I` followed by the length-prefixed
  package, module path and item name, then the generic arguments in a canonical
  spelling. `export fn` uses its name unchanged. Mangled names are part of determinism,
  not of any stable ABI.

## 8. The runtime boundary

The runtime is ordinary Ingot code in `std`, built with the program. The compiler
depends on a small set of entry points in `std.sys.rt`. Their exact signatures belong
to the runtime's own design document, which is not written yet. The compiler needs:

| Need | Used by |
|---|---|
| process entry: build `Env`, call `main`, map its result to an exit status | every executable |
| `trap(code, record) -> never` | every check |
| scope enter, spawn (function pointer plus an argument record), handle join, scope wait, scope cancel | `scope`, `spawn` |
| split-and-run over a range with a body function and a captures record | `par for`, `exec.par` |
| park and wake | `std.sync`, `std.io` |
| select over a fixed list of waitable operations | `select` |
| stack probe target and guard-page handler | function entry |

A program that uses none of the concurrency entry points must not link the scheduler.

## 9. Tools

All tools are front ends to the same pipeline and must agree with it by construction.
The compiler is a library with a query interface ("the diagnostics of this package",
"the type at this position", "the loans live at this line"), and the command line and
`ingot serve` are both thin callers of it. [tools.md](tools.md) defines what they print.

* `ingot fix` uses the fix edits of phase 1 to 9 diagnostics and nothing else.
* `ingot fmt` works on tokens and comments from phase 2 and the AST from phase 3. It
  must be idempotent, and must refuse a file with E01xx or E02xx errors.
* `ingot api` prints from phase 6 results: signatures exactly as written, doc comments,
  type properties, sizes and alignments.
* `ingot cost` prints from phase 10. `ingot layout` prints from phase 6.
* `ingot explain CODE` prints the catalog entry, a minimal failing example and its
  corrected form. The examples are conformance tests, read from the same files.
* `ingot audit` prints from the typed tree: `unsafe` blocks with reasons, `extern`
  declarations, `@forget`, `@debug`, `unsafe impl`, and statics with interior
  mutability.

## 10. Profiling (planned, not specified)

Nothing here is to be built before the first target runs real programs. It is recorded
so that earlier work does not rule it out.

* **One mechanism, two consumers.** `ingot build --profile` inserts counters. Running
  the program writes a profile file. `ingot profile --json` reports it to the author:
  hot functions and loops, and how often each surviving check and each allocation site
  ran. The same file can be given back to the optimizer.
* **The author comes first.** A change the author makes from the report is visible in
  the source and holds for every later build. A profile fed only to the optimizer is
  invisible and goes stale.
* **Instrumentation, not sampling.** Compiler-inserted counters are exact and work the
  same on all three operating systems. Hardware sampling does not.
* **Requirements, whenever this is built:**
  1. a profile is an explicit build input, named in `ingot.toml` and hashed, so that
     section 2 (determinism) still holds;
  2. a profile must not change the meaning of a program, the removals of section 6, or
     the rows of `ingot cost` that are the same in both pipelines. It may change
     inlining, block layout and branch weights only;
  3. a profile that no longer matches the source is ignored function by function, with
     a line in the build summary, never an error.
* **Post-link layout optimization is deferred.** The whole program is already compiled
  together, the existing tools are believed to handle only one of the three object
  formats, and the usual gain is on binaries far larger than these. Revisit if a
  measured program loses time in the processor's front end.

## 11. Known gaps in this document

* The runtime's design (scheduler, stacks, I/O per operating system) is not specified
  beyond §23 and section 8 above.
* The kernel IR and its three emitters (masked vector code, Metal Shading Language,
  SPIR-V) are not specified.
* The standard library's API is an outline (§35), so `alloc`, `lock` and `park_point`
  cost items cannot be fully defined yet.
* The debug-information format is not specified.
