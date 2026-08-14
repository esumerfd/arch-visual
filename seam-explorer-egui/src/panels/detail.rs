//! Right `SidePanel`: SEAM-02 seam detail -- verdict title/color, reasons,
//! both sides' bridge components, and per-direction crossing counts. Every
//! value read straight off `seam_core::SeamDetail`; no aggregation or
//! graph-walking happens in this file (thin call-through discipline,
//! pattern map "Domain calls stay thin"). TRACE-02's trace-result rendering
//! is Plan 05's -- this file leaves `app.trace` unread and reserves space
//! below the verdict block for it.

use crate::app::SeamExplorerApp;

const EMPTY_PROMPT: &str = "Select a seam to see its bridge components and verdict — or turn on Trace mode and drag between two components to trace the call path between them.";

/// Side A / cool token (05-UI-SPEC.md Color table) -- the pull-apart
/// canvas's left side; tints this panel's `detail.a` bridge chip list.
const SIDE_A: &str = "#38d6c4";
/// Side B / warm token -- the pull-apart canvas's right side; tints this
/// panel's `detail.b` bridge chip list.
const SIDE_B: &str = "#f2a63c";

fn muted_color() -> egui::Color32 {
    egui::Color32::from_hex("#93a1bd").expect("valid hex")
}

fn side_a_color() -> egui::Color32 {
    egui::Color32::from_hex(SIDE_A).expect("valid hex")
}

fn side_b_color() -> egui::Color32 {
    egui::Color32::from_hex(SIDE_B).expect("valid hex")
}

/// Right-panel body. Verbatim empty-state prompt when no seam is selected
/// (05-UI-SPEC.md Copywriting Contract); otherwise the full verdict/reasons/
/// bridges/metrics rendering below.
pub fn show(ui: &mut egui::Ui, app: &mut SeamExplorerApp) {
    ui.add_space(24.0);

    let Some(detail) = app.detail.as_ref() else {
        ui.colored_label(muted_color(), EMPTY_PROMPT);
        return;
    };

    render_detail(ui, detail);

    // TRACE-02 (Plan 05): the trace result renders into this same panel,
    // below the verdict block -- reserve room for it here rather than
    // building any trace UI in this task.
    ui.add_space(24.0);
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
