//! Generates `sample/graph-perf.json`: a deterministic, webapi-scale graph
//! for exercising the runtime's load and per-frame cost.
//!
//! ```text
//! cargo run -p seam-core --release --example gen_perf_graph -- sample/graph-perf.json
//! ```
//!
//! The shape follows the 109k-node webapi graph measured in
//! `wk-seam-explorer/research-render-perf.md`: about 65k production code
//! nodes and 80k structural edges survive ingest, with a long-tailed
//! community-size distribution, high-degree hubs, and ~15% of edges crossing
//! communities. On top of that it carries what ingest must throw away, at
//! webapi's proportions: test code (~40% of nodes), markdown documents,
//! concept/rationale nodes, `contains`/`imports` edges outside the relation
//! allow-list, and INFERRED edges. The same seed always produces the same
//! file, so frame times are comparable run to run.

use serde_json::{json, Value};
use std::io::Write;

/// Production files. Each holds 1-3 classes, each class 1-7 methods, which
/// lands at roughly 65k production nodes.
const PROD_FILES: usize = 6_000;
/// Test files (dropped at ingest), sized so test nodes are ~40% of the raw
/// graph, as in webapi.
const TEST_FILES: usize = 4_000;
const DOCUMENTS: usize = 4_000;
const CONCEPTS: usize = 50;
const EXTERNALS: usize = 40;
const COMMUNITIES: usize = 6000;
/// Zipf exponent for community sizes: a few very large communities and a
/// long tail of tiny ones.
const COMMUNITY_SKEW: f64 = 0.9;
/// Share of call/reference edges that leave their own community.
const CROSS_COMMUNITY: f64 = 0.1;

/// SplitMix64: tiny, deterministic, no dependency.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    fn chance(&mut self, p: f64) -> bool {
        (self.next() as f64 / u64::MAX as f64) < p
    }
    fn range(&mut self, lo: usize, hi: usize) -> usize {
        lo + self.below(hi - lo + 1)
    }
}

/// Samples an index in `0..weights.len()` proportionally to `weights`, via a
/// precomputed cumulative table.
struct Weighted(Vec<f64>);

impl Weighted {
    fn zipf(n: usize, s: f64) -> Self {
        let mut acc = 0.0;
        Weighted(
            (1..=n)
                .map(|i| {
                    acc += 1.0 / (i as f64).powf(s);
                    acc
                })
                .collect(),
        )
    }
    fn sample(&self, rng: &mut Rng) -> usize {
        let total = *self.0.last().unwrap();
        let x = rng.next() as f64 / u64::MAX as f64 * total;
        self.0.partition_point(|&c| c < x).min(self.0.len() - 1)
    }
}

const AREAS: [&str; 8] = [
    "Estimates",
    "Takeoff",
    "Partner",
    "Account",
    "Projects",
    "Reports",
    "Pricing",
    "Export",
];
const KINDS: [&str; 6] = [
    "Service",
    "Repository",
    "Controller",
    "Mapper",
    "Validator",
    "Builder",
];
const VERBS: [&str; 10] = [
    "Get", "Save", "Build", "Load", "Apply", "Find", "Map", "Validate", "Export", "Resolve",
];

struct Graph {
    nodes: Vec<Value>,
    links: Vec<Value>,
}

impl Graph {
    fn node(
        &mut self,
        id: &str,
        label: &str,
        file_type: &str,
        file: Option<&str>,
        community: usize,
    ) {
        let mut n = json!({
            "id": id,
            "label": label,
            "norm_label": label.to_lowercase(),
            "file_type": file_type,
            "community": community,
        });
        if let Some(file) = file {
            n["source_file"] = json!(file);
            n["source_location"] = json!(format!("L{}", 1 + id.len() * 7 % 400));
        }
        self.nodes.push(n);
    }
    fn link(&mut self, source: &str, target: &str, relation: &str, confidence: &str) {
        self.links.push(json!({
            "source": source,
            "target": target,
            "relation": relation,
            "confidence": confidence,
            "confidence_score": if confidence == "EXTRACTED" { 1.0 } else { 0.6 },
            "weight": 1.0,
        }));
    }
}

