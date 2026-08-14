//! Load flow: `pick_file` (impure — the only filesystem/dialog touchpoint)
//! and `read_and_ingest` (pure — no egui, no filesystem). Ports the Tauri
//! command sequence from `seam-explorer/src/commands/graph.rs`
//! (`pick_and_load_graph`): `seam_core::from_json` -> warnings -> `Model` ->
//! `finalize_scc` -> `seam_core::detect`. No async runtime: `pick_file` is
//! called synchronously from `update()` (RESEARCH Pattern 1 — eframe already
//! runs on the main thread on native targets).

use crate::app::{Banner, BannerKind};

/// Mirrors the Tauri app's `CommandError` variant taxonomy (dialog
/// cancelled / io / core) minus the IPC-only variants — no `NoGraphLoaded`
/// or `Internal` here, since this type only covers the load path itself.
#[derive(Debug)]
pub enum LoadError {
    Cancelled,
    Io(std::io::Error),
    Core(seam_core::SeamCoreError),
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LoadError::Cancelled => write!(f, "no file selected"),
            LoadError::Io(e) => write!(f, "failed to read file: {e}"),
            LoadError::Core(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for LoadError {}

impl From<std::io::Error> for LoadError {
    fn from(e: std::io::Error) -> Self {
        LoadError::Io(e)
    }
}

impl From<seam_core::SeamCoreError> for LoadError {
    fn from(e: seam_core::SeamCoreError) -> Self {
        LoadError::Core(e)
    }
}

/// Result of a successful ingest: the finalized model, its ranked seams, and
/// an optional non-fatal warning banner (GRAPH-02).
#[derive(Debug)]
pub struct LoadOutcome {
    pub model: seam_core::Model,
    pub seams: Vec<seam_core::Seam>,
    pub banner: Option<Banner>,
}

/// Native "Open File" dialog — the only impure part of the load flow. Called
/// synchronously; briefly blocking the UI thread while the OS dialog is open
/// is expected/accepted native-app UX (RESEARCH Pattern 1).
pub fn pick_file() -> Option<std::path::PathBuf> {
    rfd::FileDialog::new()
        .add_filter("Graph JSON", &["json"])
        .pick_file()
}

/// Pure ingest: parse `json`, finalize the whole-graph SCC index, and rank
/// seams by crossing count. No filesystem, no egui — this is what
/// `tests/tracer_smoke.rs` and the `--lib` unit tests drive directly, with
/// no live `egui::Context` required.
pub fn read_and_ingest(json: &str) -> Result<LoadOutcome, LoadError> {
    let ingest = seam_core::from_json(json)?;

    let banner = if ingest.warnings.is_empty() {
        None
    } else {
        let n = ingest.warnings.len();
        let plural = if n == 1 { "" } else { "s" };
        Some(Banner {
            kind: BannerKind::Warning,
            heading: "Some edges were dropped".to_string(),
            body: format!(
                "{n} edge{plural} referenced a component id that isn't in this graph, so they were skipped. Everything else loaded normally — seam counts below reflect only the valid edges."
            ),
        })
    };

    let mut model = ingest.model;
    // CPU-bound Tarjan SCC — run inline for now (RESEARCH Pattern 1: measure
    // before optimizing; sample/graph.json at ~1.5MB is fast enough).
    model.finalize_scc();
    let seams = seam_core::detect(&model);

    Ok(LoadOutcome {
        model,
        seams,
        banner,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const CLEAN_FIXTURE: &str = include_str!("../../seam-core/tests/fixtures/clean.json");

    #[test]
    fn read_and_ingest_populates_model_and_seams() {
        let outcome = read_and_ingest(CLEAN_FIXTURE).expect("clean fixture must ingest");
        assert!(outcome.model.graph.node_count() > 0);
        assert!(!outcome.seams.is_empty());
    }

    #[test]
    fn read_and_ingest_returns_core_error_not_panic_on_bad_json() {
        let result = read_and_ingest("not json at all");
        assert!(matches!(result, Err(LoadError::Core(_))));
    }
}
