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
//! - **An implementation block is not a node.** `impl Stats {` and
//!   `impl std::fmt::Display for BindError {` contribute nothing. The type
//!   being implemented already exists as a node, and an implementation is a
//!   relationship rather than a class. Research explored an
//!   implementation-target extractor and it was consciously left out, not
//!   forgotten -- including the `impl Trait for Type {}` form that opens and
//!   closes on ONE line, which any brace-depth tracker has to special-case.
//!   This scanner has no depth counter at all, so that form cannot
//!   desynchronise anything.
//! - **A rename is indistinguishable from a delete plus an add.** Nothing in
//!   the payload says the two names are the same item, so a renamed symbol
//!   reports one removal and one addition. Claiming otherwise would mean
//!   guessing, and a wrong guess is worse than two honest events.
//!
//! Both scans are single-pass and linear over their input, so a very large
//! whole-file write costs time proportional to its length and nothing worse.
//! That property comes free from `split`/`strip_prefix` and would not survive
//! a switch to a backtracking matcher -- a second reason this crate has no
//! such dependency.

use std::collections::BTreeSet;

use seam_core::GraphEvent;

/// Compares two versions of a Rust file and returns one advertisement per
/// top-level item that appeared or disappeared between them.
///
/// Returns a list rather than an `Option` (DP-07-02): one edit can genuinely
/// add more than one thing, and silently dropping the extras would be a scope
/// reduction disguised as a signature. "Exactly one event for a single
/// change" is then a `len() == 1` assertion, and "no event for a no-op" an
/// `is_empty()` one, which is what that criterion actually means.
///
/// Structured as a set DIFFERENCE rather than a scan-with-a-guard, so
/// additions and removals fall out of the same comparison and there is
/// exactly one place identity is computed. The sets are ordered
/// ([`BTreeSet`]), which is the whole of the determinism guarantee
/// (T-07-03-04): a hash-ordered collection would vary the emitted sequence
/// run to run and make every downstream test flaky for a cause nobody would
/// find quickly.
pub fn detect(old: &str, new: &str, source_file: Option<&str>) -> Vec<GraphEvent> {
    let source_file = seam_core::normalize_source_file(source_file);
    let before = definitions(old);
    let after = definitions(new);
    let mut events = Vec::new();

    for symbol in after.difference(&before) {
        // D-03: this process advertises what changed and never resolves graph
        // semantics, so `community` is left absent BY DESIGN. It is not an
        // omission and it is not a stub -- a resolver written here would have
        // no live graph to consult and no caller. Phase 8, which holds a
        // loaded `Model`, is where an absent community becomes a real one.
        // Do not "fix" this by inventing a placeholder.
        events.push(GraphEvent::AddNode {
            id: node_id(source_file.as_deref(), symbol),
            label: (*symbol).to_string(),
            community: None,
            source_file: source_file.clone(),
        });
    }

    for symbol in before.difference(&after) {
        // Same identity function as the addition above, so an add and a later
        // remove for one symbol carry the same id and refer to the same thing
        // (DP-07-01).
        events.push(GraphEvent::RemoveNode {
            id: node_id(source_file.as_deref(), symbol),
        });
    }

    events
}

