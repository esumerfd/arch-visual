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

use seam_core::GraphEvent;
use seam_explorer_egui::app::SeamExplorerApp;
use seam_explorer_egui::{event_stream, graph_view, history};

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
