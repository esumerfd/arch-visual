//! `seam-explorer-egui` entry point — native, webview-free eframe app.
//! Replaces `tauri::Builder` (see `seam-explorer/src/lib.rs::run`); no
//! `invoke_handler` registration needed, since every command becomes a
//! direct method call inside `SeamExplorerApp::update()`. Takes an optional
//! `graph.json` path as its first CLI argument to preload at startup (plan
//! 05-14) -- parsed by `startup::graph_path_from_args`, applied by
//! `startup::preload_graph`.

use seam_explorer_egui::app::SeamExplorerApp;
use seam_explorer_egui::settings;
use seam_explorer_egui::startup;

fn main() -> eframe::Result<()> {
    // The only binding of the settings global to a real path in the whole
    // crate (plan 05-21). Placed before eframe::run_native and outside the
    // creation closure: the closure runs once, but settings are needed by
    // code paths that do not go through it, and there is no reason to bind
    // the global lazily.
    if let Some(path) = settings::default_config_path() {
        settings::init(path);
    }

    let graph_path = startup::graph_path_from_args(std::env::args());
    let native_options = eframe::NativeOptions::default();
    eframe::run_native(
        "Seam Explorer",
        native_options,
        Box::new(move |cc| {
            // new(cc) deserializes the whole app struct from eframe storage;
            // preloading before it would be silently discarded, and no test
            // in this suite can catch that (none of them can build a
            // CreationContext).
            let mut app = SeamExplorerApp::new(cc);
            if let Some(path) = graph_path.as_deref() {
                startup::preload_graph(&mut app, path);
            }
            Ok(Box::new(app))
        }),
    )
}
