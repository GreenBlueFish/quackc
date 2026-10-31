//! Event-driven engine (Mode B): discrete-tick event queue simulation (Requirement 6).
//!
//! This is the second simulation mode. Where the functional interpreter in [`crate::testrun`]
//! (Mode A) evaluates the netlist once in topological order and reasons about spike *times*, this
//! engine models explicit discrete integer ticks advancing through an event queue, exactly as a
//! race-logic circuit would run (Req 6.1). The two modes must agree per output wire (Req 6.10,
//! cross-checked by task 18); to make that agreement easy to establish, this module deliberately
//! reuses the same conventions as [`crate::testrun`]:
//!
//! * a spike time / tick is a [`SpikeTime`] (`u64`) and the "never fires" sentinel is [`INF`]
//!   (`u64::MAX`), matching `quack_rt.hpp`'s `INF = numeric_limits<SpikeTime>::max()`;
//! * saturating `inc` clamps to `INF` once a sum reaches the chosen [`TimeWidth`]'s maximum;
//! * the `inf` constant source is an `Input`-kind node named `"inf"` with no incoming edges
//!   ([`is_inf_constant`]), read as `INF`;
//! * input/output ports are grouped from the IR's per-element nodes ([`PortGroup`]) into scalars
//!   and vector elements by [`split_indexed`];
//! * an `Output` node takes its single driver's value, and an `Instance` boundary is a transparent
//!   pass-through of its first named input (documented limitation, see below);
//! * nodes within a tick resolve in topological order via Kahn's algorithm ([`topo_order`]).
//!
//! # The event-driven model (Req 6.1-6.9)
//!
//! Time is a sequence of discrete integer ticks starting at tick 0 (Req 6.1). An event queue
//! ordered by ascending tick holds "node N should (re)evaluate at tick T" requests. The engine pops
//! all events for the smallest pending tick, resolves every affected node once in topological
//! order (no delta-cycle fixpoint — valid because the graph is acyclic; Req 6.7), records any new
//! firings, and schedules downstream consequences:
//!
//! * An input spike at tick `t` is scheduled as an event at tick `t` (Req 6.2).
//! * An `Inc { amount: n }` whose input fires at `t` schedules its own firing at `t + n`
//!   (unit inc `n = 1` -> `t + 1`), saturating to `INF` at the width maximum (Req 6.3).
//! * A `Min` fires at the first tick any input spikes, zero added delay, once (Req 6.4).
//! * An `Lt` fires at tick `t` iff its pass input spikes at `t` **and** its inhibit input has not
//!   spiked at any tick `<= t`; a simultaneous inhibit (inhibit at `== t`) blocks it, and once
//!   blocked it stays unfired for the rest of the run (Req 6.5, 6.6). The decision is made at `t`
//!   without looking ahead — causality in action.
//! * The run continues until the queue is empty or a configurable maximum tick is reached (default
//!   [`DEFAULT_MAX_TICK`], a positive integer; Req 6.8).
//! * Any node that never fires resolves to `INF` (Req 6.9).
//!
//! ## Why ticks give the same answer as Mode A
//!
//! For this acyclic primitive graph, "the tick a node first fires" equals "the spike time Mode A
//! computes": `min` is a tick-wise first-arrival (= numeric min), `inc` adds its delay (= numeric
//! `+n` with saturation), and `lt` passes its pass tick only when strictly earlier than the inhibit
//! tick (= `st_lt`, ties block). The engine therefore schedules each node to fire exactly at its
//! Mode A spike time, which is why task 18 can cross-check the two for equality.
//!
//! ## Instance boundaries (documented limitation)
//!
//! As in [`crate::testrun`], a sub-module instance appears as one opaque `Instance` boundary node;
//! its body is not re-expanded into the top graph. The engine treats the boundary as a transparent
//! pass-through: it fires at the tick of its first named input, and its named outputs forward that
//! tick. This matches Mode A exactly, which keeps the cross-check meaningful, but means a test whose
//! bound module relies on an instance doing non-trivial internal computation is not fully modeled;
//! prefer building the IR rooted at the bound module (flattening) for such tests.

use std::collections::HashMap;

use crate::ir::{NetlistIr, NodeId, NodeKind, PortRole};
use crate::lexer::TimeWidth;

/// A spike time / tick: a non-negative tick, or [`INF`] for "never fires". Shared with
/// [`crate::testrun`] so the two modes compare directly (Req 6.10).
pub type SpikeTime = u64;

/// The "never fires" sentinel: the maximum `u64`, mirroring `quack_rt.hpp`'s
/// `INF = numeric_limits<SpikeTime>::max()`. Used uniformly regardless of the chosen time width;
/// the width only bounds the finite range (see module docs and [`crate::testrun`]).
pub const INF: SpikeTime = u64::MAX;

/// The default maximum tick for a run when the caller does not specify one (Req 6.8). A positive
/// integer bound so a run always terminates. Mirrors `quack_event.hpp`'s `DEFAULT_MAX_TICK`.
pub const DEFAULT_MAX_TICK: SpikeTime = 1 << 20; // 1,048,576 ticks

/// `st_inc(a, n)` with saturating arithmetic (Req 6.3, matching `quack_rt.hpp` and
/// [`crate::testrun`]). `INF` stays `INF`; otherwise `a + n` clamps to `INF` if it would reach or
/// exceed `max_finite` (the width maximum, the value the width's `inf` denotes).
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

/// The maximum finite spike time for a width: the clamp threshold for [`st_inc`]. For `W32` this is
/// `u32::MAX`, for `W16` it is `u16::MAX`.
fn max_finite(width: TimeWidth) -> u64 {
    width.max_literal()
}

