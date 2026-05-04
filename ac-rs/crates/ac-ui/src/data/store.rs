use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use triple_buffer::{triple_buffer, Input, Output};

use std::collections::{HashMap, VecDeque};

use super::types::{
    DisplayConfig, DisplayFrame, FrameMeta, LoudnessReadout, ScopeFrame, SpectrumFrame, SweepDone,
    SweepPoint, TransferFrame, TransferPair,
};

struct ChannelSlot {
    buffer: Output<SpectrumFrame>,
    /// Shared with any DisplayFrame the app is still holding from the previous
    /// tick. Mutated via `Arc::make_mut` so that when the app dropped the old
    /// frame before calling `read_all` the refcount is 1 and mutation is free.
    averaged: Arc<Vec<f32>>,
    cached_freqs: Option<Arc<Vec<f32>>>,
    last_freqs_len: usize,
    has_data: bool,
    last_frame_id: u64,
}

impl ChannelSlot {
    fn new(buffer: Output<SpectrumFrame>) -> Self {
        Self {
            buffer,
            averaged: Arc::new(Vec::new()),
            cached_freqs: None,
            last_freqs_len: 0,
            has_data: false,
            last_frame_id: 0,
        }
    }

    fn read(&mut self, config: &DisplayConfig) -> Option<DisplayFrame> {
        let frame = self.buffer.read();
        let n = frame.spectrum.len();
        if n == 0 {
            if !self.has_data {
                return None;
            }
        } else if frame.freqs.len() != n {
            return None;
        }

        if n != self.last_freqs_len {
            self.averaged = Arc::new(frame.spectrum.clone());
            self.last_freqs_len = n;
        }

        let is_fresh = frame.frame_id != 0 && frame.frame_id != self.last_frame_id;
        if is_fresh {
            self.last_frame_id = frame.frame_id;
        }

        if n > 0 {
            let alpha = config.averaging_alpha.clamp(0.0, 1.0);
            if alpha >= 0.999 || self.averaged.len() != n {
                self.averaged = Arc::new(frame.spectrum.clone());
            } else {
                let buf = Arc::make_mut(&mut self.averaged);
                for (dst, src) in buf.iter_mut().zip(frame.spectrum.iter()) {
                    *dst = alpha * *src + (1.0 - alpha) * *dst;
                }
            }
            self.has_data = true;
        }

        // Daemon produces freqs deterministically from (N, sr), so keying the
        // cache on length is enough: same length ⇒ same bin grid in practice.
        let freqs = match self.cached_freqs.as_ref() {
            Some(a) if a.len() == frame.freqs.len() => a.clone(),
            _ => {
                let a = Arc::new(frame.freqs.clone());
                self.cached_freqs = Some(a.clone());
                a
            }
        };

        let new_row = if is_fresh && n > 0 {
            Some(self.averaged.clone())
        } else {
            None
        };

        Some(DisplayFrame {
            spectrum: self.averaged.clone(),
            freqs,
            meta: FrameMeta::from(frame),
            new_row,
        })
    }
}

pub struct ChannelStore {
    channels: Vec<ChannelSlot>,
}

impl ChannelStore {
    pub fn new(n_channels: usize) -> (Vec<Input<SpectrumFrame>>, Self) {
        let mut inputs = Vec::with_capacity(n_channels);
        let mut channels = Vec::with_capacity(n_channels);
        for _ in 0..n_channels {
            let (input, output) = triple_buffer(&SpectrumFrame::default());
            inputs.push(input);
            channels.push(ChannelSlot::new(output));
        }
        (inputs, Self { channels })
    }

    pub fn len(&self) -> usize {
        self.channels.len()
    }

    pub fn read_all(&mut self, config: &DisplayConfig) -> Vec<Option<DisplayFrame>> {
        self.channels.iter_mut().map(|c| c.read(config)).collect()
    }
}

/// Shared latest-H1 slot. Receiver writes, main thread reads. Mutex is fine:
/// update rate is a few Hz and the payload is small (≤ 2000 points × 4 lanes).
/// `serial` increments on every write so consumers can detect freshness
/// without diffing payloads (the virtual-channel waterfall renderer uses
/// this to decide when to scroll in a new row).
#[derive(Default)]
struct TransferInner {
    frame: Option<TransferFrame>,
    serial: u64,
}

#[derive(Clone, Default)]
pub struct TransferStore {
    inner: Arc<Mutex<TransferInner>>,
}

