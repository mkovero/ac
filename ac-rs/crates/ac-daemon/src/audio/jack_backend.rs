//! JACK audio backend.
//!
//! Real-time safe: the process callback never locks and never allocates.
//! - Tone buffer is swapped via `ArcSwap<Arc<Vec<f32>>>` — RT loads a
//!   pointer, control thread publishes a new buffer.
//! - Capture rings are lock-free SPSC (`ringbuf`). When no consumer is
//!   draining (e.g. output-only `generate` commands), the producer overruns
//!   the fixed capacity and drops the NEWEST samples, so memory stays
//!   bounded (see issue #25).

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::Thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use arc_swap::ArcSwap;
use jack::{AudioIn, AudioOut, Client, ClientOptions, Control, ProcessScope};
use ringbuf::traits::{Consumer, Observer, Producer, Split};
use ringbuf::{HeapCons, HeapProd, HeapRb};

use super::AudioEngine;
use ac_core::generator::{generate_pink_noise, generate_sine_1s};

/// 16 s at 192 kHz — comfortably larger than any single capture request.
/// Fixed at construction so neither thread ever reallocates.
const RING_CAPACITY: usize = 16 * 192_000;

/// 4 s at 192 kHz — ref inputs are only used by (multi-pair) transfer_stream
/// whose `capture_duration(4, sr)` ≈ 2.5 s, so this leaves a comfortable
/// margin without the 16 s footprint of the main ring.
const REF_RING_CAPACITY: usize = 4 * 192_000;

/// Capacity ceiling for simultaneously-live reference-input ports. Ports are
/// registered on-demand from `add_ref_input` (not pre-registered at
/// `start()`), so JACK sees only the ports actually wired up. The ceiling
/// exists so the RT handler can pre-reserve `Vec` capacity and push new
/// ports without allocating in the process callback.
const MAX_REF_INPUTS: usize = 16;

/// Queue capacity for main-thread → RT-handler port hand-off. `MAX_REF_INPUTS`
/// is plenty: the handler drains the queue every period, so it only needs to
/// hold pending adds between two periods.
const REF_ADD_QUEUE_CAPACITY: usize = MAX_REF_INPUTS;

// -----------------------------------------------------------------------

struct SharedState {
    tone_buf: ArcSwap<Vec<f32>>,
    silence:  AtomicBool,
    xruns:    AtomicUsize,
    // Consumer (wait_ring) parks on its own thread handle; the RT process
    // callback does a `try_lock().unpark()` after pushing samples so the
    // waiter wakes within microseconds of data arriving instead of polling
    // on a 10 ms sleep.
    waker:    Mutex<Option<Thread>>,
}

/// Main-thread → RT-handler hand-off for a freshly-registered ref input.
/// The port is registered on the main thread via `AsyncClient::as_client()`
/// and shipped in along with its dedicated SPSC producer, which the RT
/// handler appends to its capture list on the next period.
struct RefAdd {
    port: jack::Port<AudioIn>,
    prod: HeapProd<f32>,
}

pub struct JackEngine {
    sample_rate:    u32,
    state:          Arc<SharedState>,
    ring_cons:      Option<HeapCons<f32>>,
    /// One SPSC consumer per active ref input, in insertion order. Parallel
    /// to `ref_ports`. Grows as `add_ref_input` is called; the RT handler
    /// owns the matching producers and drains new ones via `ref_add_prod`.
    ring_ref_cons:  Vec<HeapCons<f32>>,
    _async_client:  Option<jack::AsyncClient<Notifications, Process>>,
    output_ports:   Vec<String>,
    input_port:     Option<String>,
    /// Active ref source ports in insertion order; parallel to
    /// `ring_ref_cons`. Unlike the old slot-based design, there are no
    /// `None` holes — ports are only appended, never re-slotted.
    ref_ports:      Vec<String>,
    /// Main-side producer of the on-demand port hand-off queue. `None`
    /// between `stop()` and the next `start()`.
    ref_add_prod:   Option<HeapProd<RefAdd>>,
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
                tone_buf: ArcSwap::new(Arc::new(vec![0.0f32; 48_000])),
                silence:  AtomicBool::new(true),
                xruns:    AtomicUsize::new(0),
                waker:    Mutex::new(None),
            }),
            ring_cons:     None,
            ring_ref_cons: Vec::new(),
            _async_client: None,
            output_ports:  Vec::new(),
            input_port:    None,
            ref_ports:     Vec::new(),
            ref_add_prod:  None,
        }
    }
}

