//! What a multi-run verification says (#398): header fields, run table,
//! exclusion and unchecked lines, verdict rows, harmonic rows, flatness
//! rows and the four charts — pre-formatted text read by the terminal
//! printer in `ac-cli`, `report_html` and `report_pdf` alike, so the three
//! cannot print different numbers.
//!
//! Numerals use ASCII hyphen-minus; the en dash appears only in ranges.
//! Every flatness or deviation figure carries its band, and the band
//! printed is the clipped band actually used.

use super::chart::{BandMark, Chart, Series, XScale};
use crate::measurement::report::TailDecayRecord;
use crate::measurement::report::PRE_IMPULSE_SNR_MIN_DB;
use crate::measurement::verification::{
    Band, CurveIdentity, Exclusion, FieldValue, Figure, HarmonicReading, HarmonicRow, NotComputed,
    Run, RunMic, RunStatus, SetField, Verdict, VerificationRefusal, VerificationSet,
    BAND_LOWER_EDGE_HZ, HARMONIC_TRACK_TOLERANCE_DB, MIN_DRIVE_SPAN_DB, MIN_USED_RUNS,
    STATS_GRID_BPO, TAIL_DECAY_REQUIRED_DB,
};

/// Title line of the terminal summary and of both documents.
pub const TITLE: &str = "ac report verify";

/// A labelled block: the first line sits beside the label, the rest are
/// continuation lines under the first.
#[derive(Debug, Clone, PartialEq)]
pub struct Field {
    pub label: &'static str,
    pub lines: Vec<String>,
}

impl Field {
    fn new(label: &'static str, lines: Vec<String>) -> Self {
        Self { label, lines }
    }
}

/// One run-table row. `attention` marks a state the operator must not
/// skim past (excluded, unchecked, not recorded).
#[derive(Debug, Clone, PartialEq)]
pub struct RunRow {
    pub run: String,
    pub level: String,
    pub snr: String,
    pub tail: String,
    pub status: String,
    pub tail_attention: bool,
    pub status_attention: bool,
}

