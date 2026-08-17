//! Plan 08 gap closure (G-05-2/G-05-3/G-05-4): `egui_kittest` integration
//! coverage driving synthetic pointer/wheel/keyboard input through
//! `graph_view::show` and asserting on the transform the widget actually
//! renders with -- the missing coverage class that let all three gaps ship
//! (unit tests on `keyboard::apply_key`/`graph_view::view_to_frame` passed
//! while the live wiring connecting them to the rendered canvas was
//! entirely absent; see `.planning/debug/keyboard-pan-not-visible.md`,
//! `.planning/debug/camera-zoom-reset-not-responding.md`,
//! `.planning/debug/drag-to-trace-not-intercepting-gesture.md`).
//!
//! Recipe (planner probe, `05-08-PLAN.md`): `Harness::new_ui_state` with
//! `SeamExplorerApp` as state (so `harness.state()`/`state_mut()` reads and
//! drives `app.view` directly, exactly like `tests/panels.rs`'s existing
//! click tests), `run_steps(n)` to settle past the widget's unconditional
//! first-frame fit (bare `run()` panics on this widget -- it requests a
//! repaint every frame for its layout animation), and a `Rc<RefCell<...>>`
//! mirror written from inside the closure to read the persisted
//! `MetadataFrame` back out after each step (it lives in `ui.data`, only
//! reachable with a live `Ui`).

use std::cell::RefCell;
use std::rc::Rc;

use egui_kittest::kittest::Queryable;
use egui_kittest::Harness;
use seam_explorer_egui::app::{FocusState, SeamExplorerApp, ViewState};
use seam_explorer_egui::layout::SeamLayoutState;
use seam_explorer_egui::{graph_view, keyboard, panels, trace};

const CLEAN_FIXTURE: &str = include_str!("../../seam-core/tests/fixtures/clean.json");

/// A small real graph (6 nodes, 3 communities) so `graph_view::show` renders
/// an actual `GraphView` widget instead of the "no graph loaded" placeholder.
fn build_test_app() -> SeamExplorerApp {
    let outcome = seam_explorer_egui::load::read_and_ingest(CLEAN_FIXTURE)
        .expect("fixture must ingest cleanly");
    SeamExplorerApp {
        model: Some(outcome.model),
        seams: outcome.seams,
        ..Default::default()
    }
}

/// Builds a harness rendering `graph_view::show` with `SeamExplorerApp` as
/// state, plus a mirror of the persisted `MetadataFrame` written after every
/// `show()` call so tests can assert on the widget's actual rendered
/// transform after stepping.
fn canvas_harness() -> (
    Harness<'static, SeamExplorerApp>,
    Rc<RefCell<Option<egui_graphs::MetadataFrame>>>,
) {
    let app = build_test_app();
    let mirror: Rc<RefCell<Option<egui_graphs::MetadataFrame>>> = Rc::new(RefCell::new(None));
    let mirror_inner = mirror.clone();
    let harness = Harness::new_ui_state(
        move |ui, app: &mut SeamExplorerApp| {
            graph_view::show(ui, app);
            *mirror_inner.borrow_mut() = Some(egui_graphs::MetadataFrame::new(None).load(ui));
        },
        app,
    );
    (harness, mirror)
}

/// Primary-button drag recipe from the planner's probe: `PointerMoved` to
/// (200,200), `PointerButton{pressed:true}`, `PointerMoved` to (260,240),
/// `PointerButton{pressed:false}` -- measured to pan the widget's own
/// `MetadataFrame` by exactly `(+60, +40)` when trace mode is off.
fn synthetic_drag(harness: &mut Harness<'static, SeamExplorerApp>) {
    harness
        .input_mut()
        .events
        .push(egui::Event::PointerMoved(egui::pos2(200.0, 200.0)));
    harness.step();
    harness.input_mut().events.push(egui::Event::PointerButton {
        pos: egui::pos2(200.0, 200.0),
        button: egui::PointerButton::Primary,
        pressed: true,
        modifiers: egui::Modifiers::default(),
    });
    harness.step();
    harness
        .input_mut()
        .events
        .push(egui::Event::PointerMoved(egui::pos2(260.0, 240.0)));
    harness.step();
    harness.input_mut().events.push(egui::Event::PointerButton {
        pos: egui::pos2(260.0, 240.0),
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers: egui::Modifiers::default(),
    });
    harness.step();
}

