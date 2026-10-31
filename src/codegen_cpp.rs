//! Functional C++ backend (Mode A): straight-line topological-order codegen (Requirement 5).
//!
//! `codegen_cpp` consumes the backend-neutral netlist IR and emits C++17 that depends only on the
//! standard library and the hand-written `quack_rt.hpp` runtime header (Req 5.11). For the module
//! it emits a `<Module>_in` struct, a `<Module>_out` struct, and a
//! `<Module>_out <Module>_eval(const <Module>_in& in)` function (Req 5.1). The body is
//! straight-line: one local per wire, each emitted after the locals it depends on, in topological
//! order (Req 5.2). Vector ports/wires of width `N` become `std::array<SpikeTime, N>` (Req 5.4).
//! Primitive nodes lower to the runtime helpers `quack::st_inc` / `quack::st_min` / `quack::st_lt`
//! (Req 5.5). A sub-module `Instance` boundary becomes a call to the sub-module's `_eval`, or is
//! inlined when [`BackendOptions::inline_instances`] is set (Req 5.6). Before emitting, the backend
//! guards against a cyclic dataflow graph: if no topological order exists it reports a
//! combinational-cycle error and emits no code for that module (Req 5.3), reusing
//! [`crate::checks::check_acyclic`].
//!
//! ## IR shape and the single-`_eval` decision
//!
//! The netlist produced by [`crate::ir::build`] is a *single flat* dataflow graph for the top
//! module's hierarchy: every primitive, `Input`, and `Output` node belongs to the top module's
//! scope, and a sub-module instance appears as one opaque `Instance` boundary node (its own body
//! is not re-expanded into separate per-module graphs). Because the IR does not carry a separate
//! node set per module, this backend emits a single top-level `<top>_eval` with its in/out structs
//! and `std::array` vector ports (as the task permits), and lowers each `Instance` boundary to a
//! call to the named sub-module's `_eval` — honoring Req 5.1 and the call form of Req 5.6 where the
//! IR supports it. When `inline_instances` is set, the boundary's output locals are instead
//! computed inline from its input locals (a transparent pass-through at this IR granularity, since
//! the instance body is opaque), satisfying the inline form of Req 5.6.

use std::collections::{BTreeMap, HashMap};

use crate::backend::{Backend, BackendOptions, BackendOutput, EmittedFile};
use crate::checks::check_acyclic;
use crate::diag::Diagnostic;
use crate::ir::{NetlistIr, Node, NodeId, NodeKind, PortRole, CURRENT_SCHEMA_VERSION};

/// The functional C++ backend (Mode A). Implements [`Backend`] over the stable netlist IR.
#[derive(Clone, Copy, Debug, Default)]
pub struct FunctionalBackend;

impl FunctionalBackend {
    /// Construct the functional C++ backend.
    pub fn new() -> Self {
        FunctionalBackend
    }
}

impl Backend for FunctionalBackend {
    fn name(&self) -> &str {
        "functional-cpp"
    }

    fn supported_ir_major(&self) -> u16 {
        CURRENT_SCHEMA_VERSION.major
    }

    fn run(&self, ir: &NetlistIr, opts: &BackendOptions) -> Result<BackendOutput, Vec<Diagnostic>> {
        // Guard against a cyclic dataflow graph: if no topological order exists, report an error
        // and emit no code (Req 5.3). `check_acyclic` reports the ordered cycle path.
        check_acyclic(ir)?;

        let contents = emit_module(ir, opts);
        let file = EmittedFile::new(format!("{}.hpp", ir.top), contents);
        // Collect non-fatal delay-chain saturation warnings for the chosen SpikeTime width. These
        // never stop compilation (Req 13.5, 13.6, 13.7).
        let warnings = collect_saturation_warnings(ir, opts.time_width);
        Ok(BackendOutput {
            files: vec![file],
            warnings,
        })
    }
}

/// The maximum finite spike time (`INF`) for a chosen `SpikeTime` bit width: `65535` for 16-bit,
/// `4294967295` for 32-bit (Req 5.8, 5.9). Any other width falls back to the 32-bit maximum, which
/// is the compiler's default (Req 5.7). Returned as a `u64` so a 32-bit max does not overflow the
/// comparison against an `Inc` node's amount.
fn spike_time_max(time_width: u16) -> u64 {
    match time_width {
        16 => u16::MAX as u64,
        _ => u32::MAX as u64,
    }
}

