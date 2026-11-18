# Ingot v1 specification, part 5: tooling and the standard library

The tools are part of the language definition. An implementation that compiles Ingot but
does not provide `check --json`, `fix`, `cost` and `audit` is not complete, because the
intended author cannot work without them.

## 30. Diagnostics

* Every diagnostic is an **error**. There are no warnings, lints or notes that can be
  ignored. Something is either allowed or it is not.
* `ingot check --json` prints one JSON object per line, in a stable order:

  ```json
  {"type":"diagnostic","code":"E1104","name":"move_while_borrowed","message":"`buf` is moved while it is borrowed",
   "span":{"file":"io.ingot","start":{"line":41,"col":18},"end":{"line":41,"col":21}},
   "related":[{"role":"borrow","span":{"file":"io.ingot","start":{"line":38,"col":16},"end":{"line":38,"col":20}}},
              {"role":"later_use","span":{"file":"io.ingot","start":{"line":44,"col":9},"end":{"line":44,"col":14}}}],
   "fixes":[{"message":"move after the last use of the borrow","edits":[...]}]}
  ```

* A code is stable forever and has an entry in `ingot explain CODE` with a minimal
  failing example and its corrected form. A test fails the build of the compiler if it
  can emit a code the catalog lacks. The catalog is
  [impl/diagnostics.md](impl/diagnostics.md): every code, the exact condition, the
  primary span, the roles and the fix.
* The order is: by file path, then by the start of the primary span, then by code.
* A file with a lexical or syntax error is not analyzed further. Other files are.
* `related` carries the other places that explain the error, each with a fixed `role`:
  for ownership errors `borrow`, `move`, `later_use`, `declared`, `scope_end`,
  `signature`. An agent should not have to parse prose to find them.
* `fixes` are edits that make this diagnostic go away without changing behavior that
  already compiled. `ingot fix` applies the first fix of each diagnostic, re-checks and
  repeats until nothing changes.
* Lines and columns are 1-based, columns count bytes, spans are half-open. Each position
  also carries its byte offset in the file.
* The compiler reports every independent error in a file in one run. It does not report
  errors that are only consequences of an earlier one.

## 31. Cost reports and guaranteed check removal

`ingot cost FILE [--fn NAME] --json` reports what a function costs beyond the arithmetic
that is visible in its source. One object per item:

| `kind` | Meaning |
|---|---|
| `bounds_check`, `overflow_check`, `div_check`, `shift_check` | a check that survived, with the expression's span. A division has one check with two outcomes (T04, T05) and one `div_check`. The range checks of `@gather` and `@scatter` and the coverage check of `dispatch` are `bounds_check`s that are never removed |
| `scatter_check` | the check that the indices of a `@scatter` are distinct (T15): cheap with AVX-512CD, a compare network on NEON |
| `alloc` | a call whose error type is `std.mem.AllocErr` or `std.mem.AllocFail[T]`: the ordinary and the `_or_return` form of section 35's convention. `allocator` is the static type of the callee's one generic argument bound by `Allocator`, absent if there is none or more than one |
| `indirect_call` | a call whose callee is a `fn` value, a `&dyn` method or an `extern fn` pointer |
| `copy` | a copy or move of a value larger than 64 bytes, arguments and results included. A size that depends on a type parameter is not reported in the generic body |
| `not_inlined` | a call to a small function that was not inlined, and why |
| `loop` | one entry per loop: vectorized or not, the vector width, the reason if not |
| `task_spawn` | a `spawn` |
| `lock` | a call of a function that pipeline.md section 8 lists as a lock (`Mutex.lock`, `RwLock.read`, `RwLock.write` and their `try_` forms) |
| `park_point` | a call of a function that may park: `rt`'s park entry and every function that calls one, with indirect calls assumed to. It is the one item that is not local to a function: it needs the call graph of phase 11a |
| `frame` | stack frame size |

`@narrow`, `@assert` and `@panic` are not cost items: they are written in the source.

