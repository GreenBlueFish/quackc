# Lesson 11b — Multi-file programs and `include`

**Goal:** split a design across several `.sta` files and pull them together with `include`, and
understand how `build` and `test` treat a multi-file program.

## Concept: `include` vs `import`

Two mechanisms bring in other files, and they are not the same:

- **`import name.sta;`** resolves against the *library search path* (where `std.sta` lives). Use it
  for shared libraries. The standard library is auto-imported for you.
- **`include path/to/file.sta;`** resolves *relative to the file that writes it*. Use it to compose
  your own project's files. This is the one you want for multi-file programs.

```
include helpers/math.sta;   // relative to THIS file's folder; subfolders allowed
```

An `include` path is a bare path (identifiers plus `.`, `/`, `-`); no quotes. Includes are
**transitive** — an included file may include others — and cycles are detected and reported.

## Concept: what each command does with includes

- **`quackc build top.sta --top T --out dir`** gathers `top.sta` and everything it includes
  (transitively), plus the auto-imported standard library, and flattens the module `T` into a
  single generated header `dir/T.hpp`. One build, one file, all pieces resolved.
- **`quackc test top.sta`** runs **only the tests defined in `top.sta` itself**. Tests inside
  included files (and inside the standard library) do **not** run. Included files contribute their
  modules and functions — not their testbenches. This keeps a test run focused on the file you
  named.

## Concept: the `--std-tests` flag

By default `quackc test foo.sta` does not run the standard library's own testbenches, so your
output shows only your results. If you want to also run the library's testbenches (for example to
confirm your toolchain is intact), add `--std-tests`.

## Files to write

Make a small project folder with a subfolder:

```
project/
  helpers/
    math.sta      // module Delay1 (input time a, output time y) { y = a + 1; }
  top.sta         // includes helpers/math.sta, instantiates Delay1, has its own test
```

1. In `helpers/math.sta`, define a reusable `Delay1` module (and, to prove the point, a `test`
   block for it).
2. In `top.sta`, write `include helpers/math.sta;`, a `Top` module that instantiates `Delay1`, and
   a `test` for `Top`.

## Run it

```
quackc test top.sta             # runs only top.sta's test; helper's test is ignored
quackc test top.sta --std-tests # also runs the standard library testbenches
quackc build top.sta --top Top --out out
```

## What you should see

- `test top.sta` reports only your `Top` test — not the helper's test, and no stdlib noise.
- `build` emits `out/Top.hpp` containing the flattened design, with `Delay1` pulled in from the
  included file.

Reminder from Lesson 10: the test runner treats a sub-module `inst` as an opaque pass-through, so
if you want to *simulate* the composed behavior in a testbench, test a primitive-only (flat) module.
Includes still compose and build correctly regardless.

## Common mistakes

- Using `import` for your own sibling file — it will not be found; use `include`.
- Expecting an included file's tests to run — they do not; only the named file's tests run.
- An include path that does not exist relative to the including file -> a clear "cannot resolve
  include" error naming the directory it searched.
- An include cycle (a includes b includes a) -> an "include cycle detected" error.

## Exercises

1. Split a two-stage pipeline so each stage is its own file, included by a top file. Build it and
   confirm one `.hpp` contains both stages.
2. Add a `test` block to an included file, run `quackc test top.sta`, and confirm it does not run.
   Then move that test into `top.sta` and watch it run.
3. Create a `helpers/` subfolder and include a file from it with `include helpers/thing.sta;`.