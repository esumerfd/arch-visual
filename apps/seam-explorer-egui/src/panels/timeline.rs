//! Bottom panel: the time-travel indicator and its four navigation buttons
//! (TIME-01, TIME-02, TIME-04; plan 09-05).
//!
//! **This panel decides nothing.** It renders `crate::timeline::status` and
//! hands a `TimelineAction` to `crate::timeline::apply_action` -- the same
//! entry point `keyboard.rs` calls. It computes no target, does no
//! `SequenceId` arithmetic, and clears no selection. That is what makes
//! ROADMAP SC-1's "either route produces the same single step" structural
//! rather than coincidental: there is nothing here for the two routes to
//! disagree about.
//!
//! **Two modules are named `timeline`, so every path below is written out in
//! full.** `crate::timeline` is the navigation logic (09-02); this file is
//! `crate::panels::timeline`, the panel. A bare `timeline::...` inside this
//! file resolves against nothing -- a module's own name is not in its own
//! scope -- so it is a compile error rather than a subtle mix-up. A
//! `use crate::timeline;` at the top would paper over exactly that: an
//! unqualified `timeline::apply_action` sitting inside a file called
//! `timeline.rs` reads as self-reference to the next person, and the whole
//! point of this panel is that it delegates rather than reimplements.
//!
//! D-04 is why the badge and the position are unconditional: they render in
//! every state, Live or Paused, including before a single event has arrived.
//! Nothing here is revealed only after first use.

use crate::app::SeamExplorerApp;

/// The badge while the view follows the live graph.
pub const LIVE_BADGE: &str = "Live";

/// The badge while the view is pinned at a historical position.
pub const PAUSED_BADGE: &str = "Paused";

/// The position line before anything has been recorded. "Event 0 of 0" would
/// invite the reader to wonder which event zero is; this is an honest empty
/// state, rendered in the same place the position always is (D-04).
pub const EMPTY_POSITION: &str = "No events yet";

/// D-04's disclosure: the user is pinned at the oldest event that still exists,
/// and what came before it is gone.
///
/// A const, not a literal at the call site, so the tests assert the string the
/// panel actually renders -- the same discipline [`LIVE_BADGE`] and
/// [`EMPTY_POSITION`] follow. One short line, sized to sit beside
/// "Event N of M": it says where the user is AND that older history is gone,
/// and deliberately stops there. No count of lost events and no call to action,
/// because there is nothing the user can do about it and D-04 explicitly
/// rejected building a notification surface for this.
pub const EVICTED_NOTICE: &str = "Earliest available (older history evicted)";

/// Is the user pinned at the oldest still-retained event of a buffer that has
/// actually wrapped? (D-04, deferred to Phase 10 by 09-VERIFICATION.md.)
///
/// Public for the same reason [`position_text`] is: the rule is then testable
/// directly, without a window, rather than only through what rendered.
///
/// Three decisions worth recording, because each is one a later reader would
/// otherwise have to re-derive:
///
/// - **Why `bounds` rather than the counter.** `crate::timeline::bounds`'s first
///   element IS `History::evicted_count()`, and it is the same pair
///   `next_position` resolves through. Going through it means this notice and
///   the disabled Step-back button agree BY CONSTRUCTION rather than by two
///   rules someone has to keep in step -- and it keeps this panel's standing
///   "no position arithmetic here" contract intact.
/// - **Why `earliest > 0` is required**, even though D-04's sentence does not
///   name it. With nothing ever evicted `earliest` is 0, so a user sitting at
///   the very first event satisfies a literal "position equals the evicted
///   count" -- and would be told older history was evicted when none ever
///   existed. The guard is what makes the sentence TRUE, not a narrowing of
///   D-04.
/// - **Why there is no Live branch.** `scrub_position` is `None` while Live, so
///   Live cannot match. That is structural, not lucky: once anything has been
///   evicted the buffer is full at `LIVE_BUFFER_CAPACITY`, so the newest
///   retained identity is `earliest + LIVE_BUFFER_CAPACITY - 1` and can never
///   equal `earliest`. A Live branch would be unreachable.
pub fn shows_evicted_notice(app: &SeamExplorerApp) -> bool {
    let Some((earliest, _)) = crate::timeline::bounds(&app.history) else {
        return false;
    };
    earliest > 0 && app.scrub_position.is_some_and(|pinned| pinned == earliest)
}

pub const EARLIEST_LABEL: &str = "Earliest";
pub const STEP_BACK_LABEL: &str = "Step back";
pub const STEP_FORWARD_LABEL: &str = "Step forward";
pub const LATEST_LABEL: &str = "Latest";

