# Lesson 12 — Simulation modes and property checks

**Goal:** understand the two simulators, use `--mode both` to cross-check them, and verify the two
defining properties of the algebra with the built-in checkers.

## Concept: two simulators

- **functional** (Mode A): straight-line evaluation of the settled combinational result. Fast; the
  default.
- **event** (Mode B): a discrete event-driven engine that advances tick by tick, modeling spikes
  propagating through delays and inhibition over time.

They should always agree on the final output timing. `--mode both` runs both and fails if they
disagree on any wire — an excellent way to catch a mistake in either your design or your mental
model.

```
quackc test design.sta --mode functional   # default
quackc test design.sta --mode event
quackc test design.sta --mode both
```

## Concept: invariance checking

`--check-invariance` re-runs each wave with every finite input shifted later by a random constant
`c` (leaving `inf` alone) and verifies every finite output shifts by exactly `c` and no `inf`
output becomes finite. This is the time-shift invariance from Lesson 01, checked automatically. A
design that hard-codes a tick will fail here.

## Concept: causality checking

`--check-causality` confirms (in event mode) that no output fires earlier than the earliest input
that can influence it — the system never "sees the future".

```
quackc test design.sta --mode both --check-invariance --check-causality
```

## Files to write

No new module required — reuse `mintree.sta` or `hierarchy.sta`. The work here is running them
under each mode and both property checks, and interpreting the output.

## What you should see

All green under `--mode both` with both checks. If you still have a hand-built operator from an
earlier lesson, run it under invariance — a correct one passes.

## Common mistakes

- Assuming a functional PASS means the design is fully correct — run `--mode both` and the property
  checks for real confidence.

## Exercises

1. Intentionally break invariance by... you cannot easily, since bare constants are illegal — which
   is the point. Instead, reason about why the language makes invariance hard to violate.
2. Run a WTA design under `--check-causality` and argue why no output could precede its inputs.