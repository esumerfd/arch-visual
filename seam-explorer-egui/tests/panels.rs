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
use seam_explorer_egui::app::{Banner, BannerKind, SeamExplorerApp};
use seam_explorer_egui::panels::{banner, detail, legend, seam_list};

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

// ============================================================
// SEAM-03: legend + GRAPH-02: banner (Plan 02 Task 3)
// ============================================================

/// The legend renders with no graph loaded and with a graph loaded -- it is
/// never conditional on load state.
#[test]
fn legend_always_visible() {
    let mut no_graph_harness = ui_harness(|ui| {
        legend::show(ui);
    });
    no_graph_harness.run();
    no_graph_harness.get_by_label("Clean");
    no_graph_harness.get_by_label("Watch");
    no_graph_harness.get_by_label("Leaky");

    let mut app = app_with_seams(&[("a", "b", 1)]);
    let mut loaded_harness = ui_harness(|ui| {
        seam_list::show(ui, &mut app);
        legend::show(ui);
    });
    loaded_harness.run();
    loaded_harness.get_by_label("Clean");
    loaded_harness.get_by_label("Watch");
    loaded_harness.get_by_label("Leaky");
}

/// Exactly three legend entries -- no fourth entry for the never-constructed
/// `Verdict::Utility`.
#[test]
fn legend_has_exactly_three_entries() {
    let mut harness = ui_harness(|ui| {
        legend::show(ui);
    });
    harness.run();

    harness.get_by_label("Clean");
    harness.get_by_label("Watch");
    harness.get_by_label("Leaky");
    assert!(
        harness.query_by_label("Utility").is_none(),
        "no fourth legend entry for the never-constructed Utility verdict"
    );
}

fn warning_banner(n: usize) -> Banner {
    let plural = if n == 1 { "" } else { "s" };
    Banner {
        kind: BannerKind::Warning,
        heading: "Some edges were dropped".to_string(),
        body: format!(
            "{n} edge{plural} referenced a component id that isn't in this graph, so they were skipped. Everything else loaded normally — seam counts below reflect only the valid edges."
        ),
    }
}

/// A dropped-edge `Banner` renders the verbatim heading and a body whose
/// singular/plural wording is correct for n==1 and n==2.
#[test]
fn banner_warning_variant() {
    let singular = warning_banner(1);
    let mut singular_harness = ui_harness(|ui| {
        banner::show(ui, &singular);
    });
    singular_harness.run();
    singular_harness.get_by_label("Some edges were dropped");
    singular_harness.get_by_label_contains("1 edge referenced");

    let plural = warning_banner(2);
    let mut plural_harness = ui_harness(|ui| {
        banner::show(ui, &plural);
    });
    plural_harness.run();
    plural_harness.get_by_label("Some edges were dropped");
    plural_harness.get_by_label_contains("2 edges referenced");
}

/// A fatal `Banner` renders the verbatim heading with the interpolated
/// reason.
#[test]
fn banner_error_variant() {
    let b = Banner {
        kind: BannerKind::Error,
        heading: "Couldn't load this file".to_string(),
        body: "This doesn't look like a Graphify graph.json export — bad reason. Choose a different file and try again.".to_string(),
    };
    let mut harness = ui_harness(|ui| {
        banner::show(ui, &b);
    });
    harness.run();

    harness.get_by_label("Couldn't load this file");
    harness.get_by_label_contains("bad reason");
}

/// With `app.banner` as `None`, no banner nodes appear.
#[test]
fn banner_absent_renders_nothing() {
    let mut app = SeamExplorerApp::default();
    let mut harness = ui_harness(|ui| {
        seam_list::show(ui, &mut app);
    });
    harness.run();

    assert!(harness.query_by_label("Warning").is_none());
    assert!(harness.query_by_label("Load failed").is_none());
}

// ============================================================
// NAV-01: search-to-jump (Plan 04 Task 3)
// ============================================================

