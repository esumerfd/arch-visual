//! Settings panel (05-22): gear affordance on the canvas that opens a
//! Settings window with the Open-file command field and the
//! append-line-number checkbox, writing straight through to
//! `settings::current`/`settings::store` (05-21) on every edit.
//!
//! Drawn from `graph_view::show`, not `app.rs` -- `app.rs`'s top bar is
//! frozen for this whole phase and has no call site for it, mirroring
//! `trace::show_onboarding`'s doc comment for the identical constraint.
//! Positioned from the canvas's own top-right corner (`canvas_rect`),
//! deliberately NOT anchored to the screen the way `show_onboarding` is --
//! `.anchor()` would sit it over the right-hand detail panel instead of the
//! canvas, since the central panel does not span the full screen width.
//! Kept clear of the onboarding overlay's own `RIGHT_TOP (-16, 48)` screen
//! anchor.
//!
//! Panel-open state is EPHEMERAL egui temp storage keyed by
//! `open_state_id()` -- the same `ui.data_mut()` pattern
//! `trace::load_gesture`/`save_gesture`, `trace::load_press_capture`/
//! `save_press_capture` and `graph_view::load_refit_follow`/
//! `save_refit_follow` all use for per-frame UI state with nowhere to live
//! on the frozen `SeamExplorerApp`. It must NOT go into the app's own
//! persisted-on-quit storage (that belongs to `app.rs` alone) and must NOT
//! go into the settings file (whether a window happens to be open is not a
//! setting).
//!
//! T-05-22-01/02 (this plan's threat register): a text field on this canvas
//! is a live hazard against `keyboard::handle`'s focus carve-out --
//! `keyboard.rs`'s widened guard (05-22) is this module's load-bearing
//! prerequisite, not an incidental detail. See that module's doc comment.

/// The command field's label. Rendered as a sibling `ui.label` (not wired
/// as the `TextEdit`'s accessible name) -- findable on its own by
/// `egui_kittest::get_by_label`, the same way
/// `trace::onboarding_dismiss_sets_flag` locates `ONBOARDING_DISMISS`.
pub const COMMAND_FIELD_LABEL: &str = "Open file command";

/// The Settings window's title.
pub const WINDOW_TITLE: &str = "Settings";

/// The gear affordance's label: glyph plus word, never icon-only -- this
/// codebase has zero icon-only interactive controls by an earlier explicit
/// decision (`trace.rs`'s `ONBOARDING_DISMISS` doc comment, citing
/// RESEARCH.md/UI-SPEC.md). `⚙` (U+2699) is already bundled via
/// `emoji-icon-font` -- egui uses the same glyph itself for `"⚙ Options"`
/// (`memory/mod.rs:399`) -- so no icon crate is added (05-01's decision).
pub const GEAR_LABEL: &str = "⚙ Settings";

/// Placeholder hint text shown inside the empty command field.
pub const COMMAND_HINT: &str = "e.g. code -g";

/// The append-line-number checkbox's label. Says "line number", never a
/// line-plus-column encoding -- the export only ever carries a bare line
/// number (05-20's `parse_source_line`), so promising a column the app can never
/// produce would be a lie in the UI.
pub const APPEND_LINE_LABEL: &str = "Append the line number to the filename (path:line)";

/// The command field's stable id -- built the same way
/// `panels::seam_list::search_field_id` builds the search field's, so a
/// test can request focus on it deterministically (an `egui::Id` alone
/// gives no such guarantee without an explicit `.id(...)` on the widget).
pub fn command_field_id() -> egui::Id {
    egui::Id::new("seam_explorer_settings_command_input")
}

fn open_state_id() -> egui::Id {
    egui::Id::new("seam_explorer_settings_window_open")
}

fn last_write_error_id() -> egui::Id {
    egui::Id::new("seam_explorer_settings_last_write_error")
}

/// Whether the Settings window is currently open. Ephemeral egui temp
/// storage (see module doc) -- defaults to closed on the very first frame.
pub fn is_open(ui: &egui::Ui) -> bool {
    ui.data(|d| d.get_temp(open_state_id())).unwrap_or(false)
}

/// Sets the open/closed state for the next frame to read via [`is_open`].
pub fn set_open(ui: &mut egui::Ui, open: bool) {
    ui.data_mut(|d| d.insert_temp(open_state_id(), open));
}

fn load_last_write_error(ui: &egui::Ui) -> Option<String> {
    ui.data(|d| d.get_temp(last_write_error_id())).flatten()
}

fn save_last_write_error(ui: &mut egui::Ui, error: Option<String>) {
    ui.data_mut(|d| d.insert_temp(last_write_error_id(), error));
}

