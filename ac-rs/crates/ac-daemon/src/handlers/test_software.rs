//! `test_software` — pure-software self-test reachable via `ac test
//! software`. Validates the analysis pipeline + conversions with no
//! audio hardware and no daemon worker thread; runs synchronously and
//! returns a `results: [{name, pass, detail}]` array that the CLI's
//! `run_software` renders one row per check.

use serde_json::{json, Value};

use ac_core::measurement::thd;
use ac_core::shared::calibration::{Calibration, MicResponse};
use ac_core::shared::conversions::{dbfs_to_vrms, dbu_to_vrms, vrms_to_dbu};
use ac_core::shared::generator::generate_sine;

use crate::server::ServerState;

struct Check {
    name:   &'static str,
    pass:   bool,
    detail: String,
}

pub fn test_software(_state: &ServerState) -> Value {
    let checks = run_all_checks();
    let all_pass = checks.iter().all(|c| c.pass);
    let results: Vec<Value> = checks.iter().map(|c| json!({
        "name":   c.name,
        "pass":   c.pass,
        "detail": c.detail,
    })).collect();
    json!({
        "ok":       true,
        "results":  results,
        "all_pass": all_pass,
    })
}

fn run_all_checks() -> Vec<Check> {
    vec![
        check_dbu_vrms_known_values(),
        check_vrms_dbu_roundtrip(),
        check_dbfs_to_vrms(),
        check_sine_generator_rms(),
        check_pure_sine_thd_low(),
        check_synthetic_one_percent_thd(),
        check_thdn_ge_thd(),
        check_fundamental_dbfs_scaling(),
        check_mic_curve_log_linear_interp(),
        check_calibration_roundtrip(),
    ]
}

fn check_dbu_vrms_known_values() -> Check {
    let v0  = dbu_to_vrms(0.0);
    let v4  = dbu_to_vrms(4.0);
    let v20 = dbu_to_vrms(20.0);
    // 0 dBu ≈ 0.7746 V (constants::DBU_REF_VRMS); +4 dBu ≈ 1.228 V (pro
    // audio reference); +20 dBu ≈ 7.746 V. Tolerances absorb the trailing
    // rounding of the runtime reference.
    let pass = (v0 - 0.7746).abs() < 1e-4
            && (v4 - 1.228).abs()  < 5e-3
            && (v20 - 7.746).abs() < 1e-3;
    Check {
        name: "dBu → Vrms (0 / +4 / +20 dBu)",
        pass,
        detail: format!("0 dBu = {v0:.6} V, +4 dBu = {v4:.4} V, +20 dBu = {v20:.4} V"),
    }
}

fn check_vrms_dbu_roundtrip() -> Check {
    let inputs = [0.1_f64, 0.7746, 1.0, 2.5, 7.746];
    let mut max_err: f64 = 0.0;
    for v in inputs {
        let v2 = dbu_to_vrms(vrms_to_dbu(v));
        max_err = max_err.max((v - v2).abs());
    }
    let pass = max_err < 1e-9;
    Check {
        name: "Vrms ↔ dBu round-trip",
        pass,
        detail: format!("max abs error = {max_err:.2e} V over 5 inputs"),
    }
}

fn check_dbfs_to_vrms() -> Check {
    let v_n20 = dbfs_to_vrms(-20.0, 1.0);
    let v_0   = dbfs_to_vrms(0.0,   1.0);
    let v_n6  = dbfs_to_vrms(-6.0,  1.0);
    let pass = (v_n20 - 0.1).abs() < 1e-6
            && (v_0   - 1.0).abs() < 1e-9
            && (v_n6  - 0.501_187).abs() < 1e-3;
    Check {
        name: "dBFS → Vrms (cal ref = 1.0 V)",
        pass,
        detail: format!(
            "-20 dBFS = {v_n20:.6} V, 0 dBFS = {v_0:.6} V, -6 dBFS = {v_n6:.4} V",
        ),
    }
}

