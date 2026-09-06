//! `history.rs`: the per-frame live-event apply pipeline (Phase 8, plan
//! 08-01).
//!
//! One call, one frame, no deferral:
//! the `event_stream` drain -> `seam_core::apply_batch` ->
//! `seam_core::detect` -> `app.seams`. Everything a panel reads later in the
//! same frame has
//! already caught up by the time [`drain_and_apply`] returns -- that is
//! ROADMAP SC-1's "not a stale list beside a changed canvas", and it is why
//! `app.rs::ui()` calls this ABOVE its panel dispatch rather than from inside
//! `graph_view::show` (which runs after both side panels).
//!
//! Plan 08-03 adds the rotating event-history buffer (EVENT-04) to this same
//! module -- the file is named for that eventual owner. This plan puts only
//! the apply pipeline in it.

use crate::app::SeamExplorerApp;

/// What one [`drain_and_apply`] call did, for callers that want to react to
/// it. Plan 08-03 extends this with the assigned history sequence ids.
#[derive(Debug, Default, PartialEq)]
pub struct ApplySummary {
    pub applied_count: usize,
    /// The real ids of nodes this batch removed, carried as data so removal
    /// consequences never need a second scan of the model.
    pub removed_node_ids: Vec<String>,
}

/// Drain every queued live event and apply it to `app.model`, recomputing
/// whatever the mutation invalidated before returning.
///
/// Runs EVERY frame, so the idle path matters: `drain()` is called first and
/// unconditionally, and an empty batch returns immediately having touched
/// nothing but one channel poll (T-08-01-04).
///
/// `drain()` comes BEFORE the `app.model` check on purpose -- an event
/// arriving before the user has loaded a graph must still be absorbed rather
/// than backing up in the bounded channel. That is the property the
/// superseded `graph_view.rs` call site's comment protected, preserved here.
pub fn drain_and_apply(app: &mut SeamExplorerApp) -> ApplySummary {
    let events = crate::event_stream::drain();
    if events.is_empty() {
        return ApplySummary::default();
    }

    // The model is mutated IN PLACE through `app.model.as_mut()`. Never
    // clone it to dodge a borrow: a mutation applied to a throwaway copy is
    // an app that silently ignores every live event, with no error anywhere
    // to say so (08-RESEARCH.md Pitfall 4).
    let outcome = match app.model.as_mut() {
        Some(model) => seam_core::apply_batch(model, &events),
        // No graph loaded: the events were still drained above, so nothing
        // is stranded in the channel. There is simply nothing to apply them
        // to.
        None => return ApplySummary::default(),
    };

    if outcome.topology_changed {
        // Disjoint field borrows: `app.model` is read while `app.seams` is
        // written. This is why `drain_and_apply` takes `&mut SeamExplorerApp`
        // and reaches through direct field paths instead of going through a
        // helper that hands out a borrow of the whole struct -- such a helper
        // would make the later tasks' clearing logic uncompilable.
        if let Some(model) = app.model.as_ref() {
            app.seams = seam_core::detect(model);
        }
    }

    ApplySummary {
        applied_count: outcome.applied.len(),
        removed_node_ids: outcome.removed_node_ids,
    }
}
