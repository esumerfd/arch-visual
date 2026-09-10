//! The two processes meet (plan 07-04, Task 2).
//!
//! Everything else in Phase 7 tests one side of the pipe. This file is the
//! only test in the workspace that runs BOTH: the real built `seam-client`
//! binary as a real child process on one end, and this app's own real
//! `event_stream::bind_at` + `spawn_receiver` receive loop on the other, over
//! a real `AF_UNIX`/`SOCK_DGRAM` socket at the path `seam_core` resolves for
//! both of them independently.
//!
//! **What this proves:** an ordinary edit, shaped exactly as the live capture
//! showed Claude Code sends it, reaches the app's receive loop and is
//! ACCEPTED -- `Stats::received` moves and `Stats::discarded` does not -- and
//! the event that comes off the drain is intact, not merely counted.
//!
//! **What this deliberately does NOT prove:** that anything appears on the
//! canvas. Applying an event to the rendered graph is Phase 8's work, and
//! there is deliberately nothing here that does it. This phase's boundary is
//! the receive loop, and that is exactly where this test stops.
//!
//! Additive by construction: `tests/event_stream.rs` is Phase 6's regression
//! net and is not touched. Like that file's `spawn_receiver` tests, nothing
//! here reaches the process-global, so no lock is needed.
//!
//! **Plan 10-01 migration.** The harness this file used to own -- locating and
//! building the cross-crate client binary, running it, and building a payload
//! of the live-captured shape -- now lives in `tests/common/mod.rs`, the single
//! authority `crash_relaunch.rs` also uses. Nothing about what these two tests
//! assert changed; the helpers moved so a third copy of `client_binary()`
//! cannot appear. `write_payload` gained an explicit file-path argument (this
//! phase needs several distinct edited files) and both call sites below stay
//! pointed at `src/lib.rs`, which is what keeps the `src/lib.rs::parse_datagram`
//! assertion below meaningful.

mod common;

use std::time::Duration;

use seam_core::GraphEvent;
use seam_explorer_egui::event_stream;

use common::{run_client, temp_config_home, wait_until, write_payload};

/// The edited file both tests below declare. Deliberately NOT
/// `common::HOOK_FILE_PATH`: this file's assertion is about
/// `src/lib.rs::parse_datagram`, and pointing it at the phase-wide constant
/// would silently change what it proves.
const BRIDGE_FILE_PATH: &str = "/private/tmp/hook-capture-test/src/lib.rs";

/// Generous: this waits on a real process spawn plus a real socket hop, and
/// it only ever runs to completion when something has genuinely gone wrong.
const ARRIVAL_DEADLINE: Duration = Duration::from_secs(10);

/// How long the negative case waits before concluding nothing is coming.
/// Long enough that a slow-but-real delivery would still be caught.
const SILENCE_WINDOW: Duration = Duration::from_secs(2);

#[test]
fn a_real_client_subprocess_drives_the_real_receive_loop_counter() {
    let config_home = temp_config_home("bridge-positive");
    let socket_path = seam_core::socket_path_from(config_home.to_str(), None)
        .expect("the socket path must resolve from an explicit base");

    let socket =
        event_stream::bind_at(&socket_path).expect("bind_at must succeed for a fresh path");
    let receiver = event_stream::spawn_receiver(socket, egui::Context::default());

    let before_received = receiver.stats().received();
    let before_discarded = receiver.stats().discarded();

    run_client(
        &config_home,
        &write_payload(
            BRIDGE_FILE_PATH,
            "//! A tiny module.\n\npub fn parse_datagram(bytes: &[u8]) -> u32 {\n    bytes.len() as u32\n}\n",
        ),
    );

    assert!(
        wait_until(ARRIVAL_DEADLINE, || {
            receiver.stats().received() == before_received + 1
        }),
        "the real client's advertisement must reach the real receive loop: received went {} -> {}, \
         discarded {} -> {}",
        before_received,
        receiver.stats().received(),
        before_discarded,
        receiver.stats().discarded()
    );

    // A counter-only assertion would pass just as happily against an event
    // that arrived and was thrown away.
    assert_eq!(
        receiver.stats().discarded(),
        before_discarded,
        "the event must be ACCEPTED, not received-and-discarded"
    );

    // ...and the message must have survived the whole path intact, not merely
    // moved a number.
    let events = receiver.drain();
    assert_eq!(events.len(), 1, "exactly one event, got {events:?}");
    match &events[0] {
        GraphEvent::AddNode {
            id,
            label,
            community,
            source_file,
        } => {
            assert_eq!(
                id, "src/lib.rs::parse_datagram",
                "DP-07-01's identity shape"
            );
            assert_eq!(label, "parse_datagram", "the bare symbol name");
            assert_eq!(
                *community, None,
                "D-03: the client advertises, it never resolves a community -- resolving this is \
                 Phase 8's job and nothing here does it"
            );
            assert_eq!(
                *source_file,
                Some("src/lib.rs".to_string()),
                "Pitfall A: repo-relative, never the absolute path the hook handed over"
            );
        }
        other => panic!("expected an AddNode advertisement, got {other:?}"),
    }

    let _ = std::fs::remove_dir_all(&config_home);
}

#[test]
fn a_structurally_meaningless_edit_moves_no_counter_at_all() {
    // The negative half of the phase goal, and the half a
    // counter-increments-only test would happily let regress: the pipeline
    // must be quiet when there is nothing to say, not merely loud when there
    // is.
    let config_home = temp_config_home("bridge-negative");
    let socket_path = seam_core::socket_path_from(config_home.to_str(), None)
        .expect("the socket path must resolve from an explicit base");

    let socket =
        event_stream::bind_at(&socket_path).expect("bind_at must succeed for a fresh path");
    let receiver = event_stream::spawn_receiver(socket, egui::Context::default());

    let before_received = receiver.stats().received();
    let before_discarded = receiver.stats().discarded();

    run_client(
        &config_home,
        &write_payload(
            BRIDGE_FILE_PATH,
            "//! A tiny module.\n\n// just a note about what will go here one day\n",
        ),
    );

    let moved = wait_until(SILENCE_WINDOW, || {
        receiver.stats().received() != before_received
            || receiver.stats().discarded() != before_discarded
    });
    assert!(
        !moved,
        "a comment-only edit must advertise nothing: received {} -> {}, discarded {} -> {}",
        before_received,
        receiver.stats().received(),
        before_discarded,
        receiver.stats().discarded()
    );
    assert!(
        receiver.drain().is_empty(),
        "nothing may be queued for the UI thread either"
    );

    let _ = std::fs::remove_dir_all(&config_home);
}
