//! `apply.rs`: structural mutation of a loaded [`Model`] from live
//! [`GraphEvent`]s (Phase 8, plan 08-01).
//!
//! This module owns graph mutation and NOTHING about rendering -- no
//! `egui`/`eframe` type appears here, and none may, per `seam-core`'s
//! Phase-1 mandate that it stay a standalone, app-shell-free domain crate
//! shared by both UI stacks.
//!
//! It is also the enforcement point for the invariant `event.rs` documents
//! but cannot enforce: an `AddNode` whose `community` is `None` must never
//! survive into applied state. [`resolve_community`] is the single place an
//! absent wire community becomes concrete, and it never returns an absent
//! value.

use crate::event::GraphEvent;
use crate::model::{CommunityId, Model, Node};
use petgraph::stable_graph::NodeIndex;
use std::collections::{BTreeMap, HashSet};

/// The single shared bucket every node with an unresolvable community lands
/// in (D-04: ONE bucket, not one per `source_file`).
///
/// Implemented as a reserved sentinel [`CommunityId`] rather than by widening
/// [`Node::community`] to an optional type (D-04a). `ingest.rs` tolerantly
/// accepts an arbitrary string community, so a real `graph.json` could in
/// principle name a community `__unknown__` and collide with this sentinel.
/// That risk is possible-but-vanishingly-unlikely and is a disclosed,
/// accepted tradeoff for this single-user local tool -- the same
/// small-blast-radius choice Phase 7 made when it put `Option<CommunityId>`
/// on the WIRE type (`GraphEvent::AddNode`) instead of on the internal
/// `Model`.
///
/// No new display code is needed: [`Model::community_label`] already falls
/// back to the raw id for a community it has no resolved name for, so this
/// renders as `__unknown__` out of the box.
pub const UNKNOWN_COMMUNITY: &str = "__unknown__";

/// The one bound for the whole live pipeline: how many events the rotating
/// history holds (D-01), and the same ceiling D-05a assigns to plan 08-04's
/// pending-edge store.
///
/// **One constant, deliberately, not two numbers that agree today.** D-05a's
/// wording is "one wraparound discipline, not two separate cap numbers" --
/// two independent literals would agree on the day they were written and
/// drift the first time either is tuned, leaving the history and the pending
/// store silently disagreeing about how far back "live" reaches.
///
/// It lives in `seam-core` rather than in the application crate for a
/// mechanical reason: 08-04's pending-edge store is a `seam-core` type, and
/// `seam-core` cannot import from `seam-explorer-egui` (the dependency runs
/// the other way, and must, per this crate's app-shell-free mandate).
///
/// D-01's own consequence, worth knowing before tuning this: the value is
/// exactly the range Phase 9's time-travel scrub can reach backwards from the
/// live edge. Raising it lengthens the scrub; lowering it shortens it.
pub const LIVE_BUFFER_CAPACITY: usize = 100;

/// How many times [`promote_unknown_communities`] will repeat before it stops,
/// whether or not it has reached a fixed point.
///
/// **Why more than one pass.** A promotion in one pass can be exactly the
/// evidence a DIFFERENT node needs in the next: minting a community for a
/// group of siblings gives every node with an edge into that group a
/// neighbour it can inherit from. A single-pass sweep would leave those chains
/// unresolved until the next batch arrived, which for a user who stopped
/// editing means "until they type something else".
///
/// **Why not unbounded.** The sweep runs on the UI thread, inside the frame
/// that applied the batch, and its working set is controlled by whoever is
/// sending events. An unbounded loop over a pathological input stalls that
/// frame (T-08-05-02). The cap turns the worst case into "some nodes stay
/// unresolved a little longer" -- a state D-04 already treats as entirely
/// legitimate, since "no evidence yet" is the normal condition for a node
/// whose siblings have not been edited.
///
/// Five is the value 08-RESEARCH.md proposed. Real chains observed in practice
/// are one or two links; five leaves room without letting a hostile input buy
/// meaningful frame time.
pub const MAX_PROMOTION_PASSES: usize = 5;

/// The prefix on every community the promotion sweep MINTS.
///
/// Deliberately distinct from [`UNKNOWN_COMMUNITY`] so that "still genuinely
/// unplaced" and "freshly formed by live event data" can never be confused --
/// not by a reader, not by a log line, and not by Phase 9 replaying a history
/// (T-08-05-05). The residual risk, a loaded `graph.json` whose own community
/// happens to start with this prefix, is the same disclosed-and-accepted
/// sentinel-collision tradeoff D-04a already settled for this single-user
/// local tool.
const MINTED_COMMUNITY_PREFIX: &str = "__live__:";

