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

pub struct FakeEngine {
    sample_rate:  u32,
    freq_hz:      f64,
    amplitude:    f64,
    xruns:        u32,
    output_ports: Vec<String>,
    input_port:   Option<String>,
    ref_port:     Option<String>,
}

impl FakeEngine {
    pub fn new() -> Self {
        Self {
            sample_rate:  48_000,
            freq_hz:      1_000.0,
            amplitude:    0.0,
            xruns:        0,
            output_ports: Vec::new(),
            input_port:   None,
            ref_port:     None,
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

    fn effective_freq(&self, port: Option<&str>) -> f64 {
        let ch = port.map(Self::channel_index).unwrap_or(0);
        self.freq_hz + ch as f64 * CHANNEL_OFFSET_HZ
    }

    /// Generate `duration` seconds of synthetic signal at the given frequency.
    fn make_samples_at(&self, freq: f64, duration: f64) -> Vec<f32> {
        let n   = (self.sample_rate as f64 * duration) as usize;
        let amp = if self.amplitude > 0.0 { self.amplitude } else { 0.1 };
        let sr  = self.sample_rate as f64;
        (0..n).map(|i| {
            let t = i as f64 / sr;
            let sig = amp * (2.0 * PI * freq * t).sin()
                    + amp * 0.01 * (4.0 * PI * freq * t).sin();
            sig as f32
        }).collect()
    }
}

impl AudioEngine for FakeEngine {
    fn start(&mut self, output_ports: &[String], input_port: Option<&str>) -> Result<()> {
        self.output_ports = output_ports.to_vec();
        self.input_port   = input_port.map(str::to_string);
        Ok(())
    }

    fn stop(&mut self) {}

    fn sample_rate(&self) -> u32 { self.sample_rate }

    fn set_tone(&mut self, freq_hz: f64, amplitude: f64) {
        self.freq_hz  = freq_hz;
        self.amplitude = amplitude;
    }

    fn set_pink(&mut self, amplitude: f64) {
        self.amplitude = amplitude;
    }

    fn set_silence(&mut self) {
        self.amplitude = 0.0;
    }

    fn capture_block(&mut self, duration: f64) -> Result<Vec<f32>> {
        std::thread::sleep(Duration::from_secs_f64(duration));
        let freq = self.effective_freq(self.input_port.as_deref());
        Ok(self.make_samples_at(freq, duration))
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
        let meas_freq = self.effective_freq(self.input_port.as_deref());
        // If no explicit ref_port, reference mirrors the generator (channel 0).
        let ref_freq  = self.effective_freq(self.ref_port.as_deref());
        let meas  = self.make_samples_at(meas_freq, duration);
        let refch = self.make_samples_at(ref_freq,  duration);
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

    fn xruns(&self) -> u32 { self.xruns }

    fn supports_routing(&self) -> bool { true }
    fn backend_name(&self) -> &'static str { "fake" }

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
        assert_eq!(FakeEngine::channel_index("fake:capture_0"),  0);
        assert_eq!(FakeEngine::channel_index("fake:capture_7"),  7);
        assert_eq!(FakeEngine::channel_index("fake:capture_19"), 19);
        assert_eq!(FakeEngine::channel_index("garbage"),         0);
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
        let diff: f32 = bufs[0].iter().zip(&bufs[1]).map(|(a, b)| (a - b).abs()).sum();
        assert!(diff > 0.0, "multi channels should differ between meas and ref");
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
}
