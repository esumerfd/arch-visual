//! Custom `egui_graphs::Layout`: seam pull-apart positioning (D-13,
//! RESEARCH Pattern 4). `egui_graphs`'s built-in layouts are
//! Fruchterman-Reingold (position-based); the D3 original used d3-force
//! (velocity/strength-based) -- there is no direct translation, so this
//! ports the **target positions** (`cx +/- min(W*0.28, 320)`), not the
//! force-simulation mechanics (RESEARCH Pitfall 5).
//!
//! `Layout::next<N, E, Ty, Ix, Dn, De>` is itself generic over the node
//! payload type `N` (bound only by `Clone`), so it cannot read a node's
//! `community` directly -- that data lives in `graph_view::PayloadNode`, a
//! concrete type this module deliberately does not depend on. Instead,
//! `graph_view::apply_focus_styling` (Task 1) computes each node's target x
//! (via `seam_target_x` below) and injects it into this module's persisted
//! `SeamLayoutState` (keyed by `NodeIndex::index()`, stable across frames
//! since the graph is rebuilt identically from the same immutable `Model`
//! every frame) immediately before the `GraphView` widget's own
//! `sync_layout` reads it. `SeamLayout::next` then only has to ease each
//! node toward the target already sitting in its own state -- no payload
//! type needed.

use egui_graphs::{DisplayEdge, DisplayNode, Graph, Layout, LayoutState};
use petgraph::stable_graph::IndexType;
use petgraph::EdgeType;
use std::collections::HashMap;

/// Fraction of the remaining distance to the target closed per frame --
/// tuned so the pull-apart reads as a deliberate motion rather than a snap
/// (Task 2 action: "Ease nodes toward their targets across frames").
const EASE_FACTOR: f32 = 0.12;

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

/// Persisted layout state: per-node (keyed by `NodeIndex::index()`) target
/// x (injected externally each frame by `graph_view::apply_focus_styling`,
/// the only place with `community`/`focus` data this generic layout can't
/// see) and eased current position (owned entirely by this layout,
/// evolved one easing step per frame), plus the canvas center/dimensions
/// used for vertical centering, first-frame seeding (see `seed_position`),
/// and as the fallback for any node with no target yet.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct SeamLayoutState {
    targets: HashMap<usize, f32>,
    positions: HashMap<usize, egui::Pos2>,
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
    /// Injects this frame's per-node target-x map, canvas center, and
    /// canvas width/height bands. Called by
    /// `graph_view::inject_layout_targets` via the same
    /// `LayoutState::load`/`save` keys `GraphView`'s own `sync_layout` uses
    /// internally, so the values written here are visible to
    /// `SeamLayout::next` the very same frame.
    pub fn set_targets(
        &mut self,
        targets: HashMap<usize, f32>,
        center: egui::Pos2,
        band_width: f32,
        band_height: f32,
    ) {
        self.targets = targets;
        self.center = center;
        self.band_width = band_width;
        self.band_height = band_height;
    }
}

/// Deterministic low-discrepancy (golden-ratio) spread for a node keyed by
/// its stable index, within `+/- band/2` of zero. Delegating fully to the
/// built-in Fruchterman-Reingold algorithm for intra-side jitter (as
/// originally scoped) would require integrating a second `Layout`
/// implementation's own state into this one; this simpler deterministic
/// spread avoids every node on a side collapsing onto the exact same
/// point, which is the concrete problem local jitter exists to solve,
/// without that additional integration surface.
fn jitter(key: usize, band: f32) -> f32 {
    if band <= 0.0 {
        return 0.0;
    }
    let frac = (key as f32 * 0.618_034).fract();
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
        for idx in indices {
            let key = idx.index();
            let target_x = self
                .state
                .targets
                .get(&key)
                .copied()
                .unwrap_or(self.state.center.x);
            let current = self.state.positions.get(&key).copied().unwrap_or_else(|| {
                seed_position(
                    key,
                    self.state.center,
                    self.state.band_width,
                    self.state.band_height,
                )
            });
            let target_y = self.state.center.y + jitter(key, self.state.band_height * 0.6);
            let target = egui::Pos2::new(target_x, target_y);
            let eased = current + (target - current) * EASE_FACTOR;
            self.state.positions.insert(key, eased);
            if let Some(n) = g.node_mut(idx) {
                n.set_location(eased);
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
            .map(|key| seed_position(key, center, band_width, band_height))
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
        let a = seed_position(7, center, 1200.0, 800.0);
        let b = seed_position(7, center, 1200.0, 800.0);
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

        let mut g: Graph<(), (), Directed, DefaultIx, DefaultNodeShape, DefaultEdgeShape> =
            Graph::new(StableGraph::default());
        for _ in 0..40 {
            g.add_node(());
        }

        let center = egui::Pos2::new(600.0, 400.0);
        let mut state = SeamLayoutState::default();
        // Empty targets map -> every node's target_x falls back to
        // `center.x` (via the `unwrap_or(self.state.center.x)` in
        // `next`), matching real usage when `app.focus` is `None`.
        state.set_targets(HashMap::new(), center, 1200.0, 800.0);

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
}
