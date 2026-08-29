//! SCRATCH dev tool — not part of the shipped crate. Renders the graph
//! canvas headlessly against a graph fixture and saves a PNG for visual
//! iteration, plus prints quantitative spread/overlap stats pulled
//! straight from `SeamLayoutState` (bounding box, min pairwise distance,
//! wall time per step) so convergence/regression can be checked without a
//! display or a human eyeballing the PNG.
//!
//! Defaults to the REAL `sample/graph.json` (1097 nodes) since that's the
//! scale that reproduces the no-repulsion collapse bug
//! (`.planning/debug/no-repulsion-in-seamlayout.md`) -- the tiny 4-node
//! `watch.json` fixture never showed it. Override with `DEV_SNAPSHOT_FIXTURE`.
//!
//! Usage: `cargo run -p seam-explorer-egui --example dev_snapshot [A,B]`
//! (optional `A,B` focuses that seam; omit for the default/unfocused view,
//! which is the exact scenario the bug report was filed against).
use egui_kittest::Harness;
use seam_explorer_egui::app::{FocusState, SeamExplorerApp};
use seam_explorer_egui::graph_view;
use seam_explorer_egui::layout::SeamLayoutState;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

const DEFAULT_FIXTURE: &str =
    "/Users/esumerfd/GoogleDrive/edward/Personal/projects/arch-visual/sample/graph.json";

/// Checkpoints (cumulative step counts) at which bounding-box/min-distance
/// stats are printed, so convergence can be judged empirically instead of
/// guessed -- if spread is still growing meaningfully between the last two
/// checkpoints, `run_steps`'s final value is too low.
const CHECKPOINTS: &[usize] = &[30, 60, 120, 240, 480];

fn bounding_box(positions: &HashMap<usize, egui::Pos2>) -> (f32, f32) {
    let xs = positions.values().map(|p| p.x);
    let ys = positions.values().map(|p| p.y);
    let (min_x, max_x) = xs.fold((f32::MAX, f32::MIN), |(mn, mx), v| (mn.min(v), mx.max(v)));
    let (min_y, max_y) = ys.fold((f32::MAX, f32::MIN), |(mn, mx), v| (mn.min(v), mx.max(v)));
    (max_x - min_x, max_y - min_y)
}

/// O(n^2) min pairwise distance -- fine for one-off dev-tool reporting at
/// this node count (~600K pairs for 1097 nodes, sub-second).
fn min_pairwise_distance(positions: &HashMap<usize, egui::Pos2>) -> f32 {
    let pts: Vec<egui::Pos2> = positions.values().copied().collect();
    let mut min_d = f32::MAX;
    for i in 0..pts.len() {
        for j in (i + 1)..pts.len() {
            min_d = min_d.min((pts[i] - pts[j]).length());
        }
    }
    min_d
}

fn main() {
    let focus_seam = std::env::args().nth(1); // e.g. "A,B" to focus that seam; omit for default view
    let fixture_path =
        std::env::var("DEV_SNAPSHOT_FIXTURE").unwrap_or_else(|_| DEFAULT_FIXTURE.to_string());
    let json = std::fs::read_to_string(&fixture_path)
        .unwrap_or_else(|e| panic!("fixture read ({fixture_path}): {e}"));
    let outcome = seam_explorer_egui::load::read_and_ingest(&json).expect("fixture must ingest");
    let model = outcome.model;
    let seams = outcome.seams;
    let node_count = model.graph.node_count();

    let (focus, detail) = if let Some(spec) = focus_seam {
        let mut parts = spec.splitn(2, ',');
        let a = parts.next().unwrap().to_string();
        let b = parts.next().unwrap().to_string();
        let scc = model.scc.as_ref().expect("scc finalized by ingest");
        let detail = seam_core::seam_detail(&model, scc, &a, &b);
        (Some(FocusState { a, b }), Some(detail))
    } else {
        (None, None)
    };

    let mut app = SeamExplorerApp {
        model: Some(model),
        seams,
        focus,
        detail,
        ..Default::default()
    };

    let captured: Rc<RefCell<HashMap<usize, egui::Pos2>>> = Rc::new(RefCell::new(HashMap::new()));
    let captured_for_closure = captured.clone();

    let mut harness = Harness::new_ui(move |ui| {
        graph_view::show(ui, &mut app);
        let state = egui_graphs::get_layout_state::<SeamLayoutState>(ui, None);
        *captured_for_closure.borrow_mut() = state.positions().clone();
    });
    harness.set_size(egui::vec2(1200.0, 800.0));

    println!("fixture: {fixture_path} ({node_count} nodes)");
    let mut steps_run = 0usize;
    for &checkpoint in CHECKPOINTS {
        let delta = checkpoint - steps_run;
        let t0 = std::time::Instant::now();
        harness.run_steps(delta);
        let elapsed = t0.elapsed();
        steps_run = checkpoint;

        let positions = captured.borrow();
        let (spread_x, spread_y) = bounding_box(&positions);
        let min_d = min_pairwise_distance(&positions);
        println!(
            "steps={steps_run:>4} spread=({spread_x:>8.1} x {spread_y:>8.1}) \
             min_pairwise_dist={min_d:>6.2} last_{delta}_steps_wall={elapsed:.2?} \
             per_step={:.3?}",
            elapsed / (delta.max(1) as u32)
        );
    }

    let image = harness.render().expect("render");
    image
        .save("/tmp/egui_canvas_snapshot.png")
        .expect("save png");
    println!(
        "saved /tmp/egui_canvas_snapshot.png ({}x{})",
        image.width(),
        image.height()
    );
}
