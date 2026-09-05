//! The payload boundary: everything Claude Code hands this process on stdin
//! enters here and nowhere else.
//!
//! The field names below come from a REAL payload captured against the
//! actually-installed CLI (07-RESEARCH.md's live-capture section), not from a
//! documentation snapshot. That distinction matters: the docs snapshot this
//! project started from described two field names that the installed CLI does
//! not send, and building against them would have produced a hook that
//! silently never fired.
//!
//! Every field is optional and there is deliberately NO
//! `deny_unknown_fields`: the live capture carried several fields the earlier
//! snapshot did not have at all, and a payload that GROWS a field must never
//! become a payload this binary rejects.

use serde::Deserialize;

/// One `PostToolUse` invocation, as it arrives on stdin.
///
/// Every field is `Option` because none of them is guaranteed by anything
/// this process controls. `serde` maps a missing key on an `Option` field to
/// `None` without needing an explicit default, and an unknown key is ignored
/// rather than being an error.
#[derive(Debug, Clone, Deserialize)]
pub struct HookPayload {
    pub hook_event_name: Option<String>,
    pub tool_name: Option<String>,
    /// The session's working directory. The fallback base for the
    /// absolute-to-repo-relative conversion when `CLAUDE_PROJECT_DIR` is
    /// absent -- a real fallback, not a decorative one (research Assumptions
    /// Log A2 flags the env var as not independently verified).
    pub cwd: Option<String>,
    pub tool_input: Option<ToolInput>,
    pub tool_response: Option<ToolResponse>,
}

/// The `tool_input` object. `content` is the `Write` branch's whole-file
/// body; `old_string`/`new_string` are the `Edit` branch's before/after
/// fragments.
#[derive(Debug, Clone, Deserialize)]
pub struct ToolInput {
    pub file_path: Option<String>,
    pub old_string: Option<String>,
    pub new_string: Option<String>,
    pub content: Option<String>,
    pub replace_all: Option<bool>,
}

/// The `tool_response` object, of which this crate needs exactly one field.
///
/// The wire key is camel-case while the rest of the payload is snake-case;
/// that asymmetry is the CLI's, faithfully mirrored here rather than
/// smoothed over.
#[derive(Debug, Clone, Deserialize)]
pub struct ToolResponse {
    #[serde(rename = "originalFile")]
    pub original_file: Option<String>,
}

/// A before/after text pair plus the path it belongs to -- the only thing
/// [`crate::detect`] ever needs, and the reason this crate never opens a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    pub old_text: String,
    pub new_text: String,
    pub file_path: String,
}

/// Decodes stdin. Returns a `Result` rather than an `Option` so a future
/// caller could distinguish "not JSON" from "JSON without the fields we
/// want"; today's single caller treats both as a silent no-op.
pub fn parse(stdin_text: &str) -> Result<HookPayload, serde_json::Error> {
    serde_json::from_str(stdin_text)
}

/// Extracts the before/after text this edit represents.
///
/// **Which branches exist today:** only `Write`, where the payload's
/// `content` IS the whole new file and there is by definition no prior text
/// in the payload. `Edit` (an `old_string`/`new_string` fragment pair) and
/// the `Write`-over-an-existing-file case (where `tool_response`'s
/// `original_file` carries the prior body) are plan 07-03's, deliberately.
/// This is sequencing, not omission -- the tracer proves one path end to end
/// before the others are filled in behind it.
///
/// Returns `None` when there is no usable path, since a change this process
/// cannot locate is a change it cannot usefully advertise.
pub fn change_from(payload: &HookPayload) -> Option<Change> {
    let tool_input = payload.tool_input.as_ref()?;
    let file_path = tool_input
        .file_path
        .as_deref()
        .map(str::trim)
        .filter(|path| !path.is_empty())?;

    match payload.tool_name.as_deref() {
        Some("Write") => Some(Change {
            old_text: String::new(),
            new_text: tool_input.content.clone().unwrap_or_default(),
            file_path: file_path.to_string(),
        }),
        _ => None,
    }
}

/// Converts the hook's absolute `file_path` into the repo-relative form
/// [`seam_core::GraphEvent`]'s `source_file` requires, trying `project_dir`
/// first and the payload's own `cwd` second.
///
/// **Why a wrong answer here is worse than no answer.** The receiving side
/// will eventually compare this string against a loaded node's
/// `Node::source_file` with plain equality, and there is no error path
/// anywhere between here and there -- neither `parse_datagram` nor the app's
/// receive loop validates anything but wire shape. A fabricated relative
/// path, or the absolute path passed through unchanged, therefore produces a
/// permanently unresolvable advertisement that looks EXACTLY like a correct
/// one, forever, silently. That is why this function returns nothing rather
/// than guessing: `None` is an honest "I don't know" the wire type already
/// accommodates, and a receiver can act on it.
pub fn repo_relative(
    file_path: &str,
    project_dir: Option<&str>,
    cwd: Option<&str>,
) -> Option<String> {
    for base in [project_dir, cwd].into_iter().flatten() {
        if let Some(remainder) = strip_base(file_path, base) {
            // Blank handling is delegated to the same authority that decided
            // it for the graph's own nodes, rather than re-implemented here
            // by a second independent rule that could drift from it.
            return seam_core::normalize_source_file(Some(remainder));
        }
    }
    None
}

