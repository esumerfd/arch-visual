//! Golden + property + cache-reuse tests for `seam_core::verdict` (SEAM-02).
//!
//! RED (Task 1): these tests compile against the stubbed `verdict.rs` (all
//! function bodies are `todo!()`) and are expected to fail/panic. GREEN
//! (Task 2) fills in the implementation without editing this file.

use proptest::prelude::*;
use seam_core::{Model, Node, Verdict};

fn load_model(path: &str) -> Model {
    let raw = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("fixture {path} should exist: {e}"));
    seam_core::from_json(&raw)
        .unwrap_or_else(|e| panic!("fixture {path} should parse: {e}"))
        .model
}

// ---------------------------------------------------------------------
// Golden fixture tests
// ---------------------------------------------------------------------

#[test]
fn leaky_fixture_has_cross_cycle_and_leaky_verdict() {
    let model = load_model("tests/fixtures/leaky.json");
    let scc = seam_core::compute_scc(&model);
    let a = "A".to_string();
    let b = "B".to_string();

    assert!(
        seam_core::has_cross_cycle(&scc, &model, &a, &b),
        "leaky.json's a1<->b1 edges form a genuine cross-community cycle"
    );
    // Per the plan: verdict(_, true) == Leaky regardless of the
    // bidirectional argument.
    assert_eq!(seam_core::verdict(true, true), Verdict::Leaky);
    assert_eq!(seam_core::verdict(false, true), Verdict::Leaky);
}

#[test]
fn watch_fixture_bidirectional_without_cycle_yields_watch() {
    let model = load_model("tests/fixtures/watch.json");
    let scc = seam_core::compute_scc(&model);
    let a = "A".to_string();
    let b = "B".to_string();

    assert!(
        !seam_core::has_cross_cycle(&scc, &model, &a, &b),
        "watch.json's a1->b1 and b2->a2 crossings share no node, so no SCC spans both communities"
    );
    assert_eq!(seam_core::verdict(true, false), Verdict::Watch);
}

#[test]
fn clean_fixture_one_directional_without_cycle_yields_clean() {
    // Reuse Plan 01's clean.json fixture (A->B, B->C, A->C, all one-directional).
    let model = load_model("tests/fixtures/clean.json");
    let scc = seam_core::compute_scc(&model);
    let a = "A".to_string();
    let b = "B".to_string();

    assert!(
        !seam_core::has_cross_cycle(&scc, &model, &a, &b),
        "clean.json's A-B crossings are one-directional (A->B only) with no shared SCC"
    );
    assert_eq!(seam_core::verdict(false, false), Verdict::Clean);
}

// ---------------------------------------------------------------------
// D-06: Utility is never constructed by the three-branch verdict() function
// ---------------------------------------------------------------------

#[test]
fn verdict_never_returns_utility() {
    for bidirectional in [false, true] {
        for cycle in [false, true] {
            let v = seam_core::verdict(bidirectional, cycle);
            assert_ne!(
                v,
                Verdict::Utility,
                "verdict({bidirectional}, {cycle}) must never be Utility (D-06)"
            );
        }
    }
}

// ---------------------------------------------------------------------
// Cache-reuse: compute_scc invoked once; seam_detail/has_cross_cycle only
// ever accept `&SccIndex` (never call compute_scc themselves — enforced at
// the type level here, and separately grep-gated in Task 2's acceptance
// criteria against verdict.rs's source).
// ---------------------------------------------------------------------

#[test]
fn seam_detail_reuses_the_same_cached_scc_across_multiple_pairs() {
    let model = load_model("tests/fixtures/leaky.json");
    let scc = seam_core::compute_scc(&model); // computed exactly once
    let a = "A".to_string();
    let b = "B".to_string();

    let detail1 = seam_core::seam_detail(&model, &scc, &a, &b);
    let detail2 = seam_core::seam_detail(&model, &scc, &a, &b);

    assert_eq!(detail1.verdict, detail2.verdict);
    assert_eq!(detail1.a_to_b, detail2.a_to_b);
    assert_eq!(detail1.b_to_a, detail2.b_to_a);
    assert_eq!(detail1.verdict, Verdict::Leaky);
}

// ---------------------------------------------------------------------
// Property test: has_cross_cycle agrees with a brute-force reachability
// oracle (any node of community `a` reaches a node of community `b` AND
// vice versa) across randomly generated small directed graphs.
// ---------------------------------------------------------------------

fn build_model(communities: &[bool], raw_edges: &[(usize, usize)]) -> Model {
    use petgraph::stable_graph::StableDiGraph;
    use std::collections::HashMap;

    let mut graph = StableDiGraph::new();
    let mut indices = Vec::with_capacity(communities.len());
    for (i, is_a) in communities.iter().enumerate() {
        let community = if *is_a { "A" } else { "B" }.to_string();
        let idx = graph.add_node(Node {
            id: i.to_string(),
            label: i.to_string(),
            community,
            file_type: None,
            community_name: None,
            source_file: None,
            source_line: None,
        });
        indices.push(idx);
    }

    let n = communities.len();
    for &(s, t) in raw_edges {
        let (s, t) = (s % n, t % n);
        graph.add_edge(indices[s], indices[t], ());
    }

    let mut index = HashMap::new();
    for (i, idx) in indices.iter().enumerate() {
        index.insert(i.to_string(), *idx);
    }

    Model {
        graph,
        index,
        ..Default::default()
    }
}

/// Brute-force reachability oracle: true iff some node of community `a` and
/// some node of community `b` can reach each other (i.e. share a strongly
/// connected component).
fn brute_force_has_cross_cycle(model: &Model, a: &str, b: &str) -> bool {
    use petgraph::algo::has_path_connecting;

    let a_nodes: Vec<_> = model
        .graph
        .node_indices()
        .filter(|&i| model.graph[i].community == a)
        .collect();
    let b_nodes: Vec<_> = model
        .graph
        .node_indices()
        .filter(|&i| model.graph[i].community == b)
        .collect();

    for &an in &a_nodes {
        for &bn in &b_nodes {
            let a_to_b = has_path_connecting(&model.graph, an, bn, None);
            let b_to_a = has_path_connecting(&model.graph, bn, an, None);
            if a_to_b && b_to_a {
                return true;
            }
        }
    }
    false
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    #[test]
    fn has_cross_cycle_matches_brute_force_oracle(
        communities in prop::collection::vec(any::<bool>(), 2..=6),
        raw_edges in prop::collection::vec((0usize..6, 0usize..6), 0..=15),
    ) {
        let model = build_model(&communities, &raw_edges);
        let scc = seam_core::compute_scc(&model);

        let expected = brute_force_has_cross_cycle(&model, "A", "B");
        let actual = seam_core::has_cross_cycle(&scc, &model, &"A".to_string(), &"B".to_string());

        prop_assert_eq!(actual, expected);
    }
}
