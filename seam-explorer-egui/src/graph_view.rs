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
//! Correction (Plan 08 gap closure, closing UAT gaps G-05-2/G-05-3/G-05-4,
//! see `.planning/debug/keyboard-pan-not-visible.md`): the claim that used to
//! live here -- that `GraphView` exposes no public getter/setter for its own
//! pan/zoom metadata in 0.31.0 -- was factually wrong and was the root cause
//! of those three gaps. `egui_graphs::MetadataFrame::pan`/`::zoom` are
//! public, directly-settable fields; the `MetadataFrame::new(id).load(ui)` /
//! `.save(ui)` pattern this file already uses read-only elsewhere is the
//! same pattern that writes `app.view` into the widget's rendered transform.
//! `view_to_frame`/`frame_to_view` (below) define that mapping precisely;
//! `sync_view_into_frame`/`read_frame_into_view` apply it every frame in
//! both directions so `app.view` is a truthful, bidirectionally-synced
//! source of view state rather than the write-only field it was before.
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

/// Fractional padding `fit_view` applies to a bounds rect before computing
/// zoom, so framed content doesn't touch the viewport edges exactly --
/// mirrors `egui_graphs`' own `fit_to_screen_padding` intent.
const FIT_VIEW_PADDING: f32 = 0.10;

/// The `app.view` <-> `egui_graphs::MetadataFrame` transform contract (Plan
/// 08 gap closure, G-05-2/G-05-3). `C` is the viewport centre expressed as a
/// `Vec2` (`viewport / 2.0`), matching `egui_graphs`' own origin-at-`ZERO`
/// local rect used by its internal pan/zoom math:
///
/// - widget space (`egui_graphs`): `local_screen = canvas * frame.zoom + frame.pan`
/// - app space (this contract):   `local_screen = (canvas + view.pan - C) * view.zoom + C`
///
/// Equating the two and solving for `frame.{zoom,pan}` in terms of
/// `view.{zoom,pan}` yields this function: `zoom = view.zoom`,
/// `pan = (view.pan - C) * view.zoom + C`. `frame_to_view` is the exact
/// algebraic inverse. This is what makes a keyboard pan move a *constant*
/// screen-space distance regardless of zoom (`keyboard::apply_key` divides
/// its step by `view.zoom` before adding it to `view.pan`; multiplying that
/// same term by `view.zoom` here cancels the division, leaving a constant
/// screen-space delta) and what makes a pure zoom change anchor on the
/// viewport centre (see `zoom_change_keeps_viewport_centre_fixed` below).
pub fn view_to_frame(view: crate::app::ViewState, viewport: egui::Vec2) -> (f32, egui::Vec2) {
    let center = egui::Vec2::new(viewport.x / 2.0, viewport.y / 2.0);
    let pan = (view.pan - center) * view.zoom + center;
    (view.zoom, pan)
}

/// Exact inverse of `view_to_frame` -- see its doc comment for the contract.
pub fn frame_to_view(zoom: f32, pan: egui::Vec2, viewport: egui::Vec2) -> crate::app::ViewState {
    let center = egui::Vec2::new(viewport.x / 2.0, viewport.y / 2.0);
    crate::app::ViewState {
        zoom,
        pan: (pan - center) / zoom + center,
    }
}