/// `sync_view_into_frame` writes `app.view` into the persisted
/// `MetadataFrame` (via `view_to_frame`), and a second call with an
/// unchanged view is a no-op (the no-drift guarantee, T-05-08-03).
#[test]
fn sync_writes_app_view_into_metadata_frame() {
    let viewport = egui::vec2(800.0, 600.0);
    let view = ViewState {
        zoom: 1.5,
        pan: egui::vec2(20.0, -10.0),
    };

    // `Harness::new_ui`'s construction runs the closure at least twice
    // internally (an initial frame, then `run_ok()`'s stabilization pass) --
    // record every invocation's outcome and assert on the *first*, since a
    // later invocation would already find the frame synced from the one
    // before it.
    let invocations: Rc<RefCell<Vec<(bool, bool)>>> = Rc::new(RefCell::new(Vec::new()));
    let invocations_inner = invocations.clone();

    let _harness = Harness::new_ui(move |ui| {
        let first = graph_view::sync_view_into_frame(ui, view, viewport);
        let second = graph_view::sync_view_into_frame(ui, view, viewport);
        invocations_inner.borrow_mut().push((first, second));

        let frame = egui_graphs::MetadataFrame::new(None).load(ui);
        let (expected_zoom, expected_pan) = graph_view::view_to_frame(view, viewport);
        assert!(
            (frame.zoom - expected_zoom).abs() < 1e-4,
            "frame zoom must equal view_to_frame(view).0 after sync"
        );
        assert!(
            (frame.pan - expected_pan).length() < 1e-3,
            "frame pan must equal view_to_frame(view).1 after sync"
        );
    });

    let recorded = invocations.borrow();
    let (first_wrote, second_wrote) = *recorded
        .first()
        .expect("closure must have run at least once during construction");
    assert!(
        first_wrote,
        "first sync (frame differs from the default) must write"
    );
    assert!(
        !second_wrote,
        "second sync with an unchanged view must be a no-op (no-drift guarantee)"
    );
}

/// The direct regression test for G-05-3: applying `keyboard::apply_key`'s
/// `PanLeft` to `app.view` (exactly what `keyboard::handle` does) must move
/// the *rendered* frame's pan by +40 screen px in x -- not just `app.view`
/// itself (already covered by `keyboard.rs`'s own unit tests, which is why
/// this gap shipped despite passing unit tests).
#[test]
fn arrow_key_pan_moves_rendered_frame() {
    let (mut harness, mirror) = canvas_harness();
    harness.run_steps(3);

    let before = mirror
        .borrow()
        .clone()
        .expect("frame must be mirrored after settling");

    let current_view = harness.state().view;
    let panned = keyboard::apply_key(current_view, keyboard::KeyAction::PanLeft);
    harness.state_mut().view = panned;
    harness.step();

    let after = mirror
        .borrow()
        .clone()
        .expect("frame must be mirrored after stepping");

    assert!(
        (after.pan.x - before.pan.x - 40.0).abs() < 1e-2,
        "arrow-key pan must move the rendered frame by +40 screen px in x, got {} -> {} (delta {})",
        before.pan.x,
        after.pan.x,
        after.pan.x - before.pan.x
    );
}