/// A verdict's value cells.
#[derive(Debug, Clone, PartialEq)]
pub struct VerdictCells {
    pub measured: String,
    pub limit: String,
    pub result: String,
    /// `fail`.
    pub attention: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct VerdictRow {
    pub name: &'static str,
    pub band: String,
    /// The cells, or the `not computed: …` sentence that replaces them.
    pub body: Result<VerdictCells, String>,
}

/// A harmonic row's slope cells.
#[derive(Debug, Clone, PartialEq)]
pub struct HarmonicCells {
    pub slope: String,
    pub expected: String,
    pub reading: String,
    /// `floor-limited` or `rises faster`.
    pub attention: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct HarmonicLine {
    pub order: String,
    /// `≤ ` on an upper bound, three spaces otherwise, so values align.
    pub prefix: &'static str,
    pub level: String,
    pub body: Result<HarmonicCells, String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FlatnessRow {
    pub band: String,
    pub value: String,
}

/// A plain table for the document backends: header cells and rows.
#[derive(Debug, Clone, PartialEq)]
pub struct Table {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<String>>,
}

/// Everything a verification report prints, in order.
#[derive(Debug, Clone, PartialEq)]
pub struct VerificationLayout {
    pub title: &'static str,
    /// When the document was rendered — not a capture time.
    pub rendered_utc: String,
    /// `input` … `absolute`, verbatim at the top of every output.
    pub header: Vec<Field>,
    pub run_header: RunRow,
    /// The fixed requirements, on the line under the run-table header.
    pub run_thresholds: RunRow,
    pub runs: Vec<RunRow>,
    /// `excluded` and `unchecked` lines.
    pub warnings: Vec<Field>,
    pub verdicts: Vec<VerdictRow>,
    pub harmonics: Field,
    /// Column header of the level column: the highest used drive.
    pub harmonic_level_header: String,
    pub harmonic_rows: Vec<HarmonicLine>,
    pub harmonic_footer: Vec<String>,
    pub response: Field,
    pub flatness: Vec<FlatnessRow>,
    pub charts: Vec<Chart>,
}

/// A refusal block: the `error:` title and aligned label/value rows,
/// ending in `output not written`.
#[derive(Debug, Clone, PartialEq)]
pub struct RefusalBlock {
    pub title: String,
    pub rows: Vec<(String, String)>,
}

// ---------------------------------------------------------------------------
// Number formatting
// ---------------------------------------------------------------------------

/// `v` to two decimals with trailing zeros dropped: `1.5`, `16`, `12.5`.
fn trim(v: f64) -> String {
    let s = format!("{v:.2}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s == "-0" {
        "0".into()
    } else {
        s.to_string()
    }
}

/// `20 Hz`, `1.5 kHz`, `96 kHz`.
pub fn fmt_freq(f: f64) -> String {
    if f >= 1_000.0 {
        format!("{} kHz", trim(f / 1_000.0))
    } else {
        format!("{} Hz", trim(f))
    }
}

/// A band in one unit when both edges share it: `1.5–16 kHz`,
/// `200 Hz–5 kHz`.
pub fn fmt_band(b: Band) -> String {
    if b.lo_hz >= 1_000.0 {
        format!(
            "{}\u{2013}{} kHz",
            trim(b.lo_hz / 1_000.0),
            trim(b.hi_hz / 1_000.0)
        )
    } else if b.hi_hz < 1_000.0 {
        format!("{}\u{2013}{} Hz", trim(b.lo_hz), trim(b.hi_hz))
    } else {
        format!("{}\u{2013}{}", fmt_freq(b.lo_hz), fmt_freq(b.hi_hz))
    }
}

/// Fixed one-decimal frequency, for a grid's range: `20.0 Hz`, `48.0 kHz`.
fn fmt_freq_1(f: f64) -> String {
    if f >= 1_000.0 {
        format!("{:.1} kHz", f / 1_000.0)
    } else {
        format!("{f:.1} Hz")
    }
}

/// Fixed two-decimal frequency, for a single bin: `2.00 kHz`.
fn fmt_freq_2(f: f64) -> String {
    if f >= 1_000.0 {
        format!("{:.2} kHz", f / 1_000.0)
    } else {
        format!("{f:.2} Hz")
    }
}

/// A 1/3-octave band centre by its nominal IEC 61260-1 name
/// (`12.5 kHz`, `16 kHz`) rather than its exact base-ten centre
/// (`12.59 kHz`), which no reader looks for.
fn fmt_third_octave(f: f64) -> String {
    const R10: [f64; 10] = [1.0, 1.25, 1.6, 2.0, 2.5, 3.15, 4.0, 5.0, 6.3, 8.0];
    if !(f.is_finite() && f > 0.0) {
        return fmt_freq(f);
    }
    let decade = 10f64.powf(f.log10().floor());
    let m = f / decade;
    let nearest = R10
        .iter()
        .chain(std::iter::once(&10.0))
        .copied()
        .min_by(|a, b| (m / a).ln().abs().total_cmp(&(m / b).ln().abs()))
        .unwrap_or(1.0);
    fmt_freq(nearest * decade)
}

fn fmt_db(v: f64) -> String {
    format!("{v:.1} dB")
}

/// `run 2`, `runs 1–3`, `runs 1, 3`.
pub fn fmt_runs(numbers: &[usize]) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut i = 0;
    while i < numbers.len() {
        let mut j = i;
        while j + 1 < numbers.len() && numbers[j + 1] == numbers[j] + 1 {
            j += 1;
        }
        if j - i >= 2 {
            parts.push(format!("{}\u{2013}{}", numbers[i], numbers[j]));
        } else {
            for n in &numbers[i..=j] {
                parts.push(n.to_string());
            }
        }
        i = j + 1;
    }
    let noun = if numbers.len() == 1 { "run" } else { "runs" };
    format!("{noun} {}", parts.join(", "))
}

fn not_computed(n: &NotComputed) -> String {
    let body = match n {
        NotComputed::TooFewRuns { used } => format!(
            "{used} run{} used, {MIN_USED_RUNS} needed",
            if *used == 1 { "" } else { "s" }
        ),
        NotComputed::AboveSweepStop { f2_hz } => format!("above sweep stop {}", fmt_freq(*f2_hz)),
        NotComputed::BelowSweepStart { f1_hz } => {
            format!("below sweep start {}", fmt_freq(*f1_hz))
        }
        NotComputed::BelowGateLimit { f_low_hz } => {
            format!("below the gate's lowest frequency {}", fmt_freq(*f_low_hz))
        }
        NotComputed::NoGridPoints => "no 1/6-octave cell in the band".into(),
        NotComputed::DriveSpan { span_db } => format!(
            "drive span {span_db:.1} dB, {} dB needed",
            trim(MIN_DRIVE_SPAN_DB)
        ),
        NotComputed::NoLevel => format!("no level in {MIN_USED_RUNS} used runs"),
    };
    format!("not computed: {body}")
}

fn curve_id(id: &CurveIdentity) -> String {
    let path = id.source_path.as_deref().unwrap_or("path not recorded");
    format!("{path}, {} points", id.n_points)
}

// ---------------------------------------------------------------------------
// Header
// ---------------------------------------------------------------------------

fn mic_lines(set: &VerificationSet) -> Vec<String> {
    let post: Vec<usize> = set
        .runs
        .iter()
        .filter(|r| r.mic == RunMic::PostHoc)
        .map(|r| r.number)
        .collect();
    let cap: Vec<usize> = set
        .runs
        .iter()
        .filter(|r| matches!(r.mic, RunMic::AtCapture(_)))
        .map(|r| r.number)
        .collect();
    let mixed = !post.is_empty() && !cap.is_empty();
    let pad = if mixed { "    " } else { "  " };
    let mut lines = Vec::new();
    if post.is_empty() && cap.is_empty() {
        lines.push("not applied: response includes the microphone".into());
        return lines;
    }
    if mixed {
        lines.push(format!(
            "post-hoc to {}; at capture to {}",
            fmt_runs(&post),
            fmt_runs(&cap)
        ));
    } else if !post.is_empty() {
        lines.push(format!(
            "applied post-hoc to {}, frequency response only",
            fmt_runs(&post)
        ));
    } else {
        lines.push(format!(
            "applied at capture to {}, frequency response only",
            fmt_runs(&cap)
        ));
    }
    if let (false, Some(s)) = (post.is_empty(), &set.mic.supplied) {
        lines.push(format!("supplied{pad}{}", curve_id(s)));
    }
    if !cap.is_empty() {
        let unrecorded = set
            .runs
            .iter()
            .any(|r| matches!(r.mic, RunMic::AtCapture(None)));
        for id in &set.mic.at_capture {
            lines.push(format!("at capture  {}", curve_id(id)));
            if let Some(t) = &id.imported_at {
                lines.push(format!("            imported {t}"));
            }
        }
        if unrecorded {
            lines.push("at capture  curve not recorded in the report".into());
        }
    }
    lines
}

fn distance_line(runs: &[Run]) -> String {
    let first = runs.first().and_then(|r| r.distance_m);
    if runs.iter().all(|r| r.distance_m == first) {
        match first {
            Some(d) => format!("{d:.2} m, in-room"),
            None => "not recorded".into(),
        }
    } else {
        "differs across runs".into()
    }
}

fn header(set: &VerificationSet) -> Vec<Field> {
    let s = &set.sweep;
    let gate = match &set.gate {
        Some(g) => format!(
            "{:.2} ms {}; statistics on 1/{STATS_GRID_BPO}-octave means",
            g.gate_length_s * 1_000.0,
            g.window_kind
        ),
        None => format!("not recorded; statistics on 1/{STATS_GRID_BPO}-octave means"),
    };
    vec![
        Field::new(
            "input",
            set.runs
                .iter()
                .map(|r| format!("{}  {}  {}", r.number, r.timestamp_utc, r.file))
                .collect(),
        ),
        Field::new("order", vec!["stimulus level, ascending".into()]),
        Field::new(
            "sweep",
            vec![format!(
                "{}\u{2013}{}, {:.2} s, at {}",
                fmt_freq(s.f1_hz),
                fmt_freq(s.f2_hz),
                s.duration_s,
                fmt_freq(s.sample_rate_hz as f64)
            )],
        ),
        Field::new("gate", vec![gate]),
        Field::new("distance", vec![distance_line(&set.runs)]),
        Field::new(
            "bands",
            vec![format!(
                "lower edge {}, fixed; below it the response is the room",
                fmt_freq(BAND_LOWER_EDGE_HZ)
            )],
        ),
        Field::new("mic curve", mic_lines(set)),
        Field::new(
            "absolute",
            vec![
                "not compared: interface output volume not recorded".into(),
                "gain spread assumes it was unchanged across this set".into(),
            ],
        ),
    ]
}

// ---------------------------------------------------------------------------
// Run table and warnings
// ---------------------------------------------------------------------------

fn run_row(r: &Run) -> RunRow {
    let snr = match r.pre_impulse_snr_db {
        Some(v) if v.is_finite() => fmt_db(v),
        Some(_) => "silent floor".into(),
        None => "not measured".into(),
    };
    let (tail, tail_attention) = match &r.tail_decay {
        None => ("not recorded".to_string(), true),
        Some(TailDecayRecord::NotEvaluated { .. }) => ("not evaluated".to_string(), true),
        Some(TailDecayRecord::Checked(c)) => {
            let band = fmt_third_octave(c.worst_band_hz);
            if c.worst_decay_db.is_finite() {
                (format!("{} at {band}", fmt_db(c.worst_decay_db)), false)
            } else {
                ("silent tail".to_string(), false)
            }
        }
    };
    let (status, status_attention) = match &r.status {
        RunStatus::Used => ("used", false),
        RunStatus::Unchecked => ("unchecked", true),
        RunStatus::Excluded(_) => ("excluded", true),
    };
    RunRow {
        run: r.number.to_string(),
        level: format!("{:.1} dBFS", r.level_dbfs),
        snr,
        tail,
        status: status.into(),
        tail_attention,
        status_attention,
    }
}

const LEFT_OUT: &str = "left out of every figure below";

fn exclusion_lines(run: usize, e: &Exclusion) -> Vec<String> {
    match e {
        Exclusion::PreImpulse {
            snr_db: Some(v),
            required_db,
            ..
        } => vec![
            format!("run {run}: pre-impulse SNR {v:.1} dB, {required_db:.1} dB required;"),
            LEFT_OUT.into(),
        ],
        Exclusion::PreImpulse {
            snr_db: None,
            reason,
            ..
        } => vec![
            format!("run {run}: pre-impulse check failed ({reason});"),
            LEFT_OUT.into(),
        ],
        Exclusion::TailDecayFailed {
            decay_db,
            band_hz,
            required_db,
        } => vec![
            format!(
                "run {run}: tail decay {decay_db:.1} dB at {}, {required_db:.1} dB required",
                fmt_third_octave(*band_hz)
            ),
            format!("(ISO 18233 \u{a7}6.3.2); {LEFT_OUT}"),
        ],
        Exclusion::TailDecayNotEvaluated { reason } => vec![
            format!("run {run}: tail decay not evaluated ({reason});"),
            LEFT_OUT.into(),
        ],
    }
}

fn warnings(set: &VerificationSet) -> Vec<Field> {
    let mut out = Vec::new();
    for r in &set.runs {
        if let RunStatus::Excluded(reasons) = &r.status {
            for e in reasons {
                out.push(Field::new("excluded", exclusion_lines(r.number, e)));
            }
        }
    }
    let mut versions: Vec<u32> = set
        .runs
        .iter()
        .filter(|r| r.status == RunStatus::Unchecked)
        .map(|r| r.schema_version)
        .collect();
    versions.sort_unstable();
    versions.dedup();
    for v in versions {
        let runs: Vec<usize> = set
            .runs
            .iter()
            .filter(|r| r.status == RunStatus::Unchecked && r.schema_version == v)
            .map(|r| r.number)
            .collect();
        out.push(Field::new(
            "unchecked",
            vec![
                format!(
                    "{}: tail decay not recorded (schema v{v}); used in",
                    fmt_runs(&runs)
                ),
                "every figure below, ISO 18233 \u{a7}6.3.2 not verified".into(),
            ],
        ));
    }
    out
}

// ---------------------------------------------------------------------------
// Verdicts, harmonics, flatness
// ---------------------------------------------------------------------------

fn verdict_row(name: &'static str, v: &Verdict, pm: &str) -> VerdictRow {
    let body = match (&v.value, v.passed()) {
        (Figure::Value(x), Some(pass)) => Ok(VerdictCells {
            measured: format!("{pm}{x:.2} dB"),
            limit: format!("\u{2264} {pm}{:.2} dB", v.limit_db),
            result: if pass { "pass" } else { "fail" }.into(),
            attention: !pass,
        }),
        (Figure::NotComputed(n), _) => Err(not_computed(n)),
        (Figure::Value(_), None) => Err(not_computed(&NotComputed::NoGridPoints)),
    };
    VerdictRow {
        name,
        band: fmt_band(v.band),
        body,
    }
}

fn reading_text(r: HarmonicReading) -> &'static str {
    match r {
        HarmonicReading::TracksDrive => "tracks drive",
        HarmonicReading::FloorLimited => "floor-limited",
        HarmonicReading::RisesFaster => "rises faster",
    }
}

fn harmonic_line(h: &HarmonicRow) -> HarmonicLine {
    let reading = h.reading();
    let upper = reading == Some(HarmonicReading::FloorLimited);
    let body = match (&h.slope, reading) {
        (Figure::Value(s), Some(r)) => Ok(HarmonicCells {
            slope: format!("{s:+.1} dB"),
            expected: format!("{:+.0} dB", h.expected_db),
            reading: reading_text(r).into(),
            attention: r != HarmonicReading::TracksDrive,
        }),
        (Figure::NotComputed(n), _) => Err(not_computed(n)),
        (Figure::Value(_), None) => Err(not_computed(&NotComputed::NoLevel)),
    };
    HarmonicLine {
        order: format!("H{}", h.order),
        prefix: if upper { "\u{2264}  " } else { "   " },
        level: h.level_db.map_or("not measured".into(), fmt_db),
        body,
    }
}

// ---------------------------------------------------------------------------
// Charts
// ---------------------------------------------------------------------------

fn charts(set: &VerificationSet) -> Vec<Chart> {
    let used = set.used_runs();
    let deviation = Chart::new(
        "Level deviation from the series mean, each band re its own median",
        "frequency",
        "deviation, dB",
        XScale::LogFrequency,
        set.deviations
            .iter()
            .map(|d| Series::new(format!("run {}", d.run), d.points.clone()))
            .collect(),
        set.deviation_bands
            .iter()
            .map(|b| BandMark {
                lo: b.lo_hz,
                hi: b.hi_hz,
                label: fmt_band(*b),
            })
            .collect(),
        1.0,
    );
    let gain = Chart::new(
        format!("Gain against drive, {}", fmt_band(set.gain_spread.band)),
        "drive, dBFS",
        "gain, dB (median of band)",
        XScale::Linear,
        vec![Series::new(
            "gain",
            set.runs
                .iter()
                .filter_map(|r| r.gain_db.map(|g| (r.level_dbfs, g)))
                .collect(),
        )],
        vec![],
        1.0,
    );
    let harmonics = Chart::new(
        "Harmonic level against drive, re fundamental",
        "drive, dBFS",
        "level re fundamental, dB",
        XScale::Linear,
        set.harmonics
            .iter()
            .map(|h| {
                let reading = h.reading();
                let label = match reading {
                    Some(HarmonicReading::FloorLimited) => {
                        format!("H{} \u{2264} floor-limited", h.order)
                    }
                    Some(r) => format!("H{} {}", h.order, reading_text(r)),
                    None => format!("H{}", h.order),
                };
                Series {
                    label,
                    points: h.points.clone(),
                    upper_bound: reading == Some(HarmonicReading::FloorLimited),
                }
            })
            .collect(),
        vec![],
        6.0,
    );
    let mean = Chart::new(
        format!("Mean response of {}", fmt_runs(&used)),
        "frequency",
        "level, dB",
        XScale::LogFrequency,
        vec![Series::new(
            format!("mean of {}", fmt_runs(&used)),
            set.mean_response.clone(),
        )],
        vec![],
        6.0,
    );
    vec![deviation, gain, harmonics, mean]
}

// ---------------------------------------------------------------------------
// Entry points
// ---------------------------------------------------------------------------

/// The layout of an accepted set. `rendered_utc` is the render time the
/// title line carries.
pub fn layout(set: &VerificationSet, rendered_utc: &str) -> VerificationLayout {
    let used = set.used_runs();
    let top_drive = set
        .runs
        .iter()
        .filter(|r| r.status.is_used())
        .map(|r| r.level_dbfs)
        .next_back();
    let mut verdicts = vec![verdict_row("gain spread", &set.gain_spread, "")];
    verdicts.extend(
        set.tonal_stability
            .iter()
            .map(|v| verdict_row("tonal stability", v, "\u{b1}")),
    );
    let tol = format!("{HARMONIC_TRACK_TOLERANCE_DB:.1}");
    VerificationLayout {
        title: TITLE,
        rendered_utc: rendered_utc.to_string(),
        header: header(set),
        run_header: RunRow {
            run: "run".into(),
            level: "level".into(),
            snr: "pre-impulse SNR".into(),
            tail: "tail decay".into(),
            status: "status".into(),
            tail_attention: false,
            status_attention: false,
        },
        run_thresholds: RunRow {
            run: String::new(),
            level: String::new(),
            snr: format!("min {PRE_IMPULSE_SNR_MIN_DB:.1} dB"),
            tail: format!("min {TAIL_DECAY_REQUIRED_DB:.1} dB"),
            status: String::new(),
            tail_attention: false,
            status_attention: false,
        },
        runs: set.runs.iter().map(run_row).collect(),
        warnings: warnings(set),
        verdicts,
        harmonics: Field::new(
            "harmonics",
            vec![format!(
                "re fundamental, broadband over the sweep, {}",
                if used.is_empty() {
                    "no run used".to_string()
                } else {
                    fmt_runs(&used)
                }
            )],
        ),
        harmonic_level_header: match top_drive {
            Some(d) => format!("at {d:.1} dBFS"),
            None => "at top drive".into(),
        },
        harmonic_rows: set.harmonics.iter().map(harmonic_line).collect(),
        harmonic_footer: vec![
            format!("within \u{b1}{tol} dB of expected: tracks drive"),
            "below: floor-limited, level is an upper bound".into(),
            "above: rises faster, level is a value".into(),
        ],
        response: Field::new(
            "response",
            vec![format!(
                "mean of {}, each band re its own median",
                if used.is_empty() {
                    "no run".to_string()
                } else {
                    fmt_runs(&used)
                }
            )],
        ),
        flatness: set
            .flatness
            .iter()
            .map(|f| FlatnessRow {
                band: fmt_band(f.band),
                value: match &f.value {
                    Figure::Value(v) => format!("\u{b1}{v:.2} dB"),
                    Figure::NotComputed(n) => not_computed(n),
                },
            })
            .collect(),
        charts: charts(set),
    }
}

impl VerificationLayout {
    /// The run table with its requirement line as the first row.
    pub fn run_table(&self) -> Table {
        let cells = |r: &RunRow| {
            vec![
                r.run.clone(),
                r.level.clone(),
                r.snr.clone(),
                r.tail.clone(),
                r.status.clone(),
            ]
        };
        Table {
            columns: cells(&self.run_header),
            rows: std::iter::once(&self.run_thresholds)
                .chain(&self.runs)
                .map(cells)
                .collect(),
        }
    }