/// One generated file: its id, community, and the ids of its classes and
/// methods, so edges can be drawn between them afterwards.
struct File {
    id: String,
    community: usize,
    classes: Vec<String>,
    methods: Vec<String>,
}

fn build_files(
    g: &mut Graph,
    rng: &mut Rng,
    communities: &Weighted,
    count: usize,
    test: bool,
) -> Vec<File> {
    let mut files = Vec::with_capacity(count);
    for f in 0..count {
        let community = communities.sample(rng);
        let area = AREAS[community % AREAS.len()];
        let kind = KINDS[rng.below(KINDS.len())];
        let (path, stem) = match (test, f % 3) {
            (false, 0) => (
                format!(
                    "client/src/app/{}/c{community}/{}-{f}.ts",
                    area.to_lowercase(),
                    kind.to_lowercase()
                ),
                format!("{area}{kind}{f}"),
            ),
            (false, _) => (
                format!("app/src/Server/{area}/C{community}/{area}{kind}{f}.cs"),
                format!("{area}{kind}{f}"),
            ),
            (true, 0) => (
                format!(
                    "client/src/app/{}/c{community}/{}-{f}.spec.ts",
                    area.to_lowercase(),
                    kind.to_lowercase()
                ),
                format!("{area}{kind}{f}Spec"),
            ),
            (true, _) => (
                format!("app/tests/Server.Tests/{area}/{area}{kind}{f}Tests.cs"),
                format!("{area}{kind}{f}Tests"),
            ),
        };
        let file_id = format!("{}f{f}", if test { "t" } else { "p" });
        g.node(
            &file_id,
            path.rsplit('/').next().unwrap(),
            "code",
            Some(&path),
            community,
        );

        let mut file = File {
            id: file_id,
            community,
            classes: Vec::new(),
            methods: Vec::new(),
        };
        for c in 0..rng.range(1, 3) {
            let class_id = format!("{}_c{c}", file.id);
            let class_label = if c == 0 {
                stem.clone()
            } else {
                format!("{stem}Part{c}")
            };
            g.node(&class_id, &class_label, "code", Some(&path), community);
            g.link(&file.id, &class_id, "contains", "EXTRACTED");
            for m in 0..rng.range(1, 7) {
                let method_id = format!("{class_id}_m{m}");
                let verb = VERBS[rng.below(VERBS.len())];
                g.node(
                    &method_id,
                    &format!("{class_label}.{verb}{m}()"),
                    "code",
                    Some(&path),
                    community,
                );
                g.link(&class_id, &method_id, "method", "EXTRACTED");
                file.methods.push(method_id);
            }
            file.classes.push(class_id);
        }
        files.push(file);
    }
    files
}

/// Picks a target file: usually from the source's own community, otherwise
/// from anywhere, weighted toward the big communities.
fn pick_target<'a>(
    rng: &mut Rng,
    from: &File,
    by_community: &'a [Vec<usize>],
    files: &'a [File],
    communities: &Weighted,
) -> &'a File {
    let community = if rng.chance(CROSS_COMMUNITY) {
        communities.sample(rng)
    } else {
        from.community
    };
    let bucket = &by_community[community];
    if bucket.is_empty() {
        return &files[rng.below(files.len())];
    }
    &files[bucket[rng.below(bucket.len())]]
}

