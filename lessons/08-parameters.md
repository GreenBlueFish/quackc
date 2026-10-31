# Lesson 08 — Parameters

**Goal:** make modules reusable across sizes with compile-time parameters.

## Concept: parameterized modules

```
module Name #(param N = 4) ( input time x[N], output time y ) { ... }
```

`param N` is a compile-time integer with a default. It can set vector widths, loop bounds, delay
amounts, and anything else that must be known at compile time. A module with all-defaulted params
can be used directly; otherwise you pick values when you instantiate it (Lesson 10).

## Concept: params flow into compile-time expressions

A param can appear anywhere a compile-time integer is allowed: `x[N]`, `a + N`, `for i in 0..N`,
`x[N-1]`, and in `assert`s. You can do arithmetic on them: `N * 2`, `N - 1`, etc.

## Files to write

**`delayn.sta`** — a module `DelayN #(param N = 3) (input time a, output time y)` where
`y = a + N`. Add a testbench. Because a testbench binds a module at its **default** params, test
the default here; you will exercise non-default values via `inst` in Lesson 10.

Also write **`mintree_fixed.sta`** — a module `Min4 (input time x[4], output time y)` that folds
`min` over the four elements by hand (no loop yet). Keep it; Lesson 09 rewrites it with a loop and
a param so you can compare.

## Run it

```
quackc test delayn.sta
quackc test mintree_fixed.sta --mode both
```

## What you should see

`DelayN` at the default `N=3` shifts a finite input by 3 and leaves `inf` as `inf`.

## Common mistakes

- Expecting a testbench to override a param — it does not; it binds the default. Use `inst` for
  other sizes.
- Using a param where a *runtime* value is needed, or a wire where a compile-time value is needed.

## Exercises

1. Add a param `K` to `Min4` that delays the result by `K`, defaulting to 0, and confirm the
   default behaves like the un-delayed version.
2. Predict what `assert N;` would do if you set the default to 0. (An `assert` is true when
   nonzero, so `assert N;` fails at N == 0. You will add asserts in Lesson 09.)