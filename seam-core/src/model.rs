//! Graph model: `Node`/`Model` wrapping a `petgraph::StableDiGraph`, plus the
//! id -> `NodeIndex` map that replaces the JS prototype's `byId` Map.
//!
//! Port target: `apps/seam-explorer-web/seam-explorer.html` `normalize()`
//! (lines 270-292) — id/label/community shape only; `adj`/`radj` maps are
//! NOT ported (petgraph's own edge iteration replaces them, per
//! 01-RESEARCH.md "Don't Hand-Roll").

use crate::verdict::SccIndex;
use petgraph::stable_graph::{NodeIndex, StableDiGraph};
use std::collections::HashMap;

/// Community identifier. The real `sample/graph.json`'s `community` field is
/// a JSON integer (e.g. `44`), not a string — `ingest.rs` tolerantly
/// stringifies it at parse time so every downstream consumer (`seams.rs`)
/// only ever sees a `String`.
pub type CommunityId = String;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Node {
    pub id: String,
    pub label: String,
    pub community: CommunityId,
    pub file_type: Option<String>,
}

/// The in-memory graph produced by `ingest::from_json`. All edges present in
/// `graph` have already passed the D-01/D-02/D-03/D-04 relation+confidence
/// filter exactly once, at ingest time (see `ingest.rs`) — no downstream
/// consumer (`seams::detect`) re-filters.
#[derive(Debug, Default)]
pub struct Model {
    pub graph: StableDiGraph<Node, ()>,
    pub index: HashMap<String, NodeIndex>,
    /// Populated once by `finalize_scc` — the future app-shell crate's
    /// command layer calls this inside `spawn_blocking` right after ingest.
    /// `verdict::seam_detail`/`has_cross_cycle` take an `&SccIndex`
    /// explicitly rather than reading this field, so this slot is a
    /// load-time persistence point, not a dependency of `verdict.rs`'s own
    /// functions.
    pub scc: Option<SccIndex>,
}

impl Model {
    /// Compute this graph's whole-graph Tarjan SCC index once and cache it.
    /// Call exactly once per load (e.g. right after `ingest::from_json`) —
    /// never per seam-detail lookup (precompute-once, see `verdict.rs`).
    pub fn finalize_scc(&mut self) {
        self.scc = Some(crate::verdict::compute_scc(self));
    }
}

/// Pure conflict-resolution function (05-09, DP-09-01/DP-09-02): given every
/// `(community, community_name)` pair observed across a graph's nodes,
/// return the one resolved name per community that has a usable name.
///
/// - Blank (empty or whitespace-only, after trimming) and absent names are
///   never candidates (DP-09-02) — a community whose every node is blank or
///   unnamed gets no entry at all.
/// - The most frequent non-blank candidate wins (DP-09-01, majority-wins).
/// - An exact tie is broken by taking the lexicographically smallest
///   candidate, so the result never depends on `HashMap` iteration order or
///   insertion order (verified by `resolves_a_tied_community_deterministically_by_lexical_order`,
///   which repeats the assertion across 20 fresh calls).
///
/// Free function, not a method — driven directly by hand-built pairs in
/// tests, with no `Model`/graph required.
pub fn resolve_community_names(
    entries: &[(CommunityId, Option<String>)],
) -> HashMap<CommunityId, String> {
    let mut counts: HashMap<CommunityId, HashMap<String, usize>> = HashMap::new();
    for (community, name) in entries {
        let Some(name) = name else { continue };
        let trimmed = name.trim();
        if trimmed.is_empty() {
            continue;
        }
        *counts
            .entry(community.clone())
            .or_default()
            .entry(trimmed.to_string())
            .or_insert(0) += 1;
    }

    counts
        .into_iter()
        .filter_map(|(community, candidates)| {
            candidates
                // Comparator sorts on count first (higher wins), then on the
                // *reverse* of name ordering so that, on an exact count tie,
                // the lexicographically smallest name is judged "greater"
                // and wins — independent of HashMap iteration order.
                .into_iter()
                .max_by(|a, b| a.1.cmp(&b.1).then_with(|| b.0.cmp(&a.0)))
                .map(|(name, _)| (community, name))
        })
        .collect()
}
