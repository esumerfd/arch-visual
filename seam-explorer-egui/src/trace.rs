//! TRACE-01/TRACE-02: drag-to-trace state machine + first-time
//! discoverability overlay (D-14). Stub for M1 — the drag gesture, overlay
//! rendering, and highlight wiring land in a later plan (M3). This task only
//! declares the frozen result type `SeamExplorerApp::trace` holds.

/// Outcome of a single trace attempt (`seam_core::trace_path(from, to)`),
/// paired with the human-readable endpoints for the no-path message
/// (RESEARCH Pattern 5). `path: None` is the "no directed call path" case,
/// not an error — TRACE-02's zero-crossing/no-path messages are both
/// positive/neutral framing, never an error banner.
#[derive(Debug, Clone)]
pub struct TraceResult {
    pub from: String,
    pub to: String,
    pub path: Option<seam_core::TracePath>,
}