/// The return leg that makes Reset real (G-05-2's reset half): a mouse drag
/// must update `app.view`, not leave it dead the way it was before this
/// plan (mouse pan/zoom previously mutated only the widget's own internal
/// `MetadataFrame`, never `app.view`).
#[test]
fn mouse_drag_updates_app_view() {
    let (mut harness, _mirror) = canvas_harness();
    harness.run_steps(3);

    let before_view = harness.state().view;
    synthetic_drag(&mut harness);
    let after_view = harness.state().view;

    assert!(
        (after_view.pan - before_view.pan).length() > 1e-2
            || (after_view.zoom - before_view.zoom).abs() > 1e-4,
        "a mouse drag must update app.view (the read-back leg), got before={before_view:?} after={after_view:?}"
    );
}

/// The direct regression test for G-05-2's reset half: dragging the canvas
/// off-centre, then simulating the frozen top-bar "Reset view" button / `0`
/// key (both of which assign `app.view = ViewState::default()`), must
/// re-frame the whole graph -- not leave `app.view` sitting at the sentinel
/// default (the pre-fix no-op) and not leave the rendered frame unchanged.
#[test]
fn reset_sentinel_refits_the_graph() {
    let (mut harness, mirror) = canvas_harness();
    harness.run_steps(3);

    synthetic_drag(&mut harness);
    let panned_frame = mirror
        .borrow()
        .clone()
        .expect("frame must be mirrored after drag");

    // Simulate the frozen top-bar "Reset view" button / the `0` key.
    harness.state_mut().view = ViewState::default();
    harness.run_steps(3);

    let after_view = harness.state().view;
    let after_frame = mirror
        .borrow()
        .clone()
        .expect("frame must be mirrored after reset");

    let default_view = ViewState::default();
    assert!(
        (after_view.zoom - default_view.zoom).abs() > 1e-4
            || (after_view.pan - default_view.pan).length() > 1e-2,
        "reset must re-fit app.view to the graph, not leave it at the sentinel default"
    );
    assert!(
        (after_frame.pan - panned_frame.pan).length() > 1e-2
            || (after_frame.zoom - panned_frame.zoom).abs() > 1e-4,
        "reset must change the rendered frame's pan/zoom from the panned state"
    );
}

/// A plain wheel/two-finger scroll over the canvas must zoom it (G-05-2's
/// zoom half) -- `egui_graphs::handle_zoom` only reads `zoom_delta()`,
/// populated exclusively by a genuine pinch or Ctrl+scroll, so this
/// exercises `graph_view::apply_scroll_zoom`'s live wiring in `show()`.
#[test]
fn plain_scroll_zooms_the_canvas() {
    let (mut harness, mirror) = canvas_harness();
    harness.run_steps(3);

    let before = mirror
        .borrow()
        .clone()
        .expect("frame must be mirrored after settling");

    harness
        .input_mut()
        .events
        .push(egui::Event::PointerMoved(egui::pos2(400.0, 300.0)));
    harness.step();
    harness.input_mut().events.push(egui::Event::MouseWheel {
        unit: egui::MouseWheelUnit::Line,
        delta: egui::vec2(0.0, 3.0),
        phase: egui::TouchPhase::Move,
        modifiers: egui::Modifiers::default(),
    });
    harness.step();
    harness.step();

    let after = mirror
        .borrow()
        .clone()
        .expect("frame must be mirrored after scroll");

    assert!(
        after.zoom > before.zoom,
        "a plain scroll over the canvas must zoom in, got before={} after={}",
        before.zoom,
        after.zoom
    );
}

/// The direct regression test for G-05-4: with Trace mode on, a
/// primary-button drag must leave the rendered frame's pan unchanged --
/// the gesture belongs to `handle_trace_gesture`, not `egui_graphs`' own
/// internal pan handling.
#[test]
fn trace_mode_drag_does_not_pan_canvas() {
    let (mut harness, mirror) = canvas_harness();
    harness.state_mut().trace_mode = true;
    harness.run_steps(3);

    let before = mirror
        .borrow()
        .clone()
        .expect("frame must be mirrored after settling");
    synthetic_drag(&mut harness);
    let after = mirror
        .borrow()
        .clone()
        .expect("frame must be mirrored after drag");

    assert!(
        (after.pan - before.pan).length() < 1e-3,
        "trace-mode drag must not pan the canvas, got before={:?} after={:?}",
        before.pan,
        after.pan
    );
}

