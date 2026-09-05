//! CLIENT-04's promise, made structural (plan 07-04, Task 1).
//!
//! The phase goal claims the user "cannot tell the difference when the
//! visualizer isn't there." That is a claim about failure paths and about
//! latency, and it is only true if something measures it. This file feeds the
//! REAL built binary a hostile corpus as a REAL child process and asserts,
//! for every single item, the same three things: exit status 0, empty stdout,
//! empty stderr. Then it puts a real clock across the no-server path and
//! asserts a bound.
//!
//! Every case is its own named `#[test]` rather than a table row, so a
//! failure names the input that caused it instead of an index. That is worth
//! the extra process spawns: this suite exists to be read when something has
//! gone wrong.
//!
//! Deliberately NOT a copy of `end_to_end.rs`'s harness by reference --
//! integration test files are separate crates and cannot share one. What is
//! shared is the recipe (per-test-unique `XDG_CONFIG_HOME`, the `sun_path`
//! guard, `CLAUDE_PROJECT_DIR` removed), plus one difference that matters:
//! `run_client` here IGNORES the stdin write result, because a binary that
//! correctly refuses an over-ceiling input stops reading and exits, which
//! breaks the pipe under the writer. Treating that as a harness failure would
//! make the fix look like a bug.

use std::io::Write;
use std::os::unix::net::UnixDatagram;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use seam_core::{MAX_EVENT_BYTES, MAX_SUN_PATH_BYTES};

/// CLIENT-04's budget. The requirement says ~100-200ms; this is its ceiling,
/// measured across the whole child process from spawn to exit.
const BUDGET: Duration = Duration::from_millis(200);

/// A missing datagram must fail as a timeout, never hang the suite. Short,
/// because every use of it here EXPECTS nothing to arrive.
const RECV_TIMEOUT: Duration = Duration::from_millis(500);

// ---------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------

fn config_home(unique: &str) -> PathBuf {
    let leaf = format!("fo-{}-{}", std::process::id(), unique);
    let from_os_temp = std::env::temp_dir().join(&leaf);
    if socket_path_for(&from_os_temp)
        .is_some_and(|path| path.as_os_str().len() <= MAX_SUN_PATH_BYTES)
    {
        return from_os_temp;
    }
    PathBuf::from("/tmp").join(leaf)
}

/// The destination the client will independently compute, through the SAME
/// function the client calls.
fn socket_path_for(config_home: &Path) -> Option<PathBuf> {
    seam_core::socket_path_from(config_home.to_str(), None)
}

/// A config base that exists and is empty: nothing is listening, which is the
/// ordinary state of a user's machine for most of the day.
fn empty_config_home(unique: &str) -> PathBuf {
    let base = config_home(unique);
    std::fs::create_dir_all(&base).expect("create an empty config home");
    base
}

fn bind_socket(config_home: &Path) -> UnixDatagram {
    let path =
        socket_path_for(config_home).expect("socket path must resolve from an explicit base");
    assert!(
        path.as_os_str().len() <= MAX_SUN_PATH_BYTES,
        "socket path is {} bytes, over the {MAX_SUN_PATH_BYTES}-byte sun_path ceiling: {}",
        path.as_os_str().len(),
        path.display()
    );
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create the socket's parent directory");
    }
    let _ = std::fs::remove_file(&path);
    let socket = UnixDatagram::bind(&path).expect("bind a real datagram socket");
    socket
        .set_read_timeout(Some(RECV_TIMEOUT))
        .expect("set a receive timeout so a missing datagram cannot hang the suite");
    socket
}

/// Runs the REAL built binary as a child with `stdin_bytes` on stdin.
///
/// The write result is deliberately discarded. A child that refuses an
/// over-ceiling payload stops reading and exits, and the resulting broken
/// pipe on this side is the CORRECT observable outcome of that refusal, not a
/// harness error.
fn run_client(config_home: &Path, stdin_bytes: &[u8]) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_seam-client"))
        .env("XDG_CONFIG_HOME", config_home)
        .env_remove("CLAUDE_PROJECT_DIR")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn the built seam-client binary");
    {
        let mut stdin = child.stdin.take().expect("the child's stdin was piped");
        let _ = stdin.write_all(stdin_bytes);
        let _ = stdin.flush();
    }
    child
        .wait_with_output()
        .expect("wait for the child to exit")
}