    pub fn verdict_table(&self) -> Table {
        Table {
            columns: ["verdict", "band", "measured", "limit", "result"]
                .map(String::from)
                .to_vec(),
            rows: self
                .verdicts
                .iter()
                .map(|v| match &v.body {
                    Ok(c) => vec![
                        v.name.into(),
                        v.band.clone(),
                        c.measured.clone(),
                        c.limit.clone(),
                        c.result.clone(),
                    ],
                    Err(n) => vec![
                        v.name.into(),
                        v.band.clone(),
                        n.clone(),
                        String::new(),
                        String::new(),
                    ],
                })
                .collect(),
        }
    }

    pub fn harmonic_table(&self) -> Table {
        Table {
            columns: vec![
                "order".into(),
                self.harmonic_level_header.clone(),
                "per 10 dB drive".into(),
                "expected".into(),
                "reading".into(),
            ],
            rows: self
                .harmonic_rows
                .iter()
                .map(|h| {
                    let level = if h.prefix.trim().is_empty() {
                        h.level.clone()
                    } else {
                        format!("{} {}", h.prefix.trim(), h.level)
                    };
                    match &h.body {
                        Ok(c) => vec![
                            h.order.clone(),
                            level,
                            c.slope.clone(),
                            c.expected.clone(),
                            c.reading.clone(),
                        ],
                        Err(n) => vec![
                            h.order.clone(),
                            level,
                            n.clone(),
                            String::new(),
                            String::new(),
                        ],
                    }
                })
                .collect(),
        }
    }

