//! Multi-time-window (MTW) ladder for the live transfer display.
//!
//! One measurement pair in, log-frequency columns out, with every column
//! backed by real bins and carrying the resolution, window and averaging that
//! produced it.
//!
//! # What it fixes
//!
//! The full-rate estimator this sits beside analyses everything with one
//! window (`nperseg = sr`, Δf = 1 Hz), which has three consequences:
//!
//! 1. **Density exceeding resolution.** A 1/48-octave grid is real only above
//!    `Δf · κ(48)` = 69.25 Hz at 1 Hz resolution; below that the aggregator's
//!    interpolation branch fills columns in from their neighbours. 86 columns
//!    of a 20 Hz–24 kHz display are synthesised. Here the grid widens instead
//!    ([`ladder::column_edges`]) and nothing is interpolated.
//! 2. **One window for the whole band.** A 1 s window makes a 15 kHz rattle as
//!    slow to appear as a 20 Hz reading. Here HF is analysed at full rate with
//!    a 4096-point window — 43 ms at 96 kHz — while LF gets the long window it
//!    genuinely needs.
//! 3. **Transient ripple (#208).** A sliding, re-segmented Welch re-analyses
//!    one impulse once per segment position. Here the pipeline **pushes**:
//!    each input sample enters each stage's analysis exactly once, and the
//!    averaging is an EMA over accumulated cross-spectra rather than a
//!    re-scan of retained audio.
//!
//! # Shape
//!
//! ```text
//! (meas, ref) --> PairAligner --> PairDecimator (one per stage)
//!                  one signed        one coefficient set,
//!                  offset, at        both channels, ONE phase
//!                  full rate         counter
//!                                          |
//!                                          v
//!                                    Hann/50% segments -> Sxx,Syy,Sxy
//!                                          |
//!                                          v
//!                                       BandEma (uniform N_eff)
//!                                          |
//!                                          v
//!                                       splice::assemble -> columns
//! ```
//!
//! # What stays off the ladder
//!
//! `Gxy/Gxx` cancels `|Hdec|²`; `Sxx` alone does not — it is multiplied by it.
//! So the cancellation argument covers `H1` and coherence and **nothing else**.
//! Absolute levels — `spl`, and the calibrated per-channel `meas_spectrum` /
//! `ref_spectrum` — stay on the full-rate path where they are today. This is a
//! fence, not an omission: routing a calibrated absolute level through the
//! ladder would require deconvolving the decimator near each band edge, which
//! is the same fabrication this module exists to remove, in a new place.

pub mod align;
pub mod decimate;
pub mod ema;
pub mod ladder;
pub mod splice;

use realfft::RealFftPlanner;

use align::PairAligner;
use decimate::PairDecimator;
use ema::{BandEma, HANN_50_RHO};
use ladder::{Ladder, Stage, HOP, NFFT};
use splice::{Column, StageSpectra};

/// Periodic Hann window of length `n`.
fn hann(n: usize) -> Vec<f64> {
    (0..n)
        .map(|i| 0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / n as f64).cos())
        .collect()
}

/// One rung's running state: decimator, segment buffer, accumulator.
struct Band {
    decim: PairDecimator,
    /// Decimated samples not yet consumed by a segment.
    buf_meas: Vec<f64>,
    buf_ref: Vec<f64>,
    ema: BandEma,
    /// Decimated samples still to be discarded while the filter settles.
    /// Analysing the transient would put a real, filter-shaped artifact into
    /// the first frames of every session.
    warmup: usize,
}

/// The ladder, running.
pub struct MtwPair {
    ladder: Ladder,
    aligner: PairAligner,
    bands: Vec<Band>,
    window: Vec<f64>,
    planner: RealFftPlanner<f64>,
    /// Scratch, reused across ticks so the hot loop does not allocate.
    aligned_meas: Vec<f32>,
    aligned_ref: Vec<f32>,
    dec_meas: Vec<f64>,
    dec_ref: Vec<f64>,
}

