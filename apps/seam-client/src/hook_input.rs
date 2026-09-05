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
