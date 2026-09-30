//! Test code is never presented: `is_test_path` classifies a node's
//! `source_file`, and both the ingest path and the live `add_node` path drop
//! nodes it flags. Path cases are taken from a real 109k-node webapi graph,
//! where "Estimate" contains "test" and a substring match would have hidden
//! half the production services.

use seam_core::{apply_add_node, from_json, is_test_path, Model};

#[test]
fn test_directories_are_test_code() {
    for path in [
        "app/tests/CTKO.CombinedServer.Tests/Repositories/FooTests.cs",
        "tests/cli_integration.rs",
        "src/test/java/com/acme/Thing.java",
        "client/src/app/calendar/render-tests/calendar.component.ts",
        "client/src/app/takeoff/canvas/contract-tests/activate-test-helpers.ts",
        "client/src/app/account/test-data/fixture.ts",
        "client/src/app/takeoff/partner-export-dialog/testing/fixtures.ts",
        "common/libtest/lib/src/CTKO.LibTest/Log/LogTracker.cs",
        "client/src-ajs/testUtil/Any.m.ts",
        "client/src/app/components/ai-assistant/_tests/fakes.ts",
        "client/src/components/__tests__/button.tsx",
        "client/src-ajs/stackcore/libraries/__mocks__/FontResizer.m.ts",
        "client/cypress/support/commands.js",
        "client/e2e/login.po.ts",
        "spec/models/user_spec.rb",
        "pkg/parser/testdata/input.go",
        "toxic/library/appfeature_test/aerial_images/get/010_setup.groovy",
        "C:\\repo\\App.Tests\\Thing.cs",
    ] {
        assert!(is_test_path(path), "expected test code: {path}");
    }
}

#[test]
fn test_file_names_are_test_code() {
    for path in [
        "client/src/app/estimates/estimate-worksheet.component.spec.ts",
        "client/src/app/foo.test.tsx",
        "client/src/app/foo.cy.ts",
        "bin/culture-gate.test.sh",
        "internal/server/handler_test.go",
        "pkg/tool/test_parser.py",
        "pkg/tool/parser_test.py",
        "src/Acme/ThingTests.cs",
        "src/Acme/ThingTest.java",
    ] {
        assert!(is_test_path(path), "expected test code: {path}");
    }
}

#[test]
fn production_code_containing_test_letters_is_not_test_code() {
    for path in [
        "app/src/CTKO.CombinedServer/Services/Estimates/IndependentEstimateService.cs",
        "common/libmain/lib/src/CTKO.MainLibrary/Domain/IndependentEstimateSnapshot.cs",
        "app/src/Contest/Attestation.cs",
        "client/src/app/latest/latest.component.ts",
        "docs/superpowers/specs/2026-07-02-unified-error-page-design.md",
        "client/src/app/estimates/ui/testing.md",
        "client/src/app/takeoff/canvas/docs/test-rubric.md",
        "api/openapi-spec/schema.ts",
        "src/main.rs",
        "",
    ] {
        assert!(!is_test_path(path), "expected production code: {path}");
    }
}

const MIXED: &str = r#"{
  "nodes": [
    {"id": "svc", "label": "Service", "community": 0, "source_file": "app/src/Service.cs"},
    {"id": "svc_test", "label": "ServiceTests", "community": 0, "source_file": "app/tests/ServiceTests.cs"},
    {"id": "ext", "label": "json", "community": 1}
  ],
  "links": [
    {"source": "svc_test", "target": "svc", "relation": "calls", "confidence": "EXTRACTED"},
    {"source": "svc", "target": "ext", "relation": "calls", "confidence": "EXTRACTED"}
  ]
}"#;

#[test]
fn ingest_drops_test_nodes_and_their_edges_without_warnings() {
    let result = from_json(MIXED).expect("fixture parses");
    let model = &result.model;

    assert!(model.index.contains_key("svc"));
    assert!(
        model.index.contains_key("ext"),
        "a node with no source_file is kept"
    );
    assert!(
        !model.index.contains_key("svc_test"),
        "test node must be dropped"
    );
    assert_eq!(model.graph.node_count(), 2);
    assert_eq!(model.graph.edge_count(), 1, "only svc -> ext survives");
    assert!(
        result.warnings.is_empty(),
        "an edge to a dropped test node is not a missing endpoint: {:?}",
        result.warnings
    );
}

#[test]
fn live_add_node_ignores_test_code() {
    let mut model = Model::default();
    let outcome = apply_add_node(
        &mut model,
        "svc_test",
        "ServiceTests",
        None,
        Some("app/tests/ServiceTests.cs"),
    );
    assert!(outcome.is_none(), "a test node is a no-op");
    assert_eq!(model.graph.node_count(), 0);
}
