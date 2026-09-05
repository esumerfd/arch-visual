//! The embedded Unix Domain **datagram** server (EVENT-01/02/03).
//!
//! `SOCK_DGRAM` is a locked roadmap decision, not a local preference: one
//! `send_to` is one `recv`, message boundaries are a kernel guarantee, and
//! there is deliberately no framing layer anywhere in this file or in the
//! future `apps/seam-client` (Phase 7). This file is the ONLY place in this
//! crate that names a socket type -- enforced by an acceptance grep in
//! `06-02-PLAN.md`.
//!
//! The socket lives at `~/.config/seam-explorer/seam.sock` per D-01,
//! resolved by the same XDG-then-HOME rule `settings.rs` uses for
//! `settings.json` -- see [`socket_path_from`], which mirrors
//! `settings::config_path_from` byte for byte.
//!
//! Task 1 gives [`bind_at`] a deliberately minimal body (create the
//! directory, bind, map any error to `Io`). Task 2 replaces the middle of
//! that function with the full stale-socket / single-instance lifecycle;
//! this doc comment and the `BindError` enum are written against that final
//! shape up front so the public API does not change shape between tasks.

use std::os::unix::ffi::OsStrExt;
use std::os::unix::net::UnixDatagram;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use seam_core::GraphEvent;

/// The socket's file name, sibling of `settings::CONFIG_FILE_NAME`. Reuses
/// `settings::CONFIG_DIR_NAME` for the directory rather than re-declaring
/// `"seam-explorer"` -- one string, one authority, so the socket and the
/// settings file cannot drift into different directories.
pub const SOCKET_FILE_NAME: &str = "seam.sock";

/// macOS/BSD's `sockaddr_un.sun_path` limit, in bytes. Linux's is 108, which
/// is irrelevant here (this is a macOS-only project, per `PROJECT.md`'s
/// Constraints), but the number should not look arbitrary. Enforced by
/// Task 2's length guard.
pub const MAX_SUN_PATH_BYTES: usize = 104;

/// Bound for the channel between the recv thread and the UI-thread drain.
pub const CHANNEL_CAPACITY: usize = 256;

/// Pure path-resolution core, byte-for-byte the same precedence logic as
/// `settings::config_path_from`, ending in [`SOCKET_FILE_NAME`] instead of
/// `settings::CONFIG_FILE_NAME`.
pub fn socket_path_from(xdg_config_home: Option<&str>, home: Option<&str>) -> Option<PathBuf> {
    let base = match xdg_config_home.map(str::trim) {
        Some(xdg) if !xdg.is_empty() => PathBuf::from(xdg),
        _ => PathBuf::from(home?).join(".config"),
    };
    Some(
        base.join(crate::settings::CONFIG_DIR_NAME)
            .join(SOCKET_FILE_NAME),
    )
}

/// The single impure wrapper reading the two env vars, with exactly one
/// production call site (`main.rs`).
pub fn default_socket_path() -> Option<PathBuf> {
    let xdg = std::env::var("XDG_CONFIG_HOME").ok();
    let home = std::env::var("HOME").ok();
    socket_path_from(xdg.as_deref(), home.as_deref())
}

/// Every way [`bind_at`] can fail to produce a bound, ready-to-use socket.
/// Declared with its full Task-2 shape now so the public API does not
/// change between tasks; Task 1's `bind_at` only ever produces `Io`.
#[derive(Debug)]
pub enum BindError {
    /// No `$HOME` (and no `$XDG_CONFIG_HOME`) available to resolve a path.
    NoHome,
    /// The resolved path exceeds [`MAX_SUN_PATH_BYTES`] -- rejected before
    /// any syscall, rather than surfacing as an opaque `EINVAL`. Wired by
    /// Task 2.
    PathTooLong { path: PathBuf, len: usize },
    /// Another running instance already holds this socket (D-03). The file
    /// is NEVER deleted in this case. Wired by Task 2.
    AlreadyRunning { path: PathBuf },
    /// Any other I/O failure (directory creation or bind).
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
}

