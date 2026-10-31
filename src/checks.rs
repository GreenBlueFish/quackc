//! Netlist checks: acyclicity and IR schema-version compatibility (Requirement 4).
//!
//! These checks guard the stable netlist-IR hub before any backend consumes it. Both read the IR
//! through its public interface only (`&NetlistIr`, [`NetlistIr::outgoing`], node accessors) and
//! never mutate it, so a failing check aborts backend consumption while leaving the IR state
//! unchanged (Req 4.6, 4.7, 4.10, 10.8).
//!
//! * [`check_acyclic`] confirms the dataflow graph is a DAG. A combinational cycle produces a
//!   combinational-cycle error whose message prints the cycle as an ordered sequence of the wires
//!   or instances forming it, beginning and ending at the same element (Req 4.6, 4.7, 13.3).
//! * [`check_schema_version`] rejects an IR whose major schema version differs from the one a
//!   backend declares support for, without consuming the IR (Req 4.10, 10.8).
//!
//! Each returns `Result<(), Vec<Diagnostic>>` per the design's checks-module contract: an empty
//! `Ok(())` on success, or the collected diagnostics on failure.

use std::collections::HashMap;

use crate::diag::Diagnostic;
use crate::ir::{NetlistIr, NodeId};

/// Confirm the netlist graph is acyclic before any backend consumes it (Req 4.7).
///
/// Runs a depth-first search over the directed dataflow graph. If a back edge is found — an edge
/// to a node currently on the DFS stack — the graph has a combinational cycle: the check reports a
/// combinational-cycle error whose message prints the ordered cycle path (Req 4.6, 13.3) and
/// returns `Err`, aborting backend consumption. The IR is read through `&NetlistIr` only and is
/// never mutated, so its state is left unchanged (Req 4.6).
///
/// On success returns `Ok(())`.
pub fn check_acyclic(ir: &NetlistIr) -> Result<(), Vec<Diagnostic>> {
    // DFS node colors: White = unvisited, Gray = on the current DFS stack, Black = finished.
    #[derive(Clone, Copy, PartialEq)]
    enum Color {
        White,
        Gray,
        Black,
    }

    let mut color: HashMap<NodeId, Color> = ir.nodes.iter().map(|n| (n.id, Color::White)).collect();

    // Iterative DFS so a deep (long delay chain) graph cannot overflow the stack. The `stack`
    // holds (node, next-successor-index); `path` mirrors the gray nodes in order so a detected
    // back edge can reconstruct the ordered cycle.
    for start in ir.nodes.iter().map(|n| n.id) {
        if color[&start] != Color::White {
            continue;
        }

        // Precompute each node's successors lazily as we descend. `stack` entries own the list of
        // successor ids plus a cursor into it.
        let mut stack: Vec<(NodeId, Vec<NodeId>, usize)> =
            vec![(start, successors(ir, start), 0)];
        let mut path: Vec<NodeId> = vec![start];
        color.insert(start, Color::Gray);

        while let Some(&mut (node, ref succ, ref mut idx)) = stack.last_mut() {
            if *idx < succ.len() {
                let next = succ[*idx];
                *idx += 1;
                match color[&next] {
                    Color::White => {
                        color.insert(next, Color::Gray);
                        path.push(next);
                        let s = successors(ir, next);
                        stack.push((next, s, 0));
                    }
                    Color::Gray => {
                        // Back edge `node -> next`: `next` is an ancestor on the current path, so
                        // the cycle is the path segment from `next` up to `node`, closed back to
                        // `next` (Req 13.3: begins and ends at the same element).
                        let at = path.iter().position(|&n| n == next).unwrap_or(0);
                        let mut cycle: Vec<NodeId> = path[at..].to_vec();
                        cycle.push(next);
                        let _ = node;
                        return Err(vec![cycle_diagnostic(ir, &cycle)]);
                    }
                    Color::Black => {}
                }
            } else {
                // Exhausted this node's successors: pop it, mark finished.
                color.insert(node, Color::Black);
                path.pop();
                stack.pop();
            }
        }
    }

    Ok(())
}

