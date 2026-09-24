//! Derived read-out quantities for an impulse-response payload, and the
//! verdict on whether its peak is a trustworthy deconvolution result
//! (#376). Computed once here so `ac-cli`'s text read-out and
//! `ac-scene`'s sweep-IR panel cannot disagree about a capture.

use super::{
    GateParams, InterPairOffset, InterfaceLatency, MeasurementData, MeasurementReport,
    ReferenceLatency,
};
use crate::measurement::sweep::{
    band_limit_available, band_limit_top_hz, ir_default_window_len, ir_peak, lobe_window_samples,
    pre_impulse_snr_db, pre_impulse_snr_db_before, second_lobe, zero_phase_high_pass, BoundInputs,
    CausalBound, EdgeGuard, MissingBoundInput, OnsetEstimate, OnsetPick, WindowLimit,
    ARRIVAL_HIGH_PASS_CORNER_HZ, BAND_LIMIT_MIN_F2_RATIO, IR_DEFAULT_DURATION_S, IR_DEFAULT_F1_HZ,
    IR_DEFAULT_F2_HZ,
};
use crate::shared::calibration::{
    compare_tau_readings, EnumerationCheck, TauComparison, TauDisagreement,
};

/// Minimum pre-impulse SNR, in dB, below which a deconvolution is
/// reported as failed rather than as a result (#376). Below this floor
/// the linear-IR peak is not reliably the system response — it can land
/// wherever the pre-impulse noise floor happens to be largest, producing
/// a plausible-looking arrival/distance from noise.
///
/// Value: 18.0 dB — the worst observed *bad* capture in the rig table in
/// the #376 issue body (−42 dBFS drive, pre-impulse SNR up to 16.5 dB with
/// a peak index far from the true arrival) plus a 1.5 dB margin. The raw
/// log and results doc the issue body itself cites for that table
/// (`audit/rig-353-2026-08-23/ladder-3m.log`,
/// `work/rig/rig-2026-08-23-onset-353-results.md`) never landed in this
/// repo — the table reproduced in the issue is the only source checked
/// here; do not add a citation to either path without confirming the file
/// exists first. The same table's worst observed *good* capture (−36 dBFS
/// drive) reaches down to 14.5 dB, so
/// no single threshold separates this dataset cleanly — 18.0 dB is set
/// at or above the worst bad case rather than at the overlap's midpoint,
/// so a false refusal (cheap: re-run) is preferred over a false accept
/// (expensive: a silently wrong logged distance). This also means some
/// borderline-good low-drive captures near the boundary will be
/// refused — low drive is the operator-encouraged *safe* choice under
/// the rig's emission consent rules, so that blind spot is real and
/// documented here rather than picked by eye.
///
/// **Scored for the default sweep (#501).** The figure a clean loopback
/// reads is set by the stimulus, not by the capture's noise (#471), so
/// this value means something only for the stimulus it was checked
/// against. It was checked against the `plot_ir` defaults in
/// [`crate::measurement::sweep::IR_DEFAULT_DURATION_S`] and its siblings —
/// 20 Hz–20 kHz, 4.0 s, a 0.4 s window, 5 harmonics, 0.5 s tail — on a
/// synthetic chain that mirrors `plot_ir` (`log_sweep` → delay → tail →
/// `deconvolve_full` → `extract_irs` → `ir_peak` → `pre_impulse_snr_db`):
/// - a perfect loopback reads 21.1–22.0 dB for τ from 0 to 40 ms, the same
///   to 0.01 dB at 44.1, 48, 96 and 192 kHz, and unmoved by added noise
///   at −110 or −70 dBFS;
/// - a noise-only capture (no signal path) read at most 16.3 dB over
///   about 200 draws at 48 and 96 kHz (median 10.1 dB, 95th percentile
///   12.7 dB); none reached this value.
///
/// Rig anchor: under the previous defaults (1 s, 4096 samples) pupu's
/// 96 kHz electrical loopback read 12.8 dB (2026-09-16, #501), against
/// 13.2 dB from the same synthetic chain at the rig's τ — a perfect cable
/// could not clear this value there, which is what #501 fixed by changing
/// the defaults rather than this number. A derived threshold (#471's
/// floor − 3 dB) was rejected at those defaults because it accepted a
/// noise-only capture in 15 of 200 draws; the fixed value accepted none.
///
/// **Tail domain (#550).** The tail is not among the parameters the
/// scoring depends on, as long as it is at least half the window.
/// `deconvolve_full` is a full linear convolution and `extract_irs` cuts
/// the linear IR at `full[N − 1 − ⌊W/2⌋ ..][..W]` (N the sweep length, W the
/// window), and `full[m]` reads only capture samples `0..=m`. Every capture
/// of at least `N + ⌈W/2⌉` samples therefore gives the same linear IR, up
/// to FFT rounding, and the figure is a function of the linear IR alone.
/// The 0.5 s tail #501 scored against a 0.4 s window stands for every tail
/// ≥ W/2. A tail below W/2 truncates the late half of the IR, which is
/// #576, not a scoring question. Harmonic count reaches the linear IR only
/// through the window clamp, which shows as the window length.
///
/// A typed configuration (another band, length or window) is still judged
/// against this value, which nobody has derived for it (#474).
/// [`pre_impulse_snr_scope`] compares exactly those three against the
/// defaults and names the ones that differ on every read-out.
///
/// **Region (#550).** When the band-limited arrival (#537) is trusted —
/// its SNR measured over a non-empty floor before it, and its standing
/// not [`ArrivalCrossCheck::BandLimitedSnrLow`] (`floor_anchor` holds the
/// rule) — the floor ends one guard band before the *earlier* of that
/// arrival and the broadband peak ([`IrStats::pre_impulse_floor_anchor`]).
/// Otherwise it ends before the broadband peak, the region #501 scored: on
/// a noise-only capture the high-passed argmax is not a response, and
/// anchoring on it accepted 9 of 400 noise-only draws. On pupu's
/// default-band captures at 1.00 m and −50 dBFS (`d1p0-onaxis`, 2026-09-21)
/// the broadband argmax was a low-frequency room mode ≈ 2300 samples after
/// the arrival, so a floor ending before it held the direct sound and the
/// early field — measured, per capture, before-argmax vs before-arrival:
///
/// | capture | arrival | argmax | before argmax | before arrival |
/// |---|---|---|---|---|
/// | 01 | 21246 | 23545 | 17.4 dB | 25.1 dB |
/// | 02 | 21246 | 23547 | 19.1 dB | 25.3 dB |
/// | 03 | 21245 | 23545 | 16.1 dB | 21.6 dB |
/// | 04 | 21245 | 23545 | 17.4 dB | 22.2 dB |
/// | 05 | 21246 | 23546 | 17.1 dB | 21.5 dB |
///
/// Where arrival and peak are the same sample (a clean loopback, or no
/// band-limited arrival) the region is the one #501 scored, unchanged. The
/// replay is pinned in `pre_impulse_region_replay.rs`.
pub const PRE_IMPULSE_SNR_MIN_DB: f64 = 18.0;

/// The places to check under a failed deconvolution, first line: the
/// sweep rows, the measured lever first (#550 UX — on pupu the band start
/// moved the figure by 18 dB, the length by 0.5 dB). `ac-cli` prints it
/// and [`PRE_IMPULSE_SNR_CHECKS_CHAIN`] as two `check:` lines; `ac-scene`'s
/// fault detail joins them with `, `.
pub const PRE_IMPULSE_SNR_CHECKS_SWEEP: &str = "sweep band start, length, window";

/// The places to check under a failed deconvolution, second line: the
/// capture chain. See [`PRE_IMPULSE_SNR_CHECKS_SWEEP`].
pub const PRE_IMPULSE_SNR_CHECKS_CHAIN: &str = "drive level, input gain, distance, room noise";

/// A sweep parameter [`PRE_IMPULSE_SNR_MIN_DB`] was scored for (#550), in
/// the order `ac plot ir`'s `IR sweep` block prints them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScoredSweepParam {
    /// f1 and f2 against `IR_DEFAULT_F1_HZ` / `IR_DEFAULT_F2_HZ`.
    Band,
    /// The sweep duration against `IR_DEFAULT_DURATION_S`.
    Length,
    /// `linear_ir.len()` against `ir_default_window_len(sample_rate_hz)`.
    Window,
}

impl ScoredSweepParam {
    /// Every scoped parameter, in print order.
    pub const ALL: [ScoredSweepParam; 3] = [Self::Band, Self::Length, Self::Window];

    /// The row name the parameter prints under.
    pub fn name(self) -> &'static str {
        match self {
            Self::Band => "band",
            Self::Length => "length",
            Self::Window => "window",
        }
    }
}

/// Whether [`PRE_IMPULSE_SNR_MIN_DB`] was scored for a report's sweep
/// (#550): see [`pre_impulse_snr_scope`]. Its `Display` is the scope line
/// `ac-cli` and `ac-scene` print verbatim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreImpulseSnrScope {
    /// Band, length and window equal the scored defaults.
    Scored,
    /// The parameters that differ, in [`ScoredSweepParam::ALL`] order;
    /// never empty.
    Unscored(Vec<ScoredSweepParam>),
}

impl std::fmt::Display for PreImpulseSnrScope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (lead, params): (&str, &[ScoredSweepParam]) = match self {
            Self::Scored => ("scored", &ScoredSweepParam::ALL),
            Self::Unscored(params) => ("unscored", params),
        };
        let names: Vec<&str> = params.iter().map(|p| p.name()).collect();
        write!(f, "{lead} for this sweep's {}", names.join(", "))
    }
}

/// The scope of [`PRE_IMPULSE_SNR_MIN_DB`] for `report`'s first
/// `ImpulseResponse` payload (#550): scored when its band, length and
/// window equal the `plot_ir` defaults it was scored against, otherwise
/// the ones that differ. Compared exactly — defaults reach the report as
/// the same constants. Takes no tail and no harmonic count: the linear IR
/// does not depend on either inside the scored domain (see
/// [`PRE_IMPULSE_SNR_MIN_DB`]'s tail paragraph). `None` without an
/// impulse-response payload.
pub fn pre_impulse_snr_scope(report: &MeasurementReport) -> Option<PreImpulseSnrScope> {
    let (sample_rate_hz, f1_hz, f2_hz, duration_s, window_len) =
        report.data.iter().find_map(|p| match &p.data {
            MeasurementData::ImpulseResponse {
                sample_rate_hz,
                f1_hz,
                f2_hz,
                duration_s,
                linear_ir,
                ..
            } => Some((
                *sample_rate_hz,
                *f1_hz,
                *f2_hz,
                *duration_s,
                linear_ir.len(),
            )),
            _ => None,
        })?;
    let differs = |param: &ScoredSweepParam| match param {
        ScoredSweepParam::Band => f1_hz != IR_DEFAULT_F1_HZ || f2_hz != IR_DEFAULT_F2_HZ,
        ScoredSweepParam::Length => duration_s != IR_DEFAULT_DURATION_S,
        ScoredSweepParam::Window => window_len != ir_default_window_len(sample_rate_hz),
    };
    let unscored: Vec<ScoredSweepParam> =
        ScoredSweepParam::ALL.into_iter().filter(differs).collect();
    Some(if unscored.is_empty() {
        PreImpulseSnrScope::Scored
    } else {
        PreImpulseSnrScope::Unscored(unscored)
    })
}

/// Minimum pre-impulse SNR of the high-passed IR, in dB, below which the
/// band-limited arrival withholds the flight time (#537,
/// [`ArrivalCrossCheck::BandLimitedSnrLow`]). Separate from
/// [`PRE_IMPULSE_SNR_MIN_DB`], which gates the broadband deconvolution and
/// keeps its meaning.
///
/// Provenance (#537 architect revision 3): derived — ISO 3382-1:2009
/// §A.3.4's trigger level, 20 dB, plus the peak-over-RMS of band-limited
/// background across the default 0.4 s gate, about 12.5 dB. The clause
/// reads the start of a filtered IR as the point where it "first rises
/// significantly above the background but is more than 20 dB below the
/// maximum"; for that trigger to sit above the background's *peaks*, not
/// only its RMS, the SNR must clear 20 dB by the noise crest. Also measured
/// on synthesis: the prototype suite refused every capture on noise at
/// 30 dB and none at 35 dB. Rig headroom: 50.8–53.9 dB at 2 kHz on pupu.
///
/// **Coupled** to [`ARRIVAL_EARLIER_COMPARABLE_DB`] and to the gate length:
/// below this value, background peaks reach the earlier-comparable level
/// and that guard fires on noise, naming the wrong reason. The
/// coupled-constants test in `arrival_suite.rs` fails if either moves
/// alone. A longer typed gate raises the noise crest, which errs toward
/// refusal.
///
/// **Also decides the #376 verdict's floor (#550).** Only an arrival that
/// cleared this value may end [`PRE_IMPULSE_SNR_MIN_DB`]'s floor before
/// the broadband peak. An unmeasured SNR — a pick inside the guard band,
/// whose `+inf` has no floor under it — does not count as clearing it for
/// that floor, whatever #577 decides for the standing. The noise crest of the high-passed IR over the
/// default window is about 13 dB (√(2 ln W), derived; 11.5–12.9 dB
/// measured on noise-only draws). Lowering this toward it lets a noise
/// argmax cut the floor short again, which accepted 9 of 400 noise-only
/// draws before #550's revision 3. The negative control
/// `no_signal_is_refused_before_and_after_the_floor_anchor_moves` fails if
/// a noise-only draw reaches this value.
///
/// Was 20.0 dB (revision 2): §A.3.4's trigger level taken as the gate on
/// its own. Falsified by the prototype suite once the earlier-comparable
/// guard moved to 20 dB: at an arrival SNR of 30 dB every capture was
/// refused as an earlier arrival rather than on its SNR.
pub const ARRIVAL_SNR_MIN_DB: f64 = 35.0;

/// What [`ARRIVAL_SNR_MIN_DB`] rests on, as printed under the arrival SNR.
pub const ARRIVAL_SNR_BASIS: &str =
    "ISO 3382-1:2009 \u{a7}A.3.4 trigger (\u{2212}20 dB) above noise peaks";

/// How far the broadband peak compared against ([`ArrivalCrossCheck`]'s
/// `r`) may sit from the band-limited arrival, either way, before the
/// cross-check names the disagreement (#537), in seconds.
///
/// Provenance: measured (#537 architect revision 2). On the 14 pupu
/// captures recorded at PR #538's first head (Genelec 1083 at 2 m and
/// 0.5 m, a 1–10 kHz sweep, and the cable), healthy `r − arrival` ran
/// −8 … +143 samples at 96 kHz (≤ 1.49 ms); #537's room mode put the
/// broadband maximum 15–16 ms after the arrival. Scored on one speaker;
/// [`ARRIVAL_CROSS_CHECK_BASIS`] says so.
pub const ARRIVAL_CROSS_CHECK_TOLERANCE_S: f64 = 0.002;

/// What [`ARRIVAL_CROSS_CHECK_TOLERANCE_S`] rests on, as printed under the
/// broadband Δ.
pub const ARRIVAL_CROSS_CHECK_BASIS: &str = "rig-scored on 1 speaker";

/// An earlier high-passed sample within this many dB of the band-limited
/// maximum makes the arrival [`ArrivalCrossCheck::EarlierComparable`]
/// (#537): the pick may be a later, stronger path than the first one.
///
/// Provenance (#537 architect revision 4): assumed, the level borrowed from
/// ISO 3382-1:2009 §A.3.4 ("more than 20 dB below the maximum"). This guard
/// does not implement the clause and is not derived from it. The clause
/// takes the start at the first point significantly above the background,
/// with the 20 dB as an upper limit on that trigger, so it would take an
/// earlier path at −25 or −30 dB that stands above the background. This
/// guard stops at −20 dB: an earlier path further below the maximum passes,
/// and the pick lands on the later one. That gap is the residual the
/// operator accepted on 2026-09-19, late only and bounded by the separation
/// D with or without a distance (#552: a distance has no late edge, so it
/// shows the error as an excess over `d / c` but does not tighten it; see
/// `ac-rs/ZMQ.md` §"Band-limited arrival (#537)"). The falsification suite shows 20 dB holds inside its stated
/// region; no measurement makes it the right edge.
///
/// Measured for headroom: on the 17 pupu captures of 2026-09-18 the
/// largest high-passed sample more than one
/// corner period before the arrival sat at −33.6 dB (speaker) and
/// −26.1 dB (cable), 13.6 and 6.1 dB clear of it. Coupled to
/// [`ARRIVAL_SNR_MIN_DB`].
///
/// Was 6.0 dB (revision 2), measured as the same headroom with a margin.
/// Falsified by the prototype falsification suite: with a later copy
/// stronger than the first path and separated by at least 0.5 ms, revision 2
/// produced a wrong flight time in 502 of 720 cases, usually as a plain
/// `Agrees`; at 20 dB, 71.
pub const ARRIVAL_EARLIER_COMPARABLE_DB: f64 = 20.0;

/// Hand-tape tolerance on a typed `position.distance_m`, in metres: part of
/// the ε that places [`DistanceCheck`]'s one edge, the earliest arrival
/// `d / c − ε`. There is no edge above `d / c` (#552).
///
/// Provenance: measured — hand-tape repeatability on the rig, ±5 cm
/// (#537 architect revision 3).
pub const DISTANCE_TAPE_TOLERANCE_M: f64 = 0.05;

/// Relative uncertainty of the speed of sound [`DistanceCheck`] divides by,
/// as a fraction.
///
/// Provenance: derived — c at 20 ± 10 °C varies by 0.606·10 / 343 ≈ 1.8 %,
/// rounded up (#537 architect revision 3). Covers an unset temperature.
pub const DISTANCE_SPEED_OF_SOUND_REL_TOL: f64 = 0.02;

/// A second local maximum of the high-passed IR within one corner period of
/// the pick and less than this many dB below it makes the arrival
/// [`ArrivalCrossCheck::ArrivalAmbiguous`] (#537 architect revision 2): the
/// pick may be one half-cycle off its pulse's first lobe.
///
/// Provenance: measured. Healthy picks on the pupu captures (2026-09-18,
/// Genelec 1083, 2 kHz corner) cleared 4.9–6.5 dB (9.4 dB on the cable);
/// picks that hopped a half-cycle — simulated from the same IRs, and
/// validated against the one real 20 Hz–2 kHz sweep (+668 against +669) —
/// stayed at or under 1.1 dB. Run-to-run spread was ≤ 0.8 dB.
pub const ARRIVAL_LOBE_MARGIN_MIN_DB: f64 = 3.0;

/// How far below the broadband maximum a peak may sit and still be the one
/// [`ArrivalCrossCheck`] compares the arrival against (`r`, #537 architect
/// revision 2): the earliest local maximum of `|h|` at or after
/// `arrival − tolerance` within this many dB of the maximum.
///
/// Provenance: measured. On the pupu captures the broadband argmax flipped
/// between near-equal later lobes from run to run (0.0–1.7 dB apart); a
/// 3 dB cut still flipped on 2 of 5 runs at 2 m, 6 dB held on all 14, and
/// the weakest healthy lead lobe sat at −4.4 dB. Coupled to
/// [`ARRIVAL_CROSS_CHECK_TOLERANCE_S`]: at the 18 dB broadband SNR gate,
/// pre-arrival noise inside the tolerance peaks near −9 dB, below this.
/// Since #550 the gate's floor is the pre-arrival one when the arrival is
/// trusted, so that is the
/// quantity gated; `pre_impulse_region_replay.rs` asserts the margin on the
/// `d1p0-onaxis` captures.
pub const ARRIVAL_BROADBAND_COMPARABLE_DB: f64 = 6.0;

/// [`ARRIVAL_CROSS_CHECK_TOLERANCE_S`] at `sample_rate_hz`, rounded to whole
/// samples — the bound the cross-check compares against.
pub fn arrival_cross_check_tolerance_samples(sample_rate_hz: u32) -> i64 {
    (ARRIVAL_CROSS_CHECK_TOLERANCE_S * sample_rate_hz as f64).round() as i64
}

/// The arrival a linear IR yields under #537's rule, with its cross-check
/// against the broadband IR.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct BandLimitedArrival {
    pub(crate) arrival_index: usize,
    pub(crate) source: ArrivalSource,
    pub(crate) band_limited_snr_db: Option<f64>,
    /// The pick's lobe margin; see [`IrStats::arrival_lobe_margin_db`].
    pub(crate) lobe_margin_db: Option<f64>,
    /// See [`IrStats::arrival_lobe_offset`].
    pub(crate) lobe_offset: Option<i64>,
    /// See [`IrStats::broadband_delta_level_db`].
    pub(crate) broadband_delta_level_db: Option<f64>,
    pub(crate) cross_check: ArrivalCrossCheck,
}

/// `r`: the peak of the broadband IR `linear_ir` the arrival is compared
/// against (#537 architect revision 2) — among the local maxima of `|h|` at
/// index ≥ `from`, plus the argmax `peak_index`, the earliest within
/// [`ARRIVAL_BROADBAND_COMPARABLE_DB`] of the maximum. Returns its index
/// and its level re the maximum, in dB (0 when it is the argmax).
fn comparable_broadband_peak(linear_ir: &[f64], from: usize, peak_index: usize) -> (usize, f64) {
    let max = linear_ir[peak_index].abs();
    let cut = max * 10f64.powf(-ARRIVAL_BROADBAND_COMPARABLE_DB / 20.0);
    let n = linear_ir.len();
    let r = (from.max(1)..n.saturating_sub(1))
        .take_while(|&i| i < peak_index)
        .find(|&i| {
            let (l, c, rr) = (
                linear_ir[i - 1].abs(),
                linear_ir[i].abs(),
                linear_ir[i + 1].abs(),
            );
            c > l && c >= rr && c >= cut
        })
        .unwrap_or(peak_index);
    let level_db = if r == peak_index || max == 0.0 {
        0.0
    } else {
        20.0 * (linear_ir[r].abs() / max).log10()
    };
    (r, level_db)
}

/// The two thresholds of [`band_limited_arrival`] that #537 architect
/// revision 3 re-scored. One value ships ([`ArrivalRule::SHIPPED`]); the
/// falsification suite runs the rejected revision-2 values through the same
/// code, so it measures the rule it rejects rather than a copy of it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct ArrivalRule {
    /// See [`ARRIVAL_SNR_MIN_DB`].
    pub(crate) snr_min_db: f64,
    /// See [`ARRIVAL_EARLIER_COMPARABLE_DB`].
    pub(crate) earlier_comparable_db: f64,
}

impl ArrivalRule {
    /// The shipped constants.
    pub(crate) const SHIPPED: Self = Self {
        snr_min_db: ARRIVAL_SNR_MIN_DB,
        earlier_comparable_db: ARRIVAL_EARLIER_COMPARABLE_DB,
    };
}

/// Pick the arrival of `linear_ir` off the IR high-passed at
/// [`ARRIVAL_HIGH_PASS_CORNER_HZ`] and cross-check it against the broadband
/// IR, whose argmax is `peak_index` (#537). The standings are checked in
/// [`ArrivalCrossCheck`]'s order; the first that fires is the result.
///
/// When the stimulus does not reach [`BAND_LIMIT_MIN_F2_RATIO`] times the
/// corner, the arrival is the broadband peak ([`ArrivalSource::Peak`]) and
/// the standing is [`ArrivalCrossCheck::BandLimitUnavailable`].
pub(crate) fn band_limited_arrival(
    linear_ir: &[f64],
    sample_rate_hz: u32,
    f2_hz: f64,
    peak_index: usize,
) -> BandLimitedArrival {
    band_limited_arrival_under(
        linear_ir,
        sample_rate_hz,
        f2_hz,
        peak_index,
        ArrivalRule::SHIPPED,
    )
}

/// [`band_limited_arrival`] under `rule`'s thresholds.
pub(crate) fn band_limited_arrival_under(
    linear_ir: &[f64],
    sample_rate_hz: u32,
    f2_hz: f64,
    peak_index: usize,
    rule: ArrivalRule,
) -> BandLimitedArrival {
    let corner_hz = ARRIVAL_HIGH_PASS_CORNER_HZ;
    if !band_limit_available(sample_rate_hz, f2_hz, corner_hz) {
        return BandLimitedArrival {
            arrival_index: peak_index,
            source: ArrivalSource::Peak,
            band_limited_snr_db: None,
            lobe_margin_db: None,
            lobe_offset: None,
            broadband_delta_level_db: None,
            cross_check: ArrivalCrossCheck::BandLimitUnavailable {
                band_top_hz: band_limit_top_hz(sample_rate_hz, f2_hz),
                required_hz: BAND_LIMIT_MIN_F2_RATIO * corner_hz,
            },
        };
    }
    let h_hp = zero_phase_high_pass(linear_ir, sample_rate_hz, corner_hz);
    let (arrival_index, arrival_magnitude) = ir_peak(&h_hp);
    let snr_db = pre_impulse_snr_db(&h_hp, arrival_index);
    let tolerance = arrival_cross_check_tolerance_samples(sample_rate_hz);
    let lobe_window = lobe_window_samples(sample_rate_hz, corner_hz);

    // Row 3: the pick against its neighbouring half-cycles, ±1 corner
    // period. No other maximum there leaves the margin unbounded.
    let lobe = second_lobe(&h_hp, arrival_index, lobe_window);
    let lobe_margin_db = lobe.map_or(f64::INFINITY, |l| l.margin_db);

    // Row 4: everything before `arrival − 1 corner period`, which row 3
    // does not cover. Not the SNR's pre-impulse region: its guard band
    // (`len / 32`, 12.5 ms on the default 0.4 s window) is wider than a
    // direct-to-reflection gap this check exists to see.
    let earlier_end = arrival_index.saturating_sub(lobe_window);
    let earlier = ir_peak(&h_hp[..earlier_end]);
    let earlier_level_db = if earlier_end > 0 && arrival_magnitude > 0.0 && earlier.1 > 0.0 {
        Some(20.0 * (earlier.1 / arrival_magnitude).log10())
    } else {
        None
    };

    let argmax_gap = peak_index as i64 - arrival_index as i64;
    let mut broadband_delta_level_db = None;
    // A NaN SNR (a non-finite IR) withholds like a low one.
    let cross_check = if snr_db.is_nan() || snr_db < rule.snr_min_db {
        ArrivalCrossCheck::BandLimitedSnrLow { snr_db }
    } else if let Some(l) = lobe.filter(|l| l.margin_db < ARRIVAL_LOBE_MARGIN_MIN_DB) {
        ArrivalCrossCheck::ArrivalAmbiguous {
            margin_db: l.margin_db,
            offset: l.offset,
        }
    } else if let Some(level_db) = earlier_level_db.filter(|l| *l >= -rule.earlier_comparable_db) {
        ArrivalCrossCheck::EarlierComparable {
            index: earlier.0,
            level_db,
        }
    } else if argmax_gap < -tolerance {
        ArrivalCrossCheck::BroadbandEarlier { gap: argmax_gap }
    } else {
        // The argmax is at or after `arrival − tolerance` here, so `r`
        // exists and is no later than it.
        let from = arrival_index.saturating_sub(tolerance as usize);
        let (r, level_db) = comparable_broadband_peak(linear_ir, from, peak_index);
        broadband_delta_level_db = Some(level_db);
        let gap = r as i64 - arrival_index as i64;
        if gap > tolerance {
            ArrivalCrossCheck::BroadbandLater { gap }
        } else {
            ArrivalCrossCheck::Agrees { gap }
        }
    };
    BandLimitedArrival {
        arrival_index,
        source: ArrivalSource::BandLimitedPeak { corner_hz },
        band_limited_snr_db: Some(snr_db),
        lobe_margin_db: Some(lobe_margin_db),
        lobe_offset: lobe.map(|l| l.offset),
        broadband_delta_level_db,
        cross_check,
    }
}

