//! Bundled font install (UX #191, finding 3).
//!
//! egui's default proportional font (Ubuntu-Light) has no glyph for the
//! `→` in the ARMED banner, nor for `°`/`µ`/box-drawing/block/Braille —
//! the character graphics `ux.md` calls for. It rendered as tofu (`□`),
//! making the safety banner look corrupted.
//!
//! The fix belongs in the renderer, not in `ac-scene`: `ac-scene`
//! computes the true string, `ac-view` draws whatever it emits. Changing
//! the scene's string because the renderer lacks a glyph would leak the
//! renderer's font coverage upstream to mutate ground truth — and make
//! the scene vocabulary hostage to the next missing glyph. So we bundle a
//! font with the coverage instead.
//!
//! **Bundled, not system-loaded.** `include_bytes!` embeds the TTF in the
//! binary: the field measurement box is exactly where a system-font
//! dependency renders fine on the dev machine and tofu on the target.
//! DejaVu Sans covers `→ ° µ █ ─ ⠇` (verified) and ships under the
//! permissive Bitstream Vera / DejaVu license.

use egui::{FontData, FontDefinitions, FontFamily};

const DEJAVU_SANS: &[u8] = include_bytes!("../assets/DejaVuSans.ttf");

/// Install the bundled font as the primary proportional and monospace
/// family, keeping egui's defaults as fallbacks. Call once at startup,
/// on the real app's context and on any test harness that renders text —
/// otherwise the pixel snapshots would tofu the same glyph the app does.
pub fn install(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    fonts.font_data.insert(
        "dejavu".to_owned(),
        FontData::from_static(DEJAVU_SANS).into(),
    );

    // Front of both families: the bundled font is tried first, egui's
    // defaults remain as fallbacks for anything it happens to lack.
    for family in [FontFamily::Proportional, FontFamily::Monospace] {
        fonts
            .families
            .entry(family)
            .or_default()
            .insert(0, "dejavu".to_owned());
    }

    ctx.set_fonts(fonts);
}