/// A query matching no seam name and no member node label renders the
/// verbatim no-match message from the Copywriting Contract, with the query
/// interpolated inside double quotes exactly as specified.
#[test]
fn search_no_match_message() {
    let mut app = app_with_seams(&[("a", "b", 1)]);
    app.search_query = "zzz_no_such_component".to_string();
    let mut harness = ui_harness(|ui| {
        seam_list::show(ui, &mut app);
    });
    harness.run();

    harness.get_by_label_contains("No component or seam matches");
    harness.get_by_label_contains("\"zzz_no_such_component\"");
}

/// A 500-character query is accepted and rendered without truncation or
/// panic -- the native single-line `TextEdit` handles it internally, no
/// custom long-input handling exists.
#[test]
fn search_long_input_is_accepted() {
    let long_query = "x".repeat(500);
    let mut app = app_with_seams(&[("a", "b", 1)]);
    app.search_query = long_query.clone();
    {
        let mut harness = ui_harness(|ui| {
            seam_list::show(ui, &mut app);
        });
        harness.run(); // must not panic
    }

    assert_eq!(app.search_query.len(), 500, "query must not be truncated");
    assert_eq!(app.search_query, long_query);
}

// ============================================================
// TRACE-02: trace result panel (Plan 05 Task 2)
// ============================================================

