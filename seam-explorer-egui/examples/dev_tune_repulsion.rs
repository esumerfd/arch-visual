//! SCRATCH dev tool -- fast (no egui rendering) constant-tuning harness for
//! `layout::repulsion`. Runs `SeamLayout::next` directly on a synthetic
//! N-node single-group graph (mirrors the real unfocused 1097-node
//! collapse scenario) and prints spread/min-pairwise-distance per
//! checkpoint, without the ~50ms/frame egui_kittest render overhead --
//! lets constants be iterated in milliseconds instead of seconds.
use egui_graphs::{DefaultEdgeShape, DefaultNodeShape, Graph, Layout};
use petgraph::stable_graph::{DefaultIx, StableGraph};
use petgraph::Directed;
use seam_explorer_egui::layout::{SeamLayout, SeamLayoutState};
use std::collections::HashMap;

fn main() {
    let n: usize = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(1097);
    let steps_per_checkpoint: usize = std::env::args()
        .nth(2)
        .and_then(|s| s.parse().ok())
        .unwrap_or(30);
    let checkpoints: usize = std::env::args()
        .nth(3)
        .and_then(|s| s.parse().ok())
        .unwrap_or(10);

    let mut g: Graph<(), (), Directed, DefaultIx, DefaultNodeShape, DefaultEdgeShape> =
        Graph::new(StableGraph::default());
    for _ in 0..n {
        g.add_node(());
    }

    let center = egui::Pos2::new(600.0, 400.0);
    let band_width = 1200.0;
    let band_height = 800.0;
    let mut state = SeamLayoutState::default();
    state.set_targets(
        HashMap::new(),
        HashMap::new(),
        center,
        band_width,
        band_height,
    );

    let mut layout = SeamLayout::from_state(state);
    let ctx = egui::Context::default();

    println!("n={n} band=({band_width}x{band_height})");
    let mut total_steps = 0usize;
    for _ in 0..checkpoints {
        let t0 = std::time::Instant::now();
        for _ in 0..steps_per_checkpoint {
            let raw_input = egui::RawInput::default();
            let _ = ctx.run_ui(raw_input, |ui| layout.next(&mut g, ui));
        }
        let elapsed = t0.elapsed();
        total_steps += steps_per_checkpoint;

        let pts: Vec<egui::Pos2> = g
            .g()
            .node_indices()
            .map(|i| g.node(i).unwrap().location())
            .collect();
        let (min_x, max_x) = pts.iter().fold((f32::MAX, f32::MIN), |(mn, mx), p| {
            (mn.min(p.x), mx.max(p.x))
        });
        let (min_y, max_y) = pts.iter().fold((f32::MAX, f32::MIN), |(mn, mx), p| {
            (mn.min(p.y), mx.max(p.y))
        });
        let mut min_d = f32::MAX;
        for i in 0..pts.len() {
            for j in (i + 1)..pts.len() {
                min_d = min_d.min((pts[i] - pts[j]).length());
            }
        }
        println!(
            "steps={total_steps:>5} spread=({:>7.1} x {:>7.1}) min_pairwise_dist={min_d:>7.2} wall={elapsed:.2?} per_step={:.3?}",
            max_x - min_x,
            max_y - min_y,
            elapsed / (steps_per_checkpoint as u32).max(1)
        );
    }
}
