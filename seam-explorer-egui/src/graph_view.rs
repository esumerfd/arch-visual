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
/// Edge stroke alpha (0-255) -- always this value now that the reduced-
/// opacity focus fade (05-10) is gone; edges are either present (fully
/// visible at this alpha) or absent from the graph entirely, never faded.
const EDGE_ALPHA: u8 = 200;
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

/// NAV-04 focus-mode hiding (05-10 DP-10-01): true when nothing is focused,
/// or when `community` is either side of the focused seam. This is the
/// exact membership test the app's focus treatment already used (formerly
/// `node_opacity`'s full/dimmed split) -- NOT graph edge-reachability.
pub fn node_visible(
    community: &seam_core::CommunityId,
    focus: Option<&crate::app::FocusState>,
) -> bool {
    match focus {
        None => true,
        Some(f) => community == &f.a || community == &f.b,
    }
}

/// Pure structural mapping: `seam_core::Model` -> the `egui_graphs::Graph`
/// the widget consumes. Free of `egui::Ui`/`egui::Context` so
/// `unfocused_build_still_covers_every_node_and_edge`/
/// `test_render_mapping_is_scoped` can drive it directly. With `focus ==
/// None`, renders the **entire** graph unconditionally -- no node-count
/// perf safety-valve of any kind (Phase 2 D-01/D-02, ported unchanged;
/// DP-10-04: this filter is user-intent-driven, not node-count-driven).
/// With `focus == Some`, nodes failing `node_visible` are never added, and
/// any edge with at least one absent endpoint is never added (05-10
/// DP-10-02: hiding is filtered at graph construction, not at paint time,
/// so a hidden node cannot be hovered/clicked/dragged/traced).
pub fn build_graph(model: &seam_core::Model, focus: Option<&crate::app::FocusState>) -> SeamGraph {
    let mut g: SeamGraph = egui_graphs::Graph::new(petgraph::stable_graph::StableGraph::default());
    let mut index_map: std::collections::HashMap<
        petgraph::stable_graph::NodeIndex,
        petgraph::stable_graph::NodeIndex,
    > = std::collections::HashMap::new();

    for idx in model.graph.node_indices() {
        let node = &model.graph[idx];
        if !node_visible(&node.community, focus) {
            continue;
        }
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
        let (Some(&new_s), Some(&new_t)) = (index_map.get(&s), index_map.get(&t)) else {
            // At least one endpoint was excluded by `node_visible` above --
            // an edge can never dangle onto a node absent from `g`.
            continue;
        };
        let payload = PayloadEdge {
            source_community: model.graph[s].community.clone(),
            target_community: model.graph[t].community.clone(),
        };
        g.add_edge(new_s, new_t, payload);
    }

    g
}

