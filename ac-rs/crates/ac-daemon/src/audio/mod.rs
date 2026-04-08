//! Audio backend abstraction.

pub mod fake;

#[cfg(feature = "jack-audio")]
pub mod jack_backend;

use anyhow::Result;

/// Minimal trait for audio playback + capture, matching Python's JackEngine duck-type contract.
pub trait AudioEngine: Send + 'static {
    /// Start the engine, connecting to the given port names.
    fn start(&mut self, output_ports: &[String], input_port: Option<&str>) -> Result<()>;

    /// Stop and disconnect.
    fn stop(&mut self);

    /// Sample rate in Hz.
    fn sample_rate(&self) -> u32;

    /// Set continuous sine tone output.
    fn set_tone(&mut self, freq_hz: f64, amplitude: f64);

    /// Set continuous pink noise output.
    fn set_pink(&mut self, amplitude: f64);

    /// Silence output.
    fn set_silence(&mut self);

    /// Capture audio for `duration` seconds and return the samples.
    fn capture_block(&mut self, duration: f64) -> Result<Vec<f32>>;

    /// Number of xruns since start.
    fn xruns(&self) -> u32;

    /// List of available playback port names.
    fn playback_ports(&self) -> Vec<String>;

    /// List of available capture port names.
    fn capture_ports(&self) -> Vec<String>;
}

/// Build an audio engine based on the `fake_audio` flag.
pub fn make_engine(fake_audio: bool) -> Box<dyn AudioEngine> {
    if fake_audio {
        return Box::new(fake::FakeEngine::new());
    }

    #[cfg(feature = "jack-audio")]
    {
        Box::new(jack_backend::JackEngine::new())
    }

    #[cfg(not(feature = "jack-audio"))]
    {
        eprintln!("ac-daemon: compiled without JACK support, falling back to fake audio");
        Box::new(fake::FakeEngine::new())
    }
}
