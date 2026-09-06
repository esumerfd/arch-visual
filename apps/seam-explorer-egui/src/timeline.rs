//! `timeline.rs`: time-travel navigation (Phase 9, plan 09-02).
//!
//! Four things live here and nothing else: the pure arithmetic that turns a
//! navigation action into a target identity ([`next_position`]), the ONE
//! navigation entry point that acts on it ([`apply_action`]), the replay that
//! reconstructs a historical moment, and the accessor pair every display site
//! asks "which model am I showing" through ([`display_model`]/
//! [`display_seams`]).
//!
//! **This module decides and computes; it draws nothing.** No `egui` widget
//! code, by construction -- plan 09-05 owns the timeline panel, 09-04 owns the
//! keyboard bindings, and both call [`apply_action`] rather than reimplementing
//! any part of it. That is what makes ROADMAP SC-1's "either route produces the
//! same single step" structural rather than coincidental.
//!
//! **The reconstruction never lands in `app.model`.** It is kept in
//! `app.scrub_model` and read only through the accessors.
//! `graph_view::inject_layout_targets` prunes persisted node positions against
//! `app.model`'s id set every frame; a historical model has fewer nodes, so a
//! single frame rendered with a reconstruction sitting in `app.model`
//! permanently deletes the settled position of every node the live graph gained
//! after the paused point (T-09-02-02, 09-RESEARCH.md Pitfall 2). Keeping the
//! reconstruction in its own field makes that pruning immune by construction
//! rather than by a runtime guard someone can later delete.

use crate::app::SeamExplorerApp;
use crate::history::{History, SequenceId};

/// The closed set of navigation actions, as one enum. The keyboard scheme
/// (09-04) and the on-screen buttons (09-05) both map onto exactly these four.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimelineAction {
    StepBack,
    StepForward,
    JumpEarliest,
    JumpLatest,
}

/// What the Live/Paused indicator needs (TIME-04, D-03/D-04), as data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimelineStatus {
    /// One-based display number for the currently displayed event. Equal to
    /// [`Self::total`] while Live.
    pub position: u64,
    /// Every event ever recorded on this timeline -- D-03's "M", which keeps
    /// climbing while the user is paused.
    pub total: u64,
    pub paused: bool,
}

/// `(earliest, latest)` — the oldest still-retained identity and the newest
/// recorded one — or nothing when the history is empty.
///
/// Both bounds come from O(1) counters. Never scan [`History::iter`] for a
/// min/max: that would reintroduce exactly the positional thinking
/// `history.rs`'s own doc comment refuses, and it would be slower for no gain.
pub fn bounds(history: &History) -> Option<(SequenceId, SequenceId)> {
    if history.is_empty() {
        return None;
    }
    Some((
        history.evicted_count(),
        history.next_seq().saturating_sub(1),
    ))
}

