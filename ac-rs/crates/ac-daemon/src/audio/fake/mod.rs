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
//!
//! # What this backend must never do
//!
//! **Return a plausible wrong value rather than failing.** A refusal or a
//! missing buffer is visible and gets fixed; a number that looks like a
//! measurement is not, and it propagates. Offline experiments are built on
//! fake sessions here — the gate scoring, the parity fixtures, the frame
//! cadence work — so a quantity this backend synthesises incorrectly is
//! inherited by every conclusion drawn from them, with nothing to mark it.
//!
//! That is a harder rule than "model what the hardware does", and it is the
//! one that decides the design when the two conflict. #254's fix could have
//! returned one buffer per port while leaving a single shared measurement
//! read cursor: three buffers would have arrived, the session would have
//! published, and the second measurement channel's delay would have been an
//! artefact of call order that no presence check could distinguish from a real
//! one (`correlated_meas_pos`). Prefer the shape that cannot produce a wrong
//! answer over the shape that usually produces a right one, and where a
//! quantity has a configured ground truth, pin it in a test rather than
//! asserting that it arrived.

mod hooks;
mod ring_mode;
mod stimulus;

use anyhow::Result;
use std::time::Duration;

use self::hooks::{next_loopback_delay_samples, period_size_override};
use self::ring_mode::{FakeRings, RingDrain};
use self::stimulus::{Stimulus, StimulusGen, Synth};
use super::AudioEngine;

pub struct FakeEngine {
    sample_rate: u32,
    /// Everything the sample generator reads or advances — the configured
    /// stimulus plus its per-channel positions. Its own field so ring mode
    /// can borrow it alongside the ring producers; see [`StimulusGen`].
    gen: StimulusGen,
    xruns: u32,
    output_ports: Vec<String>,
    input_port: Option<String>,
    /// Every port registered through `add_ref_input`, in registration order,
    /// deduplicated. `capture_multi` returns one buffer per entry after the
    /// measurement buffer, which is what the handler's `rings` expects:
    /// `unique_ports[0]` is the measurement channel and `unique_ports[1..]`
    /// arrive here in the same order.
    ///
    /// It was a single `Option<String>`, last-write-wins, which is #254: a
    /// session over three distinct channels registered two ref inputs, kept
    /// only the second, and still captured two buffers — so the third ring
    /// never filled and the session warmed up forever. The **first** entry is
    /// the reference leg proper ([`Self::ref_port`]); with two channels there
    /// is exactly one entry and every path below is unchanged.
    ref_ports: Vec<String>,
    /// `Some` only when the caller opted into ring-backed capture. `None` —
    /// the default — leaves every capture path byte-identical to before.
    ring: Option<FakeRings>,
}

impl FakeEngine {
    pub fn new() -> Self {
        Self {
            sample_rate: 48_000,
            gen: StimulusGen::default(),
            xruns: 0,
            output_ports: Vec::new(),
            input_port: None,
            ref_ports: Vec::new(),
            ring: None,
        }
    }

    /// Test-only: set the rate the engine reports and synthesises at.
    ///
    /// `FakeEngine::new()` hardcodes 48 kHz. Any test that models a specific
    /// rig must set this, or it will synthesise at 48 kHz while analysing at
    /// the assumed rate and every frequency will read wrong by that ratio —
    /// which is exactly the mistake `audio/contiguity.rs` made before this
    /// existed (a 96 kHz model whose tones came out an octave high).
    #[cfg(test)]
    pub fn set_sample_rate(&mut self, sr: u32) {
        self.sample_rate = sr;
    }

    /// The reference leg proper: the **first** port registered through
    /// `add_ref_input`. See [`Self::ref_ports`] for why first rather than
    /// last, and why the distinction only arises above two channels.
    fn ref_port(&self) -> Option<&str> {
        self.ref_ports.first().map(String::as_str)
    }