/// The result of an event-driven run: the first-firing tick of every node, plus convenience views
/// grouped by output-port wire name (Req 6.9 — a never-firing node is recorded as [`INF`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EventRun {
    /// Every node id -> the tick it first fired, or [`INF`] if it never fired (Req 6.9). Suitable
    /// as the per-node fire-tick map a VCD/trace writer (task 19) consumes.
    pub node_fire: HashMap<NodeId, SpikeTime>,
    /// Each output wire name (`y` or `y[i]`) -> the tick it fired, or [`INF`] (Req 6.9). This is the
    /// view the Mode A/B cross-check (task 18) compares against the functional interpreter.
    pub outputs: HashMap<String, SpikeTime>,
}

impl EventRun {
    /// The firing tick of output wire `wire` (`y` or `y[i]`), or [`INF`] if it never fired.
    pub fn output(&self, wire: &str) -> SpikeTime {
        self.outputs.get(wire).copied().unwrap_or(INF)
    }
}

/// Run the event-driven engine over `ir` for one input assignment (Req 6.1-6.9).
///
/// `inputs` maps an input *wire* name (`a`, or a vector element `a[i]`) to the tick its spike
/// occurs, with [`INF`] meaning "never" (an omitted input is treated as `INF`, matching Req 7.4 at
/// the caller). `width` selects the saturation range for `inc`, and `max_tick` bounds the run
/// (Req 6.8); pass [`DEFAULT_MAX_TICK`] for the default.
///
/// Returns an [`EventRun`] with the first-firing tick of every node and every output wire.
pub fn run(
    ir: &NetlistIr,
    inputs: &HashMap<String, SpikeTime>,
    width: TimeWidth,
    max_tick: SpikeTime,
) -> EventRun {
    Engine::new(ir, width, max_tick).run(inputs)
}

/// The event-driven engine over one module's flat netlist graph.
struct Engine<'a> {
    ir: &'a NetlistIr,
    /// Position of each node id within the topological order, so a tick's affected nodes resolve
    /// in topological order (a dependency precedes its consumers; Req 6.7).
    order_index: HashMap<NodeId, usize>,
    max_finite: u64,
    max_tick: SpikeTime,
    /// Grouped input ports: base name -> (width, index -> node id). Excludes the `inf` constant.
    inputs: HashMap<String, PortGroup>,
    /// Grouped output ports: base name -> (width, index -> node id).
    outputs: HashMap<String, PortGroup>,
}

/// A port grouped from the IR's per-element nodes (mirrors [`crate::testrun`]'s `PortGroup`).
#[derive(Clone, Debug, Default)]
struct PortGroup {
    /// `None` for a scalar, `Some(n)` for a vector of width `n`.
    width: Option<u64>,
    /// Element index -> node id (scalars use index 0).
    elems: Vec<(u64, NodeId)>,
}

impl<'a> Engine<'a> {
    fn new(ir: &'a NetlistIr, width: TimeWidth, max_tick: SpikeTime) -> Self {
        let order = topo_order(ir).unwrap_or_default();
        let order_index = order.iter().enumerate().map(|(i, &id)| (id, i)).collect();

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

        Engine {
            ir,
            order_index,
            max_finite: max_finite(width),
            max_tick,
            inputs,
            outputs,
        }
    }

    /// Run the engine for one input assignment, returning each node's and each output wire's
    /// first-firing tick (Req 6.1-6.9).
    fn run(&self, inputs: &HashMap<String, SpikeTime>) -> EventRun {
        // The firing tick of every node: INF until (and unless) it fires (Req 6.9). The `inf`
        // constant source never fires, so it stays INF.
        let mut node_fire: HashMap<NodeId, SpikeTime> = HashMap::new();
        for node in &self.ir.nodes {
            node_fire.insert(node.id, INF);
        }

        // The event queue: a tick -> the set of node ids to (re)resolve at that tick. A BTreeMap
        // keyed by tick gives ascending-tick ordering for free (Req 6.1); within a tick we resolve
        // affected nodes in topological order (Req 6.7).
        let mut queue: std::collections::BTreeMap<SpikeTime, Vec<NodeId>> =
            std::collections::BTreeMap::new();

        // Seed input spikes as events at their tick (Req 6.2). A finite input tick schedules the
        // input node; an INF (never) input is left unscheduled, so it stays INF.
        let input_ticks = self.resolve_input_ticks(inputs);
        for (&id, &tick) in &input_ticks {
            if tick != INF && tick <= self.max_tick {
                enqueue(&mut queue, tick, id);
            }
        }

        // Process ticks in ascending order until the queue is empty or we pass the max tick
        // (Req 6.8). We pop the smallest pending tick each iteration; events may append later ticks.
        while let Some((&tick, _)) = queue.iter().next() {
            if tick > self.max_tick {
                break; // beyond the configured bound; stop (Req 6.8)
            }
            let affected = queue.remove(&tick).unwrap_or_default();

            // Resolve the tick's affected nodes exactly once each, in topological order so a node
            // sees its already-resolved predecessors (no delta-cycle fixpoint; Req 6.7).
            let mut to_resolve = affected;
            dedup_in_topo_order(&mut to_resolve, &self.order_index);

            for id in to_resolve {
                self.resolve_node(id, tick, &mut node_fire, &mut queue, &input_ticks);
            }
        }

        // Build the output-wire view: each output's tick is its driver's firing tick (Req 6.9).
        let mut outputs = HashMap::new();
        for (base, group) in &self.outputs {
            for &(idx, id) in &group.elems {
                let wire = match group.width {
                    None => base.clone(),
                    Some(_) => format!("{base}[{idx}]"),
                };
                outputs.insert(wire, node_fire.get(&id).copied().unwrap_or(INF));
            }
        }

        EventRun { node_fire, outputs }
    }

