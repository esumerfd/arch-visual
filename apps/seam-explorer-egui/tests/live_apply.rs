//! Plan 08-01: the live-apply tracer. Every test here drives a REAL
//! `AF_UNIX`/`SOCK_DGRAM` socket -> the real background receive thread -> the
//! real process-global drain -> `history::drain_and_apply` -> `app.model`,
//! with no mock socket and no hand-built event vector short-circuiting the
//! pipeline. That is the whole point: ROADMAP SC-1 is a claim about the live
//! path, and only the live path can prove it.
//!
//! `SERVE_TEST_LOCK` serializes every test that touches
//! `event_stream::serve`/`drain`/`received_count` -- `cargo test`'s default
//! parallelism would otherwise race on the process-global receiver. Same
//! recipe as `tests/event_stream.rs`; deliberately reused rather than
//! reinvented.
//!
//! Plans 08-03 and 08-04 extend this same file (history buffer, edge
//! application), so the helpers below are written to be shared, not inlined.

use std::os::unix::ffi::OsStrExt;
use std::os::unix::net::UnixDatagram;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use egui_kittest::Harness;
use seam_core::GraphEvent;
use seam_explorer_egui::app::{FocusState, SeamExplorerApp};
use seam_explorer_egui::trace::TraceResult;
use seam_explorer_egui::{event_stream, graph_view, history, panels};

/// The fixture whose nodes carry `source_file`, which is what the
/// sibling-inheritance half of `resolve_community` needs. 6 nodes across
/// three communities (A: a1/a2, B: b1/b2, C: c1/c2), 7 surviving edges.
const SOURCE_PATHS_FIXTURE: &str = include_str!("../../seam-core/tests/fixtures/source_paths.json");

/// A DIFFERENT graph, used only by the "loading a new graph starts a new
/// timeline" test. Its identity as a different file is the whole point.
const CLEAN_FIXTURE: &str = include_str!("../../seam-core/tests/fixtures/clean.json");

/// Serializes every test that touches `event_stream`'s process-global.
static SERVE_TEST_LOCK: Mutex<()> = Mutex::new(());

fn temp_socket_path(unique: &str) -> PathBuf {
    // Kept short deliberately: this path plus "/seam.sock" must stay under
    // the 104-byte sun_path ceiling on top of whatever length $TMPDIR is.
    std::env::temp_dir()
        .join(format!("es-live-{}-{}", std::process::id(), unique))
        .join("seam.sock")
}

