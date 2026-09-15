//! quick-260915-ppq: this file owns two claims -- the top bar's real
//! rendered left-to-right button order, and the three new single-key
//! shortcuts (`o`, `l`, `r`) that were added alongside it.
//!
//! **Honesty note, up front.** `o` and `l` terminate in a native OS dialog
//! (a folder picker and a file picker respectively) that would hang this
//! test process if actually invoked. Neither key gets a keypress test here.
//! Only `r` -- which resolves to a pure in-process view mutation with no
//! dialog involved -- gets a real keypress driven through the production
//! `keyboard::handle`. `o` and `l`'s only evidence is structural (grepped in
//! `keyboard.rs`) plus this plan's Task 3 live walkthrough.

use egui::Modifiers;
use egui_kittest::kittest::Queryable;
use egui_kittest::Harness;
use seam_explorer_egui::app::{SeamExplorerApp, ViewState};
use seam_explorer_egui::keyboard;

/// Drives the real assembled `eframe::App::ui` -- every panel, the canvas,
/// the top bar, and the keyboard handler -- exactly as `v1_0_parity.rs`'s
/// `app_ui_harness` does. `Frame::_new_kittest()` is eframe's own
/// constructor for exactly this; `run_steps` rather than `run` because
/// `graph_view::show` renders through `egui_graphs`, whose layout keeps
/// requesting repaints, so running to quiescence would panic at `max_steps`.
fn app_ui_harness(app: SeamExplorerApp) -> Harness<'static, SeamExplorerApp> {
    let mut frame = eframe::Frame::_new_kittest();
    Harness::new_ui_state(
        move |ui, app: &mut SeamExplorerApp| {
            <SeamExplorerApp as eframe::App>::ui(app, ui, &mut frame);
        },
        app,
    )
}

/// The top bar renders regardless of whether a graph is loaded, and a
/// default-constructed app keeps this test cheap.
#[test]
fn the_top_bar_reads_open_project_load_reset_trace_left_to_right() {
    let mut harness = app_ui_harness(SeamExplorerApp::default());
    harness.run_steps(3);

    // Guard: all four buttons must be found at all. A missing query must
    // fail loudly rather than let a downstream comparison pass vacuously.
    let open_project = harness.get_by_label("Open Project...");
    let open_x = open_project.rect().min.x;

    let load_graph = harness.get_by_label("Load graph.json");
    let load_x = load_graph.rect().min.x;

    let reset_view = harness.get_by_label("Reset view");
    let reset_x = reset_view.rect().min.x;

    // `get_by_label_contains("Trace mode")` is ambiguous here: the detail
    // panel's legend paragraph is a `Role::Label` node whose accesskit
    // `value()` ("...or turn on Trace mode and drag...") also contains the
    // substring, and `By::label_contains` falls back to `value()` for
    // `Role::Label` nodes. Matching the exact current label sidesteps the
    // collision: a freshly `SeamExplorerApp::default()`-constructed app has
    // `trace_mode == false`, so the button's label is deterministically
    // "Trace mode · off".
    let trace_mode = harness.get_by_label("Trace mode · off");
    let trace_x = trace_mode.rect().min.x;

    assert!(
        open_x < load_x,
        "Open Project... (x={open_x}) must be left of Load graph.json (x={load_x})"
    );
    assert!(
        load_x < reset_x,
        "Load graph.json (x={load_x}) must be left of Reset view (x={reset_x})"
    );
    assert!(
        reset_x < trace_x,
        "Reset view (x={reset_x}) must be left of Trace mode (x={trace_x})"
    );
}

/// The literal `app.rs` builds its `search_id` from. Duplicated here rather
/// than imported because `app.rs` is frozen and exports no constant for it --
/// the same duplication `keyboard_scrub.rs` and `panels/seam_list.rs` already
/// carry, for the same reason.
fn search_id() -> egui::Id {
    egui::Id::new("seam_explorer_search_input")
}

/// A harness whose whole per-frame body is the real `keyboard::handle` --
/// the same function `app.rs`'s frozen call site calls. Nothing here reaches
/// past `handle` to poke `reset_view` directly, which would prove the reset
/// works and say nothing about whether a keypress reaches it.
fn keyboard_harness(app: SeamExplorerApp) -> Harness<'static, SeamExplorerApp> {
    Harness::new_ui_state(
        |ui, app: &mut SeamExplorerApp| {
            let ctx = ui.ctx().clone();
            keyboard::handle(&ctx, app, search_id());
        },
        app,
    )
}

/// The same harness, plus a real search `TextEdit` carrying the id `app.rs`
/// passes to `handle`, focused every frame. Exercises the production focus
/// carve-out rather than simulating it.
fn keyboard_harness_with_focused_search(app: SeamExplorerApp) -> Harness<'static, SeamExplorerApp> {
    Harness::new_ui_state(
        |ui, app: &mut SeamExplorerApp| {
            let response = ui.add(
                egui::TextEdit::singleline(&mut app.search_query)
                    .id(search_id())
                    .hint_text("search"),
            );
            response.request_focus();
            let ctx = ui.ctx().clone();
            keyboard::handle(&ctx, app, search_id());
        },
        app,
    )
}

/// One keypress with no modifiers held.
fn press(harness: &mut Harness<'static, SeamExplorerApp>, key: egui::Key) {
    harness.key_press_modifiers(Modifiers::NONE, key);
    harness.step();
}

/// A view that is clearly off `ViewState::default()`, so a reset is a real,
/// observable change rather than a no-op that happens to look like success.
fn off_default_view() -> ViewState {
    ViewState {
        zoom: 2.5,
        pan: egui::vec2(123.0, -45.0),
    }
}

#[test]
fn pressing_r_resets_the_view() {
    let app = SeamExplorerApp {
        view: off_default_view(),
        ..Default::default()
    };
    let default_view = ViewState::default();
    assert!(
        app.view.zoom != default_view.zoom || app.view.pan != default_view.pan,
        "guard: the view must start off-default, or a reset would pass vacuously"
    );

    let mut harness = keyboard_harness(app);
    press(&mut harness, egui::Key::R);

    assert_eq!(
        harness.state().view.zoom,
        default_view.zoom,
        "r must reset zoom"
    );
    assert_eq!(
        harness.state().view.pan,
        default_view.pan,
        "r must reset pan"
    );
}

#[test]
fn a_focused_text_field_swallows_r() {
    let app = SeamExplorerApp {
        view: off_default_view(),
        ..Default::default()
    };
    let before = app.view;
    let default_view = ViewState::default();
    assert!(
        before.zoom != default_view.zoom || before.pan != default_view.pan,
        "guard: the view must start off-default"
    );

    let mut harness = keyboard_harness_with_focused_search(app);
    press(&mut harness, egui::Key::R);

    assert_eq!(
        harness.state().view.zoom,
        before.zoom,
        "a focused text field must swallow `r` -- zoom must be exactly what it was before the press, not the default"
    );
    assert_eq!(
        harness.state().view.pan,
        before.pan,
        "a focused text field must swallow `r` -- pan must be exactly what it was before the press, not the default"
    );
}
