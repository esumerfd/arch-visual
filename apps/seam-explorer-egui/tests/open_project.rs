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

use egui_kittest::kittest::Queryable;
use seam_explorer_egui::app::{BannerKind, SeamExplorerApp};
use seam_explorer_egui::project::BuildOutcome;
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

// ============================================================
// Task 2: building the graph with graphify when the project has none
// ============================================================

#[test]
fn the_build_argument_vector_is_the_headless_code_only_extract() {
    let argv = project::build_command(Path::new("/x/y"));
    assert_eq!(
        argv.len(),
        4,
        "expected program, extract, dir, --code-only, got {argv:?}"
    );
    assert_eq!(
        &argv[1..],
        &[
            "extract".to_string(),
            "/x/y".to_string(),
            "--code-only".to_string()
        ]
    );
}

#[test]
fn a_clean_exit_that_wrote_a_graph_succeeds() {
    assert!(matches!(
        project::classify_build(true, "", true),
        BuildOutcome::Succeeded(_)
    ));
}

#[test]
fn a_clean_exit_that_wrote_nothing_fails() {
    match project::classify_build(true, "", false) {
        BuildOutcome::Failed(msg) => {
            assert!(
                !msg.is_empty() && msg.to_lowercase().contains("graph"),
                "message must mention the missing graph rather than be empty, got {msg:?}"
            );
        }
        other => panic!("expected Failed, got {other:?}"),
    }
}

#[test]
fn a_nonzero_exit_fails_and_keeps_the_stderr_tail() {
    match project::classify_build(false, "boom", false) {
        BuildOutcome::Failed(msg) => {
            assert!(
                msg.contains("boom"),
                "message must keep the stderr tail, got {msg:?}"
            );
        }
        other => panic!("expected Failed, got {other:?}"),
    }
}

#[test]
fn the_build_error_banner_is_an_error_kind_banner_naming_the_tool() {
    let banner = project::build_error_banner("boom");
    assert_eq!(banner.kind, BannerKind::Error);
    assert!(
        banner.body.contains("graphify"),
        "body must name graphify, got {:?}",
        banner.body
    );
    assert!(
        banner.body.contains("boom"),
        "body must carry the failure detail, got {:?}",
        banner.body
    );
}

