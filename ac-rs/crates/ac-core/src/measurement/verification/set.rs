//! The run set: validity, ordering, per-run status, mic-curve provenance,
//! and assembling the statistics over the runs fit to aggregate.

use super::stats::{self, Band, Flatness, Grid, HarmonicRow, RunSeries, Verdict};
use super::{GAIN_BAND_HZ, GAIN_SPREAD_LIMIT_DB, TONAL_BANDS_HZ, TONAL_STABILITY_LIMIT_DB};
use crate::measurement::report::{
    GateParams, IrVerdict, MeasurementData, MeasurementReport, MicResponseRef, TailDecayRecord,
    PRE_IMPULSE_SNR_MIN_DB,
};
use crate::measurement::sweep::HarmonicIr;
use crate::shared::calibration::MicResponse;

/// One archived report and the path it was read from. The path is how a
/// refusal names the run, since no run numbering exists until the set is
/// accepted.
#[derive(Debug, Clone)]
pub struct RunInput {
    pub file: String,
    pub report: MeasurementReport,
}

/// A field every run of a set must share.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetField {
    SampleRate,
    SweepStart,
    SweepStop,
    SweepDuration,
    GateLength,
    GateWindow,
    FrequencyGrid,
}

impl SetField {
    /// Gate fields — and the grid a gate produces — rather than sweep
    /// fields.
    pub fn is_gate(self) -> bool {
        matches!(
            self,
            SetField::GateLength | SetField::GateWindow | SetField::FrequencyGrid
        )
    }
}

/// One run's value of a [`SetField`], unformatted.
#[derive(Debug, Clone, PartialEq)]
pub enum FieldValue {
    Hz(f64),
    Seconds(f64),
    Text(String),
    NotRecorded,
    /// A gated-response grid: point count and first/last frequency.
    Grid {
        points: usize,
        first_hz: f64,
        last_hz: f64,
    },
    /// Same count and range as the other run's grid; the first bin where
    /// the two differ.
    GridDiffersAt {
        freq_hz: f64,
    },
}

/// The first field two runs disagree on.
#[derive(Debug, Clone, PartialEq)]
pub struct Mismatch {
    pub field: SetField,
    pub first: (String, FieldValue),
    pub other: (String, FieldValue),
}

/// A microphone curve as a report or the command line identifies it. The
/// report can print identities; it cannot prove two are the same data.
#[derive(Debug, Clone, PartialEq)]
pub struct CurveIdentity {
    pub source_path: Option<String>,
    pub n_points: usize,
    /// Import time into `cal.json`. Only capture-time curves have one; a
    /// file supplied on the command line was never imported.
    pub imported_at: Option<String>,
}

impl From<&MicResponseRef> for CurveIdentity {
    fn from(r: &MicResponseRef) -> Self {
        Self {
            source_path: r.source_path.clone(),
            n_points: r.n_points,
            imported_at: Some(r.imported_at.clone()),
        }
    }
}

impl CurveIdentity {
    /// A curve supplied at render time.
    pub fn supplied(curve: &MicResponse) -> Self {
        Self {
            source_path: curve.source_path.clone(),
            n_points: curve.freqs_hz.len(),
            imported_at: None,
        }
    }
}

/// Why a set was refused. Nothing is computed or written for a refused
/// set.
#[derive(Debug, Clone, PartialEq)]
pub enum VerificationRefusal {
    TooFewReports {
        given: usize,
    },
    /// Missing the impulse-response or gated-response payload.
    NotPlotIr {
        file: String,
        missing: Vec<&'static str>,
    },
    /// Runs are not one sweep, or not one gate and grid.
    NotOneSet(Mismatch),
    /// Two files carry the same capture timestamp.
    SameRunTwice {
        captured: String,
        files: Vec<String>,
    },
    /// Some runs were corrected at capture and some were not, and no curve
    /// was supplied to correct the rest.
    MixedCorrectionNoCurve {
        at_capture: Vec<String>,
        raw: Vec<String>,
    },
    /// A curve was supplied for a set every run of which was already
    /// corrected at capture.
    CurveSuppliedAllCorrected {
        supplied: CurveIdentity,
    },
}

/// How a run's frequency response was corrected for the microphone.
#[derive(Debug, Clone, PartialEq)]
pub enum RunMic {
    NotApplied,
    /// At capture, with the curve the calibration snapshot recorded, when
    /// it recorded one.
    AtCapture(Option<CurveIdentity>),
    /// By this computation, with the supplied curve.
    PostHoc,
}

/// Why a run was left out of every figure.
#[derive(Debug, Clone, PartialEq)]
pub enum Exclusion {
    /// The pre-impulse SNR verdict (#376) failed. `snr_db` is the measured
    /// value when one exists; `reason` is the verdict's own wording.
    PreImpulse {
        snr_db: Option<f64>,
        required_db: f64,
        reason: String,
    },
    /// ISO 18233 §6.3.2: the captured tail did not decay far enough.
    TailDecayFailed {
        decay_db: f64,
        band_hz: f64,
        required_db: f64,
    },
    /// The tail-decay check could not run; fails closed.
    TailDecayNotEvaluated { reason: String },
}

