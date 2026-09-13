//! Project-directory entry point for "Open Project" (quick task
//! 260913-gud): pick or receive a project directory, and if it already has
//! a Graphify-produced graphify-out/graph.json, hand the path to
//! startup::preload_graph -- the one ingest authority -- rather than
//! re-reading or re-parsing it here. When no graph exists yet, arm a
//! prompt (rendered by poll_and_prompt) offering to build one.
//!
//! This module's state lives in a process-global (OnceLock<Mutex<...>>),
//! following load.rs's and event_stream.rs's idiom, rather than as a
//! field on the frozen SeamExplorerApp -- see app.rs's module doc for
//! why that struct is frozen.

use crate::app::SeamExplorerApp;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

/// The subdirectory Graphify writes its export into, relative to a project
/// directory.
const GRAPHIFY_OUT_DIR: &str = "graphify-out";
/// The graph file Graphify writes inside GRAPHIFY_OUT_DIR.
const GRAPH_FILE_NAME: &str = "graph.json";

/// Joins the Graphify output layout onto dir. Pure.
pub fn graph_path_for(dir: &Path) -> PathBuf {
    dir.join(GRAPHIFY_OUT_DIR).join(GRAPH_FILE_NAME)
}

/// This module's process-global state. Idle and Prompting cover this
/// task; a later Building variant joins the background build run.
/// Matched exhaustively everywhere -- no catch-all arm -- so the compiler
/// finds every site a new variant needs to touch.
enum ProjectState {
    Idle,
    Prompting(PathBuf),
}

static PROJECT_STATE: OnceLock<Mutex<ProjectState>> = OnceLock::new();

fn project_state_lock() -> &'static Mutex<ProjectState> {
    PROJECT_STATE.get_or_init(|| Mutex::new(ProjectState::Idle))
}

/// The directory a "no graph found here" prompt is currently armed for, or
/// None if nothing is pending.
pub fn pending_prompt_dir() -> Option<PathBuf> {
    let guard = project_state_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    match &*guard {
        ProjectState::Idle => None,
        ProjectState::Prompting(dir) => Some(dir.clone()),
    }
}

/// Resets this module's state to Idle. Used by tests to isolate cases
/// sharing the process-global.
pub fn clear_pending() {
    let mut guard = project_state_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    *guard = ProjectState::Idle;
}

/// If dir already has a Graphify graph, loads it through the one ingest
/// authority (startup::preload_graph) and clears any pending prompt.
/// Otherwise arms the "no graph here" prompt for dir and leaves app
/// untouched. Contains no read, no parse, and no ingest of its own -- the
/// error banner, the dropped-edge warning banner, and the
/// remember_graph_path capture all come from the one existing load path.
pub fn open_project_dir(app: &mut SeamExplorerApp, dir: &Path) {
    let path = graph_path_for(dir);
    if path.is_file() {
        crate::startup::preload_graph(app, &path);
        let mut guard = project_state_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        *guard = ProjectState::Idle;
    } else {
        let mut guard = project_state_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        *guard = ProjectState::Prompting(dir.to_path_buf());
    }
}

/// Native "choose a project directory" dialog. Cancel is silent, matching
/// load::pick_file's cancel semantics. The only impure entry point in
/// this module, and the only function here the tests cannot drive.
pub fn open_project(app: &mut SeamExplorerApp) {
    let Some(dir) = rfd::FileDialog::new().pick_folder() else {
        return;
    };
    open_project_dir(app, &dir);
}
