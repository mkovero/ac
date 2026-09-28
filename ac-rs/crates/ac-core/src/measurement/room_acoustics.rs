//! Tier 1 — ISO 3382-1:2009 room acoustic parameters from an impulse
//! response: reverberation time (T20, T30), early decay time (EDT), clarity
//! (C50, C80) and definition (D50), per octave band, with the single-number
//! averages of Table A.1 (500 Hz and 1 kHz bands).
//!
//! The method, by clause:
//! - **Bands** — IEC 61260-1 octave bands, 125 Hz to 4 kHz, the range
//!   ISO 3382-1 §5.2.1 names, limited to the measured frequency range
//!   ([`crate::measurement::filterbank`]).
//! - **Start** (A.3.4) — the broadband trigger point, the first sample
//!   within 20 dB of the IR's maximum, for C and D; for a band's decay
//!   curve, the band's own trigger (its filtered response within 20 dB of
//!   its maximum), which A.3.4 gives when filtering first — a regression
//!   from the broadband start fits the filter's rise (EDT read 7 % long
//!   at 125 Hz on a synthetic decay).
//! - **C and D** (A.3.4's preferred approach) — the broadband IR is
//!   windowed at the early limit *before* filtering; the early and late
//!   parts are filtered separately and each part's energy counted with its
//!   ringing. Filtering first and shifting the split by half the filter
//!   delay (A.3.4's approximation) read C50 0.9 dB low at 250 Hz.
//! - **Decay curve** (§5.3.3) — backward integration of the band's squared
//!   IR, truncated at `t1`, where a line through the decay meets the
//!   band's background noise, plus the correction `C` for the energy an
//!   exponential decay at that line's rate carries past `t1`.
//! - **T20, T30** (§6) — least-squares fit over −5…−25 dB and −5…−35 dB of
//!   the decay curve, `T = 60/d`. **EDT** (A.2.2) — over 0…−10 dB.
//! - **Range** (§5.3.2) — a decay must start at least 35 dB above the band's
//!   background for T20 and 45 dB for T30; a band short of that gets no
//!   value, with the reason.
//! - **C50, C80, D50** (A.2.3, Eq. A.10–A.12) — early over late energy from
//!   the start, early interval `te`.
//!
//! Background noise is read as the mean squared band IR over the last
//! [`NOISE_TAIL_FRACTION`] of the capture, and the line meeting it is
//! fitted from the band's peak down to 10 dB above it on the 10 ms-smoothed
//! level: one pass of the §5.3.3 construction, not an iterative scheme.
//!
//! STI (IEC 60268-16) is not computed: that standard is not held here.

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

use crate::measurement::filterbank::Filterbank;
use crate::measurement::report::StandardsCitation;

/// Octave bands ISO 3382-1 §5.2.1 names: 125 Hz to 4 kHz.
pub const BAND_RANGE_HZ: (f64, f64) = (125.0, 4000.0);

/// Share of the capture, from its end, read as background noise. The tail
/// of a sweep capture past the room's decay (ISO 18233 §6.3.2 asks for
/// 30 dB of decay inside it).
pub const NOISE_TAIL_FRACTION: f64 = 0.1;

/// Smoothing window for the level the truncation line is fitted to, s.
const SMOOTH_S: f64 = 0.010;

/// One octave band's parameters. A `None` value carries its reason in
/// [`BandParams::refused`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BandParams {
    pub centre_hz: f64,
    pub t20_s: Option<f64>,
    pub t30_s: Option<f64>,
    pub edt_s: Option<f64>,
    pub c50_db: Option<f64>,
    pub c80_db: Option<f64>,
    pub d50: Option<f64>,
    /// Band peak over band background, dB — what §5.3.2's 35 / 45 dB is
    /// judged against.
    pub peak_to_noise_db: f64,
    /// Why a value is absent, one line each.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub refused: Vec<String>,
}

