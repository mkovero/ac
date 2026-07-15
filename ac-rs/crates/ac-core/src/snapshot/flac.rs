//! 24-bit multichannel FLAC encode/decode for `.acsnap` (handoff:
//! snapshot-backend M1, decision 4).
//!
//! Encode via `flacenc` (its primary, non-experimental purpose). Decode via
//! `claxon`, a mature, focused, non-experimental pure-Rust FLAC decoder —
//! deliberately **not** `flacenc`'s own `decode` feature, which its own
//! README labels "(experimental)". Both crates are pure Rust, no system
//! library dependency (required — `ac-core` will be linked directly by
//! `ac-view`, D8, on whatever platform that ships on).
//!
//! Samples cross the FLAC boundary as i32 on the 24-bit grid
//! (`-2^23 .. 2^23-1`). `f32 → i24`: scale by `2^23`, round, saturate.
//! Samples that already sit on the i24 grid (real hardware, 24-bit
//! converters) round-trip bit-exact; synthetic `--fake-audio` f32 that
//! doesn't quantizes at the i24 LSB, ≈ −138 dBFS
//! (`20·log10(1/2^23) ≈ -138.99`) — the I-B parity tolerance in the
//! acceptance tests accounts for exactly this floor and nothing more.

use anyhow::{anyhow, Context, Result};
use flacenc::error::Verify;

/// 24-bit signed integer full-scale magnitude (`2^23`).
const I24_SCALE: f64 = 8_388_608.0; // 2^23
const I24_MIN: i32 = -8_388_608; // -2^23
const I24_MAX: i32 = 8_388_607; // 2^23 - 1

/// `f32 → i24` (stored in `i32`): scale by `2^23`, round to nearest,
/// saturate to the 24-bit range. Values outside `[-1.0, 1.0)` (shouldn't
/// occur for a well-behaved capture, but a snapshot must never panic on
/// out-of-range input) clamp rather than wrap.
fn f32_to_i24(x: f32) -> i32 {
    let scaled = (x as f64 * I24_SCALE).round();
    if scaled.is_nan() {
        0
    } else {
        scaled.clamp(I24_MIN as f64, I24_MAX as f64) as i32
    }
}

/// `i24 → f32`, exact inverse of the scale (not the clamp) in
/// [`f32_to_i24`].
fn i24_to_f32(x: i32) -> f32 {
    (x as f64 / I24_SCALE) as f32
}

/// Encode `n_channels` interleaved f32 channels into a 24-bit FLAC byte
/// stream. `channels[c]` holds channel `c`'s samples; all channels must be
/// the same length.
pub fn encode(channels: &[Vec<f32>], sr: u32) -> Result<Vec<u8>> {
    let n_channels = channels.len();
    if n_channels == 0 {
        return Err(anyhow!("encode: no channels"));
    }
    let n_frames = channels[0].len();
    if channels.iter().any(|c| c.len() != n_frames) {
        return Err(anyhow!("encode: channels have mismatched lengths"));
    }
    // Below this, flacenc silently emits a stream too short for its own
    // format constraints to produce a valid final block — better to fail
    // clearly here (e.g. a `snapshot` requested moments after a transfer
    // session starts, before the ring has meaningfully filled) than hand
    // back a `.acsnap` that fails to decode.
    if n_frames < flacenc::constant::MIN_BLOCK_SIZE {
        return Err(anyhow!(
            "encode: {n_frames} frames is below the {}-frame minimum FLAC block size",
            flacenc::constant::MIN_BLOCK_SIZE
        ));
    }

    let mut interleaved = Vec::with_capacity(n_frames * n_channels);
    for i in 0..n_frames {
        for ch in channels {
            interleaved.push(f32_to_i24(ch[i]));
        }
    }

    let config = flacenc::config::Encoder::default()
        .into_verified()
        .map_err(|(_cfg, e)| anyhow!("flacenc config invalid: {e:?}"))?;
    let source = flacenc::source::MemSource::from_samples(
        &interleaved,
        n_channels,
        24, // bits_per_sample
        sr as usize,
    );
    let stream = flacenc::encode_with_fixed_block_size(&config, source, config.block_size)
        .map_err(|e| anyhow!("flacenc encode failed: {e:?}"))?;

    use flacenc::component::BitRepr;
    let mut sink = flacenc::bitsink::ByteSink::new();
    stream
        .write(&mut sink)
        .map_err(|e| anyhow!("flacenc bitstream write failed: {e:?}"))?;
    Ok(sink.as_slice().to_vec())
}

