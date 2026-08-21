//! User-editable settings for `seam-explorer-egui`, persisted at
//! `~/.config/seam-explorer/settings.json`.
//!
//! This is a SECOND, separate persistence mechanism from `eframe::Storage`'s
//! `app.ron` (see `app.rs`'s "Persistence discipline" doc comment, T-05-04 /
//! D-14): `app.ron` lives under the Apple-conventional
//! `~/Library/Application Support/Seam-Explorer/` and keeps exactly one
//! field, `has_seen_trace_onboarding`, round-tripped by `eframe` itself.
//! This module keeps two different fields (the Open-file command and the
//! append-line-number flag) in a plain JSON file at the user's explicitly,
//! deliberately requested location: "Use the mac standard ~/.config/seam-explorer
//! creating a file there." That is NOT `~/Library/Application Support/`, and
//! it is NOT `app.ron`. Do not "correct" the path to the Apple convention --
//! the user asked for this one, verbatim, this session.
//!
//! The process-global store here is unbound until `main.rs` calls [`init`]
//! with a real path. `store` on an unbound global updates memory only and
//! writes nothing to disk. `init` has exactly one call site in the whole
//! crate (`main.rs`, unreachable from any test), so no test run can ever
//! write to a developer's real `~/.config/seam-explorer/settings.json` --
//! tests that need on-disk persistence call `init` with a temp directory
//! explicitly.

use std::path::{Path, PathBuf};
use std::sync::{OnceLock, RwLock};

pub const CONFIG_DIR_NAME: &str = "seam-explorer";
pub const CONFIG_FILE_NAME: &str = "settings.json";

/// The two-field settings record. `#[serde(default)]` at the container level
/// (matching `SeamExplorerApp`'s own use of this attribute, see `app.rs`)
/// means a settings file written by a future version with extra keys, or an
/// older one missing a key, still loads instead of failing to deserialize.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Settings {
    pub open_file_command: String,
    pub append_line_number: bool,
}

/// Pure path-resolution core: `$XDG_CONFIG_HOME` when present and non-empty
/// (after trimming), else `$HOME/.config`, else `None`. Takes both
/// environment values as parameters so the XDG-vs-HOME precedence is
/// unit-testable without touching real process environment variables.
///
/// STUB (Task 1 / RED): always returns `None`.
pub fn config_path_from(_xdg_config_home: Option<&str>, _home: Option<&str>) -> Option<PathBuf> {
    None
}

/// The only impure part of path resolution: reads `$XDG_CONFIG_HOME` and
/// `$HOME` from the real process environment and delegates to
/// [`config_path_from`]. Exactly one production call site (`main.rs`) and no
/// test call site -- enforced by a grep gate in this plan's `<verify>`.
///
/// STUB (Task 1 / RED): always returns `None`.
pub fn default_config_path() -> Option<PathBuf> {
    None
}

/// Tolerant parse: any malformed, truncated, or unexpected-shape JSON
/// degrades silently to [`Settings::default`] rather than surfacing an
/// error or panicking. A settings file is a convenience; a two-field
/// convenience that can brick startup is a bad trade.
///
/// STUB (Task 1 / RED): always returns the default, regardless of `json`.
pub fn parse(_json: &str) -> Settings {
    Settings::default()
}

/// Pretty-printed JSON with a trailing newline, so the file stays
/// hand-editable.
///
/// STUB (Task 1 / RED): always returns an empty string.
pub fn to_json(_s: &Settings) -> String {
    String::new()
}

/// Reads `path` and parses it via [`parse`]; any IO error (missing file,
/// unreadable, a directory) also degrades to [`Settings::default`] -- a
/// missing settings file is exactly as fine as a corrupt one.
///
/// STUB (Task 1 / RED): always returns the default, regardless of `path`.
pub fn load_from(_path: &Path) -> Settings {
    Settings::default()
}

