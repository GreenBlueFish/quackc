//! Netlist IR exporters: JSON and DOT (Requirement 12).
//!
//! A future tool author consumes the netlist IR through these exporters, never through frontend
//! internals: the [`JsonExporter`] emits the machine-readable JSON document a visualizer reads, and
//! the [`DotExporter`] emits a Graphviz quick-view graph. Both are plain [`Backend`]
//! implementors that read only `&NetlistIr` and declare the IR major version they understand, so
//! the version-checking dispatcher rejects an incompatible IR before any export happens (Req 10.8,
//! 10.9, 4.10).
//!
//! ## DOT shape (Req 12.2, 12.3, 12.4)
//!
//! The DOT exporter emits a Graphviz `digraph` with exactly one DOT node per IR node and exactly
//! one DOT edge per IR edge (Req 12.2). Each node's label carries its kind, id, and original name;
//! an `Inc` node additionally shows its delay `amount` (Req 12.3). Each edge is labeled with its
//! port role, so an `Lt` node's `pass` and `inhibit` inputs are distinguishable in the quick-view
//! graph (Req 12.3):
//!
//! ```dot
//! digraph "Sort2" {
//!   n0 [label="Input #0\na"];
//!   n3 [label="Lt #3\n_t"];
//!   n1 -> n3 [label="pass"];
//!   n0 -> n3 [label="inhibit"];
//! }
//! ```
//!
//! By default a multi-tick `Inc { amount: n }` is drawn as a single compact node labeled with `n`.
//! When delay-chain expansion is requested ([`DotExporter::with_delay_chain_expansion`]), the IR is
//! first expanded via [`NetlistIr::expand_delay_chains`] so the same `Inc { amount: n }` becomes
//! `n` unit `Inc { amount: 1 }` nodes chained in series (Req 12.4) — still exactly one DOT node per
//! (expanded) IR node and one edge per (expanded) IR edge.
//!
//! ## JSON shape (Req 12.1, 12.5)
//!
//! The document carries the `schema_version` at its root so a consumer can reject an incompatible
//! major version before reading the rest (Req 4.10). Every node emits its id, kind, source
//! location (`file:line:col`), original wire/instance name, and module hierarchy path; every edge
//! emits its endpoints and port role, which distinguishes an `Lt` node's pass input from its
//! inhibit input and records an `Inc` node's delay amount (Req 12.1, 12.3, 12.5):
//!
//! ```json
//! {
//!   "schema_version": { "major": 1, "minor": 0 },
//!   "top": "Sort2",
//!   "nodes": [
//!     { "id": 0, "kind": "Input", "name": "a", "loc": { "file": "sort2.sta", "line": 2, "col": 18 }, "hierarchy_path": ["Sort2"] }
//!   ],
//!   "edges": [ { "from": 0, "to": 2, "port_role": "Operand" } ]
//! }
//! ```
//!
//! The `kind` is the node's stable label ([`NodeKind::label`]) rather than serde's default
//! enum encoding, matching the shape above. An `Inc` node additionally carries its `amount` and an
//! `Instance` node its `module`, so no provenance is lost for the data-carrying kinds.
//!
//! ## Provenance guard (Req 12.6)
//!
//! Before serializing, the exporter validates that every node carries all required provenance: a
//! non-empty name, a non-empty source file, and `line >= 1` / `col >= 1`. If any node is missing a
//! field, the exporter reports an error naming the node and the missing field and emits *no*
//! document — a partial export containing an under-specified node is never produced (Req 12.6).

use serde::Serialize;

use crate::backend::{Backend, BackendOptions, BackendOutput, EmittedFile};
use crate::diag::{Diagnostic, Span};
use crate::ir::{Edge, NetlistIr, Node, NodeKind, PortRole, SchemaVersion, CURRENT_SCHEMA_VERSION};

/// Suggested file name for the emitted JSON netlist document.
pub const JSON_FILE_NAME: &str = "netlist.json";

