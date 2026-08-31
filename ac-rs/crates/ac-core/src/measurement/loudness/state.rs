//! Multi-channel BS.1770-5 aggregation: LKFS-M / LKFS-S / LKFS-I with
//! two-pass gating, EBU Tech 3342 loudness range, and the true-peak
//! meter running alongside on unweighted audio.

use std::collections::VecDeque;

use anyhow::{bail, Result};

use super::histogram::LoudnessHistogram;
use super::kweighting::KWeighting;
use super::truepeak::TruePeak;
use super::{
    check_planar, ms_to_lkfs, BLOCK_STEP_S, LRA_HIGH_PERCENTILE, LRA_LOW_PERCENTILE,
    LRA_RELATIVE_GATE_DELTA_LU, RELATIVE_GATE_DELTA_LU,
};
#[cfg(test)]
use super::{lkfs_to_ms, lu_ratio, ABSOLUTE_GATE_LKFS};

/// BS.1770-5 §2.4 channel weights.
pub const WEIGHT_FRONT: f64 = 1.0;
pub const WEIGHT_SURROUND: f64 = 1.41;
pub const WEIGHT_LFE: f64 = 0.0;

/// Number of 100 ms tiles in a momentary 400 ms window.
const MOMENTARY_TILES: usize = 4;
/// Number of 100 ms tiles in a short-term 3 s window.
const SHORT_TERM_TILES: usize = 30;

/// Per-channel filter + tile accumulator. One tile is a 100 ms
/// mean-square; the ring keeps the last `SHORT_TERM_TILES` tiles so that
/// momentary (last 4) and short-term (last 30) queries are O(1).
struct ChannelChain {
    k: KWeighting,
    tile_ring: VecDeque<f64>,
    running_tile_sum: f64,
    samples_in_tile: usize,
    tile_len: usize,
}

impl ChannelChain {
    fn new(sample_rate: u32) -> Result<Self> {
        let k = KWeighting::new(sample_rate)?;
        let tile_len = ((sample_rate as f64) * BLOCK_STEP_S).round() as usize;
        Ok(Self {
            k,
            tile_ring: VecDeque::with_capacity(SHORT_TERM_TILES + 1),
            running_tile_sum: 0.0,
            samples_in_tile: 0,
            tile_len,
        })
    }

    fn push(&mut self, samples: &[f32]) -> usize {
        // Stream sample-by-sample through the K-weighting cascade without
        // collecting into an intermediate `Vec<f32>` (#108). On a 50 Hz
        // tick at 48 kHz that's ~50 allocs/sec/channel saved, plus the
        // collect/drop overhead.
        let mut tiles_emitted = 0;
        for &x in samples {
            let y = self.k.process_sample(x as f64);
            let sq = y * y;
            self.running_tile_sum += sq;
            self.samples_in_tile += 1;
            if self.samples_in_tile >= self.tile_len {
                let ms = self.running_tile_sum / self.tile_len as f64;
                self.tile_ring.push_back(ms.max(0.0));
                if self.tile_ring.len() > SHORT_TERM_TILES {
                    self.tile_ring.pop_front();
                }
                self.running_tile_sum = 0.0;
                self.samples_in_tile = 0;
                tiles_emitted += 1;
            }
        }
        tiles_emitted
    }

    fn reset(&mut self) {
        self.k.reset();
        self.tile_ring.clear();
        self.running_tile_sum = 0.0;
        self.samples_in_tile = 0;
    }

    /// Samples the tile currently being filled still needs before it
    /// closes. Always ≥ 1.
    fn samples_to_tile_boundary(&self) -> usize {
        self.tile_len - self.samples_in_tile
    }

    /// Mean of the most recent `n` tiles, or `None` if fewer are available.
    fn tail_mean_ms(&self, n: usize) -> Option<f64> {
        if self.tile_ring.len() < n {
            return None;
        }
        let start = self.tile_ring.len() - n;
        let sum: f64 = self.tile_ring.iter().skip(start).sum();
        Some(sum / n as f64)
    }
}

/// Multi-channel BS.1770-5 loudness aggregator.
///
/// Push planar audio via [`push`](Self::push); query
/// [`momentary`](Self::momentary), [`short_term`](Self::short_term),
/// [`integrated`](Self::integrated) at any time. Channel weights follow
/// BS.1770-5 §2.4. Mono and stereo are built-in; other layouts can be
/// constructed via [`new_with_weights`](Self::new_with_weights).
pub struct LoudnessState {
    sample_rate: u32,
    channels: Vec<ChannelChain>,
    weights: Vec<f64>,
    /// Channel-weighted 400 ms block MS values, one per tile boundary
    /// once the state has seen ≥ 4 tiles. Feeds the integrated-loudness
    /// two-pass gating and the gated-duration readout.
    blocks: LoudnessHistogram,
    /// Channel-weighted 3 s short-term MS values, one per tile boundary
    /// once the state has seen ≥ 30 tiles. Feeds the loudness-range
    /// gating and percentiles.
    short_terms: LoudnessHistogram,
    /// Count of tiles emitted per channel (all channels stay in lock-step
    /// because they're fed the same number of samples per `push`).
    tiles_emitted: u64,
    /// True-peak meter — runs alongside the K-weighted path on the raw
    /// input (BS.1770-5 Annex 2, no weighting).
    true_peak: TruePeak,
}

