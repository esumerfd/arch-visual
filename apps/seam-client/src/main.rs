//! The Claude Code `PostToolUse` hook entry point.
//!
//! One exit, and it is always success. The whole body routes through
//! [`run`], whose return value nobody acts on, so there is no path -- not a
//! parse failure, not a missing socket, not a send timeout -- that can turn
//! into a non-zero status or a printed line inside the user's live session.

use std::io::Read;

use seam_client::{detect, hook_input, send};

fn main() {
    let mut stdin_text = String::new();
    // Reading STDIN through the standard-input handle is not a filesystem
    // read: this binary never opens the edited path, because the payload
    // already carries its before and after text verbatim.
    let _ = std::io::stdin().read_to_string(&mut stdin_text);
    let _ = run(&stdin_text);
}

/// Returns how many advertisements were sent -- a number nobody reads.
///
/// Short-circuits to a silent zero when stdin does not parse, no change can
/// be extracted, the file is not Rust, detection finds nothing structural, or
/// no destination path resolves.
fn run(stdin_text: &str) -> usize {
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
