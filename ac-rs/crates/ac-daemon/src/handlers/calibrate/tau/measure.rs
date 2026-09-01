//! One τ reading: play a short ESS, deconvolve, and report where the
//! linear-IR peak sits — or refuse, when the peak is close enough to the
//! window edge that "outside the window" and "at this position" are the
//! same picture (#340).
//!
//! Everything about the *window* lives here — its size in time, the
//! edge margin, and the rig-instrument overrides for both. Whether one
//! reading counts as a measurement is a separate question, answered by
//! the parent module.

use ac_core::measurement::sweep::{
    deconvolve_full, extract_irs, inverse_sweep, log_sweep, SweepParams,
};

use crate::audio::AudioEngine;

/// Short-ESS parameters for the τ measurement (#281). Deliberately much
/// shorter than a `plot_ir` sweep — this only has to locate the linear-IR
/// peak, not resolve harmonics or a decay tail, so `n_harmonics = 1` on
/// the `extract_irs` call below. That matters for the window bound: with
/// `n_harmonics == 1` there is no neighbouring order, so
/// `per_order_window_lens`'s harmonic-gap clamp (`sweep/harmonics.rs`) never runs —
/// it does not bind here, only on multi-harmonic callers like `plot_ir`.
///
/// The bound that actually fires is the requested window itself, so it
/// must be sized directly to the round trip this needs to measure rather
/// than left as a fixed sample count (#340): `TAU_WINDOW_LEN` used to be a
/// `usize`, so its *time* value shrank as sample rate rose — 85 ms at
/// 48 kHz (looked generous) but only 42.67 ms at 96 kHz, 1.03× short of
/// this rig's measured 43.75 ms τ, and wrong at every rate in between.
/// `TAU_MIN_HALF_WINDOW_S` fixes that: it is a *time* bound, converted to
/// a sample window at measurement time, so the measurable ceiling is the
/// same 50 ms at every sample rate. `TAU_TAIL_S` (150 ms) stays well
/// above it so the capture always holds the whole window regardless of
/// sample rate.
const TAU_F1_HZ: f64 = 100.0;
const TAU_DURATION_S: f64 = 0.2;
const TAU_TAIL_S: f64 = 0.15;
/// Half-width of the τ measurement window, in seconds (AC5 of #340). The
/// largest round trip `measure_tau` can report is just under this value.
const TAU_MIN_HALF_WINDOW_S: f64 = 0.05;
/// Fraction of the half-window treated as "too close to the edge to
/// trust" (AC4 of #340). A peak this close to either edge is
/// indistinguishable from one pinned by an arrival outside the window
/// entirely, so it is refused rather than reported. Not derived from rig
/// data — a threshold newly introduced by this change; may need revisiting
/// once measured against real noise floors.
const TAU_EDGE_MARGIN_FRAC: f64 = 0.10;

/// Rig-instrument overrides for the two τ window constants (#350).
///
/// Compiled in only under the `tau-window-override` feature, which is off
/// by default, so a production daemon cannot be perturbed by its
/// environment. The rig needs them because the only lever hardware has on
/// edge proximity is τ itself, and τ moves in period-sized steps
/// (44.5 %, 33.8 %, 12.5 %, then off the end of the window) — there is no
/// way to sample between 0 % and the shipped 10 % margin that way. Moving
/// the *window* while the round trip stays fixed samples it continuously,
/// inside one JACK client lifetime, which also keeps #347's one-period
/// jump out of the comparison.
#[cfg(feature = "tau-window-override")]
fn tau_env_f64(key: &str, default: f64) -> f64 {
    let raw = match std::env::var(key) {
        Ok(v) => v,
        Err(_) => return default,
    };
    match raw.trim().parse::<f64>() {
        Ok(x) if x.is_finite() && x >= 0.0 => x,
        _ => {
            eprintln!(
                "calibrate: {key}={raw:?} is not a finite non-negative number —                  ignoring it and using {default}"
            );
            default
        }
    }
}

#[cfg(feature = "tau-window-override")]
fn tau_half_window_s() -> f64 {
    tau_env_f64("AC_TAU_HALF_WINDOW_S", TAU_MIN_HALF_WINDOW_S)
}

