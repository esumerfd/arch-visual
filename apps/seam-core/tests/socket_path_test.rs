//! Failing (RED) tests for `seam_core::socket`. See 07-01-PLAN.md Task 3's
//! `<behavior>` block for the exact contract these assert. The relocation
//! lands in the following commit (GREEN).
//!
//! The point of this module living in `seam-core` at all: two separate
//! processes -- the running app and the dependency-light `apps/seam-client`
//! hook binary (Phase 7) -- must agree on ONE socket path. These tests are
//! reachable from a crate with no GUI framework anywhere in its dependency
//! tree, which is precisely the property that makes the relocation worth
//! doing.

use seam_core::{socket_path_from, CONFIG_DIR_NAME, MAX_SUN_PATH_BYTES};
use std::path::PathBuf;

#[test]
fn an_explicit_xdg_base_wins() {
    assert_eq!(
        socket_path_from(Some("/x"), Some("/home/u")),
        Some(PathBuf::from("/x/seam-explorer/seam.sock"))
    );
}

#[test]
fn a_blank_or_whitespace_xdg_base_falls_back_to_home() {
    let expected = Some(PathBuf::from("/home/u/.config/seam-explorer/seam.sock"));
    assert_eq!(socket_path_from(Some("   "), Some("/home/u")), expected);
    assert_eq!(socket_path_from(None, Some("/home/u")), expected);
}

#[test]
fn no_base_at_all_resolves_to_nothing() {
    assert_eq!(socket_path_from(None, None), None);
}

/// The socket and the settings file must provably land in the same
/// directory. If they ever disagree the app binds one place and clients send
/// to another, failing silently with no error anywhere (T-07-01-04).
#[test]
fn the_socket_and_the_settings_file_are_siblings() {
    let path = socket_path_from(Some("/x"), None).expect("an explicit xdg base always resolves");
    let parent = path.parent().expect("a resolved socket path has a parent");
    assert!(
        parent.ends_with(CONFIG_DIR_NAME),
        "socket parent {parent:?} must be the shared config directory {CONFIG_DIR_NAME:?}"
    );
}

/// A canary, not a coincidence: a future rename of the directory or file
/// name that blows the `sockaddr_un.sun_path` ceiling fails HERE, loudly,
/// rather than as an opaque bind error at runtime.
#[test]
fn a_realistic_resolved_path_fits_the_sun_path_ceiling() {
    let path = socket_path_from(None, Some("/Users/someone")).expect("a real home always resolves");
    let byte_length = path.as_os_str().len();
    assert!(
        byte_length <= MAX_SUN_PATH_BYTES,
        "a realistic resolved socket path is {byte_length} bytes, over the \
         {MAX_SUN_PATH_BYTES}-byte sun_path ceiling: {path:?}"
    );
}