/// Hover text naming each button's keyboard equivalent, so the locked scheme
/// is discoverable rather than folklore.
pub const EARLIEST_TOOLTIP: &str = "Cmd+Left";
pub const STEP_BACK_TOOLTIP: &str = "Alt+Left";
pub const STEP_FORWARD_TOOLTIP: &str = "Alt+Right";
pub const LATEST_TOOLTIP: &str = "Cmd+Right";

/// TIME-04's "Event N of M", as one function so the tests assert the string
/// the panel actually renders rather than a duplicated literal.
///
/// `N` is one-based and equals `M` while Live. `M` is every event ever
/// recorded on this timeline, so it keeps climbing while the user sits at a
/// historical position (D-03) -- a frozen count would claim events stopped
/// arriving when they did not.
pub fn position_text(status: &crate::timeline::TimelineStatus) -> String {
    if status.total == 0 {
        EMPTY_POSITION.to_string()
    } else {
        format!("Event {} of {}", status.position, status.total)
    }
}

/// Would this action change where the user is?
///
/// Derived from `crate::timeline::next_position` -- the very function
/// `apply_action` resolves through -- rather than restated as a second rule.
/// A disabled button and a no-op keypress therefore agree by construction,
/// not by two conditions someone must keep in step.
fn would_move(app: &SeamExplorerApp, action: crate::timeline::TimelineAction) -> bool {
    let next = crate::timeline::next_position(
        app.scrub_position,
        crate::timeline::bounds(&app.history),
        action,
    );
    next != app.scrub_position
}

/// One navigation button: enabled iff its action would move, labelled, and
/// carrying its keyboard equivalent as hover text. Returns whether it was
/// clicked; the caller performs the navigation, so every `apply_action` call
/// site is visible in this file rather than hidden behind one shared call.
fn nav_button(
    ui: &mut egui::Ui,
    app: &SeamExplorerApp,
    label: &str,
    tooltip: &str,
    action: crate::timeline::TimelineAction,
) -> bool {
    ui.add_enabled(would_move(app, action), egui::Button::new(label))
        .on_hover_text(tooltip)
        .clicked()
}

/// The bottom panel's body. Badge, position, then the four buttons in the
/// order the timeline runs: oldest on the left, newest on the right, matching
/// the arrow keys that drive them.
pub fn show(ui: &mut egui::Ui, app: &mut SeamExplorerApp) {
    let status = crate::timeline::status(app);

    ui.horizontal(|ui| {
        // Colours come from `panels/mod.rs`'s single palette source, never
        // from a fresh hex literal here -- Clean's green for the ordinary
        // Live state, Watch's amber for the deliberate, temporary one.
        let (badge, badge_color) = if status.paused {
            (
                PAUSED_BADGE,
                super::verdict_color(&seam_core::Verdict::Watch),
            )
        } else {
            (LIVE_BADGE, super::verdict_color(&seam_core::Verdict::Clean))
        };
        ui.colored_label(badge_color, badge);

        ui.separator();
        ui.colored_label(ui.visuals().weak_text_color(), position_text(&status));

        // D-04's disclosure, inline beside the position it qualifies rather than
        // in a new UI surface. Watch's amber, from the one palette file -- the
        // same colour the Paused badge uses for the deliberate, temporary state,
        // because this is informative, not an error.
        if shows_evicted_notice(app) {
            ui.separator();
            ui.colored_label(
                super::verdict_color(&seam_core::Verdict::Watch),
                EVICTED_NOTICE,
            );
        }

        ui.separator();

        if nav_button(
            ui,
            app,
            EARLIEST_LABEL,
            EARLIEST_TOOLTIP,
            crate::timeline::TimelineAction::JumpEarliest,
        ) {
            crate::timeline::apply_action(app, crate::timeline::TimelineAction::JumpEarliest);
        }
        if nav_button(
            ui,
            app,
            STEP_BACK_LABEL,
            STEP_BACK_TOOLTIP,
            crate::timeline::TimelineAction::StepBack,
        ) {
            crate::timeline::apply_action(app, crate::timeline::TimelineAction::StepBack);
        }
        if nav_button(
            ui,
            app,
            STEP_FORWARD_LABEL,
            STEP_FORWARD_TOOLTIP,
            crate::timeline::TimelineAction::StepForward,
        ) {
            crate::timeline::apply_action(app, crate::timeline::TimelineAction::StepForward);
        }
        if nav_button(
            ui,
            app,
            LATEST_LABEL,
            LATEST_TOOLTIP,
            crate::timeline::TimelineAction::JumpLatest,
        ) {
            crate::timeline::apply_action(app, crate::timeline::TimelineAction::JumpLatest);
        }
    });
}
