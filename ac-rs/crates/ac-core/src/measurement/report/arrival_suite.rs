//! #537 architect revision 3: the falsification suite for the band-limited
//! arrival (operator ruling 2026-09-18, item 2: "freeze revision 2 and try
//! to falsify it before tuning any constant").
//!
//! Every case is two copies of one kernel — a first path at `t0` and a
//! second, whole-kernel copy `D` later, at a level re the first and either
//! polarity, delayed by a fractional amount through an FFT phase shift —
//! over no background or the recorded pupu background at a stated arrival
//! SNR. Each case runs the production path, [`MeasurementReport::ir_stats`],
//! and is scored as one of three outcomes: a flight time produced within
//! [`BUDGET`] samples of the first path, a flight time produced wrong, or a
//! flight time withheld. Accepted wrong arrivals are counted apart from
//! refusals; a repeated wrong answer is a failure, never stability.
//!
//! Kernels, both at 96 kHz and captured through the Farina chain in the
//! default band and in 20 Hz–10 kHz:
//! - the pure-delay ESS chain (the cable's analogue);
//! - #346's two-way DUT at its rig-like group delay
//!   ([`crate::measurement::sweep::TWO_WAY_RIG_LIKE`], the speaker's).
//!
//! Architect revision 3's prototype of this suite
//! (`$AC_HOME/log/arch-537/suite2.py`, `suite3.py`) used recorded rig IRs
//! as kernels; this one uses synthetic kernels so it can run in the tree.

use rand::{rngs::StdRng, Rng, SeedableRng};
use rayon::prelude::*;
use realfft::num_complex::Complex;
use realfft::RealFftPlanner;

use super::fixtures::*;
use super::ir_stats::{band_limited_arrival_under, ArrivalRule};
use super::*;
use crate::measurement::sweep::{
    deconvolve_full, extract_irs, inverse_sweep, ir_peak, log_sweep, two_way_bounded,
    zero_phase_high_pass, SweepParams, ARRIVAL_HIGH_PASS_CORNER_HZ, IR_DEFAULT_DURATION_S,
    IR_DEFAULT_F1_HZ, IR_DEFAULT_F2_HZ, TWO_WAY_RIG_LIKE, TWO_WAY_T0,
};

const SR: u32 = 96_000;
/// The default 0.4 s gate at 96 kHz.
const LEN: usize = 38_400;
/// Where every kernel's first path lands: 600 samples after the gate
/// centre.
const T0: usize = LEN / 2 + 600;
/// The stored τ every case subtracts.
const TAU_S: f64 = 0.001;
/// A produced flight time more than this many samples from the first path
/// is wrong.
const BUDGET: i64 = 5;
/// Raised-cosine taper on each end of a kernel before it is shifted: the
/// prototype's first run was corrupted by edge wrap.
const TAPER: usize = 256;
/// Background draws per kernel; case `i` uses draw `i mod DRAWS`.
const DRAWS: usize = 16;

const LEVELS_DB: [f64; 10] = [-6.0, -3.0, 0.0, 3.0, 6.0, 10.0, 15.0, 19.0, 21.0, 25.0];
const SEPARATIONS_MS: [f64; 7] = [0.25, 0.5, 0.75, 1.0, 2.0, 5.0, 10.0];
const FRACTIONS: [f64; 2] = [0.0, 0.37];
const POLARITIES: [f64; 2] = [1.0, -1.0];
/// `None`: no background. `Some(snr)`: the recorded background at that
/// arrival SNR, in dB.
const BACKGROUNDS: [Option<f64>; 3] = [None, Some(50.0), Some(40.0)];

/// The first 9600 samples (100 ms) of pupu's 2 m capture
/// `2026-09-18T22-05-38Z-plot_ir.json`, `data[0].data.linear_ir`, as
/// little-endian f32: the recorded pre-impulse background.
static BACKGROUND_F32LE: &[u8] =
    include_bytes!("testdata/pupu-background-2026-09-18T22-05-38Z.f32le");

