# quackc — a Quack s-t algebra compiler

[![CI](https://github.com/GreenBlueFish/quackc/actions/workflows/ci.yml/badge.svg)](https://github.com/GreenBlueFish/quackc/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

`quackc` compiles **Quack**, a small hardware-description language built on J. E. Smith's
space-time (s-t) algebra, into portable **C++17**. A Quack design is a feed-forward network of
combinational modules whose wires carry *spike times*; the compiler elaborates it to a flat
netlist, optionally optimizes it, and emits C++ that evaluates the network, along with a built-in
simulator and testbench runner.

The crate is a single cargo package with both a library target (`quackc`) and a binary target
(`quackc`).

---

## 1. The s-t algebra in brief

### Values: spike times

Every wire carries a single **spike time** — a value that says *when* a spike occurs, not how
large it is:

- a non-negative integer `t` in `{0, 1, 2, ...}` meaning "a spike occurred at tick `t`", or
- `inf` meaning "no spike ever occurs". In the simulator `inf` is the maximum value of the
  `SpikeTime` type.

A wire carries at most one spike per evaluation. `inf` is allowed as a literal in module bodies
because `inf + c = inf`; ordinary numeric constants are **not** allowed as wire values (see
invariance below).

### The three primitives

Everything in the algebra is built from three operators:

| primitive       | meaning                                                                          |
|-----------------|----------------------------------------------------------------------------------|
| `inc` (`a + n`) | **delay**: shift a spike later by a compile-time constant `n` (`inf + n = inf`). |
| `min(...)`      | **first-arrival**: the earliest of two or more spike times.                      |
| `lt` (`a < b`) | **inhibit**: pass `a` if `a < b`, otherwise `inf`. If `b` arrives at or before `a`, `a` is blocked. |

### Derived operators

Every other operator is a macro expanded into the three primitives during elaboration. Their
authoritative meanings are the if/else forms below (where a paper construction and the if/else
meaning disagree, the if/else meaning wins):

| operator            | infix     | meaning                                      |
|---------------------|-----------|----------------------------------------------|
| `lt(a, b)`          | `a < b`   | `a` if `a < b` else `inf` (primitive)        |
| `gt(a, b)`          | `a > b`   | `a` if `a > b` else `inf`                    |
| `le(a, b)`          | `a <= b`  | `a` if `a <= b` else `inf`                   |
| `ge(a, b)`          | `a >= b`  | `a` if `a >= b` else `inf`                   |
| `eq(a, b)`          | `a == b`  | `a` if `a == b` else `inf`                   |
| `neq(a, b)`         | `a != b`  | `a` if `a != b` else `inf`                   |
| `min(a, b)`         | `a ^ b`   | the **earlier** arrival (primitive)          |
| `max(a, b)`         |           | the **later** arrival                        |
| `emin(a, b)`        |           | earlier arrival, but `inf` if the two tie    |
| `emax(a, b)`        |           | later arrival, but `inf` if the two tie      |

These are defined in the standard library (`std/std.sta`) entirely in Quack itself, so adding an
operator needs no compiler change.

### The two defining properties

- **Causality** — an output at tick `t` depends only on inputs arriving at or before `t`. A
  network cannot see the future.
- **Invariance** (time-shift invariance) — shifting every input by a constant `c` shifts every
  output by exactly `c` (`f(a1 + c, ...) = f(a1, ...) + c`, with `inf + c = inf`). This is the
  reason numeric constants are forbidden as wire values: a literal tick would break invariance.

Both properties can be checked automatically on any testbench with `--check-causality` and
`--check-invariance` (see below).

---

## 2. The Quack language

A Quack source file uses the `.sta` extension. A design is one or more modules, optionally with
functions, imports, and testbenches.

### Modules, ports, wires

A **module** is a combinational s-t network with named input/output ports. It is a pure function
`time^N -> time^M` — nothing carries between evaluations.

```quack
module Sort2 (
    input  time a, input time b,
    output time lo, output time hi
) {
    lo = min(a, b);
    hi = max(a, b);   // library op, expands to the primitives
}
```

- **Ports** are typed `time` (a single spike time) or `time[N]` (a vector of `N` wires, 0-based
  indexing). `N` is a compile-time constant. Ports of the same direction can be grouped by
  comma-separating the names: `input time a, b, output time y, z` is shorthand for
  `input time a, input time b, output time y, output time z` (a per-name `[N]` suffix applies only
  to that name).
- **Wires** are internal signals declared with `wire time w;` or `wire time v[N];`. Several wires of
  the same type can be declared in one statement by comma-separating the names:
  `wire time a, b, c;` is shorthand for `wire time a; wire time b; wire time c;` (a per-name `[N]`
  suffix applies only to that name). An inline `= driver` is allowed only for a single wire
  (`wire time w = min(a, b);`); a comma-grouped declaration must be undriven.
- Every wire and output port must have **exactly one driver** (no multiple drivers, no undriven
  signals).

### Operators and expressions

Inside a body you combine signals with the primitives and derived operators:

```quack
y[0] = x[0] < min(x[1], x[2]);    // lt (inhibit) and min (first-arrival)
r    = hi + 2;                    // inc / delay by a compile-time 2
e    = (a == b);                  // derived eq
```

The relational operators `<`, `>`, `<=`, `>=`, `==`, `!=` (and the `^` min alias) are
**non-associative** — you cannot chain them (`a < b < c` is a parse error; parenthesize instead).

### Instances

A module may instantiate other modules with named port connections (and optional param overrides):

```quack
module Top (input time p, input time q, output time r) {
    wire time lo; wire time hi;
    inst s = Sort2 (.a(p), .b(q), .lo(lo), .hi(hi));
    r = hi + 2;
}
```

Recursion (direct or indirect) is forbidden; the network must be a feed-forward DAG.

### Params and generate blocks

Modules can be parameterized, and `for` / `if` run at compile time to build structure:

```quack
module MinN #(param N = 4) (input time x[N], output time y) {
    wire time acc[N];
    acc[0] = x[0];
    for i in 1..N {
        acc[i] = min(acc[i - 1], x[i]);
    }
    y = acc[N - 1];
}

module DelayN #(param N = 3) (input time a, output time y) {
    y = a + N;
}
```

Delay amounts, vector sizes, loop bounds, and indices must all be compile-time constants.

### Functions

A `func` is a pure compile-time macro with a single `time` output; it is inlined and expanded to
the primitives during elaboration:

```quack
func le(time a, time b) -> time { return a < (b < a); }
```

### Imports and the standard library

`import name.sta;` pulls in another file from the library search path. The standard library
`std.sta` is **auto-imported** before your design is parsed, so `max`, the relational operators,
sorting networks, winner-take-all, coincidence detectors, and min/max trees are available without
an explicit import.

For your own multi-file projects use `include path/to/file.sta;`, which resolves **relative to the
including file** (subfolders allowed, e.g. `include helpers/math.sta;`). Includes are transitive and
cycle-checked. `quackc build top.sta --top T` flattens the top file plus everything it includes into
one generated header; `quackc test top.sta` runs **only the tests defined in `top.sta`** (included
files and the standard library contribute modules/functions, not testbenches). Pass `--std-tests`
to additionally run the standard library's own testbenches.

### Testbenches: `test` / `wave` / `expect`

A `test` block binds to a module and drives one or more `wave`s of stimulus, each listing expected
outputs. Omitted inputs default to `inf`. Values are integer ticks, `inf`, or vector literals like
`[3, inf, 5]`:

```quack
test Sort2Tb for Sort2 {
    wave { a = 3; b = 5; expect lo = 3; expect hi = 5; }
    wave { a = 5; b = 3; expect lo = 3; expect hi = 5; }
    wave { a = 2; b = inf; expect lo = 2; expect hi = inf; }
}
```

Comparison is exact: `inf` equals only `inf`. A failing wave is reported by number and by the
differing wires, and the `test` command exits nonzero.

**Only outputs you `expect` are checked.** An output a wave does not mention is ignored. To instead
assert that every unmentioned output equals its default, add `expect default;` to the wave:

```quack
test Sort2Tb for Sort2 {
    wave { a = 3; b = 5; expect lo = 3; expect default; }   // hi is checked against its default
}
```

The default is `inf` unless a per-test `default { .. }` block overrides it for named outputs:

```quack
test T for M {
    default { ready = 0; }                 // `ready` defaults to 0 instead of inf
    wave { a = 3; expect default; }
}
```

For many similar vectors, a `cases` block is a `test` whose inputs and expected outputs are
equal-length vectors; column `i` becomes wave `i` (an optional `expect default;` line applies to
every expanded wave):

```quack
cases Sort2Cases for Sort2 {
    a      = [3, 5, 2];
    b      = [5, 3, inf];
    expect lo = [3, 3, 2];
    expect hi = [5, 5, inf];
}
```

---

## 3. Building the compiler

`quackc` builds with a standard Rust toolchain (edition 2021):

```sh
cargo build            # build the library and the quackc binary
cargo build --release  # optimized build
cargo test             # run the full test suite
```

The generated-C++ end-to-end tests need a C++17 compiler discoverable through the `CXX`
environment variable; if `CXX` is unset those tests are skipped while the rest of the suite runs.

---

## 4. Installing `quackc` on your PATH

The steps above build `quackc` inside this project's `target\` directory. To run `quackc`
from any folder on your machine, put the binary on your `PATH` and tell it where its standard
library lives.

### Quick install (Windows, one command)

From the project root, run the bundled script. It builds the release binary, copies it and the
standard library into `%USERPROFILE%\quackc`, sets `QUACKC_STDLIB_PATH`, and adds the folder to
your user `PATH`:

```powershell
.\install.ps1
```

Re-run it any time you change the compiler or standard library to update the installed copy. Useful
options:

```powershell
.\install.ps1 -InstallDir D:\tools\quackc   # install somewhere else
.\install.ps1 -SkipBuild                    # reinstall without rebuilding
```

Open a **new** terminal afterwards (so the updated `PATH` takes effect), then run `quackc --help`.

The manual steps are below if you prefer to run them yourself or are on macOS/Linux.

> **Why the stdlib path matters.** Every design auto-imports `std.sta` (for `max`, the
> relational operators, sorting networks, and so on). By default the compiler looks for it next
> to the project source, a location baked in when the binary was compiled. A copied binary
> therefore needs the `QUACKC_STDLIB_PATH` environment variable pointed at the directory that
> contains `std.sta`; otherwise builds fail with "cannot resolve import `std.sta`".

### Windows (PowerShell)

Run these once from the project root after `cargo build --release`. They copy the binary and the
standard library into `%USERPROFILE%\quackc`, add that folder to your user `PATH`, and set
`QUACKC_STDLIB_PATH`:

```powershell
$install = Join-Path $env:USERPROFILE "quackc"
$stdDir  = Join-Path $install "std"
New-Item -ItemType Directory -Force -Path $install, $stdDir | Out-Null

# Copy the release binary and the standard library.
Copy-Item "target\release\quackc.exe" -Destination $install -Force
Copy-Item "std\std.sta"               -Destination $stdDir  -Force

# Point the compiler at its standard library (the directory containing std.sta).
[Environment]::SetEnvironmentVariable("QUACKC_STDLIB_PATH", $stdDir, "User")

# Add the install folder to the user PATH if it is not already there.
$userPath = [Environment]::GetEnvironmentVariable("Path", "User")
if (($userPath -split ';') -notcontains $install) {
    [Environment]::SetEnvironmentVariable("Path", ($userPath.TrimEnd(';') + ";" + $install), "User")
}
```

Open a **new** terminal afterwards so the updated `PATH` and environment variables take effect
(an already-open terminal will not see them). Then verify from any directory:

```powershell
quackc --version   # prints: quackc 0.1.0
quackc --help      # prints the full usage text
where.exe quackc   # prints the install path
```

### macOS / Linux (bash/zsh)

The equivalent, installing into `~/.local/bin` (already on most `PATH`s) with the stdlib under
`~/.local/share/quackc`:

```sh
mkdir -p ~/.local/bin ~/.local/share/quackc
cp target/release/quackc ~/.local/bin/
cp std/std.sta           ~/.local/share/quackc/

# Add to your shell profile (~/.bashrc or ~/.zshrc):
echo 'export QUACKC_STDLIB_PATH="$HOME/.local/share/quackc"' >> ~/.bashrc
echo 'export PATH="$HOME/.local/bin:$PATH"'                  >> ~/.bashrc
```

Reload the shell (`source ~/.bashrc`) or open a new terminal, then run `quackc --version` or `quackc --help` to verify.

### Using it

Once installed you can keep your designs anywhere — for example a `st-algebra\` folder with one
subfolder per project — and build them with a bare command:

```sh
quackc build mydesign.sta --top MyTop --out out
quackc test  mydesign.sta --mode both
```

### Updating or uninstalling

- **Update**: after changing the compiler or standard library, run `cargo build --release` again
  and re-copy `quackc.exe` and `std.sta` into the install folder.
- **Uninstall**: delete the install folder, remove its entry from your user `PATH`, and delete the
  `QUACKC_STDLIB_PATH` variable.

---

## 5. Using the `quackc` CLI

Run the binary with `cargo run --` or the built `quackc` executable. There are three commands.

### `build` — emit C++

```sh
quackc build <design.sta> --top <TopModule> --out <dir>
```

Compiles the design rooted at `--top` and writes one C++ header, `<TopModule>.hpp`, into `<dir>`
(the directory is created if needed). `--top` and `--out` are required.

#### Compiling and running the generated C++

The generated `<TopModule>.hpp` is a **header**, not a runnable program: it defines
`<TopModule>_in` / `<TopModule>_out` structs and a `<TopModule>_eval(...)` function, but no
`main()`. To run it you write a small driver, then compile the two together with a C++17 compiler.

The generated header starts with `#include "quack_rt.hpp"` — a *bare* filename with no path, by
design, so the header stays portable. You tell the compiler where the runtime lives with an
include-path flag (`-I`). The runtime header ships in this repo's `runtime/` directory.

A minimal driver, `main.cpp` (placed in the same `<dir>` as the generated header):

```cpp
#include <iostream>
#include "MyTop.hpp"      // the generated header (use your --top name)
#include "quack_rt.hpp"   // runtime: SpikeTime, INF, st_print

int main() {
    MyTop_in  in{ /* fill input ports, e.g. */ 3, 5 };
    MyTop_out out = MyTop_eval(in);
    quack::st_print(std::cout, out./* an output port, e.g. */ y) << "\n";
    return 0;
}
```

Compile it, pointing `-I` at both the output directory and the repo's `runtime/` folder:

```sh
g++ -std=c++17 -I <dir> -I <repo>/runtime <dir>/main.cpp -o <dir>/app
```

On Windows with MSYS2 g++ (PowerShell), the same with backslashes and an `.exe`:

```powershell
g++ -std=c++17 -I <dir> -I <repo>\runtime <dir>\main.cpp -o <dir>\app.exe
```

Then run the produced executable (`<dir>/app` or `<dir>\app.exe`). The two `-I` flags are what let
`#include "quack_rt.hpp"` resolve — the header deliberately does not hard-code the runtime path.

Note: `build` emits only the generated header; it does **not** copy `quack_rt.hpp` into `<dir>`.
That is why the `-I <repo>/runtime` flag is required. For simply observing a design's behavior you
usually do not need this C++ path at all — prefer `quackc test` and `quackc emit-netlist`.

### `test` — run testbenches

```sh
quackc test <design.sta> [--mode functional|event|both]
                         [--check-invariance] [--check-causality]
```

Compiles the design and runs every `test` block, printing a PASS/FAIL report and the pass/fail
wave counts; a failing wave makes the process exit nonzero.

- `--mode` selects the simulator and defaults to `functional`:
  - `functional` — straight-line functional evaluation (Mode A).
  - `event` — the discrete event-driven engine (Mode B).
  - `both` — run both and cross-check that they agree on every output wire.
- `--check-invariance` re-runs each wave with every finite input shifted by a random constant `c`
  (leaving `inf` inputs unchanged) and fails if any finite output does not shift by exactly `c` or
  any `inf` output becomes finite.
- `--check-causality` confirms in event mode that no output fires earlier than the earliest input
  that can influence it.

### `emit-netlist` — export the IR

```sh
quackc emit-netlist <design.sta> --top <TopModule> --format json|dot
```

Exports the elaborated netlist for `--top` to stdout, either as a versioned JSON document or as a
Graphviz DOT graph (one node per IR node, one edge per IR edge, labeling `Inc` amounts and `Lt`
pass/inhibit roles). `--top` and `--format` are required; a `--format` other than `json`/`dot` is
a usage error.

### Global flags

These apply to every command:

- `--time-width 16|32` — the `SpikeTime` width to compile/simulate at (default `32`).
- `--no-opt` — skip the optimization passes, preserving structure for visualization.
- `--vcd` — enable VCD output from the event-driven mode.

Invalid or missing arguments (an unknown option, a missing required option, a bad `--format` or
`--time-width`) are usage errors: the CLI exits nonzero and prints nothing.

### Help and version

```sh
quackc --help      # (or -h) print usage text listing every command and flag
quackc --version   # (or -V) print the version
```

Both print to stdout and exit `0`. Note that `--help` is the only way to get usage text: because
an unrecognized invocation is a silent usage error, a mistyped command prints nothing rather than a
hint, so reach for `quackc --help` when in doubt.

---

## 6. A worked example: writing and running a test

Create a file `adder_pair.sta` that keeps the earlier of two spikes and delays it:

```quack
module EarlyDelay (input time a, input time b, output time y) {
    y = min(a, b) + 1;    // first-arrival, then delayed by one tick
}

test EarlyDelayTb for EarlyDelay {
    // the earlier input wins, then +1
    wave { a = 3; b = 5; expect y = 4; }
    wave { a = 5; b = 3; expect y = 4; }
    // one input never fires -> the other wins
    wave { a = 2; b = inf; expect y = 3; }
    // both never fire -> output never fires (inf + 1 = inf)
    wave { a = inf; b = inf; expect y = inf; }
}
```

Run it:

```sh
quackc test adder_pair.sta
```

Each wave applies its stimulus, evaluates the network, and compares the produced spike times
against the `expect` values. The runner prints a PASS/FAIL line per test with the pass/fail wave
counts, exiting `0` when everything passes and nonzero if any wave fails.

Run the same testbench under both simulators and check the algebra's properties:

```sh
quackc test adder_pair.sta --mode both --check-invariance --check-causality
```

To see the C++ this design compiles to, or its netlist:

```sh
quackc build adder_pair.sta --top EarlyDelay --out out/
quackc emit-netlist adder_pair.sta --top EarlyDelay --format dot
```

---

## License

Licensed under the [MIT License](LICENSE).

## Contributing

Contributions are welcome — see [CONTRIBUTING.md](CONTRIBUTING.md). The curriculum in
[`lessons/`](lessons/) is a good way to get familiar with the language first.