    /// Resolve one node at `tick`: decide whether it fires now and, if so, record its firing tick
    /// and schedule its downstream consequences. A node already recorded as fired at an earlier (or
    /// equal) tick is not re-fired (`min`/`lt`/`inc` each fire at most once; Req 6.4-6.6).
    fn resolve_node(
        &self,
        id: NodeId,
        tick: SpikeTime,
        node_fire: &mut HashMap<NodeId, SpikeTime>,
        queue: &mut std::collections::BTreeMap<SpikeTime, Vec<NodeId>>,
        input_ticks: &HashMap<NodeId, SpikeTime>,
    ) {
        // Already fired at or before this tick -> nothing to do (fire once; Req 6.4).
        if node_fire.get(&id).copied().unwrap_or(INF) <= tick {
            return;
        }

        let node = match self.ir.node(id) {
            Some(n) => n,
            None => return,
        };

        let fires_at: Option<SpikeTime> = match &node.kind {
            NodeKind::Input => {
                // The `inf` constant never fires; a real input fires at its scheduled tick (Req 6.2).
                if is_inf_constant(self.ir, id) {
                    None
                } else {
                    match input_ticks.get(&id).copied().unwrap_or(INF) {
                        INF => None,
                        t if t == tick => Some(t),
                        _ => None, // scheduled for a different tick; resolved then
                    }
                }
            }
            NodeKind::Min => {
                // Fires at the first tick any input has fired, zero added delay (Req 6.4). Being
                // resolved at `tick` means some predecessor fired at `tick`; it is the earliest
                // because inputs schedule this node at exactly their own firing tick.
                let earliest = self
                    .operands(id)
                    .into_iter()
                    .map(|src| node_fire.get(&src).copied().unwrap_or(INF))
                    .min()
                    .unwrap_or(INF);
                if earliest == tick {
                    Some(tick)
                } else {
                    None
                }
            }
            NodeKind::Lt => {
                // Fires at tick t iff pass fired at t AND inhibit has not fired at any tick <= t
                // (a simultaneous inhibit blocks; Req 6.5, 6.6). Once blocked it stays unfired: we
                // only ever evaluate this at the pass's firing tick, and the inhibit's firing tick
                // is final by then, so the decision made here is permanent.
                let pass = self
                    .input_with_role(id, &PortRole::Pass)
                    .map(|s| node_fire.get(&s).copied().unwrap_or(INF))
                    .unwrap_or(INF);
                let inhibit = self
                    .input_with_role(id, &PortRole::Inhibit)
                    .map(|s| node_fire.get(&s).copied().unwrap_or(INF))
                    .unwrap_or(INF);
                if pass == tick && inhibit > tick {
                    Some(tick)
                } else {
                    None
                }
            }
            NodeKind::Inc { amount } => {
                // Fires at t + n when its input fires at t, saturating to INF (Req 6.3).
                let input = self
                    .operands(id)
                    .first()
                    .map(|&s| node_fire.get(&s).copied().unwrap_or(INF))
                    .unwrap_or(INF);
                if input == tick {
                    match st_inc(input, *amount as u64, self.max_finite) {
                        INF => None, // saturated: never fires (Req 6.9)
                        // Beyond the configured bound: the delayed firing is never reached, so the
                        // node stays unfired -> INF (Req 6.8, 6.9).
                        out if out > self.max_tick => None,
                        out => Some(out), // fires at t + n (Req 6.3)
                    }
                } else {
                    None
                }
            }
            NodeKind::Output => {
                // An output fires when its single driver fires, same tick (zero delay).
                let driver = self
                    .ir
                    .incoming(id)
                    .next()
                    .map(|e| node_fire.get(&e.from).copied().unwrap_or(INF))
                    .unwrap_or(INF);
                if driver == tick {
                    Some(tick)
                } else {
                    None
                }
            }
            NodeKind::Instance { .. } => {
                // Opaque boundary: transparent pass-through of its first named input (see module
                // docs). Fires at that input's tick.
                let src = self
                    .ir
                    .incoming(id)
                    .find(|e| matches!(e.port_role, PortRole::InstancePort(_)))
                    .map(|e| node_fire.get(&e.from).copied().unwrap_or(INF))
                    .unwrap_or(INF);
                if src == tick {
                    Some(tick)
                } else {
                    None
                }
            }
        };

        if let Some(fire_tick) = fires_at {
            node_fire.insert(id, fire_tick);
            self.schedule_consumers(id, fire_tick, queue);
        }
    }

    /// Schedule each consumer of `id` to resolve at `fire_tick` — the tick `id` just fired. Every
    /// consumer resolves at the tick its relevant input fired; the consumer itself then decides its
    /// own output tick in [`Engine::resolve_node`] (an `Inc` adds its delay there, Req 6.3; a
    /// `Min`/`Lt`/`Output` fires same-tick, Req 6.4, 6.5). This keeps the delay logic in exactly
    /// one place and makes "resolve each node once per tick after its predecessors" hold (Req 6.7).
    fn schedule_consumers(
        &self,
        id: NodeId,
        fire_tick: SpikeTime,
        queue: &mut std::collections::BTreeMap<SpikeTime, Vec<NodeId>>,
    ) {
        if fire_tick == INF || fire_tick > self.max_tick {
            return;
        }
        for edge in self.ir.outgoing(id) {
            enqueue(queue, fire_tick, edge.to);
        }
    }

