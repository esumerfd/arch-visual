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

use std::io::ErrorKind;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
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

/// Bound for the channel between the recv thread and the UI-thread drain:
/// up to 256 parsed events may queue between two UI frames. At a human edit
/// rate this is far more headroom than a single Claude Code hook invocation
/// (Phase 7) will ever produce inside one frame gap -- the 257th event
/// arriving before the UI drains is dropped rather than queued without
/// limit (research/ARCHITECTURE.md Anti-Pattern 2).
///
/// Two distinct loss points sit upstream of the user ever seeing an event,
/// and only the second is visible to this crate's counters:
/// 1. The kernel's own receive buffer (measured `net.local.dgram.recvspace`
///    -- 4096 bytes on this machine, upstream of this code and not tunable
///    without a new `libc` dependency). Loss here happens before any of our
///    code runs and is invisible to [`Stats`] entirely.
/// 2. This channel. Loss here increments [`Stats::dropped`].
///
/// `Stats::dropped` therefore undercounts total loss under extreme load; it
/// is real, useful, and honestly scoped -- not a complete picture of every
/// datagram this process never got to see.
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

/// Three atomics counting the recv loop's outcomes. `received` is
/// incremented only by a successful, drained delivery; `discarded` counts a
/// sender problem (oversized, unparseable, or a caught panic during
/// delivery -- see [`handle_datagram`]); `dropped` counts a saturated UI
/// (the channel was full). Two different counters because they are two
/// different problems -- a broken sender versus a busy UI thread.
#[derive(Debug, Default)]
pub struct Stats {
    received: AtomicU64,
    discarded: AtomicU64,
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

/// Length guard, checked before any syscall (T-06-02-07): the only check
/// that is certainly wrong to attempt anything after, and cheaper than
/// creating a directory tree for a path that can never be bound.
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

/// Binds a `UnixDatagram` at `path`, handling every real-world failure mode:
///
/// 1. **Length guard first** (T-06-02-07).
/// 2. **Directory creation** (D-02).
/// 3. **Attempt the bind.** `AddrInUse` alone cannot distinguish "another
///    instance is running" from "the last run was force-quit" -- both look
///    identical on the filesystem, so an `AddrInUse` falls through to the
///    probe below rather than being reported directly.
/// 4. **The live-versus-dead probe** (T-06-02-02 / T-06-02-06):
///    `UnixDatagram::unbound().connect(path)`. A successful connect means a
///    live peer owns it -- return [`BindError::AlreadyRunning`] and,
///    critically, **do not unlink** (the single most important line in this
///    file: unconditionally deleting the file here would silently steal a
///    running instance's socket). A `ConnectionRefused` means the inode is
///    dead residue from an unclean exit (`kill -9`) -- remove it and retry
///    the bind exactly once. If that retry also loses to `AddrInUse`,
///    another process won the race between the unlink and the retry:
///    report `AlreadyRunning`, never loop, never retry again.
/// 5. **Harden permissions** (T-06-02-04, Pitfall 5) -- `0600`.
pub fn bind_at(path: &Path) -> Result<UnixDatagram, BindError> {
    check_path_length(path)?;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| BindError::Io {
            path: path.to_path_buf(),
            source,
        })?;
    }

    let socket = match UnixDatagram::bind(path) {
        Ok(socket) => socket,
        Err(e) if e.kind() == ErrorKind::AddrInUse => {
            // The live-versus-dead probe: connecting is the only way to
            // distinguish a live peer from a crash-left inode, since both
            // look identical to `stat`.
            let probe = UnixDatagram::unbound().map_err(|source| BindError::Io {
                path: path.to_path_buf(),
                source,
            })?;
            match probe.connect(path) {
                Ok(()) => {
                    // A live peer answered. This is the single most
                    // important branch in this file: do NOT unlink here --
                    // unconditionally deleting the file would silently
                    // steal a running instance's socket (T-06-02-02).
                    return Err(BindError::AlreadyRunning {
                        path: path.to_path_buf(),
                    });
                }
                Err(probe_err) if probe_err.kind() == ErrorKind::ConnectionRefused => {
                    // Dead residue from an unclean exit (kill -9). Remove
                    // it and retry the bind exactly once.
                    std::fs::remove_file(path).map_err(|source| BindError::Io {
                        path: path.to_path_buf(),
                        source,
                    })?;
                    match UnixDatagram::bind(path) {
                        Ok(socket) => socket,
                        Err(e) if e.kind() == ErrorKind::AddrInUse => {
                            // Another process won the race between the
                            // unlink and the retry bind. Never loop again --
                            // resolve into the safe, visible D-03 outcome.
                            return Err(BindError::AlreadyRunning {
                                path: path.to_path_buf(),
                            });
                        }
                        Err(source) => {
                            return Err(BindError::Io {
                                path: path.to_path_buf(),
                                source,
                            })
                        }
                    }
                }
                Err(source) => {
                    // The probe itself failed in some other way -- the
                    // diagnostic should point at the probe, not the
                    // original bind.
                    return Err(BindError::Io {
                        path: path.to_path_buf(),
                        source,
                    });
                }
            }
        }
        Err(source) => {
            return Err(BindError::Io {
                path: path.to_path_buf(),
                source,
            })
        }
    };

    // Harden permissions (T-06-02-04). The mode bits are set and asserted
    // by test, but whether Darwin's kernel enforces them on an AF_UNIX
    // connect is not something this test suite can prove -- a test cannot
    // become another user. Stated residual, not a gap.
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).map_err(|source| {
        BindError::Io {
            path: path.to_path_buf(),
            source,
        }
    })?;

    Ok(socket)
}

