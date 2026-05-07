// Fire / dark-red high-contrast palette. Background is pure black; channel
// hues sweep ember → vermilion → amber → pale gold so overlapping traces
// stay distinguishable while the overall look stays in-family. Chrome
// (grid lines, axis labels, status text) is deliberately muted so the
// spectrum traces own the foreground and there is minimum competing light.
pub const BG: [f32; 4] = [0.000, 0.000, 0.000, 1.0];
pub const GRID_LABEL: [u8; 3] = [0x60, 0x3C, 0x30];
pub const TEXT: [u8; 3] = [0xC8, 0xA0, 0x70];
pub const CLIP_LED: [u8; 3] = [0xFF, 0x3A, 0x1C];
pub const SELECT_BORDER: [u8; 3] = [0xFF, 0xC8, 0x3A];


pub const CHANNEL_COLORS: [[f32; 4]; 10] = [
    rgb(0xFF, 0xC8, 0x3A), // bright gold
    rgb(0xFF, 0x6A, 0x00), // hot orange
    rgb(0xE6, 0x24, 0x2B), // vermilion red
    rgb(0xFF, 0xA6, 0x3D), // amber
    rgb(0xA8, 0x11, 0x20), // blood red
    rgb(0xFF, 0x8A, 0x5C), // coral
    rgb(0xFF, 0xE0, 0x7A), // pale gold
    rgb(0xD6, 0x4B, 0x1A), // burnt orange
    rgb(0x7A, 0x04, 0x0E), // deep ember
    rgb(0xFF, 0x4E, 0x33), // flame
];

const fn rgb(r: u8, g: u8, b: u8) -> [f32; 4] {
    [
        r as f32 / 255.0,
        g as f32 / 255.0,
        b as f32 / 255.0,
        1.0,
    ]
}

pub fn channel_color(idx: usize) -> [f32; 4] {
    let base = CHANNEL_COLORS[idx % CHANNEL_COLORS.len()];
    let cycle = idx / CHANNEL_COLORS.len();
    if cycle == 0 {
        return base;
    }
    let shift = 1.0 + (cycle as f32) * 0.15;
    [
        (base[0] * shift).min(1.0),
        (base[1] * shift).min(1.0),
        (base[2] * shift).min(1.0),
        base[3],
    ]
}

/// Desaturated channel hue for cell-frame accents: 50/50 mix of the
/// channel's `channel_color` with mid-grey. The accent reads as a
/// "channel-tinted hairline" rather than as a competing trace colour,
/// which is critical when ember substrate views render every channel
/// in the same thermal palette. Same hue source as `channel_color`
/// so RC-8's keytip pill strip can reuse this directly.
pub fn desaturated_channel_color(idx: usize) -> [f32; 4] {
    let c = channel_color(idx);
    [
        0.5 * c[0] + 0.5 * 0.5,
        0.5 * c[1] + 0.5 * 0.5,
        0.5 * c[2] + 0.5 * 0.5,
        c[3],
    ]
}

pub const GRID_LABEL_PX: f32 = 13.0;
pub const STATUS_PX: f32 = 14.0;
pub const READOUT_PX: f32 = 15.0;

/// Default y-axis dB window for the **Spectrum** (line plot) view. A
/// 120 dB span is comfortable for a line plot: clip-to-floor visible
/// in one shot, peaks aren't packed at the top edge. Colormap views
/// override this — see `default_db_window_for_view`.
pub const DEFAULT_DB_MIN: f32 = -120.0;
pub const DEFAULT_DB_MAX: f32 =    0.0;

/// Default colormap dB window for **Waterfall / CWT / CQT / Reassigned**.
/// 80 dB span centred where bench signals actually sit (typical capture
/// peaks at -10..-30 dBFS, noise floor around -90..-110 dBFS). Matches
/// the defaults REW / Smaart / ARTA ship with — wider windows make
/// every interesting signal map into one or two shades of the colormap.
pub const DEFAULT_COLORMAP_DB_MIN: f32 = -90.0;
pub const DEFAULT_COLORMAP_DB_MAX: f32 = -10.0;

