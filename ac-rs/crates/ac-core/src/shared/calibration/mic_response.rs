//! Mic frequency-response correction layer.
//!
//! A curve imported from a manufacturer `.frd` / `.txt` file, stored
//! inline on the calibration entry so `cal.json` stays self-contained —
//! moving it between machines or sessions must not strand the cal.
//!
//! Independent of the voltage and τ layers (see the parent module's
//! "layer topology" doc): nothing here reads a Vrms or a τ, and the
//! correction is a per-frequency dB offset applied to a magnitude, never
//! folded into either.

use anyhow::Result;
use serde::{Deserialize, Serialize};

/// Parsed and validated mic frequency-response correction curve.
///
/// `freqs_hz[i]` is monotonically increasing (asserted on import). At any
/// reading frequency `f`, the mic over-reads the true level by
/// `correction_at(f)` dB, so consumers SUBTRACT this from the captured
/// magnitude to recover the truth: `corrected_dbfs = raw_dbfs - correction`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MicResponse {
    pub freqs_hz: Vec<f32>,
    pub gain_db: Vec<f32>,
    /// Original .frd / .txt path the curve was imported from. Informational
    /// only — never re-read at runtime; the curve data above is the source
    /// of truth.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_path: Option<String>,
    /// RFC3339 timestamp of the import. Lets the user tell at a glance
    /// when a curve was attached without diffing cal.json.
    pub imported_at: String,
}

impl MicResponse {
    /// Hard upper bound on point count — typical mic .frd files have
    /// 100–500 points (1/24 to 1/48 octave); 4096 is generous and keeps
    /// cal.json under ~50 KB per channel.
    pub const MAX_POINTS: usize = 4096;
    /// Hard lower bound — fewer than 16 points produces an
    /// uncomfortably coarse log-linear interpolation across the audio band.
    pub const MIN_POINTS: usize = 16;

    /// Linear interpolation of `gain_db` in log-frequency space. Frequencies
    /// outside the curve's range clamp to the nearest endpoint (constant
    /// extrapolation — better than zero-extrapolation for room-acoustic work
    /// where the curve usually defines just the audio band).
    pub fn correction_at(&self, freq_hz: f32) -> f32 {
        if self.freqs_hz.is_empty() {
            return 0.0;
        }
        if !freq_hz.is_finite() || freq_hz <= 0.0 {
            return self.gain_db[0];
        }
        if freq_hz <= self.freqs_hz[0] {
            return self.gain_db[0];
        }
        let last = self.freqs_hz.len() - 1;
        if freq_hz >= self.freqs_hz[last] {
            return self.gain_db[last];
        }
        // Binary search for the bracketing pair.
        let i = self
            .freqs_hz
            .partition_point(|&f| f <= freq_hz)
            .saturating_sub(1);
        let f_lo = self.freqs_hz[i];
        let f_hi = self.freqs_hz[i + 1];
        let g_lo = self.gain_db[i];
        let g_hi = self.gain_db[i + 1];
        let log_lo = f_lo.ln();
        let log_hi = f_hi.ln();
        let log_f = freq_hz.ln();
        let t = ((log_f - log_lo) / (log_hi - log_lo)).clamp(0.0, 1.0);
        g_lo + (g_hi - g_lo) * t
    }
}

/// Parse the two-column ASCII format used by Behringer / Dayton / miniDSP
/// mic calibration files. One `<freq_hz> <gain_db>` pair per line, optional
/// whitespace, comments starting with `*` or `#` are ignored. An optional
/// third column (phase) is ignored. Validates monotonically increasing
/// frequencies, finite values, and the [`MicResponse::MIN_POINTS`] /
/// [`MicResponse::MAX_POINTS`] bounds.
pub fn parse_mic_curve(text: &str, source_path: Option<String>) -> Result<MicResponse> {
    let mut freqs = Vec::new();
    let mut gains = Vec::new();
    for (line_no, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with('*') || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        let mut cols = line.split_whitespace();
        let f_tok = cols.next();
        let g_tok = cols.next();
        let (f_str, g_str) = match (f_tok, g_tok) {
            (Some(f), Some(g)) => (f, g),
            _ => anyhow::bail!(
                "line {}: expected `freq_hz gain_db [phase]`, got {raw:?}",
                line_no + 1
            ),
        };
        let f: f32 = f_str.parse().map_err(|e| {
            anyhow::anyhow!("line {}: failed to parse freq {f_str:?}: {e}", line_no + 1)
        })?;
        let g: f32 = g_str.parse().map_err(|e| {
            anyhow::anyhow!("line {}: failed to parse gain {g_str:?}: {e}", line_no + 1)
        })?;
        if !f.is_finite() || f <= 0.0 {
            anyhow::bail!("line {}: freq must be > 0 Hz, got {f}", line_no + 1);
        }
        if !g.is_finite() {
            anyhow::bail!("line {}: gain must be finite, got {g}", line_no + 1);
        }
        if let Some(&prev) = freqs.last() {
            if f <= prev {
                anyhow::bail!(
                    "line {}: frequencies must increase strictly (got {f} after {prev})",
                    line_no + 1
                );
            }
        }
        freqs.push(f);
        gains.push(g);
    }
    if freqs.len() < MicResponse::MIN_POINTS {
        anyhow::bail!(
            "mic curve too sparse: got {} points, need ≥ {}",
            freqs.len(),
            MicResponse::MIN_POINTS
        );
    }
    if freqs.len() > MicResponse::MAX_POINTS {
        anyhow::bail!(
            "mic curve too dense: got {} points, max {}",
            freqs.len(),
            MicResponse::MAX_POINTS
        );
    }
    Ok(MicResponse {
        freqs_hz: freqs,
        gain_db: gains,
        source_path,
        imported_at: crate::shared::time::now_utc_iso8601(),
    })
}

