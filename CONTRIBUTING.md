# Contributing to quackc

Thanks for your interest! This project is a compiler for the Quack space-time (s-t) algebra
language, written in Rust and emitting C++17.

## Getting started

```sh
cargo build        # build the library and the quackc binary
cargo test         # run the full test suite (390+ tests)
```

The C++ end-to-end tests compile generated code with the compiler named by the `CXX`
environment variable (e.g. `g++`). If `CXX` is unset, those specific tests are skipped and the
rest of the suite still runs.

## Making changes

- Keep changes focused and covered by tests. New language features or bug fixes should come with
  tests in the relevant module (`src/*.rs`) or integration tests (`tests/*.rs`).
- Run `cargo test` before opening a pull request; CI runs build + test on every push and PR.
- Style: `cargo fmt` and `cargo clippy` are run by CI as non-blocking checks. New code is welcome
  to be fmt-clean; a repo-wide reformat is intentionally deferred to keep history readable.
- The standard library (`std/std.sta`) is written in Quack itself — adding a library block is a
  pure `.sta` edit, no compiler change.

## Learning the language

See the [`lessons/`](lessons/) folder for a guided curriculum, and the README for a language
overview and CLI reference.

## License

By contributing, you agree that your contributions are licensed under the MIT License (see
[LICENSE](LICENSE)).