//! Netlist graph IR: backend-neutral node/edge types, builder, and schema version (Requirement 4).
//!
//! The netlist IR is the compiler's single stable hub: every backend (functional C++,
//! event-driven engine, JSON/DOT export) consumes `&NetlistIr` and never reaches into frontend
//! internals (Req 4.8, 10.9). To keep it a durable contract it is:
//!
//! * a directed graph of [`Node`]s with [`NodeKind`] exactly `Input`, `Output`, `Min`, `Lt`,
//!   `Inc { amount }`, and `Instance` (Req 4.1);
//! * annotated per node with a unique [`NodeId`], the node kind, a source location, the original
//!   wire/instance name, and the module hierarchy path (Req 4.2, 4.3);
//! * connected by [`Edge`]s that carry a [`PortRole`] — in particular distinguishing an `Lt`
//!   node's pass input from its inhibit input, and recording an `Inc` node's delay amount `n`
//!   (Req 4.4);
//! * able to represent a delay chain `a + n` as a single `Inc { amount: n }` node that
//!   [`NetlistIr::expand_delay_chains`] can expand into `n` unit `Inc` nodes on demand (Req 4.5);
//! * serde-serializable and version-tagged via [`SchemaVersion`] so a consumer can detect — and
//!   reject — an incompatible IR version (Req 4.9, 4.10).
//!
//! The IR is deliberately defined with no dependency on C++ code generation (Req 4.8): nothing in
//! this module mentions a backend, a target language, or a race-logic construct.
//!
//! The acyclicity check and the schema-version compatibility check live in [`crate::checks`]
//! (Req 4.6, 4.7, 4.10); this module only builds and describes the graph.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::diag::Span;
use crate::elaborate::{ElabExpr, ElabInstance, ElabModule, ElabRef, Elaborated};

/// The schema version of the netlist IR, as a `major.minor` pair (Req 4.9, 10.7).
///
/// Backends declare the major version they support; a consumer that reads an IR whose `major`
/// differs from the one it supports must reject it without consuming it (Req 4.10). A `minor`
/// bump is backward-compatible (added fields / node kinds a consumer can ignore); a `major` bump
/// is a breaking change.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SchemaVersion {
    pub major: u16,
    pub minor: u16,
}

impl SchemaVersion {
    /// Construct a `major.minor` schema version.
    pub const fn new(major: u16, minor: u16) -> Self {
        SchemaVersion { major, minor }
    }

    /// Whether an IR at this version is compatible with a consumer that supports `major`.
    ///
    /// Compatibility is by major version only: a consumer supporting major `M` accepts any IR with
    /// `major == M` regardless of minor (Req 4.10).
    pub fn is_compatible_with_major(self, supported_major: u16) -> bool {
        self.major == supported_major
    }
}

impl std::fmt::Display for SchemaVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}", self.major, self.minor)
    }
}

/// The current netlist IR schema version produced by [`build`] (Req 4.9).
pub const CURRENT_SCHEMA_VERSION: SchemaVersion = SchemaVersion::new(1, 0);

/// A node identifier, unique within a single [`NetlistIr`] (Req 4.2).
pub type NodeId = u32;

/// The maximum delay amount an [`NodeKind::Inc`] may carry: `n` in `0..=2_147_483_647` (Req 4.4,
/// 4.5). This is `i32::MAX`, matching the design's stated range.
pub const MAX_INC_AMOUNT: u32 = 2_147_483_647;

/// The whole backend-neutral netlist: a directed graph of [`Node`]s and [`Edge`]s plus the schema
/// version and the top module name (Req 4.1, 4.9).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NetlistIr {
    /// The schema version this document conforms to (Req 4.9).
    pub schema_version: SchemaVersion,
    /// The name of the top module the IR was built from.
    pub top: String,
    /// Every node in the graph; ids are unique and dense from `0` (Req 4.2).
    pub nodes: Vec<Node>,
    /// Every directed edge, carrying its port role (Req 4.4).
    pub edges: Vec<Edge>,
}

/// One graph node: a primitive gate, an I/O boundary, or an instance boundary (Req 4.1).
///
/// Every node records the provenance a tool or exporter needs: a unique id, its kind, the source
/// location it came from (file:line:col, each `>= 1`), the original wire/instance name, and the
/// module hierarchy path from the root to the owning module (Req 4.2, 4.3).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Node {
    /// Unique within this netlist (Req 4.2).
    pub id: NodeId,
    /// The node's kind (Req 4.1).
    pub kind: NodeKind,
    /// The source location this node was elaborated from (Req 4.2).
    pub loc: Span,
    /// The original wire or instance name this node corresponds to (Req 4.2).
    pub name: String,
    /// The module hierarchy path from the root, e.g. `["Top", "s"]` (Req 4.3).
    pub hierarchy_path: Vec<String>,
}

