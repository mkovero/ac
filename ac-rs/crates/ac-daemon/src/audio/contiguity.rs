//! Capture-contiguity evidence (`handoff-capture-contiguity.md`, D3).
//!
//! # Scope correction (2026-07-25, from the reporter)
//!
//! **What this module characterises is NOT the originally reported bug.** The
//! handoff describes the symptom as "the displayed spectrum shows the response
//! three times" and its D4 asks for three *frequencies* to be classified as
//! geometric or linear-symmetric — a frequency-axis reading. The reporter has
//! since clarified that the symptom is **temporal**: the response recurs every
//! roughly 3–5 seconds, each recurrence identical, decaying as the stimulus
//! ends and then repeating again and again *with no stimulus present*.
//!
//! A splice removes time; it cannot repeat it. So the defect characterised
//! here — real, and confirmed on hardware — is a **separate** one that this
//! investigation found along the way. The reported bug is a recurrence of
//! stale audio, which points at H3 (ring backlog, `RING_CAPACITY`, and the
//! never-clearing `capture_available` path) rather than at H1, and plausibly
//! at the same root cause as the separately-filed "LF ~10 s anomaly".
//!
//! Nothing below is invalidated by that — the splice, its hardware
//! confirmation and the period-quantisation selection rule all stand on their
//! own measurements. But do not read this module as an account of the
//! three-copies report.
//!
//! Test-only module. It exists to answer one falsifiable question: **does the
//! `clear()`-before-wait ordering in `capture_multi` put spectral replicas
//! into the estimator's output?**
//!
//! The experiment is a controlled A/B over exactly one variable. Both arms
//! use the same ring-backed fake backend, the same synthetic clock, the same
//! HF tone, and the same window assembly the transfer worker performs
//! (`handlers/transfer.rs`: `extend_from_slice` into a `target_total`-capped
//! ring, then `h1_estimate_with_delay`). They differ only in which drain they
//! call — `capture_multi`, which clears, versus `capture_available`, which
//! does not. That is precisely hypothesis H1's discriminator, and it is the
//! headless equivalent of the `ac monitor 0` / `ac monitor 0-1` hardware A/B
//! that D4 will run.
//!
//! **Status: the defect is fixed (#207).** `transfer_stream` now calls
//! `capture_multi_contiguous`, which does not clear before waiting. These
//! tests were written as guards that *asserted the defect* while it was
//! present, and have been inverted per house convention now that it is not:
//!
//! - `fixed_streaming_drain_of_a_single_tone_produces_one_peak` is the
//!   inverted form and asserts the correct behaviour.
//! - The `Drain::Clearing` tests are **kept, still asserting replication**,
//!   because `capture_multi` is not going away — it remains the correct call
//!   for a one-shot measurement. They are now the demonstration of *why* it
//!   must never be used for streaming, and they will catch anyone re-pointing
//!   a streaming consumer at it.
//!
//! Do not "fix" a failure here by relaxing an assertion; a failure means
//! either the defect changed or the reproducer stopped reproducing, and both
//! are findings.
//!
//! Measured spacing is recorded in `spliced_replica_spacing_matches_tick_rate`
//! (acceptance criterion 5), which reconciles it against the `sr/L` the splice
//! hypothesis predicts rather than taking the number on trust.
//!
//! # Mutation verification at birth (acceptance criterion 3)
//!
//! A guard test that cannot tell spliced input from contiguous input is not
//! evidence. Both runs, on this commit:
//!
//! | run | `spliced_*_replicates_the_response` | `*_spacing_*` | `discard_count_*` | controls |
//! |---|---|---|---|---|
//! | current `main` (pre-wait `clear()` present) | pass — 101 peaks | pass — 20 Hz | pass — 480/tick | pass |
//! | mutant (pre-wait `clear()` removed from `CaptureRings::capture_block` and `capture_multi`) | **fail — 1 peak, at 15 100 Hz** | **fail — only one peak to measure** | **fail — 0 discarded** | pass |
//!
//! The mutant is what the eventual fix looks like, and under it the stimulus
//! recovers to exactly one peak at the stimulus frequency. That is the
//! discrimination these tests are required to demonstrate.

use crate::audio::fake::FakeEngine;
use crate::audio::AudioEngine;

/// Mirrors the transfer worker's Welch settings (`handlers/transfer.rs`):
/// `nperseg = sr` for 1 Hz bins, 50% overlap, 4 averages.
const N_AVERAGES: usize = 4;

/// Matches the worker's `chunk_secs` — 50 ms, a 20 Hz tick rate.
const CHUNK_SECS: f64 = 0.05;

/// HF because the defect is HF-specific: a splice randomises phase in
/// proportion to frequency, so a 10 ms gap is ~1 cycle at 100 Hz but ~150
/// fully-decorrelated cycles at 15 kHz.
///
/// **Why not a round 15 000 Hz.** What a splice actually does is impose a
/// phase step of `2π · frac(f · gap / sr)` on each retained fragment. At
/// `f = 15 000`, `sr = 48 000` and a 5 ms gap (240 samples), `f · gap / sr =
/// 75.0` exactly — the gap is a whole number of tone periods, the phase step
/// is zero, and the spliced window is *bit-identical to a contiguous one*.
/// The first run of this test used 15 000 Hz and reported a single clean
/// peak, which reads as "no defect" and is entirely an artifact of the
/// stimulus choice. This is the same cycle-alignment trap the handoff already
/// flagged for `generate_sine_1s`.
///
/// 15 100 Hz gives `frac(15 100 · 240 / 48 000) = 0.5` — the worst case, a
/// half-cycle step per fragment. Chosen deliberately: a reproducer should sit
/// at the maximum of the effect it is trying to observe, not at a random
/// point that might be a null.
///
/// Practical consequence beyond this test: on real hardware the visibility of
/// this defect depends on the arithmetic relationship between the stimulus
/// frequency and the tick gap, so a sweep can show it and a round-number tone
/// can hide it completely.
const TONE_HZ: f64 = 15_100.0;

