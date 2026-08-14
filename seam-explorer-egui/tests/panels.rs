//! Shared `egui_kittest` harness entry point (Wave 0 validation
//! infrastructure). Plan 02 and Plan 05 extend this file with real panel
//! snapshot cases (SEAM-01/02/03, TRACE-02 onboarding); this task only
//! proves the harness itself boots and can query a rendered widget before
//! any of that real coverage is trusted.
//!
//! Pin note (RESEARCH.md Pitfall 6): `egui_kittest` is pinned to the same
//! `0.35.x` line as this crate's `egui`/`eframe` — a `0.36` kittest would
//! not type-match this crate's `egui::Context`.

use egui_kittest::kittest::Queryable;
use egui_kittest::Harness;

/// Builds a stateless [`Harness`] over a closure taking `&mut egui::Ui`.
/// Kept public within the test crate (not `pub(crate)`, integration test
/// binaries are separate crates) so later plans add panel-rendering cases
/// without restructuring this helper.
pub fn ui_harness<'a>(app: impl FnMut(&mut egui::Ui) + 'a) -> Harness<'a> {
    Harness::new_ui(app)
}

/// Wave 0 gate: an `egui_kittest::Harness` constructs, renders one frame of
/// a trivial `ui.label(...)`, and the accessibility-tree query finds that
/// label. Proves the harness works before Plan 02 writes real panel
/// snapshots against it.
#[test]
fn harness_boots() {
    let mut harness = ui_harness(|ui| {
        ui.label("Load a graph.json to begin.");
    });
    harness.run();

    harness.get_by_label("Load a graph.json to begin.");
}