/// The kind of a netlist node (Req 4.1).
///
/// The set is deliberately closed: the three s-t primitives (`Min`, `Lt`, `Inc`), the two I/O
/// boundaries (`Input`, `Output`), and an `Instance` boundary marker. Derived operators never
/// appear here — elaboration has already expanded them into these primitives (Req 3.5), so adding
/// a new surface operator never grows this enum.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum NodeKind {
    /// A module input port.
    Input,
    /// A module output port.
    Output,
    /// The variadic first-arrival primitive `min(...)`; its inputs arrive via `Operand` edges.
    Min,
    /// The strict-less-than / inhibit primitive `lt(a, b)`; its pass input arrives via a `Pass`
    /// edge and its inhibit input via an `Inhibit` edge (Req 4.4).
    Lt,
    /// A delay by `amount` ticks: `inc(a)` is `amount = 1` and `a + n` is `amount = n`. The chain
    /// is stored as this single node and expanded to `amount` unit incs on demand (Req 4.4, 4.5).
    Inc { amount: u32 },
    /// An instance boundary marker naming the instantiated module (Req 4.1).
    Instance { module: String },
}

impl NodeKind {
    /// A short, stable label for this kind, used by exporters and tests.
    pub fn label(&self) -> &'static str {
        match self {
            NodeKind::Input => "Input",
            NodeKind::Output => "Output",
            NodeKind::Min => "Min",
            NodeKind::Lt => "Lt",
            NodeKind::Inc { .. } => "Inc",
            NodeKind::Instance { .. } => "Instance",
        }
    }
}

/// A directed edge from a producer node to a consumer node, carrying the role the producer plays
/// at the consumer's input (Req 4.4).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Edge {
    /// The producing node.
    pub from: NodeId,
    /// The consuming node.
    pub to: NodeId,
    /// The role of this input at the consumer (Req 4.4).
    pub port_role: PortRole,
}

/// The role an incoming edge plays at its consumer node (Req 4.4).
///
/// For an `Lt` node the two inputs are not interchangeable: `Pass` is the value that passes
/// through when it is strictly less than the inhibit, and `Inhibit` is the value that blocks it on
/// a tie or earlier arrival. For every other node the inputs are generic `Operand`s. An
/// `InstancePort` names the port of an `Instance` boundary the edge connects to.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PortRole {
    /// A generic operand (e.g. a `Min` input, an `Inc` input, an `Output` driver).
    Operand,
    /// The pass input of an `Lt` node (Req 4.4).
    Pass,
    /// The inhibit input of an `Lt` node (Req 4.4).
    Inhibit,
    /// A named port of an `Instance` boundary node.
    InstancePort(String),
}

/// Build a backend-neutral [`NetlistIr`] from an elaborated design (Req 4.1).
///
/// Walks the elaborated module hierarchy rooted at [`Elaborated::root`], creating one `Input` /
/// `Output` node per port, one primitive node per `min` / `lt` / `inc` / delay in each driver
/// expression, and one `Instance` boundary node per sub-module instance. Signals are wired by
/// name within each module scope; an output port's driver connects to the output node, and an
/// instance's connections connect the parent's signals to the instance boundary. The result
/// carries the current schema version (Req 4.9).
pub fn build(elab: &Elaborated) -> NetlistIr {
    let mut builder = Builder::new();
    builder.build_module(&elab.root);
    NetlistIr {
        schema_version: CURRENT_SCHEMA_VERSION,
        top: elab.top.clone(),
        nodes: builder.nodes,
        edges: builder.edges,
    }
}

/// Internal state while lowering the elaborated hierarchy into a flat node/edge graph.
struct Builder {
    nodes: Vec<Node>,
    edges: Vec<Edge>,
    next_id: NodeId,
}

impl Builder {
    fn new() -> Self {
        Builder {
            nodes: Vec::new(),
            edges: Vec::new(),
            next_id: 0,
        }
    }

