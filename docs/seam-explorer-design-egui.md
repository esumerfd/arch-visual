# Design Doc — Seam Explorer (Solution 2: native egui, no webview)

**Status:** Draft · **Owner:** _TBD_ · **Last updated:** 2026-07-11
**Related:** `seam-explorer-design-tauri.md` (Solution 1), `seam-core` crate spec (shared)

---

## 1. Summary

Rebuild the Seam Explorer as a **fully native Rust application** on **egui/eframe** —
no webview, no HTML, no IPC bridge. UI and analysis live in one process and one
language. Graph rendering uses an egui graph widget (`egui_graphs` or `egui_xyflow`)
over a `petgraph` model, sharing the same `seam-core` analysis crate as Solution 1.

This path trades the reuse of the prototype's HTML/D3 frontend for a single-process,
single-language codebase that starts instantly, has no per-webview rendering quirks,
compiles to WASM for a browser build "for free," and has a clean path to GPU rendering
for very large graphs.

## 2. Goals

- Preserve the two product goals:
  1. **Components on either side of a seam** — interface (bridge) nodes per side,
     crossing calls with direction, and a computed decoupling verdict.
  2. **Zoom + trace** — search/zoom to a seam; drag between components to trace a call
     path and see the seams it crosses.
- One Rust codebase, one process, no IPC.
- Native desktop (macOS/Windows/Linux) **and** a WASM browser build from the same source.
- A rendering architecture that can scale to large graphs via GPU (wgpu) later.
- Share `seam-core` verbatim with Solution 1.

## 3. Non-goals

