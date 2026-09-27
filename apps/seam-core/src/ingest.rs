//! `from_json`: parse a Graphify `graph.json` (NetworkX `node_link_data`
//! shape), apply the D-01/D-02/D-03/D-04 relation+confidence filter exactly
//! once (Pitfall 2, 01-RESEARCH.md), and collect endpoint-missing edges as
//! `IngestWarning`s rather than silently dropping them (GRAPH-02, Pattern 1).
//! Also applies the quick-260926-xbl hard test-code exclusion filter (see
//! below `STRUCTURAL_RELATIONS`) before any node reaches the `Model`.
//!
//! Port target: `apps/seam-explorer-web/seam-explorer.html` `normalize()`
//! (lines 270-292) for the overall shape; RESEARCH.md Patterns 1 and 2 for
//! the warning-collection and tolerant-parsing behavior the JS version does
//! NOT have.

use crate::error::SeamCoreError;
use crate::model::{CommunityId, Model, Node};
use petgraph::stable_graph::StableDiGraph;
use serde::Deserialize;
use std::collections::{HashMap, HashSet};

/// D-01/D-02/D-03: only these five relation types count as coupling signal.
/// Single edit point if the allow-list ever changes (RESEARCH.md
/// Anti-Patterns note).
pub const STRUCTURAL_RELATIONS: [&str; 5] = [
    "calls",
    "references",
    "method",
    "implements",
    "imports_from",
];

// ---------------------------------------------------------------------
// Quick task 260926-xbl: the second single-edit-point filter in this file.
//
// A node is discarded before it ever enters the `Model` when EITHER its
// (normalized) `source_file` looks like a test-code path (D-01, "path"), OR
// its raw `label`/`norm_label` starts with a locked test-symbol-name prefix
// (D-01, "name"). Both classes are required: path alone would MISS the
// motivating case (`MockAgentRuntime` lives in a normal production source
// file, `orchestrator/src/handlers/ai_agent.rs` -- see 260926-xbl-PLAN.md
// finding 1). This is a hard, non-optional filter (D-03): no flag, no
// parameter, no UI toggle turns it off, and it runs for every caller of
// `from_json`.
// ---------------------------------------------------------------------

/// Directory path components (every segment except the final filename)
/// that, matched case-insensitively and EXACTLY (never as a substring —
/// `contest`/`latest`/`spectrum` must never match `test`/`spec`), mark
/// every file beneath them as test code.
pub const TEST_PATH_DIRS: [&str; 7] = [
    "test",
    "tests",
    "spec",
    "specs",
    "__tests__",
    "__mocks__",
    "testdata",
];

/// A base filename's STEM (the part before the last `.`, or the whole base
/// name when there is no `.`) that, matched case-insensitively and exactly,
/// marks the file as test code regardless of its directory.
pub const TEST_FILE_STEMS: [&str; 5] = ["test", "tests", "spec", "specs", "conftest"];

/// A base-filename stem ending in one of these, case-insensitively, marks
/// the file as test code — covers `helper_test.go`, `helper.test.ts`,
/// `helper.spec.ts`, etc.
pub const TEST_FILE_STEM_SUFFIXES: [&str; 4] = ["_test", "_spec", ".test", ".spec"];

/// A base filename starting with one of these, case-insensitively, marks
/// the file as test code — covers the `test_helpers.py`-style Python
/// convention.
pub const TEST_FILE_NAME_PREFIXES: [&str; 1] = ["test_"];

/// D-02: the LOCKED whole-word test-symbol-name prefix set. Exactly these
/// five, case-insensitive, whole-word-prefix-only. Widening this to a
/// substring match anywhere in the name (e.g. matching `agent_mock_impl`)
/// was explicitly REJECTED during planning — the user chose the
/// conservative, whole-word-only rule specifically to bound false-positive
/// risk on legitimately-named production code (`MockupRenderer`,
/// `Attestation`, `ContestEntry`, `Testament`/`TESTAMENT` must never match).
/// Do not "improve" this into a substring match.
pub const TEST_NAME_PREFIXES: [&str; 5] = ["mock", "fake", "stub", "dummy", "test"];

