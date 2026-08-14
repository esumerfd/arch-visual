//! SEAM-03: persistent, always-visible verdict legend. Static content, no
//! `SeamExplorerApp` dependency. `Utility` is never shown (Phase 1 D-06,
//! reconfirmed for this phase — the variant is never constructed).

pub fn show(ui: &mut egui::Ui) {
    ui.horizontal(|ui| {
        legend_item(ui, egui::Color32::from_rgb(0x4f, 0xd0, 0x8a), "Clean");
        legend_item(ui, egui::Color32::from_rgb(0xf2, 0xc1, 0x4e), "Watch");
        legend_item(ui, egui::Color32::from_rgb(0xff, 0x5c, 0x72), "Leaky");
    });
}

fn legend_item(ui: &mut egui::Ui, color: egui::Color32, label: &str) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(8.0, 8.0), egui::Sense::hover());
    ui.painter().circle_filled(rect.center(), 4.0, color);
    ui.label(label);
}
