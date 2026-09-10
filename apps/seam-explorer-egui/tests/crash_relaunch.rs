//! ROADMAP SC-1, end to end and headless (plan 10-01).
//!
//! Force-quit a receiving `seam-explorer-egui` mid-stream while a hook client
//! keeps firing, relaunch it, and get events again with no manual cleanup.
//!
//! **Why this is possible without a display.** `main.rs` binds the event
//! socket entirely BEFORE `eframe::run_native` and outside the creation
//! closure (main.rs:42-51), so the whole bind/crash/relaunch lifecycle is
//! exercisable in a plain `cargo test` process. Nothing here needs a window.
//! `the_relaunched_app_receives_applies_and_records_a_real_client_event`'s
//! structural gate keeps that ordering checkable.
//!
//! **Why a REAL child process, and not an in-process `EventReceiver` drop.**
//! The cheaper-looking route -- bind, `spawn_receiver`, then drop the
//! `EventReceiver` handle to "simulate" a crash -- cannot work. In
//! `event_stream.rs:400-411` the receive thread's `deliver` closure matches on
//! `tx.try_send(event)` and maps EVERY `Err(_)` -- including the
//! `TrySendError::Disconnected` that dropping the receiver produces -- to
//! `Delivery::ChannelFull`, a counted, NON-FATAL outcome. So the recv loop
//! carries on after the handle is gone: the socket stays bound, the thread
//! stays alive, and nothing about the process resembles a crashed one. An
//! in-process drop simulates a full channel, not a `kill -9`. Only a separate
//! OS process that can actually be SIGKILLed produces the state SC-1 is about
//! -- a socket inode surviving with no owning process -- which is why
//! `examples/headless_receiver.rs` exists at all.
//!
//! **What is new here.** No prior test combines BOTH processes' lifecycles.
//! 06-02's `a_socket_left_behind_by_a_crash_does_not_block_the_next_launch`
//! drops a socket that was never serving; 07-04's `seam_client_bridge.rs` runs
//! a real client against a receiver that never dies. This file kills a
//! receiver that was genuinely RECEIVING, with a real client firing on both
//! sides of the outage.
//!
//! Additive by construction: `tests/event_stream.rs` is Phase 6's regression
//! net and is not touched.

mod common;

use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use egui_kittest::Harness;
use seam_core::GraphEvent;
use seam_explorer_egui::app::SeamExplorerApp;
use seam_explorer_egui::event_stream;

use common::{
    loaded_app, run_client, temp_config_home, wait_until, write_payload, HOOK_FILE_PATH,
    HOOK_REPO_RELATIVE, SOURCE_PATHS_FIXTURE,
};

/// Serializes the tests in this file that touch the process-global receiver,
/// following `timeline_reconstruction.rs`'s single-lock-per-file discipline.
///
/// Read this for what it actually is: a plain PROCESS-LOCAL mutex, not a file
/// lock and not cross-process anything. It serializes tests within THIS test
/// binary and does nothing at all for a second `cargo test` process or for the
/// child processes this file spawns. Cross-process safety here comes entirely
/// from `common::temp_config_home` embedding `std::process::id()`, so every
/// process gets its own socket path.
static SERVE_TEST_LOCK: Mutex<()> = Mutex::new(());

/// Generous: this waits on a real process spawn plus a real socket hop, and it
/// only ever runs to completion when something has genuinely gone wrong.
const ARRIVAL_DEADLINE: Duration = Duration::from_secs(10);

/// How long to wait for the spawned example to finish binding and create its
/// log file. Same reasoning as `ARRIVAL_DEADLINE`.
const READY_DEADLINE: Duration = Duration::from_secs(10);

/// CLIENT-04's budget, the same number `fail_open.rs` enforces. SC-1's "hook
/// clients keep firing" half is only true if firing into the gap stays free.
const BUDGET: Duration = Duration::from_millis(200);

/// How long to watch a log that should not be growing. Long enough that a
/// slow-but-real write would still be caught.
const QUIET_WINDOW: Duration = Duration::from_millis(500);

// ---------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------

