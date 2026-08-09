//! Tauri app-shell bootstrap: registers the dialog plugin (scoped to
//! `dialog:allow-open` only, see `capabilities/default.json`), manages the
//! single `Mutex<AppState>` slot, and wires up the IPC command handlers.
//! Contains no domain/analysis logic — that lives entirely in `seam-core`.

mod commands;
mod error;
mod state;

use state::AppState;
use std::sync::Mutex;

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(Mutex::new(AppState::default()))
        .invoke_handler(tauri::generate_handler![
            commands::graph::pick_and_load_graph,
            commands::graph::graph_render_data,
            commands::seams::list_seams,
            commands::seams::seam_detail,
            commands::trace::trace_path,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Seam Explorer");
}
