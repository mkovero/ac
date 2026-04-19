use crate::data::types::SweepPoint;
use crate::ui::overlay::HoverReadout;

/// Broadband statistics summarising a live spectrum for the monitor
/// bottom-left readout. Derived directly from the displayed dB-magnitude
/// array — no assumption that the signal is a single tone, so THD numbers
/// (which require a known fundamental) are deliberately omitted.
#[derive(Debug, Clone, Copy)]
pub struct BroadbandStats {
    /// Peak bin value in dBFS.
    pub peak_db:  f32,
    /// Frequency of the peak bin.
    pub peak_hz:  f32,
    /// 10th-percentile of all finite bins — an estimate of the noise floor
    /// that's robust to a handful of bright peaks.
    pub floor_db: f32,
    /// `peak_db - floor_db` — dynamic range of the visible spectrum. A
    /// clean tone reads 80+ dB; broadband noise reads 20–30 dB.
    pub span_db:  f32,
}

/// Compute peak / floor / span from a dB-magnitude spectrum and its frequency
/// grid. Returns `None` for empty inputs or all-NaN spectra. Operates on the
/// post-smoothing values so the readout matches what's visually on screen.
pub fn broadband_stats(spectrum: &[f32], freqs: &[f32]) -> Option<BroadbandStats> {
    let n = spectrum.len().min(freqs.len());
    if n == 0 {
        return None;
    }
    let mut peak_db = f32::NEG_INFINITY;
    let mut peak_idx = 0usize;
    let mut finite: Vec<f32> = Vec::with_capacity(n);
    for i in 0..n {
        let v = spectrum[i];
        if !v.is_finite() {
            continue;
        }
        if v > peak_db {
            peak_db = v;
            peak_idx = i;
        }
        finite.push(v);
    }
    if finite.is_empty() {
        return None;
    }
    // 10th-percentile floor: a single sort is O(n log n), negligible for the
    // few-thousand-bin spectra the UI works with. `partial_cmp` can't fail
    // since NaNs are already filtered.
    finite.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let floor_idx = (finite.len() as f32 * 0.10) as usize;
    let floor_db = finite[floor_idx.min(finite.len() - 1)];
    Some(BroadbandStats {
        peak_db,
        peak_hz: freqs[peak_idx],
        floor_db,
        span_db: peak_db - floor_db,
    })
}

/// Primary monitor readout (bottom-left overlay). Shows broadband stats
/// derived from the displayed spectrum rather than THD — THD is only
/// meaningful when a known pure tone is driving the system (sweep / thd
/// commands), not in live monitoring.
pub fn spectrum_readout(stats: &BroadbandStats, in_dbu: Option<f32>) -> String {
    let dbu = in_dbu
        .map(|v| format!("   {:+.1} dBu", v))
        .unwrap_or_default();
    format!(
        "peak {:>6.1} dBFS @ {}  │  floor {:>6.1} dBFS  │  span {:>5.1} dB{}",
        stats.peak_db,
        format_hz(stats.peak_hz).trim(),
        stats.floor_db,
        stats.span_db,
        dbu,
    )
}

/// Transfer delay readout (top center).
pub fn transfer_delay(delay_ms: f32, delay_samples: i64) -> String {
    format!("Δt = {:+.2} ms  ({:+} samp)", delay_ms, delay_samples)
}

/// Sweep point readout (bottom bar in sweep layout).
pub fn sweep_readout(pt: &SweepPoint) -> String {
    let mut parts = Vec::new();
    parts.push(format!("{:.1} Hz", pt.fundamental_hz));
    parts.push(format!("THD {:.4}%", pt.thd_pct));
    parts.push(format!("THD+N {:.4}%", pt.thdn_pct));
    if let Some(g) = pt.gain_db {
        parts.push(format!("Gain {:+.2} dB", g));
    }
    parts.push(format!("Fund {:.1} dBFS", pt.fundamental_dbfs));
    if let Some(dbu) = pt.in_dbu {
        parts.push(format!("In {:+.2} dBu", dbu));
    }
    if let Some(dbu) = pt.out_dbu {
        parts.push(format!("Out {:+.2} dBu", dbu));
    }
    parts.join("   ")
}

/// Hover crosshair readout label.
pub fn hover_label(channel: usize, freq_hz: f32, readout: &HoverReadout) -> String {
    match readout {
        HoverReadout::Db(v) => format!(
            "CH{} {} {:+6.1} dB",
            channel,
            format_hz(freq_hz),
            v,
        ),
        HoverReadout::Phase(v) => format!(
            "CH{} {} {:+6.1} deg",
            channel,
            format_hz(freq_hz),
            v,
        ),
        HoverReadout::Coherence(v) => format!(
            "CH{} {} coh {:.3}",
            channel,
            format_hz(freq_hz),
            v,
        ),
        HoverReadout::Thd(v) => format!(
            "{} THD {:.4}%",
            format_hz(freq_hz),
            v,
        ),
        HoverReadout::Gain(v) => format!(
            "{} {:+.2} dB",
            format_hz(freq_hz),
            v,
        ),
    }
}

