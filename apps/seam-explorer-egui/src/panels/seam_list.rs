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
/// The find-node results cap (quick-260926-nop, DP-NOP-04): on a large real
/// graph a short query can match hundreds of nodes; an uncapped list would
/// bury the seam list under the fold. Part of `find_nodes`'s own pure return
/// value (`NodeHits::hidden`), not a render-time truncation, so the cap is
/// unit-testable without a live `egui::Ui`.
const MAX_NODE_RESULTS: usize = 8;

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
    // 05-11 DP-11-04: also test the resolved display names, so a user who
    // knows a community by its rendered name (rather than its raw id) can
    // find it too.
    if model.community_label(&seam.a).to_lowercase().contains(&q)
        || model.community_label(&seam.b).to_lowercase().contains(&q)
    {
        return true;
    }
    model.graph.node_indices().any(|idx| {
        let node = &model.graph[idx];
        (node.community == seam.a || node.community == seam.b) && node_label_matches(node, &q)
    })
}

/// The single per-node label-match clause (quick-260926-nop, extracted
/// verbatim from `matches`'s own `.any(..)` closure): case-insensitive
/// substring over `node.label`, false for an empty query. `matches` itself
/// keeps its own empty-query early return exactly where it is (discovery
/// finding 1: `matches` never reaches this predicate with an empty query at
/// all), so this function's own empty-query behaviour is unobservable from
/// `matches` and is instead the behaviour `find_nodes` below actually needs.
pub fn node_label_matches(node: &seam_core::Node, query: &str) -> bool {
    if query.is_empty() {
        return false;
    }
    node.label.to_lowercase().contains(&query.to_lowercase())
}

/// One find-node result (quick-260926-nop): the render layer needs no
/// `Model` -- `find_nodes` resolves `community_label` once per hit here
/// (05-11 DP-11-01, the one resolver).
#[derive(Clone, Debug)]
pub struct NodeHit {
    pub id: String,
    pub label: String,
    pub community_label: String,
}

/// `find_nodes`'s return shape: the (possibly capped) visible hits plus a
/// count of how many additional matches were not shown (DP-NOP-04).
#[derive(Clone, Debug, Default)]
pub struct NodeHits {
    pub shown: Vec<NodeHit>,
    pub hidden: usize,
}

/// Enumerates individual matching NODES -- a different shape of question
/// than `matches` (which answers "does this seam match"). Scope is enforced
/// by extraction, not re-implementation (DP-NOP-05): a node is a candidate
/// only when it passes BOTH `node_label_matches` and
/// `crate::graph_view::node_rendered` -- the exact same rendered-set
/// membership predicate `graph_view::build_graph` is built from, fed the
/// same `focus`/`forced` pair `graph_view::show` hands it. This function
/// adds no new source of "is it visible" truth; a node force-included by an
/// active resolved trace path is therefore findable too -- correct, because
/// it is genuinely on screen (see `node_rendered`'s own doc comment).
/// Results are sorted deterministically by label then id.
pub fn find_nodes(
    model: &seam_core::Model,
    focus: Option<&crate::app::FocusState>,
    forced: &std::collections::HashSet<String>,
    query: &str,
) -> NodeHits {
    let mut shown: Vec<NodeHit> = model
        .graph
        .node_indices()
        .filter_map(|idx| {
            let node = &model.graph[idx];
            if node_label_matches(node, query)
                && crate::graph_view::node_rendered(node, focus, forced)
            {
                Some(NodeHit {
                    id: node.id.clone(),
                    label: node.label.clone(),
                    community_label: model.community_label(&node.community).to_string(),
                })
            } else {
                None
            }
        })
        .collect();
    shown.sort_by(|a, b| a.label.cmp(&b.label).then_with(|| a.id.cmp(&b.id)));

    // DP-NOP-04: the cap lives in the pure function's own return value, not
    // in the render layer -- `shown` keeps the first `MAX_NODE_RESULTS` in
    // the deterministic order just established, `hidden` counts the rest.
    let hidden = shown.len().saturating_sub(MAX_NODE_RESULTS);
    shown.truncate(MAX_NODE_RESULTS);
    NodeHits { shown, hidden }
}