/// Writes `s` to `path` as JSON, creating parent directories as needed.
/// Returns the `io::Result` so the caller can decide what to do with a
/// write failure; nothing in this crate treats one as fatal.
///
/// STUB (Task 1 / RED): always reports success without writing anything.
pub fn save_to(_path: &Path, _s: &Settings) -> std::io::Result<()> {
    Ok(())
}

/// Process-global settings store. Holds no bound path until [`init`] sets
/// one -- see this module's doc comment for why that is a deliberate
/// test-safety mechanism, not an accident.
struct Store {
    settings: Settings,
    path: Option<PathBuf>,
}

static STORE: OnceLock<RwLock<Store>> = OnceLock::new();

/// Binds the global store to `path`, loading whatever is already there (or
/// defaults, if nothing is). Called from exactly one place in the whole
/// crate: `main.rs`, before `eframe::run_native`.
///
/// STUB (Task 1 / RED): does nothing.
pub fn init(_path: PathBuf) {}

/// The path the global store is currently bound to, or `None` if [`init`]
/// has not been called (or the process is a test, which never calls it).
///
/// STUB (Task 1 / RED): always returns `None`.
pub fn bound_path() -> Option<PathBuf> {
    None
}

/// The current in-memory settings value. Cheap to call -- clones out of the
/// lock rather than holding it.
///
/// STUB (Task 1 / RED): always returns the default.
pub fn current() -> Settings {
    Settings::default()
}

