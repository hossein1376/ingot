# Ingot language specification (v0.1, draft)

Ingot is a statically typed, ahead-of-time compiled systems language. It compiles
directly to AArch64 machine code for Apple silicon (`macos-arm64`) and for Linux
(`linux-arm64`). This document describes what the compiler in this repository
implements. Parts that are planned but not implemented are marked **(planned)**.

Design rule: **one obvious spelling, no implicit behavior.** Whenever something is
ambiguous, the compiler reports an error with a fix instead of guessing.

## 1. Lexical structure

* Source is UTF-8. Identifiers are `[A-Za-z_][A-Za-z0-9_]*`. Line comments start with `//`. There are no block comments, which keeps tools simple.
* Keywords: `import enum switch defer try catch inline fn pub extern struct let var const if else while for in return break continue true false null as test`.
* Builtins are spelled `@name`, so they live in a separate namespace and can never clash with user names.
* Integer literals: `123`, `1_000_000`, `0xff`, `0b1010`. Float literals: `1.5`, `2e10`, `1.0e-3`. A float needs a digit on both sides of the dot (`1.` is not a float), so `0..n` is never ambiguous.
* String literals: `"..."` has type `[]u8` (pointer and length, with no terminator counted in `.len`). `c"..."` has type `*u8` and is NUL-terminated, for C. Escapes: `\n \t \r \0 \\ \" \' \xHH`.

## 2. Grammar (EBNF)

```ebnf
file        = { decl } ;
decl        = import_decl | fn_decl | struct_decl | enum_decl | global_decl | test_decl ;
enum_decl   = "enum" IDENT [ ":" type ] "{" [ member { "," member } [ "," ] ] "}" ;
member      = IDENT [ "=" expr ] ;                (* constant; default = previous + 1, first = 0 *)
import_decl = "import" STRING [ "as" IDENT ] ";" ;
fn_decl     = [ "pub" ] [ "extern" ] [ "inline" ] "fn" IDENT "(" [ params ] ")" [ "->" type [ "!" type ] ] ( block | ";" ) ;
                                              (* "! E": fallible, E is an enum with non-zero members *)
params      = param { "," param } [ "," "..." ] | "..." ;      (* "..." only in extern fns *)
param       = IDENT ":" type ;
struct_decl = "struct" IDENT "{" [ field { "," field } [ "," ] ] "}" ;
field       = IDENT ":" type ;
global_decl = ( "const" | "var" ) IDENT ":" type "=" expr ";" ;
test_decl   = "test" STRING block ;

type        = IDENT                         (* i8..i64 u8..u64 f32 f64 bool void, vector types, struct names *)
            | "*" type                      (* pointer *)
            | "[" INT "]" type              (* array, fixed length *)
            | "[" "]" type                  (* slice: { ptr: *T, len: i64 } *)
            | "fn" "(" [ type { "," type } ] ")" [ "->" type ] ;   (* C function pointer *)

block       = "{" { stmt } "}" ;
stmt        = ( "let" | "var" ) IDENT [ ":" type ] "=" expr ";"
            | "if" expr block [ "else" ( block | if_stmt ) ]
            | "while" expr block
            | "defer" block
            | "switch" expr "{" { expr { "," expr } "=>" block } [ "else" "=>" block ] "}"
            | "for" IDENT "in" expr ".." expr block
            | "return" [ expr ] ";"           (* in a fallible fn also: return @err(expr); *)
            | "break" ";" | "continue" ";"
            | block
            | expr ASSIGN_OP expr ";"       (* = += -= *= /= %= &= |= ^= <<= >>= *)
            | expr ";" ;                    (* calls, try or catch only *)

expr        = or [ "catch" IDENT "=>" ( block | or ) ] ;   (* only on a fallible call *)
or          = and { "||" and } ;
and         = cmp { "&&" cmp } ;
cmp         = bit [ CMP_OP bit ] ;          (* == != < <= > >=, non-associative *)
bit         = shift { ("&" | "|" | "^") shift } ;
shift       = add { ("<<" | ">>") add } ;
add         = mul { ("+" | "-") mul } ;
mul         = cast { ("*" | "/" | "%") cast } ;
cast        = prefix { "as" type } ;
prefix      = ("-" | "!" | "~" | "&" | "try") prefix | postfix ;   (* try: only on a fallible call *)
postfix     = primary { "(" args ")" | "[" expr "]" | "[" expr ".." expr "]" | "." IDENT | ".*" | ".{" inits "}" } ;
primary     = IDENT | INT | FLOAT | STRING | CSTRING | "true" | "false" | "null"
            | "(" expr ")" | "[" INT "]" type ".{" inits "}" | BUILTIN [ "(" [ type "," ] args ")" ] ;
inits       = [ IDENT ":" expr { "," IDENT ":" expr } | expr { "," expr } ] [ "," ] ;
```

