# Lesson 09 — Generate loops and conditionals

**Goal:** build structure programmatically with compile-time `for` and `if`, and guard assumptions
with `assert`.

## Concept: `for` generate

```
for i in lo..hi { <statements> }
```

This is **unrolled at compile time**, not a runtime loop. The bounds are compile-time integers
(often params), and `i` is a compile-time value usable as an index or in arithmetic. A classic use
is folding an operator across a vector:

```
wire time acc[N];
acc[0] = x[0];
for i in 1..N { acc[i] = min(acc[i-1], x[i]); }
y = acc[N-1];
```

That builds a min-tree over `N` elements with one line of structure.

## Concept: `if` generate and `assert`

`if (<compile-time cond>) { ... } else { ... }` selects structure at compile time — useful for
special-casing a width. `assert <compile-time expr>;` fails compilation if the condition is false,
which is how you document and enforce a param's valid range.

Important: compile-time expressions use only `+ - * / %` — there is **no** `>=`, `==`, or `<`. An
`assert` is simply a compile-time integer, and it is **true when nonzero, false when zero**. So you
express a bound arithmetically:

- "N must be nonzero" -> `assert N;`
- "N must be at least 2" -> `assert N - 1;` (which is 0, i.e. false, exactly when N == 1)

Likewise an `if` condition is a nonzero/zero integer, e.g. `if (N) { ... }`.

## Files to write

**`mintree.sta`** — rewrite Lesson 08's fixed `Min4` as a parameterized fold using a
`for`-generate as shown above. Add `assert N - 1;` (true for N >= 2). Keep the default at 4 so the
testbench (which binds defaults) exercises a 4-wide fold. Compare it to your hand-written `Min4`.

Name your modules `MinFold` / `MaxFold`, **not** `MinTree` / `MaxTree`: the standard library already
declares `MinTree` and `MaxTree`, and redeclaring a library name is a duplicate-declaration error
(a useful thing to discover — see Lesson 11).

## Run it

```
quackc test mintree.sta --mode both
```

## What you should see

Identical results to the hand-unrolled `Min4` from Lesson 08 — because the loop *is* the
unrolling. That equivalence is the whole point of generate loops.

## Common mistakes

- Treating `for` as runtime iteration — it is compile-time structure generation.
- An index that goes out of range for some param value -> compile error; this is where `assert`
  earns its keep.
- Driving `acc[0]` inside the loop as well as before it -> multiple drivers.

## Exercises

1. Add `if (N == 1) { y = x[0]; } else { <the fold> }` so the module also handles the degenerate
   width, and relax the `assert` accordingly (remember: nonzero = true).
2. Build a "delay line" with a loop: `s[0] = a; for i in 1..N { s[i] = s[i-1] + 1; }` and expose
   `s[N-1]`. What single expression is that equivalent to?