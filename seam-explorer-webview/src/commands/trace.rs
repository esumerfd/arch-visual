//! `trace_path`: thin read-only IPC adapter over the already-loaded
//! `seam_core::Model` held in `Mutex<AppState>`. Locks state, delegates
//! directly to `seam_core::trace_path`, and maps errors — no path-finding
//! logic lives here (that stays in `seam-core`, per 01-RESEARCH.md
//! Anti-Patterns / Pitfall 8).

use crate::error::CommandError;
use crate::state::AppState;
use std::sync::Mutex;

#[tauri::command]
pub async fn trace_path(
    from: String,
    to: String,
    state: tauri::State<'_, Mutex<AppState>>,
) -> Result<Option<seam_core::TracePath>, CommandError> {
    let guard = state
        .lock()
        .map_err(|e| CommandError::Internal(format!("state lock poisoned: {e}")))?;
    let model = guard.model.as_ref().ok_or(CommandError::NoGraphLoaded)?;
    Ok(seam_core::trace_path(model, &from, &to))
}
