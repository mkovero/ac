//! The default stimulus of `plot_ir` (#501) — the one place it is defined.
//!
//! `ac-daemon` applies these to every field a `plot_ir` request omits and
//! echoes the result in its ack; `ac-cli` keeps no copy of its own and prints
//! what the ack reports. A value changed here therefore changes the emitted
//! sweep and the printed one together.
//!
//! These defaults and [`crate::measurement::report::PRE_IMPULSE_SNR_MIN_DB`]
//! only work together. The pre-impulse figure of a clean loopback is fixed by
//! the stimulus, not by how quiet the capture was (#471, #501): at the
//! previous defaults (1 s, 4096 samples) a perfect cable read 10.8–13.2 dB at
//! 96 kHz and 14.9–17.7 dB at 48 kHz, so the 18 dB gate refused every default
//! run. At these defaults a perfect loopback reads 21.1–22.0 dB at 44.1, 48,
//! 96 and 192 kHz, and a noise-only capture stays below the gate. The test
//! module of `report::ir_stats` records that coupling; a change to either side
//! has to keep those tests green.
//!
//! **Why 4.0 s and 0.4 s go together.** `extract_irs` clamps the linear IR's
//! gate to its distance from harmonic order 2, which is
//! `duration × ln 2 / ln(f2/f1)` = 4.0 × 0.6931 / 6.9078 = 0.4014 s at these
//! values. A 0.4 s window fits under that at every sample rate, so the linear
//! IR keeps the full default gate. A shorter sweep at the same band, or a
//! longer window at this sweep, would clamp the linear IR on every default
//! run and name order 1 in the clamp note. Orders 2–5 sit closer together
//! and are clamped at these defaults, as they were at the previous ones
//! (22540 / 15992 / 12404 / 12404 samples at 96 kHz).
//!
//! **Why the window is in seconds.** The pre-impulse figure depends on the
//! window's length in time and not otherwise on the sample rate: 4096 samples
//! at 48 kHz and 8189 at 96 kHz read the same. A default in samples therefore
//! meant a different gate, and a ~4 dB different figure, at each rate. The
//! daemon converts [`IR_DEFAULT_WINDOW_S`] with [`ir_default_window_len`] once
//! the engine reports its rate. A typed window stays a sample count.

/// Lower band edge of the default sweep, Hz.
pub const IR_DEFAULT_F1_HZ: f64 = 20.0;

/// Upper band edge of the default sweep, Hz. Below Nyquist at 44.1 kHz.
pub const IR_DEFAULT_F2_HZ: f64 = 20_000.0;

/// Default sweep length, seconds (#501; was 1.0).
pub const IR_DEFAULT_DURATION_S: f64 = 4.0;

/// Default linear-IR gate length, seconds (#501; was 4096 samples).
/// Converted to samples by [`ir_default_window_len`].
pub const IR_DEFAULT_WINDOW_S: f64 = 0.4;

/// Default number of harmonic orders extracted, the linear IR included.
pub const IR_DEFAULT_N_HARMONICS: usize = 5;

/// Default capture beyond the end of the sweep, seconds.
pub const IR_DEFAULT_TAIL_S: f64 = 0.5;

/// [`IR_DEFAULT_WINDOW_S`] in samples at `sample_rate`: 19200 at 48 kHz,
/// 38400 at 96 kHz. Never 0, so it is always a valid `extract_irs` request.
pub fn ir_default_window_len(sample_rate: u32) -> usize {
    ((IR_DEFAULT_WINDOW_S * sample_rate as f64).round() as usize).max(1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::measurement::sweep::SweepParams;

    /// The coupling the module doc states: the default window fits under the
    /// order-2 spacing of the default sweep, so the linear IR is never
    /// clamped. `extract_irs` itself is exercised at four rates in
    /// `report::ir_stats`' tests; this pins the arithmetic the doc quotes.
    #[test]
    fn default_window_fits_under_the_order_two_spacing() {
        let p = SweepParams {
            f1_hz: IR_DEFAULT_F1_HZ,
            f2_hz: IR_DEFAULT_F2_HZ,
            duration_s: IR_DEFAULT_DURATION_S,
            sample_rate: 48_000,
        };
        let spacing = p.harmonic_time_offset_s(2);
        assert!(
            (spacing - 0.4014).abs() < 1e-4,
            "order-2 spacing {spacing} s"
        );
        assert!(IR_DEFAULT_WINDOW_S < spacing);
    }

    #[test]
    fn default_window_len_is_rounded_seconds() {
        assert_eq!(ir_default_window_len(44_100), 17_640);
        assert_eq!(ir_default_window_len(48_000), 19_200);
        assert_eq!(ir_default_window_len(96_000), 38_400);
        assert_eq!(ir_default_window_len(192_000), 76_800);
        assert_eq!(ir_default_window_len(1), 1, "never zero");
    }

    #[test]
    fn default_band_is_valid_at_the_lowest_supported_rate() {
        let p = SweepParams {
            f1_hz: IR_DEFAULT_F1_HZ,
            f2_hz: IR_DEFAULT_F2_HZ,
            duration_s: IR_DEFAULT_DURATION_S,
            sample_rate: 44_100,
        };
        assert!(p.validate().is_ok());
    }
}
