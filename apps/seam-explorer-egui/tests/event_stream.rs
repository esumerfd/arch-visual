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
//! Plan 06-03 adds a third wave: hostile input (garbage/oversized/malformed
//! datagrams discarded and counted, the receive loop surviving all of it),
//! concurrent senders (message wholeness under both a paced and an unpaced
//! burst), and the render-cadence-under-flood test proving `graph_view::show`
//! keeps producing frames while a background thread floods the socket.
//!
//! `SERVE_TEST_LOCK` serializes every test that touches the process-global
//! (`event_stream::serve`/`drain`/`received_count`) -- `cargo test`'s default
//! parallelism would otherwise race on it, exactly as plan 05-22 documented
//! for `settings::Store`. Tests using `spawn_receiver` directly need no lock,
//! which is the point of the split (see `spawn_receiver_hands_back_isolated_stats`).

use std::collections::HashSet;
use std::os::unix::net::UnixDatagram;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
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

// ---------------------------------------------------------------------
// Plan 06-03 Task 1: hostile input
// ---------------------------------------------------------------------

/// Deterministic xorshift64 byte generator -- same algorithm
/// `seam-core/tests/event_test.rs::no_panic_over_200_seeded_pseudo_random_slices`
/// already uses, so this real-socket sweep is reproducible across runs.
fn xorshift_bytes(seed: u64, count: usize) -> Vec<u8> {
    let mut state = seed.max(1);
    (0..count)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state & 0xff) as u8
        })
        .collect()
}

/// One test, table-driven over the hostile corpus: a well-formed datagram
/// must arrive before anything hostile is sent (proving the socket is
/// genuinely live), then every hostile payload must be discarded and
/// counted, then a final well-formed datagram must still arrive -- the
/// liveness probe that is this test's actual assertion, not the absence
/// alone.
#[test]
fn hostile_datagrams_are_discarded_and_the_server_keeps_serving() {
    let path = temp_socket_path("hostile-corpus");
    let socket = event_stream::bind_at(&path).expect("bind_at must succeed for a fresh path");
    let receiver = event_stream::spawn_receiver(socket, egui::Context::default());
    let sender = UnixDatagram::unbound().expect("unbound socket must be constructible");

    let send_well_formed = |suffix: &str| {
        let bytes = seam_core::to_datagram(&GraphEvent::AddNode {
            id: format!("svc::liveness-{suffix}"),
            label: "L".to_string(),
            community: "c".to_string(),
        });
        sender
            .send_to(&bytes, &path)
            .expect("send_to a bound socket must succeed");
    };

    // Prove the socket is genuinely live BEFORE anything hostile is sent --
    // a test that "proves" garbage is discarded while the socket is broken
    // proves nothing.
    let before_start = receiver.stats().received();
    send_well_formed("start");
    assert!(
        wait_until(Duration::from_secs(5), || {
            receiver.stats().received() > before_start
        }),
        "the initial well-formed datagram must arrive before the hostile corpus begins"
    );

    let mut corpus: Vec<Vec<u8>> = vec![
        vec![0xff, 0xfe, 0xfd], // not valid UTF-8
        b"{".to_vec(),          // truncated JSON
        b"null".to_vec(),       // valid JSON, wrong kind
        b"[]".to_vec(),
        b"1234".to_vec(),
        br#"{"event_type":"launch_missiles"}"#.to_vec(), // unknown variant
        br#"{"event_type":"add_node","id":"","label":"L","community":"C"}"#.to_vec(), // blank id
        Vec::new(),                                      // zero-length
    ];
    corpus.extend((0..200usize).map(|i| {
        let len = (i * 7) % 300;
        xorshift_bytes(0x1234_5678_9abc_def0u64.wrapping_add(i as u64), len)
    }));

    for (i, payload) in corpus.iter().enumerate() {
        let before_discarded = receiver.stats().discarded();
        sender.send_to(payload, &path).unwrap_or_else(|e| {
            panic!(
                "send_to must accept corpus item {i} ({} bytes): {e}",
                payload.len()
            )
        });
        let discarded = wait_until(Duration::from_secs(5), || {
            receiver.stats().discarded() > before_discarded
        });
        assert!(
            discarded,
            "corpus item {i} ({} bytes) must be discarded and counted, got discarded={}, received={}",
            payload.len(),
            receiver.stats().discarded(),
            receiver.stats().received()
        );
    }

    // The final liveness probe: the receive loop must still be serving
    // after the entire hostile corpus.
    let before_final = receiver.stats().received();
    send_well_formed("end");
    assert!(
        wait_until(Duration::from_secs(5), || {
            receiver.stats().received() > before_final
        }),
        "a well-formed datagram sent after the entire hostile corpus must still arrive"
    );

    let _ = std::fs::remove_dir_all(temp_dir("hostile-corpus"));
}

