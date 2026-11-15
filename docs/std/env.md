# `std.Env` and the process capabilities

`Env` is the root capability bundle. Authority starts only here (section 33.1): every
effect in safe code needs a value that traces back to `main`'s `Env`, except
`@debug`. The capability types have no public field, so safe code outside `std` cannot
make one (section 4.2).

```ingot
import std.fs.Fs;
import std.gpu;
import std.io.Reader;
import std.io.Writer;
import std.mem.Heap;
import std.mem.Vm;
import std.net.Net;
import std.rand.Rand;
import std.time.Clock;
```

## 1. `Env`

```ingot
/// Everything the process was given (section 33.1). The compiler knows this type by
/// path: it is the parameter of `main` and the result of `@test_env()`.
pub struct Env {
    pub args: [][]u8,
    pub vars: &Vars,
    pub heap: Heap,
    pub vm: Vm,
    pub fs: &Fs,
    pub net: &Net,
    pub clock: &Clock,
    pub rand: &Rand,
    pub stdin: &Reader,
    pub stdout: &Writer,
    pub stderr: &Writer,
    pub gpu: ?gpu.Device,
}
```

Properties of `Env`:

| Property | Value | Reason |
|---|---|---|
| `Copy` | no | it is a plain struct, and `?gpu.Device` is not `Copy` |
| `Discard` | yes | it owns no resource that needs consuming |
| linear | no | the runtime owns the process and the capabilities |
| `Owned` | no | it holds borrows of runtime-owned values |
| view | yes | the `&` fields |
| `Send` | no | the stream capabilities are not shared between tasks |
| `Sync` | no | `Reader`/`Writer` may buffer or hold a non-reentrant handle |

`main` takes `&Env`; the compiler does not require `Env` to be `Send`. A program that
wants to give one capability to a task passes the field it needs (`&Fs`, `&Clock`,
`env.heap`) and the task's argument check applies to that field.

`args` is the process argument vector including `argv[0]` (D5). Each element and the
outer slice are `[]u8`/`[][]u8`; the runtime owns the memory and keeps it alive until
`main` returns. `vars` is the same for the environment block.

`gpu` is `.none` on a machine with no usable device. `env.heap` and `env.vm` are `Copy`
capabilities; copying one does not give new authority, it is the same allocator or
reservation authority.

## 2. `Vars`

`Vars` has no module of its own in section 35; it is declared next to `Env`.

```ingot
pub struct Vars { ... }

impl Vars {
    /// The value of `name`, or `.none`. The result borrows the process environment.
    pub fn get(self: &Vars, name: []u8) -> ?[]u8 from self

    pub fn contains(self: &Vars, name: []u8) -> bool
}
```

`Vars` is `Send`, `Sync`, `Owned` and `Discard`; it is not `Copy` and not linear. The
environment block is immutable after process start, so a shared borrow may be used from
several tasks. `get` returns a borrowed `[]u8`, not a copy; a caller that needs the
value after the process environment changes (it never does) must copy it. Name matching
is byte-exact on macOS and Linux; on Windows it follows the platform's
case-insensitive environment rules.

## 3. `@test_env`

```ingot
/// Builtin, available only inside a `test` block. One environment per test block;
/// repeated calls in the same block return the same `&Env`.
@test_env() -> &Env
```

The test environment is built by `std.sys.rt.test_env` and is not constructible by
safe code. It has:

- a file system rooted in a fresh temporary directory that is removed after the test;
- a clock whose wall and monotonic time move only when the test asks (see D4);
- a network capability whose only addresses are loopback;
- the same `Heap`, `Vm`, `Rand`, standard streams and `gpu` shape as a normal `Env`.

`@test_env` is a builtin because the compiler must reject it outside a `test` block
and because the test harness, not the program, owns the temporary directory. Its result
borrows the test harness's storage for the duration of the test block.

## Decisions for the human

- **D1. `Vars` surface.** **Decided: accepted.** `get` and `contains` only.
- **D2. `Env` sharing.** **Decided: accepted.** `Env` is neither `Send` nor `Sync`;
  tasks receive the individual capability they need.
- **D3. `@test_env` identity.** **Decided: accepted.** One environment per test block,
  reused by repeated calls.
- **D4. Stepping the test clock.** **Decided: accepted provisionally** through
  `time.md` D5. The builtin `@test_clock_step(clock, d)` is the mechanism; the
  test-only API goes to design review.
- **D5. `args` includes `argv[0]`.** **Decided: accepted.** `Env.args` is the process
  argument vector.
- **D6. `Vars` returns a borrow.** **Decided: accepted.** `?[]u8 from self`.

## Language questions

- **L1. Test-only API.** Ingot has no conditional compilation for tests and no
  "available only in a test" method. `@test_env` is a builtin exception, but stepping
  the clock, observing the temporary directory path and asserting the network is
  loopback all need test-only members. The spec says these behaviors exist but not how
  a test reaches them (section 32). A builtin is the cleanest place.
- **L2. Capability visibility.** `Env`'s fields are `pub`, while the capability types
  have private fields. That stops construction of a capability, but not a struct
  literal of `Env` if a program somehow has every field value. The spec's privacy rule
  (section 4.2) covers this, but `@test_env` needs a compiler-known way to build `Env`
  from inside `std`; no path for that is specified.
- **L3. Test environment cleanup.** The temporary directory is removed after the test;
  a test that hands a path to a foreign thread or a spawned task may still use it. The
  test harness waits for tasks in the test's scopes, but a `std.thread` group or a
  foreign callback is not covered. The spec does not say how cleanup is ordered.
