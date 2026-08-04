//! Fractional-octave smoothing of an already-computed display curve (#229).
//!
//! This is a *display* operation on a curve that is already formed: it
//! averages neighbouring columns of `|H|` in dB and of unwrapped phase. It
//! runs after coherence exists, which is what makes it safe — the delay
//! sensitivity that makes a coarse column grid fragile
//! (`design-mtw-ladder.md`, decision 3) comes from summing `Sxy` across a
//! column's bins *before* the division, and nothing here touches a cross
//! spectrum. PPO stays 48 and the 616 µs tolerance stays with it however
//! heavy the smoothing is.
//!
//! # What is smoothed, and what is not
//!
//! - **Magnitude is smoothed in dB**, not as complex `H1`. Averaging complex
//!   `H1` across columns would let real and imaginary parts cancel wherever
//!   phase rotates, which is delay sensitivity reintroduced at the display —
//!   exactly what running after coherence formation avoids.
//! - **Phase is unwrapped before averaging.** A mean taken across a ±180°
//!   wrap lands near 0° — a number the measurement never contained.
//! - **Coherence is never smoothed.** It is the trust indicator, and
//!   smoothing it makes a bad measurement look good, which is the one
//!   direction an instrument must not fail in. There is deliberately no
//!   function here that smooths it.
//!
//! # Band geometry — base-2, not `G_OCTAVE` (decided, not inherited)
//!
//! An "octave" here is a factor of **two**: the half-width factor is
//! `δ = 2^(1/(2·bpo))`. This is the ladder's convention
//! ([`crate::visualize::mtw::ladder`]), not IEC 61260-1's
//! `G = 10^(3/10)` ([`crate::shared::constants::G_OCTAVE`]), and the choice
//! is deliberate:
//!
//! - the columns being averaged are laid out on the ladder's `2^(1/P)` grid,
//!   so a window specified in base-2 octaves spans a fixed number of columns
//!   across the whole axis; specifying it in base-ten octaves would not, and
//!   the window would breathe against the grid it slides over;
//! - this is Tier 2 display work. It claims no standards conformance, so it
//!   has no reason to carry the conformant ratio, and
//!   [`crate::visualize::fractional_octave`]'s `ioct_band_centers` /
//!   `ioct_band_edges` — which do carry it, because they aggregate into named
//!   bands — are deliberately **not** reused here. Sharing them would couple a
//!   display control to a constant that is normative for something else.
//!
//! The two differ by 0.24%. At 1/6 octave that is a window 0.24% wide of
//! wrong, which is invisible; the reason to write it down is that the next
//! person to unify the two constants must find both refusals, not one.
//!
//! # Window shape — Hann, widened to keep the designator honest
//!
//! The window is **Hann in log frequency**, not rectangular. A boxcar average
//! is the obvious implementation and the wrong one next to a sharp feature: a
//! deep narrow notch enters and leaves the window abruptly, so the smoothed
//! curve carries small ripples either side of it — structure the measurement
//! does not contain, at exactly the frequencies an operator is looking
//! hardest at. Hann tapers to zero at both edges and the ripples go with it.
//!
//! Tapering costs width, so the width is corrected rather than left to drift.
//! A Hann window's equivalent noise bandwidth is `(∫w)²/∫w² = 2/3` of its
//! full width, so the full width here is **1.5×** the nominal band: the
//! half-width is `2^(1.5/(2·bpo))`, and the result has the ENBW of a
//! `1/bpo`-octave rectangular average. Without that correction "1/6 octave"
//! would smooth like 1/9, and the label would overstate what was done — the
//! same class of quiet mismatch the caption exists to prevent.
//!
//! At the ends of the axis the window is **truncated, not reflected** — the
//! first and last columns are averaged over what actually exists, with the
//! weights they actually have. Reflecting would invent columns beyond the
//! measured range and draw them as measurement.

/// Smooth a dB-valued curve over `1/bpo` octave (base-2).
///
/// `freqs` must be ascending and positive — the ladder's column grid. `valid`
/// marks the columns the display trusts (the caller's coherence mask):
/// invalid columns never contribute to any window and pass through unchanged,
/// so a masked column's magnitude cannot leak into a drawn one.
///
/// Averaging is confined to **contiguous runs of valid columns**. A masked gap
/// splits the curve for exactly the reason it splits the drawn trace: the
/// columns either side of it are not neighbours in any sense the measurement
/// supports.
///
/// Returns `values_db` unchanged when `bpo == 0` or the three slices disagree
/// in length — a malformed call draws the input rather than a guess.
pub fn smooth_db(freqs: &[f64], values_db: &[f64], valid: &[bool], bpo: u32) -> Vec<f64> {
    smooth_runs(freqs, values_db, valid, bpo, |run| run.to_vec(), |v| v)
}

