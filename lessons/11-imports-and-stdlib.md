# Lesson 11 — Imports and the standard library

**Goal:** reuse code across files with `import`, and get fluent with the auto-imported standard
library so you stop reinventing blocks.

## Concept: `import`

`import other.sta;` brings another file's modules and funcs into scope. Imports form a DAG (a file
cannot import itself in a cycle).

A practical note on the current CLI: import targets are resolved against the standard-library
search path (`QUACKC_STDLIB_PATH` plus the built-in `std/` directory), **not** the design file's
own folder. In practice that means `std.sta` resolves automatically, but importing one of your own
files only works if that file lives on the library search path. The exercises below therefore focus
on the auto-imported standard library, which is where imports matter most day to day.

## Concept: `std.sta` is auto-imported

Every design implicitly imports the standard library before your source is parsed, so these are
always available with no `import` line:

- derived operators: `le`, `ge`, `gt`, `eq`, `neq` (as funcs *and* as modules `Le`/`Ge`/...),
  plus `max` and `min`.
- structural blocks: `Sort2`, `MinTree`/`MaxTree`, sorting networks (`SortNet2/3/4`), winner-take-
  all (`Wta3Strict`, `Wta3First`), and a `Coincidence` detector.

Because the library is written in Quack itself, you can read `std/std.sta` to see exactly how each
block is built — it is some of the best example code available.

## Concept: a snapshot vs the source tree

If `quackc` runs outside its source tree, it finds `std.sta` via the `QUACKC_STDLIB_PATH`
environment variable (the project `install.ps1` sets this). Worth knowing if an import of `std`
ever fails to resolve.

## Files to write

**`lib_demo.sta`** — a design that `inst`antiates library blocks directly: e.g. a `SortNet4` and a
`Wta3Strict`, wired to your own top-level ports. Test the primitive-only block (`Wta3Strict`)
directly; for the `inst`-based sorting network, use `build` / `emit-netlist` and test a flat twin
(wrap the library `Sort4Flat`), exactly as in Lesson 10.

## Run it

```
quackc test lib_demo.sta --mode both
```

## What you should see

Your top module behaves exactly like the underlying library blocks — a sorting network emits sorted
arrival order; strict WTA passes only a unique earliest input and blocks on ties.

## Common mistakes

- Re-declaring a name the library already defines (e.g. your own `module Sort2`) *and* importing it
  -> duplicate declaration. Pick one.
- Importing a file that imports back into yours -> import cycle error.

## Exercises

1. Open `std/std.sta` and read the definition of `gt` and `neq`. Re-derive why `neq` uses `min`
   (first-arrival) rather than `max`.
2. Compare the library `SortNet4` wiring with the `Sort4` you built in Lesson 10's exercises.