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
//! Plan 08-03 added the rotating event-history buffer (EVENT-04) to this same
//! module -- [`History`], [`HistoryEntry`], [`SequenceId`] below. It is the
//! substrate Phase 9's time-travel scrub reads from; Phase 9 itself (scrub
//! position, Live/Paused mode, replay, the timeline panel, the keyboard
//! scheme) is deliberately NOT built here.

use crate::app::SeamExplorerApp;

/// The identity handed out for one recorded event.
///
/// A `u64` counter, never a position. At even one event per millisecond
/// sustained, exhausting this takes longer than the heat death of anything
/// that could plausibly be described as an editing session -- so the "what
/// happens on overflow" question that a narrower integer would raise simply
/// does not arise, and no wrapping logic is needed or wanted.
pub type SequenceId = u64;

/// One recorded event and the identity assigned to it.
///
/// `event` is a FULLY RESOLVED event taken from
/// [`seam_core::ApplyOutcome::applied`] -- an `AddNode` here always carries a
/// concrete community and the reconciled real node id, never the raw wire
/// advertisement. That distinction is the whole reason `ApplyOutcome` carries
/// `applied` separately from the drained batch: what history records must be
/// what the graph actually did, or Phase 9 replays a graph the user never saw.
#[derive(Debug, Clone, PartialEq)]
pub struct HistoryEntry {
    pub seq: SequenceId,
    pub event: seam_core::GraphEvent,
}

/// The bounded rotating event history (EVENT-04, D-01).
///
/// Holds at most [`seam_core::LIVE_BUFFER_CAPACITY`] entries; past that, the
/// oldest is evicted on each arrival. The identity of an entry is assigned
/// from a monotonic counter that is entirely independent of the container:
/// it is issued BEFORE the eviction that may accompany the same push, so
/// nothing about the buffer's occupancy can influence it. An identity handed
/// out once therefore stays meaningful forever -- after wraparound it simply
/// stops resolving (correctly: it was evicted, and [`Self::evicted_count`]
/// accounts for it) rather than quietly resolving to some other event.
///
/// `Default` produces an empty history with the counter at zero. Note there
/// is deliberately no `capacity` FIELD: a derived `Default` would set it to
/// zero, producing a history that evicts every entry it is given. The cap is
/// read from the shared constant on each use instead.
#[derive(Debug, Default)]
pub struct History {
    entries: std::collections::VecDeque<HistoryEntry>,
    /// The next identity to issue. Never decreases while a timeline lives,
    /// and never repeats within one.
    next_seq: SequenceId,
    /// How many entries have been pushed out of the front. Phase 9's
    /// "Event N of M" needs this, and ROADMAP SC-4's "evicted events still
    /// accounted for" is exactly the equation
    /// `evicted_count + len == next_seq`.
    evicted_count: u64,
}

impl History {
    /// Record one applied event and return the identity assigned to it.
    ///
    /// Order matters and is not incidental: the identity is taken from the
    /// counter FIRST, then the front is evicted if the buffer is already
    /// full, then the entry is appended. Assigning before evicting is what
    /// makes the identity independent of the container's state -- an
    /// implementation that derived the identity from the post-eviction
    /// length would produce exactly the positional identity EVENT-04 rules
    /// out.
    pub fn push(&mut self, event: seam_core::GraphEvent) -> SequenceId {
        let seq = self.next_seq;
        self.next_seq += 1;
        if self.entries.len() >= self.capacity() {
            self.entries.pop_front();
            self.evicted_count += 1;
        }
        self.entries.push_back(HistoryEntry { seq, event });
        seq
    }

    /// How many entries are currently retained (never more than
    /// [`Self::capacity`]).
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The cap, read from the single shared constant rather than restated
    /// here (D-05a).
    pub fn capacity(&self) -> usize {
        seam_core::LIVE_BUFFER_CAPACITY
    }

    /// How many entries have been evicted since this timeline began.
    pub fn evicted_count(&self) -> u64 {
        self.evicted_count
    }

    /// The identity the next push will assign. Also the total number of
    /// events ever recorded on this timeline -- Phase 9's "M".
    pub fn next_seq(&self) -> SequenceId {
        self.next_seq
    }

    /// Look up an entry BY ITS IDENTITY. Returns nothing when that identity
    /// has been evicted, or was never issued.
    ///
    /// This searches on the sequence VALUE. Entries are pushed in issuing
    /// order and never reordered, so they are sorted by `seq` and a binary
    /// search is both correct and cheap.
    ///
    /// The tempting constant-time alternative -- treating
    /// `seq - evicted_count` as an offset into the buffer -- is rejected, but
    /// be precise about why, because the two candidates are NOT equally
    /// wrong and this was measured rather than assumed (08-03, by mutation):
    ///
    /// - `entries.get(seq)` -- the literal raw array index EVENT-04 names --
    ///   is genuinely broken after a wrap, and the tests catch it. Mutating
    ///   this method to that form fails
    ///   `an_evicted_identity_reports_as_gone_rather_than_resolving_to_the_wrong_event`
    ///   and the integration test on the same claim.
    /// - `entries.get(seq - evicted_count)` is, under TODAY's push
    ///   discipline, observationally identical to the search below: mutating
    ///   this method to that form passes the entire suite. No test defends
    ///   against it, and none can, because nothing distinguishes them.
    ///
    /// It is still the wrong choice, and the reason is worth stating since no
    /// test will say it for you. Its correctness rests on an invariant this
    /// type does not enforce -- that entries leave the front one at a time,
    /// each one incrementing `evicted_count`, and that no other path ever
    /// removes an entry. The day that stops holding (a Phase 9 truncation, a
    /// compaction, a partial replay) the offset form does not start returning
    /// errors. It returns a plausible WRONG entry, which Phase 9 renders to
    /// the user as history. The search below stays correct through all of
    /// that because it asks the only question that is actually being asked:
    /// which entry has this identity.
    ///
    /// So: the slower-looking choice is deliberate, and the acceptance grep
    /// plus this comment -- not a test -- are what defend it. Do not
    /// "optimise" it.
    pub fn get(&self, seq: SequenceId) -> Option<&HistoryEntry> {
        let found = self
            .entries
            .binary_search_by(|entry| entry.seq.cmp(&seq))
            .ok()?;
        self.entries.get(found)
    }