// -----------------------------------------------------------------------

struct Process {
    out_port:       jack::Port<AudioOut>,
    in_port:        jack::Port<AudioIn>,
    /// Ref capture ports, grown on-demand from the `ref_add_cons` queue.
    /// Pre-allocated to `MAX_REF_INPUTS` so RT-side `push` never reallocates.
    in_ref_ports:   Vec<jack::Port<AudioIn>>,
    state:          Arc<SharedState>,
    tone_pos:       usize,
    ring_prod:      HeapProd<f32>,
    /// Parallel to `in_ref_ports`, same pre-allocated capacity.
    ring_ref_prods: Vec<HeapProd<f32>>,
    /// Receives port hand-offs from the main thread; drained each period.
    ref_add_cons:   HeapCons<RefAdd>,
}

/// Fill `out` from `tone`, wrapping at buffer boundary. Returns updated position.
fn fill_tone(out: &mut [f32], tone: &[f32], mut pos: usize) -> usize {
    let n = tone.len();
    for s in out.iter_mut() {
        *s = tone[pos];
        pos += 1;
        if pos >= n { pos = 0; }
    }
    pos
}

impl jack::ProcessHandler for Process {
    fn process(&mut self, _: &Client, scope: &ProcessScope) -> Control {
        let out_buf = self.out_port.as_mut_slice(scope);
        let in_buf  = self.in_port.as_slice(scope);

        if self.state.silence.load(Ordering::Relaxed) {
            out_buf.fill(0.0);
        } else {
            let tone = self.state.tone_buf.load();
            if !tone.is_empty() {
                self.tone_pos = fill_tone(out_buf, &tone, self.tone_pos);
            } else {
                out_buf.fill(0.0);
            }
        }

        // Drain any pending ref-port additions before reading capture data.
        // Capacity was pre-reserved so `push` does not allocate.
        while let Some(add) = self.ref_add_cons.try_pop() {
            if self.in_ref_ports.len() < self.in_ref_ports.capacity() {
                self.in_ref_ports.push(add.port);
                self.ring_ref_prods.push(add.prod);
            }
            // Silently drop if we somehow exceeded capacity — the main-side
            // guard prevents this, but dropping is still RT-safe.
        }

        self.ring_prod.push_slice(in_buf);
        for (port, prod) in self.in_ref_ports.iter().zip(self.ring_ref_prods.iter_mut()) {
            prod.push_slice(port.as_slice(scope));
        }

        // Wake wait_ring if someone is parked. try_lock keeps this RT-safe:
        // if the consumer is mid-register/deregister, we skip — the next
        // period (≤ ~3 ms at 128-frame quanta) will catch it, and park_timeout
        // bounds worst-case latency anyway.
        if let Ok(guard) = self.state.waker.try_lock() {
            if let Some(t) = guard.as_ref() { t.unpark(); }
        }

        Control::Continue
    }
}

struct Notifications {
    state: Arc<SharedState>,
}

impl jack::NotificationHandler for Notifications {
    fn xrun(&mut self, _: &Client) -> Control {
        self.state.xruns.fetch_add(1, Ordering::Relaxed);
        Control::Continue
    }
}

// -----------------------------------------------------------------------

