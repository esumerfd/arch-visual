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
use crate::panels::verdict_color;

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

/// Verdict -> the fault line's stroke color. Thin wrapper over
/// `panels::verdict_color` -- the single source of the three verdict hex
/// values (D-15) -- so this file never re-types them (`4fd08a` gate).
pub fn seam_line_color(verdict: &seam_core::Verdict) -> egui::Color32 {
    verdict_color(verdict)
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
    fn test_line_color_follows_verdict() {
        assert_eq!(
            seam_line_color(&seam_core::Verdict::Leaky),
            verdict_color(&seam_core::Verdict::Leaky)
        );
        assert_eq!(
            seam_line_color(&seam_core::Verdict::Clean),
            verdict_color(&seam_core::Verdict::Clean)
        );
        assert_ne!(
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