/// [`default_socket_path`] or [`BindError::NoHome`], then [`bind_at`].
pub fn bind_default() -> Result<UnixDatagram, BindError> {
    let path = default_socket_path().ok_or(BindError::NoHome)?;
    bind_at(&path)
}

/// What a delivery attempt reported back to [`handle_datagram`] -- lets it
/// tell "the UI is saturated" (a busy channel) from "the sender is broken"
/// (an unparseable or oversized datagram, or a caught panic), which is why
/// they land in two different [`Stats`] counters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Delivery {
    /// The event was handed to the channel successfully.
    Sent,
    /// The channel was full; the newest event was dropped.
    ChannelFull,
}

/// Whether [`recv_error_control`] says the recv loop should keep going or
/// end after a `recv` error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LoopControl {
    Continue,
    Stop,
}

/// Classifies a `recv` error kind: `Interrupted` (a signal arrived mid-call)
/// and `WouldBlock` (not expected on a blocking socket, but harmless if it
/// ever occurs) cost one iteration and nothing else; every other kind means
/// the socket itself is no longer usable (T-06-03-04) and the loop must
/// stop -- looping forever on a persistent error would spin a core, and
/// silently stopping on a transient one would lose the feature for no
/// reason. Pure, no I/O, unit-tested directly.
fn recv_error_control(kind: ErrorKind) -> LoopControl {
    match kind {
        ErrorKind::Interrupted | ErrorKind::WouldBlock => LoopControl::Continue,
        _ => LoopControl::Stop,
    }
}

