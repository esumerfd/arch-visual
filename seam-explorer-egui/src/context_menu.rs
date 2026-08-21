//! The right-click-a-node "Open file" context menu: the pure decision half
//! only (05-23). `graph_view::handle_context_menu` is the live half --
//! hit-test, popup render, spawn, banner -- and lives next to
//! `handle_trace_gesture` because both need the same private
//! `hit_test_node` and the same `Response` (see this plan's
//! `<must_haves.key_links>`).
//!
//! [`plan_open_with`]/[`plan_open`] are the single decision authority for
//! what a right-click on a node should do: spawn the configured editor, or
//! tell the user honestly why it can't (no source file recorded, or no
//! editor command configured). `graph_view::handle_context_menu` must call
//! one of these two functions rather than re-deriving the decision --
//! T-05-23-01/T-05-23-06 in this plan's `<threat_model>` are mitigated by
//! that single call site, pinned by a verify gate that fails the task if
//! `handle_context_menu` calls `open_file::build_command` or
//! `open_file::resolve_source_path` directly.
//!
//! The right-clicked node's id lives in egui temp memory
//! ([`load_target`]/[`save_target`]), not on `SeamExplorerApp` --
//! `app.rs` is frozen this whole phase, and this is the same pattern
//! `trace::load_press_capture`/`trace::save_press_capture` and
//! `graph_view::load_refit_follow` (via its own snapshot id) already use.

use std::path::Path;

use crate::settings::Settings;

/// The menu's one actionable item's label -- singular, per the user's own
/// words: "shows menu with option Open file".
pub const OPEN_FILE_LABEL: &str = "Open file";
/// Muted hint shown instead of an enabled item when the right-clicked node
/// has no recorded `source_file` (29% of real nodes in `sample/graph.json`
/// -- not an edge case, see this plan's `<must_haves.truths>`).
pub const NO_SOURCE_HINT: &str = "No source file recorded for this component.";
/// Muted hint shown instead of an enabled item when no editor command is
/// configured yet -- names the gear from `settings_panel::GEAR_LABEL` so the
/// user can act on it directly rather than guessing where to go.
pub const NO_COMMAND_HINT: &str =
    "Set the Open file command in Settings (⚙ Settings) first.";

/// The three-state outcome of right-clicking a node, decided once by
/// [`plan_open_with`]/[`plan_open`] and rendered by
/// `graph_view::handle_context_menu`. `Spawn`'s `Vec<String>` is a complete
/// argument vector (program plus args plus the resolved path, in that
/// order) -- exactly what `open_file::spawn` expects, never a command
/// string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MenuAction {
    /// Activating the menu item should spawn this exact argv.
    Spawn(Vec<String>),
    /// The node has no recorded `source_file` -- checked FIRST (see
    /// [`plan_open_with`]'s doc comment for why), regardless of whether a
    /// command is configured.
    DisabledNoSource,
    /// The node has a source file, but no editor command is configured (or
    /// `open_file::build_command` unexpectedly declined a non-blank one --
    /// never a panic, never an empty `Spawn`).
    DisabledNoCommand,
}

/// egui memory key for the right-clicked node id, named consistently with
/// `trace::press_capture_id`, in the same per-widget temp storage.
fn target_id() -> egui::Id {
    egui::Id::new("seam_explorer_context_menu_target")
}

/// Loads the currently remembered right-clicked node id, if any. Flattens
/// the stored `Option` (defaulting to `None` when nothing has ever been
/// written) so the caller doesn't have to distinguish "never written" from
/// "explicitly cleared" -- both read as no target, mirroring
/// `trace::load_press_capture`.
pub fn load_target(ui: &egui::Ui) -> Option<String> {
    ui.data(|d| d.get_temp(target_id())).flatten()
}

/// Records or clears the remembered target. Writing `None` is the clear --
/// a single setter for both record and clear, mirroring
/// `trace::save_press_capture`.
pub fn save_target(ui: &mut egui::Ui, target: Option<String>) {
    ui.data_mut(|d| d.insert_temp(target_id(), target));
}

/// The pure decision core, with the filesystem-existence check injected
/// (`exists`) so every branch -- including the 29%-of-real-nodes disabled
/// case -- is unit-testable without touching disk. [`plan_open`] is the
/// thin real-filesystem wrapper, mirroring `open_file::resolve_source_path`
/// over `open_file::resolve_source_path_with`.
///
/// Decision order (itself a decision worth pinning, see this plan's
/// `<behavior>`): no source file -> [`MenuAction::DisabledNoSource`],
/// checked FIRST, because a node with no file is disabled regardless of
/// whether a command is configured, and the more specific message is the
/// more useful one. Otherwise a blank (or whitespace-only) command ->
/// [`MenuAction::DisabledNoCommand`]. Otherwise the path is resolved
/// through `open_file::resolve_source_path_with` and the argv is built
/// through `open_file::build_command` -- this function never rebuilds
/// either of those (T-05-23-01).
pub fn plan_open_with(
    _source_file: Option<&str>,
    _line: Option<u32>,
    _graph_dir: Option<&Path>,
    _settings: &Settings,
    _exists: &dyn Fn(&Path) -> bool,
) -> MenuAction {
    // Task 1 (RED) stub: unconditionally DisabledNoSource. Replaced with the
    // real decision in Task 2 (GREEN) -- see `<tdd_discipline>` in
    // `05-23-PLAN.md`. This makes
    // `plan_open_disables_when_the_node_has_no_source_file` (test 1) and
    // `target_round_trips_through_egui_memory` (test 6) pass by accident
    // (expected, per the plan's own Task 1 acceptance criteria) while tests
    // 2-5 fail on values.
    MenuAction::DisabledNoSource
}

