//! SEAM-03: persistent, always-visible verdict legend. Static content, no
//! `SeamExplorerApp` dependency -- rendered unconditionally by `app.rs`
//! regardless of load state. `Utility` is never shown (Phase 1 D-06,
//! reconfirmed for this phase -- the variant is never constructed).

pub fn show(ui: &mut egui::Ui) {
    ui.horizontal(|ui| {
        legend_item(ui, seam_core::Verdict::Clean, "Clean");
        legend_item(ui, seam_core::Verdict::Watch, "Watch");
        legend_item(ui, seam_core::Verdict::Leaky, "Leaky");
    });
}

fn legend_item(ui: &mut egui::Ui, verdict: seam_core::Verdict, label: &str) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(8.0, 8.0), egui::Sense::hover());
    ui.painter()
        .circle_filled(rect.center(), 4.0, super::verdict_color(&verdict));
    ui.label(label);
}