/// Peak separation for the replica counts, in 1 Hz bins. Replicas sit
/// `1/CHUNK_SECS` = 20 Hz apart, so this must stay well under that or the
/// cluster is merged into a single reported peak and the defect disappears
/// into the measurement.
const MIN_PEAK_SEP_BINS: usize = 3;

/// A producer granularity of one sample — i.e. *no* period quantisation.
///
/// Used by the tests below that isolate the processing gap as the single
/// variable. It is deliberately **not** what real hardware does: a JACK
/// producer moves whole periods, and that quantisation turns out to dominate
/// which frequencies expose the defect at all. See
/// `period_quantisation_decides_which_frequencies_expose_the_splice`, which
/// covers the realistic case, and `FakeRings::period`.
const SAMPLE_ACCURATE: usize = 1;

/// Whether a tick's capture goes through the clearing drain or the
/// contiguous one. The single independent variable of the experiment.
#[derive(Clone, Copy, PartialEq)]
enum Drain {
    /// `capture_multi` — clears the ring before waiting. This is the
    /// one-shot-measurement drain; issue #207 was `transfer_stream` calling it
    /// in a streaming loop. Kept here so the defect stays *demonstrable*: it
    /// is still the correct call for a one-shot capture, and these tests are
    /// what show why it must not be used for streaming.
    Clearing,
    /// `capture_available` — never clears. The control.
    Contiguous,
    /// `capture_multi_contiguous` — the #207 fix: no pre-wait clear, drains
    /// everything available. What `transfer_stream` uses now.
    Fixed,
}

/// Run a ring-backed fake session and return the measurement-channel window
/// the estimator would be handed, plus the engine's discard count.
///
/// Deliberately reproduces the worker's assembly rather than calling it: the
/// worker builds its window from `capture_multi` output with
/// `extend_from_slice` and a front `drain` to `target_total`, and that
/// assembly is the step that presents spliced fragments as continuous time.
fn assemble_window(drain: Drain, process_secs: f64, sr: u32) -> (Vec<f32>, u64) {
    assemble_window_with_period(drain, process_secs, sr, SAMPLE_ACCURATE, TONE_HZ)
}

/// As `assemble_window`, but with an explicit producer granularity and tone.
fn assemble_window_with_period(
    drain: Drain,
    process_secs: f64,
    sr: u32,
    period: usize,
    tone_hz: f64,
) -> (Vec<f32>, u64) {
    let nperseg = sr as usize;
    let step = nperseg / 2;
    let target_total = nperseg + step * (N_AVERAGES - 1);
    let chunk_samples = (sr as f64 * CHUNK_SECS) as usize;

    let mut eng = FakeEngine::new();
    // Must precede everything else: the engine synthesises at this rate, and
    // the analysis below assumes it.
    eng.set_sample_rate(sr);
    eng.set_tone(tone_hz, 0.5);
    eng.reconnect_input("fake:capture_0").unwrap();
    eng.add_ref_input("fake:capture_0").unwrap();
    eng.enable_ring_mode(process_secs, 1, period);

    let mut ring: Vec<f32> = Vec::with_capacity(target_total + step);
    // Enough ticks to fill the window several times over, so the result is
    // steady-state rather than dominated by the initial fill.
    let ticks = (target_total / chunk_samples) * 3;
    for _ in 0..ticks {
        let block = match drain {
            Drain::Clearing => eng.capture_multi(CHUNK_SECS).unwrap().remove(0),
            Drain::Contiguous => eng.capture_available(chunk_samples).unwrap(),
            Drain::Fixed => eng.capture_multi_contiguous(CHUNK_SECS).unwrap().remove(0),
        };
        ring.extend_from_slice(&block);
        if ring.len() > target_total {
            let excess = ring.len() - target_total;
            ring.drain(..excess);
        }
    }
    (ring, eng.discarded_samples())
}

/// Magnitude spectrum of `window` in dB, via the same `meas_amp` the wire
/// frame's spectrum column mapping is built from — so this counts what the
/// display would show, not a private FFT.
fn meas_amp_db(window: &[f32], sr: u32) -> Vec<f64> {
    let result = ac_core::visualize::transfer::h1_estimate_with_delay(window, window, sr, 0);
    let peak = result
        .meas_amp
        .iter()
        .cloned()
        .fold(f64::NEG_INFINITY, f64::max);
    result
        .meas_amp
        .iter()
        .map(|&a| 20.0 * (a / peak).log10())
        .collect()
}

