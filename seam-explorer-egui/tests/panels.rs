//! Shared `egui_kittest` harness entry point (Wave 0 validation
//! infrastructure). Plan 02 and Plan 05 extend this file with real panel
//! snapshot cases (SEAM-01/02/03, TRACE-02 onboarding); this task only
//! proves the harness itself boots and can query a rendered widget before
//! any of that real coverage is trusted.
//!
//! Pin note (RESEARCH.md Pitfall 6): `egui_kittest` is pinned to the same
//! `0.35.x` line as this crate's `egui`/`eframe` — a `0.36` kittest would
//! not type-match this crate's `egui::Context`.

use egui_kittest::kittest::{NodeT, Queryable};
use egui_kittest::Harness;
use seam_explorer_egui::app::SeamExplorerApp;
use seam_explorer_egui::panels::{detail, seam_list};

/// Builds a stateless [`Harness`] over a closure taking `&mut egui::Ui`.
/// Kept public within the test crate (not `pub(crate)`, integration test
/// binaries are separate crates) so later plans add panel-rendering cases
/// without restructuring this helper.
pub fn ui_harness<'a>(app: impl FnMut(&mut egui::Ui) + 'a) -> Harness<'a> {
    Harness::new_ui(app)
}

/// Wave 0 gate: an `egui_kittest::Harness` constructs, renders one frame of
/// a trivial `ui.label(...)`, and the accessibility-tree query finds that
/// label. Proves the harness works before Plan 02 writes real panel
/// snapshots against it.
#[test]
fn harness_boots() {
    let mut harness = ui_harness(|ui| {
        ui.label("Load a graph.json to begin.");
    });
    harness.run();

    harness.get_by_label("Load a graph.json to begin.");
}

// ============================================================
// SEAM-01: seam_list panel (Plan 02 Task 1)
// ============================================================

