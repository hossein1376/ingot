# The scheduler: how it is built

Status: **plan**, from the language designer's second review (B5). 03-concurrency §23
stays the norm: this document says how the runtime meets it and in which order it is
built. `rt.md` is the boundary the compiler calls. The deque, wait-queue and timer
algorithms are not chosen yet; they are written here when step R3 or R4 starts, and
this document is accepted, like every std design, only when its declarations pass
`ingot check` as stubs.

## Three stages

Each stage runs real programs, so each is a step that ends with tests passing (COMPILER.md
steps R2, R3 and R4).

1. **Inline** (R2). One thread and no stack switch. `spawn` pushes a task record; `join`
   and the scope wait run pending records on the current stack; `par for` runs its
   chunks in order. An operation that would have to park runs other pending records
   instead, and traps T13 when there are none. This is enough for `scope`, `spawn`,
   `join`, `par for`, uncontended locks and the three concurrency `run` tests
   (`concurrency/spawn_join`, `par_for_disjoint`, `par_for_atomic`). A task record and
   its argument and result record come from a region the runtime reserves from the
   operating system (`Vm`), never from the general heap, and go back to a free list at
   `join` or, for a statement-form spawn, when the task finishes; a scope's pending
   records are a list in its `ScopeState`. The current task is found through the OS
   thread-local slot from the start, so stages 2 and 3 change no caller.
2. **Parking** (R3). Stacks with guard pages, taken from a cache; the stack switch (one
   assembly file per target, `rt.md` §2); wait queues with tickets and `select`
   (03-concurrency §21, `rt.md` §3); channels, timers, the test clock; T11 (stack
   exhaustion) and T13 (deadlock); still one worker. **`--seed N`** is this stage with
   every choice taken from a documented generator: which record runs at a spawn and at
   a park, and the order `select` polls its arms. New tests: a channel between two
   tasks, a `select` with a timer on the test clock, a deadlock trap, a
   stack-exhaustion trap.
3. **Workers** (R4). A deque per worker with stealing; an unstolen spawn is run by its
   parent at `join` (§23); the two groups on macOS (performance and efficiency cores,
   by quality-of-service class); per-worker caches for task records, stacks and the
   heap; `exec.par`. The same tests on all workers, and a `bench` that shows a `par
   for` scaling.

## Rules that hold in every stage

* A spawn that its parent runs inline shares the parent's stack. If it parks, the whole
  stack parks, which is correct because the parent is waiting for it.
* A task record is recycled at `join`, or when a statement-form task finishes, never
  held until the scope's end (03-concurrency §23).
* `spawn` and parking never call the general heap; running out of record or stack
  memory traps T12.
* The current worker and task are found through one OS thread-local slot, read by a
  call, so nothing cached before a park is reused after the task resumes elsewhere.
* The body of an `export fn` runs outside any task (T17 for anything that needs one);
  `task.run(f)` enters the runtime from there.

## Not normative

The generator behind `--seed`, the size of the caches, the default `par for` grain
beyond "at least eight chunks per worker", and the stealing policy.
