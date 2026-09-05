//! This file exists to answer `.planning/ROADMAP.md`'s "Open Phase-Level Checks
//! (v1.1)" row assigned to Phase 6: whether `egui::Context::request_repaint()`,
//! called from a background thread against the pinned egui 0.35.0, is safe --
//! the open upstream caveat is egui#1379, about a possible panic/deadlock when
//! a repaint is requested while the UI thread holds certain `Context` locks.
//! Plan 06-02's entire wake mechanism (a background socket thread waking an
//! idle eframe loop) rests on the answer this file gives. It is kept
//! PERMANENTLY in the suite as a regression guard against a future egui
//! version bump silently reintroducing the hazard.
//!
//! Source trail (`egui-0.35.0/src/context.rs`): `Context::request_repaint`
//! (~line 1753) delegates to `request_repaint_of` (~line 1770), which builds a
//! `RepaintCause` and calls `self.write(|ctx| ctx.request_repaint(id, cause))`;
//! `Context::write` (~line 751) is `writer(&mut self.0.write())` over
//! `pub struct Context(Arc<RwLock<ContextImpl>>)` (~line 710), where that
//! `RwLock` is `epaint::mutex::RwLock` (imported at `context.rs` line 6-14).
//! `has_requested_repaint` (~line 1866) and `repaint_causes` (~line 1879 --
//! it returns `prev_causes`, i.e. the PREVIOUS pass's causes, which is why
//! this file steps once before reading it) and `RepaintCause { file, line,
//! reason }` (~line 250) are the read-side API this file asserts against.
//!
//! Source trail (`epaint-0.35.0/src/mutex.rs`): egui's own `RwLock` wrapper is
//! NOT a bare `parking_lot::RwLock` and does not rely on parking_lot's
//! optional `deadlock_detection` Cargo feature (neither `egui-0.35.0/Cargo.toml`
//! nor `epaint-0.35.0/Cargo.toml` enables that feature -- confirmed by reading
//! both manifests, no `parking_lot` feature list includes it). Instead,
//! `epaint::mutex::RwLock::write`/`read` (mutex.rs lines ~78-110) call
//! `try_write_for(Duration::from_secs(10))` / `try_read_for(...)`, and on
//! timeout, IN DEBUG BUILDS ONLY (`cfg!(debug_assertions)`), panic with
//! "DEBUG PANIC: Failed to acquire RwLock {read,write} after 10s. Deadlock?".
//! This is a hand-rolled, always-on (in debug builds) deadlock detector
//! bundled into egui/epaint itself, not a parking_lot feature flag one grep
//! could miss -- an important correction to the planning-time assumption that
//! "no deadlock-detection feature" implied "expect blocking, not panicking":
//! a genuine cross-thread deadlock in a `cargo test` run (which builds in
//! debug mode by default) would surface as a panic within ~10 seconds, not an
//! infinite hang. That panic happens on whichever thread is blocked; since
//! this file's stress test deliberately does not `join()` its background
//! threads (see below), such a panic would be printed to stderr by the
//! Rust test harness's own default panic hook but would NOT crash the main
//! test thread or the process -- which is exactly why this file polls a
//! shared completion counter with a wall-clock deadline instead of joining.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// The deterministic half: proves the wake signal actually lands, which is
/// the property EVENT-03 depends on -- a stronger claim than "did not crash".
#[test]
fn a_background_thread_repaint_request_is_observable_on_the_ui_thread() {
    let mut harness = egui_kittest::Harness::new_ui(|_ui| {});
    harness.run_steps(3);

    let ctx = harness.ctx.clone();
    let handle = std::thread::spawn(move || {
        ctx.request_repaint();
    });
    handle.join().expect(
        "background thread calling Context::request_repaint() must not panic -- \
         this is the exact call egui#1379 raises a caveat about",
    );

    assert!(
        harness.ctx.has_requested_repaint(),
        "a repaint requested from a background thread must be visible to the \
         UI thread via Context::has_requested_repaint() immediately after the \
         background thread's join, before any further step -- egui#1379's \
         caveat would manifest here as either a panic (already ruled out by \
         the successful join above) or a silently-lost request"
    );

    harness.step();
    let causes = harness.ctx.repaint_causes();
    assert!(
        causes.iter().any(|c| c.file.ends_with("repaint_probe.rs")),
        "expected repaint_causes() to attribute at least one repaint to this \
         file's background call site (egui attributes causes by #[track_caller] \
         file/line), got: {causes:?}"
    );
}

/// The stress half: interleaves 2000 cross-thread repaint requests (4 threads
/// x 500 each) with 200 real UI passes on the main thread, so the UI thread is
/// genuinely taking and releasing the same `Context` lock throughout -- not a
/// single lucky uncontended call.
///
/// Deliberately does NOT `join()` the worker threads: a genuine egui#1379-class
/// deadlock leaves a worker permanently blocked (or, per the source-read
/// finding above, panicking after epaint's own 10s debug-mode timeout), and
/// joining would risk hanging `cargo test` with no diagnostic -- the single
/// worst outcome for a probe whose job is to produce an answer. Instead this
/// polls a shared completion counter against a wall-clock deadline and fails
/// with a clear diagnostic if not all 4 workers finish in time.
#[test]
fn concurrent_background_repaint_requests_neither_panic_nor_deadlock() {
    let mut harness = egui_kittest::Harness::new_ui(|_ui| {});
    harness.run_steps(3);

    const WORKERS: usize = 4;
    const REQUESTS_PER_WORKER: usize = 500;
    let completed = Arc::new(AtomicUsize::new(0));

    for _ in 0..WORKERS {
        let ctx = harness.ctx.clone();
        let completed = completed.clone();
        // Detached: not joined, by design (see doc comment above).
        std::thread::spawn(move || {
            for _ in 0..REQUESTS_PER_WORKER {
                ctx.request_repaint();
            }
            completed.fetch_add(1, Ordering::SeqCst);
        });
    }

    harness.run_steps(200);

    let deadline = Instant::now() + Duration::from_secs(30);
    while completed.load(Ordering::SeqCst) < WORKERS && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }

    let finished = completed.load(Ordering::SeqCst);
    assert_eq!(
        finished, WORKERS,
        "only {finished} of {WORKERS} background workers finished within 30s \
         of wall-clock waiting -- this reproduces the egui#1379 failure mode \
         (a background thread calling Context::request_repaint() while the UI \
         thread holds the same Context's internal RwLock) on the pinned \
         egui 0.35.0"
    );
}