/// PURE navigation arithmetic: current position plus bounds plus an action
/// gives the next position. No `app`, no `egui`, no `Model`.
///
/// This is where TIME-01 and TIME-02 are actually decided, and being pure is
/// what makes them unit-testable without a window -- the same discipline
/// `keyboard::apply_key` established.
///
/// Each rule traces to a locked decision:
///
/// - Nothing recorded: every action stays Live. There is no position to pause
///   on.
/// - Live's effective position is the newest recorded event, and the current
///   position is CLAMPED into range first. The clamp is load-bearing, not
///   defensive noise: while paused the buffer can wrap past the pinned
///   position, leaving `current` below `earliest`, and clamping first is what
///   makes the next keypress land at the oldest retained event instead of
///   computing a target outside the retained range.
/// - `StepForward` returns `Some` even at `latest`: stepping forward at the
///   newest event is a no-op that STAYS Paused and must not flip to Live,
///   because D-02 names jump-to-latest as the ONLY resume route.
/// - `JumpEarliest` targets the oldest still-retained event (D-05), never a
///   position with nothing replayed. That target lands on the TRUE state of
///   that moment by construction: `app.replay_baseline` is defined as the state
///   immediately before `earliest`, so reconstructing there replays exactly one
///   event onto exactly the right starting point, wrapped or not.
/// - `JumpLatest` returns `None`, literally resuming Live (D-02) -- not
///   "reconstruct up to the newest event and stay Paused". With `None` the
///   display accessors fall through to the ever-current live fields at zero
///   reconstruction cost, which is exactly today's shipped behavior.
pub fn next_position(
    current: Option<SequenceId>,
    bounds: Option<(SequenceId, SequenceId)>,
    action: TimelineAction,
) -> Option<SequenceId> {
    let (earliest, latest) = bounds?;
    let cur = current.unwrap_or(latest).clamp(earliest, latest);
    match action {
        // Saturating, never a bare subtraction -- `cur` can be zero.
        TimelineAction::StepBack => Some(cur.saturating_sub(1).max(earliest)),
        TimelineAction::StepForward => Some(cur.saturating_add(1).min(latest)),
        TimelineAction::JumpEarliest => Some(earliest),
        TimelineAction::JumpLatest => None,
    }
}

/// The ONE navigation entry point (09-04's keyboard and 09-05's buttons both
/// call exactly this).
///
/// Navigation is a READ-ONLY re-render (09-CONTEXT.md's terminology note): no
/// event, no history entry, no part of Phase 8's stored record is removed or
/// mutated here, and `app.model` is never written.
///
/// The early return on an unchanged position is D-01's scope, not an
/// optimisation: pressing jump-to-latest while already Live, or stepping
/// forward while already at the newest event, must not destroy a selection the
/// user made, because nothing about what is displayed changed.
///
/// The reconstruction is computed HERE, once per navigation, and stored -- never
/// from a per-frame path. The obvious mental model is wrong: the live drain must
/// run every frame because events arrive at any time, but a paused position by
/// construction only changes in response to an explicit navigation, so a
/// per-frame replay would pay the full cost sixty times a second while the user
/// sits still.
pub fn apply_action(app: &mut SeamExplorerApp, action: TimelineAction) {
    let next = next_position(app.scrub_position, bounds(&app.history), action);
    if next == app.scrub_position {
        return;
    }
    app.scrub_position = next;
    clear_view_selection(app);
    match next {
        Some(seq) => {
            let (model, seams) = reconstruct(app, seq);
            app.scrub_model = Some(model);
            app.scrub_seams = seams;
        }
        None => {
            app.scrub_model = None;
            app.scrub_seams = Vec::new();
        }
    }
}

/// D-01: navigating to a different point in history clears the transient
/// trace/seam-focus/detail selection, unconditionally.
///
/// **Deliberately NOT `history.rs`'s stale-selection clean-up, and not an
/// imitation of it.** Read that function and three reasons it is the wrong tool
/// here are visible: it clears a trace only when the trace is BROKEN, it
/// PRESERVES a focus whose seam still exists, and its third rule recomputes
/// `app.detail` against `app.model` -- the LIVE model, which is precisely the
/// value that must not be shown beside a historical canvas.
///
/// The two rules are different because the situations are: a mutation makes a
/// selection possibly-invalid, and the clean-up asks which parts actually went
/// stale. A navigation makes the selection's equivalence to what is on screen
/// undefined by construction, so there is no part of it worth keeping.
fn clear_view_selection(app: &mut SeamExplorerApp) {
    app.trace = None;
    app.focus = None;
    app.detail = None;
}

