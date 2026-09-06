//! Plan 09-01 Task 2: `Model` snapshot independence.
//!
//! Plan 09-02's replay mechanism retains ONE baseline snapshot of the
//! originally-loaded graph and clones it per navigation. Nothing in the app
//! retains such a baseline today -- `apply_load_outcome` sets `app.model`
//! once and `drain_and_apply` mutates it in place forever after -- and
//! `Model` could not be cloned at all before this plan.
//!
//! These three tests are the standing proof of the three properties that
//! mechanism rests on: a clone carries the WHOLE graph (including the SCC
//! cache), mutating a clone through the REAL `apply_batch` path leaves the
//! original untouched, and a clone ranks identically to its original.

use seam_core::{apply_batch, detect, from_json, GraphEvent, Model};

/// 6 nodes across three communities (A: a1/a2, B: b1/b2, C: c1/c2).
const CLEAN_FIXTURE: &str = include_str!("fixtures/clean.json");

/// 09-01 Task 1's fixture: all six community pairs tied at 1 crossing, so
/// the ranking is decided entirely by `detect`'s tie-break.
const TIED_FIXTURE: &str = include_str!("fixtures/tied_seams.json");

fn scored_model(raw: &str) -> Model {
    let mut model = from_json(raw).expect("fixture must ingest cleanly").model;
    model.finalize_scc();
    model
}

/// A clone that silently dropped the SCC cache would make every replayed
/// seam verdict in plan 09-02 fall back to `Clean` -- wrong, and quiet.
#[test]
fn a_cloned_model_carries_the_whole_graph() {
    let original = scored_model(CLEAN_FIXTURE);
    let clone = original.clone();

    assert_eq!(
        clone.graph.node_count(),
        original.graph.node_count(),
        "clone must carry every node"
    );
    assert_eq!(
        clone.graph.edge_count(),
        original.graph.edge_count(),
        "clone must carry every edge"
    );

    let mut original_ids: Vec<&String> = original.index.keys().collect();
    let mut clone_ids: Vec<&String> = clone.index.keys().collect();
    original_ids.sort();
    clone_ids.sort();
    assert_eq!(
        clone_ids, original_ids,
        "clone must carry the whole id index"
    );

    assert_eq!(
        clone.community_names, original.community_names,
        "clone must carry the resolved community names"
    );
    assert!(
        original.scc.is_some(),
        "guard: the original must actually be scored, or the next assertion \
         passes vacuously"
    );
    assert!(
        clone.scc.is_some(),
        "clone must carry the SCC cache -- dropping it would silently \
         degrade every replayed seam verdict to Clean"
    );
}

/// Routing the mutation through the REAL `apply_batch` rather than poking
/// `graph` directly is deliberate: it is the exact path plan 09-02's replay
/// uses, so this proves independence of the operation that will actually run.
#[test]
fn mutating_a_clone_leaves_the_original_untouched() {
    let original = scored_model(CLEAN_FIXTURE);
    let baseline_nodes = original.graph.node_count();
    let baseline_index_len = original.index.len();
    assert_eq!(baseline_nodes, 6, "guard: clean.json ingests to 6 nodes");
    assert!(
        original.index.contains_key("c2"),
        "guard: the node this test removes must exist to begin with"
    );

    let mut clone = original.clone();

    apply_batch(
        &mut clone,
        &[GraphEvent::AddNode {
            id: "z1".to_string(),
            label: "z1".to_string(),
            community: Some("A".to_string()),
            source_file: None,
        }],
    );
    assert_eq!(
        clone.graph.node_count(),
        baseline_nodes + 1,
        "the clone must actually have gained the added node"
    );

    apply_batch(
        &mut clone,
        &[GraphEvent::RemoveNode {
            id: "c2".to_string(),
        }],
    );
    assert_eq!(
        clone.graph.node_count(),
        baseline_nodes,
        "the clone must actually have lost the removed node"
    );
    assert!(
        !clone.index.contains_key("c2"),
        "the clone must no longer index the removed node"
    );

    // The whole point: none of the above reached the original.
    assert_eq!(
        original.graph.node_count(),
        baseline_nodes,
        "the original's node count must be untouched by the clone's mutations"
    );
    assert_eq!(
        original.index.len(),
        baseline_index_len,
        "the original's id index must be untouched by the clone's mutations"
    );
    assert!(
        original.index.contains_key("c2"),
        "the node removed from the clone must still be present in the original"
    );
    assert!(
        !original.index.contains_key("z1"),
        "the node added to the clone must never appear in the original"
    );
}

/// Ties Task 1's determinism guarantee to Task 2's snapshot mechanism: what
/// plan 09-02 actually does is rank a clone and compare it against what the
/// live model shows.
#[test]
fn a_cloned_model_ranks_identically_to_its_original() {
    let original = scored_model(TIED_FIXTURE);
    let clone = original.clone();

    let seams = detect(&original);
    assert_eq!(
        seams.len(),
        6,
        "guard: the tied fixture must produce all six pairs"
    );
    assert!(
        seams.iter().all(|s| s.crossings == 1),
        "guard: every pair must be tied, or the tie-break is never exercised"
    );

    assert_eq!(
        detect(&clone),
        seams,
        "a clone must rank identically to its original, including among ties"
    );
}
