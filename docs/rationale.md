# Ingot v1: rationale

This file records why each major decision in the specification went the way it did and
what was considered instead. It is not normative.

## 1. What "for agents" changes

Three properties of the intended author drove most decisions.

**An agent reasons best about what is in front of it.** It reads one function and the
signatures it calls. So every rule in the language is checkable from exactly that. There
is no inference across function boundaries, no global analysis whose result the author
has to predict, and no error that depends on code elsewhere. This is why borrow flow is
written in signatures and why generic bodies are checked against their bounds once.

**An agent already knows several languages well.** Syntax and semantics that match Rust,
Zig, Go or Swift cost nothing to learn, and anything new is learned only from compiler
errors. So Ingot spends its novelty budget in two places, origins (section 13) and
linear cleanup (section 10), and otherwise takes the most widely shared form: `&`/`&mut`
borrows, `enum` with payloads and `match`, `try`, `defer`, `impl`, `Send`/`Sync`.

**An agent pays for tokens, not keystrokes.** Verbosity is not free: it uses context and
time. The rule applied throughout was that a token must carry information. `&mut x` at a
call site carries information. `p.*.x` for every field read through a borrow does not,
so borrows reach fields directly and only raw pointers need the explicit `.*`. Type arguments that
repeat the argument types do not, so they are inferred. A `defer { x.deinit(); }` line
does carry information, because it is the only place the release happens.

**An agent iterates against the compiler.** So the compiler's output is an API with
fix edits, there are no warnings to weigh, and semantics never depend on the build mode,
because "passes in debug, breaks in release" is a failure an agent cannot see.

## 2. Memory safety without lifetime variables

*Chosen:* owned values, `&`/`&mut` borrows with exclusivity, and origin clauses
(`from`, `into`) in place of lifetime parameters. One origin set per view value.

*Rejected:*

* **Rust's lifetimes as they are.** They are the most expressive option and models know
  them. But lifetime parameters on types, variance, higher-ranked bounds and the
  interaction with trait objects are where generated Rust most often needs several
  rounds, and they are a large implementation. `from`/`into` say the same thing as the
  common Rust signatures (`fn f<'a>(x: &'a T) -> &'a U`) in words tied to parameter
  names, and the checker stays a per-function dataflow.
* **No checking, as in Zig.** Fast to implement and simple, but it leaves
  use-after-free and data races to testing, which is the opposite of goal 4.
