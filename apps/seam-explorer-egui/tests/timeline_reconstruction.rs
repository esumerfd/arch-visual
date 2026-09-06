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

use egui::Modifiers;
use egui_kittest::kittest::{NodeT, Queryable};
use egui_kittest::Harness;
use seam_core::GraphEvent;
use seam_explorer_egui::app::{FocusState, SeamExplorerApp};
use seam_explorer_egui::layout::SeamLayoutState;
use seam_explorer_egui::panels::{detail, seam_list, timeline as timeline_panel};
use seam_explorer_egui::timeline::{self, TimelineAction};
use seam_explorer_egui::trace::{TraceGesture, TraceResult};
use seam_explorer_egui::{context_menu, event_stream, graph_view, history, keyboard, trace};

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

// =====================================================================
// Plan 09-03 -- the remaining display surfaces
// =====================================================================

// ---------------------------------------------------------------------
// 09-03 Task 1: the ranked seam list
// ---------------------------------------------------------------------

/// The two rendered row names this section asserts on, built the way
/// `seam_list::seam_display_name` builds them. `source_paths.json` carries no
/// `community_name`, so `Model::community_label` returns the raw ids and the
/// rendered string is exactly `"{a} \u{2194} {b}"`.
const ROW_AB: &str = "A \u{2194} B";
const ROW_AC: &str = "A \u{2194} C";

fn add_edge(source: &str, target: &str) -> GraphEvent {
    GraphEvent::AddEdge {
        source: source.to_string(),
        target: target.to_string(),
    }
}

/// Renders the LEFT panel through the real `seam_list::show`, with the same
/// live-events-first order `app.rs::ui()` uses. Deliberately the real panel
/// entry point rather than a test-local re-derivation of the row list: a test
/// that recomputes the ranking itself passes whether or not `show` was ever
/// redirected.
fn seam_list_harness(fixture: &str) -> Harness<'static, SeamExplorerApp> {
    Harness::new_ui_state(
        |ui, app: &mut SeamExplorerApp| {
            history::drain_and_apply(app);
            seam_list::show(ui, app);
        },
        loaded_app(fixture),
    )
}

/// Runs a frame and reads back, in rendered top-to-bottom order, every seam
/// row's name and its crossing-count chip. Same accesskit query idiom as
/// `tests/panels.rs::seam_list_ranked_order`, but through the non-panicking
/// `query_all_*` form: an EMPTY rendered list is a legitimate outcome here (the
/// search test asserts exactly that), and `get_all_*` panics on zero matches.
fn rendered_rows(harness: &mut Harness<'static, SeamExplorerApp>) -> (Vec<String>, Vec<String>) {
    harness.run();
    let names: Vec<String> = harness
        .query_all_by_label_contains("\u{2194}")
        .filter_map(|n| {
            let node = n.accesskit_node();
            node.label().or_else(|| node.value())
        })
        .collect();
    let counts: Vec<String> = harness
        .query_all_by_label_contains("\u{d7}")
        .filter_map(|n| {
            let node = n.accesskit_node();
            node.label().or_else(|| node.value())
        })
        .collect();
    (names, counts)
}

fn send_and_drain(
    path: &Path,
    harness: &mut Harness<'static, SeamExplorerApp>,
    events: &[GraphEvent],
) {
    send_and_wait(path, events);
    harness.run();
}

/// The shared 09-03 Task 1 setup: a seam-list harness whose LIVE ranking has
/// been genuinely reordered by real datagrams, with seq 0 a moment that ranks
/// differently.
///
/// Sent as three separate drained batches, not one, so no assertion below can
/// depend on datagram ordering within a batch. `AddNode a3` must land in its own
/// batch before any `a3` edge: `apply_add_edge`'s source resolution SYNTHESIZES
/// a node for an unknown source, so an edge arriving first would invent a
/// different `a3` than the one the script means.
///
/// Ranking, verified against the fixture:
///
/// | position | A\u{2194}B | B\u{2194}C | A\u{2194}C | top row |
/// |---|---|---|---|---|
/// | seq 0 | 3 | 2 | 2 | `A \u{2194} B` |
/// | live  | 3 | 2 | 5 | `A \u{2194} C` |
fn send_ranking_script(path: &Path, harness: &mut Harness<'static, SeamExplorerApp>) {
    send_and_drain(path, harness, &[add_edge("a1", "c2")]);
    send_and_drain(
        path,
        harness,
        &[add_node("a3", "a3", Some(SIBLING_SOURCE_FILE))],
    );
    send_and_drain(
        path,
        harness,
        &[
            add_edge("a2", "c1"),
            add_edge("a3", "c1"),
            add_edge("a3", "c2"),
        ],
    );

    assert_eq!(
        harness.state().history.next_seq(),
        5,
        "guard: five applied events, five recorded entries"
    );
    assert_eq!(
        harness.state().history.evicted_count(),
        0,
        "guard: nothing evicted, so seq 0 is a reachable position"
    );
    assert!(
        live_ids(harness.state()).contains("a3"),
        "guard: `a3` must genuinely have landed in the live model, or the \
         historical/live difference every test below turns on does not exist"
    );
}

fn ranked_list_harness(path: &Path) -> Harness<'static, SeamExplorerApp> {
    let mut harness = seam_list_harness(SOURCE_PATHS_FIXTURE);
    send_ranking_script(path, &mut harness);
    harness
}

/// TIME-03 / ROADMAP SC-3 at the ranked list: while paused, the rows are the
/// seams that existed at that moment, ranked by the crossings that existed at
/// that moment.
///
/// Both directions are asserted -- the historical top row IS at rank 0 and the
/// live top row is NOT -- so the test cannot pass on an empty or an unchanged
/// render.
#[test]
fn a_paused_seam_list_ranks_the_seams_of_that_moment() {
    let _guard = SERVE_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let path = serve_at("paused-rank");
    let mut harness = ranked_list_harness(&path);

    let (live_names, live_counts) = rendered_rows(&mut harness);
    assert_eq!(
        live_names.first().map(String::as_str),
        Some(ROW_AC),
        "guard: the LIVE list must genuinely be topped by `{ROW_AC}`, or the \
         paused claim below is vacuous -- got {live_names:?}"
    );
    assert_eq!(
        live_counts.first().map(String::as_str),
        Some("5\u{d7}"),
        "guard: the live top row's crossing count, got {live_counts:?}"
    );

    timeline::apply_action(harness.state_mut(), TimelineAction::JumpEarliest);
    assert_eq!(
        harness.state().scrub_position,
        Some(0),
        "guard: nothing was evicted, so the oldest retained event is seq 0"
    );

    let (paused_names, paused_counts) = rendered_rows(&mut harness);
    assert_eq!(
        paused_names.len(),
        3,
        "guard: three seams existed at seq 0, so an empty render is not a pass"
    );
    assert_eq!(
        paused_names.first().map(String::as_str),
        Some(ROW_AB),
        "the paused list must rank by the crossings that existed at seq 0, \
         where `{ROW_AB}` had 3 and `{ROW_AC}` had only 2 -- got {paused_names:?}"
    );
    assert_ne!(
        paused_names.first(),
        live_names.first(),
        "the LIVE top row must not be at rank 0 while paused -- a list that \
         still leads with `{ROW_AC}` is the live ranking beside a historical canvas"
    );
    assert_eq!(
        paused_counts.first().map(String::as_str),
        Some("3\u{d7}"),
        "the rendered crossing count must be the historical one, got {paused_counts:?}"
    );
}

