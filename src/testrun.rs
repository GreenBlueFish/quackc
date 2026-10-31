//! Test runner and property checks: waves, expectations, invariance/causality (Requirement 7).
//!
//! Stub scaffolded by task 1; implemented by task 14 (and extended by tasks 15, 18, 20).
//!
//! # Task 14 — the test runner (Req 7.1, 7.2, 7.3, 7.4, 7.8)
//!
//! For each `test` block (which binds to a module) and each `wave` in it, the runner:
//!
//! * applies the wave's input stimulus, defaulting any omitted input to `inf` (Req 7.4);
//! * runs the functional (Mode A) simulation of the bound module;
//! * compares every produced output spike time against its `expect` value for *exact* equality,
//!   treating `inf` as equal only to `inf` (Req 7.1);
//! * reports PASS when all outputs match, or FAIL naming the wave number and each differing wire
//!   (Req 7.2);
//! * collects per-wave results into a [`TestReport`] whose [`TestReport::any_failed`] lets the CLI
//!   (task 27) set a nonzero exit code (Req 7.3).
//!
//! Numeric literals and `inf` are accepted as stimulus / expected values, including vector literals
//! like `[3, inf, 5]` (Req 7.8); these come from the AST as [`crate::ast::StimVal`].
//!
//! ## Mode A simulation: a Rust-side interpreter of the netlist IR
//!
//! The functional backend ([`crate::codegen_cpp`]) emits C++ text that is not runnable from Rust.
//! For the test runner we instead interpret the [`crate::ir::NetlistIr`] directly in Rust, using
//! the exact saturating semantics of the hand-written C++ runtime `quack_rt.hpp`:
//!
//! * A spike time is a [`SpikeTime`] (`u64`); the "never spikes" sentinel is [`INF`] (`u64::MAX`),
//!   mirroring the runtime's `INF = numeric_limits<SpikeTime>::max()`.
//! * [`st_inc`] saturates: `INF` stays `INF`, and any sum that would reach or exceed the chosen
//!   time width's maximum clamps to `INF` — matching `quack_rt.hpp`'s 64-bit-accumulator test
//!   against `INF`.
//! * [`st_min`] is a plain minimum (`INF`-as-max makes first-arrival correct).
//! * [`st_lt`] returns `a` when `a < b`, else `INF` (ties block).
//!
//! The internal `INF` sentinel is always `u64::MAX` regardless of the chosen [`TimeWidth`]; the
//! width only bounds the *finite* range, so `st_inc` saturates to `INF` once a sum reaches the
//! width maximum (the same value the width's `inf` keyword denotes). This keeps the interpreter
//! consistent with the C++ runtime for whichever width a design is compiled at.
//!
//! Nodes are evaluated in topological order. `Input` nodes read the wave's input assignment (the
//! `inf` constant node — an `Input`-kind node named `"inf"` with no incoming edges, exactly as
//! [`crate::ir`] emits it — reads as `INF`). An `Output` node takes its single driver's value.
//!
//! ### Instance boundaries
//!
//! Sub-module instances are FLATTENED into the single top graph during IR build (see
//! `ir::build_instance`): each submodule's primitive nodes are spliced in, its input ports bound to
//! the parent's connected producers and its output ports wired back to the parent's signals. The
//! interpreter therefore evaluates an instance exactly as if its body had been written inline, so a
//! hierarchical design simulates identically to its flat equivalent. There is no opaque instance
//! boundary at this stage.

use std::collections::HashMap;

use crate::ast::{Design, StimVal, StimValKind, Test, Wave};
use crate::ir::{self, NetlistIr, NodeId, NodeKind, PortRole};
use crate::lexer::TimeWidth;
use crate::sim_event;

/// Which simulation backend(s) the test runner drives for each wave (Req 7.5).
///
/// The CLI (task 27) maps `--mode functional|event|both` onto these; the default is
/// [`Mode::Functional`], matching Req 8.3 ("default the mode to functional").
///
/// * [`Mode::Functional`] — compare the functional interpreter's output (Mode A) against each
///   wave's `expect` values (Req 7.1).
/// * [`Mode::Event`] — compare the event-driven engine's output (Mode B, via [`sim_event::run`])
///   against each wave's `expect` values (Req 7.1).
/// * [`Mode::Both`] — compare Mode A against the expectations *and* cross-check Mode A against
///   Mode B per output wire, reporting any disagreement as a cross-check failure (Req 6.10, 6.11).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Mode {
    /// Functional interpreter (Mode A) only; the default (Req 8.3).
    #[default]
    Functional,
    /// Event-driven engine (Mode B) only.
    Event,
    /// Both backends, with a per-wire Mode A/B cross-check (Req 6.10, 6.11).
    Both,
}

/// A spike time in the interpreter: a non-negative tick, or [`INF`] for "never spikes".
pub type SpikeTime = u64;

/// The "never spikes" sentinel: the maximum `u64`, mirroring `quack_rt.hpp`'s
/// `INF = numeric_limits<SpikeTime>::max()`. Used uniformly regardless of the chosen time width;
/// the width only bounds the finite range (see module docs).
pub const INF: SpikeTime = u64::MAX;

/// `st_inc(a, n)` with saturating arithmetic (Req 7, matching `quack_rt.hpp`).
///
/// `INF` stays `INF`; otherwise `a + n` clamps to `INF` if it would reach or exceed `max_finite`
/// (the chosen width's maximum, which is the value the width's `inf` denotes).
fn st_inc(a: SpikeTime, n: u64, max_finite: u64) -> SpikeTime {
    if a == INF {
        return INF;
    }
    let sum = a.saturating_add(n);
    if sum >= max_finite {
        INF
    } else {
        sum
    }
}

/// `st_min(a, b)` — first arrival. `INF`-as-max makes a plain minimum correct (Req 7, `quack_rt`).
fn st_min(a: SpikeTime, b: SpikeTime) -> SpikeTime {
    a.min(b)
}

/// `st_lt(a, b)` — strict less-than / inhibit. Returns `a` if `a < b`, else `INF` (ties block)
/// (Req 7, matching `quack_rt.hpp`).
fn st_lt(a: SpikeTime, b: SpikeTime) -> SpikeTime {
    if a < b {
        a
    } else {
        INF
    }
}

/// The maximum finite spike time for a width: the value the width's `inf` keyword denotes and the
/// clamp threshold for `st_inc`. For `W32` this is `u32::MAX`, for `W16` it is `u16::MAX`.
fn max_finite(width: TimeWidth) -> u64 {
    width.max_literal()
}

/// The outcome of comparing one output wire against its expected value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WireMismatch {
    /// The output wire name (e.g. `y` or `y[1]`).
    pub wire: String,
    /// The value the simulation produced.
    pub produced: SpikeTime,
    /// The value the wave expected.
    pub expected: SpikeTime,
}

/// The outcome of cross-checking one output wire between Mode A and Mode B (Req 6.10, 6.11).
///
/// A cross-check mismatch is a *distinct* failure from an expectation mismatch ([`WireMismatch`]):
/// it means the two backends disagree with each other, which is a compiler bug, independent of what
/// the wave expected.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CrossCheckMismatch {
    /// The output wire name (e.g. `y` or `y[1]`).
    pub wire: String,
    /// The tick the functional interpreter (Mode A) produced.
    pub mode_a: SpikeTime,
    /// The tick the event-driven engine (Mode B) produced.
    pub mode_b: SpikeTime,
}

/// The outcome of the invariance property check for one output wire (Req 7.6, design Property 4).
///
/// This is a *distinct* failure from an expectation mismatch ([`WireMismatch`]) or a cross-check
/// disagreement ([`CrossCheckMismatch`]): it means shifting every finite input by a constant `c`
/// did **not** shift this finite output by exactly `c`, or turned an `inf` output finite (or vice
/// versa). For a legal program invariance always holds, so a nonempty list indicates a compiler bug
/// (design Property 4).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InvarianceFailure {
    /// The output wire name (e.g. `y` or `y[1]`).
    pub wire: String,
    /// The output tick on the unshifted (baseline) run.
    pub base: SpikeTime,
    /// The output tick on the run with every finite input shifted by `shift`.
    pub shifted: SpikeTime,
    /// The constant `c` added to every finite input for this check.
    pub shift: u64,
}

/// The outcome of the causality property check for one output wire (Req 7.7, design Property 3).
///
/// A causality failure means an output fired *earlier* than the earliest input that can influence
/// it (computed from graph reachability over the IR). For a legal program this never happens, so a
/// nonempty list indicates a compiler bug (design Property 3). A never-firing output (`inf`)
/// trivially satisfies causality and never appears here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CausalityFailure {
    /// The output wire name (e.g. `y` or `y[1]`).
    pub wire: String,
    /// The tick this output fired.
    pub output: SpikeTime,
    /// The tick of the earliest input that can influence this output (`inf` if no finite
    /// influencing input exists).
    pub earliest_input: SpikeTime,
}

/// The result of running one `wave`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WaveResult {
    /// 1-based index of this wave within its test block (used in FAIL messages) (Req 7.2).
    pub index: usize,
    /// Every output wire whose produced value differed from its expected value; empty on PASS.
    ///
    /// "Produced" is the active mode's output: Mode A under [`Mode::Functional`]/[`Mode::Both`],
    /// Mode B under [`Mode::Event`] (Req 7.1, 7.5).
    pub mismatches: Vec<WireMismatch>,
    /// Every output wire on which Mode A and Mode B disagreed, populated only under [`Mode::Both`]
    /// (Req 6.10, 6.11); empty otherwise. A nonempty list fails the wave distinctly from an
    /// expectation mismatch.
    pub cross_check_mismatches: Vec<CrossCheckMismatch>,
    /// Every output wire that violated the invariance property, populated only when the invariance
    /// check is enabled via [`CheckOptions`] (Req 7.6); empty otherwise. A nonempty list fails the
    /// wave distinctly from an expectation or cross-check mismatch (design Property 4).
    pub invariance_failures: Vec<InvarianceFailure>,
    /// Every output wire that violated the causality property, populated only when the causality
    /// check is enabled via [`CheckOptions`] (Req 7.7); empty otherwise. A nonempty list fails the
    /// wave distinctly from an expectation or cross-check mismatch (design Property 3).
    pub causality_failures: Vec<CausalityFailure>,
}

impl WaveResult {
    /// Whether this wave passed: every output matched its expectation, no Mode A/B cross-check
    /// disagreement occurred, and no enabled property check (invariance / causality) failed
    /// (Req 7.2, 6.11, 7.6, 7.7).
    pub fn passed(&self) -> bool {
        self.mismatches.is_empty()
            && self.cross_check_mismatches.is_empty()
            && self.invariance_failures.is_empty()
            && self.causality_failures.is_empty()
    }
}

/// The result of running one `test` block over all of its waves.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TestResult {
    /// The test block's name.
    pub name: String,
    /// The module the test is bound to.
    pub module: String,
    /// Per-wave results in declaration order.
    pub waves: Vec<WaveResult>,
    /// A setup error (e.g. elaboration failed, or the bound module is missing) that prevented the
    /// test from running; `None` when the test ran. A test that could not run counts as failed.
    pub error: Option<String>,
}

impl TestResult {
    /// Whether every wave in this test passed and no setup error occurred (Req 7.2, 7.3).
    pub fn passed(&self) -> bool {
        self.error.is_none() && self.waves.iter().all(WaveResult::passed)
    }

    /// Number of passing waves.
    pub fn passed_waves(&self) -> usize {
        self.waves.iter().filter(|w| w.passed()).count()
    }

    /// Number of failing waves.
    pub fn failed_waves(&self) -> usize {
        self.waves.iter().filter(|w| !w.passed()).count()
    }
}

/// The full report of running every `test` block in a design: the data the CLI consumes to decide
/// its exit code (Req 7.3).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TestReport {
    /// One entry per `test` block, in declaration order.
    pub tests: Vec<TestResult>,
}

impl TestReport {
    /// Whether any wave in any test failed, or any test could not run (Req 7.3).
    ///
    /// The CLI (task 27) sets a nonzero process exit code when this is `true`.
    pub fn any_failed(&self) -> bool {
        self.tests.iter().any(|t| !t.passed())
    }

    /// Total number of passing waves across all tests.
    pub fn total_passed_waves(&self) -> usize {
        self.tests.iter().map(TestResult::passed_waves).sum()
    }

