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

use super::rings::CaptureRings;
use super::AudioEngine;
use crate::handlers::MAX_STIMULUS_DURATION_S;
use ac_core::shared::generator::{generate_pink_noise, generate_sine_1s};

/// Capture-ring capacity, in samples, needed to hold the full `plot_ir`
/// budget (`handlers::MAX_STIMULUS_DURATION_S` applies independently to
/// both `duration` and `tail_s`, so `play_and_capture_cancellable` can be
/// asked for up to twice that combined) at a given sample rate.
///
/// Before #437's rig verification the ring was a fixed `16 * 192_000` —
/// "comfortably larger than any single capture request" was true only
/// because nothing yet validated a request up to the 60 s budget; a rig run
/// at 96 kHz (`rig-2026-09-14-pr437-plot-budget`, finding 1) showed a
/// within-budget `plot_ir` request play its full stimulus and then time out
/// with no IR, because 60 s alone already exceeded that ring's 32 s
/// capacity at that rate. The fix after that (`120 * 192_000`, sized to the
/// budget at the project's then-assumed highest rate) reintroduced the same
/// class of bug one level up: `start()` accepts JACK's actual live sample
/// rate with no ceiling, and a rig running above 192 kHz (e.g. the 384 kHz
/// path `ac_core::visualize::mtw::ladder` already exercises) again exceeds
/// a fixed capacity sized only for the assumed worst case (codex-qa, PR
/// #437 at 942c0e27). Computing capacity from the *actual* live rate at
/// `start()` instead of any fixed assumption closes both bugs at once: the
/// ring always fits the accepted budget, at whatever rate JACK reports. See
/// `meas_ring_capacity_fits_stimulus_duration_and_tail_budget_at_every_rate`
/// below.
fn meas_ring_capacity(sample_rate: u32) -> usize {
    ((MAX_STIMULUS_DURATION_S * 2.0) * sample_rate as f64).ceil() as usize
}

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
    // Reference ports the RT handler has adopted from `ref_add_cons` (#460
    // invariant d): a same-capture reference one-shot is not armed until its
    // port is in the callback's list.
    refs_adopted: AtomicUsize,
    // Set by the consumer to abandon an armed reference one-shot (cancel or
    // timeout). The callback drops it plus any queued start, then clears the
    // flag as the acknowledgement.
    ref_one_shot_abort: AtomicBool,
}

/// Main-thread → RT-handler hand-off for a freshly-registered ref input.
/// The port is registered on the main thread via `AsyncClient::as_client()`
/// and shipped in along with its dedicated SPSC producer, which the RT
/// handler appends to its capture list on the next period.
struct RefAdd {
    port: jack::Port<AudioIn>,
    prod: HeapProd<f32>,
}

/// Main-thread → RT hand-off that arms a same-capture reference one-shot
/// (#460). Everything the callback needs travels in the message, so arming,
/// the stimulus's first output sample and both captures' first samples all
/// happen in the one period that pops it: alignment by construction, not by
/// ordering two statements on the consumer thread against the callback
/// (#467).
struct RefOneShotStart {
    stimulus: Arc<Vec<f32>>,
    ref_index: usize,
    capture_len: usize,
    meas: HeapProd<f32>,
    reference: HeapProd<f32>,
}