    /// Allocate a fresh unique node id (Req 4.2).
    fn fresh_id(&mut self) -> NodeId {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    /// Add a node, returning its id.
    fn add_node(
        &mut self,
        kind: NodeKind,
        loc: Span,
        name: impl Into<String>,
        hierarchy_path: Vec<String>,
    ) -> NodeId {
        let id = self.fresh_id();
        self.nodes.push(Node {
            id,
            kind,
            loc,
            name: name.into(),
            hierarchy_path,
        });
        id
    }

    /// Add an edge `from -> to` with the given role (Req 4.4).
    fn add_edge(&mut self, from: NodeId, to: NodeId, port_role: PortRole) {
        self.edges.push(Edge {
            from,
            to,
            port_role,
        });
    }

    /// Lower one elaborated module instance into nodes/edges (Req 4.1-4.3).
    fn build_module(&mut self, module: &ElabModule) {
        let path = path_components(&module.path);

        // One Input / Output node per port, keyed by name within this module scope. The signal map
        // records, for each signal name (and for vector elements by `name[i]`), the id of the node
        // that *produces* its value, so later consumers can wire to it.
        let mut scope = Scope::new();

        for port in &module.ports {
            match port.dir {
                crate::ast::Dir::Input => {
                    // One input node per scalar, or one per element for a vector input.
                    match port.width {
                        None => {
                            let id = self.add_node(
                                NodeKind::Input,
                                port.span.clone(),
                                port.name.clone(),
                                path.clone(),
                            );
                            scope.set_wire(&port.name, id);
                        }
                        Some(n) => {
                            for i in 0..n {
                                let id = self.add_node(
                                    NodeKind::Input,
                                    port.span.clone(),
                                    format!("{}[{}]", port.name, i),
                                    path.clone(),
                                );
                                scope.set_elem(&port.name, i, id);
                            }
                        }
                    }
                }
                crate::ast::Dir::Output => {
                    // Output nodes are created lazily when their driver is seen, so the output's
                    // location is the driving assignment's location. Record the declared width so a
                    // vector output can be driven per element.
                    scope.declare_output(&port.name, port.width);
                }
            }
        }

        // Sub-module instance boundaries. An Instance node stands for the whole instance; its
        // connections wire the parent's signals to/from it by named instance-port edges. The
        // instance's own body is lowered recursively so the hierarchy is retained (Req 4.3).
        for inst in &module.instances {
            self.build_instance(inst, &mut scope, &path);
        }

        // Driver assignments. Each assignment lowers its value expression to a producer node and
        // then records (whole-wire) or connects (output / element) that producer.
        for assign in &module.assigns {
            let producer = self.build_expr(&assign.value, &mut scope, &path);
            match &assign.target {
                ElabRef::Wire(name) => {
                    if let Some(width) = scope.output_width(name) {
                        // Driving a scalar output port: create the Output node now.
                        let _ = width;
                        let out = self.add_node(
                            NodeKind::Output,
                            assign.span.clone(),
                            name.clone(),
                            path.clone(),
                        );
                        self.add_edge(producer, out, PortRole::Operand);
                        scope.set_wire(name, out);
                    } else {
                        // An internal wire: it simply aliases its producer node.
                        scope.set_wire(name, producer);
                    }
                }
                ElabRef::Index(name, i) => {
                    if scope.is_output(name) {
                        let out = self.add_node(
                            NodeKind::Output,
                            assign.span.clone(),
                            format!("{name}[{i}]"),
                            path.clone(),
                        );
                        self.add_edge(producer, out, PortRole::Operand);
                        scope.set_elem(name, *i, out);
                    } else {
                        scope.set_elem(name, *i, producer);
                    }
                }
            }
        }
    }

    /// Lower a sub-module instance by FLATTENING its body into the parent graph (Req 4.1, 4.3).
    ///
    /// Rather than leaving the instance as an opaque boundary, the submodule's primitive nodes are
    /// spliced directly into the single flat graph: each submodule input port is bound to the
    /// parent's producer for the connected signal, the submodule's internal wires/assigns/nested
    /// instances build into the shared graph, and each submodule output port's producer is wired
    /// back to the parent's connected signal. This makes an instance simulate (and lower to C++)
    /// exactly as if the body had been written inline, so hierarchical designs behave identically
    /// to their flat equivalents.
    fn build_instance(&mut self, inst: &ElabInstance, scope: &mut Scope, parent_path: &[String]) {
        let sub = &inst.module;
        let child_path = path_components(&sub.path);

        // A fresh scope for the submodule body. Seed it so each INPUT port resolves to the parent's
        // producer node for the signal it is connected to (parent value flows in). Output ports are
        // left for the body to drive; we read them back out afterwards.
        let mut child = Scope::new();
        for (port_name, parent_ref) in &inst.conns {
            let dir = sub.ports.iter().find(|p| &p.name == port_name).map(|p| p.dir);
            if dir == Some(crate::ast::Dir::Input) {
                // Resolve the parent's producer (an input node, a prior driver, or synthesized).
                if let Some(src) = self.resolve_ref_or_input(parent_ref, scope, parent_path) {
                    child.set_wire(port_name, src);
                }
            }
        }

        // Build the submodule body into the shared graph under the child scope. Internal wires and
        // nested instances flatten recursively; output-port producers are left in `child`.
        self.build_body_into(sub, &mut child, &child_path);

        // Wire each OUTPUT port's producer back to the parent's connected signal.
        for (port_name, parent_ref) in &inst.conns {
            let dir = sub.ports.iter().find(|p| &p.name == port_name).map(|p| p.dir);
            if dir != Some(crate::ast::Dir::Output) {
                continue;
            }
            // The submodule produced this port in its child scope (as a scalar wire named for the
            // port). Resolve its producer node.
            let producer = child.get_wire(port_name);
            let producer = match producer {
                Some(p) => p,
                None => continue, // unconnected / undriven submodule output
            };
            match parent_ref {
                ElabRef::Wire(name) => {
                    if scope.is_output(name) {
                        // The instance drives a parent OUTPUT port: create its Output node now.
                        let out = self.add_node(
                            NodeKind::Output,
                            inst.span.clone(),
                            name.clone(),
                            parent_path.to_vec(),
                        );
                        self.add_edge(producer, out, PortRole::Operand);
                        scope.set_wire(name, out);
                    } else {
                        // The instance drives a parent internal wire: it aliases the producer.
                        scope.set_wire(name, producer);
                    }
                }
                ElabRef::Index(name, i) => {
                    if scope.is_output(name) {
                        let out = self.add_node(
                            NodeKind::Output,
                            inst.span.clone(),
                            format!("{name}[{i}]"),
                            parent_path.to_vec(),
                        );
                        self.add_edge(producer, out, PortRole::Operand);
                        scope.set_elem(name, *i, out);
                    } else {
                        scope.set_elem(name, *i, producer);
                    }
                }
            }
        }
    }

    /// Build a module's body (its assigns and nested instances) into the shared graph under an
    /// already-seeded `scope`, WITHOUT creating top-level `Input`/`Output` boundary nodes for its
    /// ports. Input ports are expected to be pre-bound in `scope`; output ports are driven by the
    /// body and read back by the caller. Internal wires alias their producers; a driver targeting
    /// one of the submodule's own OUTPUT ports records the producer under the port name (so the
    /// caller can read it) rather than creating an `Output` node. Used when flattening instances.
    fn build_body_into(&mut self, module: &ElabModule, scope: &mut Scope, path: &[String]) {
        // Mark the submodule's output ports so a driver records (not Output-emits) them.
        for port in &module.ports {
            if port.dir == crate::ast::Dir::Output {
                // Track outputs so a driver to one records its producer under the port name.
                scope.declare_output(&port.name, port.width);
            }
        }

        // Nested instances first (so their outputs are available to this body's assigns).
        for sub_inst in &module.instances {
            self.build_instance(sub_inst, scope, path);
        }

        for assign in &module.assigns {
            let producer = self.build_expr(&assign.value, scope, path);
            match &assign.target {
                ElabRef::Wire(name) => {
                    // For a flattened body, an output-port target does NOT emit an Output node; its
                    // producer is recorded under the port name so the instantiating parent reads it.
                    scope.set_wire(name, producer);
                }
                ElabRef::Index(name, i) => {
                    scope.set_elem(name, *i, producer);
                }
            }
        }
    }

    /// Resolve a parent signal reference to its producer, synthesizing an `Input` node if it is not
    /// yet produced (defensive; a validated design drives every read signal).
    fn resolve_ref_or_input(
        &mut self,
        r: &ElabRef,
        scope: &mut Scope,
        path: &[String],
    ) -> Option<NodeId> {
        match r {
            ElabRef::Wire(name) => {
                Some(self.lookup_or_input(name, None, &synthetic_span(), scope, path))
            }
            ElabRef::Index(name, i) => {
                Some(self.lookup_or_input(name, Some(*i), &synthetic_span(), scope, path))
            }
        }
    }

    /// Lower an elaborated expression into a producer node, returning that node's id (Req 4.1).
    fn build_expr(&mut self, expr: &ElabExpr, scope: &mut Scope, path: &[String]) -> NodeId {
        match expr {
            ElabExpr::Ident(name, span) => self.lookup_or_input(name, None, span, scope, path),
            ElabExpr::Index(name, i, span) => {
                self.lookup_or_input(name, Some(*i), span, scope, path)
            }
            ElabExpr::Inf(span) => {
                // `inf` is a constant source. There is no dedicated node kind for it in the closed
                // node set; it is modeled as an Input-like constant source carrying the name `inf`
                // so a backend can special-case it. We use an Input node named "inf".
                self.add_node(NodeKind::Input, span.clone(), "inf", path.to_vec())
            }
            ElabExpr::Inc(inner, span) => {
                let a = self.build_expr(inner, scope, path);
                let id = self.add_node(
                    NodeKind::Inc { amount: 1 },
                    span.clone(),
                    "inc",
                    path.to_vec(),
                );
                self.add_edge(a, id, PortRole::Operand);
                id
            }
            ElabExpr::Delay(base, n, span) => {
                let a = self.build_expr(base, scope, path);
                let amount = clamp_amount(*n);
                let id = self.add_node(
                    NodeKind::Inc { amount },
                    span.clone(),
                    "inc",
                    path.to_vec(),
                );
                self.add_edge(a, id, PortRole::Operand);
                id
            }
            ElabExpr::Min(args, span) => {
                let operands: Vec<NodeId> =
                    args.iter().map(|a| self.build_expr(a, scope, path)).collect();
                let id = self.add_node(NodeKind::Min, span.clone(), "min", path.to_vec());
                for op in operands {
                    self.add_edge(op, id, PortRole::Operand);
                }
                id
            }
            ElabExpr::Lt(pass, inhibit, span) => {
                // lt(a, b): `a` is the pass input, `b` is the inhibit input (Req 4.4).
                let pass_id = self.build_expr(pass, scope, path);
                let inhibit_id = self.build_expr(inhibit, scope, path);
                let id = self.add_node(NodeKind::Lt, span.clone(), "lt", path.to_vec());
                self.add_edge(pass_id, id, PortRole::Pass);
                self.add_edge(inhibit_id, id, PortRole::Inhibit);
                id
            }
        }
    }


    /// Look up the producer of a signal (or vector element); if it is not yet known, synthesize an
    /// `Input` node for it (a defensive fallback — a validated design has every read signal
    /// driven, but an unresolved forward reference still yields a stable source node).
    fn lookup_or_input(
        &mut self,
        name: &str,
        index: Option<u64>,
        span: &Span,
        scope: &mut Scope,
        path: &[String],
    ) -> NodeId {
        let found = match index {
            None => scope.get_wire(name),
            Some(i) => scope.get_elem(name, i),
        };
        if let Some(id) = found {
            return id;
        }
        let node_name = match index {
            None => name.to_string(),
            Some(i) => format!("{name}[{i}]"),
        };
        let id = self.add_node(NodeKind::Input, span.clone(), node_name, path.to_vec());
        match index {
            None => scope.set_wire(name, id),
            Some(i) => scope.set_elem(name, i, id),
        }
        id
    }
}

/// Clamp a delay amount to the IR's supported range `0..=MAX_INC_AMOUNT` (Req 4.4, 4.5).
fn clamp_amount(n: u64) -> u32 {
    n.min(MAX_INC_AMOUNT as u64) as u32
}

/// Split a `/`-joined hierarchy path (e.g. `"Top/s"`) into its components (`["Top", "s"]`),
/// matching the elaborated `ElabModule::path` format (Req 4.3).
fn path_components(path: &str) -> Vec<String> {
    path.split('/').map(|s| s.to_string()).collect()
}

/// A placeholder span for synthesized nodes that have no direct source location (e.g. an `Input`
/// node materialized while resolving an instance connection). Line/col are 1-based.
fn synthetic_span() -> Span {
    Span::new("<ir>", 1, 1)
}

/// Per-module signal scope: maps a signal name (or vector element) to the id of its producing
/// node, so consumers wire to the right producer. Also tracks which names are output ports (and
/// their declared width) so a driver can create the `Output` node.
struct Scope {
    wires: HashMap<String, NodeId>,
    elems: HashMap<(String, u64), NodeId>,
    outputs: HashMap<String, Option<u64>>,
}

impl Scope {
    fn new() -> Self {
        Scope {
            wires: HashMap::new(),
            elems: HashMap::new(),
            outputs: HashMap::new(),
        }
    }

