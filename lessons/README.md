# Learning Quack — a lesson plan

A guided path through the Quack space-time (s-t) algebra language, from first principles to
full structural designs. Each lesson is its own document in this folder. The lessons are **plans**:
each one tells you what to understand, what `.sta` file(s) to write yourself, and how to run them.
You write the `.sta` files as you go — that is the point.

## How to use this plan

1. Read the lesson document.
2. Create the `.sta` file(s) it describes in a scratch folder of your own.
3. Build or test with `quackc` (see each lesson's "Run it" box).
4. Check your result against the "What you should see" notes, then try the exercises.

Every design auto-imports the standard library, so `max`, the relational operators, and the
structural blocks are always available. If `quackc` is not on your PATH yet, see the project
README section "Installing `quackc` on your PATH".

## The arc

The curriculum moves in four stages: **primitives -> structure -> abstraction -> systems.**
Each lesson builds on the ones before it.

| # | Lesson | You will learn |
|---|--------|----------------|
| 00 | [Setup and your first design](00-setup-and-first-design.md) | Install check, the `.sta` file shape, build + test a one-line module |
| 01 | [Spike times and `inf`](01-spike-times-and-inf.md) | What a value *is* in this language; `inf`; why there are no ordinary constants |
| 02 | [The three primitives](02-primitives-min-inc-lt.md) | `min` (first-arrival), `+ n` / `inc` (delay), `<` (inhibit) |
| 03 | [Modules, ports, and wires](03-modules-ports-wires.md) | Module shape, `input`/`output time`, internal `wire`, the single-driver rule |
| 04 | [Testbenches](04-testbenches.md) | `test` / `wave` / `expect`, default-`inf` inputs, reading PASS/FAIL |
| 05 | [Derived operators](05-derived-operators.md) | `max`, `<` `>` `<=` `>=` `==` `!=` `^`, their if/else meanings, non-associativity |
| 06 | [Vectors](06-vectors.md) | `time[N]` ports and wires, element indexing, vector stimulus literals |
| 07 | [Functions (`func`)](07-functions.md) | Pure compile-time macros, inlining, building your own derived ops |
| 08 | [Parameters](08-parameters.md) | `#(param N = ..)`, parameterized widths, instantiating at a chosen size |
| 09 | [Generate loops and conditionals](09-generate-for-if.md) | `for i in lo..hi`, `if`, `assert`, building structure programmatically |
| 10 | [Instances and hierarchy](10-instances-and-hierarchy.md) | `inst`, named port connections, feed-forward composition, no recursion |
| 11 | [Imports and the standard library](11-imports-and-stdlib.md) | `import`, the auto-imported `std.sta`, reusing library blocks |
| 11b | [Multi-file programs and `include`](11b-multi-file-and-includes.md) | `include`, relative paths, build vs test scoping, `--std-tests` |
| 12 | [Simulation modes and property checks](12-simulation-and-properties.md) | functional vs event mode, `--mode both`, invariance and causality |
| 13 | [Inspecting the netlist](13-netlist-and-codegen.md) | `emit-netlist` json/dot, `build` to C++, `--no-opt`, what the compiler produced |
| 14 | [Capstone projects](14-capstone-projects.md) | Larger designs that combine everything: sorters, WTA, coincidence pipelines |

## Conventions used in the lessons

- **"Run it"** boxes give the exact `quackc` command(s) for the lesson's files.
- **"What you should see"** describes the expected output so you can self-check.
- **"Common mistakes"** calls out the errors the compiler will give you and what they mean.
- **"Exercises"** are open-ended; most have no single right answer.

## Worked solutions

A complete, compiler-verified `.sta` file accompanies most lessons in the [`sta/`](sta/) folder
(e.g. `sta/02-primitives.sta`). Every solution file passes `quackc test ... --mode both`. Try the
lesson yourself first, then compare. Note that running any lesson file also runs the standard
library''s own testbenches (they are auto-imported), so you will see extra PASS lines for
`GtExhaustive`, `Sort4FlatTb`, and friends — those are the library, not your design.

A note on philosophy: Quack is not a numeric language. Everything is about *when* a spike arrives,
not how big it is. If you keep asking "which spike arrives first, and when?", the operators will
feel natural. Lessons 01 and 02 are where that intuition is built — do not rush them.