/// A reference one-shot in progress, owned by the RT callback.
struct RefOneShot {
    stimulus: Arc<Vec<f32>>,
    pos: usize,
    ref_index: usize,
    remaining: usize,
    meas: HeapProd<f32>,
    reference: HeapProd<f32>,
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
    /// Main-side producer of the reference one-shot hand-off (#460). `None`
    /// between `stop()` and the next `start()`.
    ref_one_shot_prod: Option<HeapProd<RefOneShotStart>>,
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
                waker: Mutex::new(None),
                one_shot_buf: ArcSwap::new(Arc::new(Vec::new())),
                one_shot_pos: AtomicUsize::new(0),
                one_shot_active: AtomicBool::new(false),
                refs_adopted: AtomicUsize::new(0),
                ref_one_shot_abort: AtomicBool::new(false),
            }),
            rings: CaptureRings::new(),
            _async_client: None,
            output_ports: Vec::new(),
            input_port: None,
            ref_ports: Vec::new(),
            ref_add_prod: None,
            ref_one_shot_prod: None,
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
    /// The reference one-shot currently running, if any (#460).
    ref_one_shot: Option<RefOneShot>,
    /// Receives reference one-shot starts; popped at the top of a period.
    ref_one_shot_cons: HeapCons<RefOneShotStart>,
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
/// One period of a same-capture reference one-shot (#460), factored out of
/// `Process::process` so its alignment is testable without a JACK server.
///
/// When `slot` is empty and `start` arrives, the one-shot begins in *this*
/// period: the stimulus starts filling `out` here, and `meas_in` / `ref_in`
/// from this same period are the first samples of both captures. Pushes stop
/// once `capture_len` samples are in; the slot is then released. A missing
/// `ref_in` (port not adopted) pushes nothing to the reference ring, so the
/// consumer sees a short reference and refuses it rather than padding it.
///
/// Returns `true` when the one-shot owned `out` this period, so the caller
/// must not overwrite it.
fn ref_one_shot_period(
    slot: &mut Option<RefOneShot>,
    start: Option<RefOneShotStart>,
    out: &mut [f32],
    meas_in: &[f32],
    ref_in: Option<&[f32]>,
) -> bool {
    if slot.is_none() {
        if let Some(s) = start {
            *slot = Some(RefOneShot {
                stimulus: s.stimulus,
                pos: 0,
                ref_index: s.ref_index,
                remaining: s.capture_len,
                meas: s.meas,
                reference: s.reference,
            });
        }
    }
    let Some(shot) = slot.as_mut() else {
        return false;
    };
    let (pos, _) = fill_one_shot(out, &shot.stimulus, shot.pos);
    shot.pos = pos;
    let n = meas_in.len().min(shot.remaining);
    shot.meas.push_slice(&meas_in[..n]);
    if let Some(r) = ref_in {
        shot.reference.push_slice(&r[..n.min(r.len())]);
    }
    shot.remaining -= n;
    if shot.remaining == 0 {
        *slot = None;
    }
    true
}

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

        // Drain any pending ref-port additions before reading capture data.
        // Capacity was pre-reserved so `push` does not allocate. Runs before
        // the output fill (it used to run after it): a same-capture reference
        // one-shot (#460) reads its port in this same period, so the port must
        // already be in the list.
        while let Some(add) = self.ref_add_cons.try_pop() {
            if self.in_ref_ports.len() < self.in_ref_ports.capacity() {
                self.in_ref_ports.push(add.port);
                self.ring_ref_prods.push(add.prod);
            }
            // Silently drop if we somehow exceeded capacity — the main-side
            // guard prevents this, but dropping is still RT-safe.
        }
        self.state
            .refs_adopted
            .store(self.in_ref_ports.len(), Ordering::Relaxed);

        // #460: abandon a reference one-shot and any queued start when the
        // consumer asks, then acknowledge. The consumer keeps its ring halves
        // and the stimulus alive until this acknowledgement, so dropping them
        // here only decrements reference counts.
        if self.state.ref_one_shot_abort.load(Ordering::Relaxed) {
            self.ref_one_shot = None;
            while self.ref_one_shot_cons.try_pop().is_some() {}
            self.state
                .ref_one_shot_abort
                .store(false, Ordering::Relaxed);
        }
        let start = if self.ref_one_shot.is_none() {
            self.ref_one_shot_cons.try_pop()
        } else {
            None
        };
        let ref_index = self
            .ref_one_shot
            .as_ref()
            .map(|s| s.ref_index)
            .or_else(|| start.as_ref().map(|s| s.ref_index));
        let ref_in = ref_index
            .and_then(|i| self.in_ref_ports.get(i))
            .map(|p| p.as_slice(scope));
        let ref_shot_owns_output =
            ref_one_shot_period(&mut self.ref_one_shot, start, out_buf, in_buf, ref_in);

        if !ref_shot_owns_output {
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
        // Sized from the live rate JACK just reported, not a fixed
        // assumption (#437, codex-qa at 942c0e27).
        let rb = HeapRb::<f32>::new(meas_ring_capacity(self.sample_rate));
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

        // #460: hand-off for a same-capture reference one-shot. One runs at a
        // time (the worker is serial); the second slot tolerates a start the
        // callback has not yet dropped after an abort.
        let (ref_one_shot_prod, ref_one_shot_cons) = HeapRb::<RefOneShotStart>::new(2).split();
        self.ref_one_shot_prod = Some(ref_one_shot_prod);
        self.state.refs_adopted.store(0, Ordering::Relaxed);

        let process = Process {
            out_port,
            in_port,
            in_ref_ports,
            state: self.state.clone(),
            tone_pos: 0,
            ring_prod,
            ring_ref_prods,
            ref_add_cons,
            ref_one_shot: None,
            ref_one_shot_cons,
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

        self.output_ports = output_ports.to_vec();
        self._async_client = Some(async_client);
        Ok(())
    }

    fn stop(&mut self) {
        self._async_client = None;
        self.rings.teardown();
        self.ref_ports.clear();
        self.ref_add_prod = None;
        self.ref_one_shot_prod = None;
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
        self.play_and_capture_cancellable(samples, tail_s, &AtomicBool::new(false))
    }

    fn play_and_capture_cancellable(
        &mut self,
        samples: &[f32],
        tail_s: f64,
        stop: &AtomicBool,
    ) -> Result<Vec<f32>> {
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
        let timeout = Instant::now() + Duration::from_secs_f64(duration_s + 2.0);
        *self.state.waker.lock().unwrap() = Some(std::thread::current());
        let wait = loop {
            if stop.load(Ordering::Relaxed) {
                self.state.silence.store(true, Ordering::Relaxed);
                break Err(anyhow::anyhow!("play_and_capture cancelled"));
            }
            if self.rings.occupied() >= n_total {
                break Ok(());
            }
            if Instant::now() > timeout {
                break Err(anyhow::anyhow!("capture timeout after {duration_s:.1}s"));
            }
            std::thread::park_timeout(Duration::from_millis(10));
        };
        *self.state.waker.lock().unwrap() = None;

        // Ensure RT stops consuming one-shot even if we bailed early.
        self.state.one_shot_active.store(false, Ordering::Release);

        wait?;

        Ok(self.rings.capture_available(n_total))
    }

    fn supports_reference_capture(&self) -> bool {
        true
    }

    fn play_and_capture_with_reference(
        &mut self,
        samples: &[f32],
        tail_s: f64,
        reference_port: &str,
        stop: &AtomicBool,
    ) -> Result<(Vec<f32>, Vec<f32>)> {
        if samples.is_empty() {
            anyhow::bail!("play_and_capture_with_reference: empty stimulus");
        }
        if self._async_client.is_none() {
            anyhow::bail!("play_and_capture_with_reference before start()");
        }
        self.add_ref_input(reference_port)?;
        let ref_index = self
            .ref_ports
            .iter()
            .position(|p| p == reference_port)
            .ok_or_else(|| anyhow::anyhow!("reference port {reference_port} was not registered"))?;
        // (d) The callback must hold the port before the one-shot is armed.
        let adopt_deadline = Instant::now() + Duration::from_secs(2);
        while self.state.refs_adopted.load(Ordering::Relaxed) <= ref_index {
            if Instant::now() > adopt_deadline {
                anyhow::bail!(
                    "reference port {reference_port} was not adopted by the audio callback \
                     within 2 s"
                );
            }
            std::thread::sleep(Duration::from_millis(5));
        }

        let sr = self.sample_rate as f64;
        let tail_n = (tail_s.max(0.0) * sr) as usize;
        let capture_len = samples.len() + tail_n;
        // (c) Rings sized to this request, which the handler budget bounds.
        let (meas_prod, mut meas_cons) = HeapRb::<f32>::new(capture_len).split();
        let (ref_prod, mut ref_cons) = HeapRb::<f32>::new(capture_len).split();
        let stimulus = Arc::new(samples.to_vec());
        self.state.silence.store(true, Ordering::Relaxed);
        self.state
            .ref_one_shot_abort
            .store(false, Ordering::Relaxed);
        let start = RefOneShotStart {
            stimulus: stimulus.clone(),
            ref_index,
            capture_len,
            meas: meas_prod,
            reference: ref_prod,
        };
        let Some(ref mut queue) = self.ref_one_shot_prod else {
            anyhow::bail!("play_and_capture_with_reference before start()");
        };
        if queue.try_push(start).is_err() {
            anyhow::bail!("reference one-shot queue full");
        }

        let duration_s = capture_len as f64 / sr;
        let timeout = Instant::now() + Duration::from_secs_f64(duration_s + 2.0);
        *self.state.waker.lock().unwrap() = Some(std::thread::current());
        let wait = loop {
            if stop.load(Ordering::Relaxed) {
                break Err(anyhow::anyhow!("play_and_capture cancelled"));
            }
            if meas_cons.occupied_len() >= capture_len && ref_cons.occupied_len() >= capture_len {
                break Ok(());
            }
            if Instant::now() > timeout {
                break Err(anyhow::anyhow!(
                    "capture timeout after {duration_s:.1}s (measurement {} / reference {} of \
                     {capture_len} samples)",
                    meas_cons.occupied_len(),
                    ref_cons.occupied_len()
                ));
            }
            std::thread::park_timeout(Duration::from_millis(10));
        };
        *self.state.waker.lock().unwrap() = None;
        if let Err(e) = wait {
            // Ask the callback to drop the one-shot, and hold the ring halves
            // and stimulus until it acknowledges, so nothing is freed on the
            // RT thread.
            self.state.ref_one_shot_abort.store(true, Ordering::Relaxed);
            let ack_deadline = Instant::now() + Duration::from_millis(500);
            while self.state.ref_one_shot_abort.load(Ordering::Relaxed)
                && Instant::now() < ack_deadline
            {
                std::thread::sleep(Duration::from_millis(5));
            }
            self.state.silence.store(true, Ordering::Relaxed);
            drop(stimulus);
            return Err(e);
        }

        // (a) Exact length on both, never padded.
        let mut meas = vec![0.0f32; capture_len];
        let got_meas = meas_cons.pop_slice(&mut meas);
        let mut reference = vec![0.0f32; capture_len];
        let got_ref = ref_cons.pop_slice(&mut reference);
        drop(stimulus);
        if got_meas != capture_len || got_ref != capture_len {
            anyhow::bail!(
                "reference one-shot returned {got_meas} measurement / {got_ref} reference \
                 samples of {capture_len}"
            );
        }
        Ok((meas, reference))
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

    fn period_size(&self) -> Option<u32> {
        // Queried fresh from the live client, never cached — a running
        // jackd's buffer size can change mid-session (see trait docs).
        self._async_client
            .as_ref()
            .map(|ac| ac.as_client().buffer_size())
    }

    /// Sum of this client's own ports' declared latency ranges (#363).
    ///
    /// Read back by name from the live client, the same way the client's own
    /// name is recovered at `start` — the port objects themselves moved into
    /// `Process` when the callback was activated. The `max` of each range is
    /// taken: JACK reports a range because a port's latency can differ per
    /// path through the graph, and the larger bound is the conservative
    /// account of what the signal traversed.
    ///
    /// An all-zero total reads as `None`, not as a declared zero. jackd
    /// reports `(0, 0)` for ports whose latency nothing has set, and treating
    /// that as a declaration would make two lifecycles "disagree" the moment
    /// one of them happened to be read before the graph settled.
    fn declared_latency_frames(&self) -> Option<u32> {
        let client = self._async_client.as_ref()?.as_client();
        let name = client.name().to_string();
        let out = client.port_by_name(&format!("{name}:out"))?;
        let inp = client.port_by_name(&format!("{name}:in"))?;
        let (_, out_max) = out.get_latency_range(jack::LatencyType::Playback);
        let (_, in_max) = inp.get_latency_range(jack::LatencyType::Capture);
        let total = out_max.saturating_add(in_max);
        (total != 0).then_some(total)
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

    // ---- capture ring vs. protocol budget (#437 rig finding 1; codex-qa
    // live-sample-rate finding at 942c0e27) ----

    #[test]
    fn meas_ring_capacity_fits_stimulus_duration_and_tail_budget_at_every_rate() {
        // `meas_ring_capacity` derives capacity from the *live* rate at
        // `start()` rather than a fixed assumption, so this isn't a
        // coupled-constant check against a hardcoded ceiling any more (the
        // fixed `120 * 192_000` this replaced broke silently above 192 kHz,
        // e.g. the 384 kHz path `mtw::ladder` already exercises — codex-qa,
        // PR #437 at 942c0e27). What's still worth asserting is that the
        // *formula* actually covers the full accepted budget at a rate,
        // including rates above the old fixed ceiling, rather than trusting
        // the arithmetic by inspection alone.
        let max_combined_s = MAX_STIMULUS_DURATION_S * 2.0;
        for sr in [44_100_u32, 48_000, 96_000, 192_000, 384_000] {
            let cap = meas_ring_capacity(sr);
            let n_total = (max_combined_s * sr as f64) as usize;
            assert!(
                cap >= n_total,
                "sr={sr}: {max_combined_s}s combined duration+tail_s needs \
                 {n_total} samples, meas_ring_capacity returned only {cap}"
            );
        }
    }

    // ---- ref_one_shot_period (#460 invariant b; #467) ----

    fn arm_ref_one_shot(
        stimulus: &[f32],
        capture_len: usize,
    ) -> (RefOneShotStart, HeapCons<f32>, HeapCons<f32>) {
        let (meas, meas_cons) = HeapRb::<f32>::new(capture_len).split();
        let (reference, ref_cons) = HeapRb::<f32>::new(capture_len).split();
        (
            RefOneShotStart {
                stimulus: Arc::new(stimulus.to_vec()),
                ref_index: 0,
                capture_len,
                meas,
                reference,
            },
            meas_cons,
            ref_cons,
        )
    }

    fn drain_all(c: &mut HeapCons<f32>) -> Vec<f32> {
        let mut v = vec![0.0f32; c.occupied_len()];
        let n = c.pop_slice(&mut v);
        v.truncate(n);
        v
    }

    /// A hardware loopback with exactly one period of latency feeds both
    /// inputs: what the callback reads in period `p` is what it wrote in
    /// `p - 1`. Whatever period pops the start, both captures must be equal
    /// sample for sample, and the stimulus must land at exactly one period in.
    #[test]
    fn ref_one_shot_aligns_stimulus_and_both_captures_whatever_period_arms_it() {
        const PERIOD: usize = 8;
        let stimulus: Vec<f32> = (1..=20).map(|v| v as f32).collect();
        let capture_len = stimulus.len() + 2 * PERIOD;
        for arm_at in 0..4usize {
            let (start, mut meas_cons, mut ref_cons) = arm_ref_one_shot(&stimulus, capture_len);
            let mut start = Some(start);
            let mut slot: Option<RefOneShot> = None;
            let mut prev_out = vec![0.0f32; PERIOD];
            for p in 0..(arm_at + capture_len / PERIOD + 2) {
                let mut out = vec![0.0f32; PERIOD];
                let msg = if p == arm_at { start.take() } else { None };
                let input = prev_out.clone();
                ref_one_shot_period(&mut slot, msg, &mut out, &input, Some(&input));
                prev_out = out;
            }
            assert!(slot.is_none(), "arm_at {arm_at}: one-shot not released");
            let meas = drain_all(&mut meas_cons);
            let reference = drain_all(&mut ref_cons);
            assert_eq!(meas.len(), capture_len, "arm_at {arm_at}");
            assert_eq!(
                meas, reference,
                "arm_at {arm_at}: measurement and reference must be sample-aligned"
            );
            assert!(
                meas[..PERIOD].iter().all(|&v| v == 0.0),
                "arm_at {arm_at}: {meas:?}"
            );
            assert_eq!(
                &meas[PERIOD..PERIOD + stimulus.len()],
                &stimulus[..],
                "arm_at {arm_at}: stimulus must land exactly one loopback period in"
            );
        }
    }

    /// The rejected implementation, computed here: the single-input one-shot
    /// clears the ring and then enables playback from the consumer thread,
    /// while the callback reads the enable flag at the top of a period and
    /// pushes that period's input at the bottom. When both consumer
    /// statements land inside one callback, a silent period is left at the
    /// head of the capture and the stimulus reads exactly one period late.
    /// `ref_one_shot_period` has no such interleaving: arming is one message
    /// popped at the top of a period.
    #[test]
    fn clear_then_enable_puts_the_stimulus_one_period_late_when_it_races_the_callback() {
        const PERIOD: usize = 8;
        let stimulus: Vec<f32> = (1..=16).map(|v| v as f32).collect();
        let first_stimulus_index = |raced: bool| -> usize {
            let mut ring: Vec<f32> = Vec::new();
            let mut active = false;
            let mut pos = 0usize;
            let mut prev_out = vec![0.0f32; PERIOD];
            for p in 0..6 {
                let checked_active = active; // callback top: flag read
                if p == 1 && raced {
                    // Consumer's clear + enable land inside this callback.
                    ring.clear();
                    active = true;
                }
                let mut out = vec![0.0f32; PERIOD];
                if checked_active {
                    let (np, _) = fill_one_shot(&mut out, &stimulus, pos);
                    pos = np;
                }
                ring.extend_from_slice(&prev_out); // callback bottom: push input
                if p == 1 && !raced {
                    // Consumer runs between callbacks.
                    ring.clear();
                    active = true;
                }
                prev_out = out;
            }
            ring.iter()
                .position(|&v| v == 1.0)
                .expect("stimulus captured")
        };
        let clean = first_stimulus_index(false);
        let raced = first_stimulus_index(true);
        assert_eq!(
            raced,
            clean + PERIOD,
            "the race must shift the stimulus by exactly one period (clean {clean}, raced {raced})"
        );
    }

    /// Invariant (a) on the RT side: with the reference port absent, nothing
    /// is pushed to the reference ring, so the consumer sees a short reference
    /// and refuses it. It is never padded into a full-length buffer.
    #[test]
    fn ref_one_shot_without_its_port_leaves_the_reference_short() {
        const PERIOD: usize = 8;
        let stimulus = vec![1.0f32; PERIOD];
        let capture_len = 3 * PERIOD;
        let (start, meas_cons, ref_cons) = arm_ref_one_shot(&stimulus, capture_len);
        let mut start = Some(start);
        let mut slot: Option<RefOneShot> = None;
        for _ in 0..4 {
            let mut out = vec![0.0f32; PERIOD];
            let input = vec![0.5f32; PERIOD];
            ref_one_shot_period(&mut slot, start.take(), &mut out, &input, None);
        }
        assert!(slot.is_none());
        assert_eq!(meas_cons.occupied_len(), capture_len);
        assert_eq!(
            ref_cons.occupied_len(),
            0,
            "a missing reference port must not be padded into a full-length buffer"
        );
    }

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
            waker: Mutex::new(None),
            one_shot_buf: ArcSwap::new(Arc::new(Vec::new())),
            one_shot_pos: AtomicUsize::new(0),
            one_shot_active: AtomicBool::new(false),
            refs_adopted: AtomicUsize::new(0),
            ref_one_shot_abort: AtomicBool::new(false),
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
            waker: Mutex::new(None),
            one_shot_buf: ArcSwap::new(Arc::new(Vec::new())),
            one_shot_pos: AtomicUsize::new(0),
            one_shot_active: AtomicBool::new(false),
            refs_adopted: AtomicUsize::new(0),
            ref_one_shot_abort: AtomicBool::new(false),
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
