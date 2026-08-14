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
/// evolved one easing step per frame), plus the canvas center used for
/// vertical centering and as the fallback for any node with no target yet.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct SeamLayoutState {
    targets: HashMap<usize, f32>,
    positions: HashMap<usize, egui::Pos2>,
    center: egui::Pos2,
    /// Vertical band (canvas height, roughly) local jitter is spread
    /// across, so nodes pulled to the same side don't collapse onto one
    /// exact y (see `vertical_jitter`).
    band_height: f32,
}

impl LayoutState for SeamLayoutState {}

impl SeamLayoutState {
    /// Injects this frame's per-node target-x map, canvas center, and
    /// vertical band height. Called by
    /// `graph_view::inject_layout_targets` via the same
    /// `LayoutState::load`/`save` keys `GraphView`'s own `sync_layout` uses
    /// internally, so the values written here are visible to
    /// `SeamLayout::next` the very same frame.
    pub fn set_targets(
        &mut self,
        targets: HashMap<usize, f32>,
        center: egui::Pos2,
        band_height: f32,
    ) {
        self.targets = targets;
        self.center = center;
        self.band_height = band_height;
    }
}

/// Deterministic low-discrepancy (golden-ratio) vertical spread for a node
/// keyed by its stable index, within `+/- band_height/2` of center.
/// Delegating fully to the built-in Fruchterman-Reingold algorithm for
/// intra-side jitter (as originally scoped) would require integrating a
/// second `Layout` implementation's own state into this one; this simpler
/// deterministic spread avoids every node on a side collapsing onto the
/// exact same point, which is the concrete problem local jitter exists to
/// solve, without that additional integration surface.
fn vertical_jitter(key: usize, band_height: f32) -> f32 {
    if band_height <= 0.0 {
        return 0.0;
    }
    let frac = (key as f32 * 0.618_034).fract();
    (frac - 0.5) * band_height
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
            let current = self
                .state
                .positions
                .get(&key)
                .copied()
                .unwrap_or(self.state.center);
            let target_y = self.state.center.y + vertical_jitter(key, self.state.band_height * 0.6);
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
}
