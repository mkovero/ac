//! What the fake backend synthesises, and the state that synthesis carries
//! between calls.
//!
//! [`StimulusGen`] owns every field that exists only to feed a sample
//! generator: the configured stimulus and the per-channel positions the
//! `Noise` and `CorrelatedPair` arms advance. Keeping them here rather than
//! on `FakeEngine` is what lets ring mode borrow the generator and the ring
//! producers at the same time (see [`Synth`]).

use std::collections::HashMap;
use std::f64::consts::PI;

/// Channel-index → frequency offset, in Hz. Picked so that two channels
/// never alias into the same FFT bin at common analysis lengths.
pub(super) const CHANNEL_OFFSET_HZ: f64 = 100.0;

/// What `capture_block` / `capture_stereo` synthesize. `Tones` covers both
/// the historical single-tone `set_tone` path and the multi-tone
/// `set_tone_pair` path added for the display-truth harness (#170) — a
/// single-element vec reproduces the old behaviour exactly. `Noise` is a
/// deterministic pseudo-random broadband signal for the I2 flat-noise
/// continuity invariant; deterministic (fixed LCG per channel offset) so
/// harness runs are reproducible. `CorrelatedPair` (handoff: parity-
/// completion M1.5) is a fake DUT with known ground truth: the ref-role
/// port carries a seeded broadband source, the meas-role port carries the
/// *same* source scaled and delayed — `|H1| = gain`, coherence ≈ 1.
#[derive(Clone)]
pub(super) enum Stimulus {
    Tones(Vec<(f64, f64)>),
    Noise(f64),
    CorrelatedPair { gain: f64, delay_samples: usize },
}

/// Fixed seed for `CorrelatedPair` — deterministic across runs so
/// fixture regeneration (`ac_core::snapshot`'s regenerator test) is
/// reproducible: same seed, same stimulus, same `.acsnap` bytes, same
/// sha256, every time.
pub(super) const CORRELATED_PAIR_SEED: u64 = 0xC0FFEE_C0FFEE_u64;

/// Deterministic pseudo-random sample at absolute index `index`, in
/// `[-1, 1)`. A *pure* function of `(seed, index)` — unlike `Stimulus::
/// Noise`'s sequentially-advanced LCG, this needs to be independently
/// seekable at arbitrary (possibly negative-relative, i.e. "before the
/// source existed") offsets, since the meas-role reads the same
/// underlying stream `delay_samples` behind the ref-role's position with
/// no shared mutable cursor between the two (call order between meas and
/// ref within one tick is not guaranteed — see `make_samples_for`).
/// SplitMix64's finalizer — good avalanche, no persistent state needed.
pub(super) fn correlated_source_at(seed: u64, index: u64) -> f32 {
    let mut z = seed.wrapping_add(index.wrapping_mul(0x9E37_79B9_7F4A_7C15));
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^= z >> 31;
    let u = ((z >> 40) as f64 / (1u64 << 24) as f64) * 2.0 - 1.0;
    u as f32
}

impl Default for Stimulus {
    fn default() -> Self {
        Stimulus::Tones(vec![(1_000.0, 0.0)])
    }
}

