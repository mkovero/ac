use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use triple_buffer::Input;

use super::types::SpectrumFrame;

pub struct SyntheticSource {
    pub n_channels: usize,
    pub n_bins: usize,
    pub update_hz: f32,
}

pub struct SyntheticHandle {
    stop: Arc<AtomicBool>,
    join: Option<thread::JoinHandle<()>>,
}

impl Drop for SyntheticHandle {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.join.take() {
            let _ = h.join();
        }
    }
}

impl SyntheticSource {
    pub fn spawn(self, mut inputs: Vec<Input<SpectrumFrame>>) -> SyntheticHandle {
        assert_eq!(inputs.len(), self.n_channels);
        let stop = Arc::new(AtomicBool::new(false));
        let stop_c = stop.clone();
        let join = thread::spawn(move || {
            let period = Duration::from_secs_f32(1.0 / self.update_hz.max(0.1));
            let freqs = make_log_freqs(self.n_bins, 20.0, 24000.0);
            let start = Instant::now();
            let mut frame_idx: u64 = 0;
            while !stop_c.load(Ordering::Relaxed) {
                let t = start.elapsed().as_secs_f32();
                for (ch, input) in inputs.iter_mut().enumerate() {
                    let spectrum = synth_spectrum(&freqs, ch, t, frame_idx);
                    let (fund_bin, fund_db) = find_peak(&spectrum);
                    let fund_hz = freqs.get(fund_bin).copied().unwrap_or(1000.0);
                    let frame = SpectrumFrame {
                        freqs: freqs.clone(),
                        spectrum,
                        freq_hz: fund_hz,
                        fundamental_dbfs: fund_db,
                        thd_pct: 0.01 + 0.005 * ((t + ch as f32).sin() + 1.0),
                        thdn_pct: 0.02 + 0.005 * ((t + ch as f32).cos() + 1.0),
                        in_dbu: None,
                        sr: 48000,
                        clipping: false,
                        xruns: 0,
                        channel: Some(ch as u32),
                        n_channels: Some(self.n_channels as u32),
                        frame_id: frame_idx + 1,
                    };
                    input.write(frame);
                }
                frame_idx += 1;
                thread::sleep(period);
            }
        });
        SyntheticHandle {
            stop,
            join: Some(join),
        }
    }
}

fn make_log_freqs(n: usize, fmin: f32, fmax: f32) -> Vec<f32> {
    if n == 0 {
        return Vec::new();
    }
    let lo = fmin.max(1.0).ln();
    let hi = fmax.max(lo.exp() * 1.01).ln();
    (0..n)
        .map(|i| {
            let t = i as f32 / (n - 1).max(1) as f32;
            (lo + (hi - lo) * t).exp()
        })
        .collect()
}

fn synth_spectrum(freqs: &[f32], ch: usize, t: f32, frame_idx: u64) -> Vec<f32> {
    let mut rng = XorShift::new(0xC0FFEE ^ (frame_idx as u32) ^ (ch as u32 * 0x9E37));
    let drift = ((t * 0.3 + ch as f32).sin()) * 3.0;
    let fund = 1000.0 * (1.0 + 0.01 * ((t * 0.1 + ch as f32).sin()));
    let mut out = Vec::with_capacity(freqs.len());
    for &f in freqs {
        let pink = -3.0 * (f / 20.0).log10();
        let noise = (rng.next_f32() - 0.5) * 6.0;
        let mut v = -50.0 + pink + noise + drift;
        for k in 1..=5 {
            let fk = fund * k as f32;
            let bw = fk * 0.004;
            let dist = (f - fk).abs();
            if dist < bw * 5.0 {
                let gauss = (-(dist * dist) / (2.0 * bw * bw)).exp();
                let level = match k {
                    1 => -3.0,
                    2 => -45.0,
                    3 => -55.0,
                    4 => -65.0,
                    _ => -75.0,
                };
                v = v.max(level + 10.0 * gauss - 10.0);
                if k == 1 && gauss > 0.5 {
                    v = level;
                }
            }
        }
        out.push(v.clamp(-140.0, 0.0));
    }
    out
}

fn find_peak(spectrum: &[f32]) -> (usize, f32) {
    spectrum
        .iter()
        .enumerate()
        .fold((0usize, -140.0_f32), |(bi, bv), (i, &v)| {
            if v > bv {
                (i, v)
            } else {
                (bi, bv)
            }
        })
}

struct XorShift {
    state: u32,
}

impl XorShift {
    fn new(seed: u32) -> Self {
        Self {
            state: if seed == 0 { 0xDEADBEEF } else { seed },
        }
    }
    fn next_u32(&mut self) -> u32 {
        let mut x = self.state;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.state = x;
        x
    }
    fn next_f32(&mut self) -> f32 {
        (self.next_u32() as f32) / (u32::MAX as f32)
    }
}