/// The JSON exporter for the netlist IR (Req 12.1). Implements [`Backend`] over the stable IR,
/// reading only `&NetlistIr` and declaring the IR major version it understands (Req 10.9, 4.10).
#[derive(Clone, Copy, Debug, Default)]
pub struct JsonExporter;

impl JsonExporter {
    /// Construct the JSON exporter.
    pub fn new() -> Self {
        JsonExporter
    }

    /// Serialize `ir` to a pretty-printed JSON document string, or report the provenance errors
    /// that prevented a complete export (Req 12.1, 12.6).
    ///
    /// This is the exporter's core, factored out of [`Backend::run`] so callers that want the JSON
    /// text directly (and tests) need not build a [`BackendOutput`].
    pub fn to_json(ir: &NetlistIr) -> Result<String, Vec<Diagnostic>> {
        let doc = JsonDocument::build(ir)?;
        // serde_json cannot fail serializing this plain, owned, finite structure; treat an
        // unexpected error defensively as an export error rather than panicking.
        serde_json::to_string_pretty(&doc).map_err(|e| {
            vec![Diagnostic::error(
                Span::new(ir.top.clone(), 1, 1),
                format!("failed to serialize the netlist IR to JSON: {e}"),
            )]
        })
    }
}

impl Backend for JsonExporter {
    fn name(&self) -> &str {
        "json-export"
    }

    fn supported_ir_major(&self) -> u16 {
        CURRENT_SCHEMA_VERSION.major
    }

    fn run(&self, ir: &NetlistIr, _opts: &BackendOptions) -> Result<BackendOutput, Vec<Diagnostic>> {
        let json = Self::to_json(ir)?;
        Ok(BackendOutput::from_files(vec![EmittedFile::new(
            JSON_FILE_NAME,
            json,
        )]))
    }
}

/// The root of the exported JSON document (Req 12.1). The `schema_version` travels at the root so a
/// consumer can reject an incompatible major version before reading further (Req 4.10).
#[derive(Debug, Serialize)]
struct JsonDocument {
    schema_version: SchemaVersion,
    top: String,
    nodes: Vec<JsonNode>,
    edges: Vec<Edge>,
}

/// A node's provenance as exported (Req 12.1, 12.5). `kind` is the node's stable label rather than
/// serde's default enum encoding, matching the documented JSON shape. `amount` / `module` carry the
/// data of the data-bearing kinds and are omitted for the others.
#[derive(Debug, Serialize)]
struct JsonNode {
    id: u32,
    kind: &'static str,
    name: String,
    loc: Span,
    hierarchy_path: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    amount: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    module: Option<String>,
}

impl JsonDocument {
    /// Validate every node's provenance and build the serializable document, or collect the
    /// errors that make a complete export impossible (Req 12.6).
    fn build(ir: &NetlistIr) -> Result<Self, Vec<Diagnostic>> {
        let mut errors: Vec<Diagnostic> = Vec::new();
        let mut nodes: Vec<JsonNode> = Vec::with_capacity(ir.nodes.len());

        for node in &ir.nodes {
            match validate_node(node) {
                Ok(json_node) => nodes.push(json_node),
                Err(mut node_errors) => errors.append(&mut node_errors),
            }
        }

        // Any missing provenance field means no document is emitted at all (Req 12.6).
        if !errors.is_empty() {
            return Err(errors);
        }

        Ok(JsonDocument {
            schema_version: ir.schema_version,
            top: ir.top.clone(),
            nodes,
            edges: ir.edges.clone(),
        })
    }
}