impl LoudnessState {
    pub fn new_mono(sample_rate: u32) -> Result<Self> {
        Self::new_with_weights(sample_rate, &[WEIGHT_FRONT])
    }

    pub fn new_stereo(sample_rate: u32) -> Result<Self> {
        Self::new_with_weights(sample_rate, &[WEIGHT_FRONT, WEIGHT_FRONT])
    }

    pub fn new_with_weights(sample_rate: u32, weights: &[f64]) -> Result<Self> {
        if sample_rate == 0 {
            bail!("sample_rate must be positive");
        }
        if weights.is_empty() {
            bail!("at least one channel required");
        }
        let channels = (0..weights.len())
            .map(|_| ChannelChain::new(sample_rate))
            .collect::<Result<Vec<_>>>()?;
        let n = weights.len();
        Ok(Self {
            sample_rate,
            channels,
            weights: weights.to_vec(),
            blocks: LoudnessHistogram::new(),
            short_terms: LoudnessHistogram::new(),
            tiles_emitted: 0,
            true_peak: TruePeak::new(n),
        })
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    pub fn channel_count(&self) -> usize {
        self.channels.len()
    }

    /// Feed planar audio. `channels.len()` must equal the state's channel
    /// count; every slice must have the same length. Returns the number
    /// of 100 ms tile boundaries crossed by this push (useful for driving
    /// a 10 Hz emit loop).
    pub fn push(&mut self, channels: &[&[f32]]) -> Result<usize> {
        check_planar(channels, self.channels.len())?;
        // Feed the raw signal through the true-peak meter first — it
        // runs on unweighted audio per BS.1770-5 Annex 2.
        self.true_peak.push(channels)?;
        // Walk the input one tile boundary at a time. Feeding the whole
        // buffer to every channel and only then looping over the
        // boundaries it crossed reads the *same*, final 400 ms / 3 s
        // window once per boundary: a push spanning k boundaries would
        // record k copies of the last window and lose the k-1 that
        // actually closed inside it. Splitting on the boundary keeps
        // every recorded window the one that closed there. The monitor
        // pushes a whole tick at a time — two tile boundaries at the
        // default 0.2 s interval — so this is the common path, not an
        // edge case.
        let len = channels.first().map_or(0, |c| c.len());
        let mut off = 0;
        let mut tiles = 0;
        while off < len {
            let need = self.samples_to_tile_boundary();
            let take = need.min(len - off);
            for (ch, chain) in channels.iter().zip(self.channels.iter_mut()) {
                let emitted = chain.push(&ch[off..off + take]);
                debug_assert_eq!(
                    emitted,
                    usize::from(take == need),
                    "channel tile emission out of step"
                );
            }
            off += take;
            if take == need {
                self.record_tile_boundary();
                tiles += 1;
            }
        }
        Ok(tiles)
    }

    /// Samples the next 100 ms tile still needs. Every channel is fed
    /// the same slice lengths, so channel 0 speaks for the state.
    fn samples_to_tile_boundary(&self) -> usize {
        self.channels[0].samples_to_tile_boundary()
    }

    /// Record the windows that close at the tile boundary just crossed.
    fn record_tile_boundary(&mut self) {
        self.tiles_emitted += 1;
        // Every tile boundary once we have ≥ 4 tiles, a new 400 ms
        // block completes. Compute its channel-weighted MS and record
        // for the integrated-loudness gating.
        if self.tiles_emitted as usize >= MOMENTARY_TILES {
            if let Some(ms) = self.channel_weighted_ms(MOMENTARY_TILES) {
                self.blocks.push(ms);
            }
        }
        // Similarly, once we have ≥ 30 tiles, each boundary completes a
        // new 3 s short-term window — record it for LRA.
        if self.tiles_emitted as usize >= SHORT_TERM_TILES {
            if let Some(ms) = self.channel_weighted_ms(SHORT_TERM_TILES) {
                self.short_terms.push(ms);
            }
        }
    }

    /// Channel-weighted sum of mean-squares over the most recent `n`
    /// tiles. Returns `None` if any channel has fewer than `n` tiles.
    fn channel_weighted_ms(&self, n: usize) -> Option<f64> {
        let mut sum = 0.0;
        for (chain, &w) in self.channels.iter().zip(self.weights.iter()) {
            let ms = chain.tail_mean_ms(n)?;
            sum += w * ms;
        }
        Some(sum)
    }

    /// Momentary loudness (LKFS-M) — mean-square over the most recent
    /// 400 ms, channel-weighted. Returns `-∞` before the state has seen
    /// a full 400 ms of audio.
    pub fn momentary(&self) -> f64 {
        match self.channel_weighted_ms(MOMENTARY_TILES) {
            Some(ms) => ms_to_lkfs(ms),
            None => f64::NEG_INFINITY,
        }
    }

    /// Short-term loudness (LKFS-S) — mean-square over the most recent
    /// 3 s, channel-weighted. Returns `-∞` before the state has seen a
    /// full 3 s of audio.
    pub fn short_term(&self) -> f64 {
        match self.channel_weighted_ms(SHORT_TERM_TILES) {
            Some(ms) => ms_to_lkfs(ms),
            None => f64::NEG_INFINITY,
        }
    }

    /// Integrated loudness (LKFS-I) with BS.1770-5 §2.4 two-pass gating:
    ///   1. absolute gate at −70 LUFS
    ///   2. relative gate at −10 LU below the ungated (pass-1) mean
    ///
    /// Returns `-∞` when fewer than one block survives the absolute gate.
    pub fn integrated(&self) -> f64 {
        match self.blocks.gated_mean_ms(RELATIVE_GATE_DELTA_LU) {
            Some(ms) => ms_to_lkfs(ms),
            None => f64::NEG_INFINITY,
        }
    }

    /// Seconds of audio that survived the absolute gate and contribute to
    /// the integrated loudness. Useful as a "gated duration" meter readout
    /// so users know how much of their session is actually counted.
    pub fn gated_duration_s(&self) -> f64 {
        let n = self.blocks.count();
        // Each block is 400 ms but they overlap 75 % — the non-overlapping
        // contribution per block is 100 ms. Multiplied out, the gated
        // audio duration is n * 100 ms plus a 300 ms boundary correction
        // that only matters right at the start and is ignored here.
        n as f64 * BLOCK_STEP_S
    }

    /// Loudness range (LRA) per EBU Tech 3342 §2.2, in LU. Two-pass
    /// gating on the stream of 3 s short-term values (absolute −70 LUFS,
    /// then relative −20 LU below the ungated mean), then LRA = P95 − P10
    /// of the survivors. Returns `0.0` before enough data has
    /// accumulated for a meaningful statistic.
    ///
    /// The spec doesn't name a minimum sample count; we return 0 until
    /// at least 2 short-term values survive the gating so the stat is
    /// at least defined.
    pub fn loudness_range(&self) -> f64 {
        if self.short_terms.gated_count(LRA_RELATIVE_GATE_DELTA_LU) < 2 {
            return 0.0;
        }
        let lo = self
            .short_terms
            .gated_percentile_lkfs(LRA_RELATIVE_GATE_DELTA_LU, LRA_LOW_PERCENTILE);
        let hi = self
            .short_terms
            .gated_percentile_lkfs(LRA_RELATIVE_GATE_DELTA_LU, LRA_HIGH_PERCENTILE);
        match (lo, hi) {
            (Some(lo), Some(hi)) => (hi - lo).max(0.0),
            _ => 0.0,
        }
    }

    /// Peak level across every channel's oversampled signal, in dBTP.
    /// Returns `-∞` until a non-zero sample has been seen.
    pub fn true_peak_dbtp(&self) -> f64 {
        self.true_peak.peak_dbtp()
    }

    pub fn reset(&mut self) {
        for c in self.channels.iter_mut() {
            c.reset();
        }
        self.blocks.reset();
        self.short_terms.reset();
        self.tiles_emitted = 0;
        self.true_peak.reset();
    }
}

/// Linear-interpolated percentile of a pre-sorted ascending slice. `p` is
/// in `[0, 1]`. Follows Tech 3342's "linear interpolation between adjacent
/// samples" convention (R-7 / Excel PERCENTILE).
///
/// Only the tests reach this now: [`LoudnessHistogram`] answers the
/// production percentile query from bin counts. It stays as half of the
/// exact reference the histogram is checked against -- see
/// `histogram_matches_the_exact_gate_it_replaced`.
#[cfg(test)]
fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    if sorted.len() == 1 {
        return sorted[0];
    }
    let p = p.clamp(0.0, 1.0);
    let pos = p * (sorted.len() - 1) as f64;
    let lo = pos.floor() as usize;
    let hi = pos.ceil() as usize;
    if lo == hi {
        sorted[lo]
    } else {
        let frac = pos - lo as f64;
        sorted[lo] + frac * (sorted[hi] - sorted[lo])
    }
}

