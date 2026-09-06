//! NAV-05: single global keyboard dispatch, mirroring the D3 app's one
//! `keydown` listener (Phase 2 D-09) — arrows pan, `+`/`=` zoom in, `-` zoom
//! out, `0` resets, `t` toggles trace mode, with a focus carve-out for ANY
//! focused text edit (the egui equivalent of the original's
//! `document.activeElement === searchInput` guard, widened -- 05-22 -- to
//! cover the settings panel's Open-file command field alongside the
//! original search field: a second text field now lives on this surface,
//! and a guard scoped to only one of them would let typing in the other
//! drive the canvas, T-05-22-01).
//!
//! `apply_key` is a pure `ViewState -> ViewState` transform (design doc
//! §10) so the whole scheme is unit-testable with no live `egui::Context` --
//! `handle` is the only place that touches `ctx`/`Ui`, and it is the single
//! per-frame input-polling site in the whole crate (mirrors the original's
//! "only ever one keydown listener" discipline).
//!
//! Constants ported verbatim from `frontend/index.html:1118-1161`
//! (RESEARCH.md Pattern 3): pan step 40px divided by current zoom, zoom
//! factor ×1.3 / ÷1.3. `egui::Key` variant names below were verified against
//! the pinned 0.35.0 source (`~/.cargo/registry/src/*/egui-0.35.0/src/data/key.rs`)
//! before writing this match, closing RESEARCH Assumption A4.

use crate::app::{SeamExplorerApp, ViewState};

/// Pan step in screen-space px, divided by the current zoom before applying
/// (verbatim port of the D3 original's `PAN_STEP / t.k`).
const PAN_STEP: f32 = 40.0;

/// Zoom multiplier for `+`/`=`; `-` applies its reciprocal (verbatim port).
const ZOOM_FACTOR: f32 = 1.3;

/// The closed set of view/mode mutations a keypress can trigger. `ToggleTrace`
/// is included here (rather than handled as a separate ad-hoc branch in
/// `handle`) so the full key scheme is expressed as one enum, even though it
/// mutates `trace_mode` (via `apply_trace_toggle`), not the view -- `apply_key`
/// passes the view through unchanged for this variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyAction {
    PanLeft,
    PanRight,
    PanUp,
    PanDown,
    ZoomIn,
    ZoomOut,
    Reset,
    ToggleTrace,
}

/// Pure view-state transform for every `KeyAction` except `ToggleTrace`
/// (passed through unchanged -- trace-mode toggling is a separate pure
/// function, `apply_trace_toggle`, since it mutates a `bool` not a
/// `ViewState`). No `egui::Ui`/`egui::Context` parameter -- fully
/// unit-testable in isolation.
pub fn apply_key(view: ViewState, action: KeyAction) -> ViewState {
    let step = PAN_STEP / view.zoom;
    match action {
        KeyAction::PanLeft => ViewState {
            pan: view.pan + egui::vec2(step, 0.0),
            ..view
        },
        KeyAction::PanRight => ViewState {
            pan: view.pan + egui::vec2(-step, 0.0),
            ..view
        },
        KeyAction::PanUp => ViewState {
            pan: view.pan + egui::vec2(0.0, step),
            ..view
        },
        KeyAction::PanDown => ViewState {
            pan: view.pan + egui::vec2(0.0, -step),
            ..view
        },
        KeyAction::ZoomIn => ViewState {
            zoom: view.zoom * ZOOM_FACTOR,
            ..view
        },
        KeyAction::ZoomOut => ViewState {
            zoom: view.zoom / ZOOM_FACTOR,
            ..view
        },
        KeyAction::Reset => ViewState::default(),
        KeyAction::ToggleTrace => view,
    }
}

/// Pure `bool -> bool` flip for `t`'s trace-mode toggle -- kept separate
/// from `apply_key` since it mutates `app.trace_mode`, not `app.view`.
pub fn apply_trace_toggle(trace_mode: bool) -> bool {
    !trace_mode
}

/// The two arrows that take modifiers (09-04). Up and Down keep their
/// existing unconditional pan branches and are deliberately NOT modelled
/// here -- the locked scheme gives them no modified meaning, and adding
/// them would invite a future reader to think one exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arrow {
    Left,
    Right,
}