    pub fn flatness_table(&self) -> Table {
        Table {
            columns: vec!["band".into(), "flatness".into()],
            rows: self
                .flatness
                .iter()
                .map(|f| vec![f.band.clone(), f.value.clone()])
                .collect(),
        }
    }

    /// Every verdict, reading and flatness string the layout carries —
    /// what a document must show for the numbers to agree with the
    /// terminal.
    pub fn verdict_strings(&self) -> Vec<String> {
        let mut out = Vec::new();
        for t in [
            self.verdict_table(),
            self.harmonic_table(),
            self.flatness_table(),
        ] {
            out.extend(t.rows.into_iter().flatten().filter(|c| !c.is_empty()));
        }
        out
    }
}

fn field_name(f: SetField) -> &'static str {
    match f {
        SetField::SampleRate => "sample rate",
        SetField::SweepStart => "sweep start",
        SetField::SweepStop => "sweep stop",
        SetField::SweepDuration => "sweep duration",
        SetField::GateLength => "gate length",
        SetField::GateWindow => "gate window",
        SetField::FrequencyGrid => "frequency grid",
    }
}

fn field_value(field: SetField, v: &FieldValue) -> String {
    match v {
        FieldValue::Hz(f) => fmt_freq(*f),
        FieldValue::Seconds(s) if field == SetField::GateLength => {
            format!("{:.2} ms", s * 1_000.0)
        }
        FieldValue::Seconds(s) => format!("{s:.2} s"),
        FieldValue::Text(t) => t.clone(),
        FieldValue::NotRecorded => "not recorded".into(),
        FieldValue::Grid {
            points,
            first_hz,
            last_hz,
        } => format!(
            "{points} points, {}\u{2013}{}",
            fmt_freq_1(*first_hz),
            fmt_freq_1(*last_hz)
        ),
        FieldValue::GridDiffersAt { freq_hz } => {
            format!("values differ at {}", fmt_freq_2(*freq_hz))
        }
    }
}

