//! Cursor and SPL readout formatting (deliverable 4). Formatting rules
//! are part of the contract, written down here rather than left
//! implicit in a call site:
//!
//! - Levels are formatted to 2 decimal places (`{:.2}`) — matches this
//!   project's established `-6.75 dB`-class precision convention
//!   (`qa-signoff-m1.5.md`'s fixture derivation), reused here rather
//!   than picking a new one.
//! - The reference label (`dBFS` vs `dB SPL`) is decided **only** by
//!   whether an SPL calibration layer is present (`spl.is_some()` on
//!   the input) — nothing else, per AC5.
//! - `spl`'s own value is voltage-cal-independent by design (parallel
//!   calibration layers off raw digital amplitude — see
//!   `ac_core::shared::calibration`'s module doc). This crate only
//!   formats the number; it doesn't re-derive it, so that guarantee
//!   lives entirely in `ac-core` and `ac-daemon`, not here.
//! - The SPL readout's weighting/integration tags are echoed verbatim
//!   from the frame — never renamed or re-derived. When the input has
//!   no integration tag (the snapshot-derived path — architect review,
//!   decision 3), the readout omits the integration clause entirely
//!   rather than fabricating one (decision 3a).
//! - Cursor frequency is formatted to **whole Hz, no decimals** (UX
//!   review on `handoff-ac-scene.md`) — the value names a log-spaced
//!   *column* (D18), not a single bin; at 1 kHz with 48 cols/octave the
//!   column is ~15 Hz wide, wider at higher frequencies, so any
//!   sub-Hz precision would claim resolution the column geometry
//!   doesn't have. This is a display-precision decision only — the
//!   underlying `f64` frequency and level values are unchanged and
//!   still QA-verified to full precision; only their rendering is
//!   capped here.

use ac_core::visualize::weighting_curves::WeightingCurve;

/// `"{value:.2} dB SPL (A, fast)"` or, with no integration tag (a
/// snapshot-derived scene), `"{value:.2} dB SPL (A)"`. Returns `None`
/// when `spl` is `None` (no SPL calibration layer) — there is nothing
/// to read out.
pub fn format_spl_readout(
    spl: Option<f64>,
    weighting: WeightingCurve,
    integration: Option<&str>,
) -> Option<String> {
    let spl = spl?;
    Some(match integration {
        Some(integ) => format!("{spl:.2} dB SPL ({}, {integ})", weighting.tag()),
        None => format!("{spl:.2} dB SPL ({})", weighting.tag()),
    })
}

/// `"{freq_hz:.0} Hz: {level:.2} dBFS"` or `"... dB SPL"` — the label is
/// decided purely by `has_spl_cal` (AC5), independent of the numeric
/// level shown (which is always the column's own band level, not the
/// broadband `spl` scalar). Frequency is whole Hz (UX review) — the
/// value names a column, not a bin; the level keeps its established
/// 2-decimal precision (a single scalar reading, not a band label).
pub fn format_cursor_readout(freq_hz: f64, level_db: f64, has_spl_cal: bool) -> String {
    let unit = if has_spl_cal { "dB SPL" } else { "dBFS" };
    format!("{freq_hz:.0} Hz: {level_db:.2} {unit}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spl_readout_none_when_no_spl_cal() {
        assert_eq!(
            format_spl_readout(None, WeightingCurve::A, Some("fast")),
            None
        );
    }

    #[test]
    fn spl_readout_with_integration_tag() {
        assert_eq!(
            format_spl_readout(Some(72.3), WeightingCurve::A, Some("fast")),
            Some("72.30 dB SPL (A, fast)".to_string())
        );
    }

    #[test]
    fn spl_readout_without_integration_tag() {
        assert_eq!(
            format_spl_readout(Some(-6.75), WeightingCurve::Z, None),
            Some("-6.75 dB SPL (Z)".to_string())
        );
    }

    #[test]
    fn cursor_readout_labels() {
        assert_eq!(
            format_cursor_readout(1000.0, -6.75, false),
            "1000 Hz: -6.75 dBFS"
        );
        assert_eq!(
            format_cursor_readout(1000.0, -6.75, true),
            "1000 Hz: -6.75 dB SPL"
        );
    }
}