The check, `alloc`, `indirect_call`, `copy` and concurrency rows are the same for both
build pipelines, because they are properties of the checked semantics. `not_inlined`,
`loop` and `frame` describe the optimizing pipeline only. The removals below are
**required**: a conforming compiler performs them in every build, so an agent can rely
on them when writing a hot loop.

1. `for x in xs` and `for i, x in xs` over an array or slice: no bounds check on the
   element.
2. `a[i]` inside `for i in lo..hi` where `lo` is a non-negative constant and `hi` is
   `a.len` or a constant not larger than the array length, and `a` is not reassigned in
   the loop.
3. `a[i + k]`, `s[i..i + k]`, `@load(V, s, i)` and `@store(s, i, v)` when the access is
   dominated by a condition or `@assert` of the form `i + c <= s.len` (or `<`) with
   constant `c` large enough, `i` is known non-negative, and neither `i` nor `s` changes
   in between. A constant index counts as `i = 0`, so `@assert(s.len >= 2)` removes the
   checks on `s[0]` and `s[1]`.
4. No overflow check on a `for` loop counter, on `i += c` in a loop guarded by
   `i + c <= n`, or on an operation whose operands were widened with `as` so that the
   result always fits.
5. No overflow check where both operands are constants (it is evaluated at compile
   time and an overflow is a compile error).
6. A second identical check on the same values, dominated by the first with no
   assignment to them in between.
7. No division check on `a / c` or `a % c` where the divisor `c` is a constant other
   than 0 and, for a signed type, other than -1: neither T04 nor T05 can happen.

The compiler may remove more. `ingot cost` is the source of truth for what is left, and
the way to remove a remaining check is to restructure the code or add an `@assert` that
hoists it, not a build flag.

`ingot audit --json` lists every `unsafe` block with its reason string, every `extern`
declaration, every `@forget`, every `@debug`, every `unsafe impl`, and every `static`
with interior mutability.

`ingot layout TYPE --json` prints size, alignment, field offsets and padding.

`ingot asm FILE --fn NAME` prints the generated assembly for one function.

## 32. Tests

* `test "name" { ... }` blocks live next to the code. `ingot test` compiles and runs
  them and prints one JSON line per test: name, `ok` or the failure with its span.
* A test body may use `try`. The test fails on a trap or if an error reaches the end.
* `@test_env()` is available only inside `test` blocks (E1706). It gives a
  `&testing.TestEnv`: `env`, a `&Env` whose file system is a fresh temporary directory
  and whose network is loopback only; `clock`, the `&ManualClock` that `env.clock`
  reads, which only advances when the test calls `clock.step(d)`; and `dir`, the
  temporary directory's path. Stepping is test-only because a `ManualClock` can only
  come from `@test_env`. A later test-only need (an address, a fault to inject) is a
  field or a method of `TestEnv`, not a new builtin.
* `ingot test --seed N` runs the scheduler on one worker thread and takes every
  scheduling decision from the seed: which task runs at each spawn and park point, and
  each `select` choice. An interleaving that fails this way replays exactly. Without
  `--seed`, tests run on all workers, and a failure that depends on real parallel
  timing is not reproducible by seed.
* Tests in a dependency are not run.
* `bench "name" { ... }` blocks are measured by `ingot bench`, which reports time per
  iteration and, where the OS exposes them, cycles and instructions.

## 33. Packages and builds

```toml
# ingot.toml
name = "linecount"
ingot = "0.4"                    # the oldest compiler that builds it
targets = ["aarch64-macos-m4", "x86_64-linux-znver4", "x86_64-windows-znver4"]

[deps]
csv = { path = "../csv" }
zstd = { git = "https://example.com/zstd-ingot", rev = "4f1c...", hash = "b3:9f2c..." }

[override]
zlib = { git = "https://example.com/zlib-ingot", rev = "77ab...", hash = "b3:01de..." }

[runtime]
task_stack = "1MiB"

[link.aarch64-macos-m4]
frameworks = ["Metal"]

[link.x86_64-linux-znver4]
libs = ["vulkan"]

[link.x86_64-windows-znver4]
libs = ["vulkan-1"]
```

