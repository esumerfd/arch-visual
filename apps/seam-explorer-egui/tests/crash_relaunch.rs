//! ROADMAP SC-1, end to end and headless (plan 10-01).
//!
//! Force-quit a receiving `seam-explorer-egui` mid-stream while a hook client
//! keeps firing, relaunch it, and get events again with no manual cleanup.
//!
//! **Why this is possible without a display.** `main.rs` binds the event
//! socket entirely BEFORE `eframe::run_native` and outside the creation
//! closure (main.rs:42-51), so the whole bind/crash/relaunch lifecycle is
//! exercisable in a plain `cargo test` process. Nothing here needs a window.
//! `the_relaunched_app_receives_applies_and_records_a_real_client_event`'s
//! structural gate keeps that ordering checkable.
//!
//! **Why a REAL child process, and not an in-process `EventReceiver` drop.**
//! The cheaper-looking route -- bind, `spawn_receiver`, then drop the
//! `EventReceiver` handle to "simulate" a crash -- cannot work. In
//! `event_stream.rs:400-411` the receive thread's `deliver` closure matches on
//! `tx.try_send(event)` and maps EVERY `Err(_)` -- including the
//! `TrySendError::Disconnected` that dropping the receiver produces -- to
//! `Delivery::ChannelFull`, a counted, NON-FATAL outcome. So the recv loop
//! carries on after the handle is gone: the socket stays bound, the thread
//! stays alive, and nothing about the process resembles a crashed one. An
//! in-process drop simulates a full channel, not a `kill -9`. Only a separate
//! OS process that can actually be SIGKILLed produces the state SC-1 is about
//! -- a socket inode surviving with no owning process -- which is why
//! `examples/headless_receiver.rs` exists at all.
//!
//! **What is new here.** No prior test combines BOTH processes' lifecycles.
//! 06-02's `a_socket_left_behind_by_a_crash_does_not_block_the_next_launch`
//! drops a socket that was never serving; 07-04's `seam_client_bridge.rs` runs
//! a real client against a receiver that never dies. This file kills a
//! receiver that was genuinely RECEIVING, with a real client firing on both
//! sides of the outage.
//!
//! Additive by construction: `tests/event_stream.rs` is Phase 6's regression
//! net and is not touched.

mod common;

use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::Duration;

use common::{
    run_client, temp_config_home, wait_until, write_payload, HOOK_FILE_PATH, HOOK_REPO_RELATIVE,
};

/// Generous: this waits on a real process spawn plus a real socket hop, and it
/// only ever runs to completion when something has genuinely gone wrong.
const ARRIVAL_DEADLINE: Duration = Duration::from_secs(10);

/// How long to wait for the spawned example to finish binding and create its
/// log file. Same reasoning as `ARRIVAL_DEADLINE`.
const READY_DEADLINE: Duration = Duration::from_secs(10);

// ---------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------

/// A new-file `Write` body declaring exactly one `pub fn`.
///
/// Deliberately the same shape `seam_client_bridge.rs` already proved yields
/// exactly ONE `AddNode` and nothing else, so a line count in the receiver's
/// log is an unambiguous event count.
fn declares(symbol: &str) -> String {
    format!("//! A tiny module.\n\npub fn {symbol}(bytes: &[u8]) -> u32 {{\n    bytes.len() as u32\n}}\n")
}

/// The node id the client will advertise for `declares(symbol)`, derived from
/// what this test itself sends rather than read back out of the thing under
/// test. `timeline_reconstruction.rs`'s expectation discipline: an expectation
/// read out of the system under test cannot fail.
fn expected_node_id(symbol: &str) -> String {
    format!("{HOOK_REPO_RELATIVE}::{symbol}")
}

fn assert_sun_path_fits(path: &Path) {
    let len = path.as_os_str().as_bytes().len();
    assert!(
        len <= seam_core::MAX_SUN_PATH_BYTES,
        "socket path is {len} bytes, over the {} byte sun_path ceiling: {}",
        seam_core::MAX_SUN_PATH_BYTES,
        path.display()
    );
}