    /// Total number of failing waves across all tests.
    pub fn total_failed_waves(&self) -> usize {
        self.tests.iter().map(TestResult::failed_waves).sum()
    }

    /// Render the report as plain-language PASS/FAIL lines, one per wave, with per-wire detail on a
    /// failing wave (Req 7.2). A test that failed to run is reported with its setup error.
    pub fn render(&self) -> String {
        let mut lines = Vec::new();
        for test in &self.tests {
            if let Some(err) = &test.error {
                lines.push(format!(
                    "test `{}` for module `{}`: ERROR: {}",
                    test.name, test.module, err
                ));
                continue;
            }
            for wave in &test.waves {
                if wave.passed() {
                    lines.push(format!(
                        "test `{}` wave {}: PASS",
                        test.name, wave.index
                    ));
                    continue;
                }
                let mut parts = Vec::new();
                if !wave.mismatches.is_empty() {
                    let details = wave
                        .mismatches
                        .iter()
                        .map(|m| {
                            format!(
                                "{} (produced {}, expected {})",
                                m.wire,
                                fmt_time(m.produced),
                                fmt_time(m.expected)
                            )
                        })
                        .collect::<Vec<_>>()
                        .join(", ");
                    parts.push(details);
                }
                if !wave.cross_check_mismatches.is_empty() {
                    // A cross-check disagreement is a distinct, compiler-bug class of failure
                    // (Req 6.11): name the wire and the differing ticks from each mode.
                    let details = wave
                        .cross_check_mismatches
                        .iter()
                        .map(|m| {
                            format!(
                                "{} (functional {}, event {})",
                                m.wire,
                                fmt_time(m.mode_a),
                                fmt_time(m.mode_b)
                            )
                        })
                        .collect::<Vec<_>>()
                        .join(", ");
                    parts.push(format!("cross-check mismatch: {details}"));
                }
                if !wave.invariance_failures.is_empty() {
                    // An invariance violation is a distinct, compiler-bug class of failure
                    // (Req 7.6, design Property 4): name the wire, the shift `c`, and the baseline
                    // vs shifted output ticks.
                    let details = wave
                        .invariance_failures
                        .iter()
                        .map(|f| {
                            format!(
                                "{} (base {}, shifted {}, by c={})",
                                f.wire,
                                fmt_time(f.base),
                                fmt_time(f.shifted),
                                f.shift
                            )
                        })
                        .collect::<Vec<_>>()
                        .join(", ");
                    parts.push(format!("invariance violation: {details}"));
                }
                if !wave.causality_failures.is_empty() {
                    // A causality violation is a distinct, compiler-bug class of failure
                    // (Req 7.7, design Property 3): name the wire, the output tick, and the tick of
                    // the earliest input that can influence it.
                    let details = wave
                        .causality_failures
                        .iter()
                        .map(|f| {
                            format!(
                                "{} (output {}, earliest influencing input {})",
                                f.wire,
                                fmt_time(f.output),
                                fmt_time(f.earliest_input)
                            )
                        })
                        .collect::<Vec<_>>()
                        .join(", ");
                    parts.push(format!("causality violation: {details}"));
                }
                lines.push(format!(
                    "test `{}` wave {}: FAIL: {}",
                    test.name,
                    wave.index,
                    parts.join("; ")
                ));
            }
        }
        lines.join("\n")
    }
}

/// Cross-check two per-output-wire tick maps (Mode A vs Mode B) for exact per-wire equality,
/// treating `inf` as equal only to `inf` (Req 6.10, 6.11).
///
/// Returns one [`CrossCheckMismatch`] per wire whose ticks differ, in deterministic (wire-name)
/// order. The union of both maps' keys is checked so a wire present in only one mode (which should
/// never happen for the shared port grouping) still surfaces as a disagreement rather than being
/// silently dropped. For a legal program the two modes always agree, so an empty result is the
/// expected case; a nonempty result indicates a compiler bug (design Property 5).
fn cross_check(
    mode_a: &HashMap<String, SpikeTime>,
    mode_b: &HashMap<String, SpikeTime>,
) -> Vec<CrossCheckMismatch> {
    let mut wires: Vec<&String> = mode_a.keys().chain(mode_b.keys()).collect();
    wires.sort();
    wires.dedup();

    let mut mismatches = Vec::new();
    for wire in wires {
        let a = mode_a.get(wire).copied().unwrap_or(INF);
        let b = mode_b.get(wire).copied().unwrap_or(INF);
        if a != b {
            mismatches.push(CrossCheckMismatch {
                wire: wire.clone(),
                mode_a: a,
                mode_b: b,
            });
        }
    }
    mismatches
}

/// Add a finite shift `c` to a spike time, leaving `inf` unchanged and clamping with the same
/// saturating rule as [`st_inc`] (Req 7.6). An `inf` input stays `inf`; a finite input becomes
/// `a + c`, saturating to `INF` at the width maximum (which, with a well-chosen `c`, never happens
/// for the inputs under test — see [`choose_shift`]).
fn shift_finite(a: SpikeTime, c: u64, max_finite: u64) -> SpikeTime {
    if a == INF {
        INF
    } else {
        st_inc(a, c, max_finite)
    }
}

/// Choose the finite invariance shift `c` for a wave (Req 7.6).
///
/// The shift must be finite and small enough that a legal program's finite outputs can move by `c`
/// without clamping to `inf`: we keep `max_finite_input + c` strictly below the width's finite
/// maximum, so no finite input (and hence, for a shift-equivariant legal program, no finite output)
/// saturates. Within that headroom we pick `c` with a tiny deterministic PRNG seeded from the
/// wave's own stimulus, so the chosen `c` is reproducible across runs yet varies per wave (no new
/// crate dependency — see [`lcg_next`]).
///
/// Returns `0` only when there is no headroom at all (`max_finite_input` already at the ceiling),
/// in which case the caller skips the check (a zero shift is trivially satisfied).
fn choose_shift(wave: &Wave, max_finite: u64, max_finite_input: u64) -> u64 {
    // Reserve a margin below the width maximum so `max_finite_input + c < max_finite`.
    let headroom = max_finite.saturating_sub(max_finite_input);
    if headroom <= 1 {
        return 0;
    }
    // Keep c within [1, upper] where upper leaves at least 1 below the ceiling. Cap the upper bound
    // to a modest value so the shift stays small and readable while still exercising a real move.
    let upper = headroom.saturating_sub(1).min(1024);
    if upper == 0 {
        return 0;
    }
    let seed = seed_from_wave(wave);
    1 + (lcg_next(seed) % upper)
}

/// Derive a deterministic PRNG seed from a wave's stimulus so the invariance shift is reproducible
/// (same wave -> same `c`) without a clock or external randomness source.
fn seed_from_wave(wave: &Wave) -> u64 {
    // A simple order-sensitive mix of each input's name bytes and value. Collisions are harmless:
    // the seed only needs to be deterministic, not unique.
    let mut acc: u64 = 0x9E3779B97F4A7C15; // golden-ratio constant as a starting state
    for (name, val) in &wave.inputs {
        for b in name.bytes() {
            acc = acc.wrapping_mul(31).wrapping_add(b as u64);
        }
        acc = acc.wrapping_mul(31).wrapping_add(stim_scalar(val) & 0xFFFF);
    }
    acc
}

/// One step of a tiny inline linear-congruential generator (LCG), returning a pseudo-random `u64`.
///
/// Uses the Numerical-Recipes-style constants; it is not cryptographic and is only used to pick a
/// reproducible invariance shift without adding a crate dependency.
fn lcg_next(state: u64) -> u64 {
    state
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407)
}

/// Compare a baseline output map against a shifted output map for the invariance property
/// (Req 7.6, design Property 4).
///
/// For each output wire (the union of both maps' keys), with `c` the shift that was added to every
/// finite input:
///
/// * a finite baseline must become `base + c` (saturating like [`st_inc`]) in the shifted run;
/// * an `inf` baseline must stay `inf`.
///
/// Any wire that violates either rule — a finite output that did not move by exactly `c`, an `inf`
/// output that became finite, or a finite output that became `inf` — yields an [`InvarianceFailure`].
/// Results are returned in deterministic (wire-name) order. For a legal program this is always
/// empty; a nonempty result is a compiler bug.
fn check_invariance(
    baseline: &HashMap<String, SpikeTime>,
    shifted: &HashMap<String, SpikeTime>,
    c: u64,
) -> Vec<InvarianceFailure> {
    let mut wires: Vec<&String> = baseline.keys().chain(shifted.keys()).collect();
    wires.sort();
    wires.dedup();

    let mut failures = Vec::new();
    for wire in wires {
        let base = baseline.get(wire).copied().unwrap_or(INF);
        let got = shifted.get(wire).copied().unwrap_or(INF);
        let want = if base == INF {
            // An inf output must stay inf.
            INF
        } else {
            // A finite output must shift by exactly c (saturating like the runtime's inc).
            base.saturating_add(c)
        };
        if got != want {
            failures.push(InvarianceFailure {
                wire: wire.clone(),
                base,
                shifted: got,
                shift: c,
            });
        }
    }
    failures
}

/// Check the causality property for one wave (Req 7.7, design Property 3).
///
/// `earliest_input` maps each output wire to the tick of the earliest input that can influence it
/// (via graph reachability; [`INF`] when no finite influencing input exists). `outputs` maps each
/// output wire to the tick it fired. An output violates causality when it fires strictly earlier
/// than its earliest influencing input — i.e. `output_tick < earliest_input_tick` with the output
/// finite. A never-firing output (`INF`) trivially satisfies causality.
///
/// Returns one [`CausalityFailure`] per violating wire in deterministic (wire-name) order. For a
/// legal program this is always empty; a nonempty result is a compiler bug.
fn check_causality(
    earliest_input: &HashMap<String, SpikeTime>,
    outputs: &HashMap<String, SpikeTime>,
) -> Vec<CausalityFailure> {
    let mut wires: Vec<&String> = outputs.keys().collect();
    wires.sort();
    wires.dedup();

    let mut failures = Vec::new();
    for wire in wires {
        let out = outputs.get(wire).copied().unwrap_or(INF);
        if out == INF {
            continue; // never fires: trivially causal (Req 7.7)
        }
        let earliest = earliest_input.get(wire).copied().unwrap_or(INF);
        // A finite output that fires before the earliest influencing input violates causality.
        if out < earliest {
            failures.push(CausalityFailure {
                wire: wire.clone(),
                output: out,
                earliest_input: earliest,
            });
        }
    }
    failures
}

/// The set of `Input`-kind source node ids (excluding the `inf` constant) that can reach `target`
/// along directed edges — i.e. the inputs that can influence `target` (Req 7.7, design Property 3).
///
/// Computed by a backward breadth-first walk from `target` over incoming edges; any `Input` node
/// (that is not the `inf` constant source) encountered is an influencing input.
fn reachable_input_sources(ir: &NetlistIr, target: NodeId) -> Vec<NodeId> {
    use std::collections::HashSet;
    let mut seen: HashSet<NodeId> = HashSet::new();
    let mut sources: Vec<NodeId> = Vec::new();
    let mut stack: Vec<NodeId> = vec![target];
    seen.insert(target);

    while let Some(id) = stack.pop() {
        if let Some(node) = ir.node(id) {
            if id != target
                && matches!(node.kind, NodeKind::Input)
                && !is_inf_constant(ir, id)
            {
                sources.push(id);
            }
        }
        for edge in ir.incoming(id) {
            if seen.insert(edge.from) {
                stack.push(edge.from);
            }
        }
    }
    sources.sort_unstable();
    sources
}

/// Render a spike time for a diagnostic: `inf` for the sentinel, else the integer (Req 7.1, 7.2).
fn fmt_time(t: SpikeTime) -> String {
    if t == INF {
        "inf".to_string()
    } else {
        t.to_string()
    }
}

