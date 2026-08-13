//! `from_json`: parse a Graphify `graph.json` (NetworkX `node_link_data`
//! shape), apply the D-01/D-02/D-03/D-04 relation+confidence filter exactly
//! once (Pitfall 2, 01-RESEARCH.md), and collect endpoint-missing edges as
//! `IngestWarning`s rather than silently dropping them (GRAPH-02, Pattern 1).
//!
//! Port target: `apps/seam-explorer-web/seam-explorer.html` `normalize()`
//! (lines 270-292) for the overall shape; RESEARCH.md Patterns 1 and 2 for
//! the warning-collection and tolerant-parsing behavior the JS version does
//! NOT have.

use crate::error::SeamCoreError;
use crate::model::{CommunityId, Model, Node};
use petgraph::stable_graph::StableDiGraph;
use serde::Deserialize;
use std::collections::HashMap;

/// D-01/D-02/D-03: only these five relation types count as coupling signal.
/// Single edit point if the allow-list ever changes (RESEARCH.md
/// Anti-Patterns note).
pub const STRUCTURAL_RELATIONS: [&str; 5] =
    ["calls", "references", "method", "implements", "imports_from"];

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
/// swallowed (GRAPH-02).
pub fn from_json(raw: &str) -> Result<IngestResult, SeamCoreError> {
    let doc: RawGraphDoc = serde_json::from_str(raw)?;

    // Structural validation, after a successful JSON parse: a document
    // missing `nodes` or `links` entirely is a fatal load failure, never a
    // silently-empty graph (GRAPH-02).
    let nodes = doc.nodes.ok_or(SeamCoreError::MissingArray("nodes"))?;
    let links = doc.links.ok_or(SeamCoreError::MissingArray("links"))?;

    let mut graph: StableDiGraph<Node, ()> = StableDiGraph::new();
    let mut index = HashMap::new();

    for n in &nodes {
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
        };
        let idx = graph.add_node(node);
        index.insert(n.id.clone(), idx);
    }

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
        },
        warnings,
    })
}
