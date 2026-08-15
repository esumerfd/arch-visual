//! `CentralPanel` graph canvas: the `egui_graphs`-backed rendering of the
//! whole graph (NAV-02/03/04). Custom `DisplayNode`/`DisplayEdge`
//! implementations (`SeamNodeShape`/`SeamEdgeShape`) express focus-mode
//! dimming, side tinting, bridge-node stroke, directed arrowheads, and
//! focus-scoped per-direction crossing-count labels. The seam pull-apart
//! `Layout` (D-13) is wired in from `layout.rs`; the fault-line overlay
//! (D-13) is drawn on top from `overlay.rs`.
//!
//! `egui_graphs` 0.31.0's actual `DisplayNode`/`DisplayEdge`/`Layout`/
//! `GraphView` API was read directly from
//! `~/.cargo/registry/src/*/egui_graphs-0.31.0/src/` before writing this
//! file (RESEARCH.md Assumption A1, now closed) -- notably: `DisplayNode`/
//! `DisplayEdge::shapes` only ever see `NodeProps`/`EdgeProps` (payload plus
//! a handful of built-in fields: `color`, `selected`, `dragged`, `hovered`,
//! `label`, `order`), never arbitrary app state. So all focus-driven
//! rendering (opacity, side tint, bridge stroke, crossing-count text) is
//! computed once per frame in `apply_focus_styling` from `app.focus`/
//! `app.detail`, and baked into those built-in slots (`set_color`,
//! `set_label`) or into the custom `is_bridge`/`dim` fields via
//! `display_mut()`, before the `GraphView` widget draws.
//!
//! Also discovered while implementing (closing RESEARCH Open Question 2):
//! `GraphView` exposes no public getter/setter for its own pan/zoom
//! metadata in 0.31.0 -- only `egui_graphs::reset_metadata` (a full reset).
//! `app.view` therefore cannot be kept in literal sync with the widget's
//! internal pan/zoom on every frame; `detect_reset` below uses the one
//! public hook that exists (`reset_metadata`) to satisfy NAV-02's "Reset
//! view returns to the full graph" from the top bar's existing button.
//!
//! Deviation note (Task 2): `Layout::next` is generic over the node
//! payload type and cannot see `community`/`focus`, so wiring the custom
//! `layout::SeamLayout`/`SeamLayoutState` in (rather than the crate's
//! default random layout used through Task 1) necessarily touches this
//! file's `GraphView` type parameters and adds the target-injection pass
//! below, even though Task 2's own `<files>` scope names only `layout.rs`.
//! Not doing so would leave the plan's central must_have (the seam
//! pull-apart) entirely unimplemented -- Rule 3 (auto-fix blocking issues).

use crate::app::SeamExplorerApp;
use egui_graphs::{DisplayEdge, DisplayNode, DrawContext, EdgeProps, LayoutState, NodeProps};
use petgraph::stable_graph::DefaultIx;
use petgraph::Directed;

const DIMMED_FILL_HEX: &str = "#61708c";
const SIDE_A_HEX: &str = "#38d6c4";
const SIDE_B_HEX: &str = "#f2a63c";
const EDGE_HEX: &str = "#93a1bd";
const TEXT_HEX: &str = "#dfe6f2";
const DIM_OPACITY: f32 = 0.35;
const NODE_RADIUS: f32 = 6.0;
const LABEL_MAX_CHARS: usize = 24;

fn hex(h: &str) -> egui::Color32 {
    egui::Color32::from_hex(h).expect("valid hex")
}

/// Node payload carried into the render layer: id/label/community only --
/// deliberately excludes `seam_core::Node`'s `file_type` (and any future
/// metadata) so nothing beyond what the canvas actually needs leaks in
/// (mirrors the Tauri app's `render_data_from_model`/`RenderNode` scoping
/// discipline, 05-PATTERNS.md).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PayloadNode {
    pub id: String,
    pub label: String,
    pub community: seam_core::CommunityId,
}

/// Edge payload: each endpoint's community, so focus-driven dimming/
/// crossing-label logic can classify an edge without re-walking the model.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PayloadEdge {
    pub source_community: seam_core::CommunityId,
    pub target_community: seam_core::CommunityId,
}