/// Which property checks the test runner runs per wave, in addition to comparing against each
/// wave's expectations (Req 7.6, 7.7).
///
/// The CLI (task 27) maps `--check-invariance` / `--check-causality` onto these; both default to
/// disabled so [`run_tests`] / [`run_tests_mode`] behave exactly as before. A property check never
/// fails for a legal program — a failure means a compiler bug (design Properties 3 and 4) — so
/// enabling them over the Section-4 example designs is expected to pass.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct CheckOptions {
    /// Run the invariance property check per wave (Req 7.6, design Property 4): shift every finite
    /// input by a finite constant `c`, re-run, and verify every finite output shifted by exactly
    /// `c` while `inf` outputs stayed `inf`.
    pub invariance: bool,
    /// Run the causality property check per wave (Req 7.7, design Property 3): in event mode,
    /// verify no output fires earlier than the earliest input that can influence it (via graph
    /// reachability on the IR).
    pub causality: bool,
}

impl CheckOptions {
    /// No property checks enabled (the default): the runner only compares against expectations and
    /// (under [`Mode::Both`]) cross-checks the two backends.
    pub const fn none() -> Self {
        CheckOptions {
            invariance: false,
            causality: false,
        }
    }

    /// Whether any property check is enabled.
    pub fn any(self) -> bool {
        self.invariance || self.causality
    }
}

/// Run every `test` block in `design` and collect a [`TestReport`] (Req 7.1-7.4, 7.8).
///
/// Each test is elaborated and lowered to a netlist IR rooted at its bound module, then simulated
/// per wave with the Mode A interpreter. The `width` selects the spike-time range for saturation
/// (defaulting to `W32` matches the compiler's default). A test that cannot be set up (missing
/// module, elaboration failure) is recorded with an `error` and counts as failed (Req 7.3).
pub fn run_tests(design: &Design, width: TimeWidth) -> TestReport {
    run_tests_mode(design, width, Mode::Functional)
}

/// Run every `test` block in `design` with an explicit [`Mode`] and collect a [`TestReport`]
/// (Req 7.1-7.5, 6.10, 6.11, 7.8).
///
/// The `mode` selects which backend's output is compared against each wave's expectations, and
/// whether a Mode A/B cross-check runs:
///
/// * [`Mode::Functional`] — compare Mode A output vs expected (identical to [`run_tests`]).
/// * [`Mode::Event`] — compare Mode B output vs expected.
/// * [`Mode::Both`] — compare Mode A vs expected *and* cross-check Mode A against Mode B per output
///   wire, surfacing any disagreement as a [`CrossCheckMismatch`] (Req 6.10, 6.11).
///
/// The event-driven engine (Mode B) is bounded by [`sim_event::DEFAULT_MAX_TICK`]: a fixed positive
/// bound (Req 6.8) that comfortably exceeds the width's finite range, so any firing a legal program
/// can produce within the saturation range is reached before the bound, and anything that would
/// exceed it already saturates to `inf`. This keeps Mode B's result identical to Mode A's.
pub fn run_tests_mode(design: &Design, width: TimeWidth, mode: Mode) -> TestReport {
    run_tests_checked(design, width, mode, CheckOptions::none())
}

/// Run every `test` block in `design` with an explicit [`Mode`] *and* a set of property
/// [`CheckOptions`], collecting a [`TestReport`] (Req 7.1-7.7, 6.10, 6.11, 7.8).
///
/// This is the full entry point: it behaves exactly like [`run_tests_mode`] (same expectation
/// comparison and Mode A/B cross-check), and additionally — per wave — runs whichever property
/// checks `checks` enables:
///
/// * [`CheckOptions::invariance`] (Req 7.6, design Property 4): pick a finite constant `c`, add it
///   to every finite input (leaving `inf` inputs), re-run the active backend, and record an
///   [`InvarianceFailure`] for any output that did not shift by exactly `c` (or crossed the
///   finite/`inf` boundary).
/// * [`CheckOptions::causality`] (Req 7.7, design Property 3): run the event backend and record a
///   [`CausalityFailure`] for any output whose firing tick precedes the earliest input that can
///   influence it (via graph reachability on the IR).
///
/// A property check never fails for a legal program, so a nonempty failure list is a compiler bug
/// that [`TestReport::any_failed`] propagates to the CLI exit code (Req 7.3).
pub fn run_tests_checked(
    design: &Design,
    width: TimeWidth,
    mode: Mode,
    checks: CheckOptions,
) -> TestReport {
    let tests = design
        .tests
        .iter()
        .map(|t| run_test(design, t, width, mode, checks))
        .collect();
    TestReport { tests }
}

/// Run a single `test` block over all of its waves.
fn run_test(
    design: &Design,
    test: &Test,
    width: TimeWidth,
    mode: Mode,
    checks: CheckOptions,
) -> TestResult {
    // Build the netlist IR rooted at the bound module so instance bodies are at least present in
    // the hierarchy; the interpreter evaluates the flat top-module graph.
    let netlist = match build_netlist(design, &test.module) {
        Ok(ir) => ir,
        Err(e) => {
            return TestResult {
                name: test.name.clone(),
                module: test.module.clone(),
                waves: Vec::new(),
                error: Some(e),
            };
        }
    };

    let sim = Simulator::new(&netlist, width);

    // Resolve the test's `default { .. }` block (if any) into output-wire -> value, used only when
    // a wave writes `expect default;`. A scalar default `y = v` applies to `y`; a vector default
    // `v = [..]` applies element-wise to `v[i]`. Outputs not named here fall back to `inf`.
    let defaults = sim.resolve_defaults(&test.defaults);

    let waves = test
        .waves
        .iter()
        .enumerate()
        .map(|(i, wave)| sim.run_wave(i + 1, wave, &netlist, width, mode, checks, &defaults))
        .collect();

    TestResult {
        name: test.name.clone(),
        module: test.module.clone(),
        waves,
        error: None,
    }
}

/// Elaborate `design` rooted at `top` and build its netlist IR, or return a plain-language error.
fn build_netlist(design: &Design, top: &str) -> Result<NetlistIr, String> {
    let checked = crate::sema::analyze(design).map_err(|errs| render_errs("semantic analysis", &errs))?;
    let elab = crate::elaborate::elaborate(&checked, top)
        .map_err(|errs| render_errs("elaboration", &errs))?;
    Ok(ir::build(&elab))
}

/// Join a diagnostic list into one plain-language message for a setup error.
fn render_errs(stage: &str, errs: &[crate::diag::Diagnostic]) -> String {
    let rendered = errs
        .iter()
        .map(|d| d.render())
        .collect::<Vec<_>>()
        .join("; ");
    format!("{stage} failed: {rendered}")
}

/// A Mode A interpreter over one module's flat netlist graph.
struct Simulator<'a> {
    ir: &'a NetlistIr,
    /// Topological order of node ids; a dependency precedes its consumers.
    order: Vec<NodeId>,
    max_finite: u64,
    /// Grouped input ports: base name -> (width, index -> node id).
    inputs: HashMap<String, PortGroup>,
    /// Grouped output ports: base name -> (width, index -> node id).
    outputs: HashMap<String, PortGroup>,
}

/// A port grouped from the IR's per-element nodes.
#[derive(Clone, Debug, Default)]
struct PortGroup {
    /// `None` for a scalar, `Some(n)` for a vector of width `n`.
    width: Option<u64>,
    /// Element index -> node id (scalars use index 0).
    elems: Vec<(u64, NodeId)>,
}

impl<'a> Simulator<'a> {
    fn new(ir: &'a NetlistIr, width: TimeWidth) -> Self {
        let order = topo_order(ir).unwrap_or_default();
        let mut inputs: HashMap<String, PortGroup> = HashMap::new();
        let mut outputs: HashMap<String, PortGroup> = HashMap::new();

        for node in &ir.nodes {
            match node.kind {
                NodeKind::Input if !is_inf_constant(ir, node.id) => {
                    group_insert(&mut inputs, &node.name, node.id);
                }
                NodeKind::Output => {
                    group_insert(&mut outputs, &node.name, node.id);
                }
                _ => {}
            }
        }

        Simulator {
            ir,
            order,
            max_finite: max_finite(width),
            inputs,
            outputs,
        }
    }

    /// Run one wave under `mode`: apply its inputs, evaluate the selected backend(s), compare the
    /// active mode's output against the wave's expectations, (under [`Mode::Both`]) cross-check
    /// Mode A against Mode B per output wire, and run whichever property checks `checks` enables
    /// (Req 7.1, 7.2, 7.5, 6.10, 6.11, 7.6, 7.7).
    fn run_wave(
        &self,
        index: usize,
        wave: &Wave,
        netlist: &NetlistIr,
        width: TimeWidth,
        mode: Mode,
        checks: CheckOptions,
        defaults: &HashMap<String, SpikeTime>,
    ) -> WaveResult {
        // Resolve the input stimulus per input-port element, defaulting omissions to `inf`
        // (Req 7.4). The map is keyed by node id of each input element node.
        let input_values = self.resolve_inputs(wave);

        // Mode A (functional interpreter): each output wire -> produced tick.
        let mode_a: HashMap<String, SpikeTime> = {
            let values = self.evaluate(&input_values);
            self.output_wire_values(&values)
        };

        // Mode B (event-driven engine): run only when the mode needs it (Event or Both). The engine
        // takes a wire-keyed input map (`a` / `a[i]`) and is bounded by DEFAULT_MAX_TICK (Req 6.8).
        let mode_b: Option<HashMap<String, SpikeTime>> = match mode {
            Mode::Functional => None,
            Mode::Event | Mode::Both => {
                let event_inputs = self.event_input_map(wave);
                let run = sim_event::run(netlist, &event_inputs, width, sim_event::DEFAULT_MAX_TICK);
                Some(run.outputs)
            }
        };

        // Resolve expected values per output-port element (Req 7.1, 7.8). Only explicitly
        // expected outputs are checked, plus (if the wave wrote `expect default;`) all other
        // outputs against the test defaults or `inf`.
        let expected = self.resolve_expects(wave, defaults);

        // The "produced" output to compare against expectations is the active mode's (Req 7.5):
        // Mode A for Functional/Both, Mode B for Event.
        let produced = match (mode, &mode_b) {
            (Mode::Event, Some(b)) => b,
            _ => &mode_a,
        };

        // Compare produced vs expected for exact equality (inf == inf only) (Req 7.1, 7.2).
        let mut mismatches = Vec::new();
        for (wire, &expected_val) in &expected {
            let got = produced.get(wire).copied().unwrap_or(INF);
            if got != expected_val {
                mismatches.push(WireMismatch {
                    wire: wire.clone(),
                    produced: got,
                    expected: expected_val,
                });
            }
        }
        // Deterministic order for FAIL messages.
        mismatches.sort_by(|a, b| a.wire.cmp(&b.wire));

        // Under Both, cross-check Mode A against Mode B per output wire (Req 6.10, 6.11).
        let cross_check_mismatches = match (mode, &mode_b) {
            (Mode::Both, Some(b)) => cross_check(&mode_a, b),
            _ => Vec::new(),
        };

        // Invariance property check (Req 7.6, design Property 4): shift every finite input by a
        // finite constant `c`, re-run the active backend, and record any output that did not shift
        // by exactly `c` (or crossed the finite/inf boundary). The baseline is the active mode's
        // produced output (`produced`), so the check compares like-for-like with the backend whose
        // output the wave's expectations target.
        let invariance_failures = if checks.invariance {
            self.run_invariance_check(wave, netlist, width, mode, produced)
        } else {
            Vec::new()
        };

        // Causality property check (Req 7.7, design Property 3): in event mode, verify no output
        // fires before the earliest input that can influence it. We run the event backend here
        // regardless of `mode` so the check is well-defined even under Mode::Functional (the task
        // specifies "in EVENT mode").
        let causality_failures = if checks.causality {
            self.run_causality_check(wave, netlist, width)
        } else {
            Vec::new()
        };

        WaveResult {
            index,
            mismatches,
            cross_check_mismatches,
            invariance_failures,
            causality_failures,
        }
    }