// ---------------------------------------------------------------------
// Plan 06-03 Task 2: concurrent senders and channel backpressure
// ---------------------------------------------------------------------

/// Canonical serialized form of an event, used as a set-membership key
/// instead of `GraphEvent` itself -- `GraphEvent` deliberately does not
/// derive `Hash` (it is `seam-core`'s wire type, out of this plan's two-file
/// scope), and `to_datagram`'s JSON output is deterministic per event.
fn canonical_key(event: &GraphEvent) -> String {
    String::from_utf8(seam_core::to_datagram(event)).expect("to_datagram produces valid UTF-8")
}

/// Deterministic `(id, label)` corpus for the concurrency tests: `senders` x
/// `per_sender` distinct events, with a VARYING label length per message (up
/// to ~400 bytes, comfortably under the measured `maxdgram`) so a framing
/// bug -- one message's tail bleeding into another's -- cannot hide behind
/// uniform message sizes.
fn concurrency_corpus(senders: usize, per_sender: usize) -> Vec<GraphEvent> {
    const SIZES: [usize; 5] = [8, 64, 150, 300, 400];
    let mut events = Vec::with_capacity(senders * per_sender);
    for sender in 0..senders {
        for index in 0..per_sender {
            let size = SIZES[(sender + index) % SIZES.len()];
            events.push(GraphEvent::AddNode {
                id: format!("s{sender}-m{index}"),
                label: "x".repeat(size),
                community: "concurrency-test".to_string(),
            });
        }
    }
    events
}

/// Spawns one thread per sender, each sending its contiguous slice of
/// `events` to `path` in order, sleeping `pause` between sends (zero =
/// unpaced). `events.len()` must be an exact multiple of `senders`.
fn spawn_senders(
    path: PathBuf,
    events: Vec<GraphEvent>,
    senders: usize,
    pause: Duration,
) -> Vec<std::thread::JoinHandle<()>> {
    let per_sender = events.len() / senders;
    assert_eq!(
        per_sender * senders,
        events.len(),
        "events.len() must be an exact multiple of senders"
    );
    events
        .chunks(per_sender)
        .map(|chunk| {
            let chunk = chunk.to_vec();
            let send_path = path.clone();
            std::thread::spawn(move || {
                let sender = UnixDatagram::unbound().expect("unbound socket must be constructible");
                for event in chunk {
                    let bytes = seam_core::to_datagram(&event);
                    let _ = sender.send_to(&bytes, &send_path);
                    if !pause.is_zero() {
                        std::thread::sleep(pause);
                    }
                }
            })
        })
        .collect()
}

/// The SC-2 test: 4 sender threads x 25 datagrams each, overlapping in time
/// but paced (1ms between sends per sender) so the burst stays inside the
/// measured 4096-byte kernel receive buffer and full delivery can be
/// asserted without flaking. Set equality against the sent corpus carries
/// the whole assertion: a truncated message is not in the set, a merged
/// message is not in the set, a reordered one still is.
#[test]
fn concurrent_senders_each_deliver_every_message_whole() {
    let path = temp_socket_path("concurrent-paced");
    let socket = event_stream::bind_at(&path).expect("bind_at must succeed for a fresh path");
    let receiver = event_stream::spawn_receiver(socket, egui::Context::default());

    let senders = 4;
    let per_sender = 25;
    let events = concurrency_corpus(senders, per_sender);
    let expected: HashSet<String> = events.iter().map(canonical_key).collect();
    assert_eq!(
        expected.len(),
        events.len(),
        "fixture must construct 100 distinct events"
    );

    let handles = spawn_senders(path.clone(), events, senders, Duration::from_millis(1));

    let mut drained: Vec<GraphEvent> = Vec::new();
    wait_until(Duration::from_secs(10), || {
        drained.extend(receiver.drain());
        drained.len() >= senders * per_sender
    });
    for handle in handles {
        handle.join().expect("sender thread must not panic");
    }
    drained.extend(receiver.drain());

    let received: HashSet<String> = drained.iter().map(canonical_key).collect();
    assert_eq!(
        received.len(),
        drained.len(),
        "no duplicates: every delivered event must be distinct, got {} events, {} distinct",
        drained.len(),
        received.len()
    );
    assert_eq!(
        received, expected,
        "the delivered set must equal the sent set exactly -- no truncation, merging, or loss"
    );

    let _ = std::fs::remove_dir_all(temp_dir("concurrent-paced"));
}