/// Where the `k`-th harmonic of `tone_hz` lands in the analysed base band,
/// after any folding around Nyquist.
///
/// `FakeEngine`'s tone stimulus deliberately carries a 1% (−40 dB) second
/// harmonic, so it models a DUT rather than a mathematically pure sine. That
/// harmonic is a property of the *stimulus*, present identically in every arm,
/// and it sits exactly on the −40 dB floor these tests count against — so it
/// must be excluded rather than counted as a capture artifact.
///
/// Whether it needs folding depends on the rate, which is why this cannot be
/// hardcoded: at 48 kHz the 30 200 Hz second harmonic of a 15 100 Hz tone is
/// above Nyquist and folds back to 17 800 Hz, while at 96 kHz it is simply
/// in-band at 30 200 Hz. An earlier version of these tests only handled the
/// folded case and consequently failed at 96 kHz on a peak that was never a
/// defect.
fn harmonic_in_band(tone_hz: f64, k: u32, sr: u32) -> f64 {
    let sr = sr as f64;
    let mut f = (tone_hz * k as f64) % sr;
    if f > sr / 2.0 {
        f = sr - f;
    }
    f
}

/// Drop peaks belonging to the stimulus's own harmonic series.
///
/// Splice replicas sit a few tens of Hz from the fundamental (the tick rate),
/// never at a harmonic, so this cannot hide the effect under test —
/// `alias_is_present_in_the_contiguous_arm` pins that down.
fn without_alias(peaks: Vec<usize>, tone_hz: f64, sr: u32) -> Vec<usize> {
    let harmonics: Vec<f64> = (2..=8).map(|k| harmonic_in_band(tone_hz, k, sr)).collect();
    peaks
        .into_iter()
        .filter(|&p| {
            !harmonics
                .iter()
                .any(|&h| (p as f64 - h).abs() <= 5.0 && (p as f64 - tone_hz).abs() > 5.0)
        })
        .collect()
}

/// Indices of local maxima at least `floor_db` below the peak, separated by
/// at least `min_sep` bins so one broad lobe counts once.
fn peak_bins(db: &[f64], floor_db: f64, min_sep: usize) -> Vec<usize> {
    let mut peaks: Vec<usize> = Vec::new();
    for i in 1..db.len().saturating_sub(1) {
        if db[i] >= floor_db && db[i] >= db[i - 1] && db[i] > db[i + 1] {
            if let Some(&last) = peaks.last() {
                if i - last < min_sep {
                    if db[i] > db[last] {
                        *peaks.last_mut().unwrap() = i;
                    }
                    continue;
                }
            }
            peaks.push(i);
        }
    }
    peaks
}

/// **The control arm must be clean.** A single HF tone through the
/// non-clearing drain produces exactly one peak. If this ever fails, the
/// reproducer is broken — the fake's generator or the ring is manufacturing
/// artifacts on its own, and nothing the clearing arm reports means anything.
#[test]
fn contiguous_capture_of_a_single_tone_produces_one_peak() {
    let sr = 48_000;
    let (window, discarded) = assemble_window(Drain::Contiguous, 0.005, sr);

    assert_eq!(
        discarded, 0,
        "the contiguous drain must never clear the ring"
    );

    let db = meas_amp_db(&window, sr);
    let peaks = without_alias(peak_bins(&db, -40.0, MIN_PEAK_SEP_BINS), TONE_HZ, sr);
    assert_eq!(
        peaks.len(),
        1,
        "contiguous capture of one tone must give one peak, got {} at {:?} Hz",
        peaks.len(),
        peaks
    );
    assert!(
        (peaks[0] as f64 - TONE_HZ).abs() < 5.0,
        "the one peak must be at the stimulus frequency, got {} Hz",
        peaks[0]
    );
}

/// **Guard test — asserts the defect.** The clearing drain discards
/// everything that accrued while the previous tick was being processed, so
/// the assembled window is a concatenation of non-contiguous fragments. This
/// asserts that the resulting spectrum is *not* clean, which is the bug.
///
/// Inverts to `peaks.len() == 1` as part of the fix.
///
/// Mutation-verified at birth: with `process_secs = 0` (no accrual between
/// ticks, so `clear()` finds an empty ring and discards nothing) this same
/// setup produces one peak — see
/// `zero_gap_clearing_capture_is_clean_which_isolates_the_gap_as_the_cause`.
/// A test that could not tell spliced from contiguous input would not be
/// evidence.
#[test]
fn spliced_capture_of_a_single_tone_replicates_the_response() {
    let sr = 48_000;
    let (window, discarded) = assemble_window(Drain::Clearing, 0.005, sr);

    assert!(
        discarded > 0,
        "the clearing drain must discard the samples that accrued during processing"
    );

    let db = meas_amp_db(&window, sr);
    let peaks = without_alias(peak_bins(&db, -40.0, MIN_PEAK_SEP_BINS), TONE_HZ, sr);
    assert!(
        peaks.len() > 1,
        "spliced capture must replicate the response; got {} peak(s) at {:?} Hz. \
         If this now reports 1, the capture layer was fixed — invert this assertion.",
        peaks.len(),
        peaks
    );
}

