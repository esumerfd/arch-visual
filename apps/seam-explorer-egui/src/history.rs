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
        Some(model) => {
            let outcome = seam_core::apply_batch(model, &events);
            if outcome.topology_changed {
                // Unconditional on ANY topology change -- deliberately NOT
                // gated on some narrower "was this SCC-relevant" test. The
                // whole-graph Tarjan pass is O(V+E) and already established
                // as affordable at this project's real graph sizes, while
                // every narrower gate is a fresh opportunity for the cache
                // and the model to disagree. The concrete consequence of
                // getting that wrong is not a stale number: the seam list
                // scores every visible row eagerly through the cached index
                // (`panels::seam_list::seam_verdict` -> `seam_detail` ->
                // `has_cross_cycle`, which indexes the cache raw), so a
                // single missed recomputation is a crash on the next
                // rendered frame.
                model.finalize_scc();
            }
            outcome
        }
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
        // would make `clear_stale_selection` below uncompilable.
        if let Some(model) = app.model.as_ref() {
            app.seams = seam_core::detect(model);
        }
        // Strictly after the model is mutated, the SCC cache recomputed, and
        // `app.seams` refreshed -- rule 2 below reads that fresh seam list.
        clear_stale_selection(app);
    }

    ApplySummary {
        applied_count: outcome.applied.len(),
        removed_node_ids: outcome.removed_node_ids,
    }
}

/// D-03's "never show a lie" clean-up: after a batch that changed the graph,
/// nothing left on screen may reference data that is gone.
///
/// Reaches through direct field paths on `&mut SeamExplorerApp` throughout.
/// The `app.model` borrow and the `app.trace`/`app.focus`/`app.detail` writes
/// are disjoint fields and coexist fine -- but only because no intermediate
/// helper hands out a borrow of the whole struct.
fn clear_stale_selection(app: &mut SeamExplorerApp) {
    let Some(model) = app.model.as_ref() else {
        return;
    };

    // Rule 1: clear a trace whose hops no longer all exist, or whose
    // consecutive hops no longer connect. A path drawn between nodes that no
    // longer connect is exactly the kind of lie this project's negative-case
    // test discipline has consistently refused to ship.
    //
    // Compared through the node-id strings the trace already stores, never
    // translated through graph indices -- indices move under mutation, ids
    // do not.
    let trace_broken = app
        .trace
        .as_ref()
        .and_then(|trace| trace.path.as_ref())
        .is_some_and(|path| {
            path.hops.iter().any(|hop| !model.index.contains_key(hop))
                || path.hops.windows(2).any(|pair| {
                    match (model.index.get(&pair[0]), model.index.get(&pair[1])) {
                        (Some(&from), Some(&to)) => model.graph.find_edge(from, to).is_none(),
                        _ => true,
                    }
                })
        });
    if trace_broken {
        app.trace = None;
    }

    let Some((a, b)) = app.focus.as_ref().map(|f| (f.a.clone(), f.b.clone())) else {
        return;
    };

    // Rule 2: a focused seam with no crossing edges left has stopped being a
    // seam. `seam_core::detect` only emits pairs with at least one crossing,
    // so absence from the freshly recomputed `app.seams` IS "no longer a
    // seam". Matched unordered, even though `detect` normalises `a < b`.
    let still_a_seam = app
        .seams
        .iter()
        .any(|s| (s.a == a && s.b == b) || (s.a == b && s.b == a));
    if !still_a_seam {
        app.focus = None;
        app.detail = None;
        return;
    }

    // Rule 3: a surviving focus gets a FRESHLY recomputed detail rather than
    // keeping the previous value. Deliberately stronger than D-03's literal
    // "cleared" wording, for two concrete reasons: SC-1 requires verdicts to
    // be recomputed to match, and `graph_view::apply_focus_styling` paints
    // bridge highlights every frame from `app.detail`'s node-id lists -- a
    // stale list paints the wrong nodes as bridges. Snapping the detail panel
    // shut on every unrelated event would be the wrong reading of D-03.
    if let Some(scc) = model.scc.as_ref() {
        app.detail = Some(seam_core::seam_detail(model, scc, &a, &b));
    }
}
