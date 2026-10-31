# Lesson 03 — Modules, ports, and wires

**Goal:** write well-formed modules with internal structure, and learn the single-driver rule that
governs every signal.

## Concept: the module shape

```
module Name ( <ports> ) {
    <statements>
}
```

Ports are declared `input time x` or `output time y`. A module is a pure combinational function:
given input spike times, it produces output spike times, with no memory between evaluations.

## Concept: grouping ports of the same direction

You can declare several ports of the same direction and type in one clause by separating the names
with commas; a bare name reuses the current direction and type until the next `input`/`output`:

```
module M (input time a, b, output time y, z) { ... }
```

is exactly the same as the longhand:

```
module M (input time a, input time b, output time y, output time z) { ... }
```

A per-name vector suffix applies only to that name: in `input time x[4], y`, `x` is `time[4]` and
`y` is a scalar `time`. The first entry in the port list must start with `input` or `output`.

## Concept: internal wires

Use `wire time w;` to declare an internal signal, then drive it in a later statement, or declare
and drive it in one line: `wire time w = min(a, b);`. Wires let you name intermediate results and
share them across several outputs.

Like ports, several wires of the same type can be declared together by comma-separating the names:

```
wire time a, b, c;        // same as: wire time a; wire time b; wire time c;
wire time v[4], s;        // v is time[4]; s is a scalar (the [N] suffix is per-name)
```

Only a single wire may carry an inline `= driver`; a comma-grouped declaration must be undriven
(drive each one on its own line). So `wire time w = min(a, b);` is fine, but
`wire time a = x, b;` is a parse error.

## Concept: the single-driver rule

Every output port and every wire must be driven **exactly once**. Not zero times (undriven), not
twice (multiple drivers). This is the rule you will trip over most early on, and it is a feature:
it makes a design a clean dataflow graph.

## Files to write

**`compare.sta`** — a module `MinMax` with `input time a, b` and `output time lo, hi` where
`lo` is the earlier arrival and `hi` the later. Use an internal wire for at least one shared
subexpression even if you do not strictly need it, to practice wire declarations. Add a testbench.

## Run it

```
quackc test compare.sta
```

## What you should see

`lo` is `min(a, b)`; `hi` is the later one (you will meet `max` formally in Lesson 05 — for now
you may use the library `max`, which is auto-imported). Ties produce `lo == hi == a`.

## Common mistakes

- An output you forgot to drive -> "undriven" error naming the port.
- Driving the same wire in two statements -> "multiple drivers" error.
- Using a wire before declaring it -> an unknown-name error.

## Exercises

1. Add a third output `both = min(a, b)` reusing the same wire that drives `lo`. Does the
   single-driver rule stop you from reusing a wire's *value*? (It does not — reuse is fine; only
   *driving* twice is the error.)
2. Split `MinMax` so `lo` and `hi` are each driven directly from `a` and `b` with no shared wire,
   then with a shared wire. Compare readability.