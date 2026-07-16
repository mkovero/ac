//! Fake audio engine for tests and `--fake-audio` mode.
//!
//! Issue #34: the fake backend models routing so that tests can verify
//! `reconnect_input` / `add_ref_input` / `connect_output` actually changed
//! the channel the caller will sample.
//!
//! Implementation: every "fake:capture_N" / "fake:playback_N" port name
//! carries a channel index. `capture_block()` synthesizes a sine at
//! `freq_hz + channel_idx * 100 Hz`, so a test that reroutes from
//! `fake:capture_0` to `fake:capture_3` and captures at a nominal 1 kHz will
//! observe energy at 1 300 Hz instead. `capture_stereo()` emits independent
//! offsets for the measurement and reference channels.

use anyhow::Result;
use std::collections::HashMap;
use std::f64::consts::PI;
use std::time::Duration;

use super::AudioEngine;

/// Channel-index → frequency offset, in Hz. Picked so that two channels
/// never alias into the same FFT bin at common analysis lengths.
const CHANNEL_OFFSET_HZ: f64 = 100.0;

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
enum Stimulus {
    Tones(Vec<(f64, f64)>),
    Noise(f64),
    CorrelatedPair { gain: f64, delay_samples: usize },
}

/// Fixed seed for `CorrelatedPair` — deterministic across runs so
/// fixture regeneration (`ac_core::snapshot`'s regenerator test) is
/// reproducible: same seed, same stimulus, same `.acsnap` bytes, same
/// sha256, every time.
const CORRELATED_PAIR_SEED: u64 = 0xC0FFEE_C0FFEE_u64;

