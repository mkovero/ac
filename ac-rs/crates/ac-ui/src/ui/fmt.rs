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
/// meaningful when a known pure tone is driving the system (sweep / thd /
/// plot commands), not in live monitoring.
///
/// When the channel has been voltage-calibrated, both dBu and dBV appear
/// alongside the peak so the user sees the analog-domain level in either
/// convention. dBV is derived from dBu via the fixed
/// `dBV = dBu + 20·log10(V_ref_dbu)` relation
/// (see `ac_core::shared::conversions::dbu_to_dbv`).
///
/// When the channel has been pistonphone-calibrated (`spl_offset_db` is
/// `Some`), peak and floor are reported in **dB SPL** instead of dBFS.
/// `span_db` stays in dB (it's a difference) but its label drops `FS` to
/// match. Both calibrations compose: a fully-cal'd channel shows dB SPL
/// peak/floor *and* dBu/dBV.
pub fn spectrum_readout(
    stats:          &BroadbandStats,
    in_dbu:         Option<f32>,
    spl_offset_db:  Option<f32>,
    mic_correction: Option<&str>,
) -> String {
    let cal = in_dbu
        .map(|dbu| {
            let dbv = ac_core::shared::conversions::dbu_to_dbv(dbu as f64) as f32;
            format!("   {:+.1} dBu   {:+.1} dBV", dbu, dbv)
        })
        .unwrap_or_default();
    let mic = match mic_correction {
        Some("on")  => "  [mic-corrected]",
        Some("off") => "  [mic-cal off]",
        _ => "",
    };
    match spl_offset_db {
        Some(off) => format!(
            "peak {:>6.1} dB SPL @ {}  │  floor {:>6.1} dB SPL  │  span {:>5.1} dB{}{}",
            stats.peak_db + off,
            format_hz(stats.peak_hz).trim(),
            stats.floor_db + off,
            stats.span_db,
            cal,
            mic,
        ),
        None => format!(
            "peak {:>6.1} dBFS @ {}  │  floor {:>6.1} dBFS  │  span {:>5.1} dB{}{}",
            stats.peak_db,
            format_hz(stats.peak_hz).trim(),
            stats.floor_db,
            stats.span_db,
            cal,
            mic,
        ),
    }
}

/// Live-monitor readout for the FFT knobs shown top-right in Spectrum mode.
/// Pure function so the exact text — including the `Δf = sr / N` math — is
/// covered by unit tests rather than only by the paint-test harness.
pub fn monitor_knobs_readout(interval_ms: u32, fft_n: u32, sr: u32) -> String {
    let df = sr as f32 / fft_n.max(1) as f32;
    format!("{:>4} ms  │  N {}  │  Δf {:.1} Hz", interval_ms, fft_n, df)
}

/// Compact label for the top-right top-line ("{sr} Hz │ {channel}").
pub fn top_right_status(sr: u32, channel_label: &str) -> String {
    format!("{} Hz │ {}", sr, channel_label)
}

/// Tier 2 technique badge shown top-right so the reader knows which
/// live-analysis technique is producing the view. `analysis_mode` is
/// the server-global setting (`"fft"` / `"cwt"` / `"cqt"` /
/// `"reassigned"`); unknown values are surfaced verbatim so bad state
/// is visible instead of silent.
///
/// `cqt_f_min_hz` is the active CQT grid's lowest displayed bin —
/// dynamic because the daemon clamps it above
/// `cqt::DEFAULT_F_MIN` based on the ring length / sample rate. The
/// caller reads it from the most recent CQT frame's `freqs[0]`. For
/// other modes the value is ignored.
pub fn tier_badge(
    analysis_mode: &str,
    fft_n: u32,
    cwt_sigma: f32,
    cwt_n_scales: usize,
    cqt_f_min_hz: f32,
) -> String {
    match analysis_mode {
        "fft" => format!("FFT · N={fft_n} · Hann"),
        "cwt" => format!("CWT · Morlet · σ={cwt_sigma:.0} · N_scales={cwt_n_scales}"),
        "cqt" => format!(
            "CQT · {} bpo · fmin={:.0} Hz",
            ac_core::visualize::cqt::DEFAULT_BPO,
            cqt_f_min_hz,
        ),
        "reassigned" => format!(
            "Reassigned · N={} · {} bins · Hann",
            ac_core::visualize::reassigned::DEFAULT_N,
            ac_core::visualize::reassigned::DEFAULT_N_OUT_BINS,
        ),
        other => format!("{other}"),
    }
}

