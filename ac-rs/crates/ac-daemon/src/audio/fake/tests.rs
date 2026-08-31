//! Unit tests for the fake backend.

use super::stimulus::channel_index;
use super::*;
use std::f64::consts::PI;

/// Goertzel magnitude at `freq`, normalised by length — enough to confirm
/// energy landed where a tone was requested without pulling in a full FFT
/// for a unit test. `freq` is snapped to the nearest bin, so a caller
/// comparing two frequencies should pick ones that fall on bins at the
/// length it captures.
fn goertzel_mag(samples: &[f32], sr: f64, freq: f64) -> f64 {
    let n = samples.len();
    let k = (0.5 + (n as f64 * freq) / sr).floor();
    let w = 2.0 * PI * k / n as f64;
    let cw = w.cos();
    let coeff = 2.0 * cw;
    let (mut s1, mut s2) = (0.0_f64, 0.0_f64);
    for &x in samples {
        let s0 = x as f64 + coeff * s1 - s2;
        s2 = s1;
        s1 = s0;
    }
    (s1 * s1 + s2 * s2 - s1 * s2 * coeff).sqrt() / n as f64
}

/// `meas[i]` must equal `gain * refch[i - delay]` for every `i` past the
/// initial `delay` samples, which are silence — the `CorrelatedPair` ground
/// truth, asserted against the captured arrays rather than a "they differ"
/// proxy.
fn assert_delayed_scaled(meas: &[f32], refch: &[f32], gain: f64, delay: usize, what: &str) {
    for (i, &m) in meas.iter().enumerate().take(delay) {
        assert_eq!(
            m, 0.0,
            "{what}: meas[{i}] should be silence before delay elapses"
        );
    }
    for i in delay..meas.len() {
        let expected = gain as f32 * refch[i - delay];
        assert!(
            (meas[i] - expected).abs() < 1e-6,
            "{what}: meas[{i}]={} expected {expected} (= {gain} * ref[{}]={})",
            meas[i],
            i - delay,
            refch[i - delay]
        );
    }
}

#[test]
fn channel_index_parses_trailing_number() {
    assert_eq!(channel_index("fake:capture_0"), 0);
    assert_eq!(channel_index("fake:capture_7"), 7);
    assert_eq!(channel_index("fake:capture_19"), 19);
    assert_eq!(channel_index("garbage"), 0);
}

#[test]
fn reroute_shifts_effective_frequency() {
    let mut eng = FakeEngine::new();
    eng.set_tone(1_000.0, 0.5);
    eng.reconnect_input("fake:capture_0").unwrap();
    assert!((eng.gen.effective_freq(eng.input_port.as_deref()) - 1_000.0).abs() < 1e-9);
    eng.reconnect_input("fake:capture_3").unwrap();
    assert!((eng.gen.effective_freq(eng.input_port.as_deref()) - 1_300.0).abs() < 1e-9);
}

#[test]
fn capture_multi_matches_stereo_default() {
    // Fake backend inherits the default `capture_multi` which calls
    // `capture_stereo` — covers the CPAL fallback path too.
    //
    // **Two buffers here is the two-channel case, not the contract.**
    // This test previously read as the latter, and #254 is what that cost:
    // `capture_multi` returned a fixed pair however many ports were
    // registered, and the assertion below ratified it. The count is
    // asserted per registered port in
    // `capture_multi_returns_one_buffer_per_registered_port`.
    let mut eng = FakeEngine::new();
    eng.set_tone(1_000.0, 0.5);
    eng.reconnect_input("fake:capture_0").unwrap();
    eng.add_ref_input("fake:capture_2").unwrap();
    let bufs = eng.capture_multi(0.02).unwrap();
    assert_eq!(bufs.len(), 2);
    assert_eq!(bufs[0].len(), bufs[1].len());
    let diff: f32 = bufs[0]
        .iter()
        .zip(&bufs[1])
        .map(|(a, b)| (a - b).abs())
        .sum();
    assert!(
        diff > 0.0,
        "multi channels should differ between meas and ref"
    );
}

