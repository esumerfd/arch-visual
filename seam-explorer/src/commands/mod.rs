//! Command layer barrel: re-exports the three thin IPC adapters registered
//! in `generate_handler!` (`lib.rs`). Each command locks `Mutex<AppState>`,
//! delegates to `seam-core`, and maps errors — no analysis/aggregation logic
//! lives here (that stays in `seam-core`, per 01-RESEARCH.md Anti-Patterns).

pub mod graph;
pub mod seams;
pub mod trace;