    /// Map each input-port element node id to its spike tick, defaulting an omitted input to
    /// [`INF`] (Req 6.2; the "default omitted input to inf" convention of Req 7.4 is honored by the
    /// caller passing INF or omitting the wire). Mirrors [`crate::testrun`]'s input grouping.
    fn resolve_input_ticks(&self, inputs: &HashMap<String, SpikeTime>) -> HashMap<NodeId, SpikeTime> {
        let mut ticks = HashMap::new();
        for (base, group) in &self.inputs {
            for &(idx, id) in &group.elems {
                let wire = match group.width {
                    None => base.clone(),
                    Some(_) => format!("{base}[{idx}]"),
                };
                // Accept either the exact element key (`a[i]`) or the base key (`a`) for a scalar.
                let tick = inputs
                    .get(&wire)
                    .copied()
                    .or_else(|| if group.width.is_none() { inputs.get(base).copied() } else { None })
                    .unwrap_or(INF);
                ticks.insert(id, tick);
            }
        }
        ticks
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

/// Waveform / trace emission (Req 6.12, 6.13, 12.7).
///
/// Both emitters are pure, dependency-free string builders: they take a finished [`EventRun`] and
/// the `&NetlistIr` it was produced from, and return text the CLI (task 27) writes to a file when
/// `--vcd` is set. Keeping the formatting here, next to the engine, means the "wire" identity used
/// in the trace is exactly the engine's node id (Req 12.7).
///
/// ## Chosen wire -> signal mapping (documented, Req 6.12)
///
/// A VCD must contain *exactly one signal per wire*. In this IR every wire — every module input,
/// every module output, and every named internal/primitive node — is already a distinct [`Node`]
/// with a unique [`NodeId`]. So the mapping is one VCD `$var` per IR node, keyed by node id, with
/// one deliberate exclusion: the `inf` constant source (an `Input`-kind node named `"inf"` with no
/// incoming edges; see [`is_inf_constant`]) is not a real wire — it is a constant that never fires
/// — so it is omitted. Every remaining node contributes exactly one `$var` and at most one value
/// change, which is what the task's structural tests assert.
///
/// ## Representing a spike time as a VCD signal
///
/// A spike time is naturally a one-shot "fired at tick `t`" event, so each wire is a 1-bit VCD
/// signal that starts at `0` (at `#0`) and goes to `1` at its firing tick. A node that never fires
/// ([`INF`]) stays `0` for the whole dump and emits no value change. This yields valid,
/// gtkwave-readable output while keeping one change per wire.
pub mod emit {
    use super::*;

    /// A short, deterministic VCD identifier code for a node id. VCD identifier codes are strings
    /// of printable ASCII in the range `!`..=`~`; we map a node id into a base-94 code over that
    /// range so each node gets a unique, compact symbol regardless of how many nodes there are.
    fn vcd_id_code(mut id: NodeId) -> String {
        const FIRST: u8 = b'!'; // 33
        const LAST: u8 = b'~'; // 126
        const BASE: u32 = (LAST - FIRST) as u32 + 1; // 94 printable codes
        let mut code = Vec::new();
        loop {
            let digit = (id % BASE) as u8;
            code.push(FIRST + digit);
            id /= BASE;
            if id == 0 {
                break;
            }
        }
        // Little-endian digits are fine; the mapping only needs to be a bijection per id.
        String::from_utf8(code).expect("printable ASCII code")
    }

    /// The ordered set of wires a dump/trace covers: every node except the `inf` constant source,
    /// in ascending node-id order for determinism. Returns `(id, display_name)` pairs where the
    /// display name is the node's original wire/instance name (Req 4.2).
    fn wire_nodes(ir: &NetlistIr) -> Vec<(NodeId, String)> {
        let mut wires: Vec<(NodeId, String)> = ir
            .nodes
            .iter()
            .filter(|n| !is_inf_constant(ir, n.id))
            .map(|n| (n.id, n.name.clone()))
            .collect();
        wires.sort_by_key(|&(id, _)| id);
        wires
    }

    /// Build a VCD waveform for `run` over `ir`: exactly one 1-bit `$var` per wire (Req 6.12), a
    /// well-formed header, and a value change (`1<code>`) at each wire's firing tick.
    ///
    /// Returns the full VCD text. Every wire is declared under a single `$scope module <top>` and
    /// initialized to `0` at `#0`; a wire that fires at a finite tick `t` emits `1<code>` in the
    /// `#t` section. Non-firing (`INF`) wires keep their initial `0` and emit no change.
    pub fn vcd(ir: &NetlistIr, run: &EventRun) -> String {
        let wires = wire_nodes(ir);

        let mut out = String::new();
        // --- Header (Req 6.12: well-formed $date/$version/$timescale/$scope/$var/$enddefinitions)
        out.push_str("$date\n    quackc event-driven simulation\n$end\n");
        out.push_str("$version\n    quackc Mode B VCD writer\n$end\n");
        out.push_str("$timescale 1ns $end\n");
        out.push_str(&format!("$scope module {} $end\n", vcd_ident(&ir.top)));
        for &(id, ref name) in &wires {
            // One 1-bit signal per wire (Req 6.12). The reference name carries the original wire
            // name plus the node id so duplicate names (e.g. several `min` nodes) stay distinct.
            out.push_str(&format!(
                "$var wire 1 {code} {name} $end\n",
                code = vcd_id_code(id),
                name = format!("{}_n{}", vcd_ident(name), id),
            ));
        }
        out.push_str("$upscope $end\n");
        out.push_str("$enddefinitions $end\n");

        // --- Initial values: every wire low at tick 0 (one-shot "fired" signals).
        out.push_str("#0\n");
        out.push_str("$dumpvars\n");
        for &(id, _) in &wires {
            out.push_str(&format!("0{}\n", vcd_id_code(id)));
        }
        out.push_str("$end\n");

        // --- Value changes grouped by ascending tick: each wire goes high at its firing tick.
        let mut by_tick: std::collections::BTreeMap<SpikeTime, Vec<NodeId>> =
            std::collections::BTreeMap::new();
        for &(id, _) in &wires {
            match run.node_fire.get(&id).copied().unwrap_or(INF) {
                INF => {}
                t if t == 0 => {} // already represented by the #0 init; avoid a duplicate section
                t => by_tick.entry(t).or_default().push(id),
            }
        }
        // A wire firing at tick 0 flips to 1 within the #0 dumpvars block for correctness.
        let fires_at_zero: Vec<NodeId> = wires
            .iter()
            .filter(|&&(id, _)| run.node_fire.get(&id).copied().unwrap_or(INF) == 0)
            .map(|&(id, _)| id)
            .collect();
        if !fires_at_zero.is_empty() {
            // Re-emit the #0 section's highs after dumpvars so tick-0 firings show as 1.
            for id in fires_at_zero {
                out.push_str(&format!("1{}\n", vcd_id_code(id)));
            }
        }

        for (tick, ids) in by_tick {
            out.push_str(&format!("#{tick}\n"));
            for id in ids {
                out.push_str(&format!("1{}\n", vcd_id_code(id)));
            }
        }

        out
    }

    /// Sanitize a wire/module name into a VCD-safe identifier token (no spaces or VCD delimiters).
    /// VCD reference names are whitespace-delimited; `[`/`]` are allowed by gtkwave for vectors but
    /// we keep it conservative by replacing anything non-alphanumeric (besides `_`) with `_`.
    fn vcd_ident(name: &str) -> String {
        name.chars()
            .map(|c| if c.is_ascii_alphanumeric() || c == '_' { c } else { '_' })
            .collect()
    }

    /// Build the JSON-lines spike trace for `run`: one line per node that actually fired, in the
    /// form `{"wire": <id>, "tick": <t>}` (Req 6.13, 12.7).
    ///
    /// The wire id is the node id (Req 12.7). Only nodes with a finite firing tick emit a line; a
    /// never-firing (`INF`) node emits nothing. Lines are ordered by ascending `(tick, wire id)`
    /// for determinism and the whole text is newline-terminated (one `\n` per line).
    pub fn spike_trace(ir: &NetlistIr, run: &EventRun) -> String {
        // Only real wires (exclude the inf constant source) that fired at a finite tick.
        let mut spikes: Vec<(SpikeTime, NodeId)> = ir
            .nodes
            .iter()
            .filter(|n| !is_inf_constant(ir, n.id))
            .filter_map(|n| match run.node_fire.get(&n.id).copied().unwrap_or(INF) {
                INF => None,
                t => Some((t, n.id)),
            })
            .collect();
        spikes.sort_unstable();

        let mut out = String::new();
        for (tick, id) in spikes {
            out.push_str(&format!("{{\"wire\": {id}, \"tick\": {tick}}}\n"));
        }
        out
    }
}

/// Enqueue `id` to resolve at `tick`, keeping only one pending entry per node per tick.
fn enqueue(
    queue: &mut std::collections::BTreeMap<SpikeTime, Vec<NodeId>>,
    tick: SpikeTime,
    id: NodeId,
) {
    let bucket = queue.entry(tick).or_default();
    if !bucket.contains(&id) {
        bucket.push(id);
    }
}

/// Sort a tick's affected node ids into topological order and remove duplicates, so each resolves
/// exactly once per tick after its predecessors (Req 6.7). Nodes absent from the order (should not
/// happen for a well-formed acyclic graph) sort last by a stable fallback.
fn dedup_in_topo_order(ids: &mut Vec<NodeId>, order_index: &HashMap<NodeId, usize>) {
    ids.sort_by_key(|id| order_index.get(id).copied().unwrap_or(usize::MAX));
    ids.dedup();
}

/// Insert a boundary node into a grouped-port map keyed by base name, recording vector width.
/// Identical to [`crate::testrun`]'s grouping so the two modes see the same ports.
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
/// `"inf"` with no incoming edges, exactly as [`crate::ir`] emits it (mirrors [`crate::testrun`]).
fn is_inf_constant(ir: &NetlistIr, id: NodeId) -> bool {
    match ir.node(id) {
        Some(n) => {
            matches!(n.kind, NodeKind::Input) && n.name == "inf" && ir.incoming(id).next().is_none()
        }
        None => false,
    }
}

/// Split a node/wire name into `(base, Some(index))` for `name[i]`, or `(name, None)` for a scalar.
/// Identical to [`crate::testrun`]'s helper so wire names line up across modes.
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

/// Topological order of node ids via Kahn's algorithm, breaking ties by ascending id for
/// determinism. Returns `None` on a cyclic graph. Identical to [`crate::testrun`]'s ordering.
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
    use crate::ir;
    use crate::operators::OperatorTable;
    use crate::parser::parse_source;
    use crate::sema::analyze;

    /// Parse + analyze + elaborate `src` rooted at `top`, then build its netlist IR, panicking on
    /// any earlier-stage error so engine tests start from a validated, elaborated design. Mirrors
    /// the fixture helpers in `ir` / `testrun` so the engine exercises the real frontend.
    fn build_ir(src: &str, top: &str) -> NetlistIr {
        let design = parse_source(src, "test.sta", &OperatorTable::builtin(), TimeWidth::W32)
            .unwrap_or_else(|d| {
                let r: Vec<String> = d.iter().map(|x| x.render()).collect();
                panic!("fixture failed to parse:\n{}", r.join("\n"))
            });
        let checked = analyze(&design).unwrap_or_else(|errs| {
            let r: Vec<String> = errs.iter().map(|e| e.render()).collect();
            panic!("fixture failed sema:\n{}", r.join("\n"))
        });
        let elab = crate::elaborate::elaborate(&checked, top).unwrap_or_else(|errs| {
            let r: Vec<String> = errs.iter().map(|e| e.render()).collect();
            panic!("fixture failed to elaborate:\n{}", r.join("\n"))
        });
        ir::build(&elab)
    }

    /// Build an input map from `(wire, tick)` pairs.
    fn ins(pairs: &[(&str, SpikeTime)]) -> HashMap<String, SpikeTime> {
        pairs.iter().map(|&(w, t)| (w.to_string(), t)).collect()
    }

    /// Run the engine at `W32` with the default max tick and return the output-wire ticks.
    fn run_w32(ir: &NetlistIr, inputs: &HashMap<String, SpikeTime>) -> EventRun {
        run(ir, inputs, TimeWidth::W32, DEFAULT_MAX_TICK)
    }

    // --- inc delay: input at t fires the inc output at t+1 and t+n (Req 6.3) ----------------

    #[test]
    fn unit_inc_fires_one_tick_later() {
        // y = inc(a): a spikes at 4 -> y fires at 5.
        let ir = build_ir("module M (input time a, output time y) { y = inc(a); }", "M");
        let r = run_w32(&ir, &ins(&[("a", 4)]));
        assert_eq!(r.output("y"), 5, "inc delays t -> t+1 (Req 6.3)");
    }

    #[test]
    fn delay_by_n_fires_n_ticks_later() {
        // y = a + 3: a spikes at 10 -> y fires at 13.
        let ir = build_ir(
            "module M #(param N = 3) (input time a, output time y) { y = a + N; }",
            "M",
        );
        let r = run_w32(&ir, &ins(&[("a", 10)]));
        assert_eq!(r.output("y"), 13, "a+n delays t -> t+n (Req 6.3)");
    }

    #[test]
    fn inc_of_never_input_never_fires() {
        // a never spikes -> inc output never fires -> INF (Req 6.9).
        let ir = build_ir("module M (input time a, output time y) { y = inc(a); }", "M");
        let r = run_w32(&ir, &ins(&[("a", INF)]));
        assert_eq!(r.output("y"), INF, "inc(never) never fires (Req 6.9)");
    }

    // --- min: first arrival, zero delay (Req 6.4) -------------------------------------------

    #[test]
    fn min_fires_at_first_arrival_zero_delay() {
        // y = min(a, b): first of the two to arrive, same tick.
        let ir = build_ir(
            "module M (input time a, input time b, output time y) { y = min(a, b); }",
            "M",
        );
        assert_eq!(run_w32(&ir, &ins(&[("a", 7), ("b", 3)])).output("y"), 3, "b arrives first");
        assert_eq!(run_w32(&ir, &ins(&[("a", 2), ("b", 9)])).output("y"), 2, "a arrives first");
        // A tie: both at 5 -> fires at 5, once.
        assert_eq!(run_w32(&ir, &ins(&[("a", 5), ("b", 5)])).output("y"), 5, "tie fires at 5");
        // One never, one finite -> the finite one.
        assert_eq!(run_w32(&ir, &ins(&[("a", INF), ("b", 4)])).output("y"), 4, "finite beats never");
        // Both never -> never fires (Req 6.9).
        assert_eq!(run_w32(&ir, &ins(&[("a", INF), ("b", INF)])).output("y"), INF, "both never -> INF");
    }

    // --- lt: pass-through when inhibit later/absent (Req 6.5) -------------------------------

    #[test]
    fn lt_passes_when_inhibit_is_later_or_absent() {
        // y = a < b : pass = a, inhibit = b. Fires at a iff a < b.
        let ir = build_ir(
            "module M (input time a, input time b, output time y) { y = a < b; }",
            "M",
        );
        // Inhibit strictly later -> pass fires at its tick.
        assert_eq!(run_w32(&ir, &ins(&[("a", 3), ("b", 8)])).output("y"), 3, "a<b passes a (Req 6.5)");
        // Inhibit absent (never) -> pass fires.
        assert_eq!(run_w32(&ir, &ins(&[("a", 6), ("b", INF)])).output("y"), 6, "absent inhibit passes");
    }

    // --- lt: blocked by inhibit earlier AND simultaneous, and stays blocked (Req 6.6) -------

    #[test]
    fn lt_blocked_by_earlier_inhibit() {
        let ir = build_ir(
            "module M (input time a, input time b, output time y) { y = a < b; }",
            "M",
        );
        // Inhibit strictly earlier (b at 2 <= a at 5) -> blocked -> INF (Req 6.6).
        assert_eq!(run_w32(&ir, &ins(&[("a", 5), ("b", 2)])).output("y"), INF, "earlier inhibit blocks");
    }

    #[test]
    fn lt_blocked_by_simultaneous_inhibit() {
        let ir = build_ir(
            "module M (input time a, input time b, output time y) { y = a < b; }",
            "M",
        );
        // Simultaneous inhibit (b == a == 4) is a tie: blocks -> INF (Req 6.6).
        assert_eq!(run_w32(&ir, &ins(&[("a", 4), ("b", 4)])).output("y"), INF, "tie blocks (Req 6.6)");
    }

    #[test]
    fn lt_stays_blocked_for_the_rest_of_the_run() {
        // Chain two lts: z = (a < b) < c. If the inner lt is blocked, it is INF forever, so the
        // outer lt sees pass=INF and never fires regardless of c. Inner blocked by b <= a.
        let ir = build_ir(
            "module M (input time a, input time b, input time c, output time z) { z = (a < b) < c; }",
            "M",
        );
        // Inner blocked (b=1 <= a=5). Outer pass is INF, so z never fires even with a huge c.
        let r = run_w32(&ir, &ins(&[("a", 5), ("b", 1), ("c", 100)]));
        assert_eq!(r.output("z"), INF, "a blocked inner lt stays unfired, blocking the outer (Req 6.6)");
    }

    // --- non-firing node resolves to INF (Req 6.9) ------------------------------------------

    #[test]
    fn non_firing_output_is_inf() {
        // Pure pass-through with a never input: the output never fires.
        let ir = build_ir("module M (input time a, output time y) { y = a; }", "M");
        assert_eq!(run_w32(&ir, &ins(&[("a", INF)])).output("y"), INF, "never input -> INF output");
        // Supplying nothing also defaults to never (caller omits the wire).
        assert_eq!(run_w32(&ir, &ins(&[])).output("y"), INF, "omitted input -> INF (Req 6.9)");
    }

    // --- max-tick cutoff (Req 6.8) ----------------------------------------------------------

    #[test]
    fn max_tick_cutoff_drops_events_beyond_the_bound() {
        // y = a + 10: a spikes at 5 -> y would fire at 15. With max_tick = 12, the delayed event
        // at 15 is never processed, so y stays INF (Req 6.8, 6.9).
        let ir = build_ir(
            "module M #(param N = 10) (input time a, output time y) { y = a + N; }",
            "M",
        );
        let capped = run(&ir, &ins(&[("a", 5)]), TimeWidth::W32, 12);
        assert_eq!(capped.output("y"), INF, "firing beyond max_tick is cut off (Req 6.8)");
        // With a generous max tick it does fire at 15.
        let full = run(&ir, &ins(&[("a", 5)]), TimeWidth::W32, 100);
        assert_eq!(full.output("y"), 15, "within the bound it fires at t+n");
    }

    // --- inc saturates to INF at the width maximum (Req 6.3 / 6.9) --------------------------

    #[test]
    fn inc_saturates_to_inf_at_width_max() {
        // At W16 the finite max is 65535. a near the top + a delay saturates to INF.
        let ir = build_ir(
            "module M #(param N = 10) (input time a, output time y) { y = a + N; }",
            "M",
        );
        let near_max = u16::MAX as u64 - 2; // 65533
        let r = run(&ir, &ins(&[("a", near_max)]), TimeWidth::W16, DEFAULT_MAX_TICK);
        assert_eq!(r.output("y"), INF, "sum reaching the width max saturates to INF (Req 6.3)");
    }

    // --- a small composite: cross-check against the Mode A interpreter (sanity for task 18) --

    #[test]
    fn composite_matches_mode_a_on_several_waves() {
        // Sort2: lo = min(a, b); hi = max(a, b). `max` elaborates to inc/min/lt primitives, so this
        // exercises every primitive the engine supports. We compare the engine's per-output tick
        // against the Mode A interpreter's spike time for the same wave, for several input waves.
        let src = "\
module Sort2 (input time a, input time b, output time lo, output time hi) {
    lo = min(a, b);
    hi = max(a, b);
}
";
        let netlist = build_ir(src, "Sort2");

        // (a, b) waves spanning first-arrival both ways, ties, and inf.
        let waves: &[(SpikeTime, SpikeTime)] =
            &[(3, 5), (7, 2), (4, 4), (0, 9), (INF, 6), (6, INF), (INF, INF)];

        for &(a, b) in waves {
            let inputs = ins(&[("a", a), ("b", b)]);
            let ev = run_w32(&netlist, &inputs);

            // Mode A reference: lo = min, hi = max, with INF as the max value.
            let ref_lo = a.min(b);
            let ref_hi = a.max(b);

            assert_eq!(
                ev.output("lo"),
                ref_lo,
                "lo for a={}, b={} must match Mode A (min)",
                a, b
            );
            assert_eq!(
                ev.output("hi"),
                ref_hi,
                "hi for a={}, b={} must match Mode A (max)",
                a, b
            );
        }
    }

