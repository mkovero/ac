/// Channel-index → frequency offset, in Hz. Picked so that two channels
/// never alias into the same FFT bin at common analysis lengths.
pub(super) const CHANNEL_OFFSET_HZ: f64 = 100.0;

/// What `capture_block` / `capture_stereo` synthesize. `Tones` covers both
/// the historical single-tone `set_tone` path and the multi-tone
/// `set_tone_pair` path added for the display-truth harness (#170) — a
/// single-element vec reproduces the old behaviour exactly. `Noise` is a
/// deterministic pseudo-random broadband signal for the I2 flat-noise
/// continuity invariant; deterministic (fixed LCG per channel offset) so
/// harness runs are reproducible. `CorrelatedPair` (handoff: parity-
/// completion M1.5) is a fake DUT with known ground truth: the ref-role
/// port carries a seeded broadband source, the meas-role port carries the
/// *same* source scaled and delayed — `|H1| = gain`, coherence ≈ 1.
#[derive(Clone)]
pub(super) enum Stimulus {
    Tones(Vec<(f64, f64)>),
    Noise(f64),
    CorrelatedPair { gain: f64, delay_samples: usize },
}

/// Fixed seed for `CorrelatedPair` — deterministic across runs so
/// fixture regeneration (`ac_core::snapshot`'s regenerator test) is
/// reproducible: same seed, same stimulus, same `.acsnap` bytes, same
/// sha256, every time.
pub(super) const CORRELATED_PAIR_SEED: u64 = 0xC0FFEE_C0FFEE_u64;

/// Deterministic pseudo-random sample at absolute index `index`, in
/// `[-1, 1)`. A *pure* function of `(seed, index)` — unlike `Stimulus::
/// Noise`'s sequentially-advanced LCG, this needs to be independently
/// seekable at arbitrary (possibly negative-relative, i.e. "before the
/// source existed") offsets, since the meas-role reads the same
/// underlying stream `delay_samples` behind the ref-role's position with
/// no shared mutable cursor between the two (call order between meas and
/// ref within one tick is not guaranteed — see `make_samples_for`).
/// SplitMix64's finalizer — good avalanche, no persistent state needed.
pub(super) fn correlated_source_at(seed: u64, index: u64) -> f32 {
    let mut z = seed.wrapping_add(index.wrapping_mul(0x9E37_79B9_7F4A_7C15));
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^= z >> 31;
    let u = ((z >> 40) as f64 / (1u64 << 24) as f64) * 2.0 - 1.0;
    u as f32
}

impl Default for Stimulus {
    fn default() -> Self {
        Stimulus::Tones(vec![(1_000.0, 0.0)])
    }
}
