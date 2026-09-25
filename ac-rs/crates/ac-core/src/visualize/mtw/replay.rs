//! Ladder replay: the live multi-time-window columns, re-derived from a
//! stored ring (#221).
//!
//! A snapshot stores raw capture, and its offline derivation must show what
//! the live view showed. The live view draws the ladder's columns, so the
//! snapshot has to be pushed through the same [`MtwPair`] the daemon ran —
//! and started at a point where the replay's block grid is the live one.
//!
//! # Why an exact replay exists
//!
//! [`MtwPair`] holds no state that recent input does not determine:
//!
//! - the aligner pairs `meas[n]` with `ref[n − D]` relative to its own first
//!   sample ([`super::align::PairAligner::new`]);
//! - each decimator's phase counter, and each stage's post-warmup block grid,
//!   are anchored to the ladder's first input sample (`MtwPair::push`'s
//!   "Fixed block grid");
//! - the FIR line is fully overwritten once the warmup skip has passed, and
//!   `BlockAverage::mean` sums the last N blocks fresh, with no running total.
//!
//! So a fresh ladder started at full-rate index `s0`, with
//! `(s0 − origin) mod L = 0` where `L = HOP · lcm(stage decims)` and `origin`
//! is the live ladder's first input sample, analyses the same blocks the live
//! one did from the point its own warmup ends: every stage's grid is shifted
//! by a whole number of hops. Its last N blocks per stage are then the live
//! ladder's last N — bit for bit on identical `f32` input. `L` is 0.512 s at
//! 48/96/192 kHz and 2.043 s at 44.1 kHz.
//!
//! The warmup skip is identical for a fresh ladder, so nothing compensates
//! for it here.
//!
//! # What the stored ring must hold
//!
//! A full replay needs `L` (worst-case wait for a grid-aligned start) plus
//! the decimator transient plus the deepest rung's settling `W + hop·(N−1)`
//! plus `|offset|`: ≈ 3.1 s + |offset| at 48/96/192 kHz, ≈ 4.7 s + |offset|
//! at 44.1 kHz. A shorter ring settles fewer rungs. That is reported through
//! `settled_stages` exactly as a warming live frame reports it, and no rung
//! is ever drawn over fewer than N blocks.

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};

use super::ladder::{self, Ladder, HOP, NFFT};
use super::{wire_columns, wire_stages, MtwPair};
use crate::wire::MtwColumns;

/// One rung as the capture's ladder had it — enough to refuse a replay under
/// a different layout, not a second copy of the wire's stage description.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StageProvenance {
    pub decim: usize,
    pub rate: f64,
}

/// Everything the replay needs about one pair's live ladder, recorded at
/// capture (`.acsnap` format v3, `session.mtw[k]`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MtwProvenance {
    /// The pair's alignment offset in full-rate samples, signed — the `D`
    /// the live ladder was built with.
    pub offset: i64,
    /// The live ladder's first input sample, as a full-rate index relative
    /// to the first stored sample. Negative when the ladder started before
    /// the ring's retained window.
    pub origin: i64,
    /// Blocks averaged per stage.
    pub n_blocks: usize,
    /// Column density the live frame was assembled at, points per octave.
    pub ppo: f64,
    /// Column grid bounds the live frame was assembled over, in Hz.
    pub f_min: f64,
    pub f_max: f64,
    pub nfft: usize,
    pub hop: usize,
    /// The ladder's rungs, shallowest first.
    pub stages: Vec<StageProvenance>,
}

impl MtwProvenance {
    /// The provenance of a ladder the running code builds at `sr`: the
    /// layout fields are read from [`ladder::layout`], never typed.
    #[allow(clippy::too_many_arguments)]
    pub fn for_layout(
        sr: u32,
        offset: i64,
        origin: i64,
        n_blocks: usize,
        ppo: f64,
        f_min: f64,
        f_max: f64,
    ) -> Result<Self, ladder::LadderError> {
        let l = ladder::layout(sr)?;
        Ok(Self {
            offset,
            origin,
            n_blocks,
            ppo,
            f_min,
            f_max,
            nfft: NFFT,
            hop: HOP,
            stages: l
                .stages
                .iter()
                .map(|s| StageProvenance {
                    decim: s.decim,
                    rate: s.rate,
                })
                .collect(),
        })
    }

