//! TRACE-01/TRACE-02: drag-to-trace state machine + first-time
//! discoverability overlay (D-14).
//!
//! `TraceGesture`/`update_gesture` are the pure drag-vs-reposition state
//! machine (RESEARCH.md Architecture Diagram, "Drag gesture on a node";
//! ported conceptually from `frontend/index.html`'s `dragStart`/`dragMove`/
//! `dragEnd` two-branch split, lines 663-746). Kept free of `egui::Ui`/
//! `egui::Context` so the whole gesture is testable without a window
//! (05-VALIDATION.md Wave 0 requirement #4) -- `graph_view::handle_trace_gesture`
//! is the only place that turns live egui `Response`/hit-testing into the
//! `GestureInput` values fed into `update_gesture`.
//!
//! In-flight gesture state has nowhere to live on `SeamExplorerApp` (frozen
//! this whole phase -- see `<cross_repo_protocol>`), so it round-trips
//! through egui's own per-frame temp storage via `load_gesture`/
//! `save_gesture`, the same pattern `graph_view::detect_reset` already uses
//! for its view-state snapshot.

/// Outcome of a single trace attempt (`seam_core::trace_path(from, to)`),
/// paired with the human-readable endpoints for the no-path message
/// (RESEARCH Pattern 5). `path: None` is the "no directed call path" case,
/// not an error -- TRACE-02's zero-crossing/no-path messages are both
/// positive/neutral framing, never an error banner.
#[derive(Debug, Clone)]
pub struct TraceResult {
    pub from: String,
    pub to: String,
    pub path: Option<seam_core::TracePath>,
}

/// The drag-vs-reposition gesture state machine (TRACE-01). `from`/`to` are
/// `seam_core::Node::id` values (not labels -- labels are looked up for
/// display only, at the panel/no-path-message layer).
#[derive(Debug, Clone, PartialEq, Default)]
pub enum TraceGesture {
    #[default]
    Idle,
    Dragging {
        from: String,
        cursor: egui::Pos2,
    },
    Completed {
        from: String,
        to: String,
    },
}

/// One frame's gesture input, already resolved from a live `egui::Response`
/// plus node hit-testing by `graph_view::handle_trace_gesture` -- this type
/// is the seam between the live/untestable half of the gesture (which node
/// is under the cursor this frame) and the pure/testable half (what state
/// that input drives the gesture to).
#[derive(Debug, Clone)]
pub enum GestureInput {
    /// A drag began while the pointer was over `node`.
    DragStart { node: String, cursor: egui::Pos2 },
    /// The pointer moved while a drag was in flight; `cursor` is the
    /// rubber-band's live endpoint (already snapped to a node's screen
    /// position by the caller when the pointer is over one).
    DragMove { cursor: egui::Pos2 },
    /// A drag ended; `node` is the node under the pointer at release, or
    /// `None` for an empty-canvas drop.
    DragStop { node: Option<String> },
}

/// Pure transition function: `(state, input, trace_mode) -> state`. With
/// `trace_mode` off, every input is a no-op that returns `Idle` -- the
/// gesture belongs entirely to `egui_graphs`' own node-reposition drag in
/// that mode (`graph_view::show` disables this module's gesture handling by
/// never feeding it inputs when `trace_mode` is false, but the function
/// itself is defensive about the parameter too, matching the D3 original's
/// `dragTraceActive` snapshot-once-per-gesture discipline).
pub fn update_gesture(state: TraceGesture, input: GestureInput, trace_mode: bool) -> TraceGesture {
    if !trace_mode {
        return TraceGesture::Idle;
    }
    match (state, input) {
        // A fresh drag always (re)starts the gesture, whether the previous
        // gesture was Idle or already Completed (a completed trace doesn't
        // block starting a new one -- trace mode stays on until re-toggled).
        (_, GestureInput::DragStart { node, cursor }) => {
            TraceGesture::Dragging { from: node, cursor }
        }

        (TraceGesture::Dragging { from, .. }, GestureInput::DragMove { cursor }) => {
            TraceGesture::Dragging { from, cursor }
        }

        // Drag-to-self is ignored -- no trace attempted, matches the D3
        // original's `best === dragFrom` silent no-op (index.html:723).
        (TraceGesture::Dragging { from, .. }, GestureInput::DragStop { node: Some(to) })
            if to == from =>
        {
            TraceGesture::Idle
        }
        (TraceGesture::Dragging { from, .. }, GestureInput::DragStop { node: Some(to) }) => {
            TraceGesture::Completed { from, to }
        }
        // Drop on empty canvas -- no trace attempted (index.html:723's
        // `!best` branch).
        (TraceGesture::Dragging { .. }, GestureInput::DragStop { node: None }) => {
            TraceGesture::Idle
        }

        // Any other (state, input) pairing -- e.g. a stray DragMove/DragStop
        // with no gesture in flight -- is a no-op.
        (state, _) => state,
    }
}