/// Replaces the current settings value. If a path is bound, writes through
/// to disk on this same call (never deferred to app exit -- there is no
/// `eframe::App::save` hook reachable from this module) and returns
/// `Some(result)`. If no path is bound, updates memory only and returns
/// `None`.
///
/// STUB (Task 1 / RED): does nothing and always returns `None`.
pub fn store(_s: Settings) -> Option<std::io::Result<()>> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Builds a per-test-unique temp directory under the OS temp dir, so
    /// parallel test runs cannot collide. No `tempfile` dependency (see
    /// `<design_decision>` -- zero new dependencies).
    fn temp_dir(unique: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "seam-explorer-{}-{}",
            std::process::id(),
            unique
        ))
    }

    #[test]
    fn default_settings_are_blank_command_and_flag_off() {
        let s = Settings::default();
        assert_eq!(s.open_file_command, "", "default command must be blank");
        assert!(!s.append_line_number, "default flag must be off");
    }

    #[test]
    fn settings_round_trip_through_json() {
        let configured = Settings {
            open_file_command: "code -g".to_string(),
            append_line_number: true,
        };
        assert_eq!(
            parse(&to_json(&configured)),
            configured,
            "a configured settings value must survive a to_json/parse round trip"
        );

        let default = Settings::default();
        assert_eq!(
            parse(&to_json(&default)),
            default,
            "the default settings value must also survive a to_json/parse round trip"
        );
    }

    #[test]
    fn parse_tolerates_every_broken_file_shape() {
        for bad in [
            "",
            "not json",
            "{",
            "[]",
            "null",
            r#"{"open_file_command": 42}"#,
            r#"{"unknown_key": true}"#,
        ] {
            assert_eq!(
                parse(bad),
                Settings::default(),
                "broken shape {bad:?} must degrade to defaults, never panic"
            );
        }

        // A file carrying one valid known key plus one unknown key: the
        // known key's value must survive, not be discarded wholesale along
        // with the unrecognised one.
        let mixed = parse(r#"{"append_line_number": true, "future_key": 1}"#);
        assert!(
            mixed.append_line_number,
            "a valid key alongside an unknown key must still be honored, got {mixed:?}"
        );
    }

    #[test]
    fn load_from_a_missing_file_is_defaults() {
        let dir = temp_dir("load-missing");
        let path = dir.join("does-not-exist.json");
        assert!(!path.exists(), "fixture precondition: path must not exist yet");

        assert_eq!(load_from(&path), Settings::default());
        assert!(
            !path.exists(),
            "load_from must never create the file it failed to find"
        );
    }

    #[test]
    fn save_to_then_load_from_round_trips_on_disk() {
        let dir = temp_dir("save-load-roundtrip");
        let path = dir.join("nested").join("settings.json");
        assert!(!dir.exists(), "fixture precondition: dir must not exist yet");

        let original = Settings {
            open_file_command: "subl -n".to_string(),
            append_line_number: true,
        };
        save_to(&path, &original)
            .expect("save_to must succeed into a not-yet-existing nested directory");

        let raw =
            fs::read_to_string(&path).expect("save_to must have created a readable file at path");
        let value: serde_json::Value =
            serde_json::from_str(&raw).expect("saved file must be valid JSON");
        assert!(
            value.get("open_file_command").is_some(),
            "saved JSON must contain open_file_command, got {raw}"
        );
        assert!(
            value.get("append_line_number").is_some(),
            "saved JSON must contain append_line_number, got {raw}"
        );

        assert_eq!(load_from(&path), original);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn config_path_from_prefers_xdg_then_home() {
        assert_eq!(
            config_path_from(Some("/x"), Some("/h")),
            Some(PathBuf::from("/x/seam-explorer/settings.json")),
            "XDG_CONFIG_HOME must win when present and non-empty"
        );
        assert_eq!(
            config_path_from(None, Some("/h")),
            Some(PathBuf::from("/h/.config/seam-explorer/settings.json")),
            "with XDG_CONFIG_HOME unset, HOME/.config/seam-explorer/settings.json is the user's \
             explicitly requested location -- verbatim: \"Use the mac standard \
             ~/.config/seam-explorer creating a file there.\""
        );
        assert_eq!(
            config_path_from(Some(""), Some("/h")),
            Some(PathBuf::from("/h/.config/seam-explorer/settings.json")),
            "an empty (but present) XDG_CONFIG_HOME must be treated the same as unset"
        );
        assert_eq!(
            config_path_from(None, None),
            None,
            "with neither variable available, there is no path to resolve"
        );
    }

    #[test]
    fn store_on_an_unbound_global_writes_nothing() {
        // Read bound_path() first and assert on store's return value
        // consistently with what it finds, rather than assuming the global
        // is pristine -- meaningful regardless of test execution order.
        let value = Settings {
            open_file_command: "seam-explorer-egui-store-unbound-probe".to_string(),
            append_line_number: true,
        };
        match bound_path() {
            None => {
                assert!(
                    store(value).is_none(),
                    "store on an unbound global must return None -- nothing was written to disk"
                );
            }
            Some(p) => {
                let real = default_config_path();
                assert_ne!(
                    Some(p.clone()),
                    real,
                    "a test-bound path must never equal the real config path -- got {p:?}"
                );
                match store(value) {
                    Some(Ok(())) => {}
                    other => panic!("expected Some(Ok(())) when a path is bound, got {other:?}"),
                }
            }
        }
    }

    #[test]
    fn init_binds_a_path_and_loads_it() {
        let dir = temp_dir("init-binds");
        let path = dir.join("settings.json");
        let written = Settings {
            open_file_command: "idea".to_string(),
            append_line_number: false,
        };
        save_to(&path, &written)
            .expect("fixture setup must be able to write the file init() will read");

        init(path.clone());

        assert_eq!(current(), written, "init must load what was on disk");
        assert_eq!(bound_path(), Some(path.clone()), "init must bind the given path");

        let modified = Settings {
            open_file_command: "code -g".to_string(),
            append_line_number: true,
        };
        let result = store(modified.clone());
        assert!(
            matches!(result, Some(Ok(()))),
            "store on a bound global must write through and return Some(Ok(())), got {result:?}"
        );
        assert_eq!(
            load_from(&path),
            modified,
            "store must have written the modified value to the bound path"
        );

        let _ = fs::remove_dir_all(&dir);
    }
}
