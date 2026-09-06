//! `SeamExplorerApp`: the eframe app struct (design doc §6.5). This is the
//! frozen interface for Plans 02-05 — the complete field set is declared
//! here even though most fields stay unread by this task's UI. Plans 02-05
//! fill panel/module bodies; they must NOT edit this file's field list or
//! `update()`'s panel-dispatch wiring.
//!
//! Named exceptions to that freeze, in the order they were taken:
//! - 08-01 — the one `drain_and_apply` statement at the top of `ui()`.
//! - 09-02 — four `#[serde(skip)]` scrub/baseline fields, plus their
//!   capture-and-reset in `apply_load_outcome`. `ui()` is NOT touched by
//!   09-02; plan 09-05 declares its own separate bottom-panel exception.
//!
//! Persistence discipline (T-05-04, D-14): only `has_seen_trace_onboarding`
//! round-trips through `eframe::Storage`. Every runtime field carries
//! `#[serde(skip)]` so graph contents / analysed file names never reach
//! disk.

use crate::trace::TraceResult;
use crate::{graph_view, keyboard, panels};

/// Pan/zoom state for the central graph canvas (NAV-02).
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
pub struct ViewState {
    pub zoom: f32,
    pub pan: egui::Vec2,
}

impl Default for ViewState {
    fn default() -> Self {
        Self {
            zoom: 1.0,
            pan: egui::Vec2::ZERO,
        }
    }
}

/// The two communities pulled apart by a seam-focus click (NAV-04, D-13's
/// pull-apart visual).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FocusState {
    pub a: seam_core::CommunityId,
    pub b: seam_core::CommunityId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BannerKind {
    Warning,
    Error,
}

/// GRAPH-02's in-UI warning/error banner — same taxonomy as the Tauri app's
/// `CommandError`, re-expressed as renderable data instead of a `Result::Err`
/// (Pattern map "Error handling" shared pattern).
#[derive(Debug, Clone)]
pub struct Banner {
    pub kind: BannerKind,
    pub heading: String,
    pub body: String,
}

/// The eframe app struct. Complete field set per design doc §6.5 — declared
/// now so Plans 02-05 code against a frozen interface.
#[derive(serde::Serialize, serde::Deserialize, Default)]
#[serde(default)]
pub struct SeamExplorerApp {
    /// The ONLY field that round-trips through `eframe::Storage` (D-14).
    pub has_seen_trace_onboarding: bool,

    #[serde(skip)]
    pub model: Option<seam_core::Model>,
    #[serde(skip)]
    pub seams: Vec<seam_core::Seam>,
    #[serde(skip)]
    pub focus: Option<FocusState>,
    #[serde(skip)]
    pub detail: Option<seam_core::SeamDetail>,
    #[serde(skip)]
    pub trace: Option<TraceResult>,
    #[serde(skip)]
    pub trace_mode: bool,
    #[serde(skip)]
    pub banner: Option<Banner>,
    #[serde(skip)]
    pub search_query: String,
    #[serde(skip)]
    pub view: ViewState,
    /// 08-03: the skip is load-bearing, not decoration -- see the module doc.
    #[serde(skip)]
    pub history: crate::history::History,

    /// 09-02: the state immediately BEFORE the oldest still-retained
    /// [`crate::history::History`] entry — the starting point every replay
    /// reconstruction clones.
    ///
    /// Named for what it MEANS, not for where it came from. It is captured at
    /// load as the graph exactly as ingested, but that is only its value while
    /// the buffer is under its cap. `history::record` advances it by exactly
    /// one event at the instant the buffer evicts one, so
    /// `replay_baseline + retained entries` always equals the full recorded
    /// event stream. A name like "loaded snapshot" would encode an invariant
    /// that stops holding at the hundred-and-first event, and a baseline
    /// frozen there reconstructs a graph missing every evicted event's effect
    /// — self-consistent, repeatable, and wrong (T-09-02-07).
    #[serde(skip)]
    pub replay_baseline: Option<seam_core::Model>,

    /// 09-02: `None` means Live. `Some(seq)` means Paused, displaying the
    /// state after events up to and including `seq`.
    #[serde(skip)]
    pub scrub_position: Option<crate::history::SequenceId>,