    /// Refuse, don't misread: a file whose ladder differs from the one the
    /// running code builds at this rate cannot be replayed by it. The same
    /// rule as an unknown `format_version`.
    fn check_layout(&self, l: &Ladder) -> Result<()> {
        if self.nfft != NFFT || self.hop != HOP {
            return Err(anyhow!(
                "mtw replay: stored nfft/hop {}/{} differ from this reader's {NFFT}/{HOP}",
                self.nfft,
                self.hop
            ));
        }
        let same_stages = self.stages.len() == l.stages.len()
            && self.stages.iter().zip(l.stages.iter()).all(|(p, s)| {
                // `rate` crossed JSON; `decim` is exact and decides it, the
                // rate is compared to within the parser's last bit.
                p.decim == s.decim && (p.rate - s.rate).abs() <= 1e-9 * s.rate.abs()
            });
        if !same_stages {
            let stored: Vec<usize> = self.stages.iter().map(|s| s.decim).collect();
            let current: Vec<usize> = l.stages.iter().map(|s| s.decim).collect();
            return Err(anyhow!(
                "mtw replay: stored stage decims {stored:?} differ from this reader's \
                 {current:?} at {} Hz",
                l.sr
            ));
        }
        if self.n_blocks == 0 {
            return Err(anyhow!("mtw replay: stored n_blocks is 0"));
        }
        Ok(())
    }
}

fn gcd(a: usize, b: usize) -> usize {
    if b == 0 {
        a
    } else {
        gcd(b, a % b)
    }
}

/// `L = HOP · lcm(stage decims)`: the full-rate period after which every
/// stage's block grid repeats.
pub fn grid_period(l: &Ladder) -> usize {
    let lcm = l
        .stages
        .iter()
        .fold(1usize, |acc, s| acc / gcd(acc, s.decim) * s.decim);
    HOP * lcm
}

/// The smallest index `s0 ≥ max(0, origin)` with `(s0 − origin) mod period
/// == 0`.
pub fn replay_start(origin: i64, period: usize) -> usize {
    let period = period as i64;
    let lo = origin.max(0);
    let whole_periods = (lo - origin + period - 1) / period;
    (origin + whole_periods * period) as usize
}