impl MeasurementReport {
    /// Derived read-out quantities for the report's first
    /// `ImpulseResponse` payload: arrival timing, peak magnitude,
    /// pre-impulse SNR, and the time gate's low-frequency limit. `None`
    /// when no payload carries an impulse response, or its linear IR is
    /// empty (see issue #283).
    ///
    /// Arrival (`delay_samples`/`arrival_s`) is the magnitude peak of the IR
    /// high-passed at [`ARRIVAL_HIGH_PASS_CORNER_HZ`] with zero phase
    /// ([`ArrivalSource::BandLimitedPeak`], #537, operator ruling 2026-09-18),
    /// guarded by its lobe margin and cross-checked against the broadband IR
    /// in [`IrStats::arrival_cross_check`]. When the stimulus does not reach
    /// an octave above the corner it is the broadband peak, with the flight
    /// time withheld
    /// ([`ArrivalSource::Peak`]). The onset estimate
    /// ([`crate::measurement::sweep::estimate_onset`]) is anchored on that
    /// arrival and carried beside it as a diagnostic, with its standing in
    /// [`IrStats::onset_standing`]; it does not affect any number (#346
    /// architect revision 4, operator decision 2026-09-16).
    /// When this report carries both a measured same-capture reference
    /// latency and a recorded `position.distance_m`, the onset estimate is
    /// bound to reject any candidate earlier than pure flight time allows.
    pub fn ir_stats(&self) -> Option<IrStats> {
        let (payload, sample_rate_hz, f2_hz, linear_ir) =
            self.data.iter().find_map(|p| match &p.data {
                MeasurementData::ImpulseResponse {
                    sample_rate_hz,
                    f2_hz,
                    linear_ir,
                    ..
                } => Some((p, sample_rate_hz, *f2_hz, linear_ir)),
                _ => None,
            })?;
        if linear_ir.is_empty() || *sample_rate_hz == 0 {
            return None;
        }
        let window_len = linear_ir.len();
        let (peak_index, peak_magnitude) = ir_peak(linear_ir);

        // `extract_irs` (`measurement::sweep`) centres the gate at the
        // sweep endpoint — the position an identity (zero-delay) system
        // would peak at.
        let centre = window_len / 2;

        // #537: the arrival comes from the zero-phase high-passed IR and is
        // cross-checked against the broadband peak above.
        let BandLimitedArrival {
            arrival_index,
            source: arrival_source,
            band_limited_snr_db,
            lobe_margin_db: arrival_lobe_margin_db,
            lobe_offset: arrival_lobe_offset,
            broadband_delta_level_db,
            cross_check: arrival_cross_check,
        } = band_limited_arrival(linear_ir, *sample_rate_hz, f2_hz, peak_index);

        // The broadband floor before the arrival: the onset picker's floor
        // below, and the measured condition of the verdict's floor anchor.
        let arrival_region = pre_impulse_region(linear_ir, arrival_index);

        // #550: the verdict's floor ends before the first *trusted*
        // response, not before the broadband peak alone, which can sit a
        // room mode after the arrival and put the direct sound into the
        // floor. See [`floor_anchor`] for the trust rule. The numerator
        // stays the broadband peak. Same formula `ac-daemon`'s τ gate calls
        // (#368), with the anchor passed separately.
        let (floor_anchor_index, pre_impulse_floor_anchor) = floor_anchor(
            arrival_index,
            peak_index,
            &arrival_source,
            &arrival_cross_check,
            arrival_region.len(),
        );
        let pre_region = pre_impulse_region(linear_ir, floor_anchor_index);
        let pre_impulse_floor_end = pre_region.len();
        let pre_impulse_snr_db =
            pre_impulse_snr_db_before(linear_ir, peak_index, floor_anchor_index);

        // The onset picker's validity gate runs off a *median* floor, not
        // off `pre_impulse_snr_db`'s RMS one — see [`onset_floor`] for why
        // the two coexist rather than one replacing the other. #537: the
        // floor is taken from the broadband IR before the arrival the onset
        // is searched ahead of, not before the broadband peak, which can sit
        // a room mode later.
        let onset_floor = onset_floor(arrival_region);

        // The earliest sample the capture's own geometry admits as an onset
        // (#460): pure flight time from the same-capture reference latency
        // and the operator-entered distance, as a sample index. Never from
        // the stored `interface_latency`: it re-picks by a multiple of the
        // FireWire SYT interval on every device enumeration (#461), which
        // exceeds the bound's margin, and it is resolved from calibration
        // rather than measured with this IR.
        let causal_bound = causal_bound(self, centre, *sample_rate_hz);

        // #359: compare this capture's same-capture reference τ against
        // what `calibrate` has on file for that pair. Since #544 this is the
        // drift readout — how far the reference moved since it was stored —
        // and it withholds nothing: the live reading is what is subtracted.
        let arrival_check = arrival_check(self, *sample_rate_hz);

        let onset = crate::measurement::sweep::estimate_onset(
            linear_ir,
            arrival_index,
            *sample_rate_hz,
            onset_floor,
            &causal_bound,
        );
        let verdict = ir_verdict(peak_magnitude, pre_region, pre_impulse_snr_db);
        let onset_standing = onset_standing(&causal_bound, &onset, &verdict);
        let onset_index = onset.index;
        let onset_rule = onset.rule;

        // The arrival is a peak — band-limited when the band allows (#537),
        // never the onset: the bounded onset's pre-registered rig check
        // refused to conclude (pupu, 2026-09-16, 0/12 captures passed the
        // edge guard), and a peak is what pairs with a peak-picked τ (#351).
        let delay_samples = arrival_index as i64 - centre as i64;
        let arrival_s = delay_samples as f64 / *sample_rate_hz as f64;
        let (gate_window_s, gate_f_low_hz, gate_window_kind) =
            resolve_gate(payload.gate.as_ref(), window_len, *sample_rate_hz);

        // #544: the latency the flight time subtracts is this capture's own
        // reference leg plus the stored inter-pair offset — never the stored
        // absolute τ in `interface_latency`, in any branch. `arrival_check`
        // (live vs stored reference) is the drift readout and gates nothing,
        // and a refused `interface_latency.session_check` no longer withholds
        // anything, since the value it judged is not subtracted.
        //
        // #537: an arrival its cross-check disputes is never subtracted
        // from — the pick may not be the first path's delay.
        //
        // #537 architect revision 3: nor is one that falls below the
        // earliest arrival a typed distance allows (#552: no late edge). The check scores the live-basis
        // subtraction whether or not another layer withholds it, so a
        // read-out can name every reason a flight time is missing.
        let latency_basis = latency_basis(self);
        let distance_check = distance_check(self, arrival_s, &latency_basis);
        let flight_time_s = match &latency_basis {
            _ if arrival_cross_check.withholds_flight_time() => None,
            _ if distance_check.withholds_flight_time() => None,
            LatencyBasis::Live { .. } => latency_basis.latency_s().map(|l| arrival_s - l),
            LatencyBasis::Withheld(_) => None,
        };

        Some(IrStats {
            sample_rate_hz: *sample_rate_hz,
            window_len,
            peak_index,
            peak_magnitude,
            onset_index,
            onset_rule,
            causal_bound,
            arrival_source,
            arrival_index,
            band_limited_snr_db,
            arrival_lobe_margin_db,
            arrival_lobe_offset,
            broadband_delta_level_db,
            arrival_cross_check,
            onset_standing,
            delay_samples,
            arrival_s,
            arrival_check,
            latency_basis,
            flight_time_s,
            distance_check,
            pre_impulse_snr_db,
            pre_impulse_floor_anchor,
            pre_impulse_floor_end,
            gate_window_s,
            gate_f_low_hz,
            gate_window_kind,
            verdict,
        })
    }
}

/// The onset diagnostic's standing (#346 architect revision 4): a verdict
/// on the onset pick, not a gate on the arrival, which is always the peak.
/// The conditions are checked in this order; the first that fails is the
/// named standing, and [`OnsetStanding::Unscored`] means all held:
///
/// 1. the causal bound is enforced;
/// 2. the bound set the search window's start (it is not earlier than the
///    search span);
/// 3. the picker did not decline;
/// 4. the pick is not on the window start;
/// 5. the edge-following guard passed;
/// 6. the deconvolution verdict is not `Failed`.
///
/// Reads only values `ir_stats` already computed, never `onset.rule`.
pub(super) fn onset_standing(
    causal_bound: &CausalBound,
    onset: &OnsetEstimate,
    verdict: &IrVerdict,
) -> OnsetStanding {
    if !matches!(causal_bound, CausalBound::Enforced { .. }) {
        return OnsetStanding::NoCausalBound;
    }
    let OnsetPick::Picked {
        limit,
        pinned,
        edge_guard,
        ..
    } = &onset.pick
    else {
        // A bound at or after the peak is a decline, not a non-binding
        // bound: the bound was the limit, and it left nothing to search.
        return OnsetStanding::PickerDeclined;
    };
    if *limit != WindowLimit::CausalBound {
        return OnsetStanding::BoundNotBinding;
    }
    if *pinned {
        return OnsetStanding::PickOnWindowStart;
    }
    // `estimate_onset` runs the guard on every clear pick in a window the
    // bound started, so a guard that did not run does not arise here; it
    // is refused as unchecked rather than read as a pass.
    match edge_guard {
        Some(EdgeGuard::Passed) => {}
        Some(EdgeGuard::Failed { repick }) => {
            return OnsetStanding::EdgeFollowing { repick: *repick };
        }
        None => return OnsetStanding::EdgeFollowing { repick: None },
    }
    if matches!(verdict, IrVerdict::Failed { .. }) {
        return OnsetStanding::DeconvolutionFailed;
    }
    OnsetStanding::Unscored
}

/// Corroborate `report`'s same-capture reference τ ([`ReferenceLatency`])
/// against the τ `calibrate` has on file for that same reference pair
/// ([`MeasurementReport::reference_stored_latency`], #359).
///
/// A single `plot_ir` capture is one client lifetime, and a graph-buffering
/// shift of exactly one period is invisible within one lifetime (#347). The
/// reference leg is read in the same lifetime as the IR itself (#460), so
/// comparing it against an independently-lifecycled stored reading is the
/// same "measure a difference within a single client" remedy #347 uses for
/// `calibrate` — reused via [`compare_tau_readings`] rather than
/// reimplemented, so the wording is recognisably the same fault (AC2/AC3).
///
/// Any input other than two measured readings is [`ArrivalCheck::Unchecked`],
/// naming what is missing. This never reads the *capture's own*
/// `interface_latency`: that is a different pair's τ (#461), and unrelated
/// to whether the reference pair's lifetime matches this one.
pub(super) fn arrival_check(report: &MeasurementReport, sample_rate_hz: u32) -> ArrivalCheck {
    let same_capture = match &report.reference_latency {
        Some(ReferenceLatency::Measured(r)) => Ok(r.tau_s),
        Some(ReferenceLatency::Unavailable { reason }) => Err(reason.clone()),
        None => Err("no same-capture reference in this report".to_string()),
    };
    let stored = match &report.reference_stored_latency {
        Some(InterfaceLatency::Measured(m)) => Ok((m.tau_s, m.period_size)),
        Some(InterfaceLatency::Unavailable { reason }) => Err(reason.clone()),
        None => Err(
            "no stored latency for the reference pair (report predates schema v9, or no \
             reference configured)"
                .to_string(),
        ),
    };
    match (same_capture, stored) {
        (Ok(same_capture_tau_s), Ok((stored_tau_s, period_size))) => {
            match compare_tau_readings(
                stored_tau_s,
                same_capture_tau_s,
                sample_rate_hz,
                period_size,
            ) {
                TauComparison::Agree => ArrivalCheck::Agree,
                TauComparison::Disagree(d) if d.periods.is_some() => ArrivalCheck::PeriodShift(d),
                TauComparison::Disagree(d) => ArrivalCheck::Mismatch(d),
            }
        }
        (Err(reason), _) | (_, Err(reason)) => ArrivalCheck::Unchecked { reason },
    }
}

/// The latency [`IrStats::flight_time_s`] subtracts, or why there is none
/// (#544). Checked in this order, first miss wins: the report predates
/// v12; no reference loopback is configured; this capture's reference leg
/// has no valid reading; no inter-pair offset is on file for this pair.
/// The stored absolute τ (`interface_latency`) is never read.
pub(super) fn latency_basis(report: &MeasurementReport) -> LatencyBasis {
    let offset = match &report.inter_pair_offset {
        None => return LatencyBasis::Withheld(WithheldBasis::PredatesV12),
        Some(InterPairOffset::NotConfigured) => {
            return LatencyBasis::Withheld(WithheldBasis::NoReference)
        }
        Some(InterPairOffset::Identity) => Ok(LiveOffset::Identity),
        Some(InterPairOffset::Measured(m)) => Ok(LiveOffset::Measured {
            offset_s: m.offset_s,
            enumeration: m.enumeration.clone(),
        }),
        Some(InterPairOffset::Unavailable { reason }) => Err(reason.clone()),
    };
    let reference_tau_s = match &report.reference_latency {
        Some(ReferenceLatency::Measured(r)) => r.tau_s,
        Some(ReferenceLatency::Unavailable { reason }) => {
            return LatencyBasis::Withheld(WithheldBasis::ReferenceUnavailable {
                reason: reason.clone(),
            })
        }
        None => {
            return LatencyBasis::Withheld(WithheldBasis::ReferenceUnavailable {
                reason: "no same-capture reference in this report".to_string(),
            })
        }
    };
    match offset {
        Ok(offset) => LatencyBasis::Live {
            reference_tau_s,
            offset,
        },
        Err(reason) => LatencyBasis::Withheld(WithheldBasis::OffsetNotMeasured { reason }),
    }
}

/// Build the onset search's causal bound for `report` (#460).
///
/// Enforced only when the report carries a measured same-capture
/// [`ReferenceLatency`] *and* a finite, positive `position.distance_m`.
/// Anything else is [`CausalBound::Unavailable`] naming what is missing. A
/// report without the field (written before schema v7, or by a producer
/// with no reference) counts as the reference missing. The stored
/// `interface_latency` is deliberately not read: see #461.
///
/// #544: a measured inter-pair offset is added, so the bound sits where the
/// capture pair's own flight would start. Any other offset state adds
/// nothing — zero was the bound's implicit assumption before #544, and the
/// bound stays a diagnostic.
pub(super) fn causal_bound(
    report: &MeasurementReport,
    centre: usize,
    sample_rate_hz: u32,
) -> CausalBound {
    let distance_m = report
        .position
        .as_ref()
        .and_then(|p| p.distance_m)
        .filter(|d| d.is_finite() && *d > 0.0);
    let reference = match &report.reference_latency {
        Some(ReferenceLatency::Measured(r)) => Ok(r.tau_s),
        Some(ReferenceLatency::Unavailable { reason }) => Err(reason.clone()),
        None => Err("no same-capture reference in this report".to_string()),
    };
    match (reference, distance_m) {
        (Ok(reference_tau_s), Some(distance_m)) => {
            let temperature_c = report.position.as_ref().and_then(|p| p.temperature_c);
            let c = crate::shared::conversions::speed_of_sound_from_config(temperature_c);
            let pair_offset_s = match &report.inter_pair_offset {
                Some(InterPairOffset::Measured(m)) => m.offset_s,
                _ => 0.0,
            };
            let offset = (reference_tau_s + pair_offset_s + distance_m / c) * sample_rate_hz as f64;
            CausalBound::Enforced {
                index: (centre as f64 + offset).round().max(0.0) as usize,
                inputs: BoundInputs {
                    reference_tau_s,
                    distance_m,
                    speed_of_sound_m_s: c,
                    temperature_c,
                },
            }
        }
        (Ok(_), None) => CausalBound::Unavailable(MissingBoundInput::Distance),
        (Err(reason), Some(_)) => {
            CausalBound::Unavailable(MissingBoundInput::ReferenceLatency { reason })
        }
        (Err(reference_reason), None) => {
            CausalBound::Unavailable(MissingBoundInput::Both { reference_reason })
        }
    }
}

/// Score `arrival_s − latency` against the typed distance (#537 architect
/// revision 3, operator ruling 3). The latency is `basis`'s — the same-
/// capture reference plus the inter-pair offset (#544), the one
/// [`IrStats::flight_time_s`] subtracts. Runs whether or not another layer
/// withholds the flight time; [`DistanceCheck::NoLatency`] when the basis
/// has no latency.
///
/// One-sided (#552): only an arrival earlier than `d/c − ε` is refused.
/// There is no late edge — the excess over `d/c` is reported, not judged.
pub(super) fn distance_check(
    report: &MeasurementReport,
    arrival_s: f64,
    basis: &LatencyBasis,
) -> DistanceCheck {
    let position = report.position.as_ref();
    let Some(distance_m) = position.and_then(|p| p.distance_m) else {
        return DistanceCheck::NotGiven;
    };
    if !(distance_m.is_finite() && distance_m > 0.0) {
        return DistanceCheck::NotPositive { distance_m };
    }
    let Some(latency_s) = basis.latency_s() else {
        return DistanceCheck::NoLatency { distance_m };
    };
    let temperature_c = position.and_then(|p| p.temperature_c);
    let window = DistanceWindow::new(distance_m, temperature_c);
    let excess_s = arrival_s - latency_s - window.expected_s;
    if excess_s < window.low_s {
        DistanceCheck::TooEarly { window, excess_s }
    } else {
        DistanceCheck::Consistent { window, excess_s }
    }
}

/// Pre-impulse noise floor region: everything strictly before the peak,
/// minus a small guard band so the peak's own skirt doesn't bias the
/// floor estimate upward. Empty when the guard band consumes the whole
/// pre-peak window — which [`ir_verdict`] treats as a failure, not as a
/// clean floor.
///
/// The guard arithmetic itself lives in `measurement::sweep` (#368), so
/// `ac-daemon`'s τ gate and this read-out cannot drift apart on what
/// "pre-impulse" means; this only turns the length into the slice
/// [`ir_verdict`] needs for its empty check.
pub(super) fn pre_impulse_region(linear_ir: &[f64], peak_index: usize) -> &[f64] {
    &linear_ir[..crate::measurement::sweep::pre_impulse_region_len(linear_ir.len(), peak_index)]
}

/// The index [`IrStats::pre_impulse_snr_db`]'s floor ends before, and which
/// of the two it was (#550 architect revision 4). `arrival_floor_len` is
/// `pre_impulse_region(linear_ir, arrival_index).len()`.
///
/// The floor ends before the arrival only when the arrival is *trusted*:
/// band-limited, its SNR measured (a non-empty floor before it), and its
/// standing not [`ArrivalCrossCheck::BandLimitedSnrLow`]. Otherwise it ends
/// before the peak, the region #501 scored. An untrusted pick is not used:
/// on a noise-only capture the high-passed argmax lands early and at
/// random, and a floor cut short before it accepted 9 of 400 noise-only
/// draws (#550 architect revision 3).
///
/// The standing alone does not decide trust. A pick inside the guard band
/// has an empty floor, so its SNR reads `+inf` and clears
/// [`ARRIVAL_SNR_MIN_DB`] without having been measured (#577). Trusting
/// it emptied the verdict's floor on 11 of 400 noise-only draws, and the
/// refusal then blamed a peak that sat far outside the guard band (#550
/// revision 4). The measured test is on the region's length, not on the
/// SNR's finiteness: `+inf` over a non-empty all-zero floor is measured
/// and stays trusted. It is decided before `BandLimitedSnrLow`, so the
/// anchor holds whichever standing #577 gives an unmeasured pick.
fn floor_anchor(
    arrival_index: usize,
    peak_index: usize,
    arrival_source: &ArrivalSource,
    arrival_cross_check: &ArrivalCrossCheck,
    arrival_floor_len: usize,
) -> (usize, PreImpulseAnchor) {
    match arrival_index.cmp(&peak_index) {
        std::cmp::Ordering::Equal => (peak_index, PreImpulseAnchor::ArrivalAndPeak),
        std::cmp::Ordering::Greater => (peak_index, PreImpulseAnchor::Peak),
        std::cmp::Ordering::Less => {
            if !matches!(arrival_source, ArrivalSource::BandLimitedPeak { .. }) {
                (peak_index, PreImpulseAnchor::Peak)
            } else if arrival_floor_len == 0 {
                (peak_index, PreImpulseAnchor::PeakArrivalUnmeasured)
            } else if matches!(
                arrival_cross_check,
                ArrivalCrossCheck::BandLimitedSnrLow { .. }
            ) {
                (peak_index, PreImpulseAnchor::PeakArrivalNotTrusted)
            } else {
                (arrival_index, PreImpulseAnchor::Arrival)
            }
        }
    }
}

/// Contamination-robust pre-impulse floor: the median absolute sample of
/// `pre_region`, scaled by the standard MAD-to-σ constant so it targets
/// the same quantity [`crate::measurement::sweep::pre_impulse_snr_db`]'s
/// RMS floor does on clean
/// noise. `0.0` for an empty region.
///
/// #353 (option A′), retained under #378 with a narrower job. The RMS
/// floor has a breakdown point of zero — a single sample of the peak's own
/// sustained energy bleeding into `pre_region` moves it, by an amount that
/// scales with that sample's amplitude. A rank statistic has a 50%
/// breakdown point: up to half of `pre_region` can be lobe by count and
/// the median still reads the noise floor.
///
/// #378 demoted it from `estimate_onset`'s threshold input to its validity
/// gate — the picker takes no threshold at all, and this floor now decides
/// only whether the search window holds anything above the floor, never
/// where inside it the onset is. Its 50% breakdown point is what makes
/// that gate trustworthy on a contaminated pre-impulse region.
/// [`crate::measurement::sweep::pre_impulse_snr_db`] and its RMS floor
/// stay exactly as they were —
/// this is a second floor for a second question, not a replacement (#346
/// architect review, #378 AC5).
pub(super) fn onset_floor(pre_region: &[f64]) -> f64 {
    /// Φ⁻¹(0.75) — the standard MAD-to-σ constant, not a tuned value.
    const MAD_TO_SIGMA: f64 = 0.6744897501960817;
    if pre_region.is_empty() {
        return 0.0;
    }
    let mut abs_vals: Vec<f64> = pre_region.iter().map(|v| v.abs()).collect();
    abs_vals.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n = abs_vals.len();
    let median = if n % 2 == 1 {
        abs_vals[n / 2]
    } else {
        (abs_vals[n / 2 - 1] + abs_vals[n / 2]) / 2.0
    };
    median / MAD_TO_SIGMA
}

/// Gate duration, low-frequency limit and window shape for an IR payload.
///
/// Prefers the gate the producer actually applied. #280 stores `f_low_hz`
/// on the payload precisely so a reader does not recompute it; falling
/// back to `window_len / sample_rate_hz` only covers legacy (v1-v3)
/// reports, where no gate was recorded and the rectangular `extract_irs`
/// window is the only gate that could have produced this payload.
pub(super) fn resolve_gate(
    gate: Option<&GateParams>,
    window_len: usize,
    sample_rate_hz: u32,
) -> (f64, f64, String) {
    match gate {
        Some(g) => (g.gate_length_s, g.f_low_hz, g.window_kind.clone()),
        None => {
            let len_s = window_len as f64 / sample_rate_hz as f64;
            (len_s, 1.0 / len_s, "rectangular (not recorded)".to_string())
        }
    }
}

