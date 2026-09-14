//! Audio backend abstraction.

pub mod fake;

pub(crate) mod drain_telemetry;

pub(crate) mod rings;

/// Capture-contiguity evidence (`handoff-capture-contiguity.md`, D3).
#[cfg(test)]
mod contiguity;

#[cfg(feature = "jack-audio")]
pub mod jack_backend;

#[cfg(feature = "cpal-audio")]
pub mod cpal_backend;

use anyhow::{bail, Result};

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
    /// Farina swept-sine IR measurement (`plot_ir`). The returned buffer
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

    /// Contiguous multi-channel capture for **streaming** consumers.
    ///
    /// Same pacing as `capture_multi` — blocks until `duration` of audio is
    /// available — but without the pre-wait `clear()`, and returns everything
    /// that has accumulated rather than exactly `duration`.
    ///
    /// Use this wherever successive blocks are concatenated into one analysis
    /// window (`transfer_stream`'s 2.5 s H1 window). Use `capture_multi` for a
    /// one-shot measurement, which genuinely wants the flush so it cannot
    /// return audio recorded before its stimulus was set. Issue #207 was
    /// `transfer_stream` using the one-shot call in a streaming loop, which
    /// spliced ~50 non-contiguous fragments into every window.
    ///
    /// Default delegates to `capture_multi`: a backend with no ring (the
    /// on-demand fake generator) has nothing to clear and is already
    /// contiguous.
    fn capture_multi_contiguous(&mut self, duration: f64) -> Result<Vec<Vec<f32>>> {
        self.capture_multi(duration)
    }

    /// Reconnect the measurement input port without restarting the engine.
    /// Default no-op (used by fake engine; JACK backend overrides).
    fn reconnect_input(&mut self, _port: &str) -> Result<()> {
        Ok(())
    }

    /// Connect a reference input port (second capture channel for transfer / DUT tests).
    /// Default no-op.
    fn add_ref_input(&mut self, _port: &str) -> Result<()> {
        Ok(())
    }

    /// Discard buffered capture samples.
    /// Default no-op.
    fn flush_capture(&mut self) {}

    /// Connect our output to an additional destination port.
    /// Default no-op.
    fn connect_output(&mut self, _port: &str) -> Result<()> {
        Ok(())
    }

    /// Disconnect our output from a destination port.
    /// Default no-op.
    fn disconnect_output(&mut self, _port: &str) {}

    /// Number of xruns since start.
    fn xruns(&self) -> u32;

    /// Cumulative samples thrown away by the `clear()` that precedes the wait
    /// in `capture_block` / `capture_stereo` / `capture_multi`.
    ///
    /// Instrumentation for the capture-contiguity investigation
    /// (`handoff-capture-contiguity.md`, D2): a nonzero value means the
    /// buffer those calls assemble is discontinuous by exactly this many
    /// samples per tick boundary. Excludes routing clears (`reconnect_input`)
    /// and explicit `flush_capture` — only per-tick discards count, or the
    /// number stops answering the question it was added for.
    ///
    /// Default 0 for backends with no ring (the on-demand fake generator),
    /// where the concept genuinely does not apply. Unlike `xruns()` — which
    /// returns a hardcoded 0 on every backend (issue #24) — a backend that
    /// *does* have a ring must report the real count, since a false zero here
    /// reads as positive evidence of contiguity.
    fn discarded_samples(&self) -> u64 {
        0
    }

    /// Per-ring occupancy sampled inside the most recent
    /// `capture_multi_contiguous`, immediately before it popped — measurement
    /// channel first, then each reference in registration order.
    ///
    /// Instrumentation for the triple-recurrence investigation (#208, D1):
    /// what the drain rate cannot distinguish on its own is a consumer that
    /// keeps up from one whose rings are already deep, and the occupancy the
    /// pop was taken from is the number that separates them. It is sampled
    /// inside the drain rather than polled from the worker because the only
    /// interesting moment is after the wait and before the pop, which the
    /// caller cannot observe.
    ///
    /// Empty for backends with no ring, and before the first contiguous drain.
    fn last_drain_occupancy(&self) -> Vec<usize> {
        Vec::new()
    }

    /// List of available playback port names.
    fn playback_ports(&self) -> Vec<String>;

    /// List of available capture port names.
    fn capture_ports(&self) -> Vec<String>;

    /// Whether this backend honours `reconnect_input`, `add_ref_input`,
    /// `connect_output`, `disconnect_output`. Backends that default-no-op
    /// these should return `false` so handlers that depend on routing
    /// (`probe`, `transfer`, `test_hardware`, `test_dut`) can refuse up-front
    /// instead of producing silently-wrong measurements.
    fn supports_routing(&self) -> bool {
        false
    }

    /// Human-readable backend name for error messages.
    fn backend_name(&self) -> &'static str {
        "unknown"
    }

    /// Period/buffer size in frames, if this backend can report one.
    ///
    /// `None` means "not applicable to this backend" (documented that way,
    /// not "unknown" — see `TauConditions::period_size`), not a value this
    /// backend simply hasn't gotten around to reporting. Queried fresh at
    /// measurement time by callers that need it, not cached at `start()` —
    /// a running JACK server's buffer size can change out from under a
    /// long-lived client (`jack_bufsize` run externally).
    fn period_size(&self) -> Option<u32> {
        None
    }

    /// Set continuous output as the sum of several simultaneous sine tones,
    /// each `(freq_hz, amplitude)`. Used by the display-truth harness (#170)
    /// to drive the I3 orientation invariant (two tones at distinct levels,
    /// check the louder one renders higher). Default delegates to
    /// `set_tone` with the first tone and drops the rest — only the fake
    /// backend (test/`--fake-audio` use) needs true multi-tone synthesis;
    /// real backends generating a single physical output tone is an
    /// acceptable degradation since routing-dependent commands already gate
    /// on `supports_routing`.
    fn set_tone_pair(&mut self, tones: &[(f64, f64)]) {
        if let Some(&(freq, amp)) = tones.first() {
            self.set_tone(freq, amp);
        }
    }

    /// Set continuous calibrated broadband noise at the given peak
    /// amplitude (0..1 full-scale). Used by the display-truth harness
    /// (#170) to drive the I2 flat-noise continuity invariant, which needs
    /// genuine spectral content (not a single tone) to check for
    /// band-boundary steps. Default delegates to `set_pink`.
    fn set_broadband_noise(&mut self, amplitude: f64) {
        self.set_pink(amplitude);
    }

    /// Set a correlated-pair stimulus (handoff: parity-completion M1.5):
    /// the reference-role port carries a seeded deterministic broadband
    /// source; the measurement-role port carries the *same* source scaled
    /// by `gain` and delayed by `delay_samples` — a fake DUT with known
    /// ground truth (`|H1| = gain`, coherence ≈ 1). Default no-op — only
    /// the fake backend (test / `--fake-audio` use) implements this; real
    /// backends have no "known ground truth" concept to synthesize.
    fn set_correlated_pair(&mut self, _gain: f64, _delay_samples: usize) {}

    /// Route fake capture through a real ring driven by a synthetic clock,
    /// with `process_secs` of modelled per-tick consumer processing time,
    /// `n_refs` reference channels, and a producer granularity of `period`
    /// samples (the backend's period/quantum — 1024 on the verified rig).
    ///
    /// Instrumentation for the capture-contiguity investigation
    /// (`handoff-capture-contiguity.md`, D1). The default fake backend
    /// synthesises samples on demand and has no ring, so it cannot reproduce
    /// a splice, drop, or backlog defect at all — which is why this class of
    /// bug has never reproduced headlessly. Opt-in, so the default fake path
    /// stays byte-identical.
    ///
    /// Default no-op, same as `set_correlated_pair`: real backends already
    /// have rings, fed by their own hardware clock.
    fn enable_ring_mode(&mut self, _process_secs: f64, _n_refs: usize, _period: usize) {}
}