/// Control for `trace_mode_drag_does_not_pan_canvas`: with Trace mode off,
/// the same drag must still pan the canvas by the drag delta (the probe
/// measured exactly `+60, +40` for a 200,200 -> 260,240 drag).
#[test]
fn drag_pans_canvas_when_trace_mode_off() {
    let (mut harness, mirror) = canvas_harness();
    harness.run_steps(3);

    let before = mirror
        .borrow()
        .clone()
        .expect("frame must be mirrored after settling");
    synthetic_drag(&mut harness);
    let after = mirror
        .borrow()
        .clone()
        .expect("frame must be mirrored after drag");

    let delta = after.pan - before.pan;
    assert!(
        (delta.x - 60.0).abs() < 1.0 && (delta.y - 40.0).abs() < 1.0,
        "drag must pan the canvas by the drag delta when trace mode is off, got delta={delta:?}"
    );
}

// ============================================================
// 05-10 Task 2: the live-wiring integration tests proving `show()` itself
// -- not just the pure `hiding_active`/`node_visible` helpers -- applies
// the hiding filter. This is precisely the failure class that let G-05-2,
// G-05-3 and G-05-4 all ship green (a passing unit test with an unwired
// live call site), so these two tests exercise the real rendered canvas.
// ============================================================

/// The direct regression test for DP-10-02/DP-10-03: with `app.focus` set
/// to two of `clean.json`'s three communities BEFORE the first frame, the
/// persisted layout state must only ever have been populated with those
/// two communities' nodes -- the number positioned equals exactly that
/// count, and is strictly less than the model's total node count. Setting
/// focus before the first frame matters: it guarantees the persisted
/// position map was never populated with the excluded nodes on an earlier
/// frame, so the assertion cannot pass or fail for a stale-state reason.
#[test]
fn focused_canvas_positions_only_the_focused_communities() {
    let mut app = build_test_app();
    app.focus = Some(FocusState {
        a: "A".to_string(),
        b: "B".to_string(),
    });

    let model = app.model.as_ref().expect("test app has a model");
    let total_nodes = model.graph.node_count();
    let focused_nodes = model
        .graph
        .node_indices()
        .filter(|&idx| {
            let community = &model.graph[idx].community;
            community == "A" || community == "B"
        })
        .count();

    let positioned_count: Rc<RefCell<usize>> = Rc::new(RefCell::new(0));
    let positioned_inner = positioned_count.clone();
    let mut harness = Harness::new_ui_state(
        move |ui, app: &mut SeamExplorerApp| {
            graph_view::show(ui, app);
            let state = egui_graphs::get_layout_state::<SeamLayoutState>(ui, None);
            *positioned_inner.borrow_mut() = state.positions().len();
        },
        app,
    );
    harness.run_steps(3);

    let positioned = *positioned_count.borrow();
    assert_eq!(
        positioned, focused_nodes,
        "positioned node count must equal only the two focused communities' nodes, \
         got {positioned} positioned vs {focused_nodes} expected (model total {total_nodes})"
    );
    assert!(
        positioned < total_nodes,
        "positioned node count ({positioned}) must be strictly less than the model's \
         total ({total_nodes}) -- hiding must actually be excluding the third community"
    );
}