/// The three assertions this entire file is about, applied to every case.
fn assert_silent_success(output: &Output, case: &str) {
    assert!(
        output.status.success(),
        "case `{case}`: the hook must always exit 0; got {:?}",
        output.status
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "",
        "case `{case}`: nothing may reach stdout -- it lands in the user's live session"
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "",
        "case `{case}`: nothing may reach stderr -- it lands in the user's live session"
    );
}

fn no_datagram_arrives(socket: &UnixDatagram, why: &str) {
    let mut buffer = vec![0u8; MAX_EVENT_BYTES * 2];
    assert!(socket.recv(&mut buffer).is_err(), "{why}");
}

// ---------------------------------------------------------------------
// Payload builders
// ---------------------------------------------------------------------

/// A `Write` payload of the LIVE-CAPTURED shape, with a chosen path and body.
fn write_payload(file_path: &str, content: &str) -> String {
    let mut payload: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/post_tool_use_write.json"))
            .expect("the Write fixture is valid JSON");
    payload["tool_input"]["file_path"] = serde_json::Value::String(file_path.to_string());
    payload["tool_input"]["content"] = serde_json::Value::String(content.to_string());
    payload.to_string()
}

/// Realistic, edge-dense Rust source -- NOT a degenerate filler string.
///
/// Plan 07-03 found (its Deviation 2) that a fixture with no `::` in it is the
/// wrong thing to time: under DP-07-03 every path-qualified call is a real
/// edge, so a `std`-heavy file is the honest worst case for the reference
/// scan. This block is deliberately full of them.
///
/// The block REPEATS verbatim rather than generating unique symbol names,
/// which keeps the emitted event count small on purpose. These tests measure
/// the cost of SCANNING a large file, not the cost of sending thousands of
/// datagrams; mixing the two would make a linearity claim unreadable.
const RUST_BLOCK: &str = r#"
use std::collections::BTreeSet;
use std::sync::atomic::{AtomicU64, Ordering};

pub fn handle_datagram(bytes: &[u8]) -> Option<String> {
    let mut buffer = Vec::new();
    buffer.extend_from_slice(bytes);
    let text = String::from_utf8(buffer).ok()?;
    let counter = AtomicU64::new(0);
    counter.fetch_add(1, Ordering::SeqCst);
    let seen = BTreeSet::new();
    let _ = std::time::Duration::from_millis(100);
    let _ = std::path::PathBuf::from("/tmp");
    if seen.is_empty() {
        return Some(text.trim().to_string());
    }
    None
}

pub struct Ledger {
    entries: Vec<String>,
}

pub trait Recorded {
    fn record(&self) -> usize;
}

pub type Outcome = Result<Ledger, std::io::Error>;
"#;

/// `RUST_BLOCK` repeated until it is at least `bytes` long.
fn rust_source_of_at_least(bytes: usize) -> String {
    let repeats = bytes.div_ceil(RUST_BLOCK.len());
    RUST_BLOCK.repeat(repeats)
}

// ---------------------------------------------------------------------
// Corpus: nothing that arrives on stdin may produce a byte or a bad exit
// ---------------------------------------------------------------------

#[test]
fn empty_stdin_is_a_silent_success() {
    let base = empty_config_home("empty");
    let output = run_client(&base, b"");
    assert_silent_success(&output, "empty stdin");
}

#[test]
fn a_single_space_is_a_silent_success() {
    let base = empty_config_home("space");
    let output = run_client(&base, b" ");
    assert_silent_success(&output, "a single space");
}

#[test]
fn a_lone_newline_is_a_silent_success() {
    let base = empty_config_home("newline");
    let output = run_client(&base, b"\n");
    assert_silent_success(&output, "a lone newline");
}

#[test]
fn text_that_is_not_json_at_all_is_a_silent_success() {
    let base = empty_config_home("notjson");
    let output = run_client(&base, b"not json at all");
    assert_silent_success(&output, "not json at all");
}

#[test]
fn a_lone_opening_brace_is_a_silent_success() {
    let base = empty_config_home("brace");
    let output = run_client(&base, b"{");
    assert_silent_success(&output, "{");
}