/// A new-file `Write` body declaring exactly one `pub fn`.
///
/// Deliberately the same shape `seam_client_bridge.rs` already proved yields
/// exactly ONE `AddNode` and nothing else, so a line count in the receiver's
/// log is an unambiguous event count.
fn declares(symbol: &str) -> String {
    format!("//! A tiny module.\n\npub fn {symbol}(bytes: &[u8]) -> u32 {{\n    bytes.len() as u32\n}}\n")
}

/// The node id the client will advertise for `declares(symbol)`, derived from
/// what this test itself sends rather than read back out of the thing under
/// test. `timeline_reconstruction.rs`'s expectation discipline: an expectation
/// read out of the system under test cannot fail.
fn expected_node_id(symbol: &str) -> String {
    format!("{HOOK_REPO_RELATIVE}::{symbol}")
}

fn assert_sun_path_fits(path: &Path) {
    let len = path.as_os_str().as_bytes().len();
    assert!(
        len <= seam_core::MAX_SUN_PATH_BYTES,
        "socket path is {len} bytes, over the {} byte sun_path ceiling: {}",
        seam_core::MAX_SUN_PATH_BYTES,
        path.display()
    );
}

/// Locates the `headless_receiver` example, BUILDING it if it is not there
/// yet -- the same discipline (and for the same reason) as
/// `common::client_binary`.
///
/// Skipping is NOT an option: a test that quietly skips when it cannot find
/// its fixture reports green while proving nothing (T-10-01-04, inherited from
/// T-07-04-06), so every failure path below panics with the exact path it
/// looked at.
fn example_binary() -> PathBuf {
    let test_exe = std::env::current_exe().expect("the running test binary must have a path");
    // .../target/<profile>/deps/crash_relaunch-<hash>
    let profile_dir = test_exe
        .parent()
        .and_then(Path::parent)
        .unwrap_or_else(|| {
            panic!(
                "expected the test binary at target/<profile>/deps/..., got {}",
                test_exe.display()
            )
        })
        .to_path_buf();
    let candidate = profile_dir.join("examples").join("headless_receiver");
    if candidate.is_file() {
        return candidate;
    }

    let workspace_manifest = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("Cargo.toml");
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let mut build = Command::new(&cargo);
    build
        .arg("build")
        .arg("--example")
        .arg("headless_receiver")
        .arg("-p")
        .arg("seam-explorer-egui")
        .arg("--manifest-path")
        .arg(&workspace_manifest);
    if profile_dir
        .file_name()
        .is_some_and(|name| name == "release")
    {
        build.arg("--release");
    }
    let status = build.status().unwrap_or_else(|e| {
        panic!("could not run `{cargo} build --example headless_receiver`: {e}");
    });
    assert!(
        status.success(),
        "`{cargo} build --example headless_receiver` failed with {status:?}; expected the binary \
         at {}",
        candidate.display()
    );
    assert!(
        candidate.is_file(),
        "built headless_receiver but no binary is at {} -- this test cannot be skipped, so the \
         path resolution above is what needs fixing",
        candidate.display()
    );
    candidate
}

/// A spawned `headless_receiver` whose `Drop` kills and REAPS it.
///
/// A guard rather than a kill call at the end of each test on purpose
/// (T-10-01-03): an assertion that fails unwinds past any trailing cleanup,
/// and a surviving child holds the socket and poisons every later run against
/// the same path.
struct ReceiverProcess {
    child: Child,
    reaped: bool,
}

impl ReceiverProcess {
    /// Spawns the example and waits for it to signal readiness by creating its
    /// log file -- which it does only AFTER a successful `bind_at`, so no
    /// stdout parsing is needed and a bind failure cannot look like readiness.
    fn spawn(socket_path: &Path, log_path: &Path) -> Self {
        let binary = example_binary();
        let child = Command::new(&binary)
            .arg(socket_path)
            .arg(log_path)
            .spawn()
            .unwrap_or_else(|e| panic!("spawn the example at {}: {e}", binary.display()));
        let receiver = ReceiverProcess {
            child,
            reaped: false,
        };
        assert!(
            wait_until(READY_DEADLINE, || log_path.exists()),
            "the receiving process never signalled readiness by creating {}",
            log_path.display()
        );
        receiver
    }