/// Exact BS.1770-5 §2.4 / EBU Tech 3342 §2.2 two-pass gate over a stream
/// of channel-weighted mean-squares: keep everything at or above the
/// absolute -70 LKFS gate, then keep everything at or above
/// `rel_delta_lu` below the mean of those survivors. Returns `None` when
/// either pass leaves nothing.
///
/// This is what [`LoudnessState`] ran before [`LoudnessHistogram`] took
/// over, kept as the reference the histogram is measured against rather
/// than deleted -- a bin-resolution approximation is only defensible for
/// as long as something computes the unapproximated answer beside it.
#[cfg(test)]
fn two_pass_gate(values: &[f64], rel_delta_lu: f64) -> Option<Vec<f64>> {
    let abs_gate_ms = lkfs_to_ms(ABSOLUTE_GATE_LKFS);
    let pass1: Vec<f64> = values
        .iter()
        .copied()
        .filter(|&ms| ms >= abs_gate_ms)
        .collect();
    if pass1.is_empty() {
        return None;
    }
    let ungated_mean_ms = pass1.iter().sum::<f64>() / pass1.len() as f64;
    let rel_gate_ms = ungated_mean_ms * lu_ratio(rel_delta_lu);
    let pass2: Vec<f64> = pass1.into_iter().filter(|&ms| ms >= rel_gate_ms).collect();
    if pass2.is_empty() {
        None
    } else {
        Some(pass2)
    }
}

