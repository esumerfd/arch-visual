//! Plan 09-04: the keyboard route onto history navigation, proven in a
//! running app.
//!
//! Every test here drives a REAL `egui_kittest` harness whose per-frame body
//! calls `keyboard::handle` — the same function `app.rs`'s frozen call site
//! calls — and injects real `egui::Event::Key` events through the harness's
//! own `key_press_modifiers`, which sets `RawInput::modifiers` for the
//! key-down frame exactly as a real keypress would. Nothing here reaches past
//! `handle` to poke `timeline::apply_action` directly; if it did, it would
//! prove the navigation works and say nothing at all about whether a keypress
//! reaches it.
//!
//! **The socket is deliberately not involved.** This file's subject is
//! keyboard dispatch. Plan 09-02's `tests/timeline_reconstruction.rs` already
//! drives the real `AF_UNIX` socket -> receive thread -> `drain_and_apply`
//! path end to end and proves the reconstruction itself; re-driving it here
//! would add a process-global lock and two seconds of wall clock to prove
//! something already proven, while making these tests worse at their actual
//! job. The setup helper below instead records history synchronously, in
//! `drain_and_apply`'s exact order.
//!
//! TIME-05's evidence is a TRANSITION, not a single assertion: the pan tests
//! below pass against the unmodified `handle` and must still pass after the
//! wiring lands, with the scrub tests flipping from red to green in between.

use std::collections::BTreeSet;

use egui::Modifiers;
use egui_kittest::Harness;
use seam_core::GraphEvent;
use seam_explorer_egui::app::{SeamExplorerApp, ViewState};
use seam_explorer_egui::{keyboard, timeline};

/// The same fixture 09-02 and 09-03 use: 6 nodes across three communities,
/// every node carrying a `source_file` so a scripted node can inherit a
/// community by sibling path.
const SOURCE_PATHS_FIXTURE: &str = include_str!("../../seam-core/tests/fixtures/source_paths.json");

/// The `source_file` a scripted node claims, so `resolve_community` gives it
/// a concrete community rather than leaving it unresolved.
const SIBLING_SOURCE_FILE: &str = "src/auth/login.rs";

/// How many events the ordinary setup records. Small on purpose: these tests
/// are about which keypress produces which navigation, and 09-02 already owns
/// the buffer-wrap arithmetic.
const EVENTS: usize = 6;

/// The literal `app.rs` builds its `search_id` from. Duplicated here rather
/// than imported because `app.rs` is frozen and exports no constant for it —
/// the same duplication `panels/seam_list.rs` already carries, with the same
/// reason.
fn search_id() -> egui::Id {
    egui::Id::new("seam_explorer_search_input")
}

fn add_node(id: &str, label: &str) -> GraphEvent {
    GraphEvent::AddNode {
        id: id.to_string(),
        label: label.to_string(),
        community: None,
        source_file: Some(SIBLING_SOURCE_FILE.to_string()),
    }
}

/// The `n`th scripted live node. The id encodes `n`, which is also the
/// event's own `seq`, since every scripted event applies and therefore
/// records exactly one history entry.
fn scripted(n: usize) -> GraphEvent {
    let id = format!("live_{n:03}");
    add_node(&id, &id)
}

/// An app with a REAL recorded history and a REAL replay baseline.
///
/// **Built through `apply_load_outcome`, never field-by-field.** That is not
/// stylistic. `apply_load_outcome` is where 09-02 captures
/// `replay_baseline = Some(outcome.model.clone())`; a
/// `SeamExplorerApp { .. Default::default() }` construction skips it and
/// leaves the baseline `None`, which makes `timeline::reconstruct` take its
/// documented empty-model fallback. Every scrub test in this file would then
/// be exercising that fallback rather than real historical data, and would
/// pass while proving nothing about reconstruction.
///
/// The recording loop mirrors `history::drain_and_apply`'s exact order —
/// `apply_batch`, then `finalize_scc` on a topology change, then `detect`,
/// then push the RESOLVED `outcome.applied` events — so the entries recorded
/// here are the same resolved events the live path records. Pushing the raw
/// scripted events instead would record a `community: None` `AddNode` that
/// the live path never stores.
fn app_with_history(count: usize) -> SeamExplorerApp {
    let outcome = seam_explorer_egui::load::read_and_ingest(SOURCE_PATHS_FIXTURE)
        .expect("fixture must ingest cleanly");
    let mut app = SeamExplorerApp::default();
    app.apply_load_outcome(outcome);
    assert!(
        app.replay_baseline.is_some(),
        "guard: the setup must route through apply_load_outcome so a real \
         baseline exists — without it every reconstruct below silently returns \
         an empty model and these tests pass vacuously"
    );

    for n in 0..count {
        let event = scripted(n);
        let applied = {
            let model = app
                .model
                .as_mut()
                .expect("apply_load_outcome must have installed a model");
            let outcome = seam_core::apply_batch(model, std::slice::from_ref(&event));
            if outcome.topology_changed {
                model.finalize_scc();
            }
            outcome
        };
        if applied.topology_changed {
            if let Some(model) = app.model.as_ref() {
                app.seams = seam_core::detect(model);
            }
        }
        for event in &applied.applied {
            app.history.push(event.clone());
        }
    }

    assert_eq!(
        app.history.next_seq(),
        count as u64,
        "guard: every scripted event must have applied and recorded exactly one \
         entry — a silent setup failure here would make every assertion below \
         pass for the wrong reason"
    );
    app
}

