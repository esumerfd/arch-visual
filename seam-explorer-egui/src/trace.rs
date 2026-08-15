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
}