impl JackEngine {
    /// Wait until the measurement ring holds at least `n` samples or timeout.
    ///
    /// Parks this thread and relies on the JACK process callback to unpark
    /// us as soon as it pushes new samples. A 10 ms `park_timeout` is still
    /// used as a safety net so a missed wake (e.g. waker slot cleared
    /// between unpark attempts) can't deadlock.
    fn wait_ring(&mut self, n: usize, duration: f64) -> Result<()> {
        let timeout = Instant::now() + Duration::from_secs_f64(duration + 2.0);

        // Fast path: data may already be present.
        if let Some(ref c) = self.ring_cons {
            if c.occupied_len() >= n { return Ok(()); }
        }

        *self.state.waker.lock().unwrap() = Some(std::thread::current());
        let result = loop {
            if let Some(ref c) = self.ring_cons {
                if c.occupied_len() >= n { break Ok(()); }
            }
            if Instant::now() > timeout {
                break Err(anyhow::anyhow!("capture timeout after {duration:.1}s"));
            }
            std::thread::park_timeout(Duration::from_millis(10));
        };
        *self.state.waker.lock().unwrap() = None;
        result
    }
}

impl AudioEngine for JackEngine {
    fn start(&mut self, output_ports: &[String], input_port: Option<&str>) -> Result<()> {
        let (client, _status) = Client::new("ac-daemon", ClientOptions::NO_START_SERVER)
            .context("JACK client")?;

        self.sample_rate = client.sample_rate() as u32;

        let out_port = client.register_port("out", AudioOut::default()).context("register out")?;
        let in_port  = client.register_port("in",  AudioIn::default()) .context("register in")?;

        // Publish an initial silent 1-second tone buffer at the real sample rate.
        self.state.tone_buf.store(Arc::new(vec![0.0f32; self.sample_rate as usize]));
        self.state.silence.store(true, Ordering::Relaxed);

        // Split SPSC rings: producer → RT callback, consumer → worker thread.
        let rb = HeapRb::<f32>::new(RING_CAPACITY);
        let (ring_prod, ring_cons) = rb.split();
        self.ring_cons = Some(ring_cons);

        // Pre-allocated slots for on-demand ref inputs. No ports are
        // registered up front; `add_ref_input` ships a (port, prod) pair
        // through `ref_add_*` and the RT handler appends to these Vecs.
        let in_ref_ports:   Vec<jack::Port<AudioIn>> = Vec::with_capacity(MAX_REF_INPUTS);
        let ring_ref_prods: Vec<HeapProd<f32>>       = Vec::with_capacity(MAX_REF_INPUTS);
        self.ring_ref_cons = Vec::with_capacity(MAX_REF_INPUTS);
        self.ref_ports     = Vec::with_capacity(MAX_REF_INPUTS);

        let (ref_add_prod, ref_add_cons) =
            HeapRb::<RefAdd>::new(REF_ADD_QUEUE_CAPACITY).split();
        self.ref_add_prod = Some(ref_add_prod);

        let process = Process {
            out_port, in_port, in_ref_ports,
            state: self.state.clone(),
            tone_pos: 0,
            ring_prod,
            ring_ref_prods,
            ref_add_cons,
        };
        let async_client = client
            .activate_async(Notifications { state: self.state.clone() }, process)
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
        self.ring_cons     = None;
        self.ring_ref_cons.clear();
        self.ref_ports.clear();
        self.ref_add_prod  = None;
    }

    fn sample_rate(&self) -> u32 { self.sample_rate }

    fn set_tone(&mut self, freq_hz: f64, amplitude: f64) {
        let buf = generate_sine_1s(freq_hz, amplitude, self.sample_rate);
        self.state.tone_buf.store(Arc::new(buf));
        self.state.silence.store(false, Ordering::Relaxed);
    }

    fn set_pink(&mut self, amplitude: f64) {
        let buf = generate_pink_noise(amplitude, self.sample_rate);
        self.state.tone_buf.store(Arc::new(buf));
        self.state.silence.store(false, Ordering::Relaxed);
    }