/// Replay the recorded events up to and including `target` onto a fresh clone
/// of the replay baseline, then rank the result.
///
/// **Why replaying only the RETAINED entries is complete.** It looks like it
/// cannot be -- the buffer holds at most `LIVE_BUFFER_CAPACITY` entries and
/// drops the rest. It is complete because `history::record` defines
/// `app.replay_baseline` as the state immediately before the oldest retained
/// entry and keeps that true by folding each evicted event into it as it
/// leaves, at this same one-event-per-call granularity. Baseline-plus-retained
/// is therefore exactly "every event ever recorded, replayed one per call".
///
/// Do NOT add a fallback that tries to reach evicted events here: they are gone
/// from the buffer by design (`history.rs`'s `evicted_count` doc), and their
/// effect is already in the baseline. If this function ever appears to need
/// them, the fold in `history::record` has broken and that is the thing to fix.
///
/// Two more things about the shape below are decisions, not incidentals:
///
/// 1. It reaches the recorded events only through [`History::iter`]'s
///    guaranteed oldest-first order, never through the buffer's by-identity
///    lookup. That keeps Phase 8's deferred concern about that method's
///    positional-arithmetic alternative genuinely inert -- this phase's
///    mechanism does not exercise the path at all, which is a stronger position
///    than "still fine to defer".
/// 2. One event per `seam_core::apply_batch` call, not one call with the whole
///    slice. That function runs the promotion sweep once per CALL, capped at
///    five passes. One call over a hundred events gives the sweep a single
///    five-pass budget for the entire replay; a call per event gives it up to a
///    hundred separate budgets -- strictly more chances to converge, never
///    fewer. And the recorded entries carry no batch-grouping information at
///    all, so exact-batch-shaped replay is not even available as an option.
///
/// `finalize_scc` runs unconditionally after the replay rather than being
/// inherited from the baseline: `model.rs` recomputes wholesale, so no stale
/// cache can survive into anything displayed.
///
/// **The `replay_baseline == None` contract.** That is the pre-load state -- no
/// graph has ever gone through `apply_load_outcome`. It is a real reachable
/// state for any test harness that builds a `SeamExplorerApp` field-by-field
/// (09-04's keyboard-scrub setup helper is exactly this shape), so this returns
/// an empty model rather than panicking.
fn reconstruct(
    app: &SeamExplorerApp,
    target: SequenceId,
) -> (seam_core::Model, Vec<seam_core::Seam>) {
    let Some(baseline) = app.replay_baseline.as_ref() else {
        return (seam_core::Model::default(), Vec::new());
    };
    let mut model = baseline.clone();
    for entry in app.history.iter() {
        if entry.seq > target {
            break;
        }
        seam_core::apply_batch(&mut model, std::slice::from_ref(&entry.event));
    }
    model.finalize_scc();
    let seams = seam_core::detect(&model);
    (model, seams)
}

/// The model every display site renders: the reconstruction while paused, the
/// live model otherwise. ONE decision point, not eleven copy-pasted
/// conditionals across the panels.
pub fn display_model(app: &SeamExplorerApp) -> Option<&seam_core::Model> {
    if app.scrub_position.is_some() {
        app.scrub_model.as_ref()
    } else {
        app.model.as_ref()
    }
}

/// [`display_model`]'s seam-list mirror.
pub fn display_seams(app: &SeamExplorerApp) -> &[seam_core::Seam] {
    if app.scrub_position.is_some() {
        &app.scrub_seams
    } else {
        &app.seams
    }
}

pub fn is_paused(app: &SeamExplorerApp) -> bool {
    app.scrub_position.is_some()
}