/// What a horizontal arrow press means once its modifiers are read: either
/// the v1.0 canvas pan, or a history navigation.
///
/// Modelled as data rather than performed inline so the whole decision is
/// testable with no `egui::Context` -- the same discipline `apply_key`
/// established for the unmodified scheme.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArrowRoute {
    Pan(KeyAction),
    Scrub(crate::timeline::TimelineAction),
}

/// PURE modifier dispatch for the horizontal arrows (TIME-01/TIME-02/TIME-05).
/// No `egui::Context`, no `Ui`, no `app`.
///
/// | condition | Left | Right |
/// |---|---|---|
/// | `modifiers.command` | `Scrub(JumpEarliest)` | `Scrub(JumpLatest)` |
/// | else `modifiers.alt` | `Scrub(StepBack)` | `Scrub(StepForward)` |
/// | else | `Pan(PanLeft)` | `Pan(PanRight)` |
///
/// **Command is tested BEFORE Alt, deliberately.** Holding both must resolve
/// to the jump rather than to whichever branch happens to come first after a
/// later edit. A reader cannot recover a precedence *choice* from an
/// `if`/`else if` chain alone, so it is stated here and pinned by
/// `command_wins_over_alt_when_both_are_held`.
///
/// **Every combination outside the locked scheme pans**, by falling through
/// rather than by an enumerated allowlist that could miss one. Shift+Left
/// pans, Ctrl+Left pans, Shift+Alt+Ctrl+Left pans -- all of them pan today,
/// because this file has never had a modifier check at all, and TIME-05's
/// word is "completely unaffected". Reading only `command` and `alt` gives
/// that for free.
///
/// **Why `modifiers.command` and not `mac_cmd`** (09-RESEARCH.md, verified
/// against the pinned egui 0.35.0 source): on this macOS-only app `command`
/// mirrors `mac_cmd` by egui's own design, so no `cfg(target_os)` branch is
/// needed, and `Modifiers::COMMAND` in a test sets exactly the field the
/// production code reads.
///
/// The `Scrub` arms name a [`crate::timeline::TimelineAction`] and compute no
/// navigation arithmetic of their own. That delegation is what makes the
/// keyboard and 09-05's on-screen buttons structurally incapable of drifting
/// apart (ROADMAP SC-1) -- both hand the same four actions to the same
/// `timeline::apply_action`.
pub fn route_arrow(arrow: Arrow, modifiers: egui::Modifiers) -> ArrowRoute {
    use crate::timeline::TimelineAction;

    if modifiers.command {
        match arrow {
            Arrow::Left => ArrowRoute::Scrub(TimelineAction::JumpEarliest),
            Arrow::Right => ArrowRoute::Scrub(TimelineAction::JumpLatest),
        }
    } else if modifiers.alt {
        match arrow {
            Arrow::Left => ArrowRoute::Scrub(TimelineAction::StepBack),
            Arrow::Right => ArrowRoute::Scrub(TimelineAction::StepForward),
        }
    } else {
        match arrow {
            Arrow::Left => ArrowRoute::Pan(KeyAction::PanLeft),
            Arrow::Right => ArrowRoute::Pan(KeyAction::PanRight),
        }
    }
}

