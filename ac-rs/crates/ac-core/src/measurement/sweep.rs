//! Tier 1 — Farina exponential-sweep impulse response measurement.
//!
//! Per Farina 2000, *Simultaneous measurement of impulse response and
//! distortion with a swept-sine technique*, AES 108th convention preprint
//! #5093, §2 "Theoretical basis". Verified against the full preprint at
//! `stddocs/iec-full/Simultaneous_Measurement_of_Impulse_Response_and_D.pdf`:
//! the log-sweep `x(t) = sin[K·(e^(t/L) − 1)]` with `K = T·ω1/ln(ω2/ω1)`
//! and `L = T/ln(ω2/ω1)`, the `exp(-t/L)` inverse-filter envelope, and
//! the harmonic offset `Δt_N = T·ln(N)/ln(ω2/ω1)` all match the formulae
//! implemented below.
//!
//! The technique:
//! 1. Drive the DUT with a logarithmic (exponential) sine sweep `x(t)`
//!    covering `[f1, f2]` over `T` seconds.
//! 2. Record the response `y(t)`.
//! 3. Convolve `y` with the time-reversed, amplitude-modulated inverse
//!    filter `x_inv(t)` — Farina's closed-form inverse that makes
//!    `x(t) ∗ x_inv(t) ≈ δ(t−T)`.
//! 4. The linear IR appears centred at the end of the convolution
//!    (offset `≈ N−1` for equal-length sweeps). k-th-order harmonic IRs
//!    appear earlier at known offsets
//!    `Δt_k = (T / ln(f2/f1)) · ln(k)` seconds before the linear IR,
//!    because the k-th harmonic of an exponential sweep is the
//!    fundamental of a time-shifted version of the same sweep.
//!
//! Time-gating the pre-impulse region into windows centred at each
//! `Δt_k` yields per-order harmonic impulse responses, suitable for
//! calculating a frequency-resolved THD curve.

use std::f64::consts::PI;

use anyhow::{bail, Result};
use realfft::RealFftPlanner;
use serde::{Deserialize, Serialize};

use crate::measurement::filterbank::Filterbank;
use crate::measurement::report::StandardsCitation;

/// Parameters for a Farina log sweep.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SweepParams {
    pub f1_hz: f64,
    pub f2_hz: f64,
    pub duration_s: f64,
    pub sample_rate: u32,
}

impl SweepParams {
    pub fn validate(&self) -> Result<()> {
        if self.sample_rate == 0 {
            bail!("sample_rate must be positive");
        }
        if !self.f1_hz.is_finite() || !self.f2_hz.is_finite() || !self.duration_s.is_finite() {
            bail!("non-finite parameter");
        }
        if self.f1_hz <= 0.0 {
            bail!("f1_hz must be positive (got {})", self.f1_hz);
        }
        if self.f2_hz <= self.f1_hz {
            bail!(
                "f2_hz must exceed f1_hz (got f1={}, f2={})",
                self.f1_hz,
                self.f2_hz
            );
        }
        if self.duration_s <= 0.0 {
            bail!("duration_s must be positive (got {})", self.duration_s);
        }
        if self.f2_hz >= self.sample_rate as f64 * 0.5 {
            bail!(
                "f2_hz must be below Nyquist ({} Hz); got {}",
                self.sample_rate as f64 * 0.5,
                self.f2_hz
            );
        }
        Ok(())
    }

    pub fn n_samples(&self) -> usize {
        (self.duration_s * self.sample_rate as f64).round() as usize
    }

    /// `L = T / ln(f2/f1)` — the exponential-sweep time constant.
    /// Instantaneous frequency is `f1 · exp(t / L)`.
    pub fn time_constant(&self) -> f64 {
        self.duration_s / (self.f2_hz / self.f1_hz).ln()
    }

    /// Time offset at which the k-th harmonic IR appears BEFORE the
    /// linear IR in a Farina deconvolution, in seconds.
    ///
    /// `Δt_k = L · ln(k)`. `k = 1` returns 0.
    pub fn harmonic_time_offset_s(&self, k: u32) -> f64 {
        if k == 0 {
            return 0.0;
        }
        self.time_constant() * (k as f64).ln()
    }
}

/// Generate the exponential sine sweep `x(t)` at unit peak amplitude.
pub fn log_sweep(p: &SweepParams) -> Result<Vec<f32>> {
    p.validate()?;
    let n = p.n_samples();
    let l = p.time_constant();
    let k_phase = 2.0 * PI * p.f1_hz * l;
    let fs = p.sample_rate as f64;
    Ok((0..n)
        .map(|i| {
            let t = i as f64 / fs;
            let phase = k_phase * ((t / l).exp() - 1.0);
            phase.sin() as f32
        })
        .collect())
}

/// Generate Farina's inverse filter `x_inv(t)`: the time-reversed sweep,
/// amplitude-modulated by `exp(-(T-t)/L)` so that the spectrum of
/// `x(t) * x_inv(t)` is flat and the convolution approximates a unit
/// impulse at `t = T` regardless of `log(f2/f1)`.
///
/// The returned buffer is normalised so that for a unity-amplitude sweep
/// `log_sweep(p)` the peak of `deconvolve_full(log_sweep(p), x_inv)` is
/// unity — i.e. an identity system yields a unit-magnitude IR. Without
/// that normalisation the peak carries an arbitrary Farina scale factor
/// that users would otherwise have to back out by hand.
pub fn inverse_sweep(p: &SweepParams) -> Result<Vec<f32>> {
    p.validate()?;
    let n = p.n_samples();
    let fs = p.sample_rate as f64;
    let l = p.time_constant();
    let k_phase = 2.0 * PI * p.f1_hz * l;
    let a = 1.0 / l;
    let t_end = (n - 1) as f64 / fs;

    let mut inv: Vec<f64> = (0..n)
        .map(|i| {
            let t = i as f64 / fs;
            // Sample value of forward sweep at time (t_end - t):
            let t_fwd = t_end - t;
            let phase = k_phase * ((t_fwd / l).exp() - 1.0);
            let env = (-a * t_fwd).exp();
            phase.sin() * env
        })
        .collect();

    // Normalise so the identity-system IR has peak magnitude 1. Farina's
    // construction places that peak exactly at convolution lag `N-1`
    // (= length-N forward ⊛ length-N reverse, the centre sample of the
    // 2N-1 output). Discrete sampling can shift the maximum by ±1 sample,
    // so evaluate the convolution at a small window of central lags via
    // direct dot products and take the max. O(k·N) for `k` lags beats
    // the previous full FFT convolution (one FFT + one IFFT of
    // next_pow_of_2(2N-1)) by an order of magnitude on long sweeps.
    let x: Vec<f64> = log_sweep(p)?.into_iter().map(|v| v as f64).collect();
    const HALF_WIN: i64 = 4;
    let centre = n as i64 - 1;
    let full_len = 2 * n - 1;
    let mut peak = 0.0_f64;
    for offset in -HALF_WIN..=HALF_WIN {
        let m = centre + offset;
        if m < 0 || (m as usize) >= full_len {
            continue;
        }
        let m = m as usize;
        let k_lo = m.saturating_sub(n - 1);
        let k_hi = m.min(n - 1);
        let mut s = 0.0_f64;
        for k in k_lo..=k_hi {
            s += x[k] * inv[m - k];
        }
        let av = s.abs();
        if av > peak {
            peak = av;
        }
    }
    if peak > 0.0 && peak.is_finite() {
        let scale = 1.0 / peak;
        for v in &mut inv {
            *v *= scale;
        }
    }

    Ok(inv.into_iter().map(|v| v as f32).collect())
}

/// Full linear convolution of `y` and `x_inv` via FFT. Returned length
/// is `y.len() + x_inv.len() - 1`. All math `f64` internally.
pub fn deconvolve_full(y: &[f32], x_inv: &[f32]) -> Vec<f64> {
    let y64: Vec<f64> = y.iter().map(|&v| v as f64).collect();
    let inv64: Vec<f64> = x_inv.iter().map(|&v| v as f64).collect();
    fft_linear_convolve(&y64, &inv64)
}

fn fft_linear_convolve(a: &[f64], b: &[f64]) -> Vec<f64> {
    if a.is_empty() || b.is_empty() {
        return Vec::new();
    }
    let out_len = a.len() + b.len() - 1;
    let n = out_len.next_power_of_two();
    let mut planner = RealFftPlanner::<f64>::new();
    let fft = planner.plan_fft_forward(n);
    let ifft = planner.plan_fft_inverse(n);

    let mut ap = vec![0.0_f64; n];
    ap[..a.len()].copy_from_slice(a);
    let mut bp = vec![0.0_f64; n];
    bp[..b.len()].copy_from_slice(b);

    let mut a_spec = fft.make_output_vec();
    let mut b_spec = fft.make_output_vec();
    fft.process(&mut ap, &mut a_spec).unwrap();
    fft.process(&mut bp, &mut b_spec).unwrap();
    for (s_a, s_b) in a_spec.iter_mut().zip(b_spec.iter()) {
        *s_a *= *s_b;
    }
    let mut out = vec![0.0_f64; n];
    ifft.process(&mut a_spec, &mut out).unwrap();
    let norm = 1.0 / n as f64;
    for v in &mut out {
        *v *= norm;
    }
    out.truncate(out_len);
    out
}

/// A single harmonic-order impulse response extracted from a Farina
/// deconvolution.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HarmonicIr {
    pub order: u32,
    pub samples: Vec<f64>,
}

/// Outcome of `extract_irs`: the linear IR plus per-order harmonic IRs.
#[derive(Debug, Clone, PartialEq)]
pub struct DeconvolvedIrs {
    pub linear: Vec<f64>,
    pub harmonics: Vec<HarmonicIr>,
    /// The `window_len` the caller asked `extract_irs` for, before any
    /// clamping.
    pub window_len_requested: usize,
    /// Gate length actually used per order: index `i` is order `i + 1`, so
    /// index 0 is the linear IR. Each entry is `window_len_requested`
    /// clamped down to the sample distance to that order's nearest
    /// neighbouring order — see [`extract_irs`]. This is the gate-length
    /// decision, so it stays populated for an order whose gate fell off
    /// the front of `full` and therefore has empty `samples`.
    pub window_len_used: Vec<usize>,
    /// The sweep parameters `extract_irs` was called with. These set the
    /// adjacent-harmonic-order gap (`Δt_k = duration_s · ln(k) /
    /// ln(f2_hz/f1_hz)`) that [`Self::clamp_note`] names as the reason for
    /// any clamp — kept here rather than re-threaded through the call site
    /// so the note can be produced from `self` alone.
    pub params: SweepParams,
}

impl DeconvolvedIrs {
    /// Gate length used for harmonic `order` (order 1 = the linear IR),
    /// or `None` if that order was not extracted.
    pub fn window_len_for(&self, order: u32) -> Option<usize> {
        let idx = (order as usize).checked_sub(1)?;
        self.window_len_used.get(idx).copied()
    }

    /// One-line operator-facing note naming every order whose gate was
    /// clamped below the requested length, or `None` when the request was
    /// honoured for every order. Meant for `MeasurementReport.notes`: a
    /// shortened gate changes what the harmonic IRs mean, so it must not
    /// reach the operator as a silent substitution.
    ///
    /// Names the reason (the adjacent-harmonic-order gap) and the sweep
    /// parameters that set it (#342) — a caller who only sees "clamped to
    /// N samples" has no knob to turn; one who sees `duration_s`/`f1_hz`/
    /// `f2_hz` does.
    pub fn clamp_note(&self) -> Option<String> {
        let clamped: Vec<String> = self
            .window_len_used
            .iter()
            .enumerate()
            .filter(|(_, &w)| w < self.window_len_requested)
            .map(|(i, &w)| format!("order {} \u{2192} {} samples", i + 1, w))
            .collect();
        if clamped.is_empty() {
            return None;
        }
        Some(format!(
            "harmonic gate window clamped below the requested {} samples so \
             adjacent orders do not overlap: {}. Orders not listed used the \
             requested length. The adjacent-harmonic-order gap is set by \
             f1_hz={} Hz, f2_hz={} Hz, duration_s={} s (sample_rate={} Hz) \
             — widen it by increasing duration_s or narrowing the \
             f2_hz/f1_hz span.",
            self.window_len_requested,
            clamped.join(", "),
            self.params.f1_hz,
            self.params.f2_hz,
            self.params.duration_s,
            self.params.sample_rate,
        ))
    }
}

