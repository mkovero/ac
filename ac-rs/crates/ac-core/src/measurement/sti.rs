//! Tier 1 — the speech transmission index, IEC 60268-16:2011 (read from BS
//! EN 60268-16:2011, identical text), by the indirect method: the modulation
//! transfer function from an impulse response (clause 6).
//!
//! The method, by clause:
//! - **Bands** (A.5.1.2) — the seven octave bands 125 Hz – 8 kHz, IEC 61260
//!   filters ([`crate::measurement::filterbank`]). A capture that does not
//!   reach all seven gets no STI.
//! - **MTF** (clause 6, Schroeder) — `m_k(F) = |∫h_k²(t)e^{−j2πFt}dt| /
//!   ∫h_k²(t)dt` at the 14 modulation frequencies of A.2.2, over the band's
//!   room window — its own trigger to §5.3.3's truncation point, plus the
//!   exponential tail past it — the window the ISO 3382-1 parameters read
//!   ([`crate::measurement::room_acoustics`]), so the measurement noise in
//!   the capture's tail is not read as the room.
//! - **Requirements** (§6.2) — the IR at least 1.6 s long and at least half
//!   the reverberation time (b); at least 20 dB of signal over noise in
//!   every band (c). Short of either, no STI, with the reason.
//! - **Level corrections** (A.3, A.5.3, Annex M) — background noise,
//!   level-dependent auditory masking (Table A.1) and the absolute speech
//!   reception threshold (Table A.2) need octave levels in dB SPL. The IR
//!   carries none, so [`sti_from_ir`] reports the noise-free STI of the
//!   channel's time behaviour and says so; [`correct_mtf`] applies them when
//!   the levels are known (tested on Annex M's worked example).
//! - **STI** (A.5.4 – A.5.6) — effective SNR, clipped to ±15 dB; TI; MTI per
//!   band; the male weighting and redundancy factors of Table A.3; values
//!   above 1 set to 1.

use serde::{Deserialize, Serialize};

use crate::measurement::filterbank::Filterbank;
use crate::measurement::report::StandardsCitation;

/// The seven octave bands (A.5.1.2).
pub const OCTAVES_HZ: [f64; 7] = [125.0, 250.0, 500.0, 1000.0, 2000.0, 4000.0, 8000.0];

/// The 14 modulation frequencies (A.2.2).
pub const MOD_FREQS_HZ: [f64; 14] = [
    0.63, 0.80, 1.00, 1.25, 1.60, 2.00, 2.50, 3.15, 4.00, 5.00, 6.3, 8.00, 10.0, 12.5,
];

/// Table A.3, males: octave band weighting factors α.
pub const ALPHA_MALE: [f64; 7] = [0.085, 0.127, 0.230, 0.233, 0.309, 0.224, 0.173];

/// Table A.3, males: redundancy factors β between band k and k + 1.
pub const BETA_MALE: [f64; 6] = [0.085, 0.078, 0.065, 0.011, 0.047, 0.095];

/// Table A.2: absolute speech reception threshold per band, dB SPL.
pub const ART_DB: [f64; 7] = [46.0, 27.0, 12.0, 6.5, 7.5, 8.0, 12.0];

/// Table A.4, males: octave band levels relative to the A-weighted speech
/// level, dB.
pub const MALE_SPECTRUM_DB: [f64; 7] = [2.9, 2.9, -0.8, -6.8, -12.8, -18.8, -24.8];

/// Background noise captured before the sweep for the level corrections,
/// s (#726).
pub const NOISE_CAPTURE_S: f64 = 3.0;

/// §6.2 b): the least length of the impulse response, s.
pub const MIN_IR_S: f64 = 1.6;

/// §6.2 c): the least signal over noise in every band, dB.
pub const MIN_BAND_SNR_DB: f64 = 20.0;

/// One MTF value per band and modulation frequency: `m[k][f]`.
pub type Mtf = [[f64; 14]; 7];

/// Table A.1: auditory masking in band k, dB, from the level of band k − 1.
pub fn amf_db(level_db: f64) -> f64 {
    if level_db < 63.0 {
        0.5 * level_db - 65.0
    } else if level_db < 67.0 {
        1.8 * level_db - 146.9
    } else if level_db < 100.0 {
        0.5 * level_db - 59.8
    } else {
        -10.0
    }
}

/// Operational octave levels, dB SPL, band by band (125 Hz first).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Levels {
    pub speech_db: [f64; 7],
    pub noise_db: [f64; 7],
}

