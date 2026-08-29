//! Failing (RED) tests for `seam_core::seams::detect`. See 01-01-PLAN.md
//! Task 2's `<behavior>` block for the exact contract these assert.
//! Implementation lands in Task 3 (GREEN).

use seam_core::{detect, from_json};

const REAL_GRAPH: &str = include_str!("fixtures/graph.json");
const CLEAN_FIXTURE: &str = include_str!("fixtures/clean.json");

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
