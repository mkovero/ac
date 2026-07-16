//! `.acsnap` — self-contained snapshot format + offline derivation
//! (handoff: snapshot-backend M1).
//!
//! A snapshot is raw pre-processing capture plus full provenance (D4), not
//! a saved display: every calibrated/derived quantity the daemon ships
//! live is re-derivable offline from the raw samples this module stores,
//! using the identical `ac-core` functions the daemon's live path calls
//! (no reimplementation — D8).
//!
//! Container: a zip with exactly two entries, `meta.json` and
//! `audio.flac` (D5). Self-containment is a hard requirement — reading a
//! `.acsnap` needs no daemon, no audio backend, no external config file,
//! and reprocessing an old file years later must reproduce the same
//! numbers given the same code (`format_version` exists to let a future
//! reader refuse an unknown schema rather than silently misread one).

mod flac;

use std::io::{Cursor, Read, Write};

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::shared::calibration::Calibration;

/// Current `.acsnap` schema version. Bump on any breaking `meta.json`
/// layout change (e.g. a future 32-bit FLAC path) — readers must refuse
/// an unrecognised version rather than guess.
pub const FORMAT_VERSION: u32 = 1;

/// Per-channel provenance stored alongside the raw audio. `weighting` /
/// `integration` use the string-identical vocabulary to the M0
/// `transfer_stream` frame tags (`"A"|"C"|"Z"`, `"fast"|"slow"`) so a
/// reader never has to translate between two tag sets.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChannelMeta {
    /// Session role: `"meas_0"`, `"meas_1"`, `"ref"`, etc. — matches the
    /// channel's position in `SnapshotMeta::channel_map`.
    pub role: String,
    /// Session-level input channel index (`ac-daemon` capture port index),
    /// independent of FLAC stream position.
    pub input_channel: u32,
    pub weighting: String,
    pub integration: String,
    /// Full 3-layer calibration in effect for this channel at capture
    /// time. `None` when the channel had no cal entry at all.
    pub calibration: Option<Calibration>,
}

/// Session configuration in effect at capture time — enough to reproduce
/// the exact H1 pairing the live daemon used.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionMeta {
    /// `(meas_input_channel, ref_input_channel)` pairs, session indices
    /// (not FLAC stream positions).
    pub pairs: Vec<(u32, u32)>,
    /// Per-pair delay in samples, in the same order as `pairs`.
    pub delay_samples: Vec<i64>,
    /// Welch segment length in effect (`h1_estimate_core` pins this to
    /// `sr`, but it's recorded explicitly rather than assumed, so a
    /// future estimator change can't silently break old snapshots).
    pub nperseg: usize,
}

/// `.acsnap`'s `meta.json` — full provenance for the paired `audio.flac`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SnapshotMeta {
    pub format_version: u32,
    pub sr: u32,
    /// FLAC stream channel index → session role. `channel_map[i]` is
    /// `per_channel[i].role`, kept as a separate flat list because it's
    /// the field a reader checks first (D4's explicit requirement) before
    /// looking anything else up.
    pub channel_map: Vec<String>,
    pub per_channel: Vec<ChannelMeta>,
    pub session: SessionMeta,
    /// RFC3339 UTC timestamp of the capture instant (ring-tail time, i.e.
    /// when `snapshot` was triggered — the ring's *start* is
    /// `ring_duration_s` seconds earlier).
    pub captured_at_utc: String,
    pub daemon_version: String,
    pub ring_duration_s: f64,
}

/// A fully self-contained, decoded snapshot: provenance plus raw
/// per-channel samples in FLAC stream order (matches `meta.channel_map`).
pub struct Snapshot {
    pub meta: SnapshotMeta,
    pub channels: Vec<Vec<f32>>,
}

impl Snapshot {
    /// FLAC stream channel index whose `ChannelMeta::input_channel`
    /// equals `input_channel`, if any.
    fn channel_index_for(&self, input_channel: u32) -> Option<usize> {
        self.meta
            .per_channel
            .iter()
            .position(|c| c.input_channel == input_channel)
    }