impl std::fmt::Display for BindError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BindError::NoHome => {
                write!(f, "cannot resolve a socket path: no $HOME or $XDG_CONFIG_HOME")
            }
            BindError::PathTooLong { path, len } => write!(
                f,
                "socket path {path:?} is {len} bytes, over the {MAX_SUN_PATH_BYTES}-byte sun_path limit"
            ),
            BindError::AlreadyRunning { path } => {
                write!(f, "another instance is already running at {path:?}")
            }
            BindError::Io { path, source } => write!(f, "I/O error binding {path:?}: {source}"),
        }
    }
}

impl std::error::Error for BindError {}

/// Three atomics counting the recv loop's outcomes. Task 1 (this plan) only
/// ever increments `received`; `discarded` (parse rejections) and `dropped`
/// (channel-full drops) exist now so plan 06-03 adds behavior, not API.
#[derive(Debug, Default)]
pub struct Stats {
    received: AtomicU64,
    #[allow(dead_code)] // written by plan 06-03
    discarded: AtomicU64,
    #[allow(dead_code)] // written by plan 06-03
    dropped: AtomicU64,
}

impl Stats {
    pub fn received(&self) -> u64 {
        self.received.load(Ordering::SeqCst)
    }

    pub fn discarded(&self) -> u64 {
        self.discarded.load(Ordering::SeqCst)
    }

    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::SeqCst)
    }
}

/// A receiver bound to its own channel and its own [`Stats`] -- no
/// process-global is touched by [`spawn_receiver`] itself, which is what
/// lets `spawn_receiver_hands_back_isolated_stats` run in parallel with
/// every other test with no shared-state race.
pub struct EventReceiver {
    rx: std::sync::mpsc::Receiver<GraphEvent>,
    stats: Arc<Stats>,
}

impl EventReceiver {
    /// Drains every event currently queued, non-blocking. Returns an empty
    /// `Vec` immediately if nothing is queued -- this is the ONLY UI-thread
    /// contact point with the channel, and it must never block
    /// (T-06-02-01).
    pub fn drain(&self) -> Vec<GraphEvent> {
        let mut events = Vec::new();
        // Stops on Empty or Disconnected alike -- both mean "nothing more
        // to hand back right now", non-blocking by construction.
        while let Ok(event) = self.rx.try_recv() {
            events.push(event);
        }
        events
    }

    pub fn stats(&self) -> &Stats {
        &self.stats
    }
}

/// Length guard, checked before any syscall (T-06-02-07). Task 1 does not
/// yet call this from [`bind_at`] -- Task 2 wires it in as the first step
/// of the full lifecycle.
#[allow(dead_code)] // wired into bind_at by Task 2
fn check_path_length(path: &Path) -> Result<(), BindError> {
    let len = path.as_os_str().as_bytes().len();
    if len > MAX_SUN_PATH_BYTES {
        return Err(BindError::PathTooLong {
            path: path.to_path_buf(),
            len,
        });
    }
    Ok(())
}

/// Binds a `UnixDatagram` at `path`. Task 1's body: create the parent
/// directory (D-02), then bind, mapping any error into [`BindError::Io`].
/// Task 2 replaces the middle of this function with the full
/// stale-socket / single-instance lifecycle (length guard, live/dead probe,
/// permission hardening) -- deliberately not pre-built here.
pub fn bind_at(path: &Path) -> Result<UnixDatagram, BindError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| BindError::Io {
            path: path.to_path_buf(),
            source,
        })?;
    }

    UnixDatagram::bind(path).map_err(|source| BindError::Io {
        path: path.to_path_buf(),
        source,
    })
}

/// [`default_socket_path`] or [`BindError::NoHome`], then [`bind_at`].
pub fn bind_default() -> Result<UnixDatagram, BindError> {
    let path = default_socket_path().ok_or(BindError::NoHome)?;
    bind_at(&path)
}

