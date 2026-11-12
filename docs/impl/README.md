# Implementing Ingot

These documents are for whoever writes an Ingot compiler, runtime or tool. They are
written so that an agent can follow them without having been part of the design.

## Which document says what

| Document | Role | Wins a conflict against |
|---|---|---|
| [The specification](../README.md) (`docs/01` to `05`, `grammar.md`) | Defines the language: what programs mean and which are rejected | everything below |
| [glossary.md](glossary.md) | One definition per term, with the section that owns it | nothing: it only points |
| [diagnostics.md](diagnostics.md) | Every error code and trap: the exact condition, the primary span, related spans, the fix | the specification's wording of *when* an error is reported, never *whether* |
| [pipeline.md](pipeline.md) | Compiler phases, what each may assume, required behavior that is not about language meaning (ordering, determinism, recovery, lowering, code generation) | nothing in the specification |
| [checker.md](checker.md) | The ownership checker as algorithms over a control-flow graph | nothing: it must accept and reject exactly what sections 10 to 13 and 18 to 20 say |
| [../../conformance/](../../conformance/README.md) | Programs with an expected outcome | nothing: a test that disagrees with the specification is a bug in the test |

**The specification is the only source of language rules.** These documents do not
restate rules. They add what an implementer needs and the specification leaves open:
codes, spans, order, algorithms, representation.

## Rules for implementers

1. **When the specification is silent or contradicts itself, stop and fix the
   specification.** Do not pick a behavior in code. Add the rule to the specification,
   add a conformance test, then implement. Two compilers that each guessed will not
   agree.
2. **A diagnostic code is never reused and never changes meaning.** A new condition gets
   a new code in [diagnostics.md](diagnostics.md) before it is emitted.
3. **Every code has at least one conformance test that expects it,** and every fix has a
   `.fixed.ingot` test. `conformance/README.md` lists the codes that still lack one.
4. **Nothing here is proven.** [checker.md](checker.md) states the invariants the checker
   must keep. A program that the checker accepts and that reads freed memory or races is
   a soundness bug in the specification or the checker, and is the most important kind
   of bug to report.
5. **The conformance suite was written by hand, before any compiler existed.** Expect
   mistakes in it. When a test looks wrong, check it against the specification and fix
   whichever is wrong.

## Suggested reading order

1. The specification's [README](../README.md), then sections 1 to 9.
2. [glossary.md](glossary.md).
3. [pipeline.md](pipeline.md), to see where each rule is enforced.
4. Sections 10 to 16 of the specification together with [checker.md](checker.md).
5. [diagnostics.md](diagnostics.md) alongside the phase being implemented.
6. Sections 17 to 35 as the corresponding parts are reached.

## Status of this set

| Part | State |
|---|---|
| Glossary | complete for sections 1 to 35 |
| Diagnostic catalog | covers every "error" and "trap" the specification names |
| Pipeline | front end, ownership checker and lowering are specified. Runtime and GPU back ends have notes only |
| Checker algorithms | moves, linear values, loans, origins, scopes. Kernel checking is a list of rules, not an algorithm |
| Conformance tests | a first set per area. Coverage is listed in `conformance/README.md` |