/// Isolates the gap as the cause rather than the drain call itself: same
/// clearing drain, but no processing time charged, so nothing accrues to be
/// discarded. Clean spectrum. This is the mutation control — it is what makes
/// the guard test above evidence rather than an observation.
#[test]
fn zero_gap_clearing_capture_is_clean_which_isolates_the_gap_as_the_cause() {
    let sr = 48_000;
    let (window, discarded) = assemble_window(Drain::Clearing, 0.0, sr);

    assert_eq!(
        discarded, 0,
        "with no processing time charged there is nothing in the ring to discard"
    );

    let db = meas_amp_db(&window, sr);
    let peaks = without_alias(peak_bins(&db, -40.0, MIN_PEAK_SEP_BINS), TONE_HZ, sr);
    assert_eq!(
        peaks.len(),
        1,
        "a clearing drain with a zero-length gap must still be contiguous, got {:?} Hz",
        peaks
    );
}

/// **Acceptance criterion 5** — the measured replica spacing, recorded as a
/// number and reconciled against what H1 predicts.
///
/// **Derivation, since the handoff's "multiples of `sr/L`" leaves `L`
/// ambiguous and the two candidate readings give different numbers.**
///
/// Let `C` be the retained chunk (`CHUNK_SECS · sr`) and `G` the discarded
/// gap (`process_secs · sr`). In the *assembled* window, fragment `k` occupies
/// samples `[kC, (k+1)C)` but carries the phase of absolute time
/// `k(C+G) + (m − kC)`. So the window equals a clean tone multiplied by a
/// staircase phase factor that steps by `2π · frac(f·G/sr)` once per `C`
/// samples.
///
/// The periodicity is therefore `C` — the *chunk*, not `C+G`. Sidebands land
/// at multiples of `sr/C = 1/CHUNK_SECS = 20 Hz`. Reading `L` as the fragment
/// stride `C+G` would predict 18.2 Hz, which is wrong: the stride sets the
/// step *size*, the chunk sets the repeat *rate*.
///
/// **Measured, on this reproducer, at `sr = 48 000`, `CHUNK_SECS = 0.05`,
/// `process_secs = 0.005`, tone 15 100 Hz:**
///
/// - **101 replicas** above −40 dB, spanning **14 130 Hz … 16 130 Hz**;
/// - **spacing exactly 20 Hz**, uniform — every one of the 100 adjacent gaps
///   measured 20 Hz, with no scatter;
/// - predicted `sr/C = 48 000/2400 = 20 Hz`. **Reconciled exactly.**
///
/// The reading `L = C + G` would have predicted 18.2 Hz and is falsified by
/// the measurement, which settles the ambiguity noted above.
///
/// **This does not explain the reported symptom, and that is the point.** The
/// reported symptom is a *temporal* recurrence — the response reappearing
/// every ~3–5 s with no stimulus present (see the scope correction in this
/// module's docs). What splicing produces is a dense 101-line comb in a ±1 kHz
/// skirt around the stimulus: a frequency-domain artifact that cannot repeat
/// anything in time. Two consequences, both binding:
///
/// 1. A confirmed splice does **not** close the reported issue, and cannot:
///    it is the wrong domain. Acceptance criterion 5's spacing reconciliation
///    is satisfied *for this defect* (20 Hz, derived and measured) while
///    saying nothing about the recurrence.
/// 2. The splice is a second, real, independently-fixable defect that these
///    tests now pin down. The reported recurrence needs its own instrument —
///    the D2 buffer dump over a stimulus-then-silence run, which answers
///    whether the captured audio itself repeats.
#[test]
fn spliced_replica_spacing_matches_tick_rate() {
    let sr = 48_000u32;
    let process_secs = 0.005;
    let (window, _) = assemble_window(Drain::Clearing, process_secs, sr);
    let db = meas_amp_db(&window, sr);

    // 1 Hz bins (nperseg = sr), so bin index is Hz and adjacent-peak spacing
    // in bins is spacing in Hz.
    let peaks = without_alias(peak_bins(&db, -40.0, MIN_PEAK_SEP_BINS), TONE_HZ, sr);
    assert!(
        peaks.len() >= 2,
        "need at least two peaks to measure a spacing, got {peaks:?}"
    );

    let gaps: Vec<usize> = peaks.windows(2).map(|w| w[1] - w[0]).collect();
    let median = {
        let mut g = gaps.clone();
        g.sort_unstable();
        g[g.len() / 2]
    };

    // Repeat rate is the chunk, not the fragment stride — see the derivation
    // in this test's doc comment.
    let chunk_samples = (sr as f64 * CHUNK_SECS).round();
    let predicted_hz = sr as f64 / chunk_samples;
    let l_samples = chunk_samples;

    // Generous tolerance: the point is to distinguish "tick-rate sidebands"
    // (~20 Hz) from "widely separated copies" (kHz), not to pin a decimal.
    assert!(
        (median as f64 - predicted_hz).abs() < predicted_hz * 0.5,
        "measured replica spacing {median} Hz does not reconcile with the \
         sr/L = {predicted_hz:.1} Hz that splicing at L = {l_samples} samples \
         predicts. An unreconciled spacing keeps the issue open — do not \
         widen this tolerance to make it pass. Peaks: {peaks:?}"
    );
}