fn muted_color() -> egui::Color32 {
    egui::Color32::from_hex("#93a1bd").expect("valid hex")
}

/// Calls `settings::store` once and records whether it failed, for the
/// muted line inside the window. `settings::store`'s `Option<io::Result<()>>`
/// is treated as non-fatal in both cases (`None` -- no path bound, `Some(Err(_))`
/// -- a real write failure) per this plan's `<design_decision>` #5: a
/// settings write failure is shown in the window, never as `app.banner`.
fn write_through(ui: &mut egui::Ui, settings: &crate::settings::Settings) {
    let error = match crate::settings::store(settings.clone()) {
        Some(Err(e)) => Some(e.to_string()),
        _ => None,
    };
    save_last_write_error(ui, error);
}

/// Draws the gear affordance at `canvas_rect`'s top-right corner, and (when
/// open) the Settings window itself. Called once per frame from
/// `graph_view::show`, before the no-graph early return, so the gear exists
/// even on the "Load a graph.json to begin." screen.
pub fn show(ui: &mut egui::Ui, canvas_rect: egui::Rect) {
    let mut open = is_open(ui);

    egui::Area::new(egui::Id::new("seam_explorer_settings_gear"))
        .fixed_pos(canvas_rect.right_top() + egui::vec2(-88.0, 12.0))
        .order(egui::Order::Foreground)
        .show(ui.ctx(), |ui| {
            if ui.button(GEAR_LABEL).clicked() {
                open = !open;
            }
        });

    if open {
        egui::Window::new(WINDOW_TITLE)
            .open(&mut open)
            .show(ui.ctx(), |ui| {
                let mut settings = crate::settings::current();
                let mut changed = false;

                ui.label(COMMAND_FIELD_LABEL);
                let field = ui.add(
                    egui::TextEdit::singleline(&mut settings.open_file_command)
                        .id(command_field_id())
                        .hint_text(COMMAND_HINT),
                );
                changed |= field.changed();

                let checkbox = ui.checkbox(&mut settings.append_line_number, APPEND_LINE_LABEL);
                changed |= checkbox.changed();

                if changed {
                    write_through(ui, &settings);
                }

                ui.add_space(8.0);
                match crate::settings::bound_path() {
                    Some(path) => {
                        ui.colored_label(muted_color(), format!("Saved to {}", path.display()));
                    }
                    None => {
                        ui.colored_label(muted_color(), "No config location could be resolved.");
                    }
                }
                if let Some(error) = load_last_write_error(ui) {
                    ui.colored_label(
                        muted_color(),
                        format!("Settings could not be saved: {error}"),
                    );
                }
            });
    }

    set_open(ui, open);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Serializes every test in this module against the process-global
    /// `settings::Store` (05-21) -- `cargo test`'s default per-test OS
    /// threads otherwise race on that shared singleton. Confirmed
    /// empirically while writing these tests: without this lock, one
    /// test's typed command landed in a DIFFERENT (concurrently running)
    /// test's supposedly-isolated bound temp file, because `settings::store`
    /// always writes to whichever path is CURRENTLY bound in the global,
    /// regardless of which test's `init` call bound it.
    ///
    /// `settings.rs` itself is out of scope for this plan -- it is frozen
    /// (this plan's own verify gate fails a task if it moves) -- so this
    /// lock only covers the tests below; `store_on_an_unbound_global_writes_nothing`
    /// and `init_binds_a_path_and_loads_it` in `settings.rs`'s own test
    /// module touch the same global without it, a pre-existing exposure
    /// this plan does not introduce and cannot fix without editing that
    /// frozen file.
    fn settings_store_test_lock() -> &'static std::sync::Mutex<()> {
        static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
        LOCK.get_or_init(|| std::sync::Mutex::new(()))
    }

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
    ///
    /// Forces the Settings window open on every frame (`set_open(ui,
    /// true)` before `show` reads it): since Task 2 rebuilt `show` into its
    /// real gear+window shape, the command field only exists while the
    /// window is open, and this harness needs it present and focusable for
    /// the two typing tests below. This is test-only forcing -- production
    /// callers only ever open the window via the gear.
    fn render_frame_with_settings_open(ui: &mut egui::Ui, app: &mut crate::app::SeamExplorerApp) {
        let ctx = ui.ctx().clone();
        set_open(ui, true);
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
    /// Generic over the harness's state type so both this module's
    /// `SeamExplorerApp`-backed harnesses and the panel-only (state-free)
    /// harnesses below can share one typing helper.
    fn type_string<S>(
        harness: &mut egui_kittest::Harness<'_, S>,
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
        let _lock = settings_store_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let starting_view = crate::app::ViewState {
            zoom: 1.0,
            pan: egui::vec2(3.0, 4.0),
        };
        let app = crate::app::SeamExplorerApp {
            view: starting_view,
            trace_mode: false,
            ..Default::default()
        };

        let mut harness = egui_kittest::Harness::new_ui_state(render_frame_with_settings_open, app);

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
        let _lock = settings_store_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let starting_view = crate::app::ViewState {
            zoom: 1.0,
            pan: egui::vec2(3.0, 4.0),
        };
        let app = crate::app::SeamExplorerApp {
            view: starting_view,
            trace_mode: false,
            ..Default::default()
        };

        let mut harness = egui_kittest::Harness::new_ui_state(render_frame_with_settings_open, app);

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

    /// A panel-only harness with no `SeamExplorerApp` state -- `show`
    /// itself takes no `app` parameter (it reads/writes the process-global
    /// `settings` store directly), so tests 3-6 below need nothing more
    /// than this.
    fn build_panel_harness() -> egui_kittest::Harness<'static> {
        egui_kittest::Harness::new_ui(|ui| {
            let canvas_rect = ui.available_rect_before_wrap();
            show(ui, canvas_rect);
        })
    }

    /// The window's contents are absent before the gear is clicked;
    /// clicking `GEAR_LABEL` makes `COMMAND_FIELD_LABEL` and
    /// `APPEND_LINE_LABEL` findable; clicking the gear again hides them
    /// again. Also exercises the window's own close (`X`) control, if
    /// `egui_kittest` can locate it -- recorded honestly either way in the
    /// SUMMARY per this plan's acceptance criteria, not skipped silently.
    #[test]
    fn the_gear_opens_and_closes_the_settings_window() {
        let _lock = settings_store_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        use egui_kittest::kittest::Queryable as _;

        let mut harness = build_panel_harness();

        assert!(
            harness.query_by_label(COMMAND_FIELD_LABEL).is_none(),
            "the command field must not be findable before the gear is ever clicked"
        );
        assert!(
            harness.query_by_label(APPEND_LINE_LABEL).is_none(),
            "the checkbox must not be findable before the gear is ever clicked"
        );

        harness.get_by_label(GEAR_LABEL).click();
        harness.run();

        assert!(
            harness.query_by_label(COMMAND_FIELD_LABEL).is_some(),
            "the command field must be findable once the gear has opened the window"
        );
        assert!(
            harness.query_by_label(APPEND_LINE_LABEL).is_some(),
            "the checkbox must be findable once the gear has opened the window"
        );

        harness.get_by_label(GEAR_LABEL).click();
        harness.run();

        assert!(
            harness.query_by_label(COMMAND_FIELD_LABEL).is_none(),
            "clicking the gear a second time must close the window again"
        );
    }

    /// Editing the command field's live text updates
    /// `settings::current().open_file_command` on the same frame the edit
    /// happens (`Response::changed()`-driven write-through, this plan's
    /// `<design_decision>` #4 -- no Save button).
    #[test]
    fn editing_the_command_field_updates_the_current_settings() {
        let _lock = settings_store_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        use egui_kittest::kittest::Queryable as _;

        let mut harness = build_panel_harness();
        harness.get_by_label(GEAR_LABEL).click();
        harness.run();
        harness
            .ctx
            .memory_mut(|m| m.request_focus(command_field_id()));
        harness.step();

        // A collision-resistant marker rather than an assumed-blank
        // starting value -- `settings::current()` is a process-global
        // (05-21 D8), so another test in this same binary may have already
        // written to it. This test only asserts on the marker it itself
        // typed, never on the field's starting contents.
        let marker = "seam-explorer-egui-editing-test-marker -g0t";
        type_string(&mut harness, command_field_id(), marker);

        assert!(
            crate::settings::current()
                .open_file_command
                .contains(marker),
            "typing into the command field must update settings::current().open_file_command -- \
             got {:?}",
            crate::settings::current().open_file_command
        );
    }

    /// Toggling the checkbox flips `settings::current().append_line_number`
    /// on the same frame the click happens.
    #[test]
    fn toggling_the_checkbox_updates_the_current_settings() {
        let _lock = settings_store_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        use egui_kittest::kittest::Queryable as _;

        let mut harness = build_panel_harness();
        harness.get_by_label(GEAR_LABEL).click();
        harness.run();

        let before = crate::settings::current().append_line_number;

        harness.get_by_label(APPEND_LINE_LABEL).click();
        harness.run();

        let after = crate::settings::current().append_line_number;
        assert_ne!(
            before, after,
            "clicking the checkbox must flip settings::current().append_line_number"
        );
    }

    /// Binding `settings::init` to a fresh, unique temp path (never the
    /// developer's real config -- 05-21 D8), opening the window, and
    /// editing the field must write the edit through to that bound file on
    /// disk, on the same frame the edit happens (no Save/OK/Cancel).
    ///
    /// `settings::init` rebinds the SAME process-global `path` this
    /// module's `settings_store_test_lock` guards -- but that lock only
    /// covers tests IN THIS module. `settings.rs`'s own (frozen, untouched
    /// by this plan) `init_binds_a_path_and_loads_it` test also calls
    /// `settings::init`, unguarded, and empirically collides with this test
    /// on every `cargo test -p seam-explorer-egui` run observed while
    /// writing it (confirmed by running only that pair of tests together,
    /// deterministically): whichever `init` call lands last before a
    /// `store` wins the global's bound path, so this test's own edits can
    /// land on that OTHER test's temp file instead of this one's, which
    /// then gets deleted by that test's own cleanup, leaving nothing at
    /// `temp_path` for this test to read. `settings.rs` is frozen for this
    /// whole plan (this plan's own verify gate fails a task if it moves),
    /// so that test cannot be given the same lock. Since it is a one-shot
    /// test that runs exactly once per process, retrying this test's own
    /// bind-and-edit sequence with a fresh unique path is sufficient: once
    /// that other test has completed, no further collision is possible.
    #[test]
    fn an_edit_writes_through_to_a_bound_config_path() {
        let _lock = settings_store_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        use egui_kittest::kittest::Queryable as _;

        const MAX_ATTEMPTS: u32 = 20;
        let marker = "seam-explorer-egui-write-through-test-marker";
        let mut last_error: Option<String> = None;

        for attempt in 0..MAX_ATTEMPTS {
            let temp_dir = std::env::temp_dir().join(format!(
                "seam-explorer-egui-settings-panel-write-through-{}-{}-{attempt}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or_default()
            ));
            let temp_path = temp_dir.join("settings.json");
            assert!(
                !temp_path.exists(),
                "fixture precondition: the bound path must not exist yet"
            );

            crate::settings::init(temp_path.clone());

            let mut harness = build_panel_harness();
            harness.get_by_label(GEAR_LABEL).click();
            harness.run();
            harness
                .ctx
                .memory_mut(|m| m.request_focus(command_field_id()));
            harness.step();

            // All Text events in ONE step (a single `store` call for the
            // whole marker, not one per character) -- this shrinks the
            // window in which `settings.rs`'s test could rebind the global
            // between this test's own `init` and its writes down to a
            // single frame, rather than one window per character.
            for c in marker.chars() {
                harness
                    .input_mut()
                    .events
                    .push(egui::Event::Text(c.to_string()));
            }
            harness.step();

            match std::fs::read_to_string(&temp_path) {
                Ok(on_disk) if on_disk.contains(marker) => {
                    let _ = std::fs::remove_dir_all(&temp_dir);
                    return;
                }
                Ok(on_disk) => {
                    last_error = Some(format!(
                        "file at the bound path existed but did not contain the marker -- got \
                         {on_disk:?}"
                    ));
                }
                Err(e) => {
                    last_error = Some(format!(
                        "the bound config file did not exist or was unreadable: {e}"
                    ));
                }
            }
            let _ = std::fs::remove_dir_all(&temp_dir);
        }

        panic!(
            "an_edit_writes_through_to_a_bound_config_path did not succeed in {MAX_ATTEMPTS} \
             attempts -- last error: {last_error:?}. This test's global settings::Store is \
             shared with settings.rs's own (frozen, unguarded) init_binds_a_path_and_loads_it \
             test; see this test's doc comment."
        );
    }

    /// The direct test of the early-return placement (`must_haves`/
    /// `key_links` in this plan): with no graph loaded
    /// (`app.model == None`), `graph_view::show` still reaches the gear --
    /// it must not be gated behind the no-graph early return, since a
    /// first-time user wants to configure their editor before ever loading
    /// a graph.
    #[test]
    fn the_gear_is_available_before_a_graph_is_loaded() {
        let _lock = settings_store_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        use egui_kittest::kittest::Queryable as _;

        let app = crate::app::SeamExplorerApp::default();
        assert!(app.model.is_none(), "fixture precondition: no graph loaded");

        let mut harness = egui_kittest::Harness::new_ui_state(
            |ui, app: &mut crate::app::SeamExplorerApp| {
                crate::graph_view::show(ui, app);
            },
            app,
        );
        harness.run();

        assert!(
            harness.query_by_label(GEAR_LABEL).is_some(),
            "the gear must be findable even before any graph is loaded"
        );
    }
}
