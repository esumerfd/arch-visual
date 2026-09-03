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
// 05-27 Task 1 (RED): live-wiring proof of WINDOWS.md entry 5 / UAT test
// 12 -- `paint_crossing_threads` drew every crossing thread at a single
// fixed `canvas_rect.center().y`, so with more than one crossing edge
// between the focused pair, every thread visually collapsed onto one
// horizontal line. This test renders the real `graph_view::show` path
// (through `Harness::new_ui_state`, same recipe as
// `focused_canvas_paints_two_more_shapes_than_unfocused` above) and asserts
// on the real painted `Shape::LineSegment` geometry: every real crossing
// edge must still be drawn (no sampling), AND more than one distinct y
// must appear among them once the fix lands.
// ============================================================

/// Content-based shape filter (same style as `count_side_tinted_text_shapes`
/// above) over `output.shapes`, matching `egui::Shape::LineSegment` entries
/// whose `stroke.color` is exactly `paint_crossing_threads`' accent color
/// (`overlay::ACCENT_HEX` `#ff4d8d` at `gamma_multiply(0.35)`) -- distinct
/// from the fault line's full-strength (2.5px) and 0.16-gamma (14px glow)
/// strokes, and from the rubber-band/traced-path strokes (neither of which
/// paint in this focused-but-not-tracing scenario). Returns each matched
/// segment's first point's y.
fn crossing_thread_line_ys(output: &egui::FullOutput) -> Vec<f32> {
    let accent = egui::Color32::from_hex("#ff4d8d")
        .expect("valid hex")
        .gamma_multiply(0.35);
    output
        .shapes
        .iter()
        .filter_map(|clipped| match &clipped.shape {
            egui::Shape::LineSegment { points, stroke } if stroke.color == accent => {
                Some(points[0].y)
            }
            _ => None,
        })
        .collect()
}