/// The `ViewState` that frames `bounds` (canvas-space) entirely inside
/// `viewport`, mirroring `egui_graphs`' own `fit_to_screen` intent (NAV-02
/// "Reset view" / "0 key" -> re-frame the whole graph). Scales so the padded
/// bounds fit both axes (taking the smaller scale) and centres the bounds on
/// the viewport. Guards non-finite/inverted bounds (an empty graph's
/// collapsed rect) and zero-area bounds (a single node) by falling back to
/// `zoom = 1.0` rather than producing an infinite or NaN transform.
pub fn fit_view(bounds: egui::Rect, viewport: egui::Vec2) -> crate::app::ViewState {
    let center = egui::Vec2::new(viewport.x / 2.0, viewport.y / 2.0);
    let (min, max) = (bounds.min, bounds.max);
    let invalid_bounds = !min.x.is_finite()
        || !min.y.is_finite()
        || !max.x.is_finite()
        || !max.y.is_finite()
        || min.x > max.x
        || min.y > max.y;
    if invalid_bounds {
        return crate::app::ViewState::default();
    }

    let bounds_center = bounds.center().to_vec2();
    let diag = max - min;
    if !diag.x.is_finite() || !diag.y.is_finite() || diag.x <= 0.0 || diag.y <= 0.0 {
        // Zero-area bounds (single node): frame it at 1.0x, centred.
        return crate::app::ViewState {
            zoom: 1.0,
            pan: center - bounds_center,
        };
    }

    let padded = diag * (1.0 + FIT_VIEW_PADDING);
    let width = padded.x.max(1e-3);
    let height = padded.y.max(1e-3);
    let zoom_x = (viewport.x / width).abs();
    let zoom_y = (viewport.y / height).abs();
    let mut zoom = zoom_x.min(zoom_y);
    if !zoom.is_finite() || zoom <= 0.0 {
        zoom = 1.0;
    }

    crate::app::ViewState {
        zoom,
        pan: center - bounds_center,
    }
}

/// Epsilon below which two transforms are considered unchanged -- used by
/// both sync legs so a steady state performs no write and cannot accumulate
/// float drift (T-05-08-03).
const ZOOM_EPSILON: f32 = 1e-4;
const PAN_EPSILON: f32 = 1e-3;

fn transform_differs(zoom_a: f32, pan_a: egui::Vec2, zoom_b: f32, pan_b: egui::Vec2) -> bool {
    (zoom_a - zoom_b).abs() >= ZOOM_EPSILON || (pan_a - pan_b).length() >= PAN_EPSILON
}

/// Writes `view` into the persisted `egui_graphs::MetadataFrame` (via
/// `view_to_frame`) so the widget actually renders with it, using the same
/// `MetadataFrame::new(None).load(ui)` / `.save(ui)` idiom this file already
/// uses read-only elsewhere. Only mutates + saves when the target differs
/// from the frame's current value by more than the epsilon guard above, so
/// a steady state (no pan/zoom this frame) never rewrites the frame. Returns
/// whether it wrote, for tests.
pub fn sync_view_into_frame(
    ui: &mut egui::Ui,
    view: crate::app::ViewState,
    viewport: egui::Vec2,
) -> bool {
    let mut frame = egui_graphs::MetadataFrame::new(None).load(ui);
    let (target_zoom, target_pan) = view_to_frame(view, viewport);
    if !transform_differs(frame.zoom, frame.pan, target_zoom, target_pan) {
        return false;
    }
    frame.zoom = target_zoom;
    frame.pan = target_pan;
    frame.save(ui);
    true
}

/// Reads the persisted `egui_graphs::MetadataFrame` back into a `ViewState`
/// (via `frame_to_view`) -- the return leg that keeps `app.view` truthful
/// after mouse/trackpad pan and genuine pinch/Ctrl-scroll zoom (both of
/// which mutate the frame directly inside the widget's own `Widget::ui()`).
pub fn read_frame_into_view(ui: &egui::Ui, viewport: egui::Vec2) -> crate::app::ViewState {
    let frame = egui_graphs::MetadataFrame::new(None).load(ui);
    frame_to_view(frame.zoom, frame.pan, viewport)
}