/// The direct regression test for DP-10-03 case (c): a fresh harness with
/// both `app.focus` set and a resolved `app.trace` present positions every
/// node in the model -- proving the trace-result suspension is wired into
/// the live `show()` path, not merely unit-tested against `hiding_active`
/// in isolation.
#[test]
fn trace_result_restores_the_whole_canvas() {
    let mut app = build_test_app();
    app.focus = Some(FocusState {
        a: "A".to_string(),
        b: "B".to_string(),
    });

    let model = app.model.as_ref().expect("test app has a model");
    let total_nodes = model.graph.node_count();
    let resolved = trace::run(model, "a1", "c1");
    assert!(
        resolved.path.is_some(),
        "fixture must have a directed path from a1 to c1 for this test to be meaningful"
    );
    app.trace = Some(resolved);

    let positioned_count: Rc<RefCell<usize>> = Rc::new(RefCell::new(0));
    let positioned_inner = positioned_count.clone();
    let mut harness = Harness::new_ui_state(
        move |ui, app: &mut SeamExplorerApp| {
            graph_view::show(ui, app);
            let state = egui_graphs::get_layout_state::<SeamLayoutState>(ui, None);
            *positioned_inner.borrow_mut() = state.positions().len();
        },
        app,
    );
    harness.run_steps(3);

    let positioned = *positioned_count.borrow();
    assert_eq!(
        positioned, total_nodes,
        "a resolved trace result must suspend hiding and position every node in the model, \
         got {positioned} positioned vs {total_nodes} expected"
    );
}

// ============================================================
// 05-12 Task 2: live-wiring proof that `show()` itself paints the canvas
// side labels, not just that the pure `overlay::side_labels` helper
// computes them correctly in isolation -- the same failure class 05-10's
// two tests above exist to close (a passing unit test with an unwired live
// call site).
// ============================================================

/// Builds a focused-and-detailed test app: `app.focus` set to two of
/// `clean.json`'s three communities and `app.detail` populated via a real
/// `seam_core::seam_detail` call, so `show()`'s focused-overlay block
/// (which requires both `Some`) actually executes its paint calls.
fn build_focused_test_app() -> SeamExplorerApp {
    let mut app = build_test_app();
    let focus = FocusState {
        a: "A".to_string(),
        b: "B".to_string(),
    };
    let detail = {
        let model = app.model.as_ref().expect("test app has a model");
        let scc = model
            .scc
            .as_ref()
            .expect("read_and_ingest/finalize_scc must have run");
        seam_core::seam_detail(model, scc, &focus.a, &focus.b)
    };
    app.focus = Some(focus);
    app.detail = Some(detail);
    app
}

/// Counts painted `Shape::Text` entries whose color is exactly one of the
/// two side tints (`--sideA`/`--sideB`, `overlay::SIDE_A_HEX`/`SIDE_B_HEX`).
/// This is a strictly stronger, fixture-independent replacement for a raw
/// total-shape-count comparison: a raw count ties or goes negative on
/// `clean.json` (hiding its third community removes as many or more shapes
/// than the fault line/threads/labels add back, confirmed empirically
/// against all three of its seam pairs, and the same "more shapes when
/// focused" outcome already held true from the pre-existing fault-line and
/// crossing-thread overlay alone -- it would pass even with
/// `paint_side_labels` never wired in). Filtering on the exact side tints
/// is precise instead: no other painted element in this app ever draws
/// `Shape::Text` in `SIDE_A_HEX`/`SIDE_B_HEX` (node fills use those tints
/// but as `Shape::Circle`, node/edge label text uses `TEXT_HEX`/the edge's
/// own color) -- so a nonzero count can only come from `paint_side_labels`.
fn count_side_tinted_text_shapes(output: &egui::FullOutput) -> usize {
    let side_a = egui::Color32::from_hex("#38d6c4").expect("valid hex");
    let side_b = egui::Color32::from_hex("#f2a63c").expect("valid hex");
    output
        .shapes
        .iter()
        .filter(|clipped| {
            matches!(
                &clipped.shape,
                egui::Shape::Text(text) if text.fallback_color == side_a || text.fallback_color == side_b
            )
        })
        .count()
}

