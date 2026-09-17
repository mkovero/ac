//! Structural validation of `.acsnap` metadata (#435), shared by
//! [`super::read_acsnap`] and [`super::write_acsnap`] so the writer and the
//! reader cannot disagree about what a valid file is.
//!
//! Format v1 binds a FLAC stream to its metadata by position alone, so the
//! checks here are structural:
//!
//! 1. `per_channel.len() == channel_map.len() ==` stream count.
//! 2. `per_channel[i].role == channel_map[i]` for every `i`.
//! 3. Every `per_channel[*].input_channel` is unique.
//! 4. `session.delay_samples.len() == session.pairs.len()`.
//! 5. Both inputs of every pair equal the `input_channel` of some
//!    `per_channel` entry (rule 3 makes it the only one).
//!
//! Deliberately **not** rules, because the daemon writes both cases: unique
//! roles (pairs `[[m,r1],[m,r2]]` give two `"ref"` channels) and
//! `meas != ref` within a pair (`parse_transfer_pairs` accepts `(0, 0)`).
//!
//! A joint permutation of `channel_map` and `per_channel` against the audio
//! still passes: v1 carries no per-stream identity to detect it with.
//!
//! Error text names the fields and indices that disagree. It never states a
//! cause, because the reader cannot know one.

use anyhow::{anyhow, Result};

use super::SnapshotMeta;

/// Rules 1 (metadata half), 2, 3, 4 and 5 — everything that needs only
/// `meta.json`. The reader runs this before decoding `audio.flac`.
pub(super) fn validate_metadata(meta: &SnapshotMeta) -> Result<()> {
    if meta.per_channel.len() != meta.channel_map.len() {
        return Err(anyhow!(
            "per_channel has {} entries but channel_map has {}",
            meta.per_channel.len(),
            meta.channel_map.len()
        ));
    }

    for (i, (ch, role)) in meta
        .per_channel
        .iter()
        .zip(meta.channel_map.iter())
        .enumerate()
    {
        if ch.role != *role {
            return Err(anyhow!(
                "per_channel[{i}].role {:?} != channel_map[{i}] {role:?}",
                ch.role
            ));
        }
    }

    for (j, ch) in meta.per_channel.iter().enumerate() {
        if let Some(i) = meta.per_channel[..j]
            .iter()
            .position(|earlier| earlier.input_channel == ch.input_channel)
        {
            return Err(anyhow!(
                "per_channel[{j}].input_channel {} duplicates per_channel[{i}].input_channel",
                ch.input_channel
            ));
        }
    }

    let session = &meta.session;
    if session.delay_samples.len() != session.pairs.len() {
        return Err(anyhow!(
            "session.delay_samples has {} entries but session.pairs has {}",
            session.delay_samples.len(),
            session.pairs.len()
        ));
    }

    let known = |input: u32| meta.per_channel.iter().any(|c| c.input_channel == input);
    for (k, &(meas, refch)) in session.pairs.iter().enumerate() {
        if !known(meas) {
            return Err(anyhow!(
                "session.pairs[{k}].0 (meas input {meas}) matches no per_channel[*].input_channel"
            ));
        }
        if !known(refch) {
            return Err(anyhow!(
                "session.pairs[{k}].1 (ref input {refch}) matches no per_channel[*].input_channel"
            ));
        }
    }

    Ok(())
}