    /// The crash: a real SIGKILL to a real process, then a REAP.
    ///
    /// Reaping matters and is not tidiness -- the kernel closes the socket as
    /// part of tearing the process down, and an unreaped child leaves that
    /// ordering unobservable, so the "the socket file survived the kill"
    /// assertions below would be racing the very teardown they describe.
    fn kill_and_reap(&mut self) {
        self.child.kill().expect("SIGKILL the receiving process");
        self.child
            .wait()
            .expect("reap the killed receiving process");
        self.reaped = true;
    }
}

impl Drop for ReceiverProcess {
    fn drop(&mut self) {
        if !self.reaped {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

fn log_lines(path: &Path) -> Vec<String> {
    match std::fs::read_to_string(path) {
        Ok(text) => text.lines().map(str::to_string).collect(),
        Err(_) => Vec::new(),
    }
}

/// Five real invocations, reported in `fail_open.rs::best_of_five`'s format.
///
/// Best-of-five rather than a single sample because this measures a whole
/// child process against a wall clock on a shared machine, where one
/// descheduled run says nothing about the code under test.
fn best_of_five(label: &str, config_home: &Path, payload: &str) -> Duration {
    let mut runs = Vec::new();
    for _ in 0..5 {
        let start = Instant::now();
        // Asserts exit 0, empty stdout and empty stderr on every one of the
        // five -- the "silent" half of this measurement, not a separate step.
        run_client(config_home, payload);
        runs.push(start.elapsed());
    }
    let micros: Vec<u128> = runs.iter().map(Duration::as_micros).collect();
    let best = runs.iter().copied().min().unwrap_or(Duration::MAX);
    println!("TIMING {label}: runs(us)={micros:?} best={best:?} budget={BUDGET:?}");
    best
}

// ---------------------------------------------------------------------
// Task 1 -- the tracer: one real client, one separate receiving process
// ---------------------------------------------------------------------

/// The thinnest path that touches every layer SC-1 exercises: a real
/// `seam-client` binary, a real `AF_UNIX`/`SOCK_DGRAM` socket, and a real
/// SEPARATE receiving process that records what it got. No crash yet.
#[test]
fn a_real_client_reaches_a_separate_receiving_process() {
    let config_home = temp_config_home("t1");
    let socket_path = seam_core::socket_path_from(config_home.to_str(), None)
        .expect("the socket path must resolve from an explicit base");
    assert_sun_path_fits(&socket_path);
    let log_path = config_home.join("events.log");

    let _receiver = ReceiverProcess::spawn(&socket_path, &log_path);

    let symbol = "verify_token";
    run_client(
        &config_home,
        &write_payload(HOOK_FILE_PATH, &declares(symbol)),
    );

    assert!(
        wait_until(ARRIVAL_DEADLINE, || log_lines(&log_path).len() == 1),
        "the real client's advertisement must reach the separate receiving process; the log at {} \
         holds {:?}",
        log_path.display(),
        log_lines(&log_path)
    );

    let lines = log_lines(&log_path);
    assert_eq!(lines.len(), 1, "exactly one event, got {lines:?}");
    let expected = expected_node_id(symbol);
    assert!(
        lines[0].contains(&expected),
        "the recorded event must be the client's advertisement of `{expected}`, got {:?}",
        lines[0]
    );

    let _ = std::fs::remove_dir_all(&config_home);
}

// ---------------------------------------------------------------------
// Task 2 -- the crash: the residue, the lost event, the probe that must
// not widen
// ---------------------------------------------------------------------

/// SC-1's crash half. The receiver is killed while it is genuinely RECEIVING,
/// not merely bound -- an event is driven all the way into its log first, so
/// the SIGKILL lands on something live. That is what distinguishes this from
/// 06-02's `a_socket_left_behind_by_a_crash_does_not_block_the_next_launch`,
/// which drops a socket that never served anything.
///
/// There is NO cleanup call of any kind between the kill and the recovery
/// bind, and the `assert!(socket_path.exists())` immediately before that bind
/// is what makes "no manual cleanup" a fact about the run rather than an
/// inference from what this file does not contain.
#[test]
fn a_killed_receiver_leaves_a_socket_the_next_launch_binds_over() {
    let config_home = temp_config_home("kill");
    let socket_path = seam_core::socket_path_from(config_home.to_str(), None)
        .expect("the socket path must resolve from an explicit base");
    assert_sun_path_fits(&socket_path);
    let log_path = config_home.join("events.log");

    let mut receiver = ReceiverProcess::spawn(&socket_path, &log_path);

    let symbol = "issue_session";
    run_client(
        &config_home,
        &write_payload(HOOK_FILE_PATH, &declares(symbol)),
    );
    assert!(
        wait_until(ARRIVAL_DEADLINE, || log_lines(&log_path).len() == 1),
        "guard: the receiver must be provably RECEIVING before the kill, so this is a kill of \
         something live; the log at {} holds {:?}",
        log_path.display(),
        log_lines(&log_path)
    );

    receiver.kill_and_reap();

    // The crash residue: the inode outlives the process that owned it.
    assert!(
        socket_path.exists(),
        "a SIGKILLed receiver must leave its socket file behind at {}",
        socket_path.display()
    );

    // ...and nothing is still writing to the log, which is the observable
    // difference between a dead process and a slow one.
    let after_kill = log_lines(&log_path).len();
    std::thread::sleep(QUIET_WINDOW);
    assert_eq!(
        log_lines(&log_path).len(),
        after_kill,
        "the killed receiver must have stopped writing"
    );

    // The relaunch. Note what is NOT between the kill above and this line:
    // any removal of the socket path. `bind_at`'s live-versus-dead probe is
    // what makes this work.
    assert!(
        socket_path.exists(),
        "guard: the residue must still be present immediately before the recovery bind"
    );
    let _recovered = event_stream::bind_at(&socket_path)
        .expect("bind_at must detect the dead inode, remove it, and rebind");

    let _ = std::fs::remove_dir_all(&config_home);
}

/// D-03, the locked data-loss expectation, as three separate assertions:
/// the client is SILENT, it is FAST, and its event is permanently LOST.
///
/// The binary is WARMED with one throwaway invocation before the clock is
/// started. 07-04 measured 270-319ms for the first execution of a freshly
/// built binary against ~5ms warm (WINDOWS 34), so an unwarmed measurement
/// here would be measuring the host OS's per-inode first-exec cost rather than
/// anything about the client.
#[test]
fn a_client_firing_during_the_outage_is_silent_fast_and_lost() {
    let config_home = temp_config_home("out");
    let socket_path = seam_core::socket_path_from(config_home.to_str(), None)
        .expect("the socket path must resolve from an explicit base");
    assert_sun_path_fits(&socket_path);
    let log_path = config_home.join("events.log");

    let mut receiver = ReceiverProcess::spawn(&socket_path, &log_path);

    // The warming invocation, which doubles as the guard that the receiver was
    // genuinely receiving before the kill.
    run_client(
        &config_home,
        &write_payload(HOOK_FILE_PATH, &declares("warm_the_binary")),
    );
    assert!(
        wait_until(ARRIVAL_DEADLINE, || log_lines(&log_path).len() == 1),
        "guard: the receiver must be provably RECEIVING before the kill; the log at {} holds {:?}",
        log_path.display(),
        log_lines(&log_path)
    );

    receiver.kill_and_reap();
    let before_outage = log_lines(&log_path).len();

    // Silent and fast. `best_of_five` asserts exit 0 / empty stdout / empty
    // stderr on every run through `run_client`, and prints the measurement.
    let payload = write_payload(HOOK_FILE_PATH, &declares("lost_to_the_outage"));
    let best = best_of_five(
        "a real client firing into the outage",
        &config_home,
        &payload,
    );
    assert!(
        best < BUDGET,
        "a client firing while the visualizer is down must stay inside the {BUDGET:?} hook budget; \
         best of five was {best:?}"
    );

    // Lost. No queue, no catch-up, no replay (D-03) -- five real
    // advertisements were sent into the gap and not one of them is anywhere.
    assert_eq!(
        log_lines(&log_path).len(),
        before_outage,
        "events fired while the visualizer was down must be silently LOST, not buffered; the log \
         at {} holds {:?}",
        log_path.display(),
        log_lines(&log_path)
    );

    let _ = std::fs::remove_dir_all(&config_home);
}

/// The mitigation for T-10-01-01: the dangerous way to pass the crash test
/// above is to widen `bind_at` into an unconditional "always unlink".
///
/// With the receiving child still ALIVE and serving, a second `bind_at` on the
/// same path must be refused with `BindError::AlreadyRunning` AND the child
/// must still be receiving afterwards. This is 06-02's
/// `a_live_peer_is_never_deleted_and_reports_a_conflict` re-proven against a
/// live peer in a SEPARATE PROCESS -- that test holds both sockets inside one
/// process, so it cannot see a steal that only manifests across a process
/// boundary.
#[test]
fn a_live_peer_is_still_refused_after_the_recovery_path_exists() {
    let config_home = temp_config_home("live");
    let socket_path = seam_core::socket_path_from(config_home.to_str(), None)
        .expect("the socket path must resolve from an explicit base");
    assert_sun_path_fits(&socket_path);
    let log_path = config_home.join("events.log");

    let _receiver = ReceiverProcess::spawn(&socket_path, &log_path);

    run_client(
        &config_home,
        &write_payload(HOOK_FILE_PATH, &declares("before_the_conflict")),
    );
    assert!(
        wait_until(ARRIVAL_DEADLINE, || log_lines(&log_path).len() == 1),
        "guard: the peer must be provably LIVE and receiving before the conflicting bind"
    );

    match event_stream::bind_at(&socket_path) {
        Err(event_stream::BindError::AlreadyRunning { path: reported }) => {
            assert_eq!(
                reported, socket_path,
                "the reported path must be the real socket path"
            );
        }
        other => panic!("expected BindError::AlreadyRunning, got {other:?}"),
    }

    // The dangerous failure mode is not a wrong error code -- it is the second
    // instance silently unlinking and stealing the live socket. Prove the
    // original process is STILL delivering.
    let symbol = "after_the_conflict";
    run_client(
        &config_home,
        &write_payload(HOOK_FILE_PATH, &declares(symbol)),
    );
    assert!(
        wait_until(ARRIVAL_DEADLINE, || log_lines(&log_path).len() == 2),
        "the live peer must still be receiving after a refused conflict; the log at {} holds {:?}",
        log_path.display(),
        log_lines(&log_path)
    );
    let lines = log_lines(&log_path);
    let expected = expected_node_id(symbol);
    assert!(
        lines[1].contains(&expected),
        "the post-conflict event must be the client's advertisement of `{expected}`, got {:?}",
        lines[1]
    );

    let _ = std::fs::remove_dir_all(&config_home);
}

// ---------------------------------------------------------------------
// Task 3 -- the relaunch: bind over the residue, receive, apply, record
// ---------------------------------------------------------------------

/// Drives the REAL `eframe::App::ui`, so the drain, the apply, the SCC
/// recompute, the seam re-detect and the history record all run on the real
/// per-frame path. Copied in shape from `timeline_reconstruction.rs`.
fn app_ui_harness(app: SeamExplorerApp) -> Harness<'static, SeamExplorerApp> {
    let mut frame = eframe::Frame::_new_kittest();
    Harness::new_ui_state(
        move |ui, app: &mut SeamExplorerApp| {
            <SeamExplorerApp as eframe::App>::ui(app, ui, &mut frame);
        },
        app,
    )
}

/// The community the fixture itself says `source_file` belongs to, derived
/// from the fixture JSON rather than read back out of the app. An expectation
/// read out of the system under test cannot fail.
fn expected_inherited_community(source_file: &str) -> String {
    let doc: serde_json::Value =
        serde_json::from_str(SOURCE_PATHS_FIXTURE).expect("fixture must be valid JSON");
    doc["nodes"]
        .as_array()
        .expect("fixture must have a nodes array")
        .iter()
        .filter(|n| n["source_file"].as_str() == Some(source_file))
        .filter_map(|n| n["community"].as_str().map(str::to_string))
        .min()
        .unwrap_or_else(|| panic!("fixture has no node with source_file {source_file}"))
}

fn history_node_ids(app: &SeamExplorerApp) -> Vec<String> {
    app.history
        .iter()
        .filter_map(|entry| match &entry.event {
            GraphEvent::AddNode { id, .. } => Some(id.clone()),
            _ => None,
        })
        .collect()
}

fn model_community_of(app: &SeamExplorerApp, id: &str) -> Option<String> {
    let model = app.model.as_ref()?;
    let index = model.index.get(id)?;
    Some(model.graph[*index].community.clone())
}

/// The last NON-COMMENT line number mentioning `needle`, 1-based.
///
/// Comment lines are excluded because `main.rs` discusses `run_native` in
/// prose well above the code that calls it, and a gate that matched prose
/// would be asserting about a doc comment rather than about the startup
/// sequence.
fn last_code_line_containing(source: &str, needle: &str) -> Option<usize> {
    source
        .lines()
        .enumerate()
        .filter(|(_, line)| !line.trim_start().starts_with("//"))
        .filter(|(_, line)| line.contains(needle))
        .map(|(i, _)| i + 1)
        .last()
}

/// SC-1's "comes back up receiving events with no lost usability", asserted at
/// the TIMELINE level rather than the counter level.
///
/// A counter-only assertion passes just as happily against an event that
/// arrived and was thrown away (07-04's own lesson), so every claim below is
/// about what reached `app.history` and `app.model`.
#[test]
fn the_relaunched_app_receives_applies_and_records_a_real_client_event() {
    let _guard = SERVE_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    let config_home = temp_config_home("relaunch");
    let socket_path = seam_core::socket_path_from(config_home.to_str(), None)
        .expect("the socket path must resolve from an explicit base");
    assert_sun_path_fits(&socket_path);
    let log_path = config_home.join("events.log");

    // The instance that will crash, driven to a recorded event first so the
    // kill lands on something genuinely receiving.
    let mut receiver = ReceiverProcess::spawn(&socket_path, &log_path);
    run_client(
        &config_home,
        &write_payload(HOOK_FILE_PATH, &declares("before_the_crash")),
    );
    assert!(
        wait_until(ARRIVAL_DEADLINE, || log_lines(&log_path).len() == 1),
        "guard: the receiver must be provably RECEIVING before the kill"
    );
    receiver.kill_and_reap();

    // The outage edit: fired while the visualizer is down, and therefore gone
    // forever (D-03). Its absence after the relaunch is asserted below.
    let outage_symbol = "made_during_the_outage";
    let outage_id = expected_node_id(outage_symbol);
    run_client(
        &config_home,
        &write_payload(HOOK_FILE_PATH, &declares(outage_symbol)),
    );

    // 1. The residue is still there, untouched.
    assert!(
        socket_path.exists(),
        "the crash residue must still be present at {}",
        socket_path.display()
    );

    // 2. The relaunch. Nothing removed the socket path between the kill above
    //    and this line -- `bind_at`'s live-versus-dead probe is the whole
    //    mechanism, and this is SC-1's "no manual cleanup" in one call.
    let socket = event_stream::bind_at(&socket_path)
        .expect("the relaunch must bind over the crash residue with no cleanup step");

    // 3. The process-global path, so `history::drain_and_apply` reaches it
    //    exactly as the running app does.
    event_stream::serve(socket, egui::Context::default());

    // 4. A real app, built through the real load path.
    let app = loaded_app(SOURCE_PATHS_FIXTURE);
    let baseline_seq = app.history.next_seq();
    let mut harness = app_ui_harness(app);
    harness.run_steps(3);
    assert_eq!(
        harness.state().history.next_seq(),
        baseline_seq,
        "guard: nothing may have been recorded before the post-relaunch client fires"
    );

    // 5. One real client, and then the real per-frame path until it lands.
    let symbol = "after_the_relaunch";
    let expected_id = expected_node_id(symbol);
    run_client(
        &config_home,
        &write_payload(HOOK_FILE_PATH, &declares(symbol)),
    );

    let start = Instant::now();
    while harness.state().history.next_seq() < baseline_seq + 1 {
        assert!(
            start.elapsed() < ARRIVAL_DEADLINE,
            "the relaunched app never recorded the post-relaunch event; history holds {:?}",
            history_node_ids(harness.state())
        );
        std::thread::sleep(Duration::from_millis(10));
        harness.run_steps(1);
    }

    let app = harness.state();

    assert_eq!(
        app.history.next_seq(),
        baseline_seq + 1,
        "exactly one event may have been recorded, got {:?}",
        history_node_ids(app)
    );
    assert!(
        history_node_ids(app).contains(&expected_id),
        "the recorded event must be the client's advertisement of `{expected_id}`, history holds \
         {:?}",
        history_node_ids(app)
    );
    assert!(
        app.model
            .as_ref()
            .is_some_and(|model| model.index.contains_key(&expected_id)),
        "`{expected_id}` must be in the live model after the relaunch"
    );

    // Sibling inheritance from the fixture's own `src/auth/login.rs` node --
    // the reason that path was chosen. A node parked in the unknown bucket
    // would make every assertion above measure an event that arrived without
    // meaning anything.
    let expected_community = expected_inherited_community(HOOK_REPO_RELATIVE);
    assert_eq!(
        expected_community, "A",
        "fixture guard: {HOOK_REPO_RELATIVE} must belong to community A"
    );
    assert_eq!(
        model_community_of(app, &expected_id).as_deref(),
        Some(expected_community.as_str()),
        "the relaunched app must resolve the advertised node to a real community, not the \
         `{}` sentinel",
        seam_core::UNKNOWN_COMMUNITY
    );

    // D-03: relaunching recovers the APP, never the events. The edit made
    // during the outage is gone from both the model and the timeline.
    assert!(
        !history_node_ids(app).contains(&outage_id),
        "the outage event must never appear in the timeline; history holds {:?}",
        history_node_ids(app)
    );
    assert!(
        app.model
            .as_ref()
            .is_some_and(|model| !model.index.contains_key(&outage_id)),
        "the outage event must never appear in the model"
    );

    let _ = std::fs::remove_dir_all(&config_home);
}

/// T-10-01-02: recovery must not silently reintroduce the umask default.
///
/// `event_stream.rs`'s `the_bound_socket_is_owner_only` only covers a FRESH
/// bind. A recovery bind takes a different branch -- unlink, then rebind -- and
/// an information-disclosure regression on that branch is one no other test in
/// the workspace would catch.
#[test]
fn the_recovered_socket_is_still_owner_only() {
    use std::os::unix::fs::PermissionsExt;

    let config_home = temp_config_home("perm");
    let socket_path = seam_core::socket_path_from(config_home.to_str(), None)
        .expect("the socket path must resolve from an explicit base");
    assert_sun_path_fits(&socket_path);
    let log_path = config_home.join("events.log");

    let mut receiver = ReceiverProcess::spawn(&socket_path, &log_path);
    receiver.kill_and_reap();
    assert!(
        socket_path.exists(),
        "guard: the recovery branch is only exercised when the residue survives"
    );

    let _recovered = event_stream::bind_at(&socket_path).expect("the recovery bind must succeed");

    let mode = std::fs::metadata(&socket_path)
        .expect("the recovered socket file must exist")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(
        mode, 0o600,
        "the RECOVERED socket file must be mode 0600, got {mode:o}"
    );

    let _ = std::fs::remove_dir_all(&config_home);
}

/// The structural gate on this whole plan's headless argument.
///
/// The example binary is a HARNESS for the real lifecycle, not a second
/// implementation of it, and that claim rests on `main.rs` still binding
/// BEFORE the window exists and serving after it. If that ordering ever
/// changes, every headless test in this file stops describing the real
/// startup sequence -- and this says so instead of passing quietly.
#[test]
fn the_production_startup_still_binds_before_the_window_exists() {
    let source = include_str!("../src/main.rs");

    let bind = last_code_line_containing(source, "bind_default")
        .expect("main.rs must still call event_stream::bind_default");
    let run = last_code_line_containing(source, "run_native")
        .expect("main.rs must still call eframe::run_native");
    let serve = last_code_line_containing(source, "event_stream::serve")
        .expect("main.rs must still call event_stream::serve");

    assert!(
        bind < run,
        "main.rs must bind the socket BEFORE eframe::run_native (bind at line {bind}, run_native \
         at line {run}) -- this ordering is the entire basis for plan 10-01's crash/relaunch \
         coverage being exercisable without a display"
    );
    assert!(
        run < serve,
        "main.rs must serve from inside the creation closure, after run_native (run_native at \
         line {run}, serve at line {serve}) -- the recv thread's wake target only exists there"
    );
}