/// True when `source_file` (already normalized, DP-XBL-03 — one
/// normalization shared with the `Node` field) looks like a test path by
/// directory or filename convention. Backslash-separated paths are out of
/// scope: this app targets macOS only, and Graphify emits `/`.
pub fn is_test_path(source_file: &str) -> bool {
    let mut components: Vec<&str> = source_file.split('/').collect();
    let Some(base_name) = components.pop() else {
        return false;
    };

    if components
        .iter()
        .any(|c| TEST_PATH_DIRS.iter().any(|dir| c.eq_ignore_ascii_case(dir)))
    {
        return true;
    }

    let base_lower = base_name.to_ascii_lowercase();
    if TEST_FILE_NAME_PREFIXES
        .iter()
        .any(|prefix| base_lower.starts_with(prefix))
    {
        return true;
    }

    let stem = base_lower
        .rsplit_once('.')
        .map(|(stem, _ext)| stem)
        .unwrap_or(base_lower.as_str());

    TEST_FILE_STEMS.contains(&stem)
        || TEST_FILE_STEM_SUFFIXES
            .iter()
            .any(|suffix| stem.ends_with(suffix))
}

/// True when `name` starts with one of the D-02 locked prefixes
/// (case-insensitive) AND that prefix ends at a real word boundary
/// (DP-XBL-02):
///
/// (a) the prefix IS the whole name (nothing follows it),
/// (b) the next character is not alphanumeric (`_`, `-`, `.`, space, `(`,
///     ...), or
/// (c) the next character is an uppercase letter AND the matched prefix's
///     own last character (as it actually appears in `name`) is lowercase —
///     a genuine camelCase transition. This lowercase requirement is what
///     keeps all-caps `TESTAMENT` out: its `TEST` is followed by an
///     uppercase `A`, but `TEST`'s own last character is the uppercase
///     `T`, so there is no camelCase transition.
///
/// A digit is alphanumeric and is deliberately NOT a boundary
/// (`Mock2Runtime` survives) — the conservative reading of D-02.
///
/// The prefix comparison is byte-wise `eq_ignore_ascii_case` over
/// `prefix.len()` bytes. Because that comparison can only succeed when
/// those bytes in `name` are themselves ASCII, the index `prefix.len()` is
/// guaranteed to land on a char boundary — so the following
/// `name[prefix.len()..].chars().next()` cannot panic, even on a
/// multi-byte (non-ASCII) identifier.
pub fn is_test_symbol_name(name: &str) -> bool {
    for prefix in TEST_NAME_PREFIXES {
        let plen = prefix.len();
        if name.len() < plen {
            continue;
        }
        if !name.as_bytes()[..plen].eq_ignore_ascii_case(prefix.as_bytes()) {
            continue;
        }
        // Safe: the byte-equality check above only succeeds when
        // name.as_bytes()[..plen] is ASCII, so `plen` is guaranteed to be a
        // char boundary in `name`.
        match name[plen..].chars().next() {
            None => return true,
            Some(next) if !next.is_alphanumeric() => return true,
            Some(next) if next.is_uppercase() => {
                let prefix_last_is_lower = name[..plen]
                    .chars()
                    .next_back()
                    .map(char::is_lowercase)
                    .unwrap_or(false);
                if prefix_last_is_lower {
                    return true;
                }
            }
            Some(_) => {}
        }
    }
    false
}

/// The two independent classes a node can be excluded under (D-01). Kept as
/// a two-variant enum (not a bool) so `classify_test_code`'s callers can
/// distinguish "which rule caught this" for the `by_path`/`by_name`
/// counters (DP-XBL-06).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TestCodeRule {
    Path,
    Name,
}

