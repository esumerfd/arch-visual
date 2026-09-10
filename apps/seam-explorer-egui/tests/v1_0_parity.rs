//! Plan 10-03 / ROADMAP SC-5: with no hook installed and no client ever
//! running, the app behaves exactly as v1.0 did.
//!
//! **The container IS the mechanism.** Nothing anywhere in this test binary
//! binds a socket, serves a receiver, or spawns a receive thread. There is no
//! `bind_at` call, no `bind_default` call, no `serve` call, no
//! `spawn_receiver` call, no `mod common;`, and no client subprocess. The
//! process-global `event_stream::RECEIVER` therefore stays `None` for the
//! entire run of every test in this file, so `event_stream::drain()` takes its
//! `None` branch and returns an empty `Vec` on every frame of every test, and
//! `history::drain_and_apply` is a no-op by construction rather than by
//! arrangement. That is what makes the parity claim here STRUCTURAL: it is a
//! fact about the process, not a per-test setup a later edit could quietly
//! undo. `nothing_in_this_process_ever_binds_serves_or_receives` is the first
//! test in the file so a reader meets the guard before the claims that rest on
//! it; if someone adds a socket here, that test fails and the parity claim
//! correctly stops being made.
//!
//! **Why the small duplication is deliberate.** This file does NOT
//! `mod common;`. The shared module (plan 10-01) exists to locate and BUILD
//! the cross-crate hook client — exactly the capability whose absence is this
//! file's entire claim. Importing it would put a client build one careless line
//! away from a binary that asserts no client ever runs here. A file-local
//! `include_str!` of the fixture plus a four-line loader is the price of that
//! isolation, and it is worth paying.
//!
//! **The oracle is outside the app under test.** Every parity assertion below
//! compares the app against a SECOND, independent `load::read_and_ingest` of
//! the same fixture bytes (`oracle_model`), never against values read back out
//! of the app. A parity test that compares the app with itself passes against a
//! broken app, which is the failure mode `timeline_reconstruction.rs`'s
//! expectation discipline was written to prevent (T-10-03-02).
//!
//! **One honesty note on the surrounding suites.** `tests/panels.rs` is a
//! genuinely pure v1.0-era suite (zero references to the live machinery).
//! `tests/canvas.rs` is NOT: it grew a Phase-8 live-apply section that binds a
//! socket. Only its earlier portion is v1.0-era evidence. See 10-03-SUMMARY.md.

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::rc::Rc;

use egui_kittest::kittest::Queryable;
use egui_kittest::Harness;
use seam_explorer_egui::app::SeamExplorerApp;
use seam_explorer_egui::event_stream;
use seam_explorer_egui::panels::timeline as timeline_panel;
use seam_explorer_egui::timeline;
use seam_explorer_egui::trace::{self, TraceGesture};

/// The v1.0 fixture every test in this file loads: 6 nodes across three
/// communities (A: a1/a2, B: b1/b2, C: c1/c2), with real `source_file` values.
/// File-local on purpose — see this module's doc comment on isolation.
const SOURCE_PATHS_FIXTURE: &str = include_str!("../../seam-core/tests/fixtures/source_paths.json");

/// The app under test, built through the REAL load path (`read_and_ingest` +
/// `apply_load_outcome`) rather than field-by-field, because the load path is
/// where the replay baseline is captured and a hand-built app would have none.
fn loaded_app() -> SeamExplorerApp {
    let outcome = seam_explorer_egui::load::read_and_ingest(SOURCE_PATHS_FIXTURE)
        .expect("the v1.0 fixture must ingest cleanly");
    let mut app = SeamExplorerApp::default();
    app.apply_load_outcome(outcome);
    app
}

/// The INDEPENDENT oracle: a second, separate ingest of the same fixture
/// bytes. This is the whole point — every ranking/focus/trace expectation below
/// is computed against this model, so no assertion can be satisfied by the app
/// agreeing with itself.
fn oracle_model() -> seam_core::Model {
    seam_explorer_egui::load::read_and_ingest(SOURCE_PATHS_FIXTURE)
        .expect("the v1.0 fixture must ingest cleanly")
        .model
}