/// What a call to [`apply_batch`] actually did, reported as data so callers
/// never have to re-scan the model to find out.
#[derive(Debug, Default, PartialEq)]
pub struct ApplyOutcome {
    /// The FULLY RESOLVED events, in the order they were applied -- each
    /// `AddNode` carries a concrete community and the reconciled REAL node
    /// id, never the raw wire advertisement. Plan 08-03's rotating history
    /// buffer pushes exactly these, so what history records is what the
    /// graph actually did.
    pub applied: Vec<GraphEvent>,
    /// True when a node was inserted or removed. Gates the caller's
    /// whole-graph recomputations (SCC cache, seam detection) -- a
    /// label-only update changes neither.
    pub topology_changed: bool,
    /// The real ids of nodes removed by this batch, so removal consequences
    /// (stale trace/focus clearing) travel as data rather than as a second
    /// scan of the model.
    pub removed_node_ids: Vec<String>,
    /// How many `AddEdge`s were parked pending an internal target that has
    /// not arrived yet (D-05). Reported so the caller never has to diff the
    /// pending store to find out, and so 08-05's retry sweep has a place to
    /// report from.
    pub parked_edges: usize,
    /// How many `AddEdge`s were dropped because their target names something
    /// outside the user's project (D-05a). Counted, but recorded nowhere
    /// else -- a dropped edge did not happen to the graph.
    pub dropped_external_edges: usize,
    /// How many previously parked edges the retry pass landed (08-05, D-05).
    /// Each one also appears in [`Self::applied`] -- a materialized edge is a
    /// real change to the graph and must reach the history.
    pub materialized_edges: usize,
    /// How many parked edges the retry pass examined and put back, still
    /// unresolvable. Purely informational: the store's size did not change
    /// because of them.
    pub reparked_edges: usize,
    /// How many parked edges the retry pass DROPPED because their target had
    /// become classifiable as external -- the escape hatch that stops the
    /// store silting up when the graph changes shape underneath it.
    pub dropped_pending_edges: usize,
    /// What the promotion sweep did after this batch's structural changes
    /// landed (08-05, D-04). Zero-valued when nothing was parked in the
    /// unknown bucket, which is the common case.
    pub promotion: PromotionOutcome,
}

/// What one run of [`promote_unknown_communities`] did.
///
/// Reported as data for the same reason [`ApplyOutcome`] is: the caller
/// should never have to re-scan the model to find out whether the graph's
/// community structure moved, and Task 3's flood measurement needs the pass
/// count to prove the cap is doing its job under load.
#[derive(Debug, Default, PartialEq)]
pub struct PromotionOutcome {
    /// How many passes actually ran. Zero when nothing was parked -- the
    /// sweep runs on every batch and must cost nothing when idle.
    pub passes: usize,
    /// How many nodes left the unknown bucket.
    pub promoted: usize,
    /// The identifiers of communities this sweep FORMED, in the order it
    /// formed them. Non-empty is ROADMAP SC-3's amended half made visible:
    /// the live graph gained structure the original export never had.
    pub minted_communities: Vec<CommunityId>,
}

/// Edges whose target has not resolved YET, waiting for the `AddNode` that
/// might make them resolvable (D-05).
///
/// Bounded by [`LIVE_BUFFER_CAPACITY`] -- the same constant that bounds the
/// event history, deliberately, because D-05a asks for "one wraparound
/// discipline, not two separate cap numbers". Overflow evicts oldest-first,
/// exactly as the history does.
///
/// Entries hold the RAW wire endpoint strings, unchanged. They have to: the
/// whole reason an entry exists is that its target did not resolve, so there
/// is no resolved form to store, and the source is left raw too so that a
/// later `RemoveEdge` carrying the same two strings can cancel it by simple
/// equality.
#[derive(Debug, Default)]
pub struct PendingEdges {
    entries: std::collections::VecDeque<(String, String)>,
    evicted: u64,
}

impl PendingEdges {
    /// Park one unresolved edge. Re-parking a pair already waiting is a
    /// no-op -- a real editing session re-reports the same reference
    /// constantly, and letting duplicates in would evict genuinely distinct
    /// pending edges to make room for copies of one.
    pub fn park(&mut self, source: &str, target: &str) {
        if self.entries.iter().any(|(s, t)| s == source && t == target) {
            return;
        }
        if self.entries.len() >= LIVE_BUFFER_CAPACITY {
            self.entries.pop_front();
            self.evicted += 1;
        }
        self.entries
            .push_back((source.to_string(), target.to_string()));
    }