/// Validate one node's required provenance, returning its exportable view or every missing-field
/// error it has. The checks mirror Req 12.1 / 12.6: a non-empty original name, a non-empty source
/// file, and 1-based line and column (`line >= 1`, `col >= 1`). The node id and kind are always
/// present by construction (the IR's types make them non-optional), so they cannot be "missing";
/// the id is included in each error so the offending node is identifiable (Req 12.6).
fn validate_node(node: &Node) -> Result<JsonNode, Vec<Diagnostic>> {
    let mut errors: Vec<Diagnostic> = Vec::new();
    let span = &node.loc;

    if node.name.trim().is_empty() {
        errors.push(missing_field(node, span, "original name"));
    }
    if span.file.trim().is_empty() {
        errors.push(missing_field(node, span, "source file"));
    }
    if span.line < 1 {
        errors.push(missing_field(node, span, "source line (must be >= 1)"));
    }
    if span.col < 1 {
        errors.push(missing_field(node, span, "source column (must be >= 1)"));
    }

    if !errors.is_empty() {
        return Err(errors);
    }

    let (amount, module) = match &node.kind {
        NodeKind::Inc { amount } => (Some(*amount), None),
        NodeKind::Instance { module } => (None, Some(module.clone())),
        _ => (None, None),
    };

    Ok(JsonNode {
        id: node.id,
        kind: node.kind.label(),
        name: node.name.clone(),
        loc: span.clone(),
        hierarchy_path: node.hierarchy_path.clone(),
        amount,
        module,
    })
}

/// Build a missing-provenance-field error naming the node (id and kind) and the field, in plain
/// language rather than a bare code (Req 12.6, 13.2).
fn missing_field(node: &Node, span: &Span, field: &str) -> Diagnostic {
    Diagnostic::error(
        span.clone(),
        format!(
            "cannot export netlist node #{} ({}): missing required provenance field `{}`",
            node.id,
            node.kind.label(),
            field
        ),
    )
    .with_note(
        "every exported node must carry its id, kind, source location (file, line, column), \
         original name, and hierarchy path; no export is emitted while a node is missing one",
    )
}

/// Suggested file name for the emitted DOT netlist graph.
pub const DOT_FILE_NAME: &str = "netlist.dot";

/// The DOT (Graphviz) exporter for the netlist IR (Req 12.2). Like [`JsonExporter`] it implements
/// [`Backend`] over the stable IR, reading only `&NetlistIr` and declaring the IR major version it
/// understands (Req 10.9, 4.10).
///
/// The exporter has one mode knob, `expand_delay_chains`: when `false` (the default, compact form)
/// a multi-tick `Inc { amount: n }` is drawn as a single node labeled with its amount; when `true`
/// the IR is first run through [`NetlistIr::expand_delay_chains`] so each such node becomes `n`
/// unit `Inc` nodes chained in series before the graph is emitted (Req 12.4). The knob lives on the
/// exporter rather than in [`BackendOptions`] because it is specific to this backend.
#[derive(Clone, Copy, Debug, Default)]
pub struct DotExporter {
    /// Expand each multi-tick `Inc { amount: n }` into `n` unit `Inc` nodes before emitting
    /// (Req 12.4). Defaults to `false` — the compact single-node form.
    expand_delay_chains: bool,
}

impl DotExporter {
    /// Construct the DOT exporter in its default compact form (a multi-tick delay is one node).
    pub fn new() -> Self {
        DotExporter::default()
    }

    /// Construct the DOT exporter in delay-chain-expansion mode: each `Inc { amount: n }` is drawn
    /// as `n` chained unit `Inc` nodes (Req 12.4).
    pub fn with_delay_chain_expansion() -> Self {
        DotExporter {
            expand_delay_chains: true,
        }
    }

    /// Render `ir` to a Graphviz DOT document string (Req 12.2, 12.3, 12.4).
    ///
    /// This is the exporter's core, factored out of [`Backend::run`] (mirroring
    /// [`JsonExporter::to_json`]) so callers that want the DOT text directly — and tests — need not
    /// build a [`BackendOutput`]. In expansion mode the IR is first expanded; either way the
    /// emitted graph has exactly one DOT node per IR node and exactly one DOT edge per IR edge of
    /// the graph it renders (Req 12.2).
    pub fn to_dot(&self, ir: &NetlistIr) -> String {
        if self.expand_delay_chains {
            Self::render(&ir.expand_delay_chains())
        } else {
            Self::render(ir)
        }
    }