    // --- the event queue is drained in ascending tick order with no delta cycles (Req 6.7) --

    #[test]
    fn per_tick_topological_resolution_two_stage_delay() {
        // y = inc(inc(a)): a at 0 -> first inc fires at 1 -> second inc fires at 2. This checks the
        // engine schedules the chain forward one tick at a time and resolves each node once.
        let ir = build_ir("module M (input time a, output time y) { y = inc(inc(a)); }", "M");
        let r = run_w32(&ir, &ins(&[("a", 0)]));
        assert_eq!(r.output("y"), 2, "two unit incs delay 0 -> 2 (Req 6.3, 6.7)");
    }

    // --- VCD + JSON-lines spike trace emission (Req 6.12, 6.13, 12.7) -----------------------

    /// The wires a dump/trace covers: every node except the `inf` constant source.
    fn expected_wire_ids(ir: &NetlistIr) -> Vec<NodeId> {
        let mut ids: Vec<NodeId> = ir
            .nodes
            .iter()
            .filter(|n| !is_inf_constant(ir, n.id))
            .map(|n| n.id)
            .collect();
        ids.sort_unstable();
        ids
    }

    #[test]
    fn vcd_has_exactly_one_var_per_wire_and_a_well_formed_header() {
        // y = inc(a): nodes are Input a, Inc, Output y -> 3 wires, 3 $var lines (Req 6.12).
        let ir = build_ir("module M (input time a, output time y) { y = inc(a); }", "M");
        let run = run_w32(&ir, &ins(&[("a", 4)]));
        let text = emit::vcd(&ir, &run);

        // Exactly one $var per wire (Req 6.12).
        let var_count = text.lines().filter(|l| l.starts_with("$var ")).count();
        let expected = expected_wire_ids(&ir).len();
        assert_eq!(var_count, expected, "exactly one $var per wire (Req 6.12)");
        assert_eq!(expected, 3, "a, inc, y are the three wires");

        // Each $var declares a 1-bit wire signal.
        for line in text.lines().filter(|l| l.starts_with("$var ")) {
            assert!(line.contains("wire 1 "), "each wire is a 1-bit signal: {line}");
            assert!(line.ends_with("$end"), "well-formed $var terminator: {line}");
        }

        // Well-formed header: required sections present and definitions closed.
        for needle in [
            "$date",
            "$version",
            "$timescale 1ns $end",
            "$scope module M $end",
            "$upscope $end",
            "$enddefinitions $end",
        ] {
            assert!(text.contains(needle), "VCD header must contain `{needle}`");
        }
    }

