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
//! 3. **Transient ripple (#208).** Today's code cuts analysis blocks from the
//!    head of a buffer that slides by a variable amount each tick, so a
//!    transient's position inside the block layout shifts and it is
//!    re-analysed at a different weighting each time. Here the block grid is
//!    **fixed to the sample stream**: block `k` always covers decimated
//!    samples `[k·HOP, k·HOP + NFFT)`, whatever the drain hands over, so each
//!    block of audio is analysed exactly once and the artifact cannot form.
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
//!                                    Hann/50% blocks on a FIXED grid
//!                                          |            -> Sxx,Syy,Sxy
//!                                          v
//!                                    BlockAverage (plain mean of last N,
//!                                          |       N uniform across stages)
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
pub mod average;
pub mod decimate;
pub mod ladder;
pub mod splice;

use realfft::RealFftPlanner;

use align::PairAligner;
use average::BlockAverage;
use decimate::PairDecimator;
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
    avg: BlockAverage,
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
    pub fn new(sr: u32, offset: i64, n_blocks: usize) -> Result<Self, ladder::LadderError> {
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
                    avg: BlockAverage::new(bins, n_blocks),
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

    /// Blocks analysed per stage so far — warmup progress, shallowest first.
    pub fn blocks(&self) -> Vec<u64> {
        self.bands.iter().map(|b| b.avg.total_blocks()).collect()
    }

    /// Blocks averaged per stage.
    pub fn n_blocks(&self) -> usize {
        self.bands
            .first()
            .map(|b| b.avg.n_blocks())
            .unwrap_or_default()
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

            // Fixed block grid (criterion 5b). `buf` always begins at a whole
            // multiple of HOP in the decimated stream, because the only thing
            // ever removed from its front is a whole number of hops. The block
            // boundaries are therefore a property of the stream, not of how
            // the drain happened to chunk it — which is precisely what today's
            // head-relative segmentation gets wrong.
            let mut pos = 0usize;
            while pos + NFFT <= band.buf_meas.len() {
                analyse_block(
                    &mut self.planner,
                    &self.window,
                    &band.buf_meas[pos..pos + NFFT],
                    &band.buf_ref[pos..pos + NFFT],
                    &mut band.avg,
                );
                pos += HOP;
            }
            if pos > 0 {
                band.buf_meas.drain(..pos);
                band.buf_ref.drain(..pos);
            }
        }
    }

    /// Assemble display columns. `None` until every stage holds a full `N`
    /// blocks.
    ///
    /// Gating on the full `N` rather than on the first block is what makes the
    /// reported `N` unambiguous — every column, at every frequency, is the
    /// mean of the same number of blocks. The wait is the settling time the
    /// design already states: `W + hop·(N−1)`, 2.56 s at the bottom stage.
    pub fn columns(&self, f_min: f64, f_max: f64, ppo: f64) -> Option<Vec<Column>> {
        // The display fills downward as the rungs settle rather than staying
        // blank until the deepest one does. Only the bottom rung takes the
        // full 2.56 s; the top is ready in 0.11 s at 96 kHz, and that is the
        // band a rattle is hunted in. Waiting for all three would hide a live
        // top band behind a settling bottom one for 2.4 s of every session.
        //
        // A stage that has not completed its `N` blocks contributes nothing —
        // it is not drawn at a shallower average. Every emitted column is
        // therefore backed by the same `N`, which is what keeps the coherence
        // bias equal either side of a crossover; an under-averaged column
        // would reintroduce that step at a frequency that drifts as the
        // ladder warms.
        if self.bands.iter().all(|b| !b.avg.settled()) {
            return None;
        }
        let edges = ladder::column_edges(&self.ladder, f_min, f_max, ppo);
        if edges.len() < 2 {
            return None;
        }
        let means: Vec<Option<StageMean>> = self
            .bands
            .iter()
            .map(|b| {
                let (sxx, syy, sxy) = b.avg.settled().then(|| b.avg.mean()).flatten()?;
                Some(StageMean {
                    sxx,
                    syy,
                    sxy,
                    // Blocks actually averaged, not the configured target. If
                    // the `settled()` gate above is ever loosened, the frame
                    // must report the depth it really has rather than the one
                    // it aimed for — a column averaged over one block that
                    // claims four is the failure criterion 5c exists to catch,
                    // and nothing downstream could detect it.
                    n: b.avg.held(),
                })
            })
            .collect();
        let views: Vec<Option<StageSpectra<'_>>> = means
            .iter()
            .map(|m| {
                m.as_ref().map(|m| StageSpectra {
                    sxx: &m.sxx,
                    syy: &m.syy,
                    sxy: &m.sxy,
                    n: m.n,
                })
            })
            .collect();
        let cols = splice::assemble(&self.ladder, &views, &edges);
        (!cols.is_empty()).then_some(cols)
    }

    /// Which rungs have settled, shallowest first. Exposed so a caller can
    /// report warmup progress rather than inferring it from a shrinking
    /// column list.
    pub fn settled_stages(&self) -> Vec<bool> {
        self.bands.iter().map(|b| b.avg.settled()).collect()
    }
}

