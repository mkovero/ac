//! Audio backend abstraction.

pub mod fake;

#[cfg(feature = "jack-audio")]
pub mod jack_backend;

#[cfg(feature = "cpal-audio")]
pub mod cpal_backend;

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

    /// Play `samples` out the configured output and synchronously capture
    /// `samples.len() + tail` samples from the measurement input. Used by
    /// Farina swept-sine IR measurement (`sweep_ir`). The returned buffer
    /// length is `samples.len() + round(tail_s · sample_rate)`.
    ///
    /// Default returns an error — only the fake backend (used in tests and
    /// `--fake-audio` mode) implements this today; real backends need a
    /// buffer-playback path, tracked as a follow-up (see issue #75 + #78).
    fn play_and_capture(&mut self, _samples: &[f32], _tail_s: f64) -> Result<Vec<f32>> {
        anyhow::bail!(
            "play_and_capture is not implemented for the {} backend",
            self.backend_name()
        )
    }

    /// Non-blocking drain of up to `max_samples` from the capture ring,
    /// without the pre-clear that `capture_block` performs. Returns whatever
    /// has accumulated since the last call (possibly empty on backends that
    /// buffer per-period, possibly full on long gaps). Used by the
    /// `monitor_spectrum` sliding-ring path so refresh rate can be decoupled
    /// from FFT window length without losing contiguity across ticks.
    ///
    /// Default falls back to `capture_block(max_samples / sr)` — safe but
    /// clears the ring, so sr-agnostic callers still get data. JACK overrides
    /// with a true non-clearing drain.
    fn capture_available(&mut self, max_samples: usize) -> Result<Vec<f32>> {
        let sr = self.sample_rate() as f64;
        self.capture_block(max_samples as f64 / sr.max(1.0))
    }

    /// Capture two channels simultaneously: (measurement, reference).
    ///
    /// Default: both channels are the same mono signal (suitable for loopback testing).
    /// The JACK backend overrides this to capture from `in` and `in_ref` ports in sync.
    fn capture_stereo(&mut self, duration: f64) -> Result<(Vec<f32>, Vec<f32>)> {
        let ch = self.capture_block(duration)?;
        let clone = ch.clone();
        Ok((ch, clone))
    }

    /// Capture N channels simultaneously in the order they were registered
    /// via `start(..., input_port)` + subsequent `add_ref_input(..)` calls.
    /// Used by the multi-pair `transfer_stream` worker.
    ///
    /// Default falls back to `capture_stereo` and returns 2 buffers — backends
    /// that can't do >2 (e.g. CPAL) inherit this and the multi-pair handler
    /// degrades gracefully to one pair.
    fn capture_multi(&mut self, duration: f64) -> Result<Vec<Vec<f32>>> {
        let (meas, refch) = self.capture_stereo(duration)?;
        Ok(vec![meas, refch])
    }

    /// Reconnect the measurement input port without restarting the engine.
    /// Default no-op (used by fake engine; JACK backend overrides).
    fn reconnect_input(&mut self, _port: &str) -> Result<()> { Ok(()) }

    /// Connect a reference input port (second capture channel for transfer / DUT tests).
    /// Default no-op.
    fn add_ref_input(&mut self, _port: &str) -> Result<()> { Ok(()) }

    /// Discard buffered capture samples.
    /// Default no-op.
    fn flush_capture(&mut self) {}

    /// Connect our output to an additional destination port.
    /// Default no-op.
    fn connect_output(&mut self, _port: &str) -> Result<()> { Ok(()) }

    /// Disconnect our output from a destination port.
    /// Default no-op.
    fn disconnect_output(&mut self, _port: &str) {}

    /// Number of xruns since start.
    fn xruns(&self) -> u32;

    /// List of available playback port names.
    fn playback_ports(&self) -> Vec<String>;

    /// List of available capture port names.
    fn capture_ports(&self) -> Vec<String>;

    /// Whether this backend honours `reconnect_input`, `add_ref_input`,
    /// `connect_output`, `disconnect_output`. Backends that default-no-op
    /// these should return `false` so handlers that depend on routing
    /// (`probe`, `transfer`, `test_hardware`, `test_dut`) can refuse up-front
    /// instead of producing silently-wrong measurements.
    fn supports_routing(&self) -> bool { false }

    /// Human-readable backend name for error messages.
    fn backend_name(&self) -> &'static str { "unknown" }
}

/// Build an audio engine: fake → JACK (if available) → CPAL (non-Linux only) → fake.
///
/// Linux is JACK-only on purpose: CPAL on Linux means ALSA, which both
/// competes with JACK for the hardware and inherits the no-op routing
/// methods from the `AudioEngine` default impls, breaking any command
/// that relies on port routing (probe, transfer, test_hardware, test_dut
/// — see issue #27). If you actually want CPAL on Linux, run with
/// `--fake-audio` for tests or wire JACK up over ALSA the normal way.
pub fn make_engine(fake_audio: bool) -> Box<dyn AudioEngine> {
    if fake_audio {
        return Box::new(fake::FakeEngine::new());
    }

    #[cfg(feature = "jack-audio")]
    if jack_backend::JackEngine::available() {
        return Box::new(jack_backend::JackEngine::new());
    }

    #[cfg(all(feature = "cpal-audio", not(target_os = "linux")))]
    {
        return Box::new(cpal_backend::CpalEngine::new());
    }

    #[allow(unreachable_code)]
    {
        #[cfg(target_os = "linux")]
        eprintln!(
            "ac-daemon: JACK not running — falling back to fake audio. \
             Start JACK first (e.g. `jackd -d alsa -d hw:0 -r 48000 -p 1024 -n 2`); \
             CPAL/ALSA fallback is disabled on Linux on purpose."
        );
        #[cfg(not(target_os = "linux"))]
        eprintln!("ac-daemon: no audio backend available, falling back to fake audio");
        Box::new(fake::FakeEngine::new())
    }
}