/// `CentralPanel` entry point (frozen signature, Plan 01 -- `&mut` per the
/// Artifacts section, since this task also reads/writes `app.view`).
/// Renders the pre-load placeholder when no graph is loaded; otherwise
/// builds a fresh `SeamGraph` from `app.model` each frame and renders it
/// with mouse/trackpad pan+zoom enabled.
pub fn show(ui: &mut egui::Ui, app: &mut SeamExplorerApp) {
    // D-05/D-14: shown once ever, regardless of whether a graph is loaded
    // yet -- the trace-mode toggle this overlay points at is always present
    // in the (frozen) top bar. Called from here, not `app.rs`, since
    // `app.rs`'s panel-dispatch wiring is frozen this whole phase and has
    // no call site for it (Rule 3 -- the same constraint 05-02's banner
    // wiring and 05-03/05-04's `graph_view.rs` touch-ups document).
    crate::trace::show_onboarding(ui, app);

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
    // `canvas_rect` (not `response.rect`, unavailable until after `ui.add`
    // below) is the one viewport value used for both sync legs and the
    // Reset fit -- self-consistency of the centre term matters more than
    // which rect it came from (Plan 08 gap closure).
    let viewport = canvas_rect.size();
    sync_view_into_frame(ui, app.view, viewport);

    let nav = egui_graphs::SettingsNavigation::new()
        .with_zoom_and_pan_enabled(true)
        .with_fit_to_screen_enabled(false);
    // TRACE-01: while trace mode is on, `egui_graphs`' own node-drag
    // reposition is disabled so the drag belongs entirely to
    // `handle_trace_gesture` below -- the reposition branch (trace mode
    // off) and the trace branch (trace mode on) stay mutually exclusive,
    // matching the D3 original's `dragTraceActive` two-branch split
    // (RESEARCH Architecture Diagram, "Drag gesture on a node").
    let interaction =
        egui_graphs::SettingsInteraction::new().with_dragging_enabled(!app.trace_mode);

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
        .with_navigations(&nav)
        .with_interactions(&interaction),
    );

    // Overlay drawn strictly after the GraphView widget so it composites on
    // top (D-13) -- only when a seam is focused.
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

    // TRACE-01/02: drag-to-trace gesture handling (rubber band while
    // dragging, `seam_core::trace_path` call on a valid drop) and the
    // resolved path's canvas highlight. A no-op while trace mode is off.
    handle_trace_gesture(ui, &graph, &response, app);
    if let Some(trace) = &app.trace {
        if let Some(path) = &trace.path {
            let meta = egui_graphs::MetadataFrame::new(None).load(ui);
            let hop_positions: Vec<egui::Pos2> = path
                .hops
                .iter()
                .filter_map(|id| find_node_screen_pos(&graph, &meta, response.rect, id))
                .collect();
            crate::overlay::paint_traced_path(ui, &hop_positions);
        }
    }

    // Read the widget's own MetadataFrame (mutated by mouse pan and genuine
    // pinch/Ctrl-scroll zoom inside `ui.add` above) back into `app.view`, so
    // it stops being a write-only field (G-05-2's reset half) -- the same
    // epsilon guard as the write leg keeps a steady state a no-op.
    let read_back = read_frame_into_view(ui, viewport);
    if transform_differs(app.view.zoom, app.view.pan, read_back.zoom, read_back.pan) {
        app.view = read_back;
    }

    detect_reset(ui, &graph, viewport, app);
}

/// Screen-space position of a canvas-space point, using this frame's
/// `GraphView`-saved metadata plus the widget's own rect offset -- the same
/// conversion `overlay::paint_seam_line` uses for its own endpoints.
fn to_screen(
    meta: &egui_graphs::MetadataFrame,
    graph_rect: egui::Rect,
    canvas_pos: egui::Pos2,
) -> egui::Pos2 {
    meta.canvas_to_screen_pos(canvas_pos) + graph_rect.left_top().to_vec2()
}

/// Screen-space position of the node whose `seam_core::Node::id == id`, if
/// it exists in `graph` this frame. Used both to anchor the in-flight
/// rubber band's origin and to resolve a completed trace path's hops to
/// screen points.
fn find_node_screen_pos(
    graph: &SeamGraph,
    meta: &egui_graphs::MetadataFrame,
    graph_rect: egui::Rect,
    id: &str,
) -> Option<egui::Pos2> {
    graph
        .nodes_iter()
        .find(|(_, n)| n.payload().id == id)
        .map(|(_, n)| to_screen(meta, graph_rect, n.location()))
}

/// Hit-tests a screen-space pointer position against `graph`'s nodes via
/// `egui_graphs::Graph::node_by_screen_pos` -- the same hit-test the
/// widget's own hover/click handling uses internally, so trace-mode
/// hit-testing stays pixel-identical to the widget's own idea of "over this
/// node". Returns the hit node's `seam_core::Node::id` and its current
/// screen position.
fn hit_test_node(
    graph: &SeamGraph,
    meta: &egui_graphs::MetadataFrame,
    graph_rect: egui::Rect,
    screen_pos: egui::Pos2,
) -> Option<(String, egui::Pos2)> {
    let local = (screen_pos - graph_rect.left_top()).to_pos2();
    let idx = graph.node_by_screen_pos(meta, local)?;
    let node = graph.node(idx)?;
    let id = node.payload().id.clone();
    let screen = to_screen(meta, graph_rect, node.location());
    Some((id, screen))
}

