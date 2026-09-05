//! Failing (RED) tests for `seam_core::event`. See 06-01-PLAN.md Task 2's
//! `<behavior>` block for the exact contract these assert. Implementation
//! lands in the following commit (GREEN).
//!
//! This is the hostile-input and round-trip suite for the wire contract
//! shared by `seam-explorer-egui`'s embedded UDS datagram server (Phase 6)
//! and `apps/seam-client` (Phase 7). `parse_datagram` is the ONLY entry point
//! from raw socket bytes to a `GraphEvent`; every property proven here holds
//! for the live socket by construction.

use proptest::prelude::*;
use seam_core::{parse_datagram, to_datagram, EventRejected, GraphEvent, MAX_EVENT_BYTES};

// ---------------------------------------------------------------------------
// Round-trip and shape
// ---------------------------------------------------------------------------

#[test]
fn add_node_round_trips() {
    let event = GraphEvent::AddNode {
        id: "n1".to_string(),
        label: "Node One".to_string(),
        community: "c1".to_string(),
    };
    let bytes = to_datagram(&event);
    assert_eq!(parse_datagram(&bytes), Ok(event));
}

#[test]
fn remove_node_round_trips() {
    let event = GraphEvent::RemoveNode {
        id: "n1".to_string(),
    };
    let bytes = to_datagram(&event);
    assert_eq!(parse_datagram(&bytes), Ok(event));
}

#[test]
fn add_edge_round_trips() {
    let event = GraphEvent::AddEdge {
        source: "n1".to_string(),
        target: "n2".to_string(),
    };
    let bytes = to_datagram(&event);
    assert_eq!(parse_datagram(&bytes), Ok(event));
}

#[test]
fn remove_edge_round_trips() {
    let event = GraphEvent::RemoveEdge {
        source: "n1".to_string(),
        target: "n2".to_string(),
    };
    let bytes = to_datagram(&event);
    assert_eq!(parse_datagram(&bytes), Ok(event));
}

/// Assert on the parsed `serde_json::Value`, not a whole-document string
/// comparison, so field order cannot make the test lie.
#[test]
fn to_datagram_uses_the_exact_four_event_type_tags() {
    let cases: &[(GraphEvent, &str)] = &[
        (
            GraphEvent::AddNode {
                id: "n1".to_string(),
                label: "L".to_string(),
                community: "c".to_string(),
            },
            "add_node",
        ),
        (
            GraphEvent::RemoveNode {
                id: "n1".to_string(),
            },
            "remove_node",
        ),
        (
            GraphEvent::AddEdge {
                source: "n1".to_string(),
                target: "n2".to_string(),
            },
            "add_edge",
        ),
        (
            GraphEvent::RemoveEdge {
                source: "n1".to_string(),
                target: "n2".to_string(),
            },
            "remove_edge",
        ),
    ];
    for (event, expected_tag) in cases {
        let bytes = to_datagram(event);
        let value: serde_json::Value =
            serde_json::from_slice(&bytes).expect("to_datagram must produce valid JSON");
        assert_eq!(
            value.get("event_type").and_then(|v| v.as_str()),
            Some(*expected_tag)
        );
    }
}

/// A non-blank string strategy that deliberately includes quotes,
/// backslashes, newlines, and non-ASCII characters -- the property under
/// test is that JSON escaping does its job, not that a hand-picked fixture
/// happens to survive.
fn adversarial_string() -> impl Strategy<Value = String> {
    proptest::string::string_regex(r"(\PC|\n){1,40}")
        .expect("valid regex")
        .prop_filter("must not be blank after trim", |s| !s.trim().is_empty())
}

proptest! {
    #[test]
    fn round_trip_survives_adversarial_strings(
        id in adversarial_string(),
        label in adversarial_string(),
        community in adversarial_string(),
    ) {
        let event = GraphEvent::AddNode { id, label, community };
        let bytes = to_datagram(&event);
        let parsed = parse_datagram(&bytes).expect("well-formed round trip must parse");
        prop_assert_eq!(parsed, event);
    }
}