    /// Render a `digraph`, emitting one DOT node per IR node and one DOT edge per IR edge in IR
    /// order so the output is deterministic (Req 12.2).
    fn render(ir: &NetlistIr) -> String {
        let mut out = String::new();
        out.push_str(&format!("digraph {} {{\n", quote(&ir.top)));

        // Exactly one DOT node per IR node (Req 12.2). The label carries the kind, id, and original
        // name; an Inc node also shows its delay amount (Req 12.3).
        for node in &ir.nodes {
            out.push_str(&format!(
                "  n{} [label={}];\n",
                node.id,
                quote(&node_label(node))
            ));
        }

        // Exactly one DOT edge per IR edge (Req 12.2), labeled with its port role so an Lt node's
        // pass and inhibit inputs are distinguishable (Req 12.3).
        for edge in &ir.edges {
            out.push_str(&format!(
                "  n{} -> n{} [label={}];\n",
                edge.from,
                edge.to,
                quote(&edge_label(&edge.port_role))
            ));
        }

        out.push_str("}\n");
        out
    }
}

impl Backend for DotExporter {
    fn name(&self) -> &str {
        "dot-export"
    }

    fn supported_ir_major(&self) -> u16 {
        CURRENT_SCHEMA_VERSION.major
    }

    fn run(&self, ir: &NetlistIr, _opts: &BackendOptions) -> Result<BackendOutput, Vec<Diagnostic>> {
        let dot = self.to_dot(ir);
        Ok(BackendOutput::from_files(vec![EmittedFile::new(
            DOT_FILE_NAME,
            dot,
        )]))
    }
}

/// The label text for a node: its kind, id, and original name, with an `Inc` node additionally
/// showing its delay amount (Req 12.3). Newlines (`\n`) separate the lines; [`quote`] turns them
/// into Graphviz line breaks in the emitted label.
fn node_label(node: &Node) -> String {
    match &node.kind {
        NodeKind::Inc { amount } => {
            format!("{} #{} (amount={})\n{}", node.kind.label(), node.id, amount, node.name)
        }
        NodeKind::Instance { module } => {
            format!("{} #{} ({})\n{}", node.kind.label(), node.id, module, node.name)
        }
        _ => format!("{} #{}\n{}", node.kind.label(), node.id, node.name),
    }
}

/// The label text for an edge: its port role. An `Lt` node's inputs are distinguished as `pass`
/// and `inhibit`; a generic input is `operand`; an instance-port edge names its port (Req 12.3).
fn edge_label(role: &PortRole) -> String {
    match role {
        PortRole::Operand => "operand".to_string(),
        PortRole::Pass => "pass".to_string(),
        PortRole::Inhibit => "inhibit".to_string(),
        PortRole::InstancePort(port) => format!("port:{port}"),
    }
}