/// Sample offsets of the harmonic-order gate centres, orders `1..=n`,
/// measured back from the linear IR (so index 0 is always 0 and the
/// sequence is non-decreasing).
fn harmonic_offsets_samples(p: &SweepParams, n_harmonics: usize) -> Vec<i64> {
    let fs = p.sample_rate as f64;
    (1..=n_harmonics as u32)
        .map(|k| (p.harmonic_time_offset_s(k) * fs).round() as i64)
        .collect()
}

/// Per-order gate lengths for `window_len`, each clamped down to the
/// sample distance to that order's nearest neighbouring order.
///
/// Clamping per order rather than globally is what keeps the linear IR
/// usable: order 1's only neighbour is order 2, which for a 1 s 20 Hz–
/// 20 kHz sweep sits ~4816 samples away at 48 kHz, while the narrowest
/// gap in the set (order 4 → 5) is ~1551. A single global clamp would cut
/// the linear IR — the measurement's primary output — to the spacing of
/// the highest orders, which constrain nothing about it.
///
/// Because neighbouring orders `k` and `k+1` are each clamped to at most
/// their shared gap `g`, their facing half-windows sum to at most `g`, so
/// the gates cannot overlap. Errors if a gap rounds to zero or below, at
/// which point no non-overlapping gate exists at all.
fn per_order_window_lens(
    p: &SweepParams,
    n_harmonics: usize,
    window_len: usize,
) -> Result<Vec<usize>> {
    let offsets = harmonic_offsets_samples(p, n_harmonics);
    let mut used = vec![window_len; n_harmonics];
    for i in 0..n_harmonics.saturating_sub(1) {
        let gap = offsets[i + 1] - offsets[i];
        if gap <= 0 {
            bail!(
                "adjacent-harmonic spacing between orders {} and {} rounds to \
                 {} samples (f1={} Hz, f2={} Hz, duration_s={}, \
                 sample_rate={}); no non-overlapping gate exists — reduce \
                 n_harmonics or increase duration_s",
                i + 1,
                i + 2,
                gap,
                p.f1_hz,
                p.f2_hz,
                p.duration_s,
                p.sample_rate
            );
        }
        let gap = gap as usize;
        used[i] = used[i].min(gap);
        used[i + 1] = used[i + 1].min(gap);
    }
    Ok(used)
}

/// Split the full deconvolution output `full` into the linear IR plus
/// `n_harmonics - 1` pre-impulse harmonic IRs.
///
/// `full` is the output of [`deconvolve_full`] on a recording of a
/// sweep generated by [`log_sweep`] on `p`. `window_len` is the gate
/// length (samples) requested for each IR. Gates for adjacent orders must
/// not overlap, or the orders cross-contaminate, so each order's gate is
/// clamped down to the sample distance to its nearest neighbouring order.
/// The lengths actually used are reported back as
/// [`DeconvolvedIrs::window_len_used`], with
/// [`DeconvolvedIrs::clamp_note`] rendering them for an operator. With
/// `n_harmonics == 1` there is no neighbour and `window_len` is used as
/// asked.
///
/// The linear IR is centred at the sweep endpoint (sample `N−1` of the
/// forward sweep). Each harmonic IR is centred at
/// `linear_centre − round(Δt_k · fs)`.
pub fn extract_irs(
    full: &[f64],
    p: &SweepParams,
    n_harmonics: usize,
    window_len: usize,
) -> Result<DeconvolvedIrs> {
    p.validate()?;
    if n_harmonics == 0 {
        bail!("n_harmonics must be ≥ 1");
    }
    if window_len == 0 {
        bail!("window_len must be ≥ 1");
    }
    let n_sweep = p.n_samples();
    if full.len() < n_sweep {
        bail!(
            "convolution output too short: got {} samples, need at least {}",
            full.len(),
            n_sweep
        );
    }
    let window_len_used = per_order_window_lens(p, n_harmonics, window_len)?;

    let linear_centre = n_sweep - 1;
    let linear = gate(full, linear_centre, window_len_used[0]);

    let mut harmonics = Vec::with_capacity(n_harmonics.saturating_sub(1));
    let fs = p.sample_rate as f64;
    for k in 2..=(n_harmonics as u32) {
        let dt = p.harmonic_time_offset_s(k);
        let offset = (dt * fs).round() as i64;
        let centre = linear_centre as i64 - offset;
        if centre < 0 {
            harmonics.push(HarmonicIr {
                order: k,
                samples: Vec::new(),
            });
            continue;
        }
        let samples = gate(full, centre as usize, window_len_used[k as usize - 1]);
        harmonics.push(HarmonicIr { order: k, samples });
    }
    Ok(DeconvolvedIrs {
        linear,
        harmonics,
        window_len_requested: window_len,
        window_len_used,
        params: *p,
    })
}

/// Return `window_len` samples centred on `centre` within `buf`, padding
/// with zeros outside the buffer. The IR peak is placed at
/// `window_len / 2`.
fn gate(buf: &[f64], centre: usize, window_len: usize) -> Vec<f64> {
    let half = window_len / 2;
    let start = centre as i64 - half as i64;
    (0..window_len)
        .map(|i| {
            let idx = start + i as i64;
            if idx < 0 || (idx as usize) >= buf.len() {
                0.0
            } else {
                buf[idx as usize]
            }
        })
        .collect()
}

/// Outcome of [`check_tail_decay`].
#[derive(Debug, Clone, PartialEq)]
pub struct TailDecayCheck {
    /// Bands-per-octave the check ran at (fixed at 1/3-octave — the
    /// resolution ISO 18233 §6.3.2's "each fractional-octave band"
    /// language is conventionally read at).
    pub bpo: u32,
    /// Centre frequency of the worst-margin band, Hz.
    pub worst_band_hz: f64,
    /// Smallest per-band decay observed from the linear-IR peak to the
    /// end of the captured tail, dB.
    pub worst_decay_db: f64,
    /// ISO 18233 §6.3.2's required decay, dB (30).
    pub required_db: f64,
    pub passed: bool,
    /// How many of `bands_total` in-range bands had a long enough analysis
    /// window to clear their own settling prefix and be candidates for the
    /// worst-case comparison (whether or not they ended up carrying
    /// measurable energy). `bands_settled < bands_total` means the capture
    /// was too short for `tail_s` to say anything about the missing bands —
    /// distinct from a band settling fine and genuinely reading silence.
    pub bands_settled: usize,
    /// Count of 1/3-octave bands in `[f1_hz, f2_hz]` this check considered.
    pub bands_total: usize,
}

impl TailDecayCheck {
    /// One-line verdict, meant for `MeasurementReport.notes` — this is
    /// where acceptance criterion 6 (issue #282) puts the tail_s basis:
    /// not a pre-capture guess, a stated post-hoc check against the room
    /// actually measured.
    pub fn note(&self) -> String {
        let coverage = if self.bands_settled < self.bands_total {
            format!(
                " ({} of {} in-range 1/{}-oct bands never cleared their filter's settling \
                 prefix within this tail_s and were not evaluated \u{2014} increase tail_s to \
                 cover them.)",
                self.bands_total - self.bands_settled,
                self.bands_total,
                self.bpo
            )
        } else {
            String::new()
        };
        if self.passed {
            format!(
                "ISO 18233 \u{a7}6.3.2 tail-decay check: worst-case 1/{}-oct {:.0} Hz band decayed \
                 {:.1} dB over the captured tail (need \u{2265}{:.0} dB) \u{2014} capture adequate.{}",
                self.bpo, self.worst_band_hz, self.worst_decay_db, self.required_db, coverage
            )
        } else {
            format!(
                "ISO 18233 \u{a7}6.3.2 tail-decay check FAILED: 1/{}-oct {:.0} Hz band only decayed \
                 {:.1} dB over the captured tail (need \u{2265}{:.0} dB) \u{2014} this capture may be \
                 unreliable for band-resolved work; re-run with a longer tail_s.{}",
                self.bpo, self.worst_band_hz, self.worst_decay_db, self.required_db, coverage
            )
        }
    }
}

/// Post-hoc verification that the captured tail satisfies ISO 18233
/// §6.3.2: "the capture covers from the start of excitation until the
/// response in each fractional-octave band has decayed by more than
/// 30 dB." Per ISO 18233 B.2, sweep duration is not related to
/// reverberation time (unlike periodic excitation), so there is no
/// pre-capture RT60 estimator this crate could use to size `tail_s`
/// ahead of a real room — the check instead runs after deconvolution,
/// against the room actually measured. This inline reference to the ISO
/// clause is not a `StandardsCitation`: nobody here has cross-checked it
/// against the published ISO 18233 text (see `report.rs`'s
/// `every_measurement_module_emits_populated_citation` and
/// `ARCHITECTURE.md`'s citation-audit workflow), and this repo does not
/// carry that PDF under `stddocs/iec-full/` to check against.
///
/// Compares the in-band level of a short window right at the linear-IR
/// peak (`early`) against a window of the same length at the end of the
/// captured tail (`late`), per 1/3-octave (IEC 61260-1) band across
/// `[f1_hz, f2_hz]`. Reports the worst (smallest) per-band decay.
///
/// The early/late window is sized to clear the narrowest (lowest-frequency,
/// longest time-constant) in-range band's own settling prefix — capped at
/// half the tail so the two windows never overlap — rather than a flat
/// quarter of `tail_len`. A flat quarter split left the lowest 1/3-oct
/// bands permanently `NEG_INFINITY` from [`Filterbank::process`] (settling
/// prefix longer than the window) at the daemon's own shipped `tail_s`
/// default, which the old code folded into the same "no measurable energy"
/// bucket as genuine silence and silently dropped from the worst-case
/// comparison — invisible even though those bands carried real energy and
/// are exactly the ones ISO 18233 §6.3.2 is hardest on. Bands that still
/// can't clear their settling prefix within the capped window (`tail_s`
/// itself too short for the room, not a windowing artefact) are counted in
/// `TailDecayCheck::bands_settled` / surfaced in `TailDecayCheck::note`
/// instead; bands that settle fine and genuinely read back silence are
/// still excluded from the worst-case pick — there is nothing there to
/// decay.
///
/// `full` is the full [`deconvolve_full`] output (not the windowed
/// `DeconvolvedIrs::linear` from `extract_irs`) — `tail_s` of captured
/// signal past the sweep endpoint has to still be present to check.
pub fn check_tail_decay(full: &[f64], p: &SweepParams, tail_s: f64) -> Result<TailDecayCheck> {
    p.validate()?;
    if !tail_s.is_finite() || tail_s <= 0.0 {
        bail!("tail_s must be positive (got {tail_s})");
    }
    const REQUIRED_DB: f64 = 30.0;
    const BPO: usize = 3;

    let fs = p.sample_rate as f64;
    let linear_centre = p.n_samples().saturating_sub(1);
    let tail_len = ((tail_s * fs).round() as usize).min(full.len().saturating_sub(linear_centre));

    let f_min = p.f1_hz.max(20.0);
    let f_max = p.f2_hz.min(fs * 0.45 - 1.0);
    let fb = Filterbank::new(p.sample_rate, BPO, f_min, f_max)?;
    let settle = fb.settle_samples();
    let max_settle = settle.iter().copied().max().unwrap_or(0);
    // Quarter-tail is the floor (unchanged for short tails / high f_min,
    // where no in-range band needs more); otherwise grow to clear the
    // widest settling prefix plus a measurement margin, never past half the
    // tail.
    let win = (tail_len / 4)
        .max(max_settle + max_settle / 4)
        .min(tail_len / 2);
    if win < 2 {
        bail!("captured tail too short to evaluate decay ({tail_len} samples past the sweep end)");
    }

    let early: Vec<f32> = full[linear_centre..linear_centre + win]
        .iter()
        .map(|&v| v as f32)
        .collect();
    let late_start = linear_centre + tail_len - win;
    let late: Vec<f32> = full[late_start..late_start + win]
        .iter()
        .map(|&v| v as f32)
        .collect();

    let early_db = fb.process(&early);
    let late_db = fb.process(&late);
    let centres = fb.centres_hz();

    let mut worst: Option<(f64, f64)> = None; // (centre_hz, decay_db)
    let mut bands_settled = 0usize;
    for (((&e, &l), &c), &s) in early_db
        .iter()
        .zip(late_db.iter())
        .zip(centres.iter())
        .zip(settle.iter())
    {
        if win <= s {
            continue; // tail_s itself too short for this band to settle — not evaluated
        }
        bands_settled += 1;
        if !e.is_finite() {
            continue; // settled fine, genuinely no energy at capture start — nothing to decay
        }
        let decay = if l.is_finite() { e - l } else { f64::INFINITY };
        if worst.map(|(_, d)| decay < d).unwrap_or(true) {
            worst = Some((c, decay));
        }
    }
    let (worst_band_hz, worst_decay_db) = worst
        .ok_or_else(|| anyhow::anyhow!("no 1/3-octave band carried measurable energy to check"))?;

    Ok(TailDecayCheck {
        bpo: BPO as u32,
        worst_band_hz,
        worst_decay_db,
        required_db: REQUIRED_DB,
        passed: worst_decay_db >= REQUIRED_DB,
        bands_settled,
        bands_total: centres.len(),
    })
}

