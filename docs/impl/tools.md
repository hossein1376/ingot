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
* **Failures.** A command that stops with status 2 for a reason it can name (a path
  that does not exist, a part of the compiler that is not written yet) writes
  `{"type":"error","message":M}` and then its summary, with `ok` false. Only a crash
  leaves the summary out.
* **Compatibility.** `protocol` is 1. Within a protocol version, fields are added but
  never removed, renamed or given another type. Consumers must ignore unknown fields.
* **Paths** are relative to the package root and use `/` on every system. When `PATH`
  is a single file inside a package (an `ingot.toml` in its directory or an ancestor),
  the package is checked and only the diagnostics of that file are printed; paths stay
  relative to the package root. A file with no manifest above it is a package of its
  own: its name is the file's stem, its root is the directory containing it, it holds
  that one module, and it depends only on `std`. Its paths are bare file names.
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
  timing fields (`ms`, `ns_per_iter`).

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

`bad` and `good` are the text of conformance tests for that code, when the suite has
them.

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

### `ingot test [PATH] [--filter TEXT] [--seed N]`

Build diagnostics first, if any. Then one line per test, in source order:

```json
{"type":"test","name":"find reports the first match","module":"t.search",
 "span":{...},"status":"ok","ms":0.4}
{"type":"test","name":"parses empty input","module":"t.parse","span":{...},
 "status":"trapped","trap":{"code":"T01","text":"index 0 out of bounds for length 0","span":{...}},"seed":7731}
{"type":"test","name":"rejects junk","module":"t.parse","span":{...},
 "status":"failed","error":"ParseErr.bad_digit(120)","seed":7731}
```

* `status` is `ok`, `failed` (an error value reached the end of the test block) or
  `trapped`. `error` is the error value as `{}` formatting prints it.
* `seed` is present on a test that did not pass, so the run can be repeated with
  `--seed`.
* Summary fields: `passed`, `failed`, `seed`.
* Each test runs in its own process, so a trap in one does not hide the others.

### `ingot bench [PATH] [--filter TEXT]`

```json
{"type":"bench","name":"dot 4096","module":"t.simd","iterations":200000,
 "ns_per_iter":212.4,"cycles_per_iter":934.0,"instructions_per_iter":3170.0}
```

The cycle and instruction fields are absent where the operating system does not expose
the counters.

## 4. Reports

### `ingot api [PATH]`

`PATH` is a package, a module (`std.mem`) or an item (`std.mem.Arena`). One line per
public item, in source order:

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

* `signature` is the declaration exactly as written, without the body.
* `size`, `align` and offsets are absent for a generic type, whose layout depends on its
  arguments.
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
| `alloc` | `allocator` (its static type) |
| `indirect_call` | `through`: `fn`, `dyn` or `extern` |
| `copy` | `bytes` |
| `not_inlined` | `callee`, `reason` |
| `loop` | `vectorized`, `bits` when vectorized, `reason` when not |
| `task_spawn`, `lock`, `park_point` | none |
| `frame` | `bytes` |

The first three rows and the concurrency row are the same in both build pipelines.
`not_inlined`, `loop` and `frame` come from the optimizing pipeline and are absent with
`--fast` (§31).

### `ingot audit [PATH]`

```json
{"type":"audit","kind":"unsafe_block","fn":"std.mem.Arena.alloc","span":{...},
 "reason":"the bump pointer was checked against the reserved range above"}
```

`kind` is one of `unsafe_block` (with `reason`), `unsafe_fn`, `extern` (with `abi` and
`symbol`), `export` (with `symbol`), `forget`, `debug`, `unsafe_impl` (with `interface`
and `for`), `static_interior` (with `value_type`).

### `ingot layout TYPE`

```json
{"type":"layout","name":"t.net.Header","size":24,"align":8,
 "fields":[{"name":"kind","type":"u8","offset":0,"size":1},{"name":"len","type":"i64","offset":8,"size":8}],
 "padding":[{"offset":1,"size":7}]}
```

An enum has `tag` (`type`, `offset`, `size`) and `variants`, each with its payload's
`offset` and `size`. A generic type must be given with its arguments.

### `ingot targets`

One `{"type":"target","name":N,"host":B}` per target the compiler supports.

### `ingot asm FILE --fn NAME`

Prints assembly text. It has no JSON mode.

## 5. Queries

These answer questions about a program that otherwise need a text search. They run the
front end only, and they answer for a package with errors as far as the errors allow.

A query takes a position (`FILE:LINE:COL`) or `--name PATH`.

### `ingot query def`

Where the thing named at the position is declared.

```json
{"type":"definition","path":"t.parse.Parser.rest","item":"fn","span":{...},
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

One `{"type":"symbol","path":P,"item":K,"span":S,"pub":B}` per item, public or not, in
source order. `--match` keeps items whose path contains the text.

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
| semantic tokens | the lexer's token classes alone |
| inlay hints | `query type` for unannotated bindings, `query borrows` for origins |

* **Positions differ.** LSP lines and characters are 0-based, and characters count
  UTF-16 units unless the client agrees to `utf-8` in `positionEncodings`. The adapter
  asks for `utf-8`, and otherwise converts each line.
* Completion is not specified for v1. Agents do not need it, and it can be added as a
  `query complete` without changing anything else.

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