* **Garbage collection or pervasive reference counting.** Rules out goal 3.
* **Second-class references only** (Hylo's mutable value semantics). Removes origin
  annotations entirely, but a borrow can then not be stored in a struct, and parsers,
  iterators and arena-backed data all want exactly that.
* **Run-time generation checks everywhere** (Vale). A check on every dereference is a
  cost on the hottest paths. Ingot keeps the idea as an opt-in library type, `Handle`.

*Known cost:* collapsing a view struct to one origin set is stricter than Rust when a
struct holds borrows of things with different lifetimes. The work-around is to split
the struct. If this proves common, named origin parameters on structs can be added
later without changing existing code.

*Known cost:* locks and channels hold only `Owned` values (section 13.6). Rust can put
a `&'a [u8]` in a channel because the lifetime is part of the channel's type. Ingot's
origin sets are computed per function and per moment, and a store made through a shared
borrow by another task is invisible to that. Rather than add type-level lifetimes for
this one case, borrowed data crosses task boundaries only as `spawn` arguments and
`par for` items, and a pipeline passes offsets, handles or owned buffers.

## 3. Linear types and no implicit drops

*Chosen:* the compiler never inserts a cleanup call. Types that own something are
linear and must be consumed; forgetting is a compile error with a fix.

*Rejected:* **Rust/C++/Swift destructors.** They are the more convenient design and
prevent the same leaks. They were rejected for three reasons. They are hidden calls, and
the first principle of the performance model is that code which is not written does not
run. A destructor cannot return an error or take a parameter, so `close()` failures are
swallowed and "which allocator frees this" has to be stored or global. And implicit
teardown of nested owning structures hides an O(n) traversal behind a closing brace,
where the arena alternative is O(1); making the traversal something one has to write
pushes code toward the cheap design.

*Known cost:* one `defer` line per owned value, and `deinit_with` for containers of
owned values. `@forget` exists so that the rule can always be satisfied explicitly.

## 4. Allocators in the type, allocation fallible

*Chosen:* containers are `List[T, A]`, store their allocator, and dispatch statically.
Allocation returns `! AllocErr`.

*Rejected:*

* **Passing the allocator to each call** (Zig's unmanaged containers). It shows
  allocation at every call site, but freeing with a different allocator than the one
  that allocated is undefined behavior the type system cannot see. Storing the allocator
  costs zero bytes for the heap and eight for an arena and closes that hole.
* **A global or context allocator** (C, Rust's default, Odin). Hidden state, and it makes
  arenas the unusual path instead of the usual one.
* **A dynamic allocator interface in every container.** An indirect call for a bump
  allocation that should be three instructions.
* **Abort on out-of-memory.** With fixed-size arenas, running out is ordinary and
  recoverable. And `try` on allocating calls is the visible marker that Zig gets from
  its allocator parameter.

## 5. Errors

*Chosen:* `T ! E` with one concrete error enum per function, `try`, `catch`, `fail`,
and `from` variants for wrapping. Bugs trap and end the process.

* **`fail` as its own statement** keeps `return` meaning success in every function, so
  there is no implicit wrapping of a return value and nothing to get wrong.
* **`from` variants** were preferred to Zig's inferred error sets (which make a
  signature depend on the bodies below it, against the local-reasoning rule) and to a
  single dynamic error type (which loses exhaustive `match` and costs an allocation or an
  indirection).
* **No unwinding.** Unwind tables, landing pads and exception-safety reasoning are all
  cost and complexity for a recovery path that a deterministic trap handles better in a
  language whose recoverable failures are already values.

## 6. Arithmetic and conversions

*Chosen:* trap on overflow in every build, explicit wrapping and saturating operators,
`as` for lossless conversions only.

Silent wraparound is the integer bug that neither tests nor review reliably catch, and a
mode-dependent rule (Rust's debug/release split, or a global flag that removes checks) means the tested
program is not the shipped program. The cost is an add-and-branch-on-flags that both
target cores predict and fuse. Where it matters, section 31 guarantees removal for
counted loops, and `+%` is one character away when wrapping is what the code means.
Integer vectors and kernels have no trapping form at all, because it cannot be made
cheap there, so those contexts require the explicit operators.

## 7. Concurrency

*Chosen:* structured, stackful tasks on a work-stealing scheduler; `scope`, `spawn`,
`par for`; data-race freedom from the borrow rules.

*Rejected:*

* **`async`/`await`** (Rust, Swift, C#). Zero-cost state machines, but two kinds of
  function, a second set of library APIs, and in a borrow-checked language a long list
  of interactions (pinning, borrows across suspension points, cancellation at every
  `await`). That is a large surface for mistakes of exactly the kind goal 4 is about.
* **Threads only.** No runtime, but one OS thread per waiting connection, and no
  cheap fork-join.
* **Unstructured tasks** (Go's `go`). A task that can outlive its caller cannot borrow
  from it, so everything shared must be heap-allocated and reference-counted, and
  leaked tasks and unjoined errors become possible.
* **Preemption.** Needs safepoints or signals. Core counts on these machines are small
  and known, and the OS still preempts the worker threads.

*Known costs:* a runtime of a few thousand lines linked into concurrent programs; stacks
are fixed reservations, so very deep recursion inside a task traps where a growable
stack would not; a blocking foreign call must be declared as such; and `spawn` traps
when task memory runs out, because making every `spawn` fallible would put a `try` on
the most common concurrent statement for a failure that programs cannot usefully
handle.

Stackful tasks and stack borrows fit together only because stacks never move. That is
why Go-style copying stacks were not an option and reservations with lazy commit were.

## 8. SIMD, kernels and the GPU

* **Fixed-width vector types on both CPUs** instead of scalable vectors: the M4 has
  no SVE, and code written against `f32x16` runs correctly on NEON as four registers.
  `std.simd` aliases give the "widest native" spelling for loops that want it.
* **No guaranteed auto-vectorization of ordinary loops.** A guarantee the author cannot
  predict is not a guarantee. The optimizer may vectorize, `ingot cost` says whether it
  did, and certainty comes from vector types or kernels.
* **Kernels** exist because the three wide execution resources on these machines (SIMD
  lanes, cores, the GPU) all want the same thing: a function applied independently to
  every index. Defining that subset once, strictly enough that vectorization and GPU
  translation cannot fail, gives one source for all three and makes data races
  impossible by construction. ISPC, Halide and Futhark are the prior art.
* **Explicit executors.** Whether a GPU wins depends on data size and on where the data
  already lives. On the M4 Pro with unified memory there is no transfer; on the RTX 4070
  there is. A compiler that guesses will sometimes be badly wrong, so the choice is in
  the source and a benchmark decides it.
* **Metal and Vulkan.** Metal is the only compute API on the M4. For the RTX 4070,
  Vulkan runs under both Linux and Windows, so one SPIR-V backend covers both systems.
  Direct3D 12 would be a second backend for Windows alone, and CUDA a vendor SDK for the
  same card.

## 9. Capabilities

Safe code cannot call C and has no mutable globals, so the only way to affect the world
is through a value passed in. That falls out of the safety design at no extra cost, and
the standard library keeps it by taking `&Fs`, `&Net`, `&Clock` and allocators as
parameters instead of offering global functions. The benefits are specific to generated
code: a function's signature shows whether it can touch the disk, tests swap in fakes
without mocking frameworks, and code from an agent can be handed a restricted `Env`.

## 10. Three operating systems

The 7800X3D runs both Linux and Windows, so there are three targets over two CPUs. The
rule was that an operating system may cost work in the runtime and in `std.sys`, but
may not appear in the language or in the public standard library.

* **Memory.** Reserve-then-commit arenas were chosen before Windows was in scope, and
  they happen to be the Windows model exactly. Allocation was already fallible, which
  Windows needs, since it refuses a commit where Linux overcommits.
* **I/O.** Linux and Windows complete operations; macOS reports readiness. Because
  tasks are stackful, `std.io` can be the same blocking-looking call on all three, with
  the difference hidden under the park. This is also why I/O is not a `select` arm: an
  operation that has started cannot be withdrawn the same way on all three.
* **Foreign code.** Three C ABIs is a cost in the compiler, not in programs. The one
  thing a program sees is that C's `long` differs, and Ingot declarations never used
  `long`.
* **No libc.** Linux gets direct system calls and Windows gets `kernel32`/`ntdll`
  imports. macOS has to go through libSystem. Nothing in the language assumes a C
  runtime, which is what makes that split possible.
* **Text.** Paths are bytes. WTF-8 at the Windows boundary keeps one path type and
  loses no file names.
* **Process model.** `std` offers spawning a process with arguments and streams. It
  does not offer `fork` or signals, because they cannot be given the same meaning on
  Windows.

*Known cost:* the runtime's stack switching and fault handling are written three times,
and the Windows version has the most rules to follow. `when` branches for a system the
author is not on are only checked by `ingot check --all-targets`, so that command has
to be part of the normal loop.

## 11. Smaller syntax decisions

| Decision | Reason |
|---|---|
| Case decides type versus value | The parser never needs a symbol table, `List[T]` works in expressions, and an agent cannot name a type like a variable |
| `T.{ ... }` literals | No ambiguity with a block after `if x == y` |
| `.name` shorthand for enum values | Cuts the most repetitive tokens in `match` and `return`, and the expected type is always a declared one |
| Receiver auto-borrow, nothing else | `list.push(x)` is what every prior language writes. Every other borrow is visible |
| Blocks have no value | Removes the "is the semicolon there" class of error. `if` and `match` expressions hold single expressions |
| No shadowing, no truthiness, the parenthesization rule | Each removes a silent mistake that tests and review do not reliably catch |
| `i64` for every length and index | One integer type for sizes on a 64-bit-only language. A negative index fails the same unsigned compare as a too-large one |
| Structs in declaration order | Layout is predictable and FFI-compatible, and `ingot layout` shows the padding |
| No operator overloading, no function overloading, no default arguments | One meaning per name and per operator |
| Import cycles allowed inside a package | Nothing runs at load time, so there is no order to get wrong, and files can stay small enough to read whole |
| Construction respects field privacy, including `@zero` and `@bits` | Without it, safe code could forge a `List` with any pointer or conjure a capability |
| `Option` is `{ none, some }` | All-zero bytes are `.none` for every `?T`, with or without the pointer niche |

## 12. Left out on purpose

Macros and compile-time code generation. Reflection. Inheritance. Exceptions.
`async`/`await`. A garbage collector. Implicit conversions of any kind. Global mutable
state. Separately compiled libraries and a stable ABI between Ingot packages. Run-time
CPU feature detection. Any CPU or operating system beyond the three targets.

**Deferred, not rejected:** tracked in [later.md](later.md), with why each waits and
what would bring it forward.

## 13. Where this design is most likely to be wrong

1. **Linear cleanup may be too noisy** in code with many small owned values. The test is
   to write the standard library's `Map` and a JSON parser both ways.
2. **One origin set per struct may be too coarse** for iterator adaptors that hold a
   borrow of a container and a borrow of a closure's environment.
3. **Fixed task stacks** need a default size that is right for both a recursive parser
   and a hundred thousand idle connections. The compile-time stack bound helps only for
   call graphs without recursion.
4. **Required check removal (section 31) may be too weak** for integer-heavy code, which
   would push authors to `+%` everywhere and lose the point of trapping.
5. **The kernel subset may be too narrow** for real GPU workloads, which often want
   scatter writes and neighbor access with custom boundaries.
6. **`Owned`-only channels and locks may force copies** in pipelines that would pass
   borrowed slices between stages.
7. **Arguments of a task stay borrowed until the scope ends**, not until the task is
   joined. Code that joins early and then wants the data back needs a nested scope.

Each of these is cheap to test with a small prototype before the corresponding part of
the compiler is built.

## 14. Implementation notes

* A compiler for this language is being built on the `compiler` branch (`COMPILER.md`
  records its design and steps). An earlier prototype (a different, smaller language: no
  ownership checking, a Go front end, a direct AArch64 backend) was removed from the
  tree when this specification became the definition of Ingot. It is in git history at
  commit `ab090de`. Its JSON diagnostics and fix application, and its handling
  of the Apple arm64 ABI and Mach-O, are worth reading before writing the new ones.
* The ownership checker needs a control-flow-graph IR between the syntax tree and code
  generation. Build that IR first; everything else hangs off it.
* Optimized builds emit LLVM IR on all three targets. LLVM has tuned `apple-m4` and
  `znver4` models, implements all three C ABIs and object formats, and `lld` links all
  three. The costs are build speed and tracking LLVM's IR across versions. A small
  direct backend for `--fast` builds can come later if compile time demands it. Nothing
  in the language depends on LLVM, so the choice can be revisited.
* Kernels need their own small IR with three emitters: masked vector code, Metal
  Shading Language and SPIR-V.
* **The compiler is bootstrapped in Rust**, and the language is designed so that the
  compiler can eventually be written in Ingot. The reasons:
  * Rust rejects a `match` that forgets a case. A compiler is a few dozen node kinds
    handled across many phases, and a forgotten case is its most common bug. Go, the
    other serious candidate, has no sum types and lets that bug compile.
  * Ingot has ownership, borrows, enums with payloads, arenas and error values, and no
    garbage collector. A Rust compiler built on arenas and indices maps onto Ingot
    nearly structure for structure, so self-hosting is a port. A compiler that leans
    on a garbage collector and interfaces would need its memory management and data
    model redesigned first.
  * LLVM has maintained Rust bindings, and Cranelift, a code generator written in
    Rust, is a candidate for the `--fast` backend. Its support for the three object
    formats and for wide vectors has not been checked.
  * Zig is closest to Ingot's memory model but changes incompatibly between releases,
    and generated Zig is often written for an older one. `zig cc` remains a good
    cross-linker. C++ has the best LLVM interface and none of the safety. OCaml is
    excellent for compilers, but models are less fluent in it.
  * The costs: slower builds than Go, and two similar languages in one repository, so
    Rust syntax will sometimes leak into Ingot source and be rejected.
  * **Bootstrap rules.** Write the compiler in the subset of Rust that Ingot can
    express: arenas and indices or handles for trees and graphs, no reference
    counting, no trait objects where an enum will do, no macros beyond derives, no
    `async`, and as few dependencies as possible. That is what keeps the later port
    mechanical.
* Hosted CI runners are not these CPUs. Tests run on the two machines themselves, or
  under an emulator that implements SME and AVX-512.
* Profile-guided optimization is planned, but with the author as the first reader of
  the profile and the optimizer as the second. Much of what a profile tells a C++
  compiler is already known statically here: trap paths are cold, there is no dynamic
  dispatch unless written, generics are monomorphized and `inline` is explicit. So the
  larger win is expected from showing the author where a check or an allocation is
  hot, which leads to a change in the source. The expected gains are a judgment, not a
  measurement. The constraints are in section 10 of
  [impl/pipeline.md](impl/pipeline.md).
* Post-link optimization is deferred: see the same section.
