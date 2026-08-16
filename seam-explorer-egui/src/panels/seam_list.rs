//! Left `SidePanel`: SEAM-01 ranked seam list. `app.seams` already arrives
//! ranked (descending crossing count) from `seam_core::detect` (see
//! `load::read_and_ingest`) -- this panel renders that order, it never
//! re-sorts (thin call-through discipline, pattern map "Domain calls stay
//! thin"). NAV-01 (Plan 04, Task 3): a search `TextEdit` bound to
//! `app.search_query` filters the rendered rows live via `matches`, and
//! selecting a (possibly filtered) row jumps the canvas via
//! `graph_view::jump_to` in addition to the existing focus write.
//!
//! GRAPH-02 (Plan 02, Task 3): this is also the only left-panel call site
//! `app.rs` already wires (`panels::seam_list::show(ui, self)`), and it
//! matches the original frontend's `#bannerContainer` position above the
//! seam list -- so `app.banner` renders here, at the top, via
//! `panels::banner::show`. `app.rs` is frozen for this whole phase; routing
//! the banner call through this already-existing entry point (rather than
//! adding a new call site to `app.rs`) is a deliberate deviation, documented
//! in the 05-02 plan Summary.

use crate::app::{FocusState, SeamExplorerApp};

const EMPTY_HEADING: &str = "No graph loaded yet";
const EMPTY_BODY: &str = "Load a graph.json exported by Graphify to see its architectural seams ranked by crossing count.";
const SEARCH_PLACEHOLDER: &str = "search component or seam\u{2026}";

fn muted_color() -> egui::Color32 {
    egui::Color32::from_hex("#93a1bd").expect("valid hex")
}

fn accent_color() -> egui::Color32 {
    egui::Color32::from_hex("#ff4d8d").expect("valid hex")
}

/// The search `TextEdit`'s stable id -- constructed from the identical
/// string literal `app.rs` (frozen) uses to build the `search_id` it passes
/// to `keyboard::handle`. `egui::Id::new` is a deterministic hash of its
/// input, so this call and `app.rs`'s produce the exact same `Id` value;
/// assigning it explicitly via `TextEdit::id` (rather than letting the
/// widget derive an id from its position in the UI tree) is what makes the
/// two sides of the carve-out provably the same widget, not a
/// freshly-constructed placeholder that happens to collide.
pub fn search_field_id() -> egui::Id {
    egui::Id::new("seam_explorer_search_input")
}

/// Case-insensitive substring predicate over a seam's own name (`a`/`b`
/// community ids, the same text the row renders as `"{a} \u{2194} {b}"`)
/// and the labels of every node belonging to either side -- the two
/// categories NAV-01's search field covers, per its placeholder above. An
/// empty query matches everything; a query matching neither the seam name
/// nor any member node's label matches nothing.
///
/// Deviation note: 05-04-PLAN.md's `<artifacts_this_phase_produces>` lists
/// this function as `matches(seam: &seam_core::Seam, query: &str) -> bool`,
/// but the plan's own `<behavior>` block requires matching "both seam names
/// and node labels" -- `Seam` alone (`a`/`b`/`crossings`) carries no node
/// data, so reaching node labels requires the `Model`. The behavior spec
/// (with its own named test) is the binding contract; the artifacts
/// signature is adjusted to satisfy it (Rule 1).
pub fn matches(model: &seam_core::Model, seam: &seam_core::Seam, query: &str) -> bool {
    if query.is_empty() {
        return true;
    }
    let q = query.to_lowercase();
    if seam.a.to_lowercase().contains(&q) || seam.b.to_lowercase().contains(&q) {
        return true;
    }
    model.graph.node_indices().any(|idx| {
        let node = &model.graph[idx];
        (node.community == seam.a || node.community == seam.b)
            && node.label.to_lowercase().contains(&q)
    })
}