/// The clicked-row lookup must index the DISPLAYED list. The index came from
/// enumerating that list, so reading it back out of the live list selects a
/// different seam whenever the two orders differ -- which, while paused, is
/// exactly when they do.
///
/// Then `select_seam` must score the seam it found against the historical model,
/// or the detail beside a historical canvas describes the live graph.
#[test]
fn a_paused_seam_row_click_computes_its_detail_against_the_historical_model() {
    let _guard = SERVE_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let path = serve_at("paused-click");
    let mut harness = ranked_list_harness(&path);

    // What the LIVE model would have produced for this same seam -- captured
    // before pausing, so the difference asserted below is a measured one.
    let live_bridges_a: BTreeSet<String> = {
        let app = harness.state();
        let model = app.model.as_ref().expect("the live model must exist");
        let scc = model.scc.as_ref().expect("load must finalize the SCC");
        seam_core::seam_detail(model, scc, &"A".to_string(), &"C".to_string())
            .bridges_a
            .into_iter()
            .collect()
    };
    assert_eq!(
        live_bridges_a,
        ["a1", "a2", "a3"]
            .iter()
            .map(|s| s.to_string())
            .collect::<BTreeSet<String>>(),
        "guard: the live A<->C seam must bridge through all three A-side nodes"
    );

    timeline::apply_action(harness.state_mut(), TimelineAction::JumpEarliest);
    assert_eq!(harness.state().scrub_position, Some(0));
    assert!(
        harness.state().detail.is_none() && harness.state().focus.is_none(),
        "guard: navigating cleared the selection (D-01), so the click below is \
         what sets it"
    );

    harness.run();
    harness.get_by_label_contains(ROW_AC).click();
    harness.run();

    let focus = harness
        .state()
        .focus
        .clone()
        .expect("clicking a rendered row must set the focus");
    assert_eq!(
        (focus.a.as_str(), focus.b.as_str()),
        ("A", "C"),
        "the clicked row's index must be read back out of the DISPLAYED list -- \
         at seq 0 `{ROW_AC}` sits at index 1, where the live list has A<->B"
    );

    let detail = harness
        .state()
        .detail
        .clone()
        .expect("clicking a rendered row must populate the detail");
    let bridges_a: BTreeSet<String> = detail.bridges_a.iter().cloned().collect();
    let bridges_b: BTreeSet<String> = detail.bridges_b.iter().cloned().collect();
    assert_eq!(
        bridges_a,
        ["a1"]
            .iter()
            .map(|s| s.to_string())
            .collect::<BTreeSet<String>>(),
        "at seq 0 the only A-side node crossing into C is `a1`"
    );
    assert_eq!(
        bridges_b,
        ["c1", "c2"]
            .iter()
            .map(|s| s.to_string())
            .collect::<BTreeSet<String>>(),
        "at seq 0 `a1` crosses into both `c1` (fixture) and `c2` (seq 0's own edge)"
    );
    assert_ne!(
        bridges_a, live_bridges_a,
        "a detail whose bridge set matches the live model's is the live model's \
         -- `a2`/`a3` do not cross into C at seq 0"
    );
    assert!(
        !bridges_a.contains("a3"),
        "`a3` did not exist at seq 0 and cannot bridge anything there"
    );
}

/// D-02: jump-to-latest is the route back, and the live ranking comes with it --
/// no residue from the paused list.
#[test]
fn resuming_live_restores_the_live_ranked_list() {
    let _guard = SERVE_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let path = serve_at("resume-rank");
    let mut harness = ranked_list_harness(&path);

    timeline::apply_action(harness.state_mut(), TimelineAction::JumpEarliest);
    let (paused_names, _) = rendered_rows(&mut harness);
    assert_eq!(
        paused_names.first().map(String::as_str),
        Some(ROW_AB),
        "guard: the list must genuinely be showing something else first"
    );

    timeline::apply_action(harness.state_mut(), TimelineAction::JumpLatest);
    let (resumed_names, resumed_counts) = rendered_rows(&mut harness);

    assert!(
        !timeline::is_paused(harness.state()),
        "guard: the app must actually be Live again"
    );
    assert_eq!(
        resumed_names.first().map(String::as_str),
        Some(ROW_AC),
        "resuming Live must restore the live ranking, got {resumed_names:?}"
    );
    assert_eq!(
        resumed_counts.first().map(String::as_str),
        Some("5\u{d7}"),
        "and the live crossing counts with it, got {resumed_counts:?}"
    );
}

/// The search filter reads the same `model` binding the row list does, so one
/// test keeps it from being left as the single surface still asking the live
/// graph.
///
/// `a3` is the discriminator: it is a live A-side node that did not exist at
/// seq 0, so a query naming it matches two seams live and nothing at all in the
/// historical model.
#[test]
fn a_paused_seam_list_with_a_search_query_filters_the_historical_seams() {
    let _guard = SERVE_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let path = serve_at("paused-search");
    let mut harness = ranked_list_harness(&path);

    harness.state_mut().search_query = "a3".to_string();
    let (live_names, _) = rendered_rows(&mut harness);
    assert_eq!(
        live_names.len(),
        2,
        "guard: live, `a3` is an A-side node label, so both A-side seams match \
         -- got {live_names:?}"
    );

    timeline::apply_action(harness.state_mut(), TimelineAction::JumpEarliest);
    assert_eq!(harness.state().scrub_position, Some(0));
    assert_eq!(
        harness.state().search_query,
        "a3",
        "guard: navigating must not have cleared the query"
    );

    let (paused_names, _) = rendered_rows(&mut harness);
    assert!(
        paused_names.is_empty(),
        "`a3` did not exist at seq 0, so the historical filter must match \
         nothing -- got {paused_names:?}"
    );
    harness.get_by_label_contains("No component or seam matches");
}

// ---------------------------------------------------------------------
// 09-03 Task 2: the detail panel
// ---------------------------------------------------------------------

/// A live node that exists only in the middle of the script's timeline, with a
/// LABEL deliberately different from its ID. `node_label` falls back to the raw
/// id when the model it asks cannot find the node, so id-equals-label would make
/// the historical and live renderings identical and the hop-label test vacuous.
const GONE_ID: &str = "gone";
const GONE_LABEL: &str = "GoneService";

/// Renders the left panel and then the right panel into the same `Ui`, in
/// `app.rs::ui()`'s own order, so a row click and the detail it produces are
/// observed through the two REAL panel entry points rather than through
/// `app.detail` alone.
fn seam_list_and_detail_harness(fixture: &str) -> Harness<'static, SeamExplorerApp> {
    Harness::new_ui_state(
        |ui, app: &mut SeamExplorerApp| {
            history::drain_and_apply(app);
            seam_list::show(ui, app);
            detail::show(ui, app);
        },
        loaded_app(fixture),
    )
}

fn detail_only_harness(fixture: &str) -> Harness<'static, SeamExplorerApp> {
    Harness::new_ui_state(
        |ui, app: &mut SeamExplorerApp| {
            history::drain_and_apply(app);
            detail::show(ui, app);
        },
        loaded_app(fixture),
    )
}

/// The script for the two tests that need a node and a seam which exist at a
/// historical moment and NOT live. Four separately-drained batches, for the same
/// datagram-ordering reason `send_ranking_script` gives.
///
/// | position | `gone` | `A \u{2194} C` seam |
/// |---|---|---|
/// | seq 1 | present, labelled `GoneService` | present (a1\u{2192}c1, gone\u{2192}c1) |
/// | live  | removed                          | absent (0 crossings) |
fn send_disappearing_script(path: &Path, harness: &mut Harness<'static, SeamExplorerApp>) {
    send_and_drain(
        path,
        harness,
        &[add_node(GONE_ID, GONE_LABEL, Some(SIBLING_SOURCE_FILE))],
    );
    send_and_drain(path, harness, &[add_edge(GONE_ID, "c1")]);
    send_and_drain(
        path,
        harness,
        &[GraphEvent::RemoveNode {
            id: GONE_ID.to_string(),
        }],
    );
    send_and_drain(
        path,
        harness,
        &[GraphEvent::RemoveEdge {
            source: "a1".to_string(),
            target: "c1".to_string(),
        }],
    );

    assert_eq!(
        harness.state().history.next_seq(),
        4,
        "guard: four applied events, four recorded entries, so seq 1 is the \
         moment `gone` and its edge both exist"
    );
    assert_eq!(
        harness.state().history.evicted_count(),
        0,
        "guard: nothing evicted, so seq 1 is a reachable position"
    );
    assert!(
        !live_ids(harness.state()).contains(GONE_ID),
        "guard: `{GONE_ID}` must genuinely be absent from the LIVE model"
    );
    assert!(
        !harness
            .state()
            .seams
            .iter()
            .any(|s| (s.a == "A" && s.b == "C") || (s.a == "C" && s.b == "A")),
        "guard: the LIVE ranked list must genuinely have no A<->C seam left, or \
         the crossed-seam claim below is vacuous -- got {:?}",
        harness.state().seams
    );
}

/// Steps back to seq 1 -- the moment `gone` and its crossing edge both exist --
/// and guards that we landed there.
fn pause_at_seq_1(harness: &mut Harness<'static, SeamExplorerApp>) {
    timeline::apply_action(harness.state_mut(), TimelineAction::StepBack);
    timeline::apply_action(harness.state_mut(), TimelineAction::StepBack);
    assert_eq!(
        harness.state().scrub_position,
        Some(1),
        "guard: two steps back from Live (whose effective position is seq 3) is seq 1"
    );
    assert!(
        scrub_ids(harness.state()).contains(GONE_ID),
        "guard: `{GONE_ID}` must be present in the reconstruction of seq 1"
    );
}

