//! `seam-explorer-egui` entry point — native, webview-free eframe app.
//! Replaces `tauri::Builder` (see `seam-explorer/src/lib.rs::run`); no
//! `invoke_handler` registration needed, since every command becomes a
//! direct method call inside `SeamExplorerApp::update()`.

use seam_explorer_egui::app::SeamExplorerApp;

fn main() -> eframe::Result<()> {
    let native_options = eframe::NativeOptions::default();
    eframe::run_native(
        "Seam Explorer",
        native_options,
        Box::new(|cc| Ok(Box::new(SeamExplorerApp::new(cc)))),
    )
}
