//! Left/right `SidePanel` content — SEAM-01 (seam list), SEAM-02 (detail),
//! SEAM-03 (legend), GRAPH-02 (banner, Task 3).
//!
//! `verdict_color`/`verdict_title` live here (not in an individual panel
//! module) so the three verdict hex values appear in exactly one file and
//! every panel — plus Plan 03's canvas overlay — reuses the same mapping
//! instead of re-typing hex literals (05-UI-SPEC.md Color table,
//! 05-RESEARCH.md Pattern 5).

pub mod detail;
pub mod legend;
pub mod seam_list;

/// Verdict -> the ported palette. Single source of truth for the three
/// verdict hex values.
pub fn verdict_color(v: &seam_core::Verdict) -> egui::Color32 {
    match v {
        seam_core::Verdict::Clean => egui::Color32::from_hex("#4fd08a").expect("valid hex"),
        seam_core::Verdict::Watch => egui::Color32::from_hex("#f2c14e").expect("valid hex"),
        seam_core::Verdict::Leaky => egui::Color32::from_hex("#ff5c72").expect("valid hex"),
        // D-06 (Phase 1 CONTEXT, reconfirmed for this phase): `Utility` is
        // never constructed in v1 -- kept only so this match stays
        // exhaustive; no real call site can reach this arm.
        seam_core::Verdict::Utility => egui::Color32::from_hex("#61708c").expect("valid hex"),
    }
}

/// Verdict -> the detail-panel heading title (05-RESEARCH.md Pattern 5,
/// verbatim from the D3 original's `VERDICT_TITLE` map).
pub fn verdict_title(v: &seam_core::Verdict) -> &'static str {
    match v {
        seam_core::Verdict::Clean => "Clean seam",
        seam_core::Verdict::Watch => "Watch — coupling",
        seam_core::Verdict::Leaky => "Leaky seam",
        seam_core::Verdict::Utility => "Utility",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verdict_color_maps_exact_hex() {
        assert_eq!(
            verdict_color(&seam_core::Verdict::Clean),
            egui::Color32::from_rgb(0x4f, 0xd0, 0x8a)
        );
        assert_eq!(
            verdict_color(&seam_core::Verdict::Watch),
            egui::Color32::from_rgb(0xf2, 0xc1, 0x4e)
        );
        assert_eq!(
            verdict_color(&seam_core::Verdict::Leaky),
            egui::Color32::from_rgb(0xff, 0x5c, 0x72)
        );
    }

    #[test]
    fn verdict_title_maps_exact_strings() {
        assert_eq!(verdict_title(&seam_core::Verdict::Clean), "Clean seam");
        assert_eq!(
            verdict_title(&seam_core::Verdict::Watch),
            "Watch — coupling"
        );
        assert_eq!(verdict_title(&seam_core::Verdict::Leaky), "Leaky seam");
    }
}
