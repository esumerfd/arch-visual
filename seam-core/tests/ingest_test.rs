//! Failing (RED) tests for `seam_core::ingest::from_json`. See
//! 01-01-PLAN.md Task 2's `<behavior>` block for the exact contract these
//! assert. Implementation lands in Task 3 (GREEN).

use seam_core::{
    from_json, normalize_source_file, parse_source_line, resolve_community_names, SeamCoreError,
    STRUCTURAL_RELATIONS,
};

const REAL_GRAPH: &str = include_str!("fixtures/graph.json");
const CLEAN_FIXTURE: &str = include_str!("fixtures/clean.json");
const MALFORMED_FIXTURE: &str = include_str!("fixtures/malformed.json");
const COMMUNITY_NAMES_FIXTURE: &str = include_str!("fixtures/community_names.json");
const SOURCE_PATHS_FIXTURE: &str = include_str!("fixtures/source_paths.json");

#[test]
fn structural_relations_allowlist_matches_locked_decision_d01() {
    assert_eq!(STRUCTURAL_RELATIONS.len(), 5);
    for r in ["calls", "references", "method", "implements", "imports_from"] {
        assert!(
            STRUCTURAL_RELATIONS.contains(&r),
            "D-01 allow-list missing relation: {r}"
        );
    }
}

#[test]
fn ingests_real_sample_with_exact_filtered_counts_and_zero_warnings() {
    let result = from_json(REAL_GRAPH).expect("real sample/graph.json must parse successfully");
    assert_eq!(
        result.model.graph.edge_count(),
        3212,
        "structural relations {{calls,references,method,implements,imports_from}} at \
         confidence EXTRACTED only must total exactly 3212 edges"
    );
    assert_eq!(result.model.graph.node_count(), 1594);
    assert!(
        result.warnings.is_empty(),
        "real sample/graph.json has zero dangling edges — warnings must be empty"
    );
}

#[test]
fn drops_contains_semantic_and_inferred_edges_keeps_structural_extracted() {
    let result = from_json(CLEAN_FIXTURE).expect("clean fixture must parse");
    // clean.json has 7 structural+EXTRACTED edges (including one same-community
    // edge a1->a2) and 3 edges that must be dropped: one `contains` (D-02),
    // one `shares_data_with` (D-03), one INFERRED `calls` (D-04).
    assert_eq!(
        result.model.graph.edge_count(),
        7,
        "only structural-relation, EXTRACTED-confidence edges should be added to the graph"
    );
}

#[test]
fn malformed_fixture_drops_dangling_edge_and_records_non_empty_warning() {
    let result =
        from_json(MALFORMED_FIXTURE).expect("malformed fixture must NOT error — drop, don't fail");
    assert!(
        !result.warnings.is_empty(),
        "an edge whose target id is absent from nodes[] must produce a warning, never be silently dropped"
    );
    let warning = result
        .warnings
        .iter()
        .find(|w| w.target == "b1_ghost")
        .expect("warning must name the offending dropped edge's target");
    assert_eq!(warning.source, "a1");
}

#[test]
fn fatal_invalid_json_is_not_ok_and_is_distinguishable_from_missing_array() {
    let err = from_json("{ not json").expect_err(
        "non-JSON input must be a fatal Err, never an Ok — the file isn't parseable at all",
    );
    assert!(
        matches!(err, SeamCoreError::Parse(_)),
        "invalid JSON must produce the Parse variant (a syntax failure), distinct from \
         MissingArray (a structural failure on otherwise-valid JSON); got {err:?}"
    );
}

#[test]
fn fatal_missing_nodes_array_is_not_ok_with_an_empty_graph() {
    let err = from_json(r#"{"links": []}"#).expect_err(
        "a document with no `nodes` array must be a fatal Err naming the missing array — \
         never a silently-empty-graph Ok",
    );
    match err {
        SeamCoreError::MissingArray(name) => assert_eq!(name, "nodes"),
        other => panic!("expected SeamCoreError::MissingArray(\"nodes\"), got {other:?}"),
    }
}

#[test]
fn fatal_missing_links_array_is_not_ok_with_an_empty_graph() {
    let err = from_json(r#"{"nodes": []}"#).expect_err(
        "a document with no `links` array must be a fatal Err naming the missing array — \
         never a silently-empty-graph Ok",
    );
    match err {
        SeamCoreError::MissingArray(name) => assert_eq!(name, "links"),
        other => panic!("expected SeamCoreError::MissingArray(\"links\"), got {other:?}"),
    }
}