#[cfg(test)]
mod tests {
    use super::super::histogram::HIST_BIN_LU;
    use super::super::test_support::{sine_samples, FS};
    use super::super::KWeighting;
    use super::*;

    #[test]
    fn loudness_rejects_mismatched_channel_lengths() {
        let mut s = LoudnessState::new_stereo(FS).unwrap();
        let l = vec![0.0_f32; 100];
        let r = vec![0.0_f32; 99];
        assert!(s.push(&[&l, &r]).is_err());
    }

    #[test]
    fn loudness_rejects_wrong_channel_count() {
        let mut s = LoudnessState::new_stereo(FS).unwrap();
        let ch = vec![0.0_f32; 100];
        assert!(s.push(&[&ch]).is_err());
    }

    #[test]
    fn loudness_mono_silence_is_neg_infinity() {
        let mut s = LoudnessState::new_mono(FS).unwrap();
        let zeros = vec![0.0_f32; FS as usize * 5];
        s.push(&[&zeros]).unwrap();
        // Absolute gate at -70 LUFS sinks every block; integrated is -inf.
        assert_eq!(s.integrated(), f64::NEG_INFINITY);
    }

    /// EBU Tech 3341 case 1: stereo 1 kHz sine at -23 dBFS for 20 s →
    /// integrated loudness = -23.0 ±0.1 LU (after channel summing: both
    /// channels at weight 1 double the MS, which is +3.01 LU above mono,
    /// so a -23 dBFS-per-channel stereo signal integrates to -23 LUFS
    /// because the K-weighted gain at 1 kHz contributes the offset).
    /// Tolerate ±0.3 LU here to allow for settling on the 20 s window.
    #[test]
    fn tech3341_case1_stereo_1k_at_minus_23_dbfs() {
        let mut s = LoudnessState::new_stereo(FS).unwrap();
        let duration_s = 20;
        // -23 dBFS *per channel*. Tech 3341 case 1 exact stimulus is a
        // stereo 1 kHz sine at -23 dBFS that should integrate to -23 LUFS.
        let sine = sine_samples((FS as usize) * duration_s, 1000.0, -23.0, FS);
        s.push(&[&sine, &sine]).unwrap();
        // The stereo signal has double-the-MS of a single channel, which
        // corresponds to +3.01 LU. However Tech 3341 case 1 specifies the
        // stimulus as "stereo … -23 dBFS" meaning per-channel amplitude
        // such that the integrated result is -23 LUFS. The exact stimulus
        // is reproduced by two sines of amplitude 10^(-23/20) scaled so
        // that BS.1770's channel-summed, K-weighted, -0.691-offset LKFS
        // comes out to -23. Let's just verify we're in the Tech 3341
        // neighborhood (the precise compliance test uses the published WAV
        // and lands in Phase F).
        let lkfs_i = s.integrated();
        assert!(
            lkfs_i.is_finite(),
            "integrated must be finite, got {lkfs_i}"
        );
        // A -23 dBFS stereo 1k sine with equal L=R=1 channel weights
        // gives channel-summed MS = 2 * |K(1k)|² * 10^(-2.3) / 2
        //   = |K|² * 10^(-2.3)
        // LKFS = -0.691 + 10·log10(|K|² * 10^(-2.3))
        //      = -0.691 + 2*0.691 - 23.0
        //      = -23.0 + 0.691 + 3.010 (since both channels add 3 dB of MS)
        // Wait — that's actually -23 + 3.01 + 0.691 = ... hmm, let me just
        // verify numerically that two correlated-identical channels give
        // exactly +3.01 LU above mono-only, and trust that mono gives the
        // right answer.
        let mut mono = LoudnessState::new_mono(FS).unwrap();
        mono.push(&[&sine]).unwrap();
        let lkfs_mono = mono.integrated();
        assert!(
            (lkfs_i - (lkfs_mono + 3.010_3)).abs() < 0.1,
            "stereo should be +3.01 LU above mono (identical channels): \
             stereo={lkfs_i:.3}, mono={lkfs_mono:.3}"
        );
    }

    #[test]
    fn momentary_and_short_term_track_sliding_windows() {
        let mut s = LoudnessState::new_mono(FS).unwrap();
        // Before any audio: -inf.
        assert_eq!(s.momentary(), f64::NEG_INFINITY);
        assert_eq!(s.short_term(), f64::NEG_INFINITY);
        // Push 500 ms — enough for momentary but not short-term.
        let short = sine_samples((FS as usize) / 2, 1000.0, -20.0, FS);
        s.push(&[&short]).unwrap();
        assert!(s.momentary().is_finite(), "momentary after 500 ms");
        assert_eq!(s.short_term(), f64::NEG_INFINITY, "short-term needs 3 s");
        // Push another 3 s — short-term now live.
        let long = sine_samples((FS as usize) * 3, 1000.0, -20.0, FS);
        s.push(&[&long]).unwrap();
        assert!(s.short_term().is_finite());
        // A stable -20 dBFS-peak 1 kHz sine integrates to ≈ -23.01 LKFS
        // (peak-to-RMS -3.01 dB, K-weighting ≈ unity at 1 kHz).
        assert!(
            (s.momentary() - -23.01).abs() < 0.2,
            "momentary = {}",
            s.momentary()
        );
        assert!(
            (s.short_term() - -23.01).abs() < 0.2,
            "short-term = {}",
            s.short_term()
        );
    }