#[derive(Debug, Clone, PartialEq)]
pub enum RunStatus {
    Used,
    /// Used, but its tail decay was never recorded (schema v≤12), so
    /// ISO 18233 §6.3.2 is unverified for it.
    Unchecked,
    Excluded(Vec<Exclusion>),
}

impl RunStatus {
    /// Used and unchecked runs feed every figure; excluded runs feed none.
    pub fn is_used(&self) -> bool {
        !matches!(self, RunStatus::Excluded(_))
    }
}

/// One run, in set order.
#[derive(Debug, Clone, PartialEq)]
pub struct Run {
    /// 1-based, after ordering.
    pub number: usize,
    pub file: String,
    pub timestamp_utc: String,
    pub schema_version: u32,
    pub level_dbfs: f64,
    /// `None` when the report yields no IR statistics.
    pub pre_impulse_snr_db: Option<f64>,
    /// `None` on reports older than v13: not recorded.
    pub tail_decay: Option<TailDecayRecord>,
    pub status: RunStatus,
    pub mic: RunMic,
    pub distance_m: Option<f64>,
    /// Median of the corrected response in the gain band; `None` for an
    /// excluded run or an empty band.
    pub gain_db: Option<f64>,
}

/// The sweep every run shares.
#[derive(Debug, Clone, PartialEq)]
pub struct SweepSummary {
    pub sample_rate_hz: u32,
    pub f1_hz: f64,
    pub f2_hz: f64,
    pub duration_s: f64,
}

/// How the set was corrected for the microphone.
#[derive(Debug, Clone, PartialEq)]
pub struct MicProvenance {
    pub supplied: Option<CurveIdentity>,
    /// Distinct capture-time curves, in run order.
    pub at_capture: Vec<CurveIdentity>,
}

/// An accepted, computed set.
#[derive(Debug, Clone, PartialEq)]
pub struct VerificationSet {
    pub runs: Vec<Run>,
    pub sweep: SweepSummary,
    pub gate: Option<GateParams>,
    pub mic: MicProvenance,
    pub gain_spread: Verdict,
    /// One per [`TONAL_BANDS_HZ`] band.
    pub tonal_stability: Vec<Verdict>,
    pub harmonics: Vec<HarmonicRow>,
    /// One per [`TONAL_BANDS_HZ`] band.
    pub flatness: Vec<Flatness>,
    /// `n_r(f) − m(f)` per used run, over the tonal bands.
    pub deviations: Vec<RunSeries>,
    /// The tonal bands as clipped, for marking on the deviation chart.
    pub deviation_bands: Vec<Band>,
    /// `M(f)`, the mean corrected response of the used runs.
    pub mean_response: Vec<(f64, f64)>,
}

impl VerificationSet {
    /// Numbers of the runs that feed the figures.
    pub fn used_runs(&self) -> Vec<usize> {
        self.runs
            .iter()
            .filter(|r| r.status.is_used())
            .map(|r| r.number)
            .collect()
    }
}

/// The payloads a run is read from.
struct Parts<'a> {
    sample_rate_hz: u32,
    f1_hz: f64,
    f2_hz: f64,
    duration_s: f64,
    linear_ir: &'a [f64],
    harmonics: &'a [HarmonicIr],
    gated: Vec<(f64, f64)>,
    gate: Option<&'a GateParams>,
}

fn parts(report: &MeasurementReport) -> Result<Parts<'_>, Vec<&'static str>> {
    let ir = report.data.iter().find_map(|p| match &p.data {
        MeasurementData::ImpulseResponse {
            sample_rate_hz,
            f1_hz,
            f2_hz,
            duration_s,
            linear_ir,
            harmonics,
            ..
        } if !linear_ir.is_empty() => Some((
            *sample_rate_hz,
            *f1_hz,
            *f2_hz,
            *duration_s,
            linear_ir,
            harmonics,
        )),
        _ => None,
    });
    let gated = report.data.iter().find_map(|p| match &p.data {
        MeasurementData::GatedFrequencyResponse { points } => Some((points, p.gate.as_ref())),
        _ => None,
    });
    match (ir, gated) {
        (
            Some((sample_rate_hz, f1_hz, f2_hz, duration_s, linear_ir, harmonics)),
            Some((points, gate)),
        ) => Ok(Parts {
            sample_rate_hz,
            f1_hz,
            f2_hz,
            duration_s,
            linear_ir,
            harmonics,
            gated: points.iter().map(|p| (p.freq_hz, p.magnitude_db)).collect(),
            gate,
        }),
        (ir, gated) => {
            let mut missing = Vec::new();
            if ir.is_none() {
                missing.push("impulse response");
            }
            if gated.is_none() {
                missing.push("gated frequency response");
            }
            Err(missing)
        }
    }
}