/// Installs a REAL trace computed against whatever model is currently being
/// DISPLAYED -- which is exactly what a drag-to-trace on a paused canvas
/// produces once 09-03 Task 3 lands.
fn install_display_trace(app: &mut SeamExplorerApp, from: &str, to: &str) {
    let model = timeline::display_model(app).expect("something must be displayed");
    let path = seam_core::trace_path(model, from, to)
        .unwrap_or_else(|| panic!("the DISPLAYED graph must have a path {from} -> {to}"));
    app.trace = Some(TraceResult {
        from: from.to_string(),
        to: to.to_string(),
        path: Some(path),
    });
}

/// The rendered bridge-component chips must be the ones that bridged that seam
/// at the displayed moment.
///
/// The write site this depends on (`seam_list::select_seam`) was redirected by
/// Task 1, so this is a CONFIRMATION through the rendered detail panel rather
/// than a fresh red bar -- reported as such rather than manufactured into one
/// (07-02 / 05-16 precedent).
#[test]
fn a_paused_detail_panel_labels_bridge_components_from_the_historical_model() {
    let _guard = SERVE_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let path = serve_at("paused-bridges");
    let mut harness = seam_list_and_detail_harness(SOURCE_PATHS_FIXTURE);
    send_ranking_script(&path, &mut harness);

    timeline::apply_action(harness.state_mut(), TimelineAction::JumpEarliest);
    assert_eq!(harness.state().scrub_position, Some(0));

    harness.run();
    harness.get_by_label_contains(ROW_AC).click();
    harness.run();

    // At seq 0 only `a1` crosses into C; live, `a2` and `a3` do too.
    harness.get_by_label("a1");
    harness.get_by_label("c1");
    harness.get_by_label("c2");
    assert_eq!(
        harness.query_all_by_label("a3").count(),
        0,
        "`a3` did not exist at seq 0 and must not be rendered as a bridge \
         component of the moment being displayed"
    );
    assert_eq!(
        harness.query_all_by_label("a2").count(),
        0,
        "`a2` gained its crossing into C at seq 2 and must not be rendered as a \
         bridge component of seq 0"
    );
}

/// `find_seam_for_pair` must search the DISPLAYED seam list. Paused at seq 1 the
/// `A \u{2194} C` seam exists; live it does not exist at all, so a lookup against
/// `app.seams` finds nothing and the click is silently swallowed.
///
/// Combined with Task 1's redirected `select_seam`, the click then scores the
/// seam it found against the historical model too -- the two changes have to
/// land together, or a crossed-seam click while paused finds a historical seam
/// and scores it against the live graph.
#[test]
fn a_paused_crossed_seam_click_focuses_the_historical_seam() {
    let _guard = SERVE_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let path = serve_at("paused-crossed");
    let mut harness = detail_only_harness(SOURCE_PATHS_FIXTURE);
    send_disappearing_script(&path, &mut harness);

    pause_at_seq_1(&mut harness);
    install_display_trace(harness.state_mut(), "a1", "c1");
    let crossed = harness
        .state()
        .trace
        .as_ref()
        .and_then(|t| t.path.as_ref())
        .expect("guard: the trace must have resolved a path")
        .seams_crossed
        .clone();
    assert_eq!(
        crossed,
        vec![("A".to_string(), "C".to_string())],
        "guard: the historical a1 -> c1 hop must cross exactly the A<->C seam"
    );

    harness.run();
    harness.get_by_label_contains("A \u{2192} C").click();
    harness.run();

    let focus = harness
        .state()
        .focus
        .clone()
        .expect("clicking a crossed-seam entry must focus that seam");
    assert_eq!(
        (focus.a.as_str(), focus.b.as_str()),
        ("A", "C"),
        "the seam looked up must be the HISTORICAL one -- the live ranked list \
         has no A<->C entry at all"
    );

    let detail = harness
        .state()
        .detail
        .clone()
        .expect("focusing must populate the detail");
    let bridges_a: BTreeSet<String> = detail.bridges_a.iter().cloned().collect();
    assert!(
        bridges_a.contains(GONE_ID),
        "the detail must be scored against the historical model, where \
         `{GONE_ID}` is one of the A-side bridges -- got {bridges_a:?}"
    );
}

/// `node_label` must resolve hop ids in the model that rendered them, including
/// the case where the live graph no longer has the node at all.
///
/// `gone` is labelled `GoneService`, so the historical rendering and the raw-id
/// fallback are textually distinguishable.
#[test]
fn a_paused_trace_hop_label_names_the_node_on_screen() {
    let _guard = SERVE_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let path = serve_at("paused-hop");
    let mut harness = detail_only_harness(SOURCE_PATHS_FIXTURE);
    send_disappearing_script(&path, &mut harness);

    pause_at_seq_1(&mut harness);
    install_display_trace(harness.state_mut(), GONE_ID, "c1");
    assert_eq!(
        harness
            .state()
            .trace
            .as_ref()
            .and_then(|t| t.path.as_ref())
            .expect("guard: the trace must have resolved a path")
            .hops,
        vec![GONE_ID.to_string(), "c1".to_string()],
        "guard: the historical path must be exactly gone -> c1"
    );

    harness.run();

    harness.get_by_label_contains(&format!("{GONE_LABEL} \u{2192} c1"));
    assert!(
        harness.query_all_by_label_contains(GONE_LABEL).count() >= 2,
        "`{GONE_LABEL}` must name the node in BOTH the from/to heading and the \
         hop chip -- a lookup against the live model finds no `{GONE_ID}` at \
         all and falls back to the raw id"
    );
}

// ---------------------------------------------------------------------
// 09-03 Task 3: drag-to-trace, the node context menu, and the standing
// layout-pruning guard
// ---------------------------------------------------------------------

/// A canvas harness with two plant slots and one read-back mirror, so the two
/// reads this task redirects can be reached through the REAL `graph_view::show`
/// without simulating pixel-perfect pointer geometry.
///
/// Planting is legitimate, not a shortcut around the production path: both
/// values are exactly what the live half of each feature records for the next
/// frame to consume. `handle_trace_gesture` loads its gesture from
/// `trace::load_gesture` every frame and acts on a `Completed` one; and
/// `handle_context_menu` deliberately loads the remembered target FIRST so a
/// non-click frame re-rendering an already-open menu keeps whatever was last
/// recorded. Both redirected reads sit AFTER those loads, on the real code path.
#[allow(clippy::type_complexity)]
fn probe_canvas_harness(
    fixture: &str,
) -> (
    Harness<'static, SeamExplorerApp>,
    Rc<RefCell<Option<TraceGesture>>>,
    Rc<RefCell<Option<String>>>,
    Rc<RefCell<Option<String>>>,
) {
    let plant_gesture: Rc<RefCell<Option<TraceGesture>>> = Rc::new(RefCell::new(None));
    let plant_target: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
    let observed_target: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));

    let gesture_inner = plant_gesture.clone();
    let target_inner = plant_target.clone();
    let observed_inner = observed_target.clone();
    let harness = Harness::new_ui_state(
        move |ui, app: &mut SeamExplorerApp| {
            history::drain_and_apply(app);
            if let Some(gesture) = gesture_inner.borrow_mut().take() {
                trace::save_gesture(ui, gesture);
            }
            if let Some(id) = target_inner.borrow_mut().take() {
                context_menu::save_target(ui, Some(id));
            }
            graph_view::show(ui, app);
            *observed_inner.borrow_mut() = context_menu::load_target(ui);
        },
        loaded_app(fixture),
    );
    (harness, plant_gesture, plant_target, observed_target)
}

/// `send_and_drain`'s canvas counterpart. The canvas harness renders through
/// `egui_graphs`, whose layout keeps requesting repaints, so `Harness::run`
/// (which runs to quiescence and panics at `max_steps`) cannot be used here --
/// a bounded `run_steps` is the harness's own documented answer.
fn send_and_step(
    path: &Path,
    harness: &mut Harness<'static, SeamExplorerApp>,
    events: &[GraphEvent],
) {
    send_and_wait(path, events);
    harness.run_steps(2);
}

/// The hop list `seam_core::trace_path` resolves for `from -> to` in a given
/// model, as owned strings.
fn hops_in(model: &seam_core::Model, from: &str, to: &str) -> Vec<String> {
    seam_core::trace_path(model, from, to)
        .unwrap_or_else(|| panic!("expected a path {from} -> {to}"))
        .hops
}