/// Confirm the IR's schema version is compatible with the major version a backend supports (Req
/// 4.10, 10.8).
///
/// Compatibility is by major version only (a minor bump is backward-compatible). If
/// `ir.schema_version.major` differs from `supported_major`, reports an incompatible-version error
/// and returns `Err`, so the consumer rejects the operation without consuming the IR. The IR is
/// read through `&NetlistIr` only and is never mutated.
pub fn check_schema_version(
    ir: &NetlistIr,
    supported_major: u16,
) -> Result<(), Vec<Diagnostic>> {
    if ir.schema_version.is_compatible_with_major(supported_major) {
        return Ok(());
    }

    // Report at the first node's location if present, else synthesize a top-of-file location: the
    // version mismatch is a property of the whole document, not a single construct.
    let span = ir
        .nodes
        .first()
        .map(|n| n.loc.clone())
        .unwrap_or_else(|| crate::diag::Span::new(ir.top.clone(), 1, 1));

    let message = format!(
        "incompatible netlist IR version: the IR is schema version {} but this backend supports \
         major version {supported_major}; refusing to consume it",
        ir.schema_version
    );
    Err(vec![Diagnostic::error(span, message)])
}

/// The successors (fan-out targets) of a node, in a stable order.
fn successors(ir: &NetlistIr, id: NodeId) -> Vec<NodeId> {
    ir.outgoing(id).map(|e| e.to).collect()
}

