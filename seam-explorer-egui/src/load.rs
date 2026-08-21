//! Load flow: `pick_file` (impure — the only filesystem/dialog touchpoint)
//! and `read_and_ingest` (pure — no egui, no filesystem). Ports the Tauri
//! command sequence from `seam-explorer/src/commands/graph.rs`
//! (`pick_and_load_graph`): `seam_core::from_json` -> warnings -> `Model` ->
//! `finalize_scc` -> `seam_core::detect`. No async runtime: `pick_file` is
//! called synchronously from `update()` (RESEARCH Pattern 1 — eframe already
//! runs on the main thread on native targets).
//!
//! `pick_file` is also the point where the graph's own directory is
//! captured (`remember_graph_path`/`graph_dir`, plan 05-21) for later
//! Graphify `source_file` resolution -- the capture lives here, alongside
//! `startup::preload_graph`'s matching capture, rather than on the frozen
//! `SeamExplorerApp` struct.

use crate::app::{Banner, BannerKind};
use std::path::{Path, PathBuf};
use std::sync::{OnceLock, RwLock};

static GRAPH_DIR: OnceLock<RwLock<Option<PathBuf>>> = OnceLock::new();

fn graph_dir_lock() -> &'static RwLock<Option<PathBuf>> {
    GRAPH_DIR.get_or_init(|| RwLock::new(None))
}

/// Records the directory containing `path` as the most recently loaded
/// graph's location, so `open_file::resolve_source_path` has something to
/// resolve a Graphify-exported repo-relative `source_file` against. Called
/// from both places a graph path enters this program: `pick_file` (below)
/// and `startup::preload_graph`.
pub fn remember_graph_path(path: &Path) {
    let dir = path.parent().map(Path::to_path_buf);
    let mut guard = graph_dir_lock().write().unwrap_or_else(|e| e.into_inner());
    *guard = dir;
}

/// The directory of the most recently loaded graph, or `None` if nothing
/// has been loaded yet in this process.
pub fn graph_dir() -> Option<PathBuf> {
    let guard = graph_dir_lock().read().unwrap_or_else(|e| e.into_inner());
    guard.clone()
}

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
    let path = rfd::FileDialog::new()
        .add_filter("Graph JSON", &["json"])
        .pick_file()?;
    remember_graph_path(&path);
    Some(path)
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

/// Maps a load-path failure (bad file read, unparseable/structurally
/// invalid `graph.json`) into the same error-kind `Banner` shape
/// `SeamExplorerApp::load_graph` renders. Pure — no egui, no filesystem —
/// so the error-banner mapping is unit-testable without a live
/// `egui::Context` (GRAPH-02).
pub fn error_banner(e: &LoadError) -> Banner {
    Banner {
        kind: BannerKind::Error,
        heading: "Couldn't load this file".to_string(),
        body: format!(
            "This doesn't look like a Graphify graph.json export — {e}. Choose a different file and try again."
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remembering_a_graph_path_exposes_its_directory() {
        remember_graph_path(Path::new("/some/repo/graphify-out/graph.json"));
        assert_eq!(graph_dir(), Some(PathBuf::from("/some/repo/graphify-out")));
    }

    const CLEAN_FIXTURE: &str = include_str!("../../seam-core/tests/fixtures/clean.json");
    // Single dangling edge (a1 -> b1_ghost) — exercises the n==1 singular
    // wording branch of the dropped-edge banner.
    const DROPPED_EDGE_SINGULAR_FIXTURE: &str =
        include_str!("../../seam-core/tests/fixtures/malformed.json");
    // Two dangling edges — exercises the n>1 plural wording branch. No
    // existing fixture has more than one dangling edge, so this one is
    // authored fresh under tests/fixtures/ (Task 3 action note).
    const DROPPED_EDGE_PLURAL_FIXTURE: &str =
        include_str!("../tests/fixtures/dropped_edges_plural.json");

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

    /// GRAPH-01: `read_and_ingest` on a valid fixture yields a populated
    /// model and a non-empty seam list ranked by crossing count descending.
    #[test]
    fn test_load_updates_model_and_seams() {
        let outcome = read_and_ingest(CLEAN_FIXTURE).expect("clean fixture must ingest");
        assert!(
            outcome.model.graph.node_count() > 0,
            "model must have nodes"
        );
        assert!(!outcome.seams.is_empty(), "must produce at least one seam");

        let crossings: Vec<usize> = outcome.seams.iter().map(|s| s.crossings).collect();
        let mut sorted_desc = crossings.clone();
        sorted_desc.sort_by(|a, b| b.cmp(a));
        assert_eq!(
            crossings, sorted_desc,
            "seams must be ranked by crossing count descending (SEAM-01)"
        );
    }

    /// GRAPH-02: dropped-edge warnings render as a singular/plural-correct
    /// `Banner`; a clean fixture produces no banner; a structurally invalid
    /// (unparseable / missing-array) load error maps to an error banner via
    /// `error_banner`.
    #[test]
    fn test_ingest_warnings_render() {
        // Clean fixture: no warnings, no banner.
        let clean = read_and_ingest(CLEAN_FIXTURE).expect("clean fixture must ingest");
        assert!(
            clean.banner.is_none(),
            "a clean fixture must not produce a banner"
        );

        // n == 1: singular wording, no trailing "s" on "edge".
        let singular = read_and_ingest(DROPPED_EDGE_SINGULAR_FIXTURE)
            .expect("dropped-edge fixture must still ingest (edge dropped, not fatal)");
        match singular.banner {
            Some(Banner {
                kind: BannerKind::Warning,
                ref body,
                ..
            }) => {
                assert!(
                    body.contains("1 edge referenced"),
                    "singular wording expected, got: {body}"
                );
                assert!(
                    !body.contains("1 edges"),
                    "must not pluralize a single dropped edge, got: {body}"
                );
            }
            other => panic!("expected Some(Banner{{kind: Warning, ..}}), got {other:?}"),
        }

        // n > 1: plural wording ("edges").
        let plural = read_and_ingest(DROPPED_EDGE_PLURAL_FIXTURE)
            .expect("dropped-edge fixture must still ingest (edges dropped, not fatal)");
        match plural.banner {
            Some(Banner {
                kind: BannerKind::Warning,
                ref body,
                ..
            }) => {
                assert!(
                    body.contains("2 edges referenced"),
                    "plural wording expected, got: {body}"
                );
            }
            other => panic!("expected Some(Banner{{kind: Warning, ..}}), got {other:?}"),
        }

        // Structurally invalid (unparseable JSON): read_and_ingest errors;
        // error_banner maps that error to an error-kind Banner.
        let bad_json_err =
            read_and_ingest("{ not json").expect_err("unparseable JSON must be an error");
        match error_banner(&bad_json_err) {
            Banner {
                kind: BannerKind::Error,
                ..
            } => {}
            other => panic!("expected Banner{{kind: Error, ..}}, got {other:?}"),
        }

        // Structurally invalid (missing `nodes` array): also errors, also
        // maps to an error-kind Banner.
        let missing_array_err = read_and_ingest(r#"{"links": []}"#)
            .expect_err("a document missing `nodes` must be an error");
        match error_banner(&missing_array_err) {
            Banner {
                kind: BannerKind::Error,
                ..
            } => {}
            other => panic!("expected Banner{{kind: Error, ..}}, got {other:?}"),
        }
    }
}
