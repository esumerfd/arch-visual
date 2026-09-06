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

use std::cell::{Cell, RefCell};
use std::collections::BTreeSet;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::net::UnixDatagram;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use egui_kittest::Harness;
use seam_core::GraphEvent;
use seam_explorer_egui::app::{FocusState, SeamExplorerApp};
use seam_explorer_egui::layout::SeamLayoutState;
use seam_explorer_egui::timeline::{self, TimelineAction};
use seam_explorer_egui::trace::TraceResult;
use seam_explorer_egui::{event_stream, graph_view, history};

/// The fixture whose nodes carry `source_file`, which is what the
/// sibling-inheritance half of `resolve_community` needs. 6 nodes across three
/// communities (A: a1/a2, B: b1/b2, C: c1/c2).
const SOURCE_PATHS_FIXTURE: &str = include_str!("../../seam-core/tests/fixtures/source_paths.json");

/// A DIFFERENT graph, used only by the "loading a second graph resets the scrub
/// state" test. Its identity as a different graph is the whole point, and that
/// is asserted rather than assumed: `clean.json` — `live_apply.rs`'s choice for
/// its own reload test — declares the SAME six node ids as `source_paths.json`,
/// so a baseline swap between those two is invisible to an id-set assertion.
/// `tied_seams.json` (09-01) declares `na`/`nb`/`nc`/`nd`, which are disjoint.
const SECOND_FIXTURE: &str = include_str!("../../seam-core/tests/fixtures/tied_seams.json");

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