/// Build the combinational-cycle diagnostic from an ordered cycle (which already begins and ends
/// at the same node id). The message prints the cycle as `a -> b -> ... -> a` using each node's
/// original wire/instance name, and the note repeats the path for emphasis (Req 13.3).
fn cycle_diagnostic(ir: &NetlistIr, cycle: &[NodeId]) -> Diagnostic {
    let name_of = |id: NodeId| {
        ir.node(id)
            .map(|n| n.name.clone())
            .unwrap_or_else(|| format!("node#{id}"))
    };
    let path: Vec<String> = cycle.iter().map(|&id| name_of(id)).collect();
    let rendered = path.join(" -> ");

    // Point the diagnostic at the source location of the cycle's starting element.
    let span = cycle
        .first()
        .and_then(|&id| ir.node(id))
        .map(|n| n.loc.clone())
        .unwrap_or_else(|| crate::diag::Span::new(ir.top.clone(), 1, 1));

    Diagnostic::error(
        span,
        "combinational cycle detected: the netlist graph is not acyclic, so no backend can \
         consume it",
    )
    .with_note(format!("cycle path: {rendered}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diag::Span;
    use crate::ir::{
        Edge, NetlistIr, Node, NodeKind, PortRole, SchemaVersion, CURRENT_SCHEMA_VERSION,
    };

    fn span() -> Span {
        Span::new("test.sta", 1, 1)
    }

    /// A node with the given id, kind, and name; all in one hierarchy level.
    fn node(id: NodeId, kind: NodeKind, name: &str) -> Node {
        Node {
            id,
            kind,
            loc: span(),
            name: name.to_string(),
            hierarchy_path: vec!["M".to_string()],
        }
    }

    fn edge(from: NodeId, to: NodeId) -> Edge {
        Edge {
            from,
            to,
            port_role: PortRole::Operand,
        }
    }

    /// An acyclic IR: a -> inc -> y.
    fn acyclic_ir() -> NetlistIr {
        NetlistIr {
            schema_version: CURRENT_SCHEMA_VERSION,
            top: "M".to_string(),
            nodes: vec![
                node(0, NodeKind::Input, "a"),
                node(1, NodeKind::Inc { amount: 1 }, "inc"),
                node(2, NodeKind::Output, "y"),
            ],
            edges: vec![edge(0, 1), edge(1, 2)],
        }
    }

    /// A cyclic IR: a -> m -> n -> m (the min nodes feed back into each other).
    fn cyclic_ir() -> NetlistIr {
        NetlistIr {
            schema_version: CURRENT_SCHEMA_VERSION,
            top: "M".to_string(),
            nodes: vec![
                node(0, NodeKind::Input, "a"),
                node(1, NodeKind::Min, "m"),
                node(2, NodeKind::Min, "n"),
                node(3, NodeKind::Output, "y"),
            ],
            // a -> m, m -> n, n -> m (cycle), m -> y
            edges: vec![edge(0, 1), edge(1, 2), edge(2, 1), edge(1, 3)],
        }
    }

    // --- acyclicity (Req 4.6, 4.7, 13.3) ----------------------------------------------------

    #[test]
    fn acyclic_ir_passes() {
        assert!(check_acyclic(&acyclic_ir()).is_ok());
    }

    #[test]
    fn cyclic_ir_produces_a_cycle_error_with_the_ordered_path() {
        let ir = cyclic_ir();
        let err = check_acyclic(&ir).expect_err("a cyclic IR must fail the acyclicity check");
        assert_eq!(err.len(), 1, "exactly one combinational-cycle diagnostic");
        let diag = &err[0];
        assert!(diag.is_error());
        assert!(
            diag.message.contains("combinational cycle"),
            "message names the combinational-cycle rule in plain language, got: {}",
            diag.message
        );
        // The note prints the ordered cycle path, beginning and ending at the same element.
        let note = diag.note.as_deref().expect("cycle diagnostic carries a path note");
        assert!(note.contains("->"), "path is an ordered sequence, got: {note}");
        // The two Min nodes `m` and `n` form the cycle; the path begins and ends at the same node.
        let path = note.trim_start_matches("cycle path: ");
        let elems: Vec<&str> = path.split(" -> ").collect();
        assert!(elems.len() >= 2, "cycle has at least two elements: {path}");
        assert_eq!(
            elems.first(),
            elems.last(),
            "cycle begins and ends at the same element (Req 13.3): {path}"
        );
        assert!(elems.contains(&"m") && elems.contains(&"n"), "names the cycle wires: {path}");
    }

    #[test]
    fn acyclicity_check_does_not_mutate_the_ir() {
        // The IR must be left unchanged whether the check passes or fails (Req 4.6).
        let ok_ir = acyclic_ir();
        let ok_before = ok_ir.clone();
        let _ = check_acyclic(&ok_ir);
        assert_eq!(ok_ir, ok_before, "a passing check leaves the IR unchanged");

        let bad_ir = cyclic_ir();
        let bad_before = bad_ir.clone();
        let _ = check_acyclic(&bad_ir);
        assert_eq!(bad_ir, bad_before, "a failing check leaves the IR unchanged");
    }

    #[test]
    fn a_self_loop_is_a_cycle_whose_path_repeats_the_node() {
        let ir = NetlistIr {
            schema_version: CURRENT_SCHEMA_VERSION,
            top: "M".to_string(),
            nodes: vec![node(0, NodeKind::Min, "m")],
            edges: vec![edge(0, 0)],
        };
        let err = check_acyclic(&ir).expect_err("a self-loop is a cycle");
        let note = err[0].note.as_deref().unwrap();
        assert_eq!(note, "cycle path: m -> m", "self-loop path repeats the node");
    }

    // --- schema-version compatibility (Req 4.10, 10.8) --------------------------------------

    #[test]
    fn matching_major_version_passes() {
        let ir = acyclic_ir(); // schema major 1
        assert!(check_schema_version(&ir, 1).is_ok());
    }

    #[test]
    fn mismatched_major_version_is_rejected_without_consuming_the_ir() {
        let ir = acyclic_ir(); // schema major 1
        let before = ir.clone();
        let err = check_schema_version(&ir, 2)
            .expect_err("a backend supporting major 2 must reject a major-1 IR");
        assert_eq!(err.len(), 1);
        assert!(err[0].is_error());
        assert!(
            err[0].message.contains("incompatible") && err[0].message.contains("version"),
            "message names the incompatible-version rule: {}",
            err[0].message
        );
        // The IR is not consumed/mutated by the rejecting check (Req 4.10).
        assert_eq!(ir, before, "a rejected IR is left unchanged");
    }

    #[test]
    fn minor_version_differences_remain_compatible() {
        // A consumer supporting major 1 accepts any minor (Req 4.10, 10.7).
        let ir = NetlistIr {
            schema_version: SchemaVersion::new(1, 9),
            ..acyclic_ir()
        };
        assert!(check_schema_version(&ir, 1).is_ok());
    }
}
