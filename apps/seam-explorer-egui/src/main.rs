//! `seam-explorer-egui` entry point — native, webview-free eframe app.
//! Replaces `tauri::Builder` (see `seam-explorer-webview/src/lib.rs::run`); no
//! `invoke_handler` registration needed, since every command becomes a
//! direct method call inside `SeamExplorerApp::update()`. Takes an optional
//! `graph.json` path as its first CLI argument to preload at startup (plan
//! 05-14) -- parsed by `startup::graph_path_from_args`, applied by
//! `startup::preload_graph`.

use seam_explorer_egui::app::SeamExplorerApp;
use seam_explorer_egui::event_stream;
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

    // Bind the event socket BEFORE eframe::run_native, not inside the `cc`
    // closure -- a deliberate refinement of research/ARCHITECTURE.md's
    // suggested single `event_stream::init(path, ctx)` shape. Only the recv
    // thread needs `cc.egui_ctx`; binding does not. Binding first means a
    // bind failure can be handled (and, for a live-instance conflict, exit
    // the whole app) before any window has ever been created -- no window
    // flash, no half-initialised eframe to tear down.
    //
    // Task 1: any bind error is a non-fatal stderr line, and live ingestion
    // simply stays inactive (`event_stream::drain()` returns empty). Task 2
    // wires the real D-03 branch: `BindError::AlreadyRunning` exits the
    // whole app via `event_stream::exit_already_running`.
    let socket = match event_stream::bind_default() {
        Ok(socket) => Some(socket),
        Err(e) => {
            eprintln!("seam-explorer-egui: could not bind the live-event socket: {e}");
            None
        }
    };

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
            // `cc.egui_ctx` first exists here -- the only place in the
            // whole crate the recv thread's wake target is available.
            if let Some(socket) = socket {
                event_stream::serve(socket, cc.egui_ctx.clone());
            }
            Ok(Box::new(app))
        }),
    )
}