/// A.5.3 with Annex M NOTE 2: the MTF under background noise, auditory
/// masking and the reception threshold. The noise reduces each band by
/// `1/(1 + 10^(−SNR/10))`; masking and threshold by `I_k/(I_k + I_am,k +
/// I_rt,k)`, where `I_k` is the band's speech-plus-noise intensity and the
/// masking comes from the band below's (A.3.2 — the 125 Hz band is not
/// masked). Values above 1 are set to 1 (NOTE 1).
pub fn correct_mtf(m: &Mtf, levels: &Levels) -> Mtf {
    let intensity = |db: f64| 10f64.powf(db / 10.0);
    let combined: Vec<f64> = (0..7)
        .map(|k| intensity(levels.speech_db[k]) + intensity(levels.noise_db[k]))
        .collect();
    let mut out = *m;
    for k in 0..7 {
        let snr = levels.speech_db[k] - levels.noise_db[k];
        let noise = 1.0 / (1.0 + 10f64.powf(-snr / 10.0));
        let i_k = combined[k];
        let i_am = if k == 0 {
            0.0
        } else {
            let l_below = 10.0 * combined[k - 1].log10();
            combined[k - 1] * intensity(amf_db(l_below))
        };
        let i_rt = intensity(ART_DB[k]);
        let auditory = i_k / (i_k + i_am + i_rt);
        for f in 0..14 {
            out[k][f] = (m[k][f] * noise * auditory).min(1.0);
        }
    }
    out
}

/// A.5.4 – A.5.6: the STI of `m`, and each band's MTI.
pub fn sti_from_mtf(m: &Mtf) -> (f64, [f64; 7]) {
    let mut mti = [0.0; 7];
    for k in 0..7 {
        let ti_sum: f64 = m[k]
            .iter()
            .map(|&v| {
                let snr = if v >= 1.0 {
                    15.0
                } else if v <= 0.0 {
                    -15.0
                } else {
                    (10.0 * (v / (1.0 - v)).log10()).clamp(-15.0, 15.0)
                };
                (snr + 15.0) / 30.0
            })
            .sum();
        mti[k] = ti_sum / 14.0;
    }
    let weighted: f64 = (0..7).map(|k| ALPHA_MALE[k] * mti[k]).sum();
    let redundancy: f64 = (0..6)
        .map(|k| BETA_MALE[k] * (mti[k] * mti[k + 1]).sqrt())
        .sum();
    ((weighted - redundancy).min(1.0), mti)
}

/// Clause 6's Schroeder MTF of one band's room window: `e` the squared
/// band IR, summed from `from` to `end` (samples at `fs`), plus the energy
/// an exponential decay of time constant `tau_s` carries past `tail_at` —
/// `tail` in the units of `e` summed — whose transform is
/// `tail · e^{−jωt}/(1 + jωτ)` at `t = tail_at`. `tail_at` is the decay's
/// truncation point, which a later echo can put before `end` (Codex
/// recheck of #722: the correction belongs to the decay it continues).
pub fn schroeder_mtf(
    e: &[f64],
    from: usize,
    end: usize,
    tail: f64,
    tail_at: usize,
    tau_s: f64,
    fs: f64,
) -> [f64; 14] {
    let t1 = end.min(e.len().saturating_sub(1));
    let from = from.min(t1);
    let total: f64 = e[from..=t1].iter().sum::<f64>() + tail;
    let mut out = [0.0; 14];
    for (f, &fm) in MOD_FREQS_HZ.iter().enumerate() {
        let w = 2.0 * std::f64::consts::PI * fm;
        let (mut re, mut im) = (0.0, 0.0);
        for (k, &v) in e.iter().enumerate().take(t1 + 1).skip(from) {
            let t = k as f64 / fs;
            re += v * (w * t).cos();
            im -= v * (w * t).sin();
        }
        // Tail: tail · e^{−jωt} / (1 + jωτ), t the decay's truncation.
        let t1s = tail_at as f64 / fs;
        let (c, s) = ((w * t1s).cos(), -(w * t1s).sin());
        let (dr, di) = (1.0, w * tau_s);
        let den = dr * dr + di * di;
        let (qr, qi) = ((c * dr + s * di) / den, (s * dr - c * di) / den);
        re += tail * qr;
        im += tail * qi;
        out[f] = if total > 0.0 {
            (re * re + im * im).sqrt() / total
        } else {
            0.0
        };
    }
    out
}

/// The speech octave levels of Table A.4 (male) at an A-weighted speech
/// level, dB SPL.
pub fn speech_levels(speech_dba: f64) -> [f64; 7] {
    MALE_SPECTRUM_DB.map(|d| speech_dba + d)
}

