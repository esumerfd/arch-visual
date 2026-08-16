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

use egui_kittest::Harness;
use seam_explorer_egui::app::{FocusState, SeamExplorerApp, ViewState};
use seam_explorer_egui::layout::SeamLayoutState;
use seam_explorer_egui::{graph_view, keyboard, trace};

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
