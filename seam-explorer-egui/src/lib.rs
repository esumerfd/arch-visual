//! `seam-explorer-egui`: native, webview-free rebuild of Seam Explorer
//! (design doc Solution 2, phase 5, M0-M3). Library crate so integration
//! tests (`tests/tracer_smoke.rs`, `tests/panels.rs`) and `src/main.rs` share
//! one module tree.

pub mod app;
pub mod graph_view;
pub mod keyboard;
pub mod layout;
pub mod load;
pub mod open_file;
pub mod overlay;
pub mod panels;
pub mod settings;
pub mod startup;
pub mod trace;
