//! Left `SidePanel`: SEAM-01 ranked seam list + NAV-01 search (search box
//! lands in Plan 02+). Task 2 implements enough for the M1 tracer to be
//! real: one row per seam, community pair plus crossing count, ranked
//! (already sorted by `seam_core::detect`) — full visual treatment
//! (verdict dot, `.xcount` mono styling, hover) is Plan 02's.

use crate::app::SeamExplorerApp;

pub fn show(ui: &mut egui::Ui, app: &SeamExplorerApp) {
    ui.heading("Seams");

    let Some(_model) = &app.model else {
        ui.label("No graph loaded yet");
        ui.label(
            "Load a graph.json exported by Graphify to see its architectural seams ranked by crossing count.",
        );
        return;
    };

    if let Some(banner) = &app.banner {
        ui.colored_label(banner_color(banner.kind), &banner.heading);
        ui.label(&banner.body);
        ui.separator();
    }

    egui::ScrollArea::vertical().show(ui, |ui| {
        for seam in &app.seams {
            ui.horizontal(|ui| {
                ui.label(format!("{} ↔ {}", seam.a, seam.b));
                ui.monospace(seam.crossings.to_string());
            });
        }
    });
}

fn banner_color(kind: crate::app::BannerKind) -> egui::Color32 {
    match kind {
        crate::app::BannerKind::Warning => egui::Color32::from_rgb(0xf2, 0xc1, 0x4e),
        crate::app::BannerKind::Error => egui::Color32::from_rgb(0xff, 0x5c, 0x72),
    }
}
