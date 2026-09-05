//! The pure structural-change core: two strings in, a list of advertisements
//! out. No I/O of any kind, so every behaviour here is unit-testable against
//! literal Rust source without a hook, a socket, or a process.
//!
//! Implements D-02: a simple line-anchored text scan, scoped to Rust only.
//! Syntax-tree parsing was considered and explicitly rejected as unneeded
//! complexity for this milestone.
//!
//! # What this heuristic deliberately does not see
//!
//! Anchoring at column zero is a decision, not an accident -- it is what
//! makes "top-level item" mean something to a scanner that has no parser.
//! The cost is a known, accepted inventory of blind spots:
//!
//! - **A function nested inside another function's body** is indented, so it
//!   does not register. That is the intent.
//! - **A method inside an implementation block** is also indented, so it does
//!   not register either. This one is a real limitation rather than a happy
//!   side effect: adding a method is an ordinary way Rust code grows, and
//!   this scanner will not notice. D-02 scoped detection to "new/removed
//!   top-level item," and that scope is being honoured literally rather than
//!   over-claimed.
//! - **A closure bound to a local** never contains the definition keyword at
//!   column zero, so it is correctly not a definition.
//! - **A definition-shaped line inside a string literal or a comment** IS
//!   matched, because a line-oriented scan has no idea it is inside one. This
//!   is a false positive the approach cannot avoid without becoming a parser.
//!   It is asserted truthfully in the tests rather than papered over.
//!
//! Both scans are single-pass and linear over their input, so a very large
//! whole-file write costs time proportional to its length and nothing worse.
//! That property comes free from `split`/`strip_prefix` and would not survive
//! a switch to a backtracking matcher -- a second reason this crate has no
//! such dependency.

use std::collections::HashSet;

use seam_core::GraphEvent;

/// Compares two versions of a Rust file and returns one advertisement per
/// newly-appeared top-level item.
///
/// Returns a list rather than an `Option` (DP-07-02): one edit can genuinely
/// add more than one thing, and silently dropping the extras would be a scope
/// reduction disguised as a signature. "Exactly one event for a single
/// change" is then a `len() == 1` assertion, and "no event for a no-op" an
/// `is_empty()` one, which is what that criterion actually means.
///
/// This tracer implements one rule: added top-level functions. Removal
/// detection, types, and reference edges are plan 07-03's.
pub fn detect(old: &str, new: &str, source_file: Option<&str>) -> Vec<GraphEvent> {
    let source_file = seam_core::normalize_source_file(source_file);
    let before: HashSet<&str> = old.lines().filter_map(top_level_fn_name).collect();
    let mut advertised: HashSet<&str> = HashSet::new();
    let mut events = Vec::new();

    for symbol in new.lines().filter_map(top_level_fn_name) {
        if before.contains(symbol) || !advertised.insert(symbol) {
            continue;
        }
        // D-03: this process advertises what changed and never resolves graph
        // semantics, so `community` is left absent BY DESIGN. It is not an
        // omission and it is not a stub -- a resolver written here would have
        // no live graph to consult and no caller. Phase 8, which holds a
        // loaded `Model`, is where an absent community becomes a real one.
        // Do not "fix" this by inventing a placeholder.
        events.push(GraphEvent::AddNode {
            id: node_id(source_file.as_deref(), symbol),
            label: symbol.to_string(),
            community: None,
            source_file: source_file.clone(),
        });
    }

    events
}

/// DP-07-01's locked node identity: `{source_file}::{symbol}`, falling back
/// to the bare symbol when no repo-relative path is available.
///
/// The client cannot reproduce the opaque slug identifiers a real exported
/// graph uses -- research proved those are not derivable from a path -- so it
/// emits something deterministic and documented instead, and reconciling it
/// against the loaded graph is the receiving side's job. Same split as
/// `community`, same reason.
pub fn node_id(source_file: Option<&str>, symbol: &str) -> String {
    match source_file {
        Some(path) => format!("{path}::{symbol}"),
        None => symbol.to_string(),
    }
}