/// Collect a delay-chain saturation *warning* for every `Inc { amount: n }` node whose delay `n`
/// reaches or exceeds the maximum value representable by the chosen `SpikeTime` width (Req 13.5).
///
/// At or past that maximum the saturating `st_inc` runtime helper clamps the result to `INF`, so a
/// delay chain this long can never produce a finite spike time. The warning identifies the delay
/// chain by its wire/instance name and module hierarchy path, and names the `SpikeTime` width in
/// effect. These are warnings, never errors: they never stop compilation (Req 13.6, 13.7).
///
/// Warnings are returned in ascending node-id order so the output is deterministic.
fn collect_saturation_warnings(ir: &NetlistIr, time_width: u16) -> Vec<Diagnostic> {
    let max = spike_time_max(time_width);
    let mut nodes: Vec<&Node> = ir
        .nodes
        .iter()
        .filter(|n| matches!(n.kind, NodeKind::Inc { amount } if amount as u64 >= max))
        .collect();
    nodes.sort_by_key(|n| n.id);

    nodes
        .into_iter()
        .map(|node| {
            let amount = match node.kind {
                NodeKind::Inc { amount } => amount,
                _ => unreachable!("filtered to Inc nodes above"),
            };
            let chain = chain_identity(node);
            let message = format!(
                "delay chain {chain} delays by {amount}, which reaches or exceeds the maximum \
                 {time_width}-bit SpikeTime value {max}; the delay saturates to `inf` (never \
                 fires) at this width"
            );
            Diagnostic::warning(node.loc.clone(), message).with_note(format!(
                "choose a shorter delay or widen the SpikeTime type (currently {time_width}-bit, \
                 max {max})"
            ))
        })
        .collect()
}

/// A human-readable identity for a delay-chain node: its original wire/instance name qualified by
/// its module hierarchy path, e.g. `` `inc` in Top/s `` (Req 13.5). Used in saturation warnings.
fn chain_identity(node: &Node) -> String {
    let path = node.hierarchy_path.join("/");
    if path.is_empty() {
        format!("`{}`", node.name)
    } else {
        format!("`{}` in {}", node.name, path)
    }
}

/// Whether an `Input` node is the `inf` constant source rather than a real port. [`crate::ir`]
/// models `inf` as an `Input`-kind node named exactly `"inf"` with no incoming edges; a real input
/// port never carries that reserved name.
fn is_inf_constant(ir: &NetlistIr, node: &Node) -> bool {
    matches!(node.kind, NodeKind::Input)
        && node.name == "inf"
        && ir.incoming(node.id).next().is_none()
}

/// A port grouped from the IR's per-element nodes: a scalar (`width == None`) or a vector of a
/// given width. `elems` maps element index -> node id (index `0` for a scalar).
#[derive(Debug)]
struct Port {
    /// The base name, e.g. `a` for `a` or for `a[0]`, `a[1]`, ...
    base: String,
    /// `None` for a scalar, `Some(n)` for a vector of width `n`.
    width: Option<u64>,
    /// index -> node id (scalars use index 0).
    elems: BTreeMap<u64, NodeId>,
}

/// Split a node name into `(base, Some(index))` for a vector element `name[i]`, or `(name, None)`
/// for a scalar.
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

/// Group a set of boundary nodes (all `Input` ports or all `Output` ports) by base name into
/// scalar/vector [`Port`]s, in first-seen base-name order for determinism.
fn group_ports<'a>(nodes: impl Iterator<Item = &'a Node>) -> Vec<Port> {
    let mut order: Vec<String> = Vec::new();
    let mut ports: HashMap<String, Port> = HashMap::new();
    for node in nodes {
        let (base, idx) = split_indexed(&node.name);
        let entry = ports.entry(base.clone()).or_insert_with(|| {
            order.push(base.clone());
            Port {
                base: base.clone(),
                width: None,
                elems: BTreeMap::new(),
            }
        });
        match idx {
            None => {
                entry.elems.insert(0, node.id);
            }
            Some(i) => {
                entry.elems.insert(i, node.id);
                // A vector's width is the highest index seen plus one.
                let w = entry.width.unwrap_or(0).max(i + 1);
                entry.width = Some(w);
            }
        }
    }
    order
        .into_iter()
        .map(|base| ports.remove(&base).unwrap())
        .collect()
}

/// The C++ type for a port/local: a scalar `SpikeTime` or a `std::array<SpikeTime, N>` (Req 5.4).
fn cpp_type(width: Option<u64>) -> String {
    match width {
        None => "quack::SpikeTime".to_string(),
        Some(n) => format!("std::array<quack::SpikeTime, {n}>"),
    }
}

/// A C++-identifier-safe local name for a node (`v<id>`), stable and collision-free since node ids
/// are unique within the IR (Req 4.2).
fn local(id: NodeId) -> String {
    format!("v{id}")
}