/// The first field `b` differs from `a` on, with both values.
fn first_mismatch(a: &Parts, b: &Parts) -> Option<(SetField, FieldValue, FieldValue)> {
    use FieldValue::*;
    if a.sample_rate_hz != b.sample_rate_hz {
        return Some((
            SetField::SampleRate,
            Hz(a.sample_rate_hz as f64),
            Hz(b.sample_rate_hz as f64),
        ));
    }
    for (field, x, y) in [
        (SetField::SweepStart, a.f1_hz, b.f1_hz),
        (SetField::SweepStop, a.f2_hz, b.f2_hz),
    ] {
        if x != y {
            return Some((field, Hz(x), Hz(y)));
        }
    }
    if a.duration_s != b.duration_s {
        return Some((
            SetField::SweepDuration,
            Seconds(a.duration_s),
            Seconds(b.duration_s),
        ));
    }
    let length = |p: &Parts| p.gate.map_or(NotRecorded, |g| Seconds(g.gate_length_s));
    let (la, lb) = (length(a), length(b));
    if la != lb {
        return Some((SetField::GateLength, la, lb));
    }
    let window = |p: &Parts| p.gate.map_or(NotRecorded, |g| Text(g.window_kind.clone()));
    let (wa, wb) = (window(a), window(b));
    if wa != wb {
        return Some((SetField::GateWindow, wa, wb));
    }
    let grid = |p: &Parts| Grid {
        points: p.gated.len(),
        first_hz: p.gated.first().map_or(f64::NAN, |x| x.0),
        last_hz: p.gated.last().map_or(f64::NAN, |x| x.0),
    };
    let (ga, gb) = (grid(a), grid(b));
    if ga != gb {
        return Some((SetField::FrequencyGrid, ga, gb));
    }
    let differs = a
        .gated
        .iter()
        .zip(&b.gated)
        .find(|(x, y)| x.0 != y.0)
        .map(|(x, _)| x.0);
    differs.map(|freq_hz| (SetField::FrequencyGrid, ga, GridDiffersAt { freq_hz }))
}

fn status(
    report: &MeasurementReport,
    snr_db: Option<f64>,
    verdict: Option<&IrVerdict>,
) -> RunStatus {
    let mut excluded = Vec::new();
    match verdict {
        Some(IrVerdict::Ok) => {}
        Some(IrVerdict::Failed { reason }) => excluded.push(Exclusion::PreImpulse {
            snr_db: snr_db.filter(|v| v.is_finite()),
            required_db: PRE_IMPULSE_SNR_MIN_DB,
            reason: reason.clone(),
        }),
        None => excluded.push(Exclusion::PreImpulse {
            snr_db: None,
            required_db: PRE_IMPULSE_SNR_MIN_DB,
            reason: "no impulse-response statistics".into(),
        }),
    }
    let mut unchecked = false;
    match &report.tail_decay {
        Some(TailDecayRecord::Checked(c)) if !c.passed => {
            excluded.push(Exclusion::TailDecayFailed {
                decay_db: c.worst_decay_db,
                band_hz: c.worst_band_hz,
                required_db: c.required_db,
            });
        }
        Some(TailDecayRecord::Checked(_)) => {}
        Some(TailDecayRecord::NotEvaluated { reason }) => {
            excluded.push(Exclusion::TailDecayNotEvaluated {
                reason: reason.clone(),
            });
        }
        None => unchecked = true,
    }
    if !excluded.is_empty() {
        RunStatus::Excluded(excluded)
    } else if unchecked {
        RunStatus::Unchecked
    } else {
        RunStatus::Used
    }
}

