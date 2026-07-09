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
/// harness runs are reproducible.
#[derive(Clone)]
enum Stimulus {
    Tones(Vec<(f64, f64)>),
    Noise(f64),
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
            Stimulus::Noise(_) => offset,
        }
    }

    /// Generate `duration` seconds of synthetic signal for `port`'s channel
    /// (frequency-shifted per `CHANNEL_OFFSET_HZ`, same as pre-#170).
    fn make_samples_for(&self, port: Option<&str>, duration: f64) -> Vec<f32> {
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
                let mut state: u64 = 0x9E3779B97F4A7C15 ^ offset.to_bits();
                (0..n)
                    .map(|_| {
                        state = state
                            .wrapping_mul(6364136223846793005)
                            .wrapping_add(1442695040888963407);
                        let u = ((state >> 40) as f64 / (1u64 << 24) as f64) * 2.0 - 1.0;
                        (amp * u) as f32
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

    fn capture_block(&mut self, duration: f64) -> Result<Vec<f32>> {
        std::thread::sleep(Duration::from_secs_f64(duration));
        Ok(self.make_samples_for(self.input_port.as_deref(), duration))
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
        let meas = self.make_samples_for(self.input_port.as_deref(), duration);
        let refch = self.make_samples_for(self.ref_port.as_deref(), duration);
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
}
