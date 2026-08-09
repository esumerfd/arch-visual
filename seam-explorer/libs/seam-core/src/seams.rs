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
    seams.sort_by(|x, y| y.crossings.cmp(&x.crossings));
    seams
}
