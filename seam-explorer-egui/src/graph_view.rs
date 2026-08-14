//! `CentralPanel` graph canvas. Task 2 only needs the pre-load placeholder
//! (must_haves: "Before any graph is loaded the central canvas area shows
//! the placeholder copy, not a blank void."); the real `egui_graphs`
//! `GraphView` + custom `DisplayNode`/`DisplayEdge` wiring is Plan 02+ (M2).

use crate::app::SeamExplorerApp;

pub fn show(ui: &mut egui::Ui, app: &SeamExplorerApp) {
    if app.model.is_none() {
        ui.centered_and_justified(|ui| {
            ui.label("Load a graph.json to begin.");
        });
        return;
    }

    // Real egui_graphs rendering lands in a later plan (M2).
    ui.centered_and_justified(|ui| {
        ui.label("Graph loaded — canvas rendering not yet implemented.");
    });
}