    /// Forget a parked pair. Returns whether one was actually waiting.
    pub fn cancel(&mut self, source: &str, target: &str) -> bool {
        let before = self.entries.len();
        self.entries.retain(|(s, t)| !(s == source && t == target));
        self.entries.len() != before
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// How many pending edges have been pushed out of the front since this
    /// model was loaded.
    pub fn evicted_count(&self) -> u64 {
        self.evicted
    }

    /// Drain every waiting pair, for [`retry_pending_edges`] to re-examine.
    /// Crate-visible: the retry pass is `seam-core`'s own, and nothing
    /// outside this crate has any business emptying the store.
    pub(crate) fn take_all(&mut self) -> Vec<(String, String)> {
        self.entries.drain(..).collect()
    }
}

/// Turn an optional wire community into a concrete one (D-04).
///
/// 1. An explicit wire value is used verbatim -- the sender knew something
///    this function does not.
/// 2. Otherwise, inherit from a node sharing `source_file`, taking the
///    lexicographically smallest community among the siblings. That
///    tie-break is not invented here: it is the convention
///    [`crate::model::resolve_community_names`] already established, so the
///    result never depends on graph iteration order.
/// 3. Otherwise, [`UNKNOWN_COMMUNITY`].
///
/// Never returns an absent value -- that is the whole point (see the module
/// doc).
pub fn resolve_community(
    model: &Model,
    community: Option<&CommunityId>,
    source_file: Option<&str>,
) -> CommunityId {
    if let Some(community) = community {
        return community.clone();
    }
    if let Some(path) = source_file {
        let sibling = model
            .graph
            .node_weights()
            .filter(|n| n.source_file.as_deref() == Some(path))
            .map(|n| &n.community)
            .min();
        if let Some(community) = sibling {
            return community.clone();
        }
    }
    UNKNOWN_COMMUNITY.to_string()
}

/// Reconcile an incoming node id against the ids the loaded graph actually
/// uses (Phase 7 DP-07-01).
///
/// `apps/seam-client` advertises `{source_file}::{symbol}`; Graphify's export
/// uses its own opaque ids, with the plain symbol name in [`Node::label`] and
/// the repo-relative path in [`Node::source_file`]. The two schemes can never
/// compare equal, so without this function every edit to an already-known
/// symbol would insert a duplicate node.
///
/// - Exact hit in `model.index` first.
/// - On a miss, take the trailing segment of `id` after its last `::` (the
///   whole string when there is none) and look for a node with a matching
///   `source_file` AND a `label` equal to that segment.
/// - Ambiguity resolves to the lexicographically smallest real node id --
///   the same deterministic tie-break rule as [`resolve_community`].
///
/// Returns nothing when this is a genuinely new symbol.
pub fn resolve_node_id(model: &Model, id: &str, source_file: Option<&str>) -> Option<NodeIndex> {
    if let Some(&idx) = model.index.get(id) {
        return Some(idx);
    }
    let path = source_file?;
    let symbol = trailing_symbol(id);
    model
        .graph
        .node_indices()
        .filter(|&idx| {
            let node = &model.graph[idx];
            node.source_file.as_deref() == Some(path) && node.label == symbol
        })
        .min_by(|&a, &b| model.graph[a].id.cmp(&model.graph[b].id))
}

/// "The symbol part of a qualified name" -- defined ONCE, here, and shared by
/// [`resolve_node_id`] and [`resolve_edge_target`].
///
/// The two callers reconcile different wire shapes (`{source_file}::{symbol}`
/// versus a cross-module reference as written), but they must agree on where
/// the symbol starts or one of them will resolve a name the other rejects.
/// Two copies of `rsplit("::")` would agree on the day they were written; one
/// definition cannot drift from itself.
fn trailing_symbol(name: &str) -> &str {
    name.rsplit("::").next().unwrap_or(name)
}

/// Whether an `AddEdge` target that matched no node could EVER match one
/// (D-05a).
///
/// This is the decision that keeps the pending-edge store from becoming a
/// leak: `Internal` targets are parked and retried, `External` ones are
/// dropped on the spot because no future `AddNode` can ever make them
/// resolvable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetClass {
    Internal,
    External,
}

/// The language-level crate roots that can never be part of a user's own
/// project, whatever the loaded graph happens to contain.
///
/// **This is not a general-purpose denylist and must not grow into one.** The
/// client deliberately refused to maintain an allow/deny list (see
/// `detect.rs`'s own blind-spot inventory) precisely because such a list is
/// never finished, and adding third-party crate names here would recreate
/// exactly that maintenance burden on this side of the wire. Everything
/// beyond these four roots is classified by evidence the loaded graph
/// supplies (see [`local_roots`]) or defaults to external -- which already
/// covers every third-party crate for free.
pub const EXTERNAL_ROOTS: &[&str] = &["std", "core", "alloc", "proc_macro"];

/// The set of leading-segment tokens this loaded graph can vouch for as
/// belonging to the user's own project.
///
/// Derived from the graph itself rather than configured: for every node,
/// every component of its `source_file` (with any extension stripped and
/// hyphens folded to underscores, so `my-crate/src/lib.rs` yields
/// `my_crate`, `src` and `lib`), plus every node's `label` verbatim.
///
/// Two false-positive shapes are known and ACCEPTED:
///
/// 1. a directory name that coincidentally matches an external crate's root
///    (a project with a `src/serde/` directory vouches for `serde::`);
/// 2. a label that coincidentally matches an external type name (the real
///    `sample/graph.json` contains a node labelled `String`, which makes
///    `String::from` look internal).
///
/// Both cost exactly one parked edge that never resolves and is eventually
/// evicted by the store's bound -- never a wrong graph, never a synthesized
/// noise node. That asymmetry is the whole justification for deriving this
/// set: a false positive is cheap and self-clearing, while the alternative (a
/// hand-maintained denylist) is expensive, never finished, and wrong in the
/// direction that actually damages the picture.
pub fn local_roots(model: &Model) -> HashSet<String> {
    let mut roots = HashSet::new();
    for node in model.graph.node_weights() {
        if let Some(path) = node.source_file.as_deref() {
            for component in path.split('/') {
                let stem = match component.rfind('.') {
                    Some(dot) => &component[..dot],
                    None => component,
                };
                let token = stem.replace('-', "_");
                if !token.is_empty() {
                    roots.insert(token);
                }
            }
        }
        roots.insert(node.label.clone());
    }
    roots
}

