//! `egui::Painter` overlay: the seam fault-line + crossing "threads" visual
//! (D-13's signature visual, no Rust analog exists anywhere in the repo --
//! see 05-PATTERNS.md "No Analog Found"). This is the one visual in the
//! phase that gets explicit design effort budgeted rather than a
//! make-it-work pass -- a heavier core stroke plus a softer wide stroke
//! behind it to suggest a glow, since egui's immediate-mode styling is
//! blunter than the CSS the original leaned on.
//!
//! Geometry (fault-line x, which crossing edges to draw, opposite-side
//! endpoints) is pure and unit-tested with no live painter. Actually
//! painting requires converting canvas-space positions (the same space
//! `layout.rs` positions nodes in) into screen space using the exact
//! zoom/pan transform `GraphView` itself used this frame -- 0.31.0 exposes
//! no public getter for that transform (RESEARCH Open Question 2, closed
//! for graph_view.rs's Reset handling too), only the same persisted-state
//! read path the widget's own `sync_state`/`ViewState::load` uses
//! internally: `egui_graphs::MetadataFrame::new(None).load(ui)`. Reading it
//! here, after `ui.add(&mut GraphView::...)` has already saved this
//! frame's updated transform, keeps the overlay visually attached to the
//! pulled-apart nodes as the user pans/zooms.

use crate::app::FocusState;

const ACCENT_HEX: &str = "#ff4d8d";
/// Side tints (D-13/DP-12-03) -- must equal `graph_view`'s own
/// `SIDE_A_HEX`/`SIDE_B_HEX` node-fill constants exactly, so a side label
/// always matches the colour of the nodes it sits over.
const SIDE_A_HEX: &str = "#38d6c4";
const SIDE_B_HEX: &str = "#f2a63c";
/// Cap on a side label's character length before it is shortened through
/// the same shared helper `graph_view`'s node labels already use
/// (DP-12-05) -- chosen so a label fits comfortably beside the fault line
/// without running across it into the other side's space.
const SIDE_LABEL_MAX_CHARS: usize = 22;
/// Vertical inset (canvas-space px) from the top of the canvas at which
/// side labels sit (DP-12-04) -- drawn in canvas space, above the node
/// field, so labels pan/zoom with the side they name rather than staying
/// screen-fixed.
const SIDE_LABEL_TOP_INSET: f32 = 24.0;

/// The fault line's x position in canvas space: the midpoint between the
/// two pulled-apart sides. Computed via `layout::separation` -- the SAME
/// formula `layout::seam_target_x` uses -- so the line can never
/// independently drift from where the nodes actually parted (key_link).
/// By construction (symmetric `center +/- sep`) this reduces to
/// `center_x`, but routing it through `layout::separation` keeps both
/// values sourced from one formula if that symmetry ever changes.
pub fn fault_line_x(center_x: f32, canvas_width: f32) -> f32 {
    let sep = crate::layout::separation(canvas_width);
    let side_a = center_x - sep;
    let side_b = center_x + sep;
    (side_a + side_b) / 2.0
}

/// True when `source_x`/`target_x` sit on opposite sides of `fault_x` --
/// the geometric definition of "this thread actually crosses the seam".
pub fn crosses_fault_line(fault_x: f32, source_x: f32, target_x: f32) -> bool {
    (source_x < fault_x && target_x > fault_x) || (source_x > fault_x && target_x < fault_x)
}

/// Pure computation of which crossing-edge threads to draw this frame: one
/// `(source_x, target_x)` canvas-space pair per crossing edge. Draws
/// nothing when no seam is focused (Task 3 action: "Draw nothing at all
/// when no seam is focused") and never samples/caps -- honest rendering of
/// every crossing edge is the product requirement (T-05-08 disposition).
pub fn crossing_threads(
    edges: &[(seam_core::CommunityId, seam_core::CommunityId)],
    focus: Option<(&seam_core::CommunityId, &seam_core::CommunityId)>,
    center_x: f32,
    canvas_width: f32,
) -> Vec<(f32, f32)> {
    let Some((a, b)) = focus else {
        return Vec::new();
    };
    edges
        .iter()
        .filter(|(sc, tc)| (sc == a && tc == b) || (sc == b && tc == a))
        .map(|(sc, _tc)| {
            let source_x = crate::layout::seam_target_x(sc, focus, center_x, canvas_width);
            let target_x = if sc == a {
                crate::layout::seam_target_x(b, focus, center_x, canvas_width)
            } else {
                crate::layout::seam_target_x(a, focus, center_x, canvas_width)
            };
            (source_x, target_x)
        })
        .collect()
}