    /// Run the invariance property check for one wave (Req 7.6, design Property 4).
    ///
    /// Picks a finite constant `c` deterministically (see [`choose_shift`]), adds it to every
    /// finite input element (leaving `inf` inputs unchanged), re-runs the active backend on the
    /// shifted inputs, and compares each output against the baseline via [`check_invariance`].
    ///
    /// `c` is chosen small enough that a legal program's finite outputs can shift by `c` without
    /// saturating to `inf`: the bound keeps `max_finite_input + c` strictly below the width's
    /// finite maximum (see [`choose_shift`]). Because every primitive is shift-equivariant
    /// (`min`/`lt`/`inc` all commute with adding a constant to their finite operands) a legal
    /// program's finite outputs then shift by exactly `c` and `inf` outputs stay `inf`.
    fn run_invariance_check(
        &self,
        wave: &Wave,
        netlist: &NetlistIr,
        width: TimeWidth,
        mode: Mode,
        baseline: &HashMap<String, SpikeTime>,
    ) -> Vec<InvarianceFailure> {
        // The per-element baseline input ticks (node id -> tick), defaulting omitted inputs to inf.
        let base_input_values = self.resolve_inputs(wave);

        // Largest finite input tick in this wave, used to bound `c` away from saturation.
        let max_finite_input = base_input_values
            .values()
            .copied()
            .filter(|&t| t != INF)
            .max()
            .unwrap_or(0);
        let c = choose_shift(wave, self.max_finite, max_finite_input);
        if c == 0 {
            // No headroom to shift (every legal program trivially satisfies a zero shift); skip.
            return Vec::new();
        }

        // Re-run the active backend with every finite input shifted by `c`.
        let shifted: HashMap<String, SpikeTime> = match mode {
            Mode::Event => {
                let mut event_inputs = self.event_input_map(wave);
                for v in event_inputs.values_mut() {
                    *v = shift_finite(*v, c, self.max_finite);
                }
                sim_event::run(netlist, &event_inputs, width, sim_event::DEFAULT_MAX_TICK).outputs
            }
            Mode::Functional | Mode::Both => {
                let shifted_values: HashMap<NodeId, SpikeTime> = base_input_values
                    .iter()
                    .map(|(&id, &t)| (id, shift_finite(t, c, self.max_finite)))
                    .collect();
                let values = self.evaluate(&shifted_values);
                self.output_wire_values(&values)
            }
        };

        check_invariance(baseline, &shifted, c)
    }

    /// Run the causality property check for one wave (Req 7.7, design Property 3).
    ///
    /// Runs the event backend, computes the earliest influencing input tick per output via graph
    /// reachability on the IR ([`influencing_input_ticks`]), and compares via [`check_causality`].
    fn run_causality_check(
        &self,
        wave: &Wave,
        netlist: &NetlistIr,
        width: TimeWidth,
    ) -> Vec<CausalityFailure> {
        let event_inputs = self.event_input_map(wave);
        let run = sim_event::run(netlist, &event_inputs, width, sim_event::DEFAULT_MAX_TICK);

        // Earliest influencing input tick per output wire (via reachability), and output ticks.
        let earliest = self.influencing_input_ticks(&event_inputs);
        check_causality(&earliest, &run.outputs)
    }

    /// For each output wire (`y` / `y[i]`), the tick of the earliest input that can *influence* it,
    /// computed from graph reachability on the IR: an input influences an output when the input's
    /// node can reach the output's node along directed edges (Req 7.7, design Property 3).
    ///
    /// `input_ticks` maps input wire names to their spike ticks for this wave. The returned map
    /// gives each output wire the minimum tick over its influencing inputs, or [`INF`] when no
    /// finite influencing input exists (an output driven only by `inf` sources, or by nothing).
    fn influencing_input_ticks(
        &self,
        input_ticks: &HashMap<String, SpikeTime>,
    ) -> HashMap<String, SpikeTime> {
        // Map each (non-inf) input node id to the wire name used in `input_ticks`.
        let mut input_wire_of: HashMap<NodeId, String> = HashMap::new();
        for (base, group) in &self.inputs {
            for &(idx, id) in &group.elems {
                let wire = match group.width {
                    None => base.clone(),
                    Some(_) => format!("{base}[{idx}]"),
                };
                input_wire_of.insert(id, wire);
            }
        }

        // Reachability: which input nodes can reach each output node. We compute, per output node,
        // the set of input nodes that reach it by a backward walk over incoming edges.
        let mut result = HashMap::new();
        for (base, group) in &self.outputs {
            for &(idx, out_id) in &group.elems {
                let wire = match group.width {
                    None => base.clone(),
                    Some(_) => format!("{base}[{idx}]"),
                };
                let sources = reachable_input_sources(self.ir, out_id);
                let earliest = sources
                    .iter()
                    .filter_map(|id| input_wire_of.get(id))
                    .map(|w| input_ticks.get(w).copied().unwrap_or(INF))
                    .min()
                    .unwrap_or(INF);
                result.insert(wire, earliest);
            }
        }
        result
    }

    /// Project a node-id -> tick map onto a wire-name -> tick map over the output ports (`y` or
    /// `y[i]`), so Mode A's result lines up with Mode B's [`sim_event::EventRun::outputs`] keys.
    fn output_wire_values(&self, values: &HashMap<NodeId, SpikeTime>) -> HashMap<String, SpikeTime> {
        let mut out = HashMap::new();
        for (base, group) in &self.outputs {
            for &(idx, id) in &group.elems {
                let wire = match group.width {
                    None => base.clone(),
                    Some(_) => format!("{base}[{idx}]"),
                };
                out.insert(wire, values.get(&id).copied().unwrap_or(INF));
            }
        }
        out
    }

    /// Build the wire-keyed input map [`sim_event::run`] expects from this wave, applying the same
    /// defaulting rules as [`Simulator::resolve_inputs`]: an omitted input is `inf`, a vector reads
    /// element-wise with missing elements `inf`, and keys are `a` for scalars / `a[i]` for vectors
    /// (Req 7.4, 7.8). Reusing the same port grouping guarantees Mode A and Mode B see identical
    /// wire keys (so their outputs can be cross-checked directly, Req 6.10).
    fn event_input_map(&self, wave: &Wave) -> HashMap<String, SpikeTime> {
        let supplied: HashMap<&str, &StimVal> = wave
            .inputs
            .iter()
            .map(|(name, val)| (name.as_str(), val))
            .collect();

        let mut map = HashMap::new();
        for (base, group) in &self.inputs {
            let stim = supplied.get(base.as_str()).copied();
            for &(idx, _id) in &group.elems {
                let (wire, v) = match (group.width, stim) {
                    (None, None) => (base.clone(), INF),
                    (None, Some(s)) => (base.clone(), stim_scalar(s)),
                    (Some(_), None) => (format!("{base}[{idx}]"), INF),
                    (Some(_), Some(s)) => (format!("{base}[{idx}]"), stim_vec_elem(s, idx)),
                };
                map.insert(wire, v);
            }
        }
        map
    }

    /// Map each input-port element node id to its stimulus value, defaulting omitted inputs to
    /// `inf` (Req 7.4). Scalar inputs read a scalar stimulus; vector inputs read a vector stimulus
    /// element-wise, defaulting any missing element to `inf` as well.
    fn resolve_inputs(&self, wave: &Wave) -> HashMap<NodeId, SpikeTime> {
        let supplied: HashMap<&str, &StimVal> = wave
            .inputs
            .iter()
            .map(|(name, val)| (name.as_str(), val))
            .collect();

        let mut values = HashMap::new();
        for (base, group) in &self.inputs {
            let stim = supplied.get(base.as_str()).copied();
            for &(idx, id) in &group.elems {
                let v = match (group.width, stim) {
                    // Omitted entirely -> inf (Req 7.4).
                    (_, None) => INF,
                    // Scalar input: read the scalar stimulus directly.
                    (None, Some(s)) => stim_scalar(s),
                    // Vector input: read element `idx`, defaulting a missing element to inf.
                    (Some(_), Some(s)) => stim_vec_elem(s, idx),
                };
                values.insert(id, v);
            }
        }
        values
    }

    /// Map each output wire name (`y` or `y[i]`) to its expected value from the wave (Req 7.1, 7.8).
    fn resolve_expects(
        &self,
        wave: &Wave,
        defaults: &HashMap<String, SpikeTime>,
    ) -> HashMap<String, SpikeTime> {
        let supplied: HashMap<&str, &StimVal> = wave
            .expects
            .iter()
            .map(|(name, val)| (name.as_str(), val))
            .collect();

        let mut expected = HashMap::new();
        for (base, group) in &self.outputs {
            let stim = supplied.get(base.as_str()).copied();
            for &(idx, _id) in &group.elems {
                let wire = match group.width {
                    None => base.clone(),
                    Some(_) => format!("{base}[{idx}]"),
                };
                if let Some(s) = stim {
                    // Explicitly expected: always checked (Req 7.1).
                    let v = match group.width {
                        None => stim_scalar(s),
                        Some(_) => stim_vec_elem(s, idx),
                    };
                    expected.insert(wire, v);
                } else if wave.expect_default {
                    // `expect default;` — check this unmentioned output against the test default
                    // (if named) or `inf`. Without `expect default`, an unmentioned output is NOT
                    // checked (it is omitted from the map entirely).
                    let v = defaults.get(&wire).copied().unwrap_or(INF);
                    expected.insert(wire, v);
                }
            }
        }
        expected
    }

    /// Resolve a test's `default { .. }` bindings into output-wire -> value. A scalar binding
    /// `y = v` maps `y`; a vector binding `v = [..]` maps `v[i]` element-wise (missing elements and
    /// unlisted outputs are left out, so `resolve_expects` falls back to `inf` for them).
    fn resolve_defaults(&self, bindings: &[(String, StimVal)]) -> HashMap<String, SpikeTime> {
        let supplied: HashMap<&str, &StimVal> =
            bindings.iter().map(|(n, v)| (n.as_str(), v)).collect();
        let mut map = HashMap::new();
        for (base, group) in &self.outputs {
            if let Some(s) = supplied.get(base.as_str()).copied() {
                for &(idx, _id) in &group.elems {
                    match group.width {
                        None => {
                            map.insert(base.clone(), stim_scalar(s));
                        }
                        Some(_) => {
                            map.insert(format!("{base}[{idx}]"), stim_vec_elem(s, idx));
                        }
                    }
                }
            }
        }
        map
    }

    /// Resolve the node id producing output wire `wire` (`y` or `y[i]`). Used by the test-only
    /// `compiled_op` helper; the production path reads outputs via [`Simulator::output_wire_values`].
    #[cfg(test)]
    fn output_node(&self, wire: &str) -> Option<NodeId> {
        let (base, idx) = split_indexed(wire);
        let group = self.outputs.get(&base)?;
        let want = idx.unwrap_or(0);
        group
            .elems
            .iter()
            .find(|&&(i, _)| i == want)
            .map(|&(_, id)| id)
    }

    /// Evaluate every node in topological order, returning each node id's spike time.
    fn evaluate(&self, input_values: &HashMap<NodeId, SpikeTime>) -> HashMap<NodeId, SpikeTime> {
        let mut values: HashMap<NodeId, SpikeTime> = HashMap::new();
        for &id in &self.order {
            let node = match self.ir.node(id) {
                Some(n) => n,
                None => continue,
            };
            let value = match &node.kind {
                NodeKind::Input => {
                    if is_inf_constant(self.ir, id) {
                        INF
                    } else {
                        input_values.get(&id).copied().unwrap_or(INF)
                    }
                }
                NodeKind::Min => {
                    let mut acc = INF;
                    for src in self.operands(id) {
                        acc = st_min(acc, values.get(&src).copied().unwrap_or(INF));
                    }
                    acc
                }
                NodeKind::Lt => {
                    let pass = self
                        .input_with_role(id, &PortRole::Pass)
                        .and_then(|s| values.get(&s).copied())
                        .unwrap_or(INF);
                    let inhibit = self
                        .input_with_role(id, &PortRole::Inhibit)
                        .and_then(|s| values.get(&s).copied())
                        .unwrap_or(INF);
                    st_lt(pass, inhibit)
                }
                NodeKind::Inc { amount } => {
                    let a = self
                        .operands(id)
                        .first()
                        .and_then(|&s| values.get(&s).copied())
                        .unwrap_or(INF);
                    st_inc(a, *amount as u64, self.max_finite)
                }
                NodeKind::Output => {
                    // An output takes its single driver's value, whatever the edge role (a plain
                    // operand driver, or an instance-port edge when an instance drives it).
                    self.ir
                        .incoming(id)
                        .next()
                        .and_then(|e| values.get(&e.from).copied())
                        .unwrap_or(INF)
                }
                NodeKind::Instance { .. } => {
                    // Opaque instance boundary: a transparent pass-through of its first named input
                    // (see module docs). Its outputs forward this value.
                    self.ir
                        .incoming(id)
                        .find(|e| matches!(e.port_role, PortRole::InstancePort(_)))
                        .and_then(|e| values.get(&e.from).copied())
                        .unwrap_or(INF)
                }
            };
            values.insert(id, value);
        }
        values
    }