/// Everything the sample generator reads or advances, split out of
/// `FakeEngine` so the ring producers can be borrowed alongside it.
///
/// Four fields of `FakeEngine` existed solely to serve one `&mut self`
/// generator call, which forced ring mode to synthesise every channel into
/// an intermediate `Vec<Vec<f32>>` before it could touch a producer. As its
/// own field the generator is a disjoint borrow, so ring mode writes each
/// block straight into its ring.
#[derive(Default)]
pub(super) struct StimulusGen {
    stimulus: Stimulus,
    /// Per-channel-offset LCG state for `Stimulus::Noise`, keyed on the
    /// offset's bit pattern (one entry per distinct channel). Persisted
    /// across `capture_block`/`capture_stereo` calls so a soak driving the
    /// I5 display-truth invariant sees a genuine continuing
    /// pseudorandom stream rather than the same block on every tick — the
    /// LCG used to be re-seeded to the same fixed state on every single
    /// call (state was a local var in the generator), so a ring buffer fed
    /// one identical block per tick became a periodic buffer after
    /// wrapping, freezing the FFT output on whatever comb spectrum that
    /// periodicity produced. Reproducible from a fresh engine (same offset
    /// -> same starting state) so replay from a logged seed still works;
    /// see `noise_stream_advances_across_calls`.
    noise_state: HashMap<u64, u64>,
    /// Absolute-sample read position per role for `Stimulus::
    /// CorrelatedPair`, tracked independently (not a shared cursor) so
    /// the two roles' blocks are correct regardless of which is
    /// generated first within a tick — see `correlated_source_at`'s doc.
    /// Both advance by the same `n` each tick since `capture_stereo`/
    /// `capture_multi` always request the same `duration` for both, so
    /// they stay equal call-for-call; that equality (not call order) is
    /// what makes "ref now" and "meas now, sourced from `now - delay`"
    /// consistent.
    correlated_ref_pos: u64,
    /// Read position per **measurement** port, not one shared meas cursor.
    ///
    /// With a single measurement channel the two are identical. With two
    /// (`pairs=[[0,3],[1,3]]`, the second measurement position) a shared
    /// cursor advances twice per tick — once per channel — so each channel
    /// would read a different window of the source and the second one would
    /// carry a delay that is an artefact of call order rather than of
    /// `delay_samples`. Keyed by port name so every measurement channel
    /// tracks the ref independently.
    correlated_meas_pos: HashMap<String, u64>,
}

impl StimulusGen {
    pub(super) fn set(&mut self, stimulus: Stimulus) {
        self.stimulus = stimulus;
    }

    /// Set the correlated-pair stimulus and restart both roles at `t = 0`.
    ///
    /// Fresh stimulus, fresh positions — otherwise a session that switches
    /// stimulus mid-life would read from a stale absolute index instead of
    /// starting the pair cleanly at `t = 0`.
    pub(super) fn set_correlated_pair(&mut self, gain: f64, delay_samples: usize) {
        self.stimulus = Stimulus::CorrelatedPair {
            gain,
            delay_samples,
        };
        self.correlated_ref_pos = 0;
        self.correlated_meas_pos.clear();
    }

    /// Effective (channel-shifted) frequency of the first configured tone.
    /// Test-only: multi-tone stimuli don't have one "the" frequency so
    /// this only inspects `tones[0]`, which is enough for the single-tone
    /// regression coverage below.
    #[cfg(test)]
    pub(super) fn effective_freq(&self, port: Option<&str>) -> f64 {
        let offset = channel_offset_hz(port);
        match &self.stimulus {
            Stimulus::Tones(tones) => tones.first().map(|&(f, _)| f + offset).unwrap_or(0.0),
            Stimulus::Noise(_) | Stimulus::CorrelatedPair { .. } => offset,
        }
    }
}

/// Parse the trailing channel index from a `fake:<kind>_<N>` name.
/// Returns 0 when the format doesn't match.
pub(super) fn channel_index(port: &str) -> usize {
    port.rsplit('_')
        .next()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(0)
}

fn channel_offset_hz(port: Option<&str>) -> f64 {
    let ch = port.map(channel_index).unwrap_or(0);
    ch as f64 * CHANNEL_OFFSET_HZ
}

/// A borrowed [`StimulusGen`] plus the per-session context every synthesis
/// call needs, so callers pass one value instead of threading three.
///
/// `ref_port` is the reference leg proper — the **first** port registered
/// through `add_ref_input`. `CorrelatedPair`'s role dispatch turns on it:
/// that port is the source, anything else reads the same source scaled and
/// delayed.
pub(super) struct Synth<'a> {
    pub(super) gen: &'a mut StimulusGen,
    pub(super) sample_rate: u32,
    pub(super) ref_port: Option<&'a str>,
}