/// Decide whether an unresolvable target is worth parking (D-05a).
///
/// The leading segment before the first `::` (the whole string when there is
/// none) is what gets judged: a language root is external, a token the loaded
/// graph vouches for is internal, and **anything else defaults to external**.
///
/// That default is deliberate and it has a real cost, stated here rather than
/// discovered later: a genuinely new top-level module whose root appears
/// nowhere in the loaded graph -- no directory component, no node label --
/// classifies as external, and its edges are dropped rather than parked. This
/// is a disclosed limitation of a design that has to terminate. The
/// alternative default (park anything unrecognised) makes the pending store
/// unbounded in the only way that matters: every standard-library and
/// third-party reference an editing session produces, forever, crowding out
/// the internal edges that could actually resolve.
pub fn classify_target(target: &str, local_roots: &HashSet<String>) -> TargetClass {
    let root = target.split("::").next().unwrap_or(target);
    if EXTERNAL_ROOTS.contains(&root) {
        return TargetClass::External;
    }
    if local_roots.contains(root) {
        return TargetClass::Internal;
    }
    TargetClass::External
}

/// Resolve an `AddEdge.source` -- a bare repo-relative file path standing for
/// that file's own node -- onto a real [`NodeIndex`], synthesizing the node
/// when the graph has none.
///
/// 1. A node whose `source_file` AND `label` are both the path. That is the
///    real export's one-node-per-file convention, confirmed against
///    `sample/graph.json` (a file node's `label`, `norm_label` and
///    `source_file` are all the repo-relative path, while its `id` is an
///    unrelated slug).
/// 2. Failing that, an exact id match -- so a source that has already been
///    synthesized once resolves onto itself rather than being synthesized
///    twice.
/// 3. Failing that, synthesize: the path as both id and label, the path as
///    `source_file`, the community from the existing [`resolve_community`]
///    (never a second assignment rule), and nothing for the export-only
///    fields no event ever carried.
///
/// **Why synthesizing is always safe HERE and never safe for a target.** The
/// source endpoint is, by construction, a file the user is actively editing
/// inside their own project: the client only emits an edge when it has a
/// repo-relative path for the file being written (`detect.rs` skips edge
/// emission entirely without one). A target is the opposite -- an
/// unfiltered reference as written, just as likely to name a
/// standard-library symbol. Synthesizing those would fill the live graph with
/// noise nodes and destroy the very picture this app exists to show
/// (T-08-04-01).
pub fn resolve_edge_source(model: &mut Model, path: &str) -> NodeIndex {
    if let Some(idx) = find_edge_source(model, path) {
        return idx;
    }

    let community = resolve_community(model, None, Some(path));
    let node = Node {
        id: path.to_string(),
        label: path.to_string(),
        community,
        file_type: None,
        community_name: None,
        source_file: Some(path.to_string()),
        source_line: None,
    };
    let idx = model.graph.add_node(node);
    model.index.insert(path.to_string(), idx);
    idx
}

/// The lookup half of [`resolve_edge_source`], with no synthesis.
///
/// Separate because `apply_remove_edge` must never create the node it was
/// asked to disconnect: a removal that synthesized its own endpoint would
/// grow the graph while claiming to shrink it.
fn find_edge_source(model: &Model, path: &str) -> Option<NodeIndex> {
    let file_node = model
        .graph
        .node_indices()
        .filter(|&idx| {
            let node = &model.graph[idx];
            node.source_file.as_deref() == Some(path) && node.label == path
        })
        .min_by(|&a, &b| model.graph[a].id.cmp(&model.graph[b].id));
    file_node.or_else(|| model.index.get(path).copied())
}

/// Resolve an `AddEdge.target` -- a cross-module reference exactly as it was
/// written in the diff -- onto an EXISTING node, or nothing.
///
/// Never synthesizes; see [`resolve_edge_source`] for why that asymmetry is
/// the point rather than an inconsistency.
///
/// - An exact id match first.
/// - Otherwise the [`trailing_symbol`] of the reference, matched against node
///   `label`s.
/// - Ambiguity (several files defining the same symbol name) resolves to the
///   lexicographically smallest REAL node id -- the same deterministic
///   tie-break [`resolve_community`] and [`resolve_node_id`] already use, so
///   the answer never depends on graph iteration order (T-08-04-03).
pub fn resolve_edge_target(model: &Model, target: &str) -> Option<NodeIndex> {
    if let Some(&idx) = model.index.get(target) {
        return Some(idx);
    }
    let symbol = trailing_symbol(target);
    model
        .graph
        .node_indices()
        .filter(|&idx| model.graph[idx].label == symbol)
        .min_by(|&a, &b| model.graph[a].id.cmp(&model.graph[b].id))
}

