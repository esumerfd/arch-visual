//! Plan 08-01: unit coverage of `apply.rs`'s pure half -- community
//! resolution (D-04/D-04a), client-style id reconciliation (Phase 7
//! DP-07-01), the EVENT-05 community-immutability guard, and removal.
//!
//! No socket and no egui here on purpose: `seam-core` must stay renderer-free
//! and app-shell-free (its Phase-1 mandate), so the live-pipeline half of
//! this plan's coverage lives in
//! `apps/seam-explorer-egui/tests/live_apply.rs` instead.

use seam_core::{GraphEvent, Model};

/// 6 nodes across three communities (A: a1/a2, B: b1/b2, C: c1/c2). a1
/// carries `source_file == "src/auth/login.rs"`, a2
/// `"src/auth/session.rs"`; b1/b2 carry none.
const SOURCE_PATHS_FIXTURE: &str = include_str!("fixtures/source_paths.json");

fn fixture_model() -> Model {
    let mut model = seam_core::from_json(SOURCE_PATHS_FIXTURE)
        .expect("fixture must ingest cleanly")
        .model;
    model.finalize_scc();
    model
}

fn community_of(model: &Model, id: &str) -> String {
    let idx = *model
        .index
        .get(id)
        .unwrap_or_else(|| panic!("node {id} must exist"));
    model.graph[idx].community.clone()
}

// ---------------------------------------------------------------------
// D-04 / D-04a: community resolution
// ---------------------------------------------------------------------

#[test]
fn an_absent_community_inherits_from_a_source_file_sibling() {
    let mut model = fixture_model();
    let sibling = community_of(&model, "a1");

    seam_core::apply_add_node(
        &mut model,
        "src/auth/login.rs::verify_token",
        "verify_token",
        None,
        Some("src/auth/login.rs"),
    );

    assert_eq!(
        community_of(&model, "src/auth/login.rs::verify_token"),
        sibling,
        "a node must inherit the community of a node sharing its source_file"
    );
}

#[test]
fn an_absent_community_with_no_sibling_becomes_the_unknown_sentinel() {
    let mut model = fixture_model();

    seam_core::apply_add_node(
        &mut model,
        "src/brand/new.rs::freshly_written",
        "freshly_written",
        None,
        Some("src/brand/new.rs"),
    );

    assert_eq!(
        community_of(&model, "src/brand/new.rs::freshly_written"),
        seam_core::UNKNOWN_COMMUNITY,
        "an unresolvable community must be the single reserved sentinel bucket"
    );
}

#[test]
fn an_explicit_community_on_the_wire_is_used_verbatim() {
    let mut model = fixture_model();
    let explicit = "Z".to_string();

    seam_core::apply_add_node(
        &mut model,
        "src/auth/login.rs::explicitly_placed",
        "explicitly_placed",
        Some(&explicit),
        // A source_file whose sibling would resolve to "A" -- so this test
        // fails if the explicit value is ever overridden by inheritance.
        Some("src/auth/login.rs"),
    );

    assert_eq!(
        community_of(&model, "src/auth/login.rs::explicitly_placed"),
        "Z",
        "an explicit wire community must win over sibling inheritance"
    );
}

// ---------------------------------------------------------------------
// EVENT-05: an originally-loaded node never moves between communities
// ---------------------------------------------------------------------

#[test]
fn re_adding_an_existing_id_updates_in_place_and_never_moves_its_community() {
    let mut model = fixture_model();
    let before = community_of(&model, "a1");
    let node_count_before = model.graph.node_count();
    let hostile = "Z".to_string();

    seam_core::apply_add_node(&mut model, "a1", "a1_renamed", Some(&hostile), None);

    assert_eq!(
        community_of(&model, "a1"),
        before,
        "EVENT-05: an event must never move a loaded node between communities"
    );
    assert_eq!(
        model.graph.node_count(),
        node_count_before,
        "re-adding an existing id must update in place, never grow the graph"
    );
    let idx = model.index["a1"];
    assert_eq!(
        model.graph[idx].label, "a1_renamed",
        "the update must still land on the fields it is allowed to touch"
    );
}

// ---------------------------------------------------------------------
// Phase 7 DP-07-01: {source_file}::{symbol} identity reconciliation
// ---------------------------------------------------------------------

#[test]
fn a_client_style_id_reconciles_onto_the_graphs_own_opaque_id() {
    let mut model = fixture_model();
    let node_count_before = model.graph.node_count();
    // The fixture's node for src/auth/login.rs is `a1`, with label `a1` --
    // the client would advertise it as "src/auth/login.rs::a1".
    const CLIENT_STYLE_ID: &str = "src/auth/login.rs::a1";

    seam_core::apply_add_node(
        &mut model,
        CLIENT_STYLE_ID,
        "a1",
        None,
        Some("src/auth/login.rs"),
    );

    assert_eq!(
        model.graph.node_count(),
        node_count_before,
        "a client-style id matching an existing node must not insert a second node"
    );
    assert!(
        !model.index.contains_key(CLIENT_STYLE_ID),
        "the graph's own opaque id must keep owning that node"
    );
    assert!(
        model.index.contains_key("a1"),
        "the original opaque id must survive"
    );
}

