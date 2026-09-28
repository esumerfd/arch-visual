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
const EDGE_HEX: &str = "#9dabc7";
const TEXT_HEX: &str = "#dfe6f2";
/// The `--seam` accent token (05-UI-SPEC.md Color table) -- same hex
/// `overlay`, `detail`, and `seam_list` already use for this app's accent
/// role. quick-260915-sf7 reused it for the selected-node ring rather than
/// inventing a new colour.
///
/// Correction (quick-260927-iy9, `<design_decision>` 7): this token now
/// means ONLY the trace-armed ring -- the search-jump/bridge-row ring moved
/// to its own colour, `JUMP_RING_HEX`. This constant is deliberately NOT
/// renamed and NOT recoloured (its value and `SELECTED_RING_WIDTH` stay
/// byte-for-byte the shipped value), even though the name now under-
/// describes its narrowed meaning -- see `trace_armed_ring_color()`, the
/// other half of the pair.
const SELECTED_RING_HEX: &str = "#ff4d8d";
/// Selected-ring stroke width -- noticeably heavier than the bridge
/// stroke's `2.0` (quick-260915-sf7) so the highlight reads as unmistakable
/// rather than merely technically true. Shared by both rings (the jump ring
/// keeps this exact width; the trace-armed ring keeps it too, just pushed
/// out to a concentric outer radius -- `TRACE_RING_OFFSET` below).
const SELECTED_RING_WIDTH: f32 = 4.0;
/// The trace-armed ring's outer radius offset from the node's own scaled
/// radius: half the inner (jump-ring) stroke width, a 2.0pt visible gap,
/// half the outer (trace-armed-ring) stroke width --
/// `SELECTED_RING_WIDTH / 2.0 + 2.0 + SELECTED_RING_WIDTH / 2.0`, which is
/// `SELECTED_RING_WIDTH + 2.0` (6.0 at the current width). Written as that
/// expression, not the literal `6.0`, so it stays correct if
/// `SELECTED_RING_WIDTH` ever moves.
///
/// Correction (quick-260927-rmx, reversing quick-260927-iy9's
/// `<design_decision>` 3): this value is now a CANVAS-space quantity,
/// consumed through the SAME `ctx.meta.canvas_to_screen_size` call that
/// scales the node's own radius one line above -- not applied raw after
/// scaling. iy9's doc block claimed the raw-after-scaling approach kept the
/// gap "the same screen size at every zoom", which is the opposite of what
/// real-display testing found: at `MIN_ZOOM` a flat 6.0px gap next to a
/// 0.6px node read as a detached halo (10:1 gap-to-radius), and at
/// `MAX_ZOOM` the same flat gap read fine (0.1:1) -- the ratio, not the
/// pixel count, is what a viewer actually perceives. The invariant this
/// value now holds is `TRACE_RING_OFFSET / NODE_RADIUS == 1.0` (both 6.0):
/// the outer ring always sits at exactly twice the node's radius, at every
/// zoom.
///
/// Disclosed cost: stroke WIDTHS (`SELECTED_RING_WIDTH`, and every other
/// stroke in this file) still do NOT scale with zoom -- that is this file's
/// uniform convention and changing it is a separate, much larger task. So
/// when both rings are present at low zoom, two 4pt strokes separated by a
/// sub-pixel gap will visually merge into one thick band. Making the gap
/// proportional is the fix; the low-zoom merge is its accepted consequence,
/// not something engineered around here.
const TRACE_RING_OFFSET: f32 = SELECTED_RING_WIDTH / 2.0 + 2.0 + SELECTED_RING_WIDTH / 2.0;
/// Edge stroke alpha (0-255) -- always this value now that the reduced-
/// opacity focus fade (05-10) is gone; edges are either present (fully
/// visible at this alpha) or absent from the graph entirely, never faded.
const EDGE_ALPHA: u8 = 255;
/// Edge stroke width, in points. Must stay strictly below `3.0` --
/// `SeamEdgeShape::is_inside`'s click-tolerance floors at `3.0` via the
/// stroke width, so any value `>= 3.0` would silently widen edge
/// hit-testing as a side effect of a purely visual tuning change
/// (quick-260926-gh2).
const EDGE_WIDTH: f32 = 2.25;
/// Arrowhead tip size, in points, before the per-frame `ctx.meta.zoom`
/// scale factor is applied (quick-260926-gh2).
const ARROW_TIP_SIZE: f32 = 9.5;
const NODE_RADIUS: f32 = 6.0;
const LABEL_MAX_CHARS: usize = 24;

fn hex(h: &str) -> egui::Color32 {
    egui::Color32::from_hex(h).expect("valid hex")
}

/// The single source of the edge line/arrowhead/crossing-count-label
/// colour -- `SeamEdgeShape::shapes` calls this instead of re-deriving the
/// colour inline, so the line, its arrowhead, and its crossing-count
/// galley are guaranteed to share one value, and so a test can measure the
/// real production colour instead of a re-derived literal (quick-260926-gh2).
fn edge_stroke_color() -> egui::Color32 {
    let base = hex(EDGE_HEX);
    egui::Color32::from_rgba_unmultiplied(base.r(), base.g(), base.b(), EDGE_ALPHA)
}

/// The arrowhead's own colour token -- a distinct green, derived (not
/// picked) as the CIELAB hue bisector of the two focused-seam side tints
/// (`SIDE_A_HEX` / `SIDE_B_HEX`), so it cannot be read as belonging to
/// either side of a focused seam (quick-260926-nnr). Before this task the
/// arrowhead shared the edge line's colour exactly, which is why raising
/// that shared colour's contrast uniformly (quick-260926-gh2) did not make
/// direction any easier to follow -- a same-coloured triangle at the end of
/// a same-coloured line is still one visual object. This value is
/// iso-luminant with the edge line to five decimal places (relative
/// luminance differs by roughly 0.0000062), so every bit of the added
/// salience is chromatic and none of it is brightness: a future edit that
/// brightens this token is exactly what the luminance-parity guard test
/// exists to stop. It also deliberately still clears the same 1.8:1
/// separation from the near-white node-label text that the edge line
/// clears, even though that floor was written for a thin achromatic stroke
/// under glyphs rather than a chromatic mark -- weakening a shipped guard
/// as a side effect of an unrelated change is not done here. Two
/// colour-vision-deficiency collisions are known and accepted rather than
/// engineered away: for deuteranope/protanope viewers this green converges
/// with the side-B orange tint, and for the (very rare) tritanope it
/// converges with the side-A teal tint -- see the CVD test below.
const ARROW_HEAD_HEX: &str = "#69bf28";

/// The single source of the arrowhead's fill colour -- a second named
/// colour source beside `edge_stroke_color()`, so a test can measure the
/// real production arrowhead colour instead of re-deriving a literal
/// (quick-260926-nnr). Routed through the plain, non-premultiplying hex
/// parser: a filled triangle is never translucent, so there is no alpha to
/// tune, and adding one here would recreate the exact premultiplication
/// trap quick-260926-gh2 removed from the edge line.
fn arrow_head_color() -> egui::Color32 {
    hex(ARROW_HEAD_HEX)
}

/// The jumped-to (search result / bridge row) node's ring colour --
/// derived, not picked, under the quick-260926-gh2/nnr discipline
/// (`<design_decision>` 2 of quick-260927-iy9). Measured against the
/// production `SELECTED_RING_HEX` (the trace-armed ring, unchanged by this
/// task): ΔE76 90.02 and 86.04 degrees of CIELAB hue separation (both 0.00
/// before -- the two states were literally the same colour, the reported
/// defect). Reads as blue: hue 277.37 degrees, chroma 60.21. Clears 45.2 /
/// 45.1 ΔE76 against the blue-grey `EDGE_HEX`/`DIMMED_FILL_HEX` tokens that
/// already sit within 3 degrees of any blue's hue (chroma is the honest
/// discriminator there, not hue distance). Clears 5.551:1 WCAG contrast
/// over the canvas fill `egui::Visuals::dark().panel_fill` -- slightly
/// BETTER than the armed ring's own 5.494:1, so the derived floor is parity
/// with the ring it sits beside, not gh2/nnr's unreachable 7.0:1 for a mark
/// this large. 2.473:1 separated from `TEXT_HEX`. Within 0.003456 relative
/// luminance of the armed ring, so the two rings differ by hue and chroma
/// only, never brightness -- neither implies "more important" than the
/// other. Stays distinct under Vienot-1999 colour-blindness simulation:
/// ΔE76 82.64 (deuteranope) / 49.13 (protanope), versus 0.00 today.
const JUMP_RING_HEX: &str = "#3094fc";

/// The jumped-to node's ring colour -- see `JUMP_RING_HEX`'s doc comment
/// for the full derivation and measured figures. `SeamNodeShape::shapes`
/// calls this instead of inlining `hex(JUMP_RING_HEX)`, so a test measures
/// the real production colour instead of a re-derived literal
/// (quick-260926-gh2's lesson).
fn jump_ring_color() -> egui::Color32 {
    hex(JUMP_RING_HEX)
}

