//! Right `SidePanel`: SEAM-02 seam detail -- verdict title/color, reasons,
//! both sides' bridge components, and per-direction crossing counts -- plus
//! TRACE-02's trace-result rendering below it (hops, clickable crossed
//! seams, no-path/zero-crossing messages). Every value read straight off
//! `seam_core::SeamDetail`/`seam_core::TracePath`; no aggregation or
//! graph-walking happens in this file (thin call-through discipline,
//! pattern map "Domain calls stay thin").

use crate::app::SeamExplorerApp;

const EMPTY_PROMPT: &str = "Select a seam to see its bridge components and verdict — or turn on Trace mode and drag between two components to trace the call path between them.";

/// Side A / cool token (05-UI-SPEC.md Color table) -- the pull-apart
/// canvas's left side; tints this panel's `detail.a` bridge chip list.
const SIDE_A: &str = "#38d6c4";
/// Side B / warm token -- the pull-apart canvas's right side; tints this
/// panel's `detail.b` bridge chip list.
const SIDE_B: &str = "#f2a63c";
/// `--seam` accent (05-UI-SPEC.md Color table) -- the crossed-seams list's
/// arrow glyph, matching the canvas fault-line/thread accent.
const ACCENT: &str = "#ff4d8d";

fn muted_color() -> egui::Color32 {
    egui::Color32::from_hex("#93a1bd").expect("valid hex")
}

fn side_a_color() -> egui::Color32 {
    egui::Color32::from_hex(SIDE_A).expect("valid hex")
}

fn side_b_color() -> egui::Color32 {
    egui::Color32::from_hex(SIDE_B).expect("valid hex")
}

fn accent_color() -> egui::Color32 {
    egui::Color32::from_hex(ACCENT).expect("valid hex")
}

/// Right-panel body. Verbatim empty-state prompt when neither a seam nor a
/// trace result is present (05-UI-SPEC.md Copywriting Contract); otherwise
/// the seam-detail block (when a seam is focused) followed by the trace
/// result block (when a trace has run) -- `graph_view::handle_trace_gesture`
/// and `panels::seam_list::select_seam` keep the two mutually exclusive
/// (D-12/D-15), so in practice at most one of the two renders at a time, but
/// this function does not itself assume that invariant.
pub fn show(ui: &mut egui::Ui, app: &mut SeamExplorerApp) {
    ui.add_space(24.0);

    if app.detail.is_none() && app.trace.is_none() {
        ui.colored_label(muted_color(), EMPTY_PROMPT);
        return;
    }

    if let Some(detail) = app.detail.clone() {
        render_detail(ui, &detail);
        ui.add_space(24.0);
    }

    show_trace_result(ui, app);
}

/// TRACE-02: renders `app.trace` (a no-op when `None`) -- hop list, then
/// either the crossed-seams list (with the count in its eyebrow label) or
/// the distinct zero-crossing/no-path messages. Node labels are looked up
/// from `app.model` (falling back to the raw id if the model is somehow
/// gone), matching the D3 original's `labelFor` (`frontend/index.html:805`).
fn show_trace_result(ui: &mut egui::Ui, app: &mut SeamExplorerApp) {
    let Some(trace) = app.trace.clone() else {
        return;
    };

    let from_label = node_label(app, &trace.from);
    let to_label = node_label(app, &trace.to);

    ui.label(egui::RichText::new("Call path traced").small());
    ui.add_space(8.0);
    ui.label(
        egui::RichText::new(format!("{from_label} \u{2192} {to_label}"))
            .strong()
            .size(15.0),
    );
    ui.add_space(8.0);

    let Some(path) = &trace.path else {
        // No-path case (D-09/D-11): verbatim, plain muted styling -- this
        // is an informative outcome, never an error/warning treatment.
        ui.colored_label(
            muted_color(),
            format!(
                "No directed call path from {from_label} to {to_label}. They may be decoupled — or the dependency runs the other way."
            ),
        );
        return;
    };

    ui.horizontal(|ui| {
        metric(ui, "hops", path.hops.len());
        metric(ui, "seams crossed", path.seams_crossed.len());
    });
    ui.add_space(12.0);

    ui.label(egui::RichText::new("Path").small());
    ui.horizontal_wrapped(|ui| {
        for (i, id) in path.hops.iter().enumerate() {
            if i > 0 {
                ui.colored_label(muted_color(), "\u{2192}");
            }
            ui.monospace(node_label(app, id));
        }
    });
    ui.add_space(12.0);

    if path.seams_crossed.is_empty() {
        // Zero-crossing case: the distinct positive message, Clean-colored,
        // sourced from `verdict_color` rather than a re-typed hex literal.
        ui.colored_label(
            super::verdict_color(&seam_core::Verdict::Clean),
            "Stays within one community — no seam crossed.",
        );
        return;
    }

    // One and many crossed seams render the identical list structure --
    // only the eyebrow's count changes, never the markup (Open Question 2,
    // ported unchanged: no de-duplication -- a path crossing the same seam
    // pair twice lists it twice).
    ui.label(egui::RichText::new(format!("Crossed seams ({})", path.seams_crossed.len())).small());

    // A single `Label` widget per row (rather than a `ui.horizontal(...)`
    // group retrofitted with `Response::interact`, the pattern
    // `seam_list::row` uses) so each entry is its own directly-addressable
    // accesskit node -- `05-02`'s summary documents the horizontal-group
    // retrofit pattern as not reliably targetable by
    // `kittest::Node::click()`; this task's `crossed_seam_entry_is_clickable`
    // test needs a real automated click, not a deferred human-check.
    let mut clicked: Option<(seam_core::CommunityId, seam_core::CommunityId)> = None;
    for (ca, cb) in &path.seams_crossed {
        let text = format!("{ca} \u{2192} {cb}");
        let response = ui.add(
            egui::Label::new(egui::RichText::new(text).color(accent_color()))
                .sense(egui::Sense::click()),
        );
        if response.clicked() {
            clicked = Some((ca.clone(), cb.clone()));
        }
    }

    // Route the click through the same focus write site a seam-row click
    // uses (`seam_list::select_seam`) rather than adding a second one --
    // two focus writers is how the canvas and the panel end up disagreeing
    // about what is focused (this task's key_link).
    if let Some((ca, cb)) = clicked {
        if let Some(seam) = find_seam_for_pair(&app.seams, &ca, &cb) {
            super::seam_list::select_seam(app, &seam);
        }
    }
}