/// Validate, order and compute a set of `plot ir` reports. `curve` is a
/// microphone curve supplied at render time: it is applied to the gated
/// frequency response of every run not already corrected at capture, and
/// never to the impulse response or the harmonics.
pub fn verify(
    inputs: Vec<RunInput>,
    curve: Option<&MicResponse>,
) -> Result<VerificationSet, VerificationRefusal> {
    if inputs.len() < 2 {
        return Err(VerificationRefusal::TooFewReports {
            given: inputs.len(),
        });
    }
    let mut all = Vec::with_capacity(inputs.len());
    for input in &inputs {
        match parts(&input.report) {
            Ok(p) => all.push(p),
            Err(missing) => {
                return Err(VerificationRefusal::NotPlotIr {
                    file: input.file.clone(),
                    missing,
                })
            }
        }
    }
    for (input, p) in inputs.iter().zip(&all).skip(1) {
        if let Some((field, a, b)) = first_mismatch(&all[0], p) {
            return Err(VerificationRefusal::NotOneSet(Mismatch {
                field,
                first: (inputs[0].file.clone(), a),
                other: (input.file.clone(), b),
            }));
        }
    }
    for (i, a) in inputs.iter().enumerate() {
        let twins: Vec<String> = inputs[i..]
            .iter()
            .filter(|b| b.report.timestamp_utc == a.report.timestamp_utc)
            .map(|b| b.file.clone())
            .collect();
        if twins.len() > 1 {
            return Err(VerificationRefusal::SameRunTwice {
                captured: a.report.timestamp_utc.clone(),
                files: twins,
            });
        }
    }
    let corrected = |i: &RunInput| i.report.processing_chain.mic_correction_applied;
    let (at_capture, raw): (Vec<&RunInput>, Vec<&RunInput>) =
        inputs.iter().partition(|i| corrected(i));
    match curve {
        Some(c) if raw.is_empty() => {
            return Err(VerificationRefusal::CurveSuppliedAllCorrected {
                supplied: CurveIdentity::supplied(c),
            })
        }
        None if !at_capture.is_empty() && !raw.is_empty() => {
            return Err(VerificationRefusal::MixedCorrectionNoCurve {
                at_capture: at_capture.iter().map(|i| i.file.clone()).collect(),
                raw: raw.iter().map(|i| i.file.clone()).collect(),
            })
        }
        _ => {}
    }

    // Ascending drive, ties broken by capture time.
    let mut order: Vec<usize> = (0..inputs.len()).collect();
    order.sort_by(|&a, &b| {
        let (ra, rb) = (&inputs[a].report, &inputs[b].report);
        ra.stimulus
            .level_dbfs
            .total_cmp(&rb.stimulus.level_dbfs)
            .then_with(|| ra.timestamp_utc.cmp(&rb.timestamp_utc))
    });

    // The frequencies the sweep and the gate resolve; the set is one sweep
    // and one gate, so run 1's hold for all. Bins outside never reach the
    // grid: a band clipped at the sweep stop would otherwise admit an edge
    // cell whose mean reaches up to 1/12 octave past `f2_hz`.
    let lo_hz = all[0].f1_hz.max(all[0].gate.map_or(0.0, |g| g.f_low_hz));
    let hi_hz = all[0].f2_hz;
    let resolved = |f: f64| f >= lo_hz && f <= hi_hz;

    let mut runs = Vec::with_capacity(order.len());
    let mut grids: Vec<Option<Grid>> = Vec::with_capacity(order.len());
    for (n, &i) in order.iter().enumerate() {
        let report = &inputs[i].report;
        let p = &all[i];
        let ir_stats = report.ir_stats();
        let snr = ir_stats.as_ref().map(|s| s.pre_impulse_snr_db);
        let status = status(report, snr, ir_stats.as_ref().map(|s| &s.verdict));
        let mic = if report.processing_chain.mic_correction_applied {
            RunMic::AtCapture(
                report
                    .calibration
                    .as_ref()
                    .and_then(|c| c.mic_response.as_ref())
                    .map(CurveIdentity::from),
            )
        } else if curve.is_some() {
            RunMic::PostHoc
        } else {
            RunMic::NotApplied
        };
        let grid = status.is_used().then(|| {
            let inside = p.gated.iter().filter(|&&(f, _)| resolved(f));
            let points: Vec<(f64, f64)> = match (&mic, curve) {
                (RunMic::PostHoc, Some(c)) => {
                    inside.map(|&(f, db)| (f, c.corrected_db(f, db))).collect()
                }
                _ => inside.copied().collect(),
            };
            stats::sixth_octave_means(&points)
        });
        grids.push(grid);
        runs.push(Run {
            number: n + 1,
            file: inputs[i].file.clone(),
            timestamp_utc: report.timestamp_utc.clone(),
            schema_version: report.schema_version,
            level_dbfs: report.stimulus.level_dbfs,
            pre_impulse_snr_db: snr,
            tail_decay: report.tail_decay.clone(),
            status,
            mic,
            distance_m: report.position.as_ref().and_then(|p| p.distance_m),
            gain_db: None,
        });
    }

    let first = &all[0];
    let sweep = SweepSummary {
        sample_rate_hz: first.sample_rate_hz,
        f1_hz: first.f1_hz,
        f2_hz: first.f2_hz,
        duration_s: first.duration_s,
    };
    let gate = first.gate.cloned();
    let f_low = gate.as_ref().map(|g| g.f_low_hz);
    let clip = |(lo, hi): (f64, f64)| {
        let nominal = Band::new(lo, hi);
        (
            nominal,
            stats::clip_band(nominal, sweep.f1_hz, sweep.f2_hz, f_low),
        )
    };

    let used: Vec<&Grid> = grids.iter().flatten().collect();
    let used_idx: Vec<usize> = grids
        .iter()
        .enumerate()
        .filter(|(_, g)| g.is_some())
        .map(|(i, _)| i)
        .collect();

    let (gain_nominal, gain_band) = clip(GAIN_BAND_HZ);
    for (i, g) in used_idx.iter().zip(stats::gains(&used, gain_band.clone())) {
        runs[*i].gain_db = g;
    }
    let gain_spread = Verdict {
        band: gain_band.clone().unwrap_or(gain_nominal),
        value: stats::gain_spread(&used, gain_band),
        limit_db: GAIN_SPREAD_LIMIT_DB,
    };

    let mut tonal_stability = Vec::new();
    let mut flatness = Vec::new();
    let mut deviation_bands = Vec::new();
    let mut deviations: Vec<RunSeries> = used_idx
        .iter()
        .map(|i| RunSeries {
            run: runs[*i].number,
            points: Vec::new(),
        })
        .collect();
    for band in TONAL_BANDS_HZ {
        let (nominal, clipped) = clip(band);
        let shown = clipped.clone().unwrap_or(nominal);
        tonal_stability.push(Verdict {
            band: shown,
            value: stats::tonal_stability(&used, clipped.clone()),
            limit_db: TONAL_STABILITY_LIMIT_DB,
        });
        flatness.push(Flatness {
            band: shown,
            value: stats::flatness(&used, clipped.clone()),
        });
        if let Ok(b) = clipped {
            deviation_bands.push(b);
        }
        for (series, dev) in deviations.iter_mut().zip(stats::deviations(&used, clipped)) {
            series.points.extend(dev);
        }
    }

    let drives: Vec<f64> = used_idx.iter().map(|i| runs[*i].level_dbfs).collect();
    let mut orders: Vec<u32> = used_idx
        .iter()
        .flat_map(|i| all[order[*i]].harmonics.iter().map(|h| h.order))
        .collect();
    orders.sort_unstable();
    orders.dedup();
    let harmonics = orders
        .into_iter()
        .map(|n| {
            let levels: Vec<Option<f64>> = used_idx
                .iter()
                .map(|i| {
                    let p = &all[order[*i]];
                    p.harmonics
                        .iter()
                        .find(|h| h.order == n)
                        .and_then(|h| stats::harmonic_level_db(&h.samples, p.linear_ir))
                })
                .collect();
            stats::harmonic_row(n, &drives, &levels)
        })
        .collect();

    let mean_response = stats::mean_grid(&used)
        .into_iter()
        .map(|(k, v)| (stats::centre_hz(k), v))
        .collect();

    let mut at_capture_ids: Vec<CurveIdentity> = Vec::new();
    for r in &runs {
        if let RunMic::AtCapture(Some(id)) = &r.mic {
            if !at_capture_ids.contains(id) {
                at_capture_ids.push(id.clone());
            }
        }
    }

    Ok(VerificationSet {
        runs,
        sweep,
        gate,
        mic: MicProvenance {
            supplied: curve.map(CurveIdentity::supplied),
            at_capture: at_capture_ids,
        },
        gain_spread,
        tonal_stability,
        harmonics,
        flatness,
        deviations,
        deviation_bands,
        mean_response,
    })
}

