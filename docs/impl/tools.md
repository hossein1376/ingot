# Tool protocol

How every `ingot` command is invoked and what it prints in `--json` mode. Agents drive
the compiler through this interface, and editor integrations are adapters over it, so
it is specified as carefully as the language.

Section 33 of the specification lists the commands. This file fixes their output.

## 1. Conventions

* **JSON Lines.** With `--json`, a command writes one JSON object per line to standard
  output, UTF-8, no pretty-printing. Nothing else is written to standard output.
* **Every line has a `type`.** A consumer dispatches on it and ignores types it does not
  know.
* **The last line is always a `summary`.** If it is missing, the command crashed.

  ```json
  {"type":"summary","command":"check","ok":false,"protocol":1,"errors":3,"ms":41}
  ```

* **Exit status.** 0 when `ok` is true, 1 when the command ran and found problems, 2 when
  the compiler itself failed. A consumer should trust `ok`, not the absence of lines.
* **Failures.** A command that stops before doing its work, with status 2, for a reason
  it can name (a path that does not exist, a command or option that is not written yet)
  writes `{"type":"error","message":M}` and then its summary, with `ok` false. Only a
  crash leaves the summary out.
* **Compatibility.** `protocol` is 1. Within a protocol version, fields are added but
  never removed, renamed or given another type. Consumers must ignore unknown fields.
* **Paths** are relative to the package root and use `/` on every system. When `PATH`
  is a single file inside a package (an `ingot.toml` in its directory or an ancestor),
  the package is checked and only the diagnostics of that file are printed; paths stay
  relative to the package root. `build` and `test`, which an error anywhere in the
  package stops, print every diagnostic of the package. A file with no manifest above it is a package of its
  own: its name is the file's stem, its root is the directory containing it, it holds
  that one module, and it depends only on `std`. Its paths are bare file names. A stem
  that is not a value identifier is made one: uppercase ASCII is lowered, every other
  character that cannot be in an identifier becomes `_`, a leading digit gets a `_`
  before it, and a keyword or type name gets a `_` after it (`my-tool.ingot` is
  `my_tool`, `test.ingot` is `test_`).
* **Positions** are objects `{"line":L,"col":C,"offset":O}`: line and column are
  1-based, the column counts bytes, and `offset` is the 0-based byte offset in the file.
  A **span** is `{"file":F,"start":P,"end":P}`, half-open. Offsets are there so that an
  edit can be applied without counting lines.
* **A position argument** on the command line is `FILE:LINE:COL`. It selects the token
  that contains that byte, or the one that ends there.
* **Names** of items are written as full paths: `pkg.module.Type.method`.
* Without `--json` a command prints text meant for a person. That text is not specified
  and must not be parsed.
* **Determinism.** The same inputs give the same lines in the same order, except for
  timing fields (`ms`, `ns_per_iter`) and the `seed` that `ingot test` draws when
  `--seed` is not given.

## 2. Checking and fixing

### `ingot check [PATH] [--target T | --all-targets]`

Runs the front end and the ownership checker (pipeline phases 1 to 10). `PATH` is a
package directory or a single file. The default is the current package.

```json
{"type":"diagnostic","code":"E1104","name":"move_while_borrowed",
 "message":"`buf` is moved while it is borrowed",
 "span":{"file":"src/io.ingot","start":{"line":41,"col":18,"offset":1203},"end":{"line":41,"col":21,"offset":1206}},
 "related":[{"role":"borrow","span":{...}},{"role":"later_use","span":{...}}],
 "fixes":[],
 "targets":["aarch64-macos-m4"]}
```

* Codes, names, roles and fixes are defined in [diagnostics.md](diagnostics.md).
* `targets` lists the targets on which the diagnostic occurs. With `--all-targets` an
  identical diagnostic on several targets is one line with several entries.
* Summary fields: `errors`, `files`, `targets`.
* At a workspace root, `check` runs over every member (and the root, if it is a
  package) and prints one sorted list of diagnostics; `files` counts each file once.

### `ingot fix [PATH] [--dry-run]`

Applies fixes until a round applies nothing, then reports what is left.

