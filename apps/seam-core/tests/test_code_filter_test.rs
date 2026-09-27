//! Behaviour matrix for the hard test-code exclusion filter (quick task
//! 260926-xbl). See 260926-xbl-PLAN.md's `<behavior>` block for the exact
//! contract these pin: D-01 (two independent pattern classes), D-02 (the
//! locked whole-word `Mock`/`Fake`/`Stub`/`Dummy`/`Test` prefix set, no
//! substring matching), D-03 (hard, non-optional, applies to every edge
//! touching an excluded node), DP-XBL-01 (raw label fields, not the
//! constructed `Node.label`), DP-XBL-04 (excluded nodes never vote on a
//! community name), DP-XBL-05 (an exclusion-dropped edge is never reported
//! as a missing-id warning), DP-XBL-06 (path beats name on precedence, and
//! the two counters are disjoint).

use seam_core::{
    classify_test_code, from_json, is_test_path, is_test_symbol_name, TestCodeExcluded,
    TestCodeRule,
};

const REAL_GRAPH: &str = include_str!("fixtures/graph.json");
const TEST_CODE_FIXTURE: &str = include_str!("fixtures/test_code.json");

// ---------------------------------------------------------------------
// is_test_symbol_name: DP-XBL-02's whole-word boundary rule.
// ---------------------------------------------------------------------

#[test]
fn is_test_symbol_name_matches_locked_prefixes_case_insensitively() {
    for name in [
        "MockAgentRuntime",
        "TestHelper",
        "FakeClock",
        "DummyX",
        "StubY",
        "Mock",
        "test_foo",
        "mock_agent",
        "MOCK_AGENT",
        "stub_definition()",
        "fake_claude.sh",
    ] {
        assert!(
            is_test_symbol_name(name),
            "expected {name:?} to match a locked test-name prefix"
        );
    }
}

#[test]
fn is_test_symbol_name_rejects_the_named_false_positives_and_edge_cases() {
    for name in [
        "MockupRenderer",
        "Attestation",
        "ContestEntry",
        "Testament",
        "TESTAMENT",
        "mockagentruntime",
        "Mock2Runtime",
        "LoginService",
        "",
        ".new()",
    ] {
        assert!(
            !is_test_symbol_name(name),
            "expected {name:?} to NOT match (D-02 is whole-word-prefix only, never substring)"
        );
    }
}

// ---------------------------------------------------------------------
// is_test_path: path-directory and filename conventions.
// ---------------------------------------------------------------------

#[test]
fn is_test_path_matches_common_test_directory_and_file_conventions() {
    for path in [
        "tests/login_integration.rs",
        "src/__tests__/foo.ts",
        "spec/models/user_spec.rb",
        "a/Tests/B.cs",
        "src/util/helper_test.go",
        "src/util/helper.test.ts",
        "src/util/helper.spec.ts",
        "pkg/test_helpers.py",
        "src/tests.rs",
        "python/conftest.py",
        "go/testdata/golden.json",
    ] {
        assert!(is_test_path(path), "expected {path:?} to be a test path");
    }
}

#[test]
fn is_test_path_rejects_directory_name_substring_false_positives() {
    for path in [
        "orchestrator/src/handlers/ai_agent.rs",
        "src/contest/entry.rs",
        "src/latest/report.rs",
        "src/testament.rs",
        "src/protest_handler.rs",
        "src/spectrum/color.rs",
    ] {
        assert!(
            !is_test_path(path),
            "expected {path:?} to NOT be a test path (directory match is exact, never substring)"
        );
    }
}

// ---------------------------------------------------------------------
// classify_test_code: precedence + the raw-label reason this exists at all.
// ---------------------------------------------------------------------

#[test]
fn classify_test_code_prefers_path_over_name_when_both_would_fire() {
    // Both the path AND the name would independently match; DP-XBL-06 says
    // the two counters are disjoint and path is checked first.
    let rule = classify_test_code(Some("tests/mock_helper.rs"), Some("MockHelper"), None);
    assert_eq!(rule, Some(TestCodeRule::Path));
}

#[test]
fn classify_test_code_catches_the_production_path_test_name_shape() {
    // The exact MockAgentRuntime shape: a normal production source_file, a
    // test-shaped name. This is the whole reason the name rule exists.
    let rule = classify_test_code(
        Some("orchestrator/src/handlers/ai_agent.rs"),
        Some("MockAgentRuntime"),
        Some("mockagentruntime"),
    );
    assert_eq!(rule, Some(TestCodeRule::Name));
}

#[test]
fn classify_test_code_returns_none_for_a_node_with_neither_signal() {
    let rule = classify_test_code(Some("src/foo.rs"), Some("Foo"), Some("foo"));
    assert_eq!(rule, None);
}

