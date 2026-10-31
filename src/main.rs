//! Binary entry point for the `quackc` compiler.
//!
//! Argument parsing and command dispatch live in the `cli` module (Requirement 8); this is a thin
//! wrapper that delegates to it and maps the result to a process exit code. The `cli::run` stub is
//! filled in by task 27.

use std::process::ExitCode;

fn main() -> ExitCode {
    quackc::cli::run(std::env::args().skip(1).collect())
}