/// The single per-message boundary between a raw datagram and a delivered
/// [`GraphEvent`] -- parse, deliver, count, absorb. Private to the module,
/// but reachable from the `#[cfg(test)]` module below (in this same file),
/// which is the whole reason it is extracted: neither the caught-panic
/// branch nor the channel-full branch can be driven through a real socket
/// from an integration test.
///
/// In order:
/// 1. A datagram larger than `seam_core::MAX_EVENT_BYTES` is discarded
///    without being parsed -- the recv buffer is one byte larger than that
///    ceiling precisely so a filled buffer is distinguishable from a legal
///    maximum-size message (T-06-03-08).
/// 2. The remainder -- deserializing the bytes and calling `deliver` -- runs
///    inside `catch_unwind`. `AssertUnwindSafe` is required because the
///    closure borrows `deliver` and `stats`; that is acceptable here and
///    only here, because this function's stack frame holds nothing whose
///    invariants a partial unwind could corrupt -- `stats` is atomics and
///    `deliver` is a `Fn`. This is defence in depth over the panic-free
///    discipline plan 06-01 already proved for the parser, not a substitute
///    for it (T-06-03-01).
/// 3. A rejection -- whether from the parser or from a caught panic -- is
///    counted in `stats.discarded` and returns **silently**: no write to
///    any output stream, at any level, for any rejected datagram. An
///    unauthenticated local sender controls both the content and the rate
///    of these; giving it a terminal-output primitive would be a
///    denial-of-service surface for a message that is by definition already
///    being thrown away (T-06-03-02).
/// 4. A successfully parsed event is handed to `deliver`. `Delivery::Sent`
///    increments nothing here -- an event that reached the channel but was
///    never drained has not been received by anything that matters, so
///    `Stats::received` is [`EventReceiver::drain`]'s alone to increment.
///    `Delivery::ChannelFull` increments `stats.dropped`.
fn handle_datagram(bytes: &[u8], deliver: &dyn Fn(GraphEvent) -> Delivery, stats: &Stats) {
    if bytes.len() > seam_core::MAX_EVENT_BYTES {
        stats.discarded.fetch_add(1, Ordering::SeqCst);
        return;
    }

    // seam_core's bytes-to-GraphEvent parser is the ONLY path from socket
    // bytes to an event -- no second validation copy exists anywhere in
    // this crate. The whole parse-and-deliver step is unwind-protected
    // (see this function's doc comment above) so a panic in either cannot
    // end the caller's receive loop.
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        seam_core::parse_datagram(bytes).map(deliver)
    }));

    match outcome {
        Ok(Ok(Delivery::Sent)) => {}
        Ok(Ok(Delivery::ChannelFull)) => {
            stats.dropped.fetch_add(1, Ordering::SeqCst);
        }
        Ok(Err(_)) => {
            // A typed rejection from the parser -- shape-invalid input.
            stats.discarded.fetch_add(1, Ordering::SeqCst);
        }
        Err(_) => {
            // A caught panic, from the parser or from `deliver` itself.
            stats.discarded.fetch_add(1, Ordering::SeqCst);
        }
    }
}

