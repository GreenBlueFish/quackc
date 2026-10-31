# Lesson 05 — Derived operators

**Goal:** use the operators built on top of the three primitives, understand their exact meanings,
and learn the non-associativity rule.

## Concept: derived operators are macros

`max`, and the relational operators, are not new primitives — they expand into `min` / delay /
`<` during compilation. They live in the standard library (`std.sta`), which is auto-imported.

## The operators and their if/else meanings

Each relational passes its left operand when the relation holds, otherwise `inf`:

| write      | name | meaning |
|------------|------|---------|
| `a < b`    | lt   | `a` if `a < b` else `inf` (this is the primitive) |
| `a > b`    | gt   | `a` if `a > b` else `inf` |
| `a <= b`   | le   | `a` if `a <= b` else `inf` |
| `a >= b`   | ge   | `a` if `a >= b` else `inf` |
| `a == b`   | eq   | `a` if `a == b` else `inf` |
| `a != b`   | neq  | `a` if `a != b` else `inf` |
| `a ^ b`    | min  | the **earlier** arrival (an infix alias for `min(a, b)`) |
| `max(a, b)`|      | the **later** arrival |
| `emin(a, b)`|     | earlier arrival, but `inf` if the two tie (exclusive min) |
| `emax(a, b)`|     | later arrival, but `inf` if the two tie (exclusive max) |

The if/else meaning is authoritative. Note these are *timing* comparisons: `a == b` fires (at
`a`) only when the two spikes arrive on the same tick.

## Concept: relational operators do not chain

`a < b < c` is a **parse error**. The relationals are non-associative: you must parenthesize to
say what you mean. This prevents ambiguous timing expressions.

## Files to write

**`relations.sta`** — a module `Relations` with `input time a, b` and one output per operator
(`lt`, `gt`, `eq`, `neq`, `mx`), each driven by the corresponding operator. Add a testbench that
sweeps a handful of ordered pairs including equal values and `inf`.

## Run it

```
quackc test relations.sta --mode both
```

## What you should see

- `a == b` fires only on exact ties; otherwise `inf`.
- `a > b` passes `a` only when `a` is strictly later; `max` always returns the later of two
  finite spikes and `inf` only if either is `inf` for `max`... predict these before checking.

## Common mistakes

- Chaining relationals without parentheses -> parse error. Write `(a < b) < c` if you truly mean
  it (though think about whether that is meaningful).
- Confusing `max` (later arrival) with "the bigger number" — it is about time ordering, which here
  happen to coincide, but keep the timing framing.

## Exercises

1. Express `max(a, b)` using only `>` and `min`. Compare with the library definition later
   (Lesson 11) to see if you matched it.
2. Build an output that fires iff `a` and `b` arrive on the same tick, carrying that tick.