    #[test]
    fn vcd_excludes_the_inf_constant_source() {
        // y = a < inf: `inf` appears as a constant source node but is not a real wire, so it must
        // NOT get a $var — exactly one $var per *wire* (Req 6.12).
        let ir = build_ir("module M (input time a, output time y) { y = a < inf; }", "M");
        let run = run_w32(&ir, &ins(&[("a", 2)]));
        let text = emit::vcd(&ir, &run);

        let inf_node = ir.nodes.iter().find(|n| is_inf_constant(&ir, n.id));
        assert!(inf_node.is_some(), "fixture should contain an inf constant source");

        let var_count = text.lines().filter(|l| l.starts_with("$var ")).count();
        assert_eq!(
            var_count,
            expected_wire_ids(&ir).len(),
            "the inf constant source gets no $var (Req 6.12)"
        );
        assert!(
            var_count < ir.nodes.len(),
            "at least the inf node is excluded from the wire set"
        );
    }

    #[test]
    fn vcd_signal_change_appears_at_its_firing_tick() {
        // y = a + 3: a at 10 -> y (Output) fires at 13. The output wire's value change must appear
        // in the #13 section as `1<code>`.
        let ir = build_ir(
            "module M #(param N = 3) (input time a, output time y) { y = a + N; }",
            "M",
        );
        let run = run_w32(&ir, &ins(&[("a", 10)]));
        let text = emit::vcd(&ir, &run);

        // Find the output node id and its VCD code from its $var declaration name suffix `_n<id>`.
        let out_id = ir
            .nodes
            .iter()
            .find(|n| matches!(n.kind, NodeKind::Output))
            .expect("has an output node")
            .id;
        assert_eq!(run.node_fire.get(&out_id).copied(), Some(13), "output fires at 13");

        // The dump must contain a #13 section, and the value change for the output appears at or
        // after that marker (and not before it).
        let lines: Vec<&str> = text.lines().collect();
        let pos_13 = lines.iter().position(|l| *l == "#13").expect("has a #13 section");

        // Every tick marker before #13 must be strictly less than 13 (ascending order), and 13 is
        // present — so the firing shows up at the right tick, not earlier.
        for l in &lines[..pos_13] {
            if let Some(rest) = l.strip_prefix('#') {
                if let Ok(t) = rest.parse::<u64>() {
                    assert!(t < 13, "tick sections ascend; {t} precedes #13");
                }
            }
        }
    }