/// Format a frequency value for display.
pub fn format_hz(hz: f32) -> String {
    if hz >= 1000.0 {
        format!("{:>6.2} kHz", hz / 1000.0)
    } else {
        format!("{:>6.1} Hz ", hz)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::types::SweepPoint;

    fn test_sweep_point(freq: f32, thd: f32, thdn: f32) -> SweepPoint {
        SweepPoint {
            n: 1,
            drive_db: 0.0,
            thd_pct: thd,
            thdn_pct: thdn,
            fundamental_hz: freq,
            fundamental_dbfs: -3.0,
            harmonic_levels: vec![],
            spectrum: vec![],
            freqs: vec![],
            clipping: false,
            out_dbu: None,
            in_dbu: None,
            gain_db: None,
        }
    }

    // ── broadband_stats + spectrum_readout ────────────────────────────

    fn mk_spec(peak_idx: usize, peak: f32, floor: f32, n: usize) -> (Vec<f32>, Vec<f32>) {
        let mut spec = vec![floor; n];
        spec[peak_idx] = peak;
        let freqs: Vec<f32> = (0..n).map(|i| i as f32 * 24_000.0 / (n - 1) as f32).collect();
        (spec, freqs)
    }

    #[test]
    fn broadband_stats_finds_peak() {
        let (spec, freqs) = mk_spec(100, -3.0, -90.0, 1024);
        let s = broadband_stats(&spec, &freqs).unwrap();
        assert!((s.peak_db - -3.0).abs() < 1e-4);
        assert!((s.peak_hz - freqs[100]).abs() < 1e-4);
        // With one bright peak in 1024 bins, 10th percentile is the floor.
        assert!((s.floor_db - -90.0).abs() < 1e-4);
        assert!((s.span_db - 87.0).abs() < 1e-4);
    }

    #[test]
    fn broadband_stats_empty_is_none() {
        assert!(broadband_stats(&[], &[]).is_none());
    }

    #[test]
    fn broadband_stats_skips_non_finite() {
        let spec = vec![f32::NAN, -40.0, -20.0, f32::NEG_INFINITY];
        let freqs = vec![0.0, 100.0, 200.0, 300.0];
        let s = broadband_stats(&spec, &freqs).unwrap();
        assert!((s.peak_db - -20.0).abs() < 1e-4);
        assert!((s.peak_hz - 200.0).abs() < 1e-4);
    }

    #[test]
    fn spectrum_readout_contains_peak_floor_span() {
        let stats = BroadbandStats {
            peak_db: -3.0,
            peak_hz: 1000.0,
            floor_db: -96.0,
            span_db: 93.0,
        };
        let s = spectrum_readout(&stats, None);
        assert!(s.contains("peak"));
        assert!(s.contains("-3.0 dBFS"));
        assert!(s.contains("1.00 kHz"));
        assert!(s.contains("floor"));
        assert!(s.contains("-96.0 dBFS"));
        assert!(s.contains("span"));
        assert!(s.contains("93.0 dB"));
    }

    #[test]
    fn spectrum_readout_no_thd_nomencalture() {
        // THD / THD+N are meaningless on broadband signals, so they must not
        // appear in the monitor readout.
        let stats = BroadbandStats {
            peak_db: -3.0,
            peak_hz: 1000.0,
            floor_db: -96.0,
            span_db: 93.0,
        };
        let s = spectrum_readout(&stats, None);
        assert!(!s.contains("THD"));
    }

    #[test]
    fn spectrum_readout_with_dbu() {
        let stats = BroadbandStats {
            peak_db: -3.0,
            peak_hz: 1000.0,
            floor_db: -96.0,
            span_db: 93.0,
        };
        let s = spectrum_readout(&stats, Some(4.0));
        assert!(s.contains("+4.0 dBu"));
    }

    #[test]
    fn spectrum_readout_no_dbu_absent() {
        let stats = BroadbandStats {
            peak_db: -3.0,
            peak_hz: 1000.0,
            floor_db: -96.0,
            span_db: 93.0,
        };
        let s = spectrum_readout(&stats, None);
        assert!(!s.contains("dBu"));
    }

    // ── transfer_delay ────────────────────────────────────────────────

    #[test]
    fn transfer_delay_positive() {
        let s = transfer_delay(0.0625, 3);
        assert_eq!(s, "Δt = +0.06 ms  (+3 samp)");
    }

    #[test]
    fn transfer_delay_negative() {
        let s = transfer_delay(-0.0625, -3);
        assert_eq!(s, "Δt = -0.06 ms  (-3 samp)");
    }

    #[test]
    fn transfer_delay_zero() {
        let s = transfer_delay(0.0, 0);
        assert_eq!(s, "Δt = +0.00 ms  (+0 samp)");
    }

    #[test]
    fn transfer_delay_large() {
        let s = transfer_delay(12.34, 592);
        assert_eq!(s, "Δt = +12.34 ms  (+592 samp)");
    }

    // ── sweep_readout ─────────────────────────────────────────────────

    #[test]
    fn sweep_readout_basic() {
        let pt = test_sweep_point(1000.0, 0.0042, 0.0053);
        let s = sweep_readout(&pt);
        assert!(s.contains("1000.0 Hz"));
        assert!(s.contains("THD 0.0042%"));
        assert!(s.contains("THD+N 0.0053%"));
        assert!(s.contains("Fund -3.0 dBFS"));
        assert!(!s.contains("Gain"));
        assert!(!s.contains("dBu"));
    }

    #[test]
    fn sweep_readout_with_gain() {
        let mut pt = test_sweep_point(1000.0, 0.003, 0.005);
        pt.gain_db = Some(-0.50);
        let s = sweep_readout(&pt);
        assert!(s.contains("Gain -0.50 dB"));
    }

    #[test]
    fn sweep_readout_positive_gain() {
        let mut pt = test_sweep_point(1000.0, 0.003, 0.005);
        pt.gain_db = Some(3.21);
        let s = sweep_readout(&pt);
        assert!(s.contains("Gain +3.21 dB"));
    }

    #[test]
    fn sweep_readout_with_dbu() {
        let mut pt = test_sweep_point(1000.0, 0.003, 0.005);
        pt.in_dbu = Some(3.89);
        pt.out_dbu = Some(4.12);
        let s = sweep_readout(&pt);
        assert!(s.contains("In +3.89 dBu"));
        assert!(s.contains("Out +4.12 dBu"));
    }

    #[test]
    fn sweep_readout_negative_dbu() {
        let mut pt = test_sweep_point(1000.0, 0.003, 0.005);
        pt.in_dbu = Some(-12.0);
        let s = sweep_readout(&pt);
        assert!(s.contains("In -12.00 dBu"));
    }

    #[test]
    fn sweep_readout_thd_4_decimals() {
        let pt = test_sweep_point(1000.0, 0.00123, 0.00456);
        let s = sweep_readout(&pt);
        assert!(s.contains("THD 0.0012%"));
        assert!(s.contains("THD+N 0.0046%"));
    }

    // ── hover_label ───────────────────────────────────────────────────

    #[test]
    fn hover_db() {
        let s = hover_label(0, 1000.0, &HoverReadout::Db(-12.3));
        assert!(s.contains("CH0"));
        assert!(s.contains("kHz"));
        assert!(s.contains("-12.3 dB"));
    }

    #[test]
    fn hover_phase() {
        let s = hover_label(1, 500.0, &HoverReadout::Phase(-90.0));
        assert!(s.contains("CH1"));
        assert!(s.contains("500.0 Hz"));
        assert!(s.contains("-90.0 deg"));
    }

    #[test]
    fn hover_coherence() {
        let s = hover_label(0, 2000.0, &HoverReadout::Coherence(0.987));
        assert!(s.contains("coh 0.987"));
    }

    #[test]
    fn hover_thd() {
        let s = hover_label(0, 1000.0, &HoverReadout::Thd(0.0034));
        assert!(s.contains("THD 0.0034%"));
        assert!(!s.contains("CH")); // Thd variant has no channel prefix
    }

    #[test]
    fn hover_gain() {
        let s = hover_label(0, 5000.0, &HoverReadout::Gain(-1.23));
        assert!(s.contains("-1.23 dB"));
        assert!(s.contains("kHz"));
        assert!(!s.contains("CH")); // Gain variant has no channel prefix
    }

    // ── format_hz ─────────────────────────────────────────────────────

    #[test]
    fn format_hz_below_1k() {
        let s = format_hz(999.9);
        assert!(s.contains("999.9"));
        assert!(s.contains("Hz"));
        assert!(!s.contains("kHz"));
    }

    #[test]
    fn format_hz_exactly_1k() {
        let s = format_hz(1000.0);
        assert!(s.contains("1.00 kHz"));
    }

    #[test]
    fn format_hz_above_1k() {
        let s = format_hz(12500.0);
        assert!(s.contains("12.50 kHz"));
    }

    #[test]
    fn format_hz_low_freq() {
        let s = format_hz(20.0);
        assert!(s.contains("20.0"));
        assert!(s.contains("Hz"));
    }

    #[test]
    fn format_hz_field_width() {
        // {:>6.2} and {:>6.1} produce 6-char fields
        assert_eq!(format_hz(1000.0).trim().len(), "1.00 kHz".len());
    }
}
