# Lesson 02 — The three primitives: `min`, delay, inhibit

**Goal:** learn the three operations everything else is built from. If you understand these, you
understand the algebra; every other operator is a macro over them.

## Primitive 1: `min(...)` — first arrival

`min(a, b)` is the **earlier** of two spikes. `min` is variadic: `min(a, b, c)` is the earliest
of three. Because `inf` is the largest time, `min` ignores inputs that never fire unless they all
do (then the result is `inf`). Think of `min` as "whichever spike shows up first wins".

## Primitive 2: `a + n` (delay) / `inc`

`a + n` shifts a spike **later** by a compile-time constant `n` ticks. If `a` arrives at tick 3,
`a + 2` arrives at tick 5. `inf + n` is still `inf`. `n` must be a constant known at compile time
(a literal, a param, or a compile-time expression) — never another wire. The one-tick form `inc(a)`
is `a + 1`.

This is the only place a number appears in wire logic, and it is a *relative* shift, which is why
it is allowed (see Lesson 01 on invariance).

## Primitive 3: `a < b` (`lt`, inhibit)

`a < b` passes `a` **only if `a` arrives strictly before `b`**; otherwise the result is `inf`.
Read `<` as "a, unless b blocks it". If `b` arrives at the same tick as `a` or earlier, `a` is
inhibited and nothing comes out. This is the only primitive that can *suppress* a spike, and it is
what makes winner-take-all and all the relational operators possible.

## Files to write

**`primitives.sta`** — one small module per primitive so you can test each in isolation:
1. `FirstArrival` — `output = min(a, b)`.
2. `DelayTwo` — `output = a + 2`.
3. `Inhibit` — `output = a < b`.

Add a `test` block for each, with waves that cover: both finite, a tie, one `inf`, both `inf`.

## Run it

```
quackc test primitives.sta --mode both
```

(`--mode both` runs two independent simulators and cross-checks them — more in Lesson 12. Using it
now gives you extra confidence your mental model is right.)

## What you should see

Work out each expected value by hand first, then confirm:
- `min(3, 5) = 3`, `min(5, 5) = 5`, `min(4, inf) = 4`, `min(inf, inf) = inf`.
- `3 + 2 = 5`, `inf + 2 = inf`.
- `3 < 5 = 3` (3 is earlier, passes), `5 < 3 = inf` (blocked), `4 < 4 = inf` (tie blocks).

## Common mistakes

- Writing `a + b` where `b` is a wire -> error: the delay amount must be compile-time.
- Expecting a tie to pass through `<` -> it does not; `<` is *strictly* earlier.

## Exercises

1. Build `a < (a + 1)`. What does it always equal, and why? (This is a useful identity.)
2. Predict `min(a, a + 3)` for any finite `a`. Confirm it.
3. Combine two primitives: make an output that is `a` delayed by 1, but only if `a` beats `b`.