/// 09-RESEARCH.md Open Question 2, first site. A trace computes a path between
/// two nodes the user dragged between ON THE CANVAS. While paused those nodes
/// came from the historical model, so running the search against the live model
/// would draw a path over a canvas whose edges do not support it.
///
/// The fixture's own route from `a2` to `c1` runs through `b1`; the script adds
/// a direct `a2 -> c1` shortcut that exists only live. The test asserts the
/// specific expected hop list, and guards -- in-test, the way 05-15's re-arm
/// test does -- that the historical and live outcomes genuinely differ, so a
/// fixture change cannot quietly turn it into a tautology.
#[test]
fn a_paused_trace_runs_against_the_historical_graph() {
    let _guard = SERVE_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let path = serve_at("paused-trace");
    let (mut harness, plant_gesture, _plant_target, _observed) =
        probe_canvas_harness(SOURCE_PATHS_FIXTURE);

    send_and_step(
        &path,
        &mut harness,
        &[add_node("live_000", "live_000", Some(SIBLING_SOURCE_FILE))],
    );
    send_and_step(&path, &mut harness, &[add_edge("a2", "c1")]);
    assert_eq!(
        harness.state().history.next_seq(),
        2,
        "guard: two applied events, two recorded entries"
    );

    let live_hops = hops_in(
        harness.state().model.as_ref().expect("a live model"),
        "a2",
        "c1",
    );
    assert_eq!(
        live_hops,
        vec!["a2".to_string(), "c1".to_string()],
        "guard: live, the script's own shortcut is the shortest route"
    );

    // Trace mode on -- drag-to-trace is the gesture this read serves, and
    // `handle_trace_gesture` returns immediately when trace mode is off.
    harness.state_mut().trace_mode = true;
    timeline::apply_action(harness.state_mut(), TimelineAction::StepBack);
    assert_eq!(
        harness.state().scrub_position,
        Some(0),
        "guard: one step back from Live (effective seq 1) is seq 0"
    );

    let historical_hops = hops_in(
        harness
            .state()
            .scrub_model
            .as_ref()
            .expect("a paused app must hold a reconstruction"),
        "a2",
        "c1",
    );
    assert_ne!(
        historical_hops, live_hops,
        "guard: the historical and live outcomes must genuinely differ, or this \
         test is a tautology"
    );

    *plant_gesture.borrow_mut() = Some(TraceGesture::Completed {
        from: "a2".to_string(),
        to: "c1".to_string(),
    });
    harness.run_steps(1);

    let trace = harness
        .state()
        .trace
        .clone()
        .expect("a completed gesture must install a trace result");
    let resolved = trace
        .path
        .expect("the historical graph must still resolve a route a2 -> c1");
    assert_eq!(
        resolved.hops,
        vec!["a2".to_string(), "b1".to_string(), "c1".to_string()],
        "the paused trace must run against the graph the dragged nodes came \
         from -- the direct a2 -> c1 edge is not on that canvas"
    );
    assert_ne!(
        resolved.hops, live_hops,
        "a path drawn over edges the paused canvas does not show is the exact \
         lie this redirection exists to remove"
    );
}

/// 09-RESEARCH.md Open Question 2, second site. The comment above the read
/// states the invariant plainly: re-look-up the target in the CURRENT model
/// every frame, never cache the node's fields across frames.
///
/// `display_model` IS that current model once a paused view exists -- it is the
/// graph that rendered the node under the cursor. Leaving it live BREAKS the
/// invariant: a node visible on a paused canvas but since removed live fails its
/// `index` lookup and the menu is dismissed with no explanation.
#[test]
fn a_paused_context_menu_resolves_a_node_the_live_graph_no_longer_has() {
    let _guard = SERVE_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let path = serve_at("paused-menu");
    let (mut harness, _plant_gesture, plant_target, observed) =
        probe_canvas_harness(SOURCE_PATHS_FIXTURE);

    send_and_step(
        &path,
        &mut harness,
        &[add_node(GONE_ID, GONE_LABEL, Some(SIBLING_SOURCE_FILE))],
    );
    send_and_step(
        &path,
        &mut harness,
        &[GraphEvent::RemoveNode {
            id: GONE_ID.to_string(),
        }],
    );
    assert_eq!(
        harness.state().history.next_seq(),
        2,
        "guard: two applied events, two recorded entries"
    );
    assert!(
        !live_ids(harness.state()).contains(GONE_ID),
        "guard: `{GONE_ID}` must genuinely be gone from the LIVE model, or an \
         unredirected lookup would find it and this test would pass for the \
         wrong reason"
    );

    timeline::apply_action(harness.state_mut(), TimelineAction::StepBack);
    assert_eq!(
        harness.state().scrub_position,
        Some(0),
        "guard: one step back from Live (effective seq 1) is seq 0"
    );
    assert!(
        scrub_ids(harness.state()).contains(GONE_ID),
        "guard: `{GONE_ID}` must be on the paused canvas the user is pointing at"
    );

    *plant_target.borrow_mut() = Some(GONE_ID.to_string());
    harness.run_steps(1);

    assert_eq!(
        observed.borrow().as_deref(),
        Some(GONE_ID),
        "the target must resolve against the model that RENDERED the node -- an \
         unredirected lookup misses it, clears the target and dismisses the menu \
         with no explanation"
    );

    // And the file it would open is the one the graph recorded for that node.
    let source_file = harness
        .state()
        .scrub_model
        .as_ref()
        .and_then(|m| m.index.get(GONE_ID).map(|&idx| &m.graph[idx]))
        .and_then(|node| node.source_file.clone());
    assert_eq!(
        source_file.as_deref(),
        Some(SIBLING_SOURCE_FILE),
        "the historical node carries the source_file the graph recorded for it, \
         which is the file the user is pointing at"
    );
}

/// The deliberate exception, restated behaviourally and kept as a STANDING test
/// now that three more redirections have landed in `graph_view.rs`'s file.
///
/// `inject_layout_targets` prunes persisted node positions against the id set it
/// reads. A historical reconstruction has fewer nodes, so redirecting that one
/// read would permanently delete the settled position of every node the live
/// graph gained after the paused point (09-RESEARCH.md Pitfall 2, T-09-02-02).
/// 09-02 watched this claim fail against exactly that wrong implementation.
#[test]
fn the_layout_prune_still_reads_the_live_model() {
    let _guard = SERVE_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let path = serve_at("prune-live");
    let (mut harness, positions, _wipe) = live_canvas_harness(SOURCE_PATHS_FIXTURE);

    harness.run_steps(SETTLE_STEPS);
    send_and_wait(&path, &[scripted(0), scripted(1), scripted(2)]);
    harness.run_steps(SETTLE_STEPS);

    let before = positions.borrow().clone();
    assert_eq!(
        before.keys().cloned().collect::<BTreeSet<String>>(),
        fixture_plus_scripted_through(SOURCE_PATHS_FIXTURE, 2),
        "guard: every fixture node and every live addition must be positioned \
         and settled before the pause"
    );

    timeline::apply_action(harness.state_mut(), TimelineAction::JumpEarliest);
    assert_eq!(
        harness.state().scrub_position,
        Some(0),
        "guard: nothing was evicted, so the oldest retained event is seq 0"
    );
    let historical = fixture_plus_scripted_through(SOURCE_PATHS_FIXTURE, 0);
    for later in [scripted_id(1), scripted_id(2)] {
        assert!(
            !historical.contains(&later),
            "guard: `{later}` did not exist at seq 0, so it is absent from the \
             moment about to be displayed"
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
         and re-seeded, which is what happens the moment `inject_layout_targets` \
         is redirected too (Pitfall 2)",
        before[&worst_id],
        after[&worst_id]
    );
}

// =====================================================================
// Plan 09-05 -- the bottom timeline panel
// =====================================================================

// ---------------------------------------------------------------------
// 09-05 Task 1: the panel itself
// ---------------------------------------------------------------------

/// How many events the panel tests record. Small on purpose: this section's
/// subject is what the panel RENDERS and which navigation a button reaches,
/// and 09-02 already owns the buffer-wrap arithmetic.
const PANEL_EVENTS: usize = 6;

/// An app with a real replay baseline and `count` recorded events, built
/// SYNCHRONOUSLY rather than through the socket.
///
/// The socket is this file's default and stays so -- every reconstruction claim
/// above is proven through it, and Task 3's tests below drive it again. This
/// second recipe exists for one concrete reason:
/// `the_step_back_button_and_alt_left_produce_the_same_position` needs TWO
/// identically-constructed apps, and the event receiver is process-global, so a
/// socket-fed pair cannot be built identically without interleaving their
/// streams.
///
/// Still built through `apply_load_outcome` -- this file's stated discipline --
/// so the replay baseline is real. The recording loop mirrors
/// `history::drain_and_apply`'s exact order (`apply_batch`, `finalize_scc` on a
/// topology change, `detect`, then push the RESOLVED events), the same shape
/// `keyboard_scrub.rs::app_with_history` established, including its guards.
fn app_with_recorded_history(count: usize) -> SeamExplorerApp {
    let mut app = loaded_app(SOURCE_PATHS_FIXTURE);
    assert!(
        app.replay_baseline.is_some(),
        "guard: the setup must route through apply_load_outcome so a real \
         baseline exists -- without it every reconstruction below silently \
         returns an empty model and these tests pass vacuously"
    );

    for n in 0..count {
        let event = scripted(n);
        let applied = {
            let model = app
                .model
                .as_mut()
                .expect("apply_load_outcome must have installed a model");
            let outcome = seam_core::apply_batch(model, std::slice::from_ref(&event));
            if outcome.topology_changed {
                model.finalize_scc();
            }
            outcome
        };
        if applied.topology_changed {
            if let Some(model) = app.model.as_ref() {
                app.seams = seam_core::detect(model);
            }
        }
        for event in &applied.applied {
            app.history.push(event.clone());
        }
    }

    assert_eq!(
        app.history.next_seq(),
        count as u64,
        "guard: every scripted event must have applied and recorded exactly one \
         entry -- a silent setup failure here would make every assertion below \
         pass for the wrong reason"
    );
    app
}

/// A harness whose whole per-frame body is the real `panels::timeline::show`,
/// the same way `panels.rs` tests every other panel.
///
/// Deliberately NO `drain_and_apply` here. These tests build their history
/// synchronously and never touch the socket, so draining the process-global
/// receiver could only pull in another test's events. Task 2's whole-app tests
/// below drive the real `ui()`, which does drain, and take the file lock for it.
fn timeline_panel_harness(app: SeamExplorerApp) -> Harness<'static, SeamExplorerApp> {
    Harness::new_ui_state(
        |ui, app: &mut SeamExplorerApp| {
            timeline_panel::show(ui, app);
        },
        app,
    )
}

