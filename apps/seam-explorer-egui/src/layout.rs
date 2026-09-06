//! Custom `egui_graphs::Layout`: seam pull-apart positioning (D-13,
//! RESEARCH Pattern 4). `egui_graphs`'s built-in layouts are
//! Fruchterman-Reingold (position-based); the D3 original used d3-force
//! (velocity/strength-based) -- there is no direct translation, so this
//! ports the **target positions** (`cx +/- min(W*0.28, 320)`), not the
//! force-simulation mechanics (RESEARCH Pitfall 5).
//!
//! `Layout::next<N, E, Ty, Ix, Dn, De>` is itself generic over the node
//! payload type `N` (bound only by `Clone`), so it cannot read a node's
//! `community` -- or its `id` -- directly; that data lives in
//! `graph_view::PayloadNode`, a concrete type this module deliberately does
//! not depend on. Instead, `graph_view::inject_layout_targets` computes each
//! node's target x (via `seam_target_x` below) and repulsion group (via
//! `seam_group`) and injects both into this module's persisted
//! `SeamLayoutState` immediately before the `GraphView` widget's own
//! `sync_layout` reads it. `SeamLayout::next` then only has to ease each
//! node toward the target already sitting in its own state -- no payload
//! type needed.
//!
//! # Why the persisted state is keyed by node id, not by graph index (08-02)
//!
//! This state used to be keyed by `NodeIndex::index()`, on the argument
//! that the index was stable across frames because the render graph was
//! rebuilt the same way from the same immutable `Model` every frame. That
//! was literally true through Phases 5, 6 and 7, and stopped being true the
//! moment Phase 8 made the model mutate live. Two independent, compounding
//! mechanisms break an index key:
//!
//! 1. **Dense per-frame renumbering.** `graph_view::build_graph` constructs
//!    a brand-new graph every frame, adding the currently-present nodes in
//!    ascending model order onto an empty graph -- so render-graph indices
//!    are always a dense `0..n`. Removing any node shifts every later
//!    node's key by one, reassigning a whole tail of persisted positions at
//!    once.
//! 2. **`StableDiGraph`'s free list.** `try_add_node` hands a freed index
//!    straight back out (LIFO). Remove node A, then add an unrelated node
//!    B, and B gets A's index -- and, under an index key, A's cached
//!    position. That is not staleness; it is a persisted position silently
//!    retargeted onto a completely different node, with no error anywhere.
//!
//! So `targets`, `groups` and `positions` are keyed by the stable
//! `seam_core::Node::id`, which never changes across a node's lifetime.
//! Because `next` still only ever sees indices, `inject_layout_targets`
//! also builds a per-frame `id_by_index` translation table (it has concrete
//! `PayloadNode` access and can read `id`; `next` cannot) and injects it
//! alongside the other two maps. `next` translates index -> id through that
//! table and does every lookup and write-back by id.
//!
//! The deterministic seed/jitter spread is likewise derived from the id
//! (see `id_key`), not the index, so the spread that prevents the
//! first-frame collapse survives a renumbering intact.

use egui_graphs::{DisplayEdge, DisplayNode, Graph, Layout, LayoutState};
use petgraph::stable_graph::IndexType;
use petgraph::EdgeType;
use std::collections::HashMap;

/// Fraction of the remaining distance to the target closed per frame --
/// tuned so the pull-apart reads as a deliberate motion rather than a snap
/// (Task 2 action: "Ease nodes toward their targets across frames").
const EASE_FACTOR: f32 = 0.12;

/// Inter-node repulsion, ported from `egui_graphs` 0.31.0's own
/// Fruchterman-Reingold implementation
/// (`egui_graphs::layouts::force_directed::implementations::fruchterman_reingold::core`,
/// `compute_repulsion`/`apply_displacements` -- `pub(crate)` in that crate,
/// so the formula is re-implemented here rather than imported). Without
/// this, every node whose target_x coincides with others' (the unfocused
/// default view, where every community's target is `center.x`) eases onto
/// the exact same point given enough frames -- invisible at 4-node fixture
/// scale, but a deterministic, N-scaling collapse into an unreadable
/// overlapping column at `sample/graph.json`'s 1097-node scale (RCA,
/// `.planning/debug/no-repulsion-in-seamlayout.md`).
///
/// Repulsion is computed only between nodes sharing the same `group` (see
/// `seam_group`) -- confined to run "within each side only", per the
/// design's Pattern 4 -- so it spreads nodes apart locally without ever
/// pushing side-A nodes toward side-B's target (which would fight the
/// seam pull-apart's whole purpose) or diluting the `center_x` collapse
/// that the unfocused view intentionally wants.
mod repulsion {
    /// Ideal-separation scale factor -- multiplies the `sqrt(area/n)`
    /// baseline (same `k` derivation `egui_graphs`' FR uses,
    /// `prepare_constants` in the crate source).
    const K_SCALE: f32 = 1.0;
    /// Coulomb-like repulsion coefficient (same role as FR's `c_repulse`).
    const C_REPULSE: f32 = 1.0;
    /// Floor on the distance used in the `1/distance` force term (pixels).
    /// Unlike FR's `epsilon` (1e-3, meant only to avoid a literal
    /// divide-by-zero), this is deliberately much larger: it also caps the
    /// *maximum single-pair force* at `k*k/DISTANCE_FLOOR`, so a cluster of
    /// many near-coincident nodes produces a large but finite, sane force
    /// sum (proportional to how many close neighbors a node has) instead
    /// of a handful of near-infinite pair forces drowning out the rest.
    const DISTANCE_FLOOR: f32 = 1.0;
    /// Scales the raw per-frame force sum down to a position delta --
    /// plays the role of FR's `dt * damping`. Deliberately small: with
    /// many nodes sharing a group (the unfocused view puts every node in
    /// one group), the *summed* force a crowded node feels from all its
    /// neighbors is large by construction, so the per-neighbor scale must
    /// be small for the total step to stay sane -- tuned empirically
    /// against `sample/graph.json`'s 1097-node scale (see
    /// `examples/dev_tune_repulsion.rs`), not analytically derived.
    const STEP_SCALE: f32 = 0.02;
    /// Caps a single frame's repulsion-driven displacement -- a safety
    /// ceiling against numerical blowup, not the primary limiter (that's
    /// `DISTANCE_FLOOR` capping each pair's contribution). Deliberately
    /// generous relative to `EASE_FACTOR`'s unclamped, distance-proportional
    /// pull-to-target, so repulsion can actually win when many nodes are
    /// crowded onto the same target (see
    /// `.planning/debug/no-repulsion-in-seamlayout.md` -- a small flat
    /// clamp here was the previous tuning's bug: it discarded the
    /// aggregate-force signal from crowding, letting the pull-to-target
    /// spring re-collapse the cluster every single frame).
    const MAX_STEP: f32 = 260.0;

