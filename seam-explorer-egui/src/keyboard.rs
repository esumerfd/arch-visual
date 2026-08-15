//! NAV-05: single global keyboard dispatch, mirroring the D3 app's one
//! `keydown` listener (Phase 2 D-09) — arrows pan, `+`/`=` zoom in, `-` zoom
//! out, `0` resets, `t` toggles trace mode, with a search-input focus
//! carve-out (the egui equivalent of the original's
//! `document.activeElement === searchInput` guard).
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

/// The single per-frame input dispatch (NAV-05) -- called once from
/// `app.rs`'s already-wired call site. Checks the search-input focus
/// carve-out first (the egui equivalent of the original's `activeElement`
/// guard): if the search `TextEdit` identified by `search_id` currently
/// holds keyboard focus, every shortcut below is skipped for this frame, so
/// typing a component name containing `t` or `0` never touches the view or
/// trace mode.
pub fn handle(ctx: &egui::Context, app: &mut SeamExplorerApp, search_id: egui::Id) {
    if ctx.memory(|m| m.has_focus(search_id)) {
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
}
