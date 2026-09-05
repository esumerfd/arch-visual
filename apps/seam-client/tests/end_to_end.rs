//! The tracer's own proof (plan 07-02, Task 1): a REAL captured-shape
//! `PostToolUse`/`Write` payload, fed on stdin to the REAL built binary
//! running as a child process, against a REAL `AF_UNIX`/`SOCK_DGRAM` socket
//! this test binds itself. No mock socket type and no in-process shortcut
//! exists here -- the whole point is that every hop in
//!
//!   stdin JSON -> hook_input::parse -> hook_input::change_from
//!     -> detect::detect -> seam_core::to_datagram -> send::send_events
//!     -> the socket seam_core::socket_path_from resolves
//!
//! is exercised exactly as it will be when Claude Code invokes the hook.
//!
//! The harness below (`config_home`, `bind_socket`, `run_client`, `recv_one`)
//! is built once here and is meant to be reused verbatim by plans 07-03 and
//! 07-04 rather than reinvented. It follows
//! `apps/seam-explorer-egui/tests/event_stream.rs`'s per-test-unique
//! temp-directory recipe (no `tempfile` dependency).

use std::io::Write;
use std::os::unix::net::UnixDatagram;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::Duration;

use seam_core::{GraphEvent, MAX_EVENT_BYTES, MAX_SUN_PATH_BYTES};

/// A missing datagram must fail this test as a timeout, never hang the suite.
const RECV_TIMEOUT: Duration = Duration::from_secs(2);

/// A per-test-unique directory to hand the child as `XDG_CONFIG_HOME`, so
/// `seam_core::socket_path_from` resolves a path unique to this test and
/// parallel `cargo test` runs cannot collide.
///
/// The OS temp directory can be long enough that the resolved socket path
/// blows the `sun_path` ceiling; `/tmp` is the documented fallback. Silently
/// letting an over-long path through would surface as an opaque `bind`
/// failure that looks exactly like a client bug.
fn config_home(unique: &str) -> PathBuf {
    let leaf = format!("sc-{}-{}", std::process::id(), unique);
    let from_os_temp = std::env::temp_dir().join(&leaf);
    if socket_path_for(&from_os_temp)
        .is_some_and(|path| path.as_os_str().len() <= MAX_SUN_PATH_BYTES)
    {
        return from_os_temp;
    }
    PathBuf::from("/tmp").join(leaf)
}

/// The destination the client will independently compute. Deliberately routed
/// through `seam_core::socket_path_from` -- the SAME function the client and
/// the app both call -- so this test cannot pass against a path the real
/// client would never send to.
fn socket_path_for(config_home: &Path) -> Option<PathBuf> {
    seam_core::socket_path_from(config_home.to_str(), None)
}

/// Binds a real datagram socket at the destination the client will resolve,
/// with a read timeout so an absent datagram fails as a timeout.
fn bind_socket(config_home: &Path) -> UnixDatagram {
    let path =
        socket_path_for(config_home).expect("socket path must resolve from an explicit base");
    assert!(
        path.as_os_str().len() <= MAX_SUN_PATH_BYTES,
        "socket path is {} bytes, over the {MAX_SUN_PATH_BYTES}-byte sun_path ceiling: {}",
        path.as_os_str().len(),
        path.display()
    );
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create the socket's parent directory");
    }
    let _ = std::fs::remove_file(&path);
    let socket = UnixDatagram::bind(&path).expect("bind a real datagram socket");
    socket
        .set_read_timeout(Some(RECV_TIMEOUT))
        .expect("set a receive timeout so a missing datagram cannot hang the suite");
    socket
}

/// Runs the REAL built binary as a child process with `stdin_json` on stdin.
///
/// `CLAUDE_PROJECT_DIR` is explicitly removed rather than left to whatever the
/// test runner inherited: these tests exercise the payload's own `cwd` as the
/// repo-relative base (research Assumptions Log A2 -- the env var is not
/// independently verified, so the fallback must be a real, tested path).
fn run_client(config_home: &Path, stdin_json: &str) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_seam-client"))
        .env("XDG_CONFIG_HOME", config_home)
        .env_remove("CLAUDE_PROJECT_DIR")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn the built seam-client binary");
    let mut stdin = child.stdin.take().expect("the child's stdin was piped");
    stdin
        .write_all(stdin_json.as_bytes())
        .expect("write the payload to the child's stdin");
    drop(stdin);
    child
        .wait_with_output()
        .expect("wait for the child to exit")
}