/// Apply one `AddNode`. Returns the FULLY RESOLVED event describing what
/// actually happened (concrete community, real node id), or nothing when the
/// call was a no-op.
///
/// On a hit, the existing node's `label` and `source_file` are updated in
/// place and its **`community` is never touched, not even when the wire
/// carried an explicit one**. EVENT-05 fixes the loaded grouping, and
/// `event.rs`'s own doc comment says a live event may add or remove presence
/// but may never move a node between communities. This is also the structural
/// mitigation for T-08-01-02: a crafted `add_node` reusing a real id cannot
/// silently redraw every seam, because the update branch has no write path to
/// `community` at all.
pub fn apply_add_node(
    model: &mut Model,
    id: &str,
    label: &str,
    community: Option<&CommunityId>,
    source_file: Option<&str>,
) -> Option<GraphEvent> {
    if let Some(idx) = resolve_node_id(model, id, source_file) {
        let node = &mut model.graph[idx];
        node.label = label.to_string();
        if let Some(path) = source_file {
            node.source_file = Some(path.to_string());
        }
        return Some(GraphEvent::AddNode {
            id: node.id.clone(),
            label: node.label.clone(),
            community: Some(node.community.clone()),
            source_file: node.source_file.clone(),
        });
    }

    let community = resolve_community(model, community, source_file);
    let source_file = source_file.map(str::to_string);
    let node = Node {
        id: id.to_string(),
        label: label.to_string(),
        community: community.clone(),
        // The wire carries none of these three; inventing values here would
        // be fabricating export metadata that no event ever contained.
        file_type: None,
        community_name: None,
        source_file: source_file.clone(),
        source_line: None,
    };
    let idx = model.graph.add_node(node);
    model.index.insert(id.to_string(), idx);
    Some(GraphEvent::AddNode {
        id: id.to_string(),
        label: label.to_string(),
        community: Some(community),
        source_file,
    })
}

/// Apply one `RemoveNode`, resolving the id the same way [`apply_add_node`]
/// does. Returns the REAL id removed, or nothing when no such node exists.
///
/// Removal is immediate and total, per D-03 -- `StableDiGraph::remove_node`
/// drops every incident edge with it, and the `model.index` entry goes too.
/// No fade, no tombstone, no deferred cleanup.
pub fn apply_remove_node(model: &mut Model, id: &str) -> Option<String> {
    let idx = resolve_node_id(model, id, None)?;
    let removed = model.graph.remove_node(idx)?;
    model.index.remove(&removed.id);
    Some(removed.id)
}

/// What one [`apply_add_edge`] call did. Four outcomes, kept distinct
/// because the caller has to treat them differently: only `Applied` is worth
/// recording in the history, and `Parked` and `DroppedExternal` are counted
/// separately because they mean opposite things about the future (one might
/// still land, the other never will).
#[derive(Debug, Clone, PartialEq)]
pub enum AddEdgeOutcome {
    /// The edge landed. Carries the fully resolved event, both endpoints as
    /// REAL node ids, ready for the history.
    Applied(GraphEvent),
    /// An edge already connected that ordered pair (T-08-04-04).
    AlreadyPresent,
    /// The target is inside the user's project but has not arrived yet, so
    /// the edge waits (D-05).
    Parked,
    /// The target names something outside the user's project and can never
    /// resolve, so the edge is gone (D-05a).
    DroppedExternal,
}

/// Apply one `AddEdge`, reconciling the two wire identity shapes against the
/// loaded graph.
///
/// **The target is resolved FIRST, before the source is so much as looked
/// at, and that order is load-bearing.** `resolve_edge_source` synthesizes a
/// node when the graph has none for a path; if it ran first, every dropped
/// or parked edge would leave a source node behind as a side effect, and the
/// live graph would slowly fill with file nodes that no edge ever connects.
/// This is the single most likely thing for a later refactor to reorder, so:
/// do not. `an_edge_to_an_external_target_is_dropped_and_never_parked`
/// asserts the node count precisely to catch it.
pub fn apply_add_edge(
    model: &mut Model,
    source: &str,
    target: &str,
    local_roots: &HashSet<String>,
) -> AddEdgeOutcome {
    let Some(to) = resolve_edge_target(model, target) else {
        return match classify_target(target, local_roots) {
            TargetClass::External => AddEdgeOutcome::DroppedExternal,
            TargetClass::Internal => {
                model.pending_edges.park(source, target);
                AddEdgeOutcome::Parked
            }
        };
    };

    let from = resolve_edge_source(model, source);
    if model.graph.find_edge(from, to).is_some() {
        return AddEdgeOutcome::AlreadyPresent;
    }
    model.graph.add_edge(from, to, ());
    AddEdgeOutcome::Applied(GraphEvent::AddEdge {
        source: model.graph[from].id.clone(),
        target: model.graph[to].id.clone(),
    })
}