/// Decode a 24-bit FLAC byte stream back into per-channel f32 samples.
/// Returns `(channels, sr)`.
pub fn decode(flac_bytes: &[u8]) -> Result<(Vec<Vec<f32>>, u32)> {
    let cursor = std::io::Cursor::new(flac_bytes);
    let mut reader = claxon::FlacReader::new(cursor).context("claxon: failed to open stream")?;
    let info = reader.streaminfo();
    let n_channels = info.channels as usize;
    let sr = info.sample_rate;
    if n_channels == 0 {
        return Err(anyhow!("decode: streaminfo reports 0 channels"));
    }

    let mut channels: Vec<Vec<f32>> = vec![Vec::new(); n_channels];
    for (i, sample) in reader.samples().enumerate() {
        let s = sample.context("claxon: sample decode error")?;
        channels[i % n_channels].push(i24_to_f32(s));
    }
    Ok((channels, sr))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(n: usize, freq_hz: f64, sr: f64, amp: f32) -> Vec<f32> {
        (0..n)
            .map(|i| {
                (amp as f64 * (2.0 * std::f64::consts::PI * freq_hz * i as f64 / sr).sin()) as f32
            })
            .collect()
    }

    #[test]
    fn f32_i24_round_trip_on_grid() {
        // Values that sit exactly on the i24 grid must round-trip exactly.
        for &i in &[0i32, 1, -1, I24_MAX, I24_MIN, 12345, -54321] {
            let f = i24_to_f32(i);
            assert_eq!(f32_to_i24(f), i, "i={i} f={f}");
        }
    }

    #[test]
    fn f32_i24_off_grid_error_bounded_by_one_lsb() {
        let one_lsb = (1.0 / I24_SCALE) as f32;
        for x in [0.1_f32, -0.33333, 0.99999, 0.0001] {
            let i = f32_to_i24(x);
            let back = i24_to_f32(i);
            assert!(
                (back - x).abs() <= one_lsb,
                "x={x} back={back} exceeds 1 LSB ({one_lsb})"
            );
        }
    }

    #[test]
    fn f32_i24_saturates_out_of_range() {
        assert_eq!(f32_to_i24(2.0), I24_MAX);
        assert_eq!(f32_to_i24(-2.0), I24_MIN);
        assert_eq!(f32_to_i24(f32::NAN), 0);
    }

    /// AC #4 core claim: 24-bit, 4-channel round-trip is bit-exact for
    /// on-grid input. This is the empirical proof the architect's
    /// verify-first spike (decision 4) demanded — crate docs alone don't
    /// confirm multichannel/24-bit support, actually running it does.
    #[test]
    fn encode_decode_round_trip_4ch_24bit_bit_exact_on_grid() {
        let sr = 48_000u32;
        let n = 4_800; // 0.1 s
        let channels: Vec<Vec<f32>> = (0..4)
            .map(|ch| {
                sine(n, 200.0 + ch as f64 * 137.0, sr as f64, 0.4)
                    .into_iter()
                    .map(|x| i24_to_f32(f32_to_i24(x))) // snap to grid first
                    .collect()
            })
            .collect();

        let flac_bytes = encode(&channels, sr).expect("encode");
        let (decoded, decoded_sr) = decode(&flac_bytes).expect("decode");

        assert_eq!(decoded_sr, sr);
        assert_eq!(decoded.len(), 4, "channel count must round-trip");
        for (ch, (orig, got)) in channels.iter().zip(decoded.iter()).enumerate() {
            assert_eq!(orig.len(), got.len(), "ch{ch} length mismatch");
            for (i, (&o, &g)) in orig.iter().zip(got.iter()).enumerate() {
                assert_eq!(
                    f32_to_i24(o),
                    f32_to_i24(g),
                    "ch{ch} sample {i}: {o} != {g} (bit-exact on i24 grid)"
                );
            }
        }
    }

    /// Off-grid synthetic f32 (not pre-snapped) still round-trips within
    /// the documented 1-LSB quantization floor — the realistic
    /// `--fake-audio` case, not the idealized on-grid case above.
    #[test]
    fn encode_decode_round_trip_off_grid_within_one_lsb() {
        let sr = 48_000u32;
        let n = 4_800;
        let channels: Vec<Vec<f32>> = vec![sine(n, 1_000.0, sr as f64, 0.3162)]; // -10 dBFS-ish

        let flac_bytes = encode(&channels, sr).expect("encode");
        let (decoded, _) = decode(&flac_bytes).expect("decode");

        let one_lsb = (1.0 / I24_SCALE) as f32;
        for (i, (&o, &g)) in channels[0].iter().zip(decoded[0].iter()).enumerate() {
            assert!(
                (o - g).abs() <= one_lsb * 1.01, // tiny float-compare slack
                "sample {i}: {o} vs {g}, diff {} exceeds 1 LSB",
                (o - g).abs()
            );
        }
    }

    #[test]
    fn encode_rejects_mismatched_channel_lengths() {
        let channels = vec![vec![0.0_f32; 100], vec![0.0_f32; 99]];
        assert!(encode(&channels, 48_000).is_err());
    }

    #[test]
    fn encode_rejects_empty_channels() {
        let channels: Vec<Vec<f32>> = vec![];
        assert!(encode(&channels, 48_000).is_err());
    }

    /// AC #5-adjacent edge case: a `snapshot` requested moments after a
    /// transfer session starts, before the ring has meaningfully filled.
    /// Must fail clearly, not hand back an undecodable file.
    #[test]
    fn encode_rejects_below_minimum_block_size() {
        let channels = vec![vec![0.0_f32; 4]];
        let err = encode(&channels, 48_000).expect_err("4 frames must be rejected");
        assert!(
            err.to_string().contains("minimum"),
            "expected a minimum-block-size error, got: {err}"
        );
    }
}