/// The concrete `egui_graphs::Graph` type this app renders.
pub type SeamGraph =
    egui_graphs::Graph<PayloadNode, PayloadEdge, Directed, DefaultIx, SeamNodeShape, SeamEdgeShape>;

/// Pure structural mapping: `seam_core::Model` -> the `egui_graphs::Graph`
/// the widget consumes. Free of `egui::Ui`/`egui::Context` so
/// `test_build_graph_covers_every_node`/`test_render_mapping_is_scoped` can
/// drive it directly. Renders the **entire** graph unconditionally -- no
/// node-count threshold or sampling cap (Phase 2 D-01/D-02, ported
/// unchanged).
pub fn build_graph(model: &seam_core::Model) -> SeamGraph {
    let mut g: SeamGraph = egui_graphs::Graph::new(petgraph::stable_graph::StableGraph::default());
    let mut index_map: std::collections::HashMap<
        petgraph::stable_graph::NodeIndex,
        petgraph::stable_graph::NodeIndex,
    > = std::collections::HashMap::new();

    for idx in model.graph.node_indices() {
        let node = &model.graph[idx];
        let payload = PayloadNode {
            id: node.id.clone(),
            label: node.label.clone(),
            community: node.community.clone(),
        };
        let label = payload.label.clone();
        let new_idx = g.add_node_with_label(payload, label);
        index_map.insert(idx, new_idx);
    }

    for e in model.graph.edge_indices() {
        let (s, t) = model
            .graph
            .edge_endpoints(e)
            .expect("edge_indices() only yields edges with valid endpoints");
        let payload = PayloadEdge {
            source_community: model.graph[s].community.clone(),
            target_community: model.graph[t].community.clone(),
        };
        g.add_edge(index_map[&s], index_map[&t], payload);
    }

    g
}

/// NAV-04 focus dimming: full opacity for a node whose community is either
/// side of the focused seam (or when nothing is focused), a reduced
/// opacity for everything else.
pub fn node_opacity(
    community: &seam_core::CommunityId,
    focus: Option<&crate::app::FocusState>,
) -> f32 {
    match focus {
        None => 1.0,
        Some(f) => {
            if community == &f.a || community == &f.b {
                1.0
            } else {
                DIM_OPACITY
            }
        }
    }
}

/// Truncates `label` to `max_chars`, appending an ellipsis when shortened.
/// The pre-truncation label is kept by the caller (`SeamNodeShape` stores
/// both) for hover reveal (planner_assumptions: node-label overflow).
pub fn truncate_label(label: &str, max_chars: usize) -> String {
    if label.chars().count() <= max_chars {
        return label.to_string();
    }
    let truncated: String = label.chars().take(max_chars.saturating_sub(1)).collect();
    format!("{truncated}\u{2026}")
}

/// Custom node display: fill tinted by side membership when a seam is
/// focused, dimmed opacity baked into the fill alpha, a distinguishing
/// stroke for bridge nodes, and a truncated label (full label shown on
/// hover). `DisplayNode::shapes` has no `egui::Response` to attach
/// `on_hover_text` to; `self.hovered` is populated by `GraphView`'s own
/// hover detection, so swapping to the full label on hover is the
/// equivalent affordance this trait surface actually supports.
#[derive(Clone, Debug)]
pub struct SeamNodeShape {
    pos: egui::Pos2,
    selected: bool,
    dragged: bool,
    hovered: bool,
    color: egui::Color32,
    label_full: String,
    label_truncated: String,
    pub is_bridge: bool,
    radius: f32,
}

impl From<NodeProps<PayloadNode>> for SeamNodeShape {
    fn from(props: NodeProps<PayloadNode>) -> Self {
        let label_full = props.label.clone();
        Self {
            pos: props.location(),
            selected: props.selected,
            dragged: props.dragged,
            hovered: props.hovered,
            color: props.color().unwrap_or_else(|| hex(DIMMED_FILL_HEX)),
            label_truncated: truncate_label(&label_full, LABEL_MAX_CHARS),
            label_full,
            is_bridge: false,
            radius: NODE_RADIUS,
        }
    }
}

impl DisplayNode<PayloadNode, PayloadEdge, Directed, DefaultIx> for SeamNodeShape {
    fn closest_boundary_point(&self, dir: egui::Vec2) -> egui::Pos2 {
        self.pos + dir.normalized() * self.radius
    }