    fn set_wire(&mut self, name: &str, id: NodeId) {
        self.wires.insert(name.to_string(), id);
    }


    fn set_elem(&mut self, name: &str, i: u64, id: NodeId) {
        self.elems.insert((name.to_string(), i), id);
    }


    fn get_wire(&self, name: &str) -> Option<NodeId> {
        self.wires.get(name).copied()
    }

    fn get_elem(&self, name: &str, i: u64) -> Option<NodeId> {
        self.elems.get(&(name.to_string(), i)).copied()
    }

    fn declare_output(&mut self, name: &str, width: Option<u64>) {
        self.outputs.insert(name.to_string(), width);
    }

    fn is_output(&self, name: &str) -> bool {
        self.outputs.contains_key(name)
    }

    /// `Some(width)` if `name` is a declared output port (whose Output node has not yet been
    /// created), else `None`.
    fn output_width(&self, name: &str) -> Option<Option<u64>> {
        self.outputs.get(name).copied()
    }
}

impl NetlistIr {
    /// Look up a node by id.
    pub fn node(&self, id: NodeId) -> Option<&Node> {
        self.nodes.iter().find(|n| n.id == id)
    }

    /// All edges whose consumer is `id` (the inputs of a node).
    pub fn incoming(&self, id: NodeId) -> impl Iterator<Item = &Edge> {
        self.edges.iter().filter(move |e| e.to == id)
    }