/// Classify a node as test code, or not, from its RAW fields.
///
/// This takes `source_file`/`label`/`norm_label` as three separate raw
/// `Option<&str>` parameters rather than a constructed `Node`, and that is
/// deliberate, not an oversight: `from_json`'s node loop builds
/// `Node.label` as `norm_label.or(label).or(id)` — `norm_label` wins when
/// present. On the real `sample/graph.json`, the motivating
/// `MockAgentRuntime` node has `label: "MockAgentRuntime"` but
/// `norm_label: "mockagentruntime"` (lowercased, separators stripped), so
/// the CONSTRUCTED `Node.label` would be `"mockagentruntime"` — and a
/// whole-word prefix rule applied to THAT finds `mock` followed by a
/// lowercase `a`, which is not a word boundary, and would silently MISS the
/// exact case that motivated this filter. Evaluating the raw `label`
/// (which still says `MockAgentRuntime`) is what catches it (DP-XBL-01).
///
/// Path is checked first (DP-XBL-06): a node matching both rules is
/// reported as `Path`, never both, so the `by_path`/`by_name` counters stay
/// disjoint and sum to the total excluded count.
pub fn classify_test_code(
    source_file: Option<&str>,
    label: Option<&str>,
    norm_label: Option<&str>,
) -> Option<TestCodeRule> {
    if source_file.is_some_and(is_test_path) {
        return Some(TestCodeRule::Path);
    }
    // The explicit closure (rather than passing `is_test_symbol_name` as a
    // bare function item) is intentional: this repo's own verify gate greps
    // for a literal `is_test_symbol_name(` call to prove there is exactly
    // one production call site (plus the definition itself).
    #[allow(clippy::redundant_closure)]
    let name_hit = label
        .into_iter()
        .chain(norm_label)
        .any(|raw| is_test_symbol_name(raw));
    if name_hit {
        return Some(TestCodeRule::Name);
    }
    None
}

/// How much of a `graph.json` load was discarded by the D-01/D-02/D-03 test
/// code filter. `by_path + by_name` always equals `nodes` (DP-XBL-06 — the
/// two counters are disjoint, a node matching both rules counts under
/// `by_path` only). `edges` counts an edge exactly once even when BOTH of
/// its endpoints were excluded — it is incremented at most once per link,
/// during the single edge-loop pass, and only for a link that had already
/// survived the relation/confidence filters (so it only ever counts edges
/// that would otherwise have become real graph edges).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TestCodeExcluded {
    pub nodes: usize,
    pub by_path: usize,
    pub by_name: usize,
    pub edges: usize,
}

#[derive(Debug, Clone)]
pub struct IngestWarning {
    pub reason: String,
    pub source: String,
    pub target: String,
}

#[derive(Debug)]
pub struct IngestResult {
    pub model: Model,
    pub warnings: Vec<IngestWarning>,
    /// Quick task 260926-xbl: how many nodes/edges the hard test-code
    /// exclusion filter discarded from this load. Always populated, never
    /// optional — the filter always runs (D-03).
    pub excluded_test_code: TestCodeExcluded,
}

/// Pattern 2 (01-RESEARCH.md lines 333-357): `source`/`target` may be a bare
/// id string or an embedded node object (`{"id": "..."}"`) depending on the
/// NetworkX `node_link_data` export options.
#[derive(Deserialize)]
#[serde(untagged)]
enum SourceOrTarget {
    Id(String),
    Node { id: String },
}

impl SourceOrTarget {
    fn into_id(self) -> String {
        match self {
            SourceOrTarget::Id(s) => s,
            SourceOrTarget::Node { id } => id,
        }
    }
}

fn deserialize_source_or_target<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(SourceOrTarget::deserialize(deserializer)?.into_id())
}

/// The real `sample/graph.json`'s `community` field is a JSON integer (e.g.
/// `44`), not a string; tolerate either shape and stringify, consistent with
/// the tolerant-parsing precedent above (`SourceOrTarget`).
#[derive(Deserialize)]
#[serde(untagged)]
enum RawCommunity {
    Str(String),
    Int(i64),
}