    fn shapes(&mut self, ctx: &DrawContext) -> Vec<egui::Shape> {
        let mut shapes = Vec::with_capacity(2);
        let center = ctx.meta.canvas_to_screen_pos(self.pos);
        let radius = ctx.meta.canvas_to_screen_size(self.radius);

        let stroke = if self.is_bridge {
            egui::Stroke::new(2.0, hex(TEXT_HEX))
        } else {
            egui::Stroke::NONE
        };

        shapes.push(
            egui::epaint::CircleShape {
                center,
                radius,
                fill: self.color,
                stroke,
            }
            .into(),
        );

        let text = if self.hovered || self.selected {
            &self.label_full
        } else {
            &self.label_truncated
        };
        if !text.is_empty() {
            let galley = ctx.ctx.fonts_mut(|f| {
                f.layout_no_wrap(
                    text.clone(),
                    egui::FontId::new(9.0 * ctx.meta.zoom.max(0.3), egui::FontFamily::Monospace),
                    hex(TEXT_HEX),
                )
            });
            let label_pos =
                egui::Pos2::new(center.x - galley.size().x / 2.0, center.y + radius + 2.0);
            shapes.push(egui::epaint::TextShape::new(label_pos, galley, hex(TEXT_HEX)).into());
        }

        shapes
    }

    fn update(&mut self, state: &NodeProps<PayloadNode>) {
        self.pos = state.location();
        self.selected = state.selected;
        self.dragged = state.dragged;
        self.hovered = state.hovered;
        if let Some(c) = state.color() {
            self.color = c;
        }
        self.label_full = state.label.clone();
        self.label_truncated = truncate_label(&self.label_full, LABEL_MAX_CHARS);
        // `is_bridge` intentionally left untouched -- `NodeProps` has no
        // slot for it; `apply_focus_styling` sets it directly via
        // `display_mut()` each frame instead.
    }

    fn is_inside(&self, pos: egui::Pos2) -> bool {
        (pos - self.pos).length() <= self.radius
    }
}

/// Custom edge display: directed arrowhead on every edge (NAV-03, every
/// edge is directed per Phase 1 D-09), dimmed in sympathy with its
/// endpoints, and a per-direction crossing-count label drawn only when a
/// seam is focused (`apply_focus_styling` leaves `label` empty otherwise).
#[derive(Clone, Debug)]
pub struct SeamEdgeShape {
    #[allow(dead_code)]
    order: usize,
    #[allow(dead_code)]
    selected: bool,
    label_text: String,
    width: f32,
    tip_size: f32,
    pub dim: bool,
}

impl From<EdgeProps<PayloadEdge>> for SeamEdgeShape {
    fn from(props: EdgeProps<PayloadEdge>) -> Self {
        Self {
            order: props.order,
            selected: props.selected,
            label_text: props.label,
            width: 1.5,
            tip_size: 8.0,
            dim: false,
        }
    }
}

impl DisplayEdge<PayloadNode, PayloadEdge, Directed, DefaultIx, SeamNodeShape> for SeamEdgeShape {
    fn shapes(
        &mut self,
        start: &egui_graphs::Node<PayloadNode, PayloadEdge, Directed, DefaultIx, SeamNodeShape>,
        end: &egui_graphs::Node<PayloadNode, PayloadEdge, Directed, DefaultIx, SeamNodeShape>,
        ctx: &DrawContext,
    ) -> Vec<egui::Shape> {
        let dir = (end.location() - start.location()).normalized();
        let start_p = start.display().closest_boundary_point(dir);
        let end_p = end.display().closest_boundary_point(-dir);
        let start_screen = ctx.meta.canvas_to_screen_pos(start_p);
        let end_screen = ctx.meta.canvas_to_screen_pos(end_p);

        let alpha = if self.dim { 70 } else { 200 };
        let color = hex(EDGE_HEX).gamma_multiply(alpha as f32 / 255.0);
        let stroke = egui::Stroke::new(self.width, color);

        let mut shapes = vec![egui::Shape::LineSegment {
            points: [start_screen, end_screen],
            stroke,
        }];

        if ctx.is_directed {
            shapes.push(arrow_head_shape(
                end_screen,
                dir,
                self.tip_size * ctx.meta.zoom.max(0.3),
                color,
            ));
        }

        if !self.label_text.is_empty() {
            let mid = start_screen + (end_screen - start_screen) / 2.0;
            let galley = ctx.ctx.fonts_mut(|f| {
                f.layout_no_wrap(
                    self.label_text.clone(),
                    egui::FontId::new(10.0, egui::FontFamily::Monospace),
                    color,
                )
            });
            shapes.push(egui::epaint::TextShape::new(mid, galley, color).into());
        }

        shapes
    }

