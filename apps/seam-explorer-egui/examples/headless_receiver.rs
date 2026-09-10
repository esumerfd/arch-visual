//! A dev-only, window-free receiving process (plan 10-01).
//!
//! **Why this exists.** ROADMAP SC-1 is about a receiver that is force-quit
//! mid-stream: a socket inode surviving with no owning process. The
//! cheaper-looking way to produce that state from inside a test -- bind,
//! `spawn_receiver`, then drop the `EventReceiver` handle -- does not produce
//! it at all. In `event_stream.rs` the recv thread's `deliver` closure maps
//! EVERY `tx.try_send` error, `TrySendError::Disconnected` included, to
//! `Delivery::ChannelFull`, which is a counted, NON-FATAL outcome. The loop
//! carries on: the socket stays bound and the thread stays alive. An
//! in-process drop therefore simulates a full channel, not a `kill -9`. Only a
//! separate OS process can actually be SIGKILLed, so `crash_relaunch.rs`
//! spawns this.
//!
//! **This is a HARNESS for the real lifecycle, not a second implementation of
//! it.** It reaches the same production functions `main.rs` reaches -- bind
//! first, then serve -- and calls each exactly once. `crash_relaunch.rs` keeps
//! that correspondence checkable with a structural gate over `main.rs`'s
//! bind-before-window ordering.
//!
//! Usage: `headless_receiver <socket-path> <log-path>`.
//!
//! It appends one line per drained event to the log and NEVER exits on its
//! own. Killing it is the point.

use std::io::Write;

use seam_explorer_egui::event_stream;

/// How long to nap between drains. Short enough that a test polling the log
/// is not waiting on this, long enough not to spin a core.
const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(5);

fn main() {
    let mut args = std::env::args().skip(1);
    let socket_path = match args.next() {
        Some(path) => std::path::PathBuf::from(path),
        None => {
            eprintln!("usage: headless_receiver <socket-path> <log-path>");
            std::process::exit(2);
        }
    };
    let log_path = match args.next() {
        Some(path) => std::path::PathBuf::from(path),
        None => {
            eprintln!("usage: headless_receiver <socket-path> <log-path>");
            std::process::exit(2);
        }
    };

    // A clean, non-zero failure rather than a panic: the test asserts on this
    // path, and a panic's stderr would be noise around the one fact that
    // matters.
    let socket = match event_stream::bind_at(&socket_path) {
        Ok(socket) => socket,
        Err(e) => {
            eprintln!(
                "headless_receiver: could not bind {}: {e}",
                socket_path.display()
            );
            std::process::exit(3);
        }
    };

    // The readiness signal, and it is created strictly AFTER a successful
    // bind so a bind failure can never look like readiness. Its EXISTENCE is
    // what the test polls for -- no stdout parsing, no line protocol.
    let mut log = match std::fs::File::create(&log_path) {
        Ok(file) => file,
        Err(e) => {
            eprintln!(
                "headless_receiver: could not create {}: {e}",
                log_path.display()
            );
            std::process::exit(4);
        }
    };

    // `Context::default()` needs no display. The recv thread only ever calls
    // `request_repaint` on it, which is a no-op with no window attached.
    let receiver = event_stream::spawn_receiver(socket, egui::Context::default());

    loop {
        for event in receiver.drain() {
            let bytes = seam_core::to_datagram(&event);
            if log.write_all(&bytes).is_err() || log.write_all(b"\n").is_err() {
                eprintln!("headless_receiver: could not append to the log");
                std::process::exit(5);
            }
            let _ = log.flush();
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}
