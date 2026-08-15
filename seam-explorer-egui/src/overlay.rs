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