/// The seven octave levels of `capture`, `10·log10(mean square)` — the
/// convention `calibrate_spl` stores the mic sensitivity in (`20·log10(rms)`),
/// so adding the calibration's `spl_offset_db` gives dB SPL. Not the AES17
/// sine-referenced dBFS, which would read 3.01 dB high here (Codex review of
/// #726). IEC 61260 filters, the first 0.1 s of each band skipped as filter
/// settling. `Err` when a band is outside what `sample_rate` reaches.
pub fn octave_levels_db(capture: &[f32], sample_rate: u32) -> Result<[f64; 7], String> {
    let fs = sample_rate as f64;
    let x: Vec<f64> = capture.iter().map(|&v| f64::from(v)).collect();
    let hi = (0.45 * fs - 1.0).min(OCTAVES_HZ[6] * 2f64.sqrt());
    let fb = Filterbank::new(sample_rate, 1, OCTAVES_HZ[0] / 2f64.sqrt(), hi)
        .map_err(|e| format!("octave filters: {e}"))?;
    let skip = (0.1 * fs) as usize;
    let mut out = [0.0; 7];
    for (k, &oct) in OCTAVES_HZ.iter().enumerate() {
        let i = fb
            .centres_hz()
            .iter()
            .position(|&c| (c / oct - 1.0).abs() < 0.05)
            .ok_or_else(|| format!("the {oct:.0} Hz band is out of reach at {sample_rate} Hz"))?;
        let y = fb
            .filter_band(i, &x)
            .ok_or("band filter produced nothing")?;
        let y = y
            .get(skip..)
            .filter(|s| !s.is_empty())
            .ok_or("noise capture too short")?;
        let ms = y.iter().map(|v| v * v).sum::<f64>() / y.len() as f64;
        out[k] = 10.0 * ms.max(1e-24).log10();
    }
    Ok(out)
}

/// The STI under operational levels (A.5.3, Annex M step 3) — speech at an
/// A-weighted level with Table A.4's male spectrum, and the background
/// noise measured at the position.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StiLevels {
    pub sti: f64,
    pub mti: Vec<f64>,
    /// The speech level asked for, dB(A), and its octave levels, dB SPL.
    pub speech_dba: f64,
    pub speech_db: Vec<f64>,
    /// The measured background noise, octave levels, dB SPL.
    pub noise_db: Vec<f64>,
}

/// The STI of an impulse response (clause 6), without level corrections.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Sti {
    /// The STI, or `None` with the reason in [`Sti::refused`].
    pub sti: Option<f64>,
    /// MTI per band, 125 Hz first, when the STI was computed.
    #[serde(default)]
    pub mti: Vec<f64>,
    /// The noise-free MTF behind it, per band (125 Hz first) at the 14
    /// modulation frequencies — what the level corrections start from.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mtf: Vec<[f64; 14]>,
    /// The STI with background noise, masking and threshold applied (#726),
    /// when a speech level was given and the noise could be measured in dB
    /// SPL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub levels: Option<StiLevels>,
    /// Why the level-corrected STI is absent although it was asked for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub levels_refused: Option<String>,
    /// Why there is no STI.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refused: Option<String>,
    /// What the value does not include — always said, since the IR carries
    /// no levels (A.3.1 NOTE; Annex M).
    pub note: String,
    pub citation: StandardsCitation,
}

/// The note every IR-derived STI carries.
pub const NOISE_FREE_NOTE: &str =
    "noise-free STI of the channel's time behaviour: background noise, \
     auditory masking and the reception threshold not applied (no speech or noise levels; \
     IEC 60268-16 A.3.1, Annex M)";