/// The refusal block for a refused set.
pub fn refusal(r: &VerificationRefusal) -> RefusalBlock {
    let row = |l: &str, v: String| (l.to_string(), v);
    let (title, mut rows) = match r {
        VerificationRefusal::TooFewReports { given } => (
            format!("report verify needs at least {MIN_USED_RUNS} reports"),
            vec![row("given", given.to_string())],
        ),
        VerificationRefusal::NotPlotIr { file, missing } => (
            "not a plot ir report".to_string(),
            vec![
                row("file", file.clone()),
                row("missing", missing.join(", ")),
            ],
        ),
        VerificationRefusal::NotOneSet(m) => (
            if m.field.is_gate() {
                "runs are not one gate"
            } else {
                "runs are not one sweep"
            }
            .to_string(),
            vec![
                row("field", field_name(m.field).into()),
                row(&m.first.0, field_value(m.field, &m.first.1)),
                row(&m.other.0, field_value(m.field, &m.other.1)),
            ],
        ),
        VerificationRefusal::SameRunTwice { captured, files } => {
            let mut rows = vec![row("captured", captured.clone())];
            rows.extend(files.iter().map(|f| row("file", f.clone())));
            ("same run given twice".to_string(), rows)
        }
        VerificationRefusal::MixedCorrectionNoCurve { at_capture, raw } => (
            "mic correction differs across runs, no mic curve supplied".to_string(),
            vec![
                row("at capture", at_capture.join(", ")),
                row("raw", raw.join(", ")),
                row("supply", "mic-curve <path>".into()),
            ],
        ),
        VerificationRefusal::CurveSuppliedAllCorrected { supplied } => (
            "mic curve supplied, every run already corrected at capture".to_string(),
            vec![row(
                "supplied",
                supplied
                    .source_path
                    .clone()
                    .unwrap_or_else(|| "(no path)".into()),
            )],
        ),
    };
    rows.push(row("output", "not written".into()));
    RefusalBlock { title, rows }
}