// ---------------------------------------------------------------------------
// Optional-field contract on `AddNode` (07-01 Task 1)
//
// D-03/D-04 split responsibility: seam-client ADVERTISES a node without
// claiming to know its community; the receiving app RESOLVES one. These
// assert that `community: None` is a first-class, valid wire state, that a
// Phase-6-era message carrying a resolved community still parses, that a
// present-but-blank value is still refused, and that the other three
// variants were not touched on the wire.
// ---------------------------------------------------------------------------

#[test]
fn an_unresolved_add_node_round_trips() {
    let event = GraphEvent::AddNode {
        id: "src/lib.rs::parse".to_string(),
        label: "parse".to_string(),
        community: None,
        source_file: Some("src/lib.rs".to_string()),
    };
    let bytes = to_datagram(&event);
    assert_eq!(parse_datagram(&bytes), Ok(event));
}

/// Assert on the parsed `serde_json::Value`'s KEYS, never on a whole-document
/// string comparison -- same discipline as
/// `to_datagram_uses_the_exact_four_event_type_tags`.
#[test]
fn an_unresolved_add_node_omits_the_community_key_on_the_wire() {
    let event = GraphEvent::AddNode {
        id: "src/lib.rs::parse".to_string(),
        label: "parse".to_string(),
        community: None,
        source_file: Some("src/lib.rs".to_string()),
    };
    let bytes = to_datagram(&event);
    let value: serde_json::Value =
        serde_json::from_slice(&bytes).expect("to_datagram must produce valid JSON");
    let object = value.as_object().expect("wire form must be a JSON object");
    assert!(
        !object.contains_key("community"),
        "an unresolved advertisement must not spend wire bytes on a null community: {object:?}"
    );
    assert_eq!(
        object.get("source_file").and_then(|v| v.as_str()),
        Some("src/lib.rs")
    );
}

/// The Phase-6-compatibility guarantee: a hand-written message from before
/// this plan existed must still be accepted, unchanged.
#[test]
fn a_wire_message_carrying_a_resolved_community_still_parses() {
    let bytes = br#"{"event_type":"add_node","id":"n1","label":"L","community":"c1"}"#;
    assert_eq!(
        parse_datagram(bytes),
        Ok(GraphEvent::AddNode {
            id: "n1".to_string(),
            label: "L".to_string(),
            community: Some("c1".to_string()),
            source_file: None,
        })
    );
}

#[test]
fn a_blank_community_is_rejected_when_present() {
    let event = GraphEvent::AddNode {
        id: "n1".to_string(),
        label: "L".to_string(),
        community: Some("   ".to_string()),
        source_file: None,
    };
    let bytes = to_datagram(&event);
    assert_eq!(
        parse_datagram(&bytes),
        Err(EventRejected::BlankField("community"))
    );
}

#[test]
fn a_blank_source_file_is_rejected_when_present() {
    let event = GraphEvent::AddNode {
        id: "n1".to_string(),
        label: "L".to_string(),
        community: Some("c1".to_string()),
        source_file: Some(String::new()),
    };
    let bytes = to_datagram(&event);
    assert_eq!(
        parse_datagram(&bytes),
        Err(EventRejected::BlankField("source_file"))
    );
}