/// Deterministic pseudo-random sample at absolute index `index`, in
/// `[-1, 1)`. A *pure* function of `(seed, index)` — unlike `Stimulus::
/// Noise`'s sequentially-advanced LCG, this needs to be independently
/// seekable at arbitrary (possibly negative-relative, i.e. "before the
/// source existed") offsets, since the meas-role reads the same
/// underlying stream `delay_samples` behind the ref-role's position with
/// no shared mutable cursor between the two (call order between meas and
/// ref within one tick is not guaranteed — see `make_samples_for`).
/// SplitMix64's finalizer — good avalanche, no persistent state needed.
fn correlated_source_at(seed: u64, index: u64) -> f32 {
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

pub struct FakeEngine {
    sample_rate: u32,
    stimulus: Stimulus,
    xruns: u32,
    output_ports: Vec<String>,
    input_port: Option<String>,
    ref_port: Option<String>,
    /// Per-channel-offset LCG state for `Stimulus::Noise`, keyed on the
    /// offset's bit pattern (one entry per distinct channel). Persisted
    /// across `capture_block`/`capture_stereo` calls so a soak driving the
    /// I5 display-truth invariant (handoff.md) sees a genuine continuing
    /// pseudorandom stream rather than the same block on every tick — the
    /// LCG used to be re-seeded to the same fixed state on every single
    /// call (state was a local var in `make_samples_for`, `&self`), so a
    /// ring buffer fed one identical block per tick became a periodic
    /// buffer after wrapping, freezing the FFT output on whatever comb
    /// spectrum that periodicity produced. Reproducible from a fresh
    /// engine (same offset -> same starting state) so replay from a
    /// logged seed still works; see `noise_stream_advances_across_calls`.
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
    correlated_meas_pos: u64,
}

impl FakeEngine {
    pub fn new() -> Self {
        Self {
            sample_rate: 48_000,
            stimulus: Stimulus::default(),
            xruns: 0,
            output_ports: Vec::new(),
            input_port: None,
            ref_port: None,
            noise_state: HashMap::new(),
            correlated_ref_pos: 0,
            correlated_meas_pos: 0,
        }
    }

    /// Parse the trailing channel index from a `fake:<kind>_<N>` name.
    /// Returns 0 when the format doesn't match.
    fn channel_index(port: &str) -> usize {
        port.rsplit('_')
            .next()
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(0)
    }

    fn channel_offset_hz(port: Option<&str>) -> f64 {
        let ch = port.map(Self::channel_index).unwrap_or(0);
        ch as f64 * CHANNEL_OFFSET_HZ
    }

    /// Effective (channel-shifted) frequency of the first configured tone.
    /// Test-only: multi-tone stimuli don't have one "the" frequency so
    /// this only inspects `tones[0]`, which is enough for the single-tone
    /// regression coverage below.
    #[cfg(test)]
    fn effective_freq(&self, port: Option<&str>) -> f64 {
        let offset = Self::channel_offset_hz(port);
        match &self.stimulus {
            Stimulus::Tones(tones) => tones.first().map(|&(f, _)| f + offset).unwrap_or(0.0),
            Stimulus::Noise(_) | Stimulus::CorrelatedPair { .. } => offset,
        }
    }

    /// Generate `duration` seconds of synthetic signal for `port`'s channel
    /// (frequency-shifted per `CHANNEL_OFFSET_HZ`, same as pre-#170).
    fn make_samples_for(&mut self, port: Option<&str>, duration: f64) -> Vec<f32> {
        let n = (self.sample_rate as f64 * duration) as usize;
        let offset = Self::channel_offset_hz(port);
        let sr = self.sample_rate as f64;
        match &self.stimulus {
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
                        let t = i as f64 / sr;
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
                // State persists in `self.noise_state` across calls (keyed
                // by the channel offset) so consecutive captures continue
                // the same pseudorandom stream instead of each replaying an
                // identical block — see the field doc on `noise_state`.
                let key = offset.to_bits();
                let state = self
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
                let is_ref = port.is_some() && port == self.ref_port.as_deref();
                let start_pos = if is_ref {
                    let p = self.correlated_ref_pos;
                    self.correlated_ref_pos += n as u64;
                    p
                } else {
                    let p = self.correlated_meas_pos;
                    self.correlated_meas_pos += n as u64;
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

impl AudioEngine for FakeEngine {
    fn start(&mut self, output_ports: &[String], input_port: Option<&str>) -> Result<()> {
        self.output_ports = output_ports.to_vec();
        self.input_port = input_port.map(str::to_string);
        Ok(())
    }

    fn stop(&mut self) {}

    fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    fn set_tone(&mut self, freq_hz: f64, amplitude: f64) {
        self.stimulus = Stimulus::Tones(vec![(freq_hz, amplitude)]);
    }

    fn set_pink(&mut self, amplitude: f64) {
        self.stimulus = Stimulus::Noise(amplitude);
    }

    fn set_silence(&mut self) {
        self.stimulus = Stimulus::Tones(vec![(1_000.0, 0.0)]);
    }

    fn set_tone_pair(&mut self, tones: &[(f64, f64)]) {
        self.stimulus = Stimulus::Tones(tones.to_vec());
    }

    fn set_broadband_noise(&mut self, amplitude: f64) {
        self.stimulus = Stimulus::Noise(amplitude);
    }

    fn set_correlated_pair(&mut self, gain: f64, delay_samples: usize) {
        self.stimulus = Stimulus::CorrelatedPair {
            gain,
            delay_samples,
        };
        // Fresh stimulus, fresh positions — otherwise a session that
        // switches stimulus mid-life would read from a stale absolute
        // index instead of starting the pair cleanly at t=0.
        self.correlated_ref_pos = 0;
        self.correlated_meas_pos = 0;
    }

    fn capture_block(&mut self, duration: f64) -> Result<Vec<f32>> {
        std::thread::sleep(Duration::from_secs_f64(duration));
        let port = self.input_port.clone();
        Ok(self.make_samples_for(port.as_deref(), duration))
    }

    /// Fake loopback: returns `samples` delayed by a fixed number of
    /// samples (`FAKE_LOOPBACK_DELAY_SAMPLES`), padded with trailing
    /// zeros to `samples.len() + tail` total length. Used by the
    /// `sweep_ir` integration test to verify the deconvolved linear IR
    /// peaks at the expected offset.
    fn play_and_capture(&mut self, samples: &[f32], tail_s: f64) -> Result<Vec<f32>> {
        const FAKE_LOOPBACK_DELAY_SAMPLES: usize = 32;
        let tail = (tail_s * self.sample_rate as f64).round() as usize;
        let total = samples.len() + tail;
        let mut out = vec![0.0f32; total];
        for (i, &s) in samples.iter().enumerate() {
            let j = i + FAKE_LOOPBACK_DELAY_SAMPLES;
            if j < total {
                out[j] = s;
            }
        }
        Ok(out)
    }

    fn capture_stereo(&mut self, duration: f64) -> Result<(Vec<f32>, Vec<f32>)> {
        std::thread::sleep(Duration::from_secs_f64(duration));
        // If no explicit ref_port, reference mirrors the generator (channel 0).
        let in_port = self.input_port.clone();
        let ref_port = self.ref_port.clone();
        let meas = self.make_samples_for(in_port.as_deref(), duration);
        let refch = self.make_samples_for(ref_port.as_deref(), duration);
        Ok((meas, refch))
    }

    fn reconnect_input(&mut self, port: &str) -> Result<()> {
        self.input_port = Some(port.to_string());
        Ok(())
    }

    fn add_ref_input(&mut self, port: &str) -> Result<()> {
        self.ref_port = Some(port.to_string());
        Ok(())
    }

    fn connect_output(&mut self, port: &str) -> Result<()> {
        if !self.output_ports.iter().any(|p| p == port) {
            self.output_ports.push(port.to_string());
        }
        Ok(())
    }

    fn disconnect_output(&mut self, port: &str) {
        self.output_ports.retain(|p| p != port);
    }

    fn flush_capture(&mut self) {}

    fn xruns(&self) -> u32 {
        self.xruns
    }

    fn supports_routing(&self) -> bool {
        true
    }
    fn backend_name(&self) -> &'static str {
        "fake"
    }

    fn playback_ports(&self) -> Vec<String> {
        (0..20).map(|i| format!("fake:playback_{i}")).collect()
    }

    fn capture_ports(&self) -> Vec<String> {
        (0..20).map(|i| format!("fake:capture_{i}")).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_index_parses_trailing_number() {
        assert_eq!(FakeEngine::channel_index("fake:capture_0"), 0);
        assert_eq!(FakeEngine::channel_index("fake:capture_7"), 7);
        assert_eq!(FakeEngine::channel_index("fake:capture_19"), 19);
        assert_eq!(FakeEngine::channel_index("garbage"), 0);
    }

    #[test]
    fn reroute_shifts_effective_frequency() {
        let mut eng = FakeEngine::new();
        eng.set_tone(1_000.0, 0.5);
        eng.reconnect_input("fake:capture_0").unwrap();
        assert!((eng.effective_freq(eng.input_port.as_deref()) - 1_000.0).abs() < 1e-9);
        eng.reconnect_input("fake:capture_3").unwrap();
        assert!((eng.effective_freq(eng.input_port.as_deref()) - 1_300.0).abs() < 1e-9);
    }

    #[test]
    fn capture_multi_matches_stereo_default() {
        // Fake backend inherits the default `capture_multi` which calls
        // `capture_stereo` — covers the CPAL fallback path too.
        let mut eng = FakeEngine::new();
        eng.set_tone(1_000.0, 0.5);
        eng.reconnect_input("fake:capture_0").unwrap();
        eng.add_ref_input("fake:capture_2").unwrap();
        let bufs = eng.capture_multi(0.02).unwrap();
        assert_eq!(bufs.len(), 2);
        assert_eq!(bufs[0].len(), bufs[1].len());
        let diff: f32 = bufs[0]
            .iter()
            .zip(&bufs[1])
            .map(|(a, b)| (a - b).abs())
            .sum();
        assert!(
            diff > 0.0,
            "multi channels should differ between meas and ref"
        );
    }

    #[test]
    fn stereo_channels_are_independent() {
        let mut eng = FakeEngine::new();
        eng.set_tone(1_000.0, 0.5);
        eng.reconnect_input("fake:capture_0").unwrap();
        eng.add_ref_input("fake:capture_2").unwrap();
        let (meas, refch) = eng.capture_stereo(0.02).unwrap();
        // Both non-empty and distinct signals.
        assert!(!meas.is_empty());
        assert_eq!(meas.len(), refch.len());
        let diff: f32 = meas.iter().zip(&refch).map(|(a, b)| (a - b).abs()).sum();
        assert!(diff > 0.0, "meas and ref channels should differ");
    }

    /// Goertzel magnitude at `freq` — enough to confirm energy landed where
    /// a tone was requested without pulling in a full FFT for a unit test.
    fn goertzel_mag(samples: &[f32], sr: f64, freq: f64) -> f64 {
        let n = samples.len();
        let k = (0.5 + (n as f64 * freq) / sr).floor();
        let w = 2.0 * PI * k / n as f64;
        let cw = w.cos();
        let coeff = 2.0 * cw;
        let (mut s1, mut s2) = (0.0_f64, 0.0_f64);
        for &x in samples {
            let s0 = x as f64 + coeff * s1 - s2;
            s2 = s1;
            s1 = s0;
        }
        (s1 * s1 + s2 * s2 - s1 * s2 * coeff).sqrt() / n as f64
    }

    #[test]
    fn tone_pair_synthesizes_both_frequencies() {
        // #170: I3/I1 stimulus needs two simultaneous tones at distinct
        // levels — confirm both actually land in the captured signal, not
        // just the first (the old `set_tone` single-tone behaviour).
        let sr = 48_000;
        let mut eng = FakeEngine::new();
        eng.set_tone_pair(&[(1_000.0, 0.5), (5_000.0, 0.1)]);
        let s = eng.capture_block(0.5).unwrap();
        let m1 = goertzel_mag(&s, sr as f64, 1_000.0);
        let m2 = goertzel_mag(&s, sr as f64, 5_000.0);
        assert!(m1 > 0.1, "expected energy at 1000 Hz, got mag {m1}");
        assert!(m2 > 0.01, "expected energy at 5000 Hz, got mag {m2}");
        assert!(
            m1 > m2,
            "louder tone (0.5) should measure higher than quieter tone (0.1): {m1} vs {m2}"
        );
    }

    /// Regression for the frozen/repeated-block bug the I5 soak invariant
    /// exists to catch (`handoff.md`): before the fix, `Stimulus::Noise`
    /// re-seeded its LCG to the same fixed state on every `capture_block`
    /// call, so a caller polling repeatedly (as `monitor_spectrum`'s LF
    /// ring does) saw the identical block over and over — a ring fed only
    /// identical blocks becomes periodic once fully wrapped, freezing
    /// whatever spectrum falls out of that periodicity. Two consecutive
    /// captures must now differ.
    #[test]
    fn noise_stream_advances_across_calls() {
        let mut eng = FakeEngine::new();
        eng.set_broadband_noise(0.5);
        eng.reconnect_input("fake:capture_0").unwrap();
        let a = eng.capture_block(0.01).unwrap();
        let b = eng.capture_block(0.01).unwrap();
        assert_eq!(a.len(), b.len());
        assert_ne!(
            a, b,
            "consecutive noise captures must not repeat the same block"
        );
    }

    /// Same starting state (fresh engine, same channel) must reproduce the
    /// same first block — the soak's "same seed -> same result" acceptance
    /// criterion (handoff.md) depends on this, not just on the stream
    /// advancing.
    #[test]
    fn noise_stream_is_deterministic_from_a_fresh_engine() {
        let mut eng1 = FakeEngine::new();
        eng1.set_broadband_noise(0.5);
        eng1.reconnect_input("fake:capture_0").unwrap();
        let first = eng1.capture_block(0.01).unwrap();

        let mut eng2 = FakeEngine::new();
        eng2.set_broadband_noise(0.5);
        eng2.reconnect_input("fake:capture_0").unwrap();
        let replay = eng2.capture_block(0.01).unwrap();

        assert_eq!(first, replay, "same seed must replay identically");
    }

    #[test]
    fn broadband_noise_has_no_dominant_tone() {
        // #170: I2 stimulus needs genuine spectral content, not the old
        // `set_pink` fallback (which only ever synthesized a sine).
        let mut eng = FakeEngine::new();
        eng.set_broadband_noise(0.5);
        let s = eng.capture_block(0.5).unwrap();
        assert!(!s.is_empty());
        let rms: f64 = (s.iter().map(|x| (*x as f64).powi(2)).sum::<f64>() / s.len() as f64).sqrt();
        assert!(rms > 0.05, "expected broadband energy, rms = {rms}");
        // A single-bin Goertzel magnitude at any one frequency should be
        // small relative to total RMS energy — noise, not a tone.
        let m = goertzel_mag(&s, 48_000.0, 1_000.0) / s.len() as f64;
        assert!(
            m < rms,
            "energy concentrated at 1000 Hz looks tonal, not broadband: mag/n={m} rms={rms}"
        );
    }

    /// Ground truth (handoff: parity-completion M1.5): meas must equal
    /// `gain * ref[i - delay_samples]` sample-for-sample, for every `i`
    /// once past the initial `delay_samples` silence — checked directly
    /// against the captured arrays, not just "differs" (the way
    /// `stereo_channels_are_independent` checks the *old* stimuli).
    #[test]
    fn correlated_pair_meas_is_exact_delayed_scaled_copy_of_ref() {
        let mut eng = FakeEngine::new();
        let gain = 0.5_f64;
        let delay = 37_usize;
        eng.set_correlated_pair(gain, delay);
        eng.reconnect_input("fake:capture_0").unwrap();
        eng.add_ref_input("fake:capture_1").unwrap();

        let (meas, refch) = eng.capture_stereo(0.01).unwrap();
        assert_eq!(meas.len(), refch.len());
        assert!(
            meas.len() > delay,
            "test capture too short to exercise the delay"
        );

        for i in delay..meas.len() {
            let expected = gain as f32 * refch[i - delay];
            assert!(
                (meas[i] - expected).abs() < 1e-6,
                "meas[{i}]={} expected {expected} (= {gain} * ref[{}]={})",
                meas[i],
                i - delay,
                refch[i - delay]
            );
        }
        // Before the delay has elapsed, meas is silence (no output before
        // the DUT's input arrived).
        for (i, &m) in meas.iter().enumerate().take(delay) {
            assert_eq!(m, 0.0, "meas[{i}] should be silence before delay elapses");
        }
    }

    /// Same check across a call boundary (two consecutive `capture_stereo`
    /// calls) — the per-role position counters must keep the delay
    /// relationship correct across ticks, not just within one block.
    #[test]
    fn correlated_pair_delay_relationship_holds_across_call_boundary() {
        let mut eng = FakeEngine::new();
        let gain = 0.7_f64;
        let delay = 5_usize;
        eng.set_correlated_pair(gain, delay);
        eng.reconnect_input("fake:capture_0").unwrap();
        eng.add_ref_input("fake:capture_1").unwrap();

        let (mut meas_all, mut ref_all) = (Vec::new(), Vec::new());
        for _ in 0..5 {
            let (meas, refch) = eng.capture_stereo(0.001).unwrap();
            meas_all.extend(meas);
            ref_all.extend(refch);
        }
        assert!(meas_all.len() > delay * 2);
        for i in delay..meas_all.len() {
            let expected = gain as f32 * ref_all[i - delay];
            assert!(
                (meas_all[i] - expected).abs() < 1e-6,
                "meas_all[{i}]={} expected {expected}",
                meas_all[i]
            );
        }
    }

    /// Broadband, not a hidden tone — the ground-truth H1/coherence test
    /// (`it_snapshot.rs`) needs genuine spectral content, same reasoning
    /// as `broadband_noise_has_no_dominant_tone`.
    #[test]
    fn correlated_pair_ref_is_broadband_not_tonal() {
        let mut eng = FakeEngine::new();
        eng.set_correlated_pair(1.0, 0);
        eng.reconnect_input("fake:capture_0").unwrap();
        eng.add_ref_input("fake:capture_1").unwrap();
        let (_, refch) = eng.capture_stereo(0.5).unwrap();
        let rms: f64 =
            (refch.iter().map(|x| (*x as f64).powi(2)).sum::<f64>() / refch.len() as f64).sqrt();
        assert!(rms > 0.05, "expected broadband energy, rms = {rms}");
        let m = goertzel_mag(&refch, 48_000.0, 1_000.0) / refch.len() as f64;
        assert!(
            m < rms,
            "energy concentrated at 1000 Hz, not broadband: mag/n={m} rms={rms}"
        );
    }

    /// Determinism (needed for reproducible fixture regeneration): same
    /// seed (fixed in code) + same params ⇒ identical stream from a
    /// fresh engine, same acceptance criterion as `Stimulus::Noise`'s own
    /// `noise_stream_is_deterministic_from_a_fresh_engine`.
    #[test]
    fn correlated_pair_is_deterministic_from_a_fresh_engine() {
        let build = || {
            let mut eng = FakeEngine::new();
            eng.set_correlated_pair(0.5, 10);
            eng.reconnect_input("fake:capture_0").unwrap();
            eng.add_ref_input("fake:capture_1").unwrap();
            eng.capture_stereo(0.01).unwrap()
        };
        let (meas1, ref1) = build();
        let (meas2, ref2) = build();
        assert_eq!(meas1, meas2, "meas stream must replay identically");
        assert_eq!(ref1, ref2, "ref stream must replay identically");
    }
}