/// #254. The handler sizes `rings` from the session's unique capture
/// channels and fills them from `capture_multi`'s buffers positionally,
/// so a short return leaves the tail rings permanently below one Welch
/// segment — the warmup gate then skips every tick and the session never
/// publishes. One buffer per registered port, in registration order, is
/// what makes `pairs=[[0,3],[1,3]]` — a second measurement position
/// against a shared reference, which the rig has already run — testable
/// off the rig at all.
#[test]
fn capture_multi_returns_one_buffer_per_registered_port() {
    let mut eng = FakeEngine::new();
    eng.set_tone(1_000.0, 0.5);
    eng.reconnect_input("fake:capture_0").unwrap();
    eng.add_ref_input("fake:capture_3").unwrap();
    eng.add_ref_input("fake:capture_1").unwrap();

    let bufs = eng.capture_multi(0.02).unwrap();
    assert_eq!(
        bufs.len(),
        3,
        "three registered ports must produce three buffers"
    );
    for (i, b) in bufs.iter().enumerate() {
        assert_eq!(b.len(), bufs[0].len(), "buffer {i} length differs");
        assert!(b.iter().any(|s| *s != 0.0), "buffer {i} is silent");
    }

    // Positional, not incidental: buffer 2 must carry capture_1's tone
    // offset (1 100 Hz), not capture_3's (1 300 Hz). A fill that returned
    // the right *number* of buffers in the wrong order would put a
    // measurement channel's audio on a reference ring and still look
    // healthy from the frame count alone.
    // 1 100 and 1 300 Hz both land exactly on a bin at this capture length
    // (960 samples at 48 kHz, 50 Hz bins), so the bin snap costs nothing.
    let energy_at = |buf: &[f32], freq: f64| goertzel_mag(buf, 48_000.0, freq);
    assert!(
        energy_at(&bufs[2], 1_100.0) > 10.0 * energy_at(&bufs[2], 1_300.0),
        "buffer 2 must be capture_1 (1 100 Hz), got 1 100 Hz {:.6} vs 1 300 Hz {:.6}",
        energy_at(&bufs[2], 1_100.0),
        energy_at(&bufs[2], 1_300.0),
    );
    assert!(
        energy_at(&bufs[1], 1_300.0) > 10.0 * energy_at(&bufs[1], 1_100.0),
        "buffer 1 must be capture_3 (1 300 Hz), got 1 300 Hz {:.6} vs 1 100 Hz {:.6}",
        energy_at(&bufs[1], 1_300.0),
        energy_at(&bufs[1], 1_100.0),
    );
}

/// Two measurement channels against one reference must each read the
/// source at the *same* delay. A single shared meas cursor advances once
/// per channel per tick, so the second channel would drift by one
/// buffer's length every tick — a delay that is an artefact of call order
/// and would have made the fake's multi-position support useless for
/// rehearsing exactly the session shape #254 blocks.
#[test]
fn correlated_pair_tracks_each_measurement_port_separately() {
    let mut eng = FakeEngine::new();
    eng.reconnect_input("fake:capture_0").unwrap();
    eng.add_ref_input("fake:capture_3").unwrap();
    eng.add_ref_input("fake:capture_1").unwrap();
    eng.set_correlated_pair(0.5, 0);

    for tick in 0..3 {
        let bufs = eng.capture_multi(0.02).unwrap();
        assert_eq!(bufs.len(), 3);
        // delay 0 and gain 0.5: both measurement channels are the ref
        // scaled, sample for sample, on every tick. A shared meas cursor
        // drifts the second channel by one buffer per tick.
        assert_delayed_scaled(&bufs[0], &bufs[1], 0.5, 0, &format!("tick {tick} meas 0"));
        assert_delayed_scaled(&bufs[2], &bufs[1], 0.5, 0, &format!("tick {tick} meas 1"));
    }
}

