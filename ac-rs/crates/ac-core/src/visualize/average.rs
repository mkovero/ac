//! Averaging stored transfer-function traces (#671) — Smaart v7's Trace
//! Average with the Coherence Weighted option (User Guide pp. 16–17, 33):
//! a spatial average over microphone positions.
//!
//! Per frequency point, over the traces:
//! - magnitude: mean of the dB values ("decibel averaging is used for
//!   magnitude data"), weighted by each trace's coherence there;
//! - phase: the angle of the weighted sum of unit phasors ("phase averaging
//!   is based on complex data") — a mean of angles that does not break at
//!   the ±180° wrap;
//! - coherence: the plain mean, which is what the averaged trace is masked
//!   by.
//!
//! With weighting off every trace weighs 1. With weighting on, weight is
//! coherence ("the coherence value of each frequency data point in each
//! trace as a weighting factor"); a point where every trace has coherence 0
//! has no weight at all, and is returned with coherence 0 so it is masked.

/// One trace's arrays, parallel to a shared frequency grid.
#[derive(Debug, Clone, Copy)]
pub struct TraceArrays<'a> {
    pub magnitude_db: &'a [f64],
    pub phase_deg: &'a [f64],
    pub coherence: &'a [f64],
}

/// The averaged trace, parallel to the same grid.
#[derive(Debug, Clone, PartialEq)]
pub struct Averaged {
    pub magnitude_db: Vec<f64>,
    pub phase_deg: Vec<f64>,
    pub coherence: Vec<f64>,
}

/// Average `traces` point by point (see the module doc). Every trace must
/// be the same length — the caller checks they share one grid.
pub fn average(traces: &[TraceArrays<'_>], coherence_weighted: bool) -> Averaged {
    let n = traces.first().map_or(0, |t| t.magnitude_db.len());
    let mut out = Averaged {
        magnitude_db: Vec::with_capacity(n),
        phase_deg: Vec::with_capacity(n),
        coherence: Vec::with_capacity(n),
    };
    for k in 0..n {
        let (mut w_sum, mut mag, mut re, mut im, mut coh) = (0.0, 0.0, 0.0, 0.0, 0.0);
        for t in traces {
            let c = t.coherence[k].clamp(0.0, 1.0);
            let w = if coherence_weighted { c } else { 1.0 };
            let phi = t.phase_deg[k].to_radians();
            w_sum += w;
            mag += w * t.magnitude_db[k];
            re += w * phi.cos();
            im += w * phi.sin();
            coh += c;
        }
        if w_sum > 0.0 {
            out.magnitude_db.push(mag / w_sum);
            out.phase_deg.push(im.atan2(re).to_degrees());
            out.coherence.push(coh / traces.len() as f64);
        } else {
            out.magnitude_db.push(f64::NAN);
            out.phase_deg.push(f64::NAN);
            out.coherence.push(0.0);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t<'a>(m: &'a [f64], p: &'a [f64], c: &'a [f64]) -> TraceArrays<'a> {
        TraceArrays {
            magnitude_db: m,
            phase_deg: p,
            coherence: c,
        }
    }

    /// Tested against the rejected implementation (the plain average,
    /// computed here): a position with poor coherence at one frequency —
    /// a reverberant build-up reading +12 dB — drags the plain average and
    /// barely moves the weighted one.
    #[test]
    fn a_low_coherence_outlier_moves_the_plain_average_not_the_weighted_one() {
        let good = [0.0, 0.0];
        let bad = [0.0, 12.0];
        let p = [0.0, 0.0];
        let c_good = [0.95, 0.95];
        let c_bad = [0.95, 0.05];
        let traces = [
            t(&good, &p, &c_good),
            t(&good, &p, &c_good),
            t(&bad, &p, &c_bad),
        ];
        let plain = average(&traces, false);
        let weighted = average(&traces, true);
        assert!(
            (plain.magnitude_db[1] - 4.0).abs() < 1e-9,
            "plain {}",
            plain.magnitude_db[1]
        );
        assert!(
            weighted.magnitude_db[1] < 0.35,
            "weighted {}",
            weighted.magnitude_db[1]
        );
        // Where all agree, both are exact.
        assert_eq!(plain.magnitude_db[0], 0.0);
        assert_eq!(weighted.magnitude_db[0], 0.0);
    }

    /// Phase averages across the wrap: +170° and −170° average to 180°,
    /// not 0° as a mean of the numbers would.
    #[test]
    fn phase_averages_across_the_wrap() {
        let m = [0.0];
        let c = [1.0];
        let a = average(&[t(&m, &[170.0], &c), t(&m, &[-170.0], &c)], true);
        assert!(
            (a.phase_deg[0].abs() - 180.0).abs() < 1e-9,
            "{}",
            a.phase_deg[0]
        );
    }

    /// A point with no coherence anywhere has no weight: masked, not a
    /// division by zero.
    #[test]
    fn a_point_with_no_coherence_anywhere_is_masked() {
        let a = average(
            &[t(&[3.0], &[0.0], &[0.0]), t(&[5.0], &[0.0], &[0.0])],
            true,
        );
        assert_eq!(a.coherence[0], 0.0);
        assert!(a.magnitude_db[0].is_nan());
        // Unweighted, the same point still averages.
        let a = average(
            &[t(&[3.0], &[0.0], &[0.0]), t(&[5.0], &[0.0], &[0.0])],
            false,
        );
        assert_eq!(a.magnitude_db[0], 4.0);
    }
}