/// Whether a capture's peak is trustworthy enough to present as a result
/// (#376). Split out of [`MeasurementReport::ir_stats`] so this rule —
/// the one `ac-cli` and `ac-scene` both read through
/// [`IrStats::verdict`] — can be exercised directly, without assembling
/// a whole report around it.
///
/// `snr_db` goes to +inf two different ways, and only one of them is a
/// failure. A zero floor against a nonzero peak (`rms == 0.0`,
/// `pre_region` nonempty) is the *best* possible capture — infinite SNR,
/// not an unmeasurable one — and clears any finite threshold below, so it
/// falls through to the ordinary threshold comparison rather than being
/// special-cased out. What fails closed is the case with nothing to
/// measure at all: an empty `pre_region` (the guard band consumed the
/// whole pre-peak window) or a zero peak (nothing captured, so there is
/// no signal to compare a floor against either) — absence of proof of a
/// good floor is not the same as proof of one.
pub(super) fn ir_verdict(peak_magnitude: f64, pre_region: &[f64], snr_db: f64) -> IrVerdict {
    if peak_magnitude == 0.0 {
        IrVerdict::Failed {
            reason: "no signal captured (linear IR is all zero)".to_string(),
        }
    } else if pre_region.is_empty() {
        IrVerdict::Failed {
            reason: "no measurable pre-impulse floor (peak too close to \
                     the start of the gated window)"
                .to_string(),
        }
    } else if snr_db < PRE_IMPULSE_SNR_MIN_DB {
        IrVerdict::Failed {
            reason: "pre-impulse SNR below threshold".to_string(),
        }
    } else {
        IrVerdict::Ok
    }
}

/// See [`MeasurementReport::ir_stats`].
#[derive(Debug, Clone, PartialEq)]
pub struct IrStats {
    pub sample_rate_hz: u32,
    /// Length of the gated linear IR, in samples.
    pub window_len: usize,
    /// Index of the broadband peak-magnitude sample within the gated IR.
    /// What [`Self::pre_impulse_snr_db`] and [`Self::verdict`] are read
    /// from, and what [`Self::arrival_cross_check`] compares the arrival
    /// against. The arrival itself is [`Self::arrival_index`] (#537).
    pub peak_index: usize,
    /// `|linear_ir[peak_index]|`.
    pub peak_magnitude: f64,
    /// Index of the estimated onset within the gated IR — see
    /// [`crate::measurement::sweep::estimate_onset`], searched before
    /// [`Self::arrival_index`]. A diagnostic, never
    /// the arrival: #378's AC6 rig run (pupu, 2026-09-15) found the
    /// unbounded onset's 1.000 m → 2.000 m increment missing
    /// `transfer_stream`'s by 143.75 samples, against the peak's 8.62, and
    /// the bounded onset's pre-registered rig check (pupu, 2026-09-16)
    /// refused to conclude. [`Self::onset_standing`] states its standing.
    pub onset_index: usize,
    /// The rule that produced `onset_index`, from
    /// [`crate::measurement::sweep::OnsetEstimate::rule`] — states
    /// whether a causal bound (known geometry) was enforced, so a
    /// persisted onset can be told apart from a bare peak read a year
    /// later (#346 acceptance criterion 4).
    pub onset_rule: String,
    /// The causal bound the onset search ran under, or which input it
    /// lacked (#460). Carries the bound's inputs so a read-out prints them
    /// rather than re-deriving them; `min_admissible_index()` is the index.
    pub causal_bound: CausalBound,
    /// Which rule produced `delay_samples` / `arrival_s` (#346 acceptance
    /// criterion 4): [`ArrivalSource::BandLimitedPeak`], or
    /// [`ArrivalSource::Peak`] when the band does not allow it (#537).
    pub arrival_source: ArrivalSource,
    /// Index of the arrival within the gated IR: the peak of the IR
    /// high-passed per [`Self::arrival_source`], or [`Self::peak_index`]
    /// when the source is [`ArrivalSource::Peak`]. What `delay_samples` /
    /// `arrival_s` are derived from. On a multi-way loudspeaker a peak sits
    /// a group-delay offset past the wavefront, so the arrival carries that
    /// offset (#346); the high-pass removes a late low-frequency maximum
    /// (#537), not that offset.
    pub arrival_index: usize,
    /// Pre-impulse SNR of the high-passed IR at [`Self::arrival_index`],
    /// in dB, gated at [`ARRIVAL_SNR_MIN_DB`]. `None` when the arrival is
    /// not band-limited.
    pub band_limited_snr_db: Option<f64>,
    /// How far the largest other local maximum of the high-passed IR within
    /// one corner period of the arrival sits below it, in dB (#537
    /// architect revision 2), gated at [`ARRIVAL_LOBE_MARGIN_MIN_DB`].
    /// `+inf` when there is no other maximum in that window; `None` when
    /// the arrival is not band-limited.
    pub arrival_lobe_margin_db: Option<f64>,
    /// Where that maximum sits, `lobe − arrival` in signed samples
    /// (negative: before the arrival). `None` when there is none, or the
    /// arrival is not band-limited.
    pub arrival_lobe_offset: Option<i64>,
    /// Level of the broadband peak the Δ is measured to (`r`), in dB re the
    /// broadband maximum: 0 when `r` is the maximum itself. `Some` on
    /// `Agrees` and `BroadbandLater` only — `BroadbandEarlier` measures to
    /// the maximum, and the other standings measure no Δ.
    pub broadband_delta_level_db: Option<f64>,
    /// The arrival's guards and cross-check against the broadband IR (#537). Some
    /// standings withhold [`Self::flight_time_s`]; see
    /// [`ArrivalCrossCheck::withholds_flight_time`].
    pub arrival_cross_check: ArrivalCrossCheck,
    /// The onset diagnostic's standing: the first of `onset_standing`'s
    /// conditions that failed, or [`OnsetStanding::Unscored`] when all
    /// held. It does not affect any number on this struct.
    pub onset_standing: OnsetStanding,
    /// Signed offset of the arrival — `arrival_index` — from the gate centre
    /// (`window_len / 2`), in samples. Positive means the response arrived after the
    /// zero-delay reference position.
    pub delay_samples: i64,
    /// `delay_samples / sample_rate_hz` — arrival time relative to the
    /// gate's zero-delay reference. This is **not** acoustic path delay:
    /// it still contains any uncorrected interface latency, which is why
    /// it must not be converted to a distance without a calibrated τ.
    pub arrival_s: f64,
    /// This capture's same-capture reference τ against the stored τ
    /// `calibrate` has on file for that pair (#359). Since #544 the **drift
    /// readout**: how far the reference moved since it was stored. It gates
    /// nothing — [`Self::flight_time_s`] subtracts the live reading, so a
    /// moved reference is the case compensation handles, not a fault.
    pub arrival_check: ArrivalCheck,
    /// What [`Self::flight_time_s`] subtracts, or why it cannot (#544): this
    /// capture's reference latency plus the stored inter-pair offset, never
    /// the stored absolute τ of the capture pair.
    pub latency_basis: LatencyBasis,
    /// `arrival_s − (reference latency + inter-pair offset)` (#544): a peak
    /// arrival minus this capture's own peak-picked reference τ and the
    /// offset between the two pairs, measured once in one capture. With the
    /// arrival band-limited (#537) it is a **band-limited delay estimate at
    /// the corner** of [`Self::arrival_source`]: the same IR reads
    /// differently at another corner, and nothing here identifies the
    /// direct path. [`Self::distance_check`] is the only evidence about the
    /// path, and it says only whether the number is not earlier than the
    /// typed distance allows, and by how much it exceeds `d/c`.
    ///
    /// `None` whenever [`Self::arrival_cross_check`] or
    /// [`Self::distance_check`] withholds it, and whenever
    /// [`Self::latency_basis`] is [`LatencyBasis::Withheld`] — no reference
    /// configured, no valid reference reading, no offset on file, or a
    /// report predating v12. There is no fallback to the stored τ.
    pub flight_time_s: Option<f64>,
    /// The flight time scored against the typed `position.distance_m`
    /// (#537 architect revision 3). Only [`DistanceCheck::TooEarly`]
    /// withholds [`Self::flight_time_s`]; there is no late edge (#552), so
    /// any delay past `d/c` is reported as the excess, not judged. Scored
    /// even when another layer withholds it, so every reason can be named.
    pub distance_check: DistanceCheck,
    /// `20·log10(peak_magnitude / rms(linear_ir[..pre_impulse_floor_end]))`:
    /// the broadband peak over the floor that ends one guard band before
    /// [`Self::pre_impulse_floor_anchor`] (#550). `+inf` when no
    /// pre-impulse energy was measurable at all (silent floor).
    pub pre_impulse_snr_db: f64,
    /// Which index the pre-impulse floor ends before (#550): the earlier of
    /// [`Self::arrival_index`] and [`Self::peak_index`] when the arrival is
    /// trusted (band-limited, its SNR measured over a non-empty floor, and
    /// not `BandLimitedSnrLow`), otherwise the peak.
    pub pre_impulse_floor_anchor: PreImpulseAnchor,
    /// Exclusive end of the pre-impulse floor region, in samples: the
    /// floor is `linear_ir[..pre_impulse_floor_end]`. Zero when the guard
    /// band consumes everything before the anchor.
    pub pre_impulse_floor_end: usize,
    /// Gate window duration, in seconds — the recorded
    /// [`GateParams::gate_length_s`] when the payload carries one.
    pub gate_window_s: f64,
    /// The lowest frequency for which one full period fits inside the
    /// gate window. Read from [`GateParams::f_low_hz`] when recorded;
    /// content below it is not reliably resolved by a gate this short.
    pub gate_f_low_hz: f64,
    /// Window shape the gate applied, from [`GateParams::window_kind`].
    /// `"rectangular (not recorded)"` for legacy reports that stored no
    /// gate — an inference from `extract_irs`, flagged as such so a
    /// reader does not mistake it for a recorded value.
    pub gate_window_kind: String,
    /// Whether this capture's peak is trustworthy enough to present as a
    /// result, per [`PRE_IMPULSE_SNR_MIN_DB`] (#376). Computed once here
    /// so `ac-cli`'s text read-out and `ac-scene`'s sweep-IR panel read
    /// the same verdict rather than each re-deriving their own rule from
    /// [`Self::pre_impulse_snr_db`].
    pub verdict: IrVerdict,
}

impl IrStats {
    /// The broadband Δ, signed samples, read from the standing rather than
    /// recomputed ([`ArrivalCrossCheck::gap`]): `r − arrival`, or `argmax −
    /// arrival` on `BroadbandEarlier`. `None` on standings that measured no
    /// gap.
    pub fn broadband_delta_samples(&self) -> Option<i64> {
        self.arrival_cross_check.gap()
    }

    /// The sample the pre-impulse floor is anchored on (#550): the
    /// arrival or the peak, whichever [`Self::pre_impulse_floor_anchor`]
    /// names.
    pub fn pre_impulse_floor_anchor_index(&self) -> usize {
        match self.pre_impulse_floor_anchor {
            PreImpulseAnchor::Peak
            | PreImpulseAnchor::PeakArrivalNotTrusted
            | PreImpulseAnchor::PeakArrivalUnmeasured => self.peak_index,
            PreImpulseAnchor::Arrival | PreImpulseAnchor::ArrivalAndPeak => self.arrival_index,
        }
    }

    /// Where the pre-impulse floor ends, as `ac-cli` prints it line by line
    /// and `ac-scene` joins with `, ` (#550 UX): `floor ends 1200 samples
    /// before arrival, sample 21246`. On
    /// [`PreImpulseAnchor::PeakArrivalNotTrusted`] a second line names the
    /// earlier pick that was set aside and why, without its sample: `not
    /// before arrival — arrival SNR 12.2 dB, required ≥ 35.0 dB`. On
    /// [`PreImpulseAnchor::PeakArrivalUnmeasured`] the second line is the
    /// fixed `not before arrival — ` + [`ARRIVAL_SNR_UNMEASURED_REASON`]: it
    /// never formats the arrival SNR, which is `+inf` there (#577). Empty
    /// when the floor region is — the verdict's reason already says there
    /// is no floor.
    pub fn pre_impulse_floor_lines(&self) -> Vec<String> {
        if self.pre_impulse_floor_end == 0 {
            return Vec::new();
        }
        let anchor = self.pre_impulse_floor_anchor_index();
        let mut lines = vec![format!(
            "floor ends {} samples before {}, sample {anchor}",
            anchor - self.pre_impulse_floor_end,
            self.pre_impulse_floor_anchor.name(),
        )];
        match self.pre_impulse_floor_anchor {
            PreImpulseAnchor::PeakArrivalNotTrusted => {
                if let ArrivalCrossCheck::BandLimitedSnrLow { snr_db } = self.arrival_cross_check {
                    lines.push(format!(
                        "not before arrival \u{2014} {}",
                        arrival_snr_low_reason(snr_db)
                    ));
                }
            }
            PreImpulseAnchor::PeakArrivalUnmeasured => lines.push(format!(
                "not before arrival \u{2014} {ARRIVAL_SNR_UNMEASURED_REASON}"
            )),
            PreImpulseAnchor::Arrival
            | PreImpulseAnchor::Peak
            | PreImpulseAnchor::ArrivalAndPeak => {}
        }
        lines
    }
}

/// Why a band-limited arrival inside the guard band was not trusted for the
/// pre-impulse floor (#550 UX revision 4): there was no floor before it to
/// measure its SNR against. A fixed string, so an unmeasured `+inf` never
/// prints as a number.
pub const ARRIVAL_SNR_UNMEASURED_REASON: &str = "arrival SNR unmeasured, no floor before it";

/// Why a band-limited arrival was not trusted (#537, #550):
/// `arrival SNR 12.2 dB, required ≥ 35.0 dB`. One string for the flight
/// time's withheld reason and the pre-impulse floor line, so the two
/// cannot drift.
pub fn arrival_snr_low_reason(snr_db: f64) -> String {
    format!("arrival SNR {snr_db:.1} dB, required \u{2265} {ARRIVAL_SNR_MIN_DB:.1} dB")
}

/// Which index [`IrStats::pre_impulse_snr_db`]'s floor ends before, and
/// whether a band-limited arrival was passed over for it (#550).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreImpulseAnchor {
    /// A trusted band-limited arrival (its SNR measured over a non-empty
    /// floor and clearing [`ARRIVAL_SNR_MIN_DB`]) precedes the broadband
    /// peak — the room-mode case #550 fixes.
    Arrival,
    /// The broadband peak precedes the arrival
    /// ([`ArrivalCrossCheck::BroadbandEarlier`]), trusted or not, or there
    /// is no band-limited arrival. The region is the one #501 scored. An
    /// earlier band-limited arrival that was set aside is never this
    /// variant: see [`Self::PeakArrivalNotTrusted`] and
    /// [`Self::PeakArrivalUnmeasured`].
    Peak,
    /// The two are the same sample, trusted or not: a clean loopback, or
    /// no band-limited arrival. The region is the one #501 scored.
    ArrivalAndPeak,
    /// The band-limited arrival precedes the peak but missed its own SNR
    /// gate ([`ArrivalCrossCheck::BandLimitedSnrLow`]), so the floor ends
    /// before the peak — the region #501 scored — and may contain that
    /// pick.
    PeakArrivalNotTrusted,
    /// The band-limited arrival precedes the peak but sits inside the guard
    /// band, so there was no floor to measure its SNR against (#550
    /// revision 4). The floor ends before the peak — the region #501
    /// scored. Decided before the standing, whatever #577 makes of the
    /// `+inf` SNR.
    PeakArrivalUnmeasured,
}

impl PreImpulseAnchor {
    /// The anchor's word in the floor line.
    pub fn name(self) -> &'static str {
        match self {
            Self::Arrival => "arrival",
            Self::Peak | Self::PeakArrivalNotTrusted | Self::PeakArrivalUnmeasured => "peak",
            Self::ArrivalAndPeak => "arrival and peak",
        }
    }
}

/// What an [`IrStats::flight_time_s`] is measured against (#544).
#[derive(Debug, Clone, PartialEq)]
pub enum LatencyBasis {
    /// This capture's reference leg, plus the inter-pair offset. The live
    /// reading carries whatever converter, transport and graph delay the
    /// capture had; the offset carries what the two pairs do not share.
    Live {
        reference_tau_s: f64,
        offset: LiveOffset,
    },
    /// No latency to subtract; the flight time is withheld.
    Withheld(WithheldBasis),
}

impl LatencyBasis {
    /// `reference_tau_s + offset_s` when live.
    pub fn latency_s(&self) -> Option<f64> {
        match self {
            LatencyBasis::Live {
                reference_tau_s,
                offset,
            } => Some(reference_tau_s + offset.offset_s()),
            LatencyBasis::Withheld(_) => None,
        }
    }
}

/// The offset half of a [`LatencyBasis::Live`].
#[derive(Debug, Clone, PartialEq)]
pub enum LiveOffset {
    /// The capture pair is the reference pair: zero by definition.
    Identity,
    /// A stored offset for this exact topology. `enumeration` is its
    /// epoch against this capture's, a flag and never a gate.
    Measured {
        offset_s: f64,
        enumeration: EnumerationCheck,
    },
}

impl LiveOffset {
    pub fn offset_s(&self) -> f64 {
        match self {
            LiveOffset::Identity => 0.0,
            LiveOffset::Measured { offset_s, .. } => *offset_s,
        }
    }
}

/// Why [`LatencyBasis`] has no latency, in the order
/// `latency_basis` checks them.
#[derive(Debug, Clone, PartialEq)]
pub enum WithheldBasis {
    /// The report predates schema v12: no offset was recorded.
    PredatesV12,
    /// No reference loopback was configured for the capture.
    NoReference,
    /// The reference leg gave no valid reading in this capture (#471's
    /// derived floor, the edge or xrun gates, or no leg captured).
    /// `reason` is the reference's own, `<observation>[; check: …]`.
    ReferenceUnavailable { reason: String },
    /// No inter-pair offset on file for this topology. `reason` is
    /// [`super::InterPairOffset::Unavailable`]'s, naming the pair.
    OffsetNotMeasured { reason: String },
}

/// Verdict on whether an [`IrStats`] peak is a trustworthy deconvolution
/// result or noise-floor pickup masquerading as one (#376). `Failed`
/// never carries a computed arrival, distance, or peak-as-result — only
/// the reason, naming what to check without asserting a cause (drive
/// level, mic gain, distance, room noise are all plausible; the
/// instrument cannot tell which).
#[derive(Debug, Clone, PartialEq)]
pub enum IrVerdict {
    Ok,
    Failed { reason: String },
}

/// Which rule produced an [`IrStats`] arrival (#346 acceptance criterion 4).
/// An enum so the tag is typed. A new variant needs a design decision
/// backed by a scored rig check (#346 architect revision 3, operator
/// decision 2026-09-16); `BandLimitedPeak` is #537's, with its rig check
/// on that issue.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ArrivalSource {
    /// The broadband magnitude peak (argmax |h|). Only when the band does
    /// not allow [`Self::BandLimitedPeak`].
    Peak,
    /// The magnitude peak of the IR high-passed at `corner_hz` with zero
    /// phase ([`crate::measurement::sweep::zero_phase_high_pass`], #537).
    BandLimitedPeak { corner_hz: f64 },
}

/// The band-limited arrival's guards and its cross-check against the
/// broadband IR (#537, operator option 4, architect revision 2). The
/// standings are checked in declaration order and the first that fires is
/// the result. They only add to the existing gates (#376 `verdict`, #544
/// `latency_basis`): a flight time is produced only when every layer
/// allows one. Every standing that doubts the pick
/// withholds; only `BroadbandLater` marks a produced value.
#[derive(Debug, Clone, PartialEq)]
pub enum ArrivalCrossCheck {
    /// The stimulus does not reach
    /// [`crate::measurement::sweep::BAND_LIMIT_MIN_F2_RATIO`] times the
    /// corner: `band_top_hz` (the payload's `f2_hz`, capped at Nyquist)
    /// against `required_hz`. The arrival is the broadband peak, which
    /// is #537's defect when a room mode sits late, so the flight time is
    /// withheld.
    BandLimitUnavailable { band_top_hz: f64, required_hz: f64 },
    /// The high-passed IR's pre-impulse SNR is below
    /// [`ARRIVAL_SNR_MIN_DB`]. Withholds the flight time.
    BandLimitedSnrLow { snr_db: f64 },
    /// Another local maximum of the high-passed IR within one corner period
    /// of the pick is less than [`ARRIVAL_LOBE_MARGIN_MIN_DB`] below it: the
    /// pick may be a half-cycle off. `margin_db` is how far below the pick
    /// it sits, `offset` its position re the pick (negative: before).
    /// Withholds the flight time.
    ArrivalAmbiguous { margin_db: f64, offset: i64 },
    /// A high-passed sample more than one corner period before the arrival
    /// is within [`ARRIVAL_EARLIER_COMPARABLE_DB`] of it: the pick may be a
    /// strong HF reflection. `index` is that sample (the largest such),
    /// `level_db` its level re the arrival. Withholds the flight time.
    EarlierComparable { index: usize, level_db: f64 },
    /// The broadband argmax is earlier than the arrival by more than the
    /// tolerance (`gap` = argmax − arrival, negative): something arrived
    /// before the pick. Withholds the flight time.
    BroadbandEarlier { gap: i64 },
    /// `r` — the earliest broadband peak at or after `arrival − tolerance`
    /// within [`ARRIVAL_BROADBAND_COMPARABLE_DB`] of the maximum — is later
    /// than the arrival by more than the tolerance (`gap` = r − arrival,
    /// positive): nothing comparable near the arrival, as with #537's room
    /// mode. The flight time is produced and marked with the gap.
    BroadbandLater { gap: i64 },
    /// `r` is within the tolerance of the arrival (`gap` = r − arrival).
    Agrees { gap: i64 },
}

impl ArrivalCrossCheck {
    /// Whether this standing withholds [`IrStats::flight_time_s`].
    pub fn withholds_flight_time(&self) -> bool {
        matches!(
            self,
            ArrivalCrossCheck::BandLimitUnavailable { .. }
                | ArrivalCrossCheck::BandLimitedSnrLow { .. }
                | ArrivalCrossCheck::ArrivalAmbiguous { .. }
                | ArrivalCrossCheck::EarlierComparable { .. }
                | ArrivalCrossCheck::BroadbandEarlier { .. }
        )
    }

    /// The broadband gap this standing measured, signed samples: `r −
    /// arrival` for `Agrees`/`BroadbandLater`, `argmax − arrival` for
    /// `BroadbandEarlier`. `None` where no gap was measured.
    pub fn gap(&self) -> Option<i64> {
        match *self {
            ArrivalCrossCheck::Agrees { gap }
            | ArrivalCrossCheck::BroadbandLater { gap }
            | ArrivalCrossCheck::BroadbandEarlier { gap } => Some(gap),
            _ => None,
        }
    }
}

/// The earliest flight time a typed distance allows (#537 architect
/// revision 3): `expected_s + low_s`. One-sided (#552): there is no late
/// edge. How late sound leaves the system under test (crossover, DSP,
/// deliberate alignment delay) is a property of that system, not a bound
/// the instrument can know, so the excess over `d/c` is reported, not
/// judged.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DistanceWindow {
    /// The typed `position.distance_m`.
    pub distance_m: f64,
    /// The speed of sound the window divides by, from
    /// [`crate::shared::conversions::speed_of_sound_from_config`].
    pub speed_of_sound_m_s: f64,
    /// The recorded temperature `speed_of_sound_m_s` came from; `None` when
    /// the default was assumed.
    pub temperature_c: Option<f64>,
    /// `d / c`, seconds.
    pub expected_s: f64,
    /// `−ε`, seconds, with `ε = (`[`DISTANCE_TAPE_TOLERANCE_M`]` +
    /// `[`DISTANCE_SPEED_OF_SOUND_REL_TOL`]`·d) / c`. A zero-phase peak
    /// cannot precede its path, so only the tape, c or τ can put a flight
    /// time below this.
    pub low_s: f64,
}

impl DistanceWindow {
    /// The earliest-arrival edge for `distance_m` at `temperature_c`.
    pub fn new(distance_m: f64, temperature_c: Option<f64>) -> Self {
        let c = crate::shared::conversions::speed_of_sound_from_config(temperature_c);
        let epsilon_s =
            (DISTANCE_TAPE_TOLERANCE_M + DISTANCE_SPEED_OF_SOUND_REL_TOL * distance_m) / c;
        Self {
            distance_m,
            speed_of_sound_m_s: c,
            temperature_c,
            expected_s: distance_m / c,
            low_s: -epsilon_s,
        }
    }
}

/// The flight time against the typed distance (#537 architect revision 3).
/// The only evidence about the path the instrument has, and all it says is
/// *not earlier than the distance allows* — never *direct*, and never
/// *matches d/c*. `excess_s` is `flight − d / c`, seconds, compared against
/// [`DistanceWindow::low_s`] only (#552).
#[derive(Debug, Clone, PartialEq)]
pub enum DistanceCheck {
    /// No distance typed. The flight time is produced unchecked.
    NotGiven,
    /// A distance was typed but is not a finite positive length (a `0m`
    /// cable, say): nothing to check against. Not a verdict.
    NotPositive { distance_m: f64 },
    /// A distance was typed but [`IrStats::latency_basis`] has no latency
    /// (#544), so there is no flight time to check. Not a verdict.
    NoLatency { distance_m: f64 },
    /// Not earlier than `d/c − ε`. Means only that; it does not mean the
    /// flight matches `d/c`. `excess_s` is reported, not judged (#552): a
    /// delay tower 150 ms late is `Consistent` with `excess_s` ≈ 0.15.
    Consistent {
        window: DistanceWindow,
        excess_s: f64,
    },
    /// Earlier than `d/c − ε`: with correct inputs this cannot happen, so
    /// the typed distance, temperature, reference latency or offset is
    /// wrong. Withholds the flight time.
    TooEarly {
        window: DistanceWindow,
        excess_s: f64,
    },
}

impl DistanceCheck {
    /// Whether this check withholds [`IrStats::flight_time_s`].
    pub fn withholds_flight_time(&self) -> bool {
        matches!(self, DistanceCheck::TooEarly { .. })
    }

