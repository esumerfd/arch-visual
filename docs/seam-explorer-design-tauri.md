# Design Doc — Seam Explorer (Solution 1: Tauri + Rust core)

**Status:** Draft · **Owner:** _TBD_ · **Last updated:** 2026-07-11
**Related:** `seam-explorer-design-egui.md` (Solution 2), `seam-core` crate spec (shared)

---

## 1. Summary

Rebuild the Seam Explorer as a **Tauri v2** desktop application: keep the existing
HTML/D3/SVG interface almost verbatim as the webview frontend, and move all graph
parsing and analysis into a **Rust backend** built on `petgraph`. The frontend calls
Rust through Tauri's `invoke` bridge instead of computing seams in JavaScript.

The point of the rewrite is not the shell — it's the core. Doing analysis in Rust
replaces the browser-side heuristics with rigorous graph algorithms (true
strongly-connected-component cycle detection, real shortest paths) and lets the app
share a data model — and potentially a binary — with the existing Rust
`graphify-export` toolchain, so one tool can both build and explore the graph.

## 2. Goals

- Preserve the two product goals from the prototype:
  1. **Components on either side of a seam** — list the interface (bridge) nodes on
     each side, the crossing calls with direction, and a computed decoupling verdict.
  2. **Zoom + trace** — search and zoom to a seam; drag from one component to another
     to trace a call path and see which seams it crosses.
- Read Graphify's `graph.json` (NetworkX `node_link` format) natively.
- Replace the prototype's cycle heuristic with correct cross-seam cycle detection.
- Ship a small (single-digit MB) native app for macOS, Windows, and Linux.
- Reuse the prototype's frontend with minimal changes.

## 3. Non-goals