/// One kernel, `LEN` long, first path at [`T0`], with its shifted copies
/// and background draws precomputed.
struct Kernel {
    name: &'static str,
    f2_hz: f64,
    ir: Vec<f64>,
    /// `(separation_ms, fraction)` → the kernel delayed by that much.
    copies: Vec<((f64, f64), Vec<f64>)>,
    /// Background draws scaled to 0 dB arrival SNR against this kernel.
    backgrounds: Vec<Vec<f64>>,
}

#[derive(Debug, Clone, Copy)]
struct Case {
    kernel: usize,
    level_db: f64,
    separation_ms: f64,
    fraction: f64,
    polarity: f64,
    background: Option<f64>,
}

impl Case {
    /// The second path's delay past the first, in samples.
    fn separation_samples(&self) -> f64 {
        self.separation_ms * SR as f64 / 1000.0 + self.fraction
    }

    /// `D ≥ 1/f_c`: separated by at least one corner period.
    fn resolved(&self) -> bool {
        self.separation_ms * 1e-3 >= 1.0 / ARRIVAL_HIGH_PASS_CORNER_HZ
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Outcome {
    /// A flight time produced within [`BUDGET`] of the first path.
    Right,
    /// A flight time produced `error` samples from the first path.
    Wrong {
        error: i64,
    },
    Withheld,
}

fn score(produced: bool, arrival_index: usize) -> Outcome {
    let error = arrival_index as i64 - T0 as i64;
    match (produced, error.abs() <= BUDGET) {
        (false, _) => Outcome::Withheld,
        (true, true) => Outcome::Right,
        (true, false) => Outcome::Wrong { error },
    }
}

/// `x` delayed by `d` samples (fractional allowed) through a phase shift
/// over a transform twice its length, so nothing wraps.
fn delayed(x: &[f64], d: f64) -> Vec<f64> {
    let n = x.len();
    let m = 2 * n;
    let mut planner = RealFftPlanner::<f64>::new();
    let r2c = planner.plan_fft_forward(m);
    let c2r = planner.plan_fft_inverse(m);
    let mut input = r2c.make_input_vec();
    input[..n].copy_from_slice(x);
    let mut spectrum = r2c.make_output_vec();
    r2c.process(&mut input, &mut spectrum).unwrap();
    for (k, v) in spectrum.iter_mut().enumerate() {
        let phase = -2.0 * std::f64::consts::PI * k as f64 * d / m as f64;
        *v *= Complex::from_polar(1.0, phase);
    }
    // The inverse of a real signal: DC and Nyquist are real.
    spectrum[0].im = 0.0;
    let last = spectrum.len() - 1;
    spectrum[last].im = 0.0;
    let mut output = c2r.make_output_vec();
    c2r.process(&mut spectrum, &mut output).unwrap();
    output.truncate(n);
    output.iter_mut().for_each(|v| *v /= m as f64);
    output
}

/// Stationary noise `len` long with the recorded background's magnitude
/// spectrum and a seeded uniform random phase.
fn background_draw(len: usize, seed: u64) -> Vec<f64> {
    let recorded: Vec<f64> = BACKGROUND_F32LE
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f64)
        .collect();
    assert_eq!(recorded.len(), 9_600, "testdata length");
    let mut planner = RealFftPlanner::<f64>::new();
    let r2c = planner.plan_fft_forward(len);
    let c2r = planner.plan_fft_inverse(len);
    let mut input = r2c.make_input_vec();
    let n = recorded.len().min(len);
    input[..n].copy_from_slice(&recorded[..n]);
    let mut spectrum = r2c.make_output_vec();
    r2c.process(&mut input, &mut spectrum).unwrap();
    let mut rng = StdRng::seed_from_u64(seed);
    for v in spectrum.iter_mut() {
        let phase = 2.0 * std::f64::consts::PI * rng.gen::<f64>();
        *v = Complex::from_polar(v.norm(), phase);
    }
    spectrum[0].im = 0.0;
    let last = spectrum.len() - 1;
    spectrum[last].im = 0.0;
    let mut output = c2r.make_output_vec();
    c2r.process(&mut spectrum, &mut output).unwrap();
    output
}

fn rms(x: &[f64]) -> f64 {
    (x.iter().map(|v| v * v).sum::<f64>() / x.len() as f64).sqrt()
}

/// Taper `ir`'s ends and place it so its sample `t0` lands on [`T0`] of a
/// `LEN`-long buffer.
fn embed(ir: &[f64], t0: usize) -> Vec<f64> {
    let mut tapered = ir.to_vec();
    let n = tapered.len();
    for k in 0..TAPER {
        let w = 0.5 - 0.5 * (std::f64::consts::PI * k as f64 / TAPER as f64).cos();
        tapered[k] *= w;
        tapered[n - 1 - k] *= w;
    }
    let mut out = vec![0.0; LEN];
    let offset = T0 - t0;
    out[offset..offset + n].copy_from_slice(&tapered);
    out
}

fn band(f2_hz: f64) -> SweepParams {
    SweepParams {
        f1_hz: IR_DEFAULT_F1_HZ,
        f2_hz,
        duration_s: IR_DEFAULT_DURATION_S,
        sample_rate: SR,
    }
}

/// The pure-delay chain: `log_sweep` → 1000-sample delay →
/// `deconvolve_full` → `extract_irs`, 4096-sample window.
fn pure_delay(p: &SweepParams) -> Vec<f64> {
    const DELAY: usize = 1_000;
    const WINDOW: usize = 4_096;
    let x = log_sweep(p).unwrap();
    let mut y = vec![0.0_f32; x.len() + DELAY];
    y[DELAY..].copy_from_slice(&x);
    let full = deconvolve_full(&y, &inverse_sweep(p).unwrap());
    let irs = extract_irs(&full, p, 1, WINDOW).unwrap();
    embed(&irs.linear, WINDOW / 2 + DELAY)
}

/// #346's two-way DUT at its rig-like group delay, first path at its HF
/// component.
fn speaker(p: &SweepParams) -> Vec<f64> {
    let r = two_way_bounded(p, &TWO_WAY_RIG_LIKE);
    embed(&r.ir, (r.centre as i64 + TWO_WAY_T0) as usize)
}

fn kernel(name: &'static str, f2_hz: f64, ir: Vec<f64>, seed: u64) -> Kernel {
    let copies = SEPARATIONS_MS
        .iter()
        .flat_map(|&d| FRACTIONS.iter().map(move |&f| (d, f)))
        .map(|(d, f)| ((d, f), delayed(&ir, d * SR as f64 / 1000.0 + f)))
        .collect();
    let peak_hp = ir_peak(&zero_phase_high_pass(&ir, SR, ARRIVAL_HIGH_PASS_CORNER_HZ)).1;
    let backgrounds = (0..DRAWS as u64)
        .map(|i| {
            let draw = background_draw(LEN, seed * 1_000 + i);
            let scale = peak_hp
                / rms(&zero_phase_high_pass(
                    &draw,
                    SR,
                    ARRIVAL_HIGH_PASS_CORNER_HZ,
                ));
            draw.into_iter().map(|v| v * scale).collect()
        })
        .collect();
    Kernel {
        name,
        f2_hz,
        ir,
        copies,
        backgrounds,
    }
}

fn kernels() -> Vec<Kernel> {
    [IR_DEFAULT_F2_HZ, 10_000.0]
        .into_iter()
        .enumerate()
        .flat_map(|(i, f2)| {
            let p = band(f2);
            [
                kernel("pure delay", f2, pure_delay(&p), 10 + i as u64),
                kernel("two-way", f2, speaker(&p), 20 + i as u64),
            ]
        })
        .collect()
}

fn cases(n_kernels: usize) -> Vec<Case> {
    let mut out = Vec::new();
    for kernel in 0..n_kernels {
        for &level_db in &LEVELS_DB {
            for &separation_ms in &SEPARATIONS_MS {
                for &fraction in &FRACTIONS {
                    for &polarity in &POLARITIES {
                        for &background in &BACKGROUNDS {
                            out.push(Case {
                                kernel,
                                level_db,
                                separation_ms,
                                fraction,
                                polarity,
                                background,
                            });
                        }
                    }
                }
            }
        }
    }
    out
}

/// The capture of `case`: first path, second path, background.
fn capture(k: &Kernel, case: &Case, index: usize) -> Vec<f64> {
    let copy = &k
        .copies
        .iter()
        .find(|(key, _)| *key == (case.separation_ms, case.fraction))
        .expect("precomputed")
        .1;
    let gain = case.polarity * 10f64.powf(case.level_db / 20.0);
    let mut h: Vec<f64> = k.ir.iter().zip(copy).map(|(a, b)| a + gain * b).collect();
    if let Some(snr_db) = case.background {
        let scale = 10f64.powf(-snr_db / 20.0);
        for (v, n) in h.iter_mut().zip(&k.backgrounds[index % DRAWS]) {
            *v += scale * n;
        }
    }
    h
}

/// A case's outcomes under the shipped rule, through `ir_stats`, without
/// and with the true distance typed, and under the rejected revision-2
/// rule, inline.
#[derive(Debug, Clone, Copy)]
struct Scored {
    case: Case,
    shipped: Outcome,
    with_distance: Outcome,
    /// The flight time produced with the distance typed, seconds.
    flight_with_distance_s: Option<f64>,
    rejected: Outcome,
}

/// The distance whose `d / c` is exactly the first path's flight time
/// (`T0 − centre − τ`), at the default speed of sound.
fn true_distance_m() -> f64 {
    let flight = (T0 - LEN / 2) as f64 / SR as f64 - TAU_S;
    flight * crate::shared::conversions::speed_of_sound_from_config(None)
}

/// The window around `d / c` the true distance allows, in seconds re
/// `d / c`, computed from the constants rather than read from the check.
fn true_window_s() -> (f64, f64) {
    let d = true_distance_m();
    let c = crate::shared::conversions::speed_of_sound_from_config(None);
    let eps = (DISTANCE_TAPE_TOLERANCE_M + DISTANCE_SPEED_OF_SOUND_REL_TOL * d) / c;
    (-eps, eps + ARRIVAL_EXCESS_DELAY_ALLOWANCE_S)
}

/// Revision 2's thresholds: `EarlierComparable` at 6 dB, the SNR gate at
/// 20 dB, and no distance check.
const REVISION_2: ArrivalRule = ArrivalRule {
    snr_min_db: 20.0,
    earlier_comparable_db: 6.0,
};

fn run(k: &Kernel, case: &Case, index: usize) -> Scored {
    let h = capture(k, case, index);
    let rejected = {
        let a = band_limited_arrival_under(&h, SR, k.f2_hz, ir_peak(&h).0, REVISION_2);
        score(!a.cross_check.withholds_flight_time(), a.arrival_index)
    };
    let mut report = ir_report_with_custom_ir_band(h, SR, k.f2_hz);
    with_live_latency(&mut report, TAU_S);
    let stats = report.ir_stats().expect("an impulse response");
    report.position = Some(PositionSnapshot {
        distance_m: Some(true_distance_m()),
        ..Default::default()
    });
    let with = report.ir_stats().expect("an impulse response");
    Scored {
        case: *case,
        shipped: score(stats.flight_time_s.is_some(), stats.arrival_index),
        with_distance: score(with.flight_time_s.is_some(), with.arrival_index),
        flight_with_distance_s: with.flight_time_s,
        rejected,
    }
}

fn relation(level_db: f64) -> &'static str {
    if level_db < 0.0 {
        "weaker"
    } else if level_db == 0.0 {
        "equal"
    } else {
        "stronger"
    }
}