    fn update(&mut self, state: &EdgeProps<PayloadEdge>) {
        self.order = state.order;
        self.selected = state.selected;
        self.label_text = state.label.clone();
        // `dim` intentionally left untouched -- see `SeamNodeShape::update`.
    }

    fn is_inside(
        &self,
        start: &egui_graphs::Node<PayloadNode, PayloadEdge, Directed, DefaultIx, SeamNodeShape>,
        end: &egui_graphs::Node<PayloadNode, PayloadEdge, Directed, DefaultIx, SeamNodeShape>,
        pos: egui::Pos2,
    ) -> bool {
        distance_segment_to_point(start.location(), end.location(), pos) <= self.width.max(3.0)
    }
}

fn arrow_head_shape(
    tip: egui::Pos2,
    dir: egui::Vec2,
    size: f32,
    color: egui::Color32,
) -> egui::Shape {
    let dir = dir.normalized();
    let back = tip - dir * size;
    let perp = egui::Vec2::new(-dir.y, dir.x);
    let spread = size * 0.5;
    let left = back + perp * spread;
    let right = back - perp * spread;
    egui::Shape::convex_polygon(vec![tip, left, right], color, egui::Stroke::NONE)
}

fn distance_segment_to_point(a: egui::Pos2, b: egui::Pos2, point: egui::Pos2) -> f32 {
    let ab = b - a;
    let len_sq = ab.length_sq();
    if len_sq <= f32::EPSILON {
        return (point - a).length();
    }
    let t = ((point - a).dot(ab) / len_sq).clamp(0.0, 1.0);
    let proj = a + ab * t;
    (point - proj).length()
}

/// `CentralPanel` entry point (frozen signature, Plan 01 -- `&mut` per the
/// Artifacts section, since this task also reads/writes `app.view`).
/// Renders the pre-load placeholder when no graph is loaded; otherwise
/// builds a fresh `SeamGraph` from `app.model` each frame and renders it
/// with mouse/trackpad pan+zoom enabled.
pub fn show(ui: &mut egui::Ui, app: &mut SeamExplorerApp) {
    let Some(model) = &app.model else {
        ui.centered_and_justified(|ui| {
            ui.label("Load a graph.json to begin.");
        });
        return;
    };

    let mut graph = build_graph(model);
    apply_focus_styling(&mut graph, app);
    let canvas_rect = ui.available_rect_before_wrap();
    inject_layout_targets(ui, canvas_rect, &graph, app);

    let nav = egui_graphs::SettingsNavigation::new()
        .with_zoom_and_pan_enabled(true)
        .with_fit_to_screen_enabled(false);

    let response = ui.add(
        &mut egui_graphs::GraphView::<
            PayloadNode,
            PayloadEdge,
            Directed,
            DefaultIx,
            SeamNodeShape,
            SeamEdgeShape,
            crate::layout::SeamLayoutState,
            crate::layout::SeamLayout,
        >::new(&mut graph)
        .with_navigations(&nav),
    );

    // Overlay drawn strictly after the GraphView widget so it composites on
    // top (D-13, Task 3 action) -- only when a seam is focused.
    if let Some(focus) = &app.focus {
        if let Some(detail) = &app.detail {
            crate::overlay::paint_seam_line(ui, canvas_rect, response.rect, &detail.verdict);
            let edges: Vec<_> = graph
                .edges_iter()
                .map(|(_, e)| {
                    (
                        e.payload().source_community.clone(),
                        e.payload().target_community.clone(),
                    )
                })
                .collect();
            crate::overlay::paint_crossing_threads(ui, canvas_rect, response.rect, &edges, focus);
        }
    }

    detect_reset(ui, app);
}

