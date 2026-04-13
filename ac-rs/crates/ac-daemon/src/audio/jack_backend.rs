//! JACK audio backend.
//!
//! Runs a JACK client with one output and two input ports:
//!   - `in`     — measurement input  (primary capture)
//!   - `in_ref` — reference input    (for transfer function / DUT tests)
//!
//! The RT process callback fills both ringbuffers simultaneously.

use std::f64::consts::PI;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use jack::{AudioIn, AudioOut, Client, ClientOptions, Control, ProcessScope};

use super::AudioEngine;
use ac_core::generator::{dbfs_to_amplitude, generate_pink_noise, generate_sine_1s};

// -----------------------------------------------------------------------

struct SharedState {
    // Tone buffer cycled by the RT callback
    tone_buf:  Mutex<Vec<f32>>,
    tone_pos:  AtomicUsize,
    silence:   AtomicBool,

    // Measurement ringbuffer
    ring:      Mutex<Vec<f32>>,
    // Reference ringbuffer (second input port)
    ring_ref:  Mutex<Vec<f32>>,

    xruns:     AtomicUsize,
}

pub struct JackEngine {
    sample_rate:    u32,
    state:          Arc<SharedState>,
    _async_client:  Option<jack::AsyncClient<Notifications, Process>>,
    output_ports:   Vec<String>,
    input_port:     Option<String>,
    ref_port:       Option<String>,
}

impl JackEngine {
    /// Probe whether a JACK server is reachable without starting one.
    pub fn available() -> bool {
        jack::Client::new("ac-daemon-probe", jack::ClientOptions::NO_START_SERVER).is_ok()
    }

    pub fn new() -> Self {
        Self {
            sample_rate: 48_000,
            state: Arc::new(SharedState {
                tone_buf: Mutex::new(vec![0.0f32; 48_000]),
                tone_pos: AtomicUsize::new(0),
                silence:  AtomicBool::new(true),
                ring:     Mutex::new(Vec::new()),
                ring_ref: Mutex::new(Vec::new()),
                xruns:    AtomicUsize::new(0),
            }),
            _async_client: None,
            output_ports:  Vec::new(),
            input_port:    None,
            ref_port:      None,
        }
    }

    fn client_name(&self) -> Option<String> {
        self._async_client.as_ref().map(|ac| ac.as_client().name().to_string())
    }
}

// -----------------------------------------------------------------------

struct Process {
    out_port:     jack::Port<AudioOut>,
    in_port:      jack::Port<AudioIn>,
    in_ref_port:  jack::Port<AudioIn>,
    state:        Arc<SharedState>,
}

impl jack::ProcessHandler for Process {
    fn process(&mut self, _: &Client, scope: &ProcessScope) -> Control {
        let out_buf = self.out_port.as_mut_slice(scope);
        let in_buf  = self.in_port.as_slice(scope);
        let ref_buf = self.in_ref_port.as_slice(scope);

        // Fill output
        if self.state.silence.load(Ordering::Relaxed) {
            out_buf.fill(0.0);
        } else {
            let tone = self.state.tone_buf.lock().unwrap();
            let n = tone.len();
            if n > 0 {
                for s in out_buf.iter_mut() {
                    let pos = self.state.tone_pos.fetch_add(1, Ordering::Relaxed) % n;
                    *s = tone[pos];
                }
            } else {
                out_buf.fill(0.0);
            }
        }

        // Capture measurement channel
        self.state.ring.lock().unwrap().extend_from_slice(in_buf);
        // Capture reference channel
        self.state.ring_ref.lock().unwrap().extend_from_slice(ref_buf);

        Control::Continue
    }
}

struct Notifications;
impl jack::NotificationHandler for Notifications {
    fn xrun(&mut self, _: &Client) -> Control {
        Control::Continue
    }
}

// -----------------------------------------------------------------------

impl AudioEngine for JackEngine {
    fn start(&mut self, output_ports: &[String], input_port: Option<&str>) -> Result<()> {
        let (client, _status) = Client::new("ac-daemon", ClientOptions::NO_START_SERVER)
            .context("JACK client")?;

        self.sample_rate = client.sample_rate() as u32;

        let out_port     = client.register_port("out",    AudioOut::default()).context("register out")?;
        let in_port      = client.register_port("in",     AudioIn::default()) .context("register in")?;
        let in_ref_port  = client.register_port("in_ref", AudioIn::default()) .context("register in_ref")?;

        let state = self.state.clone();
        {
            let mut buf = state.tone_buf.lock().unwrap();
            *buf = vec![0.0f32; self.sample_rate as usize];
        }

        let process = Process { out_port, in_port, in_ref_port, state };
        let async_client = client.activate_async(Notifications, process)
            .context("JACK activate")?;

        let name = async_client.as_client().name().to_string();
        let out_name = name.clone() + ":out";
        let in_name  = name.clone() + ":in";

        for dest in output_ports {
            async_client.as_client().connect_ports_by_name(&out_name, dest).ok();
        }
        if let Some(src) = input_port {
            async_client.as_client().connect_ports_by_name(src, &in_name).ok();
            self.input_port = Some(src.to_string());
        }

        self.output_ports  = output_ports.to_vec();
        self._async_client = Some(async_client);
        Ok(())
    }

