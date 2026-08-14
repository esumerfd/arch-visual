//! `SeamExplorerApp`: the eframe app struct (design doc §6.5). This is the
//! frozen interface for Plans 02-05 — the complete field set is declared
//! here even though most fields stay unread by this task's UI. Plans 02-05
//! fill panel/module bodies; they must NOT edit this file's field list or
//! `update()`'s panel-dispatch wiring.
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
        self.model = Some(outcome.model);
        self.seams = outcome.seams;
        self.banner = outcome.banner;
        self.focus = None;
        self.detail = None;
        self.trace = None;
    }
}

impl eframe::App for SeamExplorerApp {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
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
