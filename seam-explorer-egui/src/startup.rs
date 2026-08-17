//! CLI-arg preload for the review loop (plan 05-14): `graph_path_from_args`
//! parses `std::env::args()`-shaped input into an optional graph path, and
//! `preload_graph` mirrors `SeamExplorerApp::load_graph`'s post-dialog half
//! so a graph can be loaded before the first painted frame instead of via
//! the "Load graph.json" dialog button.
//!
//! This exists as a library module -- not inline in `main.rs` -- because a
//! binary crate's `fn main()` is unreachable from integration tests, and
//! this logic needs the same test coverage as every other load path in this
//! crate.

/// Parses the graph path out of an argv-shaped iterator (program name
/// first). Positional and flagless: the first non-empty argument after the
/// program name wins, and anything beyond it is ignored.
pub fn graph_path_from_args<I>(_args: I) -> Option<std::path::PathBuf>
where
    I: IntoIterator<Item = String>,
{
    None
}

/// Reads `path` and applies it to `app` exactly as the dialog path does
/// (`SeamExplorerApp::load_graph`, minus its `pick_file` step). Inert stub
/// for RED -- filled in Task 2 (GREEN).
pub fn preload_graph(_app: &mut crate::app::SeamExplorerApp, _path: &std::path::Path) {}
