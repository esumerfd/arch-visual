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
///
/// The `contains` link from the file node to its symbol is faithful to the
/// real export AND is filtered out by `ingest`'s D-01/D-02/D-03 relation
/// allow-list, so this fixture ingests to **2** edges, not 3. Left that way
/// on purpose: a fixture that quietly swapped the relation to smuggle the
/// edge through would misrepresent what a real loaded graph looks like.
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

// ---------------------------------------------------------------------
// 08-04 Task 2: applying edges, parking what might still resolve,
// dropping what never will
// ---------------------------------------------------------------------

/// An `AddEdge` in the exact shape `seam-client` emits: a bare
/// repo-relative path for the source, a reference as written for the target.
fn add_edge(source: &str, target: &str) -> GraphEvent {
    GraphEvent::AddEdge {
        source: source.to_string(),
        target: target.to_string(),
    }
}

fn remove_edge(source: &str, target: &str) -> GraphEvent {
    GraphEvent::RemoveEdge {
        source: source.to_string(),
        target: target.to_string(),
    }
}

fn edge_exists(model: &Model, from_id: &str, to_id: &str) -> bool {
    match (model.index.get(from_id), model.index.get(to_id)) {
        (Some(&from), Some(&to)) => model.graph.find_edge(from, to).is_some(),
        _ => false,
    }
}

#[test]
fn an_edge_between_two_known_nodes_is_applied() {
    let mut model = edge_shapes_model();
    let edges_before = model.graph.edge_count();
    assert!(
        !edge_exists(&model, "src_auth_login", "src_db_pool_connect"),
        "guard: the fixture must not already contain the edge under test"
    );

    let outcome =
        seam_core::apply_batch(&mut model, &[add_edge("src/auth/login.rs", "db::connect")]);

    assert_eq!(
        model.graph.edge_count(),
        edges_before + 1,
        "an edge between two resolvable endpoints must land, exactly once"
    );
    assert!(
        edge_exists(&model, "src_auth_login", "src_db_pool_connect"),
        "the edge must connect the two RESOLVED nodes, looked up by their real ids"
    );
    assert!(outcome.topology_changed, "an added edge changes topology");
    assert_eq!(
        outcome.applied,
        vec![GraphEvent::AddEdge {
            source: "src_auth_login".to_string(),
            target: "src_db_pool_connect".to_string(),
        }],
        "the applied event must carry the two REAL node ids, never the wire strings"
    );
    assert_eq!(outcome.parked_edges, 0);
    assert_eq!(outcome.dropped_external_edges, 0);
}

#[test]
fn applying_the_same_edge_twice_does_not_duplicate_it() {
    // Real editing sessions re-report the same reference constantly. Parallel
    // duplicates would inflate every crossing count and silently reclassify
    // seams (T-08-04-04).
    let mut model = edge_shapes_model();
    let edges_before = model.graph.edge_count();

    seam_core::apply_batch(&mut model, &[add_edge("src/auth/login.rs", "db::connect")]);
    let outcome =
        seam_core::apply_batch(&mut model, &[add_edge("src/auth/login.rs", "db::connect")]);

    assert_eq!(
        model.graph.edge_count(),
        edges_before + 1,
        "re-reporting an edge must not add a parallel duplicate"
    );
    assert!(
        outcome.applied.is_empty(),
        "a no-op re-report must not be recorded as applied -- the history would \
         otherwise fill with events that changed nothing"
    );
    assert!(!outcome.topology_changed);
}

#[test]
fn an_edge_to_an_external_target_is_dropped_and_never_parked() {
    let mut model = edge_shapes_model();
    let nodes_before = model.graph.node_count();
    let edges_before = model.graph.edge_count();

    // The source path deliberately has NO file node, so a resolver that
    // touched the source before deciding the target's fate would leave a
    // synthesized node behind -- which the node-count assertion catches.
    let outcome = seam_core::apply_batch(
        &mut model,
        &[add_edge("src/db/pool.rs", "std::collections::HashMap")],
    );

    assert_eq!(
        model.graph.node_count(),
        nodes_before,
        "a dropped edge must leave NO node behind -- not the target, and not a \
         synthesized source either (T-08-04-01)"
    );
    assert_eq!(model.graph.edge_count(), edges_before);
    assert!(
        model.pending_edges.is_empty(),
        "an external target can never resolve, so parking it would only consume \
         a slot an internal edge needs (D-05a)"
    );
    assert_eq!(outcome.dropped_external_edges, 1);
    assert_eq!(outcome.parked_edges, 0);
    assert!(outcome.applied.is_empty());
    assert!(!outcome.topology_changed);
}

