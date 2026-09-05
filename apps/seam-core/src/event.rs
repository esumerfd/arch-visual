//! The wire contract shared by `seam-explorer-egui`'s embedded UDS datagram
//! server (Phase 6) and `apps/seam-client` (Phase 7). This module is the
//! ONLY definition of that format in the repo. See `06-CONTEXT.md`'s
//! Claude's Discretion entry, which placed this type in `seam-core` (the one
//! crate both UI stacks and the future hook client already depend on)
//! rather than in `seam-explorer-egui` alone.
//!
//! Deliberately describes node/edge PRESENCE changes only -- never a
//! community or grouping change -- because EVENT-05 fixes the community
//! structure at the originally-loaded graph; a live event can add or remove
//! a node/edge but can never move one between communities.

use crate::model::CommunityId;

/// The largest datagram the kernel will carry over an `AF_UNIX`/`SOCK_DGRAM`
/// socket on macOS, as measured on this machine:
///
/// ```text
/// $ sysctl net.local.dgram.maxdgram net.local.dgram.recvspace
/// net.local.dgram.maxdgram: 2048
/// net.local.dgram.recvspace: 4096
/// ```
///
/// A ceiling above `maxdgram` would describe messages that cannot physically
/// be sent over this socket type, so `MAX_EVENT_BYTES` is set from the
/// measured kernel value rather than an invented round number.
pub const MAX_EVENT_BYTES: usize = 2048;

/// The four graph mutations this milestone recognises. Internally-tagged
/// (`tag = "event_type"`) rather than externally-tagged so the wire form is
/// a flat object a human can write by hand into `nc -U` while debugging.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "event_type", rename_all = "snake_case")]
pub enum GraphEvent {
    AddNode {
        id: String,
        label: String,
        /// `None` means "the sender could not and deliberately did not
        /// resolve a community -- the receiving app must look one up."
        ///
        /// This field is optional because of D-03/D-04's split of
        /// responsibility: D-03, `apps/seam-client` ADVERTISES what changed
        /// and deliberately never resolves graph semantics (it is a
        /// stateless, one-shot process with no principled access to the
        /// loaded graph's community assignments); D-04, the receiving app
        /// OWNS turning a raw advertisement into a fully-formed event,
        /// including the community lookup.
        ///
        /// Invariant this type does NOT enforce: a `None` must never
        /// survive into applied or history-stored graph state, or EVENT-05's
        /// "communities never change" guarantee silently breaks. Enforcing
        /// that is Phase 8's job -- the phase that has a live `Model` in
        /// scope -- not this type's. `None` is the single, explicit,
        /// greppable way to say "unresolved"; a present-but-blank value is
        /// still a `BlankField` rejection.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        community: Option<CommunityId>,
        /// The repo-relative source path this node came from, in
        /// [`crate::model::Node::source_file`]'s exact convention, so a
        /// future resolver can compare the two with `==` and find the
        /// sibling nodes whose community a new node should inherit.
        ///
        /// `None` when the sender could not determine a repo-relative path
        /// (for example an edit outside the project root). A sender must
        /// never fall back to an absolute path here: an absolute path would
        /// compare unequal to every `Node::source_file` in the loaded graph,
        /// silently resolving to "no sibling found" with no error surfaced
        /// anywhere in the pipeline.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        source_file: Option<String>,
    },
    RemoveNode {
        id: String,
    },
    AddEdge {
        source: String,
        target: String,
    },
    RemoveEdge {
        source: String,
        target: String,
    },
}

/// Every way `parse_datagram` can refuse to turn bytes into a [`GraphEvent`].
/// A rejection is a per-message discard in a loop that keeps running --
/// never fatal, unlike [`crate::SeamCoreError`] (see that type's doc comment
/// for why the two are kept separate).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EventRejected {
    #[error("empty datagram")]
    Empty,
    #[error("datagram of {0} bytes exceeds the maximum datagram size")]
    TooLarge(usize),
    #[error("datagram is not valid UTF-8")]
    NotUtf8,
    #[error("not a recognized graph event: {0}")]
    NotAGraphEvent(String),
    #[error("field `{0}` is blank")]
    BlankField(&'static str),
}

/// Upper bound (in `char`s, not bytes) on the serde diagnostic text embedded
/// in [`EventRejected::NotAGraphEvent`]. T-06-01-04 mitigation: `serde_json`'s
/// unknown-variant error text quotes the offending value, so an attacker who
/// controls `event_type` controls part of the message; this constructor
/// truncates it so a rejection can never become a channel for echoing an
/// unbounded amount of attacker-supplied text.
const NOT_A_GRAPH_EVENT_MAX_CHARS: usize = 200;