    /// Derive H1 + calibrated spectra + SPL for `pairs[pair_idx]`, under
    /// the session's own capture-time weighting (deliverable 5's
    /// "caller-chosen weighting/integration" — pass a different
    /// [`crate::visualize::weighting_curves::WeightingCurve`] to
    /// reprocess under a different one; `sample_range` narrows to a
    /// sub-window of the ring, `None` = the whole capture).
    ///
    /// Calls the exact same low-level functions
    /// (`h1_estimate_with_delay`, `spectrum_to_columns_wire`,
    /// `weighted_broadband_dbfs`) the live daemon path calls — see
    /// `visualize::pair_derivation`.
    pub fn derive_pair(
        &self,
        pair_idx: usize,
        weighting: crate::visualize::weighting_curves::WeightingCurve,
        sample_range: Option<std::ops::Range<usize>>,
    ) -> Result<crate::visualize::pair_derivation::PairDerivation> {
        let &(meas_ch, ref_ch) = self
            .meta
            .session
            .pairs
            .get(pair_idx)
            .ok_or_else(|| anyhow!("derive_pair: pair index {pair_idx} out of range"))?;
        let delay_samples = *self
            .meta
            .session
            .delay_samples
            .get(pair_idx)
            .ok_or_else(|| anyhow!("derive_pair: no recorded delay for pair {pair_idx}"))?;

        let meas_idx = self
            .channel_index_for(meas_ch)
            .ok_or_else(|| anyhow!("derive_pair: no channel recorded for meas input {meas_ch}"))?;
        let ref_idx = self
            .channel_index_for(ref_ch)
            .ok_or_else(|| anyhow!("derive_pair: no channel recorded for ref input {ref_ch}"))?;

        fn slice<'a>(
            ch_samples: &'a [f32],
            range: &Option<std::ops::Range<usize>>,
        ) -> Result<&'a [f32]> {
            match range {
                Some(r) => ch_samples
                    .get(r.clone())
                    .ok_or_else(|| anyhow!("derive_pair: sample_range {r:?} out of bounds")),
                None => Ok(ch_samples),
            }
        }
        let meas_samples = slice(&self.channels[meas_idx], &sample_range)?;
        let ref_samples = slice(&self.channels[ref_idx], &sample_range)?;

        let meas_cal = self.meta.per_channel[meas_idx].calibration.as_ref();
        let ref_cal = self.meta.per_channel[ref_idx].calibration.as_ref();

        Ok(crate::visualize::pair_derivation::derive_pair(
            ref_samples,
            meas_samples,
            self.meta.sr,
            delay_samples,
            meas_cal,
            ref_cal,
            weighting,
        ))
    }
}

/// Serialize `meta` + encode `channels` to FLAC, package as a `.acsnap`
/// zip, and return the raw bytes plus their sha256 (the daemon ships both
/// straight through — `sha256` in the `snapshot` CTRL reply, bytes to the
/// spool file).
pub fn write_acsnap(meta: &SnapshotMeta, channels: &[Vec<f32>]) -> Result<(Vec<u8>, String)> {
    if meta.format_version != FORMAT_VERSION {
        return Err(anyhow!(
            "write_acsnap: meta.format_version {} != current {}",
            meta.format_version,
            FORMAT_VERSION
        ));
    }
    if channels.len() != meta.channel_map.len() {
        return Err(anyhow!(
            "write_acsnap: {} channels but channel_map has {} entries",
            channels.len(),
            meta.channel_map.len()
        ));
    }

    let flac_bytes = flac::encode(channels, meta.sr).context("encoding audio.flac")?;
    let meta_json = serde_json::to_vec_pretty(meta).context("serializing meta.json")?;

    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let options =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    zip.start_file("meta.json", options)
        .context("starting meta.json entry")?;
    zip.write_all(&meta_json).context("writing meta.json")?;
    zip.start_file("audio.flac", options)
        .context("starting audio.flac entry")?;
    zip.write_all(&flac_bytes).context("writing audio.flac")?;
    let cursor = zip.finish().context("finalizing .acsnap zip")?;
    let bytes = cursor.into_inner();

    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    let sha256 = hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();

    Ok((bytes, sha256))
}

