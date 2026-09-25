//! Structural validation of `.acsnap` metadata (#435), shared by
//! [`super::read_acsnap`] and [`super::write_acsnap`] so the writer and the
//! reader cannot disagree about what a valid file is.
//!
//! A FLAC stream is bound to its metadata by position. Rules 1–6 are
//! structural; rule 7 (format v2, #637) checks the binding itself:
//!
//! 1. `per_channel.len() == channel_map.len() ==` stream count.
//! 2. `per_channel[i].role == channel_map[i]` for every `i`.
//! 3. Every `per_channel[*].input_channel` is unique.
//! 4. `session.delay_samples.len() == session.pairs.len()`.
//! 5. Both inputs of every pair equal the `input_channel` of some
//!    `per_channel` entry (rule 3 makes it the only one).
//! 6. A channel whose `voltage_check` is refused carries no
//!    `vrms_at_0dbfs_*` in its `calibration` (#466): a refused scale that
//!    is still present would be applied by any reader that ignores the
//!    verdict.
//! 7. Format v2: `per_channel[i].stream_sha256` equals the digest of decoded
//!    stream `i` (`flac::StreamHasher`). Its presence and shape are checked
//!    with the metadata: v2 requires it as 64 lowercase hex characters, v1
//!    must not carry it. The digest sits inside the entry it certifies, so
//!    any reorder of `per_channel` against the audio — a same-role swap, or
//!    `channel_map` and `per_channel` permuted together — moves a digest
//!    off its stream.
//! 8. Format v3 (#221): `session.mtw` is present with one entry per
//!    `session.pairs` entry; v1 and v2 must not carry it. Whether an entry's
//!    ladder is one the reader builds is not checked here: that is
//!    [`crate::visualize::mtw::replay`]'s refusal, at derivation time, so a
//!    file from a later ladder still opens and says why it cannot replay.
//!
//! Deliberately **not** rules, because the daemon writes both cases: unique
//! roles (pairs `[[m,r1],[m,r2]]` give two `"ref"` channels) and
//! `meas != ref` within a pair (`parse_transfer_pairs` accepts `(0, 0)`).
//!
//! Format v1 files carry no digest, so rule 7 cannot run on them (#637): a
//! v1 `per_channel` reordered against the audio without changing a role —
//! two `"ref"` entries of a multi-ref session swapped, or `channel_map`
//! permuted with it — still reads, and binds calibration and pair identity
//! to the wrong stream. v1 files are read exactly as before, no better and
//! no worse. In v2, sample-identical streams share a digest, so a swap
//! between them passes; it is also harmless, since the audio each entry
//! then describes is identical.
//!
//! Error text names the fields and indices that disagree. It never states a
//! cause, because the reader cannot know one.

use anyhow::{anyhow, Result};

use super::SnapshotMeta;

/// Rules 1 (metadata half), 2, 3, 4, 5, 6, 7's presence/shape half and 8 —
/// everything that needs only `meta.json`. The reader runs this before
/// decoding `audio.flac`.
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

    for (i, ch) in meta.per_channel.iter().enumerate() {
        let refused = ch.voltage_check.as_ref().is_some_and(|v| v.is_refused());
        if refused && ch.calibration.as_ref().is_some_and(|c| c.has_voltage()) {
            return Err(anyhow!(
                "per_channel[{i}].voltage_check is refused but per_channel[{i}].calibration \
                 carries vrms_at_0dbfs"
            ));
        }
    }

    match (meta.format_version, session.mtw.as_ref()) {
        (1 | 2, None) => {}
        (v @ (1 | 2), Some(_)) => {
            return Err(anyhow!(
                "session.mtw present, but format_version {v} has no such field"
            ));
        }
        (v, None) => {
            return Err(anyhow!(
                "session.mtw missing; format_version {v} requires it"
            ));
        }
        (_, Some(m)) => {
            if m.len() != session.pairs.len() {
                return Err(anyhow!(
                    "session.mtw has {} entries but session.pairs has {}",
                    m.len(),
                    session.pairs.len()
                ));
            }
        }
    }

    for (i, ch) in meta.per_channel.iter().enumerate() {
        match (meta.format_version, ch.stream_sha256.as_deref()) {
            (1, None) => {}
            (1, Some(_)) => {
                return Err(anyhow!(
                    "per_channel[{i}].stream_sha256 present, but format_version 1 has no such field"
                ));
            }
            (_, None) => return Err(missing_digest(i, meta.format_version)),
            (_, Some(d)) => {
                if !is_digest_shaped(d) {
                    return Err(anyhow!(
                        "per_channel[{i}].stream_sha256 is not 64 lowercase hex characters"
                    ));
                }
            }
        }
    }

    Ok(())
}