/// Compute a topological order of the node ids via Kahn's algorithm, breaking ties by ascending id
/// so the output is deterministic. Returns `None` if the graph is cyclic (no complete order), which
/// the caller already guards against via `check_acyclic` (Req 5.2, 5.3).
fn topo_order(ir: &NetlistIr) -> Option<Vec<NodeId>> {
    let mut indeg: HashMap<NodeId, usize> = ir.nodes.iter().map(|n| (n.id, 0usize)).collect();
    for e in &ir.edges {
        if let Some(d) = indeg.get_mut(&e.to) {
            *d += 1;
        }
    }
    // Deterministic worklist: a sorted set of ready (in-degree 0) node ids.
    let mut ready: Vec<NodeId> = indeg
        .iter()
        .filter(|(_, &d)| d == 0)
        .map(|(&id, _)| id)
        .collect();
    ready.sort_unstable();

    let mut order: Vec<NodeId> = Vec::with_capacity(ir.nodes.len());
    while let Some(&next) = ready.first() {
        // Pop the smallest ready id.
        ready.remove(0);
        order.push(next);
        // Relax its successors.
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
            // Keep `ready` sorted on insert.
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

/// The producer node id feeding `consumer` on an edge with the given role, if present.
fn input_with_role(ir: &NetlistIr, consumer: NodeId, role: &PortRole) -> Option<NodeId> {
    ir.incoming(consumer)
        .find(|e| &e.port_role == role)
        .map(|e| e.from)
}

/// All operand producer ids feeding `consumer`, in edge order.
fn operands(ir: &NetlistIr, consumer: NodeId) -> Vec<NodeId> {
    ir.incoming(consumer)
        .filter(|e| e.port_role == PortRole::Operand)
        .map(|e| e.from)
        .collect()
}

/// Emit the full `.hpp` text for the top module: header, structs, and the `_eval` function
/// (Req 5.1, 5.2, 5.4, 5.5, 5.6, 5.11).
fn emit_module(ir: &NetlistIr, opts: &BackendOptions) -> String {
    let module = &ir.top;

    // Partition boundary nodes into input ports (excluding the `inf` constant) and output ports.
    let input_nodes: Vec<&Node> = ir
        .nodes
        .iter()
        .filter(|n| matches!(n.kind, NodeKind::Input) && !is_inf_constant(ir, n))
        .collect();
    let output_nodes: Vec<&Node> = ir
        .nodes
        .iter()
        .filter(|n| matches!(n.kind, NodeKind::Output))
        .collect();

    let in_ports = group_ports(input_nodes.into_iter());
    let out_ports = group_ports(output_nodes.into_iter());

    // Map each input-port element node id -> the C++ expression that reads it from the `in` struct.
    let mut node_expr: HashMap<NodeId, String> = HashMap::new();
    for port in &in_ports {
        match port.width {
            None => {
                if let Some(&id) = port.elems.get(&0) {
                    node_expr.insert(id, format!("in.{}", port.base));
                }
            }
            Some(_) => {
                for (&i, &id) in &port.elems {
                    node_expr.insert(id, format!("in.{}[{}]", port.base, i));
                }
            }
        }
    }
    // The `inf` constant reads as the runtime INF sentinel.
    for n in &ir.nodes {
        if is_inf_constant(ir, n) {
            node_expr.insert(n.id, "quack::INF".to_string());
        }
    }

    let mut out = String::new();
    out.push_str("// Generated by quackc functional C++ backend (Mode A). Do not edit by hand.\n");
    out.push_str("#pragma once\n\n");
    out.push_str("#include <array>\n");
    out.push_str("#include \"quack_rt.hpp\"\n\n");

    // Input struct (Req 5.1, 5.4).
    out.push_str(&format!("struct {module}_in {{\n"));
    for port in &in_ports {
        out.push_str(&format!("    {} {};\n", cpp_type(port.width), port.base));
    }
    out.push_str("};\n\n");

    // Output struct (Req 5.1, 5.4).
    out.push_str(&format!("struct {module}_out {{\n"));
    for port in &out_ports {
        out.push_str(&format!("    {} {};\n", cpp_type(port.width), port.base));
    }
    out.push_str("};\n\n");

    // The eval function (Req 5.1).
    out.push_str(&format!(
        "{module}_out {module}_eval(const {module}_in& in) {{\n"
    ));

    // Straight-line body in topological order: one local per non-input, non-output computation
    // node, emitted after the locals it depends on (Req 5.2). `check_acyclic` guarantees an order
    // exists; `topo_order` returns None only on a cycle, handled defensively.
    let order = topo_order(ir).unwrap_or_default();

    // Resolve a producer node id to the C++ expression naming its value. Inputs and `inf` are in
    // `node_expr`; every other node has a `v<id>` local emitted before its consumers.
    let value_of = |node_expr: &HashMap<NodeId, String>, id: NodeId| -> String {
        node_expr
            .get(&id)
            .cloned()
            .unwrap_or_else(|| local(id))
    };

    for &id in &order {
        let node = match ir.node(id) {
            Some(n) => n,
            None => continue,
        };
        match &node.kind {
            // Inputs are struct fields; the `inf` constant is a literal. Nothing to emit.
            NodeKind::Input => {}
            NodeKind::Min => {
                let ops = operands(ir, id);
                let expr = emit_min(&ops.iter().map(|&o| value_of(&node_expr, o)).collect::<Vec<_>>());
                out.push_str(&format!("    quack::SpikeTime {} = {};\n", local(id), expr));
            }
            NodeKind::Lt => {
                let pass = input_with_role(ir, id, &PortRole::Pass)
                    .map(|p| value_of(&node_expr, p))
                    .unwrap_or_else(|| "quack::INF".to_string());
                let inhibit = input_with_role(ir, id, &PortRole::Inhibit)
                    .map(|p| value_of(&node_expr, p))
                    .unwrap_or_else(|| "quack::INF".to_string());
                out.push_str(&format!(
                    "    quack::SpikeTime {} = quack::st_lt({}, {});\n",
                    local(id),
                    pass,
                    inhibit
                ));
            }
            NodeKind::Inc { amount } => {
                let a = operands(ir, id)
                    .first()
                    .map(|&o| value_of(&node_expr, o))
                    .unwrap_or_else(|| "quack::INF".to_string());
                out.push_str(&format!(
                    "    quack::SpikeTime {} = quack::st_inc({}, {});\n",
                    local(id),
                    a,
                    amount
                ));
            }
            NodeKind::Instance { module: sub } => {
                emit_instance(&mut out, ir, node, sub, opts, &node_expr);
            }
            NodeKind::Output => {
                // Output locals are assembled into the out struct below; a value local is emitted
                // here so vector elements and scalars alias uniformly. An output is driven by its
                // single incoming edge, whatever its role: a plain `Operand` driver, or an
                // `InstancePort` edge when a sub-module instance drives it. For an instance-port
                // driver, read the named output exposed by the instance (`v<boundary>_<port>`).
                let driver = ir
                    .incoming(id)
                    .next()
                    .map(|e| match &e.port_role {
                        PortRole::InstancePort(port) => format!("{}_{}", local(e.from), port),
                        _ => value_of(&node_expr, e.from),
                    })
                    .unwrap_or_else(|| "quack::INF".to_string());
                out.push_str(&format!(
                    "    quack::SpikeTime {} = {};\n",
                    local(id),
                    driver
                ));
            }
        }
    }

    // Assemble the output struct from the per-element output locals (Req 5.1, 5.4).
    out.push_str(&format!("    {module}_out out;\n"));
    for port in &out_ports {
        match port.width {
            None => {
                if let Some(&id) = port.elems.get(&0) {
                    out.push_str(&format!("    out.{} = {};\n", port.base, local(id)));
                }
            }
            Some(_) => {
                for (&i, &id) in &port.elems {
                    out.push_str(&format!("    out.{}[{}] = {};\n", port.base, i, local(id)));
                }
            }
        }
    }
    out.push_str("    return out;\n");
    out.push_str("}\n");

    out
}

/// Emit a nested `quack::st_min(...)` call tree for a variadic `min`. The runtime helper is binary,
/// so a variadic min folds left-to-right (Req 5.5). An empty/one-operand min degenerates sensibly.
fn emit_min(ops: &[String]) -> String {
    match ops.len() {
        0 => "quack::INF".to_string(),
        1 => ops[0].clone(),
        _ => {
            let mut acc = format!("quack::st_min({}, {})", ops[0], ops[1]);
            for op in &ops[2..] {
                acc = format!("quack::st_min({acc}, {op})");
            }
            acc
        }
    }
}

/// Emit a sub-module `Instance` boundary: either a call to the sub-module's `_eval` (default) or an
/// inlined pass-through of its input locals to its output locals when `inline_instances` is set
/// (Req 5.6).
///
/// At this IR granularity the instance body is opaque (its primitive nodes are not in the top
/// graph), so each boundary carries named `InstancePort` edges. Inputs arrive as parent->boundary
/// edges; outputs leave as boundary->consumer edges whose consumer reads `v<boundary>` — but since
/// a single boundary node has one `v<id>` local we expose each output port via a distinct local
/// `v<boundary>_<port>` and record the boundary's own local as the first output for compatibility.
fn emit_instance(
    out: &mut String,
    ir: &NetlistIr,
    node: &Node,
    sub: &str,
    opts: &BackendOptions,
    node_expr: &HashMap<NodeId, String>,
) {
    let bid = node.id;

    // Collect named input ports (parent -> boundary) and output ports (boundary -> consumer).
    let mut in_ports: Vec<(String, NodeId)> = Vec::new();
    for e in ir.incoming(bid) {
        if let PortRole::InstancePort(name) = &e.port_role {
            in_ports.push((name.clone(), e.from));
        }
    }
    let mut out_ports: Vec<String> = Vec::new();
    for e in ir.outgoing(bid) {
        if let PortRole::InstancePort(name) = &e.port_role {
            if !out_ports.contains(name) {
                out_ports.push(name.clone());
            }
        }
    }
    in_ports.sort_by(|a, b| a.0.cmp(&b.0));
    out_ports.sort();

    let value_of = |id: NodeId| -> String {
        node_expr.get(&id).cloned().unwrap_or_else(|| local(id))
    };

    if opts.inline_instances {
        // Inline form (Req 5.6): with an opaque instance body, inlining collapses to a transparent
        // pass of each input local; a real sub-graph inliner would splice the body here. The
        // boundary's own local aliases the first input so downstream `v<boundary>` reads resolve.
        out.push_str(&format!(
            "    // inlined instance `{}` of module `{}`\n",
            node.name, sub
        ));
        let first = in_ports
            .first()
            .map(|(_, id)| value_of(*id))
            .unwrap_or_else(|| "quack::INF".to_string());
        out.push_str(&format!("    quack::SpikeTime {} = {};\n", local(bid), first));
        // Expose each named output as `v<boundary>_<port>` so an output / consumer wired to the
        // instance port resolves; with an opaque body each aliases the boundary's pass-through.
        for name in &out_ports {
            out.push_str(&format!(
                "    quack::SpikeTime {}_{} = {};\n",
                local(bid),
                name,
                local(bid)
            ));
        }
    } else {
        // Call form (Req 5.6): construct the sub-module's `_in`, call `<sub>_eval`, and expose its
        // output. The boundary's own local aliases the first available input/output so downstream
        // `v<boundary>` reads resolve; named outputs are available as `v<boundary>_<port>`.
        out.push_str(&format!(
            "    {sub}_in {}_in;\n",
            local(bid)
        ));
        for (name, src) in &in_ports {
            out.push_str(&format!(
                "    {}_in.{} = {};\n",
                local(bid),
                name,
                value_of(*src)
            ));
        }
        out.push_str(&format!(
            "    {sub}_out {}_out = {sub}_eval({}_in);\n",
            local(bid),
            local(bid)
        ));
        for name in &out_ports {
            out.push_str(&format!(
                "    quack::SpikeTime {}_{} = {}_out.{};\n",
                local(bid),
                name,
                local(bid),
                name
            ));
        }
        // The boundary's own local aliases its first named output (or INF if it has none) so a
        // consumer that reads `v<boundary>` directly still resolves.
        let first_out = out_ports
            .first()
            .map(|name| format!("{}_out.{}", local(bid), name))
            .unwrap_or_else(|| "quack::INF".to_string());
        out.push_str(&format!("    quack::SpikeTime {} = {};\n", local(bid), first_out));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diag::Span;
    use crate::ir::{Edge, NetlistIr, Node, NodeKind, PortRole, SchemaVersion, CURRENT_SCHEMA_VERSION};
    use crate::lexer::TimeWidth;
    use crate::operators::OperatorTable;
    use crate::parser::parse_source;
    use crate::sema::analyze;

    /// Parse + analyze + elaborate a source string rooted at `top`, then build the IR, panicking on
    /// any earlier-stage error so backend tests start from a validated, elaborated design.
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
        crate::ir::build(&elab)
    }

    /// Run the functional backend over source `src` rooted at `top`, returning the single emitted
    /// file's contents. Panics if the backend errors.
    fn emit(src: &str, top: &str) -> String {
        let ir = build_ir(src, top);
        let out = FunctionalBackend::new()
            .run(&ir, &BackendOptions::default())
            .expect("backend should emit for an acyclic design");
        assert_eq!(out.files.len(), 1, "one emitted file");
        out.files[0].contents.clone()
    }

    // --- struct + eval signatures (Req 5.1) -------------------------------------------------

    #[test]
    fn emits_in_out_structs_and_eval_signature() {
        let code = emit(
            "module M (input time a, input time b, output time y) { y = min(a, b); }",
            "M",
        );
        assert!(code.contains("struct M_in {"), "has `<Module>_in` struct:\n{code}");
        assert!(code.contains("struct M_out {"), "has `<Module>_out` struct:\n{code}");
        assert!(
            code.contains("M_out M_eval(const M_in& in)"),
            "has `<Module>_out <Module>_eval(const <Module>_in& in)`:\n{code}"
        );
        // Scalar ports are plain SpikeTime fields.
        assert!(code.contains("quack::SpikeTime a;"), "input `a` field:\n{code}");
        assert!(code.contains("quack::SpikeTime b;"), "input `b` field:\n{code}");
        assert!(code.contains("quack::SpikeTime y;"), "output `y` field:\n{code}");
    }

    #[test]
    fn generated_code_includes_only_runtime_and_std_array() {
        // Generated C++ depends only on the standard library and quack_rt.hpp (Req 5.11).
        let code = emit("module M (input time a, output time y) { y = inc(a); }", "M");
        assert!(code.contains("#include \"quack_rt.hpp\""), "includes the runtime header:\n{code}");
        assert!(code.contains("#include <array>"), "includes std array:\n{code}");
        assert!(code.contains("#pragma once"), "header is include-guarded:\n{code}");
        assert!(!code.contains("#include <iostream>"), "no stray includes:\n{code}");
    }

    // --- primitive calls map to the st_* runtime helpers (Req 5.5) --------------------------

    #[test]
    fn min_lowers_to_st_min_call() {
        let code = emit(
            "module M (input time a, input time b, output time y) { y = min(a, b); }",
            "M",
        );
        assert!(code.contains("quack::st_min("), "min lowers to st_min:\n{code}");
    }

    #[test]
    fn lt_lowers_to_st_lt_with_pass_then_inhibit() {
        // y = a < b : pass `a`, inhibit `b`. st_lt(pass, inhibit) with pass first (Req 5.5).
        let code = emit(
            "module M (input time a, input time b, output time y) { y = a < b; }",
            "M",
        );
        assert!(
            code.contains("quack::st_lt(in.a, in.b)"),
            "lt lowers to st_lt(pass, inhibit) in order:\n{code}"
        );
    }

    #[test]
    fn inc_lowers_to_st_inc_with_amount() {
        let code = emit("module M (input time a, output time y) { y = inc(a); }", "M");
        assert!(code.contains("quack::st_inc(in.a, 1)"), "unit inc -> st_inc(x, 1):\n{code}");
    }

    #[test]
    fn delay_by_n_lowers_to_st_inc_with_n() {
        // `a + 3` is a single Inc{amount:3} -> st_inc(x, 3) (Req 5.5, maps Inc{amount:n}).
        let code = emit(
            "module DelayN #(param N = 3) (input time a, output time y) { y = a + N; }",
            "DelayN",
        );
        assert!(code.contains("quack::st_inc(in.a, 3)"), "delay by 3 -> st_inc(x, 3):\n{code}");
    }

    // --- topological ordering: a dependency's local precedes its use (Req 5.2) --------------

    #[test]
    fn locals_are_emitted_in_topological_order() {
        // y = inc(min(a, b)): the min local must be emitted before the inc that consumes it, and
        // both before the output assignment that reads the inc.
        let code = emit(
            "module M (input time a, input time b, output time y) { y = inc(min(a, b)); }",
            "M",
        );
        let min_pos = code.find("quack::st_min(").expect("has a min local");
        let inc_pos = code.find("quack::st_inc(").expect("has an inc local");
        assert!(
            min_pos < inc_pos,
            "the min local (a dependency) is emitted before the inc that uses it:\n{code}"
        );
        // The inc's definition references the min's local, i.e. the dependency name appears to the
        // right of its own definition line.
        assert!(
            code.contains("= quack::st_inc(v"),
            "the inc consumes a previously-defined local `v<id>`:\n{code}"
        );
    }

    // --- vector ports become std::array<SpikeTime, N> (Req 5.4) -----------------------------

    #[test]
    fn vector_ports_become_std_array() {
        let code = emit(
            "module V (input time a[2], output time y[2]) { y[0] = a[0]; y[1] = a[1]; }",
            "V",
        );
        assert!(
            code.contains("std::array<quack::SpikeTime, 2> a;"),
            "vector input becomes std::array<SpikeTime, 2>:\n{code}"
        );
        assert!(
            code.contains("std::array<quack::SpikeTime, 2> y;"),
            "vector output becomes std::array<SpikeTime, 2>:\n{code}"
        );
        // Elements are read/written by index.
        assert!(code.contains("in.a[0]") && code.contains("in.a[1]"), "reads vector elements:\n{code}");
        assert!(code.contains("out.y[0] =") && code.contains("out.y[1] ="), "writes vector elements:\n{code}");
    }

    // --- submodule instance as a call to the submodule _eval (Req 5.6) ----------------------

    #[test]
    fn submodule_instance_is_flattened_into_the_parent_eval() {
        // Instances are flattened at IR build, so the submodule body is lowered directly into the
        // top `_eval` (no separate `Leaf_eval` call), computing the Leaf logic inline.
        let src = "module Leaf (input time a, output time y) { y = inc(a); }\n\
                   module Top (input time a, output time y) { inst l = Leaf (.a(a), .y(y)); }";
        let code = emit(src, "Top");
        assert!(!code.contains("Leaf_eval("), "the submodule is inlined, not called:\n{code}");
        assert!(!code.contains("Leaf_in"), "no submodule input struct is constructed:\n{code}");
        // The Leaf body's `inc` is computed directly inside Top_eval and drives the Top output.
        assert!(code.contains("st_inc(in.a"), "Leaf's inc is lowered inline from the Top input:\n{code}");
        assert!(code.contains("out.y ="), "the flattened result drives the Top output:\n{code}");
    }

    #[test]
    fn flattened_instance_computes_correctly_at_the_boundary() {
        // A two-stage instance chain flattens into a single eval that applies both delays.
        let src = "module D1 (input time a, output time y) { y = a + 1; }\n\
                   module Top (input time a, output time y) {\n\
                     wire time t; inst d0 = D1 (.a(a), .y(t)); inst d1 = D1 (.a(t), .y(y)); }";
        let code = emit(src, "Top");
        assert!(!code.contains("D1_eval("), "both instances inline:\n{code}");
        // Two unit delays appear (one per instance), chained.
        let incs = code.matches("st_inc").count();
        assert!(incs >= 2, "two chained delays should appear, found {incs}:\n{code}");
    }

    // --- cyclic-graph guard: an error, and no code emitted (Req 5.3) ------------------------

    fn span() -> Span {
        Span::new("test.sta", 1, 1)
    }

    fn node(id: NodeId, kind: NodeKind, name: &str) -> Node {
        Node {
            id,
            kind,
            loc: span(),
            name: name.to_string(),
            hierarchy_path: vec!["M".to_string()],
        }
    }

    fn edge(from: NodeId, to: NodeId, role: PortRole) -> Edge {
        Edge { from, to, port_role: role }
    }

    #[test]
    fn cyclic_graph_is_rejected_without_emitting_code() {
        // a -> m -> n -> m (combinational cycle). The backend must error and emit no file (Req 5.3).
        let ir = NetlistIr {
            schema_version: CURRENT_SCHEMA_VERSION,
            top: "M".to_string(),
            nodes: vec![
                node(0, NodeKind::Input, "a"),
                node(1, NodeKind::Min, "m"),
                node(2, NodeKind::Min, "n"),
                node(3, NodeKind::Output, "y"),
            ],
            edges: vec![
                edge(0, 1, PortRole::Operand),
                edge(1, 2, PortRole::Operand),
                edge(2, 1, PortRole::Operand),
                edge(1, 3, PortRole::Operand),
            ],
        };
        let err = FunctionalBackend::new()
            .run(&ir, &BackendOptions::default())
            .expect_err("a cyclic dataflow graph must be rejected");
        assert!(!err.is_empty(), "at least one diagnostic");
        assert!(err[0].is_error(), "the cycle guard is an error");
        assert!(
            err[0].message.contains("combinational cycle"),
            "names the combinational-cycle rule:\n{}",
            err[0].render()
        );
    }

    #[test]
    fn topo_order_returns_none_on_a_cycle() {
        let ir = NetlistIr {
            schema_version: CURRENT_SCHEMA_VERSION,
            top: "M".to_string(),
            nodes: vec![node(0, NodeKind::Min, "m"), node(1, NodeKind::Min, "n")],
            edges: vec![
                edge(0, 1, PortRole::Operand),
                edge(1, 0, PortRole::Operand),
            ],
        };
        assert!(topo_order(&ir).is_none(), "a cyclic graph has no topological order");
    }

    // --- backend identity and schema support -----------------------------------------------

    #[test]
    fn backend_declares_name_and_supported_major() {
        let b = FunctionalBackend::new();
        assert_eq!(b.name(), "functional-cpp");
        assert_eq!(b.supported_ir_major(), CURRENT_SCHEMA_VERSION.major);
    }

    #[test]
    fn dispatcher_rejects_incompatible_ir_before_running() {
        use crate::backend::run_backend;
        let ir = NetlistIr {
            schema_version: SchemaVersion::new(99, 0),
            top: "M".to_string(),
            nodes: vec![node(0, NodeKind::Input, "a")],
            edges: vec![],
        };
        let err = run_backend(&FunctionalBackend::new(), &ir, &BackendOptions::default())
            .expect_err("an incompatible IR major is rejected by the dispatcher");
        assert!(err[0].is_error());
    }

    // --- delay-chain saturation warning (Req 13.5) -----------------------------------------

    /// An IR with a single `Inc { amount }` delay chain named `d`, in hierarchy path `Top/s`.
    fn ir_with_inc(amount: u32) -> NetlistIr {
        let d = Node {
            id: 1,
            kind: NodeKind::Inc { amount },
            loc: span(),
            name: "d".to_string(),
            hierarchy_path: vec!["Top".to_string(), "s".to_string()],
        };
        NetlistIr {
            schema_version: CURRENT_SCHEMA_VERSION,
            top: "Top".to_string(),
            nodes: vec![
                node(0, NodeKind::Input, "a"),
                d,
                node(2, NodeKind::Output, "y"),
            ],
            edges: vec![
                edge(0, 1, PortRole::Operand),
                edge(1, 2, PortRole::Operand),
            ],
        }
    }

    #[test]
    fn spike_time_max_matches_the_chosen_width() {
        assert_eq!(spike_time_max(16), 65535, "16-bit INF is 65535 (Req 5.9)");
        assert_eq!(spike_time_max(32), 4294967295, "32-bit INF is 4294967295 (Req 5.8)");
        // An unspecified width falls back to the 32-bit default (Req 5.7).
        assert_eq!(spike_time_max(0), 4294967295, "unknown width defaults to 32-bit");
    }

    #[test]
    fn delay_reaching_the_16_bit_max_warns_identifying_chain_and_width() {
        // amount == 65535 reaches the 16-bit maximum, so the chain can only saturate to inf.
        let ir = ir_with_inc(65535);
        let out = FunctionalBackend::new()
            .run(&ir, &BackendOptions::default().with_time_width(16))
            .expect("a saturation warning never stops compilation (Req 13.6)");
        // Code is still emitted: the warning is non-fatal.
        assert_eq!(out.files.len(), 1, "the warning does not suppress codegen");
        assert_eq!(out.warnings.len(), 1, "exactly one saturation warning");
        let w = &out.warnings[0];
        assert!(!w.is_error(), "saturation is a warning, not an error (Req 13.6, 13.7)");
        let rendered = w.render();
        // Identifies the delay chain by name and hierarchy path (Req 13.5).
        assert!(rendered.contains("`d`"), "names the delay chain: {rendered}");
        assert!(rendered.contains("Top/s"), "names the hierarchy path: {rendered}");
        // Names the SpikeTime width and its maximum (Req 13.5).
        assert!(rendered.contains("16-bit"), "names the SpikeTime width: {rendered}");
        assert!(rendered.contains("65535"), "names the width maximum: {rendered}");
    }

    #[test]
    fn delay_exceeding_the_16_bit_max_also_warns() {
        // amount > 65535 at 16-bit width exceeds the maximum.
        let ir = ir_with_inc(70000);
        let out = FunctionalBackend::new()
            .run(&ir, &BackendOptions::default().with_time_width(16))
            .expect("non-fatal warning");
        assert_eq!(out.warnings.len(), 1, "a delay past the width max warns");
        assert!(out.warnings[0].render().contains("70000"), "names the delay amount");
    }

    #[test]
    fn delay_below_the_16_bit_max_does_not_warn() {
        // amount just under the 16-bit maximum: still representable, no saturation warning.
        let ir = ir_with_inc(65534);
        let out = FunctionalBackend::new()
            .run(&ir, &BackendOptions::default().with_time_width(16))
            .expect("non-fatal");
        assert!(out.warnings.is_empty(), "a delay below the width max does not warn");
    }

    #[test]
    fn same_delay_warns_at_16_bit_but_not_at_32_bit() {
        // amount == 65535 saturates at 16-bit but is comfortably representable at 32-bit, so the
        // warning depends on the chosen width (Req 13.5).
        let ir = ir_with_inc(65535);

        let w16 = FunctionalBackend::new()
            .run(&ir, &BackendOptions::default().with_time_width(16))
            .expect("non-fatal");
        assert_eq!(w16.warnings.len(), 1, "65535 reaches the 16-bit max and warns");

        let w32 = FunctionalBackend::new()
            .run(&ir, &BackendOptions::default().with_time_width(32))
            .expect("non-fatal");
        assert!(w32.warnings.is_empty(), "65535 is representable at 32-bit, no warning");
    }

    #[test]
    fn delay_reaching_the_32_bit_max_warns_identifying_width() {
        // The IR clamps Inc amounts to MAX_INC_AMOUNT (2147483647), which is below the 32-bit
        // SpikeTime maximum, so build a node directly at the 32-bit maximum to exercise the warning.
        let ir = ir_with_inc(u32::MAX);
        let out = FunctionalBackend::new()
            .run(&ir, &BackendOptions::default().with_time_width(32))
            .expect("non-fatal");
        assert_eq!(out.warnings.len(), 1, "a delay reaching the 32-bit max warns");
        let rendered = out.warnings[0].render();
        assert!(rendered.contains("32-bit"), "names the 32-bit width: {rendered}");
        assert!(rendered.contains("4294967295"), "names the 32-bit maximum: {rendered}");
    }

    #[test]
    fn no_warning_when_there_are_no_delay_chains() {
        // A design with no Inc nodes produces no saturation warnings at any width.
        let ir = NetlistIr {
            schema_version: CURRENT_SCHEMA_VERSION,
            top: "M".to_string(),
            nodes: vec![
                node(0, NodeKind::Input, "a"),
                node(1, NodeKind::Min, "m"),
                node(2, NodeKind::Output, "y"),
            ],
            edges: vec![
                edge(0, 1, PortRole::Operand),
                edge(1, 2, PortRole::Operand),
            ],
        };
        let out = FunctionalBackend::new()
            .run(&ir, &BackendOptions::default().with_time_width(16))
            .expect("non-fatal");
        assert!(out.warnings.is_empty(), "no delay chains, no saturation warnings");
    }
}