impl TransferStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn write(&self, frame: TransferFrame) {
        if let Ok(mut g) = self.inner.lock() {
            g.frame = Some(frame);
            g.serial = g.serial.wrapping_add(1);
        }
    }

    pub fn read(&self) -> Option<TransferFrame> {
        self.inner.lock().ok().and_then(|g| g.frame.clone())
    }

    /// Current frame paired with its monotonic write serial. Callers that
    /// render scrolling views keep a last-seen serial and treat any increase
    /// as a fresh row.
    pub fn read_with_serial(&self) -> (u64, Option<TransferFrame>) {
        self.inner
            .lock()
            .ok()
            .map(|g| (g.serial, g.frame.clone()))
            .unwrap_or((0, None))
    }

    pub fn clear(&self) {
        if let Ok(mut g) = self.inner.lock() {
            g.frame = None;
        }
    }
}

/// Shared list of virtual transfer channels. Each registered pair gets its
/// own `TransferStore`; the receiver demuxes incoming transfer frames by
/// `(meas_channel, ref_channel)` into the matching slot. Cheap-clone via
/// `Arc` so the main thread and the receiver thread share the same list.
#[derive(Clone, Default)]
pub struct VirtualChannelStore {
    inner: Arc<Mutex<Vec<(TransferPair, TransferStore)>>>,
}