/// Builds a [`EventRejected::NotAGraphEvent`], truncating `message` to at
/// most [`NOT_A_GRAPH_EVENT_MAX_CHARS`] characters with a trailing ellipsis
/// when longer. Truncates on a `char_indices` boundary, never a byte index,
/// so a multi-byte character can never be cut in half.
fn not_a_graph_event(message: String) -> EventRejected {
    if message.chars().count() <= NOT_A_GRAPH_EVENT_MAX_CHARS {
        return EventRejected::NotAGraphEvent(message);
    }
    let cut = message
        .char_indices()
        .nth(NOT_A_GRAPH_EVENT_MAX_CHARS)
        .map(|(byte_index, _)| byte_index)
        .unwrap_or(message.len());
    let mut truncated = message[..cut].to_string();
    truncated.push_str("...");
    EventRejected::NotAGraphEvent(truncated)
}

/// Rejects any id/label/community/source_file/source/target that is blank --
/// empty or whitespace-only after `trim()` -- following `model.rs`'s
/// `normalize_source_file` convention of deciding blank-is-not-a-value once,
/// at the boundary, never re-checked downstream. `AddNode`'s two optional
/// fields are checked only when present: absent is a valid state, blank
/// never is.
fn validate(event: &GraphEvent) -> Result<(), EventRejected> {
    fn check(field: &'static str, value: &str) -> Result<(), EventRejected> {
        if value.trim().is_empty() {
            Err(EventRejected::BlankField(field))
        } else {
            Ok(())
        }
    }
    match event {
        GraphEvent::AddNode {
            id,
            label,
            community,
            source_file,
        } => {
            check("id", id)?;
            check("label", label)?;
            // Only blank-check the optional fields when they are `Some`:
            // absence is the legitimate "unresolved" state (D-03), while a
            // present-but-blank value is still a `BlankField` rejection --
            // so `None` stays the single explicit way to say "unresolved".
            if let Some(community) = community {
                check("community", community)?;
            }
            if let Some(source_file) = source_file {
                check("source_file", source_file)?;
            }
        }
        GraphEvent::RemoveNode { id } => {
            check("id", id)?;
        }
        GraphEvent::AddEdge { source, target } | GraphEvent::RemoveEdge { source, target } => {
            check("source", source)?;
            check("target", target)?;
        }
    }
    Ok(())
}

/// Converts an arbitrary byte slice from an unauthenticated local process
/// into a [`GraphEvent`] or a typed [`EventRejected`]. No panicking
/// accessor, no slice indexing that can go out of range, and no arithmetic
/// that can overflow anywhere in this function or its helpers -- this
/// function is the whole of EVENT-02's guarantee.
///
/// Deliberately does NOT validate graph membership ("does node X exist in
/// the currently loaded `Model`?") -- that belongs to Phase 8, which has the
/// loaded graph in scope. This function only validates the WIRE SHAPE.
pub fn parse_datagram(bytes: &[u8]) -> Result<GraphEvent, EventRejected> {
    if bytes.is_empty() {
        return Err(EventRejected::Empty);
    }
    if bytes.len() > MAX_EVENT_BYTES {
        return Err(EventRejected::TooLarge(bytes.len()));
    }
    let text = std::str::from_utf8(bytes).map_err(|_| EventRejected::NotUtf8)?;
    let event: GraphEvent =
        serde_json::from_str(text).map_err(|e| not_a_graph_event(e.to_string()))?;
    validate(&event)?;
    Ok(event)
}

/// Serializes a [`GraphEvent`] to its wire form. Mirrors
/// `settings::to_json`'s explicit-`Result`-handling treatment of the
/// can't-realistically-fail case: `GraphEvent` is a plain data enum of
/// `String`/`CommunityId` fields, so `serde_json::to_vec` cannot fail on it
/// in practice, but the `Result` is still matched explicitly rather than
/// asserted away in a comment.
pub fn to_datagram(event: &GraphEvent) -> Vec<u8> {
    // `GraphEvent` is a plain data enum of `String`/`CommunityId` fields, so
    // `serde_json::to_vec` cannot fail on it in practice -- the impossible
    // branch is still handled explicitly (an empty `Vec`, never a panic),
    // via `unwrap_or_default` rather than a match, per clippy's
    // `manual_unwrap_or_default` lint.
    serde_json::to_vec(event).unwrap_or_default()
}