/// Node ids as a set, for the inertness assertions.
fn model_ids(model: &seam_core::Model) -> BTreeSet<String> {
    model.index.keys().cloned().collect()
}

/// Node ids read straight out of the fixture JSON — derived from the bytes, not
/// from any ingest, so it is usable as an expectation about an ingested model.
fn fixture_ids(fixture: &str) -> BTreeSet<String> {
    let doc: serde_json::Value = serde_json::from_str(fixture).expect("fixture must be valid JSON");
    doc["nodes"]
        .as_array()
        .expect("fixture must have a nodes array")
        .iter()
        .map(|n| {
            n["id"]
                .as_str()
                .expect("every fixture node must have a string id")
                .to_string()
        })
        .collect()
}

/// Drives the real assembled `eframe::App::ui` — every panel, the canvas, the
/// keyboard handler, and (crucially) the unconditional per-frame
/// `history::drain_and_apply` at the top of `ui()`. `Frame::_new_kittest()` is
/// eframe's own constructor for exactly this; `run_steps` rather than `run`
/// because `graph_view::show` renders through `egui_graphs`, whose layout keeps
/// requesting repaints, so running to quiescence would panic at `max_steps`.
/// Both notes are 09-02's findings (`timeline_reconstruction.rs`).
fn app_ui_harness(app: SeamExplorerApp) -> Harness<'static, SeamExplorerApp> {
    let mut frame = eframe::Frame::_new_kittest();
    Harness::new_ui_state(
        move |ui, app: &mut SeamExplorerApp| {
            <SeamExplorerApp as eframe::App>::ui(app, ui, &mut frame);
        },
        app,
    )
}

/// `app_ui_harness` plus a one-shot slot for planting a completed trace
/// gesture into egui's own temp storage just before `ui()` runs. 09-03's idiom:
/// `handle_trace_gesture` loads its gesture from `trace::load_gesture` every
/// frame and acts on a `Completed` one, so planting exactly the value the live
/// half records for the next frame reaches the real code path — chosen over
/// simulating pixel-perfect pointer geometry, which the 05-13 gap closure
/// showed to be fragile at this node radius.
fn traceable_app_harness() -> (
    Harness<'static, SeamExplorerApp>,
    Rc<RefCell<Option<TraceGesture>>>,
) {
    let plant: Rc<RefCell<Option<TraceGesture>>> = Rc::new(RefCell::new(None));
    let plant_inner = plant.clone();
    let mut frame = eframe::Frame::_new_kittest();
    let harness = Harness::new_ui_state(
        move |ui, app: &mut SeamExplorerApp| {
            if let Some(gesture) = plant_inner.borrow_mut().take() {
                trace::save_gesture(ui, gesture);
            }
            <SeamExplorerApp as eframe::App>::ui(app, ui, &mut frame);
        },
        loaded_app(),
    );
    (harness, plant)
}

// ---------------------------------------------------------------------
// The guard that makes everything below meaningful
// ---------------------------------------------------------------------

/// The structural premise of this whole file, asserted at runtime rather than
/// argued in prose: in this process the live machinery was never started, so
/// the drain is empty and all three counters read zero.
///
/// This is deliberately the FIRST test in the file. Its failure mode is the
/// valuable one: if a future edit adds a socket to this binary, the parity
/// claims here stop describing "a process with no live machinery at all" and
/// this test says so.
#[test]
fn nothing_in_this_process_ever_binds_serves_or_receives() {
    assert!(
        event_stream::drain().is_empty(),
        "no receiver was ever served in this process, so drain() must take its \
         None branch and return empty"
    );
    assert_eq!(
        event_stream::received_count(),
        0,
        "a process that never bound a socket cannot have received an event"
    );
    assert_eq!(
        event_stream::discarded_count(),
        0,
        "a process that never bound a socket cannot have discarded a datagram"
    );
    assert_eq!(
        event_stream::dropped_count(),
        0,
        "a process that never bound a socket cannot have dropped an event"
    );
}

// ---------------------------------------------------------------------
// SC-5's four named v1.0 behaviours
// ---------------------------------------------------------------------

