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
use ringbuf::traits::Split;
use ringbuf::HeapRb;
use std::collections::HashMap;
use std::f64::consts::PI;
use std::time::Duration;

use self::hooks::{next_loopback_delay_samples, period_size_override};
use self::ring_mode::{push_block, FakeRings, RingDrain, FAKE_RING_CAPACITY};
use self::stimulus::{correlated_source_at, Stimulus, CHANNEL_OFFSET_HZ, CORRELATED_PAIR_SEED};
use super::rings::CaptureRings;
use super::AudioEngine;

pub struct FakeEngine {
    sample_rate: u32,
    stimulus: Stimulus,
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
    /// Per-channel-offset LCG state for `Stimulus::Noise`, keyed on the
    /// offset's bit pattern (one entry per distinct channel). Persisted
    /// across `capture_block`/`capture_stereo` calls so a soak driving the
    /// I5 display-truth invariant sees a genuine continuing
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
    /// `Some` only when the caller opted into ring-backed capture. `None` —
    /// the default — leaves every capture path byte-identical to before.
    ring: Option<FakeRings>,
}

impl FakeEngine {
    pub fn new() -> Self {
        Self {
            sample_rate: 48_000,
            stimulus: Stimulus::default(),
            xruns: 0,
            output_ports: Vec::new(),
            input_port: None,
            ref_ports: Vec::new(),
            noise_state: HashMap::new(),
            correlated_ref_pos: 0,
            correlated_meas_pos: HashMap::new(),
            ring: None,
        }
    }

    /// Enable ring-backed capture with `process_secs` of per-tick consumer
    /// processing time. Opt-in, fake-only; see [`FakeRings`].
    ///
    /// `n_refs` reference rings are allocated up front rather than on
    /// `add_ref_input`, because the fake has no RT handler to hand producers
    /// to and the transfer worker registers its refs before the first tick.
    fn enable_ring_mode_inner(&mut self, process_secs: f64, n_refs: usize, period: usize) {
        let (meas_prod, meas_cons) = HeapRb::<f32>::new(FAKE_RING_CAPACITY).split();
        let mut rings = CaptureRings::new();
        rings.set_meas(meas_cons);

        let mut ref_prods = Vec::with_capacity(n_refs);
        rings.reserve_refs(n_refs);
        for _ in 0..n_refs {
            let (p, c) = HeapRb::<f32>::new(FAKE_RING_CAPACITY).split();
            ref_prods.push(p);
            rings.push_ref(c);
        }

        self.ring = Some(FakeRings {
            rings,
            meas_prod,
            ref_prods,
            process_secs: process_secs.max(0.0),
            tone_pos: 0,
            period: period.max(1),
            residue: 0,
        });
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
        let n_refs = self.ring.as_ref().map(|r| r.ref_prods.len()).unwrap_or(0);
        let mut ports = Vec::with_capacity(1 + n_refs);
        ports.push(self.input_port.clone());
        for _ in 0..n_refs {
            ports.push(self.ref_port().map(str::to_string));
        }
        ports
    }

    /// Push `periods` whole periods of synthesised audio into the rings.
    ///
    /// The producer only ever moves in whole periods — that is the property
    /// that decides which stimulus frequencies expose the splice at all (see
    /// [`FakeRings::period`]). Samples are generated *before* the producers
    /// are borrowed so the generator's `&mut self` and the ring's `&mut` never
    /// overlap.
    fn ring_push_periods(&mut self, periods: usize) {
        if periods == 0 || self.ring.is_none() {
            return;
        }
        let period = self.ring.as_ref().map(|r| r.period).unwrap_or(1);
        let n = periods * period;
        let ports = self.ring_ports();
        let duration = n as f64 / self.sample_rate as f64;
        let tone_pos = self.ring.as_ref().map(|r| r.tone_pos).unwrap_or(0);

        let blocks: Vec<Vec<f32>> = ports
            .iter()
            .map(|p| self.make_samples_from(p.as_deref(), duration, tone_pos))
            .collect();

        let Some(ref mut r) = self.ring else { return };
        r.tone_pos += n as u64;
        // Overrun drops the newest samples, matching the JACK producer's
        // bounded-memory behaviour (see `jack_backend`'s module docs).
        push_block(&mut r.meas_prod, &blocks[0]);
        for (prod, block) in r.ref_prods.iter_mut().zip(blocks.iter().skip(1)) {
            push_block(prod, block);
        }
    }

