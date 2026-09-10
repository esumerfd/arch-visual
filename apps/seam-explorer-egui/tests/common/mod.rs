//! The ONE authority for reaching the cross-crate `seam-client` binary, for
//! building hook payloads of the live-captured shape, and for building an app
//! through the real load path (plan 10-01).
//!
//! Integration test files are separate crates and cannot share a harness by
//! reference, which is how `seam_client_bridge.rs` and `fail_open.rs` ended up
//! with parallel copies of the same recipe. This module is the fix for THIS
//! crate: `seam_client_bridge.rs` was migrated onto it in plan 10-01 and
//! `crash_relaunch.rs` was written against it, so a third copy of
//! `client_binary()` can never quietly appear.
//!
//! `dead_code` is allowed for the whole module because a test binary that
//! includes `mod common;` and uses only some of these functions would
//! otherwise warn -- and `cargo clippy --all-targets -- -D warnings` is a
//! phase gate.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use seam_explorer_egui::app::SeamExplorerApp;

/// The fixture whose nodes carry `source_file`, which is what the
/// sibling-inheritance half of `resolve_community` needs. 6 nodes across three
/// communities (A: a1/a2, B: b1/b2, C: c1/c2).
pub const SOURCE_PATHS_FIXTURE: &str =
    include_str!("../../../seam-core/tests/fixtures/source_paths.json");

/// The `cwd` the live-captured payload carries. `repo_relative` strips it from
/// the absolute `file_path`, which is how the client arrives at a repo-relative
/// `source_file` (Pitfall A).
pub const HOOK_CWD: &str = "/private/tmp/hook-capture-test";

/// The repo-relative path every hook payload in this phase declares an edit to.
///
/// `src/auth/login.rs` is deliberate, not arbitrary: it is the `source_file` of
/// fixture node `a1` in [`SOURCE_PATHS_FIXTURE`], so sibling inheritance
/// resolves the advertised node's community to a concrete `A` and the event
/// genuinely APPLIES and is therefore RECORDED. An unresolvable file would
/// leave the event parked in the unknown bucket and every history assertion
/// downstream would be measuring nothing.
pub const HOOK_REPO_RELATIVE: &str = "src/auth/login.rs";

/// [`HOOK_CWD`] and [`HOOK_REPO_RELATIVE`], joined. `concat!` cannot take const
/// identifiers, so the two halves are restated here as literals; this is the
/// only place in the phase that happens.
pub const HOOK_FILE_PATH: &str =
    concat!("/private/tmp/hook-capture-test", "/", "src/auth/login.rs");

pub fn temp_config_home(unique: &str) -> PathBuf {
    std::env::temp_dir().join(format!("scb-{}-{}", std::process::id(), unique))
}