/// The honest-loss test: 4 threads x 100 datagrams each, entirely unpaced.
/// Only the guarantees that hold regardless of load are asserted (no
/// corruption, no duplicates, a floor on delivery) -- full delivery is
/// deliberately NOT asserted, since datagram loss under an unpaced burst
/// against a 4096-byte kernel receive buffer is the trade-off
/// research/PITFALLS.md Pitfall 7 says to accept knowingly.
#[test]
fn an_unpaced_burst_never_corrupts_what_it_does_deliver() {
    let path = temp_socket_path("concurrent-unpaced");
    let socket = event_stream::bind_at(&path).expect("bind_at must succeed for a fresh path");
    let receiver = event_stream::spawn_receiver(socket, egui::Context::default());

    let senders = 4;
    let per_sender = 100;
    let events = concurrency_corpus(senders, per_sender);
    let expected: HashSet<String> = events.iter().map(canonical_key).collect();
    assert_eq!(expected.len(), events.len());

    let handles = spawn_senders(path.clone(), events, senders, Duration::ZERO);
    for handle in handles {
        let _ = handle.join();
    }

    // Give the receive thread a moment to drain everything the kernel
    // actually delivered before reading final stats -- this is a floor
    // assertion, not a race-sensitive exact-count one.
    std::thread::sleep(Duration::from_millis(500));
    let drained = receiver.drain();
    let received: HashSet<String> = drained.iter().map(canonical_key).collect();

    assert_eq!(
        received.len(),
        drained.len(),
        "no duplicates, even under an unpaced burst"
    );
    assert!(
        received.is_subset(&expected),
        "every delivered event must be one that was actually sent -- no corruption"
    );

    let sent = senders * per_sender;
    let delivered = received.len();
    eprintln!(
        "[an_unpaced_burst_never_corrupts_what_it_does_deliver] delivered {delivered}/{sent} \
         ({:.1}%) under an unpaced burst -- measured net.local.dgram.maxdgram=2048, \
         net.local.dgram.recvspace=4096",
        (delivered as f64 / sent as f64) * 100.0
    );
    assert!(
        delivered * 2 >= sent,
        "expected at least half of {sent} unpaced messages to survive the kernel receive \
         buffer, got {delivered} ({:.1}%)",
        (delivered as f64 / sent as f64) * 100.0
    );

    // Liveness probe: the receive loop must still be serving after the burst.
    let before = receiver.stats().received();
    let bytes = seam_core::to_datagram(&GraphEvent::AddNode {
        id: "svc::liveness-after-burst".to_string(),
        label: "L".to_string(),
        community: "c".to_string(),
    });
    let sender = UnixDatagram::unbound().expect("unbound socket must be constructible");
    sender.send_to(&bytes, &path).expect("send_to must succeed");
    assert!(
        wait_until(Duration::from_secs(5), || {
            receiver.stats().received() > before
        }),
        "a well-formed datagram after the unpaced burst must still arrive"
    );

    let _ = std::fs::remove_dir_all(temp_dir("concurrent-unpaced"));
}

/// The integration counterpart to `handle_datagram`'s unit-tested
/// backpressure branch: oversubscribe the channel by 64 messages without
/// draining once, then prove the channel bound is real (drain() returns no
/// more than `CHANNEL_CAPACITY`) and that the receive loop is not wedged by
/// having left the channel full.
#[test]
fn a_saturated_channel_does_not_wedge_the_receive_loop() {
    let path = temp_socket_path("channel-saturation");
    let socket = event_stream::bind_at(&path).expect("bind_at must succeed for a fresh path");
    let receiver = event_stream::spawn_receiver(socket, egui::Context::default());
    let sender = UnixDatagram::unbound().expect("unbound socket must be constructible");

    let total = event_stream::CHANNEL_CAPACITY + 64;
    for i in 0..total {
        let bytes = seam_core::to_datagram(&GraphEvent::RemoveNode {
            id: format!("svc::sat-{i}"),
        });
        // Best-effort: this test is about the CHANNEL bound, not about
        // every one of these arriving at the kernel socket.
        let _ = sender.send_to(&bytes, &path);
    }

    // Give the receive thread time to pull everything off the kernel socket
    // and pile it up behind the (undrained) channel.
    std::thread::sleep(Duration::from_millis(500));

    let drained = receiver.drain();
    assert!(
        drained.len() <= event_stream::CHANNEL_CAPACITY,
        "drain() must never return more than CHANNEL_CAPACITY events, got {}",
        drained.len()
    );

    // The real requirement: a full, undrained channel must not wedge the
    // receive loop.
    let before = receiver.stats().received();
    let probe = seam_core::to_datagram(&GraphEvent::RemoveNode {
        id: "svc::after-saturation".to_string(),
    });
    sender.send_to(&probe, &path).expect("send_to must succeed");
    assert!(
        wait_until(Duration::from_secs(5), || {
            receiver.drain();
            receiver.stats().received() > before
        }),
        "a datagram sent after channel saturation must still arrive"
    );

    let _ = std::fs::remove_dir_all(temp_dir("channel-saturation"));
}