/// The keyboard's own harness, reused verbatim in shape from
/// `keyboard_scrub.rs`: the per-frame body is the real `keyboard::handle`, and
/// presses go through `egui_kittest`'s `key_press_modifiers` so the key-down
/// frame observes `RawInput::modifiers` already set, exactly as a real keypress
/// does.
fn keyboard_harness(app: SeamExplorerApp) -> Harness<'static, SeamExplorerApp> {
    Harness::new_ui_state(
        |ui, app: &mut SeamExplorerApp| {
            let ctx = ui.ctx().clone();
            keyboard::handle(&ctx, app, seam_list::search_field_id());
        },
        app,
    )
}

fn press(harness: &mut Harness<'static, SeamExplorerApp>, modifiers: Modifiers, key: egui::Key) {
    harness.key_press_modifiers(modifiers, key);
    harness.step();
}

/// The position string the panel renders for the app's current status, asked of
/// the panel's own formatter rather than re-typed here. A test that re-types the
/// literal it is asserting passes forever after someone edits only the panel.
fn rendered_position(app: &SeamExplorerApp) -> String {
    timeline_panel::position_text(&timeline::status(app))
}

/// D-04: the badge and the position are visible from the first frame, before the
/// user has navigated anything -- not revealed only after first use.
#[test]
fn the_timeline_panel_shows_live_and_the_count_before_any_navigation() {
    let mut harness = timeline_panel_harness(app_with_recorded_history(PANEL_EVENTS));
    harness.run();

    let status = timeline::status(harness.state());
    assert!(
        !status.paused,
        "guard: nothing has navigated, so the app must genuinely be Live"
    );
    assert_eq!(
        (status.position, status.total),
        (PANEL_EVENTS as u64, PANEL_EVENTS as u64),
        "guard: while Live the displayed position IS the newest recorded event"
    );

    harness.get_by_label(timeline_panel::LIVE_BADGE);
    assert_eq!(
        harness
            .query_all_by_label(timeline_panel::PAUSED_BADGE)
            .count(),
        0,
        "a Live view must not also claim to be Paused"
    );

    let position = rendered_position(harness.state());
    assert_eq!(
        position,
        format!("Event {PANEL_EVENTS} of {PANEL_EVENTS}"),
        "the formatter must produce the exact wording TIME-04 names"
    );
    harness.get_by_label(&position);
}

/// The other half of D-04, and the one that proves the badge is bound to state
/// rather than painted once: after a single step the panel says Paused and names
/// the position the user actually landed on.
#[test]
fn the_timeline_panel_shows_paused_and_the_position_after_navigating() {
    let mut harness = timeline_panel_harness(app_with_recorded_history(PANEL_EVENTS));
    harness.run();
    harness.get_by_label(timeline_panel::LIVE_BADGE);

    timeline::apply_action(harness.state_mut(), TimelineAction::StepBack);
    harness.run();

    assert_eq!(
        harness.state().scrub_position,
        Some(PANEL_EVENTS as u64 - 2),
        "guard: one step back from Live (effective seq {}) is seq {}",
        PANEL_EVENTS - 1,
        PANEL_EVENTS - 2
    );

    harness.get_by_label(timeline_panel::PAUSED_BADGE);
    assert_eq!(
        harness
            .query_all_by_label(timeline_panel::LIVE_BADGE)
            .count(),
        0,
        "a Paused view must not still claim to be Live"
    );

    let position = rendered_position(harness.state());
    assert_eq!(
        position,
        format!("Event {} of {PANEL_EVENTS}", PANEL_EVENTS - 1),
        "the position must be the DECREMENTED one, one-based -- seq {} is the \
         {}th event",
        PANEL_EVENTS - 2,
        PANEL_EVENTS - 1
    );
    harness.get_by_label(&position);
}

/// D-04 taken literally: "persistently visible" includes the state before any
/// event has ever arrived. "Event 0 of 0" would invite the reader to wonder
/// which event zero is, so the position reads an honest empty string instead --
/// and the badge still renders, because the panel is never hidden.
#[test]
fn the_timeline_panel_renders_its_empty_state_with_no_events() {
    let mut harness = timeline_panel_harness(loaded_app(SOURCE_PATHS_FIXTURE));
    harness.run();

    assert_eq!(
        harness.state().history.next_seq(),
        0,
        "guard: a freshly loaded app must have recorded nothing"
    );

    harness.get_by_label(timeline_panel::LIVE_BADGE);
    harness.get_by_label(timeline_panel::EMPTY_POSITION);
    assert_eq!(
        rendered_position(harness.state()),
        timeline_panel::EMPTY_POSITION,
        "with nothing recorded the formatter must produce the empty-state string"
    );
    assert_eq!(
        harness.query_all_by_label_contains("Event ").count(),
        0,
        "there is no event zero to name"
    );
}

/// Enablement is DERIVED from `timeline::next_position` -- the same pure
/// function the keyboard's navigation resolves through -- so "would this button
/// do anything" and "what would this key do" cannot diverge. With an empty
/// history every action is a no-op, which is the whole of the no-graph-loaded
/// case and needs no separate branch.
#[test]
fn every_button_is_disabled_with_an_empty_history() {
    let mut harness = timeline_panel_harness(loaded_app(SOURCE_PATHS_FIXTURE));
    harness.run();

    assert_eq!(
        harness.state().history.next_seq(),
        0,
        "guard: the history must genuinely be empty"
    );

    for label in [
        timeline_panel::EARLIEST_LABEL,
        timeline_panel::STEP_BACK_LABEL,
        timeline_panel::STEP_FORWARD_LABEL,
        timeline_panel::LATEST_LABEL,
    ] {
        let node = harness.get_by_label(label);
        assert!(
            node.accesskit_node().is_disabled(),
            "`{label}` must be disabled with nothing recorded -- there is no \
             position for it to move to"
        );
    }
}