/// 05-10 DP-10-03: the single place the three hiding-suspension conditions
/// live. Any call site that re-derives "should I hide" from `app.focus`
/// directly would drift from the trace suspension and silently break the
/// traced-path highlight -- this function is the only one allowed to make
/// that call. True only when all three hold: (a) a seam is focused --
/// nothing to hide against otherwise; (b) trace mode is off -- the user
/// needs the whole graph on screen to pick a drag source and target; (c)
/// no trace result is currently on screen -- a traced path routes through
/// arbitrary communities and `find_node_screen_pos` can only resolve hops
/// that exist in the graph, so hiding during a trace would silently draw a
/// broken polyline.
pub fn hiding_active(app: &SeamExplorerApp) -> bool {
    app.focus.is_some() && !app.trace_mode && app.trace.is_none()
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
/// focused, a distinguishing stroke for bridge nodes, and a truncated
/// label (full label shown on
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
/// edge is directed per Phase 1 D-09), and a per-direction crossing-count
/// label drawn only when a seam is focused (`apply_focus_styling` leaves
/// `label` empty otherwise). No fade treatment (05-10): an edge with an
/// endpoint outside the focused pair is excluded from the graph entirely by
/// `build_graph`, so every edge that reaches this shape is fully visible.
#[derive(Clone, Debug)]
pub struct SeamEdgeShape {
    #[allow(dead_code)]
    order: usize,
    #[allow(dead_code)]
    selected: bool,
    label_text: String,
    width: f32,
    tip_size: f32,
}

impl From<EdgeProps<PayloadEdge>> for SeamEdgeShape {
    fn from(props: EdgeProps<PayloadEdge>) -> Self {
        Self {
            order: props.order,
            selected: props.selected,
            label_text: props.label,
            width: 1.5,
            tip_size: 8.0,
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

        let base = hex(EDGE_HEX);
        let color = egui::Color32::from_rgba_unmultiplied(base.r(), base.g(), base.b(), EDGE_ALPHA);
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

/// Settled-bounds epsilon (canvas px) below which the refit follow (Plan 15)
/// considers the rendered bounds to have stopped moving. Planner probe
/// (`05-15-PLAN.md` `<design_decision>`): a 1.0px frame-to-frame delta is
/// reached around step 30 on both a 12-visible-node and a 176-visible-node
/// graph, with residual framing error under 4% of extent at that point --
/// well inside `FIT_VIEW_PADDING`. The delta plateaus rather than reaching
/// zero (0.24-0.60px indefinitely on the demo fixture), so
/// `FOLLOW_FRAME_CAP` below is a correctness requirement, not
/// belt-and-braces.
const FOLLOW_SETTLED_EPSILON: f32 = 1.0;
/// Hard frame cap on the refit follow -- three times the observed
/// convergence point (~30 frames), ~1.5s at 60fps. The follow's
/// frame-to-frame bounds delta never decays to zero (see
/// `FOLLOW_SETTLED_EPSILON`'s doc comment), so this is the load-bearing
/// termination guarantee, not a fallback -- it fires unconditionally,
/// regardless of what the bounds are doing.
const FOLLOW_FRAME_CAP: u32 = 90;
/// Pan takeover threshold (canvas px) -- deliberately far looser than
/// `PAN_EPSILON` below. `app.view` round-trips through
/// `view_to_frame`/`frame_to_view` every frame, and the f32 error on pan
/// magnitudes near 1e3 is of the same order as `PAN_EPSILON` (1e-3);
/// reusing `transform_differs` for the follow's takeover check would cancel
/// the follow on float noise rather than on a real user gesture.
const FOLLOW_PAN_TAKEOVER: f32 = 2.0;
/// Zoom takeover threshold (relative, i.e. 1%) -- same rationale as
/// `FOLLOW_PAN_TAKEOVER`.
const FOLLOW_ZOOM_TAKEOVER: f32 = 0.01;

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

/// Sensitivity applied to `smooth_scroll_delta.y` in `apply_scroll_zoom` --
/// calibrated so a single 3-line wheel notch (planner probe measured
/// `smooth_scroll_delta.y == 10.8`) lands at roughly a 1.24x step,
/// deliberately close to the keyboard scheme's 1.3x (`keyboard::ZOOM_FACTOR`)
/// so the two feel like one control.
const SCROLL_ZOOM_SENSITIVITY: f32 = 0.02;
/// Zoom clamp bounds (G-05-2 zoom half, T-05-08-02): no wheel input sequence
/// -- however fast the flick or however high the trackpad's sample rate --
/// can drive the transform to zero, infinity, or a scale where the
/// layout/overlay passes do unbounded work.
const MIN_ZOOM: f32 = 0.1;
const MAX_ZOOM: f32 = 10.0;

/// Divisor applied to `SCROLL_ZOOM_SENSITIVITY` for `ZoomSpeed::Slow`
/// (Plan 17, the user's request: "add a shift scroll wheel for slow zoom,
/// target half the speed of the current zoom"). Dividing the SENSITIVITY
/// (the exponent), not the resulting factor's distance from 1.0, is what
/// makes "half speed" compose exactly -- `factor_slow^2 == factor_fast` for
/// any delta -- and stay symmetric under zoom-in/zoom-out
/// (`05-17-PLAN.md` `<design_decision>` section 2).
const SLOW_ZOOM_DIVISOR: f32 = 2.0;

/// Selects `apply_scroll_zoom`'s sensitivity: `Normal` is today's plain
/// scroll/two-finger zoom speed, `Slow` is the Shift-held half-speed
/// modifier (Plan 17). Kept as a two-variant enum rather than a bare `bool`
/// or `f32` so the sensitivity mapping lives in exactly one place, next to
/// the constant it derives from, and so `ZoomSpeed::Normal`'s sensitivity
/// equaling `SCROLL_ZOOM_SENSITIVITY` is a one-line test that directly
/// enforces "plain scroll stays as it is" (`05-17-PLAN.md`
/// `<design_decision>` section 3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ZoomSpeed {
    Normal,
    Slow,
}

impl ZoomSpeed {
    /// `Normal` reproduces today's plain-scroll step size exactly; `Slow`
    /// (Plan 17, GREEN) halves the SENSITIVITY -- not the resulting factor's
    /// distance from 1.0 -- so `factor_slow^2 == factor_fast` for any delta
    /// and two Shift-held notches land on precisely the same zoom as one
    /// plain notch (`05-17-PLAN.md` `<design_decision>` section 2).
    fn sensitivity(self) -> f32 {
        match self {
            ZoomSpeed::Normal => SCROLL_ZOOM_SENSITIVITY,
            ZoomSpeed::Slow => SCROLL_ZOOM_SENSITIVITY / SLOW_ZOOM_DIVISOR,
        }
    }

    /// The factor-domain twin of `sensitivity()` (Plan 19): `Normal` is the
    /// identity, `Slow` is `factor.powf(1.0 / SLOW_ZOOM_DIVISOR)` -- deriving
    /// from the same `SLOW_ZOOM_DIVISOR` constant `sensitivity()` divides
    /// by, so "half speed" is defined exactly once no matter which domain a
    /// caller is working in. Two `Slow` applications compose to exactly one
    /// `Normal` application of the same factor
    /// (`slow.apply_to_factor(f).powi(2) == normal.apply_to_factor(f)`),
    /// the same identity 05-17 proved in the scroll-magnitude domain
    /// (`05-19-PLAN.md` `<design_decision>` section 4).
    ///
    pub fn apply_to_factor(self, factor: f32) -> f32 {
        match self {
            ZoomSpeed::Normal => factor,
            ZoomSpeed::Slow => factor.powf(1.0 / SLOW_ZOOM_DIVISOR),
        }
    }
}

/// Pure: a plain wheel/two-finger scroll's zoom step (G-05-2's zoom half --
/// `egui_graphs::handle_zoom` only reads `zoom_delta()`, populated
/// exclusively by a genuine pinch or Ctrl+scroll, so a plain scroll has zero
/// effect without this fallback), now CURSOR-ANCHORED (Plan 16): the canvas
/// point under `cursor` (the pointer's frame-local position, `viewport`'s
/// centre when there is none) before the change stays under it after --
/// matching `egui_graphs`' own `zoom()` on the Ctrl+scroll/pinch path, whose
/// `graph_center_pos = (center_pos - meta.pan) / meta.zoom` /
/// `pan_delta = graph_center_pos * (meta.zoom - new_zoom)` this function
/// re-expresses in this file's `view_to_frame` contract rather than
/// reinventing (`05-16-PLAN.md` `<design_decision>` sections 1-2). Solving
/// `view_to_frame`'s own `local = (canvas + view.pan - C) * view.zoom + C`
/// for the pan that keeps the canvas point under `cursor` fixed while zoom
/// moves from `view.zoom` to the clamped `zoom` yields the whole
/// implementation: `pan = view.pan + (cursor - C) * (1/zoom - 1/view.zoom)`.
///
/// The clamp is applied BEFORE this compensation, not after: writing the
/// delta against the requested (pre-clamp) zoom would make the term
/// non-zero exactly when the clamp refuses the zoom -- a pan with no zoom,
/// visible as slow sideways creep while the wheel is held at `MIN_ZOOM` or
/// `MAX_ZOOM` (`<design_decision>` section 3, T-05-16-02). Clamping first
/// makes the reciprocal difference identically zero at the boundary, so
/// there is no special case to write for the clamped case, the
/// cursor-at-centre case (`cursor - C == 0`), or a zero scroll (`zoom ==
/// view.zoom`) -- each falls out of the algebra as a zero term, which is
/// what lets `scroll_zoom_step_is_pure`'s old "pan must not touch pan"
/// assertion stay literally true at the centre without being relaxed.
///
/// Guarded against poisoning `app.view` (T-05-16-01): a non-finite or
/// non-positive incoming `view.zoom` returns `view` untouched, and a
/// non-finite computed pan (a non-finite `cursor`/`viewport`) is discarded
/// in favour of the incoming pan -- `app.view` is read back and re-written
/// every frame, so a single NaN written into it is not transient.
///
/// Takes a `speed` (Plan 17): `ZoomSpeed::Normal` reproduces today's step
/// size exactly (`ZoomSpeed::Normal.sensitivity() == SCROLL_ZOOM_SENSITIVITY`
/// is a pinned test); `ZoomSpeed::Slow` is the Shift-held half-speed
/// modifier. `scroll_y * speed.sensitivity()` is exponentiated into a
/// multiplicative factor and handed to `apply_zoom_factor` (Plan 19), which
/// owns the clamp ordering, cursor anchoring, and non-finite guards -- see
/// its own doc comment for those derivations; nothing about them changed
/// when they moved.
pub fn apply_scroll_zoom(
    view: crate::app::ViewState,
    scroll_y: f32,
    cursor: egui::Vec2,
    viewport: egui::Vec2,
    speed: ZoomSpeed,
) -> crate::app::ViewState {
    apply_zoom_factor(
        view,
        (scroll_y * speed.sensitivity()).exp(),
        cursor,
        viewport,
    )
}

/// Pure: the cursor-anchored, clamped, guarded zoom core (Plan 19),
/// extracted verbatim from `apply_scroll_zoom`'s body -- everything from
/// the clamp onward is unchanged in behaviour, only reachable now by a
/// multiplicative `factor` directly rather than exclusively through a
/// scroll magnitude. This is the entry point egui's own `zoom_delta()`
/// needs: egui hands the app an already-exponentiated factor for a genuine
/// pinch or Cmd/Ctrl+scroll (`05-19-PLAN.md` `<discovery_findings>` section
/// 2), and re-deriving a scroll magnitude from it would require inverting
/// egui's own exponent and re-exponentiating at the app's differently
/// calibrated sensitivity -- shown to overshoot straight to `MAX_ZOOM`
/// (`<design_decision>` section 3).
///
/// The canvas point under `cursor` (the pointer's frame-local position,
/// `viewport`'s centre when there is none) before the change stays under it
/// after -- matching `egui_graphs`' own `zoom()`, whose
/// `graph_center_pos = (center_pos - meta.pan) / meta.zoom` /
/// `pan_delta = graph_center_pos * (meta.zoom - new_zoom)` this function
/// re-expresses in this file's `view_to_frame` contract rather than
/// reinventing (`05-16-PLAN.md` `<design_decision>` sections 1-2). Solving
/// `view_to_frame`'s own `local = (canvas + view.pan - C) * view.zoom + C`
/// for the pan that keeps the canvas point under `cursor` fixed while zoom
/// moves from `view.zoom` to the clamped `zoom` yields the whole
/// implementation: `pan = view.pan + (cursor - C) * (1/zoom - 1/view.zoom)`.
///
/// The clamp is applied BEFORE this compensation, not after: writing the
/// delta against the requested (pre-clamp) zoom would make the term
/// non-zero exactly when the clamp refuses the zoom -- a pan with no zoom,
/// visible as slow sideways creep while held at `MIN_ZOOM` or `MAX_ZOOM`
/// (`05-16-PLAN.md` `<design_decision>` section 3, T-05-16-02). Clamping
/// first makes the reciprocal difference identically zero at the boundary,
/// so there is no special case to write for the clamped case, the
/// cursor-at-centre case (`cursor - C == 0`), or a factor of `1.0` (`zoom ==
/// view.zoom`) -- each falls out of the algebra as a zero term.
///
/// Guarded against poisoning `app.view`: a non-finite or non-positive
/// incoming `view.zoom` returns `view` untouched (T-05-16-01, inherited
/// from `apply_scroll_zoom`), a non-finite computed pan (a non-finite
/// `cursor`/`viewport`) is discarded in favour of the incoming pan
/// (inherited), and a non-finite or non-positive `factor` ALSO returns
/// `view` untouched -- new in this plan (T-05-19-03), because `f32::clamp`
/// propagates NaN rather than absorbing it and this entry point can receive
/// a factor straight from gesture input rather than from a
/// guaranteed-finite `exp()`. `app.view` is read back and re-written every
/// frame, so a single NaN written into it is not transient.
///
pub fn apply_zoom_factor(
    view: crate::app::ViewState,
    factor: f32,
    cursor: egui::Vec2,
    viewport: egui::Vec2,
) -> crate::app::ViewState {
    if !view.zoom.is_finite() || view.zoom <= 0.0 {
        return view;
    }
    if !factor.is_finite() || factor <= 0.0 {
        return view;
    }

    let zoom = (view.zoom * factor).clamp(MIN_ZOOM, MAX_ZOOM);

    let center = egui::Vec2::new(viewport.x / 2.0, viewport.y / 2.0);
    let offset = cursor - center;
    let pan = view.pan + offset * (1.0 / zoom - 1.0 / view.zoom);

    if pan.x.is_finite() && pan.y.is_finite() {
        crate::app::ViewState { zoom, pan }
    } else {
        crate::app::ViewState {
            zoom,
            pan: view.pan,
        }
    }
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

    // 05-22: same placement reasoning as `show_onboarding` above -- the
    // settings gear must exist on the "Load a graph.json to begin." screen
    // too (a first-time user wants to configure their editor before ever
    // loading a graph), so this call sits BEFORE the no-graph early return
    // below, not after it.
    crate::settings_panel::show(ui, ui.available_rect_before_wrap());

    let Some(model) = &app.model else {
        ui.centered_and_justified(|ui| {
            ui.label("Load a graph.json to begin.");
        });
        return;
    };

    // DP-10-03: hide only when the three-condition suspension rule allows
    // it -- see this function's own doc comment for why no other call site
    // is permitted to re-derive this decision.
    //
    // Plan 15: `render_focus` is an OWNED snapshot (not a borrow of
    // `app.focus`) precisely so it can be reused at the bottom of this
    // function, past several intervening `app.*` mutations, without
    // fighting the borrow checker over a live field borrow spanning a
    // whole-`app` reborrow. `refit_follow_step` below must arm from this
    // same value, not re-derive it from `app.focus` -- the two diverge
    // whenever trace mode or a trace result suspends hiding (DP-10-03), and
    // arming on `app.focus` alone would leave the camera framing a
    // two-community subset while the whole graph is on screen.
    let hiding = hiding_active(app);
    let render_focus: Option<crate::app::FocusState> =
        if hiding { app.focus.clone() } else { None };
    let mut graph = build_graph(model, render_focus.as_ref());
    apply_focus_styling(&mut graph, app);
    let canvas_rect = ui.available_rect_before_wrap();
    inject_layout_targets(ui, canvas_rect, &graph, app);
    // `canvas_rect` (not `response.rect`, unavailable until after `ui.add`
    // below) is the one viewport value used for both sync legs and the
    // Reset fit -- self-consistency of the centre term matters more than
    // which rect it came from (Plan 08 gap closure).
    let viewport = canvas_rect.size();
    sync_view_into_frame(ui, app.view, viewport);

    // TRACE-01/G-05-4: while trace mode is on, both flags below must move
    // together. `with_dragging_enabled` gates `egui_graphs`' own node-drag
    // reposition so the drag belongs entirely to `handle_trace_gesture`
    // below. `with_zoom_and_pan_enabled` (Plan 08 gap closure -- previously
    // hardcoded `true`, unlike this file's sibling flag) must be gated the
    // same way: in 0.31.0, disabling dragging also disables
    // `handle_node_drag` entirely, which is the crate's *only* writer of
    // `dragged_node()`, which is in turn `handle_pan`'s *only* guard against
    // claiming a drag as a canvas pan. So with pan left enabled during trace
    // mode, `dragged_node()` could never become `Some`, and the widget's own
    // internal pan handling claimed every primary-button drag -- including
    // one starting on a node -- before `handle_trace_gesture` ever ran that
    // frame. The reposition branch (trace mode off) and the trace branch
    // (trace mode on) stay mutually exclusive, matching the D3 original's
    // `dragTraceActive` two-branch split (RESEARCH Architecture Diagram,
    // "Drag gesture on a node").
    //
    // Plan 19 correction: this flag staying gated on trace mode ALSO
    // disables `egui_graphs`' own `handle_zoom` (the crate has exactly one
    // combined `zoom_and_pan_enabled` toggle, no zoom-only variant --
    // `05-19-PLAN.md` `<discovery_findings>` section 2), which is why a
    // genuine pinch or Cmd/Ctrl+scroll used to do nothing at all while
    // Trace mode was on. That flag is NOT re-enabled here -- doing so would
    // re-arm `handle_pan` and reintroduce the drag-steals-pan defect this
    // comment describes above. Instead the zoom half of that trade is
    // covered by the app's own branch near the end of this function, gated
    // on this exact same `native_zoom_and_pan` binding (read twice, never
    // two independent spellings of the same negation) so the two paths can
    // never both fire and can never drift apart.
    let native_zoom_and_pan = !app.trace_mode;
    let nav = egui_graphs::SettingsNavigation::new()
        .with_zoom_and_pan_enabled(native_zoom_and_pan)
        .with_fit_to_screen_enabled(false);
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
    #[cfg(test)]
    test_probe::publish_node_screen_positions(ui, &graph, response.rect);

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
            crate::overlay::paint_side_labels(ui, canvas_rect, response.rect, model, focus);
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

    // G-05-2 zoom half: a plain wheel/two-finger scroll never populates
    // `zoom_delta()` (only a genuine pinch or Ctrl+scroll does), so
    // `egui_graphs::handle_zoom` never sees it -- this fallback reads
    // `smooth_scroll_delta` directly. Guarded on all three of: the pointer
    // is over the canvas (so scrolling elsewhere in the app can't reach the
    // canvas), `zoom_delta() == 1.0` this frame (a genuine pinch/Ctrl+scroll
    // already applied via the widget above -- don't double-apply on top of
    // it), and the selected scroll magnitude being non-zero. Not gated on
    // `app.trace_mode` -- the wheel is not the primary-button drag, so it
    // cannot conflict with the trace gesture.
    //
    // Plan 17 -- Shift-held slow zoom, and the axis this MUST read. egui
    // 0.35's `InputOptions::horizontal_scroll_modifier` defaults to
    // `Modifiers::SHIFT` (`input_state/mod.rs`), and its wheel-state
    // (`input_state/wheel_state.rs`) REWRITES a Shift-held wheel delta onto
    // the HORIZONTAL axis before this code ever sees it: `.y` becomes
    // exactly `0.0` and the whole vertical magnitude moves onto `.x`, sign
    // preserved (verified empirically, 05-17-PLAN.md Task 1's SHIFT-PROBE:
    // plain=[0.0, 108.0], shift=[108.0, 0.0]). Reading only `.y` -- as this
    // branch did before this plan -- makes a Shift-held scroll do NOTHING
    // AT ALL, not slow zoom. So the magnitude is selected by the modifier,
    // never by "whichever component happens to be non-zero": with Shift
    // held, take `d.x + d.y` (egui's own fold already zeroed `.y`, so this
    // is just `.x`, but written as the sum so it stays correct if egui ever
    // changes which axis it folds into); without Shift, take `d.y`,
    // byte-identical to every plain scroll before this plan. Reading `.x`
    // unconditionally (instead of gating on the modifier) would make a
    // plain two-finger horizontal swipe zoom the canvas -- a change to
    // default behaviour this plan forbids. The speed is built from the same
    // modifier boolean that selects the axis, so the two can never
    // disagree (`05-17-PLAN.md` `<design_decision>` section 1).
    //
    // Two accepted consequences of relying on egui's own axis fold rather
    // than reimplementing it (T-05-17-01/T-05-17-03, not mitigated further):
    // (1) egui has already merged "Shift + vertical wheel" and "Shift +
    // genuine horizontal swipe" into the same delta by the time this code
    // runs, so both zoom -- a user holding Shift over the canvas is asking
    // to zoom. (2) `i.modifiers.shift` reflects the CURRENT modifier state
    // while `smooth_scroll_delta` may be a remainder egui is still draining
    // across several frames; releasing Shift mid-flick leaves that
    // remainder sitting on `.x`, where the non-Shift path does not read it,
    // so the zoom stops early rather than finishing at full speed. That is
    // the defensible behaviour -- do not add machinery to latch the
    // modifier.
    //
    // Plan 19: `zoom_delta` itself (not just the identity boolean derived
    // from it) is kept, so the new trace-mode zoom branch below can read
    // the exact factor egui computed for the gesture rather than a second,
    // possibly-differently-timed read of `ui.input`. `cursor` and `speed`
    // are hoisted above both branches (rather than recomputed inside each)
    // so there is exactly one definition of each and the cursor anchoring
    // is identical on both paths.
    let zoom_delta = ui.input(|i| i.zoom_delta());
    let zoom_delta_is_identity = zoom_delta == 1.0;
    let (scroll_delta, shift_held) = ui.input(|i| (i.smooth_scroll_delta(), i.modifiers.shift));
    let scroll_magnitude = if shift_held {
        scroll_delta.x + scroll_delta.y
    } else {
        scroll_delta.y
    };
    let speed = if shift_held {
        ZoomSpeed::Slow
    } else {
        ZoomSpeed::Normal
    };
    // Plan 16: the cursor's FRAME-LOCAL position, matching what
    // egui_graphs' own `handle_zoom` reads for the identical purpose
    // (`local_pos` there subtracts `resp.rect.left_top()`). Must be
    // `response.rect`, not `canvas_rect` -- the same offset `to_screen`
    // adds back a few lines above and what egui_graphs' `local_pos`
    // subtracts (05-16-PLAN.md frontmatter key link). Falls back to the
    // viewport centre when there is no hover position, reproducing today's
    // centre-anchored behaviour rather than skipping the zoom.
    let cursor = ui
        .input(|i| i.pointer.hover_pos())
        .map(|p| p - response.rect.left_top())
        .unwrap_or(viewport / 2.0);

    if response.contains_pointer() && zoom_delta_is_identity && scroll_magnitude != 0.0 {
        app.view = apply_scroll_zoom(app.view, scroll_magnitude, cursor, viewport, speed);
    }

    // Plan 19: with Trace mode on, `egui_graphs`' own navigation is
    // disabled (`native_zoom_and_pan` above), so a genuine pinch or
    // Cmd/Ctrl+scroll's `zoom_delta` is never consumed by anybody --
    // `05-19-PLAN.md` `<discovery_findings>` sections 1-2, confirmed by a
    // planning-time probe against the real crate. It is NOT reachable via
    // the plain-scroll branch above: egui 0.35 zeroes
    // `smooth_scroll_delta` entirely whenever the zoom modifier is held
    // (`<discovery_findings>` section 2), so `scroll_magnitude` is exactly
    // `0.0` on every frame of this gesture and that branch's own guard
    // correctly declines, no matter how it is retuned.
    //
    // Gated on `!native_zoom_and_pan` (the SAME binding that disabled the
    // widget's navigation above, not a second spelling of `app.trace_mode`)
    // because when navigation is enabled, `egui_graphs::handle_zoom` has
    // ALREADY consumed this exact `zoom_delta` inside `ui.add()` earlier in
    // this same frame -- applying it again here would double-apply the
    // gesture. Mutually exclusive with the plain-scroll branch above by
    // construction: that branch requires the identity case
    // (`zoom_delta == 1.0`), this one requires its negation.
    //
    // The factor is applied EXACTLY as egui computed it -- `apply_zoom_factor`
    // takes `zoom_delta` (through `speed.apply_to_factor`, for Shift-held
    // half-speed) directly rather than re-deriving a scroll magnitude from
    // it, which `05-19-PLAN.md` `<design_decision>` section 3 shows would
    // require inverting egui's own exponent and re-exponentiating at this
    // app's differently-calibrated sensitivity -- overshooting straight to
    // `MAX_ZOOM` for a single wheel notch. `apply_zoom_factor` is the same
    // cursor-anchored, clamped, guarded core the plain-scroll branch above
    // uses -- not a second implementation of the zoom algebra.
    if response.contains_pointer() && !zoom_delta_is_identity && !native_zoom_and_pan {
        app.view = apply_zoom_factor(
            app.view,
            speed.apply_to_factor(zoom_delta),
            cursor,
            viewport,
        );
    }

    refit_follow_step(ui, &graph, viewport, render_focus.as_ref(), app);
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

/// Test-only position probe (Task 1, G-05-5): publishes this frame's
/// `(node id, screen position)` pairs into egui temp memory, using the same
/// `MetadataFrame` + `graph_rect` pairing `find_node_screen_pos` already
/// uses, so a synthetic-drag test can learn where the rendered nodes
/// actually are without any access to `handle_trace_gesture`'s internals.
/// Module-private and `#[cfg(test)]`-gated -- compiles out of the shipped
/// binary entirely.
#[cfg(test)]
mod test_probe {
    use super::*;

    fn positions_id() -> egui::Id {
        egui::Id::new("seam_explorer_test_node_screen_positions")
    }

    /// Publishes this frame's node-id -> screen-position pairs, loading a
    /// fresh `MetadataFrame` and mapping every node via the same `to_screen`
    /// conversion `find_node_screen_pos` uses.
    pub fn publish_node_screen_positions(
        ui: &mut egui::Ui,
        graph: &SeamGraph,
        graph_rect: egui::Rect,
    ) {
        let meta = egui_graphs::MetadataFrame::new(None).load(ui);
        let positions: Vec<(String, egui::Pos2)> = graph
            .nodes_iter()
            .map(|(_, n)| {
                (
                    n.payload().id.clone(),
                    to_screen(&meta, graph_rect, n.location()),
                )
            })
            .collect();
        ui.data_mut(|d| d.insert_temp(positions_id(), positions));
    }

    /// Loads the most recently published position vector, defaulting to
    /// empty if nothing has been published yet.
    pub fn load_node_screen_positions(ui: &egui::Ui) -> Vec<(String, egui::Pos2)> {
        ui.data(|d| d.get_temp(positions_id())).unwrap_or_default()
    }
}

/// Test-only last-argv probe (05-23, alongside `test_probe` above, same
/// `#[cfg(test)]`-gated / module-private shape). Under `#[cfg(test)]`,
/// Task 2's live context-menu wiring records here, at its spawn site, the
/// argv it WOULD launch, instead of calling `open_file::spawn`, so
/// `activating_open_file_spawns_the_configured_argv` can assert on the
/// launch without actually launching an editor. Takes `&egui::Context`
/// (not `&mut egui::Ui`, unlike `test_probe`'s functions) so a test can
/// read it straight off `harness.ctx` after `step()` with no render-closure
/// mirror needed. In a release build this module doesn't exist at all --
/// the argv-to-process link itself is covered by 05-21's own real-process
/// tests (`open_file::spawn`'s `spawn_launches_a_real_process_and_returns_ok`)
/// plus this plan's human-check, not by anything here.
#[cfg(test)]
mod argv_probe {
    fn argv_id() -> egui::Id {
        egui::Id::new("seam_explorer_test_last_spawned_argv")
    }

    /// Records the argv that would have been spawned this frame.
    pub fn record(ctx: &egui::Context, argv: Vec<String>) {
        ctx.data_mut(|d| d.insert_temp(argv_id(), argv));
    }

    /// Loads the most recently recorded argv, if any.
    pub fn load(ctx: &egui::Context) -> Option<Vec<String>> {
        ctx.data(|d| d.get_temp(argv_id()))
    }
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
///
/// G-05-5 correction (Plan 13 gap closure): the `DragStart` branch used to
/// hit-test `response.interact_pointer_pos()` at the frame `drag_started()`
/// first became true. egui 0.35's `Sense::click_and_drag()` deliberately
/// withholds `drag_started()`/`interact_pointer_pos()` until the pointer has
/// moved further than `InputOptions::max_click_dist` (6.0pt, the built-in
/// click-vs-drag disambiguation threshold) from the actual mouse-down
/// point -- the same order of magnitude as `SeamNodeShape::NODE_RADIUS`
/// (6.0 canvas units) -- so that position had, by construction, already
/// left the node the drag actually started on; the hit-test missed every
/// time, deterministically. The pressed node is now captured on the
/// undelayed pointer-down frame (`response.is_pointer_button_down_on()` +
/// `response.hover_pos()`, persisted via `trace::PressCapture`) and read
/// back here, mirroring `egui_graphs::GraphView::handle_node_drag`'s own
/// working pattern for exactly this situation.
///
/// 05-18 button policy: the drag-START (and the press capture that feeds
/// it) is gated on `trace::TRACE_BUTTONS` -- Primary and Secondary only,
/// named and tested in `trace.rs` rather than left as an emergent property
/// of egui's button-agnostic drag machinery. The drag-MOVE and drag-STOP
/// arms below are deliberately left button-blind: `Response::drag_stopped_by(b)`
/// requires an actual button release, but egui also synthesises
/// `drag_stopped` with NO release at all when Escape is pressed mid-drag
/// (`interaction.rs:137-141`). Filtering the stop by button would leave an
/// Escape-aborted gesture stuck in `Dragging` forever, painting a rubber
/// band with no way to clear it -- strictly worse than not filtering at
/// all. Filtering the start alone is sufficient: a non-trace-button drag
/// never reaches `Dragging`, so its later move/stop inputs arrive with the
/// state still `Idle`, where `update_gesture`'s final `(state, _) => state`
/// arm is a no-op. Do not "finish the job" by adding `drag_stopped_by`
/// here -- see `<threat_model>` T-05-18-01 in 05-18-PLAN.md.
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
        crate::trace::save_press_capture(ui, None);
        return;
    }

    let meta = egui_graphs::MetadataFrame::new(None).load(ui);
    let graph_rect = response.rect;

    // Whether a trace button (`trace::TRACE_BUTTONS`) is currently down,
    // asked of the input state directly rather than the response -- the
    // response's own accessors answer "is this widget being interacted
    // with", a different question from "which button is physically down
    // right now". Consulted by both the press-capture guard and its clear
    // condition below, so the two halves can never disagree.
    let any_trace_button_down = ui.input(|i| {
        crate::trace::TRACE_BUTTONS
            .iter()
            .any(|&b| i.pointer.button_down(b))
    });

    // Pointer-down capture: while a trace button is down on this widget
    // and nothing is recorded yet, hit-test the current (undelayed)
    // pointer position and record a hit. Guarding on "nothing recorded
    // yet" makes this a first-true capture rather than a per-frame
    // overwrite, so the node cannot be swapped mid-drag as the cursor
    // passes over others.
    let mut capture = crate::trace::load_press_capture(ui);
    if response.is_pointer_button_down_on() && any_trace_button_down && capture.is_none() {
        if let Some((node, pos)) = response
            .hover_pos()
            .and_then(|p| hit_test_node(graph, &meta, graph_rect, p))
        {
            capture = Some(crate::trace::PressCapture { node, pos });
            crate::trace::save_press_capture(ui, capture.clone());
        }
    }

    let input = if crate::trace::TRACE_BUTTONS
        .iter()
        .any(|&b| response.drag_started_by(b))
    {
        // The node comes from the recorded capture; `press_origin()` --
        // also undelayed -- is the only fallback, for a capture that
        // somehow never recorded a hit. The cursor stays the live pointer
        // position, so the rubber band follows the mouse rather than
        // snapping back to the press point for a frame.
        let node = capture.as_ref().map(|c| c.node.clone()).or_else(|| {
            ui.input(|i| i.pointer.press_origin())
                .and_then(|p| hit_test_node(graph, &meta, graph_rect, p))
                .map(|(id, _)| id)
        });
        let cursor = response
            .interact_pointer_pos()
            .or_else(|| ui.input(|i| i.pointer.hover_pos()));
        node.zip(cursor)
            .map(|(node, cursor)| crate::trace::GestureInput::DragStart { node, cursor })
    } else if response.dragged() {
        // Deliberately button-blind -- see this function's doc comment.
        response.interact_pointer_pos().map(|p| {
            let snapped = hit_test_node(graph, &meta, graph_rect, p).map(|(_, screen)| screen);
            crate::trace::GestureInput::DragMove {
                cursor: snapped.unwrap_or(p),
            }
        })
    } else if response.drag_stopped() {
        // Deliberately button-blind -- see this function's doc comment.
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

    // Clear the capture once no trace button is down on this widget --
    // covers both a completed drag and an abandoned press, and (05-18)
    // also a capture whose button was never a trace button to begin with.
    // Must run after `input` above, since the `DragStart` arm reads the
    // capture.
    if !response.is_pointer_button_down_on() || !any_trace_button_down {
        crate::trace::save_press_capture(ui, None);
    }

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
        // Full-strength side tint (or the default fog fill when unfocused)
        // -- 05-10 removed the reduced-opacity fade entirely; a node that
        // fails `node_visible` is now absent from `graph` altogether
        // (`build_graph`), so every node reaching this pass is already
        // known-visible and never needs a dimmed fill.
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
            n.set_color(base);
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

/// Detects that `app.view` just became exactly `ViewState::default()` --
/// i.e. an actual reset request (the top bar's "Reset view" button, or the
/// `0` key via `reset_view`, both of which set that exact value), one of
/// `refit_follow_step`'s two arming triggers (Plan 15; the other is
/// `render_focus_changed` below).
///
/// Deliberately fires only when `app.view` just *became* the default value,
/// not on every change (Rule 1, unchanged since Plan 08): mouse/keyboard
/// pan/zoom mutate `app.view` every frame via `show()`'s sync legs above --
/// treating every nudge as "changed" would re-arm the follow on every
/// pan/zoom, which is a regression, not a no-op. Narrowing the trigger to
/// "became default" preserves the original reset-detection intent (both the
/// button and `0` set exactly `ViewState::default()` to signal "reset
/// requested").
fn reset_sentinel_fired(ui: &mut egui::Ui, app: &SeamExplorerApp) -> bool {
    let id = egui::Id::new("seam_explorer_graph_view_snapshot");
    let current = (app.view.zoom, app.view.pan);
    let default_view = crate::app::ViewState::default();
    let default = (default_view.zoom, default_view.pan);
    ui.data_mut(|d| {
        let prev: Option<(f32, egui::Vec2)> = d.get_temp(id);
        d.insert_temp(id, current);
        current == default && prev.is_some_and(|p| p != current)
    })
}

/// Bounding rect of every node's current `location()` in `graph` -- `None`
/// for an empty graph. Reads the ALREADY-FILTERED graph `show()` built --
/// 05-10 excludes unfocused communities structurally -- so a focused fit
/// frames the focused pair only, never the whole model. Must be called
/// after `ui.add` has returned so positions reflect this frame's easing
/// step (lifted from the bounding-rect loop the prior `detect_reset`
/// inlined -- same read, same timing requirement).
fn rendered_bounds(graph: &SeamGraph) -> Option<egui::Rect> {
    if graph.node_count() == 0 {
        return None;
    }
    let mut bounds = egui::Rect::NOTHING;
    for (_, node) in graph.nodes_iter() {
        bounds.extend_with(node.location());
    }
    Some(bounds)
}

/// True when the larger of the two bounds rects' corner movements (min
/// corner, max corner) is below `FOLLOW_SETTLED_EPSILON` -- the refit
/// follow's convergence test. Tolerates a non-finite or empty
/// `previous`/`current` rect by reporting not-settled rather than
/// panicking, so the first comparison after arming (with no real previous
/// bounds yet) can never short-circuit the follow.
fn bounds_settled(previous: egui::Rect, current: egui::Rect) -> bool {
    let finite = |r: egui::Rect| {
        r.min.x.is_finite() && r.min.y.is_finite() && r.max.x.is_finite() && r.max.y.is_finite()
    };
    if !finite(previous) || !finite(current) {
        return false;
    }
    let min_delta = (current.min - previous.min).length();
    let max_delta = (current.max - previous.max).length();
    min_delta.max(max_delta) < FOLLOW_SETTLED_EPSILON
}

/// True when `current` has diverged from `written` (the view the refit
/// follow last wrote) by more than `FOLLOW_PAN_TAKEOVER`/
/// `FOLLOW_ZOOM_TAKEOVER` -- the user-takeover test. See those constants'
/// doc comments for why the thresholds are deliberately looser than this
/// file's steady-state sync epsilons (`PAN_EPSILON`/`ZOOM_EPSILON`).
fn user_took_over(written: crate::app::ViewState, current: crate::app::ViewState) -> bool {
    let pan_delta = (current.pan - written.pan).length();
    let zoom_rel_delta = (current.zoom - written.zoom).abs() / written.zoom.max(1e-6);
    pan_delta > FOLLOW_PAN_TAKEOVER || zoom_rel_delta > FOLLOW_ZOOM_TAKEOVER
}

/// Persisted refit-follow state (Plan 15): how many frames it has been
/// running, the bounds it fit last frame (for the convergence comparison),
/// and the view it last wrote (for the user-takeover comparison). `None`
/// fields mean "armed this frame, not written yet" -- a follow's very first
/// step has nothing to compare bounds/takeover against, so both checks are
/// skipped until after the first write. `Clone`/`Copy`/`Debug` so it can
/// live in egui temp data, the same storage `reset_sentinel_fired` and
/// `trace.rs`'s `PressCapture` already use for per-frame state with nowhere
/// to live on the frozen `SeamExplorerApp`.
#[derive(Clone, Copy, Debug)]
struct RefitFollowState {
    frame: u32,
    bounds: Option<egui::Rect>,
    written_view: Option<crate::app::ViewState>,
}

fn refit_follow_id() -> egui::Id {
    egui::Id::new("seam_explorer_refit_follow")
}

/// Loads the currently active refit follow, if any. Flattens the stored
/// `Option` (never written vs. explicitly cleared both read as `None`),
/// mirroring `trace::load_press_capture`'s discipline.
fn load_refit_follow(ui: &egui::Ui) -> Option<RefitFollowState> {
    ui.data(|d| d.get_temp(refit_follow_id())).flatten()
}

/// Records or clears the refit follow. Writing `None` is the clear -- a
/// single setter for both record and clear, avoiding egui's `remove_temp`
/// API (which additionally requires `T: Default`), the same reasoning
/// `trace::save_press_capture`'s doc comment gives.
fn save_refit_follow(ui: &mut egui::Ui, state: Option<RefitFollowState>) {
    ui.data_mut(|d| d.insert_temp(refit_follow_id(), state));
}

/// Snapshots the render-focus value handed to `build_graph` this frame
/// (`hiding_active(app) ? app.focus : None`, computed once in `show()` and
/// passed in here as `render_focus`) into its own temp-data slot, reporting
/// whether it differs from the previous frame's snapshot -- one of
/// `refit_follow_step`'s two arming triggers. Uses the same "previous
/// exists AND differs" shape `reset_sentinel_fired` uses for its sentinel,
/// so the very first frame arms nothing (there is no previous snapshot to
/// differ from yet). Covers a seam being focused, a different seam being
/// clicked while one is already focused, focus being cleared, Trace mode
/// being toggled while a seam is focused, and a trace result arriving or
/// clearing -- all of those change `render_focus`'s value, because all of
/// those change what `build_graph` actually renders.
fn render_focus_changed(ui: &mut egui::Ui, render_focus: Option<&crate::app::FocusState>) -> bool {
    let id = egui::Id::new("seam_explorer_refit_follow_render_focus");
    let current: Option<crate::app::FocusState> = render_focus.cloned();
    ui.data_mut(|d| {
        let prev: Option<Option<crate::app::FocusState>> = d.get_temp(id);
        d.insert_temp(id, current.clone());
        prev.is_some_and(|p| p != current)
    })
}

/// The per-frame refit-follow step (Plan 15, NAV-02/NAV-04 combined):
/// closes the user's "I need to press reset view to get it centered. Can
/// these be combined." gap by re-framing the canvas whenever the rendered
/// node set changes (armed by `render_focus_changed`) or an explicit reset
/// is requested (armed by `reset_sentinel_fired`), and by continuing to
/// re-fit every frame while the pull-apart layout is still easing toward
/// its targets -- tracking the animation to rest instead of fitting once
/// against positions that haven't finished moving (see `05-15-PLAN.md`'s
/// `<design_decision>` for the measured evidence this design is based on).
///
/// Called from `show()` at the exact position the old `detect_reset` used
/// to occupy -- the last statement, after the frame read-back leg and after
/// the scroll-zoom fallback. Anything earlier and the read-back leg
/// overwrites the fit with the pre-fit frame on the same frame.
///
/// Order of operations: arm (replacing any in-flight follow) if either
/// trigger fired; return if no follow is live; if the follow has already
/// written at least once and the current `app.view` has diverged from what
/// it wrote, clear the follow and return -- the user has taken over, and
/// the fit must not fight a live gesture; get the rendered bounds, clearing
/// the follow and returning if there are none (an empty graph re-frames
/// nothing); assign `app.view` from `fit_view`; then decide whether to
/// continue -- stop (clearing the follow) if the bounds were settled
/// relative to the previous frame's, stop unconditionally if the frame
/// counter has reached `FOLLOW_FRAME_CAP` regardless of what the bounds are
/// doing, otherwise persist the advanced state with this frame's bounds and
/// this frame's written view. Applying the fit before the stop decision
/// matters: the frame the bounds settle on is still a frame worth framing.
fn refit_follow_step(
    ui: &mut egui::Ui,
    graph: &SeamGraph,
    viewport: egui::Vec2,
    render_focus: Option<&crate::app::FocusState>,
    app: &mut SeamExplorerApp,
) {
    let armed_by_focus_change = render_focus_changed(ui, render_focus);
    let armed_by_reset = reset_sentinel_fired(ui, app);

    let mut follow = load_refit_follow(ui);
    if armed_by_focus_change || armed_by_reset {
        follow = Some(RefitFollowState {
            frame: 0,
            bounds: None,
            written_view: None,
        });
    }

    let Some(mut state) = follow else {
        return;
    };

    if let Some(written) = state.written_view {
        if user_took_over(written, app.view) {
            save_refit_follow(ui, None);
            return;
        }
    }

    let Some(bounds) = rendered_bounds(graph) else {
        save_refit_follow(ui, None);
        return;
    };

    app.view = fit_view(bounds, viewport);
    let settled = state
        .bounds
        .map(|previous| bounds_settled(previous, bounds))
        .unwrap_or(false);
    state.frame += 1;

    if settled || state.frame >= FOLLOW_FRAME_CAP {
        save_refit_follow(ui, None);
    } else {
        state.bounds = Some(bounds);
        state.written_view = Some(app.view);
        save_refit_follow(ui, Some(state));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CLEAN_FIXTURE: &str = include_str!("../../seam-core/tests/fixtures/clean.json");

    /// A node whose community is either side of the focused seam is
    /// visible; a third community is not (05-10 DP-10-01: hiding uses the
    /// same community-membership test the focus treatment already used).
    #[test]
    fn node_visible_keeps_both_focused_sides() {
        let a: seam_core::CommunityId = "A".to_string();
        let b: seam_core::CommunityId = "B".to_string();
        let c: seam_core::CommunityId = "C".to_string();
        let focus = crate::app::FocusState {
            a: a.clone(),
            b: b.clone(),
        };

        assert!(node_visible(&a, Some(&focus)));
        assert!(node_visible(&b, Some(&focus)));
        assert!(!node_visible(&c, Some(&focus)));
    }

    /// With no focus, every community is visible.
    #[test]
    fn node_visible_keeps_everything_when_nothing_is_focused() {
        let a: seam_core::CommunityId = "A".to_string();
        let b: seam_core::CommunityId = "B".to_string();
        let c: seam_core::CommunityId = "C".to_string();

        assert!(node_visible(&a, None));
        assert!(node_visible(&b, None));
        assert!(node_visible(&c, None));
    }

    /// Building from a three-community model with a focus on two of them
    /// yields a graph whose node count equals only those two communities'
    /// nodes -- strictly less than the model's total node count.
    #[test]
    fn focused_build_excludes_nodes_outside_the_pair() {
        let ingest = seam_core::from_json(CLEAN_FIXTURE).expect("clean fixture must ingest");
        let model = ingest.model;
        let focus = crate::app::FocusState {
            a: "A".to_string(),
            b: "B".to_string(),
        };
        let graph = build_graph(&model, Some(&focus));

        let expected = model
            .graph
            .node_indices()
            .filter(|&idx| {
                let community = &model.graph[idx].community;
                community == "A" || community == "B"
            })
            .count();
        assert_eq!(graph.node_count(), expected);
        assert!(
            graph.node_count() < model.graph.node_count(),
            "the focused build must exclude at least the third community's nodes"
        );
    }

    /// The same focused build yields no edge touching the excluded
    /// community, and still yields the edges between the two kept
    /// communities.
    #[test]
    fn focused_build_excludes_edges_with_an_absent_endpoint() {
        let ingest = seam_core::from_json(CLEAN_FIXTURE).expect("clean fixture must ingest");
        let model = ingest.model;
        let focus = crate::app::FocusState {
            a: "A".to_string(),
            b: "B".to_string(),
        };
        let graph = build_graph(&model, Some(&focus));

        for (_, edge) in graph.edges_iter() {
            let payload = edge.payload();
            assert!(
                payload.source_community == "A" || payload.source_community == "B",
                "edge source community {:?} must be one of the focused pair",
                payload.source_community
            );
            assert!(
                payload.target_community == "A" || payload.target_community == "B",
                "edge target community {:?} must be one of the focused pair",
                payload.target_community
            );
        }
        assert!(
            graph.edge_count() > 0,
            "the focused pair A/B must still keep the edges between them"
        );
    }

    /// Replaces the old full-coverage test; asserts node count and edge
    /// count both equal the model's when nothing is focused, guarding
    /// DP-10-04 (no perf safety-valve).
    #[test]
    fn unfocused_build_still_covers_every_node_and_edge() {
        let ingest = seam_core::from_json(CLEAN_FIXTURE).expect("clean fixture must ingest");
        let model = ingest.model;
        let graph = build_graph(&model, None);
        assert_eq!(graph.node_count(), model.graph.node_count());
        assert_eq!(graph.edge_count(), model.graph.edge_count());
    }

    // ============================================================
    // 05-10 Task 2 (DP-10-03): the single three-condition suspension rule.
    // ============================================================

    fn focus_state() -> crate::app::FocusState {
        crate::app::FocusState {
            a: "A".to_string(),
            b: "B".to_string(),
        }
    }

    /// DP-10-03 case (a): with no focus, there is nothing to hide against.
    #[test]
    fn hiding_is_inactive_without_a_focus() {
        let app = crate::app::SeamExplorerApp::default();
        assert!(!hiding_active(&app));
    }

    /// A focus alone -- trace mode off, no trace result -- activates hiding.
    #[test]
    fn hiding_is_active_with_a_focus_alone() {
        let app = crate::app::SeamExplorerApp {
            focus: Some(focus_state()),
            ..Default::default()
        };
        assert!(hiding_active(&app));
    }

    /// DP-10-03 case (b): trace mode on suspends hiding even with a seam
    /// focused -- the user needs the whole graph on screen to pick a drag
    /// source and target.
    #[test]
    fn hiding_is_suspended_while_trace_mode_is_on() {
        let app = crate::app::SeamExplorerApp {
            focus: Some(focus_state()),
            trace_mode: true,
            ..Default::default()
        };
        assert!(!hiding_active(&app));
    }

    /// DP-10-03 case (c): a resolved trace result suspends hiding -- the
    /// traced path routes through arbitrary communities and
    /// `find_node_screen_pos` can only resolve hops that exist in the
    /// graph, so hiding during a trace would silently draw a broken
    /// polyline.
    #[test]
    fn hiding_is_suspended_while_a_trace_result_is_present() {
        let app = crate::app::SeamExplorerApp {
            focus: Some(focus_state()),
            trace: Some(crate::trace::TraceResult {
                from: "a1".to_string(),
                to: "c1".to_string(),
                path: Some(seam_core::TracePath {
                    hops: vec!["a1".to_string(), "b1".to_string(), "c1".to_string()],
                    seams_crossed: vec![],
                }),
            }),
            ..Default::default()
        };
        assert!(!hiding_active(&app));
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
        let graph = build_graph(&model, None);
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

    // ============================================================
    // Plan 08 gap closure (G-05-2 zoom half): apply_scroll_zoom.
    // ============================================================

    /// Positive/negative/zero `scroll_y` behaviour plus both clamp bounds,
    /// all exercised with the cursor at the viewport centre -- Plan 16 makes
    /// that cursor position explicit (an added parameter) rather than
    /// implied by the old signature's total absence of one; the assertion
    /// that pan is untouched stays literally true at the centre because
    /// centre-anchored zoom is the cursor-at-centre special case of the
    /// wider cursor-anchored contract Plan 16 introduces.
    #[test]
    fn scroll_zoom_step_is_pure() {
        let viewport = egui::vec2(800.0, 600.0);
        let centre_cursor = viewport / 2.0;
        let base = crate::app::ViewState {
            zoom: 1.0,
            pan: egui::vec2(12.0, -8.0),
        };

        let zoomed_in = apply_scroll_zoom(base, 3.0, centre_cursor, viewport, ZoomSpeed::Normal);
        assert!(
            zoomed_in.zoom > base.zoom,
            "positive scroll_y must increase zoom"
        );
        assert_eq!(
            zoomed_in.pan, base.pan,
            "apply_scroll_zoom must not touch pan"
        );

        let zoomed_out = apply_scroll_zoom(base, -3.0, centre_cursor, viewport, ZoomSpeed::Normal);
        assert!(
            zoomed_out.zoom < base.zoom,
            "negative scroll_y must decrease zoom"
        );

        let identity = apply_scroll_zoom(base, 0.0, centre_cursor, viewport, ZoomSpeed::Normal);
        assert!(
            (identity.zoom - base.zoom).abs() < 1e-6,
            "zero scroll_y must be the identity"
        );

        let clamped_low =
            apply_scroll_zoom(base, -1000.0, centre_cursor, viewport, ZoomSpeed::Normal);
        assert_eq!(
            clamped_low.zoom, MIN_ZOOM,
            "an extreme negative scroll must clamp at MIN_ZOOM"
        );

        let clamped_high =
            apply_scroll_zoom(base, 1000.0, centre_cursor, viewport, ZoomSpeed::Normal);
        assert_eq!(
            clamped_high.zoom, MAX_ZOOM,
            "an extreme positive scroll must clamp at MAX_ZOOM"
        );
    }

    // ============================================================
    // Plan 16 (RED): cursor-anchored scroll zoom. `apply_scroll_zoom` now
    // takes the cursor's frame-local position and the viewport; these tests
    // pin the anchor invariant `05-16-PLAN.md` `<design_decision>` section 2
    // derives, expressed in the renderer's own coordinate space via
    // `view_to_frame` rather than a transcribed copy of the algebra.
    // ============================================================

    /// Maps a canvas-space point to its frame-local screen position for a
    /// given `view`/`viewport`, via `view_to_frame` -- not a hand-copied
    /// formula, so this helper (and its inverse below) assert the anchor
    /// property in exactly the space the widget renders in.
    fn canvas_to_frame_local(
        canvas: egui::Vec2,
        view: crate::app::ViewState,
        viewport: egui::Vec2,
    ) -> egui::Vec2 {
        let (zoom, pan) = view_to_frame(view, viewport);
        canvas * zoom + pan
    }

    /// Exact inverse of `canvas_to_frame_local`, also via `view_to_frame`.
    fn frame_local_to_canvas(
        local: egui::Vec2,
        view: crate::app::ViewState,
        viewport: egui::Vec2,
    ) -> egui::Vec2 {
        let (zoom, pan) = view_to_frame(view, viewport);
        (local - pan) / zoom
    }

    /// The anchor invariant, as a matrix: for a spread of starting zooms
    /// (well below and well above 1.0), cursor positions (off-centre in
    /// each quadrant, plus one near an edge), scroll deltas in both
    /// directions, and BOTH `ZoomSpeed` variants (Plan 17: the added
    /// dimension, per `05-17-PLAN.md`'s instruction to extend this matrix
    /// rather than duplicate its body for the Slow-speed invariant coverage)
    /// -- the canvas-space point currently under the cursor, mapped forward
    /// through the POST-zoom transform, lands back on the same cursor
    /// position within a sub-pixel tolerance.
    #[test]
    fn scroll_zoom_anchors_the_canvas_point_under_the_cursor() {
        let viewport = egui::vec2(800.0, 600.0);
        let zooms = [0.2, 0.5, 1.0, 2.0, 5.0];
        let cursors = [
            egui::vec2(120.0, 90.0),  // top-left quadrant
            egui::vec2(680.0, 90.0),  // top-right quadrant
            egui::vec2(120.0, 510.0), // bottom-left quadrant
            egui::vec2(680.0, 510.0), // bottom-right quadrant
            egui::vec2(796.0, 4.0),   // near an edge/corner
        ];
        let deltas = [3.0_f32, -3.0, 12.0, -12.0];
        let speeds = [ZoomSpeed::Normal, ZoomSpeed::Slow];

        for &z0 in &zooms {
            for &cursor in &cursors {
                for &scroll_y in &deltas {
                    for &speed in &speeds {
                        let view = crate::app::ViewState {
                            zoom: z0,
                            pan: egui::vec2(15.0, -25.0),
                        };

                        let canvas_point = frame_local_to_canvas(cursor, view, viewport);
                        let zoomed = apply_scroll_zoom(view, scroll_y, cursor, viewport, speed);
                        let mapped_forward = canvas_to_frame_local(canvas_point, zoomed, viewport);

                        assert!(
                            (mapped_forward - cursor).length() < 1e-2,
                            "the canvas point under the cursor before a scroll must map back to \
                             the same cursor position after it: z0={z0} cursor={cursor:?} \
                             scroll_y={scroll_y} speed={speed:?} -> mapped={mapped_forward:?} \
                             (expected {cursor:?}), view={view:?} zoomed={zoomed:?}"
                        );
                    }
                }
            }
        }
    }

    /// Cursor at the viewport centre leaves pan untouched -- the bridge to
    /// the old contract and to NAV-05's keyboard `+`/`-` scheme, which never
    /// has a cursor to anchor on.
    #[test]
    fn scroll_zoom_leaves_pan_untouched_when_cursor_is_at_the_centre() {
        let viewport = egui::vec2(800.0, 600.0);
        let centre_cursor = viewport / 2.0;
        let view = crate::app::ViewState {
            zoom: 1.0,
            pan: egui::vec2(12.0, -8.0),
        };

        let zoomed = apply_scroll_zoom(view, 3.0, centre_cursor, viewport, ZoomSpeed::Normal);
        assert!(
            zoomed.zoom > view.zoom,
            "zoom must still increase with the cursor at the centre"
        );
        assert!(
            (zoomed.pan - view.pan).length() < 1e-4,
            "cursor at the viewport centre must leave pan untouched -- the old \
             centre-anchored contract survives as this special case, got pan {:?} -> {:?}",
            view.pan,
            zoomed.pan
        );
    }

    /// No drift at the MIN_ZOOM clamp: an off-centre cursor scrolling
    /// further down at MIN_ZOOM must leave both zoom AND pan unchanged --
    /// the test that catches a compensation computed against the pre-clamp
    /// (requested) zoom instead of the post-clamp one.
    #[test]
    fn scroll_zoom_does_not_drift_pan_when_clamped_at_min_zoom() {
        let viewport = egui::vec2(800.0, 600.0);
        let off_centre_cursor = egui::vec2(120.0, 480.0);
        let view = crate::app::ViewState {
            zoom: MIN_ZOOM,
            pan: egui::vec2(30.0, -15.0),
        };

        let zoomed = apply_scroll_zoom(
            view,
            -1000.0,
            off_centre_cursor,
            viewport,
            ZoomSpeed::Normal,
        );

        assert_eq!(
            zoomed.zoom, MIN_ZOOM,
            "zoom refused by the clamp must stay at MIN_ZOOM"
        );
        assert!(
            (zoomed.pan - view.pan).length() < 1e-3,
            "a zoom refused by the clamp must not move pan either -- holding the wheel at the \
             limit must not creep the canvas sideways, got pan {:?} -> {:?}",
            view.pan,
            zoomed.pan
        );
    }

    /// No drift at the MAX_ZOOM clamp -- the same guarantee at the opposite
    /// limit.
    #[test]
    fn scroll_zoom_does_not_drift_pan_when_clamped_at_max_zoom() {
        let viewport = egui::vec2(800.0, 600.0);
        let off_centre_cursor = egui::vec2(680.0, 90.0);
        let view = crate::app::ViewState {
            zoom: MAX_ZOOM,
            pan: egui::vec2(-40.0, 22.0),
        };

        let zoomed =
            apply_scroll_zoom(view, 1000.0, off_centre_cursor, viewport, ZoomSpeed::Normal);

        assert_eq!(
            zoomed.zoom, MAX_ZOOM,
            "zoom refused by the clamp must stay at MAX_ZOOM"
        );
        assert!(
            (zoomed.pan - view.pan).length() < 1e-3,
            "a zoom refused by the clamp must not move pan either -- holding the wheel at the \
             limit must not creep the canvas sideways, got pan {:?} -> {:?}",
            view.pan,
            zoomed.pan
        );
    }

    /// Zero scroll is a total no-op for an off-centre cursor: both fields
    /// unchanged.
    #[test]
    fn scroll_zoom_is_a_total_no_op_for_zero_scroll_even_off_centre() {
        let viewport = egui::vec2(800.0, 600.0);
        let off_centre_cursor = egui::vec2(210.0, 505.0);
        let view = crate::app::ViewState {
            zoom: 1.4,
            pan: egui::vec2(8.0, 3.0),
        };

        let zoomed = apply_scroll_zoom(view, 0.0, off_centre_cursor, viewport, ZoomSpeed::Normal);

        assert!(
            (zoomed.zoom - view.zoom).abs() < 1e-6,
            "zero scroll_y must leave zoom unchanged, even off-centre"
        );
        assert!(
            (zoomed.pan - view.pan).length() < 1e-6,
            "zero scroll_y must leave pan unchanged, even off-centre"
        );
    }

    /// Non-finite inputs cannot poison the view: a non-finite cursor
    /// position or a non-finite viewport must leave the computed pan
    /// finite, since `app.view` is read back and re-written every frame --
    /// a single NaN written into it persists and poisons the transform
    /// permanently (T-05-16-01).
    #[test]
    fn scroll_zoom_guards_non_finite_cursor_and_viewport() {
        let viewport = egui::vec2(800.0, 600.0);
        let view = crate::app::ViewState {
            zoom: 1.0,
            pan: egui::vec2(5.0, 5.0),
        };

        let non_finite_cursor = egui::vec2(f32::NAN, 100.0);
        let result = apply_scroll_zoom(view, 3.0, non_finite_cursor, viewport, ZoomSpeed::Normal);
        assert!(
            result.pan.x.is_finite() && result.pan.y.is_finite(),
            "a non-finite cursor position must not poison pan with NaN, got {:?}",
            result.pan
        );

        let non_finite_viewport = egui::vec2(f32::INFINITY, 600.0);
        let result = apply_scroll_zoom(
            view,
            3.0,
            egui::vec2(100.0, 100.0),
            non_finite_viewport,
            ZoomSpeed::Normal,
        );
        assert!(
            result.pan.x.is_finite() && result.pan.y.is_finite(),
            "a non-finite viewport must not poison pan with NaN/Inf, got {:?}",
            result.pan
        );
    }

    // ============================================================
    // Plan 17 (RED): ZoomSpeed. `apply_scroll_zoom` now takes a ZoomSpeed,
    // and in this task both variants are a deliberate equal-speed stub --
    // these tests pin the exact half-speed identity `05-17-PLAN.md`
    // `<design_decision>` section 2 derives (halving the SENSITIVITY, not
    // the resulting factor's distance from 1.0, so two Slow steps compose
    // exactly into one Normal step) and MUST fail against the stub. The
    // Normal-equals-the-constant test and the direction test pin properties
    // that are already true of the stub and must keep passing forever.
    // ============================================================

    /// The direct encoding of the user's "scroll wheel remains fast zoom as
    /// it is": `ZoomSpeed::Normal`'s sensitivity must equal
    /// `SCROLL_ZOOM_SENSITIVITY` exactly -- not a re-typed literal. PASSES
    /// against the stub (and must keep passing after Task 3 too).
    #[test]
    fn zoom_speed_normal_equals_the_constant() {
        assert_eq!(
            ZoomSpeed::Normal.sensitivity(),
            SCROLL_ZOOM_SENSITIVITY,
            "ZoomSpeed::Normal must resolve to exactly SCROLL_ZOOM_SENSITIVITY -- the user \
             explicitly asked for plain scroll to stay as it is"
        );
    }

    /// Composition: applying `Slow` twice with delta `d` must yield the same
    /// zoom as applying `Normal` once with delta `d`, at several starting
    /// zooms and for both signs of `d`, chosen to stay clear of the clamps.
    /// FAILS against the stub -- two stub-slow steps (equal sensitivity to
    /// Normal) overshoot to the SQUARE of one normal step, not match it.
    #[test]
    fn zoom_speed_composition_two_slow_equals_one_normal() {
        let viewport = egui::vec2(800.0, 600.0);
        let centre_cursor = viewport / 2.0;
        let starting_zooms = [0.5_f32, 1.0, 2.0];
        let deltas = [3.0_f32, -3.0];

        for &z0 in &starting_zooms {
            for &d in &deltas {
                let view = crate::app::ViewState {
                    zoom: z0,
                    pan: egui::vec2(0.0, 0.0),
                };

                let one_normal =
                    apply_scroll_zoom(view, d, centre_cursor, viewport, ZoomSpeed::Normal);
                let one_slow = apply_scroll_zoom(view, d, centre_cursor, viewport, ZoomSpeed::Slow);
                let two_slow =
                    apply_scroll_zoom(one_slow, d, centre_cursor, viewport, ZoomSpeed::Slow);

                let relative_error = (two_slow.zoom - one_normal.zoom).abs() / one_normal.zoom;
                assert!(
                    relative_error < 0.01,
                    "two ZoomSpeed::Slow steps must compose to exactly one ZoomSpeed::Normal \
                     step of the same delta: z0={z0} d={d} -> one_normal.zoom={} \
                     two_slow.zoom={} (relative error {relative_error})",
                    one_normal.zoom,
                    two_slow.zoom
                );
            }
        }
    }

    /// The quantitative definition of "half speed": `ln(zoom_slow / z0)` is
    /// exactly half `ln(zoom_normal / z0)` for the same delta. FAILS against
    /// the stub -- the ratio is 1.0 there, not 0.5.
    #[test]
    fn zoom_speed_log_ratio_is_exactly_half() {
        let viewport = egui::vec2(800.0, 600.0);
        let centre_cursor = viewport / 2.0;
        let z0 = 1.0_f32;
        let view = crate::app::ViewState {
            zoom: z0,
            pan: egui::vec2(0.0, 0.0),
        };
        let d = 5.0_f32;

        let normal = apply_scroll_zoom(view, d, centre_cursor, viewport, ZoomSpeed::Normal);
        let slow = apply_scroll_zoom(view, d, centre_cursor, viewport, ZoomSpeed::Slow);

        let log_ratio_normal = (normal.zoom / z0).ln();
        let log_ratio_slow = (slow.zoom / z0).ln();
        let expected_slow = log_ratio_normal / 2.0;

        assert!(
            (log_ratio_slow - expected_slow).abs() < 1e-4,
            "ln(zoom_slow / z0) must be exactly half ln(zoom_normal / z0): expected {expected_slow}, \
             got {log_ratio_slow} (log_ratio_normal={log_ratio_normal})"
        );
    }

    /// Cheap insurance against a sign or reciprocal mistake in Task 3: a
    /// positive delta under `Slow` still increases zoom, a negative delta
    /// still decreases it. PASSES against the stub.
    #[test]
    fn zoom_speed_direction_is_preserved_under_slow() {
        let viewport = egui::vec2(800.0, 600.0);
        let centre_cursor = viewport / 2.0;
        let view = crate::app::ViewState {
            zoom: 1.0,
            pan: egui::vec2(0.0, 0.0),
        };

        let zoomed_in = apply_scroll_zoom(view, 3.0, centre_cursor, viewport, ZoomSpeed::Slow);
        assert!(
            zoomed_in.zoom > view.zoom,
            "a positive delta under ZoomSpeed::Slow must still increase zoom, got {} -> {}",
            view.zoom,
            zoomed_in.zoom
        );

        let zoomed_out = apply_scroll_zoom(view, -3.0, centre_cursor, viewport, ZoomSpeed::Slow);
        assert!(
            zoomed_out.zoom < view.zoom,
            "a negative delta under ZoomSpeed::Slow must still decrease zoom, got {} -> {}",
            view.zoom,
            zoomed_out.zoom
        );
    }

    /// 05-16's centre-cursor pan-preservation invariant, re-proven under
    /// `ZoomSpeed::Slow` -- the anchoring math is independent of the
    /// sensitivity exponent, so this must PASS against the stub (and must
    /// keep passing after Task 3 changes the exponent).
    #[test]
    fn zoom_speed_slow_leaves_pan_untouched_at_centre() {
        let viewport = egui::vec2(800.0, 600.0);
        let centre_cursor = viewport / 2.0;
        let view = crate::app::ViewState {
            zoom: 1.0,
            pan: egui::vec2(12.0, -8.0),
        };

        let zoomed = apply_scroll_zoom(view, 3.0, centre_cursor, viewport, ZoomSpeed::Slow);
        assert!(
            zoomed.zoom > view.zoom,
            "zoom must still increase with the cursor at the centre, under ZoomSpeed::Slow"
        );
        assert!(
            (zoomed.pan - view.pan).length() < 1e-4,
            "cursor at the viewport centre must leave pan untouched under ZoomSpeed::Slow too, \
             got pan {:?} -> {:?}",
            view.pan,
            zoomed.pan
        );
    }

    /// 05-16's MIN_ZOOM no-drift invariant, re-proven under
    /// `ZoomSpeed::Slow`.
    #[test]
    fn zoom_speed_slow_does_not_drift_pan_at_min_zoom_clamp() {
        let viewport = egui::vec2(800.0, 600.0);
        let off_centre_cursor = egui::vec2(120.0, 480.0);
        let view = crate::app::ViewState {
            zoom: MIN_ZOOM,
            pan: egui::vec2(30.0, -15.0),
        };

        let zoomed = apply_scroll_zoom(view, -1000.0, off_centre_cursor, viewport, ZoomSpeed::Slow);

        assert_eq!(
            zoomed.zoom, MIN_ZOOM,
            "zoom refused by the clamp must stay at MIN_ZOOM under ZoomSpeed::Slow too"
        );
        assert!(
            (zoomed.pan - view.pan).length() < 1e-3,
            "a zoom refused by the clamp must not move pan either, under ZoomSpeed::Slow, got \
             pan {:?} -> {:?}",
            view.pan,
            zoomed.pan
        );
    }

    /// 05-16's MAX_ZOOM no-drift invariant, re-proven under
    /// `ZoomSpeed::Slow`.
    #[test]
    fn zoom_speed_slow_does_not_drift_pan_at_max_zoom_clamp() {
        let viewport = egui::vec2(800.0, 600.0);
        let off_centre_cursor = egui::vec2(680.0, 90.0);
        let view = crate::app::ViewState {
            zoom: MAX_ZOOM,
            pan: egui::vec2(-40.0, 22.0),
        };

        let zoomed = apply_scroll_zoom(view, 1000.0, off_centre_cursor, viewport, ZoomSpeed::Slow);

        assert_eq!(
            zoomed.zoom, MAX_ZOOM,
            "zoom refused by the clamp must stay at MAX_ZOOM under ZoomSpeed::Slow too"
        );
        assert!(
            (zoomed.pan - view.pan).length() < 1e-3,
            "a zoom refused by the clamp must not move pan either, under ZoomSpeed::Slow, got \
             pan {:?} -> {:?}",
            view.pan,
            zoomed.pan
        );
    }

    /// 05-16's non-finite-input guard, re-proven under `ZoomSpeed::Slow`.
    #[test]
    fn zoom_speed_slow_guards_non_finite_cursor_and_viewport() {
        let viewport = egui::vec2(800.0, 600.0);
        let view = crate::app::ViewState {
            zoom: 1.0,
            pan: egui::vec2(5.0, 5.0),
        };

        let non_finite_cursor = egui::vec2(f32::NAN, 100.0);
        let result = apply_scroll_zoom(view, 3.0, non_finite_cursor, viewport, ZoomSpeed::Slow);
        assert!(
            result.pan.x.is_finite() && result.pan.y.is_finite(),
            "a non-finite cursor position must not poison pan with NaN under ZoomSpeed::Slow, \
             got {:?}",
            result.pan
        );

        let non_finite_viewport = egui::vec2(f32::INFINITY, 600.0);
        let result = apply_scroll_zoom(
            view,
            3.0,
            egui::vec2(100.0, 100.0),
            non_finite_viewport,
            ZoomSpeed::Slow,
        );
        assert!(
            result.pan.x.is_finite() && result.pan.y.is_finite(),
            "a non-finite viewport must not poison pan with NaN/Inf under ZoomSpeed::Slow, got \
             {:?}",
            result.pan
        );
    }

    // ============================================================
    // Plan 19 Task 2 (RED-first micro-cycle): apply_zoom_factor, the
    // factor-domain core extracted from apply_scroll_zoom, and
    // ZoomSpeed::apply_to_factor, its half-speed twin. These tests pin the
    // exactness/clamp/guard properties on the new factor-domain entry point
    // (`<design_decision>` section 3) and the half-speed identity in the
    // factor domain (`<design_decision>` section 4). Two of them (the
    // factor guard and the two-slow-equals-one-normal composition) MUST
    // fail against Task 2's deliberate stub (no guard, apply_to_factor the
    // identity for both variants) before the guard and the `powf` are
    // added.
    // ============================================================

    /// Factor 1.0 is a total no-op, even off-centre -- mirrors
    /// `scroll_zoom_is_a_total_no_op_for_zero_scroll_even_off_centre`'s
    /// shape in the factor domain (factor 1.0 == exp(0), the identity
    /// scroll).
    #[test]
    fn zoom_factor_of_one_is_a_total_no_op() {
        let viewport = egui::vec2(800.0, 600.0);
        let off_centre_cursor = egui::vec2(210.0, 505.0);
        let view = crate::app::ViewState {
            zoom: 1.4,
            pan: egui::vec2(8.0, 3.0),
        };

        let zoomed = apply_zoom_factor(view, 1.0, off_centre_cursor, viewport);

        assert!(
            (zoomed.zoom - view.zoom).abs() < 1e-6,
            "factor 1.0 must leave zoom unchanged, even off-centre"
        );
        assert!(
            (zoomed.pan - view.pan).length() < 1e-6,
            "factor 1.0 must leave pan unchanged, even off-centre"
        );
    }

    /// The core identity from `05-19-PLAN.md` `<design_decision>` section
    /// 3(A): for a factor comfortably inside the clamp, the resulting zoom
    /// equals `view.zoom * factor` exactly (within float tolerance) -- the
    /// app applies EXACTLY the factor it is given, not a re-derived one.
    #[test]
    fn zoom_factor_is_applied_exactly() {
        let viewport = egui::vec2(800.0, 600.0);
        let centre_cursor = viewport / 2.0;
        let view = crate::app::ViewState {
            zoom: 1.1023769,
            pan: egui::vec2(12.0, -8.0),
        };

        let factor = 1.716007_f32;
        let zoomed = apply_zoom_factor(view, factor, centre_cursor, viewport);

        assert!(
            (zoomed.zoom - view.zoom * factor).abs() < 1e-6,
            "result.zoom must equal view.zoom * factor exactly, got {} expected {}",
            zoomed.zoom,
            view.zoom * factor
        );
    }

    /// **RED half of Task 2's micro-cycle.** A non-finite or non-positive
    /// factor must leave `view` completely unchanged -- zoom AND pan -- and
    /// in particular must never produce a non-finite zoom. `f32::clamp`
    /// propagates NaN rather than absorbing it, and this entry point
    /// receives a factor straight from gesture input rather than from a
    /// guaranteed-finite `exp()`, so this guard is additive relative to
    /// `apply_scroll_zoom`'s existing guards (`<design_decision>` section
    /// 3). Fails against the un-guarded extraction; passes once the guard
    /// is added.
    #[test]
    fn zoom_factor_guards_non_finite_and_non_positive() {
        let viewport = egui::vec2(800.0, 600.0);
        let cursor = egui::vec2(210.0, 505.0);
        let view = crate::app::ViewState {
            zoom: 1.4,
            pan: egui::vec2(8.0, 3.0),
        };

        for &bad_factor in &[f32::NAN, f32::INFINITY, 0.0_f32, -1.0_f32] {
            let result = apply_zoom_factor(view, bad_factor, cursor, viewport);
            assert_eq!(
                result.zoom, view.zoom,
                "a non-finite or non-positive factor ({bad_factor}) must leave zoom completely \
                 unchanged, got {}",
                result.zoom
            );
            assert_eq!(
                result.pan, view.pan,
                "a non-finite or non-positive factor ({bad_factor}) must leave pan completely \
                 unchanged, got {:?}",
                result.pan
            );
            assert!(
                result.zoom.is_finite(),
                "a non-finite or non-positive factor ({bad_factor}) must never produce a \
                 non-finite zoom, got {}",
                result.zoom
            );
        }
    }

    /// 05-16's no-creep-at-the-clamp property, re-proven through the new
    /// factor-domain entry point: a huge factor with an off-centre cursor
    /// lands exactly on MAX_ZOOM and leaves pan untouched.
    #[test]
    fn zoom_factor_respects_the_clamp_without_pan_drift() {
        let viewport = egui::vec2(800.0, 600.0);
        let off_centre_cursor = egui::vec2(680.0, 90.0);
        let view = crate::app::ViewState {
            zoom: MAX_ZOOM,
            pan: egui::vec2(-40.0, 22.0),
        };

        let zoomed = apply_zoom_factor(view, 1_000_000.0, off_centre_cursor, viewport);

        assert_eq!(
            zoomed.zoom, MAX_ZOOM,
            "a huge factor must clamp exactly at MAX_ZOOM"
        );
        assert!(
            (zoomed.pan - view.pan).length() < 1e-3,
            "a zoom refused by the clamp must not move pan either, got pan {:?} -> {:?}",
            view.pan,
            zoomed.pan
        );
    }

    /// `ZoomSpeed::Normal.apply_to_factor(f)` is the identity for any `f` --
    /// the factor-domain twin of `Normal.sensitivity() ==
    /// SCROLL_ZOOM_SENSITIVITY`.
    #[test]
    fn zoom_speed_apply_to_factor_normal_is_identity() {
        for &f in &[1.0_f32, 1.716007, 0.5, 2.3] {
            assert_eq!(
                ZoomSpeed::Normal.apply_to_factor(f),
                f,
                "ZoomSpeed::Normal.apply_to_factor must be the identity, got {} for input {}",
                ZoomSpeed::Normal.apply_to_factor(f),
                f
            );
        }
    }

    /// **RED half of Task 2's micro-cycle.** Two `ZoomSpeed::Slow`
    /// applications must compose to exactly one `ZoomSpeed::Normal`
    /// application, in the factor domain -- the same `slow² == fast`
    /// identity 05-17 proved in the scroll-magnitude domain
    /// (`<design_decision>` section 4), derived from the same
    /// `SLOW_ZOOM_DIVISOR` constant so "half speed" is defined once. Fails
    /// against the stub (which makes Slow equal Normal, so squaring
    /// overshoots); passes once `powf(1.0 / SLOW_ZOOM_DIVISOR)` is added.
    #[test]
    fn zoom_speed_apply_to_factor_two_slow_equals_one_normal() {
        for &f in &[1.716007_f32, 1.24, 3.0] {
            let one_normal = ZoomSpeed::Normal.apply_to_factor(f);
            let two_slow = ZoomSpeed::Slow.apply_to_factor(f).powi(2);
            let rel_err = (two_slow - one_normal).abs() / one_normal.max(1e-6);
            assert!(
                rel_err < 1e-5,
                "two ZoomSpeed::Slow applications must compose to exactly one \
                 ZoomSpeed::Normal application of the same factor: f={f} \
                 one_normal={one_normal} two_slow={two_slow} (relative error {rel_err})"
            );
        }
    }

    // ============================================================
    // Plan 13 gap closure (G-05-5): live-wiring regression test for the
    // drag-to-trace gesture -- drives the REAL DragStart -> Dragging ->
    // DragStop path through a live-rendered GraphView, not the pure
    // trace::update_gesture state machine in isolation (05-05 shipped
    // exactly that and missed the defect).
    // ============================================================

    /// Two position snapshots are "the same" (settled) when every node's id
    /// matches (same order -- both come from the same `graph.nodes_iter()`
    /// sequence, sorted by id below for safety) and its screen position has
    /// moved less than `POSITION_SETTLE_EPSILON` since the previous step.
    const POSITION_SETTLE_EPSILON: f32 = 0.3;

    fn positions_stable(a: &[(String, egui::Pos2)], b: &[(String, egui::Pos2)]) -> bool {
        a.len() == b.len()
            && a.iter()
                .zip(b.iter())
                .all(|((id_a, pos_a), (id_b, pos_b))| {
                    id_a == id_b && (*pos_a - *pos_b).length() < POSITION_SETTLE_EPSILON
                })
    }

    #[test]
    fn drag_between_two_nodes_produces_a_trace() {
        use std::cell::RefCell;
        use std::rc::Rc;

        let outcome =
            crate::load::read_and_ingest(CLEAN_FIXTURE).expect("fixture must ingest cleanly");
        let app = crate::app::SeamExplorerApp {
            model: Some(outcome.model),
            seams: outcome.seams,
            trace_mode: true,
            ..Default::default()
        };

        let positions_mirror: Rc<RefCell<Vec<(String, egui::Pos2)>>> =
            Rc::new(RefCell::new(Vec::new()));
        let gesture_mirror: Rc<RefCell<crate::trace::TraceGesture>> =
            Rc::new(RefCell::new(crate::trace::TraceGesture::Idle));
        let positions_inner = positions_mirror.clone();
        let gesture_inner = gesture_mirror.clone();

        let mut harness = egui_kittest::Harness::new_ui_state(
            move |ui, app: &mut crate::app::SeamExplorerApp| {
                show(ui, app);
                *positions_inner.borrow_mut() = test_probe::load_node_screen_positions(ui);
                *gesture_inner.borrow_mut() = crate::trace::load_gesture(ui);
            },
            app,
        );

        // Settle deterministically: step until the mirrored position vector
        // is byte-stable (within epsilon) across two consecutive steps,
        // rather than a magic step count -- an unsettled canvas makes the
        // hit-test miss for an unrelated reason (layout drift), and a test
        // that can fail for two different reasons cannot be a regression
        // test for either. `SeamLayout`'s ease+repulsion dynamic on this
        // 6-node fixture reaches its main equilibrium (deterministically,
        // confirmed stable across repeated runs) around step ~330 --
        // POSITION_SETTLE_EPSILON is deliberately not tighter than that:
        // a much slower secondary drift (repulsion vs. the fixed y-target,
        // an unrelated pre-existing SeamLayout tuning characteristic, not
        // a G-05-5 concern) continues for well over 1000 more steps and
        // eventually carries node a1 just off the top edge of the canvas
        // -- settling at the coarser, still-sub-pixel epsilon below avoids
        // that irrelevant drift while still being a genuine convergence
        // check, not a magic step count. MAX_SETTLE_STEPS gives generous
        // headroom above the observed settle point.
        const MAX_SETTLE_STEPS: usize = 3000;
        let mut prev: Option<Vec<(String, egui::Pos2)>> = None;
        let mut settled = false;
        for _ in 0..MAX_SETTLE_STEPS {
            harness.step();
            let mut current = positions_mirror.borrow().clone();
            current.sort_by(|a, b| a.0.cmp(&b.0));
            if let Some(prev_positions) = &prev {
                if positions_stable(prev_positions, &current) {
                    settled = true;
                    break;
                }
            }
            prev = Some(current);
        }
        assert!(
            settled,
            "canvas did not settle within {MAX_SETTLE_STEPS} steps"
        );

        // Re-read positions from the mirror immediately before use.
        let positions = positions_mirror.borrow().clone();
        assert!(
            !positions.is_empty(),
            "position probe published no positions -- fixture failed to ingest or render, \
             not the bug under test"
        );
        let pos_of = |id: &str| -> egui::Pos2 {
            positions
                .iter()
                .find(|(nid, _)| nid == id)
                .map(|(_, p)| *p)
                .unwrap_or_else(|| {
                    panic!("node {id} not found in published positions: {positions:?}")
                })
        };
        let a1_pos = pos_of("a1");
        let c1_pos = pos_of("c1");
        assert!(
            (a1_pos - c1_pos).length() > 6.0,
            "a1 and c1 must be further apart than egui's 6pt click threshold for this to be a \
             genuine drag, got a1={a1_pos:?} c1={c1_pos:?}"
        );

        // Perform the drag: press down exactly on a1, move past the 6pt
        // threshold toward c1 (the frame where drag_started() fires and
        // where the bug bites), then release exactly on c1. Record the
        // mirrored gesture after every step so the Dragging assertion can
        // inspect the whole sequence rather than one lucky frame.
        let mut recorded_gestures: Vec<crate::trace::TraceGesture> = Vec::new();

        harness
            .input_mut()
            .events
            .push(egui::Event::PointerMoved(a1_pos));
        harness.step();
        recorded_gestures.push(gesture_mirror.borrow().clone());

        harness.input_mut().events.push(egui::Event::PointerButton {
            pos: a1_pos,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::default(),
        });
        harness.step();
        recorded_gestures.push(gesture_mirror.borrow().clone());

        let midpoint = a1_pos + (c1_pos - a1_pos) * 0.5;
        harness
            .input_mut()
            .events
            .push(egui::Event::PointerMoved(midpoint));
        harness.step();
        recorded_gestures.push(gesture_mirror.borrow().clone());

        harness
            .input_mut()
            .events
            .push(egui::Event::PointerMoved(c1_pos));
        harness.step();
        recorded_gestures.push(gesture_mirror.borrow().clone());

        harness.input_mut().events.push(egui::Event::PointerButton {
            pos: c1_pos,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::default(),
        });
        harness.step();
        recorded_gestures.push(gesture_mirror.borrow().clone());

        let dragging_from_a1 = recorded_gestures.iter().any(
            |g| matches!(g, crate::trace::TraceGesture::Dragging { from, .. } if from == "a1"),
        );
        assert!(
            dragging_from_a1,
            "gesture must reach Dragging{{from: \"a1\", ..}} at some point during the drag -- \
             recorded gesture sequence: {recorded_gestures:?}, a1_pos={a1_pos:?}, c1_pos={c1_pos:?}"
        );

        let trace = harness.state().trace.clone().unwrap_or_else(|| {
            panic!(
                "app.trace must be Some after releasing on c1 -- recorded gesture sequence: \
                 {recorded_gestures:?}, a1_pos={a1_pos:?}, c1_pos={c1_pos:?}"
            )
        });
        assert_eq!(trace.from, "a1");
        assert_eq!(trace.to, "c1");
        assert!(
            trace.path.is_some(),
            "a1 -> c1 has a direct edge in the fixture; the trace must resolve a path"
        );
    }

    // ============================================================
    // Plan 18 (05-18): live-wiring tests for the trace-button policy.
    // Written FIRST, against unmodified production code -- see
    // <tdd_discipline>. Planning's own probe proved a secondary-button
    // drag already traces today (an accident of egui 0.35's button-
    // agnostic drag machinery); this plan turns that accident into a
    // specification. Exactly one of the four tests below --
    // `middle_button_drag_between_two_nodes_does_not_trace` -- is
    // expected to FAIL at the end of this task. The other three are
    // regression locks that pass today and must keep passing after
    // Task 2 narrows the button policy.
    // ============================================================

    /// Outcome of `drive_button_gesture`: the recorded per-step gesture
    /// sequence, the harness (to read `harness.state().trace`), and both
    /// node screen positions used for the gesture -- surfaced so every
    /// assertion below can print the same diagnostics
    /// `drag_between_two_nodes_produces_a_trace` does. A named struct
    /// (rather than a bare tuple) keeps the return type under clippy's
    /// `type_complexity` threshold, matching `RefitTestPositions`'s
    /// precedent below.
    struct DriveResult {
        gestures: Vec<crate::trace::TraceGesture>,
        harness: egui_kittest::Harness<'static, crate::app::SeamExplorerApp>,
        from_pos: egui::Pos2,
        to_pos: Option<egui::Pos2>,
    }

    /// Drives a synthetic press-[move-move]-release sequence for `button`
    /// starting on `from_id`, settling the canvas first exactly as
    /// `drag_between_two_nodes_produces_a_trace` does above. When `to_id`
    /// is `Some`, the sequence is a genuine drag (press on `from_id`, move
    /// to the midpoint, move to `to_id`, release on `to_id`) -- the same
    /// five-event shape the existing test uses. When `to_id` is `None`,
    /// the sequence is a plain click (press and release on `from_id` with
    /// NO intervening movement at all) -- the parameter that skips the
    /// intermediate `PointerMoved` events for
    /// `primary_button_click_without_drag_does_not_trace`.
    ///
    /// Locals are named `from_pos`/`to_pos`, deliberately not
    /// `a1_pos`/`c1_pos` -- 05-13's `drag_between_two_nodes_produces_a_trace`
    /// above is left completely untouched by this plan, and a verify gate
    /// distinguishes this plan's new code from that test by exactly those
    /// identifiers.
    fn drive_button_gesture(
        button: egui::PointerButton,
        trace_mode: bool,
        from_id: &str,
        to_id: Option<&str>,
    ) -> DriveResult {
        use std::cell::RefCell;
        use std::rc::Rc;

        let outcome =
            crate::load::read_and_ingest(CLEAN_FIXTURE).expect("fixture must ingest cleanly");
        let app = crate::app::SeamExplorerApp {
            model: Some(outcome.model),
            seams: outcome.seams,
            trace_mode,
            ..Default::default()
        };

        let positions_mirror: Rc<RefCell<Vec<(String, egui::Pos2)>>> =
            Rc::new(RefCell::new(Vec::new()));
        let gesture_mirror: Rc<RefCell<crate::trace::TraceGesture>> =
            Rc::new(RefCell::new(crate::trace::TraceGesture::Idle));
        let positions_inner = positions_mirror.clone();
        let gesture_inner = gesture_mirror.clone();

        let mut harness = egui_kittest::Harness::new_ui_state(
            move |ui, app: &mut crate::app::SeamExplorerApp| {
                show(ui, app);
                *positions_inner.borrow_mut() = test_probe::load_node_screen_positions(ui);
                *gesture_inner.borrow_mut() = crate::trace::load_gesture(ui);
            },
            app,
        );

        // Settle deterministically, exactly as `drag_between_two_nodes_produces_a_trace`
        // does above -- reusing the same `positions_stable`/`POSITION_SETTLE_EPSILON`
        // this test module already defines. `MAX_SETTLE_STEPS` is redeclared here
        // (that test's own copy is a fn-local const, not module-scoped).
        const MAX_SETTLE_STEPS: usize = 3000;
        let mut prev: Option<Vec<(String, egui::Pos2)>> = None;
        let mut settled = false;
        for _ in 0..MAX_SETTLE_STEPS {
            harness.step();
            let mut current = positions_mirror.borrow().clone();
            current.sort_by(|a, b| a.0.cmp(&b.0));
            if let Some(prev_positions) = &prev {
                if positions_stable(prev_positions, &current) {
                    settled = true;
                    break;
                }
            }
            prev = Some(current);
        }
        assert!(
            settled,
            "canvas did not settle within {MAX_SETTLE_STEPS} steps"
        );

        let positions = positions_mirror.borrow().clone();
        assert!(
            !positions.is_empty(),
            "position probe published no positions -- fixture failed to ingest or render, not \
             the bug under test"
        );
        let pos_of = |id: &str| -> egui::Pos2 {
            positions
                .iter()
                .find(|(nid, _)| nid == id)
                .map(|(_, p)| *p)
                .unwrap_or_else(|| {
                    panic!("node {id} not found in published positions: {positions:?}")
                })
        };
        let from_pos = pos_of(from_id);

        let mut recorded_gestures: Vec<crate::trace::TraceGesture> = Vec::new();

        harness
            .input_mut()
            .events
            .push(egui::Event::PointerMoved(from_pos));
        harness.step();
        recorded_gestures.push(gesture_mirror.borrow().clone());

        harness.input_mut().events.push(egui::Event::PointerButton {
            pos: from_pos,
            button,
            pressed: true,
            modifiers: egui::Modifiers::default(),
        });
        harness.step();
        recorded_gestures.push(gesture_mirror.borrow().clone());

        let (release_pos, to_pos) = if let Some(to_id) = to_id {
            let to_pos = pos_of(to_id);
            assert!(
                (from_pos - to_pos).length() > 6.0,
                "from and to nodes must be further apart than egui's 6pt click threshold for \
                 this to be a genuine drag, got from={from_pos:?} to={to_pos:?}"
            );

            let midpoint = from_pos + (to_pos - from_pos) * 0.5;
            harness
                .input_mut()
                .events
                .push(egui::Event::PointerMoved(midpoint));
            harness.step();
            recorded_gestures.push(gesture_mirror.borrow().clone());

            harness
                .input_mut()
                .events
                .push(egui::Event::PointerMoved(to_pos));
            harness.step();
            recorded_gestures.push(gesture_mirror.borrow().clone());

            (to_pos, Some(to_pos))
        } else {
            (from_pos, None)
        };

        harness.input_mut().events.push(egui::Event::PointerButton {
            pos: release_pos,
            button,
            pressed: false,
            modifiers: egui::Modifiers::default(),
        });
        harness.step();
        recorded_gestures.push(gesture_mirror.borrow().clone());

        DriveResult {
            gestures: recorded_gestures,
            harness,
            from_pos,
            to_pos,
        }
    }

    /// A secondary-button (right) drag between two nodes reaches
    /// `Dragging{from: "a1", ..}` and resolves the same trace a
    /// left-button drag would. **Passes today** -- this is the regression
    /// lock proving Task 2's narrowing of the button policy does not
    /// overshoot and delete the feature it exists to specify.
    #[test]
    fn secondary_button_drag_between_two_nodes_produces_a_trace() {
        let result = drive_button_gesture(egui::PointerButton::Secondary, true, "a1", Some("c1"));

        let dragging_from_a1 = result.gestures.iter().any(
            |g| matches!(g, crate::trace::TraceGesture::Dragging { from, .. } if from == "a1"),
        );
        assert!(
            dragging_from_a1,
            "a secondary-button drag must reach Dragging{{from: \"a1\", ..}} at some point -- \
             recorded gesture sequence: {:?}, from_pos={:?}, to_pos={:?}",
            result.gestures, result.from_pos, result.to_pos
        );

        let trace = result.harness.state().trace.clone().unwrap_or_else(|| {
            panic!(
                "app.trace must be Some after a secondary-button drag from a1 to c1 -- recorded \
                 gesture sequence: {:?}, from_pos={:?}, to_pos={:?}",
                result.gestures, result.from_pos, result.to_pos
            )
        });
        assert_eq!(trace.from, "a1");
        assert_eq!(trace.to, "c1");
        assert!(
            trace.path.is_some(),
            "a1 -> c1 has a direct edge in the fixture; the trace must resolve a path"
        );
    }

    /// A middle-button drag between two nodes must NEVER reach `Dragging`
    /// and must NEVER produce a trace -- middle is `egui_graphs`' own pan
    /// gesture, deliberately excluded from the trace-button policy.
    /// **This is the RED**: it fails today because `handle_trace_gesture`'s
    /// drag detection is button-agnostic.
    #[test]
    fn middle_button_drag_between_two_nodes_does_not_trace() {
        let result = drive_button_gesture(egui::PointerButton::Middle, true, "a1", Some("c1"));

        let dragging_from_a1 = result.gestures.iter().any(
            |g| matches!(g, crate::trace::TraceGesture::Dragging { from, .. } if from == "a1"),
        );
        assert!(
            !dragging_from_a1,
            "a middle-button drag must NEVER reach Dragging -- middle is egui_graphs' own pan \
             gesture, not a trace-starting button -- recorded gesture sequence: {:?}, \
             from_pos={:?}, to_pos={:?}",
            result.gestures, result.from_pos, result.to_pos
        );

        assert!(
            result.harness.state().trace.is_none(),
            "app.trace must stay None after a middle-button drag from a1 to c1 -- recorded \
             gesture sequence: {:?}, from_pos={:?}, to_pos={:?}, got trace={:?}",
            result.gestures,
            result.from_pos,
            result.to_pos,
            result.harness.state().trace
        );
    }

    /// A plain left click (press and release on a1 with NO intervening
    /// movement) must do nothing: no trace, and the gesture must end
    /// `Idle` -- no stuck rubber band left on the canvas. This is the
    /// direct test of the user's "retain left click". **Passes today.**
    #[test]
    fn primary_button_click_without_drag_does_not_trace() {
        let result = drive_button_gesture(egui::PointerButton::Primary, true, "a1", None);

        assert!(
            result.harness.state().trace.is_none(),
            "a plain left click (no movement) must never trace -- recorded gesture sequence: \
             {:?}, from_pos={:?}, got trace={:?}",
            result.gestures,
            result.from_pos,
            result.harness.state().trace
        );
        let final_gesture = result.gestures.last().cloned().unwrap_or_default();
        assert_eq!(
            final_gesture,
            crate::trace::TraceGesture::Idle,
            "a plain left click must leave the gesture Idle -- no stuck rubber band -- recorded \
             gesture sequence: {:?}, from_pos={:?}",
            result.gestures,
            result.from_pos
        );
    }

    /// With Trace mode OFF, a secondary-button drag must still produce no
    /// trace -- the `!app.trace_mode` early return in `handle_trace_gesture`
    /// is button-blind by construction, and this pins that the new button
    /// path cannot leak past it. **Passes today.**
    #[test]
    fn secondary_button_drag_does_not_trace_when_trace_mode_is_off() {
        let result = drive_button_gesture(egui::PointerButton::Secondary, false, "a1", Some("c1"));

        assert!(
            result.harness.state().trace.is_none(),
            "a secondary-button drag must not trace when trace_mode is off -- recorded gesture \
             sequence: {:?}, from_pos={:?}, to_pos={:?}, got trace={:?}",
            result.gestures,
            result.from_pos,
            result.to_pos,
            result.harness.state().trace
        );
    }

    // ============================================================
    // Plan 23 (05-23): live-wiring tests for the right-click context menu.
    // Written FIRST (Task 1, RED), against unmodified production code --
    // see <tdd_discipline>. Uses SOURCE_PATHS_FIXTURE (05-20's
    // seam-core/tests/fixtures/source_paths.json -- a1 has a source file
    // and a line, b1 is blank, b2 has neither key), not CLEAN_FIXTURE.
    // Locals are named menu_target_pos/menu_other_pos, deliberately not
    // a1_pos/c1_pos/from_pos/to_pos, so a diff gate can tell this plan's
    // code from 05-13's/05-18's.
    // ============================================================

    const SOURCE_PATHS_FIXTURE: &str =
        include_str!("../../seam-core/tests/fixtures/source_paths.json");

    /// Mirrors this frame's gesture and press-capture state out of egui
    /// temp memory into plain Rust state a test can read after
    /// `harness.step()` -- the same mirroring
    /// `drag_between_two_nodes_produces_a_trace` already establishes for
    /// `positions`/`gesture`; this plan adds a press-capture mirror
    /// alongside it (the direct lock on probe conclusion 2). Popup-open
    /// state and label findability need no mirror at all -- both are read
    /// straight off `harness.ctx`/`harness` after `step()` (see
    /// `argv_probe`'s doc comment for why the argv probe is designed the
    /// same way).
    struct MenuTestMirrors {
        positions: std::rc::Rc<std::cell::RefCell<Vec<(String, egui::Pos2)>>>,
        gesture: std::rc::Rc<std::cell::RefCell<crate::trace::TraceGesture>>,
        press_capture: std::rc::Rc<std::cell::RefCell<Option<crate::trace::PressCapture>>>,
    }

    /// Builds a settled `egui_kittest::Harness` over `SOURCE_PATHS_FIXTURE`,
    /// mirroring `drag_between_two_nodes_produces_a_trace`'s
    /// settle-to-stability discipline exactly (same
    /// `positions_stable`/`POSITION_SETTLE_EPSILON`, this module's own
    /// `MAX_SETTLE_STEPS` redeclared here as `drive_button_gesture` also
    /// does). Returns the harness, the settled node id -> screen position
    /// map, and the mirrors so a test can inspect gesture/press-capture
    /// state after each subsequent step.
    fn settle_menu_harness(
        trace_mode: bool,
    ) -> (
        egui_kittest::Harness<'static, crate::app::SeamExplorerApp>,
        Vec<(String, egui::Pos2)>,
        MenuTestMirrors,
    ) {
        let ingest =
            seam_core::from_json(SOURCE_PATHS_FIXTURE).expect("fixture must ingest cleanly");
        let app = crate::app::SeamExplorerApp {
            model: Some(ingest.model),
            trace_mode,
            ..Default::default()
        };

        let mirrors = MenuTestMirrors {
            positions: std::rc::Rc::new(std::cell::RefCell::new(Vec::new())),
            gesture: std::rc::Rc::new(std::cell::RefCell::new(crate::trace::TraceGesture::Idle)),
            press_capture: std::rc::Rc::new(std::cell::RefCell::new(None)),
        };
        let positions_inner = mirrors.positions.clone();
        let gesture_inner = mirrors.gesture.clone();
        let press_capture_inner = mirrors.press_capture.clone();

        let mut harness = egui_kittest::Harness::new_ui_state(
            move |ui, app: &mut crate::app::SeamExplorerApp| {
                show(ui, app);
                *positions_inner.borrow_mut() = test_probe::load_node_screen_positions(ui);
                *gesture_inner.borrow_mut() = crate::trace::load_gesture(ui);
                *press_capture_inner.borrow_mut() = crate::trace::load_press_capture(ui);
            },
            app,
        );

        const MAX_SETTLE_STEPS: usize = 3000;
        let mut prev: Option<Vec<(String, egui::Pos2)>> = None;
        let mut settled = false;
        for _ in 0..MAX_SETTLE_STEPS {
            harness.step();
            let mut current = mirrors.positions.borrow().clone();
            current.sort_by(|a, b| a.0.cmp(&b.0));
            if let Some(prev_positions) = &prev {
                if positions_stable(prev_positions, &current) {
                    settled = true;
                    break;
                }
            }
            prev = Some(current);
        }
        assert!(
            settled,
            "canvas did not settle within {MAX_SETTLE_STEPS} steps"
        );

        let positions = mirrors.positions.borrow().clone();
        assert!(
            !positions.is_empty(),
            "position probe published no positions -- fixture failed to ingest or render, not \
             the bug under test"
        );

        (harness, positions, mirrors)
    }

    fn menu_pos_of(positions: &[(String, egui::Pos2)], id: &str) -> egui::Pos2 {
        positions
            .iter()
            .find(|(nid, _)| nid == id)
            .map(|(_, p)| *p)
            .unwrap_or_else(|| {
                panic!("node {id} not found in published positions: {positions:?}")
            })
    }

    /// Drives a plain right-click (press then release with NO intervening
    /// movement) at `pos`.
    fn right_click_no_movement(
        harness: &mut egui_kittest::Harness<'static, crate::app::SeamExplorerApp>,
        pos: egui::Pos2,
    ) {
        harness
            .input_mut()
            .events
            .push(egui::Event::PointerMoved(pos));
        harness.step();
        harness.input_mut().events.push(egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Secondary,
            pressed: true,
            modifiers: egui::Modifiers::default(),
        });
        harness.step();
        harness.input_mut().events.push(egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Secondary,
            pressed: false,
            modifiers: egui::Modifiers::default(),
        });
        harness.step();
    }

    /// Right-clicking a node opens the menu at the pointer, with the
    /// `OPEN_FILE_LABEL` item findable. **Fails today** -- Task 2's live
    /// context-menu wiring does not exist yet, so `Response::context_menu`
    /// is never called on a hit and no popup ever opens.
    #[test]
    fn right_click_on_a_node_opens_the_context_menu() {
        use egui_kittest::kittest::Queryable as _;
        let (mut harness, positions, _mirrors) = settle_menu_harness(false);
        let menu_target_pos = menu_pos_of(&positions, "a1");

        right_click_no_movement(&mut harness, menu_target_pos);

        assert!(
            egui::Popup::is_any_open(&harness.ctx),
            "right-clicking a node must open a popup"
        );
        assert!(
            harness
                .query_by_label(crate::context_menu::OPEN_FILE_LABEL)
                .is_some(),
            "the menu must show the '{}' item",
            crate::context_menu::OPEN_FILE_LABEL
        );
    }

    /// Right-clicking empty canvas (a point provably far from every
    /// published node position, asserted as a setup guard) opens no popup
    /// and shows no menu item. **Passes today** -- it is a lock, and it
    /// must still pass at the end of Task 2, which is the harder half.
    #[test]
    fn right_click_on_empty_canvas_opens_nothing() {
        use egui_kittest::kittest::Queryable as _;
        let (mut harness, positions, _mirrors) = settle_menu_harness(false);

        // Top-left corner of the default 800x600 egui_kittest viewport --
        // this 6-node fixture's unfocused layout clusters near the canvas
        // centre, so this corner is comfortably far from every node.
        let empty_spot = egui::Pos2::new(15.0, 15.0);
        let min_dist = positions
            .iter()
            .map(|(_, p)| (*p - empty_spot).length())
            .fold(f32::INFINITY, f32::min);
        assert!(
            min_dist > 100.0,
            "setup guard: {empty_spot:?} must be far from every published node position, \
             closest was {min_dist} -- positions={positions:?}"
        );

        right_click_no_movement(&mut harness, empty_spot);

        assert!(
            !egui::Popup::is_any_open(&harness.ctx),
            "right-clicking empty canvas must open no popup at all"
        );
        assert!(
            harness
                .query_by_label(crate::context_menu::OPEN_FILE_LABEL)
                .is_none(),
            "no menu item may be findable after a miss"
        );
    }

    /// A right-click leaves no residue in the trace gesture machinery, in
    /// EITHER trace mode: `app.trace` stays `None`, the gesture stays
    /// `Idle`, and no `PressCapture` survives it -- the direct lock on
    /// probe conclusion 2. **Passes today.**
    #[test]
    fn right_click_leaves_no_trace_residue_in_either_mode() {
        for trace_mode in [true, false] {
            let (mut harness, positions, mirrors) = settle_menu_harness(trace_mode);
            let menu_target_pos = menu_pos_of(&positions, "a1");

            right_click_no_movement(&mut harness, menu_target_pos);

            assert!(
                harness.state().trace.is_none(),
                "trace_mode={trace_mode}: a right-click must never start or complete a trace, \
                 got {:?}",
                harness.state().trace
            );
            assert_eq!(
                *mirrors.gesture.borrow(),
                crate::trace::TraceGesture::Idle,
                "trace_mode={trace_mode}: the trace gesture must be Idle after a right-click"
            );
            assert_eq!(
                *mirrors.press_capture.borrow(),
                None,
                "trace_mode={trace_mode}: no press capture may survive a right-click"
            );
        }
    }

    /// This plan's headline regression lock: a real right-DRAG between two
    /// nodes still traces exactly as 05-18 shipped it, AND no popup ever
    /// opens at any step of the drag (recorded per-step, not sampled at one
    /// frame). **Passes today.**
    #[test]
    fn right_drag_between_two_nodes_still_traces_and_opens_no_menu() {
        let (mut harness, positions, _mirrors) = settle_menu_harness(true);
        let menu_target_pos = menu_pos_of(&positions, "a1");
        let menu_other_pos = menu_pos_of(&positions, "c1");
        assert!(
            (menu_target_pos - menu_other_pos).length() > 6.0,
            "a1 and c1 must be further apart than egui's 6pt click threshold for this to be a \
             genuine drag, got a1={menu_target_pos:?} c1={menu_other_pos:?}"
        );

        let mut popup_open_per_step: Vec<bool> = Vec::new();

        harness
            .input_mut()
            .events
            .push(egui::Event::PointerMoved(menu_target_pos));
        harness.step();
        popup_open_per_step.push(egui::Popup::is_any_open(&harness.ctx));

        harness.input_mut().events.push(egui::Event::PointerButton {
            pos: menu_target_pos,
            button: egui::PointerButton::Secondary,
            pressed: true,
            modifiers: egui::Modifiers::default(),
        });
        harness.step();
        popup_open_per_step.push(egui::Popup::is_any_open(&harness.ctx));

        let midpoint = menu_target_pos + (menu_other_pos - menu_target_pos) * 0.5;
        harness
            .input_mut()
            .events
            .push(egui::Event::PointerMoved(midpoint));
        harness.step();
        popup_open_per_step.push(egui::Popup::is_any_open(&harness.ctx));

        harness
            .input_mut()
            .events
            .push(egui::Event::PointerMoved(menu_other_pos));
        harness.step();
        popup_open_per_step.push(egui::Popup::is_any_open(&harness.ctx));

        harness.input_mut().events.push(egui::Event::PointerButton {
            pos: menu_other_pos,
            button: egui::PointerButton::Secondary,
            pressed: false,
            modifiers: egui::Modifiers::default(),
        });
        harness.step();
        popup_open_per_step.push(egui::Popup::is_any_open(&harness.ctx));

        assert!(
            popup_open_per_step.iter().all(|&open| !open),
            "no popup may ever open during a right-drag -- per-step popup-open states: \
             {popup_open_per_step:?}"
        );

        let trace = harness.state().trace.clone().unwrap_or_else(|| {
            panic!(
                "app.trace must be Some after a right-drag from a1 to c1 with the context menu \
                 installed -- per-step popup-open states: {popup_open_per_step:?}"
            )
        });
        assert_eq!(trace.from, "a1");
        assert_eq!(trace.to, "c1");
        assert!(
            trace.path.is_some(),
            "a1 -> c1 has a direct edge in the fixture; the trace must resolve a path"
        );
    }

    /// Module-local lock serializing this file's two settings-touching
    /// tests against each other, mirroring `settings_panel.rs`'s
    /// `settings_store_test_lock` precedent (05-22 Deviations) -- 05-21's
    /// `settings::current`/`store` are a single process-wide `OnceLock`, so
    /// concurrent writers in the same test binary can race. This lock only
    /// covers this module's own two settings-touching tests (it cannot
    /// reach into `settings_panel.rs`'s private lock, and that file is not
    /// touched by this plan); the residual cross-module race is the same
    /// pre-existing condition 05-22 already documented, not introduced
    /// here.
    fn context_menu_settings_test_lock() -> &'static std::sync::Mutex<()> {
        static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
        LOCK.get_or_init(|| std::sync::Mutex::new(()))
    }

    /// Activating "Open file" on a node with a real source file records
    /// exactly the argv `context_menu::plan_open` would produce for it --
    /// covering the launch without launching an editor (see `argv_probe`'s
    /// doc comment for the honest scope of what this test does and does
    /// not cover). **Fails today** -- there is no menu item to activate.
    #[test]
    fn activating_open_file_spawns_the_configured_argv() {
        use egui_kittest::kittest::Queryable as _;
        let _guard = context_menu_settings_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let previous = crate::settings::current();
        let configured = crate::settings::Settings {
            open_file_command: "code -g".to_string(),
            append_line_number: false,
        };
        crate::settings::store(configured.clone());

        let (mut harness, positions, _mirrors) = settle_menu_harness(false);
        let menu_target_pos = menu_pos_of(&positions, "a1");
        right_click_no_movement(&mut harness, menu_target_pos);

        harness
            .get_by_label(crate::context_menu::OPEN_FILE_LABEL)
            .click();
        harness.step();

        let recorded = argv_probe::load(&harness.ctx);
        let graph_dir = crate::load::graph_dir();
        // Restore settings before any assertion that might panic, so a
        // failing assertion doesn't poison shared state for a later test.
        crate::settings::store(previous);

        let recorded = recorded.unwrap_or_else(|| {
            panic!("activating Open file must record an argv into the test-only argv probe")
        });
        let expected = crate::context_menu::plan_open(
            Some("src/auth/login.rs"),
            Some(42),
            graph_dir.as_deref(),
            &configured,
        );
        let crate::context_menu::MenuAction::Spawn(expected_argv) = expected else {
            panic!(
                "plan_open must produce Spawn for a1 with a configured command, got {expected:?}"
            );
        };
        assert_eq!(
            recorded, expected_argv,
            "the recorded argv must be exactly what plan_open produces for the same inputs"
        );
    }

    /// A node with no recorded source file (`b1` in the fixture) still gets
    /// a menu -- disabled, with `NO_SOURCE_HINT` visible -- and activating
    /// the item (if it is even hittable) never records an argv.
    /// **Fails today** -- there is no menu at all yet.
    #[test]
    fn open_file_is_disabled_for_a_node_with_no_source_file() {
        use egui_kittest::kittest::Queryable as _;
        let (mut harness, positions, _mirrors) = settle_menu_harness(false);
        let menu_target_pos = menu_pos_of(&positions, "b1");

        right_click_no_movement(&mut harness, menu_target_pos);

        assert!(
            egui::Popup::is_any_open(&harness.ctx),
            "the menu must still open for a node with no source file -- disabled, not silent"
        );
        assert!(
            harness
                .query_by_label(crate::context_menu::NO_SOURCE_HINT)
                .is_some(),
            "the no-source hint must be visible"
        );

        if let Some(item) = harness.query_by_label(crate::context_menu::OPEN_FILE_LABEL) {
            item.click();
        }
        harness.step();

        assert!(
            argv_probe::load(&harness.ctx).is_none(),
            "activating a disabled Open file item (if it is even hittable) must never record \
             an argv"
        );
    }

    // ============================================================
    // Plan 15 (05-15): the refit-follow's pure predicates and its
    // live-rendered geometric behaviour -- the two geometric tests are the
    // only tests in this plan that can see where nodes actually rendered;
    // they need `test_probe`, which is `cfg(test)`-gated and unreachable
    // from `tests/canvas.rs`'s integration tests.
    // ============================================================

    const REFIT_TEST_VIEWPORT: egui::Vec2 = egui::vec2(1200.0, 800.0);

    fn refit_test_app_from(json: &str) -> crate::app::SeamExplorerApp {
        let ingest = seam_core::from_json(json).expect("fixture must ingest cleanly");
        crate::app::SeamExplorerApp {
            model: Some(ingest.model),
            ..Default::default()
        }
    }

    /// Mirror of each frame's published node screen positions
    /// (`test_probe::load_node_screen_positions`) -- named alias so
    /// `refit_test_harness`'s signature stays under clippy's
    /// `type_complexity` threshold.
    type RefitTestPositions = std::rc::Rc<std::cell::RefCell<Vec<(String, egui::Pos2)>>>;

    /// Builds a harness rendering `show()` alone at `REFIT_TEST_VIEWPORT`
    /// over a model ingested from `json`, stepping once BEFORE focus is set
    /// so the next focus assignment is a genuine change
    /// `render_focus_changed` can observe. Setting focus before
    /// construction would leave no previous snapshot to differ from, so
    /// nothing would arm -- the single easiest way to write a test that
    /// passes for the wrong reason (05-15-PLAN.md Task 2 action text).
    /// Returns the harness plus a mirror of each frame's published node
    /// screen positions.
    fn refit_test_harness_from(
        json: &str,
    ) -> (
        egui_kittest::Harness<'static, crate::app::SeamExplorerApp>,
        RefitTestPositions,
    ) {
        let positions_mirror: RefitTestPositions =
            std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let positions_inner = positions_mirror.clone();
        let mut harness = egui_kittest::Harness::builder()
            .with_size(REFIT_TEST_VIEWPORT)
            .build_ui_state(
                move |ui, app: &mut crate::app::SeamExplorerApp| {
                    show(ui, app);
                    *positions_inner.borrow_mut() = test_probe::load_node_screen_positions(ui);
                },
                refit_test_app_from(json),
            );
        harness.step();
        (harness, positions_mirror)
    }

    fn refit_test_harness() -> (
        egui_kittest::Harness<'static, crate::app::SeamExplorerApp>,
        RefitTestPositions,
    ) {
        refit_test_harness_from(CLEAN_FIXTURE)
    }

    /// After focusing a seam and letting the follow run past
    /// `FOLLOW_FRAME_CAP`, every rendered node's screen position lies
    /// inside the canvas rect (with a small margin to spare) -- nothing is
    /// framed off screen. The direct regression test for the user's
    /// complaint: the click alone must produce a real, on-screen framing.
    #[test]
    fn focused_follow_frames_every_node_inside_the_canvas() {
        let (mut harness, positions_mirror) = refit_test_harness();

        harness.state_mut().focus = Some(crate::app::FocusState {
            a: "A".to_string(),
            b: "B".to_string(),
        });
        harness.run_steps(FOLLOW_FRAME_CAP as usize + 20);

        let positions = positions_mirror.borrow().clone();
        assert!(
            !positions.is_empty(),
            "position probe published no positions -- fixture failed to ingest or render, \
             not the bug under test"
        );

        let canvas = egui::Rect::from_min_size(egui::Pos2::ZERO, REFIT_TEST_VIEWPORT);
        let margin = 8.0;
        for (id, pos) in &positions {
            assert!(
                canvas.expand(margin).contains(*pos),
                "node {id} at {pos:?} must land inside the canvas ({canvas:?}, {margin}px \
                 margin) once the follow has finished -- nothing should be framed off screen"
            );
        }
    }

    /// After the same sequence, the centre of the rendered nodes' screen
    /// bounding box sits near the centre of the canvas rect -- "centred",
    /// the word the user used ("I need to press reset view to get it
    /// centered").
    #[test]
    fn focused_follow_centers_the_pulled_apart_pair() {
        let (mut harness, positions_mirror) = refit_test_harness();

        harness.state_mut().focus = Some(crate::app::FocusState {
            a: "A".to_string(),
            b: "B".to_string(),
        });
        harness.run_steps(FOLLOW_FRAME_CAP as usize + 20);

        let positions = positions_mirror.borrow().clone();
        assert!(
            !positions.is_empty(),
            "position probe published no positions"
        );

        let mut screen_bounds = egui::Rect::NOTHING;
        for (_, pos) in &positions {
            screen_bounds.extend_with(*pos);
        }
        let bbox_center = screen_bounds.center();
        let canvas_center =
            egui::Rect::from_min_size(egui::Pos2::ZERO, REFIT_TEST_VIEWPORT).center();

        // Generous tolerance: fit_view's construction maps the fitted
        // bounds' centre to the viewport centre exactly (see its doc
        // comment), but a few more frames run past FOLLOW_FRAME_CAP let the
        // layout's slow post-settle drift (the plateau in
        // `05-15-PLAN.md`'s <design_decision>) move it slightly since the
        // follow stopped writing. 15% of the shorter viewport dimension is
        // comfortably tighter than "near an edge" (450+px away) while
        // tolerating that drift.
        let tolerance = REFIT_TEST_VIEWPORT.y * 0.15;
        assert!(
            (bbox_center - canvas_center).length() < tolerance,
            "the pulled-apart pair's bounding-box centre {bbox_center:?} must land near the \
             canvas centre {canvas_center:?} (within {tolerance}px), got distance {}",
            (bbox_center - canvas_center).length()
        );
    }

    /// `bounds_settled` returns false at the step-20 delta the planner
    /// probe measured (~3.81px on the demo fixture -- well above
    /// `FOLLOW_SETTLED_EPSILON`) and true at the step-40 delta (~0.24px --
    /// below it), pinning the epsilon to the measurement rather than to
    /// taste.
    #[test]
    fn bounds_settled_pins_epsilon_to_the_measured_convergence() {
        let previous = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(100.0, 100.0));

        let step_20 = egui::Rect::from_min_max(egui::pos2(3.81, 0.0), egui::pos2(100.0, 100.0));
        assert!(
            !bounds_settled(previous, step_20),
            "a step-20-scale delta (probe: ~3.81px) must not be reported as settled"
        );

        let step_40 = egui::Rect::from_min_max(egui::pos2(0.24, 0.0), egui::pos2(100.0, 100.0));
        assert!(
            bounds_settled(previous, step_40),
            "a step-40-scale delta (probe: ~0.24px) must be reported as settled"
        );
    }

    /// Degenerate input (an empty rect, a non-finite rect) is handled by
    /// `bounds_settled` without panicking, and is never reported as
    /// settled -- a degenerate "previous" has nothing real to compare
    /// against.
    #[test]
    fn bounds_settled_handles_degenerate_input_without_panicking() {
        let finite = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(10.0, 10.0));
        let empty = egui::Rect::NOTHING;
        let nan = egui::Rect::from_min_max(egui::pos2(f32::NAN, 0.0), egui::pos2(10.0, 10.0));

        assert!(!bounds_settled(empty, finite));
        assert!(!bounds_settled(finite, empty));
        assert!(!bounds_settled(nan, finite));
        assert!(!bounds_settled(finite, nan));
    }

    // ============================================================
    // Plan 15 Task 3: user_took_over's own noise-vs-real-gesture split, the
    // follow's unconditional termination at FOLLOW_FRAME_CAP, and re-arming
    // cleanly on a second focus mid-follow.
    // ============================================================

    /// Float round-trip noise at the scale of `PAN_EPSILON`/`ZOOM_EPSILON`
    /// must NOT count as takeover (a follow left alone runs to its natural
    /// convergence); a real drag's magnitude (the measured `+60, +40`
    /// screen-px delta `tests/canvas.rs`'s `synthetic_drag` produces) MUST.
    #[test]
    fn user_took_over_ignores_round_trip_noise_but_catches_a_real_drag() {
        let written = crate::app::ViewState {
            zoom: 1.5,
            pan: egui::vec2(120.0, -80.0),
        };

        let noisy = crate::app::ViewState {
            zoom: written.zoom + ZOOM_EPSILON * 0.5,
            pan: written.pan + egui::vec2(PAN_EPSILON * 0.5, 0.0),
        };
        assert!(
            !user_took_over(written, noisy),
            "float round-trip noise at the scale of PAN_EPSILON/ZOOM_EPSILON must not be \
             reported as takeover, got written={written:?} noisy={noisy:?}"
        );

        let dragged = crate::app::ViewState {
            zoom: written.zoom,
            pan: written.pan + egui::vec2(60.0, 40.0),
        };
        assert!(
            user_took_over(written, dragged),
            "a real drag's magnitude must be reported as takeover, got written={written:?} \
             dragged={dragged:?}"
        );
    }

    /// The follow terminates unconditionally at `FOLLOW_FRAME_CAP` even
    /// when nothing settles: stepping well past the cap with no user input
    /// leaves the follow no longer live -- observable as `app.view` no
    /// longer changing across consecutive frames, and a subsequent manual
    /// view assignment surviving the next frame instead of being
    /// overwritten by a follow that refused to end.
    #[test]
    fn follow_terminates_at_the_frame_cap_with_no_user_input() {
        let (mut harness, _positions) = refit_test_harness();

        harness.state_mut().focus = Some(crate::app::FocusState {
            a: "A".to_string(),
            b: "B".to_string(),
        });
        harness.run_steps(FOLLOW_FRAME_CAP as usize + 20);

        let before = harness.state().view;
        harness.step();
        let after = harness.state().view;
        assert!(
            (before.pan - after.pan).length() < 1e-3 && (before.zoom - after.zoom).abs() < 1e-5,
            "the follow must have stopped writing app.view by the time FOLLOW_FRAME_CAP has \
             elapsed with no user input, got before={before:?} after={after:?}"
        );

        // A subsequent manual view assignment must survive the next frame
        // -- if the follow were still live, its next write would overwrite
        // it.
        let manual = crate::app::ViewState {
            zoom: 3.0,
            pan: egui::vec2(123.0, -45.0),
        };
        harness.state_mut().view = manual;
        harness.step();
        let survived = harness.state().view;
        assert!(
            (survived.pan - manual.pan).length() < 1e-3
                && (survived.zoom - manual.zoom).abs() < 1e-5,
            "a manual view assignment after the follow has terminated must survive the next \
             frame, got manual={manual:?} survived={survived:?}"
        );
    }

    /// `clean.json`'s three communities are all the same size (2 nodes
    /// each), so ANY two-community focus settles to a geometrically
    /// identical bounding shape (same per-side node count, same jitter/
    /// repulsion formula) -- confirmed empirically while writing this test:
    /// focusing (A,B) and (B,C) on `clean.json` produced byte-identical
    /// `ViewState`s. A fixture with deliberately DIFFERENT per-community
    /// node counts (A: 1, B: 3, C: 5) is needed so two different focus
    /// pairs actually settle to distinguishable framings -- otherwise this
    /// test's guard assertion could never pass, real re-arm bug or not.
    const REARM_FIXTURE: &str = r#"{"nodes":[
        {"id":"a1","community":"A"},
        {"id":"b1","community":"B"},{"id":"b2","community":"B"},{"id":"b3","community":"B"},
        {"id":"c1","community":"C"},{"id":"c2","community":"C"},{"id":"c3","community":"C"},{"id":"c4","community":"C"},{"id":"c5","community":"C"}
    ],"links":[
        {"source":"a1","target":"b1","relation":"calls","confidence":"EXTRACTED"},
        {"source":"b1","target":"c1","relation":"calls","confidence":"EXTRACTED"},
        {"source":"b2","target":"c2","relation":"calls","confidence":"EXTRACTED"},
        {"source":"b3","target":"c3","relation":"calls","confidence":"EXTRACTED"}
    ]}"#;

    /// Settles `focus` alone on a fresh `REARM_FIXTURE` harness and returns
    /// the final `app.view` -- shared setup for the re-arm test's guard
    /// assertion and its main comparison.
    fn settle_focused_view(focus: &crate::app::FocusState) -> crate::app::ViewState {
        let (mut harness, _positions) = refit_test_harness_from(REARM_FIXTURE);
        harness.state_mut().focus = Some(focus.clone());
        harness.run_steps(FOLLOW_FRAME_CAP as usize + 20);
        harness.state().view
    }

    /// Focusing a second seam while a follow from the first is still
    /// running re-arms cleanly and frames the NEW pair, rather than being
    /// ignored or blending the two. A guard assertion up front proves the
    /// two pairs' settled framings are actually distinguishable (see
    /// `REARM_FIXTURE`'s doc comment) -- without that, this test could pass
    /// even if the re-arm were broken.
    #[test]
    fn refocusing_mid_follow_frames_the_new_pair_not_the_old() {
        let focus_ab = crate::app::FocusState {
            a: "A".to_string(),
            b: "B".to_string(),
        };
        let focus_bc = crate::app::FocusState {
            a: "B".to_string(),
            b: "C".to_string(),
        };

        let view_ab = settle_focused_view(&focus_ab);
        let view_bc = settle_focused_view(&focus_bc);
        let guard_diff = (view_ab.pan - view_bc.pan).length() + (view_ab.zoom - view_bc.zoom).abs();
        assert!(
            guard_diff > 1.0,
            "focusing (A,B) and (B,C) must settle to distinguishable views for this test to be \
             meaningful, got view_ab={view_ab:?} view_bc={view_bc:?}"
        );

        let (mut harness, _positions) = refit_test_harness_from(REARM_FIXTURE);
        harness.state_mut().focus = Some(focus_ab);
        harness.run_steps(10); // partway -- the follow is still live

        harness.state_mut().focus = Some(focus_bc);
        harness.run_steps(FOLLOW_FRAME_CAP as usize + 20);

        let final_view = harness.state().view;
        let dist_to_bc =
            (final_view.pan - view_bc.pan).length() + (final_view.zoom - view_bc.zoom).abs();
        let dist_to_ab =
            (final_view.pan - view_ab.pan).length() + (final_view.zoom - view_ab.zoom).abs();
        assert!(
            dist_to_bc < dist_to_ab,
            "refocusing mid-follow must frame the NEW pair (B,C), not stay stuck on the old \
             pair (A,B)'s framing -- got final_view={final_view:?}, distance to (B,C) \
             settle={dist_to_bc}, distance to (A,B) settle={dist_to_ab}"
        );
    }
}