#[test]
fn classify_test_code_reads_the_raw_label_not_the_constructed_node_label() {
    // DP-XBL-01, finding 1: the constructed Node.label prefers norm_label
    // ("mockagentruntime"), which has no word boundary and would NOT match.
    // classify_test_code must be given the raw fields separately and check
    // either one -- a hit on the raw `label` here is what proves this.
    let rule = classify_test_code(
        Some("orchestrator/src/handlers/ai_agent.rs"),
        Some("MockAgentRuntime"),
        Some("mockagentruntime"),
    );
    assert_eq!(
        rule,
        Some(TestCodeRule::Name),
        "a hit on the raw `label` field must exclude, even though norm_label alone would not"
    );

    // Given ONLY norm_label (no raw label) the answer must be None -- this
    // is the test that pins WHY the raw label must be consulted, not just a
    // comment asserting it.
    let rule_norm_only = classify_test_code(
        Some("orchestrator/src/handlers/ai_agent.rs"),
        None,
        Some("mockagentruntime"),
    );
    assert_eq!(
        rule_norm_only, None,
        "norm_label alone (\"mockagentruntime\", no word boundary) must not match"
    );
}

// ---------------------------------------------------------------------
// Ingest-level behaviour, driven through from_json(TEST_CODE_FIXTURE).
// ---------------------------------------------------------------------

#[test]
fn excludes_four_nodes_from_both_the_graph_and_the_index() {
    let result = from_json(TEST_CODE_FIXTURE).expect("test_code.json must parse");
    let model = &result.model;

    assert_eq!(model.graph.node_count(), 6);

    for excluded_id in [
        "drop_name_mock",
        "drop_name_fake",
        "drop_path_tests_dir",
        "drop_path_test_suffix",
    ] {
        assert!(
            !model.index.contains_key(excluded_id),
            "{excluded_id} must be absent from the index"
        );
        assert!(
            model.graph.node_weights().all(|n| n.id != excluded_id),
            "{excluded_id} must be absent from the graph's node weights"
        );
    }
}

#[test]
fn excluded_test_code_counts_match_and_the_disjoint_invariant_holds() {
    let result = from_json(TEST_CODE_FIXTURE).expect("test_code.json must parse");
    assert_eq!(
        result.excluded_test_code,
        TestCodeExcluded {
            nodes: 4,
            by_path: 2,
            by_name: 2,
            edges: 2,
        }
    );
    assert_eq!(
        result.excluded_test_code.by_path + result.excluded_test_code.by_name,
        result.excluded_test_code.nodes,
        "by_path + by_name must equal nodes (DP-XBL-06)"
    );
}

#[test]
fn exclusion_dropped_edges_never_produce_a_missing_id_warning() {
    let result = from_json(TEST_CODE_FIXTURE).expect("test_code.json must parse");

    // Two edges survive: keep_prod->keep_contest and keep_attestation->keep_mockup.
    assert_eq!(result.model.graph.edge_count(), 2);

    // DP-XBL-05: the two exclusion-dropped edges produce NO warnings; only
    // the genuinely dangling ghost_missing target does.
    assert_eq!(result.warnings.len(), 1);
    assert_eq!(result.warnings[0].target, "ghost_missing");
}

#[test]
fn non_structural_relation_from_an_excluded_node_does_not_inflate_the_edge_counter() {
    // The `contains` link (drop_name_fake -> keep_prod) is killed by the
    // pre-existing relation allow-list BEFORE the exclusion counter ever
    // sees it -- so excluded.edges stays at 2, not 3.
    let result = from_json(TEST_CODE_FIXTURE).expect("test_code.json must parse");
    assert_eq!(
        result.excluded_test_code.edges, 2,
        "the non-structural `contains` edge from an excluded node must not be counted"
    );
}

#[test]
fn excluded_nodes_never_vote_in_the_community_name_resolution() {
    // Community A has two surviving nodes named "Auth" and three EXCLUDED
    // nodes named "TestFixtures". If the excluded nodes voted, "TestFixtures"
    // would win 3-2 (DP-XBL-04).
    let result = from_json(TEST_CODE_FIXTURE).expect("test_code.json must parse");
    assert_eq!(result.model.community_label(&"A".to_string()), "Auth");
}

#[test]
fn a_node_with_neither_signal_survives_with_every_field_intact() {
    let result = from_json(TEST_CODE_FIXTURE).expect("test_code.json must parse");
    let node = &result.model.graph[result.model.index["keep_prod"]];
    assert_eq!(node.label, "LoginService");
    assert_eq!(node.community, "A");
    assert_eq!(node.source_file.as_deref(), Some("src/auth/login.rs"));
    assert_eq!(node.source_line, Some(10));
}

// ---------------------------------------------------------------------
// Real-fixture reconciliation (finding 8): the measured impact on
// apps/seam-core/tests/fixtures/graph.json.
// ---------------------------------------------------------------------

#[test]
fn real_fixture_exclusion_counts_match_the_measured_impact() {
    let result = from_json(REAL_GRAPH).expect("real sample/graph.json must parse successfully");
    assert_eq!(
        result.excluded_test_code,
        TestCodeExcluded {
            nodes: 212,
            by_path: 29,
            by_name: 183,
            edges: 266,
        }
    );
}
