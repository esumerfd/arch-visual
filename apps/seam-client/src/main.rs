//! The Claude Code `PostToolUse` hook entry point.
//!
//! One exit, and it is always success. The whole body routes through
//! [`run`], whose return value nobody acts on, so there is no path -- not a
//! parse failure, not a missing socket, not a send timeout -- that can turn
//! into a non-zero status or a printed line inside the user's live session.

use std::io::Read;

use seam_client::{detect, hook_input, send};

/// The most stdin this process will accept, and therefore the upper bound on
/// how much work a caller can make it do.
///
/// **What it protects against (T-07-04-02).** An enormous payload does not
/// break correctness -- it already exits 0 in silence -- it breaks the
/// AVAILABILITY half of CLIENT-04, which is the half a user actually feels.
/// Measured against this binary with no ceiling: 8 MiB cost 782ms, 16 MiB
/// 1798ms, 32 MiB 3507ms, all against a 200ms budget.
///
/// **Why this number.** At the ceiling the shipped release binary costs 87ms,
/// inside the budget with room to spare. Stated honestly: an unoptimized
/// debug build costs 390ms at the same ceiling, so the budget claim is about
/// the release binary a user actually registers as a hook (see the README's
/// build step), not about every possible build of this code.
///
/// It is deliberately far above the ~1 MiB a large real Rust file reaches, so
/// no honest edit is ever refused -- and deliberately above the ~1 MB payload
/// `a_one_megabyte_payload_with_no_server_finishes_inside_the_budget` uses,
/// so that test keeps proving the linear-scan claim rather than passing
/// because its input got rejected at the door.
const MAX_STDIN_BYTES: usize = 4 * 1024 * 1024;

/// The deepest bracket nesting this process will hand to the decoder.
///
/// **What it protects against (T-07-04-01).** A recursive decoder can exhaust
/// the stack on pathologically nested input, and stack exhaustion is the one
/// failure the rest of the fail-open discipline structurally cannot cover: it
/// aborts the process with a signal, which is neither a zero exit nor
/// silence.
///
/// **Honest note on what this is worth today.** `serde_json` already refuses
/// past its own internal 128-level limit ("recursion limit exceeded"), so no
/// nesting depth -- 200, or 100,000, both measured -- can currently crash
/// this binary. This check is therefore defence in depth, not a fix for an
/// observed crash, and the SUMMARY says so. It earns its place by making the
/// guarantee one THIS crate states and tests, at a limit this crate chooses,
/// rather than one inherited from a dependency's internal default that no
/// test here pins and no semver promise covers.
///
/// 64 is far above the depth any real payload reaches; the live-captured
/// shape nests about four levels.
const MAX_JSON_DEPTH: usize = 64;