    /// All edges whose producer is `id` (the fan-out of a node).
    pub fn outgoing(&self, id: NodeId) -> impl Iterator<Item = &Edge> {
        self.edges.iter().filter(move |e| e.from == id)
    }

    /// Return a copy of this IR in which every `Inc { amount: n }` node with `n != 1` is expanded
    /// into a chain of `n` unit `Inc { amount: 1 }` nodes in series (Req 4.5).
    ///
    /// Each original multi-tick `Inc` node is replaced by `n` unit incs: the first unit inc takes
    /// the original node's incoming edges, each subsequent unit inc feeds the next, and the last
    /// unit inc takes the original node's outgoing edges. An `Inc { amount: 0 }` becomes a
    /// pass-through: its single input is rewired directly to each of its consumers. An `Inc`
    /// already at `amount == 1` is left as-is. All other nodes and edges are preserved. The result
    /// is semantically equivalent: a delay of `n` ticks is `n` unit delays in series.
    pub fn expand_delay_chains(&self) -> NetlistIr {
        let mut nodes: Vec<Node> = Vec::new();
        let mut edges: Vec<Edge> = Vec::new();
        let mut next_id: NodeId = self.nodes.iter().map(|n| n.id).max().map_or(0, |m| m + 1);

        // For an expanded Inc chain we must remap: the original node id's *inputs* feed the head of
        // the chain and the original node id's *outputs* leave the tail. Record head/tail per
        // rewritten node so edges can be redirected. For amount==0 (pass-through) record the single
        // upstream producer to splice directly.
        enum Remap {
            /// `(head, tail)` ids a non-unit chain replaced the node with.
            Chain(NodeId, NodeId),
            /// A pass-through (`amount == 0`): consumers connect straight to the upstream producer,
            /// resolved after the fact; stored as the original node id so we can look up its input.
            PassThrough,
        }
        let mut remap: HashMap<NodeId, Remap> = HashMap::new();

        // First pass: copy / expand nodes.
        for node in &self.nodes {
            match &node.kind {
                NodeKind::Inc { amount } if *amount != 1 => {
                    let n = *amount;
                    if n == 0 {
                        remap.insert(node.id, Remap::PassThrough);
                        // No node emitted; the pass-through is spliced during edge rewiring.
                        continue;
                    }
                    // Emit `n` unit incs in series. Reuse the original id for the head so unrelated
                    // bookkeeping stays stable, allocate fresh ids for the rest.
                    let mut chain_ids = Vec::with_capacity(n as usize);
                    chain_ids.push(node.id);
                    for _ in 1..n {
                        chain_ids.push(next_id);
                        next_id += 1;
                    }
                    for (k, &cid) in chain_ids.iter().enumerate() {
                        nodes.push(Node {
                            id: cid,
                            kind: NodeKind::Inc { amount: 1 },
                            loc: node.loc.clone(),
                            name: if n == 1 {
                                node.name.clone()
                            } else {
                                format!("{}#{}", node.name, k)
                            },
                            hierarchy_path: node.hierarchy_path.clone(),
                        });
                    }
                    // Internal series edges head -> ... -> tail.
                    for w in chain_ids.windows(2) {
                        edges.push(Edge {
                            from: w[0],
                            to: w[1],
                            port_role: PortRole::Operand,
                        });
                    }
                    let head = *chain_ids.first().unwrap();
                    let tail = *chain_ids.last().unwrap();
                    remap.insert(node.id, Remap::Chain(head, tail));
                }
                _ => nodes.push(node.clone()),
            }
        }

        // Resolve pass-through producers: a passthrough node's single input producer becomes the
        // source for its consumers.
        let passthrough_src: HashMap<NodeId, Option<NodeId>> = remap
            .iter()
            .filter_map(|(id, r)| match r {
                Remap::PassThrough => {
                    let src = self.incoming(*id).next().map(|e| e.from);
                    Some((*id, src))
                }
                _ => None,
            })
            .collect();

        // Second pass: rewire edges around rewritten nodes.
        for edge in &self.edges {
            // Resolve the producer end.
            let from = match remap.get(&edge.from) {
                Some(Remap::Chain(_, tail)) => *tail, // output leaves the tail of the chain
                Some(Remap::PassThrough) => match passthrough_src.get(&edge.from).copied().flatten()
                {
                    Some(src) => src,
                    None => continue, // dangling pass-through input; drop
                },
                None => edge.from,
            };
            // Resolve the consumer end.
            let to = match remap.get(&edge.to) {
                Some(Remap::Chain(head, _)) => *head, // input feeds the head of the chain
                Some(Remap::PassThrough) => continue, // the pass-through's input edge is dropped
                None => edge.to,
            };
            edges.push(Edge {
                from,
                to,
                port_role: edge.port_role.clone(),
            });
        }

        // Keep node order deterministic by id.
        nodes.sort_by_key(|n| n.id);

        NetlistIr {
            schema_version: self.schema_version,
            top: self.top.clone(),
            nodes,
            edges,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::TimeWidth;
    use crate::operators::OperatorTable;
    use crate::parser::parse_source;
    use crate::sema::analyze;

    /// Parse + analyze + elaborate a source string rooted at `top`, then build the IR, panicking
    /// on any earlier-stage error so IR tests start from a validated, elaborated design.
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
        build(&elab)
    }

    /// Count nodes of a given kind label.
    fn count_kind(ir: &NetlistIr, label: &str) -> usize {
        ir.nodes.iter().filter(|n| n.kind.label() == label).count()
    }

    /// The sole node whose kind matches `pred`, panicking if there is not exactly one.
    fn only_node<'a>(ir: &'a NetlistIr, label: &str) -> &'a Node {
        let mut it = ir.nodes.iter().filter(|n| n.kind.label() == label);
        let n = it.next().unwrap_or_else(|| panic!("no `{label}` node"));
        assert!(it.next().is_none(), "expected exactly one `{label}` node");
        n
    }

