//! "Open Project" integration tests (quick task 260913-gud).
//!
//! Serializes every test that reads or writes `project.rs`'s process-global
//! prompt state (or `load.rs`'s process-global `graph_dir`, which
//! `project::open_project_dir` also touches via `startup::preload_graph`)
//! behind a module-local `TEST_LOCK`, the same recipe
//! `settings_panel.rs::tests::settings_store_test_lock` uses against the
//! `settings::Store` global -- `cargo test` runs a test binary's tests on
//! parallel threads by default, and these globals are shared across all of
//! them.

use seam_explorer_egui::app::{BannerKind, SeamExplorerApp};
use seam_explorer_egui::{load, project};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

fn test_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

const CLEAN_FIXTURE: &str = include_str!("../../seam-core/tests/fixtures/clean.json");

/// A fresh, empty scratch directory unique to this test process and case --
/// same `temp_dir(unique)` shape as `settings.rs`/`event_stream.rs`'s own
/// tests.
fn scratch_dir(unique: &str) -> PathBuf {
    std::env::temp_dir().join(format!("op-test-{}-{}", std::process::id(), unique))
}

#[test]
fn graph_path_for_appends_the_graphify_out_layout() {
    let result = project::graph_path_for(Path::new("/x/y"));
    assert_eq!(result, PathBuf::from("/x/y/graphify-out/graph.json"));
}

#[test]
fn opening_a_directory_that_already_has_a_graph_loads_it() {
    let _guard = test_lock().lock().unwrap_or_else(|e| e.into_inner());
    project::clear_pending();

    let dir = scratch_dir("has-graph");
    let out_dir = dir.join("graphify-out");
    std::fs::create_dir_all(&out_dir).expect("must create scratch graphify-out dir");
    std::fs::write(out_dir.join("graph.json"), CLEAN_FIXTURE).expect("must write graph.json");

    let mut app = SeamExplorerApp::default();
    project::open_project_dir(&mut app, &dir);

    assert!(app.model.is_some(), "a valid graph must populate app.model");
    assert!(
        app.model.as_ref().unwrap().graph.node_count() > 0,
        "the loaded graph must have nodes"
    );
    assert!(!app.seams.is_empty(), "must produce at least one seam");
    assert!(
        app.banner.is_none(),
        "a clean fixture must not produce a banner, got {:?}",
        app.banner
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn opening_a_directory_with_a_graph_records_its_directory_for_source_resolution() {
    let _guard = test_lock().lock().unwrap_or_else(|e| e.into_inner());
    project::clear_pending();

    let dir = scratch_dir("records-dir");
    let out_dir = dir.join("graphify-out");
    std::fs::create_dir_all(&out_dir).expect("must create scratch graphify-out dir");
    std::fs::write(out_dir.join("graph.json"), CLEAN_FIXTURE).expect("must write graph.json");

    let mut app = SeamExplorerApp::default();
    project::open_project_dir(&mut app, &dir);

    assert_eq!(
        load::graph_dir(),
        Some(out_dir.clone()),
        "load::graph_dir() must resolve to <dir>/graphify-out -- proof the tracer went \
         through the real load path, not a private shortcut"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn opening_a_directory_with_no_graph_arms_the_prompt() {
    let _guard = test_lock().lock().unwrap_or_else(|e| e.into_inner());
    project::clear_pending();

    let dir = scratch_dir("no-graph");
    std::fs::create_dir_all(&dir).expect("must create scratch dir");

    let mut app = SeamExplorerApp::default();
    project::open_project_dir(&mut app, &dir);

    assert!(
        app.model.is_none(),
        "no graph exists yet, so app.model must stay None"
    );
    assert_eq!(
        project::pending_prompt_dir(),
        Some(dir.clone()),
        "a directory with no graph must arm the prompt for that directory"
    );

    project::clear_pending();
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn opening_a_directory_whose_graph_is_corrupt_banners_instead_of_panicking() {
    let _guard = test_lock().lock().unwrap_or_else(|e| e.into_inner());
    project::clear_pending();

    let dir = scratch_dir("corrupt-graph");
    let out_dir = dir.join("graphify-out");
    std::fs::create_dir_all(&out_dir).expect("must create scratch graphify-out dir");
    std::fs::write(out_dir.join("graph.json"), "{ not json").expect("must write corrupt graph");

    let mut app = SeamExplorerApp::default();
    project::open_project_dir(&mut app, &dir);

    assert!(
        app.model.is_none(),
        "a corrupt graph must not populate app.model"
    );
    match &app.banner {
        Some(banner) if banner.kind == BannerKind::Error => {}
        other => panic!("expected Some(Banner{{kind: Error, ..}}), got {other:?}"),
    }

    let _ = std::fs::remove_dir_all(&dir);
}