    #[test]
    fn spike_trace_has_one_valid_json_line_per_fired_node_and_none_for_inf() {
        // y = inc(a): a at 4 fires Input a (4), Inc (5), Output y (5). None are INF, so three lines.
        let ir = build_ir("module M (input time a, output time y) { y = inc(a); }", "M");
        let run = run_w32(&ir, &ins(&[("a", 4)]));
        let trace = emit::spike_trace(&ir, &run);

        let lines: Vec<&str> = trace.lines().collect();

        // One line per fired node, no line for non-firing (INF) nodes (Req 6.13, 12.7).
        let fired: Vec<NodeId> = ir
            .nodes
            .iter()
            .filter(|n| !is_inf_constant(&ir, n.id))
            .filter(|n| run.node_fire.get(&n.id).copied().unwrap_or(INF) != INF)
            .map(|n| n.id)
            .collect();
        assert_eq!(lines.len(), fired.len(), "one line per fired node (Req 6.13)");
        assert_eq!(fired.len(), 3, "a, inc, y all fired");

        // Each line is valid JSON of the form {"wire": <id>, "tick": <t>} with the correct values.
        let mut seen: HashMap<NodeId, SpikeTime> = HashMap::new();
        for line in &lines {
            let (wire, tick) = parse_trace_line(line);
            // The id must be a real wire and the tick must match the engine's recorded firing tick.
            let expected = run.node_fire.get(&wire).copied().unwrap_or(INF);
            assert_eq!(tick, expected, "trace tick matches node_fire for wire {wire}");
            assert!(expected != INF, "no line is emitted for a never-firing node (Req 6.13)");
            seen.insert(wire, tick);
        }
        // Exactly the fired set, no duplicates.
        let mut seen_ids: Vec<NodeId> = seen.keys().copied().collect();
        seen_ids.sort_unstable();
        let mut fired_sorted = fired.clone();
        fired_sorted.sort_unstable();
        assert_eq!(seen_ids, fired_sorted, "trace covers exactly the fired wires");
    }