#[test]
fn stereo_channels_are_independent() {
    let mut eng = FakeEngine::new();
    eng.set_tone(1_000.0, 0.5);
    eng.reconnect_input("fake:capture_0").unwrap();
    eng.add_ref_input("fake:capture_2").unwrap();
    let (meas, refch) = eng.capture_stereo(0.02).unwrap();
    // Both non-empty and distinct signals.
    assert!(!meas.is_empty());
    assert_eq!(meas.len(), refch.len());
    let diff: f32 = meas.iter().zip(&refch).map(|(a, b)| (a - b).abs()).sum();
    assert!(diff > 0.0, "meas and ref channels should differ");
}

#[test]
fn tone_pair_synthesizes_both_frequencies() {
    // #170: I3/I1 stimulus needs two simultaneous tones at distinct
    // levels — confirm both actually land in the captured signal, not
    // just the first (the old `set_tone` single-tone behaviour).
    let sr = 48_000;
    let mut eng = FakeEngine::new();
    eng.set_tone_pair(&[(1_000.0, 0.5), (5_000.0, 0.1)]);
    let s = eng.capture_block(0.5).unwrap();
    let m1 = goertzel_mag(&s, sr as f64, 1_000.0);
    let m2 = goertzel_mag(&s, sr as f64, 5_000.0);
    assert!(m1 > 0.1, "expected energy at 1000 Hz, got mag {m1}");
    assert!(m2 > 0.01, "expected energy at 5000 Hz, got mag {m2}");
    assert!(
        m1 > m2,
        "louder tone (0.5) should measure higher than quieter tone (0.1): {m1} vs {m2}"
    );
}

/// Regression for the frozen/repeated-block bug the I5 soak invariant
/// exists to catch: before the fix, `Stimulus::Noise`
/// re-seeded its LCG to the same fixed state on every `capture_block`
/// call, so a caller polling repeatedly (as `monitor_spectrum`'s LF
/// ring does) saw the identical block over and over — a ring fed only
/// identical blocks becomes periodic once fully wrapped, freezing
/// whatever spectrum falls out of that periodicity. Two consecutive
/// captures must now differ.
#[test]
fn noise_stream_advances_across_calls() {
    let mut eng = FakeEngine::new();
    eng.set_broadband_noise(0.5);
    eng.reconnect_input("fake:capture_0").unwrap();
    let a = eng.capture_block(0.01).unwrap();
    let b = eng.capture_block(0.01).unwrap();
    assert_eq!(a.len(), b.len());
    assert_ne!(
        a, b,
        "consecutive noise captures must not repeat the same block"
    );
}

/// Same starting state (fresh engine, same channel) must reproduce the
/// same first block — the soak's "same seed -> same result" acceptance
/// criterion depends on this, not just on the stream
/// advancing.
#[test]
fn noise_stream_is_deterministic_from_a_fresh_engine() {
    let mut eng1 = FakeEngine::new();
    eng1.set_broadband_noise(0.5);
    eng1.reconnect_input("fake:capture_0").unwrap();
    let first = eng1.capture_block(0.01).unwrap();

    let mut eng2 = FakeEngine::new();
    eng2.set_broadband_noise(0.5);
    eng2.reconnect_input("fake:capture_0").unwrap();
    let replay = eng2.capture_block(0.01).unwrap();

    assert_eq!(first, replay, "same seed must replay identically");
}

#[test]
fn broadband_noise_has_no_dominant_tone() {
    // #170: I2 stimulus needs genuine spectral content, not the old
    // `set_pink` fallback (which only ever synthesized a sine).
    let mut eng = FakeEngine::new();
    eng.set_broadband_noise(0.5);
    let s = eng.capture_block(0.5).unwrap();
    assert!(!s.is_empty());
    let rms: f64 = (s.iter().map(|x| (*x as f64).powi(2)).sum::<f64>() / s.len() as f64).sqrt();
    assert!(rms > 0.05, "expected broadband energy, rms = {rms}");
    // A single-bin Goertzel magnitude at any one frequency should be
    // small relative to total RMS energy — noise, not a tone.
    let m = goertzel_mag(&s, 48_000.0, 1_000.0) / s.len() as f64;
    assert!(
        m < rms,
        "energy concentrated at 1000 Hz looks tonal, not broadband: mag/n={m} rms={rms}"
    );
}