#[test]
fn program_resolution_prefers_an_existing_absolute_candidate() {
    let dir = scratch_dir("program-resolution");
    std::fs::create_dir_all(&dir).expect("must create scratch dir");
    let missing = dir.join("does-not-exist-graphify");
    let present = dir.join("graphify");
    std::fs::write(&present, "#!/bin/sh\n").expect("must write stand-in binary");

    let resolved = project::program_from(&[missing.clone(), present.clone()]);
    assert_eq!(resolved, present.to_string_lossy());

    let all_missing = [
        dir.join("nope-one").join("graphify"),
        dir.join("nope-two").join("graphify"),
    ];
    let fallback = project::program_from(&all_missing);
    assert_eq!(
        fallback, "graphify",
        "when nothing exists, the bare tool name must be returned so PATH lookup still gets its chance"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn spawning_a_build_returns_immediately_and_reports_later() {
    let build_argv = vec!["/bin/sleep".to_string(), "1".to_string()];
    let label_argv = vec!["/bin/true".to_string()];
    let graph_path = PathBuf::from("/definitely/not/a/real/graphify-out/graph.json");

    let start = std::time::Instant::now();
    let rx = project::spawn_build(build_argv, label_argv, graph_path, egui::Context::default());
    let elapsed = start.elapsed();
    assert!(
        elapsed.as_millis() < 100,
        "spawn_build must return immediately (non-blocking), took {elapsed:?}"
    );

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        match rx.try_recv() {
            Ok(report) => {
                assert!(
                    matches!(report.outcome, BuildOutcome::Failed(_)),
                    "sleep writes no graph, so this must not report Succeeded, got {:?}",
                    report.outcome
                );
                assert!(
                    report.label.is_none(),
                    "sleep writes no graph, so naming must never have been attempted, got {:?}",
                    report.label
                );
                break;
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                if std::time::Instant::now() > deadline {
                    panic!("no outcome arrived within 10s");
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                panic!("sender dropped without sending an outcome")
            }
        }
    }
}

// ============================================================
// Task 1 (quick-260913-rjt): naming communities after a successful extract
// ============================================================

#[test]
fn the_label_argument_vector_names_the_same_resolved_binary() {
    let argv = project::label_command(Path::new("/x/y"));
    assert_eq!(argv.len(), 3, "expected program, label, dir, got {argv:?}");
    assert_eq!(
        &argv[1..],
        &["label".to_string(), "/x/y".to_string()],
        "elements after the program must be exactly the naming subcommand and the dir"
    );
    assert_eq!(
        argv[0],
        project::build_command(Path::new("/x/y"))[0],
        "both invocations must resolve through the same binary-resolution authority"
    );
}

#[test]
fn a_clean_naming_exit_is_labeled() {
    assert!(matches!(
        project::classify_label(true, ""),
        project::LabelOutcome::Labeled
    ));
}

#[test]
fn a_nonzero_naming_exit_is_skipped_and_keeps_its_reason() {
    match project::classify_label(false, "no backend configured") {
        project::LabelOutcome::Skipped(reason) => {
            assert!(
                reason.contains("no backend configured"),
                "reason must carry the stderr tail, got {reason:?}"
            );
        }
        other => panic!("expected Skipped, got {other:?}"),
    }

    match project::classify_label(false, "") {
        project::LabelOutcome::Skipped(reason) => {
            assert!(
                !reason.is_empty(),
                "reason must be non-empty even with no stderr"
            );
        }
        other => panic!("expected Skipped, got {other:?}"),
    }
}

#[test]
fn naming_is_not_attempted_when_the_extract_produced_no_graph() {
    let dir = scratch_dir("no-graph-no-naming");
    std::fs::create_dir_all(&dir).expect("must create scratch dir");
    let marker = dir.join("marker");
    let graph_path = dir.join("graphify-out").join("graph.json"); // never written

    let build_argv = vec!["/usr/bin/false".to_string()];
    let label_argv = vec![
        "/usr/bin/touch".to_string(),
        marker.to_string_lossy().into_owned(),
    ];

    let report = project::run_project_build(&build_argv, &label_argv, &graph_path);

    assert!(
        matches!(report.outcome, BuildOutcome::Failed(_)),
        "expected Failed, got {:?}",
        report.outcome
    );
    assert!(
        report.label.is_none(),
        "naming must not run when the extract produced no graph, got {:?}",
        report.label
    );
    assert!(
        !marker.exists(),
        "the naming process must never have been spawned"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn naming_runs_after_an_extract_that_wrote_a_graph() {
    let dir = scratch_dir("extract-then-name");
    let out_dir = dir.join("graphify-out");
    std::fs::create_dir_all(&out_dir).expect("must create scratch graphify-out dir");
    let graph_path = out_dir.join("graph.json");
    let marker = dir.join("marker");

    let build_argv = vec![
        "/usr/bin/touch".to_string(),
        graph_path.to_string_lossy().into_owned(),
    ];
    let label_argv = vec![
        "/usr/bin/touch".to_string(),
        marker.to_string_lossy().into_owned(),
    ];

    let report = project::run_project_build(&build_argv, &label_argv, &graph_path);

    match report.outcome {
        BuildOutcome::Succeeded(path) => assert_eq!(path, graph_path),
        other => panic!("expected Succeeded, got {other:?}"),
    }
    assert!(
        matches!(report.label, Some(project::LabelOutcome::Labeled)),
        "expected Some(Labeled), got {:?}",
        report.label
    );
    assert!(marker.exists(), "the naming step must actually have run");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_skipped_naming_step_still_loads_the_graph() {
    let _guard = test_lock().lock().unwrap_or_else(|e| e.into_inner());
    project::clear_pending();

    let dir = scratch_dir("skipped-naming-loads");
    let out_dir = dir.join("graphify-out");
    std::fs::create_dir_all(&out_dir).expect("must create scratch graphify-out dir");
    let graph_path = out_dir.join("graph.json");
    std::fs::write(&graph_path, CLEAN_FIXTURE).expect("must write graph.json");

    let mut app = SeamExplorerApp::default();
    let report = project::BuildReport {
        outcome: BuildOutcome::Succeeded(graph_path.clone()),
        label: Some(project::LabelOutcome::Skipped(
            "no LLM backend configured".to_string(),
        )),
    };
    project::apply_build_report(&mut app, report);

    assert!(app.model.is_some(), "a valid graph must populate app.model");
    assert!(
        app.model.as_ref().unwrap().graph.node_count() > 0,
        "the loaded graph must have nodes"
    );
    assert!(!app.seams.is_empty(), "must produce at least one seam");
    assert!(
        !matches!(app.banner, Some(ref b) if b.kind == BannerKind::Error),
        "a skipped naming step must not be reported as a build failure, got {:?}",
        app.banner
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_failed_extract_still_banners_an_error_and_never_reports_naming() {
    let mut app = SeamExplorerApp::default();
    let report = project::BuildReport {
        outcome: BuildOutcome::Failed("boom".to_string()),
        label: None,
    };
    project::apply_build_report(&mut app, report);

    assert!(
        app.model.is_none(),
        "a failed build must not populate app.model"
    );
    match &app.banner {
        Some(banner) if banner.kind == BannerKind::Error => {
            assert!(
                banner.body.contains("graphify"),
                "body must name graphify, got {:?}",
                banner.body
            );
            assert!(
                banner.body.contains("boom"),
                "body must carry the failure detail, got {:?}",
                banner.body
            );
        }
        other => panic!("expected Some(Banner{{kind: Error, ..}}), got {other:?}"),
    }
}

// ============================================================
// Task 2 (quick-260913-rjt): informational banner when naming is skipped
// ============================================================

#[test]
fn the_naming_note_is_a_non_error_banner_that_says_what_was_skipped() {
    let note = project::label_skipped_note("no LLM backend configured");
    assert_eq!(
        note.kind,
        BannerKind::Info,
        "expected the informational banner kind, got {:?}",
        note.kind
    );
    assert_ne!(
        note.kind,
        BannerKind::Error,
        "a skipped naming step must not read as an error"
    );
    assert!(
        note.body.to_lowercase().contains("communit"),
        "body must mention communities, got {:?}",
        note.body
    );
    assert!(
        note.body.contains("no LLM backend configured"),
        "body must carry the reason, got {:?}",
        note.body
    );
}

#[test]
fn the_note_stands_alone_when_the_load_produced_no_banner() {
    let note = project::label_skipped_note("no LLM backend configured");
    let merged = project::merge_label_note(None, note.clone())
        .expect("merging with no existing banner must yield Some(note)");
    assert_eq!(merged.kind, note.kind);
    assert_eq!(merged.body, note.body);
}

#[test]
fn the_note_never_erases_a_banner_the_load_already_produced() {
    let existing = seam_explorer_egui::app::Banner {
        kind: BannerKind::Warning,
        heading: "Some edges were dropped".to_string(),
        body: "2 edges referenced a component id that isn't in this graph.".to_string(),
    };
    let note = project::label_skipped_note("no LLM backend configured");

    let merged = project::merge_label_note(Some(existing.clone()), note.clone())
        .expect("merging with an existing banner must yield Some");

    assert_eq!(
        merged.kind, existing.kind,
        "kind must stay the existing banner's kind"
    );
    assert_eq!(
        merged.heading, existing.heading,
        "heading must stay the existing banner's heading"
    );
    assert!(
        merged.body.contains(&existing.body),
        "merged body must contain the existing body, got {:?}",
        merged.body
    );
    assert!(
        merged.body.contains(&note.body),
        "merged body must contain the note's body, got {:?}",
        merged.body
    );
}

#[test]
fn a_skipped_naming_step_shows_the_informational_note() {
    let _guard = test_lock().lock().unwrap_or_else(|e| e.into_inner());
    project::clear_pending();

    let dir = scratch_dir("skipped-naming-note");
    let out_dir = dir.join("graphify-out");
    std::fs::create_dir_all(&out_dir).expect("must create scratch graphify-out dir");
    let graph_path = out_dir.join("graph.json");
    std::fs::write(&graph_path, CLEAN_FIXTURE).expect("must write graph.json");

    let mut app = SeamExplorerApp::default();
    let report = project::BuildReport {
        outcome: BuildOutcome::Succeeded(graph_path.clone()),
        label: Some(project::LabelOutcome::Skipped(
            "no LLM backend configured".to_string(),
        )),
    };
    project::apply_build_report(&mut app, report);

    assert!(app.model.is_some(), "the graph must still load");
    match &app.banner {
        Some(banner) => {
            assert_eq!(
                banner.kind,
                BannerKind::Info,
                "expected the informational banner kind, got {:?}",
                banner.kind
            );
            assert!(
                banner.body.contains("no LLM backend configured"),
                "body must carry the reason, got {:?}",
                banner.body
            );
        }
        None => panic!("expected Some(informational banner)"),
    }

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_successful_naming_step_adds_no_note() {
    let _guard = test_lock().lock().unwrap_or_else(|e| e.into_inner());
    project::clear_pending();

    let dir = scratch_dir("labeled-no-note");
    let out_dir = dir.join("graphify-out");
    std::fs::create_dir_all(&out_dir).expect("must create scratch graphify-out dir");
    let graph_path = out_dir.join("graph.json");
    std::fs::write(&graph_path, CLEAN_FIXTURE).expect("must write graph.json");

    let mut app = SeamExplorerApp::default();
    let report = project::BuildReport {
        outcome: BuildOutcome::Succeeded(graph_path.clone()),
        label: Some(project::LabelOutcome::Labeled),
    };
    project::apply_build_report(&mut app, report);

    assert!(app.model.is_some(), "the graph must load");
    assert!(
        app.banner.is_none(),
        "a successful naming step must add no banner, got {:?}",
        app.banner
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_skipped_naming_note_is_appended_to_a_dropped_edge_warning() {
    let _guard = test_lock().lock().unwrap_or_else(|e| e.into_inner());
    project::clear_pending();

    const DROPPED_EDGES_PLURAL: &str = include_str!("fixtures/dropped_edges_plural.json");

    let dir = scratch_dir("skipped-naming-with-warning");
    let out_dir = dir.join("graphify-out");
    std::fs::create_dir_all(&out_dir).expect("must create scratch graphify-out dir");
    let graph_path = out_dir.join("graph.json");
    std::fs::write(&graph_path, DROPPED_EDGES_PLURAL).expect("must write graph.json");

    // Independently derive the exact warning body load.rs produces for this
    // fixture, rather than hardcoding its wording here (which belongs to
    // load.rs and must not get a second copy).
    let mut baseline_app = SeamExplorerApp::default();
    seam_explorer_egui::startup::preload_graph(&mut baseline_app, &graph_path);
    let original_body = baseline_app
        .banner
        .clone()
        .expect("this fixture must produce a dropped-edge warning banner")
        .body;

    let mut app = SeamExplorerApp::default();
    let report = project::BuildReport {
        outcome: BuildOutcome::Succeeded(graph_path.clone()),
        label: Some(project::LabelOutcome::Skipped(
            "no LLM backend configured".to_string(),
        )),
    };
    project::apply_build_report(&mut app, report);

    match &app.banner {
        Some(banner) => {
            assert_eq!(
                banner.kind,
                BannerKind::Warning,
                "the dropped-edge warning must survive, got {:?}",
                banner.kind
            );
            assert!(
                banner.body.contains(&original_body),
                "merged body must still contain the original warning body, got {:?}",
                banner.body
            );
            assert!(
                banner.body.contains("no LLM backend configured"),
                "merged body must also carry the naming note, got {:?}",
                banner.body
            );
        }
        None => panic!("expected Some(Warning banner with the note appended)"),
    }

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_prompt_offers_to_build_and_names_the_directory() {
    let _guard = test_lock().lock().unwrap_or_else(|e| e.into_inner());
    project::clear_pending();

    let dir = scratch_dir("prompt-ui");
    std::fs::create_dir_all(&dir).expect("must create scratch dir");

    let mut app = SeamExplorerApp::default();
    project::open_project_dir(&mut app, &dir);

    let mut harness = egui_kittest::Harness::new_ui_state(
        |ui, app: &mut SeamExplorerApp| {
            let ctx = ui.ctx().clone();
            project::poll_and_prompt(&ctx, app);
        },
        app,
    );
    harness.run();

    harness.get_by_label_contains("Build graph");
    harness.get_by_label_contains("Cancel");

    project::clear_pending();
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn cancelling_the_prompt_disarms_it() {
    let _guard = test_lock().lock().unwrap_or_else(|e| e.into_inner());
    project::clear_pending();

    let dir = scratch_dir("prompt-cancel");
    std::fs::create_dir_all(&dir).expect("must create scratch dir");

    let mut app = SeamExplorerApp::default();
    project::open_project_dir(&mut app, &dir);

    let mut harness = egui_kittest::Harness::new_ui_state(
        |ui, app: &mut SeamExplorerApp| {
            let ctx = ui.ctx().clone();
            project::poll_and_prompt(&ctx, app);
        },
        app,
    );
    harness.run();
    harness.get_by_label_contains("Cancel").click();
    harness.run();

    assert_eq!(
        project::pending_prompt_dir(),
        None,
        "clicking Cancel must disarm the prompt"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// Environment-dependent, and honest about it: this is the one test in the
/// file that depends on a user-installed graphify binary. If run_build
/// itself reports that graphify could not be spawned at all, that specific
/// failure is treated as an inconclusive skip rather than a test failure --
/// every other failure still fails the test.
#[test]
fn real_graphify_extract_produces_a_seam_core_loadable_graph() {
    let _guard = test_lock().lock().unwrap_or_else(|e| e.into_inner());
    project::clear_pending();

    let dir = scratch_dir("real-graphify");
    let src = dir.join("src");
    std::fs::create_dir_all(&src).expect("must create scratch src dir");
    std::fs::write(
        src.join("a.py"),
        "def alpha():\n    return beta()\n\n\ndef beta():\n    return 1\n",
    )
    .expect("must write a.py");
    std::fs::write(
        src.join("b.py"),
        "from a import alpha\n\n\ndef gamma():\n    return alpha()\n",
    )
    .expect("must write b.py");

    let argv = project::build_command(&dir);
    let graph_path = project::graph_path_for(&dir);
    let outcome = project::run_build(&argv, &graph_path);

    match outcome {
        BuildOutcome::Failed(msg) if msg.contains("could not run") => {
            eprintln!(
                "SKIP: graphify could not be run on this machine ({msg}) -- real_graphify_extract_produces_a_seam_core_loadable_graph is inconclusive, not failing"
            );
        }
        BuildOutcome::Failed(msg) => panic!("graphify extract failed: {msg}"),
        BuildOutcome::Succeeded(path) => {
            let json = std::fs::read_to_string(&path).expect("graph.json must be readable");
            let ingest = load::read_and_ingest(&json).expect("graphify's own output must ingest");
            println!(
                "real_graphify_extract_produces_a_seam_core_loadable_graph: {} nodes",
                ingest.model.graph.node_count()
            );
            assert!(ingest.model.graph.node_count() > 0);
        }
    }

    let _ = std::fs::remove_dir_all(&dir);
}
