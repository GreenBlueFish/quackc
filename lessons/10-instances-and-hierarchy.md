# Lesson 10 — Instances and hierarchy

**Goal:** compose modules out of smaller modules with `inst`, and understand the feed-forward
(no-recursion) rule.

## Concept: instantiation

```
inst u = SubModule (.in1(x), .in2(y), .out1(z));
```

`inst` creates one instance of `SubModule`, wiring your signals to its ports **by name**. Each
`.port(wire)` connection binds one of the submodule's ports to a wire or port in the current
module. Outputs of the instance drive your wires; your wires feed its inputs.

## Concept: passing params to an instance

If the submodule has params, set them in the instantiation:

```
inst t = MinTree #(.N(8)) (.x(data), .y(result));
```

This is how you use a parameterized module at a non-default size (recall Lesson 08: testbenches
only bind defaults, but `inst` can choose any value).

## Concept: feed-forward only

The instance graph must be a DAG — no module may instantiate itself directly or in a cycle, and
the dataflow among instances cannot form a combinational loop. The compiler rejects cycles with a
clear error naming the path. This keeps every design a pure, settle-once combinational network.

## Files to write

**`hierarchy.sta`** — a `Sort2 (input time a, b; output time lo, hi)` compare-exchange module
(`lo = min`, `hi = max`), and a top module `Sort3` that sorts three inputs into nondecreasing
arrival order by instantiating `Sort2` three times (compare (0,1), then (0,2), then (1,2)). Wire
the intermediate results with internal wires. Add a testbench on `Sort3`.

## Run it

```
quackc test hierarchy.sta --mode both
quackc build hierarchy.sta --top Sort3 --out out
```

## What you should see

`Sort3` outputs the three inputs in earliest-to-latest order. Test with distinct values, a tie, and
an `inf`.

## Common mistakes

- Leaving an instance port unconnected, or connecting a name that is not a port -> error.
- Accidentally driving a wire both from an instance output and a separate assignment -> multiple
  drivers.
- Creating a cycle between instances -> combinational-cycle error (read the path it prints).

## Exercises

1. Extend to `Sort4` using the optimal 5-comparator network. Compare your wiring to the library
   `SortNet4` in Lesson 11.
2. Build a 2-stage pipeline-looking structure (still combinational) where one block's outputs feed
   the next, and confirm there is no cycle.