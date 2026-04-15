//! GPU timestamp queries for per-pass timing.
//!
//! Two render passes (spectrum, egui) → 4 timestamps per frame.
//! Two readback buffers cycled per-frame so the CPU never blocks waiting
//! for the GPU to finish writing the most recent frame's resolves.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

pub const SLOTS: u32 = 4;
pub const SPECTRUM_BEGIN: u32 = 0;
pub const SPECTRUM_END:   u32 = 1;
pub const EGUI_BEGIN:     u32 = 2;
pub const EGUI_END:       u32 = 3;

const READBACK_BYTES: u64 = (SLOTS as u64) * 8;

#[derive(Clone, Copy, Debug, Default)]
pub struct PassTimings {
    pub spectrum_ms: f32,
    pub egui_ms:     f32,
    pub gpu_ms:      f32,
}

pub struct GpuTiming {
    pub query_set: wgpu::QuerySet,
    resolve_buf:   wgpu::Buffer,
    readbacks:     [wgpu::Buffer; 2],
    state:         [Slot; 2],
    write_idx:     usize,
    period_ns:     f32,
    last:          PassTimings,
}

struct Slot {
    in_flight: bool,
    ready:     Arc<AtomicBool>,
}

impl GpuTiming {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Self {
        let query_set = device.create_query_set(&wgpu::QuerySetDescriptor {
            label: Some("ac-ui timing queries"),
            ty: wgpu::QueryType::Timestamp,
            count: SLOTS,
        });
        let resolve_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("ac-ui timing resolve"),
            size: READBACK_BYTES,
            usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let make_readback = |label| device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(label),
            size: READBACK_BYTES,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Self {
            query_set,
            resolve_buf,
            readbacks: [make_readback("ac-ui timing readback 0"), make_readback("ac-ui timing readback 1")],
            state: [
                Slot { in_flight: false, ready: Arc::new(AtomicBool::new(false)) },
                Slot { in_flight: false, ready: Arc::new(AtomicBool::new(false)) },
            ],
            write_idx: 0,
            period_ns: queue.get_timestamp_period(),
            last: PassTimings::default(),
        }
    }

    pub fn spectrum_writes(&self) -> wgpu::RenderPassTimestampWrites<'_> {
        wgpu::RenderPassTimestampWrites {
            query_set: &self.query_set,
            beginning_of_pass_write_index: Some(SPECTRUM_BEGIN),
            end_of_pass_write_index:       Some(SPECTRUM_END),
        }
    }

    pub fn egui_writes(&self) -> wgpu::RenderPassTimestampWrites<'_> {
        wgpu::RenderPassTimestampWrites {
            query_set: &self.query_set,
            beginning_of_pass_write_index: Some(EGUI_BEGIN),
            end_of_pass_write_index:       Some(EGUI_END),
        }
    }

    /// Encode `resolve_query_set` + copy into the active readback buffer.
    /// Call once per frame after the passes that write timestamps but
    /// before submitting the encoder.
    ///
    /// The query set is always drained into `resolve_buf` so the GPU
    /// doesn't keep stale timestamps queued. The copy into the readback
    /// is only encoded when the active slot is free — otherwise we'd be
    /// asking wgpu to write into a still-mapped buffer (validation error)
    /// and the frame would crash. When skipped, `after_submit` also skips
    /// scheduling the map and just rotates, so the slot stays consistent.
    pub fn resolve(&mut self, encoder: &mut wgpu::CommandEncoder) {
        encoder.resolve_query_set(&self.query_set, 0..SLOTS, &self.resolve_buf, 0);
        if !self.state[self.write_idx].in_flight {
            encoder.copy_buffer_to_buffer(
                &self.resolve_buf,
                0,
                &self.readbacks[self.write_idx],
                0,
                READBACK_BYTES,
            );
        }
    }

    /// Schedule the async map of the just-written readback buffer and
    /// rotate. Call once per frame after `queue.submit`.
    pub fn after_submit(&mut self) {
        let idx = self.write_idx;
        if !self.state[idx].in_flight {
            self.state[idx].in_flight = true;
            let ready = self.state[idx].ready.clone();
            self.readbacks[idx].slice(..).map_async(wgpu::MapMode::Read, move |res| {
                if res.is_ok() { ready.store(true, Ordering::Release); }
            });
        }
        self.write_idx ^= 1;
    }

    /// Drain any completed mappings into `self.last` and unmap the buffers.
    /// Call once per frame after `device.poll(Maintain::Poll)`.
    pub fn poll(&mut self) {
        for idx in 0..2 {
            if !self.state[idx].ready.swap(false, Ordering::Acquire) { continue; }
            let buf = &self.readbacks[idx];
            let mut ts = [0u64; SLOTS as usize];
            {
                let view = buf.slice(..).get_mapped_range();
                for (i, chunk) in view.chunks_exact(8).enumerate().take(SLOTS as usize) {
                    ts[i] = u64::from_le_bytes(chunk.try_into().unwrap());
                }
            }
            buf.unmap();
            self.state[idx].in_flight = false;

            let to_ms = |a: u64, b: u64| -> f32 {
                if b <= a { 0.0 } else { (b - a) as f32 * self.period_ns / 1_000_000.0 }
            };
            let spectrum = to_ms(ts[SPECTRUM_BEGIN as usize], ts[SPECTRUM_END as usize]);
            let egui     = to_ms(ts[EGUI_BEGIN     as usize], ts[EGUI_END     as usize]);
            let gpu_total = to_ms(ts[SPECTRUM_BEGIN as usize], ts[EGUI_END    as usize]);
            self.last = PassTimings { spectrum_ms: spectrum, egui_ms: egui, gpu_ms: gpu_total };
        }
    }

    pub fn last(&self) -> PassTimings { self.last }
}