impl MtwPair {
    /// `offset` is the pair's alignment offset in full-rate samples, signed —
    /// see [`align`].
    pub fn new(sr: u32, offset: i64, n_target: f64) -> Result<Self, ladder::LadderError> {
        let ladder = ladder::layout(sr)?;
        let srf = f64::from(sr);
        let bins = NFFT / 2 + 1;
        let bands = ladder
            .stages
            .iter()
            .map(|s| {
                let decim = PairDecimator::for_stage(srf, s);
                let warmup = decim.transient_samples() / s.decim;
                Band {
                    decim,
                    buf_meas: Vec::with_capacity(NFFT * 2),
                    buf_ref: Vec::with_capacity(NFFT * 2),
                    ema: BandEma::new(bins, n_target, HANN_50_RHO),
                    warmup,
                }
            })
            .collect();
        Ok(Self {
            ladder,
            aligner: PairAligner::new(offset),
            bands,
            window: hann(NFFT),
            planner: RealFftPlanner::<f64>::new(),
            aligned_meas: Vec::new(),
            aligned_ref: Vec::new(),
            dec_meas: Vec::new(),
            dec_ref: Vec::new(),
        })
    }

    pub fn ladder(&self) -> &Ladder {
        &self.ladder
    }

    /// Segments completed per stage so far — warmup progress, shallowest
    /// first.
    pub fn frames(&self) -> Vec<u64> {
        self.bands.iter().map(|b| b.ema.frames()).collect()
    }

    /// Push one tick of captured audio.
    ///
    /// The two blocks are the raw capture for this pair; they need not be the
    /// same length, since the aligner is what absorbs the offset. Everything
    /// downstream of the aligner sees equal-length blocks by construction.
    pub fn push(&mut self, meas: &[f32], reference: &[f32]) {
        self.aligned_meas.clear();
        self.aligned_ref.clear();
        self.aligner.push(
            meas,
            reference,
            &mut self.aligned_meas,
            &mut self.aligned_ref,
        );
        if self.aligned_meas.is_empty() {
            return;
        }
        for band in self.bands.iter_mut() {
            self.dec_meas.clear();
            self.dec_ref.clear();
            band.decim.push(
                &self.aligned_meas,
                &self.aligned_ref,
                &mut self.dec_meas,
                &mut self.dec_ref,
            );

            let skip = band.warmup.min(self.dec_meas.len());
            band.warmup -= skip;
            band.buf_meas.extend_from_slice(&self.dec_meas[skip..]);
            band.buf_ref.extend_from_slice(&self.dec_ref[skip..]);

            let mut pos = 0usize;
            while pos + NFFT <= band.buf_meas.len() {
                accumulate_segment(
                    &mut self.planner,
                    &self.window,
                    &band.buf_meas[pos..pos + NFFT],
                    &band.buf_ref[pos..pos + NFFT],
                    &mut band.ema,
                );
                pos += HOP;
            }
            if pos > 0 {
                band.buf_meas.drain(..pos);
                band.buf_ref.drain(..pos);
            }
        }
    }

    /// Assemble display columns. `None` until every stage has produced at
    /// least one segment — a partially-warm ladder would show a crossover
    /// between a live band and an empty one.
    pub fn columns(&self, f_min: f64, f_max: f64, ppo: f64) -> Option<Vec<Column>> {
        if self.bands.iter().any(|b| b.ema.frames() == 0) {
            return None;
        }
        let edges = ladder::column_edges(&self.ladder, f_min, f_max, ppo);
        if edges.len() < 2 {
            return None;
        }
        let views: Vec<StageSpectra<'_>> = self
            .bands
            .iter()
            .map(|b| StageSpectra {
                sxx: b.ema.sxx(),
                syy: b.ema.syy(),
                sxy: b.ema.sxy(),
                n_eff: b.ema.n_eff(),
            })
            .collect();
        Some(splice::assemble(&self.ladder, &views, &edges))
    }
}

