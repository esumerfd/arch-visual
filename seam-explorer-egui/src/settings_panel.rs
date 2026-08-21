//! Settings panel (05-22): gear affordance on the canvas that opens a
//! Settings window with the Open-file command field and the
//! append-line-number checkbox, writing straight through to
//! `settings::current`/`settings::store` (05-21) on every edit.
//!
//! Task 1 (RED) scaffolding note: at this point in the plan's TDD cycle,
//! `show` below renders ONLY the always-visible command field -- no gear,
//! no window, no checkbox, no persistence. This is deliberately the
//! smallest possible surface that makes
//! `typing_in_the_settings_field_must_not_drive_the_canvas` (below) a real
//! measurement of today's defect (T-05-22-01) rather than a compile error:
//! `keyboard::handle`'s guard only carves out the search field, so a
//! synthesised keystroke landing in ANY other focused text field -- even
//! this bare-bones stub -- still reaches the global shortcut dispatch and
//! mutates `app.view`/`app.trace_mode`. Task 2 rebuilds this module into
//! its real gear+window shape once `keyboard.rs`'s guard has widened to
//! cover it.

/// The command field's label. Rendered as a sibling `ui.label` (not wired
/// as the `TextEdit`'s accessible name) -- findable on its own by
/// `egui_kittest::get_by_label`, the same way
/// `trace::onboarding_dismiss_sets_flag` locates `ONBOARDING_DISMISS`.
pub const COMMAND_FIELD_LABEL: &str = "Open file command";

/// The command field's stable id -- built the same way
/// `panels::seam_list::search_field_id` builds the search field's, so a
/// test can request focus on it deterministically (an `egui::Id` alone
/// gives no such guarantee without an explicit `.id(...)` on the widget).
pub fn command_field_id() -> egui::Id {
    egui::Id::new("seam_explorer_settings_command_input")
}