/// The single place the seam's "A ↔ B" pair display string is built,
/// resolving both sides through `Model::community_label` (05-11 DP-11-01 --
/// the one resolver, no panel writes its own fallback). `row` renders
/// whatever string this returns; it never derives a name from `seam` itself.
pub fn seam_display_name(model: &seam_core::Model, seam: &seam_core::Seam) -> String {
    format!(
        "{} \u{2194} {}",
        model.community_label(&seam.a),
        model.community_label(&seam.b)
    )
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
    ui.add_space(16.0);

    // Plan 09-03: the model guard and the row source both come from the
    // timeline accessors, so this panel describes whatever moment the canvas
    // is showing. The question the guard asks widens from "is a graph loaded"
    // to "is there anything to display"; the empty state below is unchanged.
    let Some(model) = crate::timeline::display_model(app) else {
        ui.strong(EMPTY_HEADING);
        ui.colored_label(muted_color(), EMPTY_BODY);
        return;
    };

    let query = app.search_query.clone();
    // Resolve each visible seam's display name here, into the same
    // collected vector the scroll-area closure below already reads from --
    // rather than reaching back into `model` inside the closure -- keeping
    // the existing simple-borrow-shape discipline this `show` already uses.
    let visible: Vec<(usize, seam_core::Seam, String)> = crate::timeline::display_seams(app)
        .iter()
        .enumerate()
        .filter(|(_, seam)| matches(model, seam, &query))
        .map(|(i, seam)| {
            let name = seam_display_name(model, seam);
            (i, seam.clone(), name)
        })
        .collect();

    // quick-260926-nop: the findable-node set, derived from the SAME
    // render-focus/forced-visible-ids pair `graph_view::show` hands
    // `build_graph` -- see `find_nodes`'s own doc comment (DP-NOP-05).
    let render_focus = crate::graph_view::render_focus(app);
    let forced = crate::graph_view::forced_visible_ids(app);
    let hits = find_nodes(model, render_focus.as_ref(), &forced, &query);

    if visible.is_empty() && hits.shown.is_empty() && !query.is_empty() {
        ui.colored_label(
            muted_color(),
            format!("No component or seam matches \"{query}\"."),
        );
        return;
    }

    let mut clicked_index: Option<usize> = None;
    let mut clicked_node_id: Option<String> = None;

    egui::ScrollArea::vertical().show(ui, |ui| {
        ui.spacing_mut().item_spacing.y = 8.0;

        // quick-260926-nop (D-01): node results render above the seam rows,
        // under their own heading, visually distinct from a seam row (a
        // square swatch, not a round verdict dot -- DP-NOP-03).
        if !hits.shown.is_empty() {
            ui.label(egui::RichText::new(format!("Components ({})", hits.shown.len())).small());
            ui.add_space(8.0);
            for hit in &hits.shown {
                let selected = app.selected_node.as_deref() == Some(hit.id.as_str());
                if node_row(ui, hit, selected).clicked() {
                    clicked_node_id = Some(hit.id.clone());
                }
            }
            if hits.hidden > 0 {
                ui.colored_label(
                    muted_color(),
                    format!(
                        "\u{2026} {} more match \u{2014} narrow your search",
                        hits.hidden
                    ),
                );
            }
            ui.separator();
        }

        ui.label(egui::RichText::new("Seams \u{b7} ranked by crossings").small());
        ui.add_space(8.0);
        for (i, seam, name) in &visible {
            let verdict = seam_verdict(model, seam);
            let selected = app
                .focus
                .as_ref()
                .is_some_and(|f| f.a == seam.a && f.b == seam.b);
            if row(ui, seam, name, verdict, selected).clicked() {
                clicked_index = Some(*i);
            }
        }
    });

    if let Some(i) = clicked_index {
        // Plan 09-03: indexed back out of the DISPLAYED list, never the live
        // one. The index came from enumerating the displayed list above, so
        // reading it out of `app.seams` selects a different seam whenever the
        // two lists differ -- which, while paused, is exactly when they do.
        let seam = crate::timeline::display_seams(app)[i].clone();
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

    if let Some(id) = clicked_node_id {
        // quick-260926-nop (D-02/D-03): routed through the crate's single
        // node-click jump function -- the same one the detail panel's
        // bridge-row click (`panels::detail::bridge_list`) calls. Never
        // writes `app.focus`, never touches the forced-visible set, never
        // touches `app.trace_gesture`.
        crate::graph_view::jump_to_node(ui, app, &id);
    }
}

/// One node-search-result row (quick-260926-nop, DP-NOP-03): a small SQUARE
/// swatch (distinct from a seam row's round verdict dot), the node's
/// monospace label as the click target, and a muted small community name --
/// the same clickable shape `detail::bridge_list`'s bridge row uses
/// (hover-only swatch sensing, `Sense::click()` only on the label), never
/// the group-retrofit `.interact()` pattern `row`'s own doc comment records
/// as unreliable in egui 0.35.
fn node_row(ui: &mut egui::Ui, hit: &NodeHit, selected: bool) -> egui::Response {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;

        let (swatch_rect, _) = ui.allocate_exact_size(egui::vec2(6.0, 6.0), egui::Sense::hover());
        ui.painter().rect_filled(swatch_rect, 0.0, muted_color());

        let label_text = egui::RichText::new(hit.label.as_str()).monospace();
        // quick-260926-nop Task 2: a selected node result takes the same
        // accent tint a selected seam row takes (DP-NOP-03).
        let label_text = if selected {
            label_text.color(accent_color())
        } else {
            label_text
        };
        let response = ui.add(egui::Label::new(label_text).sense(egui::Sense::click()));

        ui.colored_label(
            muted_color(),
            egui::RichText::new(hit.community_label.as_str()).small(),
        );

        response
    })
    .inner
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
    // Plan 09-03: scored against the DISPLAYED model and its SCC cache. This
    // is the single focus/detail write site, so redirecting it here is what
    // stops a click on a paused row from writing a live-model detail into a
    // historical view -- `detail.rs`'s crossed-seam entry routes through this
    // same function and inherits the fix.
    let Some(model) = crate::timeline::display_model(app) else {
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
    // quick-260915-sf7: this is the app's one focus writer, and therefore
    // the one and only place `app.selected_node` is cleared -- a highlight
    // left over from a previous seam's interface list would point at a
    // node that may no longer even be rendered under the new focus.
    app.selected_node = None;
}

