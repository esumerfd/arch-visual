//! `CommandError`: wraps `seam-core`'s plain, non-`Serialize` errors plus
//! command-layer-only failure modes (dialog cancelled, file I/O) into a
//! single error type IPC commands can return. `seam-core::SeamCoreError`
//! deliberately has no `Serialize` bound (see `seam-core/src/error.rs`) —
//! only this wrapper implements `Serialize`, so the frontend always receives
//! a plain string it can render as a warning/error banner.

#[derive(Debug, thiserror::Error)]
pub enum CommandError {
    #[error("no file selected")]
    Cancelled,
    // WR-05: distinct from `Cancelled` ("no file selected", the user
    // dismissed the native dialog) so read commands that run before any
    // graph has been loaded surface an accurate message instead of being
    // silently swallowed by the frontend's `reason === 'no file selected'`
    // cancel-dialog check.
    #[error("no graph loaded yet")]
    NoGraphLoaded,
    #[error("failed to read file: {0}")]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Core(#[from] seam_core::SeamCoreError),
    // WR-07: converts a background (`spawn_blocking`) task panic into a
    // `CommandError` the frontend can render as a load-error banner,
    // instead of a second unhandled panic via `.expect(...)` on the
    // `JoinError`.
    #[error("internal error: {0}")]
    Internal(String),
}

impl serde::Serialize for CommandError {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}
