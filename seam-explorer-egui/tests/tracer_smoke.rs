//! End-to-end tracer smoke test (M1): drives `load::read_and_ingest` directly
//! (no window, no `egui::Context`) to prove GRAPH-01/SEAM-01 work before any
//! UI code is trusted. Written FIRST — the crate has no `load` module yet,
//! so this must fail to compile until Task 2's GREEN step adds it.

use seam_explorer_egui::load::{self, LoadError};

const SAMPLE_GRAPH: &str = include_str!("../../sample/graph.json");

#[test]
fn smoke_sample_graph_produces_seams() {
    let outcome =
        load::read_and_ingest(SAMPLE_GRAPH).expect("sample graph.json must ingest cleanly");

    assert!(
        outcome.model.graph.node_count() > 0,
        "ingested model must have nodes"
    );
    assert!(
        !outcome.seams.is_empty(),
        "sample graph must produce at least one seam"
    );

    let crossings: Vec<usize> = outcome.seams.iter().map(|s| s.crossings).collect();
    let mut sorted_desc = crossings.clone();
    sorted_desc.sort_by(|a, b| b.cmp(a));
    assert_eq!(
        crossings, sorted_desc,
        "seams must be ranked by crossing count descending (SEAM-01)"
    );
}

#[test]
fn smoke_malformed_json_is_error_not_panic() {
    let result = load::read_and_ingest("{ not json");
    match result {
        Err(LoadError::Core(_)) => {}
        other => panic!("expected LoadError::Core for malformed JSON, got {other:?}"),
    }
}

#[test]
fn smoke_scc_is_finalized() {
    let outcome =
        load::read_and_ingest(SAMPLE_GRAPH).expect("sample graph.json must ingest cleanly");
    assert!(
        outcome.model.scc.is_some(),
        "finalize_scc must run during ingest so seam_detail never hits NoGraphLoaded"
    );
}
