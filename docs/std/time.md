# `std.time`: clocks, instants and durations

`std.time` is the `Clock` capability, `Instant`, `Duration` and `Timestamp`. Wall time
is not the same thing as monotonic time: a wall clock may jump because of an NTP
adjustment or a manual step, while `Instant` may not go backwards. The two therefore
have different types.

## 1. `Duration`

```ingot
/// A signed span of time, stored as nanoseconds. The range is about ±292 years.
pub copy struct Duration {
    nanos: i64,
}

impl Duration {
    pub fn zero() -> Duration
    pub fn from_nanos(nanos: i64) -> Duration

    /// `.none` if the multiplication overflows `i64`.
    pub fn from_micros(micros: i64) -> ?Duration
    pub fn from_millis(millis: i64) -> ?Duration
    pub fn from_secs(secs: i64) -> ?Duration

    pub fn as_nanos(self: Duration) -> i64
    pub fn as_micros(self: Duration) -> i64
    pub fn as_millis(self: Duration) -> i64
    pub fn as_secs(self: Duration) -> i64
    pub fn is_negative(self: Duration) -> bool

    pub fn checked_add(self: Duration, other: Duration) -> ?Duration
    pub fn checked_sub(self: Duration, other: Duration) -> ?Duration
    pub fn checked_mul(self: Duration, n: i64) -> ?Duration
    pub fn checked_div(self: Duration, n: i64) -> ?Duration

    pub fn saturating_add(self: Duration, other: Duration) -> Duration
    pub fn saturating_sub(self: Duration, other: Duration) -> Duration
    pub fn saturating_mul(self: Duration, n: i64) -> Duration
    pub fn abs(self: Duration) -> Duration
}

impl Eq for Duration {
    fn eq(self: &Duration, other: &Duration) -> bool
}

impl Ord for Duration {
    fn cmp(self: &Duration, other: &Duration) -> Ordering
}
```

`as_micros`, `as_millis` and `as_secs` truncate toward zero. A negative `Duration` is a
value, not a bug; the consumer decides whether it is meaningful (a negative sleep span
completes at once, see `Clock`). `checked_div` gives `.none` for a zero divisor. `abs(MIN)`
would overflow; `abs` saturates at the largest positive value. `Duration` is `Copy`,
`Discard`, `Owned`, `Send`, `Sync`; it is not linear.

## 2. `Instant` and `Timestamp`

```ingot
/// A point on the monotonic clock. Opaque: the epoch is not specified.
pub copy struct Instant {
    ticks: i64,
}

impl Instant {
    /// Zero if `earlier` is later than `self`; monotonic time never goes backwards on
    /// the same clock.
    pub fn duration_since(self: Instant, earlier: Instant) -> Duration
    pub fn checked_duration_since(self: Instant, earlier: Instant) -> ?Duration
    pub fn checked_add(self: Instant, d: Duration) -> ?Instant
    pub fn checked_sub(self: Instant, d: Duration) -> ?Instant
}

impl Eq for Instant {
    fn eq(self: &Instant, other: &Instant) -> bool
}

impl Ord for Instant {
    fn cmp(self: &Instant, other: &Instant) -> Ordering
}

/// A point in wall time, as Unix seconds and a nanosecond part in `0..1_000_000_000`.
pub copy struct Timestamp {
    seconds: i64,
    nanos: u32,
}

impl Timestamp {
    /// `.bad_nanos` if `nanos >= 1_000_000_000`.
    pub fn from_unix(seconds: i64, nanos: u32) -> Timestamp ! TimestampErr

    pub fn unix_seconds(self: Timestamp) -> i64
    pub fn nanos(self: Timestamp) -> u32

    pub fn checked_add(self: Timestamp, d: Duration) -> ?Timestamp
    pub fn checked_sub(self: Timestamp, d: Duration) -> ?Timestamp

    /// `.none` if the difference does not fit in `Duration`.
    pub fn checked_duration_since(self: Timestamp, earlier: Timestamp) -> ?Duration
}

impl Eq for Timestamp {
    fn eq(self: &Timestamp, other: &Timestamp) -> bool
}

impl Ord for Timestamp {
    fn cmp(self: &Timestamp, other: &Timestamp) -> Ordering
}

pub copy enum TimestampErr: u8 { bad_nanos }
```

`Instant` and `Timestamp` are `Copy`, `Discard`, `Owned`, `Send`, `Sync`; they are not
linear and not views. Their fields are private so that a value can only come from a
`Clock` or from the checked constructors. `Timestamp` ignores leap seconds; it is the
system's Unix time, and the resolution is target-dependent. `Instant` is comparable
only with an instant from the same clock; the type cannot enforce that, so the contract
is on the caller. `Timestamp` may go backwards if the system clock is adjusted; that is
why it is not an `Instant`.

## 3. `Clock`

```ingot
/// The clock capability from `env.clock`. No public field; only `std` makes one.
pub struct Clock { ... }

impl Clock {
    /// The current wall time.
    pub fn wall(self: &Clock) -> Timestamp

    /// The current monotonic time. It never goes backwards on one clock.
    pub fn monotonic(self: &Clock) -> Instant

    /// Waits until `delay` has passed. A delay of zero or less returns at once; a
    /// canceled scope fails with `.canceled`.
    pub waitable fn after(self: &Clock, delay: Duration) -> void ! TimerErr

    /// The millisecond form of `after`. Zero or a negative `ms` returns at once.
    pub waitable fn after_ms(self: &Clock, ms: i64) -> void ! TimerErr

    /// Blocks the calling task for `delay`. A delay of zero or less returns at once;
    /// a canceled scope fails with `.canceled`.
    pub fn sleep(self: &Clock, delay: Duration) -> void ! TimerErr
}

pub copy enum TimerErr: u8 { canceled }
```