/// D-01 enforcement: the shipped shape already carries the link and the
/// class-identifying information the user confirmed, so these three variants
/// need no wire change -- and are proven byte-stable here rather than merely
/// left alone. Only `AddNode` is widened by this plan.
#[test]
fn the_other_three_variants_are_unchanged_on_the_wire() {
    let cases: &[(GraphEvent, &[&str])] = &[
        (
            GraphEvent::RemoveNode {
                id: "n1".to_string(),
            },
            &["event_type", "id"],
        ),
        (
            GraphEvent::AddEdge {
                source: "n1".to_string(),
                target: "n2".to_string(),
            },
            &["event_type", "source", "target"],
        ),
        (
            GraphEvent::RemoveEdge {
                source: "n1".to_string(),
                target: "n2".to_string(),
            },
            &["event_type", "source", "target"],
        ),
    ];
    for (event, expected_keys) in cases {
        let bytes = to_datagram(event);
        let value: serde_json::Value =
            serde_json::from_slice(&bytes).expect("to_datagram must produce valid JSON");
        let object = value.as_object().expect("wire form must be a JSON object");
        let mut actual: Vec<&str> = object.keys().map(String::as_str).collect();
        actual.sort_unstable();
        let mut expected: Vec<&str> = expected_keys.to_vec();
        expected.sort_unstable();
        assert_eq!(actual, expected, "wire key set changed for {event:?}");
    }
}

// ---------------------------------------------------------------------------
// Rejection -- every one of these must return Err, never panic.
// ---------------------------------------------------------------------------

#[test]
fn empty_slice_is_rejected() {
    assert_eq!(parse_datagram(&[]), Err(EventRejected::Empty));
}

#[test]
fn oversized_slice_is_rejected() {
    let bytes = padded_add_node_json(MAX_EVENT_BYTES + 1);
    assert_eq!(bytes.len(), MAX_EVENT_BYTES + 1);
    assert_eq!(
        parse_datagram(&bytes),
        Err(EventRejected::TooLarge(MAX_EVENT_BYTES + 1))
    );
}

#[test]
fn exactly_max_event_bytes_is_accepted_the_boundary_is_inclusive() {
    let bytes = padded_add_node_json(MAX_EVENT_BYTES);
    assert_eq!(bytes.len(), MAX_EVENT_BYTES);
    assert!(
        parse_datagram(&bytes).is_ok(),
        "a datagram of exactly MAX_EVENT_BYTES must be accepted (inclusive boundary)"
    );
}

/// Builds a valid `add_node` JSON datagram of exactly `target` bytes by
/// padding the label field with plain ASCII filler (1 byte per char, no
/// escaping needed) so the byte count is exact and deterministic.
fn padded_add_node_json(target: usize) -> Vec<u8> {
    let base = to_datagram(&GraphEvent::AddNode {
        id: "x".to_string(),
        label: String::new(),
        community: "c".to_string(),
    });
    assert!(
        base.len() <= target,
        "target {target} too small for the unpadded skeleton ({} bytes)",
        base.len()
    );
    let pad_needed = target - base.len();
    let bytes = to_datagram(&GraphEvent::AddNode {
        id: "x".to_string(),
        label: "a".repeat(pad_needed),
        community: "c".to_string(),
    });
    assert_eq!(bytes.len(), target, "padding arithmetic must be exact");
    bytes
}

#[test]
fn invalid_utf8_is_rejected() {
    let bytes: &[u8] = &[0xff, 0xfe, 0xfd];
    assert_eq!(parse_datagram(bytes), Err(EventRejected::NotUtf8));
}

#[test]
fn valid_utf8_pseudo_random_bytes_are_rejected_not_panicked() {
    // 64 bytes drawn from the printable ASCII range: always valid UTF-8,
    // essentially never valid JSON matching our schema.
    let mut state: u64 = 0xDEAD_BEEF_CAFE_F00D;
    let bytes: Vec<u8> = (0..64)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            0x20 + (state % 0x5f) as u8 // printable ASCII 0x20..=0x7e
        })
        .collect();
    let result = parse_datagram(&bytes);
    assert!(
        result.is_err(),
        "64 pseudo-random printable bytes should not accidentally parse"
    );
}

#[test]
fn truncated_json_is_rejected() {
    assert!(parse_datagram(b"{").is_err());
}

