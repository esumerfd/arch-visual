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
/// program name wins, and anything beyond it is ignored. There is no option
/// syntax to interpret, so a misspelled or unrecognised argument is treated
/// as a path -- deliberately: it surfaces as `preload_graph`'s error banner
/// rather than as a silent no-op, and a silently ignored argument is the
/// worse failure for the review loop this exists to serve.
pub fn graph_path_from_args<I>(args: I) -> Option<std::path::PathBuf>
where
    I: IntoIterator<Item = String>,
{
    let mut iter = args.into_iter();
    iter.next()?; // skip the program name
    let first = iter.next()?;
    if first.is_empty() {
        return None;
    }
    Some(std::path::PathBuf::from(first))
}

/// Reads `path` and applies it to `app` exactly as the dialog path does --
/// this is `SeamExplorerApp::load_graph`'s body minus its `pick_file` step,
/// deliberately kept parallel arm-for-arm so the CLI and dialog routes
/// cannot drift. Lives here rather than in `app.rs` because that file is
/// frozen for this phase; `apply_load_outcome` is already the public seam
/// built for exactly this reuse.
pub fn preload_graph(app: &mut crate::app::SeamExplorerApp, path: &std::path::Path) {
    match std::fs::read_to_string(path) {
        Ok(json) => match crate::load::read_and_ingest(&json) {
            Ok(outcome) => app.apply_load_outcome(outcome),
            Err(e) => app.banner = Some(crate::load::error_banner(&e)),
        },
        Err(e) => app.banner = Some(crate::load::error_banner(&crate::load::LoadError::from(e))),
    }
}