* A package is a directory tree of `.ingot` files with one `ingot.toml`. The entry point
  of an executable is `pub fn main` in `main.ingot`. `ingot check` and `ingot test` do
  not need one.
* A dependency is source. There is no registry, no version solving and no version
  number the compiler reads (a `version` key may exist for people). See section 33.2.
* A build is deterministic: the same sources, compiler and target give the same bytes.
* `--target NAME` selects one of the listed targets and defaults to the host. `ingot
  check --all-targets` type-checks every listed target, which needs nothing from the
  other systems. Building for another target is possible from any host once that
  target's system libraries are available to the linker.
* There is one language and two pipelines. `ingot build` optimizes. `ingot build --fast`
  compiles quickly with little optimization, for the edit-and-test loop. Both have
  identical semantics, traps included. There is no build mode that removes checks.
* Commands: `build`, `run`, `check`, `fix`, `test`, `bench`, `fmt`, `explain`, `api`,
  `cost`, `audit`, `layout`, `asm`, `targets`, `deps` (section 33.2), `query` (definition, references, type,
  live borrows, symbols), `rename` and `serve` (one long-running process that answers
  the same commands). Every command that reports something accepts `--json`, and
  [impl/tools.md](impl/tools.md) defines what each prints.

`ingot api [PATH] --json` prints the public surface of a module, a package or `std`:
each item's signature exactly as written (including `from`, `into`, `! E`, bounds and
modifiers), its doc comment, and for types their properties (`Copy`, linear, view,
`Send`, `Sync`), size and alignment. This is how an agent learns a library without
reading its implementation.

### 33.1 Program entry

```ingot
import std.Env;

pub fn main(env: &Env) -> void ! AppErr { ... }
```

`main` takes `&Env` and returns `void` or `u8` (the exit status), optionally fallible.
If an error is returned, the runtime prints one line to standard error and exits with
status 1. The line is `error: ` followed by the error value in the structural form of
its type (section 35): `error: ParseErr.bad_digit(120)`.
`Env` holds everything the process was given:

| Field | Type | Gives |
|---|---|---|
| `env.args` | `[][]u8` | command-line arguments |
| `env.vars` | `&Vars` | environment variables |
| `env.heap` | `Heap` | the general allocator |
| `env.vm` | `Vm` | address-space reservation, for arenas |
| `env.fs` | `&Fs` | the file system |
| `env.net` | `&Net` | sockets |
| `env.clock` | `&Clock` | wall and monotonic time, timers |
| `env.rand` | `&Rand` | OS entropy |
| `env.stdin`, `env.stdout`, `env.stderr` | `&Reader`, `&Writer` | standard streams |
| `env.gpu` | `?gpu.Device` | the GPU, if there is one |

Authority starts only here. The capability types in this table cannot be constructed
outside `std`
(section 4.2), so safe code performs an effect only with a value that traces back to
`main`'s `Env`. Code that was given a capability can park it in a `static` behind a
lock for other code to pick up, and `ingot audit` lists every such `static` so that
this is visible. `@debug("fmt", args...)` is the single built-in exception: it writes
to standard error from anywhere, and `ingot audit` lists it too.


### 33.2 Dependencies and workspaces

* **A git dependency** is pinned in the manifest that names it: `rev` says where to fetch
  and `hash` is what is checked (E1703), so a mirror or a moved tag cannot change a
  build. The hash is BLAKE3 over the files a build can read: `ingot.toml`, every
  `.ingot` file, and every file the manifest names (`asm` sources, a profile). For each,
  in byte order of the relative path with `/` separators: the path's length, the path,
  the content's length, the content. Nothing else in the directory counts.
* **A path dependency** is not pinned: it is your own source, in the same repository or
  workspace. A path dependency of a git dependency must stay inside that dependency's
  tree, so that its hash covers it.
* **There is no lock file.** A dependency's own manifest is inside its hash, so the root
  manifest determines the whole graph.