/// Right/wrong/withheld per kernel, second-path relation, separation class
/// and background.
fn print_table(kernels: &[Kernel], scored: &[Scored], pick: fn(&Scored) -> Outcome, title: &str) {
    println!("== {title}: right/WRONG/withheld");
    for (ki, k) in kernels.iter().enumerate() {
        for rel in ["weaker", "equal", "stronger"] {
            for resolved in [true, false] {
                let row: Vec<String> = BACKGROUNDS
                    .iter()
                    .map(|bg| {
                        let (mut r, mut w, mut x) = (0, 0, 0);
                        for s in scored.iter().filter(|s| {
                            s.case.kernel == ki
                                && relation(s.case.level_db) == rel
                                && s.case.resolved() == resolved
                                && s.case.background == *bg
                        }) {
                            match pick(s) {
                                Outcome::Right => r += 1,
                                Outcome::Wrong { .. } => w += 1,
                                Outcome::Withheld => x += 1,
                            }
                        }
                        let bg = bg.map_or("none".to_string(), |v| format!("{v:.0} dB"));
                        format!("{bg}: {r}/{w}/{x}")
                    })
                    .collect();
                println!(
                    "  {:10} {:>5.0} Hz  {:8} D{}0.5 ms  {}",
                    k.name,
                    k.f2_hz,
                    rel,
                    if resolved { ">=" } else { "< " },
                    row.join("  ")
                );
            }
        }
    }
}