/// The STI of `ir` (the linear IR with its tail, as captured, from just
/// before the direct sound), sampled at `sample_rate`, measured over
/// `[f_lo, f_hi]`. §6.2 b)'s reverberation time is each band's own early
/// decay (see [`crate::measurement::room_acoustics::window_rt`]).
pub fn sti_from_ir(ir: &[f64], sample_rate: u32, f_lo: f64, f_hi: f64) -> Sti {
    let refuse = |why: String| Sti {
        sti: None,
        mti: Vec::new(),
        mtf: Vec::new(),
        levels: None,
        levels_refused: None,
        refused: Some(why),
        note: NOISE_FREE_NOTE.into(),
        citation: citation(),
    };
    let fs = sample_rate as f64;
    let len_s = ir.len() as f64 / fs;
    if len_s < MIN_IR_S {
        return refuse(format!(
            "impulse response {len_s:.2} s long, under the 1.6 s IEC 60268-16 \u{a7}6.2 b) asks \
             for (plot ir: tail and sweep duration 1.6 s or more)"
        ));
    }

    let lo = f_lo.max(OCTAVES_HZ[0] / 2f64.sqrt());
    let hi = f_hi.min(0.45 * fs - 1.0);
    let fb = match Filterbank::new(sample_rate, 1, lo, hi) {
        Ok(fb) => fb,
        Err(e) => return refuse(format!("octave filters: {e}")),
    };
    let centres = fb.centres_hz();
    let mut m: Mtf = [[0.0; 14]; 7];
    // §6.2 b) against each band's own early decay — not a room T30, which
    // over a noise-dominated curve read a loopback as 47 s (#726).
    let mut longest_rt: f64 = 0.0;
    let start = crate::measurement::room_acoustics::trigger(ir, sample_rate, f_hi);
    for (k, &oct) in OCTAVES_HZ.iter().enumerate() {
        let Some(i) = centres.iter().position(|&c| (c / oct - 1.0).abs() < 0.05) else {
            return refuse(format!(
                "the {oct:.0} Hz octave band is outside the measured range \
                 ({f_lo:.0}\u{2013}{f_hi:.0} Hz at {sample_rate} Hz): STI needs 125 Hz \u{2013} 8 kHz"
            ));
        };
        let w = match crate::measurement::room_acoustics::band_window(&fb, i, ir, start, fs) {
            Ok(w) => w,
            Err((_, why)) => return refuse(format!("{oct:.0} Hz band: {why}")),
        };
        if w.peak_to_noise_db < MIN_BAND_SNR_DB {
            return refuse(format!(
                "{oct:.0} Hz band {:.1} dB over its noise, under the 20 dB IEC 60268-16 \
                 \u{a7}6.2 c) asks for",
                w.peak_to_noise_db
            ));
        }
        let tau_s = 10.0 / (-w.slope_db_s * std::f64::consts::LN_10);
        // To the last point the band stands 10 dB over its background: a
        // strong echo after a quiet gap is part of the channel.
        let end = w.t1.max(w.last_above);
        m[k] = schroeder_mtf(&w.e, w.band_start, end, w.correction, w.t1, tau_s, fs);
        // The band's reverberation time from its backward-integrated decay.
        if let Some(rt) = crate::measurement::room_acoustics::window_rt(&w, fs) {
            longest_rt = longest_rt.max(rt);
        }
    }
    // §6.2 b) against the longest band decay found, not only a caller's
    // RT: a room whose T20/T30 could not be read still has one (Codex
    // review of #722).
    if len_s < longest_rt / 2.0 {
        return refuse(format!(
            "impulse response {len_s:.2} s long, under half the reverberation time \
             ({longest_rt:.2} s; \u{a7}6.2 b)"
        ));
    }
    let (sti, mti) = sti_from_mtf(&m);
    Sti {
        sti: Some(sti),
        mti: mti.to_vec(),
        mtf: m.to_vec(),
        levels: None,
        levels_refused: None,
        refused: None,
        note: NOISE_FREE_NOTE.into(),
        citation: citation(),
    }
}

impl Sti {
    /// Apply operational levels (#726): speech at `speech_dba` (Table A.4
    /// male spectrum) and the measured background `noise_db` (octave levels,
    /// dB SPL), through [`correct_mtf`]. With no MTF (the STI itself was
    /// refused) it says so rather than staying silent.
    pub fn with_levels(mut self, speech_dba: f64, noise_db: [f64; 7]) -> Sti {
        let Ok(m): Result<Mtf, _> = self.mtf.clone().try_into() else {
            self.levels_refused = Some("no STI to correct (see above)".to_string());
            return self;
        };
        let speech = speech_levels(speech_dba);
        let corrected = correct_mtf(
            &m,
            &Levels {
                speech_db: speech,
                noise_db,
            },
        );
        let (sti, mti) = sti_from_mtf(&corrected);
        self.levels = Some(StiLevels {
            sti,
            mti: mti.to_vec(),
            speech_dba,
            speech_db: speech.to_vec(),
            noise_db: noise_db.to_vec(),
        });
        self
    }

    /// Record why the level-corrected STI could not be formed.
    pub fn levels_refused(mut self, why: String) -> Sti {
        self.levels_refused = Some(why);
        self
    }
}