/// Computes this frame's per-node target x (D-13 seam pull-apart, via
/// `layout::seam_target_x`) and the canvas center, and injects both into
/// the persisted `SeamLayoutState` the widget's own `sync_layout` will read
/// a moment later this same frame (see module doc). Recomputes the target
/// map every frame (cheap -- O(nodes), a pure function of
/// community/focus/center/width) rather than diffing on focus/resize
/// changes; the easing itself (not this recomputation) is what keeps the
/// pull-apart from snapping, so the two are visually equivalent.
fn inject_layout_targets(
    ui: &mut egui::Ui,
    canvas_rect: egui::Rect,
    graph: &SeamGraph,
    app: &SeamExplorerApp,
) {
    let center = canvas_rect.center();
    let canvas_width = canvas_rect.width().max(1.0);
    let focus_pair = app.focus.as_ref().map(|f| (&f.a, &f.b));

    let mut targets: std::collections::HashMap<usize, f32> = std::collections::HashMap::new();
    let mut groups: std::collections::HashMap<usize, u8> = std::collections::HashMap::new();
    for (idx, node) in graph.nodes_iter() {
        let target_x = crate::layout::seam_target_x(
            &node.payload().community,
            focus_pair,
            center.x,
            canvas_width,
        );
        targets.insert(idx.index(), target_x);
        groups.insert(
            idx.index(),
            crate::layout::seam_group(&node.payload().community, focus_pair),
        );
    }

    let mut state = crate::layout::SeamLayoutState::load(ui, None);
    state.set_targets(targets, groups, center, canvas_width, canvas_rect.height());
    state.save(ui, None);
}

/// One pass over every node/edge baking focus-driven styling (opacity,
/// side tint, bridge stroke, crossing labels) into the graph's built-in
/// `color`/`label` slots and the custom `is_bridge`/`dim` fields --
/// everything `SeamNodeShape`/`SeamEdgeShape` cannot compute themselves
/// since `DisplayNode`/`DisplayEdge::shapes` only ever see `NodeProps`/
/// `EdgeProps`, never arbitrary app state (RESEARCH Assumption A1, closed).
fn apply_focus_styling(graph: &mut SeamGraph, app: &SeamExplorerApp) {
    let node_indices: Vec<_> = graph.nodes_iter().map(|(idx, _)| idx).collect();
    for idx in node_indices {
        let (community, id) = {
            let payload = graph.node(idx).unwrap().payload();
            (payload.community.clone(), payload.id.clone())
        };
        let opacity = node_opacity(&community, app.focus.as_ref());
        let base = match &app.focus {
            Some(f) if community == f.a => hex(SIDE_A_HEX),
            Some(f) if community == f.b => hex(SIDE_B_HEX),
            _ => hex(DIMMED_FILL_HEX),
        };
        let is_bridge = app
            .detail
            .as_ref()
            .is_some_and(|d| d.bridges_a.contains(&id) || d.bridges_b.contains(&id));

        if let Some(n) = graph.node_mut(idx) {
            n.set_color(base.gamma_multiply(opacity));
            n.display_mut().is_bridge = is_bridge;
        }
    }

    let edge_indices: Vec<_> = graph.edges_iter().map(|(idx, _)| idx).collect();
    for idx in edge_indices {
        let (sc, tc) = {
            let payload = graph.edge(idx).unwrap().payload();
            (
                payload.source_community.clone(),
                payload.target_community.clone(),
            )
        };
        let dim = match &app.focus {
            None => false,
            Some(f) => !((sc == f.a || sc == f.b) && (tc == f.a || tc == f.b)),
        };
        let mut label = String::new();
        if let (Some(f), Some(detail)) = (&app.focus, &app.detail) {
            if sc == f.a && tc == f.b {
                label = detail.a_to_b.to_string();
            } else if sc == f.b && tc == f.a {
                label = detail.b_to_a.to_string();
            }
        }
        if let Some(e) = graph.edge_mut(idx) {
            e.set_label(label);
            e.display_mut().dim = dim;
        }
    }
}

/// The one shared fit-to-view reset (NAV-02) -- `keyboard::handle`'s `0` key
/// calls this. The top bar's "Reset view" button (`app.rs`, frozen for this
/// whole phase) sets `app.view = ViewState::default()` inline rather than
/// calling this function directly, since `app.rs` cannot be edited this
/// plan -- but that inline assignment is byte-for-byte the same semantics
/// this function performs, so the two call sites can never drift apart even
/// though they aren't literally the same call site.
pub fn reset_view(app: &mut SeamExplorerApp) {
    app.view = crate::app::ViewState::default();
}

