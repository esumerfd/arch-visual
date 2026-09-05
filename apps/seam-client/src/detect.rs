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
