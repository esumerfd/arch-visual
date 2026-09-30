//! `is_test_path`: decides whether a node's `source_file` is test code, so
//! test code is never presented. Both [`crate::from_json`] and
//! [`crate::apply_add_node`] drop a node this flags.
//!
//! Matching is by whole path segment or whole word, never by substring: on a
//! real 109k-node graph, "Estimate" contains "test", and a substring rule
//! hid most of the production services.

/// Directory names that mean test code only when they are the whole segment.
/// `spec` is deliberately here and not in [`TEST_WORDS`]: `openapi-spec` and
/// `docs/specs` are design documents, not tests.
const TEST_DIRS: [&str; 5] = ["spec", "e2e", "cypress", "__mocks__", "testdata"];

/// Words that mark a directory as test code wherever they appear in its name
/// (`render-tests`, `test-data`, `CTKO.LibTest`, `testUtil`, `App.Tests`).
const TEST_WORDS: [&str; 3] = ["test", "tests", "testing"];

/// The middle part of `name.<marker>.<ext>` (`foo.spec.ts`, `foo.test.sh`,
/// `foo.cy.ts`).
const TEST_FILE_MARKERS: [&str; 3] = ["spec", "test", "cy"];

pub fn is_test_path(path: &str) -> bool {
    let segments: Vec<&str> = path.split(['/', '\\']).filter(|s| !s.is_empty()).collect();
    let Some((file, dirs)) = segments.split_last() else {
        return false;
    };
    dirs.iter().any(|dir| is_test_dir(dir)) || is_test_file(file)
}

fn is_test_dir(dir: &str) -> bool {
    let lower = dir.to_ascii_lowercase();
    TEST_DIRS.contains(&lower.as_str())
        || words(dir).iter().any(|w| TEST_WORDS.contains(&w.as_str()))
}

fn is_test_file(file: &str) -> bool {
    let lower = file.to_ascii_lowercase();
    let parts: Vec<&str> = lower.split('.').collect();
    if parts.len() >= 3 && TEST_FILE_MARKERS.contains(&parts[parts.len() - 2]) {
        return true;
    }
    if lower.ends_with("_test.go") || lower.ends_with("_test.py") {
        return true;
    }
    if lower.starts_with("test_") && lower.ends_with(".py") {
        return true;
    }
    // C#/Java/Kotlin convention: `ThingTests.cs`, `ThingTest.java`. Case
    // sensitive on purpose so `Contest.cs` and `Latest.java` stay production.
    let Some((stem, ext)) = file.rsplit_once('.') else {
        return false;
    };
    matches!(ext, "cs" | "java" | "kt")
        && ["Tests", "Test"]
            .iter()
            .any(|suffix| stem.len() > suffix.len() && stem.ends_with(suffix))
}

/// Splits a name into lowercase words on `-`, `_`, `.` and camelCase
/// boundaries: `CTKO.LibTest` -> `ctko lib test`, `IDTests` -> `id tests`.
fn words(name: &str) -> Vec<String> {
    let mut out = Vec::new();
    for part in name.split(['-', '_', '.']).filter(|p| !p.is_empty()) {
        let chars: Vec<char> = part.chars().collect();
        let mut current = String::new();
        for (i, &c) in chars.iter().enumerate() {
            let prev = i.checked_sub(1).map(|p| chars[p]);
            let next = chars.get(i + 1).copied();
            let boundary = c.is_uppercase()
                && match prev {
                    Some(p) if p.is_lowercase() || p.is_ascii_digit() => true,
                    Some(p) if p.is_uppercase() => next.is_some_and(char::is_lowercase),
                    _ => false,
                };
            if boundary && !current.is_empty() {
                out.push(std::mem::take(&mut current));
            }
            current.extend(c.to_lowercase());
        }
        if !current.is_empty() {
            out.push(current);
        }
    }
    out
}