/// ROADMAP SC-1 made a test rather than a claim: "either route produces the same
/// single step".
///
/// Two identically-constructed apps, one driven by a real button click and one
/// by a real Alt+Left keypress. The "both actually moved" guard is what stops
/// this passing on two apps that each did nothing.
#[test]
fn the_step_back_button_and_alt_left_produce_the_same_position() {
    let mut by_button = timeline_panel_harness(app_with_recorded_history(PANEL_EVENTS));
    let mut by_key = keyboard_harness(app_with_recorded_history(PANEL_EVENTS));

    by_button.run();
    by_key.run();
    assert_eq!(
        by_button.state().scrub_position,
        None,
        "guard: the button-driven app starts Live"
    );
    assert_eq!(
        by_key.state().scrub_position,
        None,
        "guard: the key-driven app starts Live"
    );

    by_button
        .get_by_label(timeline_panel::STEP_BACK_LABEL)
        .click();
    by_button.run();

    press(&mut by_key, Modifiers::ALT, egui::Key::ArrowLeft);

    assert_ne!(
        by_button.state().scrub_position,
        None,
        "guard: the button must genuinely have moved the position, or the \
         equality below is two apps agreeing that nothing happened"
    );
    assert_ne!(
        by_key.state().scrub_position,
        None,
        "guard: the key must genuinely have moved the position"
    );
    assert_eq!(
        by_button.state().scrub_position,
        by_key.state().scrub_position,
        "the button and the key both hand the same TimelineAction to the same \
         timeline::apply_action, so they cannot land anywhere else"
    );
}

/// D-02: jump-to-latest is the only route that resumes Live, and the button is
/// its on-screen equivalent.
#[test]
fn the_latest_button_resumes_live_from_a_paused_position() {
    let mut harness = timeline_panel_harness(app_with_recorded_history(PANEL_EVENTS));

    timeline::apply_action(harness.state_mut(), TimelineAction::StepBack);
    timeline::apply_action(harness.state_mut(), TimelineAction::StepBack);
    harness.run();
    assert!(
        timeline::is_paused(harness.state()),
        "guard: the app must genuinely be Paused before the button is clicked"
    );
    harness.get_by_label(timeline_panel::PAUSED_BADGE);

    harness.get_by_label(timeline_panel::LATEST_LABEL).click();
    harness.run();

    assert_eq!(
        harness.state().scrub_position,
        None,
        "the Latest button resumes Live literally, exactly as Cmd+Right does"
    );
    assert!(!timeline::is_paused(harness.state()));
    harness.get_by_label(timeline_panel::LIVE_BADGE);
}

// ---------------------------------------------------------------------
// 09-05 Task 2: the panel wired into the running app
// ---------------------------------------------------------------------

/// A harness whose per-frame body is the REAL `eframe::App::ui` -- the whole
/// assembled app, drain and top/left/right/bottom/central panels and keyboard
/// handling included.
///
/// `eframe::Frame::_new_kittest()` is eframe's own constructor for exactly this
/// (`epi.rs`), which is what makes driving the real `ui()` possible at all: a
/// `Frame` cannot otherwise be built outside a running event loop, and that is
/// why every earlier test in this project reaches a panel entry point directly
/// instead.
///
/// `run_steps` rather than `run`: `graph_view::show` renders through
/// `egui_graphs`, whose layout keeps requesting repaints, so `Harness::run`
/// (which runs to quiescence and panics at `max_steps`) cannot be used -- the
/// same reason `send_and_step` above exists.
fn app_ui_harness(app: SeamExplorerApp) -> Harness<'static, SeamExplorerApp> {
    let mut frame = eframe::Frame::_new_kittest();
    Harness::new_ui_state(
        move |ui, app: &mut SeamExplorerApp| {
            <SeamExplorerApp as eframe::App>::ui(app, ui, &mut frame);
        },
        app,
    )
}

/// The panel is reachable in the ASSEMBLED app, not only in a panel-scoped
/// harness. Without this, `app.rs`'s dispatch line could be missing entirely and
/// every Task 1 test would still pass.
#[test]
fn the_running_app_shows_the_timeline_panel() {
    let _guard = SERVE_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut harness = app_ui_harness(app_with_recorded_history(PANEL_EVENTS));
    harness.run_steps(3);

    harness.get_by_label(timeline_panel::LIVE_BADGE);
    let position = rendered_position(harness.state());
    assert_eq!(
        position,
        format!("Event {PANEL_EVENTS} of {PANEL_EVENTS}"),
        "guard: the assembled app must be Live at the newest event"
    );
    harness.get_by_label(&position);

    for label in [
        timeline_panel::EARLIEST_LABEL,
        timeline_panel::STEP_BACK_LABEL,
        timeline_panel::STEP_FORWARD_LABEL,
        timeline_panel::LATEST_LABEL,
    ] {
        harness.get_by_label(label);
    }
}

/// The whole assembly end to end: real `ui()`, real button, real navigation,
/// asserted position. TIME-01's second route, in the app the user actually runs.
#[test]
fn clicking_step_back_in_the_running_app_moves_the_position() {
    let _guard = SERVE_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut harness = app_ui_harness(app_with_recorded_history(PANEL_EVENTS));
    harness.run_steps(3);
    assert_eq!(
        harness.state().scrub_position,
        None,
        "guard: the assembled app starts Live"
    );

    harness
        .get_by_label(timeline_panel::STEP_BACK_LABEL)
        .click();
    harness.run_steps(3);

    assert_eq!(
        harness.state().scrub_position,
        Some(PANEL_EVENTS as u64 - 2),
        "clicking the real button in the real app must step exactly one event \
         back, the same single step Alt+Left produces"
    );
    harness.get_by_label(timeline_panel::PAUSED_BADGE);
    harness.get_by_label(&format!("Event {} of {PANEL_EVENTS}", PANEL_EVENTS - 1));
}

// ---------------------------------------------------------------------
// 09-05 Task 3: events arriving while paused move nothing
// ---------------------------------------------------------------------

/// A live node id that plays no part in any historical claim -- it exists only
/// to make an unrelated live event arrive with a real topology change.
const UNRELATED_ID: &str = "unrelated";

/// D-01 / TIME-06, at the one place the earlier plans left open.
///
/// `history::drain_and_apply` runs every frame and, on any topology change,
/// applies a stale-selection rule that tests the LIVE ranked list and recomputes
/// `app.detail` against the LIVE model. That rule is right for a live view and
/// wrong for a paused one: while paused the selection describes the
/// reconstruction, so an unrelated live event either clears the user's focus
/// (because the historical seam is absent from the live list) or replaces their
/// historical verdict with a live one. Both are TIME-06 violations caused by an
/// event the user never asked to see.
///
/// Deliberately set up so the seam the user selected does NOT exist live at all:
/// that is the failure mode with teeth, and it makes the assertion below fail
/// loudly rather than subtly.
#[test]
fn a_live_event_does_not_disturb_a_selection_made_while_paused() {
    let _guard = SERVE_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let path = serve_at("paused-sel");
    let mut harness = seam_list_harness(SOURCE_PATHS_FIXTURE);
    send_disappearing_script(&path, &mut harness);

    pause_at_seq_1(&mut harness);

    // The selection, made by a real click on a real rendered row of the
    // HISTORICAL list.
    harness.run();
    harness.get_by_label_contains(ROW_AC).click();
    harness.run();

    let focus = harness
        .state()
        .focus
        .clone()
        .expect("clicking a rendered row must set the focus");
    assert_eq!(
        (focus.a.as_str(), focus.b.as_str()),
        ("A", "C"),
        "guard: the selected seam must be the historical A<->C, which the LIVE \
         ranked list does not contain at all"
    );
    let detail = harness
        .state()
        .detail
        .clone()
        .expect("clicking a rendered row must populate the detail");
    assert!(
        detail.bridges_a.iter().any(|id| id == GONE_ID),
        "guard: the detail must be the HISTORICAL one -- `{GONE_ID}` bridges A \
         into C at seq 1 and does not exist in the live model at all -- got {:?}",
        detail.bridges_a
    );
    let position = harness.state().scrub_position;

    // An unrelated live event the user never asked to see. `AddNode` sets
    // `topology_changed`, which is what drives the stale-selection rule.
    send_and_drain(
        &path,
        &mut harness,
        &[add_node(
            UNRELATED_ID,
            UNRELATED_ID,
            Some(SIBLING_SOURCE_FILE),
        )],
    );
    assert_eq!(
        harness.state().history.next_seq(),
        5,
        "guard: the live event must genuinely have applied and been recorded, \
         or nothing had the chance to disturb the selection"
    );
    assert!(
        live_ids(harness.state()).contains(UNRELATED_ID),
        "guard: `{UNRELATED_ID}` must genuinely be in the live model"
    );

    assert_eq!(
        harness.state().scrub_position,
        position,
        "TIME-06: the position must not have moved"
    );
    assert_eq!(
        harness.state().focus,
        Some(focus),
        "a live event must not clear a selection the user made against the \
         historical graph -- only their own navigation may do that (D-01)"
    );
    assert_eq!(
        harness.state().detail,
        Some(detail),
        "and it must not recompute that selection's verdict against the LIVE \
         model, which would put a live verdict beside a historical canvas"
    );
}

