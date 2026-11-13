# Ingot standard library

`std/` is the standard library package. It is ordinary Ingot source, checked
with the program that imports it. The compiler attaches it in
`loader::attach_std`:

* `INGOT_STD` names a directory when set and it contains `ingot.toml`.
* Otherwise the compiler looks for `std/` beside the executable, one or two
  directories above it, then at the cargo manifest directory used to build it.

`std.ingot` is the package root module. Its public items are imported as
`std.Item` (`import std.Env`). Other files are modules under the `std` prefix:
`mem.ingot` is `std.mem`, and `sys/macos.ingot` is `std.sys.macos`.

## Modules

| Module | State |
|---|---|
| `std` | `Env`, `Vars`, `Rand`; the process capabilities |
| `std.mem` | `AllocErr`, `Allocator`, `Heap`, `Vm`, `Arena`, `FixedBuf`, `copy`, `fill`, `eq` |
| `std.io` | `IoErr`, `Reader`, `Writer` interfaces |
| `std.fs` | `Fs`, `File`, `IoErr` re-export |
| `std.net` | `Net`, `Tcp`, `Udp` |
| `std.time` | `Clock`, `Duration`, `Instant` |
| `std.text` | UTF-8 validation and counting, byte search, prefix/suffix tests, trimming, `i64` parsing |
| `std.fmt` | `u64` and `i64` decimal encoders for `@format` and user code |
| `std.sys` | platform-neutral `Errno` |
| `std.sys.macos` | raw libc declarations for the first target |

Modules that are not here yet keep the old opaque behavior: `std.sync`,
`std.task`, `std.simd`, `std.exec` and `arch.*` resolve as external names, so
existing conformance programs continue to check and run. As each module lands,
the opaque fallback stops applying to it.

## Rules

* Every public item is `pub`; implementation details stay private.
* A capability type has a private field, so safe code cannot construct it.
* An allocating function takes an allocator. Allocation failure is an
  `AllocErr`, never a trap.
* No function in `std` panics on bad input. A trap in `std` is a bug.
* Text is UTF-8 bytes. Validation and decoding are explicit.
* `unsafe("reason")` states the invariant at every raw-pointer boundary.

## Next

1. `std.list` and `std.str` need the allocator implementations and raw-slice
   constructors from the code generator; their APIs should wait for those, not
   fake an owning container over a borrow.
2. `std.sys.macos` needs the typed wrappers over `read`, `write`, `mmap`,
   `munmap`, `open`, `close` and `exit`, then `std.fs` and `std.io` built on
   them.
3. `std.fmt` grows the checked `@format` lowering and `Format` interface once
   `Writer` dispatch through `&dyn` is available.
