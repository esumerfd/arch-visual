//! `trace_path`: directed shortest hop-count path between two nodes, plus
//! the community-pair transitions ("seams") it crosses along the way.
//!
//! Port target: `apps/seam-explorer-web/seam-explorer.html` `tracePath()`
//! (lines 596-613) — replaces its hand-rolled BFS/predecessor-map with a
//! single call into petgraph's `astar` algorithm module (unit edge cost,
//! zero heuristic), which returns the reconstructed path directly
//! (03-RESEARCH.md Pattern 1).

use crate::model::{CommunityId, Model};

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct TracePath {
    pub hops: Vec<String>,
    pub seams_crossed: Vec<(CommunityId, CommunityId)>,
}

/// Directed shortest (unit hop-count) path from `from` to `to`. Returns
/// `None` when either id is unknown (Pitfall 3 — `?` on `Option`, never
/// `.unwrap()`/`.expect()`) or when no directed path exists. `seams_crossed`
/// preserves traversal direction (not normalized `a < b` like `seams::detect`
/// — Pitfall 4) and is built only from `Node.community` (already a `String`
/// — Pitfall 5, never re-derived from raw ingest data).
pub fn trace_path(model: &Model, from: &str, to: &str) -> Option<TracePath> {
    let from_idx = *model.index.get(from)?;
    let to_idx = *model.index.get(to)?;

    let (_cost, path) = petgraph::algo::astar(
        &model.graph,
        from_idx,
        |n| n == to_idx,
        |_edge| 1usize,
        |_n| 0usize,
    )?;

    let hops: Vec<String> = path.iter().map(|&i| model.graph[i].id.clone()).collect();

    let mut seams_crossed = Vec::new();
    for w in path.windows(2) {
        let ca = &model.graph[w[0]].community;
        let cb = &model.graph[w[1]].community;
        if ca != cb {
            seams_crossed.push((ca.clone(), cb.clone()));
        }
    }

    Some(TracePath { hops, seams_crossed })
}
