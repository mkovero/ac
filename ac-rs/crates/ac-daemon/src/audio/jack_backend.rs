//! JACK audio backend.
//!
//! Runs a JACK client with one output and one input port.
//! The RT process callback fills output from a tone buffer and copies input
//! into a ringbuffer; the main thread drains the ringbuffer in `capture_block`.

use std::f64::consts::PI;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use jack::{AudioIn, AudioOut, Client, ClientOptions, Control, ProcessScope};

use super::AudioEngine;
use ac_core::generator::{dbfs_to_amplitude, generate_pink_noise, generate_sine_1s};

// -----------------------------------------------------------------------

struct SharedState {
    // Tone buffer cycled by the RT callback
    tone_buf:   Mutex<Vec<f32>>,
    tone_pos:   AtomicUsize,
    silence:    AtomicBool,

    // Ringbuffer: RT callback writes, main thread reads
    ring:       Mutex<Vec<f32>>,
    xruns:      AtomicUsize,
}

pub struct JackEngine {
    sample_rate: u32,
    state:       Arc<SharedState>,
    _async_client: Option<jack::AsyncClient<Notifications, Process>>,
    output_ports: Vec<String>,
    input_port:   Option<String>,
}

impl JackEngine {
    pub fn new() -> Self {
        Self {
            sample_rate: 48_000,
            state: Arc::new(SharedState {
                tone_buf: Mutex::new(vec![0.0f32; 48_000]),
                tone_pos: AtomicUsize::new(0),
                silence:  AtomicBool::new(true),
                ring:     Mutex::new(Vec::new()),
                xruns:    AtomicUsize::new(0),
            }),
            _async_client: None,
            output_ports:  Vec::new(),
            input_port:    None,
        }
    }
}

struct Process {
    out_port: jack::Port<AudioOut>,
    in_port:  jack::Port<AudioIn>,
    state:    Arc<SharedState>,
}

impl jack::ProcessHandler for Process {
    fn process(&mut self, _: &Client, scope: &ProcessScope) -> Control {
        let out_buf = self.out_port.as_mut_slice(scope);
        let in_buf  = self.in_port.as_slice(scope);

        if self.state.silence.load(Ordering::Relaxed) {
            out_buf.fill(0.0);
        } else {
            let tone = self.state.tone_buf.lock().unwrap();
            let n = tone.len();
            if n > 0 {
                for (i, s) in out_buf.iter_mut().enumerate() {
                    let pos = self.state.tone_pos.fetch_add(1, Ordering::Relaxed) % n;
                    *s = tone[pos];
                }
            } else {
                out_buf.fill(0.0);
            }
        }

        // Copy captured audio into ringbuffer
        let mut ring = self.state.ring.lock().unwrap();
        ring.extend_from_slice(in_buf);

        Control::Continue
    }
}

struct Notifications;
impl jack::NotificationHandler for Notifications {
    fn xrun(&mut self, _: &Client) -> Control {
        Control::Continue
    }
}

impl AudioEngine for JackEngine {
    fn start(&mut self, output_ports: &[String], input_port: Option<&str>) -> Result<()> {
        let (client, _status) = Client::new("ac-daemon", ClientOptions::NO_START_SERVER)
            .context("JACK client")?;

        self.sample_rate = client.sample_rate() as u32;

        let out_port = client.register_port("out", AudioOut::default())
            .context("register out port")?;
        let in_port  = client.register_port("in", AudioIn::default())
            .context("register in port")?;

        let state = self.state.clone();

        // Pre-fill tone buffer with silence at the correct sample rate
        {
            let mut buf = state.tone_buf.lock().unwrap();
            *buf = vec![0.0f32; self.sample_rate as usize];
        }

        let process = Process { out_port, in_port, state: state.clone() };
        let async_client = client.activate_async(Notifications, process)
            .context("JACK activate")?;

        // Connect ports
        let out_name = async_client.as_client().name().to_string()
            + ":out";
        let in_name  = async_client.as_client().name().to_string()
            + ":in";

        for dest in output_ports {
            async_client.as_client().connect_ports_by_name(&out_name, dest).ok();
        }
        if let Some(src) = input_port {
            async_client.as_client().connect_ports_by_name(src, &in_name).ok();
        }

        self.output_ports = output_ports.to_vec();
        self.input_port   = input_port.map(str::to_string);
        self._async_client = Some(async_client);
        Ok(())
    }

    fn stop(&mut self) {
        self._async_client = None; // drops and deactivates JACK client
    }

    fn sample_rate(&self) -> u32 { self.sample_rate }

    fn set_tone(&mut self, freq_hz: f64, amplitude: f64) {
        let buf = generate_sine_1s(freq_hz, amplitude, self.sample_rate);
        *self.state.tone_buf.lock().unwrap() = buf;
        self.state.silence.store(false, Ordering::Relaxed);
    }

    fn set_pink(&mut self, amplitude: f64) {
        let buf = generate_pink_noise(amplitude, self.sample_rate);
        *self.state.tone_buf.lock().unwrap() = buf;
        self.state.silence.store(false, Ordering::Relaxed);
    }

    fn set_silence(&mut self) {
        self.state.silence.store(true, Ordering::Relaxed);
    }

    fn capture_block(&mut self, duration: f64) -> Result<Vec<f32>> {
        let n_needed = (self.sample_rate as f64 * duration) as usize;

        // Clear the ringbuffer and collect fresh samples
        {
            let mut ring = self.state.ring.lock().unwrap();
            ring.clear();
        }

        // Wait until we have enough samples
        let timeout = std::time::Instant::now()
            + std::time::Duration::from_secs_f64(duration + 2.0);

        loop {
            std::thread::sleep(std::time::Duration::from_millis(10));
            let len = self.state.ring.lock().unwrap().len();
            if len >= n_needed { break; }
            if std::time::Instant::now() > timeout {
                anyhow::bail!("capture_block timeout after {duration}s");
            }
        }

        let mut ring = self.state.ring.lock().unwrap();
        let samples: Vec<f32> = ring.drain(..n_needed).collect();
        Ok(samples)
    }

    fn xruns(&self) -> u32 {
        self.state.xruns.load(Ordering::Relaxed) as u32
    }

    fn playback_ports(&self) -> Vec<String> {
        if let Some(ref ac) = self._async_client {
            ac.as_client()
              .ports(None, Some("32 bit float mono audio"), jack::PortFlags::IS_INPUT)
        } else {
            // Try a temporary client just for discovery
            if let Ok((c, _)) = Client::new("ac-daemon-probe", ClientOptions::NO_START_SERVER) {
                c.ports(None, Some("32 bit float mono audio"), jack::PortFlags::IS_INPUT)
            } else {
                Vec::new()
            }
        }
    }

    fn capture_ports(&self) -> Vec<String> {
        if let Some(ref ac) = self._async_client {
            ac.as_client()
              .ports(None, Some("32 bit float mono audio"), jack::PortFlags::IS_OUTPUT)
        } else {
            if let Ok((c, _)) = Client::new("ac-daemon-probe", ClientOptions::NO_START_SERVER) {
                c.ports(None, Some("32 bit float mono audio"), jack::PortFlags::IS_OUTPUT)
            } else {
                Vec::new()
            }
        }
    }
}
