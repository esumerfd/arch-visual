//! `list_seams` / `seam_detail`: thin read-only IPC adapters over the
//! already-loaded `seam_core::Model` held in `Mutex<AppState>`. Both lock
//! state, delegate directly to `seam_core`, and map errors — no
//! aggregation/graph-walking logic lives here (Pitfall 8).

use crate::error::CommandError;
use crate::state::AppState;
use std::sync::Mutex;

#[tauri::command]
pub async fn list_seams(
    state: tauri::State<'_, Mutex<AppState>>,
) -> Result<Vec<seam_core::Seam>, CommandError> {
    // WR-01: map a poisoned lock to a renderable error instead of
    // panicking (a panic here would otherwise permanently break every
    // subsequent state-locking command until the app restarts).
    let guard = state
        .lock()
        .map_err(|e| CommandError::Internal(format!("state lock poisoned: {e}")))?;
    // WR-05: `NoGraphLoaded` (distinct from `Cancelled`, the file-dialog
    // sentinel) so callers can tell "no graph loaded yet" apart from "the
    // user dismissed the open-file dialog".
    let model = guard.model.as_ref().ok_or(CommandError::NoGraphLoaded)?;
    Ok(seam_core::detect(model))
}

#[tauri::command]
pub async fn seam_detail(
    a: String,
    b: String,
    state: tauri::State<'_, Mutex<AppState>>,
) -> Result<seam_core::SeamDetail, CommandError> {
    // WR-01: see list_seams above — same poisoned-lock handling.
    let guard = state
        .lock()
        .map_err(|e| CommandError::Internal(format!("state lock poisoned: {e}")))?;
    let model = guard.model.as_ref().ok_or(CommandError::NoGraphLoaded)?;
    let scc = model.scc.as_ref().ok_or(CommandError::NoGraphLoaded)?;
    Ok(seam_core::seam_detail(model, scc, &a, &b))
}
