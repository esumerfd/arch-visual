//! The one byte WINDOWS 31 names, and nothing else (plan 10.1-01, Task 1).
//!
//! `send_events` refused every event at-or-over `MAX_EVENT_BYTES` while
//! `seam_core::parse_datagram` accepts exactly-`MAX_EVENT_BYTES` inclusive, so
//! one legal message size -- exactly 2048 bytes -- was silently dropped by the
//! sender even though the receiver would have taken it and the kernel would
//! have carried it. The v1.1 milestone audit raised the same divergence
//! independently as integration warning W-1.
//!
//! No test covered that boundary before this file, deliberately:
//! `fail_open.rs`'s oversized case puts its symbol FAR past the ceiling
//! precisely so it would not depend on the `>=`-versus-`>` divergence, and its
//! own comment says so. Rather than rewrite that test (which would destroy the
//! independence it was written to have), the boundary gets its own file.
//!
//! This reaches the real `seam_client::send::send_events` directly rather than
//! through the built binary as a child process. The subprocess route would
//! have to manufacture a hook payload whose detected event happens to
//! serialize to exactly 2048 bytes -- arithmetic nobody can read, and
//! arithmetic that would silently stop being exact the day `GraphEvent` gains
//! a field.
//!
//! Deliberately NOT a copy of `fail_open.rs`'s harness by reference --
//! integration test files are separate crates and cannot share one. What is
//! shared is the recipe: a per-test-unique config base, the `sun_path` ceiling
//! guard asserted rather than assumed, and a read timeout so a missing
//! datagram fails as a timeout instead of hanging the suite.

use std::os::unix::net::UnixDatagram;
use std::path::{Path, PathBuf};
use std::time::Duration;

use seam_client::send::send_events;
use seam_core::{parse_datagram, to_datagram, GraphEvent, MAX_EVENT_BYTES, MAX_SUN_PATH_BYTES};

/// A missing datagram must fail as a timeout, never hang the suite.
const RECV_TIMEOUT: Duration = Duration::from_millis(500);

// ---------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------

fn config_home(unique: &str) -> PathBuf {
    let leaf = format!("wb-{}-{}", std::process::id(), unique);
    let from_os_temp = std::env::temp_dir().join(&leaf);
    if socket_path_for(&from_os_temp)
        .is_some_and(|path| path.as_os_str().len() <= MAX_SUN_PATH_BYTES)
    {
        return from_os_temp;
    }
    PathBuf::from("/tmp").join(leaf)
}

/// The destination through the SAME function the app and the client compute
/// it with, so this test cannot accidentally agree with itself about a path
/// neither of them would use.
fn socket_path_for(config_home: &Path) -> Option<PathBuf> {
    seam_core::socket_path_from(config_home.to_str(), None)
}

/// A real bound `AF_UNIX`/`SOCK_DGRAM` socket at the resolved path, with the
/// `sun_path` ceiling asserted rather than hoped for.
fn bind_socket(config_home: &Path) -> (UnixDatagram, PathBuf) {
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
    (socket, path)
}

fn no_datagram_arrives(socket: &UnixDatagram, why: &str) {
    let mut buffer = vec![0u8; MAX_EVENT_BYTES * 2];
    assert!(socket.recv(&mut buffer).is_err(), "{why}");
}

/// Builds an `AddNode` event whose `to_datagram` form is EXACTLY `target`
/// bytes, by measuring an unpadded skeleton and padding the label with plain
/// ASCII filler (1 byte per char, no JSON escaping to account for).
///
/// Mirrors `seam-core`'s `event_test.rs::padded_add_node_json` including both
/// of its internal assertions. The padding arithmetic asserting itself is what
/// stops this file from quietly measuring 2047 and calling it the boundary.
fn padded_add_node(target: usize) -> GraphEvent {
    let skeleton = GraphEvent::AddNode {
        id: "x".to_string(),
        label: String::new(),
        community: Some("c".to_string()),
        source_file: None,
    };
    let base = to_datagram(&skeleton);
    assert!(
        base.len() <= target,
        "target {target} too small for the unpadded skeleton ({} bytes)",
        base.len()
    );
    let pad_needed = target - base.len();
    let event = GraphEvent::AddNode {
        id: "x".to_string(),
        label: "a".repeat(pad_needed),
        community: Some("c".to_string()),
        source_file: None,
    };
    assert_eq!(
        to_datagram(&event).len(),
        target,
        "padding arithmetic must be exact"
    );
    event
}

