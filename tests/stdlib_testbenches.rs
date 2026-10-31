//! Standard-library testbench end-to-end tests (task 26.1; Requirements 9.5-9.11).
//!
//! Task 26 added the structural stdlib blocks to `std.sta` (Min-tree / Max-tree, `Sort2`, sorting
//! networks built from `Sort2`, strict and first-wins winner-take-all, and a coincidence detector),
//! and task 26.1 added a `test` block for each of them — including an EXHAUSTIVE sweep of `gt`,
//! `eq`, and `neq` over every ordered pair from `{0,1,2,3,4,5,6, inf}` checked against each
//! operator's if/else meaning (Req 9.11).
//!
//! These tests run those `std.sta` testbenches through the real auto-import path and the public test
//! runner. The library's `test` blocks are merged into the design by [`import::load_design`] exactly
//! like a user file's, so running them here is the same thing `quackc test std.sta` would do.
//!
//! The key requirement is Req 9.10/9.11 "run under both modes": every wave is executed under
//! [`testrun::Mode::Both`], which compares the functional interpreter (Mode A) against each wave's
//! expectation AND cross-checks Mode A against the event-driven engine (Mode B) per output wire. A
//! green result therefore proves both that each block computes the specified timing and that the two
//! simulation backends agree on every wave (Req 6.10, 6.11, 9.10, 9.11).

use quackc::import::{self, ImportResolver};
use quackc::lexer::TimeWidth;
use quackc::operators::OperatorTable;
use quackc::testrun::{self, Mode};

/// Load the standard library (auto-imported) merged ahead of a trivial user design, so the merged
/// design carries every `std.sta` `module` and `test` block. Panics with rendered diagnostics on any
/// parse/resolution error so a regression in `std.sta` surfaces immediately.
fn load_std() -> quackc::ast::Design {
    let resolver = ImportResolver::new().with_auto_std();
    import::load_design_scoped(
        &resolver,
        // A minimal, self-contained user module; all blocks under test live in std.sta.
        "module Harness (input time a, output time y) { y = a; }",
        "user.sta",
        &OperatorTable::builtin(),
        TimeWidth::W32,
        // Opt into the standard library's own testbenches (they are off by default now).
        true,
    )
    .unwrap_or_else(|d| {
        let r: Vec<String> = d.iter().map(|x| x.render()).collect();
        panic!("loading std.sta failed:\n{}", r.join("\n"))
    })
}

/// Run every `test` block in `std.sta` under [`Mode::Both`] and assert all waves pass (Req 9.10,
/// 9.11). Under `Both` a wave fails on either an expectation mismatch or a Mode A/B cross-check
/// disagreement, so a green run certifies both.
#[test]
fn all_std_testbenches_pass_under_both_modes() {
    let design = load_std();
    assert!(
        !design.tests.is_empty(),
        "std.sta should define testbench blocks (task 26.1)"
    );

    let report = testrun::run_tests_mode(&design, TimeWidth::W32, Mode::Both);
    assert!(
        !report.any_failed(),
        "all std.sta testbenches must pass under --mode both (Req 9.10, 9.11):\n{}",
        report.render()
    );
}

/// The exhaustive gt/eq/neq sweeps (Req 9.11) must be present and must each cover all 64 ordered
/// pairs of `{0,1,2,3,4,5,6, inf}` (8 x 8). This pins the "exhaustive" requirement structurally so a
/// future edit that trims the sweep is caught even if the remaining waves still pass.
#[test]
fn exhaustive_operator_sweeps_cover_every_ordered_pair() {
    let design = load_std();
    for name in ["GtExhaustive", "EqExhaustive", "NeqExhaustive"] {
        let t = design
            .tests
            .iter()
            .find(|t| t.name == name)
            .unwrap_or_else(|| panic!("std.sta must define the `{name}` testbench (Req 9.11)"));
        assert_eq!(
            t.waves.len(),
            64,
            "`{name}` must sweep all 8x8 = 64 ordered pairs of {{0..6, inf}} (Req 9.11)"
        );
    }
}

/// A testbench must exist for each structural block (Req 9.10): the trees, the compare-exchange,
/// the sorting networks, both WTA variants, and the coincidence detector.
#[test]
fn each_structural_block_has_a_testbench() {
    let design = load_std();
    let names: Vec<&str> = design.tests.iter().map(|t| t.name.as_str()).collect();
    for want in [
        "MinTree4Tb",
        "MaxTree4Tb",
        "MinTreeTb",
        "MaxTreeTb",
        "Sort2Tb",
        "Sort2FlatTb",
        "Sort3FlatTb",
        "Sort4FlatTb",
        "Wta3StrictTb",
        "Wta3FirstTb",
        "CoincidenceTb",
    ] {
        assert!(
            names.contains(&want),
            "std.sta must define the `{want}` testbench (Req 9.10); have {names:?}"
        );
    }
}