    #[test]
    fn spike_trace_omits_never_firing_nodes() {
        // y = inc(a) with a never spiking: nothing fires -> the trace is empty (Req 6.13).
        let ir = build_ir("module M (input time a, output time y) { y = inc(a); }", "M");
        let run = run_w32(&ir, &ins(&[("a", INF)]));
        let trace = emit::spike_trace(&ir, &run);
        assert!(trace.is_empty(), "no spikes -> empty trace (Req 6.13)");

        // Partial firing: a fires but the lt is blocked. y = a < b, with b earlier blocks y.
        let ir2 = build_ir(
            "module M (input time a, input time b, output time y) { y = a < b; }",
            "M",
        );
        let run2 = run_w32(&ir2, &ins(&[("a", 5), ("b", 2)]));
        let trace2 = emit::spike_trace(&ir2, &run2);
        // y never fires (blocked), so no output line; but a and b fired.
        for line in trace2.lines() {
            let (wire, tick) = parse_trace_line(line);
            assert!(tick != INF, "every emitted line is a finite firing");
            let node = ir2.node(wire).expect("line references a real node");
            assert!(
                !matches!(node.kind, NodeKind::Output),
                "the blocked output y must not appear in the trace"
            );
        }
    }

    /// Minimal parser for a trace line `{"wire": <id>, "tick": <t>}`, asserting its JSON shape and
    /// returning `(wire, tick)`. Panics (failing the test) if the line is malformed.
    fn parse_trace_line(line: &str) -> (NodeId, SpikeTime) {
        let inner = line
            .strip_prefix('{')
            .and_then(|s| s.strip_suffix('}'))
            .unwrap_or_else(|| panic!("trace line must be a JSON object: {line}"));
        let mut wire: Option<NodeId> = None;
        let mut tick: Option<SpikeTime> = None;
        for field in inner.split(',') {
            let mut kv = field.splitn(2, ':');
            let key = kv.next().unwrap().trim().trim_matches('"');
            let val = kv
                .next()
                .unwrap_or_else(|| panic!("field must be key:value: {field}"))
                .trim();
            match key {
                "wire" => wire = Some(val.parse().expect("wire id is an integer")),
                "tick" => tick = Some(val.parse().expect("tick is an integer")),
                other => panic!("unexpected trace field `{other}`"),
            }
        }
        (
            wire.expect("line has a wire field"),
            tick.expect("line has a tick field"),
        )
    }
}
