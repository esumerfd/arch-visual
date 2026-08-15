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
