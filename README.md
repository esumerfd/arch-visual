# Seam Explorer

![Seam Explorer](docs/seam-explorer.png)

Seam Explorer is a native macOS desktop app that visualizes **architectural
seams** — the boundaries between components (communities) in a codebase graph
produced by [Graphify](https://github.com/esumerfd/graphify). It shows what
sits on either side of a seam and lets you trace a call path across seams to
see real coupling.

You can see the components on either side of an architectural seam and trace
a call path across seams to understand how decoupled — or leaky — that
boundary really is.

The app is read-only: it never edits your codebase or the graph, and nothing
is shared or exported — each teammate runs it locally against their own
`graph.json`.

## Requirements

- macOS (this is the only supported platform)
- [Rust](https://rustup.rs) (stable toolchain, installed via `rustup`)
- [Node.js](https://nodejs.org) (for the Tauri CLI)
- A `graph.json` file produced by Graphify

## Building and running

```sh
git clone git@github.com:esumerfd/arch-visual.git
cd arch-visual
make run
```

`make run` installs npm dependencies on first use and launches the app in
Tauri's dev mode.

There is some sample data in sample. Select Load graph.json and select that file.

To produce a native `.app` bundle instead:

```sh
make build
```

The bundle is written to `seam-explorer/target/release/bundle/macos/`.

## Installing

```sh
make install
```

This builds a release bundle and copies `Seam Explorer.app` into
`/Applications`.

### Why does macOS warn me when I open it?

This build is unsigned — it isn't distributed with an Apple Developer
certificate, since it's an internal tool rather than a public release. macOS
Gatekeeper will block it by default. `make install` already runs the fix
(`xattr -cr`) for you; if you instead copy the `.app` in some other way and
see "app is damaged and can't be opened," run:

```sh
xattr -cr "/Applications/Seam Explorer.app"
```

This unblocks only the one app you installed — it does not disable
Gatekeeper for anything else.

## User guide

### Loading a graph

Click **Load graph.json** and pick a `graph.json` file exported by Graphify
(NetworkX `node_link_data` format: a `nodes` array with `community` per node,
and a `links` array of edges).

### Exploring seams

Once loaded, the graph renders as communities of nodes with the **seam** —
the crossing edges between two communities — highlighted. Use the search box
to jump to a specific component or seam by name.

Selecting a seam shows its detail: the bridge nodes on each side (the ones
with at least one edge crossing the boundary), the crossing calls and their
direction, and a computed verdict:

- **Clean** — crossings flow one way, no cycle between the two communities.
- **Watch** — crossings exist both ways but no confirmed cycle; worth
  keeping an eye on.
- **Leaky** — a real cycle crosses the seam, meaning the two communities
  depend on each other rather than being cleanly layered.

### Tracing a call path

Toggle **Trace mode**, then drag from one component to another to compute
and draw the path between them. The trace highlights every seam the path
crosses, so you can see exactly how many architectural boundaries a call has
to cross to get from A to B.

Use **Reset view** at any point to clear the current search, trace, and
zoom/pan state.

## Project layout

```
arch-visual/
├── docs/                              design docs for the app's two
│                                       considered implementations (Tauri,
│                                       the one that shipped, and egui)
└── seam-explorer/                     the app
    ├── src/                           Rust/Tauri backend (commands, state)
    ├── libs/seam-core/                graph model, seam detection, tracing
    ├── frontend/                      HTML + D3 webview UI
    └── tauri.conf.json
```
