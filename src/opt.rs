//! Optimization framework: `OptimizationPass` trait, `PassManager`, and passes (Requirement 11).
//!
//! The optimizer is a registry of passes, each implementing one common trait so passes can be
//! added, removed, or reordered independently of one another and of the backends (Req 11.1, 10.6).
//! Every pass rewrites the netlist IR in place and must preserve the network's observable I/O
//! semantics: for every input vector (including `inf`) the optimized network produces the same
//! outputs as the original (Req 11.4, Property 6). Task 23.1 pins that guarantee with a
//! semantics-preservation test.
//!
//! The built-in pipeline ([`PassManager::default_pipeline`]) runs, in order:
//!
//! * [`DelayFold`] — fold a chain of consecutive `Inc` nodes into a single `Inc { amount: sum }`
//!   (Req 11.1). A delay of `m` followed by a delay of `n` is a delay of `m + n`.
//! * [`AlgebraicSimplify`] — the algebraic identities of the s-t algebra (Req 11.2):
//!   `min(a, a) -> a`, `lt(a, a) -> inf`, `min(a, inf) -> a`, `lt(inf, b) -> inf`,
//!   `lt(a, inf) -> a`.
//! * [`DeadNodeElimination`] — drop every node that cannot influence any `Output` (Req 11.3).
//!
//! `--no-opt` runs none of them (Req 11.5): the CLI simply skips building/running a `PassManager`,
//! leaving the IR's node/edge structure byte-for-byte as the frontend produced it.
//!
//! ## How the `inf` constant is modeled
//!
//! The IR has no dedicated node kind for the `inf` literal; [`crate::ir`] models it as an
//! `Input`-kind node named exactly `"inf"` with no incoming edges (the same convention the backends
//! use via `is_inf_constant`). The simplification pass recognizes that shape when matching the
//! `inf`-involving identities, and reuses or synthesizes such a node when an identity rewrites to
//! `inf` (e.g. `lt(a, a) -> inf`).

use std::collections::{HashMap, HashSet};

use crate::diag::{Diagnostic, Span};
use crate::ir::{Edge, NetlistIr, Node, NodeId, NodeKind, PortRole};

/// One optimization pass over the netlist IR (Req 11.1).
///
/// A pass rewrites `ir` in place and must preserve observable I/O semantics: for every input
/// vector the post-pass network produces the same outputs as the pre-pass network (Req 11.4). A
/// pass that cannot complete returns a [`Diagnostic`] describing why; a successful pass returns
/// `Ok(())`.
pub trait OptimizationPass {
    /// A short, stable name for this pass, used in diagnostics and tests.
    fn name(&self) -> &str;

    /// Rewrite `ir` in place; must preserve observable I/O semantics (Req 11.4).
    fn run(&self, ir: &mut NetlistIr) -> Result<(), Diagnostic>;
}

/// An ordered registry of [`OptimizationPass`]es (Req 11.1, 10.6).
///
/// Passes run in registration order. Because every pass is just a `Box<dyn OptimizationPass>`,
/// adding, removing, or reordering a pass is a change to this list alone — no backend or frontend
/// code changes (Req 10.6).
pub struct PassManager {
    passes: Vec<Box<dyn OptimizationPass>>,
}

impl PassManager {
    /// An empty pass manager with no passes registered.
    pub fn new() -> Self {
        PassManager { passes: Vec::new() }
    }

    /// The default optimization pipeline: delay-fold, then algebraic-simplify, then
    /// dead-node-elimination (Req 11.1, 11.2, 11.3). This is what a normal (non-`--no-opt`) compile
    /// runs.
    pub fn default_pipeline() -> Self {
        let mut pm = PassManager::new();
        pm.register(Box::new(DelayFold));
        pm.register(Box::new(AlgebraicSimplify));
        pm.register(Box::new(DeadNodeElimination));
        pm
    }

    /// Append a pass to the pipeline. Passes run in the order they are registered.
    pub fn register(&mut self, p: Box<dyn OptimizationPass>) {
        self.passes.push(p);
    }

    /// The names of the registered passes, in run order (for diagnostics and tests).
    pub fn pass_names(&self) -> Vec<&str> {
        self.passes.iter().map(|p| p.name()).collect()
    }

