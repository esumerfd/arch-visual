//! The editor-launch core: turning a user-typed command template plus a
//! source file into a spawned OS process, and turning a Graphify-exported
//! repo-relative `source_file` into a path an editor can actually open.
//!
//! Two decisions here carry the whole feature's safety story (see this
//! plan's `<threat_model>`, T-05-21-01 and T-05-21-05):
//!
//! - [`build_command`] never hands a single command string to a shell. The
//!   template is split on whitespace into a program plus arguments, and the
//!   filename is always appended as its own final argv element -- never
//!   concatenated into the template string -- so no character in a
//!   filename can ever be reinterpreted as shell syntax.
//! - [`resolve_source_path_with`] / [`resolve_source_path`] search a
//!   bounded, nearest-first set of ancestors of the loaded graph's own
//!   directory (Graphify records no repo root anywhere in `graph.json`),
//!   and give up rather than widen the search when nothing matches --
//!   matching the wrong file while looking like success is worse than
//!   failing.
//!
//! [`spawn`] never blocks the calling thread and never leaks a zombie
//! process: the child handle is moved into a detached thread whose only job
//! is to reap it (T-05-21-03).

use std::path::{Path, PathBuf};

/// Bound on the ancestor walk in [`resolve_source_path_with`]. An unbounded
/// search from a deep graph path would eventually match a same-named
/// relative path in an unrelated tree, which is worse than failing because
/// it looks like success.
pub const MAX_ANCESTOR_HOPS: usize = 8;

/// Builds an argument vector from a whitespace-split `template` plus `path`
/// as its own final element -- suffixed with `:line` only when
/// `append_line` is set and `line` is known. Returns `None` if the template
/// is blank (after splitting) or if `path` is empty, so a blank command
/// template launches nothing (design_decision 3).
pub fn build_command(
    template: &str,
    path: &str,
    line: Option<u32>,
    append_line: bool,
) -> Option<Vec<String>> {
    if path.is_empty() {
        return None;
    }
    let mut argv: Vec<String> = template.split_whitespace().map(String::from).collect();
    if argv.is_empty() {
        return None;
    }
    let last = match (append_line, line) {
        (true, Some(n)) => format!("{path}:{n}"),
        _ => path.to_string(),
    };
    argv.push(last);
    Some(argv)
}

/// Resolves a possibly repo-relative `source_file` against `graph_dir`,
/// nearest ancestor first, within [`MAX_ANCESTOR_HOPS`]. An absolute
/// `source_file` is returned unchanged; a `None` `graph_dir` also returns
/// the input unchanged; if nothing on disk (per `exists`) matches within
/// the bound, the raw relative path is returned unchanged -- best effort
/// beats silence. `exists` is injected so this whole search order is
/// unit-testable without a filesystem.
///
pub fn resolve_source_path_with(
    graph_dir: Option<&Path>,
    source_file: &str,
    exists: &dyn Fn(&Path) -> bool,
) -> PathBuf {
    let raw = PathBuf::from(source_file);
    if raw.is_absolute() {
        return raw;
    }
    let Some(start) = graph_dir else {
        return raw;
    };

    // Nearest-first, bounded ancestor walk: `start` itself is hop 0, then
    // up to MAX_ANCESTOR_HOPS parents. Iterated explicitly (not via
    // `Path::ancestors()` alone) so the hop limit is visible here and
    // matches MAX_ANCESTOR_HOPS -- an unbounded walk would eventually match
    // an unrelated tree's same-named relative path, which looks like
    // success and is worse than failing.
    let mut dir = Some(start);
    for _hop in 0..=MAX_ANCESTOR_HOPS {
        let Some(d) = dir else { break };
        let candidate = d.join(source_file);
        if exists(&candidate) {
            return candidate;
        }
        dir = d.parent();
    }
    raw
}

/// The real-filesystem wrapper over [`resolve_source_path_with`].
pub fn resolve_source_path(graph_dir: Option<&Path>, source_file: &str) -> PathBuf {
    resolve_source_path_with(graph_dir, source_file, &|p| p.exists())
}

