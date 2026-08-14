//! GRAPH-02: the two-tier load banner (non-fatal warning / fatal error).
//! Renders `app.banner` verbatim -- `load::read_and_ingest`/
//! `load::error_banner` already built the exact heading/body text:
//! heading "Some edges were dropped" / "Couldn't load this file", and the
//! warning body's fixed tail "seam counts below reflect only the valid edges."
//! This module renders `banner.heading`/`banner.body` as-is and never
//! imports or matches on `seam_core`'s `IngestWarning` -- re-deriving that
//! text here would create a second copy that can drift from `load.rs`'s.

use crate::app::{Banner, BannerKind};

fn warning_color() -> egui::Color32 {
    egui::Color32::from_hex("#f2c14e").expect("valid hex")
}

fn error_color() -> egui::Color32 {
    egui::Color32::from_hex("#ff5c72").expect("valid hex")
}

fn muted_color() -> egui::Color32 {
    egui::Color32::from_hex("#93a1bd").expect("valid hex")
}

/// Renders one prepared `Banner`. The caller (`seam_list::show`, the
/// left-panel region already wired in `app.rs`) decides whether to call
/// this at all based on `app.banner`'s `Option` -- this function always
/// draws a banner given one.
pub fn show(ui: &mut egui::Ui, banner: &Banner) {
    let (label, accent) = match banner.kind {
        BannerKind::Warning => ("Warning", warning_color()),
        BannerKind::Error => ("Load failed", error_color()),
    };

    ui.horizontal(|ui| {
        glyph(ui, banner.kind, accent);
        ui.colored_label(accent, label);
    });
    ui.label(egui::RichText::new(&banner.heading).strong().size(15.0));
    ui.label(egui::RichText::new(&banner.body).color(muted_color()));
    ui.add_space(12.0);
}

/// Redraws the original inline-SVG banner glyphs (triangle+bang for a
/// warning, circle+bang for an error) as `Painter` stroke paths -- no icon
/// font/crate added this phase.
fn glyph(ui: &mut egui::Ui, kind: BannerKind, color: egui::Color32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(16.0, 16.0), egui::Sense::hover());
    let stroke = egui::Stroke::new(1.5, color);
    let painter = ui.painter();

    match kind {
        BannerKind::Error => {
            painter.circle_stroke(rect.center(), rect.width() / 2.0 - 1.0, stroke);
        }
        BannerKind::Warning => {
            let top = egui::pos2(rect.center().x, rect.top() + 1.0);
            let left = egui::pos2(rect.left() + 1.0, rect.bottom() - 1.0);
            let right = egui::pos2(rect.right() - 1.0, rect.bottom() - 1.0);
            painter.line_segment([top, left], stroke);
            painter.line_segment([left, right], stroke);
            painter.line_segment([right, top], stroke);
        }
    }

    // Exclamation mark: shared by both glyphs.
    let bang_top = egui::pos2(rect.center().x, rect.center().y - 3.0);
    let bang_bottom = egui::pos2(rect.center().x, rect.center().y + 1.0);
    painter.line_segment([bang_top, bang_bottom], stroke);
    painter.circle_filled(egui::pos2(rect.center().x, rect.bottom() - 3.0), 0.8, color);
}
