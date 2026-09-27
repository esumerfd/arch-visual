//! End-to-end tracer smoke test (M1): drives `load::read_and_ingest` directly
//! (no window, no `egui::Context`) to prove GRAPH-01/SEAM-01 work before any
//! UI code is trusted. Written FIRST — the crate has no `load` module yet,
//! so this must fail to compile until Task 2's GREEN step adds it.

use seam_explorer_egui::load::{self, LoadError};

const SAMPLE_GRAPH: &str = include_str!("../../../sample/graph.json");

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

/// quick-260926-xbl: end-to-end proof of the hard test-code exclusion filter
/// against the real `sample/graph.json` (finding 9, measured at planning
/// time). `MockAgentRuntime` is caught by the NAME rule via its raw label
/// even though its path (`orchestrator/src/handlers/ai_agent.rs`) carries
/// zero test signal -- this is the motivating case for the whole filter.
#[test]
fn smoke_real_sample_excludes_test_code_and_mockagentruntime_by_exact_id() {
    let outcome =
        load::read_and_ingest(SAMPLE_GRAPH).expect("sample graph.json must ingest cleanly");

    assert_eq!(outcome.model.graph.node_count(), 782);
    assert_eq!(outcome.model.graph.edge_count(), 1328);
    assert_eq!(
        outcome.excluded_test_code,
        seam_core::TestCodeExcluded {
            nodes: 315,
            by_path: 312,
            by_name: 3,
            edges: 685,
        }
    );
    assert_eq!(outcome.seams.len(), 49);

    // Exact id equality, NOT a substring test: the five
    // `..._mockagentruntime_new`-style method nodes legitimately survive
    // (260926-xbl-PLAN.md finding 10) and a substring assertion would
    // wrongly fail on them.
    assert!(
        !outcome
            .model
            .index
            .contains_key("orchestrator_src_handlers_ai_agent_mockagentruntime"),
        "MockAgentRuntime's own node must be excluded by exact id"
    );
    assert!(
        outcome
            .model
            .graph
            .node_weights()
            .all(|n| n.label != "MockAgentRuntime" && n.label != "mockagentruntime"),
        "no surviving node's label may equal MockAgentRuntime/mockagentruntime"
    );
}