#[cfg(not(feature = "tau-window-override"))]
fn tau_half_window_s() -> f64 {
    TAU_MIN_HALF_WINDOW_S
}

#[cfg(feature = "tau-window-override")]
fn tau_edge_margin_frac() -> f64 {
    tau_env_f64("AC_TAU_EDGE_MARGIN_FRAC", TAU_EDGE_MARGIN_FRAC)
}

#[cfg(not(feature = "tau-window-override"))]
fn tau_edge_margin_frac() -> f64 {
    TAU_EDGE_MARGIN_FRAC
}

/// Per-reading τ diagnostic (#350). `measure_tau` reports only the peak
/// position, so nothing on this path has ever recorded the SNR the peak
/// was located against — which is the quantity #350 exists to measure.
/// `floor` is defined exactly as `it_loopback_ir` and `ir_probe` define
/// it (max |x| over the leading eighth of the window) so the numbers
/// compare directly against #277's record.
#[cfg(feature = "tau-window-override")]
fn tau_probe_log(
    ir: &[f64],
    peak_idx: usize,
    peak_abs: f64,
    window_len: usize,
    half: usize,
    sr: u32,
) {
    let far_end = (ir.len() / 8).max(1);
    let floor = ir[..far_end]
        .iter()
        .map(|v| v.abs())
        .fold(0.0_f64, f64::max);
    let snr_db = 20.0 * (peak_abs / floor.max(1e-15)).log10();
    let margin_frac = tau_edge_margin_frac();
    let margin = (margin_frac * half as f64).round() as usize;
    let dist_from_end = window_len.saturating_sub(1).saturating_sub(peak_idx);
    let edge_frac = dist_from_end as f64 / half as f64;
    let offset = peak_idx as i64 - half as i64;
    eprintln!("--- tau probe (#350) ---");
    eprintln!("sample_rate:   {sr} Hz");
    eprintln!(
        "half_window:   {half} samples = {:.4} ms",
        half as f64 * 1000.0 / sr as f64
    );
    eprintln!("window_len:    {window_len} samples");
    eprintln!("peak_index:    {peak_idx}");
    eprintln!("peak_abs:      {peak_abs:.6e}");
    eprintln!("floor_abs:     {floor:.6e}  (max |x| over leading {far_end} samples)");
    eprintln!("snr_db:        {snr_db:.2}");
    eprintln!("dist_from_end: {dist_from_end} samples");
    eprintln!("edge_frac:     {edge_frac:.4}  (margin_frac {margin_frac} = {margin} samples)");
    eprintln!(
        "tau:           {offset:+} samples = {:+.4} ms",
        offset as f64 * 1000.0 / sr as f64
    );
    eprintln!("------------------------");
}

/// Refuse a peak sitting within `margin_frac` of the half-window of
/// either edge of a `window_len`-sample gate. Pulled out of `measure_tau`
/// so the edge case can be driven directly in tests without an
/// `AudioEngine` (#340 AC4).
fn check_peak_within_window(
    peak_idx: usize,
    window_len: usize,
    margin_frac: f64,
) -> anyhow::Result<()> {
    let half = window_len / 2;
    let margin = (margin_frac * half as f64).round() as usize;
    let dist_from_start = peak_idx;
    let dist_from_end = window_len.saturating_sub(1).saturating_sub(peak_idx);
    if dist_from_start <= margin || dist_from_end <= margin {
        anyhow::bail!(
            "\u{3c4} peak at sample {peak_idx} of a {window_len}-sample window (half-width \
             {half} samples) sits within {margin} samples of the window edge — the arrival is \
             likely outside the window rather than at this position, so no value is reported"
        );
    }
    Ok(())
}

