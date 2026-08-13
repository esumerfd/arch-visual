//! `verdict.rs`: cached whole-graph Tarjan SCC, cross-seam cycle detection,
//! Clean/Watch/Leaky verdict computation, and `SeamDetail` assembly.
//!
//! Port target: `apps/seam-explorer-web/seam-explorer.html` `analyze()` /
//! `hasCrossCycle()` / `reaches()` (lines 310-373) for the verdict *shape*
//! only — the bounded-BFS cycle-detection heuristic is explicitly NOT
//! ported; it's replaced by petgraph's Tarjan SCC pass (`petgraph::algo`)
//! computed once over the whole graph and cached (01-RESEARCH.md Pattern
//! 4). D-06: the
//! `Verdict::Utility` variant exists for future extensibility but is never
//! constructed in v1.

use crate::model::{CommunityId, Model};
use petgraph::stable_graph::NodeIndex;
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum Verdict {
    Clean,
    Watch,
    Leaky,
    /// D-06: kept for future extensibility — no `meta.utility` signal and no
    /// fan-in heuristic exist in v1, so this variant is never constructed.
    Utility,
}

/// `NodeIndex -> scc_id` map, computed once over the whole graph by
/// `compute_scc` and reused for every `has_cross_cycle`/`seam_detail` call —
/// never recomputed per lookup (precompute-once, 01-RESEARCH.md
/// Anti-Patterns).
#[derive(Debug)]
pub struct SccIndex(pub(crate) HashMap<NodeIndex, usize>);

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SeamDetail {
    pub a: CommunityId,
    pub b: CommunityId,
    pub verdict: Verdict,
    pub a_to_b: usize,
    pub b_to_a: usize,
    pub bridges_a: Vec<String>,
    pub bridges_b: Vec<String>,
    pub reasons: Vec<String>,
}

/// Run petgraph's Tarjan SCC pass over the whole graph exactly once — the
/// only call site in this module — and build a `NodeIndex -> scc_id` cache.
pub fn compute_scc(model: &Model) -> SccIndex {
    let sccs = petgraph::algo::tarjan_scc(&model.graph);
    let mut map = HashMap::new();
    for (scc_id, nodes) in sccs.into_iter().enumerate() {
        for n in nodes {
            map.insert(n, scc_id);
        }
    }
    SccIndex(map)
}

/// O(V) cached lookup: does any single SCC contain at least one node from
/// community `a` and at least one node from community `b`? Takes the
/// already-computed `&SccIndex` — never calls `compute_scc` itself.
pub fn has_cross_cycle(scc: &SccIndex, model: &Model, a: &CommunityId, b: &CommunityId) -> bool {
    let mut seen_scc_with_a: HashSet<usize> = HashSet::new();
    for idx in model.graph.node_indices() {
        if &model.graph[idx].community == a {
            seen_scc_with_a.insert(scc.0[&idx]);
        }
    }
    model
        .graph
        .node_indices()
        .any(|idx| &model.graph[idx].community == b && seen_scc_with_a.contains(&scc.0[&idx]))
}

/// D-06: `Utility` is never returned — the enum variant exists for future
/// extensibility only.
pub fn verdict(bidirectional: bool, has_cross_cycle: bool) -> Verdict {
    if has_cross_cycle {
        Verdict::Leaky
    } else if bidirectional {
        Verdict::Watch
    } else {
        Verdict::Clean
    }
}

fn push_unique(v: &mut Vec<String>, item: String) {
    if !v.contains(&item) {
        v.push(item);
    }
}

/// Assemble the full seam-detail payload: bridge node ids on each side,
/// directed crossing counts, verdict, and human-readable reasons (ported
/// from `analyze()`'s phrasing at `seam-explorer.html` lines 326-343 — the
/// `isUtil` branch is dropped per D-06). Keeps the crossing-count/bridge
/// aggregation consistent with `seams::detect` — the graph passed in is
/// already filtered, so edges are never re-filtered here.
pub fn seam_detail(model: &Model, scc: &SccIndex, a: &CommunityId, b: &CommunityId) -> SeamDetail {
    let mut bridges_a: Vec<String> = Vec::new();
    let mut bridges_b: Vec<String> = Vec::new();
    let mut a_to_b = 0usize;
    let mut b_to_a = 0usize;

    for e in model.graph.edge_indices() {
        let (s, t) = model
            .graph
            .edge_endpoints(e)
            .expect("edge_indices() only yields edges with valid endpoints");
        let sc = &model.graph[s].community;
        let tc = &model.graph[t].community;
        // D-09: every kept edge is directed source -> target.
        if sc == a && tc == b {
            push_unique(&mut bridges_a, model.graph[s].id.clone());
            push_unique(&mut bridges_b, model.graph[t].id.clone());
            a_to_b += 1;
        } else if sc == b && tc == a {
            push_unique(&mut bridges_b, model.graph[s].id.clone());
            push_unique(&mut bridges_a, model.graph[t].id.clone());
            b_to_a += 1;
        }
    }

    let bidirectional = a_to_b > 0 && b_to_a > 0;
    let cycle = has_cross_cycle(scc, model, a, b);
    let v = verdict(bidirectional, cycle);

    let mut reasons = Vec::new();
    if cycle {
        reasons.push(
            "A call cycle crosses the seam — the boundary isn't real; changes on one side ripple back through the other."
                .to_string(),
        );
    } else if bidirectional {
        reasons.push(
            "Calls cross both directions. Clean seams usually flow one way (a caller/callee layering)."
                .to_string(),
        );
    } else {
        reasons.push("Calls flow one direction only — a clean caller→callee layering.".to_string());
    }

    let width = bridges_a.len() + bridges_b.len();
    if width <= 2 {
        reasons.push(format!(
            "Narrow interface: only {width} bridge node{} touch the boundary.",
            if width == 1 { "" } else { "s" }
        ));
    } else if width <= 4 {
        reasons.push(format!(
            "Interface is moderate — {width} bridge nodes across the seam."
        ));
    } else {
        reasons.push(format!(
            "Wide interface: {width} nodes reach across. Consider funnelling through a single port/facade."
        ));
    }

    SeamDetail {
        a: a.clone(),
        b: b.clone(),
        verdict: v,
        a_to_b,
        b_to_a,
        bridges_a,
        bridges_b,
        reasons,
    }
}