/// Rule 1's stream half: the audio carries exactly one stream per
/// `channel_map` entry. `source` names where the streams came from, for the
/// error text.
pub(super) fn validate_stream_count(
    meta: &SnapshotMeta,
    stream_count: usize,
    source: &str,
) -> Result<()> {
    if stream_count != meta.channel_map.len() {
        return Err(anyhow!(
            "{source} has {stream_count} streams but channel_map has {} entries",
            meta.channel_map.len()
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::io::{Cursor, Write};

    use super::super::{
        flac, read_acsnap, write_acsnap, ChannelMeta, SessionMeta, SnapshotMeta, FORMAT_VERSION,
    };
    use crate::visualize::weighting_curves::WeightingCurve;

    const SR: u32 = 48_000;

    fn channel(role: &str, input_channel: u32) -> ChannelMeta {
        ChannelMeta {
            role: role.to_string(),
            input_channel,
            weighting: "Z".to_string(),
            integration: "fast".to_string(),
            calibration: None,
        }
    }

    /// Valid control: the smallest set with two ref-only channels sharing
    /// one meas channel, pairs `[[m,r1],[m,r2]]`. Input ids differ from
    /// their stream positions at every position, so code that treats an id
    /// as a position fails.
    ///
    /// | position | input | role     |
    /// |----------|-------|----------|
    /// | 0        | 4     | `ref`    |
    /// | 1        | 0     | `meas_0` |
    /// | 2        | 7     | `ref`    |
    fn control_meta() -> SnapshotMeta {
        SnapshotMeta {
            format_version: FORMAT_VERSION,
            sr: SR,
            channel_map: vec!["ref".into(), "meas_0".into(), "ref".into()],
            per_channel: vec![channel("ref", 4), channel("meas_0", 0), channel("ref", 7)],
            session: SessionMeta {
                pairs: vec![(0, 4), (0, 7)],
                delay_samples: vec![0, 0],
                nperseg: SR as usize,
            },
            captured_at_utc: "2026-01-01T00:00:00Z".to_string(),
            daemon_version: "test".to_string(),
            ring_duration_s: 3.0,
        }
    }

    fn tone(n: usize, amplitude: f64) -> Vec<f32> {
        (0..n)
            .map(|i| {
                (amplitude * (2.0 * std::f64::consts::PI * 1_000.0 * i as f64 / SR as f64).sin())
                    as f32
            })
            .collect()
    }

    /// Short audio for the malformed cases: they are refused before or at
    /// the stream-count check, so the content never reaches `derive_pair`.
    fn short_audio(n_streams: usize) -> Vec<Vec<f32>> {
        vec![tone(64, 0.3); n_streams]
    }

    /// Zips `meta` and `channels` by hand. Deliberately not `write_acsnap`,
    /// which refuses the malformed metadata these tests need on disk.
    fn zip_by_hand(meta: &SnapshotMeta, channels: &[Vec<f32>]) -> Vec<u8> {
        let flac_bytes = flac::encode(channels, meta.sr).unwrap();
        let meta_json = serde_json::to_vec(meta).unwrap();
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let opts = zip::write::SimpleFileOptions::default();
        zip.start_file("meta.json", opts).unwrap();
        zip.write_all(&meta_json).unwrap();
        zip.start_file("audio.flac", opts).unwrap();
        zip.write_all(&flac_bytes).unwrap();
        zip.finish().unwrap().into_inner()
    }

    /// Reads a hand-zipped control-plus-one-change file and asserts the
    /// refusal names `expected`, so no case passes by tripping another rule.
    fn assert_read_rejects(meta: &SnapshotMeta, n_streams: usize, expected: &str) {
        let bytes = zip_by_hand(meta, &short_audio(n_streams));
        let err = match read_acsnap(&bytes) {
            Ok(_) => panic!("read_acsnap accepted malformed metadata (expected {expected:?})"),
            Err(e) => format!("{e:#}"),
        };
        assert!(
            err.contains(expected),
            "error {err:?} does not name the expected rule {expected:?}"
        );
    }

    #[test]
    fn valid_multi_channel_control_reads_and_resolves_by_input_id() {
        let n = 3 * SR as usize;
        // Stream 1 (meas) and stream 0 (ref, input 4) carry the same tone;
        // stream 2 (ref, input 7) carries it at half amplitude. So pair 0
        // reads |H1| ≈ 0 dB and pair 1 reads |H1| ≈ +6.02 dB only when each
        // input resolves to its own stream.
        let channels = vec![tone(n, 0.3), tone(n, 0.3), tone(n, 0.15)];
        let meta = control_meta();
        let snap = read_acsnap(&zip_by_hand(&meta, &channels)).expect("valid control must read");
        assert_eq!(snap.meta, meta);

        assert_eq!(snap.channel_index_for(4), Some(0));
        assert_eq!(snap.channel_index_for(0), Some(1));
        assert_eq!(snap.channel_index_for(7), Some(2));

        let bin_1khz = 1_000usize; // 1 Hz/bin, nperseg = sr
        let d0 = snap
            .derive_pair(0, WeightingCurve::Z, None)
            .expect("derive pair 0");
        let d1 = snap
            .derive_pair(1, WeightingCurve::Z, None)
            .expect("derive pair 1");
        let expected_1 = 20.0 * 2.0_f64.log10();
        assert!(
            d0.h1.magnitude_db[bin_1khz].abs() < 0.5,
            "pair 0 |H1| at 1 kHz = {} dB, expected ~0 dB",
            d0.h1.magnitude_db[bin_1khz]
        );
        assert!(
            (d1.h1.magnitude_db[bin_1khz] - expected_1).abs() < 0.5,
            "pair 1 |H1| at 1 kHz = {} dB, expected ~{expected_1:.2} dB",
            d1.h1.magnitude_db[bin_1khz]
        );
    }

    /// Together with the control's two `"ref"` roles, this pins the two
    /// deliberate non-rules: an implementation that required unique roles
    /// or `meas != ref` would refuse one of the two.
    #[test]
    fn valid_single_channel_self_pair_reads() {
        let meta = SnapshotMeta {
            channel_map: vec!["meas_0".into()],
            per_channel: vec![channel("meas_0", 0)],
            session: SessionMeta {
                pairs: vec![(0, 0)],
                delay_samples: vec![0],
                nperseg: SR as usize,
            },
            ..control_meta()
        };
        let snap = read_acsnap(&zip_by_hand(&meta, &short_audio(1))).expect("self-pair must read");
        assert_eq!(snap.channel_index_for(0), Some(0));
    }

    #[test]
    fn read_rejects_per_channel_shorter_than_channel_map() {
        let mut meta = control_meta();
        meta.per_channel.pop();
        assert_read_rejects(&meta, 3, "per_channel has 2 entries but channel_map has 3");
    }

    #[test]
    fn read_rejects_stream_count_differing_from_channel_map() {
        assert_read_rejects(
            &control_meta(),
            2,
            "audio.flac has 2 streams but channel_map has 3 entries",
        );
    }

    #[test]
    fn read_rejects_channel_map_permuted_against_per_channel() {
        let mut meta = control_meta();
        meta.channel_map.swap(0, 1);
        assert_read_rejects(
            &meta,
            3,
            r#"per_channel[0].role "ref" != channel_map[0] "meas_0""#,
        );
    }

    #[test]
    fn read_rejects_duplicated_input_channel() {
        let mut meta = control_meta();
        meta.per_channel[2].input_channel = 4;
        assert_read_rejects(
            &meta,
            3,
            "per_channel[2].input_channel 4 duplicates per_channel[0].input_channel",
        );
    }

    #[test]
    fn read_rejects_dangling_meas_input() {
        let mut meta = control_meta();
        meta.session.pairs[1].0 = 9;
        assert_read_rejects(&meta, 3, "session.pairs[1].0 (meas input 9)");
    }

    #[test]
    fn read_rejects_dangling_ref_input() {
        let mut meta = control_meta();
        meta.session.pairs[1].1 = 9;
        assert_read_rejects(&meta, 3, "session.pairs[1].1 (ref input 9)");
    }

    #[test]
    fn read_rejects_delay_samples_shorter_than_pairs() {
        let mut meta = control_meta();
        meta.session.delay_samples.pop();
        assert_read_rejects(
            &meta,
            3,
            "session.delay_samples has 1 entries but session.pairs has 2",
        );
    }

    #[test]
    fn read_rejects_delay_samples_longer_than_pairs() {
        let mut meta = control_meta();
        meta.session.delay_samples.push(0);
        assert_read_rejects(
            &meta,
            3,
            "session.delay_samples has 3 entries but session.pairs has 2",
        );
    }

    #[test]
    fn write_rejects_duplicated_input_channel() {
        let mut meta = control_meta();
        meta.per_channel[2].input_channel = 4;
        let err = match write_acsnap(&meta, &short_audio(3)) {
            Ok(_) => panic!("write_acsnap wrote malformed metadata"),
            Err(e) => format!("{e:#}"),
        };
        assert!(
            err.contains("per_channel[2].input_channel 4 duplicates per_channel[0].input_channel"),
            "error {err:?} does not name the duplicated input"
        );
    }

    #[test]
    fn write_accepts_valid_control() {
        write_acsnap(&control_meta(), &short_audio(3)).expect("valid control must write");
    }
}