/// Every top-level definition name in `text`, in a stable order.
///
/// Functions and types land in ONE set on purpose: both are nodes, the
/// difference logic that finds additions and removals is identical for them,
/// and a single set means a function renamed into a struct of the same name
/// cannot report as both an add and a remove of the same id.
fn definitions(text: &str) -> BTreeSet<&str> {
    text.lines()
        .filter_map(|line| top_level_fn_name(line).or_else(|| top_level_type_name(line)))
        .collect()
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

/// RED-phase stub (plan 07-03, Task 2). No behaviour yet -- present only so
/// the tests written against it compile and fail on their assertions rather
/// than on a missing symbol.
pub fn qualified_references(_text: &str) -> BTreeSet<String> {
    BTreeSet::new()
}

/// Every visibility opening a top-level declaration can carry.
///
/// Factored out of both scanners rather than multiplied through two prefix
/// tables: 07-02 shipped an enumerated four-entry `fn` list that happened to
/// omit `pub async fn `, which made all five of this workspace's real
/// asynchronous functions invisible while the one form it did cover had zero
/// real occurrences. Peeling visibility off ONCE and then looking for the
/// keyword is the shape that cannot develop that kind of hole.
const VISIBILITY_PREFIXES: [&str; 4] = ["pub(crate) ", "pub(super) ", "pub(self) ", "pub "];

/// `line` with any leading visibility qualifier removed, or `line` unchanged.
///
/// Returns the line itself rather than `None` when there is no qualifier,
/// because an unqualified declaration is the common case. Crucially it does
/// NOT trim: an indented line has no qualifier at column zero, so it passes
/// through still indented and fails the keyword test below -- which is
/// exactly how the column-zero anchor survives this factoring.
fn after_visibility(line: &str) -> &str {
    VISIBILITY_PREFIXES
        .iter()
        .find_map(|prefix| line.strip_prefix(prefix))
        .unwrap_or(line)
}

/// The name of the top-level function defined on `line`, or `None`.
///
/// Zero leading whitespace is required, which is what makes "top-level" mean
/// something to a scanner with no parser. A multi-line signature needs no
/// special handling because only the definition line is ever examined -- a
/// trailing `where` clause and an opening brace on later lines are simply
/// never looked at.
pub fn top_level_fn_name(line: &str) -> Option<&str> {
    let rest = after_visibility(line);
    // Optional, and checked AFTER visibility, because the real form in this
    // workspace is `pub async fn`, never a bare `async fn`.
    let rest = rest.strip_prefix("async ").unwrap_or(rest);
    let rest = rest.strip_prefix("fn ")?;
    let name = rest
        .split(|c: char| c == '(' || c == '<' || c.is_whitespace())
        .next()?;
    if name.is_empty() {
        None
    } else {
        Some(name)
    }
}

/// The keywords that open a top-level type declaration.
///
/// `impl ` is deliberately absent. An implementation block is NOT a
/// definition: the type it implements already exists as a node, and the block
/// is a relationship rather than a class. Research worked out an
/// implementation-target extractor (`impl X for Y {` -> `Y`) and it is
/// consciously left out rather than forgotten -- adding it here would mint a
/// duplicate node for a type that already has one.
const TYPE_KEYWORDS: [&str; 4] = ["struct ", "enum ", "trait ", "type "];

/// The name of the top-level type -- struct, enum, trait or alias -- defined
/// on `line`, or `None`.
///
/// The name is the run before the first `{`, `(`, `;`, `<`, `=` or space,
/// which covers the braced form, the tuple form, the generic form and the
/// alias form without a special case for any of them. A multi-line
/// declaration needs no handling for the same reason a multi-line `fn`
/// signature does not: only the declaration line is ever examined.
pub fn top_level_type_name(line: &str) -> Option<&str> {
    let rest = after_visibility(line);
    let rest = TYPE_KEYWORDS
        .iter()
        .find_map(|keyword| rest.strip_prefix(keyword))?;
    let name = rest
        .split(|c: char| {
            c == '{' || c == '(' || c == ';' || c == '<' || c == '=' || c.is_whitespace()
        })
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

    /// The id carried by a removal advertisement, or `None` for any other
    /// event kind -- the removal-side counterpart of [`add_node_parts`].
    fn remove_node_id(event: &GraphEvent) -> Option<&str> {
        match event {
            GraphEvent::RemoveNode { id } => Some(id.as_str()),
            _ => None,
        }
    }

    fn add_edge_parts(event: &GraphEvent) -> Option<(&str, &str)> {
        match event {
            GraphEvent::AddEdge { source, target } => Some((source.as_str(), target.as_str())),
            _ => None,
        }
    }

    fn remove_edge_parts(event: &GraphEvent) -> Option<(&str, &str)> {
        match event {
            GraphEvent::RemoveEdge { source, target } => Some((source.as_str(), target.as_str())),
            _ => None,
        }
    }

    /// The reference set as a plain ordered vector, so an assertion reads as
    /// the list a human would write rather than as set construction noise.
    fn references(text: &str) -> Vec<String> {
        qualified_references(text).into_iter().collect()
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
        // trace.rs:12), covered by the test below. Both forms are kept
        // because visibility and asynchrony are now independent.
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
    fn a_public_asynchronous_function_is_seen() {
        // apps/seam-explorer-webview/src/commands/trace.rs:12, verbatim.
        // 07-02 found and LOCKED this as a gap: its four-prefix list covered
        // `async fn ` but not `pub async fn `, and this workspace contains no
        // bare `async fn` at column zero at all -- so the one asynchronous
        // form the list covered has zero real occurrences while all five real
        // ones were invisible. 07-02's summary named this plan as the owner of
        // the widening, and this is it. The test is flipped rather than
        // deleted so the history shows the gap closing.
        assert_eq!(
            top_level_fn_name("pub async fn trace_path("),
            Some("trace_path")
        );
        assert_eq!(
            top_level_fn_name("pub(crate) async fn build_graph(path: String) {"),
            Some("build_graph")
        );
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

    // -----------------------------------------------------------------
    // Type definitions -- struct, enum, trait, type alias (plan 07-03,
    // Task 1). Same column-zero anchoring as the function scanner.
    // -----------------------------------------------------------------

    #[test]
    fn a_public_struct_is_seen() {
        // apps/seam-explorer-egui/src/event_stream.rs:110 (`pub struct Stats {`)
        // and the same file's `pub struct EventReceiver {`.
        assert_eq!(
            top_level_type_name("pub struct EventReceiver {"),
            Some("EventReceiver")
        );
        assert_eq!(top_level_type_name("pub struct Stats {"), Some("Stats"));
    }

    #[test]
    fn a_tuple_struct_declared_and_terminated_on_one_line_is_seen() {
        // apps/seam-core/src/verdict.rs:32, verbatim -- the whole declaration
        // is on one line and carries an inner visibility qualifier INSIDE the
        // parentheses, which the name rule must stop before rather than trip on.
        assert_eq!(
            top_level_type_name("pub struct SccIndex(pub(crate) HashMap<NodeIndex, usize>);"),
            Some("SccIndex")
        );
    }

    #[test]
    fn a_private_struct_is_seen() {
        // apps/seam-core/src/ingest.rs:89 -- no public qualifier.
        assert_eq!(top_level_type_name("struct RawNode {"), Some("RawNode"));
    }

    #[test]
    fn a_derive_attribute_line_is_not_a_definition() {
        // apps/seam-core/src/event.rs:32 and ingest.rs:88, verbatim. The
        // attribute sits on the line BEFORE the definition and must not
        // register as one itself.
        assert_eq!(
            top_level_type_name(
                "#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]"
            ),
            None
        );
        assert_eq!(top_level_type_name("#[derive(Deserialize)]"), None);
    }

    #[test]
    fn a_public_enum_is_seen() {
        // apps/seam-core/src/event.rs:34 and :90, verbatim.
        assert_eq!(
            top_level_type_name("pub enum GraphEvent {"),
            Some("GraphEvent")
        );
        assert_eq!(
            top_level_type_name("pub enum EventRejected {"),
            Some("EventRejected")
        );
    }

    #[test]
    fn a_trait_is_seen() {
        // Shape-derived, not verbatim: this workspace defines NO trait of its
        // own (`grep -rn '^pub trait ' apps/ src-tauri/` is empty -- the only
        // traits in play are std's, implemented but not declared here). The
        // form is still covered because a trait is a top-level item D-02
        // names, and the day this workspace grows one it must be seen.
        assert_eq!(top_level_type_name("pub trait X {"), Some("X"));
        assert_eq!(top_level_type_name("trait Verdict {"), Some("Verdict"));
    }

    #[test]
    fn a_type_alias_is_seen() {
        // apps/seam-core/src/model.rs:17 and
        // apps/seam-explorer-egui/src/graph_view.rs:86, both verbatim. The
        // second is a multi-line alias whose right-hand side lands on the
        // next line -- handled for free, exactly as a multi-line `fn`
        // signature is, because only the declaration line is examined.
        assert_eq!(
            top_level_type_name("pub type CommunityId = String;"),
            Some("CommunityId")
        );
        assert_eq!(
            top_level_type_name("pub type SeamGraph ="),
            Some("SeamGraph")
        );
    }

    #[test]
    fn an_indented_type_declaration_is_not_seen() {
        // apps/seam-client/tests/repo_relative_test.rs:15's alias, indented
        // here to stand for a type declared inside a function body. Same
        // column-zero rule the function scanner applies, for the same reason.
        assert_eq!(top_level_type_name("    struct Local {"), None);
        assert_eq!(top_level_type_name("    type Case = ("), None);
    }

    // -----------------------------------------------------------------
    // Implementation blocks are deliberately NOT node definitions.
    // -----------------------------------------------------------------

    #[test]
    fn an_implementation_block_is_not_a_definition() {
        // apps/seam-explorer-egui/src/event_stream.rs:116, verbatim. The type
        // being implemented already exists as a node; the block is a
        // relationship, not a class.
        assert_eq!(top_level_type_name("impl Stats {"), None);
    }

    #[test]
    fn an_implementation_for_a_trait_is_not_a_definition() {
        // apps/seam-explorer-egui/src/event_stream.rs:106, verbatim.
        assert_eq!(
            top_level_type_name("impl std::fmt::Display for BindError {"),
            None
        );
    }

    #[test]
    fn an_implementation_block_that_opens_and_closes_on_one_line_is_not_a_definition() {
        // apps/seam-explorer-egui/src/event_stream.rs:101, verbatim. Research
        // flagged this as the case any naive brace-depth tracker gets wrong.
        // This scanner sidesteps it by never tracking braces AT ALL -- there
        // is no depth counter anywhere in this module -- so a block that opens
        // and closes on one line cannot desynchronise anything. Asserted here
        // both for the line itself and for a surrounding scan that must be
        // completely undisturbed by it.
        assert_eq!(
            top_level_type_name("impl std::error::Error for BindError {}"),
            None
        );
        let source = "pub struct Stats {\n}\n\nimpl std::error::Error for BindError {}\n\npub fn received() -> u64 {\n}\n";
        let types: Vec<&str> = source.lines().filter_map(top_level_type_name).collect();
        let fns: Vec<&str> = source.lines().filter_map(top_level_fn_name).collect();
        assert_eq!(types, vec!["Stats"]);
        assert_eq!(fns, vec!["received"]);
    }

    // -----------------------------------------------------------------
    // Removal, through `detect`
    // -----------------------------------------------------------------

    #[test]
    fn a_deleted_function_reports_a_removal() {
        let old = "pub fn parse_datagram(bytes: &[u8]) -> u32 {\n    0\n}\n\npub fn to_datagram(value: u32) -> Vec<u8> {\n}\n";
        let new = "pub fn parse_datagram(bytes: &[u8]) -> u32 {\n    0\n}\n";
        let events = detect(old, new, Some("src/lib.rs"));
        assert_eq!(events.len(), 1, "exactly one event for a single deletion");
        assert_eq!(
            events.first().and_then(remove_node_id),
            // DP-07-01: the id an ADD for the same symbol would have produced,
            // so an add and a later remove refer to the same thing.
            Some(node_id(Some("src/lib.rs"), "to_datagram").as_str())
        );
    }

    #[test]
    fn a_deleted_type_reports_a_removal() {
        let old = "pub struct Stats {\n}\n\npub enum GraphEvent {\n}\n";
        let new = "pub struct Stats {\n}\n";
        let events = detect(old, new, Some("src/lib.rs"));
        assert_eq!(events.len(), 1);
        assert_eq!(
            events.first().and_then(remove_node_id),
            Some(node_id(Some("src/lib.rs"), "GraphEvent").as_str())
        );
    }

    #[test]
    fn a_simultaneous_add_and_delete_reports_both() {
        let events = detect("fn alpha() {}\n", "fn beta() {}\n", Some("src/lib.rs"));
        assert_eq!(events.len(), 2, "two events, no third");
        let added: Vec<&str> = events
            .iter()
            .filter_map(add_node_parts)
            .map(|p| p.1)
            .collect();
        let removed: Vec<&str> = events.iter().filter_map(remove_node_id).collect();
        assert_eq!(added, vec!["beta"]);
        assert_eq!(removed, vec!["src/lib.rs::alpha"]);
    }

    #[test]
    fn a_renamed_symbol_reports_a_removal_and_an_addition() {
        // The honest consequence of a text scan: a rename is INDISTINGUISHABLE
        // from a delete plus an add, because nothing in the payload says the
        // two are the same item. Asserted rather than pretended away; see this
        // module's blind-spot inventory.
        let old = "pub struct SccIndex(pub(crate) HashMap<NodeIndex, usize>);\n";
        let new = "pub struct SccMap(pub(crate) HashMap<NodeIndex, usize>);\n";
        let events = detect(old, new, Some("src/verdict.rs"));
        assert_eq!(events.len(), 2);
        assert_eq!(
            events
                .iter()
                .filter_map(add_node_parts)
                .map(|p| p.1)
                .collect::<Vec<&str>>(),
            vec!["SccMap"]
        );
        assert_eq!(
            events
                .iter()
                .filter_map(remove_node_id)
                .collect::<Vec<&str>>(),
            vec!["src/verdict.rs::SccIndex"]
        );
    }

    #[test]
    fn the_event_order_is_deterministic() {
        // A hash-ordered collection would vary the emitted sequence run to
        // run, turning every downstream test flaky for a cause nobody finds
        // quickly (T-07-03-04). Ordered sets throughout; asserted here over
        // enough symbols that a hash order would show.
        let old = "fn gone_one() {}\nstruct GoneTwo {}\nfn kept() {}\n";
        let new = "fn kept() {}\nfn added_one() {}\nstruct AddedTwo {}\nenum AddedThree {}\n";
        let first = detect(old, new, Some("src/lib.rs"));
        let second = detect(old, new, Some("src/lib.rs"));
        assert_eq!(
            first, second,
            "the same input must produce an identical sequence"
        );
        assert_eq!(first.len(), 5, "three additions and two removals");
    }

    // -----------------------------------------------------------------
    // Cross-module references (plan 07-03, Task 2). DP-07-03's rule: an
    // edge is a top-level import path or a path-qualified call -- a
    // reference written with at least one path separator.
    // -----------------------------------------------------------------

    #[test]
    fn a_single_item_import_is_a_reference() {
        // apps/seam-explorer-egui/src/event_stream.rs:33 and
        // apps/seam-core/src/event.rs:13, both verbatim.
        assert_eq!(
            references("use seam_core::GraphEvent;\n"),
            vec!["seam_core::GraphEvent"]
        );
        assert_eq!(
            references("use crate::model::CommunityId;\n"),
            vec!["crate::model::CommunityId"]
        );
    }

    #[test]
    fn a_grouped_import_expands_to_one_reference_per_item() {
        // apps/seam-explorer-egui/src/event_stream.rs:30 and :29, verbatim.
        // Grouped imports are common enough in this codebase that not
        // expanding them would lose most of the import signal.
        assert_eq!(
            references("use std::sync::atomic::{AtomicU64, Ordering};\n"),
            vec![
                "std::sync::atomic::AtomicU64",
                "std::sync::atomic::Ordering"
            ]
        );
        assert_eq!(
            references("use std::path::{Path, PathBuf};\n"),
            vec!["std::path::Path", "std::path::PathBuf"]
        );
    }

    #[test]
    fn a_path_qualified_call_is_a_reference() {
        // apps/seam-explorer-egui/src/event_stream.rs:363-364, verbatim --
        // research's own worked example. Two references on these two lines,
        // both genuinely cross-module.
        let refs = references(
            "    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {\n        seam_core::parse_datagram(bytes).map(deliver)\n",
        );
        assert_eq!(
            refs,
            vec![
                "seam_core::parse_datagram",
                "std::panic::AssertUnwindSafe",
                "std::panic::catch_unwind"
            ]
        );
    }

    #[test]
    fn an_unqualified_call_is_not_an_edge() {
        // apps/seam-explorer-egui/src/event_stream.rs:420, verbatim.
        // DP-07-03's scoped decision, not an oversight: an unqualified local
        // call is overwhelmingly a helper inside the same module, and
        // admitting it would drown the cross-module signal a seam explorer
        // exists to show.
        assert!(references("                Ok(()) => {\n").is_empty());
        assert!(references("        deliver(event)\n").is_empty());
    }

    #[test]
    fn a_method_call_through_a_receiver_is_an_accepted_miss() {
        // Research disclosed this one explicitly. `self.parse(bytes)` names a
        // real callee this scan cannot resolve -- the receiver's type is
        // nowhere on the line -- so it is skipped rather than guessed at.
        assert!(references("    self.parse(bytes)\n").is_empty());
        assert!(references("    socket.send_to(&bytes, destination)\n").is_empty());
    }

    #[test]
    fn a_call_through_a_local_binding_is_an_accepted_miss() {
        // apps/seam-explorer-egui/src/event_stream.rs:400 and :412, verbatim.
        // `deliver` is a closure bound to a local; the underlying function it
        // stands for is not written on the line at all, so nothing here can be
        // honestly reported. Research named this as the disclosed limitation.
        assert!(references("        let deliver = |event: GraphEvent| -> Delivery {\n").is_empty());
        assert!(references(
            "                Ok(n) => handle_datagram(&buf[..n], &deliver, &thread_stats),\n"
        )
        .is_empty());
    }

    #[test]
    fn a_formatting_macro_and_a_conversion_are_not_references() {
        // The noise the qualification requirement exists to exclude, without
        // a keyword deny-list anyone would have to maintain.
        assert!(references("    let s = format!(\"{a}\");\n").is_empty());
        assert!(references("    let s = x.to_string();\n").is_empty());
        assert!(references("    assert!(name.is_empty());\n").is_empty());
    }

    #[test]
    fn an_indented_import_is_not_a_reference_but_an_indented_call_is() {
        // The asymmetry is intentional: the IMPORT rule is line-anchored
        // because a top-level import is a top-level item, while the CALL rule
        // is not, because a call sits at whatever depth its enclosing body
        // does. Both halves asserted so neither can drift.
        assert!(references("    use std::path::Path;\n").is_empty());
        assert_eq!(
            references("            seam_core::parse_datagram(bytes);\n"),
            vec!["seam_core::parse_datagram"]
        );
    }

    #[test]
    fn a_wildcard_or_aliased_import_item_is_ignored() {
        // Emitting `std::io::*` or the alias name would be an edge to
        // something that is not a symbol. Nothing is better than misleading.
        assert!(references("use std::io::*;\n").is_empty());
        assert!(references("use std::fmt::Result as FmtResult;\n").is_empty());
    }

    #[test]
    fn a_nested_group_import_is_not_expanded() {
        // Shape-derived: this workspace has no nested group import. The outer
        // items still resolve; the inner group's own items are LOST. A blind
        // spot of a single-pass line scan, recorded rather than papered over.
        assert_eq!(
            references("use std::{fmt, sync::{Arc, Mutex}};\n"),
            vec!["std::fmt"]
        );
    }

    // -----------------------------------------------------------------
    // Edge events, through `detect`
    // -----------------------------------------------------------------

    #[test]
    fn an_added_qualified_call_reports_one_edge() {
        let old = "pub fn handle() {\n}\n";
        let new = "pub fn handle() {\n    seam_core::parse_datagram(bytes);\n}\n";
        let events = detect(old, new, Some("src/lib.rs"));
        assert_eq!(events.len(), 1);
        assert_eq!(
            events.first().and_then(add_edge_parts),
            // DP-07-04: the source endpoint is the changed FILE's node
            // identity, because the scan cannot know which enclosing item the
            // reference sits in and claiming one would be a fabrication.
            Some(("src/lib.rs", "seam_core::parse_datagram"))
        );
    }

    #[test]
    fn an_added_import_reports_one_edge() {
        let events = detect("", "use seam_core::GraphEvent;\n", Some("src/lib.rs"));
        assert_eq!(events.len(), 1);
        assert_eq!(
            events.first().and_then(add_edge_parts),
            Some(("src/lib.rs", "seam_core::GraphEvent"))
        );
    }

    #[test]
    fn a_removed_import_reports_one_removal_edge() {
        let events = detect("use seam_core::GraphEvent;\n", "", Some("src/lib.rs"));
        assert_eq!(events.len(), 1);
        assert_eq!(
            events.first().and_then(remove_edge_parts),
            Some(("src/lib.rs", "seam_core::GraphEvent"))
        );
    }

    #[test]
    fn a_reference_present_in_both_texts_reports_nothing() {
        let text = "use seam_core::GraphEvent;\n\npub fn handle() {\n    seam_core::parse_datagram(bytes);\n}\n";
        assert!(detect(text, text, Some("src/lib.rs")).is_empty());
    }

    #[test]
    fn the_same_reference_appearing_twice_reports_one_edge() {
        // The wire must not carry the same edge twice for one payload
        // (T-07-03-02: volume per invocation is the thing to bound).
        let old = "pub fn handle() {\n}\n";
        let new = "pub fn handle() {\n    seam_core::parse_datagram(a);\n    seam_core::parse_datagram(b);\n}\n";
        let events = detect(old, new, Some("src/lib.rs"));
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn an_import_and_a_call_for_the_same_path_report_one_edge() {
        // The two rules must not double-report the same relationship.
        let old = "pub fn handle() {\n}\n";
        let new = "use seam_core::parse_datagram;\n\npub fn handle() {\n    seam_core::parse_datagram(a);\n}\n";
        let events = detect(old, new, Some("src/lib.rs"));
        assert_eq!(events.len(), 1);
        assert_eq!(
            events.first().and_then(add_edge_parts),
            Some(("src/lib.rs", "seam_core::parse_datagram"))
        );
    }

    #[test]
    fn no_repo_relative_path_means_no_edges() {
        // DP-07-04: an edge whose source endpoint is unknowable is worse than
        // no edge, so none is emitted. Node events still are -- a node needs
        // no second endpoint to be meaningful.
        let new = "use seam_core::GraphEvent;\n\npub fn handle() {\n    seam_core::parse_datagram(a);\n}\n";
        let events = detect("", new, None);
        assert_eq!(events.len(), 1, "the node survives, the edges do not");
        assert_eq!(
            events.first().and_then(add_node_parts),
            Some(("handle", "handle", None, None))
        );
    }

    #[test]
    fn an_edit_adding_a_function_that_calls_across_modules_reports_both() {
        // The case DP-07-02's list return exists for: one payload, two
        // genuinely different events.
        let new = "pub fn handle() {\n    seam_core::parse_datagram(bytes);\n}\n";
        let events = detect("", new, Some("src/lib.rs"));
        assert_eq!(events.len(), 2);
        assert_eq!(
            events
                .iter()
                .filter_map(add_node_parts)
                .map(|p| p.0)
                .collect::<Vec<&str>>(),
            vec!["src/lib.rs::handle"]
        );
        assert_eq!(
            events
                .iter()
                .filter_map(add_edge_parts)
                .collect::<Vec<(&str, &str)>>(),
            vec![("src/lib.rs", "seam_core::parse_datagram")]
        );
    }
}