```json
{"type":"edit","code":"E0312","span":{...},"text":"let","round":1}
{"type":"diagnostic", ...}
{"type":"summary","command":"fix","ok":false,"protocol":1,"applied":4,"rounds":2,"errors":1}
```

* `edit` lines are in the order applied. With `--dry-run` the files are not written and
  the lines show what would be done.
* `ok` is true only if no diagnostic remains.

### `ingot explain CODE | --all`

```json
{"type":"explain","code":"E1003","name":"linear_not_consumed",
 "condition":"a linear value is still owned at an exit of its scope on some path",
 "sections":["10","11"],
 "bad":"...source...","good":"...source...","fix":"inserts a defer when exactly one consuming function applies"}
```

Everything comes from two sources the compiler embeds when it is built, so the answer
cannot drift from them: the code's row in [diagnostics.md](diagnostics.md) and the
conformance suite.

* `name` and `condition` are the row's name and "Reported when" column (for a trap, its
  "Raised by" column), as plain text: Markdown backticks and emphasis are removed.
* `fix` is the row's "Fix" column, absent when the column is `—`. A trap has none.
* `sections` is the union of the `//! section:` headers of every conformance test that
  marks the code, sorted as section numbers (`2` before `10`, `33` before `33.2`).
* `bad` is the text of the first `reject` test, in byte order of its path, that marks
  this code and no other and has a `.fixed.ingot`, or else of the first that marks this
  code and no other; for a trap, the first `trap` test that marks it. `good` is the
  `.fixed.ingot` of that test, when it has one. Directive lines are removed from both.
  Each is absent when there is no such test.
* A retired code (its row says **retired**) has `"retired":true` and its row's text as
  `condition`.
* `--all` prints one line per code in catalog order. A code that is not in the
  catalog is a failure (§1).

### `ingot fmt [PATH] [--check] [--stdin]`

* Default: rewrites files in place and prints one `{"type":"formatted","file":F}` per
  file changed.
* `--check`: writes nothing and prints `{"type":"unformatted","file":F}` per file that
  would change. `ok` is false if there is any.
* `--stdin`: reads one file from standard input and writes the formatted text to
  standard output with no JSON. A file with a lexical or syntax error is returned
  unchanged, with exit status 1.

## 3. Building and running

### `ingot build [--fast] [--target T] [-o PATH]`

Prints diagnostics as `check` does, then a summary with `artifact` (the path written)
and `target`.

### `ingot run [--fast] [-- ARGS]`

Builds, then runs the program with its standard streams inherited. It has no JSON mode.
An agent that wants structured output runs `ingot build --json` and then the artifact.

### `ingot test [PATH] [--filter TEXT] [--seed N] [--opt]`

Builds the `test` blocks of the package at `PATH` (default: the current directory; a
single file as in §1) with the `--fast` pipeline, or with the optimizing one under
`--opt`. Tests in a dependency are not built. `--filter TEXT` keeps the tests whose
name contains `TEXT`; the others print no line.

Build diagnostics first, if any; when the build fails no test runs. Then one line per
test, in source order (modules by path, then tests by position):

```json
{"type":"test","name":"find reports the first match","module":"t.search",
 "span":{...},"status":"ok","ms":0.4}
{"type":"test","name":"parses empty input","module":"t.parse","span":{...},
 "status":"trapped","trap":{"code":"T01","text":"index 0 out of bounds for length 0","span":{...}},"seed":7731}
{"type":"test","name":"rejects junk","module":"t.parse","span":{...},
 "status":"failed","error":"ParseErr.bad_digit(120)","seed":7731}
```

* `module` is the module's full path; `span` is the whole `test` item.
* `status` is `ok`, `failed` (an error value reached the end of the test block),
  `trapped`, or `crashed` (the process ended any other way: a signal, or an exit status
  other than 0, 1 and 101; the line carries `exit` or `signal`, an integer). `error` is
  the error value in the structural form of 05 §35, printed as the test's module sees
  it. `trap` is the trap line: its code, the text after `trap Tnn: `, and the span of
  its position.