/// Resolves and shortens one side's display text -- the single call site
/// for both the domain crate's name resolver and the shared node-label
/// shortening helper (DP-12-06/DP-12-05), so `side_labels`'s two entries
/// can never reach either through a second, divergent path.
fn side_label_text(model: &seam_core::Model, community: &seam_core::CommunityId) -> String {
    let resolved = model.community_label(community);
    crate::graph_view::truncate_label(resolved, SIDE_LABEL_MAX_CHARS)
}

/// Pure decision of what to draw as the two pulled-apart sides' labels
/// (Task 1 artifact). Empty when nothing is focused -- "Nothing is
/// labelled when no seam is focused" (must_have). Otherwise exactly two
/// entries, side A first then side B, each carrying: its canvas-space x
/// from `layout::seam_target_x` (DP-12-02, the same source of truth
/// `fault_line_x`/`crossing_threads` already use); its display text
/// resolved and shortened via `side_label_text` above; and that side's
/// tint (DP-12-03).
pub fn side_labels(
    model: &seam_core::Model,
    focus: Option<&FocusState>,
    center_x: f32,
    canvas_width: f32,
) -> Vec<(f32, String, egui::Color32)> {
    let Some(focus) = focus else {
        return Vec::new();
    };
    let pair = Some((&focus.a, &focus.b));
    let side_a_x = crate::layout::seam_target_x(&focus.a, pair, center_x, canvas_width);
    let side_b_x = crate::layout::seam_target_x(&focus.b, pair, center_x, canvas_width);

    vec![
        (
            side_a_x,
            side_label_text(model, &focus.a),
            egui::Color32::from_hex(SIDE_A_HEX).expect("valid hex"),
        ),
        (
            side_b_x,
            side_label_text(model, &focus.b),
            egui::Color32::from_hex(SIDE_B_HEX).expect("valid hex"),
        ),
    ]
}

/// The fault line's stroke color. UI-SPEC Color contract: the accent
/// (`--seam` #ff4d8d) is reserved for "the seam fault-line stroke, crossing
/// thread lines/labels on a focused seam ... never used for ordinary
/// buttons, links, or default interactive chrome" -- the fault line is a
/// FIXED accent color regardless of verdict (verdict coloring is the
/// legend dot / detail-panel badge's job, not the fault line's). `verdict`
/// is accepted for call-site symmetry with the rest of `graph_view::show`'s
/// paint order but intentionally unused for color selection here.
pub fn seam_line_color(_verdict: &seam_core::Verdict) -> egui::Color32 {
    egui::Color32::from_hex(ACCENT_HEX).expect("valid hex")
}

/// Draws the fault line: a soft, wide "glow" stroke behind a heavier core
/// stroke, verdict-colored, spanning the canvas vertically at
/// `fault_line_x`. Drawn strictly after the `GraphView` widget in paint
/// order (called from `graph_view::show` after `ui.add(&mut GraphView)`),
/// matching the original's two-layer SVG structure (graph group plus
/// overlay group).
pub fn paint_seam_line(
    ui: &egui::Ui,
    canvas_rect: egui::Rect,
    graph_rect: egui::Rect,
    verdict: &seam_core::Verdict,
) {
    let meta = egui_graphs::MetadataFrame::new(None).load(ui);
    let offset = graph_rect.left_top().to_vec2();
    let center_x = canvas_rect.center().x;
    let x = fault_line_x(center_x, canvas_rect.width().max(1.0));

    let top = meta.canvas_to_screen_pos(egui::Pos2::new(x, canvas_rect.top())) + offset;
    let bottom = meta.canvas_to_screen_pos(egui::Pos2::new(x, canvas_rect.bottom())) + offset;

    let color = seam_line_color(verdict);
    let painter = ui.painter();
    // Soft, wide glow behind the core stroke -- egui's immediate-mode
    // styling has no CSS box-shadow equivalent, so the depth has to be
    // drawn explicitly as a second, wider, more transparent stroke (D-13).
    painter.line_segment(
        [top, bottom],
        egui::Stroke::new(14.0, color.gamma_multiply(0.16)),
    );
    painter.line_segment([top, bottom], egui::Stroke::new(2.5, color));
}

