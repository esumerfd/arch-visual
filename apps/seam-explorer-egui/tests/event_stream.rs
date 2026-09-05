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
use std::path::{Path, PathBuf};
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
// ---------------------------------------------------------------------
// Task 2: bind lifecycle
// ---------------------------------------------------------------------

/// Builds a path whose byte length exceeds `MAX_SUN_PATH_BYTES`, asserting
/// the constructed length actually exceeds the limit so the fixture cannot
/// silently stop testing anything.
fn over_limit_path() -> PathBuf {
    let mut dir = temp_dir("over-limit");
    while dir.as_os_str().len() < event_stream::MAX_SUN_PATH_BYTES {
        dir = dir.join("a-directory-segment-of-real-length");
    }
    let path = dir.join("seam.sock");
    assert!(
        path.as_os_str().len() > event_stream::MAX_SUN_PATH_BYTES,
        "fixture must actually construct a path over the limit, got {} bytes",
        path.as_os_str().len()
    );
    path
}

#[test]
fn a_socket_path_over_the_sun_path_limit_is_rejected_before_bind() {
    let path = over_limit_path();
    let result = event_stream::bind_at(&path);
    match result {
        Err(event_stream::BindError::PathTooLong { len, .. }) => {
            assert_eq!(
                len,
                path.as_os_str().len(),
                "reported len must equal the real byte length of the path"
            );
        }
        other => panic!("expected BindError::PathTooLong, got {other:?}"),
    }
    assert!(
        !path.exists(),
        "no file must be created at a path rejected before bind"
    );
}

#[test]
fn the_config_directory_is_created_when_absent() {
    let dir = temp_dir("dir-creation").join("nested").join("deeper");
    let path = dir.join("seam.sock");
    assert!(
        !dir.exists(),
        "fixture precondition: the parent directory must not exist yet"
    );

    event_stream::bind_at(&path).expect("bind_at must create the missing parent and succeed");

    assert!(
        dir.exists(),
        "bind_at must have created the parent directory (D-02)"
    );

    let _ = std::fs::remove_dir_all(temp_dir("dir-creation"));
}

/// Models the `kill -9` case: the bound socket is dropped WITHOUT unlinking
/// the file first, so the inode is left behind exactly as a crash would
/// leave it.
#[test]
fn a_socket_left_behind_by_a_crash_does_not_block_the_next_launch() {
    let path = temp_socket_path("stale-recovery");
    let socket = event_stream::bind_at(&path).expect("first bind_at must succeed");
    std::mem::drop(socket);

    assert!(
        path.exists(),
        "fixture must reproduce the crash residue: the file must still exist after drop"
    );

    let second = event_stream::bind_at(&path)
        .expect("bind_at must detect the dead inode, remove it, and rebind");

    let receiver = event_stream::spawn_receiver(second, egui::Context::default());
    let bytes = seam_core::to_datagram(&GraphEvent::RemoveNode {
        id: "svc::stale".to_string(),
    });
    let sender = UnixDatagram::unbound().expect("unbound socket must be constructible");
    sender
        .send_to(&bytes, &path)
        .expect("send_to the rebound socket must succeed");

    let mut drained: Vec<GraphEvent> = Vec::new();
    wait_until(Duration::from_secs(5), || {
        drained.extend(receiver.drain());
        !drained.is_empty()
    });
    assert_eq!(
        drained.len(),
        1,
        "events must still flow through the rebound socket after stale-inode recovery"
    );

    let _ = std::fs::remove_dir_all(temp_dir("stale-recovery"));
}

/// The Pitfall 3 / D-03 test: a genuinely live peer must be reported as a
/// conflict, and MUST NOT be deleted out from under it.
#[test]
fn a_live_peer_is_never_deleted_and_reports_a_conflict() {
    let path = temp_socket_path("live-conflict");
    let first = event_stream::bind_at(&path).expect("first bind_at must succeed");

    let result = event_stream::bind_at(&path);
    match result {
        Err(event_stream::BindError::AlreadyRunning { path: reported }) => {
            assert_eq!(
                reported, path,
                "the reported path must be the real socket path"
            );
        }
        other => panic!("expected BindError::AlreadyRunning, got {other:?}"),
    }

    // The dangerous failure mode is not a wrong error code -- it is the
    // second instance silently unlinking and stealing the first instance's
    // live socket. Prove the FIRST socket still works.
    let receiver = event_stream::spawn_receiver(first, egui::Context::default());
    let bytes = seam_core::to_datagram(&GraphEvent::AddEdge {
        source: "svc::e".to_string(),
        target: "svc::f".to_string(),
    });
    let sender = UnixDatagram::unbound().expect("unbound socket must be constructible");
    sender
        .send_to(&bytes, &path)
        .expect("send_to the original, still-live socket must succeed");

    let mut drained: Vec<GraphEvent> = Vec::new();
    wait_until(Duration::from_secs(5), || {
        drained.extend(receiver.drain());
        !drained.is_empty()
    });
    assert_eq!(
        drained.len(),
        1,
        "the original instance's socket must still be delivering events after a refused conflict"
    );

    let _ = std::fs::remove_dir_all(temp_dir("live-conflict"));
}

#[test]
fn the_bound_socket_is_owner_only() {
    use std::os::unix::fs::PermissionsExt;

    let path = temp_socket_path("permissions");
    let _socket = event_stream::bind_at(&path).expect("bind_at must succeed");

    let mode = std::fs::metadata(&path)
        .expect("bound socket file must exist")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(
        mode, 0o600,
        "the bound socket file must be mode 0600, got {mode:o}"
    );

    let _ = std::fs::remove_dir_all(temp_dir("permissions"));
}

#[test]
fn the_already_running_message_names_the_path_and_the_action() {
    let path = Path::new("/Users/example/.config/seam-explorer/seam.sock");
    let message = event_stream::already_running_message(path);

    assert!(
        message.contains(
            path.to_str()
                .expect("path must be valid UTF-8 in this test")
        ),
        "message must contain the full socket path, got: {message}"
    );
    assert!(
        message.to_lowercase().contains("already running")
            || message.to_lowercase().contains("running"),
        "message must state that another instance is running, got: {message}"
    );
    assert!(
        message.to_lowercase().contains("exit"),
        "message must state that this instance is exiting, got: {message}"
    );
}