/// The name of the top-level function defined on `line`, or `None`.
///
/// Only these four prefixes, each requiring zero leading whitespace. A
/// multi-line signature needs no special handling because only the
/// definition line is ever examined -- a trailing `where` clause and an
/// opening brace on later lines are simply never looked at.
pub fn top_level_fn_name(line: &str) -> Option<&str> {
    let rest = line
        .strip_prefix("pub(crate) fn ")
        .or_else(|| line.strip_prefix("pub fn "))
        .or_else(|| line.strip_prefix("async fn "))
        .or_else(|| line.strip_prefix("fn "))?;
    let name = rest
        .split(|c: char| c == '(' || c == '<' || c.is_whitespace())
        .next()?;
    if name.is_empty() {
        None
    } else {
        Some(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every fixture line below is copied VERBATIM from this repository's own
    /// source, at the location named in the comment above it, rather than
    /// invented. Real code is the only corpus that can tell us whether a
    /// line-oriented scanner survives real syntax variety.
    ///
    /// Note there is deliberately no `unwrap`/`expect`/`panic!` anywhere in
    /// this module: the crate-wide no-panicking-accessor rule is enforced by a
    /// grep over `src/*.rs`, which includes test code.
    fn add_node_parts(event: &GraphEvent) -> Option<(&str, &str, Option<&str>, Option<&str>)> {
        match event {
            GraphEvent::AddNode {
                id,
                label,
                community,
                source_file,
            } => Some((
                id.as_str(),
                label.as_str(),
                community.as_deref(),
                source_file.as_deref(),
            )),
            _ => None,
        }
    }

    // -----------------------------------------------------------------
    // Positive recognition
    // -----------------------------------------------------------------

    #[test]
    fn a_public_top_level_function_is_seen() {
        // apps/seam-core/src/event.rs:183
        assert_eq!(
            top_level_fn_name(
                "pub fn parse_datagram(bytes: &[u8]) -> Result<GraphEvent, EventRejected> {"
            ),
            Some("parse_datagram")
        );
    }

    #[test]
    fn a_multi_line_signature_needs_no_special_handling() {
        // apps/seam-explorer-egui/src/startup.rs:19 -- the `where` clause and
        // the opening brace land on lines 20-22 and are never examined.
        assert_eq!(
            top_level_fn_name(
                "pub fn graph_path_from_args<I>(args: I) -> Option<std::path::PathBuf>"
            ),
            Some("graph_path_from_args")
        );
    }

    #[test]
    fn a_generic_function_with_a_lifetime_parameter_is_seen() {
        // apps/seam-core/src/ingest.rs:55
        assert_eq!(
            top_level_fn_name(
                "fn deserialize_source_or_target<'de, D>(deserializer: D) -> Result<String, D::Error>"
            ),
            Some("deserialize_source_or_target")
        );
    }

    #[test]
    fn a_crate_visible_function_is_seen() {
        // apps/seam-explorer-egui/src/panels/seam_list.rs:204
        assert_eq!(
            top_level_fn_name(
                "pub(crate) fn select_seam(app: &mut SeamExplorerApp, seam: &seam_core::Seam) {"
            ),
            Some("select_seam")
        );
    }

    #[test]
    fn a_bare_asynchronous_function_is_seen() {
        // Shape-derived, not verbatim: this workspace contains no BARE
        // `async fn` at column zero. Its real asynchronous functions are all
        // `pub async fn` (e.g. apps/seam-explorer-webview/src/commands/
        // trace.rs:12), which the prefix list does NOT cover -- see
        // `a_public_asynchronous_function_is_not_seen` for that gap.
        assert_eq!(
            top_level_fn_name("async fn trace_path(request: TraceRequest) -> TracePath {"),
            Some("trace_path")
        );
    }

    // -----------------------------------------------------------------
    // Accepted non-recognition -- each limitation is a named, passing test
    // rather than an unstated gap. See this module's doc comment.
    // -----------------------------------------------------------------

    #[test]
    fn a_nested_function_is_not_seen() {
        // apps/seam-core/src/event.rs:136 -- declared inside `validate`'s
        // body, so it is indented and correctly excluded.
        assert_eq!(
            top_level_fn_name(
                "    fn check(field: &'static str, value: &str) -> Result<(), EventRejected> {"
            ),
            None
        );
    }

    #[test]
    fn a_method_inside_an_implementation_block_is_not_seen() {
        // apps/seam-explorer-egui/src/event_stream.rs:117, inside `impl Stats`.
        // A DOCUMENTED GAP, not a bug: adding a method is an ordinary way Rust
        // code grows and this scanner will not notice it. D-02 scoped
        // detection to top-level items and that scope is honoured literally.
        assert_eq!(
            top_level_fn_name("    pub fn received(&self) -> u64 {"),
            None
        );
    }

    #[test]
    fn a_closure_is_not_a_definition() {
        // apps/seam-explorer-egui/src/event_stream.rs:400 -- no definition
        // keyword appears at all, so this is correctly not a definition.
        assert_eq!(
            top_level_fn_name("        let deliver = |event: GraphEvent| -> Delivery {"),
            None
        );
    }

    #[test]
    fn a_public_asynchronous_function_is_not_seen() {
        // apps/seam-explorer-webview/src/commands/trace.rs:12, verbatim.
        // The prefix list covers `async fn ` but not `pub async fn `, so every
        // real asynchronous function in this workspace is invisible to the
        // scan. Asserted truthfully rather than claimed as covered; widening
        // the prefix list is plan 07-03's coverage work.
        assert_eq!(top_level_fn_name("pub async fn trace_path("), None);
    }

    #[test]
    fn a_definition_shaped_line_inside_a_string_literal_is_a_known_false_positive() {
        // A line-oriented scan cannot know it is inside a string literal or a
        // comment. This is the honest current behaviour, not a correctness
        // claim -- avoiding it would mean becoming a parser, which D-02
        // explicitly rejected for this milestone.
        let source = "let sample = \"\nfn looks_like_a_definition() {}\n\";\n";
        let found: Vec<&str> = source.lines().filter_map(top_level_fn_name).collect();
        assert_eq!(found, vec!["looks_like_a_definition"]);
    }

    // -----------------------------------------------------------------
    // End to end through `detect`
    // -----------------------------------------------------------------

    #[test]
    fn only_the_added_function_is_reported() {
        let old = "pub fn parse_datagram(bytes: &[u8]) -> u32 {\n    0\n}\n";
        let new = "pub fn parse_datagram(bytes: &[u8]) -> u32 {\n    0\n}\n\n\
                   pub fn to_datagram(value: u32) -> Vec<u8> {\n    Vec::new()\n}\n";
        let events = detect(old, new, Some("src/lib.rs"));
        assert_eq!(events.len(), 1, "exactly one event for a single added item");
        assert_eq!(
            events.first().and_then(add_node_parts),
            Some((
                "src/lib.rs::to_datagram",
                "to_datagram",
                None,
                Some("src/lib.rs")
            ))
        );
    }

    #[test]
    fn an_unchanged_body_reports_nothing() {
        let text = "pub fn parse_datagram(bytes: &[u8]) -> u32 {\n    0\n}\n";
        assert!(detect(text, text, Some("src/lib.rs")).is_empty());
    }

    #[test]
    fn a_reformatted_body_with_the_same_definitions_reports_nothing() {
        // A real edit INSIDE a function must not look like a structural change.
        let old = "pub fn parse_datagram(bytes: &[u8]) -> u32 {\n    0\n}\n";
        let new = "pub fn parse_datagram(bytes: &[u8]) -> u32 {\n    let n = 0;\n    n\n}\n";
        assert!(detect(old, new, Some("src/lib.rs")).is_empty());
    }

    #[test]
    fn the_event_carries_the_locked_identity_shape() {
        // DP-07-01 for the id, D-03 for the absent community.
        let events = detect("", "fn parse() {}\n", Some("src/lib.rs"));
        assert_eq!(
            events.first().and_then(add_node_parts),
            Some(("src/lib.rs::parse", "parse", None, Some("src/lib.rs")))
        );
    }

    #[test]
    fn a_missing_source_path_falls_back_to_the_bare_symbol() {
        let events = detect("", "fn parse() {}\n", None);
        assert_eq!(
            events.first().and_then(add_node_parts),
            Some(("parse", "parse", None, None))
        );
    }
}
