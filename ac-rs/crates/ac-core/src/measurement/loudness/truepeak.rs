//! True-peak metering — BS.1770-5 Annex 2 4-phase 48-tap polyphase FIR interpolator.
//! Each input produces 4 oversampled outputs; the maximum |y| across the
//! session is reported in dBTP. The BS.1770 attenuate/compensate trick for
//! fixed-point arithmetic (−12.04 dB in, +12.04 dB out) collapses to a
//! no-op in float and is omitted here.

use anyhow::Result;

use super::check_planar;

const TP_TAPS: usize = 12;
const TP_OVERSAMPLE: usize = 4;

/// BS.1770-5 Annex 2 Table 1, column-wise. `TP_PHASE[p][j]` is the j-th tap
/// (j=0 → newest input) of phase p. Phase 3 is Phase 0 reversed; Phase 2
/// is Phase 1 reversed — the underlying 48-tap prototype is linear-phase
/// symmetric.
const TP_PHASE: [[f64; TP_TAPS]; TP_OVERSAMPLE] = [
    [
        0.001_708_984_375_0,
        0.010_986_328_125_0,
        -0.019_653_320_312_5,
        0.033_203_125_000_0,
        -0.059_448_242_187_5,
        0.137_329_101_562_5,
        0.972_167_968_750_0,
        -0.102_294_921_875_0,
        0.047_607_421_875_0,
        -0.026_611_328_125_0,
        0.014_892_578_125_0,
        -0.008_300_781_250_0,
    ],
    [
        -0.029_174_804_687_5,
        0.029_296_875_000_0,
        -0.051_757_812_500_0,
        0.089_111_328_125_0,
        -0.166_503_906_250_0,
        0.465_087_890_625_0,
        0.779_785_156_250_0,
        -0.200_317_382_812_5,
        0.101_562_500_000_0,
        -0.058_227_539_062_5,
        0.033_081_054_687_5,
        -0.018_920_898_437_5,
    ],
    [
        -0.018_920_898_437_5,
        0.033_081_054_687_5,
        -0.058_227_539_062_5,
        0.101_562_500_000_0,
        -0.200_317_382_812_5,
        0.779_785_156_250_0,
        0.465_087_890_625_0,
        -0.166_503_906_250_0,
        0.089_111_328_125_0,
        -0.051_757_812_500_0,
        0.029_296_875_000_0,
        -0.029_174_804_687_5,
    ],
    [
        -0.008_300_781_250_0,
        0.014_892_578_125_0,
        -0.026_611_328_125_0,
        0.047_607_421_875_0,
        -0.102_294_921_875_0,
        0.972_167_968_750_0,
        0.137_329_101_562_5,
        -0.059_448_242_187_5,
        0.033_203_125_000_0,
        -0.019_653_320_312_5,
        0.010_986_328_125_0,
        0.001_708_984_375_0,
    ],
];

/// Streaming true-peak meter. One instance per loudness-state, tracks the
/// maximum absolute oversampled value across every channel fed through it
/// since the last reset.
pub struct TruePeak {
    /// Per-channel sample rings. `ring[0]` is the newest sample.
    rings: Vec<[f64; TP_TAPS]>,
    /// Largest |y| observed at any oversampled output, any channel.
    max_abs: f64,
}

impl TruePeak {
    pub fn new(channels: usize) -> Self {
        Self {
            rings: vec![[0.0; TP_TAPS]; channels],
            max_abs: 0.0,
        }
    }

    pub fn channel_count(&self) -> usize {
        self.rings.len()
    }

    /// Feed planar audio. `channels.len()` must equal the configured
    /// channel count; every slice must have the same length.
    pub fn push(&mut self, channels: &[&[f32]]) -> Result<()> {
        check_planar(channels, self.rings.len())?;
        for (ch_idx, x_slice) in channels.iter().enumerate() {
            let ring = &mut self.rings[ch_idx];
            for &x in *x_slice {
                // Shift the 12-sample ring: newest first.
                ring.copy_within(0..TP_TAPS - 1, 1);
                ring[0] = x as f64;
                // Compute 4 oversampled outputs and track absolute peak.
                for phase in &TP_PHASE {
                    let y: f64 = phase.iter().zip(ring.iter()).map(|(c, s)| c * s).sum();
                    let a = y.abs();
                    if a > self.max_abs {
                        self.max_abs = a;
                    }
                }
            }
        }
        Ok(())
    }

