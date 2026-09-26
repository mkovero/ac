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
    //
    // #204 changed this test deliberately a second time: it drives the
    // generator, and a generator reaches the capture only while an output
    // port is open, so it now starts with one. Without the `start` both
    // buffers would be zeros and the "differ" assertion below would fail —
    // which is the routing gate working, not a regression.
    let mut eng = FakeEngine::new();
    eng.start(&["fake:playback_0".into()], Some("fake:capture_0"))
        .unwrap();
    eng.set_tone(1_000.0, 0.5);
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
    eng.set_external_tones(&[(1_000.0, 0.5)]);
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
    eng.set_external_tones(&[(1_000.0, 0.5)]);
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
    eng.set_external_tones(&[(1_000.0, 0.5), (5_000.0, 0.1)]);
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
    eng.set_external_noise(0.5);
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
    eng1.set_external_noise(0.5);
    eng1.reconnect_input("fake:capture_0").unwrap();
    let first = eng1.capture_block(0.01).unwrap();

    let mut eng2 = FakeEngine::new();
    eng2.set_external_noise(0.5);
    eng2.reconnect_input("fake:capture_0").unwrap();
    let replay = eng2.capture_block(0.01).unwrap();

    assert_eq!(first, replay, "same seed must replay identically");
}

#[test]
fn broadband_noise_has_no_dominant_tone() {
    // #170: I2 stimulus needs genuine spectral content, not the old
    // `set_pink` fallback (which only ever synthesized a sine).
    let mut eng = FakeEngine::new();
    eng.set_external_noise(0.5);
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

/// RMS of a buffer; zero for an all-zero buffer.
fn rms(s: &[f32]) -> f64 {
    (s.iter().map(|x| (*x as f64).powi(2)).sum::<f64>() / s.len().max(1) as f64).sqrt()
}

/// #204 (a): a generator driven with no output port open reaches nothing,
/// so every captured buffer is exact zeros — the #203 class, a drive into
/// nothing. The same drive with one output open is heard. Both halves in one
/// test, so the zeros cannot come from something other than routing.
#[test]
fn unrouted_generator_captures_zeros_and_routed_generator_is_heard() {
    let mut unrouted = FakeEngine::new();
    unrouted.start(&[], Some("fake:capture_0")).unwrap();
    unrouted.add_ref_input("fake:capture_1").unwrap();
    unrouted.add_ref_input("fake:capture_2").unwrap();
    unrouted.set_pink(0.5);
    let bufs = unrouted.capture_multi(0.01).unwrap();
    assert_eq!(bufs.len(), 3);
    for (i, b) in bufs.iter().enumerate() {
        assert!(!b.is_empty(), "buffer {i} is empty");
        assert!(
            b.iter().all(|&v| v == 0.0),
            "unrouted drive must capture zeros on buffer {i}"
        );
    }
    let block = unrouted.capture_block(0.01).unwrap();
    assert!(block.iter().all(|&v| v == 0.0), "capture_block too");

    let mut routed = FakeEngine::new();
    routed
        .start(&["fake:playback_0".into()], Some("fake:capture_0"))
        .unwrap();
    routed.add_ref_input("fake:capture_1").unwrap();
    routed.add_ref_input("fake:capture_2").unwrap();
    routed.set_pink(0.5);
    for (i, b) in routed.capture_multi(0.01).unwrap().iter().enumerate() {
        assert!(rms(b) > 0.05, "routed drive must be heard on buffer {i}");
    }
}

/// #204 (b): disconnecting the last output port unroutes the engine; the
/// next capture is zeros.
#[test]
fn disconnecting_the_last_output_silences_the_generator() {
    let mut eng = FakeEngine::new();
    eng.start(&["fake:playback_0".into()], Some("fake:capture_0"))
        .unwrap();
    eng.connect_output("fake:playback_1").unwrap();
    eng.set_tone(1_000.0, 0.5);
    assert!(rms(&eng.capture_block(0.01).unwrap()) > 0.1);

    eng.disconnect_output("fake:playback_0");
    assert!(
        rms(&eng.capture_block(0.01).unwrap()) > 0.1,
        "one output still open: still routed"
    );
    eng.disconnect_output("fake:playback_1");
    let after = eng.capture_block(0.01).unwrap();
    assert!(
        after.iter().all(|&v| v == 0.0),
        "no output left: the drive reaches nothing"
    );
}

/// #204 (c): an external source is a signal at the input, not a drive, so
/// it survives an engine with no output port open — the monitor and passive
/// `transfer_stream` case.
#[test]
fn external_tone_survives_an_unrouted_engine() {
    let mut eng = FakeEngine::new();
    eng.start(&[], Some("fake:capture_0")).unwrap();
    eng.set_external_tones(&[(1_000.0, 0.5)]);
    let s = eng.capture_block(0.02).unwrap();
    let m = goertzel_mag(&s, 48_000.0, 1_000.0);
    assert!(m > 0.2, "external 1 kHz must be captured, mag {m}");
}

/// External plus a routed drive add; unrouted, only the external is heard,
/// byte for byte. A drive leaking into an unrouted capture alongside an
/// external source would pass (a) and fail here.
#[test]
fn external_and_routed_drive_add_and_unrouted_drive_does_not() {
    let capture = |outputs: &[String], drive: bool| {
        let mut eng = FakeEngine::new();
        eng.start(outputs, Some("fake:capture_0")).unwrap();
        eng.set_external_tones(&[(1_000.0, 0.5)]);
        if drive {
            eng.set_tone(3_000.0, 0.25);
        }
        eng.capture_block(0.02).unwrap()
    };

    let external_only = capture(&[], false);
    let unrouted = capture(&[], true);
    assert_eq!(unrouted, external_only, "unrouted drive must add nothing");

    let routed = capture(&["fake:playback_0".into()], true);
    let ext = goertzel_mag(&routed, 48_000.0, 1_000.0);
    let drv = goertzel_mag(&routed, 48_000.0, 3_000.0);
    assert!(ext > 0.2, "external must still be heard, mag {ext}");
    assert!(drv > 0.1, "routed drive must be heard too, mag {drv}");
}

/// External noise and driven noise on one channel must be independent
/// streams. Sharing a seed would make them one sequence, so the two sources
/// together would read as one correlated stream at +6 dB.
#[test]
fn external_noise_is_independent_of_driven_noise() {
    let mut driven = FakeEngine::new();
    driven
        .start(&["fake:playback_0".into()], Some("fake:capture_0"))
        .unwrap();
    driven.set_pink(0.5);
    let d = driven.capture_block(0.01).unwrap();

    let mut external = FakeEngine::new();
    external.start(&[], Some("fake:capture_0")).unwrap();
    external.set_external_noise(0.5);
    let e = external.capture_block(0.01).unwrap();

    assert_eq!(d.len(), e.len());
    assert_ne!(d, e, "external and driven noise share one stream");
}

/// #204 (d): ring mode reads one port per ref ring. Ring 2 must carry
/// `fake:capture_1`'s channel offset (1 100 Hz), not the first ref's
/// (`fake:capture_3`, 1 300 Hz) repeated — which is what ring mode did
/// before, and which made it unable to rehearse a multi-channel session.
#[test]
fn ring_mode_reads_one_port_per_ref_ring() {
    let mut eng = FakeEngine::new();
    eng.set_external_tones(&[(1_000.0, 0.5)]);
    eng.reconnect_input("fake:capture_0").unwrap();
    eng.add_ref_input("fake:capture_3").unwrap();
    eng.add_ref_input("fake:capture_1").unwrap();
    eng.enable_ring_mode(0.0, 2, 1);

    let bufs = eng.capture_multi(0.02).unwrap();
    assert_eq!(bufs.len(), 3);
    // 960 samples at 48 kHz: 50 Hz bins, so 1 000, 1 100 and 1 300 Hz all
    // sit on a bin.
    let energy_at = |buf: &[f32], freq: f64| goertzel_mag(buf, 48_000.0, freq);
    for (i, want, other) in [
        (0, 1_000.0, 1_300.0),
        (1, 1_300.0, 1_100.0),
        (2, 1_100.0, 1_300.0),
    ] {
        assert!(
            energy_at(&bufs[i], want) > 10.0 * energy_at(&bufs[i], other),
            "ring {i} must carry {want} Hz, got {want} Hz {:.6} vs {other} Hz {:.6}",
            energy_at(&bufs[i], want),
            energy_at(&bufs[i], other),
        );
    }
}

/// #204 (e): a ring count that disagrees with the registered ref ports is
/// refused, naming both counts, rather than served by substituting a port.
#[test]
fn ring_mode_refuses_a_ref_count_mismatch() {
    let mut eng = FakeEngine::new();
    eng.set_external_tones(&[(1_000.0, 0.5)]);
    eng.reconnect_input("fake:capture_0").unwrap();
    eng.add_ref_input("fake:capture_1").unwrap();
    eng.enable_ring_mode(0.0, 2, 1);

    let err = eng
        .capture_multi(0.01)
        .expect_err("two ref rings, one ref port: must refuse")
        .to_string();
    assert!(
        err.contains("2 ref ring(s)") && err.contains("1 ref port(s)"),
        "error must name both counts: {err}"
    );
}

/// #204 (f): `play_and_capture` and `play_and_capture_with_reference` gate
/// the played burst on routing like every other synthesis path. Unrouted,
/// neither leg contains the burst — with the noise override unset, both are
/// exact zeros. With one output open the burst is present in each leg at
/// that leg's hook delay. This test sets no `AC_FAKE_*` variable; it reads
/// the hooks' values instead, so it holds whatever the process environment
/// carries, and it relies only on the meas-leg delay list being unset
/// (default 32 samples).
#[test]
fn play_and_capture_hears_the_burst_only_when_routed() {
    use std::sync::atomic::AtomicBool;
    const MEAS_DEFAULT_DELAY: usize = 32;
    assert!(
        hooks::tau_delay_override_list().is_empty(),
        "meas-leg delay override set; this test pins the default delay"
    );
    let burst: Vec<f32> = (0..64).map(|i| 0.25 + i as f32 * 0.01).collect();
    let noise_amp = tau_noise_amplitude_override();
    let stop = AtomicBool::new(false);
    // Nothing but the noise override's dither may appear: no sample may
    // exceed its amplitude, and with it unset every sample is exact zero.
    let assert_no_burst = |leg: &[f32], what: &str| {
        assert!(
            leg.iter().all(|&v| v.abs() <= noise_amp),
            "{what}: unrouted leg must not carry the burst"
        );
        if noise_amp == 0.0 {
            assert!(leg.iter().all(|&v| v == 0.0), "{what}: must be zeros");
        }
    };
    let assert_burst_at = |leg: &[f32], delay: usize, gain: f32, what: &str| {
        for (i, &s) in burst.iter().enumerate() {
            let got = leg[delay + i];
            let want = s * gain;
            assert!(
                (got - want).abs() <= noise_amp + 1e-6,
                "{what}: sample {} = {got}, burst wants {want}",
                delay + i
            );
        }
    };

    let mut unrouted = FakeEngine::new();
    unrouted.start(&[], Some("fake:capture_0")).unwrap();
    assert_no_burst(
        &unrouted.play_and_capture(&burst, 0.005).unwrap(),
        "play_and_capture",
    );
    let (meas, reference) = unrouted
        .play_and_capture_with_reference(&burst, 0.005, "fake:capture_1", &stop)
        .unwrap();
    assert_no_burst(&meas, "with_reference meas");
    assert_no_burst(&reference, "with_reference ref");

    let mut routed = FakeEngine::new();
    routed
        .start(&["fake:playback_0".into()], Some("fake:capture_0"))
        .unwrap();
    let out = routed.play_and_capture(&burst, 0.005).unwrap();
    assert_burst_at(
        &out,
        MEAS_DEFAULT_DELAY,
        tau_gain_override(),
        "play_and_capture",
    );
    let (meas, reference) = routed
        .play_and_capture_with_reference(&burst, 0.005, "fake:capture_1", &stop)
        .unwrap();
    assert_burst_at(
        &meas,
        MEAS_DEFAULT_DELAY,
        tau_gain_override(),
        "with_reference meas",
    );
    assert_burst_at(
        &reference,
        ref_delay_samples(),
        ref_gain(),
        "with_reference ref",
    );
}

/// #204 (g): ring mode builds its own `Synth`, so it needs its own copy of
/// (a) and (c). An unrouted generator drains as zeros on every ring; an
/// external tone in the same unrouted ring engine is heard.
#[test]
fn unrouted_ring_mode_drains_zeros_for_the_generator_and_hears_external() {
    let mut eng = FakeEngine::new();
    eng.start(&[], Some("fake:capture_0")).unwrap();
    eng.add_ref_input("fake:capture_1").unwrap();
    eng.set_pink(0.5);
    eng.enable_ring_mode(0.0, 1, 1);

    let bufs = eng.capture_multi(0.02).unwrap();
    assert_eq!(bufs.len(), 2);
    for (i, b) in bufs.iter().enumerate() {
        assert!(!b.is_empty(), "ring {i} drained nothing");
        assert!(
            b.iter().all(|&v| v == 0.0),
            "unrouted drive must drain zeros on ring {i}"
        );
    }

    eng.set_external_tones(&[(1_000.0, 0.5)]);
    let bufs = eng.capture_multi(0.02).unwrap();
    for (i, want) in [(0, 1_000.0), (1, 1_100.0)] {
        let m = goertzel_mag(&bufs[i], 48_000.0, want);
        assert!(
            m > 0.2,
            "ring {i} must hear the external {want} Hz, mag {m}"
        );
    }
}

/// #204 (h): the external source and the generator keep **separate** noise
/// state maps, not one map with different seeds. The combined capture minus
/// the external-alone capture must be the generator-alone stream, sample for
/// sample, across several consecutive blocks. A shared map would interleave
/// the two streams' advances and leave O(amplitude) residuals — which
/// `external_noise_is_independent_of_driven_noise` (different bytes only)
/// would not catch. Bar: one f32 add of values below 1.0 rounds by < 6e-8.
#[test]
fn external_and_generator_noise_keep_separate_state() {
    let routed = || {
        let mut eng = FakeEngine::new();
        eng.start(&["fake:playback_0".into()], Some("fake:capture_0"))
            .unwrap();
        eng
    };
    let mut both = routed();
    both.set_external_noise(0.3);
    both.set_pink(0.2);
    let mut ext_only = routed();
    ext_only.set_external_noise(0.3);
    let mut gen_only = routed();
    gen_only.set_pink(0.2);

    for block in 0..3 {
        let b = both.capture_block(0.01).unwrap();
        let e = ext_only.capture_block(0.01).unwrap();
        let g = gen_only.capture_block(0.01).unwrap();
        assert_eq!(b.len(), g.len());
        assert!(rms(&g) > 0.05 && rms(&e) > 0.05, "block {block}: silent");
        for i in 0..b.len() {
            let residual = (b[i] - e[i]) - g[i];
            assert!(
                residual.abs() <= 1e-6,
                "block {block} sample {i}: combined − external = {}, generator = {}",
                b[i] - e[i],
                g[i]
            );
        }
    }
}

/// Cross-correlation `Σ meas[i + k] · refch[i]` for every lag `k` in
/// `0..=max_lag`.
fn xcorr(meas: &[f32], refch: &[f32], max_lag: usize) -> Vec<f64> {
    (0..=max_lag)
        .map(|k| {
            (0..meas.len().saturating_sub(k))
                .map(|i| meas[i + k] as f64 * refch[i] as f64)
                .sum()
        })
        .collect()
}

/// #204 rev. 2, pin 2: a routed drive *adds* to an external correlated
/// pair, and `it_set_delay.rs` locks through that sum. It can because the
/// generator's noise is seeded per channel, so it is uncorrelated between
/// meas and ref and the pair's lag still wins. The rejected shape — the
/// generator block duplicated identically onto both channels, as a real
/// loopback would carry it — is computed here too: its lag-0 term is the
/// one that grows, which is what would let a re-find lock at 0. This records
/// the coupling between the generator's per-channel seeding and a re-find's
/// lag assertions.
#[test]
fn correlated_pair_lag_survives_a_routed_drive() {
    const DELAY: usize = 400;
    const MAX_LAG: usize = 800;
    let drive = ac_core::shared::generator::dbfs_to_amplitude(-20.0);
    let engine = |pair: bool, pink: bool| {
        let mut eng = FakeEngine::new();
        eng.start(&["fake:playback_0".into()], Some("fake:capture_0"))
            .unwrap();
        eng.add_ref_input("fake:capture_1").unwrap();
        if pair {
            eng.set_correlated_pair(0.6, DELAY);
        }
        if pink {
            eng.set_pink(drive);
        }
        eng
    };
    // 1 s = 48 000 samples, far above 4 × DELAY.
    let (meas, refch) = engine(true, true).capture_stereo(1.0).unwrap();
    let (pair_meas, pair_ref) = engine(true, false).capture_stereo(1.0).unwrap();
    let (gen_meas, gen_ref) = engine(false, true).capture_stereo(1.0).unwrap();
    assert!(meas.len() >= 4 * DELAY);

    // The generator is present in the correlated-pair capture, on each
    // channel, as that channel's own stream: combined − pair-alone must be
    // generator-alone, sample for sample. This is what refuses a stimulus
    // rule that drops the generator when `external` is `CorrelatedPair`
    // (the rejected rev. 2 shape: `meas == pair_meas` would pass every
    // lag assertion below). Bar as in
    // `external_and_generator_noise_keep_separate_state`.
    assert!(
        rms(&gen_meas) > 0.01 && rms(&gen_ref) > 0.01,
        "drive silent"
    );
    assert_ne!(gen_meas, gen_ref, "generator legs must be per-channel");
    for (leg, actual, pair, gen) in [
        ("meas", &meas, &pair_meas, &gen_meas),
        ("ref", &refch, &pair_ref, &gen_ref),
    ] {
        assert_eq!(actual.len(), gen.len());
        for i in 0..actual.len() {
            let residual = (actual[i] - pair[i]) - gen[i];
            assert!(
                residual.abs() <= 1e-6,
                "{leg} sample {i}: combined − pair = {}, generator = {}",
                actual[i] - pair[i],
                gen[i]
            );
        }
    }

    let actual = xcorr(&meas, &refch, MAX_LAG);
    let argmax = (0..=MAX_LAG)
        .max_by(|&a, &b| actual[a].total_cmp(&actual[b]))
        .unwrap();
    assert_eq!(argmax, DELAY, "routed drive moved the pair's lag");
    assert!(actual[DELAY] > actual[0]);

    // The rejected shape: one generator block on both channels.
    let dup_meas: Vec<f32> = pair_meas
        .iter()
        .zip(&gen_meas)
        .map(|(p, g)| p + g)
        .collect();
    let dup_ref: Vec<f32> = pair_ref.iter().zip(&gen_meas).map(|(p, g)| p + g).collect();
    let dup = xcorr(&dup_meas, &dup_ref, MAX_LAG);
    let pair = xcorr(&pair_meas, &pair_ref, MAX_LAG);
    let gen_energy: f64 = gen_meas.iter().map(|&g| (g as f64).powi(2)).sum();

    let dup_growth_0 = dup[0] - pair[0];
    let actual_growth_0 = actual[0] - pair[0];
    assert!(
        dup_growth_0 > 0.5 * gen_energy,
        "duplicated drive must add its energy at lag 0: growth {dup_growth_0}, energy {gen_energy}"
    );
    assert!(
        actual_growth_0.abs() < 0.25 * gen_energy,
        "per-channel drive must not add a lag-0 term: growth {actual_growth_0}, energy {gen_energy}"
    );
    assert!(
        (dup[DELAY] - pair[DELAY]).abs() < 0.25 * gen_energy,
        "lag 0 is the term that grows, not lag {DELAY}"
    );
}