/// Fixtures shared with the persistence tests in [`super::store`], which
/// round-trip a curve through disk alongside the voltage and SPL fields.
#[cfg(test)]
pub(super) mod fixtures {
    /// A synthetic `.frd` body: `n` points geometrically spaced from 20 Hz
    /// to 20 kHz, gain rising linearly with log-frequency from 0 to 4 dB.
    /// A "known curve" — every assertion about interpolation below is
    /// stated against this shape.
    pub(crate) fn dummy_curve_text(n: usize) -> String {
        let mut s = String::from("* a comment\n");
        let log_min = 20.0_f32.ln();
        let log_max = 20_000.0_f32.ln();
        for i in 0..n {
            let t = i as f32 / (n - 1) as f32;
            let f = (log_min + t * (log_max - log_min)).exp();
            let g = 4.0 * t;
            s.push_str(&format!("{f}\t{g}\n"));
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::dummy_curve_text;
    use super::*;

    #[test]
    fn parse_mic_curve_round_trip() {
        let text = dummy_curve_text(64);
        let r = parse_mic_curve(&text, Some("/tmp/test.frd".into())).unwrap();
        assert_eq!(r.freqs_hz.len(), 64);
        assert_eq!(r.gain_db.len(), 64);
        assert!((r.freqs_hz[0] - 20.0).abs() < 0.01);
        assert!((r.gain_db[0]).abs() < 0.01);
        assert!((r.freqs_hz.last().unwrap() - 20_000.0).abs() < 1.0);
        assert!((r.gain_db.last().unwrap() - 4.0).abs() < 0.01);
        assert_eq!(r.source_path.as_deref(), Some("/tmp/test.frd"));
    }

    #[test]
    fn parse_mic_curve_skips_comments() {
        let text = "# header\n* freq gain\n100 0.5\n200 0.8\n300 1.1\n400 1.4\n500 1.7\n\
                    600 2.0\n700 2.3\n800 2.6\n900 2.9\n1000 3.2\n1100 3.5\n1200 3.8\n\
                    1300 4.1\n1400 4.4\n1500 4.7\n1600 5.0\n1700 5.3\n";
        let r = parse_mic_curve(text, None).unwrap();
        assert_eq!(r.freqs_hz.len(), 17);
    }

    #[test]
    fn parse_mic_curve_third_column_ignored() {
        let mut text = String::new();
        for i in 0..20 {
            let f = 100.0_f32 * 1.2_f32.powi(i);
            text.push_str(&format!("{f}\t0.{i}\t-12.5\n"));
        }
        let r = parse_mic_curve(&text, None).unwrap();
        assert_eq!(r.freqs_hz.len(), 20);
    }

    #[test]
    fn parse_mic_curve_rejects_too_few_points() {
        let text = "100 0\n200 1\n300 2\n";
        let err = parse_mic_curve(text, None).unwrap_err();
        assert!(err.to_string().contains("too sparse"), "got {err}");
    }

    #[test]
    fn parse_mic_curve_rejects_non_monotonic() {
        let mut text = dummy_curve_text(20);
        text.push_str("50 0\n"); // out-of-order
        let err = parse_mic_curve(&text, None).unwrap_err();
        assert!(err.to_string().contains("strictly"), "got {err}");
    }

    #[test]
    fn parse_mic_curve_rejects_zero_freq() {
        let text = format!("0 0\n{}", dummy_curve_text(20));
        let err = parse_mic_curve(&text, None).unwrap_err();
        assert!(err.to_string().contains("> 0 Hz"), "got {err}");
    }

    #[test]
    fn correction_at_endpoints_clamps() {
        let r = parse_mic_curve(&dummy_curve_text(50), None).unwrap();
        // Below the first freq: clamps to first gain (0).
        assert!((r.correction_at(1.0) - 0.0).abs() < 0.01);
        // Above the last: clamps to last gain (4.0).
        assert!((r.correction_at(50_000.0) - 4.0).abs() < 0.01);
    }

    #[test]
    fn correction_at_interpolates_in_log_freq() {
        // Curve: linear gain ramp 0..4 dB over log(20..20k). At
        // geometric mid-point (sqrt(20*20000) ≈ 632), correction = 2 dB.
        let r = parse_mic_curve(&dummy_curve_text(50), None).unwrap();
        let mid = (20.0_f32 * 20_000.0).sqrt();
        let g = r.correction_at(mid);
        assert!(
            (g - 2.0).abs() < 0.1,
            "got {g} dB at f={mid}, expected ≈ 2.0"
        );
    }

    #[test]
    fn correction_at_negative_freq_clamps_to_first() {
        let r = parse_mic_curve(&dummy_curve_text(50), None).unwrap();
        assert!((r.correction_at(-100.0) - r.gain_db[0]).abs() < 1e-6);
    }
}