fn model_ids(model: &seam_core::Model) -> BTreeSet<String> {
    model.index.keys().cloned().collect()
}

/// A harness whose whole per-frame body is the real `keyboard::handle`.
fn harness(app: SeamExplorerApp) -> Harness<'static, SeamExplorerApp> {
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
fn harness_with_focused_search(app: SeamExplorerApp) -> Harness<'static, SeamExplorerApp> {
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

/// One keypress. `key_press_modifiers` queues four items — set modifiers, key
/// down, key up, reset modifiers — and `Harness::step` runs one frame per
/// queued item, so the key-down frame sees `RawInput::modifiers` already set.
/// That is precisely the shape a real keypress has, and it is why `handle`
/// can read the modifier off the same `ctx.input` call that observes the key.
fn press(harness: &mut Harness<'static, SeamExplorerApp>, modifiers: Modifiers, key: egui::Key) {
    harness.key_press_modifiers(modifiers, key);
    harness.step();
}

/// The v1.0 pan distance at the default zoom of 1.0 — the same literal
/// `keyboard::tests::test_keyboard_scheme` asserts.
const V1_PAN_STEP: f32 = 40.0;

/// TIME-05. Both halves matter: the press must still pan by exactly the v1.0
/// amount, AND it must not have navigated. A binding that panned and *also*
/// stepped would pass a pan-only assertion.
#[test]
fn plain_left_and_right_still_pan_by_the_v1_amount() {
    let mut h = harness(app_with_history(EVENTS));
    assert_eq!(h.state().view.pan, egui::Vec2::ZERO, "guard: fresh view");
    assert_eq!(h.state().view.zoom, 1.0, "guard: pan step is 40/zoom");

    press(&mut h, Modifiers::NONE, egui::Key::ArrowLeft);
    assert_eq!(
        h.state().view.pan,
        egui::vec2(V1_PAN_STEP, 0.0),
        "plain Left must still pan by exactly the v1.0 amount"
    );
    assert_eq!(
        h.state().scrub_position,
        None,
        "a plain arrow must not have navigated history"
    );

    press(&mut h, Modifiers::NONE, egui::Key::ArrowRight);
    assert_eq!(
        h.state().view.pan,
        egui::Vec2::ZERO,
        "plain Right must pan back by exactly the v1.0 amount"
    );
    assert_eq!(h.state().scrub_position, None);

    // Off-scheme combinations pan too — "completely unaffected" is TIME-05's
    // own word, and Shift/Ctrl arrows pan today because no modifier check
    // exists at all.
    for modifiers in [Modifiers::SHIFT, Modifiers::CTRL] {
        press(&mut h, modifiers, egui::Key::ArrowLeft);
        assert_eq!(
            h.state().view.pan,
            egui::vec2(V1_PAN_STEP, 0.0),
            "{modifiers:?}+Left must pan"
        );
        assert_eq!(
            h.state().scrub_position,
            None,
            "{modifiers:?}+Left must not navigate"
        );
        press(&mut h, modifiers, egui::Key::ArrowRight);
        assert_eq!(h.state().view.pan, egui::Vec2::ZERO);
        assert_eq!(h.state().scrub_position, None);
    }
}