#[test]
fn falsification_suite() {
    let kernels = kernels();
    let cases = cases(kernels.len());
    let scored: Vec<Scored> = cases
        .par_iter()
        .enumerate()
        .map(|(i, case)| run(&kernels[case.kernel], case, i))
        .collect();

    print_table(
        &kernels,
        &scored,
        |s| s.shipped,
        "shipped rule, no distance",
    );
    print_table(
        &kernels,
        &scored,
        |s| s.with_distance,
        "shipped rule, true distance",
    );
    print_table(&kernels, &scored, |s| s.rejected, "revision 2, inline");

    let expected_s =
        true_distance_m() / crate::shared::conversions::speed_of_sound_from_config(None);
    let (low_s, high_s) = true_window_s();
    let mut rejected_wrong_in_s1 = 0;
    let mut violations = Vec::new();
    for s in &scored {
        let c = &s.case;
        let k = &kernels[c.kernel];
        let label = format!(
            "{} {:.0} Hz, second path {:+} dB at {} ms + {} (polarity {:+}), background {:?}",
            k.name, k.f2_hz, c.level_db, c.separation_ms, c.fraction, c.polarity, c.background
        );
        let s1_region = c.resolved() && c.level_db <= 15.0;
        for (outcome, distance) in [(s.shipped, "no distance"), (s.with_distance, "distance")] {
            if let Outcome::Wrong { error } = outcome {
                // S1: resolved and no more than 15 dB stronger → never wrong.
                if s1_region {
                    violations.push(format!("S1: produced {error:+}, {distance} — {label}"));
                }
                // S4: a weaker or equal second path → never wrong.
                if c.level_db <= 0.0 {
                    violations.push(format!("S4: produced {error:+}, {distance} — {label}"));
                }
                // S3: the residual is late, and no later than the second path.
                if error <= 0 || error as f64 > c.separation_samples() + 2.0 {
                    violations.push(format!(
                        "S3: error {error:+} outside (0, D + 2], {distance} — {label}"
                    ));
                }
            }
        }
        // S2 (and S3's distance bound): with a distance, every produced
        // flight time lies inside the window. That bound is ε + A about the
        // exact typed distance; the operator-facing bound is the window
        // width, see `distance_residual_reaches_the_full_window_width`.
        if let Some(flight_s) = s.flight_with_distance_s {
            let excess_s = flight_s - expected_s;
            if !(low_s..=high_s).contains(&excess_s) {
                violations.push(format!(
                    "S2: flight {:+.1} samples re d/c outside the window — {label}",
                    excess_s * SR as f64
                ));
            }
        }
        if s1_region && matches!(s.rejected, Outcome::Wrong { .. }) {
            rejected_wrong_in_s1 += 1;
        }
    }
    println!(
        "S1 region, revision 2 inline: {rejected_wrong_in_s1} produced wrong \
         (S5 needs at least 1)"
    );
    for v in violations.iter().take(40) {
        println!("{v}");
    }
    for tag in ["S1", "S2", "S3", "S4"] {
        let n = violations.iter().filter(|v| v.starts_with(tag)).count();
        println!("{tag}: {n} violations");
    }
    // Each kernel's S1/S4 rows must be able to go red, or say plainly that
    // they cannot: a kernel that produces nothing makes them vacuous there.
    for (ki, k) in kernels.iter().enumerate() {
        let right = scored
            .iter()
            .filter(|s| s.case.kernel == ki && s.shipped == Outcome::Right)
            .count();
        if k.name == "two-way" && k.f2_hz == 10_000.0 {
            // #346's B rig-like: ArrivalAmbiguous on every case, the known
            // false refusal recorded in peak.rs. If this starts producing,
            // S1/S4 gain a kernel; re-read the table before trusting a green.
            assert_eq!(right, 0, "{} {:.0} Hz now produces", k.name, k.f2_hz);
        } else {
            assert!(
                right > 0,
                "{} {:.0} Hz produces nothing: S1/S4 vacuous",
                k.name,
                k.f2_hz
            );
        }
    }
    // S5: the rejected rule, run inline over S1's region, is wrong at least
    // once — so S1 is able to go red.
    assert!(
        rejected_wrong_in_s1 >= 1,
        "S5: revision 2 produced no wrong arrival in S1's region, so S1 cannot fail"
    );
    assert!(
        violations.is_empty(),
        "{} violations; first: {}",
        violations.len(),
        violations[0]
    );
}

