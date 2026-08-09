//! Failing (RED) tests for `seam_core::trace_path`. See 03-01-PLAN.md Task 1's
//! `<behavior>` block for the exact contract these assert.
//! Implementation lands in Task 2 (GREEN).
//!
//! Deviation from the plan's literal node choice: the plan's `<behavior>`
//! describes a directed multi-hop path `a1 -> b1 -> c1`, but `clean.json`
//! also contains a direct `a1 -> c1` edge (`imports_from`, EXTRACTED — passes
//! the ingest filter), which is a shorter (1-hop) path than `a1 -> b1 -> c1`.
//! `petgraph::algo::astar` (unit edge cost) would therefore return the
//! direct 1-hop path for `a1 -> c1`, not the 2-hop path the plan describes.
//! `a2` has exactly one outgoing kept edge (`a2 -> b1`) and no direct edge to
//! `c1`, so `a2 -> b1 -> c1` is the fixture's only genuine multi-hop,
//! no-shortcut path — used here in place of `a1` for the multi-hop
//! assertions so the test actually exercises the described behavior.

use seam_core::{from_json, trace_path};

const CLEAN_FIXTURE: &str = include_str!("fixtures/clean.json");

#[test]
fn trace_finds_directed_path() {
    let ingest = from_json(CLEAN_FIXTURE).expect("clean fixture must parse");
    let result = trace_path(&ingest.model, "a2", "c1").expect("a2 -> c1 must have a directed path via b1");
    assert_eq!(result.hops, vec!["a2".to_string(), "b1".to_string(), "c1".to_string()]);
}

#[test]
fn trace_returns_none_for_unreachable() {
    let ingest = from_json(CLEAN_FIXTURE).expect("clean fixture must parse");
    // c1 has no outgoing edges in the kept (post-filter) graph, so no
    // directed path back to a1 exists.
    let result = trace_path(&ingest.model, "c1", "a1");
    assert_eq!(result, None, "c1 -> a1 must be None: no directed path exists");
}

#[test]
fn trace_unknown_id_returns_none() {
    let ingest = from_json(CLEAN_FIXTURE).expect("clean fixture must parse");
    let result = trace_path(&ingest.model, "does-not-exist", "c1");
    assert_eq!(result, None, "unknown source id must return None, not panic");
}

#[test]
fn trace_lists_crossed_seams() {
    let ingest = from_json(CLEAN_FIXTURE).expect("clean fixture must parse");
    let result = trace_path(&ingest.model, "a2", "c1").expect("a2 -> c1 must have a directed path");
    assert_eq!(
        result.seams_crossed,
        vec![("A".to_string(), "B".to_string()), ("B".to_string(), "C".to_string())],
        "seams_crossed must list community transitions in traversal order, direction preserved"
    );
}