    /// Borrow the generator with the context a synthesis call needs.
    ///
    /// Takes `&mut self`, so a caller that also needs a port name must clone
    /// it first — every capture path below does.
    fn synth(&mut self) -> Synth<'_> {
        let FakeEngine {
            gen,
            ref_ports,
            sample_rate,
            ..
        } = self;
        Synth {
            gen,
            sample_rate: *sample_rate,
            ref_port: ref_ports.first().map(String::as_str),
        }
    }

    /// Port names for the ring-mode channels, measurement first then refs, in
    /// the order `capture_multi` returns them.
    ///
    /// **Ring mode still reads one channel for every ref ring** — the first
    /// registered ref — which is enough for the contiguity question, since
    /// that is per-channel and not about routing. The off-ring path no longer
    /// works this way (#254: one buffer per registered port), so the two
    /// differ, and **ring mode is not a way to rehearse a multi-channel
    /// session**: it models N refs on one channel, while the case that
    /// mattered is two distinct *measurement* channels sharing a reference.
    /// That remaining blind spot is #204.
    fn ring_ports(&self) -> Vec<Option<String>> {
        let n_refs = self.ring.as_ref().map(FakeRings::n_refs).unwrap_or(0);
        let mut ports = Vec::with_capacity(1 + n_refs);
        ports.push(self.input_port.clone());
        for _ in 0..n_refs {
            ports.push(self.ref_port().map(str::to_string));
        }
        ports
    }

    /// Route one capture of `n` samples through ring mode, or `None` when
    /// ring mode is off and the caller should synthesise on demand.
    ///
    /// `n` is passed rather than derived from `duration`: `capture_available`
    /// is given a sample count, and a round trip through seconds and back
    /// need not land on the same integer.
    fn ring_capture(
        &mut self,
        n: usize,
        duration: f64,
        kind: RingDrain,
    ) -> Option<Result<Vec<Vec<f32>>>> {
        self.ring.as_ref()?;
        let ports = self.ring_ports();
        let FakeEngine {
            gen,
            ref_ports,
            sample_rate,
            ring,
            ..
        } = self;
        let mut synth = Synth {
            gen,
            sample_rate: *sample_rate,
            ref_port: ref_ports.first().map(String::as_str),
        };
        let rings = ring.as_mut()?;
        Some(rings.drain(&mut synth, &ports, n, duration, kind))
    }

    /// Samples in `duration` seconds at the engine's rate.
    fn samples_in(&self, duration: f64) -> usize {
        (self.sample_rate as f64 * duration) as usize
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

    fn period_size(&self) -> Option<u32> {
        period_size_override()
    }

    fn set_tone(&mut self, freq_hz: f64, amplitude: f64) {
        self.gen.set(Stimulus::Tones(vec![(freq_hz, amplitude)]));
    }

    /// The fake's pink noise *is* its broadband noise — there is no spectral
    /// shaping here (see `Stimulus::Noise`), so the trait's default
    /// `set_broadband_noise`, which delegates to this, is already correct and
    /// the fake does not override it.
    fn set_pink(&mut self, amplitude: f64) {
        self.gen.set(Stimulus::Noise(amplitude));
    }

    fn set_silence(&mut self) {
        self.gen.set(Stimulus::Tones(vec![(1_000.0, 0.0)]));
    }

    fn set_tone_pair(&mut self, tones: &[(f64, f64)]) {
        self.gen.set(Stimulus::Tones(tones.to_vec()));
    }

    fn set_correlated_pair(&mut self, gain: f64, delay_samples: usize) {
        self.gen.set_correlated_pair(gain, delay_samples);
    }

    fn capture_block(&mut self, duration: f64) -> Result<Vec<f32>> {
        let n = self.samples_in(duration);
        if let Some(out) = self.ring_capture(n, duration, RingDrain::Block) {
            return Ok(out?.into_iter().next().unwrap_or_default());
        }
        std::thread::sleep(Duration::from_secs_f64(duration));
        let port = self.input_port.clone();
        Ok(self.synth().block(port.as_deref(), duration, 0))
    }

    /// Non-clearing drain. In ring mode this is the *contiguous* control arm:
    /// identical to `capture_block` except for the absent `clear()`, which is
    /// precisely the variable H1 says the defect turns on.
    fn capture_available(&mut self, max_samples: usize) -> Result<Vec<f32>> {
        let sr = self.sample_rate() as f64;
        let duration = max_samples as f64 / sr.max(1.0);
        if let Some(out) = self.ring_capture(max_samples, duration, RingDrain::Available) {
            return Ok(out?.into_iter().next().unwrap_or_default());
        }
        self.capture_block(duration)
    }

    /// Fake loopback: returns `samples` delayed by a fixed number of
    /// samples ([`hooks::next_loopback_delay_samples`]), padded with trailing
    /// zeros to `samples.len() + tail` total length. Used by the
    /// `plot_ir` integration test to verify the deconvolved linear IR
    /// peaks at the expected offset.
    fn play_and_capture(&mut self, samples: &[f32], tail_s: f64) -> Result<Vec<f32>> {
        let delay_samples = next_loopback_delay_samples();
        let tail = (tail_s * self.sample_rate as f64).round() as usize;
        let total = samples.len() + tail;
        let mut out = vec![0.0f32; total];
        for (i, &s) in samples.iter().enumerate() {
            let j = i + delay_samples;
            if j < total {
                out[j] = s;
            }
        }
        Ok(out)
    }

    fn capture_stereo(&mut self, duration: f64) -> Result<(Vec<f32>, Vec<f32>)> {
        let n = self.samples_in(duration);
        if let Some(out) = self.ring_capture(n, duration, RingDrain::Stereo) {
            let mut out = out?.into_iter();
            let meas = out.next().unwrap_or_default();
            let refch = out.next().unwrap_or_default();
            return Ok((meas, refch));
        }
        std::thread::sleep(Duration::from_secs_f64(duration));
        // If no explicit ref_port, reference mirrors the generator (channel 0).
        let in_port = self.input_port.clone();
        let ref_port = self.ref_port().map(str::to_string);
        let meas = self.synth().block(in_port.as_deref(), duration, 0);
        let refch = self.synth().block(ref_port.as_deref(), duration, 0);
        Ok((meas, refch))
    }

    /// One buffer per capture channel this session registered: the
    /// measurement port first, then every `add_ref_input` port in
    /// registration order — the order the handler's `rings` are indexed in.
    ///
    /// #254: this used to end at `vec![meas, refch]` regardless of how many
    /// ports had been registered, so a session over three distinct channels
    /// got two buffers, its third ring never reached one Welch segment, and
    /// the warmup gate skipped every tick for the life of the session. `ok:
    /// true`, then nothing, forever.
    ///
    /// With two channels there is exactly one ref port and this is
    /// byte-identical to the old path — `capture_stereo` still produces both
    /// buffers, including the `CorrelatedPair` role dispatch, which is why
    /// it is still called rather than replaced by a loop.
    fn capture_multi(&mut self, duration: f64) -> Result<Vec<Vec<f32>>> {
        let n = self.samples_in(duration);
        if let Some(out) = self.ring_capture(n, duration, RingDrain::Multi) {
            return out;
        }
        let extra: Vec<String> = self.ref_ports.iter().skip(1).cloned().collect();
        let (meas, refch) = self.capture_stereo(duration)?;
        let mut out = Vec::with_capacity(2 + extra.len());
        out.push(meas);
        out.push(refch);
        for port in &extra {
            // `capture_stereo` already slept for `duration`; these are the
            // same wall-clock tick, so they must not sleep again.
            out.push(self.synth().block(Some(port.as_str()), duration, 0));
        }
        Ok(out)
    }

    fn capture_multi_contiguous(&mut self, duration: f64) -> Result<Vec<Vec<f32>>> {
        let n = self.samples_in(duration);
        if let Some(out) = self.ring_capture(n, duration, RingDrain::MultiContiguous) {
            return out;
        }
        // No ring to splice: the on-demand generator is already contiguous.
        self.capture_multi(duration)
    }

    fn discarded_samples(&self) -> u64 {
        self.ring
            .as_ref()
            .map(FakeRings::discarded_samples)
            .unwrap_or(0)
    }

    /// Per-ring occupancy from the last contiguous drain (#208 D1).
    ///
    /// Ring mode exists so ring-shaped capture defects are reproducible
    /// without hardware; inheriting the trait's empty default here left the
    /// telemetry blind in exactly that mode, so a test could read `occ=[]`
    /// and conclude nothing was wrong. Empty off ring mode, where there is
    /// genuinely no ring to report.
    fn last_drain_occupancy(&self) -> Vec<usize> {
        self.ring
            .as_ref()
            .map(FakeRings::last_drain_occupancy)
            .unwrap_or_default()
    }

    fn enable_ring_mode(&mut self, process_secs: f64, n_refs: usize, period: usize) {
        self.ring = Some(FakeRings::new(process_secs, n_refs, period));
    }

    fn reconnect_input(&mut self, port: &str) -> Result<()> {
        self.input_port = Some(port.to_string());
        Ok(())
    }

    fn add_ref_input(&mut self, port: &str) -> Result<()> {
        if !self.ref_ports.iter().any(|p| p == port) {
            self.ref_ports.push(port.to_string());
        }
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
mod tests;