/// The real-filesystem wrapper over [`plan_open_with`], mirroring
/// `open_file::resolve_source_path` over `resolve_source_path_with`.
pub fn plan_open(
    source_file: Option<&str>,
    line: Option<u32>,
    graph_dir: Option<&Path>,
    settings: &Settings,
) -> MenuAction {
    plan_open_with(source_file, line, graph_dir, settings, &|p| p.exists())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings_with(command: &str, append_line_number: bool) -> Settings {
        Settings {
            open_file_command: command.to_string(),
            append_line_number,
        }
    }

    #[test]
    fn plan_open_disables_when_the_node_has_no_source_file() {
        let configured = settings_with("code -g", false);
        assert_eq!(
            plan_open_with(None, None, None, &configured, &|_| true),
            MenuAction::DisabledNoSource,
            "a node with no source file must be disabled even when a command IS configured"
        );

        let unconfigured = settings_with("", false);
        assert_eq!(
            plan_open_with(None, None, None, &unconfigured, &|_| true),
            MenuAction::DisabledNoSource,
            "no source AND no command must report DisabledNoSource -- the more specific \
             message wins, not DisabledNoCommand"
        );
    }

    #[test]
    fn plan_open_disables_when_no_command_is_configured() {
        for blank in ["", "   "] {
            let settings = settings_with(blank, false);
            assert_eq!(
                plan_open_with(Some("src/thing.rs"), None, None, &settings, &|_| true),
                MenuAction::DisabledNoCommand,
                "a blank command template {blank:?} must disable with DisabledNoCommand \
                 when a real source file IS present"
            );
        }
    }

    #[test]
    fn plan_open_builds_the_argv_with_the_resolved_path_last() {
        let graph_dir = Path::new("/r/graphify-out");
        let root_match = Path::new("/r/src/thing.rs");
        let exists = |p: &Path| p == root_match;
        let settings = settings_with("code -g", false);

        let action = plan_open_with(
            Some("src/thing.rs"),
            None,
            Some(graph_dir),
            &settings,
            &exists,
        );
        assert_eq!(
            action,
            MenuAction::Spawn(vec![
                "code".to_string(),
                "-g".to_string(),
                "/r/src/thing.rs".to_string(),
            ]),
            "the argv must be the split template plus the RESOLVED path as the final element"
        );
    }

    #[test]
    fn plan_open_appends_the_line_only_when_the_checkbox_is_on_and_a_line_is_known() {
        let never_exists = |_: &Path| false;
        let cases: [(bool, Option<u32>, &str); 4] = [
            (false, None, "a.rs"),
            (false, Some(42), "a.rs"),
            (true, None, "a.rs"),
            (true, Some(42), "a.rs:42"),
        ];
        for (append_line_number, line, expected_last) in cases {
            let settings = settings_with("code", append_line_number);
            let action = plan_open_with(Some("a.rs"), line, None, &settings, &never_exists);
            let MenuAction::Spawn(argv) = action else {
                panic!(
                    "append_line_number={append_line_number}, line={line:?}: expected Spawn, \
                     got {action:?}"
                );
            };
            assert_eq!(
                argv.last().map(String::as_str),
                Some(expected_last),
                "append_line_number={append_line_number}, line={line:?}, got argv={argv:?}"
            );
        }
    }

    #[test]
    fn plan_open_falls_back_to_the_raw_relative_path_when_nothing_resolves() {
        let never_exists = |_: &Path| false;
        let settings = settings_with("code", false);
        let action = plan_open_with(
            Some("src/thing.rs"),
            None,
            Some(Path::new("/r/graphify-out")),
            &settings,
            &never_exists,
        );
        assert_eq!(
            action,
            MenuAction::Spawn(vec![
                "code".to_string(),
                "src/thing.rs".to_string(),
            ]),
            "when nothing on disk matches, the raw relative path must pass through unchanged \
             -- best effort beats silence"
        );
    }

    #[test]
    fn target_round_trips_through_egui_memory() {
        let ctx = egui::Context::default();
        let raw_input = egui::RawInput::default();
        let _ = ctx.run_ui(raw_input, |ui| {
            assert_eq!(
                load_target(ui),
                None,
                "no target has ever been written yet"
            );

            save_target(ui, Some("a1".to_string()));
            assert_eq!(load_target(ui), Some("a1".to_string()));

            save_target(ui, None);
            assert_eq!(
                load_target(ui),
                None,
                "writing None must clear a previously recorded target"
            );
        });
    }
}