    /// Run every registered pass over `ir` in order, rewriting it in place (Req 11.1).
    ///
    /// Each pass is expected to preserve observable I/O semantics (Req 11.4). If any pass reports a
    /// diagnostic the run stops and the collected diagnostics are returned; otherwise `Ok(())`.
    pub fn run_all(&self, ir: &mut NetlistIr) -> Result<(), Vec<Diagnostic>> {
        let mut diags: Vec<Diagnostic> = Vec::new();
        for pass in &self.passes {
            if let Err(d) = pass.run(ir) {
                diags.push(d);
            }
        }
        if diags.is_empty() {
            Ok(())
        } else {
            Err(diags)
        }
    }
}

impl Default for PassManager {
    fn default() -> Self {
        PassManager::default_pipeline()
    }
}

// ---------------------------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------------------------

/// Whether `node` is the `inf` constant source: an `Input`-kind node named exactly `"inf"` with no
/// incoming edges (the convention [`crate::ir`] and the backends use).
fn is_inf_constant(ir: &NetlistIr, node: &Node) -> bool {
    matches!(node.kind, NodeKind::Input)
        && node.name == "inf"
        && ir.incoming(node.id).next().is_none()
}

/// Whether the node with id `id` is the `inf` constant source.
fn is_inf_id(ir: &NetlistIr, id: NodeId) -> bool {
    ir.node(id)
        .map(|n| is_inf_constant(ir, n))
        .unwrap_or(false)
}

/// Rewire every edge so that any edge producing from `old` now produces from `new`, preserving each
/// edge's consumer and port role. Used to replace a node by an equivalent producer: consumers of
/// `old` become consumers of `new` with the same role.
fn redirect_producer(ir: &mut NetlistIr, old: NodeId, new: NodeId) {
    for e in &mut ir.edges {
        if e.from == old {
            e.from = new;
        }
    }
}

/// Allocate a node id not currently used in the IR.
fn fresh_id(ir: &NetlistIr) -> NodeId {
    ir.nodes.iter().map(|n| n.id).max().map_or(0, |m| m + 1)
}

/// Find an existing reusable `inf` constant source, or synthesize one carrying `loc`/`path`, and
/// return its id. Synthesizing a fresh `inf` node keeps the "no incoming edges" invariant that
/// [`is_inf_constant`] relies on.
fn get_or_make_inf(ir: &mut NetlistIr, loc: Span, path: Vec<String>) -> NodeId {
    if let Some(n) = ir.nodes.iter().find(|n| is_inf_constant(ir, n)) {
        return n.id;
    }
    let id = fresh_id(ir);
    ir.nodes.push(Node {
        id,
        kind: NodeKind::Input,
        loc,
        name: "inf".to_string(),
        hierarchy_path: path,
    });
    id
}

// ---------------------------------------------------------------------------------------------
// Pass: delay-fold
// ---------------------------------------------------------------------------------------------

/// Fold a chain of consecutive `Inc` nodes into a single `Inc { amount: sum }` (Req 11.1).
///
/// If an `Inc { amount: n }` node's sole operand is another `Inc { amount: m }` node whose only
/// consumer is this node, the two are a serial delay chain `m` then `n`, equal to a single delay of
/// `m + n` (saturating at the IR's `MAX_INC_AMOUNT`). The pass splices the lower inc out: this inc
/// takes its grandparent's producer as its operand and its amount becomes `m + n`. Semantics are
/// preserved because delaying by `m` then `n` ticks equals delaying by `m + n` ticks.
pub struct DelayFold;

impl OptimizationPass for DelayFold {
    fn name(&self) -> &str {
        "delay-fold"
    }

