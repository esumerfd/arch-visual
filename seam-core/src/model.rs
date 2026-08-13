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