    /// Operand producer ids feeding `consumer`, in edge order.
    fn operands(&self, consumer: NodeId) -> Vec<NodeId> {
        self.ir
            .incoming(consumer)
            .filter(|e| e.port_role == PortRole::Operand)
            .map(|e| e.from)
            .collect()
    }

    /// The producer feeding `consumer` on an edge with the given role.
    fn input_with_role(&self, consumer: NodeId, role: &PortRole) -> Option<NodeId> {
        self.ir
            .incoming(consumer)
            .find(|e| &e.port_role == role)
            .map(|e| e.from)
    }
}

/// Insert a boundary node into a grouped-port map keyed by base name, recording vector width.
fn group_insert(groups: &mut HashMap<String, PortGroup>, name: &str, id: NodeId) {
    let (base, idx) = split_indexed(name);
    let group = groups.entry(base).or_default();
    match idx {
        None => group.elems.push((0, id)),
        Some(i) => {
            group.elems.push((i, id));
            group.width = Some(group.width.unwrap_or(0).max(i + 1));
        }
    }
    group.elems.sort_by_key(|&(i, _)| i);
}

/// Whether an `Input` node is the `inf` constant source: an `Input`-kind node named exactly
/// `"inf"` with no incoming edges, exactly as [`crate::ir`] emits it (mirrors the C++ backend).
fn is_inf_constant(ir: &NetlistIr, id: NodeId) -> bool {
    match ir.node(id) {
        Some(n) => {
            matches!(n.kind, NodeKind::Input) && n.name == "inf" && ir.incoming(id).next().is_none()
        }
        None => false,
    }
}

/// Split a node/wire name into `(base, Some(index))` for `name[i]`, or `(name, None)` for a scalar.
fn split_indexed(name: &str) -> (String, Option<u64>) {
    if let Some(open) = name.rfind('[') {
        if name.ends_with(']') {
            let idx_str = &name[open + 1..name.len() - 1];
            if let Ok(i) = idx_str.parse::<u64>() {
                return (name[..open].to_string(), Some(i));
            }
        }
    }
    (name.to_string(), None)
}

/// Interpret a scalar stimulus value: a concrete time, or `inf` (Req 7.8). A vector supplied for a
/// scalar port is a design error caught earlier; defensively take its first element or `inf`.
fn stim_scalar(stim: &StimVal) -> SpikeTime {
    match &stim.kind {
        StimValKind::Time(t) => *t,
        StimValKind::Inf => INF,
        StimValKind::Vec(vs) => vs.first().map(stim_scalar).unwrap_or(INF),
    }
}

/// Interpret element `idx` of a vector stimulus like `[3, inf, 5]`, defaulting a missing element to
/// `inf` (Req 7.8, 7.4). A scalar supplied for a vector port broadcasts to each element.
fn stim_vec_elem(stim: &StimVal, idx: u64) -> SpikeTime {
    match &stim.kind {
        StimValKind::Vec(vs) => vs.get(idx as usize).map(stim_scalar).unwrap_or(INF),
        // A scalar/inf supplied for a vector port applies to the element.
        StimValKind::Time(t) => *t,
        StimValKind::Inf => INF,
    }
}

