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

/// Plan 08-04's fixture, shaped like the REAL Graphify export rather than
/// like toy ids -- confirmed field by field against `sample/graph.json` this
/// session:
///
/// - a node's `id` is an opaque slug (`src_auth_login`), never a path;
/// - a FILE node's `label` and `source_file` are both the repo-relative path
///   (`src/auth/login.rs`), which is exactly the shape `AddEdge.source`
///   arrives as;
/// - a SYMBOL node's `label` is the bare symbol (`verify`) and its
///   `source_file` is the file it lives in.
///
/// Deliberate asymmetries the tests below depend on:
/// - `src/auth/login.rs` HAS a file node; `src/db/pool.rs` has only a symbol
///   node, so a source endpoint naming it must be synthesized AND must
///   inherit `B` from its `source_file` sibling;
/// - `Handler` is the label of TWO nodes in different files, so a target
///   naming it is genuinely ambiguous and the tie-break is observable. The
///   lexicographically smaller id (`aa_web_handler`) is deliberately the
///   SECOND-inserted of the pair, so a resolver that just took the first
///   match would fail rather than pass by luck.
const EDGE_SHAPES_FIXTURE: &str = include_str!("fixtures/edge_shapes.json");

/// A graph whose source paths share NO leading component with
/// `EDGE_SHAPES_FIXTURE`'s. Exists solely so the same reference string can be
/// classified against two different loaded graphs and come out differently --
/// the proof that classification is derived, not listed.
const OTHER_ROOTS_FIXTURE: &str = r#"{
  "nodes": [
    {"id":"w1","norm_label":"lib/widget/render.rs","source_file":"lib/widget/render.rs","community":"W"},
    {"id":"w2","norm_label":"draw","source_file":"lib/widget/render.rs","community":"W"}
  ],
  "links": []
}"#;

fn fixture_model() -> Model {
    let mut model = seam_core::from_json(SOURCE_PATHS_FIXTURE)
        .expect("fixture must ingest cleanly")
        .model;
    model.finalize_scc();
    model
}

fn edge_shapes_model() -> Model {
    let mut model = seam_core::from_json(EDGE_SHAPES_FIXTURE)
        .expect("edge_shapes fixture must ingest cleanly")
        .model;
    model.finalize_scc();
    model
}

fn model_from(json: &str) -> Model {
    seam_core::from_json(json)
        .expect("fixture must ingest cleanly")
        .model
}