/// A panel harness that DOES drain, because these tests drive the real socket
/// and the climbing count is the whole point (D-03). Task 1's panel harness
/// deliberately does not drain; this one must.
fn timeline_panel_live_harness(fixture: &str) -> Harness<'static, SeamExplorerApp> {
    Harness::new_ui_state(
        |ui, app: &mut SeamExplorerApp| {
            history::drain_and_apply(app);
            timeline_panel::show(ui, app);
        },
        loaded_app(fixture),
    )
}

/// `send_scripted`'s harness counterpart, chunked under the channel capacity the
/// same way, but stepping the harness (which is what drains) instead of calling
/// `drain_and_apply` directly. Takes a RANGE so a second burst continues the
/// same id-to-`seq` script rather than restarting it.
fn send_scripted_to_harness(
    path: &Path,
    harness: &mut Harness<'static, SeamExplorerApp>,
    range: std::ops::Range<usize>,
) {
    let mut sent = range.start;
    while sent < range.end {
        let end = (sent + CHUNK).min(range.end);
        let batch: Vec<GraphEvent> = (sent..end).map(scripted).collect();
        send_and_wait(path, &batch);
        harness.run_steps(2);
        sent = end;
    }
}

/// `focus_seam`'s paused counterpart: a real seam of the DISPLAYED graph, with
/// the detail computed against that same graph -- exactly what a row click on a
/// paused list produces (09-03).
fn focus_display_seam(app: &mut SeamExplorerApp, a: &str, b: &str) {
    assert!(
        timeline::display_seams(app)
            .iter()
            .any(|s| (s.a == a && s.b == b) || (s.a == b && s.b == a)),
        "guard: {a} <-> {b} must be a real seam of the DISPLAYED graph before \
         focusing it"
    );
    let model = timeline::display_model(app).expect("something must be displayed");
    let scc = model
        .scc
        .as_ref()
        .expect("every reconstruction finalizes its own SCC");
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

/// TIME-06's core claim, both halves: new events arriving while paused move
/// neither the position nor the graph on screen.
#[test]
fn new_events_while_paused_leave_the_position_and_the_graph_pinned() {
    let _guard = SERVE_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let path = serve_at("pinned-view");
    let mut app = loaded_app(SOURCE_PATHS_FIXTURE);

    send_scripted(&path, &mut app, 4);
    timeline::apply_action(&mut app, TimelineAction::StepBack);
    timeline::apply_action(&mut app, TimelineAction::StepBack);
    assert_eq!(
        app.scrub_position,
        Some(1),
        "guard: two steps back from Live (effective seq 3) is seq 1"
    );

    let pinned_position = app.scrub_position;
    let pinned_ids = model_ids(timeline::display_model(&app).expect("a paused view displays"));
    assert_eq!(
        pinned_ids,
        fixture_plus_scripted_through(SOURCE_PATHS_FIXTURE, 1),
        "guard: the pinned view must be the fixture plus the first two scripted \
         nodes, derived from the fixture and the script"
    );

    // Five more events the user never asked to see.
    for n in 4..9 {
        send_and_wait(&path, &[scripted(n)]);
        history::drain_and_apply(&mut app);
    }
    assert_eq!(
        app.history.next_seq(),
        9,
        "guard: all five must genuinely have applied and been recorded, or \
         nothing had the chance to move the view"
    );
    assert!(
        live_ids(&app).contains(&scripted_id(8)),
        "guard: the LIVE model must have gained the newest arrival"
    );

    assert_eq!(
        app.scrub_position, pinned_position,
        "TIME-06: the scrub position must stay exactly where the user put it -- \
         no jump to latest"
    );
    assert_eq!(
        model_ids(timeline::display_model(&app).expect("a paused view still displays")),
        pinned_ids,
        "and the displayed graph must not have gained a single node the user \
         did not navigate to"
    );
}

/// D-03: the "M" keeps climbing while the user sits at position N, because
/// events really are still arriving and being recorded. A frozen M would claim
/// the stream stopped, which would contradict TIME-06's own premise.
#[test]
fn the_total_event_count_climbs_while_paused() {
    let _guard = SERVE_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let path = serve_at("climbing-m");
    let mut harness = timeline_panel_live_harness(SOURCE_PATHS_FIXTURE);

    send_scripted_to_harness(&path, &mut harness, 0..4);
    timeline::apply_action(harness.state_mut(), TimelineAction::StepBack);
    harness.run_steps(2);

    let before = timeline::status(harness.state());
    assert_eq!(
        (before.position, before.total, before.paused),
        (3, 4, true),
        "guard: paused at seq 2, which is the third of four recorded events"
    );
    let before_text = rendered_position(harness.state());
    assert_eq!(before_text, "Event 3 of 4");
    harness.get_by_label(&before_text);

    send_scripted_to_harness(&path, &mut harness, 4..9);

    let after = timeline::status(harness.state());
    assert_eq!(
        after.position, before.position,
        "N must not move -- the user is still looking at the same event"
    );
    assert_eq!(
        after.total,
        before.total + 5,
        "M must climb by exactly the five events that arrived (D-03)"
    );
    let after_text = rendered_position(harness.state());
    assert_eq!(after_text, "Event 3 of 9");
    harness.get_by_label(&after_text);
    harness.get_by_label(timeline_panel::PAUSED_BADGE);
}

/// The other side of TIME-06's coin: everything that arrived during the pause is
/// there the moment the user resumes, and the selection they made against the
/// historical graph is gone -- cleared by their own navigation (D-01), which is
/// the paused view's equivalent discipline to the live view's stale-selection
/// rule.
#[test]
fn resuming_live_after_events_arrived_while_paused_shows_the_current_graph() {
    let _guard = SERVE_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let path = serve_at("resume-after");
    let (mut harness, positions, wipe) = live_canvas_harness(SOURCE_PATHS_FIXTURE);

    send_scripted_to_harness(&path, &mut harness, 0..3);
    timeline::apply_action(harness.state_mut(), TimelineAction::JumpEarliest);
    assert_eq!(
        harness.state().scrub_position,
        Some(0),
        "guard: nothing evicted, so the oldest retained event is seq 0"
    );

    focus_display_seam(harness.state_mut(), "A", "B");
    assert!(
        harness.state().focus.is_some() && harness.state().detail.is_some(),
        "guard: a selection must genuinely exist before the resume"
    );

    send_scripted_to_harness(&path, &mut harness, 3..6);
    assert_eq!(
        harness.state().history.next_seq(),
        6,
        "guard: three more events must have arrived while paused"
    );
    assert_eq!(
        harness.state().scrub_position,
        Some(0),
        "guard: and they must have left the position alone"
    );

    timeline::apply_action(harness.state_mut(), TimelineAction::JumpLatest);
    let resumed = rendered_ids_for_one_frame(&mut harness, &positions, &wipe);

    assert_eq!(
        resumed,
        fixture_plus_scripted_through(SOURCE_PATHS_FIXTURE, 5),
        "resuming Live must show EVERY node that arrived during the pause, not \
         only those that existed when the user paused"
    );
    assert!(!timeline::is_paused(harness.state()));
    assert!(
        harness.state().focus.is_none() && harness.state().detail.is_none(),
        "D-01: the user's own navigation is what clears a historical selection, \
         and the return to Live is a navigation -- so no historical selection \
         can survive into a live view"
    );
}

/// T-09-05-03, asserted as CONTENT on BOTH halves rather than as the absence of
/// a panic.
///
/// A test that only checks "did not panic" and "something is still on screen"
/// passes against a graph whose every node is wrong, which is precisely the
/// failure mode "no silently wrong graph" claims to rule out.
///
/// Half one, the pinned view: the reconstruction is cached, so it must not move
/// one node across the evicting events.
///
/// Half two, the next navigation: `StepBack` is named specifically because from
/// a pinned position already below `earliest` it is the only action whose
/// landing position IS `evicted_count()` -- `StepForward` lands one past it, and
/// `JumpEarliest` lands there too but through a separate code path this test is
/// not naming. The expected set is derived from the fixture and the script, the
/// way 09-02's own post-wrap tests derive theirs, never read back out of the app.
///
/// The user-facing affordance for this state (telling the user their position
/// was evicted) is Phase 10 success criterion 3's; this plan owns only that it
/// is SAFE and that what it shows is TRUE.
#[test]
fn a_pinned_position_the_buffer_wrapped_past_still_renders_and_navigates_safely() {
    let _guard = SERVE_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let path = serve_at("wrapped-past");
    let mut harness = app_ui_harness(loaded_app(SOURCE_PATHS_FIXTURE));
    harness.run_steps(2);

    let cap = seam_core::LIVE_BUFFER_CAPACITY;
    let total = cap + 30;
    let dropped_before = event_stream::dropped_count();

    send_scripted_to_harness(&path, &mut harness, 0..10);
    timeline::apply_action(harness.state_mut(), TimelineAction::JumpEarliest);
    timeline::apply_action(harness.state_mut(), TimelineAction::StepForward);
    timeline::apply_action(harness.state_mut(), TimelineAction::StepForward);
    assert_eq!(
        harness.state().scrub_position,
        Some(2),
        "guard: pinned at seq 2, which the buffer is about to wrap past"
    );
    harness.run_steps(2);

    let pinned_ids = model_ids(
        timeline::display_model(harness.state()).expect("a paused view must display something"),
    );
    assert_eq!(
        pinned_ids,
        fixture_plus_scripted_through(SOURCE_PATHS_FIXTURE, 2),
        "guard: the pinned view must be the fixture plus live_000..=live_002"
    );

    // Enough events to evict the pinned position out from under the user.
    send_scripted_to_harness(&path, &mut harness, 10..total);

    assert_eq!(
        event_stream::dropped_count(),
        dropped_before,
        "guard: no datagram may be dropped, or the id-to-seq mapping every \
         assertion below depends on is shifted"
    );
    assert_eq!(
        harness.state().history.next_seq(),
        total as u64,
        "guard: one recorded event per scripted event"
    );
    let evicted = harness.state().history.evicted_count();
    assert_eq!(
        evicted, 30,
        "guard: the buffer must provably have wrapped PAST the pinned seq 2"
    );

    // --- Half one: the pinned view did not move. -----------------------
    harness.run_steps(1);
    assert_eq!(
        harness.state().scrub_position,
        Some(2),
        "the position must still be where the user put it, evicted or not"
    );
    assert_eq!(
        model_ids(
            timeline::display_model(harness.state())
                .expect("an evicted-past paused view must still display something")
        ),
        pinned_ids,
        "the cached reconstruction must not move a single node across the \
         evicting events -- and it must not silently become some other graph"
    );

    // --- Half two: the next navigation is safe AND true. ---------------
    timeline::apply_action(harness.state_mut(), TimelineAction::StepBack);
    harness.run_steps(1);

    assert_eq!(
        harness.state().scrub_position,
        Some(evicted),
        "StepBack from a position below `earliest` clamps forward and lands on \
         the oldest RETAINED event"
    );
    assert_eq!(
        model_ids(timeline::display_model(harness.state()).expect("the new position must display")),
        fixture_plus_scripted_through(SOURCE_PATHS_FIXTURE, evicted as usize),
        "and the graph it shows must be the TRUE state of that moment -- the \
         fixture plus every scripted node up to seq {evicted}, derived from the \
         script rather than read back out of the app"
    );
    harness.get_by_label(timeline_panel::PAUSED_BADGE);
}

/// The guard must be SCOPED, not a weakening. Both live rules that could fire
/// here still do, proven from this file so the scoping claim is not merely
/// inherited from `live_apply.rs` passing unedited.
#[test]
fn the_four_existing_live_stale_selection_rules_still_apply_when_not_paused() {
    let _guard = SERVE_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let path = serve_at("live-rules");
    let mut app = loaded_app(SOURCE_PATHS_FIXTURE);
    assert!(
        !timeline::is_paused(&app),
        "guard: this whole test runs Live"
    );

    // Rule 3: a SURVIVING focus keeps its focus and gets a FRESHLY recomputed
    // detail. Proven by planting a deliberately wrong detail first, so
    // "preserved" and "recomputed" are distinguishable outcomes.
    focus_seam(&mut app, "A", "B");
    let true_detail = app.detail.clone().expect("focus_seam populates the detail");
    let mut wrong = true_detail.clone();
    wrong.bridges_a = vec!["NOT_A_REAL_NODE".to_string()];
    assert_ne!(wrong, true_detail, "guard: the plant must genuinely differ");
    app.detail = Some(wrong);

    send_and_wait(
        &path,
        &[add_node(
            UNRELATED_ID,
            UNRELATED_ID,
            Some(SIBLING_SOURCE_FILE),
        )],
    );
    history::drain_and_apply(&mut app);

    assert_eq!(
        app.focus.as_ref().map(|f| (f.a.as_str(), f.b.as_str())),
        Some(("A", "B")),
        "rule 2 must still PRESERVE a focus whose seam still exists"
    );
    assert_eq!(
        app.detail,
        Some(true_detail),
        "rule 3 must still recompute the detail against the live model -- the \
         planted wrong value must have been replaced"
    );

    // Rule 2: a focus whose seam has stopped being a seam is still cleared.
    focus_seam(&mut app, "A", "C");
    send_and_wait(
        &path,
        &[GraphEvent::RemoveEdge {
            source: "a1".to_string(),
            target: "c1".to_string(),
        }],
    );
    history::drain_and_apply(&mut app);

    assert!(
        !app.seams
            .iter()
            .any(|s| (s.a == "A" && s.b == "C") || (s.a == "C" && s.b == "A")),
        "guard: A<->C must genuinely have stopped being a seam, or rule 2 had \
         nothing to fire on -- got {:?}",
        app.seams
    );
    assert!(
        app.focus.is_none() && app.detail.is_none(),
        "rule 2 must still clear a focus whose seam is gone -- the paused-view \
         guard must not have weakened the LIVE path"
    );
    assert!(!timeline::is_paused(&app), "guard: still Live throughout");
}

// ---------------------------------------------------------------------
// 09-05, added during execution (Rule 2 -- see the SUMMARY's Deviations)
// ---------------------------------------------------------------------

/// The plan named six Task 1 tests, and between them they prove the click path
/// for TWO of the four buttons (`Step back` and `Latest`). That leaves the
/// label-to-action mapping of the other two asserted nowhere, and a copy-pasted
/// `TimelineAction` in `nav_button`'s argument list -- the single most likely
/// defect in a block of four near-identical calls -- would pass every other test
/// in this file.
///
/// TIME-02's own sentence names "jump to the earliest event ... also available
/// as on-screen buttons", so marking it Complete on a button whose action is
/// unverified would be a claim this suite cannot support. Added rather than
/// waived.
///
/// Each assertion is chosen to fail against the specific plausible mis-wire:
/// `Earliest` wired to `JumpLatest` lands on `None`; `Step forward` wired to
/// `StepBack` moves the wrong way.
#[test]
fn the_earliest_and_step_forward_buttons_reach_their_own_actions() {
    let mut harness = timeline_panel_harness(app_with_recorded_history(PANEL_EVENTS));

    for _ in 0..3 {
        timeline::apply_action(harness.state_mut(), TimelineAction::StepBack);
    }
    harness.run();
    assert_eq!(
        harness.state().scrub_position,
        Some(2),
        "guard: three steps back from Live (effective seq 5) is seq 2 -- mid \
         range, so both buttons below have somewhere to move in each direction"
    );

    harness
        .get_by_label(timeline_panel::STEP_FORWARD_LABEL)
        .click();
    harness.run();
    assert_eq!(
        harness.state().scrub_position,
        Some(3),
        "the Step forward button must step FORWARD by exactly one -- a button \
         wired to StepBack would land on seq 1"
    );

    harness.get_by_label(timeline_panel::EARLIEST_LABEL).click();
    harness.run();
    assert_eq!(
        harness.state().scrub_position,
        Some(harness.state().history.evicted_count()),
        "the Earliest button must target the oldest RETAINED event (D-05)"
    );
    assert_eq!(
        harness.state().scrub_position,
        Some(0),
        "guard: nothing was evicted here, so that is seq 0 -- and a button \
         mis-wired to JumpLatest would have landed on None instead"
    );
    assert!(
        timeline::is_paused(harness.state()),
        "jumping to earliest must leave the app Paused, not resume Live"
    );
}