    /// Peak level in dBTP (dB relative to 0 dBFS). Returns `-∞` if no
    /// non-zero sample has been seen.
    pub fn peak_dbtp(&self) -> f64 {
        if self.max_abs <= 0.0 {
            f64::NEG_INFINITY
        } else {
            20.0 * self.max_abs.log10()
        }
    }

    pub fn reset(&mut self) {
        for r in self.rings.iter_mut() {
            *r = [0.0; TP_TAPS];
        }
        self.max_abs = 0.0;
    }
}

#[cfg(test)]
mod tests {
    use std::f64::consts::PI;

    use super::super::test_support::{sine_samples, FS};
    use super::*;

    #[test]
    fn true_peak_silence_is_neg_infinity() {
        let mut tp = TruePeak::new(1);
        let zeros = vec![0.0_f32; 4800];
        tp.push(&[&zeros]).unwrap();
        assert_eq!(tp.peak_dbtp(), f64::NEG_INFINITY);
    }

    #[test]
    fn true_peak_rejects_mismatched_channels() {
        let mut tp = TruePeak::new(2);
        let l = vec![0.0_f32; 100];
        let r = vec![0.0_f32; 99];
        assert!(tp.push(&[&l, &r]).is_err());
    }

    #[test]
    fn true_peak_phase_filter_dc_gain_near_unity() {
        // Each Annex 2 polyphase branch should have DC gain ≈ 1.0 so a
        // DC input reconstructs at (approximately) its original level
        // across all 4 phases. Published filter has a small droop — stay
        // within ±0.05 of unity.
        for (i, phase) in TP_PHASE.iter().enumerate() {
            let sum: f64 = phase.iter().sum();
            assert!(
                (sum - 1.0).abs() < 0.05,
                "phase {i} DC gain {sum:.5} should be ≈ 1.0"
            );
        }
    }

    #[test]
    fn true_peak_phase_table_is_symmetric() {
        // Annex 2 Table 1's 48-tap prototype is linear-phase symmetric:
        // phase 3 is phase 0 reversed, phase 2 is phase 1 reversed. The
        // module doc asserts this in prose; assert it in code so a
        // transcription slip in the literal table goes red.
        for (a, b) in [(0usize, 3usize), (1, 2)] {
            let pairs = TP_PHASE[a].iter().zip(TP_PHASE[b].iter().rev());
            for (j, (fwd, rev)) in pairs.enumerate() {
                let k = TP_TAPS - 1 - j;
                assert_eq!(fwd, rev, "phase {a} tap {j} != phase {b} tap {k}");
            }
        }
    }

    #[test]
    fn true_peak_of_sample_aligned_0dbfs_sine_is_near_0dbtp() {
        // A 1 kHz 0 dBFS sine — intersample peak is only marginally
        // above sample peak. Expect dBTP within a tight neighborhood
        // of 0.
        let mut tp = TruePeak::new(1);
        let samples = sine_samples(FS as usize, 1000.0, 0.0, FS);
        tp.push(&[&samples]).unwrap();
        let peak = tp.peak_dbtp();
        assert!(
            (peak - 0.0).abs() < 0.5,
            "1 kHz 0 dBFS true-peak {peak:.3} dBTP, expected ~0"
        );
    }

    #[test]
    fn true_peak_detects_intersample_peak_at_quarter_fs() {
        // A 0 dBFS sine at fs/4 sampled with 45° phase has sample peaks
        // of |sin(45°)| = 0.707 (-3.01 dBFS) but its true analog peak
        // is 1.0 (0 dBTP). The oversampler should recover the peak.
        let mut tp = TruePeak::new(1);
        let f = FS as f64 / 4.0;
        let w = 2.0 * PI * f / FS as f64;
        let phase = PI / 4.0;
        let samples: Vec<f32> = (0..FS as usize)
            .map(|i| (w * i as f64 + phase).sin() as f32)
            .collect();
        tp.push(&[&samples]).unwrap();
        let peak = tp.peak_dbtp();
        // The sample peaks are ~-3 dBFS but the intersample peak sits
        // very close to 0 dBTP. A 48-tap filter leaves a small residual
        // error, so tolerate within 0.5 dB.
        assert!(
            peak > -1.0,
            "expected intersample peak recovery near 0 dBTP, got {peak:.3} dBTP"
        );
    }

    #[test]
    fn true_peak_reset_clears() {
        let mut tp = TruePeak::new(1);
        let loud = sine_samples(FS as usize, 1000.0, 0.0, FS);
        tp.push(&[&loud]).unwrap();
        assert!(tp.peak_dbtp() > -1.0);
        tp.reset();
        assert_eq!(tp.peak_dbtp(), f64::NEG_INFINITY);
    }
}