/// Builds a minimal Graphify-shaped `graph.json` with one community pair per
/// `(a, b, crossing_count)` triple, `crossing_count` parallel edges each.
/// `seam_core::detect` ranks the resulting seams by crossing count
/// descending regardless of the order pairs are listed here.
fn build_graph_json(pairs: &[(&str, &str, usize)]) -> String {
    let mut nodes = Vec::new();
    let mut links = Vec::new();
    for (a, b, count) in pairs {
        let a_id = format!("{a}1");
        let b_id = format!("{b}1");
        nodes.push(format!(r#"{{"id":"{a_id}","community":"{a}"}}"#));
        nodes.push(format!(r#"{{"id":"{b_id}","community":"{b}"}}"#));
        for _ in 0..*count {
            links.push(format!(
                r#"{{"source":"{a_id}","target":"{b_id}","relation":"calls","confidence":"EXTRACTED"}}"#
            ));
        }
    }
    format!(
        r#"{{"nodes":[{}],"links":[{}]}}"#,
        nodes.join(","),
        links.join(",")
    )
}

/// Ingests `build_graph_json(pairs)` into a fresh `SeamExplorerApp` with
/// `model`/`seams` populated exactly like a real Load-graph action would.
fn app_with_seams(pairs: &[(&str, &str, usize)]) -> SeamExplorerApp {
    let json = build_graph_json(pairs);
    let outcome =
        seam_explorer_egui::load::read_and_ingest(&json).expect("fixture must ingest cleanly");
    SeamExplorerApp {
        model: Some(outcome.model),
        seams: outcome.seams,
        ..Default::default()
    }
}

/// With no graph loaded, the seam list renders the verbatim empty-state
/// heading and body from the Copywriting Contract.
#[test]
fn seam_list_empty_state() {
    let mut app = SeamExplorerApp::default();
    let mut harness = ui_harness(|ui| {
        seam_list::show(ui, &mut app);
    });
    harness.run();

    harness.get_by_label("No graph loaded yet");
    harness.get_by_label_contains("ranked by crossing count");
}

/// Three seams with crossing counts 9, 2, 5 render top-to-bottom in
/// descending crossing-count order -- the order `seam_core::detect` already
/// produces, which `seam_list::show` must not re-sort or disturb.
#[test]
fn seam_list_ranked_order() {
    let mut app = app_with_seams(&[("a", "b", 9), ("c", "d", 2), ("e", "f", 5)]);
    let mut harness = ui_harness(|ui| {
        seam_list::show(ui, &mut app);
    });
    harness.run();

    let counts: Vec<String> = harness
        .get_all_by_label_contains("\u{d7}")
        .filter_map(|n| {
            let node = n.accesskit_node();
            node.label().or_else(|| node.value())
        })
        .collect();
    assert_eq!(counts, vec!["9\u{d7}", "5\u{d7}", "2\u{d7}"]);
}

/// The seam list renders the same static section heading and one row per
/// seam at 0, 1, and 3 items -- no copy branches on count.
#[test]
fn seam_list_zero_one_many() {
    let cases: [&[(&str, &str, usize)]; 3] = [
        &[],
        &[("a", "b", 1)],
        &[("a", "b", 3), ("c", "d", 2), ("e", "f", 1)],
    ];

    for pairs in cases {
        let mut app = app_with_seams(pairs);
        let mut harness = ui_harness(|ui| {
            seam_list::show(ui, &mut app);
        });
        harness.run();

        harness.get_by_label_contains("ranked by crossings");
        let row_count = harness.query_all_by_label_contains("\u{d7}").count();
        assert_eq!(
            row_count,
            pairs.len(),
            "row count must match seam count for {pairs:?}"
        );
    }
}

// ============================================================
// SEAM-02: detail panel (Plan 02 Task 2)
// ============================================================

/// Builds a Graphify-shaped `graph.json` from explicit directed edges
/// `(source_id, source_community, target_id, target_community)`, then
/// ingests + finalizes SCC and returns `seam_core::seam_detail(a, b)`.
fn seam_detail_fixture(
    edges: &[(&str, &str, &str, &str)],
    a: &str,
    b: &str,
) -> seam_core::SeamDetail {
    let mut node_communities: std::collections::HashMap<&str, &str> =
        std::collections::HashMap::new();
    for (sid, scomm, tid, tcomm) in edges {
        node_communities.insert(sid, scomm);
        node_communities.insert(tid, tcomm);
    }
    let nodes: Vec<String> = node_communities
        .iter()
        .map(|(id, comm)| format!(r#"{{"id":"{id}","community":"{comm}"}}"#))
        .collect();
    let links: Vec<String> = edges
        .iter()
        .map(|(sid, _, tid, _)| {
            format!(
                r#"{{"source":"{sid}","target":"{tid}","relation":"calls","confidence":"EXTRACTED"}}"#
            )
        })
        .collect();
    let json = format!(
        r#"{{"nodes":[{}],"links":[{}]}}"#,
        nodes.join(","),
        links.join(",")
    );

    let outcome =
        seam_explorer_egui::load::read_and_ingest(&json).expect("fixture must ingest cleanly");
    let scc = outcome.model.scc.as_ref().expect("finalize_scc must run");
    seam_core::seam_detail(&outcome.model, scc, &a.to_string(), &b.to_string())
}

/// With no seam selected, the detail panel renders the verbatim prompt from
/// the Copywriting Contract.
#[test]
fn detail_empty_state() {
    let mut app = SeamExplorerApp::default();
    let mut harness = ui_harness(|ui| {
        detail::show(ui, &mut app);
    });
    harness.run();

    harness.get_by_label_contains("Select a seam to see its bridge components");
}

/// A seam with bidirectional community traffic across two distinct node
/// pairs (no cycle through either pair alone -- Watch verdict) renders both
/// side labels, both bridge chip lists, and both per-direction counts.
#[test]
fn detail_renders_both_sides() {
    let d = seam_detail_fixture(&[("a1", "A", "b1", "B"), ("b2", "B", "a2", "A")], "A", "B");
    let mut app = SeamExplorerApp {
        detail: Some(d),
        ..Default::default()
    };
    let mut harness = ui_harness(|ui| {
        detail::show(ui, &mut app);
    });
    harness.run();

    harness.get_by_label_contains("A / B");
    harness.get_by_label_contains("a1");
    harness.get_by_label_contains("b1");
    harness.get_by_label_contains("a2");
    harness.get_by_label_contains("b2");
    harness.get_by_label_contains("A \u{2192} B");
    harness.get_by_label_contains("B \u{2192} A");
}

/// A Clean fixture (single-direction edge, no cycle) renders "Clean seam";
/// a Leaky fixture (direct two-node cycle) renders "Leaky seam".
#[test]
fn detail_verdict_title() {
    let clean = seam_detail_fixture(&[("a1", "A", "b1", "B")], "A", "B");
    let mut clean_app = SeamExplorerApp {
        detail: Some(clean),
        ..Default::default()
    };
    let mut clean_harness = ui_harness(|ui| {
        detail::show(ui, &mut clean_app);
    });
    clean_harness.run();
    clean_harness.get_by_label("Clean seam");

    let leaky = seam_detail_fixture(&[("a1", "A", "b1", "B"), ("b1", "B", "a1", "A")], "A", "B");
    let mut leaky_app = SeamExplorerApp {
        detail: Some(leaky),
        ..Default::default()
    };
    let mut leaky_harness = ui_harness(|ui| {
        detail::show(ui, &mut leaky_app);
    });
    leaky_harness.run();
    leaky_harness.get_by_label("Leaky seam");
}

/// Bridge sets are structurally derived from crossing edges -- a seam only
/// exists when both sides have at least one bridge node, so both lists must
/// be non-empty on any real fixture.
#[test]
fn detail_bridge_lists_are_never_empty() {
    let d = seam_detail_fixture(&[("a1", "A", "b1", "B")], "A", "B");
    assert!(
        !d.bridges_a.is_empty(),
        "bridges_a must be non-empty by construction"
    );
    assert!(
        !d.bridges_b.is_empty(),
        "bridges_b must be non-empty by construction"
    );
}
