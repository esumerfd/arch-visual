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
use seam_explorer_egui::app::{SeamExplorerApp, ViewState};
use seam_explorer_egui::{graph_view, keyboard};

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
