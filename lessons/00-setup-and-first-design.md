# Lesson 00 — Setup and your first design

**Goal:** confirm your toolchain works and understand the shape of a `.sta` source file by
building and testing a trivial module.

## Before you start

- `quackc` should be on your PATH. Open a fresh terminal and run:
  ```
  quackc --version
  ```
  You should see `quackc 0.1.0`. If the command is not found, see the project README section
  "Installing `quackc` on your PATH" (or run `install.ps1` from the project root).
- Make a scratch folder anywhere for your lesson files, e.g. `quack-lessons\`.

## Concept: what a `.sta` file is

A Quack source file (`.sta`) describes **combinational hardware over spike times**. The smallest
useful unit is a `module`: a named block with typed input and output ports and a body that drives
each output from the inputs. Nothing in a module stores state between evaluations — it is a pure
function from input spike times to output spike times.

## Files to write

**`hello.sta`** — a module that passes its input straight through, plus a tiny testbench.

Plan the file to contain:
1. A `module` named `Passthrough` with one `input time a` and one `output time y`.
2. A body that drives `y` from `a` directly.
3. A `test` block bound to `Passthrough` with two or three `wave`s, each setting `a` and
   expecting `y` to equal it.

## Run it

```
quackc test hello.sta
quackc build hello.sta --top Passthrough --out out
```

## What you should see

- `quackc test` prints a PASS line and exits 0.
- `quackc build` prints `emitted out\Passthrough.hpp` and creates that file. Open it and skim —
  you will recognize a generated C++ function later in Lesson 13.

## Common mistakes

- Forgetting the trailing `;` on a statement -> a parse error pointing at the next line.
- Driving `y` twice -> a "multiple drivers" error (every wire/output has exactly one driver).

## Exercises

1. Add a wave where `a = inf` and predict what `y` should be before you run it.
2. Rename the output port and watch which error the testbench gives when the names no longer match.