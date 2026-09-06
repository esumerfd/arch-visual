//! Plan 09-02: the time-travel tracer's integration coverage — baseline
//! capture at load, the replay baseline's advance on eviction, replay
//! reconstruction, and (from Task 3) the paused canvas.
//!
//! Every app under test is built through `SeamExplorerApp::apply_load_outcome`,
//! never field-by-field the way `live_apply.rs::build_test_app` does. That is
//! deliberate: the load path IS the baseline capture point, so a hand-built app
//! would have no baseline at all and every reconstruction assertion below would
//! be measuring an empty model.
//!
//! The socket harness helpers (`serve_at`/`send_and_wait`/`add_node`) mirror
//! `tests/live_apply.rs` rather than starting a third recipe — 08-01's summary
//! asks for exactly that. As there, `SERVE_TEST_LOCK` serializes every test that
//! touches the process-global receiver.
//!
//! **Expectation discipline.** The defect this file's wraparound tests guard
//! against (a baseline that never advances) produces a self-consistent,
//! repeatable, WRONG graph. Every circular comparison — reconstruction against
//! itself, against the live model, against a second reconstruction — passes
//! against it. So expected node sets are always derived from the FIXTURE JSON
//! plus the scripted event list, never read back out of the app under test.
//! `live_apply.rs::expected_inherited_community` established this discipline.

use std::collections::BTreeSet;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::net::UnixDatagram;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use seam_core::GraphEvent;
use seam_explorer_egui::app::SeamExplorerApp;
use seam_explorer_egui::{event_stream, history};

/// The fixture whose nodes carry `source_file`, which is what the
/// sibling-inheritance half of `resolve_community` needs. 6 nodes across three
/// communities (A: a1/a2, B: b1/b2, C: c1/c2).
const SOURCE_PATHS_FIXTURE: &str = include_str!("../../seam-core/tests/fixtures/source_paths.json");

/// A DIFFERENT graph, used only by the "loading a second graph resets the scrub
/// state" test. Its identity as a different file is the whole point.
const CLEAN_FIXTURE: &str = include_str!("../../seam-core/tests/fixtures/clean.json");

/// The `source_file` every scripted live node is given. It belongs to fixture
/// node `a1`, so sibling inheritance resolves the scripted node's community to
/// a concrete `A` and the event is genuinely APPLIED — and therefore recorded.
/// An unresolved or dropped event would shift the id-to-`seq` mapping every
/// assertion in this file depends on.
const SIBLING_SOURCE_FILE: &str = "src/auth/login.rs";

/// Datagrams per chunk, kept comfortably under
/// `event_stream::CHANNEL_CAPACITY` (256) so nothing is ever dropped by the
/// bounded channel between one `drain_and_apply` and the next.
const CHUNK: usize = 50;

/// Serializes every test that touches `event_stream`'s process-global.
static SERVE_TEST_LOCK: Mutex<()> = Mutex::new(());

fn temp_socket_path(unique: &str) -> PathBuf {
    // Kept short deliberately: this path plus "/seam.sock" must stay under the
    // 104-byte sun_path ceiling on top of whatever length $TMPDIR is.
    std::env::temp_dir()
        .join(format!("es-tl-{}-{}", std::process::id(), unique))
        .join("seam.sock")
}

