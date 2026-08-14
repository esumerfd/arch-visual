//! NAV-05: single global keyboard dispatch, mirroring the D3 app's one
//! `keydown` listener (Phase 2 D-09) — arrows pan, `+`/`-` zoom, `0` resets,
//! `t` toggles trace mode, with a search-input focus carve-out. Compiling
//! stub this task (frozen signature only); real key handling lands in a
//! later plan (NAV-05).

use crate::app::SeamExplorerApp;

pub fn handle(_ctx: &egui::Context, _app: &mut SeamExplorerApp, _search_id: egui::Id) {
    // Real implementation (RESEARCH Pattern 3) lands in a later plan.
}