/// "Event N of M" as data (TIME-04). `total` keeps climbing while paused
/// (D-03) -- a frozen M would misrepresent what is actually happening in the
/// background.
pub fn status(app: &SeamExplorerApp) -> TimelineStatus {
    let total = app.history.next_seq();
    TimelineStatus {
        position: app.scrub_position.map(|seq| seq + 1).unwrap_or(total),
        total,
        paused: is_paused(app),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A history holding `total` pushed events, so its counters describe a real
    /// buffer occupancy rather than hand-set numbers.
    fn filled(total: usize) -> History {
        let mut history = History::default();
        for n in 0..total {
            history.push(seam_core::GraphEvent::AddNode {
                id: format!("n{n}"),
                label: format!("n{n}"),
                community: Some("A".to_string()),
                source_file: Some("src/live.rs".to_string()),
            });
        }
        history
    }

    #[test]
    fn bounds_of_an_empty_history_are_none() {
        assert_eq!(bounds(&History::default()), None);
    }

    #[test]
    fn bounds_come_from_the_counters_not_from_a_scan() {
        // Under the cap: nothing evicted, so earliest is 0.
        assert_eq!(bounds(&filled(10)), Some((0, 9)));

        // Wrapped: earliest is the evicted count, latest is next_seq - 1.
        let cap = seam_core::LIVE_BUFFER_CAPACITY;
        let history = filled(cap + 25);
        assert_eq!(
            bounds(&history),
            Some((25, (cap + 25 - 1) as SequenceId)),
            "after a wrap the oldest retained identity is the evicted count"
        );
        assert_eq!(
            history.evicted_count(),
            25,
            "guard: the buffer must genuinely have wrapped"
        );
    }

    #[test]
    fn every_action_on_an_empty_history_stays_live() {
        for action in [
            TimelineAction::StepBack,
            TimelineAction::StepForward,
            TimelineAction::JumpEarliest,
            TimelineAction::JumpLatest,
        ] {
            assert_eq!(
                next_position(None, None, action),
                None,
                "with nothing recorded there is no position to pause on: {action:?}"
            );
            assert_eq!(
                next_position(Some(3), None, action),
                None,
                "even from a stale position, an empty history means Live: {action:?}"
            );
        }
    }

    #[test]
    fn every_action_from_live_uses_the_newest_event_as_its_starting_point() {
        let b = Some((0, 9));
        assert_eq!(
            next_position(None, b, TimelineAction::StepBack),
            Some(8),
            "stepping back from Live lands one before the newest recorded event"
        );
        assert_eq!(
            next_position(None, b, TimelineAction::StepForward),
            Some(9),
            "there is nothing newer than the newest, and this must not resume Live"
        );
        assert_eq!(
            next_position(None, b, TimelineAction::JumpEarliest),
            Some(0)
        );
        assert_eq!(next_position(None, b, TimelineAction::JumpLatest), None);
    }

    #[test]
    fn stepping_from_mid_range_moves_exactly_one_in_each_direction() {
        let b = Some((0, 9));
        assert_eq!(next_position(Some(4), b, TimelineAction::StepBack), Some(3));
        assert_eq!(
            next_position(Some(4), b, TimelineAction::StepForward),
            Some(5)
        );
    }

    /// D-02: jump-to-latest is the ONLY route back to Live. Stepping forward at
    /// the newest event is a no-op that STAYS Paused.
    #[test]
    fn step_forward_at_the_newest_event_stays_paused() {
        let b = Some((0, 9));
        assert_eq!(
            next_position(Some(9), b, TimelineAction::StepForward),
            Some(9),
            "must return Some(latest), never None -- None would silently resume Live"
        );

        // Same claim after a wrap, where `latest` is not a small number.
        let wrapped = Some((25, 124));
        assert_eq!(
            next_position(Some(124), wrapped, TimelineAction::StepForward),
            Some(124)
        );
    }

    /// D-05: "earliest" is the oldest still-retained event, never the baseline
    /// and never `0` once the buffer has wrapped.
    #[test]
    fn jump_earliest_targets_the_oldest_retained_event_not_the_baseline() {
        let wrapped = Some((25, 124));
        assert_eq!(
            next_position(None, wrapped, TimelineAction::JumpEarliest),
            Some(25),
            "with 25 events evicted, the oldest RETAINED identity is 25, not 0"
        );
        assert_eq!(
            next_position(Some(80), wrapped, TimelineAction::JumpEarliest),
            Some(25)
        );
        assert_ne!(
            next_position(None, wrapped, TimelineAction::JumpEarliest),
            Some(0),
            "targeting 0 would name an evicted identity with nothing to replay"
        );
    }

    #[test]
    fn step_back_at_the_oldest_retained_event_holds() {
        // At zero: the saturating subtraction is what keeps this from panicking.
        assert_eq!(
            next_position(Some(0), Some((0, 9)), TimelineAction::StepBack),
            Some(0)
        );
        // After a wrap: it must hold at `earliest`, not walk into evicted range.
        assert_eq!(
            next_position(Some(25), Some((25, 124)), TimelineAction::StepBack),
            Some(25)
        );
    }

    /// While paused, the buffer keeps wrapping past the pinned position. The
    /// clamp is what makes the next keypress land at the oldest retained event
    /// rather than computing a target outside the retained range.
    #[test]
    fn a_position_the_buffer_wrapped_past_clamps_forward_to_the_oldest_retained() {
        let wrapped = Some((25, 124));
        assert_eq!(
            next_position(Some(3), wrapped, TimelineAction::StepBack),
            Some(25),
            "a position the buffer wrapped past clamps forward to `earliest` first"
        );
        assert_eq!(
            next_position(Some(3), wrapped, TimelineAction::StepForward),
            Some(26),
            "stepping forward from a wrapped-past position resumes from `earliest`"
        );
        assert_eq!(
            next_position(Some(3), wrapped, TimelineAction::JumpEarliest),
            Some(25)
        );
        assert_eq!(
            next_position(Some(3), wrapped, TimelineAction::JumpLatest),
            None
        );

        // The symmetric case: a position somehow ABOVE `latest` clamps back.
        assert_eq!(
            next_position(Some(999), wrapped, TimelineAction::StepForward),
            Some(124)
        );
    }

    #[test]
    fn jump_latest_from_a_paused_position_returns_to_live() {
        assert_eq!(
            next_position(Some(4), Some((0, 9)), TimelineAction::JumpLatest),
            None,
            "D-02: this is literally resuming Live, not pausing at the newest event"
        );
    }

    /// A real reachable state: any harness that builds a `SeamExplorerApp`
    /// field-by-field never goes through `apply_load_outcome`, so it has no
    /// baseline. 09-04's scrub tests are exactly that shape and must not panic
    /// on their first navigation.
    #[test]
    fn reconstruct_with_no_baseline_returns_an_empty_model_rather_than_panicking() {
        let app = SeamExplorerApp::default();
        assert!(
            app.replay_baseline.is_none(),
            "guard: a default-built app must genuinely have no baseline"
        );

        let (model, seams) = reconstruct(&app, 0);
        assert_eq!(model.graph.node_count(), 0);
        assert!(model.index.is_empty());
        assert!(seams.is_empty());

        // And through the public entry point, which is how 09-04 reaches it.
        let mut app = SeamExplorerApp::default();
        apply_action(&mut app, TimelineAction::StepBack);
        assert_eq!(
            app.scrub_position, None,
            "with no history there is nothing to pause on"
        );
    }

    #[test]
    fn status_reports_a_one_based_position_and_a_total_that_keeps_climbing() {
        let mut app = SeamExplorerApp {
            history: filled(10),
            ..Default::default()
        };

        assert_eq!(
            status(&app),
            TimelineStatus {
                position: 10,
                total: 10,
                paused: false
            },
            "while Live the position equals the total"
        );

        app.scrub_position = Some(3);
        assert_eq!(
            status(&app),
            TimelineStatus {
                position: 4,
                total: 10,
                paused: true
            },
            "seq 3 is the fourth event, displayed one-based"
        );

        // D-03: M keeps climbing while paused.
        app.history = filled(17);
        app.scrub_position = Some(3);
        assert_eq!(status(&app).total, 17);
        assert_eq!(status(&app).position, 4);
    }
}