    /// Per-node repulsion displacement for one group of same-side nodes,
    /// computed from their current positions (O(m^2) pairwise over the
    /// group's `m` members, same approach `egui_graphs`' own FR algorithm
    /// uses for the whole graph -- here bounded to one group's membership
    /// instead of the whole graph). `members` holds *local* indices into
    /// `positions`/`disp` (both aligned 1:1 with the frame's node list) --
    /// plain array indexing, not a hash map, since this is the O(n^2) hot
    /// path and a `HashMap<usize, _>` lookup per pairwise term was
    /// measured to dominate the whole frame's cost at 1097 nodes (~40ms/
    /// step, see `examples/dev_tune_repulsion.rs`).
    pub(super) fn compute_into(
        members: &[usize],
        positions: &[egui::Pos2],
        group_area: f32,
        disp: &mut [egui::Vec2],
    ) {
        if members.len() < 2 {
            return;
        }
        let n = members.len() as f32;
        let k = (group_area.max(1.0) / n).sqrt() * K_SCALE;
        let k_sq = k * k;
        for i in 0..members.len() {
            for j in (i + 1)..members.len() {
                let (li, lj) = (members[i], members[j]);
                let delta = positions[li] - positions[lj];
                let distance = delta.length().max(DISTANCE_FLOOR);
                let force = C_REPULSE * k_sq / distance;
                let dir = delta / distance;
                disp[li] += dir * force;
                disp[lj] -= dir * force;
            }
        }
    }

    /// Scales and clamps a raw repulsion displacement into a single
    /// frame's position delta (same clamp-to-`max_step` discipline as
    /// `egui_graphs`' FR `apply_displacements`).
    pub(super) fn step(raw: egui::Vec2) -> egui::Vec2 {
        let scaled = raw * STEP_SCALE;
        let len = scaled.length();
        if len > MAX_STEP && len.is_finite() {
            scaled / len * MAX_STEP
        } else {
            scaled
        }
    }
}

/// 28%-of-canvas-width pull-apart separation, capped at 320px -- the direct
/// port of the D3 original's `Math.min(W() * 0.28, 320)`
/// (`frontend/index.html:596-602`, RESEARCH Pattern 4).
pub fn separation(canvas_width: f32) -> f32 {
    (canvas_width * 0.28).min(320.0)
}

/// Target x position for a node in `community`, given the currently
/// focused seam pair (if any). Ported from the D3 `forceX` callback: side
/// `a` -> `center - sep`, side `b` -> `center + sep`, every other
/// community pushed beyond both focused sides (the original's else-branch,
/// margins), no focus -> `center`.
pub fn seam_target_x(
    community: &seam_core::CommunityId,
    focus: Option<(&seam_core::CommunityId, &seam_core::CommunityId)>,
    center_x: f32,
    canvas_width: f32,
) -> f32 {
    let Some((a, b)) = focus else {
        return center_x;
    };
    let sep = separation(canvas_width);
    if community == a {
        center_x - sep
    } else if community == b {
        center_x + sep
    } else {
        // Pushed beyond both focused sides, to the margins -- matches the
        // original's else-branch (RESEARCH Pattern 4).
        center_x + sep * 2.0
    }
}

/// Repulsion group id for a node in `community`, given the currently
/// focused seam pair (if any). Mirrors `seam_target_x`'s branching exactly
/// (same four outcomes: unfocused/center, side A, side B, margin) so every
/// node sharing a `seam_target_x` value also shares a `seam_group` --
/// repulsion (scoped per group, see the `repulsion` module) only ever acts
/// between nodes that would otherwise collapse onto the same target,
/// never across the seam pull-apart's side boundary.
pub fn seam_group(
    community: &seam_core::CommunityId,
    focus: Option<(&seam_core::CommunityId, &seam_core::CommunityId)>,
) -> u8 {
    let Some((a, b)) = focus else {
        return 0; // unfocused -- every node targets center.x
    };
    if community == a {
        1
    } else if community == b {
        2
    } else {
        3 // margin
    }
}

/// Persisted layout state: per-node (keyed by the stable
/// `seam_core::Node::id` -- see the module doc for why NOT by graph index)
/// target x (injected externally each frame by
/// `graph_view::inject_layout_targets`, the only place with
/// `community`/`focus`/`id` data this generic layout can't see) and eased
/// current position (owned entirely by this layout, evolved one easing step
/// per frame), plus the canvas center/dimensions used for vertical
/// centering, first-frame seeding (see `seed_position`), and as the
/// fallback for any node with no target yet.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct SeamLayoutState {
    targets: HashMap<String, f32>,
    /// Per-node repulsion group (see `seam_group`) -- nodes only repel
    /// other nodes sharing the same group id, so the repulsion pass in
    /// `SeamLayout::next` spreads nodes apart locally without pushing a
    /// side-A node toward side B (or vice versa). A node with no entry
    /// (e.g. any caller that never populates this map) defaults to group
    /// `0`, so the whole graph acts as a single group -- correct for the
    /// unfocused default view, where every node's target is `center.x`
    /// anyway.
    groups: HashMap<String, u8>,
    positions: HashMap<String, egui::Pos2>,
    /// This frame's index-to-id translation table -- the ONLY way
    /// `SeamLayout::next` can reach a stable identity, since its trait
    /// bound (`N: Clone`) forbids it from reading any payload field.
    /// Rebuilt from scratch every frame by
    /// `graph_view::inject_layout_targets` precisely because the indices it
    /// maps are not stable; persisted only because
    /// `LayoutState`/`from_state` is the sole channel `egui_graphs`
    /// preserves across frames, never because a previous frame's entries
    /// mean anything.
    id_by_index: HashMap<usize, String>,
    center: egui::Pos2,
    /// Horizontal band (canvas width, roughly) a brand-new node's starting
    /// position is spread across (see `seed_position`).
    band_width: f32,
    /// Vertical band (canvas height, roughly) local jitter is spread
    /// across, so nodes pulled to the same side don't collapse onto one
    /// exact y (see `jitter`).
    band_height: f32,
}