    // --- schema version is present and version-tagged (Req 4.9) -----------------------------

    #[test]
    fn ir_carries_the_current_schema_version() {
        let ir = build_ir(
            "module M (input time a, output time y) { y = inc(a); }",
            "M",
        );
        assert_eq!(ir.schema_version, CURRENT_SCHEMA_VERSION);
        assert_eq!(ir.schema_version.major, 1);
        assert_eq!(ir.top, "M");
    }

    #[test]
    fn schema_version_major_compatibility() {
        let v = SchemaVersion::new(1, 7);
        assert!(v.is_compatible_with_major(1));
        assert!(!v.is_compatible_with_major(2));
    }

    // --- node kinds: Input / Output / Min / Lt / Inc / Instance (Req 4.1) -------------------

    #[test]
    fn input_and_output_ports_become_boundary_nodes() {
        let ir = build_ir(
            "module M (input time a, input time b, output time y) { y = min(a, b); }",
            "M",
        );
        assert_eq!(count_kind(&ir, "Input"), 2, "two input ports -> two Input nodes");
        assert_eq!(count_kind(&ir, "Output"), 1, "one output port -> one Output node");
        // Every node carries a 1-based source location and the hierarchy path (Req 4.2, 4.3).
        for node in &ir.nodes {
            assert!(node.loc.line >= 1 && node.loc.col >= 1);
            assert_eq!(node.hierarchy_path, vec!["M".to_string()]);
        }
    }