impl VirtualChannelStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Snapshot the currently-registered pairs, in insertion order.
    pub fn pairs(&self) -> Vec<TransferPair> {
        self.inner
            .lock()
            .ok()
            .map(|g| g.iter().map(|(p, _)| *p).collect())
            .unwrap_or_default()
    }

    /// Register a new virtual channel. Returns `false` if the pair was
    /// already present (caller can use the return value to toggle).
    pub fn add(&self, pair: TransferPair) -> bool {
        if let Ok(mut g) = self.inner.lock() {
            if g.iter().any(|(p, _)| *p == pair) {
                return false;
            }
            g.push((pair, TransferStore::new()));
            true
        } else {
            false
        }
    }

    /// Unregister a virtual channel. Returns `true` if a matching pair was
    /// removed, `false` if it wasn't present.
    pub fn remove(&self, pair: TransferPair) -> bool {
        if let Ok(mut g) = self.inner.lock() {
            if let Some(i) = g.iter().position(|(p, _)| *p == pair) {
                g.remove(i);
                return true;
            }
        }
        false
    }

    // Retained: exercised by unit tests below; no production caller yet.
    #[allow(dead_code)]
    pub fn store_for(&self, pair: TransferPair) -> Option<TransferStore> {
        self.inner.lock().ok().and_then(|g| {
            g.iter()
                .find(|(p, _)| *p == pair)
                .map(|(_, s)| s.clone())
        })
    }

    /// Receiver-side dispatch: write `frame` into the slot matching
    /// `(frame.meas_channel, frame.ref_channel)`. Silently drops frames for
    /// pairs that were unregistered between daemon dispatch and arrival.
    pub fn write(&self, pair: TransferPair, frame: TransferFrame) {
        if let Ok(g) = self.inner.lock() {
            if let Some((_, s)) = g.iter().find(|(p, _)| *p == pair) {
                s.write(frame);
            }
        }
    }

    /// Snapshot every pair with its current frame and write serial. Consumers
    /// that scroll waterfalls keep per-pair last-seen serials and emit a new
    /// row only when the serial increases.
    pub fn read_all_with_serial(
        &self,
    ) -> Vec<(TransferPair, u64, Option<TransferFrame>)> {
        self.inner
            .lock()
            .ok()
            .map(|g| {
                g.iter()
                    .map(|(p, s)| {
                        let (serial, frame) = s.read_with_serial();
                        (*p, serial, frame)
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn len(&self) -> usize {
        self.inner.lock().ok().map(|g| g.len()).unwrap_or(0)
    }

    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    // Retained: exercised by unit tests below; no production caller yet.
    #[allow(dead_code)]
    pub fn clear(&self) {
        if let Ok(mut g) = self.inner.lock() {
            g.clear();
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct SweepState {
    pub points: Vec<SweepPoint>,
    pub done: Option<SweepDone>,
}

#[derive(Clone, Default)]
pub struct SweepStore {
    inner: Arc<Mutex<SweepState>>,
}

impl SweepStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&self, point: SweepPoint) {
        if let Ok(mut g) = self.inner.lock() {
            g.points.push(point);
        }
    }

    pub fn set_done(&self, done: SweepDone) {
        if let Ok(mut g) = self.inner.lock() {
            g.done = Some(done);
        }
    }

    pub fn read(&self) -> SweepState {
        self.inner
            .lock()
            .ok()
            .map(|g| g.clone())
            .unwrap_or_default()
    }
}

/// Latest BS.1770-5 / R128 readout per monitored channel. Written by the
/// receiver on each `measurement/loudness` frame; read by the overlay
/// each redraw. Cleared on receiver shutdown / source swap.
#[derive(Clone, Default)]
pub struct LoudnessStore {
    inner: Arc<Mutex<HashMap<u32, LoudnessReadout>>>,
}

impl LoudnessStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn write(&self, channel: u32, readout: LoudnessReadout) {
        if let Ok(mut g) = self.inner.lock() {
            g.insert(channel, readout);
        }
    }

    pub fn read(&self, channel: u32) -> Option<LoudnessReadout> {
        self.inner.lock().ok().and_then(|g| g.get(&channel).copied())
    }

    pub fn clear(&self) {
        if let Ok(mut g) = self.inner.lock() {
            g.clear();
        }
    }
}

/// Per-channel ring of raw audio samples consumed by Goniometer /
/// PhaseScope3D (`unified.md` Phase 0b). Keyed by *physical* channel id
/// straight from the wire (no slot-allocation), mirroring `LoudnessStore`
/// so per-physical-channel late-arriving streams (e.g. `--channels
/// 10,11`) just work without preallocating slots in `ChannelStore`'s
/// triple-buffer scheme.
///
/// Capacity is fixed; trim from the back (oldest). At the design
/// rate (~50 ticks/sec × ~800 samples/tick) the ring spans ~170 ms
/// at 48 kHz — enough to absorb several render-frame's worth of
/// jitter without unbounded growth.
pub const SCOPE_RING_CAP: usize = 8192;

struct ScopeChannel {
    samples: VecDeque<f32>,
    sr: u32,
    last_frame_idx: u64,
    last_recv_at: Option<Instant>,
}

#[derive(Clone, Default)]
pub struct ScopeStore {
    inner: Arc<Mutex<HashMap<u32, ScopeChannel>>>,
}

impl ScopeStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn write(&self, frame: ScopeFrame) {
        if let Ok(mut g) = self.inner.lock() {
            let entry = g.entry(frame.channel).or_insert_with(|| ScopeChannel {
                samples: VecDeque::with_capacity(SCOPE_RING_CAP),
                sr: frame.sr,
                last_frame_idx: 0,
                last_recv_at: None,
            });
            entry.sr = frame.sr;
            entry.last_frame_idx = frame.frame_idx;
            entry.last_recv_at = Some(Instant::now());
            entry.samples.extend(frame.samples.iter().copied());
            while entry.samples.len() > SCOPE_RING_CAP {
                entry.samples.pop_front();
            }
        }
    }

    /// Pull the freshest `n` samples for `channel`. `None` if the channel
    /// hasn't been seen, the last receive was more than `max_age` ago,
    /// or the ring has fewer than `n` samples buffered. Returns
    /// `(sr, last_frame_idx, samples)`. Caller pairs L+R by comparing
    /// `last_frame_idx` between channels.
    pub fn read_recent(
        &self,
        channel: u32,
        n: usize,
        max_age: Duration,
    ) -> Option<(u32, u64, Vec<f32>)> {
        let g = self.inner.lock().ok()?;
        let ch = g.get(&channel)?;
        if ch.last_recv_at.is_none_or(|t| t.elapsed() > max_age) {
            return None;
        }
        if ch.samples.len() < n || n == 0 {
            return None;
        }
        let start = ch.samples.len() - n;
        // VecDeque iterators are O(1) per step; collecting a tail of
        // size n is O(n) and contiguous — good enough at the rates
        // we operate (≤4096 samples per frame at 60 fps).
        let tail: Vec<f32> = ch.samples.iter().skip(start).copied().collect();
        Some((ch.sr, ch.last_frame_idx, tail))
    }

    pub fn clear(&self) {
        if let Ok(mut g) = self.inner.lock() {
            g.clear();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pair(m: u32, r: u32) -> TransferPair {
        TransferPair { meas: m, ref_ch: r }
    }

    fn sample_frame(meas: u32, ref_ch: u32) -> TransferFrame {
        TransferFrame {
            freqs: vec![100.0, 200.0],
            magnitude_db: vec![-1.0, -2.0],
            phase_deg: vec![0.0, 10.0],
            coherence: vec![0.95, 0.90],
            delay_samples: 0,
            delay_ms: 0.0,
            meas_channel: meas,
            ref_channel: ref_ch,
            sr: 48000,
        }
    }

    #[test]
    fn virtual_channel_add_remove_toggle() {
        let store = VirtualChannelStore::new();
        assert!(store.is_empty());

        assert!(store.add(pair(0, 3)));
        assert!(!store.add(pair(0, 3))); // duplicate → false
        assert_eq!(store.len(), 1);

        assert!(store.add(pair(1, 3)));
        assert_eq!(store.pairs(), vec![pair(0, 3), pair(1, 3)]);

        assert!(store.remove(pair(0, 3)));
        assert!(!store.remove(pair(0, 3))); // gone → false
        assert_eq!(store.pairs(), vec![pair(1, 3)]);
    }

    #[test]
    fn virtual_channel_write_dispatches_by_pair() {
        let store = VirtualChannelStore::new();
        store.add(pair(0, 3));
        store.add(pair(1, 3));

        store.write(pair(1, 3), sample_frame(1, 3));
        let s = store.store_for(pair(1, 3)).unwrap();
        let f = s.read().unwrap();
        assert_eq!(f.meas_channel, 1);
        assert_eq!(f.ref_channel, 3);

        // Unregistered pair → write is silently dropped.
        store.write(pair(9, 9), sample_frame(9, 9));
        assert!(store.store_for(pair(9, 9)).is_none());
    }

    #[test]
    fn virtual_channel_clear_empties() {
        let store = VirtualChannelStore::new();
        store.add(pair(0, 3));
        store.add(pair(1, 3));
        store.clear();
        assert!(store.is_empty());
    }

    fn scope_frame(channel: u32, frame_idx: u64, samples: Vec<f32>) -> ScopeFrame {
        ScopeFrame {
            channel,
            sr: 48_000,
            frame_idx,
            samples,
            n_channels: Some(2),
        }
    }

    #[test]
    fn scope_store_round_trip_returns_recent_tail() {
        let store = ScopeStore::new();
        store.write(scope_frame(0, 1, vec![0.1, 0.2, 0.3, 0.4]));
        let (sr, idx, tail) = store
            .read_recent(0, 3, Duration::from_secs(1))
            .expect("recent samples");
        assert_eq!(sr, 48_000);
        assert_eq!(idx, 1);
        assert_eq!(tail, vec![0.2, 0.3, 0.4]);
    }

    #[test]
    fn scope_store_caps_at_ring_size() {
        let store = ScopeStore::new();
        // Push more than SCOPE_RING_CAP across many frames.
        let chunk: Vec<f32> = (0..1000).map(|i| i as f32 / 1000.0).collect();
        for k in 0..(SCOPE_RING_CAP / 1000 + 5) {
            store.write(scope_frame(0, k as u64, chunk.clone()));
        }
        // The ring must stay bounded — read_recent at exactly cap must
        // succeed; at cap+1 must fail.
        assert!(store
            .read_recent(0, SCOPE_RING_CAP, Duration::from_secs(1))
            .is_some());
        assert!(store
            .read_recent(0, SCOPE_RING_CAP + 1, Duration::from_secs(1))
            .is_none());
    }

    #[test]
    fn scope_store_read_recent_returns_none_when_stale() {
        let store = ScopeStore::new();
        store.write(scope_frame(0, 1, vec![0.1, 0.2, 0.3]));
        // max_age = 0 means *every* prior write counts as stale.
        assert!(store
            .read_recent(0, 2, Duration::from_secs(0))
            .is_none());
    }

    #[test]
    fn scope_store_pairs_l_and_r_via_frame_idx() {
        let store = ScopeStore::new();
        store.write(scope_frame(0, 42, vec![0.1, 0.2, 0.3, 0.4]));
        store.write(scope_frame(1, 42, vec![-0.1, -0.2, -0.3, -0.4]));
        let (_, fi_l, _) = store.read_recent(0, 4, Duration::from_secs(1)).unwrap();
        let (_, fi_r, _) = store.read_recent(1, 4, Duration::from_secs(1)).unwrap();
        assert_eq!(fi_l, fi_r, "frame_idx must match across L and R within a tick");
    }
}