/// Result of [`estimate_onset`]: the onset sample index plus the rule
/// that produced it, so a persisted arrival can be told apart from a
/// bare peak read a year later (#346, acceptance criterion 4).
#[derive(Debug, Clone, PartialEq)]
pub struct OnsetEstimate {
    pub index: usize,
    pub rule: String,
}

/// Length of the onset picker's search window, in seconds.
///
/// The window ends at the magnitude peak; its start is the later of this
/// span back from the peak and the capture's own causal bound. It exists
/// only so the picker has a bounded window when no geometry is known —
/// where a causal bound is available that bound is always the tighter
/// limit and this constant never binds.
///
/// Derivation: the onset-to-peak distances measured on the rig are 110.4
/// samples at 1.000 m and 92.2 at 3.000 m, n = 12 each at 96 kHz
/// (`work/rig/rig-2026-08-23-onset-353-results.md`) — 1.15 ms and
/// 0.96 ms. 10 ms is 8.7x the larger of the two, so the window brackets
/// the onset with headroom and the picker's leading (pre-onset) segment
/// is never starved by its trailing one: at the measured geometry the
/// pre-onset part of the window is roughly 8x the post-onset part.
/// Expressed as a duration rather than a sample count because the
/// quantity it has to exceed is a flight-time difference; the sample
/// count follows from the capture's own rate.
pub const ONSET_SEARCH_WINDOW_S: f64 = 10.0e-3;

/// Sample variance of `xs[from..to]`, from prefix sums. Zero for
/// segments shorter than two samples.
fn segment_variance(prefix_sum: &[f64], prefix_sq: &[f64], from: usize, to: usize) -> f64 {
    if to <= from + 1 {
        return 0.0;
    }
    let n = (to - from) as f64;
    let s = prefix_sum[to] - prefix_sum[from];
    let q = prefix_sq[to] - prefix_sq[from];
    (q / n - (s / n) * (s / n)).max(0.0)
}

/// Estimate the wavefront onset in a deconvolved impulse response `ir`,
/// given its magnitude peak at `peak_index`, the capture's
/// `sample_rate_hz`, a `gate_floor` (the pre-impulse median floor, see
/// [`crate::measurement::report::MeasurementReport::ir_stats`]) and,
/// where geometry is known, `min_admissible_index`.
///
/// Not `argmax|h|` (issue #346): on a multi-way loudspeaker the sample of
/// largest magnitude sits at a fixed group-delay offset past the
/// wavefront that actually left the baffle first — LF and crossover
/// phase pull the peak later, by an amount that does not shrink with
/// distance (it cancels in an increment between two positions but
/// persists in the absolute, per the issue's rig table).
///
/// Not a level crossing either (issue #378). A threshold referenced to
/// the pre-impulse floor cannot see energy arriving *after* the
/// pre-impulse window, and the rig showed the resulting onset moving
/// 18.2 samples toward the peak between 1.000 m and 3.000 m while the
/// pre-impulse SNR moved 0.83 dB — 2.8 samples' worth at the measured
/// within-position slope. Expressing the same threshold relative to the
/// peak buys back only those same 2.8 samples with the sign flipped, so
/// the residual is not a threshold-level error at all: at a constant
/// level re peak the IR reaches that level 110.4 samples before the peak
/// at 1 m and 92.2 at 3 m. It is the edge's shape, and a level crossing
/// on a rising edge is a shape-dependent estimator by construction.
///
/// Rule: an AIC change-point pick (Maeda 1985, the standard
/// single-parameter onset picker; literature, not a standard — it is
/// deliberately absent from [`StandardsCitation`]) over the search
/// window. For each candidate split `k` the window is cut into a leading
/// and a trailing segment and
///
/// ```text
/// AIC(k) = n_lead * ln var(lead) + n_trail * ln var(trail)
/// ```
///
/// is minimised; the onset is the first sample of the trailing segment.
/// Scaling the whole IR by `c` adds `(n_lead + n_trail) * ln c²`, which
/// is constant in `k`, so the pick is *exactly* invariant to a uniform
/// rescale — the direct level dropping with distance cannot move it.
/// There is no threshold and no margin constant in the rule.
///
/// `min_admissible_index`, when supplied, is the earliest sample the
/// measurement's own known geometry allows an onset to occupy (pure
/// flight time converted to a sample index). It is the search window's
/// lower limit, so a bandlimited pre-ring that a floor-relative scan
/// would return non-causally is outside the picker's reach entirely
/// rather than being clamped after the fact. When it is `None` the bound
/// cannot be enforced (no geometry known for this capture) and `rule`
/// says so, so a reader can tell a geometry-checked onset from a
/// best-effort one.
///
/// Breakdown (#378 acceptance criterion 3, extending #353's): unlike the
/// backward walk the picker cannot fail to move, so the degenerate cases
/// are named explicitly and gated. On any of them the returned index is
/// `peak_index` — today's answer, never earlier and never non-causal —
/// and `rule` names which case fired. `gate_floor` is #377's
/// contamination-robust median floor, retained but demoted from the
/// threshold's input to this gate: it decides only whether the window
/// holds anything at all, never where the onset is.
///
/// Two limits, stated so they are not discovered as surprises:
///
/// - *Without geometry the pre-ring is unguarded.* The picker keys on
///   where the IR's variance changes, not on amplitude, so on a
///   band-limited deconvolution it reads the leading skirt of the main
///   lobe — earlier than the old level crossing did, and earlier than
///   sound can have arrived. Only `min_admissible_index` can reject
///   that, and it exists only when both a measured τ and a recorded
///   distance are present. Where geometry is known this is the tighter
///   limit and the case does not arise.
/// - *A window with no pre-onset noise in it is uninformative.* If the
///   causal bound truncates the window past the true onset, the window
///   is close to homogeneous and no split is much better than any
///   other. The null model's AIC penalty catches the exactly-tied case
///   and reports the window start (flagged in `rule`), but a near-tie
///   still resolves to some index. Nothing here can recover an onset
///   that geometry says is inadmissible.
pub fn estimate_onset(
    ir: &[f64],
    peak_index: usize,
    sample_rate_hz: u32,
    gate_floor: f64,
    min_admissible_index: Option<usize>,
) -> OnsetEstimate {
    let declined = |reason: &str| OnsetEstimate {
        index: peak_index.min(ir.len().saturating_sub(1)),
        rule: format!("onset picker declined ({reason}) — index is the peak, not an onset"),
    };
    if ir.is_empty() {
        return OnsetEstimate {
            index: peak_index,
            rule: "onset picker declined (search window shorter than 2 samples) — index is \
                   the peak, not an onset"
                .to_string(),
        };
    }
    let end = peak_index.min(ir.len() - 1);
    if end == 0 {
        return declined("peak at sample 0");
    }

    let span = if sample_rate_hz == 0 {
        end
    } else {
        (ONSET_SEARCH_WINDOW_S * sample_rate_hz as f64)
            .round()
            .max(0.0) as usize
    };
    let span_start = end.saturating_sub(span);
    let bound = min_admissible_index.map(|b| b.min(end));
    let window_start = bound.unwrap_or(0).max(span_start);

    let window = &ir[window_start..=end];
    if window.len() < 2 {
        return declined("search window shorter than 2 samples");
    }
    // The validity gate (#377's median floor, demoted): with nothing in
    // the window above the pre-impulse floor there is no wavefront to
    // find, only floor. Compared bare — no margin, no dB — because the
    // floor no longer sets an operating point, it only answers whether
    // the window holds anything at all.
    if !window.iter().any(|v| v.abs() > gate_floor) {
        return declined("nothing in the search window above the pre-impulse floor");
    }

    let n = window.len();
    let mut prefix_sum = vec![0.0_f64; n + 1];
    let mut prefix_sq = vec![0.0_f64; n + 1];
    for (i, &v) in window.iter().enumerate() {
        prefix_sum[i + 1] = prefix_sum[i] + v;
        prefix_sq[i + 1] = prefix_sq[i] + v * v;
    }
    let total_var = segment_variance(&prefix_sum, &prefix_sq, 0, n);
    if total_var <= 0.0 || !total_var.is_finite() {
        return declined("zero variance in the search window");
    }
    // A segment that is exactly constant would otherwise give ln(0).
    // The guard is a fraction of the window's own variance, not an
    // absolute epsilon, so it scales with the IR exactly as the two
    // segment variances do — the pick stays invariant to a uniform
    // rescale even on the samples where it binds.
    let var_floor = total_var * 1e-12;
    // A segment of fewer than two samples has no variance to estimate,
    // so it carries no likelihood information and contributes nothing —
    // the same convention that lets `k = 0` (an empty leading segment)
    // stand for "no change point inside this window". Without it a lone
    // sample's exact-zero variance would be clamped and then rewarded,
    // and every window ending in an isolated spike would pick its own
    // last sample.
    let term = |from: usize, to: usize| {
        if to <= from + 1 {
            return 0.0;
        }
        let n = (to - from) as f64;
        n * segment_variance(&prefix_sum, &prefix_sq, from, to)
            .max(var_floor)
            .ln()
    };

    // `k` is the count of samples in the leading segment. `k = 0` is the
    // null model — one variance over the whole window, i.e. no change
    // point inside it — and it is scored against the best split rather
    // than competing with it, because the two models do not have the
    // same number of parameters.
    let null_aic = term(0, n);
    let mut best_k = 0usize;
    let mut best_aic = f64::INFINITY;
    for k in 1..n {
        let aic = term(0, k) + term(k, n);
        if aic < best_aic {
            best_aic = aic;
            best_k = k;
        }
    }
    // Akaike's penalty, not a tuned threshold: the split model carries
    // two parameters the null does not — a second variance and the
    // change point's own location — and AIC charges 2 per parameter. A
    // homogeneous window is a near-tie between the null and whatever
    // split the noise happens to favour, so without this the picker
    // returns an arbitrary index there with full confidence. Below the
    // penalty the window supports no change point at all and the answer
    // is its own start, flagged as such in `rule`.
    const SPLIT_MODEL_EXTRA_PARAMETERS: f64 = 2.0;
    const AIC_PENALTY: f64 = 2.0 * SPLIT_MODEL_EXTRA_PARAMETERS;
    let onset = if best_aic + AIC_PENALTY < null_aic {
        window_start + best_k
    } else {
        window_start
    };
    if onset >= end {
        return declined("no change point earlier than the peak in the window");
    }

    let window_ms = ONSET_SEARCH_WINDOW_S * 1000.0;
    let limit = match bound {
        // Which limit actually set the window start is the operator's
        // next question when a pick sits on it, so the clause names the
        // binding one rather than only whether geometry was known.
        Some(b) if b >= span_start => "causal bound enforced".to_string(),
        Some(b) => format!("causal bound enforced at sample {b}, search span is the tighter limit"),
        None => "no causal bound (geometry not known for this capture)".to_string(),
    };
    let mut rule = format!(
        "AIC change-point pick over a {window_ms:.1} ms window; window start at sample \
         {window_start}, {limit}"
    );
    if onset == window_start {
        rule.push_str("; pick landed on the window start — the true onset may lie earlier");
    }
    OnsetEstimate { index: onset, rule }
}

