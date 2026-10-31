# Lesson 14 — Capstone projects

**Goal:** combine everything — primitives, vectors, funcs, params, generate loops, instances, and
the standard library — into complete designs, verified under both modes with property checks.

By now you can read `std/std.sta` fluently; treat it as a reference and a challenge to match or
extend. For each project: design it, write a thorough testbench, and verify with
`--mode both --check-invariance --check-causality`.

## Project A — N-input sorting network

Build a parameterized sorting network `SortN #(param N)` from `Sort2` compare-exchange instances
(an odd-even or bitonic network). Start by matching the library `SortNet4`, then generalize with a
`for`-generate so the structure scales with `N`. Verify the outputs are in nondecreasing arrival
order for many input vectors, including ties and `inf`s.

Skills: instances, generate loops, params, vectors, hierarchy.

## Project B — Winner-take-all with tie policy

Implement WTA over `N` inputs in two variants:
1. **strict** — an output fires only if its input is the *unique* earliest (ties produce no
   winner), using `x[i] < min(of all others)`.
2. **first-wins** — on a shared earliest tick, the lowest-indexed input wins, using `le`/`<`.

Compare your results to `Wta3Strict` / `Wta3First`. Verify exactly one output fires in the
first-wins case whenever any input fires.

Skills: inhibition intuition, funcs, generate loops, careful testbenching.

## Project C — Coincidence pipeline

Build a detector that fires at the later arrival of two inputs *only if* they arrive within `K`
ticks, parameterized by `K`, using `le` and delays (`max(le(a, b+K), le(b, a+K))`). Then compose
several detectors to flag coincidences across a bank of inputs.

Skills: delays, derived ops, params, composition.

## Project D — Your own standard-library block

Design something not in `std.sta`: e.g. a "median of 3" (the middle arrival), a "k-th earliest"
selector, or a debounce-like block using inhibition and delay. Write it as both a `func` (where it
has one output) and/or a `module`, document its if/else meaning, and prove it with an exhaustive
small-range testbench.

Skills: everything; plus specifying correctness precisely.

## Finishing move

For any capstone, emit the DOT netlist (Lesson 13) and render it. Being able to *see* the spike
dataflow you designed — and recognize the primitives you learned in Lesson 02 at the leaves — is a
good sign you have internalized the language.