/// Thin call-through to `seam_core::trace_path` (RESEARCH Architecture
/// Diagram, Pattern map "Domain calls stay thin") -- no IPC, no lock, no
/// async, no staleness guard, unlike the Tauri command this ports
/// (`commands/trace.rs`), because there is no `await` point here for a
/// second drag to interleave across. Crossed seams are read directly off
/// the returned `TracePath.seams_crossed`, never re-derived by walking the
/// graph in this layer.
pub fn run(model: &seam_core::Model, from: &str, to: &str) -> TraceResult {
    let path = seam_core::trace_path(model, from, to);
    TraceResult {
        from: from.to_string(),
        to: to.to_string(),
        path,
    }
}

/// egui memory key for the in-flight gesture (`app.rs` is frozen this whole
/// phase and has no field for it -- kept in egui's own per-widget temp
/// storage, the same pattern `graph_view::detect_reset` already uses for its
/// view-state snapshot).
fn gesture_id() -> egui::Id {
    egui::Id::new("seam_explorer_trace_gesture")
}

/// Loads this frame's starting gesture state (defaults to `Idle` on the
/// very first frame, or if trace mode has never been used yet).
pub fn load_gesture(ui: &egui::Ui) -> TraceGesture {
    ui.data(|d| d.get_temp(gesture_id())).unwrap_or_default()
}

/// Persists the gesture state computed this frame for the next frame to
/// read via `load_gesture`.
pub fn save_gesture(ui: &mut egui::Ui, gesture: TraceGesture) {
    ui.data_mut(|d| d.insert_temp(gesture_id(), gesture));
}

/// The node under the pointer, and the pointer's own position, recorded on
/// the frame a trace button (`TRACE_BUTTONS` -- Primary or Secondary, 05-18)
/// first went down over it (G-05-5).
///
/// This exists because egui deliberately delays `Response::drag_started()`
/// (and `interact_pointer_pos()` at that moment) until the pointer has
/// moved further than `InputOptions::max_click_dist` (6.0pt, egui 0.35's
/// default click-vs-drag disambiguation threshold) from the actual
/// mouse-down point -- so by the frame `drag_started()` first fires, the
/// pointer has already left whatever node was under it at press-down,
/// which is the same order of magnitude as `SeamNodeShape::NODE_RADIUS`
/// (6.0 canvas units). The pressed node has to be captured from the
/// undelayed pointer-down frame (`Response::is_pointer_button_down_on()`)
/// or it cannot be recovered later -- the same undelayed-capture pattern
/// `egui_graphs::GraphView::handle_node_drag` already uses for its own
/// node-reposition drag.
///
/// 05-18: the capture guard additionally requires a trace button to be
/// down (not any button), and its clear condition widens to match, so a
/// capture recorded for a non-trace button (or one outliving the trace
/// buttons that created it) can never feed a stale node id into the next
/// drag start.
#[derive(Debug, Clone, PartialEq)]
pub struct PressCapture {
    pub node: String,
    pub pos: egui::Pos2,
}

/// egui memory key for the press capture -- named consistently with
/// `gesture_id`, alongside it in the same per-widget temp storage.
fn press_capture_id() -> egui::Id {
    egui::Id::new("seam_explorer_trace_press_capture")
}

/// Loads the currently recorded press capture, if any. Flattens the stored
/// `Option` (defaulting to `None` when nothing has ever been written) so
/// the caller doesn't have to distinguish "never written" from
/// "explicitly cleared" -- both read as no capture.
pub fn load_press_capture(ui: &egui::Ui) -> Option<PressCapture> {
    ui.data(|d| d.get_temp(press_capture_id())).flatten()
}

/// Records or clears the press capture. Writing `None` is the clear -- a
/// single setter for both record and clear avoids egui's `remove_temp` API,
/// which additionally requires `T: Default`.
pub fn save_press_capture(ui: &mut egui::Ui, capture: Option<PressCapture>) {
    ui.data_mut(|d| d.insert_temp(press_capture_id(), capture));
}