/// Apply one `RemoveEdge`. Returns the resolved event when an edge actually
/// went, or nothing.
///
/// Cancels any parked entry for the same raw pair EITHER WAY. Without that,
/// a `RemoveEdge` arriving for a pair still waiting to materialize would be
/// a silent no-op and the edge would appear later -- after the user had
/// already deleted it (T-08-04-06).
///
/// Neither endpoint is ever synthesized here; see [`find_edge_source`].
pub fn apply_remove_edge(model: &mut Model, source: &str, target: &str) -> Option<GraphEvent> {
    model.pending_edges.cancel(source, target);

    let from = find_edge_source(model, source)?;
    let to = resolve_edge_target(model, target)?;
    let edge = model.graph.find_edge(from, to)?;
    model.graph.remove_edge(edge);
    Some(GraphEvent::RemoveEdge {
        source: model.graph[from].id.clone(),
        target: model.graph[to].id.clone(),
    })
}

/// What one [`retry_pending_edges`] pass did.
#[derive(Debug, Default)]
struct RetryOutcome {
    /// The fully resolved events for edges that landed, ready for the history.
    materialized: Vec<GraphEvent>,
    reparked: usize,
    dropped: usize,
}

/// Re-examine every parked edge exactly once, in case the events just applied
/// made one of them resolvable (D-05).
///
/// **The retry re-enters [`apply_add_edge`] rather than re-resolving by hand.**
/// A parked edge and a freshly arrived one must be judged by identical rules,
/// or the contents of the graph start depending on when an event happened to
/// arrive rather than on what it said. Re-entering the same path also means
/// each of the three non-landing outcomes is handled by the ordinary rule that
/// already owns it: a pair that still cannot resolve is re-parked by the
/// parking rule, one whose target became external is dropped by the
/// classification rule, and the store's bound and eviction discipline apply
/// unchanged.
///
/// **One pass over a DRAINED snapshot, never a loop over the live store.**
/// Draining first is what makes it impossible for this pass to re-examine a
/// pair it just re-parked -- which, on the UI thread, would be a spin
/// (T-08-05-03). It is also why a full store survives a retry with no
/// evictions: the store is empty when the re-parking starts.
///
/// `local_roots` is recomputed here rather than reused from the batch's own
/// lazy value. A retry runs precisely because new nodes arrived, which is
/// exactly when the vouched-for root set has changed; a stale set would
/// classify a newly-vouched-for root as external and drop an edge that had
/// just become resolvable.
fn retry_pending_edges(model: &mut Model) -> RetryOutcome {
    let mut outcome = RetryOutcome::default();
    if model.pending_edges.is_empty() {
        return outcome;
    }

    let waiting = model.pending_edges.take_all();
    let roots = local_roots(model);
    for (source, target) in waiting {
        match apply_add_edge(model, &source, &target, &roots) {
            AddEdgeOutcome::Applied(resolved) => outcome.materialized.push(resolved),
            // The pair resolved, but the graph already had that edge -- so it
            // leaves the store (correctly: it will never need retrying again)
            // and is counted nowhere, because nothing happened to the graph.
            AddEdgeOutcome::AlreadyPresent => {}
            AddEdgeOutcome::Parked => outcome.reparked += 1,
            AddEdgeOutcome::DroppedExternal => outcome.dropped += 1,
        }
    }
    outcome
}

