//! Integration suite for the CLI-preload startup path (plan 05-14).
//!
//! Covers `startup::graph_path_from_args` (pure argv -> `Option<PathBuf>`)
//! and `startup::preload_graph` (mirrors `SeamExplorerApp::load_graph`'s
//! post-dialog half) against a real `SeamExplorerApp`.
//!
//! These functions live in the library, not in `main.rs`, because an
//! integration test cannot reach a binary crate's items and `fn main()`
//! cannot be called directly. Every preload test below builds the app with
//! `SeamExplorerApp::default()` rather than `SeamExplorerApp::new(cc)`,
//! because `eframe::CreationContext` cannot be constructed in a test --
//! `app.rs`'s `new()` returns `Self::default()` whenever `cc.storage` is
//! `None` (app.rs lines 84-90), so the substitution is exact, not an
//! approximation of the shipped path.
//!
//! Written before `src/startup.rs` exists: this file must fail to compile
//! on its first run (RED-0), the same shape as `tests/tracer_smoke.rs`
//! being written before `src/load.rs` existed.

use seam_explorer_egui::app::{BannerKind, SeamExplorerApp};
use seam_explorer_egui::startup;

/// Joins a path relative to the crate root onto `CARGO_MANIFEST_DIR`, so
/// these tests behave identically regardless of the directory `cargo test`
/// was invoked from.
fn manifest_path(relative: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(relative)
}

// --- graph_path_from_args -------------------------------------------------

#[test]
fn no_cli_arg_yields_no_graph_path() {
    let args = vec!["seam-explorer-egui".to_string()];
    assert_eq!(startup::graph_path_from_args(args), None);
}

#[test]
fn first_cli_arg_is_the_graph_path() {
    let args = vec![
        "seam-explorer-egui".to_string(),
        "sample/graph-demo.json".to_string(),
    ];
    assert_eq!(
        startup::graph_path_from_args(args),
        Some(std::path::PathBuf::from("sample/graph-demo.json"))
    );
}

#[test]
fn extra_cli_args_are_ignored() {
    // First-wins is the contract; the parser is positional and takes no
    // flags.
    let args = vec![
        "seam-explorer-egui".to_string(),
        "first.json".to_string(),
        "second.json".to_string(),
        "third.json".to_string(),
    ];
    assert_eq!(
        startup::graph_path_from_args(args),
        Some(std::path::PathBuf::from("first.json"))
    );
}

#[test]
fn empty_cli_arg_yields_no_graph_path() {
    // A shell expanding an unset variable into an empty argument (e.g.
    // `make run-egui GRAPH=` -> `-- ""`) must not become a read of `""`.
    let args = vec!["seam-explorer-egui".to_string(), "".to_string()];
    assert_eq!(startup::graph_path_from_args(args), None);
}

// --- preload_graph ---------------------------------------------------------

#[test]
fn preload_graph_loads_the_demo_sample() {
    let mut app = SeamExplorerApp::default();
    // The preload must not clobber the one field that round-trips through
    // eframe storage (D-14).
    app.has_seen_trace_onboarding = true;

    let path = manifest_path("../sample/graph-demo.json");
    startup::preload_graph(&mut app, &path);

    assert!(
        app.model.is_some(),
        "expected model to be populated after preloading {path:?}"
    );
    assert!(
        !app.seams.is_empty(),
        "expected a non-empty seam list after preloading {path:?}"
    );
    match &app.banner {
        Some(b) if b.kind == BannerKind::Error => {
            panic!("expected no error banner, got {heading}: {body}", heading = b.heading, body = b.body)
        }
        _ => {}
    }
    assert!(
        app.has_seen_trace_onboarding,
        "preload must not clobber has_seen_trace_onboarding (D-14)"
    );
}

#[test]
fn preload_graph_on_a_missing_file_sets_an_error_banner() {
    let mut app = SeamExplorerApp::default();
    let path = manifest_path("tests/fixtures/does_not_exist.json");

    startup::preload_graph(&mut app, &path);

    assert!(
        app.model.is_none(),
        "a missing file must not populate a model"
    );
    match &app.banner {
        Some(b) if b.kind == BannerKind::Error => {}
        other => panic!(
            "expected Some(Banner {{ kind: Error, .. }}) for a missing file, got {other:?}"
        ),
    }
}

#[test]
fn preload_graph_on_malformed_json_sets_an_error_banner() {
    let mut app = SeamExplorerApp::default();
    let path = manifest_path("tests/fixtures/not_a_graph.json");

    startup::preload_graph(&mut app, &path);

    assert!(
        app.model.is_none(),
        "malformed JSON must not populate a model"
    );
    match &app.banner {
        Some(b) if b.kind == BannerKind::Error => {}
        other => panic!(
            "expected Some(Banner {{ kind: Error, .. }}) for malformed JSON, got {other:?}"
        ),
    }
}

#[test]
fn preload_graph_surfaces_dropped_edge_warnings() {
    let mut app = SeamExplorerApp::default();
    let path = manifest_path("tests/fixtures/dropped_edges_plural.json");

    startup::preload_graph(&mut app, &path);

    // The CLI route carries GRAPH-02's warning tier, not just its error
    // tier: the graph still loads, and a warning banner is set.
    assert!(
        app.model.is_some(),
        "a graph with dangling edges must still load"
    );
    match &app.banner {
        Some(b) if b.kind == BannerKind::Warning => {}
        other => panic!(
            "expected Some(Banner {{ kind: Warning, .. }}) for dropped edges, got {other:?}"
        ),
    }
}