    fn stop(&mut self) {
        self._async_client = None;
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

        self.state.ring.lock().unwrap().clear();

        let timeout = Instant::now() + Duration::from_secs_f64(duration + 2.0);
        loop {
            std::thread::sleep(Duration::from_millis(10));
            if self.state.ring.lock().unwrap().len() >= n_needed { break; }
            if Instant::now() > timeout {
                anyhow::bail!("capture_block timeout after {duration}s");
            }
        }

        let samples: Vec<f32> = self.state.ring.lock().unwrap().drain(..n_needed).collect();
        Ok(samples)
    }

    fn capture_stereo(&mut self, duration: f64) -> Result<(Vec<f32>, Vec<f32>)> {
        let n_needed = (self.sample_rate as f64 * duration) as usize;

        // Clear both buffers to start a fresh synchronized capture
        self.state.ring.lock().unwrap().clear();
        self.state.ring_ref.lock().unwrap().clear();

        let timeout = Instant::now() + Duration::from_secs_f64(duration + 2.0);
        loop {
            std::thread::sleep(Duration::from_millis(10));
            if self.state.ring.lock().unwrap().len() >= n_needed { break; }
            if Instant::now() > timeout {
                anyhow::bail!("capture_stereo timeout after {duration}s");
            }
        }

        let meas: Vec<f32> = self.state.ring.lock().unwrap().drain(..n_needed).collect();
        let ref_available = self.state.ring_ref.lock().unwrap().len();
        let take = n_needed.min(ref_available);
        let mut refch: Vec<f32> = self.state.ring_ref.lock().unwrap().drain(..take).collect();
        refch.resize(n_needed, 0.0); // pad if ref port disconnected

        Ok((meas, refch))
    }

    fn reconnect_input(&mut self, port: &str) -> Result<()> {
        if let Some(ref ac) = self._async_client {
            let in_name = ac.as_client().name().to_string() + ":in";
            if let Some(ref old) = self.input_port {
                ac.as_client().disconnect_ports_by_name(old, &in_name).ok();
            }
            ac.as_client().connect_ports_by_name(port, &in_name)
                .context("reconnect_input")?;
            self.state.ring.lock().unwrap().clear();
            self.input_port = Some(port.to_string());
        }
        Ok(())
    }

    fn add_ref_input(&mut self, port: &str) -> Result<()> {
        if let Some(ref ac) = self._async_client {
            let ref_name = ac.as_client().name().to_string() + ":in_ref";
            if let Some(ref old) = self.ref_port {
                ac.as_client().disconnect_ports_by_name(old, &ref_name).ok();
            }
            ac.as_client().connect_ports_by_name(port, &ref_name)
                .context("add_ref_input")?;
            self.state.ring_ref.lock().unwrap().clear();
            self.ref_port = Some(port.to_string());
        }
        Ok(())
    }

    fn flush_capture(&mut self) {
        self.state.ring.lock().unwrap().clear();
        self.state.ring_ref.lock().unwrap().clear();
    }

    fn connect_output(&mut self, port: &str) -> Result<()> {
        if let Some(ref ac) = self._async_client {
            let out_name = ac.as_client().name().to_string() + ":out";
            ac.as_client().connect_ports_by_name(&out_name, port)
                .context("connect_output")?;
            if !self.output_ports.contains(&port.to_string()) {
                self.output_ports.push(port.to_string());
            }
        }
        Ok(())
    }

    fn disconnect_output(&mut self, port: &str) {
        if let Some(ref ac) = self._async_client {
            let out_name = ac.as_client().name().to_string() + ":out";
            ac.as_client().disconnect_ports_by_name(&out_name, port).ok();
        }
        self.output_ports.retain(|p| p != port);
    }

    fn xruns(&self) -> u32 {
        self.state.xruns.load(Ordering::Relaxed) as u32
    }

    fn playback_ports(&self) -> Vec<String> {
        if let Some(ref ac) = self._async_client {
            ac.as_client().ports(None, Some("32 bit float mono audio"), jack::PortFlags::IS_INPUT)
        } else if let Ok((c, _)) = Client::new("ac-daemon-probe", ClientOptions::NO_START_SERVER) {
            c.ports(None, Some("32 bit float mono audio"), jack::PortFlags::IS_INPUT)
        } else {
            Vec::new()
        }
    }

    fn capture_ports(&self) -> Vec<String> {
        if let Some(ref ac) = self._async_client {
            ac.as_client().ports(None, Some("32 bit float mono audio"), jack::PortFlags::IS_OUTPUT)
        } else if let Ok((c, _)) = Client::new("ac-daemon-probe", ClientOptions::NO_START_SERVER) {
            c.ports(None, Some("32 bit float mono audio"), jack::PortFlags::IS_OUTPUT)
        } else {
            Vec::new()
        }
    }
}