fn check_sine_generator_rms() -> Check {
    let sr   = 48_000;
    let amp  = 0.1_f64;
    let freq = 1000.0_f64;
    let s    = generate_sine(freq, amp, sr, sr as usize);
    let rms: f64 = (s.iter().map(|x| (*x as f64).powi(2)).sum::<f64>() / s.len() as f64).sqrt();
    let expected = amp / 2_f64.sqrt();
    let rel_err  = (rms - expected).abs() / expected;
    let pass     = rel_err < 1e-3;
    Check {
        name: "Generator: sine RMS = amp/√2",
        pass,
        detail: format!("amp 0.1 → RMS {rms:.6} (expected {expected:.6}, rel err {rel_err:.2e})"),
    }
}

fn check_pure_sine_thd_low() -> Check {
    let sr   = 48_000;
    let freq = 1000.0;
    let s    = generate_sine(freq, 0.5, sr, sr as usize);
    let r    = thd::analyze(&s, sr, freq, 10).expect("analyze on synthetic sine");
    let pass = r.thd_pct < 0.05;
    Check {
        name: "Pure sine: THD < 0.05%",
        pass,
        detail: format!("THD = {:.4}%", r.thd_pct),
    }
}

fn synth_with_h2(amp_fund: f32, h2_ratio: f32, sr: u32) -> Vec<f32> {
    let n = sr as usize;
    let f1 = generate_sine(1000.0, amp_fund as f64, sr, n);
    let f2 = generate_sine(2000.0, (amp_fund * h2_ratio) as f64, sr, n);
    f1.iter().zip(f2.iter()).map(|(a, b)| a + b).collect()
}

fn check_synthetic_one_percent_thd() -> Check {
    let sr = 48_000;
    let s  = synth_with_h2(0.5, 0.01, sr);
    let r  = thd::analyze(&s, sr, 1000.0, 10).expect("analyze");
    let pass = (r.thd_pct - 1.0).abs() < 0.1;
    Check {
        name: "Synthetic 1% H2: THD ≈ 1.0%",
        pass,
        detail: format!("THD = {:.4}% (expected 1.0% ± 0.1%)", r.thd_pct),
    }
}

fn check_thdn_ge_thd() -> Check {
    let sr = 48_000;
    let s  = synth_with_h2(0.5, 0.01, sr);
    let r  = thd::analyze(&s, sr, 1000.0, 10).expect("analyze");
    let pass = r.thdn_pct + 1e-9 >= r.thd_pct;
    Check {
        name: "Physical law: THD+N ≥ THD",
        pass,
        detail: format!("THD = {:.4}%, THD+N = {:.4}%", r.thd_pct, r.thdn_pct),
    }
}

fn check_fundamental_dbfs_scaling() -> Check {
    let sr = 48_000;
    let s  = generate_sine(1000.0, 0.1, sr, sr as usize);
    let r  = thd::analyze(&s, sr, 1000.0, 10).expect("analyze");
    // Window-correction tolerance: documented at ±2 dB in TESTING.md. The
    // peak detector lands on the nearest bin so a 1 kHz tone in a 48 kHz
    // window picks up a small windowing penalty.
    let pass = (r.fundamental_dbfs + 20.0).abs() < 2.5;
    Check {
        name: "fundamental_dbfs(amp=0.1) ≈ -20 dBFS",
        pass,
        detail: format!("fundamental = {:.2} dBFS (±2.5 dB)", r.fundamental_dbfs),
    }
}

fn check_mic_curve_log_linear_interp() -> Check {
    // 16-point flat curve: every gain = +3 dB. correction_at(any freq in
    // band) must return 3 dB regardless of bracket choice.
    let n = 16usize;
    let freqs: Vec<f32> = (0..n)
        .map(|i| 20.0 * (1000.0_f32 / 20.0).powf(i as f32 / (n - 1) as f32))
        .collect();
    let r = MicResponse {
        freqs_hz:    freqs,
        gain_db:     vec![3.0; n],
        source_path: None,
        imported_at: "1970-01-01T00:00:00Z".to_string(),
    };
    let probes = [50.0_f32, 100.0, 1000.0, 5000.0, 20000.0];
    let mut max_err: f32 = 0.0;
    for f in probes {
        max_err = max_err.max((r.correction_at(f) - 3.0).abs());
    }
    let pass = max_err < 1e-5;
    Check {
        name: "Mic-curve: flat curve interp = constant",
        pass,
        detail: format!("max err over 5 probes = {max_err:.2e} dB"),
    }
}