/// Builds a Graphify-shaped `graph.json` from explicit directed edges
/// `(source_id, source_community, target_id, target_community)`, ingests +
/// finalizes SCC, runs `seam_core::trace_path(from, to)`, and returns a
/// `SeamExplorerApp` with `model`/`seams`/`trace` populated exactly as a
/// real completed drag-to-trace gesture would leave them. Each node's
/// `label` is set to its own `id` so hop assertions can match on either.
fn app_with_trace(edges: &[(&str, &str, &str, &str)], from: &str, to: &str) -> SeamExplorerApp {
    let mut node_communities: std::collections::HashMap<&str, &str> =
        std::collections::HashMap::new();
    for (sid, scomm, tid, tcomm) in edges {
        node_communities.insert(sid, scomm);
        node_communities.insert(tid, tcomm);
    }
    let nodes: Vec<String> = node_communities
        .iter()
        .map(|(id, comm)| format!(r#"{{"id":"{id}","label":"{id}","community":"{comm}"}}"#))
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
    let path = seam_core::trace_path(&outcome.model, from, to);

    let mut app = SeamExplorerApp {
        model: Some(outcome.model),
        seams: outcome.seams,
        ..Default::default()
    };
    app.trace = Some(seam_explorer_egui::trace::TraceResult {
        from: from.to_string(),
        to: to.to_string(),
        path,
    });
    app
}

/// A multi-hop trace (three communities, two seam crossings) renders every
/// hop and every crossed seam.
#[test]
fn trace_result_lists_hops_and_seams() {
    let mut app = app_with_trace(
        &[("a1", "A", "b1", "B"), ("b1", "B", "c1", "C")],
        "a1",
        "c1",
    );
    let mut harness = ui_harness(|ui| {
        detail::show(ui, &mut app);
    });
    harness.run();

    // "a1"/"c1" also appear inside the "a1 -> c1" heading, so use
    // `get_all_by_label_contains` (allows >=1 match) rather than
    // `get_by_label_contains` (panics on ambiguity) for those two; "b1"
    // (the middle hop) is unambiguous.
    assert!(harness.get_all_by_label_contains("a1").next().is_some());
    harness.get_by_label_contains("b1");
    assert!(harness.get_all_by_label_contains("c1").next().is_some());
    harness.get_by_label_contains("A \u{2192} B");
    harness.get_by_label_contains("B \u{2192} C");
}

/// With no directed path between two disconnected components, the verbatim
/// no-path message renders with both endpoint labels interpolated.
/// (`no_path_message_color_is_muted_not_destructive`, `src/panels/detail.rs`,
/// covers the color half of this claim -- accesskit carries no rendered
/// foreground-color data for `egui_kittest::Harness` to query.)
#[test]
fn trace_no_path_message() {
    // Two nodes with no edge between them at all -> no directed path.
    let json = r#"{"nodes":[{"id":"a1","label":"PaymentService","community":"A"},{"id":"b1","label":"OrderService","community":"B"}],"links":[]}"#;
    let outcome =
        seam_explorer_egui::load::read_and_ingest(json).expect("fixture must ingest cleanly");
    let path = seam_core::trace_path(&outcome.model, "a1", "b1");
    assert!(path.is_none(), "fixture must have no directed path");

    let mut app = SeamExplorerApp {
        model: Some(outcome.model),
        seams: outcome.seams,
        ..Default::default()
    };
    app.trace = Some(seam_explorer_egui::trace::TraceResult {
        from: "a1".to_string(),
        to: "b1".to_string(),
        path: None,
    });

    let mut harness = ui_harness(|ui| {
        detail::show(ui, &mut app);
    });
    harness.run();

    harness.get_by_label_contains("No directed call path from PaymentService to OrderService");
    harness.get_by_label_contains("the dependency runs the other way");
}

/// A trace that stays within one community (both endpoints in the same
/// community, zero seams crossed) renders the distinct positive
/// zero-crossing message.
#[test]
fn trace_zero_crossing_message() {
    let mut app = app_with_trace(&[("a1", "A", "a2", "A")], "a1", "a2");
    let mut harness = ui_harness(|ui| {
        detail::show(ui, &mut app);
    });
    harness.run();

    harness.get_by_label_contains("Stays within one community");
    harness.get_by_label_contains("no seam crossed");
}

/// Zero crossings renders the positive message (not a list); one and many
/// crossings render the identical list structure, with only the eyebrow's
/// count changing.
#[test]
fn crossed_seams_zero_one_many() {
    let zero = app_with_trace(&[("a1", "A", "a2", "A")], "a1", "a2");
    let mut zero = zero;
    let mut zero_harness = ui_harness(|ui| {
        detail::show(ui, &mut zero);
    });
    zero_harness.run();
    zero_harness.get_by_label_contains("Stays within one community");
    assert!(
        zero_harness
            .query_by_label_contains("Crossed seams")
            .is_none(),
        "zero crossings must not render the list eyebrow"
    );

    let mut one = app_with_trace(&[("a1", "A", "b1", "B")], "a1", "b1");
    let mut one_harness = ui_harness(|ui| {
        detail::show(ui, &mut one);
    });
    one_harness.run();
    one_harness.get_by_label_contains("Crossed seams (1)");

    let mut many = app_with_trace(
        &[
            ("a1", "A", "b1", "B"),
            ("b1", "B", "c1", "C"),
            ("c1", "C", "d1", "D"),
        ],
        "a1",
        "d1",
    );
    let mut many_harness = ui_harness(|ui| {
        detail::show(ui, &mut many);
    });
    many_harness.run();
    many_harness.get_by_label_contains("Crossed seams (3)");
}

/// Clicking a crossed-seam entry focuses that seam -- routed through the
/// same `seam_list::select_seam` write site a seam-row click uses.
#[test]
fn crossed_seam_entry_is_clickable() {
    let app = app_with_trace(
        &[("a1", "A", "b1", "B"), ("b1", "B", "c1", "C")],
        "a1",
        "c1",
    );
    let mut harness = egui_kittest::Harness::new_ui_state(
        |ui, app: &mut SeamExplorerApp| {
            detail::show(ui, app);
        },
        app,
    );
    harness.run();

    harness.get_by_label_contains("A \u{2192} B").click();
    harness.run();

    let focus = harness
        .state()
        .focus
        .clone()
        .expect("clicking a crossed-seam entry must set focus");
    assert_eq!(focus.a, "A");
    assert_eq!(focus.b, "B");
    assert!(
        harness.state().trace.is_none(),
        "focusing a seam clears the trace result (select_seam's mutual-exclusivity write)"
    );
}