/// Ground truth (handoff: parity-completion M1.5): meas must equal
/// `gain * ref[i - delay_samples]` sample-for-sample, for every `i`
/// once past the initial `delay_samples` silence — checked directly
/// against the captured arrays, not just "differs" (the way
/// `stereo_channels_are_independent` checks the *old* stimuli).
#[test]
fn correlated_pair_meas_is_exact_delayed_scaled_copy_of_ref() {
    let mut eng = FakeEngine::new();
    let gain = 0.5_f64;
    let delay = 37_usize;
    eng.set_correlated_pair(gain, delay);
    eng.reconnect_input("fake:capture_0").unwrap();
    eng.add_ref_input("fake:capture_1").unwrap();

    let (meas, refch) = eng.capture_stereo(0.01).unwrap();
    assert_eq!(meas.len(), refch.len());
    assert!(
        meas.len() > delay,
        "test capture too short to exercise the delay"
    );

    assert_delayed_scaled(&meas, &refch, gain, delay, "single capture");
}

/// Same check across a call boundary (two consecutive `capture_stereo`
/// calls) — the per-role position counters must keep the delay
/// relationship correct across ticks, not just within one block.
#[test]
fn correlated_pair_delay_relationship_holds_across_call_boundary() {
    let mut eng = FakeEngine::new();
    let gain = 0.7_f64;
    let delay = 5_usize;
    eng.set_correlated_pair(gain, delay);
    eng.reconnect_input("fake:capture_0").unwrap();
    eng.add_ref_input("fake:capture_1").unwrap();

    let (mut meas_all, mut ref_all) = (Vec::new(), Vec::new());
    for _ in 0..5 {
        let (meas, refch) = eng.capture_stereo(0.001).unwrap();
        meas_all.extend(meas);
        ref_all.extend(refch);
    }
    assert!(meas_all.len() > delay * 2);
    assert_delayed_scaled(&meas_all, &ref_all, gain, delay, "across five captures");
}

/// Broadband, not a hidden tone — the ground-truth H1/coherence test
/// (`it_snapshot.rs`) needs genuine spectral content, same reasoning
/// as `broadband_noise_has_no_dominant_tone`.
#[test]
fn correlated_pair_ref_is_broadband_not_tonal() {
    let mut eng = FakeEngine::new();
    eng.set_correlated_pair(1.0, 0);
    eng.reconnect_input("fake:capture_0").unwrap();
    eng.add_ref_input("fake:capture_1").unwrap();
    let (_, refch) = eng.capture_stereo(0.5).unwrap();
    let rms: f64 =
        (refch.iter().map(|x| (*x as f64).powi(2)).sum::<f64>() / refch.len() as f64).sqrt();
    assert!(rms > 0.05, "expected broadband energy, rms = {rms}");
    let m = goertzel_mag(&refch, 48_000.0, 1_000.0) / refch.len() as f64;
    assert!(
        m < rms,
        "energy concentrated at 1000 Hz, not broadband: mag/n={m} rms={rms}"
    );
}

/// Determinism (needed for reproducible fixture regeneration): same
/// seed (fixed in code) + same params ⇒ identical stream from a
/// fresh engine, same acceptance criterion as `Stimulus::Noise`'s own
/// `noise_stream_is_deterministic_from_a_fresh_engine`.
#[test]
fn correlated_pair_is_deterministic_from_a_fresh_engine() {
    let build = || {
        let mut eng = FakeEngine::new();
        eng.set_correlated_pair(0.5, 10);
        eng.reconnect_input("fake:capture_0").unwrap();
        eng.add_ref_input("fake:capture_1").unwrap();
        eng.capture_stereo(0.01).unwrap()
    };
    let (meas1, ref1) = build();
    let (meas2, ref2) = build();
    assert_eq!(meas1, meas2, "meas stream must replay identically");
    assert_eq!(ref1, ref2, "ref stream must replay identically");
}
