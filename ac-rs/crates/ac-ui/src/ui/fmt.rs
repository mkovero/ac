use crate::data::types::SweepPoint;
use crate::ui::overlay::HoverReadout;

/// Primary spectrum readout (bottom-left overlay).
pub fn spectrum_readout(
    freq_hz: f32,
    fundamental_dbfs: f32,
    thd_pct: f32,
    thdn_pct: f32,
    in_dbu: Option<f32>,
) -> String {
    let dbu = in_dbu
        .map(|v| format!("   {:+.1} dBu", v))
        .unwrap_or_default();
    format!(
        "{:>7.1} Hz   {:>6.1} dBFS   THD {:.3}%   THD+N {:.3}%{}",
        freq_hz, fundamental_dbfs, thd_pct, thdn_pct, dbu,
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
            cmd: String::new(),
            drive_db: 0.0,
            freq_hz: Some(freq),
            thd_pct: thd,
            thdn_pct: thdn,
            fundamental_hz: freq,
            fundamental_dbfs: -3.0,
            linear_rms: 0.707,
            harmonic_levels: vec![],
            noise_floor_dbfs: -100.0,
            spectrum: vec![],
            freqs: vec![],
            clipping: false,
            ac_coupled: false,
            out_vrms: None,
            out_dbu: None,
            in_vrms: None,
            in_dbu: None,
            gain_db: None,
            vrms_at_0dbfs_out: None,
            vrms_at_0dbfs_in: None,
        }
    }

    // ── spectrum_readout ──────────────────────────────────────────────

    #[test]
    fn spectrum_readout_basic() {
        let s = spectrum_readout(1000.0, -3.0, 0.003, 0.005, None);
        assert_eq!(
            s,
            " 1000.0 Hz     -3.0 dBFS   THD 0.003%   THD+N 0.005%"
        );
    }

    #[test]
    fn spectrum_readout_thd_distinct_magnitudes() {
        let a = spectrum_readout(1000.0, -3.0, 0.003, 0.005, None);
        let b = spectrum_readout(1000.0, -3.0, 0.030, 0.050, None);
        assert_ne!(a, b);
        assert!(a.contains("THD 0.003%"));
        assert!(b.contains("THD 0.030%"));
    }

    #[test]
    fn spectrum_readout_with_dbu() {
        let s = spectrum_readout(1000.0, -3.0, 0.003, 0.005, Some(4.0));
        assert!(s.contains("+4.0 dBu"));
    }

    #[test]
    fn spectrum_readout_negative_dbu() {
        let s = spectrum_readout(1000.0, -3.0, 0.003, 0.005, Some(-10.5));
        assert!(s.contains("-10.5 dBu"));
    }

    #[test]
    fn spectrum_readout_no_dbu_absent() {
        let s = spectrum_readout(1000.0, -3.0, 0.003, 0.005, None);
        assert!(!s.contains("dBu"));
    }

    #[test]
    fn spectrum_readout_zero_thd() {
        let s = spectrum_readout(1000.0, -3.0, 0.0, 0.0, None);
        assert!(s.contains("THD 0.000%"));
        assert!(s.contains("THD+N 0.000%"));
    }

    #[test]
    fn spectrum_readout_high_thd() {
        let s = spectrum_readout(1000.0, -3.0, 99.999, 100.0, None);
        assert!(s.contains("THD 99.999%"));
    }

    #[test]
    fn spectrum_readout_wide_frequency() {
        let s = spectrum_readout(12345.6, -3.0, 0.003, 0.005, None);
        assert!(s.contains("12345.6 Hz"));
    }

    #[test]
    fn spectrum_readout_low_frequency() {
        let s = spectrum_readout(50.0, -20.0, 0.100, 0.200, None);
        assert!(s.contains("50.0 Hz"));
        assert!(s.contains("-20.0 dBFS"));
    }

    #[test]
    fn spectrum_readout_field_alignment() {
        let s = spectrum_readout(50.0, -3.0, 0.003, 0.005, None);
        // {:>7.1} for freq → 7 chars wide
        assert!(s.starts_with("   50.0 Hz"));
        // {:>6.1} for dBFS → 6 chars wide
        assert!(s.contains("  -3.0 dBFS"));
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