`Clock` is `Discard`, `Owned`, `Send` and `Sync`; it is not `Copy` and not linear. It
has no public field, so safe code outside `std` cannot construct one. `wall` and
`monotonic` are cheap reads; they do not block. `after`, `after_ms` and `sleep` use
`rt.park` with a monotonic deadline (`rt.md` D5). The timer implementation is in the
scheduler; `Clock` only names the deadline and the cancellation rule.

`after` and `after_ms` are the waitable operations of section 21. `sleep` is the
non-waitable form: it always blocks the current task until the delay passes or the
scope is canceled. Neither form observes a `select` that did not choose it; that is the
waitable lowering question, not a clock operation.

Properties of the time types:

| Type | `Copy` | `Discard` | linear | `Owned` | `Send` | `Sync` | Reason |
|---|---|---|---|---|---|---|---|
| `Clock` | no | yes | no | yes | yes | yes | capability with no public field; methods only read OS state and park |
| `Instant` | yes | yes | no | yes | yes | yes | opaque monotonic value |
| `Timestamp` | yes | yes | no | yes | yes | yes | Unix seconds and nanos |
| `Duration` | yes | yes | no | yes | yes | yes | signed nanoseconds |
| `TimerErr`, `TimestampErr` | yes | yes | no | yes | yes | yes | payload-free enums |

`Clock` is `Sync` because every method takes `&self` and the runtime owns the mutable
timer state; `wall` and `monotonic` are reads. If the implementation stores a raw OS
handle in `Clock`, its `Send`/`Sync` are `unsafe impl`s for that reason.

## 4. The test clock

`@test_env()` gives a `Clock` whose wall and monotonic values start at a fixed point
and advance only when the test says so:

```ingot
/// Builtin, available only inside a `test` block. Advances both the wall and monotonic
/// clock and makes every timer whose deadline is now past ready.
@test_clock_step(clock: &Clock, d: Duration) -> void
```

The recommended start is `Timestamp.from_unix(0, 0)` and monotonic zero; a test that
cares about a realistic date constructs its own `Timestamp` for comparisons. A
negative `d` is treated as zero (no step). Stepping is what wakes `after`/`after_ms`
tasks: a test must step the clock from a task that is not itself parked on that clock.
A test that calls `sleep` on a manual clock with no other task to step it is a deadlock
and reaches T13 through the scheduler's timer check; that is a named runtime failure,
not a `std.time` trap.

## 5. What it needs from `rt` and `std.sys`

- `std.sys` provides the OS wall clock and monotonic counter: `clock_gettime` and
  `mach_absolute_time` on the Unix targets, `QueryPerformanceCounter` and
  `GetSystemTimePreciseAsFileTime` on Windows. The target differences are the
  resolution and the epoch; the public API hides both.
- `rt.park` provides the monotonic deadline and the waiter. `std.time` owns the timer
  list and wakes tasks whose deadlines have passed; the scheduler owns the park/wake
  path.
- The test clock is a runtime object in `std.sys.rt.test_env`, not a second `Clock`
  type, because `Env.clock` has one static type.

`Clock` contains no `Cell`: its methods only read OS state and park. Its `after` and
`sleep` calls go through `rt.park`, which is `unsafe` at the boundary; the `unsafe`
block is inside `std.time` and states that the deadline and waiter stay valid until the
park returns.

## Decisions for the human

All seven are decided. D1–D7 were accepted as recommended; D5 is provisional. One
behavior change outside the numbering: a negative delay is zero, not a trap, for
`after`, `after_ms` and `sleep` (§35: a trap in `std` is index misuse or a bug in
`std`).

- **D1. Wall-time type.** **Decided: accepted.** Add `Timestamp`; `Instant` stays
  monotonic.
- **D2. Timer result.** **Decided: accepted.** `void ! TimerErr` for `after`,
  `after_ms` and `sleep`. A negative delay is a zero delay, not an error.
- **D3. `Duration` representation.** **Decided: accepted.** Signed `i64` nanoseconds.
- **D4. Duration overflow.** **Decided: accepted.** Checked methods returning
  `?Duration`, plus saturating forms; constructors return `?Duration` on overflow.
- **D5. Test-clock stepping.** **Decided: accepted provisionally.** Keep
  `@test_clock_step(clock, d)`; the test-only API goes to design review. This settles
  env.md D4.
- **D6. Test-clock epoch.** **Decided: accepted.** Unix epoch 0 and monotonic zero.
- **D7. `after` plus `after_ms`.** **Decided: accepted.** Keep both.

## Language questions

- **L1. Test-only clock control.** `@test_env` is the only test-only builtin the spec
  names. `@test_clock_step` is accepted provisionally and needs the same "test block
  only" rule; the spec has no general way to declare a test-only function. Section 32
  and env.md L1 are involved.
- **L2. A fallible waitable operation in `select`.** `after_ms` is listed in section 21
  as a waitable function, and section 22 says a canceled timer fails with `.canceled`.
  The example in section 21 writes `_ = clock.after_ms(250)`, which cannot consume a
  linear `void ! TimerErr`. Either a `select` arm may discard a fallible result, or a
  waitable timer needs a non-fallible result that carries the cancellation. This is the
  same lowering gap as `rt.md` L1.
- **L3. `Instant` from different clocks.** The type has no clock identity, so
  `duration_since` between an instant from the process clock and one from a test clock
  is accepted. The spec has no rule for this; a clock tag in `Instant` would cost an
  extra field and a compare.
- **L4. Arithmetic on wall time.** `Timestamp.checked_add(Duration)` ignores leap
  seconds and cannot express a calendar date. The spec names no calendar or time-zone
  type; `std.time` deliberately does not add one. If a later module needs dates, that
  is a new design, not a gap in this one.