impl Synth<'_> {
    /// Generate `duration` seconds of synthetic signal for `port`'s channel
    /// (frequency-shifted per [`CHANNEL_OFFSET_HZ`]), with tone phase
    /// advanced to absolute sample position `tone_start`.
    ///
    /// Only ring mode passes a nonzero `tone_start`; the on-demand path
    /// calls this with `0`, which reduces to the original expression
    /// exactly, so default output stays byte-identical. `Noise` and
    /// `CorrelatedPair` already track their own absolute position and are
    /// unaffected.
    pub(super) fn block(&mut self, port: Option<&str>, duration: f64, tone_start: u64) -> Vec<f32> {
        let sr = self.sample_rate as f64;
        let n = (sr * duration) as usize;
        let offset = channel_offset_hz(port);
        match &self.gen.stimulus {
            Stimulus::Tones(tones) => {
                // Historical default: nothing has set a nonzero amplitude
                // yet → fall back to a 0.1-amplitude sine so `--fake-audio`
                // produces plausible output out of the box (unchanged from
                // pre-#170 behaviour).
                let effective: Vec<(f64, f64)> = if tones.iter().all(|&(_, a)| a <= 0.0) {
                    vec![(tones.first().map(|&(f, _)| f).unwrap_or(1_000.0), 0.1)]
                } else {
                    tones.clone()
                };
                (0..n)
                    .map(|i| {
                        let t = (tone_start + i as u64) as f64 / sr;
                        let sig: f64 = effective
                            .iter()
                            .map(|&(freq, amp)| {
                                let f = freq + offset;
                                amp * (2.0 * PI * f * t).sin()
                                    + amp * 0.01 * (4.0 * PI * f * t).sin()
                            })
                            .sum();
                        sig as f32
                    })
                    .collect()
            }
            Stimulus::Noise(amp) => {
                let amp = if *amp > 0.0 { *amp } else { 0.1 };
                // Deterministic LCG, seeded from the channel offset so
                // simultaneously-captured channels (meas/ref) don't share
                // one sample sequence. Not spectrally flattened to true
                // pink/white — good enough as a calibrated-amplitude
                // broadband stimulus for I2, which checks for band-boundary
                // steps rather than an exact spectral shape.
                //
                // State persists in `noise_state` across calls (keyed by
                // the channel offset) so consecutive captures continue the
                // same pseudorandom stream instead of each replaying an
                // identical block — see the field doc on `noise_state`.
                let key = offset.to_bits();
                let state = self
                    .gen
                    .noise_state
                    .entry(key)
                    .or_insert(0x9E3779B97F4A7C15 ^ key);
                (0..n)
                    .map(|_| {
                        *state = state
                            .wrapping_mul(6364136223846793005)
                            .wrapping_add(1442695040888963407);
                        let u = ((*state >> 40) as f64 / (1u64 << 24) as f64) * 2.0 - 1.0;
                        (amp * u) as f32
                    })
                    .collect()
            }
            Stimulus::CorrelatedPair {
                gain,
                delay_samples,
            } => {
                let gain = *gain as f32;
                let delay_samples = *delay_samples as u64;
                // Role dispatch: the ref-role port (`self.ref_port`) is the
                // source; anything else (the meas-role `input_port`, in
                // practice) reads the same source, scaled and delayed.
                // Independent per-role position counters (not a shared
                // cursor) — see the field doc on `correlated_ref_pos`.
                let is_ref = port.is_some() && port == self.ref_port;
                let start_pos = if is_ref {
                    let p = self.gen.correlated_ref_pos;
                    self.gen.correlated_ref_pos += n as u64;
                    p
                } else {
                    let slot = self
                        .gen
                        .correlated_meas_pos
                        .entry(port.unwrap_or_default().to_string())
                        .or_insert(0);
                    let p = *slot;
                    *slot += n as u64;
                    p
                };
                (0..n)
                    .map(|i| {
                        let abs_index = start_pos + i as u64;
                        if is_ref {
                            correlated_source_at(CORRELATED_PAIR_SEED, abs_index)
                        } else {
                            // Silence before the source "existed" (real DUT:
                            // no output before its input arrived) rather
                            // than wrapping into negative-index territory.
                            match abs_index.checked_sub(delay_samples) {
                                Some(src_index) => {
                                    gain * correlated_source_at(CORRELATED_PAIR_SEED, src_index)
                                }
                                None => 0.0,
                            }
                        }
                    })
                    .collect()
            }
        }
    }
}