/// One Hann-windowed segment pair into the band's accumulator.
///
/// Note what is *not* here: no dB, no magnitude, no division. The segment
/// contributes raw `Sxx`, `Syy`, `Sxy`; everything derived comes later and
/// once.
fn accumulate_segment(
    planner: &mut RealFftPlanner<f64>,
    window: &[f64],
    meas: &[f64],
    reference: &[f64],
    ema: &mut BandEma,
) {
    let fft = planner.plan_fft_forward(NFFT);
    let mut bm: Vec<f64> = meas.iter().zip(window).map(|(&s, &w)| s * w).collect();
    let mut br: Vec<f64> = reference.iter().zip(window).map(|(&s, &w)| s * w).collect();
    let mut fm = fft.make_output_vec();
    let mut fr = fft.make_output_vec();
    if fft.process(&mut bm, &mut fm).is_err() || fft.process(&mut br, &mut fr).is_err() {
        return;
    }
    let n = fm.len();
    let mut sxx = Vec::with_capacity(n);
    let mut syy = Vec::with_capacity(n);
    let mut sxy = Vec::with_capacity(n);
    for k in 0..n {
        // x is the reference (the estimator's input), y the measurement:
        // H1 = Sxy/Sxx must be meas-over-ref.
        let x = fr[k];
        let y = fm[k];
        sxx.push(x.norm_sqr());
        syy.push(y.norm_sqr());
        sxy.push(x.conj() * y);
    }
    ema.update(&sxx, &syy, &sxy);
}