/// Header line for the peak-hold corner readout — "PEAK CH{n}". Sits above
/// one or more `peak_rank_line` rows listing ranked local maxima.
pub fn peak_header(channel: usize) -> String {
    format!("PEAK CH{channel}")
}

/// Ranked-peak row under the peak-hold corner header — e.g.
/// "  1.  220.0 Hz  -12.3 dB". `rank` is 1-based; layout matches the 6-slot
/// corner budget (header + up to 5 rows).
pub fn peak_rank_line(rank: usize, f_hz: f32, amp_db: f32) -> String {
    format!("  {}. {:>9}  {:+.1} dB", rank, format_freq_compact(f_hz), amp_db)
}

/// Compact frequency formatter used by the peak overlay. Threshold-picked
/// so the narrow right-edge corner column never overflows:
///   - below 1 kHz  → "NNN.N Hz"
///   - 1–10 kHz     → "N.NNN kHz" (three decimals preserve bin resolution)
///   - 10 kHz+      → "NN.NN kHz"
pub fn format_freq_compact(hz: f32) -> String {
    if hz >= 10_000.0 {
        format!("{:.2} kHz", hz / 1000.0)
    } else if hz >= 1_000.0 {
        format!("{:.3} kHz", hz / 1000.0)
    } else {
        format!("{:.1} Hz", hz)
    }
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

/// Cursor-driven footer readout for the spectrum / waterfall views. Used
/// instead of plastering a label next to the crosshair (which obstructs
/// the trace and collides with other annotations). Replaces the
/// broadband stats when the cursor is over a cell.
///
/// `cursor_db_at_bin` is the dBFS the cursor is currently over (raw bin
/// magnitude). `peaks` is the daemon's parabolic-interpolated peak list
/// — when the cursor sits within `snap_tol_hz` of a peak frequency, the
/// readout snaps to that peak's scallop-corrected dBFS and the line is
/// suffixed with `▲` so the user can tell measured-vs-interpolated.
///
/// SPL takes precedence over voltage cal (acoustic channels render as
/// dB SPL); voltage cal renders dBFS / dBu / dBV; uncal'd renders dBFS
/// only.
pub fn cursor_readout(
    cursor_freq_hz:   f32,
    cursor_db_at_bin: f32,
    peaks:            &[[f32; 2]],
    dbu_offset_db:    Option<f32>,
    spl_offset_db:    Option<f32>,
) -> String {
    // Snap window: 1% of cursor freq, floor at 2 Hz so low-freq peaks
    // (where 1% is sub-Hz) still snap reliably.
    let snap_tol_hz = (cursor_freq_hz * 0.01).max(2.0);
    let mut best: Option<(f32, f32)> = None;
    let mut best_dist = f32::INFINITY;
    for p in peaks {
        let d = (p[0] - cursor_freq_hz).abs();
        if d < best_dist && d <= snap_tol_hz {
            best_dist = d;
            best = Some((p[0], p[1]));
        }
    }
    let (freq, db, snapped) = match best {
        Some((f, d)) => (f, d, true),
        None         => (cursor_freq_hz, cursor_db_at_bin, false),
    };
    let snap_tag = if snapped { "  ▲" } else { "" };

    if let Some(off) = spl_offset_db {
        format!(
            "cursor {} {:>6.1} dB SPL{}",
            format_hz(freq),
            db + off,
            snap_tag,
        )
    } else if let Some(off) = dbu_offset_db {
        let dbu = db + off;
        let dbv = ac_core::shared::conversions::dbu_to_dbv(dbu as f64) as f32;
        format!(
            "cursor {} {:+6.2} dBFS  {:+6.2} dBu  {:+6.2} dBV{}",
            format_hz(freq), db, dbu, dbv, snap_tag,
        )
    } else {
        format!(
            "cursor {} {:+6.2} dBFS{}",
            format_hz(freq), db, snap_tag,
        )
    }
}

/// Hover crosshair readout label.
///
/// For `Db` readouts (dBFS magnitude at a cursor position):
/// - `spl_offset_db = Some(off)` — render `dB SPL` (`v + off`); takes
///   precedence over voltage cal since pistonphone-cal'd channels are
///   acoustic and dBu/dBV would be misleading.
/// - `dbu_offset_db = Some(off)` — voltage-cal'd channel: render
///   `dBFS / dBu / dBV` so the user can read the analog level off any
///   bin without picking up the bottom-strip readout. Sine-on-bin
///   assumption — scallop biases the result by up to ~1.4 dB at
///   worst-case frequencies; the bottom-strip in_dbu (time-domain RMS)
///   is the authoritative reading for clean tones.
/// - neither — bare dBFS as before.
///
/// Other variants (THD%, gain dB, time-ago) ignore both offsets since
/// they're not single-sample dBFS readings.
pub fn hover_label(
    channel:       usize,
    freq_hz:       f32,
    readout:       &HoverReadout,
    spl_offset_db: Option<f32>,
    dbu_offset_db: Option<f32>,
) -> String {
    match readout {
        HoverReadout::Db(v) => {
            if let Some(off) = spl_offset_db {
                format!(
                    "CH{} {} {:>6.1} dB SPL",
                    channel,
                    format_hz(freq_hz),
                    v + off,
                )
            } else if let Some(off) = dbu_offset_db {
                let dbu = v + off;
                let dbv = ac_core::shared::conversions::dbu_to_dbv(dbu as f64) as f32;
                format!(
                    "CH{} {} {:+6.1} dBFS  {:+6.2} dBu  {:+6.2} dBV",
                    channel,
                    format_hz(freq_hz),
                    v, dbu, dbv,
                )
            } else {
                format!(
                    "CH{} {} {:+6.1} dB",
                    channel,
                    format_hz(freq_hz),
                    v,
                )
            }
        }
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
        HoverReadout::TimeAgo(s) => format!(
            "CH{} {} t-{}",
            channel,
            format_hz(freq_hz),
            format_time_ago(*s),
        ),
    }
}

/// Format a non-negative time-ago in seconds as a short human label: `12ms`,
/// `340ms`, `1.23s`, `17.5s`. Anchors the waterfall/CWT hover readout.
pub fn format_time_ago(s: f32) -> String {
    let s = s.max(0.0);
    if s < 1.0 {
        format!("{:.0}ms", s * 1000.0)
    } else if s < 10.0 {
        format!("{:.2}s", s)
    } else {
        format!("{:.1}s", s)
    }
}

/// EBU R128 delivery target (integrated loudness).
pub const R128_TARGET_LKFS: f64 = -23.0;
/// EBU R128 pass tolerance (broadcast delivery).
pub const R128_TOLERANCE_TIGHT_LU: f64 = 0.5;
/// EBU R128 loose tolerance — live / streaming delivery.
pub const R128_TOLERANCE_LOOSE_LU: f64 = 2.0;

/// Build the top-right loudness status lines for the current meter
/// readout. Returns up to three lines: one M/S/I/LRA summary, one
/// dBTP + gated-duration, and (when integrated is valid) an R128
/// pass/warn/fail tag.
///
/// **SPL composition.** When the readout's `spl_offset_db` is `Some`,
/// M/S/I render as K-weighted dB SPL (`Mk`/`Sk`/`Ik`, suffix `dB SPL`)
/// and the true-peak line becomes `Lpk(K) X dB SPL`. The R128 badge
/// stays anchored on the raw integrated LKFS value because the
/// `-23 LKFS` target is independent of the absolute reference — the
/// delta is meaningful in either calibration state.
pub fn loudness_readout_lines(l: &crate::data::types::LoudnessReadout) -> Vec<crate::ui::overlay::LoudnessLine> {
    use crate::ui::overlay::{LoudnessLine, LoudnessTint};
    let off = l.spl_offset_db;
    let fmt_val = |v: Option<f64>| -> String {
        match v {
            Some(x) if x.is_finite() => match off {
                Some(o) => format!("{:6.1}", x + o),                // SPL → unsigned
                None    => format!("{:+6.1}", x),                   // LKFS → signed
            },
            _ => "  —  ".into(),
        }
    };
    let mut out = Vec::new();
    let lra = l.lra_lu;
    let m = fmt_val(l.momentary_lkfs);
    let s = fmt_val(l.short_term_lkfs);
    let i = fmt_val(l.integrated_lkfs);
    let line1 = match off {
        Some(_) => format!("Mk{m} Sk{s} Ik{i} dB SPL  LRA{lra:4.1}"),
        None    => format!("M{m} S{s} I{i} LRA{lra:4.1}"),
    };
    out.push(LoudnessLine { text: line1, tint: LoudnessTint::Default });
    let tp = match l.true_peak_dbtp {
        Some(v) if v.is_finite() => match off {
            Some(o) => format!("{:5.1}", v + o),
            None    => format!("{:+5.1}", v),
        },
        _ => "  —".into(),
    };
    let dur = l.gated_duration_s;
    let line2 = match off {
        Some(_) => format!("Lpk(K) {tp} dB SPL   gated {dur:.1}s"),
        None    => format!("dBTP {tp}   gated {dur:.1}s"),
    };
    out.push(LoudnessLine { text: line2, tint: LoudnessTint::Default });
    // R128 pass/warn/fail badge on the integrated value. Only emit once
    // integrated is defined — pre-gate silence stays quiet. The target
    // (`-23 LKFS`) is independent of SPL calibration, so this branch
    // intentionally ignores `spl_offset_db` and reads the raw LKFS.
    if let Some(i) = l.integrated_lkfs {
        if i.is_finite() {
            let delta = i - R128_TARGET_LKFS;
            let (tint, tag) = if delta.abs() <= R128_TOLERANCE_TIGHT_LU {
                (LoudnessTint::Good, "PASS")
            } else if delta.abs() <= R128_TOLERANCE_LOOSE_LU {
                (LoudnessTint::Warn, "WARN")
            } else {
                (LoudnessTint::Bad, "FAIL")
            };
            out.push(LoudnessLine {
                text: format!("R128 {tag}  Δ {delta:+.1} LU"),
                tint,
            });
        }
    }
    out
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
        let s = spectrum_readout(&stats, None, None, None);
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
        let s = spectrum_readout(&stats, None, None, None);
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
        let s = spectrum_readout(&stats, Some(4.0), None, None);
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
        let s = spectrum_readout(&stats, None, None, None);
        assert!(!s.contains("dBu"));
        assert!(!s.contains("dBV"));
    }

    #[test]
    fn spectrum_readout_shows_dbv_when_calibrated() {
        let stats = BroadbandStats {
            peak_db: -3.0,
            peak_hz: 1000.0,
            floor_db: -96.0,
            span_db: 93.0,
        };
        // 0 dBu is exactly V_ref_dbu (sqrt(0.6) V rms by default), which in
        // dBV is −2.218... dB. The readout must show both in the correct
        // relation.
        let s = spectrum_readout(&stats, Some(0.0), None, None);
        assert!(s.contains("+0.0 dBu"), "want dBu in: {s}");
        assert!(s.contains("-2.2 dBV"), "want dBV at −2.2 in: {s}");
    }

    #[test]
    fn spectrum_readout_dbspl_when_spl_calibrated() {
        // SPL cal example from the issue: pistonphone at 94 dB SPL captured
        // -32 dBFS → spl_offset_db = 126. A peak at -3 dBFS reads 123 dB SPL,
        // and the floor at -96 dBFS reads 30 dB SPL. The "dBFS" suffix must
        // not appear when SPL is active.
        let stats = BroadbandStats {
            peak_db: -3.0,
            peak_hz: 1000.0,
            floor_db: -96.0,
            span_db: 93.0,
        };
        let s = spectrum_readout(&stats, None, Some(126.0), None);
        assert!(s.contains("123.0 dB SPL"), "want SPL peak in: {s}");
        assert!(s.contains("30.0 dB SPL"),  "want SPL floor in: {s}");
        assert!(!s.contains("dBFS"),        "must not show dBFS: {s}");
        assert!(s.contains("93.0 dB"),      "span unchanged: {s}");
    }

    #[test]
    fn spectrum_readout_spl_round_trip_at_94() {
        // Acceptance criterion: pistonphone reference at 94 dB SPL → reads
        // 94 dB SPL. The peak is the captured dBFS at calibration time;
        // applied SPL offset = 94 - peak. Check the readout reads 94 dB SPL.
        let captured_dbfs = -32.0_f32;
        let stats = BroadbandStats {
            peak_db: captured_dbfs,
            peak_hz: 1000.0,
            floor_db: -90.0,
            span_db: 58.0,
        };
        let off = 94.0 - captured_dbfs;
        let s = spectrum_readout(&stats, None, Some(off), None);
        assert!(s.contains("94.0 dB SPL"), "round-trip readout: {s}");
    }

    #[test]
    fn hover_label_uses_dbspl_when_calibrated() {
        let no_cal = hover_label(0, 1000.0, &HoverReadout::Db(-12.5), None, None);
        assert!(no_cal.contains("-12.5 dB"));
        assert!(!no_cal.contains("SPL"));
        assert!(!no_cal.contains("dBu"));

        let cal = hover_label(0, 1000.0, &HoverReadout::Db(-32.0), Some(126.0), None);
        assert!(cal.contains("94.0 dB SPL"), "want SPL: {cal}");
    }

    #[test]
    fn hover_label_non_db_variants_ignore_spl_offset() {
        let thd = hover_label(0, 1000.0, &HoverReadout::Thd(0.5), Some(126.0), None);
        assert!(thd.contains("THD 0.5000%"));
        assert!(!thd.contains("SPL"));

        let time = hover_label(0, 1000.0, &HoverReadout::TimeAgo(1.5), Some(126.0), None);
        assert!(time.contains("t-"));
        assert!(!time.contains("SPL"));
    }

    #[test]
    fn hover_label_voltage_cal_shows_dbfs_dbu_dbv() {
        // FF400-rig cal example: vrms_at_0dbfs_in = 3.5331 V, dbu_ref = sqrt(0.6).
        // dbu_offset = 20·log10(3.5331 / (sqrt(2) · 0.7746)) ≈ 10.17 dB.
        // For a -10 dBFS bin (peak-ref), expected dBu = ~+0.17.
        let s = hover_label(0, 1000.0, &HoverReadout::Db(-10.0), None, Some(10.17));
        assert!(s.contains("-10.0 dBFS"), "want dBFS: {s}");
        assert!(s.contains("+0.17 dBu") || s.contains("+0.16 dBu") || s.contains("+0.18 dBu"),
            "want dBu near +0.17: {s}");
        assert!(s.contains("dBV"), "want dBV: {s}");
    }

    #[test]
    fn hover_label_spl_offset_overrides_dbu_offset() {
        // Pistonphone-cal'd channels are acoustic; rendering dBu/dBV would
        // be misleading. SPL takes precedence when both offsets are present.
        let s = hover_label(0, 1000.0, &HoverReadout::Db(-32.0), Some(126.0), Some(10.17));
        assert!(s.contains("dB SPL"), "want SPL: {s}");
        assert!(!s.contains("dBu"), "should not show dBu when SPL active: {s}");
    }

    #[test]
    fn spectrum_readout_dbu_dbv_offset_is_consistent() {
        // For any calibrated dBu, the dBV reading must equal
        // ac_core::shared::conversions::dbu_to_dbv(dbu), rounded to one decimal.
        for dbu in [-20.0_f32, -4.0, 0.0, 4.0, 12.5] {
            let stats = BroadbandStats {
                peak_db: -3.0,
                peak_hz: 1000.0,
                floor_db: -96.0,
                span_db: 93.0,
            };
            let s = spectrum_readout(&stats, Some(dbu), None, None);
            let expected = ac_core::shared::conversions::dbu_to_dbv(dbu as f64) as f32;
            let needle = format!("{:+.1} dBV", expected);
            assert!(s.contains(&needle), "want {needle} in: {s}");
        }
    }

    // ── broadband_stats math ──────────────────────────────────────────

    #[test]
    fn broadband_stats_span_matches_peak_minus_floor() {
        let (spec, freqs) = mk_spec(50, 0.0, -120.0, 2048);
        let s = broadband_stats(&spec, &freqs).unwrap();
        assert!((s.span_db - (s.peak_db - s.floor_db)).abs() < 1e-5);
    }

    #[test]
    fn broadband_stats_floor_is_tenth_percentile() {
        // Construct a spectrum with a known distribution: bins 0..=9 at -90,
        // 10..=99 at -40. 10th percentile index in a sorted ascending list
        // is 10 — just past the "-90" block → -40. Verify.
        let n = 100;
        let mut spec = vec![-40.0f32; n];
        for v in spec.iter_mut().take(10) {
            *v = -90.0;
        }
        let freqs: Vec<f32> = (0..n).map(|i| i as f32).collect();
        let s = broadband_stats(&spec, &freqs).unwrap();
        // With 100 finite values, idx = 10 after sort. Sorted[10] = -40.
        assert!((s.floor_db - -40.0).abs() < 1e-5, "floor = {}", s.floor_db);
    }

    // ── monitor knobs ─────────────────────────────────────────────────

    #[test]
    fn monitor_knobs_delta_f_math() {
        // Δf = sr / N. 48000 / 4096 = 11.71875 → rounds to "11.7 Hz".
        let s = monitor_knobs_readout(10, 4096, 48_000);
        assert!(s.contains("Δf 11.7 Hz"), "got: {s}");
        // 96 kHz, N = 8192 → 11.71875 as well.
        let s = monitor_knobs_readout(10, 8192, 96_000);
        assert!(s.contains("Δf 11.7 Hz"), "got: {s}");
        // 48 kHz, N = 2048 → 23.4375 → "23.4 Hz".
        let s = monitor_knobs_readout(5, 2048, 48_000);
        assert!(s.contains("Δf 23.4 Hz"), "got: {s}");
    }

    #[test]
    fn monitor_knobs_formats_interval_and_n() {
        let s = monitor_knobs_readout(7, 16384, 48_000);
        assert!(s.contains("   7 ms"));
        assert!(s.contains("N 16384"));
    }

    #[test]
    fn monitor_knobs_zero_n_does_not_panic() {
        // Defensive guard — mp.fft_n.max(1).
        let _ = monitor_knobs_readout(1, 0, 48_000);
    }

    // ── top-right status ──────────────────────────────────────────────

    #[test]
    fn tier_badge_fft() {
        assert_eq!(tier_badge("fft", 16384, 12.0, 512, 0.0), "FFT · N=16384 · Hann");
    }

    #[test]
    fn tier_badge_cwt() {
        assert_eq!(
            tier_badge("cwt", 16384, 12.0, 512, 0.0),
            "CWT · Morlet · σ=12 · N_scales=512",
        );
    }

    #[test]
    fn tier_badge_cqt_uses_live_f_min() {
        // The badge picks up the runtime f_min the daemon clamped to
        // (≈ 34 Hz at default ring at 48 kHz with bpo=24). It does
        // not hardcode the const 30 Hz.
        assert_eq!(
            tier_badge("cqt", 0, 0.0, 0, 34.13),
            "CQT · 24 bpo · fmin=34 Hz",
        );
    }

    #[test]
    fn tier_badge_reassigned() {
        assert_eq!(
            tier_badge("reassigned", 0, 0.0, 0, 0.0),
            "Reassigned · N=4096 · 1024 bins · Hann",
        );
    }

    #[test]
    fn tier_badge_unknown_mode_falls_through() {
        // Unknown values surface verbatim so bad state is visible
        // instead of silent.
        assert_eq!(tier_badge("future-mode", 0, 0.0, 0, 0.0), "future-mode");
    }

    #[test]
    fn top_right_status_format() {
        assert_eq!(top_right_status(48_000, "CH0"), "48000 Hz │ CH0");
        assert_eq!(
            top_right_status(96_000, "transfer0"),
            "96000 Hz │ transfer0"
        );
    }

    // ── peak-hold corner label ────────────────────────────────────────

    #[test]
    fn peak_header_format() {
        assert_eq!(peak_header(0), "PEAK CH0");
        assert_eq!(peak_header(7), "PEAK CH7");
    }

    #[test]
    fn peak_rank_line_signed_db() {
        // Freq column is right-aligned in a 9-wide slot. "500.5 Hz" (8 chars)
        // gets one pad; "5.000 kHz"/"12.35 kHz" (9 chars) fill it exactly.
        assert_eq!(peak_rank_line(1, 500.5, -12.3), "  1.  500.5 Hz  -12.3 dB");
        assert_eq!(peak_rank_line(2, 2000.0, -48.3), "  2. 2.000 kHz  -48.3 dB");
        assert_eq!(peak_rank_line(3, 12_345.6, -0.4), "  3. 12.35 kHz  -0.4 dB");
    }

    #[test]
    fn peak_rank_line_positive_db() {
        assert_eq!(peak_rank_line(5, 5000.0, 6.1), "  5. 5.000 kHz  +6.1 dB");
    }

    // ── format_freq_compact boundaries ────────────────────────────────

    #[test]
    fn format_freq_compact_below_1k() {
        assert_eq!(format_freq_compact(50.0), "50.0 Hz");
        assert_eq!(format_freq_compact(999.9), "999.9 Hz");
    }

    #[test]
    fn format_freq_compact_1k_to_10k() {
        assert_eq!(format_freq_compact(1000.0), "1.000 kHz");
        assert_eq!(format_freq_compact(2345.0), "2.345 kHz");
        assert_eq!(format_freq_compact(9999.9), "10.000 kHz");
        // ^ rounding: 9999.9/1000 = 9.9999 → {:.3} rounds to 10.000.
        // That's cosmetically fine since 10.00 kHz would mean the same thing.
    }

    #[test]
    fn format_freq_compact_above_10k() {
        assert_eq!(format_freq_compact(10_000.0), "10.00 kHz");
        assert_eq!(format_freq_compact(12_345.6), "12.35 kHz");
        assert_eq!(format_freq_compact(48_000.0), "48.00 kHz");
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
        let s = hover_label(0, 1000.0, &HoverReadout::Db(-12.3), None, None);
        assert!(s.contains("CH0"));
        assert!(s.contains("kHz"));
        assert!(s.contains("-12.3 dB"));
    }

    #[test]
    fn hover_thd() {
        let s = hover_label(0, 1000.0, &HoverReadout::Thd(0.0034), None, None);
        assert!(s.contains("THD 0.0034%"));
        assert!(!s.contains("CH")); // Thd variant has no channel prefix
    }

    #[test]
    fn hover_gain() {
        let s = hover_label(0, 5000.0, &HoverReadout::Gain(-1.23), None, None);
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

    // ── loudness_readout_lines: SPL composition ───────────────────────

    fn dummy_readout() -> crate::data::types::LoudnessReadout {
        crate::data::types::LoudnessReadout {
            momentary_lkfs: Some(-23.0),
            short_term_lkfs: Some(-23.0),
            integrated_lkfs: Some(-23.0),
            lra_lu: 4.5,
            true_peak_dbtp: Some(-1.0),
            gated_duration_s: 17.3,
            spl_offset_db: None,
        }
    }

    #[test]
    fn loudness_lines_are_lkfs_when_uncalibrated() {
        let lines = loudness_readout_lines(&dummy_readout());
        let joined = lines.iter().map(|l| l.text.as_str()).collect::<Vec<_>>().join(" | ");
        assert!(joined.contains("M -23.0") || joined.contains("M-23.0"), "{joined}");
        assert!(joined.contains("dBTP"), "{joined}");
        assert!(joined.contains("LRA"), "{joined}");
        assert!(!joined.contains("SPL"), "must not show SPL when uncalibrated: {joined}");
        assert!(!joined.contains("Mk"),  "Mk only appears in SPL mode: {joined}");
    }

    #[test]
    fn loudness_lines_compose_with_spl_calibration() {
        // Pistonphone offset of +97 → integrated -23 LKFS reads 74 dB SPL
        // (K-weighted). True peak shifts the same way.
        let mut r = dummy_readout();
        r.spl_offset_db = Some(97.0);
        let lines = loudness_readout_lines(&r);
        let joined = lines.iter().map(|l| l.text.as_str()).collect::<Vec<_>>().join(" | ");
        assert!(joined.contains("Mk"),    "K-suffix marker missing: {joined}");
        assert!(joined.contains("Sk"),    "Sk-suffix marker missing: {joined}");
        assert!(joined.contains("Ik"),    "Ik-suffix marker missing: {joined}");
        assert!(joined.contains("dB SPL"),"dB SPL unit missing: {joined}");
        assert!(joined.contains("74.0"),  "integrated at 74 dB SPL missing: {joined}");
        assert!(joined.contains("Lpk(K)"),"true-peak label missing: {joined}");
        assert!(joined.contains("96.0"),  "true peak at 96 dB SPL missing: {joined}");
        // Old labels must not appear in SPL mode.
        assert!(!joined.contains("dBTP"), "dBTP must be replaced: {joined}");
    }

    // ── spectrum_readout: mic-correction tag ──────────────────────────

    #[test]
    fn spectrum_readout_shows_mic_corrected_when_on() {
        let stats = BroadbandStats {
            peak_db: -3.0, peak_hz: 1000.0, floor_db: -96.0, span_db: 93.0,
        };
        let s = spectrum_readout(&stats, None, None, Some("on"));
        assert!(s.contains("[mic-corrected]"), "expected mic tag in: {s}");
    }

    #[test]
    fn spectrum_readout_shows_off_when_loaded_but_disabled() {
        let stats = BroadbandStats {
            peak_db: -3.0, peak_hz: 1000.0, floor_db: -96.0, span_db: 93.0,
        };
        let s = spectrum_readout(&stats, None, None, Some("off"));
        assert!(s.contains("[mic-cal off]"), "expected off tag in: {s}");
    }

    #[test]
    fn spectrum_readout_no_mic_tag_when_no_curve() {
        let stats = BroadbandStats {
            peak_db: -3.0, peak_hz: 1000.0, floor_db: -96.0, span_db: 93.0,
        };
        let s_none = spectrum_readout(&stats, None, None, None);
        let s_str = spectrum_readout(&stats, None, None, Some("none"));
        assert!(!s_none.contains("mic-"), "no mic tag without curve: {s_none}");
        assert!(!s_str.contains("mic-"),  "no mic tag for explicit \"none\": {s_str}");
    }

    #[test]
    fn spectrum_readout_mic_tag_composes_with_spl() {
        // Both calibrations active: peak in dB SPL with mic-corrected suffix.
        let stats = BroadbandStats {
            peak_db: -3.0, peak_hz: 1000.0, floor_db: -96.0, span_db: 93.0,
        };
        let s = spectrum_readout(&stats, None, Some(97.0), Some("on"));
        assert!(s.contains("dB SPL"), "{s}");
        assert!(s.contains("[mic-corrected]"), "{s}");
    }

    #[test]
    fn r128_pass_unaffected_by_spl_offset() {
        // Target is -23 LKFS regardless of SPL calibration; the badge
        // anchors on the raw integrated value, not the offset reading.
        let mut r = dummy_readout();
        r.integrated_lkfs = Some(-23.0);
        r.spl_offset_db = Some(97.0);
        let lines = loudness_readout_lines(&r);
        let joined = lines.iter().map(|l| l.text.as_str()).collect::<Vec<_>>().join(" | ");
        assert!(joined.contains("R128 PASS"), "PASS still expected at integrated=-23: {joined}");
        assert!(joined.contains("Δ +0.0 LU"), "delta still measured against -23 LKFS: {joined}");
    }
}
