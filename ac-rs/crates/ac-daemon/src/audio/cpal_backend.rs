//! CPAL audio backend — fallback when JACK is not running.
//!
//! Enabled by the `cpal-audio` feature flag. To build with CPAL support:
//!   1. Uncomment the `cpal` dep in Cargo.toml
//!   2. Uncomment the `cpal-audio` feature in Cargo.toml
//!   3. `cargo build -p ac-daemon --features cpal-audio`
//!
//! Port names are formatted as `cpal:<device-name>:ch<N>`.
//! `start()` ignores port names and opens the default I/O device pair;
//! port-level routing is a JACK-specific concept.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, Stream, StreamConfig};

use super::AudioEngine;
use ac_core::shared::generator::{generate_pink_noise, generate_sine_1s};

// ---------------------------------------------------------------------------
// Shared state between engine and stream callbacks
// ---------------------------------------------------------------------------

struct SharedState {
    tone_buf: Mutex<Vec<f32>>,
    tone_pos: AtomicUsize,
    silence:  AtomicBool,
    ring:     Mutex<Vec<f32>>,
    xruns:    AtomicUsize,
    // One-shot playback for `play_and_capture`. When `one_shot_active`
    // is set, output callbacks fill from `one_shot_buf[one_shot_pos..]`
    // instead of the looping tone; when the buffer is exhausted the flag
    // is cleared and playback falls back to silence.
    one_shot_buf:    Mutex<Vec<f32>>,
    one_shot_pos:    AtomicUsize,
    one_shot_active: AtomicBool,
}

pub struct CpalEngine {
    sample_rate: u32,
    state:       Arc<SharedState>,
    _out_stream: Option<Stream>,
    _in_stream:  Option<Stream>,
}

// SAFETY: CpalEngine is created and used entirely within a single worker thread.
// cpal::Stream is !Send on Linux only as a conservative platform-level marker
// (guarding against JACK callbacks on some targets); the streams are never
// shared or moved across threads in our usage.
unsafe impl Send for CpalEngine {}

impl CpalEngine {
    pub fn new() -> Self {
        Self {
            sample_rate: 44_100,
            state: Arc::new(SharedState {
                tone_buf: Mutex::new(vec![0.0f32; 44_100]),
                tone_pos: AtomicUsize::new(0),
                silence:  AtomicBool::new(true),
                ring:     Mutex::new(Vec::new()),
                xruns:    AtomicUsize::new(0),
                one_shot_buf:    Mutex::new(Vec::new()),
                one_shot_pos:    AtomicUsize::new(0),
                one_shot_active: AtomicBool::new(false),
            }),
            _out_stream: None,
            _in_stream:  None,
        }
    }
}

// ---------------------------------------------------------------------------
// Output callback helpers
// ---------------------------------------------------------------------------

/// If one-shot playback is active, drain `data.len()` samples from
/// `one_shot_buf`, zero-padding the tail if the buffer is exhausted
/// (and clearing `one_shot_active` in that case). Returns `true` when
/// it handled the fill, `false` when the caller should fall through to
/// silence / looping tone.
fn try_fill_one_shot(data: &mut [f32], state: &SharedState) -> bool {
    if !state.one_shot_active.load(Ordering::Acquire) {
        return false;
    }
    let buf = state.one_shot_buf.lock().unwrap();
    let pos = state.one_shot_pos.load(Ordering::Relaxed);
    let remaining = buf.len().saturating_sub(pos);
    let n = data.len().min(remaining);
    if n > 0 {
        data[..n].copy_from_slice(&buf[pos..pos + n]);
    }
    for s in &mut data[n..] {
        *s = 0.0;
    }
    let new_pos = pos + n;
    state.one_shot_pos.store(new_pos, Ordering::Relaxed);
    if new_pos >= buf.len() {
        state.one_shot_active.store(false, Ordering::Release);
    }
    true
}