/// Keeps [`without_alias`] honest. The filter drops a peak from every count
/// in this module, so it has to be shown that the thing it drops is really
/// the stimulus's own harmonic — present in the *contiguous* arm, where there
/// is no splice to blame it on.
///
/// Also pins the rate-dependence that an earlier version of this module got
/// wrong: the second harmonic folds at 48 kHz but not at 96 kHz, so a filter
/// that only handled the folded case failed at the higher rate on a peak that
/// was never a defect.
#[test]
fn alias_is_present_in_the_contiguous_arm() {
    let sr = 48_000;
    let alias = harmonic_in_band(TONE_HZ, 2, sr);
    assert!(
        (alias - 17_800.0).abs() < 1.0,
        "at 48 kHz the 2nd harmonic of 15 100 Hz must fold to 17 800 Hz, computed {alias}"
    );
    assert!(
        (harmonic_in_band(TONE_HZ, 2, 96_000) - 30_200.0).abs() < 1.0,
        "at 96 kHz the same harmonic is in-band at 30 200 Hz and must not be folded"
    );

    let (window, _) = assemble_window(Drain::Contiguous, 0.005, sr);
    let db = meas_amp_db(&window, sr);
    let unfiltered = peak_bins(&db, -40.0, MIN_PEAK_SEP_BINS);

    assert!(
        unfiltered.iter().any(|&p| (p as f64 - alias).abs() <= 5.0),
        "the alias must be what the filter removes; contiguous peaks were {unfiltered:?}"
    );
    assert_eq!(
        without_alias(unfiltered.clone(), TONE_HZ, sr).len(),
        unfiltered.len() - 1,
        "the filter must remove exactly the alias and nothing else"
    );
}

/// The discard count is the direct per-tick measure of the splice, so it must
/// track the modelled processing time rather than merely being nonzero.
#[test]
fn discard_count_tracks_the_modelled_processing_time() {
    let sr = 48_000u32;
    let ticks = 20;

    let mut eng = FakeEngine::new();
    eng.set_tone(TONE_HZ, 0.5);
    eng.reconnect_input("fake:capture_0").unwrap();
    eng.enable_ring_mode(0.01, 0, SAMPLE_ACCURATE);
    for _ in 0..ticks {
        eng.capture_block(CHUNK_SECS).unwrap();
    }

    // One gap is discarded per tick after the first (the first tick's clear
    // finds an empty ring), each of `process_secs · sr` samples.
    let per_gap = (sr as f64 * 0.01) as u64;
    let expected = per_gap * (ticks - 1);
    assert_eq!(
        eng.discarded_samples(),
        expected,
        "expected {ticks} ticks to discard {per_gap} samples each after the first"
    );
}

// ---------------------------------------------------------------------------
// Hardware runbook (D4, partial — transfer path only)
// ---------------------------------------------------------------------------

/// **Stimulus ceiling, enforced here because this test *is* the driver.**
///
/// There is no daemon in this path to clamp the request, so the cap lives in
/// the code and is deliberately not overridable from the environment. −40 dBFS
/// nominal; for a sine the nominal amplitude *is* the instantaneous peak (no
/// crest factor, unlike pink noise), so −40 dBFS here means −40 dBFS peak.
#[cfg(feature = "jack-audio")]
const HW_LEVEL_DBFS: f64 = -40.0;

/// HF sweep for the hardware read. Deliberately mixes round frequencies with
/// off-round ones: the headless work showed the effect vanishes when the tick
/// gap is a whole number of tone periods, so a sweep measures that
/// frequency-dependence instead of assuming it.
#[cfg(feature = "jack-audio")]
const HW_SWEEP_HZ: [f64; 4] = [12_000.0, 15_000.0, 15_100.0, 18_000.0];

