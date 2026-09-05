//! Plan 07-02, Task 2: the absolute-to-repo-relative conversion, proven
//! against the cases that break SILENTLY.
//!
//! This conversion has no error path anywhere downstream of it. The receiving
//! side will compare the value against a loaded `Node::source_file` with plain
//! equality, and neither `parse_datagram` nor the app's receive loop validates
//! anything but wire shape. A wrong answer here therefore produces a
//! permanently unresolvable advertisement that looks exactly like a correct
//! one -- which is why this file exists at all, and why several of the cases
//! below assert on `None` rather than on a value.

use seam_client::hook_input::repo_relative;

/// One row of the case table: name, file_path, project_dir, cwd, expected.
type Case = (
    &'static str,
    &'static str,
    Option<&'static str>,
    Option<&'static str>,
    Option<&'static str>,
);

/// Every case in this file. The two property-style tests at the bottom sweep
/// this table rather than re-listing the cases, so a case added here cannot
/// escape them.
const CASES: &[Case] = &[
    (
        "project dir wins",
        "/a/repo/src/lib.rs",
        Some("/a/repo"),
        Some("/a"),
        Some("src/lib.rs"),
    ),
    (
        "cwd fallback",
        "/a/repo/src/lib.rs",
        None,
        Some("/a/repo"),
        Some("src/lib.rs"),
    ),
    (
        "trailing separator",
        "/a/repo/src/lib.rs",
        Some("/a/repo/"),
        None,
        Some("src/lib.rs"),
    ),
    (
        "outside every base",
        "/elsewhere/x.rs",
        Some("/a/repo"),
        None,
        None,
    ),
    (
        "sibling name prefix",
        "/a/repository/src/lib.rs",
        Some("/a/repo"),
        None,
        None,
    ),
    ("the base itself", "/a/repo", Some("/a/repo"), None, None),
    (
        "already relative",
        "src/lib.rs",
        Some("/a/repo"),
        None,
        None,
    ),
    ("no base at all", "/a/repo/src/lib.rs", None, None, None),
];

fn run(case: &Case) -> Option<String> {
    let (_, file_path, project_dir, cwd, _) = *case;
    repo_relative(file_path, project_dir, cwd)
}

#[test]
fn the_project_directory_wins_over_the_working_directory() {
    assert_eq!(
        repo_relative("/a/repo/src/lib.rs", Some("/a/repo"), Some("/a")),
        Some("src/lib.rs".to_string()),
        "the project directory is the more specific base and must be tried first; \
         falling through to cwd would yield `repo/src/lib.rs`, which matches no node"
    );
}

#[test]
fn the_working_directory_is_a_real_fallback() {
    // Research Assumptions Log A2: $CLAUDE_PROJECT_DIR is NOT independently
    // verified to be set on every invocation, so this path is load-bearing,
    // not decorative.
    assert_eq!(
        repo_relative("/a/repo/src/lib.rs", None, Some("/a/repo")),
        Some("src/lib.rs".to_string())
    );
}

#[test]
fn a_trailing_separator_on_the_base_is_tolerated() {
    assert_eq!(
        repo_relative("/a/repo/src/lib.rs", Some("/a/repo/"), None),
        Some("src/lib.rs".to_string()),
        "a base with a trailing separator must not produce a leading-separator remainder"
    );
}

#[test]
fn a_path_outside_every_base_yields_nothing() {
    assert_eq!(
        repo_relative("/elsewhere/x.rs", Some("/a/repo"), None),
        None,
        "an honest unknown -- never a fabricated relative path, never the absolute one"
    );
}

#[test]
fn a_sibling_directory_sharing_a_name_prefix_is_not_a_match() {
    // A naive `starts_with(base)` passes this WRONGLY, yielding
    // "sitory/src/lib.rs". The separator requirement is what makes it fail
    // correctly.
    assert_eq!(
        repo_relative("/a/repository/src/lib.rs", Some("/a/repo"), None),
        None
    );
}

#[test]
fn the_base_itself_yields_nothing() {
    assert_eq!(
        repo_relative("/a/repo", Some("/a/repo"), None),
        None,
        "nothing remains after the prefix, so there is no path to advertise"
    );
}

#[test]
fn an_already_relative_path_yields_nothing() {
    assert_eq!(repo_relative("src/lib.rs", Some("/a/repo"), None), None);
}

#[test]
fn the_result_never_starts_with_a_separator() {
    // A leading slash would make the future equality comparison against a
    // graph node's path fail for every node, forever, silently.
    for case in CASES {
        if let Some(value) = run(case) {
            assert!(
                !value.starts_with('/'),
                "case `{}` returned `{value}`, which starts with a separator",
                case.0
            );
        }
    }
}

#[test]
fn an_absolute_path_can_never_be_the_result() {
    for case in CASES {
        let actual = run(case);
        assert_eq!(
            actual.as_deref(),
            case.4,
            "case `{}` did not produce its expected value",
            case.0
        );
        if let Some(value) = actual {
            assert_ne!(
                value, case.1,
                "case `{}` echoed the input path back verbatim",
                case.0
            );
            assert!(
                !std::path::Path::new(&value).is_absolute(),
                "case `{}` returned an absolute path `{value}`",
                case.0
            );
        }
    }
}