- Reusing the prototype's HTML/CSS/D3 (this is a from-scratch UI).
- Recomputing communities (we consume Graphify's `community` field).
- Pixel-identical parity with the webview prototype; egui has its own visual language.
- Editing the codebase or graph (read-only tool).

## 4. Background

Same as Solution 1: Graphify emits `graph.json` (NetworkX `node_link_data`); a **seam**
is a boundary between two communities; the components "on either side" are the **bridge
nodes** with edges crossing that boundary. The analysis (`seam-core`) is identical
across both solutions — only the shell differs. See the Tauri doc §4 for the data
background; it is not repeated here.

## 5. Architecture

```
┌────────────────────────── eframe app (single binary / WASM) ──────────────────────────┐
│                                                                                        │
│   egui UI (immediate mode)                                                             │
│   ┌───────────────┐   ┌──────────────────────────────┐   ┌──────────────────────────┐ │
│   │ Left panel    │   │ Central canvas               │   │ Right panel              │ │
│   │  seam list    │   │  graph widget (egui_graphs / │   │  seam detail:            │ │
│   │  search box   │   │  egui_xyflow) + seam-line     │   │  bridges A|B, crossings, │ │
│   │  trace ctrls  │   │  overlay, zoom/pan/drag        │   │  verdict, reasons        │ │
│   └───────────────┘   └───────────────┬──────────────┘   └──────────────────────────┘ │
│                                       │ reads/writes                                    │
│   ┌───────────────────────────────────▼──────────────────────────────────────────────┐ │
│   │  App state:  Model (petgraph) · focus · zoom · selection · trace result           │ │
│   └───────────────────────────────────┬──────────────────────────────────────────────┘ │
│                                       │ uses                                            │
│   ┌───────────────────────────────────▼──────────────────────────────────────────────┐ │
│   │  seam-core (shared): seam detection · Tarjan SCC · verdict · path tracing         │ │
│   └────────────────────────────────────────────────────────────────────────────────── ┘
│                                                                                        │
│   (scaling option) wgpu render layer + fdg force layout for >10k nodes                  │
└────────────────────────────────────────────────────────────────────────────────────────┘
```

Because everything is in-process, "commands" are just function calls on `seam-core`;
there is no serialization boundary.

## 6. Detailed design

### 6.1 Shared analysis (`seam-core`)

Identical to Solution 1 (see its §6.1–6.5): `petgraph` model, seam detection by
unordered community pair, decoupling verdict with **rigorous cross-seam cycle detection
via `tarjan_scc`** (a seam is leaky iff some SCC spans both its communities), and
BFS/Dijkstra path tracing. This crate has no UI or webview dependencies, so both
solutions link it unchanged.

### 6.2 Rendering layer — decision

Three candidates were evaluated:

| Option | What it gives | Cost |
|---|---|---|
| **`egui_graphs`** | petgraph-native widget; Fruchterman–Reingold + hierarchical layouts; zoom/pan/drag/select; events; custom `DisplayNode`/`DisplayEdge`; incremental `fast_forward` stepping; WASM-ready | naive O(n²) FR layout; upstream maintenance cadence is uncertain (author has signalled reduced development; forks exist) |
| **`egui_xyflow`** | React-Flow-style node/flow editor; ships `dependency_graph`, `disjoint_force_graph` (clustered force layout), and `hierarchical_edge_bundling` examples | younger (v0.3.x, Apr 2026); editor-oriented, so some features are unused |
| **Custom on `egui::Painter`** | total control of the seam-line signature and two-sided pull-apart | most work; reimplements pan/zoom/hit-testing |

**Decision:** start on **`egui_graphs`** as the base (it is petgraph-native, which
matches `seam-core`, and already handles interaction), and implement seam-specific
visuals through a custom `DisplayNode`/`DisplayEdge` plus a custom `Layout`. Keep
**`egui_xyflow`** as a fallback specifically for its clustered layout + edge bundling if
the seam metaphor reads better bundled. Custom `Painter` is the escape hatch if neither
widget can express the seam line cleanly. This decision should be revalidated at M2 with
a real graph on screen.

### 6.3 The seam-focus layout (signature interaction)

The prototype's signature — two communities pulling to opposite sides with a seam line
between them — is implemented as a custom `Layout` (or a position-seeding pass):

- When a seam `{a,b}` is focused, assign a target x of `−sep` to nodes in `a`, `+sep` to
  nodes in `b`, and push all other communities to the far margins with low opacity.
- Run FR only within each side for local structure; the x-target force does the
  separation.
- Draw the seam line and the crossing "threads" as an overlay in the central panel using
  `egui::Painter` on top of the widget, colored to make a leaky (tangled) seam obvious.
- Bridge nodes get a distinct stroke/size via the custom `DisplayNode`.

### 6.4 Interaction model → the two goals

- **Goal 1 (either side):** the right panel renders `SeamDetail` from `seam-core`:
  bridges A | B as clickable lists, the directed crossings, and the verdict badge with
  reasons. Selecting a seam in the left list drives §6.3.
- **Goal 2 (zoom + trace):** the search box filters/zooms to a node or seam (egui's
  `Response` + the widget's navigation settings handle pan/zoom). A **Trace mode**
  toggle switches drag behavior: in trace mode, press on a source node and release on a
  target to call `seam_core::trace_path`; the result highlights the path and lists the
  seams crossed. Out of trace mode, drag repositions nodes.

### 6.5 App/state structure

```rust
struct App {
    model: Model,                    // petgraph + seam-core
    seams: Vec<Seam>,                // ranked, cached at load
    focus: Option<(CommunityId, CommunityId)>,
    detail: Option<SeamDetail>,
    trace: Option<TracePath>,
    trace_mode: bool,
    view: GraphView<…>,              // egui_graphs widget state (zoom/pan/layout)
}
impl eframe::App for App { fn update(&mut self, ctx, frame) { /* 3 panels */ } }
```

Loading a `graph.json` (native file dialog via `rfd`, or a file input in WASM) rebuilds
`model` and recomputes `seams` once.

## 7. Tech stack & dependencies

- **eframe / egui** (0.32-era) — window, panels, immediate-mode UI; native + WASM.
- **`egui_graphs`** (primary) or **`egui_xyflow`** (fallback) — graph rendering widget.
- **petgraph** — model + `tarjan_scc` + paths (via `seam-core`).
- **serde / serde_json** — `graph.json` ingest.
- **rfd** — native file dialog; **trunk** — WASM build/serve.
- Scaling (later): **wgpu** render layer + an **fdg**-style force-layout crate.

## 8. Performance & scaling

egui_graphs' bundled FR layout is naive O(n²) — fine to a few thousand nodes, not tens
of thousands. Two-stage plan:

1. **v1:** egui_graphs + focus mode (only two communities laid out/drawn at a time)
   keeps the working set small regardless of total graph size.
2. **Scale-out:** when whole-graph rendering of very large codebases is required, move
   the node/edge draw to a **wgpu** layer and the layout to a Barnes-Hut / `fdg`
   force simulation. This is the decisive advantage of the native path over Solution 1:
   there is no SVG/DOM ceiling and no webview between the data and the GPU.

Analysis (`seam-core`) is near-linear and never the bottleneck.

## 9. Cross-platform & rendering consistency

Unlike Solution 1, there is **no webview**, so there are no WebKitGTK-vs-WebView2
rendering differences — egui draws the same everywhere (its own tessellated renderer).
The tradeoff is that egui's immediate-mode styling is **blunter than CSS**: gradients,
fine typography, and the seam-line polish take more manual `Painter` work than the
prototype's stylesheet did.

## 10. Testing

- `seam-core`: shared golden tests (same fixtures as Solution 1 — clean/leaky/utility
  seams).
- UI logic: extract focus-layout target assignment and trace-mode state transitions into
  pure functions and unit-test them without a live egui context.
- Snapshot/visual tests via `egui_kittest` (headless egui harness) on the panels and the
  seam overlay.
- WASM smoke test: load the sample graph in a headless browser build.

## 11. Milestones

- **M0 — shared core:** `seam-core` (shared with Solution 1): ingest, seams, verdict,
  SCC, trace, golden tests.
- **M1 — eframe skeleton:** three-panel layout; load `graph.json`; render the graph with
  egui_graphs; zoom/pan/drag.
- **M2 — seam focus:** custom layout (two-sided pull-apart) + seam-line overlay + bridge
  styling; right-panel `SeamDetail`. **Revalidate the widget choice here.**
- **M3 — the two goals complete:** search/zoom; Trace mode drag-to-trace with path
  highlight and seams-crossed list.
- **M4 — WASM build + packaging:** trunk build; native bundles.
- **M5 — optional scale-out:** wgpu render layer + fdg layout for large graphs.

## 12. Alternatives considered

- **Solution 1 (Tauri):** reuses the prototype frontend and keeps CSS expressiveness,
  but keeps a webview (WebKitGTK Linux quirks, SVG ceiling) and an IPC bridge. Best when
  minimizing rewrite matters most.
- **`egui_xyflow` as primary instead of `egui_graphs`:** stronger clustered layout and
  edge bundling out of the box, but editor-oriented and younger; kept as fallback.
- **Full custom `Painter` from day one:** maximum control of the signature, but
  reimplements interaction that the widgets already provide; deferred to escape hatch.
- **Other native toolkits (Slint, Iced, gtk-rs):** viable Rust UI, but none has a
  petgraph-native interactive graph widget comparable to egui_graphs/egui_xyflow, so
  they'd mean building the graph view from scratch.

## 13. Risks & mitigations

- **egui_graphs maintenance uncertainty** → pin a known-good version; keep the rendering
  layer behind a thin trait so egui_xyflow or a custom painter can be swapped in.
- **O(n²) layout** → focus mode in v1; wgpu + fdg for scale-out.
- **Styling effort** → budget explicit time for the seam-line/`Painter` polish; it is
  the one place to spend design effort.

## 14. Open questions

- Is the WASM browser build a shipping target or just a dev convenience? (Affects how
  much we invest in file I/O parity.)
- Do we need multiple graph views open at once (comparison)? egui_graphs supports
  per-instance IDs if so.
- At what node count do we commit to the wgpu path rather than deferring it?