/// Left-panel body: a search field (NAV-01) above the verdict dot + name +
/// mono crossing count list, descending crossing-count order, inside a
/// vertical scroll area. Verbatim empty state before a graph is loaded
/// (05-UI-SPEC.md Copywriting Contract). Clicking a row is the single write
/// site for `app.focus`/`app.detail` (Plan 03's canvas reads `app.focus`)
/// and now also calls `graph_view::jump_to` (NAV-01) to pan/zoom the canvas
/// to the selected seam.
pub fn show(ui: &mut egui::Ui, app: &mut SeamExplorerApp) {
    if let Some(banner) = &app.banner {
        super::banner::show(ui, banner);
    }

    ui.add_space(24.0);
    ui.add(
        egui::TextEdit::singleline(&mut app.search_query)
            .id(search_field_id())
            .hint_text(SEARCH_PLACEHOLDER)
            .desired_width(f32::INFINITY),
    );
    ui.add_space(8.0);
    ui.label(egui::RichText::new("Seams \u{b7} ranked by crossings").small());
    ui.add_space(16.0);

    let Some(model) = app.model.as_ref() else {
        ui.strong(EMPTY_HEADING);
        ui.colored_label(muted_color(), EMPTY_BODY);
        return;
    };

    let query = app.search_query.clone();
    let visible: Vec<(usize, seam_core::Seam)> = app
        .seams
        .iter()
        .enumerate()
        .filter(|(_, seam)| matches(model, seam, &query))
        .map(|(i, seam)| (i, seam.clone()))
        .collect();

    if visible.is_empty() && !query.is_empty() {
        ui.colored_label(
            muted_color(),
            format!("No component or seam matches \"{query}\"."),
        );
        return;
    }

    let mut clicked_index: Option<usize> = None;

    egui::ScrollArea::vertical().show(ui, |ui| {
        ui.spacing_mut().item_spacing.y = 8.0;
        for (i, seam) in &visible {
            let verdict = seam_verdict(model, seam);
            let selected = app
                .focus
                .as_ref()
                .is_some_and(|f| f.a == seam.a && f.b == seam.b);
            if row(ui, seam, verdict, selected).clicked() {
                clicked_index = Some(*i);
            }
        }
    });

    if let Some(i) = clicked_index {
        let seam = app.seams[i].clone();
        select_seam(app, &seam);
        // NAV-01: selecting a search result focuses it the same way a plain
        // row click does (select_seam, above) and additionally jumps the
        // canvas. A focused seam's pull-apart layout is always centered on
        // the canvas center by construction (`layout::seam_target_x`'s
        // `cx = W()/2`), so "jump to this seam" is exactly "center the
        // view" -- `JumpTarget::Seam` with no offset, not an arbitrary
        // position this panel would otherwise have no way to know (canvas
        // geometry lives in `graph_view.rs`, not here).
        crate::graph_view::jump_to(app, crate::graph_view::JumpTarget::Seam(egui::Pos2::ZERO));
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

/// Sets `app.focus` + `app.detail` for the clicked seam -- the single write
/// site for both fields (Plan 05 Task 2's `panels::detail::show_trace_result`
/// routes crossed-seam-entry clicks through this same function rather than
/// adding a second one, per its own action text). Called once, after the
/// scroll area's closure has finished borrowing `app` immutably, to keep the
/// borrow shapes simple.
///
/// Also clears any active trace result (mirrors the D3 original's
/// `focusSeam` calling `clearTrace()` first, `frontend/index.html:591`) --
/// mutual exclusivity (D-12/D-15): a trace highlight and a focused seam's
/// pull-apart/fault-line never coexist on canvas. The opposite direction
/// (completing a trace clearing `app.focus`/`app.detail`) lives in
/// `graph_view::handle_trace_gesture`.
pub(crate) fn select_seam(app: &mut SeamExplorerApp, seam: &seam_core::Seam) {
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
    app.trace = None;
}

/// One seam row: verdict-colored dot (`egui::Painter`) + a frameless,
/// natively-click-sensing `egui::Button` carrying the name + mono crossing
/// count (a `right_to_left` sub-layout keeps the crossing count pinned to
/// the row's right edge, matching the original visual order). Long names
/// wrap naturally inside the fixed-width panel (no nowrap/ellipsis, matching
/// the original).
///
/// The previous implementation built the row as a `ui.horizontal(...)`
/// group and retrofitted click-sensing via `.interact(egui::Sense::click())`
/// -- confirmed, via an isolated `egui_kittest` reproduction
/// (`.planning/debug/seam-list-click-to-focus-broken.md`), to never register
/// `.clicked()` for a group container in egui 0.35's interaction-resolution
/// pipeline, even for a synthetic click landing exactly inside the group's
/// own `interact_rect`. This `Button` senses clicks natively at the moment
/// it's added to the `Ui` -- the same class of leaf widget `detail.rs`'s
/// crossed-seam-entry rows already use successfully. Do not reintroduce the
/// group-retrofit pattern here.
pub fn row(
    ui: &mut egui::Ui,
    seam: &seam_core::Seam,
    verdict: seam_core::Verdict,
    selected: bool,
) -> egui::Response {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;

        let (dot_rect, _) = ui.allocate_exact_size(egui::vec2(8.0, 8.0), egui::Sense::hover());
        ui.painter()
            .circle_filled(dot_rect.center(), 4.0, super::verdict_color(&verdict));

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.monospace(format!("{}\u{d7}", seam.crossings));

            let name = format!("{} \u{2194} {}", seam.a, seam.b);
            let name_text = if selected {
                egui::RichText::new(name).color(accent_color())
            } else {
                egui::RichText::new(name)
            };
            let button = egui::Button::new(name_text)
                .frame(false)
                .min_size(egui::vec2(ui.available_width(), 0.0));
            ui.add(button)
        })
        .inner
    })
    .inner
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model_from(json: &str) -> seam_core::Model {
        seam_core::from_json(json)
            .expect("fixture must ingest cleanly")
            .model
    }

    const SEAM_NAME_FIXTURE: &str = r#"{"nodes":[{"id":"a1","community":"Alpha"},{"id":"b1","community":"Beta"}],"links":[{"source":"a1","target":"b1","relation":"calls","confidence":"EXTRACTED"}]}"#;

    const NODE_LABEL_FIXTURE: &str = r#"{"nodes":[{"id":"a1","label":"PaymentService","community":"Alpha"},{"id":"b1","label":"OrderService","community":"Beta"}],"links":[{"source":"a1","target":"b1","relation":"calls","confidence":"EXTRACTED"}]}"#;

    /// 05-11: two communities carrying `community_name` (05-09) -- the
    /// user's motivating example, raw numeric-looking ids "0"/"16" named
    /// "Commands"/"Runtime".
    const NAMED_SEAM_FIXTURE: &str = r#"{"nodes":[{"id":"a1","community":"0","community_name":"Commands"},{"id":"b1","community":"16","community_name":"Runtime"}],"links":[{"source":"a1","target":"b1","relation":"calls","confidence":"EXTRACTED"}]}"#;

    fn seam(a: &str, b: &str) -> seam_core::Seam {
        seam_core::Seam {
            a: a.to_string(),
            b: b.to_string(),
            crossings: 1,
        }
    }

    #[test]
    fn test_matches_seam_name_case_insensitive_substring() {
        let model = model_from(SEAM_NAME_FIXTURE);
        let s = seam("Alpha", "Beta");

        assert!(matches(&model, &s, "alpha"));
        assert!(matches(&model, &s, "BETA"));
        assert!(!matches(&model, &s, "gamma"));
    }

    #[test]
    fn test_matches_node_label_case_insensitive_substring() {
        let model = model_from(NODE_LABEL_FIXTURE);
        let s = seam("Alpha", "Beta");

        assert!(matches(&model, &s, "payment"));
        assert!(matches(&model, &s, "ORDERSERVICE"));
        assert!(!matches(&model, &s, "inventory"));
    }

    #[test]
    fn test_matches_empty_query_matches_everything() {
        let model = model_from(SEAM_NAME_FIXTURE);
        let s = seam("Alpha", "Beta");

        assert!(matches(&model, &s, ""));
    }

    #[test]
    fn test_matches_no_hit_matches_nothing() {
        let model = model_from(SEAM_NAME_FIXTURE);
        let s = seam("Alpha", "Beta");

        assert!(!matches(&model, &s, "nonexistent_query_xyz"));
    }

    /// 05-11 DP-11-04: search must also find a community by its resolved
    /// display name, not just its raw id or a node label.
    #[test]
    fn matches_finds_a_community_by_its_name() {
        let model = model_from(NAMED_SEAM_FIXTURE);
        let s = seam("0", "16");

        assert!(matches(&model, &s, "commands"));
        assert!(matches(&model, &s, "RUNTIME"));
        assert!(!matches(&model, &s, "nonexistent"));
    }

    /// 05-11 DP-11-04: users who know a community by its raw id keep
    /// working even once names are displayed.
    #[test]
    fn matches_still_finds_a_community_by_its_raw_id() {
        let model = model_from(NAMED_SEAM_FIXTURE);
        let s = seam("0", "16");

        assert!(matches(&model, &s, "0"));
        assert!(matches(&model, &s, "16"));
    }

    /// 05-11 regression guard: node-label matching (pre-existing behaviour)
    /// must not regress once name matching is added.
    #[test]
    fn matches_still_finds_a_node_by_its_label() {
        let model = model_from(NODE_LABEL_FIXTURE);
        let s = seam("Alpha", "Beta");

        assert!(matches(&model, &s, "payment"));
        assert!(matches(&model, &s, "ORDERSERVICE"));
        assert!(!matches(&model, &s, "inventory"));
    }
}