#[test]
fn an_edge_to_an_internal_but_unknown_target_is_parked() {
    let mut model = edge_shapes_model();
    let nodes_before = model.graph.node_count();
    let edges_before = model.graph.edge_count();

    let outcome = seam_core::apply_batch(&mut model, &[add_edge("src/db/pool.rs", "auth::ghost")]);

    assert_eq!(
        model.graph.node_count(),
        nodes_before,
        "parking must add nothing to the graph -- resolution order (target \
         first) is what prevents a synthesized source appearing here"
    );
    assert_eq!(model.graph.edge_count(), edges_before);
    assert_eq!(
        model.pending_edges.len(),
        1,
        "an internal target might still arrive, so its edge waits (D-05)"
    );
    assert_eq!(outcome.parked_edges, 1);
    assert_eq!(outcome.dropped_external_edges, 0);
    assert!(outcome.applied.is_empty());
    assert!(!outcome.topology_changed);

    // The entry holds the RAW endpoint strings, identified by cancelling
    // exactly them.
    assert!(
        model.pending_edges.cancel("src/db/pool.rs", "auth::ghost"),
        "the parked entry must be keyed by the raw wire endpoints"
    );
    assert!(model.pending_edges.is_empty());
}

#[test]
fn parking_the_same_pending_edge_twice_keeps_one_entry() {
    let mut model = edge_shapes_model();

    seam_core::apply_batch(&mut model, &[add_edge("src/db/pool.rs", "auth::ghost")]);
    seam_core::apply_batch(&mut model, &[add_edge("src/db/pool.rs", "auth::ghost")]);

    assert_eq!(
        model.pending_edges.len(),
        1,
        "the same unresolved reference re-reported must not consume a second slot"
    );
}

#[test]
fn the_pending_store_evicts_oldest_first_at_the_shared_bound() {
    let cap = seam_core::LIVE_BUFFER_CAPACITY;
    let overflow = 10;
    let mut model = edge_shapes_model();

    let script: Vec<GraphEvent> = (0..cap + overflow)
        .map(|n| add_edge("src/db/pool.rs", &format!("auth::ghost{n}")))
        .collect();
    let outcome = seam_core::apply_batch(&mut model, &script);

    assert_eq!(outcome.parked_edges, cap + overflow, "every one must park");
    assert_eq!(
        model.pending_edges.len(),
        cap,
        "the store must be bounded by the SAME cap the history uses (D-05a)"
    );
    assert_eq!(
        model.pending_edges.evicted_count(),
        overflow as u64,
        "the eviction count must account for the difference exactly"
    );

    // Identified by CONTENT, not by position: entry `overflow - 1` was the
    // last one pushed out of the front, and entry `overflow` is the oldest
    // survivor.
    assert!(
        !model
            .pending_edges
            .cancel("src/db/pool.rs", &format!("auth::ghost{}", overflow - 1)),
        "the newest EVICTED entry must be gone"
    );
    assert!(
        model
            .pending_edges
            .cancel("src/db/pool.rs", &format!("auth::ghost{overflow}")),
        "the oldest SURVIVOR must be the entry immediately after the evicted run"
    );
}

#[test]
fn removing_an_edge_that_is_only_parked_cancels_the_parked_entry() {
    // T-08-04-06: without cancellation the RemoveEdge is a no-op and the
    // edge materializes later, AFTER the user deleted it.
    let mut model = edge_shapes_model();
    seam_core::apply_batch(&mut model, &[add_edge("src/db/pool.rs", "auth::ghost")]);
    assert_eq!(model.pending_edges.len(), 1, "guard: it must be parked");

    let outcome =
        seam_core::apply_batch(&mut model, &[remove_edge("src/db/pool.rs", "auth::ghost")]);

    assert!(
        model.pending_edges.is_empty(),
        "a RemoveEdge for a still-parked pair must cancel it, or the edge \
         appears later after the user already deleted it"
    );
    assert!(
        outcome.applied.is_empty(),
        "cancelling a parked entry changed no edge, so nothing is recorded"
    );
    assert!(!outcome.topology_changed);
}