/// Order-agnostic seam lookup for a `TracePath.seams_crossed` pair --
/// `seams_crossed` preserves traversal direction while `app.seams` (from
/// `seam_core::detect`) normalizes each pair `a < b`, so a direct field
/// match would miss half of all crossings. Ports `findSeamForPair`
/// (`frontend/index.html:801-803`).
fn find_seam_for_pair(
    seams: &[seam_core::Seam],
    ca: &seam_core::CommunityId,
    cb: &seam_core::CommunityId,
) -> Option<seam_core::Seam> {
    seams
        .iter()
        .find(|s| (&s.a == ca && &s.b == cb) || (&s.a == cb && &s.b == ca))
        .cloned()
}

/// Looks up a node's display label by id, falling back to the raw id if the
/// model is gone or the id is unknown (defensive only -- both endpoints of
/// any `app.trace` always came from a node that existed in the currently
/// loaded model).
fn node_label(app: &SeamExplorerApp, id: &str) -> String {
    let Some(model) = app.model.as_ref() else {
        return id.to_string();
    };
    let Some(&idx) = model.index.get(id) else {
        return id.to_string();
    };
    model.graph[idx].label.clone()
}

fn render_detail(ui: &mut egui::Ui, detail: &seam_core::SeamDetail) {
    let color = super::verdict_color(&detail.verdict);
    let title = super::verdict_title(&detail.verdict);

    ui.label(egui::RichText::new(title).strong().size(15.0).color(color));
    ui.label(
        egui::RichText::new(format!("{} / {}", detail.a, detail.b))
            .small()
            .color(muted_color()),
    );
    ui.add_space(8.0);

    for reason in &detail.reasons {
        ui.colored_label(muted_color(), format!("\u{2022} {reason}"));
    }
    ui.add_space(12.0);

    ui.columns(2, |columns| {
        columns[0].label(egui::RichText::new(format!("{} \u{b7} interface", detail.a)).small());
        bridge_list(&mut columns[0], &detail.bridges_a, side_a_color());

        columns[1].label(egui::RichText::new(format!("{} \u{b7} interface", detail.b)).small());
        bridge_list(&mut columns[1], &detail.bridges_b, side_b_color());
    });
    ui.add_space(12.0);

    ui.horizontal(|ui| {
        metric(
            ui,
            &format!("{} \u{2192} {}", detail.a, detail.b),
            detail.a_to_b,
        );
        metric(
            ui,
            &format!("{} \u{2192} {}", detail.b, detail.a),
            detail.b_to_a,
        );
    });
}

/// One bridge-node chip list, tinted by side. Bridge sets are structurally
/// derived from crossing edges (a seam only exists when both sides have at
/// least one bridge node), so no empty-list branch is implemented here.
fn bridge_list(ui: &mut egui::Ui, ids: &[String], tick_color: egui::Color32) {
    for id in ids {
        ui.horizontal(|ui| {
            let (tick_rect, _) = ui.allocate_exact_size(egui::vec2(6.0, 6.0), egui::Sense::hover());
            ui.painter().rect_filled(tick_rect, 2.0, tick_color);
            ui.monospace(id);
        });
    }
}

/// One directional-crossing metric tile: muted label + a large monospace
/// value (05-UI-SPEC.md Typography "Display / metric" role, 20px).
fn metric(ui: &mut egui::Ui, label: &str, value: usize) {
    ui.vertical(|ui| {
        ui.label(egui::RichText::new(label).small().color(muted_color()));
        ui.label(
            egui::RichText::new(value.to_string())
                .monospace()
                .strong()
                .size(20.0),
        );
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The no-path message's color must be the muted token, never the
    /// destructive/error token (`panels::verdict_color(&Verdict::Leaky)`) --
    /// this is an informative outcome, not a failure. Accesskit (the tree
    /// `egui_kittest`'s `Harness` queries against) does not carry rendered
    /// foreground color [VERIFIED: `egui` 0.35.0 source has no
    /// `set_foreground_color` call anywhere in its accesskit integration],
    /// so this claim is checked here as a plain `--lib` unit test against
    /// the same color-selection functions `show_trace_result` calls, rather
    /// than via a kittest node query -- `trace_no_path_message` (in
    /// `tests/panels.rs`) covers the message's verbatim text content.
    #[test]
    fn no_path_message_color_is_muted_not_destructive() {
        let no_path_color = muted_color();
        let destructive_color = crate::panels::verdict_color(&seam_core::Verdict::Leaky);
        assert_eq!(no_path_color, egui::Color32::from_rgb(0x93, 0xa1, 0xbd));
        assert_ne!(no_path_color, destructive_color);
    }
}