// ---------------------------------------------------------------------
// Plan 06-03 Task 3: render cadence under flood
// ---------------------------------------------------------------------

const CLEAN_FIXTURE: &str = include_str!("../../seam-core/tests/fixtures/clean.json");

/// Copied from `tests/canvas.rs::build_test_app` -- Cargo integration test
/// binaries cannot share helpers without a `tests/common/mod.rs`, not worth
/// adding for eight lines. Builds a real, small graph so `graph_view::show`
/// renders an actual `GraphView` widget rather than the "no graph loaded"
/// placeholder.
fn build_test_app() -> SeamExplorerApp {
    let outcome = seam_explorer_egui::load::read_and_ingest(CLEAN_FIXTURE)
        .expect("fixture must ingest cleanly");
    SeamExplorerApp {
        model: Some(outcome.model),
        seams: outcome.seams,
        ..Default::default()
    }
}

/// EVENT-03 / ROADMAP SC-5: the real `graph_view::show` render path must
/// keep producing frames while datagrams stream in continuously. Three
/// assertions, in order of importance: (1) all 120 steps complete at all --
/// if a socket read ever reached the UI thread the harness would block and
/// this test would never return; (2) `received_count()` increased during
/// the timed window, so this test cannot pass having measured an idle
/// application; (3) no single step reached the 500ms stall threshold.
#[test]
fn the_render_loop_keeps_its_cadence_while_datagrams_stream_in() {
    let _guard = SERVE_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    let path = temp_socket_path("render-cadence");
    let socket = event_stream::bind_at(&path).expect("bind_at must succeed for a fresh path");

    let app = build_test_app();
    let mut harness = Harness::new_ui_state(
        |ui, app: &mut SeamExplorerApp| graph_view::show(ui, app),
        app,
    );
    harness.run_steps(3);

    event_stream::serve(socket, harness.ctx.clone());

    let baseline = event_stream::received_count();

    let stop = Arc::new(AtomicBool::new(false));
    let flood_stop = stop.clone();
    let flood_path = path.clone();
    let flood = std::thread::spawn(move || {
        let sender = UnixDatagram::unbound().expect("unbound socket must be constructible");
        let mut counter: u64 = 0;
        while !flood_stop.load(Ordering::Relaxed) {
            let bytes = seam_core::to_datagram(&GraphEvent::AddNode {
                id: format!("svc::flood-{counter}"),
                label: "L".to_string(),
                community: "c".to_string(),
            });
            let _ = sender.send_to(&bytes, &flood_path);
            counter += 1;
            std::thread::sleep(Duration::from_micros(200));
        }
    });

    let mut durations: Vec<Duration> = Vec::with_capacity(120);
    for _ in 0..120 {
        let start = Instant::now();
        harness.step();
        durations.push(start.elapsed());
    }

    stop.store(true, Ordering::Relaxed);
    flood.join().expect("flood thread must not panic");

    let delta = event_stream::received_count() - baseline;
    let max = durations.iter().max().copied().unwrap_or_default();
    let total: Duration = durations.iter().sum();
    let mean = total / durations.len() as u32;
    let mut sorted = durations.clone();
    sorted.sort();
    let median = sorted[sorted.len() / 2];

    eprintln!(
        "[the_render_loop_keeps_its_cadence_while_datagrams_stream_in] 120 steps, \
         received_count delta={delta}, max={max:?}, median={median:?}, mean={mean:?}"
    );

    assert_eq!(durations.len(), 120, "all 120 steps must have completed");
    assert!(
        delta > 0,
        "received_count() must have increased during the timed window -- got a delta of \
         {delta}, which would mean this test measured an idle application"
    );
    assert!(
        max < Duration::from_millis(500),
        "the slowest single step took {max:?}, at or over the 500ms stall threshold"
    );

    let _ = std::fs::remove_dir_all(temp_dir("render-cadence"));
}
