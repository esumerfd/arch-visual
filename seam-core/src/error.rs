//! `SeamCoreError` — plain `thiserror`-derived enum, deliberately with NO
//! `Serialize` bound (per 01-RESEARCH.md Standard Stack table: only the
//! future app-shell crate's `CommandError` wraps this for IPC serialization).

#[derive(Debug, thiserror::Error)]
pub enum SeamCoreError {
    /// The input is not valid JSON at all — a syntax failure. Distinct from
    /// `MissingArray`, which is a *structural* failure on otherwise-valid
    /// JSON (01-05-PLAN.md Task 1).
    #[error("the file isn't valid JSON: {0}")]
    Parse(#[from] serde_json::Error),
    /// The JSON parsed fine but a required top-level array (`nodes` or
    /// `links`) is absent. Fatal — never falls back to an empty-graph `Ok`
    /// (GRAPH-02's structural validation guard).
    #[error("the file is missing a `{0}` array")]
    MissingArray(&'static str),
}