/// TIME-01. A press must do ONE thing: step, and not also pan.
#[test]
fn alt_left_steps_back_exactly_one_event() {
    let mut h = harness(app_with_history(EVENTS));
    let latest = h.state().history.next_seq() - 1;
    assert_eq!(h.state().scrub_position, None, "guard: starts Live");

    press(&mut h, Modifiers::ALT, egui::Key::ArrowLeft);

    assert_eq!(
        h.state().scrub_position,
        Some(latest - 1),
        "Alt+Left from Live must land exactly one event before the newest"
    );
    assert_eq!(
        h.state().view.pan,
        egui::Vec2::ZERO,
        "Alt+Left must NOT also pan the canvas"
    );
    assert!(
        timeline::is_paused(h.state()),
        "stepping back must leave the app Paused"
    );

    // And it is a genuine reconstruction, not the empty-model fallback.
    let shown =
        model_ids(timeline::display_model(h.state()).expect("a paused app must display a model"));
    assert!(
        shown.contains("a1"),
        "the reconstruction must hold the fixture's own nodes — an empty model \
         here means the setup skipped apply_load_outcome: {shown:?}"
    );
    assert!(
        !shown.contains(&format!("live_{:03}", EVENTS - 1)),
        "the newest event's node must NOT be in a reconstruction one step back"
    );
}

/// TIME-01 forward, plus D-02: stepping forward at the newest event is a
/// no-op that STAYS Paused. Only jump-to-latest resumes Live.
#[test]
fn alt_right_from_a_paused_position_steps_forward_one() {
    let mut h = harness(app_with_history(EVENTS));
    let latest = h.state().history.next_seq() - 1;

    press(&mut h, Modifiers::ALT, egui::Key::ArrowLeft);
    press(&mut h, Modifiers::ALT, egui::Key::ArrowLeft);
    assert_eq!(
        h.state().scrub_position,
        Some(latest - 2),
        "guard: two steps back must land two before the newest"
    );

    press(&mut h, Modifiers::ALT, egui::Key::ArrowRight);
    assert_eq!(
        h.state().scrub_position,
        Some(latest - 1),
        "Alt+Right must step forward exactly one"
    );

    press(&mut h, Modifiers::ALT, egui::Key::ArrowRight);
    assert_eq!(h.state().scrub_position, Some(latest));

    // D-02: at the newest event, pressing again holds — it must not silently
    // resume Live.
    press(&mut h, Modifiers::ALT, egui::Key::ArrowRight);
    assert_eq!(
        h.state().scrub_position,
        Some(latest),
        "stepping forward at the newest event must HOLD, never return to None"
    );
    assert!(
        timeline::is_paused(h.state()),
        "D-02: jump-to-latest is the only route back to Live"
    );
    assert_eq!(
        h.state().view.pan,
        egui::Vec2::ZERO,
        "none of those presses may have panned"
    );
}

/// TIME-02 and D-05: "earliest" is the oldest still-RETAINED event, never `0`
/// once the buffer has wrapped. Recorded over the buffer cap on purpose — at
/// a small history `evicted_count()` is `0` and the assertion could not tell
/// the two readings apart.
#[test]
fn command_left_jumps_to_the_earliest_retained_event() {
    let evicted = 30_u64;
    let total = seam_core::LIVE_BUFFER_CAPACITY + evicted as usize;
    let mut h = harness(app_with_history(total));
    assert_eq!(
        h.state().history.evicted_count(),
        evicted,
        "guard: the buffer must genuinely have wrapped, or this test cannot \
         distinguish `earliest` from `0`"
    );

    press(&mut h, Modifiers::COMMAND, egui::Key::ArrowLeft);

    assert_eq!(
        h.state().scrub_position,
        Some(h.state().history.evicted_count()),
        "D-05: Cmd+Left targets the oldest still-retained event"
    );
    assert_ne!(
        h.state().scrub_position,
        Some(0),
        "targeting 0 would name an evicted identity with nothing to replay"
    );
    assert_eq!(
        h.state().view.pan,
        egui::Vec2::ZERO,
        "Cmd+Left must not also pan"
    );

    // The reconstruction at `earliest` holds the fixture plus exactly the
    // evicted events' nodes plus the oldest retained one — proof the baseline
    // is real rather than the empty-model fallback.
    let shown =
        model_ids(timeline::display_model(h.state()).expect("a paused app must display a model"));
    assert!(
        shown.contains("a1") && shown.contains(&format!("live_{:03}", evicted)),
        "the oldest retained moment must include the fixture and every evicted \
         event's node: {shown:?}"
    );
    assert!(
        !shown.contains(&format!("live_{:03}", evicted + 1)),
        "and nothing newer than the oldest retained event"
    );
}