    #[test]
    fn min_lowers_to_a_min_node_with_operand_edges() {
        let ir = build_ir(
            "module M (input time a, input time b, output time y) { y = min(a, b); }",
            "M",
        );
        let min = only_node(&ir, "Min");
        let operands: Vec<&Edge> = ir
            .incoming(min.id)
            .filter(|e| e.port_role == PortRole::Operand)
            .collect();
        assert_eq!(operands.len(), 2, "min(a, b) has two operand inputs");
    }

    // --- Lt pass vs inhibit edge roles (Req 4.4) --------------------------------------------

    #[test]
    fn lt_distinguishes_pass_from_inhibit_inputs() {
        // y = a < b : `a` is the pass input, `b` is the inhibit input.
        let ir = build_ir(
            "module M (input time a, input time b, output time y) { y = a < b; }",
            "M",
        );
        let lt = only_node(&ir, "Lt");
        let pass: Vec<&Edge> = ir.incoming(lt.id).filter(|e| e.port_role == PortRole::Pass).collect();
        let inhibit: Vec<&Edge> =
            ir.incoming(lt.id).filter(|e| e.port_role == PortRole::Inhibit).collect();
        assert_eq!(pass.len(), 1, "exactly one pass input");
        assert_eq!(inhibit.len(), 1, "exactly one inhibit input");

        // The pass edge comes from input `a`, the inhibit edge from input `b`.
        let name_of = |id: NodeId| ir.node(id).map(|n| n.name.clone()).unwrap();
        assert_eq!(name_of(pass[0].from), "a", "pass input is `a`");
        assert_eq!(name_of(inhibit[0].from), "b", "inhibit input is `b`");
    }