/// One datagram, or `None` when the read timed out.
fn recv_one(socket: &UnixDatagram) -> Option<Vec<u8>> {
    let mut buffer = vec![0u8; MAX_EVENT_BYTES * 2];
    match socket.recv(&mut buffer) {
        Ok(read) => {
            buffer.truncate(read);
            Some(buffer)
        }
        Err(_) => None,
    }
}

/// Asserts the hook produced nothing a user could see, on every path.
fn assert_silent_success(output: &Output) {
    assert!(
        output.status.success(),
        "the hook must always exit 0; got {:?}",
        output.status
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "",
        "the hook must print nothing to stdout -- it lands in the user's live session"
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "",
        "the hook must print nothing to stderr -- it lands in the user's live session"
    );
}

fn write_fixture() -> serde_json::Value {
    let raw = include_str!("fixtures/post_tool_use_write.json");
    serde_json::from_str(raw).expect("the Write fixture is valid JSON")
}

/// Rebuilds the fixture with a different edited path and file body, keeping
/// every other captured field (including the ones research found the docs
/// snapshot did not have) exactly as the live capture produced them.
fn write_fixture_with(file_path: &str, content: &str) -> String {
    let mut payload = write_fixture();
    payload["tool_input"]["file_path"] = serde_json::Value::String(file_path.to_string());
    payload["tool_input"]["content"] = serde_json::Value::String(content.to_string());
    payload.to_string()
}

#[test]
fn a_written_rust_function_arrives_as_one_add_node_on_a_real_socket() {
    let config_home = config_home("write");
    let socket = bind_socket(&config_home);

    let output = run_client(
        &config_home,
        include_str!("fixtures/post_tool_use_write.json"),
    );
    assert_silent_success(&output);

    let datagram = recv_one(&socket).expect("exactly one datagram must arrive within the timeout");
    let event = seam_core::parse_datagram(&datagram)
        .expect("the already-shipped parser must accept what this client sent");

    match event {
        GraphEvent::AddNode {
            id,
            label,
            community,
            source_file,
        } => {
            assert_eq!(label, "parse_datagram", "the label is the bare symbol name");
            assert_eq!(
                community, None,
                "D-03: the client advertises, it never resolves a community"
            );
            assert_eq!(
                source_file,
                Some("src/lib.rs".to_string()),
                "Pitfall A: the wire carries the repo-relative path, never the absolute one the hook handed over"
            );
            assert_eq!(
                id, "src/lib.rs::parse_datagram",
                "DP-07-01's locked identity shape"
            );
        }
        other => panic!("expected an AddNode advertisement, got {other:?}"),
    }

    assert!(
        recv_one(&socket).is_none(),
        "exactly one datagram: a second receive must time out with nothing"
    );
}

#[test]
fn a_non_rust_file_produces_no_datagram() {
    let config_home = config_home("markdown");
    let socket = bind_socket(&config_home);

    let payload = write_fixture_with(
        "/private/tmp/hook-capture-test/docs/README.md",
        "# Title\n\npub fn parse_datagram(bytes: &[u8]) -> u32 {\n",
    );
    let output = run_client(&config_home, &payload);
    assert_silent_success(&output);

    assert!(
        recv_one(&socket).is_none(),
        "D-02 scopes the heuristic to Rust; a markdown write must advertise nothing"
    );
}

#[test]
fn a_structurally_meaningless_change_produces_no_datagram() {
    let config_home = config_home("comment");
    let socket = bind_socket(&config_home);

    let payload = write_fixture_with(
        "/private/tmp/hook-capture-test/src/lib.rs",
        "// just a note about what will go here one day\n\n",
    );
    let output = run_client(&config_home, &payload);
    assert_silent_success(&output);

    assert!(
        recv_one(&socket).is_none(),
        "a Rust write with no structural content must advertise nothing"
    );
}

#[test]
fn no_socket_at_all_is_a_silent_success() {
    // Nothing is bound: the app is simply not running, which is the ordinary
    // case for most of a user's day. CLIENT-04's core promise, present from
    // the tracer forward rather than bolted on later.
    let config_home = config_home("nosocket");
    std::fs::create_dir_all(&config_home).expect("create an empty config home");

    let output = run_client(
        &config_home,
        include_str!("fixtures/post_tool_use_write.json"),
    );
    assert_silent_success(&output);
}
