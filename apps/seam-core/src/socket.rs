//! Where the app's Unix Domain datagram socket lives, and the names that
//! decide it. Relocated here from `seam-explorer-egui`'s `event_stream.rs`
//! by 07-01; the app now reaches these same five items through a re-export,
//! so the resolved path is byte-identical to what Phase 6 shipped.
//!
//! **An honest note on the tension.** A config-directory name is
//! app-shell-flavoured, and this crate's own header calls itself
//! app-shell-free. It lives here anyway because TWO SEPARATE PROCESSES must
//! agree on ONE path: the running app, which binds the socket, and
//! `apps/seam-client` (Phase 7), a hook binary that sends to it. The
//! alternative -- having that hook binary depend on the app crate, whose
//! `[lib]` target does export these functions today -- would drag an entire
//! GUI framework stack (`eframe`/`egui`/`egui_graphs`/`rfd`) into a binary
//! whose whole job is read stdin, maybe write one datagram, exit fast. That
//! trade is the reason for the placement. This is a decision, not a smell.
//!
//! `CHANNEL_CAPACITY` deliberately did NOT move: it describes the app's own
//! recv-thread-to-UI-thread handoff, and no other process has any business
//! knowing it.

use std::path::PathBuf;

/// The config directory both the socket and `settings.json` live in. This is
/// the ONE authority for the string in the whole workspace -- the app's
/// `settings::CONFIG_DIR_NAME` is initialized from this constant rather than
/// declaring a second independent literal, so the socket and the settings
/// file cannot drift into different directories (T-07-01-04).
pub const CONFIG_DIR_NAME: &str = "seam-explorer";

/// The socket's file name, sibling of the app's `settings::CONFIG_FILE_NAME`.
pub const SOCKET_FILE_NAME: &str = "seam.sock";

/// macOS/BSD's `sockaddr_un.sun_path` limit, in bytes. Linux's is 108, which
/// is irrelevant here (this is a macOS-only project, per `PROJECT.md`'s
/// Constraints), but the number should not look arbitrary. Enforced by the
/// app's own bind-time length guard.
pub const MAX_SUN_PATH_BYTES: usize = 104;

/// Pure path-resolution core, byte-for-byte the same precedence logic as the
/// app's `settings::config_path_from`, ending in [`SOCKET_FILE_NAME`] instead
/// of `settings::CONFIG_FILE_NAME`.
pub fn socket_path_from(xdg_config_home: Option<&str>, home: Option<&str>) -> Option<PathBuf> {
    let base = match xdg_config_home.map(str::trim) {
        Some(xdg) if !xdg.is_empty() => PathBuf::from(xdg),
        _ => PathBuf::from(home?).join(".config"),
    };
    Some(base.join(CONFIG_DIR_NAME).join(SOCKET_FILE_NAME))
}

/// The single impure wrapper reading the two env vars. One production call
/// site in the app (`main.rs`, via the re-export); `apps/seam-client` will be
/// the second, in a different process.
pub fn default_socket_path() -> Option<PathBuf> {
    let xdg = std::env::var("XDG_CONFIG_HOME").ok();
    let home = std::env::var("HOME").ok();
    socket_path_from(xdg.as_deref(), home.as_deref())
}
