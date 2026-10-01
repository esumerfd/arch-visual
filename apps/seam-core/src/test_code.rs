//! `is_test_path` / `is_test_symbol_name`: decide whether a node is test
//! code, so test code is never presented. Both [`crate::from_json`] and
//! [`crate::apply_add_node`] drop a node either flags.
//!
//! Path matching is by whole path segment or whole word, never by
//! substring: on a real 109k-node graph, "Estimate" contains "test", and a
//! substring rule hid most of the production services (see
//! `production_code_containing_test_letters_is_not_test_code` in
//! `test_code_test.rs`). Name matching (quick-260926-xbl) is a separate,
//! LOCKED whole-word-prefix-only rule over five symbol-name prefixes -- see
//! `TEST_NAME_PREFIXES` -- added because path alone misses a node like
//! `MockAgentRuntime` living in an ordinary production source file (see
//! `test_code_filter_test.rs`'s `classify_test_code_catches_the_production_path_test_name_shape`).

/// Directory names that mean test code only when they are the whole segment.
/// `spec` (singular) is here; `specs` (plural) deliberately is not -- a real
/// corpus has `docs/.../specs/*.md` design documents that must stay
/// production (see `production_code_containing_test_letters_is_not_test_code`).
const TEST_DIRS: [&str; 5] = ["spec", "e2e", "cypress", "__mocks__", "testdata"];

/// Words that mark a directory as test code wherever they appear in its name
/// (`render-tests`, `test-data`, `CTKO.LibTest`, `testUtil`, `App.Tests`,
/// `__tests__`).
const TEST_WORDS: [&str; 3] = ["test", "tests", "testing"];

/// The middle part of `name.<marker>.<ext>` (`foo.spec.ts`, `foo.test.sh`,
/// `foo.cy.ts`).
const TEST_FILE_MARKERS: [&str; 3] = ["spec", "test", "cy"];

/// A base filename's stem (before its last `.`, or the whole name) that
/// exactly equals one of these, case-insensitively, is test code regardless
/// of extension -- covers bare `test.rs`, `conftest.py`, `spec.js`.
const TEST_FILE_STEMS: [&str; 5] = ["test", "tests", "spec", "specs", "conftest"];

/// A stem starting with this, case-insensitively, is test code regardless of
/// extension -- the Python `test_helpers.py` convention, generalized to
/// every language (`test_helpers.go`, `test_helpers.rs`, not just `.py`).
const TEST_STEM_PREFIXES: [&str; 1] = ["test_"];

/// A stem ending with one of these, case-insensitively, is test code
/// regardless of extension -- generalizes the `.<marker>.` rule above to
/// two-part filenames with no third `.ts`-style segment (`helper_test.rs`).
const TEST_STEM_SUFFIXES: [&str; 4] = ["_test", "_spec", ".test", ".spec"];

/// D-02: the LOCKED whole-word test-symbol-name prefix set. Exactly these
/// five, case-insensitive, whole-word-prefix-only. Widening this to a
/// substring match anywhere in the name (e.g. matching `agent_mock_impl`)
/// was explicitly REJECTED during planning (quick-260926-xbl) -- the
/// conservative, whole-word-only rule specifically bounds false-positive
/// risk on legitimately-named production code (`MockupRenderer`,
/// `Attestation`, `ContestEntry`, `Testament`/`TESTAMENT` must never match).
/// Do not "improve" this into a substring match.
pub const TEST_NAME_PREFIXES: [&str; 5] = ["mock", "fake", "stub", "dummy", "test"];

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
    let stem = lower
        .rsplit_once('.')
        .map(|(stem, _ext)| stem)
        .unwrap_or(lower.as_str());

    if TEST_FILE_STEMS.contains(&stem) {
        return true;
    }
    if TEST_STEM_PREFIXES.iter().any(|p| stem.starts_with(p)) {
        return true;
    }
    if TEST_STEM_SUFFIXES.iter().any(|s| stem.ends_with(s)) {
        return true;
    }

    let parts: Vec<&str> = lower.split('.').collect();
    if parts.len() >= 3 && TEST_FILE_MARKERS.contains(&parts[parts.len() - 2]) {
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

/// True when `name` starts with one of the D-02 locked prefixes
/// (case-insensitive) AND that prefix ends at a real word boundary
/// (DP-XBL-02):
///
/// (a) the prefix IS the whole name (nothing follows it),
/// (b) the next character is not alphanumeric (`_`, `-`, `.`, space, `(`,
///     ...), or
/// (c) the next character is an uppercase letter AND the matched prefix's
///     own last character (as it actually appears in `name`) is lowercase --
///     a genuine camelCase transition. This lowercase requirement is what
///     keeps all-caps `TESTAMENT` out: its `TEST` is followed by an
///     uppercase `A`, but `TEST`'s own last character is the uppercase `T`,
///     so there is no camelCase transition.
///
/// A digit is alphanumeric and is deliberately NOT a boundary
/// (`Mock2Runtime` survives) -- the conservative reading of D-02.
///
/// The prefix comparison is byte-wise `eq_ignore_ascii_case` over
/// `prefix.len()` bytes. Because that comparison can only succeed when
/// those bytes in `name` are themselves ASCII, the index `prefix.len()` is
/// guaranteed to land on a char boundary -- so the following
/// `name[prefix.len()..].chars().next()` cannot panic, even on a multi-byte
/// (non-ASCII) identifier.
pub fn is_test_symbol_name(name: &str) -> bool {
    for prefix in TEST_NAME_PREFIXES {
        let plen = prefix.len();
        if name.len() < plen {
            continue;
        }
        if !name.as_bytes()[..plen].eq_ignore_ascii_case(prefix.as_bytes()) {
            continue;
        }
        // Safe: the byte-equality check above only succeeds when
        // name.as_bytes()[..plen] is ASCII, so `plen` is guaranteed to be a
        // char boundary in `name`.
        match name[plen..].chars().next() {
            None => return true,
            Some(next) if !next.is_alphanumeric() => return true,
            Some(next) if next.is_uppercase() => {
                let prefix_last_is_lower = name[..plen]
                    .chars()
                    .next_back()
                    .map(char::is_lowercase)
                    .unwrap_or(false);
                if prefix_last_is_lower {
                    return true;
                }
            }
            Some(_) => {}
        }
    }
    false
}