/// Turns this frame's live drag `Response` into a `trace::GestureInput`,
/// feeds it through the pure `trace::update_gesture` state machine, paints
/// the in-flight rubber band (snapped to a node when the cursor is over
/// one, per the plan's action text), and runs `seam_core::trace_path` on a
/// completed gesture -- storing the outcome in `app.trace` for the detail
/// panel to render. A no-op while trace mode is off; `egui_graphs`' own
/// node-reposition drag (enabled via `with_dragging_enabled` above) handles
/// that case instead.
fn handle_trace_gesture(
    ui: &mut egui::Ui,
    graph: &SeamGraph,
    response: &egui::Response,
    app: &mut SeamExplorerApp,
) {
    if !app.trace_mode {
        // Keep egui's own per-frame gesture memory reset so a stale
        // in-flight drag from before trace mode was toggled off can't
        // resurrect itself the next time trace mode is toggled back on.
        crate::trace::save_gesture(ui, crate::trace::TraceGesture::Idle);
        return;
    }

    let meta = egui_graphs::MetadataFrame::new(None).load(ui);
    let graph_rect = response.rect;

    let input = if response.drag_started() {
        response.interact_pointer_pos().and_then(|p| {
            hit_test_node(graph, &meta, graph_rect, p).map(|(id, _)| {
                crate::trace::GestureInput::DragStart {
                    node: id,
                    cursor: p,
                }
            })
        })
    } else if response.dragged() {
        response.interact_pointer_pos().map(|p| {
            let snapped = hit_test_node(graph, &meta, graph_rect, p).map(|(_, screen)| screen);
            crate::trace::GestureInput::DragMove {
                cursor: snapped.unwrap_or(p),
            }
        })
    } else if response.drag_stopped() {
        let pos = response
            .interact_pointer_pos()
            .or_else(|| ui.input(|i| i.pointer.hover_pos()));
        let node = pos
            .and_then(|p| hit_test_node(graph, &meta, graph_rect, p))
            .map(|(id, _)| id);
        Some(crate::trace::GestureInput::DragStop { node })
    } else {
        None
    };

    let mut gesture = crate::trace::load_gesture(ui);
    if let Some(input) = input {
        gesture = crate::trace::update_gesture(gesture, input, app.trace_mode);
    }

    if let crate::trace::TraceGesture::Dragging { from, cursor } = &gesture {
        if let Some(from_screen) = find_node_screen_pos(graph, &meta, graph_rect, from) {
            crate::overlay::paint_rubber_band(ui, from_screen, *cursor);
        }
    }

    if let crate::trace::TraceGesture::Completed { from, to } = &gesture {
        if let Some(model) = &app.model {
            let result = crate::trace::run(model, from, to);
            // D-07 dual dismissal: only a resolved path (not a no-path
            // outcome) counts as a "successful trace" for onboarding
            // purposes, ported verbatim from `renderTraceResult`'s
            // `if (result && ...)` guard (`frontend/index.html:900`).
            if result.path.is_some() {
                crate::trace::dismiss_on_first_trace(app);
            }
            app.trace = Some(result);
        }
        gesture = crate::trace::TraceGesture::Idle;
    }

    crate::trace::save_gesture(ui, gesture);
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

/// Zoom level applied when jumping to a search result (NAV-01) -- closer-in
/// than the default 1.0 so the target reads clearly.
const JUMP_ZOOM: f32 = 1.6;

/// A search-to-jump target (NAV-01): a specific node's canvas-space
/// position, or a seam's own center. Kept as canvas-space `Pos2` (not a
/// `seam_core` id) since panels outside `graph_view` (e.g. `seam_list`)
/// have no access to this module's live canvas geometry.
pub enum JumpTarget {
    Node(egui::Pos2),
    Seam(egui::Pos2),
}

/// Pure: the `ViewState` that centers `target` at `JUMP_ZOOM`. `pan` is
/// defined as the offset that brings `target` to the canvas origin,
/// matching `keyboard::apply_key`'s existing pan semantics (both mutate the
/// same `app.view`). No `egui::Ui`/`egui::Context` parameter -- unit
/// testable in isolation.
pub fn compute_jump_view(target: egui::Pos2) -> crate::app::ViewState {
    crate::app::ViewState {
        zoom: JUMP_ZOOM,
        pan: egui::vec2(-target.x, -target.y),
    }
}

/// Pans/zooms the canvas to `target` (NAV-01) -- mutates the same
/// `app.view` mouse/keyboard navigation uses (`keyboard::apply_key`,
/// `reset_view`), so it never becomes a second, drifting source of view
/// state.
pub fn jump_to(app: &mut SeamExplorerApp, target: JumpTarget) {
    let pos = match target {
        JumpTarget::Node(p) | JumpTarget::Seam(p) => p,
    };
    app.view = compute_jump_view(pos);
}

/// Detects an actual reset request (the top bar's "Reset view" button, or
/// the `0` key via `reset_view`, both of which set
/// `app.view = ViewState::default()`) and re-frames the whole graph by
/// assigning `app.view = fit_view(bounds, viewport)`, where `bounds` is the
/// bounding rect of every node's current `location()` from the
/// already-rendered `graph` (node positions are current once `ui.add` has
/// returned this frame). When the graph has no nodes, `app.view` is left at
/// the default rather than fit against an empty/degenerate rect.
///
/// Deliberately fires only when `app.view` just *became* the default value,
/// not on every change (Rule 1, unchanged from the prior version of this
/// function): mouse/keyboard pan/zoom now also mutate `app.view` every
/// frame via `show()`'s sync legs above -- treating every nudge as "changed"
/// would re-fit the view on every pan/zoom, which is a regression, not a
/// no-op. Narrowing the trigger to "became default" preserves the original
/// reset-detection intent (both the button and `0` set exactly
/// `ViewState::default()` to signal "reset requested").
///
/// Plan 08 gap closure (G-05-2's reset half): this function used to call
/// `egui_graphs::reset::<SeamLayoutState>(ui, None)` instead of computing a
/// fit. That call is deliberately no longer made, for three reasons: NAV-02
/// asks for fit-to-view, not re-layout; `egui_graphs::reset` also wipes the
/// persisted `SeamLayoutState`, which would re-seed every node position and
/// destroy the pull-apart arrangement the user is currently looking at; and
/// a bare metadata reset cannot re-fit afterwards, because `egui_graphs`'
/// first-frame fit hook lives in `MetadataInstance`, which 0.31.0 does not
/// re-export (`lib.rs` exports only `reset_metadata` and `MetadataFrame`).
/// Computing the fit here, from this app's own node bounds, is both more
/// correct (it actually frames the graph, where the old call only reset
/// pan/zoom to `(1.0, ZERO)` -- coincidentally identical to the sentinel
/// default, so this bug was invisible until mouse/keyboard interaction
/// stopped leaving `app.view` at the default permanently) and more stable.
fn detect_reset(
    ui: &mut egui::Ui,
    graph: &SeamGraph,
    viewport: egui::Vec2,
    app: &mut SeamExplorerApp,
) {
    let id = egui::Id::new("seam_explorer_graph_view_snapshot");
    let current = (app.view.zoom, app.view.pan);
    let default_view = crate::app::ViewState::default();
    let default = (default_view.zoom, default_view.pan);
    let became_default = ui.data_mut(|d| {
        let prev: Option<(f32, egui::Vec2)> = d.get_temp(id);
        d.insert_temp(id, current);
        current == default && prev.is_some_and(|p| p != current)
    });
    if !became_default || graph.node_count() == 0 {
        return;
    }

    let mut bounds = egui::Rect::NOTHING;
    for (_, node) in graph.nodes_iter() {
        bounds.extend_with(node.location());
    }
    app.view = fit_view(bounds, viewport);
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

    /// `compute_jump_view` produces a `ViewState` whose pan centers the
    /// target's position (offset that brings `target` to the canvas
    /// origin) and whose zoom is the defined jump zoom level (NAV-01).
    #[test]
    fn test_jump_to_centers_target() {
        let target = egui::pos2(120.0, 40.0);
        let view = compute_jump_view(target);
        assert_eq!(view.pan, egui::vec2(-120.0, -40.0));
        assert_eq!(view.zoom, JUMP_ZOOM);
    }

    // ============================================================
    // Plan 08 gap closure (G-05-2/G-05-3): view_to_frame / frame_to_view /
    // fit_view contract. Each test name below is the exact name the plan's
    // <behavior> block specifies.
    // ============================================================

    /// `frame_to_view(view_to_frame(v, vp), vp) == v` within 1e-3, for
    /// several sample views including non-unit zoom and negative pan.
    #[test]
    fn view_frame_mapping_round_trips() {
        let viewport = egui::vec2(800.0, 600.0);
        let samples = [
            crate::app::ViewState {
                zoom: 1.0,
                pan: egui::Vec2::ZERO,
            },
            crate::app::ViewState {
                zoom: 2.0,
                pan: egui::vec2(50.0, -30.0),
            },
            crate::app::ViewState {
                zoom: 0.5,
                pan: egui::vec2(-120.0, 75.0),
            },
            crate::app::ViewState {
                zoom: 1.75,
                pan: egui::vec2(-10.0, -10.0),
            },
        ];
        for view in samples {
            let (zoom, pan) = view_to_frame(view, viewport);
            let round = frame_to_view(zoom, pan, viewport);
            assert!(
                (round.zoom - view.zoom).abs() < 1e-3,
                "zoom round-trip failed for {view:?}: got {round:?}"
            );
            assert!(
                (round.pan - view.pan).length() < 1e-3,
                "pan round-trip failed for {view:?}: got {round:?}"
            );
        }
    }

    /// `ViewState::default()` maps to `zoom == 1.0`, `pan == ZERO` -- the
    /// widget's own pristine `MetadataFrame::default()`.
    #[test]
    fn default_view_maps_to_pristine_frame() {
        let viewport = egui::vec2(800.0, 600.0);
        let (zoom, pan) = view_to_frame(crate::app::ViewState::default(), viewport);
        assert_eq!(zoom, 1.0);
        assert_eq!(pan, egui::Vec2::ZERO);
    }

    /// For zoom in {0.5, 1.0, 2.0}, feeding `keyboard::apply_key` through
    /// `view_to_frame` moves the frame pan by exactly `40.0` screen px in
    /// the matching direction -- the property the D3 original had
    /// (`translateBy(PAN_STEP / k)` nets a constant 40 screen px), and the
    /// direct regression test for G-05-3 at the pure-function level.
    #[test]
    fn keyboard_pan_moves_a_constant_forty_screen_px() {
        let viewport = egui::vec2(800.0, 600.0);
        for zoom in [0.5_f32, 1.0, 2.0] {
            let view = crate::app::ViewState {
                zoom,
                pan: egui::vec2(10.0, -5.0),
            };
            let (_, before) = view_to_frame(view, viewport);

            let left = crate::keyboard::apply_key(view, crate::keyboard::KeyAction::PanLeft);
            let (_, after_left) = view_to_frame(left, viewport);
            assert!(
                (after_left.x - before.x - 40.0).abs() < 1e-2,
                "PanLeft must move +40 screen px at zoom {zoom}, got {}",
                after_left.x - before.x
            );

            let right = crate::keyboard::apply_key(view, crate::keyboard::KeyAction::PanRight);
            let (_, after_right) = view_to_frame(right, viewport);
            assert!(
                (after_right.x - before.x + 40.0).abs() < 1e-2,
                "PanRight must move -40 screen px at zoom {zoom}, got {}",
                after_right.x - before.x
            );

            let up = crate::keyboard::apply_key(view, crate::keyboard::KeyAction::PanUp);
            let (_, after_up) = view_to_frame(up, viewport);
            assert!(
                (after_up.y - before.y - 40.0).abs() < 1e-2,
                "PanUp must move +40 screen px at zoom {zoom}, got {}",
                after_up.y - before.y
            );

            let down = crate::keyboard::apply_key(view, crate::keyboard::KeyAction::PanDown);
            let (_, after_down) = view_to_frame(down, viewport);
            assert!(
                (after_down.y - before.y + 40.0).abs() < 1e-2,
                "PanDown must move -40 screen px at zoom {zoom}, got {}",
                after_down.y - before.y
            );
        }
    }

    /// The canvas point that maps to the viewport centre before a
    /// zoom-only change (same `view.pan`) still maps to the centre after
    /// it, for both zoom-in and zoom-out.
    #[test]
    fn zoom_change_keeps_viewport_centre_fixed() {
        let viewport = egui::vec2(800.0, 600.0);
        let center = viewport / 2.0;
        let view = crate::app::ViewState {
            zoom: 1.0,
            pan: egui::vec2(30.0, -20.0),
        };

        for &new_zoom in &[2.0_f32, 0.5] {
            let (zoom0, pan0) = view_to_frame(view, viewport);
            let canvas_point_before = (center - pan0) / zoom0;

            let zoomed_view = crate::app::ViewState {
                zoom: new_zoom,
                ..view
            };
            let (zoom1, pan1) = view_to_frame(zoomed_view, viewport);
            let canvas_point_after = (center - pan1) / zoom1;

            assert!(
                (canvas_point_after - canvas_point_before).length() < 1e-3,
                "viewport-centre canvas point drifted for new_zoom {new_zoom}: {canvas_point_before:?} -> {canvas_point_after:?}"
            );
        }
    }

    /// For a bounds rect wider than tall, and one taller than wide, all
    /// four corners map strictly inside the viewport rect and the bounds
    /// centre maps to the viewport centre.
    #[test]
    fn fit_view_frames_every_corner_inside_the_viewport() {
        let viewport = egui::vec2(800.0, 600.0);
        let wide = egui::Rect::from_min_max(egui::pos2(-200.0, -20.0), egui::pos2(200.0, 20.0));
        let tall = egui::Rect::from_min_max(egui::pos2(-20.0, -200.0), egui::pos2(20.0, 200.0));

        for bounds in [wide, tall] {
            let view = fit_view(bounds, viewport);
            let (zoom, pan) = view_to_frame(view, viewport);

            for corner in [
                bounds.left_top(),
                bounds.right_top(),
                bounds.left_bottom(),
                bounds.right_bottom(),
            ] {
                let screen = corner.to_vec2() * zoom + pan;
                assert!(
                    screen.x > 0.0 && screen.x < viewport.x,
                    "corner {corner:?} -> screen {screen:?} outside viewport x for bounds {bounds:?}"
                );
                assert!(
                    screen.y > 0.0 && screen.y < viewport.y,
                    "corner {corner:?} -> screen {screen:?} outside viewport y for bounds {bounds:?}"
                );
            }

            let center_screen = bounds.center().to_vec2() * zoom + pan;
            let viewport_center = viewport / 2.0;
            assert!(
                (center_screen - viewport_center).length() < 1e-2,
                "bounds centre must map to viewport centre, got {center_screen:?} vs {viewport_center:?}"
            );
        }
    }

    /// Zero-area bounds (a single node, or an empty graph's collapsed rect)
    /// yield a finite `ViewState` with `zoom == 1.0` rather than infinity or
    /// NaN.
    #[test]
    fn fit_view_handles_degenerate_bounds() {
        let viewport = egui::vec2(800.0, 600.0);

        let single_point = egui::Rect::from_min_max(egui::pos2(5.0, 5.0), egui::pos2(5.0, 5.0));
        let view = fit_view(single_point, viewport);
        assert_eq!(view.zoom, 1.0);
        assert!(view.zoom.is_finite() && view.pan.x.is_finite() && view.pan.y.is_finite());

        let empty_bounds = egui::Rect::NOTHING;
        let view_empty = fit_view(empty_bounds, viewport);
        assert_eq!(view_empty.zoom, 1.0);
        assert!(
            view_empty.pan.x.is_finite() && view_empty.pan.y.is_finite(),
            "empty-graph collapsed rect must not produce a NaN/infinite pan"
        );
    }
}