/// The wall-clock time constant a band's cadence implies for the
/// configured `N_target`. Reported so a viewer can tell how long a band takes
/// to settle without reverse-engineering it from the frame rate.
pub fn tau_seconds(stage: &Stage, alpha: f64) -> f64 {
    if alpha >= 1.0 {
        return stage.hop_s;
    }
    stage.hop_s / -(1.0 - alpha).ln()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic broadband source, seekable at any index so a delayed copy
    /// is exact.
    fn source_at(index: i64) -> f32 {
        if index < 0 {
            return 0.0;
        }
        let mut z =
            0xC0FF_EEC0_FFEEu64.wrapping_add((index as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15));
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        (((z >> 40) as f64 / (1u64 << 24) as f64) * 2.0 - 1.0) as f32
    }

    /// Drive a pair through `secs` of a delayed, scaled copy of one source.
    fn run(sr: u32, gain: f32, dut_delay: i64, offset: i64, secs: f64) -> MtwPair {
        let mut p = MtwPair::new(sr, offset, 4.0).unwrap();
        let n = (f64::from(sr) * secs) as i64;
        let block = 2_400usize;
        let mut i = 0i64;
        while i < n {
            let len = block.min((n - i) as usize);
            let meas: Vec<f32> = (0..len)
                .map(|k| gain * source_at(i + k as i64 - dut_delay))
                .collect();
            let refc: Vec<f32> = (0..len).map(|k| source_at(i + k as i64)).collect();
            p.push(&meas, &refc);
            i += len as i64;
        }
        p
    }

    /// Ground truth end to end: a known flat gain must come back as a flat
    /// `|H1|` with coherence ~1, in every band.
    #[test]
    fn recovers_a_known_flat_gain_across_the_whole_ladder() {
        let p = run(48_000, 0.5, 0, 0, 12.0);
        let cols = p.columns(20.0, 24_000.0, 48.0).expect("warm");
        assert!(!cols.is_empty());
        let mut checked = [0usize; 3];
        for c in &cols {
            // Skip the extreme LF, where 12 s is only a few stage-2 segments.
            if c.freq < 80.0 || c.freq > 20_000.0 {
                continue;
            }
            checked[c.stage] += 1;
            let db = 20.0 * c.h1.norm().max(1e-12).log10();
            assert!(
                (db + 6.0206).abs() < 1.0,
                "{} Hz (stage {}): {db} dB, coherence {}",
                c.freq,
                c.stage,
                c.coherence
            );
            assert!(
                c.coherence > 0.9,
                "{} Hz: coherence {}",
                c.freq,
                c.coherence
            );
        }
        assert!(
            checked.iter().all(|&n| n > 0),
            "every stage must have been exercised, got {checked:?}"
        );
    }

    /// Criterion 6: coherence is delay-invariant in **every** band, including
    /// stage 0, once the pair is aligned.
    ///
    /// Stage 0's window is 85 ms at 48 kHz, so an unaligned 50 ms DUT delay
    /// leaves it with `((W−D)/W)² = 0.17` at best and zero by 85 ms. A test
    /// that exercised only the deepest band could not tell alignment from a
    /// post-hoc phase rotation.
    #[test]
    fn coherence_is_delay_invariant_in_every_band_when_aligned() {
        for dut_delay in [0i64, 480, 2_400, 4_800] {
            let p = run(48_000, 0.5, dut_delay, dut_delay, 12.0);
            let cols = p.columns(20.0, 24_000.0, 48.0).expect("warm");
            for c in cols.iter().filter(|c| c.freq > 80.0 && c.freq < 20_000.0) {
                assert!(
                    c.coherence > 0.9,
                    "delay {dut_delay}: {} Hz (stage {}) coherence {}",
                    c.freq,
                    c.stage,
                    c.coherence
                );
            }
        }
    }

    /// The mutation half of criterion 6: with the offset disabled, stage 0
    /// must collapse. If it does not, the test above proves nothing.
    #[test]
    fn disabling_the_offset_collapses_stage_zero_coherence() {
        let dut_delay = 4_800i64; // 100 ms at 48 kHz, past stage 0's 85 ms window
        let p = run(48_000, 0.5, dut_delay, 0, 12.0);
        let cols = p.columns(20.0, 24_000.0, 48.0).expect("warm");
        let hf: Vec<&Column> = cols
            .iter()
            .filter(|c| c.stage == 0 && c.freq > 2_000.0 && c.freq < 20_000.0)
            .collect();
        assert!(!hf.is_empty());
        let worst = hf.iter().fold(0.0f64, |a, c| a.max(c.coherence));
        assert!(
            worst < 0.5,
            "stage 0 kept coherence {worst} without alignment — the alignment \
             test cannot distinguish alignment from rotation"
        );
        // ...while the deepest band, whose window dwarfs the delay, barely
        // notices. This is why a deep-band-only test is not enough.
        let lf = cols
            .iter()
            .filter(|c| c.stage == cols[0].stage.max(2) && c.freq > 60.0 && c.freq < 150.0)
            .fold(0.0f64, |a, c| a.max(c.coherence));
        assert!(
            lf > 0.8,
            "deep band should be unbothered by 100 ms, got {lf}"
        );
    }

    /// #208: an impulse must be reported once, not once per segment position.
    /// A sliding re-segmented Welch produces `n_averages` maxima as the
    /// impulse crawls through the window; a push pipeline produces one.
    #[test]
    fn an_impulse_is_analysed_once_not_once_per_segment_position() {
        let sr = 48_000u32;
        let mut p = MtwPair::new(sr, 0, 4.0).unwrap();
        let mut trace: Vec<f64> = Vec::new();
        let block = 2_400usize;
        for tick in 0..200 {
            let meas: Vec<f32> = (0..block)
                .map(|k| {
                    // One impulse, one tick, one sample.
                    if tick == 60 && k == 0 {
                        1.0
                    } else {
                        0.0
                    }
                })
                .collect();
            let refc = vec![0.0f32; block];
            p.push(&meas, &refc);
            // Stage 0's own accumulated energy is the observable: a re-scan
            // would re-present the impulse on later ticks.
            trace.push(p.bands[0].ema.syy().iter().sum::<f64>());
        }
        let peak = trace.iter().cloned().fold(0.0f64, f64::max);
        assert!(peak > 0.0, "impulse never registered");
        let peak_tick = trace.iter().position(|&v| v == peak).unwrap();
        // After the peak the trace must decay monotonically: every later rise
        // is the same impulse being counted again.
        for w in trace[peak_tick..].windows(2) {
            assert!(
                w[1] <= w[0] + 1e-15,
                "energy rose again after the impulse — it is being re-analysed"
            );
        }
    }

    #[test]
    fn tau_follows_each_bands_own_cadence() {
        let l = ladder::layout(96_000).unwrap();
        let alpha = ema::alpha_for_n_eff(4.0, HANN_50_RHO);
        let taus: Vec<f64> = l.stages.iter().map(|s| tau_seconds(s, alpha)).collect();
        // HF settles fast, LF slowly — the point of uniform N_eff.
        assert!(taus[0] < 0.06, "stage 0 tau {}", taus[0]);
        assert!(taus[2] > 1.0 && taus[2] < 2.5, "stage 2 tau {}", taus[2]);
        assert!(taus[0] < taus[1] && taus[1] < taus[2]);
    }
}
