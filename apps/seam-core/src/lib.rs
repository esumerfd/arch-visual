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
    apply_add_edge, apply_add_node, apply_batch, apply_remove_edge, apply_remove_node,
    classify_target, local_roots, promote_unknown_communities, resolve_community,
    resolve_edge_source, resolve_edge_target, resolve_node_id, AddEdgeOutcome, ApplyOutcome,
    PendingEdges, PromotionOutcome, TargetClass, EXTERNAL_ROOTS, LIVE_BUFFER_CAPACITY,
    MAX_PROMOTION_PASSES, UNKNOWN_COMMUNITY,
};
pub use error::SeamCoreError;
pub use event::{parse_datagram, to_datagram, EventRejected, GraphEvent, MAX_EVENT_BYTES};
pub use ingest::{
    classify_test_code, from_json, is_test_path, is_test_symbol_name, IngestResult, IngestWarning,
    TestCodeExcluded, TestCodeRule, STRUCTURAL_RELATIONS, TEST_FILE_NAME_PREFIXES, TEST_FILE_STEMS,
    TEST_FILE_STEM_SUFFIXES, TEST_NAME_PREFIXES, TEST_PATH_DIRS,
};
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