impl LayoutState for SeamLayoutState {}

impl SeamLayoutState {
    /// Injects this frame's id-keyed target-x map, id-keyed repulsion group
    /// map (see `seam_group`), index-to-id translation table, canvas
    /// center, and canvas width/height bands. Called by
    /// `graph_view::inject_layout_targets` via the same
    /// `LayoutState::load`/`save` keys `GraphView`'s own `sync_layout` uses
    /// internally, so the values written here are visible to
    /// `SeamLayout::next` the very same frame.
    ///
    /// All three maps are wholesale replacements, not merges -- every one
    /// of them describes only this frame, and `id_by_index` in particular
    /// would be actively dangerous to accumulate, since a previous frame's
    /// index may now belong to a different node entirely.
    pub fn set_targets(
        &mut self,
        targets: HashMap<String, f32>,
        groups: HashMap<String, u8>,
        id_by_index: HashMap<usize, String>,
        center: egui::Pos2,
        band_width: f32,
        band_height: f32,
    ) {
        self.targets = targets;
        self.groups = groups;
        self.id_by_index = id_by_index;
        self.center = center;
        self.band_width = band_width;
        self.band_height = band_height;
    }

    /// Drops persisted positions for nodes that are no longer in the loaded
    /// model, so a long live session's layout state stays bounded rather
    /// than keeping a position for every node ever seen (T-08-02-03).
    ///
    /// `known_ids` MUST come from the model's own node set, not from the
    /// currently-rendered graph. A node hidden by seam focus is absent from
    /// the rendered graph but very much still loaded; pruning it would
    /// throw away a settled position and re-seed the node the instant focus
    /// cleared. Only a genuine `RemoveNode` should release anything here.
    pub fn retain_positions(&mut self, known_ids: &std::collections::HashSet<String>) {
        self.positions.retain(|id, _| known_ids.contains(id));
    }

    /// Read-only view of persisted node positions, keyed by stable node id
    /// -- used by dev/verification tooling (`examples/dev_snapshot.rs`) to
    /// measure bounding-box spread and overlap at real graph scale without
    /// needing to inspect a rendered image.
    pub fn positions(&self) -> &HashMap<String, egui::Pos2> {
        &self.positions
    }
}

