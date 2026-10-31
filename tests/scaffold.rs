//! Integration test confirming the `cargo test` harness runs and the crate's library target links.
//!
//! Scaffolded by task 1. As later tasks fill in the pipeline-stage modules, their own unit and
//! integration tests join this harness. This test exists so `cargo test` has at least one test to
//! run cleanly from the start of the project.

#[test]
fn crate_links_and_cli_runs() {
    // Exercising the one stubbed public entry point keeps the library + binary wiring honest.
    // `ExitCode` is opaque (no `PartialEq`), so we just confirm the call links and returns.
    let _code = quackc::cli::run(Vec::new());
}