/// Task 1 (RED) stub -- see module doc. Renders the label and an
/// always-visible single-line field bound to
/// `settings::current().open_file_command`. `canvas_rect` is unused by this
/// stub (kept in the signature now so Task 2's real implementation, which
/// needs it to place the gear, is a body-only change).
pub fn show(ui: &mut egui::Ui, canvas_rect: egui::Rect) {
    let _ = canvas_rect;
    let mut command = crate::settings::current().open_file_command;
    ui.label(COMMAND_FIELD_LABEL);
    ui.add(egui::TextEdit::singleline(&mut command).id(command_field_id()));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The id `app.rs` passes to `keyboard::handle` as `search_id` --
    /// duplicated here (not imported) because this module's tests build
    /// their own harness closure rather than driving the real, frozen
    /// `app.rs::ui()` (which needs a live `eframe::Frame` this harness
    /// doesn't construct).
    fn search_field_id() -> egui::Id {
        egui::Id::new("seam_explorer_search_input")
    }

    /// Reproduces `app.rs::ui()`'s dispatch order for exactly the two
    /// fields this plan cares about: the settings field (via this module's
    /// `show`), a stand-in search field carrying the identical id `app.rs`
    /// passes to `keyboard::handle`, then `keyboard::handle` LAST -- the
    /// same order `app.rs::ui()` calls `graph_view::show(...)` (which now
    /// calls this module) followed by `keyboard::handle(&ctx, self,
    /// search_id)`.
    fn render_frame(ui: &mut egui::Ui, app: &mut crate::app::SeamExplorerApp) {
        let ctx = ui.ctx().clone();
        let canvas_rect = ui.available_rect_before_wrap();
        show(ui, canvas_rect);
        ui.add(egui::TextEdit::singleline(&mut app.search_query).id(search_field_id()));
        crate::keyboard::handle(&ctx, app, search_field_id());
    }

    /// Types `text` into whichever field currently holds keyboard focus,
    /// character by character, synthesising BOTH the `Event::Key` and the
    /// matching `Event::Text` a real keystroke produces
    /// (`egui::Key::from_name` parses single characters as well as variant
    /// names -- `"-"` -> `Key::Minus`, `"0"` -> `Key::Num0`, `"t"` ->
    /// `Key::T`, the exact three hazard keys this plan cares about).
    /// `keyboard::handle`'s dispatch reads `i.key_pressed(...)`, which
    /// (per egui 0.35's `InputState::key_pressed`/`num_presses`) filters
    /// the whole frame's `i.events` directly -- `TextEdit` reads the same
    /// events via `ui.input(|i| i.filtered_events(..))`, a read, not a
    /// drain -- so both consumers see every key this synthesises,
    /// regardless of who has focus. That is the actual mechanism behind
    /// the hazard this test measures, not a shortcut around it.
    ///
    /// Returns the highest text-cursor char index observed under `id` at
    /// any point during typing -- NOT just the index after the final step.
    /// This plan's Task 1 stub rebuilds its bound `String` fresh from
    /// `settings::current()` every frame (no per-frame persistence yet), so
    /// a persisted `TextEditState` cursor computed against an
    /// again-empty galley clamps back toward `0` on the very next frame --
    /// checking only the post-loop value would misreport "no character
    /// ever reached the field" even when every character visibly did, one
    /// frame at a time.
    fn type_string(
        harness: &mut egui_kittest::Harness<'_, crate::app::SeamExplorerApp>,
        id: egui::Id,
        text: &str,
    ) -> usize {
        let mut max_reached = 0usize;
        for c in text.chars() {
            let key = egui::Key::from_name(&c.to_string());
            if let Some(key) = key {
                harness.input_mut().events.push(egui::Event::Key {
                    key,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::default(),
                });
            }
            harness
                .input_mut()
                .events
                .push(egui::Event::Text(c.to_string()));
            harness.step();

            let reached = egui::text_edit::TextEditState::load(&harness.ctx, id)
                .and_then(|state| state.cursor.char_range())
                .map(|range| range.primary.index.0)
                .unwrap_or(0);
            max_reached = max_reached.max(reached);

            if let Some(key) = key {
                harness.input_mut().events.push(egui::Event::Key {
                    key,
                    physical_key: None,
                    pressed: false,
                    repeat: false,
                    modifiers: egui::Modifiers::default(),
                });
                harness.step();
            }
        }
        max_reached
    }

    /// T-05-22-01, RED half. Focuses the settings field, types a command
    /// containing `-`, `0` and `t` (the three live global shortcuts), and
    /// asserts the canvas never moved. **THIS FAILS TODAY** -- see this
    /// module's doc comment: nothing yet carves the settings field out of
    /// `keyboard::handle`'s dispatch.
    #[test]
    fn typing_in_the_settings_field_must_not_drive_the_canvas() {
        let starting_view = crate::app::ViewState {
            zoom: 1.0,
            pan: egui::vec2(3.0, 4.0),
        };
        let app = crate::app::SeamExplorerApp {
            view: starting_view,
            trace_mode: false,
            ..Default::default()
        };

        let mut harness = egui_kittest::Harness::new_ui_state(render_frame, app);

        harness
            .ctx
            .memory_mut(|m| m.request_focus(command_field_id()));
        harness.step();

        assert!(
            harness.ctx.memory(|m| m.has_focus(command_field_id())),
            "fixture precondition failed: the settings field never actually took focus, so \
             this test would prove nothing"
        );

        let reached = type_string(&mut harness, command_field_id(), "code -g0t");
        assert!(
            reached > 0,
            "fixture precondition failed: no character actually reached the settings field's \
             own cursor state (max index observed stayed 0) -- the keystrokes went nowhere, \
             which would make the assertions below pass for the wrong reason"
        );

        let after_view = harness.state().view;
        let after_trace_mode = harness.state().trace_mode;

        // Several hazard keys can fire at once from one typed string (a
        // reset via `0` moves BOTH zoom and pan back to defaults), so this
        // names every mutation actually observed rather than guessing a
        // single cause.
        let mut culprits: Vec<&str> = Vec::new();
        if after_view.zoom != starting_view.zoom {
            culprits.push("Minus (zoom changed)");
        }
        if after_view.pan != starting_view.pan {
            culprits.push("0 (Num0 -> reset_view) and/or an arrow key (pan changed)");
        }
        if after_trace_mode {
            culprits.push("T (trace_mode toggled)");
        }
        let culprit = if culprits.is_empty() {
            "none -- canvas untouched".to_string()
        } else {
            culprits.join(", ")
        };

        assert_eq!(
            after_view.zoom, starting_view.zoom,
            "typing '-' into the settings field must not zoom the canvas -- before zoom={:?} \
             pan={:?} trace_mode=false, after zoom={:?} pan={:?} trace_mode={after_trace_mode}, \
             likely culprit key: {culprit}",
            starting_view.zoom, starting_view.pan, after_view.zoom, after_view.pan
        );
        assert_eq!(
            after_view.pan, starting_view.pan,
            "typing into the settings field must not pan the canvas -- before zoom={:?} \
             pan={:?} trace_mode=false, after zoom={:?} pan={:?} trace_mode={after_trace_mode}, \
             likely culprit key: {culprit}",
            starting_view.zoom, starting_view.pan, after_view.zoom, after_view.pan
        );
        assert!(
            !after_trace_mode,
            "typing 't' into the settings field must not toggle trace_mode -- before zoom={:?} \
             pan={:?} trace_mode=false, after zoom={:?} pan={:?} trace_mode={after_trace_mode}, \
             likely culprit key: {culprit}",
            starting_view.zoom, starting_view.pan, after_view.zoom, after_view.pan
        );
    }

    /// The regression lock: the identical sequence, but focusing the
    /// SEARCH field's id instead. **PASSES today** --
    /// `keyboard::handle`'s existing `has_focus(search_id)` guard already
    /// covers it, and Task 2 must not have replaced that guard with a
    /// different one.
    #[test]
    fn typing_in_the_search_field_still_does_not_drive_the_canvas() {
        let starting_view = crate::app::ViewState {
            zoom: 1.0,
            pan: egui::vec2(3.0, 4.0),
        };
        let app = crate::app::SeamExplorerApp {
            view: starting_view,
            trace_mode: false,
            ..Default::default()
        };

        let mut harness = egui_kittest::Harness::new_ui_state(render_frame, app);

        harness
            .ctx
            .memory_mut(|m| m.request_focus(search_field_id()));
        harness.step();

        assert!(
            harness.ctx.memory(|m| m.has_focus(search_field_id())),
            "fixture precondition failed: the search field never actually took focus, so this \
             test would prove nothing"
        );

        type_string(&mut harness, search_field_id(), "code -g0t");

        let after_view = harness.state().view;
        let after_trace_mode = harness.state().trace_mode;

        assert_eq!(
            after_view.zoom, starting_view.zoom,
            "the existing search-field carve-out must still hold -- zoom before={:?} after={:?}",
            starting_view.zoom, after_view.zoom
        );
        assert_eq!(
            after_view.pan, starting_view.pan,
            "the existing search-field carve-out must still hold -- pan before={:?} after={:?}",
            starting_view.pan, after_view.pan
        );
        assert!(
            !after_trace_mode,
            "the existing search-field carve-out must still hold -- trace_mode must stay false"
        );
    }
}
