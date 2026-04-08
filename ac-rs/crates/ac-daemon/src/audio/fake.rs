//! Fake audio engine for tests and `--fake-audio` mode.
//!
//! Generates a synthetic loopback: captures produce a clean sine at the
//! requested frequency so that `analyze()` yields plausible THD/THD+N values.

use anyhow::Result;
use std::f64::consts::PI;
use std::time::Duration;

use super::AudioEngine;

pub struct FakeEngine {
    sample_rate: u32,
    freq_hz:     f64,
    amplitude:   f64,
    xruns:       u32,
}

impl FakeEngine {
    pub fn new() -> Self {
        Self {
            sample_rate: 48_000,
            freq_hz:     1_000.0,
            amplitude:   0.0,
            xruns:       0,
        }
    }
}

impl AudioEngine for FakeEngine {
    fn start(&mut self, _output_ports: &[String], _input_port: Option<&str>) -> Result<()> {
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
        // Simulate real-time by sleeping
        std::thread::sleep(Duration::from_secs_f64(duration));

        let n    = (self.sample_rate as f64 * duration) as usize;
        let freq = self.freq_hz;
        // If silent, still produce a signal so monitor_spectrum has something to analyze
        let amp  = if self.amplitude > 0.0 { self.amplitude } else { 0.1 };
        let sr   = self.sample_rate as f64;

        let samples: Vec<f32> = (0..n)
            .map(|i| {
                let t = i as f64 / sr;
                // Matches Python FakeJackEngine: 1 % 2nd harmonic so tests see ≈1 % THD
                let sig = amp * (2.0 * PI * freq * t).sin()
                        + amp * 0.01 * (4.0 * PI * freq * t).sin();
                sig as f32
            })
            .collect();
        Ok(samples)
    }

    fn xruns(&self) -> u32 { self.xruns }

    fn playback_ports(&self) -> Vec<String> {
        (0..20).map(|i| format!("fake:playback_{i}")).collect()
    }

    fn capture_ports(&self) -> Vec<String> {
        (0..20).map(|i| format!("fake:capture_{i}")).collect()
    }
}
