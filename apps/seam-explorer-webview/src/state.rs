//! `AppState`: single-slot, load-once, read-mostly state holding the
//! currently loaded `seam_core::Model`. Managed behind `Mutex<AppState>` by
//! the Tauri builder in `lib.rs`.

#[derive(Default)]
pub struct AppState {
    pub model: Option<seam_core::Model>,
}
