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
use ac_core::generator::{generate_pink_noise, generate_sine_1s};

// ---------------------------------------------------------------------------
// Shared state between engine and stream callbacks
// ---------------------------------------------------------------------------

struct SharedState {
    tone_buf: Mutex<Vec<f32>>,
    tone_pos: AtomicUsize,
    silence:  AtomicBool,
    ring:     Mutex<Vec<f32>>,
    xruns:    AtomicUsize,
}

pub struct CpalEngine {
    sample_rate: u32,
    state:       Arc<SharedState>,
    _out_stream: Option<Stream>,
    _in_stream:  Option<Stream>,
}

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
            }),
            _out_stream: None,
            _in_stream:  None,
        }
    }
}

// ---------------------------------------------------------------------------
// Output callback helpers
// ---------------------------------------------------------------------------

fn fill_output_f32(data: &mut [f32], state: &SharedState) {
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
    let err = |e| eprintln!("cpal output: {e}");
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
    let err = |e| eprintln!("cpal input: {e}");
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