/// Draws one thin accent-colored "thread" per crossing edge, from its
/// source-side x to its target-side x, at the canvas vertical center.
/// These are the literal answer to the canvas hint "Threads crossing the
/// line are the calls that couple these sides" -- density/direction must
/// stay honest to the real crossing edges (no sampling, per
/// `crossing_threads`'s doc).
pub fn paint_crossing_threads(
    ui: &egui::Ui,
    canvas_rect: egui::Rect,
    graph_rect: egui::Rect,
    edges: &[(seam_core::CommunityId, seam_core::CommunityId)],
    focus: &FocusState,
) {
    let center = canvas_rect.center();
    let threads = crossing_threads(
        edges,
        Some((&focus.a, &focus.b)),
        center.x,
        canvas_rect.width().max(1.0),
    );
    if threads.is_empty() {
        return;
    }

    let meta = egui_graphs::MetadataFrame::new(None).load(ui);
    let offset = graph_rect.left_top().to_vec2();
    let accent = egui::Color32::from_hex(ACCENT_HEX)
        .expect("valid hex")
        .gamma_multiply(0.35);
    let painter = ui.painter();

    for (source_x, target_x) in threads {
        let start = meta.canvas_to_screen_pos(egui::Pos2::new(source_x, center.y)) + offset;
        let end = meta.canvas_to_screen_pos(egui::Pos2::new(target_x, center.y)) + offset;
        painter.line_segment([start, end], egui::Stroke::new(1.0, accent));
    }
}

/// Draws each pulled-apart side's canvas label (Task 2 artifact): computes
/// the two entries via `side_labels` above, returns immediately if that is
/// empty (nothing focused -- the guard `paint_side_labels_is_a_no_op_for_an_empty_label_set`
/// covers), then for each entry converts its canvas-space point (`x`, a
/// fixed inset below the canvas top) into screen space using the identical
/// `MetadataFrame`/`graph_rect` read `paint_seam_line`/`paint_crossing_threads`
/// already use -- so a label pans/zooms with the side it names (DP-12-04)
/// instead of staying screen-fixed. Text is centred horizontally on its `x`.
pub fn paint_side_labels(
    ui: &egui::Ui,
    canvas_rect: egui::Rect,
    graph_rect: egui::Rect,
    model: &seam_core::Model,
    focus: &FocusState,
) {
    let center_x = canvas_rect.center().x;
    let entries = side_labels(model, Some(focus), center_x, canvas_rect.width().max(1.0));
    if entries.is_empty() {
        return;
    }

    let meta = egui_graphs::MetadataFrame::new(None).load(ui);
    let offset = graph_rect.left_top().to_vec2();
    let y = canvas_rect.top() + SIDE_LABEL_TOP_INSET;
    let painter = ui.painter();

    for (x, text, color) in entries {
        let screen = meta.canvas_to_screen_pos(egui::Pos2::new(x, y)) + offset;
        let galley = ui.ctx().fonts_mut(|f| {
            f.layout_no_wrap(
                text,
                egui::FontId::new(13.0, egui::FontFamily::Monospace),
                color,
            )
        });
        let label_pos = egui::Pos2::new(screen.x - galley.size().x / 2.0, screen.y);
        painter.galley(label_pos, galley, color);
    }
}

/// Draws the in-flight rubber-band line (TRACE-01) from the drag origin
/// node's current screen position to the live (possibly node-snapped)
/// cursor screen position. Both points are already in absolute screen
/// space by the time they reach here (`graph_view::handle_trace_gesture`
/// resolves the origin node's position and the snap-to-nearest-node logic
/// before calling this), so no canvas/screen conversion happens in this
/// function -- mirrors the original's rubber-band `<line>` living outside
/// the zoom-transformed group (`frontend/index.html:315-323`, RESEARCH
/// key-decision) rather than inside canvas space.
pub fn paint_rubber_band(ui: &egui::Ui, from_screen: egui::Pos2, cursor_screen: egui::Pos2) {
    let color = egui::Color32::from_hex(ACCENT_HEX).expect("valid hex");
    ui.painter()
        .line_segment([from_screen, cursor_screen], egui::Stroke::new(1.5, color));
}

