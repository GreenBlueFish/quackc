//! Semantics-preservation test for the optimization framework (task 23.1, Requirement 11.4).
//!
//! This test pins **Property 6** from the design: for every input vector, the optimized and
//! unoptimized networks produce identical outputs (Req 11.4). It builds the netlist IR for several
//! designs that exercise each built-in pass (delay-fold, algebraic-simplify, dead-node-elimination),
//! clones it, runs [`quackc::opt::PassManager::default_pipeline`] over one copy, then evaluates both
//! the optimized and the original IR with the event-driven engine [`quackc::sim_event`] over a grid
//! of sampled input vectors that **includes finite spike times and `inf`**, asserting the two
//! networks fire every output wire at exactly the same tick for every sampled input.
//!
//! The event engine is used as the shared evaluation path (the task allows either backend); its
//! per-wire first-firing ticks are the observable I/O semantics a pass must preserve. `inf` is the
//! engine's "never fires" sentinel [`quackc::sim_event::INF`], so sampling it alongside finite
//! values covers the `inf`-involving algebraic identities (`min(a, inf)`, `lt(inf, b)`,
//! `lt(a, inf)`).

use std::collections::HashMap;

use quackc::ir::{self, NetlistIr, NodeKind};
use quackc::lexer::TimeWidth;
use quackc::opt::PassManager;
use quackc::operators::OperatorTable;
use quackc::parser::parse_source;
use quackc::sema::analyze;
use quackc::sim_event::{self, SpikeTime, DEFAULT_MAX_TICK, INF};

/// Parse + analyze + elaborate `src` rooted at `top`, then build the netlist IR, panicking with the
/// rendered diagnostics on any earlier-stage error so the test starts from a validated design.
fn build_ir(src: &str, top: &str) -> NetlistIr {
    let design = parse_source(src, "opt_semantics.sta", &OperatorTable::builtin(), TimeWidth::W32)
        .unwrap_or_else(|d| {
            let r: Vec<String> = d.iter().map(|x| x.render()).collect();
            panic!("fixture failed to parse:\n{}", r.join("\n"))
        });
    let checked = analyze(&design).unwrap_or_else(|errs| {
        let r: Vec<String> = errs.iter().map(|e| e.render()).collect();
        panic!("fixture failed sema:\n{}", r.join("\n"))
    });
    let elab = quackc::elaborate::elaborate(&checked, top).unwrap_or_else(|errs| {
        let r: Vec<String> = errs.iter().map(|e| e.render()).collect();
        panic!("fixture failed to elaborate:\n{}", r.join("\n"))
    });
    ir::build(&elab)
}

/// The names of the real input-port wires of an IR (excluding the `inf` constant source), each as a
/// scalar wire name `a` or a vector element `a[i]`.
fn input_wires(ir: &NetlistIr) -> Vec<String> {
    let mut wires: Vec<String> = ir
        .nodes
        .iter()
        .filter(|n| matches!(n.kind, NodeKind::Input))
        .filter(|n| !(n.name == "inf" && ir.incoming(n.id).next().is_none()))
        .map(|n| n.name.clone())
        .collect();
    wires.sort();
    wires.dedup();
    wires
}

/// The names of the output-port wires of an IR.
fn output_wires(ir: &NetlistIr) -> Vec<String> {
    let mut wires: Vec<String> = ir
        .nodes
        .iter()
        .filter(|n| matches!(n.kind, NodeKind::Output))
        .map(|n| n.name.clone())
        .collect();
    wires.sort();
    wires.dedup();
    wires
}

/// The sampled spike-time values each input wire ranges over: a spread of finite ticks plus `inf`
/// (the "never fires" sentinel). Including `inf` is required by task 23.1 so the `inf`-involving
/// algebraic identities are exercised.
const SAMPLE_VALUES: &[SpikeTime] = &[0, 1, 2, 3, 5, INF];