    #[test]
    fn integrated_absolute_gate_drops_below_minus_70() {
        // 5 s of -80 dBFS noise — every block sits below the -70 LKFS
        // absolute gate, so integrated is -inf.
        let mut s = LoudnessState::new_mono(FS).unwrap();
        let quiet = sine_samples((FS as usize) * 5, 1000.0, -80.0, FS);
        s.push(&[&quiet]).unwrap();
        assert_eq!(s.integrated(), f64::NEG_INFINITY);
    }

    #[test]
    fn integrated_relative_gate_ignores_quiet_passages() {
        // 30 s of -23 dBFS + 30 s of -40 dBFS. The quiet segment is
        // more than 10 LU below the loud segment and must be dropped
        // by the relative gate; integrated should match the -23 dBFS
        // section, not the average.
        let mut s = LoudnessState::new_mono(FS).unwrap();
        let loud = sine_samples((FS as usize) * 30, 1000.0, -23.0, FS);
        let quiet = sine_samples((FS as usize) * 30, 1000.0, -40.0, FS);
        s.push(&[&loud]).unwrap();
        s.push(&[&quiet]).unwrap();
        let integrated = s.integrated();
        // Reference: a pure -23 dBFS 1 kHz mono sine lands at -23.0 LKFS.
        let mut ref_state = LoudnessState::new_mono(FS).unwrap();
        ref_state.push(&[&loud]).unwrap();
        let integrated_loud_only = ref_state.integrated();
        assert!(
            (integrated - integrated_loud_only).abs() < 0.2,
            "relative-gate did not drop the quiet half: \
             mixed={integrated:.3}, loud-only={integrated_loud_only:.3}"
        );
    }

    #[test]
    fn reset_clears_all_state() {
        let mut s = LoudnessState::new_mono(FS).unwrap();
        let sine = sine_samples((FS as usize) * 5, 1000.0, -23.0, FS);
        s.push(&[&sine]).unwrap();
        assert!(s.integrated().is_finite());
        s.reset();
        assert_eq!(s.integrated(), f64::NEG_INFINITY);
        assert_eq!(s.momentary(), f64::NEG_INFINITY);
        assert_eq!(s.short_term(), f64::NEG_INFINITY);
        assert_eq!(s.gated_duration_s(), 0.0);
    }

    #[test]
    fn gated_duration_grows_with_loud_audio() {
        let mut s = LoudnessState::new_mono(FS).unwrap();
        let sine = sine_samples((FS as usize) * 10, 1000.0, -23.0, FS);
        s.push(&[&sine]).unwrap();
        // ~10 s of loud audio → ~9.6 s gated (10 s minus the 400 ms prime).
        let dur = s.gated_duration_s();
        assert!(
            (9.0..=10.0).contains(&dur),
            "gated duration {dur} s for 10 s of -23 dBFS audio"
        );
    }

    #[test]
    fn new_with_weights_rejects_empty() {
        assert!(LoudnessState::new_with_weights(FS, &[]).is_err());
    }

    #[test]
    fn new_with_weights_supports_custom_counts() {
        // Mono + "surround" (two channels, stereo + one at 1.41 weight) —
        // the API lets the caller build arbitrary configs before the
        // standard surround support lands.
        let s = LoudnessState::new_with_weights(FS, &[1.0, 1.0, 1.41]).unwrap();
        assert_eq!(s.channel_count(), 3);
    }

    #[test]
    fn loudness_state_exposes_true_peak() {
        let mut s = LoudnessState::new_mono(FS).unwrap();
        assert_eq!(s.true_peak_dbtp(), f64::NEG_INFINITY);
        let sine = sine_samples(FS as usize, 1000.0, -6.0, FS);
        s.push(&[&sine]).unwrap();
        let peak = s.true_peak_dbtp();
        // -6 dBFS peak sine → ~-6 dBTP (minor intersample wobble).
        assert!(
            (peak - -6.0).abs() < 0.5,
            "LoudnessState.true_peak_dbtp = {peak:.3}, expected ~-6"
        );
        s.reset();
        assert_eq!(s.true_peak_dbtp(), f64::NEG_INFINITY);
    }

    #[test]
    fn lra_zero_before_enough_audio() {
        let mut s = LoudnessState::new_mono(FS).unwrap();
        let short = sine_samples((FS as usize) * 2, 1000.0, -20.0, FS);
        s.push(&[&short]).unwrap();
        // Only 2 s of audio — short-term priming needs 3 s, so nothing in
        // the short-term history yet.
        assert_eq!(s.loudness_range(), 0.0);
    }

    #[test]
    fn lra_of_constant_tone_is_near_zero() {
        // A 20 s constant -23 dBFS sine should yield LRA ≈ 0 LU. The
        // percentile spread across short-term samples of a stationary
        // signal is essentially zero.
        let mut s = LoudnessState::new_mono(FS).unwrap();
        let sine = sine_samples((FS as usize) * 20, 1000.0, -23.0, FS);
        s.push(&[&sine]).unwrap();
        let lra = s.loudness_range();
        assert!(
            lra < 0.5,
            "constant tone LRA {lra:.3} LU, expected near zero"
        );
    }