* **Updating** is a command that edits the manifest: `ingot deps update NAME [--rev R]`
  fetches, computes and writes `rev` and `hash`. `ingot deps` prints the graph and
  `ingot deps fetch` fills the cache. `build`, `check` and `test` never use the network.
  E1703 has no fix edit: accepting changed content is a deliberate command.
* **One package per name.** A package is identified by its `name`, and a build holds one
  package per name. If the graph reaches two different contents under one name, that is
  E1708, naming both chains; the root's `[override]` picks the one used everywhere. The
  key in `[deps]` must equal the package's `name`, and renaming is at the import only
  (`import zstd.frame as zf;`).
* **No dev-dependencies.** An import used only by a `test` block is still an import of
  the module. A dependency that only tests use costs a fetch and nothing in the
  executable, since unreached code is not compiled.
* **Workspaces.** `[workspace] members = ["tools/*"]` in a root manifest gives one build
  directory and one `[override]` table to all members, and `check`, `test` and `fmt`
  at the root run over every member. A root may be a package too.
* **`[link]` is the union** over every package in the build, without duplicates, in
  package order; the build summary lists the result. **`[runtime]` is the root's**: in a
  dependency it has no effect. `[link.<target>] asm`, a list of assembly sources, is
  allowed in `std`'s manifest only (E1701 elsewhere); `ingot audit` lists the files.
* **Targets.** Building for a target needs every package in the graph to list it (E1709
  at the `[deps]` entry otherwise). `--all-targets` uses the root's list.
* **`std` is tied to the compiler.** It ships with it, is not in `[deps]`, cannot be
  overridden, and is not hashed per package. `ingot = "0.4"` states the oldest compiler
  a package works with (E1710 when the compiler is older).
* **Output.** `ingot build` produces an executable. A C-callable library is deferred
  (rationale section 12). Every Ingot symbol is local to the executable; only the
  `export fn`s and the entry are global. Two `export fn`s with one name in a build are
  E1711; a package should prefix its exported names, and `std` uses `ingot_rt_`.
* **Reproducible output.** No timestamps, a build identifier computed from content (ELF
  build-id, Mach-O UUID), no absolute paths (inputs are passed by relative path), and a
  fixed order of inputs.

## 34. Formatting

`ingot fmt` produces the one canonical layout: four-space indentation, one statement
per line, a fixed line-breaking rule for long calls and declarations, imports sorted.
It never changes a token other than whitespace and trailing commas. `ingot fmt --check`
is how a repository enforces it.

## 35. Standard library outline

Only the shape is specified here. Each module gets its own document as it is designed.

| Module | Contents |
|---|---|
| `std.mem` | `Allocator`, `Heap`, `Vm`, `Arena`, `FixedBuf`, `Box`, `Arc`, `Pool`/`Handle`, `copy`, `fill`, `eq` |
| `std.list`, `std.map`, `std.set`, `std.str`, `std.ring`, `std.heap` | `List[T, A]`, `Map[K, V, A]` (open addressing, SIMD group probing), `Set`, `Str[A]`, fixed ring buffer, binary heap |
| `std.text` | UTF-8 validation and decoding, number parsing and printing, splitting, searching (SIMD) |
| `std.fmt` | `@format(writer, "x={} y={:.3}", x, y)`: the format string is checked at compile time against the argument types. Any value prints with `{}`: a type that does not implement `Format` gets the structural form (below) |
| `std.io`, `std.fs`, `std.net` | `Reader`, `Writer`, buffered wrappers, files, directories, memory mapping, TCP and UDP. Blocking calls park the task |
| `std.time` | `Clock`, `Instant`, `Duration`, `Timestamp` |
| `std.sync` | `Cell`, `Atomic`, `LoadOrder`, `StoreOrder`, `AtomicOrder`, `Mutex`, `RwLock`, `Once`, `Chan`, `WaitGroup`. Contents are `Owned` (section 13.6) |
| `std.task`, `std.thread` | `Scope`, `Task`, `Wait`, `run`, `set_qos`, `yield`, `canceled`, `first_of`; `thread.spawn(s, f)` for a dedicated OS thread in a scope |
| `std.testing` | `TestEnv`, `ManualClock`: what `@test_env()` gives a `test` block (section 32) |
| `std.par` | `Split`, `chunks`, `zip`, `sum`, `map_reduce`, `try_each`, `sort`, `Arenas` |
| `std.simd` | native-width aliases (`simd.F32`, `simd.U8`, ...), chunked iteration helpers |
| `std.exec`, `std.gpu` | executors, `Device`, `Queue`, `Buffer`, `Grid` |
| `std.linalg` | dense vectors and matrices, `matmul` (SME on the M4, AVX-512 on the 7800X3D) |
| `std.math`, `std.hash`, `std.rand`, `std.sort` | correctly rounded elementary functions with the same results on every target; hashing; PRNGs; sorting |
| `std.sys` | the `unsafe` OS layer everything above is built on, one implementation per operating system (`std.sys.macos`, `std.sys.linux`, `std.sys.windows`) behind a common internal interface. Its items are `pub(pkg)`, so programs cannot import it and `ingot api std` does not print it |
| `arch.aarch64`, `arch.x86_64` | per-instruction intrinsics |