#[test]
fn removing_an_applied_edge_removes_exactly_that_edge() {
    let mut model = edge_shapes_model();
    seam_core::apply_batch(&mut model, &[add_edge("src/auth/login.rs", "db::connect")]);
    let edges_before = model.graph.edge_count();
    let nodes_before = model.graph.node_count();

    let outcome = seam_core::apply_batch(
        &mut model,
        &[remove_edge("src/auth/login.rs", "db::connect")],
    );

    assert_eq!(
        model.graph.edge_count(),
        edges_before - 1,
        "exactly one edge must go"
    );
    assert_eq!(
        model.graph.node_count(),
        nodes_before,
        "removing an edge must never synthesize a node"
    );
    assert!(!edge_exists(
        &model,
        "src_auth_login",
        "src_db_pool_connect"
    ));
    assert!(
        edge_exists(&model, "zz_routes_handler", "src_auth_login_verify"),
        "the fixture's other edges must survive untouched"
    );
    assert!(
        edge_exists(&model, "src_auth_login_verify", "src_db_pool_connect"),
        "an unrelated edge sharing an endpoint must survive"
    );
    assert!(outcome.topology_changed);
    assert_eq!(
        outcome.applied,
        vec![GraphEvent::RemoveEdge {
            source: "src_auth_login".to_string(),
            target: "src_db_pool_connect".to_string(),
        }],
        "the recorded removal must carry the real node ids too"
    );
}