/// The real id of the node an index points at -- so every resolution
/// assertion below is written in terms of the graph's own identity rather
/// than a `NodeIndex` number that means nothing to a reader.
fn id_at(model: &Model, idx: petgraph::stable_graph::NodeIndex) -> String {
    model.graph[idx].id.clone()
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

// ---------------------------------------------------------------------
// 08-04 Task 1: the two EDGE identity shapes
//
// `AddEdge.source` is a bare repo-relative file path standing for the
// file's own node; `AddEdge.target` is the cross-module reference exactly
// as it appeared in the diff. Neither equals a Graphify-exported node id,
// and they do not equal each other's shape either -- confirmed by reading
// `apps/seam-client/src/detect.rs`'s `edge_endpoint`/`node_id` directly.
// ---------------------------------------------------------------------

#[test]
fn an_edge_source_resolves_onto_the_graphs_own_node_for_that_file() {
    let mut model = edge_shapes_model();
    let node_count_before = model.graph.node_count();

    let idx = seam_core::resolve_edge_source(&mut model, "src/auth/login.rs");

    assert_eq!(
        id_at(&model, idx),
        "src_auth_login",
        "a source path must resolve onto the graph's own FILE node for that path, \
         by the export's label==source_file==path convention"
    );
    assert_eq!(
        model.graph.node_count(),
        node_count_before,
        "resolving onto an existing node must never synthesize a second one"
    );
}

#[test]
fn an_edge_source_for_a_file_the_graph_has_no_node_for_is_synthesized() {
    let mut model = edge_shapes_model();
    let node_count_before = model.graph.node_count();

    // `src/db/pool.rs` has a SYMBOL node (`connect`, community B) but no
    // file node -- so the source must be synthesized, and must inherit B by
    // the ordinary `resolve_community` sibling rule rather than by any new
    // assignment rule invented for edges.
    let idx = seam_core::resolve_edge_source(&mut model, "src/db/pool.rs");

    assert_eq!(
        model.graph.node_count(),
        node_count_before + 1,
        "a source path with no node must synthesize exactly one node"
    );
    let node = model.graph[idx].clone();
    assert_eq!(node.id, "src/db/pool.rs", "the path is the synthesized id");
    assert_eq!(
        node.label, "src/db/pool.rs",
        "the path is the synthesized label"
    );
    assert_eq!(
        node.source_file.as_deref(),
        Some("src/db/pool.rs"),
        "the synthesized node must carry the path as its source_file"
    );
    assert_eq!(
        node.community, "B",
        "the synthesized node's community must come from the ordinary \
         source_file sibling-inheritance rule"
    );
    assert_eq!(
        model.index.get("src/db/pool.rs").copied(),
        Some(idx),
        "a synthesized node must be reachable by its own id afterwards"
    );

    // Inheritance is by EXACT source_file equality, never by directory
    // proximity: `src/db/migrate.rs` sits beside `src/db/pool.rs` and still
    // has no sibling, so it lands in the single unknown bucket.
    let sibling_free = seam_core::resolve_edge_source(&mut model, "src/db/migrate.rs");
    assert_eq!(
        model.graph[sibling_free].community,
        seam_core::UNKNOWN_COMMUNITY,
        "a directory neighbour is NOT a sibling -- with no exact source_file \
         match the synthesized node must land in the unknown bucket"
    );
}

#[test]
fn synthesizing_an_edge_source_never_disturbs_an_existing_nodes_community() {
    let mut model = edge_shapes_model();
    let before: Vec<(String, String)> = model
        .graph
        .node_weights()
        .map(|n| (n.id.clone(), n.community.clone()))
        .collect();

    seam_core::resolve_edge_source(&mut model, "src/db/pool.rs");
    seam_core::resolve_edge_source(&mut model, "src/brand/new.rs");

    for (id, community) in &before {
        let idx = model.index[id];
        assert_eq!(
            &model.graph[idx].community, community,
            "EVENT-05: synthesizing an edge source must not move node {id} \
             between communities"
        );
    }
}

#[test]
fn an_edge_target_resolves_by_its_trailing_symbol_against_node_labels() {
    let model = edge_shapes_model();

    let idx = seam_core::resolve_edge_target(&model, "auth::verify")
        .expect("a qualified reference whose trailing symbol is a node label must resolve");

    assert_eq!(
        id_at(&model, idx),
        "src_auth_login_verify",
        "the target must resolve onto the graph's own opaque id, never the wire string"
    );
    assert!(
        seam_core::resolve_edge_target(&model, "auth::no_such_symbol").is_none(),
        "a reference whose trailing symbol matches no label must resolve to nothing"
    );
}

#[test]
fn an_ambiguous_target_resolves_deterministically_to_the_smallest_node_id() {
    // Two nodes in different files share the label `Handler`. Rebuild the
    // model on every iteration so any hash-iteration luck gets 20 fresh
    // chances to produce a different answer -- the same discipline
    // `resolves_a_tied_community_deterministically_by_lexical_order`
    // established in this crate.
    for attempt in 0..20 {
        let model = edge_shapes_model();
        let idx = seam_core::resolve_edge_target(&model, "web::Handler")
            .expect("an ambiguous target must still resolve to SOMETHING");
        assert_eq!(
            id_at(&model, idx),
            "aa_web_handler",
            "attempt {attempt}: ambiguity must break to the lexicographically \
             smallest REAL node id, identically every time"
        );
    }
}

#[test]
fn an_exact_id_target_resolves_without_the_symbol_fallback() {
    let model = edge_shapes_model();

    let idx = seam_core::resolve_edge_target(&model, "src_db_pool_connect")
        .expect("an exact node id must resolve");

    assert_eq!(
        id_at(&model, idx),
        "src_db_pool_connect",
        "an exact id must win outright"
    );
    // Proof the exact-id path is what fired: no node in this graph carries
    // `src_db_pool_connect` as a LABEL, so the trailing-symbol fallback
    // could not have produced this answer.
    assert!(
        !model
            .graph
            .node_weights()
            .any(|n| n.label == "src_db_pool_connect"),
        "guard: no label equals this id, so only the exact-id branch can resolve it"
    );
}

// ---------------------------------------------------------------------
// 08-04 Task 1 / D-05a: internal vs external classification
//
// The client emits references with no allowlist and no denylist BY DESIGN
// (`detect.rs`'s own blind-spot inventory says so), so a target is just as
// likely to name a standard-library symbol as one of the user's own.
// ---------------------------------------------------------------------

#[test]
fn a_standard_library_reference_is_external() {
    let model = edge_shapes_model();
    let roots = seam_core::local_roots(&model);

    for target in [
        // A leading standard-library crate root.
        "std::collections::HashMap",
        "core::mem::swap",
        "alloc::vec::Vec",
        // A bare associated-function call on a well-known container type --
        // exactly what `collect_qualified_calls` emits for `HashMap::new()`.
        "HashMap::new",
        // A duration-style constructor, the other shape that scanner
        // produces constantly.
        "Duration::from_secs",
    ] {
        assert_eq!(
            seam_core::classify_target(target, &roots),
            seam_core::TargetClass::External,
            "{target} names something outside the user's project and can never \
             resolve -- it must classify External so it is dropped, not parked"
        );
    }
}

#[test]
fn a_reference_into_the_users_own_project_is_internal() {
    let model = edge_shapes_model();
    let roots = seam_core::local_roots(&model);

    for target in [
        // `auth` is a directory component of src/auth/login.rs.
        "auth::verify",
        // `db` is a directory component of src/db/pool.rs.
        "db::pool::acquire",
        // `login` is a FILE stem, with its extension stripped.
        "login::verify",
        // A node's own label, verbatim and unqualified.
        "Handler",
    ] {
        assert_eq!(
            seam_core::classify_target(target, &roots),
            seam_core::TargetClass::Internal,
            "{target}'s leading segment is vouched for by the loaded graph's own \
             source paths -- it must classify Internal so it can be parked"
        );
    }
}

#[test]
fn classification_is_derived_from_the_loaded_graph_not_a_fixed_list() {
    let here = seam_core::local_roots(&edge_shapes_model());
    let elsewhere = seam_core::local_roots(&model_from(OTHER_ROOTS_FIXTURE));

    // The SAME string, classified against two different loaded graphs, must
    // come out differently -- in BOTH directions. A hand-maintained denylist
    // wearing a disguise could not do this.
    assert_eq!(
        seam_core::classify_target("db::pool::acquire", &here),
        seam_core::TargetClass::Internal
    );
    assert_eq!(
        seam_core::classify_target("db::pool::acquire", &elsewhere),
        seam_core::TargetClass::External,
        "a graph with no `db` anywhere in its source paths cannot vouch for it"
    );
    assert_eq!(
        seam_core::classify_target("widget::draw", &elsewhere),
        seam_core::TargetClass::Internal
    );
    assert_eq!(
        seam_core::classify_target("widget::draw", &here),
        seam_core::TargetClass::External,
        "the reverse direction too, so the test cannot pass by one graph simply \
         being more permissive than the other"
    );

    // The closed language-root set is NOT derived and must survive any
    // graph: a project with a directory literally named `std` still cannot
    // claim `std::` references as its own.
    assert_eq!(
        seam_core::classify_target("std::fmt::Debug", &here),
        seam_core::TargetClass::External
    );
}

#[test]
fn an_empty_model_classifies_everything_as_external() {
    let empty = Model::default();
    let roots = seam_core::local_roots(&empty);

    assert!(
        roots.is_empty(),
        "a graph with no nodes can vouch for nothing"
    );
    for target in ["auth::verify", "widget::draw", "Handler", "anything_at_all"] {
        assert_eq!(
            seam_core::classify_target(target, &roots),
            seam_core::TargetClass::External,
            "with nothing loaded, {target} must default to External so it can \
             never accumulate as a pending edge forever"
        );
    }
}