/// The separator requirement is the substantive part: a plain
/// `starts_with(base)` test would wrongly match `/a/repository/src/lib.rs`
/// against base `/a/repo`, producing `sitory/src/lib.rs`.
fn strip_base<'a>(file_path: &'a str, base: &str) -> Option<&'a str> {
    let base = base.trim();
    let base = base.strip_suffix(std::path::MAIN_SEPARATOR).unwrap_or(base);
    if base.is_empty() {
        return None;
    }
    let remainder = file_path.strip_prefix(base)?;
    // Requiring the separator also rules out the base itself (nothing
    // remains) and any already-relative path (no prefix to strip).
    let remainder = remainder.strip_prefix(std::path::MAIN_SEPARATOR)?;
    if remainder.is_empty() {
        None
    } else {
        Some(remainder)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Goes through the real [`parse`] rather than building a `HookPayload`
    /// by hand, so every case here is also a small assertion that the struct
    /// still deserializes the captured wire shape.
    ///
    /// No `unwrap`/`expect`/`panic!` anywhere in this module: the crate-wide
    /// no-panicking-accessor rule is enforced by a grep over `src/*.rs`, which
    /// includes test code.
    fn change_from_json(json: &str) -> Option<Change> {
        let payload = parse(json).ok()?;
        change_from(&payload)
    }

    fn change_parts(change: &Change) -> (&str, &str, &str) {
        (
            change.old_text.as_str(),
            change.new_text.as_str(),
            change.file_path.as_str(),
        )
    }

    #[test]
    fn an_edit_compares_the_replaced_fragment_before_and_after() {
        // The everyday case. The payload carries ONLY the replaced fragment,
        // never the whole file, which is precisely why the before string has
        // to be used: comparing the fragment against an empty string would
        // report every symbol in the fragment as new.
        let json = r#"{
            "hook_event_name": "PostToolUse",
            "tool_name": "Edit",
            "cwd": "/private/tmp/hook-capture-test",
            "tool_input": {
                "file_path": "/private/tmp/hook-capture-test/src/lib.rs",
                "old_string": "pub fn parse() {}\n",
                "new_string": "pub fn parse() {}\n\npub fn build() {}\n",
                "replace_all": false
            }
        }"#;
        assert_eq!(
            change_from_json(json).as_ref().map(change_parts),
            Some((
                "pub fn parse() {}\n",
                "pub fn parse() {}\n\npub fn build() {}\n",
                "/private/tmp/hook-capture-test/src/lib.rs"
            ))
        );
    }

    #[test]
    fn an_edit_with_a_missing_after_string_yields_nothing() {
        let json = r#"{
            "tool_name": "Edit",
            "tool_input": { "file_path": "/repo/src/lib.rs", "old_string": "pub fn parse() {}\n" }
        }"#;
        assert_eq!(change_from_json(json), None);
    }

    #[test]
    fn a_created_file_compares_against_nothing() {
        // The live capture sends `"originalFile": null` on a create. Absent
        // and null must behave identically -- both mean "there was no file."
        let with_null = r#"{
            "tool_name": "Write",
            "tool_input": { "file_path": "/repo/src/lib.rs", "content": "pub fn parse() {}\n" },
            "tool_response": { "type": "create", "originalFile": null }
        }"#;
        let without_the_field = r#"{
            "tool_name": "Write",
            "tool_input": { "file_path": "/repo/src/lib.rs", "content": "pub fn parse() {}\n" },
            "tool_response": { "type": "create" }
        }"#;
        for json in [with_null, without_the_field] {
            assert_eq!(
                change_from_json(json).as_ref().map(change_parts),
                Some(("", "pub fn parse() {}\n", "/repo/src/lib.rs"))
            );
        }
    }

    #[test]
    fn an_overwritten_file_compares_against_the_previous_contents() {
        // The single worst failure mode available to this component: without
        // this, overwriting a hundred-symbol file reports a hundred additions
        // in one burst (T-07-03-02). The payload already carries the previous
        // whole-file contents, so comparing against them costs nothing.
        let json = r#"{
            "tool_name": "Write",
            "tool_input": {
                "file_path": "/repo/src/lib.rs",
                "content": "pub fn parse() {}\n\npub fn build() {}\n"
            },
            "tool_response": {
                "type": "update",
                "originalFile": "pub fn parse() {}\n"
            }
        }"#;
        assert_eq!(
            change_from_json(json).as_ref().map(change_parts),
            Some((
                "pub fn parse() {}\n",
                "pub fn parse() {}\n\npub fn build() {}\n",
                "/repo/src/lib.rs"
            ))
        );
    }

    #[test]
    fn an_unrecognized_tool_name_yields_nothing() {
        // CLIENT-01's matcher is `Edit|Write`. Anything else -- including
        // MultiEdit, whose shape research explicitly did NOT verify -- must
        // not be guessed at.
        for tool in ["MultiEdit", "Bash", "NotebookEdit", ""] {
            let json = format!(
                r#"{{
                    "tool_name": "{tool}",
                    "tool_input": {{ "file_path": "/repo/src/lib.rs", "content": "pub fn parse() {{}}\n" }}
                }}"#
            );
            assert_eq!(change_from_json(&json), None, "tool name `{tool}`");
        }
    }

    #[test]
    fn a_blank_or_missing_file_path_yields_nothing() {
        // A change this process cannot locate is a change it cannot usefully
        // advertise -- and a blank `source_file` on the wire is a
        // `BlankField` rejection at the receiver anyway.
        let blank = r#"{
            "tool_name": "Write",
            "tool_input": { "file_path": "   ", "content": "pub fn parse() {}\n" }
        }"#;
        let missing = r#"{
            "tool_name": "Write",
            "tool_input": { "content": "pub fn parse() {}\n" }
        }"#;
        let no_input = r#"{ "tool_name": "Write" }"#;
        for json in [blank, missing, no_input] {
            assert_eq!(change_from_json(json), None);
        }
    }
}
