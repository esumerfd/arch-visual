//! Left `SidePanel`: SEAM-01 ranked seam list. `app.seams` already arrives
//! ranked (descending crossing count) from `seam_core::detect` (see
//! `load::read_and_ingest`) -- this panel renders that order, it never
//! re-sorts (thin call-through discipline, pattern map "Domain calls stay
//! thin"). NAV-01 search filtering over `app.search_query` is Plan 04's;
//! this file leaves that field unread for now.
//!
//! GRAPH-02 (Task 3): this is also the only left-panel call site `app.rs`
//! already wires (`panels::seam_list::show(ui, self)`), and it matches the
//! original frontend's `#bannerContainer` position above the seam list --
//! so `app.banner` renders here, at the top, via `panels::banner::show`.
//! `app.rs` is frozen for this whole plan; routing the banner call through
//! this already-existing entry point (rather than adding a new call site to
//! `app.rs`) is a deliberate Task 3 deviation, documented in the plan
//! Summary.

use crate::app::{FocusState, SeamExplorerApp};

const EMPTY_HEADING: &str = "No graph loaded yet";
const EMPTY_BODY: &str = "Load a graph.json exported by Graphify to see its architectural seams ranked by crossing count.";

fn muted_color() -> egui::Color32 {
    egui::Color32::from_hex("#93a1bd").expect("valid hex")
}

fn accent_color() -> egui::Color32 {
    egui::Color32::from_hex("#ff4d8d").expect("valid hex")
}

/// Left-panel body: verdict dot + name + mono crossing count per seam,
/// descending crossing-count order, inside a vertical scroll area. Verbatim
/// empty state before a graph is loaded (05-UI-SPEC.md Copywriting
/// Contract). Clicking a row is the single write site for
/// `app.focus`/`app.detail` (Plan 03's canvas reads `app.focus`).
pub fn show(ui: &mut egui::Ui, app: &mut SeamExplorerApp) {
    if let Some(banner) = &app.banner {
        super::banner::show(ui, banner);
    }

    ui.add_space(24.0);
    ui.label(egui::RichText::new("Seams \u{b7} ranked by crossings").small());
    ui.add_space(16.0);

    let Some(model) = app.model.as_ref() else {
        ui.strong(EMPTY_HEADING);
        ui.colored_label(muted_color(), EMPTY_BODY);
        return;
    };

    let mut clicked_index: Option<usize> = None;

    egui::ScrollArea::vertical().show(ui, |ui| {
        ui.spacing_mut().item_spacing.y = 8.0;
        for (i, seam) in app.seams.iter().enumerate() {
            let verdict = seam_verdict(model, seam);
            let selected = app
                .focus
                .as_ref()
                .is_some_and(|f| f.a == seam.a && f.b == seam.b);
            if row(ui, seam, verdict, selected).clicked() {
                clicked_index = Some(i);
            }
        }
    });

    if let Some(i) = clicked_index {
        let seam = app.seams[i].clone();
        select_seam(app, &seam);
    }
}

/// Looks up a single seam's verdict via `seam_core::seam_detail` (thin
/// call-through -- no aggregation here). Falls back to `Clean` only if the
/// SCC index somehow isn't finalized yet; the load path always finalizes it
/// before `app.seams` is populated, so this branch is unreachable in
/// practice.
fn seam_verdict(model: &seam_core::Model, seam: &seam_core::Seam) -> seam_core::Verdict {
    model
        .scc
        .as_ref()
        .map(|scc| seam_core::seam_detail(model, scc, &seam.a, &seam.b).verdict)
        .unwrap_or(seam_core::Verdict::Clean)
}

/// Sets `app.focus` + `app.detail` for the clicked seam. Called once, after
/// the scroll area's closure has finished borrowing `app` immutably, to
/// keep the borrow shapes simple.
fn select_seam(app: &mut SeamExplorerApp, seam: &seam_core::Seam) {
    let Some(model) = app.model.as_ref() else {
        return;
    };
    let Some(scc) = model.scc.as_ref() else {
        return;
    };
    let detail = seam_core::seam_detail(model, scc, &seam.a, &seam.b);
    app.focus = Some(FocusState {
        a: seam.a.clone(),
        b: seam.b.clone(),
    });
    app.detail = Some(detail);
}

/// One seam row: verdict-colored dot (`egui::Painter`) + name + mono
/// crossing count. Long names wrap naturally inside the fixed-width panel
/// (no nowrap/ellipsis, matching the original). Returns a `Response` sensing
/// clicks across the whole row, not just the label.
pub fn row(
    ui: &mut egui::Ui,
    seam: &seam_core::Seam,
    verdict: seam_core::Verdict,
    selected: bool,
) -> egui::Response {
    let response = ui
        .horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;

            let (dot_rect, _) = ui.allocate_exact_size(egui::vec2(8.0, 8.0), egui::Sense::hover());
            ui.painter()
                .circle_filled(dot_rect.center(), 4.0, super::verdict_color(&verdict));

            let name = format!("{} \u{2194} {}", seam.a, seam.b);
            let name_text = if selected {
                egui::RichText::new(name).color(accent_color())
            } else {
                egui::RichText::new(name)
            };
            ui.label(name_text);

            ui.monospace(format!("{}\u{d7}", seam.crossings));
        })
        .response;

    response.interact(egui::Sense::click())
}