// ---------------------------------------------------------------------
// D-03: removal
// ---------------------------------------------------------------------

#[test]
fn removing_a_node_that_does_not_exist_is_a_silent_no_op() {
    let mut model = fixture_model();
    let node_count_before = model.graph.node_count();
    let edge_count_before = model.graph.edge_count();

    let removed = seam_core::apply_remove_node(&mut model, "no_such_node");

    assert_eq!(removed, None, "an unknown id must report nothing removed");
    assert_eq!(model.graph.node_count(), node_count_before);
    assert_eq!(model.graph.edge_count(), edge_count_before);
}

// ---------------------------------------------------------------------
// Edge events are deferred, not applied and not lost (08-04 owns them)
// ---------------------------------------------------------------------

#[test]
fn edge_events_are_carried_forward_as_deferred_data_not_dropped() {
    let mut model = fixture_model();
    let edge_count_before = model.graph.edge_count();
    let edge = GraphEvent::AddEdge {
        source: "src/auth/login.rs".to_string(),
        target: "verify_token".to_string(),
    };

    let outcome = seam_core::apply_batch(&mut model, std::slice::from_ref(&edge));

    assert_eq!(
        outcome.deferred_edges,
        vec![edge],
        "an edge event must be handed forward verbatim as deferred data"
    );
    assert!(
        outcome.applied.is_empty(),
        "an edge event must not be reported as applied"
    );
    assert!(!outcome.topology_changed);
    assert_eq!(
        model.graph.edge_count(),
        edge_count_before,
        "08-04 owns edge application -- this plan must not apply one"
    );
}

// ---------------------------------------------------------------------
// 08-RESEARCH.md Pitfall 1: the stale-SCC hazard
// ---------------------------------------------------------------------

/// CHARACTERIZATION test, not a RED that later turns green: it pins the
/// hazard itself, and is expected to pass both before and after the fix.
/// `verdict::has_cross_cycle` indexes the cached `NodeIndex -> scc_id` map
/// raw, so scoring a seam against a cache older than the model is a panic,
/// never a wrong number. The fix (`history::drain_and_apply` recomputing the
/// cache) removes the only way to REACH this state in the live app; it does
/// not and cannot make the raw index safe, which is exactly why this test
/// must keep asserting the panic.
#[test]
#[should_panic(expected = "no entry found for key")]
fn a_stale_scc_cache_panics_when_a_new_node_is_scored() {
    let mut model = fixture_model();
    let stale = model
        .scc
        .take()
        .expect("finalize_scc must have populated the cache");

    seam_core::apply_add_node(
        &mut model,
        "src/auth/login.rs::verify_token",
        "verify_token",
        None,
        Some("src/auth/login.rs"),
    );

    let a = community_of(&model, "src/auth/login.rs::verify_token");
    let b = community_of(&model, "b1");
    let _ = seam_core::seam_detail(&model, &stale, &a, &b);
}

/// The structural invariant, checked directly rather than by waiting for a
/// panic to surface a violation: after a mixed batch, every node index in
/// the model has an entry in a freshly finalized SCC cache.
#[test]
fn every_node_index_in_the_model_has_an_scc_entry_after_an_applied_batch() {
    let mut model = fixture_model();

    let outcome = seam_core::apply_batch(
        &mut model,
        &[
            GraphEvent::AddNode {
                id: "src/auth/login.rs::verify_token".to_string(),
                label: "verify_token".to_string(),
                community: None,
                source_file: Some("src/auth/login.rs".to_string()),
            },
            GraphEvent::AddNode {
                id: "src/brand/new.rs::freshly_written".to_string(),
                label: "freshly_written".to_string(),
                community: None,
                source_file: Some("src/brand/new.rs".to_string()),
            },
            GraphEvent::RemoveNode {
                id: "a1".to_string(),
            },
        ],
    );
    assert!(
        outcome.topology_changed,
        "guard: the batch must have moved topology"
    );

    model.finalize_scc();
    let scc = model.scc.as_ref().expect("cache must be populated");
    let missing: Vec<_> = model
        .graph
        .node_indices()
        .filter(|&idx| scc.scc_of(idx).is_none())
        .collect();
    assert!(
        missing.is_empty(),
        "every node index must have an SCC entry, missing: {missing:?}"
    );
}