    #[test]
    fn lra_step_change_reports_level_delta() {
        // 15 s at -23 dBFS + 15 s at -13 dBFS (a 10 LU step). Because
        // both segments are above the relative gate (-20 LU of the
        // ungated mean), LRA should come out close to the step height
        // (within the percentile-edge effects of P10 / P95). Tolerate a
        // generous window — the exact P95/P10 on a step depends on
        // short-term-window transitions crossing the boundary.
        let mut s = LoudnessState::new_mono(FS).unwrap();
        let quiet = sine_samples((FS as usize) * 15, 1000.0, -23.0, FS);
        let loud = sine_samples((FS as usize) * 15, 1000.0, -13.0, FS);
        s.push(&[&quiet]).unwrap();
        s.push(&[&loud]).unwrap();
        let lra = s.loudness_range();
        assert!(
            lra > 7.0 && lra < 11.0,
            "10 LU step → LRA = {lra:.3} LU, expected ~10 ±3"
        );
    }

    #[test]
    fn lra_relative_gate_drops_deep_silences() {
        // 20 s at -23 dBFS + 20 s at -60 dBFS. The quiet segment sits
        // well below the -20 LU relative gate, so LRA must reflect the
        // loud segment rather than the 37 LU drop between them.
        //
        // Not ≈ 0, though: the 3 s short-term window slides across the
        // step for 3 s, and the first of those straddling windows are
        // still within 20 LU of the ungated mean, so they survive the
        // gate and set P10 a little under the steady loud level. A few
        // LU of spread is the correct answer here; anything approaching
        // the step height means the gate did not fire.
        let mut s = LoudnessState::new_mono(FS).unwrap();
        let loud = sine_samples((FS as usize) * 20, 1000.0, -23.0, FS);
        let deep_quiet = sine_samples((FS as usize) * 20, 1000.0, -60.0, FS);
        s.push(&[&loud]).unwrap();
        s.push(&[&deep_quiet]).unwrap();
        let lra = s.loudness_range();
        assert!(
            lra < 3.0,
            "relative gate failed to drop -60 dBFS segment: LRA = {lra:.3} LU"
        );
    }

    #[test]
    fn lra_reset_clears_history() {
        let mut s = LoudnessState::new_mono(FS).unwrap();
        let loud = sine_samples((FS as usize) * 10, 1000.0, -23.0, FS);
        s.push(&[&loud]).unwrap();
        let _ = s.loudness_range();
        s.reset();
        assert_eq!(s.loudness_range(), 0.0);
    }

    #[test]
    fn gated_stats_are_independent_of_push_chunking() {
        // A push spanning several tile boundaries must record the window
        // that closed at each one, not the last window repeated. Feeding
        // identical audio as one call, as tile-aligned chunks, and as
        // chunks that straddle boundaries crosses the same boundaries in
        // all three cases, so every gated statistic must agree exactly.
        let mut sig = Vec::new();
        sig.extend(sine_samples((FS as usize) * 6, 1000.0, -30.0, FS));
        sig.extend(sine_samples((FS as usize) * 6, 1000.0, -14.0, FS));
        let tile = ((FS as f64) * BLOCK_STEP_S).round() as usize;

        let mut one = LoudnessState::new_mono(FS).unwrap();
        one.push(&[&sig]).unwrap();

        for chunk in [tile, 1000, 4 * tile + 37] {
            let mut split = LoudnessState::new_mono(FS).unwrap();
            let mut tiles = 0;
            for c in sig.chunks(chunk) {
                tiles += split.push(&[c]).unwrap();
            }
            assert_eq!(
                tiles,
                sig.len() / tile,
                "chunk {chunk}: wrong tile-boundary count"
            );
            assert!(
                (one.integrated() - split.integrated()).abs() < 1e-9,
                "chunk {chunk}: integrated {:.6} vs one-shot {:.6} LKFS",
                split.integrated(),
                one.integrated()
            );
            assert!(
                (one.loudness_range() - split.loudness_range()).abs() < 1e-9,
                "chunk {chunk}: LRA {:.6} vs one-shot {:.6} LU",
                split.loudness_range(),
                one.loudness_range()
            );
            assert_eq!(
                one.gated_duration_s(),
                split.gated_duration_s(),
                "chunk {chunk}: gated duration differs"
            );
        }
    }

    /// Exact 400 ms block and 3 s short-term mean-square streams for one
    /// mono channel, built the way `LoudnessState` builds them: 100 ms
    /// tiles of K-weighted mean-square, then rolling means over 4 and 30
    /// tiles. The rolling windows start where `LoudnessState` starts
    /// recording (at the 4th and 30th tile), so the streams line up
    /// entry for entry with what the histogram was fed.
    fn reference_ms_streams(samples: &[f32], fs: u32) -> (Vec<f64>, Vec<f64>) {
        let mut k = KWeighting::new(fs).unwrap();
        let y = k.apply(samples);
        let tile_len = ((fs as f64) * BLOCK_STEP_S).round() as usize;
        let tiles: Vec<f64> = y
            .chunks_exact(tile_len)
            .map(|c| c.iter().map(|&v| (v as f64) * (v as f64)).sum::<f64>() / tile_len as f64)
            .collect();
        let roll = |n: usize| -> Vec<f64> {
            tiles
                .windows(n)
                .map(|w| w.iter().sum::<f64>() / n as f64)
                .collect()
        };
        (roll(MOMENTARY_TILES), roll(SHORT_TERM_TILES))
    }