/// Detects an actual reset request (the top bar's "Reset view" button, or
/// Plan 04's `0` key via `reset_view`, both of which set
/// `app.view = ViewState::default()`) and resets `egui_graphs`'s own
/// pan/zoom metadata *and* the persisted `SeamLayoutState` to match -- there
/// is no public setter for either in 0.31.0's API, only a reset (NAV-02).
///
/// Deliberately fires only when `app.view` just *became* the default value,
/// not on every change (Task 2 fix, Rule 1): Plan 04's keyboard pan/zoom
/// (`keyboard::apply_key`) also mutates `app.view` every keypress, and
/// `egui_graphs::reset` wipes the entire custom `SeamLayoutState` (node
/// positions/easing/repulsion), not just pan/zoom -- treating every pan/zoom
/// nudge as "changed" would trigger a full graph-layout reset on every arrow
/// key, which is a regression, not a no-op. Narrowing the trigger to "became
/// default" preserves the original reset-detection intent (both the button
/// and `0` set exactly `ViewState::default()` to signal "reset requested")
/// while leaving keyboard pan/zoom's separate live-wiring gap (WINDOWS.md
/// #3) untouched -- that gap predates this plan and is out of scope here.
fn detect_reset(ui: &mut egui::Ui, app: &SeamExplorerApp) {
    let id = egui::Id::new("seam_explorer_graph_view_snapshot");
    let current = (app.view.zoom, app.view.pan);
    let default_view = crate::app::ViewState::default();
    let default = (default_view.zoom, default_view.pan);
    let became_default = ui.data_mut(|d| {
        let prev: Option<(f32, egui::Vec2)> = d.get_temp(id);
        d.insert_temp(id, current);
        current == default && prev.is_some_and(|p| p != current)
    });
    if became_default {
        egui_graphs::reset::<crate::layout::SeamLayoutState>(ui, None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CLEAN_FIXTURE: &str = include_str!("../../seam-core/tests/fixtures/clean.json");

    #[test]
    fn test_focus_dimming() {
        let a: seam_core::CommunityId = "A".to_string();
        let b: seam_core::CommunityId = "B".to_string();
        let c: seam_core::CommunityId = "C".to_string();
        let focus = crate::app::FocusState {
            a: a.clone(),
            b: b.clone(),
        };

        assert_eq!(node_opacity(&a, Some(&focus)), 1.0);
        assert_eq!(node_opacity(&b, Some(&focus)), 1.0);
        assert!(node_opacity(&c, Some(&focus)) < 1.0);

        assert_eq!(node_opacity(&a, None), 1.0);
        assert_eq!(node_opacity(&b, None), 1.0);
        assert_eq!(node_opacity(&c, None), 1.0);
    }

    #[test]
    fn test_build_graph_covers_every_node() {
        let ingest = seam_core::from_json(CLEAN_FIXTURE).expect("clean fixture must ingest");
        let model = ingest.model;
        let graph = build_graph(&model);
        assert_eq!(graph.node_count(), model.graph.node_count());
        assert_eq!(graph.edge_count(), model.graph.edge_count());
    }

    #[test]
    fn test_truncate_label() {
        assert_eq!(truncate_label("short", 24), "short");

        let long = "a_very_long_component_name_that_exceeds_the_limit";
        let truncated = truncate_label(long, 24);
        assert!(truncated.chars().count() <= 24);
        assert!(truncated.ends_with('\u{2026}'));
    }

    #[test]
    fn test_render_mapping_is_scoped() {
        let ingest = seam_core::from_json(CLEAN_FIXTURE).expect("clean fixture must ingest");
        let model = ingest.model;
        let graph = build_graph(&model);
        let idx = model
            .graph
            .node_indices()
            .next()
            .expect("fixture has nodes");
        let source = &model.graph[idx];
        let payload = graph
            .nodes_iter()
            .map(|(_, n)| n.payload())
            .find(|p| p.id == source.id)
            .expect("payload for this node must exist");
        assert_eq!(payload.label, source.label);
        assert_eq!(payload.community, source.community);
        // `PayloadNode` structurally has no `file_type`/metadata field at
        // all -- scoping is enforced at compile time, not just by this
        // runtime check of the fields that *are* present.
    }
}
