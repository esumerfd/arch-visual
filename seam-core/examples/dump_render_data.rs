//! SCRATCH dev tool — not part of the shipped crate. Dumps list_seams and
//! graph_render_data-equivalent JSON for a fixture, to feed a mocked-Tauri
//! copy of the D3 frontend for visual reference rendering.
use serde::Serialize;

#[derive(Serialize)]
struct RenderNode {
    id: String,
    label: String,
    community: String,
}
#[derive(Serialize)]
struct RenderEdge {
    source: String,
    target: String,
}
#[derive(Serialize)]
struct GraphRenderData {
    nodes: Vec<RenderNode>,
    edges: Vec<RenderEdge>,
}

fn main() {
    let path = std::env::args()
        .nth(1)
        .expect("usage: dump_render_data <fixture.json>");
    let json = std::fs::read_to_string(&path).expect("read fixture");
    let ingest = seam_core::from_json(&json).expect("fixture must parse");
    let model = ingest.model;

    let seams = seam_core::detect(&model);

    let nodes = model
        .graph
        .node_indices()
        .map(|idx| {
            let n = &model.graph[idx];
            RenderNode {
                id: n.id.clone(),
                label: n.id.clone(),
                community: n.community.clone(),
            }
        })
        .collect();
    let edges = model
        .graph
        .edge_indices()
        .map(|e| {
            let (s, t) = model.graph.edge_endpoints(e).unwrap();
            RenderEdge {
                source: model.graph[s].id.clone(),
                target: model.graph[t].id.clone(),
            }
        })
        .collect();
    let render_data = GraphRenderData { nodes, edges };

    let scc = seam_core::compute_scc(&model);
    let details: Vec<_> = seams
        .iter()
        .map(|s| seam_core::seam_detail(&model, &scc, &s.a, &s.b))
        .collect();

    println!("__SEAMS__{}", serde_json::to_string(&seams).unwrap());
    println!("__RENDER__{}", serde_json::to_string(&render_data).unwrap());
    println!("__DETAILS__{}", serde_json::to_string(&details).unwrap());
}
