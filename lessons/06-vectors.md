# Lesson 06 — Vectors

**Goal:** work with bundles of wires using `time[N]` ports and wires, element indexing, and vector
stimulus in testbenches.

## Concept: vector types

A port or wire can be a vector of spike times: `input time x[4]` is four wires. Declare internal
vector wires with `wire time v[N];`. Indexing is 0-based: `x[0]` is the first element.

## Concept: indices are compile-time

An index like `x[i]` must resolve to a compile-time integer — a literal, a param, or a
`for`-generate loop variable (Lesson 09). You cannot index with a spike-time wire.

## Concept: vector stimulus in waves

In a testbench, drive a vector port with a bracket literal: `x = [3, inf, 1, 5];`. Expected vector
outputs use the same form: `expect y = [1, 3, 5, inf];`.

## Files to write

**`vectors.sta`** — a module `Pick` with `input time x[3]` and `output time first` where `first`
is the earliest of the three elements, driven by `min(x[0], x[1], x[2])`. Add a testbench with
vector stimulus covering a tie and an `inf` element.

## Run it

```
quackc test vectors.sta --mode both
```

## What you should see

`first` is the earliest finite element, or `inf` if all three never fire. Confirm with a wave like
`x = [5, 2, 9] -> first = 2` and `x = [inf, inf, inf] -> first = inf`.

## Common mistakes

- Off-by-one indexing: `x[3]` on a `time[3]` port is out of range -> compile error.
- Writing a scalar where a vector stimulus is expected (or vice versa) -> a type/shape error.

## Exercises

1. Add an output `last` that is the latest arrival (`max` over the elements). Build it with nested
   `max` calls for now; Lesson 09 will show how to fold over the vector with a loop.
2. Make a `time[2]` output that is the pair (earliest, latest).