/// Resolve the configured backend requirement without opening or starting an
/// engine. The returned tuple is `(required, available, selected)`; `selected`
/// is absent when the requirement cannot be met.
pub fn backend_status(fake_audio: bool, required: Option<&str>) -> (String, bool, Option<String>) {
    let required = if fake_audio {
        "fake"
    } else {
        required.unwrap_or(if cfg!(target_os = "linux") {
            "jack"
        } else {
            "cpal"
        })
    };
    let canonical = if required == "sounddevice" {
        "cpal"
    } else {
        required
    };
    let available = match canonical {
        "fake" => true,
        "jack" => {
            #[cfg(feature = "jack-audio")]
            {
                jack_backend::JackEngine::available()
            }
            #[cfg(not(feature = "jack-audio"))]
            {
                false
            }
        }
        "cpal" => cfg!(all(feature = "cpal-audio", not(target_os = "linux"))),
        _ => false,
    };
    (
        canonical.to_string(),
        available,
        available.then(|| canonical.to_string()),
    )
}

/// Build the explicitly requested audio engine. Real-audio requirements fail
/// closed: fake audio is selected only by `--fake-audio` or `backend: fake`.
///
/// Linux is JACK-only on purpose: CPAL on Linux means ALSA, which both
/// competes with JACK for the hardware and inherits the no-op routing
/// methods from the `AudioEngine` default impls, breaking any command
/// that relies on port routing (probe, transfer, test_hardware, test_dut
/// — see issue #27). If you actually want CPAL on Linux, run with
/// `--fake-audio` for tests or wire JACK up over ALSA the normal way.
pub fn make_engine(fake_audio: bool, required: Option<&str>) -> Result<Box<dyn AudioEngine>> {
    let (required, available, _) = backend_status(fake_audio, required);
    if !available {
        bail!("audio backend unavailable — required {required}; measurement not started");
    }
    match required.as_str() {
        "fake" => Ok(Box::new(fake::FakeEngine::new())),
        "jack" => {
            #[cfg(feature = "jack-audio")]
            {
                Ok(Box::new(jack_backend::JackEngine::new()))
            }
            #[cfg(not(feature = "jack-audio"))]
            unreachable!("availability check rejects an uncompiled JACK backend")
        }
        "cpal" => {
            #[cfg(all(feature = "cpal-audio", not(target_os = "linux")))]
            {
                Ok(Box::new(cpal_backend::CpalEngine::new()))
            }
            #[cfg(any(not(feature = "cpal-audio"), target_os = "linux"))]
            unreachable!("availability check rejects an uncompiled CPAL backend")
        }
        _ => bail!("invalid audio backend requirement {required:?}"),
    }
}

#[cfg(test)]
mod backend_selection_tests {
    use super::*;

    #[test]
    fn explicit_fake_is_available_and_constructed() {
        let (required, available, selected) = backend_status(false, Some("fake"));
        assert_eq!(required, "fake");
        assert!(available);
        assert_eq!(selected.as_deref(), Some("fake"));
        assert_eq!(
            make_engine(false, Some("fake")).unwrap().backend_name(),
            "fake"
        );
    }

    #[cfg(any(not(feature = "cpal-audio"), target_os = "linux"))]
    #[test]
    fn unavailable_cpal_backend_fails_closed() {
        let (required, available, selected) = backend_status(false, Some("cpal"));
        assert_eq!(required, "cpal");
        assert!(!available);
        assert_eq!(selected, None);

        let err = match make_engine(false, Some("cpal")) {
            Ok(_) => panic!("unavailable CPAL backend unexpectedly constructed"),
            Err(e) => e.to_string(),
        };
        assert!(err.contains("audio backend unavailable"), "{err}");
        assert!(err.contains("measurement not started"), "{err}");
    }
}