* `seed` is present on a test that did not pass, so the run can be repeated with
  `--seed`. Without `--seed`, the seed is drawn from the operating system's entropy once
  per run. Until the scheduler exists (COMPILER.md R3) it changes nothing.
* Summary fields: `passed`, `failed` (every test that did not pass), `seed`. `ok` is
  true when the build succeeded and no test failed.
* Each test runs in its own process, so a trap in one does not hide the others. Its
  standard streams are captured; the text form prints them for a test that did not
  pass. The harness makes the test's temporary directory (`TestEnv.dir`) before the
  process starts and removes it after the process ends, whatever the outcome.

**The test executable.** One executable holds every selected test. Its generated entry
(pipeline §8) calls `std.sys.rt.test_start` with the tests as a table of functions in
the order above; the harness runs it once per test with two arguments, the test's index
in that table and its temporary directory. The test's `env.args` holds `argv[0]` only.
A test block is compiled as a function that takes the `&TestEnv` and returns 0 when it
passes; a `try` that leaves the block prints `error: ` and the structural form of its
error to standard error and returns 1. `@test_env()` is that parameter, so repeated
calls give the same `&TestEnv` and no global state is needed.

### `ingot bench [PATH] [--filter TEXT]`

```json
{"type":"bench","name":"dot 4096","module":"t.simd","iterations":200000,
 "ns_per_iter":212.4,"cycles_per_iter":934.0,"instructions_per_iter":3170.0}
```

The cycle and instruction fields are absent where the operating system does not expose
the counters.

## 4. Reports

The summaries of `api`, `audit`, `layout`, `targets` and `explain` have only the fields
of §1. A report never needs the back end: it runs the front end as far as `check` does.
`api`, `audit` and `layout` first print the diagnostics `check` would print for the
same path, then the report for what did resolve; any diagnostic makes `ok` false and the
exit status 1, so an empty report is never mistaken for an empty package.

### `ingot api [PATH] [--pkg] [--target T]`

`PATH` is a package, a module (`std.mem`) or an item (`std.mem.Arena`). A `PATH` that
exists on disk is a package directory or a file of one (§1); anything else is a dotted
path resolved in the build of the current directory, that is its packages and `std`. A
package or a module prints all its items; an item prints itself and everything under
it: a type's fields, variants and methods and the `impl I for` lines whose type is it.
The default is the current package. One line per `pub` item, modules in path order
and items in source order within one; `--pkg` adds the `pub(pkg)` items, for work
inside the package, with `"pub(pkg)"` in `modifiers`:

```json
{"type":"api","path":"std.mem.Arena.alloc_slice","item":"fn",
 "signature":"pub fn alloc_slice[T: Copy](self: &Arena, n: i64, fill: T) -> []mut T ! AllocErr from self",
 "doc":"Allocates `n` copies of `fill`.","span":{...},
 "generics":[{"name":"T","bounds":["Copy"]}],
 "params":[{"name":"self","type":"&Arena"},{"name":"n","type":"i64"},{"name":"fill","type":"T"}],
 "result":{"type":"[]mut T","error":"AllocErr","from":["self"]},
 "modifiers":["pub"]}
{"type":"api","path":"std.mem.Arena","item":"struct","signature":"pub linear struct Arena",
 "doc":"...","span":{...},
 "properties":{"copy":false,"linear":true,"view":false,"send":true,"sync":false},
 "size":32,"align":8,"fields":[]}
```

| `item` | Extra fields |
|---|---|
| `fn` | `generics`, `params` (each with `into` when declared), `result` (`type`, `error`, `from`, `reduce`), `modifiers` |
| `struct` | `properties`, `size`, `align`, `fields` (public ones: `name`, `type`, `offset`) |
| `enum` | `properties`, `size`, `align`, `tag_type`, `variants` (`name`, `payload`, `from`, `value`) |
| `interface` | `generics`, `requires` (bounds), `associated_types`, `methods` (paths of their own `api` lines) |
| `impl` | `interface`, `for` |
| `const`, `static` | `value_type` |
| `type` | `aliases` |