    /// Iterate the retained entries oldest-first -- the order Phase 9's
    /// replay depends on.
    pub fn iter(&self) -> impl Iterator<Item = &HistoryEntry> {
        self.entries.iter()
    }

    /// Start a new timeline: drop every entry AND reset both counters.
    ///
    /// **The sequence counter resets too, and that is the deliberate
    /// choice.** A history is cleared when a different `graph.json` is
    /// loaded, and events recorded against the old graph describe changes
    /// that were never applied to the new one. Carrying the counter forward
    /// would leave a Phase 9 reader holding identities from a graph that is
    /// no longer on screen, with no way to tell them apart from live ones.
    /// Resetting makes a stale identity resolve as evicted-or-never-issued,
    /// which is the honest answer.
    pub fn clear(&mut self) {
        self.entries.clear();
        self.next_seq = 0;
        self.evicted_count = 0;
    }
}

/// What one [`drain_and_apply`] call did, for callers that want to react to
/// it.
#[derive(Debug, Default, PartialEq)]
pub struct ApplySummary {
    pub applied_count: usize,
    /// The real ids of nodes this batch removed, carried as data so removal
    /// consequences never need a second scan of the model.
    pub removed_node_ids: Vec<String>,
    /// The inclusive `(first, last)` identity range this call recorded into
    /// the history, or nothing when it recorded none.
    ///
    /// Reported as data so a caller (or a test) can say what was written
    /// without reaching into the buffer and, in doing so, reintroducing the
    /// positional thinking [`History::get`] exists to avoid.
    pub recorded: Option<(SequenceId, SequenceId)>,
    /// How many `AddEdge`s this call parked pending an internal target that
    /// has not arrived yet (08-04, D-05).
    ///
    /// Carried up to the app layer so a future plan can surface it (a "3
    /// pending" indicator, say) without changing this signature. **No UI is
    /// built for it here** -- and none should be until 08-05's retry sweep
    /// exists, or the number would only ever go up and read as a leak.
    pub parked_edges: usize,
    /// How many `AddEdge`s this call dropped because their target names
    /// something outside the user's project (08-04, D-05a). Same
    /// carry-it-do-not-show-it rule as [`Self::parked_edges`].
    pub dropped_external_edges: usize,
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
                //
                // 08-04: an applied or removed EDGE sets `topology_changed`
                // too, so it drives this same recomputation and the seam
                // refresh below. It has to. An added crossing edge changes
                // the ranked list AND can close a cycle, flipping a seam's
                // verdict; skipping either would put a stale verdict beside
                // a changed canvas, which is the exact thing ROADMAP SC-1
                // names. A parked or dropped edge changes neither, and
                // correctly does not set the flag.
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

    // Record ONLY the apply outcome's resolved-events list, never the raw
    // drained batch. That distinction is the entire reason `ApplyOutcome`
    // carries `applied` separately: the raw batch also contains no-ops,
    // unresolved communities, and (from 08-04 onward) events that were parked
    // or dropped rather than applied -- none of which describe something that
    // happened to the graph. Recording one of those would make Phase 9 replay
    // a graph the user never saw (T-08-03-02).
    let mut recorded: Option<(SequenceId, SequenceId)> = None;
    for event in &outcome.applied {
        // `event.rs`'s doc comment on `AddNode.community` names Phase 8 as
        // the enforcer of "a `None` must never survive into applied or
        // history-stored graph state". The integration test is the real
        // enforcement; this fires during development, right next to the
        // mistake.
        debug_assert!(
            !matches!(
                event,
                seam_core::GraphEvent::AddNode {
                    community: None,
                    ..
                }
            ),
            "a recorded AddNode must carry a concrete community -- event.rs's \
             invariant that an absent wire community never survives into \
             applied or history-stored state: {event:?}"
        );
        let seq = app.history.push(event.clone());
        recorded = Some(match recorded {
            Some((first, _)) => (first, seq),
            None => (seq, seq),
        });
    }

    ApplySummary {
        applied_count: outcome.applied.len(),
        removed_node_ids: outcome.removed_node_ids,
        recorded,
        parked_edges: outcome.parked_edges,
        dropped_external_edges: outcome.dropped_external_edges,
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
