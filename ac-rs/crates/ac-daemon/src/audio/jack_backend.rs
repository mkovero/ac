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
use ringbuf::traits::{Consumer, Producer, Split};
use ringbuf::{HeapCons, HeapProd, HeapRb};

use super::rings::CaptureRings;
use super::AudioEngine;
use ac_core::shared::generator::{generate_pink_noise, generate_sine_1s};

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
    silence: AtomicBool,
    xruns: AtomicUsize,
    /// Bumped by JACK's `ports_connected` notification. The worker re-reads
    /// `get_connections()` only when this moves, so steady state costs one
    /// atomic load per frame instead of a graph query at the tick rate
    /// (#205 ruling 2). The callback itself only bumps the counter — decoding
    /// port IDs there would be work in a notification handler.
    graph_epoch: AtomicUsize,
    // Consumer (wait_ring) parks on its own thread handle; the RT process
    // callback does a `try_lock().unpark()` after pushing samples so the
    // waiter wakes within microseconds of data arriving instead of polling
    // on a 10 ms sleep.
    waker: Mutex<Option<Thread>>,
    // One-shot playback for `play_and_capture` (Farina IR stimulus). When
    // `one_shot_active` is set, the RT callback fills `out_buf` from
    // `one_shot_buf[one_shot_pos..]` and advances `one_shot_pos`, ignoring
    // the looping tone. When the buffer is exhausted it clears
    // `one_shot_active` and falls back to silence.
    one_shot_buf: ArcSwap<Vec<f32>>,
    one_shot_pos: AtomicUsize,
    one_shot_active: AtomicBool,
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
    sample_rate: u32,
    state: Arc<SharedState>,
    /// Consumer halves plus the shared clear/wait/pop ordering. One SPSC
    /// consumer per active ref input, in insertion order, parallel to
    /// `ref_ports`; grows as `add_ref_input` is called, and the RT handler
    /// owns the matching producers and drains new ones via `ref_add_prod`.
    rings: CaptureRings,
    _async_client: Option<jack::AsyncClient<Notifications, Process>>,
    output_ports: Vec<String>,
    input_port: Option<String>,
    /// Active ref source ports in insertion order; parallel to
    /// `ring_ref_cons`. Unlike the old slot-based design, there are no
    /// `None` holes — ports are only appended, never re-slotted.
    ref_ports: Vec<String>,
    /// Main-side producer of the on-demand port hand-off queue. `None`
    /// between `stop()` and the next `start()`.
    ref_add_prod: Option<HeapProd<RefAdd>>,
    /// Last observed connection list for our `out` port, and the `graph_epoch`
    /// it was read at. Re-read only when the epoch moves.
    conn_cache: Option<Vec<String>>,
    conn_cache_epoch: Option<usize>,
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
                silence: AtomicBool::new(true),
                xruns: AtomicUsize::new(0),
                graph_epoch: AtomicUsize::new(0),
                waker: Mutex::new(None),
                one_shot_buf: ArcSwap::new(Arc::new(Vec::new())),
                one_shot_pos: AtomicUsize::new(0),
                one_shot_active: AtomicBool::new(false),
            }),
            rings: CaptureRings::new(),
            _async_client: None,
            output_ports: Vec::new(),
            input_port: None,
            ref_ports: Vec::new(),
            ref_add_prod: None,
            conn_cache: None,
            conn_cache_epoch: None,
        }
    }
}

// -----------------------------------------------------------------------

struct Process {
    out_port: jack::Port<AudioOut>,
    in_port: jack::Port<AudioIn>,
    /// Ref capture ports, grown on-demand from the `ref_add_cons` queue.
    /// Pre-allocated to `MAX_REF_INPUTS` so RT-side `push` never reallocates.
    in_ref_ports: Vec<jack::Port<AudioIn>>,
    state: Arc<SharedState>,
    tone_pos: usize,
    ring_prod: HeapProd<f32>,
    /// Parallel to `in_ref_ports`, same pre-allocated capacity.
    ring_ref_prods: Vec<HeapProd<f32>>,
    /// Receives port hand-offs from the main thread; drained each period.
    ref_add_cons: HeapCons<RefAdd>,
}