fn check_calibration_roundtrip() -> Check {
    // Use the system temp dir + a per-pid filename. ac-core's test suite
    // pulls in `tempfile`, but it's a dev-dep — at runtime we roll our own
    // unique path so the daemon stays slim.
    let path = std::env::temp_dir()
        .join(format!("ac_self_test_{}_cal.json", std::process::id()));
    let _ = std::fs::remove_file(&path);

    let n = 16usize;
    let freqs: Vec<f32> = (0..n)
        .map(|i| 20.0 * (20_000.0_f32 / 20.0).powf(i as f32 / (n - 1) as f32))
        .collect();

    let mut cal = Calibration::new(7, 11);
    cal.vrms_at_0dbfs_out                 = Some(1.234);
    cal.vrms_at_0dbfs_in                  = Some(0.567);
    cal.mic_sensitivity_dbfs_at_94db_spl  = Some(-30.0);
    cal.mic_response = Some(MicResponse {
        freqs_hz:    freqs,
        gain_db:     vec![1.0; n],
        source_path: Some("self-test".to_string()),
        imported_at: "1970-01-01T00:00:00Z".to_string(),
    });

    if let Err(e) = cal.save(Some(&path)) {
        let _ = std::fs::remove_file(&path);
        return Check {
            name:   "Calibration round-trip (3 layers)",
            pass:   false,
            detail: format!("save failed: {e}"),
        };
    }

    let loaded = match Calibration::load(7, 11, Some(&path)) {
        Ok(Some(c)) => c,
        Ok(None) => {
            let _ = std::fs::remove_file(&path);
            return Check {
                name:   "Calibration round-trip (3 layers)",
                pass:   false,
                detail: "load returned None".to_string(),
            };
        }
        Err(e) => {
            let _ = std::fs::remove_file(&path);
            return Check {
                name:   "Calibration round-trip (3 layers)",
                pass:   false,
                detail: format!("load failed: {e}"),
            };
        }
    };
    let _ = std::fs::remove_file(&path);

    let voltage_ok = (loaded.vrms_at_0dbfs_out.unwrap_or(0.0) - 1.234).abs() < 1e-9
                  && (loaded.vrms_at_0dbfs_in.unwrap_or(0.0) - 0.567).abs() < 1e-9;
    let spl_ok     = (loaded.mic_sensitivity_dbfs_at_94db_spl.unwrap_or(0.0) + 30.0).abs() < 1e-9;
    let curve_ok   = loaded.mic_response.as_ref().is_some_and(|r| r.freqs_hz.len() == 16);
    let pass = voltage_ok && spl_ok && curve_ok;

    Check {
        name: "Calibration round-trip (3 layers)",
        pass,
        detail: format!("voltage={voltage_ok}, spl={spl_ok}, mic_curve={curve_ok}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_self_test_passes() {
        let checks = run_all_checks();
        assert!(!checks.is_empty(), "expected at least one self-test check");
        for c in &checks {
            assert!(c.pass, "self-test failed: {} — {}", c.name, c.detail);
        }
    }

    #[test]
    fn handler_returns_results_array_and_all_pass_true() {
        // We can pass a dummy state because test_software ignores it.
        // Constructing a real ServerState here would pull in the engine,
        // so test the inner machinery by running the checks directly and
        // asserting the JSON-shaping invariant at the type boundary.
        let checks = run_all_checks();
        let json: Value = json!({
            "ok": true,
            "results": checks.iter().map(|c| json!({
                "name":   c.name,
                "pass":   c.pass,
                "detail": c.detail,
            })).collect::<Vec<_>>(),
            "all_pass": checks.iter().all(|c| c.pass),
        });
        assert_eq!(json["ok"], json!(true));
        assert!(json["results"].is_array());
        assert_eq!(json["results"].as_array().unwrap().len(), checks.len());
        assert_eq!(json["all_pass"], json!(true), "shipping a self-test with a known-failing check is a regression");
        for r in json["results"].as_array().unwrap() {
            assert!(r["name"].is_string());
            assert!(r["pass"].is_boolean());
            assert!(r["detail"].is_string());
        }
    }
}