#[test]
fn an_unterminated_member_is_a_silent_success() {
    let base = empty_config_home("unterminated");
    let output = run_client(&base, br#"{"unterminated": "#);
    assert_silent_success(&output, r#"{"unterminated": "#);
}

#[test]
fn an_empty_array_is_a_silent_success() {
    let base = empty_config_home("array");
    let output = run_client(&base, b"[]");
    assert_silent_success(&output, "[]");
}

#[test]
fn a_bare_null_is_a_silent_success() {
    let base = empty_config_home("null");
    let output = run_client(&base, b"null");
    assert_silent_success(&output, "null");
}

#[test]
fn a_bare_string_is_a_silent_success() {
    let base = empty_config_home("barestring");
    let output = run_client(&base, br#""a bare string""#);
    assert_silent_success(&output, r#""a bare string""#);
}

#[test]
fn a_bare_number_is_a_silent_success() {
    let base = empty_config_home("number");
    let output = run_client(&base, b"12345");
    assert_silent_success(&output, "12345");
}

#[test]
fn bytes_that_are_not_valid_text_are_a_silent_success() {
    // Not merely invalid JSON -- invalid UTF-8, so the failure happens in the
    // stdin read rather than in the decoder. A different code path with the
    // same required outcome.
    let base = empty_config_home("nonutf8");
    let output = run_client(&base, &[0xff, 0xfe, 0xfd, 0x00, 0x80, 0xc3, 0x28]);
    assert_silent_success(&output, "invalid UTF-8 bytes");
}

#[test]
fn a_well_formed_object_with_no_recognized_fields_is_a_silent_success() {
    let base = empty_config_home("norecognized");
    let output = run_client(&base, br#"{"alpha": 1, "beta": {"gamma": [true, null]}}"#);
    assert_silent_success(&output, "an object with no recognized fields");
}

#[test]
fn a_number_where_the_file_path_belongs_is_a_silent_success() {
    let base = empty_config_home("wrongpath");
    let output = run_client(
        &base,
        br#"{"tool_name":"Write","cwd":"/repo","tool_input":{"file_path":12345,"content":"pub fn a() {}\n"}}"#,
    );
    assert_silent_success(&output, "a number where the path belongs");
}

#[test]
fn an_array_where_the_tool_input_object_belongs_is_a_silent_success() {
    let base = empty_config_home("wronginput");
    let output = run_client(
        &base,
        br#"{"tool_name":"Write","cwd":"/repo","tool_input":["file_path","/repo/src/lib.rs"]}"#,
    );
    assert_silent_success(&output, "an array where tool_input belongs");
}

#[test]
fn a_nested_object_where_a_string_belongs_is_a_silent_success() {
    let base = empty_config_home("wrongstring");
    let output = run_client(
        &base,
        br#"{"tool_name":{"name":"Write"},"cwd":"/repo","tool_input":{"file_path":"/repo/src/lib.rs","content":"pub fn a() {}\n"}}"#,
    );
    assert_silent_success(&output, "a nested object where a string belongs");
}

#[test]
fn a_boolean_where_content_belongs_is_a_silent_success() {
    let base = empty_config_home("wrongcontent");
    let output = run_client(
        &base,
        br#"{"tool_name":"Write","cwd":"/repo","tool_input":{"file_path":"/repo/src/lib.rs","content":true}}"#,
    );
    assert_silent_success(&output, "a boolean where content belongs");
}

#[test]
fn json_nested_two_hundred_levels_deep_is_a_silent_success() {
    // The case a recursive decoder can blow the stack on. Stack exhaustion is
    // not a catchable failure -- it aborts the process with a signal, which is
    // neither a zero exit nor silence -- so it is the one hostile shape the
    // rest of the fail-open discipline structurally cannot cover
    // (T-07-04-01). The nesting sits in an UNKNOWN field alongside otherwise
    // valid ones, which is the shape a real hostile payload would take.
    let base = empty_config_home("deep");
    let deep = format!("{}{}", "[".repeat(200), "]".repeat(200));
    let payload = format!(
        r#"{{"tool_name":"Write","cwd":"/repo","tool_input":{{"file_path":"/repo/src/lib.rs","content":"pub fn a() {{}}\n"}},"junk":{deep}}}"#
    );
    let output = run_client(&base, payload.as_bytes());
    assert_silent_success(&output, "JSON nested two hundred levels deep");
}

#[test]
fn a_one_megabyte_content_field_is_a_silent_success() {
    let base = empty_config_home("onemeg");
    let payload = write_payload(
        "/private/tmp/hook-capture-test/src/lib.rs",
        &rust_source_of_at_least(1_000_000),
    );
    let output = run_client(&base, payload.as_bytes());
    assert_silent_success(&output, "a one-megabyte content field");
}

#[test]
fn a_ten_thousand_character_file_path_is_a_silent_success() {
    let base = empty_config_home("longpath");
    let long = format!(
        "/private/tmp/hook-capture-test/{}/lib.rs",
        "d".repeat(10_000)
    );
    let payload = write_payload(&long, "pub fn a() {}\n");
    let output = run_client(&base, payload.as_bytes());
    assert_silent_success(&output, "a ten-thousand-character file path");
}

#[test]
fn an_unrecognized_tool_name_is_a_silent_success() {
    let base = empty_config_home("unknowntool");
    let output = run_client(
        &base,
        br#"{"tool_name":"LaunchMissiles","cwd":"/repo","tool_input":{"file_path":"/repo/src/lib.rs","content":"pub fn a() {}\n"}}"#,
    );
    assert_silent_success(&output, "an unrecognized tool name");
}

#[test]
fn extra_unknown_top_level_fields_are_still_processed_normally() {
    // The forward-compatibility guarantee, and the one corpus item whose
    // assertion is a PRESENCE rather than an absence. The live capture
    // already carried fields the earlier documentation did not have (`effort`
    // among them), so a payload that GROWS a field must never become a
    // payload this binary rejects. Without the arriving datagram below, this
    // test would pass just as happily against a binary that rejected the
    // whole payload -- which is exactly the regression it exists to catch.
    let base = config_home("forwardcompat");
    let socket = bind_socket(&base);

    let payload = format!(
        r#"{{"tool_name":"Write","cwd":"/private/tmp/hook-capture-test","effort":{{"level":"high"}},"a_field_from_the_future":[1,2,3],"tool_input":{{"file_path":"/private/tmp/hook-capture-test/src/lib.rs","content":{}}},"tool_response":{{"type":"create","originalFile":null}}}}"#,
        serde_json::to_string("pub fn newly_added() {}\n").expect("escape a short literal")
    );
    let output = run_client(&base, payload.as_bytes());
    assert_silent_success(&output, "extra unknown top-level fields");

    let mut buffer = vec![0u8; MAX_EVENT_BYTES * 2];
    let read = socket
        .recv(&mut buffer)
        .expect("an unknown extra field must not stop the payload being processed");
    buffer.truncate(read);
    let event = seam_core::parse_datagram(&buffer).expect("the shipped parser must accept it");
    match event {
        seam_core::GraphEvent::AddNode { id, .. } => {
            assert_eq!(id, "src/lib.rs::newly_added")
        }
        other => panic!("expected an AddNode advertisement, got {other:?}"),
    }
}

#[test]
fn a_symbol_name_over_the_wire_ceiling_is_a_silent_success_and_sends_nothing() {
    // CONFIRMS (does not rewrite) 07-02's oversized-event skip: an event that
    // would not fit the wire is dropped BEFORE the send, silently. The symbol
    // is far past `MAX_EVENT_BYTES` rather than one byte past it, so this
    // case does not depend on 07-02's known `>=`-versus-`>` boundary
    // divergence from `parse_datagram`.
    let base = config_home("oversized");
    let socket = bind_socket(&base);

    let symbol = "a".repeat(MAX_EVENT_BYTES * 2);
    let payload = write_payload(
        "/private/tmp/hook-capture-test/src/lib.rs",
        &format!("pub fn {symbol}() {{}}\n"),
    );
    let output = run_client(&base, payload.as_bytes());
    assert_silent_success(&output, "a symbol name over the wire ceiling");

    no_datagram_arrives(
        &socket,
        "an event over the wire ceiling must be skipped, not sent as a truncated message",
    );
}

// ---------------------------------------------------------------------
// Timing: a real clock, with nothing listening
// ---------------------------------------------------------------------

/// Five real runs, every one printed, the best returned.
///
/// Best-of-five keeps first-run page-in cost from making the suite flaky, and
/// printing all five means the SUMMARY can quote real numbers instead of a
/// pass/fail bit.
fn best_of_five(label: &str, base: &Path, payload: &[u8]) -> Duration {
    let mut runs = Vec::new();
    for _ in 0..5 {
        let start = Instant::now();
        let output = run_client(base, payload);
        runs.push(start.elapsed());
        assert_silent_success(&output, label);
    }
    let micros: Vec<u128> = runs.iter().map(Duration::as_micros).collect();
    let best = runs.iter().copied().min().unwrap_or(Duration::MAX);
    println!("TIMING {label}: runs(us)={micros:?} best={best:?} budget={BUDGET:?}");
    best
}

fn assert_inside_budget(label: &str, best: Duration) {
    assert!(
        best < BUDGET,
        "{label}: best of five was {best:?}, over CLIENT-04's {BUDGET:?} budget"
    );
}

#[test]
fn a_realistic_payload_with_no_server_finishes_inside_the_budget() {
    let base = empty_config_home("timing-realistic");
    let payload = write_payload(
        "/private/tmp/hook-capture-test/src/lib.rs",
        &format!("{RUST_BLOCK}\npub fn newly_added() {{}}\n"),
    );
    let best = best_of_five(
        "a realistic payload, nothing listening",
        &base,
        payload.as_bytes(),
    );
    assert_inside_budget("a realistic payload, nothing listening", best);
}

#[test]
fn a_one_megabyte_payload_with_no_server_finishes_inside_the_budget() {
    // The linear-scan claim, proven empirically rather than by inspection.
    // The body is `std`-heavy on purpose (see `RUST_BLOCK`).
    let base = empty_config_home("timing-onemeg");
    let payload = write_payload(
        "/private/tmp/hook-capture-test/src/lib.rs",
        &rust_source_of_at_least(1_000_000),
    );
    let best = best_of_five(
        "a one-megabyte payload, nothing listening",
        &base,
        payload.as_bytes(),
    );
    assert_inside_budget("a one-megabyte payload, nothing listening", best);
}

#[test]
fn stdin_far_over_the_size_ceiling_finishes_inside_the_budget() {
    // The one corpus item whose enforcing assertion is a CLOCK rather than an
    // exit code. An enormous stdin already exits 0 silently today -- it just
    // does so slowly, which breaks CLIENT-04 through the availability door
    // rather than the correctness one. Measured before the ceiling existed:
    // 8 MiB took 782ms and 32 MiB took 3507ms against the same 200ms budget.
    //
    // 32 MiB is 8x `MAX_STDIN_BYTES`, so this stays unambiguous in both the
    // debug profile these tests run under and the release profile a user
    // registers.
    let base = empty_config_home("timing-oversized-stdin");
    let payload = write_payload(
        "/private/tmp/hook-capture-test/src/lib.rs",
        &rust_source_of_at_least(32 * 1024 * 1024),
    );
    let best = best_of_five("stdin far over the size ceiling", &base, payload.as_bytes());
    assert_inside_budget("stdin far over the size ceiling", best);
}

#[test]
fn a_dead_socket_file_does_not_stall_the_budget() {
    // Distinct from "nothing there" and slower to fail: the send now targets
    // something that EXISTS but is not a socket, so it fails on a different
    // errno path (T-07-04-03). The write timeout is set before any send
    // attempt, which is what keeps this bounded.
    let base = config_home("timing-deadsocket");
    let path = socket_path_for(&base).expect("socket path must resolve");
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create the socket's parent directory");
    }
    let _ = std::fs::remove_file(&path);
    std::fs::write(&path, b"this is a plain file, not a socket")
        .expect("plant a plain file where the socket belongs");

    let payload = write_payload(
        "/private/tmp/hook-capture-test/src/lib.rs",
        &format!("{RUST_BLOCK}\npub fn newly_added() {{}}\n"),
    );
    let best = best_of_five("a dead socket file", &base, payload.as_bytes());
    assert_inside_budget("a dead socket file", best);
}