/// Reads the actual replica frequencies off real hardware — the measurement
/// that discriminates the mechanisms (`handoff-capture-contiguity.md` D4):
/// geometric (`f`, `f·r`, `f·r²`) points at resampling, linear-symmetric
/// (`f`, `F−f`, `F+f`) at aliasing images, a tight ~20 Hz cluster at H1.
///
/// **Emits.** Drives `AC_TEST_OUTPUT_PORTS` at [`HW_LEVEL_DBFS`] and captures
/// the loopback from `AC_TEST_MONITOR_PORT`. Monitor ports mirror what is sent
/// to the corresponding playback channel, so no patch cable is needed — but
/// the signal does reach the physical output, which is why this is `#[ignore]`d
/// and run only with explicit per-run operator consent.
///
/// ```text
/// AC_TEST_OUTPUT_PORTS='Babyface Pro Pro:playback_4,Babyface Pro Pro:playback_5' \
/// AC_TEST_MONITOR_PORT='Babyface Pro Pro:monitor_4' \
///   cargo test --bin ac-daemon -- --ignored --nocapture jack_hf_sweep
/// ```
#[cfg(feature = "jack-audio")]
#[test]
#[ignore = "emits stimulus on real hardware; needs per-run operator consent"]
fn jack_hf_sweep_replica_read() {
    use crate::audio::jack_backend::JackEngine;
    use std::time::Duration;

    let out_ports: Vec<String> = std::env::var("AC_TEST_OUTPUT_PORTS")
        .expect("set AC_TEST_OUTPUT_PORTS")
        .split(',')
        .map(|s| s.trim().to_string())
        .collect();
    let monitor = std::env::var("AC_TEST_MONITOR_PORT").expect("set AC_TEST_MONITOR_PORT");
    let amplitude = 10f64.powf(HW_LEVEL_DBFS / 20.0);

    eprintln!("=== D4 hardware read: {HW_LEVEL_DBFS} dBFS into {out_ports:?}, capture {monitor}");

    for &tone in HW_SWEEP_HZ.iter() {
        let mut eng = JackEngine::new();
        eng.start(&out_ports, Some(&monitor)).expect("JACK start");
        let sr = eng.sample_rate();
        let nperseg = sr as usize;
        let step = nperseg / 2;
        let target_total = nperseg + step * (N_AVERAGES - 1);
        let chunk_samples = (sr as f64 * CHUNK_SECS) as usize;

        eng.set_tone(tone, amplitude);
        let _ = eng.capture_multi(0.3); // settle the output and the ring

        let baseline = eng.discarded_samples();
        let mut ring: Vec<f32> = Vec::with_capacity(target_total + step);
        let ticks = (target_total / chunk_samples) * 2;
        for _ in 0..ticks {
            let block = eng
                .capture_multi(CHUNK_SECS)
                .expect("capture_multi")
                .remove(0);
            ring.extend_from_slice(&block);
            if ring.len() > target_total {
                let excess = ring.len() - target_total;
                ring.drain(..excess);
            }
            // Stand-in for the transfer worker's per-tick compute.
            std::thread::sleep(Duration::from_millis(5));
        }
        let discarded = eng.discarded_samples() - baseline;

        // Silence before teardown so nothing is left driving the output.
        eng.set_silence();
        eng.stop();

        let rms =
            (ring.iter().map(|&x| (x as f64).powi(2)).sum::<f64>() / ring.len() as f64).sqrt();
        let db = meas_amp_db(&ring, sr);
        let peaks = without_alias(peak_bins(&db, -40.0, MIN_PEAK_SEP_BINS), tone, sr);
        let gaps: Vec<usize> = peaks.windows(2).map(|w| w[1] - w[0]).collect();
        let median_gap = if gaps.is_empty() {
            0
        } else {
            let mut g = gaps.clone();
            g.sort_unstable();
            g[g.len() / 2]
        };
        let phase_step = (tone * (discarded as f64 / ticks as f64) / sr as f64).fract();

        eprintln!(
            "tone={tone:>7.0} Hz  sr={sr}  rms={rms:.5} ({:.1} dBFS)  \
             discarded={discarded} ({:.0}/tick)  phase_step={phase_step:.3} cyc\n\
             \tpeaks={}  span={:?}..{:?} Hz  median_spacing={median_gap} Hz\n\
             \tfirst 12: {:?}",
            20.0 * rms.log10(),
            discarded as f64 / ticks as f64,
            peaks.len(),
            peaks.first(),
            peaks.last(),
            &peaks[..peaks.len().min(12)],
        );
    }
}

/// **The selection rule, measured on hardware and reproduced here.**
///
/// A real producer hands the ring one whole *period* at a time, so the gap a
/// `clear()` discards is always `k · period` samples. The phase discontinuity
/// a splice imposes is therefore `frac(f · period / sr)` cycles — **exactly
/// zero** whenever the stimulus is an integer multiple of `sr / period`. Such
/// a tone survives an arbitrary number of discarded periods with no phase
/// error at all, and the defect is spectrally invisible.
///
/// This is not a refinement, it is the dominant term. Measured on the RME
/// Babyface Pro (`sr = 96 000`, quantum 1024, so `sr/period = 93.75 Hz`),
/// −40 dBFS, identical discard rate (326/tick) in every case:
///
/// | tone | multiple of 93.75? | peaks | spacing |
/// |---|---|---|---|
/// | 12 000 Hz | yes (×128) | 1 | — |
/// | 15 000 Hz | yes (×160) | 1 | — |
/// | 18 000 Hz | yes (×192) | 1 | — |
/// | 15 100 Hz | **no** (×161.07) | **67** | 20 Hz |
///
/// A sample-accurate producer predicts splatter at all four and is simply
/// wrong. The check below is that the fake, given the same granularity,
/// reproduces the same split — i.e. that it *predicts* hardware rather than
/// merely resembling it.
///
/// Practical consequence, and the reason this matters beyond the model: the
/// round frequencies an operator naturally reaches for are exactly the ones
/// likely to be commensurate with the period, so **the obvious test tone can
/// hide this defect completely.**
#[test]
fn period_quantisation_decides_which_frequencies_expose_the_splice() {
    const SR: u32 = 96_000;
    const PERIOD: usize = 1024;
    let period_rate = SR as f64 / PERIOD as f64; // 93.75 Hz

    // Same split the hardware showed: three commensurate tones, one not.
    for &(tone, expect_clean) in &[
        (12_000.0f64, true),
        (15_000.0, true),
        (18_000.0, true),
        (15_100.0, false),
    ] {
        let commensurate = (tone / period_rate).fract() < 1e-9;
        assert_eq!(
            commensurate, expect_clean,
            "{tone} Hz vs period rate {period_rate}: test's own premise is wrong"
        );

        let (window, discarded) =
            assemble_window_with_period(Drain::Clearing, 0.005, SR, PERIOD, tone);
        assert!(discarded > 0, "{tone} Hz: the clear() must still discard");

        let db = meas_amp_db(&window, SR);
        let peaks = without_alias(peak_bins(&db, -40.0, MIN_PEAK_SEP_BINS), tone, SR);

        if expect_clean {
            assert_eq!(
                peaks.len(),
                1,
                "{tone} Hz is an exact multiple of sr/period ({period_rate} Hz), so a \
                 discarded whole period costs it zero phase — it must stay clean \
                 despite {discarded} samples discarded. Got peaks at {peaks:?} Hz"
            );
        } else {
            assert!(
                peaks.len() > 1,
                "{tone} Hz is NOT a multiple of sr/period ({period_rate} Hz), so each \
                 discarded period costs it {:.3} cycles of phase — it must replicate. \
                 Got peaks at {peaks:?} Hz",
                (tone * PERIOD as f64 / SR as f64).fract()
            );
        }
    }
}

