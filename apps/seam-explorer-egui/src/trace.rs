//! TRACE-01/TRACE-02: click-to-arm / click-to-complete state machine (D-01
//! through D-04, quick-260926-gb2) + first-time discoverability overlay
//! (D-14).
//!
//! `TraceGesture`/`update_gesture` are the pure arm-vs-complete state
//! machine. With Trace mode on, a primary click on a node ARMS a trace from
//! it; a later primary click on a DIFFERENT node COMPLETES it immediately --
//! it never re-arms and never needs a cancel-then-restart round trip (D-03).
//! A click on the armed node itself, a click on empty canvas, or an explicit
//! `Cancel` input (Escape, wired in `keyboard::handle`) all return the
//! gesture to `Idle` (D-02, DP-GB2-01). Kept free of `egui::Ui`/
//! `egui::Context` so the whole gesture is testable without a window
//! (05-VALIDATION.md Wave 0 requirement #4) -- `graph_view::handle_trace_gesture`
//! is the only place that turns a live egui `Response`/node hit-test into the
//! `GestureInput` values fed into `update_gesture`.
//!
//! quick-260926-gb2 replaced the previous continuous drag-to-trace gesture
//! (mouse-down on the source, drag, release on the destination, all in one
//! uninterrupted motion) outright, rather than keeping it alongside this one
//! (D-04): on a busy graph the destination node is often off-screen, and
//! there was no way to pan or zoom mid-drag to reach it. Click-to-arm /
//! click-to-complete decouples node selection from a continuous pointer
//! gesture entirely, so the user can pan and zoom freely between the two
//! clicks -- the whole reason for the redesign.
//!
//! The gesture's state lives on `SeamExplorerApp::trace_gesture`, not in
//! egui's own per-frame temp storage the way the old drag machine's state
//! did. Two consumers outside the canvas handler need to read it with no
//! `egui::Ui` in hand to reach temp memory through: `graph_view::apply_focus_styling`
//! (the armed ring, `&SeamExplorerApp`, no `ui` parameter) and
//! `keyboard::handle` (the Escape cancel), which is why the field moved onto
//! the app struct instead (finding 6).

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

/// The arm-vs-complete gesture state machine (TRACE-01, quick-260926-gb2).
/// `from`/`to` are `seam_core::Node::id` values (not labels -- labels are
/// looked up for display only, at the banner/panel/no-path-message layer).
#[derive(Debug, Clone, PartialEq, Default)]
pub enum TraceGesture {
    #[default]
    Idle,
    Armed {
        from: String,
    },
    Completed {
        from: String,
        to: String,
    },
}

impl TraceGesture {
    /// The armed node's id, or `None` in every state but `Armed`. Read by
    /// `graph_view::apply_focus_styling` (`&SeamExplorerApp`, no `ui`
    /// parameter, so it cannot see egui temp memory -- finding 6) to OR the
    /// armed ring into the same `selected` flag quick-260915-sf7's
    /// click-to-jump highlight drives (DP-GB2-06: both may ring at once).
    pub fn armed_node(&self) -> Option<&str> {
        match self {
            TraceGesture::Armed { from } => Some(from.as_str()),
            _ => None,
        }
    }
}

/// One frame's gesture input, already resolved from a live `egui::Response`
/// plus node hit-testing by `graph_view::handle_trace_gesture` -- this type
/// is the seam between the live/untestable half of the gesture (which node,
/// if any, was clicked this frame, or whether Escape was pressed) and the
/// pure/testable half (what state that input drives the gesture to).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GestureInput {
    /// A primary click landed on `node`.
    NodeClick { node: String },
    /// A primary click landed on empty canvas -- no node under the pointer.
    EmptyClick,
    /// The Escape key was pressed (D-02's third cancel, wired in Task 3's
    /// `keyboard::handle`). Defined here, alongside the other two inputs,
    /// so the machine is complete in this task and Task 3 adds no new
    /// variant.
    Cancel,
}