#[test]
fn valid_json_wrong_kind_is_rejected() {
    for input in ["null", "[]", "42", "\"a string\""] {
        assert!(
            parse_datagram(input.as_bytes()).is_err(),
            "expected Err for valid-JSON-wrong-kind input: {input}"
        );
    }
}

#[test]
fn unknown_variant_is_rejected() {
    let bytes = br#"{"event_type":"launch_missiles"}"#;
    assert!(parse_datagram(bytes).is_err());
}

#[test]
fn known_variant_missing_required_fields_is_rejected() {
    let bytes = br#"{"event_type":"add_node","id":"x"}"#;
    assert!(parse_datagram(bytes).is_err());
}

#[test]
fn blank_id_is_rejected_empty_and_whitespace_only() {
    for id_value in [r#""""#, r#""   ""#] {
        let bytes =
            format!(r#"{{"event_type":"add_node","id":{id_value},"label":"L","community":"C"}}"#);
        assert_eq!(
            parse_datagram(bytes.as_bytes()),
            Err(EventRejected::BlankField("id"))
        );
    }
}

#[test]
fn blank_label_is_rejected() {
    let bytes = br#"{"event_type":"add_node","id":"x","label":"","community":"C"}"#;
    assert_eq!(
        parse_datagram(bytes),
        Err(EventRejected::BlankField("label"))
    );
}

#[test]
fn blank_community_is_rejected() {
    let bytes = br#"{"event_type":"add_node","id":"x","label":"L","community":""}"#;
    assert_eq!(
        parse_datagram(bytes),
        Err(EventRejected::BlankField("community"))
    );
}

#[test]
fn blank_source_is_rejected() {
    let bytes = br#"{"event_type":"add_edge","source":"","target":"n2"}"#;
    assert_eq!(
        parse_datagram(bytes),
        Err(EventRejected::BlankField("source"))
    );
}

#[test]
fn blank_target_is_rejected() {
    let bytes = br#"{"event_type":"add_edge","source":"n1","target":"   "}"#;
    assert_eq!(
        parse_datagram(bytes),
        Err(EventRejected::BlankField("target"))
    );
}

/// An unknown ADDITIONAL field is tolerated (forward-compatibility stance,
/// matching `settings.rs`'s deliberate default) -- an unknown VARIANT is
/// not. This is a design decision under explicit test, not an accident: an
/// unintended `deny_unknown_fields` would silently break Phase 7's ability
/// to add a field later.
#[test]
fn unknown_additional_field_is_tolerated() {
    let bytes = br#"{"event_type":"add_node","id":"x","label":"L","community":"C","extra":true}"#;
    assert!(parse_datagram(bytes).is_ok());
}

// ---------------------------------------------------------------------------
// Bounded diagnostics
// ---------------------------------------------------------------------------

#[test]
fn rejection_diagnostic_does_not_echo_unbounded_attacker_text() {
    let junk = "x".repeat(1900);
    let doc = format!(r#"{{"event_type":"{junk}"}}"#);
    assert_eq!(doc.len(), 1900 + r#"{"event_type":""}"#.len());
    let err = parse_datagram(doc.as_bytes()).expect_err("unknown variant must be rejected");
    let message = err.to_string();
    assert!(
        message.len() < 300,
        "rejection diagnostic must be bounded under 300 chars, got {} chars: {message}",
        message.len()
    );
}

// ---------------------------------------------------------------------------
// No-panic sweep
// ---------------------------------------------------------------------------

#[test]
fn no_panic_over_200_seeded_pseudo_random_slices() {
    let mut state: u64 = 0x1234_5678_9abc_def0;
    let mut next_byte = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        (state & 0xff) as u8
    };
    for i in 0..200usize {
        let len = (i * 7) % 300;
        let bytes: Vec<u8> = (0..len).map(|_| next_byte()).collect();
        // The point is that this returns rather than unwinds.
        let _ = parse_datagram(&bytes);
    }
}
