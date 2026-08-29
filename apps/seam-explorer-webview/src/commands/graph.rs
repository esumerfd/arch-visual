//! `pick_and_load_graph`: the one command that touches the filesystem.
//! Native "Open File" dialog runs server-side (webview never gets ambient
//! FS access — capability is scoped to `dialog:allow-open` only), reads the
//! picked path, ingests it via `seam_core::from_json`, dispatches the
//! CPU-bound Tarjan SCC finalize off the async runtime via
//! `tokio::task::spawn_blocking` (Pitfall 3), and stores the finalized
//! `Model` in `Mutex<AppState>`.
//!
//! No relation/confidence filtering happens here — that already ran once,
//! inside `seam_core::ingest` (Pitfall 8: business logic must not leak into
//! the command layer).

use crate::error::CommandError;
use crate::state::AppState;
use std::sync::Mutex;
use tauri_plugin_dialog::DialogExt;

/// Serializable mirror of `seam_core::IngestWarning` — the domain type has
/// no `Serialize` bound (kept `tauri`-free), so the command layer maps it to
/// this IPC-shaped DTO field-for-field, no additional logic.
#[derive(Debug, Clone, serde::Serialize)]
pub struct IngestWarningDto {
    pub reason: String,
    pub source: String,
    pub target: String,
}

impl From<&seam_core::IngestWarning> for IngestWarningDto {
    fn from(w: &seam_core::IngestWarning) -> Self {
        Self {
            reason: w.reason.clone(),
            source: w.source.clone(),
            target: w.target.clone(),
        }
    }
}

/// Returned to the frontend after a successful load: counts for the
/// just-loaded graph plus any dropped-edge warnings (GRAPH-02).
#[derive(Debug, Clone, serde::Serialize)]
pub struct GraphSummary {
    pub node_count: usize,
    pub edge_count: usize,
    pub seam_count: usize,
    pub warnings: Vec<IngestWarningDto>,
}

#[tauri::command]
pub async fn pick_and_load_graph(
    app: tauri::AppHandle,
    state: tauri::State<'_, Mutex<AppState>>,
) -> Result<GraphSummary, CommandError> {
    // WR-03: `blocking_pick_file()` parks its calling thread until the user
    // responds to the native dialog. Dispatch it via `spawn_blocking` (same
    // pattern as `finalize_scc` below) so it doesn't tie up a Tokio worker
    // thread — and thus other concurrent `invoke()` calls — for the
    // arbitrary duration a user-driven modal dialog stays open.
    let app_for_dialog = app.clone();
    let picked = tauri::async_runtime::spawn_blocking(move || {
        app_for_dialog
            .dialog()
            .file()
            .add_filter("Graph JSON", &["json"])
            .blocking_pick_file()
    })
    .await
    // WR-07: convert a background-task panic into a CommandError the
    // frontend can render, instead of a second unhandled panic here.
    .map_err(|e| CommandError::Internal(format!("file picker background task panicked: {e}")))?
    .ok_or(CommandError::Cancelled)?;
    let path = picked
        .into_path()
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e.to_string()))?;

    // WR-04: dispatch the (potentially large) graph.json read via
    // spawn_blocking, consistent with how the CPU-bound finalize_scc pass
    // below is already kept off the async runtime's thread.
    let json = tokio::task::spawn_blocking(move || std::fs::read_to_string(path))
        .await
        // WR-07: same background-task-panic-to-CommandError conversion as
        // the file picker above and finalize_scc below.
        .map_err(|e| CommandError::Internal(format!("file read background task panicked: {e}")))??;
    let ingest = seam_core::from_json(&json)?;
    let warnings: Vec<IngestWarningDto> = ingest.warnings.iter().map(IngestWarningDto::from).collect();

    // Pitfall 3: the Tarjan SCC pass is CPU-bound — never call finalize_scc
    // inline on the async runtime.
    let mut model = ingest.model;
    let model = tokio::task::spawn_blocking(move || {
        model.finalize_scc();
        model
    })
    .await
    // WR-07: a panic inside finalize_scc (e.g. an edge case in a
    // malformed-but-parseable graph.json) now becomes a CommandError the
    // frontend renders as a load-error banner, instead of a second
    // unhandled panic via `.expect(...)` on the JoinError.
    .map_err(|e| CommandError::Internal(format!("finalize_scc background task panicked: {e}")))?;

    let node_count = model.graph.node_count();
    let edge_count = model.graph.edge_count();
    let seam_count = seam_core::detect(&model).len();

    // WR-06: don't propagate mutex poisoning — a panic anywhere else while
    // holding this lock should not permanently brick every subsequent
    // graph-loading/rendering command via a chained `.unwrap()` panic.
    let mut guard = state.lock().unwrap_or_else(|e| e.into_inner());
    guard.model = Some(model);

    Ok(GraphSummary {
        node_count,
        edge_count,
        seam_count,
        warnings,
    })
}