/// The literal proof of WINDOWS.md entry 5 / UAT test 12: with
/// `clean.json`'s A/B seam pair (4 real crossing edges -- `a1->b1`,
/// `a2->b1`, `a1->b2`, `a2->b2`, confirmed via `SeamDetail.a_to_b +
/// b_to_a` rather than a hardcoded literal so this test stays correct even
/// if the fixture changes), every real crossing edge must still be drawn
/// (no sampling), AND their drawn y-coordinates must not all collapse onto
/// one shared value -- distinct source/target nodes have distinct real
/// canvas positions, and the drawn threads must reflect that.
#[test]
fn crossing_threads_do_not_collapse_onto_a_single_shared_y() {
    let app = build_focused_test_app();
    let expected_thread_count = {
        let detail = app.detail.as_ref().expect("focused test app has detail");
        detail.a_to_b + detail.b_to_a
    };

    let mut harness = Harness::new_ui_state(
        move |ui, app: &mut SeamExplorerApp| {
            graph_view::show(ui, app);
        },
        app,
    );
    harness.run_steps(5);

    let ys = crossing_thread_line_ys(harness.output());
    assert_eq!(
        ys.len(),
        expected_thread_count,
        "every real crossing edge must be drawn as a thread -- no sampling, no cap -- expected \
         {expected_thread_count}, got {} ({ys:?})",
        ys.len()
    );

    // Collapse near-duplicate y values (small epsilon) to distinct buckets;
    // today's shipped code draws every thread at the identical
    // canvas-vertical-center y, so this collapses to exactly one bucket --
    // the literal proof of the bug.
    const EPSILON: f32 = 0.5;
    let mut distinct_ys: Vec<f32> = Vec::new();
    for y in &ys {
        if !distinct_ys
            .iter()
            .any(|existing| (existing - y).abs() < EPSILON)
        {
            distinct_ys.push(*y);
        }
    }
    assert!(
        distinct_ys.len() > 1,
        "crossing threads must connect their real per-node y-positions, not collapse onto a \
         single shared y -- got {} distinct y value(s) among {ys:?}",
        distinct_ys.len()
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

// ============================================================
// 05-16 Task 1 (RED): the live-wiring test proving the rendered transform
// depends on WHERE the cursor is, not just on the fact that a wheel event
// happened. `plain_scroll_zooms_the_canvas` above only asserts zoom
// increased and is completely blind to the defect this closes: today
// `apply_scroll_zoom` never reads the cursor, so a plain scroll anchors on
// the viewport centre regardless of where the pointer sits (`05-16-PLAN.md`
// `<design_decision>` section 5).
// ============================================================

/// Settles a fresh harness, captures the before-scroll frame, drives the
/// same `PointerMoved` + `MouseWheel` recipe `plain_scroll_zooms_the_canvas`
/// uses (same event shape, same `step()` count -- one after the pointer
/// move, two after the wheel event) at the given cursor position and
/// modifiers, and returns the before/after mirrored `MetadataFrame`s plus
/// the `smooth_scroll_delta` egui itself reported for the wheel event.
///
/// Plan 17: generalised over 05-16 to take a `modifiers` parameter and
/// return the observed scroll-delta probe, needed by
/// `shift_scroll_zooms_at_half_the_plain_scroll_speed` to empirically
/// confirm/refute `<design_decision>` section 1 (egui rewrites a Shift-held
/// wheel delta onto the horizontal axis before the app ever sees it). The
/// modifier is set in BOTH places a real winit event sets it: on the
/// `MouseWheel` event's own `modifiers` field, which is what egui's
/// wheel-state axis-rewrite (`horizontal_scroll_modifier`, defaulting to
/// `Modifiers::SHIFT`) reads when deciding whether to fold the delta onto
/// `.x`; and on the harness's global `RawInput::modifiers`, which is what
/// `i.modifiers.shift` reads in production at `apply_scroll_zoom`'s call
/// site. Setting only one gives a test that fails for a reason unrelated to
/// the feature.
///
/// This does not delegate to `canvas_harness()` (unlike every other test in
/// this file) because the scroll-delta probe must be read from INSIDE the
/// same `show()` frame that processes the wheel event, which requires
/// injecting an extra read into the closure passed to
/// `Harness::new_ui_state` -- `canvas_harness()`'s closure has no hook for
/// that without changing its signature for every other call site in this
/// file. The rest of the setup (same `build_test_app()`, same
/// `MetadataFrame` mirror pattern) is otherwise identical.
///
/// `wheel_delta_y` is the `Line`-unit magnitude pushed on the `MouseWheel`
/// event (in the SAME units `plain_scroll_zooms_the_canvas`'s hardcoded
/// `3.0` used before this generalisation -- callers reproducing that test's
/// exact behaviour should keep passing `3.0`). Made a parameter rather than
/// staying hardcoded because this file's harness applies egui's default
/// `line_scroll_speed` (40.0 pts/line, native) BEFORE this function's caller
/// ever sees a value: `3.0` lines resolves to a raw `smooth_scroll_delta` of
/// ~108 (confirmed via the SHIFT-PROBE this same generalisation captures),
/// which drives a factor of `exp(108 * 0.02) ~= 8.7` -- comfortably clear of
/// the zoom clamps for the anchor test's single-leg checks, but too close to
/// `MAX_ZOOM` for a test that also needs both legs, and their exact log
/// ratio, to stay clamp-free (05-17-PLAN.md Task 1's own instruction:
/// "choose a wheel delta small enough that neither leg gets anywhere near
/// the zoom limits ... adjust the delta if either leg is close to a
/// limit").
fn scroll_zoom_leg(
    cursor: egui::Pos2,
    modifiers: egui::Modifiers,
    wheel_delta_y: f32,
) -> (
    egui_graphs::MetadataFrame,
    egui_graphs::MetadataFrame,
    egui::Vec2,
) {
    let app = build_test_app();
    let mirror: Rc<RefCell<Option<egui_graphs::MetadataFrame>>> = Rc::new(RefCell::new(None));
    let mirror_inner = mirror.clone();
    let probe: Rc<RefCell<egui::Vec2>> = Rc::new(RefCell::new(egui::Vec2::ZERO));
    let probe_inner = probe.clone();
    let mut harness = Harness::new_ui_state(
        move |ui, app: &mut SeamExplorerApp| {
            // Read BEFORE show() so this reflects exactly what
            // apply_scroll_zoom's call site would see this same frame.
            *probe_inner.borrow_mut() = ui.input(|i| i.smooth_scroll_delta());
            graph_view::show(ui, app);
            *mirror_inner.borrow_mut() = Some(egui_graphs::MetadataFrame::new(None).load(ui));
        },
        app,
    );
    harness.run_steps(3);
    let before = mirror
        .borrow()
        .clone()
        .expect("frame must be mirrored after settling");

    harness.input_mut().modifiers = modifiers;
    harness
        .input_mut()
        .events
        .push(egui::Event::PointerMoved(cursor));
    harness.step();
    harness.input_mut().events.push(egui::Event::MouseWheel {
        unit: egui::MouseWheelUnit::Line,
        delta: egui::vec2(0.0, wheel_delta_y),
        phase: egui::TouchPhase::Move,
        modifiers,
    });
    harness.step();
    let observed_delta = *probe.borrow();
    harness.step();

    let after = mirror
        .borrow()
        .clone()
        .expect("frame must be mirrored after scroll");
    (before, after, observed_delta)
}

/// The direct regression test for the user's reported defect: a plain
/// scroll/two-finger zoom at two different cursor positions must produce
/// two different rendered pans, and the difference must equal the specific
/// quantity `(cursor_a - cursor_b) * (z0 - z1) / z0` derived in
/// `05-16-PLAN.md` `<design_decision>` section 5 -- not just "some"
/// difference. Under today's centre-anchored code the two pans are
/// bit-identical (the cursor is not an input to `apply_scroll_zoom` at
/// all), so the difference is exactly zero against a predicted difference
/// of roughly 86px for the positions chosen below -- an unambiguous RED
/// signal.
///
/// Guard assertions (all precede the main assertion, in the order this
/// test's own module doc calls for, so a broken setup can never masquerade
/// as the defect): identical start transforms, both legs actually zoomed
/// in, both legs reached the same post-scroll zoom, and the zoom actually
/// moved by more than float noise.
#[test]
fn scroll_zoom_anchors_on_the_cursor_not_the_canvas_centre() {
    // Two cursor positions well inside the default 800x600 harness rect,
    // separated by a few hundred px on both axes, both clear of the
    // top-right corner where `trace::show_onboarding`'s `RIGHT_TOP`-anchored
    // `Area` floats, and neither at the exact viewport centre (400, 300) --
    // the one position at which a correct and an incorrect implementation
    // agree.
    let cursor_a = egui::pos2(150.0, 150.0); // top-left quadrant
    let cursor_b = egui::pos2(550.0, 450.0); // bottom-right-of-centre, clear of top-right

    let (before_a, after_a, _probe_a) = scroll_zoom_leg(cursor_a, egui::Modifiers::default(), 3.0);
    let (before_b, after_b, _probe_b) = scroll_zoom_leg(cursor_b, egui::Modifiers::default(), 3.0);

    // Guard 1: the two harnesses started from the same transform -- the
    // precondition that makes the differential identity below valid at all
    // (05-16-PLAN.md design_decision section 5's determinism argument:
    // layout::jitter is a pure function of the node key, render_focus_changed
    // and reset_sentinel_fired cannot arm on the first frame, and no focus is
    // ever set in canvas_harness).
    assert!(
        (before_a.zoom - before_b.zoom).abs() < 1e-6,
        "the two independently-built harnesses must settle to the same starting zoom, \
         got {} vs {}",
        before_a.zoom,
        before_b.zoom
    );
    assert!(
        (before_a.pan - before_b.pan).length() < 1e-3,
        "the two independently-built harnesses must settle to the same starting pan, \
         got {:?} vs {:?}",
        before_a.pan,
        before_b.pan
    );

    // Guard 2: each leg's zoom actually increased -- proving the wheel event
    // reached the canvas and the chosen cursor position was genuinely over
    // it, in both legs.
    assert!(
        after_a.zoom > before_a.zoom,
        "leg A's scroll must have zoomed in, got before={} after={}",
        before_a.zoom,
        after_a.zoom
    );
    assert!(
        after_b.zoom > before_b.zoom,
        "leg B's scroll must have zoomed in, got before={} after={}",
        before_b.zoom,
        after_b.zoom
    );

    // Guard 3: the two legs reached the same post-scroll zoom -- proving the
    // zoom step itself is independent of cursor position (only pan should
    // differ between the two legs).
    assert!(
        (after_a.zoom - after_b.zoom).abs() < 1e-4,
        "both legs must reach the same post-scroll zoom (only pan should differ by cursor \
         position), got {} vs {}",
        after_a.zoom,
        after_b.zoom
    );

    // Guard 4: the zoom actually moved by more than float noise -- without
    // this, the predicted pan difference collapses to zero and the test
    // asserts nothing.
    let z0 = before_a.zoom;
    let z1 = after_a.zoom;
    assert!(
        (z1 - z0).abs() > 1e-3,
        "the scroll must have moved zoom by more than float noise for this test to be \
         meaningful, got z0={z0} z1={z1}"
    );

    // The assertion whose failure IS the defect: the rendered pan must
    // differ when the cursor differs. Under today's centre-anchored code
    // this is the assertion that fails.
    let observed = after_a.pan - after_b.pan;
    assert!(
        observed.length() > 1.0,
        "scrolling at two different cursor positions must produce two different rendered \
         pans -- the zoom must anchor on the cursor, not on the canvas centre. Got \
         after_a.pan={:?} after_b.pan={:?} (identical -- the cursor position never reached \
         apply_scroll_zoom)",
        after_a.pan,
        after_b.pan
    );

    // The quantitative identity from design_decision section 5: derived from
    // the observed z0/z1 rather than hardcoded, so a future retune of
    // SCROLL_ZOOM_SENSITIVITY does not silently invalidate this test.
    let expected = (cursor_a - cursor_b) * (z0 - z1) / z0;
    assert!(
        (observed - expected).length() < 2.0,
        "the pan difference must equal (cursor_a - cursor_b) * (z0 - z1) / z0 -- expected \
         {expected:?}, observed {observed:?} (z0={z0}, z1={z1}, cursor_a={cursor_a:?}, \
         cursor_b={cursor_b:?})"
    );
}

/// Plan 17, Task 1 (RED): the direct regression test for the user's
/// request -- Shift-held scroll must zoom the canvas at exactly half the log
/// rate of a plain scroll.
///
/// `<design_decision>` section 1 predicts the exact shape of today's
/// failure: egui 0.35's `InputOptions::horizontal_scroll_modifier` defaults
/// to `Modifiers::SHIFT`, so a Shift-held wheel event is rewritten by egui's
/// own wheel-state onto the HORIZONTAL axis (`vec2(delta.x + delta.y,
/// 0.0)`) before the app ever sees it. The existing call site in
/// `graph_view::show()` reads only `smooth_scroll_delta().y` and is guarded
/// on it being non-zero, so today it sees exactly `0.0` under Shift and does
/// nothing at all -- not "less zoom", but NO zoom whatsoever. That is the
/// specific assertion this test expects to fail on right now: not a guard,
/// and not the ratio.
#[test]
fn shift_scroll_zooms_at_half_the_plain_scroll_speed() {
    // Well inside the default 800x600 harness rect, clear of the top-right
    // corner where `trace::show_onboarding`'s Area floats (same discipline
    // as `scroll_zoom_anchors_on_the_cursor_not_the_canvas_centre`).
    let cursor = egui::pos2(300.0, 300.0);

    // A much smaller Line-unit delta than the anchor test's `3.0`: measured
    // via this same test's own SHIFT-PROBE, `3.0` resolves to a raw
    // `smooth_scroll_delta` of ~108 (`exp(108 * 0.02) ~= 8.7`), which drives
    // the plain leg to zoom=9.56 -- right at `MAX_ZOOM`'s doorstep and too
    // close for this test's clamp guard, which needs BOTH legs (plain and
    // half-speed Shift) to land clear of either clamp so the log-ratio
    // identity below is meaningful. `0.3` keeps the plain leg's factor near
    // the design_decision worked example (`exp(10.8 * 0.02) ~= 1.24`).
    let wheel_delta_y = 0.3;

    let (before_a, after_a, probe_a) =
        scroll_zoom_leg(cursor, egui::Modifiers::default(), wheel_delta_y);
    let (before_b, after_b, probe_b) =
        scroll_zoom_leg(cursor, egui::Modifiers::SHIFT, wheel_delta_y);

    // SHIFT-PROBE: the empirical record for <design_decision> section 1 --
    // run with `--nocapture` to see it. If egui really does fold the Shift
    // leg's delta onto .x, probe_b.y must be exactly 0.0 with a non-zero
    // probe_b.x, while probe_a should show the opposite (non-zero .y, .x
    // untouched).
    println!(
        "SHIFT-PROBE plain smooth_scroll_delta={probe_a:?} shift smooth_scroll_delta={probe_b:?}"
    );

    // Guard 1: both legs started from the same rendered transform -- the
    // precondition for comparing their zoom deltas at all.
    assert!(
        (before_a.zoom - before_b.zoom).abs() < 1e-6,
        "the two independently-built harnesses must settle to the same starting zoom, \
         got {} vs {}",
        before_a.zoom,
        before_b.zoom
    );

    // Guard 2: leg A (plain) actually zoomed -- proving the wheel reached
    // the canvas at all, so a failure below can't be blamed on a broken
    // harness recipe.
    assert!(
        after_a.zoom > before_a.zoom,
        "leg A (plain) must have zoomed in, got before={} after={}",
        before_a.zoom,
        after_a.zoom
    );

    // The assertion that fails TODAY, by the largest possible margin: leg B
    // (Shift) must have zoomed at all. Bit-identical to its starting zoom is
    // the unambiguous RED signature of a delta arriving on an axis nothing
    // reads.
    assert!(
        after_b.zoom > before_b.zoom,
        "a Shift-held scroll must zoom the canvas too (at half speed) -- got NO zoom \
         whatsoever: before={} after={} (SHIFT-PROBE smooth_scroll_delta={probe_b:?}). This is \
         exactly the axis-rewrite defect <design_decision> section 1 predicts: egui folds a \
         Shift-held wheel delta onto the horizontal axis, and the call site currently only \
         reads .y.",
        before_b.zoom,
        after_b.zoom
    );

    // Guard 3: leg B zoomed strictly LESS than leg A -- half speed, not
    // equal or more.
    assert!(
        after_b.zoom < after_a.zoom,
        "the Shift leg must zoom LESS than the plain leg (half speed, not equal or more), \
         got plain after={} shift after={}",
        after_a.zoom,
        after_b.zoom
    );

    // Guard 4: neither leg reached graph_view's MIN_ZOOM (0.1) / MAX_ZOOM
    // (10.0) clamp -- a clamped leg invalidates the log-ratio identity
    // below. Literal bounds mirrored here with generous margin since those
    // constants are private to graph_view.rs and not importable from this
    // integration-test crate.
    assert!(
        (0.2..9.0).contains(&after_a.zoom) && (0.2..9.0).contains(&after_b.zoom),
        "neither leg may approach a zoom clamp for this test's ratio to be meaningful, got \
         plain after={} shift after={}",
        after_a.zoom,
        after_b.zoom
    );

    // The quantitative relationship: ln(zoom_B / zoom_start) is half
    // ln(zoom_A / zoom_start) -- derived from the observed zooms, not a
    // hardcoded step size, so a future retune of SCROLL_ZOOM_SENSITIVITY (or
    // its slow-path divisor) does not silently invalidate this test.
    let z0 = before_a.zoom;
    let log_ratio_plain = (after_a.zoom / z0).ln();
    let log_ratio_shift = (after_b.zoom / z0).ln();
    let expected_shift_log_ratio = log_ratio_plain / 2.0;
    let tolerance = log_ratio_plain.abs() * 0.05; // a few percent of the log ratio
    assert!(
        (log_ratio_shift - expected_shift_log_ratio).abs() < tolerance,
        "Shift-held scroll must zoom at exactly HALF the log rate of a plain scroll: expected \
         ln(zoom_shift/z0)={expected_shift_log_ratio}, got {log_ratio_shift} \
         (ln(zoom_plain/z0)={log_ratio_plain}, tolerance={tolerance})"
    );
}

// ============================================================
// 05-19 Task 1 (RED): live-wiring tests proving Cmd/Ctrl+scroll and pinch
// zoom the canvas during Trace mode -- the user's reported defect
// ("the cmd-scroll doesn't work when trace is on"). `<discovery_findings>`
// sections 1-3 established, via a throwaway probe against the real crate,
// that today `zoom_delta()` reaches a healthy non-identity value every frame
// of the gesture while the rendered `MetadataFrame.zoom` never moves --
// neither `egui_graphs`' own `handle_zoom` (disabled by
// `zoom_and_pan_enabled(!app.trace_mode)`, the 05-08 G-05-4 fix) nor the
// app's plain-scroll fallback (guarded on a non-zero `smooth_scroll_delta`,
// which egui 0.35 zeroes entirely when the zoom modifier is held) ever
// consumes it.
// ============================================================

/// A gesture `native_zoom_leg` can inject: a wheel scroll carrying a
/// `Line`-unit magnitude (the Cmd/Ctrl+scroll case, which requires the zoom
/// modifier on the event so egui's own wheel-state folds it into
/// `zoom_delta` rather than `smooth_scroll_delta`), or a single synthetic
/// pinch carrying its own multiplicative factor directly (`egui::Event::Zoom`,
/// what a real trackpad pinch synthesises -- `<discovery_findings>` section 3).
enum ZoomGesture {
    Wheel(f32),
    Pinch(f32),
}

/// Sibling of `scroll_zoom_leg` for the native `zoom_delta()` path -- NOT a
/// generalisation of it (see `scroll_zoom_leg`'s own doc comment for why:
/// its numeric expectations are tuned to its exact step count and event
/// ordering, and the two helpers need different return values and different
/// gesture injection anyway).
///
/// Builds a fresh app with `trace_mode` set BEFORE the first frame, mirrors
/// the persisted `MetadataFrame` after every `show()` call exactly as
/// `scroll_zoom_leg` does, and additionally records `ui.input(|i|
/// i.zoom_delta())` -- read BEFORE `show()`, so it is precisely what the
/// call site sees that frame -- into a growing vector (`observed_deltas`,
/// deliberately not named `probe` so a diff gate can distinguish this
/// plan's code from 05-16/05-17's `scroll_zoom_leg`).
///
/// Settles with `run_steps(3)`, captures `before`, moves the pointer to
/// `cursor`, injects `gesture` (setting `modifiers` in BOTH places a real
/// winit event sets them -- on the `MouseWheel` event's own `modifiers`
/// field, which is what egui's wheel-state reads when deciding `is_zoom`,
/// and on the harness's global `RawInput::modifiers`, which is what
/// `i.modifiers.shift` reads in production), clears the recorded vector,
/// then steps 8 frames for egui's smoothing to drain fully (the probe
/// needed 4 non-identity frames for a wheel gesture; 8 is headroom).
///
/// Returning the observed `zoom_delta` sequence -- rather than a hardcoded
/// expectation -- is the point: it lets each caller compute its own
/// expected outcome from what egui actually reported, with no magic
/// constants to go stale if egui retunes its smoothing.
fn native_zoom_leg(
    trace_mode: bool,
    cursor: egui::Pos2,
    modifiers: egui::Modifiers,
    gesture: ZoomGesture,
) -> (
    egui_graphs::MetadataFrame,
    egui_graphs::MetadataFrame,
    Vec<f32>,
) {
    let mut app = build_test_app();
    app.trace_mode = trace_mode;

    let mirror: Rc<RefCell<Option<egui_graphs::MetadataFrame>>> = Rc::new(RefCell::new(None));
    let mirror_inner = mirror.clone();
    let observed_deltas: Rc<RefCell<Vec<f32>>> = Rc::new(RefCell::new(Vec::new()));
    let observed_deltas_inner = observed_deltas.clone();
    let mut harness = Harness::new_ui_state(
        move |ui, app: &mut SeamExplorerApp| {
            // Read BEFORE show() so this reflects exactly what the new
            // trace-mode zoom branch's call site would see this same frame.
            let zd = ui.input(|i| i.zoom_delta());
            observed_deltas_inner.borrow_mut().push(zd);
            graph_view::show(ui, app);
            *mirror_inner.borrow_mut() = Some(egui_graphs::MetadataFrame::new(None).load(ui));
        },
        app,
    );
    harness.run_steps(3);
    let before = mirror
        .borrow()
        .clone()
        .expect("frame must be mirrored after settling");

    harness.input_mut().modifiers = modifiers;
    harness
        .input_mut()
        .events
        .push(egui::Event::PointerMoved(cursor));
    harness.step();

    match gesture {
        ZoomGesture::Wheel(delta_y) => {
            harness.input_mut().events.push(egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Line,
                delta: egui::vec2(0.0, delta_y),
                phase: egui::TouchPhase::Move,
                modifiers,
            });
        }
        ZoomGesture::Pinch(factor) => {
            harness.input_mut().events.push(egui::Event::Zoom(factor));
        }
    }
    observed_deltas.borrow_mut().clear();
    harness.run_steps(8);

    let after = mirror
        .borrow()
        .clone()
        .expect("frame must be mirrored after gesture");
    let deltas = observed_deltas.borrow().clone();
    (before, after, deltas)
}

/// The user's reported defect, reproduced live: with Trace mode ON, a
/// Cmd/Ctrl-held scroll must zoom the canvas. **This is a RED.** Today
/// `zoom_delta()` reaches a real non-identity value every frame of the
/// gesture (proven by the `gesture_reached_egui` guard) while the rendered
/// zoom does not move by a single ulp -- neither `egui_graphs`' own
/// `handle_zoom` (disabled during trace mode, G-05-4) nor the app's
/// plain-scroll fallback (guarded on a non-zero `smooth_scroll_delta`, which
/// egui zeroes entirely when the zoom modifier is held) ever consumes it.
#[test]
fn cmd_scroll_zooms_the_canvas_in_trace_mode() {
    // Well inside the default 800x600 harness rect, clear of the top-right
    // corner where `trace::show_onboarding`'s Area floats (same discipline
    // as the existing scroll tests). `3.0` Line-unit lines is the planner
    // probe's own recipe and keeps both trace-mode-on and trace-mode-off
    // legs well inside the 0.2..9.0 clamp margin (`<discovery_findings>`
    // section 1).
    let cursor = egui::pos2(400.0, 300.0);
    let (before, after, observed_deltas) = native_zoom_leg(
        true,
        cursor,
        egui::Modifiers::COMMAND,
        ZoomGesture::Wheel(3.0),
    );

    // Guard: at least one observed zoom_delta must differ from 1.0 by more
    // than float noise -- otherwise the gesture never reached egui and this
    // test proves nothing about the feature under test.
    let gesture_reached_egui = observed_deltas.iter().any(|d| (d - 1.0).abs() > 1e-3);
    assert!(
        gesture_reached_egui,
        "the Cmd+scroll gesture never registered a non-identity zoom_delta -- the test's own \
         setup is broken, not the feature under test. observed_deltas={observed_deltas:?}"
    );

    // The assertion that fails TODAY: the rendered zoom must have increased
    // at all. The exact phrase below ("no zoom whatsoever") is grepped by
    // this task's own verify gate to prove the RED failed on this
    // assertion, not on a setup guard.
    assert!(
        after.zoom > before.zoom,
        "Cmd/Ctrl+scroll must zoom the canvas while Trace mode is on -- got no zoom whatsoever: \
         before={} after={} (observed zoom_delta sequence={observed_deltas:?})",
        before.zoom,
        after.zoom
    );

    assert!(
        (0.2..9.0).contains(&before.zoom) && (0.2..9.0).contains(&after.zoom),
        "neither end may be near a zoom clamp for this test's quantitative identity to be \
         meaningful, got before={} after={}",
        before.zoom,
        after.zoom
    );

    // The quantitative identity from `<design_decision>` section 3(A): the
    // app applies EXACTLY the factor egui computed for the gesture --
    // after.zoom == before.zoom * product(observed zoom_deltas). Derived
    // from the observed vector, not a hardcoded constant.
    let expected_factor: f32 = observed_deltas.iter().product();
    let expected = before.zoom * expected_factor;
    let rel_err = (after.zoom - expected).abs() / expected.max(1e-6);
    assert!(
        rel_err < 1e-3,
        "the rendered zoom must equal before.zoom * product(observed zoom_deltas): expected \
         {expected} (factor={expected_factor}), got {} (relative error {rel_err}) -- before={} \
         observed_deltas={observed_deltas:?}",
        after.zoom,
        before.zoom
    );
}

/// The trackpad-pinch sibling of `cmd_scroll_zooms_the_canvas_in_trace_mode`
/// -- a single `Event::Zoom(1.10)`, what a real trackpad pinch synthesises
/// (`<discovery_findings>` section 3). **This is a RED**, for the identical
/// reason: `InputState::zoom_delta()` returns the multi-touch value when a
/// pinch is live, so this exercises the same dead-end as the Cmd+scroll
/// case through one accessor.
#[test]
fn pinch_zooms_the_canvas_in_trace_mode() {
    let cursor = egui::pos2(400.0, 300.0);
    let (before, after, observed_deltas) = native_zoom_leg(
        true,
        cursor,
        egui::Modifiers::default(),
        ZoomGesture::Pinch(1.10),
    );

    let gesture_reached_egui = observed_deltas.iter().any(|d| (d - 1.0).abs() > 1e-3);
    assert!(
        gesture_reached_egui,
        "the pinch gesture never registered a non-identity zoom_delta -- the test's own setup \
         is broken, not the feature under test. observed_deltas={observed_deltas:?}"
    );

    assert!(
        after.zoom > before.zoom,
        "a trackpad pinch must zoom the canvas while Trace mode is on -- got no zoom whatsoever: \
         before={} after={} (observed zoom_delta sequence={observed_deltas:?})",
        before.zoom,
        after.zoom
    );

    assert!(
        (0.2..9.0).contains(&before.zoom) && (0.2..9.0).contains(&after.zoom),
        "neither end may be near a zoom clamp for this test's quantitative identity to be \
         meaningful, got before={} after={}",
        before.zoom,
        after.zoom
    );

    // The probe (`<discovery_findings>` section 3) shows exactly one
    // non-identity frame with zoom_delta == 1.10, so the expected post-pinch
    // value is before.zoom * 1.10 -- still computed from the observed
    // vector, not hardcoded.
    let expected_factor: f32 = observed_deltas.iter().product();
    let expected = before.zoom * expected_factor;
    let rel_err = (after.zoom - expected).abs() / expected.max(1e-6);
    assert!(
        rel_err < 1e-3,
        "the rendered zoom must equal before.zoom * product(observed zoom_deltas): expected \
         {expected} (factor={expected_factor}), got {} (relative error {rel_err}) -- before={} \
         observed_deltas={observed_deltas:?}",
        after.zoom,
        before.zoom
    );
}

/// The regression lock against the single most likely way to ship this
/// broken: with Trace mode OFF, the same Cmd+scroll gesture must produce
/// EXACTLY `egui_graphs`' own native (magnitude-blind, `<discovery_findings>`
/// section 1) outcome, and must be strictly less than the outcome the app
/// would produce if it ALSO applied its own factor on top. **This PASSES
/// today** -- trace mode off is not this plan's defect, so this must never
/// have been red.
#[test]
fn cmd_scroll_zoom_is_not_double_applied_when_trace_mode_off() {
    let cursor = egui::pos2(400.0, 300.0);
    let (before, after, observed_deltas) = native_zoom_leg(
        false,
        cursor,
        egui::Modifiers::COMMAND,
        ZoomGesture::Wheel(3.0),
    );

    let gesture_reached_egui = observed_deltas.iter().any(|d| (d - 1.0).abs() > 1e-3);
    assert!(
        gesture_reached_egui,
        "the Cmd+scroll gesture never registered a non-identity zoom_delta -- the test's own \
         setup is broken, not the feature under test. observed_deltas={observed_deltas:?}"
    );

    assert!(
        after.zoom > before.zoom,
        "Cmd+scroll must zoom the canvas with Trace mode off (the control case) -- got no zoom \
         whatsoever: before={} after={} (observed zoom_delta sequence={observed_deltas:?})",
        before.zoom,
        after.zoom
    );

    assert!(
        (0.2..9.0).contains(&before.zoom) && (0.2..9.0).contains(&after.zoom),
        "neither end may be near a zoom clamp for this test's quantitative identity to be \
         meaningful, got before={} after={}",
        before.zoom,
        after.zoom
    );

    // `egui_graphs::handle_zoom` (read directly, vendored source) gates on
    // an EXACT `zoom_delta == 1.0` comparison and, when it fires, applies a
    // fixed `zoom_speed` (default 0.1) step per frame based only on
    // `signum(zoom_delta - 1)` -- magnitude-blind. `k` below mirrors that
    // exact-inequality gate, not a tolerance, so it counts precisely the
    // frames egui_graphs itself would have stepped on.
    let k = observed_deltas.iter().filter(|d| **d != 1.0_f32).count() as i32;
    let native_only_factor = 1.1_f32.powi(k);
    let native_only_expected = before.zoom * native_only_factor;
    let rel_err = (after.zoom - native_only_expected).abs() / native_only_expected.max(1e-6);
    assert!(
        rel_err < 1e-3,
        "with Trace mode off, the canvas must zoom by exactly egui_graphs' own native \
         (magnitude-blind) step -- 1.1^k for k={k} non-identity zoom_delta frames -- expected \
         {native_only_expected}, got {} (relative error {rel_err}), \
         observed_deltas={observed_deltas:?}",
        after.zoom
    );

    // The bound that catches double-application: if the app ALSO applied
    // its own factor on top of egui_graphs' native step, the result would
    // be before.zoom * product(observed zoom_deltas) -- ~1.822x for this
    // gesture, a 24% separation from the native-only ~1.464x
    // (`<discovery_findings>` section 1). `after.zoom` must stay
    // comfortably below that doubled value.
    let double_applied_factor: f32 = observed_deltas.iter().product();
    let double_applied_bound = before.zoom * double_applied_factor * 0.95;
    assert!(
        after.zoom < double_applied_bound,
        "the app must NOT also apply its own zoom factor on top of egui_graphs' native step \
         with Trace mode off (that would be double-application) -- got after={} which is not \
         comfortably below the double-applied bound {double_applied_bound} \
         (double_applied_factor={double_applied_factor}, observed_deltas={observed_deltas:?})",
        after.zoom
    );
}