/// The named policy (05-18): the buttons that may START a trace gesture.
/// Primary (left) and Secondary (right) only -- Middle is deliberately
/// excluded because it is `egui_graphs`' own pan gesture
/// (`egui_graphs::graph_view.rs:1017`, `handle_pan` matches
/// `PointerButton::Middle`/`Primary`), and reserving it keeps a future
/// middle-drag-pans-during-trace-mode option open. `Extra1`/`Extra2`
/// (browser back/forward buttons) have no business starting a trace.
///
/// This constant is the single source of truth `graph_view::handle_trace_gesture`
/// consults for its drag-start detection and its `PressCapture` guard/clear
/// condition -- both must agree with this list, never re-derive it.
///
/// RED-stub history (05-18 Task 2 micro-cycle, `<tdd_discipline>`): this
/// constant was first written to list all five `PointerButton` variants --
/// a deliberately permissive stub reproducing today's actual button-agnostic
/// drag behaviour exactly, so the pure test below could FAIL against a
/// genuine measurement of the coming behaviour change rather than a
/// strawman -- then narrowed to the two buttons below. See 05-18-SUMMARY.md
/// for the captured RED/GREEN output of that cycle.
pub const TRACE_BUTTONS: &[egui::PointerButton] =
    &[egui::PointerButton::Primary, egui::PointerButton::Secondary];

/// Whether `button` may start a trace gesture, per `TRACE_BUTTONS`.
pub fn is_trace_button(button: egui::PointerButton) -> bool {
    TRACE_BUTTONS.contains(&button)
}

/// The onboarding overlay's verbatim body copy (05-UI-SPEC.md Copywriting
/// Contract, ported from `frontend/index.html:860`).
pub const ONBOARDING_BODY: &str = "Turn on Trace mode, then drag from one component to another on the canvas to see the call path between them — and which seams it crosses.";
/// The onboarding overlay's dismiss control label (verbatim,
/// `frontend/index.html:869`) -- rendered as a text-style button, not
/// icon-only (this codebase has zero icon-only interactive controls, per
/// RESEARCH.md/UI-SPEC.md).
pub const ONBOARDING_DISMISS: &str = "Got it";

fn onboarding_accent() -> egui::Color32 {
    egui::Color32::from_hex("#ff4d8d").expect("valid hex")
}

fn onboarding_muted() -> egui::Color32 {
    egui::Color32::from_hex("#93a1bd").expect("valid hex")
}

/// Renders the once-ever discoverability overlay (D-14) when
/// `app.has_seen_trace_onboarding` is false; a no-op otherwise. Anchored to
/// the top-right of the screen -- pointing at the trace-mode toggle in the
/// frozen `app.rs` top bar (Phase 3 D-05/D-06/D-07 placement) -- since this
/// module has no direct handle on that button's own `egui::Response` to
/// attach to.
///
/// Dismissal (either this function's own `ONBOARDING_DISMISS` control
/// click, or `dismiss_on_first_trace` below) writes directly to
/// `app.has_seen_trace_onboarding` -- the one field `app.rs` deliberately
/// left un-skipped for `eframe::Storage` persistence (D-14, T-05-04).
pub fn show_onboarding(ui: &mut egui::Ui, app: &mut crate::app::SeamExplorerApp) {
    if app.has_seen_trace_onboarding {
        return;
    }

    egui::Area::new(egui::Id::new("seam_explorer_trace_onboarding"))
        .anchor(egui::Align2::RIGHT_TOP, egui::vec2(-16.0, 48.0))
        .order(egui::Order::Foreground)
        .show(ui.ctx(), |ui| {
            egui::Frame::popup(ui.style())
                .stroke(egui::Stroke::new(1.0, onboarding_accent()))
                .show(ui, |ui| {
                    ui.set_max_width(260.0);
                    ui.label(ONBOARDING_BODY);
                    ui.add_space(10.0);
                    let dismiss = ui.add(
                        egui::Label::new(
                            egui::RichText::new(ONBOARDING_DISMISS)
                                .size(11.0)
                                .color(onboarding_muted()),
                        )
                        .sense(egui::Sense::click()),
                    );
                    if dismiss.clicked() {
                        app.has_seen_trace_onboarding = true;
                    }
                });
        });
}