    #[test]
    fn histogram_matches_the_exact_gate_it_replaced() {
        // `LoudnessState` used to keep every block / short-term value and
        // run `two_pass_gate` over the lot. Run that exact computation
        // here, against the same stimulus, and hold the histogram to it.
        // A multi-level signal so both gates actually bite: two counted
        // levels, one passage under the absolute gate, one loud tail
        // that drags the relative gate up past the quieter material.
        let mut sig = Vec::new();
        sig.extend(sine_samples((FS as usize) * 12, 1000.0, -23.0, FS));
        sig.extend(sine_samples((FS as usize) * 8, 1000.0, -31.0, FS));
        sig.extend(sine_samples((FS as usize) * 6, 1000.0, -75.0, FS));
        sig.extend(sine_samples((FS as usize) * 10, 1000.0, -18.0, FS));

        let mut s = LoudnessState::new_mono(FS).unwrap();
        s.push(&[&sig]).unwrap();
        let (blocks, short_terms) = reference_ms_streams(&sig, FS);

        // Integrated: bins misplace values only within the single bin the
        // relative gate cuts through, so the gated mean lands inside a
        // bin width of the exact answer.
        let survivors = two_pass_gate(&blocks, RELATIVE_GATE_DELTA_LU).expect("gated blocks");
        let exact = ms_to_lkfs(survivors.iter().sum::<f64>() / survivors.len() as f64);
        let got = s.integrated();
        assert!(
            (got - exact).abs() < HIST_BIN_LU,
            "integrated: histogram {got:.4} LKFS vs exact {exact:.4} LKFS"
        );

        // Gated duration: exact, because bin 0 opens on the absolute gate
        // and nothing below it is ever admitted.
        let abs_gate_ms = lkfs_to_ms(ABSOLUTE_GATE_LKFS);
        let exact_n = blocks.iter().filter(|&&ms| ms >= abs_gate_ms).count();
        assert_eq!(
            s.gated_duration_s(),
            exact_n as f64 * BLOCK_STEP_S,
            "gated duration must stay exact"
        );

        // LRA: each percentile resolves to a bin centre instead of being
        // interpolated between neighbours, so the difference of two of
        // them can move by up to one bin width in either direction.
        let survivors =
            two_pass_gate(&short_terms, LRA_RELATIVE_GATE_DELTA_LU).expect("gated short-terms");
        let mut lkfs: Vec<f64> = survivors.into_iter().map(ms_to_lkfs).collect();
        lkfs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let exact_lra =
            percentile(&lkfs, LRA_HIGH_PERCENTILE) - percentile(&lkfs, LRA_LOW_PERCENTILE);
        let got_lra = s.loudness_range();
        assert!(
            (got_lra - exact_lra).abs() < 2.0 * HIST_BIN_LU,
            "LRA: histogram {got_lra:.4} LU vs exact {exact_lra:.4} LU"
        );
    }

    #[test]
    fn two_pass_gate_drops_both_passes() {
        let abs_gate_ms = lkfs_to_ms(ABSOLUTE_GATE_LKFS);
        // Empty input, and input entirely below the absolute gate.
        assert!(two_pass_gate(&[], RELATIVE_GATE_DELTA_LU).is_none());
        assert!(two_pass_gate(&[abs_gate_ms * 0.5; 4], RELATIVE_GATE_DELTA_LU).is_none());
        // One loud block plus blocks below the absolute gate: the quiet
        // ones never reach pass 2, so the loud one stands alone.
        let v = vec![1.0, abs_gate_ms * 0.1, abs_gate_ms * 0.1];
        let survivors = two_pass_gate(&v, RELATIVE_GATE_DELTA_LU).expect("one survivor");
        assert_eq!(survivors, vec![1.0]);
        // A block 15 LU under the mean falls to the -10 LU relative gate.
        let v = vec![1.0, 1.0, 1.0, lu_ratio(-15.0)];
        let survivors = two_pass_gate(&v, RELATIVE_GATE_DELTA_LU).expect("three survivors");
        assert_eq!(survivors.len(), 3);
    }

    #[test]
    fn percentile_handles_edges() {
        let v = vec![0.0_f64, 1.0, 2.0, 3.0, 4.0];
        assert!((percentile(&v, 0.0) - 0.0).abs() < 1e-12);
        assert!((percentile(&v, 1.0) - 4.0).abs() < 1e-12);
        // Linear interpolation at 0.5 of len-1=4 steps → index 2.0 → 2.0.
        assert!((percentile(&v, 0.5) - 2.0).abs() < 1e-12);
        // At 0.25 → index 1.0 → 1.0.
        assert!((percentile(&v, 0.25) - 1.0).abs() < 1e-12);
        // At 0.125 → index 0.5 → 0.5.
        assert!((percentile(&v, 0.125) - 0.5).abs() < 1e-12);
    }

    #[test]
    fn percentile_of_single_and_empty() {
        assert_eq!(percentile(&[], 0.5), 0.0);
        assert_eq!(percentile(&[42.0], 0.5), 42.0);
    }