    fn set_silence(&mut self) {
        self.state.silence.store(true, Ordering::Relaxed);
    }

    fn capture_block(&mut self, duration: f64) -> Result<Vec<f32>> {
        let n_needed = (self.sample_rate as f64 * duration) as usize;

        if let Some(ref mut c) = self.ring_cons { c.clear(); }

        self.wait_ring(n_needed, duration)?;

        let mut samples = vec![0.0f32; n_needed];
        let got = self.ring_cons.as_mut()
            .map(|c| c.pop_slice(&mut samples))
            .unwrap_or(0);
        samples.truncate(got);
        Ok(samples)
    }

    fn capture_available(&mut self, max_samples: usize) -> Result<Vec<f32>> {
        let mut samples = vec![0.0f32; max_samples];
        let got = self.ring_cons.as_mut()
            .map(|c| c.pop_slice(&mut samples))
            .unwrap_or(0);
        samples.truncate(got);
        Ok(samples)
    }

    fn capture_stereo(&mut self, duration: f64) -> Result<(Vec<f32>, Vec<f32>)> {
        let n_needed = (self.sample_rate as f64 * duration) as usize;

        if let Some(ref mut c) = self.ring_cons { c.clear(); }
        if let Some(c) = self.ring_ref_cons.get_mut(0) { c.clear(); }

        self.wait_ring(n_needed, duration)?;

        let mut meas = vec![0.0f32; n_needed];
        let got_m = self.ring_cons.as_mut()
            .map(|c| c.pop_slice(&mut meas))
            .unwrap_or(0);
        meas.truncate(got_m);

        let mut refch = vec![0.0f32; n_needed];
        let got_r = self.ring_ref_cons.get_mut(0)
            .map(|c| c.pop_slice(&mut refch))
            .unwrap_or(0);
        // Pad if ref port was disconnected / silent.
        for s in refch.iter_mut().skip(got_r) { *s = 0.0; }

        Ok((meas, refch))
    }

    fn capture_multi(&mut self, duration: f64) -> Result<Vec<Vec<f32>>> {
        let n_needed = (self.sample_rate as f64 * duration) as usize;

        if let Some(ref mut c) = self.ring_cons { c.clear(); }
        for c in self.ring_ref_cons.iter_mut() { c.clear(); }

        self.wait_ring(n_needed, duration)?;

        let mut out = Vec::with_capacity(1 + self.ring_ref_cons.len());

        let mut meas = vec![0.0f32; n_needed];
        let got = self.ring_cons.as_mut()
            .map(|c| c.pop_slice(&mut meas))
            .unwrap_or(0);
        meas.truncate(got);
        out.push(meas);

        for c in self.ring_ref_cons.iter_mut() {
            let mut buf = vec![0.0f32; n_needed];
            let got = c.pop_slice(&mut buf);
            for s in buf.iter_mut().skip(got) { *s = 0.0; }
            out.push(buf);
        }
        Ok(out)
    }

    fn reconnect_input(&mut self, port: &str) -> Result<()> {
        if let Some(ref ac) = self._async_client {
            let in_name = ac.as_client().name().to_string() + ":in";
            if let Some(ref old) = self.input_port {
                ac.as_client().disconnect_ports_by_name(old, &in_name).ok();
            }
            ac.as_client().connect_ports_by_name(port, &in_name)
                .context("reconnect_input")?;
            if let Some(ref mut c) = self.ring_cons { c.clear(); }
            self.input_port = Some(port.to_string());
        }
        Ok(())
    }