    /// Model the consumer's processing time: the ring keeps filling while the
    /// caller works on the block it just popped. Whatever accrues here is
    /// exactly what the *next* tick's `clear()` discards — the splice.
    ///
    /// Accrued time is banked in `residue` and materialises only as whole
    /// periods, so a gap shorter than one period does not produce a small
    /// discard every tick — it produces a *whole period* discarded on some
    /// ticks and nothing on the others, which is what the hardware does.
    fn ring_charge_processing(&mut self) {
        let Some(r) = self.ring.as_ref() else { return };
        let period = r.period;
        let gap = (self.sample_rate as f64 * r.process_secs) as usize;
        let banked = r.residue + gap;
        let periods = banked / period;
        if let Some(r) = self.ring.as_mut() {
            r.residue = banked % period;
        }
        self.ring_push_periods(periods);
    }

    /// One ring-mode capture tick.
    ///
    /// Timeline, matching what a real consumer does against a live ring:
    ///
    /// 1. the requested drain runs — `clear()` (for every variant except
    ///    `Available`) throws away whatever accrued during step 3 of the
    ///    *previous* tick, then the wait synthesises this tick's `n` samples
    ///    and they are popped;
    /// 2. the caller gets its block;
    /// 3. the caller processes it, during which `process_secs` more audio
    ///    accrues — charged here so the next tick's `clear()` has something
    ///    real to discard.
    ///
    /// Step 3 is what makes this reproduce anything. A virtual clock that
    /// advanced only inside the wait would leave the ring empty at every
    /// `clear()`, discarding nothing, and the mode would model no defect at
    /// all.
    ///
    /// Samples are pre-generated before the producers are borrowed, so the
    /// generator's `&mut self` never overlaps the ring borrow — that is why
    /// the wait closure only needs the producers.
    fn ring_drain(&mut self, n: usize, duration: f64, kind: RingDrain) -> Result<Vec<Vec<f32>>> {
        let Some(r) = self.ring.as_ref() else {
            return Ok(Vec::new());
        };
        let period = r.period;
        // The clearing drains empty the ring first, so plan the fill from what
        // will actually be there when the wait runs. Whole periods only — a
        // producer that could deliver a partial period would model the wrong
        // machine (see `FakeRings::period`).
        let occupied_at_wait = match kind {
            RingDrain::Available => r.rings.occupied(),
            _ => 0,
        };
        let shortfall = n.saturating_sub(occupied_at_wait);
        let fill_periods = shortfall.div_ceil(period);
        let fill = fill_periods * period;

        let ports = self.ring_ports();
        let tone_pos = self.ring.as_ref().map(|r| r.tone_pos).unwrap_or(0);
        let wait_duration = fill as f64 / self.sample_rate as f64;
        let blocks: Vec<Vec<f32>> = ports
            .iter()
            .map(|p| self.make_samples_from(p.as_deref(), wait_duration, tone_pos))
            .collect();

        let Some(ref mut r) = self.ring else {
            return Ok(Vec::new());
        };
        r.tone_pos += fill as u64;

        // Destructured so `rings` and the producers are disjoint borrows:
        // the drain owns the ordering, the waiter owns the fill.
        let FakeRings {
            rings,
            meas_prod,
            ref_prods,
            ..
        } = r;
        let mut wait = |_: &CaptureRings, _n: usize, _d: f64| {
            push_block(meas_prod, &blocks[0]);
            for (prod, block) in ref_prods.iter_mut().zip(blocks.iter().skip(1)) {
                push_block(prod, block);
            }
            Ok(())
        };

        let out = match kind {
            RingDrain::Block => rings.capture_block(n, duration, &mut wait).map(|b| vec![b]),
            RingDrain::Available => {
                wait(&*rings, n, duration)?;
                Ok(vec![rings.capture_available(n)])
            }
            RingDrain::Stereo => rings
                .capture_stereo(n, duration, &mut wait)
                .map(|(m, r)| vec![m, r]),
            RingDrain::Multi => rings.capture_multi(n, duration, &mut wait),
            RingDrain::MultiContiguous => rings.capture_multi_contiguous(n, duration, &mut wait),
        }?;

        self.ring_charge_processing();
        Ok(out)
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
        self.make_samples_from(port, duration, 0)
    }

    /// The reference leg proper: the **first** port registered through
    /// `add_ref_input`. See [`Self::ref_ports`] for why first rather than
    /// last, and why the distinction only arises above two channels.
    fn ref_port(&self) -> Option<&str> {
        self.ref_ports.first().map(String::as_str)
    }