fn missing_digest(i: usize, format_version: u32) -> anyhow::Error {
    anyhow!("per_channel[{i}].stream_sha256 missing; format_version {format_version} requires it")
}

fn is_digest_shaped(d: &str) -> bool {
    d.len() == 64 && d.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Rule 7: every entry's `stream_sha256` equals the digest of the decoded
/// stream at its position. `digests[i]` is stream `i`'s digest from
/// `flac::decode`; rule 1 has already made the counts agree. A v1 file has
/// no digests and passes unchecked (the v1 limit in the module header).
///
/// The error names the entry, its input, and which stream the stored digest
/// does match, if any — that separates a reordered entry from a damaged
/// one without asserting either.
pub(super) fn validate_stream_identity(meta: &SnapshotMeta, digests: &[String]) -> Result<()> {
    if meta.format_version == 1 {
        return Ok(());
    }
    for (i, ch) in meta.per_channel.iter().enumerate() {
        let stored = ch
            .stream_sha256
            .as_deref()
            .ok_or_else(|| missing_digest(i, meta.format_version))?;
        if digests.get(i).map(String::as_str) == Some(stored) {
            continue;
        }
        let input = ch.input_channel;
        let matches: Vec<usize> = digests
            .iter()
            .enumerate()
            .filter(|(_, d)| d.as_str() == stored)
            .map(|(j, _)| j)
            .collect();
        return Err(if matches.is_empty() {
            anyhow!("per_channel[{i}].stream_sha256 (input {input}) matches no audio.flac stream")
        } else {
            anyhow!(
                "per_channel[{i}].stream_sha256 (input {input}) matches audio.flac {}, not {i}",
                streams_phrase(&matches)
            )
        });
    }
    Ok(())
}

/// `stream 2`, `streams 2 and 3`, `streams 2, 3 and 5`. `positions` is
/// non-empty and ascending.
fn streams_phrase(positions: &[usize]) -> String {
    match positions {
        [only] => format!("stream {only}"),
        [head @ .., last] => {
            let head: Vec<String> = head.iter().map(usize::to_string).collect();
            format!("streams {} and {last}", head.join(", "))
        }
        [] => "no stream".to_string(),
    }
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
            voltage_check: None,
            stream_sha256: None,
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
                mtw: Some(vec![None, None]),
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

    /// `meta` with each `per_channel[i].stream_sha256` set to the digest of
    /// `channels[i]`, as `write_acsnap` would. For hand-zipped v2 files that
    /// must get past rule 7 without going through the writer.
    fn with_digests(meta: &SnapshotMeta, channels: &[Vec<f32>]) -> SnapshotMeta {
        let mut meta = meta.clone();
        for (ch, d) in meta
            .per_channel
            .iter_mut()
            .zip(flac::stream_digests(channels))
        {
            ch.stream_sha256 = Some(d);
        }
        meta
    }

    /// Reads a hand-zipped control-plus-one-change file and asserts the
    /// refusal names `expected`, so no case passes by tripping another rule.
    /// Digests come from one short stream per `per_channel` entry (all the
    /// same tone), so rule 7 holds whatever the stream count.
    fn assert_read_rejects(meta: &SnapshotMeta, n_streams: usize, expected: &str) {
        let meta = &with_digests(meta, &short_audio(meta.per_channel.len()));
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
        let meta = with_digests(&control_meta(), &channels);
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
                mtw: Some(vec![None]),
            },
            ..control_meta()
        };
        let meta = with_digests(&meta, &short_audio(1));
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

    fn refused_check() -> crate::shared::calibration::LayerVerdict {
        use crate::shared::calibration::session::{CheckSource, Evidence, VerdictUnit};
        crate::shared::calibration::LayerVerdict::Refused {
            evidence: Evidence {
                measured: 2.42,
                stored: -0.6,
                delta: 3.02,
                tolerance: 0.1,
                unit: VerdictUnit::Db,
                stored_at: "2026-09-15T23:43:04Z".into(),
                checked_at: "2026-09-16T14:02:11.000Z".into(),
                source: CheckSource::Probe,
            },
            via: None,
            delta_bound: None,
        }
    }

    fn with_scale() -> crate::shared::calibration::Calibration {
        let mut cal = crate::shared::calibration::Calibration::new(0, 0);
        cal.vrms_at_0dbfs_in = Some(1.5);
        cal
    }

    #[test]
    fn read_and_write_reject_a_refused_check_beside_a_voltage_scale() {
        let mut meta = control_meta();
        meta.per_channel[1].voltage_check = Some(refused_check());
        meta.per_channel[1].calibration = Some(with_scale());
        let expected = "per_channel[1].voltage_check is refused but per_channel[1].calibration \
                        carries vrms_at_0dbfs";
        assert_read_rejects(&meta, 3, expected);
        let err = match write_acsnap(&meta, &short_audio(3)) {
            Ok(_) => panic!("write_acsnap wrote a refused scale"),
            Err(e) => format!("{e:#}"),
        };
        assert!(err.contains(expected), "{err}");
    }

    #[test]
    fn a_refused_check_with_the_scale_withheld_is_valid() {
        let mut meta = control_meta();
        meta.per_channel[1].voltage_check = Some(refused_check());
        meta.per_channel[1].calibration = Some(with_scale().without_voltage());
        write_acsnap(&meta, &short_audio(3)).expect("withheld scale must write");
    }

    #[test]
    fn write_accepts_valid_control() {
        write_acsnap(&control_meta(), &short_audio(3)).expect("valid control must write");
    }

    // ---- #637: per-stream identity (rule 7) and the v1 limit ----

    /// The worked case's audio: streams 0 and 1 carry the same tone, stream
    /// 2 carries it at half amplitude, so pair 0 reads ≈ 0 dB and pair 1
    /// ≈ +6.02 dB only when each input resolves to its own stream.
    fn worked_audio() -> Vec<Vec<f32>> {
        let n = 3 * SR as usize;
        vec![tone(n, 0.3), tone(n, 0.3), tone(n, 0.15)]
    }

    /// `control_meta()` as `write_acsnap` stores it for `channels`: format v2
    /// with the writer's own digests, read back from a real written file.
    fn written_meta(channels: &[Vec<f32>]) -> SnapshotMeta {
        let (bytes, _) = write_acsnap(&control_meta(), channels).expect("write control");
        read_acsnap(&bytes).expect("read written control").meta
    }

    fn read_err(bytes: &[u8]) -> String {
        match read_acsnap(bytes) {
            Ok(_) => panic!("read_acsnap accepted the file"),
            Err(e) => format!("{e:#}"),
        }
    }

    fn v1_control_meta() -> SnapshotMeta {
        let mut meta = SnapshotMeta {
            format_version: 1,
            ..control_meta()
        };
        meta.session.mtw = None;
        meta
    }

    /// Worked case, v2: the two `"ref"` entries swapped in `meta.json` only.
    /// Every structural rule holds; rule 7 refuses, naming the entry and the
    /// stream its digest actually belongs to.
    #[test]
    fn v2_same_role_per_channel_swap_is_refused() {
        let channels = worked_audio();
        let mut meta = written_meta(&channels);
        assert_eq!(meta.format_version, FORMAT_VERSION);
        meta.per_channel.swap(0, 2); // ref/4 <-> ref/7, roles unchanged
        assert_eq!(
            read_err(&zip_by_hand(&meta, &channels)),
            "read_acsnap: per_channel[0].stream_sha256 (input 7) matches audio.flac stream 2, not 0"
        );
    }

    /// Worked case, v1: the format-v1 limit, pinned. A v1 file carries no
    /// digest, so the same swap still reads and binds input 4 to stream 2:
    /// pair 0 reads 20·log10(0.3/0.15) = +6.02 dB where the unswapped file
    /// reads 0 dB (provenance: measured, QA on PR #636; the ±0.5 dB bar is
    /// assumed). If v1 reading changes, this turns red.
    #[test]
    fn v1_same_role_per_channel_swap_is_accepted_and_misbinds_pair() {
        let channels = worked_audio();
        let bin_1khz = 1_000usize;

        let meta = v1_control_meta();
        let control = read_acsnap(&zip_by_hand(&meta, &channels)).expect("v1 control reads");
        let d_control = control.derive_pair(0, WeightingCurve::Z, None).unwrap();
        assert!(
            d_control.h1.magnitude_db[bin_1khz].abs() < 0.5,
            "unswapped v1 pair 0 = {} dB, expected ~0 dB",
            d_control.h1.magnitude_db[bin_1khz]
        );

        let mut swapped = meta;
        swapped.per_channel.swap(0, 2);
        let snap = read_acsnap(&zip_by_hand(&swapped, &channels)).expect("v1 accepts it");
        assert_eq!(snap.channel_index_for(4), Some(2));
        let d0 = snap
            .derive_pair(0, WeightingCurve::Z, None)
            .expect("derive");
        let misbound = 20.0 * 2.0_f64.log10();
        assert!(
            (d0.h1.magnitude_db[bin_1khz] - misbound).abs() < 0.5,
            "swapped v1 pair 0 = {} dB, expected ~{misbound:.2} dB",
            d0.h1.magnitude_db[bin_1khz]
        );
    }

    /// `channel_map` and `per_channel` reordered together against the
    /// audio: rule 2 still holds, rule 7 refuses. (Triage asked whether the
    /// digest covers this at no extra cost; it does.)
    #[test]
    fn v2_joint_channel_map_and_per_channel_permutation_is_refused() {
        let channels = worked_audio();
        let mut meta = written_meta(&channels);
        meta.per_channel.swap(1, 2);
        meta.channel_map.swap(1, 2);
        assert_eq!(
            read_err(&zip_by_hand(&meta, &channels)),
            "read_acsnap: per_channel[1].stream_sha256 (input 7) matches audio.flac stream 2, not 1"
        );
    }

    /// A digest that matches several other streams lists them all.
    #[test]
    fn v2_swap_onto_duplicated_streams_names_every_match() {
        let n = 3 * SR as usize;
        let channels = vec![tone(n, 0.3), tone(n, 0.15), tone(n, 0.15)];
        let mut meta = written_meta(&channels);
        meta.per_channel.swap(0, 2);
        assert_eq!(
            read_err(&zip_by_hand(&meta, &channels)),
            "read_acsnap: per_channel[0].stream_sha256 (input 7) matches audio.flac streams 1 \
             and 2, not 0"
        );
    }

    #[test]
    fn v2_digest_matching_no_stream_is_refused() {
        let channels = short_audio(3);
        let mut meta = with_digests(&control_meta(), &channels);
        meta.per_channel[0].stream_sha256 = Some("0".repeat(64));
        assert_eq!(
            read_err(&zip_by_hand(&meta, &channels)),
            "read_acsnap: per_channel[0].stream_sha256 (input 4) matches no audio.flac stream"
        );
    }

    #[test]
    fn v2_entry_without_a_digest_is_refused() {
        let channels = short_audio(3);
        let mut meta = with_digests(&control_meta(), &channels);
        meta.per_channel[1].stream_sha256 = None;
        assert_eq!(
            read_err(&zip_by_hand(&meta, &channels)),
            "read_acsnap: per_channel[1].stream_sha256 missing; format_version 3 requires it"
        );
    }

    #[test]
    fn v1_entry_carrying_a_digest_is_refused() {
        let channels = short_audio(3);
        let mut meta = v1_control_meta();
        meta.per_channel[0].stream_sha256 = flac::stream_digests(&channels).pop();
        assert_eq!(
            read_err(&zip_by_hand(&meta, &channels)),
            "read_acsnap: per_channel[0].stream_sha256 present, but format_version 1 has no \
             such field"
        );
    }

    #[test]
    fn v2_malformed_digest_is_refused_before_rule_7() {
        let channels = short_audio(3);
        let good = with_digests(&control_meta(), &channels);
        let right = good.per_channel[1].stream_sha256.clone().unwrap();
        for bad in [
            right.to_uppercase(),
            right[..63].to_string(),
            format!("{right}0"),
        ] {
            let mut meta = good.clone();
            meta.per_channel[1].stream_sha256 = Some(bad.clone());
            assert_eq!(
                read_err(&zip_by_hand(&meta, &channels)),
                "read_acsnap: per_channel[1].stream_sha256 is not 64 lowercase hex characters",
                "digest {bad:?}"
            );
        }
    }

    /// The measured gap: sample-identical streams share a digest, so a swap
    /// between their entries passes. It is harmless — each entry's
    /// calibration moves with it onto identical audio — and this shows it:
    /// both pairs derive exactly what the unswapped file does, with the two
    /// entries carrying different calibrations.
    #[test]
    fn v2_swap_of_sample_identical_streams_is_accepted_and_harmless() {
        let n = 3 * SR as usize;
        let channels = vec![tone(n, 0.15), tone(n, 0.3), tone(n, 0.15)];
        let mut base = control_meta();
        base.per_channel[0].calibration = Some(with_scale());
        let base = with_digests(&base, &channels);
        let mut swapped = base.clone();
        swapped.per_channel.swap(0, 2);

        let a = read_acsnap(&zip_by_hand(&base, &channels)).expect("unswapped reads");
        let b = read_acsnap(&zip_by_hand(&swapped, &channels)).expect("identical swap reads");
        assert_eq!(b.channel_index_for(4), Some(2));
        for pair in 0..2 {
            let da = a.derive_pair(pair, WeightingCurve::Z, None).unwrap();
            let db = b.derive_pair(pair, WeightingCurve::Z, None).unwrap();
            assert_eq!(da.h1.magnitude_db, db.h1.magnitude_db, "pair {pair}");
            assert_eq!(da.h1.phase_deg, db.h1.phase_deg, "pair {pair}");
            assert_eq!(da.h1.coherence, db.h1.coherence, "pair {pair}");
            assert_eq!(da.ref_spectrum, db.ref_spectrum, "pair {pair}");
        }
    }

    /// #637 test 6, multi-ref half: an unmodified two-`"ref"` archive derives
    /// bit-identical H1 for both pairs whether read as a hand-zipped v1 file
    /// or as a `write_acsnap` v2 file of the same samples.
    #[test]
    fn unmodified_multi_ref_derives_identically_from_v1_and_v2() {
        let channels = worked_audio();
        let v1 = read_acsnap(&zip_by_hand(&v1_control_meta(), &channels)).expect("v1 reads");
        let (bytes, _) = write_acsnap(&control_meta(), &channels).expect("write v2");
        let v2 = read_acsnap(&bytes).expect("v2 reads");
        assert_eq!(v2.meta.format_version, FORMAT_VERSION);
        for pair in 0..2 {
            let d1 = v1.derive_pair(pair, WeightingCurve::Z, None).unwrap();
            let d2 = v2.derive_pair(pair, WeightingCurve::Z, None).unwrap();
            assert_eq!(d1.h1.magnitude_db, d2.h1.magnitude_db, "pair {pair}");
            assert_eq!(d1.h1.phase_deg, d2.h1.phase_deg, "pair {pair}");
            assert_eq!(d1.h1.coherence, d2.h1.coherence, "pair {pair}");
        }
    }

    // ---- #221: rule 8, `session.mtw` presence and shape ----

    #[test]
    fn v3_round_trips_session_mtw() {
        use crate::visualize::mtw::replay::MtwProvenance;
        let mut meta = control_meta();
        let prov = MtwProvenance::for_layout(SR, -120, -4_000, 4, 48.0, 20.0, 24_000.0).unwrap();
        meta.session.mtw = Some(vec![Some(prov), None]);
        let (bytes, _) = write_acsnap(&meta, &short_audio(3)).expect("v3 with a ladder writes");
        let snap = read_acsnap(&bytes).expect("and reads");
        assert_eq!(snap.meta.session.mtw, meta.session.mtw);
    }

    #[test]
    fn v3_without_session_mtw_is_refused() {
        let mut meta = control_meta();
        meta.session.mtw = None;
        assert_read_rejects(
            &meta,
            3,
            "session.mtw missing; format_version 3 requires it",
        );
        let err = match write_acsnap(&meta, &short_audio(3)) {
            Ok(_) => panic!("write_acsnap wrote a v3 file with no session.mtw"),
            Err(e) => format!("{e:#}"),
        };
        assert!(err.contains("session.mtw missing"), "{err}");
    }

    #[test]
    fn v3_session_mtw_length_differing_from_pairs_is_refused() {
        let mut meta = control_meta();
        meta.session.mtw = Some(vec![None]);
        assert_read_rejects(
            &meta,
            3,
            "session.mtw has 1 entries but session.pairs has 2",
        );
    }

    #[test]
    fn v2_carrying_session_mtw_is_refused() {
        let meta = SnapshotMeta {
            format_version: 2,
            ..control_meta()
        };
        assert_read_rejects(
            &meta,
            3,
            "session.mtw present, but format_version 2 has no such field",
        );
    }

    #[test]
    fn v2_without_session_mtw_reads_with_no_ladder() {
        let mut meta = SnapshotMeta {
            format_version: 2,
            ..control_meta()
        };
        meta.session.mtw = None;
        let channels = short_audio(3);
        let meta = with_digests(&meta, &channels);
        let snap = read_acsnap(&zip_by_hand(&meta, &channels)).expect("v2 reads");
        assert!(snap.meta.session.mtw.is_none());
    }

    #[test]
    fn streams_phrase_joins_in_ascending_order() {
        use super::streams_phrase;
        assert_eq!(streams_phrase(&[2]), "stream 2");
        assert_eq!(streams_phrase(&[2, 3]), "streams 2 and 3");
        assert_eq!(streams_phrase(&[2, 3, 5]), "streams 2, 3 and 5");
    }
}
