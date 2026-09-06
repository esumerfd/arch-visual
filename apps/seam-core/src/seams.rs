//! `detect`: crossing-edge aggregation by unordered community pair, ranked
//! by crossing count descending.
//!
//! Port target: `apps/seam-explorer-web/seam-explorer.html` `seams()`
//! (lines 295-308) — exact 1:1 algorithm match (RESEARCH.md Pattern 3).
//! D-07: no minimum-community-size filter — keep parity with the JS version,
//! which has none.

use crate::model::{CommunityId, Model};
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Seam {
    pub a: CommunityId,
    pub b: CommunityId,
    pub crossings: usize,
}

/// Group crossing edges by the unordered community-pair key (`a < b`), skip
/// same-community edges, count crossings per pair, and rank by crossing
/// count descending — matches the JS `seams()`'s
/// `.sort((x,y)=>y.edges.length-x.edges.length)`.
///
/// The ranking is **total-ordered**: crossing count descending, then the
/// pair ascending lexicographically (`a`, then `b`). The secondary keys are
/// not decoration. Without them the order among seams tied on `crossings`
/// falls out of `groups`' `HashMap` iteration, and Rust seeds every
/// `HashMap` instance independently — so the same `&Model` ranked twice in
/// ONE process gave two different lists (09-RESEARCH.md reproduced 24 of 25
/// successive calls differing; 08-VERIFICATION.md W1 found it first). That
/// is a correctness defect for Phase 9's time travel, whose whole premise is
/// that re-navigating to the same event shows the same ranked list.
///
/// This is the single ranking authority for BOTH the live path and the
/// replay path — do not wrap it in a replay-local sort, which would leave
/// the live list unstable.
///
/// Ascending-lexicographic is the tie-break convention this crate already
/// uses in [`crate::apply::resolve_community`] and
/// [`crate::model::resolve_community_names`], reused rather than reinvented.
pub fn detect(model: &Model) -> Vec<Seam> {
    let mut groups: HashMap<(CommunityId, CommunityId), usize> = HashMap::new();

    for e in model.graph.edge_indices() {
        let (s, t) = model
            .graph
            .edge_endpoints(e)
            .expect("edge_indices() only yields edges with valid endpoints");
        let ca = &model.graph[s].community;
        let cb = &model.graph[t].community;
        if ca == cb {
            continue; // not a seam
        }
        let key = if ca < cb {
            (ca.clone(), cb.clone())
        } else {
            (cb.clone(), ca.clone())
        };
        *groups.entry(key).or_insert(0) += 1;
    }

    let mut seams: Vec<Seam> = groups
        .into_iter()
        .map(|((a, b), crossings)| Seam { a, b, crossings })
        .collect();
    seams.sort_by(|x, y| {
        y.crossings
            .cmp(&x.crossings)
            .then_with(|| x.a.cmp(&y.a))
            .then_with(|| x.b.cmp(&y.b))
    });
    seams
}