#[test]
fn no_edge_event_changes_any_nodes_community() {
    // EVENT-05's guard for this task, over a scripted mix that exercises
    // every edge outcome: applied, duplicate, parked, dropped, removed.
    let mut model = edge_shapes_model();
    let before: Vec<(String, String)> = model
        .graph
        .node_weights()
        .map(|n| (n.id.clone(), n.community.clone()))
        .collect();

    let outcome = seam_core::apply_batch(
        &mut model,
        &[
            add_edge("src/auth/login.rs", "db::connect"),
            add_edge("src/auth/login.rs", "db::connect"),
            add_edge("src/db/pool.rs", "std::sync::Arc"),
            add_edge("src/db/pool.rs", "auth::ghost"),
            add_edge("src/api/routes.rs", "web::Handler"),
            remove_edge("src/auth/login.rs", "db::connect"),
        ],
    );
    assert!(
        outcome.topology_changed,
        "guard: the script must actually have moved the graph"
    );

    for (id, community) in &before {
        let idx = model.index[id];
        assert_eq!(
            &model.graph[idx].community, community,
            "EVENT-05: no edge event may move node {id} between communities"
        );
    }
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

// ---------------------------------------------------------------------
// 08-05 Task 1: the bounded fixed-point promotion sweep (D-04)
//
// The user's mandate, verbatim from 08-CONTEXT.md: "All events have the
// power to create unknown or ultimately known communities. Evolve the graph
// as closely to the data in the event as possible." A design that assigned a
// community once at insertion and never revisited it would not satisfy that,
// and 08-01 shipped exactly that limited behaviour so this plan could
// complete it honestly.
//
// The counterweight, equally locked, is EVENT-05: a community that came from
// the loaded graph.json never moves. Both halves are requirements, and the
// tests below assert both.
// ---------------------------------------------------------------------

/// An `AddNode` in the shape `seam-client` emits.
fn add_node(
    id: &str,
    label: &str,
    community: Option<&str>,
    source_file: Option<&str>,
) -> GraphEvent {
    GraphEvent::AddNode {
        id: id.to_string(),
        label: label.to_string(),
        community: community.map(str::to_string),
        source_file: source_file.map(str::to_string),
    }
}

/// Insert a node that lands in the unknown bucket, and return its index.
///
/// Deliberately routed through the ORDINARY `apply_add_node` path with no
/// wire community rather than poking `Node.community` directly: a test that
/// hand-wrote the sentinel could pass against a sweep that never agrees with
/// how nodes actually get there. The assertion below is the proof it worked.
fn park_unknown(
    model: &mut Model,
    id: &str,
    source_file: Option<&str>,
) -> petgraph::stable_graph::NodeIndex {
    seam_core::apply_add_node(model, id, id, None, source_file);
    assert_eq!(
        community_of(model, id),
        seam_core::UNKNOWN_COMMUNITY,
        "guard: {id} must actually be parked in the unknown bucket, or this \
         test proves nothing about promotion"
    );
    model.index[id]
}

/// Every community currently in the model, so a minted one can be shown to be
/// genuinely NEW rather than a collision with something already there.
fn communities(model: &Model) -> std::collections::BTreeSet<String> {
    model
        .graph
        .node_weights()
        .map(|n| n.community.clone())
        .collect()
}

/// Every (id, community) pair for nodes NOT in the unknown bucket -- the set
/// EVENT-05 says the sweep may never touch.
fn resolved_snapshot(model: &Model) -> Vec<(String, String)> {
    model
        .graph
        .node_weights()
        .filter(|n| n.community != seam_core::UNKNOWN_COMMUNITY)
        .map(|n| (n.id.clone(), n.community.clone()))
        .collect()
}

#[test]
fn an_unknown_node_inherits_its_neighbours_community_across_an_edge() {
    // Direction 1: the unknown node CALLS OUT into a resolved community.
    let mut model = edge_shapes_model();
    let caller = park_unknown(&mut model, "ghost_caller", None);
    let verify = model.index["src_auth_login_verify"];
    model.graph.add_edge(caller, verify, ());

    let outcome = seam_core::promote_unknown_communities(&mut model);

    assert_eq!(
        community_of(&model, "ghost_caller"),
        "A",
        "an edge to a node whose community is known is evidence enough to promote"
    );
    assert_eq!(outcome.promoted, 1);
    assert!(outcome.passes >= 1, "the sweep must have run at least once");

    // Direction 2: a resolved community CALLS IN to the unknown node. A call
    // into a community is as much evidence as a call out of it, and a sweep
    // that only walked outgoing edges would miss half of every real graph.
    let mut model = edge_shapes_model();
    let callee = park_unknown(&mut model, "ghost_callee", None);
    let connect = model.index["src_db_pool_connect"];
    model.graph.add_edge(connect, callee, ());

    seam_core::promote_unknown_communities(&mut model);

    assert_eq!(
        community_of(&model, "ghost_callee"),
        "B",
        "an INCOMING edge from a resolved community must promote too"
    );
}

#[test]
fn an_unknown_node_inherits_from_a_later_arriving_source_file_sibling() {
    let mut model = edge_shapes_model();

    // Arrives first, with nothing anywhere to inherit from.
    seam_core::apply_batch(
        &mut model,
        &[add_node("first", "first", None, Some("src/fresh/mod.rs"))],
    );
    assert_eq!(
        community_of(&model, "first"),
        seam_core::UNKNOWN_COMMUNITY,
        "guard: with no sibling and no neighbour, the first arrival must park"
    );

    // Arrives later, carrying an explicit community. That is the evidence the
    // sweep has been waiting for.
    seam_core::apply_batch(
        &mut model,
        &[add_node(
            "second",
            "second",
            Some("Q"),
            Some("src/fresh/mod.rs"),
        )],
    );

    assert_eq!(community_of(&model, "second"), "Q", "guard: the wire value");
    assert_eq!(
        community_of(&model, "first"),
        "Q",
        "a sibling in the same source file, arriving later, must promote the \
         node already parked -- this is the half 08-01 deliberately left undone"
    );
}

#[test]
fn siblings_that_cannot_inherit_anything_mint_a_new_community_together() {
    let mut model = edge_shapes_model();
    let before = communities(&model);

    let outcome = seam_core::apply_batch(
        &mut model,
        &[
            add_node("orphan_one", "orphan_one", None, Some("src/fresh/mod.rs")),
            add_node("orphan_two", "orphan_two", None, Some("src/fresh/mod.rs")),
        ],
    );

    let minted = community_of(&model, "orphan_one");
    assert_eq!(
        community_of(&model, "orphan_two"),
        minted,
        "siblings nothing can place must form ONE community together, not two"
    );
    assert_ne!(
        minted,
        seam_core::UNKNOWN_COMMUNITY,
        "a formed community is not the unresolved bucket"
    );
    assert!(
        !minted.contains(seam_core::UNKNOWN_COMMUNITY),
        "the minted identifier must be visibly distinct from the sentinel, so \
         'genuinely unplaced' and 'freshly formed' can never be confused by a \
         reader, a log, or Phase 9's replay -- got {minted}"
    );
    assert!(
        !before.contains(&minted),
        "ROADMAP SC-3 as amended: the live graph may GAIN a community, but a \
         minted one must never collide with one the original export had"
    );
    assert_eq!(
        outcome.promotion.minted_communities,
        vec![minted.clone()],
        "the outcome must report exactly the community it formed"
    );
    assert_eq!(outcome.promotion.promoted, 2, "both siblings were promoted");

    let label = model.community_label(&minted);
    assert_ne!(
        label, minted,
        "community_names must have gained a readable entry, or the seam list \
         renders a raw synthetic identifier at the user"
    );
    assert!(
        label.contains("src/fresh/mod.rs"),
        "the readable label must name the source file the group formed around, \
         got {label}"
    );
}

#[test]
fn a_lone_unknown_node_with_no_siblings_stays_unknown() {
    // D-04's own step-d, and the test that stops the sweep from degenerating
    // into "invent a community per node" -- which would pass every other
    // promotion test in this file.
    let mut model = edge_shapes_model();

    let outcome = seam_core::apply_batch(
        &mut model,
        &[
            add_node("lonely", "lonely", None, Some("src/alone/only.rs")),
            add_node("pathless", "pathless", None, None),
        ],
    );

    assert_eq!(
        community_of(&model, "lonely"),
        seam_core::UNKNOWN_COMMUNITY,
        "no evidence yet is a legitimate state, not a problem to paper over"
    );
    assert_eq!(
        community_of(&model, "pathless"),
        seam_core::UNKNOWN_COMMUNITY,
        "a node with NO source_file has no siblings by definition and must \
         never be grouped with other pathless nodes"
    );
    assert_eq!(outcome.promotion.promoted, 0);
    assert!(
        outcome.promotion.minted_communities.is_empty(),
        "nothing may be minted for a group of one"
    );
}

#[test]
fn a_promotion_chain_resolves_inside_one_sweep() {
    // The case a single pass would miss: the minting in one pass is exactly
    // the neighbour evidence a different node needs in the next.
    let mut model = edge_shapes_model();
    let pair_a = park_unknown(&mut model, "pair_a", Some("src/fresh/mod.rs"));
    park_unknown(&mut model, "pair_b", Some("src/fresh/mod.rs"));
    let downstream = park_unknown(&mut model, "downstream", Some("src/downstream/solo.rs"));
    // `downstream` is alone in its own file, so it can NEVER mint; its only
    // possible route out of the bucket is the community pair_a/pair_b form.
    model.graph.add_edge(downstream, pair_a, ());

    let outcome = seam_core::promote_unknown_communities(&mut model);

    let minted = community_of(&model, "pair_a");
    assert_ne!(minted, seam_core::UNKNOWN_COMMUNITY);
    assert_eq!(community_of(&model, "pair_b"), minted);
    assert_eq!(
        community_of(&model, "downstream"),
        minted,
        "a chain must resolve inside ONE call, not leave the far end waiting a \
         frame for the next batch"
    );
    assert!(
        outcome.passes >= 2,
        "this shape is unresolvable in a single pass, so a one-pass sweep must \
         not be able to pass this test; got {} passes",
        outcome.passes
    );
    assert!(outcome.passes <= seam_core::MAX_PROMOTION_PASSES);
    assert_eq!(outcome.promoted, 3);
}

#[test]
fn the_sweep_terminates_on_a_pathological_input() {
    // Graceful degradation is the contract, so it is asserted rather than
    // hoped for (T-08-05-02). A chain longer than the cap can walk must leave
    // its tail parked -- never spin the frame, never corrupt what it did not
    // reach.
    let mut model = edge_shapes_model();
    let seed = park_unknown(&mut model, "seed_a", Some("src/seed/mod.rs"));
    park_unknown(&mut model, "seed_b", Some("src/seed/mod.rs"));

    let chain_len = seam_core::MAX_PROMOTION_PASSES * 3;
    let mut prev = seed;
    let mut chain: Vec<String> = Vec::with_capacity(chain_len);
    for n in 0..chain_len {
        let id = format!("link{n}");
        let idx = park_unknown(&mut model, &id, Some(&format!("src/chain/link{n}.rs")));
        model.graph.add_edge(idx, prev, ());
        prev = idx;
        chain.push(id);
    }
    let nodes_before = model.graph.node_count();

    let outcome = seam_core::promote_unknown_communities(&mut model);

    assert!(
        outcome.passes <= seam_core::MAX_PROMOTION_PASSES,
        "the sweep must stop at its cap, got {} passes",
        outcome.passes
    );
    let still_unknown: Vec<&String> = chain
        .iter()
        .filter(|id| community_of(&model, id) == seam_core::UNKNOWN_COMMUNITY)
        .collect();
    assert!(
        !still_unknown.is_empty(),
        "guard: the chain must be longer than the cap can walk, or this test \
         measures nothing"
    );
    for id in &still_unknown {
        assert_eq!(
            community_of(&model, id),
            seam_core::UNKNOWN_COMMUNITY,
            "the unresolved remainder must sit in the bucket, not in some \
             half-written state"
        );
    }
    assert_eq!(
        model.graph.node_count(),
        nodes_before,
        "a capped sweep must not have added or lost a node"
    );
}

#[test]
fn promotion_never_touches_a_node_that_already_had_a_community() {
    // EVENT-05 at the level of the one function that deliberately WRITES
    // Node.community. Every other mutation path in this phase has no write
    // access to the field at all; this one does, so the guarantee has to be
    // asserted rather than argued (T-08-05-01).
    let mut model = edge_shapes_model();
    let before = resolved_snapshot(&model);
    assert!(
        !before.is_empty(),
        "guard: the fixture must have communities"
    );

    // A sweep that genuinely does work: an inheritance, a minting, and a node
    // that stays put.
    let caller = park_unknown(&mut model, "ghost_caller", None);
    let verify = model.index["src_auth_login_verify"];
    model.graph.add_edge(caller, verify, ());
    park_unknown(&mut model, "orphan_one", Some("src/fresh/mod.rs"));
    park_unknown(&mut model, "orphan_two", Some("src/fresh/mod.rs"));
    park_unknown(&mut model, "lonely", Some("src/alone/only.rs"));

    let outcome = seam_core::promote_unknown_communities(&mut model);
    assert!(
        outcome.promoted >= 3,
        "guard: the sweep must actually have promoted something, or byte-identity \
         below is trivially true"
    );

    for (id, community) in &before {
        assert_eq!(
            &community_of(&model, id),
            community,
            "EVENT-05: the sweep must not move originally-loaded node {id}"
        );
    }
}

#[test]
fn an_idle_batch_runs_no_sweep() {
    let mut model = edge_shapes_model();
    assert!(
        model
            .graph
            .node_weights()
            .all(|n| n.community != seam_core::UNKNOWN_COMMUNITY),
        "guard: the fixture must have nothing parked"
    );

    let outcome =
        seam_core::apply_batch(&mut model, &[add_edge("src/auth/login.rs", "db::connect")]);

    assert!(outcome.topology_changed, "guard: the batch did real work");
    assert_eq!(
        outcome.promotion,
        seam_core::PromotionOutcome::default(),
        "the sweep runs every batch and must cost nothing when there is nothing \
         parked -- zero passes, not one that finds nothing"
    );
    assert_eq!(outcome.promotion.passes, 0);
}

#[test]
fn a_node_with_several_candidate_communities_resolves_deterministically() {
    // Two neighbours in different communities. Rebuild the model on every
    // iteration so any hash-iteration luck gets 20 fresh chances to produce a
    // different answer -- the discipline
    // `resolves_a_tied_community_deterministically_by_lexical_order` set in
    // this crate and `an_ambiguous_target_resolves_deterministically_to_the_
    // smallest_node_id` reused in 08-04.
    for attempt in 0..20 {
        let mut model = edge_shapes_model();
        let torn = park_unknown(&mut model, "torn", None);
        let in_b = model.index["src_db_pool_connect"];
        let in_c = model.index["zz_routes_handler"];
        let in_a = model.index["src_auth_login_verify"];
        // Deliberately wired B first and A last, so a resolver that took the
        // first candidate it saw would fail rather than pass by luck.
        model.graph.add_edge(torn, in_b, ());
        model.graph.add_edge(in_c, torn, ());
        model.graph.add_edge(torn, in_a, ());

        seam_core::promote_unknown_communities(&mut model);

        assert_eq!(
            community_of(&model, "torn"),
            "A",
            "attempt {attempt}: several candidates must break to the \
             lexicographically smallest community, identically every time"
        );
    }
}