/// The single per-frame input dispatch (NAV-05) -- called once from
/// `app.rs`'s already-wired call site. Checks the focus carve-out first
/// (the egui equivalent of the original's `activeElement` guard): if
/// EITHER the search `TextEdit` identified by `search_id` currently holds
/// keyboard focus, OR any other text-edit widget on screen does
/// (`Context::text_edit_focused`, true whenever the currently focused
/// widget has a loaded `TextEditState` -- e.g. the settings panel's
/// Open-file command field, 05-22), every shortcut below is skipped for
/// this frame. Widened (05-22) from a search-only guard: a second text
/// field now lives on this surface (`settings_panel::show`), and typing a
/// command like `code -g` into it must not zoom, pan, reset or toggle
/// trace mode any more than typing a component name into the search field
/// does.
pub fn handle(ctx: &egui::Context, app: &mut SeamExplorerApp, search_id: egui::Id) {
    if ctx.memory(|m| m.has_focus(search_id)) || ctx.text_edit_focused() {
        return;
    }

    ctx.input(|i| {
        if i.key_pressed(egui::Key::ArrowLeft) {
            app.view = apply_key(app.view, KeyAction::PanLeft);
        }
        if i.key_pressed(egui::Key::ArrowRight) {
            app.view = apply_key(app.view, KeyAction::PanRight);
        }
        if i.key_pressed(egui::Key::ArrowUp) {
            app.view = apply_key(app.view, KeyAction::PanUp);
        }
        if i.key_pressed(egui::Key::ArrowDown) {
            app.view = apply_key(app.view, KeyAction::PanDown);
        }
        if i.key_pressed(egui::Key::Plus) || i.key_pressed(egui::Key::Equals) {
            app.view = apply_key(app.view, KeyAction::ZoomIn);
        }
        if i.key_pressed(egui::Key::Minus) {
            app.view = apply_key(app.view, KeyAction::ZoomOut);
        }
        if i.key_pressed(egui::Key::Num0) {
            crate::graph_view::reset_view(app);
        }
        if i.key_pressed(egui::Key::T) {
            app.trace_mode = apply_trace_toggle(app.trace_mode);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The exact test name 05-VALIDATION.md's map requires
    /// (`cargo test --lib test_keyboard_scheme`). Asserts the actual ported
    /// numbers, not just that something changed: pan moves by 40px (at
    /// zoom 1.0) in the correct direction for each of the four arrow keys,
    /// zoom-in multiplies by 1.3, zoom-out divides by 1.3, and reset returns
    /// the fit-to-view (`ViewState::default()`) state.
    #[test]
    fn test_keyboard_scheme() {
        let base = ViewState {
            zoom: 1.0,
            pan: egui::Vec2::ZERO,
        };

        let left = apply_key(base, KeyAction::PanLeft);
        assert_eq!(left.pan, egui::vec2(40.0, 0.0));
        assert_eq!(left.zoom, 1.0);

        let right = apply_key(base, KeyAction::PanRight);
        assert_eq!(right.pan, egui::vec2(-40.0, 0.0));

        let up = apply_key(base, KeyAction::PanUp);
        assert_eq!(up.pan, egui::vec2(0.0, 40.0));

        let down = apply_key(base, KeyAction::PanDown);
        assert_eq!(down.pan, egui::vec2(0.0, -40.0));

        let zoomed_in = apply_key(base, KeyAction::ZoomIn);
        assert!((zoomed_in.zoom - 1.3).abs() < 1e-5);

        let zoomed_out = apply_key(base, KeyAction::ZoomOut);
        assert!((zoomed_out.zoom - (1.0 / 1.3)).abs() < 1e-5);

        let moved = ViewState {
            zoom: 2.0,
            pan: egui::vec2(5.0, 5.0),
        };
        let reset = apply_key(moved, KeyAction::Reset);
        assert_eq!(reset.zoom, ViewState::default().zoom);
        assert_eq!(reset.pan, ViewState::default().pan);
    }

    /// At zoom 2.0 a pan moves half the world-space distance it does at
    /// zoom 1.0 -- the behavior the original's division by the transform's
    /// scale (`PAN_STEP / t.k`) produced.
    #[test]
    fn test_pan_step_scales_inversely_with_zoom() {
        let at_1 = ViewState {
            zoom: 1.0,
            pan: egui::Vec2::ZERO,
        };
        let at_2 = ViewState {
            zoom: 2.0,
            pan: egui::Vec2::ZERO,
        };

        let moved_1 = apply_key(at_1, KeyAction::PanLeft);
        let moved_2 = apply_key(at_2, KeyAction::PanLeft);

        assert!((moved_2.pan.x - moved_1.pan.x / 2.0).abs() < 1e-5);
    }

    /// Applying zoom-in then zoom-out returns the original zoom within
    /// float tolerance.
    #[test]
    fn test_zoom_in_then_out_is_identity() {
        let base = ViewState {
            zoom: 1.0,
            pan: egui::Vec2::ZERO,
        };
        let round_trip = apply_key(apply_key(base, KeyAction::ZoomIn), KeyAction::ZoomOut);
        assert!((round_trip.zoom - base.zoom).abs() < 1e-5);
    }

    /// The toggle action flips `trace_mode` (via `apply_trace_toggle`) and
    /// leaves the view untouched (`apply_key`'s `ToggleTrace` arm is a pass
    /// through).
    #[test]
    fn test_toggle_trace_flips_mode() {
        assert!(apply_trace_toggle(false));
        assert!(!apply_trace_toggle(true));

        let view = ViewState {
            zoom: 1.5,
            pan: egui::vec2(3.0, 4.0),
        };
        let untouched = apply_key(view, KeyAction::ToggleTrace);
        assert_eq!(untouched.zoom, view.zoom);
        assert_eq!(untouched.pan, view.pan);
    }

    // ---- 09-04: the arrow-key modifier decision table (TIME-01/02/05) ----

    use crate::timeline::TimelineAction;
    use egui::Modifiers;

    #[test]
    fn plain_arrows_route_to_pan() {
        assert_eq!(
            route_arrow(Arrow::Left, Modifiers::NONE),
            ArrowRoute::Pan(KeyAction::PanLeft)
        );
        assert_eq!(
            route_arrow(Arrow::Right, Modifiers::NONE),
            ArrowRoute::Pan(KeyAction::PanRight)
        );
    }

    /// TIME-05's real content. Not merely "the bare arrow still pans" -- that
    /// the new modifier checks did not accidentally CAPTURE a combination that
    /// used to pan. Every one of these pans today, because `handle` has no
    /// modifier check at all, and TIME-05's word is "completely unaffected".
    ///
    /// `Modifiers::CTRL` leaves `command` FALSE (egui folds Ctrl into `command`
    /// only on non-Mac; this app is macOS-only and `command` mirrors `mac_cmd`),
    /// which is exactly what makes Ctrl+Left a pan on this platform rather than
    /// an accidental jump-to-earliest.
    #[test]
    fn shift_and_ctrl_arrows_still_route_to_pan() {
        for modifiers in [Modifiers::SHIFT, Modifiers::CTRL] {
            assert_eq!(
                route_arrow(Arrow::Left, modifiers),
                ArrowRoute::Pan(KeyAction::PanLeft),
                "{modifiers:?}+Left must still pan"
            );
            assert_eq!(
                route_arrow(Arrow::Right, modifiers),
                ArrowRoute::Pan(KeyAction::PanRight),
                "{modifiers:?}+Right must still pan"
            );
        }

        // The guard that keeps the Ctrl half above from passing for the wrong
        // reason. Written as a `const` block on clippy's own suggestion
        // (`assertions_on_constants`) -- which makes it strictly stronger than
        // the runtime form: if a future egui ever folded Ctrl into `command`
        // on macOS, this crate would fail to COMPILE rather than fail a test.
        const {
            assert!(
                !Modifiers::CTRL.command,
                "on this macOS-only app Ctrl must NOT set `command`"
            )
        };
    }

    #[test]
    fn alt_arrows_route_to_single_steps() {
        assert_eq!(
            route_arrow(Arrow::Left, Modifiers::ALT),
            ArrowRoute::Scrub(TimelineAction::StepBack)
        );
        assert_eq!(
            route_arrow(Arrow::Right, Modifiers::ALT),
            ArrowRoute::Scrub(TimelineAction::StepForward)
        );
    }

    #[test]
    fn command_arrows_route_to_the_jumps() {
        assert_eq!(
            route_arrow(Arrow::Left, Modifiers::COMMAND),
            ArrowRoute::Scrub(TimelineAction::JumpEarliest)
        );
        assert_eq!(
            route_arrow(Arrow::Right, Modifiers::COMMAND),
            ArrowRoute::Scrub(TimelineAction::JumpLatest)
        );
    }

    /// The precedence choice, pinned so it cannot be inverted by a later
    /// refactor without a failing test. A reader cannot recover "Command was
    /// tested first, deliberately" from an `if`/`else if` chain alone.
    #[test]
    fn command_wins_over_alt_when_both_are_held() {
        let both = Modifiers {
            command: true,
            alt: true,
            ..Modifiers::NONE
        };
        assert_eq!(
            route_arrow(Arrow::Left, both),
            ArrowRoute::Scrub(TimelineAction::JumpEarliest),
            "holding both resolves to the JUMP, not to an order-dependent accident"
        );
        assert_eq!(
            route_arrow(Arrow::Right, both),
            ArrowRoute::Scrub(TimelineAction::JumpLatest)
        );
    }
}