/// Dismisses the onboarding overlay on a first successful trace (D-07 dual
/// dismissal, ported from `renderTraceResult`'s
/// `if (result && !hasSeenTraceOnboarding()) dismissTraceOnboarding();`
/// (`frontend/index.html:900`) -- note `result` there is the resolved
/// `TracePath`, so only an actually-found path dismisses, not a no-path
/// outcome; callers pass `path.is_some()`. Returns whether this call
/// actually wrote the flag (`false` when already dismissed), so repeated
/// traces after dismissal are provably cheap no-ops that never re-touch
/// storage, not just idempotent no-op *values*.
pub fn dismiss_on_first_trace(app: &mut crate::app::SeamExplorerApp) -> bool {
    if app.has_seen_trace_onboarding {
        return false;
    }
    app.has_seen_trace_onboarding = true;
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cursor(x: f32, y: f32) -> egui::Pos2 {
        egui::pos2(x, y)
    }

    /// Pure, window-free proof of the trace-button policy (05-18) over all
    /// five `egui::PointerButton` variants explicitly -- no `egui::Context`,
    /// no harness. Primary and Secondary must start a trace; Middle (reserved
    /// for `egui_graphs`' own pan gesture) and both Extra buttons must not.
    #[test]
    fn is_trace_button_matches_the_named_policy_for_every_button() {
        assert!(
            is_trace_button(egui::PointerButton::Primary),
            "left-button drag must start a trace"
        );
        assert!(
            is_trace_button(egui::PointerButton::Secondary),
            "right-button drag must start a trace -- the user's explicit request"
        );
        assert!(
            !is_trace_button(egui::PointerButton::Middle),
            "middle-button drag must NOT start a trace -- reserved for egui_graphs' own pan \
             gesture"
        );
        assert!(
            !is_trace_button(egui::PointerButton::Extra1),
            "Extra1 (browser back) must not start a trace"
        );
        assert!(
            !is_trace_button(egui::PointerButton::Extra2),
            "Extra2 (browser forward) must not start a trace"
        );
    }

    /// The exact test name 05-VALIDATION.md's coverage map requires for
    /// TRACE-01/02: with trace mode on, drag-start over a node moves
    /// `Idle` -> `Dragging`; drag-stop over a different node moves it to
    /// `Completed` carrying both endpoints; drag-stop over empty canvas
    /// returns to `Idle` with no trace attempted.
    #[test]
    fn test_trace_state_machine() {
        let mut state = TraceGesture::Idle;
        state = update_gesture(
            state,
            GestureInput::DragStart {
                node: "a".to_string(),
                cursor: cursor(1.0, 1.0),
            },
            true,
        );
        assert!(matches!(&state, TraceGesture::Dragging { from, .. } if from == "a"));

        state = update_gesture(
            state,
            GestureInput::DragStop {
                node: Some("b".to_string()),
            },
            true,
        );
        assert_eq!(
            state,
            TraceGesture::Completed {
                from: "a".to_string(),
                to: "b".to_string(),
            }
        );

        // Drop on empty canvas -> Idle, no trace attempted.
        let mut state2 = TraceGesture::Idle;
        state2 = update_gesture(
            state2,
            GestureInput::DragStart {
                node: "a".to_string(),
                cursor: cursor(1.0, 1.0),
            },
            true,
        );
        state2 = update_gesture(state2, GestureInput::DragStop { node: None }, true);
        assert_eq!(state2, TraceGesture::Idle);
    }

    /// With trace mode off, the identical input sequence leaves the state
    /// `Idle` -- the drag belongs to the reposition branch instead.
    #[test]
    fn test_trace_mode_off_does_not_trace() {
        let mut state = TraceGesture::Idle;
        state = update_gesture(
            state,
            GestureInput::DragStart {
                node: "a".to_string(),
                cursor: cursor(1.0, 1.0),
            },
            false,
        );
        assert_eq!(state, TraceGesture::Idle);

        state = update_gesture(
            state,
            GestureInput::DragStop {
                node: Some("b".to_string()),
            },
            false,
        );
        assert_eq!(state, TraceGesture::Idle);
    }

    /// Dragging from a node back onto itself returns to `Idle` without
    /// attempting a trace.
    #[test]
    fn test_drag_to_self_is_ignored() {
        let mut state = TraceGesture::Idle;
        state = update_gesture(
            state,
            GestureInput::DragStart {
                node: "a".to_string(),
                cursor: cursor(1.0, 1.0),
            },
            true,
        );
        state = update_gesture(
            state,
            GestureInput::DragStop {
                node: Some("a".to_string()),
            },
            true,
        );
        assert_eq!(state, TraceGesture::Idle);
    }

    /// Completing a trace does NOT clear `trace_mode` -- it stays on until
    /// explicitly toggled (Phase 3's locked behavior). `trace_mode` lives
    /// outside `TraceGesture` entirely (it's `app.trace_mode`, passed in
    /// fresh every call), so this test proves a second gesture can start
    /// immediately after a `Completed` result with `trace_mode` still
    /// `true`, with no special "re-arm" step required.
    #[test]
    fn test_trace_mode_persists_after_completion() {
        let mut state = TraceGesture::Idle;
        state = update_gesture(
            state,
            GestureInput::DragStart {
                node: "a".to_string(),
                cursor: cursor(0.0, 0.0),
            },
            true,
        );
        state = update_gesture(
            state,
            GestureInput::DragStop {
                node: Some("b".to_string()),
            },
            true,
        );
        assert_eq!(
            state,
            TraceGesture::Completed {
                from: "a".to_string(),
                to: "b".to_string(),
            }
        );

        // trace_mode is still true here (never mutated by update_gesture) --
        // a new drag starts a fresh gesture with no extra re-arm step.
        state = update_gesture(
            state,
            GestureInput::DragStart {
                node: "c".to_string(),
                cursor: cursor(2.0, 2.0),
            },
            true,
        );
        assert_eq!(
            state,
            TraceGesture::Dragging {
                from: "c".to_string(),
                cursor: cursor(2.0, 2.0),
            }
        );
    }

    /// `save_press_capture`/`load_press_capture` round-trip a `PressCapture`
    /// (G-05-5): loading before any write returns `None`; saving `Some`
    /// then loading returns the same value; saving `None` clears it back
    /// to `None` -- the single-setter clear discipline `PressCapture`'s
    /// own doc comment describes.
    #[test]
    fn press_capture_round_trips_through_egui_memory() {
        let ctx = egui::Context::default();
        let raw_input = egui::RawInput::default();
        let _ = ctx.run_ui(raw_input, |ui| {
            assert_eq!(
                load_press_capture(ui),
                None,
                "no capture has ever been written yet"
            );

            let capture = PressCapture {
                node: "a1".to_string(),
                pos: cursor(12.0, 34.0),
            };
            save_press_capture(ui, Some(capture.clone()));
            assert_eq!(load_press_capture(ui), Some(capture));

            save_press_capture(ui, None);
            assert_eq!(
                load_press_capture(ui),
                None,
                "writing None must clear a previously recorded capture"
            );
        });
    }

    /// Activating the `ONBOARDING_DISMISS` control sets
    /// `has_seen_trace_onboarding`.
    #[test]
    fn onboarding_dismiss_sets_flag() {
        let app = crate::app::SeamExplorerApp::default();
        assert!(!app.has_seen_trace_onboarding);

        let mut harness = egui_kittest::Harness::new_ui_state(
            |ui, app: &mut crate::app::SeamExplorerApp| {
                show_onboarding(ui, app);
            },
            app,
        );
        harness.run();

        use egui_kittest::kittest::Queryable as _;
        harness.get_by_label(ONBOARDING_DISMISS).click();
        harness.run();

        assert!(harness.state().has_seen_trace_onboarding);
    }

    /// A first successful trace (a resolved path) sets the flag; a second
    /// trace after dismissal is a cheap no-op that does not re-touch
    /// storage (proven by the `false` return, not just the post-condition
    /// value staying `true`).
    #[test]
    fn onboarding_dismissed_by_first_successful_trace() {
        let mut app = crate::app::SeamExplorerApp::default();
        assert!(!app.has_seen_trace_onboarding);

        assert!(
            dismiss_on_first_trace(&mut app),
            "first successful trace must actually write the flag"
        );
        assert!(app.has_seen_trace_onboarding);

        assert!(
            !dismiss_on_first_trace(&mut app),
            "a second trace after dismissal must be a no-op, not a re-write"
        );
        assert!(app.has_seen_trace_onboarding);
    }

    /// Serializing the app struct and deserializing it preserves the flag
    /// as `true`, and -- critically -- does NOT carry any runtime field
    /// through (T-05-04): `search_query` and `model` are both
    /// `#[serde(skip)]` on `SeamExplorerApp`, so graph contents and other
    /// session state never reach the persisted bytes, let alone survive a
    /// round trip.
    #[test]
    fn onboarding_flag_survives_round_trip() {
        let app = crate::app::SeamExplorerApp {
            has_seen_trace_onboarding: true,
            search_query: "should not persist".to_string(),
            ..Default::default()
        };

        let serialized = serde_json::to_string(&app).expect("app must serialize");
        assert!(
            !serialized.contains("should not persist"),
            "a skipped runtime field must never reach the serialized bytes at all"
        );

        let restored: crate::app::SeamExplorerApp =
            serde_json::from_str(&serialized).expect("app must deserialize");
        assert!(restored.has_seen_trace_onboarding);
        assert!(restored.model.is_none());
        assert_eq!(restored.search_query, String::new());
    }
}