/// Move nodes out of the unknown bucket once later events supply the evidence
/// to place them, repeating to a fixed point or [`MAX_PROMOTION_PASSES`],
/// whichever comes first (D-04).
///
/// The user's mandate, verbatim: *"All events have the power to create unknown
/// or ultimately known communities. Evolve the graph as closely to the data in
/// the event as possible."* and *"group in unknown until events result in
/// siblings and communities to be formed."* 08-01 assigned a community once at
/// insertion and never revisited it; this is the half that makes the model
/// actually dynamic.
///
/// **This is a receiver-side inference, not a wire event.** No `GraphEvent`
/// variant exists for a community change and none should: REQUIREMENTS.md puts
/// attribute and community-reassignment events out of scope for v1.1, and
/// adding one here would violate that from the inside. Everything below is
/// derived from the four events already defined.
///
/// Each pass considers only the nodes currently in the unknown bucket, never
/// the whole graph, so the cost is proportional to the unresolved set rather
/// than to graph size. Within a pass, in order:
///
/// 1. inherit from a resolved neighbour across any incident edge, either
///    direction;
/// 2. inherit from another node sharing this node's exact `source_file`;
/// 3. otherwise group the still-unknown nodes by `source_file` and mint one
///    new community per group of two or more;
/// 4. leave anything else unknown -- "no evidence yet" is a legitimate state,
///    not a problem to paper over.
///
/// Ties in steps 1 and 2 break to the lexicographically smallest community.
/// That rule is not invented here: it is the convention
/// [`crate::model::resolve_community_names`] established and
/// [`resolve_community`] already reuses, so the answer never depends on graph
/// iteration order.
pub fn promote_unknown_communities(model: &mut Model) -> PromotionOutcome {
    let mut outcome = PromotionOutcome::default();

    while outcome.passes < MAX_PROMOTION_PASSES {
        let unknown: Vec<NodeIndex> = model
            .graph
            .node_indices()
            .filter(|&idx| model.graph[idx].community == UNKNOWN_COMMUNITY)
            .collect();
        // The zero-cost idle path: nothing parked, no pass, no reported work.
        if unknown.is_empty() {
            break;
        }
        outcome.passes += 1;

        // **Every decision in a pass is made against the state at the START of
        // that pass, and only applied once they are all made.** Promoting
        // in-place mid-pass would let a node's fate depend on whether its
        // evidence happened to sit at a lower `NodeIndex` than itself -- so
        // two graphs with identical structure but different insertion order
        // would converge in different numbers of passes and, once the cap
        // bites, to different ANSWERS. Deciding from a snapshot makes the
        // result a function of graph structure alone.
        let mut decisions: Vec<(NodeIndex, CommunityId)> = Vec::new();
        // Steps 1 and 2, for every parked node, before any minting -- so a
        // node that CAN inherit real structure never gets a synthetic
        // community instead.
        let mut still_unknown: Vec<NodeIndex> = Vec::new();
        for idx in unknown {
            match inheritable_community(model, idx) {
                Some(community) => decisions.push((idx, community)),
                None => still_unknown.push(idx),
            }
        }

        // Step 3. `BTreeMap` rather than `HashMap`: the minted list is
        // reported to the caller and asserted by tests, so its order must not
        // depend on hash iteration. A node with NO `source_file` has no
        // siblings by definition and is deliberately absent from the grouping
        // -- pathless nodes are not each other's siblings.
        let mut groups: BTreeMap<String, Vec<NodeIndex>> = BTreeMap::new();
        for idx in still_unknown {
            if let Some(path) = model.graph[idx].source_file.clone() {
                groups.entry(path).or_default().push(idx);
            }
        }
        let mut minted_this_pass: Vec<(CommunityId, String)> = Vec::new();
        for (path, members) in groups {
            if members.len() < 2 {
                continue;
            }
            let minted = format!("{MINTED_COMMUNITY_PREFIX}{path}");
            for idx in members {
                decisions.push((idx, minted.clone()));
            }
            minted_this_pass.push((minted, path));
        }

        let mut promoted_this_pass = 0usize;
        for (idx, community) in decisions {
            if promote_node(model, idx, community) {
                promoted_this_pass += 1;
            }
        }
        for (minted, path) in minted_this_pass {
            // A readable name, or the seam list shows the raw synthetic
            // identifier at the user. `community_label` falls back to the id
            // when there is no entry, so this is the only thing standing
            // between a formed community and an unreadable row.
            model
                .community_names
                .entry(minted.clone())
                .or_insert_with(|| format!("(new) {path}"));
            outcome.minted_communities.push(minted);
        }

        outcome.promoted += promoted_this_pass;
        // A pass that changed nothing means the fixed point is reached; a
        // further pass would look at the same nodes and reach the same answer.
        if promoted_this_pass == 0 {
            break;
        }
    }

    outcome
}

/// The community a parked node can inherit, or nothing.
///
/// Neighbour evidence first (an actual edge is the strongest signal the graph
/// has), then an exact `source_file` sibling. Both filter the sentinel out
/// before taking the minimum -- inheriting "unknown" from an unknown node
/// would be a promotion that promoted nothing.
fn inheritable_community(model: &Model, idx: NodeIndex) -> Option<CommunityId> {
    let neighbour = model
        .graph
        .neighbors_undirected(idx)
        .map(|other| &model.graph[other].community)
        .filter(|community| community.as_str() != UNKNOWN_COMMUNITY)
        .min()
        .cloned();
    if neighbour.is_some() {
        return neighbour;
    }

    let path = model.graph[idx].source_file.clone()?;
    model
        .graph
        .node_indices()
        .filter(|&other| other != idx)
        .map(|other| &model.graph[other])
        .filter(|node| {
            node.source_file.as_deref() == Some(path.as_str())
                && node.community != UNKNOWN_COMMUNITY
        })
        .map(|node| &node.community)
        .min()
        .cloned()
}

