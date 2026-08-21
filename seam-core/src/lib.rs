//! `seam-core`: standalone, app-shell-free domain crate for ingesting a
//! Graphify `graph.json` and computing crossing-count-ranked architectural
//! seams. See `01-01-PLAN.md` for the locked ingest/filter decisions
//! (D-01..D-10) this crate implements.

mod error;
mod ingest;
mod model;
mod seams;
mod trace;
mod verdict;

pub use error::SeamCoreError;
pub use ingest::{from_json, IngestResult, IngestWarning, STRUCTURAL_RELATIONS};
pub use model::{
    normalize_source_file, parse_source_line, resolve_community_names, CommunityId, Model, Node,
};
pub use seams::{detect, Seam};
pub use trace::{trace_path, TracePath};
pub use verdict::{compute_scc, has_cross_cycle, seam_detail, verdict, SccIndex, SeamDetail, Verdict};
