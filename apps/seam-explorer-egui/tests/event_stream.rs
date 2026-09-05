//! Real-socket integration tests for `event_stream` (plan 06-02). Every test
//! here uses a REAL `AF_UNIX`/`SOCK_DGRAM` socket on disk -- no mock socket
//! type exists anywhere in this crate (EVENT-03's structural guarantee).
//!
//! Task 1 (this file's four tests) proves the tracer: one datagram, sent by
//! a separate thread, travelling kernel socket -> background thread ->
//! bounded channel -> a drain reachable from `graph_view::show` on the
//! eframe UI thread, plus the repaint wake that makes "the UI updates
//! without the user jiggling the mouse" true (ROADMAP SC-1).
//!
//! Task 2 adds a second wave of tests to this same file, proving the bind
//! lifecycle survives the four real-world failure modes: a too-long path, a
//! missing config directory, a stale inode left by `kill -9`, and a live
//! second instance.
//!
//! `SERVE_TEST_LOCK` serializes every test that touches the process-global
//! (`event_stream::serve`/`drain`/`received_count`) -- `cargo test`'s default
//! parallelism would otherwise race on it, exactly as plan 05-22 documented
//! for `settings::Store`. Tests using `spawn_receiver` directly need no lock,
//! which is the point of the split (see `spawn_receiver_hands_back_isolated_stats`).

use std::os::unix::net::UnixDatagram;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use egui_kittest::Harness;
use seam_core::GraphEvent;
use seam_explorer_egui::app::SeamExplorerApp;
use seam_explorer_egui::{event_stream, graph_view};

/// Serializes every test that touches `event_stream`'s process-global.
static SERVE_TEST_LOCK: Mutex<()> = Mutex::new(());

/// Per-test-unique temp directory under the OS temp dir, so parallel test
/// runs cannot collide. No `tempfile` dependency -- same recipe as
/// `settings.rs`'s test module.
fn temp_dir(unique: &str) -> PathBuf {
    // Kept short deliberately: this path plus "/seam.sock" must stay well
    // under the 104-byte sun_path ceiling for every non-over_limit_path
    // test in this file, on top of whatever length $TMPDIR already is.
    std::env::temp_dir().join(format!("es-evt-{}-{}", std::process::id(), unique))
}