    fn run(&self, ir: &mut NetlistIr) -> Result<(), Diagnostic> {
        // Iterate to a fixed point so a chain of three or more incs folds fully.
        loop {
            // Find a foldable (child Inc, parent Inc) pair: the child's single operand is the
            // parent, and the parent's only consumer is the child.
            let mut target: Option<(NodeId, NodeId, u32, u32)> = None;
            for node in &ir.nodes {
                let child_amount = match node.kind {
                    NodeKind::Inc { amount } => amount,
                    _ => continue,
                };
                let operands: Vec<&Edge> = ir
                    .incoming(node.id)
                    .filter(|e| e.port_role == PortRole::Operand)
                    .collect();
                if operands.len() != 1 {
                    continue;
                }
                let parent_id = operands[0].from;
                let parent_amount = match ir.node(parent_id).map(|n| &n.kind) {
                    Some(NodeKind::Inc { amount }) => *amount,
                    _ => continue,
                };
                // The parent must feed only this child (otherwise folding would drop a fan-out).
                let parent_fanout = ir.outgoing(parent_id).count();
                if parent_fanout != 1 {
                    continue;
                }
                target = Some((node.id, parent_id, child_amount, parent_amount));
                break;
            }

            let (child, parent, child_amount, parent_amount) = match target {
                Some(t) => t,
                None => break,
            };

            // The grandparent producer feeding the parent inc (if any).
            let grandparent = ir
                .incoming(parent)
                .find(|e| e.port_role == PortRole::Operand)
                .map(|e| e.from);

            let summed =
                (child_amount as u64 + parent_amount as u64).min(crate::ir::MAX_INC_AMOUNT as u64)
                    as u32;

            // Rewrite the child's amount to the sum.
            if let Some(n) = ir.nodes.iter_mut().find(|n| n.id == child) {
                n.kind = NodeKind::Inc { amount: summed };
            }

            // Re-point the child's operand edge from `parent` to the grandparent (dropping the
            // parent from the chain). Remove the parent's own incoming edges and the parent node.
            ir.edges.retain(|e| !(e.to == parent)); // drop parent's inputs
            for e in &mut ir.edges {
                if e.to == child && e.from == parent {
                    match grandparent {
                        Some(gp) => e.from = gp,
                        None => { /* leave; cleaned below */ }
                    }
                }
            }
            if grandparent.is_none() {
                // Parent had no operand: the child now has no producer. Drop the dangling edge.
                ir.edges.retain(|e| !(e.to == child && e.from == parent));
            }
            ir.nodes.retain(|n| n.id != parent);
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------------------------
// Pass: algebraic-simplify
// ---------------------------------------------------------------------------------------------

/// Apply the s-t algebra's algebraic identities (Req 11.2):
///
/// * `min(a, a) -> a` — first-arrival of a value with itself is that value.
/// * `min(a, inf) -> a` — `inf` never arrives, so the other operand wins.
/// * `lt(a, a) -> inf` — a value is never strictly less than itself (ties block).
/// * `lt(inf, b) -> inf` — an `inf` pass never passes.
/// * `lt(a, inf) -> a` — nothing can inhibit against `inf`, so the pass passes.
///
/// Each rewrite replaces the matched node by an equivalent producer (an operand node, or a fresh
/// `inf` source) by redirecting the matched node's consumers to that producer, then dropping the
/// now-dead node and its input edges. Dead-node-elimination later removes anything left unreachable.
pub struct AlgebraicSimplify;

impl OptimizationPass for AlgebraicSimplify {
    fn name(&self) -> &str {
        "algebraic-simplify"
    }

    fn run(&self, ir: &mut NetlistIr) -> Result<(), Diagnostic> {
        loop {
            let mut rewrite: Option<Rewrite> = None;

            for node in &ir.nodes {
                match node.kind {
                    NodeKind::Min => {
                        let ops: Vec<NodeId> = ir
                            .incoming(node.id)
                            .filter(|e| e.port_role == PortRole::Operand)
                            .map(|e| e.from)
                            .collect();
                        // Partition operands into non-inf and inf sources. `min(a, inf) -> a` and
                        // (transitively) `min(a, a) -> a` reduce the operand multiset.
                        let non_inf: Vec<NodeId> =
                            ops.iter().copied().filter(|&o| !is_inf_id(ir, o)).collect();
                        let distinct: Vec<NodeId> = dedup_keep_order(&non_inf);

                        // If dropping inf operands or de-duplicating collapses to a single distinct
                        // producer, the min is that producer: min(a,a)=a, min(a,inf)=a.
                        if distinct.len() == 1 && (ops.len() != 1 || distinct[0] != ops[0]) {
                            rewrite = Some(Rewrite::ReplaceWith {
                                node: node.id,
                                producer: distinct[0],
                            });
                            break;
                        }
                        // If every operand was `inf`, the min is `inf`.
                        if distinct.is_empty() && !ops.is_empty() {
                            rewrite = Some(Rewrite::ReplaceWithInf {
                                node: node.id,
                                loc: node.loc.clone(),
                                path: node.hierarchy_path.clone(),
                            });
                            break;
                        }
                    }
                    NodeKind::Lt => {
                        let pass = ir
                            .incoming(node.id)
                            .find(|e| e.port_role == PortRole::Pass)
                            .map(|e| e.from);
                        let inhibit = ir
                            .incoming(node.id)
                            .find(|e| e.port_role == PortRole::Inhibit)
                            .map(|e| e.from);

                        let pass_inf = pass.map(|p| is_inf_id(ir, p)).unwrap_or(false);
                        let inhibit_inf = inhibit.map(|i| is_inf_id(ir, i)).unwrap_or(false);
                        let same = matches!((pass, inhibit), (Some(p), Some(i)) if p == i);

                        if pass_inf {
                            // lt(inf, b) -> inf.
                            rewrite = Some(Rewrite::ReplaceWithInf {
                                node: node.id,
                                loc: node.loc.clone(),
                                path: node.hierarchy_path.clone(),
                            });
                            break;
                        }
                        if same && !pass_inf {
                            // lt(a, a) -> inf (ties block). (If a==inf it is already handled above.)
                            rewrite = Some(Rewrite::ReplaceWithInf {
                                node: node.id,
                                loc: node.loc.clone(),
                                path: node.hierarchy_path.clone(),
                            });
                            break;
                        }
                        if inhibit_inf {
                            // lt(a, inf) -> a.
                            if let Some(p) = pass {
                                rewrite = Some(Rewrite::ReplaceWith {
                                    node: node.id,
                                    producer: p,
                                });
                                break;
                            }
                        }
                    }
                    _ => {}
                }
            }

            match rewrite {
                None => break,
                Some(Rewrite::ReplaceWith { node, producer }) => {
                    redirect_producer(ir, node, producer);
                    ir.edges.retain(|e| e.to != node);
                    ir.nodes.retain(|n| n.id != node);
                }
                Some(Rewrite::ReplaceWithInf { node, loc, path }) => {
                    let inf = get_or_make_inf(ir, loc, path);
                    redirect_producer(ir, node, inf);
                    ir.edges.retain(|e| e.to != node);
                    ir.nodes.retain(|n| n.id != node);
                }
            }
        }
        Ok(())
    }
}

/// A pending algebraic rewrite discovered during a scan, applied after the borrow ends.
enum Rewrite {
    /// Replace `node` by routing its consumers to an existing `producer`.
    ReplaceWith { node: NodeId, producer: NodeId },
    /// Replace `node` by an `inf` constant source (synthesized if needed).
    ReplaceWithInf {
        node: NodeId,
        loc: Span,
        path: Vec<String>,
    },
}

/// De-duplicate ids preserving first-seen order.
fn dedup_keep_order(ids: &[NodeId]) -> Vec<NodeId> {
    let mut seen: HashSet<NodeId> = HashSet::new();
    let mut out = Vec::new();
    for &id in ids {
        if seen.insert(id) {
            out.push(id);
        }
    }
    out
}

// ---------------------------------------------------------------------------------------------
// Pass: dead-node-elimination
// ---------------------------------------------------------------------------------------------

/// Remove every node that cannot influence any `Output` (Req 11.3).
///
/// Starting from the `Output` nodes, walk the graph backwards over incoming edges; every node
/// reached is live. All other nodes — and every edge touching a dead node — are dropped. Outputs
/// (and their transitive producers) are always kept, so observable I/O semantics are preserved: a
/// removed node, by construction, feeds nothing observable.
pub struct DeadNodeElimination;

impl OptimizationPass for DeadNodeElimination {
    fn name(&self) -> &str {
        "dead-node-elimination"
    }

    fn run(&self, ir: &mut NetlistIr) -> Result<(), Diagnostic> {
        // Reverse adjacency: consumer -> its producers.
        let mut producers: HashMap<NodeId, Vec<NodeId>> = HashMap::new();
        for e in &ir.edges {
            producers.entry(e.to).or_default().push(e.from);
        }

        // Seed the live set with every Output node and walk backwards.
        let mut live: HashSet<NodeId> = HashSet::new();
        let mut stack: Vec<NodeId> = ir
            .nodes
            .iter()
            .filter(|n| matches!(n.kind, NodeKind::Output))
            .map(|n| n.id)
            .collect();
        for &id in &stack {
            live.insert(id);
        }
        while let Some(id) = stack.pop() {
            if let Some(ps) = producers.get(&id) {
                for &p in ps {
                    if live.insert(p) {
                        stack.push(p);
                    }
                }
            }
        }

        ir.nodes.retain(|n| live.contains(&n.id));
        ir.edges
            .retain(|e| live.contains(&e.from) && live.contains(&e.to));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diag::Span;
    use crate::ir::{Edge, NetlistIr, Node, NodeId, NodeKind, PortRole, CURRENT_SCHEMA_VERSION};

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
        Edge {
            from,
            to,
            port_role: role,
        }
    }

    fn ir_with(nodes: Vec<Node>, edges: Vec<Edge>) -> NetlistIr {
        NetlistIr {
            schema_version: CURRENT_SCHEMA_VERSION,
            top: "M".to_string(),
            nodes,
            edges,
        }
    }

    fn inc_amount(ir: &NetlistIr, id: NodeId) -> Option<u32> {
        match ir.node(id).map(|n| &n.kind) {
            Some(NodeKind::Inc { amount }) => Some(*amount),
            _ => None,
        }
    }

    // --- PassManager / trait wiring (Req 11.1, 10.6) ----------------------------------------

    #[test]
    fn default_pipeline_registers_the_three_builtin_passes_in_order() {
        let pm = PassManager::default_pipeline();
        assert_eq!(
            pm.pass_names(),
            vec!["delay-fold", "algebraic-simplify", "dead-node-elimination"]
        );
    }

    #[test]
    fn register_appends_passes_in_order() {
        let mut pm = PassManager::new();
        pm.register(Box::new(DeadNodeElimination));
        pm.register(Box::new(DelayFold));
        assert_eq!(pm.pass_names(), vec!["dead-node-elimination", "delay-fold"]);
    }

    // --- delay-fold (Req 11.1) --------------------------------------------------------------

    #[test]
    fn delay_fold_collapses_two_incs_into_one() {
        // a -> inc(2) -> inc(3) -> y  ==>  a -> inc(5) -> y
        let ir = &mut ir_with(
            vec![
                node(0, NodeKind::Input, "a"),
                node(1, NodeKind::Inc { amount: 2 }, "inc0"),
                node(2, NodeKind::Inc { amount: 3 }, "inc1"),
                node(3, NodeKind::Output, "y"),
            ],
            vec![
                edge(0, 1, PortRole::Operand),
                edge(1, 2, PortRole::Operand),
                edge(2, 3, PortRole::Operand),
            ],
        );
        DelayFold.run(ir).unwrap();
        // The parent inc (id 1) is gone; the surviving inc has amount 5 and takes `a` directly.
        assert!(ir.node(1).is_none(), "folded-away inc removed");
        assert_eq!(inc_amount(ir, 2), Some(5), "amounts summed");
        let operand = ir.incoming(2).next().expect("inc has an operand");
        assert_eq!(operand.from, 0, "inc now consumes `a` directly");
    }

    #[test]
    fn delay_fold_collapses_a_chain_of_three() {
        // a -> inc(1) -> inc(1) -> inc(1) -> y  ==>  a -> inc(3) -> y
        let ir = &mut ir_with(
            vec![
                node(0, NodeKind::Input, "a"),
                node(1, NodeKind::Inc { amount: 1 }, "i0"),
                node(2, NodeKind::Inc { amount: 1 }, "i1"),
                node(3, NodeKind::Inc { amount: 1 }, "i2"),
                node(4, NodeKind::Output, "y"),
            ],
            vec![
                edge(0, 1, PortRole::Operand),
                edge(1, 2, PortRole::Operand),
                edge(2, 3, PortRole::Operand),
                edge(3, 4, PortRole::Operand),
            ],
        );
        DelayFold.run(ir).unwrap();
        let incs: Vec<&Node> = ir
            .nodes
            .iter()
            .filter(|n| matches!(n.kind, NodeKind::Inc { .. }))
            .collect();
        assert_eq!(incs.len(), 1, "three incs fold to one");
        assert_eq!(inc_amount(ir, incs[0].id), Some(3), "amounts summed to 3");
    }

    #[test]
    fn delay_fold_does_not_fold_when_parent_fans_out() {
        // a -> inc(1) -> inc(1) -> y1 and inc(1) -> y2: the first inc fans out, so no fold.
        let ir = &mut ir_with(
            vec![
                node(0, NodeKind::Input, "a"),
                node(1, NodeKind::Inc { amount: 1 }, "i0"),
                node(2, NodeKind::Inc { amount: 1 }, "i1"),
                node(3, NodeKind::Output, "y1"),
                node(4, NodeKind::Output, "y2"),
            ],
            vec![
                edge(0, 1, PortRole::Operand),
                edge(1, 2, PortRole::Operand),
                edge(2, 3, PortRole::Operand),
                edge(1, 4, PortRole::Operand), // i0 also drives y2
            ],
        );
        DelayFold.run(ir).unwrap();
        assert!(ir.node(1).is_some(), "fan-out inc is preserved");
        assert_eq!(inc_amount(ir, 1), Some(1));
        assert_eq!(inc_amount(ir, 2), Some(1));
    }

    // --- algebraic-simplify (Req 11.2) ------------------------------------------------------

    #[test]
    fn simplify_min_a_a_to_a() {
        // y = min(a, a) -> y = a.
        let ir = &mut ir_with(
            vec![
                node(0, NodeKind::Input, "a"),
                node(1, NodeKind::Min, "m"),
                node(2, NodeKind::Output, "y"),
            ],
            vec![
                edge(0, 1, PortRole::Operand),
                edge(0, 1, PortRole::Operand),
                edge(1, 2, PortRole::Operand),
            ],
        );
        AlgebraicSimplify.run(ir).unwrap();
        assert!(ir.node(1).is_none(), "min node removed");
        let driver = ir.incoming(2).next().expect("output has a driver");
        assert_eq!(driver.from, 0, "output now driven directly by `a`");
    }

    #[test]
    fn simplify_min_a_inf_to_a() {
        // y = min(a, inf) -> y = a.
        let ir = &mut ir_with(
            vec![
                node(0, NodeKind::Input, "a"),
                node(1, NodeKind::Input, "inf"),
                node(2, NodeKind::Min, "m"),
                node(3, NodeKind::Output, "y"),
            ],
            vec![
                edge(0, 2, PortRole::Operand),
                edge(1, 2, PortRole::Operand),
                edge(2, 3, PortRole::Operand),
            ],
        );
        AlgebraicSimplify.run(ir).unwrap();
        assert!(ir.node(2).is_none(), "min node removed");
        let driver = ir.incoming(3).next().expect("output has a driver");
        assert_eq!(driver.from, 0, "output driven by `a`, inf dropped");
    }

    #[test]
    fn simplify_lt_a_a_to_inf() {
        // y = lt(a, a) -> y = inf.
        let ir = &mut ir_with(
            vec![
                node(0, NodeKind::Input, "a"),
                node(1, NodeKind::Lt, "l"),
                node(2, NodeKind::Output, "y"),
            ],
            vec![
                edge(0, 1, PortRole::Pass),
                edge(0, 1, PortRole::Inhibit),
                edge(1, 2, PortRole::Operand),
            ],
        );
        AlgebraicSimplify.run(ir).unwrap();
        assert!(ir.node(1).is_none(), "lt node removed");
        let driver = ir.incoming(2).next().expect("output has a driver");
        let drv = ir.node(driver.from).expect("driver node exists");
        assert!(is_inf_constant(ir, drv), "output driven by inf");
    }

    #[test]
    fn simplify_lt_inf_b_to_inf() {
        // y = lt(inf, b) -> y = inf.
        let ir = &mut ir_with(
            vec![
                node(0, NodeKind::Input, "inf"),
                node(1, NodeKind::Input, "b"),
                node(2, NodeKind::Lt, "l"),
                node(3, NodeKind::Output, "y"),
            ],
            vec![
                edge(0, 2, PortRole::Pass),
                edge(1, 2, PortRole::Inhibit),
                edge(2, 3, PortRole::Operand),
            ],
        );
        AlgebraicSimplify.run(ir).unwrap();
        assert!(ir.node(2).is_none(), "lt node removed");
        let driver = ir.incoming(3).next().expect("output has a driver");
        let drv = ir.node(driver.from).expect("driver node exists");
        assert!(is_inf_constant(ir, drv), "output driven by inf");
    }

    #[test]
    fn simplify_lt_a_inf_to_a() {
        // y = lt(a, inf) -> y = a.
        let ir = &mut ir_with(
            vec![
                node(0, NodeKind::Input, "a"),
                node(1, NodeKind::Input, "inf"),
                node(2, NodeKind::Lt, "l"),
                node(3, NodeKind::Output, "y"),
            ],
            vec![
                edge(0, 2, PortRole::Pass),
                edge(1, 2, PortRole::Inhibit),
                edge(2, 3, PortRole::Operand),
            ],
        );
        AlgebraicSimplify.run(ir).unwrap();
        assert!(ir.node(2).is_none(), "lt node removed");
        let driver = ir.incoming(3).next().expect("output has a driver");
        assert_eq!(driver.from, 0, "output driven by pass input `a`");
    }

    // --- dead-node-elimination (Req 11.3) ---------------------------------------------------

    #[test]
    fn dce_removes_nodes_that_feed_no_output() {
        // a -> y (live); b -> inc (dead, feeds nothing observable).
        let ir = &mut ir_with(
            vec![
                node(0, NodeKind::Input, "a"),
                node(1, NodeKind::Output, "y"),
                node(2, NodeKind::Input, "b"),
                node(3, NodeKind::Inc { amount: 1 }, "dead"),
            ],
            vec![edge(0, 1, PortRole::Operand), edge(2, 3, PortRole::Operand)],
        );
        DeadNodeElimination.run(ir).unwrap();
        assert!(ir.node(0).is_some() && ir.node(1).is_some(), "live chain kept");
        assert!(ir.node(2).is_none() && ir.node(3).is_none(), "dead nodes removed");
        assert_eq!(ir.edges.len(), 1, "only the live edge remains");
    }

    #[test]
    fn dce_keeps_transitive_producers_of_an_output() {
        // a -> inc -> min -> y: everything is live.
        let ir = &mut ir_with(
            vec![
                node(0, NodeKind::Input, "a"),
                node(1, NodeKind::Inc { amount: 1 }, "i"),
                node(2, NodeKind::Min, "m"),
                node(3, NodeKind::Output, "y"),
            ],
            vec![
                edge(0, 1, PortRole::Operand),
                edge(1, 2, PortRole::Operand),
                edge(2, 3, PortRole::Operand),
            ],
        );
        DeadNodeElimination.run(ir).unwrap();
        assert_eq!(ir.nodes.len(), 4, "all transitive producers kept");
    }

    // --- run_all drives the whole pipeline --------------------------------------------------

    #[test]
    fn run_all_folds_simplifies_and_prunes() {
        // y = min(inc(1)(inc(1)(a)), inc(1)(inc(1)(a))) with a dead `b` input.
        // After delay-fold each chain becomes inc(2); after simplify min(x,x)->x; after DCE the
        // dead input is pruned.
        let ir = &mut ir_with(
            vec![
                node(0, NodeKind::Input, "a"),
                node(1, NodeKind::Inc { amount: 1 }, "i0"),
                node(2, NodeKind::Inc { amount: 1 }, "i1"),
                node(3, NodeKind::Min, "m"),
                node(4, NodeKind::Output, "y"),
                node(5, NodeKind::Input, "b"), // dead
            ],
            vec![
                edge(0, 1, PortRole::Operand),
                edge(1, 2, PortRole::Operand),
                edge(2, 3, PortRole::Operand), // min operand 1 = inc chain
                edge(2, 3, PortRole::Operand), // min operand 2 = same inc chain
                edge(3, 4, PortRole::Operand),
            ],
        );
        PassManager::default_pipeline().run_all(ir).unwrap();
        // The dead input `b` is pruned.
        assert!(ir.node(5).is_none(), "dead input pruned by DCE");
        // The min collapsed to its single distinct operand (the folded inc chain).
        assert!(
            !ir.nodes.iter().any(|n| matches!(n.kind, NodeKind::Min)),
            "min(x,x) simplified away"
        );
        // Exactly one inc of amount 2 remains feeding the output.
        let incs: Vec<&Node> = ir
            .nodes
            .iter()
            .filter(|n| matches!(n.kind, NodeKind::Inc { .. }))
            .collect();
        assert_eq!(incs.len(), 1, "the two unit incs folded to one");
        assert_eq!(inc_amount(ir, incs[0].id), Some(2));
    }
}