/// Enumerate every assignment of [`SAMPLE_VALUES`] to the given input wires (the full cartesian
/// grid). With a handful of wires and six sampled values this stays small enough to run exhaustively.
fn input_grid(wires: &[String]) -> Vec<HashMap<String, SpikeTime>> {
    let mut grid: Vec<HashMap<String, SpikeTime>> = vec![HashMap::new()];
    for w in wires {
        let mut next = Vec::with_capacity(grid.len() * SAMPLE_VALUES.len());
        for assignment in &grid {
            for &v in SAMPLE_VALUES {
                let mut a = assignment.clone();
                a.insert(w.clone(), v);
                next.push(a);
            }
        }
        grid = next;
    }
    grid
}

/// Build the IR for `src`/`top`, optimize a clone, and assert the optimized and unoptimized
/// networks produce identical per-output-wire firing ticks for every sampled input vector that
/// includes `inf` (Property 6, Req 11.4).
fn assert_semantics_preserved(name: &str, src: &str, top: &str) {
    let original = build_ir(src, top);

    // Optimize a clone with the default pipeline (delay-fold, simplify, DCE).
    let mut optimized = original.clone();
    PassManager::default_pipeline()
        .run_all(&mut optimized)
        .unwrap_or_else(|errs| {
            let r: Vec<String> = errs.iter().map(|e| e.render()).collect();
            panic!("`{name}`: optimizer reported diagnostics:\n{}", r.join("\n"))
        });

    // The optimizer must not invent or drop observable ports: the output wire set is unchanged.
    assert_eq!(
        output_wires(&optimized),
        output_wires(&original),
        "`{name}`: optimization changed the set of output wires"
    );

    // Compare outputs over the full sampled grid (finite values and inf).
    let wires = input_wires(&original);
    for inputs in input_grid(&wires) {
        let before = sim_event::run(&original, &inputs, TimeWidth::W32, DEFAULT_MAX_TICK);
        let after = sim_event::run(&optimized, &inputs, TimeWidth::W32, DEFAULT_MAX_TICK);

        for out in output_wires(&original) {
            let b = before.output(&out);
            let a = after.output(&out);
            assert_eq!(
                a, b,
                "`{name}`: output `{out}` differs after optimization for inputs {inputs:?} \
                 (unoptimized={b}, optimized={a})"
            );
        }
    }
}

#[test]
fn delay_fold_preserves_semantics() {
    // Two stacked delays fold into one; the firing tick must be unchanged across finite inputs and
    // inf (inc of inf stays inf).
    assert_semantics_preserved(
        "delay-fold",
        "module M (input time a, output time y) { wire time t; t = inc(inc(a)); y = inc(t); }",
        "M",
    );
}

#[test]
fn algebraic_min_identities_preserve_semantics() {
    // min(a, a) -> a and min(a, inf) -> a. `inf` is written with the keyword and sampled as an
    // input value too, so both the structural identity and the runtime inf case are covered.
    assert_semantics_preserved(
        "min-identities",
        "module M (input time a, output time y, output time z) { \
            y = min(a, a); \
            z = min(a, inf); \
         }",
        "M",
    );
}

#[test]
fn algebraic_lt_identities_preserve_semantics() {
    // lt(a, a) -> inf, lt(inf, b) -> inf, lt(a, inf) -> a. Sampling inf for a and b exercises the
    // runtime behaviour of each identity in addition to the structural rewrite.
    assert_semantics_preserved(
        "lt-identities",
        "module M (input time a, input time b, \
                   output time p, output time q, output time r) { \
            p = a < a; \
            q = inf < b; \
            r = a < inf; \
         }",
        "M",
    );
}

#[test]
fn mixed_network_preserves_semantics() {
    // A network that triggers all three passes together: a folded delay feeding a min against
    // itself, plus a dead sub-expression that DCE prunes without touching the live output.
    assert_semantics_preserved(
        "mixed",
        "module M (input time a, input time b, output time y) { \
            wire time d; wire time dead; \
            d = inc(inc(a)); \
            dead = min(b, inc(b)); \
            y = min(d, d); \
         }",
        "M",
    );
}