/// Topological order of node ids via Kahn's algorithm, breaking ties by ascending id for
/// determinism. Returns `None` on a cyclic graph (no complete order).
fn topo_order(ir: &NetlistIr) -> Option<Vec<NodeId>> {
    let mut indeg: HashMap<NodeId, usize> = ir.nodes.iter().map(|n| (n.id, 0usize)).collect();
    for e in &ir.edges {
        if let Some(d) = indeg.get_mut(&e.to) {
            *d += 1;
        }
    }
    let mut ready: Vec<NodeId> = indeg
        .iter()
        .filter(|(_, &d)| d == 0)
        .map(|(&id, _)| id)
        .collect();
    ready.sort_unstable();

    let mut order: Vec<NodeId> = Vec::with_capacity(ir.nodes.len());
    while let Some(&next) = ready.first() {
        ready.remove(0);
        order.push(next);
        let mut newly_ready: Vec<NodeId> = Vec::new();
        for e in ir.outgoing(next) {
            if let Some(d) = indeg.get_mut(&e.to) {
                *d -= 1;
                if *d == 0 {
                    newly_ready.push(e.to);
                }
            }
        }
        for id in newly_ready {
            let pos = ready.binary_search(&id).unwrap_or_else(|p| p);
            ready.insert(pos, id);
        }
    }

    if order.len() == ir.nodes.len() {
        Some(order)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::TimeWidth;
    use crate::operators::OperatorTable;
    use crate::parser::parse_source;

    /// Parse a source string into a validated [`Design`], panicking on parse errors so fixtures
    /// start from a well-formed program (sema runs inside `run_tests`).
    fn parse(src: &str) -> Design {
        parse_source(src, "test.sta", &OperatorTable::builtin(), TimeWidth::W32).unwrap_or_else(
            |d| {
                let r: Vec<String> = d.iter().map(|x| x.render()).collect();
                panic!("fixture failed to parse:\n{}", r.join("\n"));
            },
        )
    }

    /// A `Sort2` module and a test exercising it: `lo = min(a, b)`, `hi = max(a, b)`.
    const SORT2: &str = "\
module Sort2 (input time a, input time b, output time lo, output time hi) {
    lo = min(a, b);
    hi = max(a, b);
}
test t_sort2 for Sort2 {
    wave { a = 3; b = 5; expect lo = 3; expect hi = 5; }
}
";

    // --- a passing wave (Req 7.1, 7.2) ------------------------------------------------------

    #[test]
    fn passing_wave_reports_pass() {
        let design = parse(SORT2);
        let report = run_tests(&design, TimeWidth::W32);
        assert!(!report.any_failed(), "all waves pass:\n{}", report.render());
        assert_eq!(report.total_passed_waves(), 1);
        assert_eq!(report.total_failed_waves(), 0);
        let wave = &report.tests[0].waves[0];
        assert!(wave.passed());
        assert_eq!(wave.index, 1);
    }

    // --- a failing wave names the differing wire and the wave number (Req 7.2, 7.3) ---------

    #[test]
    fn failing_wave_names_wire_and_wave_number() {
        // Expect the wrong value for `hi`: the sim produces 5 but we expect 9.
        let src = "\
module Sort2 (input time a, input time b, output time lo, output time hi) {
    lo = min(a, b);
    hi = max(a, b);
}
test t_sort2 for Sort2 {
    wave { a = 3; b = 5; expect lo = 3; expect hi = 9; }
}
";
        let design = parse(src);
        let report = run_tests(&design, TimeWidth::W32);
        assert!(report.any_failed(), "a mismatch must fail the report (Req 7.3)");

        let wave = &report.tests[0].waves[0];
        assert!(!wave.passed());
        assert_eq!(wave.index, 1, "the FAIL names the wave number (Req 7.2)");
        assert_eq!(wave.mismatches.len(), 1, "only `hi` differs");
        let m = &wave.mismatches[0];
        assert_eq!(m.wire, "hi", "the FAIL names the differing wire (Req 7.2)");
        assert_eq!(m.produced, 5);
        assert_eq!(m.expected, 9);

        // The rendered line mentions FAIL, the wave number, and the wire.
        let rendered = report.render();
        assert!(rendered.contains("FAIL"), "renders FAIL:\n{rendered}");
        assert!(rendered.contains("wave 1"), "names the wave number:\n{rendered}");
        assert!(rendered.contains("hi"), "names the differing wire:\n{rendered}");
    }

    // --- omitted input defaults to inf (Req 7.4) --------------------------------------------

    #[test]
    fn omitted_input_defaults_to_inf() {
        // `b` is omitted, so it defaults to inf. lo = min(a, inf) = a = 3; hi = max(a, inf) = inf.
        let src = "\
module Sort2 (input time a, input time b, output time lo, output time hi) {
    lo = min(a, b);
    hi = max(a, b);
}
test t_sort2 for Sort2 {
    wave { a = 3; expect lo = 3; expect hi = inf; }
}
";
        let design = parse(src);
        let report = run_tests(&design, TimeWidth::W32);
        assert!(
            !report.any_failed(),
            "omitted `b` defaults to inf so this passes (Req 7.4):\n{}",
            report.render()
        );
    }

    // --- inf == inf equality, and inf != finite (Req 7.1) -----------------------------------

    #[test]
    fn inf_equals_only_inf() {
        // With both inputs inf, both outputs are inf; expecting inf passes.
        let src = "\
module Sort2 (input time a, input time b, output time lo, output time hi) {
    lo = min(a, b);
    hi = max(a, b);
}
test t_pass for Sort2 {
    wave { a = inf; b = inf; expect lo = inf; expect hi = inf; }
}
";
        let report = run_tests(&parse(src), TimeWidth::W32);
        assert!(!report.any_failed(), "inf == inf (Req 7.1):\n{}", report.render());

        // Expecting a finite value where the sim produces inf must fail (inf != finite).
        let src_fail = "\
module Sort2 (input time a, input time b, output time lo, output time hi) {
    lo = min(a, b);
    hi = max(a, b);
}
test t_fail for Sort2 {
    wave { a = inf; b = inf; expect lo = 0; expect hi = inf; }
}
";
        let report = run_tests(&parse(src_fail), TimeWidth::W32);
        assert!(report.any_failed(), "inf != finite (Req 7.1)");
        let m = &report.tests[0].waves[0].mismatches;
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].wire, "lo");
        assert_eq!(m[0].produced, INF);
        assert_eq!(m[0].expected, 0);
    }

    // --- vector stimulus and expected, including `[3, inf, 5]` (Req 7.8) --------------------

    #[test]
    fn vector_stimulus_and_expected() {
        // A width-3 pass-through: y[i] = a[i]. Drive a vector literal with an inf element and
        // expect it back exactly.
        let src = "\
module Pass3 (input time a[3], output time y[3]) {
    y[0] = a[0];
    y[1] = a[1];
    y[2] = a[2];
}
test t_pass3 for Pass3 {
    wave { a = [3, inf, 5]; expect y = [3, inf, 5]; }
}
";
        let design = parse(src);
        let report = run_tests(&design, TimeWidth::W32);
        assert!(
            !report.any_failed(),
            "vector literal stimulus/expected round-trips (Req 7.8):\n{}",
            report.render()
        );

        // A wrong vector element fails and names the specific indexed wire.
        let src_fail = "\
module Pass3 (input time a[3], output time y[3]) {
    y[0] = a[0];
    y[1] = a[1];
    y[2] = a[2];
}
test t_pass3 for Pass3 {
    wave { a = [3, inf, 5]; expect y = [3, inf, 9]; }
}
";
        let report = run_tests(&parse(src_fail), TimeWidth::W32);
        assert!(report.any_failed(), "a differing vector element fails (Req 7.8)");
        let m = &report.tests[0].waves[0].mismatches;
        assert_eq!(m.len(), 1, "only y[2] differs");
        assert_eq!(m[0].wire, "y[2]", "names the differing indexed wire (Req 7.2)");
        assert_eq!(m[0].produced, 5);
        assert_eq!(m[0].expected, 9);
    }

    // --- multiple waves: pass/fail counts aggregate for the CLI exit code (Req 7.3) ---------

    #[test]
    fn multiple_waves_aggregate_pass_fail_counts() {
        let src = "\
module Sort2 (input time a, input time b, output time lo, output time hi) {
    lo = min(a, b);
    hi = max(a, b);
}
test t_sort2 for Sort2 {
    wave { a = 3; b = 5; expect lo = 3; expect hi = 5; }
    wave { a = 7; b = 2; expect lo = 2; expect hi = 7; }
    wave { a = 1; b = 4; expect lo = 9; expect hi = 4; }
}
";
        let report = run_tests(&parse(src), TimeWidth::W32);
        assert!(report.any_failed(), "the third wave fails (Req 7.3)");
        assert_eq!(report.total_passed_waves(), 2);
        assert_eq!(report.total_failed_waves(), 1);

        let waves = &report.tests[0].waves;
        assert!(waves[0].passed() && waves[1].passed());
        assert!(!waves[2].passed());
        assert_eq!(waves[2].index, 3, "the failing wave is wave 3 (Req 7.2)");
    }

    // --- new test-expectation semantics: unmentioned=ignored, expect default, cases ---------

    /// A module with two outputs so we can leave one unmentioned.
    const TWO_OUT: &str = "\
module Two (input time a, input time b, output time lo, output time hi) {
    lo = min(a, b);
    hi = max(a, b);
}
";

    #[test]
    fn unmentioned_output_is_ignored_without_expect_default() {
        // The wave only expects `lo`; `hi` is unmentioned and non-inf, but must NOT fail now.
        let src = format!(
            "{TWO_OUT}test t for Two {{ wave {{ a = 3; b = 5; expect lo = 3; }} }}"
        );
        let report = run_tests(&parse(&src), TimeWidth::W32);
        assert!(
            !report.any_failed(),
            "an unmentioned output must be ignored (not checked against inf):\n{}",
            report.render()
        );
    }

    #[test]
    fn expect_default_checks_unmentioned_against_inf() {
        // With `expect default;` the unmentioned `hi` is checked against inf. Here hi = max(3,5) = 5,
        // so it must FAIL.
        let src = format!(
            "{TWO_OUT}test t for Two {{ wave {{ a = 3; b = 5; expect lo = 3; expect default; }} }}"
        );
        let report = run_tests(&parse(&src), TimeWidth::W32);
        assert!(
            report.any_failed(),
            "expect default must check the unmentioned hi against inf and fail"
        );
        let m = &report.tests[0].waves[0].mismatches;
        assert!(m.iter().any(|x| x.wire == "hi" && x.expected == INF && x.produced == 5));
    }

    #[test]
    fn expect_default_passes_when_unmentioned_output_is_inf() {
        // Both inputs inf -> hi = inf, so `expect default` passes with no explicit hi expectation.
        let src = format!(
            "{TWO_OUT}test t for Two {{ wave {{ a = inf; b = inf; expect lo = inf; expect default; }} }}"
        );
        let report = run_tests(&parse(&src), TimeWidth::W32);
        assert!(!report.any_failed(), "inf unmentioned output satisfies expect default:\n{}", report.render());
    }

    #[test]
    fn test_default_block_supplies_the_expect_default_value() {
        // default { hi = 5; } makes `expect default` check hi against 5 (which is what max(3,5) is),
        // so the wave passes without naming hi.
        let src = format!(
            "{TWO_OUT}test t for Two {{ default {{ hi = 5; }} wave {{ a = 3; b = 5; expect lo = 3; expect default; }} }}"
        );
        let report = run_tests(&parse(&src), TimeWidth::W32);
        assert!(
            !report.any_failed(),
            "a per-test default of 5 for hi should let expect default pass:\n{}",
            report.render()
        );

        // And a WRONG default (hi = 9) must fail against the produced 5.
        let bad = format!(
            "{TWO_OUT}test t for Two {{ default {{ hi = 9; }} wave {{ a = 3; b = 5; expect lo = 3; expect default; }} }}"
        );
        let report2 = run_tests(&parse(&bad), TimeWidth::W32);
        assert!(report2.any_failed(), "a default of 9 for hi must mismatch the produced 5");
    }

    #[test]
    fn cases_block_runs_each_column_as_a_wave() {
        let src = format!(
            "{TWO_OUT}cases c for Two {{ a = [3, 5, 2]; b = [5, 3, 2]; expect lo = [3, 3, 2]; }}"
        );
        let report = run_tests(&parse(&src), TimeWidth::W32);
        assert!(!report.any_failed(), "all three cases should pass:\n{}", report.render());
        assert_eq!(report.total_passed_waves(), 3, "three columns -> three waves");
    }

    #[test]
    fn cases_block_reports_the_failing_column() {
        // Column 2 expects lo = 9 but min(2,2) = 2, so exactly one wave fails.
        let src = format!(
            "{TWO_OUT}cases c for Two {{ a = [3, 5, 2]; b = [5, 3, 2]; expect lo = [3, 3, 9]; }}"
        );
        let report = run_tests(&parse(&src), TimeWidth::W32);
        assert!(report.any_failed());
        assert_eq!(report.total_failed_waves(), 1);
        assert_eq!(report.total_passed_waves(), 2);
    }

    // --- instance flattening: hierarchical designs simulate like their flat twins -----------

    #[test]
    fn instance_based_module_simulates_through_the_boundary() {
        // A Top that instantiates a Leaf compare-exchange must now compute correctly end to end
        // (instances are flattened into the graph, no longer opaque pass-throughs).
        let src = "\
module Leaf (input time a, input time b, output time lo, output time hi) {
    lo = min(a, b);
    hi = max(a, b);
}
module Top (input time a, input time b, output time lo, output time hi) {
    inst c = Leaf (.a(a), .b(b), .lo(lo), .hi(hi));
}
test t for Top {
    wave { a = 5; b = 2; expect lo = 2; expect hi = 5; }
    wave { a = 3; b = 3; expect lo = 3; expect hi = 3; }
    wave { a = 4; b = inf; expect lo = 4; expect hi = inf; }
}
";
        let report = run_tests_mode(&parse(src), TimeWidth::W32, Mode::Both);
        assert!(
            !report.any_failed(),
            "an instance-based module must simulate correctly under both modes:\n{}",
            report.render()
        );
    }

    #[test]
    fn two_stage_instance_chain_simulates() {
        // t = a+1 via the first instance, then y = t+1 via the second: overall a+2.
        let src = "\
module D1 (input time a, output time y) { y = a + 1; }
module Top (input time a, output time y) {
    wire time t;
    inst d0 = D1 (.a(a), .y(t));
    inst d1 = D1 (.a(t), .y(y));
}
test t for Top {
    wave { a = 3; expect y = 5; }
    wave { a = inf; expect y = inf; }
}
";
        let report = run_tests_mode(&parse(src), TimeWidth::W32, Mode::Both);
        assert!(!report.any_failed(), "a chained instance pipeline must simulate:\n{}", report.render());
    }

    // --- variadic max (mirrors variadic min) -----------------------------------------------

    #[test]
    fn max_is_variadic_like_min() {
        let src = "\
module M (input time a, input time b, input time c, output time hi, output time lo) {
    hi = max(a, b, c);
    lo = min(a, b, c);
}
test t for M {
    wave { a = 5; b = 1; c = 8; expect hi = 8; expect lo = 1; }
    wave { a = 3; b = 3; c = 3; expect hi = 3; expect lo = 3; }   // all tie
    wave { a = 2; b = inf; c = 7; expect hi = inf; expect lo = 2; } // inf is the latest
    wave { a = inf; b = inf; c = inf; expect hi = inf; expect lo = inf; }
}
";
        let report = run_tests(&parse(src), TimeWidth::W32);
        assert!(!report.any_failed(), "max(a,b,c) must be the latest of three:\n{}", report.render());
    }

    #[test]
    fn max_with_four_arguments_folds_correctly() {
        let src = "\
module M (input time a, input time b, input time c, input time d, output time y) {
    y = max(a, b, c, d);
}
test t for M {
    wave { a = 1; b = 9; c = 4; d = 7; expect y = 9; }
    wave { a = 2; b = 2; c = 2; d = 5; expect y = 5; }
}
";
        let report = run_tests(&parse(src), TimeWidth::W32);
        assert!(!report.any_failed(), "4-arg max should fold to the overall latest:\n{}", report.render());
    }

    // --- new operators: ^ (min alias), emin, emax ------------------------------------------

    #[test]
    fn caret_is_an_alias_for_two_arg_min() {
        let src = "\
module M (input time a, input time b, output time y) { y = a ^ b; }
test t for M {
    wave { a = 3; b = 5; expect y = 3; }
    wave { a = 5; b = 3; expect y = 3; }
    wave { a = 4; b = 4; expect y = 4; }
    wave { a = inf; b = 2; expect y = 2; }
}
";
        let report = run_tests(&parse(src), TimeWidth::W32);
        assert!(!report.any_failed(), "`^` should behave like min:\n{}", report.render());
    }

    #[test]
    fn emin_is_min_except_a_tie_blocks() {
        // emin lives in std.sta; the test harness does not auto-import it, so inline the same
        // definition here to keep the design self-contained.
        let src = "\
func emin(time a, time b) -> time { return min(a < b, b < a); }
module M (input time a, input time b, output time y) { y = emin(a, b); }
test t for M {
    wave { a = 3; b = 5; expect y = 3; }     // earlier passes
    wave { a = 5; b = 3; expect y = 3; }
    wave { a = 4; b = 4; expect y = inf; }   // tie -> inf
    wave { a = 2; b = inf; expect y = 2; }
}
";
        let report = run_tests(&parse(src), TimeWidth::W32);
        assert!(!report.any_failed(), "emin: earliest unless tie:\n{}", report.render());
    }

    #[test]
    fn emax_is_max_except_a_tie_blocks() {
        let src = "\
func emax(time a, time b) -> time { return min(a > b, b > a); }
module M (input time a, input time b, output time y) { y = emax(a, b); }
test t for M {
    wave { a = 3; b = 5; expect y = 5; }     // later passes
    wave { a = 5; b = 3; expect y = 5; }
    wave { a = 4; b = 4; expect y = inf; }   // tie -> inf
    wave { a = 2; b = inf; expect y = inf; } // b never fires: emax = max(2>inf, inf>2) = max(inf,inf) = inf
}
";
        let report = run_tests(&parse(src), TimeWidth::W32);
        assert!(!report.any_failed(), "emax: latest unless tie:\n{}", report.render());
    }

    #[test]
    fn le_ge_infix_match_their_funcs() {
        let src = "\
module M (input time a, input time b, output time l, output time g) { l = a <= b; g = a >= b; }
test t for M {
    wave { a = 3; b = 5; expect l = 3; expect g = inf; }
    wave { a = 5; b = 3; expect l = inf; expect g = 5; }
    wave { a = 4; b = 4; expect l = 4; expect g = 4; }
}
";
        let report = run_tests(&parse(src), TimeWidth::W32);
        assert!(!report.any_failed(), "<= and >= :\n{}", report.render());
    }
    // --- saturating inc semantics match quack_rt.hpp ----------------------------------------

    #[test]
    fn inc_saturates_and_inf_stays_inf() {
        assert_eq!(st_inc(INF, 5, u32::MAX as u64), INF, "inf + n = inf");
        assert_eq!(st_inc(3, 4, u32::MAX as u64), 7, "finite add");
        // Reaching the width maximum clamps to inf.
        assert_eq!(st_inc(u32::MAX as u64 - 1, 1, u32::MAX as u64), INF, "clamp at width max");
        assert_eq!(st_lt(3, 5), 3, "a < b passes a");
        assert_eq!(st_lt(5, 5), INF, "a tie blocks");
        assert_eq!(st_min(4, INF), 4, "finite beats inf");
    }

    // --- Task 15: exhaustive derived-operator identity tests (Req 7.9, design Property 2) ----
    //
    // For every ordered pair (a, b) over `{0, 1, 2, 3, 4, 5, 6, inf}`, each derived operator
    // `le`, `ge`, `eq`, `max`, `gt`, `neq` is evaluated *as elaborated through the compiler* — a
    // one-line Quack module using the operator is run through
    // `parse_source -> sema::analyze -> elaborate -> ir::build -> Simulator` (the Mode A
    // interpreter), so the test genuinely exercises elaboration's expansion of the derived
    // operator into the `inc`/`min`/`lt` primitives (Req 3.5). The compiled result is compared for
    // exact equality (inf == inf only) against a direct Rust reference of each operator's if/else
    // meaning drawn from the requirements glossary and design Property 2.

    /// The operand domain for the exhaustive sweep: `{0, 1, 2, 3, 4, 5, 6, inf}`, with `inf`
    /// represented by the [`INF`] sentinel consistently throughout the test.
    const DOMAIN: [SpikeTime; 8] = [0, 1, 2, 3, 4, 5, 6, INF];

    // --- direct Rust references of each operator's if/else meaning (glossary / Property 2) ---
    //
    // Each reference takes `inf` as the `INF` sentinel. Comparisons treat `inf` as the largest
    // value, exactly as the glossary's `time` ordering does (`inf` means "never spikes").

    /// `le(a, b) = a if a <= b else inf`.
    fn ref_le(a: SpikeTime, b: SpikeTime) -> SpikeTime {
        if a <= b { a } else { INF }
    }

    /// `ge(a, b) = a if a >= b else inf`.
    fn ref_ge(a: SpikeTime, b: SpikeTime) -> SpikeTime {
        if a >= b { a } else { INF }
    }

    /// `eq(a, b) = a if a == b else inf`.
    fn ref_eq(a: SpikeTime, b: SpikeTime) -> SpikeTime {
        if a == b { a } else { INF }
    }

    /// `max(a, b)` — the larger (later) arrival, with `inf` the largest. Confirmed against
    /// elaborate.rs's expansion `max(a,b) = min(ge(a,b), ge(b,a))` and the glossary's `time`
    /// ordering: since the `INF` sentinel is `u64::MAX`, the plain integer maximum already treats
    /// `inf` as largest.
    fn ref_max(a: SpikeTime, b: SpikeTime) -> SpikeTime {
        a.max(b)
    }

    /// `gt(a, b) = a if a > b else inf`.
    fn ref_gt(a: SpikeTime, b: SpikeTime) -> SpikeTime {
        if a > b { a } else { INF }
    }

    /// `neq(a, b) = a if a != b else inf`.
    fn ref_neq(a: SpikeTime, b: SpikeTime) -> SpikeTime {
        if a != b { a } else { INF }
    }

    /// Render a [`SpikeTime`] stimulus literal: the `inf` keyword for the sentinel, else the
    /// integer. This is how `a`/`b` are written into the generated wave.
    fn stim_lit(t: SpikeTime) -> String {
        if t == INF {
            "inf".to_string()
        } else {
            t.to_string()
        }
    }

    /// Compile and simulate `y = op(a, b)` for one operand pair through the real frontend, and
    /// return the produced `y`. The generated module uses the derived operator by name so
    /// elaboration expands it into primitives; the wave supplies `a` and `b` and omits any
    /// expectation (we read the produced value directly from the report-driving interpreter).
    fn compiled_op(op: &str, a: SpikeTime, b: SpikeTime) -> SpikeTime {
        let src = format!(
            "module M (input time a, input time b, output time y) {{\n\
             \x20   y = {op}(a, b);\n\
             }}\n\
             test t for M {{\n\
             \x20   wave {{ a = {a}; b = {b}; expect y = {a}; }}\n\
             }}\n",
            op = op,
            a = stim_lit(a),
            b = stim_lit(b),
        );
        let design = parse(&src);

        // Drive the full frontend exactly as `run_tests` does, but read the produced `y` directly
        // so the reference comparison — not the embedded `expect` — is what the test asserts.
        let checked = crate::sema::analyze(&design)
            .unwrap_or_else(|e| panic!("sema failed for {op}({a:?},{b:?}): {e:?}"));
        let elab = crate::elaborate::elaborate(&checked, "M")
            .unwrap_or_else(|e| panic!("elaborate failed for {op}({a:?},{b:?}): {e:?}"));
        let netlist = ir::build(&elab);

        let sim = Simulator::new(&netlist, TimeWidth::W32);
        let wave = &design.tests[0].waves[0];
        let input_values = sim.resolve_inputs(wave);
        let values = sim.evaluate(&input_values);
        let out = sim
            .output_node("y")
            .and_then(|id| values.get(&id).copied())
            .unwrap_or(INF);
        out
    }

    /// Sweep every ordered pair in [`DOMAIN`] through `op` compiled by the frontend, asserting the
    /// compiled result equals the Rust `reference` for every pair; a mismatch names the operand
    /// pair, the compiled result, and the reference (Req 7.9).
    fn assert_identity(op: &str, reference: fn(SpikeTime, SpikeTime) -> SpikeTime) {
        let mut failures = Vec::new();
        for &a in &DOMAIN {
            for &b in &DOMAIN {
                let got = compiled_op(op, a, b);
                let want = reference(a, b);
                if got != want {
                    failures.push(format!(
                        "{op}({}, {}) = {} but reference = {}",
                        fmt_time(a),
                        fmt_time(b),
                        fmt_time(got),
                        fmt_time(want),
                    ));
                }
            }
        }
        assert!(
            failures.is_empty(),
            "derived operator `{op}` disagrees with its if/else reference for {} pair(s):\n{}",
            failures.len(),
            failures.join("\n"),
        );
    }

    #[test]
    fn derived_le_matches_if_else_reference() {
        assert_identity("le", ref_le);
    }

    #[test]
    fn derived_ge_matches_if_else_reference() {
        assert_identity("ge", ref_ge);
    }

    #[test]
    fn derived_eq_matches_if_else_reference() {
        assert_identity("eq", ref_eq);
    }

    #[test]
    fn derived_max_matches_if_else_reference() {
        assert_identity("max", ref_max);
    }

    #[test]
    fn derived_gt_matches_if_else_reference() {
        assert_identity("gt", ref_gt);
    }

    #[test]
    fn derived_neq_matches_if_else_reference() {
        assert_identity("neq", ref_neq);
    }

    // --- Task 18: Mode A / Mode B cross-check (Req 6.10, 6.11, 7.5) --------------------------

    /// A design mixing `min`, `inc`/delay, and `lt` so Mode A and Mode B exercise every primitive;
    /// both backends must agree per output wire. `lo = min(a, b)`, `d = a + 2`, `g = a < b`.
    const MIXED: &str = "\
module Mixed (input time a, input time b, output time lo, output time d, output time g) {
    lo = min(a, b);
    d = a + 2;
    g = a < b;
}
test t_mixed for Mixed {
    wave { a = 3; b = 5; expect lo = 3; expect d = 5; expect g = 3; }
    wave { a = 5; b = 2; expect lo = 2; expect d = 7; expect g = inf; }
    wave { a = inf; b = 4; expect lo = 4; expect d = inf; expect g = inf; }
}
";

    // --- Both mode: expectations pass AND the cross-check passes (modes agree) ---------------

    #[test]
    fn both_mode_passes_and_cross_check_agrees() {
        let design = parse(MIXED);
        let report = run_tests_mode(&design, TimeWidth::W32, Mode::Both);
        assert!(
            !report.any_failed(),
            "both modes agree and all expectations hold under Mode::Both (Req 6.10):\n{}",
            report.render()
        );
        // Every wave reports no cross-check disagreement.
        for wave in &report.tests[0].waves {
            assert!(
                wave.cross_check_mismatches.is_empty(),
                "wave {} should have no cross-check mismatch",
                wave.index
            );
        }
    }

    // --- Event mode: compare Mode B output against the wave's expected values (Req 7.5) ------

    #[test]
    fn event_mode_compares_mode_b_against_expected() {
        // Passing expectations under event mode.
        let report = run_tests_mode(&parse(MIXED), TimeWidth::W32, Mode::Event);
        assert!(
            !report.any_failed(),
            "event-mode output matches expectations (Req 7.5):\n{}",
            report.render()
        );

        // A wrong expectation must fail in event mode, naming the differing wire — proving the
        // comparison uses Mode B's produced value, not Mode A's.
        let src_fail = "\
module Mixed (input time a, input time b, output time lo, output time d, output time g) {
    lo = min(a, b);
    d = a + 2;
    g = a < b;
}
test t_mixed for Mixed {
    wave { a = 3; b = 5; expect lo = 3; expect d = 99; expect g = 3; }
}
";
        let report = run_tests_mode(&parse(src_fail), TimeWidth::W32, Mode::Event);
        assert!(report.any_failed(), "a wrong expectation fails in event mode (Req 7.5)");
        let m = &report.tests[0].waves[0].mismatches;
        assert_eq!(m.len(), 1, "only `d` differs");
        assert_eq!(m[0].wire, "d");
        assert_eq!(m[0].produced, 5, "Mode B produces d = a+2 = 5");
        assert_eq!(m[0].expected, 99);
        // Event mode populates no cross-check list.
        assert!(report.tests[0].waves[0].cross_check_mismatches.is_empty());
    }

    // --- Event mode handles vectors and omitted-input defaulting the same as Mode A ----------

    #[test]
    fn event_mode_vectors_and_defaulting() {
        let src = "\
module Pass3 (input time a[3], output time y[3]) {
    y[0] = a[0];
    y[1] = a[1];
    y[2] = a[2];
}
test t for Pass3 {
    wave { a = [3, inf, 5]; expect y = [3, inf, 5]; }
    wave { expect y = [inf, inf, inf]; }
}
";
        let report = run_tests_mode(&parse(src), TimeWidth::W32, Mode::Both);
        assert!(
            !report.any_failed(),
            "vector stimulus and omitted-input inf default agree across modes (Req 7.4, 7.8):\n{}",
            report.render()
        );
    }

    // --- the cross-check comparison/reporting function detects a disagreement (Req 6.11) -----
    //
    // Both engines agree for every legal program, so a real disagreement cannot be produced from a
    // compiled design. We therefore unit-test the comparison/reporting function directly with
    // synthetic Mode A vs Mode B output maps, as the task allows.

    /// Build a wire -> tick map from pairs for the synthetic cross-check tests.
    fn outs(pairs: &[(&str, SpikeTime)]) -> HashMap<String, SpikeTime> {
        pairs.iter().map(|&(w, t)| (w.to_string(), t)).collect()
    }

    #[test]
    fn cross_check_reports_only_differing_wires() {
        // y agrees (3 == 3); z differs (5 vs 6); w differs (inf vs finite).
        let a = outs(&[("y", 3), ("z", 5), ("w", INF)]);
        let b = outs(&[("y", 3), ("z", 6), ("w", 7)]);
        let mism = cross_check(&a, &b);
        assert_eq!(mism.len(), 2, "only z and w disagree");
        // Deterministic wire-name order: w before z.
        assert_eq!(mism[0].wire, "w");
        assert_eq!(mism[0].mode_a, INF);
        assert_eq!(mism[0].mode_b, 7);
        assert_eq!(mism[1].wire, "z");
        assert_eq!(mism[1].mode_a, 5);
        assert_eq!(mism[1].mode_b, 6);
    }

    #[test]
    fn cross_check_treats_inf_as_equal_only_to_inf() {
        // inf == inf agrees; inf != finite disagrees.
        assert!(cross_check(&outs(&[("y", INF)]), &outs(&[("y", INF)])).is_empty());
        let mism = cross_check(&outs(&[("y", INF)]), &outs(&[("y", 0)]));
        assert_eq!(mism.len(), 1);
        assert_eq!(mism[0].wire, "y");
        assert_eq!(mism[0].mode_a, INF);
        assert_eq!(mism[0].mode_b, 0);
    }

    #[test]
    fn cross_check_wire_present_in_only_one_mode_is_flagged() {
        // A wire missing from one map is treated as inf there, so a finite value in the other map
        // surfaces as a disagreement rather than being dropped.
        let mism = cross_check(&outs(&[("y", 4)]), &outs(&[]));
        assert_eq!(mism.len(), 1);
        assert_eq!(mism[0].wire, "y");
        assert_eq!(mism[0].mode_a, 4);
        assert_eq!(mism[0].mode_b, INF);
    }

    // --- a cross-check mismatch fails the wave distinctly and is rendered (Req 6.11) ---------
    //
    // Drive the real frontend to build a `WaveResult` whose cross-check list is populated by hand,
    // then confirm the report counts it as a failure and renders the "cross-check mismatch" detail
    // naming the wire and both modes' ticks — the surface the CLI (task 27) consumes.

    #[test]
    fn populated_cross_check_mismatch_fails_and_renders() {
        let wave = WaveResult {
            index: 2,
            mismatches: Vec::new(),
            cross_check_mismatches: vec![CrossCheckMismatch {
                wire: "y".into(),
                mode_a: 3,
                mode_b: INF,
            }],
            invariance_failures: Vec::new(),
            causality_failures: Vec::new(),
        };
        assert!(!wave.passed(), "a cross-check disagreement fails the wave (Req 6.11)");

        let report = TestReport {
            tests: vec![TestResult {
                name: "t".into(),
                module: "M".into(),
                waves: vec![wave],
                error: None,
            }],
        };
        assert!(report.any_failed(), "cross-check failure propagates to the exit-code signal (Req 7.3)");
        assert_eq!(report.total_failed_waves(), 1);

        let rendered = report.render();
        assert!(rendered.contains("FAIL"), "renders FAIL:\n{rendered}");
        assert!(rendered.contains("cross-check mismatch"), "labels the cross-check:\n{rendered}");
        assert!(rendered.contains('y'), "names the wire:\n{rendered}");
        assert!(
            rendered.contains("functional 3") && rendered.contains("event inf"),
            "names both modes' ticks:\n{rendered}"
        );
    }

    // --- functional mode (default) is unchanged: no cross-check list populated ----------------

    #[test]
    fn functional_mode_default_has_no_cross_check() {
        let report = run_tests(&parse(MIXED), TimeWidth::W32);
        assert!(!report.any_failed(), "functional default still passes:\n{}", report.render());
        for wave in &report.tests[0].waves {
            assert!(wave.cross_check_mismatches.is_empty());
        }
        // run_tests delegates to Mode::Functional.
        let via_mode = run_tests_mode(&parse(MIXED), TimeWidth::W32, Mode::Functional);
        assert_eq!(report, via_mode, "run_tests == run_tests_mode(Functional)");
    }

    // --- Task 20: invariance / causality property checks (Req 7.6, 7.7) =====================
    //
    // A legal program never violates either property (design Properties 3 and 4), so the checks
    // are run over the Section-4-style example designs and asserted to PASS. Because a correct
    // compiler cannot produce a real violation, the detection *logic* is additionally unit-tested
    // directly with fabricated output/reachability data to prove it would flag a violation.

    /// Enable both property checks.
    const BOTH_CHECKS: CheckOptions = CheckOptions {
        invariance: true,
        causality: true,
    };

    // --- Section-4-style example designs covering min, inc/delay, lt, and vectors -----------

    /// `min` and `lt` over two scalars: `lo = min(a, b)`, `g = a < b`.
    const EX_MIN_LT: &str = "\
module MinLt (input time a, input time b, output time lo, output time g) {
    lo = min(a, b);
    g = a < b;
}
test t for MinLt {
    wave { a = 3; b = 5; expect lo = 3; expect g = 3; }
    wave { a = 5; b = 2; expect lo = 2; expect g = inf; }
    wave { a = 7; b = inf; expect lo = 7; expect g = 7; }
    wave { a = inf; b = inf; expect lo = inf; expect g = inf; }
}
";

    /// `inc` / delay chain: `y = a + 3` (a delay of three ticks).
    const EX_INC_DELAY: &str = "\
module IncDelay (input time a, output time y) {
    y = a + 3;
}
test t for IncDelay {
    wave { a = 4; expect y = 7; }
    wave { a = 0; expect y = 3; }
    wave { a = inf; expect y = inf; }
}
";

    /// A width-3 vector design combining `min` and delay element-wise.
    const EX_VECTORS: &str = "\
module Vec3 (input time a[3], input time b[3], output time y[3]) {
    y[0] = min(a[0], b[0]);
    y[1] = a[1] + 2;
    y[2] = min(a[2], b[2]);
}
test t for Vec3 {
    wave { a = [3, 4, 9]; b = [5, 0, 1]; expect y = [3, 6, 1]; }
    wave { a = [inf, 2, 7]; b = [8, inf, inf]; expect y = [8, 4, 7]; }
}
";

    // --- both checks pass on every example design (Req 7.6, 7.7) ----------------------------

    #[test]
    fn invariance_and_causality_pass_on_example_designs() {
        for (label, src) in [
            ("min/lt", EX_MIN_LT),
            ("inc/delay", EX_INC_DELAY),
            ("vectors", EX_VECTORS),
            ("mixed", MIXED),
            ("sort2", SORT2),
        ] {
            // Run under every mode so the invariance check exercises both the Mode A re-run path
            // and (under Event) the Mode B re-run path, and the causality check runs regardless.
            for mode in [Mode::Functional, Mode::Event, Mode::Both] {
                let report =
                    run_tests_checked(&parse(src), TimeWidth::W32, mode, BOTH_CHECKS);
                assert!(
                    !report.any_failed(),
                    "property checks must pass for a legal program `{label}` under {mode:?}:\n{}",
                    report.render()
                );
                // No property-check failures recorded anywhere.
                for test in &report.tests {
                    for wave in &test.waves {
                        assert!(
                            wave.invariance_failures.is_empty(),
                            "`{label}` wave {} invariance failed under {mode:?}",
                            wave.index
                        );
                        assert!(
                            wave.causality_failures.is_empty(),
                            "`{label}` wave {} causality failed under {mode:?}",
                            wave.index
                        );
                    }
                }
            }
        }
    }

    // --- enabling the checks does not change expectation results (regression) ----------------

    #[test]
    fn enabling_checks_preserves_expectation_outcome() {
        let without = run_tests_mode(&parse(EX_MIN_LT), TimeWidth::W32, Mode::Functional);
        let with = run_tests_checked(&parse(EX_MIN_LT), TimeWidth::W32, Mode::Functional, BOTH_CHECKS);
        // Same pass/fail aggregate; the checked run additionally carries empty property-failure
        // lists for a legal program.
        assert_eq!(without.any_failed(), with.any_failed());
        assert_eq!(without.total_passed_waves(), with.total_passed_waves());
    }

    // --- invariance detection logic flags a fabricated violation (Req 7.6) ------------------
    //
    // A correct compiler never produces an invariance violation, so we feed `check_invariance` a
    // hand-built baseline/shifted pair and confirm each class of violation is reported.

    #[test]
    fn check_invariance_passes_when_outputs_shift_by_exactly_c() {
        // Every finite output shifted by c = 10; inf stays inf. No failures.
        let base = outs(&[("y", 3), ("z", 0), ("w", INF)]);
        let shifted = outs(&[("y", 13), ("z", 10), ("w", INF)]);
        assert!(check_invariance(&base, &shifted, 10).is_empty());
    }

    #[test]
    fn check_invariance_flags_wrong_shift_amount() {
        // `y` should become 13 but became 12 (shifted by 9, not 10).
        let base = outs(&[("y", 3)]);
        let shifted = outs(&[("y", 12)]);
        let fails = check_invariance(&base, &shifted, 10);
        assert_eq!(fails.len(), 1);
        assert_eq!(fails[0].wire, "y");
        assert_eq!(fails[0].base, 3);
        assert_eq!(fails[0].shifted, 12);
        assert_eq!(fails[0].shift, 10);
    }

    #[test]
    fn check_invariance_flags_inf_output_becoming_finite() {
        // An inf baseline output must stay inf; becoming finite is a violation.
        let base = outs(&[("y", INF)]);
        let shifted = outs(&[("y", 10)]);
        let fails = check_invariance(&base, &shifted, 10);
        assert_eq!(fails.len(), 1);
        assert_eq!(fails[0].wire, "y");
        assert_eq!(fails[0].base, INF);
        assert_eq!(fails[0].shifted, 10);
    }

    #[test]
    fn check_invariance_flags_finite_output_becoming_inf() {
        // A finite baseline output that became inf is a violation.
        let base = outs(&[("y", 3)]);
        let shifted = outs(&[("y", INF)]);
        let fails = check_invariance(&base, &shifted, 10);
        assert_eq!(fails.len(), 1);
        assert_eq!(fails[0].wire, "y");
        assert_eq!(fails[0].base, 3);
        assert_eq!(fails[0].shifted, INF);
    }

    // --- causality detection logic flags a fabricated violation (Req 7.7) -------------------

    #[test]
    fn check_causality_passes_when_output_not_earlier_than_earliest_input() {
        // Output fires at or after its earliest influencing input; inf output trivially passes.
        let earliest = outs(&[("y", 3), ("z", 5), ("w", INF)]);
        let outputs = outs(&[("y", 3), ("z", 7), ("w", INF)]);
        assert!(check_causality(&earliest, &outputs).is_empty());
    }

    #[test]
    fn check_causality_flags_output_earlier_than_its_input() {
        // `z` fires at tick 2 but its earliest influencing input is tick 5 — impossible for a
        // legal program; the check must flag it.
        let earliest = outs(&[("y", 3), ("z", 5)]);
        let outputs = outs(&[("y", 3), ("z", 2)]);
        let fails = check_causality(&earliest, &outputs);
        assert_eq!(fails.len(), 1);
        assert_eq!(fails[0].wire, "z");
        assert_eq!(fails[0].output, 2);
        assert_eq!(fails[0].earliest_input, 5);
    }

    #[test]
    fn check_causality_never_firing_output_is_trivially_causal() {
        // An inf output never violates causality even when its earliest input is finite.
        let earliest = outs(&[("y", 0)]);
        let outputs = outs(&[("y", INF)]);
        assert!(check_causality(&earliest, &outputs).is_empty());
    }

    // --- a populated property-check failure fails the wave and renders distinctly ------------

    #[test]
    fn populated_property_failures_fail_wave_and_render() {
        let wave = WaveResult {
            index: 1,
            mismatches: Vec::new(),
            cross_check_mismatches: Vec::new(),
            invariance_failures: vec![InvarianceFailure {
                wire: "y".into(),
                base: 3,
                shifted: 99,
                shift: 10,
            }],
            causality_failures: vec![CausalityFailure {
                wire: "z".into(),
                output: 2,
                earliest_input: 5,
            }],
        };
        assert!(!wave.passed(), "a property-check failure fails the wave (Req 7.6, 7.7)");

        let report = TestReport {
            tests: vec![TestResult {
                name: "t".into(),
                module: "M".into(),
                waves: vec![wave],
                error: None,
            }],
        };
        assert!(report.any_failed(), "property failure propagates to the exit-code signal (Req 7.3)");

        let rendered = report.render();
        assert!(rendered.contains("invariance violation"), "labels invariance:\n{rendered}");
        assert!(rendered.contains("causality violation"), "labels causality:\n{rendered}");
        assert!(rendered.contains('y') && rendered.contains('z'), "names both wires:\n{rendered}");
    }

    // --- reachability underpinning causality: only influencing inputs count ------------------
    //
    // Build a real IR where output `lo` depends on `a`/`b` and `g` depends on `a`/`b`, and confirm
    // the computed earliest-influencing-input tick per output comes only from inputs that can
    // actually reach that output (so an output does not "inherit" an unrelated input's tick).

    #[test]
    fn influencing_inputs_come_only_from_reachable_sources() {
        // `lo` depends only on a and c; `g` depends only on b and c. A spike on an input that does
        // not reach an output must not lower that output's earliest-influencing tick.
        let src = "\
module Split (input time a, input time b, input time c, output time lo, output time g) {
    lo = min(a, c);
    g = min(b, c);
}
test t for Split {
    wave { a = 2; b = 9; c = 7; expect lo = 2; expect g = 7; }
}
";
        let design = parse(&src);
        let checked = crate::sema::analyze(&design).unwrap();
        let elab = crate::elaborate::elaborate(&checked, "Split").unwrap();
        let netlist = ir::build(&elab);
        let sim = Simulator::new(&netlist, TimeWidth::W32);

        let wave = &design.tests[0].waves[0];
        let event_inputs = sim.event_input_map(wave);
        let earliest = sim.influencing_input_ticks(&event_inputs);

        // `lo` is influenced by a(2) and c(7) -> earliest 2; b(9) does not reach it.
        assert_eq!(earliest.get("lo").copied(), Some(2));
        // `g` is influenced by b(9) and c(7) -> earliest 7; a(2) does not reach it.
        assert_eq!(earliest.get("g").copied(), Some(7));
    }

    // --- choose_shift stays within the width's finite headroom (Req 7.6) --------------------

    #[test]
    fn choose_shift_is_finite_and_within_headroom() {
        let src = "\
module M (input time a, output time y) { y = a; }
test t for M { wave { a = 5; expect y = 5; } }
";
        let design = parse(src);
        let wave = &design.tests[0].waves[0];
        let max_finite = u32::MAX as u64;
        let max_input = 5u64;
        let c = choose_shift(wave, max_finite, max_input);
        assert!(c >= 1, "a nonzero shift when there is headroom");
        assert!(max_input + c < max_finite, "c leaves finite inputs below saturation (Req 7.6)");
        // Deterministic: same wave yields the same c.
        assert_eq!(c, choose_shift(wave, max_finite, max_input), "shift is reproducible");
    }
}
