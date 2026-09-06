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