/// Replay the live ladder over a stored `(meas, reference)` ring.
///
/// Returns the columns the live frame carried at the ring's tail, in their
/// wire form, or `None` when not one rung could settle inside the ring.
/// Errors when `prov` describes a ladder the running code does not build at
/// `sr`, or when the two legs differ in length.
pub fn replay(
    meas: &[f32],
    reference: &[f32],
    sr: u32,
    prov: &MtwProvenance,
) -> Result<Option<MtwColumns>> {
    let l = ladder::layout(sr).map_err(|e| anyhow!("mtw replay: {e}"))?;
    prov.check_layout(&l)?;
    if meas.len() != reference.len() {
        return Err(anyhow!(
            "mtw replay: meas has {} samples but reference has {}",
            meas.len(),
            reference.len()
        ));
    }
    let s0 = replay_start(prov.origin, grid_period(&l));
    let mut p =
        MtwPair::new(sr, prov.offset, prov.n_blocks).map_err(|e| anyhow!("mtw replay: {e}"))?;
    // Chunked only to bound the ladder's scratch buffers; the fixed block
    // grid makes the result independent of the chunking.
    const CHUNK: usize = 1 << 16;
    if s0 < meas.len() {
        for (m, r) in meas[s0..].chunks(CHUNK).zip(reference[s0..].chunks(CHUNK)) {
            p.push(m, r);
        }
    }
    let Some(cols) = p.columns(prov.f_min, prov.f_max, prov.ppo) else {
        return Ok(None);
    };
    Ok(Some(wire_columns(
        &cols,
        prov.ppo,
        prov.n_blocks,
        p.settled_stages(),
        wire_stages(p.ladder(), prov.n_blocks),
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot::{read_acsnap, write_acsnap, ChannelMeta, SessionMeta, SnapshotMeta};

    const SR: u32 = 48_000;
    const N_BLOCKS: usize = 4;
    const PPO: f64 = 48.0;
    const F_MIN: f64 = 20.0;
    const F_MAX: f64 = 24_000.0;
    const OFFSET: i64 = 37;

    /// The daemon parity test's bar (`it_snapshot.rs`): a replay the daemon
    /// would accept as the live frame is within all three of these.
    const TOL_DB: f64 = 0.01;
    const TOL_DEG: f64 = 0.1;
    const TOL_COH: f64 = 1e-4;

    /// Deterministic source on the i24 grid, seekable, so FLAC stores it
    /// losslessly and the round trip cannot be what makes a replay differ.
    fn on_grid(seed: u64, index: i64, amplitude: f64) -> f32 {
        if index < 0 {
            return 0.0;
        }
        let mut z = seed.wrapping_add((index as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15));
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        let u = ((z >> 40) as f64 / (1u64 << 24) as f64) * 2.0 - 1.0;
        ((amplitude * u * 8_388_608.0).round() / 8_388_608.0) as f32
    }

    /// The whole stream. `meas` is the source delayed by `OFFSET` at half
    /// gain **plus independent noise of equal power**, so coherence sits near
    /// 0.5 and H1 depends on which blocks are averaged. Under a pure
    /// gain+delay stimulus every block agrees and a replay on the wrong grid
    /// could not be told apart from the right one.
    fn stream(n: usize) -> (Vec<f32>, Vec<f32>) {
        let reference: Vec<f32> = (0..n as i64).map(|i| on_grid(0xA11C, i, 0.3)).collect();
        let meas: Vec<f32> = (0..n as i64)
            .map(|i| {
                let x = 0.5 * f64::from(on_grid(0xA11C, i - OFFSET, 0.3))
                    + f64::from(on_grid(0xB0B, i, 0.15));
                ((x * 8_388_608.0).round() / 8_388_608.0) as f32
            })
            .collect();
        (meas, reference)
    }

    /// The live ladder: built at `origin_abs` and fed the rest of the stream
    /// in deliberately irregular chunks.
    fn live(meas: &[f32], reference: &[f32], origin_abs: usize) -> MtwColumns {
        let mut p = MtwPair::new(SR, OFFSET, N_BLOCKS).unwrap();
        let chunks = [2_400usize, 1, 4_801, 97, 12_000, 331, 2_048];
        let (mut i, mut c) = (origin_abs, 0usize);
        while i < meas.len() {
            let len = chunks[c % chunks.len()].min(meas.len() - i);
            c += 1;
            p.push(&meas[i..i + len], &reference[i..i + len]);
            i += len;
        }
        let cols = p.columns(F_MIN, F_MAX, PPO).expect("live ladder warm");
        wire_columns(
            &cols,
            PPO,
            N_BLOCKS,
            p.settled_stages(),
            wire_stages(p.ladder(), N_BLOCKS),
        )
    }

    fn provenance(origin: i64) -> MtwProvenance {
        MtwProvenance::for_layout(SR, OFFSET, origin, N_BLOCKS, PPO, F_MIN, F_MAX).unwrap()
    }

    fn channel(role: &str, input_channel: u32) -> ChannelMeta {
        ChannelMeta {
            role: role.to_string(),
            input_channel,
            weighting: "Z".to_string(),
            integration: "fast".to_string(),
            calibration: None,
            voltage_check: None,
            stream_sha256: None,
        }
    }

    /// Store `ring` with `prov` as a v3 `.acsnap` and read both back, so the
    /// replay runs on what a reader actually gets.
    fn round_trip(
        meas: &[f32],
        reference: &[f32],
        prov: MtwProvenance,
    ) -> (MtwProvenance, Vec<Vec<f32>>) {
        let meta = SnapshotMeta {
            format_version: crate::snapshot::FORMAT_VERSION,
            sr: SR,
            channel_map: vec!["meas_0".into(), "ref".into()],
            per_channel: vec![channel("meas_0", 0), channel("ref", 1)],
            session: SessionMeta {
                pairs: vec![(0, 1)],
                delay_samples: vec![OFFSET],
                nperseg: SR as usize,
                mtw: Some(vec![Some(prov)]),
            },
            captured_at_utc: "2026-09-25T00:00:00Z".to_string(),
            daemon_version: "test".to_string(),
            ring_duration_s: meas.len() as f64 / f64::from(SR),
        };
        let (bytes, _) = write_acsnap(&meta, &[meas.to_vec(), reference.to_vec()]).unwrap();
        let snap = read_acsnap(&bytes).unwrap();
        let prov = snap.meta.session.mtw.unwrap()[0].clone().unwrap();
        (prov, snap.channels)
    }

    /// Bit-level identity of every field: equal JSON is equal bits, since
    /// `serde_json` writes the shortest string that round-trips an `f64`.
    fn assert_identical(a: &MtwColumns, b: &MtwColumns, what: &str) {
        assert_eq!(
            serde_json::to_string(a).unwrap(),
            serde_json::to_string(b).unwrap(),
            "{what}: replay differs from the live ladder"
        );
        assert_eq!(a.settled_stages, b.settled_stages, "{what}");
    }

    /// Whether `a` would pass the daemon's parity bar against `b`.
    fn within_daemon_tolerance(a: &MtwColumns, b: &MtwColumns) -> bool {
        if a.freqs != b.freqs || a.stage != b.stage || a.settled_stages != b.settled_stages {
            return false;
        }
        (0..a.freqs.len()).all(|i| {
            let dphi = (a.phase_deg[i] - b.phase_deg[i] + 180.0).rem_euclid(360.0) - 180.0;
            (a.magnitude_db[i] - b.magnitude_db[i]).abs() <= TOL_DB
                && dphi.abs() <= TOL_DEG
                && (a.coherence[i] - b.coherence[i]).abs() <= TOL_COH
        })
    }

    #[test]
    fn grid_period_is_the_documented_figure() {
        for (sr, secs) in [(48_000u32, 0.512), (96_000, 0.512), (192_000, 0.512)] {
            let l = ladder::layout(sr).unwrap();
            let got = grid_period(&l) as f64 / f64::from(sr);
            assert!((got - secs).abs() < 1e-9, "{sr}: {got} s");
        }
        let l = ladder::layout(44_100).unwrap();
        let got = grid_period(&l) as f64 / 44_100.0;
        assert!((got - 2.043).abs() < 1e-3, "44.1k: {got} s");
    }

    #[test]
    fn replay_start_is_the_first_grid_index_inside_the_ring() {
        assert_eq!(replay_start(0, 100), 0);
        assert_eq!(replay_start(7, 100), 7);
        assert_eq!(replay_start(-1, 100), 99);
        assert_eq!(replay_start(-100, 100), 0);
        assert_eq!(replay_start(-250, 100), 50);
    }

    /// The discriminating claim. A ring that starts after the live ladder
    /// did (negative relative origin) and one that starts exactly at it both
    /// replay to the live columns bit for bit, through a real `.acsnap`
    /// round trip. The rejected alternatives — `origin` off by one stage-0
    /// hop, and by one sample — are computed here and each fails the
    /// daemon's bar on at least one column, so the equality is a property of
    /// the grid, not of a stimulus every block agrees on.
    #[test]
    fn replay_reproduces_the_live_ladder_and_a_shifted_origin_does_not() {
        let origin_abs = 7_000usize;
        let total = origin_abs + 6 * SR as usize;
        let (meas, reference) = stream(total);
        let want = live(&meas, &reference, origin_abs);
        assert_eq!(want.settled_stages, vec![true; want.stages.len()]);

        for ring_start in [origin_abs + 12_345, origin_abs] {
            let rel = origin_abs as i64 - ring_start as i64;
            let (prov, ch) = round_trip(
                &meas[ring_start..],
                &reference[ring_start..],
                provenance(rel),
            );
            let got = replay(&ch[0], &ch[1], SR, &prov).unwrap().expect("settled");
            assert_identical(&got, &want, &format!("ring from {ring_start}"));

            for (name, shift) in [("one stage-0 hop", HOP as i64), ("one sample", 1)] {
                let wrong = MtwProvenance {
                    origin: prov.origin + shift,
                    ..prov.clone()
                };
                let alt = replay(&ch[0], &ch[1], SR, &wrong)
                    .unwrap()
                    .expect("settled");
                assert!(
                    !within_daemon_tolerance(&alt, &want),
                    "origin shifted by {name} still matches the live ladder within the \
                     daemon's tolerance — this stimulus cannot tell the grids apart"
                );
            }
        }
    }

    /// A ring too short for the deepest rung reports fewer settled stages and
    /// never emits a column averaged over fewer than N blocks.
    #[test]
    fn a_short_ring_settles_fewer_rungs_and_never_under_averages() {
        let origin_abs = 7_000usize;
        let total = origin_abs + 6 * SR as usize;
        let (meas, reference) = stream(total);
        let want = live(&meas, &reference, origin_abs);

        let ring_start = total - (3 * SR as usize) / 2; // 1.5 s
        let rel = origin_abs as i64 - ring_start as i64;
        let got = replay(
            &meas[ring_start..],
            &reference[ring_start..],
            SR,
            &provenance(rel),
        )
        .unwrap()
        .expect("the top rung settles in 1.5 s");
        let settled = got.settled_stages.iter().filter(|&&s| s).count();
        assert!(
            settled < want.stages.len(),
            "a 1.5 s ring settled every rung: {:?}",
            got.settled_stages
        );
        assert!(
            got.n.iter().all(|&n| n == N_BLOCKS),
            "a column was drawn over fewer than {N_BLOCKS} blocks: {:?}",
            got.n
        );
    }

    #[test]
    fn a_stored_layout_the_reader_does_not_build_is_refused() {
        let (meas, reference) = stream(4_096);
        let mut prov = provenance(0);
        prov.stages[1].decim += 1;
        let err = replay(&meas, &reference, SR, &prov)
            .unwrap_err()
            .to_string();
        assert!(err.contains("stage decims"), "{err}");

        let mut prov = provenance(0);
        prov.nfft = 8_192;
        let err = replay(&meas, &reference, SR, &prov)
            .unwrap_err()
            .to_string();
        assert!(err.contains("nfft/hop"), "{err}");
    }
}