    /// The window and the excess, when a verdict was reached.
    pub fn scored(&self) -> Option<(&DistanceWindow, f64)> {
        match self {
            DistanceCheck::Consistent { window, excess_s }
            | DistanceCheck::TooEarly { window, excess_s } => Some((window, *excess_s)),
            _ => None,
        }
    }
}

/// The standing of an [`IrStats`] onset diagnostic — one variant per
/// condition, in the order they are checked, plus [`Self::Unscored`] when
/// every condition held. None of them affects the arrival.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnsetStanding {
    /// No causal bound was enforced; [`IrStats::causal_bound`] names the
    /// missing input.
    NoCausalBound,
    /// A bound was enforced but the search span, not the bound, set the
    /// window start.
    BoundNotBinding,
    /// The onset picker declined; [`IrStats::onset_rule`] names the case.
    PickerDeclined,
    /// The pick sits on the window start, so the true onset may lie
    /// earlier.
    PickOnWindowStart,
    /// The edge-following guard failed. `repick: Some(r)`: the re-pick
    /// with the window start moved
    /// [`crate::measurement::sweep::EDGE_GUARD_EXTENSION_M`] earlier went
    /// to sample `r`, so the pick follows the window edge. `repick: None`:
    /// no re-pick ran, so the pick could not be checked. Mirrors
    /// [`crate::measurement::sweep::EdgeGuard::Failed`].
    EdgeFollowing { repick: Option<usize> },
    /// The deconvolution verdict is `Failed`.
    DeconvolutionFailed,
    /// Every condition held: bounded, binding, clear of the window start,
    /// guard passed, verdict not `Failed`. No rig score authorises this
    /// pick as an arrival: the pre-registered run (pupu, 2026-09-16)
    /// refused to conclude, and the operator declined a new estimator.
    Unscored,
}

/// Outcome of corroborating this capture's same-capture reference τ against
/// the stored τ `calibrate` has on file for that pair (#359). Never a bare
/// "corroborated" — #363's vocabulary rule applies here too: state the
/// evidence, not the verdict. `PeriodShift` and `Mismatch` both carry the
/// [`TauDisagreement`] that produced them, split on whether the delta is an
/// exact multiple of the period — the same distinction #347 draws for
/// `calibrate`'s own τ readings, via the same comparator
/// ([`compare_tau_readings`]).
///
/// Since #544 a drift readout only: no variant withholds the flight time.
#[derive(Debug, Clone, PartialEq)]
pub enum ArrivalCheck {
    /// The two readings match to the whole sample.
    Agree,
    /// Disagree by an exact multiple of the period — a graph-buffering
    /// shift, not hardware drift (#347's own finding, reused verbatim).
    PeriodShift(TauDisagreement),
    /// Disagree by an amount that is not a period multiple — a different
    /// fault (e.g. the #461 SYT re-pick), not this issue's failure mode.
    Mismatch(TauDisagreement),
    /// Not checked: `reason` names what is missing — no same-capture
    /// reference, a reference reading that failed its own gates, or no
    /// stored τ for the reference pair.
    Unchecked { reason: String },
}

#[cfg(test)]
mod tests {
    use super::super::fixtures::*;
    use super::super::*;
    use super::*;
    use crate::shared::calibration::LayerVerdict;

    /// The arrival source every band-limited fixture here expects (#537).
    const BAND_LIMITED: ArrivalSource = ArrivalSource::BandLimitedPeak {
        corner_hz: ARRIVAL_HIGH_PASS_CORNER_HZ,
    };

    #[test]
    fn ir_stats_reports_delay_samples_relative_to_gate_centre() {
        // Peak 32 samples after the window centre — the fake backend's
        // fixed loopback delay (see `ac-daemon/src/audio/fake.rs`).
        let window_len = 1024;
        let centre = window_len / 2;
        let r = ir_report_with_peak(window_len, centre + 32, 1.0, 0.0, 48_000);
        let stats = r.ir_stats().expect("impulse response data present");
        assert_eq!(stats.delay_samples, 32);
        assert!((stats.arrival_s - 32.0 / 48_000.0).abs() < 1e-12);
        assert_eq!(stats.peak_index, centre + 32);
        assert_eq!(stats.peak_magnitude, 1.0);
    }

    #[test]
    fn ir_stats_delay_is_negative_when_peak_precedes_centre() {
        let window_len = 1024;
        let centre = window_len / 2;
        let r = ir_report_with_peak(window_len, centre - 10, 1.0, 0.0, 48_000);
        let stats = r.ir_stats().unwrap();
        assert_eq!(stats.delay_samples, -10);
        assert!(stats.arrival_s < 0.0);
    }

    #[test]
    fn ir_stats_pre_impulse_snr_reflects_noise_floor() {
        let window_len = 1024;
        let centre = window_len / 2;
        // Peak of 1.0 against a 0.01 floor -> 20*log10(100) = 40 dB.
        let r = ir_report_with_peak(window_len, centre, 1.0, 0.01, 48_000);
        let stats = r.ir_stats().unwrap();
        assert!(
            (stats.pre_impulse_snr_db - 40.0).abs() < 0.5,
            "pre_impulse_snr_db = {}",
            stats.pre_impulse_snr_db
        );
    }

    #[test]
    fn ir_stats_snr_is_infinite_over_true_silence() {
        let window_len = 1024;
        let centre = window_len / 2;
        let r = ir_report_with_peak(window_len, centre, 1.0, 0.0, 48_000);
        let stats = r.ir_stats().unwrap();
        assert!(stats.pre_impulse_snr_db.is_infinite());
    }

    #[test]
    fn ir_stats_falls_back_to_window_duration_when_no_gate_recorded() {
        let r = ir_report_with_peak(4_800, 2_400, 1.0, 0.0, 48_000);
        let stats = r.ir_stats().unwrap();
        // 4800 samples @ 48 kHz = 100 ms window -> f_low = 10 Hz.
        assert!((stats.gate_window_s - 0.1).abs() < 1e-12);
        assert!((stats.gate_f_low_hz - 10.0).abs() < 1e-9);
        // A legacy report's gate is inferred, and must say so — a reader
        // must not take "rectangular" here for a recorded fact.
        assert!(
            stats.gate_window_kind.contains("not recorded"),
            "inferred gate must be flagged: {}",
            stats.gate_window_kind
        );
    }

    /// The recorded gate wins over the `window_len / sample_rate` guess.
    /// This is the case that separates the two: a gate whose recorded
    /// `f_low_hz` and length disagree with what the IR length implies
    /// (a half-length gate on a zero-padded payload) — if `ir_stats`
    /// recomputed instead of reading, it would report 10 Hz, not 20.
    #[test]
    fn ir_stats_prefers_the_recorded_gate_over_the_ir_length() {
        let mut r = ir_report_with_peak(4_800, 2_400, 1.0, 0.0, 48_000);
        r.data[0].gate = Some(GateParams {
            gate_start_s: 0.0,
            gate_length_s: 0.05,
            window_kind: "half-hann".into(),
            f_low_hz: 20.0,
        });
        let stats = r.ir_stats().unwrap();
        assert!((stats.gate_window_s - 0.05).abs() < 1e-12);
        assert!((stats.gate_f_low_hz - 20.0).abs() < 1e-9);
        assert_eq!(stats.gate_window_kind, "half-hann");
    }

    /// [`ir_verdict`] direct, without a report around it: the threshold is
    /// a `<` on `PRE_IMPULSE_SNR_MIN_DB`, so a capture sitting exactly on
    /// the floor passes and one a hair under it fails.
    #[test]
    fn ir_verdict_threshold_is_inclusive_at_the_floor() {
        let floor = [0.0, 0.0, 0.0, 0.0];
        assert_eq!(
            ir_verdict(1.0, &floor, PRE_IMPULSE_SNR_MIN_DB),
            IrVerdict::Ok
        );
        assert!(matches!(
            ir_verdict(1.0, &floor, PRE_IMPULSE_SNR_MIN_DB - 0.001),
            IrVerdict::Failed { .. }
        ));
    }

    /// The two ways `snr_db` reaches `+inf` are not the same verdict: a
    /// silent floor under a real peak is the best possible capture, while
    /// an empty pre-impulse region means nothing was measured at all.
    #[test]
    fn ir_verdict_separates_a_silent_floor_from_an_unmeasured_one() {
        assert_eq!(ir_verdict(1.0, &[0.0, 0.0], f64::INFINITY), IrVerdict::Ok);
        assert!(matches!(
            ir_verdict(1.0, &[], f64::INFINITY),
            IrVerdict::Failed { .. }
        ));
    }

    /// A zero peak fails ahead of every other branch — an all-zero IR has
    /// no signal to compare a floor against, however clean the floor looks.
    #[test]
    fn ir_verdict_fails_a_zero_peak_before_reading_the_snr() {
        assert!(matches!(
            ir_verdict(0.0, &[0.0, 0.0], f64::INFINITY),
            IrVerdict::Failed { .. }
        ));
    }

    #[test]
    fn ir_stats_verdict_ok_when_snr_clears_the_threshold() {
        let window_len = 1024;
        let centre = window_len / 2;
        // Peak 1.0 against a 0.1 floor -> 20*log10(10) = 20 dB, above the
        // 18.0 dB threshold.
        let r = ir_report_with_peak(window_len, centre, 1.0, 0.1, 48_000);
        let stats = r.ir_stats().unwrap();
        assert_eq!(stats.verdict, IrVerdict::Ok);
    }

    #[test]
    fn ir_stats_verdict_failed_when_snr_is_below_the_threshold() {
        let window_len = 1024;
        let centre = window_len / 2;
        // Peak 1.0 against a 0.2 floor -> 20*log10(5) \u{2248} 14.0 dB,
        // below the 18.0 dB threshold — the #376 failure shape: a plausible
        // number, but a noise-floor-scale peak.
        let r = ir_report_with_peak(window_len, centre, 1.0, 0.2, 48_000);
        let stats = r.ir_stats().unwrap();
        assert_eq!(
            stats.verdict,
            IrVerdict::Failed {
                reason: "pre-impulse SNR below threshold".to_string()
            }
        );
    }

    #[test]
    fn ir_stats_verdict_ok_on_a_perfectly_clean_capture() {
        // A zero floor against a nonzero peak is +inf SNR, but it is the
        // *best* possible capture, not an unmeasurable one — the floor was
        // measured, and it measured to exactly zero. This must not be
        // confused with a genuine failure (#387 QA correctness #1).
        let window_len = 1024;
        let centre = window_len / 2;
        let r = ir_report_with_peak(window_len, centre, 1.0, 0.0, 48_000);
        let stats = r.ir_stats().unwrap();
        assert!(stats.pre_impulse_snr_db.is_infinite());
        assert_eq!(stats.verdict, IrVerdict::Ok);
    }

    #[test]
    fn ir_stats_verdict_failed_when_nothing_was_captured() {
        // Peak magnitude itself is zero -> the whole linear IR is zero,
        // i.e. there is no signal to compare a floor against at all. This
        // is the genuine "no measurable floor" failure, distinct from the
        // clean-capture case above.
        let window_len = 1024;
        let r = ir_report_with_peak(window_len, window_len / 2, 0.0, 0.0, 48_000);
        let stats = r.ir_stats().unwrap();
        assert_eq!(
            stats.verdict,
            IrVerdict::Failed {
                reason: "no signal captured (linear IR is all zero)".to_string()
            }
        );
    }

    #[test]
    fn ir_stats_verdict_failed_when_guard_band_consumes_the_whole_pre_region() {
        // Peak sits inside the guard band from the start of the window, so
        // `pre_region` is empty — there is no data at all to measure a
        // floor from, regardless of what the peak itself looks like.
        let window_len = 1024;
        let r = ir_report_with_peak(window_len, 3, 1.0, 0.1, 48_000);
        let stats = r.ir_stats().unwrap();
        assert_eq!(
            stats.verdict,
            IrVerdict::Failed {
                reason: "no measurable pre-impulse floor (peak too close to \
                         the start of the gated window)"
                    .to_string()
            }
        );
    }

    #[test]
    fn ir_stats_none_for_non_impulse_response_report() {
        let r = sample_report(); // FrequencyResponse variant
        assert!(r.ir_stats().is_none());
    }

    /// A Farina capture emits several payloads; the impulse response is
    /// not necessarily first. `ir_stats` must find it rather than read
    /// `data[0]` and give up.
    #[test]
    fn ir_stats_finds_the_ir_payload_behind_another_payload() {
        let mut r = ir_report_with_peak(1_024, 1_024 / 2 + 32, 1.0, 0.0, 48_000);
        let ir_payload = r.data.remove(0);
        r.data = vec![
            MeasurementPayload {
                data: MeasurementData::FrequencyResponse { points: vec![] },
                standard: Vec::new(),
                gate: None,
            },
            ir_payload,
        ];
        assert_eq!(r.ir_stats().unwrap().delay_samples, 32);
    }

    #[test]
    fn ir_stats_none_for_empty_linear_ir() {
        let mut r = sample_impulse_response_report();
        r.data = vec![MeasurementPayload {
            data: MeasurementData::ImpulseResponse {
                sample_rate_hz: 48_000,
                f1_hz: 20.0,
                f2_hz: 20_000.0,
                duration_s: 1.0,
                linear_ir: vec![],
                noise_tail_start_s: None,
                harmonics: vec![],
            },
            standard: Vec::new(),
            gate: None,
        }];
        assert!(r.ir_stats().is_none());
    }

    // ─── interface latency (τ), archived alongside the arrival ─────────

    /// #378 contingency (AC6 rig run, 2026-09-15), kept by #346 for the
    /// unbounded case: with no causal bound `delay_samples` / `arrival_s`
    /// are peak-derived, and the onset is still computed and carried as a
    /// diagnostic. Tested against the rejected wiring — a
    /// synthetic multi-way-like IR where sustained onset energy sits well
    /// before the peak, so an arrival still taken from the onset would
    /// differ from the peak-derived value asserted here.
    #[test]
    fn ir_stats_arrival_is_peak_derived_onset_is_diagnostic() {
        let window_len = 1024;
        let centre = window_len / 2;
        let noise = 0.001;
        let peak_true = centre + 100;
        let onset_true = peak_true - 20; // within the guard band before the peak
        let mut ir: Vec<f64> = vec![noise; window_len];
        for v in ir.iter_mut().take(peak_true + 1).skip(onset_true) {
            *v = 0.3;
        }
        ir[peak_true] = 1.0;

        let peak_index = ir
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.abs().partial_cmp(&b.1.abs()).unwrap())
            .unwrap()
            .0;
        assert_eq!(
            peak_index, peak_true,
            "test setup: peak must be at peak_true"
        );

