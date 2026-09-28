//! End-to-end `ac report verify` (#398): the real `ac` binary over fixture
//! reports written to a scratch directory. No daemon is involved — the
//! command reads archived JSON only — so none is spawned.
//!
//! What is asserted is what an operator reads: the terminal summary, the
//! refusal block, and whether a document was written.

mod support;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use ac_core::measurement::report::{
    GateParams, GatedFrequencyResponsePoint, IntegrationParams, MeasurementData, MeasurementMethod,
    MeasurementPayload, MeasurementReport, ProcessingChain, StimulusParams, TailDecayRecord,
    SCHEMA_VERSION,
};
use ac_core::measurement::sweep::{HarmonicIr, TailDecayCheck};

const SR: u32 = 48_000;

fn tail(passed: bool) -> TailDecayRecord {
    TailDecayRecord::Checked(TailDecayCheck {
        bpo: 3,
        worst_band_hz: if passed { 12_589.25 } else { 15_848.9 },
        worst_decay_db: if passed { 41.2 } else { 11.6 },
        required_db: 30.0,
        passed,
        bands_settled: 30,
        bands_total: 30,
    })
}

/// A `plot ir` report at `level_dbfs` with an H2 that tracks drive and a
/// constant H4.
fn report(timestamp: &str, level_dbfs: f64, tail_passed: bool) -> MeasurementReport {
    let mut linear_ir = vec![0.0; 1024];
    linear_ir[512] = 1.0;
    let harmonic = |order: u32, amp: f64| {
        let mut samples = vec![0.0; 256];
        samples[128] = amp;
        HarmonicIr { order, samples }
    };
    let h2 = 10f64.powf((level_dbfs - 10.0) / 20.0);
    let points = (1..=240)
        .map(|k| {
            let f = k as f64 * 100.0;
            GatedFrequencyResponsePoint {
                freq_hz: f,
                magnitude_db: -20.0 - 2.0 * (f / 1_000.0).log2(),
                phase_deg: 0.0,
            }
        })
        .collect();
    MeasurementReport {
        schema_version: SCHEMA_VERSION,
        ac_version: "0.2.0".into(),
        timestamp_utc: timestamp.into(),
        backend: Some("fake".into()),
        method: MeasurementMethod::SweptSine {
            f1_hz: 20.0,
            f2_hz: 20_000.0,
            duration_s: 4.0,
        },
        stimulus: StimulusParams {
            sample_rate_hz: SR,
            f_start_hz: 20.0,
            f_stop_hz: 20_000.0,
            level_dbfs,
            n_points: 0,
        },
        integration: IntegrationParams {
            duration_s: 4.0,
            window: "farina-inverse".into(),
            n_averages: None,
        },
        calibration: None,
        position: None,
        interface_latency: None,
        reference_latency: None,
        reference_stored_latency: None,
        inter_pair_offset: None,
        tail_decay: Some(tail(tail_passed)),
        data: vec![
            MeasurementPayload {
                data: MeasurementData::ImpulseResponse {
                    sample_rate_hz: SR,
                    f1_hz: 20.0,
                    f2_hz: 20_000.0,
                    duration_s: 4.0,
                    linear_ir,
                    harmonics: vec![harmonic(2, h2), harmonic(4, 1e-4)],
                    noise_tail_start_s: None,
                },
                standard: vec![],
                gate: None,
            },
            MeasurementPayload {
                data: MeasurementData::GatedFrequencyResponse { points },
                standard: vec![],
                gate: Some(GateParams {
                    gate_start_s: 0.0,
                    gate_length_s: 0.005,
                    window_kind: "tukey0.25".into(),
                    f_low_hz: 200.0,
                }),
            },
        ],
        notes: None,
        processing_chain: ProcessingChain::default(),
    }
}

struct Scratch {
    home: PathBuf,
}

impl Scratch {
    fn new() -> Self {
        Self {
            home: support::alloc_home("ac-report-verify"),
        }
    }

    fn write(&self, name: &str, r: &MeasurementReport) -> String {
        let path = self.home.join(name);
        r.write_to(&path).expect("write fixture");
        path.to_str().unwrap().to_string()
    }

    fn ac(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_ac"))
            .env("HOME", &self.home)
            .env("NO_COLOR", "1")
            .current_dir(&self.home)
            .args(args)
            .output()
            .expect("run ac")
    }

    fn documents(&self) -> Vec<PathBuf> {
        let mut v: Vec<PathBuf> = fs::read_dir(&self.home)
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("verify-"))
            })
            .collect();
        v.sort();
        v
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.home);
    }
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