/// `kernel` plus `draw` scaled so the capture's measured band-limited SNR
/// ([`IrStats::band_limited_snr_db`]) is `snr_db`, to within 0.01 dB and
/// never below it.
fn at_measured_snr(kernel: &[f64], draw: &[f64], snr_db: f64) -> MeasurementReport {
    let build = |scale: f64| {
        let h = kernel
            .iter()
            .zip(draw)
            .map(|(k, n)| k + scale * n)
            .collect();
        let mut r = ir_report_with_custom_ir_band(h, SR, IR_DEFAULT_F2_HZ);
        with_live_latency(&mut r, TAU_S);
        r
    };
    let snr = |r: &MeasurementReport| r.ir_stats().unwrap().band_limited_snr_db.unwrap();
    // Start from the whole-draw RMS above the corner, as the suite scales.
    let hp = |x: &[f64]| zero_phase_high_pass(x, SR, ARRIVAL_HIGH_PASS_CORNER_HZ);
    let mut scale = ir_peak(&hp(kernel)).1 / rms(&hp(draw)) * 10f64.powf(-snr_db / 20.0);
    for _ in 0..4 {
        scale *= 10f64.powf((snr(&build(scale)) - snr_db) / 20.0);
    }
    // The pre-region is noise-dominated, so the SNR is linear in dB of
    // `scale`; a hair smaller lands on or above the target.
    let report = build(scale * (1.0 - 1e-5));
    let measured = snr(&report);
    assert!(
        (snr_db..snr_db + 0.01).contains(&measured),
        "test setup: SNR {measured}, wanted {snr_db}"
    );
    report
}

