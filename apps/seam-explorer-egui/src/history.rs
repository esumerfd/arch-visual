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

#[cfg(test)]
mod tests {
    use super::*;
    use seam_core::GraphEvent;

    /// A distinct, CONTENT-identifiable event for the `n`th push.
    ///
    /// Every assertion below that says "this is the event I expect here"
    /// compares against `ev(n)`, never against a buffer position. That is not
    /// stylistic: an implementation that located entries by position
    /// arithmetic would satisfy a position-based assertion trivially and
    /// still be exactly the thing EVENT-04 forbids.
    fn ev(n: usize) -> GraphEvent {
        GraphEvent::AddNode {
            id: format!("src/live.rs::sym{n}"),
            label: format!("sym{n}"),
            community: Some("A".to_string()),
            source_file: Some("src/live.rs".to_string()),
        }
    }

    fn filled(count: usize) -> History {
        let mut history = History::default();
        for n in 0..count {
            history.push(ev(n));
        }
        history
    }

    /// D-01: the buffer holds exactly its capacity; the next arrival evicts
    /// the oldest rather than growing.
    #[test]
    fn the_hundred_and_first_event_evicts_the_first() {
        let cap = seam_core::LIVE_BUFFER_CAPACITY;
        let mut history = History::default();
        assert_eq!(
            history.capacity(),
            cap,
            "the cap must come from the one shared constant (D-05a), never a local literal"
        );

        for n in 0..cap {
            history.push(ev(n));
        }
        assert_eq!(
            history.len(),
            cap,
            "the buffer must fill to exactly its cap"
        );
        assert_eq!(
            history.evicted_count(),
            0,
            "filling to the cap must evict nothing"
        );

        history.push(ev(cap));
        assert_eq!(
            history.len(),
            cap,
            "the 101st arrival must not grow the buffer"
        );
        assert_eq!(
            history.evicted_count(),
            1,
            "the 101st arrival must evict exactly one entry"
        );
        let oldest = history
            .iter()
            .next()
            .expect("a full buffer must have an oldest entry");
        assert_eq!(
            oldest.event,
            ev(1),
            "the oldest survivor must be the SECOND event pushed, identified by its own content"
        );
    }

    /// EVENT-04's core claim: an identity is assigned once and never reused,
    /// including across wraparound.
    #[test]
    fn a_sequence_identity_is_never_reused_across_a_wraparound() {
        let total = seam_core::LIVE_BUFFER_CAPACITY * 5 / 2; // two and a half wraps
        let mut history = History::default();
        let mut issued: Vec<SequenceId> = Vec::with_capacity(total);
        for n in 0..total {
            issued.push(history.push(ev(n)));
        }

        let unique: std::collections::HashSet<SequenceId> = issued.iter().copied().collect();
        assert_eq!(
            unique.len(),
            issued.len(),
            "no identity may ever be handed out twice"
        );
        assert!(
            issued.windows(2).all(|pair| pair[1] > pair[0]),
            "identities must be strictly increasing -- never reset, never repeated"
        );
        assert_eq!(
            *issued.last().expect("guard: some identity was issued"),
            (total - 1) as SequenceId,
            "the last identity must be total-minus-one; a counter that silently \
             restarted at each wrap could not reach this value"
        );
    }

    /// The subtlest failure this plan closes (T-08-03-04): a position-
    /// arithmetic lookup returns a plausible WRONG entry after a wrap rather
    /// than reporting the entry as gone.
    #[test]
    fn an_evicted_identity_reports_as_gone_rather_than_resolving_to_the_wrong_event() {
        let total = seam_core::LIVE_BUFFER_CAPACITY * 5 / 2;
        let history = filled(total);
        let evicted = history.evicted_count();
        assert!(evicted > 0, "guard: the buffer must actually have wrapped");

        for seq in 0..evicted {
            assert!(
                history.get(seq).is_none(),
                "evicted identity {seq} must report as gone, not resolve to a different event"
            );
        }
        for entry in history.iter() {
            let found = history
                .get(entry.seq)
                .expect("a surviving identity must resolve");
            assert_eq!(
                found.seq, entry.seq,
                "a lookup must return the entry whose identity was ASKED FOR"
            );
            assert_eq!(
                found.event,
                ev(entry.seq as usize),
                "the resolved entry must carry the content pushed under that identity, \
                 checked against the script rather than against the buffer itself"
            );
        }
        assert!(
            history.get(history.next_seq()).is_none(),
            "an identity that was never issued must resolve to nothing"
        );
    }

    /// ROADMAP SC-4's "evicted events still accounted for", as one equation.
    #[test]
    fn the_accounting_closes() {
        let cap = seam_core::LIVE_BUFFER_CAPACITY;
        for total in [0usize, 1, 7, cap - 1, cap, cap + 1, cap * 3 + 13] {
            let history = filled(total);
            assert_eq!(
                history.evicted_count() as usize + history.len(),
                total,
                "evicted plus retained must equal total ever pushed (total={total})"
            );
            assert_eq!(
                history.next_seq(),
                total as SequenceId,
                "the next identity to issue must equal total ever pushed (total={total})"
            );
        }
    }

    /// The order Phase 9's replay depends on, pinned now rather than
    /// discovered later.
    #[test]
    fn iteration_yields_entries_oldest_first() {
        let cap = seam_core::LIVE_BUFFER_CAPACITY;
        let total = cap + 17;
        let history = filled(total);

        let seen: Vec<SequenceId> = history.iter().map(|entry| entry.seq).collect();
        let expected: Vec<SequenceId> =
            ((total - cap) as SequenceId..total as SequenceId).collect();
        assert_eq!(
            seen, expected,
            "iteration must yield the surviving window oldest-first"
        );
        let first = history.iter().next().expect("guard: buffer is non-empty");
        assert_eq!(
            first.event,
            ev(total - cap),
            "the first yielded entry must be the oldest SURVIVOR, by content"
        );
    }

    /// The counter resets on clear, and this test is the place that says so.
    /// A cleared history belongs to a DIFFERENT graph; its old identities
    /// describe events that were never applied to the new one, so carrying
    /// the counter forward would let a Phase 9 reader mistake a stale
    /// identity for a live one.
    #[test]
    fn clearing_resets_the_contents_but_states_what_it_does_to_the_counter() {
        let mut history = filled(seam_core::LIVE_BUFFER_CAPACITY + 5);
        assert!(!history.is_empty(), "guard: the history must be non-empty");
        assert!(history.evicted_count() > 0, "guard: it must have wrapped");

        history.clear();

        assert!(history.is_empty(), "clear must empty the buffer");
        assert_eq!(history.len(), 0, "clear must empty the buffer");
        assert_eq!(
            history.evicted_count(),
            0,
            "a cleared history has evicted nothing -- the old graph's losses are not the new one's"
        );
        assert_eq!(
            history.next_seq(),
            0,
            "the counter resets too: a new graph starts a new timeline at zero"
        );
        assert_eq!(
            history.push(ev(0)),
            0,
            "the first identity issued after a clear must start the new timeline at zero"
        );
    }
}
