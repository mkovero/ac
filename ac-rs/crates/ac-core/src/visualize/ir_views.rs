//! Log and ETC views of an impulse response, in dB (Smaart's live IR
//! "Log" and "ETC" displays).
//!
//! - **Log**: `20·log10(|h| / max|h|)`.
//! - **ETC** (energy-time curve): the envelope of the analytic signal,
//!   `|h + j·H{h}|`, in the same dB — the magnitude of the IR with the
//!   oscillation of each arrival removed, so reflections read as the level
//!   they arrive at.
//!
//! Both are normalized to 0 dB at their peak and floored at [`FLOOR_DB`].
//! [`bucket_max`] downsamples a dB curve for display by the **maximum** of
//! each bucket: a stride pick (what the linear IR sidecar does) can step
//! over a peak one sample wide and draw the arrival as absent.

use rustfft::num_complex::Complex;
use rustfft::FftPlanner;

/// The lowest dB either view reports: an exact zero has no logarithm.
pub const FLOOR_DB: f32 = -150.0;

fn to_db(mag: &[f32]) -> Vec<f32> {
    let peak = mag.iter().copied().fold(0.0_f32, f32::max);
    if peak <= 0.0 || !peak.is_finite() {
        return vec![FLOOR_DB; mag.len()];
    }
    mag.iter()
        .map(|&m| {
            let db = 20.0 * (m / peak).log10();
            if db.is_finite() {
                db.max(FLOOR_DB)
            } else {
                FLOOR_DB
            }
        })
        .collect()
}

/// `20·log10(|h| / max|h|)`, floored at [`FLOOR_DB`].
pub fn log_db(ir: &[f32]) -> Vec<f32> {
    let mag: Vec<f32> = ir.iter().map(|v| v.abs()).collect();
    to_db(&mag)
}

/// The ETC: `|analytic(h)|` in dB relative to its peak, floored at
/// [`FLOOR_DB`]. The analytic signal is built in the frequency domain
/// (negative frequencies zeroed, positive doubled), which treats the IR as
/// circular — as it is, coming out of an inverse FFT.
pub fn etc_db(ir: &[f32]) -> Vec<f32> {
    let n = ir.len();
    if n == 0 {
        return Vec::new();
    }
    let mut planner = FftPlanner::<f64>::new();
    let mut buf: Vec<Complex<f64>> = ir
        .iter()
        .map(|&v| Complex::new(f64::from(v), 0.0))
        .collect();
    planner.plan_fft_forward(n).process(&mut buf);
    // h[0] = 1, h[1..n/2] = 2, h[n/2] = 1 when n is even, rest 0.
    let half = n / 2;
    for (k, x) in buf.iter_mut().enumerate() {
        let w = if k == 0 || (n.is_multiple_of(2) && k == half) {
            1.0
        } else if k < n.div_ceil(2) {
            2.0
        } else {
            0.0
        };
        *x *= w;
    }
    planner.plan_fft_inverse(n).process(&mut buf);
    let scale = 1.0 / n as f64;
    let mag: Vec<f32> = buf.iter().map(|z| (z.norm() * scale) as f32).collect();
    to_db(&mag)
}

/// Downsample `db` by the maximum of each `stride`-long bucket, starting at
/// 0 — the same bucket boundaries as `iter().step_by(stride)`, so element
/// `i` covers the linear sample `i` of a stride-picked series.
pub fn bucket_max(db: &[f32], stride: usize) -> Vec<f32> {
    let stride = stride.max(1);
    db.chunks(stride)
        .map(|c| c.iter().copied().fold(f32::NEG_INFINITY, f32::max))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A one-sample arrival between stride picks: the rejected stride
    /// pick draws it at the floor, the bucket maximum at 0 dB.
    #[test]
    fn bucket_max_keeps_a_peak_that_a_stride_pick_steps_over() {
        let mut ir = vec![0.0_f32; 1000];
        ir[503] = 1.0;
        ir[200] = 0.01;
        let db = log_db(&ir);
        let stride = 10;
        let picked: Vec<f32> = db.iter().step_by(stride).copied().collect();
        let kept = bucket_max(&db, stride);
        assert_eq!(picked.len(), kept.len());
        assert_eq!(picked[50], FLOOR_DB, "the stride pick was meant to miss it");
        assert_eq!(kept[50], 0.0);
        assert!((kept[20] - -40.0).abs() < 1e-4);
    }

    /// A tone burst's ETC is its envelope: flat where the tone is, not the
    /// cosine's zero crossings the log view dips into.
    #[test]
    fn the_etc_follows_the_envelope_not_the_oscillation() {
        let n = 4096;
        let ir: Vec<f32> = (0..n)
            .map(|i| {
                if (1000..3000).contains(&i) {
                    (2.0 * std::f32::consts::PI * 0.05 * i as f32).cos()
                } else {
                    0.0
                }
            })
            .collect();
        let etc = etc_db(&ir);
        let log = log_db(&ir);
        // The interior, away from the edges where a truncated burst's
        // envelope rings (and sets the 0 dB peak): flat, where the log
        // view falls into every zero crossing.
        let inside = 1200..2800;
        let spread = |v: &[f32]| {
            let lo = v.iter().copied().fold(f32::MAX, f32::min);
            let hi = v.iter().copied().fold(f32::MIN, f32::max);
            hi - lo
        };
        let etc_spread = spread(&etc[inside.clone()]);
        let log_spread = spread(&log[inside]);
        assert!(
            etc_spread < 0.5,
            "ETC varied {etc_spread} dB inside the burst"
        );
        assert!(
            log_spread > 20.0,
            "the log view was expected to dip, varied {log_spread}"
        );
        assert!(
            etc[100] < -40.0 && etc[3900] < -40.0,
            "ETC high outside the burst"
        );
    }

    #[test]
    fn silence_and_empty_input_floor_instead_of_nan() {
        assert_eq!(log_db(&[0.0; 4]), vec![FLOOR_DB; 4]);
        assert_eq!(etc_db(&[0.0; 4]), vec![FLOOR_DB; 4]);
        assert!(etc_db(&[]).is_empty());
    }
}