/// Serializes every test in this file. The canvas tests below drive the SAME
/// process-global receiver as the socket tests, so one lock for the whole file
/// -- not one per section. Two locks would let a canvas test and a socket test
/// run concurrently and clobber each other's event stream.
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

    let second = seam_explorer_egui::load::read_and_ingest(SECOND_FIXTURE)
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
        fixture_ids(SECOND_FIXTURE),
        "the baseline must now be the SECOND graph, derived from its own fixture JSON"
    );
    assert_ne!(
        fixture_ids(SECOND_FIXTURE),
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

// ---------------------------------------------------------------------
// Task 2: replay reconstruction and the navigation entry point
// ---------------------------------------------------------------------

/// The reconstructed model's id set, as a set.
fn scrub_ids(app: &SeamExplorerApp) -> BTreeSet<String> {
    model_ids(
        app.scrub_model
            .as_ref()
            .expect("a paused app must hold a reconstruction"),
    )
}

/// Runs a REAL trace between two fixture nodes and installs it, exactly as a
/// completed drag-to-trace gesture would. Same shape as
/// `live_apply.rs::install_trace`.
fn install_trace(app: &mut SeamExplorerApp, from: &str, to: &str) {
    let model = app.model.as_ref().expect("model must be loaded");
    let path = seam_core::trace_path(model, from, to)
        .unwrap_or_else(|| panic!("fixture must have a path {from} -> {to}"));
    assert!(
        path.hops.len() > 2,
        "guard: the trace must be a real multi-hop path, got {:?}",
        path.hops
    );
    app.trace = Some(TraceResult {
        from: from.to_string(),
        to: to.to_string(),
        path: Some(path),
    });
}

/// Same shape as `live_apply.rs::focus_seam` -- a real seam, with the detail
/// recomputed the way a click would.
fn focus_seam(app: &mut SeamExplorerApp, a: &str, b: &str) {
    let model = app.model.as_ref().expect("model must be loaded");
    let scc = model
        .scc
        .as_ref()
        .expect("load must have finalized the SCC");
    assert!(
        app.seams
            .iter()
            .any(|s| (s.a == a && s.b == b) || (s.a == b && s.b == a)),
        "guard: {a} <-> {b} must be a real seam before focusing it"
    );
    app.detail = Some(seam_core::seam_detail(
        model,
        scc,
        &a.to_string(),
        &b.to_string(),
    ));
    app.focus = Some(FocusState {
        a: a.to_string(),
        b: b.to_string(),
    });
}

/// TIME-03's core claim at its simplest: a reconstruction is a DIFFERENT node
/// set from the live graph, derived from the fixture plus the script rather
/// than read back out of the app.
#[test]
fn reconstructing_a_historical_position_yields_the_node_set_of_that_moment() {
    let _guard = SERVE_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let path = serve_at("reconstruct");
    let mut app = loaded_app(SOURCE_PATHS_FIXTURE);

    // Drained one at a time so each is unambiguously its own recorded event.
    for n in 0..3 {
        send_and_wait(&path, &[scripted(n)]);
        history::drain_and_apply(&mut app);
    }
    assert_eq!(
        app.history.next_seq(),
        3,
        "guard: three scripted events, three recorded entries, so seq N names live_N"
    );

    timeline::apply_action(&mut app, TimelineAction::StepBack);
    timeline::apply_action(&mut app, TimelineAction::StepBack);

    assert_eq!(
        app.scrub_position,
        Some(0),
        "two steps back from Live (whose effective position is seq 2) is seq 0"
    );
    assert_eq!(
        scrub_ids(&app),
        fixture_plus_scripted_through(SOURCE_PATHS_FIXTURE, 0),
        "the reconstruction must be the fixture plus only the FIRST scripted \
         node -- the live set minus the two most recent additions"
    );
    assert!(
        !scrub_ids(&app).contains(&scripted_id(1)) && !scrub_ids(&app).contains(&scripted_id(2)),
        "the two most recent additions must be absent, or this is the live \
         graph with a different label on it"
    );
    assert_ne!(
        scrub_ids(&app),
        live_ids(&app),
        "a reconstruction that matches the live set is a cosmetic overlay, not \
         a reconstruction (TIME-03)"
    );
    assert!(
        std::ptr::eq(
            timeline::display_model(&app).expect("a paused app must display something"),
            app.scrub_model.as_ref().expect("reconstruction must exist")
        ),
        "while paused the display accessor must return the reconstruction"
    );
}

/// Navigation is a READ-ONLY re-render (09-CONTEXT.md). The ONLY thing that
/// ever advances the baseline is an eviction, and no navigation evicts.
#[test]
fn reconstruction_never_mutates_the_live_model_or_the_baseline() {
    let _guard = SERVE_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let path = serve_at("no-mutate");
    let mut app = loaded_app(SOURCE_PATHS_FIXTURE);

    send_scripted(&path, &mut app, 5);
    let live_before = live_ids(&app);
    let baseline_before = baseline_ids(&app);
    let next_seq_before = app.history.next_seq();
    let evicted_before = app.history.evicted_count();
    let history_len_before = app.history.len();

    for action in [
        TimelineAction::StepBack,
        TimelineAction::StepBack,
        TimelineAction::JumpEarliest,
        TimelineAction::StepForward,
        TimelineAction::StepForward,
        TimelineAction::JumpLatest,
        TimelineAction::StepBack,
    ] {
        timeline::apply_action(&mut app, action);
        assert_eq!(
            live_ids(&app),
            live_before,
            "the live model must be untouched by {action:?}"
        );
        assert_eq!(
            baseline_ids(&app),
            baseline_before,
            "only an eviction may advance the baseline, and {action:?} evicts nothing"
        );
        assert_eq!(app.history.next_seq(), next_seq_before);
        assert_eq!(app.history.evicted_count(), evicted_before);
        assert_eq!(
            app.history.len(),
            history_len_before,
            "navigation must not remove, evict or truncate any history entry"
        );
    }
}

/// The correctness gate for a session longer than a hundred events, and the one
/// test in this phase that fails outright if Task 1's fold is missing
/// (T-09-02-07).
///
/// The expected set is computed from the FIXTURE and the SCRIPT. Never compare
/// the reconstruction against itself, against the live model, or against a
/// second reconstruction: the defect this guards produces a self-consistent,
/// repeatable, WRONG graph, so every circular comparison passes against it.
#[test]
fn reconstructing_a_position_after_the_buffer_wrapped_matches_an_independently_derived_node_set() {
    let _guard = SERVE_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let path = serve_at("wrap-recon");
    let mut app = loaded_app(SOURCE_PATHS_FIXTURE);

    let cap = seam_core::LIVE_BUFFER_CAPACITY;
    let total = cap + 25;
    let dropped_before = event_stream::dropped_count();

    send_scripted(&path, &mut app, total);

    assert_eq!(
        event_stream::dropped_count(),
        dropped_before,
        "guard: no datagram may be dropped, or the id-to-seq mapping shifts"
    );
    assert_eq!(
        app.history.next_seq(),
        total as u64,
        "guard: one recorded event per scripted event"
    );
    assert_eq!(
        app.history.evicted_count(),
        25,
        "guard: the buffer must provably have wrapped"
    );

    // Reached through the ONE public navigation entry point, exactly as a
    // keypress or a button will reach it.
    timeline::apply_action(&mut app, TimelineAction::JumpEarliest);
    assert_eq!(
        app.scrub_position,
        Some(25),
        "guard: JumpEarliest must land on the oldest RETAINED identity"
    );
    for _ in 0..30 {
        timeline::apply_action(&mut app, TimelineAction::StepForward);
    }
    let target = 55usize;
    assert_eq!(
        app.scrub_position,
        Some(target as u64),
        "guard: thirty forward steps from seq 25 must land on seq 55"
    );

    assert_eq!(
        scrub_ids(&app),
        fixture_plus_scripted_through(SOURCE_PATHS_FIXTURE, target),
        "past the eviction point the reconstruction must still equal the \
         fixture plus every scripted node up to the target -- a frozen baseline \
         yields a graph missing all 25 evicted events"
    );
}

/// D-05 as a CONTENT claim rather than a positional one. Against an unadvanced
/// baseline this returns the fixture plus one node -- the "renders a state that
/// never existed" failure D-05 would otherwise walk straight into on its very
/// first use after a wrap.
#[test]
fn jump_earliest_after_a_wrap_reconstructs_the_oldest_retained_moment() {
    let _guard = SERVE_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let path = serve_at("wrap-earliest");
    let mut app = loaded_app(SOURCE_PATHS_FIXTURE);

    let cap = seam_core::LIVE_BUFFER_CAPACITY;
    send_scripted(&path, &mut app, cap + 25);
    assert_eq!(
        app.history.evicted_count(),
        25,
        "guard: the buffer must provably have wrapped"
    );

    timeline::apply_action(&mut app, TimelineAction::JumpEarliest);

    let earliest = 25usize;
    assert_eq!(app.scrub_position, Some(earliest as u64));
    assert_eq!(
        scrub_ids(&app),
        fixture_plus_scripted_through(SOURCE_PATHS_FIXTURE, earliest),
        "the oldest retained moment is the fixture plus live_000..=live_025, \
         because the baseline is the state immediately BEFORE seq 25 and \
         exactly one event replays onto it"
    );
    assert!(
        scrub_ids(&app).len() > fixture_ids(SOURCE_PATHS_FIXTURE).len() + 1,
        "guard: an unadvanced baseline would yield the fixture plus a single \
         node, which is the failure this test exists to catch"
    );
}

/// TIME-03's "genuine, repeatable". This is the test that would fail without
/// plan 09-01's tie-break fix -- contents alone would match while the seam
/// ordering reshuffled between the two visits.
#[test]
fn navigating_to_the_same_position_twice_reproduces_it_exactly() {
    let _guard = SERVE_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let path = serve_at("repeatable");
    let mut app = loaded_app(SOURCE_PATHS_FIXTURE);

    send_scripted(&path, &mut app, 6);

    timeline::apply_action(&mut app, TimelineAction::StepBack);
    timeline::apply_action(&mut app, TimelineAction::StepBack);
    let position = app.scrub_position;
    let first_ids: Vec<String> = scrub_ids(&app).into_iter().collect();
    let first_seams = app.scrub_seams.clone();
    assert!(
        !first_seams.is_empty(),
        "guard: the reconstruction must produce a non-empty ranked seam list, \
         or the ordering claim below is vacuous"
    );

    timeline::apply_action(&mut app, TimelineAction::JumpLatest);
    assert_eq!(
        app.scrub_position, None,
        "guard: we genuinely navigated away"
    );

    timeline::apply_action(&mut app, TimelineAction::StepBack);
    timeline::apply_action(&mut app, TimelineAction::StepBack);
    assert_eq!(
        app.scrub_position, position,
        "guard: we must be back at the same position"
    );

    assert_eq!(
        scrub_ids(&app).into_iter().collect::<Vec<String>>(),
        first_ids,
        "the same position must reconstruct the identical model contents"
    );
    assert_eq!(
        app.scrub_seams, first_seams,
        "the same position must rank identically -- same seams, same order"
    );
}

/// D-01: navigating to a different point clears the transient trace/focus/
/// detail selection unconditionally.
#[test]
fn navigation_clears_the_trace_and_the_focus() {
    let _guard = SERVE_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let path = serve_at("clear-sel");
    let mut app = loaded_app(SOURCE_PATHS_FIXTURE);

    send_scripted(&path, &mut app, 4);

    // Installed AFTER the drain, so the live path's own stale-selection
    // clean-up cannot be what clears them.
    install_trace(&mut app, "a1", "c2");
    focus_seam(&mut app, "A", "B");
    assert!(
        app.trace.is_some() && app.focus.is_some() && app.detail.is_some(),
        "guard: all three must genuinely be installed before navigating"
    );

    timeline::apply_action(&mut app, TimelineAction::StepBack);

    assert!(
        app.trace.is_none(),
        "a trace drawn against the live graph says nothing about a historical one"
    );
    assert!(
        app.focus.is_none(),
        "D-01 clears the focus unconditionally -- unlike the live path's \
         clean-up, which PRESERVES a focus whose seam still exists"
    );
    assert!(
        app.detail.is_none(),
        "a detail recomputed against the LIVE model must not sit beside a \
         historical canvas"
    );
}

/// D-01's scope: a navigation action that leaves the position unchanged changed
/// nothing about what is displayed, so it must not destroy a selection.
#[test]
fn a_navigation_that_does_not_move_leaves_the_selection_alone() {
    let _guard = SERVE_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let path = serve_at("no-move");
    let mut app = loaded_app(SOURCE_PATHS_FIXTURE);

    send_scripted(&path, &mut app, 4);

    // Case 1: jump-to-latest while already Live.
    focus_seam(&mut app, "A", "B");
    assert_eq!(app.scrub_position, None, "guard: we start Live");
    timeline::apply_action(&mut app, TimelineAction::JumpLatest);
    assert_eq!(app.scrub_position, None, "the position did not move");
    assert!(
        app.focus.is_some() && app.detail.is_some(),
        "jump-to-latest while already Live must not destroy a selection"
    );

    // Case 2: stepping forward while already pinned at the newest event.
    timeline::apply_action(&mut app, TimelineAction::StepBack);
    timeline::apply_action(&mut app, TimelineAction::StepForward);
    let at_newest = app.scrub_position;
    assert_eq!(
        at_newest,
        Some(app.history.next_seq() - 1),
        "guard: we must be pinned at the newest recorded event, and PAUSED"
    );
    focus_seam(&mut app, "A", "B");
    timeline::apply_action(&mut app, TimelineAction::StepForward);
    assert_eq!(
        app.scrub_position, at_newest,
        "D-02: stepping forward at the newest event stays Paused, unmoved"
    );
    assert!(
        app.focus.is_some() && app.detail.is_some(),
        "an unmoved step-forward must not destroy a selection either"
    );
}

/// D-02: jump-to-latest literally resumes Live -- not "reconstruct the newest
/// event and stay Paused".
#[test]
fn jump_latest_returns_to_the_live_model() {
    let _guard = SERVE_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let path = serve_at("jump-live");
    let mut app = loaded_app(SOURCE_PATHS_FIXTURE);

    send_scripted(&path, &mut app, 4);

    timeline::apply_action(&mut app, TimelineAction::StepBack);
    assert!(
        timeline::is_paused(&app),
        "guard: we must genuinely be paused first"
    );
    assert_ne!(
        scrub_ids(&app),
        live_ids(&app),
        "guard: the paused view must genuinely differ from the live one"
    );

    timeline::apply_action(&mut app, TimelineAction::JumpLatest);

    assert_eq!(app.scrub_position, None, "jump-to-latest resumes Live");
    assert!(
        app.scrub_model.is_none(),
        "Live costs zero reconstruction -- the display accessors fall through \
         to the ever-current live fields"
    );
    assert!(app.scrub_seams.is_empty());
    assert!(!timeline::is_paused(&app));
    assert!(
        std::ptr::eq(
            timeline::display_model(&app).expect("Live must display the live model"),
            app.model.as_ref().expect("the live model must exist")
        ),
        "display_model must hand back the LIVE model once Live has resumed"
    );
    assert!(
        std::ptr::eq(timeline::display_seams(&app), app.seams.as_slice()),
        "display_seams must mirror display_model's decision"
    );
}

// ---------------------------------------------------------------------
// Task 3: the paused canvas, and the layout-pruning guard
// ---------------------------------------------------------------------

/// Steps enough frames for the layout to reach its repulsion/easing
/// equilibrium, so "held still" is measured against a settled baseline rather
/// than a still-moving one. `canvas.rs`'s established value, reused rather than
/// re-guessed.
const SETTLE_STEPS: usize = 80;

/// Per-node tolerance for "held still" across a pause/resume cycle.
/// `canvas.rs`'s established value and reasoning: at equilibrium a surviving
/// node's own per-frame motion is a fraction of a pixel, while a node whose
/// position was pruned and re-seeded lands hundreds of pixels away anywhere in
/// the 1200x800 band. The two regimes are orders of magnitude apart; this
/// threshold sits between them, not near either.
const HOLD_STILL_EPS: f32 = 20.0;

/// Max distance any id in `before` moved by `after`, with the id that moved
/// furthest, for a failure message that names the actual culprit. Same shape as
/// `canvas.rs::max_drift`.
fn max_drift(
    before: &std::collections::HashMap<String, egui::Pos2>,
    after: &std::collections::HashMap<String, egui::Pos2>,
) -> (String, f32) {
    let mut worst = (String::new(), 0.0_f32);
    for (id, was) in before {
        let now = after.get(id).unwrap_or_else(|| {
            panic!(
                "surviving node `{id}` lost its persisted position entirely -- a \
                 historical model reached the pruning read, which prunes against \
                 the id set it is given (09-RESEARCH.md Pitfall 2)"
            )
        });
        let d = (*now - *was).length();
        if d > worst.1 {
            worst = (id.clone(), d);
        }
    }
    worst
}

/// A canvas harness mirroring `app.rs::ui()`'s real order -- live events applied
/// FIRST, then the canvas rendered -- plus a mirror of the persisted, id-keyed
/// layout positions written after every frame. Same recipe as
/// `canvas.rs::live_canvas_harness`, except the app comes through the real load
/// path so it has a replay baseline.
///
/// The returned `wipe` switch is what makes the rendered node set observable
/// through the REAL render path. `SeamLayoutState`'s position map persists
/// across frames and is pruned against the LIVE model, so simply reading it
/// after a frame cannot tell a historical render from a live one. With the
/// switch on, the map is emptied immediately BEFORE `show()`, and
/// `SeamLayout::next` then seeds exactly the nodes `show()` actually rendered
/// that frame -- so the map read back afterwards IS the rendered id set. It is
/// deliberately not `build_graph(display_model(app))` recomputed in the test:
/// that would pass whether or not `show()` itself was ever redirected.
#[allow(clippy::type_complexity)]
fn live_canvas_harness(
    fixture: &str,
) -> (
    Harness<'static, SeamExplorerApp>,
    Rc<RefCell<std::collections::HashMap<String, egui::Pos2>>>,
    Rc<Cell<bool>>,
) {
    let mirror: Rc<RefCell<std::collections::HashMap<String, egui::Pos2>>> =
        Rc::new(RefCell::new(std::collections::HashMap::new()));
    let wipe = Rc::new(Cell::new(false));
    let mirror_inner = mirror.clone();
    let wipe_inner = wipe.clone();
    let harness = Harness::new_ui_state(
        move |ui, app: &mut SeamExplorerApp| {
            history::drain_and_apply(app);
            if wipe_inner.get() {
                let mut state = egui_graphs::get_layout_state::<SeamLayoutState>(ui, None);
                state.retain_positions(&std::collections::HashSet::new());
                egui_graphs::set_layout_state(ui, state, None);
            }
            graph_view::show(ui, app);
            let state = egui_graphs::get_layout_state::<SeamLayoutState>(ui, None);
            *mirror_inner.borrow_mut() = state.positions().clone();
        },
        loaded_app(fixture),
    );
    (harness, mirror, wipe)
}

/// Renders one frame with the position map emptied first, and returns exactly
/// the ids `graph_view::show` rendered on that frame.
fn rendered_ids_for_one_frame(
    harness: &mut Harness<'static, SeamExplorerApp>,
    positions: &Rc<RefCell<std::collections::HashMap<String, egui::Pos2>>>,
    wipe: &Rc<Cell<bool>>,
) -> BTreeSet<String> {
    wipe.set(true);
    harness.run_steps(1);
    wipe.set(false);
    positions.borrow().keys().cloned().collect()
}

/// TIME-03 at the canvas: while paused, `graph_view::show` renders the
/// RECONSTRUCTED graph, a genuinely different node set from the live one.
///
/// Both directions are asserted. A one-directional assertion ("the survivors
/// are present") would pass on an empty render.
#[test]
fn a_paused_canvas_renders_the_historical_node_set() {
    let _guard = SERVE_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let path = serve_at("paused-canvas");
    let (mut harness, positions, wipe) = live_canvas_harness(SOURCE_PATHS_FIXTURE);

    send_and_wait(&path, &[scripted(0), scripted(1), scripted(2)]);
    harness.run_steps(2);
    assert_eq!(
        harness.state().history.next_seq(),
        3,
        "guard: three scripted events, three recorded entries"
    );

    let live_rendered = rendered_ids_for_one_frame(&mut harness, &positions, &wipe);
    assert_eq!(
        live_rendered,
        fixture_plus_scripted_through(SOURCE_PATHS_FIXTURE, 2),
        "guard: the LIVE canvas must render the fixture plus all three additions"
    );

    timeline::apply_action(harness.state_mut(), TimelineAction::StepBack);
    timeline::apply_action(harness.state_mut(), TimelineAction::StepBack);
    assert_eq!(
        harness.state().scrub_position,
        Some(0),
        "guard: two steps back from Live is seq 0"
    );

    let paused_rendered = rendered_ids_for_one_frame(&mut harness, &positions, &wipe);

    let expected = fixture_plus_scripted_through(SOURCE_PATHS_FIXTURE, 0);
    assert_eq!(
        paused_rendered, expected,
        "the paused canvas must render the reconstructed historical graph"
    );
    for surviving in &expected {
        assert!(
            paused_rendered.contains(surviving),
            "`{surviving}` existed at seq 0 and must still be rendered"
        );
    }
    for absent in [scripted_id(1), scripted_id(2)] {
        assert!(
            !paused_rendered.contains(&absent),
            "`{absent}` did not exist at seq 0 and must NOT be rendered -- a \
             canvas still showing it is a cosmetic overlay on the live graph"
        );
    }
}

/// D-02: jump-to-latest is the route back, and the full live canvas comes with
/// it.
#[test]
fn resuming_live_restores_the_full_canvas() {
    let _guard = SERVE_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let path = serve_at("resume-canvas");
    let (mut harness, positions, wipe) = live_canvas_harness(SOURCE_PATHS_FIXTURE);

    send_and_wait(&path, &[scripted(0), scripted(1), scripted(2)]);
    harness.run_steps(2);

    timeline::apply_action(harness.state_mut(), TimelineAction::StepBack);
    timeline::apply_action(harness.state_mut(), TimelineAction::StepBack);
    let paused_rendered = rendered_ids_for_one_frame(&mut harness, &positions, &wipe);
    assert_ne!(
        paused_rendered,
        fixture_plus_scripted_through(SOURCE_PATHS_FIXTURE, 2),
        "guard: the canvas must genuinely be showing something else first"
    );

    timeline::apply_action(harness.state_mut(), TimelineAction::JumpLatest);
    let resumed = rendered_ids_for_one_frame(&mut harness, &positions, &wipe);

    assert_eq!(
        resumed,
        fixture_plus_scripted_through(SOURCE_PATHS_FIXTURE, 2),
        "resuming Live must restore the full live canvas"
    );
    assert!(
        !timeline::is_paused(harness.state()),
        "and the app must actually be Live again"
    );
}

/// 09-RESEARCH.md Pitfall 2 / T-09-02-02 -- the most valuable test in this plan.
///
/// `inject_layout_targets` prunes persisted node positions against the id set it
/// reads. A historical reconstruction has fewer nodes, so ONE frame rendered
/// with a reconstruction sitting in `app.model` permanently deletes the settled
/// position of every node the live graph gained after the paused point. Keeping
/// the reconstruction in its own field is what makes that pruning immune by
/// construction rather than by a runtime guard someone can later delete.
///
/// This test was watched failing against exactly that wrong implementation
/// before the redirection was written -- see 09-02-SUMMARY.md.
#[test]
fn pausing_and_resuming_does_not_discard_positions_of_later_nodes() {
    let _guard = SERVE_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let path = serve_at("hold-positions");
    let (mut harness, positions, _wipe) = live_canvas_harness(SOURCE_PATHS_FIXTURE);

    harness.run_steps(SETTLE_STEPS);
    send_and_wait(&path, &[scripted(0), scripted(1), scripted(2), scripted(3)]);
    harness.run_steps(SETTLE_STEPS);

    let before = positions.borrow().clone();
    assert_eq!(
        before.keys().cloned().collect::<BTreeSet<String>>(),
        fixture_plus_scripted_through(SOURCE_PATHS_FIXTURE, 3),
        "guard: every fixture node and every live addition must be positioned \
         and settled before the pause"
    );

    // Back to a moment BEFORE three of those four nodes existed.
    timeline::apply_action(harness.state_mut(), TimelineAction::JumpEarliest);
    assert_eq!(
        harness.state().scrub_position,
        Some(0),
        "guard: nothing was evicted, so the oldest retained event is seq 0"
    );
    // Guard the setup from the SCRIPT and the settled position map, never by
    // reading the app's own idea of what it is displaying: this test must stay
    // able to fail against an implementation that puts the reconstruction in the
    // wrong place, and such an implementation also answers "what am I
    // displaying" wrongly.
    let historical = fixture_plus_scripted_through(SOURCE_PATHS_FIXTURE, 0);
    for later in [scripted_id(1), scripted_id(2), scripted_id(3)] {
        assert!(
            !historical.contains(&later),
            "guard: `{later}` did not exist at seq 0, so it must be absent from \
             the moment about to be displayed"
        );
        assert!(
            before.contains_key(&later),
            "guard: `{later}` must hold a settled position before the pause, or \
             the pruning this test guards has nothing to delete"
        );
    }

    harness.run_steps(1);
    timeline::apply_action(harness.state_mut(), TimelineAction::JumpLatest);
    harness.run_steps(1);

    let after = positions.borrow().clone();
    let (worst_id, drift) = max_drift(&before, &after);
    assert!(
        drift < HOLD_STILL_EPS,
        "node `{worst_id}` moved {drift}px across a pause/resume cycle (was \
         {:?}, now {:?}) -- at this magnitude its persisted position was pruned \
         and re-seeded, which is what happens the moment a historical model \
         reaches the pruning read (Pitfall 2)",
        before[&worst_id],
        after[&worst_id]
    );
}