/// Launches `argv[0]` with `argv[1..]` as its arguments -- never through a
/// shell interpreter. Does not block: the child handle is moved into a
/// detached thread that waits on it, so a GUI editor that stays open for
/// hours cannot freeze the UI thread and cannot accumulate as a defunct
/// process. An empty `argv` is an `InvalidInput` error, never a panic.
///
pub fn spawn(argv: &[String]) -> std::io::Result<()> {
    let Some((program, args)) = argv.split_first() else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "empty argv: no program to launch",
        ));
    };
    let mut child = std::process::Command::new(program).args(args).spawn()?;
    // Never wait on the calling thread (a GUI editor stays open for
    // hours): move the child into a detached thread whose only job is to
    // reap it, so it never becomes a defunct process.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- build_command -----------------------------------------------------

    #[test]
    fn build_command_appends_the_file_as_the_final_argument() {
        assert_eq!(
            build_command("code -g", "/a/b.rs", None, false),
            Some(vec![
                "code".to_string(),
                "-g".to_string(),
                "/a/b.rs".to_string(),
            ])
        );
    }

    #[test]
    fn build_command_appends_the_line_only_when_asked_and_known() {
        let cases: [(bool, Option<u32>, &str); 4] = [
            (false, None, "/a/b.rs"),
            (false, Some(42), "/a/b.rs"),
            (true, None, "/a/b.rs"),
            (true, Some(42), "/a/b.rs:42"),
        ];
        for (append_line, line, expected_last) in cases {
            let result = build_command("code", "/a/b.rs", line, append_line).unwrap_or_else(|| {
                panic!("expected Some for append_line={append_line}, line={line:?}")
            });
            assert_eq!(
                result.last().map(String::as_str),
                Some(expected_last),
                "append_line={append_line}, line={line:?}, got {result:?}"
            );
        }
    }

    #[test]
    fn build_command_rejects_a_blank_template() {
        for blank in ["", "   ", "\t\n"] {
            assert_eq!(
                build_command(blank, "/a/b.rs", None, false),
                None,
                "template {blank:?} must be rejected"
            );
        }
        assert_eq!(
            build_command("code", "", None, false),
            None,
            "an empty path must also be rejected"
        );
    }

    #[test]
    fn build_command_collapses_runs_of_whitespace() {
        let result = build_command("  code   -g  ", "/a/b.rs", None, false)
            .unwrap_or_else(|| panic!("a non-blank template must produce Some"));
        assert_eq!(
            result,
            vec!["code".to_string(), "-g".to_string(), "/a/b.rs".to_string()],
            "whitespace runs must collapse to exactly three elements, got {result:?}"
        );
    }

    #[test]
    fn build_command_keeps_a_hostile_filename_as_one_single_argument() {
        let hostile = "/tmp/a b; rm -rf ~/`whoami`.rs";
        let result = build_command("code", hostile, None, false)
            .unwrap_or_else(|| panic!("a well-formed template must produce Some"));
        assert_eq!(
            result.len(),
            2,
            "argv-not-a-shell-string: the hostile filename must be exactly one argv element, got {result:?}"
        );
        assert_eq!(
            result[1], hostile,
            "the hostile filename must survive byte-for-byte as a single argv element"
        );
    }

    // --- resolve_source_path_with -------------------------------------------

    #[test]
    fn resolve_prefers_the_nearest_ancestor_that_has_the_file() {
        let graph_dir = Path::new("/r/graphify-out");
        let rel = "src/thing.rs";
        let nearest = Path::new("/r/graphify-out/src/thing.rs");
        let farther = Path::new("/r/src/thing.rs");
        let exists = |p: &Path| p == nearest || p == farther;

        let result = resolve_source_path_with(Some(graph_dir), rel, &exists);
        assert_eq!(
            result,
            PathBuf::from(nearest),
            "when the same relative path exists at two ancestor levels, the nearest must win"
        );
    }

    #[test]
    fn resolve_walks_up_to_the_repo_root() {
        let graph_dir = Path::new("/r/graphify-out/nested");
        let rel = "src/thing.rs";
        let root_match = Path::new("/r/src/thing.rs");
        let exists = |p: &Path| p == root_match;

        let result = resolve_source_path_with(Some(graph_dir), rel, &exists);
        assert_eq!(result, PathBuf::from(root_match));
    }

    #[test]
    fn resolve_stops_at_the_hop_limit() {
        // graph_dir is 20 levels below the level that actually has the
        // file -- beyond MAX_ANCESTOR_HOPS, so the search must give up
        // rather than widen indefinitely.
        let mut deep = PathBuf::from("/far");
        for i in 0..20 {
            deep.push(format!("level{i}"));
        }
        let rel = "src/thing.rs";
        let far_match = Path::new("/far").join(rel);
        let exists = move |p: &Path| p == far_match.as_path();

        let result = resolve_source_path_with(Some(&deep), rel, &exists);
        assert_eq!(
            result,
            PathBuf::from(rel),
            "beyond MAX_ANCESTOR_HOPS ({MAX_ANCESTOR_HOPS}) the search must give up and pass the raw relative path through unchanged"
        );
    }

    #[test]
    fn resolve_passes_through_when_nothing_exists_and_when_there_is_no_graph_dir() {
        let rel = "src/thing.rs";
        let never_exists = |_: &Path| false;

        let with_dir = resolve_source_path_with(Some(Path::new("/r")), rel, &never_exists);
        assert_eq!(with_dir, PathBuf::from(rel));

        let without_dir = resolve_source_path_with(None, rel, &never_exists);
        assert_eq!(without_dir, PathBuf::from(rel));
    }

    #[test]
    fn resolve_returns_an_absolute_path_untouched() {
        let abs = "/already/absolute/path.rs";
        let never_exists = |_: &Path| false;
        let result = resolve_source_path_with(Some(Path::new("/r")), abs, &never_exists);
        assert_eq!(
            result,
            PathBuf::from(abs),
            "an absolute source path must short-circuit, even when exists() would say false"
        );
    }

    // --- spawn ---------------------------------------------------------------

    #[test]
    fn spawn_launches_a_real_process_and_returns_ok() {
        let true_bin = "/usr/bin/true";
        if !Path::new(true_bin).exists() {
            eprintln!(
                "SKIP: {true_bin} not present on this machine -- test is inconclusive, not failing"
            );
            return;
        }
        let result = spawn(&[true_bin.to_string()]);
        assert!(
            result.is_ok(),
            "spawning a real program must return Ok, got {result:?}"
        );
    }

    #[test]
    fn spawn_reports_a_missing_program_as_an_error_not_a_panic() {
        let missing = "/definitely/not/a/real/path/seam-explorer-egui-missing-program";
        let result = spawn(&[missing.to_string()]);
        assert!(
            result.is_err(),
            "a missing program must report an error, not panic or silently succeed"
        );
    }

    #[test]
    fn spawn_rejects_an_empty_argv() {
        let result = spawn(&[]);
        assert!(
            result.is_err(),
            "an empty argv must be an error, not a panic"
        );
    }
}