/// Fill `out` from `tone`, wrapping at buffer boundary. Returns updated position.
fn fill_tone(out: &mut [f32], tone: &[f32], mut pos: usize) -> usize {
    let n = tone.len();
    for s in out.iter_mut() {
        *s = tone[pos];
        pos += 1;
        if pos >= n {
            pos = 0;
        }
    }
    pos
}

/// Fill `out` from `buf` starting at `pos`, without wrapping. Zero-pads
/// the tail if `buf` runs out mid-fill. Returns `(new_pos, exhausted)`.
fn fill_one_shot(out: &mut [f32], buf: &[f32], pos: usize) -> (usize, bool) {
    let remaining = buf.len().saturating_sub(pos);
    let n = out.len().min(remaining);
    if n > 0 {
        out[..n].copy_from_slice(&buf[pos..pos + n]);
    }
    for s in &mut out[n..] {
        *s = 0.0;
    }
    let new_pos = pos + n;
    (new_pos, new_pos >= buf.len())
}

impl jack::ProcessHandler for Process {
    fn process(&mut self, _: &Client, scope: &ProcessScope) -> Control {
        let out_buf = self.out_port.as_mut_slice(scope);
        let in_buf = self.in_port.as_slice(scope);

        if self.state.one_shot_active.load(Ordering::Acquire) {
            let buf = self.state.one_shot_buf.load();
            let pos = self.state.one_shot_pos.load(Ordering::Relaxed);
            let (new_pos, done) = fill_one_shot(out_buf, &buf, pos);
            self.state.one_shot_pos.store(new_pos, Ordering::Relaxed);
            if done {
                self.state.one_shot_active.store(false, Ordering::Release);
            }
        } else if self.state.silence.load(Ordering::Relaxed) {
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
            if let Some(t) = guard.as_ref() {
                t.unpark();
            }
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

    /// Any graph edge changing anywhere invalidates our cached view of our own
    /// output connections. Deliberately does not decode the port IDs: this is a
    /// notification callback, and a counter bump is all it needs to do. The
    /// worker resolves what actually changed, lazily, off this thread.
    fn ports_connected(
        &mut self,
        _: &Client,
        _port_id_a: jack::PortId,
        _port_id_b: jack::PortId,
        _are_connected: bool,
    ) {
        self.state.graph_epoch.fetch_add(1, Ordering::Relaxed);
    }
}

// -----------------------------------------------------------------------

/// Wait until the measurement ring holds at least `n` samples or timeout.
///
/// Parks this thread and relies on the JACK process callback to unpark us as
/// soon as it pushes new samples. A 10 ms `park_timeout` is still used as a
/// safety net so a missed wake (e.g. waker slot cleared between unpark
/// attempts) can't deadlock.
///
/// Returned as a closure rather than a method so `CaptureRings` can own the
/// clear/wait/pop ordering while the wait strategy stays backend-specific —
/// the fake backend's ring-backed mode substitutes a synthetic-clock waiter
/// here. Logic is unchanged from the former `JackEngine::wait_ring`.
fn park_waiter(state: Arc<SharedState>) -> impl FnMut(&CaptureRings, usize, f64) -> Result<()> {
    move |rings, n, duration| {
        let timeout = Instant::now() + Duration::from_secs_f64(duration + 2.0);

        // Fast path: data may already be present.
        if rings.occupied() >= n {
            return Ok(());
        }

        *state.waker.lock().unwrap() = Some(std::thread::current());
        let result = loop {
            if rings.occupied() >= n {
                break Ok(());
            }
            if Instant::now() > timeout {
                break Err(anyhow::anyhow!("capture timeout after {duration:.1}s"));
            }
            std::thread::park_timeout(Duration::from_millis(10));
        };
        *state.waker.lock().unwrap() = None;
        result
    }
}

impl AudioEngine for JackEngine {
    fn start(&mut self, output_ports: &[String], input_port: Option<&str>) -> Result<()> {
        let (client, _status) =
            Client::new("ac-daemon", ClientOptions::NO_START_SERVER).context("JACK client")?;

        self.sample_rate = client.sample_rate() as u32;

        let out_port = client
            .register_port("out", AudioOut)
            .context("register out")?;
        let in_port = client.register_port("in", AudioIn).context("register in")?;

        // Publish an initial silent 1-second tone buffer at the real sample rate.
        self.state
            .tone_buf
            .store(Arc::new(vec![0.0f32; self.sample_rate as usize]));
        self.state.silence.store(true, Ordering::Relaxed);

        // Split SPSC rings: producer → RT callback, consumer → worker thread.
        let rb = HeapRb::<f32>::new(RING_CAPACITY);
        let (ring_prod, ring_cons) = rb.split();
        self.rings.set_meas(ring_cons);

        // Pre-allocated slots for on-demand ref inputs. No ports are
        // registered up front; `add_ref_input` ships a (port, prod) pair
        // through `ref_add_*` and the RT handler appends to these Vecs.
        let in_ref_ports: Vec<jack::Port<AudioIn>> = Vec::with_capacity(MAX_REF_INPUTS);
        let ring_ref_prods: Vec<HeapProd<f32>> = Vec::with_capacity(MAX_REF_INPUTS);
        self.rings.reserve_refs(MAX_REF_INPUTS);
        self.ref_ports = Vec::with_capacity(MAX_REF_INPUTS);

        let (ref_add_prod, ref_add_cons) = HeapRb::<RefAdd>::new(REF_ADD_QUEUE_CAPACITY).split();
        self.ref_add_prod = Some(ref_add_prod);

        let process = Process {
            out_port,
            in_port,
            in_ref_ports,
            state: self.state.clone(),
            tone_pos: 0,
            ring_prod,
            ring_ref_prods,
            ref_add_cons,
        };
        let async_client = client
            .activate_async(
                Notifications {
                    state: self.state.clone(),
                },
                process,
            )
            .context("JACK activate")?;

        let name = async_client.as_client().name().to_string();
        let out_name = name.clone() + ":out";
        let in_name = name.clone() + ":in";

        for dest in output_ports {
            async_client
                .as_client()
                .connect_ports_by_name(&out_name, dest)
                .ok();
        }
        if let Some(src) = input_port {
            async_client
                .as_client()
                .connect_ports_by_name(src, &in_name)
                .ok();
            self.input_port = Some(src.to_string());
        }

        // Name the client and what it actually connected to. JACK suffixes a
        // duplicate client name (`ac-daemon-86`), so on a box running more than
        // one daemon there is otherwise no way to tell which client is which —
        // and a session's port pattern alone does not identify it, since two
        // daemons sharing a config produce identical patterns. That ambiguity
        // has already cost one misdiagnosis: an edge was torn down on the wrong
        // client and the resulting healthy report read as a stale-cache bug.
        self.output_ports = output_ports.to_vec();
        eprintln!(
            "jack: client {:?} out->{:?} in<-{:?}",
            name, self.output_ports, self.input_port
        );

        self._async_client = Some(async_client);
        Ok(())
    }

    fn stop(&mut self) {
        self._async_client = None;
        self.rings.teardown();
        self.ref_ports.clear();
        self.conn_cache = None;
        self.conn_cache_epoch = None;
        self.ref_add_prod = None;
    }

    fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

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

    fn play_and_capture(&mut self, samples: &[f32], tail_s: f64) -> Result<Vec<f32>> {
        if samples.is_empty() {
            anyhow::bail!("play_and_capture: empty stimulus");
        }
        let sr = self.sample_rate as f64;
        let tail_n = (tail_s.max(0.0) * sr) as usize;
        let n_total = samples.len() + tail_n;

        // Publish buffer and reset position BEFORE enabling — the RT
        // callback only reads one_shot_buf / one_shot_pos once it sees
        // one_shot_active=true (Acquire on the flag synchronises with the
        // Release store below).
        self.state.one_shot_buf.store(Arc::new(samples.to_vec()));
        self.state.one_shot_pos.store(0, Ordering::Relaxed);
        self.state.silence.store(true, Ordering::Relaxed);
        // Not counted as a discard: this is the start of a one-shot
        // measurement, not a per-tick splice.
        self.rings.clear_meas_uncounted();
        self.state.one_shot_active.store(true, Ordering::Release);

        let duration_s = n_total as f64 / sr;
        let mut waiter = park_waiter(self.state.clone());
        let wait = waiter(&self.rings, n_total, duration_s + 2.0);

        // Ensure RT stops consuming one-shot even if we bailed early.
        self.state.one_shot_active.store(false, Ordering::Release);

        wait?;

        Ok(self.rings.capture_available(n_total))
    }

    fn capture_block(&mut self, duration: f64) -> Result<Vec<f32>> {
        let n_needed = (self.sample_rate as f64 * duration) as usize;
        let mut waiter = park_waiter(self.state.clone());
        self.rings.capture_block(n_needed, duration, &mut waiter)
    }

    fn capture_available(&mut self, max_samples: usize) -> Result<Vec<f32>> {
        Ok(self.rings.capture_available(max_samples))
    }

    fn capture_stereo(&mut self, duration: f64) -> Result<(Vec<f32>, Vec<f32>)> {
        let n_needed = (self.sample_rate as f64 * duration) as usize;
        let mut waiter = park_waiter(self.state.clone());
        self.rings.capture_stereo(n_needed, duration, &mut waiter)
    }

    fn capture_multi(&mut self, duration: f64) -> Result<Vec<Vec<f32>>> {
        let n_needed = (self.sample_rate as f64 * duration) as usize;
        let mut waiter = park_waiter(self.state.clone());
        self.rings.capture_multi(n_needed, duration, &mut waiter)
    }

    fn observed_output_connections(&mut self) -> Option<Vec<String>> {
        let ac = self._async_client.as_ref()?;
        let epoch = self.state.graph_epoch.load(Ordering::Relaxed);
        if self.conn_cache_epoch != Some(epoch) {
            // `jack_port_get_connections` is valid for ports our own client
            // registered, which `out` is — looked up by name rather than held
            // as a second handle, since the `Port` was moved into the RT
            // process handler.
            let name = ac.as_client().name().to_string() + ":out";
            let conns = ac
                .as_client()
                .port_by_name(&name)
                .map(|p| p.get_connections())
                .unwrap_or_default();
            self.conn_cache = Some(conns);
            self.conn_cache_epoch = Some(epoch);
        }
        self.conn_cache.clone()
    }

    fn capture_multi_contiguous(&mut self, duration: f64) -> Result<Vec<Vec<f32>>> {
        let n_needed = (self.sample_rate as f64 * duration) as usize;
        let mut waiter = park_waiter(self.state.clone());
        self.rings
            .capture_multi_contiguous(n_needed, duration, &mut waiter)
    }

    fn discarded_samples(&self) -> u64 {
        self.rings.discarded_samples()
    }

    fn last_drain_occupancy(&self) -> Vec<usize> {
        self.rings.last_drain_occupancy().to_vec()
    }

    fn reconnect_input(&mut self, port: &str) -> Result<()> {
        if let Some(ref ac) = self._async_client {
            let in_name = ac.as_client().name().to_string() + ":in";
            if let Some(ref old) = self.input_port {
                ac.as_client().disconnect_ports_by_name(old, &in_name).ok();
            }
            ac.as_client()
                .connect_ports_by_name(port, &in_name)
                .context("reconnect_input")?;
            // Uncounted: routing switch, not a per-tick splice. Counting it
            // here would make `discarded_samples` report input churn as
            // capture discontinuity.
            self.rings.clear_meas_uncounted();
            self.input_port = Some(port.to_string());
        }
        Ok(())
    }

    fn add_ref_input(&mut self, port: &str) -> Result<()> {
        let Some(ref ac) = self._async_client else {
            return Ok(());
        };

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
        let new_port = ac
            .as_client()
            .register_port(&port_name, AudioIn)
            .with_context(|| format!("register {port_name}"))?;
        let full_name = ac.as_client().name().to_string() + ":" + &port_name;

        // Build the dedicated SPSC ring and hand the (port, producer) pair
        // to the RT callback via `ref_add_prod`. Capacity of the hand-off
        // queue = MAX_REF_INPUTS, so `try_push` cannot fail under the
        // enforced cap.
        let (prod, cons) = HeapRb::<f32>::new(REF_RING_CAPACITY).split();
        let add = RefAdd {
            port: new_port,
            prod,
        };
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
        ac.as_client()
            .connect_ports_by_name(port, &full_name)
            .with_context(|| format!("add_ref_input[{idx}] {port} -> {full_name}"))?;

        self.rings.push_ref(cons);
        self.ref_ports.push(port.to_string());
        Ok(())
    }

    fn flush_capture(&mut self) {
        // Uncounted: an explicit caller-requested flush, not a per-tick
        // discard. See `CaptureRings::clear_meas_uncounted`.
        self.rings.flush_all_uncounted();
    }

    fn connect_output(&mut self, port: &str) -> Result<()> {
        if let Some(ref ac) = self._async_client {
            let out_name = ac.as_client().name().to_string() + ":out";
            ac.as_client()
                .connect_ports_by_name(&out_name, port)
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
            ac.as_client()
                .disconnect_ports_by_name(&out_name, port)
                .ok();
        }
        self.output_ports.retain(|p| p != port);
    }

    fn xruns(&self) -> u32 {
        self.state.xruns.load(Ordering::Relaxed) as u32
    }

    fn supports_routing(&self) -> bool {
        true
    }
    fn backend_name(&self) -> &'static str {
        "jack"
    }

    fn playback_ports(&self) -> Vec<String> {
        // IS_INPUT | IS_PHYSICAL — JACK's `IS_INPUT` flag is "audio
        // flows INTO this port", so without `IS_PHYSICAL` the query
        // also returns the daemon's own `ac-daemon:in`, any
        // PipeWire/PulseAudio bridge sinks, Ardour bus inputs, etc.
        // The user's "channel N" mental model is "Nth physical output
        // jack on the soundcard", so PHYSICAL is the right filter.
        let flags = jack::PortFlags::IS_INPUT | jack::PortFlags::IS_PHYSICAL;
        if let Some(ref ac) = self._async_client {
            ac.as_client()
                .ports(None, Some("32 bit float mono audio"), flags)
        } else if let Ok((c, _)) = Client::new("ac-daemon-probe", ClientOptions::NO_START_SERVER) {
            c.ports(None, Some("32 bit float mono audio"), flags)
        } else {
            Vec::new()
        }
    }

    fn capture_ports(&self) -> Vec<String> {
        // Symmetric to playback_ports — only hardware capture ports
        // (excludes the daemon's own `ac-daemon:out`, virtual sources,
        // bridge clients, etc.).
        let flags = jack::PortFlags::IS_OUTPUT | jack::PortFlags::IS_PHYSICAL;
        if let Some(ref ac) = self._async_client {
            ac.as_client()
                .ports(None, Some("32 bit float mono audio"), flags)
        } else if let Ok((c, _)) = Client::new("ac-daemon-probe", ClientOptions::NO_START_SERVER) {
            c.ports(None, Some("32 bit float mono audio"), flags)
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
    // `Observer` is no longer needed at module scope (occupancy is polled
    // through `CaptureRings`), but these tests still inspect raw consumers.
    use ringbuf::traits::Observer;
    use ringbuf::HeapRb;

    // ---- fill_one_shot ----

    #[test]
    fn one_shot_fills_full_buffer() {
        let buf = [1.0f32, 2.0, 3.0, 4.0, 5.0];
        let mut out = [0.0f32; 3];
        let (pos, done) = fill_one_shot(&mut out, &buf, 0);
        assert_eq!(out, [1.0, 2.0, 3.0]);
        assert_eq!(pos, 3);
        assert!(!done);
    }

    #[test]
    fn one_shot_exhausts_and_pads_zero() {
        let buf = [1.0f32, 2.0, 3.0];
        let mut out = [9.0f32; 5];
        let (pos, done) = fill_one_shot(&mut out, &buf, 0);
        assert_eq!(out, [1.0, 2.0, 3.0, 0.0, 0.0]);
        assert_eq!(pos, 3);
        assert!(done);
    }

    #[test]
    fn one_shot_resumes_from_mid_position() {
        let buf = [1.0f32, 2.0, 3.0, 4.0];
        let mut out = [0.0f32; 2];
        let (pos, done) = fill_one_shot(&mut out, &buf, 2);
        assert_eq!(out, [3.0, 4.0]);
        assert_eq!(pos, 4);
        assert!(done);
    }

    #[test]
    fn one_shot_with_empty_buffer_emits_silence() {
        let buf: &[f32] = &[];
        let mut out = [9.0f32; 3];
        let (pos, done) = fill_one_shot(&mut out, buf, 0);
        assert_eq!(out, [0.0; 3]);
        assert_eq!(pos, 0);
        assert!(done);
    }

    #[test]
    fn one_shot_position_past_end_emits_silence() {
        let buf = [1.0f32, 2.0];
        let mut out = [9.0f32; 3];
        let (pos, done) = fill_one_shot(&mut out, &buf, 5);
        assert_eq!(out, [0.0; 3]);
        assert_eq!(pos, 5);
        assert!(done);
    }

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

    // ---- Hardware runbook (capture-contiguity D4, partial) ----

    /// Confirms on **real JACK** that `capture_multi`'s pre-wait `clear()`
    /// discards live audio between ticks — the hardware half of
    /// `handoff-capture-contiguity.md` H1, which the headless reproducer in
    /// `audio/contiguity.rs` can only model.
    ///
    /// **Emits nothing.** Capture only: no output ports are connected and no
    /// generator is started, so this is safe to run against an interface with
    /// unknown equipment downstream and needs no stimulus consent.
    ///
    /// `#[ignore]`d — needs a running JACK server, same convention as
    /// `tests/it_loopback_ir.rs`. Run with:
    ///
    /// ```text
    /// AC_TEST_CAPTURE_PORT='Babyface Pro Pro:capture_1' \
    ///   cargo test --bin ac-daemon -- --ignored --nocapture jack_capture_multi
    /// ```
    /// The #207 fix, on real JACK: `capture_multi_contiguous` must discard
    /// nothing under the same conditions where `capture_multi` discards
    /// 318 samples per tick, and must still keep up — so the audio it returns
    /// covers the full elapsed wall time rather than a fixed slice.
    ///
    /// **Emits nothing.** Capture only, same as the runbook below.
    #[test]
    #[ignore = "requires a running JACK server; set AC_TEST_CAPTURE_PORT"]
    fn jack_contiguous_drain_discards_nothing_and_keeps_up() {
        const TICKS: u64 = 40;
        const CHUNK_SECS: f64 = 0.05;
        const PROCESS: Duration = Duration::from_millis(5);

        let port = std::env::var("AC_TEST_CAPTURE_PORT")
            .unwrap_or_else(|_| "system:capture_1".to_string());

        let mut eng = JackEngine::new();
        eng.start(&[], Some(&port)).expect("JACK start");
        let sr = eng.sample_rate();

        let _ = eng.capture_multi(0.2); // warm-up flush, as the worker does
        let baseline = eng.discarded_samples();

        let mut total: usize = 0;
        for _ in 0..TICKS {
            let bufs = eng
                .capture_multi_contiguous(CHUNK_SECS)
                .expect("capture_multi_contiguous");
            total += bufs[0].len();
            std::thread::sleep(PROCESS);
        }
        let discarded = eng.discarded_samples() - baseline;
        eng.stop();

        // Wall time actually elapsed per tick: the blocking wait plus the
        // modelled processing. A drain that keeps up returns all of it.
        let per_tick_wall = CHUNK_SECS + PROCESS.as_secs_f64();
        let expected = (sr as f64 * per_tick_wall * TICKS as f64) as usize;
        eprintln!(
            "sr={sr} discarded={discarded} captured={total} samples ({:.2} s) \
             vs {expected} expected from {TICKS} x {per_tick_wall:.3} s wall",
            total as f64 / sr as f64
        );

        assert_eq!(
            discarded, 0,
            "the contiguous drain must not clear the ring — {discarded} discarded \
             means the splice is back"
        );
        // Within 10%: the first tick has no accrued surplus, and JACK period
        // quantisation moves a few hundred samples either way.
        assert!(
            total as f64 > expected as f64 * 0.9,
            "returned {total} samples over {TICKS} ticks but {expected} of wall \
             time elapsed — the drain is falling behind (#208 on this path)"
        );
    }

    #[test]
    #[ignore = "requires a running JACK server; set AC_TEST_CAPTURE_PORT"]
    fn jack_capture_multi_discards_live_audio_between_ticks() {
        const TICKS: u64 = 40;
        const CHUNK_SECS: f64 = 0.05;
        // Stands in for the transfer worker's per-tick compute (~5 ms with
        // the delay cached). This is the interval during which the ring keeps
        // filling and which the next `clear()` throws away.
        const PROCESS: Duration = Duration::from_millis(5);

        let port = std::env::var("AC_TEST_CAPTURE_PORT")
            .unwrap_or_else(|_| "system:capture_1".to_string());

        let mut eng = JackEngine::new();
        eng.start(&[], Some(&port)).expect("JACK start");
        let sr = eng.sample_rate();

        // Warm up past the connect transient before measuring.
        let _ = eng.capture_multi(0.2);
        let baseline = eng.discarded_samples();

        for _ in 0..TICKS {
            eng.capture_multi(CHUNK_SECS).expect("capture_multi");
            std::thread::sleep(PROCESS);
        }

        let discarded = eng.discarded_samples() - baseline;
        let backend = eng.backend_name();
        eng.stop();

        let per_tick = discarded as f64 / TICKS as f64;
        let expected_per_tick = sr as f64 * PROCESS.as_secs_f64();
        eprintln!(
            "backend={backend} sr={sr} port={port}\n\
             discarded {discarded} samples over {TICKS} ticks \
             = {per_tick:.0}/tick ({:.1} ms), expected ~{expected_per_tick:.0} \
             from a {:.0} ms processing gap",
            per_tick / sr as f64 * 1000.0,
            PROCESS.as_secs_f64() * 1000.0,
        );

        assert!(
            discarded > 0,
            "pre-wait clear() discarded nothing on real JACK over {TICKS} ticks — \
             either the ring never filled during the {PROCESS:?} gap, or H1 does \
             not hold on this backend. Both are findings; do not delete this test."
        );
    }

    // ---- Stereo ref-channel padding ----

    #[test]
    fn stereo_ref_padding() {
        let n_needed = 100;
        let rb_meas = HeapRb::<f32>::new(256);
        let rb_ref = HeapRb::<f32>::new(256);
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
        for s in refch.iter_mut().skip(got_r) {
            *s = 0.0;
        }

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
            silence: AtomicBool::new(true),
            xruns: AtomicUsize::new(0),
            graph_epoch: AtomicUsize::new(0),
            waker: Mutex::new(None),
            one_shot_buf: ArcSwap::new(Arc::new(Vec::new())),
            one_shot_pos: AtomicUsize::new(0),
            one_shot_active: AtomicBool::new(false),
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
            silence: AtomicBool::new(false),
            xruns: AtomicUsize::new(0),
            graph_epoch: AtomicUsize::new(0),
            waker: Mutex::new(None),
            one_shot_buf: ArcSwap::new(Arc::new(Vec::new())),
            one_shot_pos: AtomicUsize::new(0),
            one_shot_active: AtomicBool::new(false),
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
