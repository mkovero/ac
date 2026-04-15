use std::time::Instant;
use triple_buffer::{triple_buffer, Input, Output};

use super::types::{DisplayConfig, DisplayFrame, FrameMeta, SpectrumFrame};

const PEAK_DECAY_DB_PER_SEC: f32 = 20.0;
const DB_FLOOR: f32 = -200.0;

struct ChannelSlot {
    buffer: Output<SpectrumFrame>,
    peak_hold: Vec<f32>,
    averaged: Vec<f32>,
    last_peak_update: Instant,
    last_freqs_len: usize,
    has_data: bool,
    last_frame_id: u64,
}

impl ChannelSlot {
    fn new(buffer: Output<SpectrumFrame>) -> Self {
        Self {
            buffer,
            peak_hold: Vec::new(),
            averaged: Vec::new(),
            last_peak_update: Instant::now(),
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
            self.peak_hold = vec![DB_FLOOR; n];
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

            let now = Instant::now();
            let dt = now.duration_since(self.last_peak_update).as_secs_f32();
            self.last_peak_update = now;
            let decay = PEAK_DECAY_DB_PER_SEC * dt;
            for (dst, cur) in self.peak_hold.iter_mut().zip(self.averaged.iter()) {
                let decayed = *dst - decay;
                *dst = decayed.max(*cur);
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
            peak_hold: if config.peak_hold {
                self.peak_hold.clone()
            } else {
                Vec::new()
            },
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
