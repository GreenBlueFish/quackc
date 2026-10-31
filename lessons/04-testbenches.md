# Lesson 04 — Testbenches: `test`, `wave`, `expect`

**Goal:** drive your modules with stimulus and check results automatically. Testbenches are how
you learn and verify everything from here on.

## Concept: a `test` binds to a module

```
test SomeName for ModuleName {
    wave { <inputs> ; <expects> }
    wave { ... }
}
```

Each `wave` is one stimulus vector: it sets input ports and lists the expected output values. The
runner applies each wave, evaluates the module, and compares.

## Concept: inputs default to `inf`

Any input you do not mention in a wave defaults to `inf` (never fires). This is why `inf` is a
first-class value — it is the natural "no stimulus" state. Set only the inputs you care about.

## Concept: exact comparison, `inf` included

A wave passes only if every expected output matches exactly, where `inf` equals only `inf`. A
failing wave reports its number and the wires that differed, and the `test` command exits nonzero.

## Concept: an output you do NOT `expect` is ignored

A wave only checks the outputs you write an `expect` for. Any output you leave out is **not
checked** — it does not matter what the module produces for it. This lets you focus a wave on one
output at a time.

If you want the opposite — "every output I did not mention should be its default" — add a single
line `expect default;` to the wave:

```
wave { a = 3; b = 5; expect lo = 3; expect default; }
```

Now `lo` is checked against 3, and every *other* output of the module is checked against its
default (which is `inf` unless you defined otherwise — see below). `expect default;` is good
practice when you want a wave to pin down the whole output vector, but it is optional.

## Concept: per-test defaults with a `default` block

By default an unmentioned-but-`expect default`ed output is checked against `inf`. If some output
should default to a different value, declare it once per test in a `default { .. }` block:

```
test T for M {
    default { ready = 0; }          // `ready`'s default is 0, not inf
    wave { a = 3; expect default; } // checks `ready` against 0, everything else against inf
}
```

An output not named in `default` still falls back to `inf`.

## Concept: table-form tests with `cases`

Writing many near-identical waves is tedious. A `cases` block is a `test` where each input and
each expected output is a **vector of the same length**; column `i` becomes wave `i`:

```
cases FirstArrivalCases for FirstArrival {
    a      = [1, 2, 4, inf];
    b      = [2, 2, 3, 2];
    expect y = [1, 2, 3, 2];
}
```

That is four waves in four lines. All vectors must be the same length. You can add `expect
default;` on its own line inside a `cases` block to apply it to every expanded wave. The plain
`test`/`wave` form still works — use whichever is clearer for a given set of cases.

A `cases` block may also carry one `default { .. }` block, applied to every expanded wave. This is
handy when an output is always the same across all cases — declare it once instead of repeating it
in every column. For example, if an output `done` is always `inf` for this set of inputs:

```
cases MyCases for M {
    default { done = inf; }   // `done` defaults to inf for every case
    a      = [1, 2, 4];
    expect y = [1, 2, 4];
    expect default;           // check every unmentioned output (here `done`) against its default
}
```

Now you only write the input vectors and the outputs that vary; `done` is pinned to `inf`
everywhere by the `default` block.

## Files to write

Reuse `compare.sta` from Lesson 03 (or `primitives.sta`). Expand its testbench to cover, for each
module: both inputs finite, a tie, one input omitted (so it defaults to `inf`), and a wave you
*expect to fail* — then fix the expectation and watch it pass.

## Run it

```
quackc test compare.sta
quackc test compare.sta --mode both
```

## What you should see

A PASS/FAIL line per test plus a passed/failed wave count. Make a wave fail on purpose and read the
message carefully: it names the wave index and the differing wire. That message is your main
debugging tool.

## Common mistakes

- Expecting a value for an input you forgot you left at `inf`.
- Writing `expect` values for a vector port as a scalar (vectors use `[...]` — see Lesson 06).
- Thinking an unmentioned output is checked — it is **not** (unless the wave has `expect default;`).
  If you want to assert that an output stays `inf`, either write `expect that_output = inf;` or add
  `expect default;`.

## Exercises

1. For `Inhibit` (`a < b`), write the full truth-ish table over a few finite ticks plus `inf`,
   as `expect`s, before running. How many did you get right?
2. Deliberately swap two expected values and confirm the failure message points you to the right
   wave and wire.