/// Maximum bracket nesting in `text`, compared against `limit` in one cheap
/// forward pass -- no allocation, no recursion, and no decode.
///
/// String state is tracked rather than counting raw bytes, because a `Write`
/// payload's `content` is a whole Rust FILE: every brace in it sits inside a
/// JSON string literal and none of it is JSON nesting. Ignoring that would
/// let an unbalanced `Edit` fragment drift the count upward and refuse
/// perfectly honest input.
fn exceeds_json_depth(text: &str, limit: usize) -> bool {
    let mut depth: usize = 0;
    let mut in_string = false;
    let mut escaped = false;

    for byte in text.bytes() {
        if in_string {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
            continue;
        }
        match byte {
            b'"' => in_string = true,
            b'{' | b'[' => {
                depth = depth.saturating_add(1);
                if depth > limit {
                    return true;
                }
            }
            b'}' | b']' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    false
}

fn main() {
    let mut stdin_text = String::new();
    // Reading STDIN through the standard-input handle is not a filesystem
    // read: this binary never opens the edited path, because the payload
    // already carries its before and after text verbatim.
    //
    // `take` is the enforcement point, not a later length check: refusing an
    // over-ceiling payload has to mean NOT READING IT. One byte past the
    // ceiling is enough to know it is over while bounding what this process
    // will ever pull into memory. The writer sees the resulting closed pipe,
    // which is the correct signal that the hook declined -- and is why the
    // fail-open suite's harness ignores its own stdin write result.
    let mut source = std::io::stdin().take(MAX_STDIN_BYTES as u64 + 1);
    let _ = source.read_to_string(&mut stdin_text);
    let _ = run(&stdin_text);
}

/// Returns how many advertisements were sent -- a number nobody reads.
///
/// Short-circuits to a silent zero when stdin does not parse, no change can
/// be extracted, the file is not Rust, detection finds nothing structural, or
/// no destination path resolves.
fn run(stdin_text: &str) -> usize {
    // Both ceilings are checked BEFORE the decoder ever sees the bytes, and
    // in this order: the size check is the cheaper of the two and bounds the
    // work the depth scan can be made to do.
    if stdin_text.len() > MAX_STDIN_BYTES {
        return 0;
    }
    if exceeds_json_depth(stdin_text, MAX_JSON_DEPTH) {
        return 0;
    }

    let payload = match hook_input::parse(stdin_text) {
        Ok(payload) => payload,
        Err(_) => return 0,
    };
    let change = match hook_input::change_from(&payload) {
        Some(change) => change,
        None => return 0,
    };
    // D-02 scopes the heuristic to Rust. Everything else is a file this
    // milestone has no opinion about.
    if !change.file_path.ends_with(".rs") {
        return 0;
    }

    let project_dir = std::env::var("CLAUDE_PROJECT_DIR").ok();
    let source_file = hook_input::repo_relative(
        &change.file_path,
        project_dir.as_deref(),
        payload.cwd.as_deref(),
    );

    let events = detect::detect(&change.old_text, &change.new_text, source_file.as_deref());
    if events.is_empty() {
        return 0;
    }

    let destination = match seam_core::default_socket_path() {
        Some(destination) => destination,
        None => return 0,
    };
    send::send_events(&events, &destination)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// No `unwrap`/`expect`/`panic!` anywhere in this module: the crate-wide
    /// no-panicking-accessor rule is enforced by a grep over `src/*.rs`,
    /// which includes test code.
    const LIMIT: usize = 64;

    fn nested(depth: usize) -> String {
        format!("{{\"junk\":{}{}}}", "[".repeat(depth), "]".repeat(depth))
    }

    #[test]
    fn a_realistically_shaped_payload_is_nowhere_near_the_limit() {
        let payload = r#"{"hook_event_name":"PostToolUse","tool_name":"Write",
            "tool_input":{"file_path":"/repo/src/lib.rs","content":"pub fn a() {}"},
            "tool_response":{"type":"create","structuredPatch":[{"lines":["a"]}]}}"#;
        assert!(!exceeds_json_depth(payload, LIMIT));
    }

    #[test]
    fn nesting_past_the_limit_is_refused() {
        assert!(exceeds_json_depth(&nested(LIMIT + 1), LIMIT));
        assert!(exceeds_json_depth(&nested(200), LIMIT));
        assert!(exceeds_json_depth(&nested(100_000), LIMIT));
    }

    #[test]
    fn nesting_exactly_at_the_limit_is_accepted() {
        // The limit is a ceiling, not a doorstep. `nested` already adds one
        // level of its own for the wrapping object.
        assert!(!exceeds_json_depth(&nested(LIMIT - 1), LIMIT));
    }

    #[test]
    fn brackets_inside_a_string_literal_do_not_count() {
        // This is why the scan tracks string state rather than counting raw
        // bytes: a `Write` payload's `content` is a whole Rust FILE, and
        // every brace in it would otherwise be read as JSON nesting. An
        // unbalanced `Edit` fragment (`"pub fn parse() {"`) would then let
        // depth drift upward across a payload and refuse honest input.
        let payload = r#"{"content":"fn a() { fn b() { fn c() { [[[[[[[ "}"#;
        assert!(!exceeds_json_depth(payload, 4));
    }

    #[test]
    fn an_escaped_quote_does_not_end_the_string() {
        // Getting this wrong flips the scan's idea of inside-versus-outside
        // for the whole rest of the payload.
        let payload = r#"{"content":"he said \"[[[[[[[[[[\" and left"}"#;
        assert!(!exceeds_json_depth(payload, 4));
    }
}