/// FNV-1a over a node id's bytes, folded to 32 bits -- the numeric key the
/// deterministic spread (`jitter`/`seed_position`) consumes, derived from
/// the stable node id rather than from a graph index that moves under
/// mutation.
///
/// Hand-rolled in-module (four lines of arithmetic, no dependency, and
/// deliberately NOT the standard library's `DefaultHasher`): `DefaultHasher`'s
/// output is explicitly not guaranteed stable across toolchain versions, and
/// `SeamLayoutState` derives serialization and can round-trip through
/// `egui`'s persisted memory -- an unstable hash would silently reshuffle
/// every seeded position on a Rust upgrade.
///
/// Folded to 32 bits on purpose. `jitter`'s golden-ratio multiply needs a key
/// small enough that the product keeps real fractional resolution; a raw
/// 64-bit hash overflows that budget outright (its `fract()` would be
/// identically zero). At 32 bits the `f64` product resolves ~1e6 distinct
/// fractions, while a collision between two of a 1097-node graph's ids has
/// probability ~1e-4.
fn id_key(id: &str) -> usize {
    const FNV_OFFSET_BASIS: u32 = 0x811c_9dc5;
    const FNV_PRIME: u32 = 0x0100_0193;
    let mut hash = FNV_OFFSET_BASIS;
    for byte in id.as_bytes() {
        hash ^= u32::from(*byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash as usize
}

/// Deterministic low-discrepancy (golden-ratio) spread for a node keyed by
/// `id_key` of its stable id, within `+/- band/2` of zero. Delegating fully
/// to the built-in Fruchterman-Reingold algorithm for intra-side jitter (as
/// originally scoped) would require integrating a second `Layout`
/// implementation's own state into this one; this simpler deterministic
/// spread avoids every node on a side collapsing onto the exact same
/// point, which is the concrete problem local jitter exists to solve,
/// without that additional integration surface.
///
/// The multiply is done in `f64` (08-02). The sequence and the band are
/// unchanged -- what changed is the key's magnitude: `key` used to be a
/// graph index in `0..~1100`, where an `f32` product left ~14 bits of
/// fractional resolution. A 32-bit id hash pushes the product past `f32`'s
/// 24-bit mantissa, leaving barely 8 bits of fraction and quantising a
/// 1097-node graph's spread onto ~200 distinct values. `f64` restores the
/// resolution the constant was tuned against; it does not retune it.
fn jitter(key: usize, band: f32) -> f32 {
    if band <= 0.0 {
        return 0.0;
    }
    let frac = (key as f64 * 0.618_034).fract() as f32;
    (frac - 0.5) * band
}

/// Salt added to a node's key before seeding its starting *y* so the
/// first-frame x/y seed spread (see `seed_position`) is decorrelated from
/// each other and from the settled-state vertical jitter in `next` (all
/// three otherwise share the same golden-ratio sequence and key space).
const SEED_Y_SALT: usize = 999_983;

/// Starting position for a node that has no persisted `positions` entry
/// yet (its first-ever layout frame). Deliberately spreads new nodes
/// across the full canvas (`band_width` x `band_height`) rather than
/// dropping every node on the exact same point (`center`).
///
/// This matters even though the *settled* "no seam focused" state
/// intentionally collapses every node onto `center.x` (D3,
/// `seam_target_x`'s `None` branch): `egui_graphs` 0.31.0's `GraphView`
/// runs a one-time, unconditional `fit_to_screen` on a widget instance's
/// very first rendered frame (gated on its own internal
/// `MetadataInstance::first_frame_pending`, confirmed by reading
/// `graph_view.rs::handle_fit_to_screen` in the crate source -- this is
/// NOT gated by `SettingsNavigation::fit_to_screen_enabled`, which only
/// controls *continuous* re-fitting every frame, not this one-time
/// first-frame fit; there is no public API to skip it). Whatever the
/// graph's bounding box happens to be on that exact frame gets baked into
/// the persisted zoom permanently, since this app disables continuous
/// re-fit via `with_fit_to_screen_enabled(false)` to avoid the layout's
/// own settle/easing motion fighting the camera. If every new node eases
/// in from one shared starting point (`center`), that first frame's
/// bounds are a near-zero-width sliver regardless of graph size,
/// producing a permanently baked-in double-digit zoom multiplier --
/// empirically confirmed against `sample/graph.json` (1097 nodes) to
/// bake in a fixed `zoom ~= 10.4` that never changes again, rendering as
/// a canvas-filling wall of oversized, overlapping node circles and
/// monospace label glyphs (`SeamNodeShape`/`SeamEdgeShape` both scale
/// radius/font size by `ctx.meta.zoom`). Seeding a full-canvas spread
/// here keeps the first frame's bounds sane; the existing per-frame
/// easing (`EASE_FACTOR`) then smoothly collapses nodes toward `center`
/// exactly as designed, just animated in over the first few frames
/// instead of already collapsed on frame one.
fn seed_position(key: usize, center: egui::Pos2, band_width: f32, band_height: f32) -> egui::Pos2 {
    egui::Pos2::new(
        center.x + jitter(key, band_width),
        center.y + jitter(key.wrapping_add(SEED_Y_SALT), band_height),
    )
}

/// Custom `Layout`: assigns each node its `seam_target_x` (injected via
/// `SeamLayoutState`) and eases toward it across frames. Vertical
/// positioning stays centered, matching the original's weak
/// center-seeking vertical force (`d3.forceY(H()/2).strength(.05)`).
#[derive(Debug, Default)]
pub struct SeamLayout {
    state: SeamLayoutState,
}

impl Layout<SeamLayoutState> for SeamLayout {
    fn from_state(state: SeamLayoutState) -> impl Layout<SeamLayoutState> {
        Self { state }
    }

    fn next<N, E, Ty, Ix, Dn, De>(&mut self, g: &mut Graph<N, E, Ty, Ix, Dn, De>, _ui: &egui::Ui)
    where
        N: Clone,
        E: Clone,
        Ty: EdgeType,
        Ix: IndexType,
        Dn: DisplayNode<N, E, Ty, Ix>,
        De: DisplayEdge<N, E, Ty, Ix, Dn>,
    {
        let indices: Vec<_> = g.g().node_indices().collect();
        let n = indices.len();

        // Translate each graph index to the stable node id it carries THIS
        // frame, via the table `graph_view::inject_layout_targets` rebuilt
        // from the very same graph moments ago. Every lookup and write-back
        // below is by that id, never by the index (see the module doc).
        let keys: Vec<String> = indices
            .iter()
            .map(|idx| {
                self.state
                    .id_by_index
                    .get(&idx.index())
                    .cloned()
                    // DEFENSIVE, not an expected path: the table is rebuilt
                    // every frame from the same graph this loop iterates, so
                    // a missing entry means the injector was skipped
                    // entirely (a caller driving `next` directly, e.g. a
                    // dev example). A deterministic synthetic key keeps such
                    // a node's position persisted frame to frame instead of
                    // re-seeding it every single frame; it is deliberately
                    // namespaced so it can never collide with a real id.
                    .unwrap_or_else(|| format!("\u{0}idx:{}", idx.index()))
            })
            .collect();

        // Resolve every node's *current* position (existing or seeded) and
        // group id into plain `Vec`s aligned 1:1 with `keys` -- these O(n)
        // `HashMap` lookups (against the small, sparse `positions`/
        // `groups`/`targets` maps) are fine; only the O(n^2) repulsion pass
        // below needs to avoid hashing (see `repulsion::compute_into`'s
        // doc comment).
        let mut current: Vec<egui::Pos2> = Vec::with_capacity(n);
        let mut group_of: Vec<u8> = Vec::with_capacity(n);
        for key in &keys {
            let pos = self.state.positions.get(key).copied().unwrap_or_else(|| {
                seed_position(
                    id_key(key),
                    self.state.center,
                    self.state.band_width,
                    self.state.band_height,
                )
            });
            current.push(pos);
            group_of.push(self.state.groups.get(key).copied().unwrap_or(0));
        }

        // Bucket *local* indices (0..n) by group id -- confined within
        // each side group (unfocused/center, side A, side B, or margin --
        // see `seam_group`) so repulsion spreads nodes apart locally
        // without fighting the x-target/side-separation pull across
        // groups. At most 4 distinct group ids occur in practice, so this
        // bucketing pass is cheap regardless of `n`.
        let mut buckets: HashMap<u8, Vec<usize>> = HashMap::new();
        for (local_idx, &group) in group_of.iter().enumerate() {
            buckets.entry(group).or_default().push(local_idx);
        }
        let group_area = (self.state.band_width * self.state.band_height).max(1.0);
        let mut repulsion_disp = vec![egui::Vec2::ZERO; n];
        for members in buckets.values() {
            repulsion::compute_into(members, &current, group_area, &mut repulsion_disp);
        }

        for (local_idx, idx) in indices.into_iter().enumerate() {
            let key = &keys[local_idx];
            let target_x = self
                .state
                .targets
                .get(key)
                .copied()
                .unwrap_or(self.state.center.x);
            let target_y = self.state.center.y + jitter(id_key(key), self.state.band_height * 0.6);
            let target = egui::Pos2::new(target_x, target_y);
            let here = current[local_idx];
            let step = (target - here) * EASE_FACTOR + repulsion::step(repulsion_disp[local_idx]);
            let eased = here + step;
            self.state.positions.insert(key.clone(), eased);
            if let Some(node) = g.node_mut(idx) {
                node.set_location(eased);
            }
        }
    }

    fn state(&self) -> SeamLayoutState {
        self.state.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stable, made-up node id for the `i`-th node of a synthetic
    /// fixture graph. Real ids are opaque strings from `graph.json`, so
    /// these deliberately are too -- a test keying its fixtures by a
    /// stringified index would still be exercising an index key by another
    /// name.
    fn synthetic_id(i: usize) -> String {
        format!("n{i}")
    }

    /// The per-frame index-to-id translation table for a synthetic
    /// `n`-node fixture graph whose index `i` carries `synthetic_id(i)`.
    fn synthetic_table(n: usize) -> HashMap<usize, String> {
        (0..n).map(|i| (i, synthetic_id(i))).collect()
    }

    #[test]
    fn test_separation_formula() {
        assert_eq!(separation(1000.0), 280.0);
        assert_eq!(separation(2000.0), 320.0);
        assert_eq!(separation(400.0), 112.0);
    }

    #[test]
    fn test_seam_target_x_pulls_sides_apart() {
        let a: seam_core::CommunityId = "A".to_string();
        let b: seam_core::CommunityId = "B".to_string();
        let center = 500.0;
        let width = 1000.0;
        let sep = separation(width);

        let target_a = seam_target_x(&a, Some((&a, &b)), center, width);
        let target_b = seam_target_x(&b, Some((&a, &b)), center, width);

        assert_eq!(target_a, center - sep);
        assert_eq!(target_b, center + sep);
        assert_eq!(target_b - target_a, 2.0 * sep);
    }

    #[test]
    fn test_seam_target_x_pushes_others_to_margins() {
        let a: seam_core::CommunityId = "A".to_string();
        let b: seam_core::CommunityId = "B".to_string();
        let c: seam_core::CommunityId = "C".to_string();
        let center = 500.0;
        let width = 1000.0;
        let sep = separation(width);

        let target_c = seam_target_x(&c, Some((&a, &b)), center, width);

        assert!(
            (target_c - center).abs() > sep,
            "community outside the focused pair must target a position beyond both focused sides"
        );
    }

    #[test]
    fn test_no_focus_targets_center() {
        let a: seam_core::CommunityId = "A".to_string();
        let b: seam_core::CommunityId = "B".to_string();
        let c: seam_core::CommunityId = "C".to_string();
        let center = 500.0;
        let width = 1000.0;

        assert_eq!(seam_target_x(&a, None, center, width), center);
        assert_eq!(seam_target_x(&b, None, center, width), center);
        assert_eq!(seam_target_x(&c, None, center, width), center);
    }

    /// `seam_group` must partition nodes identically to `seam_target_x` --
    /// every node sharing a `seam_target_x` value (which is what would
    /// otherwise collapse onto that shared point, absent repulsion) must
    /// also share a `seam_group`, and nodes with different targets must
    /// get different groups, so repulsion never crosses the seam
    /// pull-apart's side boundary.
    #[test]
    fn test_seam_group_partitions_like_seam_target_x() {
        let a: seam_core::CommunityId = "A".to_string();
        let b: seam_core::CommunityId = "B".to_string();
        let c: seam_core::CommunityId = "C".to_string();

        // Unfocused: every community shares one group, matching
        // `seam_target_x`'s shared `center_x` target.
        let group_a_unfocused = seam_group(&a, None);
        assert_eq!(group_a_unfocused, seam_group(&b, None));
        assert_eq!(group_a_unfocused, seam_group(&c, None));

        // Focused: side A, side B, and margin communities must each get a
        // distinct group, mirroring `seam_target_x`'s three distinct
        // outcomes for a focused pair.
        let focus = Some((&a, &b));
        let group_a = seam_group(&a, focus);
        let group_b = seam_group(&b, focus);
        let group_c = seam_group(&c, focus);
        assert_ne!(group_a, group_b);
        assert_ne!(group_a, group_c);
        assert_ne!(group_b, group_c);

        // A second "other" community must share the margin group with the
        // first -- both get the same `seam_target_x` (`center + sep*2`).
        let d: seam_core::CommunityId = "D".to_string();
        assert_eq!(group_c, seam_group(&d, focus));
    }

    /// Regression test for the huge/glitched-canvas rendering bug: a brand
    /// new node's *starting* position (before any easing) must not all
    /// collapse onto the exact same `center` point, or `egui_graphs`
    /// 0.31.0's unconditional one-time first-frame `fit_to_screen` bakes a
    /// near-zero-width graph bounding box into a permanently huge zoom
    /// (see `seed_position`'s doc comment for the full mechanism,
    /// empirically confirmed against `sample/graph.json`).
    #[test]
    fn test_seed_position_spreads_new_nodes_across_the_canvas() {
        let center = egui::Pos2::new(600.0, 400.0);
        let band_width = 1200.0;
        let band_height = 800.0;

        let seeds: Vec<egui::Pos2> = (0..40)
            .map(|i| seed_position(id_key(&format!("n{i}")), center, band_width, band_height))
            .collect();

        let xs = seeds.iter().map(|p| p.x);
        let min_x = xs.clone().fold(f32::MAX, f32::min);
        let max_x = xs.fold(f32::MIN, f32::max);
        assert!(
            max_x - min_x > band_width * 0.5,
            "seeded x positions must spread across most of band_width, got spread {} \
             (min {min_x}, max {max_x})",
            max_x - min_x
        );

        let ys = seeds.iter().map(|p| p.y);
        let min_y = ys.clone().fold(f32::MAX, f32::min);
        let max_y = ys.fold(f32::MIN, f32::max);
        assert!(
            max_y - min_y > band_height * 0.5,
            "seeded y positions must spread across most of band_height, got spread {} \
             (min {min_y}, max {max_y})",
            max_y - min_y
        );
    }

    /// `seed_position` must be deterministic (same key -> same seed every
    /// call) -- `SeamLayout::next` relies on this to only need the
    /// `positions` map, never re-deriving a stale seed inconsistently.
    #[test]
    fn test_seed_position_is_deterministic() {
        let center = egui::Pos2::new(600.0, 400.0);
        let a = seed_position(id_key("seven"), center, 1200.0, 800.0);
        let b = seed_position(id_key("seven"), center, 1200.0, 800.0);
        assert_eq!(a, b);
    }

    /// `SeamLayout::next`'s first-ever step for a fresh graph (empty
    /// `targets`/`positions`, matching the real "no seam focused yet"
    /// load state where every target defaults to `center.x`) must still
    /// produce a spread-out set of node positions, not a collapsed
    /// sliver -- this is the exact scenario that baked in a permanent
    /// ~10x zoom against `sample/graph.json`.
    #[test]
    fn test_first_layout_step_does_not_collapse_new_nodes() {
        use egui_graphs::{DefaultEdgeShape, DefaultNodeShape};
        use petgraph::stable_graph::{DefaultIx, StableGraph};
        use petgraph::Directed;

        const N: usize = 40;
        let mut g: Graph<(), (), Directed, DefaultIx, DefaultNodeShape, DefaultEdgeShape> =
            Graph::new(StableGraph::default());
        for _ in 0..N {
            g.add_node(());
        }

        let center = egui::Pos2::new(600.0, 400.0);
        let mut state = SeamLayoutState::default();
        // Empty targets map -> every node's target_x falls back to
        // `center.x` (via the `unwrap_or(self.state.center.x)` in
        // `next`), matching real usage when `app.focus` is `None`.
        state.set_targets(
            HashMap::new(),
            HashMap::new(),
            synthetic_table(N),
            center,
            1200.0,
            800.0,
        );

        let mut layout = SeamLayout { state };
        let ctx = egui::Context::default();
        egui_kittest_free_step(&ctx, |ui| layout.next(&mut g, ui));

        let xs: Vec<f32> = g
            .g()
            .node_indices()
            .map(|i| g.node(i).unwrap().location().x)
            .collect();
        let min_x = xs.iter().cloned().fold(f32::MAX, f32::min);
        let max_x = xs.iter().cloned().fold(f32::MIN, f32::max);
        assert!(
            max_x - min_x > 50.0,
            "first-frame node x spread must not collapse to a sliver, got {} \
             (this is exactly what makes egui_graphs' one-time first-frame \
             fit_to_screen bake in a huge permanent zoom)",
            max_x - min_x
        );
    }

    /// Minimal helper to obtain a real `egui::Ui` for exercising
    /// `Layout::next` (whose trait signature requires one, even though
    /// `SeamLayout::next` never reads it) without pulling `egui_kittest`
    /// into this crate's non-dev dependency graph.
    fn egui_kittest_free_step(ctx: &egui::Context, f: impl FnMut(&mut egui::Ui)) {
        let raw_input = egui::RawInput::default();
        let _ = ctx.run_ui(raw_input, f);
    }

    /// Regression test for the real-graph collapse bug: with no inter-node
    /// force, every node whose target_x is identical (the unfocused
    /// default-view case -- `sample/graph.json`'s 1097 nodes, all easing
    /// toward the same `center.x`) must eventually collapse in x, since
    /// nothing pushes them apart. Runs enough steps for `EASE_FACTOR`
    /// (0.12/frame) to fully settle toward the shared target.
    #[test]
    fn test_repulsion_prevents_collapse_at_scale() {
        use egui_graphs::{DefaultEdgeShape, DefaultNodeShape};
        use petgraph::stable_graph::{DefaultIx, StableGraph};
        use petgraph::Directed;

        // N matches the real `sample/graph.json`'s node count exactly --
        // this is the literal scale the bug was reported against.
        const N: usize = 1097;
        let mut g: Graph<(), (), Directed, DefaultIx, DefaultNodeShape, DefaultEdgeShape> =
            Graph::new(StableGraph::default());
        for _ in 0..N {
            g.add_node(());
        }

        let center = egui::Pos2::new(600.0, 400.0);
        let band_width = 1200.0;
        let band_height = 800.0;
        let mut state = SeamLayoutState::default();
        // Empty targets map -> every node's target_x falls back to
        // `center.x` (the real unfocused-view scenario).
        state.set_targets(
            HashMap::new(),
            HashMap::new(),
            synthetic_table(N),
            center,
            band_width,
            band_height,
        );

        let mut layout = SeamLayout { state };
        let ctx = egui::Context::default();
        // Empirically converged well before this by ~step 150-200 (see
        // examples/dev_tune_repulsion.rs); 250 leaves margin.
        for _ in 0..250 {
            egui_kittest_free_step(&ctx, |ui| layout.next(&mut g, ui));
        }

        let positions: Vec<egui::Pos2> = g
            .g()
            .node_indices()
            .map(|i| g.node(i).unwrap().location())
            .collect();
        let xs = positions.iter().map(|p| p.x);
        let min_x = xs.clone().fold(f32::MAX, f32::min);
        let max_x = xs.fold(f32::MIN, f32::max);
        assert!(
            max_x - min_x > 100.0,
            "settled node x spread collapsed to {} -- nodes have no repulsion \
             pushing them apart, which is exactly the overlapping-column bug \
             seen against the real 1097-node sample/graph.json",
            max_x - min_x
        );

        // Circle diameter is 6.0 (NODE_RADIUS) * 2 = 12.0
        // (`graph_view::NODE_RADIUS`, not imported here to keep this test
        // decoupled from the render module -- literal threshold instead).
        // A settled min pairwise distance at or below that means node
        // circles are literally overlapping, not just "densely packed" --
        // the exact visual symptom reported.
        let mut min_pairwise_dist = f32::MAX;
        for i in 0..positions.len() {
            for j in (i + 1)..positions.len() {
                min_pairwise_dist = min_pairwise_dist.min((positions[i] - positions[j]).length());
            }
        }
        assert!(
            min_pairwise_dist > 12.0,
            "settled min pairwise node distance is {min_pairwise_dist} -- node \
             circles (12px diameter) are still overlapping at real graph.json scale"
        );
    }

    // ========================================================
    // Plan 08-02: the rekey from `NodeIndex::index()` to the stable
    // `seam_core::Node::id`. See this module's doc comment for the two
    // mechanisms that make an index key wrong from Phase 8 onward.
    // ========================================================

    /// The canvas geometry every 08-02 test below shares.
    const T_CENTER: egui::Pos2 = egui::Pos2::new(600.0, 400.0);
    const T_BAND_W: f32 = 1200.0;
    const T_BAND_H: f32 = 800.0;

    /// Builds the per-frame index-to-id translation table
    /// `graph_view::inject_layout_targets` builds for real, from a slice of
    /// ids listed in graph-index order.
    fn table(ids: &[&str]) -> HashMap<usize, String> {
        ids.iter()
            .enumerate()
            .map(|(i, id)| (i, (*id).to_string()))
            .collect()
    }

    /// Runs exactly one `SeamLayout::next` step over a fresh `N`-node graph
    /// whose index `i` carries `ids[i]`, from a fresh (empty) layout state,
    /// and returns the resulting id-keyed persisted positions. The
    /// deterministic-spread tests use this to compare what a given *id*
    /// seeds to, independent of which index happened to carry it.
    fn settle_one_step(ids: &[&str]) -> HashMap<String, egui::Pos2> {
        use egui_graphs::{DefaultEdgeShape, DefaultNodeShape};
        use petgraph::stable_graph::{DefaultIx, StableGraph};
        use petgraph::Directed;

        let mut g: Graph<(), (), Directed, DefaultIx, DefaultNodeShape, DefaultEdgeShape> =
            Graph::new(StableGraph::default());
        for _ in ids {
            g.add_node(());
        }

        let mut state = SeamLayoutState::default();
        state.set_targets(
            HashMap::new(),
            HashMap::new(),
            table(ids),
            T_CENTER,
            T_BAND_W,
            T_BAND_H,
        );

        let mut layout = SeamLayout { state };
        let ctx = egui::Context::default();
        egui_kittest_free_step(&ctx, |ui| layout.next(&mut g, ui));
        layout.state.positions().clone()
    }

    /// The confirmed `petgraph` 0.8.3 free-list hazard, exercised as the
    /// real sequence rather than paraphrased: remove a node, add a
    /// *different* one, and watch the new node be handed the removed node's
    /// slot. The removed node's persisted position must NOT come with it.
    ///
    /// The premise is asserted, not assumed: if a future `petgraph` release
    /// changes its free-list policy and the newly added node stops
    /// inheriting the removed index, this test fails loudly on the premise
    /// assertion rather than passing vacuously -- at which point it must be
    /// redesigned around whatever the new policy is, not deleted.
    #[test]
    fn a_recycled_graph_index_does_not_inherit_the_removed_nodes_position() {
        use egui_graphs::{DefaultEdgeShape, DefaultNodeShape};
        use petgraph::stable_graph::{DefaultIx, StableGraph};
        use petgraph::Directed;

        let mut g: Graph<(), (), Directed, DefaultIx, DefaultNodeShape, DefaultEdgeShape> =
            Graph::new(StableGraph::default());
        let survivor = g.add_node(());
        let doomed = g.add_node(());

        let mut state = SeamLayoutState::default();
        state.set_targets(
            HashMap::new(),
            HashMap::new(),
            HashMap::from([
                (survivor.index(), "survivor".to_string()),
                (doomed.index(), "doomed".to_string()),
            ]),
            T_CENTER,
            T_BAND_W,
            T_BAND_H,
        );
        // Park the doomed node far outside the seeding band on purpose:
        // "inherited the departed node's position" and "freshly seeded" are
        // then thousands of pixels apart, so neither assertion below needs
        // an epsilon judgement call.
        state
            .positions
            .insert("doomed".to_string(), egui::Pos2::new(5000.0, 5000.0));

        let mut layout = SeamLayout { state };
        let ctx = egui::Context::default();
        egui_kittest_free_step(&ctx, |ui| layout.next(&mut g, ui));

        let departed = layout.state.positions()["doomed"];
        assert!(
            (departed - T_CENTER).length() > 4000.0,
            "fixture precondition: the doomed node must still be parked far \
             outside the seeding band after one easing step, got {departed:?}"
        );

        g.remove_node(doomed);
        let newcomer = g.add_node(());
        assert_eq!(
            newcomer.index(),
            doomed.index(),
            "PREMISE: petgraph's StableGraph free list must hand the removed \
             node's index straight back to the next insertion -- if this ever \
             stops holding, the retarget scenario below is no longer \
             reproducible and this test must be redesigned, not deleted"
        );

        // Exactly what `inject_layout_targets` does every frame: rebuild the
        // translation table from scratch against the graph as it now stands.
        layout.state.set_targets(
            HashMap::new(),
            HashMap::new(),
            HashMap::from([
                (survivor.index(), "survivor".to_string()),
                (newcomer.index(), "newcomer".to_string()),
            ]),
            T_CENTER,
            T_BAND_W,
            T_BAND_H,
        );
        egui_kittest_free_step(&ctx, |ui| layout.next(&mut g, ui));

        let landed = layout.state.positions()["newcomer"];
        assert!(
            (landed - departed).length() > 1000.0,
            "the node added into the recycled index landed at {landed:?}, one \
             easing step away from the REMOVED node's persisted {departed:?} -- \
             a persisted position silently retargeted onto a completely \
             different node"
        );
        assert!(
            (landed - T_CENTER).length() < 1000.0,
            "the newly added node must be freshly seeded inside the \
             {T_BAND_W}x{T_BAND_H} canvas band around {T_CENTER:?}, got {landed:?}"
        );
    }

    /// The persisted maps must be keyed by the stable node id, not by
    /// whatever index happened to carry that id this frame. Proven by
    /// rendering the SAME graph twice with two translation tables that
    /// disagree about every single index: the first frame's ids must still
    /// hold exactly the positions they held, untouched by anything the
    /// second frame did under different ids.
    #[test]
    fn a_nodes_persisted_position_is_keyed_by_its_stable_id() {
        use egui_graphs::{DefaultEdgeShape, DefaultNodeShape};
        use petgraph::stable_graph::{DefaultIx, StableGraph};
        use petgraph::Directed;

        const FIRST: [&str; 3] = ["alpha", "beta", "gamma"];
        const SECOND: [&str; 3] = ["delta", "epsilon", "zeta"];

        let mut g: Graph<(), (), Directed, DefaultIx, DefaultNodeShape, DefaultEdgeShape> =
            Graph::new(StableGraph::default());
        for _ in 0..FIRST.len() {
            g.add_node(());
        }

        let mut state = SeamLayoutState::default();
        state.set_targets(
            HashMap::new(),
            HashMap::new(),
            table(&FIRST),
            T_CENTER,
            T_BAND_W,
            T_BAND_H,
        );
        let mut layout = SeamLayout { state };
        let ctx = egui::Context::default();
        egui_kittest_free_step(&ctx, |ui| layout.next(&mut g, ui));

        let after_first: Vec<egui::Pos2> = FIRST
            .iter()
            .map(|id| {
                *layout
                    .state
                    .positions()
                    .get(*id)
                    .unwrap_or_else(|| panic!("frame one must persist a position for {id}"))
            })
            .collect();

        // Same graph, same indices -- every index now claims a different id.
        layout.state.set_targets(
            HashMap::new(),
            HashMap::new(),
            table(&SECOND),
            T_CENTER,
            T_BAND_W,
            T_BAND_H,
        );
        egui_kittest_free_step(&ctx, |ui| layout.next(&mut g, ui));

        for (id, before) in FIRST.iter().zip(after_first) {
            let now = layout.state.positions()[*id];
            assert_eq!(
                now, before,
                "`{id}`'s persisted position moved from {before:?} to {now:?} across a \
                 frame in which no node claimed that id -- the map is keyed by index, \
                 not by the stable node id"
            );
        }
        for id in SECOND {
            assert!(
                layout.state.positions().contains_key(id),
                "the second frame's node `{id}` must get its own persisted entry, \
                 not overwrite whatever sat at its index"
            );
        }
        assert_eq!(
            layout.state.positions().len(),
            FIRST.len() + SECOND.len(),
            "six distinct ids were rendered across the two frames, so six distinct \
             persisted positions must exist"
        );
    }

    /// The deterministic anti-collapse spread (`seed_position`, whose whole
    /// reason for existing is documented on its own doc comment) must be
    /// derived from the stable id, not from the graph index. Otherwise the
    /// dense per-frame renumbering in `build_graph` reshuffles every seed
    /// the moment a single node is added or removed.
    #[test]
    fn the_deterministic_spread_is_derived_from_the_id_not_the_index() {
        // (a) Same index, different id -> different seed. Single-node
        // graphs, so no repulsion term can muddy the comparison.
        let alpha_at_0 = settle_one_step(&["alpha"])["alpha"];
        let beta_at_0 = settle_one_step(&["beta"])["beta"];
        assert!(
            (alpha_at_0 - beta_at_0).length() > 10.0,
            "two different ids at the SAME index seeded to {alpha_at_0:?} and \
             {beta_at_0:?} -- the spread is keyed by the index, so every node \
             would re-seed identically after a renumbering"
        );

        // (b) Same id at a different index -> the same seed. This is what
        // survives `build_graph`'s dense renumbering.
        let alpha_first = settle_one_step(&["alpha", "other", "third"])["alpha"];
        let alpha_last = settle_one_step(&["third", "other", "alpha"])["alpha"];
        assert_eq!(
            alpha_first, alpha_last,
            "`alpha` seeded to {alpha_first:?} at index 0 but {alpha_last:?} at \
             index 2 -- the spread must follow the id across a renumbering"
        );

        // (c) Byte-identical across runs, which is what makes the spread
        // safe to persist and reload (the state derives serialization).
        assert_eq!(
            settle_one_step(&["alpha"])["alpha"],
            alpha_at_0,
            "the id-derived spread must be byte-identical across runs"
        );
    }

    /// Repulsion must be scoped per group (`seam_group`) -- side-A nodes
    /// must never repel side-B nodes, or repulsion would fight the seam
    /// pull-apart's whole purpose (fix_direction: "constrained to run
    /// WITHIN each side's node subset"). Proven by comparing group-1's
    /// settled positions with and without an overlapping group-2 present:
    /// if repulsion leaks across groups, group-2's presence changes
    /// group-1's trajectory; if correctly scoped, group-1's trajectory is
    /// identical either way (group-2 contributes zero force to it).
    #[test]
    fn test_repulsion_is_scoped_per_group() {
        use egui_graphs::{DefaultEdgeShape, DefaultNodeShape};
        use petgraph::stable_graph::{DefaultIx, StableGraph};
        use petgraph::Directed;

        const GROUP_SIZE: usize = 10;
        let center = egui::Pos2::new(600.0, 400.0);
        let band_width = 1200.0;
        let band_height = 800.0;

        fn run(
            total_nodes: usize,
            center: egui::Pos2,
            band_width: f32,
            band_height: f32,
        ) -> Vec<egui::Pos2> {
            let mut g: Graph<(), (), Directed, DefaultIx, DefaultNodeShape, DefaultEdgeShape> =
                Graph::new(StableGraph::default());
            for _ in 0..total_nodes {
                g.add_node(());
            }

            let mut targets = HashMap::new();
            let mut groups = HashMap::new();
            let mut positions = HashMap::new();
            for i in 0..GROUP_SIZE {
                let key = synthetic_id(i);
                targets.insert(key.clone(), center.x - 250.0); // side A target
                groups.insert(key.clone(), 1u8);
                // Deliberately overlapping start positions -- maximizes
                // any erroneous cross-group force if scoping leaks.
                positions.insert(key, egui::Pos2::new(500.0, 300.0 + i as f32));
            }
            for i in GROUP_SIZE..total_nodes {
                let key = synthetic_id(i);
                targets.insert(key.clone(), center.x + 250.0); // side B target
                groups.insert(key.clone(), 2u8);
                let a_key = i - GROUP_SIZE;
                positions.insert(key, egui::Pos2::new(500.0, 300.0 + a_key as f32));
            }

            let mut state = SeamLayoutState::default();
            state.set_targets(
                targets,
                groups,
                synthetic_table(total_nodes),
                center,
                band_width,
                band_height,
            );
            state.positions = positions;

            let mut layout = SeamLayout { state };
            let ctx = egui::Context::default();
            for _ in 0..10 {
                egui_kittest_free_step(&ctx, |ui| layout.next(&mut g, ui));
            }

            (0..GROUP_SIZE)
                .map(|i| {
                    g.node(petgraph::stable_graph::NodeIndex::<DefaultIx>::new(i))
                        .unwrap()
                        .location()
                })
                .collect()
        }

        let group_a_alone = run(GROUP_SIZE, center, band_width, band_height);
        let group_a_with_b = run(GROUP_SIZE * 2, center, band_width, band_height);

        for i in 0..GROUP_SIZE {
            let alone = group_a_alone[i];
            let with_b = group_a_with_b[i];
            assert!(
                (alone.x - with_b.x).abs() < 0.01 && (alone.y - with_b.y).abs() < 0.01,
                "group-1 node {i} settled at {alone:?} alone but {with_b:?} with an \
                 overlapping group-2 present -- repulsion is leaking across groups, \
                 which would fight the seam pull-apart's side separation"
            );
        }
    }
}
