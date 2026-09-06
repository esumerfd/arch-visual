//! `seam-core`: standalone, app-shell-free domain crate for ingesting a
//! Graphify `graph.json` and computing crossing-count-ranked architectural
//! seams. See `01-01-PLAN.md` for the locked ingest/filter decisions
//! (D-01..D-10) this crate implements.

mod apply;
mod error;
mod event;
mod ingest;
mod model;
mod seams;
mod socket;
mod trace;
mod verdict;

pub use apply::{
    apply_add_node, apply_batch, apply_remove_node, resolve_community, resolve_node_id,
    ApplyOutcome, LIVE_BUFFER_CAPACITY, UNKNOWN_COMMUNITY,
};
pub use error::SeamCoreError;
pub use event::{parse_datagram, to_datagram, EventRejected, GraphEvent, MAX_EVENT_BYTES};
pub use ingest::{from_json, IngestResult, IngestWarning, STRUCTURAL_RELATIONS};
pub use model::{
    normalize_source_file, parse_source_line, resolve_community_names, CommunityId, Model, Node,
};
pub use seams::{detect, Seam};
pub use socket::{
    default_socket_path, socket_path_from, CONFIG_DIR_NAME, MAX_SUN_PATH_BYTES, SOCKET_FILE_NAME,
};
pub use trace::{trace_path, TracePath};
pub use verdict::{
    compute_scc, has_cross_cycle, seam_detail, verdict, SccIndex, SeamDetail, Verdict,
};