fn wait_until(deadline: Duration, mut condition: impl FnMut() -> bool) -> bool {
    let start = Instant::now();
    while start.elapsed() < deadline {
        if condition() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    condition()
}

/// Binds a real socket under a per-test-unique temp directory and installs
/// it as the process-global receiver. Asserts the path length against
/// `seam_core::MAX_SUN_PATH_BYTES` *with the actual length in the message*
/// before binding, so a $TMPDIR-length problem reads as a path-length
/// failure rather than an inscrutable bind error.
fn serve_at(unique: &str) -> PathBuf {
    let path = temp_socket_path(unique);
    let len = path.as_os_str().as_bytes().len();
    assert!(
        len <= seam_core::MAX_SUN_PATH_BYTES,
        "socket path is {len} bytes, over the {} byte sun_path ceiling: {path:?}",
        seam_core::MAX_SUN_PATH_BYTES
    );
    let _ = std::fs::remove_file(&path);
    let socket = event_stream::bind_at(&path).expect("bind_at must succeed for a fresh path");
    event_stream::serve(socket, egui::Context::default());
    path
}

/// Sends real datagrams from a separate unbound socket and waits (bounded)
/// for the receive thread to have delivered all of them before returning.
fn send_and_wait(path: &Path, events: &[GraphEvent]) {
    let baseline = event_stream::received_count();
    for event in events {
        let bytes = seam_core::to_datagram(event);
        let sender = UnixDatagram::unbound().expect("unbound socket must be constructible");
        sender
            .send_to(&bytes, path)
            .expect("send_to a bound socket must succeed");
    }
    let target = baseline + events.len() as u64;
    assert!(
        wait_until(Duration::from_secs(2), || {
            event_stream::received_count() >= target
        }),
        "expected received_count to reach {target}, stalled at {}",
        event_stream::received_count()
    );
}

/// Matches `tests/canvas.rs::build_test_app`, but on the `source_file`-
/// carrying fixture this plan's resolution logic needs.
fn build_test_app() -> SeamExplorerApp {
    let outcome = seam_explorer_egui::load::read_and_ingest(SOURCE_PATHS_FIXTURE)
        .expect("fixture must ingest cleanly");
    SeamExplorerApp {
        model: Some(outcome.model),
        seams: outcome.seams,
        ..Default::default()
    }
}

/// Reconstructs the community a new node under `source_file` should inherit
/// by reading the FIXTURE directly, never the applied model -- so the
/// inheritance assertion cannot pass by aliasing the value it is checking.
fn expected_inherited_community(source_file: &str) -> String {
    let doc: serde_json::Value =
        serde_json::from_str(SOURCE_PATHS_FIXTURE).expect("fixture must be valid JSON");
    doc["nodes"]
        .as_array()
        .expect("fixture must have a nodes array")
        .iter()
        .filter(|n| n["source_file"].as_str() == Some(source_file))
        .filter_map(|n| n["community"].as_str().map(str::to_string))
        .min()
        .unwrap_or_else(|| panic!("fixture has no node with source_file {source_file}"))
}

fn add_node(id: &str, label: &str, source_file: Option<&str>) -> GraphEvent {
    GraphEvent::AddNode {
        id: id.to_string(),
        label: label.to_string(),
        community: None,
        source_file: source_file.map(str::to_string),
    }
}

fn rendered_ids(app: &SeamExplorerApp) -> Vec<String> {
    let model = app.model.as_ref().expect("model must be loaded");
    graph_view::build_graph(model, None)
        .nodes_iter()
        .map(|(_, n)| n.payload().id.clone())
        .collect()
}

// ---------------------------------------------------------------------
// Task 1: the tracer -- a real datagram becomes a rendered node
// ---------------------------------------------------------------------

/// The tracer's own proof: one real `add_node` datagram, drained through the
/// shipped global drain, is present in `app.model` AND in what
/// `graph_view::build_graph` renders -- the binding discarded since Phase 6
/// now does work.
#[test]
fn a_live_add_node_reaches_the_model_and_the_rendered_graph() {
    let _guard = SERVE_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let path = serve_at("add-node");
    let mut app = build_test_app();

    const NEW_ID: &str = "src/auth/login.rs::verify_token";
    let expected = expected_inherited_community("src/auth/login.rs");
    assert_eq!(
        expected, "A",
        "fixture guard: src/auth/login.rs must belong to community A"
    );
    assert!(
        !rendered_ids(&app).contains(&NEW_ID.to_string()),
        "guard: the new node must not already exist before the event"
    );

    send_and_wait(
        &path,
        &[add_node(NEW_ID, "verify_token", Some("src/auth/login.rs"))],
    );
    let summary = history::drain_and_apply(&mut app);
    assert_eq!(summary.applied_count, 1, "exactly one event must apply");

    let model = app.model.as_ref().expect("model must be loaded");
    let idx = *model
        .index
        .get(NEW_ID)
        .expect("the new node must be registered in model.index");
    assert_eq!(
        model.graph[idx].community, expected,
        "an absent wire community must be inherited from a source_file sibling"
    );
    assert!(
        rendered_ids(&app).contains(&NEW_ID.to_string()),
        "the new node must be rendered by build_graph"
    );
}

/// D-04/D-04a: with no sibling to inherit from, the node lands in the single
/// reserved sentinel bucket -- concrete by construction, never absent.
#[test]
fn a_live_add_node_with_no_resolvable_sibling_lands_in_the_unknown_bucket() {
    let _guard = SERVE_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let path = serve_at("unknown-bucket");
    let mut app = build_test_app();

    const NEW_ID: &str = "src/brand/new.rs::freshly_written";
    send_and_wait(
        &path,
        &[add_node(
            NEW_ID,
            "freshly_written",
            Some("src/brand/new.rs"),
        )],
    );
    history::drain_and_apply(&mut app);

    let model = app.model.as_ref().expect("model must be loaded");
    let idx = *model
        .index
        .get(NEW_ID)
        .expect("the new node must be registered in model.index");
    assert_eq!(
        model.graph[idx].community,
        seam_core::UNKNOWN_COMMUNITY,
        "an unresolvable community must become the reserved sentinel"
    );
}

/// D-03: removal is immediate and total -- the node and every edge incident
/// to it disappear from the model and from the rendered graph.
#[test]
fn a_live_remove_node_takes_its_edges_with_it() {
    let _guard = SERVE_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let path = serve_at("remove-node");
    let mut app = build_test_app();

    let (edges_before, incident) = {
        let model = app.model.as_ref().expect("model must be loaded");
        let idx = *model.index.get("a1").expect("fixture node a1 must exist");
        let out = model
            .graph
            .edges_directed(idx, petgraph::Direction::Outgoing)
            .count();
        let inc = model
            .graph
            .edges_directed(idx, petgraph::Direction::Incoming)
            .count();
        (model.graph.edge_count(), out + inc)
    };
    assert!(
        incident > 0,
        "guard: a1 must have at least one incident edge"
    );

    send_and_wait(
        &path,
        &[GraphEvent::RemoveNode {
            id: "a1".to_string(),
        }],
    );
    history::drain_and_apply(&mut app);

    let model = app.model.as_ref().expect("model must be loaded");
    assert!(
        !model.index.contains_key("a1"),
        "the removed node must be gone from model.index"
    );
    assert!(
        !model.graph.node_weights().any(|n| n.id == "a1"),
        "the removed node must be gone from the graph"
    );
    assert_eq!(
        model.graph.edge_count(),
        edges_before - incident,
        "exactly the incident edges must have gone with it"
    );
    assert!(
        !rendered_ids(&app).contains(&"a1".to_string()),
        "build_graph must no longer render the removed node"
    );
}

/// ROADMAP SC-1: the seam list is recomputed INSIDE `drain_and_apply`, not
/// deferred to a later frame -- no caller can observe an applied event beside
/// a seam list that has not caught up.
#[test]
fn applying_a_batch_recomputes_the_ranked_seam_list_in_the_same_call() {
    let _guard = SERVE_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let path = serve_at("recompute-seams");
    let mut app = build_test_app();

    let seams_before = app.seams.clone();
    assert!(!seams_before.is_empty(), "guard: fixture must have seams");

    // A node carries no crossings of its own, so an add-only batch cannot
    // change `detect`'s output. The batch pairs the add with a removal that
    // genuinely moves the ranking -- otherwise "not the pre-call value"
    // would be unprovable (Rule 1 adjustment, recorded in the summary).
    send_and_wait(
        &path,
        &[
            add_node(
                "src/auth/session.rs::renew",
                "renew",
                Some("src/auth/session.rs"),
            ),
            GraphEvent::RemoveNode {
                id: "a1".to_string(),
            },
        ],
    );
    history::drain_and_apply(&mut app);

    let model = app.model.as_ref().expect("model must be loaded");
    let fresh = seam_core::detect(model);
    assert_ne!(
        app.seams, seams_before,
        "the seam list must not still be the pre-event value"
    );
    assert_eq!(
        app.seams, fresh,
        "the seam list must equal a freshly computed detect over the applied model"
    );
}

/// Preserves the property the superseded `graph_view.rs` call site's comment
/// protected: an event arriving before a graph is loaded is absorbed, not
/// left backing up in the bounded channel.
#[test]
fn an_event_arriving_before_a_graph_is_loaded_is_absorbed_not_stranded() {
    let _guard = SERVE_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let path = serve_at("no-graph");
    let mut app = SeamExplorerApp::default();
    assert!(app.model.is_none(), "guard: no graph loaded");

    let before = event_stream::received_count();
    send_and_wait(&path, &[add_node("x::y", "y", Some("x"))]);
    history::drain_and_apply(&mut app);

    assert!(
        event_stream::received_count() > before,
        "the event must have been received"
    );
    assert!(
        event_stream::drain().is_empty(),
        "the channel must have been emptied, not backed up"
    );
    assert!(app.model.is_none(), "no graph must have been conjured");
}

// ---------------------------------------------------------------------
// Task 2: the stale-SCC panic, through the real pipeline
// ---------------------------------------------------------------------

/// 08-RESEARCH.md Pitfall 1, as the user would actually hit it: a seam is
/// focused, the seam list is on screen scoring every visible row eagerly
/// through the cached SCC index, and a live `add_node` lands a node the cache
/// has never seen. Before the fix this panics inside
/// `verdict::has_cross_cycle`'s raw index (T-08-01-01); the panic IS the RED.
#[test]
fn a_live_add_node_while_a_seam_is_focused_does_not_crash_the_seam_list() {
    let _guard = SERVE_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let path = serve_at("focused-add");
    let mut app = build_test_app();

    // Focus a real seam pair from the ranked list, exactly as a row click
    // would (`panels::seam_list::select_seam`).
    let seam = app.seams.first().cloned().expect("fixture must have seams");
    {
        let model = app.model.as_ref().expect("model must be loaded");
        let scc = model
            .scc
            .as_ref()
            .expect("load must have finalized the SCC");
        app.detail = Some(seam_core::seam_detail(model, scc, &seam.a, &seam.b));
    }
    app.focus = Some(FocusState {
        a: seam.a.clone(),
        b: seam.b.clone(),
    });

    let mut harness = Harness::new_ui_state(
        |ui, app: &mut SeamExplorerApp| {
            panels::seam_list::show(ui, app);
        },
        app,
    );
    // One frame BEFORE the event: proves the eager per-row `seam_detail`
    // path is genuinely being exercised by this harness.
    harness.run();

    // The new node inherits community "A" from its `source_file` sibling,
    // which is one side of the focused seam -- that is what makes
    // `has_cross_cycle` reach the cache for an index the cache lacks. A node
    // parked in the sentinel bucket would never be scored against this seam
    // and so would never reproduce the crash.
    send_and_wait(
        &path,
        &[add_node(
            "src/auth/login.rs::verify_token",
            "verify_token",
            Some("src/auth/login.rs"),
        )],
    );
    history::drain_and_apply(harness.state_mut());
    assert!(
        harness
            .state()
            .model
            .as_ref()
            .expect("model must be loaded")
            .index
            .contains_key("src/auth/login.rs::verify_token"),
        "guard: the event must actually have applied, or this proves nothing"
    );

    // Rendering the seam list against the mutated model must not crash.
    harness.run();
}

// ---------------------------------------------------------------------
// Task 3: D-03 -- never show a lie
// ---------------------------------------------------------------------

/// Runs a REAL trace between two fixture nodes and installs it on the app,
/// exactly as a completed drag-to-trace gesture would. Returns the hops so a
/// test can pick an intermediate one to delete.
fn install_trace(app: &mut SeamExplorerApp, from: &str, to: &str) -> Vec<String> {
    let model = app.model.as_ref().expect("model must be loaded");
    let path = seam_core::trace_path(model, from, to)
        .unwrap_or_else(|| panic!("fixture must have a directed path {from} -> {to}"));
    let hops = path.hops.clone();
    assert!(
        hops.len() > 2,
        "guard: the trace must have an intermediate hop to delete, got {hops:?}"
    );
    app.trace = Some(TraceResult {
        from: from.to_string(),
        to: to.to_string(),
        path: Some(path),
    });
    hops
}

fn focus_seam(app: &mut SeamExplorerApp, a: &str, b: &str) {
    let model = app.model.as_ref().expect("model must be loaded");
    let scc = model
        .scc
        .as_ref()
        .expect("load must have finalized the SCC");
    assert!(
        app.seams
            .iter()
            .any(|s| (s.a == a && s.b == b) || (s.a == b && s.b == a)),
        "guard: {a} <-> {b} must be a real seam before focusing it"
    );
    app.detail = Some(seam_core::seam_detail(
        model,
        scc,
        &a.to_string(),
        &b.to_string(),
    ));
    app.focus = Some(FocusState {
        a: a.to_string(),
        b: b.to_string(),
    });
}

fn remove_node(id: &str) -> GraphEvent {
    GraphEvent::RemoveNode { id: id.to_string() }
}

/// D-03: a path drawn through a node that no longer exists is a lie. Clear
/// it rather than leave it pointing at data that is gone.
#[test]
fn a_trace_whose_hop_was_removed_is_cleared() {
    let _guard = SERVE_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let path = serve_at("trace-hop-gone");
    let mut app = build_test_app();

    let hops = install_trace(&mut app, "a1", "c2");
    let intermediate = hops[1].clone();
    assert!(
        app.trace.is_some(),
        "guard: the trace must really have resolved before the event"
    );

    send_and_wait(&path, &[remove_node(&intermediate)]);
    history::drain_and_apply(&mut app);

    assert!(
        app.trace.is_none(),
        "a trace routed through a removed node must be cleared"
    );
}

/// The negative case, asserted as hard as the positive one (Phase 6/7
/// convention): an unrelated removal must leave a healthy trace alone.
#[test]
fn a_trace_whose_endpoints_survive_is_left_alone() {
    let _guard = SERVE_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let path = serve_at("trace-survives");
    let mut app = build_test_app();

    let hops = install_trace(&mut app, "a1", "c2");
    let before = app.trace.as_ref().expect("guard: trace must exist").clone();
    // c1 is on no hop of a1 -> b2 -> c2.
    assert!(
        !hops.contains(&"c1".to_string()),
        "guard: c1 must be off-path"
    );

    send_and_wait(&path, &[remove_node("c1")]);
    history::drain_and_apply(&mut app);

    let after = app
        .trace
        .as_ref()
        .expect("an unaffected trace must survive an unrelated removal");
    assert_eq!(after.from, before.from);
    assert_eq!(after.to, before.to);
    assert_eq!(
        after.path, before.path,
        "the resolved path must be untouched"
    );
}

/// D-03: a focused seam with no crossing edges left has stopped being a
/// seam. Focus and its detail both go.
#[test]
fn a_focus_whose_seam_no_longer_exists_is_cleared_with_its_detail() {
    let _guard = SERVE_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let path = serve_at("focus-vanishes");
    let mut app = build_test_app();

    // A <-> C is held up by a single crossing edge (a1 -> c1). Removing all
    // of community C removes the seam entirely.
    focus_seam(&mut app, "A", "C");

    send_and_wait(&path, &[remove_node("c1"), remove_node("c2")]);
    history::drain_and_apply(&mut app);

    assert!(
        !app.seams
            .iter()
            .any(|s| (s.a == "A" && s.b == "C") || (s.a == "C" && s.b == "A")),
        "guard: the A <-> C seam must really be gone"
    );
    assert!(
        app.focus.is_none(),
        "a vanished seam's focus must be cleared"
    );
    assert!(app.detail.is_none(), "its detail must be cleared with it");
}

/// Stronger than D-03's literal "cleared" wording, and deliberately so:
/// SC-1 asks for verdicts recomputed to match, and
/// `graph_view::apply_focus_styling` paints bridge highlights from
/// `app.detail`'s node-id lists every frame. A surviving focus keeps its
/// panel open with a FRESHLY computed detail, not a stale one.
#[test]
fn a_surviving_focus_gets_a_freshly_recomputed_detail() {
    let _guard = SERVE_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let path = serve_at("focus-survives");
    let mut app = build_test_app();

    focus_seam(&mut app, "A", "B");
    let before = app.detail.clone().expect("guard: detail must be set");
    assert_eq!(
        before.a_to_b, 3,
        "guard: the fixture's A -> B crossings must start at 3"
    );

    // a1 carries two of the three A -> B crossings, so removing it changes
    // the detail without dissolving the seam (a2 -> b1 survives). Paired
    // with an add into community A so the batch is not removal-only.
    send_and_wait(
        &path,
        &[
            add_node(
                "src/auth/session.rs::renew",
                "renew",
                Some("src/auth/session.rs"),
            ),
            remove_node("a1"),
        ],
    );
    history::drain_and_apply(&mut app);

    assert_eq!(
        app.focus,
        Some(FocusState {
            a: "A".to_string(),
            b: "B".to_string()
        }),
        "a surviving seam must keep its focus"
    );
    let model = app.model.as_ref().expect("model must be loaded");
    let scc = model.scc.as_ref().expect("the SCC cache must be fresh");
    let fresh = seam_core::seam_detail(model, scc, &"A".to_string(), &"B".to_string());
    assert_ne!(
        fresh, before,
        "fixture guard: the event must genuinely change the detail, or this asserts nothing"
    );
    assert_eq!(
        app.detail,
        Some(fresh),
        "the surviving focus's detail must be recomputed against the current model"
    );
}

// ---------------------------------------------------------------------
// Plan 08-03 Task 2: the history in the live pipeline, and off disk
// ---------------------------------------------------------------------

/// An `eframe::Storage` that lives entirely in memory, so the persistence
/// test can exercise the REAL `eframe::set_value`/`get_value` pair rather
/// than a stand-in for them. Hand-written rather than pulled from a crate:
/// the trait is four methods, and this plan adds zero registry packages
/// (T-08-03-SC).
#[derive(Default)]
struct MemoryStorage {
    values: std::collections::HashMap<String, String>,
}

impl eframe::Storage for MemoryStorage {
    fn get_string(&self, key: &str) -> Option<String> {
        self.values.get(key).cloned()
    }
    fn set_string(&mut self, key: &str, value: String) {
        self.values.insert(key.to_string(), value);
    }
    fn remove_string(&mut self, key: &str) {
        self.values.remove(key);
    }
    fn flush(&mut self) {}
}

/// The recorded events, in the order the history holds them.
fn recorded_events(app: &SeamExplorerApp) -> Vec<GraphEvent> {
    app.history.iter().map(|e| e.event.clone()).collect()
}

fn resolved_add(id: &str, label: &str, community: &str, source_file: &str) -> GraphEvent {
    GraphEvent::AddNode {
        id: id.to_string(),
        label: label.to_string(),
        community: Some(community.to_string()),
        source_file: Some(source_file.to_string()),
    }
}

/// The history records what the graph DID, in the order it did it.
#[test]
fn applied_events_land_in_the_history_in_arrival_order() {
    let _guard = SERVE_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let path = serve_at("hist-order");
    let mut app = build_test_app();
    assert!(app.history.is_empty(), "guard: a fresh app records nothing");

    send_and_wait(
        &path,
        &[
            add_node(
                "src/auth/login.rs::verify_token",
                "verify_token",
                Some("src/auth/login.rs"),
            ),
            add_node("src/brand/new.rs::fresh", "fresh", Some("src/brand/new.rs")),
            remove_node("a1"),
        ],
    );
    let summary = history::drain_and_apply(&mut app);

    assert_eq!(app.history.len(), 3, "all three events changed the graph");
    assert_eq!(
        recorded_events(&app),
        vec![
            // Resolved forms, not the wire forms: the community was absent on
            // the wire for both adds and is concrete in both records.
            resolved_add(
                "src/auth/login.rs::verify_token",
                "verify_token",
                &expected_inherited_community("src/auth/login.rs"),
                "src/auth/login.rs",
            ),
            resolved_add(
                "src/brand/new.rs::fresh",
                "fresh",
                seam_core::UNKNOWN_COMMUNITY,
                "src/brand/new.rs",
            ),
            GraphEvent::RemoveNode {
                id: "a1".to_string()
            },
        ],
        "the history must hold exactly what was applied, in arrival order"
    );

    let ids: Vec<u64> = app.history.iter().map(|e| e.seq).collect();
    assert!(
        ids.windows(2).all(|pair| pair[1] > pair[0]),
        "identities must strictly increase across a batch, got {ids:?}"
    );
    assert_eq!(
        summary.recorded,
        Some((ids[0], ids[2])),
        "the summary must report the identity range this call recorded"
    );
}

/// The history records what happened to the graph, not what arrived on the
/// wire. A no-op that got a sequence number would make Phase 9's replay
/// produce a graph the user never saw.
#[test]
fn an_event_that_changed_nothing_is_not_recorded() {
    let _guard = SERVE_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let path = serve_at("hist-noop");
    let mut app = build_test_app();

    let before = event_stream::received_count();
    send_and_wait(&path, &[remove_node("no-such-node-anywhere")]);
    let summary = history::drain_and_apply(&mut app);

    // Distinguishes "not recorded" from "not delivered" -- without this the
    // test would pass just as happily if the datagram never arrived.
    assert!(
        event_stream::received_count() > before,
        "guard: the event must genuinely have been delivered"
    );
    assert!(
        app.history.is_empty(),
        "a remove_node for a node that does not exist changed nothing, so it \
         must not be recorded"
    );
    assert_eq!(
        app.history.next_seq(),
        0,
        "a no-op must not consume an identity either"
    );
    assert_eq!(
        summary.recorded, None,
        "a call that recorded nothing must report no identity range"
    );
}

/// The enforcement point `event.rs`'s own doc comment names and cannot
/// enforce itself: an absent wire community never survives into recorded
/// state.
#[test]
fn a_recorded_add_node_always_carries_a_resolved_community() {
    let _guard = SERVE_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let path = serve_at("hist-community");
    let mut app = build_test_app();

    // No community AND no source_file -- nothing at all for the resolver to
    // work from, the hardest case.
    send_and_wait(&path, &[add_node("bare::symbol", "symbol", None)]);
    history::drain_and_apply(&mut app);

    let community_of = |event: &GraphEvent| match event {
        GraphEvent::AddNode { community, .. } => community.clone(),
        other => panic!("expected an AddNode, got {other:?}"),
    };
    assert_eq!(app.history.len(), 1, "guard: the add must have applied");
    let recorded = recorded_events(&app);
    assert_eq!(
        community_of(&recorded[0]),
        Some(seam_core::UNKNOWN_COMMUNITY.to_string()),
        "a wholly unresolvable community must be recorded as the concrete \
         sentinel, never as absent"
    );

    // The same across a batch mixing resolvable and unresolvable nodes.
    send_and_wait(
        &path,
        &[
            add_node(
                "src/auth/session.rs::renew",
                "renew",
                Some("src/auth/session.rs"),
            ),
            add_node("src/nowhere.rs::orphan", "orphan", Some("src/nowhere.rs")),
            add_node("also-bare::thing", "thing", None),
        ],
    );
    history::drain_and_apply(&mut app);

    assert_eq!(app.history.len(), 4, "guard: every add must have applied");
    for event in recorded_events(&app) {
        let community = community_of(&event);
        assert!(
            community.is_some(),
            "every recorded AddNode must carry a concrete community \
             (event.rs's documented invariant), got None for {event:?}"
        );
        assert!(
            !community.unwrap_or_default().is_empty(),
            "a blank community is not a resolved one: {event:?}"
        );
    }
}

/// T-08-03-01: REQUIREMENTS.md's Out of Scope table excludes "Event
/// persistence across app restarts". `eframe`'s save hook fires on normal app
/// close, so a field left without `#[serde(skip)]` would ship that scope the
/// first time a user quits.
#[test]
fn the_history_does_not_survive_a_storage_round_trip() {
    let _guard = SERVE_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let path = serve_at("hist-storage");
    let mut app = build_test_app();

    // The positive control: the ONE field that IS meant to persist, set to a
    // non-default value.
    assert!(
        !SeamExplorerApp::default().has_seen_trace_onboarding,
        "guard: the control value must genuinely differ from the default"
    );
    app.has_seen_trace_onboarding = true;

    send_and_wait(
        &path,
        &[add_node(
            "src/auth/login.rs::verify_token",
            "verify_token",
            Some("src/auth/login.rs"),
        )],
    );
    history::drain_and_apply(&mut app);
    assert!(
        !app.history.is_empty(),
        "guard: there must be history to fail to persist"
    );

    let mut storage = MemoryStorage::default();
    eframe::set_value(&mut storage, eframe::APP_KEY, &app);
    let restored: SeamExplorerApp = eframe::get_value(&storage, eframe::APP_KEY)
        .expect("the app must round-trip through real eframe storage");

    assert!(
        restored.history.is_empty(),
        "the event history must NOT survive a storage round trip -- event \
         persistence across restarts is out of scope for v1.1"
    );
    assert_eq!(
        restored.history.next_seq(),
        0,
        "not even the counter may come back"
    );
    // Without this second assertion a round trip that silently did nothing at
    // all would pass the first one and prove nothing.
    assert!(
        restored.has_seen_trace_onboarding,
        "positive control: the one persisting field MUST come back with its \
         non-default value, or the round trip proved nothing"
    );
}

/// T-08-03-05: recorded events describe changes to a SPECIFIC loaded graph.
/// Replaying them against a different one would reconstruct a graph that
/// never existed.
#[test]
fn loading_a_different_graph_starts_a_new_timeline() {
    let _guard = SERVE_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let path = serve_at("hist-reload");
    let mut app = build_test_app();

    send_and_wait(
        &path,
        &[
            add_node(
                "src/auth/login.rs::verify_token",
                "verify_token",
                Some("src/auth/login.rs"),
            ),
            remove_node("a1"),
        ],
    );
    history::drain_and_apply(&mut app);
    assert_eq!(
        app.history.len(),
        2,
        "guard: the old graph's timeline must be non-empty"
    );

    let outcome = seam_explorer_egui::load::read_and_ingest(CLEAN_FIXTURE)
        .expect("the second fixture must ingest cleanly");
    app.apply_load_outcome(outcome);

    assert!(
        app.history.is_empty(),
        "a new graph must start with an empty history -- the old graph's \
         events are meaningless against it"
    );
    assert_eq!(
        app.history.next_seq(),
        0,
        "the identity counter must restart, so a stale identity cannot be \
         mistaken for a live one"
    );
    assert_eq!(
        app.history.evicted_count(),
        0,
        "the old graph's evictions are not the new graph's"
    );
}

// ---------------------------------------------------------------------
// Plan 08-03 Task 3: past the wrap -- ROADMAP SC-4
// ---------------------------------------------------------------------

/// A deterministic event script, together with the expectations derived FROM
/// THE SCRIPT as it is built. Nothing in here is ever read back out of the
/// model -- that is what stops the SC-4 assertions from comparing the model
/// to itself.
struct WrapScript {
    events: Vec<GraphEvent>,
    /// Live-added ids still present at the end of the script.
    surviving_live_ids: std::collections::BTreeSet<String>,
    /// Fixture ids the script deletes.
    removed_fixture_ids: std::collections::BTreeSet<String>,
}

/// Builds `total` events mixing adds and removes, including adds of nodes a
/// later event removes, and one removal of a FIXTURE node -- so the final
/// model is not simply "the fixture plus N nodes" and the expected edge count
/// actually moves.
///
/// Every minted id is unique and never re-added after removal, so every add
/// genuinely inserts and every remove genuinely deletes: all `total` events
/// apply, and the applied count is knowable in advance.
fn build_wrap_script(total: usize) -> WrapScript {
    let mut events: Vec<GraphEvent> = Vec::with_capacity(total);
    let mut pending: std::collections::VecDeque<String> = std::collections::VecDeque::new();
    let mut surviving_live_ids = std::collections::BTreeSet::new();
    let mut removed_fixture_ids = std::collections::BTreeSet::new();
    let mut minted = 0usize;

    while events.len() < total {
        if events.len() == total / 2 {
            events.push(remove_node("a1"));
            removed_fixture_ids.insert("a1".to_string());
            continue;
        }
        if events.len() % 3 == 2 && pending.len() > 1 {
            let doomed = pending
                .pop_front()
                .expect("guard: pending is non-empty by the branch condition");
            surviving_live_ids.remove(&doomed);
            events.push(remove_node(&doomed));
            continue;
        }
        // A quarter of the adds land under a fixture source path so community
        // inheritance is exercised under load, not just the sentinel bucket.
        let file = match minted % 4 {
            0 => "src/auth/login.rs".to_string(),
            1 => "src/db/pool.rs".to_string(),
            _ => format!("src/live/mod{}.rs", minted % 7),
        };
        let label = format!("live_sym{minted}");
        let id = format!("{file}::{label}");
        events.push(add_node(&id, &label, Some(&file)));
        pending.push_back(id.clone());
        surviving_live_ids.insert(id);
        minted += 1;
    }

    WrapScript {
        events,
        surviving_live_ids,
        removed_fixture_ids,
    }
}

struct FloodMetrics {
    applied_total: usize,
    peak_history_len: usize,
    elapsed: Duration,
}

/// Drives the script through the REAL socket in paced batches and applies
/// each batch through the real pipeline.
///
/// The pacing is not decoration. `event_stream`'s UI-thread channel is
/// bounded, so an unpaced flood legitimately sheds load and would turn every
/// assertion downstream into a coin flip. Batches stay well under
/// `CHANNEL_CAPACITY` and each is drained before the next is sent; the
/// dropped/discarded counters are then asserted unchanged, so loss FAILS this
/// test loudly instead of being silently tolerated.
fn drive_wrap_flood(path: &Path, app: &mut SeamExplorerApp, script: &[GraphEvent]) -> FloodMetrics {
    const BATCH: usize = 25;
    // A COMPILE-time check, not a runtime one: a batch must fit the bounded
    // channel with room to spare, and if someone later raises BATCH or lowers
    // CHANNEL_CAPACITY past each other, the right moment to find out is the
    // build, not a flaky test run.
    const _: () = assert!(BATCH < event_stream::CHANNEL_CAPACITY);
    let dropped_before = event_stream::dropped_count();
    let discarded_before = event_stream::discarded_count();

    let started = Instant::now();
    let mut applied_total = 0usize;
    let mut peak_history_len = 0usize;
    for chunk in script.chunks(BATCH) {
        send_and_wait(path, chunk);
        let summary = history::drain_and_apply(app);
        applied_total += summary.applied_count;
        peak_history_len = peak_history_len.max(app.history.len());
    }
    let elapsed = started.elapsed();

    assert_eq!(
        event_stream::dropped_count(),
        dropped_before,
        "the bounded channel shed load during the flood -- the pacing is wrong \
         and every count below would be a coin flip, so this fails rather than \
         tolerating the loss"
    );
    assert_eq!(
        event_stream::discarded_count(),
        discarded_before,
        "no scripted datagram may be discarded as oversized or unparseable"
    );

    FloodMetrics {
        applied_total,
        peak_history_len,
        elapsed,
    }
}

/// Every node id the fixture declares, read from the fixture JSON rather than
/// from any model.
fn fixture_node_ids() -> std::collections::BTreeSet<String> {
    let doc: serde_json::Value =
        serde_json::from_str(SOURCE_PATHS_FIXTURE).expect("fixture must be valid JSON");
    doc["nodes"]
        .as_array()
        .expect("fixture must have a nodes array")
        .iter()
        .map(|n| {
            n["id"]
                .as_str()
                .expect("every fixture node must carry a string id")
                .to_string()
        })
        .collect()
}

/// A kind+identity key, so a recorded (RESOLVED) event can be compared
/// against the script event that produced it without the resolved community
/// making them trivially unequal.
fn event_key(event: &GraphEvent) -> (&'static str, String) {
    match event {
        GraphEvent::AddNode { id, .. } => ("add_node", id.clone()),
        GraphEvent::RemoveNode { id } => ("remove_node", id.clone()),
        GraphEvent::AddEdge { source, target } => ("add_edge", format!("{source}->{target}")),
        GraphEvent::RemoveEdge { source, target } => ("remove_edge", format!("{source}->{target}")),
    }
}

/// ROADMAP SC-4, directly: past two full wraparounds the history is bounded,
/// its accounting closes, and the displayed graph is still correct.
#[test]
fn past_the_wrap_the_buffer_is_bounded_and_the_graph_is_still_right() {
    let _guard = SERVE_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let path = serve_at("wrap-bounded");
    let mut app = build_test_app();
    let cap = seam_core::LIVE_BUFFER_CAPACITY;

    // Every observation used to build the expectation is taken BEFORE the
    // flood. Nothing below reads the post-flood model to decide what the
    // post-flood model should be.
    let fixture_ids = fixture_node_ids();
    let (edges_before, incident_a1) = {
        let model = app.model.as_ref().expect("model must be loaded");
        let loaded: std::collections::BTreeSet<String> = model.index.keys().cloned().collect();
        assert_eq!(
            loaded, fixture_ids,
            "guard: ingest must not filter nodes, or a fixture-derived expectation \
             would be wrong for reasons unrelated to this plan"
        );
        let idx = *model.index.get("a1").expect("fixture node a1 must exist");
        let incident = model
            .graph
            .edges_directed(idx, petgraph::Direction::Outgoing)
            .count()
            + model
                .graph
                .edges_directed(idx, petgraph::Direction::Incoming)
                .count();
        (model.graph.edge_count(), incident)
    };

    let script = build_wrap_script(cap * 5 / 2);
    assert_eq!(
        script.events.len(),
        250,
        "guard: the script must wrap the buffer at least twice"
    );
    let metrics = drive_wrap_flood(&path, &mut app, &script.events);
    println!(
        "[past_the_wrap] drove {} events through the real socket in {:?}; \
         peak history length {}",
        metrics.applied_total, metrics.elapsed, metrics.peak_history_len
    );

    // SC-4 part 1: bounded.
    assert_eq!(
        app.history.len(),
        cap,
        "past the wrap the history must sit exactly at its capacity"
    );
    assert_eq!(
        metrics.peak_history_len, cap,
        "the history must never have exceeded its capacity at any point"
    );

    // SC-4 part 2: the accounting closes.
    assert_eq!(
        metrics.applied_total,
        script.events.len(),
        "every scripted event must have applied -- the script mints unique ids \
         and only removes nodes it knows exist, so a shortfall is a real defect"
    );
    assert_eq!(
        app.history.evicted_count() as usize + app.history.len(),
        metrics.applied_total,
        "evicted plus retained must equal total ever applied"
    );
    assert_eq!(
        app.history.next_seq(),
        metrics.applied_total as u64,
        "the next identity must equal total ever applied"
    );

    // SC-4 part 3: the model matches an INDEPENDENTLY computed expectation.
    let mut expected_ids: std::collections::BTreeSet<String> = fixture_ids
        .difference(&script.removed_fixture_ids)
        .cloned()
        .collect();
    expected_ids.extend(script.surviving_live_ids.iter().cloned());
    let expected_edges = edges_before - incident_a1;
    assert_ne!(
        expected_edges, edges_before,
        "guard: the script must genuinely move the edge count, or this half of \
         the assertion proves nothing"
    );

    let model = app.model.as_ref().expect("model must be loaded");
    let actual_ids: std::collections::BTreeSet<String> = model.index.keys().cloned().collect();
    assert_eq!(
        actual_ids, expected_ids,
        "the surviving node set must match the set computed from the fixture \
         and the script"
    );
    assert_eq!(
        model.graph.node_count(),
        expected_ids.len(),
        "the graph and its index must agree on how many nodes survived"
    );
    assert_eq!(
        model.graph.edge_count(),
        expected_edges,
        "removing a1 must have taken exactly its incident edges, and nothing else"
    );

    // SC-4 part 4: the seam list and the SCC cache agree with the final model.
    assert_eq!(
        app.seams,
        seam_core::detect(model),
        "the ranked seam list must equal a fresh detection over the final model"
    );
    let scc = model
        .scc
        .as_ref()
        .expect("the SCC cache must have been finalized");
    for idx in model.graph.node_indices() {
        assert!(
            scc.scc_of(idx).is_some(),
            "every node index must have an SCC entry; missing for {:?}",
            model.graph[idx].id
        );
    }
}

/// EVENT-04's wraparound half, asserted against the real pipeline rather than
/// a hand-built buffer.
#[test]
fn a_history_that_wrapped_still_answers_for_its_surviving_identities() {
    let _guard = SERVE_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let path = serve_at("wrap-answers");
    let mut app = build_test_app();
    let cap = seam_core::LIVE_BUFFER_CAPACITY;

    let script = build_wrap_script(cap * 5 / 2);
    let metrics = drive_wrap_flood(&path, &mut app, &script.events);
    println!(
        "[wrapped_still_answers] drove {} events in {:?}; peak history length {}",
        metrics.applied_total, metrics.elapsed, metrics.peak_history_len
    );

    let evicted = app.history.evicted_count();
    assert!(evicted > 0, "guard: the buffer must actually have wrapped");

    // An identity from before the first eviction resolves to NOTHING -- not to
    // some other event that happens to sit where it used to.
    for seq in 0..evicted {
        assert!(
            app.history.get(seq).is_none(),
            "identity {seq} was evicted and must report as gone"
        );
    }
    assert!(
        app.history.get(app.history.next_seq()).is_none(),
        "an identity never issued must resolve to nothing"
    );

    // Every survivor resolves, and resolves to ITSELF.
    for entry in app.history.iter() {
        let found = app
            .history
            .get(entry.seq)
            .expect("a surviving identity must still resolve after two wraps");
        assert_eq!(
            found.seq, entry.seq,
            "a lookup must return the entry whose identity was asked for"
        );
        assert_eq!(
            found.event, entry.event,
            "a lookup must return that entry's own event"
        );
    }

    // And the surviving window is the LAST `cap` events of the script, in
    // order -- checked against the script, not against the buffer.
    let recorded: Vec<(&'static str, String)> =
        app.history.iter().map(|e| event_key(&e.event)).collect();
    let expected: Vec<(&'static str, String)> = script.events[script.events.len() - cap..]
        .iter()
        .map(event_key)
        .collect();
    assert_eq!(
        recorded, expected,
        "the surviving window must be the last {cap} scripted events, in order"
    );
    assert_eq!(
        app.history
            .iter()
            .next()
            .expect("guard: the history is non-empty")
            .seq,
        (script.events.len() - cap) as u64,
        "the oldest survivor's identity must be total-minus-capacity"
    );
}

// ---------------------------------------------------------------------
// Plan 08-04 Task 3: edges on the canvas and in the history, end to end
// ---------------------------------------------------------------------

/// The fixture shaped like the REAL Graphify export -- a FILE node whose
/// `label` and `source_file` are both the repo-relative path, symbol nodes
/// whose `label` is the bare symbol, and opaque slug `id`s unrelated to
/// either. `SOURCE_PATHS_FIXTURE` deliberately has no file node at all, so it
/// cannot show that a resolved endpoint DIFFERS from the wire string.
const EDGE_SHAPES_FIXTURE: &str = include_str!("../../seam-core/tests/fixtures/edge_shapes.json");

fn build_edge_shapes_app() -> SeamExplorerApp {
    let outcome = seam_explorer_egui::load::read_and_ingest(EDGE_SHAPES_FIXTURE)
        .expect("edge_shapes fixture must ingest cleanly");
    SeamExplorerApp {
        model: Some(outcome.model),
        seams: outcome.seams,
        ..Default::default()
    }
}

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

/// Every edge `build_graph` actually renders, as a pair of PAYLOAD ids --
/// the same lookup path `graph_view::show` uses to paint them.
fn rendered_edges(app: &SeamExplorerApp) -> std::collections::BTreeSet<(String, String)> {
    let model = app.model.as_ref().expect("model must be loaded");
    let g = graph_view::build_graph(model, None);
    g.edges_iter()
        .filter_map(|(edge_idx, _)| {
            let (s, t) = g.edge_endpoints(edge_idx)?;
            Some((
                g.node(s)?.payload().id.clone(),
                g.node(t)?.payload().id.clone(),
            ))
        })
        .collect()
}

fn bump_crossing(
    counts: &mut std::collections::BTreeMap<(String, String), usize>,
    a: &str,
    b: &str,
) {
    if a == b {
        return;
    }
    let key = if a < b {
        (a.to_string(), b.to_string())
    } else {
        (b.to_string(), a.to_string())
    };
    *counts.entry(key).or_insert(0) += 1;
}

/// The crossing count per community pair, computed from the FIXTURE JSON by
/// re-applying ingest's own relation+confidence filter -- never read back out
/// of a model. This is what stops the rerank assertion from comparing
/// `app.seams` to itself.
fn expected_crossings(fixture: &str) -> std::collections::BTreeMap<(String, String), usize> {
    let doc: serde_json::Value = serde_json::from_str(fixture).expect("fixture must be valid JSON");
    let community: std::collections::HashMap<String, String> = doc["nodes"]
        .as_array()
        .expect("fixture must have a nodes array")
        .iter()
        .map(|n| {
            (
                n["id"].as_str().expect("string id").to_string(),
                n["community"]
                    .as_str()
                    .expect("string community")
                    .to_string(),
            )
        })
        .collect();

    let mut counts = std::collections::BTreeMap::new();
    for link in doc["links"].as_array().expect("fixture must have links") {
        let relation = link["relation"].as_str().unwrap_or_default();
        if !seam_core::STRUCTURAL_RELATIONS.contains(&relation) {
            continue;
        }
        if link["confidence"].as_str() != Some("EXTRACTED") {
            continue;
        }
        let source = link["source"].as_str().expect("string source");
        let target = link["target"].as_str().expect("string target");
        bump_crossing(&mut counts, &community[source], &community[target]);
    }
    counts
}

/// A crossing-count map turned into the ranked list `seam_core::detect`
/// would produce: highest crossing count first.
fn ranked(
    counts: &std::collections::BTreeMap<(String, String), usize>,
) -> Vec<(String, String, usize)> {
    let mut ranked: Vec<(String, String, usize)> = counts
        .iter()
        .map(|((a, b), n)| (a.clone(), b.clone(), *n))
        .collect();
    ranked.sort_by(|x, y| y.2.cmp(&x.2));
    ranked
}

fn observed_ranking(app: &SeamExplorerApp) -> Vec<(String, String, usize)> {
    app.seams
        .iter()
        .map(|s| (s.a.clone(), s.b.clone(), s.crossings))
        .collect()
}

/// SC-1 for edges: a live `add_edge` reaches the canvas AND the ranked seam
/// list in the same call.
///
/// Three edges rather than one, deliberately: one A-C crossing would tie A-C
/// with B-C at 2, and `detect`'s sort is stable over a HashMap-ordered
/// collect, so a tie makes the RANK assertion a coin flip. Three lifts A-C
/// from last place to first with no tie anywhere, which is a stronger claim
/// about reranking than "the number went up" and a deterministic one.
#[test]
fn a_live_add_edge_between_two_communities_appears_on_the_canvas_and_reranks_the_seam_list() {
    let _guard = SERVE_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let path = serve_at("edge-rerank");
    let mut app = build_test_app();

    let mut expected = expected_crossings(SOURCE_PATHS_FIXTURE);
    let ac = ("A".to_string(), "C".to_string());
    let before = *expected
        .get(&ac)
        .expect("guard: A-C must already be a seam");
    assert_eq!(
        observed_ranking(&app),
        ranked(&expected),
        "guard: the pre-event ranking must match the fixture-derived expectation"
    );

    send_and_wait(
        &path,
        &[
            add_edge("src/auth/login.rs", "db::c1"),
            add_edge("src/auth/login.rs", "db::c2"),
            add_edge("src/auth/session.rs", "db::c1"),
        ],
    );
    for _ in 0..3 {
        bump_crossing(&mut expected, "A", "C");
    }
    history::drain_and_apply(&mut app);

    // 1. The canvas.
    let drawn = rendered_edges(&app);
    for (source, target) in [
        ("src/auth/login.rs", "c1"),
        ("src/auth/login.rs", "c2"),
        ("src/auth/session.rs", "c1"),
    ] {
        assert!(
            drawn.contains(&(source.to_string(), target.to_string())),
            "build_graph must render an edge {source} -> {target}; drawn: {drawn:?}"
        );
    }

    // 2. The crossing count for that pair went up, by the amount scripted.
    let after = app
        .seams
        .iter()
        .find(|s| s.a == ac.0 && s.b == ac.1)
        .map(|s| s.crossings)
        .expect("A-C must still be a seam");
    assert_eq!(
        after,
        before + 3,
        "the crossing count must grow by the three edges"
    );

    // 3. The whole ranking, against the independently built expectation.
    assert_eq!(
        observed_ranking(&app),
        ranked(&expected),
        "the ranked seam list must match the fixture-plus-script expectation, \
         not merely 'something changed'"
    );
    assert_eq!(
        app.seams
            .first()
            .map(|s| (s.a.clone(), s.b.clone()))
            .expect("the list is non-empty"),
        ac,
        "A-C must have been lifted from last place to first"
    );
}

/// EVENT-04's remaining clause for edges: what the history records is what
/// the GRAPH gained, in the graph's own identity scheme.
#[test]
fn a_recorded_edge_event_carries_resolved_endpoints_not_the_wire_strings() {
    let _guard = SERVE_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let path = serve_at("edge-resolved");
    let mut app = build_edge_shapes_app();
    let seq_before = app.history.next_seq();

    // Source: a bare repo-relative path. Target: a qualified reference.
    // Neither equals the opaque id of the node it names.
    const WIRE_SOURCE: &str = "src/auth/login.rs";
    const WIRE_TARGET: &str = "db::connect";
    send_and_wait(&path, &[add_edge(WIRE_SOURCE, WIRE_TARGET)]);
    history::drain_and_apply(&mut app);

    assert_eq!(
        app.history.next_seq(),
        seq_before + 1,
        "an applied edge must consume exactly one identity"
    );
    let entry = app
        .history
        .get(seq_before)
        .expect("the recorded identity must resolve");
    let GraphEvent::AddEdge { source, target } = &entry.event else {
        panic!(
            "the recorded event must be an AddEdge, got {:?}",
            entry.event
        );
    };
    assert_eq!(source, "src_auth_login");
    assert_eq!(target, "src_db_pool_connect");
    // The part a passthrough implementation cannot fake.
    assert_ne!(
        source, WIRE_SOURCE,
        "the recorded source must be the RESOLVED node id, not the wire string"
    );
    assert_ne!(
        target, WIRE_TARGET,
        "the recorded target must be the RESOLVED node id, not the wire string"
    );
}

/// Silence, asserted as hard as delivery (the Phase 6/7 convention): an edge
/// to a symbol outside the project changes nothing and records nothing --
/// but it WAS received, so this is a decision, not a lost datagram.
#[test]
fn a_dropped_external_edge_is_not_recorded_and_changes_nothing() {
    let _guard = SERVE_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let path = serve_at("edge-dropped");
    let mut app = build_edge_shapes_app();
    let seq_before = app.history.next_seq();
    let history_before = app.history.len();
    let received_before = event_stream::received_count();
    let (nodes_before, edges_before) = {
        let model = app.model.as_ref().expect("model must be loaded");
        (model.graph.node_count(), model.graph.edge_count())
    };

    send_and_wait(
        &path,
        &[add_edge("src/db/pool.rs", "std::collections::HashMap")],
    );
    let summary = history::drain_and_apply(&mut app);

    assert!(
        event_stream::received_count() > received_before,
        "the datagram must genuinely have been received -- 'not recorded' has \
         to be distinguishable from 'not delivered'"
    );
    assert_eq!(summary.dropped_external_edges, 1);
    assert_eq!(summary.applied_count, 0);
    assert_eq!(summary.recorded, None, "nothing may be recorded");
    assert_eq!(app.history.len(), history_before);
    assert_eq!(
        app.history.next_seq(),
        seq_before,
        "no identity is consumed"
    );

    let model = app.model.as_ref().expect("model must be loaded");
    assert_eq!(
        model.graph.node_count(),
        nodes_before,
        "no node may be synthesized for a dropped edge -- not the target, and \
         not its source either"
    );
    assert_eq!(model.graph.edge_count(), edges_before);
    assert!(
        model.pending_edges.is_empty(),
        "an external target never parks"
    );
}

/// A parked edge waits; it does not pretend to have happened -- and then it
/// lands when its target finally arrives.
///
/// **08-04 wrote the first half only, and said so:** it stopped at "parked"
/// because the retry sweep did not exist yet, and asserting materialization
/// then would have made that plan's suite fail for a reason belonging to this
/// one. 08-05 EXTENDS the test rather than replacing it, so the deferral is
/// visibly closed instead of quietly removed.
#[test]
fn a_parked_edge_is_not_recorded_until_it_materializes() {
    let _guard = SERVE_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let path = serve_at("edge-parked");
    let mut app = build_edge_shapes_app();
    let seq_before = app.history.next_seq();
    let (nodes_before, edges_before) = {
        let model = app.model.as_ref().expect("model must be loaded");
        (model.graph.node_count(), model.graph.edge_count())
    };

    send_and_wait(
        &path,
        &[add_edge("src/db/pool.rs", "auth::not_yet_written")],
    );
    let summary = history::drain_and_apply(&mut app);

    assert_eq!(summary.parked_edges, 1);
    assert_eq!(summary.applied_count, 0);
    assert_eq!(summary.recorded, None, "a parked edge did not happen yet");
    assert_eq!(app.history.next_seq(), seq_before);

    let model = app.model.as_ref().expect("model must be loaded");
    assert_eq!(model.graph.node_count(), nodes_before);
    assert_eq!(model.graph.edge_count(), edges_before);
    assert_eq!(
        model.pending_edges.len(),
        1,
        "an internal target might still arrive, so its edge waits (D-05)"
    );

    // --- 08-05 closes the deferral: the target arrives. ---
    send_and_wait(
        &path,
        &[add_node(
            "src_auth_not_yet",
            "not_yet_written",
            Some("src/auth/login.rs"),
        )],
    );
    let summary = history::drain_and_apply(&mut app);

    assert_eq!(
        summary.materialized_edges, 1,
        "the edge that was waiting must land in the batch its target arrived in"
    );
    let model = app.model.as_ref().expect("model must be loaded");
    assert!(
        model.pending_edges.is_empty(),
        "a materialized edge must leave the store"
    );
    assert!(
        rendered_edges(&app)
            .contains(&("src/db/pool.rs".to_string(), "src_auth_not_yet".to_string())),
        "the materialized edge must be on the canvas, drawn between the \
         synthesized source node and the newly arrived target"
    );
    assert_eq!(
        app.history.next_seq(),
        seq_before + 2,
        "two identities: the node that arrived, and the edge that materialized \
         because of it"
    );
}

/// The removal half, end to end: off the canvas AND out of the ranking.
///
/// Three edges again, and for the same reason as the add test: a single
/// added A-C crossing would leave A-C tied with B-C at 2, and `seam_core::
/// detect` sorts a HashMap-ordered collect, so the ORDER between equally
/// ranked pairs is not defined. Both states asserted here -- three edges up,
/// then all three back off -- are tie-free, so the rank assertion tests the
/// rerank rather than the iteration order of a HashMap.
#[test]
fn a_live_remove_edge_takes_the_edge_off_the_canvas() {
    let _guard = SERVE_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let path = serve_at("edge-remove");
    let mut app = build_test_app();
    let baseline = expected_crossings(SOURCE_PATHS_FIXTURE);
    const SCRIPT: [(&str, &str); 3] = [
        ("src/auth/login.rs", "db::c1"),
        ("src/auth/login.rs", "db::c2"),
        ("src/auth/session.rs", "db::c1"),
    ];
    let drawn_pairs = [
        ("src/auth/login.rs".to_string(), "c1".to_string()),
        ("src/auth/login.rs".to_string(), "c2".to_string()),
        ("src/auth/session.rs".to_string(), "c1".to_string()),
    ];

    let adds: Vec<GraphEvent> = SCRIPT.iter().map(|(s, t)| add_edge(s, t)).collect();
    send_and_wait(&path, &adds);
    history::drain_and_apply(&mut app);
    let drawn = rendered_edges(&app);
    for pair in &drawn_pairs {
        assert!(
            drawn.contains(pair),
            "guard: {pair:?} must be on the canvas before the removal means anything"
        );
    }
    let mut with_edges = baseline.clone();
    for _ in 0..3 {
        bump_crossing(&mut with_edges, "A", "C");
    }
    assert_eq!(observed_ranking(&app), ranked(&with_edges));

    let removes: Vec<GraphEvent> = SCRIPT.iter().map(|(s, t)| remove_edge(s, t)).collect();
    send_and_wait(&path, &removes);
    history::drain_and_apply(&mut app);

    let drawn = rendered_edges(&app);
    for pair in &drawn_pairs {
        assert!(
            !drawn.contains(pair),
            "the removed edge {pair:?} must be off the canvas; drawn: {drawn:?}"
        );
    }
    assert_eq!(
        observed_ranking(&app),
        ranked(&baseline),
        "the ranked list must follow the removal back down"
    );
}

/// D-03's second clearing rule -- a trace whose consecutive hops no longer
/// CONNECT -- reached for the first time.
///
/// 08-01 wrote that branch when no edge could be removed, so the only way to
/// break a trace was to delete one of its hops, which the first rule
/// (`!model.index.contains_key`) catches before the connectivity check is
/// ever consulted. This plan makes edges removable, which is what finally
/// makes the branch reachable, so it is exercised here for the first time.
///
/// `b1` carries no `source_file`, so no repo-relative path names it. The
/// removal therefore travels by `apply_remove_edge`'s exact-id fallback --
/// a documented resolution path, and the only one this fixture offers.
#[test]
fn a_trace_whose_connecting_edge_was_removed_is_cleared() {
    let _guard = SERVE_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let path = serve_at("edge-trace-clear");
    let mut app = build_test_app();

    let hops = install_trace(&mut app, "a2", "c1");
    assert_eq!(
        hops,
        vec!["a2".to_string(), "b1".to_string(), "c1".to_string()],
        "guard: the trace must route a2 -> b1 -> c1, so removing b1 -> c1 breaks \
         a CONSECUTIVE PAIR while leaving every hop in the graph"
    );

    send_and_wait(&path, &[remove_edge("b1", "c1")]);
    history::drain_and_apply(&mut app);

    let model = app.model.as_ref().expect("model must be loaded");
    for hop in &hops {
        assert!(
            model.index.contains_key(hop),
            "guard: every hop must SURVIVE, or the first clearing rule fires and \
             this test proves nothing about the connectivity rule"
        );
    }
    assert!(
        app.trace.is_none(),
        "a path drawn between nodes that no longer connect is exactly the lie \
         D-03 refuses to ship"
    );
}

// ---------------------------------------------------------------------
// 08-05 Task 2: D-05's user-visible promise, over the real socket
// ---------------------------------------------------------------------

/// The crossing count for one community pair, or zero when the pair is not a
/// seam at all.
fn crossings_between(app: &SeamExplorerApp, a: &str, b: &str) -> usize {
    app.seams
        .iter()
        .find(|s| (s.a == a && s.b == b) || (s.a == b && s.b == a))
        .map(|s| s.crossings)
        .unwrap_or(0)
}

/// D-05, as the user experiences it: an edge reported before the thing it
/// points at existed shows up when that thing arrives.
///
/// The rerank assertion is engineered tie-free on purpose. `edge_shapes.json`
/// starts with A-B and A-C both at one crossing, and `seam_core::detect` has
/// no tie-break among equal counts (logged in this phase's deferred-items.md),
/// so asserting an ORDER over the starting state would be a coin flip. The
/// materialized edge lifts A-B to two, which is tie-free, and that is the
/// state the ordering assertion is made against.
#[test]
fn an_edge_sent_before_its_node_shows_up_when_the_node_arrives() {
    let _guard = SERVE_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let path = serve_at("edge-materialize");
    let mut app = build_edge_shapes_app();

    assert_eq!(
        crossings_between(&app, "A", "B"),
        1,
        "guard: the fixture's A-B seam must start at one crossing"
    );
    let drawn_before = rendered_edges(&app);

    // Sent first: `db` is vouched for by src/db/pool.rs so the target is
    // Internal, but nothing labelled `ghost_fn` exists yet.
    send_and_wait(&path, &[add_edge("src/auth/login.rs", "db::ghost_fn")]);
    let summary = history::drain_and_apply(&mut app);

    assert_eq!(summary.parked_edges, 1, "guard: the edge must be waiting");
    assert_eq!(
        rendered_edges(&app),
        drawn_before,
        "nothing may appear on the canvas for an edge that has not landed"
    );

    // Sent second: the node the edge was waiting for.
    send_and_wait(
        &path,
        &[add_node("src_db_ghost", "ghost_fn", Some("src/db/pool.rs"))],
    );
    let summary = history::drain_and_apply(&mut app);

    assert_eq!(summary.materialized_edges, 1);
    assert!(
        rendered_edges(&app).contains(&("src_auth_login".to_string(), "src_db_ghost".to_string())),
        "the edge must now be rendered by build_graph, between the two RESOLVED \
         node ids; drawn: {:?}",
        rendered_edges(&app)
    );
    assert_eq!(
        crossings_between(&app, "A", "B"),
        2,
        "the seam list must have reranked in the same call -- SC-1's 'not a \
         stale list beside a changed canvas' applies to a materialized edge \
         exactly as it does to a directly applied one"
    );
    assert_eq!(
        app.seams
            .first()
            .map(|s| (s.a.clone(), s.b.clone(), s.crossings))
            .expect("the list is non-empty"),
        ("A".to_string(), "B".to_string(), 2),
        "A-B must now lead the ranking outright"
    );
}

/// T-08-05-06: the history must hold the materialization in the batch it
/// HAPPENED in, not the batch the datagram was sent in. Phase 9 replaying a
/// history that placed it earlier would reconstruct a graph the user never
/// saw; one that omitted it entirely would reconstruct a graph missing an
/// edge.
#[test]
fn the_materialized_edge_is_recorded_in_the_history_when_it_lands_not_when_it_was_sent() {
    let _guard = SERVE_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let path = serve_at("edge-materialize-history");
    let mut app = build_edge_shapes_app();
    let seq_before = app.history.next_seq();

    send_and_wait(&path, &[add_edge("src/auth/login.rs", "db::ghost_fn")]);
    let summary = history::drain_and_apply(&mut app);

    assert_eq!(summary.recorded, None, "the earlier batch records nothing");
    assert_eq!(
        app.history.next_seq(),
        seq_before,
        "a parked edge consumes no identity"
    );

    send_and_wait(
        &path,
        &[add_node("src_db_ghost", "ghost_fn", Some("src/db/pool.rs"))],
    );
    let summary = history::drain_and_apply(&mut app);

    assert_eq!(
        summary.applied_count, 2,
        "the arriving node AND the edge it unblocked both changed the graph"
    );
    let recorded: Vec<GraphEvent> = app
        .history
        .iter()
        .filter(|entry| entry.seq >= seq_before)
        .map(|entry| entry.event.clone())
        .collect();
    assert!(
        recorded.contains(&GraphEvent::AddEdge {
            source: "src_auth_login".to_string(),
            target: "src_db_ghost".to_string(),
        }),
        "the materialized edge must be recorded with RESOLVED endpoints, in the \
         batch it landed in; recorded: {recorded:?}"
    );
    assert!(
        recorded
            .iter()
            .any(|e| matches!(e, GraphEvent::AddNode { id, .. } if id == "src_db_ghost")),
        "the node that unblocked it is recorded too, and before it"
    );
}