**The parenthesization rule (E0106).** In one unparenthesized expression, a bitwise
operator (`& | ^`) or a shift (`<< >>`) may not be combined with arithmetic,
comparisons, another shift, or a *different* bitwise operator. `a & b == 0`,
`a + b << 2` and `a | b & c` are all errors. The fix inserts the parentheses that
match the grammar above. Chained comparisons (`a < b < c`) are also errors.

**Statement-level rules:**
* Every variable must be initialized. Write `= @zero` for an explicit zero value (E0105).
* Only calls may be expression statements (E0303). There is no `x;` or `a == b;`.
* Braces are mandatory around every `if`, `while` and `for` body.

## 3. Types

| Type | Size/align | Notes |
|---|---|---|
| `i8 i16 i32 i64`, `u8 u16 u32 u64` | 1, 2, 4, 8 | two's complement |
| `f32 f64` | 4, 8 | IEEE-754 |
| `bool` | 1 | `true` or `false`; never an integer |
| `void` | 0 | return type only |
| `*T` | 8 | pointer; may be `null`. No pointer arithmetic: index with `p[i]` or take `&p[i]` |
| `[N]T` | N·size(T), align(T) | value type; assignment copies |
| `[]T` | 16, 8 | `{ ptr: *T, len: i64 }`; `.ptr` and `.len` are read-only |
| `struct S {...}` | C layout | fields in order, natural alignment, size rounded to alignment |
| `enum E: T {...}` | as T (default `i32`) | distinct type; members `E.Name`; only `==`/`!=`; convert with `as` |
| `fn(A, B) -> R` | 8 | a C function pointer. Every Ingot `fn` is a valid value of its type |
| `f32x4 f64x2 i32x4 i64x2 u8x16` | 16, 16 | one 128-bit NEON register |

**No implicit conversions.** Values only flow between identical types. The exceptions
are `null`, which converts to any pointer or `fn` type, and untyped numeric
literals, which take the type their context expects. Integer literals without a
context default to `i64` only *inside* arithmetic whose other operands are also
literals. `let x = 1;` is accepted and makes `x` an `i64`, because the right-hand
side is a constant expression. `let p = null;` and `let z = @zero;` are errors (E0204).

**Casts (`as`)** are explicit and total:
* int to int: truncate or extend, using the source's signedness.
* int to float: exact, or rounded to nearest.
* float to int: truncates toward zero and **saturates**; NaN becomes 0 (`fcvtzs`/`fcvtzu` semantics).
* f32 to f64: widening or rounding.
* bool to int: 0 or 1.
* pointer, `fn`, and `i64`/`u64` convert among each other, as do vectors of equal size (bit reinterpretation).

## 4. Semantics