/// Play a short ESS, deconvolve it, and return the interface round-trip
/// delay in seconds (peak of the linear IR, converted from samples).
///
/// Reuses the Farina machinery from `ac_core::measurement::sweep` exactly
/// as `plot_ir` does — see `handlers/audio/plot.rs` for the longer-form
/// version of the same technique.
pub(super) fn measure_tau(eng: &mut dyn AudioEngine, amp: f64) -> anyhow::Result<f64> {
    let sr = eng.sample_rate();
    let f2_hz = (sr as f64 * 0.45).min(20_000.0);
    let params = SweepParams {
        f1_hz: TAU_F1_HZ,
        f2_hz,
        duration_s: TAU_DURATION_S,
        sample_rate: sr,
    };
    let sweep = log_sweep(&params)?;
    let amp = amp as f32;
    let scaled: Vec<f32> = sweep.iter().map(|&s| s * amp).collect();
    let captured = eng.play_and_capture(&scaled, TAU_TAIL_S)?;
    let inv = inverse_sweep(&params)?;
    let full = deconvolve_full(&captured, &inv);
    let half_window_s = tau_half_window_s();
    if 2.0 * half_window_s > TAU_TAIL_S {
        anyhow::bail!(
            "\u{3c4} half-window {half_window_s} s needs a {:.4} s gate but the capture tail is \
             only {TAU_TAIL_S} s \u{2014} the window would run off the end of the capture",
            2.0 * half_window_s
        );
    }
    let half = (half_window_s * sr as f64).ceil() as usize;
    let window_len = 2 * half;
    let irs = extract_irs(&full, &params, 1, window_len)?;
    let (peak_idx, peak_val) = irs
        .linear
        .iter()
        .enumerate()
        .map(|(i, v)| (i, *v))
        .max_by(|a, b| a.1.abs().partial_cmp(&b.1.abs()).unwrap())
        .ok_or_else(|| anyhow::anyhow!("empty IR from τ sweep"))?;
    #[cfg(feature = "tau-window-override")]
    tau_probe_log(&irs.linear, peak_idx, peak_val.abs(), window_len, half, sr);
    #[cfg(not(feature = "tau-window-override"))]
    let _ = peak_val;
    check_peak_within_window(peak_idx, window_len, tau_edge_margin_frac())?;
    let offset_samples = peak_idx as i64 - half as i64;
    Ok(offset_samples as f64 / sr as f64)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #340 AC4/AC-test: a peak pinned at the window's far edge — exactly
    /// the shape #277 measured — must be refused, not converted into the
    /// plausible-looking offset the old, unguarded code would have
    /// returned. Compute that pinned value here, the way `measure_tau`
    /// used to unconditionally, and confirm the guard fires before it
    /// would ever reach the caller.
    #[test]
    fn check_peak_within_window_refuses_peak_pinned_at_edge() {
        let window_len = 9600; // 2 * 4800, i.e. a 50 ms half-window @ 96 kHz
        let half = window_len / 2;
        let peak_idx = window_len - 1; // pinned against the far edge
        let pinned_offset_samples = peak_idx as i64 - half as i64; // what the old code returned
        assert_eq!(pinned_offset_samples, half as i64 - 1);

        let result = check_peak_within_window(peak_idx, window_len, TAU_EDGE_MARGIN_FRAC);
        assert!(
            result.is_err(),
            "peak pinned at the window edge must be refused, not silently reported as offset {pinned_offset_samples}"
        );
    }

    /// A peak exactly `margin_frac` of the half-window from the start edge
    /// is refused — the margin boundary itself counts as "too close", per
    /// the architect note's instruction to test the boundary directly.
    #[test]
    fn check_peak_within_window_refuses_at_margin_boundary() {
        let window_len = 100;
        let half = window_len / 2;
        let margin = (TAU_EDGE_MARGIN_FRAC * half as f64).round() as usize;

        assert!(check_peak_within_window(margin, window_len, TAU_EDGE_MARGIN_FRAC).is_err());
        assert!(check_peak_within_window(
            window_len - 1 - margin,
            window_len,
            TAU_EDGE_MARGIN_FRAC
        )
        .is_err());
    }

    /// A peak safely inside the window, away from either edge, is
    /// accepted — the guard must not refuse the ordinary, correctly
    /// measured case.
    #[test]
    fn check_peak_within_window_accepts_interior_peak() {
        let window_len = 9600;
        let half = window_len / 2;
        assert!(check_peak_within_window(half, window_len, TAU_EDGE_MARGIN_FRAC).is_ok());
    }

    /// One sample outside the margin boundary must be accepted — pairs
    /// with `check_peak_within_window_refuses_at_margin_boundary` to pin
    /// the guard as off-by-zero (refuses exactly at the boundary, accepts
    /// exactly past it) rather than off-by-one in either direction.
    #[test]
    fn check_peak_within_window_accepts_one_sample_past_margin_boundary() {
        let window_len = 100;
        let half = window_len / 2;
        let margin = (TAU_EDGE_MARGIN_FRAC * half as f64).round() as usize;

        assert!(check_peak_within_window(margin + 1, window_len, TAU_EDGE_MARGIN_FRAC).is_ok());
        assert!(check_peak_within_window(
            window_len - 1 - margin - 1,
            window_len,
            TAU_EDGE_MARGIN_FRAC
        )
        .is_ok());
    }

    /// #340's own motivating number: the rig's measured 43.75 ms round trip
    /// (4200 samples at 96 kHz) must clear the edge guard, not just a
    /// dead-centre synthetic peak. Locks in the margin the fix actually
    /// buys at the operating point that motivated the issue (QA correctness
    /// issue 2).
    #[test]
    fn check_peak_within_window_accepts_the_rigs_measured_round_trip() {
        let window_len = 9600; // 50 ms half-window @ 96 kHz
        let half = window_len / 2;
        let offset_samples = 4200i64; // rig's measured tau, 96 kHz (#340/#277)
        let peak_idx = (half as i64 + offset_samples) as usize;
        assert!(
            check_peak_within_window(peak_idx, window_len, TAU_EDGE_MARGIN_FRAC).is_ok(),
            "the rig's own measured round trip must be inside the accepted window"
        );
    }

    /// Coupled-constant guard (QA correctness issue 1): `TAU_TAIL_S` must
    /// stay ahead of `TAU_MIN_HALF_WINDOW_S` with margin, or the capture
    /// can no longer hold the whole gate at high sample rates. Fails on a
    /// wrong pair (constants inverted) and on an unscored gap (no
    /// headroom) rather than passing silently either way.
    // Both bounds below compare two `const`s, so clippy sees a
    // compile-time-knowable value and suggests a `const {}` assertion
    // block instead. That would turn this into a build-time check instead
    // of a `cargo test` result — deliberately kept as a runtime test (the
    // shape the coupled-constants rule asks for) so it shows up in test
    // output like the rest of the suite, not just as a build failure.
    #[allow(clippy::assertions_on_constants)]
    #[test]
    fn tau_tail_s_clears_tau_min_half_window_s_with_margin() {
        assert!(
            TAU_TAIL_S > TAU_MIN_HALF_WINDOW_S,
            "capture tail ({TAU_TAIL_S}s) must exceed the half-window \
             ({TAU_MIN_HALF_WINDOW_S}s) or the gate can run off the end of the capture"
        );
        // Headroom bound: the doc comment on TAU_TAIL_S claims it "stays
        // well above" the half-window — fail if a future edit erodes that
        // below the 2x this test treats as the floor for "well above" (a
        // future edit to either constant must re-justify the number, not
        // silently drift under it).
        assert!(
            TAU_TAIL_S >= 2.0 * TAU_MIN_HALF_WINDOW_S,
            "tail no longer clears the half-window with the margin the doc comment assumed"
        );
    }

    /// The rig override parser (#350) accepts a plain number and refuses
    /// anything else — a typo must not silently become a different window
    /// than the one the run sheet says was used, which would make the
    /// recorded edge fraction wrong rather than missing. Uses keys no
    /// other test reads so the process-wide environment stays shared-safe.
    #[cfg(feature = "tau-window-override")]
    #[test]
    fn tau_env_override_parses_or_falls_back_to_the_compiled_constant() {
        assert_eq!(
            tau_env_f64("AC_TAU_TEST_UNSET_KEY_350", 0.05),
            0.05,
            "an unset variable must leave the compiled-in constant in place"
        );
        std::env::set_var("AC_TAU_TEST_GOOD_KEY_350", " 0.04862 ");
        assert_eq!(tau_env_f64("AC_TAU_TEST_GOOD_KEY_350", 0.05), 0.04862);
        for bad in ["", "48.62ms", "-0.01", "nan", "inf"] {
            std::env::set_var("AC_TAU_TEST_BAD_KEY_350", bad);
            assert_eq!(
                tau_env_f64("AC_TAU_TEST_BAD_KEY_350", 0.05),
                0.05,
                "{bad:?} is not a usable window and must fall back, not be coerced"
            );
        }
    }
}
