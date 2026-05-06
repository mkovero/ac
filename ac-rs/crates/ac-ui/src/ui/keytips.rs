//! Bottom keytip strip — RC-8, plan §4. One line at the bottom of the
//! window that lists the 3–6 most relevant keys for the current view,
//! each chip pairing the keystroke with its current state (e.g.
//! `A weighting:Z`, `O smooth:1/6`).
//!
//! The strip is build-and-render in two stages so the per-view chip
//! mapping is unit-testable without spinning up `App`:
//!
//! - `KeytipState` is a snapshot of the live state pulled at `OverlayInput`
//!   build time;
//! - `keytips_for(state)` returns the chips, no I/O;
//! - the painter side (in `overlay.rs`) just lays them out.

use crate::app::{BandWeighting, TimeIntegrationMode};
use crate::data::types::ViewMode;

/// One keytip on the bottom strip. `key` is the bare keystroke (e.g.
/// `"A"`, `","/"."`); `label` is the contextual descriptor including
/// any current-state suffix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeytipChip {
    pub key: &'static str,
    pub label: String,
}

/// Live state snapshot consumed by [`keytips_for`]. Kept as a flat
/// struct of POD values so the snapshot can be lifted off `App`
/// without grabbing references that complicate the overlay's borrow
/// scope.
#[derive(Debug, Clone, Copy)]
pub struct KeytipState {
    pub view: ViewMode,
    pub band_weighting: BandWeighting,
    pub time_integ: TimeIntegrationMode,
    pub smoothing_frac: Option<u32>,
    pub peak_hold: bool,
    pub min_hold: bool,
    pub coherence_k: f32,
    pub goniometer_ms: bool,
}

fn weighting_label(w: BandWeighting) -> &'static str {
    match w {
        BandWeighting::Off => "off",
        BandWeighting::A => "A",
        BandWeighting::C => "C",
        BandWeighting::Z => "Z",
    }
}

fn time_label(t: TimeIntegrationMode) -> &'static str {
    match t {
        TimeIntegrationMode::Off => "off",
        TimeIntegrationMode::Fast => "fast",
        TimeIntegrationMode::Slow => "slow",
        TimeIntegrationMode::Leq => "Leq",
    }
}

fn smooth_label(s: Option<u32>) -> String {
    match s {
        None => "off".to_string(),
        Some(n) => format!("1/{n}"),
    }
}

fn on_off(b: bool) -> &'static str {
    if b {
        "on"
    } else {
        "off"
    }
}

fn chip(key: &'static str, label: impl Into<String>) -> KeytipChip {
    KeytipChip {
        key,
        label: label.into(),
    }
}

/// Universal chips appended to every view's strip. `G matrix`, `H help`,
/// `S screenshot`, `Esc quit` are always available regardless of view.
/// `G` snaps to the Spectrum+Grid overview for picking a channel.
fn universal_chips() -> Vec<KeytipChip> {
    vec![
        chip("G", "matrix"),
        chip("H", "help"),
        chip("S", "screenshot"),
        chip("Esc", "quit"),
    ]
}

/// Per-view chip set — implements the §4 binding table. Universal chips
/// are appended last so they line up at the right edge of the strip.
pub fn keytips_for(state: &KeytipState) -> Vec<KeytipChip> {
    let mut chips: Vec<KeytipChip> = match state.view {
        ViewMode::SpectrumEmber => vec![
            chip("A", format!("weighting:{}", weighting_label(state.band_weighting))),
            chip("I", format!("avg:{}", time_label(state.time_integ))),
            chip("O", format!("smooth:{}", smooth_label(state.smoothing_frac))),
            chip(
                "P/M",
                format!("peak:{} min:{}", on_off(state.peak_hold), on_off(state.min_hold)),
            ),
            chip(",/.", "ember"),
            chip("W", "view"),
        ],
        ViewMode::Waterfall => vec![
            chip("A", format!("weighting:{}", weighting_label(state.band_weighting))),
            chip("O", format!("smooth:{}", smooth_label(state.smoothing_frac))),
            chip("↑↓", "FFT N"),
            chip("←→", "interval"),
            chip(";", "palette"),
            chip("W", "view"),
        ],
        ViewMode::Scope => vec![
            chip(",/.", "intensity"),
            chip("Sh+,/.", "τ_p"),
            chip("Z", "clear"),
            chip("W", "view"),
        ],
        ViewMode::Goniometer => vec![
            chip("R", if state.goniometer_ms { "M/S:on" } else { "M/S:off" }),
            chip(",/.", "ember"),
            chip("Z", "clear"),
            chip("W", "view"),
        ],
        ViewMode::IoTransfer => vec![
            chip(",/.", "ember"),
            chip("Z", "clear"),
            chip("W", "view"),
        ],
        ViewMode::BodeMag => vec![
            chip("K", format!("γ²-weight:{:.1}", state.coherence_k)),
            chip("O", format!("smooth:{}", smooth_label(state.smoothing_frac))),
            chip("Z", "clear"),
            chip("T", "transfer"),
            chip("W", "view"),
        ],
        ViewMode::BodePhase => vec![
            chip("K", format!("γ²-weight:{:.1}", state.coherence_k)),
            chip("T", "transfer"),
            chip("Z", "clear"),
            chip("W", "view"),
        ],
        ViewMode::Coherence => vec![
            chip("K", format!("γ²-weight:{:.1}", state.coherence_k)),
            chip("T", "transfer"),
            chip("W", "view"),
        ],
        ViewMode::GroupDelay => vec![
            chip("K", format!("γ²-weight:{:.1}", state.coherence_k)),
            chip("T", "transfer"),
            chip("W", "view"),
        ],
        ViewMode::Nyquist => vec![
            chip("K", format!("γ²-weight:{:.1}", state.coherence_k)),
            chip("T", "transfer"),
            chip("W", "view"),
        ],
        ViewMode::Ir => vec![
            chip("T", "transfer"),
            chip("Z", "clear"),
            chip("W", "view"),
        ],
        ViewMode::Spectrum => vec![
            chip("A", format!("weighting:{}", weighting_label(state.band_weighting))),
            chip("O", format!("smooth:{}", smooth_label(state.smoothing_frac))),
            chip("W", "view"),
        ],
    };
    chips.extend(universal_chips());
    chips
}