* **Evaluation order** is left to right: operands, arguments, then the assignment target before the value.
* **Integer arithmetic wraps** (two's complement), for every width. Narrow types are kept
  sign- or zero-extended in registers, so wrapping is exact.
* **Shifts**: the shift amount is masked to `width-1`. `>>` is arithmetic for signed types and logical for unsigned ones.
* **Division**: `/` truncates toward zero, and `%` has the sign of the dividend. Dividing by zero
  **panics** in checked builds. `MIN / -1` wraps to `MIN`.
* **Floats** follow IEEE-754 with no fast-math. Every comparison involving NaN is false except `!=`.
* `&&` and `||` short-circuit.
* **Bindings**: `let` is immutable (its address cannot be taken, and it cannot be assigned to;
  fix: E0301 → `var`). `var` is mutable. Names may not shadow an outer local (E0211).
* **Aggregates** (arrays, structs, slices) are values. Assignment and argument passing copy them.
  Ingot-to-Ingot calls pass aggregates by hidden reference to a caller-owned copy and return them
  through `x8` (the AAPCS64 indirect-result register).
* **Control flow**: a function with a non-`void` result must return on every path (E0401).
* **switch**: case values must be compile-time constants of the scrutinee's type, with no duplicates (E0502).
  There is no fallthrough. A switch on an enum must cover every member or have an `else` arm (E0504,
  whose fix inserts the missing arms). A switch on a plain integer must have an `else` arm (E0503).
  `break` and `continue` inside a switch refer to the enclosing loop.
* **inline fn**: a direct call to an `inline fn` is expanded at the call site. Its parameters and
  locals live in the caller's frame and take part in the caller's register promotion. Arguments
  are evaluated left to right before the body runs, exactly as for a call. The function is still
  emitted normally, so its address can be taken. Inline fns may not be extern, variadic or
  fallible, take or return aggregates (E0244), or reach themselves through other inline fns
  (E0245). Inlining is never implicit: the compiler inlines only what you mark.
* **defer**: `defer { ... }` schedules a block to run when the enclosing block exits, whether it
  falls off the end or leaves through `return`, `break` or `continue`. Deferred blocks of one scope
  run in reverse order of declaration, innermost scope first. A `return` value is computed before
  any deferred block runs. A defer body may not contain `return`, a `break`/`continue` that leaves
  it, or another `defer` (E0308). Defers are expanded inline at each exit, so they cost nothing at
  run time beyond the deferred code itself; there is no unwinding, so `@panic` and failed checks
  do not run them.
* **Field access through a pointer is explicit**: `p.*.x`, never `p.x` (E0217 suggests the fix).

## 5. Memory model

* There is no garbage collector, no hidden allocation and no runtime. The heap is whatever you call: `malloc`, `mmap`, an arena you write.
* Locals live in registers or in the stack frame. `&x` takes the address of a `var` local.
  Ingot does not prevent dangling pointers in v0.1 (see ROADMAP: region checks).
* **Checked mode (the default)** inserts bounds checks on array and slice indexing and slicing,
  plus divide-by-zero checks. A failed check prints `file:line:col: message` to stderr and
  exits with status **101**. `--unchecked` removes these checks for audited hot code.
  Pointer indexing `p[i]` is never checked.
* **Atomics**: `@atomic_load` is acquire (`ldar`) and `@atomic_store` is release (`stlr`).
  `@atomic_add` and `@atomic_cas` are sequentially consistent read-modify-writes (`ldaddal`, `casal`).
  `@fence()` is `dmb ish`. Plain loads and stores are not atomic, and racing on them is undefined.

## 6. Error handling

* **Compile time**: every diagnostic has a stable code (`E0xxx`), a span, optional notes and
  optional fixes. `--diag json` prints one JSON object per line:

```json
{"severity":"error","code":"E0205","message":"type mismatch: expected i64, found i32 (Ingot has no implicit conversions)",
 "span":{"file":"f.ig","start":{"line":2,"col":12},"end":{"line":2,"col":13}},
 "fixes":[{"message":"convert explicitly with 'as'","edits":[
   {"span":{"file":"f.ig","start":{"line":2,"col":12},"end":{"line":2,"col":12}},"text":"("},
   {"span":{"file":"f.ig","start":{"line":2,"col":13},"end":{"line":2,"col":13}},"text":") as i64"}]}]}
```

  Columns count bytes and are 1-based. Spans are half-open. `ingot fix FILE` applies the first
  fix of each diagnostic, whole and only if it does not overlap a fix already taken, then
  re-checks, for up to 8 rounds. `ingot explain CODE` documents every code.
* **Run time**: `@assert(cond)` and `@assert(cond, "msg")` are always checked, in every build mode.
  `@panic("msg")` and `@unreachable()` work the same way.
* **Recoverable errors** use error unions. `fn parse(s: *u8) -> i64 ! ParseErr` returns either an
  `i64` or a member of the enum `ParseErr`, whose members must all be non-zero (E0242), because 0
  means success. Inside such a function, `return v;` succeeds and `return @err(ParseErr.empty);`
  fails (`@err` anywhere else is E0253).
  A call to a fallible function cannot be ignored or used as a plain value (E0250). The caller picks
  one of these forms:
  * `try f(x)` returns the same error from the enclosing function after running its pending
    `defer` blocks. The enclosing function must fail with the same enum (E0251). `try` is not
    allowed in `main`, in tests or in a defer body.
  * `f(x) catch e => fallback` gives a value of the call's result type when the call fails. `e` is
    the error, and `_` ignores it.
  * `f(x) catch e => { ... }` runs a block when the call fails. If the call has a result, the block
    must leave through `return`, `break`, `continue` or `@panic` (E0252). If the call is `void`, the
    block may fall through.

  ABI: the result is in x0/d0 as usual, and the error code is in x1 (0 = success). Each check is one
  `cbz`, and there is no unwinding. In v0.1 a fallible function returns a scalar, pointer or void,
  extern functions cannot be fallible, and fn-pointer types cannot spell `!`.

## 7. SIMD

Vector types are first-class values. Each one occupies one 128-bit NEON register.

* `+ - *` are lane-wise. `/` works on float vectors. `& | ^` work on integer vectors. Unary `-` works on all of them.
  `i64x2 * i64x2` is rejected because NEON has no instruction for it.
* `v[i]` with a constant `i` reads a lane.
* `@splat(V, x)`, `@load(V, p)` and `@store(p, v)` (unaligned, `ldr q`/`str q`),
  `@reduce_add`, `@reduce_min`, `@reduce_max`, `@fma(a, b, c)` (`fmla`), `@min`, `@max` and `@sqrt`.
* `@lanes_lt`, `@lanes_le` and `@lanes_eq` compare lane by lane and return a mask: an integer
  vector with the same lane count and width (`f32x4` gives `i32x4`, `f64x2` gives `i64x2`, and
  integer vectors give their own type) whose lanes are all ones or all zeros. They compile to
  `fcmgt`/`fcmge`/`fcmeq`, `cmgt`/`cmge`/`cmeq`, or the unsigned `cmhi`/`cmhs`.
  `@select(mask, a, b)` on vectors picks per lane (`bsl`). On scalars, `@select(cond, a, b)`
  compiles to `csel`/`fcsel`. Both operands are always evaluated.
* Vectors convert lane-wise with `as` (`f32x4 as i32x4`, and the reverse).
* Vector locals in leaf functions stay in `v8`–`v15` across loops. Temporaries use `v16`–`v31`.

## 8. Concurrency

The language has no runtime threads. You call pthreads through FFI, and any `fn(*u8) -> *u8`
can be passed as the thread entry point. Core steering is explicit: `@qos(class)` calls
`pthread_set_qos_class_self_np` on macOS, where user-interactive (0x21) favors P-cores and
background (0x09) favors E-cores. On Linux it does nothing and returns 0. See `examples/threads.ig`.
**(planned)** a `std.par` work-stealing pool that follows Apple's advice to split work into at
least 3× as many chunks as cores.

## 9. FFI to C

* `extern fn name(params) -> R;` declares a C function. Calls use the platform C ABI, including the
  Darwin variadic rule: variadic arguments go on the stack.
* Variadic arguments must be scalars or pointers, and floats must be `f64`. Ingot never promotes
  silently; E0216 offers the fix.
* `pub fn` functions are exported with their plain name, so C can call them. Every Ingot function
  uses the C calling convention for scalar parameters.
* v0.1 limitation: an `extern fn` cannot take or return a struct by value (E0208). Pass a pointer instead.

## 10. Builtins

Run `ingot builtins` for the list the compiler actually implements:

```
@assert(cond [, msg])   @panic(msg)   @trap()   @unreachable()   @zero   @sizeof(T)
@splat(V, x)  @load(V, p)  @store(p, v)  @reduce_add/min/max(v)  @fma(a,b,c)  @min  @max  @sqrt
@atomic_load(p)  @atomic_store(p, v)  @atomic_add(p, v)  @atomic_cas(p, old, new)  @fence()
@ticks() (CNTVCT_EL0)  @tick_freq() (CNTFRQ_EL0)  @prefetch(p)  @qos(class)
```

## 11. Tests

`test "name" { ... }` blocks are compiled only by `ingot test`. The test runner calls each one in
source order and prints `test "name" ... ok`. The first failing `@assert` prints its location
and the failing source expression, then exits with 101.

## 12. Imports and the standard library

`import "path.ig";` adds another file's declarations to the program. A relative path resolves
against the importing file, and each file is loaded once even if several files import it.
Paths that start with `std/` come from the standard library, which is **embedded in the
compiler binary**, so nothing needs installing. `test` blocks are compiled only from the
root file.

**Namespaced modules.** `import "path.ig" as m;` keeps that file's names out of the importer's
namespace: they are reached as `m.name` in expressions and types (`m.Vec`, `m.Vec.{ ... }`,
`m.Axis.x`, `m.dot(a, b)`, `try m.map_put(...)`, `@sizeof(m.T)`). Inside the module its own
names stay unqualified. Each module file is instantiated once, however many files import it
and under whichever aliases, so its globals are shared. Without `as`, an import merges into
one shared namespace as before. A plain name that only exists inside a module gets an E0203
or E0212 fix that qualifies it, which `ingot fix` applies. Symbols are mangled `m.f` to
`ingot__m__f` (a `pub fn` in a module exports `m__f`). Two aliases with the same name in one
file are E0005. Extern functions are never namespaced. Several files may declare the same
`extern fn` as long as the signatures are identical.

| Module | Contents |
|---|---|
| `std/libc.ig` | `printf`, `snprintf`, `malloc`/`free`, `memcpy`, `strlen`, `write`, `sysconf`, ... |
| `std/mem.ig` | `page_size()` (queried at run time: 16 KiB on Apple silicon), `Arena` bump allocator |
| `std/par.ig` | `parallel_for(n, threads, qos, ctx, body)` with dynamic chunking, `cpu_count()`, `perf_core_count()` (`hw.perflevel0.physicalcpu`), and QoS constants |
| `std/io.ig` | `read_file(path, &out)` (whole file into a malloc'd `[]u8`, NUL-terminated), `write_file(path, data)`, both failing with `IoErr` |
| `std/time.ig` | `now_ns()` (monotonic `clock_gettime`: clock id 6 on macOS, 1 on Linux) and `since_s(start)` |
| `std/map.ig` | `U64Map`: open addressing with linear probing, splitmix64 hash, growth at 70% load. `map_init`, `map_put`, `map_get` (fail with `MapErr`), and `map_free` |

**Target-conditional code.** `@is_macos()` is a compile-time constant. In
`if @is_macos() { A } else { B }`, only the branch for the current target is compiled,
so `A` can call macOS-only functions such as `sysctlbyname`.

## 13. Program entry

An executable must declare `pub fn main() -> i32` (E0210, which also offers a fix). The return value is the exit status.