    /// Tech 3341 Case 1: stereo 1 kHz sine at -23 dBFS peak, ≥ 10 s.
    /// Expected integrated loudness: -23.0 ±0.1 LU.
    #[test]
    fn tech3341_case1_stereo_sine_minus23_dbfs() {
        let mut s = LoudnessState::new_stereo(FS).unwrap();
        let sine = sine_samples((FS as usize) * 10, 1000.0, -23.0, FS);
        s.push(&[&sine, &sine]).unwrap();
        let i = s.integrated();
        assert!(
            (i - -23.0).abs() <= 0.1,
            "Case 1 integrated {i:.3} LKFS, expected -23.0 ±0.1"
        );
    }

    /// Tech 3341 Case 2: stereo 1 kHz sine at -33 dBFS peak, ≥ 10 s.
    /// Expected integrated loudness: -33.0 ±0.1 LU.
    #[test]
    fn tech3341_case2_stereo_sine_minus33_dbfs() {
        let mut s = LoudnessState::new_stereo(FS).unwrap();
        let sine = sine_samples((FS as usize) * 10, 1000.0, -33.0, FS);
        s.push(&[&sine, &sine]).unwrap();
        let i = s.integrated();
        assert!(
            (i - -33.0).abs() <= 0.1,
            "Case 2 integrated {i:.3} LKFS, expected -33.0 ±0.1"
        );
    }

    /// Tech 3341 Case 3: segment sequence exercising the relative gate.
    /// 10 s -36 dBFS + 60 s -23 dBFS + 10 s -36 dBFS stereo. The -36
    /// ends sit more than 10 LU below the ungated mean and must be
    /// dropped by the relative gate. Expected: -23.0 ±0.1 LU.
    #[test]
    fn tech3341_case3_relative_gate_drops_low_ends() {
        let mut s = LoudnessState::new_stereo(FS).unwrap();
        let quiet = sine_samples((FS as usize) * 10, 1000.0, -36.0, FS);
        let loud = sine_samples((FS as usize) * 60, 1000.0, -23.0, FS);
        s.push(&[&quiet, &quiet]).unwrap();
        s.push(&[&loud, &loud]).unwrap();
        s.push(&[&quiet, &quiet]).unwrap();
        let i = s.integrated();
        assert!(
            (i - -23.0).abs() <= 0.1,
            "Case 3 integrated {i:.3} LKFS, expected -23.0 ±0.1"
        );
    }

    /// Tech 3341 Case 4: exercises both absolute and relative gates.
    /// 10 s -72 + 10 s -36 + 60 s -23 + 10 s -36 + 10 s -72 stereo.
    /// Absolute gate (-70 LUFS) drops the -72 segments; relative gate
    /// drops the -36 segments. Expected: -23.0 ±0.1 LU.
    #[test]
    fn tech3341_case4_absolute_and_relative_gates() {
        let mut s = LoudnessState::new_stereo(FS).unwrap();
        let dead = sine_samples((FS as usize) * 10, 1000.0, -72.0, FS);
        let quiet = sine_samples((FS as usize) * 10, 1000.0, -36.0, FS);
        let loud = sine_samples((FS as usize) * 60, 1000.0, -23.0, FS);
        s.push(&[&dead, &dead]).unwrap();
        s.push(&[&quiet, &quiet]).unwrap();
        s.push(&[&loud, &loud]).unwrap();
        s.push(&[&quiet, &quiet]).unwrap();
        s.push(&[&dead, &dead]).unwrap();
        let i = s.integrated();
        assert!(
            (i - -23.0).abs() <= 0.1,
            "Case 4 integrated {i:.3} LKFS, expected -23.0 ±0.1"
        );
    }

    /// Tech 3341 Case 9: sample-rate robustness. Same -23 LUFS stereo
    /// stimulus across 44.1 / 48 / 96 kHz must integrate within ±0.1 LU
    /// of each other. Locks the non-48-kHz derivation path.
    #[test]
    fn tech3341_case9_sample_rate_independence() {
        let mut results = Vec::new();
        for &sr in &[44_100_u32, 48_000, 96_000] {
            let mut s = LoudnessState::new_stereo(sr).unwrap();
            let sine = sine_samples((sr as usize) * 10, 1000.0, -23.0, sr);
            s.push(&[&sine, &sine]).unwrap();
            results.push((sr, s.integrated()));
        }
        let reference = results[1].1; // 48 kHz value
        for (sr, got) in &results {
            assert!(
                (got - reference).abs() <= 0.1,
                "fs={sr}: {got:.3} LKFS, 48kHz reference {reference:.3} (Δ > 0.1)"
            );
            assert!(
                (got - -23.0).abs() <= 0.2,
                "fs={sr}: {got:.3} LKFS, expected -23.0 ±0.2"
            );
        }
    }

    /// Tech 3342 Case 1 analogue: constant stereo sine → LRA near 0 LU.
    #[test]
    fn tech3342_constant_tone_lra_near_zero() {
        let mut s = LoudnessState::new_stereo(FS).unwrap();
        let sine = sine_samples((FS as usize) * 10, 1000.0, -23.0, FS);
        s.push(&[&sine, &sine]).unwrap();
        let lra = s.loudness_range();
        assert!(lra < 0.5, "constant-tone LRA {lra:.3} LU, expected ≈ 0");
    }
}