impl RawCommunity {
    fn into_id(self) -> CommunityId {
        match self {
            RawCommunity::Str(s) => s,
            RawCommunity::Int(i) => i.to_string(),
        }
    }
}

fn deserialize_community<'de, D>(deserializer: D) -> Result<CommunityId, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(RawCommunity::deserialize(deserializer)?.into_id())
}

#[derive(Deserialize)]
struct RawNode {
    id: String,
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    norm_label: Option<String>,
    #[serde(deserialize_with = "deserialize_community")]
    community: CommunityId,
    #[serde(default)]
    file_type: Option<String>,
    // 05-09: optional so a document without the key still deserializes,
    // mirroring how `label`/`norm_label`/`file_type` already opt in.
    #[serde(default)]
    community_name: Option<String>,
    // 05-20: optional AND Option (not a required field), matching the 05-09
    // community_name precedent exactly. source_location is literally `null`
    // on 13 nodes of sample/graph.json and both keys are absent entirely
    // from clean.json/graph-demo.json today -- a non-Option, non-default
    // field would turn every one of those into a hard parse failure.
    #[serde(default)]
    source_file: Option<String>,
    #[serde(default)]
    source_location: Option<String>,
}

#[derive(Deserialize)]
struct RawLink {
    #[serde(deserialize_with = "deserialize_source_or_target")]
    source: String,
    #[serde(deserialize_with = "deserialize_source_or_target")]
    target: String,
    relation: String,
    confidence: String,
    // D-05: parsed off the raw link, stored nowhere, used nowhere in v1.
    #[serde(default)]
    #[allow(dead_code)]
    weight: Option<f64>,
    #[serde(default)]
    #[allow(dead_code)]
    confidence_score: Option<f64>,
}

#[derive(Deserialize)]
struct RawGraphDoc {
    // `nodes`/`links` are `Option` (not required fields) so that a document
    // missing one of them parses fine at the JSON-syntax level and reaches
    // the explicit `MissingArray` guard in `from_json` below, rather than
    // surfacing as an indistinguishable serde "missing field" `Parse` error
    // (01-05-PLAN.md Task 1: fatal-missing-array must be its own variant).
    #[serde(default)]
    nodes: Option<Vec<RawNode>>,
    // D-10: `graph.hyperedges` is not deserialized here — this struct simply
    // has no field for it, so serde ignores it by default (no
    // deny_unknown_fields). Same for top-level `directed`/`multigraph`/
    // `graph`/`hyperedges`/`built_at_commit` — all ignored.
    #[serde(alias = "edges", default)]
    links: Option<Vec<RawLink>>,
}