    /// 09-02: the reconstruction for the current [`Self::scrub_position`].
    ///
    /// Deliberately its OWN field, never assigned into [`Self::model`]:
    /// `graph_view::inject_layout_targets` prunes persisted node positions
    /// against `app.model`'s id set every frame, and a historical model has
    /// fewer nodes, so one frame rendered with a reconstruction sitting in
    /// `model` permanently deletes the settled position of every node the live
    /// graph gained after the paused point (T-09-02-02).
    #[serde(skip)]
    pub scrub_model: Option<seam_core::Model>,

    /// 09-02: that reconstruction's ranked seam list.
    #[serde(skip)]
    pub scrub_seams: Vec<seam_core::Seam>,
}

impl SeamExplorerApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        if let Some(storage) = cc.storage {
            eframe::get_value(storage, eframe::APP_KEY).unwrap_or_default()
        } else {
            Self::default()
        }
    }

    /// Runs the Load-graph action end-to-end: native dialog -> read -> pure
    /// ingest -> apply. The only place `load::pick_file` is called.
    pub fn load_graph(&mut self) {
        let Some(path) = crate::load::pick_file() else {
            return; // dialog cancelled — no-op, matches CommandError::Cancelled being silent here
        };
        match std::fs::read_to_string(&path) {
            Ok(json) => match crate::load::read_and_ingest(&json) {
                Ok(outcome) => self.apply_load_outcome(outcome),
                Err(e) => self.banner = Some(crate::load::error_banner(&e)),
            },
            Err(e) => {
                self.banner = Some(crate::load::error_banner(&crate::load::LoadError::from(e)))
            }
        }
    }

    /// Applies a successful `load::read_and_ingest` result to app state:
    /// model, ranked seams, and any dropped-edge warning banner. Clears any
    /// stale focus/detail/trace from a previously loaded graph.
    pub fn apply_load_outcome(&mut self, outcome: crate::load::LoadOutcome) {
        // 09-02: cloned BEFORE the move below, never by cloning `self.model`
        // back out afterwards -- one clone, and the ordering makes it obvious
        // to a reader which value is captured. With an empty history, "the
        // state before the oldest retained entry" IS the loaded graph, so
        // `replay_baseline`'s invariant holds from the first frame.
        self.replay_baseline = Some(outcome.model.clone());
        self.model = Some(outcome.model);
        self.seams = outcome.seams;
        self.banner = outcome.banner;
        self.focus = None;
        self.detail = None;
        self.trace = None;
        self.history.clear(); // 08-03: a new graph starts a new timeline (T-08-03-05)

        // 09-02: not optional housekeeping. `History::clear` above resets the
        // sequence counter to zero, so a surviving position from the previous
        // graph would silently name an event on the NEW graph's timeline.
        self.scrub_position = None;
        self.scrub_model = None;
        self.scrub_seams = Vec::new();
    }
}

impl eframe::App for SeamExplorerApp {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        // Plan 08-01: the ONE deliberate, named exception to this file's
        // freeze (see the module doc above). Live events must be applied
        // BEFORE the panel dispatch below, not from inside
        // `graph_view::show`: `seam_list_panel` and `detail_panel` are drawn
        // ahead of `CentralPanel`, so applying downstream of them would leave
        // `app.seams`/`app.detail` one frame behind the canvas after every
        // arriving event -- literally ROADMAP SC-1's "a stale list beside a
        // changed canvas". Nothing else in this file changes.
        crate::history::drain_and_apply(self);

        let _ = frame;
        let ctx = ui.ctx().clone();
        let search_id = egui::Id::new("seam_explorer_search_input");

        egui::Panel::top("top_bar").show(ui, |ui| {
            ui.horizontal(|ui| {
                if ui.button("Load graph.json").clicked() {
                    self.load_graph();
                }
                if ui.button("Reset view").clicked() {
                    self.view = ViewState::default();
                }
                let label = if self.trace_mode {
                    "Trace mode · on"
                } else {
                    "Trace mode · off"
                };
                if ui.button(label).clicked() {
                    self.trace_mode = !self.trace_mode;
                }
            });
        });

        egui::Panel::left("seam_list_panel").show(ui, |ui| {
            panels::seam_list::show(ui, self);
        });

        egui::Panel::right("detail_panel").show(ui, |ui| {
            panels::detail::show(ui, self);
            ui.separator();
            panels::legend::show(ui);
        });

        egui::CentralPanel::default().show(ui, |ui| {
            graph_view::show(ui, self);
        });

        keyboard::handle(&ctx, self, search_id);
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, eframe::APP_KEY, self);
    }
}
