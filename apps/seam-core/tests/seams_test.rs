//! Failing (RED) tests for `seam_core::seams::detect`. See 01-01-PLAN.md
//! Task 2's `<behavior>` block for the exact contract these assert.
//! Implementation lands in Task 3 (GREEN).

use seam_core::{detect, from_json};

const REAL_GRAPH: &str = include_str!("fixtures/graph.json");
const CLEAN_FIXTURE: &str = include_str!("fixtures/clean.json");

/// 09-01: four nodes, one per community A/B/C/D, and exactly six links
/// wiring every unordered community pair once. Every pair therefore has
/// `crossings == 1`, so ranking by crossing count alone leaves all six in a
/// total tie and the resulting order is decided ENTIRELY by the tie-break.
/// Written to make `detect`'s ordering rule observable; a fixture without a
/// tie cannot fail the tests below no matter how the comparator behaves.
const TIED_FIXTURE: &str = include_str!("fixtures/tied_seams.json");

fn tied_model() -> seam_core::Model {
    from_json(TIED_FIXTURE)
        .expect("tied fixture must parse")
        .model
}

/// One `detect` call's result as ordered `(a, b)` pairs. `detect` already
/// normalizes each seam so `a < b`, so this is the pair as it is stored, not
/// a re-sorted view of it.
fn order_of(model: &seam_core::Model) -> Vec<(String, String)> {
    detect(model).into_iter().map(|s| (s.a, s.b)).collect()
}

#[test]
fn ranks_seams_by_crossing_count_descending_on_clean_fixture() {
    let ingest = from_json(CLEAN_FIXTURE).expect("clean fixture must parse");
    let seams = detect(&ingest.model);

    assert_eq!(
        seams.len(),
        3,
        "clean.json has exactly 3 distinct crossing community pairs: A-B, B-C, A-C"
    );

    // Highest-crossing pair (A-B, 3 crossings) must be first.
    let top = &seams[0];
    let top_pair = if top.a <= top.b {
        (top.a.clone(), top.b.clone())
    } else {
        (top.b.clone(), top.a.clone())
    };
    assert_eq!(top_pair, ("A".to_string(), "B".to_string()));
    assert_eq!(top.crossings, 3);

    // Full ranking must be strictly descending: 3, 2, 1.
    let counts: Vec<usize> = seams.iter().map(|s| s.crossings).collect();
    assert_eq!(counts, vec![3, 2, 1]);
}

#[test]
fn real_sample_crossings_sum_to_356_across_56_seam_pairs() {
    let ingest = from_json(REAL_GRAPH).expect("real sample/graph.json must parse");
    let seams = detect(&ingest.model);

    assert_eq!(seams.len(), 56, "real sample has 56 distinct crossing community pairs");

    let total: usize = seams.iter().map(|s| s.crossings).sum();
    assert_eq!(
        total, 356,
        "summed crossings across all seams must equal the 356 cross-community \
         structural+EXTRACTED edges in the real sample"
    );
}

/// 09-01 / TIME-03's precondition: navigating back to the same historical
/// position must show the same ranked list. `detect` groups into a `HashMap`
/// whose iteration order is seeded per instance, so before the tie-break fix
/// the same `&Model` ranked twice in one process gave two different lists.
#[test]
fn repeated_detect_calls_on_one_model_return_the_identical_order() {
    let model = tied_model();

    // Guard the precondition this test rests on. Without it the assertion
    // below could pass vacuously on a fixture that has no ties at all.
    let seams = detect(&model);
    assert_eq!(
        seams.len(),
        6,
        "tied fixture must produce all six unordered community pairs"
    );
    assert!(
        seams.iter().all(|s| s.crossings == 1),
        "every seam in the tied fixture must have exactly 1 crossing, \
         otherwise the ordering is decided by crossing count and the \
         tie-break is never exercised: {seams:?}"
    );

    let orderings: Vec<Vec<(String, String)>> = (0..25).map(|_| order_of(&model)).collect();
    let first = &orderings[0];
    for (i, ordering) in orderings.iter().enumerate() {
        assert_eq!(
            ordering, first,
            "detect call {i} produced a different order than call 0 on the \
             SAME model\n  call 0: {first:?}\n  call {i}: {ordering:?}"
        );
    }
}

/// The order must be a function of the graph DATA, not of one `HashMap`
/// instance's seed. Test 1 alone cannot establish this, because it reuses a
/// single model.
#[test]
fn two_independent_ingests_of_the_same_graph_rank_identically() {
    let left = tied_model();
    let right = tied_model();

    assert_eq!(
        order_of(&left),
        order_of(&right),
        "two independently ingested Models built from identical JSON must \
         rank identically"
    );
}

/// Pins WHICH deterministic order, not merely that some order repeats — a
/// comparator sorting the tie descending would satisfy the two tests above
/// and fail this one. Ascending lexicographic on `a`, then on `b`, is the
/// same convention `apply::resolve_community` and
/// `model::resolve_community_names` already use.
#[test]
fn the_tie_break_is_ascending_lexicographic_on_the_pair() {
    let expected: Vec<(String, String)> = [
        ("A", "B"),
        ("A", "C"),
        ("A", "D"),
        ("B", "C"),
        ("B", "D"),
        ("C", "D"),
    ]
    .into_iter()
    .map(|(a, b)| (a.to_string(), b.to_string()))
    .collect();

    assert_eq!(order_of(&tied_model()), expected);
}