// ---------------------------------------------------------------------
// The boundary
// ---------------------------------------------------------------------

/// The RED this task exists for, and the whole of WINDOWS 31.
///
/// The FIRST assertion is the one that fails before the fix: `send_events`
/// returns 0 where 1 is required, because its guard skipped at-or-over the
/// ceiling while the receiver accepts exactly-at it. The other three
/// assertions are what make the fix MEANINGFUL rather than merely
/// count-satisfying -- a client that sent a truncated message, or sent some
/// other valid event, or lied about the count and sent nothing at all, would
/// each satisfy the count alone.
#[test]
fn an_event_of_exactly_max_event_bytes_is_sent_and_arrives_whole() {
    let base = config_home("exactly-max");
    let (socket, path) = bind_socket(&base);

    let event = padded_add_node(MAX_EVENT_BYTES);
    let sent = send_events(std::slice::from_ref(&event), &path);
    assert_eq!(
        sent, 1,
        "an event of exactly MAX_EVENT_BYTES must be SENT -- parse_datagram accepts \
         exactly-MAX inclusive and the kernel carries 2048 bytes, so skipping it drops a \
         legal message size (D-02, WINDOWS 31)"
    );

    let mut buffer = vec![0u8; MAX_EVENT_BYTES * 2];
    let read = socket
        .recv(&mut buffer)
        .expect("a datagram of exactly MAX_EVENT_BYTES must ARRIVE on the bound socket");
    buffer.truncate(read);
    assert_eq!(
        buffer.len(),
        MAX_EVENT_BYTES,
        "the datagram must arrive WHOLE -- a short read here would mean the message was \
         truncated in flight, which the returned count alone could never reveal"
    );

    let arrived = parse_datagram(&buffer)
        .expect("the SHIPPED receive-side parser must accept what the client sent");
    assert_eq!(
        arrived, event,
        "the parsed event must EQUAL the event that was sent -- asserting only is_ok() \
         would pass against some other, smaller, valid event arriving instead"
    );
}

/// The guard on the fix: closing the one-byte gap must not widen the window by
/// two.
///
/// This matters concretely, not theoretically. `sysctl
/// net.local.dgram.maxdgram` is 2048 on this platform, and a real `sendto` of
/// 2049 bytes fails with `EMSGSIZE` ("Message too long"). So a guard relaxed
/// one byte too far would not merely send a large message -- it would hand
/// `send_to` something the OS refuses, turning a clean, intentional pre-send
/// skip into a silent failed write. Both halves are asserted: a zero return
/// AND no arrival, because a return of 0 is also what a failed `send_to`
/// produces, and only the absent datagram distinguishes "never attempted"
/// from "attempted and refused".
#[test]
fn an_event_one_byte_over_the_ceiling_is_still_skipped() {
    let base = config_home("one-over");
    let (socket, path) = bind_socket(&base);

    let event = padded_add_node(MAX_EVENT_BYTES + 1);
    let sent = send_events(std::slice::from_ref(&event), &path);
    assert_eq!(
        sent, 0,
        "an event one byte OVER MAX_EVENT_BYTES must still be skipped before the send -- \
         parse_datagram would reject it as TooLarge and this kernel would refuse it with \
         EMSGSIZE"
    );

    no_datagram_arrives(
        &socket,
        "an event over the wire ceiling must be skipped before the send, not attempted and \
         silently refused by the kernel",
    );
}

/// A standing gate on the RECEIVER, stated as a table.
///
/// No socket, no client: this asserts the receive side's half of the contract
/// across a three-point sweep so that the day somebody moves one side, the
/// table says WHICH side moved. It passes before this task's fix as well as
/// after -- it is not evidence that this task changed anything, and the
/// SUMMARY says so rather than counting it as a second red bar.
#[test]
fn the_send_side_and_the_receive_side_agree_on_the_boundary() {
    for target in [MAX_EVENT_BYTES - 1, MAX_EVENT_BYTES, MAX_EVENT_BYTES + 1] {
        let bytes = to_datagram(&padded_add_node(target));
        assert_eq!(bytes.len(), target, "padding arithmetic must be exact");
        assert_eq!(
            parse_datagram(&bytes).is_ok(),
            bytes.len() <= MAX_EVENT_BYTES,
            "the receive side must accept exactly the lengths at or under \
             MAX_EVENT_BYTES and no others; {target} bytes disagreed"
        );
    }
}