/// ISO 3382-1 parameters of one impulse response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RoomAcoustics {
    pub bands: Vec<BandParams>,
    /// Table A.1 single numbers: the arithmetic mean of the 500 Hz and
    /// 1 kHz octave bands, when both have a value.
    pub t20_mid_s: Option<f64>,
    pub t30_mid_s: Option<f64>,
    pub edt_mid_s: Option<f64>,
    pub c50_mid_db: Option<f64>,
    pub c80_mid_db: Option<f64>,
    pub d50_mid: Option<f64>,
    /// The broadband trigger point (A.3.4), seconds from the IR's first
    /// sample.
    pub start_s: f64,
    pub citation: StandardsCitation,
}

/// Compute the parameters of `ir` (the linear IR with its tail, as
/// captured), sampled at `sample_rate`, over the octave bands inside
/// `[f_lo, f_hi]` (the measured range) and [`BAND_RANGE_HZ`].
pub fn room_acoustics(ir: &[f64], sample_rate: u32, f_lo: f64, f_hi: f64) -> Result<RoomAcoustics> {
    let fs = sample_rate as f64;
    if ir.len() < (0.2 * fs) as usize {
        bail!("impulse response shorter than 0.2 s: too short for a decay");
    }
    let e: Vec<f64> = ir.iter().map(|x| x * x).collect();
    let e_max = e.iter().copied().fold(0.0, f64::max);
    if e_max <= 0.0 || e_max.is_nan() {
        bail!("impulse response is silent");
    }
    let start = e.iter().position(|&v| v >= e_max * 0.01).unwrap_or(0);

    // Octave bands whose edges sit inside the measured range (the
    // filterbank keeps a band only when both half-band edges do).
    let lo = f_lo.max(BAND_RANGE_HZ.0 / 2f64.sqrt());
    let hi = f_hi.min(BAND_RANGE_HZ.1 * 2f64.sqrt()).min(0.45 * fs - 1.0);
    let fb = Filterbank::new(sample_rate, 1, lo, hi)?;
    let bands: Vec<BandParams> = fb
        .centres_hz()
        .iter()
        .enumerate()
        .filter(|(_, &fc)| fc >= BAND_RANGE_HZ.0 * 0.99 && fc <= BAND_RANGE_HZ.1 * 1.01)
        .map(|(i, &fc)| band_params(&fb, i, fc, ir, start, fs))
        .collect();

    let mid = |f: fn(&BandParams) -> Option<f64>| {
        let pick = |c: f64| {
            bands
                .iter()
                .find(|b| (b.centre_hz / c - 1.0).abs() < 0.05)
                .and_then(f)
        };
        match (pick(500.0), pick(1000.0)) {
            (Some(a), Some(b)) => Some((a + b) / 2.0),
            _ => None,
        }
    };
    Ok(RoomAcoustics {
        t20_mid_s: mid(|b| b.t20_s),
        t30_mid_s: mid(|b| b.t30_s),
        edt_mid_s: mid(|b| b.edt_s),
        c50_mid_db: mid(|b| b.c50_db),
        c80_mid_db: mid(|b| b.c80_db),
        d50_mid: mid(|b| b.d50),
        bands,
        start_s: start as f64 / fs,
        citation: StandardsCitation {
            standard: "ISO 3382-1:2009".into(),
            clause: "§5.3.2 range, §5.3.3 backward integration, §6 T20/T30, A.2.2 EDT, \
                     A.2.3 C50/C80/D50, A.3.4 start and filter delay"
                .into(),
            verified: true,
        },
    })
}

/// Least-squares slope, dB per second, of `level` (dB) at the samples where
/// it lies in `[bottom, top]`, from `from` on. `None` if the curve never
/// reaches `bottom` or fewer than two samples qualify.
fn fit_slope(level: &[f64], from: usize, top: f64, bottom: f64, fs: f64) -> Option<f64> {
    let reach = level[from..].iter().position(|&l| l <= bottom)? + from;
    let (mut n, mut sx, mut sy, mut sxx, mut sxy) = (0.0, 0.0, 0.0, 0.0, 0.0);
    for (k, &l) in level.iter().enumerate().take(reach + 1).skip(from) {
        if l <= top && l >= bottom {
            let t = k as f64 / fs;
            n += 1.0;
            sx += t;
            sy += l;
            sxx += t * t;
            sxy += t * l;
        }
    }
    if n < 2.0 {
        return None;
    }
    let den = n * sxx - sx * sx;
    (den > 0.0).then(|| (n * sxy - sx * sy) / den)
}