/// The with-distance residual is bounded by the window's width, A + 2ε(d),
/// not A + ε(d) (#537 architect revision 5): a typed distance off by ε puts
/// the true path on the window's low edge, and a later path more than 20 dB
/// stronger, midway between A + ε and A + 2ε, is produced. The first assert
/// measures the rejected bound: if it ever fails, the window has narrowed
/// and the docs overstate the bound. The second holds the stated one, with
/// S3's 2-sample pick allowance.
#[test]
fn distance_residual_reaches_the_full_window_width() {
    let c = crate::shared::conversions::speed_of_sound_from_config(None);
    let flight_true = (T0 - LEN / 2) as f64 / SR as f64 - TAU_S;
    let rel = DISTANCE_SPEED_OF_SOUND_REL_TOL;
    let d_typed = (flight_true * c + DISTANCE_TAPE_TOLERANCE_M) / (1.0 - rel);
    let eps = (DISTANCE_TAPE_TOLERANCE_M + rel * d_typed) / c;
    let a_plus_eps = ARRIVAL_EXCESS_DELAY_ALLOWANCE_S + eps;
    let width = ARRIVAL_EXCESS_DELAY_ALLOWANCE_S + 2.0 * eps;
    // Midway between the rejected bound and the window's width.
    let d_samples = ((a_plus_eps + width) / 2.0 * SR as f64).round() as usize;
    let mut ir = vec![0.0; LEN];
    ir[T0] = 0.05; // −26 dB: below EarlierComparable's 20 dB, residual case 1
    ir[T0 + d_samples] = 1.0;
    let mut report = ir_report_with_custom_ir_band(ir, SR, 20_000.0);
    with_live_latency(&mut report, TAU_S);
    report.position = Some(PositionSnapshot {
        distance_m: Some(d_typed),
        ..Default::default()
    });
    let s = report.ir_stats().unwrap();
    let err = s.flight_time_s.expect("produced: Agrees and Consistent") - flight_true;
    println!(
        "typed {d_typed:.4} m: error {:.3} ms, A + ε {:.3} ms, A + 2ε {:.3} ms",
        err * 1e3,
        a_plus_eps * 1e3,
        width * 1e3
    );
    assert!(
        err > a_plus_eps,
        "the rejected bound A + ε is exceeded: {err}"
    );
    assert!(
        err <= width + 2.0 / SR as f64,
        "but never past the window: {err}"
    );
}