#[test]
fn a_set_prints_its_verdicts_and_writes_one_document_beside_run_one() {
    let s = Scratch::new();
    // Argument order is not drive order.
    let m30 = s.write(
        "s30-a-m30.json",
        &report("2026-09-24T10:17:22Z", -30.0, true),
    );
    let m40 = s.write(
        "s30-a-m40.json",
        &report("2026-09-24T10:12:04Z", -40.0, true),
    );
    let m35 = s.write(
        "s30-a-m35.json",
        &report("2026-09-24T10:14:47Z", -35.0, false),
    );

    let out = s.ac(&["report", "verify", &m30, &m40, &m35]);
    let stdout = text(&out.stdout);
    assert!(out.status.success(), "{stdout}\n{}", text(&out.stderr));

    let run1 = format!("  input         1  2026-09-24T10:12:04Z  {m40}");
    for line in [
        run1.as_str(),
        "  order         stimulus level, ascending",
        "  sweep         20 Hz\u{2013}20 kHz, 4.00 s, at 48 kHz",
        "  bands         lower edge 1.5 kHz, fixed; below it the response is the room",
        "  mic curve     not applied: response includes the microphone",
        "  absolute      not compared: interface output volume not recorded",
        "                gain spread assumes it was unchanged across this set",
        "  run  level        pre-impulse SNR   tail decay               status",
        "                    min 18.0 dB       min 30.0 dB",
        "    1  -40.0 dBFS   silent floor      41.2 dB at 12.5 kHz      used",
        "    2  -35.0 dBFS   silent floor      11.6 dB at 16 kHz        excluded",
        "  excluded      run 2: tail decay 11.6 dB at 16 kHz, 30.0 dB required",
        "                (ISO 18233 \u{a7}6.3.2); left out of every figure below",
        "  gain spread       1.5\u{2013}16 kHz     0.00 dB     \u{2264} 0.50 dB    pass",
        "  harmonics     re fundamental, broadband over the sweep, runs 1, 3",
        "  order   at -30.0 dBFS   per 10 dB drive   expected   reading",
        "  H2         -40.0 dB        +10.0 dB        +10 dB    tracks drive",
        "                within \u{b1}3.0 dB of expected: tracks drive",
    ] {
        assert!(
            stdout.lines().any(|l| l == line),
            "missing line {line:?} in\n{stdout}"
        );
    }
    assert!(
        stdout
            .lines()
            .any(|l| l.starts_with("  H4      \u{2264} ") && l.ends_with("floor-limited")),
        "{stdout}"
    );

    let docs = s.documents();
    assert_eq!(docs, [s.home.join("verify-20260924T101204Z.html")]);
    let wrote = format!("  wrote         {}", docs[0].display());
    assert!(stdout.lines().any(|l| l == wrote), "{stdout}");
    let html = fs::read_to_string(&docs[0]).unwrap();
    assert!(html.contains("tracks drive") && html.contains("<svg"));

    let pdf = s.ac(&["report", "verify", &m30, &m40, &m35, "pdf"]);
    assert!(pdf.status.success(), "{}", text(&pdf.stderr));
    assert!(Path::new(&s.home.join("verify-20260924T101204Z.pdf")).exists());
}

#[test]
fn a_refused_set_prints_its_block_and_writes_nothing() {
    let s = Scratch::new();
    let a = s.write("a.json", &report("2026-09-24T10:12:04Z", -40.0, true));

    let one = s.ac(&["report", "verify", &a]);
    assert_eq!(one.status.code(), Some(1));
    assert_eq!(
        text(&one.stderr).lines().collect::<Vec<_>>(),
        [
            "  error: report verify needs at least 2 reports",
            "         given      1",
            "         output     not written",
        ]
    );

    let twice = s.ac(&["report", "verify", &a, &a]);
    assert_eq!(twice.status.code(), Some(1));
    let err = text(&twice.stderr);
    assert!(err.contains("  error: same run given twice"), "{err}");
    assert!(
        err.contains("         captured   2026-09-24T10:12:04Z"),
        "{err}"
    );

    let mut fast = report("2026-09-24T10:14:00Z", -30.0, true);
    if let MeasurementData::ImpulseResponse { sample_rate_hz, .. } = &mut fast.data[0].data {
        *sample_rate_hz = 96_000;
    }
    let b = s.write("b.json", &fast);
    let mixed = s.ac(&["report", "verify", &a, &b]);
    let err = text(&mixed.stderr);
    assert!(err.contains("  error: runs are not one sweep"), "{err}");
    assert!(err.contains("         field      sample rate"), "{err}");
    assert!(err.contains(&format!("         {a}   48 kHz")), "{err}");
    assert!(err.contains(&format!("         {b}   96 kHz")), "{err}");

    assert!(s.documents().is_empty(), "{:?}", s.documents());
}

#[test]
fn a_supplied_mic_curve_is_stamped_post_hoc() {
    let s = Scratch::new();
    let a = s.write("a.json", &report("2026-09-24T10:12:04Z", -40.0, true));
    let b = s.write("b.json", &report("2026-09-24T10:17:22Z", -30.0, true));
    let curve = s.home.join("M30-7731.frd");
    let body: String = (0..32)
        .map(|i| format!("{} {}\n", 20.0 * 1.25f64.powi(i), 0.1 * i as f64))
        .collect();
    fs::write(&curve, body).unwrap();
    let curve = curve.to_str().unwrap();

    let out = s.ac(&["report", "verify", &a, &b, "mic-curve", curve]);
    let stdout = text(&out.stdout);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert!(
        stdout
            .lines()
            .any(|l| l == "  mic curve     applied post-hoc to runs 1, 2, frequency response only"),
        "{stdout}"
    );
    let supplied = format!("                supplied  {curve}, 32 points");
    assert!(stdout.lines().any(|l| l == supplied), "{stdout}");
}