/// Pure transition function: `(state, input, trace_mode) -> state`. With
/// `trace_mode` off, every input is a no-op that returns `Idle` -- the
/// gesture belongs entirely to `egui_graphs`' own node-reposition drag in
/// that mode (`graph_view::show` disables this module's gesture handling by
/// never feeding it inputs when `trace_mode` is false, but the function
/// itself is defensive about the parameter too).
pub fn update_gesture(state: TraceGesture, input: GestureInput, trace_mode: bool) -> TraceGesture {
    if !trace_mode {
        return TraceGesture::Idle;
    }
    match (state, input) {
        // Escape always returns to Idle, from any state -- one of D-02's
        // three cancel gestures (the other two -- empty-canvas click and
        // toggling trace mode off -- are the `EmptyClick` arm below and the
        // `!trace_mode` early return above).
        (_, GestureInput::Cancel) => TraceGesture::Idle,

        // Idle + a node click arms the trace.
        (TraceGesture::Idle, GestureInput::NodeClick { node }) => {
            TraceGesture::Armed { from: node }
        }

        // Armed + a click on the SAME node cancels rather than completing a
        // self-trace (DP-GB2-01): `seam_core::trace_path(x, x)` resolves to
        // a real but information-free one-node path (finding 12), and this
        // preserves the shipped "source == destination is a no-op" semantic
        // the old drag machine already had (there ported from the D3
        // original's `best === dragFrom` guard).
        (TraceGesture::Armed { from }, GestureInput::NodeClick { node }) if node == from => {
            TraceGesture::Idle
        }
        // Armed + a click on any OTHER node completes the trace immediately
        // (D-03) -- it never re-arms and never needs a cancel-then-restart
        // round trip.
        (TraceGesture::Armed { from }, GestureInput::NodeClick { node }) => {
            TraceGesture::Completed { from, to: node }
        }
        // Armed + a click on empty canvas cancels (D-02).
        (TraceGesture::Armed { .. }, GestureInput::EmptyClick) => TraceGesture::Idle,

        // Completed + a node click starts a fresh arm -- a resolved trace
        // never blocks starting a new one; trace mode stays on until
        // explicitly re-toggled.
        (TraceGesture::Completed { .. }, GestureInput::NodeClick { node }) => {
            TraceGesture::Armed { from: node }
        }

        // Any other (state, input) pairing -- Idle/Completed + EmptyClick --
        // is a no-op.
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

/// The onboarding overlay's body copy. Originally ported verbatim from
/// `frontend/index.html:860` (05-UI-SPEC.md Copywriting Contract); rewritten
/// by quick-260926-gb2 because the ported wording taught a gesture (drag
/// from one node to another) the app no longer has -- it now instructs the
/// click-to-arm / click-to-complete replacement instead (finding 13).
pub const ONBOARDING_BODY: &str = "Turn on Trace mode, then click one component and click another to see the call path between them — and which seams it crosses.";
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

/// The armed-state banner's exact wording (DP-GB2-07), built as a pure
/// function so it is unit-testable independent of rendering. `name` is the
/// armed node's DISPLAY label (resolved by the caller through
/// `panels::detail::node_label`, finding 14), never its raw id. Em dash
/// matches `ONBOARDING_BODY`'s existing punctuation style.
pub fn armed_banner_text(name: &str) -> String {
    format!("Tracing from {name} — click a destination node")
}

/// Renders the armed-state banner (D-01), naming the node a trace is armed
/// from. Modelled structurally on `show_onboarding` above -- a
/// `Foreground`-order `egui::Area` wrapping an `egui::Frame::popup` with the
/// same accent stroke -- but anchored `LEFT_TOP` at `(16.0, 16.0)`, the
/// opposite corner from the onboarding card's `RIGHT_TOP` anchor, so the two
/// can never overlap (DP-GB2-07).
pub fn show_armed_banner(ui: &mut egui::Ui, name: &str) {
    egui::Area::new(egui::Id::new("seam_explorer_trace_armed_banner"))
        .anchor(egui::Align2::LEFT_TOP, egui::vec2(16.0, 16.0))
        .order(egui::Order::Foreground)
        .show(ui.ctx(), |ui| {
            egui::Frame::popup(ui.style())
                .stroke(egui::Stroke::new(1.0, onboarding_accent()))
                .show(ui, |ui| {
                    ui.label(armed_banner_text(name));
                });
        });
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

    fn click(node: &str) -> GestureInput {
        GestureInput::NodeClick {
            node: node.to_string(),
        }
    }

    /// The exact test name 05-VALIDATION.md's coverage map requires for
    /// TRACE-01/02: with trace mode on, a click on a node arms it; a click
    /// on a DIFFERENT node completes it, carrying both endpoints; a click on
    /// empty canvas after arming returns to `Idle` with no trace attempted.
    #[test]
    fn test_trace_state_machine() {
        let mut state = TraceGesture::Idle;
        state = update_gesture(state, click("a"), true);
        assert_eq!(
            state,
            TraceGesture::Armed {
                from: "a".to_string()
            }
        );

        state = update_gesture(state, click("b"), true);
        assert_eq!(
            state,
            TraceGesture::Completed {
                from: "a".to_string(),
                to: "b".to_string(),
            }
        );

        // Arm again, then click empty canvas -> Idle, no trace attempted.
        let mut state2 = TraceGesture::Idle;
        state2 = update_gesture(state2, click("a"), true);
        state2 = update_gesture(state2, GestureInput::EmptyClick, true);
        assert_eq!(state2, TraceGesture::Idle);
    }

    /// With trace mode off, the identical input sequence leaves the state
    /// `Idle` -- the machine belongs entirely to trace mode.
    #[test]
    fn test_trace_mode_off_does_not_trace() {
        let mut state = TraceGesture::Idle;
        state = update_gesture(state, click("a"), false);
        assert_eq!(state, TraceGesture::Idle);

        state = update_gesture(state, click("b"), false);
        assert_eq!(state, TraceGesture::Idle);
    }

    /// Clicking the armed node again returns to `Idle` without attempting a
    /// trace (DP-GB2-01): `seam_core::trace_path(x, x)` resolves to a real
    /// but information-free one-node path (finding 12), and the existing,
    /// tested, D3-ported rule already treats source == destination as a
    /// no-op -- this preserves that shipped semantic rather than inventing a
    /// new one. Click-to-arm / click-again-to-disarm also reads as a fourth
    /// member of D-02's cancel set.
    #[test]
    fn clicking_the_armed_node_again_cancels() {
        let mut state = TraceGesture::Idle;
        state = update_gesture(state, click("a"), true);
        state = update_gesture(state, click("a"), true);
        assert_eq!(state, TraceGesture::Idle);
    }

    /// Completing a trace does NOT clear `trace_mode` -- it stays on until
    /// explicitly toggled (Phase 3's locked behavior). `trace_mode` lives
    /// outside `TraceGesture` entirely (it's `app.trace_mode`, passed in
    /// fresh every call), so this test proves a second gesture can ARM
    /// immediately after a `Completed` result with `trace_mode` still
    /// `true`, with no special "re-arm" step required.
    #[test]
    fn test_trace_mode_persists_after_completion() {
        let mut state = TraceGesture::Idle;
        state = update_gesture(state, click("a"), true);
        state = update_gesture(state, click("b"), true);
        assert_eq!(
            state,
            TraceGesture::Completed {
                from: "a".to_string(),
                to: "b".to_string(),
            }
        );

        // trace_mode is still true here (never mutated by update_gesture) --
        // a new click arms a fresh gesture with no extra re-arm step.
        state = update_gesture(state, click("c"), true);
        assert_eq!(
            state,
            TraceGesture::Armed {
                from: "c".to_string()
            }
        );
    }

    /// `armed_node()` returns the armed id only while `Armed`, and `None` in
    /// every other state -- the pure half of the armed-ring wiring
    /// (`graph_view::apply_focus_styling`, Task 2).
    #[test]
    fn armed_node_reflects_the_armed_state_only() {
        assert_eq!(TraceGesture::Idle.armed_node(), None);
        assert_eq!(
            TraceGesture::Armed {
                from: "a".to_string()
            }
            .armed_node(),
            Some("a")
        );
        assert_eq!(
            TraceGesture::Completed {
                from: "a".to_string(),
                to: "b".to_string(),
            }
            .armed_node(),
            None
        );
    }

    /// `armed_banner_text`'s exact wording is pinned here (DP-GB2-07), not
    /// only by the rendered query below -- an em dash, no backticks, and the
    /// node's DISPLAY label verbatim.
    #[test]
    fn armed_banner_text_matches_the_locked_wording() {
        assert_eq!(
            armed_banner_text("mockagentruntime"),
            "Tracing from mockagentruntime — click a destination node"
        );
    }

    /// An armed state renders a banner naming the node; an idle state
    /// renders none -- the shape of `onboarding_dismiss_sets_flag` above,
    /// adapted to a presence/absence query rather than a click.
    #[test]
    fn armed_banner_appears_only_while_armed() {
        let mut harness = egui_kittest::Harness::new_ui_state(
            |ui, armed: &mut Option<String>| {
                if let Some(name) = armed {
                    show_armed_banner(ui, name);
                }
            },
            None::<String>,
        );
        harness.run();

        use egui_kittest::kittest::Queryable as _;
        assert!(
            harness
                .query_all_by_label_contains("Tracing from")
                .next()
                .is_none(),
            "no banner label may be present while idle"
        );

        *harness.state_mut() = Some("mockagentruntime".to_string());
        harness.run();

        harness.get_by_label_contains("Tracing from mockagentruntime");
    }

    /// The onboarding card no longer teaches the removed drag gesture
    /// (finding 13): checked against the CONSTANT, not a literal copy of its
    /// text, so a future reword of `ONBOARDING_BODY` is what this test
    /// actually exercises.
    #[test]
    fn onboarding_body_no_longer_instructs_a_drag() {
        let body = ONBOARDING_BODY.to_lowercase();
        assert!(
            !body.contains("drag"),
            "ONBOARDING_BODY must not instruct the user to drag: {ONBOARDING_BODY:?}"
        );
        assert!(
            body.contains("click"),
            "ONBOARDING_BODY must instruct the user to click: {ONBOARDING_BODY:?}"
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