/// Smooth phase over `1/bpo` octave (base-2), unwrapping first.
///
/// The return is **unwrapped degrees** and may lie far outside ±180°. Wrapping
/// is the caller's — `ac-scene` owns the display's one wrap site, and a wrap
/// here would be a second one that could disagree with it.
///
/// Unwrapping restarts at every masked gap (see [`smooth_db`] for the run
/// rule). Continuing an unwrap through untrusted columns would carry their
/// 360° bookkeeping into the trusted columns after the gap, which is a
/// silent, unbounded error in a quantity the operator aligns systems by.
///
/// Where a run's phase advances by more than 180° between adjacent columns —
/// a large residual delay at high frequency — the unwrap follows the smaller
/// step, as every unwrap must. Smoothing then flattens that slope rather than
/// tracking it. This is inherent to smoothing a curve that is already
/// undersampled in phase, not a defect in the average.
pub fn smooth_unwrapped_phase_deg(
    freqs: &[f64],
    phase_deg: &[f64],
    valid: &[bool],
    bpo: u32,
) -> Vec<f64> {
    smooth_runs(freqs, phase_deg, valid, bpo, unwrap_deg, |v| v)
}

/// Shared machinery: split into valid runs, `pre` each run (identity for dB,
/// unwrap for phase), slide the window inside it, `post` each output value.
fn smooth_runs(
    freqs: &[f64],
    values: &[f64],
    valid: &[bool],
    bpo: u32,
    pre: impl Fn(&[f64]) -> Vec<f64>,
    post: impl Fn(f64) -> f64,
) -> Vec<f64> {
    let n = freqs.len();
    if bpo == 0 || values.len() != n || valid.len() != n {
        return values.to_vec();
    }
    // Half-width in octaves, base-2: half of the nominal `1/bpo` band, times
    // the 1.5 Hann ENBW correction (see the module header).
    let half_oct = 0.75 / bpo as f64;
    let delta = 2f64.powf(half_oct);

    let mut out = values.to_vec();
    let mut start = 0usize;
    while start < n {
        if !valid[start] {
            start += 1;
            continue;
        }
        let mut end = start;
        while end < n && valid[end] {
            end += 1;
        }
        smooth_run(
            &freqs[start..end],
            &pre(&values[start..end]),
            delta,
            half_oct,
            &post,
            &mut out[start..end],
        );
        start = end;
    }
    out
}

/// Relative slack on the window edges.
///
/// The grid is `2^(i/P)` and the window edge is `2^(1/(2·bpo))` — for the
/// designators in use the edge lands *on* a column, and the two sides of that
/// comparison are reached by different float paths. Without the slack the
/// window would gain or lose a whole column on an ULP, so a 1/6-octave
/// average over a 1/48-octave grid would span 8 columns here and 9 there.
const EDGE_EPS: f64 = 1e-12;

/// One contiguous run. `freqs` ascending, so the window bounds advance
/// monotonically and each column is visited a bounded number of times.
fn smooth_run(
    freqs: &[f64],
    values: &[f64],
    delta: f64,
    half_oct: f64,
    post: &impl Fn(f64) -> f64,
    out: &mut [f64],
) {
    let n = freqs.len();
    let mut lo = 0usize;
    let mut hi = 0usize; // exclusive
    for i in 0..n {
        let f = freqs[i];
        if f <= 0.0 || !f.is_finite() {
            // A non-positive centre has no octave neighbourhood. Pass it
            // through rather than inventing one.
            out[i] = post(values[i]);
            continue;
        }
        let f_lo = (f / delta) * (1.0 - EDGE_EPS);
        let f_hi = (f * delta) * (1.0 + EDGE_EPS);
        while lo < n && freqs[lo] < f_lo {
            lo += 1;
        }
        if hi < lo {
            hi = lo;
        }
        while hi < n && freqs[hi] <= f_hi {
            hi += 1;
        }
        // Hann weight by distance in octaves from the centre, normalised to
        // the half-width: 1 at the centre, 0 at both edges. The centre column
        // is always in the window (f/δ <= f <= f·δ) and always carries weight
        // 1, so the denominator is never zero however the window is
        // truncated at the ends of a run.
        let mut wsum = 0.0;
        let mut acc = 0.0;
        for j in lo..hi {
            let u = (freqs[j] / f).log2() / half_oct;
            let w = 0.5 * (1.0 + (std::f64::consts::PI * u.clamp(-1.0, 1.0)).cos());
            wsum += w;
            acc += w * values[j];
        }
        out[i] = post(acc / wsum);
    }
}