fn wait_until(deadline: Duration, mut condition: impl FnMut() -> bool) -> bool {
    let start = Instant::now();
    while start.elapsed() < deadline {
        if condition() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    condition()
}

fn serve_at(unique: &str) -> PathBuf {
    let path = temp_socket_path(unique);
    let len = path.as_os_str().as_bytes().len();
    assert!(
        len <= seam_core::MAX_SUN_PATH_BYTES,
        "socket path is {len} bytes, over the {} byte sun_path ceiling: {path:?}",
        seam_core::MAX_SUN_PATH_BYTES
    );
    let _ = std::fs::remove_file(&path);
    let socket = event_stream::bind_at(&path).expect("bind_at must succeed for a fresh path");
    event_stream::serve(socket, egui::Context::default());
    path
}

/// Sends real datagrams from a separate unbound socket and waits (bounded) for
/// the receive thread to have delivered all of them before returning.
fn send_and_wait(path: &Path, events: &[GraphEvent]) {
    let baseline = event_stream::received_count();
    for event in events {
        let bytes = seam_core::to_datagram(event);
        let sender = UnixDatagram::unbound().expect("unbound socket must be constructible");
        sender
            .send_to(&bytes, path)
            .expect("send_to a bound socket must succeed");
    }
    let target = baseline + events.len() as u64;
    assert!(
        wait_until(Duration::from_secs(5), || {
            event_stream::received_count() >= target
        }),
        "expected received_count to reach {target}, stalled at {}",
        event_stream::received_count()
    );
}

/// The ONLY way an app is built in this file: through the real load path, which
/// is where the replay baseline is captured.
fn loaded_app(fixture: &str) -> SeamExplorerApp {
    let outcome =
        seam_explorer_egui::load::read_and_ingest(fixture).expect("fixture must ingest cleanly");
    let mut app = SeamExplorerApp::default();
    app.apply_load_outcome(outcome);
    app
}

fn add_node(id: &str, label: &str, source_file: Option<&str>) -> GraphEvent {
    GraphEvent::AddNode {
        id: id.to_string(),
        label: label.to_string(),
        community: None,
        source_file: source_file.map(str::to_string),
    }
}

/// The `n`th scripted live node. Ids are zero-padded so `live_007` sorts where
/// a reader expects it; the number is the event's own `seq`, since every
/// scripted event applies and therefore records exactly one history entry.
fn scripted(n: usize) -> GraphEvent {
    let id = format!("live_{n:03}");
    add_node(&id, &id, Some(SIBLING_SOURCE_FILE))
}

fn scripted_id(n: usize) -> String {
    format!("live_{n:03}")
}

/// Sends `count` scripted events in chunks under the channel capacity,
/// draining after each chunk so every one is applied and recorded.
fn send_scripted(path: &Path, app: &mut SeamExplorerApp, count: usize) {
    let mut sent = 0;
    while sent < count {
        let end = (sent + CHUNK).min(count);
        let batch: Vec<GraphEvent> = (sent..end).map(scripted).collect();
        send_and_wait(path, &batch);
        history::drain_and_apply(app);
        sent = end;
    }
}

/// The node ids a fixture declares, read from the FIXTURE JSON directly — never
/// from an ingested model, so an expectation can never alias the value it is
/// checking.
fn fixture_ids(fixture: &str) -> BTreeSet<String> {
    let doc: serde_json::Value = serde_json::from_str(fixture).expect("fixture must be valid JSON");
    doc["nodes"]
        .as_array()
        .expect("fixture must have a nodes array")
        .iter()
        .map(|n| {
            n["id"]
                .as_str()
                .expect("every fixture node must have a string id")
                .to_string()
        })
        .collect()
}

fn model_ids(model: &seam_core::Model) -> BTreeSet<String> {
    model.index.keys().cloned().collect()
}

fn baseline_ids(app: &SeamExplorerApp) -> BTreeSet<String> {
    model_ids(
        app.replay_baseline
            .as_ref()
            .expect("a loaded app must retain a replay baseline"),
    )
}

fn live_ids(app: &SeamExplorerApp) -> BTreeSet<String> {
    model_ids(app.model.as_ref().expect("a loaded app must have a model"))
}

/// `fixture_ids` plus the scripted ids `live_000 ..= live_{through}`, computed
/// from the script alone.
fn fixture_plus_scripted_through(fixture: &str, through: usize) -> BTreeSet<String> {
    let mut expected = fixture_ids(fixture);
    for n in 0..=through {
        expected.insert(scripted_id(n));
    }
    expected
}

// ---------------------------------------------------------------------
// Task 1: the replay baseline
// ---------------------------------------------------------------------

/// The baseline exists the moment a graph is loaded, and with an empty history
/// "the state before the oldest retained entry" IS the loaded graph — so the
/// invariant holds from the very first frame.
#[test]
fn loading_a_graph_retains_a_pristine_baseline() {
    let app = loaded_app(SOURCE_PATHS_FIXTURE);

    let baseline = app
        .replay_baseline
        .as_ref()
        .expect("apply_load_outcome must capture a replay baseline");
    let live = app.model.as_ref().expect("the model must be loaded");

    assert_eq!(
        baseline.graph.node_count(),
        live.graph.node_count(),
        "the captured baseline must hold the graph exactly as ingested"
    );
    assert_eq!(
        model_ids(baseline),
        fixture_ids(SOURCE_PATHS_FIXTURE),
        "the baseline's id set must equal the FIXTURE's, derived from the JSON \
         rather than read back out of the app"
    );
    assert_eq!(
        model_ids(baseline),
        model_ids(live),
        "with an empty history, the baseline and the live model are the same state"
    );
    assert_eq!(
        app.scrub_position, None,
        "a freshly loaded app is Live, not paused"
    );
    assert!(app.scrub_model.is_none(), "nothing is reconstructed yet");
    assert!(app.scrub_seams.is_empty(), "nothing is reconstructed yet");
}

/// Scoped on purpose. BELOW the cap nothing is evicted, so nothing may move —
/// but the unqualified claim "live events never touch the baseline" is the
/// FALSE one this plan exists to not ship (see the eviction test below).
#[test]
fn live_events_below_the_buffer_cap_leave_the_baseline_untouched() {
    let _guard = SERVE_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let path = serve_at("under-cap");
    let mut app = loaded_app(SOURCE_PATHS_FIXTURE);

    let before = baseline_ids(&app);
    let before_count = app
        .replay_baseline
        .as_ref()
        .expect("baseline must exist")
        .graph
        .node_count();

    send_scripted(&path, &mut app, 5);

    assert_eq!(
        app.history.len(),
        5,
        "guard: all five events must have applied and been recorded"
    );
    assert_eq!(
        app.history.evicted_count(),
        0,
        "guard: five events cannot evict anything from a 100-entry buffer"
    );
    for n in 0..5 {
        assert!(
            live_ids(&app).contains(&scripted_id(n)),
            "guard: the live model must have gained {}",
            scripted_id(n)
        );
    }

    assert_eq!(
        baseline_ids(&app),
        before,
        "with nothing evicted, the baseline's id set must be byte-identical to \
         what it was before the events"
    );
    assert_eq!(
        app.replay_baseline
            .as_ref()
            .expect("baseline must exist")
            .graph
            .node_count(),
        before_count,
        "with nothing evicted, the baseline's node count must not move"
    );
}

/// The gate on the whole mechanism (T-09-02-07).
///
/// Asserted on the BASELINE's own contents, in both directions, because no
/// assertion made on a reconstruction can see the off-by-one: replaying an
/// already-applied `AddNode` is near enough idempotent, so a baseline advanced
/// one event too far reconstructs identically to a correct one.
///
/// Wrapping TWICE, not once, is also deliberate: a fold that runs only on the
/// first eviction passes a single-wrap test.
#[test]
fn evicting_an_event_folds_it_into_the_replay_baseline() {
    let _guard = SERVE_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let path = serve_at("evict-fold");
    let mut app = loaded_app(SOURCE_PATHS_FIXTURE);

    let cap = seam_core::LIVE_BUFFER_CAPACITY;
    let total = 2 * cap + 7;
    let dropped_before = event_stream::dropped_count();

    send_scripted(&path, &mut app, total);

    // Setup guards first, so a mis-set-up test fails as a setup failure rather
    // than as a false pass.
    assert_eq!(
        event_stream::dropped_count(),
        dropped_before,
        "guard: the bounded channel must not have dropped a datagram, or the \
         id-to-seq mapping every assertion below depends on is shifted"
    );
    assert_eq!(
        app.history.next_seq(),
        total as u64,
        "guard: exactly one recorded event per scripted event, so `seq N` names \
         `live_N`"
    );
    assert_eq!(
        app.history.len(),
        cap,
        "guard: the buffer must be full at its cap"
    );
    let evicted = app.history.evicted_count();
    assert_eq!(
        evicted,
        (cap + 7) as u64,
        "guard: {total} events against a {cap}-entry buffer must evict exactly \
         {} — two full wraps",
        cap + 7
    );

    // The whole claim, as one equality: the baseline is the state immediately
    // before the oldest STILL-RETAINED entry.
    let expected = fixture_plus_scripted_through(SOURCE_PATHS_FIXTURE, (evicted - 1) as usize);
    assert_eq!(
        baseline_ids(&app),
        expected,
        "the baseline must equal the fixture plus every EVICTED event's node, \
         and nothing more"
    );

    // Direction 1 — fails against a baseline that never advances at all.
    let last_evicted = scripted_id((evicted - 1) as usize);
    assert!(
        baseline_ids(&app).contains(&last_evicted),
        "the LAST evicted event's node `{last_evicted}` must be folded into the \
         baseline; without it every reconstruction after the buffer wraps is \
         missing every evicted event's effect"
    );

    // Direction 2 — fails against a baseline that advances one event too far.
    // Nothing asserted on a reconstruction anywhere in this phase can see this.
    let oldest_retained = app
        .history
        .iter()
        .next()
        .expect("guard: a full buffer must have an oldest entry");
    assert_eq!(
        oldest_retained.seq, evicted,
        "guard: the oldest retained entry's identity must be exactly the \
         evicted count"
    );
    let oldest_retained_id = scripted_id(evicted as usize);
    assert!(
        !baseline_ids(&app).contains(&oldest_retained_id),
        "the OLDEST RETAINED entry's node `{oldest_retained_id}` must NOT be in \
         the baseline — the baseline means the state BEFORE that entry, not \
         after it"
    );
}

/// `History::clear` resets the sequence counter to zero, so a surviving
/// `scrub_position` from the previous graph would silently name an event on the
/// NEW graph's timeline.
#[test]
fn loading_a_second_graph_resets_the_scrub_state() {
    let mut app = loaded_app(SOURCE_PATHS_FIXTURE);

    // Set all three scrub fields to non-empty values by hand, so the reset has
    // something real to undo.
    app.scrub_position = Some(7);
    app.scrub_model = Some(seam_core::Model::default());
    app.scrub_seams = vec![seam_core::Seam {
        a: "A".to_string(),
        b: "B".to_string(),
        crossings: 3,
    }];
    assert!(
        app.scrub_position.is_some() && app.scrub_model.is_some() && !app.scrub_seams.is_empty(),
        "guard: all three fields must genuinely be non-empty before the reload"
    );

    let second = seam_explorer_egui::load::read_and_ingest(CLEAN_FIXTURE)
        .expect("the second fixture must ingest cleanly");
    app.apply_load_outcome(second);

    assert_eq!(
        app.scrub_position, None,
        "a new graph starts Live — a position from the old timeline would name \
         an event on the new one"
    );
    assert!(
        app.scrub_model.is_none(),
        "a reconstruction of the OLD graph must not survive the load"
    );
    assert!(
        app.scrub_seams.is_empty(),
        "the old reconstruction's seam list must not survive the load"
    );
    assert_eq!(
        baseline_ids(&app),
        fixture_ids(CLEAN_FIXTURE),
        "the baseline must now be the SECOND graph, derived from its own fixture JSON"
    );
    assert_ne!(
        fixture_ids(CLEAN_FIXTURE),
        fixture_ids(SOURCE_PATHS_FIXTURE),
        "guard: the two fixtures must genuinely differ, or this proved nothing"
    );
}

/// T-09-02-03, same discipline as `live_apply.rs`'s history round-trip test:
/// `eframe`'s save hook fires on normal app close, so a field left without
/// `#[serde(skip)]` ships graph contents to disk the first time a user quits.
#[test]
fn the_scrub_fields_do_not_round_trip_through_storage() {
    let mut app = loaded_app(SOURCE_PATHS_FIXTURE);

    // The positive control: the ONE field that IS meant to persist, set to a
    // non-default value.
    assert!(
        !SeamExplorerApp::default().has_seen_trace_onboarding,
        "guard: the control value must genuinely differ from the default"
    );
    app.has_seen_trace_onboarding = true;
    app.scrub_position = Some(3);
    app.scrub_model = Some(seam_core::Model::default());
    app.scrub_seams = vec![seam_core::Seam {
        a: "A".to_string(),
        b: "B".to_string(),
        crossings: 1,
    }];

    let serialized = serde_json::to_string(&app).expect("the app must serialize");

    for field in [
        "replay_baseline",
        "scrub_position",
        "scrub_model",
        "scrub_seams",
    ] {
        assert!(
            !serialized.contains(field),
            "`{field}` reached the serialized form — it must carry \
             `#[serde(skip)]` like every other runtime field: {serialized}"
        );
    }
    assert!(
        serialized.contains("has_seen_trace_onboarding"),
        "positive control: the one persisting field MUST be in the serialized \
         form, or this test proved nothing: {serialized}"
    );
}