/// Wrap `s` as a double-quoted Graphviz ID, escaping the characters DOT treats specially inside a
/// quoted string: a backslash and a double quote are backslash-escaped, and an embedded newline
/// becomes the `\n` line-break escape Graphviz renders in a label. This keeps every emitted
/// identifier and label a single, well-formed quoted token regardless of the IR's names.
fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            other => out.push(other),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{Edge, NetlistIr, NodeKind, PortRole, CURRENT_SCHEMA_VERSION};
    use serde_json::Value;

    /// Build a well-formed node with all provenance present.
    fn node(id: u32, kind: NodeKind, name: &str, file: &str, line: u32, col: u32) -> Node {
        Node {
            id,
            kind,
            loc: Span::new(file, line, col),
            name: name.to_string(),
            hierarchy_path: vec!["Sort2".to_string()],
        }
    }

    /// A small, fully-populated IR mirroring the design's `Sort2` export example.
    fn sort2_ir() -> NetlistIr {
        NetlistIr {
            schema_version: CURRENT_SCHEMA_VERSION,
            top: "Sort2".to_string(),
            nodes: vec![
                node(0, NodeKind::Input, "a", "sort2.sta", 2, 18),
                node(1, NodeKind::Input, "b", "sort2.sta", 2, 32),
                node(2, NodeKind::Min, "lo", "sort2.sta", 6, 5),
                node(3, NodeKind::Lt, "_t", "sort2.sta", 7, 5),
                node(4, NodeKind::Output, "hi", "sort2.sta", 7, 5),
            ],
            edges: vec![
                Edge { from: 0, to: 2, port_role: PortRole::Operand },
                Edge { from: 1, to: 2, port_role: PortRole::Operand },
                Edge { from: 1, to: 3, port_role: PortRole::Pass },
                Edge { from: 0, to: 3, port_role: PortRole::Inhibit },
                Edge { from: 3, to: 4, port_role: PortRole::Operand },
            ],
        }
    }

    #[test]
    fn exports_schema_version_top_nodes_and_edges_shape() {
        let ir = sort2_ir();
        let json = JsonExporter::to_json(&ir).expect("well-formed IR exports");
        let v: Value = serde_json::from_str(&json).expect("valid JSON");

        // schema_version lives at the document root (Req 4.10, 12.1).
        assert_eq!(v["schema_version"]["major"], 1);
        assert_eq!(v["schema_version"]["minor"], 0);
        assert_eq!(v["top"], "Sort2");

        let nodes = v["nodes"].as_array().expect("nodes is an array");
        assert_eq!(nodes.len(), 5);

        // First node matches the design example exactly: id, kind label, name, loc, hierarchy.
        let n0 = &nodes[0];
        assert_eq!(n0["id"], 0);
        assert_eq!(n0["kind"], "Input");
        assert_eq!(n0["name"], "a");
        assert_eq!(n0["loc"]["file"], "sort2.sta");
        assert_eq!(n0["loc"]["line"], 2);
        assert_eq!(n0["loc"]["col"], 18);
        assert_eq!(n0["hierarchy_path"][0], "Sort2");

        // kind is the stable label, never serde's default enum encoding.
        assert_eq!(nodes[2]["kind"], "Min");
        assert_eq!(nodes[3]["kind"], "Lt");
        assert_eq!(nodes[4]["kind"], "Output");

        // Simple kinds carry no amount/module fields (skipped when absent).
        assert!(n0.get("amount").is_none());
        assert!(n0.get("module").is_none());

        let edges = v["edges"].as_array().expect("edges is an array");
        assert_eq!(edges.len(), 5);
        // Port roles distinguish Lt's pass/inhibit inputs (Req 12.3).
        assert_eq!(edges[0]["port_role"], "Operand");
        assert_eq!(edges[2]["port_role"], "Pass");
        assert_eq!(edges[3]["port_role"], "Inhibit");
    }

    #[test]
    fn inc_node_carries_amount_and_instance_carries_module() {
        let ir = NetlistIr {
            schema_version: CURRENT_SCHEMA_VERSION,
            top: "Top".to_string(),
            nodes: vec![
                node(0, NodeKind::Inc { amount: 3 }, "d", "t.sta", 4, 2),
                node(
                    1,
                    NodeKind::Instance { module: "Sub".to_string() },
                    "u0",
                    "t.sta",
                    5,
                    2,
                ),
            ],
            edges: vec![],
        };
        let json = JsonExporter::to_json(&ir).expect("exports");
        let v: Value = serde_json::from_str(&json).unwrap();

        assert_eq!(v["nodes"][0]["kind"], "Inc");
        assert_eq!(v["nodes"][0]["amount"], 3);
        assert_eq!(v["nodes"][1]["kind"], "Instance");
        assert_eq!(v["nodes"][1]["module"], "Sub");
    }

    #[test]
    fn run_emits_a_single_named_json_file() {
        let out = JsonExporter::new()
            .run(&sort2_ir(), &BackendOptions::default())
            .expect("run succeeds for a well-formed IR");
        assert_eq!(out.files.len(), 1);
        assert_eq!(out.files[0].path, JSON_FILE_NAME);
        assert!(out.warnings.is_empty());
        // The emitted contents are valid JSON.
        let _: Value = serde_json::from_str(&out.files[0].contents).expect("emitted file is JSON");
    }

    #[test]
    fn missing_name_is_an_error_and_emits_no_document() {
        let mut ir = sort2_ir();
        ir.nodes[2].name = String::new(); // blank out the Min node's original name.

        let errors = JsonExporter::to_json(&ir).expect_err("missing name must fail the export");
        assert_eq!(errors.len(), 1);
        let rendered = errors[0].render();
        // The error identifies the offending node and the missing field (Req 12.6).
        assert!(rendered.contains("#2"), "names the node id: {rendered}");
        assert!(rendered.contains("original name"), "names the field: {rendered}");

        // And run() surfaces the same error — no partial document is produced (Req 12.6).
        let run_err = JsonExporter::new()
            .run(&ir, &BackendOptions::default())
            .expect_err("run must not emit a document with a missing field");
        assert_eq!(run_err.len(), 1);
    }

    #[test]
    fn empty_source_file_is_a_missing_provenance_error() {
        let mut ir = sort2_ir();
        // Replace a node's span with one whose file is empty (constructed directly to bypass
        // Span::new's convenience, which does not constrain the file).
        ir.nodes[0].loc = Span::new("", 2, 18);

        let errors = JsonExporter::to_json(&ir).expect_err("empty file must fail the export");
        assert!(errors.iter().any(|d| d.render().contains("source file")));
    }

    #[test]
    fn multiple_missing_fields_across_nodes_are_all_reported() {
        let mut ir = sort2_ir();
        ir.nodes[0].name = String::new();
        ir.nodes[1].loc = Span::new("", 2, 32);

        let errors = JsonExporter::to_json(&ir).expect_err("must fail");
        // One error for the blank name, one for the empty file: every detected problem is reported.
        assert_eq!(errors.len(), 2);
    }

    // --- DOT exporter (Req 12.2, 12.3, 12.4) ------------------------------------------------

    /// Count the DOT node declaration lines (`  nN [label=...]`) in a rendered graph. A node line
    /// has `->` absent and a leading `  n`.
    fn count_dot_nodes(dot: &str) -> usize {
        dot.lines()
            .filter(|l| l.trim_start().starts_with('n') && !l.contains("->"))
            .count()
    }

    /// Count the DOT edge lines (`  nA -> nB [label=...]`) in a rendered graph.
    fn count_dot_edges(dot: &str) -> usize {
        dot.lines().filter(|l| l.contains("->")).count()
    }

    #[test]
    fn dot_emits_one_node_per_ir_node_and_one_edge_per_ir_edge() {
        let ir = sort2_ir();
        let dot = DotExporter::new().to_dot(&ir);

        // Exactly one DOT node per IR node and one DOT edge per IR edge (Req 12.2).
        assert_eq!(count_dot_nodes(&dot), ir.nodes.len());
        assert_eq!(count_dot_edges(&dot), ir.edges.len());

        // It is a well-formed digraph named after the top module.
        assert!(dot.starts_with("digraph \"Sort2\" {"));
        assert!(dot.trim_end().ends_with('}'));
    }

    #[test]
    fn dot_labels_inc_node_with_its_amount() {
        let ir = NetlistIr {
            schema_version: CURRENT_SCHEMA_VERSION,
            top: "Top".to_string(),
            nodes: vec![
                node(0, NodeKind::Input, "a", "t.sta", 1, 1),
                node(1, NodeKind::Inc { amount: 5 }, "d", "t.sta", 2, 1),
            ],
            edges: vec![Edge { from: 0, to: 1, port_role: PortRole::Operand }],
        };
        let dot = DotExporter::new().to_dot(&ir);

        // The Inc node's label carries its delay amount (Req 12.3), and it is a single node.
        assert_eq!(count_dot_nodes(&dot), 2);
        assert!(dot.contains("amount=5"), "Inc label shows amount: {dot}");
    }

    #[test]
    fn dot_labels_lt_edges_with_pass_and_inhibit_roles() {
        let ir = sort2_ir();
        let dot = DotExporter::new().to_dot(&ir);

        // The two Lt inputs are distinguishable: node 1 is the pass input, node 0 the inhibit
        // input of the Lt node (#3) (Req 12.3).
        assert!(dot.contains("n1 -> n3 [label=\"pass\"]"), "pass edge labeled: {dot}");
        assert!(dot.contains("n0 -> n3 [label=\"inhibit\"]"), "inhibit edge labeled: {dot}");
    }

    #[test]
    fn dot_default_draws_a_multitick_inc_as_a_single_node() {
        let ir = NetlistIr {
            schema_version: CURRENT_SCHEMA_VERSION,
            top: "Top".to_string(),
            nodes: vec![
                node(0, NodeKind::Input, "a", "t.sta", 1, 1),
                node(1, NodeKind::Inc { amount: 3 }, "d", "t.sta", 2, 1),
                node(2, NodeKind::Output, "y", "t.sta", 3, 1),
            ],
            edges: vec![
                Edge { from: 0, to: 1, port_role: PortRole::Operand },
                Edge { from: 1, to: 2, port_role: PortRole::Operand },
            ],
        };
        let dot = DotExporter::new().to_dot(&ir);

        // Compact form: the amount-3 Inc stays a single node (3 nodes total, 2 edges) (Req 12.4).
        assert_eq!(count_dot_nodes(&dot), 3);
        assert_eq!(count_dot_edges(&dot), 2);
        assert!(dot.contains("amount=3"), "single Inc keeps its amount: {dot}");
    }

    #[test]
    fn dot_expansion_mode_draws_n_unit_incs_for_a_delay_chain() {
        let ir = NetlistIr {
            schema_version: CURRENT_SCHEMA_VERSION,
            top: "Top".to_string(),
            nodes: vec![
                node(0, NodeKind::Input, "a", "t.sta", 1, 1),
                node(1, NodeKind::Inc { amount: 3 }, "d", "t.sta", 2, 1),
                node(2, NodeKind::Output, "y", "t.sta", 3, 1),
            ],
            edges: vec![
                Edge { from: 0, to: 1, port_role: PortRole::Operand },
                Edge { from: 1, to: 2, port_role: PortRole::Operand },
            ],
        };
        let dot = DotExporter::with_delay_chain_expansion().to_dot(&ir);

        // Expansion replaces the amount-3 Inc with 3 unit incs chained in series: Input + 3 incs +
        // Output = 5 nodes, and the chain has 4 edges (in -> i0 -> i1 -> i2 -> out) (Req 12.4).
        assert_eq!(count_dot_nodes(&dot), 5);
        assert_eq!(count_dot_edges(&dot), 4);
        // Every drawn Inc is now a unit inc (amount=1); no amount=3 node survives.
        assert_eq!(dot.matches("amount=1").count(), 3, "three unit incs: {dot}");
        assert!(!dot.contains("amount=3"), "no multi-tick node remains: {dot}");
    }

    #[test]
    fn dot_run_emits_a_single_named_dot_file() {
        let out = DotExporter::new()
            .run(&sort2_ir(), &BackendOptions::default())
            .expect("run succeeds for a well-formed IR");
        assert_eq!(out.files.len(), 1);
        assert_eq!(out.files[0].path, DOT_FILE_NAME);
        assert!(out.warnings.is_empty());
        assert!(out.files[0].contents.starts_with("digraph "));
    }

    #[test]
    fn dot_quotes_escape_special_characters_in_names() {
        let mut ir = sort2_ir();
        // A name with a double quote and a backslash must stay inside a single quoted token.
        ir.nodes[0].name = "a\"b\\c".to_string();
        let dot = DotExporter::new().to_dot(&ir);

        // The special characters are backslash-escaped, keeping the graph well-formed.
        assert!(dot.contains("a\\\"b\\\\c"), "escapes quote and backslash: {dot}");
        // Still exactly one node per IR node despite the awkward name.
        assert_eq!(count_dot_nodes(&dot), ir.nodes.len());
    }
}
