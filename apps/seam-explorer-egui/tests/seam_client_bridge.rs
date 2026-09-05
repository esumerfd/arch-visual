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

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use seam_core::GraphEvent;
use seam_explorer_egui::event_stream;

/// Generous: this waits on a real process spawn plus a real socket hop, and
/// it only ever runs to completion when something has genuinely gone wrong.
const ARRIVAL_DEADLINE: Duration = Duration::from_secs(10);

/// How long the negative case waits before concluding nothing is coming.
/// Long enough that a slow-but-real delivery would still be caught.
const SILENCE_WINDOW: Duration = Duration::from_secs(2);

fn temp_config_home(unique: &str) -> PathBuf {
    std::env::temp_dir().join(format!("scb-{}-{}", std::process::id(), unique))
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

/// Locates the `seam-client` binary, BUILDING it if it is not there yet.
///
/// The convenient `CARGO_BIN_EXE_*` macro only exists for binaries in the
/// same package, and this test lives in a different one. Resolving it instead
/// from this test executable's own location keeps the profile correct for
/// free: a `--release` test run finds the release client, a debug run finds
/// the debug one.
///
/// Building when absent is what turns "07-04's Task 2 happens to run after
/// something built the client" from an ordering assumption into a guarantee.
/// It costs nothing when the binary is already current.
///
/// Skipping is NOT an option here. A test that quietly skips when it cannot
/// find its fixture reports green while proving nothing, which is strictly
/// worse than having no test at all (T-07-04-06) -- so every failure path
/// below panics with the exact path it looked at.
fn client_binary() -> PathBuf {
    let test_exe = std::env::current_exe().expect("the running test binary must have a path");
    // .../target/<profile>/deps/seam_client_bridge-<hash>
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
    let candidate = profile_dir.join("seam-client");
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
        .arg("-p")
        .arg("seam-client")
        .arg("--manifest-path")
        .arg(&workspace_manifest);
    if profile_dir
        .file_name()
        .is_some_and(|name| name == "release")
    {
        build.arg("--release");
    }
    let status = build.status().unwrap_or_else(|e| {
        panic!("could not run `{cargo} build -p seam-client`: {e}");
    });
    assert!(
        status.success(),
        "`{cargo} build -p seam-client` failed with {status:?}; expected the binary at {}",
        candidate.display()
    );
    assert!(
        candidate.is_file(),
        "built seam-client but no binary is at {} -- this test cannot be skipped, so the path \
         resolution above is what needs fixing",
        candidate.display()
    );
    candidate
}

/// Runs the real client with `config_home` as its `XDG_CONFIG_HOME`, so it
/// independently resolves the same socket this test bound.
fn run_client(config_home: &Path, stdin_json: &str) {
    use std::io::Write;

    let binary = client_binary();
    let mut child = Command::new(&binary)
        .env("XDG_CONFIG_HOME", config_home)
        .env_remove("CLAUDE_PROJECT_DIR")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|e| panic!("spawn the built client at {}: {e}", binary.display()));
    {
        let mut stdin = child.stdin.take().expect("the child's stdin was piped");
        let _ = stdin.write_all(stdin_json.as_bytes());
    }
    let output = child
        .wait_with_output()
        .expect("wait for the client to exit");

    // The hook contract holds here too, not only in the client's own suite.
    assert!(output.status.success(), "the hook must exit 0");
    assert_eq!(String::from_utf8_lossy(&output.stdout), "");
    assert_eq!(String::from_utf8_lossy(&output.stderr), "");
}

/// A `PostToolUse`/`Write` payload of the LIVE-CAPTURED shape (research's
/// capture against installed CLI `2.1.261`), including the fields the earlier
/// documentation snapshot did not have.
fn write_payload(content: &str) -> String {
    let payload = serde_json::json!({
        "session_id": "281663f4-4aab-48f6-9284-fae93edde1c4",
        "cwd": "/private/tmp/hook-capture-test",
        "permission_mode": "default",
        "effort": { "level": "high" },
        "hook_event_name": "PostToolUse",
        "tool_name": "Write",
        "tool_input": {
            "file_path": "/private/tmp/hook-capture-test/src/lib.rs",
            "content": content
        },
        "tool_response": {
            "type": "create",
            "filePath": "/private/tmp/hook-capture-test/src/lib.rs",
            "content": content,
            "structuredPatch": [],
            "originalFile": null,
            "userModified": false
        },
        "tool_use_id": "toolu_01AL3pQwuQUxJGaa77vmPG9P",
        "duration_ms": 5
    });
    payload.to_string()
}

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
        &write_payload("//! A tiny module.\n\npub fn parse_datagram(bytes: &[u8]) -> u32 {\n    bytes.len() as u32\n}\n"),
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
        &write_payload("//! A tiny module.\n\n// just a note about what will go here one day\n"),
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