#[cfg(test)]
mod tests {
    use super::super::testkit::{bins, run, tail_failed};
    use super::super::{Figure, HarmonicReading, NotComputed, TAIL_DECAY_REQUIRED_DB};
    use super::*;
    use crate::measurement::report::MicResponseRef;
    use crate::measurement::sweep::{check_tail_decay, SweepParams};

    fn input(file: &str, report: MeasurementReport) -> RunInput {
        RunInput {
            file: file.into(),
            report,
        }
    }

    /// A tilted response with a little per-run texture, so statistics are
    /// not trivially zero.
    fn response(seed: f64) -> impl Fn(f64) -> f64 {
        move |f: f64| -20.0 - 2.0 * (f / 1_000.0).log2() + 0.1 * (seed * f / 700.0).sin()
    }

    fn three_runs() -> Vec<RunInput> {
        let freqs = bins(100.0);
        vec![
            input(
                "b.json",
                run(
                    "2026-09-24T10:14:47Z",
                    -30.0,
                    &freqs,
                    response(2.0),
                    &[(2, 0.01), (4, 1e-4)],
                ),
            ),
            input(
                "a.json",
                run(
                    "2026-09-24T10:12:04Z",
                    -40.0,
                    &freqs,
                    response(1.0),
                    &[(2, 0.00316), (4, 1e-4)],
                ),
            ),
            input(
                "c.json",
                run(
                    "2026-09-24T10:17:22Z",
                    -20.0,
                    &freqs,
                    response(3.0),
                    &[(2, 0.0316), (4, 1e-4)],
                ),
            ),
        ]
    }

    #[test]
    fn runs_are_ordered_by_drive_not_argument_order() {
        let set = verify(three_runs(), None).unwrap();
        let files: Vec<&str> = set.runs.iter().map(|r| r.file.as_str()).collect();
        assert_eq!(files, ["a.json", "b.json", "c.json"]);
        assert_eq!(
            set.runs.iter().map(|r| r.number).collect::<Vec<_>>(),
            [1, 2, 3]
        );
        assert!(set.runs.iter().all(|r| r.status == RunStatus::Used));
    }