/// Unwrap degrees along the column axis: each step is moved to the
/// representative within ±180° of its predecessor.
fn unwrap_deg(phase_deg: &[f64]) -> Vec<f64> {
    let mut out = Vec::with_capacity(phase_deg.len());
    let mut prev = 0.0;
    for (i, &p) in phase_deg.iter().enumerate() {
        let v = if i == 0 {
            p
        } else {
            let mut d = p - prev;
            d -= 360.0 * (d / 360.0).round();
            prev + d
        };
        prev = v;
        out.push(v);
    }
    out
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// A `2^(1/ppo)` column grid — the ladder's geometry, which is what this
    /// module is specified against.
    fn grid(f_start: f64, ppo: usize, n: usize) -> Vec<f64> {
        (0..n)
            .map(|i| f_start * 2f64.powf(i as f64 / ppo as f64))
            .collect()
    }

    fn all_valid(n: usize) -> Vec<bool> {
        vec![true; n]
    }

    #[test]
    fn the_kernel_is_hann_shaped_and_reaches_the_stated_half_width() {
        // Probe with a unit impulse on a uniform grid, where every window is
        // full and identical, so the response reads the kernel directly.
        // 1/6 octave on a 1/48-octave grid: half-width 0.75/6 octave = 6
        // columns, tapering to zero there.
        let ppo = 48;
        let n = 41;
        let freqs = grid(100.0, ppo, n);
        let mut v = vec![0.0; n];
        v[20] = 1.0;
        let out = smooth_db(&freqs, &v, &all_valid(n), 6);

        assert!(out[20] > 0.0, "centre carries no weight");
        for k in 1..6 {
            assert!(
                out[20 + k] < out[20 + k - 1] && out[20 - k] > 0.0,
                "kernel is not tapering at ±{k} columns: {out:?}"
            );
        }
        for (i, &y) in out.iter().enumerate() {
            if !(14..=26).contains(&i) {
                assert!(y.abs() < 1e-12, "column {i} outside the window: {y}");
            }
        }
        // Symmetric on a uniform grid: the weight depends on distance in
        // octaves, not on which side of the centre it falls.
        for k in 1..6 {
            assert!(
                (out[20 + k] - out[20 - k]).abs() < 1e-12,
                "kernel asymmetric at ±{k}"
            );
        }
    }

    #[test]
    fn the_designator_is_enbw_matched_to_a_rectangular_band() {
        // What "1/6 octave" promises is a 1/6-octave average. A Hann window
        // of the same full width would deliver 2/3 of that, so the width
        // carries a 1.5x correction and the check is that the correction
        // lands: the kernel's equivalent noise bandwidth, (Σw)²/Σw² in
        // columns, must match the column count a rectangular 1/6-octave
        // window would cover.
        //
        // The tolerance is one column: the discrete kernel's end taps sit
        // exactly at the zeros of the Hann taper, which costs a little of
        // the nominal width at these grid densities.
        let ppo = 48usize;
        let n = 81;
        let freqs = grid(100.0, ppo, n);
        for &bpo in &[3u32, 6, 12] {
            let mut v = vec![0.0; n];
            v[40] = 1.0;
            let k = smooth_db(&freqs, &v, &all_valid(n), bpo);
            let sum: f64 = k.iter().sum();
            let sq: f64 = k.iter().map(|x| x * x).sum();
            let enbw = sum * sum / sq;

            // Rectangular reference: columns within ±1/(2·bpo) octave.
            let half = 0.5 / bpo as f64;
            let rect = freqs
                .iter()
                .filter(|&&f| {
                    let u = (f / freqs[40]).log2();
                    u >= -half - 1e-12 && u <= half + 1e-12
                })
                .count() as f64;
            assert!(
                (enbw - rect).abs() <= 1.0 + 1e-9,
                "bpo {bpo}: ENBW {enbw:.2} columns against a rectangular {rect} columns"
            );
        }
    }

    #[test]
    fn a_sharp_notch_smooths_without_ripple_beside_it() {
        // The reason the window is not rectangular. A boxcar average moves a
        // deep narrow notch's energy in and out of the window abruptly,
        // leaving small bumps either side of the notch — structure the
        // measurement does not contain, right where an operator is looking.
        // Computed here rather than imported, so the comparison is against
        // the implementation that was rejected, not against itself.
        let ppo = 48usize;
        let n = 81;
        let freqs = grid(100.0, ppo, n);
        let mut v = vec![0.0; n];
        v[40] = -40.0;

        let hann = smooth_db(&freqs, &v, &all_valid(n), 6);
        let half = 0.5 / 6.0;
        let boxcar: Vec<f64> = (0..n)
            .map(|i| {
                let inside: Vec<f64> = (0..n)
                    .filter(|&j| {
                        let u = (freqs[j] / freqs[i]).log2();
                        u >= -half - 1e-12 && u <= half + 1e-12
                    })
                    .map(|j| v[j])
                    .collect();
                inside.iter().sum::<f64>() / inside.len() as f64
            })
            .collect();

        // "Ripple" is the curvature the smoother introduces where the input
        // is flat: away from the notch, a second difference should be zero.
        let ripple = |c: &[f64]| -> f64 {
            (0..n)
                .filter(|&i| (2..n - 2).contains(&i) && (i as i64 - 40).abs() > 14)
                .map(|i| (c[i - 1] - 2.0 * c[i] + c[i + 1]).abs())
                .fold(0.0, f64::max)
        };
        assert!(
            ripple(&hann) <= ripple(&boxcar) + 1e-12,
            "Hann rippled more than the boxcar it replaced: {} against {}",
            ripple(&hann),
            ripple(&boxcar)
        );
        // And the notch is still a notch: smoothing shallows it, it does not
        // move it or fill it in.
        let deepest = hann
            .iter()
            .enumerate()
            .min_by(|a, b| a.1.partial_cmp(b.1).unwrap())
            .unwrap()
            .0;
        assert_eq!(deepest, 40, "the notch moved");
    }

    #[test]
    fn geometry_is_base_two_not_g_octave() {
        // A column placed between the base-2 window edge and where the
        // base-ten one would fall must be *inside* the window. It carries
        // near-zero Hann weight there, so the observable is not the averaged
        // value but whether the column is reached at all: under G_OCTAVE
        // geometry (0.24% narrower) it would sit outside and could not move
        // the centre at all. This pins the constant, not merely the shape.
        let f0 = 1000.0;
        let bpo = 1u32;
        let delta_2 = 2f64.powf(0.75); // this module's half-width, base-2
        let delta_g = crate::shared::constants::G_OCTAVE.powf(0.75);
        assert!(delta_g < delta_2, "G_OCTAVE is the narrower ratio");

        let between = f0 * (delta_g + delta_2) / 2.0;
        let freqs = vec![f0, between];
        let v = vec![0.0, 10.0];
        let out = smooth_db(&freqs, &v, &all_valid(2), bpo);
        assert!(
            out[0] > 0.0,
            "column at {between} Hz must be inside the base-2 window: got {}",
            out[0]
        );
    }

    #[test]
    fn flat_curve_is_unchanged_and_off_is_a_no_op() {
        let n = 30;
        let freqs = grid(50.0, 48, n);
        let v = vec![-12.5; n];
        let out = smooth_db(&freqs, &v, &all_valid(n), 3);
        for (i, &y) in out.iter().enumerate() {
            assert!((y + 12.5).abs() < 1e-9, "column {i} moved: {y}");
        }
        // bpo 0 is the "off" designator's shape: identity, not a panic.
        let ragged: Vec<f64> = (0..n).map(|i| (i as f64).sin() * 6.0).collect();
        assert_eq!(smooth_db(&freqs, &ragged, &all_valid(n), 0), ragged);
    }

    #[test]
    fn smoothing_is_monotone_in_bpo() {
        // Wider windows (smaller bpo) must not produce a rougher curve.
        let n = 200;
        let freqs = grid(20.0, 48, n);
        let ragged: Vec<f64> = (0..n)
            .map(|i| if i % 2 == 0 { 6.0 } else { -6.0 })
            .collect();
        let roughness = |v: &[f64]| -> f64 { v.windows(2).map(|w| (w[1] - w[0]).abs()).sum() };
        let mut prev = roughness(&ragged);
        for &bpo in &[24u32, 12, 6, 3, 1] {
            let r = roughness(&smooth_db(&freqs, &ragged, &all_valid(n), bpo));
            assert!(
                r <= prev + 1e-9,
                "bpo {bpo} roughened the curve: {r} > {prev}"
            );
            prev = r;
        }
    }

    #[test]
    fn phase_average_does_not_cross_the_wrap() {
        // Two columns at +170° and −170°: the physical mean is 180°, and a
        // naive average is 0° — the wrong number by a half turn, and the
        // whole reason phase is unwrapped first.
        let freqs = vec![1000.0, 1000.0 * 2f64.powf(1.0 / 48.0)];
        let phase = vec![170.0, -170.0];
        let out = smooth_unwrapped_phase_deg(&freqs, &phase, &all_valid(2), 6);
        for (i, &y) in out.iter().enumerate() {
            // Not exactly 180: the window is Hann-weighted, so the two
            // columns do not contribute equally. The failure this pins is a
            // half-turn away — a wrapped average returns 0.
            assert!(
                (y - 180.0).abs() < 5.0,
                "column {i}: {y} (expected ≈180, unwrapped)"
            );
        }
    }

    #[test]
    fn phase_output_is_unwrapped_by_contract() {
        // A steady downward ramp through several wraps stays a ramp: the
        // caller wraps, this does not. If this returned wrapped values the
        // spread would collapse to under 360°.
        let n = 60;
        let freqs = grid(1000.0, 48, n);
        let wrapped: Vec<f64> = (0..n)
            .map(|i| {
                let raw = -20.0 * i as f64;
                let mut w = raw % 360.0;
                if w <= -180.0 {
                    w += 360.0;
                }
                w
            })
            .collect();
        let out = smooth_unwrapped_phase_deg(&freqs, &wrapped, &all_valid(n), 12);
        let spread = out.iter().cloned().fold(f64::NEG_INFINITY, f64::max)
            - out.iter().cloned().fold(f64::INFINITY, f64::min);
        assert!(spread > 360.0, "output looks wrapped: spread {spread}°");
    }

    #[test]
    fn masked_columns_neither_contribute_nor_change() {
        // One garbage column in the middle, masked. It must pass through
        // untouched, and the trusted columns beside it must be averaged as
        // if it were absent — not merely down-weighted.
        let n = 9;
        let freqs = grid(1000.0, 48, n);
        let mut v = vec![0.0; n];
        v[4] = -400.0;
        let mut valid = all_valid(n);
        valid[4] = false;

        let out = smooth_db(&freqs, &v, &valid, 6);
        assert_eq!(out[4], -400.0, "masked column was rewritten");
        for (i, &y) in out.iter().enumerate() {
            if i != 4 {
                assert!(
                    y.abs() < 1e-9,
                    "column {i} pulled by the masked column: {y}"
                );
            }
        }
    }

    #[test]
    fn unwrap_restarts_at_a_masked_gap() {
        // Left run ends mid-turn, right run starts on the other side of a
        // wrap. If the unwrap ran through the gap the right run would be
        // offset by 360°; restarting keeps each run in its own frame.
        let freqs = grid(1000.0, 48, 5);
        let phase = vec![170.0, 175.0, 0.0, -175.0, -170.0];
        let valid = vec![true, true, false, true, true];
        let out = smooth_unwrapped_phase_deg(&freqs, &phase, &valid, 6);
        assert!(out[0] > 0.0 && out[1] > 0.0, "left run moved: {out:?}");
        assert!(out[3] < 0.0 && out[4] < 0.0, "right run moved: {out:?}");
        assert_eq!(out[2], 0.0, "masked column was rewritten");
    }

    #[test]
    fn malformed_input_returns_the_input() {
        let freqs = grid(100.0, 48, 5);
        let short = vec![1.0, 2.0];
        assert_eq!(smooth_db(&freqs, &short, &all_valid(5), 6), short);
        assert_eq!(smooth_db(&freqs, &[1.0; 5], &all_valid(2), 6), vec![1.0; 5]);
        // Empty everything, and a non-positive centre frequency: no panic,
        // and the degenerate column passes through.
        assert!(smooth_db(&[], &[], &[], 6).is_empty());
        let out = smooth_db(&[0.0, 100.0], &[3.0, 4.0], &[true, true], 6);
        assert_eq!(out[0], 3.0);
    }
}