fn temp_socket_path(unique: &str) -> PathBuf {
    temp_dir(unique).join("seam.sock")
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

// ---------------------------------------------------------------------
// Task 1: the tracer
// ---------------------------------------------------------------------

/// The tracer's own proof: a datagram built and sent from a SEPARATE thread
/// arrives on `EventReceiver::drain()`, reconstructed independently on the
/// asserting side so the assertion cannot pass by aliasing the sender's
/// value.
#[test]
fn a_datagram_sent_from_another_thread_arrives_on_the_drain() {
    let path = temp_socket_path("delivery");
    let socket = event_stream::bind_at(&path).expect("bind_at must succeed for a fresh path");
    let receiver = event_stream::spawn_receiver(socket, egui::Context::default());

    let send_path = path.clone();
    std::thread::spawn(move || {
        let bytes = seam_core::to_datagram(&GraphEvent::AddEdge {
            source: "svc::a".to_string(),
            target: "svc::b".to_string(),
        });
        let sender = UnixDatagram::unbound().expect("unbound socket must be constructible");
        sender
            .send_to(&bytes, &send_path)
            .expect("send_to a bound socket must succeed");
    });

    let mut drained: Vec<GraphEvent> = Vec::new();
    wait_until(Duration::from_secs(5), || {
        drained.extend(receiver.drain());
        !drained.is_empty()
    });

    assert_eq!(
        drained,
        vec![GraphEvent::AddEdge {
            source: "svc::a".to_string(),
            target: "svc::b".to_string(),
        }],
        "the drained event must equal an independently-reconstructed AddEdge{{svc::a, svc::b}}"
    );
    assert_eq!(
        receiver.stats().received(),
        1,
        "stats().received() must count exactly the one delivered event"
    );

    let _ = std::fs::remove_dir_all(temp_dir("delivery"));
}

/// Proves the app is WOKEN by a received datagram, not merely eventually
/// consistent with one -- the difference between "no perceptible delay"
/// (ROADMAP SC-1) and "updates whenever the mouse happens to move".
#[test]
fn the_repaint_wake_fires_for_a_received_datagram() {
    let path = temp_socket_path("repaint-wake");
    let socket = event_stream::bind_at(&path).expect("bind_at must succeed for a fresh path");

    let mut harness = Harness::new_ui(|_ui| {});
    harness.run_steps(3);
    let ctx = harness.ctx.clone();

    let receiver = event_stream::spawn_receiver(socket, ctx.clone());

    let send_path = path.clone();
    std::thread::spawn(move || {
        let bytes = seam_core::to_datagram(&GraphEvent::RemoveNode {
            id: "svc::c".to_string(),
        });
        let sender = UnixDatagram::unbound().expect("unbound socket must be constructible");
        sender
            .send_to(&bytes, &send_path)
            .expect("send_to a bound socket must succeed");
    });

    let mut drained: Vec<GraphEvent> = Vec::new();
    wait_until(Duration::from_secs(5), || {
        drained.extend(receiver.drain());
        !drained.is_empty()
    });
    assert_eq!(drained.len(), 1, "exactly one event must have arrived");

    assert!(
        ctx.has_requested_repaint(),
        "a repaint requested by the recv thread after a successful delivery \
         must be visible via Context::has_requested_repaint()"
    );

    harness.step();
    let causes = ctx.repaint_causes();
    assert!(
        causes.iter().any(|c| c.file.ends_with("event_stream.rs")),
        "expected repaint_causes() to attribute at least one repaint to \
         event_stream.rs (egui attributes causes by #[track_caller] file/line), \
         got: {causes:?}"
    );

    let _ = std::fs::remove_dir_all(temp_dir("repaint-wake"));
}

/// The live-wiring test: drives the ACTUAL `graph_view::show` render path,
/// with no graph loaded, and proves the drain sits above the early return so
/// an event arriving before the user has loaded anything is still absorbed
/// rather than backing up in the channel. This is the class of coverage
/// whose absence let three gaps ship in Phase 5 (`tests/canvas.rs`'s header).
#[test]
fn graph_view_show_drains_the_event_channel_every_frame() {
    let _guard = SERVE_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    let path = temp_socket_path("live-wiring");
    let socket = event_stream::bind_at(&path).expect("bind_at must succeed for a fresh path");

    let mut harness = Harness::new_ui_state(
        |ui, app: &mut SeamExplorerApp| graph_view::show(ui, app),
        SeamExplorerApp::default(),
    );
    harness.run_steps(1);

    event_stream::serve(socket, harness.ctx.clone());

    let before = event_stream::received_count();

    let send_path = path.clone();
    std::thread::spawn(move || {
        let bytes = seam_core::to_datagram(&GraphEvent::AddNode {
            id: "svc::d".to_string(),
            label: "D".to_string(),
            community: seam_core::CommunityId::from("comm-1"),
        });
        let sender = UnixDatagram::unbound().expect("unbound socket must be constructible");
        sender
            .send_to(&bytes, &send_path)
            .expect("send_to a bound socket must succeed");
    });

    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline && event_stream::received_count() <= before {
        harness.step();
        std::thread::sleep(Duration::from_millis(10));
    }

    assert_eq!(
        event_stream::received_count(),
        before + 1,
        "received_count() must have increased by exactly one after graph_view::show \
         drained the channel on a frame with NO graph loaded"
    );

    let _ = std::fs::remove_dir_all(temp_dir("live-wiring"));
}

/// Two independently-bound receivers on two different paths must never share
/// counters -- the property that makes the rest of this suite parallel-safe.
#[test]
fn spawn_receiver_hands_back_isolated_stats() {
    let path_a = temp_socket_path("isolated-a");
    let path_b = temp_socket_path("isolated-b");

    let socket_a = event_stream::bind_at(&path_a).expect("bind_at must succeed for path a");
    let socket_b = event_stream::bind_at(&path_b).expect("bind_at must succeed for path b");

    let receiver_a = event_stream::spawn_receiver(socket_a, egui::Context::default());
    let receiver_b = event_stream::spawn_receiver(socket_b, egui::Context::default());

    let bytes = seam_core::to_datagram(&GraphEvent::RemoveEdge {
        source: "svc::x".to_string(),
        target: "svc::y".to_string(),
    });
    let sender = UnixDatagram::unbound().expect("unbound socket must be constructible");
    sender
        .send_to(&bytes, &path_a)
        .expect("send_to receiver a must succeed");

    wait_until(Duration::from_secs(5), || {
        !receiver_a.drain().is_empty() || receiver_a.stats().received() > 0
    });
    // The drain above may have already consumed the event; read stats
    // directly rather than draining twice.
    wait_until(Duration::from_secs(5), || {
        receiver_a.stats().received() >= 1
    });

    assert_eq!(
        receiver_a.stats().received(),
        1,
        "receiver a must have received exactly the one datagram sent to path a"
    );
    assert_eq!(
        receiver_b.stats().received(),
        0,
        "receiver b must be completely unaffected by traffic sent to path a"
    );

    let _ = std::fs::remove_dir_all(temp_dir("isolated-a"));
    let _ = std::fs::remove_dir_all(temp_dir("isolated-b"));
}
