# Lesson 07 — Functions (`func`)

**Goal:** factor repeated expressions into reusable compile-time macros, and build your own derived
operators the same way the standard library does.

## Concept: a `func` is a pure macro

```
func name(time a, time b) -> time { return <expression>; }
```

A `func` takes `time` parameters, returns a single `time`, and is **inlined** everywhere it is
called during compilation. It has no ports and no state — it is pure substitution. This is exactly
how `le`, `ge`, `max`, etc. are defined in `std.sta`.

## Concept: funcs vs modules

- A `func` returns one value and is inlined into an expression: `y = myop(a, b);`.
- A `module` has named ports (possibly many outputs) and is instantiated with `inst` (Lesson 10).

Reach for a `func` when you want a named expression; a `module` when you want a reusable block with
structure or multiple outputs.

## Files to write

**`myops.sta`** — define at least two of your own funcs, for example:
1. `func earlier2(time a, time b) -> time { return min(a, b); }` (a thin alias, to get the shape).
2. A func that implements "a if a <= b else inf" using only `<` (hint: `a < (b < a)`), i.e.
   your own `le`. Then compare it against the library `le` in a later lesson.

Add a module that uses your funcs and a testbench that checks them against hand-computed values.

## Run it

```
quackc test myops.sta --mode both
```

## What you should see

Your hand-built `le` should match the if/else meaning: pass `a` when `a <= b`, else `inf`.

## Common mistakes

- Trying to give a `func` more than one output -> a func returns exactly one `time`.
- Expecting a `func` to hold state or ports -> it is a pure expression macro.
- Recursion in a func (direct or indirect) -> rejected; expansion must terminate.

## Exercises

1. Build `neq` as a func and test it against every ordered pair over a small range plus `inf`.
2. Write a func `within1(time a, time b)` that fires at the later of `a`, `b` when they are within
   one tick of each other, else `inf`. (You are previewing the coincidence detector from the
   stdlib.)