/// The direct regression test for the wiring class of defect this whole
/// plan's `tdd_discipline` calls out (three prior UAT gaps shipped exactly
/// this way): rendering `graph_view::show` with no focus must paint zero
/// side-tinted text shapes, and rendering it with a real focus+detail set
/// (exercising `show()`'s existing focused-overlay block, exactly as the
/// live app does when a seam row is clicked) must paint exactly two --
/// proving `paint_side_labels`'s call site actually executes from the real
/// render path, not merely that the pure `side_labels` helper computes the
/// right values in isolation.
#[test]
fn focused_canvas_paints_two_more_shapes_than_unfocused() {
    let mut harness_unfocused = Harness::new_ui_state(
        move |ui, app: &mut SeamExplorerApp| {
            graph_view::show(ui, app);
        },
        build_test_app(),
    );
    harness_unfocused.run_steps(3);
    let unfocused_side_labels = count_side_tinted_text_shapes(harness_unfocused.output());

    let mut harness_focused = Harness::new_ui_state(
        move |ui, app: &mut SeamExplorerApp| {
            graph_view::show(ui, app);
        },
        build_focused_test_app(),
    );
    harness_focused.run_steps(3);
    let focused_side_labels = count_side_tinted_text_shapes(harness_focused.output());

    assert_eq!(
        unfocused_side_labels, 0,
        "an unfocused canvas must paint no side-tinted label text"
    );
    assert_eq!(
        focused_side_labels, 2,
        "a focused canvas must paint exactly two side-tinted label text shapes, got \
         {focused_side_labels}"
    );
}

// ============================================================
// 05-15 Task 1 (RED): the click-driven end-to-end regression test proving
// focusing a seam auto-frames it -- no Reset-view press required (the
// user's exact complaint: "I need to press reset view to get it centered.
// Can these be combined."). Uses the first harness in this file that
// renders both the seam-list panel and the canvas over one app, so the
// click is a REAL row click through a REAL rendered panel and the framing
// comparison reads the REAL rendered canvas transform.
// ============================================================

/// Fixed left-column width the combined harness gives `seam_list::show`,
/// leaving the remainder to `graph_view::show` -- mirrors the app's own
/// panel-then-canvas order (`app.rs`'s frozen panel dispatch).
const COMBINED_PANEL_WIDTH: f32 = 300.0;

/// Builds a harness rendering `seam_list::show` (fixed-width left column)
/// and `graph_view::show` (remaining width) over one `SeamExplorerApp`, at
/// 1200x800 -- the same canvas dimensions the planner probe in
/// `05-15-PLAN.md`'s `<design_decision>` measured against, so the follow's
/// constants describe the same geometry this harness exercises. Sized via
/// `Harness::builder().with_size(...)` rather than `Harness::new_ui_state`,
/// whose default 800x600 screen rect leaves the canvas too narrow once a
/// panel-width column is taken out of it. The row is built via
/// `allocate_ui_with_layout` over the full available size (not a bare
/// `ui.horizontal`, whose height auto-sizes to content) so `graph_view`'s
/// `canvas_rect` inherits the full 800px height, not a row-height sliver.
fn combined_harness() -> (
    Harness<'static, SeamExplorerApp>,
    Rc<RefCell<Option<egui_graphs::MetadataFrame>>>,
) {
    let app = build_test_app();
    let mirror: Rc<RefCell<Option<egui_graphs::MetadataFrame>>> = Rc::new(RefCell::new(None));
    let mirror_inner = mirror.clone();
    let harness = Harness::builder()
        .with_size(egui::vec2(1200.0, 800.0))
        .build_ui_state(
            move |ui, app: &mut SeamExplorerApp| {
                let available = ui.available_size();
                ui.allocate_ui_with_layout(
                    available,
                    egui::Layout::left_to_right(egui::Align::Min),
                    |ui| {
                        ui.allocate_ui_with_layout(
                            egui::vec2(COMBINED_PANEL_WIDTH, ui.available_height()),
                            egui::Layout::top_down(egui::Align::Min),
                            |ui| {
                                panels::seam_list::show(ui, app);
                            },
                        );
                        graph_view::show(ui, app);
                    },
                );
                *mirror_inner.borrow_mut() = Some(egui_graphs::MetadataFrame::new(None).load(ui));
            },
            app,
        );
    (harness, mirror)
}