pub fn wait_until(deadline: Duration, mut condition: impl FnMut() -> bool) -> bool {
    let start = Instant::now();
    while start.elapsed() < deadline {
        if condition() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    condition()
}

/// Locates the `seam-client` binary, BUILDING it if it is not there yet.
///
/// The convenient `CARGO_BIN_EXE_*` macro only exists for binaries in the
/// same package, and these tests live in a different one. Resolving it instead
/// from the running test executable's own location keeps the profile correct
/// for free: a `--release` test run finds the release client, a debug run finds
/// the debug one.
///
/// Building when absent is what turns "this test happens to run after
/// something built the client" from an ordering assumption into a guarantee.
/// It costs nothing when the binary is already current.
///
/// Skipping is NOT an option here. A test that quietly skips when it cannot
/// find its fixture reports green while proving nothing, which is strictly
/// worse than having no test at all (T-07-04-06, carried forward as
/// T-10-01-04) -- so every failure path below panics with the exact path it
/// looked at.
pub fn client_binary() -> PathBuf {
    let test_exe = std::env::current_exe().expect("the running test binary must have a path");
    // .../target/<profile>/deps/<test name>-<hash>
    let profile_dir = test_exe
        .parent()
        .and_then(Path::parent)
        .unwrap_or_else(|| {
            panic!(
                "expected the test binary at target/<profile>/deps/..., got {}",
                test_exe.display()
            )
        })
        .to_path_buf();
    let candidate = profile_dir.join("seam-client");
    if candidate.is_file() {
        return candidate;
    }

    let workspace_manifest = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("Cargo.toml");
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let mut build = Command::new(&cargo);
    build
        .arg("build")
        .arg("-p")
        .arg("seam-client")
        .arg("--manifest-path")
        .arg(&workspace_manifest);
    if profile_dir
        .file_name()
        .is_some_and(|name| name == "release")
    {
        build.arg("--release");
    }
    let status = build.status().unwrap_or_else(|e| {
        panic!("could not run `{cargo} build -p seam-client`: {e}");
    });
    assert!(
        status.success(),
        "`{cargo} build -p seam-client` failed with {status:?}; expected the binary at {}",
        candidate.display()
    );
    assert!(
        candidate.is_file(),
        "built seam-client but no binary is at {} -- this test cannot be skipped, so the path \
         resolution above is what needs fixing",
        candidate.display()
    );
    candidate
}

/// Runs the real client with `config_home` as its `XDG_CONFIG_HOME`, so it
/// independently resolves the same socket the caller bound.
///
/// Asserts the hook contract on every invocation: exit 0, empty stdout, empty
/// stderr.
pub fn run_client(config_home: &Path, stdin_json: &str) {
    use std::io::Write;

    let binary = client_binary();
    let mut child = Command::new(&binary)
        .env("XDG_CONFIG_HOME", config_home)
        .env_remove("CLAUDE_PROJECT_DIR")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|e| panic!("spawn the built client at {}: {e}", binary.display()));
    {
        let mut stdin = child.stdin.take().expect("the child's stdin was piped");
        let _ = stdin.write_all(stdin_json.as_bytes());
    }
    let output = child
        .wait_with_output()
        .expect("wait for the client to exit");

    // The hook contract holds here too, not only in the client's own suite.
    assert!(output.status.success(), "the hook must exit 0");
    assert_eq!(String::from_utf8_lossy(&output.stdout), "");
    assert_eq!(String::from_utf8_lossy(&output.stderr), "");
}

/// A `PostToolUse`/`Write` payload of the LIVE-CAPTURED shape (07-RESEARCH's
/// capture against installed CLI `2.1.261`), including the fields the earlier
/// documentation snapshot did not have.
///
/// The shape is load-bearing, not decorative: it is what makes these tests
/// evidence about the payload Claude Code actually sends. Do not "tidy" a field
/// out of it.
///
/// `file_path` is a parameter rather than a hardcoded constant because this
/// phase needs several distinct edited files; `cwd` stays [`HOOK_CWD`], which
/// is what `repo_relative` strips to reach the repo-relative `source_file`.
pub fn write_payload(file_path: &str, content: &str) -> String {
    let payload = serde_json::json!({
        "session_id": "281663f4-4aab-48f6-9284-fae93edde1c4",
        "cwd": HOOK_CWD,
        "permission_mode": "default",
        "effort": { "level": "high" },
        "hook_event_name": "PostToolUse",
        "tool_name": "Write",
        "tool_input": {
            "file_path": file_path,
            "content": content
        },
        "tool_response": {
            "type": "create",
            "filePath": file_path,
            "content": content,
            "structuredPatch": [],
            "originalFile": null,
            "userModified": false
        },
        "tool_use_id": "toolu_01AL3pQwuQUxJGaa77vmPG9P",
        "duration_ms": 5
    });
    payload.to_string()
}

/// Builds an app through the REAL load path, which is where the replay
/// baseline is captured.
///
/// Never field-by-field: a hand-built app has no baseline at all, and every
/// reconstruction assertion made against it would be measuring an empty model
/// (`timeline_reconstruction.rs`'s rule).
pub fn loaded_app(fixture: &str) -> SeamExplorerApp {
    let outcome =
        seam_explorer_egui::load::read_and_ingest(fixture).expect("fixture must ingest cleanly");
    let mut app = SeamExplorerApp::default();
    app.apply_load_outcome(outcome);
    app
}