fn main() {
    let out = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "sample/graph-perf.json".to_string());

    let mut rng = Rng(0x5EA3_E8F1_0000_0001);
    let communities = Weighted::zipf(COMMUNITIES, COMMUNITY_SKEW);
    let mut g = Graph {
        nodes: Vec::new(),
        links: Vec::new(),
    };

    let prod = build_files(&mut g, &mut rng, &communities, PROD_FILES, false);
    let tests = build_files(&mut g, &mut rng, &communities, TEST_FILES, true);

    let mut by_community = vec![Vec::new(); COMMUNITIES];
    for (i, f) in prod.iter().enumerate() {
        by_community[f.community].push(i);
    }

    // High-degree hubs with no source file (graphify's external symbols such
    // as `json`), kept by ingest because they carry no file_type other than code.
    let externals: Vec<String> = (0..EXTERNALS).map(|i| format!("ext{i}")).collect();
    for (i, id) in externals.iter().enumerate() {
        g.node(
            id,
            &format!("External{i}"),
            "code",
            None,
            communities.sample(&mut rng),
        );
    }

    for f in &prod {
        // Calls: each method calls 0-1 methods elsewhere.
        for m in &f.methods {
            for _ in 0..rng.range(0, 1) {
                let t = pick_target(&mut rng, f, &by_community, &prod, &communities);
                let target = &t.methods[rng.below(t.methods.len())];
                let confidence = if rng.chance(0.03) {
                    "INFERRED"
                } else {
                    "EXTRACTED"
                };
                g.link(m, target, "calls", confidence);
            }
        }
        // References and imports between files/classes.
        for c in &f.classes {
            for _ in 0..rng.range(0, 2) {
                let t = pick_target(&mut rng, f, &by_community, &prod, &communities);
                g.link(
                    c,
                    &t.classes[rng.below(t.classes.len())],
                    "references",
                    "EXTRACTED",
                );
            }
            if rng.chance(0.15) {
                let t = pick_target(&mut rng, f, &by_community, &prod, &communities);
                g.link(c, &t.classes[0], "implements", "EXTRACTED");
            }
            if rng.chance(0.3) {
                g.link(
                    c,
                    &externals[rng.below(EXTERNALS.min(8))],
                    "references",
                    "EXTRACTED",
                );
            }
        }
        for _ in 0..rng.range(0, 2) {
            let t = pick_target(&mut rng, f, &by_community, &prod, &communities);
            g.link(&f.id, &t.id, "imports_from", "EXTRACTED");
            g.link(&f.id, &t.id, "imports", "EXTRACTED");
        }
    }

    // Test code calls into production; every one of these edges is dropped
    // along with its test node.
    for t in &tests {
        for m in &t.methods {
            let p = &prod[rng.below(prod.len())];
            g.link(
                m,
                &p.methods[rng.below(p.methods.len())],
                "calls",
                "EXTRACTED",
            );
        }
    }

    // Documents and concepts (dropped at ingest), referencing production classes.
    for d in 0..DOCUMENTS {
        let p = &prod[rng.below(prod.len())];
        let id = format!("doc{d}");
        let path = format!(
            "docs/adr/{d:04}-{}.md",
            AREAS[d % AREAS.len()].to_lowercase()
        );
        g.node(
            &id,
            &format!("ADR {d}"),
            "document",
            Some(&path),
            p.community,
        );
        g.link(&id, &p.classes[0], "references", "EXTRACTED");
    }
    for c in 0..CONCEPTS {
        let p = &prod[rng.below(prod.len())];
        let id = format!("concept{c}");
        g.node(
            &id,
            &format!("Concept {c}"),
            if c % 2 == 0 { "concept" } else { "rationale" },
            None,
            p.community,
        );
        g.link(&id, &p.classes[0], "rationale_for", "INFERRED");
    }

    let doc = json!({
        "directed": true,
        "multigraph": false,
        "graph": {},
        "nodes": g.nodes,
        "links": g.links,
        "hyperedges": [],
    });
    let mut file = std::io::BufWriter::new(std::fs::File::create(&out).expect("create output"));
    serde_json::to_writer(&mut file, &doc).expect("write graph");
    file.flush().expect("flush graph");

    let raw = std::fs::read_to_string(&out).expect("re-read graph");
    let ingest = seam_core::from_json(&raw).expect("generated graph must ingest");
    let seams = seam_core::detect(&ingest.model);
    eprintln!(
        "{out}: {} bytes, raw {} nodes / {} links; after ingest {} nodes / {} edges, {} seams, {} warnings",
        raw.len(),
        doc["nodes"].as_array().unwrap().len(),
        doc["links"].as_array().unwrap().len(),
        ingest.model.graph.node_count(),
        ingest.model.graph.edge_count(),
        seams.len(),
        ingest.warnings.len(),
    );
}