/// SC-5, load + seam ranking. Drives the real assembled `ui()` for several
/// frames and compares `app.seams` against `seam_core::detect` over the
/// INDEPENDENT oracle, element for element IN ORDER.
///
/// Ordering is the subject, not an incidental detail: 09-01's tie-break fix
/// made `detect`'s ranking total-ordered, and a parity test comparing sets
/// would not notice a reordering — which is precisely the v1.0 regression a
/// user would see first (the wrong seam at the top of the list).
#[test]
fn the_seam_ranking_matches_a_freshly_ingested_graph() {
    let expected = seam_core::detect(&oracle_model());
    assert!(
        !expected.is_empty(),
        "guard: the oracle must rank at least one seam, or this comparison is vacuous"
    );

    let mut harness = app_ui_harness(loaded_app());
    harness.run_steps(3);

    let triple = |s: &seam_core::Seam| (s.a.clone(), s.b.clone(), s.crossings);
    let actual_order: Vec<_> = harness.state().seams.iter().map(triple).collect();
    let expected_order: Vec<_> = expected.iter().map(triple).collect();
    assert_eq!(
        actual_order, expected_order,
        "the running app's ranked seam list must equal a freshly ingested \
         graph's, element for element and in the same order"
    );
    assert_eq!(
        harness.state().seams,
        expected,
        "and the seams must be equal as whole values, not only in their \
         (a, b, crossings) projection"
    );
}

/// SC-5, seam focus + detail. Clicks a REAL seam row in the assembled app (the
/// `panels.rs::seam_row_click_sets_focus` idiom) and asserts both halves of
/// what that click is supposed to produce: the focus names that seam, and the
/// detail equals what `seam_core::seam_detail` produces for the same pair
/// against the ORACLE model.
///
/// The top-ranked row is clicked specifically, and its label is computed from
/// the oracle, so this proves the correct row maps to the correct seam rather
/// than that some click registers somewhere.
#[test]
fn a_seam_row_click_still_sets_focus_and_detail() {
    let oracle = oracle_model();
    let oracle_seams = seam_core::detect(&oracle);
    let top = oracle_seams
        .first()
        .expect("guard: the oracle must rank at least one seam to click")
        .clone();
    let row_label = format!(
        "{} \u{2194} {}",
        oracle.community_label(&top.a),
        oracle.community_label(&top.b)
    );
    let expected_detail = seam_core::seam_detail(
        &oracle,
        oracle
            .scc
            .as_ref()
            .expect("read_and_ingest finalizes the SCC index"),
        &top.a,
        &top.b,
    );

    let mut harness = app_ui_harness(loaded_app());
    harness.run_steps(3);
    assert!(
        harness.state().focus.is_none() && harness.state().detail.is_none(),
        "guard: a freshly loaded app must start with nothing focused"
    );

    harness.get_by_label(&row_label).click();
    harness.run_steps(3);

    let focus = harness
        .state()
        .focus
        .clone()
        .expect("clicking a seam row must set focus, exactly as in v1.0");
    assert_eq!(
        (focus.a, focus.b),
        (top.a.clone(), top.b.clone()),
        "the focused seam must be the one whose row was clicked"
    );
    let detail = harness
        .state()
        .detail
        .clone()
        .expect("clicking a seam row must also populate the detail panel");
    assert_eq!(
        detail, expected_detail,
        "the detail must match what a freshly ingested graph yields for the \
         same community pair"
    );
}