/// One seam row: verdict-colored dot (`egui::Painter`) + a frameless,
/// natively-click-sensing `egui::Button` carrying the name + mono crossing
/// count (a `right_to_left` sub-layout keeps the crossing count pinned to
/// the row's right edge, matching the original visual order). Long names
/// wrap naturally inside the fixed-width panel (no nowrap/ellipsis, matching
/// the original).
///
/// `name` is the already-resolved display string (05-11 -- see
/// `seam_display_name`); `row` stays a pure rendering function and never
/// derives a name from `seam` itself, so it needs no `Model` (DP-11-01).
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
    name: &str,
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

    // ============================================================
    // quick-260926-nop Task 1: find-node -- `node_label_matches` (the
    // extracted per-node clause `matches` now shares) and `find_nodes` (the
    // new node-enumeration query), scoped to what `graph_view::build_graph`
    // actually renders (D-02, DP-NOP-05).
    // ============================================================

    /// A three-community fixture (A, B, C) with distinct node labels, shaped
    /// like `clean.json`, for `find_nodes`'s scope tests.
    const NODE_SEARCH_FIXTURE: &str = r#"{"nodes":[{"id":"a1","label":"Alpha1","community":"A"},{"id":"b1","label":"Beta1","community":"B"},{"id":"c1","label":"Gamma1","community":"C"}],"links":[{"source":"a1","target":"b1","relation":"calls","confidence":"EXTRACTED"}]}"#;

    fn focus_ab() -> FocusState {
        FocusState {
            a: "A".to_string(),
            b: "B".to_string(),
        }
    }

    fn node_by_id<'a>(model: &'a seam_core::Model, id: &str) -> &'a seam_core::Node {
        &model.graph[*model.index.get(id).expect("node must exist in fixture")]
    }

    #[test]
    fn node_label_matches_is_case_insensitive_substring() {
        let model = model_from(NODE_SEARCH_FIXTURE);
        let a1 = node_by_id(&model, "a1");
        assert!(node_label_matches(a1, "alpha"));
        assert!(node_label_matches(a1, "ALPHA1"));
        assert!(!node_label_matches(a1, "zzz"));
    }

    /// An empty query names no node -- the list-level behaviour `find_nodes`
    /// needs, unobservable from `matches` (which early-returns before ever
    /// reaching this predicate, finding 1). The contrast is asserted
    /// alongside it: `matches` still returns true for an empty query.
    #[test]
    fn node_label_matches_rejects_an_empty_query() {
        let model = model_from(NODE_SEARCH_FIXTURE);
        let a1 = node_by_id(&model, "a1");
        assert!(!node_label_matches(a1, ""));

        let s = seam("A", "B");
        assert!(matches(&model, &s, ""));
    }

    #[test]
    fn find_nodes_returns_a_matching_node_inside_the_focused_pair() {
        let model = model_from(NODE_SEARCH_FIXTURE);
        let focus = focus_ab();
        let hits = find_nodes(
            &model,
            Some(&focus),
            &std::collections::HashSet::new(),
            "alpha",
        );
        assert_eq!(hits.shown.len(), 1);
        assert_eq!(hits.shown[0].id, "a1");
    }

    /// The D-02 test: with A and B focused, a query exactly matching the
    /// C-community node's label returns zero hits, proven against a real
    /// `Model` plus a real `FocusState`, not a bare string comparison.
    #[test]
    fn find_nodes_omits_a_matching_node_outside_the_focused_pair() {
        let model = model_from(NODE_SEARCH_FIXTURE);
        let focus = focus_ab();
        let hits = find_nodes(
            &model,
            Some(&focus),
            &std::collections::HashSet::new(),
            "gamma1",
        );
        assert!(
            hits.shown.is_empty(),
            "a C-community node must not be findable while A/B are focused"
        );
    }

    #[test]
    fn find_nodes_with_no_focus_searches_the_whole_model() {
        let model = model_from(NODE_SEARCH_FIXTURE);
        let hits = find_nodes(&model, None, &std::collections::HashSet::new(), "gamma1");
        assert_eq!(hits.shown.len(), 1);
        assert_eq!(hits.shown[0].id, "c1");
    }

    /// The anti-drift oracle (DP-NOP-05): `find_nodes`'s result set for a
    /// query matching nodes on BOTH sides of the focus boundary must equal
    /// exactly the intersection of the real `build_graph` output with the
    /// label-matching ids -- not a re-derived approximation of either.
    #[test]
    fn find_nodes_results_are_exactly_the_matching_subset_of_what_build_graph_renders() {
        let model = model_from(NODE_SEARCH_FIXTURE);
        let focus = focus_ab();
        let forced = std::collections::HashSet::new();

        let rendered_ids: std::collections::HashSet<String> =
            crate::graph_view::build_graph(&model, Some(&focus), &forced)
                .nodes_iter()
                .map(|(_, n)| n.payload().id.clone())
                .collect();
        assert!(
            rendered_ids.len() < model.graph.node_count(),
            "guard: the rendered set must be a strict subset of the model's nodes, or this test \
             passes vacuously"
        );

        let query = "1";
        let label_matching_ids: std::collections::HashSet<String> = model
            .graph
            .node_indices()
            .filter(|&idx| node_label_matches(&model.graph[idx], query))
            .map(|idx| model.graph[idx].id.clone())
            .collect();
        assert!(
            label_matching_ids.contains("a1") && label_matching_ids.contains("c1"),
            "guard: the query must match nodes on both sides of the focus boundary"
        );

        let expected: std::collections::HashSet<String> = rendered_ids
            .intersection(&label_matching_ids)
            .cloned()
            .collect();

        let hits = find_nodes(&model, Some(&focus), &forced, query);
        let hit_ids: std::collections::HashSet<String> =
            hits.shown.iter().map(|h| h.id.clone()).collect();
        assert_eq!(hit_ids, expected);
    }

    // ============================================================
    // quick-260926-nop Task 2: the cap (DP-NOP-04). `MAX_NODE_RESULTS` does
    // not exist in production code yet -- this is the RED phase.
    // ============================================================

    /// Ten matching in-scope nodes, one community, no focus needed -- purely
    /// to exercise the cap.
    const TEN_NODE_FIXTURE: &str = r#"{"nodes":[{"id":"n0","label":"Item0","community":"A"},{"id":"n1","label":"Item1","community":"A"},{"id":"n2","label":"Item2","community":"A"},{"id":"n3","label":"Item3","community":"A"},{"id":"n4","label":"Item4","community":"A"},{"id":"n5","label":"Item5","community":"A"},{"id":"n6","label":"Item6","community":"A"},{"id":"n7","label":"Item7","community":"A"},{"id":"n8","label":"Item8","community":"A"},{"id":"n9","label":"Item9","community":"A"}],"links":[]}"#;

    #[test]
    fn find_nodes_caps_the_visible_results_and_reports_the_remainder() {
        let model = model_from(TEN_NODE_FIXTURE);
        let hits = find_nodes(&model, None, &std::collections::HashSet::new(), "item");
        assert_eq!(hits.shown.len(), MAX_NODE_RESULTS);
        assert_eq!(hits.hidden, 10 - MAX_NODE_RESULTS);

        let expected_labels: Vec<String> =
            (0..MAX_NODE_RESULTS).map(|i| format!("Item{i}")).collect();
        let shown_labels: Vec<String> = hits.shown.iter().map(|h| h.label.clone()).collect();
        assert_eq!(
            shown_labels, expected_labels,
            "shown must still be deterministically ordered"
        );
    }

    #[test]
    fn find_nodes_reports_no_remainder_when_everything_fits() {
        let model = model_from(NODE_SEARCH_FIXTURE);
        let hits = find_nodes(&model, None, &std::collections::HashSet::new(), "1");
        assert_eq!(hits.hidden, 0);
        assert_eq!(hits.shown.len(), 3);
    }
}