/// Read a `.acsnap` file's raw bytes back into provenance + decoded
/// per-channel samples. No daemon, no audio backend, no config file —
/// everything needed lives in `bytes` (D5).
pub fn read_acsnap(bytes: &[u8]) -> Result<Snapshot> {
    let cursor = Cursor::new(bytes);
    let mut archive = zip::ZipArchive::new(cursor).context("opening .acsnap zip")?;

    let meta: SnapshotMeta = {
        let mut entry = archive
            .by_name("meta.json")
            .context("missing meta.json entry")?;
        let mut buf = Vec::new();
        entry.read_to_end(&mut buf).context("reading meta.json")?;
        serde_json::from_slice(&buf).context("parsing meta.json")?
    };
    if meta.format_version != FORMAT_VERSION {
        return Err(anyhow!(
            "read_acsnap: unsupported format_version {} (this reader supports {})",
            meta.format_version,
            FORMAT_VERSION
        ));
    }

    let flac_bytes = {
        let mut entry = archive
            .by_name("audio.flac")
            .context("missing audio.flac entry")?;
        let mut buf = Vec::new();
        entry.read_to_end(&mut buf).context("reading audio.flac")?;
        buf
    };
    let (channels, decoded_sr) = flac::decode(&flac_bytes).context("decoding audio.flac")?;
    if decoded_sr != meta.sr {
        return Err(anyhow!(
            "read_acsnap: FLAC stream sr {decoded_sr} != meta.sr {}",
            meta.sr
        ));
    }
    if channels.len() != meta.channel_map.len() {
        return Err(anyhow!(
            "read_acsnap: FLAC has {} channels but channel_map has {} entries",
            channels.len(),
            meta.channel_map.len()
        ));
    }

    Ok(Snapshot { meta, channels })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tiny_meta(n_channels: usize) -> SnapshotMeta {
        SnapshotMeta {
            format_version: FORMAT_VERSION,
            sr: 48_000,
            channel_map: (0..n_channels).map(|i| format!("meas_{i}")).collect(),
            per_channel: (0..n_channels)
                .map(|i| ChannelMeta {
                    role: format!("meas_{i}"),
                    input_channel: i as u32,
                    weighting: "Z".to_string(),
                    integration: "fast".to_string(),
                    calibration: None,
                })
                .collect(),
            session: SessionMeta {
                pairs: vec![(0, 1)],
                delay_samples: vec![0],
                nperseg: 48_000,
            },
            captured_at_utc: "2026-01-01T00:00:00Z".to_string(),
            daemon_version: "test".to_string(),
            ring_duration_s: 30.0,
        }
    }

    #[test]
    fn write_read_round_trip_preserves_meta_and_samples() {
        let meta = tiny_meta(2);
        // Above flacenc's MIN_BLOCK_SIZE (32 frames) — see
        // `flac::tests::encode_rejects_below_minimum_block_size` for the
        // dedicated too-short-input test.
        let channels: Vec<Vec<f32>> = vec![(0..64).map(|i| (i as f32 * 0.01) - 0.32).collect(); 2];
        let (bytes, sha256) = write_acsnap(&meta, &channels).expect("write");
        assert!(!sha256.is_empty());

        let snap = read_acsnap(&bytes).expect("read");
        assert_eq!(snap.meta, meta);
        assert_eq!(snap.channels.len(), 2);
        for (orig, got) in channels.iter().zip(snap.channels.iter()) {
            assert_eq!(orig.len(), got.len());
        }
    }

    #[test]
    fn write_rejects_mismatched_format_version() {
        let mut meta = tiny_meta(1);
        meta.format_version = 999;
        assert!(write_acsnap(&meta, &[vec![0.0_f32; 10]]).is_err());
    }

    /// A reader must refuse an unrecognised `format_version` even when
    /// the rest of the file is well-formed — build the file by hand
    /// (not via `write_acsnap`, which itself refuses to write a
    /// mismatched version) to simulate a future-format file landing on
    /// an old reader.
    #[test]
    fn read_rejects_unknown_format_version() {
        let mut tampered = tiny_meta(1);
        tampered.format_version = 999;
        let flac_bytes = flac::encode(&[vec![0.0_f32; 64]], tampered.sr).unwrap();
        let meta_json = serde_json::to_vec(&tampered).unwrap();
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let opts = zip::write::SimpleFileOptions::default();
        zip.start_file("meta.json", opts).unwrap();
        zip.write_all(&meta_json).unwrap();
        zip.start_file("audio.flac", opts).unwrap();
        zip.write_all(&flac_bytes).unwrap();
        let bytes = zip.finish().unwrap().into_inner();

        assert!(read_acsnap(&bytes).is_err());
    }

    #[test]
    fn read_rejects_missing_entries() {
        // A zip with no meta.json at all.
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        zip.start_file("audio.flac", zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(&flac::encode(&[vec![0.0_f32; 64]], 48_000).unwrap())
            .unwrap();
        let bytes = zip.finish().unwrap().into_inner();
        assert!(read_acsnap(&bytes).is_err());
    }

    #[test]
    fn write_rejects_channel_count_mismatch() {
        let meta = tiny_meta(2);
        let channels = vec![vec![0.0_f32; 10]]; // only 1, meta expects 2
        assert!(write_acsnap(&meta, &channels).is_err());
    }

    /// AC #3 exercised directly at the ac-core level (no daemon, no audio
    /// backend, no config file — just the bytes and this crate): write a
    /// real tone in, read a `.acsnap` back, `derive_pair`, and confirm
    /// the derived H1 and spectra are physically sane. This is the same
    /// shape the checked-in fixture test (M1 deliverable, `tests/`) will
    /// use against a file that ships in the repo instead of one built
    /// in-test.
    #[test]
    fn derive_pair_end_to_end_from_written_acsnap() {
        let sr = 48_000u32;
        let n = 3 * sr as usize; // 3 s, several Welch segments
        let sig: Vec<f32> = (0..n)
            .map(|i| {
                (0.3 * (2.0 * std::f64::consts::PI * 1_000.0 * i as f64 / sr as f64).sin()) as f32
            })
            .collect();
        let meta = tiny_meta(2); // channel_map: meas_0 (input 0), meas_1 (input 1)
        let channels = vec![sig.clone(), sig.clone()]; // unity loopback

        let (bytes, _) = write_acsnap(&meta, &channels).expect("write");
        let snap = read_acsnap(&bytes).expect("read");

        let d = snap
            .derive_pair(
                0,
                crate::visualize::weighting_curves::WeightingCurve::Z,
                None,
            )
            .expect("derive_pair");

        // Unity loopback: H1 magnitude ~0 dB, coherence ~1, in the
        // audible band — same invariant `transfer::tests::unity_loopback`
        // checks on a direct (non-snapshot) call.
        for k in 20..=20_000 {
            assert!(
                d.h1.magnitude_db[k].abs() < 0.5,
                "bin {k}: mag {} dB",
                d.h1.magnitude_db[k]
            );
            assert!(
                d.h1.coherence[k] > 0.99,
                "bin {k}: coh {}",
                d.h1.coherence[k]
            );
        }
        assert!(!d.meas_spectrum.is_empty());
        assert_eq!(
            d.meas_spectrum, d.ref_spectrum,
            "unity loopback, no cal: identical spectra"
        );
        assert!(d.spl.is_none(), "no SPL cal loaded");
    }

    /// Cross-weighting reprocessing (handoff: parity-completion M1.5,
    /// deliverable 3 — the gap `qa-signoff-m1.md` named): a snapshot
    /// recorded with `weighting: "Z"` at capture time must reprocess
    /// correctly under a *different* weighting (`A`) — D10/D11's "edit-
    /// time freedom" actually exercised, not just structurally possible.
    ///
    /// The stimulus is a pure 100 Hz tone (bin-exact: `nperseg = sr`,
    /// 1 Hz/bin), so the expected `spl_A - spl_Z` offset is exactly
    /// A-weighting's gain at 100 Hz — already standards-verified
    /// elsewhere in this codebase, cited here rather than re-derived:
    /// `weighting_curves::tests::a_weighting_standard_table_values`
    /// checks `A(100 Hz) = -19.1 dB ± 0.1` against IEC 61672-1:2013
    /// Table 2 directly. The Hann 3-tap leakage effect a bin-exact tone
    /// produces under band-power aggregation (M0's
    /// `transfer_stream_meas_spectrum_amplitude_truth` derivation)
    /// biases *both* the Z and A broadband sums by (approximately) the
    /// same amount — it's a property of the pre-weighting spectrum, not
    /// of the weighting curve — so it should mostly cancel in the A-vs-Z
    /// *difference*, which is what's asserted, not either absolute value.
    #[test]
    fn derive_pair_reprocesses_correctly_under_a_different_weighting_than_capture_time() {
        use crate::visualize::weighting_curves::WeightingCurve;

        let sr = 48_000u32;
        let n = 3 * sr as usize;
        let f0 = 100.0_f64; // exact bin at 1 Hz/bin
        let tone: Vec<f32> = (0..n)
            .map(|i| (0.3 * (2.0 * std::f64::consts::PI * f0 * i as f64 / sr as f64).sin()) as f32)
            .collect();

        let meas_cal = Calibration {
            output_channel: 0,
            input_channel: 0,
            ref_freq: 1000.0,
            vrms_at_0dbfs_out: None,
            vrms_at_0dbfs_in: None,
            ref_dbfs: -10.0,
            mic_sensitivity_dbfs_at_94db_spl: Some(-20.0),
            mic_response: None,
        };
        let meta = SnapshotMeta {
            format_version: FORMAT_VERSION,
            sr,
            channel_map: vec!["meas_0".to_string(), "ref".to_string()],
            per_channel: vec![
                ChannelMeta {
                    role: "meas_0".to_string(),
                    input_channel: 0,
                    weighting: "Z".to_string(), // capture-time — stays untouched
                    integration: "fast".to_string(),
                    calibration: Some(meas_cal),
                },
                ChannelMeta {
                    role: "ref".to_string(),
                    input_channel: 1,
                    weighting: "Z".to_string(),
                    integration: "fast".to_string(),
                    calibration: None,
                },
            ],
            session: SessionMeta {
                pairs: vec![(0, 1)],
                delay_samples: vec![0],
                nperseg: sr as usize,
            },
            captured_at_utc: "2026-01-01T00:00:00Z".to_string(),
            daemon_version: "test".to_string(),
            ring_duration_s: n as f64 / sr as f64,
        };
        let (bytes, _) = write_acsnap(&meta, &[tone.clone(), tone]).expect("write");
        let snap = read_acsnap(&bytes).expect("read");

        // Capture-time provenance is untouched by reprocessing choice.
        assert_eq!(snap.meta.per_channel[0].weighting, "Z");

        let d_z = snap
            .derive_pair(0, WeightingCurve::Z, None)
            .expect("derive_pair under Z");
        let d_a = snap
            .derive_pair(0, WeightingCurve::A, None)
            .expect("derive_pair under A");

        // Edge case: the *derived output's* tag reflects the weighting
        // actually used for that call, not the snapshot's stored one.
        assert_eq!(d_z.spl_weighting, WeightingCurve::Z);
        assert_eq!(d_a.spl_weighting, WeightingCurve::A);
        // ...and the stored snapshot metadata is still exactly what it
        // was before either reprocessing call — reprocessing must not
        // mutate capture-time provenance.
        assert_eq!(snap.meta.per_channel[0].weighting, "Z");

        let spl_z = d_z.spl.expect("spl_z: SPL cal loaded");
        let spl_a = d_a.spl.expect("spl_a: SPL cal loaded");
        let offset = spl_a - spl_z;
        let expected_offset = -19.1_f64; // IEC 61672-1:2013 Table 2, A(100 Hz)
                                         // Measured: -19.142 dB — within 0.042 dB of the standards value,
                                         // confirming the leakage-cancels-in-the-difference reasoning
                                         // above. 0.5 dB clears that with over 10x margin while still
                                         // catching a real weighting-application regression.
        assert!(
            (offset - expected_offset).abs() < 0.5,
            "A-vs-Z offset at 100 Hz = {offset:.3} dB, expected ~{expected_offset} dB \
             (spl_z={spl_z:.3} spl_a={spl_a:.3})"
        );
    }

    /// Fixture path for AC #3 (self-containment): a checked-in `.acsnap`
    /// at the repo root's `tests/fixtures/`, matching the location
    /// `aggregate.rs`'s `fixtures-spectrum-hf-garbage` already
    /// established. Not generated by build scripts — a real file,
    /// committed, so `t3_checked_in_fixture_reprocesses_with_no_daemon`
    /// below exercises exactly what a user's saved `.acsnap` would.
    fn fixture_path() -> std::path::PathBuf {
        std::path::PathBuf::from(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../tests/fixtures/snapshot-fixture-v1.acsnap"
        ))
    }

    /// Deterministic broadband source for the fixture, `[-1, 1)`. A
    /// plain LCG is enough here (this is fixture *content*, not a
    /// cross-verified stimulus — it doesn't need to match
    /// `ac-daemon::audio::fake`'s generator, which `ac-core` can't
    /// depend on anyway). Fixed seed ⇒ deterministic ⇒ reproducible
    /// fixture regeneration (AC #4's determinism proof).
    fn fixture_broadband(n: usize, amplitude: f64, seed: u64) -> Vec<f64> {
        let mut state = seed;
        (0..n)
            .map(|_| {
                state = state
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                let u = ((state >> 40) as f64 / (1u64 << 24) as f64) * 2.0 - 1.0;
                amplitude * u
            })
            .collect()
    }

    /// Regenerates the checked-in fixture. `#[ignore]`d — not part of the
    /// normal suite, run manually when the format changes:
    /// `cargo test -p ac-core --lib snapshot::tests::generate_snapshot_fixture -- --ignored`
    ///
    /// Content (handoff: parity-completion M1.5, deliverable 4): `ref` is
    /// a seeded deterministic broadband source (amplitude 0.3); `meas` is
    /// that *same* source scaled by `gain=0.5` and delayed
    /// `delay_samples=200`, **plus** an independent 1 kHz/0.25-amplitude
    /// tone summed in — the correlated broadband component gives H1/
    /// coherence a real ground truth to reprocess against (M1.5's whole
    /// point), the added tone keeps a clean, single-bin amplitude-truth
    /// case on `meas_spectrum` alone (M2's substrate, same reason M1's
    /// fixture had one). SPL-calibrated meas channel so `derive_pair`'s
    /// `spl` path is exercised too.
    #[test]
    #[ignore = "regenerates tests/fixtures/snapshot-fixture-v1.acsnap — run manually"]
    fn generate_snapshot_fixture() {
        let sr = 48_000u32;
        let n = 3 * sr as usize;
        let gain = 0.5_f64;
        let delay = 200usize;
        let seed = 0xACC0_1DED_u64;

        let broadband = fixture_broadband(n, 0.3, seed);
        let refch: Vec<f32> = broadband.iter().map(|&v| v as f32).collect();
        let tone_freq = 1_000.0_f64;
        let tone_amp = 0.25_f64;
        let mut meas = vec![0.0f32; n];
        // Before the delay elapses, only the tone (no correlated
        // component yet) — mirrors `fake.rs`'s `CorrelatedPair` silence
        // convention for the correlated leg specifically.
        for (i, m) in meas.iter_mut().enumerate() {
            let tone =
                tone_amp * (2.0 * std::f64::consts::PI * tone_freq * i as f64 / sr as f64).sin();
            *m = if i >= delay {
                (gain * broadband[i - delay] + tone) as f32
            } else {
                tone as f32
            };
        }

        let meas_cal = Calibration {
            output_channel: 0,
            input_channel: 0,
            ref_freq: 1000.0,
            vrms_at_0dbfs_out: None,
            vrms_at_0dbfs_in: Some(1.5),
            ref_dbfs: -10.0,
            mic_sensitivity_dbfs_at_94db_spl: Some(-26.0),
            mic_response: None,
        };

        let meta = SnapshotMeta {
            format_version: FORMAT_VERSION,
            sr,
            channel_map: vec!["meas_0".to_string(), "ref".to_string()],
            per_channel: vec![
                ChannelMeta {
                    role: "meas_0".to_string(),
                    input_channel: 0,
                    weighting: "Z".to_string(),
                    integration: "fast".to_string(),
                    calibration: Some(meas_cal),
                },
                ChannelMeta {
                    role: "ref".to_string(),
                    input_channel: 1,
                    weighting: "Z".to_string(),
                    integration: "fast".to_string(),
                    calibration: None,
                },
            ],
            session: SessionMeta {
                pairs: vec![(0, 1)],
                delay_samples: vec![delay as i64],
                nperseg: sr as usize,
            },
            captured_at_utc: "2026-07-16T00:00:00Z".to_string(),
            daemon_version: "fixture-generator".to_string(),
            ring_duration_s: n as f64 / sr as f64,
        };

        let (bytes, sha256) = write_acsnap(&meta, &[meas, refch]).expect("write fixture");
        std::fs::write(fixture_path(), &bytes).expect("write fixture file");
        eprintln!(
            "wrote {} ({} bytes, sha256={sha256})",
            fixture_path().display(),
            bytes.len()
        );
    }

    /// AC #4's determinism proof: regenerating twice must produce
    /// byte-identical output (same sha256) — the fixed seed actually
    /// reproduces, not just "looks deterministic".
    #[test]
    fn fixture_generation_is_deterministic_across_runs() {
        fn build_once() -> String {
            let sr = 48_000u32;
            let n = 3 * sr as usize;
            let gain = 0.5_f64;
            let delay = 200usize;
            let seed = 0xACC0_1DED_u64;
            let broadband = fixture_broadband(n, 0.3, seed);
            let refch: Vec<f32> = broadband.iter().map(|&v| v as f32).collect();
            let tone_freq = 1_000.0_f64;
            let tone_amp = 0.25_f64;
            let mut meas = vec![0.0f32; n];
            for i in 0..n {
                let tone = tone_amp
                    * (2.0 * std::f64::consts::PI * tone_freq * i as f64 / sr as f64).sin();
                meas[i] = if i >= delay {
                    (gain * broadband[i - delay] + tone) as f32
                } else {
                    tone as f32
                };
            }
            let meta = SnapshotMeta {
                format_version: FORMAT_VERSION,
                sr,
                channel_map: vec!["meas_0".to_string(), "ref".to_string()],
                per_channel: vec![
                    ChannelMeta {
                        role: "meas_0".to_string(),
                        input_channel: 0,
                        weighting: "Z".to_string(),
                        integration: "fast".to_string(),
                        calibration: None,
                    },
                    ChannelMeta {
                        role: "ref".to_string(),
                        input_channel: 1,
                        weighting: "Z".to_string(),
                        integration: "fast".to_string(),
                        calibration: None,
                    },
                ],
                session: SessionMeta {
                    pairs: vec![(0, 1)],
                    delay_samples: vec![delay as i64],
                    nperseg: sr as usize,
                },
                captured_at_utc: "2026-07-16T00:00:00Z".to_string(),
                daemon_version: "test".to_string(),
                ring_duration_s: n as f64 / sr as f64,
            };
            let (_, sha256) = write_acsnap(&meta, &[meas, refch]).expect("write");
            sha256
        }
        assert_eq!(
            build_once(),
            build_once(),
            "same seed must produce byte-identical .acsnap output"
        );
    }

    /// AC #3: the checked-in fixture reprocesses in an ac-core unit test
    /// with no daemon, no audio backend, no config file — just this
    /// crate and the file's own bytes.
    #[test]
    fn t3_checked_in_fixture_reprocesses_with_no_daemon() {
        let bytes = std::fs::read(fixture_path()).expect(
            "tests/fixtures/snapshot-fixture-v1.acsnap must exist — regenerate via \
             `cargo test -p ac-core --lib snapshot::tests::generate_snapshot_fixture -- --ignored`",
        );
        let snap = read_acsnap(&bytes).expect("read checked-in fixture");
        assert_eq!(snap.meta.format_version, FORMAT_VERSION);
        assert_eq!(snap.meta.channel_map, vec!["meas_0", "ref"]);

        let d = snap
            .derive_pair(
                0,
                crate::visualize::weighting_curves::WeightingCurve::Z,
                None,
            )
            .expect("derive_pair on fixture");

        assert!(!d.meas_spectrum.is_empty());
        assert!(!d.ref_spectrum.is_empty());
        let spl = d.spl.expect("fixture's meas channel is SPL-calibrated");
        assert!(
            spl.is_finite() && (0.0..200.0).contains(&spl),
            "implausible spl={spl}"
        );

        // Amplitude-truth (M2 substrate): the meas_spectrum *column
        // nearest 1 kHz* must read the injected tone (0.25 amplitude),
        // same style as M0's
        // `transfer_stream_meas_spectrum_amplitude_truth`. Deliberately
        // the *nearest-frequency* column, not the global peak: with a
        // broadband component summed in too, log-spaced high-frequency
        // columns are wide enough (more Hz, hence more aggregated
        // broadband bins per column) to out-power the tone's single
        // narrow column — global-max would silently grade the wrong
        // column.
        //
        // Hand-derivation (both terms required — the first attempt at
        // this test omitted the voltage-cal term and got a spurious
        // ~5.3 dB "failure" against a wrong expected value, not a real
        // bug): `meas_spectrum` is voltage-cal-scaled (M0 design — this
        // fixture's `meas_cal.vrms_at_0dbfs_in = 1.5`, kept from the
        // pre-M1.5 fixture), so the base level is
        // `20·log10(0.25 × 1.5) ≈ -8.52 dBFS`, plus the same Hann 3-tap
        // band-power leakage M0's derivation covers (~+1.76 dB at this
        // K/frequency) ⇒ **-6.76 dBFS predicted**. Measured: -6.75 dBFS
        // — within 0.01 dB. 1.0 dB tolerance clears that with large
        // margin while still catching a real regression.
        let expected_tone_dbfs = 20.0 * (0.25_f64 * 1.5).log10() + 1.76;
        let (tone_i, _) = d
            .spec_freqs
            .iter()
            .enumerate()
            .min_by(|(_, a), (_, b)| {
                (**a - 1_000.0_f64)
                    .abs()
                    .partial_cmp(&(**b - 1_000.0_f64).abs())
                    .unwrap()
            })
            .expect("non-empty spec_freqs");
        let tone_dbfs = 20.0 * d.meas_spectrum[tone_i].max(1e-12).log10();
        assert!(
            (tone_dbfs - expected_tone_dbfs).abs() < 1.0,
            "meas_spectrum near 1 kHz = {tone_dbfs:.2} dBFS, expected ~{expected_tone_dbfs:.2} dBFS"
        );

        // Ground truth (H1/coherence): checked at 5 kHz, away from the
        // 1 kHz tone's leakage, where only the correlated broadband
        // component (gain=0.5, delay=200) is present.
        let bin_5khz = 5_000usize; // 1 Hz/bin, nperseg=sr
        let expected_gain_db = 20.0 * 0.5_f64.log10(); // -6.02 dB
        assert!(
            (d.h1.magnitude_db[bin_5khz] - expected_gain_db).abs() < 1.0,
            "H1 at 5 kHz = {} dB, expected ~{expected_gain_db:.2} dB",
            d.h1.magnitude_db[bin_5khz]
        );
        assert!(
            d.h1.coherence[bin_5khz] > 0.9,
            "coherence at 5 kHz = {}, expected > 0.9 (correlated broadband region)",
            d.h1.coherence[bin_5khz]
        );
    }
}