    /// As `make_samples_for`, but with tone phase advanced to absolute sample
    /// position `tone_start` instead of restarting at `t = 0`.
    ///
    /// Only ring mode passes a nonzero `tone_start`; the on-demand path calls
    /// this with `0`, which reduces to the original expression exactly, so
    /// default output stays byte-identical. `Noise` and `CorrelatedPair`
    /// already track their own absolute position and are unaffected.
    fn make_samples_from(
        &mut self,
        port: Option<&str>,
        duration: f64,
        tone_start: u64,
    ) -> Vec<f32> {
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
                let is_ref = port.is_some() && port == self.ref_port();
                let start_pos = if is_ref {
                    let p = self.correlated_ref_pos;
                    self.correlated_ref_pos += n as u64;
                    p
                } else {
                    let slot = self
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
        self.correlated_meas_pos.clear();
    }

    fn capture_block(&mut self, duration: f64) -> Result<Vec<f32>> {
        if self.ring.is_some() {
            let n = (self.sample_rate as f64 * duration) as usize;
            let out = self.ring_drain(n, duration, RingDrain::Block)?;
            return Ok(out.into_iter().next().unwrap_or_default());
        }
        std::thread::sleep(Duration::from_secs_f64(duration));
        let port = self.input_port.clone();
        Ok(self.make_samples_for(port.as_deref(), duration))
    }

    /// Non-clearing drain. In ring mode this is the *contiguous* control arm:
    /// identical to `capture_block` except for the absent `clear()`, which is
    /// precisely the variable H1 says the defect turns on.
    fn capture_available(&mut self, max_samples: usize) -> Result<Vec<f32>> {
        if self.ring.is_some() {
            let duration = max_samples as f64 / self.sample_rate as f64;
            let out = self.ring_drain(max_samples, duration, RingDrain::Available)?;
            return Ok(out.into_iter().next().unwrap_or_default());
        }
        let sr = self.sample_rate() as f64;
        self.capture_block(max_samples as f64 / sr.max(1.0))
    }

    /// Fake loopback: returns `samples` delayed by a fixed number of
    /// samples (`FAKE_LOOPBACK_DELAY_SAMPLES`), padded with trailing
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
        if self.ring.is_some() {
            let n = (self.sample_rate as f64 * duration) as usize;
            let mut out = self.ring_drain(n, duration, RingDrain::Stereo)?.into_iter();
            let meas = out.next().unwrap_or_default();
            let refch = out.next().unwrap_or_default();
            return Ok((meas, refch));
        }
        std::thread::sleep(Duration::from_secs_f64(duration));
        // If no explicit ref_port, reference mirrors the generator (channel 0).
        let in_port = self.input_port.clone();
        let ref_port = self.ref_port().map(str::to_string);
        let meas = self.make_samples_for(in_port.as_deref(), duration);
        let refch = self.make_samples_for(ref_port.as_deref(), duration);
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
        if self.ring.is_some() {
            let n = (self.sample_rate as f64 * duration) as usize;
            return self.ring_drain(n, duration, RingDrain::Multi);
        }
        let extra: Vec<String> = self.ref_ports.iter().skip(1).cloned().collect();
        let (meas, refch) = self.capture_stereo(duration)?;
        let mut out = Vec::with_capacity(2 + extra.len());
        out.push(meas);
        out.push(refch);
        for port in &extra {
            // `capture_stereo` already slept for `duration`; these are the
            // same wall-clock tick, so they must not sleep again.
            out.push(self.make_samples_for(Some(port.as_str()), duration));
        }
        Ok(out)
    }

    fn capture_multi_contiguous(&mut self, duration: f64) -> Result<Vec<Vec<f32>>> {
        if self.ring.is_some() {
            let n = (self.sample_rate as f64 * duration) as usize;
            return self.ring_drain(n, duration, RingDrain::MultiContiguous);
        }
        // No ring to splice: the on-demand generator is already contiguous.
        self.capture_multi(duration)
    }

    fn discarded_samples(&self) -> u64 {
        self.ring
            .as_ref()
            .map(|r| r.rings.discarded_samples())
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
            .map(|r| r.rings.last_drain_occupancy().to_vec())
            .unwrap_or_default()
    }

    fn enable_ring_mode(&mut self, process_secs: f64, n_refs: usize, period: usize) {
        self.enable_ring_mode_inner(process_secs, n_refs, period);
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
