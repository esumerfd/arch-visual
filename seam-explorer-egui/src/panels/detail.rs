//! Right `SidePanel`: SEAM-02 seam detail + TRACE-02 trace result. Compiling
//! stub this task — renders the empty state only; real bridge/verdict/
//! trace rendering is Plan 02+ (detail) and Plan 03+ (trace).

use crate::app::SeamExplorerApp;

pub fn show(ui: &mut egui::Ui, app: &SeamExplorerApp) {
    if app.detail.is_some() || app.trace.is_some() {
        // Real rendering lands in a later plan; placeholder keeps the panel
        // non-empty so the frozen wiring is visibly exercised.
        ui.label("Detail rendering not yet implemented.");
        return;
    }

    ui.label(
        "Select a seam to see its bridge components and verdict — or turn on Trace mode and drag between two components to trace the call path between them.",
    );
}