/// Earlier-comparable firings on `draws` background draws at a measured
/// arrival SNR of exactly `snr_db`, under `rule`.
fn earlier_comparable_on_noise(kernel: &[f64], snr_db: f64, rule: ArrivalRule) -> usize {
    (0..50)
        .filter(|seed| {
            let draw = background_draw(LEN, 53_700 + seed);
            let report = at_measured_snr(kernel, &draw, snr_db);
            let MeasurementData::ImpulseResponse { linear_ir, .. } = &report.data[0].data else {
                unreachable!()
            };
            let a = band_limited_arrival_under(
                linear_ir,
                SR,
                IR_DEFAULT_F2_HZ,
                ir_peak(linear_ir).0,
                rule,
            );
            assert_eq!(a.arrival_index, T0, "draw {seed}: test setup");
            assert!(
                !matches!(a.cross_check, ArrivalCrossCheck::BandLimitedSnrLow { .. }),
                "draw {seed}: refused on SNR at {snr_db} dB"
            );
            matches!(a.cross_check, ArrivalCrossCheck::EarlierComparable { .. })
        })
        .count()
}

/// #537 architect revision 3: [`ARRIVAL_SNR_MIN_DB`] and
/// [`ARRIVAL_EARLIER_COMPARABLE_DB`] only work together. Recorded
/// background (phase-randomised, 50 seeded draws, default 0.4 s gate)
/// under a single ideal impulse, at an arrival SNR of exactly the gate:
/// the earlier-comparable guard must fire on no draw — noise peaks sit
/// about 12.5 dB above their RMS, so under the −20 dB level. The same
/// draws at 30 dB, with only the SNR gate moved there (computed inline),
/// must make it fire on some: the refusal on noise, under the wrong
/// reason, that moving either constant alone brings back.
///
/// Recorded, not asserted: the pure-delay ESS kernel carries its own
/// high-passed skirt at −26.2 dB one sample past the lobe window (pupu's
/// cable read −26.1 dB there), so background at the gate adds to a level
/// already 6 dB under the guard, and it fires on some draws. That is a
/// refusal, never a produced number; the coupling above covers noise
/// peaks, not a pulse's skirt plus noise.
#[test]
fn snr_gate_keeps_background_peaks_below_the_earlier_comparable_level() {
    let mut spike = vec![0.0; LEN];
    spike[T0] = 1.0;
    let moved_alone = ArrivalRule {
        snr_min_db: 30.0,
        ..ArrivalRule::SHIPPED
    };
    let at_gate = earlier_comparable_on_noise(&spike, ARRIVAL_SNR_MIN_DB, ArrivalRule::SHIPPED);
    let at_30 = earlier_comparable_on_noise(&spike, 30.0, moved_alone);
    let skirt = earlier_comparable_on_noise(
        &pure_delay(&band(IR_DEFAULT_F2_HZ)),
        ARRIVAL_SNR_MIN_DB,
        ArrivalRule::SHIPPED,
    );
    println!(
        "EarlierComparable on noise: {at_gate}/50 at the gate, {at_30}/50 at 30 dB; \
         pure-delay kernel at the gate {skirt}/50 (recorded)"
    );
    assert_eq!(at_gate, 0, "the guard fires on background at the SNR gate");
    assert!(
        at_30 > 0,
        "at 30 dB the guard never fires on background, so this test cannot show the coupling"
    );
}