/// Tolerance the automatic (click-driven) framing must match the manual
/// (post-Reset-view) framing within. Sized from the planner probe
/// (`05-15-PLAN.md` `<behavior>`): after convergence the layout still drifts
/// slowly, so two measurements taken well apart in frame count differ by
/// roughly 1% of extent -- these tolerances sit comfortably above that noise
/// floor and far below the defect being caught, where the pre-fix automatic
/// view sits at the fixed `JUMP_ZOOM` (1.6) against a fitted zoom near 0.77
/// (a ~50% gap) and a pan of `ZERO` against a fitted pan over 100px away.
const FOCUS_FIT_ZOOM_REL_TOLERANCE: f32 = 0.05;
const FOCUS_FIT_PAN_TOLERANCE: f32 = 25.0;

/// The direct regression test for the user's reported defect: clicking the
/// top-ranked seam row must, by itself, land the camera where pressing
/// Reset view afterward would land it -- no manual second step. A guard
/// assertion up front confirms the click actually set `app.focus`, so a
/// selector that silently matched nothing cannot masquerade as a framing
/// failure.
#[test]
fn focus_click_frames_the_seam_without_pressing_reset_view() {
    let (mut harness, _mirror) = combined_harness();
    // Settle past the widget's unconditional first-frame fit before the
    // click -- the pull-apart settling window itself only begins once a
    // seam is focused, below.
    harness.run_steps(5);

    // `clean.json`'s A<->B pair has the most crossings (4, every other pair
    // has fewer) and carries no `community_name` fields, so `seam_display_name`
    // falls back to the raw ids and the top-ranked row renders as "A <-> B".
    harness.get_by_label_contains("A \u{2194} B").click();
    harness.step();

    let focus = harness.state().focus.clone().expect(
        "clicking the top seam row must set focus -- a selector that matched nothing cannot \
         masquerade as a framing failure",
    );
    assert_eq!(focus.a, "A");
    assert_eq!(focus.b, "B");

    // Step well beyond the settling window the planner probe measured
    // (~30-40 frames) before recording the automatic result.
    harness.run_steps(150);
    let automatic = harness.state().view;

    // Simulate the frozen top-bar "Reset view" button / the `0` key -- the
    // exact simulation `reset_sentinel_refits_the_graph` already uses.
    harness.state_mut().view = ViewState::default();
    harness.run_steps(150);
    let manual = harness.state().view;

    let zoom_rel_diff = (automatic.zoom - manual.zoom).abs() / manual.zoom.max(1e-6);
    let pan_diff = (automatic.pan - manual.pan).length();

    assert!(
        zoom_rel_diff <= FOCUS_FIT_ZOOM_REL_TOLERANCE && pan_diff <= FOCUS_FIT_PAN_TOLERANCE,
        "clicking the seam row must frame it the same way pressing Reset view does, with no \
         manual second step required: automatic view={automatic:?}, manual (post-Reset) \
         view={manual:?}, zoom relative diff={zoom_rel_diff} (tolerance \
         {FOCUS_FIT_ZOOM_REL_TOLERANCE}), pan diff={pan_diff}px (tolerance \
         {FOCUS_FIT_PAN_TOLERANCE}px)"
    );
}

// ============================================================
// 05-15 Task 3: user takeover cancels the refit follow, and the follow
// always yields to a live gesture. Uses the combined harness from Task 1
// so the cancellation stimuli (drag/scroll) land on the real rendered
// canvas, exactly as a user's mouse would.
// ============================================================