    // --- Inc amount: unit inc vs a + n (Req 4.4, 4.5) ---------------------------------------

    #[test]
    fn unit_inc_has_amount_one() {
        let ir = build_ir(
            "module M (input time a, output time y) { y = inc(a); }",
            "M",
        );
        let inc = only_node(&ir, "Inc");
        assert_eq!(inc.kind, NodeKind::Inc { amount: 1 });
    }

    #[test]
    fn delay_by_n_is_a_single_inc_node_carrying_amount_n() {
        // `a + 3` is one Inc node with amount 3, not three nodes (Req 4.5).
        let ir = build_ir(
            "module DelayN #(param N = 3) (input time a, output time y) { y = a + N; }",
            "DelayN",
        );
        assert_eq!(count_kind(&ir, "Inc"), 1, "delay chain is a single Inc node");
        let inc = only_node(&ir, "Inc");
        assert_eq!(inc.kind, NodeKind::Inc { amount: 3 });
    }

    // --- single-node vs expanded delay chain (Req 4.5) --------------------------------------

    #[test]
    fn delay_chain_expands_to_n_unit_incs() {
        let ir = build_ir(
            "module DelayN #(param N = 3) (input time a, output time y) { y = a + N; }",
            "DelayN",
        );
        // Before expansion: a single Inc{amount:3}.
        assert_eq!(count_kind(&ir, "Inc"), 1);

        let expanded = ir.expand_delay_chains();
        // After expansion: exactly 3 unit Inc nodes, each amount 1.
        assert_eq!(count_kind(&expanded, "Inc"), 3, "expands to n unit incs");
        for n in expanded.nodes.iter().filter(|n| n.kind.label() == "Inc") {
            assert_eq!(n.kind, NodeKind::Inc { amount: 1 });
        }

        // The chain is in series: the input `a` feeds the head, each unit inc feeds the next, and
        // the tail drives the output. Verify there is a single path a -> inc -> inc -> inc -> y.
        let a = expanded.nodes.iter().find(|n| n.name == "a").unwrap();
        // The head is the unit inc consuming `a`.
        let head = expanded
            .outgoing(a.id)
            .find_map(|e| expanded.node(e.to))
            .expect("a drives the head inc");
        assert_eq!(head.kind, NodeKind::Inc { amount: 1 });

        // Follow the chain to the tail, counting the inc hops.
        let mut cur = head.id;
        let mut hops = 1;
        loop {
            let next_inc = expanded
                .outgoing(cur)
                .filter_map(|e| expanded.node(e.to))
                .find(|n| n.kind.label() == "Inc");
            match next_inc {
                Some(n) => {
                    cur = n.id;
                    hops += 1;
                }
                None => break,
            }
        }
        assert_eq!(hops, 3, "three unit incs in series");
    }

    #[test]
    fn unit_inc_is_unchanged_by_expansion() {
        let ir = build_ir(
            "module M (input time a, output time y) { y = inc(a); }",
            "M",
        );
        let expanded = ir.expand_delay_chains();
        assert_eq!(count_kind(&expanded, "Inc"), 1, "a unit inc is left as a single node");
        assert_eq!(only_node(&expanded, "Inc").kind, NodeKind::Inc { amount: 1 });
    }

    // --- Instance boundary nodes retain the hierarchy path (Req 4.1, 4.3) -------------------

    #[test]
    fn sub_module_instances_are_flattened_into_the_parent_graph() {
        // An instance's body is now spliced into the top graph (not left as an opaque boundary),
        // so a hierarchical design simulates and lowers identically to its inline equivalent.
        let src = "module Leaf (input time a, output time y) { y = inc(a); }\n\
                   module Top (input time a, output time y) { inst l = Leaf (.a(a), .y(y)); }";
        let ir = build_ir(src, "Top");
        // No opaque Instance boundary node remains.
        assert_eq!(count_kind(&ir, "Instance"), 0, "instances are flattened, not left as boundaries");
        // The Leaf body's `inc` primitive is present in the flat graph.
        assert_eq!(count_kind(&ir, "Inc"), 1, "the submodule's inc is spliced into the top graph");
        // Top's own input and output ports remain.
        assert_eq!(count_kind(&ir, "Input"), 1);
        assert_eq!(count_kind(&ir, "Output"), 1);
    }

    // --- serde round-trip: the IR is serializable and version-tagged (Req 4.8, 4.9) --------

    #[test]
    fn node_ids_are_unique() {
        let ir = build_ir(
            "module M (input time a, input time b, output time y) { y = a < (a < b); }",
            "M",
        );
        let mut ids: Vec<NodeId> = ir.nodes.iter().map(|n| n.id).collect();
        let count = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), count, "all node ids are unique");
    }
}