/// Spawns the background recv thread over `socket`, waking `ctx` on every
/// successfully-parsed, successfully-queued event. Returns an
/// [`EventReceiver`] carrying its own stats -- no process-global is touched.
///
/// The recv loop, which is the whole of EVENT-03's correctness: `try_send`
/// (never a blocking `send`), so a busy UI thread can never turn a hook
/// process's fire-and-forget write into a stall (research/ARCHITECTURE.md
/// Anti-Pattern 2). `seam_core`'s bytes-to-[`GraphEvent`] parser (called
/// once, below) is the ONLY path from socket bytes to an event -- no second
/// validation exists here.
pub fn spawn_receiver(socket: UnixDatagram, ctx: egui::Context) -> EventReceiver {
    let (tx, rx) = std::sync::mpsc::sync_channel::<GraphEvent>(CHANNEL_CAPACITY);
    let stats = Arc::new(Stats::default());
    let thread_stats = stats.clone();

    std::thread::spawn(move || {
        // One byte larger than the ceiling so a full buffer is
        // distinguishable from a legal maximum-size message.
        let mut buf = [0u8; seam_core::MAX_EVENT_BYTES + 1];
        // Ends the thread on any recv error (e.g. the socket is dropped) --
        // a `while let` rather than a `loop`/`match`/`break`, per clippy.
        while let Ok(n) = socket.recv(&mut buf) {
            if n > seam_core::MAX_EVENT_BYTES {
                // Possible-truncation signal. Counter and dedicated test
                // are plan 06-03's; Task 1 leaves this a bare continue.
                continue;
            }
            match seam_core::parse_datagram(&buf[..n]) {
                Ok(event) => {
                    if tx.try_send(event).is_ok() {
                        thread_stats.received.fetch_add(1, Ordering::SeqCst);
                        ctx.request_repaint();
                    }
                    // A try_send failure (channel full) is plan 06-03's
                    // dropped-counter territory; Task 1 leaves this a bare
                    // no-op.
                }
                Err(_) => {
                    // Parse rejection: plan 06-03 owns the discarded
                    // counter and its dedicated test.
                    continue;
                }
            }
        }
    });

    EventReceiver { rx, stats }
}

static RECEIVER: OnceLock<Mutex<Option<EventReceiver>>> = OnceLock::new();

fn receiver_lock() -> &'static Mutex<Option<EventReceiver>> {
    RECEIVER.get_or_init(|| Mutex::new(None))
}

/// Spawns the recv thread via [`spawn_receiver`] and stores the resulting
/// [`EventReceiver`] in a process-global, following `settings.rs`'s
/// `store_lock()` shape and its poison-tolerant lock idiom. A `Mutex`
/// (not an `RwLock`) because `mpsc::Receiver` is `Send` but not `Sync`.
pub fn serve(socket: UnixDatagram, ctx: egui::Context) {
    let receiver = spawn_receiver(socket, ctx);
    let mut guard = receiver_lock().lock().unwrap_or_else(|e| e.into_inner());
    *guard = Some(receiver);
}

/// The global-backed drain. Returns an empty `Vec` when [`serve`] was never
/// called, so this call site is safe in every test and in a hypothetical
/// bind-failure startup.
pub fn drain() -> Vec<GraphEvent> {
    let guard = receiver_lock().lock().unwrap_or_else(|e| e.into_inner());
    match guard.as_ref() {
        Some(receiver) => receiver.drain(),
        None => Vec::new(),
    }
}

pub fn received_count() -> u64 {
    let guard = receiver_lock().lock().unwrap_or_else(|e| e.into_inner());
    guard.as_ref().map(|r| r.stats().received()).unwrap_or(0)
}

pub fn discarded_count() -> u64 {
    let guard = receiver_lock().lock().unwrap_or_else(|e| e.into_inner());
    guard.as_ref().map(|r| r.stats().discarded()).unwrap_or(0)
}

pub fn dropped_count() -> u64 {
    let guard = receiver_lock().lock().unwrap_or_else(|e| e.into_inner());
    guard.as_ref().map(|r| r.stats().dropped()).unwrap_or(0)
}