* `signature` is the declaration's tokens exactly as written, without the body: the
  source text from its first modifier to the end of the header, with comments removed,
  each run of white space (newlines included) replaced by one space, and no space just
  inside `(`, `[` or before `)`, `]`, `,`. A signature is always one line.
* `doc` is the doc comment, its lines joined by `\n` with each line's `///` and one
  following space removed; absent when there is none.
* A method of an `impl T` or `impl I for T` has the path `T.name`, and of an interface
  `I.name`, with the module path in front. An `impl I for T` has its own line with
  path `T`'s path; an `impl T` has none, only its methods.
* `size`, `align` and offsets are for the target of `--target`, the host by default.
* `span` is the declared name, as in `query symbols` (§5).
* Every type in a line (`params[].type`, `result.type`, `result.error`, a field's
  `type`, `value_type`, `aliases`, a variant's `payload`) is the type as written in the
  declaration, spaced as `signature` is. A generic alias's parameters are in
  `generics`.
* A dotted `PATH` that names nothing in the build is a failure (§1), as a path on disk
  that does not exist is.
* `properties`, `size`, `align` and offsets are absent for a generic type, whose
  properties and layout depend on its arguments.
* An `unsafe fn` has its safety preconditions in `doc`, and `"unsafe"` in `modifiers`.

### `ingot cost [PATH] [--fn NAME]`

```json
{"type":"cost","kind":"bounds_check","fn":"t.parse.digit_at","span":{...}}
{"type":"cost","kind":"alloc","fn":"t.parse.read_all","span":{...},"allocator":"&Arena"}
{"type":"cost","kind":"loop","fn":"t.simd.dot","span":{...},"vectorized":true,"bits":128}
{"type":"cost","kind":"frame","fn":"t.simd.dot","span":{...},"bytes":48}
```

| `kind` | Extra fields |
|---|---|
| `bounds_check`, `overflow_check`, `div_check`, `shift_check` | none |
| `alloc` | `allocator` (its static type), absent when the callee has no generic argument bound by `Allocator`, or more than one |
| `indirect_call` | `through`: `fn`, `dyn` or `extern` |
| `copy` | `bytes` |
| `not_inlined` | `callee`, `reason` |
| `loop` | `vectorized`, `bits` when vectorized, `reason` when not |
| `task_spawn`, `lock`, `park_point` | none |
| `frame` | `bytes` |

The first three rows and the concurrency row are the same in both build pipelines.
`not_inlined`, `loop` and `frame` come from the optimizing pipeline and are absent with
`--fast` (§31). Until B7, `ingot cost` reports the check, `alloc`, `indirect_call`,
`copy`, `task_spawn`, `lock` and `park_point` rows only.

### `ingot audit [PATH]`

```json
{"type":"audit","kind":"unsafe_block","fn":"std.mem.Arena.alloc","span":{...},
 "reason":"the bump pointer was checked against the reserved range above"}
```

`kind` is one of `unsafe_block` (with `reason`), `unsafe_fn`, `extern` (with `abi` and
`symbol`), `export` (with `symbol`), `forget`, `debug`, `unsafe_impl` (with `interface`
and `for`), `static_interior` (with `value_type`).

* It covers every package of the build except `std`; `ingot audit std` audits `std`.
* One line per occurrence: each `unsafe` block, each `unsafe fn` declaration, each
  `extern` and `export fn` declaration, each call of `@forget` and `@debug`, each
  `unsafe impl`, and each `static` whose type has interior mutability (glossary). An
  occurrence inside a `when` branch the target does not take is not listed.
* `fn` is the full path of the function or test that contains it; absent for an item.
* An `extern` line's `abi` is the block's string and its `symbol` the function's name;
  an `export` line's `symbol` is the function's name.
* Lines are sorted by file path and then by the start of the span, as diagnostics are.

### `ingot layout TYPE [--target T]`

```json
{"type":"layout","name":"t.net.Header","size":24,"align":8,
 "fields":[{"name":"kind","type":"u8","offset":0,"size":1},{"name":"len","type":"i64","offset":8,"size":8}],
 "padding":[{"offset":1,"size":7}]}
```

An enum has `tag` (`type`, `offset`, `size`) and `variants`, each with its payload's
`offset` and `size`. A generic type must be given with its arguments.

* `TYPE` is written as in source, with every named type as a full path
  (`app.net.Header`, `std.mem.Arena`, `?&app.Node`, `[4]u32`), resolved in the build of
  the current directory. A type that does not resolve is a diagnostic as in `check`.
* The layout is for the target of `--target`, the host by default.
* `padding` lists every gap, the tail included, in offset order.

### `ingot targets`

One `{"type":"target","name":N,"host":B,"build":K,"missing":[...]}` per target the
compiler supports. `build` is true when everything a build for that target needs was
found (the pinned `clang`, the system's linker inputs, a sysroot for another system),
and `missing` names what was not. A target the back end cannot build for yet has
`build` false and `"code generation for this target"` in `missing`.

### `ingot deps [update NAME [--rev R] | fetch]`

`deps update` and `deps fetch` are the only commands that use the network (05-tooling
§33.2); `ingot deps` itself reads the cache, as `check` does (E1712). It prints one
`{"type":"dep","name":N,"source":"path"|"git","path":P,"rev":R,"hash":H,"via":[...]}`
per package of the graph other than the root and `std`, in graph order (05 §33.2), once
even when several chains reach it. `via` is the first chain, as package names from the
root (first) to the package whose manifest names this one (last): `["app"]` for a
dependency of the root `app`, `["app","csv"]` for one of `csv`. A path dependency has
`path`, relative to the root with `/` separators, and no `rev` or `hash`; a git
dependency has `rev` and `hash` and no `path`. A package taken from the root's
`[override]` has `"override":true`. The summary holds `packages`, the number of lines.

`ingot deps update NAME [--rev R]` fetches `NAME` at `R` (default: the manifest's
`rev`), computes its hash, and rewrites `rev` (as the commit fetched) and `hash` in the
entry for `NAME` in the
root's `[override]` if it has one; otherwise in every git entry for `NAME` in the
manifests reached from the root through path dependencies alone (the root's included).
Only those two values change; the rest of each file is kept byte for byte. It prints the
`dep` line. A `NAME` with no such entry is an `error` line. `ingot deps fetch` fills the
cache for every git dependency of the graph and prints nothing but the summary; it never
edits a manifest, and a fetched tree whose hash differs from the manifest's is E1703. A
fetch failure is an `error` line.

### `ingot asm FILE --fn NAME`

Prints assembly text. It has no JSON mode.

## 5. Queries

These answer questions about a program that otherwise need a text search. They run the
front end only, and they answer for a package with errors as far as the errors allow.

A query takes a position (`FILE:LINE:COL`) or `--name PATH`. The package is the one
`FILE` belongs to (§1). In server mode the position is the argument `at`, as in
`{"command":"query","args":{"kind":"def","at":"src/io.ingot:41:3"}}`; `kind` is the
query's name. A query prints no diagnostics. Its summary is
`{"type":"summary","command":"query","kind":K,"ok":true,"protocol":1,"ms":N}`: `ok` is
true when the query ran, whatever it found, and a position that is not in the file, or
a file that does not exist, is a failure (§1).

The rules shared by the queries:

* **What a position names.** The token it selects (§1). An identifier names what it
  resolves to: a segment of a path in an expression, a type, a pattern or an `import`
  (each segment names its own module or item), a field or method name after `.`, a
  variant, a field name in a struct literal or pattern, a local or parameter, or the
  name in a declaration (which names the declaration itself). `Self` names the
  interface it is written in, or the target type of the impl. A name that resolved to
  nothing (poison) names nothing, and so does a built-in name (a scalar type, `Option`,
  a built-in interface): it has no declaration in a file.
* **`item`** is one of `module`, `fn` (functions, methods, `extern` functions),
  `struct`, `enum`, `variant`, `field`, `interface`, `type` (aliases and associated
  types), `const`, `static`, `test`, `generic` (a generic parameter), `param` and
  `local` (any other binding).
* **`path`** is the item's full path (§1): `app.net.Parser` for an item, the item's path
  and the member's name for a field, a variant or a method of an interface
  (`app.net.Parser.pos`), the target type's path and the method's name for a method of
  an `impl` (`app.net.Parser.rest`), and the module path for a module. A test's path
  is its module's path and its name as written, without quotes. A generic parameter, a
  parameter and a local have their bare name.
* **`span`** is the declared name; **`extent`** is the whole declaration, from its first
  token (after its doc comment) to its last. A parameter or local has no `extent`. A
  module has no name in its file: its `span` is the empty span at the start of the
  file, and it has no `extent`.
* **`signature`** is the declaration as written, without its body: what comes before
  the `{` of a function's or test's block or of a struct's, enum's, interface's or
  impl's members, with the space before it removed. A `const`, `static`, alias, field
  or variant is written whole, without a final `;` or `,`. Parameters, locals,
  generic parameters and modules have none.
* **`doc`** is the doc comment (specification §1): the run of lines that start,
  after spaces, with `///`, ending on the line just before the declaration's first
  token. Each line loses its `///` and one following space; the lines are joined with
  `\n`. Absent when there is none.

### `ingot query def`

Where the thing named at the position is declared.

```json
{"type":"definition","path":"t.parse.Parser.rest","item":"fn","span":{...},"extent":{...},
 "signature":"fn rest(self: &Parser) -> []u8 from self.*","doc":"..."}
```

No line, and `ok: true`, if the position names nothing. Several lines if the name is
ambiguous because of an error.

### `ingot query refs`

Every use of the thing named at the position, across the package and its dependents.

```json
{"type":"reference","span":{...},"role":"call","in":"t.main.run"}
```

| `role` | The use |
|---|---|
| `definition` | the declaration itself |
| `import` | an import of it |
| `read`, `write` | of a variable or field |
| `move` | a use that moves it |
| `borrow`, `borrow_mut` | a borrow of it |
| `call` | a call of a function |
| `type` | a use of a type in a type position |
| `construct`, `match` | of a struct or variant |

The ownership roles are what make this more useful than a text search: "where is this
value moved" and "who borrows this exclusively" are single queries.

### `ingot query type`

The type of the expression or binding at the position.

```json
{"type":"type_at","span":{...},"of":"[]mut []u8",
 "properties":{"copy":false,"linear":false,"view":true,"send":true,"sync":true}}
```

The answer is for the innermost expression whose span contains the selected token, or
for the binding when the token is the name a pattern, `let`, `var`, parameter or loop
binds. `span` is that expression or name. Only the bodies of the package that phase 7
checked are looked at; a position elsewhere (a signature, a comment, a body of another
package), or an expression whose type is unknown because of an error, gives no line.
`of` is written as diagnostics write types.

### `ingot query borrows FILE:LINE`

The loans that are live at the start of that line, in the function that contains it.
This is the question behind most E11xx and E12xx diagnostics.

```json
{"type":"loan","place":"buf","kind":"exclusive","borrow":{...span of the borrow expression...},
 "held_by":["tail"],"last_use":{...span...}}
{"type":"origins","name":"words","of":"[]mut []u8","holds":["loan of `text` at 12:19","loan of `arena` at 12:25"]}
```

* One `loan` line per live loan: the borrowed place, its kind, where it was made, which
  variables hold it, and the last use that keeps it live.
* One `origins` line per view-typed variable in scope: what it may borrow from. A
  parameter origin is written `parameter s` or `inside parameter s`.
* The function must pass type checking. If the ownership checker rejects it, the answer
  is still given, since that is when it is wanted.

### `ingot query symbols [PATH] [--match TEXT]`

One `{"type":"symbol","path":P,"item":K,"span":S,"extent":E,"pub":B,"vis":V}` per item,
public or not, in source order. `vis` is `"pub"`, `"pkg"` or `"private"`; `pub` is true
for `"pub"` only. `--match` keeps items whose path contains the text. The items
are the declarations of the package's files: functions, `extern` functions, structs and
their fields, enums and their variants, interfaces and their members, the members of
impls (an impl itself is not a symbol), constants, statics, aliases and tests. Modules,
generic parameters, parameters and locals are not symbols. In server mode `PATH` is the
argument `path`.

### `ingot query complete FILE:LINE:COL`

The names that could be written at the position, for an editor's completion list. The
position is the cursor: the byte offset it names, not a token. The **partial name** is
the identifier that ends at the cursor, or nothing when no identifier ends there.

```json
{"type":"completion","name":"twice","item":"fn","path":"app.lib.Ticket.twice",
 "signature":"pub fn twice(self: &Ticket) -> i64","doc":"Twice the seat."}
{"type":"completion","name":"seat","item":"field","path":"app.lib.Ticket.seat",
 "signature":"pub seat: i64"}
{"type":"completion","name":"count","item":"local","of":"i64"}
```

What is offered depends on what comes before the partial name:

* **`value.`**: the fields of the value's type, looking through borrows as a field
  access does; its methods (inherent, from the impls of interfaces, and for a type
  parameter those of its bounds); and the members the language gives the type (`len`
  of an array or slice, `ptr` of a slice).
* **`module.`**, where the name before the dot is a module: the items of that module
  that the current module may name.
* **`Type.`**: the variants of an enum, and the functions of the type's inherent impls;
  for an interface, its methods.
* **`@`**: the builtins of the language (spec §1: `@name`), and nothing else. `name` is
  written without the `@`. The answer does not depend on the program.
* **Anything else**: the locals and parameters in scope at the cursor (declared before
  it, in a block, arm, loop or closure that contains it), the generic parameters of the
  enclosing item, the module's items and imports, the built-in names (pipeline §4.3),
  and the keywords. Keywords are offered wherever this context applies; no attempt is
  made to offer only those the grammar allows at the cursor.

Only names that start with the partial name are offered, case included: since types and
values are spelled differently (spec §1), `Ti` offers types and `ti` values. Lines are in
order of `name`, then `path`. Each has `name` and `item` (§5's list, plus `builtin` for
a built-in name, `builtin_fn` for an `@` builtin and `keyword` for a keyword); a
declared item also has `path`, `signature` and `doc` as `query def` gives them; a local
or parameter has `of`, its type, when it is known. A name that appears twice in scope
(a shadowed local) is offered once, as the innermost. A keyword is offered even where a
declared name has the same spelling, which the language does not allow anyway.

The code at the cursor is usually incomplete: `t.` with nothing after the dot is the
common case, and the file then has a syntax error. The answer must be what the program
completed at the cursor would have: how the compiler gets there is not specified. A
context the compiler cannot work out (the type before the dot is unknown because of an
error) gives no lines and `ok: true`.

### `ingot rename FILE:LINE:COL NEW [--dry-run]`

Renames a local, parameter, field, item or module across the package.

```json
{"type":"edit","span":{...},"text":"byte_count"}
{"type":"summary","command":"rename","ok":true,"protocol":1,"edits":14,"files":3}
```

It refuses, with a diagnostic and no edits, if `NEW` is in the wrong identifier class
(E0104), collides with a name in a scope it would enter (E0308) or would shadow a local
(E0309). Renaming a public item does not reach into other packages.

## 6. Server mode

`ingot serve` keeps one compiler process alive and answers requests on standard input
with the same lines as the one-shot commands. It exists for speed, and for editor
adapters that need unsaved text.

```json
→ {"id":1,"command":"check","args":{"path":"."}}
← {"id":1,"type":"diagnostic", ...}
← {"id":1,"type":"summary","command":"check","ok":false,"protocol":1,"errors":1,"ms":6}
→ {"id":2,"command":"query","args":{"kind":"borrows","at":"src/io.ingot:41"}}
```

* One request per line. Every response line carries the request's `id`, as its first
  field, written back as the request gave it. A request is finished when its `summary`
  arrives. Every request has one, the extra requests below included.
* `args` are the command's flags with the leading dashes removed and `-` written `_`,
  and `path` for the positional argument. A path in a request is absolute or relative
  to the server's working directory, as on the command line.
* **A server is a cache, not a second implementation.** For the same file contents it
  must print exactly what the one-shot command prints, except for the `id` field and,
  in each summary, an `exit` field: the status the one-shot command would have exited
  with. The conformance suite is run in both modes.
* A request the server cannot read (not a JSON object, no `command`, a command or
  argument it does not know) is answered with an `error` line and a summary with
  `"exit":2`. Its `id` is `null` when the request has none or cannot be read.
* The server reads files when a request needs them and compares them with what it has.
  It does not need to be told about saved changes.
* Extra requests:

  | `command` | Effect |
  |---|---|
  | `overlay` with `file` and `text` | use `text` in place of the file's contents until cleared. `text: null` clears it |
  | `cancel` with `target` | stop request `target`. Its summary has `"canceled":true` |
  | `shutdown` | finish and exit with status 0 |

* An overlay applies to every read of that file, in any package, `std` included. An
  overlay for a file that does not exist on disk makes it exist for the server: a
  directory listing that would contain it does.
* Requests are answered in order. A `cancel` is acted on at once: a target that has not
  started is answered by its summary alone, with `"canceled":true`. A running target
  may be stopped, and is then answered the same way; one that is not stopped, or has
  already finished, is answered in full. The `cancel` itself always succeeds.
* The end of standard input is a `shutdown`.
* **Incremental work.** After a change inside one function body, the server should not
  repeat pipeline phases 7 to 10 for any other function. After a change to a signature,
  only functions that name the changed item are checked again. This follows from the
  rule that bodies are checked against signatures only.

## 7. Editor adapters

None of these is part of the language, and none needs a design of its own. They are
listed so that the protocol above is known to be enough.

### Language Server Protocol

An LSP server is a translation layer over `ingot serve`.

| LSP | From |
|---|---|
| `publishDiagnostics` | `check` on every change, with `related` as related information |
| code actions | the `fixes` of each diagnostic |
| go to definition, hover | `query def`, `query type` |
| find references, document highlight | `query refs` |
| document and workspace symbols | `query symbols` |
| rename | `rename --dry-run`, returned as a workspace edit |
| formatting | `fmt --stdin` |
| `didOpen`, `didChange`, `didClose` | `overlay` |
| completion | `query complete` |
| semantic tokens | the lexer's token classes alone |
| inlay hints | `query type` for unannotated bindings, `query borrows` for origins |

* **Positions differ.** LSP lines and characters are 0-based, and characters count
  UTF-16 units unless the client agrees to `utf-8` in `positionEncodings`. The adapter
  asks for `utf-8`, and otherwise converts each line.
* Completion is `query complete` (§5). Agents do not need it; editors do.

### Syntax trees for other tools

Highlighters, code indexers and repository hosts want a Tree-sitter grammar. It is
written from [grammar.md](../grammar.md) once the parser exists, and three properties of
the language keep that mechanical. A change to the grammar that breaks one of them
should be refused.

* **No symbol table is needed to parse.** Types and values are different tokens by
  spelling alone.
* **The lexer needs one token of memory**, for digits after a `.` (a tuple field, not a
  float).
* **Highlighting needs no analysis.** Keywords, type names, value names, builtins
  (`@name`), literals and comments are all token classes.

### Formatter

`ingot fmt` needs its own document, because two implementations must produce the same
bytes: indentation, where long calls and signatures break, how comments attach. That
document is not written yet. It can wait for the parser.

## 8. The loop an agent runs

1. `ingot check --json`. If `ok`, go to 4.
2. `ingot fix`. Many diagnostics carry an exact edit.
3. For what remains: read the diagnostic's `related` spans. For a code that is not
   familiar, `ingot explain CODE`. For E11xx and E12xx, `ingot query borrows FILE:LINE`
   shows what is still borrowed and why. Edit, and go to 1.
4. `ingot test --json`. A failing test names its trap or error and a seed.
5. For code that must be fast: `ingot cost --fn NAME` lists the checks, allocations and
   indirect calls that are left.
6. Before handing off: `ingot fmt`, and `ingot audit` if the change touched `unsafe`.

To learn a library, `ingot api PATH` gives signatures and doc comments without reading
its source.