/// Citation for a `MeasurementReport` emitted from a Farina-sweep run.
///
/// Two standards apply, for two different things. The theoretical basis —
/// the log sweep, the closed-form inverse filter, the harmonic-order
/// offsets — is Farina's; it is not covered by an IEC or AES standard, so
/// the canonical reference is the AES 108th Convention preprint #5093 by
/// Angelo Farina, "Simultaneous measurement of impulse response and
/// distortion with a swept-sine technique" (Paris, 2000). Verified against
/// the full preprint PDF under `stddocs/iec-full/`.
///
/// The swept-sine method itself, separately, is now covered by a normative
/// standard: ISO 18233:2006 Annex B (normative), "Swept-sine method". This
/// issue adds that reference; it does not replace the preprint, which
/// remains the correct citation for the theoretical basis.
///
/// `verified` covers the whole citation. It stays `false` until a human
/// has cross-checked the Annex B text against `stddocs/iso-full/` — an
/// agent may prepare this citation but must not flip that flag.
pub fn citation() -> StandardsCitation {
    StandardsCitation {
        standard: "Farina, AES 108th Convention preprint #5093 (2000); ISO 18233:2006 Annex B (normative)".into(),
        clause: "§2 Theoretical basis (log sweep, inverse filter, harmonic offsets); Annex B (normative) Swept-sine method".into(),
        verified: false,
    }
}

/// `deconvolve_full` recovers the IR via [`fft_linear_convolve`] — a
/// *linear* deconvolution. ISO 18233 §B.5 documents the consequence: the
/// tail past the peak is a decaying noise floor, increasingly low-pass
/// filtered toward its end, and the standard requires callers state this
/// "so as not to confuse the decreasing noise floor with the reverberant
/// tail of the room". Any reader of the linear IR (printed summary or
/// persisted [`crate::measurement::report::MeasurementReport::notes`])
/// needs this stated, since nothing else in the report tells them.
pub const LINEAR_DECONV_TAIL_NOTE: &str =
    "The decaying tail after the peak is a linear-deconvolution artefact \
     (fft_linear_convolve), increasingly low-pass filtered toward its end \
     — not the measured system's reverberant decay. See ISO 18233 §B.5.";

/// The instant, measured from the linear-IR peak, past which the captured
/// `full` deconvolution can only carry noise smeared by the inverse
/// filter's own kernel — never real system response. The Farina inverse
/// filter (`inverse_sweep`) is `duration_s` long, so any true system
/// response has been fully convolved out by `duration_s` past the peak;
/// content past that point is background noise passed through a kernel
/// whose own frequency content narrows toward the end of the sweep. This
/// is derived from the sweep parameters directly, not estimated —
/// see [`MeasurementReport::ir_stats`] and issue #284, acceptance
/// criterion 5.
///
/// [`MeasurementReport::ir_stats`]: crate::measurement::report::MeasurementReport::ir_stats
pub fn noise_tail_start_s(p: &SweepParams) -> f64 {
    p.duration_s
}

/// Tapered-cosine (Tukey) window of `len` samples. `alpha` is the
/// fraction of the window given over to the cosine taper at each edge:
/// `alpha = 0.0` is rectangular (no taper), `alpha = 1.0` is a full Hann
/// window, and values between taper only the edges while the interior
/// stays flat at unity gain. Used to gate the linear IR into a
/// magnitude/phase curve (#284) without the spectral leakage a hard
/// rectangular edge would cause, and without a full Hann's bias on
/// legitimate early-response samples near the gate edges.
pub fn tukey_window(len: usize, alpha: f64) -> Vec<f64> {
    if len == 0 {
        return Vec::new();
    }
    if len == 1 {
        return vec![1.0];
    }
    let alpha = alpha.clamp(0.0, 1.0);
    if alpha == 0.0 {
        return vec![1.0; len];
    }
    let n = (len - 1) as f64;
    let taper_len = alpha * n / 2.0;
    (0..len)
        .map(|i| {
            let x = i as f64;
            if taper_len <= 0.0 {
                1.0
            } else if x < taper_len {
                0.5 * (1.0 + (PI * (x / taper_len - 1.0)).cos())
            } else if x > n - taper_len {
                0.5 * (1.0 + (PI * ((x - n) / taper_len + 1.0)).cos())
            } else {
                1.0
            }
        })
        .collect()
}

/// One point of a gated frequency response: magnitude and (wrapped)
/// phase of the FFT of a time-gated impulse response at `freq_hz`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GatedResponsePoint {
    pub freq_hz: f64,
    /// `20·log10(|H(f)|)`. `H` is the raw FFT of the gated, windowed
    /// impulse response — no additional normalisation — so an identity
    /// system (unit-magnitude IR spike) reads 0 dB at every bin.
    pub magnitude_db: f64,
    /// `atan2` phase in degrees, wrapped to `(-180, 180]` — not
    /// unwrapped. Unwrapping is a separate algorithmic choice with its
    /// own correctness bar; wrapped `atan2` output is what was actually
    /// measured (#284).
    pub phase_deg: f64,
}

/// Gate `linear_ir` to `gate_length_s` seconds starting `gate_start_s`
/// seconds after the IR's peak-reference sample (`linear_ir.len() / 2` —
/// the same zero-delay reference [`crate::measurement::report::MeasurementReport::ir_stats`]
/// uses, stable across window-length changes per #278), taper the gated
/// region with a [`tukey_window`] of shape `alpha`, and FFT the result to
/// a magnitude/phase curve.
///
/// Samples the gate requests outside `linear_ir`'s bounds are zero —
/// matching [`extract_irs`]'s own `gate` helper. Returns an empty vec
/// when `linear_ir` is empty or the requested gate is shorter than 2
/// samples (nothing to FFT).
pub fn gated_frequency_response(
    linear_ir: &[f64],
    sample_rate: u32,
    gate_start_s: f64,
    gate_length_s: f64,
    alpha: f64,
) -> Vec<GatedResponsePoint> {
    if linear_ir.is_empty() || sample_rate == 0 {
        return Vec::new();
    }
    let fs = sample_rate as f64;
    let gate_len = (gate_length_s * fs).round() as i64;
    if gate_len < 2 {
        return Vec::new();
    }
    let gate_len = gate_len as usize;

    let centre = linear_ir.len() as i64 / 2;
    let start = centre + (gate_start_s * fs).round() as i64;
    let window = tukey_window(gate_len, alpha);
    let mut buf = vec![0.0_f64; gate_len];
    for (i, w) in window.iter().enumerate() {
        let idx = start + i as i64;
        if idx >= 0 && (idx as usize) < linear_ir.len() {
            buf[i] = linear_ir[idx as usize] * w;
        }
    }

    let mut planner = RealFftPlanner::<f64>::new();
    let fft = planner.plan_fft_forward(gate_len);
    let mut spec = fft.make_output_vec();
    if fft.process(&mut buf, &mut spec).is_err() {
        return Vec::new();
    }

    spec.iter()
        .enumerate()
        .map(|(k, c)| {
            let freq_hz = k as f64 * fs / gate_len as f64;
            let mag = c.norm().max(1e-300);
            GatedResponsePoint {
                freq_hz,
                magnitude_db: 20.0 * mag.log10(),
                phase_deg: c.arg().to_degrees(),
            }
        })
        .collect()
}

/// Farina-preprint-only citation for the theoretical basis (log sweep,
/// closed-form inverse filter, harmonic-order offsets) — same text as the
/// preprint half of [`citation`], deliberately *without* the ISO
/// 18233:2006 Annex B half.
///
/// Scoped to [`crate::measurement::report::MeasurementData::GatedFrequencyResponse`]
/// (#284): ISO 18233 §1 restricts its own scope to substituting for
/// classical-method standards (ISO 140, ISO 3382, ISO 17497-1) and §9(c)
/// requires the test report to additionally name that classical
/// counterpart whenever ISO 18233 is cited — a quasi-anechoic
/// loudspeaker/system capture has no classical counterpart, so it cannot
/// carry that citation. [`citation`] itself is unchanged (still packs both
/// standards into one string) since the pre-existing `ImpulseResponse`
/// payload's use of it is out of scope here — see PR #305 review.
pub fn farina_citation() -> StandardsCitation {
    StandardsCitation {
        standard: "Farina, AES 108th Convention preprint #5093 (2000)".into(),
        clause: "§2 Theoretical basis (log sweep, inverse filter, harmonic offsets)".into(),
        verified: false,
    }
}