/// A drag scoped to the combined harness's canvas region -- which starts at
/// `x = COMBINED_PANEL_WIDTH`, not `0`, once the seam-list panel takes its
/// own column -- so the press doesn't land on the panel instead. Same
/// recipe and measured `+60, +40` screen-px delta as `synthetic_drag`
/// above, just offset into the canvas.
fn synthetic_canvas_drag(harness: &mut Harness<'static, SeamExplorerApp>) {
    let start = egui::pos2(COMBINED_PANEL_WIDTH + 200.0, 200.0);
    let end = egui::pos2(COMBINED_PANEL_WIDTH + 260.0, 240.0);
    harness
        .input_mut()
        .events
        .push(egui::Event::PointerMoved(start));
    harness.step();
    harness.input_mut().events.push(egui::Event::PointerButton {
        pos: start,
        button: egui::PointerButton::Primary,
        pressed: true,
        modifiers: egui::Modifiers::default(),
    });
    harness.step();
    harness
        .input_mut()
        .events
        .push(egui::Event::PointerMoved(end));
    harness.step();
    harness.input_mut().events.push(egui::Event::PointerButton {
        pos: end,
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers: egui::Modifiers::default(),
    });
    harness.step();
}

/// A pan during an active follow must cancel it immediately: the user's pan
/// survives, and the camera does not snap back toward the fitted transform
/// on a later frame. Sized off the drag's own magnitude (it pans the frame
/// by ~60px in x) -- a still-running follow yanking the camera back toward
/// the fit would move the view by tens more px after the drag ends; a
/// correctly-cancelled follow leaves it alone.
#[test]
fn drag_during_follow_cancels_it_and_the_pan_survives() {
    let (mut harness, _mirror) = combined_harness();
    harness.run_steps(5);

    harness.get_by_label_contains("A \u{2194} B").click();
    harness.step();
    assert!(
        harness.state().focus.is_some(),
        "the click must have set focus before the follow-cancellation behaviour can be tested"
    );

    // Only a couple of frames -- the follow is still live and still moving
    // the camera (the planner probe's step-20 delta is ~3-4px, well above
    // FOLLOW_SETTLED_EPSILON).
    harness.run_steps(2);

    synthetic_canvas_drag(&mut harness);
    let after_drag = harness.state().view;

    // Several more frames -- a still-running follow would yank the camera
    // back toward the fitted transform; a cancelled one leaves it alone.
    harness.run_steps(10);
    let after_more_steps = harness.state().view;

    let drift = (after_more_steps.pan - after_drag.pan).length();
    assert!(
        drift < 15.0,
        "a drag during an active follow must cancel it -- the view must not keep moving after \
         the drag ends. Got after_drag={after_drag:?}, after_more_steps={after_more_steps:?}, \
         drift={drift}px"
    );
}

/// The same cancellation guarantee for a scroll-zoom stimulus instead of a
/// drag.
#[test]
fn scroll_zoom_during_follow_cancels_it() {
    let (mut harness, _mirror) = combined_harness();
    harness.run_steps(5);

    harness.get_by_label_contains("A \u{2194} B").click();
    harness.step();
    assert!(
        harness.state().focus.is_some(),
        "the click must have set focus before the follow-cancellation behaviour can be tested"
    );

    harness.run_steps(2);

    let scroll_pos = egui::pos2(COMBINED_PANEL_WIDTH + 200.0, 200.0);
    harness
        .input_mut()
        .events
        .push(egui::Event::PointerMoved(scroll_pos));
    harness.step();
    harness.input_mut().events.push(egui::Event::MouseWheel {
        unit: egui::MouseWheelUnit::Line,
        delta: egui::vec2(0.0, 3.0),
        phase: egui::TouchPhase::Move,
        modifiers: egui::Modifiers::default(),
    });
    harness.step();
    let after_scroll_zoom = harness.state().view.zoom;

    harness.run_steps(10);
    let after_more_steps = harness.state().view.zoom;

    let rel_drift = (after_more_steps - after_scroll_zoom).abs() / after_scroll_zoom.max(1e-6);
    assert!(
        rel_drift < 0.05,
        "a scroll-zoom during an active follow must cancel it -- zoom must not keep moving \
         after the scroll ends. Got after_scroll_zoom={after_scroll_zoom}, \
         after_more_steps={after_more_steps}, relative drift={rel_drift}"
    );
}