/// TIME-02 and D-02: Cmd+Right is the one route back to Live.
#[test]
fn command_right_resumes_live() {
    let mut h = harness(app_with_history(EVENTS));

    press(&mut h, Modifiers::ALT, egui::Key::ArrowLeft);
    assert!(
        timeline::is_paused(h.state()),
        "guard: the app must genuinely be Paused first"
    );
    let paused_ids =
        model_ids(timeline::display_model(h.state()).expect("a paused app must display a model"));
    let live_ids = model_ids(h.state().model.as_ref().expect("a loaded app has a model"));
    assert_ne!(
        paused_ids, live_ids,
        "guard: the paused view must genuinely differ from live, or 'resumed' \
         is indistinguishable from 'never left'"
    );

    press(&mut h, Modifiers::COMMAND, egui::Key::ArrowRight);

    assert_eq!(
        h.state().scrub_position,
        None,
        "Cmd+Right must resume Live, literally — not pause at the newest event"
    );
    assert!(!timeline::is_paused(h.state()));
    assert_eq!(
        model_ids(timeline::display_model(h.state()).expect("Live displays the live model")),
        live_ids,
        "the displayed model must be the live one again"
    );
    assert_eq!(h.state().view.pan, egui::Vec2::ZERO);
}

/// The focus carve-out is the first thing `handle` does, so the new scrub
/// bindings inherit its protection for free — typing a component name into
/// the search field must not navigate history any more than it currently
/// pans.
#[test]
fn a_focused_text_field_swallows_the_scrub_bindings() {
    let mut h = harness_with_focused_search(app_with_history(EVENTS));
    h.run_steps(2);

    press(&mut h, Modifiers::ALT, egui::Key::ArrowLeft);
    assert_eq!(
        h.state().scrub_position,
        None,
        "Alt+Left with the search field focused must not navigate"
    );

    press(&mut h, Modifiers::COMMAND, egui::Key::ArrowLeft);
    assert_eq!(
        h.state().scrub_position,
        None,
        "Cmd+Left with the search field focused must not navigate"
    );

    press(&mut h, Modifiers::ALT, egui::Key::ArrowRight);
    assert_eq!(h.state().scrub_position, None);

    // The pre-existing half of the carve-out, still holding.
    press(&mut h, Modifiers::NONE, egui::Key::ArrowLeft);
    assert_eq!(
        h.state().view.pan,
        egui::Vec2::ZERO,
        "a focused text field must still swallow the plain pan binding too"
    );
}

/// A `handle` restructure could quietly drop a branch. One test presses every
/// key the plan does NOT touch and asserts its existing effect still occurs.
#[test]
fn up_down_zoom_reset_and_trace_toggle_are_unchanged() {
    let mut h = harness(app_with_history(EVENTS));

    press(&mut h, Modifiers::NONE, egui::Key::ArrowUp);
    assert_eq!(h.state().view.pan, egui::vec2(0.0, V1_PAN_STEP));
    press(&mut h, Modifiers::NONE, egui::Key::ArrowDown);
    assert_eq!(h.state().view.pan, egui::Vec2::ZERO);

    press(&mut h, Modifiers::NONE, egui::Key::Plus);
    assert!((h.state().view.zoom - 1.3).abs() < 1e-5, "`+` zooms in");
    press(&mut h, Modifiers::NONE, egui::Key::Minus);
    assert!((h.state().view.zoom - 1.0).abs() < 1e-5, "`-` zooms out");
    press(&mut h, Modifiers::NONE, egui::Key::Equals);
    assert!(
        (h.state().view.zoom - 1.3).abs() < 1e-5,
        "`=` also zooms in"
    );

    press(&mut h, Modifiers::NONE, egui::Key::ArrowUp);
    assert_ne!(
        h.state().view.pan,
        ViewState::default().pan,
        "guard: the view must be genuinely off-default before `0` resets it"
    );
    press(&mut h, Modifiers::NONE, egui::Key::Num0);
    assert_eq!(h.state().view.zoom, ViewState::default().zoom, "`0` resets");
    assert_eq!(h.state().view.pan, ViewState::default().pan);

    assert!(!h.state().trace_mode, "guard: trace mode starts off");
    press(&mut h, Modifiers::NONE, egui::Key::T);
    assert!(h.state().trace_mode, "`t` toggles trace mode on");
    press(&mut h, Modifiers::NONE, egui::Key::T);
    assert!(!h.state().trace_mode, "`t` toggles it back off");

    assert_eq!(
        h.state().scrub_position,
        None,
        "and none of the untouched keys navigated history"
    );
}