fn band_params(
    fb: &Filterbank,
    i: usize,
    fc: f64,
    ir: &[f64],
    start: usize,
    fs: f64,
) -> BandParams {
    let y = fb.filter_band(i, ir).unwrap_or_default();
    let e: Vec<f64> = y.iter().map(|v| v * v).collect();
    let n = e.len();
    let mut refused = Vec::new();

    // Background noise: the mean over the capture's last tenth.
    let tail = ((n as f64 * NOISE_TAIL_FRACTION) as usize).max(1);
    let noise = e[n - tail..].iter().sum::<f64>() / tail as f64;
    let (peak_at, peak) =
        e.iter().enumerate().skip(start).fold(
            (start, 0.0),
            |acc, (k, &v)| if v > acc.1 { (k, v) } else { acc },
        );
    let _ = peak;

    // §5.3.3: t1 where a line through the decay meets the noise, and the
    // exponential-tail correction C at that line's rate.
    let w = ((SMOOTH_S * fs) as usize).max(1);
    let smooth: Vec<f64> = (0..n)
        .step_by(w)
        .map(|k| {
            let s = &e[k..(k + w).min(n)];
            10.0 * (s.iter().sum::<f64>() / s.len() as f64).max(1e-300).log10()
        })
        .collect();
    let noise_db = 10.0 * noise.max(1e-300).log10();
    let peak_blk = peak_at / w;
    // §5.3.2 judges the level the decay starts from: the 10 ms level, not
    // one sample — a noise-like decay's single-sample peak sits ~10 dB
    // above it.
    let start_blk = smooth[start / w..]
        .iter()
        .copied()
        .fold(f64::NEG_INFINITY, f64::max);
    let peak_to_noise_db = start_blk - noise_db;
    let line_end = smooth[peak_blk..]
        .iter()
        .position(|&l| l <= noise_db + 10.0)
        .map(|k| k + peak_blk);
    let (t1, correction) = match line_end.and_then(|end| {
        let s = fit_slope(&smooth, peak_blk, f64::INFINITY, smooth[end], fs / w as f64)?;
        (s < 0.0).then_some((end, s))
    }) {
        Some((end, slope)) => {
            // The line through (peak_blk, smooth[peak_blk]) at `slope` dB/s
            // meets noise_db at t1.
            let t_peak = peak_blk as f64 * w as f64 / fs;
            let t1_s = t_peak + (noise_db - smooth[peak_blk]) / slope;
            let t1 = ((t1_s * fs) as usize).clamp(end * w, n - 1);
            let d = -slope; // dB/s
            let c = noise * fs * 10.0 / (d * std::f64::consts::LN_10);
            (t1, c)
        }
        None => {
            refused.push("no decay found above the band's background".to_string());
            (n - 1, 0.0)
        }
    };

    // The band's own trigger (A.3.4): its response within 20 dB of its
    // maximum. The decay curve and its fits start there.
    let band_start = e[start..]
        .iter()
        .position(|&v| v >= peak * 0.01)
        .map_or(start, |k| k + start);

    // Backward-integrated decay curve, dB re its total from the band start.
    let mut sched = vec![0.0; t1 + 1];
    let mut acc = correction;
    for k in (0..=t1).rev() {
        acc += e[k];
        sched[k] = acc;
    }
    let from = band_start.min(t1);
    let total = sched[from];
    let level: Vec<f64> = sched
        .iter()
        .map(|&v| 10.0 * (v / total).max(1e-300).log10())
        .collect();

    let rt = |top: f64, bottom: f64| fit_slope(&level, from, top, bottom, fs).map(|s| -60.0 / s);
    let t20_s = if peak_to_noise_db >= 35.0 {
        rt(-5.0, -25.0)
    } else {
        refused.push(format!(
            "T20: decay starts {peak_to_noise_db:.1} dB above the background; ISO 3382-1 §5.3.2 needs 35"
        ));
        None
    };
    let t30_s = if peak_to_noise_db >= 45.0 {
        rt(-5.0, -35.0)
    } else {
        refused.push(format!(
            "T30: decay starts {peak_to_noise_db:.1} dB above the background; ISO 3382-1 §5.3.2 needs 45"
        ));
        None
    };
    let edt_s = rt(0.0, -10.0);

    // A.2.3, windowed before filtering (A.3.4): the broadband IR split at
    // `start + te`, each part filtered on its own, each part's energy
    // counted with its ringing — the early part to `t1`, the late part to
    // `t1` plus the tail correction.
    let ratio = |te: f64| {
        let split = (start + (te * fs) as usize).min(ir.len());
        let mut early_in = vec![0.0; ir.len()];
        early_in[start..split].copy_from_slice(&ir[start..split]);
        let mut late_in = vec![0.0; ir.len()];
        late_in[split..].copy_from_slice(&ir[split..]);
        let band_energy = |x: &[f64]| -> f64 {
            fb.filter_band(i, x)
                .unwrap_or_default()
                .iter()
                .take(t1 + 1)
                .map(|v| v * v)
                .sum()
        };
        let early = band_energy(&early_in);
        let late = band_energy(&late_in) + correction;
        (early > 0.0 && late > 0.0).then_some((early, late))
    };
    let c50_db = ratio(0.050).map(|(a, b)| 10.0 * (a / b).log10());
    let c80_db = ratio(0.080).map(|(a, b)| 10.0 * (a / b).log10());
    let d50 = ratio(0.050).map(|(a, b)| a / (a + b));

    BandParams {
        centre_hz: fc,
        t20_s,
        t30_s,
        edt_s,
        c50_db,
        c80_db,
        d50,
        peak_to_noise_db,
        refused,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FS: u32 = 48_000;

    /// The nominal octave (IEC 61260-1 Annex E) nearest `fc`.
    fn nominal(fc: f64) -> f64 {
        [63.0, 125.0, 250.0, 500.0, 1000.0, 2000.0, 4000.0, 8000.0]
            .into_iter()
            .min_by(|a, b| (fc / a).ln().abs().total_cmp(&(fc / b).ln().abs()))
            .unwrap()
    }

    /// Deterministic unit-variance noise.
    fn noise(n: usize, seed: u64) -> Vec<f64> {
        let mut s = seed;
        (0..n)
            .map(|_| {
                let mut acc = 0.0;
                for _ in 0..12 {
                    s = s
                        .wrapping_mul(6364136223846793005)
                        .wrapping_add(1442695040888963407);
                    acc += (s >> 11) as f64 / (1u64 << 53) as f64;
                }
                acc - 6.0
            })
            .collect()
    }

    /// Exponentially decaying noise with reverberation time `rt` (60 dB of
    /// energy in `rt` s), then a floor `floor_db` below the start, `len` s;
    /// `seed` picks the noise realisation.
    fn room(rt: f64, floor_db: f64, len: f64, seed: u64) -> Vec<f64> {
        let n = (len * FS as f64) as usize;
        let decay = noise(n, 7 + seed);
        let floor = noise(n, 99 + seed);
        let a_floor = 10f64.powf(floor_db / 20.0);
        (0..n)
            .map(|k| {
                let t = k as f64 / FS as f64;
                decay[k] * 10f64.powf(-3.0 * t / rt) + a_floor * floor[k]
            })
            .collect()
    }

    /// Each band's value averaged over [`SEEDS`] noise realisations: one
    /// realisation of decaying noise scatters ±1 dB in a 50 ms window at
    /// 500 Hz, which is the noise, not the method.
    const SEEDS: u64 = 8;

    fn ensemble(rt: f64, floor_db: f64, f: fn(&BandParams) -> Option<f64>) -> Vec<(f64, f64)> {
        ensemble_n(SEEDS, rt, floor_db, f)
    }

    fn ensemble_n(
        seeds: u64,
        rt: f64,
        floor_db: f64,
        f: fn(&BandParams) -> Option<f64>,
    ) -> Vec<(f64, f64)> {
        let runs: Vec<RoomAcoustics> = (0..seeds)
            .map(|s| room_acoustics(&room(rt, floor_db, 3.0, s), FS, 20.0, 20_000.0).unwrap())
            .collect();
        (0..runs[0].bands.len())
            .map(|i| {
                let vals: Vec<f64> = runs
                    .iter()
                    .map(|r| f(&r.bands[i]).unwrap_or_else(|| panic!("{:?}", r.bands[i].refused)))
                    .collect();
                (
                    runs[0].bands[i].centre_hz,
                    vals.iter().sum::<f64>() / vals.len() as f64,
                )
            })
            .collect()
    }

    /// T20, T30 and EDT of an exponential decay read its reverberation
    /// time, within 5 % on the ensemble mean (the bound ISO 3382-1 §5.3.3
    /// states for the uncorrected case, used as the bar for the corrected
    /// one). EDT from 250 Hz up: its 0…−10 dB span is ~0.13 s, about 11
    /// degrees of freedom in the 88 Hz-wide 125 Hz band — the synthetic
    /// noise, not the method, sets the scatter there.
    #[test]
    fn reverberation_times_of_an_exponential_decay() {
        let r = room_acoustics(&room(0.8, -80.0, 3.0, 0), FS, 20.0, 20_000.0).unwrap();
        let nominal: Vec<f64> = r.bands.iter().map(|b| nominal(b.centre_hz)).collect();
        assert_eq!(nominal, [125.0, 250.0, 500.0, 1000.0, 2000.0, 4000.0]);
        for (name, lowest, f) in [
            (
                "T20",
                0.0,
                (|b: &BandParams| b.t20_s) as fn(&BandParams) -> Option<f64>,
            ),
            ("T30", 0.0, |b: &BandParams| b.t30_s),
            ("EDT", 200.0, |b: &BandParams| b.edt_s),
        ] {
            for (fc, v) in ensemble(0.8, -80.0, f)
                .into_iter()
                .filter(|(fc, _)| *fc >= lowest)
            {
                assert!((v / 0.8 - 1.0).abs() < 0.05, "{fc:.0} Hz {name} {v:.3} s");
            }
        }
        let mid = r.t30_mid_s.unwrap();
        assert!(
            (mid / 0.8 - 1.0).abs() < 0.08,
            "T30 mid of one realisation {mid}"
        );
    }

    /// C50, C80 and D50 of an exponential decay against their closed forms:
    /// with energy decaying at k = 6·ln10 / RT per second,
    /// C = 10·lg(e^{k·te} − 1) and D = 1 − e^{−k·te}. Averaged as D (a
    /// linear energy share) and only then taken to dB: a mean of dB values
    /// over ~9 degrees of freedom sits ~0.5 dB low (Jensen), which is the
    /// average, not the method. One realisation's D50 at 250 Hz scatters
    /// σ ≈ 0.11 (D(1−D)·√(2/9)); over 32 the mean's σ is 0.02, and the bar
    /// is 2.5σ: 0.05 in D, 0.6 dB in C. From 250 Hz up (the 125 Hz octave's
    /// own filter delay is a large share of 50 ms).
    #[test]
    fn clarity_and_definition_of_an_exponential_decay() {
        let rt = 0.8;
        let k = 6.0 * std::f64::consts::LN_10 / rt;
        let d = |te: f64| 1.0 - (-k * te).exp();
        let c = |te: f64| 10.0 * ((k * te).exp() - 1.0).log10();
        fn d_of_c(c_db: f64) -> f64 {
            let r = 10f64.powf(c_db / 10.0);
            r / (1.0 + r)
        }
        let c_of_d = |d: f64| 10.0 * (d / (1.0 - d)).log10();
        for (fc, d50) in ensemble_n(32, rt, -80.0, |b| b.d50)
            .into_iter()
            .filter(|(fc, _)| *fc >= 200.0)
        {
            assert!(
                (d50 - d(0.05)).abs() < 0.05,
                "{fc:.0} Hz D50 {d50:.3}, want {:.3}",
                d(0.05)
            );
            assert!(
                (c_of_d(d50) - c(0.05)).abs() < 0.6,
                "{fc:.0} Hz C50 via D50"
            );
        }
        for (fc, d80) in ensemble_n(32, rt, -80.0, |b| b.c80_db.map(d_of_c))
            .into_iter()
            .filter(|(fc, _)| *fc >= 200.0)
        {
            assert!(
                (c_of_d(d80) - c(0.08)).abs() < 0.6,
                "{fc:.0} Hz C80 {:.2}, want {:.2}",
                c_of_d(d80),
                c(0.08)
            );
        }
    }

    /// The rejected construction — integrating the noise floor to the end
    /// of the capture — bends the decay curve and overstates T20; the
    /// §5.3.3 truncation with correction does not.
    #[test]
    fn truncation_at_the_noise_removes_the_bias_the_untruncated_integral_has() {
        let rt = 0.5;
        let ir = room(rt, -40.0, 3.0, 0);
        let fb = Filterbank::new(FS, 1, 700.0, 1420.0).unwrap();
        let y = fb.filter_band(0, &ir).unwrap();
        let e: Vec<f64> = y.iter().map(|v| v * v).collect();
        let mut acc = 0.0;
        let mut sched: Vec<f64> = e
            .iter()
            .rev()
            .map(|v| {
                acc += v;
                acc
            })
            .collect();
        sched.reverse();
        let level: Vec<f64> = sched
            .iter()
            .map(|v| 10.0 * (v / sched[0]).log10())
            .collect();
        let naive_t20 = -60.0 / fit_slope(&level, 0, -5.0, -25.0, FS as f64).unwrap();
        assert!(
            naive_t20 / rt - 1.0 > 0.10,
            "the untruncated T20 was meant to overstate: {naive_t20}"
        );

        for (fc, t20) in ensemble(rt, -40.0, |b| b.t20_s)
            .into_iter()
            .filter(|(fc, _)| *fc > 900.0 && *fc < 1100.0)
        {
            assert!(
                (t20 / rt - 1.0).abs() < 0.05,
                "{fc:.0} Hz truncated T20 {t20}"
            );
        }
    }

    /// A band without the §5.3.2 range gets no T20/T30, and says why.
    #[test]
    fn short_range_is_refused_with_the_reason() {
        let r = room_acoustics(&room(0.8, -30.0, 3.0, 0), FS, 20.0, 20_000.0).unwrap();
        let b = &r.bands[3];
        assert_eq!(
            (b.t20_s, b.t30_s),
            (None, None),
            "range {:.1} dB",
            b.peak_to_noise_db
        );
        assert!(
            b.refused
                .iter()
                .any(|s| s.starts_with("T20:") && s.contains("needs 35")),
            "{:?}",
            b.refused
        );
        assert!(b
            .refused
            .iter()
            .any(|s| s.starts_with("T30:") && s.contains("needs 45")));
    }

    #[test]
    fn a_narrow_measured_range_keeps_only_the_bands_inside_it() {
        let r = room_acoustics(&room(0.8, -80.0, 3.0, 0), FS, 300.0, 3000.0).unwrap();
        let nominal: Vec<f64> = r.bands.iter().map(|b| nominal(b.centre_hz)).collect();
        assert_eq!(nominal, [500.0, 1000.0, 2000.0]);
    }
}