    /// Two identical IRs at −40 and −30 dBFS read zero gain spread. The
    /// rejected implementation subtracts `level_dbfs` from each run's gain
    /// — a second normalisation on top of `plot_ir`'s own division by the
    /// stimulus amplitude — and reads the 10 dB of drive as a 10 dB gain
    /// error.
    #[test]
    fn identical_runs_at_two_drives_read_zero_gain_spread() {
        let freqs = bins(100.0);
        let flat = |_: f64| -12.0;
        let set = verify(
            vec![
                input("a", run("t1", -40.0, &freqs, flat, &[])),
                input("b", run("t2", -30.0, &freqs, flat, &[])),
            ],
            None,
        )
        .unwrap();
        assert_eq!(set.gain_spread.value, Figure::Value(0.0));
        assert_eq!(set.gain_spread.passed(), Some(true));

        let rejected: Vec<f64> = set
            .runs
            .iter()
            .map(|r| r.gain_db.unwrap() - r.level_dbfs)
            .collect();
        let rejected_spread = rejected.iter().copied().fold(f64::MIN, f64::max)
            - rejected.iter().copied().fold(f64::MAX, f64::min);
        assert!((rejected_spread - 10.0).abs() < 1e-9, "{rejected_spread}");
    }

    /// A run failing tail decay is excluded and changes no aggregate: the
    /// set's figures are bit-equal with and without it in the input.
    #[test]
    fn a_failed_tail_decay_run_changes_no_aggregate() {
        let without = verify(three_runs(), None).unwrap();

        let mut inputs = three_runs();
        let freqs = bins(100.0);
        // Its response is wildly different, so any leak would show.
        let mut bad = run(
            "2026-09-24T10:20:00Z",
            -35.0,
            &freqs,
            |f| -60.0 + f / 1_000.0,
            &[(2, 0.9)],
        );
        bad.tail_decay = Some(tail_failed());
        inputs.push(input("bad.json", bad));
        let with = verify(inputs, None).unwrap();

        let excluded = with.runs.iter().find(|r| r.file == "bad.json").unwrap();
        assert!(matches!(
            &excluded.status,
            RunStatus::Excluded(e) if matches!(e[..], [Exclusion::TailDecayFailed { .. }])
        ));
        assert_eq!(with.runs.len(), 4, "the excluded run stays visible");
        assert_eq!(with.gain_spread, without.gain_spread);
        assert_eq!(with.tonal_stability, without.tonal_stability);
        assert_eq!(with.flatness, without.flatness);
        assert_eq!(with.harmonics, without.harmonics);
        assert_eq!(with.mean_response, without.mean_response);
        // Deviation series are keyed by run number, which the excluded run
        // shifted; the values are the same.
        let pts = |s: &VerificationSet| {
            s.deviations
                .iter()
                .map(|d| d.points.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(pts(&with), pts(&without));
    }

    #[test]
    fn a_failed_pre_impulse_snr_excludes_the_run() {
        let mut inputs = three_runs();
        if let MeasurementData::ImpulseResponse { linear_ir, .. } =
            &mut inputs[0].report.data[0].data
        {
            for (i, v) in linear_ir.iter_mut().enumerate().take(400) {
                *v = if i % 2 == 0 { 0.5 } else { -0.5 };
            }
        }
        let set = verify(inputs, None).unwrap();
        let r = set.runs.iter().find(|r| r.file == "b.json").unwrap();
        assert!(
            matches!(&r.status, RunStatus::Excluded(e) if matches!(e[..], [Exclusion::PreImpulse { .. }])),
            "{:?}",
            r.status
        );
    }

    #[test]
    fn a_v12_run_is_used_but_unchecked_and_not_evaluated_is_excluded() {
        let mut inputs = three_runs();
        inputs[0].report.tail_decay = None;
        inputs[0].report.schema_version = 12;
        inputs[1].report.tail_decay = Some(TailDecayRecord::NotEvaluated {
            reason: "capture too short".into(),
        });
        let set = verify(inputs, None).unwrap();
        let by = |f: &str| {
            set.runs
                .iter()
                .find(|r| r.file == f)
                .unwrap()
                .status
                .clone()
        };
        assert_eq!(by("b.json"), RunStatus::Unchecked);
        assert!(by("b.json").is_used());
        assert_eq!(
            by("a.json"),
            RunStatus::Excluded(vec![Exclusion::TailDecayNotEvaluated {
                reason: "capture too short".into()
            }])
        );
    }

    /// H2 generated to grow with drive at its order's rate reads
    /// `tracks drive`; a constant H4 reads `floor-limited`.
    #[test]
    fn harmonic_tracking_and_floor_limited_orders() {
        let set = verify(three_runs(), None).unwrap();
        let row = |n: u32| set.harmonics.iter().find(|h| h.order == n).unwrap();
        assert_eq!(row(2).reading(), Some(HarmonicReading::TracksDrive));
        assert!((row(2).slope.value().unwrap() - 10.0).abs() < 0.1);
        assert_eq!(row(4).reading(), Some(HarmonicReading::FloorLimited));
        // The level is read at the highest used drive (-20 dBFS here).
        assert!((row(2).level_db.unwrap() - 20.0 * 0.0316f64.log10()).abs() < 1e-6);
    }

    #[test]
    fn fewer_than_two_used_runs_computes_no_statistic() {
        let mut inputs = three_runs();
        inputs[0].report.tail_decay = Some(tail_failed());
        inputs[2].report.tail_decay = Some(tail_failed());
        let set = verify(inputs, None).unwrap();
        let none = Figure::NotComputed(NotComputed::TooFewRuns { used: 1 });
        assert_eq!(set.gain_spread.value, none);
        assert!(set.tonal_stability.iter().all(|v| v.value == none));
        assert!(set.harmonics.iter().all(|h| h.slope == none));
    }

    #[test]
    fn a_band_above_the_sweep_stop_is_not_computed_and_named() {
        let mut inputs = three_runs();
        for i in &mut inputs {
            if let MeasurementData::ImpulseResponse { f2_hz, .. } = &mut i.report.data[0].data {
                *f2_hz = 4_000.0;
            }
        }
        let set = verify(inputs, None).unwrap();
        assert_eq!(
            set.tonal_stability[1].value,
            Figure::NotComputed(NotComputed::AboveSweepStop { f2_hz: 4_000.0 })
        );
        assert_eq!(set.tonal_stability[1].band, Band::new(5_000.0, 16_000.0));
        assert_eq!(set.tonal_stability[0].band, Band::new(1_500.0, 4_000.0));
    }

    /// A band clipped at the sweep stop reads no bin above it: flooding every
    /// bin above f2 with +40 dB must not move any statistic.
    #[test]
    fn a_clipped_band_reads_no_bin_above_the_sweep_stop() {
        let f2 = 11_500.0;
        let with_stop = |mut v: Vec<RunInput>| {
            for i in &mut v {
                if let MeasurementData::ImpulseResponse { f2_hz, .. } = &mut i.report.data[0].data {
                    *f2_hz = f2;
                }
            }
            v
        };
        let clean = verify(with_stop(three_runs()), None).unwrap();
        let mut dirty = with_stop(three_runs());
        for (n, i) in dirty.iter_mut().enumerate() {
            if let MeasurementData::GatedFrequencyResponse { points } = &mut i.report.data[1].data {
                for p in points.iter_mut().filter(|p| p.freq_hz > f2) {
                    // Differs per run, so spreads would move.
                    p.magnitude_db += 40.0 + n as f64;
                }
            }
        }
        let dirty = verify(dirty, None).unwrap();
        assert_eq!(clean.tonal_stability[1].band, Band::new(5_000.0, f2));
        assert!(clean.gain_spread.value.value().is_some());
        assert_eq!(clean.gain_spread, dirty.gain_spread);
        assert_eq!(clean.tonal_stability, dirty.tonal_stability);
        assert_eq!(clean.flatness, dirty.flatness);
    }

    fn curve() -> MicResponse {
        let freqs: Vec<f32> = (0..32).map(|i| 20.0 * 1.25f32.powi(i)).collect();
        let gain_db = freqs.iter().map(|f| (f / 1_000.0).log2() * 0.8).collect();
        MicResponse {
            freqs_hz: freqs,
            gain_db,
            source_path: Some("M30-7731.frd".into()),
            imported_at: "2026-08-30T08:02:11Z".into(),
        }
    }

    /// One sign rule for both paths: a set corrected post-hoc and the same
    /// set corrected at capture give equal statistics.
    #[test]
    fn post_hoc_and_capture_time_correction_agree() {
        let c = curve();
        let post_hoc = verify(three_runs(), Some(&c)).unwrap();
        assert!(post_hoc.runs.iter().all(|r| r.mic == RunMic::PostHoc));

        let mut captured = three_runs();
        for i in &mut captured {
            if let MeasurementData::GatedFrequencyResponse { points } = &mut i.report.data[1].data {
                for p in points {
                    p.magnitude_db = c.corrected_db(p.freq_hz, p.magnitude_db);
                }
            }
            i.report.processing_chain.mic_correction_applied = true;
        }
        let capture = verify(captured, None).unwrap();
        assert!(capture
            .runs
            .iter()
            .all(|r| matches!(r.mic, RunMic::AtCapture(_))));
        assert_eq!(post_hoc.gain_spread, capture.gain_spread);
        assert_eq!(post_hoc.tonal_stability, capture.tonal_stability);
        assert_eq!(post_hoc.flatness, capture.flatness);
        assert_eq!(post_hoc.mean_response, capture.mean_response);
        // The correction did something: the mean differs from uncorrected.
        let raw = verify(three_runs(), None).unwrap();
        assert_ne!(raw.mean_response, post_hoc.mean_response);
        // The harmonics are never corrected.
        assert_eq!(raw.harmonics, post_hoc.harmonics);
    }

    #[test]
    fn mixed_correction_needs_a_curve_and_a_curve_needs_a_raw_run() {
        let mut mixed = three_runs();
        mixed[0].report.processing_chain.mic_correction_applied = true;
        assert_eq!(
            verify(mixed.clone(), None).unwrap_err(),
            VerificationRefusal::MixedCorrectionNoCurve {
                at_capture: vec!["b.json".into()],
                raw: vec!["a.json".into(), "c.json".into()],
            }
        );
        mixed[0].report.calibration = None;
        let c = curve();
        let set = verify(mixed.clone(), Some(&c)).unwrap();
        assert_eq!(set.runs[1].mic, RunMic::AtCapture(None));
        assert_eq!(set.runs[0].mic, RunMic::PostHoc);

        for i in &mut mixed {
            i.report.processing_chain.mic_correction_applied = true;
        }
        assert!(matches!(
            verify(mixed, Some(&c)).unwrap_err(),
            VerificationRefusal::CurveSuppliedAllCorrected { .. }
        ));
    }

    #[test]
    fn capture_time_curve_identity_comes_from_the_snapshot() {
        let r = MicResponseRef {
            n_points: 512,
            source_path: Some("M30-7731.frd".into()),
            imported_at: "2026-08-30T08:02:11Z".into(),
        };
        let id = CurveIdentity::from(&r);
        assert_eq!(id.imported_at.as_deref(), Some("2026-08-30T08:02:11Z"));
        assert_eq!(CurveIdentity::supplied(&curve()).imported_at, None);
    }

    #[test]
    fn refusals_name_files_and_the_first_differing_field() {
        assert_eq!(
            verify(three_runs()[..1].to_vec(), None).unwrap_err(),
            VerificationRefusal::TooFewReports { given: 1 }
        );

        let mut not_ir = three_runs();
        not_ir[1].report.data.remove(1);
        assert_eq!(
            verify(not_ir, None).unwrap_err(),
            VerificationRefusal::NotPlotIr {
                file: "a.json".into(),
                missing: vec!["gated frequency response"],
            }
        );

        let mut rate = three_runs();
        if let MeasurementData::ImpulseResponse { sample_rate_hz, .. } =
            &mut rate[2].report.data[0].data
        {
            *sample_rate_hz = 96_000;
        }
        assert_eq!(
            verify(rate, None).unwrap_err(),
            VerificationRefusal::NotOneSet(Mismatch {
                field: SetField::SampleRate,
                first: ("b.json".into(), FieldValue::Hz(48_000.0)),
                other: ("c.json".into(), FieldValue::Hz(96_000.0)),
            })
        );

        let mut gate = three_runs();
        gate[1].report.data[1].gate.as_mut().unwrap().gate_length_s = 0.003;
        let VerificationRefusal::NotOneSet(m) = verify(gate, None).unwrap_err() else {
            panic!()
        };
        assert_eq!(m.field, SetField::GateLength);
        assert!(m.field.is_gate());

        let mut grid = three_runs();
        if let MeasurementData::GatedFrequencyResponse { points } = &mut grid[2].report.data[1].data
        {
            points[19].freq_hz += 1.0;
        }
        let VerificationRefusal::NotOneSet(m) = verify(grid, None).unwrap_err() else {
            panic!()
        };
        assert_eq!(m.field, SetField::FrequencyGrid);
        assert_eq!(m.other.1, FieldValue::GridDiffersAt { freq_hz: 2_000.0 });

        let mut twice = three_runs();
        twice[2].report.timestamp_utc = twice[0].report.timestamp_utc.clone();
        assert_eq!(
            verify(twice, None).unwrap_err(),
            VerificationRefusal::SameRunTwice {
                captured: "2026-09-24T10:14:47Z".into(),
                files: vec!["b.json".into(), "c.json".into()],
            }
        );
    }

    /// The run table's printed requirement is ours; the verdict comes from
    /// `check_tail_decay`. The two must be the same number.
    #[test]
    fn tail_decay_requirement_matches_the_check() {
        let p = SweepParams {
            f1_hz: 20.0,
            f2_hz: 20_000.0,
            duration_s: 1.0,
            sample_rate: 48_000,
        };
        let mut full = vec![0.0; p.n_samples() + 48_000];
        full[p.n_samples() - 1] = 1.0;
        for (i, v) in full.iter_mut().enumerate().skip(p.n_samples()).take(2_000) {
            *v = ((i as f64) * 0.37).sin();
        }
        let check = check_tail_decay(&full, &p, 0.5).unwrap();
        assert_eq!(check.required_db, TAIL_DECAY_REQUIRED_DB);
    }
}