    fn add_ref_input(&mut self, port: &str) -> Result<()> {
        let Some(ref ac) = self._async_client else { return Ok(()); };

        // Idempotent: the transfer handler may register the same source port
        // twice when two pairs share a REF channel.
        if self.ref_ports.iter().any(|p| p == port) {
            return Ok(());
        }

        let idx = self.ref_ports.len();
        if idx >= MAX_REF_INPUTS {
            return Err(anyhow::anyhow!(
                "out of ref input slots (max {MAX_REF_INPUTS})"
            ));
        }

        // Register a fresh port on the live JACK client. Post-activation
        // registration is supported (see jack::AsyncClient docs + upstream
        // `client_cback_calls_port_registered` test).
        let port_name = format!("in_ref_{idx}");
        let new_port = ac.as_client().register_port(&port_name, AudioIn::default())
            .with_context(|| format!("register {port_name}"))?;
        let full_name = ac.as_client().name().to_string() + ":" + &port_name;

        // Build the dedicated SPSC ring and hand the (port, producer) pair
        // to the RT callback via `ref_add_prod`. Capacity of the hand-off
        // queue = MAX_REF_INPUTS, so `try_push` cannot fail under the
        // enforced cap.
        let (prod, cons) = HeapRb::<f32>::new(REF_RING_CAPACITY).split();
        let add = RefAdd { port: new_port, prod };
        let Some(ref mut q) = self.ref_add_prod else {
            return Err(anyhow::anyhow!("add_ref_input before start()"));
        };
        if q.try_push(add).is_err() {
            return Err(anyhow::anyhow!("ref_add queue full"));
        }

        // Wire the external source into our freshly-registered port. The
        // RT handler will start draining into this ring on the next period,
        // so a handful of samples between `connect` and `try_pop` may be
        // discarded — harmless compared to the pre-register approach that
        // polluted JACK's port list with 8 always-on phantom inputs.
        ac.as_client().connect_ports_by_name(port, &full_name)
            .with_context(|| format!("add_ref_input[{idx}] {port} -> {full_name}"))?;

        self.ring_ref_cons.push(cons);
        self.ref_ports.push(port.to_string());
        Ok(())
    }