- Recomputing communities from scratch (we consume Graphify's `community` field).
- A mobile build (Tauri v2 supports it, but it is out of scope for v1).
- Editing the codebase or the graph. This is a read-only analysis tool.
- Rendering graphs beyond ~5k visible nodes at 60fps (see §9 for the scaling path).

## 4. Background

Graphify emits `graph.json` in NetworkX `node_link_data` form: a `nodes` array
(each node carrying `id`, `community`, and optionally `norm_label`, `file_type`,
`source_file`) and a `links` array of `{source, target}`. A **seam** is the boundary
between two communities; the components "on either side" are the **bridge nodes** —
those with at least one edge crossing the boundary. Everything else in a cluster is
interior.

Graphify also ships a Rust crate (`graphify-export` / "graphify-rs") that already does
multi-format export (JSON, HTML, SVG, GraphML, Cypher) and an MCP server exposing
`query_graph`, `get_node`, and `shortest_path`. Both are integration surfaces for this
design.

## 5. Architecture

```
┌───────────────────────────── Tauri app (single binary) ─────────────────────────────┐
│                                                                                      │
│   Webview (system: WebView2 / WKWebView / WebKitGTK)                                  │
│   ┌────────────────────────────────────────────────────────────┐                     │
│   │  Frontend  (reused prototype: HTML + D3 + SVG)              │                     │
│   │   • seam list, search box, detail panel, trace controls    │                     │
│   │   • rendering, zoom/pan, seam-line signature, animation    │                     │
│   └───────────────▲───────────────────────┬────────────────────┘                     │
│                   │  results (JSON)        │  invoke(cmd, args)                        │
│                   │                        ▼                                           │
│   ┌───────────────┴────────────────────────────────────────────┐                     │
│   │  Rust backend (Tauri commands, async, off UI thread)       │                     │
│   │   load_graph · list_seams · seam_detail · trace_path · …    │                     │
│   └───────────────┬────────────────────────────────────────────┘                     │
│                   │ uses                                                               │
│   ┌───────────────▼────────────────────────────────────────────┐                     │
│   │  seam-core  (shared crate)                                 │                     │
│   │   petgraph model · seam detection · Tarjan SCC · paths     │                     │
│   └───────────────┬────────────────────────────────────────────┘                     │
│                   │ optional                                                           │
│   ┌───────────────▼───────────┐   ┌──────────────────────────┐                        │
│   │ graphify-rs (build graph) │   │ MCP client (shortest_path)│                        │
│   └───────────────────────────┘   └──────────────────────────┘                        │
└──────────────────────────────────────────────────────────────────────────────────────┘
```

The frontend keeps only *view* state (current focus, zoom transform, selection). All
*model* computation lives in `seam-core` behind Tauri commands.

## 6. Detailed design

### 6.1 Shared data model (`seam-core`)

```rust
pub struct Node { pub id: String, pub label: String, pub community: CommunityId,
                  pub file_type: Option<String>, pub source_file: Option<String> }
pub struct Edge { pub source: String, pub target: String }

pub struct Model {              // wraps petgraph::stable_graph::StableDiGraph
    graph: StableDiGraph<Node, ()>,
    index: HashMap<String, NodeIndex>,
    communities: Vec<CommunityId>,
    utility: HashSet<CommunityId>,   // clusters flagged as shared infra
}

pub struct Seam { pub a: CommunityId, pub b: CommunityId, pub crossings: usize }
pub struct SeamDetail {
    pub a: CommunityId, pub b: CommunityId,
    pub bridges_a: Vec<String>, pub bridges_b: Vec<String>,
    pub crossings: Vec<(String, String)>,   // directed, source→target
    pub a_to_b: usize, pub b_to_a: usize,
    pub bidirectional: bool, pub has_cross_cycle: bool,
    pub interface_width: usize,
    pub verdict: Verdict, pub reasons: Vec<String>,
}
pub enum Verdict { Clean, Watch, Leaky, Utility }
pub struct TracePath { pub hops: Vec<String>, pub seams_crossed: Vec<(CommunityId, CommunityId)> }
```

### 6.2 Ingest

Parse `graph.json` (serde_json) tolerating `source`/`target` given as id strings or as
node objects. Node label falls back `norm_label → label → id`. Build the
`StableDiGraph`, the id→index map, the community set, and the utility set (from
`meta.utility` if present, else empty). Invalid edges (endpoints missing) are dropped
with a warning surfaced to the UI.

### 6.3 Seam detection

Group every inter-community edge by the **unordered** community pair `{a,b}`. Each pair
with ≥1 crossing edge is a seam. Rank seams by crossing count for the list panel.

### 6.4 Decoupling verdict

For a seam `{a,b}`:
- `bridges_a` / `bridges_b`: nodes touching a crossing edge on each side.
- `a_to_b` / `b_to_a`: crossing counts per direction; `bidirectional = both > 0`.
- **Cross-seam cycle (rigorous):** compute SCCs of the whole graph once
  (`petgraph::algo::tarjan_scc`). A seam has `has_cross_cycle = true` iff some SCC
  contains at least one node from `a` **and** one from `b`. This is the correct
  replacement for the prototype's BFS heuristic.
- `interface_width = bridges_a.len() + bridges_b.len()`.
- **Verdict:** `Utility` if either side is a utility cluster; else `Leaky` if
  `has_cross_cycle`; else `Watch` if `bidirectional`; else `Clean`. Width adds a
  "narrow / moderate / wide interface" note to `reasons`.

SCCs are computed once per graph load and cached; per-seam lookup is O(1) against a
node→scc-id map.

### 6.5 Path tracing

`trace_path(from, to)` runs BFS (unit weights) over the directed graph for a shortest
path; returns `None` if unreachable (a legitimate signal that two components are
decoupled in that direction). `seams_crossed` is the list of community transitions
along consecutive hops. If we later want weighted paths (e.g., by change-coupling
strength), swap in `petgraph::algo::dijkstra`.

### 6.6 Command surface (IPC)

All commands are `#[tauri::command] async` so analysis runs off the UI thread; CPU-heavy
passes use `rayon`.

| Command | Args | Returns |
|---|---|---|
| `load_graph` | `source: {path}` or `{inline_json}` | `GraphSummary { nodes, edges, communities }` |
| `list_seams` | — | `Vec<Seam>` (ranked) |
| `seam_detail` | `a, b` | `SeamDetail` |
| `trace_path` | `from, to` | `Option<TracePath>` |
| `search` | `query` | `{ nodes: [...], seams: [...] }` |
| `build_graph` *(opt)* | `dir` | `GraphSummary` (invokes graphify-rs) |
| `mcp_trace` *(opt)* | `from, to` | `TracePath` (delegates to MCP `shortest_path`) |

### 6.7 Frontend changes from the prototype

- Delete the in-JS `normalize`, `seams`, `analyze`, `hasCrossCycle`, `tracePath`
  functions; replace their call sites with `await invoke('…')`.
- Keep rendering, forces, zoom, the seam-line signature, and the drag-to-trace gesture
  unchanged — they operate on the JSON the commands return.
- Add a native file-open (Tauri dialog plugin) alongside the existing browser upload.

## 7. Tech stack & dependencies

- **Tauri v2** (stable since Oct 2024, ~2.11 line): system webview, no bundled
  Chromium; sub-1 MB core, ~20–100 MB idle RAM; capability-based security; bundler for
  `.app/.dmg/.deb/.rpm/.AppImage/.msi/.exe`.
- **petgraph** — graph model, `tarjan_scc`, BFS/Dijkstra.
- **serde / serde_json** — `graph.json` ingest and IPC payloads.
- **rayon** — parallelize analysis on large graphs.
- **Frontend:** existing HTML + D3 v7 (unchanged).
- Optional: **graphify-export / graphify-rs** (build graph in-process), an MCP client.

## 8. Security

Tauri's capability model means the frontend can only call the commands we register — no
ambient file or network access from the webview. Registered file access is scoped to
user-selected paths via the dialog plugin. The app makes no outbound network calls
unless the optional MCP integration is enabled, which is off by default and points only
at a user-configured local server.

## 9. Performance & scaling

SVG (D3) rendering is comfortable to roughly 2–5k visible nodes; beyond that, focus
mode (which dims all but two communities) keeps the working set small even for large
graphs. The Rust analysis itself scales far past what the view can draw — SCC and BFS
are near-linear. If whole-graph rendering of very large codebases becomes a
requirement, the SVG ceiling — not the analysis — is the limit, and that is exactly the
case where Solution 2's GPU path (egui + wgpu) is the better vehicle. For v1 we accept
the SVG ceiling and rely on focus/scoping.

## 10. Known risk: webview rendering differences

Tauri renders through **WebView2** on Windows but **WebKit** on macOS/Linux, and
**WebKitGTK on Linux is the weak spot** — heavy SVG can perform or render differently
there. Mitigations: keep DOM node counts low via focus mode; avoid bleeding-edge CSS;
test on all three webviews in CI. The in-progress Servo-in-Tauri work (GA targeted for
H2 2026) would eventually remove this Linux gap but is not a v1 dependency.

## 11. Testing

- `seam-core`: unit tests over hand-built fixtures — the sample graph from the
  prototype (one clean seam, one leaky seam with a cycle, one utility boundary) becomes
  a golden test asserting each verdict.
- Property test: for random graphs, `has_cross_cycle` agrees with a brute-force
  reachability oracle.
- IPC contract tests: each command's JSON shape is snapshot-tested.
- Cross-platform smoke tests on the three webviews.

## 12. Milestones

- **M0 — shared core:** `seam-core` crate: ingest, seam detection, verdict, SCC, trace,
  golden tests. (Also consumed by Solution 2.)
- **M1 — Tauri shell:** wire the existing frontend to `load_graph`/`list_seams`/
  `seam_detail`; native file open.
- **M2 — the two goals:** `trace_path` + drag gesture; search/zoom. Feature-parity with
  the prototype, now backed by rigorous analysis.
- **M3 — polish & ship:** bundler config, code signing, auto-update; cross-webview QA.
- **M4 — optional:** graphify-rs in-process build; MCP delegation.

## 13. Alternatives considered

- **Solution 2 (native egui):** no webview, single language, better scaling ceiling,
  but a full UI rewrite and blunter styling. See its design doc.
- **Embed a full browser (CEF / Servo):** guarantees identical rendering but reintroduces
  the Chromium-sized bundle Tauri exists to avoid; rejected as overkill for a mostly-SVG
  tool.
- **Keep everything in the browser (no Rust):** the prototype. Rejected because the
  cycle detection cannot be made rigorous cheaply in JS and there is no shared model
  with the graphify-rs toolchain.

## 14. Open questions

- Do we bundle graphify-rs so the app can build graphs, or assume `graph.json` exists?
- Should the utility-cluster set be inferred (high fan-in heuristic) instead of read
  from `meta.utility`?
- Weighted trace paths — is change-coupling weight available in `graph.json`, or
  MCP-only?