#[test]
fn fatal_missing_array_never_produces_ok() {
    // Regression guard for the exact anti-pattern this task rules out: a
    // missing `nodes`/`links` array must never resolve to Ok(IngestResult)
    // with an empty graph.
    assert!(from_json(r#"{}"#).is_err(), "a doc with neither array present must be fatal");
}

// ---------------------------------------------------------------------
// resolve_community_names: pure conflict-resolution function (05-09
// Task 1). Driven entirely by hand-built (community, name) pairs — no
// graph/fixture required, per DP-09-01/DP-09-02.
// ---------------------------------------------------------------------

#[test]
fn resolves_a_consistent_community_name() {
    let entries = vec![
        ("1".to_string(), Some("Payments".to_string())),
        ("1".to_string(), Some("Payments".to_string())),
    ];
    let resolved = resolve_community_names(&entries);
    assert_eq!(resolved.get("1").map(String::as_str), Some("Payments"));
}

#[test]
fn resolves_a_disagreeing_community_to_the_majority_name() {
    let entries = vec![
        ("2".to_string(), Some("Orders".to_string())),
        ("2".to_string(), Some("Orders".to_string())),
        ("2".to_string(), Some("Legacy".to_string())),
    ];
    let resolved = resolve_community_names(&entries);
    assert_eq!(
        resolved.get("2").map(String::as_str),
        Some("Orders"),
        "majority-wins (DP-09-01): two Orders outvote one Legacy"
    );
}

#[test]
fn resolves_a_tied_community_deterministically_by_lexical_order() {
    // Repeated to guard against a HashMap-iteration-order dependency
    // passing by luck on a single run.
    for _ in 0..20 {
        let entries = vec![
            ("3".to_string(), Some("Alpha".to_string())),
            ("3".to_string(), Some("Beta".to_string())),
        ];
        let resolved = resolve_community_names(&entries);
        assert_eq!(
            resolved.get("3").map(String::as_str),
            Some("Alpha"),
            "exact tie must break to the lexicographically smallest name (DP-09-01)"
        );
    }
}

#[test]
fn a_community_with_no_name_gets_no_entry() {
    let entries = vec![("4".to_string(), None)];
    let resolved = resolve_community_names(&entries);
    assert!(
        !resolved.contains_key("4"),
        "a community whose only node carries no name must have no map entry"
    );
}

#[test]
fn a_blank_community_name_never_wins() {
    let entries = vec![
        ("5".to_string(), Some("".to_string())),
        ("5".to_string(), Some("   ".to_string())),
    ];
    let resolved = resolve_community_names(&entries);
    assert!(
        !resolved.contains_key("5"),
        "empty and whitespace-only names must never be candidates (DP-09-02)"
    );
}

// ---------------------------------------------------------------------
// Ingest-time capture + Model::community_label (05-09 Task 2).
// ---------------------------------------------------------------------

#[test]
fn node_carries_its_community_name() {
    let result = from_json(COMMUNITY_NAMES_FIXTURE).expect("community_names.json must parse");
    let model = result.model;

    let n1 = &model.graph[model.index["n1"]];
    assert_eq!(n1.community_name.as_deref(), Some("Payments"));

    let n8 = &model.graph[model.index["n8"]];
    assert_eq!(
        n8.community_name, None,
        "n8's community_name key is entirely absent from the fixture"
    );
}

#[test]
fn model_names_every_named_community() {
    let result = from_json(COMMUNITY_NAMES_FIXTURE).expect("community_names.json must parse");
    let names = result.model.community_names;

    assert!(names.contains_key("1"));
    assert!(names.contains_key("2"));
    assert!(names.contains_key("3"));
    assert!(!names.contains_key("4"), "community 4 has no usable name");
    assert!(!names.contains_key("5"), "community 5's only name is blank");
}

#[test]
fn community_label_returns_the_resolved_name() {
    let result = from_json(COMMUNITY_NAMES_FIXTURE).expect("community_names.json must parse");
    let model = result.model;

    assert_eq!(model.community_label(&"1".to_string()), "Payments");
    assert_eq!(model.community_label(&"2".to_string()), "Orders");
}

#[test]
fn community_label_falls_back_to_the_raw_id() {
    let result = from_json(COMMUNITY_NAMES_FIXTURE).expect("community_names.json must parse");
    let model = result.model;

    assert_eq!(
        model.community_label(&"4".to_string()),
        "4",
        "community 4 has no usable name, so the label is the raw id, never empty/placeholder"
    );
    assert_eq!(
        model.community_label(&"5".to_string()),
        "5",
        "community 5's only name is blank, so the label is the raw id"
    );
}

#[test]
fn community_label_of_an_unknown_community_is_the_id_itself() {
    let result = from_json(COMMUNITY_NAMES_FIXTURE).expect("community_names.json must parse");
    let model = result.model;

    assert_eq!(model.community_label(&"999".to_string()), "999");
}

#[test]
fn graphs_without_community_name_still_ingest_unchanged() {
    // Non-regression guard: fixtures/clean.json has no community_name field
    // anywhere. Ingesting it must be byte-for-byte the same as today.
    let result = from_json(CLEAN_FIXTURE).expect("clean fixture must parse");
    assert_eq!(result.model.graph.edge_count(), 7);
    assert!(result.model.community_names.is_empty());
    assert!(result.warnings.is_empty());
}

// ---------------------------------------------------------------------
// 05-20: Node::source_file / Node::source_line, parse_source_line,
// normalize_source_file. See 05-20-PLAN.md <design_decision> for the
// four-step parse algorithm and the blank-is-None rule this pins.
// ---------------------------------------------------------------------

#[test]
fn parse_source_line_reads_the_real_l_prefixed_shape() {
    assert_eq!(parse_source_line(Some("L42")), Some(42), "L42");
    assert_eq!(parse_source_line(Some("L1")), Some(1), "L1");
    assert_eq!(parse_source_line(Some("L1004")), Some(1004), "L1004");
}

#[test]
fn parse_source_line_accepts_a_bare_number_and_a_trailing_column() {
    assert_eq!(parse_source_line(Some("7")), Some(7), "bare number 7");
    assert_eq!(
        parse_source_line(Some("L42:7")),
        Some(42),
        "trailing column L42:7 must yield the line, not the column"
    );
    assert_eq!(
        parse_source_line(Some(" L42 ")),
        Some(42),
        "surrounding whitespace must be trimmed"
    );
}

#[test]
fn parse_source_line_rejects_every_unusable_shape_without_panicking() {
    for input in ["", "   ", "L", "none", "Lx", "L99999999999999999999"] {
        assert_eq!(
            parse_source_line(Some(input)),
            None,
            "expected None for unusable shape {input:?}, must not panic"
        );
    }
    assert_eq!(
        parse_source_line(None),
        None,
        "an absent source_location must also yield None"
    );
}

#[test]
fn normalize_source_file_treats_blank_as_absent() {
    assert_eq!(
        normalize_source_file(Some("src/a.rs")),
        Some("src/a.rs".to_string()),
        "a normal path must survive unchanged"
    );
    assert_eq!(normalize_source_file(Some("")), None, "empty string must normalize to None");
    assert_eq!(
        normalize_source_file(Some("   ")),
        None,
        "whitespace-only string must normalize to None"
    );
    assert_eq!(normalize_source_file(None), None, "an absent key must stay None");
    assert_eq!(
        normalize_source_file(Some("src/my dir/a.rs")),
        Some("src/my dir/a.rs".to_string()),
        "internal spaces must survive -- trimming is end-only, never a filter"
    );
}

#[test]
fn nodes_carry_their_source_file_and_line() {
    let result = from_json(SOURCE_PATHS_FIXTURE).expect("source_paths.json must parse");
    let model = result.model;

    let cases: &[(&str, Option<&str>, Option<u32>)] = &[
        ("a1", Some("src/auth/login.rs"), Some(42)),
        ("a2", Some("src/auth/session.rs"), Some(7)),
        ("b1", None, None),
        ("b2", None, None),
        ("c1", Some("src/db/pool.rs"), None),
        ("c2", Some("src/db/migrate.rs"), None),
    ];

    for (id, expected_file, expected_line) in cases {
        let node = &model.graph[model.index[*id]];
        assert_eq!(
            node.source_file.as_deref(),
            *expected_file,
            "node {id}: source_file mismatch (actual source_file={:?}, source_line={:?})",
            node.source_file,
            node.source_line
        );
        assert_eq!(
            node.source_line, *expected_line,
            "node {id}: source_line mismatch (actual source_file={:?}, source_line={:?})",
            node.source_file, node.source_line
        );
    }
}

#[test]
fn the_real_fixture_populates_source_locations_at_the_counted_rate() {
    // Counted directly against seam-core/tests/fixtures/graph.json during
    // planning, not estimated. A change that quietly stops parsing
    // L-prefixed values would still pass every hand-written case above --
    // this is the test that would actually catch it.
    let result = from_json(REAL_GRAPH).expect("real sample/graph.json must parse successfully");
    let with_file = result
        .model
        .graph
        .node_weights()
        .filter(|n| n.source_file.is_some())
        .count();
    let with_line = result
        .model
        .graph
        .node_weights()
        .filter(|n| n.source_line.is_some())
        .count();
    assert_eq!(with_file, 1594, "expected 1594 nodes with a usable source_file");
    assert_eq!(with_line, 1553, "expected 1553 nodes with a parseable source_line");
}

#[test]
fn graphs_without_source_fields_still_ingest_unchanged() {
    // Additive-change proof: fixtures/clean.json has neither source_file nor
    // source_location on any node. Ingesting it must be unchanged in shape,
    // and every node's new fields must be None.
    let result = from_json(CLEAN_FIXTURE).expect("clean fixture must parse");
    assert_eq!(result.model.graph.node_count(), 6);
    assert_eq!(result.model.graph.edge_count(), 7);
    for node in result.model.graph.node_weights() {
        assert_eq!(
            node.source_file, None,
            "node {}: source_file must be None when the key is absent from clean.json",
            node.id
        );
        assert_eq!(
            node.source_line, None,
            "node {}: source_line must be None when the key is absent from clean.json",
            node.id
        );
    }
}