/// Per-view default `(db_min, db_max)`. Applied on view switch (`W`)
/// so the user doesn't have to tweak gain on every mode they cycle
/// into. `+` / `-` (span) and `[` / `]` (floor) still override per-cell.
pub fn default_db_window_for_view(view_mode: crate::data::types::ViewMode) -> (f32, f32) {
    use crate::data::types::ViewMode;
    match view_mode {
        ViewMode::Spectrum
        | ViewMode::SpectrumEmber
        | ViewMode::Scope
        | ViewMode::Goniometer
        | ViewMode::IoTransfer
        | ViewMode::Coherence
        | ViewMode::Nyquist
        | ViewMode::Ir => (DEFAULT_DB_MIN, DEFAULT_DB_MAX),
        // Bode magnitude is a transfer-function ratio centred near
        // 0 dB (unity gain). The wide spectrum default (-120..0) would
        // pin unity to the top edge — wrong frame for distortion /
        // EQ-tweak work where the user wants to see ±N dB excursions
        // around 0. 80 dB span centred at 0 puts unity at mid-cell.
        ViewMode::BodeMag => (-40.0, 40.0),
        // Bode phase: wrapped phase domain is [-180°, +180°].
        ViewMode::BodePhase => (-180.0, 180.0),
        // Group delay in *milliseconds*. Audio-typical range is well
        // under ±20 ms; (-5, 20) covers most realistic DUTs (small
        // negative for digital chains, larger positive for crossovers
        // / room responses). Tunable via `[`/`]` and `+`/`-`.
        ViewMode::GroupDelay => (-5.0, 20.0),
        ViewMode::Waterfall => (DEFAULT_COLORMAP_DB_MIN, DEFAULT_COLORMAP_DB_MAX),
    }
}

pub const DEFAULT_FREQ_MIN: f32 = 20.0;
pub const DEFAULT_FREQ_MAX: f32 = 24000.0;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::types::ViewMode;

    #[test]
    fn line_plot_views_get_wide_window() {
        // Line plots can render wide dB ranges legibly; default to the
        // 120 dB window so a clipped peak and -110 dBFS noise floor are
        // visible at the same time.
        for v in [ViewMode::Spectrum, ViewMode::SpectrumEmber, ViewMode::Scope] {
            let (lo, hi) = default_db_window_for_view(v);
            assert!((hi - lo) >= 100.0,
                "{v:?} expected ≥100 dB span for a line-plot default, got {}",
                hi - lo);
            assert!(hi >= -10.0,
                "{v:?} top should reach near 0 dBFS so peaks aren't clipped, got {hi}");
        }
    }

    #[test]
    fn colormap_views_get_narrow_window() {
        // Colormaps need a narrower span so each shade actually
        // resolves a few dB. 80 dB is the typical analyzer default.
        let (lo, hi) = default_db_window_for_view(ViewMode::Waterfall);
        let span = hi - lo;
        assert!(
            (60.0..=100.0).contains(&span),
            "Waterfall span {span} dB out of useful colormap range (60..100)"
        );
        // Top should not sit at 0 dBFS — bench signals rarely exceed
        // -10 dBFS, and stretching the colormap that high wastes shades.
        assert!(hi <= -5.0,
            "Waterfall colormap top {hi} dBFS sets the brightest shade above typical signal levels");
    }

    #[test]
    fn families_disagree_so_view_switch_actually_does_something() {
        // The W-key handler keys off "previous default != next default"
        // to decide whether to overwrite the cell window. If the two
        // families resolved to the same pair, the auto-reset would
        // be silently inert.
        let line   = default_db_window_for_view(ViewMode::Spectrum);
        let colour = default_db_window_for_view(ViewMode::Waterfall);
        assert_ne!(line, colour,
            "line-plot and colormap defaults must differ for the auto-reset to fire");
    }
}