fn fill_output_f32(data: &mut [f32], state: &SharedState) {
    if try_fill_one_shot(data, state) {
        return;
    }
    let tone = state.tone_buf.lock().unwrap();
    let n = tone.len();
    if state.silence.load(Ordering::Relaxed) || n == 0 {
        data.fill(0.0);
    } else {
        for s in data.iter_mut() {
            let pos = state.tone_pos.fetch_add(1, Ordering::Relaxed) % n;
            *s = tone[pos];
        }
    }
}

fn fill_output_i16(data: &mut [i16], state: &SharedState) {
    if state.one_shot_active.load(Ordering::Acquire) {
        let mut scratch = vec![0.0f32; data.len()];
        try_fill_one_shot(&mut scratch, state);
        for (o, &v) in data.iter_mut().zip(scratch.iter()) {
            *o = (v.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
        }
        return;
    }
    let tone = state.tone_buf.lock().unwrap();
    let n = tone.len();
    if state.silence.load(Ordering::Relaxed) || n == 0 {
        data.fill(0);
    } else {
        for s in data.iter_mut() {
            let pos = state.tone_pos.fetch_add(1, Ordering::Relaxed) % n;
            *s = (tone[pos].clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
        }
    }
}

fn fill_output_i32(data: &mut [i32], state: &SharedState) {
    if state.one_shot_active.load(Ordering::Acquire) {
        let mut scratch = vec![0.0f32; data.len()];
        try_fill_one_shot(&mut scratch, state);
        for (o, &v) in data.iter_mut().zip(scratch.iter()) {
            *o = (v.clamp(-1.0, 1.0) * i32::MAX as f32) as i32;
        }
        return;
    }
    let tone = state.tone_buf.lock().unwrap();
    let n = tone.len();
    if state.silence.load(Ordering::Relaxed) || n == 0 {
        data.fill(0);
    } else {
        for s in data.iter_mut() {
            let pos = state.tone_pos.fetch_add(1, Ordering::Relaxed) % n;
            *s = (tone[pos].clamp(-1.0, 1.0) * i32::MAX as f32) as i32;
        }
    }
}

fn build_output(
    device: &cpal::Device,
    config: &StreamConfig,
    format: SampleFormat,
    state:  Arc<SharedState>,
) -> Result<Stream> {
    let err_state = state.clone();
    let err = move |e| {
        err_state.xruns.fetch_add(1, Ordering::Relaxed);
        eprintln!("cpal output: {e}");
    };
    match format {
        SampleFormat::F32 => {
            device.build_output_stream(
                config,
                move |data: &mut [f32], _| fill_output_f32(data, &state),
                err, None,
            ).context("build output (f32)")
        }
        SampleFormat::I16 => {
            device.build_output_stream(
                config,
                move |data: &mut [i16], _| fill_output_i16(data, &state),
                err, None,
            ).context("build output (i16)")
        }
        SampleFormat::I32 => {
            device.build_output_stream(
                config,
                move |data: &mut [i32], _| fill_output_i32(data, &state),
                err, None,
            ).context("build output (i32)")
        }
        other => anyhow::bail!("unsupported output sample format: {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Input callback helpers — downmix multi-channel to mono (channel 0)
// ---------------------------------------------------------------------------

fn drain_input_f32(data: &[f32], channels: usize, state: &SharedState) {
    if channels <= 1 {
        state.ring.lock().unwrap().extend_from_slice(data);
    } else {
        let mono: Vec<f32> = data.chunks_exact(channels).map(|f| f[0]).collect();
        state.ring.lock().unwrap().extend_from_slice(&mono);
    }
}

fn drain_input_i16(data: &[i16], channels: usize, state: &SharedState) {
    let scale = 1.0 / i16::MAX as f32;
    if channels <= 1 {
        let mono: Vec<f32> = data.iter().map(|&s| s as f32 * scale).collect();
        state.ring.lock().unwrap().extend_from_slice(&mono);
    } else {
        let mono: Vec<f32> = data.chunks_exact(channels).map(|f| f[0] as f32 * scale).collect();
        state.ring.lock().unwrap().extend_from_slice(&mono);
    }
}

fn drain_input_i32(data: &[i32], channels: usize, state: &SharedState) {
    let scale = 1.0 / i32::MAX as f32;
    if channels <= 1 {
        let mono: Vec<f32> = data.iter().map(|&s| s as f32 * scale).collect();
        state.ring.lock().unwrap().extend_from_slice(&mono);
    } else {
        let mono: Vec<f32> = data.chunks_exact(channels).map(|f| f[0] as f32 * scale).collect();
        state.ring.lock().unwrap().extend_from_slice(&mono);
    }
}

fn build_input(
    device:   &cpal::Device,
    config:   &StreamConfig,
    format:   SampleFormat,
    channels: usize,
    state:    Arc<SharedState>,
) -> Result<Stream> {
    let err_state = state.clone();
    let err = move |e| {
        err_state.xruns.fetch_add(1, Ordering::Relaxed);
        eprintln!("cpal input: {e}");
    };
    match format {
        SampleFormat::F32 => {
            device.build_input_stream(
                config,
                move |data: &[f32], _| drain_input_f32(data, channels, &state),
                err, None,
            ).context("build input (f32)")
        }
        SampleFormat::I16 => {
            device.build_input_stream(
                config,
                move |data: &[i16], _| drain_input_i16(data, channels, &state),
                err, None,
            ).context("build input (i16)")
        }
        SampleFormat::I32 => {
            device.build_input_stream(
                config,
                move |data: &[i32], _| drain_input_i32(data, channels, &state),
                err, None,
            ).context("build input (i32)")
        }
        other => anyhow::bail!("unsupported input sample format: {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// AudioEngine impl
// ---------------------------------------------------------------------------

impl AudioEngine for CpalEngine {
    fn start(&mut self, _output_ports: &[String], _input_port: Option<&str>) -> Result<()> {
        let host = cpal::default_host();

        // Output stream
        let out_dev = host.default_output_device()
            .context("no default output device")?;
        let out_sup = out_dev.default_output_config()
            .context("default output config")?;
        self.sample_rate = out_sup.sample_rate().0;
        {
            let mut buf = self.state.tone_buf.lock().unwrap();
            *buf = vec![0.0f32; self.sample_rate as usize];
        }
        let out_cfg: StreamConfig = out_sup.config();
        let out_fmt = out_sup.sample_format();
        let out_stream = build_output(&out_dev, &out_cfg, out_fmt, self.state.clone())?;
        out_stream.play().context("start output stream")?;
        self._out_stream = Some(out_stream);

        // Input stream
        let in_dev = host.default_input_device()
            .context("no default input device")?;
        let in_sup = in_dev.default_input_config()
            .context("default input config")?;
        let in_ch  = in_sup.channels() as usize;
        let in_cfg: StreamConfig = in_sup.config();
        let in_fmt = in_sup.sample_format();
        let in_stream = build_input(&in_dev, &in_cfg, in_fmt, in_ch, self.state.clone())?;
        in_stream.play().context("start input stream")?;
        self._in_stream = Some(in_stream);

        Ok(())
    }

    fn stop(&mut self) {
        self._out_stream = None;
        self._in_stream  = None;
    }

    fn sample_rate(&self) -> u32 { self.sample_rate }

    fn set_tone(&mut self, freq_hz: f64, amplitude: f64) {
        let buf = generate_sine_1s(freq_hz, amplitude, self.sample_rate);
        *self.state.tone_buf.lock().unwrap() = buf;
        self.state.tone_pos.store(0, Ordering::Relaxed);
        self.state.silence.store(false, Ordering::Relaxed);
    }

    fn set_pink(&mut self, amplitude: f64) {
        let buf = generate_pink_noise(amplitude, self.sample_rate);
        *self.state.tone_buf.lock().unwrap() = buf;
        self.state.tone_pos.store(0, Ordering::Relaxed);
        self.state.silence.store(false, Ordering::Relaxed);
    }

    fn set_silence(&mut self) {
        self.state.silence.store(true, Ordering::Relaxed);
    }

    fn play_and_capture(&mut self, samples: &[f32], tail_s: f64) -> Result<Vec<f32>> {
        if samples.is_empty() {
            anyhow::bail!("play_and_capture: empty stimulus");
        }
        let sr = self.sample_rate as f64;
        let n_total = samples.len() + (tail_s.max(0.0) * sr) as usize;

        // Publish one-shot buffer, reset position, stop the looping tone,
        // clear capture ring, then enable. The Release on `one_shot_active`
        // synchronises with the Acquire in the output callback.
        self.state.silence.store(true, Ordering::Relaxed);
        *self.state.one_shot_buf.lock().unwrap() = samples.to_vec();
        self.state.one_shot_pos.store(0, Ordering::Relaxed);
        self.state.ring.lock().unwrap().clear();
        self.state.one_shot_active.store(true, Ordering::Release);

        let duration_s = n_total as f64 / sr;
        let timeout = Instant::now() + Duration::from_secs_f64(duration_s + 2.0);
        let mut ok = false;
        loop {
            std::thread::sleep(Duration::from_millis(10));
            if self.state.ring.lock().unwrap().len() >= n_total {
                ok = true;
                break;
            }
            if Instant::now() > timeout {
                break;
            }
        }
        self.state.one_shot_active.store(false, Ordering::Release);
        if !ok {
            anyhow::bail!("cpal play_and_capture timeout after {duration_s:.1}s");
        }
        let out: Vec<f32> = self
            .state
            .ring
            .lock()
            .unwrap()
            .drain(..n_total)
            .collect();
        Ok(out)
    }

    fn capture_block(&mut self, duration: f64) -> Result<Vec<f32>> {
        let n_needed = (self.sample_rate as f64 * duration) as usize;
        self.state.ring.lock().unwrap().clear();
        let timeout = Instant::now() + Duration::from_secs_f64(duration + 2.0);
        loop {
            std::thread::sleep(Duration::from_millis(10));
            if self.state.ring.lock().unwrap().len() >= n_needed { break; }
            if Instant::now() > timeout {
                anyhow::bail!("cpal capture_block timeout after {duration:.1}s");
            }
        }
        let samples: Vec<f32> = self.state.ring.lock().unwrap().drain(..n_needed).collect();
        Ok(samples)
    }

    fn flush_capture(&mut self) {
        self.state.ring.lock().unwrap().clear();
    }

    fn xruns(&self) -> u32 {
        self.state.xruns.load(Ordering::Relaxed) as u32
    }

    // CPAL opens default input/output devices and cannot reroute individual
    // ports, so handlers that rely on routing must refuse on this backend.
    fn supports_routing(&self) -> bool { false }
    fn backend_name(&self) -> &'static str { "cpal" }

    fn playback_ports(&self) -> Vec<String> {
        let host = cpal::default_host();
        let mut ports = Vec::new();
        if let Ok(devices) = host.output_devices() {
            for device in devices {
                let name = device.name().unwrap_or_else(|_| "unknown".to_string());
                let n_ch = device.default_output_config()
                    .map(|c| c.channels())
                    .unwrap_or(2);
                for ch in 0..n_ch {
                    ports.push(format!("cpal:{name}:ch{ch}"));
                }
            }
        }
        ports
    }

    fn capture_ports(&self) -> Vec<String> {
        let host = cpal::default_host();
        let mut ports = Vec::new();
        if let Ok(devices) = host.input_devices() {
            for device in devices {
                let name = device.name().unwrap_or_else(|_| "unknown".to_string());
                let n_ch = device.default_input_config()
                    .map(|c| c.channels())
                    .unwrap_or(2);
                for ch in 0..n_ch {
                    ports.push(format!("cpal:{name}:ch{ch}"));
                }
            }
        }
        ports
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn make_state() -> SharedState {
        SharedState {
            tone_buf: Mutex::new(vec![1.0, 2.0, 3.0, 4.0]),
            tone_pos: AtomicUsize::new(0),
            silence:  AtomicBool::new(false),
            ring:     Mutex::new(Vec::new()),
            xruns:    AtomicUsize::new(0),
            one_shot_buf:    Mutex::new(Vec::new()),
            one_shot_pos:    AtomicUsize::new(0),
            one_shot_active: AtomicBool::new(false),
        }
    }

    // ---- Output fill: f32 ----

    #[test]
    fn fill_f32_copies_tone_with_wraparound() {
        let state = make_state();
        let mut out = [0.0f32; 7];
        fill_output_f32(&mut out, &state);
        assert_eq!(out, [1.0, 2.0, 3.0, 4.0, 1.0, 2.0, 3.0]);
    }

    #[test]
    fn fill_f32_silence_zeros() {
        let state = make_state();
        state.silence.store(true, Ordering::Relaxed);
        let mut out = [9.0f32; 4];
        fill_output_f32(&mut out, &state);
        assert_eq!(out, [0.0; 4]);
    }

    // ---- Output fill: i16 ----

    #[test]
    fn fill_i16_scales_and_clamps() {
        let state = SharedState {
            tone_buf: Mutex::new(vec![0.5, -0.5, 1.5]),
            tone_pos: AtomicUsize::new(0),
            silence:  AtomicBool::new(false),
            ring:     Mutex::new(Vec::new()),
            xruns:    AtomicUsize::new(0),
            one_shot_buf:    Mutex::new(Vec::new()),
            one_shot_pos:    AtomicUsize::new(0),
            one_shot_active: AtomicBool::new(false),
        };
        let mut out = [0i16; 3];
        fill_output_i16(&mut out, &state);
        assert_eq!(out[0], (0.5 * i16::MAX as f32) as i16);
        assert_eq!(out[1], (-0.5 * i16::MAX as f32) as i16);
        assert_eq!(out[2], i16::MAX); // 1.5 clamped to 1.0
    }

    // ---- Output fill: i32 ----

    #[test]
    fn fill_i32_scales_and_clamps() {
        let state = SharedState {
            tone_buf: Mutex::new(vec![0.25, -2.0]),
            tone_pos: AtomicUsize::new(0),
            silence:  AtomicBool::new(false),
            ring:     Mutex::new(Vec::new()),
            xruns:    AtomicUsize::new(0),
            one_shot_buf:    Mutex::new(Vec::new()),
            one_shot_pos:    AtomicUsize::new(0),
            one_shot_active: AtomicBool::new(false),
        };
        let mut out = [0i32; 2];
        fill_output_i32(&mut out, &state);
        assert_eq!(out[0], (0.25 * i32::MAX as f32) as i32);
        assert_eq!(out[1], ((-1.0f32).clamp(-1.0, 1.0) * i32::MAX as f32) as i32);
    }

    // ---- Input drain: mono and multichannel ----

    #[test]
    fn drain_f32_mono_passthrough() {
        let state = make_state();
        let input = [0.1f32, 0.2, 0.3];
        drain_input_f32(&input, 1, &state);
        let ring = state.ring.lock().unwrap();
        assert_eq!(&ring[..], &[0.1, 0.2, 0.3]);
    }

    #[test]
    fn drain_f32_stereo_takes_channel_0() {
        let state = make_state();
        // Interleaved stereo: [L0, R0, L1, R1, L2, R2]
        let input = [0.1f32, 0.9, 0.2, 0.8, 0.3, 0.7];
        drain_input_f32(&input, 2, &state);
        let ring = state.ring.lock().unwrap();
        assert_eq!(&ring[..], &[0.1, 0.2, 0.3]);
    }

    #[test]
    fn drain_i16_scales_to_float() {
        let state = make_state();
        let input = [i16::MAX, i16::MIN, 0i16];
        drain_input_i16(&input, 1, &state);
        let ring = state.ring.lock().unwrap();
        let scale = 1.0 / i16::MAX as f32;
        assert!((ring[0] - 1.0).abs() < 1e-4);
        assert!((ring[1] - (i16::MIN as f32 * scale)).abs() < 1e-4);
        assert!((ring[2]).abs() < 1e-9);
    }

    #[test]
    fn drain_i32_multichannel_extracts_ch0() {
        let state = make_state();
        // 3-channel interleaved
        let input = [i32::MAX, 0, 0, i32::MIN, 0, 0];
        drain_input_i32(&input, 3, &state);
        let ring = state.ring.lock().unwrap();
        assert_eq!(ring.len(), 2);
        assert!((ring[0] - 1.0).abs() < 1e-4);
    }

    // ---- State transitions ----

    #[test]
    fn set_tone_resets_pos_and_unsilences() {
        let mut eng = CpalEngine::new();
        eng.state.tone_pos.store(999, Ordering::Relaxed);
        eng.state.silence.store(true, Ordering::Relaxed);
        eng.set_tone(1000.0, 0.5);
        assert_eq!(eng.state.tone_pos.load(Ordering::Relaxed), 0);
        assert!(!eng.state.silence.load(Ordering::Relaxed));
        assert!(!eng.state.tone_buf.lock().unwrap().is_empty());
    }

    #[test]
    fn set_silence_flag() {
        let mut eng = CpalEngine::new();
        eng.state.silence.store(false, Ordering::Relaxed);
        eng.set_silence();
        assert!(eng.state.silence.load(Ordering::Relaxed));
    }

    // ---- Trait properties ----

    #[test]
    fn does_not_support_routing() {
        let eng = CpalEngine::new();
        assert!(!eng.supports_routing());
        assert_eq!(eng.backend_name(), "cpal");
    }

    #[test]
    fn flush_clears_ring() {
        let mut eng = CpalEngine::new();
        eng.state.ring.lock().unwrap().extend_from_slice(&[1.0, 2.0, 3.0]);
        eng.flush_capture();
        assert!(eng.state.ring.lock().unwrap().is_empty());
    }

    // ---- One-shot playback state machine ----

    #[test]
    fn try_fill_one_shot_returns_false_when_inactive() {
        let state = make_state();
        let mut out = [9.0f32; 3];
        assert!(!try_fill_one_shot(&mut out, &state));
        assert_eq!(out, [9.0, 9.0, 9.0]); // untouched
    }

    #[test]
    fn one_shot_overrides_tone_in_f32() {
        let state = make_state();
        *state.one_shot_buf.lock().unwrap() = vec![0.1, 0.2, 0.3, 0.4];
        state.one_shot_active.store(true, Ordering::Release);
        let mut out = [0.0f32; 3];
        fill_output_f32(&mut out, &state);
        assert_eq!(out, [0.1, 0.2, 0.3]);
        assert_eq!(state.one_shot_pos.load(Ordering::Relaxed), 3);
        assert!(state.one_shot_active.load(Ordering::Acquire));
    }

    #[test]
    fn one_shot_exhausts_and_clears_active_flag() {
        let state = make_state();
        *state.one_shot_buf.lock().unwrap() = vec![0.1, 0.2];
        state.one_shot_active.store(true, Ordering::Release);
        let mut out = [9.0f32; 4];
        fill_output_f32(&mut out, &state);
        assert_eq!(out, [0.1, 0.2, 0.0, 0.0]); // tail zero-padded
        assert!(!state.one_shot_active.load(Ordering::Acquire));
    }

    #[test]
    fn one_shot_preempts_i16_tone() {
        let state = make_state();
        *state.one_shot_buf.lock().unwrap() = vec![0.5, -0.5];
        state.one_shot_active.store(true, Ordering::Release);
        let mut out = [0i16; 2];
        fill_output_i16(&mut out, &state);
        assert_eq!(out[0], (0.5 * i16::MAX as f32) as i16);
        assert_eq!(out[1], (-0.5 * i16::MAX as f32) as i16);
    }

    #[test]
    fn one_shot_preempts_i32_tone() {
        let state = make_state();
        *state.one_shot_buf.lock().unwrap() = vec![0.25, 1.5];
        state.one_shot_active.store(true, Ordering::Release);
        let mut out = [0i32; 2];
        fill_output_i32(&mut out, &state);
        assert_eq!(out[0], (0.25 * i32::MAX as f32) as i32);
        assert_eq!(out[1], i32::MAX); // clamped
    }
}