    fn flush_capture(&mut self) {
        if let Some(ref mut c) = self.ring_cons { c.clear(); }
        for c in self.ring_ref_cons.iter_mut() { c.clear(); }
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

    fn supports_routing(&self) -> bool { true }
    fn backend_name(&self) -> &'static str { "jack" }

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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use ringbuf::HeapRb;

    // ---- fill_tone ----

    #[test]
    fn fill_tone_basic() {
        let tone = [1.0f32, 2.0, 3.0];
        let mut out = [0.0f32; 3];
        let pos = fill_tone(&mut out, &tone, 0);
        assert_eq!(out, [1.0, 2.0, 3.0]);
        assert_eq!(pos, 0); // wraps back to 0
    }

    #[test]
    fn fill_tone_wraps_at_boundary() {
        let tone = [1.0f32, 2.0, 3.0];
        let mut out = [0.0f32; 7];
        let pos = fill_tone(&mut out, &tone, 0);
        assert_eq!(out, [1.0, 2.0, 3.0, 1.0, 2.0, 3.0, 1.0]);
        assert_eq!(pos, 1);
    }

    #[test]
    fn fill_tone_starts_mid_buffer() {
        let tone = [10.0f32, 20.0, 30.0, 40.0];
        let mut out = [0.0f32; 5];
        let pos = fill_tone(&mut out, &tone, 2);
        assert_eq!(out, [30.0, 40.0, 10.0, 20.0, 30.0]);
        assert_eq!(pos, 3);
    }

    #[test]
    fn fill_tone_single_sample_buffer() {
        let tone = [42.0f32];
        let mut out = [0.0f32; 4];
        let pos = fill_tone(&mut out, &tone, 0);
        assert_eq!(out, [42.0; 4]);
        assert_eq!(pos, 0);
    }

    // ---- Ring buffer drain (SPSC) ----

    #[test]
    fn ring_drain_fifo_order() {
        let rb = HeapRb::<f32>::new(64);
        let (mut prod, mut cons) = rb.split();
        let data = [1.0f32, 2.0, 3.0, 4.0, 5.0];
        prod.push_slice(&data);
        assert_eq!(cons.occupied_len(), 5);

        let mut out = [0.0f32; 5];
        let got = cons.pop_slice(&mut out);
        assert_eq!(got, 5);
        assert_eq!(out, data);
    }

    #[test]
    fn ring_drain_partial() {
        let rb = HeapRb::<f32>::new(64);
        let (mut prod, mut cons) = rb.split();
        prod.push_slice(&[1.0, 2.0, 3.0]);

        let mut out = [0.0f32; 5];
        let got = cons.pop_slice(&mut out);
        assert_eq!(got, 3);
        assert_eq!(&out[..3], &[1.0, 2.0, 3.0]);
    }

    #[test]
    fn ring_clear_resets() {
        let rb = HeapRb::<f32>::new(64);
        let (mut prod, mut cons) = rb.split();
        prod.push_slice(&[1.0, 2.0]);
        cons.clear();
        assert_eq!(cons.occupied_len(), 0);
    }

    // ---- Stereo ref-channel padding ----

    #[test]
    fn stereo_ref_padding() {
        let n_needed = 100;
        let rb_meas = HeapRb::<f32>::new(256);
        let rb_ref  = HeapRb::<f32>::new(256);
        let (mut mp, mut mc) = rb_meas.split();
        let (mut rp, mut rc) = rb_ref.split();

        // Push full meas, partial ref
        let meas_data: Vec<f32> = (0..n_needed).map(|i| i as f32).collect();
        mp.push_slice(&meas_data);
        let ref_data: Vec<f32> = (0..40).map(|i| (i as f32) * 10.0).collect();
        rp.push_slice(&ref_data);

        let mut meas = vec![0.0f32; n_needed];
        let got_m = mc.pop_slice(&mut meas);
        meas.truncate(got_m);

        let mut refch = vec![0.0f32; n_needed];
        let got_r = rc.pop_slice(&mut refch);
        for s in refch.iter_mut().skip(got_r) { *s = 0.0; }

        assert_eq!(meas.len(), n_needed);
        assert_eq!(refch.len(), n_needed);
        assert_eq!(got_r, 40);
        // First 40 samples are from ref, rest are zero-padded
        assert_eq!(&refch[..40], &ref_data[..]);
        assert!(refch[40..].iter().all(|&s| s == 0.0));
    }

    // ---- SharedState / xruns ----

    #[test]
    fn xrun_counter_increments() {
        let state = Arc::new(SharedState {
            tone_buf: ArcSwap::new(Arc::new(vec![0.0f32; 48_000])),
            silence:  AtomicBool::new(true),
            xruns:    AtomicUsize::new(0),
            waker:    Mutex::new(None),
        });
        assert_eq!(state.xruns.load(Ordering::Relaxed), 0);
        state.xruns.fetch_add(1, Ordering::Relaxed);
        state.xruns.fetch_add(1, Ordering::Relaxed);
        assert_eq!(state.xruns.load(Ordering::Relaxed), 2);
    }

    // ---- ArcSwap tone buffer swap ----

    #[test]
    fn tone_buf_swap_is_visible() {
        let state = SharedState {
            tone_buf: ArcSwap::new(Arc::new(vec![0.0f32; 4])),
            silence:  AtomicBool::new(false),
            xruns:    AtomicUsize::new(0),
            waker:    Mutex::new(None),
        };
        let mut out = [0.0f32; 2];
        fill_tone(&mut out, &state.tone_buf.load(), 0);
        assert_eq!(out, [0.0, 0.0]);

        state.tone_buf.store(Arc::new(vec![5.0f32, 6.0, 7.0]));
        let mut out2 = [0.0f32; 3];
        fill_tone(&mut out2, &state.tone_buf.load(), 0);
        assert_eq!(out2, [5.0, 6.0, 7.0]);
    }
}
