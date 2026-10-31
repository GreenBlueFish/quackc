# Lesson 13 — Inspecting the netlist and generated code

**Goal:** see what the compiler actually produces — the netlist graph and the emitted C++ — and
understand what optimization does.

## Concept: the netlist

Internally the compiler elaborates your design (expanding funcs, loops, params, and instances)
into a flat **netlist**: a graph of primitive nodes (min, inc/delay, lt) and edges. `emit-netlist`
exports it:

```
quackc emit-netlist design.sta --top Top --format json
quackc emit-netlist design.sta --top Top --format dot
```

- **json** is a versioned document listing every node with its kind, source location, original
  name, and hierarchy path — good for tooling.
- **dot** is a Graphviz graph: one node per IR node, one edge per edge, with `Inc` amounts and `Lt`
  pass/inhibit roles labeled. Render it with Graphviz (`dot -Tpng`) to *see* your design.

## Concept: `--no-opt`

By default the compiler optimizes the netlist. `--no-opt` preserves the as-written structure, which
is what you usually want when visualizing — the DOT then matches your source one-to-one.

```
quackc emit-netlist design.sta --top Top --format dot --no-opt
```

## Concept: generated C++

`build` emits a C++17 **header** (`<Top>.hpp`) implementing your top module's evaluation. It is not
a runnable program on its own: it defines `<Top>_in` / `<Top>_out` structs and a `<Top>_eval(...)`
function, but no `main()`.

```
quackc build design.sta --top Top --out out
```

To actually run it, you write a tiny driver with a `main()` that calls `<Top>_eval`, then compile.
The generated header does `#include "quack_rt.hpp"` with a bare filename (no path, so it stays
portable); you point the compiler at the repo's `runtime/` folder with `-I`:

```sh
g++ -std=c++17 -I out -I <repo>/runtime out/main.cpp -o out/app
```

`build` emits only `<Top>.hpp` — it does not copy `quack_rt.hpp` into the output folder, which is
why the `-I <repo>/runtime` flag is needed. See the project README section "Compiling and running
the generated C++" for a complete `main.cpp` example. (For just *observing* behavior, `quackc test`
and `emit-netlist` are quicker and need no compiler.)

## Files to write

No new modules — use `mintree.sta` and `hierarchy.sta`. Emit both JSON and DOT for each, with and
without `--no-opt`, and diff the DOT outputs to see what optimization changed.

## What you should see

The DOT graph should mirror your source structure under `--no-opt`. With optimization on, you may
see fewer nodes where the compiler simplified. The JSON should carry provenance (source location,
original names) for every node.

## Exercises

1. Render a `MinTree` of width 8 to a PNG and confirm it is a balanced-looking fold.
2. Compare the optimized vs `--no-opt` netlist of a design that has an obvious redundancy you
   introduced; identify what the optimizer removed.