/// Write a community onto a node, and ONLY onto a node currently parked in the
/// unknown bucket.
///
/// **This guard is EVENT-05 expressed as a code invariant rather than as a
/// convention, and it is the single most important line in this module.** The
/// sweep is the first and only code in this phase that deliberately writes
/// `Node.community`; every other mutation path (`apply_add_node`'s update
/// branch, `resolve_edge_source`'s synthesis) has no write access to the field
/// at all. If this function could reach an originally-loaded node, a live
/// event could silently redraw every seam and flip every verdict, with no
/// error anywhere to say the picture had become wrong (T-08-05-01). It cannot,
/// because the only nodes it will write to are ones that were never in
/// `graph.json` in the first place -- a node in the unknown bucket got there
/// by arriving on the wire with nothing to place it.
///
/// Returns whether the write happened, so the caller's promoted count reflects
/// reality rather than intent.
fn promote_node(model: &mut Model, idx: NodeIndex, community: CommunityId) -> bool {
    let node = &mut model.graph[idx];
    debug_assert_eq!(
        node.community, UNKNOWN_COMMUNITY,
        "EVENT-05: the promotion sweep may only ever write to a node parked in \
         the unknown bucket, never to {} (community {})",
        node.id, node.community
    );
    if node.community != UNKNOWN_COMMUNITY {
        return false;
    }
    node.community = community;
    true
}

/// Apply a whole drained batch in the order received. The UI-thread channel
/// is FIFO (`std::sync::mpsc::sync_channel`, see
/// `event_stream::EventReceiver::drain`), so "in order received" is "in the
/// order the senders produced them".
pub fn apply_batch(model: &mut Model, events: &[GraphEvent]) -> ApplyOutcome {
    let mut outcome = ApplyOutcome::default();
    // Computed at most ONCE per batch, and lazily: a batch with no edge
    // events never pays for it at all, and one that does pays after its node
    // events have landed, so a file added earlier in the same batch can
    // vouch for its own root (T-08-04-07).
    let mut roots: Option<HashSet<String>> = None;

    for event in events {
        match event {
            GraphEvent::AddNode {
                id,
                label,
                community,
                source_file,
            } => {
                let before = model.graph.node_count();
                if let Some(resolved) =
                    apply_add_node(model, id, label, community.as_ref(), source_file.as_deref())
                {
                    // An update-in-place changes a label, not the shape of
                    // the graph -- neither the SCC cache nor the seam
                    // ranking can move because of it.
                    if model.graph.node_count() != before {
                        outcome.topology_changed = true;
                    }
                    outcome.applied.push(resolved);
                }
            }
            GraphEvent::RemoveNode { id } => {
                if let Some(removed) = apply_remove_node(model, id) {
                    outcome.topology_changed = true;
                    outcome.applied.push(GraphEvent::RemoveNode {
                        id: removed.clone(),
                    });
                    outcome.removed_node_ids.push(removed);
                }
            }
            GraphEvent::AddEdge { source, target } => {
                if roots.is_none() {
                    roots = Some(local_roots(model));
                }
                let known = roots.as_ref().expect("just populated above");
                match apply_add_edge(model, source, target, known) {
                    AddEdgeOutcome::Applied(resolved) => {
                        outcome.topology_changed = true;
                        outcome.applied.push(resolved);
                    }
                    // A re-report of an edge already present changed nothing,
                    // so it is recorded nowhere -- the history must hold what
                    // the graph DID, not what the wire said (T-08-03-02).
                    AddEdgeOutcome::AlreadyPresent => {}
                    AddEdgeOutcome::Parked => outcome.parked_edges += 1,
                    AddEdgeOutcome::DroppedExternal => outcome.dropped_external_edges += 1,
                }
            }
            GraphEvent::RemoveEdge { source, target } => {
                if let Some(resolved) = apply_remove_edge(model, source, target) {
                    outcome.topology_changed = true;
                    outcome.applied.push(resolved);
                }
            }
        }
    }

    // ORDER IS LOAD-BEARING, and this is the thing a later refactor is most
    // likely to reorder: the pending-edge retry runs after every event in the
    // batch has been applied structurally and BEFORE the promotion sweep. A
    // materialized edge is evidence the sweep's neighbour-inheritance step
    // consumes, so retrying afterward would leave a node parked for an extra
    // batch for no reason at all.
    // `a_materialized_edge_supplies_promotion_evidence_in_the_same_batch`
    // fails if these two are swapped.
    let retry = retry_pending_edges(model);
    outcome.materialized_edges = retry.materialized.len();
    outcome.reparked_edges = retry.reparked;
    outcome.dropped_pending_edges = retry.dropped;
    if !retry.materialized.is_empty() {
        outcome.topology_changed = true;
        outcome.applied.extend(retry.materialized);
    }

    // The promotion sweep runs AFTER every structural change in the batch has
    // landed, so it sees the evidence the whole batch supplied rather than the
    // evidence available part-way through it.
    outcome.promotion = promote_unknown_communities(model);
    if outcome.promotion.promoted > 0 {
        // A promotion is not a change in node/edge presence, but it IS a
        // change in community membership, and `seams::detect` groups by
        // community -- so the ranked seam list is stale until it is
        // recomputed. `topology_changed` is the flag the caller gates that
        // recomputation on, so a promotion has to set it or ROADMAP SC-1's
        // "not a stale list beside a changed canvas" fails for exactly the
        // events this plan exists to handle.
        outcome.topology_changed = true;
    }

    outcome
}