        let r = ir_report_with_custom_ir(ir, 48_000);
        let stats = r.ir_stats().unwrap();
        assert_eq!(stats.peak_index, peak_true);
        assert_eq!(
            stats.onset_index, onset_true,
            "the onset is still estimated and carried as a diagnostic"
        );
        assert_ne!(
            stats.delay_samples,
            onset_true as i64 - centre as i64,
            "arrival must not be the onset-derived delay — #378 contingency"
        );
        assert_eq!(stats.delay_samples, peak_true as i64 - centre as i64);
        assert!((stats.arrival_s - (peak_true as f64 - centre as f64) / 48_000.0).abs() < 1e-12);
        assert!(stats.onset_rule.contains("no causal bound"));
        assert_eq!(stats.arrival_source, BAND_LIMITED);
        assert_eq!(stats.arrival_index, stats.peak_index);
        assert_eq!(stats.onset_standing, OnsetStanding::NoCausalBound);
    }

    /// Noise in ±1e-4, from a fixed hash so the fixture is reproducible.
    fn hashed_noise(window_len: usize) -> Vec<f64> {
        (0..window_len)
            .map(|i| {
                let mut s = (i as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
                s ^= s >> 29;
                ((s >> 11) as f64 / (1u64 << 53) as f64 - 0.5) * 2e-4
            })
            .collect()
    }

    /// #346 acceptance criterion 2, tested against the rejected
    /// implementation: a multi-way-like IR — sustained energy from the
    /// wavefront to a peak 20 samples later, as on pupu at 2 m — with a
    /// measured same-capture reference and a distance whose causal bound
    /// sits 20 samples before the wavefront, so every onset condition holds
    /// and the pick is the wavefront. The rejected implementation — the
    /// promotion withdrawn by #346 architect revision 4 — is that bounded
    /// onset as the arrival. It is computed inline and must not be what
    /// `ir_stats` reports; the peak is.
    #[test]
    fn ir_stats_keeps_the_peak_arrival_over_an_unscored_onset() {
        let window_len = 1024;
        let sr = 96_000u32;
        let centre = window_len / 2;
        let peak_true = centre + 200;
        let wavefront = peak_true - 20;
        let bound_index = wavefront - 20;
        let mut ir = hashed_noise(window_len);
        for v in ir.iter_mut().take(peak_true).skip(wavefront) {
            *v += 0.3;
        }
        ir[peak_true] = 1.0;

        let (argmax, _) = crate::measurement::sweep::ir_peak(&ir);
        assert_eq!(argmax, peak_true, "test setup");

        let mut r = ir_report_with_custom_ir(ir.clone(), sr);
        let c = crate::shared::conversions::speed_of_sound_from_config(Some(20.0));
        r.position = Some(PositionSnapshot {
            temperature_c: Some(20.0),
            distance_m: Some((bound_index - centre) as f64 / sr as f64 * c),
            ..Default::default()
        });
        r.reference_latency = Some(measured_reference(0.0));

        let stats = r.ir_stats().unwrap();
        assert_eq!(stats.verdict, IrVerdict::Ok, "test setup");
        assert_eq!(
            stats.causal_bound.min_admissible_index(),
            Some(bound_index),
            "test setup"
        );
        assert_eq!(stats.onset_standing, OnsetStanding::Unscored, "test setup");

        // The rejected implementation: the bounded onset as the arrival.
        let rejected = crate::measurement::sweep::estimate_onset(
            &ir,
            argmax,
            sr,
            onset_floor(pre_impulse_region(&ir, argmax)),
            &stats.causal_bound,
        );
        assert_eq!(rejected.index, wavefront, "test setup");
        assert_eq!(stats.onset_index, wavefront, "test setup");
        assert_ne!(wavefront, peak_true, "test setup");

        assert_eq!(stats.arrival_source, BAND_LIMITED);
        assert_eq!(stats.arrival_index, stats.peak_index);
        assert_eq!(stats.delay_samples, peak_true as i64 - centre as i64);
        assert_ne!(
            stats.delay_samples,
            rejected.index as i64 - centre as i64,
            "the arrival must not be the bounded onset — #346 architect revision 4"
        );
        assert!((stats.arrival_s - stats.delay_samples as f64 / sr as f64).abs() < 1e-15);
    }

    /// #346: flight time is the peak arrival minus the peak-picked latency
    /// (#544: the live reference, here with the capture pair as the
    /// reference pair), even when every onset condition holds. The
    /// onset-derived value is computed inline and must differ.
    #[test]
    fn ir_stats_flight_time_subtracts_tau_from_the_peak_over_an_unscored_onset() {
        let window_len = 1024;
        let sr = 96_000u32;
        let centre = window_len / 2;
        let peak_true = centre + 200;
        let wavefront = peak_true - 20;
        let bound_index = wavefront - 20;
        let mut ir = hashed_noise(window_len);
        for v in ir.iter_mut().take(peak_true).skip(wavefront) {
            *v += 0.3;
        }
        ir[peak_true] = 1.0;
        let mut r = ir_report_with_custom_ir(ir, sr);
        let c = crate::shared::conversions::speed_of_sound_from_config(Some(20.0));
        // The bound sits at `centre + τ + d/c`, so d is shortened by τ to
        // keep it at `bound_index`.
        r.position = Some(PositionSnapshot {
            temperature_c: Some(20.0),
            distance_m: Some((bound_index - centre - 30) as f64 / sr as f64 * c),
            ..Default::default()
        });
        let tau_s = 30.0 / sr as f64;
        with_live_latency(&mut r, tau_s);

        let stats = r.ir_stats().unwrap();
        assert_eq!(stats.onset_standing, OnsetStanding::Unscored, "test setup");
        assert_eq!(stats.onset_index, wavefront, "test setup");
        assert_eq!(stats.arrival_source, BAND_LIMITED);
        assert_eq!(stats.arrival_index, stats.peak_index);
        let ft = stats.flight_time_s.expect("flight time");
        let peak_ft = (peak_true as f64 - centre as f64) / sr as f64 - tau_s;
        let onset_ft = (wavefront as f64 - centre as f64) / sr as f64 - tau_s;
        assert!((ft - peak_ft).abs() < 1e-15, "{ft} vs {peak_ft}");
        assert!(
            (ft - onset_ft).abs() > 1e-6,
            "flight time must not come from the onset"
        );
    }

    /// Each onset condition, failed alone, names itself — in the order
    /// `onset_standing` checks them — and all holding is `Unscored`.
    #[test]
    fn onset_standing_names_the_first_failed_condition() {
        use crate::measurement::sweep::{
            BoundInputs, EdgeGuard, MissingBoundInput, OnsetEstimate, OnsetPick, WindowLimit,
        };
        let enforced = CausalBound::Enforced {
            index: 100,
            inputs: BoundInputs {
                reference_tau_s: 0.0,
                distance_m: 1.0,
                speed_of_sound_m_s: 343.0,
                temperature_c: None,
            },
        };
        let unbounded = CausalBound::Unavailable(MissingBoundInput::Distance);
        let picked = |limit, pinned, edge_guard| OnsetEstimate {
            index: 110,
            rule: String::new(),
            pick: OnsetPick::Picked {
                window_start: 100,
                limit,
                pinned,
                edge_guard,
            },
        };
        let good = picked(WindowLimit::CausalBound, false, Some(EdgeGuard::Passed));
        let declined = OnsetEstimate {
            index: 120,
            rule: String::new(),
            pick: OnsetPick::Declined,
        };
        let failed = IrVerdict::Failed {
            reason: "pre-impulse SNR below threshold".into(),
        };
        assert_eq!(
            onset_standing(&enforced, &good, &IrVerdict::Ok),
            OnsetStanding::Unscored
        );
        let cases = [
            (
                &unbounded,
                good.clone(),
                IrVerdict::Ok,
                OnsetStanding::NoCausalBound,
            ),
            (
                &enforced,
                picked(WindowLimit::SearchSpan, false, Some(EdgeGuard::Passed)),
                IrVerdict::Ok,
                OnsetStanding::BoundNotBinding,
            ),
            (
                &enforced,
                declined,
                IrVerdict::Ok,
                OnsetStanding::PickerDeclined,
            ),
            (
                &enforced,
                picked(WindowLimit::CausalBound, true, None),
                IrVerdict::Ok,
                OnsetStanding::PickOnWindowStart,
            ),
            (
                &enforced,
                picked(
                    WindowLimit::CausalBound,
                    false,
                    Some(EdgeGuard::Failed { repick: Some(117) }),
                ),
                IrVerdict::Ok,
                OnsetStanding::EdgeFollowing { repick: Some(117) },
            ),
            (
                &enforced,
                picked(
                    WindowLimit::CausalBound,
                    false,
                    Some(EdgeGuard::Failed { repick: None }),
                ),
                IrVerdict::Ok,
                OnsetStanding::EdgeFollowing { repick: None },
            ),
            (
                &enforced,
                picked(WindowLimit::CausalBound, false, None),
                IrVerdict::Ok,
                OnsetStanding::EdgeFollowing { repick: None },
            ),
            (
                &enforced,
                good.clone(),
                failed,
                OnsetStanding::DeconvolutionFailed,
            ),
        ];
        for (bound, onset, verdict, standing) in cases {
            assert_eq!(
                onset_standing(bound, &onset, &verdict),
                standing,
                "{standing:?}"
            );
        }
    }

    /// End to end through `ir_stats`: a bound inside the wavefront's rise
    /// leaves a homogeneous window, the edge guard fires, and the onset
    /// standing names it — the bound, not the IR, set that pick. The
    /// arrival is the peak regardless.
    #[test]
    fn ir_stats_keeps_the_peak_when_the_pick_follows_the_window_edge() {
        let window_len = 1024;
        let sr = 96_000u32;
        let centre = window_len / 2;
        let peak_true = centre + 300;
        let onset_true = peak_true - 110;
        let bound_index = onset_true + 50;
        let mut ir = hashed_noise(window_len);
        let rise = (peak_true - onset_true) as f64;
        for (i, v) in ir.iter_mut().enumerate() {
            if (onset_true..=peak_true).contains(&i) {
                let x = (i - onset_true) as f64 / rise;
                *v += x * x;
            }
        }
        let mut r = ir_report_with_custom_ir(ir, sr);
        let c = crate::shared::conversions::speed_of_sound_from_config(Some(20.0));
        r.position = Some(PositionSnapshot {
            temperature_c: Some(20.0),
            distance_m: Some((bound_index - centre) as f64 / sr as f64 * c),
            ..Default::default()
        });
        r.reference_latency = Some(measured_reference(0.0));

        let stats = r.ir_stats().unwrap();
        assert_eq!(
            stats.causal_bound.min_admissible_index(),
            Some(bound_index),
            "test setup"
        );
        assert!(
            stats.onset_index > bound_index,
            "test setup: pick must be clear of the window start, got {}",
            stats.onset_index
        );
        assert!(
            matches!(
                stats.onset_standing,
                OnsetStanding::EdgeFollowing { repick: Some(r) }
                    if r.abs_diff(stats.onset_index) > 1
            ),
            "{:?}: {}",
            stats.onset_standing,
            stats.onset_rule
        );
        assert_eq!(stats.arrival_source, BAND_LIMITED);
        assert_eq!(stats.arrival_index, stats.peak_index);
        assert_eq!(stats.delay_samples, stats.peak_index as i64 - centre as i64);
    }

    /// #346 / #460: when a report carries both a same-capture reference latency and
    /// a recorded `position.distance_m`, `ir_stats` must convert them
    /// into a causal bound and enforce it — proven with a capture whose
    /// unbounded answer is non-causal, so the bound actively changes the
    /// result rather than agreeing with it by coincidence.
    ///
    /// #378 changed how: the bound is the search window's lower limit,
    /// so the non-causal candidate is outside the picker's reach rather
    /// than being clamped after the fact. What is asserted is the same
    /// requirement — a bounded capture never reads earlier than pure
    /// flight time allows — expressed against a window instead of a
    /// walk.
    #[test]
    fn ir_stats_wires_a_causal_bound_from_position_and_reference_latency() {
        let window_len = 1024;
        let sr = 48_000u32;
        let centre = window_len / 2;
        let peak_true = centre + 100;
        let bound_index = peak_true - 12;
        let wavefront = peak_true - 7; // inside the admissible window
        let pre_ring = peak_true - 52; // below it — non-causal
        let mut ir: Vec<f64> = (0..window_len)
            .map(|i| {
                let mut s = (i as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
                s ^= s >> 29;
                ((s >> 11) as f64 / (1u64 << 53) as f64 - 0.5) * 2e-4
            })
            .collect();
        for (i, v) in ir.iter_mut().enumerate() {
            if (pre_ring..wavefront).contains(&i) {
                *v += 0.02 * ((i - pre_ring) as f64 * 0.7).sin();
            } else if (wavefront..=peak_true).contains(&i) {
                *v += 0.3 * (i - wavefront + 1) as f64 / 8.0;
            }
        }
        ir[peak_true] = 1.0;

        let mut r = ir_report_with_custom_ir(ir, sr);
        let unbounded = r.ir_stats().unwrap();
        assert!(
            unbounded.onset_index < bound_index,
            "test setup: the unbounded pick must be non-causal, got {}",
            unbounded.onset_index
        );
        assert!(unbounded.onset_rule.contains("no causal bound"));

        let bound_offset_samples = (bound_index - centre) as f64;
        let c = crate::shared::conversions::speed_of_sound_from_config(Some(20.0));
        r.position = Some(PositionSnapshot {
            temperature_c: Some(20.0),
            distance_m: Some(bound_offset_samples / sr as f64 * c),
            ..Default::default()
        });
        r.reference_latency = Some(measured_reference(0.0));

        let bounded = r.ir_stats().unwrap();
        assert_eq!(
            bounded.causal_bound.min_admissible_index(),
            Some(bound_index)
        );
        assert!(
            bounded.onset_index >= bound_index,
            "causal bound must exclude the non-causal candidate, got {}",
            bounded.onset_index
        );
        assert_ne!(bounded.onset_index, unbounded.onset_index);
        assert!(bounded.onset_rule.contains("causal bound enforced"));
        assert!(bounded
            .onset_rule
            .contains(&format!("window start at sample {bound_index}")));
    }

    /// #460 AC6, tested against the rejected implementation: a report that
    /// carries a *stored* measured τ and a distance, but no same-capture
    /// reference, must not get a bound. The bound the stored-τ wiring would
    /// have built is computed here, and must not be the window start.
    #[test]
    fn ir_stats_never_builds_the_bound_from_stored_interface_latency() {
        let window_len = 1024;
        let sr = 48_000u32;
        let centre = window_len / 2;
        let peak_true = centre + 100;
        let mut ir: Vec<f64> = (0..window_len)
            .map(|i| {
                let mut s = (i as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
                s ^= s >> 29;
                ((s >> 11) as f64 / (1u64 << 53) as f64 - 0.5) * 2e-4
            })
            .collect();
        for (i, v) in ir
            .iter_mut()
            .enumerate()
            .take(peak_true + 1)
            .skip(peak_true - 7)
        {
            *v += 0.3 * (i + 8 - peak_true) as f64 / 8.0;
        }
        ir[peak_true] = 1.0;

        let mut r = ir_report_with_custom_ir(ir, sr);
        let distance_m = 0.05;
        r.position = Some(PositionSnapshot {
            temperature_c: Some(20.0),
            distance_m: Some(distance_m),
            ..Default::default()
        });
        let stored_tau_s = 0.001;
        r.interface_latency = Some(measured_tau(stored_tau_s));
        r.reference_latency = None;

        // The rejected wiring: the pre-#460 bound built from the stored τ.
        let c = crate::shared::conversions::speed_of_sound_from_config(Some(20.0));
        let stored_bound =
            (centre as f64 + (stored_tau_s + distance_m / c) * sr as f64).round() as usize;
        assert!(
            stored_bound < peak_true,
            "test setup: the stored-τ bound must be admissible, got {stored_bound}"
        );

        let stats = r.ir_stats().unwrap();
        assert_eq!(
            stats.causal_bound.min_admissible_index(),
            None,
            "a stored τ must never enforce the bound"
        );
        assert!(
            matches!(
                stats.causal_bound,
                CausalBound::Unavailable(MissingBoundInput::ReferenceLatency { .. })
            ),
            "{:?}",
            stats.causal_bound
        );
        assert!(
            stats
                .onset_rule
                .contains("no causal bound (reference latency unavailable)"),
            "{}",
            stats.onset_rule
        );
        assert!(
            !stats
                .onset_rule
                .contains(&format!("window start at sample {stored_bound}")),
            "the stored-τ bound set the window: {}",
            stats.onset_rule
        );
    }

    /// #346 (QA on #352, correctness issue 2) / #353: `floor_rms`'s guard
    /// band (`(window_len / 32).max(8)`, pre-existing) is sized off
    /// `window_len` alone — nothing ties it to how wide the peak's own
    /// sustained-energy run actually is. When that run is wider than the
    /// guard, it bleeds into `pre_region` and inflates `floor_rms`. Before
    /// #353, `estimate_onset` thresholded directly off that inflated
    /// value and the backward search stopped at the peak instead of the
    /// true onset — silently reproducing the very peak-as-arrival bug
    /// #346 exists to fix, for a signal shape the guard band was not
    /// sized for.
    ///
    /// #353 (architect revision 2, option A′) fixes this: `estimate_onset`
    /// now thresholds against a median-based floor over the same
    /// `pre_region`, which a lobe this size (contaminating 52/152 ≈ 34% of
    /// `pre_region`, comfortably under the estimator's 50% breakdown
    /// point) does not move. This test — a small window (guard = 8,
    /// `window_len` = 256) with a sustained run wide enough to defeat the
    /// old RMS floor — now asserts the corrected behaviour.
    #[test]
    fn ir_stats_onset_threshold_is_coupled_to_the_guard_band_a_wide_lobe_can_break() {
        let window_len = 256;
        let sr = 48_000u32;
        let noise = 0.001;
        let peak_true = 160;
        let onset_true = 100; // 60 samples wide — wider than guard = 8
        let mut ir = vec![noise; window_len];
        for v in ir.iter_mut().take(peak_true + 1).skip(onset_true) {
            *v = 0.3;
        }
        ir[peak_true] = 1.0;

        let r = ir_report_with_custom_ir(ir, sr);
        let stats = r.ir_stats().unwrap();
        // The median floor (#353) is not moved by this lobe: the backward
        // walk now reaches the true onset instead of stopping at the peak.
        assert_ne!(
            stats.onset_index, peak_true,
            "if this now equals peak_true, the median-floor fix (#353) has \
             regressed to the old RMS-floor behaviour — update this test's \
             assertions and its doc comment"
        );
        assert_eq!(
            stats.onset_index, onset_true,
            "median floor (#353) has a 50% breakdown point; this lobe \
             contaminates only ~34% of pre_region, well under it, so the \
             backward walk must reach the true onset"
        );
    }

    /// The rule #378 rejects, computed inline so these tests measure it
    /// rather than asserting "the new answer is closer to truth": a
    /// threshold 12 dB above the supplied pre-impulse floor, walked
    /// backward from the peak. Nothing in `ac-core` implements this any
    /// more — it exists only here, as the comparison.
    fn rejected_level_crossing_rule(ir: &[f64], peak_index: usize, floor: f64) -> usize {
        let threshold = floor * 10f64.powf(12.0 / 20.0);
        let mut onset = peak_index;
        while onset > 0 && ir[onset - 1].abs() > threshold {
            onset -= 1;
        }
        onset
    }

    /// #353's median floor, recomputed here over an arbitrary region so
    /// the rejected rule can be driven by either floor.
    fn median_abs_over_phi_inv(region: &[f64]) -> f64 {
        if region.is_empty() {
            return 0.0;
        }
        let mut abs_vals: Vec<f64> = region.iter().map(|v| v.abs()).collect();
        abs_vals.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let n = abs_vals.len();
        let median = if n % 2 == 1 {
            abs_vals[n / 2]
        } else {
            (abs_vals[n / 2 - 1] + abs_vals[n / 2]) / 2.0
        };
        median / 0.674_489_750_196_081_7
    }

    /// #353 acceptance criterion 3, carried into #378: names the input
    /// that makes the *rejected* level-crossing rule go red under both
    /// the RMS floor and the median floor (option A′), per the
    /// architect's revision-2 probe table (2026-08-23) — bracketing the
    /// crossings, not locating them to the sample, exactly as that
    /// comment states. All cases share `window_len = 256`,
    /// `peak_true = 160`, `guard = 8` (so `pre_end = 152`); `width` is
    /// `peak_true - onset_true`, i.e. how far the sustained run reaches
    /// back from the peak.
    ///
    /// Both rejected rules are computed inline against each input rather
    /// than trusted from an earlier revision (`test-against-the-rejected-
    /// implementation`). #353's own evidence is kept intact — the RMS
    /// floor breaks first, the median floor survives to its 50%
    /// breakdown point — and #378's claim is added on top: the shipped
    /// picker takes no threshold from either floor, so contamination
    /// that moves both of them does not move its answer at all.
    #[test]
    fn ir_stats_onset_floor_breakdown_point_matches_the_measured_table() {
        let window_len = 256;
        let sr = 48_000u32;
        let noise = 0.001;
        let peak_true = 160;
        let guard = (window_len / 32).max(8); // 8

        // (lobe width, RMS-floor level rule reaches onset_true?,
        //  median-floor level rule reaches onset_true?)
        let cases = [
            (16, true, true),   // RMS: 5% contaminated, green
            (18, false, true),  // RMS: 6.6% contaminated, red; median: green
            (80, false, true),  // RMS: red; median: 47% contaminated, green
            (90, false, false), // median: 54% contaminated, past 50% — red
        ];

        for (width, rms_reaches_onset, median_reaches_onset) in cases {
            let onset_true = peak_true - width;
            let mut ir = vec![noise; window_len];
            for v in ir.iter_mut().take(peak_true + 1).skip(onset_true) {
                *v = 0.3;
            }
            ir[peak_true] = 1.0;

            let pre_end = peak_true.saturating_sub(guard);
            let pre_region = &ir[..pre_end];
            let rms_floor = {
                let mean_sq =
                    pre_region.iter().map(|v| v * v).sum::<f64>() / pre_region.len() as f64;
                mean_sq.sqrt()
            };
            let median_floor = median_abs_over_phi_inv(pre_region);

            assert_eq!(
                rejected_level_crossing_rule(&ir, peak_true, rms_floor) == onset_true,
                rms_reaches_onset,
                "width {width}: RMS-floor level rule onset = {}, expected \
                 reaches_onset={rms_reaches_onset}",
                rejected_level_crossing_rule(&ir, peak_true, rms_floor)
            );
            assert_eq!(
                rejected_level_crossing_rule(&ir, peak_true, median_floor) == onset_true,
                median_reaches_onset,
                "width {width}: median-floor level rule onset = {}, expected \
                 reaches_onset={median_reaches_onset}",
                rejected_level_crossing_rule(&ir, peak_true, median_floor)
            );

            let r = ir_report_with_custom_ir(ir, sr);
            let stats = r.ir_stats().unwrap();
            assert_eq!(
                stats.onset_index, onset_true,
                "width {width}: #378's picker takes no threshold from either \
                 floor, so guard-band contamination that moves both rejected \
                 rules must not move it"
            );
        }
    }

    /// #353 acceptance criterion 5, carried into #378: the guard stays
    /// fixed and content-independent (option A′ added a distribution
    /// property, not a second constant coupled to the first) — asserted
    /// as an estimator-vs-estimator agreement rather than a hidden
    /// tolerance between the two floors, per the architect's own
    /// instruction ("do not convert that into a dB tolerance on the two
    /// floors; that number has not been measured and would ship as
    /// `assumed`"). On a clean pre-impulse region the RMS floor and the
    /// median floor must send the rejected level rule to the *same*
    /// onset index — checked over many seeded noise realisations so this
    /// is a statement, not a coincidence (architect's own probe: 200
    /// seeds, `{0: 200}`).
    ///
    /// #378 adds the stronger claim on the same 200 captures: the
    /// shipped picker's answer is identical whichever floor is handed to
    /// it, because the floor is a validity gate now and not a threshold.
    #[test]
    fn ir_stats_onset_floor_agrees_with_rms_floor_on_clean_pre_impulse_noise() {
        use rand::rngs::StdRng;
        use rand::{Rng, SeedableRng};

        let window_len: usize = 1024;
        let sr = 48_000u32;
        let peak_true: usize = 700;
        let onset_true = peak_true - 20; // within the guard band before the peak
        let guard = (window_len / 32).max(8);
        assert!(
            onset_true >= peak_true.saturating_sub(guard),
            "test setup: run must stay inside the guard band so both floors \
             see clean noise only"
        );

        for seed in 0u64..200 {
            let mut rng = StdRng::seed_from_u64(0xB353_0000 + seed);
            let mut ir: Vec<f64> = (0..window_len)
                .map(|_| (rng.gen::<f64>() * 2.0 - 1.0) * 0.001)
                .collect();
            for v in ir.iter_mut().take(peak_true + 1).skip(onset_true) {
                *v = 0.3;
            }
            ir[peak_true] = 1.0;

            let pre_end = peak_true.saturating_sub(guard);
            let pre_region = &ir[..pre_end];
            let rms_floor = {
                let mean_sq =
                    pre_region.iter().map(|v| v * v).sum::<f64>() / pre_region.len() as f64;
                mean_sq.sqrt()
            };
            let median_floor = median_abs_over_phi_inv(pre_region);

            assert_eq!(
                rejected_level_crossing_rule(&ir, peak_true, rms_floor),
                rejected_level_crossing_rule(&ir, peak_true, median_floor),
                "seed {seed}: median floor (#353) must agree with the RMS \
                 floor on an uncontaminated pre-impulse region"
            );

            let picked_with_rms = crate::measurement::sweep::estimate_onset(
                &ir,
                peak_true,
                sr,
                rms_floor,
                &CausalBound::Unavailable(MissingBoundInput::Both {
                    reference_reason: String::new(),
                }),
            );
            let r = ir_report_with_custom_ir(ir, sr);
            let stats = r.ir_stats().unwrap();
            assert_eq!(
                stats.onset_index, picked_with_rms.index,
                "seed {seed}: #378's pick must not depend on which floor \
                 reaches the validity gate"
            );
        }
    }

    // ─── #359: arrival_check / flight_time_s ─────────────────────────────

    /// #544 AC9 (4), inverting the #359 test that asserted the rejected
    /// behaviour: with no reference loopback configured, a measured stored
    /// τ for the capture pair no longer produces a flight time. The rejected
    /// rule (`arrival − stored τ`, 0.25 ms − 0.1 ms) is computed and shown to
    /// give one, so this test can fail.
    #[test]
    fn no_reference_configured_withholds_the_flight_time_despite_a_stored_tau() {
        // #537: 20 kHz, so Nyquist leaves the band an octave above the
        // arrival's 2 kHz corner (at 4 kHz the flight time is withheld).
        let sr = 20_000u32;
        let window_len = 1024;
        let centre = window_len / 2;
        // delay_samples = 5 -> arrival_s = 0.25 ms at 20 kHz.
        let mut r = ir_report_with_peak(window_len, centre + 5, 1.0, 0.0, sr);
        r.interface_latency = Some(measured_tau(0.0001)); // 0.1 ms
        r.reference_latency = Some(ReferenceLatency::Unavailable {
            reason: "no reference configured (ac setup reference)".into(),
        });
        r.inter_pair_offset = Some(InterPairOffset::NotConfigured);
        let stats = r.ir_stats().unwrap();
        assert!(
            matches!(stats.arrival_check, ArrivalCheck::Unchecked { .. }),
            "{:?}",
            stats.arrival_check
        );
        assert_eq!(
            stats.latency_basis,
            LatencyBasis::Withheld(WithheldBasis::NoReference)
        );
        assert_eq!(stats.flight_time_s, None);

        let rejected = match &r.interface_latency {
            Some(InterfaceLatency::Measured(m)) => Some(stats.arrival_s - m.tau_s),
            _ => None,
        };
        let rejected_ms = rejected.expect("the stored-τ rule gives a value") * 1000.0;
        assert!((rejected_ms - 0.15).abs() < 1e-9, "{rejected_ms}");
    }

    /// A 96 kHz IR with a single spike at `centre + arrival_samples`.
    fn spike_at(arrival_samples: usize) -> MeasurementReport {
        let window_len = 8192;
        ir_report_with_peak(
            window_len,
            window_len / 2 + arrival_samples,
            1.0,
            0.0,
            96_000,
        )
    }

    /// #544 AC9 (1), tested against the rejected rule. The transport moves
    /// the reference leg 1711 → 1535 (the 2026-09-21 values) and the
    /// arrival with it, since both pairs share it. The offset is 0 and the
    /// stored τ of both pairs stays 1711. The live flight time equals the
    /// one from an unmoved reference; the rejected `arrival − stored τ` is
    /// 176 samples off it.
    #[test]
    fn a_moved_reference_leaves_the_flight_time_unchanged() {
        let sr = 96_000.0;
        let flight = 335usize;
        let stored = 1711usize;
        let capture = |live: usize| {
            let mut r = spike_at(live + flight);
            r.interface_latency = Some(measured_tau(stored as f64 / sr));
            r.reference_latency = Some(measured_reference(live as f64 / sr));
            r.reference_stored_latency = Some(stored_reference_tau(stored as f64 / sr, Some(256)));
            r.inter_pair_offset = Some(measured_offset(0.0));
            r
        };
        let unmoved = capture(1711).ir_stats().unwrap();
        let moved_report = capture(1535);
        let moved = moved_report.ir_stats().unwrap();
        assert_eq!(unmoved.arrival_check, ArrivalCheck::Agree);
        assert!(
            matches!(moved.arrival_check, ArrivalCheck::Mismatch(ref d) if d.delta_samples == -176),
            "the drift readout names the move: {:?}",
            moved.arrival_check
        );
        let ft_unmoved = unmoved.flight_time_s.expect("unmoved: produced");
        let ft_moved = moved
            .flight_time_s
            .expect("a moved reference withholds nothing");
        assert_eq!((ft_unmoved * sr).round(), flight as f64);
        assert_eq!((ft_moved * sr).round(), flight as f64);

        let Some(InterfaceLatency::Measured(m)) = &moved_report.interface_latency else {
            unreachable!()
        };
        let rejected = moved.arrival_s - m.tau_s;
        assert_eq!(((ft_moved - rejected) * sr).round(), 176.0);
    }

    /// #544: a non-zero offset keeps its sign. The measurement pair's path
    /// is 46 samples longer than the reference's (README's measured
    /// converter-channel asymmetry), so the arrival is 46 samples later and
    /// the flight time does not move. Subtracting the offset with the wrong
    /// sign would put it 92 samples off.
    #[test]
    fn a_positive_offset_is_subtracted_with_the_reference() {
        let sr = 96_000.0;
        let (live, offset, flight) = (1711usize, 46usize, 335usize);
        let mut r = spike_at(live + offset + flight);
        r.reference_latency = Some(measured_reference(live as f64 / sr));
        r.inter_pair_offset = Some(measured_offset(offset as f64 / sr));
        let stats = r.ir_stats().unwrap();
        let ft = stats.flight_time_s.expect("produced");
        assert_eq!((ft * sr).round(), flight as f64);
        let wrong_sign = stats.arrival_s - (live as f64 - offset as f64) / sr;
        assert_eq!(((wrong_sign - ft) * sr).round(), 92.0);
        match stats.latency_basis {
            LatencyBasis::Live {
                offset: LiveOffset::Measured { offset_s, .. },
                ..
            } => assert_eq!((offset_s * sr).round(), 46.0),
            ref other => panic!("{other:?}"),
        }
    }

    /// #544 AC9 (2): a reference leg without a valid reading withholds the
    /// flight time — even with a measured offset and a stored τ in the
    /// report — and the basis carries the reference's reason.
    #[test]
    fn an_unavailable_reference_withholds_the_flight_time_and_names_why() {
        let mut r = spike_at(2046);
        r.interface_latency = Some(measured_tau(1711.0 / 96_000.0));
        let reason = "pre-impulse SNR 15.7 dB below the 25.5 dB derived floor";
        r.reference_latency = Some(ReferenceLatency::Unavailable {
            reason: reason.into(),
        });
        r.inter_pair_offset = Some(measured_offset(0.0));
        let stats = r.ir_stats().unwrap();
        assert_eq!(stats.flight_time_s, None);
        assert_eq!(
            stats.latency_basis,
            LatencyBasis::Withheld(WithheldBasis::ReferenceUnavailable {
                reason: reason.into()
            })
        );
    }

    /// #544 AC9 (3), the no-silent-fallback case: no offset on file, with a
    /// measured stored τ for the capture pair *and* a valid live reference
    /// in the report. Withheld, the pair named; the stored τ is not used.
    #[test]
    fn an_unmeasured_offset_withholds_the_flight_time_with_the_pair_named() {
        let sr = 96_000.0;
        let mut r = spike_at(2046);
        r.interface_latency = Some(measured_tau(1711.0 / sr));
        r.reference_latency = Some(measured_reference(1711.0 / sr));
        let reason = "[out0_in0] against ref [out1_in1]; \u{3c4} on file, reference leg not \
                      measured with it; check: `ac calibrate` on this pair, loopback in place";
        r.inter_pair_offset = Some(InterPairOffset::Unavailable {
            reason: reason.into(),
        });
        let stats = r.ir_stats().unwrap();
        assert_eq!(stats.flight_time_s, None);
        match &stats.latency_basis {
            LatencyBasis::Withheld(WithheldBasis::OffsetNotMeasured { reason }) => {
                assert!(reason.starts_with("[out0_in0] against ref [out1_in1]"))
            }
            other => panic!("{other:?}"),
        }
    }

    /// #544: a measured offset moves the causal bound by the offset; an
    /// identity offset leaves it where it was.
    #[test]
    fn a_measured_offset_moves_the_causal_bound() {
        let sr = 96_000.0;
        let mut r = spike_at(2046);
        r.position = Some(PositionSnapshot {
            temperature_c: Some(20.0),
            distance_m: Some(1.0),
            ..Default::default()
        });
        r.reference_latency = Some(measured_reference(1711.0 / sr));
        r.inter_pair_offset = Some(InterPairOffset::Identity);
        let identity = r.ir_stats().unwrap().causal_bound.min_admissible_index();
        r.inter_pair_offset = Some(measured_offset(46.0 / sr));
        let measured = r.ir_stats().unwrap().causal_bound.min_admissible_index();
        assert_eq!(measured.unwrap() - identity.unwrap(), 46);
    }

    /// #359 AC4: a same-capture reference reading exactly one period ahead
    /// of the stored reference τ is reported as a graph-buffering shift,
    /// naming the period — #347's own wording, reused via
    /// [`compare_tau_readings`] rather than reimplemented (AC2).
    #[test]
    fn arrival_check_detects_an_exact_period_shift_and_names_the_period() {
        let sr = 48_000u32;
        let period = 1024u32;
        let stored_tau_s = 0.0119;
        let same_capture_tau_s = stored_tau_s + period as f64 / sr as f64;
        let mut r = ir_report_with_peak(1024, 600, 1.0, 0.0, sr);
        r.reference_latency = Some(measured_reference(same_capture_tau_s));
        r.reference_stored_latency = Some(stored_reference_tau(stored_tau_s, Some(period)));
        let stats = r.ir_stats().unwrap();
        match &stats.arrival_check {
            ArrivalCheck::PeriodShift(d) => {
                assert_eq!(d.periods, Some(1));
                assert!(d.message().contains("1024 samples"), "{}", d.message());
            }
            other => panic!("expected PeriodShift, got {other:?}"),
        }
    }

    /// #359 AC3, tested against the rejected implementation: a delta one
    /// sample off an exact period must never read as a period shift — a
    /// test that only checked "a difference" would pass on this input too.
    #[test]
    fn arrival_check_a_near_period_delta_is_mismatch_not_period_shift() {
        let sr = 48_000u32;
        let period = 1024u32;
        let stored_tau_s = 0.0119;
        let same_capture_tau_s = stored_tau_s + (period as f64 + 1.0) / sr as f64;
        let mut r = ir_report_with_peak(1024, 600, 1.0, 0.0, sr);
        r.reference_latency = Some(measured_reference(same_capture_tau_s));
        r.reference_stored_latency = Some(stored_reference_tau(stored_tau_s, Some(period)));
        let stats = r.ir_stats().unwrap();
        match &stats.arrival_check {
            ArrivalCheck::Mismatch(d) => {
                assert_eq!(d.periods, None);
                assert_eq!(d.delta_samples, period as i64 + 1);
            }
            other => panic!("expected Mismatch, got {other:?}"),
        }
    }

    /// A sub-sample difference is ordinary rounding noise, not a fault —
    /// [`compare_tau_readings`]'s whole-sample agreement rule, inherited
    /// from #347 unchanged.
    #[test]
    fn arrival_check_a_fractional_sample_difference_is_noise_not_a_fault() {
        let sr = 48_000u32;
        let stored_tau_s = 0.0119;
        let same_capture_tau_s = stored_tau_s + 0.3 / sr as f64;
        let mut r = ir_report_with_peak(1024, 600, 1.0, 0.0, sr);
        r.reference_latency = Some(measured_reference(same_capture_tau_s));
        r.reference_stored_latency = Some(stored_reference_tau(stored_tau_s, Some(1024)));
        let stats = r.ir_stats().unwrap();
        assert_eq!(stats.arrival_check, ArrivalCheck::Agree);
    }

    /// A backend that cannot report a period size must never let a delta
    /// that happens to equal 1024 samples read as a period shift — there is
    /// no period on file to have been a multiple of.
    #[test]
    fn arrival_check_an_unknown_period_size_never_reads_as_a_period_shift() {
        let sr = 48_000u32;
        let stored_tau_s = 0.0119;
        let same_capture_tau_s = stored_tau_s + 1024.0 / sr as f64;
        let mut r = ir_report_with_peak(1024, 600, 1.0, 0.0, sr);
        r.reference_latency = Some(measured_reference(same_capture_tau_s));
        r.reference_stored_latency = Some(stored_reference_tau(stored_tau_s, None));
        let stats = r.ir_stats().unwrap();
        match &stats.arrival_check {
            ArrivalCheck::Mismatch(d) => assert_eq!(d.periods, None),
            other => panic!("expected Mismatch (unknown period size), got {other:?}"),
        }
    }

    /// Inverting the #359 gate (#544): a detected period shift is now a
    /// drift readout and no longer withholds the flight time, which is
    /// taken from the shifted live reading itself. The #359 rule withheld
    /// it; that rule is computed here and gives `None`.
    #[test]
    fn arrival_check_period_shift_no_longer_withholds_the_flight_time() {
        let sr = 48_000u32;
        let period = 1024u32;
        let stored_tau_s = 0.0119;
        let same_capture_tau_s = stored_tau_s + period as f64 / sr as f64;
        let mut r = ir_report_with_peak(4096, 3000, 1.0, 0.0, sr);
        r.interface_latency = Some(measured_tau(0.001));
        r.reference_latency = Some(measured_reference(same_capture_tau_s));
        r.reference_stored_latency = Some(stored_reference_tau(stored_tau_s, Some(period)));
        r.inter_pair_offset = Some(measured_offset(0.0));
        let stats = r.ir_stats().unwrap();
        assert!(matches!(stats.arrival_check, ArrivalCheck::PeriodShift(_)));
        let ft = stats
            .flight_time_s
            .expect("a period shift withholds nothing");
        assert!((ft - (stats.arrival_s - same_capture_tau_s)).abs() < 1e-15);

        let rejected = match &stats.arrival_check {
            ArrivalCheck::Agree | ArrivalCheck::Unchecked { .. } => Some(ft),
            _ => None,
        };
        assert_eq!(rejected, None, "the #359 rule withheld it");
    }

    /// A report written before v12 records no offset: its re-derived flight
    /// time is withheld as predating v12, whatever stored τ and live
    /// reference it carries. Before #544 this report produced
    /// `arrival − stored τ`.
    #[test]
    fn a_pre_v12_report_withholds_the_flight_time() {
        let sr = 48_000u32;
        let mut r = ir_report_with_peak(1024, 600, 1.0, 0.0, sr);
        r.schema_version = 11;
        r.interface_latency = Some(measured_tau(0.001));
        r.reference_latency = Some(measured_reference(0.002));
        r.reference_stored_latency = None; // v8 shape: field absent
        r.inter_pair_offset = None;
        let stats = r.ir_stats().unwrap();
        assert!(matches!(
            stats.arrival_check,
            ArrivalCheck::Unchecked { .. }
        ));
        assert_eq!(
            stats.latency_basis,
            LatencyBasis::Withheld(WithheldBasis::PredatesV12)
        );
        assert_eq!(stats.flight_time_s, None);
    }

    // ─── #466: interface_latency.session_check ───────────────────────────

    fn with_session_check(tau_s: f64, verdict: Option<LayerVerdict>) -> InterfaceLatency {
        match measured_tau(tau_s) {
            InterfaceLatency::Measured(m) => InterfaceLatency::Measured(MeasuredLatency {
                session_check: verdict,
                ..m
            }),
            other => other,
        }
    }

    fn refused_tau(via: Option<&str>) -> LayerVerdict {
        use crate::shared::calibration::session::{CheckSource, Evidence, VerdictUnit};
        LayerVerdict::Refused {
            evidence: Evidence {
                measured: 1743.0,
                stored: 1711.0,
                delta: 32.0,
                tolerance: 0.0,
                unit: VerdictUnit::Samples,
                stored_at: "2026-08-15T00:00:00Z".into(),
                checked_at: "2026-09-16T14:02:11Z".into(),
                source: CheckSource::Explicit,
            },
            via: via.map(str::to_string),
            delta_bound: None,
        }
    }

    /// Inverting R6-3 (i)–(iii) (#544): a refused session check on the
    /// capture pair's stored τ no longer withholds the flight time, with or
    /// without a propagated `via`, because that stored τ is no longer
    /// subtracted. The #466 rule withheld it on the same report.
    #[test]
    fn a_refused_session_check_no_longer_withholds_the_flight_time() {
        let sr = 48_000u32;
        let ref_tau_s = 0.0119;
        for via in [None, Some("out1_in1")] {
            let mut r = ir_report_with_peak(1024, 600, 1.0, 0.0, sr);
            r.interface_latency = Some(with_session_check(0.001, Some(refused_tau(via))));
            r.reference_latency = Some(measured_reference(ref_tau_s));
            r.reference_stored_latency = Some(stored_reference_tau(ref_tau_s, Some(1024)));
            r.inter_pair_offset = Some(measured_offset(0.0));
            let stats = r.ir_stats().unwrap();
            assert_eq!(stats.arrival_check, ArrivalCheck::Agree);
            assert_eq!(
                stats.flight_time_s,
                Some(stats.arrival_s - ref_tau_s),
                "{via:?}"
            );
            let rejected = match &r.interface_latency {
                Some(InterfaceLatency::Measured(m))
                    if m.session_check
                        .as_ref()
                        .is_some_and(LayerVerdict::is_refused) =>
                {
                    None
                }
                _ => stats.flight_time_s,
            };
            assert_eq!(rejected, None, "the #466 rule withheld it ({via:?})");
        }
    }

    // ─── #544: enumeration flags ─────────────────────────────────────────

    /// The capture pair's stored-τ enumeration no longer qualifies the
    /// flight time (the value is not subtracted); the offset's own
    /// enumeration is carried in the basis as a flag and never withholds.
    #[test]
    fn a_non_same_enumeration_never_withholds_the_flight_time() {
        use crate::shared::calibration::EnumerationCheck;
        let sr = 48_000u32;
        let cases = [
            EnumerationCheck::Same,
            EnumerationCheck::Crossed {
                boundary: "host rebooted".into(),
                since: None,
            },
            EnumerationCheck::NotObservable {
                reason: "cpal backend has no enumeration probe".into(),
            },
            EnumerationCheck::NotRecorded,
        ];
        for check in cases {
            let mut r = ir_report_with_peak(1024, 600, 1.0, 0.0, sr);
            r.interface_latency = Some(measured_tau_with_check(0.001, Some(check.clone())));
            r.reference_latency = Some(measured_reference(0.002));
            let InterPairOffset::Measured(mut m) = measured_offset(0.0) else {
                unreachable!()
            };
            m.enumeration = check.clone();
            r.inter_pair_offset = Some(InterPairOffset::Measured(m));
            let stats = r.ir_stats().unwrap();
            assert_eq!(
                stats.flight_time_s,
                Some(stats.arrival_s - 0.002),
                "{check:?} must not withhold the flight time"
            );
            assert_eq!(
                stats.latency_basis,
                LatencyBasis::Live {
                    reference_tau_s: 0.002,
                    offset: LiveOffset::Measured {
                        offset_s: 0.0,
                        enumeration: check.clone()
                    },
                },
            );
        }
    }

    // ─── #537: band-limited arrival and its cross-check ──────────────────

    /// #537 acceptance, tested against the rejected implementation: a
    /// direct impulse at `t0` and a 55 Hz room mode 15 ms later, 21.9 dB
    /// above it. The broadband peak — computed inline, the rule `ir_stats`
    /// used before this issue — lands on the mode. The arrival must be `t0`,
    /// standing `BroadbandLater`, and the flight time produced but marked.
    #[test]
    fn room_mode_after_the_direct_sound_does_not_move_the_arrival() {
        let RoomModeCapture {
            mut report,
            t0,
            mode_peak,
        } = direct_plus_room_mode_report();
        let sr = 96_000u32;
        let MeasurementData::ImpulseResponse { linear_ir, .. } = &report.data[0].data else {
            unreachable!()
        };
        let (rejected, _) = ir_peak(linear_ir);
        assert_eq!(
            rejected, mode_peak,
            "the failing case: the broadband peak is t0 + 15 ms"
        );
        assert_eq!(mode_peak - t0, 1_440, "test setup: 15 ms at 96 kHz");

        let tau_s = 1_711.0 / sr as f64;
        report.reference_latency = Some(measured_reference(tau_s));
        report.inter_pair_offset = Some(InterPairOffset::Identity);
        let stats = report.ir_stats().unwrap();
        assert_eq!(stats.verdict, IrVerdict::Ok, "test setup");
        assert_eq!(stats.peak_index, mode_peak);
        assert!(
            stats.arrival_index.abs_diff(t0) <= 1,
            "arrival {} must be t0 {t0} ± 1",
            stats.arrival_index
        );
        assert_eq!(stats.arrival_source, BAND_LIMITED);
        assert_eq!(
            stats.arrival_cross_check,
            ArrivalCrossCheck::BroadbandLater {
                gap: mode_peak as i64 - stats.arrival_index as i64
            }
        );
        assert!(stats.band_limited_snr_db.unwrap() >= ARRIVAL_SNR_MIN_DB);
        let centre = linear_ir.len() / 2;
        let ft = stats
            .flight_time_s
            .expect("BroadbandLater produces the flight time");
        let want = (stats.arrival_index as f64 - centre as f64) / sr as f64 - tau_s;
        assert!((ft - want).abs() < 1e-12, "{ft} vs {want}");
        assert!(
            !stats.arrival_cross_check.withholds_flight_time()
                && !matches!(stats.arrival_cross_check, ArrivalCrossCheck::Agrees { .. }),
            "the flight time is marked, not plain"
        );
        // Architect revision 2 checked this shape offline: margin 29.6 dB.
        let margin = stats.arrival_lobe_margin_db.unwrap();
        assert!(margin >= ARRIVAL_LOBE_MARGIN_MIN_DB, "margin {margin}");
        assert_eq!(stats.broadband_delta_level_db, Some(0.0), "r is the mode");
        assert_eq!(
            stats.broadband_delta_samples(),
            Some(mode_peak as i64 - stats.arrival_index as i64)
        );
    }

    /// #550 architect revision 3, the trusted branch and its fallback on a
    /// real-shaped IR: #537's room-mode fixture anchors the verdict's floor
    /// on the arrival; white noise added until the arrival misses
    /// [`ARRIVAL_SNR_MIN_DB`] — the pick still on the same sample — flips
    /// the anchor to the peak, and the figure is then the #501 one, bit for
    /// bit. Also pins the floor line's two forms (#550 UX).
    #[test]
    fn an_untrusted_arrival_leaves_the_floor_before_the_peak() {
        let RoomModeCapture {
            report,
            t0,
            mode_peak,
        } = direct_plus_room_mode_report();
        let clean = report.ir_stats().unwrap();
        assert!(clean.band_limited_snr_db.unwrap() >= ARRIVAL_SNR_MIN_DB);
        assert!(clean.arrival_index.abs_diff(t0) <= 1, "test setup");
        assert_eq!(clean.peak_index, mode_peak, "test setup");
        assert_eq!(clean.pre_impulse_floor_anchor, PreImpulseAnchor::Arrival);
        let guard = clean.arrival_index - clean.pre_impulse_floor_end;
        assert_eq!(
            clean.pre_impulse_floor_lines(),
            vec![format!(
                "floor ends {guard} samples before arrival, sample {}",
                clean.arrival_index
            )]
        );

        let MeasurementData::ImpulseResponse { linear_ir, .. } = &report.data[0].data else {
            unreachable!()
        };
        let noised = [1e-4, 2e-4, 5e-4, 1e-3, 2e-3, 5e-3]
            .into_iter()
            .find_map(|amplitude| {
                let ir: Vec<f64> = linear_ir
                    .iter()
                    .zip(hashed_uniform_noise(linear_ir.len(), amplitude, 550))
                    .map(|(h, n)| h + n)
                    .collect();
                let stats = ir_report_with_custom_ir(ir.clone(), 96_000)
                    .ir_stats()
                    .unwrap();
                (stats.band_limited_snr_db.unwrap() < ARRIVAL_SNR_MIN_DB).then_some((ir, stats))
            });
        let (ir, stats) = noised.expect("no noise level took the arrival under its SNR gate");
        assert_eq!(
            stats.arrival_index, clean.arrival_index,
            "the noise moved the pick, so this is not the same arrival set aside"
        );
        // The mode's crest is flat, so the noise may slide the broadband
        // argmax along it by a few samples; it stays on the mode.
        let peak = stats.peak_index;
        assert!(
            peak.abs_diff(mode_peak) <= 48,
            "argmax {peak} left the mode crest at {mode_peak}"
        );
        let ArrivalCrossCheck::BandLimitedSnrLow { snr_db } = stats.arrival_cross_check else {
            panic!("standing {:?}", stats.arrival_cross_check)
        };
        assert_eq!(
            stats.pre_impulse_floor_anchor,
            PreImpulseAnchor::PeakArrivalNotTrusted
        );
        assert_eq!(
            stats.pre_impulse_snr_db.to_bits(),
            pre_impulse_snr_db(&ir, peak).to_bits(),
            "the fallback must read the #501 region"
        );
        assert_eq!(
            stats.pre_impulse_floor_lines(),
            vec![
                format!("floor ends {guard} samples before peak, sample {peak}"),
                format!(
                    "not before arrival \u{2014} arrival SNR {snr_db:.1} dB, required \
                     \u{2265} {ARRIVAL_SNR_MIN_DB:.1} dB"
                ),
            ]
        );
    }

    /// #550 architect revision 4: the floor anchor as a table, one row per
    /// variant, plus the two rows that pin the ordering. The unmeasured row
    /// under `BandLimitedSnrLow { +inf }` fixes the order before #577 lands;
    /// the unmeasured row under `Agrees` fails on the revision-3 rule, which
    /// read trust from the standing alone.
    #[test]
    fn floor_anchor_table() {
        let (arrival, peak) = (21_246usize, 23_545usize);
        let agrees = ArrivalCrossCheck::Agrees { gap: 0 };
        let low = ArrivalCrossCheck::BandLimitedSnrLow { snr_db: 12.2 };
        let low_inf = ArrivalCrossCheck::BandLimitedSnrLow {
            snr_db: f64::INFINITY,
        };
        let earlier = ArrivalCrossCheck::BroadbandEarlier {
            gap: arrival as i64 - peak as i64,
        };
        let floor = 20_046usize;
        let rows: [(usize, usize, ArrivalSource, &ArrivalCrossCheck, usize, _); 8] = [
            (
                arrival,
                peak,
                BAND_LIMITED,
                &agrees,
                floor,
                (arrival, PreImpulseAnchor::Arrival),
            ),
            (
                peak,
                arrival,
                BAND_LIMITED,
                &earlier,
                floor,
                (arrival, PreImpulseAnchor::Peak),
            ),
            (
                peak,
                peak,
                BAND_LIMITED,
                &agrees,
                floor,
                (peak, PreImpulseAnchor::ArrivalAndPeak),
            ),
            (
                peak,
                peak,
                ArrivalSource::Peak,
                &agrees,
                floor,
                (peak, PreImpulseAnchor::ArrivalAndPeak),
            ),
            (
                arrival,
                peak,
                BAND_LIMITED,
                &low,
                floor,
                (peak, PreImpulseAnchor::PeakArrivalNotTrusted),
            ),
            (
                arrival,
                peak,
                BAND_LIMITED,
                &low_inf,
                0,
                (peak, PreImpulseAnchor::PeakArrivalUnmeasured),
            ),
            (
                arrival,
                peak,
                BAND_LIMITED,
                &agrees,
                0,
                (peak, PreImpulseAnchor::PeakArrivalUnmeasured),
            ),
            (
                arrival,
                peak,
                ArrivalSource::Peak,
                &agrees,
                floor,
                (peak, PreImpulseAnchor::Peak),
            ),
        ];
        for (i, (a, p, source, check, floor_len, want)) in rows.into_iter().enumerate() {
            assert_eq!(
                floor_anchor(a, p, &source, check, floor_len),
                want,
                "row {i}: arrival {a}, peak {p}, {source:?}, {check:?}, floor {floor_len}"
            );
        }
    }

    /// #550 UX revision 4: the unmeasured continuation is a fixed string. It
    /// never formats the `+inf` SNR or the arrival sample, and the
    /// not-trusted form keeps its number (the anchor arms must not collapse).
    #[test]
    fn an_unmeasured_arrival_prints_the_fixed_continuation() {
        let RoomModeCapture { report, .. } = direct_plus_room_mode_report();
        let mut stats = report.ir_stats().unwrap();
        let peak = stats.peak_index;
        stats.pre_impulse_floor_end = pre_impulse_region(
            match &report.data[0].data {
                MeasurementData::ImpulseResponse { linear_ir, .. } => linear_ir,
                _ => unreachable!(),
            },
            peak,
        )
        .len();
        let guard = peak - stats.pre_impulse_floor_end;
        stats.band_limited_snr_db = Some(f64::INFINITY);
        stats.arrival_cross_check = ArrivalCrossCheck::BandLimitedSnrLow {
            snr_db: f64::INFINITY,
        };
        stats.pre_impulse_floor_anchor = PreImpulseAnchor::PeakArrivalUnmeasured;
        let lines = stats.pre_impulse_floor_lines();
        assert_eq!(
            lines,
            vec![
                format!("floor ends {guard} samples before peak, sample {peak}"),
                "not before arrival \u{2014} arrival SNR unmeasured, no floor before it"
                    .to_string(),
            ]
        );
        let arrival = stats.arrival_index.to_string();
        for line in &lines {
            assert!(
                !line.contains("inf") && !line.contains('\u{221e}') && !line.contains(&arrival),
                "{line}"
            );
        }

        stats.arrival_cross_check = ArrivalCrossCheck::BandLimitedSnrLow { snr_db: 12.2 };
        stats.pre_impulse_floor_anchor = PreImpulseAnchor::PeakArrivalNotTrusted;
        assert_eq!(
            stats.pre_impulse_floor_lines()[1],
            format!(
                "not before arrival \u{2014} {}",
                arrival_snr_low_reason(12.2)
            )
        );
    }

    /// #550 (QA on PR #578): the scope line names only the parameters that
    /// differ, in the `IR sweep` block's order.
    #[test]
    fn scope_names_only_the_parameters_that_differ_in_print_order() {
        let sr = 48_000;
        let w = ir_default_window_len(sr);
        let scope = |f1: f64, dur: f64, len: usize| {
            let mut r = ir_report_with_custom_ir_band(vec![0.0; len], sr, IR_DEFAULT_F2_HZ);
            if let MeasurementData::ImpulseResponse {
                f1_hz, duration_s, ..
            } = &mut r.data[0].data
            {
                *f1_hz = f1;
                *duration_s = dur;
            }
            pre_impulse_snr_scope(&r).unwrap().to_string()
        };
        let (f1, d) = (IR_DEFAULT_F1_HZ, IR_DEFAULT_DURATION_S);
        assert_eq!(
            scope(f1, d, w),
            "scored for this sweep's band, length, window"
        );
        assert_eq!(scope(200.0, d, w), "unscored for this sweep's band");
        assert_eq!(scope(f1, 8.0, w), "unscored for this sweep's length");
        assert_eq!(scope(f1, d, w / 2), "unscored for this sweep's window");
        assert_eq!(
            scope(200.0, d, w / 2),
            "unscored for this sweep's band, window"
        );
    }

    /// 96 kHz, 0.4 s window, a ±1e-7 floor, and the live latency every
    /// cross-check firing case below subtracts (so a `None` flight time is
    /// the standing's doing, not a missing latency).
    fn cross_check_report(ir: Vec<f64>, f2_hz: f64) -> MeasurementReport {
        let mut r = ir_report_with_custom_ir_band(ir, 96_000, f2_hz);
        with_live_latency(&mut r, 0.001);
        r
    }

    const CC_LEN: usize = 38_400;
    const CC_T0: usize = CC_LEN / 2 + 598;

    fn cc_floor() -> Vec<f64> {
        hashed_uniform_noise(CC_LEN, 1e-7, 1)
    }

    /// A single spike: the band-limited and broadband peaks are the same
    /// sample, so the standing is `Agrees` and the flight time is plain.
    #[test]
    fn cross_check_agrees_on_a_single_spike() {
        let mut ir = cc_floor();
        ir[CC_T0] = 0.5;
        let stats = cross_check_report(ir, 20_000.0).ir_stats().unwrap();
        assert_eq!(
            stats.arrival_cross_check,
            ArrivalCrossCheck::Agrees { gap: 0 }
        );
        assert_eq!(stats.arrival_index, CC_T0);
        assert_eq!(stats.broadband_delta_samples(), Some(0));
        assert!(stats.flight_time_s.is_some());
    }

    /// An HF reflection 2 dB stronger than the direct sound, 3 ms later:
    /// both peaks pick the reflection, and the direct sound 288 samples
    /// earlier is within 6 dB of it. Withheld.
    #[test]
    fn cross_check_earlier_comparable_fires_on_a_stronger_late_reflection() {
        let mut ir = cc_floor();
        let reflection = CC_T0 + 288;
        ir[CC_T0] = 0.5;
        ir[reflection] = 0.5 * 10f64.powf(2.0 / 20.0);
        let stats = cross_check_report(ir, 20_000.0).ir_stats().unwrap();
        assert_eq!(stats.arrival_index, reflection, "test setup");
        assert_eq!(stats.peak_index, reflection, "test setup");
        match stats.arrival_cross_check {
            ArrivalCrossCheck::EarlierComparable { index, level_db } => {
                assert_eq!(index, CC_T0);
                assert!((level_db + 2.0).abs() < 0.05, "level {level_db}");
            }
            other => panic!("expected EarlierComparable, got {other:?}"),
        }
        assert_eq!(stats.flight_time_s, None);
    }

    /// The same pair 20.5 dB apart does not fire: the bound is 20 dB
    /// (architect revision 3). 14.2 dB — the ux example — does.
    #[test]
    fn cross_check_earlier_comparable_does_not_fire_below_its_bound() {
        let mut ir = cc_floor();
        ir[CC_T0] = 0.5 * 10f64.powf(-14.2 / 20.0);
        ir[CC_T0 + 288] = 0.5;
        let stats = cross_check_report(ir, 20_000.0).ir_stats().unwrap();
        assert!(
            matches!(
                stats.arrival_cross_check,
                ArrivalCrossCheck::EarlierComparable { .. }
            ),
            "{:?}",
            stats.arrival_cross_check
        );
        let mut ir = cc_floor();
        ir[CC_T0] = 0.5;
        ir[CC_T0 + 288] = 0.5 * 10f64.powf(20.5 / 20.0);
        let stats = cross_check_report(ir, 20_000.0).ir_stats().unwrap();
        assert!(
            matches!(stats.arrival_cross_check, ArrivalCrossCheck::Agrees { .. }),
            "{:?}",
            stats.arrival_cross_check
        );
    }

    /// A strong LF-only component (a Gaussian, σ = 2 ms, nothing left of it
    /// above 2 kHz) 5 ms before a weaker HF direct sound: the broadband peak
    /// is earlier than the arrival by more than the tolerance. Withheld.
    #[test]
    fn cross_check_broadband_earlier_fires_on_an_earlier_lf_component() {
        let mut ir = cc_floor();
        let lf_centre = CC_T0 - 480;
        for (n, v) in ir.iter_mut().enumerate() {
            let t = (n as f64 - lf_centre as f64) / 192.0;
            *v += (-0.5 * t * t).exp();
        }
        ir[CC_T0] += 0.3;
        let stats = cross_check_report(ir, 20_000.0).ir_stats().unwrap();
        assert_eq!(stats.peak_index, lf_centre, "test setup");
        assert_eq!(stats.arrival_index, CC_T0);
        assert_eq!(
            stats.arrival_cross_check,
            ArrivalCrossCheck::BroadbandEarlier { gap: -480 }
        );
        assert_eq!(stats.flight_time_s, None);
    }

    /// White noise raised until the high-passed IR's pre-impulse SNR is
    /// under the 20 dB gate. Withheld, with the measured SNR carried.
    #[test]
    fn cross_check_band_limited_snr_low_fires_on_raised_hf_noise() {
        let mut ir = hashed_uniform_noise(CC_LEN, 0.25, 2);
        ir[CC_T0] += 1.0;
        let stats = cross_check_report(ir, 20_000.0).ir_stats().unwrap();
        let snr = stats.band_limited_snr_db.unwrap();
        assert!(snr < ARRIVAL_SNR_MIN_DB, "test setup: SNR {snr}");
        assert_eq!(
            stats.arrival_cross_check,
            ArrivalCrossCheck::BandLimitedSnrLow { snr_db: snr }
        );
        assert_eq!(stats.flight_time_s, None);
    }

    /// A sweep that ends at 2 kHz has no octave above the 2 kHz corner: the
    /// arrival falls back to the broadband peak, and the flight time is
    /// withheld (architect revision 2; revision 1 produced it, marked). The
    /// rig's step-5 capture was this band.
    #[test]
    fn cross_check_unavailable_below_an_octave_above_the_corner() {
        let mut ir = cc_floor();
        ir[CC_T0] = 0.5;
        let stats = cross_check_report(ir, 2_000.0).ir_stats().unwrap();
        assert_eq!(stats.arrival_source, ArrivalSource::Peak);
        assert_eq!(stats.arrival_index, stats.peak_index);
        assert_eq!(
            stats.arrival_cross_check,
            ArrivalCrossCheck::BandLimitUnavailable {
                band_top_hz: 2_000.0,
                required_hz: 4_000.0
            }
        );
        assert_eq!(stats.band_limited_snr_db, None);
        assert_eq!(stats.arrival_lobe_margin_db, None);
        assert_eq!(stats.broadband_delta_samples(), None);
        assert_eq!(stats.flight_time_s, None);
        // The edge: exactly an octave above the corner is band-limited.
        let mut ir = cc_floor();
        ir[CC_T0] = 0.5;
        let stats = cross_check_report(ir, 4_000.0).ir_stats().unwrap();
        assert_eq!(
            stats.arrival_source,
            ArrivalSource::BandLimitedPeak {
                corner_hz: ARRIVAL_HIGH_PASS_CORNER_HZ
            }
        );
    }

    /// The tolerance is 2.0 ms in whole samples.
    #[test]
    fn cross_check_tolerance_is_two_ms_in_samples() {
        assert_eq!(arrival_cross_check_tolerance_samples(96_000), 192);
        assert_eq!(arrival_cross_check_tolerance_samples(48_000), 96);
    }

    /// A direct impulse at `CC_T0` plus a minimum-phase resonance (a
    /// 2nd-order resonator at 2.8 kHz, Q = 3, gain 0.3) — HF ringing on the
    /// direct sound — band-limited to `top_hz` the way a sweep ending there
    /// would leave it (the IR minus its zero-phase high-pass at `top_hz`),
    /// over the ±1e-7 floor. `None` for `top_hz` leaves the full band.
    fn ringing_direct_sound(top_hz: Option<f64>) -> Vec<f64> {
        const F0: f64 = 2_800.0;
        const Q: f64 = 3.0;
        const GAIN: f64 = 0.3;
        let sr = 96_000.0;
        let w0 = 2.0 * std::f64::consts::PI * F0 / sr;
        let r = (-w0 / (2.0 * Q)).exp();
        let (a1, a2, b1) = (2.0 * r * w0.cos(), -r * r, r * w0.sin());
        let mut h = vec![0.0_f64; CC_LEN];
        h[CC_T0] = 1.0;
        let (mut y1, mut y2) = (0.0_f64, 0.0_f64);
        for (k, v) in h.iter_mut().enumerate().skip(CC_T0 + 1) {
            // y[n] = b1·δ[n − 1 − t0] + a1·y[n − 1] + a2·y[n − 2]
            let x1 = if k == CC_T0 + 1 { 1.0 } else { 0.0 };
            let y = b1 * x1 + a1 * y1 + a2 * y2;
            *v += GAIN * y;
            (y2, y1) = (y1, y);
        }
        if let Some(top) = top_hz {
            let above = zero_phase_high_pass(&h, 96_000, top);
            for (v, a) in h.iter_mut().zip(above) {
                *v -= a;
            }
        }
        for (v, n) in h.iter_mut().zip(cc_floor()) {
            *v += n;
        }
        h
    }

    /// Architect revision 2's half-cycle fixture (the rig's step-5 shape),
    /// tested against the rejected implementation: with only `f2 ≥ 2·f_c`,
    /// the argmax of the high-passed IR of a direct sound with HF ringing,
    /// swept to 4 kHz, lands a half-cycle after the direct sound's first
    /// lobe, with the lobe it skipped less than 3 dB down. The revised rule
    /// must withhold that as `ArrivalAmbiguous`. The same DUT swept to
    /// 20 kHz picks the direct sound with a clear margin and agrees.
    #[test]
    fn a_half_cycle_hop_is_withheld_as_arrival_ambiguous() {
        let half_cycle = (96_000.0 / (2.0 * 2_800.0)) as i64; // 17 samples
        let ir = ringing_direct_sound(Some(4_000.0));

        // The rejected rule, inline: band available, argmax of |h_hp|.
        assert!(band_limit_available(
            96_000,
            4_000.0,
            ARRIVAL_HIGH_PASS_CORNER_HZ
        ));
        let h_hp = zero_phase_high_pass(&ir, 96_000, ARRIVAL_HIGH_PASS_CORNER_HZ);
        let (rejected, _) = ir_peak(&h_hp);
        let lobe = second_lobe(&h_hp, rejected, 48).expect("a neighbouring lobe");
        let first_lobe = rejected as i64 + lobe.offset;
        assert!(
            (first_lobe - CC_T0 as i64).abs() < half_cycle / 2,
            "the skipped lobe {first_lobe} is the direct sound's first ({CC_T0})"
        );
        assert!(
            (-lobe.offset - half_cycle).abs() <= 2,
            "the rejected pick is one half-cycle late: offset {}",
            lobe.offset
        );
        assert!(lobe.margin_db < ARRIVAL_LOBE_MARGIN_MIN_DB, "{lobe:?}");

        let stats = cross_check_report(ir, 4_000.0).ir_stats().unwrap();
        assert_eq!(stats.arrival_index, rejected, "same pick, now guarded");
        assert_eq!(
            stats.arrival_cross_check,
            ArrivalCrossCheck::ArrivalAmbiguous {
                margin_db: lobe.margin_db,
                offset: lobe.offset,
            }
        );
        assert_eq!(stats.arrival_lobe_margin_db, Some(lobe.margin_db));
        assert_eq!(stats.arrival_lobe_offset, Some(lobe.offset));
        assert!(stats.band_limited_snr_db.unwrap() >= ARRIVAL_SNR_MIN_DB);
        assert_eq!(stats.broadband_delta_samples(), None);
        assert_eq!(stats.flight_time_s, None);

        let full = cross_check_report(ringing_direct_sound(None), 20_000.0)
            .ir_stats()
            .unwrap();
        assert_eq!(full.arrival_index, CC_T0);
        assert!(full.arrival_lobe_margin_db.unwrap() >= ARRIVAL_LOBE_MARGIN_MIN_DB);
        assert!(matches!(
            full.arrival_cross_check,
            ArrivalCrossCheck::Agrees { .. }
        ));
        assert!(full.flight_time_s.is_some());
    }

    /// Architect revision 2's argmax-flip fixture (rig steps 1 and 4): a
    /// weak direct impulse at `CC_T0`, a broadband lobe 1.3 ms later (a
    /// Gaussian, σ = 0.3 ms, with nothing above 2 kHz) and a 55 Hz mode
    /// swing 16 ms later. The two later peaks are 0.3 dB apart, and the two
    /// variants swap which one is the maximum. Tested against the rejected
    /// implementation: the argmax gap exceeds the tolerance in one variant
    /// and not in the other. The revised rule reads `Agrees` with the same
    /// gap in both.
    #[test]
    fn a_flipping_broadband_argmax_does_not_flip_the_standing() {
        let sr = 96_000.0;
        let lobe_at = CC_T0 + 125;
        let mode_at = CC_T0 + 1_536;
        let build = |lobe: f64, mode: f64| {
            let mut ir = cc_floor();
            ir[CC_T0] += 0.3;
            for (n, v) in ir.iter_mut().enumerate() {
                let t = (n as f64 - lobe_at as f64) / (0.0003 * sr);
                *v += lobe * (-0.5 * t * t).exp();
                let t = (n as f64 - mode_at as f64) / sr;
                let envelope = if t < -0.010 {
                    0.0
                } else if t <= 0.0 {
                    0.5 * (1.0 - (std::f64::consts::PI * (t + 0.010) / 0.010).cos())
                } else {
                    (-t / 0.050).exp()
                };
                *v += mode * envelope * (2.0 * std::f64::consts::PI * 55.0 * t).cos();
            }
            ir
        };
        let flip = 10f64.powf(0.3 / 20.0);
        let tolerance = arrival_cross_check_tolerance_samples(96_000);
        let mut gaps = Vec::new();
        for (name, lobe, mode, argmax) in [
            ("mode is the maximum", 1.0, flip, mode_at),
            ("lobe is the maximum", flip, 1.0, lobe_at),
        ] {
            let stats = cross_check_report(build(lobe, mode), 20_000.0)
                .ir_stats()
                .unwrap();
            assert_eq!(stats.arrival_index, CC_T0, "{name}");
            assert_eq!(stats.peak_index, argmax, "{name}: test setup");
            // The rejected rule, inline: the gap to the argmax.
            let argmax_gap = stats.peak_index as i64 - stats.arrival_index as i64;
            gaps.push(argmax_gap > tolerance);
            match stats.arrival_cross_check {
                ArrivalCrossCheck::Agrees { gap } => {
                    assert!((gap - 125).abs() <= 1, "{name}: gap {gap}");
                }
                ref other => panic!("{name}: expected Agrees, got {other:?}"),
            }
            let level = stats.broadband_delta_level_db.unwrap();
            assert!(level > -ARRIVAL_BROADBAND_COMPARABLE_DB, "{name}: {level}");
            assert!(stats.flight_time_s.is_some(), "{name}");
        }
        assert_eq!(
            gaps,
            [true, false],
            "the rejected rule flips between the variants"
        );
    }

    /// QA on PR #538: the band-limited SNR gate fires exactly below
    /// [`ARRIVAL_SNR_MIN_DB`] (35 dB since architect revision 3), across a
    /// sweep of floors straddling it (uniform noise: its maximum is at most
    /// √3·rms, so no other standing pre-empts it).
    #[test]
    fn band_limited_snr_gate_fires_exactly_below_its_threshold() {
        let (mut saw_low, mut saw_ok) = (false, false);
        for step in 0..40 {
            let amp = 0.016 * 10f64.powf(step as f64 / 76.0); // ≈ 40 dB … 30 dB
            let mut ir = hashed_uniform_noise(CC_LEN, amp, 3);
            ir[CC_T0] += 1.0;
            let s = cross_check_report(ir, 20_000.0).ir_stats().unwrap();
            let snr = s.band_limited_snr_db.unwrap();
            let low = matches!(
                s.arrival_cross_check,
                ArrivalCrossCheck::BandLimitedSnrLow { .. }
            );
            assert_eq!(
                low,
                snr < ARRIVAL_SNR_MIN_DB,
                "amp {amp}: SNR {snr}, {:?}",
                s.arrival_cross_check
            );
            assert_eq!(low, s.flight_time_s.is_none(), "amp {amp}");
            saw_low |= low;
            saw_ok |= !low;
        }
        assert!(saw_low && saw_ok, "the sweep must straddle the gate");
    }

    /// #537 architect revision 2, item 6: the onset floor is read from the
    /// broadband IR before the arrival, not before the broadband peak.
    /// Fixture: a small direct impulse early in the window, then a slowly
    /// growing 100 Hz swing (nothing above 2 kHz) whose maximum is the
    /// broadband peak near the window's end. The region before that peak is
    /// mostly swing, so its median floor sits above the direct sound, and
    /// the rejected floor — computed inline — declines the onset search
    /// ("nothing above the floor"). The floor before the arrival is the
    /// noise, and the onset is picked.
    #[test]
    fn onset_floor_is_read_before_the_arrival() {
        let sr = 96_000.0;
        let t0 = 12_000usize;
        let start = t0 + 300;
        let mut ir = cc_floor();
        ir[t0] += 0.005;
        for (n, v) in ir.iter_mut().enumerate().skip(start) {
            let x = (n - start) as f64 / (CC_LEN - start) as f64;
            *v += 0.2 * x * x * (2.0 * std::f64::consts::PI * 100.0 * n as f64 / sr).sin();
        }
        let report = cross_check_report(ir, 20_000.0);
        let stats = report.ir_stats().unwrap();
        assert_eq!(stats.arrival_index, t0, "test setup");
        assert!(stats.peak_index > CC_LEN - 1_000, "test setup");
        let MeasurementData::ImpulseResponse { linear_ir, .. } = &report.data[0].data else {
            unreachable!()
        };
        let onset_with = |floor: f64| {
            crate::measurement::sweep::estimate_onset(
                linear_ir,
                t0,
                96_000,
                floor,
                &stats.causal_bound,
            )
        };
        let rejected = onset_with(onset_floor(pre_impulse_region(linear_ir, stats.peak_index)));
        assert_eq!(rejected.pick, OnsetPick::Declined, "{}", rejected.rule);
        // The floor only gates whether the window holds anything above it;
        // the revised floor must let the search run.
        let revised = onset_with(onset_floor(pre_impulse_region(linear_ir, t0)));
        assert!(
            rejected.rule.contains("above the pre-impulse floor")
                && !revised.rule.contains("above the pre-impulse floor"),
            "rejected: {}; revised: {}",
            rejected.rule,
            revised.rule
        );
        assert_eq!(stats.onset_index, revised.index);
        assert_eq!(stats.onset_rule, revised.rule);
    }

    // ─── #537 architect revision 3: the distance check ───────────────────

    /// A single spike at `CC_T0` (flight 598 − 96 = 502 samples) with `d`
    /// typed, no temperature.
    fn distance_report(distance_m: Option<f64>) -> MeasurementReport {
        let mut ir = cc_floor();
        ir[CC_T0] = 0.5;
        let mut r = cross_check_report(ir, 20_000.0);
        r.position = Some(PositionSnapshot {
            distance_m,
            ..Default::default()
        });
        r
    }

    /// The distance that puts `excess` samples of flight past `d / c`.
    fn distance_for_excess(excess: f64) -> f64 {
        let c = crate::shared::conversions::speed_of_sound_from_config(None);
        (502.0 - excess) / 96_000.0 * c
    }

    /// The window has one edge, `d/c − ε`, `ε = (5 cm + 2 %·d) / c`: at
    /// 2 m and 343 m/s, −25.2 samples (the ux arithmetic). No late edge
    /// (#552).
    #[test]
    fn distance_window_is_tape_and_c_early_only() {
        let w = DistanceWindow::new(2.0, None);
        let c = crate::shared::conversions::speed_of_sound_from_config(None);
        assert_eq!(w.speed_of_sound_m_s, c);
        assert!((w.expected_s - 2.0 / c).abs() < 1e-15);
        let eps = (0.05 + 0.02 * 2.0) / c;
        assert!((w.low_s + eps).abs() < 1e-15);
        if c == 343.0 {
            assert!(
                (w.low_s * 96_000.0 + 25.19).abs() < 0.01,
                "{}",
                w.low_s * 96_000.0
            );
        }
        let warm = DistanceWindow::new(2.0, Some(30.0));
        assert_eq!(warm.temperature_c, Some(30.0));
        assert!(warm.speed_of_sound_m_s > c);
    }

    /// Past the early edge: `Consistent`, the excess carried, the flight
    /// time produced. Just before it: `TooEarly`, withheld. Any amount past
    /// `d/c + ε` — where revision 3's late edge sat — is still `Consistent`
    /// and produced (#552). The comparison is in seconds.
    #[test]
    fn distance_check_withholds_only_a_flight_earlier_than_its_edge() {
        let stats = distance_report(Some(distance_for_excess(36.0)))
            .ir_stats()
            .unwrap();
        match stats.distance_check {
            DistanceCheck::Consistent { excess_s, .. } => {
                assert!((excess_s * 96_000.0 - 36.0).abs() < 1e-6, "{excess_s}")
            }
            ref other => panic!("{other:?}"),
        }
        assert!(stats.flight_time_s.is_some());

        // The edge scales with d, so it is solved for d: the flight is 502
        // samples, and d/c − ε must equal it. Shorter distances put the
        // flight later past d/c; the 1.0 ms rows sit where the deleted
        // allowance's edge was, the last one 400 samples past d/c.
        let c = crate::shared::conversions::speed_of_sound_from_config(None);
        let flight_s = 502.0 / 96_000.0;
        let tape = DISTANCE_TAPE_TOLERANCE_M;
        let rel = DISTANCE_SPEED_OF_SOUND_REL_TOL;
        let early_edge = (flight_s * c + tape) / (1.0 - rel);
        let old_late_edge = ((flight_s - 0.001) * c - tape) / (1.0 + rel);
        let one_sample_m = c / 96_000.0;
        for (distance, early) in [
            (early_edge + one_sample_m, true),
            (early_edge - one_sample_m, false),
            (old_late_edge + one_sample_m, false),
            (old_late_edge - one_sample_m, false),
            (distance_for_excess(400.0), false),
        ] {
            let stats = distance_report(Some(distance)).ir_stats().unwrap();
            let check = &stats.distance_check;
            assert_eq!(
                matches!(check, DistanceCheck::TooEarly { .. }),
                early,
                "{distance}: {check:?}"
            );
            if !early {
                assert!(
                    matches!(check, DistanceCheck::Consistent { .. }),
                    "{distance}: {check:?}"
                );
            }
            assert_eq!(stats.flight_time_s.is_none(), early, "{distance}");
            assert_eq!(
                stats.arrival_cross_check,
                ArrivalCrossCheck::Agrees { gap: 0 },
                "the IR side agrees; only the distance withholds"
            );
        }
    }

    /// No distance: `NotGiven`, flight time produced. A typed `0m`: not a
    /// verdict, never read as "not given". No live latency (#544: an
    /// unmeasured offset, whatever stored τ the report carries): nothing to
    /// check.
    #[test]
    fn distance_check_names_why_it_did_not_score() {
        let stats = distance_report(None).ir_stats().unwrap();
        assert_eq!(stats.distance_check, DistanceCheck::NotGiven);
        assert!(stats.flight_time_s.is_some());
        let mut r = distance_report(None);
        r.position = None;
        assert_eq!(
            r.ir_stats().unwrap().distance_check,
            DistanceCheck::NotGiven
        );

        let stats = distance_report(Some(0.0)).ir_stats().unwrap();
        assert_eq!(
            stats.distance_check,
            DistanceCheck::NotPositive { distance_m: 0.0 }
        );
        assert!(stats.flight_time_s.is_some());

        let mut r = distance_report(Some(2.0));
        r.interface_latency = Some(measured_tau(0.001));
        r.inter_pair_offset = Some(InterPairOffset::Unavailable {
            reason: "[out0_in0] against ref [out1_in1]; no \u{3c4} on file for this pair".into(),
        });
        let stats = r.ir_stats().unwrap();
        assert_eq!(
            stats.distance_check,
            DistanceCheck::NoLatency { distance_m: 2.0 }
        );
        assert_eq!(stats.flight_time_s, None);
    }

    /// The distance is scored even when the cross-check withholds the
    /// flight time. A pick 288 samples late is `Consistent` (#552: no late
    /// edge), so the cross-check alone withholds it.
    #[test]
    fn distance_check_scores_under_a_withholding_cross_check() {
        let mut ir = cc_floor();
        ir[CC_T0] = 0.5;
        ir[CC_T0 + 288] = 0.5 * 10f64.powf(2.0 / 20.0);
        let mut r = cross_check_report(ir, 20_000.0);
        // The pick is the reflection, 288 samples late.
        r.position = Some(PositionSnapshot {
            distance_m: Some(distance_for_excess(0.0)),
            ..Default::default()
        });
        let stats = r.ir_stats().unwrap();
        assert!(matches!(
            stats.arrival_cross_check,
            ArrivalCrossCheck::EarlierComparable { .. }
        ));
        match stats.distance_check {
            DistanceCheck::Consistent { excess_s, .. } => {
                assert!((excess_s * 96_000.0 - 288.0).abs() < 1e-6)
            }
            ref other => panic!("{other:?}"),
        }
        assert_eq!(stats.flight_time_s, None);
    }

    /// A single spike `excess` samples past `d/c` at `distance_m`, in a 1 s
    /// 96 kHz IR with a live latency of 1 ms, so arrivals hundreds of ms
    /// late still fit.
    fn late_arrival_report(distance_m: f64, excess: f64) -> MeasurementReport {
        const SR: u32 = 96_000;
        const LEN: usize = 96_000;
        const TAU_S: f64 = 0.001;
        let c = crate::shared::conversions::speed_of_sound_from_config(None);
        let flight = distance_m / c * SR as f64 + excess;
        let arrival = LEN / 2 + (TAU_S * SR as f64).round() as usize + flight.round() as usize;
        let mut ir = vec![0.0; LEN];
        ir[arrival] = 1.0;
        let mut r = ir_report_with_custom_ir_band(ir, SR, 20_000.0);
        with_live_latency(&mut r, TAU_S);
        r.position = Some(PositionSnapshot {
            distance_m: Some(distance_m),
            ..Default::default()
        });
        r
    }

    /// #552 AC 4 / AC 8: a delay tower 150 ms (14 400 samples) past `d/c`
    /// at 2 m is reported, not refused — `Consistent`, the flight time
    /// produced, the excess carried. Revision 3's 1.0 ms late edge read
    /// this `TooLate` with no flight time.
    #[test]
    fn an_arrival_150_ms_past_d_over_c_is_produced_with_its_excess() {
        let stats = late_arrival_report(2.0, 14_400.0).ir_stats().unwrap();
        match stats.distance_check {
            DistanceCheck::Consistent { excess_s, .. } => assert!(
                (excess_s * 96_000.0 - 14_400.0).abs() <= 2.0,
                "{}",
                excess_s * 96_000.0
            ),
            ref other => panic!("{other:?}"),
        }
        assert!(
            stats.flight_time_s.is_some(),
            "{:?}",
            stats.arrival_cross_check
        );
    }

    /// #552 AC 5: the Genelec 1083's measured +0.574 ms (#539 step 2), at
    /// 1 m ≈ 55 samples past `d/c`, stays `Consistent` and produced.
    #[test]
    fn the_measured_anchor_speaker_is_still_consistent() {
        let stats = late_arrival_report(1.0, 55.1).ir_stats().unwrap();
        match stats.distance_check {
            DistanceCheck::Consistent { excess_s, .. } => {
                assert!((excess_s * 1e3 - 0.574).abs() < 0.011, "{}", excess_s * 1e3)
            }
            ref other => panic!("{other:?}"),
        }
        assert!(
            stats.flight_time_s.is_some(),
            "{:?}",
            stats.arrival_cross_check
        );
    }

    /// Replay (#537 rig check step 0): print the revised rule's reading of
    /// every `*.json` report under `$AC_IR_REPLAY_DIR`, one line each, to
    /// compare against the architect's `revised.out` without emitting.
    /// Asserts nothing beyond parsing.
    #[test]
    #[ignore = "reads recorded reports from $AC_IR_REPLAY_DIR"]
    fn replay_recorded_reports() {
        let dir = std::env::var("AC_IR_REPLAY_DIR").expect("set AC_IR_REPLAY_DIR");
        let mut files: Vec<_> = std::fs::read_dir(&dir)
            .expect("read AC_IR_REPLAY_DIR")
            .map(|e| e.unwrap().path())
            .filter(|p| p.extension().is_some_and(|x| x == "json"))
            .collect();
        files.sort();
        for path in files {
            let text = std::fs::read_to_string(&path).unwrap();
            let report: MeasurementReport =
                serde_json::from_str(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
            let f2 = report.data.iter().find_map(|p| match &p.data {
                MeasurementData::ImpulseResponse { f2_hz, .. } => Some(*f2_hz),
                _ => None,
            });
            let s = report.ir_stats().expect("an impulse response");
            let fs = s.sample_rate_hz as f64;
            let flight = s
                .flight_time_s
                .map_or("-".to_string(), |f| format!("{}", (f * fs).round()));
            let opt = |v: Option<f64>| v.map_or("-".to_string(), |v| format!("{v:.2}"));
            let distance = match &s.distance_check {
                DistanceCheck::Consistent { excess_s, .. } => {
                    format!("Consistent excess={:+.1}", excess_s * fs)
                }
                DistanceCheck::TooEarly { excess_s, .. } => {
                    format!("TooEarly excess={:+.1}", excess_s * fs)
                }
                other => format!("{other:?}"),
            };
            println!(
                "{} f2={:.0} {:?} arrival={} flight={} gap={:?} margin={} lobe_offset={:?} \
                 arrival_snr={} distance_check={}",
                path.file_name().unwrap().to_string_lossy(),
                f2.unwrap_or(f64::NAN),
                s.arrival_cross_check,
                s.delay_samples,
                flight,
                s.broadband_delta_samples(),
                opt(s.arrival_lobe_margin_db),
                s.arrival_lobe_offset,
                opt(s.band_limited_snr_db),
                distance,
            );
        }
    }
}

/// #501: the coupling between [`PRE_IMPULSE_SNR_MIN_DB`] and `plot_ir`'s
/// default stimulus (`measurement::sweep::defaults`), recorded next to the
/// threshold whose provenance it is. Each test runs the chain `plot_ir`
/// runs — `log_sweep` → integer delay → 0.5 s tail → optional Gaussian
/// noise → `deconvolve_full` → ÷ amplitude → `extract_irs` (5 orders) →
/// `ir_peak` → `pre_impulse_snr_db` — at a −40 dBFS drive.
#[cfg(test)]
mod default_sweep_tests {
    use super::{
        ir_verdict, pre_impulse_region, ArrivalCrossCheck, ArrivalSource, IrVerdict,
        PreImpulseAnchor, ARRIVAL_SNR_MIN_DB, ARRIVAL_SNR_UNMEASURED_REASON,
        PRE_IMPULSE_SNR_MIN_DB,
    };
    use crate::measurement::sweep::{
        deconvolve_full, extract_irs, inverse_sweep, ir_default_window_len, ir_peak, log_sweep,
        pre_impulse_snr_db, pre_impulse_snr_db_before, pre_impulse_snr_floor_db, DeconvolvedIrs,
        SweepParams, IR_DEFAULT_DURATION_S, IR_DEFAULT_F1_HZ, IR_DEFAULT_F2_HZ,
        IR_DEFAULT_N_HARMONICS, IR_DEFAULT_TAIL_S,
    };

    /// −40 dBFS, the standing drive level the rig reading was taken at.
    const AMP: f64 = 0.01;
    /// pupu's measured loopback τ at 96 kHz, in samples.
    const RIG_TAU_96K: usize = 1711;
    /// The pre-impulse figure pupu read under the previous defaults (#501).
    const RIG_OLD_DEFAULTS_DB: f64 = 12.8;

    fn defaults(sample_rate: u32) -> (SweepParams, usize) {
        (
            SweepParams {
                f1_hz: IR_DEFAULT_F1_HZ,
                f2_hz: IR_DEFAULT_F2_HZ,
                duration_s: IR_DEFAULT_DURATION_S,
                sample_rate,
            },
            ir_default_window_len(sample_rate),
        )
    }

    /// The defaults before #501: 1 s and a 4096-sample window.
    fn old_defaults(sample_rate: u32) -> (SweepParams, usize) {
        (
            SweepParams {
                f1_hz: 20.0,
                f2_hz: 20_000.0,
                duration_s: 1.0,
                sample_rate,
            },
            4096,
        )
    }

    /// Standard-normal draws from a fixed-seed LCG (Box–Muller), so every
    /// run of these tests sees the same noise. The seed is the initial state
    /// as given: the increment is odd, so the generator has full period from
    /// any state, and forcing the seed odd would map seeds `2k` and `2k + 1`
    /// onto one capture.
    fn gaussian(n: usize, sigma: f64, seed: u64) -> Vec<f64> {
        let mut state = seed;
        let mut uniform = || {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((state >> 11) as f64 + 0.5) / (1u64 << 53) as f64
        };
        (0..n)
            .map(|_| {
                let (u1, u2) = (uniform(), uniform());
                sigma * (-2.0 * u1.ln()).sqrt() * (std::f64::consts::TAU * u2).cos()
            })
            .collect()
    }

    fn analyse(p: &SweepParams, window_len: usize, captured: &[f32]) -> DeconvolvedIrs {
        let inv = inverse_sweep(p).expect("inverse");
        let full: Vec<f64> = deconvolve_full(captured, &inv)
            .iter()
            .map(|v| v / AMP)
            .collect();
        extract_irs(&full, p, IR_DEFAULT_N_HARMONICS, window_len).expect("irs")
    }

    /// A perfect loopback delayed by `tau` samples, with optional added
    /// Gaussian noise `(dBFS rms, seed)`. Returns the figure and the IRs.
    fn loopback(
        p: &SweepParams,
        window_len: usize,
        tau: usize,
        noise: Option<(f64, u64)>,
    ) -> (f64, DeconvolvedIrs) {
        let sweep = log_sweep(p).expect("sweep");
        let tail = (IR_DEFAULT_TAIL_S * p.sample_rate as f64).round() as usize;
        let n = sweep.len() + tail;
        let mut captured = vec![0.0f64; n];
        for (i, &s) in sweep.iter().enumerate() {
            if let Some(slot) = captured.get_mut(i + tau) {
                *slot += AMP * s as f64;
            }
        }
        if let Some((dbfs, seed)) = noise {
            let sigma = 10f64.powf(dbfs / 20.0);
            for (c, w) in captured.iter_mut().zip(gaussian(n, sigma, seed)) {
                *c += w;
            }
        }
        let captured: Vec<f32> = captured.iter().map(|&v| v as f32).collect();
        let irs = analyse(p, window_len, &captured);
        let (idx, _) = ir_peak(&irs.linear);
        (pre_impulse_snr_db(&irs.linear, idx), irs)
    }

    /// A capture with no signal path: noise only. Returns what `ir_stats`
    /// would decide from it — peak, pre-region, figure — so the caller can
    /// apply either verdict rule with `ir_verdict`'s semantics (an empty
    /// pre-region is a failure, not +inf).
    struct NoSignal {
        peak_index: usize,
        peak_magnitude: f64,
        snr_db: f64,
        linear: Vec<f64>,
    }

    fn no_signal(p: &SweepParams, window_len: usize, seed: u64) -> NoSignal {
        let n = p.n_samples() + (IR_DEFAULT_TAIL_S * p.sample_rate as f64).round() as usize;
        let captured: Vec<f32> = gaussian(n, 1e-3, seed).iter().map(|&v| v as f32).collect();
        let irs = analyse(p, window_len, &captured);
        let (peak_index, peak_magnitude) = ir_peak(&irs.linear);
        NoSignal {
            peak_index,
            peak_magnitude,
            snr_db: pre_impulse_snr_db(&irs.linear, peak_index),
            linear: irs.linear,
        }
    }

    impl NoSignal {
        /// The shipped rule: `ir_verdict` against the fixed threshold.
        fn fixed_verdict(&self) -> IrVerdict {
            ir_verdict(
                self.peak_magnitude,
                pre_impulse_region(&self.linear, self.peak_index),
                self.snr_db,
            )
        }

        /// The rejected rule (#471 applied here): threshold = the floor at
        /// this draw's own argmax − 3 dB, falling back to 18 dB when no floor
        /// exists. Same fail-closed cases as `ir_verdict`.
        fn derived_accepts(&self, p: &SweepParams, window_len: usize) -> bool {
            let threshold = pre_impulse_snr_floor_db(p, window_len, self.peak_index)
                .map(|f| f - 3.0)
                .unwrap_or(PRE_IMPULSE_SNR_MIN_DB);
            self.peak_magnitude != 0.0
                && !pre_impulse_region(&self.linear, self.peak_index).is_empty()
                && self.snr_db >= threshold
        }
    }

    /// Test 1: the defaults clear the gate with margin on a perfect
    /// loopback over the whole plausible τ range (0–40 ms), and added
    /// noise does not move the figure. Margin: 2.0 dB = 4 × the ≤ 0.5 dB
    /// rig-vs-synthetic spread measured in #471 and #501 (multiplier
    /// assumed). Measured minimum 21.14 dB.
    #[test]
    fn default_sweep_clears_the_gate_on_a_perfect_loopback() {
        let sr = 96_000;
        let (p, wl) = defaults(sr);
        let taus = [0, 115, 1709, RIG_TAU_96K, 1967, 3840];
        for tau in taus {
            let (snr, _) = loopback(&p, wl, tau, None);
            assert!(
                snr >= PRE_IMPULSE_SNR_MIN_DB + 2.0,
                "τ = {tau} samples: a perfect loopback at the defaults reads {snr:.2} dB, \
                 under {:.1} dB + 2.0 dB margin",
                PRE_IMPULSE_SNR_MIN_DB
            );
            assert!(
                (21.0..22.1).contains(&snr),
                "τ = {tau}: {snr:.2} dB left the measured 21.1–22.0 dB range"
            );
        }
        let (clean, _) = loopback(&p, wl, RIG_TAU_96K, None);
        for dbfs in [-110.0, -70.0] {
            let (noisy, _) = loopback(&p, wl, RIG_TAU_96K, Some((dbfs, 0xA5A5)));
            assert!(
                (noisy - clean).abs() < 0.1,
                "noise at {dbfs} dBFS moved the default figure {clean:.2} → {noisy:.2} dB"
            );
        }
    }

    /// Test 2 (criteria 1, 2 and 6): the previous defaults are refused by
    /// the same 18 dB on the same perfect loopback, the synthetic reproduces
    /// the rig reading within 1.0 dB, and noise does not move it — the
    /// figure is the stimulus's, not the capture's. Also pins the
    /// band-limited run the rig passed at 32.7 dB. If the first assertion
    /// ever fails, the defaults change was unnecessary.
    #[test]
    fn previous_defaults_are_refused_by_the_shipped_threshold() {
        let sr = 96_000;
        let (p, wl) = old_defaults(sr);
        let (clean, _) = loopback(&p, wl, RIG_TAU_96K, None);
        assert!(
            clean < PRE_IMPULSE_SNR_MIN_DB,
            "the previous defaults read {clean:.2} dB, which the {PRE_IMPULSE_SNR_MIN_DB} dB \
             gate accepts — #501's premise no longer holds"
        );
        assert!(
            (clean - RIG_OLD_DEFAULTS_DB).abs() <= 1.0,
            "synthetic {clean:.2} dB is not within 1.0 dB of the rig's {RIG_OLD_DEFAULTS_DB} dB"
        );
        let (noisy, _) = loopback(&p, wl, RIG_TAU_96K, Some((-70.0, 0x5A5A)));
        assert!(
            (noisy - clean).abs() < 0.1,
            "noise moved the previous-default figure {clean:.2} → {noisy:.2} dB"
        );

        let typed = SweepParams {
            f1_hz: 200.0,
            f2_hz: 8_000.0,
            duration_s: 4.0,
            sample_rate: sr,
        };
        let (band_limited, _) = loopback(&typed, 16_384, RIG_TAU_96K, None);
        assert!(
            (band_limited - 32.7).abs() <= 1.0,
            "200 Hz–8 kHz / 4 s / 16384 reads {band_limited:.2} dB, not within 1.0 dB of 32.7"
        );
    }

    /// Test 3: the default window is set in seconds, so at every supported
    /// rate the linear gate is unclamped and the figure is the same.
    #[test]
    fn default_sweep_is_rate_independent_and_unclamped() {
        let tau_s = 0.017_8;
        let mut seen = Vec::new();
        for sr in [44_100u32, 48_000, 96_000, 192_000] {
            let (p, wl) = defaults(sr);
            let tau = (tau_s * sr as f64).round() as usize;
            let (snr, irs) = loopback(&p, wl, tau, None);
            assert_eq!(
                irs.window_len_used[0],
                (0.4 * sr as f64).round() as usize,
                "{sr} Hz: the default linear gate was clamped"
            );
            assert!(
                irs.clamp_note().is_none_or(|n| !n.contains("order 1 ")),
                "{sr} Hz: the linear IR must not appear in a clamp note"
            );
            seen.push(snr);
        }
        let spread = seen.iter().cloned().fold(f64::MIN, f64::max)
            - seen.iter().cloned().fold(f64::MAX, f64::min);
        assert!(
            spread < 0.2,
            "figure moved {spread:.2} dB across rates: {seen:?}"
        );
    }

    /// Test 4 (criterion 5): with no signal path, the fixed gate refuses
    /// every draw at the defaults. 48 kHz only — the figure does not depend
    /// on the rate (test 3). A draw whose peak lands at index 0 has an empty
    /// pre-region: `ir_verdict` refuses it, and its +inf figure is left out
    /// of `worst`. Measured over 200 distinct draws: maximum 15.49 dB, 2 with
    /// an empty pre-region (this 40-draw set: 13.44 dB, 1).
    #[test]
    fn default_sweep_refuses_a_capture_with_no_signal_path() {
        let (p, wl) = defaults(48_000);
        let mut worst = f64::MIN;
        let mut figured = 0;
        for seed in 0..NO_SIGNAL_DEFAULT_DRAWS {
            let draw = no_signal(&p, wl, NO_SIGNAL_DEFAULT_SEED ^ seed);
            if !pre_impulse_region(&draw.linear, draw.peak_index).is_empty() {
                worst = worst.max(draw.snr_db);
                figured += 1;
            }
            assert!(
                matches!(draw.fixed_verdict(), IrVerdict::Failed { .. }),
                "seed {seed}: a noise-only capture read {:.2} dB and was accepted",
                draw.snr_db
            );
        }
        assert!(
            figured * 2 > NO_SIGNAL_DEFAULT_DRAWS,
            "only {figured} of {NO_SIGNAL_DEFAULT_DRAWS} draws had a pre-region to judge"
        );
        assert!(
            worst < PRE_IMPULSE_SNR_MIN_DB,
            "worst noise-only draw {worst:.2} dB"
        );
    }

    /// Test 5, against the rejected implementation: at the previous
    /// defaults, #471's derived rule (floor at the draw's argmax − 3 dB)
    /// accepts a capture with no signal path. That is why #501 changed the
    /// defaults instead of deriving the gate. Measured: 26 of 200 distinct
    /// draws (this 60-draw set: 7).
    #[test]
    fn a_derived_threshold_would_accept_no_signal_at_the_previous_defaults() {
        let (p, wl) = old_defaults(96_000);
        let accepted = (0..NO_SIGNAL_OLD_DRAWS)
            .filter(|seed| no_signal(&p, wl, NO_SIGNAL_OLD_SEED ^ seed).derived_accepts(&p, wl))
            .count();
        assert!(
            accepted >= 1,
            "the derived rule refused all {NO_SIGNAL_OLD_DRAWS} noise-only draws — the \
             reason it was rejected for #501 no longer shows on this fixture"
        );
    }

    /// `linear` as `ir_stats` reads it: a default-band report at
    /// `sample_rate`. The verdict and figure under the shipped (#550)
    /// anchor come from `ir_stats` itself, not a copy of its rule.
    fn shipped_stats(linear: Vec<f64>, sample_rate: u32) -> super::IrStats {
        crate::measurement::report::fixtures::ir_report_with_custom_ir_band(
            linear,
            sample_rate,
            IR_DEFAULT_F2_HZ,
        )
        .ir_stats()
        .expect("an impulse response")
    }

    /// Test 6 (#550 negative control, architect revision 3): a capture
    /// with no signal path, 200 draws at 48 kHz and 200 at 96 kHz.
    /// 1. The rule before #550 (floor before the argmax) refuses every draw.
    /// 2. The rejected revision-2 rule, computed here — floor before
    ///    `min(arrival, peak)` whether or not the arrival is trusted —
    ///    accepts at least one: the proof this control can go red (9 on
    ///    the branch that falsified it). If a change to the synthetic chain
    ///    makes it accept none, the control no longer shows the shipped
    ///    rule doing anything; stop and report rather than drop this.
    /// 3. The shipped rule (`ir_stats().verdict`) refuses every draw. A
    ///    draw it accepted would falsify the design — return it to
    ///    `needs-design` with the count; do not add margin.
    /// 4. Coupling: no draw's measured band-limited SNR reaches
    ///    [`ARRIVAL_SNR_MIN_DB`], so none is trusted on a measured figure.
    ///    Fails if that threshold is lowered toward the noise crest (~13 dB;
    ///    15.15 dB the maximum over these 400).
    /// 5. Revision 4: a pick inside the guard band reads +inf (11 of 400 at
    ///    PR #578); it is not a measurement, so it is not trusted. Per draw:
    ///    anchor `PeakArrivalUnmeasured`, the peak's floor region, and the
    ///    verdict before #550, exactly. At least one such draw must occur.
    #[test]
    fn no_signal_is_refused_before_and_after_the_floor_anchor_moves() {
        let mut rejected_accepts = 0usize;
        let mut max_arrival_snr = f64::MIN;
        let mut unmeasured_arrivals = 0usize;
        for sr in [48_000u32, 96_000] {
            let (p, wl) = defaults(sr);
            for seed in 0..NO_SIGNAL_ANCHOR_DRAWS {
                let draw = no_signal(&p, wl, NO_SIGNAL_DEFAULT_SEED ^ seed);
                assert!(
                    matches!(draw.fixed_verdict(), IrVerdict::Failed { .. }),
                    "{sr} Hz seed {seed}: the rule before #550 accepted a noise-only draw \
                     at {:.2} dB",
                    draw.snr_db
                );
                let stats = shipped_stats(draw.linear.clone(), sr);

                let anchor = stats.arrival_index.min(stats.peak_index);
                let rejected = ir_verdict(
                    stats.peak_magnitude,
                    pre_impulse_region(&draw.linear, anchor),
                    pre_impulse_snr_db_before(&draw.linear, stats.peak_index, anchor),
                );
                if rejected == IrVerdict::Ok {
                    rejected_accepts += 1;
                }

                assert!(
                    matches!(stats.verdict, IrVerdict::Failed { .. }),
                    "{sr} Hz seed {seed}: the shipped anchor accepted a noise-only draw at \
                     {:.2} dB (anchor {:?}) — #550's design is falsified",
                    stats.pre_impulse_snr_db,
                    stats.pre_impulse_floor_anchor
                );
                if let Some(snr) = stats.band_limited_snr_db.filter(|s| s.is_finite()) {
                    max_arrival_snr = max_arrival_snr.max(snr);
                }
                // `pre_impulse_snr_db` reads +inf when the pick sits inside
                // the guard band — no floor, not a clear one (#577). Such a
                // pick is not trusted (revision 4): the floor stays before
                // the peak and the verdict is the one before #550, exactly.
                if stats.arrival_index < stats.peak_index
                    && pre_impulse_region(&draw.linear, stats.arrival_index).is_empty()
                {
                    unmeasured_arrivals += 1;
                    assert_eq!(
                        stats.pre_impulse_floor_anchor,
                        PreImpulseAnchor::PeakArrivalUnmeasured,
                        "{sr} Hz seed {seed}: unmeasured arrival at {}",
                        stats.arrival_index
                    );
                    assert_eq!(
                        stats.pre_impulse_floor_end,
                        pre_impulse_region(&draw.linear, stats.peak_index).len(),
                        "{sr} Hz seed {seed}: the floor left the peak's region"
                    );
                    assert_eq!(
                        stats.verdict,
                        draw.fixed_verdict(),
                        "{sr} Hz seed {seed}: the verdict differs from the rule before #550"
                    );
                    // #550 UX revision 4: the continuation on the real path.
                    let lines = stats.pre_impulse_floor_lines();
                    if !lines.is_empty() {
                        assert_eq!(
                            lines.get(1).map(String::as_str),
                            Some(
                                format!(
                                    "not before arrival \u{2014} {ARRIVAL_SNR_UNMEASURED_REASON}"
                                )
                                .as_str()
                            ),
                            "{sr} Hz seed {seed}: {lines:?}"
                        );
                    }
                }
            }
        }
        assert!(
            rejected_accepts >= 1,
            "the rejected min(arrival, peak) rule accepted no noise-only draw — this \
             control no longer shows the trust condition doing anything"
        );
        assert!(
            unmeasured_arrivals >= 1,
            "no noise-only draw picked an arrival inside the guard band (11 of 400 at PR \
             #578) — the unmeasured branch is no longer reached; stop and report"
        );
        assert!(
            max_arrival_snr < ARRIVAL_SNR_MIN_DB,
            "a noise-only draw's band-limited SNR reached {max_arrival_snr:.2} dB, against \
             ARRIVAL_SNR_MIN_DB {ARRIVAL_SNR_MIN_DB:.1} dB: noise would be trusted as an \
             arrival ({unmeasured_arrivals} draws picked inside the guard band, unmeasured)"
        );
    }

    /// Test 6b (#550 revision 4, QA on PR #578): an empty verdict floor
    /// means the peak sits inside the guard band, so `ir_verdict`'s "peak
    /// too close to the start of the gated window" is true. Against the
    /// rejected revision-3 rule, computed here — trust read from the
    /// standing alone — which empties the floor on a pick inside the guard
    /// band while the peak's own region is not empty (11 of 400 at PR
    /// #578): the proof this test can go red. Asserted on structure, not on
    /// the reason text.
    #[test]
    fn an_empty_floor_before_the_arrival_does_not_blame_the_peak() {
        let mut rejected_false_blames = 0usize;
        for sr in [48_000u32, 96_000] {
            let (p, wl) = defaults(sr);
            for seed in 0..NO_SIGNAL_ANCHOR_DRAWS {
                let draw = no_signal(&p, wl, NO_SIGNAL_DEFAULT_SEED ^ seed);
                let stats = shipped_stats(draw.linear.clone(), sr);
                let peak_region_empty =
                    pre_impulse_region(&draw.linear, stats.peak_index).is_empty();

                let rev3_trusted =
                    matches!(stats.arrival_source, ArrivalSource::BandLimitedPeak { .. })
                        && !matches!(
                            stats.arrival_cross_check,
                            ArrivalCrossCheck::BandLimitedSnrLow { .. }
                        );
                let rev3_anchor = if rev3_trusted {
                    stats.arrival_index.min(stats.peak_index)
                } else {
                    stats.peak_index
                };
                if pre_impulse_region(&draw.linear, rev3_anchor).is_empty() && !peak_region_empty {
                    rejected_false_blames += 1;
                }

                assert!(
                    stats.pre_impulse_floor_end != 0 || peak_region_empty,
                    "{sr} Hz seed {seed}: empty floor (anchor {:?}) with the peak at {} \
                     outside the guard band — the verdict blames the wrong index",
                    stats.pre_impulse_floor_anchor,
                    stats.peak_index
                );
            }
        }
        assert!(
            rejected_false_blames >= 1,
            "the rejected revision-3 rule emptied no floor with the peak outside the guard \
             band — this test no longer shows revision 4 doing anything"
        );
    }

    /// Test 7 (#550, the #501 regression): on a perfect loopback the
    /// arrival and the argmax are one sample, so the shipped anchor reads
    /// the figure #501 scored, to 0.01 dB, over the whole τ range.
    #[test]
    fn perfect_loopback_reads_the_same_under_the_shipped_anchor() {
        let sr = 96_000;
        let (p, wl) = defaults(sr);
        for tau in [0, 115, 1709, RIG_TAU_96K, 1967, 3840] {
            let (old, irs) = loopback(&p, wl, tau, None);
            let stats = shipped_stats(irs.linear, sr);
            assert!(
                (stats.pre_impulse_snr_db - old).abs() < 0.01,
                "τ = {tau}: shipped anchor {:.3} dB against #501's {old:.3} dB ({:?})",
                stats.pre_impulse_snr_db,
                stats.pre_impulse_floor_anchor
            );
            assert_eq!(stats.verdict, IrVerdict::Ok, "τ = {tau}");
        }
    }

    /// The linear IR of a perfect default-sweep loopback at `sample_rate`,
    /// delayed by `tau` samples, captured with `tail` samples after the
    /// sweep.
    fn loopback_with_tail(sample_rate: u32, tau: usize, tail: usize) -> Vec<f64> {
        let (p, wl) = defaults(sample_rate);
        let sweep = log_sweep(&p).expect("sweep");
        let mut captured = vec![0.0f32; sweep.len() + tail];
        for (i, &s) in sweep.iter().enumerate() {
            if let Some(slot) = captured.get_mut(i + tau) {
                *slot += (AMP * s as f64) as f32;
            }
        }
        analyse(&p, wl, &captured).linear
    }

    /// Whether two linear IRs are the same up to FFT round-off:
    /// `max |a − b| ≤ 1e-9 · max |a|` (#550 architect revision 3). The
    /// 1e-9 is margin over ~1e-13 f64 round-off at this size, assumed, not
    /// measured.
    fn same_linear_ir(a: &[f64], b: &[f64]) -> bool {
        let scale = a.iter().fold(0.0f64, |m, v| m.max(v.abs()));
        a.len() == b.len() && a.iter().zip(b).all(|(x, y)| (x - y).abs() <= 1e-9 * scale)
    }

    /// Test 8 (#550, what the scope line rests on): the linear IR — and so
    /// every quantity the gate reads — does not depend on the tail once
    /// the tail is at least half the window, so the scope line need not
    /// name the tail. Can-fail half: the same predicate is false with no
    /// tail, where the capture ends before the delayed sweep does (τ =
    /// 20 ms). If it held there, stop and report; do not loosen the
    /// predicate or change τ.
    #[test]
    fn the_figure_does_not_depend_on_a_tail_of_at_least_half_the_window() {
        let sr = 96_000;
        let tau = (0.020 * sr as f64).round() as usize;
        let half_window = ir_default_window_len(sr).div_ceil(2);
        let default_tail = (IR_DEFAULT_TAIL_S * sr as f64).round() as usize;
        let scored = loopback_with_tail(sr, tau, default_tail);
        let half = loopback_with_tail(sr, tau, half_window);
        assert!(
            same_linear_ir(&scored, &half),
            "tail W/2 changed the linear IR against tail 0.5 s"
        );
        let (half_snr, scored_snr) = (
            shipped_stats(half, sr).pre_impulse_snr_db,
            shipped_stats(scored.clone(), sr).pre_impulse_snr_db,
        );
        assert!(
            (half_snr - scored_snr).abs() < 1e-6,
            "tail W/2 reads {half_snr:.9} dB, tail 0.5 s {scored_snr:.9} dB"
        );
        let none = loopback_with_tail(sr, tau, 0);
        assert!(
            !same_linear_ir(&scored, &none),
            "a zero tail left the linear IR unchanged — this test cannot fail"
        );
    }

    const NO_SIGNAL_DEFAULT_SEED: u64 = 0x9E37_79B9_7F4A_7C15;
    const NO_SIGNAL_DEFAULT_DRAWS: u64 = 40;
    /// Per rate, for the #550 negative control (architect revision 3).
    const NO_SIGNAL_ANCHOR_DRAWS: u64 = 200;
    const NO_SIGNAL_OLD_SEED: u64 = 0xC2B2_AE3D_27D4_EB4F;
    const NO_SIGNAL_OLD_DRAWS: u64 = 60;

    /// Tests 4 and 5 count draws; each draw must be a distinct capture, or
    /// the count overstates the coverage. Before this check, `gaussian`
    /// forced its seed odd and each loop ran half its draws twice.
    #[test]
    fn no_signal_draws_are_distinct_captures() {
        for (base, draws) in [
            (NO_SIGNAL_DEFAULT_SEED, NO_SIGNAL_DEFAULT_DRAWS),
            (NO_SIGNAL_OLD_SEED, NO_SIGNAL_OLD_DRAWS),
        ] {
            let heads: std::collections::HashSet<Vec<u64>> = (0..draws)
                .map(|seed| {
                    gaussian(8, 1.0, base ^ seed)
                        .iter()
                        .map(|v| v.to_bits())
                        .collect()
                })
                .collect();
            assert_eq!(
                heads.len() as u64,
                draws,
                "seed base {base:#x}: {draws} draws give {} distinct captures",
                heads.len()
            );
        }
    }
}