/// Spawns the background recv thread over `socket`, waking `ctx` on every
/// successfully-parsed, successfully-queued event. Returns an
/// [`EventReceiver`] carrying its own stats -- no process-global is touched.
///
/// The recv loop is the whole of EVENT-03's correctness: `try_send` (never a
/// blocking `send`), so a busy UI thread can never turn a hook process's
/// fire-and-forget write into a stall (research/ARCHITECTURE.md
/// Anti-Pattern 2). Every byte the socket hands back is routed through
/// [`handle_datagram`], the one per-message boundary that parses, delivers,
/// counts, and absorbs a panic, so nothing here can end this loop early
/// except an unrecoverable `recv` error (see [`recv_error_control`]).
pub fn spawn_receiver(socket: UnixDatagram, ctx: egui::Context) -> EventReceiver {
    let (tx, rx) = std::sync::mpsc::sync_channel::<GraphEvent>(CHANNEL_CAPACITY);
    let stats = Arc::new(Stats::default());
    let thread_stats = stats.clone();

    std::thread::spawn(move || {
        let deliver = |event: GraphEvent| -> Delivery {
            match tx.try_send(event) {
                Ok(()) => {
                    thread_stats.received.fetch_add(1, Ordering::SeqCst);
                    // Strictly inside the success branch: waking the UI
                    // thread for an event that was just dropped is pure
                    // waste.
                    ctx.request_repaint();
                    Delivery::Sent
                }
                Err(_) => Delivery::ChannelFull,
            }
        };

        // One byte larger than the ceiling so a full buffer is
        // distinguishable from a legal maximum-size message.
        let mut buf = [0u8; seam_core::MAX_EVENT_BYTES + 1];
        loop {
            match socket.recv(&mut buf) {
                Ok(n) => handle_datagram(&buf[..n], &deliver, &thread_stats),
                Err(e) => match recv_error_control(e.kind()) {
                    LoopControl::Continue => continue,
                    LoopControl::Stop => {
                        // A receive loop ending is a real, one-time,
                        // operational fact a developer needs to see --
                        // unlike a rejected message, this happens at most
                        // once per process lifetime.
                        eprintln!("event_stream: receive loop ending: {e}");
                        break;
                    }
                },
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

/// Pure and testable: names the exact socket path, states that another Seam
/// Explorer instance is already running and receiving live events, and
/// states that this instance is exiting. Gives the user something
/// actionable: if no other instance is actually running, removing that file
/// and relaunching will recover.
pub fn already_running_message(path: &Path) -> String {
    format!(
        "Seam Explorer is already running and receiving live events at {}.\n\
         This instance is exiting.\n\
         If no other instance is actually running, remove that file and relaunch to recover.",
        path.display()
    )
}

/// Writes [`already_running_message`] to stderr AND shows it in a blocking
/// native error dialog, then exits the process with a non-zero status.
/// Both channels deliberately: stderr for a terminal launch (`make
/// run-egui`), the dialog for a double-clicked `.app` bundle where stderr
/// goes nowhere a user will ever look. Exactly one production call site
/// (`main.rs`) and no test call site -- the same discipline
/// `settings::init` documents, for the same reason: a function that
/// terminates the process must be unreachable from any test.
pub fn exit_already_running(path: &Path) -> ! {
    let message = already_running_message(path);
    eprintln!("{message}");
    rfd::MessageDialog::new()
        .set_level(rfd::MessageLevel::Error)
        .set_title("Seam Explorer is already running")
        .set_description(message)
        .set_buttons(rfd::MessageButtons::Ok)
        .show();
    std::process::exit(1)
}

/// Plan 06-03: unit tests for [`handle_datagram`], [`recv_error_control`],
/// and the two branches (caught panic, channel-full) that cannot be driven
/// through a real socket from an integration test. See
/// `tests/event_stream.rs` for the real-socket hostile-input and
/// concurrency suites this module does not duplicate.
#[cfg(test)]
mod tests {
    use super::*;

    /// Extracts the body of a top-level `fn` by counting braces, so
    /// [`no_hostile_datagram_writes_to_the_terminal`] can inspect
    /// `handle_datagram`'s actual source rather than trusting a comment.
    fn extract_function_body(source: &str, fn_name: &str) -> String {
        let marker = format!("fn {fn_name}");
        let start = source
            .find(&marker)
            .unwrap_or_else(|| panic!("{fn_name} not found in source"));
        let after_start = &source[start..];
        let open_brace = after_start
            .find('{')
            .unwrap_or_else(|| panic!("{fn_name} has no opening brace"));
        let mut depth: i32 = 0;
        let mut end = after_start.len();
        for (i, c) in after_start[open_brace..].char_indices() {
            match c {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        end = open_brace + i + 1;
                        break;
                    }
                }
                _ => {}
            }
        }
        after_start[..end].to_string()
    }

    /// The `no_hostile_datagram_writes_to_the_terminal` requirement: an
    /// unauthenticated local sender controls both the content and the rate
    /// of every datagram this function discards -- any print statement in
    /// this per-message path would be a terminal/log/disk flooding
    /// primitive for output that is, by definition, already being thrown
    /// away (T-06-03-02). Asserted structurally against this file's own
    /// source rather than by capturing stdout/stderr.
    #[test]
    fn no_hostile_datagram_writes_to_the_terminal() {
        let source = include_str!("event_stream.rs");
        let body = extract_function_body(source, "handle_datagram");
        assert!(
            !body.contains("println!") && !body.contains("eprintln!") && !body.contains("dbg!"),
            "handle_datagram must never write to any output stream, got body:\n{body}"
        );
    }

    #[test]
    fn recv_error_control_stops_only_on_an_unrecoverable_error() {
        for kind in [ErrorKind::Interrupted, ErrorKind::WouldBlock] {
            assert_eq!(
                recv_error_control(kind),
                LoopControl::Continue,
                "{kind:?} must continue the receive loop"
            );
        }
        for kind in [
            ErrorKind::ConnectionAborted,
            ErrorKind::NotConnected,
            ErrorKind::Other,
        ] {
            assert_eq!(
                recv_error_control(kind),
                LoopControl::Stop,
                "{kind:?} must stop the receive loop"
            );
        }
    }

    /// Reaches the recovery branch by actually calling `handle_datagram`
    /// with a `deliver` closure that panics -- not asserted by reading, per
    /// PITFALLS Pitfall 6 (T-06-03-01).
    #[test]
    fn a_panic_inside_delivery_cannot_end_the_receive_loop() {
        // Silence the expected panic's backtrace so a passing test does not
        // look like a failing one in `--nocapture` output.
        let previous_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));

        let stats = Stats::default();
        let panicking: &dyn Fn(GraphEvent) -> Delivery =
            &|_event| panic!("deliberate test panic -- must be caught by handle_datagram");
        let bytes = seam_core::to_datagram(&GraphEvent::RemoveNode {
            id: "svc::panic".to_string(),
        });
        handle_datagram(&bytes, panicking, &stats);

        std::panic::set_hook(previous_hook);

        assert_eq!(
            stats.discarded(),
            1,
            "a caught panic inside delivery must be counted as a discard"
        );

        // Liveness probe: a second, non-panicking call must still deliver
        // normally -- the actual proof the loop survived.
        let delivered = std::cell::RefCell::new(None);
        let recording: &dyn Fn(GraphEvent) -> Delivery = &|event| {
            *delivered.borrow_mut() = Some(event);
            Delivery::Sent
        };
        let ok_event = GraphEvent::RemoveNode {
            id: "svc::after-panic".to_string(),
        };
        handle_datagram(&seam_core::to_datagram(&ok_event), recording, &stats);

        assert_eq!(
            delivered.into_inner(),
            Some(ok_event),
            "a non-panicking call after the caught panic must still deliver normally"
        );
    }

    /// The server-side half of the oversized-datagram requirement:
    /// `handle_datagram` must discard a slice over `MAX_EVENT_BYTES`
    /// regardless of what the kernel does. The sender-side half -- what the
    /// kernel actually does when asked to carry a payload this large -- is
    /// measured against a real bound socket in the same test so the SUMMARY
    /// can record which path this machine actually took.
    #[test]
    fn an_oversized_datagram_is_discarded_rather_than_truncated_into_a_lie() {
        let stats = Stats::default();
        let deliver: &dyn Fn(GraphEvent) -> Delivery = &|_| Delivery::Sent;
        let oversized = vec![b'a'; seam_core::MAX_EVENT_BYTES + 1];
        handle_datagram(&oversized, deliver, &stats);
        assert_eq!(
            stats.discarded(),
            1,
            "a slice one byte over MAX_EVENT_BYTES must be discarded and counted"
        );

        // Measure what the kernel actually does with a real send_to this
        // large on this machine (precondition: measured
        // net.local.dgram.maxdgram = 2048, MAX_EVENT_BYTES = 2048, so
        // MAX_EVENT_BYTES + 1 exceeds it).
        let dir =
            std::env::temp_dir().join(format!("es-evt-{}-oversize-kernel", std::process::id()));
        let path = dir.join("seam.sock");
        let _socket = bind_at(&path).expect("bind_at must succeed for a fresh path");
        let sender = UnixDatagram::unbound().expect("unbound socket must be constructible");
        let result = sender.send_to(&oversized, &path);
        match &result {
            Err(e) => {
                eprintln!(
                    "[an_oversized_datagram_is_discarded_rather_than_truncated_into_a_lie] \
                     kernel refused a {}-byte send_to with: {e} (raw_os_error={:?}) -- the \
                     server-side truncation branch above is unreachable from a well-behaved \
                     sender on this machine",
                    oversized.len(),
                    e.raw_os_error()
                );
            }
            Ok(sent) => {
                eprintln!(
                    "[an_oversized_datagram_is_discarded_rather_than_truncated_into_a_lie] \
                     kernel accepted the oversized send_to, reporting {sent} bytes sent (of {}) \
                     -- real truncation is possible on this machine",
                    oversized.len()
                );
            }
        }
        assert!(
            result.is_err() || matches!(result, Ok(sent) if sent < oversized.len()),
            "an oversized send_to must either be refused outright or truncated, never \
             silently accepted whole: got {result:?}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The deterministic backpressure unit test (Task 2): a channel that is
    /// always reported full must drop the newest event and count it, never
    /// touching `received` (drain()'s counter, not handle_datagram's).
    #[test]
    fn a_full_channel_drops_the_newest_and_counts_it() {
        let stats = Stats::default();
        let always_full: &dyn Fn(GraphEvent) -> Delivery = &|_| Delivery::ChannelFull;
        for i in 0..5 {
            let bytes = seam_core::to_datagram(&GraphEvent::RemoveNode {
                id: format!("svc::full-{i}"),
            });
            handle_datagram(&bytes, always_full, &stats);
        }
        assert_eq!(
            stats.dropped(),
            5,
            "every ChannelFull delivery must increment dropped by exactly one"
        );
        assert_eq!(
            stats.received(),
            0,
            "handle_datagram never increments received -- that is drain()'s job"
        );
    }
}