/// Parse a `graph.json` document into a `Model`, applying the D-01/D-02/D-03
/// (relation allow-list) and D-04 (`confidence == "EXTRACTED"`) filter
/// exactly once, here, before any edge is added to the graph (Pitfall 2 —
/// never re-filter downstream in `seams.rs`). Endpoint-missing edges are
/// dropped from the graph but recorded as `IngestWarning`s, never silently
/// swallowed (GRAPH-02). Also applies the quick-260926-xbl hard test-code
/// exclusion filter before any node reaches `community_pairs`/`graph`/
/// `index` — see the doc comment above `TEST_NAME_PREFIXES`.
pub fn from_json(raw: &str) -> Result<IngestResult, SeamCoreError> {
    let doc: RawGraphDoc = serde_json::from_str(raw)?;

    // Structural validation, after a successful JSON parse: a document
    // missing `nodes` or `links` entirely is a fatal load failure, never a
    // silently-empty graph (GRAPH-02).
    let nodes = doc.nodes.ok_or(SeamCoreError::MissingArray("nodes"))?;
    let links = doc.links.ok_or(SeamCoreError::MissingArray("links"))?;

    let mut graph: StableDiGraph<Node, ()> = StableDiGraph::new();
    let mut index = HashMap::new();
    // 05-09: (community, community_name) pairs, one per node, collected in
    // the same pass that already walks `nodes` — no second traversal. Fed
    // to the pure conflict resolver once, after the loop (see model.rs).
    let mut community_pairs: Vec<(CommunityId, Option<String>)> = Vec::with_capacity(nodes.len());
    // 260926-xbl: ids of nodes excluded by the test-code filter, consulted
    // by the edge loop below (DP-XBL-05) so an excluded endpoint is never
    // reported through the "endpoint not found in nodes array" warning.
    let mut excluded_ids: HashSet<String> = HashSet::new();
    let mut excluded = TestCodeExcluded::default();

    for n in &nodes {
        // DP-XBL-03: normalize once, share between the filter and the
        // `Node` field below.
        let normalized_source_file = crate::model::normalize_source_file(n.source_file.as_deref());

        // 260926-xbl: hard test-code exclusion (D-01/D-02/D-03). The raw
        // `label`/`norm_label` fields are passed directly, NOT the
        // constructed `Node.label` below (DP-XBL-01, see
        // `classify_test_code`'s doc comment for why). `continue` here runs
        // BEFORE `community_pairs.push`, `graph.add_node`, and
        // `index.insert` — a single statement that removes an excluded
        // node from the graph, the id index, AND the community-name vote
        // (DP-XBL-04).
        if let Some(rule) = classify_test_code(
            normalized_source_file.as_deref(),
            n.label.as_deref(),
            n.norm_label.as_deref(),
        ) {
            match rule {
                TestCodeRule::Path => excluded.by_path += 1,
                TestCodeRule::Name => excluded.by_name += 1,
            }
            excluded.nodes += 1;
            excluded_ids.insert(n.id.clone());
            continue;
        }

        let label = n
            .norm_label
            .clone()
            .or_else(|| n.label.clone())
            .unwrap_or_else(|| n.id.clone());
        let node = Node {
            id: n.id.clone(),
            label,
            community: n.community.clone(),
            // D-08: all file_types kept, no filter.
            file_type: n.file_type.clone(),
            community_name: n.community_name.clone(),
            source_file: normalized_source_file,
            source_line: crate::model::parse_source_line(n.source_location.as_deref()),
        };
        community_pairs.push((n.community.clone(), n.community_name.clone()));
        let idx = graph.add_node(node);
        index.insert(n.id.clone(), idx);
    }

    let community_names = crate::model::resolve_community_names(&community_pairs);

    let mut warnings = Vec::new();
    for l in &links {
        // D-01/D-02/D-03: relation allow-list — single filter point (Pitfall 2).
        if !STRUCTURAL_RELATIONS.contains(&l.relation.as_str()) {
            continue;
        }
        // D-04: only EXTRACTED confidence feeds analysis.
        if l.confidence != "EXTRACTED" {
            continue;
        }
        // 260926-xbl / DP-XBL-05: an edge touching an excluded node is
        // dropped and counted HERE, before the `index.get` lookup below —
        // this arm exists specifically so an intentional removal is never
        // reported as a missing-id warning. "This graph referenced an id it
        // never defined" and "we removed this on purpose" are different
        // facts, and only the former should ever produce an
        // `IngestWarning`.
        if excluded_ids.contains(&l.source) || excluded_ids.contains(&l.target) {
            excluded.edges += 1;
            continue;
        }
        match (index.get(&l.source), index.get(&l.target)) {
            // D-09: every kept edge treated as directed source -> target,
            // regardless of the top-level `directed` flag.
            (Some(&s), Some(&t)) => {
                graph.add_edge(s, t, ());
            }
            _ => warnings.push(IngestWarning {
                reason: "endpoint not found in nodes array".to_string(),
                source: l.source.clone(),
                target: l.target.clone(),
            }),
        }
    }

    Ok(IngestResult {
        model: Model {
            graph,
            index,
            scc: None,
            community_names,
            // 08-04: a freshly ingested graph has no live events behind it
            // yet, so nothing can be waiting on one.
            pending_edges: Default::default(),
        },
        warnings,
        excluded_test_code: excluded,
    })
}