/// Citation for the [`crate::measurement::report::MeasurementData::GatedFrequencyResponse`]
/// payload (#284): a quasi-anechoic frequency response derived by
/// time-gating a Farina-swept-sine impulse response, distinct from the
/// impulse-response payload's own citation ([`citation`]) — both apply,
/// in relevance order. Paired with [`farina_citation`], not [`citation`]
/// — see [`farina_citation`]'s doc for why the ISO 18233 half must not
/// come along for this payload.
///
/// `verified` stays `false` until a human cross-checks Annex A.4.5 against
/// the published AES17-2020 text at
/// `stddocs/iec-full/aes17_2020_aes_standard_method_for_digital_audio_engineering_measurement.pdf`.
pub fn gated_response_citation() -> StandardsCitation {
    StandardsCitation {
        standard: "AES17-2020".into(),
        clause: "Annex A.4.5 (informative) (quasi-anechoic frequency response via time-gated impulse response)"
            .into(),
        verified: false,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const SR: u32 = 48_000;

    fn p_default() -> SweepParams {
        SweepParams {
            f1_hz: 20.0,
            f2_hz: 20_000.0,
            duration_s: 1.0,
            sample_rate: SR,
        }
    }

    #[test]
    fn params_validate() {
        assert!(p_default().validate().is_ok());
        let mut p = p_default();
        p.f1_hz = 0.0;
        assert!(p.validate().is_err());
        let mut p = p_default();
        p.f2_hz = p.f1_hz;
        assert!(p.validate().is_err());
        let mut p = p_default();
        p.duration_s = 0.0;
        assert!(p.validate().is_err());
        let mut p = p_default();
        p.f2_hz = 30_000.0; // above Nyquist/2
        assert!(p.validate().is_err());
    }

    #[test]
    fn harmonic_time_offsets_are_log_spaced() {
        let p = p_default();
        let dt2 = p.harmonic_time_offset_s(2);
        let dt3 = p.harmonic_time_offset_s(3);
        let dt4 = p.harmonic_time_offset_s(4);
        // ln(4) = 2·ln(2)
        assert!((dt4 - 2.0 * dt2).abs() < 1e-12);
        // ln(3) / ln(2) ≈ 1.585
        let ratio = dt3 / dt2;
        assert!((ratio - 3f64.ln() / 2f64.ln()).abs() < 1e-12);
    }

    #[test]
    fn sweep_starts_at_zero_phase() {
        let x = log_sweep(&p_default()).unwrap();
        assert!(x[0].abs() < 1e-6);
        assert!(x.iter().all(|v| v.is_finite()));
    }

    #[test]
    fn identity_system_produces_unit_linear_ir_peak() {
        let p = p_default();
        let x = log_sweep(&p).unwrap();
        let xi = inverse_sweep(&p).unwrap();
        let full = deconvolve_full(&x, &xi);
        let irs = extract_irs(&full, &p, 1, 128).unwrap();
        let peak = irs
            .linear
            .iter()
            .cloned()
            .fold(0.0_f64, |m, v| m.max(v.abs()));
        // Normalisation inside inverse_sweep should bring this to 1.
        assert!(
            (peak - 1.0).abs() < 0.05,
            "identity IR peak = {peak}, expected ~1"
        );
        // Peak should be at the window centre.
        let peak_idx = irs
            .linear
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.abs().partial_cmp(&b.1.abs()).unwrap())
            .unwrap()
            .0;
        assert_eq!(
            peak_idx, 64,
            "expected peak at window centre, got {peak_idx}"
        );
    }

    #[test]
    fn delayed_impulse_shifts_linear_ir() {
        // Model a pure delay: y(n) = x(n - d). Linear IR from Farina
        // should be a spike at (window_centre + d).
        let p = p_default();
        let x = log_sweep(&p).unwrap();
        let d = 17_usize;
        let mut y = vec![0.0_f32; x.len() + d];
        y[d..].copy_from_slice(&x);
        let xi = inverse_sweep(&p).unwrap();
        let full = deconvolve_full(&y, &xi);
        let irs = extract_irs(&full, &p, 1, 128).unwrap();
        let peak_idx = irs
            .linear
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.abs().partial_cmp(&b.1.abs()).unwrap())
            .unwrap()
            .0;
        assert_eq!(peak_idx, 64 + d);
    }

    #[test]
    fn scaled_delay_recovers_magnitude_and_sign() {
        // A pure inverting half-gain channel y(n) = -0.5 · x(n − d). The
        // Farina-deconvolved IR should be a negative spike at
        // (window_centre + d) with peak magnitude ≈ 0.5.
        let p = p_default();
        let x = log_sweep(&p).unwrap();
        let d = 9_usize;
        let mut y = vec![0.0_f32; x.len() + d];
        for (i, &v) in x.iter().enumerate() {
            y[i + d] = -0.5 * v;
        }
        let xi = inverse_sweep(&p).unwrap();
        let full = deconvolve_full(&y, &xi);
        let irs = extract_irs(&full, &p, 1, 128).unwrap();
        let (peak_idx, peak_val) = irs
            .linear
            .iter()
            .enumerate()
            .map(|(i, v)| (i, *v))
            .max_by(|a, b| a.1.abs().partial_cmp(&b.1.abs()).unwrap())
            .unwrap();
        assert_eq!(peak_idx, 64 + d, "peak at wrong offset");
        assert!(peak_val < 0.0, "peak should be negative: {peak_val}");
        assert!(
            (peak_val.abs() - 0.5).abs() < 0.05,
            "|peak| {} should be ~0.5",
            peak_val.abs()
        );
    }

    #[test]
    fn cubic_nonlinearity_produces_third_harmonic_ir() {
        // y = a·x + b·x³ has a 3rd-harmonic component at scale |b|/4
        // relative to the fundamental's scale (a + 3b/4). The extracted
        // 3rd-harmonic IR is carried by the Farina inverse filter with a
        // frequency-dependent gain, so we don't pin the absolute ratio —
        // we just verify the 3rd-harmonic IR has meaningful energy when
        // the input is clearly nonlinear, and is essentially zero when
        // the input is linear.
        let p = p_default();
        let x = log_sweep(&p).unwrap();
        let xi = inverse_sweep(&p).unwrap();
        let window = 128;

        // Linear baseline: no 3rd-harmonic energy.
        let full_lin = deconvolve_full(&x, &xi);
        let irs_lin = extract_irs(&full_lin, &p, 3, window).unwrap();
        let lin_only_h3_peak = irs_lin
            .harmonics
            .iter()
            .find(|h| h.order == 3)
            .unwrap()
            .samples
            .iter()
            .cloned()
            .fold(0.0_f64, |m, v| m.max(v.abs()));

        // Cubic nonlinearity: substantial 3rd-harmonic energy.
        let b = 0.3_f32;
        let y: Vec<f32> = x.iter().map(|&v| v + b * v * v * v).collect();
        let full_nl = deconvolve_full(&y, &xi);
        let irs_nl = extract_irs(&full_nl, &p, 3, window).unwrap();
        let nl_h3_peak = irs_nl
            .harmonics
            .iter()
            .find(|h| h.order == 3)
            .unwrap()
            .samples
            .iter()
            .cloned()
            .fold(0.0_f64, |m, v| m.max(v.abs()));

        assert!(
            nl_h3_peak > 3.0 * lin_only_h3_peak,
            "expected nonlinear 3rd-harmonic peak ({nl_h3_peak:.5}) to clearly exceed the linear baseline ({lin_only_h3_peak:.5})"
        );
        assert!(
            nl_h3_peak > 0.001,
            "nonlinear 3rd-harmonic peak too small: {nl_h3_peak}"
        );
    }

    /// Gate-centre offsets in samples for orders `1..=n`, derived from
    /// Farina's Δt_k = L·ln(k) directly rather than through
    /// `harmonic_offsets_samples`, so the window tests below measure the
    /// implementation instead of restating it.
    fn hand_derived_offsets(p: &SweepParams, n: usize) -> Vec<i64> {
        let l = p.duration_s / (p.f2_hz / p.f1_hz).ln();
        let fs = p.sample_rate as f64;
        (1..=n as u32)
            .map(|k| (l * (k as f64).ln() * fs).round() as i64)
            .collect()
    }

    fn hand_derived_gaps(p: &SweepParams, n: usize) -> Vec<i64> {
        hand_derived_offsets(p, n)
            .windows(2)
            .map(|w| w[1] - w[0])
            .collect()
    }

    fn deconvolved_default() -> (SweepParams, Vec<f64>) {
        let p = p_default();
        let x = log_sweep(&p).unwrap();
        let xi = inverse_sweep(&p).unwrap();
        let full = deconvolve_full(&x, &xi);
        (p, full)
    }

    #[test]
    fn hand_derived_gaps_match_issue_278_table() {
        // Anchors every window test below to numbers computed off the
        // spec, not off this module: 1 s, 20 Hz–20 kHz, 48 kHz, orders 1..5
        // give L = 144.76 ms and gaps of 4816 / 2818 / 1999 / 1551 samples.
        // The 1999 and 1551 figures are issue #278's own H3→H4 and H4→H5
        // rows.
        assert_eq!(
            hand_derived_gaps(&p_default(), 5),
            vec![4816, 2818, 1999, 1551]
        );
    }

    #[test]
    fn each_order_is_clamped_to_its_own_neighbour_spacing() {
        let (p, full) = deconvolved_default();
        let gaps = hand_derived_gaps(&p, 5);
        // Order i's gate is bounded by the gap below it and the gap above
        // it — not by the narrowest gap anywhere in the set.
        let expect = vec![
            4096usize,        // order 1: only neighbour is order 2 (4816)
            gaps[1] as usize, // order 2: min(4816, 2818)
            gaps[2] as usize, // order 3: min(2818, 1999)
            gaps[3] as usize, // order 4: min(1999, 1551)
            gaps[3] as usize, // order 5: only neighbour is order 4 (1551)
        ];

        let irs = extract_irs(&full, &p, 5, 4096).unwrap();
        assert_eq!(irs.window_len_requested, 4096);
        assert_eq!(irs.window_len_used, expect);
        assert_eq!(irs.linear.len(), expect[0]);
        for h in &irs.harmonics {
            assert_eq!(
                h.samples.len(),
                expect[h.order as usize - 1],
                "order {} gate length",
                h.order
            );
        }
        for (i, w) in expect.iter().enumerate() {
            assert_eq!(irs.window_len_for(i as u32 + 1), Some(*w));
        }
        assert_eq!(irs.window_len_for(6), None);
    }

    #[test]
    fn linear_gate_is_not_cut_to_the_global_minimum_spacing() {
        // The rejected alternative: one global clamp to the narrowest gap
        // in the set. Compute it here so the test fails if the code ever
        // reverts to it — under that rule the linear IR, whose nearest
        // neighbour is ~3x further away than the narrowest gap, would lose
        // most of its length for no contamination reason.
        let (p, full) = deconvolved_default();
        let global_min = *hand_derived_gaps(&p, 5).iter().min().unwrap() as usize;
        let irs = extract_irs(&full, &p, 5, 4096).unwrap();
        assert!(
            irs.linear.len() > global_min,
            "linear gate {} was cut to the global minimum spacing {}",
            irs.linear.len(),
            global_min
        );
        assert_eq!(irs.linear.len(), 4096, "linear gate should be unclamped");
    }

    #[test]
    fn adjacent_order_gates_never_overlap() {
        let (p, full) = deconvolved_default();
        let n = 5usize;
        let irs = extract_irs(&full, &p, n, 4096).unwrap();
        let centres: Vec<i64> = hand_derived_offsets(&p, n)
            .iter()
            .map(|o| (p.n_samples() as i64 - 1) - o)
            .collect();
        // `gate` puts the centre at index window/2, so order i covers
        // [centre - w/2, centre - w/2 + w). Orders run backwards in time,
        // so order i+1's window must end at or before order i's starts.
        for i in 0..n - 1 {
            let (w_hi, w_lo) = (irs.window_len_used[i], irs.window_len_used[i + 1]);
            let start_hi = centres[i] - (w_hi / 2) as i64;
            let end_lo = centres[i + 1] - (w_lo / 2) as i64 + w_lo as i64;
            assert!(
                end_lo <= start_hi,
                "order {} gate ends at {} but order {} starts at {}",
                i + 2,
                end_lo,
                i + 1,
                start_hi
            );
        }
    }

    #[test]
    fn window_is_untouched_when_it_fits_every_gap() {
        let (p, full) = deconvolved_default();
        // 128 is under the narrowest gap (1551) for orders 1..5.
        let irs = extract_irs(&full, &p, 5, 128).unwrap();
        assert_eq!(irs.window_len_used, vec![128; 5]);
        assert_eq!(irs.linear.len(), 128);
        assert_eq!(irs.clamp_note(), None);
    }

    #[test]
    fn single_order_is_never_clamped() {
        // Guards the τ measurement in ac-daemon's `calibrate`, which asks
        // for n_harmonics=1 and a long window because half of it is the
        // largest round-trip delay it can report. With no second order
        // there is no adjacent-order constraint to clamp against.
        let (p, full) = deconvolved_default();
        let irs = extract_irs(&full, &p, 1, 8192).unwrap();
        assert_eq!(irs.window_len_used, vec![8192]);
        assert_eq!(irs.linear.len(), 8192);
        assert!(irs.harmonics.is_empty());
        assert_eq!(irs.clamp_note(), None);
    }

    #[test]
    fn collapsed_spacing_is_an_error_not_a_zero_length_gate() {
        // At 0.3 ms the orders round onto each other: offsets are
        // [0, 1, 2, 3, 3] samples, so orders 4 and 5 share a centre and no
        // non-overlapping gate exists for them at any length.
        let p = SweepParams {
            duration_s: 0.0003,
            ..p_default()
        };
        assert!(p.validate().is_ok(), "params must be otherwise valid");
        assert_eq!(hand_derived_offsets(&p, 5), vec![0, 1, 2, 3, 3]);
        let full = vec![0.0_f64; 4096];
        let err = extract_irs(&full, &p, 5, 128).unwrap_err().to_string();
        assert!(
            err.contains("orders 4 and 5"),
            "error should name the collapsed pair: {err}"
        );
        // Orders 1..4 are still separated, so this must stay an error about
        // the requested set rather than silently dropping order 5.
        assert!(extract_irs(&full, &p, 4, 128).is_ok());
    }

    #[test]
    fn clamp_note_names_every_shortened_order() {
        let (p, full) = deconvolved_default();
        let irs = extract_irs(&full, &p, 5, 4096).unwrap();
        let note = irs.clamp_note().expect("orders 2..5 are clamped at 4096");
        assert!(
            note.contains("4096"),
            "note should state the request: {note}"
        );
        for order in 2..=5 {
            assert!(
                note.contains(&format!("order {order} \u{2192}")),
                "note should name order {order}: {note}"
            );
        }
        assert!(
            !note.contains("order 1 \u{2192}"),
            "order 1 was not clamped and must not be listed: {note}"
        );
    }

    #[test]
    fn clamp_note_carries_the_gap_reason_and_its_parameters() {
        // #342: "clamped to N samples" alone gives the operator no knob to
        // turn. The note must also say *why* — the adjacent-harmonic-order
        // gap — and the sweep parameters that set it, so the operator knows
        // duration_s/f1_hz/f2_hz is what to change.
        let (p, full) = deconvolved_default();
        let irs = extract_irs(&full, &p, 5, 4096).unwrap();
        let note = irs.clamp_note().expect("orders 2..5 are clamped at 4096");
        assert!(
            note.to_lowercase().contains("gap"),
            "note should name the adjacent-harmonic-order gap as the reason: {note}"
        );
        for needle in [
            format!("f1_hz={}", p.f1_hz),
            format!("f2_hz={}", p.f2_hz),
            format!("duration_s={}", p.duration_s),
        ] {
            assert!(
                note.contains(&needle),
                "note should carry `{needle}` so the operator knows which \
                 knob widens the gap: {note}"
            );
        }
    }

    #[test]
    fn citation_shape() {
        let c = citation();
        // Preprint: theoretical basis. Must not be dropped by this change.
        assert!(c.standard.contains("Farina"));
        assert!(c.clause.contains("§2"));
        // ISO 18233:2006 Annex B: normative swept-sine method, added
        // alongside the preprint (#291).
        assert!(c.standard.contains("ISO 18233:2006"));
        assert!(c.standard.contains("Annex B"));
        assert!(c.clause.contains("Annex B"));
        // Human gate: Annex B text not yet cross-checked, so the combined
        // citation is not `verified` yet. An agent must not flip this.
        assert!(!c.verified);
    }

    // ─── check_tail_decay (#282 acceptance criterion 6) ────────────

    /// Build a synthetic `full` deconvolution buffer whose early tail
    /// window carries a real broadband signal and whose late tail window
    /// is exact digital silence — the case ISO 18233 §6.3.2 describes as
    /// adequate capture, without depending on how deep a real Farina
    /// deconvolution's own residual skirt happens to be at a given window
    /// length (that skirt is real but its depth is not what this check
    /// means to pin down).
    fn full_with_silent_tail(p: &SweepParams, tail_s: f64) -> Vec<f64> {
        let linear_centre = p.n_samples() - 1;
        let tail_len = (tail_s * p.sample_rate as f64).round() as usize;
        let win = tail_len / 4;
        let mut full = vec![0.0_f64; linear_centre + tail_len + 1];
        let x = log_sweep(p).unwrap();
        for i in 0..win {
            full[linear_centre + i] = x[i] as f64;
        }
        full
    }

    #[test]
    fn tail_decay_check_passes_when_the_tail_is_true_silence() {
        let p = p_default();
        let full = full_with_silent_tail(&p, 0.3);
        let check = check_tail_decay(&full, &p, 0.3).unwrap();
        assert!(check.passed, "expected pass, got {check:?}");
        assert!(check.worst_decay_db >= check.required_db);
    }

    #[test]
    fn tail_decay_check_fails_when_the_tail_never_decays() {
        // Test against the rejected case directly: poison the end of the
        // tail with the same raw samples the check reads as "right at the
        // IR peak", so every band it can evaluate reports 0 dB of decay —
        // a room whose reverberation is nowhere close to 30 dB down by the
        // end of the captured tail.
        let p = p_default();
        let x = log_sweep(&p).unwrap();
        let xi = inverse_sweep(&p).unwrap();
        let mut full = deconvolve_full(&x, &xi);
        let linear_centre = p.n_samples() - 1;
        let tail_s = 0.3;
        let tail_len = (tail_s * p.sample_rate as f64).round() as usize;
        let win = tail_len / 4;
        let src = full[linear_centre..linear_centre + win].to_vec();
        let late_start = linear_centre + tail_len - win;
        full[late_start..late_start + win].copy_from_slice(&src);

        let check = check_tail_decay(&full, &p, tail_s).unwrap();
        assert!(!check.passed, "expected failure, got {check:?}");
        assert!(check.worst_decay_db < check.required_db);
    }

    #[test]
    fn tail_decay_check_rejects_nonpositive_tail_s() {
        let p = p_default();
        let full = vec![0.0; p.n_samples() * 2];
        assert!(check_tail_decay(&full, &p, 0.0).is_err());
        assert!(check_tail_decay(&full, &p, -1.0).is_err());
    }

    #[test]
    fn tail_decay_check_rejects_a_capture_with_no_tail() {
        let p = p_default();
        let full = vec![0.0; p.n_samples()]; // nothing captured past the sweep end
        assert!(check_tail_decay(&full, &p, 0.5).is_err());
    }

    #[test]
    fn tail_decay_check_note_names_the_band_and_margin() {
        let p = p_default();
        let full = full_with_silent_tail(&p, 0.3);
        let check = check_tail_decay(&full, &p, 0.3).unwrap();
        let note = check.note();
        assert!(note.contains("18233"));
        assert!(note.contains("6.3.2"));
        assert!(note.contains("adequate"));
    }

    /// Regression for correctness issue 1 (PR #296 QA review), adopting the
    /// review's suggested test near-verbatim: at the daemon's own shipped
    /// default `tail_s = 0.5` (the earlier `tail_decay_check_fails_when_...`
    /// test only exercised `tail_s = 0.3`), poison the tail end with the
    /// same broadband early-window content the check reads as "right at the
    /// IR peak". Before the settle-aware window fix, the flat `tail_len / 4`
    /// window (125 ms) was shorter than the lowest in-range band's settling
    /// prefix (~137 ms), so that band read `NEG_INFINITY` from
    /// `Filterbank::process` and was folded into "no energy, exclude" —
    /// this test's ~0 dB decay was still visible via other, higher bands in
    /// the old code, so it does not by itself prove the exclusion is fixed;
    /// `tail_decay_check_reports_full_band_coverage_when_tail_is_adequate`
    /// below is what actually pins the settled-band count.
    #[test]
    fn tail_decay_check_fails_at_shipped_default_tail_s() {
        let p = p_default();
        let x = log_sweep(&p).unwrap();
        let xi = inverse_sweep(&p).unwrap();
        let mut full = deconvolve_full(&x, &xi);
        let linear_centre = p.n_samples() - 1;
        let tail_s = 0.5; // daemon's shipped default (handlers/audio/plot.rs)
        let fs = p.sample_rate as f64;
        let tail_len = (tail_s * fs).round() as usize;
        let win = tail_len / 4;
        let src = full[linear_centre..linear_centre + win].to_vec();
        let late_start = linear_centre + tail_len - win;
        full[late_start..late_start + win].copy_from_slice(&src);

        let check = check_tail_decay(&full, &p, tail_s).unwrap();
        assert!(!check.passed, "expected failure, got {check:?}");
        assert!(check.worst_decay_db < check.required_db);
    }

    #[test]
    fn tail_decay_check_reports_full_band_coverage_when_tail_is_adequate() {
        let p = p_default();
        let full = full_with_silent_tail(&p, 0.5);
        let check = check_tail_decay(&full, &p, 0.5).unwrap();
        assert_eq!(
            check.bands_settled, check.bands_total,
            "expected every in-range band to clear its settling prefix at tail_s=0.5: {check:?}"
        );
    }

    // ─── noise_tail_start_s / tukey_window / gated_frequency_response (#284) ───

    #[test]
    fn noise_tail_start_s_is_the_sweep_duration() {
        let p = p_default();
        assert_eq!(noise_tail_start_s(&p), p.duration_s);
        let mut p2 = p_default();
        p2.duration_s = 2.5;
        assert_eq!(noise_tail_start_s(&p2), 2.5);
    }

    #[test]
    fn tukey_window_alpha_zero_is_rectangular() {
        let w = tukey_window(16, 0.0);
        assert_eq!(w, vec![1.0; 16]);
    }

    #[test]
    fn tukey_window_alpha_one_matches_hand_derived_hann() {
        let n = 32usize;
        let w = tukey_window(n, 1.0);
        for (i, &v) in w.iter().enumerate() {
            // Hand-derived Hann formula, computed independently of the
            // implementation's cosine-taper construction.
            let expect = 0.5 * (1.0 - (2.0 * PI * i as f64 / (n - 1) as f64).cos());
            assert!(
                (v - expect).abs() < 1e-9,
                "index {i}: got {v}, hand-derived Hann expects {expect}"
            );
        }
    }

    #[test]
    fn tukey_window_edges_taper_and_interior_stays_flat() {
        let w = tukey_window(100, 0.25);
        assert!(w[0] < 0.01, "window should start near zero: {}", w[0]);
        assert!(
            (w[99] - w[0]).abs() < 1e-9,
            "window should be symmetric: {} vs {}",
            w[0],
            w[99]
        );
        // Interior (well clear of the 12.5%-per-side taper) stays at unity.
        assert!(
            (w[50] - 1.0).abs() < 1e-9,
            "interior should be flat: {}",
            w[50]
        );
    }

    #[test]
    fn gated_frequency_response_of_a_flat_system_is_flat() {
        // A pure unit impulse at the gate's centre reference — FFT of a
        // delta is flat magnitude at every bin, by hand: |FFT(delta)| = 1.
        let sr = 48_000u32;
        let len = 256usize;
        let mut ir = vec![0.0_f64; len];
        ir[len / 2] = 1.0;
        let points = gated_frequency_response(&ir, sr, 0.0, (len / 2) as f64 / sr as f64, 0.0);
        assert!(
            points.len() > 4,
            "expected several bins, got {}",
            points.len()
        );
        for p in &points {
            assert!(
                p.magnitude_db.abs() < 0.1,
                "flat system should read ~0 dB at {} Hz, got {} dB",
                p.freq_hz,
                p.magnitude_db
            );
        }
    }

    #[test]
    fn gated_frequency_response_recovers_a_known_single_pole() {
        // h[n] = r^n for n = 0..gate_len, a causal one-pole IR, placed
        // starting exactly at the gate's centre reference so a rectangular
        // (alpha = 0.0) gate captures it whole with no truncation-vs-window
        // ambiguity. Hand-derived expectation: the finite geometric-sum DTFT
        // H(w) = sum_{n=0}^{N-1} r^n e^{-jwn} = (1 - (r e^{-jw})^N) / (1 - r e^{-jw}),
        // computed independently of `gated_frequency_response`'s FFT path.
        let sr = 48_000u32;
        let gate_len = 512usize;
        let r = 0.85_f64;
        let mut ir = vec![0.0_f64; gate_len * 2];
        let centre = ir.len() / 2;
        for n in 0..gate_len {
            ir[centre + n] = r.powi(n as i32);
        }
        let gate_length_s = gate_len as f64 / sr as f64;
        let points = gated_frequency_response(&ir, sr, 0.0, gate_length_s, 0.0);
        assert_eq!(points.len(), gate_len / 2 + 1);

        for p in points.iter().step_by(7) {
            let w = 2.0 * PI * p.freq_hz / sr as f64;
            let re_pole = r * w.cos();
            let im_pole = -r * w.sin();
            // (r e^{-jw})^N via repeated squaring on the complex pole.
            let (mut pow_re, mut pow_im) = (1.0_f64, 0.0_f64);
            for _ in 0..gate_len {
                let nr = pow_re * re_pole - pow_im * im_pole;
                let ni = pow_re * im_pole + pow_im * re_pole;
                pow_re = nr;
                pow_im = ni;
            }
            let num_re = 1.0 - pow_re;
            let num_im = -pow_im;
            let den_re = 1.0 - re_pole;
            let den_im = -im_pole;
            let den_mag_sq = den_re * den_re + den_im * den_im;
            let h_re = (num_re * den_re + num_im * den_im) / den_mag_sq;
            let h_im = (num_im * den_re - num_re * den_im) / den_mag_sq;
            let expect_mag_db = 20.0 * (h_re * h_re + h_im * h_im).sqrt().max(1e-300).log10();
            assert!(
                (p.magnitude_db - expect_mag_db).abs() < 0.5,
                "at {} Hz: got {} dB, hand-derived single-pole expects {} dB",
                p.freq_hz,
                p.magnitude_db,
                expect_mag_db
            );
        }
    }

    #[test]
    fn gate_length_implies_f_low_by_hand_arithmetic() {
        // f_low_hz = 1 / gate_length_s, the same formula `GateParams`
        // stores (#280) — pinned here independent of any report/daemon
        // plumbing. 960 samples @ 48 kHz = 20 ms -> f_low = 50 Hz.
        let sr = 48_000u32;
        let gate_len_samples = 960usize;
        let gate_length_s = gate_len_samples as f64 / sr as f64;
        let f_low_hz = 1.0 / gate_length_s;
        assert!((gate_length_s - 0.020).abs() < 1e-12);
        assert!((f_low_hz - 50.0).abs() < 1e-9);
    }

    #[test]
    fn gated_response_differs_from_ungated_when_a_reflection_is_excluded() {
        // Direct impulse at the gate's centre reference plus a later,
        // scaled "reflection" impulse. A short gate that ends before the
        // reflection must produce a smoother (lower-ripple) response than
        // the same IR analysed with a gate long enough to include the
        // reflection's comb-filter interference — mutation-verify: compute
        // the rejected case (comb-filtering left in) directly and confirm
        // the gated result is measurably different from it.
        let sr = 48_000u32;
        let len = 2048usize;
        let mut ir = vec![0.0_f64; len];
        let centre = len / 2;
        let reflection_offset = 200usize; // samples after the direct sound
        ir[centre] = 1.0;
        ir[centre + reflection_offset] = 0.5;

        let short_gate_s = (reflection_offset - 20) as f64 / sr as f64; // excludes the reflection
        let long_gate_s = (reflection_offset + 400) as f64 / sr as f64; // includes it
        let gated = gated_frequency_response(&ir, sr, 0.0, short_gate_s, 0.0);
        let ungated = gated_frequency_response(&ir, sr, 0.0, long_gate_s, 0.0);

        // Ripple (variance of magnitude_db across bins) is what a
        // reflection's comb filtering predicts: present when the
        // reflection is inside the gate, reduced when it is excluded.
        let variance = |pts: &[GatedResponsePoint]| -> f64 {
            let mean = pts.iter().map(|p| p.magnitude_db).sum::<f64>() / pts.len() as f64;
            pts.iter()
                .map(|p| (p.magnitude_db - mean).powi(2))
                .sum::<f64>()
                / pts.len() as f64
        };
        let gated_var = variance(&gated);
        let ungated_var = variance(&ungated);
        assert!(
            gated_var < ungated_var,
            "gate excluding the reflection should read smoother than the \
             gate including it: gated variance {gated_var}, ungated (with \
             reflection) variance {ungated_var}"
        );
    }

    #[test]
    fn gated_frequency_response_citation_shape() {
        let c = gated_response_citation();
        assert!(c.standard.contains("AES17"));
        assert!(c.clause.contains("A.4"));
        assert!(!c.verified);
    }

    /// PR #305 review, correctness issue 1: the gated-response payload
    /// must not end up citing ISO 18233 by way of reusing `citation()`
    /// wholesale — `farina_citation()` carries the preprint only.
    #[test]
    fn farina_citation_excludes_iso_18233() {
        let c = farina_citation();
        assert!(c.standard.contains("Farina"));
        assert!(
            !c.standard.contains("ISO 18233"),
            "gated payload's citation must not carry ISO 18233: {c:?}"
        );
        assert!(!c.verified);
    }

    // ─── estimate_onset (#346, #378) ───────────────────────────────────

    /// Deterministic pseudo-noise, uniform in ±√3·`sigma` so its
    /// variance is exactly `sigma²`. A fixed LCG rather than a real RNG
    /// so every assertion below is reproducible byte for byte.
    fn onset_noise(n: usize, sigma: f64, seed: u64) -> Vec<f64> {
        let mut s = seed | 1;
        (0..n)
            .map(|_| {
                s = s
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1_442_695_040_888_963_407);
                let u = ((s >> 11) as f64) / ((1u64 << 53) as f64);
                (u - 0.5) * 2.0 * 3.0f64.sqrt() * sigma
            })
            .collect()
    }

    /// The rule #378 rejects, computed inline so the tests below measure
    /// it rather than asserting "the new answer is closer to truth":
    /// a threshold `ONSET_FLOOR_MARGIN_DB` (12 dB) above the pre-impulse
    /// floor, walked backward from the peak. This is the whole of the
    /// pre-#378 estimator; nothing in `sweep.rs` implements it any more.
    fn rejected_level_crossing_rule(ir: &[f64], peak_index: usize, floor: f64) -> usize {
        let threshold = floor * 10f64.powf(12.0 / 20.0);
        let mut onset = peak_index;
        while onset > 0 && ir[onset - 1].abs() > threshold {
            onset -= 1;
        }
        onset
    }

    /// A capture whose direct-to-reverberant ratio is a parameter and
    /// whose true onset, noise floor and geometry are not: a fixed noise
    /// floor, a direct wavefront of amplitude `direct` rising from a
    /// fixed `onset_true` to a fixed `peak_index`, and an exponentially
    /// decaying reverberant tail of fixed level after the peak.
    ///
    /// DRR is varied through the direct's level rather than the tail's,
    /// deliberately. The tail arrives *after* the peak, so it cannot move
    /// a backward walk that starts at the peak — a fixture that varied
    /// only the tail would leave the rejected rule motionless and would
    /// therefore be unable to fail. Varying the direct is also the rig's
    /// own mechanism between 1.000 m and 3.000 m: the direct drops with
    /// 1/r while room noise at the capsule and the reverberant level do
    /// not.
    fn drr_fixture(direct: f64, sigma_n: f64, tail: f64) -> (Vec<f64>, usize, usize) {
        const N: usize = 4096;
        const ONSET_TRUE: usize = 1800;
        const PEAK_INDEX: usize = 1910;
        let mut ir = onset_noise(N, sigma_n, 0x9E37_79B9);
        let rise = (PEAK_INDEX - ONSET_TRUE) as f64;
        for (i, v) in ir.iter_mut().enumerate() {
            if (ONSET_TRUE..=PEAK_INDEX).contains(&i) {
                let x = (i - ONSET_TRUE) as f64 / rise;
                *v += direct * x * x;
            } else if i > PEAK_INDEX {
                let t = (i - PEAK_INDEX) as f64;
                let sign = if i % 2 == 0 { 1.0 } else { -1.0 };
                *v += sign * tail * (-t / 400.0).exp();
            }
        }
        (ir, ONSET_TRUE, PEAK_INDEX)
    }

    /// Test against the rejected implementation (#346 acceptance
    /// criterion 2): a synthetic IR with realistic multi-way group delay
    /// — sustained energy from the true onset through to a magnitude
    /// peak that sits well after it, standing in for LF/crossover group
    /// delay pulling the largest sample later than the wavefront. The
    /// estimator must not fall back to `argmax|h|`.
    #[test]
    fn onset_estimate_rejects_the_peak_the_way_346_names_as_wrong() {
        let floor = 0.001;
        let onset_true = 200usize;
        let peak_index = 260usize;
        let mut ir = onset_noise(512, floor, 0x1234_5678);
        for v in ir.iter_mut().take(peak_index + 1).skip(onset_true) {
            *v += 0.3;
        }
        ir[peak_index] = 1.0; // the actual magnitude maximum
        let est = estimate_onset(&ir, peak_index, 48_000, floor, None);
        assert_eq!(est.index, onset_true);
        assert_ne!(
            est.index, peak_index,
            "must not fall back to argmax|h| — #346"
        );
    }

    /// #346 acceptance criterion 3, restated for #378's window: a
    /// bandlimited pre-ring sits below the sample pure flight time
    /// allows, with the real wavefront above it. An unbounded search
    /// lands on the pre-ring (computed here directly, per "test against
    /// the rejected implementation"); supplying the causal bound as the
    /// search window's lower limit puts the non-causal candidate outside
    /// the picker's reach entirely rather than clamping it after the
    /// fact, which is what changed under #378.
    #[test]
    fn onset_estimate_rejects_bandlimited_preringing_below_the_causal_bound() {
        let sigma_n = 1e-4;
        let peak_index = 300usize;
        let bound = 280usize; // earliest sample pure flight time allows
        let wavefront = 285usize;
        let mut ir = onset_noise(512, sigma_n, 0xABCD_EF01);
        for (i, v) in ir.iter_mut().enumerate() {
            if (245..wavefront).contains(&i) {
                *v += 0.02 * ((i - 245) as f64 * 0.7).sin();
            } else if (wavefront..=peak_index).contains(&i) {
                *v += 0.3 * (i - wavefront + 1) as f64 / 16.0;
            }
        }
        ir[peak_index] = 1.0;

        let unbounded = estimate_onset(&ir, peak_index, 48_000, sigma_n, None);
        assert!(
            unbounded.index < bound,
            "test setup: unbounded pick must land non-causally, got {}",
            unbounded.index
        );

        let bounded = estimate_onset(&ir, peak_index, 48_000, sigma_n, Some(bound));
        assert!(
            bounded.index >= bound,
            "causal bound must exclude the non-causal pre-ring, got {}",
            bounded.index
        );
        assert_ne!(bounded.index, unbounded.index);
        assert!(bounded.rule.contains("causal bound enforced"));
    }

    /// #346 acceptance criterion 4: the rule string must say whether a
    /// causal bound was actually enforced, so a persisted number can be
    /// told apart from a best-effort one.
    #[test]
    fn onset_rule_states_whether_a_causal_bound_was_enforced() {
        let sigma_n = 1e-4;
        let (ir, _, peak_index) = drr_fixture(1.0, sigma_n, 0.05);
        let with_bound = estimate_onset(&ir, peak_index, 96_000, sigma_n, Some(1500));
        let without_bound = estimate_onset(&ir, peak_index, 96_000, sigma_n, None);
        assert_ne!(with_bound.rule, without_bound.rule);
        assert!(with_bound.rule.contains("causal bound enforced"));
        assert!(without_bound.rule.contains("no causal bound"));
        assert!(with_bound.rule.contains("window start at sample 1500"));
    }

    /// #378: the rule names the search window and where it started, and
    /// says which limit set it — the causal bound or the search span.
    #[test]
    fn onset_rule_names_the_window_and_which_limit_started_it() {
        let sigma_n = 1e-4;
        let (ir, _, peak_index) = drr_fixture(1.0, sigma_n, 0.05);
        let span_start = peak_index - (ONSET_SEARCH_WINDOW_S * 96_000.0).round() as usize;

        let spanned = estimate_onset(&ir, peak_index, 96_000, sigma_n, None);
        assert!(spanned
            .rule
            .contains(&format!("window start at sample {span_start}")));
        assert!(spanned
            .rule
            .contains("AIC change-point pick over a 10.0 ms window"));

        // A causal bound looser than the search span does not move the
        // window start, and the rule must not imply that it did.
        let loose = estimate_onset(&ir, peak_index, 96_000, sigma_n, Some(10));
        assert!(loose
            .rule
            .contains(&format!("window start at sample {span_start}")));
        assert!(
            loose.rule.contains("search span is the tighter limit"),
            "a non-binding causal bound must not read as the window's start: {}",
            loose.rule
        );
    }

    /// #378 acceptance criterion 2, the load-bearing one. The rejected
    /// level-crossing rule is computed inline on the same captures. As
    /// DRR falls, *its* answer must move toward the peak — the direction
    /// the rig measured (110.4 → 92.2 samples onset-to-peak between
    /// 1.000 m and 3.000 m) — while the picker's must move less, on
    /// every capture and across the sweep as a whole.
    ///
    /// The direction is asserted, not just a difference: if the fixture
    /// ever stops reproducing the defect, this test fails rather than
    /// passing for the wrong reason.
    #[test]
    fn onset_pick_tracks_drr_less_than_the_rejected_level_rule_does() {
        let sigma_n = 1e-4;
        let tail = 0.05;
        let directs = [1.0, 0.3, 0.1, 0.03, 0.01];

        let mut rejected = Vec::new();
        let mut picked = Vec::new();
        for &d in &directs {
            let (ir, onset_true, peak_index) = drr_fixture(d, sigma_n, tail);
            let r = rejected_level_crossing_rule(&ir, peak_index, sigma_n);
            let p = estimate_onset(&ir, peak_index, 96_000, sigma_n, None).index;
            assert!(
                r >= onset_true && p >= onset_true,
                "test setup: neither rule may read before the true onset \
                 (direct {d}): rejected {r}, picked {p}, true {onset_true}"
            );
            rejected.push(r - onset_true);
            picked.push(p - onset_true);
        }

        // Direction: the rejected rule's answer moves monotonically
        // toward the peak as the direct level drops.
        for w in rejected.windows(2) {
            assert!(
                w[1] > w[0],
                "rejected rule must move toward the peak as DRR falls: {rejected:?}"
            );
        }
        let rejected_spread = rejected.last().unwrap() - rejected.first().unwrap();
        let picked_spread = picked.iter().max().unwrap() - picked.iter().min().unwrap();
        assert!(
            picked_spread < rejected_spread,
            "picker must track DRR less than the rejected rule: picker spread \
             {picked_spread} ({picked:?}), rejected spread {rejected_spread} \
             ({rejected:?})"
        );
        for (i, (&r, &p)) in rejected.iter().zip(picked.iter()).enumerate() {
            assert!(
                p <= r,
                "capture {i} (direct {}): picker residual {p} must not exceed \
                 the rejected rule's {r}",
                directs[i]
            );
        }
        // At the highest DRR the two rules agree — that is the case #378
        // is not about. The claim is about the low-DRR end, where the
        // rejected rule has walked away from the onset and the picker has
        // not.
        assert!(
            picked.last().unwrap() < rejected.last().unwrap(),
            "at the lowest DRR the picker must be strictly closer to the \
             true onset: picker {picked:?}, rejected {rejected:?}"
        );
    }

    /// #378: the property the pick is chosen for. Scaling the whole IR by
    /// any constant adds a term constant in `k` to every AIC value, so the
    /// pick is *exactly* invariant — not approximately. The rejected rule
    /// is computed inline on the same scaled captures with the floor held
    /// at the unscaled value (which is what a pre-impulse-referenced
    /// threshold does when the direct level drops and the room noise does
    /// not) and must move.
    #[test]
    fn onset_pick_is_exactly_invariant_to_a_uniform_rescale() {
        let sigma_n = 1e-4;
        let (ir, _, peak_index) = drr_fixture(1.0, sigma_n, 0.05);
        let base = estimate_onset(&ir, peak_index, 96_000, sigma_n, None).index;
        let base_rejected = rejected_level_crossing_rule(&ir, peak_index, sigma_n);

        let mut moved = false;
        for scale in [0.001, 0.5, 2.0, 1000.0] {
            let scaled: Vec<f64> = ir.iter().map(|v| v * scale).collect();
            let est = estimate_onset(&scaled, peak_index, 96_000, sigma_n * scale, None);
            assert_eq!(
                est.index, base,
                "pick must be exactly invariant to a uniform rescale by {scale}"
            );
            if rejected_level_crossing_rule(&scaled, peak_index, sigma_n) != base_rejected {
                moved = true;
            }
        }
        assert!(
            moved,
            "test setup: the rejected rule must move under a rescale its floor \
             does not follow, or this test proves nothing"
        );
    }

    /// #378 acceptance criterion 4. The rule introduces exactly one new
    /// constant — [`ONSET_SEARCH_WINDOW_S`] — and no gate level: the
    /// validity gate compares against the caller's pre-impulse floor
    /// bare, with no margin. This pins that the two are independent:
    /// the pick does not change with the window span (over the range that
    /// brackets the onset, exercised through the sample rate the span is
    /// derived from), and it does not change with the gate floor (over
    /// the range that passes the gate).
    #[test]
    fn onset_window_span_and_validity_gate_are_independent() {
        let sigma_n = 1e-4;
        let (ir, onset_true, peak_index) = drr_fixture(0.3, sigma_n, 0.05);
        let reference = estimate_onset(&ir, peak_index, 96_000, sigma_n, None).index;

        // Span varies 4.8x (480 → 2304 samples); every value brackets the
        // onset, which sits 110 samples before the peak.
        for sr in [48_000u32, 96_000, 192_000, 230_400] {
            let span = (ONSET_SEARCH_WINDOW_S * sr as f64).round() as usize;
            assert!(
                span > peak_index - onset_true,
                "test setup: span {span} must bracket the onset"
            );
            let est = estimate_onset(&ir, peak_index, sr, sigma_n, None);
            assert_eq!(
                est.index, reference,
                "pick must not depend on the window span (sample rate {sr}, span {span})"
            );
        }

        // Gate floor varies 1000x; every value leaves the gate open,
        // because the picker takes no threshold from it.
        for floor in [sigma_n * 0.01, sigma_n, sigma_n * 10.0] {
            let est = estimate_onset(&ir, peak_index, 96_000, floor, None);
            assert_eq!(
                est.index, reference,
                "pick must not depend on the validity gate's floor ({floor})"
            );
        }
    }

    /// #378 acceptance criterion 3: the failure direction is bounded.
    /// Every named degenerate case returns `peak_index` — today's answer,
    /// never earlier and never non-causal — and says which case fired.
    /// The list is the whole of the picker's decline surface; a case that
    /// is not here cannot make the picker decline.
    #[test]
    fn onset_picker_declines_to_the_peak_on_every_degenerate_case() {
        struct Case {
            reason: &'static str,
            ir: Vec<f64>,
            peak_index: usize,
            floor: f64,
            bound: Option<usize>,
        }
        let cases = vec![
            Case {
                reason: "peak at sample 0",
                ir: vec![1.0, 0.1, 0.1],
                peak_index: 0,
                floor: 1e-6,
                bound: None,
            },
            Case {
                reason: "search window shorter than 2 samples",
                ir: vec![0.01, 1.0, 0.01],
                peak_index: 1,
                floor: 1e-6,
                bound: Some(1),
            },
            Case {
                reason: "zero variance in the search window",
                ir: vec![0.5; 64],
                peak_index: 40,
                floor: 0.1,
                bound: None,
            },
            Case {
                reason: "nothing in the search window above the pre-impulse floor",
                ir: onset_noise(200, 1e-3, 7),
                peak_index: 150,
                floor: 1.0,
                bound: None,
            },
            Case {
                reason: "no change point earlier than the peak in the window",
                ir: {
                    let mut v = onset_noise(200, 1e-3, 11);
                    v[150] = 1.0;
                    v
                },
                peak_index: 150,
                floor: 1e-4,
                bound: None,
            },
        ];
        for Case {
            reason,
            ir,
            peak_index,
            floor,
            bound,
        } in cases
        {
            let est = estimate_onset(&ir, peak_index, 48_000, floor, bound);
            assert_eq!(
                est.index, peak_index,
                "{reason}: breakdown must degrade to the peak, not earlier"
            );
            assert!(
                est.rule.contains("onset picker declined")
                    && est.rule.contains(reason)
                    && est.rule.contains("index is the peak, not an onset"),
                "{reason}: rule string does not name the case: {}",
                est.rule
            );
        }
    }

    /// #378: the pick can only ever land inside the admissible window, so
    /// a bounded capture can never return a non-causal answer — the
    /// property the pre-#378 clamp had to enforce after the fact.
    #[test]
    fn onset_pick_never_lands_below_the_causal_bound() {
        let sigma_n = 1e-4;
        let (ir, _, peak_index) = drr_fixture(1.0, sigma_n, 0.05);
        for bound in [1500usize, 1750, 1850, 1900] {
            let est = estimate_onset(&ir, peak_index, 96_000, sigma_n, Some(bound));
            assert!(
                est.index >= bound,
                "pick {} fell below the causal bound {bound}",
                est.index
            );
        }
    }

    #[test]
    fn onset_estimate_stays_at_peak_when_nothing_precedes_it_above_floor() {
        // No change point behind the peak: the earliest admissible onset
        // is the peak itself, and the picker must say so rather than
        // returning a confident index it did not find.
        let floor = 0.01;
        let mut ir = vec![floor; 200];
        ir[150] = 1.0;
        let est = estimate_onset(&ir, 150, 48_000, floor, None);
        assert_eq!(est.index, 150);
        assert!(est.rule.contains("onset picker declined"));
    }

    /// QA (PR #377), carried forward to #378's picker: the breakdown
    /// admission is the load-bearing addition per the UX design comment
    /// ("has to reach the line the operator reads, or the failure stays
    /// silent where it costs something"). Pins the exact substring
    /// `short_onset_rule` (ac-cli) matches on, so a typo in either place
    /// breaks a test instead of silently dropping the warning at the
    /// terminal.
    #[test]
    fn onset_rule_names_the_breakdown_when_the_picker_declines() {
        let floor = 1.0; // nothing in the window clears it
        let mut ir = onset_noise(200, 1e-3, 3);
        ir[150] = 0.5;
        let est = estimate_onset(&ir, 150, 48_000, floor, None);
        assert_eq!(est.index, 150);
        assert!(
            est.rule.contains("onset picker declined")
                && est.rule.contains("— index is the peak, not an onset"),
            "rule string missing the breakdown admission: {}",
            est.rule
        );
    }

    /// QA (PR #377), restated for #378: a causal bound sitting at the
    /// peak also leaves the answer at the peak, and the decline text must
    /// name the window that collapsed rather than implying the capture
    /// carried nothing above the floor.
    #[test]
    fn onset_rule_names_the_collapsed_window_not_a_missing_signal() {
        let sigma_n = 1e-4;
        let (ir, _, peak_index) = drr_fixture(1.0, sigma_n, 0.05);
        let est = estimate_onset(&ir, peak_index, 96_000, sigma_n, Some(peak_index));
        assert_eq!(est.index, peak_index);
        assert!(
            est.rule.contains("search window shorter than 2 samples"),
            "collapsed-window decline mislabeled: {}",
            est.rule
        );
    }

    /// #378 / UX: a pick sitting on the window start is a stable,
    /// repeatable, possibly wrong number, and the rule must say so — the
    /// operator's question is whether the answer came from the signal or
    /// from the bracket. Reached here by a causal bound that leaves the
    /// picker two admissible samples: no split beats the null model over
    /// a window that short, so the answer is the window start and the
    /// true onset may lie earlier.
    #[test]
    fn onset_rule_flags_a_pick_pinned_to_the_window_start() {
        let sigma_n = 1e-4;
        let (ir, _, peak_index) = drr_fixture(1.0, sigma_n, 0.05);
        let bound = peak_index - 1;
        let est = estimate_onset(&ir, peak_index, 96_000, sigma_n, Some(bound));
        assert_eq!(est.index, bound);
        assert!(
            est.rule
                .contains("pick landed on the window start — the true onset may lie earlier"),
            "pinned pick not flagged: {}",
            est.rule
        );
    }
}
