//! Failing (RED) tests for `seam_core::ingest::from_json`. See
//! 01-01-PLAN.md Task 2's `<behavior>` block for the exact contract these
//! assert. Implementation lands in Task 3 (GREEN).

use seam_core::{from_json, SeamCoreError, STRUCTURAL_RELATIONS};

const REAL_GRAPH: &str = include_str!("fixtures/graph.json");
const CLEAN_FIXTURE: &str = include_str!("fixtures/clean.json");
const MALFORMED_FIXTURE: &str = include_str!("fixtures/malformed.json");

#[test]
fn structural_relations_allowlist_matches_locked_decision_d01() {
    assert_eq!(STRUCTURAL_RELATIONS.len(), 5);
    for r in ["calls", "references", "method", "implements", "imports_from"] {
        assert!(
            STRUCTURAL_RELATIONS.contains(&r),
            "D-01 allow-list missing relation: {r}"
        );
    }
}

#[test]
fn ingests_real_sample_with_exact_filtered_counts_and_zero_warnings() {
    let result = from_json(REAL_GRAPH).expect("real sample/graph.json must parse successfully");
    assert_eq!(
        result.model.graph.edge_count(),
        3212,
        "structural relations {{calls,references,method,implements,imports_from}} at \
         confidence EXTRACTED only must total exactly 3212 edges"
    );
    assert_eq!(result.model.graph.node_count(), 1594);
    assert!(
        result.warnings.is_empty(),
        "real sample/graph.json has zero dangling edges — warnings must be empty"
    );
}

#[test]
fn drops_contains_semantic_and_inferred_edges_keeps_structural_extracted() {
    let result = from_json(CLEAN_FIXTURE).expect("clean fixture must parse");
    // clean.json has 7 structural+EXTRACTED edges (including one same-community
    // edge a1->a2) and 3 edges that must be dropped: one `contains` (D-02),
    // one `shares_data_with` (D-03), one INFERRED `calls` (D-04).
    assert_eq!(
        result.model.graph.edge_count(),
        7,
        "only structural-relation, EXTRACTED-confidence edges should be added to the graph"
    );
}

#[test]
fn malformed_fixture_drops_dangling_edge_and_records_non_empty_warning() {
    let result =
        from_json(MALFORMED_FIXTURE).expect("malformed fixture must NOT error — drop, don't fail");
    assert!(
        !result.warnings.is_empty(),
        "an edge whose target id is absent from nodes[] must produce a warning, never be silently dropped"
    );
    let warning = result
        .warnings
        .iter()
        .find(|w| w.target == "b1_ghost")
        .expect("warning must name the offending dropped edge's target");
    assert_eq!(warning.source, "a1");
}

#[test]
fn fatal_invalid_json_is_not_ok_and_is_distinguishable_from_missing_array() {
    let err = from_json("{ not json").expect_err(
        "non-JSON input must be a fatal Err, never an Ok — the file isn't parseable at all",
    );
    assert!(
        matches!(err, SeamCoreError::Parse(_)),
        "invalid JSON must produce the Parse variant (a syntax failure), distinct from \
         MissingArray (a structural failure on otherwise-valid JSON); got {err:?}"
    );
}

#[test]
fn fatal_missing_nodes_array_is_not_ok_with_an_empty_graph() {
    let err = from_json(r#"{"links": []}"#).expect_err(
        "a document with no `nodes` array must be a fatal Err naming the missing array — \
         never a silently-empty-graph Ok",
    );
    match err {
        SeamCoreError::MissingArray(name) => assert_eq!(name, "nodes"),
        other => panic!("expected SeamCoreError::MissingArray(\"nodes\"), got {other:?}"),
    }
}

#[test]
fn fatal_missing_links_array_is_not_ok_with_an_empty_graph() {
    let err = from_json(r#"{"nodes": []}"#).expect_err(
        "a document with no `links` array must be a fatal Err naming the missing array — \
         never a silently-empty-graph Ok",
    );
    match err {
        SeamCoreError::MissingArray(name) => assert_eq!(name, "links"),
        other => panic!("expected SeamCoreError::MissingArray(\"links\"), got {other:?}"),
    }
}

#[test]
fn fatal_missing_array_never_produces_ok() {
    // Regression guard for the exact anti-pattern this task rules out: a
    // missing `nodes`/`links` array must never resolve to Ok(IngestResult)
    // with an empty graph.
    assert!(from_json(r#"{}"#).is_err(), "a doc with neither array present must be fatal");
}
