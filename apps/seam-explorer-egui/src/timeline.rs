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
        let mut app = SeamExplorerApp::default();
        app.history = filled(10);

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