/// SC-5, drag-to-trace. Plants a real completed trace gesture between two
/// fixture nodes on opposite sides of a seam, runs real frames of the
/// assembled app, and compares the resolved path against
/// `seam_core::trace_path` over the INDEPENDENT oracle.
///
/// `a2 -> c1` deliberately: it is a multi-hop path (`a2 -> b1 -> c1`) crossing
/// two seams, where `a1 -> c1` would resolve through a single direct edge and
/// exercise no traversal at all.
///
/// Comparing against `trace::run` instead would be circular — `trace::run` is a
/// thin wrapper over `seam_core::trace_path` applied to `app.model`, so it
/// would assert the app agrees with itself. The oracle's separate ingest is
/// what makes this a parity claim rather than a tautology.
#[test]
fn drag_to_trace_still_produces_the_v1_0_path() {
    const FROM: &str = "a2";
    const TO: &str = "c1";

    let expected = seam_core::trace_path(&oracle_model(), FROM, TO)
        .expect("guard: the oracle must resolve a path, or this comparison is vacuous");
    assert!(
        expected.hops.len() > 2,
        "guard: {FROM} -> {TO} must be a multi-hop path, got {:?}",
        expected.hops
    );
    assert!(
        !expected.seams_crossed.is_empty(),
        "guard: the traced path must cross at least one seam"
    );

    let (mut harness, plant) = traceable_app_harness();
    harness.state_mut().trace_mode = true;
    harness.run_steps(3);
    assert!(
        harness.state().trace.is_none(),
        "guard: no trace may exist before the gesture is planted"
    );

    *plant.borrow_mut() = Some(TraceGesture::Completed {
        from: FROM.to_string(),
        to: TO.to_string(),
    });
    harness.run_steps(3);

    let state = harness.state();
    let result = state
        .trace
        .as_ref()
        .expect("a completed trace gesture must produce a trace result");
    assert_eq!((result.from.as_str(), result.to.as_str()), (FROM, TO));
    let path = result
        .path
        .as_ref()
        .expect("the traced pair must resolve to a path");
    assert_eq!(
        path.hops, expected.hops,
        "the hop list must equal a freshly ingested graph's shortest path"
    );
    assert_eq!(
        path.seams_crossed, expected.seams_crossed,
        "the crossed seams must match too, in traversal order"
    );
}

// ---------------------------------------------------------------------
// What v1.1 costs a user who never turns it on
// ---------------------------------------------------------------------

/// SC-5's other half: the v1.1 additions are provably INERT in a process where
/// no client ever runs. After driving several real frames of the assembled app
/// (each of which runs `history::drain_and_apply` unconditionally), the history
/// is empty, nothing is paused, the display model IS the live model, and the
/// bottom panel shows its empty state.
///
/// The honest statement of the cost is the last two assertions: one bottom
/// strip saying "No events yet", and one baseline clone captured at load. The
/// baseline exists — that is correct and costs nothing — but nothing has ever
/// been folded into it, so its node set is still exactly the fixture's.
#[test]
fn the_v1_1_additions_are_inert_when_no_client_ever_runs() {
    let mut harness = app_ui_harness(loaded_app());
    harness.run_steps(4);

    // The rendered bottom strip, before any borrow of the state.
    harness.get_by_label(timeline_panel::LIVE_BADGE);
    harness.get_by_label(timeline_panel::EMPTY_POSITION);
    assert_eq!(
        harness.query_all_by_label_contains("Event ").count(),
        0,
        "with no client ever running there is no event to be at, so the panel \
         must render its empty state rather than any \"Event N of M\""
    );

    let app = harness.state();

    assert_eq!(
        app.history.next_seq(),
        0,
        "no event was ever received, so no sequence number was ever issued"
    );
    assert!(app.history.is_empty(), "the history must hold nothing");

    assert_eq!(app.scrub_position, None, "the app must be Live, not Paused");
    assert!(
        !timeline::is_paused(app),
        "is_paused must agree with scrub_position being None"
    );

    let displayed = timeline::display_model(app).expect("a loaded app must display a model");
    let live = app
        .model
        .as_ref()
        .expect("a loaded app must retain a live model");
    assert_eq!(
        model_ids(displayed),
        model_ids(live),
        "display_model must return the LIVE model when nothing is paused"
    );
    assert_eq!(
        model_ids(displayed),
        fixture_ids(SOURCE_PATHS_FIXTURE),
        "and that model must still be exactly the fixture — no live event has \
         added, removed or renamed a node"
    );

    let baseline = app
        .replay_baseline
        .as_ref()
        .expect("the baseline is captured at load, which is correct and costs nothing");
    assert_eq!(
        model_ids(baseline),
        fixture_ids(SOURCE_PATHS_FIXTURE),
        "nothing has ever been folded into the baseline, so it is still the \
         graph as ingested — that clone is the entire storage cost of v1.1 to \
         a user who never turns it on"
    );
}