/// Owned per-stage mean, so the splice can borrow it for the assemble call.
struct StageMean {
    sxx: Vec<f64>,
    syy: Vec<f64>,
    sxy: Vec<realfft::num_complex::Complex<f64>>,
    n: usize,
}

/// One Hann-windowed block pair into the stage's average.
///
/// Note what is *not* here: no dB, no magnitude, no division. The block
/// contributes raw `Sxx`, `Syy`, `Sxy`; everything derived comes later and
/// once.
fn analyse_block(
    planner: &mut RealFftPlanner<f64>,
    window: &[f64],
    meas: &[f64],
    reference: &[f64],
    avg: &mut BlockAverage,
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
    avg.push_block(sxx, syy, sxy);
}

/// Seconds of audio a stage needs before its average is full: `W + hop·(N−1)`.
///
/// Exposed so a viewer can say how long a band takes to settle without
/// reverse-engineering it from the frame rate.
pub fn settling_seconds(stage: &Stage, n_blocks: usize) -> f64 {
    stage.window_s + stage.hop_s * (n_blocks.max(1) - 1) as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic broadband source, seekable at any index so a delayed copy
    /// is exact.
    fn source_at(index: i64) -> f32 {
        source_seeded(0xC0FF_EEC0_FFEE, index)
    }

    /// As `source_at`, with an explicit seed so two independent streams can be
    /// built for partially-coherent and uncorrelated stimuli.
    fn source_seeded(seed: u64, index: i64) -> f32 {
        if index < 0 {
            return 0.0;
        }
        let mut z = seed.wrapping_add((index as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15));
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        (((z >> 40) as f64 / (1u64 << 24) as f64) * 2.0 - 1.0) as f32
    }

    /// Drive a pair through `secs` of a delayed, scaled copy of one source.
    fn run(sr: u32, gain: f32, dut_delay: i64, offset: i64, secs: f64) -> MtwPair {
        let mut p = MtwPair::new(sr, offset, 4).unwrap();
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

    /// Criterion 5b, the #208 fix: block boundaries are a property of the
    /// sample stream, not of how the drain happened to chunk it.
    ///
    /// Today's code cuts blocks from the head of a buffer that slides by a
    /// variable amount each tick, so a transient's position inside the block
    /// layout shifts and it is re-analysed at a different weighting each time.
    /// The direct test of the fix is that the *same audio*, delivered in
    /// wildly different chunk sizes, produces bit-identical columns. Under
    /// head-relative segmentation it could not.
    #[test]
    fn block_boundaries_do_not_move_with_the_drain() {
        fn run_chunked(chunks: &[usize]) -> Vec<Column> {
            let mut p = MtwPair::new(48_000, 0, 4).unwrap();
            let n = 48_000i64 * 12;
            let mut i = 0i64;
            let mut c = 0usize;
            while i < n {
                let len = chunks[c % chunks.len()].min((n - i) as usize);
                c += 1;
                let meas: Vec<f32> = (0..len).map(|k| 0.5 * source_at(i + k as i64)).collect();
                let refc: Vec<f32> = (0..len).map(|k| source_at(i + k as i64)).collect();
                p.push(&meas, &refc);
                i += len as i64;
            }
            p.columns(20.0, 24_000.0, 48.0).expect("warm")
        }

        let steady = run_chunked(&[2_400]);
        // Deliberately ragged, including chunks that straddle and undershoot a
        // hop, which is what a real drain does under load.
        let ragged = run_chunked(&[1, 4_801, 97, 12_000, 331, 2_048]);
        assert_eq!(
            steady.len(),
            ragged.len(),
            "column count moved with the drain"
        );
        for (a, b) in steady.iter().zip(ragged.iter()) {
            assert!(
                (a.h1 - b.h1).norm() < 1e-9 && (a.coherence - b.coherence).abs() < 1e-9,
                "column at {} Hz moved with the drain: {:?} vs {:?}",
                a.freq,
                a,
                b
            );
        }
    }

    /// #208's symptom: one transient must be reported once, not repeatedly.
    ///
    /// The observable is the stage's own averaged energy over time. A single
    /// impulse must produce a single contiguous episode — rise, hold while it
    /// sits inside the N-block window, fall as it is evicted — and then
    /// nothing. A second rise after the trace has returned to the floor is the
    /// recurrence the reporter saw.
    #[test]
    fn an_impulse_is_reported_once_and_does_not_recur() {
        let mut p = MtwPair::new(48_000, 0, 4).unwrap();
        let block = 2_400usize;
        let mut trace: Vec<f64> = Vec::new();
        for tick in 0..300 {
            let meas: Vec<f32> = (0..block)
                .map(|k| if tick == 80 && k == 0 { 1.0 } else { 0.0 })
                .collect();
            let refc = vec![0.0f32; block];
            p.push(&meas, &refc);
            let energy = p.bands[0]
                .avg
                .mean()
                .map(|(_, syy, _)| syy.iter().sum::<f64>())
                .unwrap_or(0.0);
            trace.push(energy);
        }
        let peak = trace.iter().cloned().fold(0.0f64, f64::max);
        assert!(peak > 0.0, "impulse never registered");

        // One contiguous run above the floor, and nothing after it.
        let floor = peak * 1e-9;
        let first = trace.iter().position(|&v| v > floor).unwrap();
        let last = trace.iter().rposition(|&v| v > floor).unwrap();
        for (i, &v) in trace.iter().enumerate().take(last + 1).skip(first) {
            assert!(
                v > floor,
                "energy returned to the floor at tick {i} and rose again — \
                 the impulse is being re-analysed"
            );
        }
        // It really did leave, rather than the run simply reaching the end.
        assert!(last + 1 < trace.len(), "impulse never left the average");
        // And it was single-peaked: no second maximum after the decay begins.
        let peak_at = trace.iter().position(|&v| v == peak).unwrap();
        for w in trace[peak_at..].windows(2) {
            assert!(
                w[1] <= w[0] + peak * 1e-12,
                "energy rose again after the peak — recurrence"
            );
        }
    }

    /// Settling is `W + hop·(N−1)` per stage. The ratified figures at 96 kHz:
    /// 0.11 s at the top, 0.85 s in the middle, 2.56 s at the bottom — the
    /// last matching today's 2.5 s, so low frequency is not made slower.
    #[test]
    fn settling_matches_the_ratified_figures() {
        let l = ladder::layout(96_000).unwrap();
        let s: Vec<f64> = l.stages.iter().map(|st| settling_seconds(st, 4)).collect();
        assert!((s[0] - 0.106_667).abs() < 1e-4, "top {}", s[0]);
        assert!((s[1] - 0.853_333).abs() < 1e-4, "middle {}", s[1]);
        assert!((s[2] - 2.560_000).abs() < 1e-4, "bottom {}", s[2]);
        assert!(
            s[2] <= 2.6,
            "the bottom must not get slower than today's 2.5 s: {}",
            s[2]
        );
        // The top improves roughly twelvefold against today's 2.5 s.
        assert!(2.5 / s[0] > 20.0, "top stage only {}x faster", 2.5 / s[0]);
    }

    /// End to end: the column set grows downward as the rungs settle, instead
    /// of appearing all at once after the deepest one does.
    ///
    /// Drives real audio and samples the column list over time, so this
    /// exercises the actual block accounting rather than a hand-set settled
    /// flag.
    #[test]
    fn the_display_fills_downward_as_rungs_settle() {
        let sr = 96_000u32;
        let mut p = MtwPair::new(sr, 0, 4).unwrap();
        let block = 4_800usize; // 50 ms
        let mut first_seen_at: Option<f64> = None;
        let mut samples: Vec<(f64, usize, f64)> = Vec::new(); // (t, n_cols, lowest_hz)

        for tick in 0..120 {
            let t = tick as f64 * 0.05;
            let i = (tick * block) as i64;
            let meas: Vec<f32> = (0..block).map(|k| 0.5 * source_at(i + k as i64)).collect();
            let refc: Vec<f32> = (0..block).map(|k| source_at(i + k as i64)).collect();
            p.push(&meas, &refc);
            if let Some(cols) = p.columns(20.0, 48_000.0, 48.0) {
                first_seen_at.get_or_insert(t);
                samples.push((t, cols.len(), cols[0].freq));
            }
        }

        let first = first_seen_at.expect("columns must appear");
        assert!(
            first < 0.5,
            "first columns at {first} s — the top rung settles in 0.11 s and \
             should not wait for the bottom one"
        );

        // Column count never shrinks, and the lowest drawn frequency never
        // rises: the display fills, it does not flicker.
        for w in samples.windows(2) {
            assert!(w[1].1 >= w[0].1, "column count fell at {} s", w[1].0);
            assert!(
                w[1].2 <= w[0].2 + 1e-9,
                "lowest column rose at {} s: {} -> {}",
                w[1].0,
                w[0].2,
                w[1].2
            );
        }

        // It genuinely grew, and reached the bottom rung by its settling time.
        let (t0, n0, lo0) = samples[0];
        let (tn, nn, lon) = *samples.last().unwrap();
        assert!(nn > n0 * 2, "{n0} -> {nn} columns ({t0} s -> {tn} s)");
        assert!(
            lo0 > 200.0,
            "first fill reached {lo0} Hz, too deep for one rung"
        );
        assert!(lon < 25.0, "never reached the bottom rung: {lon} Hz");
        assert_eq!(p.settled_stages(), vec![true, true, true]);
    }

    /// Criterion 5c. Every emitted column reports the configured `N`, at every
    /// point in warmup — not only once every rung has settled.
    ///
    /// Drives the real pipeline rather than a fixture, because the property
    /// depends on the `settled()` gate actually filtering: an under-averaged
    /// rung must contribute nothing, not a shallower average. It can only bite
    /// because `n` is taken from blocks *held* — reporting the configured
    /// target instead would let an under-averaged column claim a depth it does
    /// not have, and no assertion could see it.
    #[test]
    fn every_emitted_column_reports_the_configured_n_throughout_warmup() {
        let sr = 96_000u32;
        let n_target = 4usize;
        let mut p = MtwPair::new(sr, 0, n_target).unwrap();
        let block = 4_800usize;
        let mut sampled = 0usize;
        for tick in 0..120 {
            let i = (tick * block) as i64;
            let meas: Vec<f32> = (0..block).map(|k| 0.5 * source_at(i + k as i64)).collect();
            let refc: Vec<f32> = (0..block).map(|k| source_at(i + k as i64)).collect();
            p.push(&meas, &refc);
            let Some(cols) = p.columns(20.0, 48_000.0, 48.0) else {
                continue;
            };
            sampled += 1;
            let depths: std::collections::BTreeSet<usize> = cols.iter().map(|c| c.n).collect();
            assert_eq!(
                depths,
                std::collections::BTreeSet::from([n_target]),
                "tick {tick}: emitted columns average over {depths:?} blocks, \
                 configured {n_target} — an under-averaged column reached the display"
            );
        }
        assert!(sampled > 20, "warmup barely sampled: {sampled} frames");
    }

    /// Criterion 5. The coherence floor on uncorrelated inputs is `1/N` per
    /// column **at one bin**.
    ///
    /// Single-bin columns only, because depth grows with bin count and a stage
    /// average therefore runs below `1/N` — that is the bin effect, not an
    /// estimator defect.
    ///
    /// The band deliberately **excludes 0.3125**, the figure a Welch ρ = 1/6
    /// overlap correction would predict. That correction does not apply to
    /// coherence bias (it corrects power-spectrum *variance*), it was once
    /// applied here, and it shipped a value further from truth than the
    /// uncorrected one. This test fails if it comes back.
    #[test]
    fn uncorrelated_floor_is_one_over_n_per_column_at_one_bin() {
        let sr = 96_000u32;
        let n = 4usize;
        let mut acc: Vec<f64> = Vec::new();
        for run in 0..8u64 {
            let mut p = MtwPair::new(sr, 0, n).unwrap();
            let blk = 4_800usize;
            for t in 0..70i64 {
                let i = t * blk as i64;
                let m: Vec<f32> = (0..blk)
                    .map(|k| source_seeded(0xAAA0 + run, i + k as i64))
                    .collect();
                let r: Vec<f32> = (0..blk)
                    .map(|k| source_seeded(0x5550 + run, i + k as i64))
                    .collect();
                p.push(&m, &r);
            }
            if let Some(cols) = p.columns(20.0, 48_000.0, 48.0) {
                acc.extend(
                    cols.iter()
                        .filter(|c| c.bins == 1 && c.blend == 0.0)
                        .map(|c| c.coherence),
                );
            }
        }
        assert!(acc.len() > 300, "too few single-bin columns: {}", acc.len());
        let mean = acc.iter().sum::<f64>() / acc.len() as f64;
        let want = 1.0 / n as f64;
        assert!(
            (mean - want).abs() < 0.05,
            "floor {mean:.4} over {} columns, expected 1/N = {want:.4}",
            acc.len()
        );
        assert!(
            mean < 0.29,
            "floor {mean:.4} is at the 1/3.2 = 0.3125 an overlap correction \
             would predict — that correction does not apply to coherence bias"
        );
    }

    /// Criterion 3. The coherence step at a crossover is **present, of the
    /// documented magnitude, and does not move as the ladder warms**.
    ///
    /// The step is structural, not a defect: crossovers sit at the
    /// reference-density validity edge, which pins the upper side at exactly
    /// one bin per column, while the lower side is deeper by the decimation
    /// ratio. It is accepted and documented (`design-mtw-ladder.md`), so this
    /// asserts it stays put rather than asserting it away.
    ///
    /// A step that *moved* during warmup would read as a wandering DUT feature
    /// — strictly worse than a fixed one. It cannot move here because a
    /// crossover column is withheld until both its stages have settled, and
    /// this pins that.
    ///
    /// If a future change genuinely reduces the step, this test fails and the
    /// documented figure should be updated — deliberately, not silently.
    #[test]
    fn the_crossover_coherence_step_is_stable_across_warmup() {
        let sr = 96_000u32;
        let l = ladder::layout(sr).unwrap();
        let (x, bt) = (l.stages[1].f_top, l.stages[1].blend_top);
        let blk = 4_800usize;

        // gamma^2 = 0.5: meas = source + an equal, independent noise.
        let step_at = |ticks: i64| -> f64 {
            let (mut sb, mut sa, mut nb, mut na) = (0.0, 0.0, 0usize, 0usize);
            for run in 0..6u64 {
                let (s0, s1) = (0xC0FF_0000 + run * 7919, 0xD00D_0000 + run * 6271);
                let mut p = MtwPair::new(sr, 0, 4).unwrap();
                for t in 0..ticks {
                    let i = t * blk as i64;
                    let m: Vec<f32> = (0..blk)
                        .map(|k| source_seeded(s0, i + k as i64) + source_seeded(s1, i + k as i64))
                        .collect();
                    let r: Vec<f32> = (0..blk).map(|k| source_seeded(s0, i + k as i64)).collect();
                    p.push(&m, &r);
                }
                let Some(cols) = p.columns(20.0, 48_000.0, 48.0) else {
                    continue;
                };
                for c in &cols {
                    if c.freq > x * 0.6 && c.freq < x {
                        sb += c.coherence;
                        nb += 1;
                    }
                    if c.freq > bt && c.freq < bt * 1.7 {
                        sa += c.coherence;
                        na += 1;
                    }
                }
            }
            assert!(nb > 0 && na > 0, "crossover not drawn at {ticks} ticks");
            (sb / nb as f64 - sa / na as f64).abs()
        };

        // Both stages of this crossover settle by ~0.85 s; 30 ticks = 1.5 s is
        // the first point at which it exists at all.
        let early = step_at(30);
        let late = step_at(110);
        for (name, v) in [("early", early), ("late", late)] {
            assert!(
                (0.01..0.10).contains(&v),
                "{name} step {v:.4} is outside the documented ~0.05 — either it \
                 vanished (update the doc) or it grew (a regression)"
            );
        }
        assert!(
            (early - late).abs() < 0.03,
            "step moved with warmup: {early:.4} -> {late:.4}. A step that drifts \
             reads as a wandering DUT feature, which is worse than a fixed one"
        );
    }
}
