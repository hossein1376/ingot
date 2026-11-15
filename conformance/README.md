# Conformance tests

Small programs, each with an expected outcome. A compiler conforms if every test here
behaves as its header says. The suite is the shared reference for anyone implementing
Ingot: two implementations that pass it agree on everything it covers.

**These tests were written by hand before any compiler existed.** None has been run.
When a test disagrees with the specification, the specification is right and the test
is fixed.

## Layout

A test is a single `name.ingot` file, or a directory with a `main.ingot` in it (for
tests that need more than one module). A single file is compiled as the root module of a
package named `t`. A directory is compiled as a package named after the directory, unless
it has an `ingot.toml`, which then names the package and is itself under test (it is the
only way to test E17xx). `main.ingot` is the package's root module (spec section 2), so
a test that imports its own modules writes `import dirname.lib;`. Only the header of
`main.ingot` is read; the other files of a directory test have none.

These are conventions of the test harness. A command-line `ingot check DIR` needs a real
`ingot.toml` (E1701 otherwise), and a file called `x.fixed.ingot` is an ordinary module.
In a directory test, `main.fixed.ingot` is the fix result of `main.ingot`, and is not
loaded as a module. The standard library (`std/`) is available to every test.

## Header

Each test starts with directive lines:

| Directive | Meaning |
|---|---|
| `//! expect: accept` | `ingot check` reports nothing |
| `//! expect: reject` | `ingot check` reports exactly the diagnostics marked in the file |
| `//! expect: run` | the program builds, runs, and exits with the status given by `//! exit:` (default 0) |
| `//! expect: trap` | the program builds, runs, prints one trap line and exits with status 101 |
| `//! expect: cost` | `ingot check` reports nothing, and `ingot cost` reports exactly the marked check items |
| `//! section: N, M` | the specification sections the test exercises |
| `//! target: NAME` | run only for that target. Absent: all targets |

## Markers

A marker is a trailing comment `//~ CODE` on a source line.

A marker in an `ingot.toml` is written `#~ CODE`, since `#` starts a TOML comment.

* In a `reject` test, `//~ E1101` means: a diagnostic with code `E1101` whose primary
  span starts on this line. The set of (code, line) pairs reported must equal the set
  marked. An unmarked diagnostic fails the test, and so does a missing one.
* In a `trap` test, `//~ T03` means: the trap line names this file, this line and code
  `T03`.
* In a `cost` test, `//~ bounds_check` means: `ingot cost` reports an item of that kind
  whose span starts on this line. Only the four check kinds (`bounds_check`,
  `overflow_check`, `div_check`, `shift_check`) are compared, and the set must be equal.

Columns and message text are not compared. Related spans are not compared yet.

## Fix tests

`name.fixed.ingot` next to `name.ingot` is the expected result of `ingot fix` on the
original. The two are compared after removing directive lines, marker comments and
trailing white space from both. The fixed file is also a test of its own and expects
`accept`.

## Rules for writing tests

* One behavior per test. A `reject` test should mark one code, on as few lines as the
  rule allows.
* A test must not trip any other rule. In particular: no unused local bindings, no
  `var` that is never mutated, no unused imports, no shadowing, and every non-`void`
  call result used.
* Prefer no imports. `run` and `trap` tests need `std.Env` for `main`, and concurrency
  tests need what they test. Avoid everything else in `std`, whose API is not settled.
* A `run` test reports failure through `@assert`, not through output.
* A `trap` test must reach its trap through a function parameter, so that the operation
  is not a constant expression (which would be error E0806 at compile time).
* Every code in [the catalog](../docs/impl/diagnostics.md) should have a test, and
  every fix a `.fixed.ingot`.

## What is covered

500 test files: 130 `accept` (43 of them `.fixed.ingot` tests; 44 fix results in
all), 347 `reject`, 13 `run`, 6 `trap`, 4 `cost`.

| Group | Codes with a test | Codes without one |
|---|---|---|
| E01xx | 2 of 11 | E0101, E0102, E0103, E0106, E0107, E0108, E0109, E0110, E0111 |
| E02xx | 7 of 13 | E0202, E0203, E0208, E0211, E0212, E0213 |
| E03xx | 16 of 17 | E0303 |
| E04xx | 22 of 22 | |
| E05xx | 23 of 23 | |
| E06xx | 13 of 13 | |
| E07xx | 10 of 10 | |
| E08xx | 12 of 13 | E0809 |
| E10xx | 6 of 6 | |
| E11xx | 5 of 5 | |
| E12xx | 6 of 7 | E1207 |
| E13xx | 5 of 5 | |
| E14xx | 9 of 10 | E1406 |
| E15xx | 12 of 14 | E1514, E1518 |
| E16xx | 5 of 6 | E1605 |
| E17xx | 5 of 7 | E1703, E1704 |
| traps | 6 of 17 | T02, T05, T08, T09, T10, T11, T12, T13, T15, T16, T17 |

Retired codes are not counted: the catalog marks E0512 and E0513 as retired, so they
appear in no row and in no total below.

164 of 199 catalog codes have a test.

E1701 for a *missing* `ingot.toml` has no conformance test, because the harness supplies
a manifest by convention; `crates/driver/tests/cli.rs` covers it. The other cases of
E1701, and E1702, E1705, have tests in `packages/`.
