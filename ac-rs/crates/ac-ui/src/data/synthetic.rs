use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use triple_buffer::Input;
use winit::event_loop::EventLoopProxy;

use super::store::{TransferStore, VirtualChannelStore};
use super::types::{SpectrumFrame, TransferFrame, TransferPair};

pub struct SyntheticSource {
    pub n_channels: usize,
    pub n_bins: usize,
    pub update_hz: f32,
    pub transfer: TransferStore,
    pub virtual_channels: VirtualChannelStore,
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
    pub fn spawn(
        self,
        mut inputs: Vec<Input<SpectrumFrame>>,
        wake: Option<EventLoopProxy<()>>,
    ) -> SyntheticHandle {
        assert_eq!(inputs.len(), self.n_channels);
        let stop = Arc::new(AtomicBool::new(false));
        let stop_c = stop.clone();
        let transfer_store = self.transfer.clone();
        let virtual_channels = self.virtual_channels.clone();
        let join = thread::spawn(move || {
            let period = Duration::from_secs_f32(1.0 / self.update_hz.max(0.1));
            let freqs = make_log_freqs(self.n_bins, 20.0, 24000.0);
            let transfer_freqs = make_log_freqs(1000, 20.0, 20000.0);
            let start = Instant::now();
            let mut frame_idx: u64 = 0;
            let transfer_period = Duration::from_millis(2500);
            let mut last_transfer = Instant::now() - transfer_period;
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
                        dbu_offset_db: None,
                        peaks: Vec::new(),
                        spl_offset_db: None,
                        mic_correction: None,
                        sr: 48000,
                        clipping: false,
                        xruns: 0,
                        channel: Some(ch as u32),
                        n_channels: Some(self.n_channels as u32),
                        frame_id: frame_idx + 1,
                        leq_duration_s: None,
                    };
                    input.write(frame);
                }
                frame_idx += 1;
                if let Some(ref p) = wake {
                    let _ = p.send_event(());
                }

                // Synthetic transfer: emit one frame per registered virtual
                // channel, plus the latest (first pair) also goes into the
                // global `TransferStore` so the legacy L-transfer view keeps
                // working. Rate-limited to roughly match the daemon's
                // `capture_duration(4, sr)` cadence so the overlay delay
                // readout behaves the same way.
                if last_transfer.elapsed() >= transfer_period {
                    let pairs = virtual_channels.pairs();
                    for (i, p) in pairs.iter().enumerate() {
                        let tf = synth_transfer(&transfer_freqs, p.meas, p.ref_ch, t);
                        virtual_channels.write(
                            TransferPair { meas: p.meas, ref_ch: p.ref_ch },
                            tf.clone(),
                        );
                        if i == 0 {
                            transfer_store.write(tf);
                        }
                    }
                    if !pairs.is_empty() {
                        last_transfer = Instant::now();
                    }
                }

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

/// Fake H1 transfer function. Shape: roughly flat passband with a gentle
/// 2nd-order resonance bump around 2 kHz and a 1st-order low-pass rolloff
/// above ~15 kHz. Phase is the minimum-phase contribution of those poles
/// plus a linear-phase term from a slowly-varying delay. Coherence is high
/// (>0.9) with band-edge dips. Everything drifts slowly in `t` so the
/// display looks live.
fn synth_transfer(freqs: &[f32], meas: u32, refc: u32, t: f32) -> TransferFrame {
    let n = freqs.len();
    let mut mag = Vec::with_capacity(n);
    let mut phase = Vec::with_capacity(n);
    let mut coh = Vec::with_capacity(n);

    // Delay drifts slowly so the Δt readout visibly updates. ~±3 samples
    // at 48 kHz (~60 µs) — same order of magnitude as a real room
    // measurement with a few-metre mic distance.
    let sr = 48000.0_f32;
    let delay_samples_f = 3.0 * (t * 0.3).sin();
    let delay_sec = delay_samples_f / sr;

    let f_res = 2000.0 + 50.0 * (t * 0.1).sin();
    let q = 4.0;
    let f_lp = 15000.0;

    let mut rng = XorShift::new(
        0xA17E ^ meas ^ (refc << 8) ^ ((t * 10.0) as u32),
    );

    for &f in freqs {
        // Resonance: 2nd-order bandpass bump, +3 dB peak at f_res.
        let x = (f / f_res) - (f_res / f);
        let res_mag = 1.0 / (1.0 + (q * x).powi(2)).sqrt();
        let res_db = 3.0 * res_mag;
        let res_phase = -((q * x).atan()).to_degrees();

        // 1st-order low-pass at f_lp.
        let r = f / f_lp;
        let lp_mag_db = -10.0 * (1.0 + r * r).log10();
        let lp_phase = -r.atan().to_degrees();

        // Linear phase from delay (wraps to ±180).
        let lin_phase = -360.0 * f * delay_sec;

        let jitter = (rng.next_f32() - 0.5) * 0.3;
        mag.push(res_db + lp_mag_db + jitter);
        let total_phase = (res_phase + lp_phase + lin_phase).rem_euclid(360.0) - 180.0;
        phase.push(total_phase);

        // Coherence: 0.95 centre, dips near band edges.
        let edge = ((f.log10() - 3.0) / 2.0).abs();
        let c = (0.97 - 0.2 * edge * edge).clamp(0.3, 0.999);
        coh.push(c + (rng.next_f32() - 0.5) * 0.01);
    }

    TransferFrame {
        freqs: freqs.to_vec(),
        magnitude_db: mag,
        phase_deg: phase,
        coherence: coh,
        delay_samples: delay_samples_f.round() as i64,
        delay_ms: delay_samples_f * 1000.0 / sr,
        meas_channel: meas,
        ref_channel: refc,
        sr: sr as u32,
    }
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