/// Scoped render-only mirror of `seam_core::Node` — id/label/community
/// verbatim, nothing else (never `source_file`/`metadata`/`confidence`; see
/// 02-RESEARCH.md Pitfall 1). Never derive `Serialize` on `seam_core::Node`
/// itself and return it directly.
#[derive(Debug, Clone, serde::Serialize)]
pub struct RenderNode {
    pub id: String,
    pub label: String,
    pub community: String,
}

/// Scoped render-only directed edge: source -> target order preserves the
/// `StableDiGraph`'s direction verbatim (D-05) — no recomputation needed by
/// the frontend to draw arrowheads.
#[derive(Debug, Clone, serde::Serialize)]
pub struct RenderEdge {
    pub source: String,
    pub target: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct GraphRenderData {
    pub nodes: Vec<RenderNode>,
    pub edges: Vec<RenderEdge>,
}

/// Pure mapping from the already-loaded `Model` to the scoped render DTO —
/// no `tauri::State`, unit-testable without the Tauri runtime. The
/// `graph_render_data` command (Task 2) is a thin wrapper around this.
pub fn render_data_from_model(model: &seam_core::Model) -> GraphRenderData {
    let nodes = model
        .graph
        .node_indices()
        .map(|idx| {
            let n = &model.graph[idx];
            RenderNode {
                id: n.id.clone(),
                label: n.label.clone(),
                community: n.community.clone(),
            }
        })
        .collect();

    let edges = model
        .graph
        .edge_indices()
        .map(|e| {
            let (s, t) = model
                .graph
                .edge_endpoints(e)
                .expect("edge index from edge_indices() must have valid endpoints");
            RenderEdge {
                source: model.graph[s].id.clone(),
                target: model.graph[t].id.clone(),
            }
        })
        .collect();

    GraphRenderData { nodes, edges }
}

/// Read-only IPC adapter over `render_data_from_model` — thin wrapper, all
/// mapping logic stays in the pure helper above. Uses the dedicated
/// `CommandError::NoGraphLoaded` variant (WR-05) that `list_seams`/
/// `seam_detail` also use for "no graph loaded yet" (see
/// `commands/seams.rs`) — kept distinct from `CommandError::Cancelled`
/// (the file-dialog-was-dismissed sentinel) so the frontend doesn't
/// mistake a genuine "not ready" failure for a cancelled dialog.
#[tauri::command]
pub async fn graph_render_data(
    state: tauri::State<'_, Mutex<AppState>>,
) -> Result<GraphRenderData, CommandError> {
    // WR-06: tolerate mutex poisoning (see pick_and_load_graph above) rather
    // than propagating it into a second, unrelated panic here.
    let guard = state.lock().unwrap_or_else(|e| e.into_inner());
    let model = guard.model.as_ref().ok_or(CommandError::NoGraphLoaded)?;
    Ok(render_data_from_model(model))
}

#[cfg(test)]
mod tests {
    //! RED: render_data_from_model and its DTOs (RenderNode/RenderEdge/
    //! GraphRenderData) do not exist yet — this module intentionally fails
    //! to compile until Task 1's GREEN step adds them. Fixture is an inline
    //! 3-node/2-edge graphify node_link JSON literal (avoids include_str!
    //! path fragility from this file's src/commands/ location).

    const FIXTURE: &str = r#"{
        "nodes": [
            {"id": "A", "label": "A", "community": "1"},
            {"id": "B", "label": "B", "community": "1"},
            {"id": "C", "label": "C", "community": "2"}
        ],
        "links": [
            {"source": "A", "target": "B", "relation": "calls", "confidence": "EXTRACTED"},
            {"source": "B", "target": "C", "relation": "calls", "confidence": "EXTRACTED"}
        ]
    }"#;

    #[test]
    fn render_data_from_model_preserves_node_and_edge_counts() {
        let ingest = seam_core::from_json(FIXTURE).expect("fixture must parse");
        let model = ingest.model;

        let data = super::render_data_from_model(&model);

        assert_eq!(data.nodes.len(), model.graph.node_count());
        assert_eq!(data.edges.len(), model.graph.edge_count());
    }

    #[test]
    fn render_data_from_model_carries_id_label_community_verbatim() {
        let ingest = seam_core::from_json(FIXTURE).expect("fixture must parse");
        let model = ingest.model;

        let data = super::render_data_from_model(&model);

        let node_a = data
            .nodes
            .iter()
            .find(|n| n.id == "A")
            .expect("node A must be present in render data");
        assert_eq!(node_a.label, "A");
        assert_eq!(node_a.community, "1");
    }

    #[test]
    fn render_data_from_model_preserves_directed_edge_order() {
        let ingest = seam_core::from_json(FIXTURE).expect("fixture must parse");
        let model = ingest.model;

        let data = super::render_data_from_model(&model);

        let has_a_to_b = data
            .edges
            .iter()
            .any(|e| e.source == "A" && e.target == "B");
        assert!(has_a_to_b, "directed edge A->B must be preserved (D-05)");
    }
}