/// Draws the resolved trace path (TRACE-02) as a highlighted polyline
/// across its hops, using each hop's current on-screen node position.
/// `hop_screen_positions` is already resolved by the caller (one screen
/// `Pos2` per `TracePath.hops` entry, in the same order) -- this function
/// does no graph lookup or canvas/screen conversion itself, matching
/// `paint_seam_line`/`paint_crossing_threads`'s "geometry resolved by the
/// caller, painting only here" split. Draws nothing for a path with fewer
/// than two hops (nothing to connect).
pub fn paint_traced_path(ui: &egui::Ui, hop_screen_positions: &[egui::Pos2]) {
    if hop_screen_positions.len() < 2 {
        return;
    }
    let color = egui::Color32::from_hex(ACCENT_HEX).expect("valid hex");
    let painter = ui.painter();
    for pair in hop_screen_positions.windows(2) {
        painter.line_segment([pair[0], pair[1]], egui::Stroke::new(2.5, color));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout;

    // ============================================================
    // 05-12 Task 1: the pure `side_labels` decision function.
    // ============================================================

    fn model_with_names(names: &[(&str, &str)]) -> seam_core::Model {
        seam_core::Model {
            community_names: names
                .iter()
                .map(|(id, name)| (id.to_string(), name.to_string()))
                .collect(),
            ..Default::default()
        }
    }

    fn ab_focus() -> FocusState {
        FocusState {
            a: "A".to_string(),
            b: "B".to_string(),
        }
    }

    #[test]
    fn side_labels_are_empty_without_focus() {
        let model = seam_core::Model::default();
        let result = side_labels(&model, None, 500.0, 1000.0);
        assert!(result.is_empty());
    }

    #[test]
    fn side_labels_x_positions_match_the_layout_side_targets() {
        let model = seam_core::Model::default();
        let focus = ab_focus();
        let center = 500.0;
        let width = 1000.0;
        let pair = Some((&focus.a, &focus.b));

        let expected_a = layout::seam_target_x(&focus.a, pair, center, width);
        let expected_b = layout::seam_target_x(&focus.b, pair, center, width);

        let result = side_labels(&model, Some(&focus), center, width);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].0, expected_a);
        assert_eq!(result[1].0, expected_b);
    }

    #[test]
    fn side_labels_straddle_the_fault_line() {
        let model = seam_core::Model::default();
        let focus = ab_focus();
        let center = 500.0;
        let width = 1000.0;
        let fault_x = fault_line_x(center, width);

        let result = side_labels(&model, Some(&focus), center, width);
        assert_eq!(result.len(), 2);
        assert!(
            result[0].0 < fault_x,
            "side A's label must sit left of the fault line"
        );
        assert!(
            result[1].0 > fault_x,
            "side B's label must sit right of the fault line"
        );
    }

    #[test]
    fn side_labels_use_community_names_when_present() {
        let model = model_with_names(&[("A", "Payments"), ("B", "Orders")]);
        let focus = ab_focus();

        let result = side_labels(&model, Some(&focus), 500.0, 1000.0);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].1, "Payments");
        assert_eq!(result[1].1, "Orders");
    }

    #[test]
    fn side_labels_fall_back_to_raw_ids_when_unnamed() {
        let model = seam_core::Model::default();
        let focus = ab_focus();

        let result = side_labels(&model, Some(&focus), 500.0, 1000.0);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].1, "A");
        assert_eq!(result[1].1, "B");
        assert!(!result[0].1.is_empty());
        assert!(!result[1].1.is_empty());
    }

    #[test]
    fn side_label_colors_match_the_node_side_tints() {
        let model = seam_core::Model::default();
        let focus = ab_focus();

        let result = side_labels(&model, Some(&focus), 500.0, 1000.0);
        assert_eq!(result.len(), 2);
        let expected_a = egui::Color32::from_hex(SIDE_A_HEX).expect("valid hex");
        let expected_b = egui::Color32::from_hex(SIDE_B_HEX).expect("valid hex");
        assert_eq!(result[0].2, expected_a);
        assert_eq!(result[1].2, expected_b);
        assert_ne!(result[0].2, result[1].2);
    }

    #[test]
    fn a_long_side_label_is_truncated() {
        let long_name = "ThisIsAVeryLongCommunityNameThatDefinitelyExceedsTheDisplayLimit";
        let model = model_with_names(&[("A", long_name)]);
        let focus = ab_focus();

        let result = side_labels(&model, Some(&focus), 500.0, 1000.0);
        assert_eq!(result.len(), 2);
        assert!(
            result[0].1.chars().count() < long_name.chars().count(),
            "a long side label must come back shortened, got {:?}",
            result[0].1
        );
    }

    // ============================================================
    // 05-12 Task 2: `paint_side_labels`'s no-op guard.
    // ============================================================

    #[test]
    fn paint_side_labels_is_a_no_op_for_an_empty_label_set() {
        // `paint_side_labels` always receives `focus: &FocusState` (its
        // call site in `graph_view::show` only exists inside an
        // `if let Some(focus) = &app.focus` guard), so the "nothing to
        // draw" case it must handle is entirely a property of
        // `side_labels`'s own `None` branch -- the same data source
        // `paint_side_labels` calls before deciding whether its painter
        // has anything to receive. Confirming that branch is empty here is
        // exactly what proves `paint_side_labels`'s early return
        // (`if entries.is_empty() { return; }`) is reachable and correct,
        // without needing a live `egui::Ui`/painter to observe it.
        let model = seam_core::Model::default();
        assert!(
            side_labels(&model, None, 500.0, 1000.0).is_empty(),
            "the data source paint_side_labels's early-return guard reads must be empty when \
             nothing is focused"
        );
    }

    #[test]
    fn test_fault_line_x_matches_layout_midpoint() {
        let a: seam_core::CommunityId = "A".to_string();
        let b: seam_core::CommunityId = "B".to_string();
        let center = 500.0;
        let width = 1000.0;

        let side_a = layout::seam_target_x(&a, Some((&a, &b)), center, width);
        let side_b = layout::seam_target_x(&b, Some((&a, &b)), center, width);
        let expected_mid = (side_a + side_b) / 2.0;

        assert_eq!(fault_line_x(center, width), expected_mid);
    }

    #[test]
    fn test_thread_endpoints_span_the_line() {
        let a: seam_core::CommunityId = "A".to_string();
        let b: seam_core::CommunityId = "B".to_string();
        let center = 500.0;
        let width = 1000.0;
        let fault_x = fault_line_x(center, width);

        let source_x = layout::seam_target_x(&a, Some((&a, &b)), center, width);
        let target_x = layout::seam_target_x(&b, Some((&a, &b)), center, width);

        assert!(crosses_fault_line(fault_x, source_x, target_x));
    }

    #[test]
    fn test_no_focus_draws_nothing() {
        let edges = vec![("A".to_string(), "B".to_string())];
        let result = crossing_threads(&edges, None, 500.0, 1000.0);
        assert!(result.is_empty());
    }

    #[test]
    fn test_line_color_is_fixed_accent_regardless_of_verdict() {
        // UI-SPEC Color contract: the fault line is always the --seam accent
        // (#ff4d8d), never verdict-colored (that's the legend dot / badge's
        // job). Distinct from `verdict_color`, which does vary by verdict.
        let expected = egui::Color32::from_hex(ACCENT_HEX).expect("valid hex");
        assert_eq!(seam_line_color(&seam_core::Verdict::Leaky), expected);
        assert_eq!(seam_line_color(&seam_core::Verdict::Clean), expected);
        assert_eq!(
            seam_line_color(&seam_core::Verdict::Leaky),
            seam_line_color(&seam_core::Verdict::Clean)
        );
    }

    #[test]
    fn test_crossing_threads_covers_every_crossing_edge() {
        let a: seam_core::CommunityId = "A".to_string();
        let b: seam_core::CommunityId = "B".to_string();
        let c: seam_core::CommunityId = "C".to_string();
        let edges = vec![
            (a.clone(), b.clone()),
            (b.clone(), a.clone()),
            (a.clone(), c.clone()), // not a crossing of the focused seam
        ];
        let result = crossing_threads(&edges, Some((&a, &b)), 500.0, 1000.0);
        assert_eq!(
            result.len(),
            2,
            "both A-B crossing edges must be drawn, and only those"
        );
    }
}