/// quick-260927-iy9: the trace-armed node's ring colour -- always
/// `SELECTED_RING_HEX`, the shipped red, byte-for-byte unchanged by this
/// task (`<scope_boundary>`). A separate named function from
/// `jump_ring_color()` so every colour test measures the real production
/// colour that actually reaches each of the two rings, never a literal
/// re-derived in the test (gh2's lesson).
fn trace_armed_ring_color() -> egui::Color32 {
    hex(SELECTED_RING_HEX)
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

/// The one rendered-set membership predicate (quick-260926-nop, DP-NOP-05):
/// extracted VERBATIM from `build_graph`'s own skip condition, and now
/// called BY `build_graph` rather than defining its own copy of this logic.
/// True when `node_visible` passes OR the node's id is in `forced`
/// (260918-ttc's per-node force-inclusion for a resolved trace path). The
/// two production callers are `build_graph`'s node loop (what is rendered)
/// and `seam_list::find_nodes`'s filter (what is findable) -- they read the
/// same two inputs through the same function and so cannot drift apart.
pub fn node_rendered(
    node: &seam_core::Node,
    focus: Option<&crate::app::FocusState>,
    forced: &std::collections::HashSet<String>,
) -> bool {
    node_visible(&node.community, focus) || forced.contains(&node.id)
}

/// Returns the resolved trace path's hop ids -- quick task `260918-ttc`,
/// and the ONLY source of ids `build_graph`'s `forced` parameter is given.
/// The user's rule: when a resolved trace path needs a node outside the
/// focused pair, exactly that node is force-included -- never that node's
/// whole community. Empty when `app.trace` is absent, or present but not
/// yet resolved to a path (`<design_decision>` 4 of 260918-ttc: there is no
/// polyline to draw in either case, and both drag endpoints came from the
/// already-rendered graph, so they are already visible by construction).
/// An id here that is absent from the displayed model is harmless -- this
/// only widens a filter over nodes the model already has; it can never
/// create one.
pub fn forced_visible_ids(app: &SeamExplorerApp) -> std::collections::HashSet<String> {
    app.trace
        .as_ref()
        .and_then(|t| t.path.as_ref())
        .map(|p| p.hops.iter().cloned().collect())
        .unwrap_or_default()
}

/// Pure structural mapping: `seam_core::Model` -> the `egui_graphs::Graph`
/// the widget consumes. Free of `egui::Ui`/`egui::Context` so
/// `unfocused_build_still_covers_every_node_and_edge`/
/// `test_render_mapping_is_scoped` can drive it directly. With `focus ==
/// None`, renders the **entire** graph unconditionally -- no node-count
/// perf safety-valve of any kind (Phase 2 D-01/D-02, ported unchanged;
/// DP-10-04: this filter is user-intent-driven, not node-count-driven).
/// With `focus == Some`, nodes failing `node_visible` are never added
/// unless force-included via `forced` (quick task `260918-ttc`:
/// `forced_visible_ids` feeds this parameter the hop ids of a resolved
/// trace path, so a path that needs a node outside the focused pair still
/// renders it -- and only it, never its community). Note (260918-ttc,
/// finding 3): a force-included node also brings in its edges to other
/// already-rendered nodes, not only the path's own edges -- deliberate,
/// and honest: every such edge is real and both endpoints are genuinely on
/// screen; no node leaks, since the both-endpoints-present rule below is
/// unchanged. Any edge with at least one absent endpoint is never added
/// (05-10 DP-10-02: hiding is filtered at graph construction, not at paint
/// time, so a hidden node cannot be hovered/clicked/dragged/traced).
pub fn build_graph(
    model: &seam_core::Model,
    focus: Option<&crate::app::FocusState>,
    forced: &std::collections::HashSet<String>,
) -> SeamGraph {
    let mut g: SeamGraph = egui_graphs::Graph::new(petgraph::stable_graph::StableGraph::default());
    let mut index_map: std::collections::HashMap<
        petgraph::stable_graph::NodeIndex,
        petgraph::stable_graph::NodeIndex,
    > = std::collections::HashMap::new();

    for idx in model.graph.node_indices() {
        let node = &model.graph[idx];
        // 260918-ttc: a node failing `node_visible` is still added when its
        // id is in `forced` -- surgical per-node inclusion for a resolved
        // trace path, never a whole-community reveal. quick-260926-nop:
        // routed through `node_rendered`, the one rendered-set membership
        // authority -- see its own doc comment.
        if !node_rendered(node, focus, forced) {
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

/// Extracted from `show()`'s `hiding_active(app)`/`app.focus.clone()` pair
/// (quick-260926-nop) so an off-canvas caller (`seam_list::find_nodes`) can
/// ask exactly the same "what is currently rendered" question `build_graph`
/// is handed, without re-deriving it from `app.focus` directly -- this
/// file's own standing comment on that re-derivation risk (discovery
/// finding 3) is why this extraction exists. Behaviourally identical to the
/// pair it replaces: hiding only when a seam is focused (DP-10-03).
pub fn render_focus(app: &SeamExplorerApp) -> Option<crate::app::FocusState> {
    if hiding_active(app) {
        app.focus.clone()
    } else {
        None
    }
}

/// 05-10 DP-10-03: the single place the hiding-suspension conditions live.
/// Any call site that re-derives "should I hide" from `app.focus` directly
/// would drift from this decision and silently diverge from what
/// `build_graph` actually renders -- this function is the only one allowed
/// to make that call. True when (a) a seam is focused -- nothing to hide
/// against otherwise.
///
/// Case (b) -- trace mode suspending hiding -- was RETIRED by quick task
/// 260918-sgx: focus is the scaling strategy for large graphs, and a mode
/// toggle must not discard it. Trace mode is no longer consulted here.
///
/// Case (c) -- a resolved trace result suspending hiding -- was RETIRED by
/// quick task `260918-ttc`. The concern case (c) existed for -- a traced
/// path hop the graph cannot resolve, silently truncating the polyline --
/// is now handled by `forced_visible_ids` feeding `build_graph`'s `forced`
/// parameter instead: the specific node(s) a resolved path needs outside
/// the focused pair are force-included, never their whole community. Both
/// historical suspension cases (b) and (c) are now retired; only case (a)
/// remains, so this is now a single boolean conjunct.
pub fn hiding_active(app: &SeamExplorerApp) -> bool {
    app.focus.is_some()
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
    /// Set by `apply_focus_styling` via `display_mut()` (same channel
    /// `is_bridge` already uses) from `app.selected_node` -- drives the
    /// INNER jump ring in `shapes()`, independent of `is_trace_armed`
    /// (quick-260927-iy9, replacing the old OR'd `selected` ring flag).
    pub is_jump_selected: bool,
    /// Set by `apply_focus_styling` via `display_mut()` from
    /// `app.trace_gesture.armed_node()` -- drives the OUTER trace-armed ring
    /// in `shapes()`, independent of `is_jump_selected` (quick-260927-iy9,
    /// replacing the old OR'd `selected` ring flag).
    pub is_trace_armed: bool,
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
            is_jump_selected: false,
            is_trace_armed: false,
            radius: NODE_RADIUS,
        }
    }
}

impl DisplayNode<PayloadNode, PayloadEdge, Directed, DefaultIx> for SeamNodeShape {
    fn closest_boundary_point(&self, dir: egui::Vec2) -> egui::Pos2 {
        self.pos + dir.normalized() * self.radius
    }

    fn shapes(&mut self, ctx: &DrawContext) -> Vec<egui::Shape> {
        let mut shapes = Vec::with_capacity(3);
        let center = ctx.meta.canvas_to_screen_pos(self.pos);
        let radius = ctx.meta.canvas_to_screen_size(self.radius);

        // quick-260927-iy9: the node circle's own stroke is now driven
        // SOLELY by `is_jump_selected` (then `is_bridge`, then none) -- it
        // no longer reads `self.selected` for any ring
        // (`<design_decision>` 5). `is_jump_selected` wins over `is_bridge`
        // when both apply, since a jumped-to node is always also a bridge
        // node (a bridge row is the only way to select a node at all,
        // quick-260915-sf7). The full-label swap (`self.hovered ||
        // self.selected` below) and the on-top paint order still come free
        // from egui_graphs' own deferred drawing of `selected` nodes
        // (`<design_decision>` 5: the built-in flag keeps its OR of both
        // states for exactly that reason, even though neither ring reads it
        // any more).
        let stroke = if self.is_jump_selected {
            egui::Stroke::new(SELECTED_RING_WIDTH, jump_ring_color())
        } else if self.is_trace_armed {
            egui::Stroke::new(SELECTED_RING_WIDTH, trace_armed_ring_color())
        } else if self.is_bridge {
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

        // Correction (quick-260927-rmx, reversing quick-260927-iy9's
        // `<design_decision>` 3): the outer trace-armed ring is now pushed
        // ONLY when a jump ring is also present (`is_jump_selected &&
        // is_trace_armed`), not whenever armed alone. iy9 always drew this
        // second circle so the ring "never jumps position" -- real-display
        // testing found the opposite: with no jump ring beside it, the
        // always-offset ring reads as a detached halo rather than hugging
        // the node. An armed-alone node's red now comes from the node
        // circle's OWN stroke above (the `else if self.is_trace_armed`
        // branch), restoring the pre-iy9 single-ring look. This branch
        // fires only for the both-rings case: blue inner at the base
        // radius, red outer at the scaled offset.
        if self.is_jump_selected && self.is_trace_armed {
            shapes.push(
                egui::epaint::CircleShape {
                    center,
                    radius: radius + ctx.meta.canvas_to_screen_size(TRACE_RING_OFFSET),
                    fill: egui::Color32::TRANSPARENT,
                    stroke: egui::Stroke::new(SELECTED_RING_WIDTH, trace_armed_ring_color()),
                }
                .into(),
            );
        }

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
            width: EDGE_WIDTH,
            tip_size: ARROW_TIP_SIZE,
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

        let color = edge_stroke_color();
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
                arrow_head_color(),
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

/// The pan half of Task 3's redesign (quick-260926-gb2, DP-GB2-03): while
/// Trace mode disables `egui_graphs`' own navigation (`native_zoom_and_pan`
/// in `show`, below), a drag must still pan the canvas -- otherwise an armed
/// trace could never reach a destination node that was off-screen when it
/// was armed, which is the entire defect this redesign exists to fix.
/// Divides by `view.zoom` -- the same `PAN_STEP / view.zoom` convention
/// `keyboard::apply_key` already established -- so a screen-pixel drag and a
/// screen-pixel arrow-key press agree about what a screen pixel means in
/// world space. Guarded exactly like `apply_zoom_factor` above: a non-finite
/// or non-positive `view.zoom` returns `view` completely unchanged, a
/// non-finite `drag_delta` also returns `view` completely unchanged, and a
/// non-finite resulting pan (unreachable from a finite delta and a finite,
/// positive zoom, but checked anyway for the same defence-in-depth reason
/// `apply_zoom_factor` checks it) falls back to the incoming pan rather than
/// ever writing a NaN into `app.view`.
pub fn apply_drag_pan(
    view: crate::app::ViewState,
    drag_delta: egui::Vec2,
) -> crate::app::ViewState {
    if !view.zoom.is_finite() || view.zoom <= 0.0 {
        return view;
    }
    if !drag_delta.x.is_finite() || !drag_delta.y.is_finite() {
        return view;
    }

    let pan = view.pan + drag_delta / view.zoom;

    if pan.x.is_finite() && pan.y.is_finite() {
        crate::app::ViewState {
            zoom: view.zoom,
            pan,
        }
    } else {
        view
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

    // Plan 08-01: the UI-thread drain that used to sit here (EVENT-03, added
    // by 06-02 with no consumer) has moved to `history::drain_and_apply`,
    // called from the top of `app.rs::ui()` -- above the side panels, so a
    // live event and the seam list it changes land in the same frame. There
    // is now exactly ONE drain call site in this crate, and it is not here.
    // Do not name a socket type in this file, in code or in this comment.

    // Plan 09-02: the ONE display read redirected by this plan. The question
    // this guard asks widens from "is a graph loaded" to "is there anything to
    // display", which is the same question whenever nothing is paused; the
    // empty-state label is unchanged. `build_graph` below already reads this
    // same binding, so it renders the reconstructed historical graph while
    // paused with no second edit. The remaining `app.model` reads in this file
    // are plan 09-03's scope -- except one, which is not:
    // `inject_layout_targets`'s pruning read stays LITERAL and unredirected. It
    // prunes persisted node positions against the id set it is handed, and a
    // historical reconstruction has fewer nodes, so redirecting it would
    // permanently delete the settled position of every node the live graph
    // gained after the paused point (09-RESEARCH.md Pitfall 2, T-09-02-02).
    let Some(model) = crate::timeline::display_model(app) else {
        ui.centered_and_justified(|ui| {
            ui.label("Load a graph.json to begin.");
        });
        return;
    };

    // DP-10-03: hide only when a seam is focused -- see this function's own
    // doc comment for why no other call site is permitted to re-derive this
    // decision. One condition now (260918-ttc retired case (c)); a resolved
    // trace no longer changes this value.
    //
    // Plan 15: `render_focus` is an OWNED snapshot (not a borrow of
    // `app.focus`) precisely so it can be reused at the bottom of this
    // function, past several intervening `app.*` mutations, without
    // fighting the borrow checker over a live field borrow spanning a
    // whole-`app` reborrow. `refit_follow_step` below must arm from this
    // same value, not re-derive it from `app.focus` -- after 260918-ttc the
    // two always agree (hiding is active exactly when a seam is focused),
    // so `render_focus` is still passed rather than re-derived purely so
    // the arming trigger keeps reading exactly what `build_graph` was
    // handed. A landing trace deliberately does NOT re-arm the refit
    // (260918-ttc `<design_decision>` 3): only a focus change does.
    let render_focus = render_focus(app);
    // 260918-ttc: the resolved trace path's hop ids, so a path that needs a
    // node outside the focused pair still renders it -- and only it.
    let forced = forced_visible_ids(app);
    let mut graph = build_graph(model, render_focus.as_ref(), &forced);
    apply_focus_styling(&mut graph, app);
    let canvas_rect = ui.available_rect_before_wrap();
    inject_layout_targets(ui, canvas_rect, &graph, app);
    // `canvas_rect` (not `response.rect`, unavailable until after `ui.add`
    // below) is the one viewport value used for both sync legs and the
    // Reset fit -- self-consistency of the centre term matters more than
    // which rect it came from (Plan 08 gap closure).
    let viewport = canvas_rect.size();
    sync_view_into_frame(ui, app.view, viewport);

    // TRACE-01/G-05-4, updated by Task 3 (quick-260926-gb2): while trace
    // mode is on, both flags below must move together.
    // `with_dragging_enabled` gates `egui_graphs`' own node-drag reposition
    // off, so a drag starting on a node cannot move it out of position while
    // Trace mode is on -- the freed-up drag goes to the app's own
    // `apply_drag_pan` branch near the end of this function instead (see its
    // call site below), never to the click-driven trace gesture, which
    // consumes only clicks (`response.clicked()`), never drags.
    // `with_zoom_and_pan_enabled` (Plan 08 gap closure -- previously
    // hardcoded `true`, unlike this file's sibling flag) must be gated the
    // same way: in 0.31.0, disabling dragging also disables
    // `handle_node_drag` entirely, which is the crate's *only* writer of
    // `dragged_node()`, which is in turn `handle_pan`'s *only* guard against
    // claiming a drag as a canvas pan. So with pan left enabled during trace
    // mode, `dragged_node()` could never become `Some`, and the widget's own
    // internal pan handling would claim every primary-button drag --
    // including one starting on a node -- racing the app's own
    // `apply_drag_pan` branch to apply the SAME drag twice. Both flags
    // therefore keep their exact current values (`!app.trace_mode`): node
    // dragging stays off so nothing repositions a node mid-trace, and the
    // widget's own navigation stays off so the app's own pan/zoom branches
    // (below) are the sole consumers of a drag or a zoom gesture while
    // Trace mode is on -- mutually exclusive with `egui_graphs`' own
    // handling by construction, in the same two-branch spirit as the D3
    // original's `dragTraceActive` split (RESEARCH Architecture Diagram,
    // "Drag gesture on a node"), even though neither branch is a trace
    // gesture any more.
    //
    // Plan 19 correction: this flag staying gated on trace mode ALSO
    // disables `egui_graphs`' own `handle_zoom` (the crate has exactly one
    // combined `zoom_and_pan_enabled` toggle, no zoom-only variant --
    // `05-19-PLAN.md` `<discovery_findings>` section 2), which is why a
    // genuine pinch or Cmd/Ctrl+scroll used to do nothing at all while
    // Trace mode was on. That flag is NOT re-enabled here -- doing so would
    // re-arm `handle_pan`/`handle_zoom` and reintroduce the
    // double-application hazard this comment describes above. Instead the
    // pan and zoom halves of that trade are covered by the app's own
    // branches near the end of this function, gated on this exact same
    // `native_zoom_and_pan` binding (read multiple times, never a second
    // independent spelling of the same negation) so the app's own paths and
    // the widget's own paths can never both fire and can never drift apart.
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
    // quick-260915-sf7: publishes this frame's node id -> centre-relative
    // canvas-space jump-target map, read by `panels::detail::bridge_list`
    // on a bridge-row click. NOT cfg(test)-gated -- this one ships. Placed
    // here (immediately after `ui.add` returns, alongside the cfg(test)
    // probe above) because `node.location()` only reflects this frame's
    // layout step once the widget has actually run; `viewport` is the same
    // binding `sync_view_into_frame`/`view_to_frame` use a few lines above,
    // so the two stay self-consistent (show()'s own comment at lines
    // 821-824 makes this the rule for every viewport-derived term).
    publish_node_jump_targets(ui, &graph, viewport);

    // Overlay drawn strictly after the GraphView widget so it composites on
    // top (D-13) -- only when a seam is focused.
    if let Some(focus) = &app.focus {
        if let Some(detail) = &app.detail {
            crate::overlay::paint_seam_line(ui, canvas_rect, response.rect, &detail.verdict);
            // 05-27 fix (WINDOWS.md entry 5 / UAT test 12): carry each
            // crossing edge's REAL per-node canvas position through to the
            // paint call, via the same `graph.edge_endpoints`/`graph.node(..)
            // .location()` accessors `test_probe::publish_node_screen_positions`/
            // `find_node_screen_pos`/`hit_test_node` already read successfully
            // elsewhere in this file (this task's own `<precondition>`) --
            // NOT the formula-derived x / hardcoded center-y the old design
            // used. An edge whose endpoint lookup unexpectedly returns `None`
            // is skipped via `filter_map`, never unwrapped/panicked (mirrors
            // `build_graph`'s own "never let an edge dangle onto an absent
            // node" discipline -- T-05-27-02).
            let edges: Vec<_> = graph
                .edges_iter()
                .filter_map(|(edge_idx, e)| {
                    let (s, t) = graph.edge_endpoints(edge_idx)?;
                    let source_pos = graph.node(s)?.location();
                    let target_pos = graph.node(t)?.location();
                    Some((
                        e.payload().source_community.clone(),
                        e.payload().target_community.clone(),
                        source_pos,
                        target_pos,
                    ))
                })
                .collect();
            crate::overlay::paint_crossing_threads(ui, response.rect, &edges, focus);
            crate::overlay::paint_side_labels(ui, canvas_rect, response.rect, model, focus);
        }
    }

    // TRACE-01/02: click-to-arm/click-to-complete gesture handling
    // (quick-260926-gb2 -- `seam_core::trace_path` call on a completed
    // click pair) and the resolved path's canvas highlight. A no-op while
    // trace mode is off.
    handle_trace_gesture(ui, &graph, &response, app);
    // 05-23: the right-click "Open file" context menu -- immediately after
    // the trace gesture so the two gestures read as siblings in the source
    // the way they are siblings at the input layer (see
    // `handle_context_menu`'s own doc comment for why no gating is needed).
    handle_context_menu(ui, &graph, &response, app);
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

    // Task 3 (quick-260926-gb2), DP-GB2-03: the pan half of the same trade
    // the branch above covers for zoom. With Trace mode on, `native_zoom_and_pan`
    // disables `egui_graphs`' own `handle_pan` (via `with_zoom_and_pan_enabled`
    // above), so without this branch a canvas armed for a long-range trace
    // could never reach an off-screen destination node -- the whole defect
    // this redesign exists to fix. Reads the SAME `native_zoom_and_pan`
    // binding the widget flag and the trace-mode zoom branch above both
    // read (never a second, independent spelling of the same negation), so
    // the app's own pan and the widget's own pan can never both fire.
    // `with_dragging_enabled(!app.trace_mode)` keeps node dragging off
    // during Trace mode, so no third consumer of this drag exists either
    // (finding 11) -- this call site is mutually exclusive with
    // `egui_graphs::handle_pan` by construction, the same guarantee the
    // trace-mode zoom branch above already has with `handle_zoom`.
    if !native_zoom_and_pan && response.dragged() {
        app.view = apply_drag_pan(app.view, response.drag_delta());
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

/// The `egui::Id` `publish_node_jump_targets`/`node_jump_target` share for
/// this frame's node id -> centre-relative canvas-space offset map
/// (quick-260915-sf7). Module-private -- `node_jump_target` is the only
/// sanctioned way to read it from outside this file.
fn node_jump_targets_id() -> egui::Id {
    egui::Id::new("seam_explorer_node_jump_targets")
}

/// Publishes this frame's `(node id -> centre-relative canvas offset)` map
/// into egui temp data (quick-260915-sf7). Stores `node.location() -
/// viewport / 2` -- ALREADY a valid `JumpTarget::Node` payload (see
/// `JumpTarget`'s own doc comment for the derivation) -- not the raw
/// canvas position, so every line of this coordinate-space contract's
/// algebra stays inside this module, the one place that owns both the
/// canvas geometry and the transform contract; `panels::detail` stays a
/// thin call-through that never does its own offset arithmetic. Called
/// from `show()` immediately after `ui.add` returns (see call site above),
/// same timing `test_probe::publish_node_screen_positions` already uses,
/// so every node's position reflects this frame's completed layout step.
/// Overwrites the previous frame's map outright -- bounded by the
/// currently-rendered node count, which `build_graph` already bounds
/// (T-SF7-01).
fn publish_node_jump_targets(ui: &mut egui::Ui, graph: &SeamGraph, viewport: egui::Vec2) {
    let center = egui::vec2(viewport.x / 2.0, viewport.y / 2.0);
    let targets: std::collections::HashMap<String, egui::Pos2> = graph
        .nodes_iter()
        .map(|(_, n)| (n.payload().id.clone(), n.location() - center))
        .collect();
    ui.data_mut(|d| d.insert_temp(node_jump_targets_id(), targets));
}

/// Resolves `id`'s centre-relative canvas-space jump target from this
/// frame's published map (quick-260915-sf7). Returns `None` when `id` is
/// not present in the currently rendered graph -- discovery finding 6:
/// under ordinary focus-based hiding this is unreachable by construction
/// (a bridge node is always a member of one of the two focused
/// communities), so the only real path here is a timeline scrub leaving
/// `app.detail` describing a moment whose model is no longer displayed.
/// Callers must treat `None` as a SILENT no-op -- no jump, no banner --
/// matching the precedent at `panels::detail::show_trace_result`'s
/// crossed-seam click handling, which swallows an unresolvable seam lookup
/// the same way. The returned `Pos2` is already a valid `JumpTarget::Node`
/// payload; callers must not treat it as a raw canvas position (see
/// `JumpTarget`'s own doc comment).
pub fn node_jump_target(ui: &egui::Ui, id: &str) -> Option<egui::Pos2> {
    let targets: std::collections::HashMap<String, egui::Pos2> = ui
        .data(|d| d.get_temp(node_jump_targets_id()))
        .unwrap_or_default();
    targets.get(id).copied()
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

/// Turns this frame's live click `Response` into a `trace::GestureInput`
/// and feeds it through the pure `trace::update_gesture` state machine --
/// `app.trace_gesture` is both this call's input and its output -- then, on
/// a completed gesture, runs `seam_core::trace_path` and stores the outcome
/// in `app.trace` for the detail panel to render. A no-op while trace mode
/// is off; `egui_graphs`' own node-reposition drag (enabled via
/// `with_dragging_enabled` above) handles that case instead. While armed,
/// also paints the two required D-01 indicators: the armed-state banner
/// (`trace::show_armed_banner`) and a hover preview line to whichever OTHER
/// node the pointer is over (`overlay::paint_rubber_band`, DP-GB2-05) --
/// the armed node's own accent RING is a separate concern, applied by
/// `apply_focus_styling` from `app.trace_gesture.armed_node()` before this
/// function ever runs.
///
/// quick-260926-gb2 replaced the previous continuous drag-to-trace gesture
/// with this click-driven one. A click and a drag are mutually exclusive at
/// egui's input layer -- `Response::clicked()` is true only when
/// `!is_decidedly_dragging()` -- so a drag (which now PANS the canvas while
/// armed, Task 3's `apply_drag_pan`) can never accidentally trace. That
/// mutual exclusivity is also why the whole press-capture dance the former
/// drag gesture needed is gone, not merely unused: `response.clicked()`
/// resolves at release with `interact_pointer_pos()` already populated on
/// that exact frame, so there is no undelayed pointer-down frame to capture
/// and no 6pt click-vs-drag disambiguation window to work around -- that was
/// the former mechanism's entire reason for existing. Primary-only is a
/// requirement here, not an incidental default: `response.clicked()` is
/// primary-button-only by definition in egui, which is exactly right,
/// because the secondary button belongs to the 05-23 "Open file" context
/// menu (`handle_context_menu` below), never to this gesture (DP-GB2-02).
fn handle_trace_gesture(
    ui: &mut egui::Ui,
    graph: &SeamGraph,
    response: &egui::Response,
    app: &mut SeamExplorerApp,
) {
    if !app.trace_mode {
        // D-02's third cancel, free: toggling trace mode off always resets
        // to Idle, and toggling back on never resurrects a stale gesture.
        app.trace_gesture = crate::trace::TraceGesture::Idle;
        return;
    }

    let meta = egui_graphs::MetadataFrame::new(None).load(ui);
    let graph_rect = response.rect;

    // Resolve this frame's input: a primary click hit-tested through the
    // identical fallback chain `handle_context_menu` uses for its own
    // secondary-click hit-test below. No click this frame means no input
    // and no state change.
    let input = if response.clicked() {
        let hit = response
            .interact_pointer_pos()
            .or_else(|| ui.input(|i| i.pointer.hover_pos()))
            .and_then(|p| hit_test_node(graph, &meta, graph_rect, p));
        Some(match hit {
            Some((node, _)) => crate::trace::GestureInput::NodeClick { node },
            None => crate::trace::GestureInput::EmptyClick,
        })
    } else {
        None
    };

    let mut gesture = std::mem::take(&mut app.trace_gesture);
    if let Some(input) = input {
        gesture = crate::trace::update_gesture(gesture, input, app.trace_mode);
    }

    // D-01/DP-GB2-05/DP-GB2-07: while armed, name the source with the
    // banner, and preview the pending completion with a line from the armed
    // node to whichever OTHER node the pointer currently hovers. Nothing is
    // drawn when the pointer is over empty canvas or over the armed node
    // itself -- clicking the armed node cancels (DP-GB2-01), it does not
    // complete, so a preview line to it would promise a trace that will not
    // happen.
    if let crate::trace::TraceGesture::Armed { from } = &gesture {
        let name = crate::panels::detail::node_label(app, from);
        crate::trace::show_armed_banner(ui, &name);

        let hovered = response
            .hover_pos()
            .and_then(|p| hit_test_node(graph, &meta, graph_rect, p));
        if let Some((hovered_id, hovered_screen)) = hovered {
            if &hovered_id != from {
                if let Some(from_screen) = find_node_screen_pos(graph, &meta, graph_rect, from) {
                    crate::overlay::paint_rubber_band(ui, from_screen, hovered_screen);
                }
            }
        }
    }

    if let crate::trace::TraceGesture::Completed { from, to } = &gesture {
        // Plan 09-03, 09-RESEARCH.md Open Question 2 (first site): the search
        // runs over the graph the two clicked nodes CAME FROM. While paused
        // that is the reconstruction; running it against the live model would
        // draw a path over a canvas whose edges do not support it -- a path
        // between nodes that, on screen, do not connect. That is the same class
        // of lie `history::clear_stale_selection`'s first rule exists to
        // prevent.
        if let Some(model) = crate::timeline::display_model(app) {
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

    app.trace_gesture = gesture;
}

/// The right-click-a-node "Open file" context menu (05-23) -- the live
/// half; `context_menu::plan_open`/`plan_open_with` are the pure half (see
/// that module's doc comment). Sits beside `handle_trace_gesture` because
/// both need the same private `hit_test_node` and the same `Response`.
///
/// Four things a future reader would otherwise have to rediscover:
///
/// 1. **A click and a drag can never collide.** egui 0.35's
///    `interaction.rs` `Released` arm sets `clicked` (and therefore
///    `secondary_clicked()`) only when `!input.pointer.is_decidedly_dragging()`,
///    so `secondary_clicked()` is never true during a drag and
///    `drag_started_by(Secondary)` is never true during a plain click (this
///    plan's `<probe_results>`, conclusion 1). That single condition is why
///    this function can be installed on the exact same `response` as
///    `handle_trace_gesture` above with no gating and no mode check.
/// 2. **Deliberately NOT gated on `app.trace_mode`.** The user's spec never
///    mentions trace mode, and (per point 1) a right-click can never be
///    mistaken for a right-drag, so reserving the button for tracing while
///    trace mode is on would remove a capability for no safety benefit
///    (`<design_decision>` point 1).
/// 3. **A miss must explicitly close the popup.** A planning probe measured
///    that simply not calling `response.context_menu(...)` on a miss frame
///    is NOT enough to close an already-open popup -- egui's own memory
///    keeps believing it is open (`show=false again (no explicit close) ->
///    open=true`, this plan's `<probe_results>` conclusion 3). So a
///    right-click that hits nothing calls `egui::Popup::close_id`
///    explicitly rather than merely skipping the render.
/// 4. **The decision lives in `context_menu::plan_open`, not here.** This
///    function never calls `open_file::build_command` or
///    `open_file::resolve_source_path` directly -- a verify gate fails the
///    task if it does (T-05-23-01).
fn handle_context_menu(
    ui: &mut egui::Ui,
    graph: &SeamGraph,
    response: &egui::Response,
    app: &mut SeamExplorerApp,
) {
    let meta = egui_graphs::MetadataFrame::new(None).load(ui);
    let graph_rect = response.rect;

    // Load the remembered target FIRST, then possibly overwrite it below --
    // so a non-click frame (re-rendering an already-open menu) keeps
    // whatever was last recorded.
    let mut target = crate::context_menu::load_target(ui);

    if response.secondary_clicked() {
        // Same undelayed pointer-position fallback chain
        // `handle_trace_gesture` uses above.
        let hit = response
            .interact_pointer_pos()
            .or_else(|| ui.input(|i| i.pointer.hover_pos()))
            .and_then(|p| hit_test_node(graph, &meta, graph_rect, p))
            .map(|(id, _)| id);
        target = hit;
        crate::context_menu::save_target(ui, target.clone());
        if target.is_none() {
            // A miss: don't render this frame at all, and explicitly clear
            // any stale open popup from a previous hit (point 3 above).
            egui::Popup::close_id(ui.ctx(), egui::Popup::default_response_id(response));
        }
    }

    let Some(id) = target else {
        return;
    };

    // T-05-23-04: re-look-up the target in the CURRENT model every frame --
    // never cache the node's fields across frames -- so a graph reload or a
    // focus change that removed this node between click and render can
    // never open the wrong (or a stale) file.
    //
    // Plan 09-03, 09-RESEARCH.md Open Question 2 (second site): `display_model`
    // IS that current model once a paused view exists -- it is the graph that
    // rendered the node under the cursor. Redirecting PRESERVES the invariant
    // above; leaving it live would break it, because a node visible on a paused
    // canvas but since removed live silently fails its `index` lookup and
    // dismisses the menu with no explanation. The historical node carries the
    // `source_file`/`source_line` the graph recorded for it, which is the file
    // the user is pointing at.
    let Some(model) = crate::timeline::display_model(app) else {
        crate::context_menu::save_target(ui, None);
        egui::Popup::close_id(ui.ctx(), egui::Popup::default_response_id(response));
        return;
    };
    let Some(&idx) = model.index.get(&id) else {
        crate::context_menu::save_target(ui, None);
        egui::Popup::close_id(ui.ctx(), egui::Popup::default_response_id(response));
        return;
    };
    let node = &model.graph[idx];
    let label = node.label.clone();
    let source_file = node.source_file.clone();
    let source_line = node.source_line;

    // Resolve every value the closure needs into OWNED locals BEFORE
    // opening the closure -- the closure borrows `ui`, so it cannot also
    // hold `&mut app`; the decision is computed here and the app mutation
    // (spawn / banner) happens AFTER the closure returns.
    let settings = crate::settings::current();
    let graph_dir = crate::load::graph_dir();
    let action = crate::context_menu::plan_open(
        source_file.as_deref(),
        source_line,
        graph_dir.as_deref(),
        &settings,
    );

    let mut activated_argv: Option<Vec<String>> = None;
    response.context_menu(|ui| {
        ui.label(egui::RichText::new(&label).strong());
        match &action {
            crate::context_menu::MenuAction::Spawn(argv) => {
                if ui.button(crate::context_menu::OPEN_FILE_LABEL).clicked() {
                    activated_argv = Some(argv.clone());
                }
            }
            crate::context_menu::MenuAction::DisabledNoSource => {
                ui.add_enabled(
                    false,
                    egui::Button::new(crate::context_menu::OPEN_FILE_LABEL),
                );
                ui.label(egui::RichText::new(crate::context_menu::NO_SOURCE_HINT).weak());
            }
            crate::context_menu::MenuAction::DisabledNoCommand => {
                ui.add_enabled(
                    false,
                    egui::Button::new(crate::context_menu::OPEN_FILE_LABEL),
                );
                ui.label(egui::RichText::new(crate::context_menu::NO_COMMAND_HINT).weak());
            }
        }
    });

    // `PopupCloseBehavior::CloseOnClick` is the default -- activating the
    // item above already closed the menu (probe-confirmed: `AFTER ITEM
    // CLICK clicks=1 open=false`). No manual close here; adding one would
    // double-close and is untestable.
    if let Some(argv) = activated_argv {
        // Under `#[cfg(test)]` the spawn is deliberately skipped -- see
        // `argv_probe`'s doc comment for the honest scope of what the live
        // tests do and do not cover. The argv-to-process link itself is
        // covered by `open_file::spawn`'s own real-process tests
        // (05-21) plus this plan's human-check.
        #[cfg(test)]
        {
            argv_probe::record(ui.ctx(), argv);
        }
        #[cfg(not(test))]
        {
            if let Err(err) = crate::open_file::spawn(&argv) {
                // T-05-23-05: the user just chose a menu item and is
                // waiting for a window -- silence here is genuinely
                // confusing, unlike the other silent give-up paths in this
                // feature.
                let program = argv.first().cloned().unwrap_or_default();
                app.banner = Some(crate::app::Banner {
                    kind: crate::app::BannerKind::Error,
                    heading: "Couldn't open file".to_string(),
                    body: format!("Failed to launch `{program}`: {err}"),
                });
            }
        }
    }
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

    // One pass over `graph.nodes_iter()` builds all three maps. This is the
    // only place in the layout path with concrete `PayloadNode` access, so
    // it is the only place that can read a node's stable `id` at all --
    // `SeamLayout::next`'s trait bound is `N: Clone` and forbids it (see
    // `layout.rs`'s module doc). The target and group maps are keyed by
    // that id directly; the translation table maps this frame's graph index
    // to it.
    let mut targets: std::collections::HashMap<String, f32> = std::collections::HashMap::new();
    let mut groups: std::collections::HashMap<String, u8> = std::collections::HashMap::new();
    let mut id_by_index: std::collections::HashMap<usize, String> =
        std::collections::HashMap::new();
    for (idx, node) in graph.nodes_iter() {
        let id = node.payload().id.clone();
        let target_x = crate::layout::seam_target_x(
            &node.payload().community,
            focus_pair,
            center.x,
            canvas_width,
        );
        targets.insert(id.clone(), target_x);
        groups.insert(
            id.clone(),
            crate::layout::seam_group(&node.payload().community, focus_pair),
        );
        id_by_index.insert(idx.index(), id);
    }

    let mut state = crate::layout::SeamLayoutState::load(ui, None);
    state.set_targets(
        targets,
        groups,
        id_by_index,
        center,
        canvas_width,
        canvas_rect.height(),
    );

    // Release layout state for nodes a live `RemoveNode` actually deleted,
    // so a long live session's persisted position map stays bounded
    // (T-08-02-03).
    //
    // Driven by the MODEL's id index, never by `graph.nodes_iter()` above.
    // The distinction is the entire point: seam-focus hiding removes
    // out-of-pair nodes from the rendered graph outright (DP-10-02), so
    // pruning against the rendered set would discard a merely-hidden node's
    // settled position and re-seed it the moment focus cleared -- which
    // reads as the canvas jumping. `show()` already returns early when
    // `app.model` is `None` (this function is unreachable in that state),
    // but the `if let` below still skips the prune rather than pruning
    // against an empty set, so no future call site can wipe every position
    // by reaching here with nothing loaded.
    if let Some(model) = app.model.as_ref() {
        let known: std::collections::HashSet<String> = model.index.keys().cloned().collect();
        state.retain_positions(&known);
    }

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
        // known-visible and never needs a dimmed fill. One exception
        // (260918-ttc): a force-included hop's community matches neither
        // side, so it deliberately takes this same neutral fill -- the
        // right affordance, since it is visibly a member of neither side.
        let base = match &app.focus {
            Some(f) if community == f.a => hex(SIDE_A_HEX),
            Some(f) if community == f.b => hex(SIDE_B_HEX),
            _ => hex(DIMMED_FILL_HEX),
        };
        let is_bridge = app
            .detail
            .as_ref()
            .is_some_and(|d| d.bridges_a.contains(&id) || d.bridges_b.contains(&id));
        // quick-260927-iy9: the two states that used to OR into one shared
        // `selected` ring flag are now driven independently, re-derived
        // every frame exactly as before -- `build_graph` reconstructs the
        // graph every frame and this pass re-runs every frame, so neither
        // can ever go stale. (1) quick-260915-sf7's `app.selected_node`,
        // set by a detail-panel bridge-row click or a find-node result
        // click (both via the shared `jump_to_node`) -- drives the INNER
        // jump ring. (2) quick-260926-gb2's `app.trace_gesture`'s armed
        // node (`TraceGesture::armed_node()`) -- drives the OUTER
        // trace-armed ring. DP-GB2-06: both may ring at once -- a jump
        // highlight and an armed trace source are never mutually
        // exclusive, and the concentric dual-ring stacking is what
        // disambiguates which is which (`<design_decision>` 3).
        let is_jump_selected = app.selected_node.as_deref() == Some(id.as_str());
        let is_trace_armed = app.trace_gesture.armed_node() == Some(id.as_str());

        if let Some(n) = graph.node_mut(idx) {
            n.set_color(base);
            n.display_mut().is_bridge = is_bridge;
            n.display_mut().is_jump_selected = is_jump_selected;
            n.display_mut().is_trace_armed = is_trace_armed;
            // The widget's own built-in `selected` flag KEEPS the OR of
            // both states (`<design_decision>` 5): `egui_graphs` defers
            // `selected() || dragged()` nodes so they paint above every
            // other node (`drawer.rs:112`), and `shapes()`'s full-label
            // reveal reads this same flag. Neither ring reads `self.selected`
            // any more (see `SeamNodeShape::shapes`), but narrowing this
            // flag to only one of the two states would silently drop the
            // OTHER state out of both the on-top paint order and the
            // full-label reveal -- which matters MORE in the both-rings
            // case, where the trace-armed ring extends `TRACE_RING_OFFSET`
            // further out (quick-260927-rmx: armed-alone no longer extends
            // outward at all -- it hugs the node at the base radius, so
            // this concern is specific to the both-flags case now). The
            // widget cannot clobber either source: `handle_click`
            // (egui_graphs 0.31.0) returns early when no click/selection
            // interaction is enabled, and this app enables only dragging
            // (`with_dragging_enabled`), so `deselect_all_nodes` is
            // unreachable.
            n.set_selected(is_jump_selected || is_trace_armed);
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

/// A search-to-jump target (NAV-01). Correction (quick-260915-sf7 discovery
/// finding 3): each variant carries a canvas-space offset measured FROM THE
/// VIEWPORT CENTRE, not a raw canvas position -- the doc comment that used
/// to live here understated this. Derivation: `compute_jump_view` below
/// sets `pan = -t`; for a point at canvas position `p` to land at the
/// viewport centre `C = viewport / 2` under this file's `view_to_frame`
/// transform contract (`local_screen = (canvas + view.pan - C) * view.zoom
/// + C`), `view.pan` must equal `C - p`, i.e. `t == p - C`. So a caller
/// must hand this enum `p - C`, never the raw `p` itself -- handing the raw
/// `location()` would miss by `C`, roughly 500-800 screen px at
/// `JUMP_ZOOM` on a normal window.
///
/// `seam_list.rs`'s `JumpTarget::Seam(Pos2::ZERO)` is not a placeholder --
/// it is literally true under this contract, because `inject_layout_targets`
/// centres the pull-apart layout on `canvas_rect.center()`, so a focused
/// seam's centre offset from the viewport centre genuinely is zero. Both
/// variants are correct as-is under this one formula; `compute_jump_view`
/// itself needs no change -- quick-260915-sf7's whole contribution is
/// `node_jump_target` below, the caller that produces a correct
/// centre-relative offset for `Node` for the first time.
///
/// Kept as canvas-space `Pos2` (not a `seam_core` id) since panels outside
/// `graph_view` (e.g. `seam_list`, `detail`) have no access to this
/// module's live canvas geometry.
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

/// Pure: the `app.selected_node` value after a click on `id`, given the
/// CURRENT selection `current` (quick-260927-iy9, `<design_decision>` 1).
/// Clicking the already-selected node clears it (`None`); clicking any
/// other node -- or clicking with nothing currently selected -- selects
/// `id`. Implemented once, uniformly, for BOTH `jump_to_node` call sites
/// (a find-node result and a detail-panel bridge row): `jump_to_node` was
/// extracted VERBATIM by quick-260926-nop so the two callers would share
/// one behaviour, and putting the toggle at only one call site would
/// re-split what that task merged. The bridge-row case needs the toggle
/// MORE, not less -- it has no search box in play at all, so without this,
/// a bridge-row selection would be a one-way door.
fn toggle_selection(current: Option<&str>, id: &str) -> Option<String> {
    if current == Some(id) {
        None
    } else {
        Some(id.to_string())
    }
}

/// The crate's single node-click jump action (D-03, quick-260926-nop):
/// resolves the click through `toggle_selection` FIRST (quick-260927-iy9)
/// -- so a repeat click on the already-selected node always clears it, even
/// if `id` has since left the rendered set -- and only resolves a jump
/// target when the click is a genuine new selection. On a resolved target,
/// calls `jump_to` and returns `true`; on a toggle-off, clears
/// `app.selected_node` and returns `true` WITHOUT touching `app.view` (a
/// toggle-off must not re-frame the canvas); on an unresolved id for a
/// would-be NEW selection, changes nothing and returns `false`.
///
/// Return-value contract: `true` when the call changed selection state
/// (jumped-and-selected, or toggled off), `false` when the id resolved to
/// nothing and nothing changed -- keeping
/// `jump_to_node_is_a_silent_no_op_for_an_unrendered_id` true as written.
///
/// Extracted VERBATIM from `panels::detail::bridge_list`'s post-click block
/// (quick-260915-sf7) so both the bridge row and the find-node result call
/// the SAME function -- see `toggle_selection`'s own doc comment for why the
/// toggle lives here, uniformly, rather than at one call site.
/// `node_jump_target` returning `None` for an id absent from the rendered
/// graph is a second, independent scope guard on a NEW jump (discovery
/// finding 4).
pub fn jump_to_node(ui: &egui::Ui, app: &mut SeamExplorerApp, id: &str) -> bool {
    match toggle_selection(app.selected_node.as_deref(), id) {
        None => {
            // Toggle-off: clear the selection without resolving a jump
            // target or touching `app.view`.
            app.selected_node = None;
            true
        }
        Some(_) => {
            if let Some(target) = node_jump_target(ui, id) {
                jump_to(app, JumpTarget::Node(target));
                app.selected_node = Some(id.to_string());
                true
            } else {
                false
            }
        }
    }
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
/// live in egui temp data, the same storage `reset_sentinel_fired` already
/// uses for per-frame state with nowhere to live on the frozen
/// `SeamExplorerApp`.
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
/// `Option` (never written vs. explicitly cleared both read as `None`) --
/// the same load/flatten discipline `context_menu::load_target` uses for
/// its own remembered right-click id.
fn load_refit_follow(ui: &egui::Ui) -> Option<RefitFollowState> {
    ui.data(|d| d.get_temp(refit_follow_id())).flatten()
}

/// Records or clears the refit follow. Writing `None` is the clear -- a
/// single setter for both record and clear, avoiding egui's `remove_temp`
/// API (which additionally requires `T: Default`), the same reasoning
/// `context_menu::save_target`'s doc comment gives.
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
/// clicked while one is already focused, and focus being cleared -- those
/// are the only transitions that change `render_focus`'s value after
/// quick task `260918-ttc` retired both historical suspension cases
/// (Trace mode toggling and a trace result arriving or clearing no longer
/// move this value at all).
fn render_focus_changed(ui: &mut egui::Ui, render_focus: Option<&crate::app::FocusState>) -> bool {
    let id = egui::Id::new("seam_explorer_refit_follow_render_focus");
    let current: Option<crate::app::FocusState> = render_focus.cloned();
    ui.data_mut(|d| {
        let prev: Option<Option<crate::app::FocusState>> = d.get_temp(id);
        d.insert_temp(id, current.clone());
        prev.is_some_and(|p| p != current)
    })
}

/// quick-260927-tlc: snapshots `SeamExplorerApp::load_generation` into its
/// own temp-data slot, reporting whether it differs from the previous
/// frame's snapshot -- the THIRD of `refit_follow_step`'s arming triggers,
/// alongside `render_focus_changed` and `reset_sentinel_fired` above. It
/// covers all three interactive load routes (the top-bar dialog, Open
/// Project, build-then-load) plus the CLI preload, because all four funnel
/// through `SeamExplorerApp::apply_load_outcome`, the sole place
/// `load_generation` is bumped.
///
/// DELIBERATE DIVERGENCE from the other two triggers above: this function
/// reads an absent previous snapshot as generation 0 (`current !=
/// prev.unwrap_or(0)`), not as "arm nothing" (`prev.is_some_and(|p| p !=
/// current)`, which both siblings use). 0 is not a missing observation --
/// it is the real, only, default-derived value of a never-loaded app, so
/// substituting one for the other loses no information. That is NOT true
/// of `render_focus` (whose `None` means "no seam focused", a legitimate
/// value distinct from "not yet observed"), which is exactly why that
/// trigger needs the stricter clause and this one does not. Without this
/// divergence, `startup::preload_graph` -- which completes before
/// `run_native` creates the first frame -- would leave the CLI preload
/// route unfixed, since there is no previous frame to have written a
/// snapshot on.
fn load_generation_changed(ui: &mut egui::Ui, app: &SeamExplorerApp) -> bool {
    let id = egui::Id::new("seam_explorer_refit_follow_load_generation");
    let current = app.load_generation;
    ui.data_mut(|d| {
        let prev: Option<u64> = d.get_temp(id);
        d.insert_temp(id, current);
        current != prev.unwrap_or(0)
    })
}

/// The per-frame refit-follow step (Plan 15, NAV-02/NAV-04 combined; third
/// trigger added by quick-260927-tlc): closes the user's "I need to press
/// reset view to get it centered. Can these be combined." gap by
/// re-framing the canvas whenever the rendered node set changes (armed by
/// `render_focus_changed`), an explicit reset is requested (armed by
/// `reset_sentinel_fired`), or a graph has just finished loading (armed by
/// `load_generation_changed`), and by continuing to re-fit every frame
/// while the pull-apart layout is still easing toward its targets --
/// tracking the animation to rest instead of fitting once against
/// positions that haven't finished moving (see `05-15-PLAN.md`'s
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
    // quick-260927-tlc / finding 6: all three trigger calls are bound to
    // locals BEFORE the `if`, never inlined into the `||` chain -- each
    // call's snapshot write is a side effect, and `||` short-circuits, so
    // inlining any of them would skip its snapshot write whenever an
    // earlier term is already true, causing a spurious arm on a later
    // frame.
    let armed_by_focus_change = render_focus_changed(ui, render_focus);
    let armed_by_reset = reset_sentinel_fired(ui, app);
    let armed_by_load = load_generation_changed(ui, app);

    let mut follow = load_refit_follow(ui);
    if armed_by_focus_change || armed_by_reset || armed_by_load {
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
        let graph = build_graph(&model, Some(&focus), &std::collections::HashSet::new());

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
        let graph = build_graph(&model, Some(&focus), &std::collections::HashSet::new());

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
        let graph = build_graph(&model, None, &std::collections::HashSet::new());
        assert_eq!(graph.node_count(), model.graph.node_count());
        assert_eq!(graph.edge_count(), model.graph.edge_count());
    }

    // ============================================================
    // 05-10 Task 2 (DP-10-03): the single hiding-suspension rule. Case (b)
    // (trace mode) was retired by quick task 260918-sgx, and case (c) (a
    // resolved trace result) was retired by quick task 260918-ttc -- see
    // hiding_active's doc comment.
    // ============================================================

    /// Lifted out of `a_node_that_is_both_jumped_to_and_armed_paints_two_concentric_rings`
    /// (quick-260927-rmx) so every ring-geometry test in this module builds
    /// its display object the same way, one call site instead of five copies.
    /// Renamed from the original nested `shape_for` for clarity now that it
    /// is shared.
    fn node_display_with_flags(
        model: &seam_core::Model,
        focus: &crate::app::FocusState,
        id: &str,
        is_jump_selected: bool,
        is_trace_armed: bool,
    ) -> SeamNodeShape {
        let mut graph = build_graph(model, Some(focus), &std::collections::HashSet::new());
        let idx = graph
            .nodes_iter()
            .find(|(_, n)| n.payload().id == id)
            .map(|(idx, _)| idx)
            .unwrap_or_else(|| panic!("node {id} not found"));
        {
            let n = graph.node_mut(idx).unwrap();
            n.display_mut().is_jump_selected = is_jump_selected;
            n.display_mut().is_trace_armed = is_trace_armed;
        }
        graph.node(idx).unwrap().display().clone()
    }

    /// Lifted alongside `node_display_with_flags` (quick-260927-rmx) --
    /// filters a `shapes()` return value down to just the `CircleShape`s,
    /// since every ring-geometry test needs this and nothing else.
    fn circles_of(shapes: &[egui::Shape]) -> Vec<&egui::epaint::CircleShape> {
        shapes
            .iter()
            .filter_map(|s| match s {
                egui::Shape::Circle(c) => Some(c),
                _ => None,
            })
            .collect()
    }

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

    /// DP-10-03 case (b) was RETIRED by quick task 260918-sgx: focus is the
    /// scaling strategy for large graphs, and a mode toggle must not discard
    /// it. Trace mode no longer suspends hiding -- a focused seam stays
    /// focused while the user picks a drag source and target. The
    /// deliberate trade-off: a trace can no longer reach a community the
    /// user has not focused on.
    #[test]
    fn hiding_stays_active_while_trace_mode_is_on() {
        let app = crate::app::SeamExplorerApp {
            focus: Some(focus_state()),
            trace_mode: true,
            ..Default::default()
        };
        assert!(hiding_active(&app));
    }

    /// DP-10-03 case (c) was RETIRED by quick task `260918-ttc`: a resolved
    /// trace result no longer suspends hiding. The concern case (c) existed
    /// for -- a hop the graph cannot resolve, silently truncating the
    /// polyline -- is now handled by `forced_visible_ids` feeding
    /// `build_graph`'s `forced` parameter instead: the specific node(s) a
    /// resolved path needs are force-included, never their whole community.
    #[test]
    fn hiding_stays_active_while_a_trace_result_is_present() {
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
        assert!(hiding_active(&app));
    }

    // ============================================================
    // quick-260918-ttc: `forced_visible_ids` and `build_graph`'s new
    // `forced` parameter -- surgical per-node inclusion for a resolved
    // trace path, replacing the retired whole-community suspension above.
    // ============================================================

    fn focus_state_ac() -> crate::app::FocusState {
        crate::app::FocusState {
            a: "A".to_string(),
            b: "C".to_string(),
        }
    }

    /// Small helper: wraps a resolved `TraceResult` into a minimal
    /// `SeamExplorerApp` for `forced_visible_ids`, which reads only
    /// `app.trace`.
    fn result_app(result: crate::trace::TraceResult) -> crate::app::SeamExplorerApp {
        crate::app::SeamExplorerApp {
            trace: Some(result),
            ..Default::default()
        }
    }

    /// No trace at all, and a trace present but not yet resolved to a
    /// path, both yield an empty forced set (`<design_decision>` 4: there
    /// is no polyline to draw in either case).
    #[test]
    fn forced_visible_ids_is_empty_without_a_resolved_path() {
        let app_no_trace = crate::app::SeamExplorerApp::default();
        assert!(forced_visible_ids(&app_no_trace).is_empty());

        let app_unresolved = crate::app::SeamExplorerApp {
            trace: Some(crate::trace::TraceResult {
                from: "a1".to_string(),
                to: "zzz".to_string(),
                path: None,
            }),
            ..Default::default()
        };
        assert!(forced_visible_ids(&app_unresolved).is_empty());
    }

    /// A resolved trace's forced set is exactly its path's hop ids.
    #[test]
    fn forced_visible_ids_are_the_resolved_paths_hops() {
        let app = crate::app::SeamExplorerApp {
            trace: Some(crate::trace::TraceResult {
                from: "a2".to_string(),
                to: "c1".to_string(),
                path: Some(seam_core::TracePath {
                    hops: vec!["a2".to_string(), "b1".to_string(), "c1".to_string()],
                    seams_crossed: vec![],
                }),
            }),
            ..Default::default()
        };
        let forced = forced_visible_ids(&app);
        let expected: std::collections::HashSet<String> =
            ["a2", "b1", "c1"].iter().map(|s| s.to_string()).collect();
        assert_eq!(forced, expected);
    }

    /// The reported "seams crossed: 0" shape: a resolved trace whose path
    /// never leaves the focused pair (`a1 -> a2`, both in `A`) must add
    /// NOTHING to the rendered graph -- the node set is byte-for-byte the
    /// same set the forced-empty build produces.
    #[test]
    fn a_trace_that_crosses_no_seam_adds_nothing_to_the_rendered_graph() {
        let ingest = seam_core::from_json(CLEAN_FIXTURE).expect("clean fixture must ingest");
        let model = ingest.model;
        let focus = focus_state_ac();

        let app = result_app(crate::trace::run(&model, "a1", "a2"));
        let path = app
            .trace
            .as_ref()
            .and_then(|t| t.path.as_ref())
            .expect("a1 -> a2 must resolve");
        assert!(
            path.seams_crossed.is_empty(),
            "guard: this trace must cross zero seams, got {:?}",
            path.seams_crossed
        );

        let forced = forced_visible_ids(&app);
        let graph_forced = build_graph(&model, Some(&focus), &forced);
        let graph_empty = build_graph(&model, Some(&focus), &std::collections::HashSet::new());

        let ids_forced: std::collections::HashSet<String> = graph_forced
            .nodes_iter()
            .map(|(_, n)| n.payload().id.clone())
            .collect();
        let ids_empty: std::collections::HashSet<String> = graph_empty
            .nodes_iter()
            .map(|(_, n)| n.payload().id.clone())
            .collect();
        assert_eq!(
            ids_forced, ids_empty,
            "a zero-seam trace must not change the rendered node set"
        );
        let expected: std::collections::HashSet<String> = ["a1", "a2", "c1", "c2"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(ids_forced, expected);
    }

    /// The real-world shape: `a2 -> c1` routes through `b1`, an
    /// intermediate hop in the unfocused community `B`. Only `b1` -- never
    /// its community-mate `b2` -- may reach the rendered graph, and every
    /// hop of the path must resolve so the polyline never drops one.
    #[test]
    fn a_hop_outside_the_focused_pair_is_added_alone() {
        let ingest = seam_core::from_json(CLEAN_FIXTURE).expect("clean fixture must ingest");
        let model = ingest.model;
        let focus = focus_state_ac();

        let app = result_app(crate::trace::run(&model, "a2", "c1"));
        let path = app
            .trace
            .as_ref()
            .and_then(|t| t.path.as_ref())
            .expect("a2 -> c1 must resolve");
        assert_eq!(
            path.hops,
            vec!["a2".to_string(), "b1".to_string(), "c1".to_string()],
            "guard: the fixture's shortest path must be a2 -> b1 -> c1"
        );

        let forced = forced_visible_ids(&app);
        let graph = build_graph(&model, Some(&focus), &forced);

        let ids: std::collections::HashSet<String> = graph
            .nodes_iter()
            .map(|(_, n)| n.payload().id.clone())
            .collect();
        let expected: std::collections::HashSet<String> = ["a1", "a2", "c1", "c2", "b1"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(ids, expected);
        assert!(
            !ids.contains("b2"),
            "b1's community-mate b2 must not leak in"
        );

        for hop in &path.hops {
            assert!(
                ids.contains(hop),
                "every hop of a resolved path must be present in the rendered graph: missing {hop}"
            );
        }

        let edge_ids: std::collections::HashSet<(String, String)> = graph
            .edges_iter()
            .filter_map(|(edge_idx, _)| {
                let (s, t) = graph.edge_endpoints(edge_idx)?;
                Some((
                    graph.node(s)?.payload().id.clone(),
                    graph.node(t)?.payload().id.clone(),
                ))
            })
            .collect();
        assert!(
            edge_ids.contains(&("a2".to_string(), "b1".to_string())),
            "the path edge a2 -> b1 must be rendered"
        );
        assert!(
            edge_ids.contains(&("b1".to_string(), "c1".to_string())),
            "the path edge b1 -> c1 must be rendered"
        );
        assert!(
            edge_ids.iter().all(|(s, t)| s != "b2" && t != "b2"),
            "no rendered edge may touch b2"
        );
    }

    // ============================================================
    // quick-260926-nop Task 1: `node_rendered` -- the one rendered-set
    // membership predicate, extracted from `build_graph`'s own skip
    // condition (DP-NOP-05) so `seam_list::find_nodes` can ask the exact
    // same question `build_graph` is handed.
    // ============================================================

    /// Mirrors `node_visible_keeps_both_focused_sides` -- when nothing is
    /// forced, `node_rendered` must agree with `node_visible` exactly: a
    /// node in each focused community is rendered, a node in a third
    /// community is not.
    #[test]
    fn node_rendered_agrees_with_node_visible_when_nothing_is_forced() {
        let ingest = seam_core::from_json(CLEAN_FIXTURE).expect("clean fixture must ingest");
        let model = ingest.model;
        let focus = focus_state();
        let forced = std::collections::HashSet::new();

        let a1 = &model.graph[*model.index.get("a1").expect("a1 must exist")];
        let b1 = &model.graph[*model.index.get("b1").expect("b1 must exist")];
        let c1 = &model.graph[*model.index.get("c1").expect("c1 must exist")];

        assert!(node_rendered(a1, Some(&focus), &forced));
        assert!(node_rendered(b1, Some(&focus), &forced));
        assert!(!node_rendered(c1, Some(&focus), &forced));
    }

    /// Mirrors `build_graph`'s own 260918-ttc behaviour: a node outside the
    /// focused pair IS rendered when its id is in the forced set.
    #[test]
    fn node_rendered_admits_a_forced_node_outside_the_focused_pair() {
        let ingest = seam_core::from_json(CLEAN_FIXTURE).expect("clean fixture must ingest");
        let model = ingest.model;
        let focus = focus_state();
        let c1 = &model.graph[*model.index.get("c1").expect("c1 must exist")];

        let empty = std::collections::HashSet::new();
        assert!(!node_rendered(c1, Some(&focus), &empty));

        let forced: std::collections::HashSet<String> = ["c1".to_string()].into_iter().collect();
        assert!(node_rendered(c1, Some(&focus), &forced));
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
        let graph = build_graph(&model, None, &std::collections::HashSet::new());
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
    // quick-260915-sf7 Task 2 (RED): `apply_focus_styling` must derive each
    // node's `selected()` flag from `app.selected_node` every frame -- the
    // pure styling pass, driven directly (no harness needed, per this
    // task's own `<behavior>`: `apply_focus_styling` is a free function over
    // `(&mut SeamGraph, &SeamExplorerApp)`).
    // ============================================================

    #[test]
    fn apply_focus_styling_marks_only_the_selected_node() {
        let outcome =
            crate::load::read_and_ingest(CLEAN_FIXTURE).expect("fixture must ingest cleanly");
        let model = outcome.model;
        let scc = model
            .scc
            .as_ref()
            .expect("read_and_ingest must finalize scc");
        let focus = focus_state();
        let detail = seam_core::seam_detail(&model, scc, &focus.a, &focus.b);
        assert!(
            detail.bridges_a.iter().any(|id| id == "a1"),
            "a1 must be a real bridge node for this test to be meaningful"
        );

        let mut graph = build_graph(&model, Some(&focus), &std::collections::HashSet::new());
        let app = crate::app::SeamExplorerApp {
            focus: Some(focus.clone()),
            detail: Some(detail.clone()),
            selected_node: Some("a1".to_string()),
            ..Default::default()
        };
        apply_focus_styling(&mut graph, &app);

        let selected_ids: Vec<String> = graph
            .nodes_iter()
            .filter(|(_, n)| n.selected())
            .map(|(_, n)| n.payload().id.clone())
            .collect();
        assert_eq!(
            selected_ids,
            vec!["a1".to_string()],
            "exactly node a1 must report selected() true, got {selected_ids:?}"
        );

        // With `selected_node` absent, nothing is selected -- the flag must
        // not latch on by accident.
        let mut graph_unselected =
            build_graph(&model, Some(&focus), &std::collections::HashSet::new());
        let app_unselected = crate::app::SeamExplorerApp {
            focus: Some(focus.clone()),
            detail: Some(detail.clone()),
            selected_node: None,
            ..Default::default()
        };
        apply_focus_styling(&mut graph_unselected, &app_unselected);
        assert!(
            graph_unselected.nodes_iter().all(|(_, n)| !n.selected()),
            "no node may report selected() true when app.selected_node is None"
        );

        // quick-260926-gb2 (D-01): an ARMED trace rings its source node the
        // same way, via `TraceGesture::armed_node()`.
        let mut graph_armed = build_graph(&model, Some(&focus), &std::collections::HashSet::new());
        let app_armed = crate::app::SeamExplorerApp {
            focus: Some(focus.clone()),
            detail: Some(detail.clone()),
            trace_gesture: crate::trace::TraceGesture::Armed {
                from: "a1".to_string(),
            },
            ..Default::default()
        };
        apply_focus_styling(&mut graph_armed, &app_armed);
        let armed_selected_ids: Vec<String> = graph_armed
            .nodes_iter()
            .filter(|(_, n)| n.selected())
            .map(|(_, n)| n.payload().id.clone())
            .collect();
        assert_eq!(
            armed_selected_ids,
            vec!["a1".to_string()],
            "exactly the armed node must report selected() true, got {armed_selected_ids:?}"
        );

        // DP-GB2-06 regression lock: sf7's own click-to-jump ring, with NO
        // trace armed, still rings exactly its own node -- unchanged.
        let mut graph_sf7_only =
            build_graph(&model, Some(&focus), &std::collections::HashSet::new());
        let app_sf7_only = crate::app::SeamExplorerApp {
            focus: Some(focus.clone()),
            detail: Some(detail.clone()),
            selected_node: Some("b1".to_string()),
            ..Default::default()
        };
        apply_focus_styling(&mut graph_sf7_only, &app_sf7_only);
        let sf7_only_ids: Vec<String> = graph_sf7_only
            .nodes_iter()
            .filter(|(_, n)| n.selected())
            .map(|(_, n)| n.payload().id.clone())
            .collect();
        assert_eq!(
            sf7_only_ids,
            vec!["b1".to_string()],
            "sf7's own selected_node ring must be unaffected by this task, got {sf7_only_ids:?}"
        );

        // DP-GB2-06: both an armed trace source AND an sf7 jump-highlight
        // may ring at once -- deliberately, not suppressed either way.
        let mut graph_both = build_graph(&model, Some(&focus), &std::collections::HashSet::new());
        let app_both = crate::app::SeamExplorerApp {
            focus: Some(focus),
            detail: Some(detail),
            selected_node: Some("b1".to_string()),
            trace_gesture: crate::trace::TraceGesture::Armed {
                from: "a1".to_string(),
            },
            ..Default::default()
        };
        apply_focus_styling(&mut graph_both, &app_both);
        let mut both_ids: Vec<String> = graph_both
            .nodes_iter()
            .filter(|(_, n)| n.selected())
            .map(|(_, n)| n.payload().id.clone())
            .collect();
        both_ids.sort();
        assert_eq!(
            both_ids,
            vec!["a1".to_string(), "b1".to_string()],
            "both the armed source and the sf7 jump-highlight must ring at once \
             (DP-GB2-06), got {both_ids:?}"
        );
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
    // Task 3 (quick-260926-gb2), RED-first: `apply_drag_pan`, the pan
    // sibling of `apply_zoom_factor` above -- same finite-guard discipline,
    // same `PAN_STEP / view.zoom` convention `keyboard::apply_key` already
    // established, so a 60px drag and a 40px-per-press arrow key agree
    // about what a screen pixel is (DP-GB2-03).
    // ============================================================

    #[test]
    fn apply_drag_pan_moves_pan_by_the_delta_divided_by_zoom() {
        let view = crate::app::ViewState {
            zoom: 1.0,
            pan: egui::Vec2::ZERO,
        };
        let moved = apply_drag_pan(view, egui::vec2(60.0, 40.0));
        assert_eq!(
            moved.pan,
            egui::vec2(60.0, 40.0),
            "at zoom 1.0 a drag delta of (60, 40) must move pan by exactly that much"
        );
        assert_eq!(moved.zoom, 1.0, "apply_drag_pan must never change zoom");
    }

    #[test]
    fn apply_drag_pan_divides_the_delta_by_zoom() {
        let view = crate::app::ViewState {
            zoom: 2.0,
            pan: egui::Vec2::ZERO,
        };
        let moved = apply_drag_pan(view, egui::vec2(60.0, 40.0));
        assert_eq!(
            moved.pan,
            egui::vec2(30.0, 20.0),
            "at zoom 2.0 a drag must move pan by half the raw delta -- the same \
             PAN_STEP / view.zoom convention keyboard::apply_key established, got {:?}",
            moved.pan
        );
    }

    #[test]
    fn apply_drag_pan_guards_non_finite_delta_and_zoom() {
        let view = crate::app::ViewState {
            zoom: 1.4,
            pan: egui::vec2(8.0, 3.0),
        };
        for bad_delta in [egui::vec2(f32::NAN, 0.0), egui::vec2(0.0, f32::INFINITY)] {
            let result = apply_drag_pan(view, bad_delta);
            assert_eq!(
                result.pan, view.pan,
                "a non-finite drag delta ({bad_delta:?}) must leave pan completely unchanged, \
                 got {:?}",
                result.pan
            );
            assert_eq!(result.zoom, view.zoom);
        }

        let non_finite_zoom_view = crate::app::ViewState {
            zoom: f32::NAN,
            pan: egui::vec2(8.0, 3.0),
        };
        let result = apply_drag_pan(non_finite_zoom_view, egui::vec2(10.0, 10.0));
        assert_eq!(result.pan, non_finite_zoom_view.pan);
        assert!(
            result.zoom.is_nan(),
            "a non-finite view.zoom must leave the view completely unchanged (still NaN), not \
             be replaced by a finite value"
        );

        let non_positive_zoom_view = crate::app::ViewState {
            zoom: 0.0,
            pan: egui::vec2(8.0, 3.0),
        };
        let result2 = apply_drag_pan(non_positive_zoom_view, egui::vec2(10.0, 10.0));
        assert_eq!(result2.pan, non_positive_zoom_view.pan);
        assert_eq!(result2.zoom, 0.0);
    }

    #[test]
    fn apply_drag_pan_zero_delta_is_a_no_op() {
        let view = crate::app::ViewState {
            zoom: 1.5,
            pan: egui::vec2(3.0, -2.0),
        };
        let result = apply_drag_pan(view, egui::Vec2::ZERO);
        assert_eq!(result.pan, view.pan);
        assert_eq!(result.zoom, view.zoom);
    }

    // ============================================================
    // Plan 13 gap closure (G-05-5), rewritten by quick-260926-gb2: live-
    // wiring regression test for the click-to-arm/click-to-complete
    // gesture -- drives the REAL click sequence through a live-rendered
    // GraphView, not the pure trace::update_gesture state machine in
    // isolation (05-05 shipped exactly that and missed a live-wiring
    // defect once already, for the drag gesture this replaces).
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

    /// Drives a plain primary click (press then release with NO intervening
    /// movement) at `pos` -- the same shape `right_click_no_movement` below
    /// uses for the secondary button, and what makes egui report a click
    /// rather than a drag.
    fn primary_click_no_movement(
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
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::default(),
        });
        harness.step();
        harness.input_mut().events.push(egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::default(),
        });
        harness.step();
    }

    #[test]
    fn clicking_two_nodes_produces_a_trace() {
        let outcome =
            crate::load::read_and_ingest(CLEAN_FIXTURE).expect("fixture must ingest cleanly");
        let app = crate::app::SeamExplorerApp {
            model: Some(outcome.model),
            seams: outcome.seams,
            trace_mode: true,
            ..Default::default()
        };

        let positions_mirror: std::rc::Rc<std::cell::RefCell<Vec<(String, egui::Pos2)>>> =
            std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let positions_inner = positions_mirror.clone();

        let mut harness = egui_kittest::Harness::new_ui_state(
            move |ui, app: &mut crate::app::SeamExplorerApp| {
                show(ui, app);
                *positions_inner.borrow_mut() = test_probe::load_node_screen_positions(ui);
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
            "a1 and c1 must be further apart than egui's 6pt click threshold, got \
             a1={a1_pos:?} c1={c1_pos:?}"
        );

        // Click a1 -- this must ARM the trace (D-01/D-03). This is also the
        // direct positive proof that a plain click with no movement (once a
        // no-op under the old drag gesture -- see the deleted
        // `primary_button_click_without_drag_does_not_trace`) is now the
        // trigger for the whole feature.
        primary_click_no_movement(&mut harness, a1_pos);
        assert_eq!(
            harness.state().trace_gesture,
            crate::trace::TraceGesture::Armed {
                from: "a1".to_string()
            },
            "clicking a1 with Trace mode on must arm the trace"
        );

        // Several idle frames with no input in between -- arming surviving
        // across frames is the entire behavioural difference from the old
        // drag gesture, and the whole reason for this redesign (D-01/D-03).
        for _ in 0..5 {
            harness.step();
        }
        assert_eq!(
            harness.state().trace_gesture,
            crate::trace::TraceGesture::Armed {
                from: "a1".to_string()
            },
            "the armed state must survive several idle frames with no input"
        );

        // Click c1 -- this must COMPLETE the trace immediately (D-03).
        primary_click_no_movement(&mut harness, c1_pos);

        let trace = harness.state().trace.clone().unwrap_or_else(|| {
            panic!("app.trace must be Some after clicking c1 while armed from a1")
        });
        assert_eq!(trace.from, "a1");
        assert_eq!(trace.to, "c1");
        assert!(
            trace.path.is_some(),
            "a1 -> c1 has a direct edge in the fixture; the trace must resolve a path"
        );
    }

    /// Builds a settled `egui_kittest::Harness` over `CLEAN_FIXTURE` with
    /// Trace mode as requested, mirroring `clicking_two_nodes_produces_a_trace`'s
    /// settle-to-stability discipline exactly (same
    /// `positions_stable`/`POSITION_SETTLE_EPSILON`/`MAX_SETTLE_STEPS`
    /// shape `settle_menu_harness` below also uses for its own fixture).
    /// Shared by the two smaller click tests below so neither duplicates the
    /// ~30-line settle loop a third time.
    fn settle_clean_fixture_harness(
        trace_mode: bool,
    ) -> (
        egui_kittest::Harness<'static, crate::app::SeamExplorerApp>,
        Vec<(String, egui::Pos2)>,
    ) {
        let outcome =
            crate::load::read_and_ingest(CLEAN_FIXTURE).expect("fixture must ingest cleanly");
        let app = crate::app::SeamExplorerApp {
            model: Some(outcome.model),
            seams: outcome.seams,
            trace_mode,
            ..Default::default()
        };

        let positions_mirror: std::rc::Rc<std::cell::RefCell<Vec<(String, egui::Pos2)>>> =
            std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let positions_inner = positions_mirror.clone();

        let mut harness = egui_kittest::Harness::new_ui_state(
            move |ui, app: &mut crate::app::SeamExplorerApp| {
                show(ui, app);
                *positions_inner.borrow_mut() = test_probe::load_node_screen_positions(ui);
            },
            app,
        );

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

        (harness, positions)
    }

    fn clean_fixture_pos_of(positions: &[(String, egui::Pos2)], id: &str) -> egui::Pos2 {
        positions
            .iter()
            .find(|(nid, _)| nid == id)
            .map(|(_, p)| *p)
            .unwrap_or_else(|| panic!("node {id} not found in published positions: {positions:?}"))
    }

    // ============================================================
    // quick-260926-nop Task 2: `jump_to_node`'s `None` branch -- a silent
    // no-op for an id absent from the currently rendered graph.
    // ============================================================

    /// With no published jump-target map entry for `id`, `jump_to_node`
    /// must return `false` and leave both `app.view` and `app.selected_node`
    /// exactly as they were (discovery finding 4/6's silent-no-op
    /// reasoning). Runs `show()` first (over the real `CLEAN_FIXTURE`) so a
    /// real jump-target map IS published for this frame -- just never for
    /// the made-up id this test asks about.
    #[test]
    fn jump_to_node_is_a_silent_no_op_for_an_unrendered_id() {
        let outcome =
            crate::load::read_and_ingest(CLEAN_FIXTURE).expect("fixture must ingest cleanly");
        let app = crate::app::SeamExplorerApp {
            model: Some(outcome.model),
            seams: outcome.seams,
            ..Default::default()
        };

        let ran: std::rc::Rc<std::cell::RefCell<bool>> =
            std::rc::Rc::new(std::cell::RefCell::new(false));
        let ran_inner = ran.clone();

        let mut harness = egui_kittest::Harness::new_ui_state(
            move |ui, app: &mut crate::app::SeamExplorerApp| {
                show(ui, app);
                let before_zoom = app.view.zoom;
                let before_pan = app.view.pan;
                let before_selected = app.selected_node.clone();

                let jumped = jump_to_node(ui, app, "definitely-not-a-real-node-id");

                assert!(
                    !jumped,
                    "jump_to_node must return false for an unrendered id"
                );
                assert_eq!(app.view.zoom, before_zoom);
                assert_eq!(app.view.pan, before_pan);
                assert_eq!(app.selected_node, before_selected);
                *ran_inner.borrow_mut() = true;
            },
            app,
        );
        harness.step();

        assert!(
            *ran.borrow(),
            "the render closure must have run at least once"
        );
    }

    /// With Trace mode OFF, clicking a node must arm nothing and trace
    /// nothing -- the `!app.trace_mode` early return in
    /// `handle_trace_gesture` resets to `Idle` unconditionally. Converted
    /// from the deleted `secondary_button_drag_does_not_trace_when_trace_mode_is_off`
    /// (05-18): the subject (trace mode off must block the gesture) is
    /// unchanged, only the input driving it is now a click, not a drag.
    #[test]
    fn primary_click_does_not_trace_when_trace_mode_is_off() {
        let (mut harness, positions) = settle_clean_fixture_harness(false);
        let a1_pos = clean_fixture_pos_of(&positions, "a1");

        primary_click_no_movement(&mut harness, a1_pos);

        assert_eq!(
            harness.state().trace_gesture,
            crate::trace::TraceGesture::Idle,
            "a click must not arm anything when trace_mode is off"
        );
        assert!(
            harness.state().trace.is_none(),
            "a click must not trace when trace_mode is off, got {:?}",
            harness.state().trace
        );
    }

    /// A primary click on empty canvas (a point provably far from every
    /// published node position) while armed cancels back to `Idle` and sets
    /// no trace (D-02) -- the live proof of Task 1's `EmptyClick` transition.
    /// Reuses the "far from every node" setup guard
    /// `right_click_on_empty_canvas_opens_nothing` below establishes for its
    /// own fixture.
    #[test]
    fn primary_click_on_empty_canvas_cancels_an_armed_trace() {
        let (mut harness, positions) = settle_clean_fixture_harness(true);
        let a1_pos = clean_fixture_pos_of(&positions, "a1");

        primary_click_no_movement(&mut harness, a1_pos);
        assert_eq!(
            harness.state().trace_gesture,
            crate::trace::TraceGesture::Armed {
                from: "a1".to_string()
            },
            "guard: a1 must be armed before the empty-canvas click under test"
        );

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

        primary_click_no_movement(&mut harness, empty_spot);

        assert_eq!(
            harness.state().trace_gesture,
            crate::trace::TraceGesture::Idle,
            "a primary click on empty canvas must cancel an armed trace"
        );
        assert!(
            harness.state().trace.is_none(),
            "a cancel must never set app.trace, got {:?}",
            harness.state().trace
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

    /// Builds a settled `egui_kittest::Harness` over `SOURCE_PATHS_FIXTURE`,
    /// mirroring `clicking_two_nodes_produces_a_trace`'s settle-to-stability
    /// discipline exactly (same `positions_stable`/`POSITION_SETTLE_EPSILON`/
    /// `MAX_SETTLE_STEPS` shape `settle_clean_fixture_harness` above also
    /// uses). Returns the harness and the settled node id -> screen position
    /// map -- gesture/trace state is read directly off `harness.state()`
    /// now that it lives on `SeamExplorerApp` rather than in egui temp
    /// memory, so no mirror is needed for it.
    fn settle_menu_harness(
        trace_mode: bool,
    ) -> (
        egui_kittest::Harness<'static, crate::app::SeamExplorerApp>,
        Vec<(String, egui::Pos2)>,
    ) {
        let ingest =
            seam_core::from_json(SOURCE_PATHS_FIXTURE).expect("fixture must ingest cleanly");
        // `has_seen_trace_onboarding: true` -- discovered during Task 2
        // (Rule 1 bug fix): `trace::show_onboarding`'s dismiss card is a
        // `Foreground`-order `egui::Area` anchored at the canvas's
        // RIGHT_TOP corner, and this fixture's settled `a1` position
        // ([584.7, 51.3] in the default 800x600 kittest viewport) lands
        // directly inside that card's rect. With onboarding left showing
        // (the `Default` for a freshly built `SeamExplorerApp`), a
        // right-click at `a1`'s position never reaches the `GraphView`
        // response at all -- `secondary_clicked()` stays false, not
        // because of any context-menu bug, but because the foreground
        // overlay silently absorbs the pointer event first. Dismissing it
        // up front removes an incidental collision that has nothing to do
        // with the feature this plan tests; every position printed by
        // `test_probe` is confirmed identical with or without this flag
        // (`Area`s float independently of the layout cursor), so this
        // changes no test's targeted node position.
        let app = crate::app::SeamExplorerApp {
            model: Some(ingest.model),
            trace_mode,
            has_seen_trace_onboarding: true,
            ..Default::default()
        };

        let positions_mirror: std::rc::Rc<std::cell::RefCell<Vec<(String, egui::Pos2)>>> =
            std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let positions_inner = positions_mirror.clone();

        let mut harness = egui_kittest::Harness::new_ui_state(
            move |ui, app: &mut crate::app::SeamExplorerApp| {
                show(ui, app);
                *positions_inner.borrow_mut() = test_probe::load_node_screen_positions(ui);
            },
            app,
        );

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

        (harness, positions)
    }

    fn menu_pos_of(positions: &[(String, egui::Pos2)], id: &str) -> egui::Pos2 {
        positions
            .iter()
            .find(|(nid, _)| nid == id)
            .map(|(_, p)| *p)
            .unwrap_or_else(|| panic!("node {id} not found in published positions: {positions:?}"))
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
        let (mut harness, positions) = settle_menu_harness(false);
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
        let (mut harness, positions) = settle_menu_harness(false);

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
    /// EITHER trace mode: `app.trace` stays `None` and `app.trace_gesture`
    /// stays `Idle` -- the direct lock on probe conclusion 2. **Passes
    /// today.**
    #[test]
    fn right_click_leaves_no_trace_residue_in_either_mode() {
        for trace_mode in [true, false] {
            let (mut harness, positions) = settle_menu_harness(trace_mode);
            let menu_target_pos = menu_pos_of(&positions, "a1");

            right_click_no_movement(&mut harness, menu_target_pos);

            assert!(
                harness.state().trace.is_none(),
                "trace_mode={trace_mode}: a right-click must never start or complete a trace, \
                 got {:?}",
                harness.state().trace
            );
            assert_eq!(
                harness.state().trace_gesture,
                crate::trace::TraceGesture::Idle,
                "trace_mode={trace_mode}: the trace gesture must be Idle after a right-click"
            );
        }
    }

    /// A right-click opens the "Open file" context menu and does NOT arm or
    /// complete a trace, with Trace mode ON (DP-GB2-02, a forced consequence
    /// of D-04): the secondary button already belongs to the context menu,
    /// so the retired drag gesture's old "right-button also traces"
    /// capability does not carry over to this click-based replacement.
    /// Converted from the deleted `right_drag_between_two_nodes_still_traces_and_opens_no_menu`
    /// (05-18) -- that test's SUBJECT (a right-button interaction and a
    /// trace can never both fire from the same input) is unchanged; only
    /// the answer to "which one wins" flipped, because the gesture that
    /// used to win (a right-drag tracing) no longer exists at all.
    #[test]
    fn right_click_opens_the_menu_and_does_not_arm_a_trace() {
        use egui_kittest::kittest::Queryable as _;
        let (mut harness, positions) = settle_menu_harness(true);
        let menu_target_pos = menu_pos_of(&positions, "a1");

        right_click_no_movement(&mut harness, menu_target_pos);

        assert!(
            egui::Popup::is_any_open(&harness.ctx),
            "right-clicking a node must still open the context menu with Trace mode on"
        );
        assert!(
            harness
                .query_by_label(crate::context_menu::OPEN_FILE_LABEL)
                .is_some(),
            "the menu must show the '{}' item",
            crate::context_menu::OPEN_FILE_LABEL
        );
        assert_eq!(
            harness.state().trace_gesture,
            crate::trace::TraceGesture::Idle,
            "a right-click must never arm a trace -- the secondary button belongs to the \
             context menu (DP-GB2-02)"
        );
        assert!(
            harness.state().trace.is_none(),
            "a right-click must never complete a trace, got {:?}",
            harness.state().trace
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
        let graph_dir = crate::load::graph_dir();
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

        // Build and settle the harness ONCE, outside the retry loop below --
        // `settle_menu_harness`'s own settle loop (up to 3000 steps) is by
        // far the most expensive, longest-wall-clock part of this test, and
        // it needs no configured settings at all (it only settles node
        // LAYOUT). Storing `configured` before it (as this test originally
        // did) held the racy global for that entire settle window; storing
        // it AFTER settling, right before the few interaction frames that
        // actually need it, shrinks the exposure window by roughly two
        // orders of magnitude.
        let (mut harness, positions) = settle_menu_harness(false);
        let menu_target_pos = menu_pos_of(&positions, "a1");

        // Bounded retry over just the cheap interaction frames, mirroring
        // `settings_panel.rs`'s own `an_edit_writes_through_to_a_bound_config_path`
        // precedent (05-22 Deviations): `context_menu_settings_test_lock`
        // only serializes THIS module's two settings-touching tests against
        // each other -- it cannot reach `settings_panel.rs`'s own separate
        // `settings_store_test_lock`, so a concurrently-scheduled
        // `settings_panel::tests::toggling_the_checkbox_updates_the_current_settings`
        // (or any other settings-writing test in that module) can overwrite
        // the process-global `settings::Store` mid-attempt (05-21's single
        // `OnceLock<RwLock<_>>`, by design). Observed deterministically
        // colliding on this machine's default `cargo test` scheduling
        // (Task 2, Rule 1) -- a bounded retry re-stores `configured` and
        // redoes just the click sequence (not the expensive settle) rather
        // than asserting against a single racy attempt.
        const MAX_ATTEMPTS: usize = 10;
        let mut recorded: Option<Vec<String>> = None;
        for _ in 0..MAX_ATTEMPTS {
            crate::settings::store(configured.clone());

            right_click_no_movement(&mut harness, menu_target_pos);
            // One extra settle frame: a freshly opened `egui::Area`/popup
            // does not know its own content size until the frame it first
            // paints, so `at_pointer_fixed()`'s final resting rect is one
            // frame later than the opening frame (a genuine egui
            // popup-positioning latency, discovered during Task 2 -- Rule
            // 1). Querying for the button BEFORE this settle step computes
            // a click position against the pre-settle (wrong) rect.
            harness.step();

            harness
                .get_by_label(crate::context_menu::OPEN_FILE_LABEL)
                .click();
            harness.step();

            let attempt = argv_probe::load(&harness.ctx);
            if attempt.as_ref() == Some(&expected_argv) {
                recorded = attempt;
                break;
            }
        }

        // Restore settings before any assertion that might panic, so a
        // failing assertion doesn't poison shared state for a later test.
        crate::settings::store(previous);

        let recorded = recorded.unwrap_or_else(|| {
            panic!(
                "activating Open file must record exactly the configured argv \
                 {expected_argv:?} within {MAX_ATTEMPTS} attempts -- either the wiring never \
                 records an argv at all, or the process-global settings race described above \
                 never converged"
            )
        });
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
        let (mut harness, positions) = settle_menu_harness(false);
        // `b1` AND `b2` both have no recorded source file (`b1`'s is blank,
        // `b2`'s is absent; `normalize_source_file` maps both to `None`), so
        // either satisfies this test's premise. `b2` is targeted because
        // 08-02's id-derived seeding settles `b1` at y=633.2 -- outside the
        // default 800x600 kittest viewport, where a synthetic right-click
        // reaches nothing at all. That is a fixture/viewport collision with
        // no bearing on the feature under test, the same class as the
        // trace-onboarding overlay collision `settle_menu_harness` already
        // documents. The guard below makes that class of failure say so
        // instead of masquerading as a context-menu defect.
        let menu_target_pos = menu_pos_of(&positions, "b2");
        let viewport = harness.ctx.viewport_rect();
        assert!(
            viewport.contains(menu_target_pos),
            "the targeted node settled at {menu_target_pos:?}, outside the harness \
             viewport {viewport:?} -- a right-click there reaches nothing, which is a \
             fixture/viewport collision, not a context-menu defect"
        );

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

    // ------------------------------------------------------------
    // quick-260927-tlc: the third refit-follow arming trigger,
    // `load_generation_changed`. RED: `load_generation_changed` does not
    // exist yet -- these two tests do not compile until the GREEN step
    // adds it. A does-not-compile red is a legitimate red (05-16 /
    // 260927-iy9 / 260927-rmx precedent).
    // ------------------------------------------------------------

    /// The trigger fires exactly on the frame after
    /// `SeamExplorerApp::load_generation` changes, and stays quiet on
    /// steady frames -- the same self-clearing, snapshot-and-compare shape
    /// `render_focus_changed`/`reset_sentinel_fired` already have.
    #[test]
    fn the_load_trigger_fires_once_per_load_and_not_on_steady_frames() {
        let armed_mirror: std::rc::Rc<std::cell::RefCell<Vec<bool>>> =
            std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let armed_inner = armed_mirror.clone();

        let mut harness = egui_kittest::Harness::new_ui_state(
            move |ui, app: &mut crate::app::SeamExplorerApp| {
                let armed = load_generation_changed(ui, app);
                armed_inner.borrow_mut().push(armed);
            },
            crate::app::SeamExplorerApp::default(),
        );

        // `Harness` construction itself runs the closure at least twice
        // (an accesskit-init frame plus `run_ok`'s settle frame) before any
        // explicit `.step()` -- both at steady generation 0, so they carry
        // no information for this test. Discard them so the assertion
        // below observes only the explicit sequence driven from here.
        armed_mirror.borrow_mut().clear();

        harness.step(); // steady at generation 0 -> false
        harness.state_mut().load_generation = 1;
        harness.step(); // generation changed 0 -> 1 -> true
        harness.step(); // steady at 1 -> false
        harness.state_mut().load_generation = 2;
        harness.step(); // generation changed 1 -> 2 -> true
        harness.step(); // steady at 2 -> false

        assert_eq!(
            armed_mirror.borrow().clone(),
            vec![false, true, false, true, false],
            "trigger must fire exactly once per load_generation change and stay quiet on \
             steady frames"
        );
    }

    /// Pins `<design_decision>` 3, the single deliberate divergence from
    /// the other two triggers: an app whose `load_generation` is already
    /// nonzero BEFORE the harness's first frame (mirroring
    /// `startup::preload_graph` completing before `run_native`) must still
    /// arm on that very first frame. `render_focus_changed` and
    /// `reset_sentinel_fired` both use `prev.is_some_and(|p| p != current)`
    /// and would return `false` here -- there is no previous snapshot yet.
    /// A second assertion in this test pins the divergence as "absent
    /// reads as zero", not "always arm on frame one": a first frame at
    /// generation 0 (the ordinary never-loaded case) must NOT arm.
    #[test]
    fn a_graph_loaded_before_the_first_frame_still_arms() {
        // `Harness` construction itself runs the closure at least once
        // (before any explicit `.step()` is ever called) to initialise
        // accesskit state -- that construction-time call IS "the very
        // first frame" this test means to pin, since `app.load_generation`
        // is set to 1 before the harness (and therefore before any
        // temp-data snapshot) exists at all. So this test reads the FIRST
        // element `Harness::new_ui_state` ever pushed, not a value
        // observed after an explicit `.step()`.
        let armed_mirror: std::rc::Rc<std::cell::RefCell<Vec<bool>>> =
            std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let armed_inner = armed_mirror.clone();

        let preloaded_app = crate::app::SeamExplorerApp {
            load_generation: 1,
            ..Default::default()
        };
        let _harness = egui_kittest::Harness::new_ui_state(
            move |ui, app: &mut crate::app::SeamExplorerApp| {
                let armed = load_generation_changed(ui, app);
                armed_inner.borrow_mut().push(armed);
            },
            preloaded_app,
        );

        let recorded = armed_mirror.borrow().clone();
        assert_eq!(
            recorded.first().copied(),
            Some(true),
            "a load_generation of 1 with no prior snapshot must arm on the very first frame \
             ever rendered -- this is what makes the CLI preload route (which completes \
             before frame 1) work; recorded sequence: {recorded:?}"
        );

        let armed_mirror_fresh: std::rc::Rc<std::cell::RefCell<Vec<bool>>> =
            std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let armed_fresh_inner = armed_mirror_fresh.clone();
        let _fresh_harness = egui_kittest::Harness::new_ui_state(
            move |ui, app: &mut crate::app::SeamExplorerApp| {
                let armed = load_generation_changed(ui, app);
                armed_fresh_inner.borrow_mut().push(armed);
            },
            crate::app::SeamExplorerApp::default(),
        );

        let recorded_fresh = armed_mirror_fresh.borrow().clone();
        assert_eq!(
            recorded_fresh.first().copied(),
            Some(false),
            "a fresh, never-loaded app (generation 0) must not arm on its first frame; \
             recorded sequence: {recorded_fresh:?}"
        );
    }

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

    /// quick-260927-tlc, the live proof of the actual user-reported bug: a
    /// freshly loaded graph, with NO focus set, is fit and centred on its
    /// own -- no Reset view press, no seam click. Built through the REAL
    /// load path (`load::read_and_ingest` -> `apply_load_outcome`), not
    /// via `refit_test_app_from`'s field-by-field construction (finding 7
    /// / `<design_decision>` 5): that construction leaves `load_generation`
    /// at 0 and would make this test pass for the wrong reason.
    ///
    /// RED expectation, stated honestly in advance: the two guard
    /// assertions (load_generation armed, positions non-empty) are
    /// expected to pass even before the fix. The `app.view` assertion is a
    /// certain red -- with no trigger armed, nothing in this scenario ever
    /// writes `app.view`. The fill-fraction assertion is the discriminating
    /// geometric red. The centring and inside-canvas assertions may or may
    /// not be red depending on where the unfocused force layout happens to
    /// settle relative to the canvas centre.
    #[test]
    fn a_freshly_loaded_graph_is_framed_without_pressing_reset_view() {
        let outcome =
            crate::load::read_and_ingest(CLEAN_FIXTURE).expect("fixture must ingest cleanly");
        let mut app = crate::app::SeamExplorerApp::default();
        app.apply_load_outcome(outcome);

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
                app,
            );
        harness.run_steps(FOLLOW_FRAME_CAP as usize + 20);

        // Guard: the setup must route through apply_load_outcome, or the
        // trigger under test is never armed and the rest of this test
        // passes for the wrong reason.
        assert_ne!(
            harness.state().load_generation,
            0,
            "guard: setup must route through apply_load_outcome so the load trigger is armed"
        );

        // Guard: the fixture must have actually rendered nodes.
        let positions = positions_mirror.borrow().clone();
        assert!(
            !positions.is_empty(),
            "guard: position probe published no positions -- fixture failed to ingest or render"
        );

        let view = harness.state().view;
        let default_view = crate::app::ViewState::default();
        assert!(
            (view.zoom - default_view.zoom).abs() > ZOOM_EPSILON
                || (view.pan - default_view.pan).length() > PAN_EPSILON,
            "a freshly loaded graph must not be left at ViewState::default() \
             ({default_view:?}); got {view:?} -- this is the direct statement of the \
             reported bug"
        );

        let mut screen_bounds = egui::Rect::NOTHING;
        for (_, pos) in &positions {
            screen_bounds.extend_with(*pos);
        }
        let canvas = egui::Rect::from_min_size(egui::Pos2::ZERO, REFIT_TEST_VIEWPORT);
        let canvas_center = canvas.center();
        let bbox_center = screen_bounds.center();

        // Centring (fit_view clause a): same tolerance and reasoning as
        // focused_follow_centers_the_pulled_apart_pair -- post-cap layout
        // plateau drift, not a new number.
        let center_tolerance = REFIT_TEST_VIEWPORT.y * 0.15;
        assert!(
            (bbox_center - canvas_center).length() < center_tolerance,
            "the freshly loaded graph's bounding-box centre {bbox_center:?} must land near \
             the canvas centre {canvas_center:?} (within {center_tolerance}px), got distance \
             {}",
            (bbox_center - canvas_center).length()
        );

        // Scale (fit_view clause b): the binding axis's rendered extent
        // must equal viewport_dim / (1.0 + FIT_VIEW_PADDING) -- fit_view's
        // own binding-axis formula, made observable, derived from the
        // constant rather than a hardcoded 0.909.
        let fill = (screen_bounds.width() / REFIT_TEST_VIEWPORT.x)
            .max(screen_bounds.height() / REFIT_TEST_VIEWPORT.y);
        let expected_fill = 1.0 / (1.0 + FIT_VIEW_PADDING);
        assert!(
            (fill - expected_fill).abs() < 0.15,
            "fill fraction {fill} must be within 0.15 of fit_view's own binding-axis fraction \
             {expected_fill} (derived from FIT_VIEW_PADDING = {FIT_VIEW_PADDING})"
        );

        // Every rendered position lies inside the canvas rect (with
        // margin), matching focused_follow_frames_every_node_inside_the_canvas.
        let margin = 8.0;
        for (id, pos) in &positions {
            assert!(
                canvas.expand(margin).contains(*pos),
                "node {id} at {pos:?} must land inside the canvas ({canvas:?}, {margin}px \
                 margin)"
            );
        }
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
    // ============================================================
    // quick-260926-gh2: edge line/arrowhead contrast raised to a measured
    // 7.45:1 against the canvas background, while staying >= 1.8:1 dimmer
    // than the near-white node label text. All four tests measure the REAL
    // production colour through `edge_stroke_color()`, never a re-derived
    // literal -- see PLAN.md discovery finding 1 for why that distinction
    // is the whole point (nominal `#93a1bd` premultiplies to an effective
    // `#79849a` at the old alpha of 200).

    /// sRGB channel linearization (the `<= 0.04045` piecewise branch),
    /// hoisted to module scope (quick-260926-nnr) so `relative_luminance`
    /// AND the new CIELAB/CVD helpers below share one definition rather
    /// than each nesting their own copy. Behaviour-preserving move: proof
    /// is that gh2's four `edge_*` tests below keep passing with identical
    /// measured values after this hoist.
    fn linearize(channel: u8) -> f64 {
        let c = channel as f64 / 255.0;
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    }

    /// sRGB relative luminance, per the WCAG formula: linearize each
    /// channel, then weight 0.2126/0.7152/0.0722.
    fn relative_luminance(c: egui::Color32) -> f64 {
        0.2126 * linearize(c.r()) + 0.7152 * linearize(c.g()) + 0.0722 * linearize(c.b())
    }

    /// WCAG contrast ratio between two colours: `(lighter + 0.05) / (darker + 0.05)`.
    fn contrast_ratio(a: egui::Color32, b: egui::Color32) -> f64 {
        let la = relative_luminance(a);
        let lb = relative_luminance(b);
        let (lighter, darker) = if la >= lb { (la, lb) } else { (lb, la) };
        (lighter + 0.05) / (darker + 0.05)
    }

    /// Models epaint's real PREMULTIPLIED compositing: `fg` was already
    /// premultiplied at construction time by the same `Color32` constructor
    /// `edge_stroke_color()` uses, so the paint-time blend against `bg` is
    /// `fg.channel + bg.channel * (1 - fg.a() / 255)` per channel, and the
    /// result is fully opaque. At `EDGE_ALPHA = 255` this correctly
    /// degenerates to `fg` unchanged. Built via `Color32::from_rgb` rather
    /// than re-deriving through the premultiplying constructor, so this
    /// helper cannot become a second, drifting copy of the colour
    /// construction that `edge_stroke_color()` alone owns.
    fn composite_over(fg: egui::Color32, bg: egui::Color32) -> egui::Color32 {
        let a = fg.a() as f64 / 255.0;
        let blend = |f: u8, b: u8| -> u8 {
            (f as f64 + b as f64 * (1.0 - a)).round().clamp(0.0, 255.0) as u8
        };
        egui::Color32::from_rgb(
            blend(fg.r(), bg.r()),
            blend(fg.g(), bg.g()),
            blend(fg.b(), bg.b()),
        )
    }

    /// The effective painted edge colour (composited over the real canvas
    /// fill, `egui::Visuals::dark().panel_fill`, read live rather than
    /// hardcoded) clears 7.0:1 WCAG contrast. FAILS today at ~4.577:1,
    /// because `EDGE_ALPHA = 200` washes 21.6% of the background into every
    /// line before it ever reaches the screen. An upper sanity ceiling of
    /// 12.0 is asserted too, so a future "just make it white" edit trips
    /// this test rather than passing it (quick-260926-gh2).
    #[test]
    fn edge_contrasts_against_the_canvas_background() {
        let bg = egui::Visuals::dark().panel_fill;
        let effective = composite_over(edge_stroke_color(), bg);
        let ratio = contrast_ratio(effective, bg);
        assert!(
            ratio >= 7.0,
            "edge-vs-background contrast must be >= 7.0, measured {ratio:.3} \
             (effective colour {effective:?} over background {bg:?})"
        );
        assert!(
            ratio < 12.0,
            "edge-vs-background contrast must stay below the 12.0 sanity ceiling \
             (a maximally bright edge would collide with the label), measured {ratio:.3}"
        );
    }

    /// REGRESSION GUARD, not a RED assertion (`<design_decision>` 6): the
    /// effective painted edge colour must stay >= 1.8:1 dimmer than
    /// `TEXT_HEX`, so brightening the line for background contrast does not
    /// merge it into the near-white node label it runs under (labels paint
    /// AFTER edges -- discovery finding 4). This already passes today at
    /// ~3.000:1 and continues to pass after this change at ~1.843:1.
    /// Lowering this floor to go brighter still is a user decision that
    /// must be recorded in the same commit as the new number, never a
    /// silent retune (quick-260926-gh2).
    #[test]
    fn edge_stays_dimmer_than_node_label_text() {
        let bg = egui::Visuals::dark().panel_fill;
        let effective = composite_over(edge_stroke_color(), bg);
        let text = hex(TEXT_HEX);
        let ratio = contrast_ratio(text, effective);
        assert!(
            ratio >= 1.8,
            "edge-vs-label-text separation must be >= 1.8, measured {ratio:.3} \
             (effective edge colour {effective:?}, text colour {text:?})"
        );
    }

    /// The edge stroke must be fully opaque -- no background wash
    /// composited into the line at paint time. FAILS today: `EDGE_ALPHA`
    /// is `200`, not `255` (quick-260926-gh2).
    #[test]
    fn edge_stroke_is_fully_opaque() {
        let alpha = edge_stroke_color().a();
        assert_eq!(
            alpha, 255,
            "edge stroke alpha must be 255 (fully opaque), measured {alpha}"
        );
    }

    /// The edge stroke must be heavier than today (`>= 2.0`) while staying
    /// strictly below the `3.0` ceiling that keeps `SeamEdgeShape::is_inside`'s
    /// width-floored click-tolerance arithmetically unchanged
    /// (discovery finding 8). The arrowhead must also be modestly larger
    /// (`> 8.0`). FAILS today: `EDGE_WIDTH` is `1.5` (quick-260926-gh2).
    ///
    /// Read through `std::hint::black_box` so these comparisons are genuine
    /// runtime assertions rather than compile-time-foldable expressions --
    /// otherwise `cargo clippy -D warnings` flags them as
    /// `assertions_on_constants` (the comparison against a `const` value
    /// const-folds to a literal `bool`), even though the whole point of
    /// this test is to catch a future edit to `EDGE_WIDTH`/`ARROW_TIP_SIZE`.
    #[test]
    fn edge_width_is_heavier_but_preserves_click_tolerance() {
        let width = std::hint::black_box(EDGE_WIDTH);
        let tip_size = std::hint::black_box(ARROW_TIP_SIZE);
        assert!(width >= 2.0, "EDGE_WIDTH must be >= 2.0, measured {width}");
        assert!(
            width < 3.0,
            "EDGE_WIDTH must stay < 3.0 or SeamEdgeShape::is_inside's click \
             tolerance silently widens (its floor is keyed to this width), measured {width}"
        );
        assert!(
            tip_size > 8.0,
            "ARROW_TIP_SIZE must be > 8.0, measured {tip_size}"
        );
    }

    // ============================================================
    // quick-260926-nnr: the arrowhead gets its own distinct green, derived
    // as the CIELAB hue bisector of the two focused-seam side tints. All
    // six tests below measure the REAL production colours through
    // `arrow_head_color()` and `edge_stroke_color()`, and read the side
    // tints through `hex(SIDE_A_HEX)`/`hex(SIDE_B_HEX)` and the background
    // through `egui::Visuals::dark().panel_fill` -- nothing is re-derived
    // from a literal (gh2's lesson: a test which rebuilds the colour
    // itself measures the colour the developer intended, not the colour
    // that reaches the screen).

    /// Converts a colour to CIELAB (D65 2 degree white point) via linear
    /// sRGB -> XYZ -> Lab, using the hoisted `linearize`.
    fn to_lab(c: egui::Color32) -> (f64, f64, f64) {
        let r = linearize(c.r());
        let g = linearize(c.g());
        let b = linearize(c.b());

        let x = 0.4124564 * r + 0.3575761 * g + 0.1804375 * b;
        let y = 0.2126729 * r + 0.7151522 * g + 0.0721750 * b;
        let z = 0.0193339 * r + 0.1191920 * g + 0.9503041 * b;

        let xn = 0.95047;
        let yn = 1.0;
        let zn = 1.08883;

        fn f(t: f64) -> f64 {
            if t > 216.0 / 24389.0 {
                t.cbrt()
            } else {
                (841.0 / 108.0) * t + 4.0 / 29.0
            }
        }

        let fx = f(x / xn);
        let fy = f(y / yn);
        let fz = f(z / zn);

        let l = 116.0 * fy - 16.0;
        let a = 500.0 * (fx - fy);
        let b_ = 200.0 * (fy - fz);
        (l, a, b_)
    }

    /// CIELAB chroma: `sqrt(a*^2 + b*^2)`.
    fn lab_chroma(c: egui::Color32) -> f64 {
        let (_, a, b) = to_lab(c);
        (a * a + b * b).sqrt()
    }

    /// CIELAB hue angle in degrees, `atan2(b*, a*)` normalised to `[0, 360)`.
    fn lab_hue_deg(c: egui::Color32) -> f64 {
        let (_, a, b) = to_lab(c);
        let deg = b.atan2(a).to_degrees();
        if deg < 0.0 {
            deg + 360.0
        } else {
            deg
        }
    }

    /// Circular hue distance between two colours' CIELAB hue angles,
    /// always `<= 180`.
    fn hue_distance_deg(a: egui::Color32, b: egui::Color32) -> f64 {
        let ha = lab_hue_deg(a);
        let hb = lab_hue_deg(b);
        let diff = (ha - hb).abs() % 360.0;
        if diff > 180.0 {
            360.0 - diff
        } else {
            diff
        }
    }

    /// CIELAB Delta E76: Euclidean distance in L*a*b* space.
    fn delta_e76(a: egui::Color32, b: egui::Color32) -> f64 {
        let (l1, a1, b1) = to_lab(a);
        let (l2, a2, b2) = to_lab(b);
        ((l1 - l2).powi(2) + (a1 - a2).powi(2) + (b1 - b2).powi(2)).sqrt()
    }

    /// Viénot 1999 RGB -> LMS matrix, row-major.
    const CVD_RGB_TO_LMS: [[f64; 3]; 3] = [
        [17.8824, 43.5161, 4.11935],
        [3.45565, 27.1554, 3.86714],
        [0.0299566, 0.184309, 1.46709],
    ];

    /// Numeric inverse of `CVD_RGB_TO_LMS`, row-major.
    const CVD_LMS_TO_RGB: [[f64; 3]; 3] = [
        [0.080944448, -0.130504409, 0.116721066],
        [-0.010248534, 0.054019327, -0.113614708],
        [-0.000365297, -0.004121615, 0.693511405],
    ];

    /// Deuteranope LMS projection matrix (Viénot 1999), row-major.
    const CVD_DEUTERANOPE_PROJECTION: [[f64; 3]; 3] =
        [[1.0, 0.0, 0.0], [0.494207, 0.0, 1.24827], [0.0, 0.0, 1.0]];

    /// Protanope LMS projection matrix (Viénot 1999), row-major.
    const CVD_PROTANOPE_PROJECTION: [[f64; 3]; 3] =
        [[0.0, 2.02344, -2.52581], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];

    fn matvec(m: [[f64; 3]; 3], v: [f64; 3]) -> [f64; 3] {
        [
            m[0][0] * v[0] + m[0][1] * v[1] + m[0][2] * v[2],
            m[1][0] * v[0] + m[1][1] * v[1] + m[1][2] * v[2],
            m[2][0] * v[0] + m[2][1] * v[1] + m[2][2] * v[2],
        ]
    }

    /// Simulates how `c` would appear to a colour-vision-deficient observer
    /// under the given LMS `projection` (Viénot 1999): linearize to `0..1`,
    /// RGB -> LMS, apply the projection, LMS -> RGB, then re-encode sRGB
    /// gamma. The matrices are applied to linear RGB in `0..1` with no 255
    /// scaling -- the chain is linear, so scaling in and out cancels
    /// exactly (verified numerically at planning time against the
    /// 255-scaled form; produces the same simulated hexes).
    fn simulate_cvd(c: egui::Color32, projection: [[f64; 3]; 3]) -> egui::Color32 {
        let linear = [linearize(c.r()), linearize(c.g()), linearize(c.b())];
        let lms = matvec(CVD_RGB_TO_LMS, linear);
        let sim_lms = matvec(projection, lms);
        let sim_linear = matvec(CVD_LMS_TO_RGB, sim_lms);

        let encode = |v: f64| -> u8 {
            let v = v.clamp(0.0, 1.0);
            let encoded = if v <= 0.0031308 {
                12.92 * v
            } else {
                1.055 * v.powf(1.0 / 2.4) - 0.055
            };
            (encoded * 255.0).round().clamp(0.0, 255.0) as u8
        };

        egui::Color32::from_rgb(
            encode(sim_linear[0]),
            encode(sim_linear[1]),
            encode(sim_linear[2]),
        )
    }

    /// The arrowhead must have its own colour, measurably distinct from the
    /// edge line -- ΔE76 >= 60.0 between the real production
    /// `arrow_head_color()` and `edge_stroke_color()`. RED today: the
    /// arrowhead shares the line's colour exactly (measured ΔE76 0.000),
    /// which is simultaneously the proof of the reported defect and the
    /// proof that the RED-phase refactor introducing `arrow_head_color()`
    /// was zero-pixel (quick-260926-nnr).
    #[test]
    fn arrow_head_has_its_own_colour_distinct_from_the_edge_line() {
        let head = arrow_head_color();
        let line = edge_stroke_color();
        let de = delta_e76(head, line);
        assert!(
            de >= 60.0,
            "arrowhead-vs-line ΔE76 must be >= 60.0, measured {de:.3} \
             (head {head:?}, line {line:?})"
        );
    }

    /// The arrowhead colour must read as GREEN: CIELAB hue in the
    /// `[110.0, 145.0]` band between the side-B orange and the side-A teal,
    /// AND chroma >= 60.0 so it is unmistakably chromatic rather than a
    /// greenish grey. RED today on BOTH arms: the arrowhead is today's
    /// blue-grey edge colour, measuring hue ~274.13° and chroma ~15.94
    /// (quick-260926-nnr).
    #[test]
    fn arrow_head_colour_reads_as_green() {
        let head = arrow_head_color();
        let hue = lab_hue_deg(head);
        let chroma = lab_chroma(head);
        let hue_ok = (110.0..=145.0).contains(&hue);
        let chroma_ok = chroma >= 60.0;
        assert!(
            hue_ok && chroma_ok,
            "arrowhead must read as green: hue must fall in [110.0, 145.0] (measured \
             {hue:.2}°, ok={hue_ok}) AND chroma must be >= 60.0 (measured {chroma:.2}, \
             ok={chroma_ok})"
        );
    }

    /// The arrowhead green must not be confusable with either focused-seam
    /// side tint. Against `SIDE_A_HEX` (teal): hue distance >= 45.0 AND
    /// ΔE76 >= 50.0. Against `SIDE_B_HEX` (orange): hue distance >= 45.0.
    /// PARTIALLY red today (quick-260926-nnr): the two hue arms already
    /// pass because today's blue-grey is already far in hue from both
    /// tints, but the teal ΔE76 arm fails (measured ~47.61, below the 50.0
    /// floor) since the blue-grey and the teal are not yet far enough
    /// apart overall.
    #[test]
    fn arrow_head_green_is_not_confusable_with_either_side_tint() {
        let head = arrow_head_color();
        let side_a = hex(SIDE_A_HEX);
        let side_b = hex(SIDE_B_HEX);

        let hue_dist_a = hue_distance_deg(head, side_a);
        let de_a = delta_e76(head, side_a);
        let hue_dist_b = hue_distance_deg(head, side_b);

        assert!(
            hue_dist_a >= 45.0,
            "arrowhead-vs-side-A hue distance must be >= 45.0, measured {hue_dist_a:.2}°"
        );
        assert!(
            de_a >= 50.0,
            "arrowhead-vs-side-A ΔE76 must be >= 50.0, measured {de_a:.2}"
        );
        assert!(
            hue_dist_b >= 45.0,
            "arrowhead-vs-side-B hue distance must be >= 45.0, measured {hue_dist_b:.2}°"
        );
    }

    /// REGRESSION GUARD, not a RED assertion (`<design_decision>` 3 of
    /// quick-260926-nnr): the arrowhead colour, composited over the real
    /// canvas fill, must clear the same >= 7.0:1 WCAG contrast floor and
    /// stay below the same < 12.0 sanity ceiling gh2 established for the
    /// edge line, and must stay >= 1.8:1 separated from `TEXT_HEX`. This
    /// already passes today at ~7.450:1 / ~1.843:1 because the arrowhead IS
    /// the line colour today, and continues to pass after the green lands
    /// because the green was deliberately chosen to land on the same
    /// figures. Lowering the 1.8 floor for the arrowhead specifically would
    /// be a user decision, never a silent one.
    #[test]
    fn arrow_head_keeps_the_established_contrast_discipline() {
        let bg = egui::Visuals::dark().panel_fill;
        let head = arrow_head_color();
        let effective = composite_over(head, bg);
        let bg_ratio = contrast_ratio(effective, bg);
        let text = hex(TEXT_HEX);
        let text_ratio = contrast_ratio(text, effective);

        assert!(
            bg_ratio >= 7.0,
            "arrowhead-vs-background contrast must be >= 7.0, measured {bg_ratio:.3}"
        );
        assert!(
            bg_ratio < 12.0,
            "arrowhead-vs-background contrast must stay below the 12.0 sanity ceiling, \
             measured {bg_ratio:.3}"
        );
        assert!(
            text_ratio >= 1.8,
            "arrowhead-vs-label-text separation must be >= 1.8, measured {text_ratio:.3}"
        );
    }

    /// GUARD (not a RED assertion): the arrowhead must add no luminance
    /// over the edge line -- `|Δrelative-luminance| <= 0.02`. Trivially
    /// 0.000000 today (arrowhead == line colour); after the green lands it
    /// measures ~0.0000062, i.e. iso-luminant to five decimal places. This
    /// is the load-bearing safety property of the whole change
    /// (quick-260926-nnr `<design_decision>` 3): the arrowhead buys its
    /// salience entirely with hue, so this change cannot make a dense
    /// graph brighter or busier. A future edit that brightens the
    /// arrowhead is exactly what this test exists to stop.
    #[test]
    fn arrow_head_adds_no_luminance_over_the_edge_line() {
        let head = arrow_head_color();
        let line = edge_stroke_color();
        let delta = (relative_luminance(head) - relative_luminance(line)).abs();
        assert!(
            delta <= 0.02,
            "arrowhead-vs-line relative luminance delta must be <= 0.02, measured {delta:.7}"
        );
    }

    /// The arrowhead must stay distinct from the edge line for red-green
    /// colour-vision-deficient viewers: after a Viénot-1999 deuteranope
    /// simulation AND a protanope simulation, ΔE76 between the simulated
    /// arrowhead and simulated edge line must be >= 40.0 for both. RED
    /// today: the arrowhead and line are the same colour, so both
    /// simulated ΔE76 measure 0.00 (quick-260926-nnr).
    ///
    /// Two collisions are known and accepted rather than tested, because no
    /// reachable green value would pass them: under deuteranopia/
    /// protanopia the arrowhead green converges with the side-B orange tint
    /// (ΔE76 ~10) -- an unavoidable consequence of red-green colour
    /// blindness collapsing that axis, bounded in practice by the
    /// arrowhead's small triangular shape and terminal position versus the
    /// side tint's much larger node-circle-fill and side-label surfaces,
    /// and by the side tint only existing while a seam is focused. Under
    /// (very rare) tritanopia the arrowhead green converges with the side-A
    /// teal tint (ΔE76 ~4.1); accepted for a personal/team tool.
    #[test]
    fn arrow_head_stays_distinct_from_the_line_under_red_green_colour_blindness() {
        let head = arrow_head_color();
        let line = edge_stroke_color();

        let head_deutan = simulate_cvd(head, CVD_DEUTERANOPE_PROJECTION);
        let line_deutan = simulate_cvd(line, CVD_DEUTERANOPE_PROJECTION);
        let de_deutan = delta_e76(head_deutan, line_deutan);

        let head_protan = simulate_cvd(head, CVD_PROTANOPE_PROJECTION);
        let line_protan = simulate_cvd(line, CVD_PROTANOPE_PROJECTION);
        let de_protan = delta_e76(head_protan, line_protan);

        let deutan_ok = de_deutan >= 40.0;
        let protan_ok = de_protan >= 40.0;
        assert!(
            deutan_ok && protan_ok,
            "arrowhead-vs-line ΔE76 must be >= 40.0 under BOTH simulations: deuteranope \
             measured {de_deutan:.2} (ok={deutan_ok}), protanope measured {de_protan:.2} \
             (ok={protan_ok})"
        );
    }

    // ============================================================
    // quick-260927-iy9 Task 1: two independently-driven rings -- a derived
    // blue for the jumped-to node, the shipped red pushed out to a
    // concentric outer ring. All colour assertions measure the real
    // production `jump_ring_color()`/`trace_armed_ring_color()` (never a
    // re-derived literal, gh2's lesson).
    // ============================================================

    /// The load-bearing RED: 0.000 ΔE76 / 0.00° hue distance today, which
    /// simultaneously proves the reported defect (the jump ring and the
    /// trace-armed ring are literally the same colour) and proves the
    /// RED-phase structural refactor (routing `shapes()` through
    /// `jump_ring_color()`) was zero-pixel.
    #[test]
    fn jump_ring_is_its_own_colour_distinct_from_the_trace_armed_ring() {
        let jump = jump_ring_color();
        let armed = trace_armed_ring_color();
        let de = delta_e76(jump, armed);
        let hue_dist = hue_distance_deg(jump, armed);
        assert!(
            de >= 60.0 && hue_dist >= 80.0,
            "jump-vs-armed ring ΔE76 must be >= 60.0 AND hue distance must be >= 80.0, \
             measured ΔE76 {de:.3}, hue distance {hue_dist:.2}°"
        );
    }

    /// The jump ring must read as BLUE: CIELAB hue in `[265.0, 290.0]` AND
    /// chroma >= 50.0. RED today: `jump_ring_color()` is still the shipped
    /// red (hue ~3.41°).
    #[test]
    fn jump_ring_colour_reads_as_blue() {
        let jump = jump_ring_color();
        let hue = lab_hue_deg(jump);
        let chroma = lab_chroma(jump);
        let hue_ok = (265.0..=290.0).contains(&hue);
        let chroma_ok = chroma >= 50.0;
        assert!(
            hue_ok && chroma_ok,
            "jump ring must read as blue: hue must fall in [265.0, 290.0] (measured \
             {hue:.2}°, ok={hue_ok}) AND chroma must be >= 50.0 (measured {chroma:.2}, \
             ok={chroma_ok})"
        );
    }

    /// GUARD, not RED (`<design_decision>` 2): `EDGE_HEX` and
    /// `DIMMED_FILL_HEX` already sit within 3 degrees of any blue's hue --
    /// they are themselves blue-greys -- so a hue-distance assertion here
    /// would pass vacuously. ΔE76 (which also weighs chroma and lightness)
    /// is the honest discriminator, and already clears 40.0 today (73.4 /
    /// 74.2) since the shipped red is nowhere near either token.
    #[test]
    fn jump_ring_is_not_confusable_with_the_blue_grey_line_or_the_unfocused_node_fill() {
        let jump = jump_ring_color();
        let edge = edge_stroke_color();
        let fill = hex(DIMMED_FILL_HEX);
        let de_edge = delta_e76(jump, edge);
        let de_fill = delta_e76(jump, fill);
        assert!(
            de_edge >= 40.0,
            "jump-ring-vs-edge-line ΔE76 must be >= 40.0, measured {de_edge:.3}"
        );
        assert!(
            de_fill >= 40.0,
            "jump-ring-vs-unfocused-fill ΔE76 must be >= 40.0, measured {de_fill:.3}"
        );
    }

    /// GUARD (`<design_decision>` 2): the 5.49 floor is not gh2/nnr's 7.0 --
    /// it is parity with the already-shipped red ring's own measured
    /// 5.494:1 background contrast. A 4pt ring is a far larger mark than a
    /// 2.25pt line, so the honest bar is "at least as visible as the ring
    /// already shipped beside it", not an independently chosen number.
    /// Already passes today (5.494 / 2.499) since the jump ring is still
    /// the shipped red.
    #[test]
    fn jump_ring_is_at_least_as_visible_as_the_ring_it_sits_beside() {
        let bg = egui::Visuals::dark().panel_fill;
        let jump = jump_ring_color();
        let effective = composite_over(jump, bg);
        let bg_ratio = contrast_ratio(effective, bg);
        let text = hex(TEXT_HEX);
        let text_ratio = contrast_ratio(text, effective);
        assert!(
            bg_ratio >= 5.49,
            "jump-ring-vs-background contrast must be >= 5.49, measured {bg_ratio:.3}"
        );
        assert!(
            bg_ratio < 12.0,
            "jump-ring-vs-background contrast must stay below the 12.0 sanity ceiling, \
             measured {bg_ratio:.3}"
        );
        assert!(
            text_ratio >= 1.8,
            "jump-ring-vs-label-text separation must be >= 1.8, measured {text_ratio:.3}"
        );
    }

    /// GUARD, trivially 0.000000 today (jump ring == armed ring), 0.003456
    /// after the blue lands: the two rings must differ by hue and chroma,
    /// not by brightness, so neither shouts louder than the other. This is
    /// the guard against a future "just make the blue pop more" edit.
    #[test]
    fn the_two_rings_differ_in_hue_not_brightness() {
        let jump = jump_ring_color();
        let armed = trace_armed_ring_color();
        let delta = (relative_luminance(jump) - relative_luminance(armed)).abs();
        assert!(
            delta <= 0.02,
            "jump-vs-armed ring relative luminance delta must be <= 0.02, measured {delta:.7}"
        );
    }

    /// RED at 0.00 / 0.00 today (same colour, so both simulations collapse
    /// to zero distance): the two rings must stay distinguishable for
    /// red-green colour-vision-deficient viewers after a Viénot-1999
    /// deuteranope AND protanope simulation, ΔE76 >= 40.0 for both.
    #[test]
    fn the_two_rings_stay_distinct_under_red_green_colour_blindness() {
        let jump = jump_ring_color();
        let armed = trace_armed_ring_color();

        let jump_deutan = simulate_cvd(jump, CVD_DEUTERANOPE_PROJECTION);
        let armed_deutan = simulate_cvd(armed, CVD_DEUTERANOPE_PROJECTION);
        let de_deutan = delta_e76(jump_deutan, armed_deutan);

        let jump_protan = simulate_cvd(jump, CVD_PROTANOPE_PROJECTION);
        let armed_protan = simulate_cvd(armed, CVD_PROTANOPE_PROJECTION);
        let de_protan = delta_e76(jump_protan, armed_protan);

        let deutan_ok = de_deutan >= 40.0;
        let protan_ok = de_protan >= 40.0;
        assert!(
            deutan_ok && protan_ok,
            "jump-vs-armed ring ΔE76 must be >= 40.0 under BOTH simulations: deuteranope \
             measured {de_deutan:.2} (ok={deutan_ok}), protanope measured {de_protan:.2} \
             (ok={protan_ok})"
        );
    }

    /// GUARD: the trace-armed ring must keep the shipped `SELECTED_RING_HEX`
    /// value and `SELECTED_RING_WIDTH` byte-for-byte (`<scope_boundary>`).
    /// Read through `std::hint::black_box` so the comparison is a genuine
    /// runtime assertion rather than a compile-time-foldable expression that
    /// `cargo clippy -D warnings` would flag as `assertions_on_constants`
    /// (precedent: `edge_width_is_heavier_but_preserves_click_tolerance`).
    #[test]
    fn the_trace_armed_ring_keeps_its_shipped_red() {
        let hex_val = std::hint::black_box(SELECTED_RING_HEX);
        let width = std::hint::black_box(SELECTED_RING_WIDTH);
        assert_eq!(
            hex_val, "#ff4d8d",
            "SELECTED_RING_HEX must stay byte-for-byte the shipped red, measured {hex_val}"
        );
        assert_eq!(
            width, 4.0,
            "SELECTED_RING_WIDTH must stay 4.0, measured {width}"
        );
    }

    /// The four-state truth table CONTEXT.md asks for, read off the REAL
    /// built graph's display objects (via `display().is_jump_selected` /
    /// `.is_trace_armed`) rather than a hand-made struct. RED today: both
    /// flags are false in every arm, since `apply_focus_styling` does not
    /// write them yet.
    #[test]
    fn apply_focus_styling_drives_the_two_rings_independently() {
        let outcome =
            crate::load::read_and_ingest(CLEAN_FIXTURE).expect("fixture must ingest cleanly");
        let model = outcome.model;
        let scc = model
            .scc
            .as_ref()
            .expect("read_and_ingest must finalize scc");
        let focus = focus_state();
        let detail = seam_core::seam_detail(&model, scc, &focus.a, &focus.b);
        assert!(
            detail.bridges_a.iter().any(|id| id == "a1"),
            "a1 must be a real bridge node for this test to be meaningful"
        );

        fn flags_for(graph: &SeamGraph, id: &str) -> (bool, bool) {
            graph
                .nodes_iter()
                .find(|(_, n)| n.payload().id == id)
                .map(|(_, n)| (n.display().is_jump_selected, n.display().is_trace_armed))
                .unwrap_or_else(|| panic!("node {id} not found in graph"))
        }

        // ONLY `selected_node` set -> jump true, armed false everywhere.
        let mut graph_jump_only =
            build_graph(&model, Some(&focus), &std::collections::HashSet::new());
        let app_jump_only = crate::app::SeamExplorerApp {
            focus: Some(focus.clone()),
            detail: Some(detail.clone()),
            selected_node: Some("a1".to_string()),
            ..Default::default()
        };
        apply_focus_styling(&mut graph_jump_only, &app_jump_only);
        assert_eq!(
            flags_for(&graph_jump_only, "a1"),
            (true, false),
            "jump-only: a1 must be jump-selected and not armed"
        );
        assert!(
            graph_jump_only
                .nodes_iter()
                .all(|(_, n)| n.payload().id == "a1"
                    || (!n.display().is_jump_selected && !n.display().is_trace_armed)),
            "jump-only: no node other than a1 may carry either flag"
        );

        // ONLY `TraceGesture::Armed` -> armed true, jump false.
        let mut graph_armed_only =
            build_graph(&model, Some(&focus), &std::collections::HashSet::new());
        let app_armed_only = crate::app::SeamExplorerApp {
            focus: Some(focus.clone()),
            detail: Some(detail.clone()),
            trace_gesture: crate::trace::TraceGesture::Armed {
                from: "a1".to_string(),
            },
            ..Default::default()
        };
        apply_focus_styling(&mut graph_armed_only, &app_armed_only);
        assert_eq!(
            flags_for(&graph_armed_only, "a1"),
            (false, true),
            "armed-only: a1 must be armed and not jump-selected"
        );

        // BOTH, on the SAME node.
        let mut graph_both_same =
            build_graph(&model, Some(&focus), &std::collections::HashSet::new());
        let app_both_same = crate::app::SeamExplorerApp {
            focus: Some(focus.clone()),
            detail: Some(detail.clone()),
            selected_node: Some("a1".to_string()),
            trace_gesture: crate::trace::TraceGesture::Armed {
                from: "a1".to_string(),
            },
            ..Default::default()
        };
        apply_focus_styling(&mut graph_both_same, &app_both_same);
        assert_eq!(
            flags_for(&graph_both_same, "a1"),
            (true, true),
            "both-same-node: a1 must be both jump-selected and armed"
        );

        // BOTH, on DIFFERENT nodes.
        let mut graph_both_diff =
            build_graph(&model, Some(&focus), &std::collections::HashSet::new());
        let app_both_diff = crate::app::SeamExplorerApp {
            focus: Some(focus.clone()),
            detail: Some(detail.clone()),
            selected_node: Some("b1".to_string()),
            trace_gesture: crate::trace::TraceGesture::Armed {
                from: "a1".to_string(),
            },
            ..Default::default()
        };
        apply_focus_styling(&mut graph_both_diff, &app_both_diff);
        assert_eq!(
            flags_for(&graph_both_diff, "a1"),
            (false, true),
            "both-different-nodes: a1 must be armed only"
        );
        assert_eq!(
            flags_for(&graph_both_diff, "b1"),
            (true, false),
            "both-different-nodes: b1 must be jump-selected only"
        );

        // NEITHER -> both false everywhere.
        let mut graph_neither =
            build_graph(&model, Some(&focus), &std::collections::HashSet::new());
        let app_neither = crate::app::SeamExplorerApp {
            focus: Some(focus),
            detail: Some(detail),
            ..Default::default()
        };
        apply_focus_styling(&mut graph_neither, &app_neither);
        assert!(
            graph_neither
                .nodes_iter()
                .all(|(_, n)| !n.display().is_jump_selected && !n.display().is_trace_armed),
            "neither: no node may carry either flag"
        );
    }

    /// The live test that cannot be faked: calls the real
    /// `SeamNodeShape::shapes` through a real `DrawContext` (built inside an
    /// `egui_kittest::Harness` render closure -- `shapes()` calls
    /// `ctx.ctx.fonts_mut(...)` for the label galley, so it must run inside
    /// a live frame, discovery finding 10) and counts the circles that come
    /// back, with radius ordering, stroke colours, and a transparent outer
    /// fill all asserted. Two booleans being true is not the claim; two
    /// shapes reaching the paint list is.
    ///
    /// quick-260927-rmx: the armed-only arm that used to live here (two
    /// circles, node stroke `Stroke::NONE`) moved out to its own test,
    /// `an_armed_node_with_no_jump_ring_hugs_the_node`, with strictly more
    /// rigor (three zooms, an explicit radius reference, a stroke-width
    /// check) -- it asserted the opposite of this task's fix and would have
    /// contradicted the new test otherwise. This test keeps its remaining
    /// three arms (both / jump-only / neither) byte-for-byte, and now builds
    /// its display objects through the shared `node_display_with_flags` /
    /// `circles_of` module helpers instead of a nested `shape_for`.
    #[test]
    fn a_node_that_is_both_jumped_to_and_armed_paints_two_concentric_rings() {
        let outcome =
            crate::load::read_and_ingest(CLEAN_FIXTURE).expect("fixture must ingest cleanly");
        let model = outcome.model;
        let focus = focus_state();

        let ran: std::rc::Rc<std::cell::RefCell<bool>> =
            std::rc::Rc::new(std::cell::RefCell::new(false));
        let ran_inner = ran.clone();
        let model_for_closure = model.clone();
        let focus_for_closure = focus.clone();

        let app = crate::app::SeamExplorerApp::default();
        let mut harness = egui_kittest::Harness::new_ui_state(
            move |ui, _app: &mut crate::app::SeamExplorerApp| {
                let meta = egui_graphs::MetadataFrame::default();
                let style = egui_graphs::SettingsStyle::default();
                let draw_ctx = egui_graphs::DrawContext {
                    ctx: ui.ctx(),
                    painter: ui.painter(),
                    style: &style,
                    is_directed: true,
                    meta: &meta,
                };

                // BOTH: exactly two circles.
                let mut both_shape = node_display_with_flags(
                    &model_for_closure,
                    &focus_for_closure,
                    "a1",
                    true,
                    true,
                );
                let both_shapes = both_shape.shapes(&draw_ctx);
                let both_circles = circles_of(&both_shapes);
                assert_eq!(
                    both_circles.len(),
                    2,
                    "a jump-selected AND armed node must paint exactly two circles, got {}",
                    both_circles.len()
                );
                let (inner, outer) = if both_circles[0].radius <= both_circles[1].radius {
                    (both_circles[0], both_circles[1])
                } else {
                    (both_circles[1], both_circles[0])
                };
                assert_eq!(
                    inner.stroke.color,
                    jump_ring_color(),
                    "the smaller circle must carry the jump ring colour in its stroke"
                );
                assert_eq!(
                    outer.stroke.color,
                    trace_armed_ring_color(),
                    "the larger circle must carry the trace-armed ring colour in its stroke"
                );
                assert_eq!(
                    outer.fill,
                    egui::Color32::TRANSPARENT,
                    "the outer ring circle must have a fully transparent fill"
                );
                assert!(
                    outer.radius >= inner.radius + SELECTED_RING_WIDTH,
                    "the outer ring's radius must exceed the inner ring's by at least \
                     SELECTED_RING_WIDTH, inner {} outer {}",
                    inner.radius,
                    outer.radius
                );
                assert_eq!(
                    inner.center, outer.center,
                    "both rings must share the same centre"
                );

                // Jump-only: ONE circle, stroked blue.
                let mut jump_only_shape = node_display_with_flags(
                    &model_for_closure,
                    &focus_for_closure,
                    "a1",
                    true,
                    false,
                );
                let jump_only_shapes = jump_only_shape.shapes(&draw_ctx);
                let jump_only_circles = circles_of(&jump_only_shapes);
                assert_eq!(
                    jump_only_circles.len(),
                    1,
                    "jump-only must paint exactly one circle, got {}",
                    jump_only_circles.len()
                );
                assert_eq!(
                    jump_only_circles[0].stroke.color,
                    jump_ring_color(),
                    "jump-only circle must be stroked with the jump ring colour"
                );

                // Neither: ONE circle with no ring stroke.
                let mut neither_shape = node_display_with_flags(
                    &model_for_closure,
                    &focus_for_closure,
                    "a1",
                    false,
                    false,
                );
                let neither_shapes = neither_shape.shapes(&draw_ctx);
                let neither_circles = circles_of(&neither_shapes);
                assert_eq!(
                    neither_circles.len(),
                    1,
                    "neither must paint exactly one circle, got {}",
                    neither_circles.len()
                );
                assert_eq!(
                    neither_circles[0].stroke,
                    egui::Stroke::NONE,
                    "neither circle must carry no ring stroke"
                );

                *ran_inner.borrow_mut() = true;
            },
            app,
        );
        harness.step();

        assert!(
            *ran.borrow(),
            "the render closure must have run at least once"
        );
    }

    /// The core geometry fix (quick-260927-rmx, fix 1). At each zoom the
    /// gap between the node's own circle and the trace-armed outer ring is
    /// MEASURED (never recomputed via `canvas_to_screen_size`) from a real
    /// `SeamNodeShape::shapes()` return value, for a node with BOTH flags
    /// set. Asserts two independent things: (a) the raw gap is NOT a fixed
    /// screen size -- it must strictly increase with zoom, by more than a
    /// factor of ten from `MIN_ZOOM` to `MAX_ZOOM`; and (b) the proportion
    /// `gap / inner.radius` IS constant across all three zooms (within
    /// `1e-3`), and that constant is `1.0` -- exactly
    /// `TRACE_RING_OFFSET / NODE_RADIUS` (both 6.0), not a tuned magic
    /// number. RED today on both halves: every gap measures a flat 6.0
    /// regardless of zoom, and the three ratios come out 10.0 / 1.0 / 0.1
    /// -- the quantified form of the reported defect.
    #[test]
    fn the_trace_ring_gap_scales_with_zoom_instead_of_holding_a_fixed_pixel_size() {
        let outcome =
            crate::load::read_and_ingest(CLEAN_FIXTURE).expect("fixture must ingest cleanly");
        let model = outcome.model;
        let focus = focus_state();

        let ran: std::rc::Rc<std::cell::RefCell<bool>> =
            std::rc::Rc::new(std::cell::RefCell::new(false));
        let ran_inner = ran.clone();
        let model_for_closure = model.clone();
        let focus_for_closure = focus.clone();

        let app = crate::app::SeamExplorerApp::default();
        let mut harness = egui_kittest::Harness::new_ui_state(
            move |ui, _app: &mut crate::app::SeamExplorerApp| {
                let style = egui_graphs::SettingsStyle::default();
                let mut gaps = Vec::with_capacity(3);
                let mut ratios = Vec::with_capacity(3);

                for &zoom in &[MIN_ZOOM, 1.0, MAX_ZOOM] {
                    let mut meta = egui_graphs::MetadataFrame::default();
                    meta.zoom = zoom;
                    let draw_ctx = egui_graphs::DrawContext {
                        ctx: ui.ctx(),
                        painter: ui.painter(),
                        style: &style,
                        is_directed: true,
                        meta: &meta,
                    };

                    let mut shape = node_display_with_flags(
                        &model_for_closure,
                        &focus_for_closure,
                        "a1",
                        true,
                        true,
                    );
                    let shapes = shape.shapes(&draw_ctx);
                    let circles = circles_of(&shapes);
                    assert_eq!(
                        circles.len(),
                        2,
                        "a jump-selected AND armed node must paint exactly two circles at \
                         zoom {zoom}, got {}",
                        circles.len()
                    );
                    let (inner, outer) = if circles[0].radius <= circles[1].radius {
                        (circles[0], circles[1])
                    } else {
                        (circles[1], circles[0])
                    };
                    let gap = outer.radius - inner.radius;
                    gaps.push(gap);
                    ratios.push(gap / inner.radius);
                }

                // (a) the gap is not a fixed screen size: strictly
                // increasing, and the extremes differ by more than 10x.
                assert!(
                    gaps[0] < gaps[1] && gaps[1] < gaps[2],
                    "the gap must strictly increase with zoom, measured {gaps:?}"
                );
                assert!(
                    gaps[2] / gaps[0] > 10.0,
                    "the largest gap must exceed the smallest by more than a factor of \
                     ten, measured smallest {} largest {} (ratio {})",
                    gaps[0],
                    gaps[2],
                    gaps[2] / gaps[0]
                );

                // (b) the gap-to-radius proportion IS constant, and it is
                // exactly TRACE_RING_OFFSET / NODE_RADIUS (both 6.0) == 1.0.
                for pair in ratios.windows(2) {
                    assert!(
                        (pair[0] - pair[1]).abs() < 1e-3,
                        "gap/radius ratios must agree with each other to within 1e-3, \
                         measured {ratios:?}"
                    );
                }
                for ratio in &ratios {
                    assert!(
                        (ratio - 1.0).abs() < 1e-3,
                        "the shared gap/radius ratio must be 1.0 \
                         (TRACE_RING_OFFSET / NODE_RADIUS, both 6.0), measured {ratio}"
                    );
                }

                *ran_inner.borrow_mut() = true;
            },
            app,
        );
        harness.step();

        assert!(
            *ran.borrow(),
            "the render closure must have run at least once"
        );
    }

    /// The hug fix (quick-260927-rmx, fix 2): a trace-armed node with NO
    /// jump ring paints exactly ONE circle, hugging the node at its own
    /// base radius, rather than a detached outer ring. At each zoom,
    /// asserts: exactly one circle; its stroke colour is
    /// `trace_armed_ring_color()`; its stroke width is
    /// `SELECTED_RING_WIDTH`; and its radius equals -- exactly, measurement
    /// against measurement, nothing recomputed -- the radius of the SAME
    /// node's plain unringed circle (both flags false) at the SAME zoom.
    /// RED today: two circles, and the node circle itself carries
    /// `Stroke::NONE`.
    #[test]
    fn an_armed_node_with_no_jump_ring_hugs_the_node() {
        let outcome =
            crate::load::read_and_ingest(CLEAN_FIXTURE).expect("fixture must ingest cleanly");
        let model = outcome.model;
        let focus = focus_state();

        let ran: std::rc::Rc<std::cell::RefCell<bool>> =
            std::rc::Rc::new(std::cell::RefCell::new(false));
        let ran_inner = ran.clone();
        let model_for_closure = model.clone();
        let focus_for_closure = focus.clone();

        let app = crate::app::SeamExplorerApp::default();
        let mut harness = egui_kittest::Harness::new_ui_state(
            move |ui, _app: &mut crate::app::SeamExplorerApp| {
                let style = egui_graphs::SettingsStyle::default();

                for &zoom in &[MIN_ZOOM, 1.0, MAX_ZOOM] {
                    let mut meta = egui_graphs::MetadataFrame::default();
                    meta.zoom = zoom;
                    let draw_ctx = egui_graphs::DrawContext {
                        ctx: ui.ctx(),
                        painter: ui.painter(),
                        style: &style,
                        is_directed: true,
                        meta: &meta,
                    };

                    let mut armed_only = node_display_with_flags(
                        &model_for_closure,
                        &focus_for_closure,
                        "a1",
                        false,
                        true,
                    );
                    let armed_shapes = armed_only.shapes(&draw_ctx);
                    let armed_circles = circles_of(&armed_shapes);
                    assert_eq!(
                        armed_circles.len(),
                        1,
                        "an armed-only node must paint exactly one circle at zoom {zoom}, \
                         got {}",
                        armed_circles.len()
                    );
                    assert_eq!(
                        armed_circles[0].stroke.color,
                        trace_armed_ring_color(),
                        "the armed-only circle's stroke must be the trace-armed colour at \
                         zoom {zoom}"
                    );
                    assert_eq!(
                        armed_circles[0].stroke.width, SELECTED_RING_WIDTH,
                        "the armed-only circle's stroke width must be SELECTED_RING_WIDTH \
                         at zoom {zoom}"
                    );

                    let mut neither = node_display_with_flags(
                        &model_for_closure,
                        &focus_for_closure,
                        "a1",
                        false,
                        false,
                    );
                    let neither_shapes = neither.shapes(&draw_ctx);
                    let neither_circles = circles_of(&neither_shapes);
                    assert_eq!(
                        neither_circles.len(),
                        1,
                        "the same node with neither flag must paint exactly one circle at \
                         zoom {zoom}, got {}",
                        neither_circles.len()
                    );
                    assert_eq!(
                        armed_circles[0].radius, neither_circles[0].radius,
                        "the armed-only circle's radius must equal the plain unringed \
                         circle's radius at zoom {zoom} (measurement against measurement)"
                    );
                }

                *ran_inner.borrow_mut() = true;
            },
            app,
        );
        harness.step();

        assert!(
            *ran.borrow(),
            "the render closure must have run at least once"
        );
    }

    /// GUARD, not a red test -- the non-regression case this task must not
    /// break. At zoom `1.0` and at `MIN_ZOOM`, with both flags set: exactly
    /// two circles; the smaller stroked `jump_ring_color()`; the larger
    /// stroked `trace_armed_ring_color()` with a fully transparent fill;
    /// `outer.radius > inner.radius` strictly; both share a centre.
    /// Deliberately does NOT carry over the existing test's
    /// `outer.radius >= inner.radius + SELECTED_RING_WIDTH` assertion --
    /// that comparison mixes a scaled quantity with a raw screen-point one
    /// and is false at `MIN_ZOOM` by design (discovery finding 8). This
    /// passes both before and after the fix; it is here to prove the fix
    /// does not regress the both-rings case, not to prove the bug existed.
    #[test]
    fn both_rings_still_stack_red_outside_blue() {
        let outcome =
            crate::load::read_and_ingest(CLEAN_FIXTURE).expect("fixture must ingest cleanly");
        let model = outcome.model;
        let focus = focus_state();

        let ran: std::rc::Rc<std::cell::RefCell<bool>> =
            std::rc::Rc::new(std::cell::RefCell::new(false));
        let ran_inner = ran.clone();
        let model_for_closure = model.clone();
        let focus_for_closure = focus.clone();

        let app = crate::app::SeamExplorerApp::default();
        let mut harness = egui_kittest::Harness::new_ui_state(
            move |ui, _app: &mut crate::app::SeamExplorerApp| {
                let style = egui_graphs::SettingsStyle::default();

                for &zoom in &[MIN_ZOOM, 1.0] {
                    let mut meta = egui_graphs::MetadataFrame::default();
                    meta.zoom = zoom;
                    let draw_ctx = egui_graphs::DrawContext {
                        ctx: ui.ctx(),
                        painter: ui.painter(),
                        style: &style,
                        is_directed: true,
                        meta: &meta,
                    };

                    let mut both = node_display_with_flags(
                        &model_for_closure,
                        &focus_for_closure,
                        "a1",
                        true,
                        true,
                    );
                    let shapes = both.shapes(&draw_ctx);
                    let circles = circles_of(&shapes);
                    assert_eq!(
                        circles.len(),
                        2,
                        "both flags set must paint exactly two circles at zoom {zoom}, got {}",
                        circles.len()
                    );
                    let (inner, outer) = if circles[0].radius <= circles[1].radius {
                        (circles[0], circles[1])
                    } else {
                        (circles[1], circles[0])
                    };
                    assert_eq!(
                        inner.stroke.color,
                        jump_ring_color(),
                        "the smaller circle must be stroked with the jump ring colour at \
                         zoom {zoom}"
                    );
                    assert_eq!(
                        outer.stroke.color,
                        trace_armed_ring_color(),
                        "the larger circle must be stroked with the trace-armed colour at \
                         zoom {zoom}"
                    );
                    assert_eq!(
                        outer.fill,
                        egui::Color32::TRANSPARENT,
                        "the outer ring's fill must be fully transparent at zoom {zoom}"
                    );
                    assert!(
                        outer.radius > inner.radius,
                        "the outer ring's radius must strictly exceed the inner ring's at \
                         zoom {zoom}, inner {} outer {}",
                        inner.radius,
                        outer.radius
                    );
                    assert_eq!(
                        inner.center, outer.center,
                        "both rings must share the same centre at zoom {zoom}"
                    );
                }

                *ran_inner.borrow_mut() = true;
            },
            app,
        );
        harness.step();

        assert!(
            *ran.borrow(),
            "the render closure must have run at least once"
        );
    }

    /// GUARD, not a red test -- the blue jump ring was never the thing with
    /// a bug and must be proven unmoved by this task. For each zoom in
    /// `[MIN_ZOOM, 1.0, MAX_ZOOM]`, measures the node-circle radius (the
    /// smallest circle in the paint list, since it is always the node's own
    /// circle regardless of how many rings are present) across all four
    /// flag states and asserts all four are equal at that zoom, and that in
    /// the jump-only and both states that circle is stroked
    /// `jump_ring_color()`. Passes today and must keep passing after both
    /// fixes.
    #[test]
    fn the_jump_ring_never_moves_at_any_zoom() {
        let outcome =
            crate::load::read_and_ingest(CLEAN_FIXTURE).expect("fixture must ingest cleanly");
        let model = outcome.model;
        let focus = focus_state();

        let ran: std::rc::Rc<std::cell::RefCell<bool>> =
            std::rc::Rc::new(std::cell::RefCell::new(false));
        let ran_inner = ran.clone();
        let model_for_closure = model.clone();
        let focus_for_closure = focus.clone();

        let app = crate::app::SeamExplorerApp::default();
        let mut harness = egui_kittest::Harness::new_ui_state(
            move |ui, _app: &mut crate::app::SeamExplorerApp| {
                let style = egui_graphs::SettingsStyle::default();

                for &zoom in &[MIN_ZOOM, 1.0, MAX_ZOOM] {
                    let mut meta = egui_graphs::MetadataFrame::default();
                    meta.zoom = zoom;
                    let draw_ctx = egui_graphs::DrawContext {
                        ctx: ui.ctx(),
                        painter: ui.painter(),
                        style: &style,
                        is_directed: true,
                        meta: &meta,
                    };

                    let smallest_circle_radius =
                        |is_jump_selected: bool, is_trace_armed: bool| -> (f32, egui::Color32) {
                            let mut shape = node_display_with_flags(
                                &model_for_closure,
                                &focus_for_closure,
                                "a1",
                                is_jump_selected,
                                is_trace_armed,
                            );
                            let shapes = shape.shapes(&draw_ctx);
                            let circles = circles_of(&shapes);
                            let smallest = circles
                                .iter()
                                .min_by(|a, b| a.radius.partial_cmp(&b.radius).unwrap())
                                .expect("at least one circle must always be painted");
                            (smallest.radius, smallest.stroke.color)
                        };

                    let (neither_radius, _) = smallest_circle_radius(false, false);
                    let (jump_only_radius, jump_only_color) = smallest_circle_radius(true, false);
                    let (armed_only_radius, _) = smallest_circle_radius(false, true);
                    let (both_radius, both_color) = smallest_circle_radius(true, true);

                    assert_eq!(
                        neither_radius, jump_only_radius,
                        "the node circle's radius must be identical between neither and \
                         jump-only at zoom {zoom}"
                    );
                    assert_eq!(
                        neither_radius, armed_only_radius,
                        "the node circle's radius must be identical between neither and \
                         armed-only at zoom {zoom}"
                    );
                    assert_eq!(
                        neither_radius, both_radius,
                        "the node circle's radius must be identical between neither and \
                         both at zoom {zoom}"
                    );
                    assert_eq!(
                        jump_only_color,
                        jump_ring_color(),
                        "the node circle must be stroked with the jump ring colour in the \
                         jump-only state at zoom {zoom}"
                    );
                    assert_eq!(
                        both_color,
                        jump_ring_color(),
                        "the node circle must be stroked with the jump ring colour in the \
                         both state at zoom {zoom}"
                    );
                }

                *ran_inner.borrow_mut() = true;
            },
            app,
        );
        harness.step();

        assert!(
            *ran.borrow(),
            "the render closure must have run at least once"
        );
    }

    /// Pure table over the new `toggle_selection`. RED: the function is
    /// absent in this RED step.
    #[test]
    fn toggle_selection_clears_a_repeat_click_and_selects_anything_else() {
        assert_eq!(
            toggle_selection(Some("a1"), "a1"),
            None,
            "clicking the already-selected node must clear the selection"
        );
        assert_eq!(
            toggle_selection(Some("a1"), "b1"),
            Some("b1".to_string()),
            "clicking a different node must select it"
        );
        assert_eq!(
            toggle_selection(None, "a1"),
            Some("a1".to_string()),
            "clicking a node with nothing selected must select it"
        );
    }

    /// Live: `jump_to_node` called twice for the same rendered node id must
    /// select then clear, without re-framing the canvas on the toggle-off.
    /// RED today: the second call still leaves `app.selected_node ==
    /// Some("a1")` because `jump_to_node` does not toggle yet.
    #[test]
    fn clicking_a_selected_node_a_second_time_clears_it() {
        let outcome =
            crate::load::read_and_ingest(CLEAN_FIXTURE).expect("fixture must ingest cleanly");
        let app = crate::app::SeamExplorerApp {
            model: Some(outcome.model),
            seams: outcome.seams,
            ..Default::default()
        };

        let ran: std::rc::Rc<std::cell::RefCell<bool>> =
            std::rc::Rc::new(std::cell::RefCell::new(false));
        let ran_inner = ran.clone();

        let mut harness = egui_kittest::Harness::new_ui_state(
            move |ui, app: &mut crate::app::SeamExplorerApp| {
                show(ui, app);

                let first = jump_to_node(ui, app, "a1");
                assert!(first, "the first click on a real rendered node must jump");
                assert_eq!(
                    app.selected_node.as_deref(),
                    Some("a1"),
                    "after the first click, a1 must be selected"
                );
                let view_after_first = app.view;

                let second = jump_to_node(ui, app, "a1");
                assert!(
                    second,
                    "the second (toggle-off) click on the same node must report a state change"
                );
                assert!(
                    app.selected_node.is_none(),
                    "after the second click on the same node, the selection must clear"
                );
                assert_eq!(
                    app.view.zoom, view_after_first.zoom,
                    "a toggle-off must not re-frame the canvas (zoom changed)"
                );
                assert_eq!(
                    app.view.pan, view_after_first.pan,
                    "a toggle-off must not re-frame the canvas (pan changed)"
                );

                *ran_inner.borrow_mut() = true;
            },
            app,
        );
        harness.step();

        assert!(
            *ran.borrow(),
            "the render closure must have run at least once"
        );
    }
}
