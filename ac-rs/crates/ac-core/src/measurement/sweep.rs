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
             requested length.",
            self.window_len_requested,
            clamped.join(", ")
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

/// Citation for the [`crate::measurement::report::MeasurementData::GatedFrequencyResponse`]
/// payload (#284): a quasi-anechoic frequency response derived by
/// time-gating a Farina-swept-sine impulse response, distinct from the
/// impulse-response payload's own citation ([`citation`]) — both apply,
/// in relevance order (see `sweep::citation`'s doc for why the theoretical
/// basis and the swept-sine method are cited separately).
///
/// `verified` stays `false` until a human cross-checks Annex A.4 against
/// the published AES17-2015 text — this repo does not carry that PDF
/// under `stddocs/`.
pub fn gated_response_citation() -> StandardsCitation {
    StandardsCitation {
        standard: "AES17-2015".into(),
        clause: "Annex A.4 (quasi-anechoic frequency response via time-gated impulse response)"
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
}