/// Render the chip list as a single space-separated line. Each chip
/// renders as `key label` joined by ` · `; the universal chips at the
/// end are visually identical so the painter doesn't have to special-
/// case them.
pub fn format_strip(chips: &[KeytipChip]) -> String {
    chips
        .iter()
        .map(|c| {
            if c.label.is_empty() {
                c.key.to_string()
            } else {
                format!("{} {}", c.key, c.label)
            }
        })
        .collect::<Vec<_>>()
        .join(" · ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_state(view: ViewMode) -> KeytipState {
        KeytipState {
            view,
            band_weighting: BandWeighting::Off,
            time_integ: TimeIntegrationMode::Off,
            smoothing_frac: None,
            peak_hold: false,
            min_hold: false,
            coherence_k: 2.0,
            goniometer_ms: true,
        }
    }

    /// Every default view in the W-cycle returns at least the universal
    /// chips (G/H/S/Esc) plus its own. None must be empty.
    #[test]
    fn every_view_returns_chips() {
        let views = [
            ViewMode::SpectrumEmber, ViewMode::Goniometer, ViewMode::IoTransfer,
            ViewMode::BodeMag, ViewMode::Coherence, ViewMode::BodePhase,
            ViewMode::GroupDelay, ViewMode::Nyquist, ViewMode::Ir,
            ViewMode::Waterfall, ViewMode::Scope, ViewMode::Spectrum,
        ];
        for v in views {
            let chips = keytips_for(&base_state(v));
            assert!(chips.len() >= 5, "view {v:?} produced too few chips: {chips:?}");
            // Universal chips must always be present.
            assert!(chips.iter().any(|c| c.key == "G"));
            assert!(chips.iter().any(|c| c.key == "H"));
            assert!(chips.iter().any(|c| c.key == "Esc"));
        }
    }

    /// State reflection: changing `band_weighting` flips the Spectrum
    /// chip's state suffix from `off` → `A` etc.
    #[test]
    fn weighting_state_appears_in_label() {
        let mut s = base_state(ViewMode::SpectrumEmber);
        s.band_weighting = BandWeighting::A;
        let chips = keytips_for(&s);
        let a = chips.iter().find(|c| c.key == "A").expect("A chip");
        assert!(a.label.contains("A"), "expected A in {:?}", a.label);
        assert!(!a.label.contains("off"));
    }

    /// Smoothing chip toggles between `off` and `1/N` based on state.
    #[test]
    fn smoothing_chip_reflects_fraction() {
        let mut s = base_state(ViewMode::SpectrumEmber);
        s.smoothing_frac = Some(6);
        let chips = keytips_for(&s);
        let o = chips.iter().find(|c| c.key == "O").expect("O chip");
        assert!(o.label.contains("1/6"), "expected 1/6 in {:?}", o.label);

        s.smoothing_frac = None;
        let chips = keytips_for(&s);
        let o = chips.iter().find(|c| c.key == "O").expect("O chip");
        assert!(o.label.contains("off"));
    }

    /// `format_strip` joins chips with the ` · ` separator and never
    /// drops one — the painter ships the strip as a single shaped run.
    #[test]
    fn format_strip_joins_with_separator() {
        let chips = keytips_for(&base_state(ViewMode::Nyquist));
        let line = format_strip(&chips);
        // K, T, W, H, S, Esc all present at minimum.
        for needle in ["K ", "T ", "W ", "H ", "S ", "Esc "] {
            assert!(line.contains(needle), "missing {needle:?} in {line:?}");
        }
        // Separator appears N-1 times in N chips.
        let sep_count = line.matches(" · ").count();
        assert_eq!(sep_count, chips.len() - 1);
    }
}
