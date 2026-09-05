//! The one place this process talks to anything outside itself.
//!
//! Fire-and-forget by design: one unbound datagram socket, a bounded write
//! timeout, no retry, no name resolution, no network. Every failure at every
//! step is swallowed.

use std::os::unix::net::UnixDatagram;
use std::path::Path;
use std::time::Duration;

use seam_core::{to_datagram, GraphEvent, MAX_EVENT_BYTES};

/// The low end of CLIENT-04's 100-200ms budget, chosen deliberately: this
/// cost lands inside a real editing session, so the client would rather drop
/// an advertisement than make the user wait for one.
pub const SEND_TIMEOUT: Duration = Duration::from_millis(100);

/// Sends each event to `destination`, returning how many actually left.
///
/// There is no error type because there is no caller who could act on one.
/// No socket file, nothing listening, a timeout, an oversized message, even
/// failing to construct the socket at all -- every one of those is the same
/// outcome from the user's point of view, and the entire failure policy is
/// "carry on quietly." The return value exists for tests and for a future
/// caller that might want to count, never for error handling.
pub fn send_events(events: &[GraphEvent], destination: &Path) -> usize {
    let socket = match UnixDatagram::unbound() {
        Ok(socket) => socket,
        Err(_) => return 0,
    };
    if socket.set_write_timeout(Some(SEND_TIMEOUT)).is_err() {
        return 0;
    }

    let mut sent: usize = 0;
    for event in events {
        let bytes = to_datagram(event);
        // An empty vector is `to_datagram`'s handling of a serialization
        // failure it cannot actually hit; the receiver would reject it as an
        // empty datagram, so there is nothing to gain by sending it.
        if bytes.is_empty() || bytes.len() >= MAX_EVENT_BYTES {
            continue;
        }
        if socket.send_to(&bytes, destination).is_ok() {
            sent = sent.saturating_add(1);
        }
    }
    sent
}