/// **Recurrence probe for #208** — the actual reported defect: the response
/// reappearing every ~3–5 s, identical, *with no stimulus present*.
///
/// Drives a tone for `TONE_SECS`, silences the generator, and keeps capturing
/// for `SILENCE_SECS`, reporting per-second level and per-second energy at the
/// stimulus frequency. If the tone re-appears during the silent stretch, the
/// captured audio itself repeats and the defect is at or before capture
/// assembly. If the silent stretch stays silent, the captured stream is clean
/// and the recurrence is downstream of capture.
///
/// Runs both drains, because that also settles whether the `ac-ui`-era and
/// `ac-view`-era sightings share a cause: `monitor_spectrum` used
/// `capture_available` (never clears, so its ring can back up) while
/// `transfer_stream` uses `capture_multi` (clears every tick).
///
/// **Emits.** `#[ignore]`d, per-run operator consent required. Level is capped
/// in code at [`HW_LEVEL_DBFS`].
#[cfg(feature = "jack-audio")]
#[test]
#[ignore = "emits stimulus on real hardware; needs per-run operator consent"]
fn jack_stimulus_then_silence_recurrence_probe() {
    use crate::audio::jack_backend::JackEngine;

    const TONE_SECS: f64 = 5.0;
    const SILENCE_SECS: f64 = 30.0;

    let out_ports: Vec<String> = std::env::var("AC_TEST_OUTPUT_PORTS")
        .expect("set AC_TEST_OUTPUT_PORTS")
        .split(',')
        .map(|s| s.trim().to_string())
        .collect();
    let monitor = std::env::var("AC_TEST_MONITOR_PORT").expect("set AC_TEST_MONITOR_PORT");
    let amplitude = 10f64.powf(HW_LEVEL_DBFS / 20.0);

    for clearing in [true, false] {
        let arm = if clearing {
            "capture_multi (transfer path, clears every tick)"
        } else {
            "capture_available (monitor path, never clears)"
        };
        let mut eng = JackEngine::new();
        eng.start(&out_ports, Some(&monitor)).expect("JACK start");
        let sr = eng.sample_rate();
        let chunk = (sr as f64 * CHUNK_SECS) as usize;

        eng.set_tone(TONE_HZ, amplitude);
        let mut stream: Vec<f32> = Vec::new();
        // Per-tick returned sizes. On the non-clearing arm a size that keeps
        // exceeding `chunk` means the ring is backing up faster than it is
        // drained — the direct signature of H3.
        let mut block_sizes: Vec<usize> = Vec::new();
        let total_ticks = ((TONE_SECS + SILENCE_SECS) / CHUNK_SECS) as usize;
        let tone_ticks = (TONE_SECS / CHUNK_SECS) as usize;

        for t in 0..total_ticks {
            if t == tone_ticks {
                eng.set_silence();
            }
            let block = if clearing {
                // Blocks internally until `chunk` samples exist, so it paces
                // itself.
                eng.capture_multi(CHUNK_SECS)
                    .expect("capture_multi")
                    .remove(0)
            } else {
                // Non-blocking: returns whatever has accrued since the last
                // call. The caller must pace itself, exactly as
                // `monitor_spectrum`'s sliding-ring loop does — without this
                // the loop spins and captures nothing.
                std::thread::sleep(std::time::Duration::from_secs_f64(CHUNK_SECS));
                eng.capture_available(chunk).expect("capture_available")
            };
            block_sizes.push(block.len());
            stream.extend_from_slice(&block);
        }
        let discarded = eng.discarded_samples();
        eng.set_silence();
        eng.stop();

        eprintln!(
            "\n=== {arm}\n    sr={sr} discarded={discarded} captured={} samples ({:.1} s of audio)",
            stream.len(),
            stream.len() as f64 / sr as f64
        );
        let bmin = block_sizes.iter().min().copied().unwrap_or(0);
        let bmax = block_sizes.iter().max().copied().unwrap_or(0);
        let bmean = block_sizes.iter().sum::<usize>() as f64 / block_sizes.len().max(1) as f64;
        eprintln!(
            "    per-tick block samples: min={bmin} max={bmax} mean={bmean:.0} (nominal {chunk})"
        );
        eprintln!("     sec | rms dBFS | tone dBFS    (generator off after {TONE_SECS} s)");
        for (i, sec) in stream.chunks(sr as usize).enumerate() {
            if sec.len() < sr as usize / 2 {
                break;
            }
            let rms =
                (sec.iter().map(|&x| (x as f64).powi(2)).sum::<f64>() / sec.len() as f64).sqrt();
            let tone = goertzel_dbfs(sec, sr as f64, TONE_HZ);
            let mark = if (i as f64) < TONE_SECS {
                "STIM"
            } else {
                "  --"
            };
            eprintln!(
                "    {mark} {i:>3} | {:>8.1} | {tone:>8.1}",
                20.0 * rms.max(1e-12).log10()
            );
        }
    }
}