fn citation() -> StandardsCitation {
    StandardsCitation {
        standard: "IEC 60268-16:2011".into(),
        clause:
            "clause 6 indirect method (Schroeder MTF), \u{a7}6.2 b) c), A.2.2, A.5.4\u{2013}A.5.6, \
                 Table A.3 (male)"
                .into(),
        verified: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Annex M, Table M.1 step 2: the MTF without noise, masking and
    /// threshold (rows 0,63 … 12,5 Hz, columns 125 Hz … 8 kHz).
    const M2: [[f64; 7]; 14] = [
        [0.983, 0.960, 0.978, 0.990, 0.990, 0.986, 0.997],
        [0.968, 0.936, 0.959, 0.974, 0.980, 0.979, 0.995],
        [0.947, 0.904, 0.931, 0.953, 0.966, 0.968, 0.992],
        [0.920, 0.869, 0.898, 0.927, 0.949, 0.955, 0.987],
        [0.886, 0.826, 0.852, 0.892, 0.925, 0.935, 0.981],
        [0.851, 0.791, 0.808, 0.856, 0.900, 0.914, 0.974],
        [0.816, 0.756, 0.764, 0.816, 0.871, 0.891, 0.964],
        [0.773, 0.721, 0.730, 0.776, 0.841, 0.866, 0.953],
        [0.741, 0.684, 0.705, 0.745, 0.809, 0.838, 0.941],
        [0.726, 0.628, 0.678, 0.736, 0.780, 0.812, 0.929],
        [0.714, 0.557, 0.656, 0.723, 0.753, 0.786, 0.916],
        [0.670, 0.520, 0.623, 0.678, 0.728, 0.765, 0.904],
        [0.591, 0.483, 0.556, 0.615, 0.701, 0.749, 0.893],
        [0.554, 0.446, 0.523, 0.614, 0.685, 0.737, 0.884],
    ];

    /// Annex M step 3: the same MTF under the operational levels.
    const M3: [[f64; 7]; 14] = [
        [0.981, 0.946, 0.946, 0.953, 0.971, 0.975, 0.992],
        [0.966, 0.922, 0.927, 0.938, 0.961, 0.968, 0.990],
        [0.945, 0.891, 0.900, 0.918, 0.947, 0.957, 0.987],
        [0.919, 0.856, 0.868, 0.893, 0.931, 0.944, 0.982],
        [0.884, 0.814, 0.823, 0.859, 0.907, 0.925, 0.976],
        [0.850, 0.779, 0.781, 0.824, 0.882, 0.904, 0.969],
        [0.814, 0.745, 0.738, 0.786, 0.855, 0.881, 0.959],
        [0.772, 0.710, 0.706, 0.747, 0.825, 0.856, 0.948],
        [0.739, 0.674, 0.681, 0.718, 0.793, 0.829, 0.936],
        [0.724, 0.619, 0.656, 0.709, 0.765, 0.804, 0.924],
        [0.713, 0.549, 0.634, 0.696, 0.739, 0.778, 0.911],
        [0.668, 0.512, 0.602, 0.653, 0.714, 0.757, 0.900],
        [0.589, 0.476, 0.537, 0.593, 0.687, 0.741, 0.889],
        [0.553, 0.439, 0.505, 0.592, 0.672, 0.729, 0.880],
    ];

    fn transpose(rows: &[[f64; 7]; 14]) -> Mtf {
        let mut m = [[0.0; 14]; 7];
        for (f, row) in rows.iter().enumerate() {
            for k in 0..7 {
                m[k][f] = row[k];
            }
        }
        m
    }

    /// The standard's own worked example (Annex M, Table M.1): its
    /// noise-free MTF under its operational speech and noise levels gives
    /// its step-3 matrix (to its 3 decimals) and its STI, 0.76.
    #[test]
    fn annex_m_worked_example_is_reproduced() {
        let levels = Levels {
            speech_db: [82.9, 82.9, 79.2, 73.2, 67.2, 61.2, 55.2],
            noise_db: [55.5, 47.5, 41.5, 37.5, 34.5, 32.5, 30.5],
        };
        let got = correct_mtf(&transpose(&M2), &levels);
        let want = transpose(&M3);
        for k in 0..7 {
            for f in 0..14 {
                assert!(
                    (got[k][f] - want[k][f]).abs() <= 0.0015,
                    "band {k} fm {f}: {} vs {}",
                    got[k][f],
                    want[k][f]
                );
            }
        }
        let (sti, mti) = sti_from_mtf(&got);
        assert!((sti - 0.76).abs() < 0.005, "STI {sti}");
        let want_mti = [0.73, 0.66, 0.67, 0.71, 0.77, 0.80, 0.92];
        for k in 0..7 {
            assert!((mti[k] - want_mti[k]).abs() < 0.006, "MTI {k}: {}", mti[k]);
        }
        let (sti3, _) = sti_from_mtf(&want);
        assert!(
            (sti3 - 0.76).abs() < 0.005,
            "STI of the printed matrix {sti3}"
        );
    }

    /// Table A.1 at the levels Annex M uses (its "Auditory masking factor
    /// amf dB" row, step 3).
    #[test]
    fn masking_follows_table_a1() {
        for (l, want) in [
            (82.9, -18.3),
            (79.2, -20.2),
            (73.2, -23.2),
            (67.2, -26.2),
            (61.2, -34.4),
        ] {
            assert!((amf_db(l) - want).abs() < 0.06, "L {l}: {}", amf_db(l));
        }
        assert_eq!(amf_db(120.0), -10.0);
        assert!((amf_db(65.0) - (1.8 * 65.0 - 146.9)).abs() < 1e-12);
    }

    /// Figure A.2 case A: an exponential decay of reverberation time T has
    /// `m(F) = 1/√(1 + (2πFT/13.8)²)` — the Schroeder integral of its
    /// energy, including the tail past a truncation point, gives exactly
    /// that.
    #[test]
    fn an_exponential_decay_has_the_theoretical_mtf() {
        let fs = 8000.0;
        let t = 1.2;
        let rate = 13.8 / t; // energy e^{-13.8 t/T}
        let n = (0.8 * fs) as usize; // truncated at 0.8 s; the tail modelled
        let e: Vec<f64> = (0..n).map(|k| (-rate * k as f64 / fs).exp()).collect();
        let tau = 1.0 / rate;
        let t1 = n - 1;
        let tail = e[t1] * tau * fs * (-rate / fs).exp(); // the energy past t1
        let m = schroeder_mtf(&e, 0, t1, tail, t1, tau, fs);
        for (f, &fm) in MOD_FREQS_HZ.iter().enumerate() {
            let want = 1.0 / (1.0 + (2.0 * std::f64::consts::PI * fm * t / 13.8).powi(2)).sqrt();
            assert!((m[f] - want).abs() < 0.01, "F {fm}: {} vs {want}", m[f]);
        }
    }

    fn noise(n: usize, seed: u64) -> Vec<f64> {
        let mut s = seed;
        (0..n)
            .map(|_| {
                s = s
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                ((s >> 40) as f64 / (1u64 << 24) as f64) * 2.0 - 1.0
            })
            .collect()
    }

    /// Noise under an exponential envelope of reverberation time `rt`, over
    /// a floor `floor_db` below its start, `len_s` long at `fs`.
    fn room(rt: f64, floor_db: f64, len_s: f64, fs: u32) -> Vec<f64> {
        let n = (len_s * fs as f64) as usize;
        let decay = noise(n, 3);
        let floor = noise(n, 9);
        let g = 10f64.powf(floor_db / 20.0);
        (0..n)
            .map(|k| {
                let t = k as f64 / fs as f64;
                decay[k] * (-6.9 * t / rt).exp() + g * floor[k]
            })
            .collect()
    }

    /// A synthetic room: the STI from its IR is the theoretical noise-free
    /// STI of its exponential decay (every band the same T). The rejected
    /// window — the whole capture, measurement noise included — is computed
    /// too: with a floor 30 dB down, the noise tail reads as late energy
    /// and pulls it 0.03 low, several times the room window's error — why
    /// the room window (§5.3.3's truncation plus its tail) is used.
    #[test]
    fn a_rooms_sti_from_its_ir_matches_theory_and_beats_the_whole_capture() {
        let fs = 48_000u32;
        let rt = 0.8;
        let ir = room(rt, -30.0, 2.0, fs);
        let got = sti_from_ir(&ir, fs, 20.0, 20_000.0);
        let sti = got
            .sti
            .unwrap_or_else(|| panic!("refused: {:?}", got.refused));
        let mut theory: Mtf = [[0.0; 14]; 7];
        for band in theory.iter_mut() {
            for (f, &fm) in MOD_FREQS_HZ.iter().enumerate() {
                band[f] =
                    1.0 / (1.0 + (2.0 * std::f64::consts::PI * fm * rt / 13.8).powi(2)).sqrt();
            }
        }
        let (want, _) = sti_from_mtf(&theory);
        assert!((sti - want).abs() < 0.01, "STI {sti} vs theory {want}");

        // Rejected: every band's whole capture, noise and all.
        let fb = Filterbank::new(fs, 1, 88.0, 11_400.0).unwrap();
        let mut whole: Mtf = [[0.0; 14]; 7];
        for (k, &oct) in OCTAVES_HZ.iter().enumerate() {
            let i = fb
                .centres_hz()
                .iter()
                .position(|&c| (c / oct - 1.0).abs() < 0.05)
                .unwrap();
            let e: Vec<f64> = fb
                .filter_band(i, &ir)
                .unwrap()
                .iter()
                .map(|v| v * v)
                .collect();
            whole[k] = schroeder_mtf(&e, 0, e.len() - 1, 0.0, e.len() - 1, 1.0, fs as f64);
        }
        let (rejected, _) = sti_from_mtf(&whole);
        // Measured: room window 0.635, whole capture 0.611, theory 0.639.
        assert!(
            want - rejected > 0.02 && (want - rejected) > 4.0 * (sti - want).abs(),
            "the whole capture read {rejected}, the room window {sti}, theory {want}"
        );
    }

    /// §6.2: too short an IR, a band outside the measured range, or too
    /// little signal over noise in a band gives no STI, with the reason; and
    /// every result says the level corrections were not applied.
    #[test]
    fn what_the_standard_does_not_allow_is_refused_with_the_reason() {
        let fs = 48_000u32;
        let short = sti_from_ir(&room(0.5, -60.0, 1.0, fs), fs, 20.0, 20_000.0);
        assert!(
            short.refused.as_deref().unwrap().contains("1.6 s"),
            "{short:?}"
        );
        let narrow = sti_from_ir(&room(0.5, -60.0, 2.0, fs), fs, 20.0, 6_000.0);
        assert!(
            narrow.refused.as_deref().unwrap().contains("8000 Hz"),
            "{narrow:?}"
        );
        let noisy = sti_from_ir(&room(0.5, -12.0, 2.0, fs), fs, 20.0, 20_000.0);
        assert!(noisy.sti.is_none(), "{noisy:?}");
        assert_eq!(short.note, NOISE_FREE_NOTE);
    }

    /// The band STIs of a noise-free IR integrated whole — the clause 6
    /// integral with nothing to exclude.
    fn whole_sti(ir: &[f64], fs: u32) -> f64 {
        let fb = Filterbank::new(fs, 1, 88.0, 11_400.0).unwrap();
        let mut m: Mtf = [[0.0; 14]; 7];
        for (k, &oct) in OCTAVES_HZ.iter().enumerate() {
            let i = fb
                .centres_hz()
                .iter()
                .position(|&c| (c / oct - 1.0).abs() < 0.05)
                .unwrap();
            let e: Vec<f64> = fb
                .filter_band(i, ir)
                .unwrap()
                .iter()
                .map(|v| v * v)
                .collect();
            m[k] = schroeder_mtf(&e, 0, e.len() - 1, 0.0, e.len() - 1, 1.0, fs as f64);
        }
        sti_from_mtf(&m).0
    }

    /// Codex review of #722: a strong echo after a quiet gap is part of the
    /// channel. The STI counts it — matching the noise-free IR integrated
    /// whole — where the rejected window (§5.3.3's first crossing into the
    /// background, before the echo) reads the channel as if it had none.
    #[test]
    fn an_echo_after_a_quiet_gap_is_counted() {
        let fs = 48_000u32;
        let n = (2.0 * fs as f64) as usize;
        let decay = noise(n, 3);
        let floor = noise(n, 9);
        let echo_at = (0.5 * fs as f64) as usize;
        let shaped = |floor_gain: f64, with_echo: bool| -> Vec<f64> {
            (0..n)
                .map(|k| {
                    let t = k as f64 / fs as f64;
                    let mut v = decay[k] * (-6.9 * t / 0.15).exp();
                    if with_echo && k >= echo_at {
                        let te = (k - echo_at) as f64 / fs as f64;
                        v += 0.7 * decay[k - echo_at] * (-6.9 * te / 0.15).exp();
                    }
                    v + floor_gain * floor[k]
                })
                .collect()
        };
        let got = sti_from_ir(&shaped(1e-3, true), fs, 20.0, 20_000.0);
        let sti = got.sti.unwrap_or_else(|| panic!("{:?}", got.refused));
        let want = whole_sti(&shaped(0.0, true), fs);
        let without = whole_sti(&shaped(0.0, false), fs);
        assert!(
            (sti - want).abs() < 0.02,
            "STI {sti}, the echo's channel {want}"
        );
        assert!(
            without - want > 0.05,
            "the echo does not matter: {without} vs {want}"
        );
    }

    /// Codex review of #722: §6.2 b) is checked against the band decays
    /// themselves, so a long reverberation refuses an IR shorter than half
    /// of it even when the caller has no RT to pass.
    #[test]
    fn half_the_reverberation_time_is_checked_without_a_caller_rt() {
        let fs = 48_000u32;
        let long = sti_from_ir(&room(4.0, -40.0, 1.7, fs), fs, 20.0, 20_000.0);
        assert!(
            long.refused.as_deref().is_some_and(|r| r.contains("half")),
            "{long:?}"
        );
    }

    /// Codex recheck of #722: the tail correction continues the decay it
    /// was fitted to, so it sits at that decay's truncation point even when
    /// the window runs on to a later echo. Exact: a decay continuing under
    /// the floor plus an echo. Modelled: the decay cut at t1, its lost part
    /// as the tail. Placed at t1 the model matches the exact MTF; placed
    /// after the echo (the rejected placement) it does not.
    #[test]
    fn the_tail_sits_at_the_decay_it_continues() {
        let fs = 2000.0;
        let n = (1.5 * fs) as usize;
        let rate = 13.8 / 0.6; // T = 0.6 s
        let t1 = (0.3 * fs) as usize;
        let echo = (0.9 * fs) as usize;
        let decay = |k: usize| (-rate * k as f64 / fs).exp();
        let exact: Vec<f64> = (0..n)
            .map(|k| {
                decay(k)
                    + if k >= echo {
                        0.5 * decay(k - echo)
                    } else {
                        0.0
                    }
            })
            .collect();
        let cut: Vec<f64> = (0..n)
            .map(|k| {
                (if k <= t1 { decay(k) } else { 0.0 })
                    + if k >= echo {
                        0.5 * decay(k - echo)
                    } else {
                        0.0
                    }
            })
            .collect();
        let tau = 1.0 / rate;
        let tail: f64 = (t1 + 1..n).map(decay).sum();
        let want = schroeder_mtf(&exact, 0, n - 1, 0.0, n - 1, 1.0, fs);
        let at_t1 = schroeder_mtf(&cut, 0, n - 1, tail, t1, tau, fs);
        let at_end = schroeder_mtf(&cut, 0, n - 1, tail, n - 1, tau, fs);
        let err = |m: &[f64; 14]| {
            m.iter()
                .zip(&want)
                .map(|(a, b)| (a - b).abs())
                .fold(0.0, f64::max)
        };
        assert!(err(&at_t1) < 0.01, "tail at t1 off by {}", err(&at_t1));
        assert!(
            err(&at_end) > 5.0 * err(&at_t1),
            "placement made no difference"
        );
    }

    /// #726: Table A.4 at 80 dB(A) is Annex M's operational speech row, and
    /// its noise-free MTF under that speech and its noise gives its STI,
    /// 0.76 — through `with_levels`, the path `plot ir` takes.
    #[test]
    fn operational_levels_reproduce_annex_m() {
        assert_eq!(
            speech_levels(80.0).map(|v| (v * 10.0).round() / 10.0),
            [82.9, 82.9, 79.2, 73.2, 67.2, 61.2, 55.2]
        );
        let m = transpose(&M2);
        let base = Sti {
            sti: Some(sti_from_mtf(&m).0),
            mti: Vec::new(),
            mtf: m.to_vec(),
            levels: None,
            levels_refused: None,
            refused: None,
            note: NOISE_FREE_NOTE.into(),
            citation: citation(),
        };
        let got = base.with_levels(80.0, [55.5, 47.5, 41.5, 37.5, 34.5, 32.5, 30.5]);
        let l = got.levels.expect("levels applied");
        assert!((l.sti - 0.76).abs() < 0.005, "STI {}", l.sti);
        assert!(
            l.sti < got.sti.unwrap(),
            "noise and masking did not lower it"
        );
    }

    /// #726: the noise octave levels are `10·log10(mean square)`, the
    /// convention `calibrate_spl` measures the mic in — a 1 kHz sine of peak
    /// 0.1 (rms 0.0707) reads -23.01 in the 1 kHz band, not the AES17 -20,
    /// and far less in bands two octaves away.
    #[test]
    fn a_tones_octave_level_is_its_mean_square_level() {
        let fs = 48_000u32;
        let a = 10f64.powf(-20.0 / 20.0);
        let x: Vec<f32> = (0..fs as usize * 2)
            .map(|k| {
                (a * (2.0 * std::f64::consts::PI * 1000.0 * k as f64 / fs as f64).sin()) as f32
            })
            .collect();
        let l = octave_levels_db(&x, fs).unwrap();
        assert!((l[3] + 23.01).abs() < 0.3, "1 kHz band {}", l[3]);
        assert!(l[1] < -60.0 && l[5] < -60.0, "{l:?}");
    }

    /// #726: an electrical path — a delta over a quiet floor — has no
    /// reverberation and an STI near 1. Its band decay's late line, fitted
    /// over noise, read 47 s and refused it by §6.2 b); the check now reads
    /// the backward-integrated decay.
    #[test]
    fn an_electrical_path_is_not_refused_and_reads_near_one() {
        let fs = 48_000u32;
        let n = 2 * fs as usize;
        let floor = noise(n, 5);
        let mut ir: Vec<f64> = floor.iter().map(|v| v * 1e-4).collect();
        ir[480] += 1.0;
        let got = sti_from_ir(&ir, fs, 20.0, 20_000.0);
        let sti = got
            .sti
            .unwrap_or_else(|| panic!("refused: {:?}", got.refused));
        assert!(sti > 0.95, "STI {sti}");
    }

    /// Codex review of #726: a strong direct sound over a long reverberant
    /// tail supplies the first 10 dB of decay at once, so the early decay
    /// alone read a short room. The decay's extent — the tail standing over
    /// the noise and gliding into it — reads it long, and a 1.7 s capture of
    /// a 3.5 s room is refused by §6.2 b).
    #[test]
    fn a_strong_direct_sound_over_a_long_tail_is_still_a_long_room() {
        let fs = 48_000u32;
        let n = (1.7 * fs as f64) as usize;
        let tail = noise(n, 11);
        let floor = noise(n, 13);
        let ir: Vec<f64> = (0..n)
            .map(|k| {
                let t = k as f64 / fs as f64;
                let direct = if k == 480 { 1.0 } else { 0.0 };
                // RT 3.5 s from -54 dB, under 2 % of the energy (so the
                // first 10 dB of decay is the direct sound alone), reaching
                // the -80 dB floor within the capture; 1.7 s is under half.
                direct + 0.002 * tail[k] * (-6.9 * t / 3.5).exp() + 1e-4 * floor[k]
            })
            .collect();
        let got = sti_from_ir(&ir, fs, 20.0, 20_000.0);
        assert!(
            got.refused.as_deref().is_some_and(|r| r.contains("half")),
            "{got:?}"
        );
    }
}