/// Locates the `headless_receiver` example, BUILDING it if it is not there
/// yet -- the same discipline (and for the same reason) as
/// `common::client_binary`.
///
/// Skipping is NOT an option: a test that quietly skips when it cannot find
/// its fixture reports green while proving nothing (T-10-01-04, inherited from
/// T-07-04-06), so every failure path below panics with the exact path it
/// looked at.
fn example_binary() -> PathBuf {
    let test_exe = std::env::current_exe().expect("the running test binary must have a path");
    // .../target/<profile>/deps/crash_relaunch-<hash>
    let profile_dir = test_exe
        .parent()
        .and_then(Path::parent)
        .unwrap_or_else(|| {
            panic!(
                "expected the test binary at target/<profile>/deps/..., got {}",
                test_exe.display()
            )
        })
        .to_path_buf();
    let candidate = profile_dir.join("examples").join("headless_receiver");
    if candidate.is_file() {
        return candidate;
    }

    let workspace_manifest = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("Cargo.toml");
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let mut build = Command::new(&cargo);
    build
        .arg("build")
        .arg("--example")
        .arg("headless_receiver")
        .arg("-p")
        .arg("seam-explorer-egui")
        .arg("--manifest-path")
        .arg(&workspace_manifest);
    if profile_dir
        .file_name()
        .is_some_and(|name| name == "release")
    {
        build.arg("--release");
    }
    let status = build.status().unwrap_or_else(|e| {
        panic!("could not run `{cargo} build --example headless_receiver`: {e}");
    });
    assert!(
        status.success(),
        "`{cargo} build --example headless_receiver` failed with {status:?}; expected the binary \
         at {}",
        candidate.display()
    );
    assert!(
        candidate.is_file(),
        "built headless_receiver but no binary is at {} -- this test cannot be skipped, so the \
         path resolution above is what needs fixing",
        candidate.display()
    );
    candidate
}

/// A spawned `headless_receiver` whose `Drop` kills and REAPS it.
///
/// A guard rather than a kill call at the end of each test on purpose
/// (T-10-01-03): an assertion that fails unwinds past any trailing cleanup,
/// and a surviving child holds the socket and poisons every later run against
/// the same path.
struct ReceiverProcess {
    child: Child,
    reaped: bool,
}

impl ReceiverProcess {
    /// Spawns the example and waits for it to signal readiness by creating its
    /// log file -- which it does only AFTER a successful `bind_at`, so no
    /// stdout parsing is needed and a bind failure cannot look like readiness.
    fn spawn(socket_path: &Path, log_path: &Path) -> Self {
        let binary = example_binary();
        let child = Command::new(&binary)
            .arg(socket_path)
            .arg(log_path)
            .spawn()
            .unwrap_or_else(|e| panic!("spawn the example at {}: {e}", binary.display()));
        let receiver = ReceiverProcess {
            child,
            reaped: false,
        };
        assert!(
            wait_until(READY_DEADLINE, || log_path.exists()),
            "the receiving process never signalled readiness by creating {}",
            log_path.display()
        );
        receiver
    }
}

impl Drop for ReceiverProcess {
    fn drop(&mut self) {
        if !self.reaped {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

fn log_lines(path: &Path) -> Vec<String> {
    match std::fs::read_to_string(path) {
        Ok(text) => text.lines().map(str::to_string).collect(),
        Err(_) => Vec::new(),
    }
}

// ---------------------------------------------------------------------
// Task 1 -- the tracer: one real client, one separate receiving process
// ---------------------------------------------------------------------

/// The thinnest path that touches every layer SC-1 exercises: a real
/// `seam-client` binary, a real `AF_UNIX`/`SOCK_DGRAM` socket, and a real
/// SEPARATE receiving process that records what it got. No crash yet.
#[test]
fn a_real_client_reaches_a_separate_receiving_process() {
    let config_home = temp_config_home("t1");
    let socket_path = seam_core::socket_path_from(config_home.to_str(), None)
        .expect("the socket path must resolve from an explicit base");
    assert_sun_path_fits(&socket_path);
    let log_path = config_home.join("events.log");

    let _receiver = ReceiverProcess::spawn(&socket_path, &log_path);

    let symbol = "verify_token";
    run_client(
        &config_home,
        &write_payload(HOOK_FILE_PATH, &declares(symbol)),
    );

    assert!(
        wait_until(ARRIVAL_DEADLINE, || log_lines(&log_path).len() == 1),
        "the real client's advertisement must reach the separate receiving process; the log at {} \
         holds {:?}",
        log_path.display(),
        log_lines(&log_path)
    );

    let lines = log_lines(&log_path);
    assert_eq!(lines.len(), 1, "exactly one event, got {lines:?}");
    let expected = expected_node_id(symbol);
    assert!(
        lines[0].contains(&expected),
        "the recorded event must be the client's advertisement of `{expected}`, got {:?}",
        lines[0]
    );

    let _ = std::fs::remove_dir_all(&config_home);
}