/// Single-bin magnitude at `freq`, in dBFS — enough to say whether the
/// stimulus tone is present in a one-second slice without a full FFT.
#[cfg(feature = "jack-audio")]
fn goertzel_dbfs(samples: &[f32], sr: f64, freq: f64) -> f64 {
    let n = samples.len();
    let k = (0.5 + (n as f64 * freq) / sr).floor();
    let w = 2.0 * std::f64::consts::PI * k / n as f64;
    let coeff = 2.0 * w.cos();
    let (mut s1, mut s2) = (0.0f64, 0.0f64);
    for &x in samples {
        let s0 = x as f64 + coeff * s1 - s2;
        s2 = s1;
        s1 = s0;
    }
    let mag = (s1 * s1 + s2 * s2 - s1 * s2 * coeff).sqrt() / n as f64 * 2.0;
    20.0 * mag.max(1e-12).log10()
}

/// **The inverted guard (#207 fix).** Same stimulus, same window assembly,
/// same period quantisation as
/// `spliced_capture_of_a_single_tone_replicates_the_response` — the only
/// change is that the tick uses `capture_multi_contiguous` instead of
/// `capture_multi`, i.e. no pre-wait `clear()`.
///
/// One tone in, one peak out, and nothing discarded.
///
/// This is the assertion the original guard test promised it would become.
#[test]
fn fixed_streaming_drain_of_a_single_tone_produces_one_peak() {
    let sr = 48_000;
    let (window, discarded) = assemble_window(Drain::Fixed, 0.005, sr);

    assert_eq!(
        discarded, 0,
        "the streaming drain must not clear the ring — any discard is a splice"
    );

    let db = meas_amp_db(&window, sr);
    let peaks = without_alias(peak_bins(&db, -40.0, MIN_PEAK_SEP_BINS), TONE_HZ, sr);
    assert_eq!(
        peaks.len(),
        1,
        "contiguous streaming capture of one tone must give exactly one peak, \
         got {} at {:?} Hz",
        peaks.len(),
        peaks
    );
    assert!(
        (peaks[0] as f64 - TONE_HZ).abs() < 5.0,
        "the one peak must sit at the stimulus frequency, got {} Hz",
        peaks[0]
    );
}

/// The fix must hold at the frequencies that *did* expose the splice.
///
/// `period_quantisation_decides_which_frequencies_expose_the_splice` shows
/// 15 100 Hz replicating under the clearing drain at a 1024-sample producer
/// granularity while the commensurate tones stay clean. Under the contiguous
/// drain every one of them must be clean — otherwise the fix only works where
/// the defect was already invisible, which would be no fix at all.
#[test]
fn fix_holds_across_the_frequencies_that_exposed_the_splice() {
    const SR: u32 = 96_000;
    const PERIOD: usize = 1024;

    for tone in [12_000.0f64, 15_000.0, 15_100.0, 18_000.0] {
        let (window, discarded) =
            assemble_window_with_period(Drain::Fixed, 0.005, SR, PERIOD, tone);
        assert_eq!(discarded, 0, "{tone} Hz: streaming drain must not discard");

        let db = meas_amp_db(&window, SR);
        let peaks = without_alias(peak_bins(&db, -40.0, MIN_PEAK_SEP_BINS), tone, SR);
        assert_eq!(
            peaks.len(),
            1,
            "{tone} Hz must be clean under the contiguous drain, got {peaks:?} Hz"
        );
    }
}

/// Latency must stay bounded. Dropping the `clear()` on its own would trade
/// the splice for an ever-growing backlog — issue #208 on this path — so the
/// contiguous drain returns *everything* available, not just the requested
/// chunk. Feed the ring faster than the consumer asks and the surplus must
/// come back out rather than accumulate.
#[test]
fn contiguous_drain_does_not_accumulate_a_backlog() {
    let sr = 48_000u32;
    let chunk = (sr as f64 * CHUNK_SECS) as usize;

    let mut eng = FakeEngine::new();
    eng.set_tone(TONE_HZ, 0.5);
    eng.reconnect_input("fake:capture_0").unwrap();
    eng.add_ref_input("fake:capture_0").unwrap();
    // process_secs deliberately larger than the chunk: every tick accrues
    // more than the caller nominally asks for.
    eng.enable_ring_mode(CHUNK_SECS * 2.0, 1, SAMPLE_ACCURATE);

    let mut returned = Vec::new();
    for _ in 0..40 {
        let bufs = eng.capture_multi_contiguous(CHUNK_SECS).unwrap();
        returned.push(bufs[0].len());
    }

    // Steady state: each tick hands back roughly the chunk plus the accrued
    // surplus. If the drain were fixed-size the surplus would pile up in the
    // ring instead and this would sit at exactly `chunk` forever while
    // latency grew without bound.
    let late: usize = returned[20..].iter().sum::<usize>() / 20;
    assert!(
        late >= chunk,
        "steady-state drain {late} is below the {chunk}-sample chunk — backlog is accumulating"
    );
    assert_eq!(
        eng.discarded_samples(),
        0,
        "bounded latency must come from draining, never from discarding"
    );
}