#[cfg(test)]
pub(crate) mod testkit {
    use super::*;
    use crate::measurement::verification::testkit::{bins, run, tail_failed};
    use crate::measurement::verification::{verify, RunInput};

    /// Frame A's shape: three runs at -40/-35/-30 dBFS, run 2 failing tail
    /// decay, file names that need escaping in HTML.
    pub(crate) fn sample_layout() -> VerificationLayout {
        let freqs = bins(100.0);
        let mut inputs: Vec<RunInput> = [(-40.0, 0.01), (-35.0, 0.0178), (-30.0, 0.0316)]
            .iter()
            .enumerate()
            .map(|(i, &(level, h2))| RunInput {
                file: format!("run<{i}>.json"),
                report: run(
                    &format!("2026-09-24T10:1{i}:04Z"),
                    level,
                    &freqs,
                    move |f| -20.0 + 0.05 * i as f64 * (f / 3_000.0).sin(),
                    &[(2, h2), (4, 1e-4)],
                ),
            })
            .collect();
        inputs[1].report.tail_decay = Some(tail_failed());
        layout(&verify(inputs, None).unwrap(), "2026-09-25T09:14:31Z")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::measurement::verification::testkit::{bins, run, tail_failed};
    use crate::measurement::verification::{verify, RunInput};

    fn set(tail_fail_middle: bool, v12: bool) -> VerificationSet {
        let freqs = bins(100.0);
        let mut inputs: Vec<RunInput> = [(-40.0, 0.01), (-35.0, 0.0178), (-30.0, 0.0316)]
            .iter()
            .enumerate()
            .map(|(i, &(level, h2))| RunInput {
                file: format!("s30-a-m{}.json", -level as i32),
                report: run(
                    &format!("2026-09-24T10:1{i}:04Z"),
                    level,
                    &freqs,
                    move |f| -20.0 + 0.05 * i as f64 * (f / 3_000.0).sin(),
                    &[(2, h2), (4, 1e-4)],
                ),
            })
            .collect();
        if tail_fail_middle {
            inputs[1].report.tail_decay = Some(tail_failed());
        }
        if v12 {
            for i in &mut inputs {
                i.report.tail_decay = None;
                i.report.schema_version = 12;
            }
        }
        verify(inputs, None).unwrap()
    }

    fn line(f: &Field) -> String {
        f.lines.join(" / ")
    }

    #[test]
    fn frame_a_header_and_exclusion_read_as_specified() {
        let l = layout(&set(true, false), "2026-09-25T09:14:31Z");
        let get = |label: &str| l.header.iter().find(|f| f.label == label).unwrap();
        assert_eq!(
            get("input").lines[0],
            "1  2026-09-24T10:10:04Z  s30-a-m40.json"
        );
        assert_eq!(line(get("order")), "stimulus level, ascending");
        assert_eq!(line(get("sweep")), "20 Hz\u{2013}20 kHz, 4.00 s, at 48 kHz");
        assert_eq!(
            line(get("gate")),
            "5.00 ms tukey0.25; statistics on 1/6-octave means"
        );
        assert_eq!(line(get("distance")), "not recorded");
        assert_eq!(
            line(get("bands")),
            "lower edge 1.5 kHz, fixed; below it the response is the room"
        );
        assert_eq!(
            line(get("mic curve")),
            "not applied: response includes the microphone"
        );
        assert_eq!(
            get("absolute").lines,
            [
                "not compared: interface output volume not recorded",
                "gain spread assumes it was unchanged across this set"
            ]
        );
        assert_eq!(l.runs[1].status, "excluded");
        assert_eq!(l.runs[1].tail, "11.6 dB at 16 kHz");
        assert_eq!(l.runs[0].tail, "41.2 dB at 12.5 kHz");
        assert_eq!(l.run_thresholds.snr, "min 18.0 dB");
        assert_eq!(l.run_thresholds.tail, "min 30.0 dB");
        assert_eq!(
            l.warnings[0].lines,
            [
                "run 2: tail decay 11.6 dB at 16 kHz, 30.0 dB required",
                "(ISO 18233 \u{a7}6.3.2); left out of every figure below"
            ]
        );
        assert_eq!(
            l.harmonics.lines[0],
            "re fundamental, broadband over the sweep, runs 1, 3"
        );
        assert_eq!(l.harmonic_level_header, "at -30.0 dBFS");
        assert_eq!(l.verdicts[0].band, "1.5\u{2013}16 kHz");
        assert_eq!(l.verdicts[1].band, "1.5\u{2013}5 kHz");
        assert_eq!(l.charts.len(), 4);
    }

    #[test]
    fn frame_e_archive_runs_read_unchecked_with_one_warning() {
        let l = layout(&set(false, true), "t");
        assert!(l
            .runs
            .iter()
            .all(|r| r.tail == "not recorded" && r.status == "unchecked"));
        assert_eq!(
            l.warnings,
            vec![Field::new(
                "unchecked",
                vec![
                    "runs 1\u{2013}3: tail decay not recorded (schema v12); used in".into(),
                    "every figure below, ISO 18233 \u{a7}6.3.2 not verified".into(),
                ]
            )]
        );
    }

    #[test]
    fn a_floor_limited_order_prints_an_upper_bound_and_dashes_its_series() {
        let l = layout(&set(false, false), "t");
        let h4 = l.harmonic_rows.iter().find(|h| h.order == "H4").unwrap();
        assert_eq!(h4.prefix, "\u{2264}  ");
        assert_eq!(h4.body.as_ref().unwrap().reading, "floor-limited");
        let h2 = l.harmonic_rows.iter().find(|h| h.order == "H2").unwrap();
        assert_eq!(h2.prefix, "   ");
        assert_eq!(h2.body.as_ref().unwrap().reading, "tracks drive");
        let chart = &l.charts[2];
        let s4 = chart
            .series
            .iter()
            .find(|s| s.label.starts_with("H4"))
            .unwrap();
        assert!(s4.upper_bound);
        assert_eq!(s4.label, "H4 \u{2264} floor-limited");
        assert!(
            !chart
                .series
                .iter()
                .find(|s| s.label.starts_with("H2"))
                .unwrap()
                .upper_bound
        );
    }

    #[test]
    fn number_and_range_formatting() {
        assert_eq!(fmt_runs(&[1, 2, 3]), "runs 1\u{2013}3");
        assert_eq!(fmt_runs(&[1, 3]), "runs 1, 3");
        assert_eq!(fmt_runs(&[2]), "run 2");
        assert_eq!(fmt_runs(&[1, 2, 3, 5]), "runs 1\u{2013}3, 5");
        assert_eq!(fmt_band(Band::new(1_500.0, 16_000.0)), "1.5\u{2013}16 kHz");
        assert_eq!(fmt_band(Band::new(200.0, 5_000.0)), "200 Hz\u{2013}5 kHz");
        assert_eq!(fmt_freq(44_100.0), "44.1 kHz");
        assert_eq!(fmt_third_octave(12_589.25), "12.5 kHz");
        assert_eq!(fmt_third_octave(15_848.9), "16 kHz");
        assert_eq!(fmt_third_octave(9_999.0), "10 kHz");
        assert_eq!(
            not_computed(&NotComputed::TooFewRuns { used: 1 }),
            "not computed: 1 run used, 2 needed"
        );
        assert_eq!(
            not_computed(&NotComputed::DriveSpan { span_db: 5.0 }),
            "not computed: drive span 5.0 dB, 10 dB needed"
        );
    }

    #[test]
    fn refusals_name_the_field_and_both_files() {
        let b = refusal(&VerificationRefusal::NotOneSet(
            crate::measurement::verification::Mismatch {
                field: SetField::SampleRate,
                first: ("s30-a-m40.json".into(), FieldValue::Hz(96_000.0)),
                other: ("s30-a-m30.json".into(), FieldValue::Hz(48_000.0)),
            },
        ));
        assert_eq!(b.title, "runs are not one sweep");
        assert_eq!(
            b.rows,
            [
                ("field".to_string(), "sample rate".to_string()),
                ("s30-a-m40.json".to_string(), "96 kHz".to_string()),
                ("s30-a-m30.json".to_string(), "48 kHz".to_string()),
                ("output".to_string(), "not written".to_string()),
            ]
        );
        let g = refusal(&VerificationRefusal::NotOneSet(
            crate::measurement::verification::Mismatch {
                field: SetField::FrequencyGrid,
                first: (
                    "a".into(),
                    FieldValue::Grid {
                        points: 481,
                        first_hz: 20.0,
                        last_hz: 48_000.0,
                    },
                ),
                other: ("b".into(), FieldValue::GridDiffersAt { freq_hz: 2_000.0 }),
            },
        ));
        assert_eq!(g.title, "runs are not one gate");
        assert_eq!(g.rows[1].1, "481 points, 20.0 Hz\u{2013}48.0 kHz");
        assert_eq!(g.rows[2].1, "values differ at 2.00 kHz");
    }
}
