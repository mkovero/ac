use std::sync::{Arc, Mutex};
use triple_buffer::{triple_buffer, Input, Output};

use super::types::{DisplayConfig, DisplayFrame, FrameMeta, SpectrumFrame, TransferFrame};

struct ChannelSlot {
    buffer: Output<SpectrumFrame>,
    averaged: Vec<f32>,
    last_freqs_len: usize,
    has_data: bool,
    last_frame_id: u64,
}

impl ChannelSlot {
    fn new(buffer: Output<SpectrumFrame>) -> Self {
        Self {
            buffer,
            averaged: Vec::new(),
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
            self.averaged = frame.spectrum.clone();
            self.last_freqs_len = n;
        }

        let is_fresh = frame.frame_id != 0 && frame.frame_id != self.last_frame_id;
        if is_fresh {
            self.last_frame_id = frame.frame_id;
        }

        if n > 0 {
            let alpha = config.averaging_alpha.clamp(0.0, 1.0);
            if alpha >= 0.999 || self.averaged.len() != n {
                self.averaged = frame.spectrum.clone();
            } else {
                for (dst, src) in self.averaged.iter_mut().zip(frame.spectrum.iter()) {
                    *dst = alpha * *src + (1.0 - alpha) * *dst;
                }
            }
            self.has_data = true;
        }

        let new_row = if is_fresh && n > 0 {
            Some(self.averaged.clone())
        } else {
            None
        };

        Some(DisplayFrame {
            spectrum: self.averaged.clone(),
            freqs: frame.freqs.clone(),
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
/// update rate is ~0.4 fps and the payload is small (≤ 2000 points × 4 lanes).
#[derive(Clone, Default)]
pub struct TransferStore {
    inner: Arc<Mutex<Option<TransferFrame>>>,
}

impl TransferStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn write(&self, frame: TransferFrame) {
        if let Ok(mut g) = self.inner.lock() {
            *g = Some(frame);
        }
    }

    pub fn read(&self) -> Option<TransferFrame> {
        self.inner.lock().ok().and_then(|g| g.clone())
    }

    pub fn clear(&self) {
        if let Ok(mut g) = self.inner.lock() {
            *g = None;
        }
    }
}