**Format strings** (`@format`, `@debug`). The format string is a string literal. A
placeholder is `{}` or `{:spec}`, and `{{` and `}}` are literal braces. `spec` is
`[<|>][0][width][.precision][x|X|b|o]`. Arguments are used in order and their count
must match the placeholders. A `[]u8` prints as text, and with `x` as hex bytes. A float
prints as the shortest text that reads back to the same value, unless a precision is
given. What each part of `spec` allows:

| Part | Allowed on |
|---|---|
| `<`, `>` and `width` | every type |
| `0` (pad with zeros) | integers and floats |
| `.precision` | floats |
| `x`, `X`, `b`, `o` | integers; `x` and `X` also on `[]u8` and `[]mut u8` |

A malformed placeholder, a spec the argument's type does not support, or a count
mismatch is E0422. The type of `@format` itself (which writers it takes and the error it
returns) is a known gap, to be settled with `std.io`.

**The structural form** is what `{}` prints for a type with no `Format` implementation.
It is not an `impl` and not an item: the compiler generates it per printed instance
(phase 11), `ingot api` does not show it, and a type opts out by implementing `Format`.
It writes the value as the language writes it: `ParseErr.bad_digit(120)`,
`Point.{ x: 1, y: 2 }`, `(1, 2)`, `[1, 2]`, `.none`. Outside the type's module it
prints only the fields visible there, and `..` for the rest, so `@debug` cannot read
private state.

Library conventions that every module follows:

* A function that allocates takes an allocator or belongs to a container that has one,
  and returns `! AllocErr` or an error enum with a `from AllocErr` variant.
* A function that takes a value and may fail to store it requires the value to be
  `Discard`, and drops it on failure. Its second form, named `_or_return`
  (`push_or_return`), takes any `T` and gives the value back in its error,
  `! AllocFail[T]`.
* A function that performs I/O takes the capability it needs as a parameter.
* A linear type's consuming function is named `deinit` when it cannot fail, and has a
  specific name (`close`, `commit`, `join`, `unlock`) when it has meaning beyond
  release.
* A function that returns a borrow into a container is named for it (`items`, `get`,
  `get_mut`), and a function that copies out says so (`to_owned`, `clone_into`).
* Nothing in `std` panics on bad input. A trap in `std` is a bug in the caller's use of
  indices or in `std`.
* The public API is the same on all three targets. Paths, arguments and environment
  values are `[]u8`. On Windows `std` converts to and from UTF-16 at the system boundary
  using WTF-8, so every Windows file name survives a round trip. `std.fs` accepts `/` as
  a separator everywhere.
* `std` exposes no concept that only one system has (`fork`, signals, drive letters as a
  type). Where behavior cannot be made the same, the difference is documented on the
  function and not hidden.
